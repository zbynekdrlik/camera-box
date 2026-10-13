"""issue 1420 -- the kiosk panel (tint2) on the Linux OBS boxes.

Owner 13.10.2026: "a nevieme mat pod linuxom nejaku listu s tray ikonkami/appkami atd...?", then
"Chcem to vidiet aj na strih a rozhodnut sa podla toho ako sa to bude spravat a vizerat" -- every role
gets it as a trial. These tests pin the declared config, the install (write-if-changed, enable, never a
start), the autostart line, the four verify rows and the setup / verify wiring.
"""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
PANEL = SCRIPTS / "lib" / "obs-box-panel.sh"


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


def _bash(body, *args, env=None, box="stream-pp"):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), *args],
                          capture_output=True, text=True, env=e, timeout=120)


def test_the_config_is_one_bottom_panel_with_taskbar_tray_and_clock_and_no_launcher():
    r = _bash("obs_box_panel_tint2rc")
    cfg = dict(ln.split(" = ", 1) for ln in r.stdout.splitlines() if " = " in ln and not ln.startswith("#"))
    assert cfg["panel_items"] == "TSC"
    assert cfg["panel_position"].startswith("bottom")
    assert cfg["panel_size"] == "100% 30"
    assert cfg["strut_policy"] == "follow_size"
    assert cfg["mouse_right"] == "none" and cfg["mouse_middle"] == "none"   # nothing closes a window
    assert "launcher_item_app" not in r.stdout


@pytest.mark.parametrize("box", ["stream-pp", "strih-pp", "strih-snv"])
def test_every_role_gets_the_panel_as_a_trial(box):
    r = _bash('obs_box_step_applies 16h-panel && echo RUN', box=box)
    assert r.stdout.strip() == "RUN", r.stderr


def _autostart(box):
    """The box's autostart through the shared renderer the issue-1361 golden uses (every lib sourced)."""
    r = subprocess.run(["bash", str(REPO / "tests/fixtures/strih_box_1361/render.sh"), str(REPO), box],
                       capture_output=True, text=True, cwd=str(REPO), timeout=120)
    assert r.returncode == 0, r.stderr
    return r.stdout.split("=== openbox-autostart ===\n", 1)[1].split("\n=== ", 1)[0].splitlines()


@pytest.mark.parametrize("box", ["stream-pp", "strih-pp"])
def test_the_autostart_starts_the_panel_after_the_display_import(box):
    lines = _autostart(box)
    start = "systemctl --user start obs-box-panel.service || true"
    imp = "systemctl --user import-environment DISPLAY XAUTHORITY || true"
    assert start in lines and lines.index(imp) < lines.index(start)


def test_the_unit_runs_tint2_with_the_declared_config_and_restarts():
    u = (REPO / "systemd" / "obs-box-panel.service").read_text()
    assert "ExecStart=/usr/bin/tint2 -c %h/.config/tint2/tint2rc" in u
    assert "Restart=always" in u and "Environment=DISPLAY=:0" in u


class Box:
    def __init__(self, tmp):
        self.tmp = tmp
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.log = tmp / "calls"
        self.home = tmp / "home"
        self.home.mkdir()
        self.state = tmp / "state"
        self.state.mkdir()
        _exec(self.bin / "apt-get", '#!/bin/bash\necho "apt-get $*" >> %s\ntouch %s/installed\n' % (self.log, self.state))
        _exec(self.bin / "dpkg-query", '#!/bin/bash\n[ -e %s/installed ] && printf "install ok installed"\nexit 0\n'
              % self.state)
        _exec(self.bin / "systemctl", '#!/bin/bash\necho "systemctl $*" >> %s\n'
              'case "$*" in *is-enabled*) cat %s/en 2>/dev/null;; *is-active*) cat %s/act 2>/dev/null;;'
              ' *MainPID*) cat %s/pid 2>/dev/null;; esac\nexit 0\n' % (self.log, self.state, self.state, self.state))
        _exec(self.bin / "wmctrl", '#!/bin/bash\ncat %s/wm 2>/dev/null\nexit 0\n' % self.state)
        _exec(self.bin / "chown", '#!/bin/bash\nexit 0\n')
        self.proc = tmp / "proc"
        (self.proc / "4242").mkdir(parents=True)
        os.symlink("/usr/bin/tint2", self.proc / "4242" / "exe")

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "STRIH_SESSION_APPS_EUID": "1000",
                "OBS_BOX_PANEL_PROC": str(self.proc)}

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""


@pytest.fixture
def box(tmp_path):
    return Box(tmp_path)


def test_install_writes_config_and_unit_enables_and_never_starts(box):
    r = _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=box.env())
    assert r.returncode == 0, r.stderr
    cfg = box.home / ".config" / "tint2" / "tint2rc"
    assert cfg.read_text() == _bash("obs_box_panel_tint2rc").stdout
    unit = box.home / ".config" / "systemd" / "user" / "obs-box-panel.service"
    assert unit.read_bytes() == (REPO / "systemd" / "obs-box-panel.service").read_bytes()
    calls = box.calls()
    assert "apt-get install -y tint2" in calls
    assert "systemctl --user enable obs-box-panel.service" in calls
    assert " start " not in calls.replace("try-restart", "")


def test_a_second_install_changes_nothing(box):
    a = ('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home))
    _bash(*a, env=box.env())
    box.log.unlink()
    r = _bash(*a, env=box.env())
    assert r.returncode == 0 and "written" not in r.stdout, r.stdout
    assert "try-restart" not in box.calls() and "apt-get" not in box.calls()


def _rows(box):
    r = _bash('obs_box_panel_grade_rows "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=box.env())
    assert r.returncode == 0, r.stderr
    return [tuple(ln.split("|", 1)) for ln in r.stdout.splitlines()]


def _healthy(box):
    _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=box.env())
    (box.state / "en").write_text("enabled\n")
    (box.state / "act").write_text("active\n")
    (box.state / "pid").write_text("4242\n")
    (box.state / "wm").write_text("0x01 -1 tint2.Tint2 strih-pp tint2\n0x02 0 obs.obs strih-pp OBS\n")


def test_a_healthy_panel_passes_four_rows(box):
    _healthy(box)
    rows = _rows(box)
    assert [st for st, _ in rows] == ["OK"] * 4, rows
    assert _bash("printf %s $OBS_BOX_PANEL_ROWS").stdout == "4"


def test_each_broken_piece_fails_its_row(box):
    _healthy(box)
    (box.home / ".config" / "tint2" / "tint2rc").write_text("panel_items = LTSC\n")
    (box.state / "act").write_text("failed\n")
    (box.state / "wm").write_text("0x02 0 obs.obs strih-pp OBS\n")
    (box.state / "installed").unlink()
    rows = dict((d.split(")", 1)[0][1:], st) for st, d in _rows(box))
    assert rows == {"panel-pkg": "FAIL", "panel-config": "FAIL", "panel-unit": "FAIL", "panel-window": "FAIL"}


def test_no_window_list_is_a_note_not_a_failure(box):
    _healthy(box)
    (box.state / "wm").unlink()
    rows = dict((d.split(")", 1)[0][1:], st) for st, d in _rows(box))
    assert rows["panel-window"] == "NOTE"


def test_setup_and_verify_wire_the_panel():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert 'obs_box_panel_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"' in s
    assert "obs_box_step_applies 16h-panel" in s
    assert 'obs_box_panel_grade_report "${HERE}/.." "$USER_HOME"' in v
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
