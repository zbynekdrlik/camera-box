"""issue 1495 (owner 9.11.2026: "stream nb sa ma prerobit na jeden ethernet"): stream-snv's VBAN/Dante leg
moves from the USB-C NIC (`mac:` leg, enx00e04c680185) to VLAN 70 tagged on its rig NIC enp56s0 (r8169).

The leg lib had no transition between leg KINDS: on a `vlan:` leg the NetworkManager scan lists only
`vlan`-type profiles, so the live ethernet `vban-dante70` (10.77.7.204/32, active on the USB NIC) stayed up
next to the new VLAN carrying the same address. Pinned here:
  * the old NIC leg is retired (its profile deleted) only AFTER the VLAN came up and read back;
  * a VLAN that does not come up leaves the old leg untouched (the box keeps its program audio);
  * a VLAN that came up this run but does not read back is taken down + left disarmed, so the box returns
    to ONE interface on the leg address, and setup fails by name;
  * the rig NIC is never brought up / down / reapplied, and a `vban-dante70` profile bound to it is never
    deleted;
  * verify item 9 FAILs a leftover NIC-leg profile and the leg address on any other interface.
Fakes: vban_leg_fakes_1439.py (nmcli keeps saved vs applied profiles, ip derives from the applied ones).
"""
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vban_leg_fakes_1439 as fk  # noqa: E402
import test_vban_leg_1439 as t39  # noqa: E402

RIG_IF = t39.RIG_IF                   # enp56s0, the onboard RTL8168 (r8169)
RIG_UUID = t39.RIG_UUID
NIC_IF = t39.LEG_IF                   # enx00e04c680185, the retired USB-C leg
VLAN_IF = RIG_IF + ".70"
ADDR = t39.LEG_ADDR                   # 10.77.7.204 on both, never on two at the end
OLD_UUID = "565ed2bb-cc72-31b4-88bb-d937e55c3c2d"


def _facts(tmp_path, **kv):
    """A copy of the box facts with stream-snv's KEY=value lines set (the leg on VLAN 70 by default)."""
    import shutil
    d = tmp_path / "boxes"
    if not d.exists():
        shutil.copytree(t39.BOXES, d)
    f = d / "stream-snv.env"
    t = f.read_text()
    kv.setdefault("STRIH_PROGRAM_VBAN_LEG", "vlan:70")
    for k, v in fk.STREAM_SNV_PRE_1502_NIC.items():  # the 9.11 box: the onboard rig NIC, before issue 1502
        kv.setdefault(k, v)
    for k, v in kv.items():
        t, n = re.subn(r"(?m)^%s=.*$" % k, "%s=%s" % (k, v), t)
        assert n == 1, k
    f.write_text(t)
    return d


def _box(tmp_path, old_active=True, old_ifname=NIC_IF, extra_profiles=(), facts=None, **over):
    """stream-snv as it runs today: the rig NIC + the live NIC leg (LIVE_PROPS) on the USB adapter."""
    old = {"uuid": OLD_UUID, "type": "802-3-ethernet",
           "props": dict(t39.LIVE_PROPS, **{"connection.interface-name": old_ifname})}
    profiles = [t39._rig_profile(), old] + list(extra_profiles)
    active = {RIG_IF: RIG_UUID}
    applied = {RIG_IF: t39._rig_profile()["props"]}
    if old_active:
        active[old_ifname] = OLD_UUID
        applied[old_ifname] = dict(old["props"])
    kw = dict(profiles=profiles, active=active, applied=applied, rig_if=RIG_IF, nft_counters=[[100, 0], [350, 0]])
    kw.update(over)
    b = t39.Box(tmp_path, **kw)
    b.nic(RIG_IF, "e8:9c:25:ce:b6:ea", driver="r8169")
    b.nic(NIC_IF, t39.LEG_MAC, driver="r8152")
    b.env["STRIH_BOXES_DIR"] = str(_facts(tmp_path, **(facts or {})))
    return b


def _rig_touched(s):
    """Every nmcli call that could bounce the rig link: an up / down / modify / delete / reapply aimed at
    the rig NIC or its profile (a VLAN's `vlan.parent enp56s0` in `connection add` is not one)."""
    bad = []
    for c in fk.calls(s, "nmcli"):
        if c[:2] in (["connection", "up"], ["connection", "down"], ["connection", "modify"],
                     ["connection", "delete"], ["device", "reapply"]) and c[2] in (RIG_IF, RIG_UUID):
            bad.append(c)
        if "ifname" in c and c[c.index("ifname") + 1] == RIG_IF and c[:2] == ["connection", "up"]:
            bad.append(c)
    return bad


def _carriers_of_addr(s):
    return sorted(d for d, p in s["applied"].items() if (p.get("ipv4.addresses") or "").startswith(ADDR + "/"))


def test_the_nic_leg_is_retired_only_after_the_vlan_came_up_and_read_back(tmp_path):
    b = _box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert _carriers_of_addr(s) == [VLAN_IF], s["applied"].keys()
    assert all(p["uuid"] != OLD_UUID for p in s["profiles"]), "the old NIC leg's profile is gone"
    log = [c for c in fk.calls(s, "nmcli") if c[:1] != ["-g"]]
    up = next(i for i, c in enumerate(log) if c[:2] == ["connection", "up"])
    gone = next(i for i, c in enumerate(log) if c[:2] in (["connection", "delete"], ["connection", "down"])
                and OLD_UUID in c)
    assert up < gone, log
    # the read-back of the new leg ran before the old one went: an address read on the VLAN precedes it
    full = s["log"]
    first_read = next(i for i, c in enumerate(full) if c[0] == "ip" and c[1:5] == ["-4", "-o", "addr", "show"]
                      and VLAN_IF in c)
    retire = next(i for i, c in enumerate(full) if c[0] == "nmcli" and OLD_UUID in c
                  and c[1:3] in (["connection", "delete"], ["connection", "down"]))
    assert first_read < retire
    assert _rig_touched(s) == [], _rig_touched(s)
    assert NIC_IF in r.stdout and "retired" in r.stdout, r.stdout


def test_a_vlan_that_does_not_come_up_leaves_the_nic_leg_untouched(tmp_path):
    b = _box(tmp_path, carrier={VLAN_IF: False})
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    s = b.st()
    assert s["active"].get(NIC_IF) == OLD_UUID and any(p["uuid"] == OLD_UUID for p in s["profiles"])
    assert _carriers_of_addr(s) == [NIC_IF]
    assert _rig_touched(s) == []


def test_a_vlan_that_does_not_read_back_is_rolled_back_to_the_one_nic_leg(tmp_path):
    # the new leg came up but a default route leaves through it: the read-back fails
    b = _box(tmp_path, extra_default=[VLAN_IF])
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    assert VLAN_IF in r.stderr and NIC_IF in r.stderr, r.stderr
    s = b.st()
    assert VLAN_IF not in s["active"], "the new VLAN is taken down again"
    assert not [p for p in s["profiles"] if p["type"] == "vlan"], "and removed: the next run creates it again"
    assert s["active"].get(NIC_IF) == OLD_UUID, "the NIC leg keeps the program audio"
    assert _carriers_of_addr(s) == [NIC_IF]
    assert _rig_touched(s) == []
    assert "vban_dante70_retiring" in s.get("nft_other", {}), "the old leg stays filtered"


def test_a_rerun_after_a_rollback_creates_the_vlan_again(tmp_path):
    b = _box(tmp_path, extra_default=[VLAN_IF])
    assert b.run("strih_vban_leg_install").returncode != 0
    s = b.st()
    s["extra_default"] = []
    b.state.write_text(json.dumps(s))
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert _carriers_of_addr(b.st()) == [VLAN_IF]


def test_a_vlan_that_does_not_read_back_stays_when_no_nic_leg_carries_the_address(tmp_path):
    # the 8.11 shape: the USB NIC dropped off the bus, its profile is inactive -- the VLAN is the only leg
    b = _box(tmp_path, old_active=False, extra_default=[VLAN_IF])
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    s = b.st()
    assert VLAN_IF in s["active"] and _carriers_of_addr(s) == [VLAN_IF], "never left with no leg"


def test_a_mac_leg_created_this_run_is_never_rolled_back(tmp_path):
    # the rollback is the vlan transition's; a fresh mac: leg whose read-back fails stays the box's leg
    b = t39.Box(tmp_path, profiles=[t39._rig_profile()], active={RIG_IF: RIG_UUID},
                applied={RIG_IF: t39._rig_profile()["props"]}, extra_default=[NIC_IF])
    b.nic(NIC_IF, t39.LEG_MAC)
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    s = b.st()
    assert s["active"].get(NIC_IF) and _carriers_of_addr(s) == [NIC_IF]
    assert not fk.calls(s, "nmcli", "connection", "delete")


def test_the_old_leg_stays_filtered_until_it_is_retired(tmp_path):
    b = _box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    nft = [c for c in s["log"] if c[0] == "nft"]
    loads = [i for i, c in enumerate(s["log"]) if c[:2] == ["nft", "-f"]]
    assert len(loads) >= 1 and any(c[1:] == ["delete", "table", "inet", "vban_dante70_retiring"] for c in nft), nft
    assert "vban_dante70_retiring" not in s.get("nft_other", {}), "removed once the old leg is gone"
    first_up = next(i for i, c in enumerate(s["log"]) if c[0] == "nmcli" and "connection" in c and "up" in c)
    # issue 1519: the real stream-snv is a Dante box, so its main filter is dstack's generic one
    main_load = next(i for i, c in enumerate(s["log"]) if c[:2] == ["systemctl", "restart"]
                     and c[2] in ("vban-dante70-filter.service", "dstack-dante-leg-filter.service"))
    retiring = [i for i in loads if i < first_up and i < main_load]
    assert retiring, "loaded before the main table moves to the VLAN and before the VLAN comes up"
    assert "keeps the old NIC leg %s filtered" % NIC_IF in r.stdout


def test_unreadable_profiles_refuse_the_step_with_nothing_changed(tmp_path):
    b = _box(tmp_path, list_fails=True)
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0 and "nothing changed" in r.stderr, r.stderr
    s = b.st()
    assert not [c for c in fk.calls(s, "nmcli") if c[:1] != ["-g"]], "no NetworkManager change"
    assert not [c for c in s["log"] if c[:2] == ["systemctl", "restart"]], "the main table is not moved"


def test_verify_fails_a_default_route_through_the_retired_nic(tmp_path):
    b = _converged(tmp_path)
    s = b.st()
    s["extra_default"] = [NIC_IF]
    b.state.write_text(json.dumps(s))
    bad = [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]
    assert any("default route through the retired NIC %s" % NIC_IF in x for x in bad), bad


def test_verify_leaves_another_interfaces_default_route_to_its_own_row(tmp_path):
    # a venue WiFi's default route is not the leg's leftover (review round 3)
    b = _converged(tmp_path)
    s = b.st()
    s["extra_default"] = ["wlp0s20f3"]
    b.state.write_text(json.dumps(s))
    assert not [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]


def test_a_vlan_that_does_not_come_up_keeps_the_old_leg_filtered(tmp_path):
    b = _box(tmp_path, carrier={VLAN_IF: False})
    assert b.run("strih_vban_leg_install").returncode != 0
    other = b.st().get("nft_other", {})
    assert NIC_IF in other.get("vban_dante70_retiring", ""), other


def test_the_retired_nic_is_never_parked_under_a_profile(tmp_path):
    # the 9.11.2026 parking profile kept the link UP (an active profile = admin up), and mbc's stale ARP entry
    # sent the Dante stream to it for ~65 min: the NIC is unmanaged + link-down instead
    # (test_vban_leg_move_1495.py), so NetworkManager has no device there to put an automatic DHCP profile on
    b = _box(tmp_path)
    assert b.run("strih_vban_leg_install").returncode == 0
    s = b.st()
    assert not [p for p in s["profiles"] if p["props"].get("connection.id") == "vban-dante70-retired"]
    assert NIC_IF in s.get("unmanaged", {})
    assert b.run("strih_vban_leg_install").returncode == 0
    assert not [p for p in b.st()["profiles"] if p["props"].get("connection.interface-name") == NIC_IF]


def test_a_rule_the_old_leg_took_with_it_comes_back_by_one_reapply(tmp_path):
    b = _box(tmp_path, delete_drops_shared_rules=True)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert fk.calls(s, "nmcli", "device", "reapply", VLAN_IF)
    assert "reapplied once" in r.stdout
    assert s["applied"][VLAN_IF]["ipv4.routing-rules"] == "priority 100 from %s table 70" % ADDR


def test_a_rule_that_does_not_come_back_fails_by_name(tmp_path):
    b = _box(tmp_path, delete_drops_shared_rules=True, reapply_fails=True)
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    assert "after retiring the NIC leg" in r.stderr and "policy rule" in r.stderr, r.stderr


def test_a_rerun_after_a_run_that_left_the_nic_leg_retires_it(tmp_path):
    b = _box(tmp_path)
    assert b.run("strih_vban_leg_install").returncode == 0
    # a second run with the old profile put back (a killed first run): it goes again, nothing else moves
    s = b.st()
    vlan = next(p for p in s["profiles"] if p["type"] == "vlan")
    old = {"uuid": OLD_UUID, "type": "802-3-ethernet",
           "props": dict(t39.LIVE_PROPS)}
    s["profiles"].append(old)
    s["active"][NIC_IF] = OLD_UUID
    s["applied"][NIC_IF] = dict(old["props"])
    s["log"] = []
    import json
    b.state.write_text(json.dumps(s))
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert _carriers_of_addr(s) == [VLAN_IF] and s["active"].get(VLAN_IF) == vlan["uuid"]
    assert not fk.calls(s, "nmcli", "connection", "up"), "the VLAN is never activated twice"
    assert all(p["uuid"] != OLD_UUID for p in s["profiles"])


def test_a_vban_profile_bound_to_the_rig_nic_is_never_touched(tmp_path):
    stray = {"uuid": "99999999-2222-3333-4444-555555555555", "type": "802-3-ethernet",
             "props": dict(t39.LIVE_PROPS, **{"connection.interface-name": RIG_IF})}
    b = _box(tmp_path, extra_profiles=[stray])
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert any(p["uuid"] == stray["uuid"] for p in s["profiles"]), "never deleted"
    assert "WARN" in r.stdout and RIG_IF in r.stdout and stray["uuid"] in r.stdout, r.stdout
    assert _rig_touched(s) == [], _rig_touched(s)


def test_an_unplugged_nic_legs_profile_is_deleted(tmp_path):
    b = _box(tmp_path, old_active=False)
    import shutil
    shutil.rmtree(b.sysfs / "class" / "net" / NIC_IF)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert all(p["uuid"] != OLD_UUID for p in b.st()["profiles"])


# --- verify: the leftover ----------------------------------------------------------------------------------

def _converged(tmp_path, **over):
    b = _box(tmp_path, **over)
    assert b.run("strih_vban_leg_install").returncode == 0
    fk.write_proc(b.proc, VLAN_IF, 1, 2)
    p = b.proc / "net" / "ipv4" / "conf" / VLAN_IF / "rp_filter"
    p.write_text("2\n")
    b.nic(VLAN_IF, "e8:9c:25:ce:b6:ea", physical=False)
    return b


def _bad(r):
    assert r.returncode == 0, r.stderr
    return [x for x in r.stdout.splitlines() if x.startswith("BAD|")]


def test_verify_names_no_leftover_once_retired(tmp_path):
    b = _converged(tmp_path)
    assert not [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]


def test_verify_fails_a_leftover_nic_leg_profile(tmp_path):
    b = _converged(tmp_path)
    import json
    s = b.st()
    s["profiles"].append({"uuid": OLD_UUID, "type": "802-3-ethernet", "props": dict(t39.LIVE_PROPS)})
    b.state.write_text(json.dumps(s))
    bad = [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]
    assert bad and OLD_UUID in bad[0] and NIC_IF in bad[0], bad


def test_verify_fails_the_leg_address_on_another_interface(tmp_path):
    b = _converged(tmp_path)
    import json
    s = b.st()
    s["profiles"].append({"uuid": OLD_UUID, "type": "802-3-ethernet", "props": dict(t39.LIVE_PROPS)})
    s["active"][NIC_IF] = OLD_UUID
    s["applied"][NIC_IF] = dict(t39.LIVE_PROPS)
    b.state.write_text(json.dumps(s))
    bad = [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]
    assert any(NIC_IF in x and ADDR in x for x in bad), bad
