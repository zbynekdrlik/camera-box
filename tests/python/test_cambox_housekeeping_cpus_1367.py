"""issue 1367 (design 6180767397): systemd PID 1 and every unit off the capture-IRQ core.

On cam5 (15.10.2026) 65 of 102 capture-phase re-seeds landed within 1 s of a systemd unit start
(chance 7.8 %); with PID 1 restricted to the other general cores, 2 re-seeds in 9 min and none near a
unit start. scripts/lib/cambox-housekeeping-cpus.sh provisions the systemd manager CPUAffinity drop-in
that keeps PID 1 off the issue-1198 reserved capture-IRQ core. These tests run the REAL lib functions
under the callers' `set -euo pipefail`, the REAL gather against a temp drop-in, and the REAL apply
program with stubbed root-only tools on PATH.
"""
from __future__ import annotations

import os
import pathlib
import stat
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
LIB = ROOT / "scripts" / "lib" / "cambox-housekeeping-cpus.sh"
SETUP = ROOT / "scripts" / "setup-device.sh"
VERIFY = ROOT / "scripts" / "verify-device.sh"


def _bash(script: str, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    full = dict(os.environ)
    full.update(env or {})
    return subprocess.run(
        ["bash", "-c", 'set -euo pipefail; . "$0"; ' + script, str(LIB), *args],
        capture_output=True, text=True, env=full, timeout=60,
    )


# --- the derivation: the issue-1198 split ---------------------------------------------------------

def test_the_four_core_fleet_keeps_housekeeping_on_0_and_1():
    r = _bash('cambox_hk_cpus "$1" "$2"', "0-3", "3")
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "0,1"


def test_the_highest_general_core_is_the_reserved_irq_core_on_a_bigger_box():
    r = _bash('cambox_hk_cpus "$1" "$2"', "0-7", "3")
    assert r.stdout.strip() == "0,1,2,4,5,6"


def test_a_box_without_an_isolated_core_gets_no_housekeeping_set():
    r = _bash('cambox_hk_cpus "$1" "$2" || echo rc=$?', "0-3", "")
    assert "rc=1" in r.stdout
    assert "no isolated capture core" in r.stderr


def test_a_box_with_fewer_than_two_general_cores_gets_none():
    r = _bash('cambox_hk_cpus "$1" "$2" || echo rc=$?', "0-1", "1")
    assert "rc=1" in r.stdout
    assert "fewer than 2 general cores" in r.stderr


def test_a_garbage_cpu_list_is_refused():
    for online, iso in (("0-x", "3"), ("", "3"), ("0-3", "3-1"), ("0-3", "a")):
        r = _bash('cambox_hk_cpus "$1" "$2" || echo rc=$?', online, iso)
        assert "rc=1" in r.stdout, (online, iso, r.stdout, r.stderr)


def test_the_cpulist_expansion_matches_the_kernel_format():
    r = _bash('cambox_hk_cpulist_expand "$1" | paste -sd, -', "0-2,5,7-8")
    assert r.stdout.strip() == "0,1,2,5,7,8"


# --- the drop-in --------------------------------------------------------------------------------

def _sys(tmp_path, online="0-3", isolated="3"):
    d = tmp_path / "cpu"
    d.mkdir(exist_ok=True)
    (d / "online").write_text(online + "\n")
    (d / "isolated").write_text(isolated + "\n")
    return d


def test_install_writes_the_manager_cpuaffinity_dropin(tmp_path):
    root = tmp_path / "root"
    r = _bash('cambox_hk_install_dropin "$1"', str(root), env={"CAMBOX_HK_SYS": str(_sys(tmp_path))})
    assert r.returncode == 0, r.stderr
    f = root / "etc/systemd/system.conf.d/60-camera-box-housekeeping-cpus.conf"
    text = f.read_text()
    assert "[Manager]\nCPUAffinity=0,1\n" in text
    assert stat.S_IMODE(f.stat().st_mode) == 0o644
    assert not (f.parent / (f.name + ".new")).exists()


def test_install_writes_nothing_on_a_box_without_an_isolated_core(tmp_path):
    root = tmp_path / "root"
    r = _bash('cambox_hk_install_dropin "$1"', str(root),
              env={"CAMBOX_HK_SYS": str(_sys(tmp_path, isolated=""))})
    assert r.returncode == 0, r.stderr
    assert "not written" in r.stdout
    assert not (root / "etc/systemd/system.conf.d").exists()


# --- the verdict --------------------------------------------------------------------------------

def _block(dropin: str | None = "0,1", pid1="0-1", online="0-3", iso="3", done=True,
           irq_dropin="1", default_aff="3", sample_done=True) -> str:
    lines = [f"ONLINE={online}", f"ISOLATED={iso}", f"PID1={pid1}"]
    if irq_dropin is not None:
        lines.append(f"IRQ_DROPIN={irq_dropin}")
    if default_aff is not None:
        lines.append(f"DEFAULT_AFF={default_aff}")
    if sample_done:
        lines.append("IRQ_SAMPLE_DONE=1")
    if dropin is None:
        lines.append("DROPIN_PRESENT=0")
    else:
        body = _bash('cambox_hk_dropin_content "$1"', dropin).stdout
        lines.append("DROPIN_PRESENT=1")
        lines += ["DROPIN=" + l for l in body.splitlines()]
    if done:
        lines.append("GATHER_DONE=1")
    return "\n".join(lines) + "\n"


def _verdict(block: str) -> str:
    return _bash('cambox_hk_verdict "$1"', block).stdout


def test_verdict_ok_when_the_dropin_matches_and_pid1_runs_there():
    assert _verdict(_block()) == "ok\n"


def test_verdict_fails_a_missing_dropin():
    v = _verdict(_block(dropin=None))
    assert v.startswith("FAIL: ") and "is missing" in v


def test_verdict_fails_a_dropin_naming_other_cores():
    v = _verdict(_block(dropin="0,1,2"))
    assert "differs" in v and "CPUAffinity=0,1" in v


def test_verdict_fails_pid1_still_on_the_irq_core():
    v = _verdict(_block(pid1="0-3"))
    assert "PID 1 runs on cpus 0-3" in v and "taskset" in v


def test_verdict_fails_an_unreadable_pid1_mask():
    v = _verdict(_block(pid1=""))
    assert "could not be read" in v


def test_verdict_fails_a_cut_read():
    v = _verdict(_block(done=False))
    assert v.startswith("FAIL: ") and "GATHER_DONE" in v


def test_verdict_is_ok_none_on_a_box_without_a_reserved_irq_core():
    v = _verdict(_block(dropin=None, iso=""))
    assert v.startswith("ok-none: ")


def test_verdict_fails_a_stray_dropin_on_a_box_without_a_reserved_irq_core():
    v = _verdict(_block(dropin="0,1", iso=""))
    assert v.startswith("FAIL: ") and "remove the drop-in" in v


def test_the_real_gather_reads_a_temp_dropin_back_through_the_verdict(tmp_path):
    d = tmp_path / "hk.conf"
    d.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout)
    snippet = _bash("cambox_hk_gather_remote_snippet", env={"CAMBOX_HK_DROPIN": str(d)}).stdout
    out = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30).stdout
    assert out.rstrip().endswith("GATHER_DONE=1")
    assert "DROPIN=CPUAffinity=0,1" in out
    assert "PID1=" in out and "ONLINE=" in out


# --- the live apply program -----------------------------------------------------------------------

def _stubs(tmp_path, mode="ro", reexec_sets: str | None = "0-1"):
    """findmnt/mount/taskset/hostname stubs. `taskset -pc <set> 1` rewrites the fake
    /proc/1/status to reexec_sets (None = PID 1's mask does not change)."""
    b = tmp_path / "bin"
    b.mkdir()
    log = tmp_path / "log"
    proc = tmp_path / "proc"
    (proc / "1").mkdir(parents=True)
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-3\n")
    state = tmp_path / "rootmode"
    state.write_text(mode)

    def stub(name, body):
        p = b / name
        p.write_text("#!/bin/bash\n" + body)
        p.chmod(0o755)

    stub("hostname", "echo cam9\n")
    stub("findmnt", f'echo "$(cat {state}),relatime"\n')
    stub("mount", f'echo "mount $*" >>{log}\n'
                  f'case "$*" in *remount,rw*) echo rw >{state};; *remount,ro*) echo ro >{state};; esac\n')
    reexec = (f'printf "Name:\\tsystemd\\nCpus_allowed_list:\\t{reexec_sets}\\n" >{proc}/1/status\n'
              if reexec_sets else "")
    stub("taskset", f'echo "taskset $*" >>{log}\n{reexec}')
    stub("systemctl", f'echo "systemctl $*" >>{log}\n')
    stub("sync", "exit 0\n")
    stub("fuser", "exit 1\n")
    stub("lsof", "exit 0\n")
    return b, log, proc


def _irq_in_place(tmp_path):
    """The camera-box irq-affinity drop-in already in place (the older apply tests are about the
    manager drop-in and PID 1, not this one)."""
    p = tmp_path / "irq-affinity.conf"
    if not p.exists():
        p.write_text(_bash("cambox_hk_irq_dropin_content").stdout)
    return p


def _apply(tmp_path, **kw):
    b, log, proc = _stubs(tmp_path, **kw)
    dropin = tmp_path / "etc" / "system.conf.d" / "60-camera-box-housekeeping-cpus.conf"
    env = {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(_sys(tmp_path)),
           "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(_irq_in_place(tmp_path))}
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    r = subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))
    return r, dropin, (log.read_text() if log.exists() else "")


def test_apply_writes_inside_the_rw_window_then_sets_pid1s_mask(tmp_path):
    r, dropin, log = _apply(tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "CPUAffinity=0,1" in dropin.read_text()
    lines = log.splitlines()
    rw = lines.index(next(l for l in lines if "remount,rw" in l))
    ro = lines.index(next(l for l in lines if "remount,ro" in l))
    rex = lines.index("taskset -pc 0,1 1")
    assert rw < ro < rex, log
    assert "PID 1 runs on cpus 0,1" in r.stdout


def test_apply_fails_loudly_when_pid1_does_not_move(tmp_path):
    r, _dropin, _log = _apply(tmp_path, reexec_sets=None)
    assert r.returncode != 0
    assert "PID 1 runs on cpus 0,1,2,3, not 0,1" in r.stderr


def test_apply_is_a_noop_on_a_box_already_in_place(tmp_path):
    b, log, proc = _stubs(tmp_path)
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-1\n")
    dropin = tmp_path / "d.conf"
    dropin.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout)
    env = {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(_sys(tmp_path)),
           "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(_irq_in_place(tmp_path))}
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    r = subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))
    assert r.returncode == 0, r.stdout + r.stderr
    text = log.read_text() if log.exists() else ""
    assert "remount" not in text and "taskset" not in text


# --- the wiring -----------------------------------------------------------------------------------

def test_setup_device_writes_the_dropin_and_verify_device_grades_it():
    setup = SETUP.read_text()
    verify = VERIFY.read_text()
    assert '. "$HERE/lib/cambox-housekeeping-cpus.sh"' in setup
    assert 'cambox_hk_install_dropin ""' in setup
    assert '. "$HERE/lib/cambox-housekeeping-cpus.sh"' in verify
    assert "cambox_hk_gather_remote_snippet" in verify and "cambox_hk_verdict" in verify
    # outside every executed slice: after (ak), before (at); (q) stays last
    au = verify.index("\n# (au) housekeeping cpus")
    assert verify.index("\n# (ak) off-box remote logging") < au < verify.index("\n# (at) zram swap")
    assert au < verify.rindex("# (q) .bak cruft drift")
    assert verify.count("(au) housekeeping cpus (issue 1367)") == 3, "header, usage() and the check"


# --- review round 1 -------------------------------------------------------------------------------

def test_a_fresh_box_reads_isolcpus_from_the_grub_file_step10_just_wrote(tmp_path):
    """Review: on a fresh box the running kernel has no isolcpus yet (STEP 10 only wrote GRUB), so
    the drop-in was never written and (au) failed after the first reboot."""
    root = tmp_path / "root"
    grub = tmp_path / "grub"
    grub.write_text('GRUB_DEFAULT=0\nGRUB_CMDLINE_LINUX=" isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2"\n')
    r = _bash('cambox_hk_install_dropin "$1"', str(root),
              env={"CAMBOX_HK_SYS": str(_sys(tmp_path, isolated="")), "CAMBOX_HK_GRUB": str(grub)})
    assert r.returncode == 0, r.stderr
    f = root / "etc/systemd/system.conf.d/60-camera-box-housekeeping-cpus.conf"
    assert "CPUAffinity=0,1" in f.read_text(), r.stdout


def test_the_grub_isolcpus_parser_drops_flags_and_ignores_comments(tmp_path):
    g = tmp_path / "grub"
    g.write_text('# GRUB_CMDLINE_LINUX="isolcpus=9"\nGRUB_CMDLINE_LINUX_DEFAULT="quiet isolcpus=managed_irq,domain,2-3"\n')
    r = _bash('cambox_hk_isolated_from_grub "$1"', str(g))
    assert r.stdout.strip() == "2-3"
    g.write_text('GRUB_CMDLINE_LINUX="quiet"\n')
    assert _bash('cambox_hk_isolated_from_grub "$1"', str(g)).stdout.strip() == ""


def test_a_preempt_rt_kernel_reserves_no_irq_core():
    r = _bash('cambox_hk_cpus "$1" "$2" "$3" || echo rc=$?', "0-3", "3", "1")
    assert "rc=1" in r.stdout and "PREEMPT_RT" in r.stderr
    assert _verdict(_block(dropin=None) + "RT=1\n").startswith("ok-none: ")


def test_the_live_apply_rewrites_a_dropin_with_an_extra_trailing_blank_line(tmp_path):
    b, log, proc = _stubs(tmp_path)
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-1\n")
    dropin = tmp_path / "d.conf"
    dropin.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout + "\n")
    env = {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(_sys(tmp_path)),
           "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(_irq_in_place(tmp_path))}
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    r = subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "drop-in write" in r.stdout
    assert dropin.read_text() == _bash('cambox_hk_dropin_content "$1"', "0,1").stdout


def test_the_verify_ok_line_says_running_daemons_move_at_the_next_boot():
    assert "running services keep their mask until they restart" in VERIFY.read_text()


# --- review round 2 -------------------------------------------------------------------------------

def test_install_removes_a_leftover_dropin_on_a_box_with_no_reserved_irq_core(tmp_path):
    root = tmp_path / "root"
    f = root / "etc/systemd/system.conf.d/60-camera-box-housekeeping-cpus.conf"
    f.parent.mkdir(parents=True)
    f.write_text("[Manager]\nCPUAffinity=0,1\n")
    rt = tmp_path / "realtime"
    rt.write_text("1\n")
    r = _bash('cambox_hk_install_dropin "$1"', str(root),
              env={"CAMBOX_HK_SYS": str(_sys(tmp_path)), "CAMBOX_HK_REALTIME": str(rt)})
    assert r.returncode == 0, r.stderr
    assert not f.exists() and "removed" in r.stdout


def test_the_live_apply_removes_a_leftover_dropin_inside_the_rw_window(tmp_path):
    b, log, proc = _stubs(tmp_path)
    dropin = tmp_path / "d.conf"
    dropin.write_text("[Manager]\nCPUAffinity=0,1\n")
    env = {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(_sys(tmp_path, isolated="")),
           "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(_irq_in_place(tmp_path))}
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    r = subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert not dropin.exists()
    lines = log.read_text().splitlines()
    assert any("remount,rw" in l for l in lines) and any("remount,ro" in l for l in lines)


def test_the_realtime_flag_must_read_exactly_1(tmp_path):
    rt = tmp_path / "realtime"
    rt.write_text("10\n")
    r = _bash("cambox_hk_kernel_rt", env={"CAMBOX_HK_REALTIME": str(rt)})
    assert r.stdout.strip() == "0" or "PREEMPT_RT" in os.uname().version
    rt.write_text("1\n")
    assert _bash("cambox_hk_kernel_rt", env={"CAMBOX_HK_REALTIME": str(rt)}).stdout.strip() == "1"


def test_verify_fails_a_dropin_without_its_final_newline(tmp_path):
    d = tmp_path / "hk.conf"
    d.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout.rstrip("\n"))
    snippet = _bash("cambox_hk_gather_remote_snippet", env={"CAMBOX_HK_DROPIN": str(d)}).stdout
    out = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30).stdout
    assert "DROPIN_NL=0" in out
    block = "\n".join(l for l in out.splitlines() if not l.startswith(("ONLINE=", "ISOLATED=", "PID1=", "RT=")))
    v = _verdict("ONLINE=0-3\nISOLATED=3\nPID1=0-1\nRT=0\n" + block + "\n")
    assert "differs" in v, v


# --- other device IRQs off the reserved capture-IRQ core (the NIC IRQ finding) --------------------
# IRQ=<n> <capture flag> <smp_affinity_list> <per-cpu interrupt deltas over a 2 s sample>

def test_verify_fails_another_device_irq_that_fired_on_the_reserved_core():
    block = _block() + "IRQ=129 1 2 0:0,1:0,2:5000,3:0\nIRQ=135 0 0-2 0:0,1:0,2:4210,3:0\nIRQ=136 0 0-2 0:3,1:0,2:0,3:0\n"
    v = _verdict(block)
    assert v.startswith("FAIL: ") and "IRQ 135" in v and "IRQ 136" not in v, v
    assert "4210" in v


def test_verify_ignores_idle_irqs_and_the_capture_irq_on_the_reserved_core():
    # an idle IRQ allowed on the reserved core (i8042, a per-cpu wifi queue) never fires there
    block = _block() + "IRQ=1 0 0-2 0:0,1:0,2:0,3:0\nIRQ=141 0 2 0:0,1:0,2:0,3:0\nIRQ=129 1 2 0:0,1:0,2:5000,3:0\nIRQ=135 0 0-1 0:900,1:0,2:0,3:0\n"
    assert _verdict(block) == "ok\n"


def test_the_real_gather_lists_the_irqs_with_flag_affinity_and_deltas():
    snippet = _bash("cambox_hk_gather_remote_snippet", env={"CAMBOX_HK_IRQ_SAMPLE_S": "0.2"}).stdout
    out = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30).stdout
    irq_lines = [l for l in out.splitlines() if l.startswith("IRQ=")]
    assert irq_lines, out
    assert "IRQ_SAMPLE_DONE=1" in out.splitlines()
    assert any(l.startswith("DEFAULT_AFF=") for l in out.splitlines()), out
    online = os.sched_getaffinity(0) | set(range(os.cpu_count() or 1))
    for l in irq_lines:
        n, flag, aff, deltas = l[4:].split(" ")
        assert n.isdigit() and flag in ("0", "1") and aff, l
        pairs = [d.split(":") for d in deltas.split(",")]
        assert all(c.isdigit() and x.isdigit() and int(c) in online for c, x in pairs), l


# --- the fleet unit never ran the IRQ split: the irq-affinity drop-in ------------------------------

def test_the_irq_affinity_dropin_runs_the_irq_setup_outside_the_unit_sandbox():
    body = _bash("cambox_hk_irq_dropin_content").stdout
    assert "[Service]\n" in body
    assert "ExecStartPre=-+/usr/local/bin/camera-box --setup-irq-affinity\n" in body
    assert "ReadWritePaths" not in body


def test_setup_device_writes_the_irq_affinity_dropin_in_step_7():
    setup = SETUP.read_text()
    step7 = setup[setup.index("# STEP 7: Create systemd service"):setup.index("# STEP 8")]
    assert "cambox_hk_irq_dropin_install" in step7


def test_verify_fails_a_box_whose_camera_box_unit_lacks_the_irq_setup():
    v = _verdict(_block(irq_dropin="0"))
    assert v.startswith("FAIL: ") and "--setup-irq-affinity" in v
    assert _verdict(_block(irq_dropin="1")) == "ok\n"


def test_the_live_apply_writes_the_irq_dropin_and_reloads(tmp_path):
    b, log, proc = _stubs(tmp_path)
    (b / "systemctl").write_text(f'#!/bin/bash\necho "systemctl $*" >>{log}\n')
    (b / "systemctl").chmod(0o755)
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-1\n")
    dropin = tmp_path / "d.conf"
    dropin.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout)
    irq = tmp_path / "irq.conf"
    env = {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(_sys(tmp_path)),
           "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(irq)}
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    r = subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                       env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert irq.read_text() == _bash("cambox_hk_irq_dropin_content").stdout
    text = log.read_text()
    assert "remount,rw" in text and "systemctl daemon-reload" in text
    assert text.index("remount,ro") < text.index("systemctl daemon-reload")


# --- review round (issue 1367): every IRQ facet fails closed --------------------------------------

def test_verify_fails_a_gather_without_the_irq_facets():
    v = _verdict(_block(irq_dropin=None))
    assert v.startswith("FAIL: ") and "IRQ_DROPIN" in v, v
    v = _verdict(_block(sample_done=False))
    assert v.startswith("FAIL: ") and "interrupt sample" in v, v
    v = _verdict(_block(default_aff=None))
    assert v.startswith("FAIL: ") and "default_smp_affinity" in v, v


def test_verify_fails_an_irq_dropin_that_runs_inside_the_unit_sandbox():
    v = _verdict(_block(irq_dropin="2"))
    assert v.startswith("FAIL: ") and "sandbox" in v, v


def test_verify_fails_a_default_irq_affinity_that_includes_the_reserved_core():
    v = _verdict(_block(default_aff="7"))
    assert v.startswith("FAIL: ") and "default_smp_affinity" in v and "core 2" in v, v
    assert _verdict(_block(default_aff="00000003")) == "ok\n"
    # a core above 31 sits in its own comma group
    v = _verdict(_block(online="0-35", iso="35", pid1="0-33", dropin="0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33",
                        default_aff="4,00000001"))
    assert "default_smp_affinity" in v and "core 34" in v, v


def test_verify_grades_the_irq_dropin_on_a_box_without_a_reserved_core():
    assert _verdict(_block(dropin=None, iso="")).startswith("ok-none: ")
    v = _verdict(_block(dropin=None, iso="", irq_dropin="0"))
    assert v.startswith("FAIL: ") and "--setup-irq-affinity" in v, v


def test_an_irq_whose_affinity_could_not_be_read_still_counts_its_interrupts():
    v = _verdict(_block() + "IRQ=135 0 - 0:0,1:0,2:4210,3:0\n")
    assert "IRQ 135" in v and "4210" in v, v


def test_the_deltas_follow_the_header_cpu_labels_not_the_column_index():
    # cpu1 offline: the columns are CPU0 CPU2 CPU3, so the reserved core 2 is the SECOND column
    v = _verdict(_block(online="0,2-3") + "IRQ=135 0 0,2 0:0,2:4210,3:0\n")
    assert "IRQ 135" in v, v


def test_verify_and_rust_pick_the_same_reserved_core_with_two_isolated_cores():
    # affinity.rs: capture = the highest isolated core (3), reserved = the highest other core (2)
    r = _bash('cambox_hk_reserved_core "$1" "$2"', "0-3", "2-3")
    assert r.stdout.strip() == "2", r.stdout + r.stderr
    # housekeeping = online - every isolated core - the reserved core
    r = _bash('cambox_hk_cpus "$1" "$2" || echo rc=$?', "0-3", "2-3")
    assert r.stdout.strip() == "0,1", r.stdout + r.stderr


def _fake_systemctl(tmp_path, show: str):
    b = tmp_path / "sbin"
    b.mkdir(exist_ok=True)
    out = tmp_path / "show.txt"
    out.write_text(show)
    (b / "systemctl").write_text(f'#!/bin/bash\n[ "$1" = show ] && cat {out}\nexit 0\n')
    (b / "systemctl").chmod(0o755)
    return b


def _gather_irq_dropin(tmp_path, show: str) -> str:
    b = _fake_systemctl(tmp_path, show)
    snippet = _bash("cambox_hk_gather_remote_snippet", env={"CAMBOX_HK_IRQ_SAMPLE_S": "0"}).stdout
    out = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, timeout=30,
                         env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}")).stdout
    return next(l for l in out.splitlines() if l.startswith("IRQ_DROPIN="))


def test_the_gather_reads_the_loaded_unit_for_the_irq_setup(tmp_path):
    pre = ("ExecStartPreEx={ path=/usr/local/bin/camera-box-free-capture-device.sh ; argv[]=/usr/local/bin/camera-box-free-capture-device.sh ; flags=ignore-failure ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
           " ; { path=/usr/local/bin/camera-box ; argv[]=/usr/local/bin/camera-box --setup-irq-affinity ; flags=ignore-failure privileged ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }\n")
    assert _gather_irq_dropin(tmp_path, pre + "ReadWritePaths=/dev /sys /run\n") == "IRQ_DROPIN=1"
    # loaded without '+' and without a /proc/irq grant: the write would fail EROFS
    sandboxed = pre.replace("flags=ignore-failure privileged", "flags=ignore-failure")
    assert _gather_irq_dropin(tmp_path, sandboxed + "ReadWritePaths=/dev /sys /run\n") == "IRQ_DROPIN=2"
    assert _gather_irq_dropin(tmp_path, sandboxed + "ReadWritePaths=/dev /sys /run /proc/irq\n") == "IRQ_DROPIN=1"
    # the file on disk but no daemon-reload yet, or an empty ExecStartPre= reset: not loaded
    only_free = pre.split(" ; { path=/usr/local/bin/camera-box ;")[0] + "\n"
    assert _gather_irq_dropin(tmp_path, only_free + "ReadWritePaths=/dev /sys /run\n") == "IRQ_DROPIN=0"


def _apply_env(tmp_path, proc, dropin, irq, sys_dir=None):
    return {"CAMBOX_HK_DROPIN": str(dropin), "CAMBOX_HK_SYS": str(sys_dir or _sys(tmp_path)),
            "CAMBOX_HK_PROC": str(proc), "CAMBOX_HK_IRQ_DROPIN": str(irq)}


def _run_prog(b, env):
    prog = _bash("cambox_hk_apply_program", env=env).stdout
    return subprocess.run(["bash", "-s"], input=prog, capture_output=True, text=True, timeout=60,
                          env=dict(os.environ, PATH=f"{b}:{os.environ['PATH']}"))


def test_a_failed_irq_dropin_write_closes_the_window_and_never_reloads(tmp_path):
    b, log, proc = _stubs(tmp_path)
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-1\n")
    dropin = tmp_path / "d.conf"
    dropin.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout)
    blocker = tmp_path / "notadir"
    blocker.write_text("x")
    r = _run_prog(b, _apply_env(tmp_path, proc, dropin, blocker / "irq.conf"))
    assert r.returncode != 0
    assert "could not write" in r.stderr
    text = log.read_text()
    assert "remount,rw" in text and "remount,ro" in text
    assert "daemon-reload" not in text


def test_a_failed_daemon_reload_fails_the_apply(tmp_path):
    b, log, proc = _stubs(tmp_path)
    (b / "systemctl").write_text(f'#!/bin/bash\necho "systemctl $*" >>{log}\nexit 1\n')
    (proc / "1" / "status").write_text("Name:\tsystemd\nCpus_allowed_list:\t0-1\n")
    dropin = tmp_path / "d.conf"
    dropin.write_text(_bash('cambox_hk_dropin_content "$1"', "0,1").stdout)
    r = _run_prog(b, _apply_env(tmp_path, proc, dropin, tmp_path / "irq.conf"))
    assert r.returncode != 0
    assert "daemon-reload failed" in r.stderr


def test_a_box_without_a_reserved_core_still_gets_the_irq_dropin(tmp_path):
    b, log, proc = _stubs(tmp_path)
    irq = tmp_path / "irq.conf"
    r = _run_prog(b, _apply_env(tmp_path, proc, tmp_path / "absent.conf", irq,
                                sys_dir=_sys(tmp_path, isolated="")))
    assert r.returncode == 0, r.stdout + r.stderr
    assert irq.read_text() == _bash("cambox_hk_irq_dropin_content").stdout
    text = log.read_text()
    assert text.index("remount,ro") < text.index("systemctl daemon-reload")
    assert "taskset" not in text
    assert "irq drop-in write" in r.stdout


def test_the_plan_line_names_the_irq_dropin(tmp_path):
    r, _dropin, _log = _apply(tmp_path)
    assert "irq drop-in in-place" in r.stdout, r.stdout
