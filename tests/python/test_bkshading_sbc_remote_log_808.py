"""Issue 808: a handheld shading board ships its RAM journal to its venue strih, and its boot clock
no longer restarts at the date of its read-only reboot.

The board (cam1/cam2/cam4-shading, an arm64 SBC on a read-only root) keeps its journal in RAM only
(`Storage=volatile`), so the cause of the 28.10. PP outage died with the power cycle. The fix reuses
the ONE remote-logging lib (scripts/lib/remote-logging.sh, issue 1311/1394):

  * remote_log_board_sink_ips BOARD = the STRIH_IP of every strih-role fact file whose
    STRIH_BKSHADING_RELAYS names `sbc-relay@register:BOARD` (a travelling board is named by both
    venues), never a literal, never dev1;
  * a generated wrapper picks, at every start, the first candidate whose journal-remote port answers
    and execs systemd-journal-upload against it (none answers = exit 1, the unit retries);
  * bkshading-provision-sbc.sh --install writes the wrapper + the upload drop-in and enables the
    uploader (never starts it), refusing an unlisted board before anything changes; --check grades it;
  * --install refreshes /etc/fake-hwclock.data in its rw window from a synchronized clock, never
    backwards.
"""
import os
import pathlib
import socket
import subprocess
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bkshading_sbc_fakes_808 import _run_provision, _sbc_paths  # noqa: E402

REPO = pathlib.Path(HERE).parents[1]
LIB = REPO / "scripts" / "lib" / "remote-logging.sh"
BOXES = REPO / "scripts" / "strih-boxes"
UPLOAD_SCRIPT_PATH = "/usr/local/sbin/bkshading-journal-upload.sh"
DROPIN_REL = os.path.join("systemd-journal-upload.service.d", "10-bkshading-board-upload.conf")


def _bash(body, *args, env=None):
    e = dict(os.environ)
    e.pop("STRIH_BOXES_DIR", None)
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + '. "$LIB"\n' + body, "harness", *args],
                          capture_output=True, text=True, timeout=60, env=dict(e, LIB=str(LIB)))


def _facts(name):
    out = {}
    for line in (BOXES / name).read_text().splitlines():
        if "=" in line and not line.startswith("#"):
            k, v = line.split("=", 1)
            out[k] = v
    return out


def _expected_sinks(board):
    """The candidates computed independently: every strih-role fact file naming the board, in the
    loader's glob order (C collation of the file names)."""
    ips = []
    for f in sorted(p.name for p in BOXES.glob("*.env")):
        facts = _facts(f)
        if facts.get("STRIH_ROLE") != "strih":
            continue
        entries = facts.get("STRIH_BKSHADING_RELAYS", "none").split()
        if any(e.split("=", 1)[1] == "sbc-relay@register:" + board for e in entries if "=" in e):
            ips.append(facts["STRIH_IP"])
    return ips


def _fact_dir(tmp, files):
    """A synthetic fact dir: copies of strih-snv.env with the given (name, ip, relays) replaced."""
    base = (BOXES / "strih-snv.env").read_text()
    for name, ip, cams, relays in files:
        repl = {"STRIH_HOSTNAME": name, "STRIH_IP": ip, "STRIH_CAMERAS": cams,
                "STRIH_BKSHADING_RELAYS": relays, "STRIH_NDI_PREFIX": name.upper()}
        out = []
        for line in base.splitlines():
            key = line.split("=", 1)[0]
            out.append(f"{key}={repl[key]}" if key in repl and "=" in line and not line.startswith("#")
                       else line)
        (tmp / f"{name}.env").write_text("\n".join(out) + "\n")
    return str(tmp)


# --- the resolver -------------------------------------------------------------------------------------

def test_every_live_board_resolves_to_the_strih_boxes_that_name_it():
    boards = set()
    for f in BOXES.glob("*.env"):
        for e in _facts(f.name).get("STRIH_BKSHADING_RELAYS", "none").split():
            if "@register:" in e:
                boards.add(e.split("@register:", 1)[1])
    assert {"cam1-shading", "cam2-shading", "cam4-shading"} <= boards
    for board in sorted(boards):
        r = _bash('remote_log_board_sink_ips "$1"', board)
        assert r.returncode == 0, r.stderr
        assert r.stdout.split() == _expected_sinks(board), board


def test_each_board_gets_the_venue_that_lists_it():
    snv = _facts("strih-snv.env")["STRIH_IP"]
    pp = _facts("strih-pp.env")["STRIH_IP"]
    assert _bash('remote_log_board_sink_ips cam2-shading').stdout.split() == [snv]
    assert _bash('remote_log_board_sink_ips cam1-shading').stdout.split() == [pp]
    # issue 1452: cam3's camera moved onto cam3's own USB (10.11.2026), so only strih-pp lists cam4-shading.
    assert _bash('remote_log_board_sink_ips cam4-shading').stdout.split() == [pp]


def test_an_unlisted_or_bad_board_name_is_refused_by_name_never_guessed():
    for board in ("cam9-shading", "cam2-shadingx", "cam2", "", "-x", "a b", "c" * 65, "cam2-shading;x"):
        r = _bash('remote_log_board_sink_ips "$1"', board)
        assert r.returncode == 1, (board, r.stdout)
        assert r.stdout == ""
        assert "remote-logging:" in r.stderr
    r = _bash('remote_log_board_sink_ips cam9-shading')
    assert "STRIH_BKSHADING_RELAYS" in r.stderr


def test_the_resolver_never_answers_the_cambox_relay_or_a_bad_address(tmp_path):
    d = _fact_dir(tmp_path, [
        ("strih-aa", "10.0.0.1", "2 3", "2=cambox-relay@cam2.lan:8771 3=sbc-relay@register:b1"),
        ("strih-bb", "10.0.0.300", "4", "4=sbc-relay@register:b2"),
    ])
    assert _bash('remote_log_board_sink_ips b1', env={"STRIH_BOXES_DIR": d}).stdout.split() == ["10.0.0.1"]
    r = _bash('remote_log_board_sink_ips cam2.lan', env={"STRIH_BOXES_DIR": d})
    assert r.returncode == 1
    r = _bash('remote_log_board_sink_ips b2', env={"STRIH_BOXES_DIR": d})
    assert r.returncode == 1 and r.stdout == "" and "10.0.0.300" in r.stderr


def test_the_resolver_leaves_the_callers_shell_as_it_was():
    r = _bash('shopt -s nocasematch; IFS=:; x="$(remote_log_board_sink_ips cam2-shading)"; '
              'shopt -q nocasematch && [ "$IFS" = : ] && declare -F strih_box_parse_file >/dev/null '
              '|| echo NOLOADER; echo "got=$x"')
    assert r.returncode == 0, r.stderr
    assert "got=" + _facts("strih-snv.env")["STRIH_IP"] in r.stdout


# --- the generated wrapper ------------------------------------------------------------------------------

def _listener():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    s.listen(8)

    def accept():
        while True:
            try:
                c, _ = s.accept()
                c.close()
            except OSError:
                return
    threading.Thread(target=accept, daemon=True).start()
    return s


def _wrapper(tmp, sinks, port):
    fake_bin = tmp / "systemd-journal-upload"
    fake_bin.write_text('#!/usr/bin/env bash\nprintf "UPLOAD %s\\n" "$*"\n')
    fake_bin.chmod(0o755)
    env = {"REMOTE_LOG_JOURNAL_PORT": str(port), "REMOTE_LOG_BOARD_UPLOAD_BIN": str(fake_bin)}
    r = _bash('remote_log_board_upload_script_content "$@"', *sinks, env=env)
    assert r.returncode == 0, r.stderr
    script = tmp / "wrapper.sh"
    script.write_text(r.stdout)
    script.chmod(0o755)
    return script, r.stdout


def test_the_wrapper_uploads_to_the_first_candidate_that_answers(tmp_path):
    s = _listener()
    try:
        port = s.getsockname()[1]
        script, text = _wrapper(tmp_path, ["127.0.0.2", "127.0.0.1"], port)
        r = subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=30)
    finally:
        s.close()
    assert r.returncode == 0, r.stderr
    assert f"--url=http://127.0.0.1:{port}" in r.stdout
    assert "--save-state=/run/systemd/journal-upload/state" in r.stdout
    assert "127.0.0.2" not in r.stdout.split("UPLOAD", 1)[1]
    assert text.startswith("#!/usr/bin/env bash\n")


def test_the_wrapper_with_no_sink_answering_exits_1_and_never_uploads(tmp_path):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()  # nothing listens there
    script, _ = _wrapper(tmp_path, ["127.0.0.1", "127.0.0.2"], port)
    r = subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 1
    assert "UPLOAD" not in r.stdout
    assert "127.0.0.1" in r.stderr and "127.0.0.2" in r.stderr


def test_the_wrapper_generator_refuses_no_or_a_bad_sink():
    for args in ([], ["10.0.0.1", "10.0.0.256"], ["strih.lan"]):
        r = _bash('remote_log_board_upload_script_content "$@"', *args)
        assert r.returncode == 1, args
        assert r.stdout == ""


def test_the_cambox_upload_dropin_is_unchanged_and_the_board_one_only_moves_execstart():
    cambox = _bash("remote_log_journal_upload_dropin_content").stdout
    assert "ExecStart=/usr/lib/systemd/systemd-journal-upload --save-state=/run/systemd/journal-upload/state\n" in cambox
    board = _bash('remote_log_journal_upload_dropin_content "$1"', UPLOAD_SCRIPT_PATH).stdout
    assert board.count("ExecStart=") == 2  # the reset + ours
    assert f"ExecStart={UPLOAD_SCRIPT_PATH}\n" in board
    strip = [ln for ln in cambox.splitlines() if not ln.startswith("ExecStart=/")]
    assert strip == [ln for ln in board.splitlines() if not ln.startswith("ExecStart=/")]


# --- --install / --check --------------------------------------------------------------------------------

def _gen(body, *args):
    r = _bash(body, *args)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_install_writes_the_upload_wrapper_and_dropin_and_only_enables_the_uploader(tmp_path):
    root = str(tmp_path)
    r, calls, _b = _run_provision("--install", root)
    assert r.returncode == 0, r.stdout + r.stderr
    p = _sbc_paths(root)
    sinks = _expected_sinks("cam2-shading")
    script = pathlib.Path(p["ju_script"])
    assert script.read_text() == _gen('remote_log_board_upload_script_content "$@"', *sinks)
    assert oct(script.stat().st_mode & 0o777) == "0o755"
    dropin = pathlib.Path(p["unit_dir"]) / DROPIN_REL
    assert dropin.read_text() == _gen('remote_log_journal_upload_dropin_content "$1"', UPLOAD_SCRIPT_PATH)
    log = pathlib.Path(calls).read_text().splitlines()
    assert "enable systemd-journal-upload.service" in log
    assert not [ln for ln in log if "journal-upload" in ln and ln.split()[0] in ("start", "restart")]
    assert not os.path.exists(p["apt_log"])  # the uploader is there: no apt
    assert f"first answering of {' '.join(sinks)} :19532" in r.stdout


def test_the_cambox_uploader_path_ignores_the_board_test_seam():
    r = _bash("remote_log_journal_upload_dropin_content",
              env={"REMOTE_LOG_BOARD_UPLOAD_BIN": "/tmp/x", "REMOTE_LOG_JU_BIN": "/tmp/y"})
    assert "ExecStart=/usr/lib/systemd/systemd-journal-upload --save-state=" in r.stdout


def test_install_installs_the_uploader_package_when_missing(tmp_path):
    root = str(tmp_path)
    r, _c, _b = _run_provision("--install", root, ju_present=False)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "systemd-journal-remote" in pathlib.Path(_sbc_paths(root)["apt_log"]).read_text()
    # with the other packages, before the relay unit or any board file is written
    out = r.stdout
    assert out.index("systemd-journal-remote") < out.index("installed ")


def test_check_names_the_age_of_the_boot_clock_file(tmp_path):
    root = str(tmp_path)
    p = _sbc_paths(root)
    _run_provision("--install", root)
    os.makedirs(os.path.dirname(p["fake_hwclock"]), exist_ok=True)
    pathlib.Path(p["fake_hwclock"]).write_text("2026-10-03 06:12:01\n")
    r, _c, _b = _run_provision("--check", root)
    assert "boot clock file" in r.stdout and "2026-10-03 06:12:01" in r.stdout
    assert "refreshed only by --install" in r.stdout


def test_install_refuses_an_unlisted_board_before_anything_changes(tmp_path):
    root = str(tmp_path)
    r, calls, _b = _run_provision("--install", root, hostname="handheld-9")
    assert r.returncode == 1
    assert "handheld-9" in r.stderr and "STRIH_BKSHADING_RELAYS" in r.stderr
    p = _sbc_paths(root)
    assert not os.path.exists(p["ju_script"])
    assert not os.path.exists(p["fstab"] + ".bak")
    assert not os.path.exists(p["mount_log"])
    assert not os.path.exists(calls) or "enable" not in pathlib.Path(calls).read_text()


def test_check_grades_the_off_box_journal(tmp_path):
    root = str(tmp_path)
    r, _c, _b = _run_provision("--install", root)
    assert r.returncode == 0, r.stderr
    r2, _c2, _b2 = _run_provision("--check", root)
    assert "OK: the board journal ships off-box" in r2.stdout, r2.stdout + r2.stderr
    p = _sbc_paths(root)
    os.remove(p["ju_script"])
    r3, _c3, _b3 = _run_provision("--check", root)
    assert r3.returncode == 1
    assert "FAIL" in r3.stderr and "bkshading-journal-upload.sh" in r3.stderr
    r4, _c4, _b4 = _run_provision("--install", root)
    assert r4.returncode == 0
    (pathlib.Path(p["unit_dir"]) / DROPIN_REL).write_text("[Service]\n")
    r5, _c5, _b5 = _run_provision("--check", root)
    assert r5.returncode == 1 and DROPIN_REL.split("/")[-1] in r5.stderr
    os.remove(p["ju_bin"])
    r6, _c6, _b6 = _run_provision("--check", root, ju_present=False)
    assert r6.returncode == 1 and "systemd-journal-upload" in r6.stderr


def test_check_fails_an_uploader_that_is_not_enabled(tmp_path):
    root = str(tmp_path)
    _run_provision("--install", root)
    r, _c, _b = _run_provision("--check", root, disabled_units=("systemd-journal-upload.service",))
    assert r.returncode == 1
    assert "systemd-journal-upload.service" in r.stderr and "not enabled" in r.stderr


# --- the boot clock file --------------------------------------------------------------------------------

def _clock_run(tmp_path, before, sync, now):
    root = str(tmp_path)
    p = _sbc_paths(root)
    if before is not None:
        os.makedirs(os.path.dirname(p["fake_hwclock"]), exist_ok=True)
        pathlib.Path(p["fake_hwclock"]).write_text(before + "\n")
    r, _c, _b = _run_provision("--install", root,
                               env_extra={"FAKE_NTP_SYNC": sync, "BKSHADING_SBC_NOW": now})
    assert r.returncode == 0, r.stdout + r.stderr
    path = pathlib.Path(p["fake_hwclock"])
    return r, (path.read_text() if path.exists() else None)


def test_install_moves_a_stale_clock_file_forward_from_a_synchronized_clock(tmp_path):
    r, text = _clock_run(tmp_path, "2026-10-03 06:12:01", "yes", "2026-11-10 12:00:00")
    assert text == "2026-11-10 12:00:00\n"
    assert "2026-10-03 06:12:01" in r.stdout and "2026-11-10 12:00:00" in r.stdout


def test_install_never_writes_an_unsynchronized_clock(tmp_path):
    r, text = _clock_run(tmp_path, "2026-10-03 06:12:01", "no", "2026-11-10 12:00:00")
    assert text == "2026-10-03 06:12:01\n"
    assert "not synchronized" in r.stdout


def test_install_never_moves_the_clock_file_backwards(tmp_path):
    _r, text = _clock_run(tmp_path, "2026-12-01 00:00:00", "yes", "2026-11-10 12:00:00")
    assert text == "2026-12-01 00:00:00\n"


def test_install_creates_no_clock_file_on_a_board_without_fake_hwclock(tmp_path):
    _r, text = _clock_run(tmp_path, None, "yes", "2026-11-10 12:00:00")
    assert text is None


def test_a_malformed_clock_value_is_never_written(tmp_path):
    _r, text = _clock_run(tmp_path, "2026-10-03 06:12:01", "yes", "2026-11-10;rm")
    assert text == "2026-10-03 06:12:01\n"


def test_the_provision_script_stays_within_its_line_budget():
    n = len((REPO / "scripts" / "bkshading-provision-sbc.sh").read_text().splitlines())
    assert n <= 1000, n
