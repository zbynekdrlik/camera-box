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


def _bash(body, env=None, box="strih-snv", args=()):
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


def test_the_seed_refuses_a_symlinked_user_ini(tmp_path):
    """setup-strih runs as root: a user.ini symlinked elsewhere must never be written or chowned."""
    target = tmp_path / "elsewhere.ini"
    target.write_text("[X]\nk=v\n")
    ini = tmp_path / "user.ini"
    ini.symlink_to(target)
    r = _seed(tmp_path, ini)
    assert r.returncode == 0, r.stderr
    assert target.read_text() == "[X]\nk=v\n" and ini.is_symlink()
    assert "WARN" in r.stdout and "symlink" in r.stdout


def test_the_seed_replaces_the_file_atomically_keeping_its_mode(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("[BasicWindow]\nMultiviewMouseSwitch=false\n")
    ini.chmod(0o640)
    before = ini.stat().st_ino
    assert _seed(tmp_path, ini).returncode == 0
    assert ini.stat().st_ino != before, "written to a temp file and renamed over the name"
    assert oct(ini.stat().st_mode & 0o777) == oct(0o640)
    assert not [p for p in tmp_path.iterdir() if p.name.startswith(".user.ini")], "no temp file left"


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

    def add(self, uuid, ctype, device="", name=None, ifname="", addresses="", autoconnect="yes", method=None):
        d = self.root / "conn" / uuid
        d.mkdir()
        (d / "type").write_text(ctype)
        (d / "device").write_text(device)
        (d / "connection.id").write_text(name or uuid)
        (d / "connection.interface-name").write_text(ifname)
        (d / "ipv4.addresses").write_text(addresses)
        (d / "connection.autoconnect").write_text(autoconnect)
        (d / "ipv4.method").write_text(method or ("manual" if addresses else "auto"))

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


def test_isolate_never_touches_the_rig_profile_bound_by_its_active_device_alone(tmp_path):
    """A rig profile bound by MAC (no interface-name) is protected by its active device only."""
    nm = Nm(tmp_path)
    nm.add("u-rig", "802-3-ethernet", "enp7s0", "rig", "", "10.77.9.202/23")
    nm.add("u-onb", "802-3-ethernet", "enp2s0", "Wired connection 1")
    r = nm.run([LO, RIG, ONBOARD])
    assert r.returncode == 0, r.stderr
    assert not [c for c in nm.calls() if "u-rig" in c], nm.calls()
    assert "nmcli connection down u-onb" in nm.calls()


def test_isolate_never_touches_an_inactive_profile_pinned_to_the_rig_nic(tmp_path):
    """A second profile pinned to the rig NIC (a spare, a backup) is the rig NIC's, never another's."""
    nm = Nm(tmp_path)
    nm.add("u-rig", "802-3-ethernet", "enp7s0", "rig", "enp7s0", "10.77.9.202/23")
    nm.add("u-spare", "802-3-ethernet", "", "rig spare", "enp7s0", "10.77.9.202/23")
    r = nm.run([LO, RIG])
    assert r.returncode == 0, r.stderr
    assert nm.calls() == []


def test_isolate_names_an_inactive_dhcp_wired_profile_it_cannot_place(tmp_path):
    """The 11.10.2026 profile was DHCP: inactive, its subnet is unknown until a cable goes in."""
    nm = Nm(tmp_path)
    nm.add("u-rig", "802-3-ethernet", "enp7s0", "rig", "enp7s0", "10.77.9.202/23")
    nm.add("u-onb", "802-3-ethernet", "", "Wired connection 1")
    r = nm.run([LO, RIG])
    assert r.returncode == 0, r.stderr
    assert nm.calls() == []
    assert "WARN" in r.stdout and "Wired connection 1" in r.stdout and "DHCP" in r.stdout, r.stdout


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
                   "STRIH_OPERATOR_OBS_RUNNING_CMD": str(_obs(tmp_path, False)),
                   # 14.10.2026: the keyboard step writes a scratch file and never touches this box's X.
                   "STRIH_OPERATOR_KEYBOARD_FILE": str(tmp_path / "keyboard"), "STRIH_OPERATOR_SETXKBMAP": "true",
                   # issue 1441: and never this box's own ibus setting.
                   "STRIH_OPERATOR_GSETTINGS": "true"})
    assert r.returncode == 0, r.stderr
    assert 'XKBLAYOUT="us,sk"' in (tmp_path / "keyboard").read_text()
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
# xrandr / journalctl: log the call, print the fixture text, exit with the scripted rc (default 0).
_FAKE_TOOL = r"""#!/bin/bash
echo "%s $* DISPLAY=${DISPLAY-}" >> "$FAKE_OP/calls"
cat "$FAKE_OP/%s" 2>/dev/null || true
exit "$(cat "$FAKE_OP/%s-rc" 2>/dev/null || echo 0)"
"""
FIX = Path(__file__).resolve().parent / "fixtures" / "strih_operator_1418"
XR_SNV = (FIX / "xrandr-strih-snv-reverse-prime.txt").read_text()  # strih-snv 12.10.2026, ticket comment
XR_ONE = (FIX / "xrandr-one-nvidia-provider.txt").read_text()
XR_MODESETTING_SINK = (FIX / "xrandr-modesetting-sink.txt").read_text()  # the Intel notebook's stable shape
# Reconstructed (not verbatim) from the abridged strih-snv trace quoted in comment 6132168799.
KLOG_SNV = (FIX / "klog-strih-snv-commit-hang.txt").read_text()
KLOG_CLEAN = (FIX / "klog-clean.txt").read_text()
KLOG_EXT4 = (FIX / "klog-ext4-hung-task.txt").read_text()
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


# The operator keyboard (owner 14.10.2026): the file X reads at its start and `setxkbmap -query` on :0.
KEYBOARD_OK = 'XKBMODEL="pc105"\nXKBLAYOUT="us,sk"\nXKBVARIANT=""\nXKBOPTIONS="grp:alt_shift_toggle"\n\nBACKSPACE="guess"\n'
XKB_LIVE_OK = "rules:      evdev\nmodel:      pc105\nlayout:     us,sk\noptions:    grp:alt_shift_toggle\n"
# issue 1441: the operator's ibus. No ibus-schemas file = ibus not installed; ibus-value = its setting.
_FAKE_GSETTINGS = r'''#!/bin/bash
echo "gsettings $*" >> "$FAKE_OP/kbd-log"
printf '%s\n' "${DBUS_SESSION_BUS_ADDRESS-none}" >> "$FAKE_OP/gsettings-bus"
case "${1-}" in
  list-schemas) [ -e "$FAKE_OP/gsettings-fails" ] && exit 1; cat "$FAKE_OP/ibus-schemas" 2>/dev/null || true ;;
  get) [ -e "$FAKE_OP/gsettings-fails" ] && exit 1; cat "$FAKE_OP/ibus-value" 2>/dev/null || true ;;
  set) [ -e "$FAKE_OP/gsettings-set-fails" ] && exit 1; printf '%s\n' "$4" > "$FAKE_OP/ibus-value" ;;
esac
'''


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
        _exec(self.bin / "xrandr", _FAKE_TOOL % ("xrandr", "xrandr", "xrandr"))
        _exec(self.bin / "journalctl", _FAKE_TOOL % ("journalctl", "klog", "klog"))
        _exec(self.bin / "wmctrl", _FAKE_CAT % "wmctrl")
        # 14.10.2026 the operator keyboard: log the args, print the live `-query` text.
        _exec(self.bin / "setxkbmap", '#!/bin/bash\necho "$*" >> "$FAKE_OP/xkb-calls"\n'
              'echo "setxkbmap $*" >> "$FAKE_OP/kbd-log"\ncat "$FAKE_OP/xkb" 2>/dev/null || true\n')
        # issue 1441 (21.10.2026): the operator's ibus over gsettings, never this box's own dconf.
        _exec(self.bin / "gsettings", _FAKE_GSETTINGS)
        _exec(self.bin / "dbus-run-session", '#!/bin/bash\n[ "${1-}" = "--" ] && shift\nexport DBUS_SESSION_BUS_ADDRESS=private\nexec "$@"\n')
        self.seams = True  # STRIH_OPERATOR_XRANDR / _KLOG point at the fixture files
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
            "[BasicWindow]\nSaveProjectors=true\nProjectorAlwaysOnTop=false\nMultiviewMouseSwitch=true\n"
            "CloseExistingProjectors=true\nWarnBeforeStartingStream=false\nWarnBeforeStoppingStream=false\n"
            "WarnBeforeStoppingRecord=false\n")
        (self.st / "wmctrl").write_text("0x01a00071  0 obs.obs strih-snv Projector - Multiview\n")
        self.ws({"server_enabled": True, "server_port": 4455, "auth_required": False})
        (self.st / "studio").write_text("true\n")
        (self.st / "installed").write_text("qt6-svg-plugins\n")
        for sub in ("iconengines/libqsvgicon.so", "imageformats/libqsvg.so"):
            f = self.libdir / "qt6" / "plugins" / sub
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("so")
        (self.st / "obs-up").write_text("")
        (self.st / "grabs").write_text("MD5=aaaa\nMD5=bbbb\n")
        (self.st / "xrandr").write_text(XR_MODESETTING_SINK)
        (self.st / "klog").write_text(KLOG_CLEAN)
        (self.st / "keyboard").write_text(KEYBOARD_OK)
        (self.st / "xkb").write_text(XKB_LIVE_OK)

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
        seams = {"STRIH_OPERATOR_XRANDR": str(self.st / "xrandr"), "STRIH_OPERATOR_KLOG": str(self.st / "klog"),
                 "STRIH_OPERATOR_KEYBOARD_FILE": str(self.st / "keyboard")}
        return {**(seams if self.seams else {}), "PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "FAKE_OP": str(self.st),
                "STRIH_OPERATOR_EUID": self.euid, "STRIH_OPERATOR_XINPUT": str(self.xinput),
                "STRIH_OPERATOR_HCICONFIG": str(self.hci), "STRIH_OPERATOR_SYSFS": str(self.sys),
                "COMPANION_SATELLITE_REST_URL": self.rest, "STRIH_OPERATOR_STUDIO_CMD": str(self.studio),
                "STRIH_OPERATOR_GRAB_CMD": str(self.grab), "STRIH_OPERATOR_GRAB_GAP_S": "0",
                "STRIH_OPERATOR_OBS_RUNNING_CMD": str(self.obs_up), "STRIH_OPERATOR_LIBDIR": str(self.libdir)}

    def rows(self, box="strih-snv"):
        r = _bash('strih_operator_grade_rows "$1" "$2" "$3"', env=self.env(), box=box,
                  args=(str(REPO), str(self.home), "operator1"))
        assert r.returncode == 0, r.stderr
        return [tuple(ln.split("|", 1)) for ln in r.stdout.splitlines() if ln]

    def row(self, item, box="strih-snv"):
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


ITEMS = ["tap-to-click", "bluetooth", "stream-deck", "mv-click", "operator-ini", "studio-mode", "ws-seed",
         "qt6-svg", "operator-input", "panel-gpu", "drm-hang", "keyboard", "operator-panel", "mv-projector"]


def test_a_healthy_operator_surface_passes_every_row(op):
    rows = op.rows()
    assert [d.split(")", 1)[0][1:] for _, d in rows] == ITEMS
    assert [st for st, _ in rows] == ["OK"] * 14, rows
    assert _bash("strih_operator_row_count").stdout == "14"


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


def test_the_12_10_dead_bluetooth_adapter_is_a_note_naming_down_and_the_zero_address(op):
    # owner 16.10.2026 ("ok nemusis to riesit netreba mys"): Bluetooth is report-only on every role
    (op.st / "hci").write_text(HCI_DEAD)
    st, d = op.row("bluetooth")
    assert st == "NOTE" and "DOWN" in d and "00:00:00:00:00:00" in d, d


def test_an_adapter_that_hciconfig_cannot_read_is_a_note(op):
    """A registered adapter hciconfig cannot read is named, report-only (owner 16.10.2026)."""
    (op.st / "hci").write_text("")
    (op.sys / "class" / "bluetooth" / "hci0").mkdir(parents=True)
    st, d = op.row("bluetooth")
    assert st == "NOTE" and "hci0" in d, d


def test_a_box_with_no_bluetooth_adapter_is_a_note_not_a_failure(op):
    """strih-pp (12.10.2026) is a desktop with a USB receiver mouse and no Bluetooth adapter at all:
    nothing to pair, so the row is a NOTE, never a FAIL."""
    (op.st / "hci").write_text("")
    st, d = op.row("bluetooth")
    assert st == "NOTE" and "no Bluetooth adapter" in d, d


def test_bluetooth_is_graded_because_setup_strih_keeps_it():
    """The BT row grades every box: setup-strih passes keep-bluetooth to the kiosk on every role."""
    assert 'obs_box_kiosk "$DESKTOP_USER" strih keep-bluetooth' in SETUP.read_text()


def test_a_missing_xinput_is_named_never_read_as_a_hung_x(op):
    op.xinput.unlink()
    for item in ("tap-to-click", "operator-input"):
        st, d = op.row(item)
        assert st == "FAIL" and "xinput is not installed" in d, d


def test_the_kiosk_installs_xinput():
    kiosk = (SCRIPTS / "lib" / "obs-box-kiosk.sh").read_text()
    line = next(ln for ln in kiosk.splitlines() if "apt-get install -y openbox lightdm" in ln)
    assert " xinput" in line.split("||")[0], line


def test_a_touchpad_whose_properties_do_not_answer_is_named(op):
    (op.st / "props-12").unlink()
    st, d = op.row("tap-to-click")
    assert st == "FAIL" and "did not answer" in d and "not driven by libinput" not in d, d


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
    st, d = op.row("mv-click")
    assert st == "FAIL" and "not seeded" in d, d


def test_multiview_click_is_read_the_way_libobs_config_get_bool_reads_it(op):
    """libobs: astrcmpi(v, "true") == 0 || strtoull(v, 10, or 16 after 0x) != 0 (config-file.c)."""
    ini = op.home / ".config" / "obs-studio" / "user.ini"
    for value, want in (("TRUE", "OK"), ("1", "OK"), ("0x1", "OK"), ("12abc", "OK"), ("on", "FAIL"),
                        ("yes", "FAIL"), ("0", "FAIL"), ("0x0", "FAIL"), ("", "FAIL")):
        ini.write_text("[BasicWindow]\nMultiviewMouseSwitch=%s\n" % value)
        assert op.row("mv-click")[0] == want, (value, op.row("mv-click"))


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


def test_the_12_10_amdgpu_ddx_panel_sink_fails_naming_the_ddx_package(op):
    """strih-snv 12.10.2026: the AMD iGPU sink ran on xf86-video-amdgpu (the Intel notebook before it ran
    modesetting and was stable), and its panel commit waited forever on an NVIDIA fence."""
    (op.st / "xrandr").write_text(XR_SNV)
    st, d = op.row("panel-gpu")
    assert st == "FAIL" and "AMD Radeon 780M Graphics @ pci:0000:64:00.0" in d and "NVIDIA-0" in d, d
    assert "xserver-xorg-video-amdgpu" in d and "modesetting" in d, d
    assert "BIOS" not in d, d


def test_a_sink_on_any_other_ddx_fails_too(op):
    (op.st / "xrandr").write_text(XR_MODESETTING_SINK.replace("name:modesetting", "name:Intel"))
    st, d = op.row("panel-gpu")
    assert st == "FAIL" and "Intel" in d, d


def test_a_secondary_provider_that_is_no_sink_is_ok(op):
    (op.st / "xrandr").write_text(XR_SNV.replace("Sink Output, ", ""))
    assert op.row("panel-gpu")[0] == "OK"


def test_one_gpu_provider_is_a_note(op):
    (op.st / "xrandr").write_text(XR_ONE)
    st, d = op.row("panel-gpu")
    assert st == "NOTE" and "one GPU provider" in d and "NVIDIA-0" in d, d


def test_a_modesetting_panel_sink_is_ok(op):
    st, d = op.row("panel-gpu")
    assert st == "OK" and "modesetting" in d and "NVIDIA-0" in d, d


def test_a_provider_read_that_fails_names_the_hung_x(op):
    (op.st / "xrandr").unlink()
    st, d = op.row("panel-gpu")
    assert st == "FAIL" and "does not answer clients" in d, d


def test_the_provider_read_runs_xrandr_on_display_0_as_the_operator_bounded(op):
    op.seams, op.euid = False, "0"
    (op.st / "xrandr").write_text(XR_SNV)
    assert op.row("panel-gpu")[0] == "FAIL"
    assert ("sudo -u operator1 env DISPLAY=:0 XAUTHORITY=%s/.Xauthority timeout 10 xrandr --listproviders"
            % op.home) in op.calls()


@pytest.mark.parametrize("rc,word", [("124", "did not answer within 10 s"), ("127", "not installed"),
                                     ("1", "does not answer clients")])
def test_a_failed_xrandr_is_named(op, rc, word):
    op.seams = False
    (op.st / "xrandr-rc").write_text(rc)
    st, d = op.row("panel-gpu")
    assert st == "FAIL" and word in d, d


def test_the_12_10_hung_display_commit_fails_naming_its_first_time(op):
    (op.st / "klog").write_text(KLOG_SNV)
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "Oct 11 22:19:14" in d and "kworker/u64:4:177186" in d, d
    assert "Oct 12 16:42:16" not in d, d


def test_a_flip_done_timeout_alone_fails(op):
    keep = [ln for ln in KLOG_SNV.splitlines() if not ln.startswith("Oct 11 22:")]
    (op.st / "klog").write_text("\n".join(keep) + "\n")
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "Oct 12 16:42:16" in d and "flip_done timed out" in d, d


def test_a_clean_kernel_log_is_ok(op):
    st, d = op.row("drm-hang")
    assert st == "OK" and "no hung display commit" in d, d


def test_a_hung_task_outside_drm_is_not_this_row(op):
    """A jbd2 hang is a disk fault, not the panel: OK here, even when a DRM word follows past its trace."""
    noise = "".join("Oct 12 18:03:%02d strih-snv kernel: r8152 2-1:1.0 eth: noise %d\n" % (i, i) for i in range(30))
    frame = "Oct 12 18:04:00 strih-snv kernel:  commit_work+0x14/0x20\n"
    (op.st / "klog").write_text(KLOG_EXT4 + noise + frame)
    assert op.row("drm-hang")[0] == "OK"
    # the control: the same frame inside the jbd2 task's 25-line window IS read as its trace
    (op.st / "klog").write_text(KLOG_EXT4 + frame)
    assert op.row("drm-hang")[0] == "FAIL"


def test_a_drm_hang_after_an_unrelated_hung_task_is_found(op):
    (op.st / "klog").write_text(KLOG_EXT4 + KLOG_SNV)
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "Oct 11 22:19:14" in d and "kworker/u64:4" in d and "jbd2" not in d, d


@pytest.mark.parametrize("line", [
    "[drm:amdgpu_dm_atomic_commit_tail [amdgpu]] *ERROR* Waiting for fences timed out!",
    "[drm:nv_drm_atomic_commit [nvidia_drm]] *ERROR* [nvidia-drm] [GPU ID 0x00000100] Flip event timeout on head 0",
    "[drm:drm_atomic_helper_wait_for_dependencies [drm_kms_helper]] *ERROR* [CRTC:62:pipe A] commit wait timed out",
    "amdgpu 0000:64:00.0: [drm] *ERROR* [CRTC:364:crtc-0] flip_done timed out",
])
def test_every_drm_commit_timeout_form_fails(op, line):
    (op.st / "klog").write_text(KLOG_CLEAN + "Oct 12 17:03:09 strih-snv kernel: %s\n" % line)
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "Oct 12 17:03:09" in d, d


def test_a_symbol_that_only_contains_commit_work_is_no_drm_frame(op):
    kj = "Oct 12 18:02:44 strih-snv kernel:  kjournald2"
    frame = "Oct 12 18:02:44 strih-snv kernel:  ext4_commit_work_done+0x10/0x20\n"
    assert KLOG_EXT4.count(kj) == 1
    (op.st / "klog").write_text(KLOG_EXT4.replace(kj, frame + kj))
    assert op.row("drm-hang")[0] == "OK"


def test_a_journal_with_no_kernel_line_is_unreadable_not_clean(op):
    """journalctl as a user outside adm/systemd-journal exits 0 and prints only `-- No entries --`."""
    (op.st / "klog").write_text("-- No entries --\n")
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "kernel log" in d and "root" in d, d

def test_an_unreadable_kernel_log_fails(op):
    (op.st / "klog").unlink()
    st, d = op.row("drm-hang")
    assert st == "FAIL" and "kernel log" in d, d


def test_the_kernel_log_read_is_this_boots_journal(op):
    op.seams = False
    (op.st / "klog").write_text(KLOG_SNV)
    assert op.row("drm-hang")[0] == "FAIL"
    assert "journalctl -k -b 0 --no-pager" in op.calls()


def test_the_report_prints_every_row_through_the_callers_functions(op):
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_operator_grade_report "$1" "$2" "$3"; echo "rc=$?"')
    r = _bash(body, env=op.env(), args=(str(REPO), str(op.home), "operator1"))
    assert r.returncode == 0, r.stderr
    assert [ln.split()[0] for ln in r.stdout.splitlines()] == ["PASS"] * 14 + ["rc=0"]


# --- the wiring: setup-strih step 16g, verify-strih items 43 + 16 ---------------------------------------

def _applies(box, step):
    r = _bash('if WHY="$(obs_box_step_applies "$1")"; then echo RUN; else echo "SKIP $WHY"; fi', box=box,
              args=(step,))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_provision_sources_the_operator_lib():
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-operator.sh"' in PROVISION.read_text()


def test_setup_runs_step_16g_once_after_the_tally_and_before_the_final_verify():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    call = 'strih_operator_install "${OBS_CFG}/user.ini" "$DESKTOP_USER"'
    assert s.count(call) == 1
    assert s.index('step "16f"') < s.index('step "16g"') < s.index(call) < s.index("step 17 ")
    # the [BasicWindow] seed moved into the lib: setup-strih keeps no copy of it
    assert 'cp.set("BasicWindow"' not in s and "MultiviewMouseSwitch" not in s


def test_setup_step_1_no_longer_asks_the_operator_to_take_the_second_profile_down():
    s = SETUP.read_text()
    step1 = s[s.index('step 1 "'):s.index('step "1b"')]
    assert "nmcli connection modify <name> connection.autoconnect no" not in step1
    assert "16g" in step1


def test_the_step_7_websocket_seed_is_what_the_ws_seed_row_requires(tmp_path):
    s = SETUP.read_text()
    seed = s[s.index("<<'WS'\n") + len("<<'WS'\n"):s.index("\nWS\n")]
    f = tmp_path / "config.json"
    f.write_text(seed + "\n")
    r = _bash('strih_operator_ws_seed_verdict "$1"', args=(str(f),))
    assert r.stdout.startswith("OK|(ws-seed)"), r.stdout


def test_verify_runs_item_43_once_after_the_tally_within_the_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    call = 'strih_operator_grade_report "${HERE}/.." "$USER_HOME" "${STRIH_LX_USER:-newlevel}"'
    assert v.count(call) == 1
    i = v.index(call)
    assert v.index("strih_tally_grade_report") < i
    # never between item 34 and 32: test_ndi_discovery_1342 runs that slice with only its own lib sourced
    assert not (v.index("# 34) NDI discovery") < i < v.index("# 32) the shared OBS-box"))
    assert "re-run setup-strih.sh step 16g" in v[v.index("SaveProjectors=true NOT pre-seeded"):][:200]


def test_the_hub_is_graded_live_on_the_strih_role_only():
    assert _applies("strih-snv", "13-hub-live") == "RUN"
    assert _applies("strih-pp", "13-hub-live") == "RUN"
    assert _applies("stream-pp", "13-hub-live").startswith("SKIP ")


def _item16(tmp_path, box, enabled, active, installed=True):
    v = VERIFY.read_text()
    item = v[v.index("# 16) intercom hub unit"):v.index("# 17)")]
    unit = tmp_path / "intercom-hub.service"
    if installed:
        unit.write_text("[Unit]\n")
    item = item.replace("/etc/systemd/system/intercom-hub.service", str(unit))
    stub = tmp_path / "stub"
    stub.mkdir()
    _exec(stub / "systemctl",
          '#!/bin/bash\ncase "$1" in\n  is-enabled) echo %s; [ %s = enabled ] ;;\n'
          '  is-active) echo %s; [ %s = active ] ;;\nesac\n' % (enabled, enabled, active, active))
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_intercom_hub_crash_loop() { return 1; }\nstrih_intercom_hub_cred_grade() { :; }\n'
            'strih_intercom_hub_memlock_grade() { :; }\nWS_HOST=192.0.2.1\n' + item)
    r = _bash(body, box=box, env={"PATH": "%s:%s" % (stub, os.environ["PATH"])})
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_item_16_fails_an_inactive_or_disabled_hub_on_the_strih_role(tmp_path):
    out = _item16(tmp_path, "strih-snv", "disabled", "inactive")
    assert out.startswith("FAIL") and "enabled=disabled" in out and "active=inactive" in out, out
    v = VERIFY.read_text()
    assert "M4" not in v[v.index("# 16) intercom hub unit"):v.index("# 17)")]


def test_item_16_passes_an_enabled_active_hub(tmp_path):
    assert _item16(tmp_path, "strih-snv", "enabled", "active").startswith("PASS")


def test_item_16_only_reports_an_inactive_hub_on_the_stream_role(tmp_path):
    assert _item16(tmp_path, "stream-pp", "enabled", "inactive").startswith("NOTE")


def test_item_16_fails_a_missing_hub_on_the_strih_role(tmp_path):
    out = _item16(tmp_path, "strih-snv", "enabled", "active", installed=False)
    assert out.startswith("FAIL") and "not installed" in out, out


def _step13_start(tmp_path, box, active, loop=False):
    s = SETUP.read_text()
    a = s.index('if WHY="$(obs_box_step_applies 13-hub-live)"')
    block = s[a:s.index("\nfi\n", a) + 4]
    stub = tmp_path / "stub"
    stub.mkdir()
    log = tmp_path / "calls"
    _exec(stub / "systemctl", '#!/bin/bash\necho "systemctl $*" >> "%s"\n'
                              '[ "$1" = is-active ] && exit %d\nexit 0\n' % (log, 0 if active else 3))
    body = ('fail() { echo "FAIL $*"; exit 1; }\nwarn() { echo "WARN $*"; }\n'
            'strih_intercom_hub_crash_loop() { %s; }\n' % ("echo auto-restart; return 0" if loop else "return 1")
            + block)
    r = _bash(body, box=box, env={"PATH": "%s:%s" % (stub, os.environ["PATH"])})
    if loop:
        return r
    assert r.returncode == 0, r.stdout + r.stderr
    return log.read_text() if log.exists() else ""


def test_setup_step_13_starts_a_stopped_hub_on_the_strih_role(tmp_path):
    calls = _step13_start(tmp_path, "strih-snv", active=False)
    assert "systemctl start intercom-hub" in calls, calls
    assert "restart" not in calls


def test_setup_step_13_never_touches_a_running_hub(tmp_path):
    calls = _step13_start(tmp_path, "strih-snv", active=True)
    assert "systemctl start" not in calls and "restart" not in calls, calls


def test_setup_step_13_leaves_the_stream_role_hub_alone(tmp_path):
    assert _step13_start(tmp_path, "stream-pp", active=False) == ""


def test_setup_fails_a_hub_that_crash_loops_after_the_start(tmp_path):
    """Type=simple: `systemctl start` returns 0 before an exec-time failure; the loop check catches it."""
    r = _step13_start(tmp_path, "strih-snv", active=False, loop=True)
    assert r.returncode == 1 and "FAIL" in r.stdout and "auto-restart" in r.stdout, r.stdout + r.stderr


def test_the_hub_starts_only_after_step_14_wrote_its_room_secret():
    """The unit's LoadCredential needs /etc/intercom-hub/janus-room.secret, which step 14 creates."""
    s = SETUP.read_text()
    assert s.index("step 14 ") < s.index("systemctl start intercom-hub") < s.index("step 15 ")


def test_setup_step_13_has_no_stale_m4_wording():
    s = SETUP.read_text()
    step13 = s[s.index("step 13 "):s.index("step 14 ")]
    assert "M4" not in step13 and "NEVER started" not in step13 and "NOT started here" not in step13


def test_the_stream_role_does_not_grade_bluetooth(op):
    """Owner 13.10.2026: "na stream-pp netreba bluethooth ani na stream snv, iba na strih-snv treba".
    stream-pp's hci0 reads DOWN (rfkill soft-blocked): a NOTE on the stream role, never a FAIL."""
    (op.st / "hci").write_text(HCI_DEAD)
    (op.sys / "class" / "bluetooth" / "hci0").mkdir(parents=True)
    st, d = op.row("bluetooth", box="stream-pp")
    assert st == "NOTE" and "stream role" in d, d


def test_the_strih_role_reports_a_dead_bluetooth_as_a_note(op):
    # owner 16.10.2026 supersedes the 13.10 strih-role FAIL: the operator needs no Bluetooth mouse
    (op.st / "hci").write_text(HCI_DEAD)
    (op.sys / "class" / "bluetooth" / "hci0").mkdir(parents=True)
    st, d = op.row("bluetooth", box="strih-pp")
    assert st == "NOTE" and "DOWN" in d, d


# ---------------------------------------------------------------------------------------------
# The operator keyboard (owner 14.10.2026): us + Slovak, Alt+Shift, in the file AND live on :0.
# ---------------------------------------------------------------------------------------------
def test_a_us_only_keyboard_file_fails_naming_the_layout(op):
    """The 14.10.2026 state of strih-pp / stream-pp / strih-snv before the fix."""
    (op.st / "keyboard").write_text('XKBMODEL="pc105"\nXKBLAYOUT="us"\nXKBVARIANT=""\nXKBOPTIONS=""\n')
    st, d = op.row("keyboard")
    assert st == "FAIL" and 'XKBLAYOUT="us"' in d and "16g" in d, d


def test_a_live_x_without_the_slovak_layout_fails(op):
    (op.st / "xkb").write_text("rules:      evdev\nmodel:      pc105\nlayout:     us\n")
    st, d = op.row("keyboard")
    assert st == "FAIL" and "running :0" in d and '"us"' in d, d


def test_x_not_answering_fails_the_keyboard_row(op):
    (op.st / "xkb").write_text("")
    st, d = op.row("keyboard")
    assert st == "FAIL" and "answered nothing" in d, d


def test_keyboard_install_writes_the_file_and_applies_it_live(op, tmp_path):
    kb = tmp_path / "keyboard"
    kb.write_text('XKBMODEL="pc105"\nXKBLAYOUT="us"\nXKBVARIANT=""\nXKBOPTIONS=""\n\nBACKSPACE="guess"\n')
    env = {**op.env(), "STRIH_OPERATOR_KEYBOARD_FILE": str(kb)}
    r = _bash('strih_operator_keyboard_install operator1 "$1"', env=env, args=(str(op.home),))
    assert r.returncode == 0, r.stdout + r.stderr
    text = kb.read_text()
    assert 'XKBLAYOUT="us,sk"' in text and 'XKBOPTIONS="grp:alt_shift_toggle"' in text, text
    assert 'XKBMODEL="pc105"' in text and 'BACKSPACE="guess"' in text, "the other keys stay: %s" % text
    assert "-layout us,sk -option  -option grp:alt_shift_toggle" in (op.st / "xkb-calls").read_text()
    # Idempotent: a second run leaves the same file.
    _bash('strih_operator_keyboard_install operator1 "$1"', env=env, args=(str(op.home),))
    assert kb.read_text() == text
    # The healthy grade reads it back as OK.
    (op.st / "keyboard").write_text(text)
    assert op.row("keyboard")[0] == "OK"


def test_keyboard_install_creates_a_missing_file(op, tmp_path):
    kb = tmp_path / "nokb"
    env = {**op.env(), "STRIH_OPERATOR_KEYBOARD_FILE": str(kb)}
    r = _bash('strih_operator_keyboard_install operator1 "$1"', env=env, args=(str(op.home),))
    assert r.returncode == 0, r.stdout + r.stderr
    assert 'XKBLAYOUT="us,sk"' in kb.read_text() and 'XKBOPTIONS="grp:alt_shift_toggle"' in kb.read_text()
