"""issue 1242 (task 1) -- residual copy/gap churn SOURCE-attribution mining tool.

The dev1-side supervisor mines a set of FINISHED E2E run dirs and, for every residual copy/gap
event in the fused verdict, aligns its `wall_clock_epoch_s` to that cambox's OWN burn log to
answer "grabber-cadence-owned vs DOWNSTREAM (genlock-FIFO / 60->30 decimation-phase / optical-beat)"
from data. Pure decision core (no I/O below the CLI, no ssh, no rig), fixture-driven RED->GREEN
under Tier-0 #557 -- the window_gate_walkdown.py / arrival_floor_decompose.py python-mirror
precedent.

The discriminator these tests pin: a capture DEFICIT and a persistent `corrupted` floor are steady
per-box BACKGROUND (present run-wide, including the fully-clean 0/0 runs), so their mere presence at
an event is a COVARIATE, never a source attribution. An event is SOURCE only when the source shows
something the run's OWN background does not: a `late-dupe copies emitted`, a corruption RISE above
the box's steady floor, or a burst deficit >= ANOMALY_DEFICIT_FLOOR. Everything else is DOWNSTREAM.
(A naive classifier that read any deficit or any `corrupted>=1` as SOURCE -- the tool's own first
draft -- would MIS-attribute the steady cam2 deficit and the steady cam7 `4 corrupted` floor to the
grabber; `steady_*_is_background_not_source_*` are the RED-catching cases.)
"""
import json
import pathlib
import subprocess
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import residual_churn_attribution as rca  # noqa: E402

_TOOL = _SCRIPTS / "residual_churn_attribution.py"

# --- real-shape burn-log lines (ANSI-wrapped, exactly as camera-box logs them) -------------------
_ESC = "\x1b"


def _c(ts, msg):
    """Compose one real-shape burn-log line: ANSI-dimmed ISO timestamp + level + module + msg."""
    return (
        _ESC + "[2m" + ts + "Z" + _ESC + "[0m " + _ESC + "[32m INFO" + _ESC + "[0m "
        + _ESC + "[2mcamera_box" + _ESC + "[0m" + _ESC + "[2m:" + _ESC + "[0m " + msg
    )


def _streaming(ts, sent, cap, dropped=2, corrupt=0):
    return _c(ts, "Streaming: 60.0 fps emitted / 60.0 fps captured (%d sent, %d captured, %d capture-dropped, %d corrupted)"
              % (sent, cap, dropped, corrupt))


def _b707(ts, emit, cap):
    return _c(ts, "#707 emit-1s: [%s] cap-1s: [%s] (1-second buckets, oldest first)"
              % (", ".join(map(str, emit)), ", ".join(map(str, cap))))


def _decim(ts, late_dupe=0, starvation=0):
    return _c(ts, "(#889) dupe-preferring decimation: 0 dupe-victim shed / 0 blind-pacing shed / "
              "%d late-dupe copies emitted (#1111 grid-lock valve) / 0 boundaries retired "
              "(#1145 over-rate absorption) / 0 depth-drained (#1145 v2 over-rate absorption) / "
              "0 fast-drained (#1145 v2.1 deep-backlog convergence) / %d starvation last-frame repeats "
              "(#1167 v4 empty-queue slot-fill) over the last ~5s" % (late_dupe, starvation))


# --------------------------------------------------------------------- parse_burn_timeline --------
def test_parse_burn_timeline_extracts_all_three_line_kinds_real_shape():
    text = "\n".join([
        _streaming("2026-09-16T19:05:48.305307", 300, 299, dropped=2, corrupt=0),
        _b707("2026-09-16T19:05:48.305340", [59, 60, 60, 60], [60, 60, 60, 60]),
        _decim("2026-09-16T19:05:48.305334", late_dupe=0, starvation=1),
        "some unrelated line with no timestamp",
    ])
    tl = rca.parse_burn_timeline(text)
    assert len(tl["stream"]) == 1
    t, sent, cap, dropped, corrupt = tl["stream"][0]
    assert (sent, cap, dropped, corrupt) == (300, 299, 2, 0)
    # the 4 oldest-first 1-s buckets ending at the line timestamp map to t-3..t
    assert len(tl["sec"]) == 4
    assert tl["sec"][t] == (60, 60)  # newest bucket at the line ts
    assert tl["sec"][t - 3] == (59, 60)  # oldest bucket, emit=59 -> deficit -1 (over-rate)
    assert tl["decim"] == [(t, 0, 1)]


def test_parse_burn_timeline_empty_is_empty_not_fabricated():
    tl = rca.parse_burn_timeline("")
    assert tl == {"stream": [], "sec": {}, "decim": []}


# ------------------------------------------------------------------------------ burn_baseline -----
def test_burn_baseline_counts_emitfill_and_corrupt_floor():
    text = "\n".join([
        _streaming("2026-09-16T19:00:00", 300, 300, corrupt=4),  # deficit 0, corrupt floor 4
        _streaming("2026-09-16T19:00:05", 301, 299, corrupt=4),  # deficit 2
        _streaming("2026-09-16T19:00:10", 300, 299, corrupt=4),  # deficit 1
        _decim("2026-09-16T19:00:05", starvation=3),
    ])
    b = rca.burn_baseline(rca.parse_burn_timeline(text))
    assert b["stream_lines"] == 3
    assert b["deficit_lines"] == 2
    assert b["total_emitfill"] == 3       # 0 + 2 + 1
    assert b["max_line_deficit"] == 2
    assert b["corrupt_floor"] == 4        # persistent floor, not 0
    assert b["total_starvation"] == 3
    assert b["mean_cap_fps"] == pytest.approx((300 + 299 + 299) / 3, abs=0.01)


# ------------------------------------------------------- source_signal_at + classify_event --------
def _timeline_steady_deficit(t0, corrupt=0):
    """A box carrying only the steady 1-2/5s emit-fill background (the cam1/cam2 signature)."""
    lines = []
    for i in range(-4, 5):
        ts = "2026-09-16T19:%02d:%02d" % (10 + (i + 12) // 60, (i + 12) % 60)
        lines.append(_streaming(ts, 301, 300, corrupt=corrupt))   # steady deficit 1
        lines.append(_decim(ts, late_dupe=0, starvation=1))
    return rca.parse_burn_timeline("\n".join(lines))


def test_steady_background_deficit_is_downstream_not_source():
    tl = _timeline_steady_deficit(0)
    # pick an event epoch inside the covered window
    t_event = tl["stream"][4][0]
    sig = rca.source_signal_at(tl, t_event, window=10)
    base = rca.burn_baseline(tl)
    attribution, reason = rca.classify_event(sig, base)
    assert sig["has_any_deficit"] is True        # the background deficit IS present
    assert attribution == "DOWNSTREAM"           # ...but it is a covariate, not a source attribution
    assert "covariate" in reason


def test_steady_corrupt_floor_is_background_not_source():
    """The cam7 case: a persistent `4 corrupted` floor must NOT classify SOURCE (the RED-catching case)."""
    tl = _timeline_steady_deficit(0, corrupt=4)
    t_event = tl["stream"][4][0]
    sig = rca.source_signal_at(tl, t_event, window=10)
    base = rca.burn_baseline(tl)
    assert sig["stream_corrupt_max"] == 4
    assert base["corrupt_floor"] == 4
    attribution, _ = rca.classify_event(sig, base)
    assert attribution == "DOWNSTREAM"


def test_late_dupe_copy_at_event_is_source():
    tl = _timeline_steady_deficit(0)
    t_event = tl["stream"][4][0]
    # inject a late-dupe emitted at the event second
    tl["decim"].append((t_event, 1, 0))
    sig = rca.source_signal_at(tl, t_event, window=10)
    base = rca.burn_baseline(tl)
    attribution, reason = rca.classify_event(sig, base)
    assert attribution == "SOURCE"
    assert "late-dupe" in reason


def test_corruption_rise_above_floor_is_source():
    tl = _timeline_steady_deficit(0, corrupt=1)     # floor 1
    t_event = tl["stream"][4][0]
    # a spike of corruption at the event window (a genuine capture fault above the floor)
    tl["stream"][4] = (t_event, 301, 300, 2, 5)
    sig = rca.source_signal_at(tl, t_event, window=10)
    base = rca.burn_baseline(tl)
    assert base["corrupt_floor"] == 1
    attribution, reason = rca.classify_event(sig, base)
    assert attribution == "SOURCE"
    assert "ROSE" in reason


def test_burst_deficit_above_floor_is_source():
    tl = _timeline_steady_deficit(0)
    t_event = tl["stream"][4][0]
    tl["stream"][4] = (t_event, 303, 300, 2, 0)     # deficit 3 == ANOMALY_DEFICIT_FLOOR
    sig = rca.source_signal_at(tl, t_event, window=10)
    base = rca.burn_baseline(tl)
    attribution, _ = rca.classify_event(sig, base)
    assert attribution == "SOURCE"


def test_no_coverage_is_unknown_never_guessed():
    tl = {"stream": [], "sec": {}, "decim": []}
    sig = rca.source_signal_at(tl, 1789585992, window=10)
    attribution, _ = rca.classify_event(sig, rca.burn_baseline(tl))
    assert attribution == "UNKNOWN"


# --------------------------------------------------------------- residual_events_from_verdict -----
def test_residual_events_dedup_top_level_and_per_segment():
    ev = {"cambox": "CAM2", "kind": "copy", "wall_clock_epoch_s": 100, "frame_index": 5}
    verdict = {"all_cambox_continuity": {
        "residual_events": [ev],
        "segments": [{"cambox": "CAM2", "residual_events": [dict(ev)]}],
    }}
    out = rca.residual_events_from_verdict(verdict)
    assert len(out) == 1
    assert out[0]["cambox"] == "CAM2" and out[0]["kind"] == "copy"


# ------------------------------------------------------------------ attribute_run + aggregate -----
def _verdict(events):
    segs = {}
    for e in events:
        segs.setdefault(e["cambox"], []).append(e)
    return {"overall_pass": False, "all_cambox_continuity": {
        "windows_failed_report_only": len(events),
        "segments": [{"cambox": cam, "copies": 0, "gaps": 0,
                      "presentation_cadence": {"beat_corrected_uniform_fraction": 0.99},
                      "residual_events": evs} for cam, evs in segs.items()],
    }}


def test_attribute_run_all_downstream_when_source_clean():
    t = rca.parse_burn_timeline("\n".join([
        _streaming("2026-09-16T19:00:%02d" % s, 301, 300) for s in range(0, 30, 5)
    ]))
    t_event = t["stream"][3][0]
    verdict = _verdict([{"cambox": "CAM2", "kind": "copy", "wall_clock_epoch_s": t_event, "frame_index": 1}])
    res = rca.attribute_run(verdict, {"CAM2": t}, window=10)
    assert res["n_events"] == 1
    assert res["events"][0]["attribution"] == "DOWNSTREAM"
    assert res["per_box"]["CAM2"]["residuals"] == {"copy": 1, "gap": 0}


def test_aggregate_verdict_downstream_and_clean_run_counterfactual():
    # one clean run (0 residuals, but real emit-fill material) + one red run (1 downstream copy)
    tl = rca.parse_burn_timeline("\n".join([
        _streaming("2026-09-16T19:00:%02d" % s, 301, 300) for s in range(0, 40, 5)
    ]))
    clean = rca.attribute_run(_verdict([]), {"CAM2": tl}, window=10)
    t_event = tl["stream"][3][0]
    red = rca.attribute_run(
        _verdict([{"cambox": "CAM2", "kind": "copy", "wall_clock_epoch_s": t_event, "frame_index": 1}]),
        {"CAM2": tl}, window=10)
    agg = rca.aggregate([clean, red])
    assert agg["source_events"] == 0
    assert agg["downstream_events"] == 1
    assert agg["clean_runs"] == 1
    assert agg["clean_run_emitfill_with_zero_residuals"] > 0   # emit-fill present, zero residuals
    assert "DOWNSTREAM" in agg["verdict"]


def test_aggregate_reports_source_verdict_when_source_dominates():
    tl = rca.parse_burn_timeline("\n".join([
        _streaming("2026-09-16T19:00:%02d" % s, 301, 300) for s in range(0, 30, 5)
    ]))
    t_event = tl["stream"][2][0]
    tl["decim"].append((t_event, 1, 0))   # a real on-box late-dupe
    verdict = _verdict([{"cambox": "CAM2", "kind": "copy", "wall_clock_epoch_s": t_event, "frame_index": 1}])
    res = rca.attribute_run(verdict, {"CAM2": tl}, window=10)
    agg = rca.aggregate([res])
    assert agg["source_events"] == 1
    assert "SOURCE" in agg["verdict"]


# ------------------------------------------------------------------------------- CLI end-to-end ---
def test_cli_end_to_end_writes_markdown_and_json(tmp_path):
    run = tmp_path / "recording-e2e-99999"
    run.mkdir()
    tl_lines = [_streaming("2026-09-16T19:00:%02d" % s, 301, 300) for s in range(0, 40, 5)]
    (run / "cam2-cbox-burn-99999.log").write_text("\n".join(tl_lines))
    # a residual event whose epoch lands inside the burn coverage
    ts0 = rca._epoch("2026-09-16T19:00:15")
    verdict = _verdict([{"cambox": "CAM2", "kind": "copy", "wall_clock_epoch_s": ts0, "frame_index": 1}])
    (run / "verdict-99999.json").write_text(json.dumps(verdict))
    out_json = tmp_path / "out.json"
    out_md = tmp_path / "out.md"
    r = subprocess.run(
        [sys.executable, str(_TOOL), str(run), "--json", str(out_json), "--markdown", str(out_md)],
        capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert "DOWNSTREAM" in r.stdout
    data = json.loads(out_json.read_text())
    assert data["aggregate"]["downstream_events"] == 1
    assert "issue 1242" in out_md.read_text()


def test_cli_accepts_bare_run_id_or_skips_missing(tmp_path):
    r = subprocess.run([sys.executable, str(_TOOL), "does-not-exist-run"], capture_output=True, text=True)
    assert r.returncode == 1
    assert "no run dirs" in r.stderr
