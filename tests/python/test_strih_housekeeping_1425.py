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


def _pcie_box(tmp_path, combined=8, irqs=range(134, 143)):
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
    env = {"FAKE_ETHTOOL_STATE": str(tmp_path / "state"), "FAKE_ETHTOOL_LOG": str(tmp_path / "ethtool.log"),
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
                 "PROC_INTERRUPTS": str(tmp_path / "no-interrupts")})
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=full, timeout=60)


def test_the_oneshot_cuts_rss_to_the_irq_cpu_count_and_pins_every_rig_nic_irq_to_cpu_1(tmp_path):
    # design 7116063669: the LAST housekeeping CPU (strih-snv 1) carries the rig NIC IRQs alone
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path)
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    calls = (tmp_path / "ethtool.log").read_text().splitlines()
    assert "-L enp7s0 combined 1" in calls
    assert "RSS queues 8 -> 1" in r.stdout
    assert "carrier_changes ? -> ?" in r.stdout   # no carrier_changes file in the fixture: named, never fatal
    # the IRQs are read AFTER the channel change: the re-requested vectors 150-151, all on 1
    for n in (150, 151):
        assert (irq_dir / str(n) / "smp_affinity_list").read_text().strip() == "1"
        assert "IRQ %d (msi enp7s0, iface enp7s0) pinned to cpu 1" % n in r.stdout
    assert "done: rig NIC IRQ(s) [150 151] pinned to cpu 1" in r.stdout


def test_a_second_run_changes_no_channel_and_still_pins(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=1, irqs=(134, 135, 136))
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr
    calls = (tmp_path / "ethtool.log").read_text().splitlines()
    assert calls == ["-l enp7s0"], calls   # never a -L (a ring re-init) when the count is right
    assert "RSS queues already 1 (combined) -- unchanged" in r.stdout
    for n in (134, 135, 136):
        assert (irq_dir / str(n) / "smp_affinity_list").read_text().strip() == "1"


def test_a_failed_channel_change_fails_the_unit_and_pins_nothing(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path)
    env["FAKE_ETHTOOL_FAIL"] = "1"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 1
    assert "FATAL: ethtool -L enp7s0 combined 1 failed (was 8)" in r.stderr
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
    for n in (134, 135, 136):
        assert (irq_dir / str(n) / "smp_affinity_list").read_text().strip() == "1"


def test_an_msi_vector_with_no_proc_irq_entry_is_skipped(tmp_path):
    # strih-snv 23.11.2026: msi_irqs lists 143, /proc/irq/143 does not exist (a vector the driver never requested)
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=1, irqs=(134, 135))
    (sysr / "devices" / "pci0000:00" / "0000:07:00.0" / "msi_irqs" / "143").write_text("msix\n")
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert "IRQ 143 of enp7s0 has no /proc/irq entry (not requested) -- skipped" in r.stdout
    assert "done: rig NIC IRQ(s) [134 135] pinned to cpu 1" in r.stdout


def test_an_irq_the_kernel_refuses_is_named_and_fails_after_trying_every_one(tmp_path):
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=1, irqs=(134, 135, 136))
    (irq_dir / "135" / "smp_affinity_list").chmod(0o444)
    try:
        r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    finally:
        (irq_dir / "135" / "smp_affinity_list").chmod(0o644)
    assert r.returncode == 1
    assert "IRQ(s) [135] of enp7s0 could not be pinned to cpu 1" in r.stderr
    assert (irq_dir / "134" / "smp_affinity_list").read_text().strip() == "1"
    assert (irq_dir / "136" / "smp_affinity_list").read_text().strip() == "1"


def test_a_kernel_compressed_read_back_counts_as_the_same_set(tmp_path):
    # an IRQ set written `0,1` reads back `0-1` from the kernel: a set compare, never text
    sysr, irq_dir, bin_dir, env = _pcie_box(tmp_path, combined=2, irqs=(134,))
    env["STRIH_HOUSEKEEPING_IRQ_CPUS"] = "0,1"
    r = _render_and_run(tmp_path, sysr, irq_dir, bin_dir, env)
    assert r.returncode == 0, r.stderr
    assert (irq_dir / "134" / "smp_affinity_list").read_text().strip() == "0,1"


# --- setup: the housekeeping install ---------------------------------------------------------------------

FAKE_SYSTEMCTL = r'''#!/usr/bin/env python3
import os, sys
args = [a for a in sys.argv[1:] if a != "--user"]
with open(os.environ["FAKE_SYSTEMCTL_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
if args[:1] == ["is-active"]:
    unit = args[-1]
    sys.exit(0 if unit in os.environ.get("FAKE_ACTIVE", "").split() else 3)
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
           "FAKE_ACTIVE": "rustdesk.service strih-display-watch.service"}
    return root, home, bin_dir, env


def test_setup_writes_the_sysctl_the_two_dropins_the_launcher_file_and_restarts_the_running_units(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    r = _run('strih_box_load strih-snv; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0, r.stderr + r.stdout
    # design 7116063669: the launcher's file + the drop-ins carry the TASK CPUs (0), the IRQs keep CPU 1 alone
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
                       'RUN+="/usr/sbin/ethtool -L $name combined 1"')
    hook = root / "etc" / "NetworkManager" / "dispatcher.d" / "60-strih-housekeeping-irq"
    assert hook.read_text().startswith("#!/bin/sh\n# camera-box issue 1425") and os.access(hook, os.X_OK)
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    assert calls == ["udevadm control --reload-rules",
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
    assert (tmp_path / "systemctl.log").read_text() == ""
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
    calls = (tmp_path / "systemctl.log").read_text().splitlines()
    assert calls == ["daemon-reload", "--user daemon-reload"], calls


def test_a_none_box_with_nothing_of_ours_is_silent(tmp_path):
    root, home, bin_dir, env = _install_env(tmp_path)
    r = _run('strih_box_load strih-pp; strih_housekeeping_install "$1" "$2"', ME, str(home), env=env, path_prefix=bin_dir)
    assert r.returncode == 0 and r.stdout == "" and r.stderr == "", (r.stdout, r.stderr)
    assert (tmp_path / "systemctl.log").read_text() == ""


# --- verify: item 16's housekeeping rows -----------------------------------------------------------------

def _grade_env(tmp_path, irq_aff="1", combined=1, nice=10, cpus="0", running=True):
    tmp_path.mkdir(parents=True, exist_ok=True)
    sysr, irq_dir, bin_dir, eenv = _pcie_box(tmp_path, combined=combined, irqs=(134, 135, 136))
    for n in (134, 135, 136):
        (irq_dir / str(n) / "smp_affinity_list").write_text(irq_aff + "\n")
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
    for i, unit in enumerate(("rustdesk.service", "strih-display-watch.service", "strih-obs.service")):
        pid = str(4000 + i)
        env["FAKE_PID_" + unit.replace("-", "_").replace(".", "_")] = pid if running else "0"
        (proc / pid).mkdir()
        (proc / pid / "status").write_text("Name:\tx\nCpus_allowed_list:\t%s\n" % cpus)
        fields = ["S"] + ["0"] * 15 + [str(nice)] + ["1"] * 30
        (proc / pid / "stat").write_text("%s (a b) %s\n" % (pid, " ".join(fields)))
    stats = sysr / "class" / "net" / "enp7s0" / "statistics"
    stats.mkdir()
    (stats / "rx_missed_errors").write_text("7\n")
    (stats / "rx_dropped").write_text("0\n")
    env.update({"STRIH_HOUSEKEEPING_SYS": str(sysr), "STRIH_HOUSEKEEPING_PROC": str(proc), "STRIH_NIC_IFACE": "enp7s0",
                "STRIH_HOUSEKEEPING_IPADDR": "", "STRIH_HOUSEKEEPING_SAMPLE_S": "0"})
    return home, "%s:%s" % (bin_dir, sbin), env


def _grade(tmp_path, **kw):
    home, path, env = _grade_env(tmp_path, **kw)
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_a_placed_box_passes_every_row(tmp_path):
    out = _grade(tmp_path)
    rows = out.splitlines()
    assert all(r.startswith("PASS") or r.startswith("NOTE (hk-rx)") for r in rows), out
    assert "NOTE (hk-rx) enp7s0: rx_missed_errors +0, rx_dropped +0 over 0 s, total missed 7 (report-only)" in out
    assert "PASS (hk-cpus) housekeeping CPUs 0-1 = online 0-15 minus the OBS mask 2-15: rig NIC IRQs on 1, tasks on 0" in out
    assert "PASS (hk-slice) rig-background.slice: AllowedCPUs 0, off the IRQ CPUs 1" in out
    for tag in ("(hk-cpus)", "(hk-rss)", "(hk-irq)", "(hk-hooks)", "(hk-sysctl)", "(hk-slice)", "(hk-unit) rustdesk.service",
                "(hk-unit) strih-display-watch.service", "(hk-unit) launcher"):
        assert any(tag in r for r in rows), (tag, out)


def test_verify_notes_a_driver_without_channel_control_and_skips_an_unrequested_vector(tmp_path):
    home, path, env = _grade_env(tmp_path)
    eth = pathlib.Path(path.split(":")[0]) / "ethtool"
    _exe(eth, "#!/bin/sh\necho 'netlink error: Operation not supported' >&2\nexit 1\n")
    msi = pathlib.Path(env["STRIH_HOUSEKEEPING_SYS"]) / "devices" / "pci0000:00" / "0000:07:00.0" / "msi_irqs"
    (msi / "143").write_text("msix\n")
    r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
    assert "NOTE (hk-rss) enp7s0: RSS channel count not readable (the driver offers no channel control): queues left alone" in r.stdout, r.stdout
    assert "PASS (hk-irq) enp7s0: IRQ(s) 134 135 136 on CPUs 1" in r.stdout, r.stdout
    assert "FAIL" not in r.stdout, r.stdout


def test_an_irq_left_on_obs_cpus_fails(tmp_path):
    out = _grade(tmp_path, irq_aff="0-15")
    assert "FAIL (hk-irq) enp7s0: IRQ(s) off the IRQ CPUs 1: 134=0-15 135=0-15 136=0-15" in out
    # the IRQs still on the whole housekeeping set (the placement before the split) fail too
    assert "FAIL (hk-irq) enp7s0: IRQ(s) off the IRQ CPUs 1: 134=0-1" in _grade(tmp_path / "b", irq_aff="0-1")


def test_eight_rss_queues_fail(tmp_path):
    assert "FAIL (hk-rss) enp7s0: 8 RSS queues" in _grade(tmp_path, combined=8)


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

def test_the_irq_cpu_is_the_last_of_the_set_and_the_tasks_get_the_rest():
    for hk, irq, task in (("0-1", "1", "0"), ("0,1", "1", "0"), ("3", "3", "3"), ("0-1,8", "8", "0-1"),
                          ("0-3", "3", "0-2"), ("none", "none", "none")):
        r = _run('strih_housekeeping_irq_cpus "$1"; strih_housekeeping_task_cpus "$1"', hk)
        assert r.returncode == 0, (hk, r.stderr)
        assert r.stdout.split() == [irq, task], (hk, r.stdout)
    r = _run('strih_box_load strih-snv; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus')
    assert r.stdout.split() == ["1", "0"], r.stdout
    r = _run('strih_box_load strih-pp; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus')
    assert r.stdout.split() == ["none", "none"], r.stdout
    for badv in ("a", "1-0", ""):
        assert _run('strih_housekeeping_irq_cpus "$1"', badv).returncode != 0, badv


def test_the_rendered_oneshot_carries_the_irq_cpu_only():
    r = _run('strih_box_load strih-snv; strih_nic_irq_affinity_script_text')
    assert r.returncode == 0, r.stderr
    assert 'IRQ_CPUS="${STRIH_HOUSEKEEPING_IRQ_CPUS:-1}"' in r.stdout
    assert "HOUSEKEEPING_CPUS:-0-1" not in r.stdout


def test_the_slice_row_fails_an_irq_cpu_in_the_background_slice(tmp_path):
    for allowed, want in (("0", "PASS (hk-slice) rig-background.slice: AllowedCPUs 0, off the IRQ CPUs 1"),
                          ("0-1", "FAIL (hk-slice) rig-background.slice: AllowedCPUs 0-1 holds the IRQ CPU 1"),
                          ("0 12-13", "PASS (hk-slice) rig-background.slice: AllowedCPUs 0,12-13, off the IRQ CPUs 1"),
                          ("", "FAIL (hk-slice) rig-background.slice: AllowedCPUs unread or empty")):
        d = tmp_path / ("c%d" % len(list(tmp_path.iterdir())))
        home, path, env = _grade_env(d)
        env["FAKE_ALLOWED_CPUS"] = allowed
        r = _run('strih_box_load strih-snv; strih_housekeeping_grade_report "$1" "$2"', str(home), ME, env=env, path_prefix=path)
        assert r.returncode == 0, r.stderr
        assert want in r.stdout, (allowed, r.stdout)


def test_the_sampler_runs_on_the_task_cpus_on_an_amd_box(tmp_path):
    # strih-snv has no cpu_atom: the sampler's CPUAffinity is the background set, now without the IRQ CPU
    (tmp_path / "online").write_text("0-15\n")
    (tmp_path / "iso").write_text("2-15\n")
    env = {"OBS_BOX_ONLINE_CPUS": str(tmp_path / "online"), "STRIH_PROGRAM_AUDIO_ISOLATED_CONF": str(tmp_path / "iso"),
           "STRIH_PROGRAM_AUDIO_SYSFS": str(tmp_path / "nosys")}
    r = _run('strih_box_load strih-snv; strih_program_audio_cpus', env=env)
    assert r.returncode == 0 and r.stdout == "0", (r.stdout, r.stderr)
    r = _run('strih_box_load strih-pp; strih_program_audio_cpus', env=env)
    assert r.returncode == 0 and r.stdout == "0-1", (r.stdout, r.stderr)


def test_setup_and_verify_keep_the_background_slice_off_the_irq_cpu():
    s = SETUP.read_text()
    v = VERIFY.read_text()
    assert s.count('obs_box_background_slice strih "$DESKTOP_USER" "$(strih_housekeeping_irq_cpus)" ;') == 1
    assert v.count('obs_box_baseline_gather_snippet strih "${STRIH_LX_USER:-newlevel}" strih-obs.service '
                   '"$(strih_box_desktop)" "$(strih_housekeeping_irq_cpus)"') == 1
