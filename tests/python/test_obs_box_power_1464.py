"""issue 1464 -- the kiosk panel's power button + the Chrome launcher; issue 1465 item 5 -- the touchpad.

Owner 23.10.2026 (verbatim): "dalsie veci co na nasich linuxoch chyba, ikonkna ka lavo dole v liste na
vypnutie a restert pc ze si otvorim a potvrdim co chcem, dalej na stream-snv nevidim v liste chrome tak
neviem ci to nejako mizne alebo tam nikdy nebolo" and "nefunguje normalne citlivo touchpad, v restreamer
oknach nefuunguje normalne scrolovanie okien len cez klikanie na poziciu scroolbaur".

These tests pin: the power dialog's decision with a stand-in Gtk (a cancel / Escape / close runs nothing,
each confirmation runs exactly one systemctl verb, a refused command is shown and exits 1); the launcher
order (power first, every box) and Chrome only on the stream role with the binary present; the install
of the power program; the (panel-power) row against a fake logind; and the touchpad values in the writer
and both graders.
"""
import ast
import importlib.util
import os
import re
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from test_obs_box_panel_1420 import REPO, _bash, _exec, _healthy, box  # noqa: E402,F401

SCRIPTS = REPO / "scripts"
POWER = SCRIPTS / "obs_box_power.py"
POWER_ICON = SCRIPTS / "lib" / "obs-box-panel-power.svg"
# The Gtk calls the program may make on a dialog: the stand-in refuses any other name, so a misspelled
# or non-Gtk-3 call fails here instead of passing silently (the real wiring was also smoke-run under
# xvfb-run on dev1 with real Gtk 3).
DIALOG_CALLS = {"set_keep_above", "set_position", "set_resizable", "set_default_response", "show_all",
                "destroy"}
MESSAGE_CALLS = {"format_secondary_text", "set_keep_above", "run", "destroy"}


def _load_power():
    spec = importlib.util.spec_from_file_location("obs_box_power_1464", POWER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# --- the power dialog, with a stand-in Gtk ----------------------------------------------------------

class _Gtk:
    """The calls PowerDialog makes on gi.repository.Gtk, recorded in one ordered list."""

    def __init__(self, response):
        self.response = response
        self.calls = []
        gtk = self

        class ResponseType:
            CANCEL = -6
            DELETE_EVENT = -4
            OK = -5

        class WindowPosition:
            CENTER = "center"

        class MessageType:
            ERROR = "error"

        class ButtonsType:
            OK = "ok"

        class Button:
            def __init__(self, label, resp):
                self.label, self.resp = label, resp

            def grab_focus(self):
                gtk.calls.append(("grab_focus", self.label))

        class ContentBox:
            def set_spacing(self, n):
                gtk.calls.append(("set_spacing", n))

            def add(self, w):
                gtk.calls.append(("add", w))

        class Label:
            def __init__(self, label=""):
                self.label = label

        class Dialog:
            def __init__(self, title=None):
                gtk.calls.append(("Dialog", title))
                gtk.dialog = self
                self.buttons = []

            def __getattr__(self, name):
                if name not in DIALOG_CALLS:
                    raise AttributeError("not a Gtk.Dialog call the stand-in knows: %s" % name)

                def rec(*a):
                    gtk.calls.append((name,) + a)
                return rec

            def add_button(self, label, resp):
                b = Button(label, resp)
                self.buttons.append(b)
                gtk.calls.append(("add_button", label, resp))
                return b

            def get_content_area(self):
                return ContentBox()

            def run(self):
                gtk.calls.append(("run",))
                return gtk.response

        class MessageDialog:
            def __init__(self, **kw):
                gtk.calls.append(("MessageDialog", kw))

            def __getattr__(self, name):
                if name not in MESSAGE_CALLS:
                    raise AttributeError("not a Gtk.MessageDialog call the stand-in knows: %s" % name)

                def rec(*a):
                    gtk.calls.append(("msg." + name,) + a)
                return rec

        self.ResponseType, self.WindowPosition = ResponseType, WindowPosition
        self.MessageType, self.ButtonsType = MessageType, ButtonsType
        self.Label, self.Dialog, self.MessageDialog = Label, Dialog, MessageDialog

    def names(self):
        return [c[0] for c in self.calls]


def _dialog(response, rc=0, err=""):
    mod = _load_power()
    gtk = _Gtk(response)
    ran = []

    def run_cmd(cmd):
        ran.append(cmd)
        return rc, err

    d = mod.PowerDialog(gtk, run_cmd=run_cmd, log=lambda m: None)
    return mod, gtk, d, ran


def test_the_dialog_is_slovak_with_cancel_reboot_poweroff_and_cancel_is_the_default():
    mod, gtk, _d, _ran = _dialog(-6)
    labels = [c[1] for c in gtk.calls if c[0] == "add_button"]
    assert labels == ["Zrušiť", "Reštartovať", "Vypnúť"]
    assert ("set_default_response", gtk.ResponseType.CANCEL) in gtk.calls
    # the focus lands on Zrušiť, AFTER show_all (show_all would move it otherwise)
    names = gtk.names()
    assert names.index("show_all") < names.index("grab_focus")
    assert ("grab_focus", "Zrušiť") in gtk.calls
    assert ("set_keep_above", True) in gtk.calls        # never hidden behind a fullscreen projector
    texts = [c[1].label for c in gtk.calls if c[0] == "add"]
    assert any("urobiť" in t for t in texts), texts
    assert mod.TITLE == "Vypnúť počítač"


@pytest.mark.parametrize("response", [-6, -4, -5, 0, 99, None])
def test_a_cancel_escape_close_or_unknown_response_runs_nothing(response):
    _mod, gtk, d, ran = _dialog(response)
    assert d.run() == 0
    assert ran == []
    assert "destroy" in gtk.names()


def test_a_confirmed_poweroff_runs_exactly_systemctl_poweroff():
    mod, _gtk, d, ran = _dialog(None)
    d._gtk.response = mod.RESPONSE_POWEROFF
    assert d.run() == 0
    assert ran == [["systemctl", "poweroff"]]


def test_a_confirmed_reboot_runs_exactly_systemctl_reboot():
    mod, _gtk, d, ran = _dialog(None)
    d._gtk.response = mod.RESPONSE_REBOOT
    assert d.run() == 0
    assert ran == [["systemctl", "reboot"]]


def test_the_two_confirm_ids_are_not_gtk_response_values():
    mod = _load_power()
    # every Gtk.ResponseType is negative; a confirmation must never collide with one
    assert mod.RESPONSE_POWEROFF > 0 and mod.RESPONSE_REBOOT > 0
    assert mod.RESPONSE_POWEROFF != mod.RESPONSE_REBOOT


def test_a_refused_command_is_shown_and_exits_1():
    mod, gtk, d, ran = _dialog(None, rc=1, err="Access denied")
    d._gtk.response = mod.RESPONSE_POWEROFF
    assert d.run() == 1
    assert ran == [["systemctl", "poweroff"]]
    msg = [c for c in gtk.calls if c[0] == "MessageDialog"]
    assert msg and "systemctl poweroff" in msg[0][1]["text"]
    assert ("msg.format_secondary_text", "Access denied") in gtk.calls
    assert ("msg.run",) in gtk.calls


def test_the_runner_reports_a_missing_binary_as_a_failure():
    mod = _load_power()
    rc, err = mod._run_command(["/nonexistent/obs-box-power-test-binary"])
    assert rc != 0 and err


def test_the_program_imports_without_gi():
    # CI has no gi; the module must only import it inside main(), never at module level
    tree = ast.parse(POWER.read_text())
    top = [n for n in tree.body if isinstance(n, (ast.Import, ast.ImportFrom))]
    names = [a.name for n in top if isinstance(n, ast.Import) for a in n.names]
    names += [n.module or "" for n in top if isinstance(n, ast.ImportFrom)]
    assert not [m for m in names if m == "gi" or m.startswith("gi.")], names


# --- the launchers ------------------------------------------------------------------------------------

def _home(tmp_path, chrome=True):
    home = tmp_path / "home"
    ud = home / ".config" / "systemd" / "user"
    ud.mkdir(parents=True)
    (ud / "strih-obs.service").write_text("[Service]\n")
    sysd = tmp_path / "sysunits"
    sysd.mkdir()
    (sysd / "restreamer.service").write_text("[Service]\n")
    ch = tmp_path / "google-chrome-stable"
    if chrome:
        _exec(ch, "#!/bin/sh\n")
    env = {"OBS_BOX_PANEL_SYS_UNIT_DIR": str(sysd), "OBS_BOX_PANEL_RUSTDESK_BIN": str(tmp_path / "none"),
           "OBS_BOX_PANEL_DISCORD_BIN": str(tmp_path / "none"), "OBS_BOX_PANEL_FILES_BIN": str(tmp_path / "none"),
           "OBS_BOX_PANEL_CHROME_BIN": str(ch), "OBS_BOX_PANEL_POWER_HELPER": "/opt/test/obs-box-power"}
    return home, env


def _launchers(home, env, box):
    r = _bash('obs_box_panel_launchers "$1"', str(home), env=env, box=box)
    assert r.returncode == 0, r.stderr
    return [ln.split("|") for ln in r.stdout.splitlines()]


@pytest.mark.parametrize("box_name", ["stream-pp", "strih-pp", "strih-snv"])
def test_the_power_launcher_is_first_on_every_box(tmp_path, box_name):
    home, env = _home(tmp_path)
    rows = _launchers(home, env, box_name)
    # the icon ships with the panel (an absolute path), never a theme name the kiosk may not carry
    icon = str(home / ".local/share/icons/obs-box-panel/power.svg")
    assert rows[0] == ["power", "Vypnúť", "/usr/bin/python3 /opt/test/obs-box-power", icon]


def test_chrome_is_a_launcher_on_the_stream_role_where_it_is_installed(tmp_path):
    home, env = _home(tmp_path)
    rows = _launchers(home, env, "stream-pp")
    assert [x[0] for x in rows] == ["power", "obs", "restreamer", "chrome"]
    assert rows[3] == ["chrome", "Chrome", str(tmp_path / "google-chrome-stable"), "google-chrome"]


def test_chrome_is_never_a_launcher_on_the_strih_role(tmp_path):
    home, env = _home(tmp_path)
    assert "chrome" not in [x[0] for x in _launchers(home, env, "strih-pp")]


def test_no_chrome_binary_no_chrome_launcher(tmp_path):
    home, env = _home(tmp_path, chrome=False)
    assert "chrome" not in [x[0] for x in _launchers(home, env, "stream-pp")]


def test_the_config_puts_the_power_button_at_the_left_end(tmp_path):
    home, env = _home(tmp_path)
    r = _bash('obs_box_panel_tint2rc "$1"', str(home), env=env)
    apps = [ln.split(" = ", 1)[1] for ln in r.stdout.splitlines() if ln.startswith("launcher_item_app = ")]
    assert apps[0] == str(home / ".local/share/applications/obs-box-panel-power.desktop")


# --- install + the (panel-power) row ------------------------------------------------------------------

def test_install_puts_the_power_program_and_its_launcher(box):
    r = _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=box.env())
    assert r.returncode == 0, r.stderr
    assert box.power.read_bytes() == POWER.read_bytes()
    assert oct(box.power.stat().st_mode & 0o777) == "0o755"
    desk = (box.home / ".local/share/applications/obs-box-panel-power.desktop").read_text()
    icon = box.home / ".local/share/icons/obs-box-panel/power.svg"
    assert icon.read_bytes() == POWER_ICON.read_bytes()
    assert "Name=Vypnúť" in desk and ("Icon=%s" % icon) in desk
    assert "Exec=/usr/bin/python3 %s" % box.power in desk
    second = _bash('obs_box_panel_install "$1" "$2" "$(id -un)"', str(REPO), str(box.home), env=box.env())
    assert second.returncode == 0 and "written" not in second.stdout, second.stdout


def _power_row(box, **extra):
    env = {**box.env(), **extra}
    r = _bash('obs_box_panel_power_row "$1" "$(id -un)"', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert len(lines) == 1, lines
    return tuple(lines[0].split("|", 1))


def test_the_power_row_passes_when_logind_says_yes_twice(box):
    _healthy(box)
    st, d = _power_row(box)
    assert st == "OK", d
    calls = box.calls()
    # the read runs in the user manager, the launcher's own context
    assert "systemd-run --user" in calls and "busctl --timeout=10 call org.freedesktop.login1" in calls
    assert "CanPowerOff" in calls and "CanReboot" in calls


@pytest.mark.parametrize("which,word", [("off", "challenge"), ("reb", "no"), ("off", "na")])
def test_the_power_row_fails_when_logind_would_refuse(box, which, word):
    _healthy(box)
    (box.state / which).write_text('s "%s"\n' % word)
    st, d = _power_row(box)
    assert st == "FAIL" and ("=%s" % word) in d, d


def test_the_power_row_fails_when_logind_does_not_answer(box):
    _healthy(box)
    (box.state / "off").write_text("")
    st, d = _power_row(box)
    assert st == "FAIL" and "CanPowerOff=no answer" in d, d


def test_a_hung_logind_read_is_bounded(box):
    _healthy(box)
    _exec(box.bin / "busctl", "#!/bin/bash\nexec sleep 30\n")
    t0 = time.monotonic()
    st, d = _power_row(box, OBS_BOX_PANEL_LOGIND_TIMEOUT="1")
    assert time.monotonic() - t0 < 15
    assert st == "FAIL" and "no answer" in d, d


def test_the_power_row_fails_a_missing_or_old_program(box):
    _healthy(box)
    box.power.write_text("# old\n")
    st, d = _power_row(box)
    assert st == "FAIL" and "differs from the checkout" in d, d


def test_as_root_the_logind_read_runs_as_the_desktop_user(box):
    _healthy(box)
    _exec(box.bin / "sudo", '#!/bin/bash\necho "sudo $*" >> %s\nshift 2\nexec env "$@"\n' % box.log)
    st, d = _power_row(box, STRIH_SESSION_APPS_EUID="0")
    assert st == "OK", d
    me = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    uid = os.getuid()
    assert "sudo -u %s XDG_RUNTIME_DIR=/run/user/%s timeout 15 systemd-run --user" % (me, uid) in box.calls()


def test_the_logind_read_never_hands_its_stdin_to_the_transient_unit(box):
    # systemd-run --pipe passes the caller's stdin on; the read must give it /dev/null
    _healthy(box)
    _exec(box.bin / "systemd-run", '#!/bin/bash\nwhile [ "${1#--}" != "$1" ]; do shift; done\n'
          'readlink /proc/self/fd/0 >> %s/stdin\nexec "$@"\n' % box.state)
    env = {**box.env()}
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\n'
                        'obs_box_panel_power_row "$4" "$(id -un)"', "h",
                        str(SCRIPTS / "lib" / "strih-box-facts.sh"), "stream-pp",
                        str(SCRIPTS / "lib" / "strih-provision.sh"), str(REPO)],
                       capture_output=True, text=True, env={**os.environ, **env}, timeout=60, input="operator tty\n")
    assert r.returncode == 0, r.stderr
    seen = (box.state / "stdin").read_text().split()
    assert seen and all(s == "/dev/null" for s in seen), seen


# --- the touchpad (issue 1465 item 5) -----------------------------------------------------------------

def _fn_body(path, name):
    text = path.read_text()
    m = re.search(r"^%s\(\) \{\n.*?^\}\n" % re.escape(name), text, re.S | re.M)
    assert m, name
    return m.group(0)


def test_the_touchpad_conf_carries_the_23_10_scroll_step_and_pointer_speed(tmp_path):
    body = _fn_body(SCRIPTS / "lib" / "obs-box-kiosk.sh", "obs_box_touchpad")
    body = body.replace("/etc/X11/xorg.conf.d", str(tmp_path / "xorg.conf.d"))
    lib = tmp_path / "fn.sh"
    lib.write_text(body)
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; obs_box_touchpad imag', "h", str(lib)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    conf = (tmp_path / "xorg.conf.d" / "30-touchpad-tap.conf").read_text()
    assert 'Option "ScrollPixelDistance" "15"' in conf
    assert 'Option "AccelSpeed" "0.3"' in conf
    assert '"50"' not in conf
    for opt in ('Option "Tapping" "on"', 'Option "TappingDrag" "on"', 'Option "NaturalScrolling" "on"'):
        assert opt in conf


OLD_CONF = """Section "InputClass"
    Identifier "touchpad tap-to-click"
    MatchIsTouchpad "on"
    Driver "libinput"
    Option "Tapping" "on"
    Option "TappingDrag" "on"
    Option "NaturalScrolling" "on"
    Option "ScrollPixelDistance" "50"
EndSection
"""
NEW_CONF = OLD_CONF.replace('    Option "ScrollPixelDistance" "50"\n',
                            '    Option "ScrollPixelDistance" "15"\n    Option "AccelSpeed" "0.3"\n')


def _gather_touchpad(tmp_path, conf_text):
    line = [ln for ln in (SCRIPTS / "lib" / "obs-box-baseline-verify.sh").read_text().splitlines()
            if ln.startswith('echo "touchpad=')]
    assert len(line) == 1, line
    f = tmp_path / "30-touchpad-tap.conf"
    if conf_text is not None:
        f.write_text(conf_text)
    cmd = line[0].replace("/etc/X11/xorg.conf.d/30-touchpad-tap.conf", str(f))
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + cmd], capture_output=True, text=True,
                          timeout=30).stdout.strip()


def test_the_baseline_touchpad_row_needs_the_new_values(tmp_path):
    assert _gather_touchpad(tmp_path, NEW_CONF) == "touchpad=1"
    assert _gather_touchpad(tmp_path, OLD_CONF) == "touchpad=0"
    assert _gather_touchpad(tmp_path, NEW_CONF.replace('    Option "AccelSpeed" "0.3"\n', "")) == "touchpad=0"
    assert _gather_touchpad(tmp_path, None) == "touchpad=0"


def _imag_conf_ok(conf):
    r = subprocess.run(["bash", "-c", '. "$1"; if imag_touchpad_conf_ok "$2"; then echo ACCEPT; else echo REJECT; fi',
                        "h", str(SCRIPTS / "verify-imag.sh"), conf], capture_output=True, text=True, timeout=60)
    return r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr


def test_verify_imag_check_w_reads_the_new_values():
    assert _imag_conf_ok(NEW_CONF) == "ACCEPT"
    assert _imag_conf_ok(OLD_CONF) == "REJECT"
    assert _imag_conf_ok(NEW_CONF.replace('    Option "AccelSpeed" "0.3"\n', "")) == "REJECT"
    assert _imag_conf_ok(NEW_CONF.replace('"AccelSpeed" "0.3"', '"AccelSpeed" "0x3"')) == "REJECT"
