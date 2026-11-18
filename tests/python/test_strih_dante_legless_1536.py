"""Issue 1536: a Dante box WITHOUT a VLAN leg (PP's flat 10.77.8.0/23 until the new switch arrives).

dstack 18.11.2026: PP boxes join Dante on their rig/LAN NIC now and move to a VLAN leg later; dstack's
dante-leg.nft must NEVER be loaded on that shared NIC (it would cut the box's LAN). So:
- the loader takes STRIH_DANTE_DEVICES with STRIH_PROGRAM_VBAN_LEG=none, and still refuses a Dante program
  box that also declares a VBAN program;
- the dante-input TOML iface is the rig NIC from the ONE resolver strih_lx_rig_nic (a leg box keeps the leg's);
- no leg filter is installed or graded there (the leg lib SKIPs / NOTEs), and strih_dante_leg_declared says no.

Every bash run sources the libs under the callers' `set -euo pipefail`; values reach it as $1.. only.
"""
import os
import subprocess
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE",
         "STRIH_DANTE_INPUT_KIND_FILE", "STRIH_VBAN_LEG_SYSFS", "STRIH_VBAN_LEG_PROC_SYS",
         "STRIH_VBAN_LEG_NFT_FILE", "STRIH_VBAN_LEG_UNIT_FILE", "STRIH_VBAN_LEG_SYSCTL_FILE",
         "STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "STRIH_DANTE_LEG_ETC", "STRIH_DANTE_LEG_UNIT_FILE",
         "STRIH_DANTE_AVAHI_CONF", "OBS_BOX_ONLINE_CPUS")
GRADE_FNS = 'ok() { printf "OK|%s\\n" "$*"; }\nbad() { printf "BAD|%s\\n" "$*"; }\nnote() { printf "NOTE|%s\\n" "$*"; }\n'
SITE = "# test site file (fake names)\nRX1=Program L|foh-tx|MASTER-L\nRX2=Program R|foh-tx|MASTER-R\n"
DEVICES = "strih-obs=4440:8800:4455"


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


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


LEGLESS_PP = {"STRIH_DANTE_DEVICES": DEVICES, "STRIH_PROGRAM_VBAN_STREAMS": "none",
              "STRIH_PROGRAM_VBAN_PORT": "none", "STRIH_PROGRAM_VBAN_RESERVE_MS": "none",
              "STRIH_DANTE_NETWORK_LATENCY_MS": "1", "STRIH_DANTE_RECEIVE_BUFFER_MS": "12.041666"}


def _legless(tmp_path, **extra):
    pairs = dict(LEGLESS_PP)
    pairs.update(extra)
    return _boxes(tmp_path, "strih-pp", pairs)


def _sysfs(tmp_path, nics):
    """A fake /sys with NAME -> driver for each NIC (class/net/<if>/device/driver -> .../drivers/<drv>)."""
    root = tmp_path / "sys"
    for ifc, drv in nics.items():
        d = root / "bus" / "pci" / "drivers" / drv
        d.mkdir(parents=True, exist_ok=True)
        dev = root / "devices" / ifc
        dev.mkdir(parents=True, exist_ok=True)
        (dev / "driver").symlink_to(d)
        net = root / "class" / "net" / ifc
        net.mkdir(parents=True, exist_ok=True)
        (net / "device").symlink_to(dev)
    return root


# --- the loader ---------------------------------------------------------------------------------------

def test_the_loader_takes_a_dante_device_without_a_leg(tmp_path):
    d = _legless(tmp_path)
    r = _bash('strih_box_load strih-pp >/dev/null\nstrih_lx_dante_devices\nstrih_lx_program_vban_leg',
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:2] == [DEVICES, "none"]


def test_a_legless_dante_program_box_with_a_vban_program_is_still_refused(tmp_path):
    d = _legless(tmp_path, STRIH_PROGRAM_VBAN_STREAMS="strih-obs", STRIH_PROGRAM_VBAN_PORT="6981",
                 STRIH_PROGRAM_VBAN_RESERVE_MS="40")
    r = _bash('strih_box_load strih-pp', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "is the program source" in r.stderr, r.stderr


def test_a_legless_dante_box_still_refuses_a_leg_vban_sender(tmp_path):
    d = _legless(tmp_path, STRIH_PROGRAM_VBAN_SENDER="10.77.8.230")
    r = _bash('strih_box_load strih-pp', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0, r.stderr


def test_the_real_fact_files_all_still_load():
    for box in sorted(p.stem for p in BOXES.glob("*.env")):
        r = _bash('strih_box_load "$1" >/dev/null', box, env={"STRIH_BOXES_DIR": str(BOXES)})
        assert r.returncode == 0, (box, r.stderr)


# --- the iface + the TOML -----------------------------------------------------------------------------

def test_the_dante_input_iface_of_a_legless_box_is_the_rig_nic(tmp_path):
    d = _legless(tmp_path)
    sysfs = _sysfs(tmp_path, {"enp5s0": "r8169", "wlp3s0": "iwlwifi"})
    r = _bash('strih_box_load strih-pp >/dev/null\n_strih_dante_input_iface',
              env={"STRIH_BOXES_DIR": str(d), "STRIH_VBAN_LEG_SYSFS": str(sysfs)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp5s0"


def test_an_unresolvable_rig_nic_fails_by_name_never_a_guess(tmp_path):
    d = _legless(tmp_path)
    sysfs = _sysfs(tmp_path, {"enp5s0": "r8169", "enp6s0": "r8169"})   # two r8169: MULTI
    r = _bash('strih_box_load strih-pp >/dev/null\n_strih_dante_input_iface',
              env={"STRIH_BOXES_DIR": str(d), "STRIH_VBAN_LEG_SYSFS": str(sysfs)})
    assert r.returncode != 0 and r.stdout.strip() == "", r.stdout
    assert "rig" in r.stderr and "STRIH_NIC_IFACE" in r.stderr, r.stderr


def test_a_leg_box_keeps_the_legs_interface(tmp_path):
    sysfs = _sysfs(tmp_path, {"enp7s0": "atlantic"})
    r = _bash('strih_box_load strih-snv >/dev/null\n_strih_dante_input_iface',
              env={"STRIH_BOXES_DIR": str(BOXES), "STRIH_VBAN_LEG_SYSFS": str(sysfs)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "enp7s0.70"


def test_install_writes_the_toml_on_the_rig_nic(tmp_path):
    d = _legless(tmp_path)
    sysfs = _sysfs(tmp_path, {"enp5s0": "r8169"})
    dest = tmp_path / "home" / ".camera-box" / "dante-input.toml"
    site = tmp_path / "site.conf"
    site.write_text(SITE)
    user = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    group = subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip()
    root = tmp_path / "root"
    (root / "etc").mkdir(parents=True)
    port = root / "proc" / "sys" / "net" / "ipv4" / "ip_unprivileged_port_start"
    port.parent.mkdir(parents=True)
    port.write_text("1024\n")
    (tmp_path / "bin").mkdir()
    sc = tmp_path / "bin" / "systemctl"
    sc.write_text("#!/bin/bash\nexit 0\n")
    sc.chmod(0o755)
    env = {"STRIH_BOXES_DIR": str(d), "STRIH_VBAN_LEG_SYSFS": str(sysfs), "STRIH_DANTE_INPUT_TOML": str(dest),
           "STRIH_DANTE_INPUT_SITE_FILE": str(site), "STRIH_DANTE_RX_PIN_ROOT": str(root),
           "STRIH_DANTE_RX_PIN_OWNER": "%s:%s" % (user, group),
           "PATH": "%s:%s" % (tmp_path / "bin", os.environ["PATH"])}
    r = _bash('. scripts/lib/obs-box-baseline.sh\nstrih_box_load strih-pp >/dev/null\nstrih_dante_input_install "$1" "$2"',
              user, tmp_path / "home", env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    t = tomllib.loads(dest.read_text())
    assert t["iface"] == "enp5s0" and t["name"] == "strih-obs", t
    assert "strih-obs on enp5s0" in r.stdout


# --- no leg filter on the shared NIC ------------------------------------------------------------------

def _logging_stubs(tmp_path):
    b = tmp_path / "stubs"
    b.mkdir()
    log = tmp_path / "calls.log"
    for name in ("nmcli", "nft", "systemctl", "sysctl", "unshare", "apt-get", "ip"):
        f = b / name
        f.write_text('#!/bin/bash\nprintf "%%s %%s\\n" "%s" "$*" >> "%s"\nexit 0\n' % (name, log))
        f.chmod(0o755)
    return b, log


def test_a_legless_dante_box_declares_no_dante_leg(tmp_path):
    d = _legless(tmp_path)
    r = _bash('strih_box_load strih-pp >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi',
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "NO"
    r = _bash('strih_box_load strih-snv >/dev/null\nif strih_dante_leg_declared; then echo YES; else echo NO; fi',
              env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.stdout.strip() == "YES", r.stderr


def test_setup_installs_no_leg_filter_on_a_legless_dante_box(tmp_path):
    d = _legless(tmp_path)
    stubs, log = _logging_stubs(tmp_path)
    etc = tmp_path / "etc-dstack"
    r = _bash('strih_box_load strih-pp >/dev/null\nstrih_vban_leg_install',
              env={"STRIH_BOXES_DIR": str(d), "STRIH_DANTE_LEG_ETC": str(etc),
                   "STRIH_VBAN_LEG_NFT_FILE": str(tmp_path / "leg.nft"),
                   "PATH": "%s:%s" % (stubs, os.environ["PATH"])})
    assert r.returncode == 0, r.stdout + r.stderr
    assert "SKIP" in r.stdout
    calls = log.read_text() if log.exists() else ""
    assert calls == "", calls
    assert not etc.exists() and not (tmp_path / "leg.nft").exists()


def test_verify_grades_a_legless_dante_box_without_a_fail(tmp_path):
    d = _legless(tmp_path)
    stubs, log = _logging_stubs(tmp_path)
    r = _bash(GRADE_FNS + 'strih_box_load strih-pp >/dev/null\nstrih_vban_leg_grade',
              env={"STRIH_BOXES_DIR": str(d), "PATH": "%s:%s" % (stubs, os.environ["PATH"])})
    assert r.returncode == 0, r.stdout + r.stderr
    rows = [l for l in r.stdout.splitlines() if l]
    assert rows and all(l.startswith("NOTE|") for l in rows), rows
    assert "dante-leg" not in r.stdout
