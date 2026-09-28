"""issue 1383 -- the race guarantees of the rig-lease holder's keep-alive, driven through the pure
module scripts/rig_lease_refresh.py (the bash helper rig_lease_refresh_if_mine is a thin wrapper
over its CLI). The `before_rename` seam runs after holder.json was read + the temp copy written and
BEFORE the re-read that guards the rename, i.e. exactly where a concurrent release or reclaim lands.

Every test works on a tmp lease dir; the live dev1 lease is never touched.
"""
import json
import os
import pathlib
import sys
from datetime import datetime, timedelta, timezone

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_lease_refresh as rlr  # noqa: E402

REPO_ID = "zbynekdrlik/camera-box"
RUN_ID = "36351718907"
NOW = datetime(2026, 9, 27, 22, 15, 0, tzinfo=timezone.utc)
ISO = "%Y-%m-%dT%H:%M:%SZ"


def _holder(**overrides):
    h = {"repo": REPO_ID, "run_id": RUN_ID, "run_url": "", "job": "full-path",
         "acquired_at": (NOW - timedelta(minutes=49)).strftime(ISO),
         "expected_release_at": (NOW - timedelta(minutes=4)).strftime(ISO)}
    h.update(overrides)
    return h


@pytest.fixture
def lease(tmp_path):
    d = tmp_path / "rig-lease"
    d.mkdir()
    (d / "holder.json").write_text(json.dumps(_holder()))
    (d / "heartbeat").write_text("")
    os.utime(d / "heartbeat", (1000, 1000))
    return d


def _refresh(lease_dir, **kw):
    args = dict(lookahead_s=900, max_hold_s=4500, now=NOW)
    args.update(kw)
    return rlr.refresh(str(lease_dir), REPO_ID, RUN_ID, **args)


def test_the_release_time_is_now_plus_the_look_ahead(lease):
    rc, status = _refresh(lease)
    assert rc == rlr.RC_REFRESHED, status
    assert json.loads((lease / "holder.json").read_text())["expected_release_at"] \
        == (NOW + timedelta(seconds=900)).strftime(ISO)
    assert (lease / "heartbeat").stat().st_mtime > 1000


def test_a_reclaim_between_the_read_and_the_rename_wins(lease):
    foreign = json.dumps(_holder(repo="zbynekdrlik/restreamer", run_id="888"))

    def reclaim():  # rig_lease_write_holder rewrites holder.json IN PLACE
        (lease / "holder.json").write_text(foreign)

    rc, status = _refresh(lease, before_rename=reclaim)
    assert rc == rlr.RC_NOT_OURS, status
    assert (lease / "holder.json").read_text() == foreign, "the new holder is never overwritten"
    assert sorted(p.name for p in lease.iterdir()) == ["heartbeat", "holder.json"], \
        "the temp copy is removed"


def test_a_release_and_a_new_acquire_mid_refresh_never_touch_the_new_lease(lease, tmp_path):
    new_holder = json.dumps(_holder(repo="zbynekdrlik/restreamer", run_id="999"))

    def release_then_acquire():  # rig_lease_release renames aside; a peer's mkdir takes the path
        os.rename(lease, tmp_path / "rig-lease.releasing.4242")
        lease.mkdir()
        (lease / "holder.json").write_text(new_holder)
        (lease / "heartbeat").write_text("")
        os.utime(lease / "heartbeat", (2000, 2000))

    rc, status = _refresh(lease, before_rename=release_then_acquire)
    assert rc == rlr.RC_NOT_OURS, status
    assert "released" in status
    assert (lease / "holder.json").read_text() == new_holder
    assert (lease / "heartbeat").stat().st_mtime == 2000
    assert sorted(p.name for p in lease.iterdir()) == ["heartbeat", "holder.json"]


def test_a_release_without_a_new_acquire_never_re_creates_the_lease(lease, tmp_path):
    def release():
        os.rename(lease, tmp_path / "rig-lease.releasing.4243")

    rc, status = _refresh(lease, before_rename=release)
    assert rc == rlr.RC_NOT_OURS, status
    assert not lease.exists()


def test_past_the_hold_ceiling_nothing_is_written(lease):
    raw = (lease / "holder.json").read_bytes()
    rc, status = _refresh(lease, now=NOW + timedelta(minutes=30))  # acquired 79 min ago
    assert rc == rlr.RC_OVER_HOLD, status
    assert (lease / "holder.json").read_bytes() == raw
    assert (lease / "heartbeat").stat().st_mtime == 1000


def test_a_symlinked_heartbeat_is_never_followed(lease, tmp_path):
    target = tmp_path / "elsewhere"
    target.write_text("keep")
    os.utime(target, (1500, 1500))
    (lease / "heartbeat").unlink()
    (lease / "heartbeat").symlink_to(target)
    rc, status = _refresh(lease)
    assert rc == rlr.RC_ERROR, status
    assert target.read_text() == "keep" and target.stat().st_mtime == 1500


def test_a_leftover_temp_of_the_same_name_does_not_block_the_refresh(lease):
    (lease / f"holder.json.refresh.{os.getpid()}").write_text("left over by a killed refresh")
    rc, status = _refresh(lease)
    assert rc == rlr.RC_REFRESHED, status
    assert sorted(p.name for p in lease.iterdir()) == ["heartbeat", "holder.json"]


def test_the_cli_prints_one_status_line_and_returns_the_code(lease, capsys):
    rc = rlr.main(["rig_lease_refresh.py", str(lease), REPO_ID, "someone-else", "900", "4500"])
    assert rc == rlr.RC_NOT_OURS
    out = capsys.readouterr().out
    assert out.startswith("RIG_LEASE_REFRESH=not-mine") and out.count("\n") == 1
