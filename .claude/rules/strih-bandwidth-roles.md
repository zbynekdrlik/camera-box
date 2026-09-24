---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "scripts/strih_scenes.py"
  - "scripts/strih_mv_scenes.py"
  - "scripts/strih-obs-start.sh"
  - "scripts/genlock_park.py"
  - "scripts/lib/genlock-park.sh"
  - "scripts/lib/connect-on-show-hold.sh"
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/cadence-alert-watchdog.sh"
  - "scripts/ndi-halving-watchdog.sh"
  - "scripts/rig-health-audit.py"
  - "scripts/set-ndi-mapping.py"
  - "tests/distroav_connect_on_show_park_1242.rs"
  - "tests/python/test_genlock_park_1242.py"
  - "tests/python/test_strih_bandwidth_roles_1242.py"
---

# strih bandwidth roles — full bandwidth only for SHOWN cameras (issue 1242)

Owner ruling 24.9.2026: strih-lx pulls FULL-bandwidth NDI only for the cameras that are shown —
preview, program, a projector, the visible item of the Grading NDI-output scene. The built-in
multiview renders low-bandwidth twins. A cold start in preview is accepted. This reverses, for the
strih role only, the issue-761 same-source multiview and the issue-764 "every genlocked input stays
connected". Why: the 2.5 GbE `foh1_video ether2` uplink tail-dropped with all 7 cameras at HIGHEST
all the time, and the switch buffer was already at `shared-buffers=80%` (nothing left to tune). The
per-camera send stagger (`.claude/rules/ndi-send-stagger.md`) is the other half of the same ticket.

## The two receiver roles (decided by ROLE flag, never by platform)

| Role | Input | Flags | Behaviour |
|---|---|---|---|
| program-path main | `NDI camN` | `genlock_connect_on_show=true` | PARKED (NDI receiver released) while nothing shows it; fresh connect on show |
| monitor twin | `MV NDI camN` | `genlock_monitor=true` | LOWEST bandwidth (#501 forcer exception), ALWAYS connected, feeds the multiview |

- Only the strih scene role lib sets `genlock_connect_on_show`, and only on fleet CAMERA senders
  (`CAM<n> (...)`). The cg inputs stay always connected (a CG cut-in must be instant), the 2ME
  feedback pair is not genlocked. **Stream (`NDI 2ME PGM`, `Zaloha kamera`) and the resolume cg OBS
  never run this lib**, so their issue-764 keep-alive is byte-for-byte unchanged. A platform-blind
  rule would disconnect stream's 2ME PGM whenever stream sits on PRE/POST — that is why the flag
  exists (default OFF, whitelisted, operator-visible).
- `genlock_monitor` WINS over the program-path flag: a twin is never parked.

## The vendored mechanism: an IN-THREAD park, never stock STOP_RESUME

libobs calls `info.show`/`info.hide` from `obs_source_video_tick` — the **graphics thread**. Stock
DistroAV's connect-on-show (`ndi_behavior=STOP_RESUME_*`) runs `ndi_source_hidden` →
`ndi_source_thread_stop` → `pthread_join` there, blocking the program render up to one
`recv_capture_v3` timeout (100 ms) or one fresh-finder wait (500 ms) on every hide, including the
old program input at every cut (the issue-1320 render-freeze class). So:

- The forced behavior stays `KEEP_ACTIVE`; `s->running` stays true; `ndi_source_name` is never
  touched → none of the `distroav-receiver-lifecycle.md` hazards (no `break`, no empty name).
- At the TOP of the receiver loop, BEFORE the reset block, the pure
  `genlock_connect_on_show_park_decision(genlock_active, monitor, connect_on_show, showing)` decides.
  On park: the receiver + framesync go to the issue-1320 detached reaper, `set_genlock_connected(false)`
  (the issue-1299 LOCK facet treats it as idle, not DEGRADED), log + `sleep 5 ms; continue`.
  On show: `was_disconnected = true`, `no_conn_since_ns = 0`, re-arm `reset_ndi_receiver` → the
  normal reset block runs the issue-1096 fresh finder.
- The role snapshot (`s->config.genlock_monitor` / `s->config.connect_on_show`) is written in
  `ndi_source_update` under `config_mutex`, gated on the genlock lockdown.
- Log family (mutually non-substring vs every other `genlock-*` marker):
  `genlock-park '<src>': state=parked parked_s=N (...)` — once on park, then a **5 s heartbeat** —
  and `genlock-park '<src>': state=unparked parked_s=N (...)` once on show.
- Gate: `tests/distroav_connect_on_show_park_1242.rs` (anchors + the lifted truth table, mutation
  seen RED) + both `windows-genlock*.yml` pwsh mirrors.
- Cost the owner accepted: PVW-select → first warm frame = the reset block's fresh finder (up to
  4 × 500 ms waits) + connect + a genlock FIFO relock. Measure it live after deploy.

## Why a multiview must not show a scene holding a full input

`Multiview::Update` calls `obs_source_inc_showing` on every scene it renders. A `Cam N` scene in the
multiview keeps its full input "showing" → never parked → no saving. So `strih_scenes.py
--apply-roles` (run by `strih-obs-start.sh` after `--bootstrap` on every launch, best-effort):

1. flags every program-path main `genlock_connect_on_show=true`;
2. heals/creates the `MV NDI camN` twins (same LIVE sender + pin as the main, monitor, audio off);
3. gives every multiview scene that holds a program input DIRECTLY an `MV <scene>` twin (items
   mirrored, each main swapped for its twin, audio-only inputs dropped); the original leaves the
   multiview, the twin joins it. Covers `Cam N` AND `Moderatori`;
4. never twins an NDI-output scene (an enabled `ndi_filter`: Grading, Interkom) — the filter only
   sends while its parent is SHOWING, and Grading's one enabled nested camera IS the wanted
   full-bandwidth grading feed;
5. swaps the custom `MULTIVIEW` grid scene's full inputs / `Cam N` refs for twins;
6. refreshes the built-in multiview with a scratch-scene create+remove (OBS re-reads membership only
   on a scene-list change; a WS private-setting write alone does not refresh it).

A correct collection is a pure read (idempotency is tested). `--apply-roles` is a SEPARATE mode from
`--bootstrap` so the issue-1317 update-only "never create" seed contract stays as pinned; the twins
are role-owned creates. New twin scenes land at `currentRow()+1` in the scene list — the operator's
multiview tile ORDER changes once (obs-websocket v5 has no scene-reorder request); fix it in the UI
once, the collection keeps it.

## Every received= consumer reads a parked input as HIDDEN BY DESIGN

A parked main's `received=` stops advancing and its `recv-timing #797` line stops. ONE parser:
`scripts/genlock_park.py` (python) and its bash twin `scripts/lib/genlock-park.sh`, pinned to each
other over the same fixtures. A source's state = its LAST park line in the log window; no line →
not parked (normal classification).

| Consumer | Handling |
|---|---|
| strih frozen-input watchdog (#1069 enumeration) | `genlock_park_watch_set`: one live receiver per camera — a live main (twin dropped), or the twin of a parked main; never both (no double page) |
| frozen-input static SOURCES / cadence / ndi-halving | parked → SKIP, no blind-tap count, stale baseline dropped |
| rig-health-audit `arrivals-low` | `low_arrival_sources`: parked inputs and `MV` twins excluded |
| set-ndi-mapping `--verify-live` | `hidden=obs_phase2.input_hidden_by_design` (settings + `GetSourceActive.videoShowing`) → SKIP, never screenshot-sampled |
| asio-starve watchdog | n/a (reads `asrc:` audio lines; camera inputs carry no audio) |
| `[4c/8]`, mv-reverify-escalate, ndi-cadence-heal, `[4j/8settle]`, `recording-e2e.sh` | the E2E HOLD (below) keeps every program-path input connected, so nothing parks during a run |
| genlock_audit_snapshot / e2e_discord_report / churn / arrival_floor | report-only analysis; arrival_floor already filters `^NDI cam` |

## The E2E hold

`recording-e2e.sh`, right after `trap cleanup` arms, behind its OWN `stray_session_check_assert`
(a strih OBS settings write is a rig mutation — it is in the issue-1271 `muts` list):
`connect_on_show_e2e_hold "$HERE" "$STRIH" "$OUTDIR/connect-on-show-hold.json" || exit 1`.
`obs_phase2.py connect-on-show --hold` writes the held list BEFORE flipping (a kill mid-hold still
leaves restore a full list), UNIONS a leftover state file, and reads every write back (failure →
the run aborts: a hidden input would be measured cold). `cleanup()` restores it after
`ndi_cadence_verify_and_heal` (always returns 0; a failed restore is fail-SAFE — the inputs just
stay connected until the next launch re-applies the roles).

## Live acceptance (supervisor, never on a production day)

Full-bundle deploy (vendored DistroAV) → `strih_scenes.py --apply-roles` (or an OBS relaunch) →
fix the multiview tile order once → `ether2` tx-drop = 0 over 30 min → PVW-select → first-frame time
measured → multiview live on all cells → E2E green.
