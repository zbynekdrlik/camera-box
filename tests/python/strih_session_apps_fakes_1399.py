"""Issue 1399 -- the shared fakes of the strih-lx session-apps tests (scripts/lib/strih-session-apps.sh).

The install (setup-strih step 16d) and the grader (verify-strih items 37-40) run for real under the
callers' `set -euo pipefail`, through their path seams, with fake `dpkg-query` / `apt-get` /
`systemctl` / `sudo` / `id` / `chown` / `wmctrl` on PATH, a fake /proc and a fake home + runtime dir.
Used by test_strih_session_apps_1399.py (the keeper + the panel) and test_strih_companion_satellite_1399.py
(Companion Satellite + its watch).
"""
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-session-apps.sh"
UNITS = ("strih-browser-keeper.service", "bkshading-panel-app.service", "companion-satellite.service",
         "strih-satellite-watch.timer")
# every unit FILE the install writes (the watch timer's oneshot service rides with the timer)
UNIT_FILES = ("strih-browser-keeper.service", "bkshading-panel-app.service", "companion-satellite.service",
              "strih-satellite-watch.timer", "strih-satellite-watch.service")
PROGRAMS = ("strih_browser_keeper.py", "bkshading_panel_app.py", "strih_satellite_watch.py")
LEAKY = ("STRIH_SESSION_APPS_BIN_DIR", "STRIH_SESSION_APPS_RUNTIME_DIR", "STRIH_SESSION_APPS_EUID",
         "STRIH_SESSION_APPS_PYTHON", "STRIH_SESSION_APPS_SATELLITE_BIN", "STRIH_LX_IP", "STRIH_BOXES_DIR")
# the main processes /proc shows (the watch's oneshot service has none between its runs)
PIDS = {"strih-browser-keeper.service": 4242, "bkshading-panel-app.service": 4343,
        "companion-satellite.service": 4444, "strih-satellite-watch.service": 0}
PANEL_CLASS = "bkshading-panel-app.Bkshading-panel-app"
WIN = "0x01  0 %s  strih-lx Shading\n" % PANEL_CLASS


def bash(body, env=None, sources=(LIB,), path_prepend=None, stdin=""):
    """Source `sources` under the CALLERS' set -euo pipefail (setup-strih / verify-strih), run `body`."""
    harness = "set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    full_env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prepend:
        full_env["PATH"] = "%s:%s" % (path_prepend, full_env["PATH"])
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=full_env,
                          cwd=str(REPO), timeout=60, input=stdin)


def stub(bindir, name, body):
    p = bindir / name
    p.write_text("#!/bin/bash\n" + body)
    p.chmod(0o755)


def fake_bin(tmp_path, *, missing=(), apt_installs=True, enable_ok=True, active="active", wmctrl=None,
             started=None, watch_started=None):
    """Fakes for every external the lib calls. Each logs its argv to <bin>/calls.log. `started` = the
    units' ExecMainStartTimestamp (unix s, default one minute from now: a process newer than the files);
    `watch_started` = the watch oneshot's last run start (default `started`; "" = it never ran)."""
    b = tmp_path / "bin"
    b.mkdir()
    log = b / "calls.log"
    state = b / "installed"
    state.write_text("".join("%s\n" % m for m in missing))
    started = int(time.time()) + 60 if started is None else started
    watch = "@%d" % started if watch_started is None else ("@%d" % watch_started if watch_started != "" else "")
    pid_cases = "".join("%s) echo %d ;; " % (u, p) for u, p in PIDS.items())
    stub(b, "dpkg-query", 'echo "dpkg-query $*" >> "%s"\nlast="${@: -1}"\n'
         'if grep -qxF "$last" "%s"; then exit 1; fi\nprintf "install ok installed"\n' % (log, state))
    stub(b, "apt-get", 'echo "apt-get $*" >> "%s"\n%s\n' % (
        log, ': > "%s"' % state if apt_installs else "exit 100"))
    stub(b, "systemctl", 'echo "systemctl $*" >> "%s"\n'
         'if [ "$2" = enable ]; then %s; fi\n'
         'if [ "$2" = is-active ]; then echo %s; [ %s = active ]; exit; fi\n'
         'if [ "$2" = show ] && [ "$4" = MainPID ]; then\n'
         '  case "${@: -1}" in %s esac\n'
         '  exit 0\nfi\n'
         'if [ "$2" = show ] && [ "$4" = ExecMainStartTimestamp ]; then\n'
         '  case "${@: -1}" in strih-satellite-watch.service) echo "%s" ;; *) echo @%d ;; esac\n'
         '  exit 0\nfi\n'
         'exit 0\n' % (log, "exit 0" if enable_ok else "{ echo 'Failed to connect to bus: No medium found' >&2; exit 1; }",
                       active, active, pid_cases, watch, started))
    # sudo -u USER [VAR=val ...] CMD ...: log it, then run CMD with the VAR=vals (the root path runs for real)
    stub(b, "sudo", 'echo "sudo $*" >> "%s"\n[ "$1" = -u ] && shift 2\nexec env "$@"\n' % log)
    stub(b, "id", 'if [ "$1" = -u ] && [ -n "${2:-}" ]; then echo 1000; else echo 1000; fi\n')
    stub(b, "chown", 'echo "chown $*" >> "%s"\nexit 0\n' % log)
    if wmctrl is not None:
        (b / "wmctrl.out").write_text(wmctrl)
        stub(b, "wmctrl", 'cat "%s"\n' % (b / "wmctrl.out"))
    return b, log


def fake_python_ok(tmp_path):
    p = tmp_path / "py-ok"
    p.write_text("#!/bin/bash\necho \"python $*\" >> \"%s\"\nexit 0\n" % (tmp_path / "bin" / "calls.log"))
    p.chmod(0o755)
    return p


def fake_satellite(tmp_path):
    """The /opt Companion Satellite binary step 16 installs (STRIH_SESSION_APPS_SATELLITE_BIN)."""
    p = tmp_path / "opt" / "companion-satellite" / "companion-satellite"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("#!/bin/sh\n")
    p.chmod(0o755)
    past = time.time() - 86400  # install.sh cp -a keeps the tarball's mtime
    os.utime(p, (past, past))
    return p


def installed_home(tmp_path, *, unit_text=None, enabled=True, autostart=True, panel_cmd=None, satellite_cmd=None):
    """A home + bin dir as step 16d leaves them (every unit + program from the checkout, enabled, started by
    the autostart) and a fake /proc with each unit's main process. `unit_text` replaces the panel unit."""
    home = tmp_path / "home"
    unitdir = home / ".config/systemd/user"
    (unitdir / "graphical-session.target.wants").mkdir(parents=True)
    bindir = tmp_path / "usrlocalbin"
    bindir.mkdir()
    for unit in UNIT_FILES:
        (unitdir / unit).write_bytes(unit_text.encode() if unit_text and unit.startswith("bkshading")
                                     else (REPO / "systemd" / unit).read_bytes())
    if enabled:
        for unit in UNITS:
            os.symlink(unitdir / unit, unitdir / "graphical-session.target.wants" / unit)
    for prog in PROGRAMS:
        shutil.copy(SCRIPTS / prog, bindir / prog)
    sat = fake_satellite(tmp_path)
    (home / ".config/openbox").mkdir(parents=True)
    lines = ["systemctl --user start strih-obs.service || true"]
    if autostart:
        lines += ["systemctl --user start %s || true" % u for u in UNITS]
    (home / ".config/openbox/autostart").write_text("\n".join(lines) + "\n")
    proc = tmp_path / "proc"
    argv = {
        "strih-browser-keeper.service": ["/usr/bin/python3", str(bindir / "strih_browser_keeper.py"),
                                         "--state-file", "/run/user/1000/strih-browser-keeper.json"],
        "bkshading-panel-app.service": panel_cmd or ["/usr/bin/python3", str(bindir / "bkshading_panel_app.py")],
        # an Electron binary: the program is argv0
        "companion-satellite.service": satellite_cmd or [str(sat)],
    }
    for unit, words in argv.items():
        (proc / str(PIDS[unit])).mkdir(parents=True)
        (proc / str(PIDS[unit]) / "cmdline").write_bytes(b"\0".join(w.encode() for w in words) + b"\0")
    return home, bindir


def keeper_state(rundir, age=3.0, connected=True):
    rundir.mkdir(parents=True, exist_ok=True)
    (rundir / "strih-browser-keeper.json").write_text(json.dumps({
        "version": 2, "updated_epoch_s": time.time() - age, "connected": connected, "obs_epoch": 1,
        "refreshes": 4, "sources": [{"name": "Odpocet", "reachable": True}], "last_error": None}))


def watch_state(rundir, age=3.0):
    rundir.mkdir(parents=True, exist_ok=True)
    (rundir / "strih-satellite-watch.json").write_text(json.dumps({
        "version": 1, "updated_epoch_s": time.time() - age, "boot_s": 1000.0, "since": {},
        "condition": "ok", "restarts": 0, "last_restart": None, "unit": "companion-satellite.service",
        "sustain_s": 60.0, "last_error": None,
        "observation": {"rest_ok": True, "rest_error": None, "connected": True,
                        "surfaces": ["Elgato Stream Deck XL"], "companion": "10.77.9.205:16622",
                        "companion_up": True, "companion_error": None, "deck_on_usb": True}}))


def all_states(rundir):
    keeper_state(rundir)
    watch_state(rundir)


def grade_env(tmp_path, bindir, rundir, euid="1000"):
    return {"STRIH_SESSION_APPS_BIN_DIR": str(bindir), "STRIH_SESSION_APPS_EUID": euid,
            "STRIH_SESSION_APPS_RUNTIME_DIR": str(rundir), "STRIH_SESSION_APPS_PROC": str(tmp_path / "proc"),
            "STRIH_SESSION_APPS_SATELLITE_BIN": str(tmp_path / "opt" / "companion-satellite" / "companion-satellite")}


def grade(tmp_path, home, bindir, rundir, **fakes):
    if (tmp_path / "bin").exists():
        shutil.rmtree(tmp_path / "bin")
    b, _log = fake_bin(tmp_path, **fakes)
    r = bash('strih_session_apps_grade_rows "%s" "%s" newlevel' % (REPO, home),
             env=grade_env(tmp_path, bindir, rundir), path_prepend=str(b))
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def row(rows, item):
    """The one grader row of `item` (e.g. `browser-keeper`, `shading-app-window`)."""
    hits = [r for r in rows if r.split("|", 1)[1].startswith("(%s) " % item)]
    assert len(hits) == 1, (item, rows)
    return hits[0]
