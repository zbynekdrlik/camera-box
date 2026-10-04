"""Issue 1399 -- Companion Satellite (the strih Stream Deck XL agent) as a supervised session app.

WHY (owner, 4.10.2026: "streamdeck tam nejako crashol teraz na strih"): the kiosk autostart launched
Satellite with a bare `/opt/companion-satellite/companion-satellite >/dev/null 2>&1 &` line. Nothing
restarted it and its output went to /dev/null, so a crash left the deck dead with no log until the next
login. The main's live stopgap (a user unit, comment 5979039607) is reverted by the next setup-strih.

What this pins (design comment 5979157737, Approach 1): companion-satellite.service and its watch
(strih-satellite-watch.timer) join STRIH_SESSION_APP_UNITS, so
  * the committed unit: DISPLAY=:0, Restart=always, RestartSec=3, never give up, journal output, the
    /opt binary step 16 installs (ONE path, strih_companion_satellite_bin);
  * the kiosk autostart STARTS the unit (`systemctl --user start companion-satellite.service || true`),
    the bare launch line is gone, and verify-strih's (companion) item greps the new line;
  * setup-strih step 16d installs + enables both under the 1399 fakes and takes the stopgap over under
    the SAME unit name (its default.target.wants link removed, the running stopgap try-restarted onto
    the new unit after a daemon-reload, never a start);
  * verify-strih items 39 + 40 grade the Satellite unit (unit, the /opt program, enabled, autostart,
    active, its main process = the /opt binary started after the unit was written) and the watch
    (both unit files + the program, the timer active, its last run after the files were written, and
    the watch's own last pass).

Tier-0: bash + pytest only.
"""
import json
import os
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strih_session_apps_fakes_1399 as F  # noqa: E402

REPO = F.REPO
SCRIPTS = F.SCRIPTS
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
SAT_UNIT = REPO / "systemd" / "companion-satellite.service"
SAT = "companion-satellite.service"
WATCH = "strih-satellite-watch.timer"
START = "systemctl --user start %s || true"


def _unit(path):
    return {line.split("=", 1)[0]: line.split("=", 1)[1]
            for line in path.read_text().splitlines() if "=" in line and not line.startswith("#")}


# --- the unit + the one binary path --------------------------------------------------------------------

def test_satellite_unit_is_supervised_with_journal_output():
    u = _unit(SAT_UNIT)
    assert u["Type"] == "simple"
    assert u["Environment"] == "DISPLAY=:0"
    assert u["Restart"] == "always" and u["RestartSec"] == "3" and u["StartLimitIntervalSec"] == "0"
    assert u["StandardOutput"] == "journal" and u["StandardError"] == "journal"
    assert u["SyslogIdentifier"] == "companion-satellite"
    # a hung Electron app must not hold a restart for the default 90 s
    assert int(u["TimeoutStopSec"]) <= 15
    assert u["WantedBy"] == "graphical-session.target"  # the strih-obs pattern, never default.target
    assert "/dev/null" not in SAT_UNIT.read_text()


def test_satellite_unit_runs_the_one_installed_binary():
    r = F.bash("strih_companion_satellite_bin", sources=(PROVISION,))
    assert r.returncode == 0 and r.stdout == "/opt/companion-satellite/companion-satellite"
    assert _unit(SAT_UNIT)["ExecStart"] == r.stdout
    r2 = F.bash('printf "%s" "$STRIH_COMPANION_SATELLITE_BIN"')
    assert r2.stdout == r.stdout, "ONE source: the session-apps lib constant"
    r3 = F.bash("strih_session_app_program %s" % SAT)
    assert r3.stdout == r.stdout
    r4 = F.bash("strih_session_app_program %s" % SAT, env={"STRIH_SESSION_APPS_SATELLITE_BIN": "/tmp/x"})
    assert r4.stdout == "/tmp/x"


def test_the_watch_entry_carries_its_oneshot_service():
    r = F.bash("strih_session_app_unit_files %s; echo ---; strih_session_app_run_unit %s; echo; "
               "strih_session_app_unit_files %s; echo ---; strih_session_app_run_unit %s"
               % (WATCH, WATCH, SAT, SAT))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n") == [WATCH, "strih-satellite-watch.service", "---", "strih-satellite-watch.service",
                                    SAT, "---", SAT]


def test_satellite_and_watch_are_session_apps_in_start_order():
    r = F.bash('printf "%s\\n" "${STRIH_SESSION_APP_UNITS[@]}"')
    units = r.stdout.split()
    assert units == list(F.UNITS)
    assert units.index(SAT) < units.index(WATCH), "the Satellite starts before its watch"


# --- the kiosk autostart: the bare launch line is gone -----------------------------------------------------

def test_kiosk_autostart_starts_the_unit_and_no_longer_launches_the_binary():
    r = F.bash("strih_openbox_autostart_text", sources=(BASELINE, PROVISION))
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert START % SAT in lines and START % WATCH in lines
    assert not [ln for ln in lines if "/opt/companion-satellite" in ln], "the bare launch line is gone"
    assert lines.index(START % "strih-obs.service") < lines.index(START % SAT) < lines.index(START % WATCH)


def test_the_bare_launch_helper_is_removed_everywhere():
    r = F.bash("declare -F strih_companion_satellite_openbox_line && echo PRESENT || echo GONE", sources=(PROVISION,))
    assert r.stdout.strip() == "GONE"
    for p in list(SCRIPTS.glob("*.sh")) + list((SCRIPTS / "lib").glob("*.sh")):
        text = p.read_text(errors="replace")
        assert "strih_companion_satellite_openbox_line" not in text, p
        assert "companion-satellite >/dev/null" not in text, p


def test_verify_companion_item_greps_the_unit_start_line():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert ('grep -qxF "$(strih_session_app_autostart_line "$STRIH_COMPANION_SATELLITE_UNIT")" "$CS_AUTOSTART"'
            in v)
    r = F.bash('strih_session_app_autostart_line "$STRIH_COMPANION_SATELLITE_UNIT"')
    assert r.stdout == START % SAT


# --- setup-strih step 16d: install + the stopgap takeover -------------------------------------------------

STOPGAP = """[Unit]
Description=Companion Satellite (stopgap, issue 1399)

[Service]
Environment=DISPLAY=:0
ExecStart=/opt/companion-satellite/companion-satellite
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
"""


def _install(tmp_path, home, **fakes):
    b, log = F.fake_bin(tmp_path, **fakes)
    py = F.fake_python_ok(tmp_path)
    sat = F.fake_satellite(tmp_path)
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home),
               env={"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "usrlocalbin"), "STRIH_SESSION_APPS_EUID": "1000",
                    "STRIH_SESSION_APPS_PYTHON": str(py), "STRIH_SESSION_APPS_SATELLITE_BIN": str(sat)},
               path_prepend=str(b))
    return r, log.read_text() if log.exists() else ""


def test_install_takes_the_running_stopgap_over_under_the_same_name(tmp_path):
    home = tmp_path / "home"
    unitdir = home / ".config/systemd/user"
    (unitdir / "default.target.wants").mkdir(parents=True)
    (unitdir / SAT).write_text(STOPGAP)
    os.symlink(unitdir / SAT, unitdir / "default.target.wants" / SAT)
    r, calls = _install(tmp_path, home)
    assert r.returncode == 0, r.stdout + r.stderr
    assert (unitdir / SAT).read_bytes() == SAT_UNIT.read_bytes()
    assert (unitdir / "strih-satellite-watch.service").read_bytes() == (
        REPO / "systemd" / "strih-satellite-watch.service").read_bytes()
    assert (unitdir / WATCH).read_bytes() == (REPO / "systemd" / WATCH).read_bytes()
    assert not os.path.lexists(unitdir / "default.target.wants" / SAT)
    assert "removed the 4.10.2026 stopgap link ~/.config/systemd/user/default.target.wants/%s" % SAT in r.stdout
    lines = calls.splitlines()
    restart = [ln for ln in lines if ln.startswith("systemctl --user try-restart ")]
    assert len(restart) == 1 and SAT in restart[0].split() and WATCH in restart[0].split()
    assert lines.index("systemctl --user daemon-reload") < lines.index(restart[0])
    assert any(ln.startswith("systemctl --user enable ") and SAT in ln.split() and WATCH in ln.split()
               for ln in lines)
    for ln in lines:
        if ln.startswith("systemctl"):
            assert "start" not in ln.split() and "restart" not in ln.split() and "--now" not in ln.split(), ln
    assert "installed %s" % SAT in r.stdout and "installed by step 16" in r.stdout
    assert all("strih-satellite-watch.service" not in ln.split() for ln in lines if " enable " in ln), \
        "the watch's oneshot service is started by its timer, never enabled"


def test_install_never_writes_the_satellite_binary(tmp_path):
    r, _ = _install(tmp_path, tmp_path / "home")
    assert r.returncode == 0, r.stderr
    sat = tmp_path / "opt" / "companion-satellite" / "companion-satellite"
    assert sat.read_text() == "#!/bin/sh\n"
    assert not (tmp_path / "usrlocalbin" / "companion-satellite").exists()


def test_install_warns_when_step_16_left_no_satellite_binary(tmp_path):
    b, _log = F.fake_bin(tmp_path)
    py = F.fake_python_ok(tmp_path)
    r = F.bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, tmp_path / "home"),
               env={"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "b"), "STRIH_SESSION_APPS_EUID": "1000",
                    "STRIH_SESSION_APPS_PYTHON": str(py),
                    "STRIH_SESSION_APPS_SATELLITE_BIN": str(tmp_path / "nope" / "companion-satellite")},
               path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert "WARN: %s not installed" % (tmp_path / "nope" / "companion-satellite") in r.stdout


# --- verify-strih items 39 + 40 ------------------------------------------------------------------------------

def _graded(tmp_path, **kw):
    home_kw = {k: kw.pop(k) for k in list(kw) if k in ("satellite_exe", "autostart", "enabled")}
    home, bindir = F.installed_home(tmp_path, **home_kw)
    F.all_states(tmp_path / "run")
    return home, bindir, F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN, **kw)


def test_grade_rows_satellite_and_watch_ok(tmp_path):
    _home, bindir, rows = _graded(tmp_path)
    sat = tmp_path / "opt" / "companion-satellite" / "companion-satellite"
    assert F.row(rows, "companion-satellite") == (
        "OK|(companion-satellite) %s: unit matches the checkout, %s installed, enabled, started by the kiosk "
        "autostart, active, its process runs %s and started after the unit was installed" % (SAT, sat, sat))
    assert F.row(rows, "satellite-watch") == (
        "OK|(satellite-watch) %s: units + %s/strih_satellite_watch.py match the checkout, enabled, started by the "
        "kiosk autostart, active, last ran the watch after the files were installed" % (WATCH, bindir))
    wpass = F.row(rows, "satellite-watch-pass")
    assert wpass.startswith("OK|(satellite-watch-pass) last pass") and "Satellite connected" in wpass
    assert len(rows) == len(F.UNITS) + 3


def test_grade_rows_a_missing_satellite_binary_points_at_step_16(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    (tmp_path / "opt" / "companion-satellite" / "companion-satellite").unlink()
    F.all_states(tmp_path / "run")
    rows = F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN)
    sat = F.row(rows, "companion-satellite")
    assert sat.startswith("FAIL|(companion-satellite) %s: script-absent" % SAT)
    assert "re-run setup-strih.sh step 16 (" in sat


def test_grade_rows_a_satellite_launched_some_other_way_is_the_wrong_program(tmp_path):
    _home, _b, rows = _graded(tmp_path, satellite_exe="/bin/bash")
    assert F.row(rows, "companion-satellite").startswith("FAIL|(companion-satellite) %s: process:wrong-program" % SAT)


def test_grade_rows_a_replaced_satellite_binary_is_the_wrong_program(tmp_path):
    # the binary was reinstalled under the running process: /proc/<pid>/exe reads "<path> (deleted)"
    sat = tmp_path / "opt" / "companion-satellite" / "companion-satellite"
    _home, _b, rows = _graded(tmp_path, satellite_exe="%s (deleted)" % sat)
    assert F.row(rows, "companion-satellite").startswith("FAIL|(companion-satellite) %s: process:wrong-program" % SAT)


def test_grade_rows_a_pipe_in_a_cmdline_never_shifts_the_process_facts(tmp_path):
    # the process facts carry a free-form cmdline: a "|" in it must not move the exe or the start time
    home, bindir = F.installed_home(tmp_path)
    keeper = tmp_path / "proc" / str(F.PIDS["strih-browser-keeper.service"]) / "cmdline"
    keeper.write_bytes(b"\0".join([b"/usr/bin/python3", str(bindir / "strih_browser_keeper.py").encode(),
                                    b"--state-file", b"/run/user/1000/a|b.json"]) + b"\0")
    sat = tmp_path / "proc" / str(F.PIDS[SAT]) / "cmdline"
    sat.write_bytes(b"companion-satellite --title=a|b|c\0")
    F.all_states(tmp_path / "run")
    rows = F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN)
    assert F.row(rows, "browser-keeper").startswith("OK|(browser-keeper) "), rows
    assert F.row(rows, "companion-satellite").startswith("OK|(companion-satellite) "), rows


def test_grade_rows_the_satellite_argv_is_never_read(tmp_path):
    # Chromium rewrites its process title; the fakes give it a title that names no path at all, and the row
    # is still OK because /proc/<pid>/exe is the binary
    _home, _b, rows = _graded(tmp_path)
    assert F.row(rows, "companion-satellite").startswith("OK|(companion-satellite) ")


@pytest.mark.parametrize("program,exe,start,newest,want", [
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite", "@200", "100", "ok"),
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite", "100", "100", "ok"),
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite", "@99", "100", "stale"),
    ("/opt/cs/companion-satellite", "/usr/bin/bash", "@200", "100", "wrong-program"),
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite (deleted)", "@200", "100", "wrong-program"),
    ("/opt/cs/companion-satellite", "", "@200", "100", "unreadable"),
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite", "", "100", "unreadable"),
    ("/opt/cs/companion-satellite", "/opt/cs/companion-satellite", "@200", "", "unreadable"),
])
def test_binary_state_table(program, exe, start, newest, want):
    r = F.bash("v=$(strih_session_app_binary_state %s %s %s %s) || true; printf '%%s' \"$v\""
               % tuple(json.dumps(x) for x in (program, exe, start, newest)))
    assert r.stdout == want, r.stderr


def test_grade_rows_a_satellite_older_than_its_unit_is_stale(tmp_path):
    _home, _b, rows = _graded(tmp_path, started=int(time.time()) - 3600)
    sat = F.row(rows, "companion-satellite")
    assert sat.startswith("FAIL|(companion-satellite) %s: process:stale" % SAT)
    assert "systemctl --user restart %s" % SAT in sat


def test_grade_rows_the_bare_autostart_line_is_no_autostart(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    auto = home / ".config/openbox/autostart"
    auto.write_text(auto.read_text().replace(START % SAT, "/opt/companion-satellite/companion-satellite >/dev/null 2>&1 &"))
    F.all_states(tmp_path / "run")
    rows = F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN)
    sat = F.row(rows, "companion-satellite")
    assert sat.startswith("FAIL|(companion-satellite) %s: no-autostart" % SAT) and "step 15" in sat


def test_grade_rows_a_differing_watch_service_fails_the_watch_row(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    (home / ".config/systemd/user/strih-satellite-watch.service").write_text("[Service]\nExecStart=/bin/true\n")
    F.all_states(tmp_path / "run")
    rows = F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN)
    w = F.row(rows, "satellite-watch")
    assert w.startswith("FAIL|(satellite-watch) %s: unit-differs" % WATCH) and "step 16d" in w


def test_grade_rows_a_watch_that_never_ran_or_ran_before_the_files(tmp_path):
    _home, _b, rows = _graded(tmp_path, watch_started="")
    w = F.row(rows, "satellite-watch")
    assert w.startswith("FAIL|(satellite-watch) %s: process:never" % WATCH)
    assert "systemctl --user restart %s" % WATCH in w and "journalctl --user -u strih-satellite-watch.service" in w
    _home, _b, rows = _graded(tmp_path / "b", watch_started=int(time.time()) - 3600)
    w = F.row(rows, "satellite-watch")
    assert w.startswith("FAIL|(satellite-watch) %s: process:stale" % WATCH)
    assert "systemctl --user restart %s" % WATCH in w


def test_grade_rows_a_stale_watch_pass(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.keeper_state(tmp_path / "run")
    F.watch_state(tmp_path / "run", age=600)
    rows = F.grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=F.WIN)
    wpass = F.row(rows, "satellite-watch-pass")
    assert wpass.startswith("FAIL|(satellite-watch-pass) stale")
    assert "journalctl --user -u strih-satellite-watch.service" in wpass
    rows = F.grade(tmp_path, home, bindir, tmp_path / "nothing", wmctrl=F.WIN)
    assert F.row(rows, "satellite-watch-pass").startswith("FAIL|(satellite-watch-pass) state file")


def test_grade_rows_read_the_watch_service_start_not_a_main_pid(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, log = F.fake_bin(tmp_path, wmctrl=F.WIN)
    r = F.bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home),
               env=F.grade_env(tmp_path, bindir, tmp_path / "run"), path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    calls = log.read_text()
    assert "systemctl --user show -p ExecMainStartTimestamp --value --timestamp=unix strih-satellite-watch.service" in calls
    assert "is-active %s" % WATCH in calls


# --- setup-strih: the step texts follow the new start path ---------------------------------------------------

def test_setup_strih_names_the_satellite_unit_not_a_bare_launch():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    step16 = s[s.index('step 16 "'):s.index('step "16b"')]
    assert "companion-satellite.service" in step16
    assert "launched by the openbox autostart" not in s
    step16d = s[s.index('step "16d"'):s.index("step 17 ")]
    assert "Companion Satellite" in step16d
