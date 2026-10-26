"""Issue 1394 (26.10.2026): netconsole ships warning/info, never drawn on the cambox HDMI preview.

The camboxes run `kernel.printk = 4 4 1 7` (Ubuntu's 10-console-messages.conf), and console_loglevel
is ONE level for every console: netcon0 shipped only emerg..err, so the info-level lines issue 1311
exists for (`usb 4-1: USB disconnect`, link changes, ext4 remounts) never left the box. Kernel 7.0
has no netconsole- or console-specific level. The other console, tty0, is drawn by fbcon on the
framebuffer camera-box writes the cameraman preview to, so a bare level 7 would print every info
line over the picture.

The generated netconsole setup script therefore routes the VT console's kernel messages to the
background VT 12 (TIOCL_SETKMSGREDIRECT, read back with TIOCL_GETKMSGREDIRECT, through perl-base)
and raises console_loglevel to 7 ONLY when that redirect reads back. verify-device (ak) grades both.
"""

import os
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "remote-logging.sh"

GREEN = ("NC_SVC_ENABLED=enabled\nNC_SVC_ACTIVE=active\nNC_SVC_RESULT=success\nNC_SCRIPT_X=yes\n"
         "NC_ENABLED=1\nNC_REMOTE_IP=10.77.9.202\nNC_REMOTE_PORT=514\nJU_SVC_ENABLED=enabled\n"
         "JU_URL=http://10.77.9.202:19532\nJU_STATE_SAVE=--save-state=/run/systemd/journal-upload/state")
SINK = "10.77.9.202"


def _bash(body, *args, env=None):
    e = dict(os.environ)
    e.pop("STRIH_BOXES_DIR", None)
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$LIB"\n' + body, "harness", *args],
                          capture_output=True, text=True, timeout=60, env=dict(e, LIB=str(LIB)))


def _exec(path, body):
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


# --- the pure decision -------------------------------------------------------------------------

def test_the_level_is_raised_only_when_the_redirect_reads_back_the_background_vt():
    for readback, vt, want in (("12", "12", "raise"), ("0", "12", "keep"), ("1", "12", "keep"),
                               ("", "12", "keep"), ("open /dev/tty12: No such file", "12", "keep"),
                               ("12 ", "12", "keep"), ("012", "12", "keep"), ("12", "", "keep"),
                               ("x", "x", "keep"), ("0", "0", "keep")):
        r = _bash('remote_log_console_level_decision "$1" "$2"', readback, vt)
        assert r.returncode == 0, r.stderr
        assert r.stdout == want + "\n", (readback, vt, r.stdout)


def test_the_constants_are_vt_12_and_level_7():
    r = _bash('printf "%s %s\\n" "$REMOTE_LOG_KMSG_VT" "$REMOTE_LOG_CONSOLE_LOGLEVEL"')
    assert r.stdout == "12 7\n", r.stdout + r.stderr


# --- the generated on-box script, run with fakes -----------------------------------------------

FAKE_IP = """#!/usr/bin/env bash
case "$*" in
  *"route get"*) echo "10.77.9.202 dev eth0 src 10.77.9.65 uid 0 cache" ;;
  *"neigh show"*) echo "10.77.9.202 dev eth0 lladdr aa:bb:cc:dd:ee:ff REACHABLE" ;;
esac
"""

# perl stub: logs its args and the printk file's content AT CALL TIME (proves redirect-before-level),
# then prints the redirect read-back the test chose.
FAKE_PERL = """#!/usr/bin/env bash
{ printf 'FIRSTARG=%s LASTARG=%s\\n' "$1" "${@: -1}"; printf 'PRINTK_AT_CALL=%s\\n' "$(cat "__PRINTK__")"; } >> "__LOG__"
printf '%s\\n' "__READBACK__"
exit __RC__
"""


def _run_setup(tmp, readback, rc=0):
    tmp = Path(tmp)
    bindir = tmp / "bin"
    bindir.mkdir()
    cfg = tmp / "cfg"
    printk = tmp / "printk"
    printk.write_text("4\t4\t1\t7\n")
    log = tmp / "perl.log"
    gen = _bash("remote_log_netconsole_setup_script_content 10.77.9.202",
                env={"REMOTE_LOG_NC_CONFIGFS": str(cfg), "REMOTE_LOG_NC_MAC_RETRY_SLEEP_S": "0",
                     "REMOTE_LOG_NC_ROUTE_RETRY_SLEEP_S": "0", "REMOTE_LOG_PRINTK_PATH": str(printk)})
    assert gen.returncode == 0, gen.stderr
    script = tmp / "setup.sh"
    script.write_text(gen.stdout)
    _exec(bindir / "ip", FAKE_IP)
    _exec(bindir / "perl", FAKE_PERL.replace("__PRINTK__", str(printk)).replace("__LOG__", str(log))
          .replace("__READBACK__", readback).replace("__RC__", str(rc)))
    for name in ("ping", "modprobe", "mount", "mountpoint"):
        _exec(bindir / name, "#!/usr/bin/env bash\nexit 0\n")
    env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"])
    r = subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=60, env=env)
    return r, printk.read_text(), (log.read_text() if log.exists() else ""), gen.stdout


def test_a_redirect_that_reads_back_raises_only_the_console_level(tmp_path):
    r, printk, log, _ = _run_setup(tmp_path, "12")
    assert r.returncode == 0, r.stdout + r.stderr
    assert printk.split() == ["7", "4", "1", "7"], printk
    # the redirect ran BEFORE the level moved: the file still read 4 when perl was called
    assert "PRINTK_AT_CALL=4\t4\t1\t7" in log, log
    assert "FIRSTARG=-e LASTARG=12" in log, log
    assert "console_loglevel 7" in r.stdout, r.stdout


def test_a_redirect_that_does_not_read_back_keeps_the_level_and_warns(tmp_path):
    for readback, rc in (("0", 0), ("open /dev/tty12: Permission denied", 2)):
        d = tmp_path / f"case-{rc}"
        d.mkdir()
        r, printk, _, _ = _run_setup(d, readback, rc)
        # the arm itself stays a success: netconsole still ships err and above
        assert r.returncode == 0, r.stdout + r.stderr
        assert printk.split() == ["4", "4", "1", "7"], printk
        assert "WARNING" in r.stderr and "VT 12" in r.stderr, r.stderr


def test_the_perl_program_opens_the_vt_without_taking_it_as_controlling_tty(tmp_path):
    _, _, _, text = _run_setup(tmp_path, "12")
    assert "O_NOCTTY" in text
    assert "0x541C" in text  # TIOCLINUX
    assert "pack(\"CC\", 11," in text  # TIOCL_SETKMSGREDIRECT
    assert "pack(\"C\", 17)" in text  # TIOCL_GETKMSGREDIRECT
    assert "LC_ALL=C perl" in text
    assert "remote_log_console_level_decision ()" in text  # embedded via declare -f
    r = subprocess.run(["bash", "-n", str(tmp_path / "setup.sh")], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


# --- the (ak) gather + verdict ------------------------------------------------------------------

def test_the_gather_reads_the_level_and_the_redirect(tmp_path):
    snip = _bash("remote_log_gather_remote_snippet").stdout
    lines = [l for l in snip.splitlines() if "CONSOLE_LOGLEVEL=" in l]
    assert len(lines) == 1, snip
    printk = tmp_path / "printk"
    printk.write_text("7\t4\t1\t7\n")
    out = subprocess.run(["bash", "-c", lines[0].replace("/proc/sys/kernel/printk", str(printk))],
                         capture_output=True, text=True).stdout
    assert out == "CONSOLE_LOGLEVEL=7\n", out
    assert any(l.startswith("KMSG_REDIRECT=") or "KMSG_REDIRECT=" in l for l in snip.splitlines()), snip
    assert "O_NOCTTY" in snip and "pack(\"C\", 17)" in snip


def test_the_verdict_passes_level_7_with_the_redirect():
    v = _bash('remote_log_verdict "$1" "$2"', GREEN + "\nCONSOLE_LOGLEVEL=7\nKMSG_REDIRECT=12", SINK).stdout
    assert v == "ok\n", v


def test_the_verdict_fails_the_ubuntu_level_4():
    v = _bash('remote_log_verdict "$1" "$2"', GREEN + "\nCONSOLE_LOGLEVEL=4\nKMSG_REDIRECT=0", SINK).stdout
    assert "FAIL" in v and "console_loglevel=4" in v and "warning/info" in v, v
    assert "cambox-ro-units-apply.sh" in v, v


def test_the_verdict_fails_level_7_drawn_on_the_preview():
    v = _bash('remote_log_verdict "$1" "$2"', GREEN + "\nCONSOLE_LOGLEVEL=7\nKMSG_REDIRECT=0", SINK).stdout
    assert "FAIL" in v and "HDMI" in v and "KMSG_REDIRECT" not in v and "redirect=0" in v, v


def test_the_verdict_fails_closed_on_an_unread_level_or_redirect():
    for extra in ("", "\nCONSOLE_LOGLEVEL=\nKMSG_REDIRECT=12", "\nCONSOLE_LOGLEVEL=7",
                  "\nCONSOLE_LOGLEVEL=x\nKMSG_REDIRECT=12", "\nCONSOLE_LOGLEVEL=7\nKMSG_REDIRECT=?"):
        v = _bash('remote_log_verdict "$1" "$2"', GREEN + extra, SINK).stdout
        assert v != "ok\n" and "FAIL" in v, (extra, v)


def test_the_verdict_accepts_a_higher_level_with_the_redirect():
    v = _bash('remote_log_verdict "$1" "$2"', GREEN + "\nCONSOLE_LOGLEVEL=8\nKMSG_REDIRECT=12", SINK).stdout
    assert v == "ok\n", v
