"""Issue 1439, ROZHODNUTÉ 1.11.2026: dsync binds PTP to the NIC the box declares, never by chance.

WHY: on 1.11.2026 stream-snv ran the whole SNV production without the Dante clock. At the 08:31
boot its VBAN-leg USB NIC got an address before the rig NIC, and dantesync 1.20.0 joined PTP on the
first IPv4 interface it listed. The owner ruled: "by defaul dantesync musi mat jasne nastavenie na
ktoru eth sa ma bindnut, neexistuje ze sa toto deje nejako nahodne". The 1.11 stopgap (an
ExecStartPre waiting for the rig IP) still relied on the rig NIC having the lowest ifindex.
dsync >= 1.21 honours `--interface <NIC>` (waits for it, never falls back) and serves
`ptp_interface_configured` / `ptp_interface` on /status (pin 1.28.0 since issue 1487).

What this pins:
  * `strih_dantesync_unit_text ROLE ARGS RIG_NIC` puts `--interface <RIG_NIC>` on ExecStart, with
    no ExecStartPre wait; a malformed NIC name is refused and emits nothing;
  * `strih_dantesync_rig_nic` resolves the NIC by the existing rig-NIC rule (`strih_lx_rig_nic`);
  * setup-strih step 2 and verify-strih item 6c render the unit with that NIC, so verify grades the
    unit setup installs;
  * verify item 6d grades dsync's OWN /status: configured AND joined NIC == the rig NIC
    (`strih_dantesync_iface_verdict`); the boot-journal reader is gone.

Tier-0: bash + pytest only (fake sysfs, fake curl).
"""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-provision.sh"
DS_LIB = SCRIPTS / "lib" / "strih-dantesync.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"


def _bash(script, *args, env=None):
    """Run SCRIPT under the callers' set -euo pipefail; values only as positional arguments."""
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *args],
                          capture_output=True, text=True, cwd=str(REPO), timeout=60,
                          env=dict(os.environ, **(env or {})))


def _unit(role, args, *extra):
    return _bash('. "$1"\nshift\nstrih_dantesync_unit_text "$@"', str(LIB), role, args, *extra)


# --- the unit --------------------------------------------------------------------------------

def test_the_server_unit_binds_the_rig_nic_and_has_no_wait_line():
    r = _unit("server", "", "enp7s0")
    assert r.returncode == 0, r.stderr
    assert "ExecStart=/usr/local/bin/dsync --interface enp7s0\n" in r.stdout, r.stdout
    assert "ExecStartPre=" not in r.stdout, "dsync waits for the configured NIC itself"
    for part in ("Type=simple", "Restart=always", "RestartSec=5", "WantedBy=multi-user.target"):
        assert part in r.stdout, r.stdout


def test_a_client_unit_binds_the_rig_nic_after_its_upstream():
    r = _unit("client", "--ntp-server strih-snv.lan", "enp56s0")
    assert r.returncode == 0, r.stderr
    assert "ExecStart=/usr/local/bin/dsync --ntp-server strih-snv.lan --interface enp56s0\n" in r.stdout


def test_without_a_nic_argument_the_unit_is_the_bare_shape():
    r = _unit("server", "")
    assert r.returncode == 0, r.stderr
    assert "ExecStart=/usr/local/bin/dsync\n" in r.stdout
    assert "--interface" not in r.stdout and "ExecStartPre=" not in r.stdout


@pytest.mark.parametrize("bad", ["", "enp7s0; reboot", "enp7s0'", "a b", "-i", "x" * 16,
                                 "10.77.9.202/23", "enp7s0\n"])
def test_a_malformed_nic_name_is_refused_and_emits_nothing(bad):
    r = _unit("server", "", bad)
    assert r.returncode != 0, bad
    assert r.stdout.strip() == "", (bad, r.stdout)


def test_a_vlan_and_a_usb_nic_name_are_accepted():
    for nic in ("enp7s0.70", "enx00e04c680185", "eth0"):
        r = _unit("server", "", nic)
        assert r.returncode == 0, (nic, r.stderr)
        assert "--interface %s\n" % nic in r.stdout


# --- the rig NIC resolver ----------------------------------------------------------------------

def _sysfs(tmp_path, nics):
    """A fake /sys with class/net/<nic>/device/driver -> <driver>."""
    root = tmp_path / "sys"
    for nic, driver in nics.items():
        d = root / "class" / "net" / nic / "device"
        d.mkdir(parents=True)
        drv = tmp_path / "drivers" / driver
        drv.mkdir(parents=True, exist_ok=True)
        (d / "driver").symlink_to(drv)
    return root


def test_the_rig_nic_is_resolved_by_the_rig_nic_rule(tmp_path):
    root = _sysfs(tmp_path, {"enp7s0": "atlantic", "enp2s0": "r8169"})
    r = _bash('. "$1"\n. "$2"\nstrih_dantesync_rig_nic', str(LIB), str(DS_LIB),
              env={"STRIH_DANTESYNC_SYSFS": str(root), "STRIH_FACT_BOX": "strih-snv"})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp7s0"


def test_an_unresolvable_rig_nic_fails_and_prints_no_name(tmp_path):
    root = _sysfs(tmp_path, {"enp2s0": "r8169"})
    r = _bash('. "$1"\n. "$2"\nip() { :; }\nstrih_dantesync_rig_nic', str(LIB), str(DS_LIB),
              env={"STRIH_DANTESYNC_SYSFS": str(root)})
    assert r.returncode != 0
    assert r.stdout.strip() == ""


# --- the verdict -------------------------------------------------------------------------------

@pytest.mark.parametrize("read,conf,joined,rig,want", [
    ("1", "enp7s0", "enp7s0", "enp7s0", "ok:enp7s0"),
    ("1", "", "enp7s0", "enp7s0", "not-configured"),
    ("1", "enp7s0.70", "enp7s0.70", "enp7s0", "configured-elsewhere:enp7s0.70:enp7s0"),
    ("1", "enp56s0", "enx00e04c680185", "enp56s0", "joined-elsewhere:enx00e04c680185:enp56s0"),
    ("1", "enp56s0", "", "enp56s0", "joined-elsewhere:-:enp56s0"),
    ("0", "", "", "enp7s0", "unreadable"),
    ("1", "enp7s0", "enp7s0", "", "norig"),
])
def test_the_iface_verdict_table(read, conf, joined, rig, want):
    r = _bash('. "$1"\nstrih_dantesync_iface_verdict "$2" "$3" "$4" "$5"; echo END',
              str(DS_LIB), read, conf, joined, rig)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == [want, "END"], r.stdout


def _fake_curl(tmp_path, body, rc=0):
    d = tmp_path / "bin"
    d.mkdir(exist_ok=True)
    (tmp_path / "body.json").write_text(body)
    (d / "curl").write_text('#!/bin/sh\necho "$*" >> "%s"\ncat "%s"\nexit %d\n'
                            % (tmp_path / "curl.log", tmp_path / "body.json", rc))
    (d / "curl").chmod(0o755)
    return {"PATH": "%s:%s" % (d, os.environ["PATH"])}


@pytest.mark.parametrize("body,want", [
    ('{"ptp_interface_configured":"enp7s0","ptp_interface":"enp7s0","x":1}', "ok:enp7s0"),
    ('{"ptp_interface_configured":null,"ptp_interface":"enp7s0"}', "not-configured"),
    ('{"ptp_interface_configured":"enp7s0","ptp_interface":null}', "joined-elsewhere:-:enp7s0"),
    ('not json', "unreadable"),
    ('', "unreadable"),
])
def test_the_grade_reads_dsyncs_own_status(tmp_path, body, want):
    env = _fake_curl(tmp_path, body)
    r = _bash('. "$1"\nstrih_dantesync_iface_grade enp7s0; echo END', str(DS_LIB), env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == [want, "END"], r.stdout
    log = (tmp_path / "curl.log").read_text()
    assert "127.0.0.1:8898/status" in log and "-m" in log.split(), log


def test_a_dead_status_endpoint_is_unreadable_under_set_e(tmp_path):
    env = _fake_curl(tmp_path, "", rc=7)
    r = _bash('. "$1"\nstrih_dantesync_iface_grade enp7s0; echo END', str(DS_LIB), env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["unreadable", "END"]


def test_the_journal_reader_is_gone():
    assert "strih_dantesync_join_verdict" not in DS_LIB.read_text()
    assert "strih_dantesync_join_verdict" not in VERIFY.read_text()


# --- the wiring --------------------------------------------------------------------------------

def test_setup_and_verify_render_the_unit_with_the_rig_nic():
    for path in (SETUP, VERIFY):
        calls = [l for l in path.read_text().splitlines() if 'strih_dantesync_unit_text "$' in l]
        assert calls, path
        for line in calls:
            assert "strih_dantesync_rig_nic" in line, (path, line)
            assert "strih_lx_ip" not in line, (path, line)


def test_verify_item_6d_grades_the_status_before_item_6c():
    v = VERIFY.read_text()
    d6 = v.index("# 6d) dsync PTP NIC")
    assert v.index("# 6b) dantesync ROLE") < d6 < v.index("# 6c) dantesync UNIT content")
    block = v[d6:v.index("# 6c) dantesync UNIT content")]
    assert "strih_dantesync_iface_grade" in block and "strih_dantesync_rig_nic" in block
    assert "(dantesync-nic)" in block and 'bad "' in block


def _item6d(tmp_path, body, nic="enp7s0"):
    """Run verify item 6d's REAL text under set -euo pipefail with a fake curl and a stub rig NIC."""
    v = VERIFY.read_text()
    block = v[v.index("# 6d) dsync PTP NIC"):v.index("# 6c) dantesync UNIT content")]
    env = _fake_curl(tmp_path, body)
    harness = ('. "$1"\nok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\n'
               'strih_dantesync_rig_nic() { [ -n "$NIC" ] && echo "$NIC"; }\n' + block + '\necho END\n')
    r = _bash(harness, str(DS_LIB), env=dict(env, NIC=nic))
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    return r.stdout.strip().splitlines()[0]


def test_verify_item_6d_passes_and_fails_by_name(tmp_path):
    ok = '{"ptp_interface_configured":"enp7s0","ptp_interface":"enp7s0"}'
    assert _item6d(tmp_path, ok).startswith("PASS (dantesync-nic)")
    line = _item6d(tmp_path, '{"ptp_interface_configured":null,"ptp_interface":"enp7s0"}')
    assert line.startswith("FAIL (dantesync-nic) not-configured"), line
    assert "setup-strih" in line, line
    assert _item6d(tmp_path, ok, nic="").startswith("FAIL (dantesync-nic) norig")
