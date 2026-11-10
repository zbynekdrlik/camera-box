"""issue 1495 -- the declared network layout of a Windows OBS box (scripts/lib/win-net-layout.sh).

10.11.2026: RESOLUME-SNV's Dante leg moved to the VLAN-70 virtual adapter "Dante70" on a new Realtek
USB 2.5GbE NIC (dstack#155). A swapped NIC / USB port, a DHCP Dante address or a second default
route would break Dante or the LAN silently, so the owner asked for a gate row. This suite pins:

  * the live 10.11 gather (the real gather program, run read-only over the fleet MCP) grades OK;
  * every declared fact, broken one at a time, grades DRIFT and NAMES the failed sub-check;
  * a missing / incomplete section is UNKNOWN, never OK; an undeclared host is OK "not declared";
  * the section is in the ONE baseline gather, reads only, and flows to win-baseline-check.sh and
    the report-only version-integrity rows.
"""
import os
import pathlib
import re
import subprocess

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "obs-box-baseline-win.sh"
LIVE = pathlib.Path(__file__).resolve().parent / "fixtures" / "win_net_layout_1495" / "resolume_live_2026-11-10.txt"


def _bash(script, env=None):
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=full_env,
                          timeout=60)


def _grade(path):
    # Graded exactly as its consumers run it: under `set -euo pipefail`.
    r = _bash(f'set -euo pipefail; . "{LIB}"; rc=0; win_net_layout_grade "{path}" || rc=$?; echo "rc=$rc"')
    assert "unbound variable" not in r.stderr, r.stderr
    lines = r.stdout.splitlines()
    rc = int(lines[-1].split("=", 1)[1])
    row = lines[0]
    assert row.startswith("net_layout "), r.stdout + r.stderr
    verdict = row.split()[1]
    assert verdict in ("OK", "DRIFT", "UNKNOWN"), row
    return rc, verdict, row


def _variant(tmp_path, old, new):
    text = LIVE.read_text()
    assert old in text, old
    p = tmp_path / "variant.txt"
    p.write_text(text.replace(old, new, 1))
    return p


def test_live_resolume_layout_is_ok():
    rc, verdict, row = _grade(LIVE)
    assert (rc, verdict) == (0, "OK"), row
    assert "Dante70" in row and "10.77.9.201" in row


def test_live_capture_with_crlf_is_ok(tmp_path):
    p = tmp_path / "crlf.txt"
    p.write_text(LIVE.read_text().replace("\n", "\r\n"))
    rc, verdict, row = _grade(p)
    assert (rc, verdict) == (0, "OK"), row


BREAKS = [
    # (old line fragment, replacement, sub-check named in the DRIFT detail)
    ("phys=Ethernet 4|Realtek Gaming USB 2.5GbE Family Controller|Up|2.5 Gbps",
     "phys=Ethernet 4|Realtek Gaming USB 2.5GbE Family Controller|Up|1 Gbps", "lan_speed"),
    ("USB\\VID_0BDA&PID_8156", "USB\\VID_0B95&PID_1790", "lan_nic_usb"),
    ("phys=Ethernet 4|Realtek Gaming USB 2.5GbE Family Controller|Up",
     "phys=Ethernet 4|Realtek USB GbE Family Controller|Up", "lan_nic:"),
    ("phys=Ethernet 4|Realtek Gaming USB 2.5GbE Family Controller|Up",
     "phys=Ethernet 4|Realtek Gaming USB 2.5GbE Family Controller|Disconnected", "lan_nic_up"),
    ("I225-V|Disabled|0 bps", "I225-V|Up|2.5 Gbps", "extra_nic_up"),
    ("AX200 160MHz|Disabled", "AX200 160MHz|Disconnected", "disabled_nic"),
    ("virt=Dante70|Realtek Virtual Adapter|Up", "virt=Dante70|Realtek Virtual Adapter|Disconnected", "dante_up"),
    ("virt=Dante70|Realtek Virtual Adapter|Up\n", "", "dante_adapter"),
    ("ip=Dante70|10.77.7.107|24|Manual", "ip=Dante70|10.77.7.107|24|Dhcp", "dante_ip"),
    ("ip=Dante70|10.77.7.107|24|Manual", "ip=Dante70|10.77.7.199|24|Manual", "dante_ip"),
    ("ip=Ethernet 4|10.77.9.201|23|Dhcp", "ip=Ethernet 4|10.77.9.150|23|Dhcp", "lan_ip"),
    ("defroute=Ethernet 4|10.77.8.1|0", "defroute=Ethernet 4|10.77.8.1|0\ndefroute=Dante70|10.77.7.1|0",
     "default_route"),
    ("defroute=Ethernet 4|10.77.8.1|0\n", "", "default_route"),
    ("ifmetric=Dante70|900|", "ifmetric=Dante70|25|", "dante_metric"),
    ("ifmetric=Ethernet 4|20|", "ifmetric=Ethernet 4|35|", "lan_metric"),
    ("vlan=Dante70|70", "vlan=Dante70|0", "dante_vlan"),
    ("dnsreg=Dante70|False", "dnsreg=Dante70|True", "dante_dns_register"),
    ("netbios=Dante70|2", "netbios=Dante70|0", "dante_netbios"),
    ("binding=ms_server|False", "binding=ms_server|True", "dante_binding: ms_server"),
    ("binding=ms_tcpip6|False\n", "", "dante_binding: ms_tcpip6"),
    ("ndi_allowed=10.77.9.201", "ndi_allowed=10.77.9.201,10.77.7.107", "ndi_allowed"),
    ("ndi_allowed=10.77.9.201", "ndi_allowed_unread=file not found", "ndi_allowed"),
]


@pytest.mark.parametrize("old,new,named", BREAKS, ids=[b[2] + str(i) for i, b in enumerate(BREAKS)])
def test_each_broken_fact_is_drift_and_named(tmp_path, old, new, named):
    rc, verdict, row = _grade(_variant(tmp_path, old, new))
    assert (rc, verdict) == (20, "DRIFT"), row
    assert named in row, row


def test_two_broken_facts_both_named(tmp_path):
    p = _variant(tmp_path, "vlan=Dante70|70", "vlan=Dante70|0")
    text = p.read_text().replace("netbios=Dante70|2", "netbios=Dante70|1")
    p.write_text(text)
    rc, verdict, row = _grade(p)
    assert verdict == "DRIFT" and "dante_vlan" in row and "dante_netbios" in row, row


def test_incomplete_section_is_unknown(tmp_path):
    text = LIVE.read_text()
    cut = text.index("ndi_allowed=")
    p = tmp_path / "cut.txt"
    p.write_text(text[:cut])
    rc, verdict, row = _grade(p)
    assert (rc, verdict) == (11, "UNKNOWN"), row


def test_missing_section_is_unknown(tmp_path):
    text = LIVE.read_text()
    p = tmp_path / "nosec.txt"
    p.write_text(text[: text.index("==WINBASELINE-SECTION== net_layout")] + "==WINBASELINE-END==\n")
    rc, verdict, row = _grade(p)
    assert (rc, verdict) == (11, "UNKNOWN"), row


def test_unreadable_file_is_unknown(tmp_path):
    rc, verdict, row = _grade(tmp_path / "absent.txt")
    assert (rc, verdict) == (11, "UNKNOWN"), row


def test_undeclared_host_is_ok_not_declared(tmp_path):
    p = _variant(tmp_path, "host=RESOLUME-SNV", "host=STREAM")
    rc, verdict, row = _grade(p)
    assert (rc, verdict) == (0, "OK") and "not declared" in row, row


def test_gather_section_is_in_the_one_gather_and_reads_only():
    r = _bash(f'set -euo pipefail; . "{LIB}"; win_baseline_gather_ps1')
    out = r.stdout
    sec = out[out.index("Write-WbSection 'net_layout'"):out.index('Write-Output "==WINBASELINE-END=="')]
    verbs = set(re.findall(r"\b([A-Z][a-z]+)-Net|\b([A-Z][a-z]+)-Cim|\b([A-Z][a-z]+)-Dns", sec))
    used = {v for t in verbs for v in t if v}
    assert used <= {"Get"}, used
    assert not re.search(r"(?i)\b(Set|New|Remove|Enable|Disable|Rename|Restart)-", sec), sec
    assert out.rstrip().endswith("exit 0")


def test_check_prints_the_net_layout_row_and_folds_its_drift(tmp_path):
    bad = _variant(tmp_path, "vlan=Dante70|70", "vlan=Dante70|0")
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f'#!/bin/bash\ncp "{bad}" "$3"\n')
    fetch.chmod(0o755)
    r = _bash(f'"{SCRIPTS}/win-baseline-check.sh" --box resolume-snv',
              env={"WIN_BASELINE_FETCH_CMD": str(fetch), "OBS_FLEET_HOME": "resolume-snv"})
    assert "box=resolume-snv item=net_layout verdict=DRIFT" in r.stdout, r.stdout + r.stderr
    assert re.search(r"^box=resolume-snv win_baseline=DRIFT drift=wer_dontshowui,net_layout$", r.stdout, re.M), r.stdout
    assert r.returncode == 20


def test_check_live_layout_ok_row(tmp_path):
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f'#!/bin/bash\ncp "{LIVE}" "$3"\n')
    fetch.chmod(0o755)
    r = _bash(f'"{SCRIPTS}/win-baseline-check.sh" --box resolume-snv',
              env={"WIN_BASELINE_FETCH_CMD": str(fetch), "OBS_FLEET_HOME": "resolume-snv"})
    assert "box=resolume-snv item=net_layout verdict=OK" in r.stdout, r.stdout + r.stderr
    assert re.search(r"^box=resolume-snv win_baseline=DRIFT drift=wer_dontshowui$", r.stdout, re.M), r.stdout


def test_gate_report_rows_carry_the_net_layout_row():
    r = _bash(f'set -euo pipefail; . "{LIB}"; win_baseline_report_rows resolume-snv="{LIVE}"')
    assert re.search(r"resolume-snv\s+win_baseline net_layout\s+OK", r.stdout), r.stdout
    assert r.returncode == 0
