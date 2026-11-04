"""Issue 1485 (comment 6634395906, 4.11.2026): each dstack engine on its OWN reserved CPU.

Live on strih-snv (hub v1.9.13 + obs v0.4.10 on dstack v0.6.25), pump WARNs per 3 min: the hub's
dstack-engine / dstack-rx unpinned on 0-14 = hub ~30; beside OBS's on CPU 15 = OBS 25 (two engines on
one CPU collide); on CPU 14 among 180 other threads = hub 24; alone on a cleared CPU 14 = 0 and 0.
So a box whose facts declare a hub Dante device beside the Dante program reserves TWO CPUs: the highest
= OBS's (15), the next = the hub's (14), OBS on 2-13, the managers on 0-13. Covered here:
  * the plan (pure), the conf's `hub=`, `strih_dante_reserved_cpus`;
  * the install / removal of the hub drop-in (ExecStartPost only, a reload on a change);
  * the root helper's `hub` mode (the unit's MainPID, only the two threads, nice -10, bounded);
  * the facts' decision, the JPEG XS cores, the managers' default and the sampler;
  * verify item 9's `(intercom-dante)` rx-pin + reserved-cpu rows (hub-mix on CPU 14 is an offender).
Every bash run sources the libs under the caller's `set -euo pipefail`; values reach bash as arguments.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_strih_dante_reserved_cpu_1485 as rc  # noqa: E402
import test_strih_dante_rx_pin_1485 as rp  # noqa: E402
from strih_dante_rx_pin_fakes_1485 import (HUB_PID, OBS_PID, fake_proc, hub_proc, pgrep_stub,  # noqa: E402
                                           read_thread, stub, systemctl_stub)

REPO = HERE.parent.parent
RX_LIB = REPO / "scripts" / "lib" / "strih-dante-rx-pin.sh"
RC_LIB = REPO / "scripts" / "lib" / "strih-dante-reserved-cpu.sh"
DI_LIB = REPO / "scripts" / "lib" / "strih-dante-input.sh"
DROPIN_REL = "etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf"
CONF2 = "reserved=15\nobs=2-13\nbase=2-15\nsmt_siblings=14-15\nhub=14\n"


# --- the plan -------------------------------------------------------------------------------------------

def test_the_hub_gets_the_next_cpu_down_and_obs_keeps_the_rest():
    assert rp._bash('strih_dante_rx_pin_plan 0-15 2-15 1').stdout == "15 2-13 14\n"
    # an offline OBS CPU is skipped for both, the comma form persists
    assert rp._bash('strih_dante_rx_pin_plan 0-13 2,3,4,5,6,7,8,9,10,11,12,13,14,15 1').stdout == "13 2-11,14-15 12\n"
    # HUB 0 (and no HUB) = the one-CPU plan, byte-identical
    assert rp._bash('strih_dante_rx_pin_plan 0-15 2-15 0').stdout == "15 2-14\n"
    assert rp._bash('strih_dante_rx_pin_plan 0-15 2-15').stdout == "15 2-14\n"


def test_the_two_cpu_plan_refuses_rather_than_guess():
    for online, obs, hub in (("0-15", "2-4", "1"), ("0-15", "15", "1"), ("0-15", "2-15", "2"), ("0-15", "2-15", "yes")):
        r = rp._bash('strih_dante_rx_pin_plan "$1" "$2" "$3"', online, obs, hub)
        assert r.returncode == 1 and r.stdout == "", (online, obs, hub, r.stdout)


def test_the_reserved_cpus_reader_lists_obs_then_the_hub(tmp_path):
    root = tmp_path / "root"
    (root / "etc").mkdir(parents=True)
    conf = root / "etc" / "strih-dante-rx-cpu.conf"
    env = {"STRIH_DANTE_RX_PIN_ROOT": str(root)}
    body = 'printf "[%s][%s][%s]\\n" "$(strih_dante_reserved_cpu)" "$(strih_dante_reserved_hub_cpu)" "$(strih_dante_reserved_cpus)"'
    assert rp._bash(body, env=env).stdout == "[][][]\n"
    conf.write_text("reserved=15\nobs=2-14\nbase=2-15\n")
    assert rp._bash(body, env=env).stdout == "[15][][15]\n"
    conf.write_text(CONF2)
    assert rp._bash(body, env=env).stdout == "[15][14][15 14]\n"
    conf.write_text("reserved=15\nhub=1x\n")
    assert rp._bash(body, env=env).stdout == "[15][][15]\n", "a non-number is no CPU"
    conf.write_text("hub=14\n")
    assert rp._bash(body, env=env).stdout == "[][14][]\n", "a hub CPU without OBS's lists nothing"


# --- install / removal -----------------------------------------------------------------------------------

def _install_env(tmp):
    root, env = rp._install_env(tmp)
    log = tmp / "systemctl.log"
    systemctl_stub(tmp / "bin", log=log)
    stub(tmp / "bin", "timeout", 'printf "timeout %%s\\n" "$*" >> "%s"\nshift\nexec "$@"\n' % log)
    return root, env, log


INSTALL = rp.INSTALL + ' "$2"'


def test_install_with_a_hub_cpu_writes_hub_and_the_drop_in_then_reloads_once(tmp_path):
    root, env, log = _install_env(tmp_path)
    r = rp._bash(INSTALL, "newlevel", "1", env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    conf = (root / "etc" / "strih-dante-rx-cpu.conf").read_text()
    assert conf.endswith("reserved=15\nobs=2-13\nbase=2-15\nsmt_siblings=14-15\nhub=14\n"), conf
    drop = root / DROPIN_REL
    assert drop.read_text() == rp._bash("strih_dante_rx_pin_hub_dropin_text").stdout
    assert oct(drop.stat().st_mode & 0o777) == "0o644"
    assert "CPU 14 reserved for the intercom hub's dstack-engine + dstack-rx at nice -10" in r.stdout
    assert log.read_text().splitlines() == ["timeout 30 systemctl daemon-reload", "daemon-reload"], log.read_text()
    # the sudoers line is still the no-argument form: the operator never runs the hub mode
    sudo = (root / "etc" / "sudoers.d" / "strih-dante-rx-pin").read_text()
    assert 'NOPASSWD: /usr/local/sbin/strih-dante-rx-pin ""' in sudo and " hub" not in sudo
    # re-run: nothing rewritten, no reload
    log.write_text("")
    r = rp._bash(INSTALL, "newlevel", "1", env=env)
    assert r.returncode == 0 and "written" not in r.stdout and log.read_text() == "", r.stdout


def test_install_without_a_hub_cpu_is_the_one_cpu_install_and_removes_our_drop_in(tmp_path):
    root, env, log = _install_env(tmp_path)
    assert rp._bash(INSTALL, "newlevel", "1", env=env).returncode == 0
    log.write_text("")
    r = rp._bash(INSTALL, "newlevel", "0", env=env)
    assert r.returncode == 0, r.stderr
    assert (root / "etc" / "strih-dante-rx-cpu.conf").read_text().endswith(
        "reserved=15\nobs=2-14\nbase=2-15\nsmt_siblings=14-15\n"), "no hub= line: byte-identical to before"
    assert not (root / DROPIN_REL).exists() and "removed" in r.stdout
    assert "daemon-reload" in log.read_text()
    # a file of that name without our header is left, named, and nothing reloads
    (root / DROPIN_REL).write_text("[Service]\nExecStartPost=/bin/true\n")
    log.write_text("")
    r = rp._bash(INSTALL, "newlevel", "0", env=env)
    assert r.returncode == 0 and (root / DROPIN_REL).exists() and "not ours" in r.stderr
    assert log.read_text() == ""
    # the box that takes no Dante program at all removes ours too
    (root / DROPIN_REL).unlink()
    assert rp._bash(INSTALL, "newlevel", "1", env=env).returncode == 0
    r = rp._bash("strih_dante_rx_pin_remove", env=env)
    assert r.returncode == 0 and not (root / DROPIN_REL).exists(), r.stdout + r.stderr


def test_a_failed_reload_fails_the_install_by_name(tmp_path):
    root, env, log = _install_env(tmp_path)
    stub(tmp_path / "bin", "systemctl", "exit 1\n")
    r = rp._bash(INSTALL, "newlevel", "1", env=env)
    assert r.returncode == 1 and "daemon-reload failed" in r.stderr, r.stdout + r.stderr


def test_the_drop_in_sets_only_exec_start_post():
    text = rp._bash("strih_dante_rx_pin_hub_dropin_text").stdout
    keys = [l for l in text.splitlines() if l and not l.startswith("#")]
    assert keys == ["[Service]", "ExecStartPost=-+/usr/local/sbin/strih-dante-rx-pin hub"], keys
    for key in ("AmbientCapabilities", "CapabilityBoundingSet", "SecureBits", "Nice", "CPUAffinity", "CPUScheduling"):
        assert key not in text, key


# --- the root helper's hub mode ---------------------------------------------------------------------------

def _hub_helper(tmp, threads=None, mainpid=HUB_PID, conf=CONF2, budget_hub=3, sleep_body=""):
    proc = fake_proc(tmp)                 # OBS 4242, untouched by the hub mode
    hub_proc(tmp, threads=threads)
    h, env, log = rp._helper(tmp, proc, sleep_body=sleep_body)
    systemctl_stub(tmp / "bin", mainpid, log)
    (tmp / "rx.conf").write_text(conf)
    text = h.read_text()
    assert text.count("\nHUB_BUDGET_S=30\n") == 1
    h.write_text(text.replace("\nHUB_BUDGET_S=30\n", "\nHUB_BUDGET_S=%d\n" % budget_hub))
    env.pop("SUDO_UID")                   # systemd runs it as root, no sudo caller
    return h, env, log, proc


def test_hub_mode_pins_only_the_hubs_two_threads(tmp_path):
    h, env, log, proc = _hub_helper(tmp_path)
    r = rp._run(h, env, "hub")
    assert r.returncode == 0, r.stdout + r.stderr
    assert read_thread(proc, 5160, HUB_PID) == ("14", -10) and read_thread(proc, 5161, HUB_PID) == ("14", -10)
    for tid in (5150, 5162, 5163):
        assert read_thread(proc, tid, HUB_PID) == ("0-13", 0), "hub-mix and every other hub thread stay"
    for tid in (4300, 4301):
        assert read_thread(proc, tid) == ("2-14", 0), "OBS is never touched by the hub mode"
    want = "intercom-hub pid %d: dstack-engine tid 5160, dstack-rx tid 5161 -> CPU 14 nice -10" % HUB_PID
    assert r.stdout == "strih-dante-rx-pin: %s\n" % want
    assert "show -p MainPID --value intercom-hub" in log.read_text()
    assert not (tmp_path / "pgrep.log").exists(), "the hub is its unit's MainPID, never a name search"


def test_hub_mode_waits_within_its_own_budget_and_names_the_hub(tmp_path):
    h, env, log, proc = _hub_helper(tmp_path, threads={5150: ("intercom-hub", "0-13", 0)}, budget_hub=2)
    r = rp._run(h, env, "hub")
    assert r.returncode == 1
    assert "FAIL: after 2 s the dstack-engine + dstack-rx threads of intercom-hub (pid %d)" % HUB_PID in r.stdout
    assert log.read_text().count("sleep 1") == 2, "the hub budget, never OBS's 120 s"
    (tmp_path / "n").mkdir()
    h, env, log, proc = _hub_helper(tmp_path / "n", mainpid=0, budget_hub=1)
    r = rp._run(h, env, "hub")
    assert r.returncode == 1 and "(pid none)" in r.stdout


def test_hub_mode_refuses_other_arguments_a_non_root_caller_and_no_hub_cpu(tmp_path):
    h, env, log, proc = _hub_helper(tmp_path)
    for args in (("hub", "1"), ("obs",), ("--hub",)):
        assert rp._run(h, env, *args).returncode == 2, args
    stub(tmp_path / "bin", "id", "echo 1000\n")
    assert rp._run(h, env, "hub").returncode == 2
    stub(tmp_path / "bin", "id", "echo 0\n")
    (tmp_path / "rx.conf").write_text("reserved=15\nobs=2-14\nbase=2-15\n")
    r = rp._run(h, env, "hub")
    assert r.returncode == 1 and "no hub CPU" in r.stdout
    assert read_thread(proc, 5160, HUB_PID) == ("0-13", 0)


def test_the_obs_mode_is_unchanged_by_a_hub_cpu(tmp_path):
    h, env, log, proc = _hub_helper(tmp_path)
    env["SUDO_UID"] = "1000"
    r = rp._run(h, env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert read_thread(proc, 4300) == ("15", -10) and read_thread(proc, 5160, HUB_PID) == ("0-13", 0)


# --- the facts decide ---------------------------------------------------------------------------------------

def test_the_hub_cpu_follows_the_facts(tmp_path):
    body = ('for b in strih-snv strih-pp stream-snv stream-pp; do (strih_box_load "$b" >/dev/null; '
            'echo "$b $(_strih_dante_hub_flag)"); done')
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "%s"\n%s' % (rp.PROVISION, body)], capture_output=True,
                       text=True, cwd=str(REPO), timeout=60,
                       env={k: v for k, v in os.environ.items() if k not in rc.LEAKY})
    assert r.stdout.splitlines() == ["strih-snv 1", "strih-pp 0", "stream-snv 0", "stream-pp 0"], r.stdout + r.stderr
    # a Dante-program box without the hub's device keeps ONE CPU (byte-identical)
    d = _boxes_without_hub_device(tmp_path)
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "%s"\nstrih_box_load strih-snv >/dev/null\n'
                        'strih_program_dante_declared && echo dante\n_strih_dante_hub_flag' % rp.PROVISION],
                       capture_output=True, text=True, cwd=str(REPO), timeout=60,
                       env=dict({k: v for k, v in os.environ.items() if k not in rc.LEAKY}, STRIH_BOXES_DIR=str(d)))
    assert r.stdout == "dante\n0", r.stdout + r.stderr


def _boxes_without_hub_device(tmp):
    d = tmp / "boxes"
    d.mkdir()
    for f in (REPO / "scripts" / "strih-boxes").glob("*.env"):
        text = f.read_text()
        if f.name == "strih-snv.env":
            lines = text.splitlines()
            for i, l in enumerate(lines):
                if l.startswith("STRIH_DANTE_DEVICES="):
                    lines[i] = "STRIH_DANTE_DEVICES=" + " ".join(
                        x for x in l.split("=", 1)[1].split() if not x.startswith("strih-snv-intercom="))
            text = "\n".join(lines) + "\n"
        (d / f.name).write_text(text)
    return d


def test_strih_snv_installs_two_cpus_end_to_end(tmp_path):
    root, r = rc._install(tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    conf = (root / "etc" / "strih-dante-rx-cpu.conf").read_text()
    assert "\nobs=2-13\n" in conf and conf.endswith("\nhub=14\n"), conf
    want = rc._bash(RC_LIB, "strih_dante_reserved_cpu_manager_text 0-13 15 14").stdout
    assert (root / rc.SYSTEM_DROPIN).read_text() == want and (root / rc.USER_DROPIN).read_text() == want
    assert "CPUAffinity=0-13" in want and "CPU 14 for the intercom hub" in want
    assert (root / DROPIN_REL).exists()
    assert "CPU 15 only for OBS's two dstack threads, CPU 14 only for the hub's" in r.stdout
    assert "daemon-reload" in (tmp_path / "systemctl.log").read_text()


def test_the_one_cpu_manager_text_is_byte_identical_to_before():
    one = rc._bash(RC_LIB, "strih_dante_reserved_cpu_manager_text 0-14 15").stdout
    assert one == rc._bash(RC_LIB, 'strih_dante_reserved_cpu_manager_text 0-14 15 ""').stdout
    assert "hub" not in one


def test_minus_takes_every_reserved_cpu():
    for a, b, want in (("0-15", "15 14", "0-13"), ("12-15", "15 14", "12-13"), ("0-1", "15 14", "0-1"), ("0-15", "", "0-15")):
        r = rc._bash(RC_LIB, 'strih_dante_reserved_cpu_minus "$1" "$2"', a, b)
        assert r.returncode == 0 and r.stdout == want + "\n", (a, b, r.stdout)
    assert rc._bash(RC_LIB, 'strih_dante_reserved_cpu_minus 14-15 "15 14"').returncode == 1


def test_the_sampler_stays_off_both_reserved_cpus(tmp_path):
    env = rc._box(tmp_path, ecores="12-15")
    (Path(env["STRIH_DANTE_RX_PIN_ROOT"]) / "etc" / "strih-dante-rx-cpu.conf").write_text(CONF2)
    r = rc._bash(rc.PA_LIB, 'strih_program_audio_cpus "$1"', env["STRIH_PROGRAM_AUDIO_SYSFS"], env=env)
    assert r.returncode == 0 and r.stdout == "12-13", r.stdout + r.stderr
    env = rc._box(tmp_path / "b")
    (Path(env["STRIH_DANTE_RX_PIN_ROOT"]) / "etc" / "strih-dante-rx-cpu.conf").write_text(CONF2)
    r = rc._bash(rc.PA_LIB, 'strih_program_audio_cpus "$1"', env["STRIH_PROGRAM_AUDIO_SYSFS"], env=env)
    assert r.returncode == 0 and r.stdout == "0-1", "strih-snv (no cpu_atom): the background set, unchanged"


# --- verify item 9 ----------------------------------------------------------------------------------------------

def _rows_env(tmp, conf=CONF2, hub_threads=None, dropin=True, mainpid=HUB_PID, hub_main="0-13"):
    root = tmp / "root"
    (root / "etc").mkdir(parents=True, exist_ok=True)
    (root / "etc" / "strih-dante-rx-cpu.conf").write_text(conf)
    if dropin:
        (root / DROPIN_REL).parent.mkdir(parents=True, exist_ok=True)
        (root / DROPIN_REL).write_text(rp._bash("strih_dante_rx_pin_hub_dropin_text").stdout)
    proc = fake_proc(tmp, main_cpus="2-13", threads={4242: ("obs", "2-13", 0), 4300: ("dstack-engine", "15", -10),
                                                    4301: ("dstack-rx", "15", -10), 4302: ("ndi:io", "2-13", 0)})
    if hub_threads is None:
        hub_threads = {5150: ("intercom-hub", "0-13", 0), 5160: ("dstack-engine", "14", -10),
                       5161: ("dstack-rx", "14", -10), 5162: ("hub-mix", "0-13", 0)}
    hub_proc(tmp, main_cpus=hub_main, threads=hub_threads)
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    pgrep_stub(b, OBS_PID)
    systemctl_stub(b, mainpid)
    return root, proc, {"PATH": "%s:%s" % (b, os.environ["PATH"]), "STRIH_DANTE_RX_PIN_ROOT": str(root),
                        "STRIH_DANTE_RX_PIN_PROC": str(proc)}


def _rx_grade(tmp, hub="1", **kw):
    root, proc, env = _rows_env(tmp, **kw)
    return rp._bash('ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; strih_dante_rx_pin_grade "$1"', hub, env=env)


def test_rx_pin_grade_passes_both_processes(tmp_path):
    r = _rx_grade(tmp_path)
    assert r.returncode == 0 and "BAD" not in r.stdout, r.stdout + r.stderr
    assert "OK (program-dante) rx-pin: dstack-engine tid 4300 on CPU 15 at nice -10" in r.stdout
    assert "OK (intercom-dante) rx-pin: the hub's main affinity 0-13 leaves CPU 14 to the Dante threads" in r.stdout
    assert "OK (intercom-dante) rx-pin: dstack-engine tid 5160 on CPU 14 at nice -10" in r.stdout
    assert "OK (intercom-dante) rx-pin: dstack-rx tid 5161 on CPU 14 at nice -10" in r.stdout
    assert "OK (intercom-dante) rx-pin: %s runs the helper after every hub start" % (tmp_path / "root" / DROPIN_REL) in r.stdout


def test_rx_pin_grade_fails_each_hub_fault_by_name(tmp_path):
    cases = {
        "unpinned": ({5150: ("intercom-hub", "0-13", 0), 5160: ("dstack-engine", "0-13", 0),
                      5161: ("dstack-rx", "14", -10)}, "dstack-engine tid 5160 runs on 0-13, not the reserved CPU 14"),
        "nice": ({5150: ("intercom-hub", "0-13", 0), 5160: ("dstack-engine", "14", 0),
                  5161: ("dstack-rx", "14", -10)}, "dstack-engine tid 5160 has nice 0, wants -10 or lower"),
        "missing": ({5150: ("intercom-hub", "0-13", 0), 5160: ("dstack-engine", "14", -10)},
                    "no dstack-rx thread in the running intercom hub"),
    }
    for name, (threads, want) in cases.items():
        r = _rx_grade(tmp_path / name, hub_threads=threads)
        assert ("BAD (intercom-dante) rx-pin: " + want) in r.stdout, (name, r.stdout)
    r = _rx_grade(tmp_path / "main", hub_main="0-15")   # the hub started before the managers' default
    assert "BAD (intercom-dante) rx-pin: the hub's main affinity 0-15 still includes the reserved CPU 14" in r.stdout
    r = _rx_grade(tmp_path / "nodrop", dropin=False)
    assert "BAD (intercom-dante) rx-pin: %s missing or different" % (tmp_path / "nodrop" / "root" / DROPIN_REL) in r.stdout
    r = _rx_grade(tmp_path / "nohub", mainpid=0)
    assert "BAD (intercom-dante) rx-pin: no running intercom hub" in r.stdout
    r = _rx_grade(tmp_path / "noconf", conf="reserved=15\nobs=2-14\nbase=2-15\n")
    assert "BAD (intercom-dante) rx-pin: no hub CPU" in r.stdout
    # the facts give no hub CPU (HUB=0) but the box still carries it: named, never silent
    r = _rx_grade(tmp_path / "stale", hub="0")
    assert "BAD (intercom-dante) rx-pin: the facts give the hub's Dante threads no CPU of their own, but the conf holds hub=14" in r.stdout
    r = _rx_grade(tmp_path / "clean", hub="0", conf="reserved=15\nobs=2-14\nbase=2-15\n", dropin=False)
    assert "intercom-dante" not in r.stdout, "a one-CPU box: no hub rows at all"


def test_the_grade_counts_its_fails_in_the_callers_shell(tmp_path):
    """Never a pipe into ok / bad: a subshell's FAILS increment is lost and verify prints ALL CLEAR."""
    root, proc, env = _rows_env(tmp_path, dropin=False)
    body = 'FAILS=0; ok() { :; }; bad() { FAILS=$((FAILS + 1)); }; strih_dante_rx_pin_grade 1; echo "FAILS=$FAILS"'
    assert rp._bash(body, env=env).stdout.strip() == "FAILS=1"
    body = 'FAILS=0; ok() { :; }; note() { :; }; bad() { FAILS=$((FAILS + 1)); }; strih_dante_reserved_cpu_grade; echo "FAILS=$FAILS"'
    hub_proc(tmp_path, threads={5150: ("intercom-hub", "0-13", 0), 5162: ("hub-mix", "14", 0)})
    assert rc._bash(RC_LIB, body, env=env).stdout.strip() == "FAILS=2", "the managers' drop-in row + hub-mix on CPU 14"


def _rc_grade(tmp, hub_threads=None, extra=()):
    root, proc, env = _rows_env(tmp, hub_threads=hub_threads)
    text = rc._bash(RC_LIB, "strih_dante_reserved_cpu_manager_text 0-13 15 14").stdout
    for rel in (rc.SYSTEM_DROPIN, rc.USER_DROPIN):
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_text(text)
    (tmp / "online").write_text("0-15\n")
    env["OBS_BOX_ONLINE_CPUS"] = str(tmp / "online")
    rc._task(proc, 1, 1, "systemd", "0-13")
    rc._task(proc, 40, 40, "ksoftirqd/14", "14", rc.PF_KTHREAD)
    for pid, comm, cpus in extra:
        rc._task(proc, pid, pid, comm, cpus)
    return rc._bash(RC_LIB, rc.GRADE, env=env)


def test_reserved_cpu_grade_scans_both_cpus(tmp_path):
    r = _rc_grade(tmp_path)
    assert r.returncode == 0 and "BAD" not in r.stdout, r.stdout + r.stderr
    assert ("OK (program-dante) reserved-cpu: systemd + user managers default to CPUAffinity=0-13 "
            "(CPU 15 kept for OBS's dstack threads, CPU 14 for the hub's)") in r.stdout
    assert "OK (program-dante) reserved-cpu: of 10 tasks only OBS's dstack-engine + dstack-rx (and kernel threads) may run on CPU 15" in r.stdout
    assert ("OK (intercom-dante) reserved-cpu: of 10 tasks only the intercom hub's dstack-engine + dstack-rx "
            "(and kernel threads) may run on CPU 14") in r.stdout


def test_reserved_cpu_grade_names_hub_mix_and_crossed_threads(tmp_path):
    threads = {5150: ("intercom-hub", "0-13", 0), 5160: ("dstack-engine", "14", -10), 5161: ("dstack-rx", "15", -10),
               5162: ("hub-mix", "14", 0)}
    r = _rc_grade(tmp_path, hub_threads=threads, extra=[(900, "bkshading", "0-15")])
    # CPU 14: the hub's SCHED_FIFO hub-mix and bkshading; CPU 15: the hub's dstack-rx (not OBS's) and bkshading
    assert "BAD (intercom-dante) reserved-cpu: 2 of 11 tasks may run on the reserved CPU 14: hub-mix x1, bkshading x1" in r.stdout, r.stdout
    assert "BAD (program-dante) reserved-cpu: 2 of 11 tasks may run on the reserved CPU 15: dstack-rx x1, bkshading x1" in r.stdout, r.stdout
    assert "taskset -a -p -c 0-13 <pid>" in r.stdout


# --- the JPEG XS program encoder --------------------------------------------------------------------------------

def test_the_jpeg_xs_cores_leave_both_cpus(tmp_path):
    import test_strih_jxs_provision_1387 as jx
    root = tmp_path / "root"
    (root / "etc").mkdir(parents=True)
    (root / "etc" / "strih-isolated-cpus.conf").write_text("2-15\n")
    (tmp_path / "online").write_text("0-15\n")
    env = {"STRIH_DANTE_RX_PIN_ROOT": str(root), "OBS_BOX_ONLINE_CPUS": str(tmp_path / "online")}
    r = jx._bash('strih_box_load strih-snv >/dev/null; strih_jxs_output_default_cores', env=env, baseline=True)
    assert r.returncode == 0 and r.stdout.strip() == "2-13", r.stdout + r.stderr
    env["STRIH_BOXES_DIR"] = str(_boxes_without_hub_device(tmp_path))
    r = jx._bash('strih_box_load strih-snv >/dev/null; strih_jxs_output_default_cores', env=env, baseline=True)
    assert r.returncode == 0 and r.stdout.strip() == "2-14", r.stdout + r.stderr


def test_an_operator_value_on_the_hub_cpu_is_warned_and_failed(tmp_path):
    import test_strih_jxs_provision_1387 as jx
    f = tmp_path / "jxs-output.json"
    f.write_text('{"jxs_cores": "10-14"}')
    r = jx._bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100 "" 2-13 "15 14"', f)
    assert r.returncode == 0 and "reserved Dante CPU 14" in r.stderr and "CPU 15" not in r.stderr, r.stderr
    env = {"STRIH_DANTE_RX_PIN_ROOT": str(tmp_path / "root")}
    (tmp_path / "root" / "etc").mkdir(parents=True)
    (tmp_path / "root" / "etc" / "strih-dante-rx-cpu.conf").write_text(CONF2)
    r = jx._bash('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }\nstrih_box_load strih-snv >/dev/null\n'
                 '_strih_jxs_cores_row "$1"', f, env=env, baseline=True)
    assert "FAIL (jxs-cores)" in r.stdout and "holds the reserved Dante CPU(s) 14" in r.stdout, r.stdout + r.stderr


def test_the_libs_wire_the_facts_flag_and_verify_strih_stays_untouched():
    s = DI_LIB.read_text()
    inst = s[s.index("strih_dante_input_install() {"):s.index("strih_dante_input_field() {")]
    assert 'strih_dante_rx_pin_install "$user" "$(_strih_dante_hub_flag)"' in inst
    assert "strih_dante_rx_pin_remove || return 1" in inst
    grade = s[s.index("strih_dante_input_grade() {"):s.index("strih_program_audio_declared()")]
    assert '  strih_dante_rx_pin_grade "$(_strih_dante_hub_flag)"\n  return 0\n}' in grade
    for f in ("verify-strih.sh", "setup-strih.sh"):
        t = (REPO / "scripts" / f).read_text()
        assert "rx_pin" not in t and "hub_flag" not in t, f
