"""issue 1425 (strih-pp, 27.11.2026 12:02 CET): a dual-port rig NIC of the fact driver.

strih-pp's rig NIC is one port of an Intel X520-class card: TWO ixgbe interfaces (enp5s0f0, enp5s0f1), the
rig one is enp5s0f1, which carries the box fact STRIH_IP 10.77.8.202. The boot oneshot died on the driver
scan (`FATAL: multiple ixgbe NICs (enp5s0f0 enp5s0f1) -- set STRIH_NIC_IFACE`), so RSS stayed at 16 queues
with the IRQs on 0-15. The fleet's ONE rig-NIC rule (`strih_lx_rig_nic`: override -> the one driver match ->
the interface carrying STRIH_IP) resolves enp5s0f1 there; the oneshot must agree with it.

Fixtures: a fake sysfs with both ports + a fake `ip` printing `ip -o -4 addr show` lines, the fake ethtool of
test_strih_housekeeping_1425.py (imported, so its tests are not collected twice).
"""
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_strih_housekeeping_1425 as hk  # noqa: E402

RIG_IP = "10.77.8.202"
PORT_IRQS = {"enp5s0f0": (100, 101, 102, 103, 104), "enp5s0f1": (200, 201, 202, 203, 204)}

FAKE_IP = r'''#!/bin/sh
# `ip -o -4 addr show` only: print the fixture lines
cat "$FAKE_IP_ADDRS"
'''


def _two_port_box(tmp_path, ip_on=("enp5s0f1",)):
    sysr = tmp_path / "sys"
    drv = sysr / "bus" / "pci" / "drivers" / "ixgbe"
    drv.mkdir(parents=True)
    irq_dir = tmp_path / "irq"
    for fn, (nic, irqs) in enumerate(sorted(PORT_IRQS.items())):
        dev = sysr / "devices" / "pci0000:00" / ("0000:05:00.%d" % fn)
        (dev / "msi_irqs").mkdir(parents=True)
        (dev / "driver").symlink_to(drv)
        net = sysr / "class" / "net" / nic
        net.mkdir(parents=True)
        (net / "device").symlink_to(dev)
        for n in irqs:
            (dev / "msi_irqs" / str(n)).write_text("msix\n")
            (irq_dir / str(n)).mkdir(parents=True)
            (irq_dir / str(n) / "smp_affinity_list").write_text("0-15\n")
    # the onboard NIC of another driver, which never matters here
    other = sysr / "devices" / "pci0000:00" / "0000:02:00.0"
    other.mkdir(parents=True)
    rdrv = sysr / "bus" / "pci" / "drivers" / "r8169"
    rdrv.mkdir(parents=True)
    (other / "driver").symlink_to(rdrv)
    (sysr / "class" / "net" / "enp2s0").mkdir(parents=True)
    (sysr / "class" / "net" / "enp2s0" / "device").symlink_to(other)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    hk._exe(bin_dir / "ethtool", hk.FAKE_ETHTOOL)
    hk._exe(bin_dir / "ip", FAKE_IP)
    (tmp_path / "state").write_text("4\n")   # strih-pp: RSS count 4 already, so no -L
    (tmp_path / "ethtool.log").write_text("")
    lines = ["2: enp2s0    inet 192.168.1.50/24 brd 192.168.1.255 scope global dynamic enp2s0\\       valid_lft 86000sec preferred_lft 86000sec"]
    for i, nic in enumerate(ip_on):
        lines.append("%d: %s    inet %s/24 brd 10.77.8.255 scope global %s\\       valid_lft forever preferred_lft forever"
                     % (4 + i, nic, RIG_IP, nic))
    (tmp_path / "addrs").write_text("\n".join(lines) + "\n")
    env = {"FAKE_ETHTOOL_STATE": str(tmp_path / "state"), "FAKE_ETHTOOL_LOG": str(tmp_path / "ethtool.log"),
           "FAKE_MSI_DIR": str(tmp_path / "unused-msi"), "FAKE_IRQ_DIR": str(irq_dir),
           "FAKE_IP_ADDRS": str(tmp_path / "addrs")}
    return sysr, irq_dir, bin_dir, env


def _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env):
    script = tmp_path / "affinity.sh"
    r = hk._run('strih_box_load strih-pp; strih_nic_irq_affinity_script_text > "$1"', str(script), real_boxes=True)
    assert r.returncode == 0, r.stderr
    full = {k: v for k, v in os.environ.items() if k not in hk.LEAKY}
    full.update(env)
    full.update({"SYS_ROOT": str(sysr), "IRQ_DIR": str(irq_dir), "PATH": "%s:%s" % (bin_dir, full["PATH"]),
                 "CARRIER_WAIT_S": "0", "IRQ_RATE_SAMPLE_S": "0",
                 "PROC_INTERRUPTS": str(tmp_path / "no-interrupts"),
                 "STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS": ""})
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=full, timeout=60)


def test_two_ports_of_the_driver_pick_the_one_carrying_the_rig_ip(tmp_path):
    sysr, irq_dir, bin_dir, env = _two_port_box(tmp_path)
    r = _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "FATAL" not in r.stderr
    assert "iface enp5s0f1" in r.stdout, r.stdout
    # the rig port's IRQs are placed on the IRQ-only CPUs 1,8-9 (round-robin: no traffic measured)
    placed = hk._aff(irq_dir, PORT_IRQS["enp5s0f1"])
    assert placed == ["1", "8", "9", "1", "8"], placed
    # the other port is never touched
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f0"]) == ["0-15"] * 5
    # only the rig port is ever asked: -l (and the read-only -S of the per-queue weight, design 7228337095)
    assert {c.split()[-1] for c in (tmp_path / "ethtool.log").read_text().splitlines()} == {"enp5s0f1"}
    assert (tmp_path / "ethtool.log").read_text().splitlines()[0] == "-l enp5s0f1"


def test_two_ports_and_no_interface_carrying_the_ip_fail_loud_naming_both(tmp_path):
    sysr, irq_dir, bin_dir, env = _two_port_box(tmp_path, ip_on=())
    r = _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL" in r.stderr
    assert "enp5s0f0 enp5s0f1" in r.stderr and RIG_IP in r.stderr, r.stderr
    assert "STRIH_NIC_IFACE" in r.stderr
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f1"]) == ["0-15"] * 5


def test_the_ip_on_two_interfaces_fails_loud(tmp_path):
    sysr, irq_dir, bin_dir, env = _two_port_box(tmp_path, ip_on=("enp5s0f0", "enp5s0f1"))
    r = _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL" in r.stderr and "enp5s0f0 enp5s0f1" in r.stderr, r.stderr
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f0"]) == ["0-15"] * 5
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f1"]) == ["0-15"] * 5


def test_the_override_still_wins_over_two_ports(tmp_path):
    sysr, irq_dir, bin_dir, env = _two_port_box(tmp_path)
    env["STRIH_NIC_IFACE"] = "enp5s0f0"
    r = _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "iface enp5s0f0" in r.stdout
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f0"]) == ["1", "8", "9", "1", "8"]
    assert hk._aff(irq_dir, PORT_IRQS["enp5s0f1"]) == ["0-15"] * 5


def test_the_oneshot_and_the_fleet_rule_resolve_the_same_port(tmp_path):
    # parity: strih_lx_rig_nic (dsync --interface, verify (hk-irq), pacing) over the SAME fixture
    sysr, irq_dir, bin_dir, env = _two_port_box(tmp_path)
    r = hk._run('strih_box_load strih-pp; strih_lx_rig_nic "$1" "$(cat "$2")" "$(strih_lx_ip)"',
                str(sysr), env["FAKE_IP_ADDRS"], real_boxes=True)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "enp5s0f1"
    one = _run_oneshot(tmp_path, sysr, irq_dir, bin_dir, env)
    assert one.returncode == 0, one.stderr
    assert "iface %s" % r.stdout in one.stdout
