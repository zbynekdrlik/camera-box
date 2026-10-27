"""issue 1425 (design 6443440513, ROZHODNUTÉ 6436368009 item 1) -- the per-box FIXED OBS audio buffering, box side.

* the box fact `STRIH_AUDIO_BUFFERING_MS` (0..960 ms, every fact file declares it, the loader refuses
  anything else by name);
* setup-strih step 5 writes it to `~/.camera-box/genlock-audio-buffering-ms` (compare-then-write, 0644,
  the OBS user's, never through a symlink) -- the file libobs reads once per audio reset;
* the ONE grade (`obs_audio_buffering_verdict`) used by verify-strih item 48 and the BLOCKING recording-e2e
  `[0/8]` row: marker == fact, the running OBS log's FIXED line == fact, no growth line and no telemetry tick
  count other than the FIXED line's after it. The log fixtures are rendered from the SHIPPED C format strings.
* the E2E row reads each Linux OBS box over plain ssh (a fake `sshpass` RUNS the remote text against a fake
  box home) and fails closed on an unreadable box.

Every bash call runs under the callers' `set -euo pipefail`; values are passed as ARGUMENTS, never spliced
into the script text.
"""
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
BOXES = SCRIPTS / "strih-boxes"
LIB = SCRIPTS / "lib" / "obs-audio-buffering.sh"
OBS_C = REPO / "vendor" / "obs-studio" / "libobs" / "obs.c"
OBS_AUDIO = REPO / "vendor" / "obs-studio" / "libobs" / "obs-audio.c"
MARKER_REL = ".camera-box/genlock-audio-buffering-ms"
STRIH_IP = "10.77.9.202"
STREAM_IP = "10.77.9.204"


def _env(**extra):
    env = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_") and k != "OBS_FLEET"}
    env.update(extra)
    return env


def _bash(body, *args, env=None, sources=(LIB,), timeout=60):
    pre = "set -euo pipefail\n" + "".join(f'. "{s}"\n' for s in sources)
    return subprocess.run(["bash", "-c", pre + body, "harness", *map(str, args)], capture_output=True, text=True,
                          env=env or _env(), cwd=str(REPO), timeout=timeout)


# --- the log lines, rendered from the shipped C format strings ------------------------------------------------

def _c_literal(path, start):
    """The C string literal(s) starting at `start` (adjacent literals joined, PRI macros as %u)."""
    s = path.read_text()
    i = s.index(start)
    out, j = "", i
    while True:
        m = re.match(r'\s*"((?:[^"\\]|\\.)*)"', s[j:])
        if m:
            out += m.group(1)
            j += m.end()
            continue
        m = re.match(r'\s*PRI[du](?:32|64)\s*', s[j:])
        if m:
            out += "u"
            j += m.end()
            continue
        break
    return out.replace("\\n", "\n")


FIXED_FMT = _c_literal(OBS_C, '"genlock-audio-buffering: FIXED')
NOTDET_FMT = _c_literal(OBS_C, '"genlock-audio-buffering: NOT deterministic')
TEL_FMT = _c_literal(OBS_AUDIO, '"audio-telemetry #800: total_buffering=')
ABOVE_FMT = _c_literal(OBS_AUDIO, '"genlock audio buffering ABOVE the floor')


def fixed(ms, ticks):
    return "10:00:00.100: " + FIXED_FMT % (ms, ticks)


def notdet(what="no"):
    return "10:00:00.100: " + NOTDET_FMT % what


def tel(ms, ticks, mx=45):
    return "10:01:00.000: " + TEL_FMT % (ms, ticks, mx, "-")


def above(add, total):
    return "10:00:05.000: " + ABOVE_FMT % (add, total, "mbc")


def test_the_fixtures_follow_the_shipped_format_strings():
    assert fixed(0, 0) == ("10:00:00.100: genlock-audio-buffering: FIXED 0 ms (0 ticks) from "
                           "~/.camera-box/genlock-audio-buffering-ms (issue 1425)")
    assert tel(0, 0).endswith("audio-telemetry #800: total_buffering=0 ms (ticks=0/45) buffering_source=-")
    assert "total audio buffering is now 64 milliseconds" in above(64, 64)
    assert "NOT deterministic on this box -- no ~/.camera-box/genlock-audio-buffering-ms" in notdet()


# --- the value grammar ------------------------------------------------------------------------------------------

@pytest.mark.parametrize("value,ok", [("0", True), ("00", True), ("85", True), ("960", True), ("961", False),
                                      ("1000", False), ("", False), ("-1", False), ("+5", False), (" 5", False),
                                      ("5 ", False), ("8a", False), ("0x5", False), ("٣", False),
                                      ("1٣", False)])
def test_the_value_is_an_integer_0_to_960(value, ok):
    # en_US.UTF-8 widens a bracket RANGE to non-ASCII digits; the lib spells the digit set out.
    r = _bash('obs_audio_buffering_ms_ok "$1"', value, env=_env(LC_ALL="en_US.UTF-8"))
    assert (r.returncode == 0) == ok, (value, r.stderr)


# --- the grade --------------------------------------------------------------------------------------------------

def verdict(fact, marker, log_lines):
    r = _bash('rc=0; obs_audio_buffering_verdict "$1" "$2" "$3" || rc=$?; echo "rc=$rc"', fact, marker,
              "\n".join(log_lines))
    assert r.returncode == 0, r.stderr
    out = r.stdout.strip().splitlines()
    return out[0], int(out[-1].split("=")[1])


def test_a_launch_held_at_its_fact_passes():
    line, rc = verdict("0", "0", ["10:00:00.000: audio settings reset:", fixed(0, 0), tel(0, 0), tel(0, 0)])
    assert rc == 0 and line.startswith("ok FIXED 0 ms (0 ticks)") and "2 telemetry read(s)" in line


def test_a_tick_rounded_fact_is_graded_on_its_ticks():
    # 85 ms = 4 ticks at 48 kHz, the telemetry prints the rounded total.
    assert verdict("85", "85", [fixed(85, 4), tel(85, 4)])[1] == 0


@pytest.mark.parametrize("fact,marker,lines,why", [
    ("0", "__ABSENT__", [fixed(0, 0)], "is missing"),
    ("0", "85", [fixed(0, 0)], "reads '85'"),
    ("0", "0", [], "no OBS log"),
    ("0", "0", ["10:00:00.000: audio settings reset:"], "no genlock-audio-buffering line"),
    ("0", "0", [notdet()], "NOT deterministic"),
    ("0", "0", [notdet("an invalid")], "an invalid"),
    ("85", "85", [fixed(0, 0)], "FIXED at 0 ms, the box fact is 85 ms"),
    ("0", "0", [fixed(0, 0), above(64, 64)], "grew its audio buffering to 64 ms"),
    ("0", "0", [fixed(0, 0), tel(0, 0), tel(64, 3)], "total_buffering=64 ms (3 ticks)"),
    ("961", "961", [fixed(0, 0)], "is not an integer 0..960"),
    ("abc", "abc", [fixed(0, 0)], "is not an integer 0..960"),
])
def test_every_way_the_buffering_is_not_the_fact_fails(fact, marker, lines, why):
    line, rc = verdict(fact, marker, lines)
    assert rc == 1 and line.startswith("FAIL: ") and why in line, line


def test_the_latest_audio_reset_is_graded():
    # A second audio reset in the same OBS run: only what follows the LAST genlock-audio-buffering line counts.
    assert verdict("0", "0", [notdet(), above(64, 64), fixed(0, 0), tel(0, 0)])[1] == 0
    line, rc = verdict("0", "0", [fixed(0, 0), tel(0, 0), notdet()])
    assert rc == 1 and "NOT deterministic" in line


# --- setup-strih step 5: the marker ------------------------------------------------------------------------------

def _user():
    return subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()


def _install(home, ms):
    return _bash('obs_audio_buffering_install "$1" "$2" "$3"', home, _user(), ms)


def test_setup_writes_the_marker_once(tmp_path):
    home = tmp_path / "home"
    r = _install(home, "0")
    assert r.returncode == 0 and "written (0 ms)" in r.stdout, r.stderr
    f = home / MARKER_REL
    assert f.read_bytes() == b"0\n"
    assert stat.S_IMODE(f.stat().st_mode) == 0o644 and f.stat().st_uid == os.getuid()
    r = _install(home, "0")
    assert r.returncode == 0 and "unchanged (0 ms)" in r.stdout
    r = _install(home, "085")
    assert r.returncode == 0 and "written (85 ms)" in r.stdout and f.read_bytes() == b"85\n"
    os.chmod(f, 0o600)
    assert "written (85 ms)" in _install(home, "85").stdout
    assert stat.S_IMODE(f.stat().st_mode) == 0o644
    assert not [p for p in f.parent.iterdir() if p.name.startswith(".genlock")], "a temp file was left behind"


def test_setup_refuses_a_bad_value_or_a_symlinked_marker(tmp_path):
    home = tmp_path / "home"
    r = _install(home, "961")
    assert r.returncode == 1 and "not an integer 0..960" in r.stderr and not (home / MARKER_REL).exists()
    (home / ".camera-box").mkdir(parents=True)
    target = tmp_path / "elsewhere"
    target.write_text("keep\n")
    (home / MARKER_REL).symlink_to(target)
    r = _install(home, "0")
    assert r.returncode == 1 and "symlink" in r.stderr and target.read_text() == "keep\n"


# --- verify-strih item 48 ----------------------------------------------------------------------------------------

def _verify_row(home, logf, fact="0"):
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }\n'
            'obs_audio_buffering_verify_row "$1" "$2" "$3"')
    r = _bash(body, fact, home, logf)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_verify_row_reads_the_marker_and_the_newest_log(tmp_path):
    home = tmp_path / "home"
    assert _install(home, "0").returncode == 0
    logf = tmp_path / "2026-10-27 10-00-00.txt"
    logf.write_text("\n".join(["10:00:00.000: audio settings reset:", fixed(0, 0), "noise " * 20, tel(0, 0)]) + "\n")
    assert _verify_row(home, logf).startswith("PASS (audio-buffering) FIXED 0 ms")
    logf.write_text(logf.read_text() + above(21, 21) + "\n")
    row = _verify_row(home, logf)
    assert row.startswith("FAIL (audio-buffering)") and "grew" in row and "(issue 1425)" in row
    assert "no OBS log" in _verify_row(home, "")
    (home / MARKER_REL).unlink()
    assert "is missing" in _verify_row(home, logf)


def test_verify_strih_carries_item_48_within_budget():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert v.count('obs_audio_buffering_verify_row "$(strih_lx_audio_buffering_ms)" "$USER_HOME" "$LOG"') == 1
    assert v.index('LOG="$(newest_log)"') < v.index("obs_audio_buffering_verify_row")
    assert len(v.splitlines()) <= 1000
    s = (SCRIPTS / "setup-strih.sh").read_text()
    call = 'obs_audio_buffering_install "$USER_HOME" "$DESKTOP_USER" "$(strih_lx_audio_buffering_ms)" || fail'
    assert s.count(call) == 1
    assert s.index('step 5 "OBS profile facts') < s.index(call) < s.index("step 6 ")
    assert len(s.splitlines()) <= 1000


# --- the box fact --------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("box", ["strih-snv", "stream-snv", "strih-pp", "stream-pp"])
def test_every_box_declares_its_fixed_buffering(box):
    lines = [ln for ln in (BOXES / f"{box}.env").read_text().splitlines() if ln.startswith("STRIH_AUDIO_BUFFERING_MS=")]
    assert lines == ["STRIH_AUDIO_BUFFERING_MS=0"]


def _boxes_copy(tmp_path, box, old, new):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        text = f.read_text()
        if f.stem == box:
            assert text.count(old) == 1
            text = text.replace(old, new)
        (d / f.name).write_text(text)
    return d


def _load(boxes, box):
    return _bash('strih_box_load "$1" >/dev/null && strih_lx_audio_buffering_ms', box,
                 env=_env(STRIH_BOXES_DIR=str(boxes)), sources=(SCRIPTS / "lib" / "strih-box-facts.sh",))


def test_the_loader_reads_the_fact():
    r = _load(BOXES, "strih-snv")
    assert r.returncode == 0 and r.stdout.strip() == "0", r.stderr


@pytest.mark.parametrize("bad", ["961", "-1", "abc", "", "0.5", "٣", "1000"])
def test_the_loader_refuses_a_bad_value_by_name(tmp_path, bad):
    d = _boxes_copy(tmp_path, "stream-snv", "STRIH_AUDIO_BUFFERING_MS=0\n", f"STRIH_AUDIO_BUFFERING_MS={bad}\n")
    r = _load(d, "stream-snv")
    assert r.returncode != 0 and "STRIH_AUDIO_BUFFERING_MS" in r.stderr, (bad, r.stderr)


def test_the_loader_refuses_a_missing_fact(tmp_path):
    d = _boxes_copy(tmp_path, "strih-pp", "STRIH_AUDIO_BUFFERING_MS=0\n", "")
    r = _load(d, "strih-pp")
    assert r.returncode != 0 and "STRIH_AUDIO_BUFFERING_MS" in r.stderr


# --- the recording-e2e [0/8] row ------------------------------------------------------------------------------------

FAKE_SSHPASS = '''#!{py}
import os, subprocess, sys
args = sys.argv[1:]
with open(os.environ["FAKE_SSH_LOG"], "a") as f:
    f.write(" ".join(args[:3]) + "\\n")
if os.environ.get("FAKE_SSH_DOWN"):
    sys.exit(255)
cmd = args[-1]
host = args[-2].split("@", 1)[1]
home = os.path.join(os.environ["FAKE_BOXES"], host)
env = dict(os.environ, HOME=home)
sys.exit(subprocess.run(["/bin/bash", "-c", cmd], env=env).returncode)
'''


@pytest.fixture
def rig(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "sshpass"
    fake.write_text(FAKE_SSHPASS.format(py=sys.executable))
    fake.chmod(0o755)
    boxes = tmp_path / "boxhomes"
    for ip in (STRIH_IP, STREAM_IP):
        home = boxes / ip
        (home / ".camera-box").mkdir(parents=True)
        (home / MARKER_REL).write_text("0\n")
        logs = home / ".config" / "obs-studio" / "logs"
        logs.mkdir(parents=True)
        old = logs / "2026-10-26 09-00-00.txt"
        old.write_text(above(64, 64) + "\n")
        os.utime(old, (1_700_000_000, 1_700_000_000))
        (logs / "2026-10-27 10-00-00.txt").write_text("\n".join([fixed(0, 0), tel(0, 0)]) + "\n")
    return {"bin": bin_dir, "boxes": boxes, "log": tmp_path / "ssh.log"}


def _gate(rig, **extra):
    env = _env(PATH=f"{rig['bin']}:{os.environ['PATH']}", FAKE_BOXES=str(rig["boxes"]), FAKE_SSH_LOG=str(rig["log"]),
               **extra)
    body = 'rc=0; obs_audio_buffering_e2e_gate_all "$1" u pw "$2" u pw || rc=$?; echo "rc=$rc"'
    return _bash(body, STRIH_IP, STREAM_IP, env=env,
                 sources=(SCRIPTS / "lib" / "obs-fleet.sh", LIB))


def test_the_e2e_row_passes_both_boxes_held_at_their_fact(rig):
    r = _gate(rig)
    assert r.stdout.strip().endswith("rc=0"), r.stdout + r.stderr
    assert "ok: strih-snv audio buffering FIXED 0 ms" in r.stdout
    assert "ok: stream-snv audio buffering FIXED 0 ms" in r.stdout
    # sshpass is the OUTERMOST command (a stubbed sshpass is never bypassed), the ssh bounded inside it.
    assert all(ln.startswith("-p pw timeout") for ln in rig["log"].read_text().splitlines())


def test_the_e2e_row_blocks_on_either_box_and_names_it(rig):
    (rig["boxes"] / STREAM_IP / MARKER_REL).unlink()
    newest = rig["boxes"] / STRIH_IP / ".config" / "obs-studio" / "logs" / "2026-10-27 10-00-00.txt"
    newest.write_text(newest.read_text() + above(21, 21) + "\n")
    r = _gate(rig)
    assert r.stdout.strip().endswith("rc=1")
    assert "ERROR: [0/8] stream-snv" in r.stderr and "is missing" in r.stderr
    assert "ERROR: [0/8] strih-snv" in r.stderr and "grew its audio buffering to 21 ms" in r.stderr


def test_the_e2e_row_fails_closed_on_an_unreadable_box(rig):
    r = _gate(rig, FAKE_SSH_DOWN="1")
    assert r.stdout.strip().endswith("rc=1")
    assert r.stderr.count("could not read its audio-buffering marker + OBS log over ssh") == 2


def test_a_box_with_no_obs_log_fails(rig):
    for p in (rig["boxes"] / STRIH_IP / ".config" / "obs-studio" / "logs").iterdir():
        p.unlink()
    r = _gate(rig)
    assert r.stdout.strip().endswith("rc=1") and "no OBS log" in r.stderr


def test_a_non_linux_box_is_named_not_graded(rig):
    env = _env(PATH=f"{rig['bin']}:{os.environ['PATH']}", FAKE_BOXES=str(rig["boxes"]),
               FAKE_SSH_LOG=str(rig["log"]))
    r = _bash('obs_audio_buffering_e2e_gate 192.0.2.9 u pw', env=env,
              sources=(SCRIPTS / "lib" / "obs-fleet.sh", LIB))
    assert r.returncode == 0 and "not a linux-genlock box" in r.stdout
    assert not rig["log"].exists()


def test_recording_e2e_runs_the_row_blocking_in_0_8():
    s = (SCRIPTS / "recording-e2e.sh").read_text()
    call = ('obs_audio_buffering_e2e_gate_all "$STRIH" "$STRIH_USER" "$STRIH_PW" "$STREAM" "$STREAM_USER" '
            '"$STREAM_PW" || exit 1')
    assert s.count(call) == 1 and s.count('. "$HERE/lib/obs-audio-buffering.sh"') == 1
    assert s.index('. "$HERE/lib/obs-audio-buffering.sh"') < s.index(call)
    # after the [0/8] operator-visibility gate, before the first rig mutation's step banner
    assert s.index('_svg_stream_znote" ] && echo') < s.index(call) < s.index('echo "[1/8]')
