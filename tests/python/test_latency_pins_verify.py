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
        monkeypatch.setattr(lpv, "read_live_pins", lambda host, pw, names: ({}, None))
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
        # to the shallow drift-guard REFERENCE. issue 1168 lever 1 re-tuned cam2 6 -> 3 (the projection
        # probe's leftover pin), so the current baseline is 3/3/20. A live read matching it -> 0; any
        # drift off it -> 1. (Production alignment itself is now the per-run floor-3 auto-align,
        # scripts/qr_align_pins.py; this verify path stays the report-only drift check.)
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: ({"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 20}, None),
        )
        assert lpv.main(["--box", "strih", "--host", "x"]) == 0
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: ({"NDI cam1": 90, "NDI cam2": 3, "NDI cam3": 20}, None),
        )
        assert lpv.main(["--box", "strih", "--host", "x"]) == 1


# ---------------------------------------------------------------------------
# #1295 -- the RESOLUME-SNV prefix-match sentinel (_ndi_inputs_matching): every live NDI input
# whose name matches the regex must equal ms; NON-matching inputs (NDIAr/VBAN overlays) untouched.
# ---------------------------------------------------------------------------
class TestPrefixMatchSentinel1295:
    _RES = {"_comment": "x", "_ndi_inputs_matching": {"regex": "(?i)^sp-.*_video$", "ms": 3}}

    def test_parse_match_spec_none_when_absent(self):
        assert lpv.parse_match_spec({"NDI cam1": 3}) is None

    def test_parse_match_spec_returns_regex_and_ms(self):
        compiled, ms = lpv.parse_match_spec(self._RES)
        assert ms == 3 and compiled.search("sp-fast_video") and compiled.search("SP-Slow_video")
        assert not compiled.search("NDIAr ppt")

    def test_parse_match_spec_rejects_malformed(self):
        for bad in ({"_ndi_inputs_matching": {"regex": "", "ms": 3}},
                    {"_ndi_inputs_matching": {"regex": "x", "ms": "3"}},
                    {"_ndi_inputs_matching": {"regex": "x", "ms": True}},
                    {"_ndi_inputs_matching": {"regex": "(", "ms": 3}},
                    {"_ndi_inputs_matching": [1, 2]}):
            with pytest.raises(ValueError):
                lpv.parse_match_spec(bad)

    def test_matching_names_case_insensitive_prefix(self):
        compiled, _ = lpv.parse_match_spec(self._RES)
        got = lpv.matching_names(compiled, ["sp-fast_video", "SP-Slow_video", "NDIAr ppt", "VBAN cg"])
        assert got == ["SP-Slow_video", "sp-fast_video"]

    def test_baseline_names_enumerates_for_match_box(self):
        # must enumerate every live NDI input (None) so the regex can pick the matching subset.
        assert lpv.baseline_names(self._RES) is None

    def test_verify_flags_only_matching_inputs_off_ms(self):
        live = {"sp-fast_video": 3, "sp-slow_video": 99, "NDIAr ppt": 50, "VBAN cg-resolume": 7}
        drifts = lpv.verify_box("resolume", self._RES, live)
        assert len(drifts) == 1 and 'input="sp-slow_video"' in drifts[0] and "got=99ms want=3ms" in drifts[0]
        assert all("NDIAr" not in d and "VBAN" not in d for d in drifts)

    def test_verify_all_matching_at_ms_is_clean(self):
        assert lpv.verify_box("resolume", self._RES, {"sp-fast_video": 3, "NDIAr ppt": 50}) == []

    def test_verify_missing_matching_na_is_drift(self):
        drifts = lpv.verify_box("resolume", self._RES, {"sp-fast_video": None})
        assert len(drifts) == 1 and "got=N/A" in drifts[0]

    def test_main_clean_exits_0(self, monkeypatch):
        monkeypatch.setattr(lpv, "read_live_pins",
                            lambda host, pw, names: ({"sp-fast_video": 3, "sp-slow_video": 3, "NDIAr ppt": 99}, None))
        assert lpv.main(["--box", "resolume", "--host", "resolume.lan"]) == 0

    def test_main_drift_exits_1(self, monkeypatch):
        monkeypatch.setattr(lpv, "read_live_pins",
                            lambda host, pw, names: ({"sp-fast_video": 3, "sp-slow_video": 33}, None))
        assert lpv.main(["--box", "resolume", "--host", "resolume.lan"]) == 1

    def test_main_zero_matching_inputs_fails_closed_exit_2(self, monkeypatch):
        # the sp-* inputs could not be found/read -> the scoped pin is unconfirmed -> FAIL CLOSED,
        # never a vacuous green (the burn-target-enumeration / camera-active-set fail-open ban).
        monkeypatch.setattr(lpv, "read_live_pins",
                            lambda host, pw, names: ({"NDIAr ppt": 3, "VBAN cg-resolume": 3}, None))
        assert lpv.main(["--box", "resolume", "--host", "resolume.lan"]) == 2


# #1295 -- the baseline file carries a resolume prefix-match block (resolume is the 4th managed box).
def test_baseline_file_has_resolume_prefix_match_sentinel_1295():
    path = _SCRIPTS / "latency-pins-baseline.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    assert {"strih", "stream", "imag", "resolume"}.issubset(data.keys())
    spec = data["resolume"]["_ndi_inputs_matching"]
    compiled, ms = lpv.parse_match_spec(data["resolume"])
    assert ms == 3 and spec["regex"] == "(?i)^sp-.*_video$"
    assert compiled.search("sp-fast_video") and not compiled.search("NDIAr ppt")


# ---------------------------------------------------------------------------
# #1295 follow-up B -- an ABSENT genlock_latency_ms_src on a GENLOCK BUILD box is the build
# DEFAULT (ndi-source.cpp ndi_source_getdefaults registers genlock_latency_ms_src=3), NOT drift.
# The build identity is read over the EXISTING WS via GetInputDefaultSettings(ndi_source); a stock
# build has no such default key -> keep N/A + DRIFT.
# ---------------------------------------------------------------------------
def _fake_rpc_with_default(default_settings):
    """A _rpc fake whose GetInputDefaultSettings(ndi_source) returns default_settings (a dict) or,
    when default_settings is None, an empty defaults dict (stock build)."""
    def _rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "GetInputDefaultSettings":
            assert (rdata or {}).get("inputKind") == "ndi_source"
            return {"defaultInputSettings": dict(default_settings or {})}
        if rtype == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": v["inputKind"]} for n, v in ws._inputs.items()]}
        if rtype == "GetInputSettings":
            name = (rdata or {}).get("inputName")
            node = ws._inputs.get(name, {})
            return {"inputSettings": dict(node.get("settings", {}))}
        raise AssertionError(f"unexpected rpc {rtype}")
    return _rpc


class TestGenlockBuildDefault1295:
    def test_read_genlock_default_ms_genlock_build(self, monkeypatch):
        # GetInputDefaultSettings carries the fork-registered default -> that int (genlock build).
        monkeypatch.setattr(lpv, "_rpc", _fake_rpc_with_default({"genlock_latency_ms_src": 3, "genlock_fifo": True}))
        assert lpv.read_genlock_default_ms(_FakeWs({})) == 3

    def test_read_genlock_default_ms_stock_build_is_none(self, monkeypatch):
        # stock DistroAV has no such default key -> None (not a genlock build).
        monkeypatch.setattr(lpv, "_rpc", _fake_rpc_with_default({"ndi_sync": 2, "ndi_bw_mode": 0}))
        assert lpv.read_genlock_default_ms(_FakeWs({})) is None

    def test_read_genlock_default_ms_read_failure_is_none(self, monkeypatch):
        def _boom(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
            raise RuntimeError("ws error")
        monkeypatch.setattr(lpv, "_rpc", _boom)
        assert lpv.read_genlock_default_ms(_FakeWs({})) is None

    def test_diff_pin_absent_on_genlock_build_is_ok_not_drift(self):
        # got=None (absent key) + build_default=3 matching want=3 -> OK (None), not N/A DRIFT.
        assert lpv.diff_pin("sp-fast_video", None, 3, build_default=3) is None

    def test_diff_pin_absent_on_genlock_build_wrong_default_is_drift_named(self):
        msg = lpv.diff_pin("sp-fast_video", None, 3, build_default=6)
        assert msg is not None and "default(6)" in msg and "want=3ms" in msg

    def test_diff_pin_absent_stock_still_na_drift(self):
        # build_default=None (stock) keeps the N/A drift path verbatim.
        msg = lpv.diff_pin("sp-fast_video", None, 3, build_default=None)
        assert msg is not None and "got=N/A" in msg

    def test_verify_box_threads_build_default(self):
        RES = {"_ndi_inputs_matching": {"regex": "(?i)^sp-.*_video$", "ms": 3}}
        live = {"sp-fast_video": None, "sp-slow_video": None, "NDIAr ppt": None}
        # genlock build (default 3): every sp-* absent-key resolves to default(3)=OK -> no drift.
        assert lpv.verify_box("resolume", RES, live, build_default=3) == []
        # stock (no default): every sp-* absent-key -> N/A DRIFT.
        drifts = lpv.verify_box("resolume", RES, live, build_default=None)
        assert len(drifts) == 2 and all("got=N/A" in d for d in drifts)

    def test_main_resolume_genlock_default_absent_keys_exit_0(self, monkeypatch):
        # read_live_pins now returns (pins, build_default); a genlock box with absent sp-* keys + a
        # build default of 3 is a CLEAN pass (exit 0), not the false N/A DRIFT exit 1.
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: ({"sp-fast_video": None, "sp-slow_video": None, "NDIAr ppt": None}, 3),
        )
        assert lpv.main(["--box", "resolume", "--host", "resolume.lan"]) == 0

    def test_main_resolume_stock_absent_keys_exit_1(self, monkeypatch):
        # a STOCK box (build_default None) with absent sp-* keys is still DRIFT (exit 1).
        monkeypatch.setattr(
            lpv, "read_live_pins",
            lambda host, pw, names: ({"sp-fast_video": None, "sp-slow_video": None}, None),
        )
        assert lpv.main(["--box", "resolume", "--host", "resolume.lan"]) == 1
