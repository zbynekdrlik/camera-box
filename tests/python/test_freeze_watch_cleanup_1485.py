"""Issue 1485 (5.11.2026): an early E2E abort must stop the in-run freeze watch in cleanup().

WHY: release E2E 42052075752 aborted at [5/8] (strih never started recording). The freeze watch
(scripts/lib/live-freeze-watch.sh, a background `while true` poll armed at [5/8 pre]) is stopped
only on the normal path at [7/8], so after the abort it kept running and held the job's stdout: the
job hung ~50 min until the loop was killed by hand.

What this pins: cleanup() stops the watch, right after the StopRecord-first block (it is an instant
kill, so it never delays the device restores) and before the heartbeat clear; the call reads the
pid file through `${FREEZE_WATCH_PID_FILE:-}` so a run that aborted before the variable was set
stays clean under `set -u`; and that call really ends a running watch.
"""
import os
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
E2E = REPO / "scripts" / "recording-e2e.sh"
LIB = REPO / "scripts" / "lib" / "live-freeze-watch.sh"
CALL = 'live_freeze_watch_stop "${FREEZE_WATCH_PID_FILE:-}"'


def _cleanup_body():
    s = E2E.read_text()
    return s[s.index("\ncleanup() {"):s.index("\ntrap cleanup EXIT HUP INT TERM")]


def test_cleanup_stops_the_freeze_watch_after_the_stoprecord_first_block():
    body = _cleanup_body()
    assert body.count(CALL) == 1, "cleanup() must stop the in-run freeze watch exactly once"
    call = body.index(CALL)
    assert body.index('record --host "$IMAG_IP" --action stop') < call < body.index("rig_heartbeat_stop")


def test_the_call_is_clean_when_the_watch_was_never_armed():
    script = 'set -euo pipefail\n. "$1"\n' + CALL + '\necho END\n'
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB)], capture_output=True, text=True,
                       timeout=30, env={k: v for k, v in os.environ.items() if k != "FREEZE_WATCH_PID_FILE"})
    assert r.returncode == 0 and r.stdout.strip() == "END", r.stdout + r.stderr


def test_the_call_ends_a_running_watch(tmp_path):
    fake = tmp_path / "bin"
    fake.mkdir()
    (fake / "python3").write_text("#!/bin/sh\nexit 0\n")
    (fake / "python3").chmod(0o755)
    pid_file = tmp_path / "freeze-watch.pid"
    env = dict(os.environ, PATH="%s:%s" % (fake, os.environ["PATH"]), FREEZE_WATCH_PID_FILE=str(pid_file))
    start = ('set -uo pipefail\nHERE=/nonexistent\n. "$1"\n'
             'live_freeze_watch_start "$FREEZE_WATCH_PID_FILE" "$2" 127.0.0.1 "NDI cam3" /x 1\n'
             'cat "$FREEZE_WATCH_PID_FILE"\n')
    # the loop inherits the starter's stdout: a pipe would never close (the very hang this fixes)
    out = tmp_path / "start.out"
    with open(out, "w") as fh:
        subprocess.run(["bash", "-c", start, "harness", str(LIB), str(tmp_path / "poison.txt")],
                       stdout=fh, stderr=subprocess.DEVNULL, timeout=30, env=env)
    pid = int(out.read_text().strip().splitlines()[-1])
    try:
        os.kill(pid, 0)
        stop = 'set -euo pipefail\n. "$1"\n' + CALL + '\n'
        r = subprocess.run(["bash", "-c", stop, "harness", str(LIB)], capture_output=True, text=True,
                           timeout=30, env=env)
        assert r.returncode == 0, r.stderr
        for _ in range(50):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            raise AssertionError("the watch loop survived the cleanup call")
        assert not pid_file.exists()
    finally:
        try:
            os.kill(pid, 9)
        except ProcessLookupError:
            pass
