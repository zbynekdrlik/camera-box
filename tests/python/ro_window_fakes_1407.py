"""A fake read-only-root box for the issue-1407 tests (not collected: no test_ prefix).

`make_box(tmp_path)` builds a stub-only PATH dir and ONE fake root state (state/root = ro|rw) that
every fake shares: `mount`, `findmnt`, `sync`, `fuser`, `lsof`, `systemctl`, `systemd-run`,
`journalctl`. Every call is logged as `<tool> <args> root=<mode>` in state/log, so a test reads the
ORDER of the calls and the root mode at each one (the issue-1405 run-the-text pattern, generalized).

The modelled failure (issue 1405, live 4.10.2026 on cam2): a service START on a WRITABLE root opens
a writer on /, so the next ro remount fails "busy". `systemctl start|restart|enable --now` on an rw
root plants that writer. Env knobs: FAKE_RW_FAIL, FAKE_RO_FAIL (busy), FAKE_RO_LIES (exit 0, root
stays rw), FAKE_FINDMNT_EMPTY, FAKE_START_RC, FAKE_ENABLE_RC, FAKE_STOP_RC, FAKE_UNIT_MISSING.
Issue 1394 added `mask`/`unmask` (refused on a ro root, like enable), `is-failed`, `reset-failed`
and `is-system-running` (state/failed-<unit> files; `degraded` while one exists, or
FAKE_SYSTEM_STATE); a failed start leaves its unit failed, a good start clears it.

`run_text(box, text)` runs emitted remote text under `set -e` and `set -euo pipefail` callers' modes
with PATH = the stub dir only (plus the few real text tools the emitted text needs), so nothing
falls through to the host's own root.
"""
import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
LIB = ROOT / "scripts" / "lib"

_MOUNT = r'''
import os, sys
st = os.environ["FAKE_STATE"]
args = " ".join(sys.argv[1:])
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write(f"mount {args} root={root}\n")
if "remount,rw" in args:
    if os.environ.get("FAKE_RW_FAIL") == "1":
        sys.stderr.write("mount: /: cannot remount read-write, is write-protected.\n")
        sys.exit(32)
    open(os.path.join(st, "root"), "w").write("rw\n")
elif "remount,ro" in args:
    if os.path.exists(os.path.join(st, "writer")) or os.environ.get("FAKE_RO_FAIL") == "1":
        sys.stderr.write("mount: /: mount point is busy.\n")
        sys.exit(32)
    if os.environ.get("FAKE_RO_LIES") == "1":
        sys.exit(0)  # exit 0, yet the root stays read-write: only findmnt tells the truth
    open(os.path.join(st, "root"), "w").write("ro\n")
'''

_FINDMNT = r'''
import os, sys
st = os.environ["FAKE_STATE"]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("findmnt " + " ".join(sys.argv[1:]) + f" root={root}\n")
if os.environ.get("FAKE_FINDMNT_EMPTY") == "1":
    sys.exit(1)
print(f"{root},relatime")
'''

_SYSTEMCTL = r'''
import os, sys
st = os.environ["FAKE_STATE"]
args = sys.argv[1:]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("systemctl " + " ".join(args) + f" root={root}\n")
verb = args[0] if args else ""
units = [a for a in args[1:] if not a.startswith("-")]
unit = units[0] if units else ""


def path(kind, u):
    return os.path.join(st, f"{kind}-{u}")


def started(u):
    # A start on a WRITABLE root opens writers on / (the modelled issue-1405 mechanism).
    if root == "rw":
        open(os.path.join(st, "writer"), "w").write("systemd-journal 76355\n")
    open(path("active", u), "w").write("active\n")
    if os.path.exists(path("failed", u)):  # a good start clears a unit's failed state
        os.remove(path("failed", u))


if verb in ("enable", "disable", "mask", "unmask"):
    if verb == "enable" and os.environ.get("FAKE_ENABLE_RC"):
        sys.stderr.write("Failed to enable unit: fake failure\n")
        sys.exit(int(os.environ["FAKE_ENABLE_RC"]))
    if root != "rw":
        sys.stderr.write(f"Failed to {verb} unit: Read-only file system\n")
        sys.exit(1)
    if verb == "enable":
        open(path("enabled", unit), "w").write("enabled\n")
        if "--now" in args:
            started(unit)
    elif verb == "mask":  # issue 1394: a mask is a symlink write on the root, like enable
        for u in units:
            open(path("masked", u), "w").write("masked\n")
    elif verb == "unmask":
        for u in units:
            if os.path.exists(path("masked", u)):
                os.remove(path("masked", u))
    elif os.path.exists(path("enabled", unit)):
        os.remove(path("enabled", unit))
elif verb in ("start", "restart"):
    if os.environ.get("FAKE_START_RC"):
        fail = os.path.join(st, "start-failed-once")
        if os.environ.get("FAKE_START_ONCE") != "1" or not os.path.exists(fail):
            open(fail, "w").write("x")
            open(path("failed", unit), "w").write("failed\n")
            sys.stderr.write(f"Job for {unit} failed.\n")
            sys.exit(int(os.environ["FAKE_START_RC"]))
    started(unit)
elif verb == "is-failed":
    state = "failed" if os.path.exists(path("failed", unit)) else "inactive"
    if "--quiet" not in args:
        print(state)
    sys.exit(0 if state == "failed" else 1)
elif verb == "reset-failed":
    for u in units:
        if os.path.exists(path("failed", u)):
            os.remove(path("failed", u))
elif verb == "is-system-running":
    # FAKE_SYSTEM_STATE wins; else `degraded` while any unit is failed, the way systemd reads it
    failed = [f for f in os.listdir(st) if f.startswith("failed-")]
    state = os.environ.get("FAKE_SYSTEM_STATE") or ("degraded" if failed else "running")
    print(state)
    sys.exit(0 if state == "running" else 1)
elif verb == "stop":
    if os.environ.get("FAKE_STOP_RC") and not unit.endswith(".timer"):
        sys.stderr.write(f"Failed to stop {unit}.\n")
        sys.exit(int(os.environ["FAKE_STOP_RC"]))
    if os.path.exists(path("active", unit)):
        os.remove(path("active", unit))
elif verb == "is-enabled":
    state = "enabled" if os.path.exists(path("enabled", unit)) else "disabled"
    if os.path.exists(path("masked", unit)):
        state = "masked"
    print(state)
    sys.exit(0 if state == "enabled" else 1)
elif verb == "is-active":
    state = "active" if os.path.exists(path("active", unit)) else "inactive"
    print(state)
    sys.exit(0 if state == "active" else 3)
elif verb == "show":
    if "ActiveState" in args:
        print("active" if os.path.exists(path("active", args[-1])) else "inactive")
elif verb == "cat":
    if os.environ.get("FAKE_UNIT_MISSING") == "1":
        sys.exit(1)
    print(f"# {unit}")
sys.exit(0)
'''

_FUSER = r'''
import os, sys
st = os.environ["FAKE_STATE"]
with open(os.path.join(st, "log"), "a") as f:
    f.write("fuser " + " ".join(sys.argv[1:]) + "\n")
# `fuser -vm /` the way a real box prints it: PID 1 and the kernel threads come first, so the one
# real writer (ACCESS F) sits far past the first 40 lines.
sys.stderr.write("                     USER        PID ACCESS COMMAND\n")
sys.stderr.write("/:                   root     kernel mount /\n")
sys.stderr.write("                     root          1 .rce. systemd\n")
for pid in range(2, 50):
    sys.stderr.write(f"                     root      {pid:5d} .rc.. kworker/{pid}:0-events\n")
sys.stderr.write("                     root      76355 F.... systemd-journal\n")
'''

# `lsof +L1`: one process holding a deleted-but-open file on / (the issue-808 EBUSY cause).
_LSOF = r'''
print("COMMAND    PID USER  FD   TYPE DEVICE SIZE/OFF NLINK  NODE NAME")
print("relay     4242 root txt    REG    8,2   123456     0 99999 /usr/local/bin/bkshading-relay (deleted)")
'''

_SYNC = r'''
import os, sys
st = os.environ["FAKE_STATE"]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("sync " + " ".join(sys.argv[1:]) + f" root={root}\n")
'''

_LOGGED_NOOP = r'''
import os, sys
st = os.environ["FAKE_STATE"]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write(os.path.basename(sys.argv[0]) + " " + " ".join(sys.argv[1:]) + f" root={root}\n")
'''

_JOURNALCTL = r'''
print("INFO camera_box: Streaming: 30.0 fps emitted / 60.0 fps captured (150 sent, 300 captured)")
'''

# Real text tools the emitted text itself needs (awk for the writer filter and the /proc/mounts
# fallback, the rest for the deleted-holder probe and the callers' own lines).
REAL_TOOLS = ("awk", "grep", "head", "tail", "rm", "sort", "tr", "readlink", "cat", "cp", "install", "chmod",
              "mv", "mktemp", "basename", "dirname", "sha256sum", "date", "sed", "sleep", "env", "bash", "wc")


def _write_stub(stub_dir, name, body):
    p = stub_dir / name
    p.write_text(f"#!{sys.executable}\n{body}")
    p.chmod(0o755)


def make_box(tmp_path, root="ro"):
    """A fake box: {"stub": PATH dir, "state": the shared state dir}. The root starts `root`."""
    stub = tmp_path / "bin"
    state = tmp_path / "state"
    stub.mkdir()
    state.mkdir()
    _write_stub(stub, "mount", _MOUNT)
    _write_stub(stub, "findmnt", _FINDMNT)
    _write_stub(stub, "systemctl", _SYSTEMCTL)
    _write_stub(stub, "fuser", _FUSER)
    _write_stub(stub, "lsof", _LSOF)
    _write_stub(stub, "sync", _SYNC)
    _write_stub(stub, "systemd-run", _LOGGED_NOOP)
    _write_stub(stub, "journalctl", _JOURNALCTL)
    for tool in REAL_TOOLS:
        real = shutil.which(tool, path="/usr/bin:/bin")
        assert real, f"{tool} not found on this host"
        (stub / tool).symlink_to(real)
    (state / "root").write_text(f"{root}\n")
    (state / "log").write_text("")
    return {"stub": stub, "state": state}


def box_env(box, **fake):
    env = {"PATH": str(box["stub"]), "FAKE_STATE": str(box["state"]), "HOME": str(box["state"])}
    env.update({k: v for k, v in fake.items() if v is not None})
    return env


def run_text(box, text, strict="set -e", **fake):
    """Run emitted REMOTE text on the fake box (bash, stub-only PATH)."""
    return subprocess.run(["/bin/bash", "-c", f"{strict}\n{text}"], env=box_env(box, **fake),
                          capture_output=True, text=True, timeout=60)


def build(body, *args, env=None):
    """Run a dev1-side builder under `set -euo pipefail` and return its stdout (asserts exit 0)."""
    e = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp")}
    e.update(env or {})
    proc = subprocess.run(["/bin/bash", "-c", "set -euo pipefail\n" + body, "harness", *args],
                          env=e, capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, f"builder exited {proc.returncode}: stderr={proc.stderr!r}"
    assert "unbound variable" not in proc.stderr, proc.stderr
    return proc.stdout


def log(box):
    return [ln for ln in (box["state"] / "log").read_text().splitlines() if ln.strip()]


def root(box):
    return (box["state"] / "root").read_text().strip()


def index(lines, prefix, start=0):
    hits = [i for i, ln in enumerate(lines) if i >= start and ln.startswith(prefix)]
    assert hits, f"no call starting {prefix!r} (from {start}) in the call log:\n" + "\n".join(lines)
    return hits[0]


def starts(lines):
    """Every call that STARTS something: a start/restart, an `enable --now`, a deadman re-arm."""
    return [ln for ln in lines
            if ln.startswith(("systemctl start", "systemctl restart", "systemd-run")) or "--now" in ln]


def starts_on_rw(lines):
    """The issue-1405/1407 invariant: nothing is started while the root is writable."""
    return [ln for ln in starts(lines) if ln.endswith("root=rw")]


def env_without(prefixes):
    return {k: v for k, v in os.environ.items() if not k.startswith(prefixes)}
