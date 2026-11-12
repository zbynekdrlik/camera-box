"""Issue 1512 (12.11.2026 18:40, strih-snv): the operator closed the strih OBS from its window, OBS
hung after `==== Shutting down` (618 % CPU, the last line `[obs-vban] plugin unloaded`), the unit
stayed active and the panel's `systemctl --user start strih-obs.service` did nothing until a manual
SIGKILL, 20 min before the production.

Pinned: strih-obs-start.sh bounds OBS's own shutdown. Once this session's OBS log carries the
`==== Shutting down` line and OBS is still alive the bound later, the wrapper kills it and exits 0
(the operator asked for the close: Restart=on-failure must not bring it back). Before the kill it
captures the threads, the per-task wait channel and a best-effort gdb backtrace for the obs project
(obs#40). A clean close and a
running OBS are never touched; a crash keeps its own exit code.

Every bash call runs under the wrapper's `set -euo pipefail`; values are passed as ARGUMENTS.
"""
import os
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
START = REPO / "scripts" / "strih-obs-start.sh"


def _watch(tmp_path, log_text, obs_life_s, bound_s, before_launch=False):
    """Run strih_obs_shutdown_watch against a fake obs (a sleep) and a fake log dir."""
    logs = tmp_path / "logs"
    logs.mkdir()
    marker = tmp_path / "killed"
    if before_launch:  # an older session's log: written before the launch, must never be read
        old = logs / "2026-11-11 01-37-06.txt"
        old.write_text("18:40:41.461: ==== Shutting down ====\n")
        os.utime(old, (time.time() - 3600, time.time() - 3600))
    script = (
        'set -euo pipefail\n. "$1"\n'
        'since=$(date +%s)\n'
        'sleep "$2" & pid=$!\n'
        'if [ -n "$5" ]; then printf "%s" "$5" > "$3/2026-11-12 18-42-16.txt"; fi\n'
        'strih_obs_shutdown_watch "$pid" "$3" "$since" "$4" "$6"\n'
        'rc=0; wait "$pid" || rc=$?\n'
        'echo "OBS_RC=$rc"\n'
    )
    t0 = time.monotonic()
    r = subprocess.run(["bash", "-c", script, "harness", str(START), str(obs_life_s), str(logs),
                        str(bound_s), log_text, str(marker)],
                       capture_output=True, text=True, timeout=60)
    return r, marker, time.monotonic() - t0


def test_a_hung_shutdown_is_killed_after_the_bound_1512(tmp_path):
    r, marker, took = _watch(tmp_path, "18:40:41.461: ==== Shutting down ====\n"
                             "18:40:46.498: [obs-vban] plugin unloaded\n", 30, 3)
    assert r.returncode == 0, r.stderr
    assert "OBS_RC=137" in r.stdout, r.stdout  # SIGKILLed
    assert "WARN issue 1512" in r.stdout and "3 s" in r.stdout, r.stdout
    assert (marker / "obs-hang-tasks.txt").read_text().strip(), "the per-task list is captured before the kill"
    assert (marker / "obs-hang-threads.txt").exists() and (marker / "obs-hang-bt.txt").exists()
    assert (marker / "obs-hang-children.txt").exists()  # obs#40: are the CEF children still alive?
    assert took < 40, took


def test_a_running_obs_without_a_shutdown_line_is_never_killed_1512(tmp_path):
    r, marker, _ = _watch(tmp_path, "21:41:04.617: dante-input status: clock=media\n", 4, 1)
    assert r.returncode == 0, r.stderr
    assert "OBS_RC=0" in r.stdout and "WARN issue 1512" not in r.stdout, r.stdout
    assert not marker.exists()


def test_a_clean_close_inside_the_bound_is_left_alone_1512(tmp_path):
    r, marker, _ = _watch(tmp_path, "22:00:00.000: ==== Shutting down ====\n", 2, 20)
    assert r.returncode == 0, r.stderr
    assert "OBS_RC=0" in r.stdout and "WARN issue 1512" not in r.stdout, r.stdout
    assert not marker.exists()


def test_an_older_sessions_shutdown_line_is_never_read_1512(tmp_path):
    r, marker, _ = _watch(tmp_path, "", 4, 1, before_launch=True)
    assert "OBS_RC=0" in r.stdout and not marker.exists(), r.stdout + r.stderr


def test_the_wrapper_exits_0_after_its_own_kill_and_runs_the_watch_1512():
    text = START.read_text()
    tail = text[text.index('echo "supervising obs (pid $OBS_PID)'):]
    assert "strih_obs_shutdown_watch" in tail
    assert "STRIH_OBS_SHUTDOWN_BOUND_S" in tail
    assert tail.index("strih_obs_shutdown_watch") < tail.index('wait "$OBS_PID"')


def test_the_watch_reads_only_the_new_end_of_the_log_never_the_whole_file_1512():
    # review of the release PR: grepping the whole OBS log every second (hundreds of MB after a
    # multi-day session) loads a broadcast box; the watch reads only what was appended since its last look.
    text = START.read_text()
    body = text[text.index("strih_obs_shutdown_watch() {"):]
    body = body[:body.index("\n}\n")]
    assert "tail -c" in body, body
    assert "grep -qF '==== Shutting down' \"$log\"" not in body, body


def test_a_shutdown_line_written_in_two_pieces_is_still_seen_1512(tmp_path):
    logs = tmp_path / "logs"
    logs.mkdir()
    dump = tmp_path / "dump"
    script = (
        'set -euo pipefail\n. "$1"\n'
        'since=$(date +%s)\n'
        'sleep 30 & pid=$!\n'
        'f="$2/2026-11-12 18-42-16.txt"; head -c 200000 /dev/zero | tr "\\0" x > "$f"; echo >> "$f"\n'
        '( sleep 1.5; printf "18:40:41.461: ==== Shut" >> "$f"; sleep 1.5; printf "ting down ====\\n" >> "$f" ) &\n'
        'strih_obs_shutdown_watch "$pid" "$2" "$since" 2 "$3"\n'
        'rc=0; wait "$pid" || rc=$?\necho "OBS_RC=$rc"\n'
    )
    r = subprocess.run(["bash", "-c", script, "h", str(START), str(logs), str(dump)],
                       capture_output=True, text=True, timeout=60)
    assert "OBS_RC=137" in r.stdout and dump.is_dir(), r.stdout + r.stderr
