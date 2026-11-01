"""issue 1387: the frozen-input watchdog reads the receiver's OBS log ONCE per pass, at nice 19.

The owner's 1.11.2026 rule: no measurement and no watchdog may risk a frame on a broadcast box. The
watchdog read the receiver log once for its enumeration and again for every watched input: on the
JPEG XS strih that was 1 + 5 ssh logins and tail runs every 5 minutes. Now one bounded read per pass
feeds the enumeration and every per-source `received=` extraction, and the Linux reader runs its file
commands at nice 19. A direct `probe_received` call outside a pass still reads by itself (the issue
1258 / 1259 / 1360 harnesses call it that way).

Tier-0: the real watchdog with NO probe/enumerate seams. A PATH-stubbed `sshpass` logs its argv and
execs the rest; a PATH-stubbed `ssh` logs one line per login and RUNS the Linux remote command against
a fixture HOME, so the number of logins and the commands that ran on the "box" are both observable.
"""
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
WATCHDOG = _ROOT / "scripts" / "frozen-input-alert-watchdog.sh"
READER = _ROOT / "scripts" / "lib" / "strih-log-read.sh"

_SCRUB = ("E2E_PROGRAM_PATH", "CAMERA_STRIH_JXS_SET", "STRIH_PROG_SOURCE", "STREAM_PROG_SOURCE", "OBS_FLEET",
          "STRIH_BOXES_DIR", "CAMERA_ACTIVE_SET", "FROZEN_INPUT_SOURCES", "FROZEN_INPUT_ENUMERATE",
          "FROZEN_INPUT_RECEIVER", "FROZEN_INPUT_SENDER", "FROZEN_INPUT_STRIH_HOST", "FROZEN_INPUT_STREAM_HOST",
          "FROZEN_INPUT_PROBE_CMD", "FROZEN_INPUT_ENUMERATE_CMD", "FROZEN_INPUT_ALERT_STATE_FILE",
          "STRIH_PLATFORM")

SSHPASS = """#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$STUB_DIR/sshpass.log"
shift 2
exec "$@"
"""

# one line per login; the last argument is the remote command, run for real against the fixture HOME
# (an empty FAIL_READ file makes the "box" answer nothing, the failed-read case)
SSH = """#!/usr/bin/env bash
printf 'login\\n' >> "$STUB_DIR/ssh.log"
for last; do :; done
printf '%s\\n' "$last" >> "$STUB_DIR/remote.log"
[ -e "$STUB_DIR/FAIL_READ" ] && exit 255
HOME="$STUB_HOME" exec bash -c "$last"
"""

NOTIFY = """#!/usr/bin/env python3
import os, sys
with open(os.environ["STUB_NOTIFY_LOG"], "a") as f:
    f.write("---NOTIFY---\\n" + "\\n".join(sys.argv[1:]) + "\\n")
"""


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


def _audit(src, n):
    return f"12:00:00.000: genlock-fifo audit '{src}': received={n} consumed=1 locked=1\n"


class Box:
    def __init__(self, tmp_path):
        self.dir = tmp_path
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        _exe(self.bin / "sshpass", SSHPASS)
        _exe(self.bin / "ssh", SSH)
        self.home = tmp_path / "home"
        self.logs = self.home / ".config" / "obs-studio" / "logs"
        self.logs.mkdir(parents=True)
        self.state = tmp_path / "state"
        self.state.mkdir()
        self.notify = _exe(tmp_path / "notify.py", NOTIFY)
        self.notify_log = tmp_path / "notify.log"
        self.notify_log.write_text("")
        self.counter = 1000

    def write_log(self, sources):
        """the newest OBS log (a spaced name, like the real one) with one audit line per source"""
        self.counter += 500
        body = "".join(_audit(s, self.counter) for s in sources)
        (self.logs / "2026-11-01 12-00-00.txt").write_text("12:00:00.000: OBS start\n" + body)

    def logins(self):
        p = self.dir / "ssh.log"
        return len(p.read_text().splitlines()) if p.exists() else 0

    def remote(self):
        p = self.dir / "remote.log"
        return p.read_text() if p.exists() else ""

    def reset_logs(self):
        for name in ("ssh.log", "remote.log", "sshpass.log"):
            (self.dir / name).unlink(missing_ok=True)

    def run(self, env):
        e = {k: v for k, v in os.environ.items() if k not in _SCRUB}
        e.update({"PATH": f"{self.bin}:{os.environ['PATH']}", "STUB_DIR": str(self.dir),
                  "STUB_HOME": str(self.home), "AIRULESET_NOTIFY": str(self.notify),
                  "STUB_NOTIFY_LOG": str(self.notify_log), "FROZEN_INPUT_ALERT_STATE_DIR": str(self.state),
                  "FROZEN_INPUT_ALERT_STATE_FILE": str(self.state / "wd.state"),
                  "FROZEN_INPUT_NETREACH_STATE_FILE": str(self.state / "netreach.state"),
                  "FROZEN_INPUT_ALERT_CONFIRM_THRESHOLD": "1"})
        e.update(env)
        r = subprocess.run(["bash", str(WATCHDOG)], capture_output=True, text=True, env=e, timeout=120,
                           cwd=str(_ROOT))
        assert r.returncode == 0, r.stdout + r.stderr
        return r


STRIH_SNV = {"FROZEN_INPUT_RECEIVER": "strih|10.77.9.202", "FROZEN_INPUT_SENDER": "strih",
             "FROZEN_INPUT_ENUMERATE": "1", "FROZEN_INPUT_ALERT_TAG": "#1069"}
STREAM_SNV = {"FROZEN_INPUT_RECEIVER": "stream|10.77.9.204", "FROZEN_INPUT_SENDER": "strih"}
ON_AIR = ("JXS cam2 src", "JXS cam3 src", "JXS cam5 src", "NDI cam3")


def test_a_strih_pass_logs_in_once_for_the_enumeration_and_every_watched_input(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    r = box.run(STRIH_SNV)
    assert box.logins() == 1, f"one ssh login per pass, got {box.logins()}:\n{r.stderr}"
    # the one read still fed the enumeration and every declared + enumerated input
    assert "enumerated" in r.stderr and "NDI cam3" in r.stderr, r.stderr
    for src in ON_AIR:
        assert f"'{src}' on strih: prev=<none> curr=1500 " in r.stderr, r.stderr


def test_the_second_pass_reads_once_again_and_grades_every_input_advancing(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    box.run(STRIH_SNV)
    box.reset_logs()
    box.write_log(ON_AIR)
    r = box.run(STRIH_SNV)
    assert box.logins() == 1, r.stderr
    for src in ON_AIR:
        assert f"'{src}' on strih: prev=1500 curr=2000 live=1 sender_reachable=1 -> ADVANCING" in r.stderr, r.stderr


def test_a_frozen_input_still_pages_from_the_shared_read(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    box.run(STRIH_SNV)
    # every input advances except JXS cam3 src, whose counter stays where the first pass left it
    box.counter += 500
    body = "".join(_audit(s, box.counter if s != "JXS cam3 src" else 1500) for s in ON_AIR)
    (box.logs / "2026-11-01 12-00-00.txt").write_text(body)
    r = box.run(STRIH_SNV)
    assert "'JXS cam3 src' on strih: prev=1500 curr=1500 live=1 sender_reachable=1 -> FROZEN" in r.stderr
    assert "frozen-input-strih-JXS cam3 src" in box.notify_log.read_text()


def test_the_stream_instance_reads_once_too(tmp_path):
    box = Box(tmp_path)
    box.write_log(("JXS PGM src",))
    r = box.run(STREAM_SNV)
    assert box.logins() == 1, r.stderr
    assert "'JXS PGM src' on stream: prev=<none> curr=1500 " in r.stderr, r.stderr


def test_a_failed_read_leaves_every_input_unknown_without_a_retry_per_input(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    box.run(STRIH_SNV)
    box.reset_logs()
    (tmp_path / "FAIL_READ").write_text("")
    r = box.run(STRIH_SNV)
    assert box.logins() == 1, r.stderr
    assert "FROZEN" not in r.stderr and box.notify_log.read_text() == "", r.stderr
    # the enumeration found nothing, so only the declared JPEG XS inputs are graded -- all UNKNOWN
    for src in ("JXS cam2 src", "JXS cam3 src", "JXS cam5 src"):
        assert f"'{src}' on strih: prev=1500 curr=<none> " in r.stderr and "-> UNKNOWN" in r.stderr, r.stderr


def test_a_box_down_pass_reads_nothing(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    (box.state / "netreach.state").write_text("alerted_strih=1\n")
    r = box.run(STRIH_SNV)
    assert box.logins() == 0, r.stderr


def test_the_linux_box_runs_the_read_at_nice_19(tmp_path):
    box = Box(tmp_path)
    box.write_log(ON_AIR)
    box.run(STRIH_SNV)
    assert "nice -n 19 tail -n 800 " in box.remote(), box.remote()


def _reader(op, arg=""):
    r = subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{READER}"\nstrih_log_remote_cmd linux "$1" "$2"',
                        "harness", op, arg], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_every_linux_reader_op_reads_the_file_at_nice_19():
    assert 'nice -n 19 tail -n 400 "$F"' in _reader("tail", "400")
    assert 'nice -n 19 tail -n +8 "$F"' in _reader("since", "7")
    assert 'nice -n 19 wc -l < "$F"' in _reader("count")
    ht = _reader("headtail", "500")
    assert 'nice -n 19 head -n 600 "$F"' in ht and 'nice -n 19 tail -n 500 "$F"' in ht
