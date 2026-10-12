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
