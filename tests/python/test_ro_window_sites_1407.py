"""issue 1407 -- every rw-window site RUN on a fake read-only-root box: nothing starts before the
verified close, and a close that leaves the root writable fails the step by name and starts nothing.

The fake box (tests/python/ro_window_fakes_1407.py) logs every call with the root mode at that moment
and plants a writer on / when something starts on a writable root (the modelled issue-1405 cause).
So `starts_on_rw(log) == []` is the issue-1405/1407 invariant itself, read from a real run:
- deploy-fleet.sh, run whole with a fake `sshpass` that executes each remote command on the fake box
  (the camera-box swap on a cambox, and the cam2 frame-probe swap in both #892 states);
- the dantesync Linux upgrade and rollback programs, run as emitted (a staged binary, real files);
- bkshading-relay-mode.sh's stop/start texts;
- the ndi-discovery `--cambox-apply` program.
bkshading-deploy-relay.sh is driven end to end by its own tests (test_bkshading_relay_gaps_808.py).
Tier-0 (#557): no cargo, no rig, no network.
"""
import hashlib
import os
import subprocess
import sys

import pytest

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
    if os.environ.get("FAKE_SCP_RC"):
        sys.exit(int(os.environ["FAKE_SCP_RC"]))
    if root != "rw":
        sys.stderr.write(f"scp: {dest}: Read-only file system\n")
        sys.exit(1)
    os.makedirs(os.path.dirname(mapped(dest)), exist_ok=True)
    shutil.copy(args[-2], mapped(dest))
    sys.exit(0)
env = {"PATH": os.environ["FAKE_BOX_PATH"], "FAKE_STATE": st, "HOME": st}
env.update({k: v for k, v in os.environ.items() if k.startswith("FAKE_")})
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


def _deploy(tmp_path, args, enabled=None, deadman=False, **fake):
    """Run the REAL deploy-fleet.sh over CAMERA_SET=cam2 against the fake box."""
    box = make_box(tmp_path, root="ro")
    st = box["state"]
    (st / "fs" / "usr" / "local" / "bin").mkdir(parents=True)
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


def test_deploy_fleet_camera_box_starts_only_after_the_verified_close(tmp_path):
    proc, box = _deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))])
    calls = log(box)
    assert proc.returncode == 0, f"{proc.stdout}\n{proc.stderr}\n" + "\n".join(calls)
    assert "FLEET ALIGNED" in proc.stdout, proc.stdout
    assert starts_on_rw(calls) == [], "camera-box started on a writable root:\n" + "\n".join(calls)
    rw = index(calls, "mount -o remount,rw /")
    stop = index(calls, "systemctl stop camera-box root=rw")
    scp = index(calls, "SCP /usr/local/bin/camera-box root=rw")
    ro = index(calls, "mount -o remount,ro / root=rw")
    read = index(calls, "findmnt -no OPTIONS / root=ro")
    start = index(calls, "systemctl start camera-box root=ro")
    assert rw < stop < scp < ro < read < start, "\n".join(calls)
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
