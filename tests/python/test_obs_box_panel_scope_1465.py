"""issue 1465 item 7 -- every panel launcher starts its app in its own transient systemd scope.

Finding 6363702188: tint2 runs as the --user unit obs-box-panel.service, so an app a launcher forks
lives in the panel's cgroup and a panel restart (step 16h's try-restart, a tint2 crash under
Restart=always) kills Chrome, RustDesk, Discord, pcmanfm in the middle of an event. Design 6753269997:
the ONE launcher list wraps every binary launcher in `systemd-run --user --scope --collect`, both
desktops render from it, and verify item 44's (panel-launchers) row FAILs a launcher file that starts
its app inside the panel's own unit. The last test proves the mechanism on the REAL user manager: a
transient service runs the rendered Exec the way tint2 does (/bin/sh -c), the service restarts, the app
lives on.
"""
import os
import shutil
import subprocess
import time
import uuid
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SCOPE = "/usr/bin/systemd-run --user --scope --collect --quiet --description=obs-box-panel-app-%s -- "


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


def _bash(body, *args, env=None, box="stream-pp"):
    # the caller's own strict mode (setup-strih / verify-strih run under set -euo pipefail)
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), *args],
                          capture_output=True, text=True, env=e, timeout=120)


def _all_apps(tmp_path):
    """A home + seams where every launcher the list knows is present."""
    home = tmp_path / "home"
    ud = home / ".config" / "systemd" / "user"
    ud.mkdir(parents=True)
    for u in ("strih-obs.service", "bkshading-panel-app.service", "restreamer.service"):
        (ud / u).write_text("[Service]\n")
    bins = {}
    for name in ("pcmanfm", "google-chrome-stable", "flameshot", "rustdesk", "discord"):
        bins[name] = _exec(tmp_path / name, "#!/bin/sh\n")
    env = {"OBS_BOX_PANEL_SYS_UNIT_DIR": str(tmp_path / "no-sys"),
           "OBS_BOX_PANEL_FILES_BIN": str(bins["pcmanfm"]),
           "OBS_BOX_PANEL_FILES_HELPER": "/opt/test/obs_box_recordings.py",
           "OBS_BOX_PANEL_POWER_HELPER": "/opt/test/obs-box-power",
           "OBS_BOX_PANEL_CHROME_BIN": str(bins["google-chrome-stable"]),
           "OBS_BOX_PANEL_SHOT_BIN": str(bins["flameshot"]),
           "OBS_BOX_PANEL_RUSTDESK_BIN": str(bins["rustdesk"]),
           "OBS_BOX_PANEL_DISCORD_BIN": str(bins["discord"])}
    return home, env, bins


def _launchers(home, env, box="stream-pp"):
    r = _bash('obs_box_panel_launchers "$1"', str(home), env=env, box=box)
    assert r.returncode == 0, r.stderr
    return {ln.split("|")[0]: ln.split("|")[2] for ln in r.stdout.splitlines()}


def test_every_binary_launcher_starts_its_app_in_its_own_scope(tmp_path):
    home, env, bins = _all_apps(tmp_path)
    ex = _launchers(home, env)
    assert ex["power"] == SCOPE % "power" + "/usr/bin/python3 /opt/test/obs-box-power"
    assert ex["files"] == SCOPE % "files" + "/usr/bin/python3 /opt/test/obs_box_recordings.py open"
    assert ex["chrome"] == SCOPE % "chrome" + "%s --password-store=basic" % bins["google-chrome-stable"]
    assert ex["screenshot"] == SCOPE % "screenshot" + "%s gui" % bins["flameshot"]
    assert ex["rustdesk"] == SCOPE % "rustdesk" + str(bins["rustdesk"])
    assert ex["discord"] == SCOPE % "discord" + str(bins["discord"])


def test_a_unit_launcher_stays_a_plain_start_of_its_own_unit(tmp_path):
    # the app runs in its own unit already; a scope around `systemctl start` would add nothing
    home, env, _ = _all_apps(tmp_path)
    ex = _launchers(home, env, box="strih-pp")
    assert ex["obs"] == "systemctl --user start strih-obs.service"
    assert ex["shading"] == "systemctl --user start bkshading-panel-app.service"
    assert ex["restreamer"] == "systemctl --user start restreamer.service"


def test_every_launcher_is_either_its_unit_or_a_scope(tmp_path):
    home, env, _ = _all_apps(tmp_path)
    for box in ("stream-pp", "strih-pp", "stream-snv", "strih-snv"):
        for lid, ex in _launchers(home, env, box=box).items():
            assert ex.startswith("systemctl --user start ") or ex.startswith(SCOPE % lid), (box, lid, ex)


def test_the_scope_check_reads_an_exec_line(tmp_path):
    ok = SCOPE % "rustdesk" + "/usr/bin/rustdesk"
    for line, want in ((ok, "0"), ("systemctl --user start strih-obs.service", "0"),
                       ("/usr/bin/rustdesk", "1"), ("/usr/bin/systemd-run --user /usr/bin/rustdesk", "1"),
                       ("", "1")):
        r = _bash('obs_box_panel_exec_scoped "$1" && echo 0 || echo 1', line)
        assert r.returncode == 0 and r.stdout.strip() == want, (line, r.stdout, r.stderr)


def _write_launchers(home, env, box="stream-pp"):
    r = _bash('while IFS="|" read -r id name ex icon; do f="$(obs_box_panel_desktop_path "$1" "$id")"; '
              'mkdir -p "$(dirname "$f")"; obs_box_panel_desktop_text "$name" "$ex" "$icon" > "$f"; '
              'done < <(obs_box_panel_launchers "$1")', str(home), env=env, box=box)
    assert r.returncode == 0, r.stderr


def _launcher_row(home, env, box="stream-pp"):
    r = _bash('_obs_box_panel_launchers_row "$1"', str(home), env=env, box=box)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_launchers_row_passes_the_declared_scoped_files(tmp_path):
    home, env, _ = _all_apps(tmp_path)
    _write_launchers(home, env)
    row = _launcher_row(home, env)
    assert row.startswith("OK|(panel-launchers)") and "own scope" in row, row


def test_a_launcher_that_starts_its_app_inside_the_panel_unit_fails_by_name(tmp_path):
    # the pre-1465 file: the binary straight from tint2, in the panel's cgroup
    home, env, bins = _all_apps(tmp_path)
    _write_launchers(home, env)
    f = home / ".local/share/applications/obs-box-panel-rustdesk.desktop"
    f.write_text(f.read_text().replace(SCOPE % "rustdesk", ""))
    row = _launcher_row(home, env)
    assert row.startswith("FAIL|(panel-launchers)"), row
    assert "inside the panel's own unit" in row and "obs-box-panel-rustdesk.desktop" in row, row


def test_the_xfce_launcher_files_carry_the_scope_too(tmp_path):
    # issue 1466: xfce4-panel renders from the same list; a plugin file without the scope is not declared
    home, env, _ = _all_apps(tmp_path)
    r = _bash('while IFS="|" read -r pid kind lid; do [ "$kind" = launcher ] || continue; '
              'IFS="|" read -r _ name ex icon < <(obs_box_panel_launchers "$1" | grep "^${lid}|"); '
              'f="$(obs_box_xfce_launcher_file "$1" "$pid" "$lid")"; mkdir -p "$(dirname "$f")"; '
              'obs_box_panel_desktop_text "$name" "$ex" "$icon" > "$f"; '
              'done < <(obs_box_xfce_panel_plugins "$1")\n'
              'obs_box_xfce_panel_launchers_ok "$1" && echo LAUNCHERS-OK', str(home), env=env, box="stream-snv")
    assert r.returncode == 0 and "LAUNCHERS-OK" in r.stdout, r.stdout + r.stderr
    files = sorted((home / ".config/xfce4/panel").glob("launcher-*/obs-box-panel-chrome.desktop"))
    assert len(files) == 1 and ("Exec=" + SCOPE % "chrome") in files[0].read_text()
    files[0].write_text(files[0].read_text().replace(SCOPE % "chrome", ""))
    r = _bash('obs_box_xfce_panel_launchers_ok "$1" || echo "BAD $(obs_box_xfce_panel_launchers_ok "$1")"',
              str(home), env=env, box="stream-snv")
    assert "BAD" in r.stdout and "obs-box-panel-chrome.desktop" in r.stdout, r.stdout + r.stderr


# --- the real mechanism on the user manager ------------------------------------------------------------

def _user_env():
    e = dict(os.environ)
    e.setdefault("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())
    return e


def _systemctl(*args, check=True):
    r = subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True, env=_user_env(),
                       timeout=60)
    if check:
        assert r.returncode == 0, (args, r.stdout, r.stderr)
    return r


def _wait(pred, secs=15.0):
    end = time.monotonic() + secs
    while time.monotonic() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.2)
    return pred()


def _read(path):
    try:
        return path.read_text().strip()
    except OSError:
        return ""


def test_a_panel_restart_leaves_an_app_started_from_a_launcher_running(tmp_path):
    """The REAL user manager: a stand-in panel unit runs the rendered launcher Exec the way tint2 does
    (/bin/sh -c), the unit restarts, the app lives on in its own scope. CI starts the runner's user
    manager for this (ci.yml); a host without one FAILS here by name, never skips."""
    if not Path(_user_env()["XDG_RUNTIME_DIR"], "systemd", "private").exists():
        pytest.fail("no systemd --user manager for uid %d (%s/systemd/private missing): this test needs one"
                    % (os.getuid(), _user_env()["XDG_RUNTIME_DIR"]))
    pidf = tmp_path / "app.pid"
    app = _exec(tmp_path / "rustdesk", "#!/bin/sh\necho $$ > %s\nexec sleep 300\n" % pidf)
    home, env, _ = _all_apps(tmp_path / "h")
    env["OBS_BOX_PANEL_RUSTDESK_BIN"] = str(app)
    ex = _launchers(home, env)["rustdesk"]
    unit = "test-obs-box-panel-1465-%s" % uuid.uuid4().hex[:12]
    script = _exec(tmp_path / "panel.sh", "#!/bin/sh\n/bin/sh -c '%s' &\nexec sleep 300\n" % ex)
    r = subprocess.run(["systemd-run", "--user", "--quiet", "--collect", "--unit=" + unit, str(script)],
                       capture_output=True, text=True, env=_user_env(), timeout=60)
    assert r.returncode == 0, r.stderr
    pid = None
    try:
        pid = _wait(lambda: _read(pidf))
        assert pid, "the launcher never started the app"
        cg = _read(Path("/proc", pid, "cgroup"))
        assert "/run-" in cg and cg.endswith(".scope") and unit not in cg, cg
        before = _systemctl("show", "-p", "MainPID", "--value", unit + ".service").stdout.strip()
        _systemctl("restart", unit + ".service")
        after = _wait(lambda: (lambda v: v if v not in ("", "0", before) else "")(
            _systemctl("show", "-p", "MainPID", "--value", unit + ".service").stdout.strip()))
        assert after, "the stand-in panel never came back after the restart"
        time.sleep(0.5)
        assert Path("/proc", pid).exists(), "the panel restart killed the app it started"
        _systemctl("stop", unit + ".service")
        time.sleep(0.5)
        assert Path("/proc", pid).exists(), "stopping the panel killed the app it started"
    finally:
        _systemctl("stop", unit + ".service", check=False)
        for p in {pid, _read(pidf)} - {None, ""}:
            subprocess.run(["kill", p], capture_output=True)
