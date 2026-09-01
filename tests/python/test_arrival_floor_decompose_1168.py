"""#1168 TASK 1 -- per-box arrival-floor STAGE decomposition mining tool.

The dev1-side supervisor mines a FINISHED E2E run's collected logs and gets one per-camera table
that decomposes each camera's arrival floor into grabber (cambox capture->emit) / NDI transport /
strih present-skew stages, so "which box owns the ~50 ms cross-camera presented-age offset" is
answered from data, not hand-mined. Pure decision core (no I/O, no ssh, no rig), fixture-driven
RED->GREEN under Tier-0 #557 -- the #1199/#1203/#1226 python-mirror precedent.

The model (proven algebraic, not fitted): each camera's floor = latency_ms + mean_head_skew_ms
(exactly what `qr_align_pins.arrival_floors_from_jitter` already derives). So the per-camera EXCESS
over the fastest camera decomposes EXACTLY into `Delta latency_ms` (strih-config pin difference) +
`Delta mean_head_skew_ms` (everything upstream of the pin). The recv-timing #797 cap_avg is the
transport arrival cadence; when it is UNIFORM across cameras the upstream excess is attributable to
the cambox grabber, corroborated by the cambox burn-log Streaming/#707 health.
"""
import json
import pathlib
import subprocess
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import arrival_floor_decompose as afd  # noqa: E402

_TOOL = _SCRIPTS / "arrival_floor_decompose.py"

# A synthetic jitter JSON in the exact `genlock-jitter-report --json` shape the harness saves as
# `qr-align-jitter-<RUN>.json`. cam3 is the anchor (fastest); cam1 is grabber-owned (high skew, same
# latency pin); cam2 is strih-config-owned (its +3 ms is purely its latency_ms=6 pin, grabber skew is
# the LOWEST); cam9 is a #1253 phantom (samples < MIN_FLOOR_SAMPLES) that MUST be omitted.
_JITTER = {
    "NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 86.0, "samples": 3},
    "NDI cam2": {"latency_ms": 6, "mean_head_skew_ms": 67.0, "samples": 3},
    "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 3},
    "NDI cam4": {"latency_ms": 3, "mean_head_skew_ms": 73.0, "samples": 3},
    "NDI cam9": {"latency_ms": 3, "mean_head_skew_ms": 99.0, "samples": 2},
}
_SRCS = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4", "NDI cam9"]
# uniform transport cadence (spread 0.1 ms) -> transport NOT the differentiator.
_CAP_UNIFORM = {"NDI cam1": 15.9, "NDI cam2": 15.95, "NDI cam3": 16.0, "NDI cam4": 15.92}


def _rows_by_src(result):
    return {r["src"]: r for r in result["rows"]}


# --------------------------------------------------------------- stage (a): grabber Streaming parse
def test_parse_streaming_extracts_rates_and_counts():
    text = (
        "2026-09-01T03:09:07Z INFO camera_box: Streaming: 60.0 fps emitted / 60.0 fps captured "
        "(301 sent, 301 captured, 1 capture-dropped, 0 corrupted)\n"
        "2026-09-01T03:09:12Z INFO camera_box: Streaming: 58.0 fps emitted / 60.0 fps captured "
        "(290 sent, 300 captured, 4 capture-dropped, 2 corrupted)\n"
    )
    g = afd.parse_streaming(text)
    assert g["lines"] == 2
    assert g["emit_fps_mean"] == pytest.approx(59.0, abs=0.01)
    assert g["cap_fps_mean"] == pytest.approx(60.0, abs=0.01)
    # emit fell >=0.5 fps behind capture on the 2nd tick -> 1 "emit-behind" tick.
    assert g["emit_behind_lines"] == 1
    assert g["max_capture_dropped"] == 4
    assert g["max_corrupted"] == 2


def test_parse_streaming_empty_is_zeroed():
    g = afd.parse_streaming("")
    assert g["lines"] == 0 and g["emit_fps_mean"] is None


# --------------------------------------------------------------- stage (a): #707 DQBUF stall parse
def test_parse_dqbuf_stalls():
    text = (
        "WARN camera_box: #707 V4L2 capture DEQUEUE STALL: 45.9ms (configured frame interval ...)\n"
        "some other line\n"
        "WARN camera_box: #707 V4L2 capture DEQUEUE STALL: 22.0ms (configured ...)\n"
    )
    d = afd.parse_dqbuf_stalls(text)
    assert d["count"] == 2
    assert d["max_ms"] == pytest.approx(45.9, abs=0.01)
    assert d["mean_ms"] == pytest.approx(33.95, abs=0.01)


def test_parse_dqbuf_stalls_none():
    d = afd.parse_dqbuf_stalls("nothing here")
    assert d["count"] == 0 and d["max_ms"] is None


# --------------------------------------------------------------- stage (b): transport cap_avg reuse
def test_cap_avg_by_source_reuses_recv_timing():
    strih = (
        "05:16:46.082: [distroav] recv-timing #797 'NDI cam1': n=300 cap_avg=15.91ms cap_max=32.91ms "
        "out_avg=0.77ms out_max=2.48ms\n"
        "05:16:51.100: [distroav] recv-timing #797 'NDI cam1': n=301 cap_avg=16.05ms cap_max=20ms "
        "out_avg=0.7ms out_max=2ms\n"
        "05:16:51.200: [distroav] recv-timing #797 'NDI cam2': n=300 cap_avg=15.80ms cap_max=20ms "
        "out_avg=0.7ms out_max=2ms\n"
    )
    caps = afd.cap_avg_by_source(strih, ["NDI cam1", "NDI cam2", "NDI cam3"])
    assert caps["NDI cam1"] == pytest.approx((15.91 + 16.05) / 2, abs=0.01)
    assert caps["NDI cam2"] == pytest.approx(15.80, abs=0.01)
    assert caps["NDI cam3"] is None  # source absent -> honest None, never fabricated


# --------------------------------------------------------------- CORE: decompose()
def test_decompose_total_floor_reuses_arrival_floors_from_jitter():
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    rows = _rows_by_src(res)
    assert rows["NDI cam1"]["floor_ms"] == pytest.approx(89.0)  # 3 + 86
    assert rows["NDI cam2"]["floor_ms"] == pytest.approx(73.0)  # 6 + 67
    assert rows["NDI cam3"]["floor_ms"] == pytest.approx(70.0)  # 3 + 67 (anchor)


def test_decompose_1253_phantom_floor_omitted():
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    assert "NDI cam9" not in _rows_by_src(res)
    assert "NDI cam9" in res["summary"]["omitted_sources"]


def test_decompose_excess_is_exactly_dlatency_plus_dskew():
    # The load-bearing algebraic identity: excess == Delta latency + Delta skew, exactly.
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    for r in res["rows"]:
        assert r["excess_ms"] == pytest.approx(r["d_latency_ms"] + r["d_skew_ms"], abs=1e-6)


def test_decompose_anchor_is_fastest_and_within_noise():
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    assert res["anchor_src"] == "NDI cam3"
    assert _rows_by_src(res)["NDI cam3"]["excess_ms"] == pytest.approx(0.0)
    assert "anchor" in _rows_by_src(res)["NDI cam3"]["owner"].lower()


def test_decompose_cam1_attributed_to_grabber_not_transport_not_strih():
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    owner = _rows_by_src(res)["NDI cam1"]["owner"].lower()
    assert "grabber" in owner or "cambox" in owner
    assert "strih-config" not in owner  # latency pin is the same 3 ms as the anchor
    assert res["summary"]["transport_uniform"] is True


def test_decompose_cam2_excess_attributed_to_strih_config_not_grabber():
    # cam2 floors 3 ms above the anchor PURELY because of its latency_ms=6 pin; its grabber skew is
    # actually the LOWEST, so it must NOT be blamed on the grabber.
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    row = _rows_by_src(res)["NDI cam2"]
    assert row["d_latency_ms"] == pytest.approx(3.0)
    assert row["d_skew_ms"] <= 0.0
    owner = row["owner"].lower()
    assert "strih-config" in owner
    assert "grabber" not in owner and "cambox" not in owner


def test_decompose_flags_nonuniform_transport():
    cap_outlier = dict(_CAP_UNIFORM)
    cap_outlier["NDI cam1"] = 31.0  # a halved/degraded transport cadence outlier
    res = afd.decompose(_JITTER, cap_outlier, {}, _SRCS)
    assert res["summary"]["transport_uniform"] is False
    assert "transport" in _rows_by_src(res)["NDI cam1"]["owner"].lower()


def test_decompose_grabber_corroboration_surfaced():
    grab = {"NDI cam1": {"lines": 196, "emit_fps_mean": 60.0, "cap_fps_mean": 60.0,
                         "emit_behind_lines": 0, "max_capture_dropped": 1, "max_corrupted": 0,
                         "dqbuf_stall_count": 2, "dqbuf_stall_max_ms": 45.9}}
    res = afd.decompose(_JITTER, _CAP_UNIFORM, grab, _SRCS)
    assert _rows_by_src(res)["NDI cam1"]["grabber"]["dqbuf_stall_max_ms"] == pytest.approx(45.9)


def test_decompose_summary_floor_spread_and_slowest():
    res = afd.decompose(_JITTER, _CAP_UNIFORM, {}, _SRCS)
    s = res["summary"]
    assert s["floor_spread_ms"] == pytest.approx(19.0)  # 89 - 70
    assert s["slowest_src"] == "NDI cam1"
    assert s["anchor_src"] == "NDI cam3"


def test_decompose_transport_unknown_owner_when_no_cap_data():
    # No recv-timing data at all -> transport_uniform is None (unknown), and the upstream excess is
    # labelled honestly as grabber/transport with the cadence unknown, never falsely "grabber-only".
    res = afd.decompose(_JITTER, {}, {}, _SRCS)
    assert res["summary"]["transport_uniform"] is None
    assert res["summary"]["transport_cap_avg_spread_ms"] is None
    assert "unknown" in _rows_by_src(res)["NDI cam1"]["owner"].lower()


def test_decompose_single_cap_sample_spread_is_none_not_zero():
    res = afd.decompose(_JITTER, {"NDI cam3": 16.0}, {}, _SRCS)  # only one camera has cap data
    assert res["summary"]["transport_cap_avg_spread_ms"] is None  # never a fabricated 0.0
    assert res["summary"]["transport_uniform"] is None


def test_decompose_all_omitted_returns_empty_table():
    phantom = {f"NDI cam{n}": {"latency_ms": 3, "mean_head_skew_ms": 70.0, "samples": 1}
               for n in (1, 2, 3)}
    srcs = ["NDI cam1", "NDI cam2", "NDI cam3"]
    res = afd.decompose(phantom, {}, {}, srcs)
    assert res["rows"] == []
    assert res["anchor_src"] is None
    assert res["summary"]["floor_spread_ms"] is None
    assert set(res["summary"]["omitted_sources"]) == set(srcs)


def test_attribute_supra_noise_but_subthreshold_is_mixed_not_within_noise():
    # excess 2.5 ms (> EXCESS_NOISE_MS=2.0) but d_lat 1.0 (not > STRIH_CONFIG_MIN_MS) and d_skew 1.5
    # (not > EXCESS_NOISE_MS): must NOT be mislabelled "within-noise".
    jj = {
        "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 3},   # anchor 70.0
        "NDI cam8": {"latency_ms": 4, "mean_head_skew_ms": 68.5, "samples": 3},   # 72.5, excess 2.5
    }
    res = afd.decompose(jj, {"NDI cam3": 16.0, "NDI cam8": 16.0}, {}, ["NDI cam3", "NDI cam8"])
    owner = _rows_by_src(res)["NDI cam8"]["owner"].lower()
    assert "within-noise" not in owner
    assert "mixed sub-threshold" in owner


# ------------------------------------------------- real green-run smoke (committed fixture, always runs)
_RUN = "1363366080"
_RUN_DIR = (
    pathlib.Path(__file__).resolve().parent.parent
    / "fixtures" / "arrival_floor_1168" / f"recording-e2e-{_RUN}"
)


def test_real_green_run_reproduces_cam1_grabber_and_cam2_strih_config():
    jj = json.loads((_RUN_DIR / f"qr-align-jitter-{_RUN}.json").read_text())
    strih = (_RUN_DIR / f"qr-align-strih-{_RUN}.log").read_text(errors="replace")
    srcs = [f"NDI cam{n}" for n in range(1, 8)]
    caps = afd.cap_avg_by_source(strih, srcs)
    res = afd.decompose(jj, caps, {}, srcs)
    rows = _rows_by_src(res)
    # cam1 is the slowest per-box floor (~89), grabber-owned; cam2 (latency pin 6) is strih-config.
    assert res["summary"]["slowest_src"] == "NDI cam1"
    assert "grabber" in rows["NDI cam1"]["owner"].lower() or "cambox" in rows["NDI cam1"]["owner"].lower()
    assert res["summary"]["transport_uniform"] is True
    assert rows["NDI cam2"]["latency_ms"] == 6
    assert "strih-config" in rows["NDI cam2"]["owner"].lower()


def test_cli_runs_over_run_dir_and_prints_table_and_summary():
    out = subprocess.run(
        [sys.executable, str(_TOOL), "--run-dir", str(_RUN_DIR)],
        capture_output=True, text=True,
    )
    assert out.returncode == 0, out.stderr
    assert "NDI cam1" in out.stdout
    assert "floor" in out.stdout.lower()
    assert "owner" in out.stdout.lower()
    # --json mode returns machine-parseable rows+summary.
    outj = subprocess.run(
        [sys.executable, str(_TOOL), "--run-dir", str(_RUN_DIR), "--json"],
        capture_output=True, text=True,
    )
    assert outj.returncode == 0, outj.stderr
    doc = json.loads(outj.stdout)
    assert doc["summary"]["slowest_src"] == "NDI cam1"


# ================================================================= #1168 TASK 2 -- multi-run mining
# The single-run table disagrees on the slowest box across runs (1363366080 -> cam1@89 slowest;
# 1556876186 -> cam5@92.2 slowest) -- transient grabber-stall / load shuffles the noisy middle. So
# the target box must be picked from MANY runs, not one. `aggregate()` folds a list of per-run
# decompose() results (REUSED, never re-parsed) into per-camera floor/excess distributions, an
# anchor/slowest mode + fraction, and a STABILITY verdict. `_keep_run` stratifies (transport-uniform,
# min-cameras) so the ~50 ms clean regime can be scoped without a code change. Pure core -> full
# local RED->GREEN, Tier-0 #557.

_SRCS4 = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]


def _mk(jit, srcs=None, caps=None):
    return afd.decompose(jit, caps or {}, {}, srcs or _SRCS4)


def _three_runs_stable_cam4_anchor_cam2_slowest():
    # anchor: cam3 once, cam4 twice (2/3 = 0.67 -> stable). slowest: cam1 once, cam2 twice -> stable.
    a = _mk({"NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 86.0, "samples": 3},  # 89 slowest
             "NDI cam2": {"latency_ms": 6, "mean_head_skew_ms": 67.0, "samples": 3},  # 73
             "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 3},  # 70 anchor
             "NDI cam4": {"latency_ms": 3, "mean_head_skew_ms": 72.0, "samples": 3}})  # 75
    b = _mk({"NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 69.0, "samples": 3},  # 72
             "NDI cam2": {"latency_ms": 6, "mean_head_skew_ms": 90.0, "samples": 3},  # 96 slowest
             "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 76.0, "samples": 3},  # 79
             "NDI cam4": {"latency_ms": 3, "mean_head_skew_ms": 68.0, "samples": 3}})  # 71 anchor
    c = _mk({"NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 71.0, "samples": 3},  # 74
             "NDI cam2": {"latency_ms": 6, "mean_head_skew_ms": 74.0, "samples": 3},  # 80 slowest
             "NDI cam3": {"latency_ms": 3, "mean_head_skew_ms": 74.0, "samples": 3},  # 77
             "NDI cam4": {"latency_ms": 3, "mean_head_skew_ms": 68.0, "samples": 3}})  # 71 anchor
    return [("A", a), ("B", b), ("C", c)]


# --------------------------------------------------------------- mine_run_dir reuses decompose()
def test_mine_run_dir_reuses_decompose_over_fixture():
    res = afd.mine_run_dir(str(_RUN_DIR))
    assert res is not None
    assert res["summary"]["slowest_src"] == "NDI cam1"  # same as the single-run fixture test
    assert {r["src"] for r in res["rows"]} >= {"NDI cam1", "NDI cam4"}


def test_mine_run_dir_none_when_no_jitter(tmp_path):
    assert afd.mine_run_dir(str(tmp_path)) is None  # empty dir -> honest None, no crash


# --------------------------------------------------------------- _keep_run stratification predicate
def test_keep_run_filters_by_uniform_and_min_cameras():
    uni = _mk(_JITTER, _SRCS, _CAP_UNIFORM)          # transport_uniform True, 4 non-phantom rows
    cap_outlier = dict(_CAP_UNIFORM)
    cap_outlier["NDI cam1"] = 31.0
    nonuni = _mk(_JITTER, _SRCS, cap_outlier)         # transport_uniform False
    assert afd._keep_run(uni, only_uniform=True, min_cameras=4) is True
    assert afd._keep_run(nonuni, only_uniform=True, min_cameras=4) is False
    assert afd._keep_run(uni, only_uniform=False, min_cameras=5) is False   # only 4 rows present
    assert afd._keep_run(uni, only_uniform=False, min_cameras=0) is True
    assert afd._keep_run({"rows": [], "summary": {"transport_uniform": None}},
                         only_uniform=False, min_cameras=1) is False  # empty run never kept


# --------------------------------------------------------------- aggregate(): stability verdict
def test_aggregate_stable_anchor_and_slowest():
    agg = afd.aggregate(_three_runs_stable_cam4_anchor_cam2_slowest())
    assert agg["n_runs"] == 3 and agg["n_usable"] == 3 and agg["n_empty"] == 0
    assert agg["anchor_counts"] == {"NDI cam3": 1, "NDI cam4": 2}
    assert agg["slowest_counts"] == {"NDI cam1": 1, "NDI cam2": 2}
    st_anchor = agg["stability"]["anchor"]
    assert st_anchor["src"] == "NDI cam4" and st_anchor["count"] == 2
    assert st_anchor["fraction"] == pytest.approx(2 / 3)
    assert st_anchor["stable"] is True
    st_slow = agg["stability"]["slowest"]
    assert st_slow["src"] == "NDI cam2" and st_slow["stable"] is True


def test_aggregate_per_camera_floor_and_excess_stats():
    agg = afd.aggregate(_three_runs_stable_cam4_anchor_cam2_slowest())
    pc = agg["per_camera"]
    assert pc["NDI cam2"]["floor_median"] == pytest.approx(80.0)   # [73,96,80]
    assert pc["NDI cam2"]["floor_max"] == pytest.approx(96.0)
    assert pc["NDI cam2"]["latency_pins"] == [6]                   # the stable strih-config pin
    assert pc["NDI cam4"]["latency_pins"] == [3]
    # cam4 is fastest (rank 1) in B,C and rank 3 in A -> mean 5/3.
    assert pc["NDI cam4"]["mean_floor_rank"] == pytest.approx(5 / 3)
    assert pc["NDI cam4"]["n"] == 3
    # ranking_by_median_floor is fastest -> slowest; cam4 first, cam2 last.
    assert agg["ranking_by_median_floor"][0] == "NDI cam4"
    assert agg["ranking_by_median_floor"][-1] == "NDI cam2"


def test_aggregate_no_stable_slowest_when_shuffled():
    # 5 runs whose slowest cycles cam1,cam2,cam3,cam4,cam1 -> mode cam1 = 2/5 = 0.4 < 0.6 -> unstable.
    runs = []
    for i, slow in enumerate(["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4", "NDI cam1"]):
        jit = {s: {"latency_ms": 3, "mean_head_skew_ms": 67.0, "samples": 3} for s in _SRCS4}
        jit[slow] = {"latency_ms": 3, "mean_head_skew_ms": 97.0, "samples": 3}  # this one is slowest
        runs.append((str(i), _mk(jit)))
    agg = afd.aggregate(runs)
    st_slow = agg["stability"]["slowest"]
    assert st_slow["src"] == "NDI cam1"           # still reports the mode
    assert st_slow["fraction"] == pytest.approx(0.4)
    assert st_slow["stable"] is False              # ...but flags it UNSTABLE


def test_aggregate_counts_empty_runs_and_ignores_them():
    good = _three_runs_stable_cam4_anchor_cam2_slowest()
    phantom = _mk({f"NDI cam{n}": {"latency_ms": 3, "mean_head_skew_ms": 70.0, "samples": 1}
                   for n in (1, 2, 3, 4)})  # all #1253 phantom -> rows == []
    agg = afd.aggregate(good + [("EMPTY", phantom)])
    assert agg["n_runs"] == 4 and agg["n_usable"] == 3 and agg["n_empty"] == 1
    assert "EMPTY" not in {r for r in agg["anchor_counts"]}  # empty run contributes nothing


def test_aggregate_empty_list_is_safe():
    agg = afd.aggregate([])
    assert agg["n_runs"] == 0 and agg["n_usable"] == 0
    assert agg["ranking_by_median_floor"] == []
    assert agg["stability"]["anchor"]["src"] is None
    assert agg["stability"]["anchor"]["stable"] is False


# --------------------------------------------------------------- CLI --multi over two REAL fixtures
_RUN2 = "659887078"
_RUN2_DIR = (
    pathlib.Path(__file__).resolve().parent.parent
    / "fixtures" / "arrival_floor_1168" / f"recording-e2e-{_RUN2}"
)


def test_cli_multi_over_two_fixtures_json():
    out = subprocess.run(
        [sys.executable, str(_TOOL), "--multi",
         "--run-dir", str(_RUN_DIR), "--run-dir", str(_RUN2_DIR), "--json"],
        capture_output=True, text=True,
    )
    assert out.returncode == 0, out.stderr
    doc = json.loads(out.stdout)
    assert doc["n_usable"] == 2
    # The two fixtures GENUINELY disagree: 1363366080 anchor cam3/slowest cam1, 659887078 anchor
    # cam4/slowest cam2. So neither anchor nor slowest reaches a >=60% mode -> both flagged unstable.
    assert doc["stability"]["anchor"]["stable"] is False
    assert doc["stability"]["slowest"]["stable"] is False
    assert doc["per_camera"]["NDI cam2"]["latency_pins"] == [6]  # cam2 pin stable across both
    assert doc["per_camera"]["NDI cam4"]["n"] == 2


def test_cli_multi_text_mode_prints_verdict():
    out = subprocess.run(
        [sys.executable, str(_TOOL), "--multi",
         "--run-dir", str(_RUN_DIR), "--run-dir", str(_RUN2_DIR)],
        capture_output=True, text=True,
    )
    assert out.returncode == 0, out.stderr
    low = out.stdout.lower()
    assert "anchor" in low and "slowest" in low
    assert "stable" in low  # the stability verdict is rendered
