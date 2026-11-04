"""Issue 1485: the strih OBS's Dante receive threads on their own CPU at nice -10.

The dante_input source's dstack-engine (wakes every 0.33 ms) and dstack-rx threads ran SCHED_OTHER
nice 0 among ~10 runnable OBS threads per CPU, finished blocks up to 6.7 ms late against a 5 ms margin
and skipped audio. The obs project's ask: one CPU out of OBS's mask, the two threads there at nice -10,
no realtime priority, no buffer change. Covered here: the derived CPU (pure), the install / removal
(moved paths, a visudo stub), the root helper (a fake /proc, fake taskset / renice / pgrep / id on its
PATH), verify item 9's rx-pin rows, and the launcher's mask step run for real. Everything runs under
`set -euo pipefail` like its callers.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from strih_dante_rx_pin_fakes_1485 import OBS_PID, fake_proc, pgrep_stub, read_thread, stub  # noqa: E402

REPO = HERE.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-dante-rx-pin.sh"
BASE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"
LAUNCHER = REPO / "scripts" / "strih-obs-start.sh"
LEAKY = ("STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "STRIH_DANTE_RX_PIN_PROC", "STRIH_DANTE_RX_PIN_SYSFS",
         "OBS_BOX_ONLINE_CPUS", "STRIH_DANTE_RX_PIN_CONF", "STRIH_ISOLATED_CPUS")


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (LIB, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _owner():
    u = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    g = subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip()
    return "%s:%s" % (u, g)


# --- the derived CPU ----------------------------------------------------------------------------------

def test_the_reserved_cpu_is_the_highest_online_obs_cpu():
    # strih-snv: 16 CPUs online, OBS 2-15 (obs_box_cpu_affinity) -> CPU 15, OBS 2-14
    assert _bash('strih_dante_rx_pin_plan 0-15 2-15').stdout == "15 2-14\n"
    # the comma form the plan persists, and an OBS CPU that is offline is never chosen
    assert _bash('strih_dante_rx_pin_plan 0-13 2,3,4,5,6,7,8,9,10,11,12,13,14,15').stdout == "13 2-12,14-15\n"
    # a box whose OBS block ends lower (imag-like 2-11 beside E-cores 12-15): CPU 11, never an E-core
    assert _bash('strih_dante_rx_pin_plan 0-15 2-11').stdout == "11 2-10\n"


def test_the_plan_refuses_rather_than_guess():
    for online, obs in (("0-15", "2-3"), ("0-15", ""), ("", "2-15"), ("0-1", "2-15"), ("0-15", "x")):
        r = _bash('strih_dante_rx_pin_plan "$1" "$2"', online, obs)
        assert r.returncode == 1 and r.stdout == "", (online, obs, r.stdout)


# --- install / removal -------------------------------------------------------------------------------

def _install_env(tmp, base="2-15", online="0-15", visudo_rc=0):
    root = tmp / "root"
    (root / "etc").mkdir(parents=True, exist_ok=True)
    if base is not None:
        (root / "etc" / "strih-isolated-cpus.conf").write_text(base + "\n")
    (tmp / "online").write_text(online + "\n")
    sysfs = tmp / "sys"
    (sysfs / "cpu15" / "topology").mkdir(parents=True, exist_ok=True)
    (sysfs / "cpu15" / "topology" / "thread_siblings_list").write_text("14-15\n")
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    stub(b, "visudo", 'cp "$2" "%s"\nexit %d\n' % (tmp / "visudo.seen", visudo_rc))
    return root, {"PATH": "%s:%s" % (b, os.environ["PATH"]), "STRIH_DANTE_RX_PIN_ROOT": str(root),
                  "STRIH_DANTE_RX_PIN_OWNER": _owner(), "OBS_BOX_ONLINE_CPUS": str(tmp / "online"),
                  "STRIH_DANTE_RX_PIN_SYSFS": str(sysfs)}


INSTALL = '. "%s"; fail() { echo "FAIL $*" >&2; exit 9; }; strih_dante_rx_pin_install "$1"' % BASE


def test_install_writes_the_conf_helper_and_one_no_argument_sudoers_line(tmp_path):
    root, env = _install_env(tmp_path)
    r = _bash(INSTALL, "newlevel", env=env)
    assert r.returncode == 0, r.stderr
    conf = (root / "etc" / "strih-dante-rx-cpu.conf").read_text()
    assert "reserved=15\nobs=2-14\nbase=2-15\nsmt_siblings=14-15\n" in conf
    helper = root / "usr" / "local" / "sbin" / "strih-dante-rx-pin"
    assert os.access(helper, os.X_OK)
    assert helper.read_text() == _bash("strih_dante_rx_pin_helper_text").stdout
    sudo = (root / "etc" / "sudoers.d" / "strih-dante-rx-pin")
    assert oct(sudo.stat().st_mode & 0o777) == "0o440"
    lines = [l for l in sudo.read_text().splitlines() if l and not l.startswith("#")]
    assert lines == ['newlevel ALL=(root) NOPASSWD: /usr/local/sbin/strih-dante-rx-pin ""'], "no arguments allowed"
    assert (tmp_path / "visudo.seen").read_text() == sudo.read_text(), "the written line is the checked one"
    assert "CPU 15 reserved for dstack-engine + dstack-rx at nice -10; OBS on 2-14 (from 2-15); SMT siblings of CPU 15: 14-15" in r.stdout
    # re-run: nothing rewritten
    r = _bash(INSTALL, "newlevel", env=env)
    assert r.returncode == 0 and "written" not in r.stdout and r.stdout.count("unchanged") == 3, r.stdout


def test_install_refuses_a_bad_sudoers_line_and_a_missing_mask(tmp_path):
    root, env = _install_env(tmp_path, visudo_rc=1)
    r = _bash(INSTALL, "newlevel", env=env)
    assert r.returncode == 1 and "does not parse" in r.stderr
    assert not (root / "etc" / "sudoers.d").exists() or not any((root / "etc" / "sudoers.d").iterdir())
    root, env = _install_env(tmp_path / "b", base=None)
    r = _bash(INSTALL, "newlevel", env=env)
    assert r.returncode == 1 and "no CPU to reserve" in r.stderr, r.stderr


def test_removal_is_silent_when_nothing_is_there_and_removes_only_ours(tmp_path):
    root, env = _install_env(tmp_path)
    r = _bash("strih_dante_rx_pin_remove", env=env)
    assert r.returncode == 0 and r.stdout == "", "a box that never had it stays byte-identical and quiet"
    assert _bash(INSTALL, "newlevel", env=env).returncode == 0
    other = root / "etc" / "sudoers.d" / "someone-else"
    other.write_text("x\n")
    r = _bash("strih_dante_rx_pin_remove", env=env)
    assert r.returncode == 0 and r.stdout.count("removed") == 3
    assert not (root / "etc" / "strih-dante-rx-cpu.conf").exists()
    assert not (root / "usr" / "local" / "sbin" / "strih-dante-rx-pin").exists()
    assert other.exists()


# --- the root helper ----------------------------------------------------------------------------------

def _helper(tmp, proc, budget=3, pgrep_pid=OBS_PID, sleep_body=""):
    """The real helper with its PATH / PROC / BUDGET_S lines moved to the test's fakes."""
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    log = tmp / "calls.log"
    stub(b, "id", "echo 0\n")
    stub(b, "logger", 'echo "logger $*" >> "%s"\n' % log)
    stub(b, "sleep", 'echo "sleep $*" >> "%s"\n%s' % (log, sleep_body))
    pgrep_stub(b, pgrep_pid)
    # taskset -p -c CPU TID / renice --priority N -p TID write what the kernel would show.
    stub(b, "taskset", 'echo "taskset $*" >> "%s"\nt=$(ls -d "%s"/*/task/"$4")\n'
                       'sed -i "s/^Cpus_allowed_list:.*/Cpus_allowed_list:\\t$3/" "$t/status"\n' % (log, proc))
    stub(b, "renice", 'echo "renice $*" >> "%s"\nt=$(ls -d "%s"/*/task/"$4")\n'
                      'python3 -c "import sys;p=sys.argv[1];h,r=open(p).read().rsplit(\') \',1);f=r.split();'
                      'f[16]=sys.argv[2];f[15]=str(20+int(sys.argv[2]));open(p,\'w\').write(h+\') \'+\' \'.join(f)+chr(10))" '
                      '"$t/stat" "$2"\n' % (log, proc))
    text = _bash("strih_dante_rx_pin_helper_text").stdout
    for old, new in (("\nPATH=/usr/sbin:/usr/bin:/sbin:/bin\n", "\nPATH=%s:/usr/bin:/bin\n" % b),
                     ("\nPROC=/proc\n", "\nPROC=%s\n" % proc), ("\nBUDGET_S=120\n", "\nBUDGET_S=%d\n" % budget),
                     ("\nCONF=/etc/strih-dante-rx-cpu.conf\n", "\nCONF=%s\n" % (tmp / "rx.conf"))):
        assert text.count(old) == 1, old
        text = text.replace(old, new)
    (tmp / "rx.conf").write_text("reserved=15\nobs=2-14\nbase=2-15\n")
    h = tmp / "strih-dante-rx-pin"
    h.write_text(text)
    h.chmod(0o755)
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"SUDO_UID": "1000", "PGREP_LOG": str(tmp / "pgrep.log")})
    return h, env, log


def _run(h, env, *args):
    return subprocess.run([str(h), *args], capture_output=True, text=True, env=env, timeout=60)


def test_helper_pins_only_the_two_threads_and_logs_one_line(tmp_path):
    proc = fake_proc(tmp_path)
    h, env, log = _helper(tmp_path, proc)
    r = _run(h, env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert read_thread(proc, 4300) == ("15", -10) and read_thread(proc, 4301) == ("15", -10)
    assert read_thread(proc, 4242) == ("2-14", 0) and read_thread(proc, 4302) == ("2-14", 0), "no other thread"
    want = "obs pid %d: dstack-engine tid 4300, dstack-rx tid 4301 -> CPU 15 nice -10" % OBS_PID
    assert r.stdout == "strih-dante-rx-pin: %s\n" % want
    assert ("logger -t strih-dante-rx-pin -- %s" % want) in log.read_text()
    calls = log.read_text()
    moves = [l for l in calls.splitlines() if l.startswith(("taskset", "renice"))]
    assert sorted(l.split()[-1] for l in moves) == ["4300", "4300", "4301", "4301"], moves
    assert "renice --priority -10 -p 4300" in calls, "absolute nice, never relative"
    assert "chrt" not in calls and "SCHED_FIFO" not in h.read_text() and "chrt" not in h.read_text()
    assert (tmp_path / "pgrep.log").read_text().startswith("-n -x -u 1000 obs"), "only the caller's own OBS"
    # idempotent: a second run lands on the same state
    r = _run(h, env)
    assert r.returncode == 0 and read_thread(proc, 4300) == ("15", -10)


def test_helper_waits_for_threads_that_start_late(tmp_path):
    proc = fake_proc(tmp_path, threads={4242: ("obs", "2-14", 0)})
    # the module loads during the second wait: the two threads appear then
    late = ('if [ "$(grep -c "^sleep" "%s")" = 2 ]; then python3 -c "import sys; sys.path.insert(0, \'%s\'); '
            'from strih_dante_rx_pin_fakes_1485 import write_thread as w; '
            'w(\'%s\', 4300, \'dstack-engine\', \'2-14\', 0); w(\'%s\', 4301, \'dstack-rx\', \'2-14\', 0)"; fi\n'
            % (tmp_path / "calls.log", HERE, proc, proc))
    h, env, log = _helper(tmp_path, proc, budget=5, sleep_body=late)
    r = _run(h, env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert log.read_text().count("sleep 1") == 2
    assert read_thread(proc, 4300) == ("15", -10) and read_thread(proc, 4301) == ("15", -10)


def test_helper_gives_up_after_its_budget_naming_the_threads(tmp_path):
    proc = fake_proc(tmp_path, threads={4242: ("obs", "2-14", 0), 4300: ("dstack-engine", "2-14", 0)})
    h, env, log = _helper(tmp_path, proc, budget=3)
    r = _run(h, env)
    assert r.returncode == 1
    assert "FAIL: after 3 s the dstack-engine + dstack-rx threads of obs (pid %d)" % OBS_PID in r.stdout
    assert log.read_text().count("sleep 1") == 3, "bounded: budget sleeps, never more"
    # no OBS at all
    (tmp_path / "n").mkdir()
    h, env, log = _helper(tmp_path / "n", fake_proc(tmp_path / "n"), budget=2, pgrep_pid=0)
    r = _run(h, env)
    assert r.returncode == 1 and "(pid none)" in r.stdout


def test_helper_refuses_arguments_root_and_a_missing_conf(tmp_path):
    proc = fake_proc(tmp_path)
    h, env, log = _helper(tmp_path, proc)
    assert _run(h, env, "1234").returncode == 2
    for uid in ("0", "", "1x"):
        assert _run(h, dict(env, SUDO_UID=uid)).returncode == 2, uid
    (tmp_path / "rx.conf").write_text("obs=2-14\n")
    r = _run(h, env)
    assert r.returncode == 1 and "no reserved CPU" in r.stdout
    assert read_thread(proc, 4300) == ("2-14", 0)


# --- verify item 9's rx-pin rows ---------------------------------------------------------------------

def _grade(tmp, threads=None, main_cpus="2-14", conf="reserved=15\nobs=2-14\nbase=2-15\n", pid=OBS_PID):
    root = tmp / "root"
    (root / "etc").mkdir(parents=True, exist_ok=True)
    if conf is not None:
        (root / "etc" / "strih-dante-rx-cpu.conf").write_text(conf)
    if threads is None:
        threads = {4242: ("obs", main_cpus, 0), 4300: ("dstack-engine", "15", -10), 4301: ("dstack-rx", "15", -10),
                   4302: ("ndi:io", main_cpus, 0)}
    proc = fake_proc(tmp, main_cpus=main_cpus, threads=threads)
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    pgrep_stub(b, pid)
    body = 'ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; strih_dante_rx_pin_grade'
    return _bash(body, env={"PATH": "%s:%s" % (b, os.environ["PATH"]), "STRIH_DANTE_RX_PIN_ROOT": str(root),
                            "STRIH_DANTE_RX_PIN_PROC": str(proc)})


def test_grade_passes_the_pinned_threads(tmp_path):
    r = _grade(tmp_path)
    assert r.returncode == 0, r.stderr
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) rx-pin: OBS's main affinity 2-14 leaves CPU 15 to the Dante threads" in r.stdout
    assert "OK (program-dante) rx-pin: dstack-engine tid 4300 on CPU 15 at nice -10" in r.stdout
    assert "OK (program-dante) rx-pin: dstack-rx tid 4301 on CPU 15 at nice -10" in r.stdout


def test_grade_fails_each_fault_by_name(tmp_path):
    cases = {
        "wrong-cpu": ({4300: ("dstack-engine", "2-14", -10), 4301: ("dstack-rx", "15", -10)}, "2-14",
                      "dstack-engine tid 4300 runs on 2-14, not the reserved CPU 15"),
        "nice": ({4300: ("dstack-engine", "15", -10), 4301: ("dstack-rx", "15", -5)}, "2-14",
                 "dstack-rx tid 4301 has nice -5, wants -10 or lower"),
        "missing": ({4300: ("dstack-engine", "15", -10)}, "2-14", "no dstack-rx thread in the running OBS"),
        "main-mask": ({4300: ("dstack-engine", "15", -10), 4301: ("dstack-rx", "15", -10)}, "2-15",
                      "OBS's main affinity 2-15 still includes the reserved CPU 15"),
    }
    for name, (threads, main, want) in cases.items():
        r = _grade(tmp_path / name, threads={**threads, 4242: ("obs", main, 0)}, main_cpus=main)
        assert r.returncode == 0
        assert ("BAD (program-dante) rx-pin: " + want) in r.stdout, (name, r.stdout)
    r = _grade(tmp_path / "noconf", conf=None)
    assert "BAD (program-dante) rx-pin: no reserved CPU" in r.stdout
    r = _grade(tmp_path / "noobs", pid=0)
    assert "BAD (program-dante) rx-pin: no running OBS" in r.stdout
    # a more negative nice still passes
    r = _grade(tmp_path / "lower", threads={4242: ("obs", "2-14", 0), 4300: ("dstack-engine", "15", -15),
                                           4301: ("dstack-rx", "15", -10)})
    assert "BAD" not in r.stdout, r.stdout


def test_item9_dante_branch_runs_the_rx_pin_rows():
    s = (REPO / "scripts" / "lib" / "strih-dante-input.sh").read_text()
    body = s[s.index("strih_dante_input_grade() {"):s.index("strih_program_audio_declared()")]
    assert '  strih_dante_rx_pin_grade "$(_strih_dante_hub_flag)"\n  return 0\n}' in body
    inst = s[s.index("strih_dante_input_install() {"):s.index("strih_dante_input_field() {")]
    assert inst.index("strih_dante_rx_pin_remove") < inst.index("strih_dante_rx_pin_install \"$user\"")
    assert "rx_pin" not in (REPO / "scripts" / "verify-strih.sh").read_text(), "the rows live in the lib"


# --- the launcher's mask step, run for real ------------------------------------------------------------

def _mask_step(tmp, isolated, conf):
    s = LAUNCHER.read_text()
    a = s.index('ISOLATED_CPUS="${STRIH_ISOLATED_CPUS:-}"')
    b = s.index("# issue 1485 end of the mask step")
    step = s[a:b].replace("/etc/strih-isolated-cpus.conf", str(tmp / "iso.conf"))
    (tmp / "iso.conf").write_text(isolated + "\n")
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if conf is not None:
        (tmp / "rx.conf").write_text(conf)
    env["STRIH_DANTE_RX_PIN_CONF"] = str(tmp / "rx.conf")
    script = 'set -euo pipefail\n%s\necho "MASK=$ISOLATED_CPUS PIN=$RX_PIN"\n' % step
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=env, timeout=30)


def test_launcher_takes_the_reduced_mask_only_from_a_matching_conf(tmp_path):
    r = _mask_step(tmp_path, "2-15", "reserved=15\nobs=2-14\nbase=2-15\n")
    assert r.returncode == 0, r.stderr
    assert r.stdout.endswith("MASK=2-14 PIN=1\n") and "CPU 15 is the Dante threads'" in r.stdout
    (tmp_path / "a").mkdir()
    r = _mask_step(tmp_path / "a", "2-15", None)
    assert r.stdout == "MASK=2-15 PIN=0\n", "no conf (every other box): unchanged and silent"
    (tmp_path / "b").mkdir()
    r = _mask_step(tmp_path / "b", "2-13", "reserved=15\nobs=2-14\nbase=2-15\n")
    assert r.stdout.endswith("MASK=2-13 PIN=0\n") and "WARN issue 1485" in r.stdout, "a stale conf never narrows"


def test_launcher_runs_the_helper_through_sudo_in_the_background_after_launch():
    s = LAUNCHER.read_text()
    launch, pin, ws = s.index("OBS_PID=$!"), s.index('if [ "$RX_PIN" = 1 ]; then'), s.index("deadline=$((SECONDS + 90))")
    assert launch < pin < ws
    block = s[pin:ws]
    assert 'sudo -n "${STRIH_DANTE_RX_PIN_BIN:-/usr/local/sbin/strih-dante-rx-pin}"' in block
    assert block.rstrip().endswith(") &\nfi"), "backgrounded: never delays the WS wait"
    assert "|| echo \"WARN issue 1485" in block, "never fatal to the unit"


def test_nothing_reintroduces_a_realtime_grant():
    text = LIB.read_text()
    assert "/etc/security/limits" not in text, "no session-wide nice or rtprio grant"
    for word in ("chrt", "SCHED_FIFO", "SCHED_RR", "rtprio "):
        assert word not in _bash("strih_dante_rx_pin_helper_text").stdout, word
