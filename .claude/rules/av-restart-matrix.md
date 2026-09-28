---
paths:
  - "scripts/av-restart-matrix.sh"
  - "scripts/lib/av-restart-matrix.sh"
  - "scripts/av_restart_matrix_decision.py"
  - "tests/python/test_av_restart_matrix_decision_1367.py"
  - "tests/python/test_av_restart_matrix_orchestrator_1367.py"
  - "tests/python/test_av_soak_lease_inherit_1367.py"
---

# The restart matrix (issue 1367) -- restart one component, measure the stream output, 3/3

Owner goal (24.9.2026): after EVERY restart the stream output's A/V offset is right by itself and
the cameras stay in sync, with no manual step. Acceptance: for each restart kind, the stream output
meets the A/V and spread bounds within the settle time, 3/3 repeats. The matrix measures only.

## The pieces

| File | Role |
|---|---|
| `scripts/av-restart-matrix.sh` | orchestrator: `--plan` (DEFAULT, touches nothing), `--run`, `--report DIR`, `--stop-leftovers DIR` (the unit's ExecStopPost) |
| `scripts/lib/av-restart-matrix.sh` | pure builders shared by plan AND run: every restart / health / leave-running remote text, the health predicate, the window argv, the stream supervisor step |
| `scripts/av_restart_matrix_decision.py` | pure decision: `record` (one matrix.tsv step), `grade-window` (one window, the baseline gate), `report` (kinds 3/3 + baseline; exit 0 PASS / 1 FAIL / 2 UNKNOWN / 3 input error) |
| `scripts/av-soak.sh --lease-run-id` | the soak's one opt-in: a window under a lease its caller holds (verified every slot, never acquired or released; recording.state names no lease) |

## The contract

- **Every window IS the soak** (`av-soak.sh --run --hours 0 --lease-run-id <matrix lease>`): its
  reads-before-writes setup, the connect-on-show hold, ONE strih-program sweep recorded on strih +
  stream, the in-place decodes, the merge, its cleanup that a signal cannot cut short. Never extract
  or copy that step: `run_slot` is bound to the soak's setup/cleanup state. All soak rules apply to
  each window (`.claude/rules/av-soak.md`): TEST mode and the `Development` stream program are
  preconditions, never a pin/offset/correction/scene switch.
- **The lease is the matrix's** (repo `camera-box-av-restart-matrix`) for the whole run. Its
  heartbeat is bumped every <= `AV_MATRIX_KEEPALIVE_S` (10 s), also while a window runs (the
  issue-1383 keep-alive), and `expected_release_at` covers the whole run. The matrix does NOT run
  its own issue-281 rig heartbeat: each window runs it, and nested refreshers would share one
  pidfile. The burn-reconcile watchdog defers on the lease alone.
- **Before every mutation** (the baseline window, each restart): `stray_session_check_assert` AND a
  proven idle rig (`rig-busy-check` | `av_soak_rig_state.py broadcast` = idle, an unreadable read
  retried `AV_SOAK_BROADCAST_READS` x `AV_SOAK_BROADCAST_RETRY_S`). Busy / live / unreadable ->
  nothing is restarted (exit 4 before any change, 5 after).
- **One service restart per step, never a reboot.**
  - `strih-obs` reuses `mv_reverify_obs_restart_linux_cmd`: the unit-installed guard and a
    BLOCKING restart, so the old OBS is gone before the health read. `MV_REVERIFY_NO_UNIT` means
    not performed.
  - `cambox` / `dantesync` run `systemctl restart <unit>` as root on one camera.
  - Every text first echoes `AV_MATRIX_RESTART_AT=<the box's epoch>`. A restart counts only on its
    positive marker; no marker (ssh/auth/timeout) = not performed.
- **Healthy** (bounded `--healthy-timeout-secs`, polled; time to healthy = dev1 restart instant ->
  first healthy read):
  - `strih-obs`: unit active AND the OBS WebSocket answers.
  - `cambox`: active AND >= 1 `Streaming:` journal line since the box's OWN restart instant (box
    clock, `--since @<epoch>`).
  - `dantesync`: `:8898/status` -> `dantesync_clock_decision.py analyze` = OK on the rig
    grandmaster (it must resolve, else `--run` refuses).
  - `stream-obs`: the WebSocket answers.
- **The stream OBS kind is a SUPERVISOR step** (checked read-only 28.9.2026, win-stream-snv
  `Get-ScheduledTask`):
  - The canonical launch is `OBS Studio.lnk` -> `obs-guarded-launch.ps1` (the issue-786 gate).
  - Every interactive-token OBS task there (`OBSCorrect`, `Start OBS`, `Start OBS Studio`,
    `StartOBS`, `YTPlayStartOBS`, `OBSStream` = `--startstreaming`!) launches a bare `obs64.exe`.
  - ssh lands in session 0 (a GUI launch is banned), and `schtasks /it` is a dead end.
  - So the run writes `<run-dir>/supervisor-step-stream-obs-rN.txt`: run `launch-obs-genlock.sh
    --box stream --force` in the win-stream-snv MCP Shell, then `date +%s >
    <run-dir>/confirm-stream-obs-rN`.
  - The run waits `--supervisor-timeout-secs` (1800) for that file, then grades the repeat like the
    others. No confirmation = not performed (UNKNOWN) and the run stops.
  - Never wire one of those tasks in instead: they bypass the guarded launch.
- **Grading is POINTWISE** (one window = one sample; the soak's `evaluate` needs a series):
  - The soak's CSV row: the measured `av_<cam>_ms` against `av_expected_ms` within
    `AV_OFFSET_GATE_TOLERANCE_MS` (inclusive).
  - The graded spread (default `av_spread_ms`) within `SPREAD_THRESHOLD_MS`.
  - `loss_<cam>_pass`, `cont_overall_pass`, and the required strih/stream hop burns.
  - The bounds are read by `av_soak_decision.load_gate_bounds`. Missing evidence is UNKNOWN; a
    breach wins.
- **Step outcomes:**
  - `measured` -> the window's grade.
  - `not_healthy` and `restart_failed` -> FAIL, and the run stops.
  - `not_performed`, `window_refused` (soak exit 4) and `window_aborted` -> UNKNOWN; the run stops,
    or aborts (exit 5) for an aborted window.
  - A kind is PASS only 3/3. The matrix is FAIL on any FAIL (baseline included), PASS only when the
    baseline and every kind pass.
  - A baseline that is not PASS stops the run before any restart (`--keep-going` restarts anyway).
- **Cleanup** (signals ignored):
  - A running window gets SIGTERM and its own cleanup is waited for (it is recorded as
    `window_aborted`).
  - Every restarted kind (`<run-dir>/restarted`) is left running: `is-active || start`. The stream
    OBS is read, and the supervisor relaunch is printed when it does not answer.
  - Then the report, then the lease is released. It is KEPT while a window's `recording.state`
    still flags a recording: run `--stop-leftovers <run-dir>`, which runs `av-soak.sh
    --stop-leftovers` per such window and then releases the matrix's own lease, holder-checked.

## Known limits (this slice)

- The owner's "power cycle of any box" is not a kind: a remote cambox reboot is banned, so a power
  cycle is a physical step at the rig.
- `dantesync` restarts only a CAMERA node (root ssh, the camera credential).
  - strih-lx needs sudo.
  - A Windows node needs `Restart-Service` over ssh plus the tray.
  - Both would be additive kinds later.

## Runbook (SUPERVISOR -- the lane never touches the rig)

Preconditions:
- TEST mode (`bash scripts/rig-mode.sh test`).
- The lease is free: `curl -s http://10.77.9.103:8890/rig-lease.json` -> `"held": false`. A soak
  or an E2E holds it otherwise.
- The soak's 0600 env file `~/.config/camera-box/av-soak.env` with `CAM_PW`, `STREAM_USER`,
  `STREAM_PW`, `STRIH_USER`, `STRIH_PW`.
- The CI artifacts of the commit under test (the same download as `.claude/rules/av-soak.md`).

```bash
cd ~/devel/camera-box
SHA=$(git rev-parse HEAD)
RUN=$(gh run list --workflow ci.yml --json databaseId,headSha,conclusion \
  --jq ".[] | select(.headSha==\"$SHA\" and .conclusion==\"success\") | .databaseId" | head -1)
A=$HOME/.camera-box/av-soak/ci-$RUN
gh run download "$RUN" -n probe-tools-linux-amd64 --dir "$A/linux"
gh run download "$RUN" -n probe-tools-windows-amd64 --dir "$A/win"
chmod +x "$A/linux/recording-verdict"
bash scripts/av-restart-matrix.sh --plan          # review; touches nothing

D=$HOME/.camera-box/av-restart-matrix/$(date -u +%Y%m%dT%H%MZ)
systemd-run --user --unit=av-restart-matrix --collect -p TimeoutStopSec=20min \
  -p EnvironmentFile=$HOME/.config/camera-box/av-soak.env \
  -p ExecStopPost="/bin/bash $HOME/devel/camera-box/scripts/av-restart-matrix.sh --stop-leftovers $D" \
  --working-directory=$HOME/devel/camera-box \
  bash scripts/av-restart-matrix.sh --run --run-dir "$D" \
  --probe-bin-dir "$A/linux" --win-verdict-exe "$A/win/recording-verdict.exe"
journalctl --user -u av-restart-matrix -f          # SUPERVISOR STEP lines appear for stream-obs
cat "$D/report.txt"; bash scripts/av-restart-matrix.sh --report "$D"
```

- ~13 windows of ~8-10 min plus the restarts: about 3 h.
- `--kinds "strih-obs cambox dantesync"` runs only the automatic kinds; `--kinds stream-obs` alone
  runs the supervisor kind.
- On a stream-obs `SUPERVISOR STEP`: run the printed program in the win-stream-snv MCP Shell, then
  `date +%s > $D/confirm-stream-obs-rN`.
- Stop: `touch $D/STOP` (ends before the next restart, with the report) or `systemctl --user stop
  av-restart-matrix`.
- Afterwards `bash scripts/rig-mode.sh test` re-asserts TEST mode, because a restarted OBS may have
  lost a saved TEST burn:
  - the stream relaunch plan directs a burn sweep-off;
  - the burn-reconcile watchdog sweeps a resurrected burn once the lease is released.
- Post `$D/report.txt` on issue 1367.

## Tier-0 verification (dev1: no cargo)

`python3 -m pytest tests/python/test_av_restart_matrix_decision_1367.py
tests/python/test_av_restart_matrix_orchestrator_1367.py tests/python/test_av_soak_lease_inherit_1367.py`
plus the soak's own three files. The fakes:
- the orchestrator test fakes the window (`AV_MATRIX_SOAK`) with a script that writes the soak's
  REAL CSV row via `av_soak_decision.row_from_verdict`, checks the matrix's lease, and honours
  SIGTERM;
- `obs_phase2.py` (`AV_SOAK_OBS_DIR`), `sshpass` + `curl` on PATH;
- a tmp `RIG_LEASE_DIR`, never the real `/var/tmp/rig-lease`.

The lease-inherit tests reuse the soak's own `rig` fixture. `bash -n` and `shellcheck -S warning`
on the three scripts (never `-x`).
