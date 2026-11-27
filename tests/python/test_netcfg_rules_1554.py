"""issue 1554 -- the venue network rule gate of the rig test, SNV + PP.

Owner 25.11.2026: a network problem or a configuration change at SNV or PP must FAIL the rig test.
The pure rules are scripts/netcfg_rules.py, the venue facts scripts/netcfg-venues.json, the ssh
orchestrator scripts/netcfg-rules.sh.

Fixtures (tests/python/fixtures/netcfg_rules_1554/): rule-relevant EXCERPTS of real reads in the
gather's marker format. snv/ = the live read of 25.11.2026 (key user camerabox) of router_snv and
foh1_audio; pp/ = the mikrotik project's read-only dumps of 25.11.2026 (user snv) of the four PP
devices, the same commands. The exports keep only the paths the rules read, MAC addresses dropped
(the repo is public, the PP config is a third party's). The PP dumps hold the violations the
ticket names: the Dante pool 10.77.7.50-199 on the router, igmp-snooping without a querier on the
switches, the Stage RB4011 without HW offload, firmware reboots pending.
"""
import copy
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import netcfg_rules as nr  # noqa: E402

FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "netcfg_rules_1554"
FACTS = _SCRIPTS / "netcfg-venues.json"
SH = _SCRIPTS / "netcfg-rules.sh"
E2E = _SCRIPTS / "recording-e2e.sh"


def _venue(name, only=None):
    v = nr.load_venue(str(FACTS), name)
    if only:
        v = copy.deepcopy(v)
        v["devices"] = [d for d in v["devices"] if d["name"] in only]
    return v


def _raw(venue, device):
    return (FIX / venue / (device + ".txt")).read_text()


def _raws(venue, names):
    return {n: _raw(venue, n) for n in names}


def _hard(findings):
    return [f for f in findings if f[1]]


def _check(venue, raws, baseline=None, local=None, prev=None):
    return nr.check_venue(venue, raws, baseline or {}, local or {}, prev or {})


def _baseline_of(raws, venue):
    out = {}
    for d in venue["devices"]:
        dev = nr.parse_device(raws[d["name"]], d["reads"])
        out[d["name"]] = nr.export_hash(dev["export"])
    return out


SNV2 = ("router_snv", "foh1_audio")
PP4 = ("pp_router", "pp_foh_a", "pp_foh_b", "pp_stage")


# ------------------------------------------------------------- the real reads
def test_the_snv_reads_pass_every_rule():
    v = _venue("snv", SNV2)
    raws = _raws("snv", SNV2)
    findings, state = _check(v, raws, baseline=_baseline_of(raws, v))
    assert _hard(findings) == [], findings
    assert state["foh1_audio"] == {"sfp-sfpplus1": 1}
    assert state["router_snv"]["ether15"] == 7  # a port that receives BPDUs is a bridge link


def test_the_pp_dumps_fail_on_the_violations_the_ticket_names():
    v = _venue("pp")
    raws = _raws("pp", PP4)
    findings, _ = _check(v, raws, baseline=_baseline_of(raws, v))
    hard = [m for s, h, m in findings if h]
    assert any(m.startswith("pp_router: Dante DHCP pool pool_dhcp_70 10.77.7.50-10.77.7.199") for m in hard), hard
    for sw in ("pp_foh_a", "pp_foh_b", "pp_stage"):
        assert any(m.startswith(sw + ": igmp-snooping=yes with no active IGMP querier") for m in hard), (sw, hard)
    assert len(hard) == 4, hard
    soft = [(s, m) for s, h, m in findings if not h]
    assert sum(1 for s, m in soft if "reboot pending" in m) == 4, soft
    # the Stage RB4011 bridges in the CPU: reported, never a FAIL (the facts mark it cpu_bridge)
    assert any(s == "WARN" and m.startswith("pp_stage: bridge port ether2::70::dante is not HW-offloaded")
               for s, m in soft), soft
    # the FOH-A PTP mirror and the QoS are right in the dump
    assert not any("PTP" in m or "QoS" in m or "trust-l3" in m for m in hard), hard


def test_the_router_without_a_monitor_read_never_passes_its_igmp_silently():
    v = _venue("pp", ("pp_router",))
    findings, _ = _check(v, _raws("pp", ("pp_router",)))
    assert ("UNKNOWN", False,
            "pp_router: igmp-snooping=yes but no bridge monitor read, the querier is not checked here") in findings


# ------------------------------------------------------------- rule 2 IGMP
def test_igmp_snooping_on_an_snv_bridge_fails():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio").replace(
        "/interface bridge add ", "/interface bridge add igmp-snooping=yes ", 1)
    hard = _hard(_check(v, {"foh1_audio": raw})[0])
    assert any("bridge bridge has igmp-snooping=yes (venue rule" in m for _, _, m in hard), hard


def test_an_active_pp_querier_passes():
    v = _venue("pp", ("pp_foh_a",))
    raw = re.sub(r"igmp-querier: none", "igmp-querier: 10.77.8.1", _raw("pp", "pp_foh_a"))
    hard = _hard(_check(v, {"pp_foh_a": raw})[0])
    assert not any("IGMP" in m for _, _, m in hard), hard


# ------------------------------------------------------------- rule 3 RSTP
def test_a_new_topology_change_on_a_bridge_link_fails_and_a_reboot_rebases():
    v = _venue("snv", ("foh1_audio",))
    raws = {"foh1_audio": _raw("snv", "foh1_audio")}
    findings, state = _check(v, raws, prev={"foh1_audio": {"sfp-sfpplus1": 0}})
    assert any(h and "bridge link sfp-sfpplus1 saw 1 new RSTP topology change" in m for _, h, m in findings), findings
    assert state == {"foh1_audio": {"sfp-sfpplus1": 1}}
    findings, _ = _check(v, raws, prev={"foh1_audio": {"sfp-sfpplus1": 1}})
    assert not any("topology" in m for _, _, m in findings), findings
    findings, _ = _check(v, raws, prev={"foh1_audio": {"sfp-sfpplus1": 5}})
    assert [(s, h) for s, h, m in findings if "went back" in m] == [("REBASE", False)], findings


def test_a_topology_change_behind_restricted_tcn_is_report_only():
    v = _venue("snv", ("router_snv",))
    findings, _ = _check(v, {"router_snv": _raw("snv", "router_snv")}, prev={"router_snv": {"ether15": 3}})
    tc = [(s, h) for s, h, m in findings if "ether15 saw 4 new" in m]
    assert tc == [("WARN", False)], findings


def test_a_host_port_that_is_not_an_edge_port_fails():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio")
    lines = raw.split("\n")
    i = next(n for n, ln in enumerate(lines) if re.match(r"^\s*edge-port:", ln))
    lines[i] = lines[i].replace("edge-port: yes", "edge-port: no ", 1)
    hard = _hard(_check(v, {"foh1_audio": "\n".join(lines)})[0])
    assert any("host port ether1 is not an RSTP edge port" in m for _, _, m in hard), hard


# ------------------------------------------------------------- rule 4 HW offload
def test_a_bridge_port_without_hw_offload_fails():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio").replace(" 0   H  interface=ether1 ", " 0      interface=ether1 ", 1)
    assert raw != _raw("snv", "foh1_audio")
    hard = _hard(_check(v, {"foh1_audio": raw})[0])
    assert any("bridge port ether1 is not HW-offloaded (no H flag)" in m for _, _, m in hard), hard


# ------------------------------------------------------------- rule 5 QoS
def test_a_port_that_drops_the_dscp_and_a_wrong_traffic_class_fail():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio")
    raw = raw.replace("/interface ethernet switch qos port set ether3 trust-l3=keep\n", "")
    raw = raw.replace("dscp=46 name=dante-audio pcp=5 traffic-class=5", "dscp=46 name=dante-audio pcp=5 traffic-class=2")
    hard = [m for _, _, m in _hard(_check(v, {"foh1_audio": raw})[0])]
    assert "foh1_audio: port ether3 does not keep the DSCP (trust-l3=keep missing)" in hard, hard
    assert "foh1_audio: no QoS profile maps DSCP 46 to traffic class 5" in hard, hard


# ------------------------------------------------------------- rule 7 Dante DHCP
def test_a_dante_gateway_fails_at_pp_and_is_allowed_at_snv():
    v = _venue("pp", ("pp_router",))
    raw = _raw("pp", "pp_router").replace(
        "/ip pool add name=pool_dhcp_70 ranges=10.77.7.50-10.77.7.199",
        "/ip pool add name=pool_dhcp_70 ranges=10.77.7.100-10.77.7.199").replace(
        "network add address=10.77.7.0/24 netmask=24", "network add address=10.77.7.0/24 gateway=10.77.7.1 netmask=24")
    hard = [m for _, _, m in _hard(_check(v, {"pp_router": raw})[0])]
    assert hard == ["pp_router: the Dante DHCP network 10.77.7.0/24 hands out a gateway "
                    "(venue rule: Dante without gateway / DNS)"], hard
    snv = _venue("snv", ("router_snv",))
    assert "gateway=10.77.7.1" in _raw("snv", "router_snv")
    assert _hard(_check(snv, {"router_snv": _raw("snv", "router_snv")})[0]) == []


def test_no_dante_dhcp_server_fails():
    v = _venue("pp", ("pp_router",))
    raw = _raw("pp", "pp_router").replace(
        "/ip dhcp-server add address-pool=pool_dhcp_70 interface=vlan::70::dante name=dhcp.70\n", "")
    hard = [m for _, _, m in _hard(_check(v, {"pp_router": raw})[0])]
    assert "pp_router: no DHCP server on the Dante network 10.77.7.0/24" in hard, hard


# ------------------------------------------------------------- rule 8 PTP mirror
def test_the_pp_ptp_mirror_is_checked():
    v = _venue("pp", ("pp_foh_a",))
    good = _raw("pp", "pp_foh_a")
    assert not any("PTP" in m or "mirror" in m for _, _, m in _hard(_check(v, {"pp_foh_a": good})[0]))
    no_rule = "\n".join(ln for ln in good.split("\n") if "comment=ptp-mirror" not in ln)
    hard = [m for _, _, m in _hard(_check(v, {"pp_foh_a": no_rule})[0])]
    assert "pp_foh_a: PTP mirror rule ptp-mirror (mirror of ether6::70::dante-gm) missing" in hard, hard
    no_target = good.replace(" mirror-target=ether7::mirror::ptp-out", "")
    hard = [m for _, _, m in _hard(_check(v, {"pp_foh_a": no_target})[0])]
    assert "pp_foh_a: switch mirror-target is not ether7::mirror::ptp-out" in hard, hard


# ------------------------------------------------------------- rule 1 drift + the read
def test_a_config_change_fails_naming_the_entry_never_a_value():
    v = _venue("pp", ("pp_foh_a",))
    good = _raw("pp", "pp_foh_a")
    base = _baseline_of({"pp_foh_a": good}, v)
    local = {"pp_foh_a": "\n".join(nr.nbq._split_sections(good, required=nr.READS["full"])["export"])}
    assert not any(s == "DRIFT" for s, _, _ in _check(v, {"pp_foh_a": good}, base, local)[0])
    changed = good.replace("interface=ether5::70+89::dsg mvrp-applicant-state=non-participant pvid=70",
                           "interface=ether5::70+89::dsg mvrp-applicant-state=non-participant pvid=71")
    drift = [m for s, h, m in _check(v, {"pp_foh_a": changed}, base, local)[0] if s == "DRIFT" and h]
    assert len(drift) == 1, drift
    assert "/interface bridge port add interface=ether5::70+89::dsg: changed" in drift[0], drift
    assert "pvid=7" not in drift[0], drift


def test_the_export_order_does_not_count_as_a_change():
    v = _venue("pp", ("pp_foh_a",))
    good = _raw("pp", "pp_foh_a")
    secs = nr.nbq._split_sections(good, required=nr.READS["full"])
    head, _, _ = good.partition("=== export\n")
    swapped = head + "=== export\n" + "\n".join(reversed(secs["export"])) + "\n=== end\n"
    assert _baseline_of({"pp_foh_a": good}, v) == _baseline_of({"pp_foh_a": swapped}, v)


def test_an_unreachable_or_cut_read_fails_the_gate():
    v = _venue("snv", ("foh1_audio",))
    for raw in (None, "", _raw("snv", "foh1_audio").replace("=== end", "")):
        hard = _hard(_check(v, {"foh1_audio": raw})[0])
        assert hard == [("FAIL", True, "foh1_audio (10.77.9.2): unreachable or incomplete read -- the network "
                         "check cannot pass without it")], hard


def test_the_export_parser_reads_the_sectioned_form_with_continuations():
    text = (
        "# 2026-11-25 21:50:29 by RouterOS 7.23.3\n"
        "/interface bridge\n"
        "add comment=defconf frame-types=admit-only-vlan-tagged mvrp=yes name=bridge \\\n"
        "    vlan-filtering=yes\n"
        "/interface ethernet switch qos port\n"
        "set ether1 trust-l3=keep\n"
        "/interface ethernet\n"
        "set [ find default-name=ether2 ] comment=\"dante+lan-tag dsg\"\n"
    )
    ents = nr.parse_export(text.split("\n"))
    assert ents[0] == {"path": "/interface bridge", "verb": "add", "target": "",
                       "props": {"comment": "defconf", "frame-types": "admit-only-vlan-tagged",
                                 "mvrp": "yes", "name": "bridge", "vlan-filtering": "yes"}}
    assert ents[1]["target"] == "ether1" and ents[1]["props"] == {"trust-l3": "keep"}
    assert ents[2]["target"] == "[ find default-name=ether2 ]"
    assert ents[2]["props"] == {"comment": "dante+lan-tag dsg"}
    terse = nr.parse_export(["/interface ethernet switch qos port set ether1 trust-l3=keep"])
    assert nr.export_lines(terse) == nr.export_lines([ents[1]])


def test_the_gather_uses_only_read_commands():
    for reads in nr.READS:
        cmd = nr.gather_cmd(reads)
        verbs = re.findall(r"(?:^|; )(/[^;]*)", cmd)
        assert verbs, cmd
        for c in verbs:
            assert re.search(r" (print|print detail|monitor \[find\] once)$", c) or c == "/export terse", c
        assert cmd.endswith(':put "=== end"')


# ------------------------------------------------------------- the orchestrator
def _facts_subset(tmp_path, venue, names, access=None):
    data = json.loads(FACTS.read_text())
    v = data["venues"][venue]
    v["devices"] = [d for d in v["devices"] if d["name"] in names]
    if access is not None:
        v["access"] = access
    p = tmp_path / "facts.json"
    p.write_text(json.dumps(data))
    return p


_NO_NET = None


def _no_network_bin():
    """A PATH dir whose ssh / sshpass refuse to run: a test that reaches them without its own fake
    must fail, never dial a real device (a PP login attempt shows in the PP admin's log)."""
    global _NO_NET
    if _NO_NET is None:
        import tempfile
        d = pathlib.Path(tempfile.mkdtemp(prefix="netcfg-rules-nonet-"))
        for tool in ("ssh", "sshpass"):
            (d / tool).write_text("#!/bin/sh\necho 'test reached a real %s' >&2\nexit 97\n" % tool)
            (d / tool).chmod(0o755)
        _NO_NET = d
    return _NO_NET


def _run_sh(args, env_extra, path_prefix=None):
    env = dict(os.environ, **env_extra)
    env["PATH"] = str(path_prefix or _no_network_bin()) + os.pathsep + env["PATH"]
    return subprocess.run(["bash", str(SH)] + args, env=env, capture_output=True, text=True, timeout=120)


def test_the_script_fails_the_pp_dumps_and_passes_snv_from_a_dir(tmp_path):
    state = tmp_path / "state"
    pp = _run_sh(["--venue", "pp", "--from-dir", str(FIX / "pp")],
                 {"NETCFG_RULES_STATE_DIR": str(state), "NETCFG_RULES_BASELINE": str(tmp_path / "b.json")})
    assert pp.returncode == 3, pp.stdout + pp.stderr
    assert "NETCFG-RULES-FAIL: venue pp network breaks 4 rule(s)" in pp.stdout, pp.stdout
    assert "  [FAIL] pp_router: Dante DHCP pool pool_dhcp_70" in pp.stdout, pp.stdout
    facts = _facts_subset(tmp_path, "snv", SNV2)
    env = {"NETCFG_RULES_STATE_DIR": str(state), "NETCFG_RULES_BASELINE": str(tmp_path / "b.json"),
           "NETCFG_RULES_FACTS": str(facts)}
    cap = _run_sh(["--venue", "snv", "--capture", "--from-dir", str(FIX / "snv")], env)
    assert cap.returncode == 0, cap.stdout + cap.stderr
    assert sorted(json.loads((tmp_path / "b.json").read_text())["snv"]) == sorted(SNV2)
    assert (state / "netcfg-export" / "snv" / "router_snv.rsc").is_file()
    ok = _run_sh(["--venue", "snv", "--from-dir", str(FIX / "snv")], env)
    assert ok.returncode == 0, ok.stdout + ok.stderr
    assert "NETCFG-RULES-CLEAN: venue snv network passes the rules" in ok.stdout
    assert json.loads((state / "netcfg-rules-snv-tc.json").read_text())["foh1_audio"] == {"sfp-sfpplus1": 1}


def test_capture_writes_nothing_when_a_read_is_incomplete(tmp_path):
    raw = tmp_path / "raw"
    raw.mkdir()
    shutil.copy(FIX / "snv" / "router_snv.txt", raw / "router_snv.txt")
    (raw / "foh1_audio.txt").write_text(_raw("snv", "foh1_audio").replace("=== end", ""))
    base = tmp_path / "b.json"
    base.write_text('{"snv": {"router_snv": "keep"}}\n')
    r = _run_sh(["--venue", "snv", "--capture", "--from-dir", str(raw)],
                {"NETCFG_RULES_STATE_DIR": str(tmp_path / "st"), "NETCFG_RULES_BASELINE": str(base),
                 "NETCFG_RULES_FACTS": str(_facts_subset(tmp_path, "snv", SNV2))})
    assert r.returncode == 2, r.stdout + r.stderr
    assert json.loads(base.read_text()) == {"snv": {"router_snv": "keep"}}


def _fake_bin(tmp_path, fixtures):
    """A PATH dir whose `ssh` logs its argv and serves fixtures[<host>]; `sshpass` logs and execs."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "calls.log"
    hosts = json.dumps({h: str(p) for h, p in fixtures.items()})
    (bindir / "ssh").write_text(
        "#!%s\nimport json, sys\nopen(%r, 'a').write('ssh ' + json.dumps(sys.argv[1:]) + '\\n')\n"
        "host = sys.argv[-2].split('@')[-1]\nsys.stdout.write(open(json.loads(%r)[host]).read())\n"
        % (sys.executable, str(log), hosts))
    (bindir / "sshpass").write_text(
        "#!%s\nimport json, os, sys\nopen(%r, 'a').write('sshpass ' + json.dumps(sys.argv[1:3]) + '\\n')\n"
        "os.execvp(sys.argv[3], sys.argv[3:])\n" % (sys.executable, str(log)))
    for f in ("ssh", "sshpass"):
        (bindir / f).chmod(0o755)
    return bindir, log


def _calls(log):
    out = []
    for ln in log.read_text().splitlines():
        tool, _, rest = ln.partition(" ")
        out.append((tool, json.loads(rest)))
    return out


def test_pp_reads_through_the_jump_host_with_the_password_file_one_call_per_device(tmp_path):
    pw = tmp_path / "pw"
    pw.write_text("not-a-real-password\n")
    pw.chmod(0o600)
    facts = _facts_subset(tmp_path, "pp", PP4, access={"user": "snv", "key": "", "password_file": str(pw),
                                                       "jump": "companion-pp"})
    hosts = {"10.77.8.%d" % (i + 1): FIX / "pp" / (n + ".txt") for i, n in enumerate(PP4)}
    bindir, log = _fake_bin(tmp_path, hosts)
    r = _run_sh(["--venue", "pp"], {"NETCFG_RULES_FACTS": str(facts), "NETCFG_RULES_STATE_DIR": str(tmp_path / "st"),
                                    "NETCFG_RULES_BASELINE": str(tmp_path / "b.json")}, bindir)
    assert r.returncode == 3, r.stdout + r.stderr
    assert "not-a-real-password" not in r.stdout + r.stderr
    calls = _calls(log)
    sshs = [a for t, a in calls if t == "ssh"]
    assert [t for t, _ in calls] == ["sshpass", "ssh"] * 4, calls
    assert all(a == ["-f", str(pw)] for t, a in calls if t == "sshpass"), calls
    assert [a[-2] for a in sshs] == ["snv@10.77.8.%d" % i for i in (1, 2, 3, 4)], sshs
    for a in sshs:
        assert a[0] == "-n" and a[a.index("-J") + 1] == "companion-pp", a
        assert "PubkeyAuthentication=no" in a and "-i" not in a, a
        assert "StrictHostKeyChecking=accept-new" in a, a
        assert "UserKnownHostsFile=%s" % (tmp_path / "st" / "netcfg-known_hosts-pp") in a, a
        assert a[-1].startswith(':put "=== identity"') and a[-1].endswith(':put "=== end"'), a


def test_snv_reads_with_the_key_user_and_no_password(tmp_path):
    key = tmp_path / "key"
    key.write_text("fake key\n")
    facts = _facts_subset(tmp_path, "snv", SNV2, access={"user": "camerabox", "key": str(key),
                                                         "password_file": "", "jump": ""})
    bindir, log = _fake_bin(tmp_path, {"10.77.8.1": FIX / "snv" / "router_snv.txt",
                                       "10.77.9.2": FIX / "snv" / "foh1_audio.txt"})
    r = _run_sh(["--venue", "snv"], {"NETCFG_RULES_FACTS": str(facts), "NETCFG_RULES_STATE_DIR": str(tmp_path / "st"),
                                     "NETCFG_RULES_BASELINE": str(tmp_path / "b.json")}, bindir)
    assert r.returncode == 0, r.stdout + r.stderr  # no baseline yet = report-only, rules clean
    calls = _calls(log)
    assert [t for t, _ in calls] == ["ssh", "ssh"], calls
    for _, a in calls:
        assert a[0] == "-n" and "-J" not in a and a[a.index("-i") + 1] == str(key) and "BatchMode=yes" in a, a


def test_a_missing_credential_is_an_error_not_a_pass(tmp_path):
    facts = _facts_subset(tmp_path, "pp", PP4, access={"user": "snv", "key": "",
                                                       "password_file": str(tmp_path / "absent"), "jump": "companion-pp"})
    r = _run_sh(["--venue", "pp"], {"NETCFG_RULES_FACTS": str(facts), "NETCFG_RULES_STATE_DIR": str(tmp_path / "st")})
    assert r.returncode == 2, r.stdout + r.stderr
    assert "no credential" in r.stderr


def test_the_committed_snv_baseline_names_every_snv_device():
    base = json.loads((_SCRIPTS / "netcfg-export-baseline.json").read_text())
    assert sorted(base["snv"]) == sorted(d["name"] for d in _venue("snv")["devices"])
    assert all(re.fullmatch(r"[0-9a-f]{64}", h) for h in base["snv"].values())


# ------------------------------------------------------------- the rig test
def test_the_e2e_runs_the_snv_network_gate_before_the_clock_gate():
    s = E2E.read_text()
    call = s.index('"$HERE/netcfg-rules.sh" --venue snv || exit 1')
    assert s.count("netcfg-rules.sh") == 1
    assert call < s.index('echo "[0/8] DanteSync NTP+PTP gate')
    assert call < s.index('echo "[0/8] camera-box version-parity gate')


# ------------------------------------------------------------- review round 1
def _with_export_lines(raw, extra):
    return raw.replace("=== end", "\n".join(extra) + "\n=== end", 1)


def test_what_the_router_changes_by_itself_is_no_drift_but_a_real_change_is():
    v = _venue("snv", ("router_snv",))
    dns = '/ip dns static add address=10.76.8.202 comment="pp-dns-switch 202" name=strih-pp.lan ttl=30s type=A'
    route = "/ip route add check-gateway=ping comment=nlvpn-dns dst-address=8.8.8.8/32 gateway=10.255.255.2"
    mangle = "/ip firewall mangle add action=mark-routing chain=prerouting comment=nlvpn disabled=yes"
    good = _with_export_lines(_raw("snv", "router_snv"), [dns, route, mangle])
    base = {"router_snv": nr.export_hash(nr.parse_device(good, "full")["export"],
                                         v["devices"][0]["drift_ignore"])}
    flipped = _with_export_lines(_raw("snv", "router_snv"),
                                 [dns.replace("10.76.8.202", "10.77.8.202"),
                                  mangle.replace(" disabled=yes", "")])
    assert not any(s == "DRIFT" for s, _, _ in _check(v, {"router_snv": flipped}, base)[0])
    renamed = _with_export_lines(_raw("snv", "router_snv"),
                                 [dns.replace("name=strih-pp.lan", "name=strih-xx.lan"), route, mangle])
    assert any(s == "DRIFT" and h for s, h, _ in _check(v, {"router_snv": renamed}, base)[0])


def test_an_export_that_no_longer_parses_is_drift_when_a_baseline_exists():
    v = _venue("pp", ("pp_foh_a",))
    good = _raw("pp", "pp_foh_a")
    base = _baseline_of({"pp_foh_a": good}, v)
    broken = _with_export_lines(good, ['/interface bridge port add comment="unbalanced'])
    findings = _check(v, {"pp_foh_a": broken}, base)[0]
    assert ("DRIFT", True, "pp_foh_a: the export no longer parses -- changed since the baseline") in findings


def test_a_drift_line_never_names_a_comment_or_an_address():
    old = nr.parse_export(['/ip firewall filter add action=accept chain=forward comment="secret thing" '
                           'src-address=10.1.2.3'])
    new = nr.parse_export(['/ip firewall filter add action=drop chain=forward comment="secret thing" '
                           'src-address=10.1.2.3'])
    labels = nr.export_diff_labels(old, new)
    assert len(labels) == 2 and all("/ip firewall filter add entry " in lb for lb in labels), labels
    assert not any("secret" in lb or "10.1.2.3" in lb for lb in labels), labels


def test_a_port_that_is_not_forwarding_yet_is_not_judged_as_an_edge_port():
    v = _venue("snv", ("foh1_audio",))
    lines = _raw("snv", "foh1_audio").split("\n")
    i = next(n for n, ln in enumerate(lines) if re.match(r"^\s*edge-port:", ln))
    lines[i] = lines[i].replace("edge-port: yes", "edge-port: no ", 1)
    j = next(n for n, ln in enumerate(lines) if re.match(r"^\s*forwarding:", ln))
    lines[j] = lines[j].replace("forwarding: yes", "forwarding: no ", 1)
    assert not any("edge port" in m for _, _, m in _check(v, {"foh1_audio": "\n".join(lines)})[0])


def test_a_link_that_is_down_keeps_its_counter_for_the_next_run():
    v = _venue("snv", ("foh1_audio",))
    _, state = _check(v, {"foh1_audio": _raw("snv", "foh1_audio")},
                      prev={"foh1_audio": {"sfp-sfpplus2": 4, "sfp-sfpplus1": 1}})
    assert state == {"foh1_audio": {"sfp-sfpplus1": 1, "sfp-sfpplus2": 4}}


def test_an_unreadable_bridge_port_table_reports_rstp_unknown():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio").replace("=== bridge-port\n", "=== bridge-port\nsomething unexpected\n", 1)
    findings = _check(v, {"foh1_audio": raw})[0]
    assert ("UNKNOWN", False, "foh1_audio: bridge ports unreadable, RSTP not checked") in findings, findings


def test_a_missing_querier_on_one_of_two_bridges_fails():
    v = _venue("pp", ("pp_foh_a",))
    raw = re.sub(r"igmp-querier: none", "igmp-querier: 10.77.8.1 none", _raw("pp", "pp_foh_a"))
    assert any("no active IGMP querier" in m for _, _, m in _hard(_check(v, {"pp_foh_a": raw})[0]))


def test_the_ptp_mirror_target_in_the_bridge_and_a_wrong_return_pvid_fail():
    v = _venue("pp", ("pp_foh_a",))
    good = _raw("pp", "pp_foh_a")
    in_bridge = good.replace("6 H interface=ether8::89::ptp", "6 H interface=ether7::mirror::ptp-out", 1)
    assert in_bridge != good
    hard = [m for _, _, m in _hard(_check(v, {"pp_foh_a": in_bridge})[0])]
    assert "pp_foh_a: mirror target ether7::mirror::ptp-out is inside the bridge" in hard, hard
    blocks = good.split("6 H interface=ether8::89::ptp", 1)
    assert len(blocks) == 2
    wrong_pvid = blocks[0] + "6 H interface=ether8::89::ptp" + blocks[1].replace("pvid=89", "pvid=70", 1)
    hard = [m for _, _, m in _hard(_check(v, {"pp_foh_a": wrong_pvid})[0])]
    assert "pp_foh_a: PTP return port ether8::89::ptp not in the bridge with pvid 89" in hard, hard


def test_a_disabled_port_is_not_held_to_the_qos_rule():
    v = _venue("snv", ("foh1_audio",))
    raw = _raw("snv", "foh1_audio").replace(" 0   H  interface=ether1 ", " 0 X H  interface=ether1 ", 1)
    assert raw != _raw("snv", "foh1_audio")
    raw = raw.replace("/interface ethernet switch qos port set ether1 trust-l3=keep\n", "")
    assert not any("ether1" in m for _, _, m in _hard(_check(v, {"foh1_audio": raw})[0]))


def test_a_replay_never_touches_the_live_state(tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    env = dict(os.environ, HOME=str(home), NETCFG_RULES_BASELINE=str(tmp_path / "b.json"))
    env["PATH"] = str(_no_network_bin()) + os.pathsep + env["PATH"]
    env.pop("NETCFG_RULES_STATE_DIR", None)
    r = subprocess.run(["bash", str(SH), "--venue", "pp", "--from-dir", str(FIX / "pp")],
                       env=env, capture_output=True, text=True, timeout=120)
    assert r.returncode == 3, r.stdout + r.stderr
    assert list(home.iterdir()) == [], list(home.iterdir())


def test_a_password_file_others_can_read_is_refused(tmp_path):
    pw = tmp_path / "pw"
    pw.write_text("x\n")
    pw.chmod(0o644)
    facts = _facts_subset(tmp_path, "pp", PP4, access={"user": "snv", "key": "", "password_file": str(pw),
                                                       "jump": "companion-pp"})
    r = _run_sh(["--venue", "pp"], {"NETCFG_RULES_FACTS": str(facts), "NETCFG_RULES_STATE_DIR": str(tmp_path / "st")})
    assert r.returncode == 2 and "readable by others" in r.stderr, r.stdout + r.stderr


# ------------------------------------------------------------- review round 2
def test_an_inline_runtime_comment_is_not_hashed():
    a = nr.parse_export(["/ip route add dst-address=0.0.0.0/0 gateway=10.77.8.1 # gateway not reachable"])
    b = nr.parse_export(["/ip route add dst-address=0.0.0.0/0 gateway=10.77.8.1"])
    assert nr.export_hash(a) == nr.export_hash(b)
    assert a[0]["props"] == {"dst-address": "0.0.0.0/0", "gateway": "10.77.8.1"}


def test_a_replay_capture_never_overwrites_the_committed_baseline(tmp_path):
    env = dict(os.environ, NETCFG_RULES_STATE_DIR=str(tmp_path / "st"))
    env.pop("NETCFG_RULES_BASELINE", None)
    env["PATH"] = str(_no_network_bin()) + os.pathsep + env["PATH"]
    before = (_SCRIPTS / "netcfg-export-baseline.json").read_bytes()
    r = subprocess.run(["bash", str(SH), "--venue", "snv", "--capture", "--from-dir", str(FIX / "snv")],
                       env=env, capture_output=True, text=True, timeout=120)
    assert r.returncode == 2 and "NETCFG_RULES_BASELINE" in r.stderr, r.stdout + r.stderr
    assert (_SCRIPTS / "netcfg-export-baseline.json").read_bytes() == before
