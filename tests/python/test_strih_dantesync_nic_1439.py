"""Issue 1439, 1.11.2026: verify-strih grades the NIC dantesync really joined PTP on.

WHY: stream-snv ran the whole SNV production without the Dante clock: at the 08:31 boot its VBAN-leg
USB NIC had an address before the rig NIC, and dantesync (which picks the first IPv4 interface it
lists) joined PTP there. Owner ruling 1.11.2026: dantesync must be bound to an explicitly configured
NIC, never by chance (dantesync#131). Until dantesync honours such a setting, verify-strih item 6d
reads the newest `Joined Multicast Groups on <iface>` line of this boot's dantesync journal (ONE
bounded read: `journalctl -g … -n 1`) and FAILs when it is not the NIC that carries the box's own
STRIH_IP.

Tier-0: bash + pytest only, fake `journalctl` / `ip` on PATH.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-dantesync.sh"
VERIFY = REPO / "scripts" / "verify-strih.sh"

IP_LISTING = ("1: lo    inet 127.0.0.1/8 scope host lo\n"
              "2: enp56s0    inet 10.77.9.204/23 brd 10.77.9.255 scope global enp56s0\n"
              "3: enx00e04c680185    inet 10.77.7.204/32 scope global enx00e04c680185\n")
JOIN_RIG = "Joined Multicast Groups on enp56s0 (10.77.9.204) - Kernel timestamping"
JOIN_VBAN = "Joined Multicast Groups on enx00e04c680185 (10.77.7.204) - Kernel timestamping (rejoin)"


def _verdict(tmp_path, journal, listing=IP_LISTING, ip="10.77.9.204"):
    d = tmp_path / "bin"
    d.mkdir(exist_ok=True)
    (d / "journalctl").write_text('#!/bin/sh\necho "$*" >> "$LOG"\nprintf "%s" "$JOURNAL"\n')
    (d / "ip").write_text('#!/bin/sh\nprintf "%s" "$LISTING"\n')
    for f in ("journalctl", "ip"):
        (d / f).chmod(0o755)
    env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", JOURNAL=journal, LISTING=listing,
               LOG=str(tmp_path / "journalctl.log"))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_dantesync_join_verdict "$2"\necho END',
                        "harness", str(LIB), ip], capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode == 0 and r.stdout.strip().endswith("END"), r.stdout + r.stderr
    log = (tmp_path / "journalctl.log").read_text() if (tmp_path / "journalctl.log").exists() else ""
    return r.stdout.strip().splitlines()[0], log


def test_ptp_on_the_rig_nic_is_ok(tmp_path):
    v, _ = _verdict(tmp_path, JOIN_RIG + "\n")
    assert v == "ok:enp56s0"


def test_ptp_on_the_vban_leg_fails_naming_both_nics(tmp_path):
    v, _ = _verdict(tmp_path, JOIN_VBAN + "\n")
    assert v == "wrong:enx00e04c680185:enp56s0"


def test_no_join_line_and_no_rig_nic_are_their_own_verdicts(tmp_path):
    assert _verdict(tmp_path, "")[0] == "nojoin"
    assert _verdict(tmp_path, JOIN_RIG + "\n", listing="1: lo    inet 127.0.0.1/8 scope host lo\n")[0] == "norig"
    assert _verdict(tmp_path, JOIN_RIG + "\n", ip="")[0] == "norig"


def test_a_look_alike_address_is_not_the_rig_nic(tmp_path):
    listing = ("2: eth9    inet 10.77.9.20/24 scope global eth9\n"
               "3: eth8    inet 110.77.9.204/24 scope global eth8\n")
    assert _verdict(tmp_path, JOIN_RIG + "\n", listing=listing)[0] == "norig"


def test_the_journal_read_is_bounded_to_the_newest_match_of_this_boot(tmp_path):
    _, log = _verdict(tmp_path, JOIN_RIG + "\n")
    args = log.split()
    assert "-b" in args and "-n" in args and args[args.index("-n") + 1] == "1", log
    assert "-g" in args or any(a.startswith("--grep") for a in args), log
    assert "dsync" in log


def test_verify_strih_grades_it_before_item_6c():
    v = VERIFY.read_text()
    d6 = v.index("# 6d) dantesync PTP NIC")
    assert v.index("# 6b) dantesync ROLE") < d6 < v.index("# 6c) dantesync UNIT content")
    block = v[d6:v.index("# 6c) dantesync UNIT content")]
    assert 'strih_dantesync_join_verdict "$(strih_lx_ip)"' in block
    assert "(dantesync-nic)" in block and 'bad "' in block
