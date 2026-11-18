"""Issue 1536 (18.11.2026): PP on its new switch = SNV's shape.

Live facts: PP's switch trunks the OBS boxes' rig ports, the rig LAN 10.77.8.0/23 untagged + the Dante
VLAN 70 (10.77.7.0/24) tagged; the Dante GM 10.77.7.200 and dsg 10.77.7.50 sit on VLAN 70, the old
flat-LAN GM 10.77.8.110 is gone. dstack (owner of dsync): strih-pp's dsync binds the Dante leg
(<rig>.70) and follows the Dante GM; one clock domain at PP. dasio: dsg sends the console program as
"STREAM master L/R", so strih-pp / stream-pp take it over Dante like strih-snv / stream-snv.

What this pins:
  * the PP fact files: the vlan:70 legs (.202 / .204), the GM 10.77.7.200, one `<role>-obs` device,
    no VBAN program, the named terms 1 / 12.041666, no TX;
  * `strih_dsync_gm_on_leg LEG ADDR GM` (pure): dsync follows the GM on the leg when the box has a leg
    and the GM is an IPv4 address inside the leg address's /24; a GM name (video-clock.lan) never is;
  * `strih_dsync_on_leg` on the real boxes: the PP boxes yes, the SNV boxes no (byte-identical);
  * `strih_dante_leg_declared`: a leg whose dsync follows the GM is a Dante leg even without Dante
    devices (dstack's generic filter admits PTP; the issue-1439 VBAN-only table would drop it);
  * the loader refuses a leg VBAN sender while dsync follows the GM on the leg;
  * `strih_dantesync_ptp_nic`: the leg interface on a PP box (a NOTE when it does not exist yet), the
    rig NIC on an SNV box; setup step 2 and verify items 6c/6d all call it.

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
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
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


def _boxes(tmp_path, box, pairs):
    """A copy of the real fact files with BOX's KEY=value lines replaced (each key must exist once)."""
    d = tmp_path / "boxes"
    d.mkdir(exist_ok=True)
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    f = d / ("%s.env" % box)
    lines = f.read_text().splitlines()
    for k, v in pairs.items():
        assert sum(1 for l in lines if l.startswith(k + "=")) == 1, k
        lines = ["%s=%s" % (k, v) if l.startswith(k + "=") else l for l in lines]
    f.write_text("\n".join(lines) + "\n")
    return d


def _sysfs(tmp_path, nics):
    """A fake /sys with NAME -> driver for each NIC (class/net/<if>/device/driver -> .../drivers/<drv>);
    a NAME with a dot is a VLAN (class/net/<if> only, no device link)."""
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


# --- the PP facts --------------------------------------------------------------------------------

@pytest.mark.parametrize("box,addr,device", [("strih-pp", "10.77.7.202", "strih-obs=4440:8800:4455"),
                                             ("stream-pp", "10.77.7.204", "stream-obs=4440:8800:4455")])
def test_the_pp_fact_files_are_the_snv_shape(box, addr, device):
    f = _facts(box)
    assert f["STRIH_PROGRAM_VBAN_LEG"] == "vlan:70"
    assert f["STRIH_PROGRAM_VBAN_LEG_ADDR"] == addr
    assert f["STRIH_DANTE_GM"] == "10.77.7.200"
    assert f["STRIH_DANTE_DEVICES"] == device
    for k in ("STRIH_PROGRAM_VBAN_STREAMS", "STRIH_PROGRAM_VBAN_PORT", "STRIH_PROGRAM_VBAN_RESERVE_MS",
              "STRIH_PROGRAM_VBAN_SENDER", "STRIH_DANTE_TX", "STRIH_DANTE_TX_TRACK"):
        assert f[k] == "none", k
    assert f["STRIH_DANTE_NETWORK_LATENCY_MS"] == "1"
    assert f["STRIH_DANTE_RECEIVE_BUFFER_MS"] == "12.041666"


def test_the_pp_boxes_keep_their_program_input_names():
    assert _facts("strih-pp")["STRIH_PROGRAM_AUDIO_INPUT"] == "ASIO zvuk"
    assert _facts("stream-pp")["STRIH_PROGRAM_AUDIO_INPUT"] == "Zvuk z mixu"


def test_the_pp_dante_terms_equal_snvs():
    for k in ("STRIH_DANTE_NETWORK_LATENCY_MS", "STRIH_DANTE_RECEIVE_BUFFER_MS"):
        assert _facts("strih-pp")[k] == _facts("strih-snv")[k] == _facts("stream-pp")[k], k


@pytest.mark.parametrize("box", ["strih-pp", "stream-pp", "strih-snv", "stream-snv"])
def test_every_box_loads(box):
    r = _bash('strih_box_load "$1" >/dev/null', box, env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr


# --- the rule: dsync follows the GM on the leg ---------------------------------------------------

@pytest.mark.parametrize("leg,addr,gm,want", [
    ("vlan:70", "10.77.7.202", "10.77.7.200", True),
    ("mac:00:e0:4c:68:01:85", "10.77.7.204", "10.77.7.200", True),
    ("vlan:70", "10.77.7.202", "video-clock.lan", False),     # a name: SNV, the rig NIC
    ("vlan:70", "10.77.7.202", "10.77.8.110", False),         # the GM on the LAN
    ("none", "none", "10.77.7.200", False),                   # no leg
    ("vlan:70", "none", "10.77.7.200", False),
    ("vlan:70", "10.77.7.202", "10.77.7.2000", False),        # not an IPv4 address
    ("vlan:70", "10.77.7.202", "10.77.70.200", False),        # a prefix of the /24, not inside it
    ("vlan:70", "10.77.7.202", "", False),
])
def test_dsync_gm_on_leg_rule(leg, addr, gm, want):
    r = _bash('if strih_dsync_gm_on_leg "$1" "$2" "$3"; then echo YES; else echo NO; fi', leg, addr, gm)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == ("YES" if want else "NO")


@pytest.mark.parametrize("box,want", [("strih-pp", "YES"), ("stream-pp", "YES"),
                                      ("strih-snv", "NO"), ("stream-snv", "NO")])
def test_dsync_on_leg_per_real_box(box, want):
    r = _bash('strih_box_load "$1" >/dev/null\nif strih_dsync_on_leg; then echo YES; else echo NO; fi', box,
              env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == want


# --- the leg's shape -----------------------------------------------------------------------------

# a PP box with its program back on obs-vban (the rollback shape): no Dante devices, the leg + the GM stay
VBAN_PP = {"STRIH_DANTE_DEVICES": "none", "STRIH_PROGRAM_VBAN_STREAMS": "strih-obs",
           "STRIH_PROGRAM_VBAN_PORT": "6981", "STRIH_PROGRAM_VBAN_RESERVE_MS": "40",
           "STRIH_DANTE_NETWORK_LATENCY_MS": "none", "STRIH_DANTE_RECEIVE_BUFFER_MS": "none"}


def test_a_leg_whose_dsync_follows_the_gm_is_a_dante_leg_without_devices(tmp_path):
    d = _boxes(tmp_path, "strih-pp", VBAN_PP)
    r = _bash('strih_box_load strih-pp >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi',
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "YES"
    r = _bash('strih_box_load strih-pp >/dev/null\n_strih_vban_leg_plen', env={"STRIH_BOXES_DIR": str(d)})
    assert r.stdout.strip() == "24", r.stderr


def test_a_leg_with_the_gm_off_it_and_no_devices_stays_vban_only(tmp_path):
    pairs = dict(VBAN_PP, STRIH_DANTE_GM="video-clock.lan")
    d = _boxes(tmp_path, "strih-pp", pairs)
    r = _bash('strih_box_load strih-pp >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi',
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "NO"


def test_the_real_pp_boxes_declare_a_dante_leg():
    for box in ("strih-pp", "stream-pp"):
        r = _bash('strih_box_load "$1" >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi',
                  box, env={"STRIH_BOXES_DIR": str(BOXES)})
        assert r.stdout.strip() == "YES", (box, r.stderr)


def test_the_loader_refuses_a_leg_vban_sender_while_dsync_follows_the_gm_on_the_leg(tmp_path):
    d = _boxes(tmp_path, "strih-pp", dict(VBAN_PP, STRIH_PROGRAM_VBAN_SENDER="10.77.7.30"))
    r = _bash('strih_box_load strih-pp', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0
    assert "STRIH_PROGRAM_VBAN_SENDER must be none while dsync follows STRIH_DANTE_GM" in r.stderr, r.stderr


def test_the_same_sender_loads_with_the_gm_off_the_leg(tmp_path):
    pairs = dict(VBAN_PP, STRIH_PROGRAM_VBAN_SENDER="10.77.7.30", STRIH_DANTE_GM="video-clock.lan")
    d = _boxes(tmp_path, "strih-pp", pairs)
    r = _bash('strih_box_load strih-pp >/dev/null', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr


# --- the dsync PTP NIC -----------------------------------------------------------------------------

def _ptp_nic(tmp_path, box, nics):
    sysfs = _sysfs(tmp_path, nics)
    return _bash('strih_box_load "$1" >/dev/null\nstrih_dantesync_ptp_nic', box,
                 env={"STRIH_BOXES_DIR": str(BOXES), "STRIH_VBAN_LEG_SYSFS": str(sysfs),
                      "STRIH_DANTESYNC_SYSFS": str(sysfs)})


def test_the_pp_dsync_binds_the_leg(tmp_path):
    r = _ptp_nic(tmp_path, "strih-pp", {"enp5s0f1": "ixgbe", "enp5s0f1.70": None})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp5s0f1.70"
    assert "NOTE" not in r.stderr, r.stderr


def test_a_leg_that_does_not_exist_yet_is_named_and_still_bound(tmp_path):
    r = _ptp_nic(tmp_path, "strih-pp", {"enp5s0f1": "ixgbe"})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp5s0f1.70"
    assert "NOTE" in r.stderr and "enp5s0f1.70" in r.stderr and "step 13" in r.stderr, r.stderr


def test_an_unresolvable_rig_nic_fails_the_leg_by_name(tmp_path):
    r = _ptp_nic(tmp_path, "strih-pp", {"enp5s0f0": "ixgbe", "enp5s0f1": "ixgbe"})
    assert r.returncode != 0 and r.stdout.strip() == "", r.stdout
    assert "STRIH_NIC_IFACE" in r.stderr, r.stderr


def test_the_snv_dsync_keeps_the_rig_nic(tmp_path):
    r = _ptp_nic(tmp_path, "strih-snv", {"enp7s0": "atlantic", "enp7s0.70": None})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp7s0"


def test_the_pp_unit_binds_the_leg(tmp_path):
    sysfs = _sysfs(tmp_path, {"enp5s0f1": "ixgbe", "enp5s0f1.70": None})
    r = _bash('strih_box_load strih-pp >/dev/null\n'
              'strih_dantesync_unit_text "$(strih_lx_dantesync_role)" "$(strih_lx_dantesync_args)" "$(strih_dantesync_ptp_nic)"',
              env={"STRIH_BOXES_DIR": str(BOXES), "STRIH_VBAN_LEG_SYSFS": str(sysfs),
                   "STRIH_DANTESYNC_SYSFS": str(sysfs)})
    assert r.returncode == 0, r.stderr
    assert "ExecStart=/usr/local/bin/dsync --interface enp5s0f1.70\n" in r.stdout, r.stdout


def test_the_resolver_refuses_when_the_leg_lib_is_not_loaded():
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_dantesync_ptp_nic', "h", str(DS_LIB)],
                       capture_output=True, text=True, cwd=str(REPO), timeout=60)
    assert r.returncode != 0 and r.stdout.strip() == "", r.stdout
    assert "strih_dsync_on_leg" in r.stderr, r.stderr


def test_setup_step_2_and_verify_6c_6d_use_the_ptp_nic():
    s = SETUP.read_text()
    assert s.count('"$(strih_dantesync_ptp_nic)"') == 1
    assert "strih_dantesync_rig_nic" not in s
    v = VERIFY.read_text()
    assert v.count("strih_dantesync_ptp_nic 2>/dev/null") == 2
    assert "strih_dantesync_rig_nic" not in v
