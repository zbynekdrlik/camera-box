"""issue 1466 -- the gradual move of the Linux OBS boxes from openbox + tint2 to XFCE.

Owner ROZHODNUTE 6737530836 (9.11.2026): "schvalujem postupny prechod na xfce". Design 6737548111: the
desktop is a box FACT (STRIH_DESKTOP = openbox | xfce), the session start is ONE script rendered from the
openbox autostart's functions, the xfce4-panel and the keys render from the tint2 panel's launcher list and
the openbox key commands, and verify grades the session the fact names. These tests pin the fact, the
session start (openbox byte-identical), the xfconf merge + grade helper, the install (under the caller's
set -euo pipefail, every path moved into the test's tmp dir) and the grade rows.
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
XFCONF = SCRIPTS / "obs_box_xfconf.py"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
ME = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
MYGROUP = subprocess.run(["id", "-gn"], capture_output=True, text=True, check=True).stdout.strip()


@pytest.fixture(autouse=True)
def _xfce_box_facts(tmp_path_factory, monkeypatch):
    """The owner rejected XFCE on 13.11.2026 and every real box is openbox again, so the xfce code path is
    tested on a COPY of the fact files where stream-snv reads `STRIH_DESKTOP=xfce`."""
    boxes = tmp_path_factory.mktemp("xfce-boxes")
    for f in (SCRIPTS / "strih-boxes").iterdir():
        if f.is_file():
            shutil.copy(f, boxes / f.name)
    env = boxes / "stream-snv.env"
    env.write_text(env.read_text().replace("STRIH_DESKTOP=openbox", "STRIH_DESKTOP=xfce"))
    monkeypatch.setenv("STRIH_BOXES_DIR", str(boxes))
    return boxes


def _bash(body, *args, box="stream-snv", env=None):
    script = ('set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\n. "$4"\nshift 4\n'
              'fail() { echo "FAIL: $*" >&2; return 1; }\n' + body)
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "h", str(FACTS), box, str(BASELINE), str(PROVISION), *args],
                          capture_output=True, text=True, env=e, timeout=120)


def _fact(box, key):
    for line in (Path(os.environ["STRIH_BOXES_DIR"]) / f"{box}.env").read_text().splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1]
    return None


# --- the fact ---------------------------------------------------------------------------------------------

def test_every_real_box_is_openbox_after_the_owner_rejected_xfce():
    # owner 13.11.2026: "xdce nepaci sa mi takze vrat stream-snv naspat ten co mame je lepsi"
    for box in ("strih-snv", "stream-snv", "strih-pp", "stream-pp"):
        line = [l for l in (SCRIPTS / "strih-boxes" / f"{box}.env").read_text().splitlines()
                if l.startswith("STRIH_DESKTOP=")]
        assert line == ["STRIH_DESKTOP=openbox"], (box, line)


def test_the_desktop_fact_is_the_last_key_and_the_accessor_reads_it():
    # issues 1510 + 1513 appended the four Dante latency / TX facts after it
    r = _bash("strih_box_fact_keys | tail -n 5 | head -n 1; strih_box_desktop; echo")
    assert r.stdout.split() == ["STRIH_DESKTOP", "xfce"], r.stderr


@pytest.mark.parametrize("value", ["gnome", "", "XFCE", "openbox xfce"])
def test_the_loader_refuses_any_other_desktop(tmp_path, value):
    boxes = tmp_path / "boxes"
    shutil.copytree(os.environ["STRIH_BOXES_DIR"], boxes)
    f = boxes / "stream-snv.env"
    f.write_text(f.read_text().replace("STRIH_DESKTOP=xfce", f"STRIH_DESKTOP={value}"))
    r = subprocess.run(["bash", "-c", '. "$1"; strih_box_load stream-snv', "h", str(FACTS)],
                       capture_output=True, text=True, env=dict(os.environ, STRIH_BOXES_DIR=str(boxes)))
    assert r.returncode != 0
    if value and " " not in value:
        assert "STRIH_DESKTOP" in r.stderr and "openbox or xfce" in r.stderr, r.stderr


# --- the ONE session start --------------------------------------------------------------------------------

@pytest.mark.parametrize("box", ["strih-snv", "stream-snv", "stream-pp", "strih-pp"])
def test_the_openbox_autostart_is_the_openbox_session_start_byte_for_byte(box):
    a = _bash("strih_openbox_autostart_text", box=box)
    b = _bash("strih_session_start_text openbox", box=box)
    assert a.returncode == 0 and b.returncode == 0, a.stderr + b.stderr
    assert a.stdout == b.stdout and "systemctl --user start strih-obs.service" in a.stdout


def test_the_xfce_session_start_differs_only_by_the_tint2_line():
    o = _bash("strih_session_start_text openbox").stdout.splitlines()
    x = _bash("strih_session_start_text xfce").stdout.splitlines()
    assert [ln for ln in o if ln not in x] == ["systemctl --user start obs-box-panel.service || true"]
    assert [ln for ln in x if ln not in o] == []
    for want in ("xset s off -dpms s noblank 2>/dev/null || true", "systemctl --user start strih-obs.service || true",
                 "systemctl --user start restreamer.service || true"):
        assert want in x


def test_a_session_start_for_an_unknown_desktop_prints_nothing_and_fails():
    r = _bash("strih_session_start_text gnome")
    assert r.returncode != 0 and r.stdout == "" and "openbox or xfce" in r.stderr


# --- the xfconf helper ------------------------------------------------------------------------------------

def _xfconf(*args, stdin=None):
    return subprocess.run([sys.executable, str(XFCONF), *args], input=stdin, capture_output=True, text=True)


LIVE_XFWM4 = """<?xml version="1.1" encoding="UTF-8"?>

<channel name="xfwm4" version="1.0">
  <property name="general" type="empty">
    <property name="activate_action" type="string" value="bring"/>
    <property name="use_compositing" type="bool" value="true"/>
    <property name="workspace_count" type="int" value="4"/>
  </property>
</channel>
"""


def test_merge_sets_the_declared_property_and_keeps_every_other(tmp_path):
    f = tmp_path / "xfwm4.xml"
    f.write_text(LIVE_XFWM4)
    r = _xfconf("merge", "xfwm4", str(f), "-", stdin="/general/use_compositing\tbool\tfalse\n")
    assert r.returncode == 0, r.stderr
    assert 'name="use_compositing" type="bool" value="false"' in r.stdout
    assert 'name="activate_action" type="string" value="bring"' in r.stdout
    assert 'name="workspace_count" type="int" value="4"' in r.stdout
    f.write_text(r.stdout)
    again = _xfconf("merge", "xfwm4", str(f), "-", stdin="/general/use_compositing\tbool\tfalse\n")
    assert again.stdout == r.stdout                    # a re-run writes the same bytes
    assert _xfconf("check", str(f), "-", stdin="/general/use_compositing\tbool\tfalse\n").stdout.strip() == "ok"


def test_check_names_a_wrong_value_and_a_missing_property(tmp_path):
    f = tmp_path / "xfwm4.xml"
    f.write_text(LIVE_XFWM4)
    r = _xfconf("check", str(f), "-", stdin="/general/use_compositing\tbool\tfalse\n")
    assert r.returncode == 1 and "use_compositing" in r.stdout and "true" in r.stdout
    r = _xfconf("check", str(f), "-", stdin="/general/nope\tint\t1\n")
    assert r.returncode == 1 and "/general/nope missing" in r.stdout
    r = _xfconf("check", str(tmp_path / "absent.xml"), "-", stdin="/a\tint\t1\n")
    assert r.returncode == 1 and "missing" in r.stdout


def test_replace_drops_the_old_subtree_and_arrays_keep_their_child_properties(tmp_path):
    f = tmp_path / "panel.xml"
    f.write_text("""<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfce4-panel" version="1.0">
  <property name="panels" type="array">
    <value type="int" value="1"/><value type="int" value="2"/>
    <property name="panel-2" type="empty"><property name="size" type="uint" value="48"/></property>
  </property>
  <property name="plugins" type="empty">
    <property name="plugin-18" type="string" value="directorymenu"/>
  </property>
  <property name="keep-me" type="int" value="7"/>
</channel>
""")
    spec = ("/panels\treplace\n/plugins\treplace\n/panels\tarray:int\t1\n/panels/panel-1/size\tuint\t30\n"
            "/plugins/plugin-1\tstring\tclock\n/panels/panel-1/plugin-ids\tarray:int\t1\n")
    r = _xfconf("merge", "xfce4-panel", str(f), "-", stdin=spec)
    assert r.returncode == 0, r.stderr
    assert "panel-2" not in r.stdout and "directorymenu" not in r.stdout
    assert 'name="keep-me" type="int" value="7"' in r.stdout
    f.write_text(r.stdout)
    assert _xfconf("check", str(f), "-", stdin=spec).stdout.strip() == "ok"
    bad = spec.replace("plugin-ids\tarray:int\t1", "plugin-ids\tarray:int\t1,2")
    assert _xfconf("check", str(f), "-", stdin=bad).returncode == 1


def test_only_a_dropped_direct_child_of_a_replaced_subtree_is_stale(tmp_path):
    """A running panel keeps its own settings under a declared panel (mode, opacity ...): never stale,
    or every run would reset and restart the operator's panel. A dropped panel / plugin / key is."""
    spec = ("/panels\treplace\n/plugins\treplace\n/panels\tarray:int\t1\n/panels/dark-mode\tbool\ttrue\n"
            "/panels/panel-1/size\tuint\t30\n/plugins/plugin-1\tstring\tclock\n")
    f = tmp_path / "panel.xml"
    f.write_text(_xfconf("merge", "xfce4-panel", str(tmp_path / "none.xml"), "-", stdin=spec).stdout)
    text = f.read_text().replace('<property name="size" type="uint" value="30" />',
                                 '<property name="size" type="uint" value="30" /><property name="mode" type="uint" value="0" />'
                                 '<property name="background-style" type="uint" value="0" />')
    text = text.replace('<property name="plugin-1" type="string" value="clock" />',
                        '<property name="plugin-1" type="string" value="clock"><property name="digital-format" type="string" value="%R" /></property>')
    f.write_text(text)
    assert _xfconf("check", str(f), "-", stdin=spec).stdout.strip() == "ok"
    f.write_text(text.replace("</channel>", '<property name="plugins2" type="int" value="1" /></channel>'))
    assert _xfconf("check", str(f), "-", stdin=spec).stdout.strip() == "ok"          # not under a root
    stale = text.replace('<property name="plugin-1" ', '<property name="plugin-9" type="string" value="pager" /><property name="plugin-1" ')
    f.write_text(stale)
    r = _xfconf("check", str(f), "-", stdin=spec)
    assert r.returncode == 1 and "/plugins/plugin-9 is not declared" in r.stdout, r.stdout


def test_an_unreadable_file_or_another_channel_is_never_merged_over(tmp_path):
    f = tmp_path / "x.xml"
    f.write_text("<channel name='xfwm4'><property")
    r = _xfconf("merge", "xfwm4", str(f), "-", stdin="/a\tint\t1\n")
    assert r.returncode == 2 and r.stdout == ""
    f.write_text('<channel name="xfce4-panel" version="1.0"/>')
    r = _xfconf("merge", "xfwm4", str(f), "-", stdin="/a\tint\t1\n")
    assert r.returncode == 2 and r.stdout == ""


@pytest.mark.parametrize("line", ["no-tab", "/a\tweird\t1", "relative\tint\t1", "/a\tarray:nope\t1"])
def test_a_bad_spec_line_is_refused(tmp_path, line):
    r = _xfconf("merge", "xfwm4", str(tmp_path / "x.xml"), "-", stdin=line + "\n")
    assert r.returncode == 2 and r.stdout == ""


# --- the panel and the keys -------------------------------------------------------------------------------

def _seams(tmp_path):
    return {"OBS_BOX_PANEL_CHROME_BIN": str(tmp_path / "no-chrome"),
            "OBS_BOX_PANEL_SHOT_BIN": str(tmp_path / "no-flameshot"),
            "OBS_BOX_PANEL_RUSTDESK_BIN": str(tmp_path / "no-rustdesk"),
            "OBS_BOX_PANEL_DISCORD_BIN": str(tmp_path / "no-discord"),
            "OBS_BOX_PANEL_FILES_BIN": str(tmp_path / "no-pcmanfm"),
            "OBS_BOX_PANEL_SYS_UNIT_DIR": str(tmp_path / "no-sysunits")}


def _home_with_obs(tmp_path):
    home = tmp_path / "home"
    (home / ".config" / "systemd" / "user").mkdir(parents=True)
    (home / ".config" / "systemd" / "user" / "strih-obs.service").write_text("[Service]\n")
    return home


def test_the_panel_carries_the_tint2_launchers_in_order_and_the_standard_plugins(tmp_path):
    home = _home_with_obs(tmp_path)
    r = _bash('obs_box_xfce_panel_plugins "$1"', str(home), env=_seams(tmp_path))
    rows = [ln.split("|") for ln in r.stdout.splitlines()]
    assert [x[1] for x in rows] == ["applicationsmenu", "launcher", "launcher", "tasklist", "separator",
                                    "xkb", "systray", "pulseaudio", "clock", "actions"], r.stdout + r.stderr
    assert [x[2] for x in rows if x[1] == "launcher"] == ["power", "obs"]
    assert [int(x[0]) for x in rows] == list(range(1, 11))


def test_every_role_shows_the_keyboard_layout_on_the_panel_1466(tmp_path):
    """Owner 12.11.2026 on stream-snv: no layout switcher on the panel, nobody remembers Alt+Shift. The
    tint2 panel had one (issue 1441); the xfce panel carries XFCE's xkb plugin as text, click switches."""
    home = _home_with_obs(tmp_path)
    for box in ("stream-snv", "strih-snv"):
        r = _bash('obs_box_xfce_panel_plugins "$1"; obs_box_xfce_panel_spec "$1"', str(home), box=box,
                  env=_seams(tmp_path))
        kinds = [ln.split("|")[1] for ln in r.stdout.splitlines() if ln.count("|") == 2]
        assert "xkb" in kinds and kinds.index("xkb") < kinds.index("systray"), (box, r.stdout + r.stderr)
        pid = next(ln.split("|")[0] for ln in r.stdout.splitlines() if ln.endswith("|xkb|"))
        assert f"/plugins/plugin-{pid}\tstring\txkb" in r.stdout, r.stdout
        assert f"/plugins/plugin-{pid}/display-type\tuint\t1" in r.stdout, r.stdout
        assert f"/plugins/plugin-{pid}/display-name\tuint\t0" in r.stdout, r.stdout
        assert f"/plugins/plugin-{pid}/group-policy\tuint\t0" in r.stdout, r.stdout


def test_the_strih_role_gets_no_pulseaudio_plugin_and_no_headphone_keys(tmp_path):
    home = _home_with_obs(tmp_path)
    r = _bash('obs_box_xfce_panel_plugins "$1"; obs_box_xfce_keys_spec', str(home), box="strih-snv", env=_seams(tmp_path))
    assert "pulseaudio" not in r.stdout and "XF86Audio" not in r.stdout, r.stdout
    assert "/commands/custom/Print\tstring\t%s gui" % (tmp_path / "no-flameshot") in r.stdout
    assert "XF86MonBrightnessUp\tstring\t/usr/local/bin/obs-box-brightness up" in r.stdout


def test_the_stream_role_binds_the_headphone_keys_to_the_helper_and_silences_the_plugin_keys(tmp_path):
    home = _home_with_obs(tmp_path)
    r = _bash('obs_box_xfce_keys_spec; obs_box_xfce_panel_spec "$1"', str(home), env=_seams(tmp_path))
    assert "XF86AudioRaiseVolume\tstring\t/usr/bin/python3 /usr/local/bin/obs-box-headphones up" in r.stdout
    assert "enable-keyboard-shortcuts\tbool\tfalse" in r.stdout
    assert "/panels/panel-1/plugin-ids\tarray:int\t1,2,3,4,5,6,7,8,9" in r.stdout


def test_the_panel_keys_match_the_openbox_key_commands():
    """The brightness keys run the same helper the openbox rc.xml binds (one source of commands)."""
    r = _bash("obs_box_brightness_keybinds_xml; obs_box_xfce_keys_spec", box="strih-snv")
    for cmd in ("/usr/local/bin/obs-box-brightness up", "/usr/local/bin/obs-box-brightness down"):
        assert r.stdout.count(cmd) == 2, r.stdout


# --- the install + the grade, end to end on a fake box ----------------------------------------------------

FAKE_PGREP = r"""#!%(py)s
# a pgrep with the real rules this lib depends on: -x matches the process NAME, which the kernel cuts at
# 15 characters (a longer -x pattern is refused and matches nothing, like procps); -f searches the whole
# command line as an extended regex. The "processes" are the command lines in RUNNING, one per line.
import re, sys
args = sys.argv[1:]
mode, pat = None, None
i = 0
while i < len(args):
    a = args[i]
    if a == "-u":
        i += 2
        continue
    if a in ("-x", "-f"):
        mode = a
    else:
        pat = a
    i += 1
procs = [ln for ln in open(%(running)r).read().splitlines() if ln.strip()]
if mode == "-x":
    if len(pat) > 15:
        sys.stderr.write("pgrep: pattern that searches for process name longer than 15 characters will result in zero matches\n")
        sys.exit(1)
    hits = [ln for ln in procs if ln.split()[0].rsplit("/", 1)[-1][:15] == pat]
else:
    hits = [ln for ln in procs if re.search(pat, ln)]
for ln in hits:
    print(ln)          # stands in for the pid: a restarted process prints another line
sys.exit(0 if hits else 1)
"""

FAKE_PANEL = r"""#!%(py)s
# xfce4-panel --restart: logs its display + bus; the new panel replaces the old one in RUNNING; with
# REVERT present it first saves an OLD layout back into the JSON xfconfd (what a panel that never
# took the new layout would do on its way out).
import json, os, sys
with open(%(plog)r, "a") as f:
    f.write("%%s %%s %%s\n" %% (os.environ.get("DISPLAY", "-"), os.environ.get("DBUS_SESSION_BUS_ADDRESS", "-"), " ".join(sys.argv[1:])))
if os.path.exists(%(revert)r):
    st = json.load(open(%(state)r))
    st["xfce4-panel"]["/panels/panel-1/plugin-ids"] = ["1"]
    json.dump(st, open(%(state)r, "w"))
lines = [("xfce4-panel --restarted" if ln.split()[0] == "xfce4-panel" else ln)
         for ln in open(%(running)r).read().splitlines()]
open(%(running)r, "w").write("".join(ln + "\n" for ln in lines))
"""

FAKE_XFCONF_QUERY = r"""#!%(py)s
# xfconf-query against a JSON "xfconfd" (channel -> path -> value or list); logs each call + its bus.
import json, os, sys
state_f, log_f = %(state)r, %(log)r
st = json.load(open(state_f)) if os.path.exists(state_f) else {}
args = sys.argv[1:]
with open(log_f, "a") as f:
    f.write(os.environ.get("DBUS_SESSION_BUS_ADDRESS", "-") + " " + " ".join(args) + "\n")
if os.path.exists(%(broken)r):
    sys.stderr.write("broken bus\n"); sys.exit(1)
ch = args[args.index("-c") + 1]
props = st.setdefault(ch, {})
if "-l" in args:
    for k in sorted(props):
        print(k)
    sys.exit(0)
p = args[args.index("-p") + 1]
vals = [args[i + 1] for i, a in enumerate(args) if a == "-s"]
if "-r" in args:
    gone = [k for k in props if k == p or k.startswith(p + "/")]
    for k in gone:
        del props[k]
    json.dump(st, open(state_f, "w"))
    sys.exit(0 if gone else 1)
if vals:
    props[p] = vals if ("-a" in args or len(vals) > 1) else vals[0]
    json.dump(st, open(state_f, "w"))
    sys.exit(0)
if p not in props:
    sys.stderr.write("Property \"%%s\" does not exist on channel \"%%s\".\n" %% (p, ch)); sys.exit(1)
v = props[p]
if isinstance(v, list):
    print("Value is an array with %%d items:\n" %% len(v))
    for x in v:
        print(x)
else:
    print(v)
"""


class Box:
    def __init__(self, tmp_path, running=()):
        self.tmp = tmp_path
        self.home = _home_with_obs(tmp_path)
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        (tmp_path / "xsession.desktop").write_text("[Desktop Entry]\n")
        for name, text in (("pgrep", FAKE_PGREP), ("xfconf-query", FAKE_XFCONF_QUERY), ("xfce4-panel", FAKE_PANEL)):
            f = self.bin / name
            f.write_text(text % {"py": sys.executable, "running": str(tmp_path / "running"),
                                 "state": str(tmp_path / "xfconfd.json"), "log": str(tmp_path / "xfconf-query.log"),
                                 "broken": str(tmp_path / "bus-broken"), "plog": str(tmp_path / "panel.log"),
                                 "revert": str(tmp_path / "revert")})
            f.chmod(0o755)
        self.set_running(*running)

    def set_running(self, *procs):
        (self.tmp / "running").write_text("".join(p + "\n" for p in procs))

    def live(self):
        import json
        f = self.tmp / "xfconfd.json"
        return json.loads(f.read_text()) if f.exists() else {}

    def env(self):
        e = _seams(self.tmp)
        e.update({"OBS_BOX_SESSION_START_BIN": str(self.tmp / "obs-box-session-start"),
                  "OBS_BOX_XFCE_POLKIT_RULE": str(self.tmp / "50-obs-box-power.rules"),
                  "OBS_BOX_XFCE_XSESSION": str(self.tmp / "xsession.desktop"),
                  "OBS_BOX_XFCE_PGREP": str(self.bin / "pgrep"),
                  "STRIH_SESSION_APPS_EUID": "1000",
                  "XDG_RUNTIME_DIR": str(self.tmp / "run"),
                  "PATH": "%s:%s" % (self.bin, os.environ["PATH"])})
        return e

    def install(self, box="stream-snv"):
        # chown of a file to root:root is root's: the test runs as the user, the owner is checked by text
        return _bash('chown() { :; }\nobs_box_session_install "$1" "$2" "$3"', str(REPO), str(self.home), ME,
                     box=box, env=self.env())

    def grade(self, box="stream-snv"):
        return _bash('obs_box_session_grade_rows "$1" "$2" "$3"', str(REPO), str(self.home), ME, box=box, env=self.env())


def _rows(r):
    return {ln.split("|", 1)[1].split(")", 1)[0] + ")": ln.split("|", 1)[0] for ln in r.stdout.splitlines()}


def test_the_xfce_install_writes_every_file_and_a_rerun_writes_nothing(tmp_path):
    box = Box(tmp_path)
    r = box.install()
    assert r.returncode == 0, r.stdout + r.stderr
    start = (tmp_path / "obs-box-session-start").read_text()
    assert start == _bash("strih_session_start_text xfce").stdout
    assert os.access(tmp_path / "obs-box-session-start", os.X_OK)
    adir = box.home / ".config" / "autostart"
    assert "Exec=%s" % (tmp_path / "obs-box-session-start") in (adir / "obs-box-session.desktop").read_text()
    for hidden in ("localsearch-3", "update-notifier", "xfce4-power-manager", "xfce4-screensaver"):
        assert "Hidden=true" in (adir / f"{hidden}.desktop").read_text()
    assert os.readlink(box.home / ".config/systemd/user/localsearch-3.service") == "/dev/null"
    ch = box.home / ".config/xfce4/xfconf/xfce-perchannel-xml"
    assert 'name="use_compositing" type="bool" value="false"' in (ch / "xfwm4.xml").read_text()
    assert 'name="SaveOnExit" type="bool" value="false"' in (ch / "xfce4-session.xml").read_text()
    assert "flameshot gui" in (ch / "xfce4-keyboard-shortcuts.xml").read_text()
    rule = (tmp_path / "50-obs-box-power.rules").read_text()
    assert f'subject.user == "{ME}"' in rule and "subject.local && subject.active &&" in rule
    again = box.install()
    assert again.returncode == 0, again.stderr
    # every user-owned file is unchanged; the two root:root files read as written only because this test
    # does not run as root (their owner can never match)
    written = [ln for ln in again.stdout.splitlines() if ln.endswith(" written")]
    assert all("obs-box-session-start" in ln or "50-obs-box-power.rules" in ln for ln in written), again.stdout
    assert "xfconf xfwm4:" in again.stdout and "unchanged" in again.stdout


def test_the_fake_pgrep_refuses_a_long_process_name_like_procps(tmp_path):
    box = Box(tmp_path, running=("/usr/bin/xfce4-power-manager --daemon",))
    pg = str(box.bin / "pgrep")
    assert subprocess.run([pg, "-u", ME, "-x", "xfce4-power-manager"]).returncode == 1
    assert subprocess.run([pg, "-u", ME, "-f", "(^|/)xfce4-power-manager( |$)"]).returncode == 0


@pytest.mark.parametrize("proc", ["xfce4-power-manager", "xfce4-screensaver", "tracker-miner-fs-3",
                                  "localsearch-3", "update-notifier"])
def test_a_running_hidden_app_fails_the_row_whatever_its_name_length(tmp_path, proc):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    box.set_running("xfce4-session", "/usr/libexec/%s --some-arg" % proc)
    r = box.grade()
    row = [ln for ln in r.stdout.splitlines() if "(xfce-hidden)" in ln][0]
    assert row.startswith("FAIL|") and proc in row, row


def test_a_live_xfconfd_gets_the_properties_through_xfconf_query_and_its_file_is_never_written(tmp_path):
    box = Box(tmp_path, running=("xfconfd",))
    r = box.install()
    assert r.returncode == 0, r.stdout + r.stderr
    assert not (box.home / ".config/xfce4/xfconf/xfce-perchannel-xml/xfwm4.xml").exists()
    live = box.live()
    assert live["xfwm4"]["/general/use_compositing"] == "false"
    assert live["xfce4-session"]["/general/SaveOnExit"] == "false"
    assert live["xfce4-keyboard-shortcuts"]["/commands/custom/override"] == "true"
    assert "applied through" in r.stdout and "read back" in r.stdout
    log = (tmp_path / "xfconf-query.log").read_text()
    assert "unix:path=%s/bus" % (tmp_path / "run") in log    # the user's own session bus
    # the replace runs first: a stale custom key the spec does not declare is gone
    box_state = box.live()
    box_state["xfce4-keyboard-shortcuts"]["/commands/custom/<Super>e"] = "thunar"
    (tmp_path / "xfconfd.json").write_text(__import__("json").dumps(box_state))
    assert box.install().returncode == 0
    assert "/commands/custom/<Super>e" not in box.live()["xfce4-keyboard-shortcuts"]


def test_a_failed_live_apply_fails_setup_by_name(tmp_path):
    box = Box(tmp_path, running=("xfconfd",))
    (tmp_path / "bus-broken").write_text("")
    r = box.install()
    assert r.returncode != 0
    assert "could not be applied through" in r.stderr and "broken bus" in r.stderr, r.stderr


def test_a_file_that_already_holds_every_property_is_left_alone(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    f = box.home / ".config/xfce4/xfconf/xfce-perchannel-xml/xfwm4.xml"
    # xfconfd's own layout: no space before />, version 1.1, extra properties -- the same declared values
    f.write_text(LIVE_XFWM4.replace('"use_compositing" type="bool" value="true"', '"use_compositing" type="bool" value="false"'))
    before = f.read_text()
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert "xfconf xfwm4: %s unchanged" % f in r.stdout
    assert f.read_text() == before


def test_a_live_xfconfd_that_already_holds_the_properties_gets_no_write(tmp_path):
    box = Box(tmp_path, running=("xfconfd",))
    assert box.install().returncode == 0                       # the first run applies them live
    (tmp_path / "xfconf-query.log").unlink()
    r = box.install()
    assert r.returncode == 0, r.stderr
    assert "xfconf xfwm4: unchanged (every declared property holds in" in r.stdout, r.stdout
    log = (tmp_path / "xfconf-query.log").read_text().splitlines()
    assert log and not [ln for ln in log if " -s " in ln or " -r " in ln], log   # reads only


def test_the_graded_xfce_box_passes_only_with_the_session_up_and_the_indexer_down(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    rows = _rows(box.grade())
    assert rows["(session-start)"] == "OK" and rows["(xfce-session-entry)"] == "OK", rows
    assert rows["(xfce-settings)"] == "OK" and rows["(xfce-keys)"] == "OK" and rows["(xfce-polkit)"] == "OK", rows
    assert rows["(xfce-running)"] == "FAIL", rows          # no xfce4-session yet
    box.set_running("xfce4-session", "localsearch-3")
    rows = _rows(box.grade())
    assert rows["(xfce-running)"] == "OK" and rows["(xfce-hidden)"] == "FAIL", rows
    box.set_running("xfce4-session")
    rows = _rows(box.grade())
    assert rows["(xfce-hidden)"] == "OK", rows
    assert len(box.grade().stdout.splitlines()) == 8


def test_under_a_live_xfconfd_verify_grades_the_daemon_not_a_lagging_file(tmp_path):
    box = Box(tmp_path, running=("xfconfd", "xfce4-session"))
    assert box.install().returncode == 0               # applied live: the daemon holds them, no file yet
    rows = _rows(box.grade())
    assert rows["(xfce-settings)"] == "OK" and rows["(xfce-keys)"] == "OK", box.grade().stdout
    st = box.live()
    st["xfwm4"]["/general/use_compositing"] = "true"
    (tmp_path / "xfconfd.json").write_text(__import__("json").dumps(st))
    row = [ln for ln in box.grade().stdout.splitlines() if "(xfce-settings)" in ln][0]
    assert row.startswith("FAIL|") and "running xfconfd" in row, row


def test_a_compositor_switched_back_on_fails_the_settings_row(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    f = box.home / ".config/xfce4/xfconf/xfce-perchannel-xml/xfwm4.xml"
    f.write_text(f.read_text().replace('value="false"', 'value="true"'))
    rows = _rows(box.grade())
    assert rows["(xfce-settings)"] == "FAIL", rows


def test_an_openbox_box_installs_only_the_session_start_and_drops_the_xfce_pieces(tmp_path):
    box = Box(tmp_path)
    adir = box.home / ".config" / "autostart"
    adir.mkdir(parents=True)
    (adir / "obs-box-session.desktop").write_text("stale\n")
    (tmp_path / "50-obs-box-power.rules").write_text("stale\n")
    r = box.install(box="strih-snv")
    assert r.returncode == 0, r.stderr
    assert (tmp_path / "obs-box-session-start").read_text() == _bash("strih_openbox_autostart_text", box="strih-snv").stdout
    assert not (adir / "obs-box-session.desktop").exists()
    assert not (tmp_path / "50-obs-box-power.rules").exists()
    g = box.grade(box="strih-snv")
    assert [ln.split("|")[0] for ln in g.stdout.splitlines()] == ["OK", "NOTE"], g.stdout


def test_an_xfce_box_without_the_xfce_session_file_refuses(tmp_path):
    box = Box(tmp_path)
    (tmp_path / "xsession.desktop").unlink()
    r = box.install()
    assert r.returncode != 0 and "xsession.desktop missing" in r.stderr, r.stdout + r.stderr


# --- the xfce4-panel install (setup step 16h) ---------------------------------------------------------------

def test_the_panel_install_writes_each_launcher_into_its_plugin_folder_and_grades_ok(tmp_path):
    box = Box(tmp_path)
    r = _bash('chown() { :; }\ndpkg-query() { echo "install ok installed"; }\n'
              'obs_box_xfce_panel_install "$1" "$2" "$3"; obs_box_xfce_check "$(obs_box_xfce_channel_path "$2" xfce4-panel)" '
              'obs_box_xfce_panel_spec "$2"; obs_box_xfce_panel_launchers_ok "$2" && echo LAUNCHERS-OK',
              str(REPO), str(box.home), ME, env=box.env())
    assert r.returncode == 0, r.stdout + r.stderr
    assert "ok" in r.stdout.splitlines() and "LAUNCHERS-OK" in r.stdout, r.stdout
    f = box.home / ".config/xfce4/panel/launcher-2/obs-box-panel-power.desktop"
    assert "Name=Vypnúť" in f.read_text()


def _panel_install(box):
    return _bash('chown() { :; }\ndpkg-query() { echo "install ok installed"; }\nobs_box_xfce_panel_install "$1" "$2" "$3"',
                 str(REPO), str(box.home), ME, env=box.env())


def test_a_live_panel_layout_change_restarts_the_panel_on_the_display_and_reads_the_layout_back(tmp_path):
    box = Box(tmp_path, running=("xfconfd", "xfce4-panel"))
    r = _panel_install(box)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "the declared layout read back" in r.stdout, r.stdout
    plog = (tmp_path / "panel.log").read_text().split()
    assert plog[0] == ":0" and plog[1] == "unix:path=%s/bus" % (tmp_path / "run") and plog[2] == "--restart"
    # nothing changed on a re-run: no apply, no restart
    (tmp_path / "panel.log").unlink()
    again = _panel_install(box)
    assert again.returncode == 0 and not (tmp_path / "panel.log").exists(), again.stdout


def test_a_panel_that_saves_its_old_layout_back_fails_the_step(tmp_path):
    box = Box(tmp_path, running=("xfconfd", "xfce4-panel"))
    (tmp_path / "revert").write_text("")
    r = _panel_install(box)
    assert r.returncode != 0 and "saved its old layout back" in r.stderr, r.stdout + r.stderr


# the verify item-44 rows of an xfce box: the shared-row helpers (discord / kbd / files / power, each with
# its own tests) are stand-ins here; systemctl, dpkg-query and wmctrl are the box's answers.
PANEL_ROWS_STUBS = '''chown() { :; }
dpkg-query() { echo "install ok installed"; }
systemctl() { if [ "$2" = is-enabled ]; then cat "$TINT2_STATE"; else :; fi; }
strih_session_apps_wmctrl() { printf '0x01 0 xfce4-panel.Xfce4-panel host xfce4-panel\\n'; }
obs_box_panel_discord_row() { echo 'NOTE|(panel-discord) stub'; }
obs_box_panel_kbd_row() { echo 'OK|(panel-kbd) stub'; }
obs_box_panel_files_row() { echo 'OK|(panel-files) stub'; }
obs_box_panel_power_row() { echo 'OK|(panel-power) stub'; }
'''


def _panel_rows(box, tint2):
    (box.tmp / "tint2").write_text(tint2 + "\n")
    env = dict(box.env(), TINT2_STATE=str(box.tmp / "tint2"))
    return _bash(PANEL_ROWS_STUBS + 'obs_box_xfce_panel_install "$1" "$2" "$3" >/dev/null\n'
                 'shot="$2/.config/flameshot"; mkdir -p "$shot"; obs_box_panel_shot_ini_merge "$2" < /dev/null > "$shot/flameshot.ini"\n'
                 'obs_box_panel_grade_rows "$1" "$2" "$3"', str(REPO), str(box.home), ME, env=env)


def test_the_xfce_panel_rows_are_ten_and_grade_xfce4_panel(tmp_path):
    box = Box(tmp_path, running=("xfce4-panel",))
    r = _panel_rows(box, "disabled")
    rows = r.stdout.splitlines()
    assert r.returncode == 0 and len(rows) == 10, r.stdout + r.stderr
    got = _rows(r)
    # (panel-launchers) is the shared row over the applications dir, written by the tint2 half of step 16h
    for k in ("(panel-pkg)", "(panel-config)", "(panel-unit)", "(panel-shot)", "(panel-window)"):
        assert got[k] == "OK", (k, r.stdout)
    assert "xfce4-panel" in [ln for ln in rows if "(panel-pkg)" in ln][0]


def test_an_enabled_tint2_or_a_missing_xfce4_panel_fails_the_unit_row(tmp_path):
    box = Box(tmp_path, running=("xfce4-panel",))
    assert _rows(_panel_rows(box, "enabled"))["(panel-unit)"] == "FAIL"
    box.set_running()
    assert _rows(_panel_rows(box, "disabled"))["(panel-unit)"] == "FAIL"


def test_the_session_report_prints_every_row_on_both_desktops(tmp_path):
    box = Box(tmp_path)
    assert box.install().returncode == 0
    body = ('ok() { echo "OK $*"; }\nbad() { echo "BAD $*"; }\nnote() { echo "NOTE $*"; }\n'
            'obs_box_session_grade_report "$1" "$2" "$3"; echo "RC=$?"')
    r = _bash(body, str(REPO), str(box.home), ME, env=box.env())
    assert "RC=0" in r.stdout and len([ln for ln in r.stdout.splitlines() if ln[:2] in ("OK", "BA", "NO")]) == 8, r.stdout
    assert box.install(box="strih-snv").returncode == 0
    r = _bash(body, str(REPO), str(box.home), ME, box="strih-snv", env=box.env())
    assert "RC=0" in r.stdout and len(r.stdout.splitlines()) == 3, r.stdout


# --- the wiring ---------------------------------------------------------------------------------------------

def test_setup_and_verify_wire_the_session_in_one_line_each():
    s = SETUP.read_text()
    assert s.count('obs_box_session_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"') == 1
    assert 'obs_box_kiosk "$DESKTOP_USER" strih keep-bluetooth "$(strih_box_desktop)"' in s
    assert s.find("strih_openbox_autostart_text >") < s.find("obs_box_session_install ")
    v = VERIFY.read_text()
    assert v.count('obs_box_session_grade_report "${HERE}/.." "$USER_HOME"') == 1
    assert 'strih-obs.service "$(strih_box_desktop)")' in v


def test_the_baseline_grader_reads_the_autologin_session_the_fact_names(tmp_path):
    v = SCRIPTS / "lib" / "obs-box-baseline-verify.sh"
    r = _bash('. "$2"; obs_box_baseline_gather_snippet strih "$1" strih-obs.service xfce > "$3"; head -4 "$3"; '
              'obs_box_baseline_gather_snippet strih "$1" strih-obs.service gnome || echo REFUSED',
              ME, str(v), str(tmp_path / "g"))
    assert "SESSION=xfce" in r.stdout and "REFUSED" in r.stdout, r.stdout + r.stderr
    assert 'grep -qxF "autologin-session=${SESSION}"' in (tmp_path / "g").read_text()
    r = _bash('. "$2"; obs_box_baseline_gather_snippet strih "$1" strih-obs.service > "$3"; head -4 "$3"',
              ME, str(v), str(tmp_path / "g"))
    assert "SESSION=openbox" in r.stdout
