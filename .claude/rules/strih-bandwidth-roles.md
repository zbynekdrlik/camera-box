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
  - "tests/python/test_e2e_twin_hold_1242.py"
  - "scripts/e2e_bandwidth_hold.py"
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
| set-ndi-mapping `--verify-live` | `hidden=obs_phase2.input_hidden_by_design` (settings + `GetSourceActive.videoShowing`) → SKIP, never screenshot-sampled; a twin the E2E hold took off the wire (`strih_bandwidth_roles.twin_is_held`) is hidden by design too, showing or not |
| asio-starve watchdog | n/a (reads `asrc:` audio lines; camera inputs carry no audio) |
| `[4c/8]`, mv-reverify-escalate, ndi-cadence-heal, `[4j/8settle]`, `recording-e2e.sh` | the E2E HOLD (below) keeps every program-path input connected, so nothing parks during a run — including across a mid-run strih OBS relaunch (the strih-side marker makes the launch-time role apply keep the mains connected). Their input sets are the `NDI camN` mains only (`camera_*_ndi_sources*_csv`, `NDI cam${cam_n}`, the `NDI_CADENCE_INPUTS` default; the live freeze watch too), so a twin the hold took off the wire is never read — pinned by `tests/python/test_e2e_twin_hold_1242.py` |
| the hold's own `connect_on_show_e2e_wait_live` | reads only the held MAINS from the hold state file (`connect_on_show_held_mains`, the bash twin of `e2e_bandwidth_hold.read_state`, pinned to it); a held twin delivers no video and is never waited on |
| in-OBS LOCK widget / the `genlock_lock` facet / its watchdog | a held twin runs with `genlock_fifo=false`, and the widget scan skips a non-genlock source (`!st.genlock_fifo`, `OBSBasicStatusBar.cpp`) — never unlocked / absent / idle, never a `recent_event` offender. Its restore at cleanup re-enables genlock, so a short `recent_event` DEGRADED after the run is expected |
| `[0/8]` reads | run BEFORE the hold. A twin left held by a SIGKILLed run is non-genlock, so the lock widget and the mains-only input sets ignore it; the next run's hold unions it and its cleanup restores it |
| dev1 frozen-input watchdog during the hold | the mains are unparked, so `genlock_park_watch_set` drops their twins; a held twin logs no audit line |
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
always returns 0. A failed restore leaves a main held at full bandwidth, or an MV twin off the wire
with a blank multiview cell (its main then stays held too), until the next run's cleanup or the next
launch re-applies the roles.

**The same hold takes every monitor twin OFF THE WIRE for the run** (design 5859315296, finding
5859213950). The protocol (state file, hold, restore, settle poll) is `scripts/e2e_bandwidth_hold.py`;
`obs_phase2.py connect-on-show` calls it with its own `_rpc` and settle seams passed in at call time,
so the module has no WebSocket dependency and every obs_phase2 test that monkeypatches `_rpc` drives
it. A twin from a camera-box sender costs ~58 Mbps at NDI "lowest", not a small proxy, so
7 mains + 7 twins (~1.4 Gbps) tail-dropped the strih-lx `foh1_video ether2` uplink during the E2E
(~480 drops/min) and failed release E2E attempts on camera arrival holds. Now:

- **Which inputs:** the role-marked twins, `strih_bandwidth_roles.twin_hold_targets` = the `MV ` name
  AND `genlock_monitor`.
- **What is written:** `E2E_TWIN_HOLD` = `{"genlock_fifo": false, "ndi_bw_mode": 2}` (audio-only).
  With genlock on, DistroAV's #150 lockdown (`force_genlock_certified_settings`) forces a monitor twin
  back to LOWEST on EVERY settings update, so an audio-only write alone never sticks. With genlock
  off the coercion does not run: audio-only stays and DistroAV's own `reset_ndi_receiver` drops the
  video. The sender name is never touched (no empty-name wedge, `ndi-name-recovery.md`).
- **Recorded before any write:** each twin's original, `twin_hold_original` of its EFFECTIVE settings
  (`genlock_fifo` defaults to TRUE in this build, so an absent key is genlocked). A twin that already
  reads held (a leftover whose state file was lost) records `TWIN_ON_WIRE`, never the held values.
- **State file:** `{"connect_on_show": [...], "twins": {name: original}}`. A legacy list of mains
  still reads. A leftover file is unioned, and its recorded twin original wins.
- **Writes only present inputs, and the ORDER is enforced, not just issued.** The hold writes the
  mains and WAITS for them to settle, then writes the twins. A twin whose main did not settle stays on
  the wire, and so does the twin of a main the hold could not read: a camera always keeps one
  receiver CONFIGURED to connect (the settle confirms the setting read back, not that frames arrive —
  an unpark or re-bind takes ~1-2 s). An input the hold cannot read at enumeration is a failure: it
  may be a twin still on the wire, or a main about to be measured parked.
- **Every read-back is a SETTLE poll**, `e2e_bandwidth_hold.await_settled`. OBS applies an input update on the next
  VIDEO TICK after the WS overlay (`obs_source_update` defers `info.update`), so an immediate
  `GetInputSettings` reads the overlay back even when the update is about to revert it. The poll:
  - starts `_SETTLE_MIN_S` (0.25 s) after the writes;
  - needs two consecutive matching reads;
  - never counts a request error as a match (a main's hold target equals the type default);
  - reads the type defaults once per settle;
  - fails an input only after `_SETTLE_BUDGET_S` (5 s) AND two complete sweeps, so a slow WebSocket
    never fails a write it never re-read.

  A failure fails the hold (exit 1): the run aborts, and its cleanup restores what was recorded.
- **Restore, twins FIRST:** `twin_restore_values` writes genlock on plus the monitor ROLE (the
  lockdown pins LOWEST only for a `genlock_monitor` source), and a held-shaped original is never
  restored as held. Only then do the mains get connect-on-show back. A main whose twin did not
  settle stays HELD (full bandwidth) and stays in the state file, so the camera keeps one receiver
  configured to connect. The restore returns `(restored, failed, held_back)`, and the CLI names the
  held-back mains. Every write is verified by the same settle poll. A deleted/renamed input is done.
  The file is removed only when every restore landed.
- **Mid-run strih OBS relaunch:** the launch-time role apply leaves a held twin alone
  (`twin_is_held`, `summary["twins_held"]`) while the fresh E2E hold marker exists. Without the
  marker its twin role heal writes `genlock_fifo=True`, so a twin left held by a SIGKILLed run comes
  back on the wire at the next launch. A twin whose held settings were never saved to the scene file
  before an OBS crash comes back on the wire for the rest of that run (a higher load, never wrong
  data).
- **Cost:** the multiview twin cells go BLANK for the run. Audio-only makes DistroAV deactivate the
  texture (`deactivate_source_output_video_texture`), so the operator sees blank cells, not frozen
  frames.

**Live check (supervisor):** during the next release E2E, read the `ether2` tx-drop delta before
and after, and the per-source rx rates (the twins at ~0 video).

## Live acceptance (supervisor, never on a production day)

Full-bundle deploy (vendored DistroAV + OBS frontend) → `strih_scenes.py --apply-roles` (or an OBS
relaunch) → fix the multiview tile order once → multiview: every cell live, labels read the program
scene names, a cut lights the right cell red, a click selects the program scene (never an `MV`
scene) → `ether2` tx-drop = 0 over 30 min idle AND during an E2E (the twins off the wire) → PVW-select → first-frame time
measured → E2E green. Unverified until then: whether an unpark's fresh-finder bind ever hits the
#1287 frame-less alternation on the rig.
