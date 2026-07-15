"""#757 -- unit tests for scripts/imag_latency_enforce.py, the imag-always-minimum-latency
self-healing enforcement (binding user directive, 2026-07-15: imag never gets per-camera pin
equalization -- every NDI input stays pinned at the 3ms floor, always).

Covers, with NO live OBS/rig:
  a. is_ndi_kind() -- case-insensitive substring match, honest False for a non-string/None kind.
  b. list_ndi_inputs() -- filters GetInputList's raw array to NDI-kind input NAMES only, in
     order, skipping malformed entries; NEVER a hardcoded cam1..7 list (imag's real input set
     includes 2 non-camera NDI sources too -- confirmed live, #757).
  c. enforce_fixed_latency() -- read-current, no-op when already compliant, SET+verify
     read-back when drifted, FAIL LOUD (SystemExit) on a read-back mismatch. Exercised against
     a FAKE ws object (a minimal _rpc-compatible stub), matching
     tests/python/test_phase_sync_calibrate.py's own fake-ws convention.
"""
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import imag_latency_enforce as ile  # noqa: E402


# ---------------------------------------------------------------------------
# is_ndi_kind / list_ndi_inputs -- pure
# ---------------------------------------------------------------------------

class TestIsNdiKind:
    def test_matches_the_real_live_kind_string(self):
        assert ile.is_ndi_kind("ndi_source") is True

    def test_case_insensitive(self):
        assert ile.is_ndi_kind("NDI_Source") is True

    def test_rejects_a_non_ndi_kind(self):
        assert ile.is_ndi_kind("ffmpeg_source") is False

    def test_rejects_none_and_non_string(self):
        assert ile.is_ndi_kind(None) is False
        assert ile.is_ndi_kind(42) is False


class TestListNdiInputs:
    def test_filters_to_ndi_kind_only_preserving_order(self):
        inputs = [
            {"inputName": "NDI CAM1", "inputKind": "ndi_source"},
            {"inputName": "Color Source", "inputKind": "color_source_v3"},
            {"inputName": "MV CAM1", "inputKind": "ndi_source"},
        ]
        assert ile.list_ndi_inputs(inputs) == ["NDI CAM1", "MV CAM1"]

    def test_includes_non_camera_ndi_sources_never_a_hardcoded_cam_list(self):
        # Live-confirmed 2026-07-15: imag's real 16-input set includes 2 non-camera NDI
        # sources ("NDI resolume imag", "MW imag resolume") -- a hardcoded cam1..7 filter
        # would silently leave these unenforced.
        inputs = [
            {"inputName": "NDI CAM3", "inputKind": "ndi_source"},
            {"inputName": "NDI resolume imag", "inputKind": "ndi_source"},
            {"inputName": "MW imag resolume", "inputKind": "ndi_source"},
        ]
        assert ile.list_ndi_inputs(inputs) == [
            "NDI CAM3",
            "NDI resolume imag",
            "MW imag resolume",
        ]

    def test_skips_a_malformed_entry_without_crashing(self):
        inputs = [
            {"inputName": "NDI CAM1", "inputKind": "ndi_source"},
            "not-a-dict",
            {"inputKind": "ndi_source"},  # no inputName
            {"inputName": 42, "inputKind": "ndi_source"},  # non-string name
        ]
        assert ile.list_ndi_inputs(inputs) == ["NDI CAM1"]

    def test_empty_input_list_returns_empty(self):
        assert ile.list_ndi_inputs([]) == []


# ---------------------------------------------------------------------------
# enforce_fixed_latency -- live (fake ws)
# ---------------------------------------------------------------------------

class _FakeWS:
    """Minimal _rpc-compatible fake: GetInputSettings/SetInputSettings against an in-memory
    {source: latency_ms} table. Mirrors test_phase_sync_calibrate.py's own fake-ws shape."""

    def __init__(self, initial: dict):
        self.state = dict(initial)
        self.set_calls = []

    def recv(self):  # pragma: no cover - not used, _rpc is monkeypatched instead
        raise NotImplementedError


def _fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
    if rtype == "GetInputSettings":
        name = rdata["inputName"]
        val = ws.state.get(name)
        settings = {} if val is None else {ile.GENLOCK_SRC_LATENCY_KEY: val}
        return {"inputSettings": settings}
    if rtype == "SetInputSettings":
        name = rdata["inputName"]
        new_val = rdata["inputSettings"][ile.GENLOCK_SRC_LATENCY_KEY]
        ws.set_calls.append((name, new_val))
        ws.state[name] = new_val
        return {}
    raise AssertionError(f"unexpected rpc type in test: {rtype}")


class TestEnforceFixedLatency:
    def test_already_compliant_source_is_a_noop(self, monkeypatch):
        monkeypatch.setattr(ile, "_rpc", _fake_rpc)
        ws = _FakeWS({"NDI CAM1": 3})
        results = ile.enforce_fixed_latency(ws, ["NDI CAM1"], target_ms=3)
        assert results == [
            {"source": "NDI CAM1", "before_ms": 3, "after_ms": 3, "corrected": False}
        ]
        assert ws.set_calls == [], "must not issue a SetInputSettings when already compliant"

    def test_drifted_source_is_corrected_and_verified(self, monkeypatch):
        monkeypatch.setattr(ile, "_rpc", _fake_rpc)
        ws = _FakeWS({"NDI CAM1": 67})
        results = ile.enforce_fixed_latency(ws, ["NDI CAM1"], target_ms=3)
        assert results == [
            {"source": "NDI CAM1", "before_ms": 67, "after_ms": 3, "corrected": True}
        ]
        assert ws.set_calls == [("NDI CAM1", 3)]
        assert ws.state["NDI CAM1"] == 3

    def test_multiple_sources_mixed_compliant_and_drifted(self, monkeypatch):
        monkeypatch.setattr(ile, "_rpc", _fake_rpc)
        ws = _FakeWS({"NDI CAM1": 3, "MV CAM1": 67, "NDI CAM2": 3})
        results = ile.enforce_fixed_latency(ws, ["NDI CAM1", "MV CAM1", "NDI CAM2"], target_ms=3)
        corrected = [r["source"] for r in results if r["corrected"]]
        assert corrected == ["MV CAM1"]

    def test_readback_mismatch_fails_loud(self, monkeypatch):
        # A rigged _rpc whose SetInputSettings "succeeds" but the read-back never reflects it
        # (the #292 force-drain class) -- must raise, never silently report success.
        def _broken_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
            if rtype == "GetInputSettings":
                return {"inputSettings": {ile.GENLOCK_SRC_LATENCY_KEY: 67}}
            if rtype == "SetInputSettings":
                return {}
            raise AssertionError(rtype)

        monkeypatch.setattr(ile, "_rpc", _broken_rpc)
        ws = _FakeWS({"NDI CAM1": 67})
        with pytest.raises(SystemExit, match="FAILED to set"):
            ile.enforce_fixed_latency(ws, ["NDI CAM1"], target_ms=3)

    def test_empty_names_list_returns_empty(self, monkeypatch):
        monkeypatch.setattr(ile, "_rpc", _fake_rpc)
        ws = _FakeWS({})
        assert ile.enforce_fixed_latency(ws, [], target_ms=3) == []
