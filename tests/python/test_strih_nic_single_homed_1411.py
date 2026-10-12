"""issue 1411: the strih must carry the rig subnet on its rig NIC only.

Live 11.-12.10.2026: the new strih notebook had its onboard port (enp2s0, NetworkManager's auto profile
"Wired connection 1", DHCP 10.77.9.192) up on the rig /23 next to the 10 GbE rig NIC (enp7s0,
10.77.9.202), with the LOWER route metric. Every packet the strih sent left from .192, the dantesync
date-authority replies included, and every dantesync follower on the rig lost PTP for ~22 h.
verify-strih item 36 graded the rig NIC's own profile and passed. The grader now also reads
`ip -o -4 addr show` and FAILs when any other interface carries an address on the rig subnet.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from test_strih_nic_driver_1391 import LIB, PROVISION, PcieSysfs, _bash, _states  # noqa: E402

RIG = "2: enp7s0    inet 10.77.9.202/23 brd 10.77.9.255 scope global noprefixroute enp7s0\\       valid_lft forever preferred_lft forever"
LO = "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever"
ONBOARD = "3: enp2s0    inet 10.77.9.192/23 brd 10.77.9.255 scope global dynamic noprefixroute enp2s0\\       valid_lft 86000sec preferred_lft 86000sec"
DOCKER = "4: docker0    inet 172.17.0.1/16 brd 172.17.255.255 scope global docker0\\       valid_lft forever preferred_lft forever"
WIFI_OTHER = "5: wlp4s0    inet 192.168.1.20/24 brd 192.168.1.255 scope global dynamic wlp4s0\\       valid_lft 3600sec preferred_lft 3600sec"


def _verdict(text, nic="enp7s0", ip="10.77.9.202"):
    return _bash('strih_nic_single_homed_verdict "$N" "$I" "$T"', env={"N": nic, "I": ip, "T": text})


def test_a_second_interface_on_the_rig_subnet_fails_naming_it():
    r = _verdict("\n".join([LO, RIG, ONBOARD]))
    assert r.returncode == 1, r
    assert r.stdout.startswith("FAIL|(nic-homed)"), r.stdout
    assert "enp2s0" in r.stdout and "10.77.9.192" in r.stdout, r.stdout
    assert "connection.autoconnect no" in r.stdout, r.stdout


def test_only_the_rig_nic_on_the_rig_subnet_passes():
    r = _verdict("\n".join([LO, RIG, DOCKER, WIFI_OTHER]))
    assert r.returncode == 0, r
    assert r.stdout.startswith("OK|(nic-homed)"), r.stdout
    assert "enp7s0" in r.stdout, r.stdout


def test_a_rig_ip_on_no_interface_is_never_a_pass():
    r = _verdict("\n".join([LO, DOCKER]))
    assert r.returncode == 1, r
    assert r.stdout.startswith("FAIL|(nic-homed)"), r.stdout


def test_no_address_text_prints_no_row():
    r = _verdict("")
    assert r.returncode == 0 and r.stdout == "", r


def test_grade_fails_the_live_dual_homed_strih(tmp_path):
    box = PcieSysfs(tmp_path)
    env = {"PATH": "%s:%s" % (box.bin, os.environ["PATH"]), "FAKE_NM": str(tmp_path / "nm.tsv"),
           "SYSROOT": str(box.sys), "ADDRS": "\n".join([LO, RIG, ONBOARD])}
    r = _bash('strih_nic_grade_rows "$SYSROOT" "$ADDRS"', env=env, sources=(PROVISION, LIB))
    assert r.returncode == 0, r.stderr
    rows = [tuple(line.split("|", 1)) for line in r.stdout.splitlines()]
    assert _states(rows)["(nic-homed)"] == "FAIL", rows


def test_setup_step_1_tells_the_operator_to_disable_every_other_wired_profile():
    setup = (LIB.parent.parent / "setup-strih.sh").read_text()
    assert "connection.autoconnect no" in setup, "step 1's operator note must say it"
