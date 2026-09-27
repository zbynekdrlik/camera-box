---
paths:
  - "scripts/av-soak.sh"
  - "scripts/lib/av-soak.sh"
  - "scripts/av_soak_decision.py"
  - "scripts/av_soak_rig_state.py"
  - "tests/python/test_av_soak_decision_1367.py"
  - "tests/python/test_av_soak_rig_state_1367.py"
  - "tests/python/test_av_soak_orchestrator_1367.py"
---

# The 8 h stream-output A/V soak (issue 1367) -- measure-only harness + runbook

Owner goal (24.9.2026): on the **stream OBS output** the A/V offset and the camera-to-camera
alignment hold for 8 h with no drift (a real drift shows after ~1 h), with zero loss. The full-path
E2E is a ~300 s snapshot that also CORRECTS the rig in its cleanup (the A/V apply, the per-run pin
align), so looping it would hide the drift. The soak only measures.

## The pieces

| File | Role |
|---|---|
| `scripts/av-soak.sh` | the orchestrator: `--plan` (DEFAULT, touches nothing), `--run`, `--report RUN_DIR`, `--stop-leftovers RUN_DIR` (the unit's `ExecStopPost` safety net) |
| `scripts/lib/av-soak.sh` | pure builders shared by plan AND run (argv of the two extracts + the merge), the window arithmetic, the two read-only cam2 reads, the record-volume free-space read |
| `scripts/av_soak_decision.py` | pure decision: `bounds`, `row` (one CSV row per window from the merged verdict JSON), `report` (1 h partial + full, exit 0 PASS / 1 FAIL / 2 UNKNOWN / 3 input error) |
| `scripts/av_soak_rig_state.py` | pure rig-state decisions over one `rig-busy-check` read: `broadcast` (live / unknown / idle -- may cleanup cut the strih program?) and `leftovers` (the `--stop-leftovers` plan: which flagged recording is provably the soak's) |
| `bundle_state_gather.recordings_free_line` | the one "<VERDICT> <free_gb>" line the free-space read prints (recording-e2e.sh keeps its inline copy -- static-anchor minefield) |

Every rig action is an existing primitive: the issue-830 lease (own holder name
`camera-box-av-soak`, expected release = the whole run, so a CI E2E fails fast), the issue-281
heartbeat, `stray_session_check_assert` (at setup, before the setup mutations, before EVERY
StartRecord), the issue-1242 connect-on-show HOLD the E2E uses (`connect_on_show_e2e_hold` /
`_wait_live` / `_restore` in `scripts/lib/connect-on-show-hold.sh`, the strih-side 4 h marker
re-asserted every slot), `obs_phase2.py record/switch/program-scene`, `obs_burn_filter.py
check/add/remove`, the E2E sweep (`switch_schedule.py plan/build`), `recording-verdict-on-strih-lx.sh`
+ `recording-verdict-on-stream.sh --execute` (parallel, each under `timeout`), `recording-verdict
--merge-partials`.

## Hard rules

- **Measure-only.** Never a latency pin, an audio sync offset, `av_sync_calibrate --apply`,
  `qr_align`, measurement pins, an NDI mapping. Never a stream scene switch: the stream program must
  ALREADY be the development scene (`Development`, issue 1380 -- refused with exit 4 otherwise; run
  `scripts/rig-mode.sh test` first) and is re-read every slot (a drift = a skipped row). Only the
  strih program is swept; the sweep flag is set BEFORE the first cut, so the snapshot is restored
  (`switch --prod-floor`) even when a cut fails; an unreadable snapshot refuses the run. The
  production scene name is never typed (the one declaration is `scripts/lib/stream-dev-scene.sh`).
- **Reads before writes.** Setup does every read (guard, both program scenes, painter, every burn
  state) before the first mutation; a refusal there is exit 4 and nothing changed. After the first
  mutation (the connect-on-show hold) every abort is exit 5 and cleanup restores.
- **Recording flags are conservative.** A box's "started" flag is set BEFORE `record --action start`
  (the start verifies the file grows AFTER StartRecord, so a failed or timed-out start can leave OBS
  recording); any start failure stops both boxes; a flag clears only when `record --action status`
  reads `active=False`, else cleanup stops it again. A recording that still reads active after a
  retried stop aborts the run, and cleanup prints `RECORDING MAY STILL BE RUNNING on <box>` and
  exits 5 -- never a clean exit over a live recording -- and KEEPS the rig lease (no E2E may
  start over it). `<run-dir>/recording.state` holds each flag, the time it was set (right before
  its StartRecord) and the lease run id.
- **`--stop-leftovers <run-dir>`** (the unit's `ExecStopPost`) runs the pure `leftovers` plan.
  Strih never streams, so "recording and not streaming" on strih alone is strih's NORMAL
  broadcast state (Companion records both boxes) -- a per-box check proves nothing. So: nothing
  is touched while ANY box streams or a box is unreadable; a flagged box's recording is stopped
  only when its own `recordTimecode` age puts its start in [flag time - 5 s, flag time +
  `AV_SOAK_OBS_TIMEOUT_S` + 30 s]; anything else is kept with the reason (exit 5, lease kept).
  With nothing left it releases the soak's lease (holder-checked, exit 0). It refuses (exit 4)
  while the soak's own process (`<run-dir>/pid`) still runs.
- **Cleanup cannot be cut short** (the issue-808 recipe, `.claude/rules/ci-testing-gotchas.md`):
  `set +e; trap '' INT TERM HUP PIPE`, every OBS/burn/connect-on-show/ssh call in its own session
  (`setsid -w`), background sleeps/decodes killed. The second-SIGTERM and process-group Ctrl-C
  tests prove the `trap ''` half; `setsid -w` is defence in depth the fakes cannot isolate (GNU
  `timeout` already starts its child in its own process group).
- **Cleanup never cuts a broadcast.** Strih's program feeds the stream box's program, so the
  strih program is restored only when `broadcast` reads a proven `idle` rig; `live` or
  `unknown` leaves it on the last sweep scene with a loud `strih program NOT restored` line (the
  issue-1271 rule). Burns and connect-on-show still go back: that returns production state.
- **A decode is stopped on the box too.** Killing the local `timeout` does not stop a remote
  `recording-verdict`: on a decode timeout, and in cleanup while a decode runs, the soak stops
  THIS run's decode, matched by its own output name `av-soak-<stamp>-s...`
  (`pkill -f 'recording-verdic[t] --extract-partial strih .*av-soak-<stamp>-s'` on strih-lx --
  the bracket keeps pkill off its own shell, the issue-626 self-match; a `Win32_Process`
  `CommandLine` match + `Stop-Process -Id` on the stream box, else the next slot's exe upload
  meets a locked file).
- **A run dir holds one run.** `--run` refuses (exit 4, before the lease) a dir that already has
  `pid`, `soak.csv` or `recording.state`: a second run there would reset the first one's flags
  and grade two runs as one series.
- **Burns:** only the ones that were OFF are turned on, and exactly those are turned off again.
- **Recordings are not deleted** (deletion is owner-only, `.claude/rules/recordings-retention.md`):
  the exact paths (incl. the ones cleanup stopped) go to `recordings.tsv` and `cleanup-plan.txt` (the
  E2E's own exact-path plan lines, `strih_lx_recording_cleanup_note` + the stream `Remove-Item`).
  Before every slot a record volume below `RECORDINGS_FREE_MIN_GB` (50) STOPS the run cleanly
  (UNKNOWN, never a false pass).
- **The slot budget is checked up front:** window + decode + merge + overhead (90 s) must fit the
  slot; the decode bound defaults to what the slot leaves (210 s for 7 x 30 s in 600 s), merge 90 s,
  and a decode bound under 60 s refuses the run (exit 3, the message names the budget that does not
  fit). Every slot writes a `timing.tsv` line -- read it after the 1 h run before trusting the
  budget.
- **The bounds are read, never retyped:** `AV_OFFSET_GATE_TOLERANCE_MS` from `src/av_window.rs`,
  `SPREAD_THRESHOLD_MS` from `src/switch_latency.rs` (a missing constant = exit 3 for `report`, exit
  4 before `--run` starts). The slope bound 2 ms/h (`SLOPE_BOUND_MS_PER_H`) is the issue's
  acceptance, defined once in the decision module.
- **The verdict's own defaults are single sources too:** the soak never passes `--burn-*-run-id`
  (it deploys no burn) nor `--av-expected-ms` (unless `AV_EXPECTED_MS` is set explicitly).

## What a window measures (and what it cannot, passively)

- Window = ONE sweep over the soak cameras, `AV_SOAK_SEGMENT_SECS` (30, the E2E's calibrated
  `SEGMENT_SECS`) each: 7 cameras = 210 s. A per-camera A/V offset needs that camera on program and
  `av_window::MIN_AV_SAMPLES` (8) clustered markers, so a flat 60 s window cannot measure every camera.
  The connect-on-show hold keeps every camera's main input connected, so each cut is warm, as in the
  E2E (measuring cold cuts would be a separate decision).
- `av_<cam>_ms` = the verdict's MEASURED `all_cambox_av_sync.<cam>.av_offset_ms` only (a `derived`
  or `unknown` value is never a sample); graded `|offset - expected_ms| <= tolerance`, inclusive.
- Spreads -- three columns, the graded set is `--spread-columns` (default = the design as written):
  `source_spread_ms` / `delivery_spread_ms` are the verdict's gate spreads and need each camera's OWN
  capture burn (the probe-featured camera-box the E2E deploys in its cam1 / all-cambox deploy steps).
  TEST mode does not deploy it, so on a passive rig both are empty and a graded empty column is
  UNKNOWN. `av_spread_ms` = `max - min` of the measured per-camera A/V offsets, cam2 excluded (its
  number pools the whole recording) = the camera alignment AT THE STREAM OUTPUT, reported always
  (note: a difference of two A/V medians carries both medians' noise). Which one to grade is the
  open design question on the issue (comment 5858491691); switching is `--spread-columns av_spread_ms`.
- Loss: the gate's OWN per-window term (`gate_window_term`) over the camera's segments -- the
  src/window_gate.rs `decide_with_tolerance(...).overall_pass_term` with the multi-source scope
  (a `multi_source` window's copies/gaps dropped, presence + the optical floor still gate), read
  from the verdict's own serialized seam flags. NOT `relaxed_pass`: that field is a different,
  looser term. The AND of `gate_window_term` equals the verdict's `overall_pass` on 25 real local
  verdicts, all within the run-wide floor (four trimmed into `tests/python/fixtures/av_soak/`).
  An older verdict without the flags falls back to `relaxed_pass`, then `pass`. The strict
  `pass` and raw copies/gaps/undecodable are recorded.
- Continuity gate: the verdict's OWN `all_cambox_continuity.overall_pass` per window. It adds
  what the per-window term cannot see -- the run-wide undecodable sum over
  `RUN_UNDECODABLE_FLOOR` (`loss_run_wide_pass`, src/probe/recording_segments.rs) and an empty
  schedule. A window where the soak's mirrored terms (every camera's loss term AND the run-wide
  term) disagree with the fold is counted (`gate_term_mismatch_windows`) and named: the Python
  copy of the Rust term drifted, so the live data itself checks the copy.
- Burn loss: `full_chain.loss.<node>.zero_loss` for the `strih` + `stream` hops (the OBS measurement
  burns the soak turns on -- the zero-loss signal at the stream output; REQUIRED, a hop never
  measured is UNKNOWN) and for each camera (only with capture burns; a camera never measured is
  not required). Once measured, a gap counts.
- The painter is the PERMANENT `cam2-painter.service` (TEST mode). Its QR `run_id` is read from its
  own `frame-probe start:` journal line each slot (`--cam2-run-id`; 0 = unpinned when unreadable) and
  a tail of `/run/rig-qpsk-markers.csv` (header kept) is the window's emit log -- pairing works on a
  long log because only rows whose frame_id falls inside the recorded tick span pair.

## Grading (`report`)

Per series: FAIL on any out-of-bound sample or a slope confidently over the bound
(`|slope| - t*SE > 2 ms/h`, t = the two-sided 95 % Student-t quantile for n - 2 degrees of freedom:
2.57 at the 1 h run's 7 windows, 2.02 at the 8 h run's 49); PASS needs `|slope| + t*SE <= 2 ms/h`;
UNKNOWN on no samples, fewer than 3 samples / under 30 min, a slope interval that straddles the
bound (per-window noise of ~2 ms over 1 h makes t*SE about 5.8 ms/h, so the 1 h check can prove a
clear drift but rarely a pass -- over 8 h the same noise is ~0.25 ms/h), or a sample gap over the
CSV's `slot_s` + 60 s (counted from the run's first window and to its last window). The run: FAIL
beats UNKNOWN beats PASS; a run whose window starts span less than the required duration - 60 s is
UNKNOWN; a run where nothing was graded is UNKNOWN.
An operator-excluded camera (every window `excluded`) is not required; a `continuity gate` never
measured (an older verdict) is NOT_MEASURED, never required. The 1 h partial is graded on
its own as soon as the CSV covers the first hour; the exit code is the FULL run's.

## Runbook (SUPERVISOR -- the lane never touches the rig)

Preconditions: rig in TEST mode (`bash scripts/rig-mode.sh test`), no E2E running (the lease is free:
`curl -s http://10.77.9.103:8890/rig-lease.json` -> `"held": false`), the CI artifacts of the commit
under test, and a 0600 env file `~/.config/camera-box/av-soak.env` with `CAM_PW`, `STREAM_USER`,
`STREAM_PW`, `STRIH_USER`, `STRIH_PW` (the values recording-e2e.sh uses, targets.md -- never
committed). `--run` refuses (exit 4) when any of the five is missing.

```bash
cd ~/devel/camera-box
SHA=$(git rev-parse HEAD)
RUN=$(gh run list --workflow ci.yml --json databaseId,headSha,conclusion \
  --jq ".[] | select(.headSha==\"$SHA\" and .conclusion==\"success\") | .databaseId" | head -1)
A=$HOME/.camera-box/av-soak/ci-$RUN
gh run download "$RUN" -n probe-tools-linux-amd64 --dir "$A/linux"
gh run download "$RUN" -n probe-tools-windows-amd64 --dir "$A/win"
chmod +x "$A/linux/recording-verdict"
bash scripts/av-soak.sh --plan --hours 1          # review the exact steps; touches nothing

# the 1 h trend check (7 windows, +0 .. +60 min), detached from the Claude session:
D=$HOME/.camera-box/av-soak/1h-$(date -u +%Y%m%dT%H%MZ)
systemd-run --user --unit=av-soak-1h --collect -p TimeoutStopSec=15min \
  -p EnvironmentFile=$HOME/.config/camera-box/av-soak.env \
  -p ExecStopPost="/bin/bash $HOME/devel/camera-box/scripts/av-soak.sh --stop-leftovers $D" \
  --working-directory=$HOME/devel/camera-box \
  bash scripts/av-soak.sh --run --hours 1 --run-dir "$D" \
  --probe-bin-dir "$A/linux" --win-verdict-exe "$A/win/recording-verdict.exe"
journalctl --user -u av-soak-1h -f                # live log; the final report is its tail
cat "$D/report-latest.txt" "$D/timing.tsv"        # progress + per-slot timing after every window
bash scripts/av-soak.sh --report "$D" --hours 1   # re-grade any time (exit 0/1/2)
```

**A passive TEST-mode run cannot PASS on the default graded spreads:** `source_spread_ms` /
`delivery_spread_ms` need the capture burns the soak does not deploy, so they are empty and the
run is UNKNOWN by construction until the open design question (comment 5858491691) is answered.
Every other series is still graded; to read the passive verdict, re-grade with the stream-output
spread: `bash scripts/av-soak.sh --report "$D" --hours 1 --spread-columns av_spread_ms`.

The 8 h run is the same with `--unit=av-soak-8h --hours 8` (49 windows). **Stop:** `touch "$D/STOP"`
(ends at the next wait/slot boundary with full cleanup + the report, exit = the verdict) or
`systemctl --user stop av-soak-1h` (SIGTERM -> cleanup, exit 5; `TimeoutStopSec` leaves cleanup
its time; if systemd still has to SIGKILL it, `ExecStopPost` stops a recording the run left).
Exit 5 with `RECORDING MAY STILL BE RUNNING` in the log = check that box before anything else;
the soak kept the rig lease, and `bash scripts/av-soak.sh --stop-leftovers "$D"` (run again
once the box is idle) stops what is provably the soak's and releases it.
**What it holds:** the rig lease for the whole run (a CI full-path E2E fails fast with
`OUTCOME=RIG_LEASE_HELD`; restreamer waits a bounded time), connect-on-show held off on strih (full
bandwidth for every camera, as during an E2E), the strih program (swept), the burns it turned on,
and a recording on strih + stream for ~4 min of every 10. Post the report (`$D/report.txt` +
`report.json`) on issue 1367, then hand the owner `$D/cleanup-plan.txt` if the disk needs the space.

## Tier-0 verification (dev1: no cargo, no heavy checks)

`python3 -m pytest tests/python/test_av_soak_decision_1367.py tests/python/test_av_soak_rig_state_1367.py tests/python/test_av_soak_orchestrator_1367.py`
(the decision tests use REAL merged verdict fixtures from `tests/python/fixtures/e2e_discord_report/`
where the shape matters; the orchestrator harness drives setup, full windows, the failure paths
(start/stop failure, a recording that never stops, a failed cut, SIGTERM and a second SIGTERM
during cleanup, a process-group Ctrl-C, SIGTERM during a decode, the STOP file, a low volume, a
painter/stream-program drift, a broadcast that starts mid-run, `--stop-leftovers` never touching
a broadcast, an unreadable rig or a recording the soak did not start) and cleanup with
fakes behind the seams `AV_SOAK_OBS_DIR`,
`AV_SOAK_STRIH_DECODE`, `AV_SOAK_STREAM_DECODE`, `PROBE_BIN_DIR`, `CONNECT_ON_SHOW_MARKER_CMD`,
`CONNECT_ON_SHOW_LOG_READ_CMD`, a fake `sshpass`/`curl` on PATH, and a tmp `RIG_LEASE_DIR` +
`CAMERA_BOX_RIG_HEARTBEAT` + `CONNECT_ON_SHOW_HOLD_STATE` -- NEVER the real `/var/tmp/rig-lease`, an
E2E may hold it), `bash -n`, `shellcheck -S warning scripts/av-soak.sh scripts/lib/av-soak.sh` (never
`-x`).
