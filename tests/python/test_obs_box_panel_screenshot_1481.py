"""issue 1481 -- the screenshot tool in the Linux OBS box panel (flameshot, the Lightshot replacement).

Owner 25.10.2026 (verbatim): "a este operator sa pyta ako si spravi scereenshot do schranky/do suboru aby
mohol poslat!!! mal by byt v liste nastroj na to podobne ako pod windows mali lightshot". These tests pin
the launcher, the flameshot.ini merge, the openbox Print / Alt+Print binding, the install and the
(panel-shot) verify row.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"

# the stock Ubuntu openbox rc.xml's two screenshot bindings (read on stream-snv 25.10.2026), trimmed
STOCK_RC = """<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
<keyboard>
  <keybind key="W-e">
    <action name="Execute"><command>kfmclient openURL ~</command></action>
  </keybind>
  <!-- Take a screenshot of the current window with scrot when Alt+Print are pressed -->
  <keybind key="A-Print">
    <action name="Execute"><command>scrot -s</command></action>
  </keybind>
  <!-- Launch scrot when Print is pressed -->
  <keybind key="Print">
    <action name="Execute"><command>scrot</command></action>
  </keybind>
</keyboard>
</openbox_config>
"""


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


def _bash(body, *args, env=None, stdin=None, box="stream-pp"):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(PROVISION), *args],
                          capture_output=True, text=True, env=e, input=stdin, timeout=120)


def _rc(text, env=None):
    return _bash("obs_box_panel_shot_rc", stdin=text, env=env)


def test_the_stock_print_keys_run_flameshot_instead_of_scrot():
    r = _rc(STOCK_RC)
    assert r.returncode == 0, r.stderr
    out = r.stdout
    assert "scrot" not in out.replace("with scrot", "").replace("Launch scrot", "")
    assert out.count("<command>/usr/bin/flameshot gui</command>") == 2
    # the other bindings and the order are kept: exactly the two command lines changed
    old, new = STOCK_RC.splitlines(), out.splitlines()
    assert len(old) == len(new)
    changed = [(a, b) for a, b in zip(old, new) if a != b]
    assert [a.strip() for a, _ in changed] == ['<action name="Execute"><command>scrot -s</command></action>',
                                              '<action name="Execute"><command>scrot</command></action>']
    assert "kfmclient openURL ~" in out


def test_the_rc_filter_is_idempotent():
    once = _rc(STOCK_RC).stdout
    assert _rc(once).stdout == once


def test_missing_print_keys_are_added_before_the_keyboard_end():
    rc = '<openbox_config>\n<keyboard>\n  <keybind key="W-e"><action name="Execute"><command>x</command></action></keybind>\n</keyboard>\n</openbox_config>\n'
    out = _rc(rc).stdout.splitlines()
    end = out.index("</keyboard>")
    added = out[end - 2:end]
    assert added == ['  <keybind key="Print"><action name="Execute"><command>/usr/bin/flameshot gui</command></action></keybind>',
                     '  <keybind key="A-Print"><action name="Execute"><command>/usr/bin/flameshot gui</command></action></keybind>']
    assert '<command>x</command>' in "\n".join(out)


def test_a_one_line_print_keybind_is_rewritten_in_place():
    rc = ('<keyboard>\n  <keybind key="Print"><action name="Execute"><command>scrot</command></action></keybind>\n'
          '  <keybind key="Print_other"><action name="Execute"><command>keep</command></action></keybind>\n</keyboard>\n')
    out = _rc(rc).stdout
    assert '<keybind key="Print"><action name="Execute"><command>/usr/bin/flameshot gui</command>' in out
    assert "<command>keep</command>" in out          # another key is never touched
    assert out.count("A-Print") == 1                 # only the missing Alt+Print is added


def test_an_rc_without_a_keyboard_section_fails_with_no_output():
    r = _rc("<openbox_config></openbox_config>\n")
    assert r.returncode != 0 and r.stdout == ""


def _ini(text, home="/home/op"):
    return _bash('obs_box_panel_shot_ini_merge "$1"', home, stdin=text)


def test_an_empty_ini_gets_the_declared_general_section():
    r = _ini("")
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["[General]", "disabledTrayIcon=true", "filenamePattern=snimka-%F_%H-%M-%S",
                                     "savePath=/home/op/Pictures/Snimky", "savePathFixed=true",
                                     "showStartupLaunchMessage=false"]


def test_the_operator_settings_are_kept_and_the_declared_ones_win():
    ini = "[General]\ndrawColor=#00ffff\nsavePath=/tmp\nshowHelp=true\n\n[Shortcuts]\nTYPE_COPY=Ctrl+C\n"
    out = _ini(ini).stdout
    lines = out.splitlines()
    assert "drawColor=#00ffff" in lines and "showHelp=true" in lines
    assert "savePath=/home/op/Pictures/Snimky" in lines and "savePath=/tmp" not in lines
    # the missing declared keys land in [General], before the next section
    assert lines.index("savePathFixed=true") < lines.index("[Shortcuts]")
    assert lines[-1] == "TYPE_COPY=Ctrl+C"
    assert _ini(out).stdout == out                   # idempotent


def test_the_live_stream_snv_hand_fix_ini_is_already_the_declared_one():
    live = ("[General]\ndisabledTrayIcon=true\ndrawColor=#00ffff\nfilenamePattern=snimka-%F_%H-%M-%S\n"
            "savePath=/home/newlevel/Pictures/Snimky\nsavePathFixed=true\nshowHelp=true\nshowStartupLaunchMessage=false\n")
    assert _ini(live, home="/home/newlevel").stdout == live


def test_the_launcher_appears_only_where_flameshot_is_installed(tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    env = {"OBS_BOX_PANEL_SYS_UNIT_DIR": str(tmp_path / "none"), "OBS_BOX_PANEL_RUSTDESK_BIN": str(tmp_path / "x"),
           "OBS_BOX_PANEL_CHROME_BIN": str(tmp_path / "x"), "OBS_BOX_PANEL_FILES_BIN": str(tmp_path / "x"),
           "OBS_BOX_PANEL_DISCORD_BIN": str(tmp_path / "x"), "OBS_BOX_PANEL_SHOT_BIN": str(tmp_path / "flameshot")}
    for box in ("stream-pp", "strih-pp"):
        r = _bash('obs_box_panel_launchers "$1"', str(home), env=env, box=box)
        assert [ln.split("|")[0] for ln in r.stdout.splitlines()] == ["power"], r.stdout
    _exec(tmp_path / "flameshot", "#!/bin/sh\n")
    for box in ("stream-pp", "strih-pp"):       # every role (one Linux workstation type)
        r = _bash('obs_box_panel_launchers "$1"', str(home), env=env, box=box)
        rows = [ln.split("|") for ln in r.stdout.splitlines()]
        assert rows[-1] == ["screenshot", "Snímka obrazovky", "%s gui" % (tmp_path / "flameshot"),
                            "org.flameshot.Flameshot"], rows


class Box:
    def __init__(self, tmp):
        self.tmp = tmp
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.state = tmp / "state"
        self.state.mkdir()
        self.log = tmp / "calls"
        self.home = tmp / "home"
        self.home.mkdir()
        self.stock = tmp / "stock-rc.xml"
        self.stock.write_text(STOCK_RC)
        _exec(self.bin / "apt-get", '#!/bin/bash\necho "apt-get $*" >> %s\ntouch %s/installed\n' % (self.log, self.state))
        _exec(self.bin / "dpkg-query", '#!/bin/bash\n[ -e %s/installed ] && printf "install ok installed"\nexit 0\n'
              % self.state)
        _exec(self.bin / "chown", "#!/bin/bash\nexit 0\n")

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "OBS_BOX_PANEL_OPENBOX_RC_SRC": str(self.stock)}

    def install(self):
        return _bash('obs_box_panel_shot_install "$1" "$(id -un)"', str(self.home), env=self.env())

    def row(self):
        r = _bash('obs_box_panel_shot_row "$1"', str(self.home), env=self.env())
        assert r.returncode == 0, r.stderr
        return r.stdout.strip().split("|", 1)


def test_install_installs_flameshot_without_recommends_and_writes_both_files(tmp_path):
    box = Box(tmp_path)
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert "apt-get install -y --no-install-recommends flameshot" in box.log.read_text()
    assert (box.home / "Pictures" / "Snimky").is_dir()
    ini = (box.home / ".config/flameshot/flameshot.ini").read_text()
    assert "savePath=%s/Pictures/Snimky" % box.home in ini
    rc = (box.home / ".config/openbox/rc.xml").read_text()
    assert rc.count("<command>/usr/bin/flameshot gui</command>") == 2
    assert box.row()[0] == "OK"


def test_a_second_install_writes_nothing_and_keeps_the_users_own_rc(tmp_path):
    box = Box(tmp_path)
    rcdir = box.home / ".config/openbox"
    rcdir.mkdir(parents=True)
    own = STOCK_RC.replace("kfmclient openURL ~", "my-own-command")
    (rcdir / "rc.xml").write_text(own)
    assert box.install().returncode == 0
    assert "my-own-command" in (rcdir / "rc.xml").read_text()   # the user's rc.xml, not the stock one
    box.log.unlink()
    r = box.install()
    assert r.returncode == 0 and "written" not in r.stdout, r.stdout
    assert not box.log.exists()                                 # no apt-get once installed


def test_a_symlinked_rc_is_refused(tmp_path):
    box = Box(tmp_path)
    rcdir = box.home / ".config/openbox"
    rcdir.mkdir(parents=True)
    os.symlink("/etc/passwd", rcdir / "rc.xml")
    r = box.install()
    assert r.returncode != 0 and "symlink" in r.stderr


def test_each_missing_piece_fails_the_row(tmp_path):
    box = Box(tmp_path)
    assert box.row()[0] == "FAIL"                                # nothing installed
    box.install()
    assert box.row()[0] == "OK"
    ini = box.home / ".config/flameshot/flameshot.ini"
    good = ini.read_text()
    ini.write_text(good.replace("savePathFixed=true", "savePathFixed=false"))
    st, d = box.row()
    assert st == "FAIL" and "flameshot.ini" in d, d
    ini.write_text(good)
    rc = box.home / ".config/openbox/rc.xml"
    rc.write_text(rc.read_text().replace("<command>/usr/bin/flameshot gui</command>", "<command>scrot</command>", 1))
    st, d = box.row()
    assert st == "FAIL" and "Print" in d, d


def test_the_panel_step_runs_the_screenshot_install_before_the_launchers():
    text = (SCRIPTS / "lib" / "obs-box-panel.sh").read_text()
    body = text[text.index("obs_box_panel_install() {"):]
    body = body[:body.index("\n}\n")]
    assert body.index("obs_box_panel_shot_install") < body.index("done < <(obs_box_panel_launchers")
