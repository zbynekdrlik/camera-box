"""#1325 — the mbc `buffered_ms` DRIFT/STEP detector (REPORT-ONLY) in the audio-lag watchdog.

The #1226/#1231 lag arm and the #1265 band arm both read ts_lag; `buffered_ms` is the honest signal
for the audio-timeline drift the ASRC servo fails to hold out of the mix buffer (the ≈ −18 ppm
Dante-GM-vs-UTC floor). On the stream box `buffered_ms` drains ~1.1 ms/min then JUMPS +20…+57 ms
when OBS re-buffers — the sawtooth every dock/E2E A/V reading inherits.

Fixture is tonight's series from the issue body: drain 76→28 ms over ~40 min, then jumps
19:24 29→50, 19:32 39→86, 20:11 26→47, 20:18 38→87.
"""
import importlib.util
import json
import pathlib

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, _SCRIPTS / f"{name}.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


bsg = _load("bundle_state_gather")
ald = _load("audio_lag_decision")


def _line(h, m, buffered, src="mbc", ts_lag=86):
    return (f"{h:02d}:{m:02d}:24.585: audio-telemetry #800 '{src}': "
            f"ts_lag_ms={ts_lag} buffered_ms={buffered} pending=0 timing_adjust_ms=0")


# ---- gather: buffered_ms_series_from_log ------------------------------------------------------

def test_gather_absent_when_too_few_readings():
    log = _line(20, 0, 72)
    assert bsg.buffered_ms_series_from_log(log, ref_src="mbc") == ("", "", "", "")


def test_gather_drain_slope_negative():
    # A steady drain 76 -> 58 over 11 min -> ~ -1.6 ms/min, no big step.
    buf = [76, 74, 73, 71, 70, 68, 66, 65, 63, 61, 58]
    log = "\n".join(_line(20, i, b) for i, b in enumerate(buf))
    slope, step, n, age = bsg.buffered_ms_series_from_log(log, ref_src="mbc")
    assert float(slope) < 0
    assert int(n) == len(buf)
    assert age == "0"
    assert int(step) <= 1   # no refill jump in this window


def test_gather_captures_refill_step():
    # A drain then a +47 refill jump (39 -> 86) => max_step captures it.
    buf = [60, 58, 55, 52, 50, 47, 44, 41, 39, 86]
    log = "\n".join(_line(20, i, b) for i, b in enumerate(buf))
    _slope, step, _n, _age = bsg.buffered_ms_series_from_log(log, ref_src="mbc")
    assert int(step) == 47


def test_gather_watches_only_the_named_source():
    # Another source's buffered readings must not pollute the mbc series.
    lines = []
    for i, b in enumerate([70, 68, 66, 64, 62, 60]):
        lines.append(_line(20, i, b, src="mbc"))
        lines.append(_line(20, i, 999, src="ASIO Input Capture"))
    slope, _step, n, _age = bsg.buffered_ms_series_from_log("\n".join(lines), ref_src="mbc")
    assert int(n) == 6            # only the 6 mbc readings
    assert float(slope) < 0       # the mbc drain, not the flat 999 sibling


def test_gather_tail_only_ignores_head_region():
    sep = bsg.LOG_BOUNDED_READ_SEPARATOR
    head = "\n".join(_line(19, i, 500) for i in range(3))   # a stale head region
    tail = "\n".join(_line(20, i, b) for i, b in enumerate([70, 68, 66, 64, 62, 60]))
    slope, _step, n, _age = bsg.buffered_ms_series_from_log(head + sep + tail, ref_src="mbc")
    assert int(n) == 6            # head readings excluded
    assert float(slope) < 0


# ---- decision: classify_buffered --------------------------------------------------------------

def test_classify_skip_when_unreachable():
    assert ald.classify_buffered(None, None, None, None, 0) == "SKIP"


def test_classify_unknown_when_facet_absent():
    assert ald.classify_buffered(None, None, None, None, 1) == "UNKNOWN"


def test_classify_unknown_when_too_few_samples():
    assert ald.classify_buffered(-1.1, 3, 4, 0, 1) == "UNKNOWN"   # n=4 < min 6


def test_classify_stale_when_telemetry_stopped():
    assert ald.classify_buffered(-1.1, 3, 10, 999, 1) == "STALE"


def test_classify_drift_on_sustained_drain():
    assert ald.classify_buffered(-1.2, 2, 10, 0, 1) == "DRIFT"


def test_classify_step_beats_drift():
    # A re-buffering source has BOTH a step and a net drain; STEP is the more specific verdict.
    assert ald.classify_buffered(-1.2, 47, 10, 0, 1) == "STEP"


def test_classify_healthy_when_flat():
    assert ald.classify_buffered(-0.1, 1, 10, 0, 1) == "HEALTHY"


# ---- decision: analyze_buffered end-to-end from the tonight fixture --------------------------

def test_analyze_buffered_step_from_json():
    body = json.dumps({
        "buffered_ms_slope_ms_per_min": "-1.1", "buffered_ms_max_step_ms": "47",
        "buffered_ms_n": "12", "buffered_ms_age_s": "0",
    })
    res = ald.analyze_buffered(body, 1)
    assert res["verdict"] == "STEP"
    assert res["max_step_ms"] == 47


def test_analyze_buffered_skip_without_parsing():
    res = ald.analyze_buffered("", 0)
    assert res["verdict"] == "SKIP"
    assert res["slope_ms_per_min"] is None


# ---- wiring: build_bundle_state carries the new facets (omit-when-empty) ---------------------

def test_build_bundle_state_omits_empty_new_facets():
    st = bsg.build_bundle_state()
    for k in ("av_offset_quality_age_s", "buffered_ms_slope_ms_per_min",
              "buffered_ms_max_step_ms", "buffered_ms_n", "buffered_ms_age_s"):
        assert k not in st


def test_build_bundle_state_carries_new_facets_when_present():
    st = bsg.build_bundle_state(
        av_offset_quality_age_s="999",
        buffered_ms_slope_ms_per_min="-1.1", buffered_ms_max_step_ms="47",
        buffered_ms_n="12", buffered_ms_age_s="0")
    assert st["av_offset_quality_age_s"] == "999"
    assert st["buffered_ms_slope_ms_per_min"] == "-1.1"
    assert st["buffered_ms_max_step_ms"] == "47"
    assert st["buffered_ms_n"] == "12"
    assert st["buffered_ms_age_s"] == "0"


def test_classify_step_reachable_in_zero_span_window():
    # #1325 review nit: a degenerate zero-span window (>=2 readings, one timestamp -> slope "") must
    # still evaluate STEP off max_step (UNKNOWN gates on n, not slope).
    assert ald.classify_buffered(None, 47, 10, 0, 1) == "STEP"
    # ...and with no step, a None slope falls through to HEALTHY (DRIFT is slope-guarded), never a crash.
    assert ald.classify_buffered(None, 2, 10, 0, 1) == "HEALTHY"
