"""issue 1495, the leg move's three follow-ups (live 9.11.2026, comments 6748851938 + 6747760672):

1. A retired NIC leg must be LINK-DOWN, not only without an IP. stream-snv's old USB-C NIC was parked under a
   `vban-dante70-retired` profile with no address but its link UP on the switch (VLAN 70 untagged); mbc kept the
   old MAC for 10.77.7.204 and sent the Dante stream there for ~65 min. Now the NIC is disconnected, its profile
   deleted, made unmanaged by a NetworkManager drop-in keyed on its MAC and its link set down; verify FAILs a
   retired NIC whose link is up.
2. The leg address is announced (iputils `arping -U`) right after a changed leg reads back and once after a
   retirement, so a neighbour updates its ARP entry at once. Never the rig NIC / STRIH_IP.
3. A leg profile change (`device reapply`) stalls a RUNNING dstack device (the hub's `strih-snv-intercom`,
   ENODEV until a restart): setup restarts a running hub after the leg is final (never starts a stopped one)
   and names a running OBS on a Dante-program box loudly.
Fakes: vban_leg_fakes_1439.py (nmcli / ip / nft / systemctl / sysctl / arping / pgrep over one state file).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vban_leg_fakes_1439 as fk  # noqa: E402
import test_vban_leg_1439 as t39  # noqa: E402
import test_vban_leg_nic_to_vlan_1495 as t95  # noqa: E402

RIG_IF, NIC_IF, VLAN_IF, ADDR, OLD_UUID = t95.RIG_IF, t95.NIC_IF, t95.VLAN_IF, t95.ADDR, t95.OLD_UUID
NIC_MAC = t39.LEG_MAC
RIG_MAC = "e8:9c:25:ce:b6:ea"
RETIRED = "vban-dante70-retired"


def _box(tmp_path, **over):
    b = t95._box(tmp_path, **over)
    b.nm_conf = tmp_path / "etc" / "NetworkManager" / "conf.d" / "90-vban-dante70-retired.conf"
    b.env["STRIH_VBAN_LEG_NM_CONF_FILE"] = str(b.nm_conf)
    return b


def _flags(b, ifc):
    return int((b.sysfs / "class" / "net" / ifc / "flags").read_text().strip(), 16)


def _link_up(b, ifc):
    return bool(_flags(b, ifc) & 0x1)


def _converged(tmp_path, **over):
    b = _box(tmp_path, **over)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    fk.write_proc(b.proc, VLAN_IF, 1, 2)
    (b.proc / "net" / "ipv4" / "conf" / VLAN_IF / "rp_filter").write_text("2\n")
    b.nic(VLAN_IF, RIG_MAC, physical=False)
    s = b.st()
    s["log"] = []
    b.state.write_text(json.dumps(s))
    return b


def _bad(r):
    assert r.returncode == 0, r.stderr
    return [x for x in r.stdout.splitlines() if x.startswith("BAD|")]


def _arp_announces(s):
    return [c for c in fk.calls(s, "arping") if "-U" in c]


# --- 1. the retired NIC: link down, unmanaged, durable -----------------------------------------------------

def test_the_retired_nic_is_link_down_and_unmanaged(tmp_path):
    b = _box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert not _link_up(b, NIC_IF), "the switch must lose the link: no frame for the old MAC is delivered"
    assert NIC_IF in s.get("unmanaged", {}), "NetworkManager lets go of it now"
    conf = b.nm_conf.read_text()
    assert "[device-vban-dante70-retired]" in conf and "managed=0" in conf
    assert "mac:%s" % NIC_MAC in conf, "keyed on the MAC: survives an interface rename"
    assert not [p for p in s["profiles"] if p["props"].get("connection.id") == RETIRED], "no parking profile"
    assert NIC_IF not in s["active"]
    log = [c for c in fk.calls(s, "nmcli") if c[:1] != ["-g"]]
    disc = next(i for i, c in enumerate(log) if c == ["device", "disconnect", NIC_IF])
    gone = next(i for i, c in enumerate(log) if c[:2] == ["connection", "delete"] and OLD_UUID in c)
    assert disc < gone, "disconnected first: its autoconnect is blocked before the profile goes"
    assert _link_up(b, RIG_IF), "the rig NIC is never touched"


def test_a_rerun_keeps_the_retired_nic_down_and_writes_nothing(tmp_path):
    b = _converged(tmp_path)
    before = b.nm_conf.stat().st_mtime_ns
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert b.nm_conf.stat().st_mtime_ns == before
    assert not _link_up(b, NIC_IF)
    assert not [c for c in fk.calls(b.st(), "ip") if c[:2] == ["link", "set"]], "nothing to change"


def test_a_retired_nic_brought_up_again_is_downed_by_the_next_run(tmp_path):
    b = _converged(tmp_path)
    (b.sysfs / "class" / "net" / NIC_IF / "flags").write_text("0x1003\n")
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert not _link_up(b, NIC_IF)


def test_the_9_11_parked_profile_is_removed_and_its_nic_downed(tmp_path):
    """stream-snv as the supervisor left it: the VLAN leg + the parked profile (link down by hand, which a
    reboot undoes: the profile autoconnects)."""
    b = _converged(tmp_path)
    s = b.st()
    s["profiles"].append({"uuid": "77777777-1111-2222-3333-444444444444", "type": "802-3-ethernet",
                          "props": {"connection.id": RETIRED, "connection.interface-name": NIC_IF,
                                    "ipv4.method": "disabled", "ipv6.method": "disabled",
                                    "connection.autoconnect": "yes"}})
    s["active"][NIC_IF] = "77777777-1111-2222-3333-444444444444"
    s["applied"][NIC_IF] = dict(s["profiles"][-1]["props"])
    s.pop("unmanaged", None)
    b.state.write_text(json.dumps(s))
    b.nm_conf.unlink()
    (b.sysfs / "class" / "net" / NIC_IF / "flags").write_text("0x1003\n")
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert not [p for p in s["profiles"] if p["props"].get("connection.id") == RETIRED]
    assert not _link_up(b, NIC_IF) and NIC_IF in s.get("unmanaged", {}) and b.nm_conf.exists()


def test_the_rig_nic_is_never_unmanaged_or_downed(tmp_path):
    stray = {"uuid": "99999999-2222-3333-4444-555555555555", "type": "802-3-ethernet",
             "props": dict(t39.LIVE_PROPS, **{"connection.interface-name": RIG_IF})}
    b = _box(tmp_path, extra_profiles=[stray])
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert RIG_IF not in s.get("unmanaged", {}) and _link_up(b, RIG_IF)
    assert "mac:%s" % RIG_MAC not in b.nm_conf.read_text() and "interface-name:%s" % RIG_IF not in b.nm_conf.read_text()


def test_a_nic_sharing_the_rig_nics_mac_is_never_unmanaged(tmp_path):
    b = _box(tmp_path)
    (b.sysfs / "class" / "net" / NIC_IF / "address").write_text(RIG_MAC + "\n")
    r = b.run("_strih_vban_leg_retire_down %s %s" % (NIC_IF, RIG_IF))
    assert r.returncode != 0 and "rig NIC" in r.stdout + r.stderr, r.stdout + r.stderr
    assert not b.nm_conf.exists() and NIC_IF not in b.st().get("unmanaged", {})


def test_verify_fails_a_retired_nic_whose_link_is_up(tmp_path):
    b = _converged(tmp_path)
    (b.sysfs / "class" / "net" / NIC_IF / "flags").write_text("0x1003\n")
    bad = [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]
    assert any(NIC_IF in x and "link" in x and "UP" in x for x in bad), bad


def test_verify_fails_a_parked_profile_left(tmp_path):
    b = _converged(tmp_path)
    s = b.st()
    s["profiles"].append({"uuid": "77777777-1111-2222-3333-444444444444", "type": "802-3-ethernet",
                          "props": {"connection.id": RETIRED, "connection.interface-name": NIC_IF,
                                    "ipv4.method": "disabled"}})
    b.state.write_text(json.dumps(s))
    bad = [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]
    assert any(RETIRED in x for x in bad), bad


def test_verify_passes_a_retired_nic_that_is_down(tmp_path):
    b = _converged(tmp_path)
    r = b.run("strih_vban_leg_grade")
    assert not [x for x in _bad(r) if "leftover" in x], r.stdout
    assert any("leftover" in x and NIC_IF in x and "down" in x for x in r.stdout.splitlines()), r.stdout


def test_verify_passes_a_retired_nic_that_was_unplugged(tmp_path):
    import shutil
    b = _converged(tmp_path)
    shutil.rmtree(b.sysfs / "class" / "net" / NIC_IF)
    assert not [x for x in _bad(b.run("strih_vban_leg_grade")) if "leftover" in x]


# --- 2. the gratuitous ARP ---------------------------------------------------------------------------------

def test_the_moved_leg_is_announced_after_it_reads_back_and_after_the_retirement(tmp_path):
    b = _box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    ann = _arp_announces(s)
    assert ann == [["-q", "-U", "-c", "2", "-I", VLAN_IF, ADDR]] * 2, ann
    full = s["log"]
    first_read = next(i for i, c in enumerate(full) if c[0] == "ip" and c[1:5] == ["-4", "-o", "addr", "show"]
                      and VLAN_IF in c)
    first_ann = next(i for i, c in enumerate(full) if c[0] == "arping" and "-U" in c)
    retire = next(i for i, c in enumerate(full) if c[0] == "nmcli" and OLD_UUID in c and "delete" in c)
    last_ann = max(i for i, c in enumerate(full) if c[0] == "arping" and "-U" in c)
    assert first_read < first_ann < retire < last_ann


def test_an_unchanged_leg_is_not_announced(tmp_path):
    b = _converged(tmp_path)
    assert b.run("strih_vban_leg_install").returncode == 0
    assert _arp_announces(b.st()) == []


def _dad_reapply_box(tmp_path, **over):
    """strih-snv 9.11.2026 19:04: the running leg's profile lacks a property setup adds -> modify + reapply."""
    b = _converged(tmp_path, **over)
    s = b.st()
    vlan = next(p for p in s["profiles"] if p["type"] == "vlan")
    vlan["props"]["ipv4.dad-timeout"] = ""
    s["applied"][VLAN_IF]["ipv4.dad-timeout"] = ""
    b.state.write_text(json.dumps(s))
    return b


def test_a_reapplied_leg_is_announced_once(tmp_path):
    b = _dad_reapply_box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert fk.calls(s, "nmcli", "device", "reapply", VLAN_IF)
    assert _arp_announces(s) == [["-q", "-U", "-c", "2", "-I", VLAN_IF, ADDR]]


def test_the_rig_address_and_the_rig_nic_are_never_announced(tmp_path):
    b = _box(tmp_path)
    r = b.run('_strih_vban_leg_announce "$2" "$3" "$4"; _strih_vban_leg_announce "$5" "$6" "$4"'.replace(
        "$2", RIG_IF).replace("$3", ADDR).replace("$4", RIG_IF).replace("$5", VLAN_IF).replace("$6", "10.77.9.204"))
    assert r.returncode == 0, r.stderr
    assert _arp_announces(b.st()) == []


def test_a_non_iputils_arping_is_a_warning_never_a_wrong_command(tmp_path):
    b = _box(tmp_path, arping_version="ARPing 2.21, by Thomas Habets")
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert _arp_announces(b.st()) == []
    assert any("WARN" in x and "gratuitous ARP" in x and "iputils" in x for x in r.stdout.splitlines()), r.stdout


def test_a_failed_announce_is_a_warning(tmp_path):
    b = _box(tmp_path, arping_rc=2)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert any("WARN" in x and "gratuitous ARP" in x and "rc 2" in x for x in r.stdout.splitlines()), r.stdout


# --- 3. the running hub after a leg change ----------------------------------------------------------------

AFTER = "strih_vban_leg_install && strih_vban_leg_after_change newlevel"


def _hub(state, active=True):
    state.setdefault("units", {})["intercom-hub"] = {"enabled": True, "active": active}
    return state


def test_a_leg_reapply_restarts_the_running_hub_after_the_leg_is_final(tmp_path):
    b = _dad_reapply_box(tmp_path)
    s = _hub(b.st())
    b.state.write_text(json.dumps(s))
    r = b.run(AFTER)
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    full = s["log"]
    restart = [i for i, c in enumerate(full) if c[:3] == ["systemctl", "try-restart", "intercom-hub"]]
    assert len(restart) == 1, full
    last_nm = max(i for i, c in enumerate(full) if c[0] == "nmcli" and c[1:2] != ["-g"])
    assert last_nm < restart[0]
    assert "intercom-hub" in r.stdout and "restarted" in r.stdout


def test_a_stopped_hub_is_never_started(tmp_path):
    b = _dad_reapply_box(tmp_path)
    b.state.write_text(json.dumps(_hub(b.st(), active=False)))
    r = b.run(AFTER)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not [c for c in fk.calls(b.st(), "systemctl") if c[:1] in (["start"], ["restart"], ["try-restart"])
                and "intercom-hub" in c]


def test_an_unchanged_leg_restarts_nothing(tmp_path):
    b = _converged(tmp_path)
    b.state.write_text(json.dumps(_hub(b.st())))
    r = b.run(AFTER)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not [c for c in fk.calls(b.st(), "systemctl") if "intercom-hub" in c and c[:1] != ["is-active"]]


def test_a_hub_that_crash_loops_after_the_restart_fails_by_name(tmp_path):
    b = _dad_reapply_box(tmp_path)
    s = _hub(b.st())
    s["show"] = {"intercom-hub": "ActiveState=failed\nSubState=failed\nNRestarts=3\nResult=exit-code\n"}
    b.state.write_text(json.dumps(s))
    r = b.run(AFTER)
    assert r.returncode != 0 and "intercom-hub" in r.stderr and "failed" in r.stderr, r.stdout + r.stderr


def test_a_running_obs_on_a_dante_box_is_named_loudly(tmp_path):
    b = _dad_reapply_box(tmp_path, procs=["obs"])
    r = b.run(AFTER)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARNING" in r.stdout and "stream-obs" in r.stdout and "strih-obs.service" in r.stdout, r.stdout
    assert not [c for c in fk.calls(b.st(), "systemctl") if "strih-obs.service" in c], "OBS is never restarted here"


def test_setup_strih_restarts_after_the_leg_and_before_the_dante_toml():
    s = (t39.SETUP).read_text()
    a = s.index("strih_vban_leg_install ||")
    m = s.index('strih_vban_leg_after_change "$DESKTOP_USER" ||')
    d = s.index('strih_dante_input_install "$DESKTOP_USER" "$USER_HOME"')
    assert a < m < d
