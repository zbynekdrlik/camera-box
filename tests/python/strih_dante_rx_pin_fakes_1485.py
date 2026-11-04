"""Issue 1485: a fake /proc of the strih OBS for the Dante thread pin (helper, grade) tests.

Not a test module (no `test_` prefix): test_strih_dante_rx_pin_1485.py and test_strih_dante_input_1485.py
import it. A thread is `<proc>/<pid>/task/<tid>/` with `comm`, `status` (Cpus_allowed_list) and `stat`
(field 19 = nice, after the comm in parentheses).
"""
from pathlib import Path

OBS_PID = 4242
# tid -> comm; 4242 is the main thread, 4300/4301 the Dante threads, 4302 an NDI thread that must
# never be touched.
THREADS = {4242: "obs", 4300: "dstack-engine", 4301: "dstack-rx", 4302: "ndi:io"}


def stat_line(tid, comm, nice):
    # 52 fields as the kernel prints them; field 19 is nice, field 18 priority (20 + nice).
    rest = ["S"] + ["0"] * 14 + [str(20 + int(nice)), str(nice)] + ["0"] * 33
    return "%d (%s) %s\n" % (tid, comm, " ".join(rest))


def write_thread(proc, tid, comm, cpus, nice, pid=OBS_PID):
    t = Path(proc) / str(pid) / "task" / str(tid)
    t.mkdir(parents=True, exist_ok=True)
    (t / "comm").write_text(comm + "\n")
    (t / "status").write_text("Name:\t%s\nCpus_allowed:\tffff\nCpus_allowed_list:\t%s\n" % (comm, cpus))
    (t / "stat").write_text(stat_line(tid, comm, nice))
    return t


def fake_proc(root, main_cpus="2-14", threads=None, pid=OBS_PID):
    """threads: {tid: (comm, cpus, nice)}; default = every thread on main_cpus at nice 0."""
    proc = Path(root) / "proc"
    p = proc / str(pid)
    p.mkdir(parents=True, exist_ok=True)
    (p / "status").write_text("Name:\tobs\nCpus_allowed_list:\t%s\n" % main_cpus)
    if threads is None:
        threads = {tid: (comm, main_cpus, 0) for tid, comm in THREADS.items()}
    for tid, (comm, cpus, nice) in threads.items():
        write_thread(proc, tid, comm, cpus, nice, pid)
    return proc


def read_thread(proc, tid, pid=OBS_PID):
    t = Path(proc) / str(pid) / "task" / str(tid)
    cpus = [l.split("\t", 1)[1] for l in (t / "status").read_text().splitlines() if l.startswith("Cpus_allowed_list:")][0]
    nice = (t / "stat").read_text().rsplit(") ", 1)[1].split()[16]
    return cpus, int(nice)


def stub(bindir, name, body):
    f = Path(bindir) / name
    f.write_text("#!/bin/bash\n" + body)
    f.chmod(0o755)
    return f


def pgrep_stub(bindir, pid=OBS_PID):
    return stub(bindir, "pgrep", 'echo "$*" >> "${PGREP_LOG:-/dev/null}"\n%s\n' % (("echo %d" % pid) if pid else "exit 1"))


# The intercom hub (4.11.2026, comment 6634395906): its own Dante device's two threads, the SCHED_FIFO
# hub-mix thread and a tokio worker that must never move. 5150 = the unit's MainPID.
HUB_PID = 5150
HUB_THREADS = {5150: "intercom-hub", 5160: "dstack-engine", 5161: "dstack-rx", 5162: "hub-mix", 5163: "tokio-rt-worker"}


def hub_proc(proc_root, main_cpus="0-13", threads=None):
    """The hub's threads under the same fake /proc (`proc_root` = the dir that holds `proc`)."""
    if threads is None:
        threads = {tid: (comm, main_cpus, 0) for tid, comm in HUB_THREADS.items()}
    return fake_proc(proc_root, main_cpus=main_cpus, threads=threads, pid=HUB_PID)


def systemctl_stub(bindir, mainpid=HUB_PID, log=None):
    """`systemctl show -p MainPID --value intercom-hub` answers MAINPID (0 = the hub does not run); every
    other call is logged and succeeds."""
    return stub(bindir, "systemctl",
                'printf "%%s\\n" "$*" >> "%s"\n'
                'if [ "$*" = "show -p MainPID --value intercom-hub" ]; then echo %d; fi\n' % (log or "/dev/null", mainpid))
