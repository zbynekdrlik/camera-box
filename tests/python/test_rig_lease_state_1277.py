"""#1277 -- PURE decision tests for scripts/rig_lease_state.py::lease_state(), the mirror of
scripts/lib/rig-lease.sh's staleness/read logic used by scripts/rig-lease-server.py.

See the module's own doc comment for the full mirror contract (heartbeat sentinel, the
absent-holder.json-forces-stale vs corrupt-holder.json-stays-heartbeat-driven split). Fixtures:
free (no dir) / held-fresh / held-stale (old heartbeat mtime) / held-missing-heartbeat / corrupt
holder.json / expected_release_at in the past (negative ttl).
"""
from __future__ import annotations

import json
import os
import pathlib
import sys
from datetime import datetime, timedelta, timezone

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_lease_state as rls  # noqa: E402

_NOW = datetime(2026, 9, 2, 12, 0, 0, tzinfo=timezone.utc)


def _write_holder(lease_dir: pathlib.Path, **overrides):
    holder = {
        "repo": "zbynekdrlik/camera-box",
        "run_id": "123456",
        "run_url": "https://github.com/zbynekdrlik/camera-box/actions/runs/123456",
        "job": "full-path-e2e",
        "acquired_at": "2026-09-02T11:30:00Z",
        "expected_release_at": "2026-09-02T13:00:00Z",
    }
    holder.update(overrides)
    (lease_dir / "holder.json").write_text(json.dumps(holder), encoding="utf-8")
    return holder


def _touch_heartbeat(lease_dir: pathlib.Path, age_s: int = 0):
    hb = lease_dir / "heartbeat"
    hb.write_text("", encoding="utf-8")
    stamp = (_NOW - timedelta(seconds=age_s)).timestamp()
    os.utime(hb, (stamp, stamp))


# --------------------------------------------------------------------------- free (no dir)
def test_free_lease_dir_absent_is_not_held():
    missing = "/nonexistent/rig-lease-1277-test-dir"
    state = rls.lease_state(missing, _NOW, 5400)
    assert state["held"] is False
    assert state["holder"] is None
    assert state["heartbeat_age_s"] is None
    assert state["stale"] is None
    assert state["expected_release_at"] is None
    assert state["ttl_s"] is None
    assert state["schema"] == rls.SCHEMA_VERSION
    assert state["now"] == "2026-09-02T12:00:00Z"


# --------------------------------------------------------------------------- held-fresh
def test_held_fresh_lease_reports_full_holder_and_positive_ttl(tmp_path):
    _write_holder(tmp_path)
    _touch_heartbeat(tmp_path, age_s=30)

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["stale"] is False
    assert state["heartbeat_age_s"] == 30
    assert state["holder"] == {
        "repo": "zbynekdrlik/camera-box",
        "run_id": "123456",
        "run_url": "https://github.com/zbynekdrlik/camera-box/actions/runs/123456",
        "job": "full-path-e2e",
        "acquired_at": "2026-09-02T11:30:00Z",
        "expected_release_at": "2026-09-02T13:00:00Z",
    }
    assert state["expected_release_at"] == "2026-09-02T13:00:00Z"
    assert state["ttl_s"] == 3600  # 13:00:00 - 12:00:00


# --------------------------------------------------------------------------- held-stale (old heartbeat)
def test_held_stale_via_old_heartbeat(tmp_path):
    _write_holder(tmp_path)
    _touch_heartbeat(tmp_path, age_s=5401)  # 1s past the 5400s default threshold

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["stale"] is True
    assert state["heartbeat_age_s"] == 5401
    # holder info is still reported even when stale-via-heartbeat -- only an ABSENT/corrupt
    # holder.json forces holder=None, never a stale heartbeat alone.
    assert state["holder"] is not None
    assert state["holder"]["repo"] == "zbynekdrlik/camera-box"


def test_held_exactly_at_threshold_is_not_fresh(tmp_path):
    # rig_lease_is_fresh requires age < stale_secs (strict) -- age == stale_secs is stale.
    _write_holder(tmp_path)
    _touch_heartbeat(tmp_path, age_s=5400)

    state = rls.lease_state(str(tmp_path), _NOW, 5400)
    assert state["stale"] is True


# --------------------------------------------------------------------------- held-missing-heartbeat
def test_held_missing_heartbeat_is_sentinel_age_and_stale(tmp_path):
    _write_holder(tmp_path)
    # deliberately never call _touch_heartbeat -- no heartbeat file at all

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["heartbeat_age_s"] == rls.HEARTBEAT_MISSING_SENTINEL
    assert state["stale"] is True
    # holder is still readable -- only the heartbeat is missing, not holder.json
    assert state["holder"] is not None


# --------------------------------------------------------------------------- corrupt holder.json
def test_corrupt_holder_json_is_held_true_holder_null_fail_closed(tmp_path):
    (tmp_path / "holder.json").write_text("{not valid json!!", encoding="utf-8")
    _touch_heartbeat(tmp_path, age_s=10)  # fresh heartbeat

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["holder"] is None
    assert state["expected_release_at"] is None
    assert state["ttl_s"] is None
    # staleness stays heartbeat-driven for a PRESENT-but-corrupt file (fresh heartbeat -> not stale)
    assert state["stale"] is False


def test_absent_holder_json_lockdir_only_is_unconditionally_stale(tmp_path):
    # lockdir exists (e.g. a crashed mkdir-then-write acquire) but holder.json was never written --
    # mirrors `[ -f "$path" ] || return 0` in rig_lease_is_stale: unconditionally stale/reclaimable,
    # regardless of heartbeat freshness.
    _touch_heartbeat(tmp_path, age_s=1)  # deliberately FRESH heartbeat -- must not matter

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["holder"] is None
    assert state["stale"] is True


# --------------------------------------------------------------------------- expected_release_at in the past
def test_expected_release_at_in_the_past_yields_negative_ttl(tmp_path):
    _write_holder(tmp_path, expected_release_at="2026-09-02T11:00:00Z")  # 1h before _NOW
    _touch_heartbeat(tmp_path, age_s=5)

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["stale"] is False  # heartbeat is fresh -- overdue release is a SEPARATE signal
    assert state["ttl_s"] == -3600


def test_unparseable_expected_release_at_yields_null_ttl_not_a_crash(tmp_path):
    _write_holder(tmp_path, expected_release_at="not-a-timestamp")
    _touch_heartbeat(tmp_path, age_s=5)

    state = rls.lease_state(str(tmp_path), _NOW, 5400)

    assert state["held"] is True
    assert state["holder"]["expected_release_at"] == "not-a-timestamp"
    assert state["expected_release_at"] == "not-a-timestamp"
    assert state["ttl_s"] is None


# --------------------------------------------------------------------------- lock-step constant
def test_default_stale_secs_matches_rig_busy_gate_sh():
    """scripts/rig-busy-gate.sh:79 reads RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}" --
    this module's own default MUST mirror that literal, or the HTTP view of staleness silently
    diverges from the bash gate's own decision for the exact same lockdir."""
    gate_sh = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "rig-busy-gate.sh"
    text = gate_sh.read_text(encoding="utf-8")
    assert 'RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}"' in text
    assert rls.DEFAULT_STALE_SECS == 5400


# --------------------------------------------------------------------------- format_ts / parse_ts
def test_format_ts_round_trips_through_parse_ts():
    dt = datetime(2026, 9, 2, 13, 5, 9, tzinfo=timezone.utc)
    text = rls.format_ts(dt)
    assert text == "2026-09-02T13:05:09Z"
    assert rls.parse_ts(text) == dt


def test_parse_ts_none_and_empty_and_garbage_all_return_none():
    assert rls.parse_ts(None) is None
    assert rls.parse_ts("") is None
    assert rls.parse_ts("garbage") is None
