---
paths:
  - "scripts/vban-receiver-bench.sh"
  - "scripts/vban_emulator.py"
  - "scripts/vban_emulator_plan.py"
  - "scripts/vban_sine_continuity.py"
  - "scripts/vban_bench_obs.py"
  - "scripts/vban_bench_log.py"
  - "tests/python/test_vban_*_1460.py"
---

# The VBAN receiver bench: an emulated lv1 against a temporary obs-vban input (issue 1460 slice 1)

The issue-1439 fix replaces the PP stream box's program audio path with lv1 VBAN into our obs-vban
receive source (a fixed reserve R, following lv1's clock), then the genlock ASRC, then the OBS mix.
lv1 is off at PP and the SNV stream box takes Dante, so the proof runs at SNV on strih-snv (the
same Linux OBS build family as stream-pp): an emulated lv1 on dev1 sends to a TEMPORARY obs-vban
input, OBS records it on a dedicated track, and the recording plus the OBS log are graded.
Main design: issue 1460 comment 6338685774 (Approach 1). What it verifies: the issue-1439
designs 6338127636 and 6338457001.

## The pieces

| File | Role |
|---|---|
| `scripts/vban_emulator_plan.py` | PURE planner: the VBAN PCM16 stereo 48 kHz, 256-frame packet schedule. Slot n = n x 256 / (48000 x (1 + ppm)) s, computed from n alone, so the average rate is exact. Profiles `clean`, `lv1` (+1.4 ppm, send gaps of 8-29 ms every ~20 s, shaped like the hub's measured lv1 gaps of 26-29 ms), `stress` (+50 ppm, gaps of 15-35 ms every ~6 s). Scheduled losses: a lost packet keeps its nuFrame and its samples, so the counter and the pilot run on. The pilot sine comes from the absolute sample index. The header layout is pinned to `scripts/vban_rate.py`'s parser by a test |
| `scripts/vban_emulator.py` | the sender: absolute deadlines on `time.monotonic_ns()`, `--dry-run` prints the schedule, `--schedule-out` writes it before the first packet. It refuses ports 6980 / 6981 / 6990 |
| `scripts/vban_sine_continuity.py` | the analysis: one channel through ffmpeg. Fits the pilot per 10 ms block. Reports every phase step (time, size in samples), every silence slot and the rate offset in ppm. `--schedule` grades it against the emulator's schedule. Exit 0 PASS / 1 FAIL / 2 ERROR / 3 usage |
| `scripts/vban_bench_obs.py` | the OBS side, over obs_phase2.py's ONE websocket client (`_conn` / `_rpc` with its production-scene guard): `preflight` (read-only, exit 4 = refused), `setup` (exit 4 = refused before any change, 2 = OBS unreachable, 5 = failed after a change), `teardown`. `OBS_PASSWORD` comes from the environment, never argv |
| `scripts/vban_bench_log.py` | grades the OBS log rows of one profile (lines after the issue-1360 reader's line-count mark) |
| `scripts/vban-receiver-bench.sh` | the orchestrator: `--plan` (DEFAULT, touches nothing), `--run`, `--cleanup RUN_DIR` |

## Hard rules

- **Only the QPSK marker may sound on the rig (owner).** The pilot reaches only the test input. That
  input feeds ONE dedicated track (default 6, 2..6 allowed). Never track 1, which is OBS mix 0: the
  program, the NDI output and the HDMI output audio. Never the stream track (`AdvOut TrackIndex`).
  Monitoring is off and read back. The emulator, the bench and `vban_bench_obs.py` refuse the ports
  a real receiver listens on (6980 hub / stream-role program, 6981 strih-role program, 6990 Janus)
  through the ONE list, `vban_emulator_plan.port_refusal`, and an `--extra-setting` may never set
  `port` / `stream_name` / `ip_from`.
- **Reads before writes.** `--run` refuses with exit 4 before any change when:
  - the lease is held;
  - the rig is not in TEST mode (`rig_mode_from_painter_snapshot` over the cam2 probe);
  - the rig is not PROVABLY idle (`av_soak_rig_busy_settled`, busy=false);
  - the OBS preflight refuses;
  - the UDP port is bound on strih-snv;
  - strih-snv has no ffmpeg (only the test track is pulled to dev1, never the program recording);
  - the record volume is under `RECORDINGS_FREE_MIN_GB` (the soak's `av_soak_free_space_verdict`;
    unreadable = one WARNING, the run goes on);
  - a credential is missing, or the strih is not the Linux strih-snv.

  After the first change every abort is exit 5, and cleanup undoes it. An unexpected error is exit
  5 after a change and 4 before one, never the FAIL code 1.
- **Before EVERY setup and EVERY StartRecord** (every profile, not only the first): a PROVEN idle
  rig (the settled broadcast read, `idle`), then the shared issue-1271 rig-busy guard
  (`stray_session_check_assert`, run in a subshell; it fails open on a fully unreadable rig, which
  the idle read refuses). Between, every poll checks the broadcast read: a live broadcast aborts.
- **Before every profile after the first** TEST mode and the free space are read again (an
  unreadable painter read is retried once after `VBAN_BENCH_MODE_RETRY_S`, 10 s). A rig that
  left TEST mode (the owner going to production), or a low volume, ends the run: full cleanup, the
  verdict `INCOMPLETE (N of M profiles ran)`, exit 1. A STOP file does the same. Only a run where
  every requested profile ran and passed is exit 0.
- **A recording is stopped only on a proven idle rig** (`av_soak_broadcast_of` = idle). During a
  broadcast it is KEPT, and so is the lease (exit 5). `--cleanup RUN_DIR` removes the test input
  first, then runs the soak's ONE leftovers path, `av_soak_stop_leftovers` (its two optional
  arguments: the bench's process pattern, and KEEP_LEASE when the teardown left something): a
  flagged recording is stopped only when `av_soak_rig_state.py leftovers` proves it is the bench's
  (its `recordTimecode` age against the flag time in `recording.state`), the stop is verified by a
  status read, and the lease is released only when nothing is left.
- **The emulator's own send lateness is graded.** A sender wake late by more than
  `--emulator-late-bound-ms` (5 ms) changes the profile's shape, so that profile is `INVALID`
  (bench.tsv column `emulator_late_ms`), never blamed on the receiver and never a PASS.
- **The test input is removed on every exit**, also during a broadcast: a silent input on a
  non-program track is safe to remove. `teardown` removes only an input whose kind, port and stream
  name are the bench's, and restores RecTracks only while it still holds the value setup wrote.
  After a setup that died between its re-program and its own preview restore, teardown puts the
  operator's preview back, but only while the preview still shows OBS's swap (the re-programmed
  scene). A scene staged since (maybe hours later, during a show, when `--cleanup` runs) is never
  touched, and after a completed setup the preview is never touched either.
- **An earlier run's input still there while RecTracks already holds the track is refused**: that
  run's original RecTracks is unknown, so it is never adopted as "the original". Finish that run
  with `--cleanup <its run dir>`, or remove the input and restore RecTracks by hand.
- **The lease:** repo `camera-box-vban-bench`, `RIG_LEASE_RESOURCES=strih-snv`, the rolling
  look-ahead kept alive by `rig_lease_refresh_if_mine` every poll, `RIG_LEASE_MAX_HOLD_SECS` = the
  declared run. The run id is set BEFORE the acquire, and cleanup releases it holder-checked on
  every exit except "something may be left".
- **Recordings are never deleted.** The exact-path `rm` lines go to `<run-dir>/cleanup-plan.txt`
  (`strih_lx_recording_cleanup_note`); deleting is the owner's step.

## Design facts the code relies on (verified in the vendored source)

- **Input kind** `net.nagater.obs-vban.source` (`ID_PREFIX "net.nagater.obs-vban."` + `"source"`).
  Settings `port`, `stream_name`, `ip_from`.
- **libobs mixes only a source that an output mix's active tree reaches.** Anything else is rendered
  for the meters only, and the issue-1381 guard re-anchors it. So the input is a scene item of the
  current PROGRAM scene.
- **In Studio Mode with scene duplication** (the OBS default) the program is a private copy, so a
  new item is not active until the scene is programmed again. Setup then sends ONE
  `SetCurrentProgramScene` of the same scene, waits for `GetSourceActive` (10 s), and puts back the
  preview the swap changed. That is the issue-343 same-scene hazard (a same-scene re-program can
  hang on a heavy NDI FIFO): it is bounded by obs_phase2's request deadline, and a timeout is a
  setup failure (exit 5, teardown).
- **`AdvancedOutput::SetupRecording` re-reads `AdvOut RecTracks` at every StartRecord** while no
  output is active, so ORing the track in over `SetProfileParameter` needs no OBS restart. The
  test track's audio stream in the file is `popcount(RecTracks & ((1 << (track - 1)) - 1))`.
- **strih-snv splits its recording every 15 min,** so `--minutes` is 2..14.
- **A FRESH input per profile.** The receiver and the ASRC counters start clean, and nuFrame starts
  at 0 again.
- **ASRC sign:** `asrc: source '<input>' estimated=` is the slope of raw vs master, so a FAST sender
  reads positive. It is compared with the injected ppm within `--ppm-bound` (5).

## The analysis (calibrated on a real AAC round trip, ffmpeg's aac at 160 kb/s like the OBS recording)

- A 256-sample hole survives AAC below -22 dB, at 16-sample resolution.
- The per-block phase wobbles about ±0.2 samples at 997 Hz. Steps are found with a two-sided
  window statistic (local maximum + sharpness, `MIN_JUMP_SAMPLES` 0.25), and the codec's onset
  transient is excluded.
- An inserted / dropped sample measures +1.00 / -1.00 samples. A silence that moved the timeline is
  bridged with the slope projection and reads as a step there.
- Grading: zero steps outside the scheduled losses; silence exactly at the scheduled losses (start
  within ±20 ms of the aligned slot, length within 64 samples); the pilot rate equal to the
  injected ppm within the bound; the lead (before the pilot) silent; the track otherwise clean
  (residual p99 under -30 dB).
- **A CONCEALED loss is graded by the timeline across it, never by silence.** The issue-1439
  receiver repeats the previous packet (crossfaded) for a run of at most
  `VBAN_RECV_CONCEAL_MAX_SLOTS` (3) lost slots between two received packets. `CONCEAL_MAX_SLOTS`
  in the script is pinned to that `#define` by a test.
  - The step detector cannot judge it: a repeat with the timeline intact reads +2..+6 samples of
    "step" inside one run of blocks (live lv1 29.10.2026: -1.3 / -1.7). So `analyze_scheduled`
    runs two passes: analyze, map every loss of <= 3 packets onto the recording, analyze again
    with those windows EXCLUDED (`analyze(exclude=[(lo, hi, loss index)])`, each widened by
    `CONCEAL_PAD`, 2 blocks). The run splits there and the existing slope-projection bridge
    measures the timeline across the window; the bridge names the loss in `conceal_windows`.
  - The mapping: r0 = the pilot onset - the frames the receiver held (its `receive emitting:
    source '<input>' frame=N` line, the first second it never plays), then refined by any silence
    slots the analysis found (the same fit the grade uses). The orchestrator reads N from the
    profile's own OBS log (`hold_frames_of`, the test input's line only) and passes
    `--hold-frames N`; absent = 187, the value every bench run logged.
  - Per loss of <= 3 packets with no silence slot: `concealed` when the bridge held (under
    `MIN_JUMP_SAMPLES`), else `timeline moved` (FAIL: a concealed loss is still a loss, the
    timeline must hold). A silence slot at such a loss is still acceptable (`ok`, the receiver
    decides at the deadline from what it stored). A loss over 3 packets without silence stays
    `no silence slot`. A bridge step at a concealed window is that loss's reason, never a second
    "phase step".

## The OBS log rows (`vban_bench_log.py`, the issue-1439 acceptance of designs 6338127636 + 6338457001)

- **FAIL** on any `buffering-guard: '<input>'` line (a pop).
- **FAIL** on no `asrc: source '<input>'` line, on `steps` / `place_jumps` above 0 on any line, on
  `basis` other than `stamp`, or on a `level_avg` more than 2 ms off `target` on any line after the
  first (the settle; the healthy band of `.claude/rules/asrc-residual-floor.md`).
- **FAIL** on the last `estimated` off the injected ppm.
- **FAIL** on the last `audio-telemetry #800 '<input>'` row's `mixed_late` above 0 (a per-source
  cumulative count; a fresh input starts at 0).
- **FAIL** on `late` / a re-anchor count above 0 on ANY status row.
- **FAIL** on the last status row's `lost` (or the stock source's `missing N packet(s)` sum)
  different from what the emulator dropped.

**Gotcha for any grader keyed on an input name:** libobs itself prints a line quoting EVERY audio
source once a minute (`audio-telemetry #800 '<src>': ... mixed_late=N`, plus the `asrc: source
'<src>'` line). "The last row that quotes the input and carries numbers" is therefore usually a
telemetry row, not the receiver's status row. The first draft of this grader did exactly that and
let a `late=2 reanchors=1` status row pass. Parse those two families separately, and select status
rows by their own keys.

The issue-1439 receiver's 10 s status line has no fixed text yet. A status row is a row naming the
input that carries a receive key (`received`, `lost`, `late`, `reanchors` / `re_anchors` /
`re-anchors`); the counters are read as cumulative. When issue 1439 ships, pin its exact keys here
and in a test.

**Known limit:** the `asrc:` line comes once a minute and the input is removed a few seconds after
the emulator stops, so the last up-to-60 s of a profile are not in any `asrc:` line. The sine
analysis covers the whole recording, so a step or a hole there is still caught; only an ASRC
`steps` / `place_jumps` count in that last minute is not.

## Runbook (SUPERVISOR, after the issue-1439 receiver is deployed on strih-snv)

Preconditions:
- TEST mode (`bash scripts/rig-mode.sh test`);
- no E2E or soak running (`curl -s http://127.0.0.1:8890/rig-lease.json` -> `"held": false`);
- the 0600 env file `~/.config/camera-box/av-soak.env` (it carries `CAM_PW`, `STRIH_USER`,
  `STRIH_PW`, the values recording-e2e.sh uses).

```bash
cd ~/devel/camera-box
D=$HOME/.camera-box/vban-bench/$(date -u +%Y%m%dT%H%MZ)
bash scripts/vban-receiver-bench.sh --plan --run-dir "$D"     # every step; touches nothing
systemd-run --user --unit=vban-bench --collect -p TimeoutStopSec=5min \
  -p EnvironmentFile=$HOME/.config/camera-box/av-soak.env \
  -p ExecStopPost="/bin/bash $HOME/devel/camera-box/scripts/vban-receiver-bench.sh --cleanup $D" \
  --working-directory=$HOME/devel/camera-box \
  bash scripts/vban-receiver-bench.sh --run --run-dir "$D" --minutes 10
journalctl --user -u vban-bench -f
cat "$D/bench.tsv"                 # one row per profile: sine, log, verdict, recording path
```

- Default profiles `clean lv1 stress:+50 stress:-50`, about 50 min, exit 0 only when every
  profile ran and its analysis AND log grade pass (and the emulator was on time).
- bench.tsv columns: `profile ppm sine log emulator_late_ms verdict recording`, the verdict
  `PASS` / `FAIL` / `INVALID` (the emulator was late).
- Add `--extra-setting <key>=<value>` for a setting the issue-1439 receiver adds (its fixed reserve
  R), once its key name is final.
- Per profile the run dir holds `pN-schedule.json`, `pN-audio.mkv`, `pN-analysis.json`,
  `pN-obs.log`, `pN-log.json`.
- **Stop early:** `touch "$D/STOP"` (ends after the running profile, full cleanup) or
  `systemctl --user stop vban-bench` (SIGTERM: the emulator stopped, StopRecord on an idle rig,
  teardown, exit 5).
- **Exit 5 with "lease KEPT"** = a recording or the test input may still be on strih-snv. Run
  `bash scripts/vban-receiver-bench.sh --cleanup "$D"` once the rig is idle.
- While it runs the bench holds the rig lease. A CI E2E arriving meanwhile waits for it (the
  rolling look-ahead) and fails after its 1800 s budget, so start the bench when no E2E is due.

## Later slice (not done here)

The E2E wiring calls this bench from `recording-e2e.sh` through a SOURCED helper lib (the issue-675
pattern), never an inline edit of that file's pinned anchors.

## Verification

`python3 -m pytest tests/python/test_vban_emulator_plan_1460.py tests/python/test_vban_sine_continuity_1460.py tests/python/test_vban_bench_obs_1460.py tests/python/test_vban_receiver_bench_1460.py`
plus `bash -n` and `shellcheck -S warning scripts/vban-receiver-bench.sh` (never `-x`).

- The orchestrator test drives the real script with fakes: `sshpass` / `ssh` on PATH, a fake
  `obs_phase2.py` + `vban_bench_obs.py` behind `VBAN_BENCH_OBS_DIR`, the REAL emulator in
  `--dry-run` behind `VBAN_BENCH_EMULATOR`, and a temp `RIG_LEASE_DIR`.
- The analysis and the log grade are the real scripts, on a recording synthesized from the
  planner's own pilot.
- The analysis tests include an ffmpeg AAC round trip. CI's `python-tests` job installs ffmpeg.
