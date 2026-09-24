---
paths:
  - "vendor/distroav/src/ndi-source.cpp"
  - "vendor/obs-studio/frontend/components/Multiview.cpp"
  - "vendor/obs-studio/frontend/components/Multiview.hpp"
  - "scripts/strih_bandwidth_roles.py"
  - "tests/obs_multiview_cell_target_1242.rs"
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
  On park: the receiver + framesync go to the issue-1320 detached reaper, the source is BLANKED
  (`deactivate_source_output_video_texture` → `obs_source_output_video(NULL)`: no hours-old frame is
  presented as live on the next show despite the certified KEEP_CONTENT, and the genlock FIFO's
  `last_frame_ts = 0` stops `async_tick` counting an underrun EVERY render tick for a parked input —
  `async_tick` runs for every async source regardless of showing), `set_genlock_connected(false)`
  (the issue-1299 LOCK facet treats it as idle, not DEGRADED), log + `sleep 5 ms; continue`.
  On show: `was_disconnected = true`, `no_conn_since_ns = 0`, re-arm `reset_ndi_receiver` → the
  normal reset block runs the issue-1096 fresh finder. The #1180 post-connect identity verify is
  KEPT for that bind (review round 2, a decision): after a long park the sender may have restarted
  and reshuffled its port, and the fresh finder itself can serve a dying sender's last
  advertisement — the wrong-camera guard outweighs its one-shot blocking finder (up to 2 × 500 ms
  right after the first frames), which is part of the accepted preview cold start. A round-1 skip
  was reverted; `tests/distroav_connect_on_show_park_1242.rs` pins that no skip exists.
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
multiview keeps its full input "showing" → never parked → no saving. So
`scripts/strih_bandwidth_roles.py` (via `strih_scenes.py --apply-roles`, run by `strih-obs-start.sh`
after `--bootstrap` on every launch, best-effort; installed next to strih_scenes.py by setup-strih
step 6):

1. flags every program-path main `genlock_connect_on_show=true` — or `false` while a FRESH strih-side
   E2E hold marker exists (`~/.camera-box/connect-on-show-e2e-hold` on strih-lx, 4 h TTL): the #1093
   wedge escalation can relaunch strih OBS mid-run, and its launch-time apply must not re-park the
   inputs the run is measuring. The path is written twice (the bash hold lib over ssh, the python
   `E2E_HOLD_MARKER` read) and pinned equal by the pytest; a future-stamped marker (clock stepped
   back) only counts within 60 s of skew, so it can never outlive the TTL;
2. heals/creates the `MV NDI camN` twins (same LIVE sender + pin as the main, monitor, audio off);
   the sender name of an existing twin goes through the #795-safe `_enforce_ndi_source_name`; a
   main with NO sender (an empty name would stop the twin's receiver thread) or a twin name already
   owned by a non-NDI input → no twin, a `problems` entry;
3. `scenes_needing_twins`: every scene that holds a program input DIRECTLY or NESTS such a scene
   (recursive, cycle-safe) gets an `MV <scene>` twin mirroring it — each main swapped for its twin,
   each nested twinned scene for ITS twin, audio-only inputs dropped, a swapped item pinned to
   `OBS_BOUNDS_SCALE_INNER` at the main item's footprint = its NOMINAL size (sourceWidth, else the
   canvas) × scale — NDI "lowest" is a lower-resolution proxy (a scale-placed twin would draw small in
   the tile corner), and a PARKED main reports a zero computed width/height (async inactive), so the
   computed size is never used (the twin would otherwise differ parked vs live and rebuild every
   launch). ASSUMPTION: a camera's native resolution equals the strih canvas (1920×1080 today). A
   parked main has no sourceWidth either, so its footprint falls back to the canvas; a camera at a
   different resolution would size differently parked vs live and its twin would be rebuilt on each
   launch (harmless, but not stable) — revisit if a non-canvas-size camera joins. Crop (main-pixel
   units) is never mirrored onto the proxy; a cropped camera item is reported. A drifted twin
   (source / enabled / transform) is rebuilt. Covers `Cam N` AND `Moderatori`;
4. never twins an NDI-output scene (an enabled `ndi_filter`: Grading, Interkom) — the filter only
   sends while its parent is SHOWING, and Grading's one enabled nested camera IS the wanted
   full-bandwidth grading feed — nor the custom grid, nor a twin;
5. the multiview membership hand-off (`membership_on_create(orig_shown, twin_shown)`: the twin takes
   the cell the original — or an already-shown twin, the migrated Windows #501/#761 layout — held; a
   nested-only twin never shown) and the twin's `camera_box_multiview_target` private key are written
   ONCE, when the twin is created or first adopted — never re-imposed (operator wins, the imag #785
   lesson);
6. a twin whose original lost its camera is RETIRED (original back in, twin out, the adoption key
   cleared so a returning camera hands the cell back to the twin; the scene is kept);
7. swaps the custom `MULTIVIEW` grid scene's full inputs / twinned scene refs for twins;
8. refreshes the built-in multiview with a scratch-scene create+remove (OBS re-reads membership only
   on a scene-list change; a WS private-setting write alone does not refresh it).

**The multiview cell stands for its TARGET (vendored frontend, `Multiview.cpp`).** Without it the
operator's multiview regresses: the tally border never lights (the twin is never on program), the
label reads `MV Cam 3`, and a click (MultiviewMouseSwitch, default on) / double-click puts the
LOW-bandwidth twin into preview or straight onto PROGRAM. `multiview_cell_target(src, priv)` resolves
`camera_box_multiview_target` (a missing key / unknown / non-scene name → the cell itself, so stream /
imag / resolume are byte-for-byte stock); `Update` records a per-cell target (labels it, NEVER
inc_showing's it — that would reconnect the camera), `Render` compares the TARGET to program/preview
for the tally, `GetSourceByPosition` returns the target for both click handlers. Guard:
`tests/obs_multiview_cell_target_1242.rs` + the python key pin. A FRONTEND change → FULL-bundle deploy.

A correct collection is a pure read (idempotency is tested). `--apply-roles` is a SEPARATE mode from
`--bootstrap` so the issue-1317 update-only "never create" seed contract stays as pinned; the twins
are role-owned creates. New twin scenes land at `currentRow()+1` in the scene list — the operator's
multiview tile ORDER changes once (obs-websocket v5 has no scene-reorder request); fix it in the UI
once, the collection keeps it. Twin sender names follow the mains only at the next launch (a
`set-ndi-mapping --heal` of a main does not touch its twin).

## Every received= consumer reads a parked input as HIDDEN BY DESIGN

A parked main's `received=` stops advancing and its `recv-timing #797` line stops. ONE parser:
`scripts/genlock_park.py` (python) and its bash twin `scripts/lib/genlock-park.sh`, pinned to each
other over the same fixtures. A source's state = its LAST park line in the log window; no line →
not parked (normal classification).

| Consumer | Handling |
|---|---|
| strih frozen-input watchdog (#1069 enumeration) | `genlock_park_watch_set`: one live receiver per camera — a live main (twin dropped), or the twin of a parked main; never both (no double page) |
| frozen-input static SOURCES / cadence / ndi-halving | parked → SKIP, no blind-tap count, stale baseline dropped |
| rig-health-audit `arrivals-low` + `cadence_check` | `park_touched_sources` (ANY park line, parked or unparked, in the window: a partial history is no measurement) + `MV` twins excluded |
| set-ndi-mapping `--verify-live` | `hidden=obs_phase2.input_hidden_by_design` (settings + `GetSourceActive.videoShowing`) → SKIP, never screenshot-sampled |
| asio-starve watchdog | n/a (reads `asrc:` audio lines; camera inputs carry no audio) |
| `[4c/8]`, mv-reverify-escalate, ndi-cadence-heal, `[4j/8settle]`, `recording-e2e.sh` | the E2E HOLD (below) keeps every program-path input connected, so nothing parks during a run — including across a mid-run strih OBS relaunch (the strih-side marker makes the launch-time role apply keep the mains connected) |
| genlock_audit_snapshot / e2e_discord_report / churn / arrival_floor | report-only analysis; arrival_floor already filters `^NDI cam` |

## The E2E hold

`recording-e2e.sh`, right after `trap cleanup` arms, behind its OWN `stray_session_check_assert`
(a strih OBS settings write is a rig mutation — it is in the issue-1271 `muts` list):
`connect_on_show_e2e_hold "$HERE" "$STRIH" "$CONNECT_ON_SHOW_HOLD_STATE" || exit 1` (first the
strih-side marker over ssh — so a mid-run OBS relaunch keeps the mains connected — then the live WS
flip; the restore clears the marker FIRST), then
`connect_on_show_e2e_wait_live` (bounded 30 s, fail-OPEN WARNING): every held input must UNPARK and
advance its `received=` (or, when its first read had no audit line, show one at all) before the
`[1/8]` pixel-liveness / `[2/8]` reverify checks run, so they never race a cold reconnect; the budget
is WALL time, so a zero poll interval still terminates. A restore treats an input deleted/renamed
since the hold as done (the state file never outlives it). The state file is STABLE (`~/.camera-box/connect-on-show-hold.json`, never the
per-run OUTDIR): `obs_phase2.py connect-on-show --hold` writes the held list BEFORE flipping, UNIONS a
leftover file (a SIGKILLed run's list is restored by the next run's cleanup), and reads every write
back (failure → the run aborts: a hidden input would be measured cold). `cleanup()` restores it
AFTER `cleanup_mv_reverify_active_boxes` + `ndi_cadence_verify_and_heal` (both read every input
connected — restoring earlier would make the #759 reverify see parked mains as wedged and escalate);
always returns 0; a failed restore is fail-SAFE (the inputs just stay connected until the next
launch re-applies the roles). During a run the uplink carries 7 full inputs + 7 LOWEST twins — more
than before this change — so the live acceptance also reads `ether2` drops DURING an E2E.

## Live acceptance (supervisor, never on a production day)

Full-bundle deploy (vendored DistroAV + OBS frontend) → `strih_scenes.py --apply-roles` (or an OBS
relaunch) → fix the multiview tile order once → multiview: every cell live, labels read the program
scene names, a cut lights the right cell red, a click selects the program scene (never an `MV`
scene) → `ether2` tx-drop = 0 over 30 min idle AND during an E2E → PVW-select → first-frame time
measured → E2E green. Unverified until then: whether an unpark's fresh-finder bind ever hits the
#1287 frame-less alternation on the rig.
