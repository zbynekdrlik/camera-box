"""issue 1425 (28.10.2026 PP deploy, comment 6474289993): strih-pp's OBS started 17:09:12, setup-strih
wrote ~/.camera-box/genlock-audio-buffering-ms at 17:09:23, so that OBS ran `NOT deterministic` until its
next restart. libobs reads the marker once per audio reset, at OBS start.

Pinned here (design comment 6757364266):
  * setup-strih writes the marker FIRST (before step 1), so no OBS start during or after setup reads an
    old or missing file, and then reports what a RUNNING OBS holds (a loud WARNING + "restart
    strih-obs.service" when it is not FIXED at the fact) -- setup never restarts OBS itself;
  * the shared grade names the cause: a running OBS that is not FIXED at the fact and started before
    the marker was last written FAILs "started before ... was last written";
  * the strih-snv / PP deploy read-back carries `abuf=` (the started OBS's last genlock-audio-buffering
    value) and refuses a started OBS that is not FIXED at the box fact.

Every bash call runs under the callers' `set -euo pipefail`; values are passed as ARGUMENTS.
"""
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
HERE = Path(__file__).resolve().parent
LIB = SCRIPTS / "lib" / "obs-audio-buffering.sh"
DEPLOY = SCRIPTS / "lib" / "strih-snv-deploy.sh"
SETUP = SCRIPTS / "setup-strih.sh"
MARKER_REL = ".camera-box/genlock-audio-buffering-ms"
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import test_obs_audio_buffering_1425 as ab  # noqa: E402  (log lines from the SHIPPED C format strings)
from test_obs_audio_buffering_1425 import rig  # noqa: E402,F401  (the fake-sshpass E2E fixture)


def _bash(body, *args, env=None, sources=(LIB,)):
    pre = "set -euo pipefail\n" + "".join(f'. "{s}"\n' for s in sources)
    e = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_")}
    e.update(env or {})
    return subprocess.run(["bash", "-c", pre + body, "harness", *map(str, args)], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _log_name(epoch):
    return time.strftime("%Y-%m-%d %H-%M-%S", time.localtime(epoch)) + ".txt"


def _box(tmp_path, marker, lines, start, marker_mtime):
    """A box home: the marker (content + mtime) and ONE OBS log named by its start epoch (local time,
    as OBS names it)."""
    home = tmp_path / "home"
    (home / ".camera-box").mkdir(parents=True)
    m = home / MARKER_REL
    m.write_text(marker + "\n")
    os.utime(m, (marker_mtime, marker_mtime))
    logs = home / ".config" / "obs-studio" / "logs"
    logs.mkdir(parents=True)
    log = logs / _log_name(start)
    log.write_text("\n".join(lines) + "\n")
    os.utime(log, (start + 30, start + 30))
    return home, m, log


T0 = 1_761_667_752  # 28.10.2026 ~17:09 local; any fixed epoch works, the log name is rendered from it


# --- setup-strih: the marker before step 1, the running OBS reported ---------------------------------------

def test_setup_writes_the_marker_before_step_1():
    s = SETUP.read_text()
    call = 'obs_audio_buffering_install "$USER_HOME" "$DESKTOP_USER" "$(strih_lx_audio_buffering_ms)" || fail'
    note = 'obs_audio_buffering_running_note "$USER_HOME" "$DESKTOP_USER" "$(strih_lx_audio_buffering_ms)"'
    assert s.count(call) == 1 and s.count(note) == 1
    assert s.index('[ "${EUID:-$(id -u)}" -eq 0 ]') < s.index(call) < s.index(note) < s.index('step 1 "')
    assert len(s.splitlines()) <= 1000


def _note(tmp_path, home, fact, running):
    bin_ = tmp_path / "bin"
    bin_.mkdir(exist_ok=True)
    (bin_ / "pgrep").write_text(f"#!/bin/bash\necho \"$*\" >> '{tmp_path}/pgrep.log'\nexit {0 if running else 1}\n")
    (bin_ / "pgrep").chmod(0o755)
    user = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
    return _bash('obs_audio_buffering_running_note "$1" "$2" "$3"; echo "RC=$?"', home, user, fact,
                 env={"PATH": f"{bin_}:{os.environ['PATH']}"})


def test_the_note_on_a_stopped_obs_says_the_next_start_reads_it(tmp_path):
    home, _, _ = _box(tmp_path, "0", [ab.notdet()], T0, T0 + 11)
    r = _note(tmp_path, home, "0", running=False)
    assert r.stdout.strip().endswith("RC=0"), r.stderr
    assert "not running" in r.stdout and "WARNING" not in r.stdout + r.stderr
    assert "-x obs" in (tmp_path / "pgrep.log").read_text()


def test_the_note_on_a_running_obs_held_at_the_fact_is_quiet(tmp_path):
    home, _, _ = _box(tmp_path, "0", [ab.fixed(0, 0), ab.tel(0, 0)], T0, T0 - 60)
    r = _note(tmp_path, home, "0", running=True)
    assert r.stdout.strip().endswith("RC=0"), r.stderr
    assert "FIXED 0 ms" in r.stdout and "WARNING" not in r.stdout + r.stderr


@pytest.mark.parametrize("lines,fact,says", [
    ([ab.notdet()], "0", "NOT deterministic"),          # the 28.10 strih-pp case
    ([ab.fixed(0, 0)], "85", "FIXED 0 ms"),              # the fact changed under a running OBS
    (["10:00:00.000: audio settings reset:"], "0", "no genlock-audio-buffering line"),
])
def test_the_note_on_a_running_obs_not_at_the_fact_warns_loudly(tmp_path, lines, fact, says):
    home, _, _ = _box(tmp_path, fact, lines, T0, T0 + 11)
    r = _note(tmp_path, home, fact, running=True)
    out = r.stdout + r.stderr
    assert r.stdout.strip().endswith("RC=0"), out
    assert "WARNING" in out and says in out and "restart strih-obs.service" in out, out
    assert "setup never restarts OBS" in out, out


# --- the predate reader + the grade's reason ----------------------------------------------------------------

def _later(marker, log):
    r = _bash('_obs_audio_buffering_later "$1" "$2"; echo "RC=$?"', marker, log)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip().splitlines()


def test_the_marker_written_after_the_obs_start_reads_later(tmp_path):
    _, m, log = _box(tmp_path, "0", [ab.notdet()], T0, T0 + 11)
    assert _later(m, log) == ["1", "RC=0"]
    os.utime(m, (T0 - 5, T0 - 5))
    assert _later(m, log) == ["0", "RC=0"]
    os.utime(m, (T0, T0))  # the same second as the OBS start: never claimed later
    assert _later(m, log) == ["0", "RC=0"]


def test_an_unreadable_name_or_marker_reads_unknown(tmp_path):
    _, m, log = _box(tmp_path, "0", [ab.notdet()], T0, T0 + 11)
    odd = log.parent / "crash.txt"
    odd.write_text("x\n")
    assert _later(m, odd) == ["RC=0"]
    assert _later(tmp_path / "nope", log) == ["RC=0"]
    assert _later(m, "") == ["RC=0"]


def _verdict(fact, marker, lines, later):
    r = _bash('rc=0; obs_audio_buffering_verdict "$1" "$2" "$3" "$4" || rc=$?; echo "RC=$rc"',
              fact, marker, "\n".join(lines), later)
    out = r.stdout.strip().splitlines()
    return out[0], out[-1]


@pytest.mark.parametrize("fact,lines,runs", [
    ("0", [ab.notdet()], "NOT deterministic"),
    ("85", [ab.fixed(0, 0)], "FIXED 0 ms"),
])
def test_a_running_obs_that_predates_the_marker_fails_by_that_name(fact, lines, runs):
    line, rc = _verdict(fact, fact, lines, "1")
    assert rc == "RC=1" and line.startswith("FAIL: the running OBS started before ~/" + MARKER_REL), line
    assert runs in line and f"{fact} ms" in line and "restart strih-obs.service" in line, line
    # not later (or unknown): the reasons stay what they were
    for later in ("0", ""):
        line, rc = _verdict(fact, fact, lines, later)
        assert rc == "RC=1" and "started before" not in line, line


def test_later_never_fails_a_box_held_at_its_fact():
    line, rc = _verdict("0", "0", [ab.fixed(0, 0), ab.tel(0, 0)], "1")
    assert rc == "RC=0" and line.startswith("ok FIXED 0 ms"), line


def test_item_48_names_a_running_obs_older_than_the_marker(tmp_path):
    home, _, log = _box(tmp_path, "0", [ab.notdet()], T0, T0 + 11)
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }\n'
            'obs_audio_buffering_verify_row "$1" "$2" "$3"')
    r = _bash(body, "0", home, log)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("FAIL (audio-buffering) the running OBS started before"), r.stdout


def test_the_e2e_row_names_a_running_obs_older_than_the_marker(rig):
    home = rig["boxes"] / ab.STRIH_IP
    logs = home / ".config" / "obs-studio" / "logs"
    for f in logs.iterdir():
        f.unlink()
    log = logs / _log_name(T0)
    log.write_text(ab.notdet() + "\n")
    os.utime(log, (T0 + 30, T0 + 30))
    os.utime(home / MARKER_REL, (T0 + 11, T0 + 11))
    r = ab._gate(rig)
    assert r.stdout.strip().endswith("rc=1"), r.stdout + r.stderr
    assert "strih-snv (10.77.9.202) audio buffering NOT deterministic: the running OBS started before" in r.stderr
    assert "ok: stream-snv audio buffering FIXED 0 ms" in r.stdout


# --- the deploy read-back -------------------------------------------------------------------------------------

def _deploy_verdict(*args):
    r = _bash('set +e\nstrih_lx_deploy_verdict "$@"\necho "RC=$?"', *args, sources=(DEPLOY,))
    return r.stdout


BASE = ["abc123", "abc123", "active", "abc123", "lib1", "lib1", "1", "v9.9.5", "v9.9.5", "", "", "off"]


def test_the_read_back_passes_a_started_obs_fixed_at_the_fact():
    out = _deploy_verdict(*BASE, "0", "0")
    assert out.splitlines()[-1:] == ["RC=0"] and "OK" in out, out
    assert _deploy_verdict(*BASE, "85", "085").splitlines()[-1:] == ["RC=0"]


@pytest.mark.parametrize("want,abuf,says", [
    ("0", "nd", "NOT deterministic"),
    ("0", "none", "no genlock-audio-buffering line"),
    ("0", "85", "FIXED at 85 ms"),
    ("0", "", "<unreadable>"),
    ("", "0", "no box fact"),
])
def test_the_read_back_refuses_a_started_obs_not_fixed_at_the_fact(want, abuf, says):
    out = _deploy_verdict(*BASE, want, abuf)
    assert out.splitlines()[-1:] == ["RC=1"] and says in out and "issue 1425" in out, out


def test_a_caller_without_the_fact_keeps_the_old_verdict():
    assert _deploy_verdict(*BASE).splitlines()[-1:] == ["RC=0"]


def _readback(tmp_path, lines, fresh=True):
    home = tmp_path / "home"
    logs = home / ".config/obs-studio/logs"
    logs.mkdir(parents=True)
    stage = tmp_path / "stage"
    stage.mkdir()
    (stage / "obs-start.marker").write_text("")
    t = os.stat(stage / "obs-start.marker").st_mtime
    log = logs / "2026-10-28 17-09-12.txt"
    log.write_text("\n".join(lines) + "\n")
    off = 10 if fresh else -10
    os.utime(log, (t + off, t + off))
    bin_ = tmp_path / "bin"
    bin_.mkdir()
    for tool in ("systemctl", "curl", "sha256sum"):
        (bin_ / tool).write_text("#!/bin/bash\nexit 1\n")
        (bin_ / tool).chmod(0o755)
    cmd = _bash('strih_lx_remote_readback_cmd "$1"', stage, sources=(DEPLOY,)).stdout
    r = subprocess.run(["bash", "-c", cmd], capture_output=True, text=True, timeout=30,
                       env={"HOME": str(home), "PATH": f"{bin_}:/usr/bin:/bin"})
    assert r.returncode == 0, r.stderr
    line = r.stdout.strip().splitlines()[-1]
    assert line.startswith("installed=") and " jxs=" in line, line
    return line.split(" abuf=", 1)[1].split()[0]


def test_the_read_back_runs_on_the_box_and_prints_the_started_obs_value(tmp_path):
    assert _readback(tmp_path / "a", [ab.fixed(0, 0), ab.tel(0, 0)]) == "0"
    assert _readback(tmp_path / "b", [ab.notdet()]) == "nd"
    assert _readback(tmp_path / "c", [ab.notdet(), ab.fixed(85, 4)]) == "85"  # the LAST audio reset
    assert _readback(tmp_path / "d", ["10:00:00.000: audio settings reset:"]) == "none"
    assert _readback(tmp_path / "e", [ab.fixed(0, 0)], fresh=False) == "none"  # a log older than the start


def test_the_verify_poll_passes_the_fact_and_the_field():
    s = DEPLOY.read_text()
    assert '"${STRIH_LX_PREP_ABUF:-}" "$(_strih_lx_field abuf "$line")"' in s
    assert "STRIH_AUDIO_BUFFERING_MS" in s
