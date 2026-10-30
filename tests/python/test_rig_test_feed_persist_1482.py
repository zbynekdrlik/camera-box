"""issue 1482 -- the direct-feed TEST state survives a cam2 reboot (checklist item of issuecomment-6515260963).

Live 29.10.2026 18:58: cam2 rebooted, the two direct-feed drop-ins lived in /run (tmpfs) and were gone, so the
ENABLED permanent cam2-painter.service came back on the HDMI PCM (no speaker) and cam2's camera-box intercom took
the jack: the stream program heard the intercom, the strih-snv program-audio guard read FOREIGN and the E2E [0/8]
program-audio preflight refused (run 41001425945).

The drop-ins now live in /etc/systemd/system (persistent, on cam2's READ-ONLY root), so TEST writes them and EVENT
removes them through a remount-rw window closed by the ONE verified close (scripts/lib/ro-window.sh). A repeat TEST
whose drop-ins already match opens no window. The old /run copies are removed on every TEST and EVENT.
These tests RUN the emitted remote text on the issue-1407 fake read-only-root box; the write tools refuse a write
under the fake /etc while the fake root reads ro, so a write outside the window fails the run.
"""
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import LIB, index, log, make_box, root, run_text  # noqa: E402

FEED = LIB / "rig-test-feed.sh"
APLAY = ("card 0: C4K [CAM LINK 4K], device 0: USB Audio [USB Audio]\n"
         "card 1: HID [USB Audio and HID], device 0: USB Audio [USB Audio]\n"
         "card 2: PCH [HDA Intel PCH], device 3: HDMI 0 [GSV2K6 DEMO]\n")
PAINTER_WANT = "[Service]\nEnvironment=FRAME_PROBE_AUDIO_MARKER_DEVICE=hw:CARD=HID,DEV=0\n"
INTERCOM_WANT = "[Service]\nEnvironment=CAMERA_BOX_INTERCOM=off\n"

# A write tool that refuses (and logs) any write under the fake box's /etc while its root reads ro.
_GUARD = r'''
import os, sys
st = os.environ["FAKE_STATE"]
etc = os.environ["FAKE_ETC"]
tool = os.path.basename(sys.argv[0])
mode = open(os.path.join(st, "root")).read().strip()
touch = [a for a in sys.argv[1:] if a.startswith(etc)]
with open(os.path.join(st, "log"), "a") as f:
    f.write(f"{tool} {' '.join(sys.argv[1:])} root={mode}\n")
if touch and mode != "rw":
    sys.stderr.write(f"{tool}: Read-only file system: {touch[0]}\n")
    sys.exit(1)
os.execv("/bin/" + tool if os.path.exists("/bin/" + tool) else "/usr/bin/" + tool, [tool] + sys.argv[1:])
'''


def _box(tmp_path):
    box = make_box(tmp_path, root="ro")
    for tool in ("mv", "rm", "mkdir", "rmdir"):
        p = box["stub"] / tool
        if p.exists() or p.is_symlink():
            p.unlink()
        p.write_text(f"#!{sys.executable}\n{_GUARD}")
        p.chmod(0o755)
    (box["stub"] / "aplay").write_text("#!/bin/sh\ncat <<'X'\n%sX\n" % APLAY)
    (box["stub"] / "aplay").chmod(0o755)
    return box


def _paths(tmp_path):
    etc, run = tmp_path / "etc" / "systemd" / "system", tmp_path / "run" / "systemd" / "system"
    return {"RIG_TEST_FEED_PAINTER_DROPIN": str(etc / "cam2-painter.service.d" / "zz-rig-direct-marker.conf"),
            "RIG_TEST_FEED_INTERCOM_DROPIN": str(etc / "camera-box.service.d" / "zz-rig-direct-intercom-off.conf"),
            "RIG_TEST_FEED_PAINTER_DROPIN_LEGACY": str(run / "cam2-painter.service.d" / "zz-rig-direct-marker.conf"),
            "RIG_TEST_FEED_INTERCOM_DROPIN_LEGACY": str(run / "camera-box.service.d" / "zz-rig-direct-intercom-off.conf")}


def _emit(fn, env):
    e = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/tmp")}
    e.update(env)
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\n' + fn, "h", str(FEED)],
                       capture_output=True, text=True, env=e, timeout=30)
    assert r.returncode == 0 and "unbound variable" not in r.stderr, r.stderr
    return r.stdout


def _run(box, tmp_path, fn, feed="direct", **fake):
    env = dict(_paths(tmp_path), RIG_TEST_FEED=feed)
    return run_text(box, _emit(fn, env), strict="set -e", FAKE_ETC=str(tmp_path / "etc"), **fake)


def _plant_legacy(tmp_path):
    for k in ("RIG_TEST_FEED_PAINTER_DROPIN_LEGACY", "RIG_TEST_FEED_INTERCOM_DROPIN_LEGACY"):
        p = _paths(tmp_path)[k]
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write("[Service]\n")


def _mounts(lines):
    return [ln for ln in lines if ln.startswith("mount ")]


def test_the_drop_ins_default_to_etc_and_the_old_run_paths_are_the_legacy():
    out = _emit('printf "%s\\n" "$RIG_TEST_FEED_PAINTER_DROPIN" "$RIG_TEST_FEED_INTERCOM_DROPIN" '
                '"$RIG_TEST_FEED_PAINTER_DROPIN_LEGACY" "$RIG_TEST_FEED_INTERCOM_DROPIN_LEGACY"', {})
    assert out.splitlines() == [
        "/etc/systemd/system/cam2-painter.service.d/zz-rig-direct-marker.conf",
        "/etc/systemd/system/camera-box.service.d/zz-rig-direct-intercom-off.conf",
        "/run/systemd/system/cam2-painter.service.d/zz-rig-direct-marker.conf",
        "/run/systemd/system/camera-box.service.d/zz-rig-direct-intercom-off.conf"]


def test_direct_test_writes_both_drop_ins_inside_a_verified_window_and_drops_the_run_copies(tmp_path):
    box, p = _box(tmp_path), _paths(tmp_path)
    _plant_legacy(tmp_path)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        r = _run(box, tmp_path, fn)
        assert r.returncode == 0, r.stdout + r.stderr
        assert "survives a cam2 reboot" in r.stdout
    assert open(p["RIG_TEST_FEED_PAINTER_DROPIN"]).read() == PAINTER_WANT
    assert open(p["RIG_TEST_FEED_INTERCOM_DROPIN"]).read() == INTERCOM_WANT
    assert not os.path.exists(p["RIG_TEST_FEED_PAINTER_DROPIN_LEGACY"])
    assert not os.path.exists(p["RIG_TEST_FEED_INTERCOM_DROPIN_LEGACY"])
    lines = log(box)
    assert root(box) == "ro"
    assert [m.split(" root=")[0] for m in _mounts(lines)] == ["mount -o remount,rw /", "mount -o remount,ro /"] * 2
    # the painter's daemon-reload runs only after its window closed (nothing but writes inside the window)
    second_ro = [i for i, ln in enumerate(lines) if ln.startswith("mount -o remount,ro /")][1]
    assert index(lines, "systemctl daemon-reload") > second_ro


def test_a_repeat_test_with_the_drop_ins_in_place_opens_no_window(tmp_path):
    box = _box(tmp_path)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        assert _run(box, tmp_path, fn).returncode == 0
    before = len(log(box))
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        r = _run(box, tmp_path, fn)
        assert r.returncode == 0, r.stderr
        assert "already in place" in r.stdout
    assert _mounts(log(box)[before:]) == []


def test_a_changed_drop_in_is_rewritten(tmp_path):
    box, p = _box(tmp_path), _paths(tmp_path)
    assert _run(box, tmp_path, "rig_test_feed_painter_dropin_cmds").returncode == 0
    open(p["RIG_TEST_FEED_PAINTER_DROPIN"], "w").write("[Service]\nEnvironment=FRAME_PROBE_AUDIO_MARKER_DEVICE=hw:CARD=PCH,DEV=3\n")
    r = _run(box, tmp_path, "rig_test_feed_painter_dropin_cmds")
    assert r.returncode == 0, r.stderr
    assert open(p["RIG_TEST_FEED_PAINTER_DROPIN"]).read() == PAINTER_WANT


def test_event_removes_the_etc_and_run_drop_ins_in_one_verified_window(tmp_path):
    box, p = _box(tmp_path), _paths(tmp_path)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        assert _run(box, tmp_path, fn).returncode == 0
    _plant_legacy(tmp_path)
    before = len(log(box))
    r = _run(box, tmp_path, "rig_test_feed_painter_dropin_clear_cmds")
    assert r.returncode == 0, r.stdout + r.stderr
    for k in p:
        assert not os.path.exists(p[k]), k
    assert [m.split(" root=")[0] for m in _mounts(log(box)[before:])] == ["mount -o remount,rw /", "mount -o remount,ro /"]
    assert root(box) == "ro"
    # nothing to remove: no window at all
    before = len(log(box))
    assert _run(box, tmp_path, "rig_test_feed_painter_dropin_clear_cmds").returncode == 0
    assert _mounts(log(box)[before:]) == []


def test_the_optical_feed_removes_the_persistent_drop_ins(tmp_path):
    box, p = _box(tmp_path), _paths(tmp_path)
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        assert _run(box, tmp_path, fn).returncode == 0
    for fn in ("rig_test_feed_intercom_dropin_cmds", "rig_test_feed_painter_dropin_cmds"):
        r = _run(box, tmp_path, fn, feed="optical")
        assert r.returncode == 0, r.stdout + r.stderr
    assert not os.path.exists(p["RIG_TEST_FEED_PAINTER_DROPIN"])
    assert not os.path.exists(p["RIG_TEST_FEED_INTERCOM_DROPIN"])
    assert root(box) == "ro"


def test_a_root_left_writable_fails_loud_and_reloads_nothing(tmp_path):
    box = _box(tmp_path)
    r = _run(box, tmp_path, "rig_test_feed_painter_dropin_cmds", FAKE_RO_FAIL="1")
    assert r.returncode == 1
    assert "root is NOT read-only after the remount-rw window" in r.stderr
    assert not [ln for ln in log(box) if ln.startswith("systemctl daemon-reload")]


def test_a_refused_rw_remount_fails_loud_and_writes_nothing(tmp_path):
    box, p = _box(tmp_path), _paths(tmp_path)
    r = _run(box, tmp_path, "rig_test_feed_intercom_dropin_cmds", FAKE_RW_FAIL="1")
    assert r.returncode == 1
    assert "could not remount / read-write" in r.stderr
    assert not os.path.exists(p["RIG_TEST_FEED_INTERCOM_DROPIN"])
