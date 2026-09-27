"""issue 1367 -- the PURE decision core of the 8 h stream-output A/V soak.

Owner goal (24.9.2026): on the stream OBS output the picture/sound offset and the camera-to-camera
alignment must hold for 8 h without drifting; any real drift already shows after about 1 h. The
soak orchestrator (scripts/av-soak.sh) records one short window per 10-minute slot, decodes it with
the same recording-verdict the E2E gate uses, and appends ONE CSV row per window. This module turns
that CSV into the verdict:

  - every sample graded against the gate's own bounds, READ from their single sources
    (`AV_OFFSET_GATE_TOLERANCE_MS` in src/av_window.rs, `SPREAD_THRESHOLD_MS` in
    src/switch_latency.rs) -- never retyped;
  - a least-squares slope per series (ms/h), bound 2 ms/h;
  - a 1 h partial report and the full report;
  - an empty / partial / gappy CSV is UNKNOWN, never a false PASS.

Tier-0: pure python, no rig, no cargo. Runnable directly or under pytest (the python-tests CI job).
"""
import csv
import importlib.util
import json
import os
import subprocess
import sys
import tempfile

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
MODULE = os.path.join(REPO, "scripts", "av_soak_decision.py")

_spec = importlib.util.spec_from_file_location("av_soak_decision", MODULE)
asd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(asd)

CAMS = ["cam1", "cam2", "cam3"]
T0 = 1_790_000_000  # an arbitrary epoch (2026-09) for the synthetic runs
SLOT = 600


# --- helpers -----------------------------------------------------------------------------------


def _bounds(av=None, spread=None):
    real = asd.load_gate_bounds(REPO)
    return {
        "av_tolerance_ms": real["av_tolerance_ms"] if av is None else av,
        "spread_threshold_ms": real["spread_threshold_ms"] if spread is None else spread,
        "sources": real["sources"],
    }


def _verdict(av=None, expected=0.0, source_spread=None, delivery_spread=None, segments=None,
             burn_loss=None):
    """A minimal merged-verdict JSON in the shape recording-verdict writes."""
    v = {}
    if av is not None:
        block = {"expected_ms": expected, "gate_tolerance_ms": 30.0, "judged_cameras": len(av)}
        for cam, (status, off) in av.items():
            block[cam] = {"node": cam, "verdict": status, "av_offset_ms": off,
                          "gate_pass": status == "measured"}
        v["all_cambox_av_sync"] = block
    v["all_cambox_latency"] = {"cross_camera_spread_ms": source_spread,
                               "spread_gate_pass": None}
    v["all_cambox_delivery_latency"] = {"cross_camera_spread_ms": delivery_spread,
                                        "spread_gate_pass": None}
    if segments is not None:
        v["all_cambox_continuity"] = {"segments": segments}
    if burn_loss is not None:
        v["full_chain"] = {"loss": burn_loss}
    return v


def _meta(slot, epoch=None, outcome="ok"):
    epoch = T0 + slot * SLOT if epoch is None else epoch
    return {"ts_utc": asd.utc_iso(epoch), "epoch_s": epoch, "slot": slot, "window_s": 90,
            "outcome": outcome, "verdict_rc": 1, "verdict_path": f"/runs/s{slot}/verdict.json",
            "painter_run_id": "123"}


def _clean_row(slot, av_by_cam=None, spread=10.0, loss_pass=True, epoch=None, expected=0.0,
               delivery=None):
    av_by_cam = av_by_cam or {"cam1": 1.0, "cam2": 2.0, "cam3": -1.0}
    segs = [{"cambox": c.upper(), "pass": loss_pass, "copies": 0 if loss_pass else 2, "gaps": 0,
             "undecodable": 0, "frames": 900} for c in CAMS]
    v = _verdict(av={c: ("measured", o) for c, o in av_by_cam.items()}, expected=expected,
                 source_spread=spread, delivery_spread=delivery if delivery is not None else spread,
                 segments=segs)
    return asd.row_from_verdict(v, CAMS, _meta(slot, epoch=epoch))


def _run(rows, **kw):
    kw.setdefault("bounds", _bounds())
    kw.setdefault("min_duration_s", 3600)
    kw.setdefault("max_gap_s", asd.DEFAULT_MAX_GAP_S)
    kw.setdefault("slope_bound", asd.SLOPE_BOUND_MS_PER_H)
    kw.setdefault("spread_columns", ("source_spread_ms", "delivery_spread_ms"))
    return asd.evaluate(rows, CAMS, **kw)


def _stringify(row):
    """What a row looks like after a CSV round trip (every value a string)."""
    return {k: ("" if v is None else str(v)) for k, v in row.items()}


# --- the bounds are READ from their single sources ----------------------------------------------


def test_bounds_are_read_from_the_rust_sources():
    b = asd.load_gate_bounds(REPO)
    assert b["av_tolerance_ms"] > 0 and b["spread_threshold_ms"] > 0
    assert b["sources"]["av_tolerance_ms"].endswith("src/av_window.rs AV_OFFSET_GATE_TOLERANCE_MS")
    assert b["sources"]["spread_threshold_ms"].endswith(
        "src/switch_latency.rs SPREAD_THRESHOLD_MS")


def test_bounds_follow_the_source_file_not_a_literal(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "av_window.rs").write_text(
        "/// doc\npub const AV_OFFSET_GATE_TOLERANCE_MS: f64 = 31.5;\n")
    (tmp_path / "src" / "switch_latency.rs").write_text(
        "pub const SPREAD_THRESHOLD_MS: f64 = 17.25;\n")
    b = asd.load_gate_bounds(str(tmp_path))
    assert b["av_tolerance_ms"] == 31.5
    assert b["spread_threshold_ms"] == 17.25


def test_a_missing_bound_constant_fails_closed(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "av_window.rs").write_text("pub const OTHER: f64 = 1.0;\n")
    (tmp_path / "src" / "switch_latency.rs").write_text(
        "pub const SPREAD_THRESHOLD_MS: f64 = 24.0;\n")
    with pytest.raises(asd.BoundsError):
        asd.load_gate_bounds(str(tmp_path))
    with pytest.raises(asd.BoundsError):
        asd.load_gate_bounds(str(tmp_path / "nope"))


def test_parse_rust_f64_const():
    assert asd.parse_rust_f64_const("pub const X: f64 = 12.5;", "X") == 12.5
    assert asd.parse_rust_f64_const("pub const X: f64 = 3.0; // c", "X") == 3.0
    assert asd.parse_rust_f64_const("pub const XY: f64 = 3.0;", "X") is None
    assert asd.parse_rust_f64_const("", "X") is None


# --- slope fit ----------------------------------------------------------------------------------


def test_slope_of_a_line_is_exact_in_ms_per_hour():
    pts = [(T0 + i * 600, 5.0 + 3.0 * (i * 600) / 3600.0) for i in range(7)]
    assert asd.fit_slope_ms_per_h(pts) == pytest.approx(3.0)


def test_slope_of_a_constant_is_zero_and_degenerate_input_is_none():
    assert asd.fit_slope_ms_per_h([(T0 + i * 600, 4.0) for i in range(4)]) == pytest.approx(0.0)
    assert asd.fit_slope_ms_per_h([(T0, 1.0)]) is None
    assert asd.fit_slope_ms_per_h([(T0, 1.0), (T0, 2.0)]) is None
    assert asd.fit_slope_ms_per_h([]) is None


# --- one CSV row per window ---------------------------------------------------------------------


def test_row_from_verdict_extracts_the_measured_offsets_spreads_and_loss():
    segs = [
        {"cambox": "CAM1", "pass": True, "copies": 0, "gaps": 1, "undecodable": 2, "frames": 400},
        {"cambox": "CAM1", "pass": False, "copies": 3, "gaps": 0, "undecodable": 0, "frames": 500},
        {"cambox": "CAM3", "pass": True, "copies": 0, "gaps": 0, "undecodable": 0, "frames": 900},
    ]
    v = _verdict(av={"cam1": ("measured", 10.0), "cam2": ("measured", 3.0),
                     "cam3": ("measured", -5.0)},
                 expected=0.0, source_spread=None, delivery_spread=12.3, segments=segs,
                 burn_loss={"cam1": {"zero_loss": True, "real_drops": 0}})
    row = asd.row_from_verdict(v, CAMS, _meta(4))
    assert set(row) == set(asd.csv_fields(CAMS))
    assert row["slot"] == "4" and row["outcome"] == "ok"
    assert row["av_cam1_ms"] == "10.000" and row["av_cam1_status"] == "measured"
    assert row["av_cam3_ms"] == "-5.000"
    assert row["av_expected_ms"] == "0.000"
    # cam2's number pools the WHOLE recording (whatever camera is on program), so it is not a
    # camera path and stays out of the stream-output cross-camera spread
    assert row["av_spread_ms"] == "15.000" and row["av_spread_cams"] == "2"
    assert row["source_spread_ms"] == "" and row["delivery_spread_ms"] == "12.300"
    # a camera's segments aggregate; its loss pass is the AND of the verdict's own segment passes
    assert row["loss_cam1_frames"] == "900" and row["loss_cam1_copies"] == "3"
    assert row["loss_cam1_gaps"] == "1" and row["loss_cam1_undecodable"] == "2"
    assert row["loss_cam1_pass"] == "false" and row["loss_cam3_pass"] == "true"
    assert row["loss_cam2_pass"] == ""  # no segment for cam2 in this window
    assert row["burn_cam1_zero_loss"] == "true" and row["burn_cam1_real_drops"] == "0"
    assert row["burn_cam3_zero_loss"] == ""


def test_a_derived_or_unknown_offset_is_never_a_sample():
    v = _verdict(av={"cam1": ("derived", None), "cam2": ("unknown", None),
                     "cam3": ("excluded", None)})
    v["all_cambox_av_sync"]["cam1"]["derived_offset_ms"] = 7.0
    row = asd.row_from_verdict(v, CAMS, _meta(0))
    assert row["av_cam1_ms"] == "" and row["av_cam1_status"] == "derived"
    assert row["av_cam2_status"] == "unknown" and row["av_cam3_status"] == "excluded"
    assert row["av_spread_ms"] == "" and row["av_spread_cams"] == "0"


def test_a_hand_edited_non_measured_value_is_still_not_a_sample():
    # the grading keys on the status column too, not only on a non-empty value
    rows = []
    for i in range(7):
        r = _clean_row(i)
        if i == 3:
            r["av_cam1_status"] = "derived"
            r["av_cam1_ms"] = "999.000"
        rows.append(_stringify(r))
    rep = _run(rows)
    assert rep["series"]["av cam1"]["n"] == 6
    assert rep["series"]["av cam1"]["breaches"] == 0


def test_no_verdict_row_keeps_the_window_with_empty_measurements():
    row = asd.row_from_verdict(None, CAMS, _meta(2, outcome="no_verdict:decode_failed"))
    assert row["outcome"] == "no_verdict:decode_failed"
    assert row["av_cam1_ms"] == "" and row["av_cam1_status"] == "absent"
    assert row["av_expected_ms"] == "" and row["loss_cam1_pass"] == ""


def test_camera_names_are_validated():
    with pytest.raises(ValueError):
        asd.csv_fields(["cam1", "cam1; rm -rf"])
    with pytest.raises(ValueError):
        asd.csv_fields([])


def test_append_row_writes_the_header_once_and_refuses_a_schema_change(tmp_path):
    path = str(tmp_path / "soak.csv")
    asd.append_row(path, _clean_row(0), CAMS)
    asd.append_row(path, _clean_row(1), CAMS)
    with open(path, newline="") as f:
        rows = list(csv.reader(f))
    assert rows[0] == asd.csv_fields(CAMS) and len(rows) == 3
    with pytest.raises(asd.CsvSchemaError):
        asd.append_row(path, asd.row_from_verdict(None, ["cam1"], _meta(2)), ["cam1"])
    fields, read = asd.read_rows(path)
    assert fields == asd.csv_fields(CAMS) and len(read) == 2
    assert asd.cams_from_fields(fields) == CAMS


# --- grading ------------------------------------------------------------------------------------


def test_empty_csv_is_unknown_never_a_pass():
    rep = _run([])
    assert rep["verdict"] == asd.UNKNOWN
    assert asd.exit_code(rep) == 2


def test_a_clean_hour_on_the_graded_spread_passes():
    rows = [_stringify(_clean_row(i)) for i in range(7)]
    rep = _run(rows)
    assert rep["verdict"] == asd.PASS, rep["reasons"]
    assert asd.exit_code(rep) == 0
    assert rep["series"]["av cam1"]["verdict"] == asd.PASS
    assert rep["series"]["av cam1"]["slope_ms_per_h"] == pytest.approx(0.0)


def test_an_unmeasured_graded_spread_is_unknown_never_a_pass():
    rows = []
    for i in range(7):
        r = _clean_row(i)
        r["source_spread_ms"] = ""
        r["delivery_spread_ms"] = ""
        rows.append(_stringify(r))
    rep = _run(rows)
    assert rep["verdict"] == asd.UNKNOWN
    assert "spread source_spread_ms" in " ".join(rep["reasons"])
    # the stream-output spread from the A/V offsets is still reported (not graded by default)
    assert rep["series"]["spread av_spread_ms"]["graded"] is False
    assert rep["series"]["spread av_spread_ms"]["n"] == 7
    # grading it instead is one option
    rep_b = _run(rows, spread_columns=("av_spread_ms",))
    assert rep_b["verdict"] == asd.PASS, rep_b["reasons"]


def test_av_bound_is_inclusive_and_relative_to_the_expected_offset():
    tol = _bounds()["av_tolerance_ms"]
    at = [_stringify(_clean_row(i, av_by_cam={"cam1": 5.0 + tol, "cam2": 5.0, "cam3": 5.0},
                                expected=5.0)) for i in range(7)]
    assert _run(at)["verdict"] == asd.PASS
    over = [_stringify(_clean_row(i, av_by_cam={"cam1": 5.0 + tol + (0.1 if i == 3 else 0.0),
                                                "cam2": 5.0, "cam3": 5.0},
                                  expected=5.0)) for i in range(7)]
    rep = _run(over)
    assert rep["verdict"] == asd.FAIL
    assert rep["series"]["av cam1"]["breaches"] == 1
    assert asd.exit_code(rep) == 1


def test_grading_uses_the_loaded_bounds_not_a_literal():
    rows = [_stringify(_clean_row(i, av_by_cam={"cam1": 6.0, "cam2": 0.0, "cam3": 0.0}))
            for i in range(7)]
    assert _run(rows)["verdict"] == asd.PASS
    assert _run(rows, bounds=_bounds(av=5.0))["verdict"] == asd.FAIL
    assert _run(rows, bounds=_bounds(spread=9.0))["verdict"] == asd.FAIL


def test_spread_at_the_threshold_passes_and_above_fails():
    thr = _bounds()["spread_threshold_ms"]
    at = [_stringify(_clean_row(i, spread=thr)) for i in range(7)]
    assert _run(at)["verdict"] == asd.PASS
    over = [_stringify(_clean_row(i, spread=thr + (0.5 if i == 5 else 0.0))) for i in range(7)]
    rep = _run(over)
    assert rep["verdict"] == asd.FAIL
    assert rep["series"]["spread source_spread_ms"]["breaches"] == 1


def test_a_drift_inside_the_bound_still_fails_on_the_slope():
    def drift(rate):
        return [_stringify(_clean_row(i, av_by_cam={"cam1": rate * i * SLOT / 3600.0,
                                                     "cam2": 0.0, "cam3": 0.0}))
                for i in range(7)]
    rep = _run(drift(3.0))
    assert rep["verdict"] == asd.FAIL
    assert rep["series"]["av cam1"]["slope_ms_per_h"] == pytest.approx(3.0)
    assert "slope" in rep["series"]["av cam1"]["reason"]
    assert _run(drift(1.5))["verdict"] == asd.PASS
    assert _run(drift(-2.5))["verdict"] == asd.FAIL


def test_a_loss_window_fails():
    rows = [_stringify(_clean_row(i, loss_pass=(i != 2))) for i in range(7)]
    rep = _run(rows)
    assert rep["verdict"] == asd.FAIL
    assert rep["series"]["loss cam1"]["breaches"] == 1
    assert rep["series"]["loss cam1"]["copies"] == 2


def test_a_burn_loss_fails_when_it_was_measured():
    rows = []
    for i in range(7):
        r = _clean_row(i)
        r["burn_cam3_zero_loss"] = "false" if i == 1 else "true"
        rows.append(_stringify(r))
    rep = _run(rows)
    assert rep["verdict"] == asd.FAIL
    assert rep["series"]["burn cam3"]["verdict"] == asd.FAIL


def test_a_missing_window_is_a_cadence_gap_so_unknown():
    rows = [_stringify(_clean_row(i)) for i in range(8) if i != 3]
    rep = _run(rows)
    assert rep["verdict"] == asd.UNKNOWN
    assert rep["series"]["av cam1"]["max_gap_s"] == pytest.approx(2 * SLOT)
    assert "gap" in rep["series"]["av cam1"]["reason"]


def test_a_failed_decode_window_leaves_a_gap():
    rows = []
    for i in range(8):
        if i == 4:
            rows.append(_stringify(asd.row_from_verdict(None, CAMS,
                                                        _meta(i, outcome="no_verdict:merge"))))
        else:
            rows.append(_stringify(_clean_row(i)))
    rep = _run(rows)
    assert rep["verdict"] == asd.UNKNOWN
    assert rep["windows"] == 8 and rep["windows_ok"] == 7


def test_a_camera_missing_at_the_start_is_a_gap_too():
    rows = []
    for i in range(7):
        r = _clean_row(i)
        if i < 2:
            r["av_cam3_ms"] = ""
            r["av_cam3_status"] = "unknown"
        rows.append(_stringify(r))
    rep = _run(rows)
    assert rep["verdict"] == asd.UNKNOWN
    assert rep["series"]["av cam3"]["max_gap_s"] == pytest.approx(2 * SLOT)


def test_a_run_shorter_than_required_is_unknown():
    rows = [_stringify(_clean_row(i)) for i in range(4)]  # 30 min of window starts
    rep = _run(rows)
    assert rep["verdict"] == asd.UNKNOWN
    assert any("shorter" in r for r in rep["reasons"])


def test_the_slope_needs_enough_span():
    rows = [_stringify(_clean_row(i, epoch=T0 + i * 300)) for i in range(4)]  # 15 min
    rep = _run(rows, min_duration_s=0)
    assert rep["series"]["av cam1"]["slope_ms_per_h"] is None
    assert rep["verdict"] == asd.UNKNOWN


def test_an_operator_excluded_camera_is_not_required():
    rows = []
    for i in range(7):
        r = _clean_row(i)
        r["av_cam3_ms"] = ""
        r["av_cam3_status"] = "excluded"
        r["loss_cam3_pass"] = ""
        rows.append(_stringify(r))
    rep = _run(rows)
    assert rep["series"]["av cam3"]["verdict"] == asd.EXCLUDED
    assert rep["series"]["loss cam3"]["verdict"] == asd.EXCLUDED
    assert rep["verdict"] == asd.PASS, rep["reasons"]


def test_partial_hour_passes_while_the_full_run_drifts():
    rows = []
    for i in range(13):  # 2 h of windows
        drift = 0.0 if i <= 6 else 8.0 * (i - 6) * SLOT / 3600.0
        rows.append(_stringify(_clean_row(i, av_by_cam={"cam1": drift, "cam2": 0.0, "cam3": 0.0})))
    scopes = asd.evaluate_scopes(rows, CAMS, bounds=_bounds(), partial_h=1.0, min_duration_h=2.0,
                                 max_gap_s=asd.DEFAULT_MAX_GAP_S,
                                 slope_bound=asd.SLOPE_BOUND_MS_PER_H,
                                 spread_columns=("source_spread_ms",))
    assert scopes["partial"]["verdict"] == asd.PASS, scopes["partial"]["reasons"]
    assert scopes["partial"]["windows"] == 7
    assert scopes["full"]["verdict"] == asd.FAIL


def test_render_text_names_every_series_and_the_verdict():
    rows = [_stringify(_clean_row(i)) for i in range(7)]
    text = asd.render_text(asd.evaluate_scopes(rows, CAMS, bounds=_bounds(), partial_h=1.0,
                                               min_duration_h=1.0,
                                               max_gap_s=asd.DEFAULT_MAX_GAP_S,
                                               slope_bound=asd.SLOPE_BOUND_MS_PER_H,
                                               spread_columns=("source_spread_ms",)))
    for needle in ("av cam1", "av cam3", "spread source_spread_ms", "spread av_spread_ms",
                   "loss cam2", "VERDICT: PASS", "AV_OFFSET_GATE_TOLERANCE_MS",
                   "SPREAD_THRESHOLD_MS"):
        assert needle in text, needle


# --- the CLI ------------------------------------------------------------------------------------


def _cli(*args):
    return subprocess.run([sys.executable, MODULE, *args], capture_output=True, text=True)


def test_cli_bounds_prints_the_single_sourced_values():
    p = _cli("bounds")
    assert p.returncode == 0, p.stderr
    b = asd.load_gate_bounds(REPO)
    assert f"av_tolerance_ms={b['av_tolerance_ms']}" in p.stdout
    assert f"spread_threshold_ms={b['spread_threshold_ms']}" in p.stdout
    assert f"slope_bound_ms_per_h={asd.SLOPE_BOUND_MS_PER_H}" in p.stdout


def test_cli_row_then_report_exit_codes(tmp_path):
    csv_path = str(tmp_path / "soak.csv")
    for i in range(7):
        vpath = tmp_path / f"v{i}.json"
        tol = asd.load_gate_bounds(REPO)["av_tolerance_ms"]
        off = 1.0 if i != 6 else tol + 5.0
        vpath.write_text(json.dumps(_verdict(
            av={"cam1": ("measured", off), "cam2": ("measured", 0.0),
                "cam3": ("measured", 0.0)},
            source_spread=5.0, delivery_spread=5.0,
            segments=[{"cambox": c.upper(), "pass": True, "copies": 0, "gaps": 0,
                       "undecodable": 0, "frames": 900} for c in CAMS])))
        p = _cli("row", "--csv", csv_path, "--cams", " ".join(CAMS), "--verdict-json", str(vpath),
                 "--epoch-s", str(T0 + i * SLOT), "--slot", str(i), "--window-s", "90",
                 "--outcome", "ok", "--verdict-rc", "1", "--painter-run-id", "77")
        assert p.returncode == 0, p.stderr
    json_out = str(tmp_path / "report.json")
    p = _cli("report", "--csv", csv_path, "--min-duration-h", "1", "--json", json_out)
    assert p.returncode == 1, p.stdout + p.stderr
    assert "VERDICT: FAIL" in p.stdout
    rep = json.load(open(json_out))
    assert rep["full"]["verdict"] == "FAIL"
    # the 8 h default on a 1 h CSV: the breach still wins over the short duration
    assert _cli("report", "--csv", csv_path).returncode == 1


def test_cli_row_without_a_verdict_and_report_unknown(tmp_path):
    csv_path = str(tmp_path / "soak.csv")
    p = _cli("row", "--csv", csv_path, "--cams", "cam1 cam3", "--no-verdict",
             "--epoch-s", str(T0), "--slot", "0", "--window-s", "90",
             "--outcome", "no_verdict:decode_failed")
    assert p.returncode == 0, p.stderr
    p = _cli("report", "--csv", csv_path, "--min-duration-h", "1")
    assert p.returncode == 2
    assert "VERDICT: UNKNOWN" in p.stdout


def test_cli_report_on_a_missing_csv_is_a_usage_error(tmp_path):
    p = _cli("report", "--csv", str(tmp_path / "absent.csv"))
    assert p.returncode == 3


def test_cli_unknown_spread_column_is_refused(tmp_path):
    csv_path = str(tmp_path / "soak.csv")
    asd.append_row(csv_path, _clean_row(0), CAMS)
    p = _cli("report", "--csv", csv_path, "--spread-columns", "bogus_ms")
    assert p.returncode == 3


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-q"]))
