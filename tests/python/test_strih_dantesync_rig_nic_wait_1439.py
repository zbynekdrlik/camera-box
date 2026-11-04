"""Issue 1439 finding 1.11.2026: dantesync must start only once the rig NIC holds its address.

WHY: stream-snv booted at 08:31:45 during the SNV production. Its VBAN-leg USB NIC (10.77.7.204/32,
the issue-1439 audio-VLAN leg) got its static address before NetworkManager brought the rig NIC up.
dantesync 1.20.0 on Linux joins PTP on the FIRST IPv4 interface it lists and never re-selects (its
`--interface` flag is parsed but unused), so it joined the audio VLAN, where PTP never arrives, and
ran without the Dante clock for 4 h 17 min. On every Linux OBS box the rig NIC has the lowest
ifindex (stream-snv enp56s0 = 2 < enx = 3; strih-snv enp7s0 = 4 < its VLAN enp7s0.70 = 5), so a
dantesync that starts only after the rig IP is assigned picks the rig NIC.

What this pins:
  * `strih_dantesync_unit_text ROLE ARGS RIG_IP` adds ONE ExecStartPre that waits (bounded) until
    `ip -o -4 addr show` lists RIG_IP; a timeout fails the start and Restart=always tries again, so
    dantesync never starts on another NIC;
  * the wait carries no `$` (systemd would expand it) and really waits: run against a fake `ip`;
  * a malformed RIG_IP is refused and emits nothing (the unit is the fail-closed shape);
  * setup-strih step 2 and verify-strih item 6c pass the box's own IP fact (`strih_lx_ip`), so the
    unit verify grades is the one setup installs.

Tier-0: bash + pytest only.
"""
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"


def _unit(role, args, *extra):
    """Run strih_dantesync_unit_text with the given positional arguments (never inside shell text)."""
    script = 'set -euo pipefail\n. "$1"\nshift\nstrih_dantesync_unit_text "$@"'
    return subprocess.run(["bash", "-c", script, "harness", str(LIB), role, args, *extra],
                          capture_output=True, text=True, cwd=str(REPO), timeout=60)


def _wait_line(unit_text):
    lines = [l for l in unit_text.splitlines() if l.startswith("ExecStartPre=")]
    assert len(lines) == 1, unit_text
    return lines[0]


def test_the_unit_waits_for_the_rig_ip_before_dantesync_starts():
    r = _unit("server", "", "10.77.9.204")
    assert r.returncode == 0, r.stderr
    out = r.stdout
    wait = _wait_line(out)
    assert "10.77.9.204/" in wait, wait
    assert out.index("ExecStartPre=") < out.index("ExecStart=/usr/local/bin/dsync"), out
    assert "timeout" in wait, "the wait is bounded, a timeout fails the start: " + wait
    assert "$" not in wait, "systemd expands $ in Exec lines: " + wait
    # the rest of the unit keeps its shape
    for part in ("Type=simple", "Restart=always", "RestartSec=5", "WantedBy=multi-user.target"):
        assert part in out, out


def test_a_client_unit_waits_too():
    r = _unit("client", "--ntp-server strih-snv.lan", "10.77.9.204")
    assert r.returncode == 0, r.stderr
    assert "10.77.9.204/" in _wait_line(r.stdout)
    assert "ExecStart=/usr/local/bin/dsync --ntp-server strih-snv.lan" in r.stdout


def test_a_malformed_rig_ip_is_refused_and_emits_nothing():
    for bad in ("", "10.77.9", "10.77.9.204; reboot", "10.77.9.204'", "a.b.c.d", "10.77.9.204/23"):
        r = _unit("server", "", bad)
        assert r.returncode != 0, bad
        assert r.stdout.strip() == "", (bad, r.stdout)


def _shell_of(wait):
    """The `/bin/sh -c '...'` script of the wait line."""
    m = re.search(r"/bin/sh -c '([^']*)'", wait)
    assert m, wait
    return m.group(1)


def _fake_ip(tmp_path, listing):
    d = tmp_path / "bin"
    d.mkdir()
    ip = d / "ip"
    ip.write_text("#!/bin/sh\nprintf '%s\\n' \"$LISTING\"\n")
    ip.chmod(0o755)
    return dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", LISTING=listing)


def test_the_wait_returns_once_the_rig_ip_is_listed(tmp_path):
    script = _shell_of(_wait_line(_unit("server", "", "10.77.9.204").stdout))
    env = _fake_ip(tmp_path, "2: enp56s0    inet 10.77.9.204/23 brd 10.77.9.255 scope global enp56s0")
    r = subprocess.run(["timeout", "5", "/bin/sh", "-c", script], env=env, timeout=10)
    assert r.returncode == 0


def test_the_wait_keeps_waiting_while_only_the_vban_leg_is_up(tmp_path):
    script = _shell_of(_wait_line(_unit("server", "", "10.77.9.204").stdout))
    # the boot of 1.11.2026: only the audio-VLAN leg holds an address; a prefix of the rig IP
    # (10.77.9.20) and the same digits elsewhere on the line must not count.
    env = _fake_ip(tmp_path, "3: enx00e04c680185    inet 10.77.7.204/32 scope global enx00e04c680185\n"
                             "4: dummy0    inet 10.77.9.20/24 scope global dummy0\n"
                             "5: dummy1    inet 110.77.9.204/24 scope global dummy1")
    r = subprocess.run(["timeout", "2", "/bin/sh", "-c", script], env=env, timeout=10)
    assert r.returncode == 124, "the wait must not return without the rig IP"


def test_setup_and_verify_pass_the_box_ip_fact():
    for path in (SETUP, VERIFY):
        text = path.read_text()
        calls = [l for l in text.splitlines() if "strih_dantesync_unit_text \"$" in l]
        assert calls, path
        for line in calls:
            assert '"$(strih_lx_ip)"' in line, (path, line)
