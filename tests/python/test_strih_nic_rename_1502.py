"""issue 1502 (lever 3, ROZHODNUTE 6849618093, owner 13.11.2026): stream-snv's rig NIC moves to a USB
2.5 GbE adapter (RTL8156, in-tree r8152) whose predictable name enx00e04c680185 is 15 characters, so its
VLAN 70 leg `<rig nic>.70` cannot exist. The box fact STRIH_NIC_RENAME names it rig0 through a systemd
.link file (scripts/lib/strih-nic-rename.sh): setup-strih step 1c writes it, verify-strih item 36 grades
it, and a detached switch program moves the box's two NetworkManager profiles with an automatic rollback.

Every function runs under the callers' `set -euo pipefail`, with every path behind a seam (never the
real /etc or /sys) and fake `udevadm` / `nmcli` / `ping` / `arping` first on PATH.
"""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
RENAME = SCRIPTS / "lib" / "strih-nic-rename.sh"
NICDRV = SCRIPTS / "lib" / "strih-nic-driver.sh"
PROV = SCRIPTS / "lib" / "strih-provision.sh"
BOXES = SCRIPTS / "strih-boxes"
MAC = "00:e0:4c:68:01:85"
OLDNAME = "enx00e04c680185"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN", "STRIH_NIC_IFACE",
         "STRIH_NIC_RENAME_LINK", "STRIH_NIC_RENAME_SYSROOT", "STRIH_NIC_RENAME_NM_RETIRED")


def _bash(body, *args, env=None, sources=(FACTS, RENAME), path_dir=None):
    """Source `sources` under the callers' `set -euo pipefail`, then run `body` ($1.. = args)."""
    harness = "set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_dir:
        full["PATH"] = "%s:%s" % (path_dir, full["PATH"])
    if env:
        full.update(env)
    return subprocess.run(["bash", "-c", harness, "harness", *args], capture_output=True, text=True,
                          env=full, cwd=str(REPO), timeout=120)


def _facts(box):
    out = {}
    for ln in (BOXES / ("%s.env" % box)).read_text().splitlines():
        if ln and not ln.startswith("#") and "=" in ln:
            k, v = ln.split("=", 1)
            out[k] = v
    return out


def _stub(bin_dir, name, body):
    bin_dir.mkdir(parents=True, exist_ok=True)
    f = bin_dir / name
    f.write_text("#!/usr/bin/env bash\n" + body)
    f.chmod(0o755)
    return f


def _udevadm(tmp_path):
    log = tmp_path / "udevadm.log"
    _stub(tmp_path / "bin", "udevadm", 'printf "%%s\\n" "$*" >> "%s"\n' % log)
    return log


def _nic(sysroot, name, mac, driver="r8152", usb=True):
    """A fake /sys/class/net/<name> with a device link (driver + a USB parent when usb)."""
    devroot = sysroot / "devices" / ("usb-" + name if usb else "pci-" + name)
    port = devroot / "2-1"
    iface = port / "2-1:1.0"
    iface.mkdir(parents=True, exist_ok=True)
    if usb:
        (port / "idVendor").write_text("0bda\n")
        (port / "speed").write_text("5000\n")
    drv = sysroot / "bus" / "drivers" / driver
    drv.mkdir(parents=True, exist_ok=True)
    os.symlink(drv, iface / "driver")
    net = sysroot / "class" / "net" / name
    net.mkdir(parents=True, exist_ok=True)
    (net / "address").write_text(mac + "\n")
    (net / "speed").write_text("2500\n")
    os.symlink(iface, net / "device")
    return net


# --- the facts ---------------------------------------------------------------------------------------

def test_stream_snv_declares_the_usb_25g_adapter_renamed_rig0():
    f = _facts("stream-snv")
    assert (f["STRIH_NIC_DRIVER"], f["STRIH_NIC_OOT_DRIVER"], f["STRIH_NIC_MIN_USB_MBPS"],
            f["STRIH_NIC_MIN_LINK_MBPS"]) == ("r8152", "none", "5000", "2500")
    assert f["STRIH_NIC_RENAME"] == MAC + "=rig0"
    assert f["STRIH_PROGRAM_VBAN_LEG"] == "vlan:70"
    assert len("rig0." + f["STRIH_PROGRAM_VBAN_LEG"].split(":")[1]) <= 15


@pytest.mark.parametrize("box", ["strih-snv", "strih-pp", "stream-pp"])
def test_every_other_box_declares_no_rename(box):
    assert _facts(box)["STRIH_NIC_RENAME"] == "none"


def test_the_fact_is_the_last_key_and_every_box_loads():
    r = _bash('strih_box_fact_keys | tail -n 1\nfor b in strih-snv strih-pp stream-pp stream-snv; do '
              'strih_box_load "$b" >/dev/null; printf "%s %s\\n" "$b" "$(strih_lx_nic_rename)"; done')
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines[0] == "STRIH_NIC_RENAME"
    assert "stream-snv %s=rig0" % MAC in lines


@pytest.mark.parametrize("value,ok", [
    ("none", True),
    (MAC + "=rig0", True),
    (MAC + "=a", True),
    (MAC + "=abcdefghij", True),         # 10 characters: <name>.4094 = 15
    (MAC + "=abcdefghijk", False),       # 11: <name>.4094 would be 16
    ("00:E0:4c:68:01:85=rig0", False),   # upper case
    ("01:e0:4c:68:01:85=rig0", False),   # a multicast MAC
    ("00:00:00:00:00:00=rig0", False),
    ("00:e0:4c:68:01=rig0", False),
    (MAC, False),
    ("=rig0", False),
    (MAC + "=Rig0", False),
    (MAC + "=0rig", False),
    (MAC + "=rig-0", False),
    (MAC + "=eth5", False),              # the kernel's own namespace (a rename race)
    (MAC + "=enp1", False),
    (MAC + "=wlan0", False),
    (MAC + "=lo", False),
    (MAC + "=rig٠", False),         # an Arabic-Indic digit: the sets are spelled out
])
def test_the_loader_validates_the_fact(value, ok):
    r = _bash('_strih_box_nic_rename_ok box "$1"', value, env={"LC_ALL": "C.UTF-8"})
    assert (r.returncode == 0) == ok, (value, r.stderr)
    if not ok:
        assert "STRIH_NIC_RENAME" in r.stderr


def test_a_bad_fact_refuses_the_box(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    body = (d / "stream-snv.env").read_text().replace(MAC + "=rig0", MAC + "=eth0")
    (d / "stream-snv.env").write_text(body)
    r = _bash('strih_box_load stream-snv', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "STRIH_NIC_RENAME" in r.stderr


# --- the .link file ----------------------------------------------------------------------------------

def test_the_link_matches_mac_and_driver_and_names_the_nic():
    r = _bash('strih_nic_rename_link_text "$1" rig0 r8152', MAC)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines[0].startswith("# camera-box issue 1502")
    i = lines.index("[Match]")
    assert lines[i + 1:i + 3] == ["MACAddress=" + MAC, "Driver=r8152"]
    assert lines[lines.index("[Link]") + 1] == "Name=rig0"


def _apply(tmp_path, value, driver="r8152"):
    link = tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link"
    env = {"STRIH_NIC_RENAME_LINK": str(link), "STRIH_NIC_RENAME_SYSROOT": str(tmp_path / "sys")}
    r = _bash('strih_nic_rename_apply "$1" "$2"', value, driver, env=env, path_dir=tmp_path / "bin")
    return r, link


def test_apply_writes_the_link_once_then_leaves_it(tmp_path):
    log = _udevadm(tmp_path)
    _nic(tmp_path / "sys", OLDNAME, MAC)
    r, link = _apply(tmp_path, MAC + "=rig0")
    assert r.returncode == 0, r.stderr
    assert "written" in r.stdout
    assert link.read_text().splitlines()[-1] == "Name=rig0"
    assert oct(link.stat().st_mode & 0o777) == "0o644"
    assert log.read_text().splitlines() == ["control --reload"]
    ino = link.stat().st_ino
    r2, _ = _apply(tmp_path, MAC + "=rig0")
    assert r2.returncode == 0 and "unchanged" in r2.stdout
    assert link.stat().st_ino == ino
    assert log.read_text().splitlines() == ["control --reload"]


def test_apply_never_renames_and_names_the_usb_rebind(tmp_path):
    _udevadm(tmp_path)
    _nic(tmp_path / "sys", OLDNAME, MAC)
    r, _ = _apply(tmp_path, MAC + "=rig0")
    assert r.returncode == 0, r.stderr
    assert "still %s" % OLDNAME in r.stdout
    assert "echo 2-1 > /sys/bus/usb/drivers/usb/unbind" in r.stdout
    assert (tmp_path / "sys" / "class" / "net" / OLDNAME).exists()


def test_apply_reports_a_renamed_nic(tmp_path):
    _udevadm(tmp_path)
    _nic(tmp_path / "sys", "rig0", MAC)
    r, _ = _apply(tmp_path, MAC + "=rig0")
    assert r.returncode == 0 and "the rig NIC is rig0" in r.stdout


def test_apply_refuses_an_operator_file(tmp_path):
    log = _udevadm(tmp_path)
    link = tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link"
    link.parent.mkdir(parents=True)
    link.write_text("[Match]\nMACAddress=aa:bb:cc:dd:ee:ff\n[Link]\nName=mine\n")
    r, _ = _apply(tmp_path, MAC + "=rig0")
    assert r.returncode == 1 and "not camera-box's" in r.stderr
    assert "Name=mine" in link.read_text()
    assert not log.exists()


def test_none_removes_only_our_file(tmp_path):
    log = _udevadm(tmp_path)
    r, link = _apply(tmp_path, MAC + "=rig0")
    assert link.exists()
    r, _ = _apply(tmp_path, "none")
    assert r.returncode == 0 and "removed" in r.stdout and not link.exists()
    assert log.read_text().splitlines() == ["control --reload", "control --reload"]
    link.write_text("[Link]\nName=mine\n")
    r, _ = _apply(tmp_path, "none")
    assert r.returncode == 0 and "left alone" in r.stdout and link.exists()


def test_a_failed_udev_reload_is_a_warning(tmp_path):
    _stub(tmp_path / "bin", "udevadm", "exit 1\n")
    r, link = _apply(tmp_path, MAC + "=rig0")
    assert r.returncode == 0 and "WARN: udevadm control --reload failed" in r.stdout and link.exists()


# --- verify item 36 --------------------------------------------------------------------------------

def _grade(tmp_path, value, rig, retired=None):
    link = tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link"
    env = {"STRIH_NIC_RENAME_LINK": str(link),
           "STRIH_NIC_RENAME_NM_RETIRED": str(tmp_path / "nm" / "90-retired.conf")}
    if retired is not None:
        (tmp_path / "nm").mkdir(exist_ok=True)
        (tmp_path / "nm" / "90-retired.conf").write_text(retired)
    r = _bash('strih_nic_rename_grade_rows "$1" "$2" "$3" r8152', str(tmp_path / "sys"), rig, value, env=env)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines(), link


def _write_link(link, mac=MAC, name="rig0"):
    r = _bash('strih_nic_rename_link_text "$1" "$2" r8152', mac, name)
    link.parent.mkdir(parents=True, exist_ok=True)
    link.write_text(r.stdout)


def test_grade_passes_a_renamed_rig_nic(tmp_path):
    _nic(tmp_path / "sys", "rig0", MAC)
    link = tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link"
    _write_link(link)
    rows, _ = _grade(tmp_path, MAC + "=rig0", "rig0")
    assert rows and all(r.startswith("OK|(nic-rename)") for r in rows), rows


def test_grade_fails_a_missing_or_stale_link(tmp_path):
    _nic(tmp_path / "sys", "rig0", MAC)
    rows, link = _grade(tmp_path, MAC + "=rig0", "rig0")
    assert any(r.startswith("FAIL|(nic-rename)") and "missing or differs" in r for r in rows), rows
    _write_link(link, name="rig1")
    rows, _ = _grade(tmp_path, MAC + "=rig0", "rig0")
    assert any("missing or differs" in r for r in rows), rows


def test_grade_fails_a_nic_not_yet_renamed_and_names_the_rebind(tmp_path):
    _nic(tmp_path / "sys", OLDNAME, MAC)
    link = tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link"
    _write_link(link)
    rows, _ = _grade(tmp_path, MAC + "=rig0", OLDNAME)
    assert any(r.startswith("FAIL|") and "still %s" % OLDNAME in r and "unbind" in r for r in rows), rows
    assert any(r.startswith("FAIL|") and "resolves to %s" % OLDNAME in r for r in rows), rows


def test_grade_fails_an_unplugged_nic_and_an_unresolved_rig_nic(tmp_path):
    (tmp_path / "sys" / "class" / "net").mkdir(parents=True)
    _write_link(tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link")
    rows, _ = _grade(tmp_path, MAC + "=rig0", "")
    assert any("no interface carries" in r for r in rows), rows
    assert any("resolves to <unresolved>" in r for r in rows), rows


def test_grade_fails_while_the_retired_dropin_unmanages_the_rig_nic(tmp_path):
    _nic(tmp_path / "sys", "rig0", MAC)
    _write_link(tmp_path / "etc" / "systemd" / "network" / "10-camera-box-rig-nic.link")
    conf = ("[device-vban-dante70-retired]\nmatch-device=interface-name:%s;mac:%s\nmanaged=0\n"
            % (OLDNAME, MAC.upper()))
    rows, _ = _grade(tmp_path, MAC + "=rig0", "rig0", retired=conf)
    assert any(r.startswith("FAIL|") and "still unmanages the rig NIC" in r for r in rows), rows
    rows, _ = _grade(tmp_path, MAC + "=rig0", "rig0",
                     retired="[device-x]\nmatch-device=mac:aa:bb:cc:dd:ee:ff\nmanaged=0\n")
    assert not any(r.startswith("FAIL|") for r in rows), rows


def test_grade_none_notes_and_fails_our_leftover(tmp_path):
    rows, link = _grade(tmp_path, "none", "enp7s0")
    assert rows == ["NOTE|(nic-rename) no rig-NIC rename declared (STRIH_NIC_RENAME=none)"]
    _write_link(link)
    rows, _ = _grade(tmp_path, "none", "enp7s0")
    assert rows[0].startswith("FAIL|(nic-rename)") and "left over" in rows[0]


def test_item36_grades_stream_snv_on_the_renamed_adapter(tmp_path):
    """The whole item-36 grader on stream-snv's real facts over a fake sysfs: the onboard r8169 still
    present, the adapter renamed rig0 (driver r8152 = the rig NIC), its 5000 USB link + 2500 Ethernet."""
    sysroot = tmp_path / "sys"
    _nic(sysroot, "rig0", MAC)
    _nic(sysroot, "enp56s0", "e8:9c:25:ce:b6:ea", driver="r8169", usb=False)
    link = tmp_path / "10-camera-box-rig-nic.link"
    _write_link(link)
    env = {"STRIH_NIC_RENAME_LINK": str(link), "STRIH_NIC_RENAME_NM_RETIRED": str(tmp_path / "none.conf"),
           "PATH": "%s:%s" % (tmp_path / "bin", os.environ["PATH"])}
    _stub(tmp_path / "bin", "nmcli", "exit 1\n")
    r = _bash('strih_box_load stream-snv\nstrih_nic_grade_rows "$1" ""', str(sysroot), env=env,
              sources=(FACTS, PROV, NICDRV))
    assert r.returncode == 0, r.stderr
    rows = r.stdout.splitlines()
    assert sum(1 for x in rows if x.startswith("OK|(nic-rename)")) == 2, rows
    assert "OK|(nic-usb) rig0: USB link 5000 Mb/s >= 5000" in rows, rows
    assert "OK|(nic-link) rig0: Ethernet link 2500 Mb/s >= 2500" in rows, rows
    assert not any(x.startswith("FAIL|(nic-rename)") for x in rows), rows


def _rig_nic(sysroot, box="stream-snv", env=None):
    return _bash('strih_box_load "$2"\nstrih_lx_rig_nic "$1" "$3" 10.77.9.204', str(sysroot), box,
                 "2: %s    inet 10.77.9.204/23 brd 10.77.9.255 scope global x" % OLDNAME,
                 env=env, sources=(FACTS, PROV))


def test_the_rig_nic_is_the_declared_name_once_it_exists(tmp_path):
    _nic(tmp_path / "sys", "rig0", MAC)
    _nic(tmp_path / "sys", "enp56s0", "e8:9c:25:ce:b6:ea", driver="r8169", usb=False)
    r = _rig_nic(tmp_path / "sys")
    assert r.returncode == 0 and r.stdout == "rig0", r.stderr


def test_the_rig_nic_fails_closed_before_the_rename(tmp_path):
    """The adapter still under its old name (same driver, even carrying the IP) is never the rig NIC
    of a box that declares a rename: dsync / the leg would bind a link that has no profile."""
    _nic(tmp_path / "sys", OLDNAME, MAC)
    _nic(tmp_path / "sys", "enp56s0", "e8:9c:25:ce:b6:ea", driver="r8169", usb=False)
    r = _rig_nic(tmp_path / "sys")
    assert r.returncode != 0 and r.stdout == "" and "does not exist yet" in r.stderr
    r = _rig_nic(tmp_path / "sys", env={"STRIH_NIC_IFACE": "enp56s0"})
    assert r.returncode == 0 and r.stdout == "enp56s0"


def test_a_box_without_a_rename_resolves_by_driver_as_before(tmp_path):
    _nic(tmp_path / "sys", "enp7s0", "00:24:27:15:e4:da", driver="atlantic", usb=False)
    r = _rig_nic(tmp_path / "sys", box="strih-snv")
    assert r.returncode == 0 and r.stdout == "enp7s0", r.stderr


# --- setup + verify wiring ----------------------------------------------------------------------------

def test_setup_runs_step_1c_after_1b_and_before_step_2():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    b, c = s.index('step "1b"'), s.index('step "1c"')
    call = s.index('strih_nic_rename_apply "$(strih_lx_nic_rename)" "$(strih_lx_nic_driver)" || fail')
    assert b < c < call < s.index("\nstep 2 ")


def test_the_nic_driver_lib_sources_the_rename_lib_and_grades_it():
    s = NICDRV.read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-nic-rename.sh"' in s
    body = s[s.index("strih_nic_grade_rows() {"):]
    assert 'strih_nic_rename_grade_rows "$sysroot" "$nic" "" "$nicdrv"' in body[:body.index("\n}\n")]


@pytest.mark.parametrize("rel", ["scripts/setup-strih.sh", "scripts/verify-strih.sh",
                                 "scripts/lib/strih-box-facts.sh"])
def test_the_line_budgets_hold(rel):
    assert len((REPO / rel).read_text().splitlines()) <= 1000


# --- the switch program ------------------------------------------------------------------------------

NMCLI = r'''
state="$FAKE_NM"
printf '%s\n' "$*" >> "$state/nmcli.log"
case "$1 $2" in
  "connection modify")
    prof="$3"; shift 3
    while [ $# -ge 2 ]; do printf '%s\n' "$2" > "$state/$prof.$1"; shift 2; done ;;
  "--wait 30")
    # an up fails after `fail-ups-after` successful ups (the rollback-FAILED case)
    n=$(( $(cat "$state/ups" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$state/ups"
    if [ -e "$state/fail-ups-after" ] && [ "$n" -gt "$(cat "$state/fail-ups-after")" ]; then exit 4; fi ;;
esac
exit 0
'''
# iputils' arping: `-V` names iputils; a `-U` announce is logged; a probe answers per $FAKE_HEALTHY.
ARPING = r'''
case "$1" in -V) echo "arping from iputils 20240117"; exit 0 ;; esac
printf '%s\n' "$*" >> "$FAKE_NM/arping.log"
case " $* " in *" -U "*) exit 0 ;; esac
[ "$FAKE_HEALTHY" = 1 ]
'''
SWITCH_ARGS = ["enp56s0", "rig0", "stream-lan", "vban-dante70", "70", "10.77.8.1", "10.77.7.232",
               "10.77.9.204", "10.77.7.204"]


def _switch(tmp_path, healthy, new_present=True, retired=None, wait="1", arping=ARPING,
            fail_ups_after=None):
    state = tmp_path / "nm"
    state.mkdir()
    if fail_ups_after is not None:
        (state / "fail-ups-after").write_text(str(fail_ups_after))
    sysroot = tmp_path / "sys"
    _nic(sysroot, "enp56s0", "e8:9c:25:ce:b6:ea", driver="r8169", usb=False)
    if new_present:
        _nic(sysroot, "rig0", MAC)
    bin_dir = tmp_path / "bin"
    _stub(bin_dir, "nmcli", NMCLI)
    _stub(bin_dir, "ping", '[ "$FAKE_HEALTHY" = 1 ]\n')
    _stub(bin_dir, "arping", arping)
    _stub(bin_dir, "ip", 'printf "%s\\n" "$*" >> "$FAKE_NM/ip.log"\n')
    conf = tmp_path / "90-retired.conf"
    if retired is not None:
        conf.write_text(retired)
    gen = subprocess.run(["bash", str(RENAME), "--switch-program", *SWITCH_ARGS, wait],
                         capture_output=True, text=True, timeout=30)
    assert gen.returncode == 0, gen.stderr
    prog = tmp_path / "switch.sh"
    prog.write_text(gen.stdout)
    env = dict(os.environ, PATH="%s:%s" % (bin_dir, os.environ["PATH"]), FAKE_NM=str(state),
               FAKE_HEALTHY="1" if healthy else "0",
               RIG_NIC_SWITCH_RESULT=str(tmp_path / "result"), RIG_NIC_SWITCH_RETIRED=str(conf),
               RIG_NIC_SWITCH_SYSROOT=str(sysroot))
    r = subprocess.run(["bash", str(prog)], capture_output=True, text=True, env=env, timeout=60)
    return r, state, conf, (tmp_path / "result")


def _log(state, name):
    f = state / name
    return f.read_text().splitlines() if f.exists() else []


RETIRED = ("# issue 1495\n[device-vban-dante70-retired]\n"
           "match-device=interface-name:%s;mac:%s;interface-name:enxaabbccddeeff;mac:aa:bb:cc:dd:ee:ff\n"
           "managed=0\n" % (OLDNAME, MAC))


def test_switch_moves_both_profiles_and_unretires_the_adapter(tmp_path):
    r, st, conf, res = _switch(tmp_path, healthy=True, retired=RETIRED)
    assert r.returncode == 0, r.stdout + r.stderr
    assert res.read_text().startswith("ok ")
    assert (st / "stream-lan.connection.interface-name").read_text().strip() == "rig0"
    assert (st / "vban-dante70.vlan.parent").read_text().strip() == "rig0"
    assert (st / "vban-dante70.connection.interface-name").read_text().strip() == "rig0.70"
    assert conf.read_text() == ("# issue 1495\n[device-vban-dante70-retired]\n"
                                "match-device=interface-name:enxaabbccddeeff;mac:aa:bb:cc:dd:ee:ff\nmanaged=0\n")
    log = "\n".join(_log(st, "nmcli.log"))
    assert "device set rig0 managed yes" in log
    assert log.index("connection up stream-lan") < log.index("connection up vban-dante70")


def test_a_successful_switch_downs_the_old_nic_and_announces_both_addresses(tmp_path):
    """The onboard NIC stays cabled until the owner unplugs it: left up it would answer ARP for the rig
    address on the same L2 (and NetworkManager could DHCP it), and neighbours would keep the old MAC."""
    r, st, _, _ = _switch(tmp_path, healthy=True)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "device disconnect enp56s0" in _log(st, "nmcli.log")
    assert _log(st, "ip.log") == ["link set dev enp56s0 down"]
    announces = [x for x in _log(st, "arping.log") if " -U " in " %s " % x]
    assert announces == ["-q -U -c 2 -I rig0 10.77.9.204", "-q -U -c 2 -I rig0.70 10.77.7.204"]


def test_switch_rolls_back_when_the_new_nic_does_not_answer(tmp_path):
    r, st, conf, res = _switch(tmp_path, healthy=False, retired=RETIRED)
    assert r.returncode == 1
    assert res.read_text().startswith("rolled-back ")
    assert (st / "stream-lan.connection.interface-name").read_text().strip() == "enp56s0"
    assert (st / "vban-dante70.vlan.parent").read_text().strip() == "enp56s0"
    assert (st / "vban-dante70.connection.interface-name").read_text().strip() == "enp56s0.70"
    assert conf.read_text() == RETIRED
    log = _log(st, "nmcli.log")
    assert log[-1] == "--wait 30 connection up vban-dante70"
    assert "device set rig0 managed no" in log
    assert log.index("device set rig0 managed no") < log.index("connection modify stream-lan connection.interface-name enp56s0")
    assert _log(st, "ip.log") == ["link set dev rig0 down"]
    assert not any(" -U " in " %s " % x for x in _log(st, "arping.log"))


def test_a_rollback_that_cannot_bring_the_old_nic_up_says_so(tmp_path):
    r, st, _, res = _switch(tmp_path, healthy=False, fail_ups_after=2)
    assert r.returncode == 2
    assert res.read_text().startswith("rollback-FAILED ") and "console" in res.read_text()


def test_switch_refuses_before_any_change_without_the_renamed_nic(tmp_path):
    r, st, conf, res = _switch(tmp_path, healthy=True, new_present=False, retired=RETIRED)
    assert r.returncode == 1 and res.read_text().startswith("refused ")
    assert not _log(st, "nmcli.log") and not _log(st, "ip.log")
    assert conf.read_text() == RETIRED


def test_switch_refuses_an_arping_that_is_not_iputils(tmp_path):
    r, st, conf, res = _switch(tmp_path, healthy=True, retired=RETIRED, arping='echo "ARPing 2.21, by Thomas Habets"\n')
    assert r.returncode == 1 and "not iputils" in res.read_text()
    assert not _log(st, "nmcli.log") and conf.read_text() == RETIRED


def test_switch_refuses_a_dropin_with_two_match_lines(tmp_path):
    two = RETIRED + "match-device=mac:11:22:33:44:55:66\n"
    r, st, conf, res = _switch(tmp_path, healthy=True, retired=two)
    assert r.returncode == 1 and "more than one match-device" in res.read_text()
    assert not _log(st, "nmcli.log") and conf.read_text() == two


def test_switch_without_a_retired_dropin_touches_no_file(tmp_path):
    r, st, conf, res = _switch(tmp_path, healthy=True)
    assert r.returncode == 0 and not conf.exists()
    assert "managed yes" not in "\n".join(_log(st, "nmcli.log"))


@pytest.mark.parametrize("bad", [
    {9: "0"},                 # WAIT_S
    {2: "stream lan"},
    {1: "rig0;id"},
    {0: "-x"},
    {4: "x70"},
    {4: "4095"},             # VLAN id over 4094
    {7: "10.77.9.204&"},
])
def test_the_switch_program_refuses_unsafe_arguments(bad):
    args = SWITCH_ARGS + ["60"]
    for i, v in bad.items():
        args[i] = v
    r = subprocess.run(["bash", str(RENAME), "--switch-program", *args], capture_output=True, text=True)
    assert r.returncode != 0 and not r.stdout


def test_the_runbook_runs_the_program_with_the_lib_signature():
    rule = (REPO / ".claude" / "rules" / "strih-linux-provisioning.md").read_text()
    assert ("--switch-program enp56s0 rig0\n   stream-lan vban-dante70 70 10.77.8.1 10.77.7.232 10.77.9.204 10.77.7.204 60"
            in rule)
