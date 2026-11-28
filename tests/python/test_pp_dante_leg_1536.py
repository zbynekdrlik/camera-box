"""Issue 1536 (18.11.2026): PP on its new switch = SNV's shape.

Live facts: PP's switch trunks the OBS boxes' rig ports, the rig LAN 10.77.8.0/23 untagged + the Dante
VLAN 70 (10.77.7.0/24) tagged; the Dante GM 10.77.7.200 ("dante-ptp") and dsg 10.77.7.50 sit on VLAN 70, and
the switch also copies dante-ptp's PTP onto the LAN untagged. The flat-LAN GM 10.77.8.110 is gone. So dsync
stays on the rig NIC exactly like SNV and follows 10.77.7.200 there (supervisor correction 18.11.2026 17:13);
dasio: dsg sends the console program as "STREAM master L/R", so strih-pp / stream-pp take it over Dante like
strih-snv / stream-snv.

What this pins:
  * the PP fact files: the vlan:70 legs (.202 / .204), the GM 10.77.7.200, one `<role>-obs` device, no
    VBAN program, the named terms 1 / 12.041666, no TX; stream-pp's USB rig NIC renamed rig0 (its
    15-character kernel name has no room for `.70`);
  * the leg is a Dante leg on both PP boxes (dstack's generic filter, the /24 in table 70), on
    `enp5s0f1.70` / `rig0.70`, where the dante-input TOML binds too;
  * dsync keeps the rig NIC: strih-pp's unit is exactly the hand-set live `dsync --interface enp5s0f1`
    (setup's next run keeps the daemon), stream-pp the strih-pp.lan client on rig0.

Every bash run sources the libs under the callers' `set -euo pipefail`; values reach it as $1.. only.
"""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
DS_LIB = SCRIPTS / "lib" / "strih-dantesync.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_VBAN_LEG_SYSFS", "STRIH_DANTESYNC_SYSFS", "RIG_GRANDMASTER_IP")


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n. "%s"\n%s' % (PROVISION, DS_LIB, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _facts(box):
    out = {}
    for line in (BOXES / ("%s.env" % box)).read_text().splitlines():
        if line and not line.startswith("#"):
            k, _, v = line.partition("=")
            out[k] = v
    return out


def _sysfs(tmp_path, nics):
    """A fake /sys with NAME -> driver for each NIC (class/net/<if>/device/driver -> .../drivers/<drv>);
    a None driver = a VLAN (class/net/<if> only, no device link)."""
    root = tmp_path / "sys"
    for ifc, drv in nics.items():
        net = root / "class" / "net" / ifc
        net.mkdir(parents=True, exist_ok=True)
        if drv is None:
            continue
        d = root / "bus" / "pci" / "drivers" / drv
        d.mkdir(parents=True, exist_ok=True)
        dev = root / "devices" / ifc
        dev.mkdir(parents=True, exist_ok=True)
        (dev / "driver").symlink_to(d)
        (net / "device").symlink_to(dev)
    return root


def _env(sysfs, addrs=""):
    """ADDRS = the `ip -o -4 addr show` text a stub `ip` prints (the live box's rig address, for the
    STRIH_IP fallback when two NICs share the driver)."""
    b = sysfs.parent / "bin"
    b.mkdir(exist_ok=True)
    (b / "ip.out").write_text(addrs)
    ip = b / "ip"
    ip.write_text('#!/bin/bash\ncat "%s"\n' % (b / "ip.out"))
    ip.chmod(0o755)
    return {"STRIH_BOXES_DIR": str(BOXES), "STRIH_VBAN_LEG_SYSFS": str(sysfs), "STRIH_DANTESYNC_SYSFS": str(sysfs),
            "PATH": "%s:%s" % (b, os.environ["PATH"])}


# strih-pp's X520-2 has two ixgbe ports, so the driver match reads MULTI and the rig NIC resolves by STRIH_IP
STRIH_PP_NICS = {"enp5s0f0": "ixgbe", "enp5s0f1": "ixgbe"}
STRIH_PP_ADDRS = ("1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever\n"
                  "4: enp5s0f1    inet 10.77.8.202/23 brd 10.77.9.255 scope global noprefixroute enp5s0f1\\"
                  "       valid_lft forever preferred_lft forever\n")


# --- the PP facts --------------------------------------------------------------------------------

# intercom#32 (owner: PP at SNV's level): the strih-pp hub carries its own Dante device in dstack slot 2 (never
# SNV's slot 1), strih-obs keeps slot 0; the hub's TOML / subscriptions are the intercom project's
@pytest.mark.parametrize("box,addr,device", [("strih-pp", "10.77.7.202", "strih-pp-intercom=4442:8802:4457 strih-obs=4440:8800:4455"),
                                             ("stream-pp", "10.77.7.204", "stream-obs=4440:8800:4455")])
def test_the_pp_fact_files_are_the_snv_shape(box, addr, device):
    f = _facts(box)
    assert f["STRIH_PROGRAM_VBAN_LEG"] == "vlan:70"
    assert f["STRIH_PROGRAM_VBAN_LEG_ADDR"] == addr
    assert f["STRIH_DANTE_GM"] == "video-clock.lan" and f["STRIH_DANTE_GM_ADDR"] == "10.77.7.200"  # issue 1553
    assert f["STRIH_DANTE_DEVICES"] == device
    for k in ("STRIH_PROGRAM_VBAN_STREAMS", "STRIH_PROGRAM_VBAN_PORT", "STRIH_PROGRAM_VBAN_RESERVE_MS",
              "STRIH_PROGRAM_VBAN_SENDER", "STRIH_DANTE_TX", "STRIH_DANTE_TX_TRACK"):
        assert f[k] == "none", k
    assert f["STRIH_DANTE_NETWORK_LATENCY_MS"] == "1"
    assert f["STRIH_DANTE_RECEIVE_BUFFER_MS"] == "12.041666"


def test_the_pp_boxes_keep_their_program_input_names_and_clock_roles():
    s, t = _facts("strih-pp"), _facts("stream-pp")
    assert s["STRIH_PROGRAM_AUDIO_INPUT"] == "ASIO zvuk" and t["STRIH_PROGRAM_AUDIO_INPUT"] == "Zvuk z mixu"
    assert (s["STRIH_DANTESYNC_ROLE"], s["STRIH_DANTESYNC_UPSTREAM"]) == ("server", "")
    assert (t["STRIH_DANTESYNC_ROLE"], t["STRIH_DANTESYNC_UPSTREAM"]) == ("client", "strih-pp.lan")


def test_the_pp_dante_terms_equal_snvs():
    for k in ("STRIH_DANTE_NETWORK_LATENCY_MS", "STRIH_DANTE_RECEIVE_BUFFER_MS"):
        assert _facts("strih-pp")[k] == _facts("strih-snv")[k] == _facts("stream-pp")[k], k


def test_stream_pp_renames_its_usb_rig_nic_to_rig0():
    f = _facts("stream-pp")
    # enx6c1ff7618e74 is 15 characters, so its `.70` could not exist: the issue-1502 rename (its MAC is the name)
    assert f["STRIH_NIC_RENAME"] == "6c:1f:f7:61:8e:74=rig0"
    assert f["STRIH_NIC_DRIVER"] == "r8152"
    assert _facts("strih-pp")["STRIH_NIC_RENAME"] == "none"   # enp5s0f1.70 fits


@pytest.mark.parametrize("box", ["strih-pp", "stream-pp", "strih-snv", "stream-snv"])
def test_every_box_loads(box):
    r = _bash('strih_box_load "$1" >/dev/null', box, env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr


# --- the leg ---------------------------------------------------------------------------------------

def test_the_pp_legs_are_dante_legs():
    for box in ("strih-pp", "stream-pp"):
        r = _bash('strih_box_load "$1" >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi\n'
                  '_strih_vban_leg_plen', box, env={"STRIH_BOXES_DIR": str(BOXES)})
        assert r.returncode == 0, (box, r.stderr)
        assert r.stdout.split() == ["YES", "24"], box


@pytest.mark.parametrize("box,nics,addrs,want", [
    ("strih-pp", STRIH_PP_NICS, STRIH_PP_ADDRS, "enp5s0f1.70"),
    ("stream-pp", {"rig0": "r8152"}, "", "rig0.70"),
])
def test_the_leg_and_the_dante_input_bind_the_vlan_on_the_rig_nic(tmp_path, box, nics, addrs, want):
    sysfs = _sysfs(tmp_path, nics)
    r = _bash('strih_box_load "$1" >/dev/null\nstrih_vban_leg_resolve\n_strih_dante_input_iface', box,
              env=_env(sysfs, addrs))
    assert r.returncode == 0, r.stderr
    leg, iface = r.stdout.strip().splitlines()
    assert leg.split()[0] == want and iface == want


def test_stream_pp_before_the_rename_resolves_no_leg(tmp_path):
    """Until rig0 exists the rig NIC resolves nothing (issue 1502): never the adapter under its old name."""
    sysfs = _sysfs(tmp_path, {"enx6c1ff7618e74": "r8152"})
    r = _bash('strih_box_load stream-pp >/dev/null\nstrih_vban_leg_resolve', env=_env(sysfs))
    assert r.returncode != 0 and r.stdout.strip() == "", r.stdout
    assert "rig NIC cannot be resolved" in r.stderr, r.stderr


# --- dsync binds the Dante VLAN at PP (dstack #487, 28.11.2026) -----------------------------------------
# The PP grandmaster dante-ptp (10.77.7.200) lives on VLAN 70. Bound to the untagged LAN, dsync heard it only
# through the router's igmp-proxy; no IGMP query reaches that VLAN, so the switch dropped the 224.0.1.129
# membership 260 s after each join: `no PTP announce for 10s -- re-joining` every ~271 s (strih-pp 206x and
# stream-pp 205x on 28.11). Fact STRIH_DANTESYNC_PTP_IFACE=leg = the PTP interface is the box's Dante leg.

def _unit(tmp_path, box, nics, addrs=""):
    sysfs = _sysfs(tmp_path, nics)
    return _bash('strih_box_load "$1" >/dev/null\n'
                 't="$(strih_dantesync_unit_text "$(strih_lx_dantesync_role)" "$(strih_lx_dantesync_args)" '
                 '"$(strih_dantesync_rig_nic)")"\nstrih_dantesync_exec_start "$t"', box, env=_env(sysfs, addrs))


def test_the_strih_pp_unit_binds_dsync_to_the_dante_vlan(tmp_path):
    r = _unit(tmp_path, "strih-pp", dict(STRIH_PP_NICS, **{"enp5s0f1.70": None}), STRIH_PP_ADDRS)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "/usr/local/bin/dsync --interface enp5s0f1.70\n"


def test_the_stream_pp_unit_is_the_strih_pp_client_on_rig0_vlan_70(tmp_path):
    r = _unit(tmp_path, "stream-pp", {"rig0": "r8152", "rig0.70": None})
    assert r.returncode == 0, r.stderr
    assert r.stdout == "/usr/local/bin/dsync --ntp-server strih-pp.lan --interface rig0.70\n"


def test_a_box_without_the_fact_keeps_dsync_on_the_rig_nic(tmp_path):
    """SNV's grandmaster is on the untagged LAN (10.77.9.230): strih-snv keeps --interface <rig NIC>."""
    r = _unit(tmp_path, "strih-snv", {"enp7s0": "atlantic", "enp7s0.70": None})
    assert r.returncode == 0, r.stderr
    assert r.stdout.rstrip("\n").endswith("--interface enp7s0"), r.stdout


# --- the launch seed switches the PP program inputs to the Dante source -----------------------------------

@pytest.mark.parametrize("box,fn,device,name", [
    ("strih-pp", "strih_lx_seed_manifest_json", "strih-obs", "ASIO zvuk"),
    ("stream-pp", "obs_box_seed_manifest_json", "stream-obs", "Zvuk z mixu"),
])
def test_the_pp_seed_manifest_takes_the_program_from_the_dante_device(box, fn, device, name):
    """The ported PP collections keep their obs-vban input (the collection-port presets are the VBAN
    rollback shape); the launch seed switches it to dante_input from this manifest, keeping its name."""
    import json
    r = _bash('strih_box_load "$1" >/dev/null\n"$2"', box, fn, env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr
    m = json.loads(r.stdout)
    assert m["program_audio_dante"] == {"device": device}
    assert m["program_audio_input"] == name and "program_audio_vban" not in m


def test_the_strih_pp_hub_device_is_graded_like_strih_snvs():
    """The `*-intercom=` device turns on item 16's `(intercom-dante)` rows (strih-intercom-hub-unit.sh) and the
    loader keeps every port distinct and the ARC ports in dstack's slots, exactly as on strih-snv."""
    r = _bash('strih_box_load strih-pp >/dev/null\nstrih_lx_dante_devices\nprintf "\\n%s\\n" "$(strih_dante_input_device)"\n'
              'strih_dante_input_ports "$(strih_lx_dante_devices)"', env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr
    lines = r.stdout.strip().splitlines()
    assert lines[0] == "strih-pp-intercom=4442:8802:4457 strih-obs=4440:8800:4455" and lines[1] == "strih-obs", lines
    snv = _facts("strih-snv")["STRIH_DANTE_DEVICES"].split()
    pp = _facts("strih-pp")["STRIH_DANTE_DEVICES"].split()
    # the hub's slot on PP differs from SNV's, the OBS device keeps slot 0 on both
    assert [d for d in pp if d.startswith("strih-obs=")] == [d for d in snv if d.startswith("strih-obs=")]
    assert {d.split("=")[1] for d in pp if "-intercom=" in d}.isdisjoint({d.split("=")[1] for d in snv if "-intercom=" in d})
