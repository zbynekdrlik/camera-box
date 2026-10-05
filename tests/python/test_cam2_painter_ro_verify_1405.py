"""issue 1405 -- the cam2-painter enable-state window must leave cam2's root READ-ONLY.

`cam2_painter_persist_state_cmds` (scripts/lib/cam2-painter-ro-persist.sh, issue 1175) opens a
`mount -o remount,rw /` window on cam2's read-only root to change the persistent enable-state of
cam2-painter.service. Its `enable-now` mode used to run `systemctl enable --now` INSIDE that window;
the following `mount -o remount,ro / 2>/dev/null || true` failed with EBUSY, the `|| true` hid it,
and cam2 ran on a WRITABLE root until the next reboot (live 4.10.2026, the second `rig-mode.sh test`
of the day). That the start opened the blocking writer is INFERRED from the timing (the painter
became active the same second as the last rw remount), not proven.

The fix (main design, issue comment 5992175067):
- only `systemctl enable` runs inside the window;
- after the ro remount, `findmnt -no OPTIONS /` must read `ro`, else FAIL LOUD naming the writers
  (`fuser -vm /`) and never start the painter;
- only then `systemctl start cam2-painter.service`;
- `disable` gets the same ro verify.

Every test here RUNS the emitted remote text (the issue-1371 run-the-text pattern), never a text
match: a stub-only PATH carries stateful fakes for `mount` / `systemctl` / `findmnt` / `fuser` that
share one fake root state and log every call with the root mode at the moment of the call. A start
on a writable root plants a writer that makes the next ro remount fail "busy" -- the MODELLED 4.10.2026
mechanism (inferred, see above). Tier-0 (#557): no cargo, no rig.
"""

import os
import pathlib
import shutil
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB_DIR = _ROOT / "scripts" / "lib"
_PERSIST_LIB = _LIB_DIR / "cam2-painter-ro-persist.sh"
_HANDOFF_LIB = _LIB_DIR / "cam2-painter-handoff.sh"
_MARKER_LIB = _LIB_DIR / "audio-marker-check.sh"
_RIG_MODE = _ROOT / "scripts" / "rig-mode.sh"

# The remote shell runs the text under `set -e` (both callers start with it); also run every case
# under the dev1-side strict mode so an unbound variable or a pipeline status cannot hide.
_MODES = ["set -e", "set -euo pipefail"]

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

_SYSTEMCTL = r'''
import os, sys
st = os.environ["FAKE_STATE"]
args = sys.argv[1:]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write("systemctl " + " ".join(args) + f" root={root}\n")
verb = args[0] if args else ""
enabled_file = os.path.join(st, "enabled")
active_file = os.path.join(st, "active")


def started():
    # A start on a WRITABLE root opens writers on / (the modelled 4.10.2026 mechanism, issue 1405).
    if root == "rw":
        open(os.path.join(st, "writer"), "w").write("systemd-journal 76355\n")
    open(active_file, "w").write("active\n")


if verb in ("enable", "disable"):
    if verb == "enable" and os.environ.get("FAKE_ENABLE_RC"):
        sys.stderr.write("Failed to enable unit: fake failure\n")
        sys.exit(int(os.environ["FAKE_ENABLE_RC"]))
    if root != "rw":
        sys.stderr.write(f"Failed to {verb} unit: Read-only file system\n")
        sys.exit(1)
    if verb == "enable":
        open(enabled_file, "w").write("enabled\n")
        if "--now" in args:
            started()
    else:
        if os.path.exists(enabled_file):
            os.remove(enabled_file)
elif verb == "start":
    if os.environ.get("FAKE_START_RC"):
        sys.stderr.write("Job for cam2-painter.service failed.\n")
        sys.exit(int(os.environ["FAKE_START_RC"]))
    started()
elif verb == "stop":
    if os.path.exists(active_file):
        os.remove(active_file)
elif verb == "is-enabled":
    state = "enabled" if os.path.exists(enabled_file) else "disabled"
    print(state)
    sys.exit(0 if state == "enabled" else 1)
elif verb == "is-active":
    state = "active" if os.path.exists(active_file) else "inactive"
    print(state)
    sys.exit(0 if state == "active" else 3)
sys.exit(0)
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

_FUSER = r'''
import os, sys
st = os.environ["FAKE_STATE"]
with open(os.path.join(st, "log"), "a") as f:
    f.write("fuser " + " ".join(sys.argv[1:]) + "\n")
if "-s" in sys.argv[1:]:
    sys.exit(0)  # `fuser -s <dev>`: the device is held (the handoff's paint check)
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
import sys
print("COMMAND    PID USER  FD   TYPE DEVICE SIZE/OFF NLINK  NODE NAME")
print("relay     4242 root txt    REG    8,2   123456     0 99999 /usr/local/bin/bkshading-relay (deleted)")
'''

_NOOP = "import sys\nsys.exit(0)\n"


def _write_stub(stub_dir: pathlib.Path, name: str, body: str) -> None:
    p = stub_dir / name
    p.write_text(f"#!{sys.executable}\n{body}")
    p.chmod(0o755)


@pytest.fixture
def box(tmp_path):
    """A fake cam2: a stub-only PATH dir + the shared fake root state (starts read-only)."""
    stub = tmp_path / "bin"
    state = tmp_path / "state"
    stub.mkdir()
    state.mkdir()
    _write_stub(stub, "mount", _MOUNT)
    _write_stub(stub, "systemctl", _SYSTEMCTL)
    _write_stub(stub, "findmnt", _FINDMNT)
    _write_stub(stub, "fuser", _FUSER)
    _write_stub(stub, "sleep", _NOOP)
    _write_stub(stub, "lsof", _LSOF)
    for tool in ("awk", "grep", "head", "rm"):
        real = shutil.which(tool, path="/usr/bin:/bin")
        assert real, f"{tool} not found on this host"
        (stub / tool).symlink_to(real)
    (state / "root").write_text("ro\n")
    (state / "log").write_text("")
    return {"stub": stub, "state": state}


def _build(body: str, *args: str) -> str:
    """Run a dev1-side builder under rig-mode.sh's own `set -euo pipefail` and return its text."""
    proc = subprocess.run(
        ["/bin/bash", "-c", "set -euo pipefail\n" + body, "harness", *args],
        env={"PATH": "/usr/bin:/bin"},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"builder exited {proc.returncode}: stderr={proc.stderr!r}"
    assert "unbound variable" not in proc.stderr, proc.stderr
    return proc.stdout


def _persist_text(mode: str) -> str:
    return _build(f'. "{_PERSIST_LIB}"\ncam2_painter_persist_state_cmds "$1"', mode)


def _handoff_text() -> str:
    return _build(
        f'. "{_MARKER_LIB}"\n. "{_HANDOFF_LIB}"\n'
        "cam2_painter_steady_state_handoff_cmds /nonexistent/rig-painter.pid /nonexistent/markers.csv"
    )


def _rig_mode_function(name: str) -> str:
    """Cut one function definition out of rig-mode.sh (its header is a count-1 anchor)."""
    text = _RIG_MODE.read_text()
    head = f"\n{name}() {{\n"
    assert text.count(head) == 1, f"{name}() must be defined exactly once in rig-mode.sh"
    start = text.index(head) + 1
    end = text.index("\n}\n", start) + 3
    return text[start:end]


def _painter_stop_text() -> str:
    # The WHOLE EVENT cam-side script, built by sourcing rig-mode.sh (its BASH_SOURCE guard skips
    # main) the way tests/rig_mode.rs::run_sourced does.
    return _build(f'set +e\n. "{_RIG_MODE}"\npainter_stop_remote /nonexistent/rig-painter.pid')


def _event_disable_text() -> str:
    return _build(f'. "{_PERSIST_LIB}"\n{_rig_mode_function("cam2_painter_service_disable_cmds")}\ncam2_painter_service_disable_cmds')


def _run(box, text: str, strict: str, **fake) -> subprocess.CompletedProcess:
    env = {"PATH": str(box["stub"]), "FAKE_STATE": str(box["state"])}
    env.update({k: v for k, v in fake.items() if v is not None})
    return subprocess.run(["/bin/bash", "-c", f"{strict}\n{text}"], env=env, capture_output=True, text=True)


def _log(box) -> list[str]:
    return [ln for ln in (box["state"] / "log").read_text().splitlines() if ln.strip()]


def _root(box) -> str:
    return (box["state"] / "root").read_text().strip()


def _index(log: list[str], prefix: str) -> int:
    hits = [i for i, ln in enumerate(log) if ln.startswith(prefix)]
    assert hits, f"no call starting {prefix!r} in the call log:\n" + "\n".join(log)
    return hits[0]


def _starts(log: list[str]) -> list[str]:
    return [ln for ln in log if ln.startswith("systemctl start") or "--now" in ln]


# ---- enable-now: enable inside the window, verify ro, start after ------------------------------ #


@pytest.mark.parametrize("strict", _MODES)
def test_enable_now_clean_window_ends_ro_before_the_start_1405(box, strict):
    proc = _run(box, _persist_text("enable-now"), strict)
    log = _log(box)
    assert proc.returncode == 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}\nlog={log}"
    assert not any("--now" in ln for ln in log), f"enable --now must never run inside the window:\n{log}"
    rw = _index(log, "mount -o remount,rw /")
    enable = _index(log, "systemctl enable cam2-painter.service root=rw")
    ro = _index(log, "mount -o remount,ro / root=rw")
    read = _index(log, "findmnt -no OPTIONS /")
    start = _index(log, "systemctl start cam2-painter.service root=ro")
    assert rw < enable < ro < read < start, f"wrong order:\n{log}"
    assert len(_starts(log)) == 1, f"exactly one start, after the verify:\n{log}"
    assert _root(box) == "ro", f"the root must end read-only:\n{log}"
    assert "ENABLED + persisted" in proc.stdout, proc.stdout


@pytest.mark.parametrize("strict", _MODES)
def test_enable_now_failed_ro_remount_fails_loud_with_no_start_1405(box, strict):
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, f"a root left rw must fail loud.\nstdout={proc.stdout!r}\nlog={log}"
    assert _starts(log) == [], f"the painter must NOT start on a writable root:\n{log}"
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    assert "mount point is busy" in proc.stderr, f"must name the mount error:\n{proc.stderr}"
    assert "76355" in proc.stderr and "systemd-journal" in proc.stderr, (
        f"must name the writers holding / (fuser -vm /):\n{proc.stderr}"
    )
    assert any(ln.startswith("fuser -vm /") for ln in log), log
    assert "ENABLED + persisted" not in proc.stdout, proc.stdout


@pytest.mark.parametrize("strict", _MODES)
def test_enable_now_trusts_findmnt_not_the_mount_exit_code_1405(box, strict):
    # The ro remount exits 0 yet the root is still rw: only the findmnt reading may decide.
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_RO_LIES="1")
    log = _log(box)
    assert proc.returncode != 0, f"rw after a zero-exit remount must fail loud:\n{log}"
    assert _starts(log) == [], log
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_enable_now_unreadable_root_state_refuses_the_start_1405(box, strict):
    # findmnt fails and the /proc/mounts fallback fails too (a failing awk here, so the host's own
    # /proc/mounts is never read): the mode is unknown, never assumed ro.
    (box["stub"] / "awk").unlink()
    _write_stub(box["stub"], "awk", "import sys\nsys.exit(2)\n")
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_FINDMNT_EMPTY="1")
    log = _log(box)
    assert proc.returncode != 0, f"an unreadable root state must fail loud:\n{log}"
    assert _starts(log) == [], log
    assert "unknown" in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_enable_failure_still_ends_ro_and_never_starts_1405(box, strict):
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_ENABLE_RC="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert _root(box) == "ro", f"the window must close read-only even when the enable failed:\n{log}"
    assert _starts(log) == [], log
    assert "rc=1" in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_start_failure_after_a_good_enable_is_its_own_named_failure_1405(box, strict):
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_START_RC="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert _root(box) == "ro", log
    assert "systemctl start cam2-painter.service" in proc.stderr, (
        f"the failed start must be named on its own:\n{proc.stderr}"
    )
    assert (box["state"] / "enabled").exists(), "the enable itself persisted"


@pytest.mark.parametrize("strict", _MODES)
def test_enable_now_window_refused_rw_fails_loud_with_no_start_1405(box, strict):
    proc = _run(box, _persist_text("enable-now"), strict, FAKE_RW_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert _starts(log) == [], log
    assert "could not remount" in proc.stderr, proc.stderr


# ---- disable: the same ro verify --------------------------------------------------------------- #


@pytest.mark.parametrize("strict", _MODES)
def test_disable_verifies_ro_after_the_window_1405(box, strict):
    (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _persist_text("disable"), strict)
    log = _log(box)
    assert proc.returncode == 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}\nlog={log}"
    disable = _index(log, "systemctl disable cam2-painter.service root=rw")
    ro = _index(log, "mount -o remount,ro / root=rw")
    read = _index(log, "findmnt -no OPTIONS /")
    assert disable < ro < read, f"the disable must be followed by a verified ro remount:\n{log}"
    assert _starts(log) == [], f"disable must never start the painter:\n{log}"
    assert _root(box) == "ro", log
    assert "DISABLED + persisted" in proc.stdout, proc.stdout


@pytest.mark.parametrize("strict", _MODES)
def test_disable_with_a_root_left_rw_fails_loud_1405(box, strict):
    (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _persist_text("disable"), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, f"disable leaving a writable root must fail loud:\n{log}"
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    assert "systemd-journal" in proc.stderr, f"must name the writers:\n{proc.stderr}"
    assert _starts(log) == [], log


# ---- the emitted text ends every statement with ';' (the $(...) newline-strip gotcha) ------------ #


@pytest.mark.parametrize("mode", ["enable-now", "disable"])
def test_embedding_never_glues_the_following_command_1405(box, tmp_path, mode):
    if mode == "disable":
        (box["state"] / "enabled").write_text("enabled\n")
    marker = tmp_path / "next-command-ran"
    marker.write_text("x")
    # Build the text the way a caller embeds it: `$(...)` strips its trailing newline, so the next
    # command lands on the SAME line. It must still run as its own statement.
    text = _build(
        f'. "{_PERSIST_LIB}"\nprintf \'%s\\n\' "$(cam2_painter_persist_state_cmds "$1") rm -f {marker}"',
        mode,
    )
    proc = _run(box, text, "set -e")
    assert proc.returncode == 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    assert not marker.exists(), f"the command after the embedded text never ran:\n{proc.stdout}"


# ---- the real callers: the TEST handoff and the EVENT disable ---------------------------------- #


@pytest.mark.parametrize("strict", _MODES)
def test_handoff_starts_the_painter_only_on_a_verified_ro_root_1405(box, strict):
    proc = _run(box, _handoff_text(), strict)
    log = _log(box)
    assert not any("--now" in ln for ln in log), log
    start = _index(log, "systemctl start cam2-painter.service root=ro")
    read = _index(log, "findmnt -no OPTIONS /")
    active = _index(log, "systemctl is-active cam2-painter.service")
    assert read < start < active, f"verify ro, then start, then the H4 active check:\n{log}"
    assert _root(box) == "ro", log
    # The later H5 paint check has no journal here and fails; the order above is what matters.
    assert "FAIL: [#1405]" not in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_handoff_with_a_writer_on_root_fails_loud_before_any_start_1405(box, strict):
    proc = _run(box, _handoff_text(), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    assert _starts(log) == [], log
    assert not any(ln.startswith("systemctl is-active") for ln in log), (
        f"the handoff must stop at the ro verify, before H4:\n{log}"
    )


@pytest.mark.parametrize("strict", _MODES)
def test_event_disable_ends_ro_and_never_starts_1405(box, strict):
    (box["state"] / "enabled").write_text("enabled\n")
    (box["state"] / "active").write_text("active\n")
    proc = _run(box, _event_disable_text(), strict)
    log = _log(box)
    assert proc.returncode == 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}\nlog={log}"
    stop = _index(log, "systemctl stop cam2-painter.service")
    read = _index(log, "findmnt -no OPTIONS /")
    assert stop < read, log
    assert _starts(log) == [], log
    assert _root(box) == "ro", log
    assert not (box["state"] / "enabled").exists(), log


@pytest.mark.parametrize("strict", _MODES)
def test_event_disable_with_a_writer_on_root_fails_loud_1405(box, strict):
    (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _event_disable_text(), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    assert "systemd-journal" in proc.stderr, proc.stderr
    assert _starts(log) == [], log


def test_stub_path_is_hermetic(box):
    # Guard against a stub falling through to a real host tool (the CI runner's own root is rw).
    names = sorted(os.listdir(box["stub"]))
    assert names == ["awk", "findmnt", "fuser", "grep", "head", "lsof", "mount", "rm", "sleep", "systemctl"], names


# ---- review round 1: the writers are named, and EVENT never leaves the dead-man armed ------------ #


@pytest.mark.parametrize("mode", ["enable-now", "disable"])
def test_failure_names_the_writer_past_the_kernel_threads_1405(box, mode):
    # The writer is PID 76355 at line ~52 of `fuser -vm /`: a `| head -n 40` would cut it off.
    if mode == "disable":
        (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _persist_text(mode), "set -e", FAKE_RO_FAIL="1")
    assert proc.returncode != 0, proc.stderr
    assert "76355" in proc.stderr and "systemd-journal" in proc.stderr, (
        f"the process holding / open for WRITING must be named:\n{proc.stderr}"
    )
    assert "kworker/3:0-events" not in proc.stderr, (
        f"only the writers (ACCESS F) are listed, not every process on /:\n{proc.stderr}"
    )


@pytest.mark.parametrize("mode", ["enable-now", "disable"])
def test_failure_names_deleted_but_open_holders_1405(box, mode):
    # A deleted-but-still-open file (a replaced binary) keeps / busy without any ACCESS F writer.
    if mode == "disable":
        (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _persist_text(mode), "set -e", FAKE_RO_FAIL="1")
    assert proc.returncode != 0, proc.stderr
    assert "/usr/local/bin/bkshading-relay (deleted)" in proc.stderr, (
        f"the deleted-but-open holder must be named (the existing issue-808 probe):\n{proc.stderr}"
    )


def test_disable_failure_reports_the_enable_state_1405(box):
    (box["state"] / "enabled").write_text("enabled\n")
    proc = _run(box, _persist_text("disable"), "set -e", FAKE_RO_FAIL="1")
    assert proc.returncode != 0, proc.stderr
    assert "is-enabled now: 'disabled'" in proc.stderr, (
        f"a failed EVENT must still say whether a reboot would re-arm the QR:\n{proc.stderr}"
    )


@pytest.mark.parametrize("strict", _MODES)
def test_event_painter_stop_disarms_the_deadman_before_a_failing_disable_1405(box, strict):
    # rig-mode.sh test ARMS the cam2-painter dead-man (it starts cam2-painter whenever no
    # frame-probe runs). If the EVENT disable step fails loud on a root left rw, the dead-man must
    # already be disarmed, or it puts the QR painter back on air within ~5 min.
    (box["state"] / "enabled").write_text("enabled\n")
    (box["state"] / "active").write_text("active\n")
    proc = _run(box, _painter_stop_text(), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}\nlog={log}"
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    disarm = _index(log, "systemctl stop cam2-painter-deadman.timer")
    stop = _index(log, "systemctl stop cam2-painter.service")
    ro = _index(log, "mount -o remount,ro /")
    assert disarm < stop < ro, f"the dead-man must be disarmed before the stop+disable:\n{log}"
    assert _starts(log) == [], log


# ---- review round 2: the TEST handoff disarms the dead-man; an unavailable fuser is named ------- #


@pytest.mark.parametrize("strict", _MODES)
def test_handoff_disarms_the_deadman_before_a_failing_window_1405(box, strict):
    # rig-mode.sh test arms the dead-man after a GOOD handoff; only EVENT disarms it. On a second
    # TEST whose handoff fails at the ro verify, a still-armed dead-man would start the painter on
    # the writable root within ~5 min, so the handoff disarms it first.
    proc = _run(box, _handoff_text(), strict, FAKE_RO_FAIL="1")
    log = _log(box)
    assert proc.returncode != 0, log
    assert "FAIL: [#1405]" in proc.stderr, proc.stderr
    disarm = _index(log, "systemctl stop cam2-painter-deadman.timer")
    rw = _index(log, "mount -o remount,rw /")
    assert disarm < rw, f"the dead-man must be disarmed before the remount-rw window:\n{log}"
    assert _starts(log) == [], log


@pytest.mark.parametrize("mode", ["enable-now", "disable"])
def test_failure_says_so_when_fuser_prints_no_listing_1405(box, mode):
    # A missing or failing fuser must never read as "no process holds a file open for writing".
    if mode == "disable":
        (box["state"] / "enabled").write_text("enabled\n")
    (box["stub"] / "fuser").unlink()
    _write_stub(box["stub"], "fuser", "import sys\nsys.exit(1)\n")
    proc = _run(box, _persist_text(mode), "set -e", FAKE_RO_FAIL="1")
    assert proc.returncode != 0, proc.stderr
    assert "fuser printed no listing" in proc.stderr, proc.stderr
    assert "(none: no process holds a file open for writing on /)" not in proc.stderr, proc.stderr
