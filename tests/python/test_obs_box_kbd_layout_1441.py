"""issue 1441 -- the keyboard layout item in the Linux OBS boxes' panel.

Owner 18.10.2026 (verbatim): "a na stream notebookoch uz je slovenska klavesnica?? da sa v liste
prepinat??" -- the boxes had us,sk + Alt+Shift but the tint2 panel showed neither the active layout nor
a way to switch it. These tests pin the helper's pure parts and loops (scripts/obs_box_kbd_layout.py),
the panel's executor item, the install of the helper and verify item 44's (panel-kbd) row.
"""
import importlib.util
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
HELPER = SCRIPTS / "obs_box_kbd_layout.py"

_spec = importlib.util.spec_from_file_location("obs_box_kbd_layout", HELPER)
kbd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(kbd)


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


# --- the helper's pure parts ---------------------------------------------------------------------

def test_parse_layouts_reads_the_setxkbmap_layout_line():
    q = "rules:      evdev\nmodel:      pc105\nlayout:     us,sk\noptions:    grp:alt_shift_toggle\n"
    assert kbd.parse_layouts(q) == ["us", "sk"]
    assert kbd.parse_layouts("rules: evdev\n") == []
    assert kbd.parse_layouts("layout:  us , sk ,\n") == ["us", "sk"]


def test_label_for_upper_cases_the_layout_and_drops_a_variant():
    assert kbd.label_for(0, ["us", "sk"]) == "US"
    assert kbd.label_for(1, ["us", "sk(qwerty)"]) == "SK"
    assert kbd.label_for(2, ["us", "sk"]) == "?"           # a group past the list
    assert kbd.label_for(0, []) == "?"                     # no layout read


def test_next_group_wraps_and_survives_no_layout():
    assert kbd.next_group(0, 2) == 1
    assert kbd.next_group(1, 2) == 0
    assert kbd.next_group(0, 0) == 0


class FakeKeyboard:
    """Stands in for X11Keyboard: a scripted sequence of groups (an OSError item = X lost)."""

    def __init__(self, groups, locks=None):
        self.groups = groups
        self.locks = locks if locks is not None else []
        self.closed = 0

    def group(self):
        item = self.groups.pop(0) if len(self.groups) > 1 else self.groups[0]
        if isinstance(item, Exception):
            raise item
        return item

    def lock(self, group):
        self.locks.append(group)

    def close(self):
        self.closed += 1


def test_watch_prints_once_and_again_only_on_a_change(capsys):
    fake = FakeKeyboard([0, 0, 1, 1, 0])
    kbd.cmd_watch(keyboard_factory=lambda: fake, layouts_reader=lambda: ["us", "sk"],
                  clock=lambda: 0.0, sleep=lambda s: None, max_iterations=5)
    assert capsys.readouterr().out.splitlines() == ["US", "SK", "US"]


def test_watch_shows_a_question_mark_when_x_is_lost_and_recovers(capsys):
    made = []

    def factory():
        made.append(1)
        if len(made) == 2:
            raise OSError("cannot open the X display")
        return FakeKeyboard([OSError("XkbGetState failed")] if len(made) == 1 else [1])

    kbd.cmd_watch(keyboard_factory=factory, layouts_reader=lambda: ["us", "sk"],
                  clock=lambda: 0.0, sleep=lambda s: None, max_iterations=4)
    assert capsys.readouterr().out.splitlines() == ["?", "SK"]
    assert len(made) == 3                                  # reconnected after each loss


def test_next_locks_the_following_group():
    locks = []
    kbd.cmd_next(keyboard_factory=lambda: FakeKeyboard([1], locks), layouts_reader=lambda: ["us", "sk"])
    assert locks == [0]


def test_main_refuses_an_unknown_mode():
    r = subprocess.run(["python3", str(HELPER), "toggle"], capture_output=True, text=True, timeout=30)
    assert r.returncode == 2 and "usage" in r.stderr


def test_main_without_x_exits_1_with_one_line(tmp_path):
    env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY")}
    r = subprocess.run(["python3", str(HELPER), "print"], capture_output=True, text=True, timeout=30, env=env)
    assert r.returncode == 1 and r.stdout == ""
    assert "obs_box_kbd_layout:" in r.stderr


# --- the panel item --------------------------------------------------------------------------------

def _cfg(text):
    return [ln for ln in text.splitlines() if not ln.startswith("#")]


def test_the_panel_has_the_layout_item_before_the_clock():
    cfg = _cfg(_bash("obs_box_panel_tint2rc").stdout)
    assert "panel_items = TSEC" in cfg
    helper = "/usr/local/lib/camera-box/obs_box_kbd_layout.py"
    assert "execp = new" in cfg
    assert "execp_command = /usr/bin/python3 %s watch" % helper in cfg
    assert "execp_continuous = 1" in cfg
    assert "execp_lclick_command = /usr/bin/python3 %s next" % helper in cfg


def test_launchers_keep_the_layout_item(tmp_path):
    home = tmp_path / "home"
    ud = home / ".config" / "systemd" / "user"
    ud.mkdir(parents=True)
    (ud / "strih-obs.service").write_text("[Service]\n")
    env = {"OBS_BOX_PANEL_SYS_UNIT_DIR": str(tmp_path / "none"), "OBS_BOX_PANEL_RUSTDESK_BIN": str(tmp_path / "none"),
           "OBS_BOX_PANEL_DISCORD_BIN": str(tmp_path / "none")}
    cfg = _cfg(_bash('obs_box_panel_tint2rc "$1"', str(home), env=env).stdout)
    assert "panel_items = LTSEC" in cfg and "execp = new" in cfg


# --- install + the (panel-kbd) row -----------------------------------------------------------------

class Box:
    def __init__(self, tmp):
        self.tmp = tmp
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.home = tmp / "home"
        self.home.mkdir()
        self.state = tmp / "state"
        self.state.mkdir()
        (self.state / "installed").write_text("")
        self.helper = tmp / "lib" / "camera-box" / "obs_box_kbd_layout.py"
        _exec(self.bin / "apt-get", "#!/bin/bash\nexit 0\n")
        _exec(self.bin / "dpkg-query", '#!/bin/bash\nprintf "install ok installed"\n')
        _exec(self.bin / "systemctl", "#!/bin/bash\nexit 0\n")
        _exec(self.bin / "chown", "#!/bin/bash\nexit 0\n")
        _exec(self.bin / "kbdpy", '#!/bin/bash\necho "$*" >> %s/kbdpy.args\ncat %s/kbd 2>/dev/null\nexit 0\n'
              % (self.state, self.state))

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "STRIH_SESSION_APPS_EUID": "1000",
                "OBS_BOX_PANEL_KBD_HELPER": str(self.helper), "OBS_BOX_PANEL_KBD_PYTHON": str(self.bin / "kbdpy"),
                "OBS_BOX_PANEL_SYS_UNIT_DIR": str(self.tmp / "none"),
                "OBS_BOX_PANEL_RUSTDESK_BIN": str(self.tmp / "none"),
                "OBS_BOX_PANEL_DISCORD_BIN": str(self.tmp / "none")}

    def install(self):
        return _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(self.home), env=self.env())

    def kbd_row(self):
        r = _bash('obs_box_panel_kbd_row "$1" "$2" "$(id -un)"', str(REPO), str(self.home), env=self.env())
        assert r.returncode == 0, r.stderr
        lines = r.stdout.splitlines()
        assert len(lines) == 1, lines
        return tuple(lines[0].split("|", 1))


def test_install_puts_the_helper_and_the_tint2rc_names_it(tmp_path):
    box = Box(tmp_path)
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert box.helper.read_bytes() == HELPER.read_bytes()
    assert oct(box.helper.stat().st_mode & 0o777) == "0o755"
    rc = (box.home / ".config" / "tint2" / "tint2rc").read_text()
    assert "execp_command = /usr/bin/python3 %s watch" % box.helper in rc
    second = box.install()
    assert second.returncode == 0 and "written" not in second.stdout, second.stdout


def test_kbd_row_ok_when_installed_and_x_reads_a_layout(tmp_path):
    box = Box(tmp_path)
    box.install()
    (box.state / "kbd").write_text("SK\n")
    st, d = box.kbd_row()
    assert st == "OK" and "(panel-kbd)" in d and "SK" in d, d
    assert (box.state / "kbdpy.args").read_text().split() == [str(box.helper), "print"]


def test_kbd_row_fails_a_missing_or_stale_helper(tmp_path):
    box = Box(tmp_path)
    (box.state / "kbd").write_text("US\n")
    st, d = box.kbd_row()
    assert st == "FAIL" and "missing" in d, d
    box.install()
    box.helper.write_text("# old\n")
    st, d = box.kbd_row()
    assert st == "FAIL" and "differs" in d, d


def test_kbd_row_notes_when_x_answers_nothing_and_fails_an_unnamed_layout(tmp_path):
    box = Box(tmp_path)
    box.install()
    st, d = box.kbd_row()
    assert st == "NOTE" and "(panel-kbd)" in d, d
    (box.state / "kbd").write_text("?\n")
    st, d = box.kbd_row()
    assert st == "FAIL" and "?" in d, d
