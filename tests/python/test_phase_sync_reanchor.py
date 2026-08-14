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


# --------------------------------------------------------------------------- (d) #900 review 🔵1
class TestRecoverUniformMargin:
    def test_margin_free_calibration_yields_zero(self):
        # the standing default: offset_ms == kernel offset (slowest at floor 3)
        assert phase_sync_reanchor.recover_uniform_margin(
            {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        ) == 0

    def test_uniform_margin_is_recovered_from_the_min_offset(self):
        # a +10ms headroom shifts every pin up uniformly: 3/6/20 -> 13/16/30
        assert phase_sync_reanchor.recover_uniform_margin(
            {"NDI cam1": 13, "NDI cam2": 16, "NDI cam3": 30}
        ) == 10

    def test_empty_offsets_yield_zero(self):
        assert phase_sync_reanchor.recover_uniform_margin({}) == 0

    def test_never_negative(self):
        # a floor-pinned min can never sit below the floor, but guard anyway
        assert phase_sync_reanchor.recover_uniform_margin({"NDI cam1": 3}) == 0


class TestLoadPersistedOffsets:
    def test_reads_offset_ms(self, tmp_path):
        p = _write(tmp_path / "last.json", [
            {"source": "NDI cam1", "latency_ms": 80.9, "offset_ms": 3},
            {"source": "NDI cam2", "latency_ms": 78.0, "offset_ms": 6},
        ])
        assert phase_sync_reanchor.load_persisted_offsets(p) == {"NDI cam1": 3, "NDI cam2": 6}

    def test_skips_entries_without_numeric_offset(self, tmp_path):
        p = _write(tmp_path / "last.json", [
            {"source": "NDI cam1", "latency_ms": 80.9, "offset_ms": 3},
            {"source": "NDI cam2", "latency_ms": 78.0},  # no offset yet
        ])
        assert phase_sync_reanchor.load_persisted_offsets(p) == {"NDI cam1": 3}


# --------------------------------------------------------------------------- (e) #900 review 🔵4
class _FakeWs:
    def close(self):
        pass


def _persisted(tmp_path):
    return _write(tmp_path / "last.json", [
        {"source": "NDI cam1", "latency_ms": 80.913, "offset_ms": 3, "applied_latency_ms": 3},
        {"source": "NDI cam2", "latency_ms": 78.057, "offset_ms": 6, "applied_latency_ms": 6},
        {"source": "NDI cam3", "latency_ms": 64.048, "offset_ms": 20, "applied_latency_ms": 20},
    ])


def _patch_ws(monkeypatch, live_pins, applied, wrote):
    """Mock the WS layer so main() runs with NO OBS. `live_pins` = what read_pin returns per
    source; `applied`/`wrote` are lists the mocks append to so a test can assert what was written."""
    monkeypatch.setattr(phase_sync_reanchor, "_conn", lambda h, p: _FakeWs())
    monkeypatch.setattr(phase_sync_reanchor, "read_pin", lambda ws, s: live_pins.get(s))
    monkeypatch.setattr(phase_sync_reanchor, "read_current_latency", lambda ws, s: live_pins.get(s) or 0)
    # margin-free kernel: slowest (cam1) at floor 3, others held back
    kernel = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
    monkeypatch.setattr(phase_sync_reanchor, "compute_phase_sync_offsets",
                        lambda measured, gate_bin=None: {s: kernel[s] for s in measured})
    monkeypatch.setattr(phase_sync_reanchor, "apply_latency",
                        lambda ws, s, cur, new: applied.append((s, cur, new)) or new)
    monkeypatch.setattr(phase_sync_reanchor, "write_last_json",
                        lambda path, cams: wrote.append((str(path), cams)) or {})


class TestMainRequirements:
    def test_noop_writes_nothing_and_never_touches_the_durable_basis(self, tmp_path, monkeypatch):
        persisted = _persisted(tmp_path)
        before = open(persisted).read()
        out = str(tmp_path / "reanchor-out.json")
        applied, wrote = [], []
        _patch_ws(monkeypatch, {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}, applied, wrote)
        rc = phase_sync_reanchor.main([
            "--host", "x", "--active-set", "cam1 cam2 cam3",
            "--persisted-json", persisted, "--out-json", out, "--gate-bin", "x", "--apply",
        ])
        assert rc == 0
        assert applied == []                      # req 2: a healthy rig is NOT churned
        assert wrote == []                        # nothing persisted on a no-op
        assert not pathlib.Path(out).exists()     # no run-scoped write either
        assert open(persisted).read() == before   # req 4: durable basis untouched

    def test_apply_writes_only_the_run_scoped_out_json_never_the_basis(self, tmp_path, monkeypatch):
        persisted = _persisted(tmp_path)
        before = open(persisted).read()
        out = str(tmp_path / "reanchor-out.json")
        applied, wrote = [], []
        # live pins drifted up (a camera left/joined shape) -> apply
        _patch_ws(monkeypatch, {"NDI cam1": 21, "NDI cam2": 22, "NDI cam3": 22}, applied, wrote)
        rc = phase_sync_reanchor.main([
            "--host", "x", "--active-set", "cam1 cam2 cam3",
            "--persisted-json", persisted, "--out-json", out, "--gate-bin", "x", "--apply",
        ])
        assert rc == 0
        assert {s for s, _, _ in applied} == {"NDI cam1", "NDI cam2", "NDI cam3"}
        assert len(wrote) == 1 and wrote[0][0] == out   # only the run-scoped file
        assert open(persisted).read() == before          # req 4: durable basis untouched

    def test_out_json_equal_to_the_basis_is_rejected(self, tmp_path, monkeypatch):
        persisted = _persisted(tmp_path)
        applied, wrote = [], []
        _patch_ws(monkeypatch, {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}, applied, wrote)
        with pytest.raises(SystemExit):
            phase_sync_reanchor.main([
                "--host", "x", "--active-set", "cam1 cam2 cam3",
                "--persisted-json", persisted, "--out-json", persisted, "--apply",
            ])
