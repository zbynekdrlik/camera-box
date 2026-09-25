---
paths:
  - "src/cg_chain_gate.rs"
  - "scripts/lib/cg-chain-e2e.sh"
  - "tests/harness_cg_chain_hops_1302.rs"
  - "tests/harness_cg_chain_e2e_1301.rs"
  - "tests/harness_cg_chain_e2e_1302.rs"
  - "scripts/cg_chain_scene.py"
  - "tests/python/test_cg_chain_scene_1302.py"
  - "scripts/recording-verdict-on-resolume.sh"
  - "scripts/lib/verdict-upload-gate.sh"
  - "tests/harness_cg_chain_onbox_1302.rs"
---

# CG-path burn-id node role — SongPlayer (911014 ORIGIN) / cg OBS (911015 HOP), REPORT-ONLY (#1301)

`recording-verdict` proves pixel-level frame contiguity for content that ORIGINATES at the cam2
optical painter (the camera chain) and for the digital node hops (cam1/strih/stream/imag). #1301
adds the SAME kind of proof for content that ORIGINATES in **SongPlayer** (the song-lyrics / CG
source) and flows `SongPlayer → cg OBS (RESOLUME-SNV) → strih → stream`.

## The two roles (the load-bearing modeling decision — do NOT conflate them)

- **SongPlayer = `BURN_RUN_ID_SONGPLAYER` = 911014, a chain ORIGIN role** — like the cam2 painter
  is the origin of the camera chain. It is the content SOURCE whose burn id is tracked THROUGH the
  chain; it is NOT a camera-under-test hop, so **911014 is never in `CAMERA_UNDER_TEST_NODES` /
  `OPTICAL_INJECTION_NODES`** (the three copies: `recording-verdict.rs`, `recording_span_gate.rs`,
  `switch_latency.rs`). SongPlayer PAINTS this burn itself (sender half = zbynekdrlik/songplayer#151,
  contract #1294 §8) — NOT the OBS burn filter.
- **cg OBS = `BURN_RUN_ID_CG` = 911015, a HOP node** — like strih/stream. The DistroAV burn filter
  composites it at `burn_geom::Corner::BottomCenterRight` (the mirror of imag's BottomCenterLeft
  from the right; resolved from the `resolume` hostname in `ndi-burn-filter.cpp`'s host-role map).

Both ids ARE tick-excluded (`src/probe/recording.rs::NODE_BURN_RUN_IDS`) + in the python mirrors
(`qr_align_pins.py`, `mv_skew_snapshot.py`) + in EVERY `all_burns`/`latency_all_burns` exclusion
array + the `#638` push site in `recording-verdict.rs` — because a CG burn can ride into a
strih/stream recording during a CG_CHAIN run and must never hijack the cam2 Vernier tick (the
#463/#312 gotcha).

## The verdict — REPORT-ONLY, provably never touches the camera-chain `overall_pass`

The pure decision is the crate-root `src/cg_chain_gate.rs` (Tier-0, mirrors `imag_tick_gate.rs` +
reuses `burn_hold`): per hop (`cg_obs`/`strih`/`stream`) the SongPlayer (911014) and cg (911015)
burn-id contiguity + max-hold (`MAX_HOLD_FRAMES=4`, #575-boundary-trimmed). Contiguity is
decimation-aware since issue 1302 slice 2 (`hop_contiguity_with_step`, below); `cg_obs` is the 1:1
case (step 1 = the old presence-only `first..=last`). `recording-verdict --cg <path>` emits `report["cg_chain"]`, gated on `--cg`
being supplied (a normal camera run omits it). It folds via `cg_chain_gate::folds_into_overall_pass`,
which is a **no-op while `gates_overall_pass() == false`** — so the camera-chain gate is untouched.

**It is REPORT-ONLY on purpose.** The SongPlayer burn shipped (songplayer 151, 25.9.2026) but no
CG_CHAIN=1 run has produced real data yet, so the hold/decimation behaviour is uncalibrated. The
flip to LIVE is the standard one-line seam
(`gates_overall_pass() -> true`, `verdict-gate-seam-calibration.md`) — but the checklist is MORE
than "calibrate the hold". Before the flip:

1. A real captured cg-OBS frame with the SP burn replaces the generated decode fixture
   (`pattern-change-needs-decode-fixture.md`).
2. **DONE in code (issue 1302 slice 2), unproven live: the strih + stream hops decode sp/cg.**
   With `CG_CHAIN=1` the harness passes `--cg-chain-burns` to the strih/stream `--extract-partial`
   calls AND the merge, which appends 911014/911015 to `args_expected_burns_for("strih"|"stream")`
   (the partial's `expected_burns` + the merge consistency check). The fast-path decode groups are
   deliberately NOT touched: a CG-window frame carries no camera-under-test burn (and no cam2
   Vernier), so the any-of group is unsatisfied and EVERY CG frame already takes the robust
   bottom-band tiling, which spans the full width (incl. `BottomCenterRight`). Adding the CG ids to
   the any-of group could only WEAKEN the gate (a frame with the cg burn but a missed SP burn would
   skip the tiling); a mandatory group would force the tiling on every camera frame. The partial
   carries every CRC-valid payload regardless, so the merge sees the CG ids. Normal runs: the flag
   is absent and both burn sets are byte-identical.
3. **DONE in code (issue 1302 slice 2), unproven live: decimation.** `hop_contiguity_with_step`
   is the `burn_contiguity_in_window_with_step` model (duplicated — the probe module is CI-only):
   per forward gap between consecutive DISTINCT present ids, the excess `gap / step - 1` (integer
   division) is charged; gap == step is decimation, `step + 1` is beat jitter (0 at step 2), and a
   charged slot is listed as `prev + k * step`. The step comes from the fps ratio
   (`painted_tick_step(--cg-source-fps, <recording fps>)`): cg_obs = `--cg-capture-fps` (60 ⇒ 1),
   strih = `--capture-fps` (30 ⇒ 2), stream = `--stream-capture-fps` (30 ⇒ 2). Each hop reports
   `expected_step` + a `forward_steps` gap histogram (`{"2": 29}`) — the calibration evidence. Watch
   it on the first live run: the camera chain found the cam(60)->strih(30) beat irregular (#571:
   bursts of delta 1 then ~7), which this integer-division model would charge. If the CG histogram
   shows the same shape, the strih/stream hops need gap-ignore (`node_render_step`'s answer), not
   this model.
   **The CG window scope (slice 2 item 3):** the merge gets `--cg-window cg-window-<RUN_ID>.json`
   (`cg_chain_merge_args_append`, only when the file exists for THIS run). The strih/stream hops then
   keep only CG payloads whose OWN `gen_ts_ns` is inside `[start_ns, end_ns]` (`pairs_in_window`) —
   a stamp window is a contiguous id range, so it opens no artificial gap, and a stray CG read far
   outside the window no longer widens `first..=last`. cg_obs is never scoped. A missing / bad /
   wrong-kind window file = a WARNING and the whole recording (`window_scoped: false`), never an
   error. The report carries `cg_chain.window` (`{start_ns,end_ns}` or null) and per hop
   `window_scoped`.
4. A green CG_CHAIN=1 run series calibrates the hold bound (`MAX_HOLD_FRAMES`) against real data.

Until all four hold LIVE, do NOT flip `gates_overall_pass()` — items 2+3 are code, not proof. The
live `CG_CHAIN=1` E2E must show the strih/stream hops populated for SP-fast and `contiguous=true`
on a clean chain (issue 1302 acceptance) before the flip is even discussed.

## The decode fixture is GENERATED — a real cg-OBS frame MUST replace it

`tests/burn_payload_parity.rs::songplayer_origin_burn_911014_round_trips_through_the_production_decoder_1301`
renders the 911014 payload with the production renderer and decodes it with the production decoder —
proving the fleet decode path reads the new origin run_id, but NOT that it survives the real lossy
chain (projection → grabber → NDI → re-encode). Mine the real fixture from the first CG_CHAIN=1 rig
run (songplayer 151 is deployed since 25.9.2026; a supervisor/rig-ops step), per the #1196 precedent.

## The E2E profile is opt-in + leak-guarded (`scripts/lib/cg-chain-e2e.sh`)

`CG_CHAIN=1` turns the SongPlayer burn ON + cuts cg OBS program to the SP scene + StartRecords cg
OBS at `[5/8]` (no cg recording started ⇒ the burn goes straight back OFF), runs ONE tail CG window
on strih before `[7/8]`, ends the CG leg after the `[7/8]` StopRecord
(`cg_chain_after_stoprecord`, placed AFTER the issue-1354 genlock-audit AFTER snapshot and the
post-record stomp re-check so it never skews their "exactly the recording" window: cg StopRecord
keeping the host path, burn OFF, strih AND cg OBS program changes restored — so nothing CG runs
through the long on-box decodes), decodes the cg recording IN PLACE on RESOLUME-SNV at `[8/8]`
(merged as a `cg=` partial, below), and — the
#246/#844 leak-guard — repeats burn OFF + every scene restore in `cleanup()` even on an early abort
(the burn must NEVER stay on the LED wall). cleanup()'s cg StopRecord is keyed on
`CG_RECORDING_STARTED`, never on `CG_HOST_IP` alone (the host resolves BEFORE StartRecord — the
#649 harness-started-boxes-only rule). cleanup() uses a 3 s
per-request burn timeout (`CG_CHAIN_CLEANUP_BURN_TIMEOUT`) so an unreachable SongPlayer never
stalls the stream/strih teardowns behind it by a minute. All runners are best-effort + loud;
`CG_CHAIN` unset = byte-for-byte inert. A live CG_CHAIN=1 run is a supervisor/rig-ops step, never
unattended.

### The shipped SongPlayer burn API (issue 1302)

- Toggle: `POST {base}/api/v1/ndi/burn` with the JSON body `{"output":"SP-fast","on":true}` (or
  `false`). Base = `CG_CHAIN_SONGPLAYER_API` (default `http://resolume.lan:8920` — SongPlayer runs on
  RESOLUME-SNV 10.77.9.201); output = `CG_CHAIN_SONGPLAYER_OUTPUT` (default `SP-fast`). on/off is in
  the BODY, never the URL.
- Confirmation: `GET {base}/api/v1/ndi/health` returns a JSON ARRAY, one object per output, keyed by
  `ndi_name`; the toggle is verified by that output's `burn_on` (true/false). Absent output, bad JSON
  or a missing `burn_on` = `unknown`, never read as off.
- `cg_chain_songplayer_burn` POSTs + reads back up to `CG_CHAIN_BURN_ATTEMPTS` (3) times. An ON that
  never reads true is a WARNING (the cg_chain section then proves nothing); an OFF that never reads
  false prints a `LEAK` line with the exact manual-off curl. Both always return 0.

### The cg recording is decoded IN PLACE on RESOLUME-SNV (issue 1302, the job-budget slice)

`cg_chain_record_stop` keeps the `obs_phase2.py record --action stop` stdout (the StopRecord
`outputPath`, e.g. `C:/Users/Resolume/Videos/<stamp>.mkv`) in `CG_HOST_RECORDING_PATH`; an empty
answer (the cleanup() re-stop) never clears it. The recording is **never copied to dev1**: the first
design scp'd it there and decoded it inside the `[8/8d]` merge, and the 25.9.2026 release E2E (run
36115692830) spent 29 min in that one decode and hit the 75-min job timeout (the 250 MB scp itself
took 3 s — the decode on the small Tier-0 dev1 box was the cost). Now it follows the stream extract:

- **`scripts/recording-verdict-on-resolume.sh`** (always executes, plain session-agnostic ssh —
  a file copy, a headless CLI decode and a download, so `win-ssh-vs-mcp.md` context B holds):
  STEP 0 first puts the newest (by LastWriteTime) `ffmpeg.exe` under `RESOLUME_FFMPEG_ROOT` (default `C:\ffmpeg`)
  on PATH — RESOLUME-SNV keeps ffmpeg under `C:\ffmpeg\<build>\bin` but NOT on PATH (read live
  25.9.2026), and the decode session gets the same prefix — then fails loud BY NAME on a missing
  `ffmpeg`/`ffprobe` (`MISSING-TOOL:`, exit 3); it stops a leftover decode of the same exe and
  deletes a stale partial of the same name; STEP 1 deploys `recording-verdict.exe` behind the issue-1118 sha256
  version gate (`Get-FileHash` on the box vs `sha256sum` on dev1); STEP 2 runs
  `recording-verdict.exe --extract-partial cg --cg <recording> --out <partial>` at the issue-1260
  BelowNormal PriorityClass (`build_onbox_command`, SOURCED from recording-verdict-on-stream.sh,
  never re-implemented) so it cannot starve the live obs64/Arena; STEP 3 pulls back only the partial
  and its `-pixels` dir (probed with PowerShell `Test-Path`, never cmd.exe `if exist` — the box default shell is not known), logging the measured STEP 2 decode time — the evidence the grace is calibrated from. The resolume-owned builders quote paths as single-quoted PowerShell literals; the STEP 2 decode command reuses the stream builder's double-quoted args (OBS timestamp paths hold no `$`/backtick). Credentials come from the caller (`RESOLUME_USER`/`RESOLUME_PW`, which
  `cg_chain_user`/`cg_chain_pw` resolve from `CG_CHAIN_USER`/`CG_CHAIN_PW`).
- **The ONE sha256 upload decision** is `scripts/lib/verdict-upload-gate.sh`
  (`verdict_upload_decision`); `onimag_upload_decision` / `onstrihlx_upload_decision` delegate to it.
  A new on-box extract uses it, never a fourth copy.
- **recording-verdict**: `--extract-partial cg` (expected burns = SongPlayer 911014 + cg 911015, the
  same pair the fused `--cg` path decodes, independent of `--cg-chain-burns`) and a `cg=` merge slot
  that fills the SAME `cg` DecodedRec the fused path does — the cg_chain section reads frames only,
  so no verdict logic changed. A cg partial wins over a stray `--cg` (a warning, no second decode).
  A cg partial that fails to load for ANY reason — or is another box's partial in the cg slot — is
  DROPPED by the crate-root `partial_schema_gate::box_drops_on_any_load_failure` (the report-only
  leg must never abort the merge into a false camera RED); only an imag drop sets
  `imag_leg_skip_reason`.
- **Harness wiring (#675, two new call lines):** `cg_chain_onbox_extract_launch "$HERE"
  "${WIN_VERDICT_EXE_LOCAL:-}" "$E2E_EXECUTE_VERDICT"` right after the stream extract's pull-back
  echo (so strih, stream, imag and cg decode CONCURRENTLY — wall time is max(), not the sum), and
  `cg_chain_onbox_extract_wait` right after the #703 strih/stream wait block, before the `[8/8d]`
  banner. `cg_chain_merge_args_append` then adds `--merge-partials cg=<partial>` when THIS run's
  partial (`cg-partial-<RUN_ID>.json`, RUN_ID-keyed) reached dev1.
- **The job budget is bounded by construction:** the collect step waits at most
  `CG_CHAIN_EXTRACT_GRACE_SECS` (default 300 — reasoned in `cg_chain_extract_grace_secs`) PAST the
  camera legs, then stops the extract's whole dev1 process group (launched under `setsid`, skipped
  when job control is on) AND, bounded, the decode on the box (`recording-verdict-on-resolume.sh
  --stop-decode`: only processes running from that one exe path, never OBS/Arena), so it never keeps
  running next to the live CG box nor keeps the exe locked for the next upload; cleanup() does the
  same on an early abort, and any other failed extract (a dev1 side that died) asks the box to stop too.
- **GOTCHA — PowerShell's exit code is the LAST statement's `$?`.** `powershell -EncodedCommand`
  exits 1 when the last statement failed, and `-ErrorAction SilentlyContinue` only hides the message
  (`$?` stays false). The first cut ended the prepare step with a silenced `Remove-Item` of files
  that do not exist on a normal run, so every run died there (a review caught it live on the box).
  Guard an expected-to-fail statement (`if (Test-Path …) { … }`) or never make it the last one; the
  harness test's fake `ssh` models this rule, so a builder regressing to that shape goes red.
- **Every outcome is ONE named run-log line**, never a red: `CG-LEG-VERIFIED` (the partial reached dev1 and goes to the merge, which may still drop an unloadable one with its own WARNING),
  `CG-LEG-SKIPPED` (no cg recording this run — resolume away/unresolvable or its StartRecord failed,
  the home gate; or a plan-only run with `E2E_EXECUTE_VERDICT=0`), `CG-LEG-NOT-VERIFIED` (attempted:
  no StopRecord path, no Windows exe, a failed decode, a grace overrun). A failed/stopped extract
  leaves no partial behind, so a stale one is never merged.
- **Live precondition (supervisor, first CG_CHAIN=1 run):** `ffmpeg` + `ffprobe` found under
  `C:\ffmpeg` on RESOLUME-SNV (STEP 0 names them if absent), and the grace vs the real on-box decode time — a first run
  that ends `CG-LEG-NOT-VERIFIED: … still running …` means the grace (or the box) needs a look, never
  a longer job timeout. Tier-0 coverage: `tests/harness_cg_chain_onbox_1302.rs` (std-only, runs with
  plain `rustc --test` + a `tempfile` shim) drives the launch/collect/marker/merge-args seam against a
  fake extract script and the resolume script's `main()` against fake `sshpass`/`ssh`/`scp`.

### The ONE tail CG window (issue 1302) — why it is NOT a switch-schedule window

- **Every program cut is a HARD CUT.** `cg_chain_scene.py` switches the box to its cut transition
  (found by `transitionKind == cut_transition`, the name can be localized), snapshots the previous
  transition and restores it LAST. A Fade blends the old program into the new one and those frames
  read as undecodable — cg OBS runs a live 2 s Fade (strih ran Cut on 25.9.2026). The sweep's own
  `obs_phase2.py switch` does not need this: every sweep cut is camera→camera over the SAME painted
  QR, so a blend is invisible there.
- **cg OBS:** `scripts/cg_chain_scene.py program` cuts program to `CG_CHAIN_CG_SCENE` (default the
  lower-cased output, `sp-fast` — the live `cg_scenes` has one `sp-*` scene per SongPlayer output)
  BEFORE the cg StartRecord, so the cg recording carries the SP burn for the whole run.
- **strih:** `cg_chain_scene.py strih-solo` finds the ONE scene carrying `CG_CHAIN_STRIH_INPUT`
  (default `CG-obs`, the `RESOLUME-SNV (cg-obs)` ndi_source; live 25.9.2026 only `CG bridge` carries
  it), shows ONLY that item (live, `CG-obs` is DISABLED in `CG bridge` and a `CG-presenter` browser
  overlay sits on top — cutting to the scene as-is would show the overlay, not the burns) and cuts
  program to it, then runs obs_phase2's own polled `_assert_program_nonblack` (`min_mean=0`,
  peak-only: an idle SongPlayer frame can be dark, a dead CG-obs is not). Several scenes carrying it
  = fail loud unless `CG_CHAIN_STRIH_SCENE` names one.
- **It is a TAIL window after the last camera window, never an entry in `switch-schedule.json`:** a
  schedule window with no cam2 tick FAILS the per-cambox verdict (zero in-window frames), and a
  mid-run CG cut would land INSIDE the optical span (undecodable frames against the live floor). After
  the last camera window the CG frames are outside every schedule window and a trailing no-tick run
  past the last optical read — the tail placement + the hard cut are DESIGNED to keep the
  camera-chain verdict out of it; the first live CG_CHAIN=1 run is what confirms it (watch the
  undecodable counts and the A/V marker pairing on that run). The window is recorded to
  `$OUTDIR/cg-window.json` (`{"kind":"cg","scene","input","start_ns","end_ns"}`) and echoed into the
  log — the run's evidence of WHEN strih carried the chain; the merge reads it as `--cg-window` to
  scope the strih/stream `cg_chain` hops (issue 1302 slice 2). Its path is `cg-window-<RUN_ID>.json`
  (`cg_chain_window_file`). The strih cut runs under `cg_chain_window_cut_timeout` = max(caller
  timeout, `OBS_BLACKCHECK_TIMEOUT_S` + 30 s): the helper enumerates every scene AND polls the
  non-black check, and a kill after the cut would leave strih on CG with no window hold. Never cut strih BACK to a camera before
  StopRecord — a cam burn reappearing after the gap opens missing ids in its `first..=last`.
- **Snapshot-before-mutate:** every scene change writes its restore snapshot
  (`$OUTDIR/cg-chain-{cg-program,strih-scene}-state-<RUN_ID>.json`: host, scene, previous program,
  previous transition, every item's enabled state) BEFORE changing anything; `restore` replays it
  (program first, then items, then the transition) and renames the file `.restored`, so the cleanup()
  second pass is a no-op. The RUN_ID key means a reused OUTDIR never replays another run's snapshot.
  strih is restored right after the `[7/8]` StopRecord, again in cleanup(). A failed restore keeps
  the snapshot and prints the manual restore command. `cg_chain_scene.py` closes every WS session it
  opens.
- `recording-e2e.sh` wiring adds NEW lines only (plus rewording three stale comment/echo lines of the
  #1301 block that said the SongPlayer burn was unshipped): `CG_CHAIN_STATE_DIR="$OUTDIR"` next to
  the other CG state; the burn-back-OFF line inside the `[5/8]` CG block; `if cg_chain_window_due; then
  cg_chain_window "$STRIH" …; fi` between the sweep/hold `fi` and the `[7/8]` banner (its comment
  must never contain the `[7/8]` literal — a test anchors on the FIRST `[7/8]` in the file); and
  `cg_chain_after_stoprecord …` after the `imag host file` echo. Anchor gotcha hit here: `[5b/8]`
  first appears in a source-block comment near the top — anchor on `# [5b/8] #707 B1`.
- Tier-0: the pure bash builders + the fake-curl / fake-python3 / fake-scp runners are driven by
  `tests/harness_cg_chain_e2e_1302.rs` (std-only — it RUNS locally with plain `rustc --test` +
  `CARGO_MANIFEST_DIR`, from a script file in a worktree); the scene helper is pytest
  (`tests/python/test_cg_chain_scene_1302.py`, a scripted fake OBS that refuses a program cut under
  anything but Cut).
- **The strih/stream hops (issue 1302 slice 2):** items 2 + 3 above — the `--cg-chain-burns` flag
  (`cg_chain_extract_burn_flag`, spliced as `${CG_CHAIN_BURN_FLAG:+"$CG_CHAIN_BURN_FLAG"}` into all
  three extract calls: strih-lx, Windows strih, stream — an empty flag adds NO argv word) and
  `cg_chain_merge_args_append` (which also feeds the on-box cg partial). `tests/harness_cg_chain_hops_1302.rs`
  pins both (std-only, runs with plain `rustc --test`); the verdict side is pinned by the
  `*_1302` tests in `recording-verdict.rs` (CI-only) and the `cg_chain_gate` unit tests (Tier-0 via
  a `#[path]` rustc replica with `burn_hold` + `recording_boundary_trim`). A verdict fixture must
  keep its CG frames clear of the 3-frame #575 lead/tail trim, or the trim silently drops them.

## Adding ANOTHER chain-origin/hop pair later

Reserve a fresh `BURN_RUN_ID_*` → add to `NODE_BURN_RUN_IDS` + the two python mirrors + every
`all_burns` array; a HOP that composites an OBS burn also needs a new `burn_geom::Corner` synced
across the #463 FOUR mirrors (burn-geom.hpp, colour_sample.rs, colour_scale.rs test,
burn_payload_parity.rs) + the burn-filter host-role map. Reuse `cg_chain_gate` (it is generic
per-hop contiguity+hold), never a parallel copy. The `recording-decode` skill's "new node role"
GOTCHA has the full ~5-site checklist.
