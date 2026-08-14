"""#900 -- unit tests for scripts/phase_sync_reanchor.py, the pre-[4h/8] phase-sync RE-ANCHOR
establisher: re-derive the ACTIVE pin set from the ALREADY-persisted per-camera transits
(phase-sync-last.json) and apply it, so the [4h/8] active-floor gate always has an establisher.

Covers, with NO live OBS and NO gate binary (the pure decision layer only -- the offset kernel is
delegated to phase_sync_calibrate.compute_phase_sync_offsets, itself proven in Rust):
  a. load_persisted_transits() -- {source: latency_ms} from the durable file; FAIL LOUD on
     missing / malformed / empty / non-numeric latency (a genuine "no calibration basis" state).
  b. restrict_to_active() -- keep only CAMERA_ACTIVE_SET sources, drop the rest; FAIL LOUD when an
     active camera is NOT covered by the persisted file (nothing to re-anchor it from).
  c. plan_reanchor() -- the no-op-vs-apply decision: a NO-OP when live pins already equal the
     re-derived set (never churn a healthy rig), an apply otherwise (incl. an unreadable live pin).

Per .claude/rules/phase-sync-calibrator-testing.md these tests NEVER call main()/--apply, so they
cannot clobber the real ~/.camera-box/phase-sync-last.json -- every file is under tmp_path.
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import phase_sync_reanchor  # noqa: E402


def _write(path, cameras, ts=1786601599.0):
    path.write_text(json.dumps({"cameras": cameras, "ts": ts}))
    return str(path)


# --------------------------------------------------------------------------- (a)
class TestLoadPersistedTransits:
    def test_parses_source_to_latency_ms(self, tmp_path):
        p = _write(tmp_path / "last.json", [
            {"source": "NDI cam1", "latency_ms": 80.913, "offset_ms": 3, "applied_latency_ms": 3},
            {"source": "NDI cam2", "latency_ms": 78.057, "offset_ms": 6, "applied_latency_ms": 6},
        ])
        assert phase_sync_reanchor.load_persisted_transits(p) == {
            "NDI cam1": 80.913, "NDI cam2": 78.057,
        }

    def test_missing_file_fails_loud(self, tmp_path):
        with pytest.raises(SystemExit):
            phase_sync_reanchor.load_persisted_transits(str(tmp_path / "nope.json"))

    def test_empty_cameras_fails_loud(self, tmp_path):
        p = _write(tmp_path / "last.json", [])
        with pytest.raises(SystemExit):
            phase_sync_reanchor.load_persisted_transits(p)

    def test_non_numeric_latency_fails_loud(self, tmp_path):
        p = _write(tmp_path / "last.json", [{"source": "NDI cam1", "latency_ms": "oops"}])
        with pytest.raises(SystemExit):
            phase_sync_reanchor.load_persisted_transits(p)

    def test_malformed_json_fails_loud(self, tmp_path):
        p = tmp_path / "last.json"
        p.write_text("{not json")
        with pytest.raises(SystemExit):
            phase_sync_reanchor.load_persisted_transits(str(p))


# --------------------------------------------------------------------------- (b)
class TestRestrictToActive:
    def test_keeps_active_drops_inactive(self):
        transits = {"NDI cam1": 80.9, "NDI cam2": 78.0, "NDI cam3": 64.0}
        assert phase_sync_reanchor.restrict_to_active(
            transits, ["NDI cam1", "NDI cam2"]
        ) == {"NDI cam1": 80.9, "NDI cam2": 78.0}

    def test_uncovered_active_camera_fails_loud(self):
        transits = {"NDI cam1": 80.9, "NDI cam2": 78.0}
        with pytest.raises(SystemExit) as ei:
            phase_sync_reanchor.restrict_to_active(
                transits, ["NDI cam1", "NDI cam2", "NDI cam4"]
            )
        # the message must name the missing camera so a human can fix the basis
        assert "cam4" in str(ei.value)

    def test_empty_active_set_fails_loud(self):
        with pytest.raises(SystemExit):
            phase_sync_reanchor.restrict_to_active({"NDI cam1": 80.9}, [])


# --------------------------------------------------------------------------- (c)
class TestPlanReanchor:
    def test_all_match_is_a_noop(self):
        desired = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        current = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        is_noop, changes = phase_sync_reanchor.plan_reanchor(desired, current)
        assert is_noop is True
        assert changes == []

    def test_constant_shift_after_a_camera_leaves_is_an_apply(self):
        # issue-898 shape: dropping the anchor shifts every survivor down a constant -18ms
        desired = {"NDI cam1": 3, "NDI cam2": 4, "NDI cam4": 5}
        current = {"NDI cam1": 21, "NDI cam2": 22, "NDI cam4": 22}
        is_noop, changes = phase_sync_reanchor.plan_reanchor(desired, current)
        assert is_noop is False
        assert set(s for s, _, _ in changes) == {"NDI cam1", "NDI cam2", "NDI cam4"}

    def test_unreadable_live_pin_counts_as_a_change(self):
        desired = {"NDI cam1": 3, "NDI cam2": 6}
        current = {"NDI cam1": 3, "NDI cam2": None}
        is_noop, changes = phase_sync_reanchor.plan_reanchor(desired, current)
        assert is_noop is False
        assert any(s == "NDI cam2" for s, _, _ in changes)
