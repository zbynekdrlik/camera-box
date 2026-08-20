"""#1061 -- unit tests for scripts/latency_pins_verify.py, the latency-pin verify-at-start
REPORT-only drift check (issue 866 latency half).

Unlike the burn half (#1057, force OFF), per-source `genlock_latency_ms_src` is the operator's
A/V-align domain (repo memory "latency is user's A/V-align domain"), so the start path may only
REPORT drift against a committed agreed-pins baseline, NEVER overwrite. These tests exercise the
PURE diff logic with NO live OBS/rig, plus the WS reader against a FAKE ws stub (the same
`monkeypatch.setattr(mod, "_rpc", fake)` convention tests/python/test_imag_latency_enforce.py uses).
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import latency_pins_verify as lpv  # noqa: E402


# ---------------------------------------------------------------------------
# normalize_spec -- pure
# ---------------------------------------------------------------------------
class TestNormalizeSpec:
    def test_bare_int_is_exact_zero_tolerance(self):
        assert lpv.normalize_spec(3) == (3, 0)

    def test_dict_with_want_and_tolerance(self):
        assert lpv.normalize_spec({"want_ms": 915, "tolerance_ms": 60}) == (915, 60)

    def test_dict_tolerance_defaults_to_zero(self):
        assert lpv.normalize_spec({"want_ms": 6}) == (6, 0)

    def test_bool_is_rejected(self):
        # bool is an int subclass -- a True/False pin is malformed, never silently 1/0
        with pytest.raises(ValueError):
            lpv.normalize_spec(True)

    def test_missing_want_is_rejected(self):
        with pytest.raises(ValueError):
            lpv.normalize_spec({"tolerance_ms": 5})

    def test_negative_tolerance_is_rejected(self):
        with pytest.raises(ValueError):
            lpv.normalize_spec({"want_ms": 3, "tolerance_ms": -1})

    def test_non_int_spec_is_rejected(self):
        with pytest.raises(ValueError):
            lpv.normalize_spec("3")


# ---------------------------------------------------------------------------
# diff_pin -- pure
# ---------------------------------------------------------------------------
class TestDiffPin:
    def test_exact_match_no_drift(self):
        assert lpv.diff_pin("NDI cam1", 3, 3) is None

    def test_within_band_no_drift(self):
        assert lpv.diff_pin("NDI 2ME PGM", 923, {"want_ms": 915, "tolerance_ms": 60}) is None

    def test_band_boundary_inclusive(self):
        assert lpv.diff_pin("NDI 2ME PGM", 975, {"want_ms": 915, "tolerance_ms": 60}) is None
        assert lpv.diff_pin("NDI 2ME PGM", 855, {"want_ms": 915, "tolerance_ms": 60}) is None

    def test_outside_band_is_drift_naming_got_and_want(self):
        msg = lpv.diff_pin("NDI 2ME PGM", 0, {"want_ms": 915, "tolerance_ms": 60})
        assert msg is not None
        assert "NDI 2ME PGM" in msg
        assert "got=0" in msg
        assert "want=915" in msg

    def test_exact_mismatch_is_drift(self):
        msg = lpv.diff_pin("NDI cam1", 73, 3)
        assert msg is not None
        assert "got=73" in msg and "want=3" in msg

    def test_missing_live_pin_is_drift_reported_as_na(self):
        msg = lpv.diff_pin("NDI cam2", None, 6)
        assert msg is not None
        assert "N/A" in msg
        assert "want=6" in msg


# ---------------------------------------------------------------------------
# verify_box -- pure (explicit names + floor sentinel)
# ---------------------------------------------------------------------------
class TestVerifyBoxExplicit:
    def test_all_pins_at_baseline_no_drift(self):
        baseline = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        live = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        assert lpv.verify_box("strih", baseline, live) == []

    def test_866_revert_scenario_all_flagged(self):
        # #866: a restart brought strih back at the rejected/unjustified persisted values.
        baseline = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        live = {"NDI cam1": 73, "NDI cam2": 68, "NDI cam3": 78}
        drifts = lpv.verify_box("strih", baseline, live)
        assert len(drifts) == 3
        assert all(d.startswith("box=strih ") for d in drifts)
        joined = "\n".join(drifts)
        assert "NDI cam1" in joined and "got=73" in joined

    def test_stream_hold_within_band_is_clean(self):
        baseline = {"NDI 2ME PGM": {"want_ms": 915, "tolerance_ms": 60}}
        assert lpv.verify_box("stream", baseline, {"NDI 2ME PGM": 923}) == []

    def test_underscore_sentinel_keys_are_not_treated_as_named_pins(self):
        baseline = {"_comment": "note", "NDI cam1": 3}
        assert lpv.verify_box("strih", baseline, {"NDI cam1": 3}) == []


class TestVerifyBoxFloor:
    def test_imag_floor_all_at_three_is_clean(self):
        baseline = {"_all_ndi_inputs_ms": 3}
        live = {"NDI CAM1": 3, "NDI CAM2": 3, "MV CAM1": 3, "NDI resolume imag": 3}
        assert lpv.verify_box("imag", baseline, live) == []

    def test_imag_floor_flags_any_input_off_the_floor(self):
        baseline = {"_all_ndi_inputs_ms": 3}
        live = {"NDI CAM1": 3, "NDI CAM2": 67, "MV CAM1": 3}
        drifts = lpv.verify_box("imag", baseline, live)
        assert len(drifts) == 1
        assert "NDI CAM2" in drifts[0] and "got=67" in drifts[0] and "want=3" in drifts[0]


# ---------------------------------------------------------------------------
# baseline_names -- picks enumerate (None) for a floor box, explicit names otherwise
# ---------------------------------------------------------------------------
class TestBaselineNames:
    def test_floor_box_enumerates(self):
        assert lpv.baseline_names({"_all_ndi_inputs_ms": 3}) is None

    def test_explicit_box_lists_named_pins_only(self):
        got = lpv.baseline_names({"NDI cam1": 3, "NDI cam2": 6, "_comment": "x"})
        assert sorted(got) == ["NDI cam1", "NDI cam2"]


# ---------------------------------------------------------------------------
# read_pins_over_ws -- fake ws + monkeypatched _rpc (mirrors imag test convention)
# ---------------------------------------------------------------------------
class _FakeWs:
    """Minimal ws: in-memory input table {name: {inputKind, settings}}."""

    def __init__(self, inputs):
        self._inputs = inputs

    def close(self):  # pragma: no cover
        pass


def _fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
    if rtype == "GetInputList":
        return {"inputs": [{"inputName": n, "inputKind": v["inputKind"]} for n, v in ws._inputs.items()]}
    if rtype == "GetInputSettings":
        name = (rdata or {}).get("inputName")
        node = ws._inputs.get(name, {})
        return {"inputSettings": dict(node.get("settings", {}))}
    raise AssertionError(f"unexpected rpc {rtype}")


class TestReadPinsOverWs:
    def test_explicit_names_read_the_genlock_key(self, monkeypatch):
        monkeypatch.setattr(lpv, "_rpc", _fake_rpc)
        ws = _FakeWs({
            "NDI cam1": {"inputKind": "ndi_source", "settings": {"genlock_latency_ms_src": 3}},
            "NDI cam2": {"inputKind": "ndi_source", "settings": {"genlock_latency_ms_src": 6}},
        })
        got = lpv.read_pins_over_ws(ws, ["NDI cam1", "NDI cam2"])
        assert got == {"NDI cam1": 3, "NDI cam2": 6}

    def test_missing_key_is_honest_none(self, monkeypatch):
        monkeypatch.setattr(lpv, "_rpc", _fake_rpc)
        ws = _FakeWs({"cg": {"inputKind": "ndi_source", "settings": {}}})
        assert lpv.read_pins_over_ws(ws, ["cg"]) == {"cg": None}

    def test_enumerate_reads_only_ndi_kind_inputs(self, monkeypatch):
        monkeypatch.setattr(lpv, "_rpc", _fake_rpc)
        ws = _FakeWs({
            "NDI CAM1": {"inputKind": "ndi_source", "settings": {"genlock_latency_ms_src": 3}},
            "some text": {"inputKind": "text_gdiplus_v3", "settings": {}},
            "MV CAM1": {"inputKind": "ndi_source", "settings": {"genlock_latency_ms_src": 3}},
        })
        got = lpv.read_pins_over_ws(ws, None)
        assert set(got) == {"NDI CAM1", "MV CAM1"}


# ---------------------------------------------------------------------------
# the committed baseline file is well-formed + covers strih/stream/imag
# ---------------------------------------------------------------------------
class TestCommittedBaseline:
    def test_baseline_file_loads_and_has_the_three_boxes(self):
        path = _SCRIPTS / "latency-pins-baseline.json"
        data = json.loads(path.read_text(encoding="utf-8"))
        assert set(["strih", "stream", "imag"]).issubset(data.keys())
        # every strih/stream entry normalizes cleanly
        for box in ("strih", "stream"):
            for name, spec in data[box].items():
                if name.startswith("_"):
                    continue
                want, tol = lpv.normalize_spec(spec)
                assert want >= 0 and tol >= 0
        # imag is the floor sentinel
        assert data["imag"].get("_all_ndi_inputs_ms") == 3


# ---------------------------------------------------------------------------
# FAIL-CLOSED enumeration (the floor path must never be a vacuous green)
# ---------------------------------------------------------------------------
class TestEnumerationFailsClosed:
    def test_getinputlist_non_dict_raises(self, monkeypatch):
        # A swallowed/errored GetInputList (returning None) must RAISE, not silently enumerate 0.
        def _rpc_bad_list(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
            if rtype == "GetInputList":
                return None
            raise AssertionError("must not reach per-input reads when the list is unusable")

        monkeypatch.setattr(lpv, "_rpc", _rpc_bad_list)
        with pytest.raises(ValueError):
            lpv.read_pins_over_ws(_FakeWs({}), None)

    def test_getinputlist_missing_inputs_key_raises(self, monkeypatch):
        def _rpc_no_inputs(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
            if rtype == "GetInputList":
                return {"notinputs": []}
            raise AssertionError("unreachable")

        monkeypatch.setattr(lpv, "_rpc", _rpc_no_inputs)
        with pytest.raises(ValueError):
            lpv.read_pins_over_ws(_FakeWs({}), None)

    def test_main_floor_box_empty_enumeration_exits_2(self, monkeypatch):
        # imag (floor box) that reads ZERO inputs is FAIL-CLOSED (exit 2), never a green exit 0.
        monkeypatch.setattr(lpv, "read_live_pins", lambda host, pw, names: {})
        rc = lpv.main(["--box", "imag", "--host", "10.77.9.182"])
        assert rc == 2

    def test_main_connect_failure_exits_2(self, monkeypatch):
        def _boom(host, pw, names):
            raise ConnectionError("unreachable box")

        monkeypatch.setattr(lpv, "read_live_pins", _boom)
        rc = lpv.main(["--box", "strih", "--host", "10.77.9.202"])
        assert rc == 2

    def test_main_drift_exits_1_clean_exits_0(self, monkeypatch):
        # #1003 owner rework (2026-08-20): the deep promoted 90/160/184 set was REJECTED + REVERTED
        # to the shallow 3/6/20 drift-guard REFERENCE. A live read matching the reverted baseline ->
        # 0; any drift off it -> 1. (Production alignment itself is now the per-run floor-3 auto-align,
        # scripts/qr_align_pins.py; this verify path stays the report-only drift check.)
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20},
        )
        assert lpv.main(["--box", "strih", "--host", "x"]) == 0
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: {"NDI cam1": 90, "NDI cam2": 6, "NDI cam3": 20},
        )
        assert lpv.main(["--box", "strih", "--host", "x"]) == 1
