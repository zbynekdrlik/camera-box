"""issue 1425 (design 7106973291): strih-snv housekeeping off OBS's CPUs.

The box fact STRIH_HOUSEKEEPING_CPUS (strih-snv 0-1, the complement of the OBS mask 2-15; the other
boxes `none`), the step-11b IRQ oneshot generalised to every IRQ of the rig NIC + the RSS channel
count, the netdev_budget_usecs drop-in, the rustdesk / display-watch drop-ins, the OBS launcher's own
placement (its shutdown watch = the stat / grep / tail loop) and verify item 16's rows.

Every lib runs under the callers' `set -euo pipefail`; host tools (ethtool, systemctl, taskset, renice)
are fakes on PATH, /sys + /proc + /etc are temp trees through the libs' seams. Values reach bash as
ARGUMENTS, never inside the script text.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
HK = SCRIPTS / "lib" / "strih-housekeeping.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
LAUNCHER = SCRIPTS / "strih-obs-start.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "STRIH_HOUSEKEEPING_CPUS",
         "STRIH_HOUSEKEEPING_IRQ_CPUS", "OBS_BOX_ONLINE_CPUS", "STRIH_PROGRAM_AUDIO_SYSFS",
         "STRIH_PROGRAM_AUDIO_ISOLATED_CONF",
         "STRIH_HOUSEKEEPING_ROOT", "STRIH_HOUSEKEEPING_SYS", "STRIH_HOUSEKEEPING_PROC", "ETHTOOL")
ME = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
MYGRP = subprocess.run(["id", "-gn"], capture_output=True, text=True, check=True).stdout.strip()


def _run(body, *args, env=None, path_prefix=None, sources=(FACTS, PROVISION)):
    harness = ("set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources) +
               'fail() { echo "FAIL: $*" >&2; return 1; }\n'
               'ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n' + body)
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        full["PATH"] = "%s:%s" % (path_prefix, full["PATH"])
    if env:
        full.update(env)
    return subprocess.run(["bash", "-c", harness, "harness", *args], capture_output=True, text=True,
                          env=full, cwd=str(REPO), timeout=60)


def _exe(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


# --- the fact ------------------------------------------------------------------------------------------

def test_every_box_declares_the_fact_strih_snv_0_1_the_others_none():
    want = {"strih-snv": "0-1", "strih-pp": "none", "stream-pp": "none", "stream-snv": "none"}
    for box, v in want.items():
        r = _run('strih_box_load "$1"; strih_lx_housekeeping_cpus', box)
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == v, box


def _box_with(tmp_path, value):
    d = tmp_path / "boxes"
    d.mkdir(exist_ok=True)
    body = (BOXES / "strih-snv.env").read_text()
    assert body.count("STRIH_HOUSEKEEPING_CPUS=0-1\n") == 1
    (d / "strih-snv.env").write_text(body.replace("STRIH_HOUSEKEEPING_CPUS=0-1\n", "STRIH_HOUSEKEEPING_CPUS=%s\n" % value))
    return {"STRIH_BOXES_DIR": str(d)}


def test_the_loader_takes_a_cpulist_and_refuses_anything_else(tmp_path):
    for good in ("0-1", "0,1", "0-1,8", "3", "none"):
        r = _run('strih_box_load strih-snv && strih_lx_housekeeping_cpus', env=_box_with(tmp_path, good))
        assert r.returncode == 0 and r.stdout.strip() == good, (good, r.stderr)
    for badv in ("1-0", "0-", "0,,1", "a", "0-1-2", "٠-١", "0 1", "-1", "00000-1"):
        r = _run('strih_box_load strih-snv', env=_box_with(tmp_path, badv))
        assert r.returncode != 0, badv
        assert "STRIH_HOUSEKEEPING_CPUS" in r.stderr, (badv, r.stderr)


def test_step_11b_runs_on_a_housekeeping_box_and_skips_a_pcie_box_without_one(tmp_path):
    body = 'strih_box_load "$1"; if WHY="$(obs_box_step_applies 11b)"; then echo RUN; else echo "SKIP $WHY"; fi'
    srcs = (FACTS, ROLE)
    assert _run(body, "strih-snv", sources=srcs).stdout.strip() == "RUN"
    out = _run(body, "strih-pp", sources=srcs).stdout.strip()
    assert out.startswith("SKIP the rig NIC is not a USB device") and "STRIH_HOUSEKEEPING_CPUS=none" in out
    assert _run(body, "stream-snv", sources=srcs).stdout.strip() == "RUN"   # a USB rig NIC, no housekeeping


# --- the IRQ oneshot in housekeeping mode ------------------------------------------------------------------

FAKE_ETHTOOL = r'''#!/usr/bin/env python3
import os, sys
state = os.environ["FAKE_ETHTOOL_STATE"]
log = os.environ["FAKE_ETHTOOL_LOG"]
with open(log, "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
cur = int(open(state).read().strip())
if sys.argv[1] == "-l":
    print("Channel parameters for %s:\nPre-set maximums:\nRX:\t\tn/a\nTX:\t\tn/a\nOther:\t\t1\nCombined:\t8\n"
          "Current hardware settings:\nRX:\t\tn/a\nTX:\t\tn/a\nOther:\t\t1\nCombined:\t%d" % (sys.argv[2], cur))
elif sys.argv[1] == "-g":
    ring = os.environ.get("FAKE_ETHTOOL_RING")
    if not ring or not os.path.exists(ring):
        print("netlink error: Operation not supported", file=sys.stderr)
        sys.exit(1)
    rx, mx = open(ring).read().split()
    print("Ring parameters for %s:\nPre-set maximums:\nRX:\t\t\t%s\nRX Mini:\t\tn/a\nTX:\t\t\t8184\n"
          "Current hardware settings:\nRX:\t\t\t%s\nRX Mini:\t\tn/a\nTX:\t\t\t2048" % (sys.argv[2], mx, rx))
elif sys.argv[1] == "-G":
    if os.environ.get("FAKE_ETHTOOL_RING_FAIL"):
        sys.exit(1)
    ring = os.environ["FAKE_ETHTOOL_RING"]
    mx = open(ring).read().split()[1]
    open(ring, "w").write("%s %s\n" % (sys.argv[4], mx))
elif sys.argv[1] == "-S":
    stats = os.environ.get("FAKE_ETHTOOL_STATS")
    if not stats:
        sys.exit(1)
    print("NIC statistics:")
    print(open(stats).read(), end="")
elif sys.argv[1] == "-L":
    if os.environ.get("FAKE_ETHTOOL_FAIL"):
        sys.exit(1)
    n = int(sys.argv[4])
    open(state, "w").write("%d\n" % n)
    # the driver re-requests its vectors: one per queue + the link vector, renumbered from 150
    msi = os.environ["FAKE_MSI_DIR"]
    for e in os.listdir(msi):
        os.remove(os.path.join(msi, e))
    irq_dir = os.environ["FAKE_IRQ_DIR"]
    for i in range(n + 1):
        open(os.path.join(msi, str(150 + i)), "w").write("msix\n")
        os.makedirs(os.path.join(irq_dir, str(150 + i)), exist_ok=True)
        open(os.path.join(irq_dir, str(150 + i), "smp_affinity_list"), "w").write("0-15\n")
'''


def _pcie_box(tmp_path, combined=8, irqs=range(134, 143), ring=8184):
    sysr = tmp_path / "sys"
    dev = sysr / "devices" / "pci0000:00" / "0000:07:00.0"
    (dev / "msi_irqs").mkdir(parents=True)
    drv = sysr / "bus" / "pci" / "drivers" / "atlantic"
    drv.mkdir(parents=True)
    (dev / "driver").symlink_to(drv)
    net = sysr / "class" / "net" / "enp7s0"
    net.mkdir(parents=True)
    (net / "device").symlink_to(dev)
    irq_dir = tmp_path / "irq"
    for n in irqs:
        (dev / "msi_irqs" / str(n)).write_text("msix\n")
        (irq_dir / str(n)).mkdir(parents=True)
        (irq_dir / str(n) / "smp_affinity_list").write_text("0-15\n")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _exe(bin_dir / "ethtool", FAKE_ETHTOOL)
    (tmp_path / "state").write_text("%d\n" % combined)
    (tmp_path / "ring").write_text("%d 8184\n" % ring)
    env = {"FAKE_ETHTOOL_STATE": str(tmp_path / "state"), "FAKE_ETHTOOL_LOG": str(tmp_path / "ethtool.log"),
           "FAKE_ETHTOOL_RING": str(tmp_path / "ring"),
           "FAKE_MSI_DIR": str(dev / "msi_irqs"), "FAKE_IRQ_DIR": str(irq_dir)}
    (tmp_path / "ethtool.log").write_text("")
    return sysr, irq_dir, bin_dir, env


def _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env):
    script = tmp_path / "affinity.sh"
    r = _run('strih_box_load strih-snv; strih_nic_irq_affinity_script_text > "$1"', str(script))
    assert r.returncode == 0, r.stderr
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    full.update(env)
    full.update({"SYS_ROOT": str(sysr), "IRQ_DIR": str(irq_dir), "PATH": "%s:%s" % (bin_dir, full["PATH"]),
                 "CARRIER_WAIT_S": env.get("CARRIER_WAIT_S", "0"),
                 "IRQ_RATE_SAMPLE_S": env.get("IRQ_RATE_SAMPLE_S", "0"),
                 "PROC_INTERRUPTS": env.get("PROC_INTERRUPTS", str(tmp_path / "no-interrupts"))})
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=full, timeout=60)


def _aff(irq_dir, irqs):
    return [(irq_dir / str(n) / "smp_affinity_list").read_text().strip() for n in irqs]


def test_the_oneshot_cuts_rss_to_the_irq_cpu_count_and_spreads_the_rig_nic_irqs_over_both_cpus(tmp_path):
    # design 7119179275: EVERY housekeeping CPU (strih-snv 0-1) carries rig NIC IRQs, one CPU per IRQ; with
    # no measured traffic (no /proc/interrupts here) they go round-robin in IRQ order
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path)
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    calls = (tmp_path / "ethtool.log").read_text().splitlines()
    assert "-L enp7s0 combined 2" in calls
    assert "RSS queues 8 -> 2" in r.stdout
    assert "carrier_changes ? -> ?" in r.stdout   # no carrier_changes file in the fixture: named, never fatal
    # the IRQs are read AFTER the channel change: the re-requested vectors 150-152
    assert _aff(irq_dir, (150, 151, 152)) == ["0", "1", "0"]
    assert "no IRQ traffic measured (rates all 0) -- round-robin in IRQ order over cpus 0 1" in r.stdout
    assert "IRQ 151 (msi enp7s0, iface enp7s0): 0/s -> cpu 1 (was 0-15)" in r.stdout
    assert "done: rig NIC IRQ(s) [150 151 152] placed: 150->0 151->1 152->0" in r.stdout


def test_a_second_run_changes_no_channel_and_still_places(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135, 136))
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr
    calls = (tmp_path / "ethtool.log").read_text().splitlines()
    # never a -L / -G (a ring re-init) when the count and the ring are right; the ring is only read
    assert calls == ["-l enp7s0", "-g enp7s0"], calls
    assert "RSS queues already 2 (combined) -- unchanged" in r.stdout
    assert _aff(irq_dir, (134, 135, 136)) == ["0", "1", "0"]


def test_a_failed_channel_change_fails_the_unit_and_pins_nothing(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path)
    env["FAKE_ETHTOOL_FAIL"] = "1"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL: ethtool -L enp7s0 combined 2 failed (was 8)" in r.stderr
    assert (irq_dir / "134" / "smp_affinity_list").read_text().strip() == "0-15"


def test_a_driver_without_channel_control_keeps_its_queues_and_still_pins(tmp_path):
    # strih-snv 23.11.2026: atlantic answers `ethtool -l` with "netlink error: Operation not supported"
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, irqs=(134, 135, 136))
    _exe(bin_dir / "ethtool", "#!/bin/sh\necho \"$*\" >> \"$FAKE_ETHTOOL_LOG\"\n"
         "echo 'netlink error: Operation not supported' >&2\nexit 1\n")
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "enp7s0: RSS channel count not readable (the driver offers no channel control) -- left alone" in r.stdout
    assert "-L" not in (tmp_path / "ethtool.log").read_text()
    assert _aff(irq_dir, (134, 135, 136)) == ["0", "1", "0"]


def test_an_msi_vector_with_no_proc_irq_entry_is_skipped(tmp_path):
    # strih-snv 23.11.2026: msi_irqs lists 143, /proc/irq/143 does not exist (a vector the driver never requested)
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135))
    (sysr / "devices" / "pci0000:00" / "0000:07:00.0" / "msi_irqs" / "143").write_text("msix\n")
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "IRQ 143 of enp7s0 has no /proc/irq entry (not requested) -- skipped" in r.stdout
    assert "done: rig NIC IRQ(s) [134 135] placed: 134->0 135->1" in r.stdout


def test_an_irq_the_kernel_refuses_is_named_and_fails_after_trying_every_one(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135, 136))
    (irq_dir / "135" / "smp_affinity_list").chmod(0o444)
    try:
        r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    finally:
        (irq_dir / "135" / "smp_affinity_list").chmod(0o644)
    assert r.returncode == 1
    assert "IRQ(s) [135] of enp7s0 could not be placed on the housekeeping CPUs 0-1 (above)" in r.stderr
    assert _aff(irq_dir, (134, 136)) == ["0", "0"]


def test_an_irq_already_on_its_cpu_is_never_written(tmp_path):
    # design 7119179275: every move costs a short loss burst, so an IRQ whose affinity already is its target
    # is left alone (the read-only file would fail a write)
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135))
    (irq_dir / "134" / "smp_affinity_list").write_text("0\n")
    (irq_dir / "134" / "smp_affinity_list").chmod(0o444)
    try:
        r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    finally:
        (irq_dir / "134" / "smp_affinity_list").chmod(0o644)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "IRQ 134 (msi enp7s0, iface enp7s0): 0/s -> cpu 0 (already there, unchanged)" in r.stdout
    assert _aff(irq_dir, (134, 135)) == ["0", "1"]


# the 24.11.2026 strih-snv read (design 7119179275): per-IRQ rates with all 9 IRQs on CPU 1
LIVE_RATES = {136: 17000, 140: 9500, 137: 7200, 138: 4000, 134: 3900, 135: 300, 139: 200, 141: 100, 142: 50}


def _interrupts(rates, scale):
    lines = ["            CPU0       CPU1"]
    for n in sorted(rates):
        lines.append("%4d:  %10d  %10d  IR-PCI-MSI-0000:07:00.0  %d-edge  enp7s0-pci-%d" %
                     (n, 11, rates[n] * scale, n, n))
    lines.append(" NMI:          0          0   Non-maskable interrupts")
    return "\n".join(lines) + "\n"


SWAP_SLEEP = r'''#!/usr/bin/env python3
import os, shutil, sys
with open(os.environ["FAKE_SLEEP_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
for pair in os.environ.get("FAKE_SLEEP_SWAP", "").split():
    src, dst = pair.split(":")
    shutil.copyfile(src, dst)
'''


def _sampled(tmp_path, rates, sample_s="2", irqs=None):
    irqs = sorted(rates) if irqs is None else irqs
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=irqs)
    (tmp_path / "interrupts").write_text(_interrupts(rates, 1))
    (tmp_path / "interrupts.after").write_text(_interrupts(rates, 1 + int(sample_s)))
    _exe(bin_dir / "sleep", SWAP_SLEEP)
    (tmp_path / "sleep.log").write_text("")
    env.update({"PROC_INTERRUPTS": str(tmp_path / "interrupts"), "IRQ_RATE_SAMPLE_S": sample_s,
                "FAKE_SLEEP_LOG": str(tmp_path / "sleep.log"),
                "FAKE_SLEEP_SWAP": "%s:%s" % (tmp_path / "interrupts.after", tmp_path / "interrupts")})
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    return r, irq_dir


def test_the_irqs_are_balanced_greedily_by_the_measured_rate(tmp_path):
    # heaviest first to the least-loaded CPU, ties to the lower CPU (142 meets 21100 on both)
    r, irq_dir = _sampled(tmp_path, LIVE_RATES)
    assert r.returncode == 0, r.stderr + r.stdout
    assert (tmp_path / "sleep.log").read_text().splitlines() == ["2"]
    want = {136: "0", 140: "1", 137: "1", 138: "1", 134: "0", 135: "1", 139: "0", 141: "1", 142: "0"}
    assert dict(zip(sorted(want), _aff(irq_dir, sorted(want)))) == want
    assert "IRQ 136 (msi enp7s0, iface enp7s0): 17000/s -> cpu 0 (was 0-15)" in r.stdout
    assert "IRQ 140 (msi enp7s0, iface enp7s0): 9500/s -> cpu 1 (was 0-15)" in r.stdout
    assert "rig NIC IRQ rates measured over 2 s, assigned load: cpu 0 21150/s, cpu 1 21100/s" in r.stdout


def test_equal_rates_go_to_the_lower_cpu_first_in_irq_order(tmp_path):
    r, irq_dir = _sampled(tmp_path, {134: 100, 135: 100, 136: 100, 137: 100})
    assert r.returncode == 0, r.stderr + r.stdout
    assert _aff(irq_dir, (134, 135, 136, 137)) == ["0", "1", "0", "1"]


def test_an_irq_missing_from_proc_interrupts_counts_as_no_traffic(tmp_path):
    r, irq_dir = _sampled(tmp_path, {134: 500}, irqs=(134, 135))
    assert r.returncode == 0, r.stderr + r.stdout
    assert "IRQ 135: no /proc/interrupts row -- rate 0" in r.stdout
    assert _aff(irq_dir, (134, 135)) == ["0", "1"]


def test_a_garbage_sample_time_is_refused(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134,))
    env["IRQ_RATE_SAMPLE_S"] = "2s"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL: IRQ_RATE_SAMPLE_S '2s' is not a whole number of seconds" in r.stderr


# --- setup: the housekeeping install ---------------------------------------------------------------------

FAKE_SYSTEMCTL = r'''#!/usr/bin/env python3
import os, sys
args = [a for a in sys.argv[1:] if a != "--user"]
with open(os.environ["FAKE_SYSTEMCTL_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
if args[:1] and args[0] in os.environ.get("FAKE_SYSTEMCTL_FAIL", "").split():
    sys.exit(1)
if args[:1] == ["is-active"]:
    unit = args[-1]
    sys.exit(0 if unit in os.environ.get("FAKE_ACTIVE", "").split() else 3)
# enable / disable / is-enabled keep their state in a file, as systemd keeps its wants symlinks
enabled_file = os.environ.get("FAKE_SYSTEMCTL_ENABLED", "")
enabled = set(open(enabled_file).read().split()) if enabled_file and os.path.exists(enabled_file) else set()
if args[:1] in (["enable"], ["disable"]):
    unit = args[-1]
    enabled = enabled | {unit} if args[0] == "enable" else enabled - {unit}
    open(enabled_file, "w").write("\n".join(sorted(enabled)) + "\n")
    sys.exit(0)
if args[:1] == ["is-enabled"]:
    sys.exit(0 if args[-1] in enabled else 1)
# the re-balance pair answers only for its own unit names (a wrong name reads nothing, like an unknown unit)
if args[:1] == ["show"] and "ExecMainStartTimestampMonotonic" in args:
    if args[-1] == "strih-nic-irq-rebalance.service":
        print(os.environ.get("FAKE_REBALANCE_SHOW", "ExecMainStartTimestampMonotonic=0\nResult=success\nExecMainStatus=0"))
    sys.exit(0)
if args[:1] == ["show"] and "SubState" in args:
    if args[-1] == "strih-nic-irq-rebalance.timer":
        print(os.environ.get("FAKE_REBALANCE_TIMER_SHOW", "ActiveState=active\nSubState=waiting"))
    sys.exit(0)
if args[:3] == ["show", "-p", "AllowedCPUs"]:
    print(os.environ.get("FAKE_ALLOWED_CPUS", "0"))
elif args[:1] == ["show"]:
    unit = args[-1]
    print(os.environ.get("FAKE_PID_" + unit.replace("-", "_").replace(".", "_"), "0"))
sys.exit(0)
'''


def _install_env(tmp_path):
    root = tmp_path / "root"
    (root / "proc" / "sys" / "net" / "core").mkdir(parents=True)
    (root / "proc" / "sys" / "net" / "core" / "netdev_budget_usecs").write_text("2000\n")
    home = tmp_path / "home"
    home.mkdir()
    bin_dir = tmp_path / "sbin"
    bin_dir.mkdir()
    _exe(bin_dir / "systemctl", FAKE_SYSTEMCTL)
    _exe(bin_dir / "udevadm", '#!/bin/sh\necho "udevadm $*" >> "$FAKE_SYSTEMCTL_LOG"\n')
    (tmp_path / "systemctl.log").write_text("")
    env = {"STRIH_HOUSEKEEPING_ROOT": str(root), "STRIH_HOUSEKEEPING_OWNER": "%s:%s" % (ME, MYGRP),
           "STRIH_SESSION_APPS_EUID": "1000", "FAKE_SYSTEMCTL_LOG": str(tmp_path / "systemctl.log"),
           "FAKE_SYSTEMCTL_ENABLED": str(tmp_path / "systemctl.enabled"),
           "FAKE_ACTIVE": "rustdesk.service strih-display-watch.service"}
    return root, home, bin_dir, env


def test_setup_writes_the_sysctl_the_two_dropins_the_launcher_file_and_restarts_the_running_units(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr + r.stdout
    # design 7119179275: the launcher's file + the drop-ins carry the TASK CPU (the first, 0)
    assert (root / "etc" / "strih-housekeeping-cpus.conf").read_text() == "0\n"
    # strih-snv 23.11.2026: kernel 7.0 at HZ 1000 refuses netdev_budget_usecs below 2000 (EINVAL), so the
    # step writes no budget drop-in and never touches the running value
    assert not (root / "etc" / "sysctl.d" / "60-strih-housekeeping.conf").exists()
    assert (root / "proc" / "sys" / "net" / "core" / "netdev_budget_usecs").read_text().strip() == "2000"
    for d in (root / "etc" / "systemd" / "system" / "rustdesk.service.d",
              home / ".config" / "systemd" / "user" / "strih-display-watch.service.d"):
        t = (d / "50-camera-box-housekeeping.conf").read_text()
        assert t.splitlines()[1:] == ["[Service]", "CPUAffinity=0", "Nice=10"]
    rule = (root / "etc" / "udev" / "rules.d" / "71-strih-housekeeping-rss.rules").read_text().splitlines()
    assert rule[1] == ('ACTION=="add", SUBSYSTEM=="net", DRIVERS=="atlantic", TEST=="device/msi_irqs", '
                       'RUN+="/usr/sbin/ethtool -L $name combined 2"')
    hook = root / "etc" / "NetworkManager" / "dispatcher.d" / "60-strih-housekeeping-irq"
    assert hook.read_text().startswith("#!/bin/sh\n# camera-box issue 1425") and os.access(hook, os.X_OK)
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    # design 7119686502: the delayed re-balance units, reloaded and the timer enabled, never started
    assert calls == ["udevadm control --reload-rules",
                     "daemon-reload", "is-enabled -q strih-nic-irq-rebalance.timer",
                     "enable strih-nic-irq-rebalance.timer",
                     "daemon-reload", "is-active -q rustdesk.service", "restart rustdesk.service",
                     "--user daemon-reload", "--user is-active -q strih-display-watch.service",
                     "--user restart strih-display-watch.service"], calls


def test_a_second_setup_run_touches_no_unit(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    body = 'strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"'
    assert _run(body, ME, str(home), env=env, path_prefix=bin_dir).returncode == 0
    (tmp_path / "systemctl.log").write_text("")
    r = _run(body, ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    # only the read-only enable-state query of the re-balance timer (already enabled: nothing done)
    assert (tmp_path / "systemctl.log").read_text() == "is-enabled -q strih-nic-irq-rebalance.timer\n"
    assert "written" not in r.stdout


def test_a_unit_not_running_or_not_installed_yet_is_never_started(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    env["FAKE_ACTIVE"] = ""
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    assert not [c for c in calls if "restart" in c or c.startswith("start")], calls
    assert "rustdesk.service is not running" in r.stdout


def test_a_box_with_none_removes_ours_and_keeps_a_foreign_file(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    assert _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env,
                path_prefix=bin_dir).returncode == 0
    foreign = root / "etc" / "sysctl.d" / "60-strih-housekeeping.conf"
    foreign.parent.mkdir(parents=True, exist_ok=True)   # setup no longer creates sysctl.d (no budget drop-in)
    foreign.write_text("net.core.netdev_budget_usecs = 800\n")
    env.update(_box_with(tmp_path, "none"))
    (tmp_path / "systemctl.log").write_text("")
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    assert not (root / "etc" / "strih-housekeeping-cpus.conf").exists()
    assert not (root / "etc" / "systemd" / "system" / "rustdesk.service.d" / "50-camera-box-housekeeping.conf").exists()
    assert not (home / ".config" / "systemd" / "user" / "strih-display-watch.service.d" / "50-camera-box-housekeeping.conf").exists()
    assert foreign.exists() and "not ours" in r.stderr
    assert not (root / "etc" / "udev" / "rules.d" / "71-strih-housekeeping-rss.rules").exists()
    assert not (root / "etc" / "NetworkManager" / "dispatcher.d" / "60-strih-housekeeping-irq").exists()
    assert not (root / "etc" / "systemd" / "system" / "strih-nic-irq-rebalance.timer").exists()
    assert not (root / "etc" / "systemd" / "system" / "strih-nic-irq-rebalance.service").exists()
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    assert calls == ["disable --now strih-nic-irq-rebalance.timer", "daemon-reload",
                     "reset-failed strih-nic-irq-rebalance.service",
                     "daemon-reload", "--user daemon-reload"], calls


def test_a_none_box_with_nothing_of_ours_is_silent(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    r = _run('strih_box_load strih-pp; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0 and r.stdout == "" and r.stderr == "", (r.stdout, r.stderr)
    assert (tmp_path / "systemctl.log").read_text() == ""


# --- verify: item 16's housekeeping rows -----------------------------------------------------------------

def _grade_env(tmp_path, irq_aff=("1", "0", "1"), combined=2, nice=10, cpus="0", running=True,
               sampler_cpus="2-15", sampler_nice=10, ring=8184):
    tmp_path.mkdir(parents=True, exist_ok=True)
    sysr, irq_dir, bin_dir, eenv = _pcie_box(tmp_path, combined=combined, irqs=(134, 135, 136), ring=ring)
    if isinstance(irq_aff, str):
        irq_aff = (irq_aff,) * 3
    for n, a in zip((134, 135, 136), irq_aff):
        (irq_dir / str(n) / "smp_affinity_list").write_text(a + "\n")
    root, home, sbin, env = _install_env(tmp_path)
    env.update(eenv)
    assert _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env,
                path_prefix=sbin).returncode == 0
    (root / "etc" / "strih-isolated-cpus.conf").write_text("2-15\n")
    (sysr / "devices" / "system" / "cpu").mkdir(parents=True)
    (sysr / "devices" / "system" / "cpu" / "online").write_text("0-15\n")
    proc = tmp_path / "proc"
    proc.mkdir()
    (proc / "irq").symlink_to(irq_dir)
    for i, unit in enumerate(("rustdesk.service", "strih-display-watch.service", "strih-obs.service",
                              "program-audio-sampler.service")):
        pid = str(4000 + i)
        sampler = unit.startswith("program-audio")
        env["FAKE_PID_" + unit.replace("-", "_").replace(".", "_")] = pid if running else "0"
        (proc / pid).mkdir()
        (proc / pid / "status").write_text("Name:\tx\nCpus_allowed_list:\t%s\n" % (sampler_cpus if sampler else cpus))
        fields = ["S"] + ["0"] * 15 + [str(sampler_nice if sampler else nice)] + ["1"] * 30
        (proc / pid / "stat").write_text("%s (a b) %s\n" % (pid, " ".join(fields)))
    (proc / "net").mkdir()
    (proc / "net" / "softnet_stat").write_text(SOFTNET_BEFORE)
    stats = sysr / "class" / "net" / "enp7s0" / "statistics"
    stats.mkdir()
    (stats / "rx_missed_errors").write_text("7\n")
    (stats / "rx_dropped").write_text("0\n")
    env.update({"STRIH_HOUSEKEEPING_SYS": str(sysr), "STRIH_HOUSEKEEPING_PROC": str(proc), "STRIH_NIC_IFACE": "enp7s0",
                "STRIH_HOUSEKEEPING_IPADDR": "", "STRIH_HOUSEKEEPING_SAMPLE_S": "0",
                "FAKE_REBALANCE_SHOW": REBALANCE_RAN_OK})
    return home, "%s:%s" % (bin_dir, sbin), env


# `systemctl show -p ExecMainStartTimestampMonotonic -p Result -p ExecMainStatus` of the re-balance service
REBALANCE_RAN_OK = "Result=success\nExecMainStartTimestampMonotonic=301204512\nExecMainStatus=0"


def _grade(tmp_path, **kw):
    home, path, env = _grade_env(tmp_path, **kw)
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert r.returncode == 0, r.stderr
    return r.stdout


# /proc/net/softnet_stat: one row per CPU, the 3rd column time_squeeze (hex), the 13th the CPU id
SOFTNET_BEFORE = ("0000aa00 00000000 00000010 " + "00000000 " * 9 + "00000000\n"
                  "0000bb00 00000000 000003e8 " + "00000000 " * 9 + "00000001\n"
                  "0000cc00 00000000 00000005 " + "00000000 " * 9 + "00000002\n")
SOFTNET_AFTER = ("0000aa10 00000000 00000012 " + "00000000 " * 9 + "00000000\n"
                 "0000bb20 00000000 000004b0 " + "00000000 " * 9 + "00000001\n"
                 "0000cc30 00000000 00000005 " + "00000000 " * 9 + "00000002\n")


def test_a_placed_box_passes_every_row(tmp_path):
    out = _grade(tmp_path)
    rows = out.splitlines()
    assert all(r.startswith("PASS") or r.startswith("NOTE (hk-rx)") or r.startswith("NOTE (hk-softnet)")
               for r in rows), out
    assert "NOTE (hk-rx) enp7s0: rx_missed_errors +0, rx_dropped +0 over 0 s, total missed 7 (report-only)" in out
    assert "PASS (hk-cpus) housekeeping CPUs 0-1 = online 0-15 minus the OBS mask 2-15: rig NIC IRQs on 0-1, tasks on 0" in out
    assert "PASS (hk-irq) enp7s0: every IRQ on one housekeeping CPU of 0-1: 134->1 135->0 136->1" in out
    assert "PASS (hk-slice) rig-background.slice: AllowedCPUs 0, off the IRQ-only CPUs 1" in out
    assert "PASS (hk-unit) sampler: pid 4003 runs on CPUs 2-15 at nice 10" in out
    for tag in ("(hk-cpus)", "(hk-rss)", "(hk-irq)", "(hk-hooks)", "(hk-sysctl)", "(hk-slice)", "(hk-unit) rustdesk.service",
                "(hk-unit) strih-display-watch.service", "(hk-unit) launcher", "(hk-softnet)", "(hk-unit) sampler",
                "(hk-rebalance)", "(hk-ring)"):
        assert any(tag in r for r in rows), (tag, out)


def test_the_softnet_note_reads_the_time_squeeze_and_the_dma_drops_over_the_sample(tmp_path):
    home, path, env = _grade_env(tmp_path)
    proc = pathlib.Path(env["STRIH_HOUSEKEEPING_PROC"])
    (tmp_path / "softnet.after").write_text(SOFTNET_AFTER)
    (tmp_path / "stats").write_text("     InPackets: 9\n     InDroppedDma: 3146\n     Queue[0] InPackets: 1\n")
    (tmp_path / "stats.after").write_text("     InPackets: 9\n     InDroppedDma: 3150\n     Queue[0] InPackets: 1\n")
    sleep_dir = tmp_path / "sleepbin"
    sleep_dir.mkdir()
    _exe(sleep_dir / "sleep", SWAP_SLEEP)
    (tmp_path / "sleep.log").write_text("")
    env.update({"STRIH_HOUSEKEEPING_SAMPLE_S": "2", "FAKE_ETHTOOL_STATS": str(tmp_path / "stats"),
                "FAKE_SLEEP_LOG": str(tmp_path / "sleep.log"),
                "FAKE_SLEEP_SWAP": "%s:%s %s:%s" % (tmp_path / "softnet.after", proc / "net" / "softnet_stat",
                                                    tmp_path / "stats.after", tmp_path / "stats")})
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env,
             path_prefix="%s:%s" % (sleep_dir, path))
    assert r.returncode == 0, r.stderr
    assert (tmp_path / "sleep.log").read_text().splitlines() == ["2"], "one sample serves (hk-rx) and (hk-softnet)"
    assert ("NOTE (hk-softnet) over 2 s: time_squeeze cpu0 +2, cpu1 +200; enp7s0 InDroppedDma +4 (total 3150) "
            "(report-only)") in r.stdout, r.stdout
    assert "FAIL" not in r.stdout, r.stdout


def test_a_wrapped_time_squeeze_counter_reads_wrapped_never_a_negative_count(tmp_path):
    home, path, env = _grade_env(tmp_path)
    proc = pathlib.Path(env["STRIH_HOUSEKEEPING_PROC"])
    (proc / "net" / "softnet_stat").write_text(SOFTNET_AFTER)
    (tmp_path / "softnet.after").write_text(SOFTNET_BEFORE)
    sleep_dir = tmp_path / "sleepbin"
    sleep_dir.mkdir()
    _exe(sleep_dir / "sleep", SWAP_SLEEP)
    (tmp_path / "sleep.log").write_text("")
    env.update({"FAKE_SLEEP_LOG": str(tmp_path / "sleep.log"),
                "FAKE_SLEEP_SWAP": "%s:%s" % (tmp_path / "softnet.after", proc / "net" / "softnet_stat")})
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env,
             path_prefix="%s:%s" % (sleep_dir, path))
    assert "NOTE (hk-softnet) over 0 s: time_squeeze cpu0 wrapped, cpu1 wrapped;" in r.stdout, r.stdout


def test_the_softnet_note_names_a_nic_without_indroppeddma_and_an_unreadable_stat(tmp_path):
    home, path, env = _grade_env(tmp_path)
    (pathlib.Path(env["STRIH_HOUSEKEEPING_PROC"]) / "net" / "softnet_stat").unlink()
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert ("NOTE (hk-softnet) over 0 s: time_squeeze unreadable; enp7s0 InDroppedDma not exposed by ethtool -S "
            "(report-only)") in r.stdout, r.stdout


def test_verify_notes_a_driver_without_channel_control_and_skips_an_unrequested_vector(tmp_path):
    home, path, env = _grade_env(tmp_path)
    eth = pathlib.Path(path.split(":")[0]) / "ethtool"
    _exe(eth, "#!/bin/sh\necho 'netlink error: Operation not supported' >&2\nexit 1\n")
    msi = pathlib.Path(env["STRIH_HOUSEKEEPING_SYS"]) / "devices" / "pci0000:00" / "0000:07:00.0" / "msi_irqs"
    (msi / "143").write_text("msix\n")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "NOTE (hk-rss) enp7s0: RSS channel count not readable (the driver offers no channel control): queues left alone" in r.stdout, r.stdout
    assert "PASS (hk-irq) enp7s0: every IRQ on one housekeeping CPU of 0-1: 134->1 135->0 136->1" in r.stdout, r.stdout
    assert "FAIL" not in r.stdout, r.stdout


def test_an_irq_left_on_obs_cpus_fails(tmp_path):
    out = _grade(tmp_path, irq_aff="0-15")
    assert "FAIL (hk-irq) enp7s0: IRQ(s) not on exactly one housekeeping CPU of 0-1: 134=0-15 135=0-15 136=0-15" in out
    # an IRQ on the whole housekeeping set (two CPUs) fails: the kernel then picks, and drifts
    assert ("FAIL (hk-irq) enp7s0: IRQ(s) not on exactly one housekeeping CPU of 0-1: 135=0-1"
            in _grade(tmp_path / "b", irq_aff=("1", "0-1", "0")))
    assert "FAIL (hk-irq) enp7s0: IRQ(s) not on exactly one housekeeping CPU of 0-1: 136=4" in _grade(
        tmp_path / "c", irq_aff=("1", "0", "4"))


def test_eight_rss_queues_fail(tmp_path):
    assert "FAIL (hk-rss) enp7s0: 8 RSS queues" in _grade(tmp_path, combined=8)


def test_the_sampler_off_the_obs_mask_or_at_nice_0_fails_and_down_is_a_note(tmp_path):
    out = _grade(tmp_path, sampler_cpus="0", sampler_nice=0)
    assert "FAIL (hk-unit) sampler: pid 4003 runs on CPUs 0 at nice 0, want 2-15 at nice 10" in out
    out = _grade(tmp_path / "b", running=False)
    assert "NOTE (hk-unit) sampler: program-audio-sampler.service is not running" in out


def test_a_unit_on_obs_cpus_or_at_nice_0_fails(tmp_path):
    out = _grade(tmp_path, cpus="0-15", nice=0)
    assert "FAIL (hk-unit) rustdesk.service: pid 4000 runs on CPUs 0-15 at nice 0, want 0 at nice 10" in out
    # a unit left on the whole housekeeping set (beside the IRQ CPU) fails
    assert "FAIL (hk-unit) rustdesk.service: pid 4000 runs on CPUs 0-1 at nice 10, want 0" in _grade(tmp_path / "b", cpus="0-1")
    assert "FAIL (hk-unit) launcher:" in out


def test_units_not_running_fail_but_obs_down_is_a_note(tmp_path):
    out = _grade(tmp_path, running=False)
    assert "FAIL (hk-unit) rustdesk.service: no running main process" in out
    assert "NOTE (hk-unit) launcher: strih-obs.service is not running" in out


def test_a_fact_that_is_not_the_complement_of_the_obs_mask_fails(tmp_path):
    home, path, env = _grade_env(tmp_path)
    (pathlib.Path(env["STRIH_HOUSEKEEPING_ROOT"]) / "etc" / "strih-isolated-cpus.conf").write_text("4-15\n")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "FAIL (hk-cpus) STRIH_HOUSEKEEPING_CPUS 0-1 is not the complement of the OBS mask 4-15" in r.stdout


def test_the_budget_row_reports_the_running_value_and_fails_our_leftover_drop_in(tmp_path):
    home, path, env = _grade_env(tmp_path)
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "PASS (hk-sysctl) net.core.netdev_budget_usecs = 2000 (left as the kernel runs it" in r.stdout, r.stdout
    left = pathlib.Path(env["STRIH_HOUSEKEEPING_ROOT"]) / "etc" / "sysctl.d" / "60-strih-housekeeping.conf"
    left.parent.mkdir(parents=True, exist_ok=True)
    left.write_text("# camera-box issue 1425: housekeeping off the OBS CPUs (scripts/lib/strih-housekeeping.sh) -- do not edit\n"
                    "net.core.netdev_budget_usecs = 500\n")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "FAIL (hk-sysctl) " + str(left) + " is a leftover" in r.stdout, r.stdout


def test_setup_removes_our_leftover_budget_drop_in(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    left = root / "etc" / "sysctl.d" / "60-strih-housekeeping.conf"
    left.parent.mkdir(parents=True)
    left.write_text("# camera-box issue 1425: housekeeping off the OBS CPUs (scripts/lib/strih-housekeeping.sh) -- do not edit\n"
                    "net.core.netdev_budget_usecs = 500\n")
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr + r.stdout
    assert not left.exists()


# --- the OBS launcher places its own shutdown watch -----------------------------------------------------

def _place(tmp_path, content, ok=True):
    bin_dir = tmp_path / "b"
    bin_dir.mkdir()
    log = tmp_path / "place.log"
    log.write_text("")
    for tool in ("taskset", "renice"):
        _exe(bin_dir / tool, '#!/bin/sh\necho "%s $*" >> "%s"\nexit %d\n' % (tool, log, 0 if ok else 1))
    f = tmp_path / "hk.conf"
    if content is not None:
        f.write_text(content)
    r = _run('strih_launcher_housekeeping_place "$1" 4242', str(f), sources=(LAUNCHER,), path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    return r.stdout, log.read_text().splitlines()


def test_the_launcher_moves_itself_to_the_housekeeping_cpus_at_nice_10(tmp_path):
    out, calls = _place(tmp_path, "0-1\n")
    assert calls == ["taskset -cp 0-1 4242", "renice --priority 10 -p 4242"]
    assert "runs on the housekeeping CPUs 0-1 at nice 10" in out


def test_the_launcher_does_nothing_without_a_list(tmp_path):
    for content in (None, "none\n", ""):
        sub = tmp_path / ("c%d" % (len(list(tmp_path.iterdir()))))
        sub.mkdir()
        out, calls = _place(sub, content)
        assert calls == [] and out == "", content


def test_a_malformed_list_or_a_failed_taskset_is_one_warn_never_fatal(tmp_path):
    a = tmp_path / "a"
    a.mkdir()
    out, calls = _place(a, "0-1;rm\n")
    assert calls == [] and out.startswith("WARN issue 1425:")
    b = tmp_path / "b2"
    b.mkdir()
    out, calls = _place(b, "0-1\n", ok=False)
    assert out.startswith("WARN issue 1425: could not put the launcher")


def test_the_launcher_places_itself_right_before_the_shutdown_watch():
    s = LAUNCHER.read_text()
    call = s.index('strih_launcher_housekeeping_place "${STRIH_HOUSEKEEPING_CPUS_FILE:-/etc/strih-housekeeping-cpus.conf}" "$$"')
    assert s.index('"$OBS_BIN" "${OBS_ARGS[@]}" &') < call < s.index('strih_obs_shutdown_watch "$OBS_PID"')


# --- wiring ----------------------------------------------------------------------------------------------

def test_setup_and_verify_wire_the_lib_inside_their_budgets():
    s = SETUP.read_text()
    v = VERIFY.read_text()
    assert len(s.splitlines()) <= 1000 and len(v.splitlines()) <= 1000
    inst = s.index('strih_housekeeping_install "$DESKTOP_USER" "$USER_HOME" || fail')
    assert s.index('step "11b"') < inst < s.index('if WHY="$(obs_box_step_applies 11b)"; then')
    gate = v.index('if WHY="$(obs_box_step_applies 11b)"; then')
    grade = v.index('strih_housekeeping_grade_report "$USER_HOME"')
    assert gate < grade < v.index('IRQ_TARGET_IP="${STRIH_LX_TARGET_IP:-$(strih_lx_ip)}"')
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-housekeeping.sh"' in PROVISION.read_text()


def test_a_drifted_udev_rule_or_hook_fails(tmp_path):
    home, path, env = _grade_env(tmp_path)
    (pathlib.Path(env["STRIH_HOUSEKEEPING_ROOT"]) / "etc" / "NetworkManager" / "dispatcher.d" /
     "60-strih-housekeeping-irq").chmod(0o644)
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "FAIL (hk-hooks) " in r.stdout and "not executable" in r.stdout


def _hook(tmp_path):
    r = _run('strih_box_load strih-snv; strih_housekeeping_hook_text "$(strih_lx_nic_driver)"')
    assert r.returncode == 0, r.stderr
    text = r.stdout
    assert text.count("/sys/class/net/") == 1 and text.count("/usr/local/bin/strih-nic-irq-affinity.sh") == 2
    sysr = tmp_path / "sys"
    for nic, drv in (("enp7s0", "atlantic"), ("enp2s0", "r8169")):
        dev = sysr / "devices" / nic
        dev.mkdir(parents=True)
        d = sysr / "bus" / "pci" / "drivers" / drv
        d.mkdir(parents=True)
        (dev / "driver").symlink_to(d)
        (sysr / "class" / "net" / nic).mkdir(parents=True)
        (sysr / "class" / "net" / nic / "device").symlink_to(dev)
    (sysr / "class" / "net" / "enp7s0.70").mkdir()
    marker = tmp_path / "ran"
    script = _exe(tmp_path / "irq.sh", '#!/bin/sh\necho "ran $*" >> "%s"\n' % marker)
    text = text.replace("/sys/class/net/", "%s/class/net/" % sysr).replace("/usr/local/bin/strih-nic-irq-affinity.sh", str(script))
    return _exe(tmp_path / "hook", text), marker


def test_the_dispatcher_hook_re_places_only_on_up_of_the_rig_nic_driver(tmp_path):
    hook, marker = _hook(tmp_path)
    for iface, action in (("enp7s0", "down"), ("enp2s0", "up"), ("enp7s0.70", "up"), ("lo", "up")):
        assert subprocess.run([str(hook), iface, action], timeout=10).returncode == 0
    assert not marker.exists()
    assert subprocess.run([str(hook), "enp7s0", "up"], timeout=10).returncode == 0
    assert marker.read_text() == "ran \n"


LATE_DROP = """
if sys.argv[1] == "-L":
    import subprocess
    subprocess.Popen(["sh", "-c", "sleep 1.2; echo 6 > '%s'"])
"""


def test_the_carrier_changes_read_waits_for_the_link_drop(tmp_path):
    # the fake -L bumps carrier_changes 1.2 s after it returns, like atlantic's late link drop
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path)
    cc = sysr / "class" / "net" / "enp7s0" / "carrier_changes"
    cc.write_text("4\n")
    _exe(bin_dir / "ethtool", (bin_dir / "ethtool").read_text() + LATE_DROP % cc)
    env["CARRIER_WAIT_S"] = "5"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr
    assert "carrier_changes 4 -> 6" in r.stdout


def test_a_file_with_our_header_on_line_2_is_ours_only_after_a_shebang(tmp_path):
    hdr = "# camera-box issue 1425: housekeeping off the OBS CPUs (scripts/lib/strih-housekeeping.sh) -- do not edit"
    cases = {"a": hdr + "\nx\n", "b": "#!/bin/sh\n" + hdr + "\n", "c": "net.x = 1\n" + hdr + "\n", "d": ""}
    want = {"a": "0", "b": "0", "c": "1", "d": "1"}
    for k, text in cases.items():
        f = tmp_path / k
        f.write_text(text)
        r = _run('rc=0; strih_housekeeping_ours "$1" || rc=$?; echo "$rc"', str(f))
        assert r.stdout.strip() == want[k], (k, r.stdout, r.stderr)


# --- the split (design 7116063669): the IRQ CPU and the task CPUs from the one fact ---------------------

def test_the_irqs_get_the_whole_set_and_the_tasks_its_first_cpu():
    # design 7119179275 (supersedes the CPU-1-alone split of 7116063669)
    for hk, irq, task in (("0-1", "0-1", "0"), ("0,1", "0-1", "0"), ("3", "3", "3"), ("0-1,8", "0-1,8", "0"),
                          ("0-3", "0-3", "0"), ("none", "none", "none")):
        r = _run('strih_housekeeping_irq_cpus "$1"; strih_housekeeping_task_cpus "$1"', hk)
        assert r.returncode == 0, (hk, r.stderr)
        assert r.stdout.split() == [irq, task], (hk, r.stdout)
    r = _run('strih_box_load strih-snv; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus')
    assert r.stdout.split() == ["0-1", "0"], r.stdout
    r = _run('strih_box_load strih-pp; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus')
    assert r.stdout.split() == ["none", "none"], r.stdout
    for badv in ("a", "1-0", ""):
        assert _run('strih_housekeeping_irq_cpus "$1"', badv).returncode != 0, badv


def test_the_rendered_oneshot_carries_every_housekeeping_cpu_and_a_2_s_sample():
    r = _run('strih_box_load strih-snv; strih_nic_irq_affinity_script_text')
    assert r.returncode == 0, r.stderr
    assert 'IRQ_CPUS="${STRIH_HOUSEKEEPING_IRQ_CPUS:-0-1}"' in r.stdout
    assert 'IRQ_RATE_SAMPLE_S="${IRQ_RATE_SAMPLE_S:-2}"' in r.stdout


def test_the_slice_row_fails_an_irq_cpu_in_the_background_slice(tmp_path):
    for allowed, want in (("0", "PASS (hk-slice) rig-background.slice: AllowedCPUs 0, off the IRQ-only CPUs 1"),
                          ("0-1", "FAIL (hk-slice) rig-background.slice: AllowedCPUs 0-1 holds the IRQ-only CPU 1"),
                          ("0 12-13", "PASS (hk-slice) rig-background.slice: AllowedCPUs 0,12-13, off the IRQ-only CPUs 1"),
                          ("", "FAIL (hk-slice) rig-background.slice: AllowedCPUs unread or empty")):
        d = tmp_path / ("c%d" % len(list(tmp_path.iterdir())))
        home, path, env = _grade_env(d)
        env["FAKE_ALLOWED_CPUS"] = allowed
        r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
        assert r.returncode == 0, r.stderr
        assert want in r.stdout, (allowed, r.stdout)


def test_the_sampler_runs_on_the_obs_mask_at_nice_10_on_a_housekeeping_box(tmp_path):
    # design 7119179275: the sampler never shares a CPU with the rig NIC's RX softirq again: with housekeeping
    # CPUs it runs on the OBS mask (14 CPUs, ~9 idle) at nice 10; a `none` box keeps the background set
    (tmp_path / "online").write_text("0-15\n")
    (tmp_path / "iso").write_text("2-15\n")
    env = {"OBS_BOX_ONLINE_CPUS": str(tmp_path / "online"), "STRIH_PROGRAM_AUDIO_ISOLATED_CONF": str(tmp_path / "iso"),
           "STRIH_PROGRAM_AUDIO_SYSFS": str(tmp_path / "nosys")}
    r = _run('strih_box_load strih-snv; strih_program_audio_cpus; echo; strih_program_audio_nice', env=env)
    assert r.returncode == 0 and r.stdout.split() == ["2-15", "10"], (r.stdout, r.stderr)
    r = _run('strih_box_load strih-pp; strih_program_audio_cpus; echo; strih_program_audio_nice; echo END', env=env)
    assert r.returncode == 0 and r.stdout.split() == ["0-1", "END"], (r.stdout, r.stderr)
    r = _run('strih_box_load strih-snv; strih_program_audio_unit_text "$1" "$(strih_program_audio_cpus)" '
             '"$(strih_program_audio_nice)"', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    i = lines.index("CPUAffinity=2-15")
    assert lines[i + 1] == "Nice=10" and r.stdout.count("\nNice=") == 1
    # no OBS mask yet: refused by name, never an every-CPU unit
    env["STRIH_PROGRAM_AUDIO_ISOLATED_CONF"] = str(tmp_path / "absent")
    r = _run('strih_box_load strih-snv; strih_program_audio_cpus', env=env)
    assert r.returncode == 1 and r.stdout == "" and "OBS mask" in r.stderr, (r.stdout, r.stderr)


def test_setup_and_verify_keep_the_background_slice_off_the_irq_cpu():
    s = SETUP.read_text()
    v = VERIFY.read_text()
    # read ONCE and fail-loud (a substitution inside an argument would turn a failed read into `none`)
    read = s.index('HK_SLICE_EXCL="$(strih_housekeeping_slice_exclude)" || fail "issue 1425:')
    assert read < s.index('obs_box_background_slice strih "$DESKTOP_USER" "$HK_SLICE_EXCL" ;')
    assert s.count('obs_box_background_slice strih "$DESKTOP_USER" "$HK_SLICE_EXCL" ;') == 1
    vread = v.index('HK_SLICE_EXCL="$(strih_housekeeping_slice_exclude)" || { bad "(housekeeping)')
    assert vread < v.index('obs_box_baseline_gather_snippet strih "${STRIH_LX_USER:-newlevel}" strih-obs.service '
                           '"$(strih_box_desktop)" "$HK_SLICE_EXCL")')


def test_the_slice_exclusion_is_the_irq_cpu_unless_it_is_the_only_one():
    for hk, want in (("0-1", "1"), ("0,1", "1"), ("0-3", "1-3"), ("3", "none"), ("none", "none")):
        r = _run('strih_housekeeping_slice_exclude "$1"', hk)
        assert r.returncode == 0 and r.stdout.strip() == want, (hk, r.stdout, r.stderr)
    r = _run('strih_box_load strih-snv; strih_housekeeping_slice_exclude')
    assert r.stdout.strip() == "1", r.stdout


def test_a_one_cpu_set_keeps_the_slice_and_the_sampler_and_passes_the_slice_row(tmp_path):
    # review finding: STRIH_HOUSEKEEPING_CPUS=0 gives IRQs and tasks CPU 0, so nothing may be kept out of the
    # slice (else no CPU is left for it and setup fails); the sampler and the (hk-slice) row follow
    box = _box_with(tmp_path, "0")
    (tmp_path / "online").write_text("0-15\n")
    (tmp_path / "iso").write_text("1-15\n")
    env = dict(box, OBS_BOX_ONLINE_CPUS=str(tmp_path / "online"),
               STRIH_PROGRAM_AUDIO_ISOLATED_CONF=str(tmp_path / "iso"), STRIH_PROGRAM_AUDIO_SYSFS=str(tmp_path / "nosys"))
    r = _run('strih_box_load strih-snv; strih_housekeeping_slice_exclude; strih_program_audio_cpus; echo; '
             'obs_box_background_cpus 0-15 1-15 "$(strih_housekeeping_slice_exclude)"', env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["none", "1-15", "0"], r.stdout
    home, path, genv = _grade_env(tmp_path / "g")
    genv.update(box)
    genv["FAKE_ALLOWED_CPUS"] = "0"
    r = _run('strih_box_load strih-snv; strih_housekeeping_slice_row "$(strih_housekeeping_slice_exclude)"',
             env=genv, path_prefix=path)
    assert "PASS (hk-slice) rig-background.slice: AllowedCPUs 0 (one housekeeping CPU: the rig NIC IRQs share it)" in r.stdout, r.stdout


# --- the delayed re-balance after boot (design 7119686502) ------------------------------------------------

REBALANCE_SERVICE = REPO / "systemd" / "strih-nic-irq-rebalance.service"
REBALANCE_TIMER = REPO / "systemd" / "strih-nic-irq-rebalance.timer"


def test_the_rebalance_units_are_the_lib_text_byte_for_byte():
    for fn, committed in (("strih_housekeeping_rebalance_service_text", REBALANCE_SERVICE),
                          ("strih_housekeeping_rebalance_timer_text", REBALANCE_TIMER)):
        r = _run(fn)
        assert r.returncode == 0, r.stderr
        assert r.stdout == committed.read_text(), fn


def test_the_rebalance_service_is_a_plain_oneshot_of_the_irq_script_after_the_boot_oneshot():
    lines = REBALANCE_SERVICE.read_text().splitlines()
    assert lines[0].startswith("# camera-box issue 1425")
    assert "Type=oneshot" in lines
    assert "ExecStart=/usr/local/bin/strih-nic-irq-affinity.sh" in lines
    assert "After=strih-nic-irq-affinity.service" in lines
    # once per boot from the timer: no RemainAfterExit (a later start would be a no-op), no [Install]
    assert not [x for x in lines if x.startswith("RemainAfterExit")]
    assert "[Install]" not in lines
    assert "strih-snv" not in REBALANCE_SERVICE.read_text()


def test_the_rebalance_timer_fires_once_5_min_after_boot():
    text = REBALANCE_TIMER.read_text()
    lines = text.splitlines()
    assert lines[0].startswith("# camera-box issue 1425")
    assert "OnBootSec=5min" in lines and "Unit=strih-nic-irq-rebalance.service" in lines
    assert "WantedBy=timers.target" in lines
    # no catch-up of a missed run and no periodic re-run: each IRQ move costs a loss burst
    assert "Persistent" not in text and "OnUnitActiveSec" not in text and "OnCalendar" not in text
    assert "strih-snv" not in text


def test_setup_writes_the_rebalance_units_and_enables_the_timer_without_starting_it(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr + r.stdout
    unitdir = root / "etc" / "systemd" / "system"
    assert (unitdir / "strih-nic-irq-rebalance.service").read_text() == REBALANCE_SERVICE.read_text()
    assert (unitdir / "strih-nic-irq-rebalance.timer").read_text() == REBALANCE_TIMER.read_text()
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    assert "enable strih-nic-irq-rebalance.timer" in calls
    assert not [c for c in calls if c.startswith("start") or "--now" in c or "rebalance.service" in c], calls


def test_a_disabled_timer_is_enabled_again_by_the_next_setup_run(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    body = 'strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"'
    assert _run(body, ME, str(home), env=env, path_prefix=bin_dir).returncode == 0
    (tmp_path / "systemctl.enabled").write_text("")
    (tmp_path / "systemctl.log").write_text("")
    r = _run(body, ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    assert (tmp_path / "systemctl.log").read_text().splitlines() == [
        "is-enabled -q strih-nic-irq-rebalance.timer", "enable strih-nic-irq-rebalance.timer"]


def test_a_foreign_rebalance_unit_is_never_removed_or_disabled(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    unitdir = root / "etc" / "systemd" / "system"
    unitdir.mkdir(parents=True)
    (unitdir / "strih-nic-irq-rebalance.timer").write_text("[Timer]\nOnBootSec=1min\n")
    env.update(_box_with(tmp_path, "none"))
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    assert (unitdir / "strih-nic-irq-rebalance.timer").exists() and "not ours" in r.stderr
    assert "disable" not in (tmp_path / "systemctl.log").read_text()


def _rebalance_rows(tmp_path, **env_over):
    home, path, env = _grade_env(tmp_path)
    env.update(env_over)
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert r.returncode == 0, r.stderr
    return [x for x in r.stdout.splitlines() if "(hk-rebalance)" in x], env


def test_verify_passes_an_enabled_timer_whose_run_this_boot_succeeded(tmp_path):
    rows, _ = _rebalance_rows(tmp_path)
    assert rows == ["PASS (hk-rebalance) strih-nic-irq-rebalance.timer enabled; the re-balance ran this boot: "
                    "success (exit 0)"], rows


NOT_RUN = "ExecMainStartTimestampMonotonic=0\nResult=success\nExecMainStatus=0"


def test_verify_notes_a_re_balance_that_has_not_run_yet_this_boot(tmp_path):
    rows, _ = _rebalance_rows(tmp_path, FAKE_REBALANCE_SHOW=NOT_RUN)
    assert rows == ["NOTE (hk-rebalance) strih-nic-irq-rebalance.timer enabled and waiting; the re-balance has "
                    "not run yet this boot (it runs 5 min after boot)"], rows


def test_verify_notes_a_timer_enabled_but_not_started_this_boot(tmp_path):
    # review finding: setup enables the timer without starting it, so the boot it was installed in never runs it
    rows, _ = _rebalance_rows(tmp_path, FAKE_REBALANCE_SHOW=NOT_RUN,
                              FAKE_REBALANCE_TIMER_SHOW="ActiveState=inactive\nSubState=dead")
    assert rows == ["NOTE (hk-rebalance) strih-nic-irq-rebalance.timer enabled but not started this boot "
                    "(installed after boot): the first re-balance runs 5 min after the next boot"], rows


def test_verify_fails_a_timer_that_fired_without_running_the_re_balance(tmp_path):
    # review finding: an elapsed timer with no service run must never read as a NOTE forever
    rows, _ = _rebalance_rows(tmp_path, FAKE_REBALANCE_SHOW=NOT_RUN,
                              FAKE_REBALANCE_TIMER_SHOW="ActiveState=active\nSubState=elapsed")
    assert len(rows) == 1 and rows[0].startswith("FAIL (hk-rebalance) strih-nic-irq-rebalance.timer is active/elapsed "
                                                 "but the re-balance never ran this boot"), rows


def test_a_failed_daemon_reload_warns_and_enables_nothing(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    env["FAKE_SYSTEMCTL_FAIL"] = "daemon-reload"
    env["FAKE_ACTIVE"] = ""
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    assert "WARN: daemon-reload failed -- run it, then systemctl enable strih-nic-irq-rebalance.timer" in r.stdout
    assert "enable strih-nic-irq-rebalance.timer" not in (tmp_path / "systemctl.log").read_text()


def test_verify_fails_a_failed_re_balance_run(tmp_path):
    rows, _ = _rebalance_rows(tmp_path, FAKE_REBALANCE_SHOW="ExecMainStartTimestampMonotonic=301204512\n"
                                                            "Result=exit-code\nExecMainStatus=1")
    assert len(rows) == 1 and rows[0].startswith("FAIL (hk-rebalance) the re-balance ran this boot and failed: "
                                                 "exit-code (exit 1)"), rows
    assert "journalctl -b -u strih-nic-irq-rebalance.service" in rows[0]


def test_verify_fails_an_unreadable_re_balance_state_empty_or_garbage(tmp_path):
    for i, mono in enumerate(("", "12x4")):
        rows, _ = _rebalance_rows(tmp_path / str(i), FAKE_REBALANCE_SHOW=f"ExecMainStartTimestampMonotonic={mono}\n"
                                                                         "Result=success\nExecMainStatus=0")
        assert rows == ["FAIL (hk-rebalance) strih-nic-irq-rebalance.timer enabled; the service's state is "
                        f"unreadable (ExecMainStartTimestampMonotonic '{mono}')"], rows


def test_verify_fails_a_disabled_timer(tmp_path):
    home, path, env = _grade_env(tmp_path)
    pathlib.Path(env["FAKE_SYSTEMCTL_ENABLED"]).write_text("")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    rows = [x for x in r.stdout.splitlines() if "(hk-rebalance)" in x]
    assert len(rows) == 1 and rows[0].startswith("FAIL (hk-rebalance) strih-nic-irq-rebalance.timer is not enabled"), rows


def test_verify_fails_a_drifted_or_missing_rebalance_unit(tmp_path):
    for name in ("strih-nic-irq-rebalance.service", "strih-nic-irq-rebalance.timer"):
        home, path, env = _grade_env(tmp_path / name)
        f = pathlib.Path(env["STRIH_HOUSEKEEPING_ROOT"]) / "etc" / "systemd" / "system" / name
        f.write_text(f.read_text().replace("5min", "1min").replace("Type=oneshot", "Type=simple"))
        r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
        rows = [x for x in r.stdout.splitlines() if "(hk-rebalance)" in x]
        assert len(rows) == 1 and rows[0].startswith("FAIL (hk-rebalance) ") and "drifted" in rows[0], (name, rows)


def test_an_inactive_timer_installed_before_this_boot_fails(tmp_path):
    # review round 2: a timer whose file predates the boot should have started with timers.target
    home, path, env = _grade_env(tmp_path)
    f = pathlib.Path(env["STRIH_HOUSEKEEPING_ROOT"]) / "etc" / "systemd" / "system" / "strih-nic-irq-rebalance.timer"
    os.utime(f, (1_000_000_000, 1_000_000_000))
    (pathlib.Path(env["STRIH_HOUSEKEEPING_PROC"]) / "stat").write_text("cpu  1 2 3\nbtime 1700000000\nprocesses 9\n")
    env.update(FAKE_REBALANCE_SHOW=NOT_RUN, FAKE_REBALANCE_TIMER_SHOW="ActiveState=inactive\nSubState=dead")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    rows = [x for x in r.stdout.splitlines() if "(hk-rebalance)" in x]
    assert len(rows) == 1 and rows[0].startswith("FAIL (hk-rebalance) strih-nic-irq-rebalance.timer is enabled but "
                                                 "inactive, installed before this boot"), rows


def test_none_never_reset_fails_a_foreign_service_beside_our_timer(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    assert _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env,
                path_prefix=bin_dir).returncode == 0
    svc = root / "etc" / "systemd" / "system" / "strih-nic-irq-rebalance.service"
    svc.write_text("[Service]\nExecStart=/bin/true\n")
    env.update(_box_with(tmp_path, "none"))
    (tmp_path / "systemctl.log").write_text("")
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr
    assert svc.exists() and "not ours" in r.stderr
    log = (tmp_path / "systemctl.log").read_text()
    assert "disable --now strih-nic-irq-rebalance.timer" in log and "reset-failed" not in log


# --- the RX ring at the driver's pre-set maximum (24.11.2026: InDroppedDma 3272 -> 30178 under E2E load) ------

def test_the_oneshot_raises_the_rx_ring_to_the_driver_maximum_and_reads_it_back(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135), ring=2048)
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    calls = (tmp_path / "ethtool.log").read_text().splitlines()
    assert "-G enp7s0 rx 8184" in calls, calls
    assert (tmp_path / "ring").read_text().split()[0] == "8184"
    assert "enp7s0: RX ring 2048 -> 8184 (the driver's pre-set maximum)" in r.stdout, r.stdout
    assert "done: rig NIC IRQ(s) [134 135] placed" in r.stdout


def test_a_ring_already_at_the_maximum_is_left_alone(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135))
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr
    assert "enp7s0: RX ring already 8184 (the driver's pre-set maximum) -- unchanged" in r.stdout, r.stdout
    assert "-G" not in (tmp_path / "ethtool.log").read_text()


def test_a_failed_ring_change_fails_the_unit(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135), ring=2048)
    env["FAKE_ETHTOOL_RING_FAIL"] = "1"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL: ethtool -G enp7s0 rx 8184 failed (was 2048)" in r.stderr, r.stderr


def test_a_driver_without_ring_control_keeps_its_ring_and_still_pins(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134, 135))
    (tmp_path / "ring").unlink()
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "enp7s0: RX ring not readable (the driver offers no ring control) -- left alone" in r.stdout, r.stdout
    assert "done: rig NIC IRQ(s) [134 135] placed" in r.stdout


def test_verify_passes_a_ring_at_the_maximum_and_fails_a_smaller_one(tmp_path):
    assert "PASS (hk-ring) enp7s0: RX ring 8184 = the driver's pre-set maximum" in _grade(tmp_path)
    out = _grade(tmp_path / "b", ring=2048)
    assert ("FAIL (hk-ring) enp7s0: RX ring 2048, the driver's pre-set maximum is 8184 -- systemctl restart "
            "strih-nic-irq-affinity.service (off-production: a ring change drops the link once)") in out, out


def test_verify_notes_a_driver_without_ring_control(tmp_path):
    home, path, env = _grade_env(tmp_path)
    (tmp_path / "ring").unlink()
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "NOTE (hk-ring) enp7s0: RX ring not readable (the driver offers no ring control)" in r.stdout, r.stdout
    assert "FAIL" not in r.stdout, r.stdout
