#!/usr/bin/env python3
"""issue 1371 -- the test camera's PRODUCTION exposure: snapshot before the first test set, restore at
the EVENT switch.

Owner, 26.9.2026: "ked vypina sa development tak ze aj vratis iso a uzavierku naspat". The E2E
[0/8] enforce snapshots the camera's ISO + shutter right before the FIRST `--set-config` of a
development period (`~/.camera-box/camera-prod-exposure.json`); `rig-mode.sh`'s EVENT path restores
it, reads it back and moves it aside. Split out of `test_camera_test_settings_1371.py` (which keeps
the enforce step's own tests) and reusing its fake-camera harness: the bash lib is driven end to end
under the caller's `set -euo pipefail` with a fake `sshpass` that RUNS the remote text against
stub `systemctl` / `pgrep` / `gphoto2`, with a temp HOME so no test touches the real snapshot.
Runnable directly or under pytest (the `python-tests` CI job).
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
_hspec = importlib.util.spec_from_file_location("camera_test_settings_harness_1371",
                                                os.path.join(HERE, "test_camera_test_settings_1371.py"))
h = importlib.util.module_from_spec(_hspec)
_hspec.loader.exec_module(h)

# the shared harness (no test_* names, so pytest collects nothing twice)
cts = h.cts
REPO = h.REPO
MODULE = h.MODULE
Rig = h.Rig
GOOD = h.GOOD
_pinned = h._pinned
_blocks = h._blocks
_cli = h._cli
_tmp_baseline = h._tmp_baseline
_baseline_text = h._baseline_text


# ---------------------------------------------------------------------------------------------
# the production-exposure snapshot + the EVENT-switch restore (owner, 26.9.2026: "ked vypina sa
# development tak ze aj vratis iso a uzavierku naspat"). The E2E snapshots what the camera had right
# before its FIRST set of a development period; rig-mode EVENT puts it back and reads it back.
# ---------------------------------------------------------------------------------------------
RIG_MODE = os.path.join(REPO, "scripts", "rig-mode.sh")
PROD = {"schema": 1, "box": "cam1", "taken_utc": "2026-09-26T15:00:00Z",
        "values": {"iso": "800", "d002": "36000"}, "context": {"d007": "60"}}


def test_default_snapshot_path_is_in_the_runner_home_and_the_env_overrides_it():
    assert cts.default_snapshot_path({"HOME": "/h"}) == "/h/.camera-box/camera-prod-exposure.json"
    assert cts.default_snapshot_path({"HOME": "/h", cts.SNAPSHOT_ENV: "/x/s.json"}) == "/x/s.json"
    assert cts.SNAPSHOT_ENV == "CAMERA_PROD_EXPOSURE_SNAPSHOT"


def test_build_snapshot_records_the_values_the_test_is_about_to_overwrite():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "800", "d002": "36000", "f-number": "f/4", "d004": "5600", "d005": "0", "d007": "60"}
    doc = cts.build_snapshot(cur, b, "cam1", "2026-09-26T15:00:00Z")
    assert doc == PROD
    # a pinned optional key is recorded too; an unpinned one is never touched, so never restored
    b = cts.load_baseline(_baseline_text(_pinned(d004=3200)))
    assert cts.build_snapshot(cur, b, "cam1", "t")["values"] == {"iso": "800", "d002": "36000", "d004": "5600"}
    # the round trip is a valid snapshot
    assert cts.load_snapshot(json.dumps(doc)) == doc


def test_build_snapshot_refuses_a_value_it_could_not_restore():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "Auto ISO", "d002": "36000", "d007": "60"}
    try:
        cts.build_snapshot(cur, b, "cam1", "t")
    except cts.SnapshotError as e:
        assert "iso" in str(e)
    else:
        raise AssertionError("a value that is not a plain token must not be snapshotted")


def test_malformed_snapshots_are_refused():
    bad = [
        "not json",
        "[]",
        json.dumps(dict(PROD, schema=2)),
        json.dumps({k: v for k, v in PROD.items() if k != "values"}),
        json.dumps(dict(PROD, values={})),
        json.dumps(dict(PROD, values={"d007": "60"})),  # fps is never restored
        json.dumps(dict(PROD, values={"iso": "800; reboot"})),
        json.dumps(dict(PROD, values={"iso": 800})),
        json.dumps(dict(PROD, box="")),
        json.dumps({k: v for k, v in PROD.items() if k != "taken_utc"}),
    ]
    for text in bad:
        try:
            cts.load_snapshot(text)
        except cts.SnapshotError:
            continue
        raise AssertionError("snapshot should be refused: %s" % text)


def test_restore_plan_and_grade_touch_only_the_snapshot_keys_that_differ():
    snap = PROD["values"]
    assert cts.restore_plan(snap, {"iso": "8000", "d002": "2160", "d007": "60"}) == [("iso", "800"), ("d002", "36000")]
    assert cts.restore_plan(snap, {"iso": "800", "d002": "2160"}) == [("d002", "36000")]
    assert cts.restore_plan(snap, {"iso": "800", "d002": "36000"}) == []
    assert cts.grade_restore(snap, {"iso": "800", "d002": "36000"}) == []
    assert cts.grade_restore(snap, {"iso": "800", "d002": "2160"}) == [("d002", "36000", "2160")]


def test_snapshot_summary_is_plain_slovak_with_the_shutter_as_1_over_n():
    assert cts.snapshot_summary(PROD) == "ISO 800, uzávierka 1/60 s (uhol 360°), cam1 2026-09-26T15:00:00Z"
    no_fps = dict(PROD, context={})
    assert cts.snapshot_summary(no_fps) == "ISO 800, uzávierka uhol 360°, cam1 2026-09-26T15:00:00Z"
    odd = dict(PROD, values={"iso": "8000", "d002": "2160"})
    assert "1/1000 s (uhol 21.6°)" in cts.snapshot_summary(odd)


def test_consumed_path_and_the_newest_consumed_snapshot():
    p = "/h/.camera-box/camera-prod-exposure.json"
    assert cts.consumed_path(p, "20260926T160000Z") == "/h/.camera-box/camera-prod-exposure.consumed-20260926T160000Z.json"
    names = ["camera-prod-exposure.consumed-20260920T100000Z.json", "other.json",
             "camera-prod-exposure.consumed-20260926T160000Z.json", "camera-prod-exposure.json"]
    assert cts.newest_consumed(p, names) == "camera-prod-exposure.consumed-20260926T160000Z.json"
    assert cts.newest_consumed(p, ["other.json"]) is None


def test_snapshot_state_line_for_the_handover_check():
    root = tempfile.mkdtemp(prefix="cts1371-state-")
    try:
        p = os.path.join(root, "camera-prod-exposure.json")
        assert cts.snapshot_state(p) == "exposure state=none"
        with open(cts.consumed_path(p, "20260926T160000Z"), "w") as f:
            json.dump(PROD, f)
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=restored restored=20260926T160000Z box=cam1")
        assert "iso=800 d002=36000" in line
        with open(p, "w") as f:
            json.dump(PROD, f)
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=pending box=cam1 taken=2026-09-26T15:00:00Z")
        assert "iso=800 d002=36000" in line
        with open(p, "w") as f:
            f.write("{broken")
        assert cts.snapshot_state(p).startswith("exposure state=invalid ")
    finally:
        shutil.rmtree(root)


def test_cli_snapshot_writes_once_and_never_overwrites():
    root = tempfile.mkdtemp(prefix="cts1371-cli-")
    base = _tmp_baseline(_pinned())
    try:
        p = os.path.join(root, "sub", "camera-prod-exposure.json")
        r = _cli(["snapshot", "--baseline", base, "--snapshot", p, "--box", "cam1"],
                 _blocks({"iso": 800, "d002": 36000, "d007": 60}))
        assert r.returncode == 0, r.stderr
        assert "SNAPSHOT saved" in r.stdout
        doc = json.load(open(p))
        assert doc["values"] == {"iso": "800", "d002": "36000"} and doc["box"] == "cam1"
        r = _cli(["snapshot", "--baseline", base, "--snapshot", p, "--box", "cam2"],
                 _blocks({"iso": 8000, "d002": 2160, "d007": 60}))
        assert r.returncode == 0 and "SNAPSHOT kept" in r.stdout
        assert json.load(open(p)) == doc
        # an unrestorable value is refused and nothing is written
        q = os.path.join(root, "q.json")
        r = _cli(["snapshot", "--baseline", base, "--snapshot", q, "--box", "cam1"],
                 _blocks({"iso": "Auto ISO", "d002": 36000, "d007": 60}))
        assert r.returncode == cts.EXIT_SNAPSHOT_FAILED and not os.path.exists(q)
    finally:
        shutil.rmtree(root)
        os.unlink(base)


def test_cli_snapshot_path_honours_the_env_and_home():
    env = dict(os.environ, HOME="/tmp/cts1371-home")
    env.pop(cts.SNAPSHOT_ENV, None)
    r = subprocess.run(["python3", MODULE, "snapshot-path"], capture_output=True, text=True, env=env)
    assert r.stdout.strip() == "/tmp/cts1371-home/.camera-box/camera-prod-exposure.json"
    env[cts.SNAPSHOT_ENV] = "/tmp/x.json"
    r = subprocess.run(["python3", MODULE, "snapshot-path"], capture_output=True, text=True, env=env)
    assert r.stdout.strip() == "/tmp/x.json"


# --- the E2E side: the snapshot is taken before the FIRST set of a development period -----------
def test_the_first_set_snapshots_the_owners_exposure_before_changing_it():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="800", d002="36000"), _pinned()).run()
    assert r.rc == 0, r.out
    doc = json.loads(r.snapshot)
    assert doc["values"] == {"iso": "800", "d002": "36000"}
    assert doc["box"] == "cam1" and doc["context"] == {"d007": "60"}
    assert r.snapshot_at_set is True, "the snapshot must be on disk BEFORE the camera is changed"
    assert "SNAPSHOT saved" in r.out


def test_a_pending_snapshot_is_never_overwritten_by_a_later_run():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), snapshot=PROD).run()
    assert r.rc == 0, r.out
    assert json.loads(r.snapshot) == PROD
    assert "SNAPSHOT kept" in r.out


def test_a_camera_already_at_the_baseline_takes_no_snapshot():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run()
    assert r.rc == 0, r.out
    assert r.snapshot is None


def test_a_set_blocked_by_a_live_broadcast_takes_no_snapshot():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), busy=True).run()
    assert r.rc == 1, r.out
    assert r.snapshot is None


def test_an_unrestorable_camera_value_aborts_before_the_set():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="Auto ISO"), _pinned()).run()
    assert r.rc == 1, r.out
    assert "production exposure" in r.out and "iso" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert r.snapshot is None


def test_an_unwritable_snapshot_aborts_before_the_set():
    # ~/.camera-box is a FILE, so the snapshot cannot be written: the owner's exposure would be lost
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned())
    with open(r.snap_dir, "w") as f:
        f.write("not a dir")
    r.run()
    assert r.rc == 1, r.out
    assert "production exposure" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls


# --- the EVENT side: restore, read back, consume ------------------------------------------------
def _restore(camera, snapshot=PROD, **kw):
    return Rig({"10.77.9.61": 1}, camera, _pinned(), snapshot=snapshot, **kw).run(mode="restore")


def test_restore_without_a_snapshot_is_a_quiet_no_op():
    r = Rig({"10.77.9.61": 1}, dict(GOOD), _pinned()).run(mode="restore")
    assert r.rc == 0, r.out
    assert "AFTER-RESTORE rc=0 outcome=none" in r.out and "AFTER-NOTE" in r.out
    assert "nothing to restore" in r.out
    assert not any(c.startswith(("PRESENCE", "RELAYSTOP", "GET", "SET", "GUARD")) for c in r.calls), r.calls
    assert r.discord == "EVENT contract ok\n"


def test_restore_puts_the_owners_exposure_back_and_consumes_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"))
    assert r.rc == 0, r.out
    assert "AFTER-RESTORE rc=0 outcome=restored" in r.out
    # review round 1: the rig-busy guard runs BEFORE the relay stop and the camera session, and
    # again right before the set (each is a rig mutation during a possibly live broadcast)
    seq = [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")]
    assert seq == ["GUARD", "RELAYSTOP", "GET", "GUARD", "SET", "GET"], r.calls
    assert "SET 10.77.9.61 iso=800 d002=36000" in r.calls
    assert r.camera["iso"] == "800" and r.camera["d002"] == "36000"
    assert r.snapshot is None, "a verified restore moves the snapshot aside"
    assert len(r.consumed) == 1 and r.consumed_docs[0] == PROD
    assert "RESTORE iso 400 -> 800" in r.out and "RESTORED iso 800" in r.out
    assert "✅" in r.discord and "ISO 800" in r.discord and "1/60 s" in r.discord
    assert r.discord.startswith("EVENT contract ok\n"), r.discord


def test_restore_when_the_camera_already_has_the_production_values_sets_nothing():
    r = _restore(dict(GOOD, iso="800", d002="36000"))
    assert r.rc == 0, r.out
    assert "outcome=already" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert [c.split(" ")[0] for c in r.calls if not c.startswith("PRESENCE")] == ["GUARD", "RELAYSTOP", "GET"]
    assert r.snapshot is None and len(r.consumed) == 1


def test_restore_with_the_camera_absent_is_loud_and_keeps_the_snapshot():
    r = Rig({"10.77.9.61": 0, "10.77.9.62": 0}, dict(GOOD), _pinned(), snapshot=PROD).run(mode="restore")
    assert r.rc == 0, r.out  # the EVENT caller itself never aborts
    assert "outcome=failed" in r.out and "AFTER-NOTE" in r.out
    assert "NOT restored" in r.out and "cam1 (10.77.9.61)" in r.out
    assert "::warning" in r.out
    assert json.loads(r.snapshot) == PROD and r.consumed == []
    assert "⚠️" in r.discord and "NEVRÁTILA" in r.discord
    # the failure is ON TOP of the EVENT confirmation, not buried under a green message, and the
    # confirmation itself survives below it
    assert r.discord.startswith("⚠️"), r.discord
    assert "EVENT contract ok" in r.discord
    # setting the camera by hand must also move the snapshot aside, or the next EVENT overwrites it;
    # the command is for the run log -- the owner's phone line asks him to tell Claude instead
    assert "camera_test_settings.py consume" in r.out
    assert "camera_test_settings.py" not in r.discord and "Claud" in r.discord
    # the handover check can tell a failed restore from a snapshot still waiting for its EVENT
    assert r.restore_failed is not None and r.restore_failed["utc"]


def test_restore_that_does_not_read_back_keeps_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"), ignore=("d002",))
    assert "outcome=failed" in r.out, r.out
    assert "MISMATCH d002 want=36000 got=18000" in r.out
    assert json.loads(r.snapshot) == PROD and r.consumed == []


def test_restore_blocked_by_a_live_broadcast_keeps_the_snapshot():
    r = _restore(dict(GOOD, iso="400", d002="18000"), busy=True)
    assert "outcome=failed" in r.out, r.out
    assert any(c.startswith("GUARD") for c in r.calls)
    # a live broadcast: no relay stop, no camera session, no set
    assert not any(c.startswith(("RELAYSTOP", "GET", "SET")) for c in r.calls), r.calls
    assert json.loads(r.snapshot) == PROD
    assert "nevysiela" in r.discord


def test_restore_refuses_a_relay_that_comes_back_before_the_set():
    r = _restore(dict(GOOD, iso="400", d002="18000"), relay_active_after_reads=1)
    assert "outcome=failed" in r.out, r.out
    assert "bkshading-relay is still active" in r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert json.loads(r.snapshot) == PROD


def test_restore_names_a_transport_timeout():
    r = _restore(dict(GOOD), read_exit=124)
    assert "outcome=failed" in r.out, r.out
    assert "transport rc=124 (timed out)" in r.out and "NOT restored" in r.out


def test_restore_of_an_invalid_snapshot_is_loud_and_touches_nothing():
    r = _restore(dict(GOOD), snapshot="{broken")
    assert "outcome=failed" in r.out, r.out
    assert "invalid" in r.out
    assert not any(c.startswith(("PRESENCE", "RELAYSTOP", "GET", "SET")) for c in r.calls), r.calls
    assert r.snapshot == "{broken"


def test_restore_that_cannot_move_the_snapshot_aside_is_restored_but_flagged():
    # review round 1: a verified restore whose consume fails is NOT "not restored"
    r = _restore(dict(GOOD, iso="400", d002="18000"), readonly_snap_dir=True)
    assert "outcome=restored-unconsumed" in r.out, r.out
    assert r.camera["iso"] == "800" and r.camera["d002"] == "36000"
    assert "NOT restored" not in r.out
    assert "camera_test_settings.py consume" in r.out
    assert json.loads(r.snapshot) == PROD
    assert "✅" in r.discord and "odložiť" in r.discord and "EVENT contract ok" in r.discord
    assert "camera_test_settings.py" not in r.discord


def test_a_successful_restore_clears_an_earlier_failure_marker():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="400", d002="18000"), _pinned(), snapshot=PROD)
    with open(os.path.join(r.snap_dir, "camera-prod-exposure.restore-failed.json"), "w") as f:
        json.dump({"utc": "2026-09-26T16:00:00Z", "reason": "camera absent"}, f)
    r.run(mode="restore")
    assert "outcome=restored" in r.out, r.out
    assert r.restore_failed is None


def test_the_snapshot_abort_does_not_depend_on_the_callers_pipefail():
    # review round 1: `python3 ... | prefix || rc=$?` only saw the python rc under pipefail
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), pipefail=False)
    with open(r.snap_dir, "w") as f:
        f.write("not a dir")
    r.run()
    assert r.rc == 1, r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls


def test_build_snapshot_refuses_a_box_or_time_it_could_not_read_back():
    b = cts.load_baseline(_baseline_text(_pinned()))
    cur = {"iso": "800", "d002": "36000", "d007": "60"}
    for box, utc in (("cam 1", "t"), ("cam1", ""), ("cam1;x", "t")):
        try:
            cts.build_snapshot(cur, b, box, utc)
        except cts.SnapshotError:
            continue
        raise AssertionError("box %r / time %r must be refused" % (box, utc))


def test_a_consumed_name_never_overwrites_an_earlier_one():
    p = "/h/.camera-box/camera-prod-exposure.json"
    first = cts.consumed_path(p, "20260926T160000Z")
    taken = {first}
    second = cts.unique_consumed_path(p, "20260926T160000Z", taken.__contains__)
    assert second != first and second.startswith("/h/.camera-box/camera-prod-exposure.consumed-20260926T160000Z")
    names = [os.path.basename(first), os.path.basename(second)]
    # review round 2: the later same-second name is the newest (a text max picked the earlier one)
    assert cts.newest_consumed(p, names) == os.path.basename(second)
    many = ["camera-prod-exposure.consumed-20260926T160000Z-%d.json" % n for n in (2, 10, 9)]
    assert cts.newest_consumed(p, many) == "camera-prod-exposure.consumed-20260926T160000Z-10.json"
    later = "camera-prod-exposure.consumed-20260926T160001Z.json"
    assert cts.newest_consumed(p, many + [later]) == later
    assert cts.unique_consumed_path(p, "20260926T160000Z", lambda _p: False) == first


def test_snapshot_state_reports_a_failed_restore():
    root = tempfile.mkdtemp(prefix="cts1371-failed-")
    try:
        p = os.path.join(root, "camera-prod-exposure.json")
        with open(p, "w") as f:
            json.dump(PROD, f)
        assert "restore_failed=" not in cts.snapshot_state(p)
        env = dict(os.environ, CAMERA_PROD_EXPOSURE_SNAPSHOT=p)
        r = subprocess.run(["python3", MODULE, "restore-failed", "--reason", "camera not on USB"],
                           capture_output=True, text=True, env=env)
        assert r.returncode == 0, r.stderr
        line = cts.snapshot_state(p)
        assert line.startswith("exposure state=pending ") and " restore_failed=" in line
        # consume (the verified restore, or the manual move-aside) clears the failure marker
        r = subprocess.run(["python3", MODULE, "consume"], capture_output=True, text=True, env=env)
        assert r.returncode == 0, r.stderr
        assert not os.path.exists(os.path.join(root, "camera-prod-exposure.restore-failed.json"))
        assert cts.snapshot_state(p).startswith("exposure state=restored ")
    finally:
        shutil.rmtree(root)


def test_the_discord_note_never_fails_without_a_message_file():
    here = os.path.join(REPO, "scripts")
    h = ('. "%s/lib/camera-test-settings.sh"\nCTS_RESTORE_OUTCOME=failed\n'
         'camera_test_settings_restore_discord_note ""\n'
         'camera_test_settings_restore_discord_note /nonexistent/dir/msg.txt\necho NOTE-OK\n' % here)
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + h], capture_output=True, text=True)
    assert r.returncode == 0 and "NOTE-OK" in r.stdout, r.stderr


def test_an_invalid_pending_snapshot_is_never_counted_as_kept():
    # review round 2: "No record = no set" -- an unreadable snapshot is no record
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned(), snapshot="{broken").run()
    assert r.rc == 1, r.out
    assert not any(c.startswith("SET") for c in r.calls), r.calls
    assert r.snapshot == "{broken"


def test_a_pending_snapshot_gains_a_newly_pinned_key_but_never_changes_a_kept_one():
    # the baseline pins d004 mid-period: the owner's d004 is still on the camera, so it is added;
    # the kept iso/d002 (the owner's, from before the first set) are never touched
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600", d004="3200"), _pinned(d004=5600), snapshot=PROD).run()
    assert r.rc == 0, r.out
    doc = json.loads(r.snapshot)
    assert doc["values"] == {"iso": "800", "d002": "36000", "d004": "3200"}
    assert doc["box"] == PROD["box"] and doc["taken_utc"] == PROD["taken_utc"]
    assert r.snapshot_at_set is True


def test_a_new_snapshot_drops_a_stale_restore_failed_marker():
    r = Rig({"10.77.9.61": 1}, dict(GOOD, iso="1600"), _pinned())
    os.makedirs(r.snap_dir)
    with open(os.path.join(r.snap_dir, "camera-prod-exposure.restore-failed.json"), "w") as f:
        json.dump({"utc": "2026-09-20T10:00:00Z", "reason": "old"}, f)
    r.run()
    assert r.rc == 0, r.out
    assert r.snapshot is not None and r.restore_failed is None


def test_the_restore_guard_gets_rig_modes_obs_password():
    # review round 2: rig-mode.sh keeps the OBS WS password in OBS_WS_PASSWORD, the shared guard
    # reads OBS_PASSWORD -- the restore must hand it over, or an auth-enabled OBS fails the guard open
    r = _restore(dict(GOOD, iso="400", d002="18000"), extra_env={"OBS_WS_PASSWORD": "wspw"})
    assert "outcome=restored" in r.out, r.out
    guards = [c for c in r.calls if c.startswith("GUARD")]
    assert guards and all(c.endswith("pw=wspw") for c in guards), guards


def test_a_failed_snapshot_write_leaves_no_temp_file():
    root = tempfile.mkdtemp(prefix="cts1371-tmp-")
    base = _tmp_baseline(_pinned())
    try:
        target = os.path.join(root, "camera-prod-exposure.json")
        orig = cts.os.fsync

        def boom(_fd):
            raise OSError(28, "No space left on device")

        cts.os.fsync = boom
        try:
            doc = cts.build_snapshot({"iso": "800", "d002": "36000"}, cts.load_baseline(open(base).read()),
                                     "cam1", "2026-09-26T15:00:00Z")
            try:
                cts.write_snapshot_once(target, doc)
            except OSError:
                pass
            else:
                raise AssertionError("the failed write must raise")
        finally:
            cts.os.fsync = orig
        assert os.listdir(root) == [], os.listdir(root)
    finally:
        shutil.rmtree(root)
        os.unlink(base)


# --- the rig-mode.sh EVENT wiring (static: #675 sourced helper, never an edited anchor line) ------
def _do_event_body(s):
    start = s.index("\ndo_event() {")
    return s[start:s.index("\nmain() {", start)]


def test_rig_mode_restores_before_the_relay_starts_and_never_aborts_the_event_switch():
    s = open(RIG_MODE, encoding="utf-8").read()
    src = '. "$RIG_MODE_DIR/lib/camera-test-settings.sh"'
    assert s.count(src) == 1
    assert s.index(src) < s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then'), "must be sourced before the source-guard"
    body = _do_event_body(s)
    call = ('camera_test_settings_restore "$RIG_MODE_DIR" "$STRIH_IP" "$STREAM_IP" "$CAM_PW" '
            '"${RIG_SOURCE_BOX}=$RIG_SOURCE_IP" "cam2=$PAINTER_IP" || true')
    assert body.count("camera_test_settings_restore ") == 1
    assert ("\n  " + call + "\n") in body
    # the relay must still be stopped: the restore runs BEFORE the EVENT relay start
    assert body.index(call) < body.index("\n  bkshading_relay_mode_apply event")
    # the outcome reaches the owner's EVENT Discord confirmation, appended before it is sent
    note = 'camera_test_settings_restore_discord_note "${EVENT_ASSERT_DISCORD_MSG_PATH:-}"'
    assert body.count(note) == 1
    assert body.index("\n  event_mode_assert\n") < body.index(note) < body.index("event_mode_discord_confirm_send")


def test_rig_mode_sources_the_restore_helpers():
    h = '. "%s"\ndeclare -F camera_test_settings_restore camera_test_settings_restore_discord_note\n' % RIG_MODE
    r = subprocess.run(["bash", "-c", "set -uo pipefail\n" + h], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert "camera_test_settings_restore" in r.stdout


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
    print("%d tests passed" % len(fns))
    sys.exit(0)
