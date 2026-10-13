"""issue 1372 (owner 13.10.2026: "ako to ze na iem pc nei je upgradovany dantesync?!!!") -- a dantesync node
outside the fleet table is NAMED by the tooling: scripts/dantesync_fleet_scan.py lists every :8898 node of
the declared rig subnets whose address is not a fleet target, and `dantesync-version-gate.sh --fleet`
reports each one in a report-only section (an ALARM line, never a gate failure)."""
import importlib.util
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
_spec = importlib.util.spec_from_file_location("dantesync_fleet_scan_1372", SCRIPTS / "dantesync_fleet_scan.py")
scan = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(scan)


def test_hosts_of_the_declared_nets():
    hosts = scan.hosts(["10.77.9", "10.76.8"])
    assert len(hosts) == 2 * 254 and hosts[0] == "10.77.9.1" and hosts[-1] == "10.76.8.254"


def test_unknown_nodes_are_the_answering_addresses_outside_the_known_set():
    answering = {"10.77.9.231": "iem-snv.lan", "10.77.9.62": "cam2.lan", "10.77.9.205": "companion-snv.lan"}
    rows = scan.unknown_nodes(["10.77.9.62", "10.77.9.231", "10.77.9.205", "10.77.9.9"],
                              known={"10.77.9.62"}, probe=lambda ip: answering.get(ip))
    assert rows == [("10.77.9.205", "companion-snv.lan"), ("10.77.9.231", "iem-snv.lan")]


def test_nothing_unknown_is_an_empty_list():
    assert scan.unknown_nodes(["10.77.9.62"], known={"10.77.9.62"}, probe=lambda ip: "cam2.lan") == []


def test_the_scan_nets_are_declared_once_in_the_fleet_lib():
    lib = (SCRIPTS / "lib" / "dantesync-fleet.sh").read_text()
    m = re.search(r'^DANTESYNC_FLEET_SCAN_NETS="\$\{DANTESYNC_FLEET_SCAN_NETS:-([0-9. ]+)\}"', lib, re.M)
    assert m and m.group(1).split() == ["10.77.9", "10.77.8", "10.77.7", "10.76.9", "10.76.8"]


def _gate(tmp_path, scan_out):
    b = tmp_path / "bin"
    b.mkdir(parents=True)
    (b / "sshpass").write_text("#!/usr/bin/env bash\nexit 255\n")
    (b / "dantesync").write_text("#!/usr/bin/env bash\necho dantesync 0.0.0\n")
    fake = tmp_path / "fake-scan"
    fake.write_text("#!/usr/bin/env bash\nprintf '%s' \"$FAKE_SCAN_OUT\"\necho \"$@\" > \"$FAKE_SCAN_ARGS\"\n")
    for f in list(b.iterdir()) + [fake]:
        f.chmod(0o755)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "OBS_FLEET"))}
    env.update({"PATH": f"{b}:/usr/bin:/bin", "OBS_FLEET_HOME": "", "DANTESYNC_FLEET_SCAN_CMD": str(fake),
                "FAKE_SCAN_OUT": scan_out, "FAKE_SCAN_ARGS": str(tmp_path / "args"),
                "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env"), "DANTESYNC_NEWEST_RELEASE": "0.0.0",
                "DANTESYNC_VERSION_GATE_SSH_PASS": "pw"})
    r = subprocess.run(["bash", str(SCRIPTS / "dantesync-version-gate.sh"), "--pin", "0.0.0", "--fleet-file",
                        str(fleet), "--fleet"], capture_output=True, text=True, env=env, timeout=120)
    args = (tmp_path / "args").read_text() if (tmp_path / "args").exists() else ""
    return r, args


def test_the_gate_names_a_node_outside_the_fleet_and_never_fails_on_it(tmp_path):
    clean, _ = _gate(tmp_path / "a", "")
    r, args = _gate(tmp_path / "b", "UNKNOWN 10.77.9.231 iem-snv.lan\n")
    assert "-- dantesync nodes outside the fleet table (report-only, ALARM never blocks) --" in r.stdout
    assert "10.77.9.231" in r.stdout and "iem-snv.lan" in r.stdout and "OUTSIDE-FLEET" in r.stdout
    assert "DANTESYNC-FLEET ALARM" in r.stderr and "10.77.9.231" in r.stderr
    assert r.returncode == clean.returncode, "report-only: the unknown node never changes the gate result"
    assert "--known" in args and "10.77.9.202" in args  # the fleet targets are passed as known
    # every fleet row is known, also one the gate SKIPPED this run (a `reach` node away, live 13.10.2026:
    # resolume-pp read OUTSIDE-FLEET while it was skipped as unreachable)
    for host in ("resolume-pp.lan", "strih-pp.lan", "iem-snv.lan", "companion-snv.lan"):
        assert host in args.split(), host


def test_the_gate_reports_none_when_every_node_is_in_the_fleet(tmp_path):
    r, _ = _gate(tmp_path, "")
    assert "-- dantesync nodes outside the fleet table" in r.stdout
    assert "  none" in r.stdout and "OUTSIDE-FLEET" not in r.stdout
