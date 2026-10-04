"""Issue 1399 -- the strih-lx session apps: their provisioning, their verify items and the panel window.

WHY (owner, 4.10.2026): "toto je produkcny pocitac vsetko ma bezat vzdy a stale" + "na strih nb ma po
starte bezat shading appka" + "nevie to byt pekna pwa appka lebo tam v prehliadacoch je ta hnusna horna
lista". The browser sources stayed empty after a boot and no window showed the shading panel.

What this pins (design comment 5977447339, Approach 1; Companion Satellite + its watch joined the same
list by design 5979157737, pinned in test_strih_companion_satellite_1399.py):
  * scripts/lib/strih-session-apps.sh -- the autostart lines, the setup-strih step 16d install (run for
    real through its path seams with fake dpkg-query / apt-get / systemctl / sudo / id on PATH: files,
    modes, the 4.10.2026 stopgap migrated, enable-only, never a start), the verify-strih rows (37
    keeper unit + last pass, 38 panel unit + "Shading" window) over a fake home + runtime dir;
  * the two committed units (Restart=always, never give up, the autostart start path, the ExecStart
    targets the install writes, the keeper's state file = the one verify reads);
  * scripts/bkshading_panel_app.py -- the real window wiring driven with stand-in Gtk / WebKit2 / GLib:
    title, size, a NORMAL window (never keep-above/below), no context menu, no new windows, no
    developer extras, a failed load / a crashed web process reloads after 3 s with no error page;
  * the wiring: the kiosk autostart starts the units, setup-strih step 16d, verify-strih items
    37-40, and the item-28 collection reads moved to scripts/lib/strih-obs-collection.sh.

Tier-0: bash + pytest only (no cargo, no root, no rig, no display). The fakes live in
strih_session_apps_fakes_1399.py (shared with the Satellite tests).
"""
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strih_session_apps_fakes_1399 as F  # noqa: E402

REPO = F.REPO
SCRIPTS = F.SCRIPTS
LIB = F.LIB
COLL_LIB = SCRIPTS / "lib" / "strih-obs-collection.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
KEEPER_UNIT = REPO / "systemd" / "strih-browser-keeper.service"
PANEL_UNIT = REPO / "systemd" / "bkshading-panel-app.service"
PANEL_APP = SCRIPTS / "bkshading_panel_app.py"
UNITS = F.UNITS
_bash = F.bash
CLASS = F.PANEL_CLASS
WIN = F.WIN
ROWS = len(UNITS) + 3


def _install(tmp_path, home, bindir, **fakes):
    b, log = F.fake_bin(tmp_path, **fakes)
    py = F.fake_python_ok(tmp_path)
    r = _bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, home),
              env={"STRIH_SESSION_APPS_BIN_DIR": str(bindir), "STRIH_SESSION_APPS_EUID": "1000",
                   "STRIH_SESSION_APPS_PYTHON": str(py)}, path_prepend=str(b))
    return r, log.read_text() if log.exists() else ""


def _never_starts(calls):
    for line in calls.splitlines():
        if line.startswith("systemctl"):
            words = line.split()
            assert "start" not in words and "restart" not in words and "--now" not in words, line


# --- constants + the autostart lines ----------------------------------------------------------------

def test_autostart_lines_start_every_unit_one_line_each():
    r = _bash("strih_session_apps_autostart_lines")
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["systemctl --user start %s || true" % u for u in UNITS]


def test_kiosk_autostart_starts_the_session_apps_after_obs_in_list_order():
    r = _bash("strih_openbox_autostart_text", sources=(BASELINE, PROVISION))
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    idx = [lines.index("systemctl --user start %s || true" % u)
           for u in ("strih-obs.service",) + UNITS]
    assert idx == sorted(idx), "strih-obs first, then the session apps in STRIH_SESSION_APP_UNITS order"
    assert subprocess.run(["bash", "-n", "-c", r.stdout]).returncode == 0


def test_strih_provision_sources_the_session_apps_lib_alone():
    # a caller that sources ONLY strih-provision.sh (the Rust anchor tests) still gets the lines
    r = _bash("declare -F strih_session_apps_autostart_lines", sources=(PROVISION,))
    assert r.returncode == 0, r.stderr


def test_imag_autostart_is_untouched():
    # imag keeps its own step-16 heredoc; the session apps are strih-only
    imag = (SCRIPTS / "setup-imag.sh").read_text()
    for name in ("strih-browser-keeper", "bkshading-panel-app", "companion-satellite.service",
                 "strih-satellite-watch"):
        assert name not in imag, name
    assert "strih_session_apps" not in (SCRIPTS / "lib" / "obs-box-kiosk.sh").read_text()


def test_script_mapping_and_items():
    r = _bash('for u in %s; do printf "%%s [%%s] %%s\\n" "$u" "$(strih_session_app_script "$u")" '
              '"$(strih_session_app_item "$u")"; done; strih_session_app_script bogus.service || echo REFUSED'
              % " ".join(UNITS))
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == [
        "strih-browser-keeper.service [strih_browser_keeper.py] browser-keeper",
        "bkshading-panel-app.service [bkshading_panel_app.py] shading-app",
        "companion-satellite.service [] companion-satellite",
        "strih-satellite-watch.timer [strih_satellite_watch.py] satellite-watch",
        "REFUSED",
    ]


# --- the two units ------------------------------------------------------------------------------------

def _unit(path):
    return {line.split("=", 1)[0]: line.split("=", 1)[1]
            for line in path.read_text().splitlines() if "=" in line and not line.startswith("#")}


def test_keeper_unit():
    u = _unit(KEEPER_UNIT)
    assert u["ExecStart"] == "/usr/bin/python3 /usr/local/bin/strih_browser_keeper.py --state-file %t/strih-browser-keeper.json"
    assert u["Restart"] == "always" and u["RestartSec"] == "5" and u["StartLimitIntervalSec"] == "0"
    assert u["WantedBy"] == "graphical-session.target"
    r = _bash('printf "%s" "$STRIH_BROWSER_KEEPER_STATE_FILE"')
    assert u["ExecStart"].endswith("%t/" + r.stdout)


def test_panel_unit():
    u = _unit(PANEL_UNIT)
    assert u["ExecStart"] == "/usr/bin/python3 /usr/local/bin/bkshading_panel_app.py"
    assert u["Restart"] == "always" and u["RestartSec"] == "3" and u["StartLimitIntervalSec"] == "0"
    assert u["Environment"] == "DISPLAY=:0"
    assert u["WantedBy"] == "graphical-session.target"  # the strih-obs pattern, never default.target


def test_units_run_the_programs_the_install_writes():
    for unit in UNITS:
        r = _bash('strih_session_app_script %s; echo; strih_session_app_run_unit %s' % (unit, unit))
        script, run_unit = r.stdout.split("\n")[:2]
        if not script:
            continue  # companion-satellite.service runs the /opt binary step 16 installs
        assert ("/usr/local/bin/%s" % script) in _unit(REPO / "systemd" / run_unit)["ExecStart"]
        assert (SCRIPTS / script).is_file()
        assert os.access(SCRIPTS / script, os.X_OK), "commit %s executable (git mode 100755)" % script


# --- setup-strih step 16d: the install ------------------------------------------------------------------

def test_install_writes_programs_and_units_and_migrates_the_stopgap(tmp_path):
    home, bindir = tmp_path / "home", tmp_path / "usrlocalbin"
    unitdir = home / ".config/systemd/user"
    (unitdir / "default.target.wants").mkdir(parents=True)
    (unitdir / "bkshading-panel-app.service").write_text("[Service]\nExecStart=%h/.local/bin/bkshading-panel-app\n")
    os.symlink(unitdir / "bkshading-panel-app.service", unitdir / "default.target.wants/bkshading-panel-app.service")
    (home / ".local/bin").mkdir(parents=True)
    (home / ".local/bin/bkshading-panel-app").write_text("#!/usr/bin/env python3\n")
    r, calls = _install(tmp_path, home, bindir, missing=("gir1.2-webkit2-4.1",))
    assert r.returncode == 0, r.stdout + r.stderr
    for unit in F.UNIT_FILES:
        assert (unitdir / unit).read_bytes() == (REPO / "systemd" / unit).read_bytes()
        assert stat.S_IMODE((unitdir / unit).stat().st_mode) == 0o644
    for prog in F.PROGRAMS:
        assert (bindir / prog).read_bytes() == (SCRIPTS / prog).read_bytes()
        assert stat.S_IMODE((bindir / prog).stat().st_mode) == 0o755
    assert sorted(p.name for p in bindir.iterdir()) == sorted(F.PROGRAMS), "the Satellite binary is step 16's"
    assert not os.path.lexists(unitdir / "default.target.wants/bkshading-panel-app.service")
    assert not (home / ".local/bin/bkshading-panel-app").exists()
    assert "removed the 4.10.2026 stopgap link" in r.stdout and "removed the stopgap copy" in r.stdout
    # only the missing package is installed, then the import preflight runs
    assert "apt-get install -y gir1.2-webkit2-4.1" in calls
    assert re.search(r"python -c import websocket, gi; .*WebKit2.*4\.1", calls)
    # every unit enabled in ONE call; the changed ones try-restarted (a running stopgap moves onto the new
    # files), never a plain start / restart / --now
    assert "systemctl --user enable %s" % " ".join(UNITS) in calls
    assert "systemctl --user try-restart %s" % " ".join(UNITS) in calls
    # a unit file was written: an explicit reload before the restart, never a cached old definition
    lines = calls.splitlines()
    assert lines.index("systemctl --user daemon-reload") < lines.index(
        "systemctl --user try-restart %s" % " ".join(UNITS))
    _never_starts(calls)
    assert "a stopped unit is NOT started" in r.stdout


def test_install_rewrites_nothing_and_restarts_nothing_on_an_unchanged_rerun(tmp_path):
    home, bindir = tmp_path / "home", tmp_path / "usrlocalbin"
    r1, _ = _install(tmp_path, home, bindir)
    assert r1.returncode == 0
    files = [bindir / p for p in F.PROGRAMS] + [home / ".config/systemd/user" / u for u in F.UNIT_FILES]
    past = time.time() - 3600
    for f in files:
        os.utime(f, (past, past))
    old = {f: f.stat().st_mtime_ns for f in files}
    shutil.rmtree(tmp_path / "bin")
    r2, calls = _install(tmp_path, home, bindir)
    assert r2.returncode == 0, r2.stderr
    assert "apt-get" not in calls and "removed" not in r2.stdout
    assert {f: f.stat().st_mtime_ns for f in files} == old, "an unchanged file keeps its mtime"
    assert "try-restart" not in calls and "daemon-reload" not in calls
    assert r2.stdout.count("unchanged") == len(UNITS)


def test_install_restarts_only_the_unit_whose_files_changed(tmp_path):
    home, bindir = tmp_path / "home", tmp_path / "usrlocalbin"
    _install(tmp_path, home, bindir)
    (bindir / "bkshading_panel_app.py").write_text("# an older panel\n")
    shutil.rmtree(tmp_path / "bin")
    r, calls = _install(tmp_path, home, bindir)
    assert r.returncode == 0, r.stderr
    assert "systemctl --user try-restart bkshading-panel-app.service\n" in calls
    assert "program written, unit unchanged" in r.stdout
    assert "daemon-reload" not in calls, "no unit file changed: nothing to reload"
    _never_starts(calls)


def test_install_as_root_enables_in_the_operator_session(tmp_path):
    b, log = F.fake_bin(tmp_path)
    py = F.fake_python_ok(tmp_path)
    r = _bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, tmp_path / "home"),
              env={"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "b"), "STRIH_SESSION_APPS_EUID": "0",
                   "STRIH_SESSION_APPS_PYTHON": str(py)}, path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert ("sudo -u newlevel XDG_RUNTIME_DIR=/run/user/1000 systemctl --user enable %s" % " ".join(UNITS)) \
        in log.read_text()


def test_install_warns_but_succeeds_without_a_user_bus_and_names_the_error(tmp_path):
    r, calls = _install(tmp_path, tmp_path / "home", tmp_path / "b", enable_ok=False)
    assert r.returncode == 0, r.stderr
    assert "WARN: systemctl --user enable failed for newlevel (Failed to connect to bus: No medium found)" in r.stdout
    # no user bus = nothing runs: no reload / try-restart attempt and no "still runs the old files" warning
    assert "try-restart" not in calls and "daemon-reload" not in calls
    assert "still runs the old files" not in r.stdout


def test_install_fails_loud_when_a_package_does_not_install(tmp_path):
    r, _ = _install(tmp_path, tmp_path / "home", tmp_path / "b", missing=("python3-gi",), apt_installs=False)
    assert r.returncode == 1
    assert "apt-get install python3-gi failed" in r.stderr
    assert not (tmp_path / "b").exists(), "nothing installed after a failed package step"


def test_install_fails_loud_on_a_failed_import_preflight(tmp_path):
    b, _log = F.fake_bin(tmp_path)
    bad = tmp_path / "py-bad"
    bad.write_text("#!/bin/bash\nexit 1\n")
    bad.chmod(0o755)
    r = _bash('strih_session_apps_install "%s" "%s" newlevel' % (REPO, tmp_path / "home"),
              env={"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "b"), "STRIH_SESSION_APPS_EUID": "1000",
                   "STRIH_SESSION_APPS_PYTHON": str(bad)}, path_prepend=str(b))
    assert r.returncode == 1 and "cannot import websocket + Gtk 3 + WebKit2 4.1" in r.stderr
    assert not (tmp_path / "b").exists()


def test_install_refuses_a_missing_source_file(tmp_path):
    fake_repo = tmp_path / "repo"
    (fake_repo / "systemd").mkdir(parents=True)
    (fake_repo / "scripts").mkdir()
    r = _bash('strih_session_apps_install "%s" "%s" newlevel' % (fake_repo, tmp_path / "home"),
              env={"STRIH_SESSION_APPS_BIN_DIR": str(tmp_path / "b")})
    assert r.returncode == 1 and "systemd/strih-browser-keeper.service not found" in r.stderr


def test_import_check_imports_what_the_programs_import():
    r = _bash("strih_session_apps_import_check")
    assert r.returncode == 0
    for mod in ("websocket", 'gi.require_version("Gtk", "3.0")', 'gi.require_version("WebKit2", "4.1")'):
        assert mod in r.stdout
    assert 'require_version("WebKit2", "4.1")' in PANEL_APP.read_text()
    assert "from websocket import create_connection" in (SCRIPTS / "strih_browser_keeper.py").read_text()


# --- verify-strih items 37-40 ------------------------------------------------------------------------------

@pytest.mark.parametrize("args,want,rc", [
    ("ok ok 1 1 active ok", "ok", 0),
    ("absent ok 1 1 active ok", "unit-absent", 1),
    ("differs ok 1 1 active ok", "unit-differs", 1),
    ("ok absent 1 1 active ok", "script-absent", 1),
    ("ok differs 1 1 active ok", "script-differs", 1),
    ("ok ok 0 1 active ok", "not-enabled", 1),
    ("ok ok 1 0 active ok", "no-autostart", 1),
    ("ok ok 1 1 inactive ok", "not-active:inactive", 1),
    ("ok ok 1 1 failed ok", "not-active:failed", 1),
    ("ok ok 1 1 '' ok", "not-active:unreadable", 1),
    ("ok ok 1 1 active stale", "process:stale", 1),
    ("ok ok 1 1 active wrong-program", "process:wrong-program", 1),
    ("ok ok 1 1 active ''", "process:unreadable", 1),
])
def test_unit_verdict_table(args, want, rc):
    r = _bash("v=$(strih_session_app_unit_verdict %s) || rc=$?; printf '%%s %%s' \"$v\" \"${rc:-0}\"" % args)
    assert r.stdout == "%s %d" % (want, rc), r.stderr


@pytest.mark.parametrize("cmdline,start,newest,want", [
    ("/usr/bin/python3 /usr/local/bin/x.py --state-file /run/user/1000/k.json ", "@200", "100", "ok"),
    ("/usr/bin/python3 /usr/local/bin/x.py ", "100", "100", "ok"),
    ("/usr/bin/python3 /usr/local/bin/x.py ", "@99", "100", "stale"),
    ("/usr/bin/python3 /home/newlevel/.local/bin/bkshading-panel-app ", "@200", "100", "wrong-program"),
    # a python entry runs `python3 <program>`: the program as argv0 is not this unit's process
    ("/usr/local/bin/x.py --enable-features=x ", "@200", "100", "wrong-program"),
    ("/opt/other/app /usr/local/bin/x.pyz ", "@200", "100", "wrong-program"),
    ("", "@200", "100", "unreadable"),
    ("/usr/bin/python3 /usr/local/bin/x.py ", "", "100", "unreadable"),
    ("/usr/bin/python3 /usr/local/bin/x.py ", "n/a", "100", "unreadable"),
    ("/usr/bin/python3 /usr/local/bin/x.py ", "@200", "", "unreadable"),
])
def test_process_state_table(cmdline, start, newest, want):
    r = _bash("v=$(strih_session_app_process_state /usr/local/bin/x.py %s %s %s) || true; printf '%%s' \"$v\""
              % (json.dumps(cmdline), json.dumps(start), json.dumps(newest)))
    assert r.stdout == want, r.stderr


@pytest.mark.parametrize("wm,present", [
    ("0x01e00003  0 %s  strih-lx Shading\n" % CLASS, True),
    ("0x01  0 obs.obs  strih-lx OBS 32.2.0 - Profile: x\n0x02a00003  0 %s  strih-lx Shading\n" % CLASS, True),
    ("0x02a00003  0 %s  N/A Shading\n" % CLASS, True),
    ("0x02a00003  0 %s  strih-lx Shading - extra\n" % CLASS, False),
    ("0x02a00003  0 firefox.Firefox  strih-lx Shading\n", False),  # another app's window titled Shading
    ("0x02a00003  0 strih-lx Shading\n", False),  # a `wmctrl -l` line (no class column)
    ("", False),
])
def test_window_present_parser(wm, present):
    r = _bash('strih_shading_window_present "$STRIH_PANEL_WM_CLASS" "$STRIH_PANEL_WINDOW_TITLE"', stdin=wm)
    assert (r.returncode == 0) == present, r.stderr


def test_window_class_is_the_class_the_panel_app_sets():
    mod = _load_panel()
    r = _bash('printf "%s" "$STRIH_PANEL_WM_CLASS"')
    assert r.stdout == "%s.%s" % (mod.PRGNAME, mod.PRGNAME[0].upper() + mod.PRGNAME[1:]) == CLASS


def test_window_present_reads_a_large_list_without_sigpipe():
    big = "".join("0x%08x  0 obs.obs  strih-lx Window %d\n" % (i, i) for i in range(20000))
    r = subprocess.run(["bash", "-c", "set -euo pipefail; . '%s'; { printf '0x1  0 %s  strih-lx Shading\\n'; "
                        "cat; } | strih_shading_window_present \"$STRIH_PANEL_WM_CLASS\" Shading; echo OK"
                        % (LIB, CLASS)], input=big, capture_output=True, text=True, timeout=60)
    assert r.returncode == 0 and r.stdout.strip() == "OK", r.stderr


def _grade(tmp_path, home, bindir, rundir, **fakes):
    return F.grade(tmp_path, home, bindir, rundir, **fakes)


def test_grade_rows_all_ok(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    rows = _grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=WIN)
    assert [r.split("|", 1)[0] for r in rows] == ["OK"] * ROWS, rows
    keeper = F.row(rows, "browser-keeper")
    assert keeper.startswith("OK|(browser-keeper) strih-browser-keeper.service: unit + ")
    assert "its process started after the files were installed" in keeper
    assert F.row(rows, "shading-app").startswith("OK|(shading-app) bkshading-panel-app.service")
    kpass = F.row(rows, "browser-keeper-pass")
    assert kpass.startswith("OK|(browser-keeper-pass) last pass") and "connected (OBS run epoch 1)" in kpass
    assert F.row(rows, "shading-app-window") == 'OK|(shading-app-window) the panel window (%s, "Shading") is open on :0' % CLASS


def test_grade_rows_catch_a_process_older_than_the_installed_files(tmp_path):
    # the first deploy: the files are new, the running process is the one started before them
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    rows = _grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=WIN, started=int(time.time()) - 3600)
    keeper = F.row(rows, "browser-keeper")
    assert keeper.startswith("FAIL|(browser-keeper) strih-browser-keeper.service: process:stale")
    assert "systemctl --user restart strih-browser-keeper.service" in keeper


def test_grade_rows_catch_the_stopgap_process_and_its_window(tmp_path):
    # the unit file is already the new one, but the stopgap program is what still runs
    home, bindir = F.installed_home(tmp_path, panel_cmd=["/usr/bin/python3", "/home/newlevel/.local/bin/bkshading-panel-app"])
    F.all_states(tmp_path / "run")
    rows = _grade(tmp_path, home, bindir, tmp_path / "run",
                  wmctrl="0x01  0 bkshading-panel-app.py.Bkshading-panel-app.py  strih-lx Shading\n")
    assert F.row(rows, "shading-app").startswith("FAIL|(shading-app) bkshading-panel-app.service: process:wrong-program")
    assert F.row(rows, "shading-app-window").startswith("FAIL|(shading-app-window) no panel window")


def test_grade_rows_as_root_read_the_operator_session(tmp_path):
    # the deploy runs verify-strih as root: every systemctl read goes through sudo -u <operator>, and the
    # sudo stub really runs it (the process facts included)
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, log = F.fake_bin(tmp_path, wmctrl=WIN)
    r = _bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home),
              env=F.grade_env(tmp_path, bindir, tmp_path / "run", euid="0"), path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    assert [row.split("|", 1)[0] for row in r.stdout.splitlines()] == ["OK"] * ROWS, r.stdout
    calls = log.read_text()
    for prop in ("MainPID", "ExecMainStartTimestamp"):
        assert ("sudo -u newlevel XDG_RUNTIME_DIR=/run/user/1000 systemctl --user show -p %s" % prop) in calls
    assert "sudo -u newlevel env DISPLAY=:0" in calls and "wmctrl -lx" in calls


def test_grade_rows_a_vanished_main_process_reads_unreadable_without_noise(tmp_path):
    # MainPID exited between `show` and the /proc read: a named FAIL, nothing on verify's stderr
    home, bindir = F.installed_home(tmp_path)
    shutil.rmtree(tmp_path / "proc" / str(F.PIDS["strih-browser-keeper.service"]))
    F.all_states(tmp_path / "run")
    if (tmp_path / "bin").exists():
        shutil.rmtree(tmp_path / "bin")
    b, _ = F.fake_bin(tmp_path, wmctrl=WIN)
    r = _bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home),
              env=F.grade_env(tmp_path, bindir, tmp_path / "run"), path_prepend=str(b))
    assert r.returncode == 0 and r.stderr == "", r.stderr
    assert F.row(r.stdout.splitlines(), "browser-keeper").startswith(
        "FAIL|(browser-keeper) strih-browser-keeper.service: process:unreadable")


def test_grade_rows_catch_the_stopgap_unit(tmp_path):
    home, bindir = F.installed_home(tmp_path, unit_text="[Service]\nExecStart=%h/.local/bin/bkshading-panel-app\n")
    F.all_states(tmp_path / "run")
    rows = _grade(tmp_path, home, bindir, tmp_path / "run", wmctrl="0x01  0 obs.obs  strih-lx OBS\n")
    panel = F.row(rows, "shading-app")
    assert panel.startswith("FAIL|(shading-app) bkshading-panel-app.service: unit-differs")
    assert "re-run setup-strih.sh step 16d" in panel
    assert F.row(rows, "shading-app-window").startswith("FAIL|(shading-app-window) no panel window")


def test_grade_rows_catch_inactive_disabled_and_no_autostart(tmp_path):
    home, bindir = F.installed_home(tmp_path, enabled=False, autostart=False)
    F.all_states(tmp_path / "run")
    rows = _grade(tmp_path, home, bindir, tmp_path / "run", active="inactive", wmctrl="")
    assert F.row(rows, "browser-keeper").startswith("FAIL|(browser-keeper) strih-browser-keeper.service: not-enabled")
    home2, bindir2 = F.installed_home(tmp_path / "b", autostart=False)
    rows2 = F.grade(tmp_path / "b", home2, bindir2, tmp_path / "run", active="inactive", wmctrl="")
    keeper2 = F.row(rows2, "browser-keeper")
    assert "no-autostart" in keeper2 and "step 15" in keeper2
    home3, bindir3 = F.installed_home(tmp_path / "c")
    rows3 = F.grade(tmp_path / "c", home3, bindir3, tmp_path / "run", active="failed", wmctrl="")
    keeper3 = F.row(rows3, "browser-keeper")
    assert "not-active:failed" in keeper3 and "journalctl --user -u strih-browser-keeper.service" in keeper3
    assert F.row(rows3, "shading-app-window").startswith("FAIL|(shading-app-window) the kiosk display :0 listed no window")


def test_grade_rows_catch_a_stale_or_disconnected_keeper(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.watch_state(tmp_path / "run")
    F.keeper_state(tmp_path / "run", age=600)
    rows = _grade(tmp_path, home, bindir, tmp_path / "run", wmctrl=WIN)
    assert F.row(rows, "browser-keeper-pass").startswith("FAIL|(browser-keeper-pass) stale")
    F.watch_state(tmp_path / "run2")
    F.keeper_state(tmp_path / "run2", connected=False)
    rows = _grade(tmp_path, home, bindir, tmp_path / "run2", wmctrl=WIN)
    kpass = F.row(rows, "browser-keeper-pass")
    assert kpass.startswith("FAIL|(browser-keeper-pass) last pass") and "NOT connected" in kpass
    rows = _grade(tmp_path, home, bindir, tmp_path / "nothing", wmctrl=WIN)
    kpass = F.row(rows, "browser-keeper-pass")
    assert kpass.startswith("FAIL|(browser-keeper-pass) state file") and "unreadable" in kpass


def test_grade_rows_name_a_missing_wmctrl(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path)
    env = F.grade_env(tmp_path, bindir, tmp_path / "run")
    env["STRIH_SESSION_APPS_WMCTRL"] = "wmctrl-not-installed-1399"
    r = _bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home), env=env, path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert "FAIL|(shading-app-window) wmctrl missing" in r.stdout, r.stdout


def test_grade_report_routes_rows_to_ok_and_bad(tmp_path):
    home, bindir = F.installed_home(tmp_path)
    F.all_states(tmp_path / "run")
    b, _ = F.fake_bin(tmp_path, wmctrl=WIN)
    r = _bash('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; '
              'strih_session_apps_grade_report "%s" "%s" newlevel; echo rc=$?' % (REPO, home),
              env=F.grade_env(tmp_path, bindir, tmp_path / "run"), path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    assert r.stdout.count("PASS (") == ROWS and r.stdout.strip().endswith("rc=0")


def test_grade_report_fails_a_grader_that_stopped_part_way():
    r = _bash('ok() { :; }; bad() { :; }; strih_session_apps_grade_rows() { printf "OK|a\\nOK|b\\nOK|c\\n"; }; '
              'strih_session_apps_grade_report x y z && echo rc=0 || echo rc=$?')
    assert r.stdout.strip() == "rc=1", r.stdout + r.stderr
    r = _bash('printf "%s" "$(strih_session_apps_row_count)"')
    assert r.stdout == str(ROWS)


# --- the panel app window ----------------------------------------------------------------------------------

def _load_panel():
    spec = importlib.util.spec_from_file_location("bkshading_panel_app_1399", PANEL_APP)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class _Rec:
    """A stand-in GObject: records every method call, keeps signal handlers."""

    def __init__(self, kind, **kw):
        self.kind, self.calls, self.handlers = kind, [], {}
        self.kw = kw

    def connect(self, signal, handler):
        self.handlers[signal] = handler

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)

        def call(*args):
            self.calls.append((name, args))
            return self.kw.get(name)
        return call


class _NetErr:
    CANCELLED = 302

    @staticmethod
    def quark():
        return 331


class _Err:
    def __init__(self, code, message="Could not connect"):
        self.code, self.message = code, message

    def matches(self, domain, code):
        return domain == 331 and code == self.code


class _LoadEvent:
    STARTED, REDIRECTED, COMMITTED, FINISHED = 0, 1, 2, 3


class _Toolkit:
    def __init__(self):
        self.windows, self.views, self.timeouts, self.quit = [], [], [], 0
        tk = self

        class Gtk:
            @staticmethod
            def Window(title=None):
                w = _Rec("window", title=title)
                w.title = title
                tk.windows.append(w)
                return w

            @staticmethod
            def main_quit():
                tk.quit += 1

        class WebKit2:
            NetworkError = _NetErr
            LoadEvent = _LoadEvent

            @staticmethod
            def WebView():
                settings = _Rec("settings")
                v = _Rec("view", get_settings=settings)
                v.settings = settings
                tk.views.append(v)
                return v

        class GLib:
            @staticmethod
            def timeout_add_seconds(secs, fn):
                tk.timeouts.append((secs, fn))
                return len(tk.timeouts)

        self.Gtk, self.WebKit2, self.GLib = Gtk, WebKit2, GLib


def _panel():
    mod = _load_panel()
    tk = _Toolkit()
    logs = []
    app = mod.PanelWindow(tk.Gtk, tk.WebKit2, tk.GLib, log=logs.append)
    return mod, tk, app, logs


def test_panel_window_is_a_normal_titled_window_with_the_view():
    mod, tk, app, _logs = _panel()
    (win,) = tk.windows
    (view,) = tk.views
    assert win.title == "Shading" == mod.WINDOW_TITLE
    assert ("set_default_size", (1280, 980)) in win.calls
    assert ("add", (view,)) in win.calls
    names = {n for n, _ in win.calls}
    for banned in ("set_keep_above", "set_keep_below", "set_type_hint", "set_wmclass", "stick",
                   "set_decorated", "fullscreen", "set_skip_taskbar_hint", "set_skip_pager_hint"):
        assert banned not in names, banned
    assert ("set_javascript_can_open_windows_automatically", (False,)) in view.settings.calls
    assert ("set_enable_developer_extras", (False,)) in view.settings.calls


def test_panel_start_loads_the_service():
    _mod, tk, app, logs = _panel()
    app.start()
    assert ("show_all", ()) in tk.windows[0].calls
    assert tk.views[0].calls[-1] == ("load_uri", ("http://127.0.0.1:8770/",))


def test_panel_no_context_menu_no_new_windows():
    _mod, tk, _app, _logs = _panel()
    h = tk.views[0].handlers
    assert h["context-menu"](tk.views[0], object(), object(), object()) is True
    assert h["create"](tk.views[0], object()) is None


def test_panel_failed_load_retries_after_3s_without_an_error_page():
    _mod, tk, app, logs = _panel()
    view = tk.views[0]
    assert view.handlers["load-failed"](view, 1, "http://127.0.0.1:8770/", _Err(1)) is True
    assert view.handlers["load-failed"](view, 1, "http://127.0.0.1:8770/", _Err(1)) is True
    assert [s for s, _ in tk.timeouts] == [3], "one pending retry at a time"
    _secs, retry = tk.timeouts[0]
    assert retry() is False  # a one-shot GLib timeout
    assert view.calls[-1] == ("load_uri", ("http://127.0.0.1:8770/",))
    assert app.retry_pending is False
    assert any("reloading http://127.0.0.1:8770/ every 3 s until it loads" in line for line in logs)
    view.handlers["load-failed"](view, 1, "http://127.0.0.1:8770/", _Err(1))
    assert len(tk.timeouts) == 2, "a later failure schedules a new retry"


def test_panel_logs_an_outage_once_and_its_end():
    """A service down all day must not write a journal line every 3 s: one line when a failure starts
    (or its reason changes), one when the panel loads again."""
    _mod, tk, app, logs = _panel()
    view = tk.views[0]
    fail = lambda msg: (view.handlers["load-changed"](view, _LoadEvent.STARTED),
                        view.handlers["load-failed"](view, 1, "http://127.0.0.1:8770/", _Err(1, msg)),
                        view.handlers["load-changed"](view, _LoadEvent.FINISHED),
                        tk.timeouts[-1][1]())
    for _ in range(50):
        fail("Connection refused")
    assert len(tk.timeouts) == 50, "every failure still reloads"
    assert sum("Connection refused" in line for line in logs) == 1
    fail("Could not resolve host")
    assert sum("Could not resolve host" in line for line in logs) == 1
    assert not any("loaded" in line for line in logs), "a FINISHED after a failed load is not a load"
    view.handlers["load-changed"](view, _LoadEvent.STARTED)
    view.handlers["load-changed"](view, _LoadEvent.FINISHED)
    assert logs[-1] == "bkshading-panel-app: loaded http://127.0.0.1:8770/ after 51 failed attempt(s)"
    n = len(logs)
    view.handlers["load-changed"](view, _LoadEvent.STARTED)
    view.handlers["load-changed"](view, _LoadEvent.FINISHED)
    assert len(logs) == n, "a normal reload logs nothing"
    fail("Connection refused")
    assert sum("Connection refused" in line for line in logs) == 2, "a new outage is logged again"


def test_panel_cancelled_load_is_not_retried():
    _mod, tk, _app, _logs = _panel()
    view = tk.views[0]
    assert view.handlers["load-failed"](view, 1, "http://x/", _Err(_NetErr.CANCELLED)) is True
    assert tk.timeouts == []


def test_panel_web_process_crash_reloads_after_3s():
    _mod, tk, _app, _logs = _panel()
    view = tk.views[0]
    view.handlers["web-process-terminated"](view, 0)
    assert [s for s, _ in tk.timeouts] == [3]
    tk.timeouts[0][1]()
    assert view.calls[-1] == ("load_uri", ("http://127.0.0.1:8770/",))


def test_panel_closing_the_window_quits_so_the_unit_restarts_it():
    _mod, tk, _app, _logs = _panel()
    tk.windows[0].handlers["destroy"](tk.windows[0])
    assert tk.quit == 1


def test_panel_url_port_is_the_service_default_bind_port():
    # ONE source of truth: the bkshading service's own default_bind (the shading-https sibling pins it too)
    cfg = (REPO / "bkshading" / "service" / "src" / "config.rs").read_text()
    port = re.search(r'fn default_bind\(\) -> String \{\s*"[0-9.]+:(\d+)"', cfg).group(1)
    assert _load_panel().PANEL_URL == "http://127.0.0.1:%s/" % port


def test_panel_source_never_uses_the_deprecated_or_stacking_calls():
    code = "\n".join(l for l in PANEL_APP.read_text().splitlines() if not l.lstrip().startswith("#"))
    body = code.split('"""', 2)[2]  # skip the module docstring, which names what it avoids
    for banned in ("set_wmclass", "keep_above", "keep_below", "set_type_hint"):
        assert banned not in body, banned
    assert "GLib.set_prgname(PRGNAME)" in body


# --- wiring: setup-strih, verify-strih, the item-28 split ---------------------------------------------------

def test_setup_strih_step_16d_installs_the_session_apps():
    s = SETUP.read_text()
    assert "TOTAL_STEPS=17" in s
    step = s[s.index('step "16d"'):s.index('step 17 ')]
    assert 'strih_session_apps_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"' in step
    assert "|| fail" in step
    assert "systemctl --user start" not in step and "restart" not in step
    assert s.index('step "16c"') < s.index('step "16d"') < s.index("step 17 ")


def test_verify_strih_grades_items_37_and_38():
    v = VERIFY.read_text()
    assert 'strih_session_apps_grade_report "${HERE}/.." "$USER_HOME" "${STRIH_LX_USER:-newlevel}"' in v
    assert '|| bad "(session-apps) the grader did not print all of its rows"' in v
    # never between item 34 and 32: test_ndi_discovery_1342 runs that slice with only its own lib sourced
    i = v.index("strih_session_apps_grade_report")
    assert not (v.index("# 34) NDI discovery") < i < v.index("# 32) the shared OBS-box"))


def test_verify_strih_stays_within_its_line_budget():
    assert len(VERIFY.read_text().splitlines()) <= 1000
    assert len(SETUP.read_text().splitlines()) <= 1000


def test_verify_item_28_reads_the_collection_through_its_lib():
    v = VERIFY.read_text()
    assert '. "${HERE}/lib/strih-obs-collection.sh"' in v
    assert 'COLL_JSON="$(strih_active_collection_json "${USER_HOME}/.config/obs-studio")"' in v
    assert 'HYG_COUNTS="$(strih_collection_hygiene_counts "$COLL_JSON")"' in v
    assert "PYHY" not in v and "count_id" not in v


def _coll_tree(tmp_path):
    base = tmp_path / "obs"
    (base / "basic/scenes").mkdir(parents=True)
    return base


def test_active_collection_json_reads_global_ini_then_falls_back_to_the_newest(tmp_path):
    base = _coll_tree(tmp_path)
    (base / "basic/scenes/Old.json").write_text("{}")
    time.sleep(0.01)
    newest = base / "basic/scenes/Newest.json"
    newest.write_text("{}")
    os.utime(newest, (time.time() + 10, time.time() + 10))
    (base / "basic/scenes/Newest.json.bak1").write_text("{}")
    run = lambda: _bash('strih_active_collection_json "%s"' % base, sources=(COLL_LIB,)).stdout
    assert run() == str(newest)
    (base / "global.ini").write_text("[Basic]\nSceneCollectionFile=Old\n")
    assert run() == str(base / "basic/scenes/Old.json")
    (base / "global.ini").write_text("[Basic]\nSceneCollectionFile=Gone\n")
    assert run() == str(newest)
    empty = tmp_path / "empty"
    (empty / "basic/scenes").mkdir(parents=True)
    assert _bash('strih_active_collection_json "%s"' % empty, sources=(COLL_LIB,)).stdout == ""


@pytest.mark.parametrize("doc,want", [
    ({"sources": [{"filters": [{"id": "shader_filter"}, {"id": "x"}]}, {"id": "shader_filter"}],
      "modules": {"scripts-tool": [{"path": "a.lua"}, {"path": "b.lua"}]}}, "2 2"),
    ({"modules": {"scripts-tool": {"scripts": [{"path": "a.lua"}]}}}, "0 1"),
    ({"modules": {"scripts-tool": {"other": 1}}}, "0 1"),
    ({"modules": {"scripts-tool": {}}}, "0 0"),
    ({}, "0 0"),
    ([{"id": "shader_filter"}], "1 0"),
])
def test_collection_hygiene_counts(tmp_path, doc, want):
    p = tmp_path / "c.json"
    p.write_text(json.dumps(doc))
    r = _bash('strih_collection_hygiene_counts "%s"' % p, sources=(COLL_LIB,))
    assert r.returncode == 0 and r.stdout.strip() == want


def test_collection_hygiene_counts_unparseable_prints_nothing(tmp_path):
    p = tmp_path / "c.json"
    p.write_text("{not json")
    r = _bash('x="$(strih_collection_hygiene_counts "%s")"; printf "[%%s]" "$x"' % p, sources=(COLL_LIB,))
    assert r.returncode == 0 and r.stdout == "[]"
