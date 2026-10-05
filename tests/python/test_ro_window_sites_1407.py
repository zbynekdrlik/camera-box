"""issue 1407 -- every rw-window site RUN on a fake read-only-root box: nothing starts before the
verified close, and a close that leaves the root writable fails the step by name and starts nothing.

The fake box (tests/python/ro_window_fakes_1407.py) logs every call with the root mode at that moment
and plants a writer on / when something starts on a writable root (the modelled issue-1405 cause).
So `starts_on_rw(log) == []` is the issue-1405/1407 invariant itself, read from a real run:
- deploy-fleet.sh, run whole with a fake `sshpass` that executes each remote command on the fake box
  (the camera-box swap on a cambox, and the cam2 frame-probe swap in both #892 states);
- the dantesync Linux upgrade and rollback programs, run as emitted (a staged binary, real files);
- bkshading-relay-mode.sh's stop/start texts;
- the ndi-discovery `--cambox-apply` program;
- rt-kernel-plan.sh's printed runbook programs (print-only; the supervisor pastes them into a box's
  root shell), run as printed.
bkshading-deploy-relay.sh is driven end to end by its own tests (test_bkshading_relay_gaps_808.py).
Tier-0 (#557): no cargo, no rig, no network.
"""
import hashlib
import os
import shutil
import subprocess
import sys


sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import (  # noqa: E402
    LIB, ROOT, box_env, build, index, log, make_box, root, run_text, starts, starts_on_rw,
)

_DEPLOY = ROOT / "scripts" / "deploy-fleet.sh"
_UPGRADE = ROOT / "scripts" / "dantesync-fleet-upgrade.sh"

# A fake `sshpass` (dev1 side): `scp` copies into the fake box's filesystem under state/fs, `ssh`
# RUNS the remote command on the fake box (stub-only PATH), with /usr/local/bin/ mapped into state/fs.
_SSHPASS = r'''
import os, shutil, subprocess, sys
st = os.environ["FAKE_STATE"]
fs = os.path.join(st, "fs")
args = sys.argv[3:]  # drop `-p <pass>`
tool = args[0]


def mapped(s):
    return s.replace("/usr/local/bin/", fs + "/usr/local/bin/")


root = open(os.path.join(st, "root")).read().strip()
if tool == "scp":
    dest = args[-1].split(":", 1)[1]
    with open(os.path.join(st, "log"), "a") as f:
        f.write(f"SCP {dest} root={root}\n")
    if os.environ.get("FAKE_SCP_SLEEP"):
        import time
        time.sleep(float(os.environ["FAKE_SCP_SLEEP"]))
    if os.environ.get("FAKE_SCP_RC"):
        sys.exit(int(os.environ["FAKE_SCP_RC"]))
    if root != "rw":
        sys.stderr.write(f"scp: {dest}: Read-only file system\n")
        sys.exit(1)
    os.makedirs(os.path.dirname(mapped(dest)), exist_ok=True)
    if os.environ.get("FAKE_SCP_PARTIAL"):  # the transfer dies half-way: half the bytes landed
        data = open(args[-2], "rb").read()
        open(mapped(dest), "wb").write(data[:len(data) // 2])
        sys.stderr.write("scp: Connection closed\n")
        sys.exit(1)
    if os.environ.get("FAKE_SCP_CORRUPT"):  # the transfer "succeeds" with the wrong bytes
        open(mapped(dest), "wb").write(b"CORRUPTED-IN-TRANSIT\n")
        sys.exit(0)
    shutil.copy(args[-2], mapped(dest))
    sys.exit(0)
env = {"PATH": os.environ["FAKE_BOX_PATH"], "FAKE_STATE": st, "HOME": st}
env.update({k: v for k, v in os.environ.items() if k.startswith("FAKE_")})
wait = os.path.join(st, "close-wait")
if os.environ.get("FAKE_CLOSE_SLEEP") and "_row_ro_err=" in args[-1] and not os.path.exists(wait):
    import time
    open(wait, "w").write("x")
    time.sleep(float(os.environ["FAKE_CLOSE_SLEEP"]))  # the first close is interrupted here
sys.exit(subprocess.run(["/bin/bash", "-c", mapped(args[-1])], env=env).returncode)
'''


def _dev1_bin(tmp_path, box):
    d = tmp_path / "dev1-bin"
    d.mkdir()
    (d / "sshpass").write_text(f"#!{sys.executable}\n{_SSHPASS}")
    (d / "sshpass").chmod(0o755)
    (d / "gh").write_text("#!/bin/sh\necho GH-CALLED-UNEXPECTEDLY >&2\nexit 1\n")
    (d / "gh").chmod(0o755)
    return d


# Box-side file tools the deploy's swap uses, LOGGED (with /usr/local/bin/ un-mapped) so a test reads
# their order, and refusing a write under the fake fs while the root is read-only.
_LOGGED_FILE_TOOL = r'''
import os, sys
real = __REAL__
st = os.environ["FAKE_STATE"]
fs = os.path.join(st, "fs")
tool = os.path.basename(sys.argv[0])
args = [a.replace(fs, "") for a in sys.argv[1:]]
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write(tool + " " + " ".join(args) + f" root={root}\n")
if tool != "sha256sum" and root != "rw" and any(a.startswith(fs) for a in sys.argv[1:]):
    sys.stderr.write(f"{tool}: cannot write: Read-only file system\n")
    sys.exit(1)
os.execv(real, [real] + sys.argv[1:])
'''


def _log_file_tools(box):
    for tool in ("mv", "rm", "chmod", "sha256sum"):
        real = shutil.which(tool, path="/usr/bin:/bin")
        stub = box["stub"] / tool
        stub.unlink()
        stub.write_text(f"#!{sys.executable}\n" + _LOGGED_FILE_TOOL.replace("__REAL__", repr(real)))
        stub.chmod(0o755)


_OLD_CAMERA_BOX = b"#!/bin/bash\necho 'camera-box 1.0.0-old'\n"


def _deploy(tmp_path, args, enabled=None, deadman=False, live=None, **fake):
    """Run the REAL deploy-fleet.sh over CAMERA_SET=cam2 against the fake box. LIVE = the bytes of
    the camera-box binary already installed on the box."""
    box = make_box(tmp_path, root="ro")
    _log_file_tools(box)
    st = box["state"]
    (st / "fs" / "usr" / "local" / "bin").mkdir(parents=True)
    if live is not None:
        (st / "fs" / "usr" / "local" / "bin" / "camera-box").write_bytes(live)
    if enabled:
        (st / "enabled-cam2-painter.service").write_text("enabled\n")
        (st / "active-cam2-painter.service").write_text("active\n")
    if deadman:
        (st / "active-cam2-painter-deadman.timer").write_text("active\n")
    dev1 = _dev1_bin(tmp_path, box)
    env = {"PATH": f"{dev1}:/usr/bin:/bin", "FAKE_STATE": str(st), "FAKE_BOX_PATH": str(box["stub"]),
           "CAMERA_SET": "cam2", "SSH_PASS": "x", "GENLOCK_WAIT_TRIES": "1", "GENLOCK_WAIT_SECS": "0",
           "HOME": str(tmp_path)}
    env.update({k: v for k, v in fake.items() if v is not None})
    proc = subprocess.run(["bash", str(_DEPLOY), *args], env=env, capture_output=True, text=True, timeout=120)
    return proc, box


def _camera_box_artifact(tmp_path):
    a = tmp_path / "camera-box-artifact"
    a.write_text("#!/bin/bash\necho 'camera-box 9.9.9-test'\n")
    a.chmod(0o755)
    return a


# ---- deploy-fleet.sh: the camera-box swap ------------------------------------------------------- #


def _live_bin(box, name="camera-box"):
    return box["state"] / "fs" / "usr" / "local" / "bin" / name


def test_deploy_fleet_camera_box_starts_only_after_the_verified_close(tmp_path):
    artifact = _camera_box_artifact(tmp_path)
    proc, box = _deploy(tmp_path, ["--binary", str(artifact)], live=_OLD_CAMERA_BOX)
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "FLEET ALIGNED" in proc.stdout, proc.stdout
    assert starts_on_rw(calls) == [], "camera-box started on a writable root:\n" + "\n".join(calls)
    # design addendum item 2: the copy lands in a SIDECAR, is byte-verified THERE, and only then goes
    # live by one atomic rename -- all inside the window; the start waits for the verified close
    rw = index(calls, "mount -o remount,rw /")
    stop = index(calls, "systemctl stop camera-box root=rw")
    scp = index(calls, "SCP /usr/local/bin/camera-box.new root=rw")
    verify = index(calls, "sha256sum /usr/local/bin/camera-box.new root=rw")
    swap = index(calls, "mv -f /usr/local/bin/camera-box.new /usr/local/bin/camera-box root=rw")
    ro = index(calls, "mount -o remount,ro / root=rw")
    read = index(calls, "findmnt -no OPTIONS / root=ro")
    start = index(calls, "systemctl start camera-box root=ro")
    assert rw < stop < scp < verify < swap < ro < read < start, "\n".join(calls)
    assert root(box) == "ro"
    assert [c for c in calls if c.startswith("SCP /usr/local/bin/camera-box root")] == [], (
        "the copy must never write the live binary in place:\n" + "\n".join(calls))
    assert _live_bin(box).read_bytes() == artifact.read_bytes()
    assert not _live_bin(box, "camera-box.new").exists(), "the sidecar is renamed away, never left behind"


def test_deploy_fleet_camera_box_partial_copy_never_reaches_the_live_binary(tmp_path):
    # design addendum item 2: a transfer that dies half-way used to leave half a binary at the LIVE
    # path (scp writes in place). The half-copy now lands in the sidecar and never goes live: the
    # old binary is still whole, the sidecar is removed inside the window, and the OLD camera-box is
    # started again, on a verified read-only root, with the box FAILED.
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], live=_OLD_CAMERA_BOX,
                        FAKE_SCP_PARTIAL="1")
    calls = log(box)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "cam2(scp-failed)" in out, out
    assert _live_bin(box).read_bytes() == _OLD_CAMERA_BOX, "a partial copy must never reach the live binary"
    assert not _live_bin(box, "camera-box.new").exists(), "the partial sidecar is removed:\n" + "\n".join(calls)
    assert [c for c in calls if c.startswith("mv ")] == [], "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert index(calls, "findmnt -no OPTIONS / root=ro") < index(calls, "systemctl start camera-box root=ro")
    assert root(box) == "ro"


def test_deploy_fleet_camera_box_corrupt_sidecar_is_never_moved_into_place(tmp_path):
    # design addendum item 2: a copy that "succeeds" with the wrong bytes fails the SIDECAR's byte
    # verify; it is never renamed over the live binary, which keeps the old, whole build.
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], live=_OLD_CAMERA_BOX,
                        FAKE_SCP_CORRUPT="1")
    calls = log(box)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert "cam2(sidecar-sha-mismatch)" in out, out
    assert _live_bin(box).read_bytes() == _OLD_CAMERA_BOX, "a corrupt sidecar must never go live"
    assert not _live_bin(box, "camera-box.new").exists(), "the corrupt sidecar is removed:\n" + "\n".join(calls)
    assert [c for c in calls if c.startswith("mv ")] == [], "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert index(calls, "findmnt -no OPTIONS / root=ro") < index(calls, "systemctl start camera-box root=ro")
    assert root(box) == "ro"


def test_deploy_fleet_camera_box_failed_close_fails_the_box_names_the_writer_and_starts_nothing(tmp_path):
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], FAKE_RO_FAIL="1")
    calls = log(box)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert [c for c in calls if c.startswith("systemctl start")] == [], "\n".join(calls)
    assert "cam2's root is NOT read-only" in out, out
    assert "FLEET NOT FULLY ALIGNED" in out, out
    assert "cam2(root-rw: systemd-journal[76355]; relay[4242] /usr/local/bin/bkshading-relay)" in out, (
        "the FAILED entry carries the holder line:\n" + out)


def test_deploy_fleet_camera_box_failed_scp_closes_before_the_restart(tmp_path):
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], FAKE_SCP_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "cam2(scp-failed)" in proc.stdout + proc.stderr
    assert starts_on_rw(calls) == [], "\n".join(calls)
    ro = index(calls, "mount -o remount,ro /")
    start = index(calls, "systemctl start camera-box root=ro")
    assert ro < start, "\n".join(calls)
    assert root(box) == "ro"


def test_deploy_fleet_camera_box_failed_stop_still_puts_the_root_back_ro(tmp_path):
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], FAKE_STOP_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "cam2(stop-failed)" in proc.stdout + proc.stderr
    assert root(box) == "ro", "a failed stop must not leave the root writable:\n" + "\n".join(calls)
    assert [c for c in calls if c.startswith("SCP")] == [], "\n".join(calls)


def test_deploy_fleet_interrupted_mid_swap_closes_the_window_and_starts_nothing(tmp_path):
    # review round 1: a SIGTERM (a CI cancel) between the rw+stop and the close must never leave the
    # cambox on a writable root silently: the EXIT path closes the open window, verified.
    import signal
    import time
    box = make_box(tmp_path, root="ro")
    st = box["state"]
    (st / "fs" / "usr" / "local" / "bin").mkdir(parents=True)
    dev1 = _dev1_bin(tmp_path, box)
    env = {"PATH": f"{dev1}:/usr/bin:/bin", "FAKE_STATE": str(st), "FAKE_BOX_PATH": str(box["stub"]),
           "CAMERA_SET": "cam2", "SSH_PASS": "x", "GENLOCK_WAIT_TRIES": "1", "GENLOCK_WAIT_SECS": "0",
           "HOME": str(tmp_path), "FAKE_SCP_SLEEP": "2"}
    p = subprocess.Popen(["bash", str(_DEPLOY), "--binary", str(_camera_box_artifact(tmp_path))], env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    for _ in range(200):
        if "SCP " in (st / "log").read_text():
            break
        time.sleep(0.05)
    p.send_signal(signal.SIGTERM)
    out, err = p.communicate(timeout=60)
    calls = log(box)
    assert p.returncode != 0, out + err
    assert root(box) == "ro", "an interrupted swap must not leave the root writable:\n" + "\n".join(calls)
    assert [c for c in calls if c.startswith("systemctl start")] == [], "\n".join(calls)
    assert "interrupted" in err, err


def test_deploy_fleet_interrupted_during_the_close_closes_it_again(tmp_path):
    # review round 2: a Ctrl-C to the whole process group while the close's ssh runs kills that ssh
    # before the box ran the close; the open-window marker must survive until the box answered, so
    # the EXIT path closes it again (in its own session, out of reach of a second Ctrl-C).
    import signal
    import time
    box = make_box(tmp_path, root="ro")
    st = box["state"]
    (st / "fs" / "usr" / "local" / "bin").mkdir(parents=True)
    dev1 = _dev1_bin(tmp_path, box)
    env = {"PATH": f"{dev1}:/usr/bin:/bin", "FAKE_STATE": str(st), "FAKE_BOX_PATH": str(box["stub"]),
           "CAMERA_SET": "cam2", "SSH_PASS": "x", "GENLOCK_WAIT_TRIES": "1", "GENLOCK_WAIT_SECS": "0",
           "HOME": str(tmp_path), "FAKE_CLOSE_SLEEP": "5"}
    p = subprocess.Popen(["bash", str(_DEPLOY), "--binary", str(_camera_box_artifact(tmp_path))], env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    for _ in range(300):
        if (st / "close-wait").exists():
            break
        time.sleep(0.05)
    os.killpg(p.pid, signal.SIGINT)
    out, err = p.communicate(timeout=60)
    calls = log(box)
    assert p.returncode != 0, out + err
    assert root(box) == "ro", "the interrupted close must be run again:\n" + "\n".join(calls) + err
    assert [c for c in calls if c.startswith("systemctl start")] == [], "\n".join(calls)
    assert "may be STOPPED" in err, err


# ---- deploy-fleet.sh: the cam2 frame-probe swap ------------------------------------------------- #


def _probe(tmp_path):
    p = tmp_path / "frame-probe-artifact"
    p.write_bytes(b"FRAME-PROBE-1407\n")
    return p


def test_deploy_fleet_painter_enables_inside_and_starts_after_the_verified_close(tmp_path):
    proc, box = _deploy(tmp_path, ["--frame-probe", str(_probe(tmp_path))], enabled=True, deadman=True)
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "frame-probe byte-verify OK" in proc.stdout + proc.stderr
    assert not any("--now" in c for c in calls), "never `enable --now` inside the window:\n" + "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    enable = index(calls, "systemctl enable cam2-painter.service root=rw")
    ro = index(calls, "mount -o remount,ro / root=rw")
    read = index(calls, "findmnt -no OPTIONS / root=ro")
    start = index(calls, "systemctl start cam2-painter.service root=ro")
    rearm = index(calls, "systemd-run")
    assert enable < ro < read < start < rearm, "\n".join(calls)
    assert root(box) == "ro"


def test_deploy_fleet_painter_failed_close_starts_nothing_and_never_rearms_the_deadman(tmp_path):
    proc, box = _deploy(tmp_path, ["--frame-probe", str(_probe(tmp_path))], enabled=True, deadman=True,
                        FAKE_RO_FAIL="1")
    calls = log(box)
    out = proc.stdout + proc.stderr
    assert proc.returncode != 0, out
    assert starts(calls) == [], "a root left rw: no painter start, no deadman re-arm:\n" + "\n".join(calls)
    assert "cam2-painter(root-rw: systemd-journal[76355]" in out, out
    assert "FRAME-PROBE DEPLOY FAILED" in out, out


def test_deploy_fleet_dark_painter_stays_dark_and_the_root_goes_back_ro(tmp_path):
    proc, box = _deploy(tmp_path, ["--frame-probe", str(_probe(tmp_path))], enabled=False)
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert starts(calls) == [], "#892: an event-mode painter is never started:\n" + "\n".join(calls)
    assert index(calls, "findmnt -no OPTIONS / root=ro") > index(calls, "SCP /usr/local/bin/frame-probe.new")
    assert root(box) == "ro"


def test_deploy_fleet_painter_failed_scp_closes_before_the_restore(tmp_path):
    proc, box = _deploy(tmp_path, ["--frame-probe", str(_probe(tmp_path))], enabled=True, deadman=True,
                        FAKE_SCP_RC="1")
    calls = log(box)
    assert proc.returncode != 0
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert index(calls, "mount -o remount,ro /") < index(calls, "systemctl start cam2-painter.service root=ro")
    assert root(box) == "ro"


# ---- the dantesync Linux upgrade + rollback programs --------------------------------------------- #


def _dantesync_box(tmp_path, running="the 1.15.0 binary\n", staged="the 1.16.0 binary\n"):
    box = make_box(tmp_path, root="ro")
    bindir = tmp_path / "usrbin"
    bindir.mkdir()
    (bindir / "dantesync").write_text(running)
    (bindir / "dantesync.bak").write_text("the pre-upgrade binary\n")
    st = tmp_path / "staged"
    st.write_text(staged)
    sha = hashlib.sha256(staged.encode()).hexdigest()
    (tmp_path / "staged.sha256").write_text(f"{sha}  dantesync-linux-amd64\n")
    fake = box["stub"] / "dantesync"
    fake.write_text(f"#!{sys.executable}\nimport os, sys\nst = os.environ['FAKE_STATE']\n"
                    "root = open(os.path.join(st, 'root')).read().strip()\n"
                    "open(os.path.join(st, 'log'), 'a').write('dantesync ' + ' '.join(sys.argv[1:]) + f' root={root}\\n')\n"
                    "print('dantesync 1.16.0')\n")
    fake.chmod(0o755)
    return box, bindir, st


def _dantesync_text(tmp_path, bindir, staged, fn):
    body = (f"DANTESYNC_LINUX_BIN='{bindir / 'dantesync'}'\n"
            f"DANTESYNC_LINUX_BAK='{bindir / 'dantesync.bak'}'\n"
            f"DANTESYNC_LINUX_STAGED='{staged}'\n"
            f"DANTESYNC_LINUX_DATE_STATE='{tmp_path / 'date-offset.json'}'\n{fn}")
    return build(f'set +e\n. "{_UPGRADE}"\nset -e\n{body}')


def _run_program(box, text, **fake):
    script = box["state"] / "program.sh"
    script.write_text(text)
    return subprocess.run(["/bin/bash", str(script)], env=box_env(box, **fake),
                          capture_output=True, text=True, timeout=60)


def test_dantesync_upgrade_restarts_only_after_the_verified_close(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_upgrade_cmd 1.16.0"))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert (bindir / "dantesync").read_text() == "the 1.16.0 binary\n"
    assert starts_on_rw(calls) == [], "\n".join(calls)
    stop = index(calls, "systemctl stop dantesync root=rw")
    ro = index(calls, "mount -o remount,ro / root=rw")
    restart = index(calls, "systemctl restart dantesync root=ro")
    version = index(calls, "dantesync --version root=ro")
    assert stop < ro < restart < version, "\n".join(calls)
    assert root(box) == "ro"


def test_dantesync_upgrade_failed_close_never_restarts_and_names_the_writer(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_upgrade_cmd 1.16.0"),
                        FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert [c for c in calls if c.startswith(("systemctl start", "systemctl restart"))] == [], "\n".join(calls)
    assert "root is NOT read-only" in proc.stderr and "systemd-journal" in proc.stderr, proc.stderr
    assert "dantesync is 'inactive' now" in proc.stderr, (
        "the failure reads whether the clock daemon runs (it was stopped for the swap):\n" + proc.stderr)


def test_dantesync_upgrade_self_heal_reopens_the_window_and_restarts_on_ro(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_upgrade_cmd 1.16.0"),
                        FAKE_START_RC="1", FAKE_START_ONCE="1")
    calls = log(box)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert "SELF-HEAL: restored previous dantesync binary" in proc.stderr, proc.stderr
    assert (bindir / "dantesync").read_text() == "the 1.15.0 binary\n", "the .bak came back"
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert sum(c.startswith("mount -o remount,rw /") for c in calls) == 2, "the restore opens its own window"
    assert root(box) == "ro", "\n".join(calls)


def test_dantesync_rollback_restarts_only_after_the_verified_close(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_rollback_cmd"))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert (bindir / "dantesync").read_text() == "the pre-upgrade binary\n"
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert index(calls, "mount -o remount,ro / root=rw") < index(calls, "systemctl restart dantesync root=ro")
    assert root(box) == "ro"


def test_dantesync_rollback_failed_close_never_restarts(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_rollback_cmd"),
                        FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert [c for c in calls if c.startswith(("systemctl start", "systemctl restart"))] == [], "\n".join(calls)
    assert "root is NOT read-only" in proc.stderr, proc.stderr


def test_dantesync_master_downgrade_deletes_the_date_state_in_its_own_verified_window(tmp_path):
    box, bindir, staged = _dantesync_box(tmp_path)
    date = tmp_path / "date-offset.json"
    date.write_text('{"version":1}\n')
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged,
                                             "dantesync_linux_upgrade_cmd 1.14.0 ntp-master"))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert not date.exists(), "the date state is removed"
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert sum(c.startswith("mount -o remount,rw /") for c in calls) == 2, "\n".join(calls)
    assert sum(c.startswith("findmnt -no OPTIONS / root=ro") for c in calls) >= 2, "both windows verified"
    assert root(box) == "ro"


def test_dantesync_self_heal_that_cannot_restore_the_bak_says_so(tmp_path):
    # review round 1: a .bak that cannot be copied back must never read "restored".
    box, bindir, staged = _dantesync_box(tmp_path)
    inst = box["stub"] / "install"
    inst.unlink()
    inst.write_text(f"#!{sys.executable}\nimport os, sys\nos.remove({str(bindir / 'dantesync.bak')!r})\nsys.exit(1)\n")
    inst.chmod(0o755)
    proc = _run_program(box, _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_upgrade_cmd 1.16.0"))
    calls = log(box)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert "SELF-HEAL FAILED" in proc.stderr, proc.stderr
    assert "SELF-HEAL: restored previous dantesync binary" not in proc.stderr, proc.stderr
    assert starts_on_rw(calls) == [] and root(box) == "ro", "\n".join(calls)


def _upgrade_node(tmp_path, remote_out):
    """Run the orchestrator's REAL upgrade_node (cut out of the flow section, which a sourced script
    never reaches; its header is a count-1 anchor) with run_upgrade stubbed to fail with REMOTE_OUT."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from dantesync_upgrade_harness_1372 import _source  # noqa: E402
    text = _UPGRADE.read_text()
    head = "\nupgrade_node() {\n"
    assert text.count(head) == 1, "upgrade_node() must be defined exactly once"
    start = text.index(head) + 1
    fn = text[start:text.index("\n}\n", start) + 3]
    body = ("TARGET=1.11.0; FORCE=0\n"
            "log() { echo \"$*\"; }\nerr() { echo \"$*\" >&2; }\n"
            "read_node_version() { echo 1.10.0; }\n"
            f"run_upgrade() {{ REMOTE_OUT={remote_out!r}; return 1; }}\n"
            f"{fn}\n"
            "upgrade_node cam1 linux root@192.0.2.1; echo rc=$?")
    return _source(tmp_path, body)


def test_dantesync_orchestrator_names_a_failed_close_never_self_healed(tmp_path):
    # review round 1: a program that stopped at a failed ro close did NOT self-heal -- dantesync can
    # be left stopped there, and the roll log must say so.
    out = ("FAIL: [issue 1407] CAM1's root is NOT read-only after the remount-rw window ('findmnt -no "
           "OPTIONS /' = 'rw,relatime' -> rw; the ro remount rc=32: busy; sync rc=0). No dantesync start "
           "runs on a writable root; dantesync is 'inactive' now.")
    r = _upgrade_node(tmp_path, out)
    assert "rc=1" in r.stdout, r.stdout + r.stderr
    assert "NOT self-healed" in r.stderr, r.stderr
    assert "self-healed to its previous version" not in r.stderr, r.stderr
    r = _upgrade_node(tmp_path, "SELF-HEAL: restored previous dantesync binary")
    assert "self-healed to its previous version (not rolled forward)" in r.stderr, r.stderr


def test_dantesync_orchestrator_names_a_failed_self_heal_never_self_healed(tmp_path):
    # review round 2: a self-heal that could not copy the .bak back is no self-heal either.
    r = _upgrade_node(tmp_path, "SELF-HEAL FAILED: the previous binary could NOT be copied back")
    assert "rc=1" in r.stdout, r.stdout + r.stderr
    assert "self-healed to its previous version" not in r.stderr, r.stderr
    assert "could NOT restore the previous binary" in r.stderr, r.stderr


def test_dantesync_err_self_heal_is_disarmed_before_the_master_date_state_delete(tmp_path):
    # review round 2: the ERR self-heal covers the swap and the version read, never the date-state
    # delete after them (a roll-back to the 1.15 .bak after its date file was deleted is the state
    # the issue-1372 delete-last order exists to avoid).
    box, bindir, staged = _dantesync_box(tmp_path)
    text = _dantesync_text(tmp_path, bindir, staged, "dantesync_linux_upgrade_cmd 1.14.0 ntp-master")
    version = text.index("\ndantesync --version\n")
    disarm = text.index("\ntrap - ERR\n", version)
    rm = text.index('rm -f "' + str(tmp_path / "date-offset.json") + '"')
    assert version < disarm < rm, text


# ---- bkshading-relay-mode.sh ---------------------------------------------------------------------- #

_RELAY = LIB / "bkshading-relay-mode.sh"


def test_relay_mode_event_starts_the_relay_only_after_the_verified_close(tmp_path):
    box = make_box(tmp_path, root="ro")
    proc = run_text(box, build(f'. "{_RELAY}"\nbkshading_relay_mode_start_cmds'))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "RELAY_ENABLED=enabled" in proc.stdout
    assert starts_on_rw(calls) == [], "\n".join(calls)
    enable = index(calls, "systemctl enable bkshading-relay.service root=rw")
    read = index(calls, "findmnt -no OPTIONS / root=ro")
    start = index(calls, "systemctl start bkshading-relay.service root=ro")
    assert enable < read < start, "\n".join(calls)


def test_relay_mode_event_failed_close_starts_nothing(tmp_path):
    box = make_box(tmp_path, root="ro")
    proc = run_text(box, build(f'. "{_RELAY}"\nbkshading_relay_mode_start_cmds'), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode == 1, proc.stderr
    assert starts(calls) == [], "\n".join(calls)
    lines = (proc.stdout + proc.stderr).splitlines()
    assert [ln for ln in lines if ln.startswith("FAIL")][-1].endswith("rig-mode.sh event."), (
        "the LAST FAIL line (the one rig-mode relays) says what to do:\n" + proc.stderr)
    assert "systemd-journal" in proc.stderr


_RELAY_SSHPASS = r'''
import os, subprocess, sys
env = {"PATH": os.environ["FAKE_BOX_PATH"], "FAKE_STATE": os.environ["FAKE_STATE"], "HOME": os.environ["FAKE_STATE"]}
env.update({k: v for k, v in os.environ.items() if k.startswith("FAKE_")})
sys.exit(subprocess.run(["/bin/bash", "-c", sys.argv[-1]], env=env).returncode)
'''


def test_relay_mode_apply_names_the_writers_on_its_one_fail_line(tmp_path):
    # review round 1: bkshading_relay_mode_apply relays ONE line per box; a failed close must still
    # name the writers there (the box's own FAIL lines are not shown by rig-mode).
    box = make_box(tmp_path, root="ro")
    dev1 = tmp_path / "dev1-bin"
    dev1.mkdir()
    (dev1 / "sshpass").write_text(f"#!{sys.executable}\n{_RELAY_SSHPASS}")
    (dev1 / "sshpass").chmod(0o755)
    env = {"PATH": f"{dev1}:/usr/bin:/bin", "FAKE_STATE": str(box["state"]), "FAKE_BOX_PATH": str(box["stub"]),
           "FAKE_RO_FAIL": "1", "HOME": str(tmp_path)}
    proc = subprocess.run(["/bin/bash", "-c", f'. "{_RELAY}"\nbkshading_relay_mode_apply event pw cam1=192.0.2.1'],
                          env=env, capture_output=True, text=True, timeout=60)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    line = [ln for ln in proc.stderr.splitlines() if "bkshading-relay" in ln and "FAIL" in ln]
    assert line and "holders: systemd-journal[76355]; relay[4242]" in line[-1], proc.stderr


def test_relay_mode_test_disables_inside_and_verifies_the_close(tmp_path):
    box = make_box(tmp_path, root="ro")
    (box["state"] / "enabled-bkshading-relay.service").write_text("enabled\n")
    proc = run_text(box, build(f'. "{_RELAY}"\nbkshading_relay_mode_stop_cmds'))
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "RELAY_ENABLED=disabled" in proc.stdout
    assert index(calls, "systemctl disable bkshading-relay.service root=rw") < index(calls, "findmnt -no OPTIONS / root=ro")
    assert starts(calls) == [] and root(box) == "ro"


def test_relay_mode_test_failed_close_fails_loud(tmp_path):
    box = make_box(tmp_path, root="ro")
    (box["state"] / "enabled-bkshading-relay.service").write_text("enabled\n")
    proc = run_text(box, build(f'. "{_RELAY}"\nbkshading_relay_mode_stop_cmds'), FAKE_RO_FAIL="1")
    assert proc.returncode == 1
    assert "root is NOT read-only" in proc.stderr


# ---- ndi-discovery --cambox-apply ------------------------------------------------------------------ #

_NDI = LIB / "ndi-discovery.sh"


def _ndi(tmp_path, **fake):
    box = make_box(tmp_path, root="ro")
    py = box["stub"] / "python3"
    if not py.exists():
        py.symlink_to(sys.executable)
    env = {"NDI_DISCOVERY_SYSTEM_DIR": str(tmp_path / "etc-ndi"),
           "NDI_DISCOVERY_CAMBOX_DROPIN": str(tmp_path / "camera-box-ndi-discovery.conf")}
    (tmp_path / "etc-ndi").mkdir()
    (tmp_path / "etc-ndi" / "ndi-config.v1.json").write_text(
        '{"ndi": {"networks": {"ips": "10.77.9.202"}}}\n')
    (tmp_path / "camera-box-ndi-discovery.conf").write_text(
        build(f'. "{_NDI}"\nndi_discovery_dropin_content', env=env))
    text = build(f'. "{_NDI}"\nndi_discovery_cambox_apply_remote_snippet', env=env)
    return box, run_text(box, text, **fake), tmp_path


def test_ndi_apply_closes_the_window_verified_before_the_daemon_reload(tmp_path):
    box, proc, tmp = _ndi(tmp_path)
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert not (tmp / "camera-box-ndi-discovery.conf").exists()
    read = index(calls, "findmnt -no OPTIONS / root=ro")
    reload_ = index(calls, "systemctl daemon-reload root=ro")
    assert read < reload_, "\n".join(calls)
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "no retry loop"
    assert root(box) == "ro"


def test_ndi_apply_failed_close_names_the_writer_and_never_reloads(tmp_path):
    box, proc, _ = _ndi(tmp_path, FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "systemd-journal" in proc.stderr and "root is NOT read-only" in proc.stderr, proc.stderr
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "one try, never a retry:\n" + "\n".join(calls)
    assert not any("daemon-reload" in c for c in calls), "\n".join(calls)
    assert "OK" not in proc.stdout


# ---- rt-kernel-plan.sh: the printed runbook programs ---------------------------------------------- #
# design addendum item 3: the planner PRINTS (never runs) the supervisor's per-step commands for the
# reboot-class kernel upgrade. Each mutating step used to end `&& mount -o remount,ro /`: an
# unverified close, skipped entirely when an earlier `&&` step failed. Each now prints ONE
# self-contained program that closes with the shared verified close (ro-window.sh) whatever the
# work did, and reports the work's own failure after it. These tests run the text AS PRINTED.

_RT = LIB / "rt-kernel-plan.sh"

# box-side tools the steps run, logged with the root mode; FAKE_<TOOL>_RC makes one fail
_RT_TOOL = r'''
import os, sys
st = os.environ["FAKE_STATE"]
tool = os.path.basename(sys.argv[0])
root = open(os.path.join(st, "root")).read().strip()
with open(os.path.join(st, "log"), "a") as f:
    f.write(tool + " " + " ".join(sys.argv[1:]) + f" root={root}\n")
knob = "FAKE_" + tool.upper().replace("-", "_")
if os.environ.get(knob + "_STDIN"):  # a tool that reads its stdin (a dpkg/debconf prompt)
    sys.stdin.read()
rc = os.environ.get(knob + "_RC")
sys.exit(int(rc) if rc else 0)
'''

# the placeholders the supervisor replaces before pasting a step, with a stand-in each
_RT_PLACEHOLDERS = {"<OLD_VER>": "6.8.0-134-generic", "<Advanced...>the new kernel>": "gnulinux-advanced>gnulinux-6.11"}


def _rt_filled(text):
    for placeholder, value in _RT_PLACEHOLDERS.items():
        text = text.replace(placeholder, value)
    return text

# every step that changes the box, with the first work command its program runs
_RT_STEPS = (
    ("install-lowlatency", "", "apt-get install"),
    ("grub-pin:saved", "", "grub-set-default"),
    ("safe-grub-regen", "", "update-grub"),
    ("purge-superseded-generic", "", "apt-get purge"),
    ("purge-superseded-generic", "6.8.0-134-generic,linux-image-generic", "apt-get purge"),
    ("blocked:no-rt-candidate", "", "apt-get update"),
)


def _rt_box(tmp_path):
    box = make_box(tmp_path, root="ro")
    for tool in ("apt-get", "update-grub", "update-initramfs", "grub-set-default", "mkdir"):
        p = box["stub"] / tool
        p.write_text(f"#!{sys.executable}\n{_RT_TOOL}")
        p.chmod(0o755)
    return box


def _rt_text(token, stale=""):
    return build(f'. "{_RT}"\nrt_kernel_step_command "$1" "$2"', token, stale)


def test_rt_kernel_runbook_steps_carry_the_shared_verified_close_never_a_bare_remount_ro():
    writers = build(f'. "{LIB / "ro-window.sh"}"\nro_window_writers_cmd')
    mode = build(f'. "{LIB / "ro-root.sh"}"\ndeclare -f ro_root_mount_mode')
    for token, stale, _work in _RT_STEPS:
        text = _rt_text(token, stale)
        assert "&& mount -o remount,ro /" not in text, f"{token}: an unverified ro close:\n{text}"
        assert text.count("mount -o remount,ro /") == 1, f"{token}: exactly the ONE shared close:\n{text}"
        assert writers.strip() in text and mode.strip() in text, f"{token}: not the shared emitter's close:\n{text}"


def test_rt_kernel_runbook_steps_run_on_a_read_only_box_and_leave_it_read_only(tmp_path):
    for n, (token, stale, work) in enumerate(_RT_STEPS):
        d = tmp_path / str(n)
        d.mkdir()
        box = _rt_box(d)
        proc = run_text(box, _rt_filled(_rt_text(token, stale)))
        calls = log(box)
        assert proc.returncode == 0, f"{token}: {proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
        rw = index(calls, "mount -o remount,rw / root=ro")
        w = index(calls, work, rw)
        assert calls[w].endswith("root=rw"), f"{token}: the work runs inside the window:\n" + "\n".join(calls)
        ro = index(calls, "mount -o remount,ro / root=rw", w)
        index(calls, "findmnt -no OPTIONS / root=ro", ro)
        assert root(box) == "ro", f"{token}:\n" + "\n".join(calls)


def test_rt_kernel_runbook_step_whose_work_fails_still_puts_the_root_back_read_only(tmp_path):
    # the old `a && b && mount -o remount,ro /` never reached the remount once a or b failed
    box = _rt_box(tmp_path)
    proc = run_text(box, _rt_text("install-lowlatency"), FAKE_APT_GET_RC="100")
    calls = log(box)
    assert proc.returncode != 0, proc.stdout + proc.stderr
    assert root(box) == "ro", "a failed apt step must still close the window:\n" + "\n".join(calls)
    assert index(calls, "apt-get") < index(calls, "mount -o remount,ro / root=rw")
    fail = [ln for ln in proc.stderr.splitlines() if ln.startswith("FAIL") and "install-lowlatency" in ln]
    assert fail and "rc=100" in fail[-1], proc.stderr


def test_rt_kernel_runbook_step_on_a_busy_root_fails_loud_naming_the_writer(tmp_path):
    box = _rt_box(tmp_path)
    proc = run_text(box, _rt_text("safe-grub-regen"), FAKE_RO_FAIL="1")
    calls = log(box)
    assert proc.returncode != 0
    assert "root is NOT read-only" in proc.stderr and "systemd-journal" in proc.stderr, proc.stderr
    assert sum(c.startswith("mount -o remount,ro") for c in calls) == 1, "one try, never a retry:\n" + "\n".join(calls)


def test_rt_kernel_runbook_step_pasted_into_a_root_shell_never_ends_that_shell(tmp_path):
    # the supervisor pastes the text into an interactive ssh session on the box: a failed close
    # exits the step's OWN child shell, never the session it was pasted into
    box = _rt_box(tmp_path)
    proc = run_text(box, _rt_text("install-lowlatency") + "\necho SESSION-STILL-ALIVE", strict=":",
                    FAKE_RO_FAIL="1")
    assert "root is NOT read-only" in proc.stderr, proc.stderr
    assert "SESSION-STILL-ALIVE" in proc.stdout, proc.stdout + proc.stderr


def test_rt_kernel_runbook_step_whose_work_reads_stdin_still_runs_the_verified_close(tmp_path):
    # review round 1: the program is the stdin of `bash -s`, so a work command that reads stdin (a
    # dpkg conffile prompt, a debconf question) used to eat the rest of it -- the verified close
    # included -- and leave the root writable with exit 0. The work now reads /dev/null.
    box = _rt_box(tmp_path)
    proc = run_text(box, _rt_text("install-lowlatency"), FAKE_APT_GET_STDIN="1")
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert root(box) == "ro", "the close must still run after a stdin reader:\n" + "\n".join(calls)
    index(calls, "findmnt -no OPTIONS / root=ro", index(calls, "apt-get install"))
    assert "OK: [issue 899] install-lowlatency" in proc.stdout, proc.stdout + proc.stderr


def test_rt_kernel_runbook_purge_steps_never_wait_for_a_prompt():
    for stale in ("", "6.8.0-134-generic,linux-image-generic"):
        text = _rt_text("purge-superseded-generic", stale)
        assert "DEBIAN_FRONTEND=noninteractive apt-get purge" in text, text


def test_rt_kernel_runbook_step_with_an_unreplaced_placeholder_refuses_before_the_rw_window(tmp_path):
    # review round 1: the placeholder steps used to be inert notes; as programs they would run a
    # literal `<OLD_VER>` / `<Advanced...>` (a bogus grub saved_entry before a reboot). Unedited, they
    # refuse before the root is touched; edited, they run.
    for n, token in enumerate(("grub-pin:saved", "purge-superseded-generic")):
        d = tmp_path / str(n)
        d.mkdir()
        box = _rt_box(d)
        proc = run_text(box, _rt_text(token))
        calls = log(box)
        assert proc.returncode != 0, f"{token}: {proc.stdout}\n{proc.stderr}"
        assert not any(c.startswith("mount") for c in calls), f"{token}: the root was touched:\n" + "\n".join(calls)
        assert "placeholder" in proc.stderr, f"{token}: {proc.stderr}"
        d2 = tmp_path / f"{n}-filled"
        d2.mkdir()
        box2 = _rt_box(d2)
        proc2 = run_text(box2, _rt_filled(_rt_text(token)))
        assert proc2.returncode == 0, f"{token}: {proc2.stdout}\n{proc2.stderr}\n" + "\n".join(log(box2))
        assert root(box2) == "ro"


def test_rt_kernel_driver_prints_a_multi_line_step_below_its_token():
    # review round 1: `%-28s %s` put `bash -s <<'RT_KERNEL_STEP'` on the token's own line, so copying
    # that visible line ran `install-lowlatency bash -s`. The token now has a line of its own.
    proc = subprocess.run(["bash", str(ROOT / "scripts" / "rt-kernel-upgrade.sh"), "--facts", "0 0 1 saved 1",
                           "--commands"], capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr
    lines = proc.stdout.splitlines()
    for token in ("install-lowlatency", "safe-grub-regen"):
        i = lines.index(token)
        assert lines[i + 1] == "bash -s <<'RT_KERNEL_STEP'", lines[i:i + 3]
    assert not [ln for ln in lines if "bash -s <<" in ln and not ln.startswith("bash -s <<")], proc.stdout
    one_liner = [ln for ln in lines if ln.startswith("reboot-into-lowlatency")]
    assert one_liner and "# SUPERVISOR:" in one_liner[0], "single-line steps keep the token column"
