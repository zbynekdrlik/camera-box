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


# --------------------------------------------------------------- real green-run smoke (skip if absent)
_RUN = "1363366080"
_RUN_DIR = pathlib.Path(f"/tmp/recording-e2e-{_RUN}")


@pytest.mark.skipif(not _RUN_DIR.is_dir(), reason="green-run artefacts not present on this box")
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


@pytest.mark.skipif(not _RUN_DIR.is_dir(), reason="green-run artefacts not present on this box")
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
