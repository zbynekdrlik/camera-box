"""issue 1418: the strih OPERATOR surface -- applied by setup-strih, graded by verify-strih.

Owner, 12.10.2026, 30 min before the SNV event: the strih-snv notebook's X server stopped answering
(OBS kept rendering, the panel froze), the Bluetooth mouse could not pair, the touchpad did not click,
clicking the Multiview did not cut, the Stream Deck was stuck. The notebook had been declared
production-ready by verify-strih, which graded services, OBS, genlock and NDI, but nothing the
operator touches. ROZHODNUTE 6126870183: every hand fix on a notebook lives in setup + verify.

scripts/lib/strih-operator.sh (design 6127043932, approach 1):
  * the setup half (setup-strih step 16g): [BasicWindow] SaveProjectors / ProjectorAlwaysOnTop /
    MultiviewMouseSwitch seeded into the OBS user.ini (never while OBS runs: OBS rewrites it on exit),
    and every OTHER wired NetworkManager profile on the rig subnet taken off it (autoconnect no + down);
  * the grade half (verify-strih item 43): one row per operator item.
"""

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-operator.sh"
NIC_LIB = SCRIPTS / "lib" / "strih-nic-driver.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
ME = os.environ.get("USER") or "nobody"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_BOXES_DIR", "OBS_FLEET",
         "STRIH_NIC_IFACE")


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


def _bash(body, env=None, box="strih-lx", args=()):
    """Run BODY under the callers' strict mode with the box loaded and the libs sourced."""
    script = ('set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\n. "$4"\n. "$5"\nshift 5\n' + body)
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", str(FACTS), box, str(PROVISION), str(NIC_LIB),
                           str(LIB), *args], capture_output=True, text=True, env=e, timeout=120)


# --- setup half: the OBS user.ini seed ---------------------------------------------------------------

def _obs(tmp_path, running):
    return _exec(tmp_path / "fake-obs-running", "#!/bin/bash\nexit %d\n" % (0 if running else 1))


def _seed(tmp_path, ini, running=False):
    return _bash('strih_operator_user_ini_seed "$1" "$2"', args=(str(ini), ME),
                 env={"STRIH_OPERATOR_OBS_RUNNING_CMD": str(_obs(tmp_path, running))})


def _ini(path):
    import configparser
    cp = configparser.RawConfigParser(strict=False)
    cp.optionxform = str
    cp.read(path)
    return cp


def test_the_seed_writes_the_three_basicwindow_keys_into_a_fresh_user_ini(tmp_path):
    ini = tmp_path / "user.ini"
    r = _seed(tmp_path, ini)
    assert r.returncode == 0, r.stderr
    cp = _ini(ini)
    assert cp.get("BasicWindow", "MultiviewMouseSwitch") == "true"
    assert cp.get("BasicWindow", "SaveProjectors") == "true"
    assert cp.get("BasicWindow", "ProjectorAlwaysOnTop") == "false"
    assert "MultiviewMouseSwitch=true" in r.stdout


def test_the_seed_turns_a_false_multiview_click_back_on_and_keeps_every_other_key(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("[General]\nFirstRun=false\n\n[BasicWindow]\nMultiviewMouseSwitch=false\n"
                   "PreviewEnabled=true\n\n[NDIPlugin]\nMainOutputName=2ME PGM\n")
    r = _seed(tmp_path, ini)
    assert r.returncode == 0, r.stderr
    cp = _ini(ini)
    assert cp.get("BasicWindow", "MultiviewMouseSwitch") == "true"
    assert cp.get("BasicWindow", "PreviewEnabled") == "true"
    assert cp.get("General", "FirstRun") == "false"
    assert cp.get("NDIPlugin", "MainOutputName") == "2ME PGM"
    assert ini.read_text().count("[BasicWindow]") == 1


def test_the_seed_never_writes_while_obs_runs(tmp_path):
    """OBS rewrites user.ini from memory on exit: a key written under a running OBS is lost."""
    ini = tmp_path / "user.ini"
    ini.write_text("[BasicWindow]\nMultiviewMouseSwitch=false\n")
    r = _seed(tmp_path, ini, running=True)
    assert r.returncode == 0, r.stderr
    assert ini.read_text() == "[BasicWindow]\nMultiviewMouseSwitch=false\n"
    assert "WARN" in r.stdout and "OBS is running" in r.stdout


def test_the_seed_never_clobbers_an_unparseable_user_ini(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("no section header here\n")
    r = _seed(tmp_path, ini)
    assert r.returncode == 0, r.stderr
    assert ini.read_text() == "no section header here\n"
    assert "WARN" in r.stdout


# --- setup half: the second NIC off the rig subnet --------------------------------------------------

RIG = "2: enp7s0    inet 10.77.9.202/23 brd 10.77.9.255 scope global noprefixroute enp7s0\\       valid_lft forever preferred_lft forever"
LO = "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever"
ONBOARD = "3: enp2s0    inet 10.77.9.192/23 brd 10.77.9.255 scope global dynamic noprefixroute enp2s0\\       valid_lft 86000sec preferred_lft 86000sec"
WIFI = "5: wlp4s0    inet 10.77.9.180/23 brd 10.77.9.255 scope global dynamic wlp4s0\\       valid_lft 3600sec preferred_lft 3600sec"
STRAY = "6: enx1    inet 10.77.8.9/23 brd 10.77.9.255 scope global enx1\\       valid_lft forever preferred_lft forever"

_FAKE_NMCLI = r"""#!/bin/bash
S="$FAKE_NM"
echo "nmcli $*" >> "$S/calls"
if [ "$1 $2 $3" = "-g UUID,TYPE,DEVICE connection" ]; then
  for d in "$S"/conn/*; do
    u="${d##*/}"
    printf '%s:%s:%s\n' "$u" "$(cat "$d/type")" "$(cat "$d/device" 2>/dev/null || true)"
  done
  exit 0
fi
if [ "$1" = -g ] && [ "$3 $4" = "connection show" ]; then
  cat "$S/conn/$5/$2" 2>/dev/null || true
  exit 0
fi
if [ "$1 $2" = "connection modify" ]; then
  [ -e "$S/modify-fails" ] && { echo "Error: failed to modify" >&2; exit 4; }
  printf '%s' "$5" > "$S/conn/$3/$4"
  exit 0
fi
if [ "$1 $2" = "connection down" ]; then
  : > "$S/conn/$3/device"
  exit 0
fi
echo "fake nmcli: unexpected $*" >&2
exit 2
"""


class Nm:
    """A fake NetworkManager: one directory per connection (type, device, property files)."""

    def __init__(self, tmp_path):
        self.root = tmp_path / "nm"
        (self.root / "conn").mkdir(parents=True)
        self.nmcli = _exec(tmp_path / "fake-nmcli", _FAKE_NMCLI)
        self.addrs = tmp_path / "ipaddr"

    def add(self, uuid, ctype, device="", name=None, ifname="", addresses="", autoconnect="yes"):
        d = self.root / "conn" / uuid
        d.mkdir()
        (d / "type").write_text(ctype)
        (d / "device").write_text(device)
        (d / "connection.id").write_text(name or uuid)
        (d / "connection.interface-name").write_text(ifname)
        (d / "ipv4.addresses").write_text(addresses)
        (d / "connection.autoconnect").write_text(autoconnect)

    def prop(self, uuid, name):
        return (self.root / "conn" / uuid / name).read_text()

    def calls(self):
        p = self.root / "calls"
        return [ln for ln in (p.read_text().splitlines() if p.exists() else [])
                if " connection modify " in " %s " % ln or " connection down " in " %s " % ln]

    def run(self, addr_lines, nic="enp7s0"):
        self.addrs.write_text("\n".join(addr_lines) + "\n")
        return _bash("strih_operator_nic_isolate",
                     env={"FAKE_NM": str(self.root), "STRIH_OPERATOR_NMCLI": str(self.nmcli),
                          "STRIH_OPERATOR_IP_ADDR_CMD": "cat %s" % self.addrs, "STRIH_NIC_IFACE": nic})


def _live_box(tmp_path):
    """The 11.10.2026 strih notebook: the onboard DHCP profile up on the rig /23 next to the rig NIC."""
    nm = Nm(tmp_path)
    nm.add("u-rig", "802-3-ethernet", "enp7s0", "rig", "enp7s0", "10.77.9.202/23")
    nm.add("u-onb", "802-3-ethernet", "enp2s0", "Wired connection 1")
    nm.add("u-old", "802-3-ethernet", "", "old 2.5G adapter", "enx6c1ff766154b", "10.77.9.203/23")
    nm.add("u-home", "802-3-ethernet", "", "home", "", "192.168.1.5/24")
    nm.add("u-wifi", "802-11-wireless", "wlp4s0", "venue wifi")
    return nm


def test_isolate_takes_the_onboard_profile_off_the_rig_subnet_and_leaves_the_rig_nic(tmp_path):
    nm = _live_box(tmp_path)
    r = nm.run([LO, RIG, ONBOARD, WIFI])
    assert r.returncode == 0, r.stderr
    calls = nm.calls()
    assert "nmcli connection modify u-onb connection.autoconnect no" in calls
    assert "nmcli connection down u-onb" in calls
    # an inactive wired profile with a rig-subnet address could come up later: autoconnect off, no down
    assert "nmcli connection modify u-old connection.autoconnect no" in calls
    assert "nmcli connection down u-old" not in calls
    # never the rig NIC's own profile, a profile off the rig subnet, or a non-wired one
    assert not [c for c in calls if "u-rig" in c or "u-home" in c or "u-wifi" in c], calls
    assert nm.prop("u-onb", "connection.autoconnect") == "no"
    assert "Wired connection 1" in r.stdout and "enp2s0" in r.stdout


def test_isolate_is_idempotent(tmp_path):
    nm = _live_box(tmp_path)
    assert nm.run([LO, RIG, ONBOARD]).returncode == 0
    (nm.root / "calls").unlink()
    r = nm.run([LO, RIG])
    assert r.returncode == 0, r.stderr
    assert nm.calls() == []
    assert "unchanged" in r.stdout


def test_isolate_touches_nothing_while_the_rig_nic_has_no_rig_address(tmp_path):
    nm = _live_box(tmp_path)
    r = nm.run([LO, ONBOARD])
    assert r.returncode == 0, r.stderr
    assert nm.calls() == []
    assert "WARN" in r.stdout and "10.77.9.202" in r.stdout


def test_isolate_fails_loud_when_networkmanager_refuses(tmp_path):
    nm = _live_box(tmp_path)
    (nm.root / "modify-fails").write_text("")
    r = nm.run([LO, RIG, ONBOARD])
    assert r.returncode == 1
    assert "could not set connection.autoconnect no" in r.stderr, r.stderr


def test_isolate_names_a_rig_subnet_address_networkmanager_does_not_manage(tmp_path):
    nm = _live_box(tmp_path)
    r = nm.run([LO, RIG, STRAY])
    assert r.returncode == 0, r.stderr
    assert "WARN" in r.stdout and "enx1" in r.stdout


def test_the_rig_subnet_others_are_the_single_homed_verdicts_own_set():
    """One subnet rule for the setup half and verify item 36 (strih-nic-driver.sh), never a copy."""
    text = "\n".join([LO, RIG, ONBOARD, WIFI, STRAY])
    r = _bash('strih_nic_rig_subnet_others enp7s0 10.77.9.202 "$1"', args=(text,))
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["enp2s0 10.77.9.192", "wlp4s0 10.77.9.180", "enx1 10.77.8.9"]
    v = _bash('strih_nic_single_homed_verdict enp7s0 10.77.9.202 "$1" || true', args=(text,))
    assert "enp2s0 10.77.9.192, wlp4s0 10.77.9.180, enx1 10.77.8.9 also on the rig subnet" in v.stdout
    lib = LIB.read_text()
    assert "strih_nic_rig_subnet_others" in lib and "0xFFFFFFFF" not in lib


def test_install_runs_the_seed_and_the_isolation(tmp_path):
    nm = _live_box(tmp_path)
    nm.addrs.write_text("\n".join([LO, RIG, ONBOARD]) + "\n")
    ini = tmp_path / "user.ini"
    r = _bash('strih_operator_install "$1" "$2"', args=(str(ini), ME),
              env={"FAKE_NM": str(nm.root), "STRIH_OPERATOR_NMCLI": str(nm.nmcli),
                   "STRIH_OPERATOR_IP_ADDR_CMD": "cat %s" % nm.addrs, "STRIH_NIC_IFACE": "enp7s0",
                   "STRIH_OPERATOR_OBS_RUNNING_CMD": str(_obs(tmp_path, False))})
    assert r.returncode == 0, r.stderr
    assert _ini(ini).get("BasicWindow", "MultiviewMouseSwitch") == "true"
    assert "nmcli connection down u-onb" in nm.calls()


# --- grade half: verify-strih item 43 ----------------------------------------------------------------

import http.server  # noqa: E402
import json  # noqa: E402
import threading  # noqa: E402

import pytest  # noqa: E402

XLIST = "\n".join([
    "⎡ Virtual core pointer                    \tid=2\t[master pointer  (3)]",
    "⎜   ↳ Virtual core XTEST pointer              \tid=4\t[slave  pointer  (2)]",
    "⎜   ↳ ELAN0791:00 04F3:30FD Touchpad          \tid=12\t[slave  pointer  (2)]",
    "⎜   ↳ Logitech MX Anywhere 2S                 \tid=15\t[slave  pointer  (2)]",
    "⎣ Virtual core keyboard                   \tid=3\t[master keyboard (2)]",
    "    ↳ Virtual core XTEST keyboard             \tid=5\t[slave  keyboard (3)]",
    "    ↳ Power Button                            \tid=6\t[slave  keyboard (3)]",
    "    ↳ Video Bus                               \tid=7\t[slave  keyboard (3)]",
    "    ↳ ITE Tech. Inc. ITE Device(8910) Keyboard\tid=10\t[slave  keyboard (3)]",
    "    ↳ ITE Tech. Inc. ITE Device(8910) Consumer Control\tid=11\t[slave  keyboard (3)]",
    "∼ Some floating device                    \tid=20\t[floating slave]",
]) + "\n"


def _props(name, tapping):
    lines = ["Device '%s':" % name, "\tDevice Enabled (165):\t1"]
    if tapping is not None:
        lines += ["\tlibinput Tapping Enabled (318):\t%s" % tapping, "\tlibinput Tapping Enabled Default (319):\t0"]
    lines.append("\tlibinput Accel Speed (300):\t0.000000")
    return "\n".join(lines) + "\n"


HCI_DEAD = ("hci0:\tType: Primary  Bus: USB\n\tBD Address: 00:00:00:00:00:00  ACL MTU: 0:0  SCO MTU: 0:0\n"
            "\tDOWN \n\tRX bytes:0 acl:0 sco:0 events:0 errors:0\n")
HCI_OK = ("hci0:\tType: Primary  Bus: USB\n\tBD Address: 5C:B4:7E:11:22:33  ACL MTU: 1021:6  SCO MTU: 255:12\n"
          "\tUP RUNNING PSCAN \n\tRX bytes:1234 acl:0 sco:0 events:80 errors:0\n")

_FAKE_XINPUT = r"""#!/bin/bash
echo "xinput $* DISPLAY=${DISPLAY-} XAUTHORITY=${XAUTHORITY-}" >> "$FAKE_OP/calls"
if [ "$1 $2" = "list --short" ]; then cat "$FAKE_OP/xlist" 2>/dev/null || true; exit 0; fi
if [ "$1" = list-props ]; then cat "$FAKE_OP/props-$2" 2>/dev/null || exit 1; exit 0; fi
exit 2
"""
_FAKE_CAT = '#!/bin/bash\ncat "$FAKE_OP/%s" 2>/dev/null || true\n'
_FAKE_GRAB = r"""#!/bin/bash
n=$(cat "$FAKE_OP/grab-n" 2>/dev/null || echo 0)
echo $((n + 1)) > "$FAKE_OP/grab-n"
sed -n "$((n + 1))p" "$FAKE_OP/grabs" 2>/dev/null || true
"""
_FAKE_DPKG_OP = r"""#!/bin/bash
grep -qxF "${@: -1}" "$FAKE_OP/installed" 2>/dev/null && printf 'install ok installed' || true
"""
_FAKE_SUDO = '#!/bin/bash\necho "sudo $*" >> "$FAKE_OP/calls"\n[ "$1" = -u ] && shift 2\nexec env "$@"\n'


class _Surfaces(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 -- http.server API
        if self.path != "/api/surfaces":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(self.server.body)

    def log_message(self, *args):
        return


class Op:
    """A fake operator surface: X input devices, hci0, the deck on USB + the Satellite, OBS, the panel."""

    def __init__(self, tmp_path):
        self.tmp = tmp_path
        self.st = tmp_path / "op"
        self.st.mkdir()
        self.bin = tmp_path / "opbin"
        self.bin.mkdir()
        _exec(self.bin / "dpkg-query", _FAKE_DPKG_OP)
        _exec(self.bin / "sudo", _FAKE_SUDO)
        self.xinput = _exec(tmp_path / "fake-xinput", _FAKE_XINPUT)
        self.hci = _exec(tmp_path / "fake-hciconfig", _FAKE_CAT % "hci")
        self.studio = _exec(tmp_path / "fake-studio", _FAKE_CAT % "studio")
        self.grab = _exec(tmp_path / "fake-grab", _FAKE_GRAB)
        self.obs_up = _exec(tmp_path / "fake-obs-up", '#!/bin/bash\n[ -e "$FAKE_OP/obs-up" ]\n')
        self.sys = tmp_path / "sys"
        (self.sys / "bus" / "usb" / "devices").mkdir(parents=True)
        self.home = tmp_path / "home"
        (self.home / ".config" / "obs-studio" / "plugin_config" / "obs-websocket").mkdir(parents=True)
        self.libdir = tmp_path / "lib"
        self.server = None
        self.rest = "http://127.0.0.1:9"  # nothing answers there
        self.euid = "1000"

    def healthy(self):
        (self.st / "xlist").write_text(XLIST)
        (self.st / "props-12").write_text(_props("ELAN0791:00 04F3:30FD Touchpad", 1))
        (self.st / "props-15").write_text(_props("Logitech MX Anywhere 2S", None))
        (self.st / "hci").write_text(HCI_OK)
        self.deck(True)
        self.serve([{"surfaceId": "streamdeck:CL123", "productName": "Stream Deck XL"}])
        (self.home / ".config" / "obs-studio" / "user.ini").write_text(
            "[BasicWindow]\nSaveProjectors=true\nMultiviewMouseSwitch=true\n")
        self.ws({"server_enabled": True, "server_port": 4455, "auth_required": False})
        (self.st / "studio").write_text("true\n")
        (self.st / "installed").write_text("qt6-svg-plugins\n")
        for sub in ("iconengines/libqsvgicon.so", "imageformats/libqsvg.so"):
            f = self.libdir / "qt6" / "plugins" / sub
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("so")
        (self.st / "obs-up").write_text("")
        (self.st / "grabs").write_text("MD5=aaaa\nMD5=bbbb\n")

    def deck(self, present):
        d = self.sys / "bus" / "usb" / "devices" / "3-1"
        if present:
            d.mkdir(exist_ok=True)
            (d / "idVendor").write_text("0fd9\n")
            (d / "idProduct").write_text("008f\n")
        elif d.exists():
            for f in d.iterdir():
                f.unlink()
            d.rmdir()

    def ws(self, cfg):
        p = self.home / ".config" / "obs-studio" / "plugin_config" / "obs-websocket" / "config.json"
        p.write_text(json.dumps(cfg))

    def serve(self, surfaces):
        self.close()
        srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Surfaces)
        srv.body = json.dumps(surfaces).encode()
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.server = srv
        self.rest = "http://127.0.0.1:%d" % srv.server_address[1]

    def close(self):
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()
            self.server = None

    def env(self):
        return {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "FAKE_OP": str(self.st),
                "STRIH_OPERATOR_EUID": self.euid, "STRIH_OPERATOR_XINPUT": str(self.xinput),
                "STRIH_OPERATOR_HCICONFIG": str(self.hci), "STRIH_OPERATOR_SYSFS": str(self.sys),
                "COMPANION_SATELLITE_REST_URL": self.rest, "STRIH_OPERATOR_STUDIO_CMD": str(self.studio),
                "STRIH_OPERATOR_GRAB_CMD": str(self.grab), "STRIH_OPERATOR_GRAB_GAP_S": "0",
                "STRIH_OPERATOR_OBS_RUNNING_CMD": str(self.obs_up), "STRIH_OPERATOR_LIBDIR": str(self.libdir)}

    def rows(self, box="strih-lx"):
        r = _bash('strih_operator_grade_rows "$1" "$2" "$3"', env=self.env(), box=box,
                  args=(str(REPO), str(self.home), "operator1"))
        assert r.returncode == 0, r.stderr
        return [tuple(ln.split("|", 1)) for ln in r.stdout.splitlines() if ln]

    def row(self, item, box="strih-lx"):
        rows = self.rows(box)
        hits = [(st, d) for st, d in rows if d.startswith("(%s)" % item)]
        assert len(hits) == 1, (item, rows)
        return hits[0]

    def calls(self):
        p = self.st / "calls"
        return p.read_text() if p.exists() else ""


@pytest.fixture
def op(tmp_path):
    o = Op(tmp_path)
    o.healthy()
    yield o
    o.close()


ITEMS = ["tap-to-click", "bluetooth", "stream-deck", "mv-click", "studio-mode", "ws-seed", "qt6-svg",
         "operator-input", "operator-panel"]


def test_a_healthy_operator_surface_passes_every_row(op):
    rows = op.rows()
    assert [d.split(")", 1)[0][1:] for _, d in rows] == ITEMS
    assert [st for st, _ in rows] == ["OK"] * 9, rows
    assert _bash("strih_operator_row_count").stdout == "9"


def test_tap_to_click_off_on_the_live_touchpad_fails_naming_it(op):
    """12.10.2026: the xorg.conf.d file was there, the live libinput value was 0."""
    (op.st / "props-12").write_text(_props("ELAN0791:00 04F3:30FD Touchpad", 0))
    st, d = op.row("tap-to-click")
    assert st == "FAIL" and "ELAN0791:00 04F3:30FD Touchpad" in d and "Tapping Enabled=0" in d, d


def test_no_touchpad_is_a_note(op):
    (op.st / "xlist").write_text("\n".join(ln for ln in XLIST.splitlines() if "Touchpad" not in ln) + "\n")
    assert op.row("tap-to-click")[0] == "NOTE"


def test_a_touchpad_not_driven_by_libinput_fails(op):
    (op.st / "props-12").write_text(_props("ELAN0791:00 04F3:30FD Touchpad", None))
    st, d = op.row("tap-to-click")
    assert st == "FAIL" and "libinput" in d, d


def test_x_not_answering_fails_the_input_rows(op):
    (op.st / "xlist").write_text("")
    assert op.row("tap-to-click")[0] == "FAIL"
    st, d = op.row("operator-input")
    assert st == "FAIL" and "xinput" in d, d


def test_the_x_reads_run_on_display_0_with_the_operators_xauthority(op):
    op.rows()
    assert "DISPLAY=:0 XAUTHORITY=%s/.Xauthority" % op.home in op.calls()


def test_as_root_the_x_reads_run_as_the_operator(op):
    op.euid = "0"
    op.rows()
    assert "sudo -u operator1 env DISPLAY=:0 XAUTHORITY=%s/.Xauthority" % op.home in op.calls()


def test_the_12_10_dead_bluetooth_adapter_fails_naming_down_and_the_zero_address(op):
    (op.st / "hci").write_text(HCI_DEAD)
    st, d = op.row("bluetooth")
    assert st == "FAIL" and "DOWN" in d and "00:00:00:00:00:00" in d, d


def test_no_bluetooth_controller_fails(op):
    (op.st / "hci").write_text("")
    st, d = op.row("bluetooth")
    assert st == "FAIL" and "hci0" in d, d


def test_bluetooth_is_graded_because_setup_strih_keeps_it():
    """The BT row grades a box whose kiosk keeps bluetooth; setup-strih passes keep-bluetooth."""
    assert 'obs_box_kiosk "$DESKTOP_USER" strih keep-bluetooth' in SETUP.read_text()
    assert _bash('printf %s "$STRIH_OPERATOR_KIOSK_BLUETOOTH"').stdout == "keep-bluetooth"


def test_no_stream_deck_on_usb_is_a_note(op):
    op.deck(False)
    st, d = op.row("stream-deck")
    assert st == "NOTE" and "0fd9:008f" in d, d


def test_a_deck_on_usb_with_no_satellite_surface_fails(op):
    """12.10.2026: the deck was stuck until the owner replugged it."""
    op.serve([])
    st, d = op.row("stream-deck")
    assert st == "FAIL" and "no surface" in d, d


def test_a_silent_satellite_rest_fails(op):
    op.close()
    op.rest = "http://127.0.0.1:9"
    st, d = op.row("stream-deck")
    assert st == "FAIL" and "/api/surfaces" in d, d


def test_the_deck_row_is_a_note_on_the_stream_role(op):
    st, d = op.row("stream-deck", box="stream-pp")
    assert st == "NOTE" and "Stream Deck" in d, d


def test_the_deck_read_reuses_the_satellite_watch():
    lib = LIB.read_text()
    assert "strih_satellite_watch" in lib and "STREAM_DECK_USB_IDS" in lib and "usb_ids_present" not in lib


def test_multiview_click_off_or_absent_fails(op):
    ini = op.home / ".config" / "obs-studio" / "user.ini"
    ini.write_text("[BasicWindow]\nMultiviewMouseSwitch=false\n")
    st, d = op.row("mv-click")
    assert st == "FAIL" and "MultiviewMouseSwitch" in d, d
    ini.write_text("[BasicWindow]\nSaveProjectors=true\n")
    assert op.row("mv-click")[0] == "FAIL"


def test_studio_mode_off_fails_and_obs_down_is_a_note(op):
    (op.st / "studio").write_text("false\n")
    assert op.row("studio-mode")[0] == "FAIL"
    (op.st / "studio").write_text("")
    assert op.row("studio-mode")[0] == "FAIL"
    (op.st / "obs-up").unlink()
    assert op.row("studio-mode")[0] == "NOTE"


def test_the_websocket_seed_must_be_the_no_auth_4455_server(op):
    op.ws({"server_enabled": True, "server_port": 4455, "auth_required": True})
    st, d = op.row("ws-seed")
    assert st == "FAIL" and "auth_required" in d, d
    (op.home / ".config" / "obs-studio" / "plugin_config" / "obs-websocket" / "config.json").unlink()
    assert op.row("ws-seed")[0] == "FAIL"


def test_the_qt6_svg_package_and_both_plugins_are_required(op):
    (op.libdir / "qt6" / "plugins" / "imageformats" / "libqsvg.so").unlink()
    st, d = op.row("qt6-svg")
    assert st == "FAIL" and "libqsvg.so" in d, d
    (op.st / "installed").write_text("")
    assert op.row("qt6-svg")[0] == "FAIL"


def test_no_real_keyboard_in_x_fails(op):
    keep = [ln for ln in XLIST.splitlines() if "ITE Device(8910) Keyboard" not in ln]
    (op.st / "xlist").write_text("\n".join(keep) + "\n")
    st, d = op.row("operator-input")
    assert st == "FAIL" and "keyboard" in d, d


def test_no_real_pointer_in_x_fails(op):
    keep = [ln for ln in XLIST.splitlines() if "Touchpad" not in ln and "MX Anywhere" not in ln]
    (op.st / "xlist").write_text("\n".join(keep) + "\n")
    st, d = op.row("operator-input")
    assert st == "FAIL" and "pointer" in d, d


def test_a_frozen_panel_while_obs_runs_fails(op):
    """12.10.2026: the X main thread stopped answering, the picture froze while OBS rendered."""
    (op.st / "grabs").write_text("MD5=aaaa\nMD5=aaaa\n")
    st, d = op.row("operator-panel")
    assert st == "FAIL" and "identical" in d, d


def test_a_grab_that_does_not_answer_fails(op):
    (op.st / "grabs").write_text("")
    st, d = op.row("operator-panel")
    assert st == "FAIL" and "x11grab" in d, d


def test_the_panel_is_a_note_while_obs_is_down(op):
    (op.st / "obs-up").unlink()
    assert op.row("operator-panel")[0] == "NOTE"


def test_the_report_prints_every_row_through_the_callers_functions(op):
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_operator_grade_report "$1" "$2" "$3"; echo "rc=$?"')
    r = _bash(body, env=op.env(), args=(str(REPO), str(op.home), "operator1"))
    assert r.returncode == 0, r.stderr
    assert [ln.split()[0] for ln in r.stdout.splitlines()] == ["PASS"] * 9 + ["rc=0"]
