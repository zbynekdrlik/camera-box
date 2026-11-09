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
    made = [p for p in s["profiles"] if p["type"] == "vlan"]
    assert made and made[0]["props"]["connection.autoconnect"] == "no", "and never re-armed"
    assert s["active"].get(NIC_IF) == OLD_UUID, "the NIC leg keeps the program audio"
    assert _carriers_of_addr(s) == [NIC_IF]
    assert _rig_touched(s) == []


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
