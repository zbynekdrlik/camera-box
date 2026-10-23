"""issue 1439 follow-up -- the SWITCH side of the VBAN-only legs in the netcfg baseline.

The 23.10.2026 changes on the CRS310s (foh2_video `sfp-sfpplus2`: VLAN 89 untagged via PVID +
VLAN 70 tagged, `frame-types=admit-all`; the QoS profile `dante-audio` DSCP 46 -> traffic class 5;
the `new-qos-profile=dante-audio` switch rule) lived only on the switches. `scripts/netcfg-audit.sh`
now gathers, per CRS310, the bridge VLAN table, the bridge ports, the QoS profiles and the switch
rules (`print detail`, one ssh call per switch), captures them into `scripts/netcfg-baseline.json`
and reports a missing/changed membership, PVID, frame-types, profile or rule as DRIFT naming the
switch + port.

The fixtures are the REAL output of that one ssh call on the four switches (23.10.2026, CRLF,
80-column wrap: the rule comment is split mid-word with no indent). The pure parse/check core is
`scripts/netcfg_bridge_qos.py`; the orchestrator is exercised end to end with a fake `sshpass`
that serves the fixtures. No cargo -- Tier-0 pytest.
"""
import json
import os
import pathlib
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import netcfg_bridge_qos as nbq  # noqa: E402

FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "netcfg_bridge_qos_1439"
SWITCHES = {
    "foh1_audio": "10.77.9.2",
    "stage_av": "10.77.9.3",
    "foh1_video": "10.77.9.4",
    "foh2_video": "10.77.9.5",
}


def _raw(node):
    # bytes -> str keeps the switch's CRLF (read_text() would translate it to LF)
    return (FIX / f"{node}.txt").read_bytes().decode("ascii")


def _facet(node, raw=None):
    return nbq.facet_from_raw(_raw(node) if raw is None else raw)


# --------------------------------------------------------------------------- parse


def test_fixtures_are_the_real_crlf_wrapped_output():
    raw = _raw("foh2_video")
    assert "\r\n" in raw
    # the rule comment is wrapped mid-word at column 80 with no indent on the continuation
    assert "(dante-audi\r\no)" in raw
    assert raw.rstrip().endswith("=== end")


def test_gather_cmd_reads_the_five_tables_with_an_end_marker():
    cmd = nbq.GATHER_CMD
    for part in (
        "/interface bridge print detail",
        "/interface bridge vlan print detail",
        "/interface bridge port print detail",
        "/interface ethernet switch qos profile print detail",
        "/interface ethernet switch rule print detail",
    ):
        assert part in cmd
    assert cmd.rstrip().endswith(':put "=== end"')
    # read-only: no RouterOS write verb anywhere in the command
    for verb in ("set ", "add ", "remove ", "enable ", "disable ", "/export"):
        assert verb not in cmd


def test_parse_static_vlan_entries_only_with_sorted_members():
    f = _facet("foh2_video")
    assert f["bridge_vlan"] == {
        "70": {"tagged": ["bridge", "sfp-sfpplus2"], "untagged": [], "disabled": "no"},
        "89": {"tagged": ["bridge"], "untagged": [], "disabled": "no"},
    }


def test_parse_bridge_ports_pvid_and_frame_types():
    ports = _facet("foh2_video")["bridge_port"]
    assert set(ports) == {f"ether{i}" for i in range(1, 9)} | {"sfp-sfpplus1", "sfp-sfpplus2"}
    assert ports["sfp-sfpplus2"] == {
        "pvid": "89", "frame-types": "admit-all", "ingress-filtering": "yes",
        "mvrp-applicant-state": "non-participant", "mvrp-registrar-state": "normal",
        "disabled": "no",
    }
    assert ports["sfp-sfpplus1"]["frame-types"] == "admit-only-vlan-tagged"
    # the trunk joins MVRP (it carries VLAN 70/89 only through MVRP-dynamic entries)
    assert ports["sfp-sfpplus1"]["mvrp-applicant-state"] == "normal-participant"
    assert ports["ether7"]["pvid"] == "70"
    # an INACTIVE (no link) port is still a configured bridge port
    assert ports["ether5"]["pvid"] == "89"


def test_parse_qos_profiles_without_the_hw_id():
    prof = _facet("foh2_video")["qos_profile"]
    assert prof["dante-audio"] == {
        "pcp": "5", "dscp": "46", "traffic-class": "5", "automap": "yes", "disabled": "no",
    }
    assert set(prof) == {"default", "dante-ptp", "dante-audio", "dante-low"}


def test_parse_static_switch_rule_keyed_by_its_wrapped_comment():
    rules = _facet("foh2_video")["switch_rule"]
    key = "camera-box issue 1439: VBAN from mbc -> strict-priority TC5 (dante-audio)"
    assert list(rules) == [key]  # the dynamic romon rule is runtime state, not baselined
    r = rules[key]
    assert r["ports"] == "ether7"
    assert r["protocol"] == "udp"
    assert r["dst-port"] == "6980"
    assert r["new-qos-profile"] == "dante-audio"
    assert r["disabled"] == "no"
    assert r["invalid"] == "no"


def test_parse_the_bridge_vlan_filtering_and_mvrp_switches():
    for node in SWITCHES:
        assert _facet(node)["bridge"] == {
            "bridge": {
                "vlan-filtering": "yes", "mvrp": "yes", "pvid": "1",
                "frame-types": "admit-only-vlan-tagged", "ingress-filtering": "yes",
                "disabled": "no",
            }
        }, node


def test_a_wrapped_flags_legend_does_not_drop_entry_zero():
    raw = _raw("foh2_video")
    legend = "Flags: X - DISABLED, I - INACTIVE; D - DYNAMIC; H - HW-OFFLOAD; Y - MANAGED \r\n"
    assert raw.count(legend) == 1
    wrapped = raw.replace(legend, "Flags: X - DISABLED, I - INACTIVE; D - DYNAMIC; H - HW-OFFLOAD; Y - MANAGED; \r\nZ - SOMETHING-NEW \r\n")
    assert nbq.facet_from_raw(wrapped) == _facet("foh2_video")


def test_an_unparseable_read_is_none_never_a_removed_entry():
    raw = _raw("foh2_video")
    # a block that does not start with an index header
    i = raw.index(" 1   H  interface=ether2")
    assert nbq.facet_from_raw(raw[:i] + "something RouterOS printed\r\n\r\n" + raw[i:]) is None
    # an entry without its key property
    assert nbq.facet_from_raw(raw.replace("interface=ether1 ", "", 1)) is None
    # an unbalanced quote
    assert nbq.facet_from_raw(raw.replace('name="dante-low"', 'name="dante-low', 1)) is None


def test_two_rules_with_the_same_comment_are_both_kept():
    raw = _raw("foh2_video")
    rule = raw[raw.index(" 1    ;;; camera-box issue 1439"): raw.index("=== end")]
    second = rule.replace(" 1    ;;;", " 2    ;;;", 1).replace("ports=ether7", "ports=ether8")
    rules = nbq.facet_from_raw(raw.replace("=== end", second + "=== end"))["switch_rule"]
    assert len(rules) == 2
    assert sorted(r["ports"] for r in rules.values()) == ["ether7", "ether8"]


def test_regrouped_vlan_ids_compare_per_vlan():
    base = _facet("foh2_video")
    live = json.loads(json.dumps(base))
    live["bridge_vlan"] = {
        "70": {"tagged": ["bridge", "sfp-sfpplus2"], "untagged": [], "disabled": "no"},
        "89": {"tagged": ["bridge"], "untagged": [], "disabled": "no"},
    }
    grouped = json.loads(json.dumps(base))
    grouped["bridge_vlan"] = {
        "70,89": {"tagged": ["bridge"], "untagged": [], "disabled": "no"},
        "70": {"tagged": ["sfp-sfpplus2"], "untagged": [], "disabled": "no"},
    }
    assert nbq.check_node("foh2_video", grouped, live) == []
    ranged = json.loads(json.dumps(base))
    ranged["bridge_vlan"] = {
        "69-71": {"tagged": ["bridge"], "untagged": [], "disabled": "no"},
    }
    msgs = [m for _, h, m in nbq.check_node("foh2_video", ranged, live) if h]
    assert any("VLAN 69" in m for m in msgs) and any("VLAN 71" in m for m in msgs)


def test_a_switch_with_no_static_rule_has_an_empty_rule_table():
    assert _facet("foh1_audio")["switch_rule"] == {}
    assert _facet("foh1_audio")["qos_profile"]["dante-audio"]["dscp"] == "46"


def test_incomplete_read_is_none_never_an_empty_table():
    raw = _raw("foh2_video").replace("=== end", "")
    assert nbq.facet_from_raw(raw) is None
    assert nbq.facet_from_raw("") is None
    # a RouterOS error aborts the script before the end marker
    assert nbq.facet_from_raw("bad command name detail (line 1 column 30)\r\n") is None
    # the end marker alone is not enough: every one of the four tables must have been printed
    raw = _raw("foh2_video")
    no_rules = raw[: raw.index("=== switch-rule")] + "=== end\r\n"
    assert nbq.facet_from_raw(no_rules) is None


# --------------------------------------------------------------------------- check


def _check(node, live_raw):
    return nbq.check_node(node, _facet(node), nbq.facet_from_raw(live_raw))


def _hard(findings):
    return [f for f in findings if f[1]]


def test_identical_live_is_clean():
    for node in SWITCHES:
        assert nbq.check_node(node, _facet(node), _facet(node)) == []


def test_removed_vlan70_tag_on_sfp_sfpplus2_is_drift_naming_switch_and_port():
    live = _raw("foh2_video").replace("tagged=bridge,sfp-sfpplus2", "tagged=bridge")
    hard = _hard(_check("foh2_video", live))
    assert len(hard) == 1
    status, is_hard, msg = hard[0]
    assert status == "DRIFT" and is_hard
    assert "foh2_video sfp-sfpplus2" in msg
    assert "VLAN 70" in msg and "tagged" in msg


def test_removed_static_vlan_entry_is_drift_for_every_member():
    raw = _raw("foh2_video")
    start = raw.index(" 1    bridge=bridge vlan-ids=70")
    end = raw.index(" 2  D ;;; added by mvrp")
    hard = _hard(_check("foh2_video", raw[:start] + raw[end:]))
    msgs = " | ".join(m for _, _, m in hard)
    assert "foh2_video sfp-sfpplus2" in msgs and "foh2_video bridge" in msgs
    assert all(s == "DRIFT" for s, _, _ in hard)


def test_tagged_to_untagged_is_one_drift_line_per_port():
    live = _raw("foh2_video").replace(
        'tagged=bridge,sfp-sfpplus2 untagged=""', 'tagged=bridge untagged=sfp-sfpplus2'
    )
    hard = _hard(_check("foh2_video", live))
    assert len(hard) == 1
    assert "baseline=tagged live=untagged" in hard[0][2]


def test_changed_pvid_and_frame_types_are_drift():
    raw = _raw("foh2_video")
    i = raw.index("interface=sfp-sfpplus2")
    tail = raw[i:].replace("pvid=89", "pvid=1", 1).replace(
        "frame-types=admit-all", "frame-types=admit-only-vlan-tagged", 1
    )
    hard = _hard(_check("foh2_video", raw[:i] + tail))
    msgs = [m for _, _, m in hard]
    assert len(msgs) == 2
    assert any("foh2_video sfp-sfpplus2" in m and "pvid" in m and "'89'" in m for m in msgs)
    assert any("frame-types" in m and "'admit-all'" in m for m in msgs)


def test_bridge_mvrp_or_vlan_filtering_off_is_drift():
    raw = _raw("foh2_video")
    hard = _hard(_check("foh2_video", raw.replace("mvrp=yes", "mvrp=no", 1)))
    assert len(hard) == 1 and "foh2_video bridge: bridge mvrp" in hard[0][2]
    hard = _hard(_check("foh2_video", raw.replace("vlan-filtering=yes", "vlan-filtering=no", 1)))
    assert len(hard) == 1 and "vlan-filtering" in hard[0][2]


def test_trunk_leaving_mvrp_is_drift():
    raw = _raw("foh2_video")
    i = raw.index("interface=sfp-sfpplus1")
    tail = raw[i:].replace("mvrp-applicant-state=normal-participant",
                           "mvrp-applicant-state=non-participant", 1)
    hard = _hard(_check("foh2_video", raw[:i] + tail))
    assert len(hard) == 1
    assert "foh2_video sfp-sfpplus1" in hard[0][2] and "mvrp-applicant-state" in hard[0][2]


def test_a_rule_the_hardware_rejected_is_drift():
    raw = _raw("foh2_video")
    hard = _hard(_check("foh2_video", raw.replace(" 1    ;;; camera-box", " 1 I  ;;; camera-box", 1)))
    assert len(hard) == 1 and "foh2_video ether7" in hard[0][2] and "invalid" in hard[0][2]


def test_removed_bridge_port_is_drift():
    raw = _raw("foh2_video")
    i = raw.index(" 9   H  interface=sfp-sfpplus2")
    j = raw.index("=== qos-profile")
    hard = _hard(_check("foh2_video", raw[:i] + raw[j:]))
    assert len(hard) == 1
    assert "foh2_video sfp-sfpplus2" in hard[0][2] and "bridge port" in hard[0][2]


def test_changed_and_missing_qos_profile_are_drift():
    raw = _raw("foh2_video")
    changed = raw.replace('name="dante-audio" hw-id=2 pcp=5 dscp=46', 'name="dante-audio" hw-id=2 pcp=5 dscp=0')
    hard = _hard(_check("foh2_video", changed))
    assert len(hard) == 1 and "dante-audio" in hard[0][2] and "dscp" in hard[0][2]
    missing = raw.replace(' 2  H name="dante-audio" hw-id=2 pcp=5 dscp=46 traffic-class=5 automap=yes', "")
    hard = _hard(_check("foh2_video", missing))
    assert len(hard) == 1 and "dante-audio" in hard[0][2] and "missing" in hard[0][2]


def test_removed_or_changed_switch_rule_is_drift_naming_its_port():
    raw = _raw("foh2_video")
    gone = raw[: raw.index(" 1    ;;; camera-box issue 1439")] + "=== end\r\n"
    hard = _hard(_check("foh2_video", gone))
    assert len(hard) == 1 and "foh2_video ether7" in hard[0][2] and "missing" in hard[0][2]
    changed = raw.replace("new-qos-profile=dante-audio", "new-qos-profile=default")
    hard = _hard(_check("foh2_video", changed))
    assert len(hard) == 1 and "foh2_video ether7" in hard[0][2] and "new-qos-profile" in hard[0][2]


def test_added_membership_port_profile_rule_are_report_only():
    raw = _raw("foh1_video").replace(
        "vlan-ids=70 tagged=bridge untagged=\"\"", "vlan-ids=70 tagged=bridge,sfp-sfpplus2 untagged=\"\""
    )
    findings = _check("foh1_video", raw)
    assert findings and not _hard(findings)
    assert "foh1_video sfp-sfpplus2" in findings[0][2]
    # a rule present live but not in the baseline (foh1_video has none in the baseline)
    rule = _raw("foh2_video")
    rule = rule[rule.index(" 1    ;;; camera-box issue 1439"): rule.index("=== end")]
    findings = _check("foh1_video", _raw("foh1_video").replace("=== end", rule + "=== end"))
    assert findings and not _hard(findings)


def test_unreadable_live_is_report_only_unknown():
    findings = nbq.check_node("foh2_video", _facet("foh2_video"), None)
    assert [(s, h) for s, h, _ in findings] == [("UNKNOWN", False)]


def test_no_baseline_facet_checks_nothing():
    assert nbq.check_node("foh2_video", None, _facet("foh2_video")) == []


# --------------------------------------------------------------------------- baseline


def test_checked_in_baseline_carries_the_23_10_switch_changes():
    nodes = json.loads((_SCRIPTS / "netcfg-baseline.json").read_text())["nodes"]
    for node in SWITCHES:
        assert nodes[node]["qos_profile"]["dante-audio"]["dscp"] == "46"
        assert nodes[node]["qos_profile"]["dante-audio"]["traffic-class"] == "5"
        assert nodes[node]["bridge_port"], node
        assert nodes[node]["bridge_vlan"], node
        assert nodes[node]["bridge"]["bridge"]["mvrp"] == "yes", node
        assert nodes[node]["bridge"]["bridge"]["vlan-filtering"] == "yes", node
    foh2 = nodes["foh2_video"]
    assert foh2["bridge_vlan"]["70"]["tagged"] == ["bridge", "sfp-sfpplus2"]
    assert foh2["bridge_port"]["sfp-sfpplus2"]["pvid"] == "89"
    assert foh2["bridge_port"]["sfp-sfpplus2"]["frame-types"] == "admit-all"
    (rule,) = foh2["switch_rule"].values()
    assert rule["new-qos-profile"] == "dante-audio" and rule["ports"] == "ether7"
    assert "router_snv" in nodes and "bridge_vlan" not in nodes["router_snv"]


# --------------------------------------------------------------------------- orchestrator


_FAKE_SSHPASS = r'''#!/usr/bin/env python3
"""Fake sshpass for scripts/netcfg-audit.sh: serves the issue-1439 fixtures + a baseline-derived
rest of the gather, keyed by the admin@<ip> argument; the RouterOS command is the last argv."""
import json, os, sys
args = sys.argv[1:]
host = next(a for a in args if a.startswith("admin@"))[len("admin@"):]
cmd = args[-1]
nodes = json.load(open(os.environ["FAKE_BASELINE"]))["nodes"]
node = next((n for n, v in nodes.items() if v.get("ip") == host), None)
if node is None:
    sys.exit(255)
v = nodes[node]
if "=== bridge-vlan" in cmd:
    sys.stdout.write(open(os.path.join(os.environ["FAKE_RAW_DIR"], node + ".txt"), newline="").read())
elif cmd.startswith("/system identity"):
    print("  name: %s" % v["identity"])
    print("  version: %s (stable)" % v["ros"])
    print("  board-name: %s" % v["board"])
elif "qos settings print" in cmd:
    print("  shared-buffers: %s" % v["shared_buffers"])
elif cmd.startswith(":foreach"):
    for p, pv in v.get("port", {}).items():
        print("PORT|%s|comment=%s|running=%s|rate=%s|fullduplex=%s|dq1=%s|fcs=%s" % (
            p, pv["comment"], pv["running"], pv["rate"], pv["fullduplex"], pv["dq1"], pv["fcs"]))
elif "print stats where name=" in cmd:
    print("  tx-drop-queue1-packet: 0")
'''


@pytest.fixture
def orch(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    fake = bindir / "sshpass"
    fake.write_text(_FAKE_SSHPASS)
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    raw_dir = tmp_path / "raw"
    raw_dir.mkdir()
    for node in SWITCHES:
        (raw_dir / f"{node}.txt").write_bytes(_raw(node).encode("ascii"))
    baseline = tmp_path / "baseline.json"
    baseline.write_text((_SCRIPTS / "netcfg-baseline.json").read_text())
    env = dict(os.environ)
    env.update(
        PATH=f"{bindir}:{env['PATH']}",
        NETCFG_SWITCH_PW="fake-pw",
        NETCFG_BASELINE=str(baseline),
        NETCFG_ROUTER="router_snv|192.0.2.1",  # unreachable: the router is never dialled here
        NETCFG_DROP_WINDOW="0",
        NETCFG_DROP_PROBE_PORTS="",
        FAKE_BASELINE=str(baseline),
        FAKE_RAW_DIR=str(raw_dir),
    )

    def run(*args):
        return subprocess.run(
            ["bash", str(_SCRIPTS / "netcfg-audit.sh"), *args],
            env=env, capture_output=True, text=True, timeout=120,
        )

    run.raw_dir = raw_dir
    run.baseline = baseline
    return run


def test_orchestrator_check_is_clean_on_the_live_fixtures(orch):
    r = orch("--check")
    assert r.returncode == 0, r.stderr
    assert "NETCFG-CLEAN" in r.stdout
    assert "[DRIFT]" not in r.stderr


def test_orchestrator_check_pages_a_removed_vlan70_tag(orch):
    f = orch.raw_dir / "foh2_video.txt"
    f.write_text(f.read_text().replace("tagged=bridge,sfp-sfpplus2", "tagged=bridge"))
    r = orch("--check")
    assert r.returncode == 3, r.stderr
    assert "NETCFG-DRIFT" in r.stdout
    assert "[DRIFT] foh2_video sfp-sfpplus2" in r.stderr
    assert "VLAN 70" in r.stderr


def test_orchestrator_check_unreadable_switch_section_never_pages(orch):
    f = orch.raw_dir / "foh2_video.txt"
    f.write_text(f.read_text().replace("=== end", ""))
    r = orch("--check")
    assert r.returncode == 0, r.stderr
    assert "[report-only UNKNOWN] foh2_video" in r.stderr


def test_orchestrator_capture_writes_the_facet_per_switch(orch, tmp_path):
    out = tmp_path / "captured.json"
    r = subprocess.run(
        ["bash", str(_SCRIPTS / "netcfg-audit.sh"), "--capture"],
        env={**_orch_env(orch), "NETCFG_BASELINE": str(out)},
        capture_output=True, text=True, timeout=120,
    )
    assert r.returncode == 0, r.stderr
    nodes = json.loads(out.read_text())["nodes"]
    assert nodes["foh2_video"]["bridge_port"]["sfp-sfpplus2"]["frame-types"] == "admit-all"
    assert nodes["foh2_video"]["bridge_vlan"]["70"]["tagged"] == ["bridge", "sfp-sfpplus2"]
    for node in SWITCHES:
        assert nodes[node]["qos_profile"]["dante-audio"]["traffic-class"] == "5"
    assert "bridge_vlan" not in nodes.get("router_snv", {})


def test_orchestrator_capture_refuses_an_incomplete_switch_read(orch, tmp_path):
    f = orch.raw_dir / "stage_av.txt"
    f.write_text(f.read_text().replace("=== end", ""))
    out = tmp_path / "captured.json"
    r = subprocess.run(
        ["bash", str(_SCRIPTS / "netcfg-audit.sh"), "--capture"],
        env={**_orch_env(orch), "NETCFG_BASELINE": str(out)},
        capture_output=True, text=True, timeout=120,
    )
    assert r.returncode == 2
    assert not out.exists()
    assert "stage_av" in r.stderr


def test_orchestrator_json_names_an_incomplete_switch_and_fails(orch):
    f = orch.raw_dir / "stage_av.txt"
    f.write_text(f.read_text().replace("=== end", ""))
    r = subprocess.run(
        ["bash", str(_SCRIPTS / "netcfg-audit.sh"), "--json"],
        env=_orch_env(orch), capture_output=True, text=True, timeout=120,
    )
    assert r.returncode == 2
    assert "stage_av" in r.stderr
    nodes = json.loads(r.stdout)["nodes"]
    assert "bridge_vlan" in nodes["foh2_video"] and "bridge_vlan" not in nodes["stage_av"]


def test_watchdog_unit_ceiling_covers_the_extra_switch_call():
    unit = (_ROOT / "systemd" / "netcfg-drift-alert-watchdog.service").read_text()
    ceiling = int(unit.split("TimeoutStartSec=", 1)[1].split()[0])
    # worst case with every read hanging to its 8 s bound: router 1 call + 4 switches x 4 calls,
    # plus the designated two-read drop probe (2 x 8 s + the 6 s window) = 158 s, with headroom
    assert ceiling >= 240


def _orch_env(orch):
    env = dict(os.environ)
    env.update(
        PATH=f"{orch.raw_dir.parent / 'bin'}:{env['PATH']}",
        NETCFG_SWITCH_PW="fake-pw",
        NETCFG_ROUTER="router_snv|192.0.2.1",
        NETCFG_DROP_WINDOW="0",
        NETCFG_DROP_PROBE_PORTS="",
        FAKE_BASELINE=str(orch.baseline),
        FAKE_RAW_DIR=str(orch.raw_dir),
    )
    return env


def test_baseline_flat_walk_skips_the_facet_sections():
    src = (_SCRIPTS / "netcfg-audit.sh").read_text()
    body = src.split("_nc_baseline_flat() {", 1)[1].split("\n}\n", 1)[0]
    for sec in ("bridge_vlan", "bridge_port", "qos_profile", "switch_rule"):
        assert sec in body
    assert "if len(prefix) == 1 and k in FACET:" in body
