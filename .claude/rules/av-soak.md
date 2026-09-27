---
paths:
  - "scripts/av-soak.sh"
  - "scripts/lib/av-soak.sh"
  - "scripts/av_soak_decision.py"
  - "tests/python/test_av_soak_decision_1367.py"
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
| `scripts/av-soak.sh` | the orchestrator: `--plan` (DEFAULT, touches nothing), `--run`, `--report RUN_DIR` |
| `scripts/lib/av-soak.sh` | pure builders shared by plan AND run (argv of the two extracts + the merge), the window arithmetic, the two read-only cam2 reads, the record-volume free-space read |
| `scripts/av_soak_decision.py` | pure decision: `bounds`, `row` (one CSV row per window from the merged verdict JSON), `report` (1 h partial + full, exit 0 PASS / 1 FAIL / 2 UNKNOWN / 3 input error) |

Every rig action is an existing primitive: the issue-830 lease (own holder name
`camera-box-av-soak`, expected release = the whole run, so a CI E2E fails fast), the issue-281
heartbeat, `stray_session_check_assert` (before the burn-on and before EVERY StartRecord),
`obs_phase2.py record/switch/program-scene`, `obs_burn_filter.py check/add/remove`, the E2E sweep
(`switch_schedule.py plan/build`), `recording-verdict-on-strih-lx.sh` + `recording-verdict-on-stream.sh
--execute` (parallel, each under `timeout`), `recording-verdict --merge-partials`. The free-space
read copies the call shape of recording-e2e.sh's inline `check_recordings_free_space` into the lib
(never edit the harness for it).

## Hard rules

- **Measure-only.** Never a latency pin, an audio sync offset, `av_sync_calibrate --apply`,
  `qr_align`, measurement pins, an NDI mapping. Never a stream scene switch: the stream program must
  ALREADY be the development scene (`Development`, issue 1380 -- the soak refuses with exit 4
  otherwise; run `scripts/rig-mode.sh test` first). Only the strih program is swept, and it is
  restored to its snapshot at cleanup. The production scene name is never typed (the one
  declaration is `scripts/lib/stream-dev-scene.sh`).
- **Burns:** only the ones that were OFF are turned on, and exactly those are turned off again.
- **Recordings are not deleted** (deletion is owner-only, `.claude/rules/recordings-retention.md`):
  the exact paths go to `recordings.tsv` and `cleanup-plan.txt` (the E2E's own exact-path plan lines,
  `strih_lx_recording_cleanup_note` + the stream `Remove-Item`). Before every slot a record volume
  below `RECORDINGS_FREE_MIN_GB` (50) STOPS the run cleanly (UNKNOWN, never a false pass). At well
  under 1 GiB per box per window, 49 windows fit easily.
- **The bounds are read, never retyped:** `AV_OFFSET_GATE_TOLERANCE_MS` from `src/av_window.rs`,
  `SPREAD_THRESHOLD_MS` from `src/switch_latency.rs` (a missing constant = exit 3). The slope bound
  2 ms/h (`SLOPE_BOUND_MS_PER_H`) is the issue's acceptance, defined once in the decision module.
- **The verdict's own defaults are single sources too:** the soak never passes `--burn-*-run-id`
  (it deploys no burn) nor `--av-expected-ms` (unless `AV_EXPECTED_MS` is set explicitly).

## What a window measures (and what it cannot, passively)

- Window = ONE sweep over the soak cameras, `AV_SOAK_SEGMENT_SECS` (30, the E2E's calibrated
  `SEGMENT_SECS`) each: 7 cameras = 210 s. A per-camera A/V offset needs that camera on program and
  `av_window::MIN_AV_SAMPLES` (8) clustered markers, so a flat 60 s window cannot measure every camera.
  A window + 60 s must fit the slot (else exit 3).
- `av_<cam>_ms` = the verdict's MEASURED `all_cambox_av_sync.<cam>.av_offset_ms` only (a `derived`
  or `unknown` value is never a sample); graded `|offset - expected_ms| <= tolerance`, inclusive.
- Spreads -- three columns, the graded set is `--spread-columns` (default = the design as written):
  `source_spread_ms` / `delivery_spread_ms` are the verdict's gate spreads and need each camera's OWN
  capture burn (the probe-featured camera-box the E2E deploys in its cam1 / all-cambox deploy steps).
  TEST mode does not deploy it, so on a passive rig both are empty and a graded empty column is
  UNKNOWN. `av_spread_ms` = `max - min` of the measured per-camera A/V offsets, cam2 excluded (its
  number pools the whole recording) = the camera alignment AT THE STREAM OUTPUT, reported always.
  Which one to grade is the open design question on the issue (comment 5858491691); switching is
  `--spread-columns av_spread_ms`.
- Loss: the verdict's own per-segment `pass` of `all_cambox_continuity.segments` (the existing bar,
  its tolerances included, never re-derived) + raw copies/gaps/undecodable; `burn_<cam>` loss only
  when the capture burns were present.
- The painter is the PERMANENT `cam2-painter.service` (TEST mode). Its QR `run_id` is read from its
  own `frame-probe start:` journal line each slot (`--cam2-run-id`; 0 = unpinned when unreadable) and
  a tail of `/run/rig-qpsk-markers.csv` (header kept) is the window's emit log -- pairing works on a
  long log because only rows whose frame_id falls inside the recorded tick span pair.

## Grading (`report`)

Per series: FAIL on any out-of-bound sample or `|slope| > 2 ms/h`; UNKNOWN on no samples, fewer than
3 samples / under 30 min for the slope, or a sample gap over 660 s (the 600 s slot + 60 s start
jitter; the gap counts from the run's first window and to its last window, so a camera missing at
either end shows). The run: FAIL beats UNKNOWN beats PASS; a run whose window starts span less than
the required duration - 60 s is UNKNOWN. An operator-excluded camera (every window `excluded`) is
not required. The 1 h partial is graded on its own (as a 1 h run) and printed when the CSV extends
past it; the exit code is the FULL run's.

## Runbook (SUPERVISOR -- the lane never touches the rig)

Preconditions: rig in TEST mode (`bash scripts/rig-mode.sh test`), no E2E running (the lease is free:
`curl -s http://10.77.9.103:8890/rig-lease.json` -> `"held": false`), the CI artifacts of the commit
under test, and a 0600 env file `~/.config/camera-box/av-soak.env` with `CAM_PW`, `STREAM_USER`,
`STREAM_PW` (the values recording-e2e.sh uses, targets.md -- never committed).

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
systemd-run --user --unit=av-soak-1h --collect -p EnvironmentFile=$HOME/.config/camera-box/av-soak.env \
  --working-directory=$HOME/devel/camera-box \
  bash scripts/av-soak.sh --run --hours 1 --run-dir "$D" \
  --probe-bin-dir "$A/linux" --win-verdict-exe "$A/win/recording-verdict.exe"
journalctl --user -u av-soak-1h -f                # live log; the final report is its tail
cat "$D/report-latest.txt"                        # progress after every window
bash scripts/av-soak.sh --report "$D" --hours 1   # re-grade any time (exit 0/1/2)
```

The 8 h run is the same with `--unit=av-soak-8h --hours 8` (49 windows). **Stop:** `touch "$D/STOP"`
(ends at the next wait/slot boundary with full cleanup + the report, exit = the verdict) or
`systemctl --user stop av-soak-1h` (SIGTERM -> cleanup, exit 5). **What it holds:** the rig lease
for the whole run (a CI full-path E2E fails fast with `OUTCOME=RIG_LEASE_HELD`; restreamer waits a
bounded time), the strih program (swept), the burns it turned on, and a recording on strih + stream
for ~4 min of every 10. Post the report (`$D/report.txt` + `report.json`) on issue 1367, then hand
the owner `$D/cleanup-plan.txt` if the disk needs the space.

## Tier-0 verification (dev1: no cargo, no heavy checks)

`python3 -m pytest tests/python/test_av_soak_decision_1367.py tests/python/test_av_soak_orchestrator_1367.py`
(the orchestrator harness drives setup, one full window and cleanup with fakes behind the seams
`AV_SOAK_OBS_DIR`, `AV_SOAK_STRIH_DECODE`, `AV_SOAK_STREAM_DECODE`, `PROBE_BIN_DIR`, a fake
`sshpass`/`curl` on PATH, and a tmp `RIG_LEASE_DIR` + `CAMERA_BOX_RIG_HEARTBEAT` -- NEVER the real
`/var/tmp/rig-lease`, an E2E may hold it), `bash -n`, `shellcheck -S warning scripts/av-soak.sh
scripts/lib/av-soak.sh` (never `-x`).
