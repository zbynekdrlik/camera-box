"""#1003 -- tests for obs_phase2.py's measurement-window equalization apply/restore + the
baseline-anchored leftover detection extension of _snapshot_and_set_test_latency.

Uses a STATEFUL fake WS (GetInputSettings reads / SetInputSettings writes a per-source pin dict)
so the read-back-verify path is exercised for real, plus an in-memory state store. No rig, no
cargo. Mirrors tests/python/test_obs_phase2_latency_delivery.py's importlib + _rpc-patch pattern."""
import importlib.util
import json
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_meq", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_meq"] = obs_phase2
_spec.loader.exec_module(obs_phase2)

_GK = "genlock_latency_ms_src"


class _WS:
    def close(self):
        pass


def _install(monkeypatch, pins):
    """Patch _rpc with a stateful fake over `pins` (source -> pin ms; a None value models an
    unreadable input), _conn to hand back a dummy ws, and _load_state/_save_state to an in-memory
    dict. Returns (state_holder, pins) so tests can assert both the live pins and the saved state."""
    state = {}

    def fake_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
        payload = payload or {}
        if op == "GetInputSettings":
            src = payload["inputName"]
            val = pins.get(src)
            return {"inputSettings": ({} if val is None else {_GK: val})}
        if op == "SetInputSettings":
            src = payload["inputName"]
            pins[src] = payload["inputSettings"][_GK]
            return {}
        if op == "GetSourceFilterList":
            return {"filters": []}
        return {}

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw="": _WS())
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: state)
    monkeypatch.setattr(obs_phase2, "_save_state", lambda s: state.update(s))
    return state, pins


def _profile_file(tmp_path):
    prof = {
        "target_delivery_ms": 207, "min_deep_pin_ms": 80, "leftover_slack_ms": 40,
        "staleness_frames": 1.5, "av_expected_ms": 0,
        "cameras": {
            "NDI cam1": {"production_pin_ms": 3, "production_delivery_p50_ms": 120.0, "production_av_offset_ms": 95.2},
            "NDI cam2": {"production_pin_ms": 6, "production_delivery_p50_ms": 44.5, "production_av_offset_ms": 24.1},
            "NDI cam3": {"production_pin_ms": 20, "production_delivery_p50_ms": 42.6, "production_av_offset_ms": 15.4},
        },
        "stream": {"source": "NDI 2ME PGM", "production_hold_ms": 971},
    }
    p = tmp_path / "profile.json"
    p.write_text(json.dumps(prof))
    return str(p)


class _Args:
    def __init__(self, **kw):
        self.__dict__.update(kw)


class TestReadCurrentPin:
    def test_returns_int_when_set(self, monkeypatch):
        _install(monkeypatch, {"NDI cam1": 20})
        assert obs_phase2.read_current_pin(_WS(), "NDI cam1") == 20

    def test_returns_none_when_unreadable(self, monkeypatch):
        _install(monkeypatch, {"NDI cam1": None})
        assert obs_phase2.read_current_pin(_WS(), "NDI cam1") is None


class TestApplyMeasurementPins:
    def test_happy_path_snapshots_production_and_sets_deep_pins(self, monkeypatch, tmp_path):
        state, pins = _install(monkeypatch, {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20})
        obs_phase2.apply_measurement_pins(
            _Args(host="strih", password="", profile=_profile_file(tmp_path)))
        assert pins == {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}  # deep pins applied
        assert state["strih"][obs_phase2._MEASUREMENT_EQ_STATE_KEY]["pins"] == {
            "NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}  # production snapshotted for restore

    def test_leftover_test_pin_is_restored_to_production_before_snapshot(self, monkeypatch, tmp_path):
        # cam1 left at its OWN test value 90 by a prior crashed run: must snapshot 3, not 90.
        state, pins = _install(monkeypatch, {"NDI cam1": 90, "NDI cam2": 6, "NDI cam3": 20})
        obs_phase2.apply_measurement_pins(
            _Args(host="strih", password="", profile=_profile_file(tmp_path)))
        assert state["strih"][obs_phase2._MEASUREMENT_EQ_STATE_KEY]["pins"]["NDI cam1"] == 3
        assert pins["NDI cam1"] == 90  # test pin still ends applied

    def test_stale_pin_far_from_production_fails_loud_and_writes_nothing(self, monkeypatch, tmp_path):
        # cam2 at 500 (neither prod 6 nor test 160) -> beyond slack, NOT the test value -> STALE:
        # the profile disagrees with the live rig, so apply must FAIL LOUD before writing anything
        # (never auto-restore a checked-in constant over a possibly-legitimately-retuned live value).
        state, pins = _install(monkeypatch, {"NDI cam1": 3, "NDI cam2": 500, "NDI cam3": 20})
        with pytest.raises(SystemExit):
            obs_phase2.apply_measurement_pins(
                _Args(host="strih", password="", profile=_profile_file(tmp_path)))
        assert obs_phase2._MEASUREMENT_EQ_STATE_KEY not in state.get("strih", {})  # no snapshot saved
        assert pins == {"NDI cam1": 3, "NDI cam2": 500, "NDI cam3": 20}  # nothing applied

    def test_unreadable_pin_snapshots_production_defensively(self, monkeypatch, tmp_path):
        state, _ = _install(monkeypatch, {"NDI cam1": None, "NDI cam2": 6, "NDI cam3": 20})
        obs_phase2.apply_measurement_pins(
            _Args(host="strih", password="", profile=_profile_file(tmp_path)))
        assert state["strih"][obs_phase2._MEASUREMENT_EQ_STATE_KEY]["pins"]["NDI cam1"] == 3

    def test_incoherent_profile_fails_loud(self, monkeypatch, tmp_path):
        _install(monkeypatch, {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20})
        prof = json.loads(pathlib.Path(_profile_file(tmp_path)).read_text())
        prof["target_delivery_ms"] = 130  # shallow -> cam1 pin below min_deep
        p = tmp_path / "bad.json"
        p.write_text(json.dumps(prof))
        with pytest.raises(SystemExit):
            obs_phase2.apply_measurement_pins(_Args(host="strih", password="", profile=str(p)))

    def test_readback_mismatch_rolls_back_and_fails_loud(self, monkeypatch, tmp_path):
        # a WS that silently refuses SetInputSettings (force-drain class) -> read-back != set.
        state = {}
        pins = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}

        def stuck_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
            payload = payload or {}
            if op == "GetInputSettings":
                return {"inputSettings": {_GK: pins[payload["inputName"]]}}
            if op == "SetInputSettings":
                # ignore deep-pin sets (never applies) but honor a rollback to a small value
                v = payload["inputSettings"][_GK]
                if v < 50:
                    pins[payload["inputName"]] = v
                return {}
            return {}

        monkeypatch.setattr(obs_phase2, "_rpc", stuck_rpc)
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, pw="": _WS())
        monkeypatch.setattr(obs_phase2, "_load_state", lambda: state)
        monkeypatch.setattr(obs_phase2, "_save_state", lambda s: state.update(s))
        with pytest.raises(SystemExit):
            obs_phase2.apply_measurement_pins(
                _Args(host="strih", password="", profile=_profile_file(tmp_path)))


class TestRestoreMeasurementPins:
    def test_restores_snapshot_and_clears_state(self, monkeypatch):
        state, pins = _install(monkeypatch, {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184})
        state["strih"] = {obs_phase2._MEASUREMENT_EQ_STATE_KEY: {
            "pins": {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}}}
        obs_phase2._restore_measurement_pins(_WS(), "strih", state)
        assert pins == {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        assert obs_phase2._MEASUREMENT_EQ_STATE_KEY not in state["strih"]

    def test_no_op_when_nothing_snapshotted(self, monkeypatch):
        state, pins = _install(monkeypatch, {"NDI cam1": 90})
        obs_phase2._restore_measurement_pins(_WS(), "strih", state)
        assert pins == {"NDI cam1": 90}  # untouched

    def test_read_back_mismatch_KEEPS_the_snapshot_for_retry(self, monkeypatch):
        # a WS that silently drops SetInputSettings -> read-back stays at the test value != snapshot.
        # The snapshot is the one durable artifact for a retry, so it must NOT be cleared on mismatch.
        state = {"strih": {obs_phase2._MEASUREMENT_EQ_STATE_KEY: {
            "pins": {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}}}}
        live = {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}

        def stuck_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
            payload = payload or {}
            if op == "GetInputSettings":
                return {"inputSettings": {_GK: live[payload["inputName"]]}}
            return {}  # SetInputSettings silently drops -> read-back never matches

        monkeypatch.setattr(obs_phase2, "_rpc", stuck_rpc)
        monkeypatch.setattr(obs_phase2, "_save_state", lambda s: None)
        obs_phase2._restore_measurement_pins(_WS(), "strih", state)
        assert obs_phase2._MEASUREMENT_EQ_STATE_KEY in state["strih"]  # snapshot KEPT for retry


class TestStreamHoldLeftoverDetection:
    def test_leftover_hold_789_is_restored_to_production_971_before_snapshot(self, monkeypatch):
        # the exact revert incident: prod-scene snapshot must capture 971, not a leftover 789.
        state, pins = _install(monkeypatch, {"NDI 2ME PGM": 789})
        obs_phase2._snapshot_and_set_test_latency(
            _WS(), "stream", "NDI 2ME PGM", 789, state,
            production_ref_ms=971, leftover_slack_ms=40)
        assert state["stream"][obs_phase2._TEST_LATENCY_STATE_KEY]["latency_ms"] == 971

    def test_genuine_production_hold_is_snapshotted_as_is(self, monkeypatch):
        state, _ = _install(monkeypatch, {"NDI 2ME PGM": 971})
        obs_phase2._snapshot_and_set_test_latency(
            _WS(), "stream", "NDI 2ME PGM", 791, state,
            production_ref_ms=971, leftover_slack_ms=40)
        assert state["stream"][obs_phase2._TEST_LATENCY_STATE_KEY]["latency_ms"] == 971

    def test_stale_hold_fails_loud_and_does_not_stomp(self, monkeypatch):
        # live hold 915 (a legitimate operator re-tune, beyond slack of the profile's 971 ref, NOT
        # the test value 791) -> STALE -> fail loud, never silently restore 971 over it.
        state, pins = _install(monkeypatch, {"NDI 2ME PGM": 915})
        with pytest.raises(SystemExit):
            obs_phase2._snapshot_and_set_test_latency(
                _WS(), "stream", "NDI 2ME PGM", 791, state,
                production_ref_ms=971, leftover_slack_ms=40)
        assert pins["NDI 2ME PGM"] == 915  # untouched — no stomp to 971

    def test_backward_compatible_without_prod_ref(self, monkeypatch):
        # no production_ref_ms -> today's exact #691 behavior: snapshot whatever is live.
        state, _ = _install(monkeypatch, {"NDI 2ME PGM": 923})
        obs_phase2._snapshot_and_set_test_latency(
            _WS(), "stream", "NDI 2ME PGM", None, state)
        assert state["stream"][obs_phase2._TEST_LATENCY_STATE_KEY]["latency_ms"] == 923


class TestMeasurementPinsMismatches:
    def _plan(self):
        return {
            "strih_pins": {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184},
            "stream_source": "NDI 2ME PGM", "stream_hold_ms": 791, "av_expected_ms": 0,
        }

    def test_strih_all_in_force_no_problems(self):
        live = {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}
        assert obs_phase2.measurement_pins_mismatches("strih", self._plan(), live) == []

    def test_strih_a_stomped_pin_is_flagged(self):
        live = {"NDI cam1": 3, "NDI cam2": 160, "NDI cam3": 184}  # cam1 stomped back to prod
        problems = obs_phase2.measurement_pins_mismatches("strih", self._plan(), live)
        assert len(problems) == 1 and "NDI cam1" in problems[0]

    def test_strih_an_unreadable_pin_is_flagged(self):
        live = {"NDI cam1": None, "NDI cam2": 160, "NDI cam3": 184}
        assert obs_phase2.measurement_pins_mismatches("strih", self._plan(), live) != []

    def test_stream_in_force_no_problems(self):
        assert obs_phase2.measurement_pins_mismatches("stream", self._plan(), 791) == []

    def test_stream_stomped_hold_is_flagged(self):
        problems = obs_phase2.measurement_pins_mismatches("stream", self._plan(), 971)
        assert len(problems) == 1 and "NDI 2ME PGM" in problems[0]


class TestVerifyMeasurementPins:
    def test_pass_when_strih_pins_in_force(self, monkeypatch, tmp_path):
        _install(monkeypatch, {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184})
        # no SystemExit == pass
        obs_phase2.verify_measurement_pins(
            _Args(host="strih", password="", profile=_profile_file(tmp_path), role="strih"))

    def test_fail_when_a_strih_pin_was_stomped(self, monkeypatch, tmp_path):
        _install(monkeypatch, {"NDI cam1": 3, "NDI cam2": 160, "NDI cam3": 184})
        with pytest.raises(SystemExit):
            obs_phase2.verify_measurement_pins(
                _Args(host="strih", password="", profile=_profile_file(tmp_path), role="strih"))

    def test_pass_when_stream_hold_in_force(self, monkeypatch, tmp_path):
        _install(monkeypatch, {"NDI 2ME PGM": 791})
        obs_phase2.verify_measurement_pins(
            _Args(host="stream", password="", profile=_profile_file(tmp_path), role="stream"))

    def test_fail_when_stream_hold_stomped(self, monkeypatch, tmp_path):
        _install(monkeypatch, {"NDI 2ME PGM": 971})
        with pytest.raises(SystemExit):
            obs_phase2.verify_measurement_pins(
                _Args(host="stream", password="", profile=_profile_file(tmp_path), role="stream"))
