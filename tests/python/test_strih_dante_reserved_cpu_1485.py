"""Issue 1485: nothing but the strih OBS's two dstack threads on the reserved Dante CPU.

The A/B on strih-snv (3.11.2026): the program-audio sampler on 0-15 (its 'E-core' placement found no
cpu_atom on the AMD box and dropped the affinity line) cost the dstack engine a rebootstrap in every
10-15 min window; moved live to 0-1 a 15.5-min window was clean. And every other process inherited
PID 1's 0-15. Covered here:
  * the sampler's CPU set (strih_program_audio_cpus): the E-cores minus the reserved CPU, else the
    background set (online minus the OBS mask), never every CPU, never empty;
  * the manager-level default (scripts/lib/strih-dante-reserved-cpu.sh): both drop-ins written by
    strih_dante_input_install on a Dante box, unchanged on a re-run, removed (ours only) elsewhere;
  * that affinity is not a bound: a narrowed mask can still be widened (the rx-pin helper's taskset
    onto the reserved CPU keeps working under the manager default);
  * verify item 9's `reserved-cpu` rows over a fake /proc: kernel threads + OBS's dstack threads
    allowed, everything else named.
Every bash run sources the lib under the caller's `set -euo pipefail`; values reach bash as arguments.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from strih_dante_rx_pin_fakes_1485 import OBS_PID, fake_proc, pgrep_stub, stub  # noqa: E402

REPO = HERE.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-dante-reserved-cpu.sh"
PA_LIB = REPO / "scripts" / "lib" / "strih-program-audio.sh"
DI_LIB = REPO / "scripts" / "lib" / "strih-dante-input.sh"
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"
BASE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
LEAKY = ("STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "STRIH_DANTE_RX_PIN_PROC", "STRIH_DANTE_RX_PIN_SYSFS",
         "OBS_BOX_ONLINE_CPUS", "STRIH_PROGRAM_AUDIO_ISOLATED_CONF", "STRIH_PROGRAM_AUDIO_SYSFS", "STRIH", "STREAM",
         "STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE")
SYSTEM_DROPIN = "etc/systemd/system.conf.d/50-strih-dante-reserved-cpu.conf"
USER_DROPIN = "etc/systemd/user.conf.d/50-strih-dante-reserved-cpu.conf"
PF_KTHREAD = 0x00200000


def _bash(lib, body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (lib, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _owner():
    u = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    g = subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip()
    return "%s:%s" % (u, g)


def _box(tmp, ecores=None, isolated="2-15", online="0-15", reserved="15"):
    """A fake strih box: sysfs (cpu_atom), the online list, the OBS mask and the rx-pin conf."""
    sysfs = tmp / "sys"
    (sysfs / "devices").mkdir(parents=True, exist_ok=True)
    if ecores is not None:
        (sysfs / "devices" / "cpu_atom").mkdir(exist_ok=True)
        (sysfs / "devices" / "cpu_atom" / "cpus").write_text(ecores + "\n")
    root = tmp / "root"
    (root / "etc").mkdir(parents=True, exist_ok=True)
    (tmp / "online").write_text(online + "\n")
    iso = tmp / "isolated.conf"
    if isolated is not None:
        iso.write_text(isolated + "\n")
    if reserved is not None:
        (root / "etc" / "strih-dante-rx-cpu.conf").write_text("reserved=%s\nobs=2-14\nbase=2-15\n" % reserved)
    return {"STRIH_PROGRAM_AUDIO_SYSFS": str(sysfs), "OBS_BOX_ONLINE_CPUS": str(tmp / "online"),
            "STRIH_PROGRAM_AUDIO_ISOLATED_CONF": str(iso), "STRIH_DANTE_RX_PIN_ROOT": str(root),
            "STRIH_DANTE_RX_PIN_OWNER": _owner()}


# --- the sampler's CPU set ---------------------------------------------------------------------------

def _cpus(tmp, **kw):
    return _bash(PA_LIB, "strih_program_audio_cpus", env=_box(tmp, **kw))


def test_no_cpu_atom_takes_the_background_set_never_every_cpu(tmp_path):
    # strih-snv as read live 3.11.2026: AMD, no cpu_atom, OBS mask 2-15, Dante CPU 15
    r = _cpus(tmp_path)
    assert r.returncode == 0 and r.stdout == "0-1", r.stdout + r.stderr


def test_a_hybrid_box_keeps_its_e_cores_without_the_reserved_cpu(tmp_path):
    assert _cpus(tmp_path / "a", ecores="12-15", isolated="2-11", reserved=None).stdout == "12-15"
    assert _cpus(tmp_path / "b", ecores="12-15", isolated="2-11", reserved="15").stdout == "12-14"
    # E-cores that are only the reserved CPU leave nothing: the background set instead
    assert _cpus(tmp_path / "c", ecores="15", isolated="2-14", reserved="15").stdout == "0-1"
    # a garbage cpu_atom is no E-core set
    assert _cpus(tmp_path / "d", ecores="abc", reserved="15").stdout == "0-1"


def test_no_derivable_set_refuses_by_name(tmp_path):
    for name, kw in (("nomask", dict(isolated=None)), ("allobs", dict(isolated="0-15")),
                     ("garbage", dict(isolated="x")), ("online", dict(online=""))):
        r = _cpus(tmp_path / name, **kw)
        assert r.returncode == 1 and r.stdout == "", (name, r.stdout)
        assert "no CPU set for the sampler" in r.stderr, (name, r.stderr)


def test_the_sampler_unit_never_renders_without_an_affinity(tmp_path):
    for bad in ("", "abc", "1-", ",3"):
        r = _bash(PA_LIB, 'strih_program_audio_unit_text "$1" "$2"', REPO, bad)
        assert r.returncode == 1 and r.stdout == "" and "not a CPU list" in r.stderr, bad
    r = _bash(PA_LIB, 'strih_program_audio_unit_text "$1" "$2"', REPO, "0-1")
    assert r.returncode == 0 and "\nCPUAffinity=0-1\n" in r.stdout


def test_the_grade_reads_a_box_with_no_cpu_set_as_a_fail(tmp_path):
    env = _box(tmp_path, isolated=None)
    home = tmp_path / "home"
    home.mkdir()
    env.update({"STRIH_PROGRAM_AUDIO_PREFIX": str(tmp_path / "prefix"), "HOME": str(home),
                "STRIH_PROGRAM_AUDIO_RETRY_SLEEP": "0", "STRIH_PROGRAM_AUDIO_PYTHON": sys.executable})
    b = tmp_path / "bin"
    b.mkdir()
    stub(b, "systemctl", "echo inactive\nexit 3\n")
    env["PATH"] = "%s:%s" % (b, os.environ["PATH"])
    r = _bash(PA_LIB, 'strih_program_audio_grade_rows "$1" "$2" "$3"', REPO, home, "nobody", env=env)
    assert r.returncode == 0, r.stderr
    assert "FAIL|(program-audio-files) unit=no-cpu-set" in r.stdout, r.stdout


# --- the manager-level default -----------------------------------------------------------------------

def test_the_manager_drop_in_text():
    r = _bash(LIB, 'strih_dante_reserved_cpu_manager_text 0-14 15')
    assert r.returncode == 0
    lines = r.stdout.splitlines()
    assert lines[0].startswith("# issue 1485: written by camera-box setup-strih step 13")
    assert lines[-2:] == ["[Manager]", "CPUAffinity=0-14"]


def test_minus_is_pure_and_never_empty():
    for a, b, want in (("0-15", "15", "0-14"), ("0-15", "7", "0-6,8-15"), ("0-1", "", "0-1"), ("2,3", "9", "2-3")):
        r = _bash(LIB, 'strih_dante_reserved_cpu_minus "$1" "$2"', a, b)
        assert r.returncode == 0 and r.stdout == want + "\n", (a, b, r.stdout)
    for a, b in (("15", "15"), ("", "15"), ("x", "1")):
        r = _bash(LIB, 'strih_dante_reserved_cpu_minus "$1" "$2"', a, b)
        assert r.returncode == 1 and r.stdout == "", (a, b)


def _install(tmp, box="strih-snv"):
    env = _box(tmp)
    site = tmp / "site.conf"
    site.write_text("RX1=prog-L|master-asio-dante|1\nRX2=prog-R|master-asio-dante|2\n")
    (Path(env["STRIH_DANTE_RX_PIN_ROOT"]) / "etc" / "strih-isolated-cpus.conf").write_text("2-15\n")
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    stub(b, "visudo", "exit 0\n")
    env.update({"STRIH_DANTE_INPUT_TOML": str(tmp / "home" / ".camera-box" / "dante-input.toml"),
                "STRIH_DANTE_INPUT_IFACE": "enp7s0.70", "STRIH_DANTE_INPUT_SITE_FILE": str(site),
                "PATH": "%s:%s" % (b, os.environ["PATH"])})
    user = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    body = ('. "%s"; fail() { echo "FAIL $*" >&2; exit 1; }; strih_box_load "$3" >/dev/null; '
            'strih_dante_input_install "$1" "$2"') % BASE
    return Path(env["STRIH_DANTE_RX_PIN_ROOT"]), _bash(PROVISION, body, user, tmp / "home", box, env=env)


def test_a_dante_box_writes_both_manager_defaults_off_the_reserved_cpu(tmp_path):
    root, r = _install(tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    want = _bash(LIB, "strih_dante_reserved_cpu_manager_text 0-14 15").stdout
    for rel in (SYSTEM_DROPIN, USER_DROPIN):
        f = root / rel
        assert f.read_text() == want, rel
        assert oct(f.stat().st_mode & 0o777) == "0o644"
    # OBS's own launch mask (obs= of the rx-pin conf) is a subset of the manager default
    conf = (root / "etc" / "strih-dante-rx-cpu.conf").read_text()
    obs = [l.split("=", 1)[1] for l in conf.splitlines() if l.startswith("obs=")][0]
    assert obs == "2-14"
    assert "systemd + user managers default to CPUs 0-14 (CPU 15 only for the two dstack threads)" in r.stdout
    assert "at the latest the next boot; running processes keep their masks until they restart" in r.stdout
    # re-run: nothing rewritten, and setup never re-executes a manager or moves a live process
    _, r = _install(tmp_path)
    assert r.returncode == 0 and r.stdout.count("dante-reserved-cpu: ") >= 3
    assert "dante-reserved-cpu: %s unchanged" % (root / SYSTEM_DROPIN) in r.stdout
    assert "dante-reserved-cpu: %s unchanged" % (root / USER_DROPIN) in r.stdout
    s = LIB.read_text()
    assert "daemon-reexec" not in s.replace("never daemon-reexecs", "").replace("Never a daemon-reexec", "")
    assert "taskset -a -p" not in s.split("strih_dante_reserved_cpu_grade() {")[0]


def test_another_box_removes_only_our_drop_ins(tmp_path):
    root, r = _install(tmp_path)
    assert r.returncode == 0, r.stderr
    _, r = _install(tmp_path, box="strih-pp")
    assert r.returncode == 0, r.stderr
    assert not (root / SYSTEM_DROPIN).exists() and not (root / USER_DROPIN).exists()
    assert r.stdout.count("dante-reserved-cpu: removed") == 2
    # a file of the same name without our header is left and named
    (root / USER_DROPIN).write_text("[Manager]\nCPUAffinity=0-3\n")
    _, r = _install(tmp_path, box="strih-pp")
    assert (root / USER_DROPIN).exists() and "not ours" in r.stderr
    # nothing there: silent
    (root / USER_DROPIN).unlink()
    _, r = _install(tmp_path, box="strih-pp")
    assert "dante-reserved-cpu" not in r.stdout + r.stderr


def test_a_narrowed_mask_can_still_be_widened():
    """A kernel fact the design rests on, pinned on the runner (not a regression test of the lib: it passes
    with or without this change). The manager default is affinity, not a cpuset: sched_setaffinity is
    bounded by the cgroup cpuset and the online CPUs only, never by the mask a process inherited. So the
    rx-pin helper's taskset of the two dstack threads onto the reserved CPU still succeeds under a manager
    default that excludes it."""
    allowed = sorted(os.sched_getaffinity(0))
    assert len(allowed) >= 2, "needs two CPUs to prove it (this runner has %s)" % allowed
    code = ("import os, sys\n"
            "a = sorted(os.sched_getaffinity(0))\n"
            "os.sched_setaffinity(0, {a[0]})\n"            # the manager default: off the last CPU
            "assert os.sched_getaffinity(0) == {a[0]}\n"
            "os.sched_setaffinity(0, {a[-1]})\n"           # the helper: onto the reserved one
            "print(sorted(os.sched_getaffinity(0)))\n")
    r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "[%d]" % allowed[-1]


# --- verify item 9's reserved-cpu rows --------------------------------------------------------------

def _task(proc, pid, tid, comm, cpus, flags=0, stat=True):
    t = proc / str(pid) / "task" / str(tid)
    t.mkdir(parents=True, exist_ok=True)
    (t / "status").write_text("Name:\t%s\nCpus_allowed_list:\t%s\n" % (comm, cpus))
    if stat:
        fields = ["S", "1", "1", "1", "0", "-1", str(flags)] + ["0"] * 45
        (t / "stat").write_text("%d (%s) %s\n" % (tid, comm, " ".join(fields)))


GRADE = 'ok() { echo "OK $*"; }; note() { echo "NOTE $*"; }; bad() { echo "BAD $*"; }; strih_dante_reserved_cpu_grade'


def _grade(tmp, extra=(), dropins=True, obs_pid=OBS_PID, boot_before_dropins=None):
    """boot_before_dropins: None = no readable boot time (the fake /proc has no `stat`); True = the box
    booted before the drop-ins were written (a deploy before its reboot); False = booted after them."""
    env = _box(tmp)
    root = Path(env["STRIH_DANTE_RX_PIN_ROOT"])
    if dropins:
        text = _bash(LIB, "strih_dante_reserved_cpu_manager_text 0-14 15").stdout
        for rel in (SYSTEM_DROPIN, USER_DROPIN):
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(text)
            os.utime(root / rel, (1_700_000_000, 1_700_000_000))
    proc = fake_proc(tmp, threads={4242: ("obs", "2-14", 0), 4300: ("dstack-engine", "15", -10),
                                   4301: ("dstack-rx", "15", -10), 4302: ("ndi:io", "2-14", 0)})
    if boot_before_dropins is not None:
        btime = 1_700_000_000 + (-3600 if boot_before_dropins else 3600)
        (proc / "stat").write_text("cpu  1 2 3 4\nintr 5\nbtime %d\nprocesses 9\n" % btime)
    _task(proc, 1, 1, "systemd", "0-14")
    _task(proc, 2, 2, "kthreadd", "0-15", PF_KTHREAD)
    _task(proc, 40, 40, "ksoftirqd/15", "15", PF_KTHREAD)
    for e in extra:
        pid, comm, cpus = e[:3]
        _task(proc, pid, pid, comm, cpus, **(e[3] if len(e) > 3 else {}))
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    pgrep_stub(b, obs_pid)
    env.update({"STRIH_DANTE_RX_PIN_PROC": str(proc), "PATH": "%s:%s" % (b, os.environ["PATH"])})
    return _bash(LIB, GRADE, env=env)


def test_grade_passes_a_box_where_only_the_dstack_threads_reach_the_reserved_cpu(tmp_path):
    r = _grade(tmp_path)
    assert r.returncode == 0, r.stderr
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) reserved-cpu: systemd + user managers default to CPUAffinity=0-14" in r.stdout
    assert "OK (program-dante) reserved-cpu: of 7 tasks only OBS's dstack-engine + dstack-rx (and kernel threads) may run on CPU 15" in r.stdout


def test_grade_names_every_process_allowed_on_the_reserved_cpu(tmp_path):
    r = _grade(tmp_path, extra=[(900, "program_audio_s", "0-15"), (901, "bkshading", "0-15"),
                                (902, "bkshading", "15"), (903, "dsync", "0-14"), (904, "Xorg", "8-15")])
    assert "BAD (program-dante) reserved-cpu: 4 of 12 tasks may run on the reserved CPU 15: " \
           "program_audio_s x1, bkshading x2, Xorg x1" in r.stdout, r.stdout
    assert "dsync" not in r.stdout


def test_grade_allows_the_dstack_names_only_inside_obs(tmp_path):
    # the hub's own dstack threads (another process) are not the reserved CPU's owners
    r = _grade(tmp_path, extra=[(950, "dstack-engine", "0-15")])
    assert "BAD (program-dante) reserved-cpu: 1 of 8 tasks may run on the reserved CPU 15: dstack-engine x1" in r.stdout
    # no running OBS: its dstack threads are offenders too (nothing is exempt)
    r = _grade(tmp_path / "noobs", obs_pid=0)
    assert "BAD (program-dante) reserved-cpu: 2 of 7 tasks" in r.stdout, r.stdout


def test_grade_bounds_the_named_offenders(tmp_path):
    extra = [(1000 + i, "app%02d" % i, "0-15") for i in range(12)]
    r = _grade(tmp_path, extra=extra)
    line = [l for l in r.stdout.splitlines() if "tasks may run on the reserved CPU" in l][0]
    assert "app07 x1 (+4 more)" in line and "app08" not in line, line


def test_grade_fails_missing_drop_ins_no_conf_and_an_unreadable_proc(tmp_path):
    r = _grade(tmp_path / "a", dropins=False)
    assert "BAD (program-dante) reserved-cpu: missing or different:" in r.stdout
    r = _bash(LIB, GRADE, env=_box(tmp_path / "b", reserved=None))
    assert "BAD (program-dante) reserved-cpu: dante-reserved-cpu: no reserved CPU" in r.stdout, r.stdout
    env = _box(tmp_path / "c")
    env["STRIH_DANTE_RX_PIN_PROC"] = str(tmp_path / "c" / "nothing")
    r = _bash(LIB, GRADE, env=env)
    assert "BAD (program-dante) reserved-cpu: no task readable under" in r.stdout, r.stdout


def test_offenders_before_the_reboot_are_a_note_and_after_it_a_fail(tmp_path):
    """A deploy writes the drop-ins while every running process still has 0-15: until the box reboots the
    scan is a named NOTE (verify, and the genlock deploy's acceptance on it, must not fail on it); a boot
    after the drop-ins makes the same offenders a FAIL. Missing drop-ins are never pending."""
    extra = [(901, "bkshading", "0-15")]
    r = _grade(tmp_path / "pending", extra=extra, boot_before_dropins=True)
    assert "NOTE (program-dante) reserved-cpu: reboot pending -- the manager default was written after this " \
           "boot; until then 1 of 8 tasks may run on the reserved CPU 15: bkshading x1" in r.stdout, r.stdout
    assert "BAD" not in r.stdout, r.stdout
    r = _grade(tmp_path / "booted", extra=extra, boot_before_dropins=False)
    assert "BAD (program-dante) reserved-cpu: 1 of 8 tasks may run on the reserved CPU 15: bkshading x1" in r.stdout
    r = _grade(tmp_path / "nofiles", extra=extra, dropins=False, boot_before_dropins=True)
    assert "BAD (program-dante) reserved-cpu: 1 of 8 tasks" in r.stdout and "NOTE" not in r.stdout, r.stdout


def test_a_task_that_exited_mid_scan_is_never_an_offender(tmp_path):
    # its status was read, its stat is gone (the task exited): neither named nor counted as a kernel thread
    r = _grade(tmp_path, extra=[(960, "short-lived", "0-15", {"stat": False})])
    assert "BAD" not in r.stdout and "short-lived" not in r.stdout, r.stdout


def test_hostile_process_names_are_only_text(tmp_path):
    """Any local user sets a process name and verify runs as root: a name is counted by string length,
    never read inside an arithmetic subscript."""
    marker = tmp_path / "INJECTED"
    names = ["$(touch %s)" % marker, "a]b[0", "`touch %s`" % marker, "x[$(id)]"]
    extra = [(970 + i, n, "0-15") for i, n in enumerate(names)] + [(980, names[1], "0-15")]
    r = _grade(tmp_path, extra=extra)
    assert r.returncode == 0, r.stderr
    assert not marker.exists()
    assert "a]b[0 x2" in r.stdout, r.stdout
    assert "5 of 12 tasks may run on the reserved CPU 15" in r.stdout


def test_the_fork_free_cpulist_check_agrees_with_the_expander():
    """_strih_dante_cpulist_has (no fork per task) against obs_box_cpulist_expand over the same vectors; an
    unparsable list holds every CPU (fail closed), where the expander refuses it."""
    vectors = ["5", "0-15", "2,4-6", "0-1,12-15", "15", "0", "3-3", "08-09"]
    for v in vectors:
        exp = _bash(LIB, 'obs_box_cpulist_expand "$1"', v)
        assert exp.returncode == 0, v
        want = set(int(x) for x in exp.stdout.split())
        for cpu in range(0, 17):
            r = _bash(LIB, '_strih_dante_cpulist_has "$1" "$2" && echo yes || echo no', v, cpu)
            assert (r.stdout.strip() == "yes") == (cpu in want), (v, cpu, r.stdout)
    for v in ("x", "1-", "-3", "1-2-3", ""):
        assert _bash(LIB, 'obs_box_cpulist_expand "$1"', v).returncode == 1, v
        r = _bash(LIB, '_strih_dante_cpulist_has "$1" 15 && echo yes || echo no', v)
        assert r.stdout.strip() == ("no" if v == "" else "yes"), (v, r.stdout)


def test_grade_reads_the_live_proc_without_a_fork_per_task():
    """The one test that reads the HOST's real /proc (read-only; dev1 / the runner): the scan finishes
    well under the verify budget. Everything else runs on a fake /proc."""
    import time
    t0 = time.monotonic()
    r = _bash(LIB, 'strih_dante_reserved_cpu_offenders /proc 0 ""')
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines()[-1].startswith("scanned ")
    assert time.monotonic() - t0 < 20


def test_item9_runs_the_reserved_cpu_rows_and_step13_installs_and_removes_them():
    s = DI_LIB.read_text()
    body = s[s.index("strih_dante_input_grade() {"):s.index("strih_program_audio_declared()")]
    assert "  strih_dante_reserved_cpu_grade\n  strih_dante_rx_pin_grade\n  return 0\n}" in body
    inst = s[s.index("strih_dante_input_install() {"):s.index("strih_dante_input_field() {")]
    assert inst.index("strih_dante_rx_pin_remove") < inst.index("strih_dante_reserved_cpu_remove")
    assert inst.index('strih_dante_rx_pin_install "$user"') < inst.index("strih_dante_reserved_cpu_install")
    for f in ("setup-strih.sh", "verify-strih.sh"):
        assert "reserved_cpu" not in (REPO / "scripts" / f).read_text(), "the logic lives in the libs"
    # the helper's per-thread taskset is left as it is: affinity onto the reserved CPU, never a cpuset
    helper = (REPO / "scripts" / "lib" / "strih-dante-rx-pin.sh").read_text()
    assert 'taskset -p -c "$CPU" "$tid"' in helper
    assert "AllowedCPUs" not in LIB.read_text()
