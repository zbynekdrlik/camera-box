#!/usr/bin/env python3
"""#1152 M3 — Tier-0 tests for the PURE pairing / statistics / delta core of the DRM-latency
measurement tooling (`scripts/drm_latency_report.py`), plus static-anchor sanity for the dev1-side
orchestrator (`scripts/drm-latency-measure.sh`).

The DRM-latency measurement (design of record: issue-1152 comment 5428521213, Approach 1) captures
raw frames off the cam2 grabber (which taps imag's HDMI) while imag's Program carries the QR burn
whose `gen_ts_ns` field is the emit wall clock, then offline-decodes each frame and pairs the
capture wall-ts against the decoded emit-ts. This file owns the PURE core: given already-decoded
per-frame records, compute the per-frame latency distribution (median/p95/p99/jitter), the
undecodable fraction, and the DORMANT−ENABLED delta table. No I/O, no ffmpeg, no cv2, no ssh — the
`ndi_halving_decision` / `strih_nic_selfheal` python-mirror precedent, chosen so the decision
RED→GREENs LOCALLY under Tier-0 #557 (cargo, even --no-run, cannot run).

The impure capture-decode glue (ffmpeg extract + `cv2.QRCodeDetector` + the shared CRC-validating
`mv_skew_snapshot.parse_payload`) and the bash orchestrator's rig I/O are NOT unit-tested here (they
need the rig / ffmpeg / a real capture — the supervisor rig campaign); this file covers the pure
statistics and the orchestrator's static shape (bash -n, shellcheck, source-guarded pure builders).

Runnable directly (`python3 tests/python/test_drm_latency_report_1152.py`) or under pytest.
"""
import math
import os
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import drm_latency_report as r  # noqa: E402

SH = _SCRIPTS / "drm-latency-measure.sh"

MS = 1_000_000  # ns per ms


def _approx(a, b, tol=1e-6):
    assert a is not None and b is not None, f"None in approx: {a!r} vs {b!r}"
    assert abs(a - b) <= tol, f"{a!r} != {b!r} (tol {tol})"


# --------------------------------------------------------------------------- #
# build_records — zip per-frame decoded maps + capture wall-ts into records
# --------------------------------------------------------------------------- #
def test_build_records_selects_target_run_id_and_marks_undecoded():
    pfm = [{9: 100, 911013: 0}, {9: 200}, {5: 5}]
    cts = [1000, 1200, 1400]
    recs = r.build_records(pfm, cts, run_id=9)
    assert recs == [
        {"frame_index": 0, "capture_ts_ns": 1000, "emit_ts_ns": 100},
        {"frame_index": 1, "capture_ts_ns": 1200, "emit_ts_ns": 200},
        {"frame_index": 2, "capture_ts_ns": 1400, "emit_ts_ns": None},
    ]


def test_build_records_truncates_to_the_shorter_of_maps_and_timestamps():
    pfm = [{9: 100}, {9: 200}, {9: 300}]
    cts = [1000, 1200]  # one fewer capture ts than frames
    recs = r.build_records(pfm, cts, run_id=9)
    assert len(recs) == 2
    assert recs[-1] == {"frame_index": 1, "capture_ts_ns": 1200, "emit_ts_ns": 200}


# --------------------------------------------------------------------------- #
# pair_latencies — latency = capture_ts_ns − emit_ts_ns for decoded frames
# --------------------------------------------------------------------------- #
def test_pair_latencies_counts_decoded_and_undecoded():
    recs = [
        {"frame_index": 0, "capture_ts_ns": 1000, "emit_ts_ns": 100},
        {"frame_index": 1, "capture_ts_ns": 1200, "emit_ts_ns": 200},
        {"frame_index": 2, "capture_ts_ns": 1400, "emit_ts_ns": None},
    ]
    p = r.pair_latencies(recs)
    assert p["latencies_ns"] == [900, 1000]
    assert p["n_frames"] == 3
    assert p["n_decoded"] == 2
    assert p["n_undecoded"] == 1


def test_pair_latencies_all_undecoded():
    recs = [{"frame_index": 0, "capture_ts_ns": 1000, "emit_ts_ns": None}]
    p = r.pair_latencies(recs)
    assert p["latencies_ns"] == []
    assert p["n_decoded"] == 0
    assert p["n_undecoded"] == 1


# --------------------------------------------------------------------------- #
# percentile — documented nearest-rank on an ascending list
# --------------------------------------------------------------------------- #
def test_percentile_nearest_rank():
    vals = [10.0, 20.0, 30.0, 40.0, 50.0]
    _approx(r.percentile(vals, 50), 30.0)
    _approx(r.percentile(vals, 95), 50.0)
    _approx(r.percentile(vals, 99), 50.0)


def test_percentile_single_value():
    _approx(r.percentile([7.0], 95), 7.0)
    _approx(r.percentile([7.0], 99), 7.0)


# --------------------------------------------------------------------------- #
# summarize — distribution of latencies (ns in, ms out)
# --------------------------------------------------------------------------- #
def test_summarize_three_values():
    s = r.summarize([10 * MS, 20 * MS, 30 * MS])
    assert s["n"] == 3
    _approx(s["median_ms"], 20.0)
    _approx(s["mean_ms"], 20.0)
    _approx(s["min_ms"], 10.0)
    _approx(s["max_ms"], 30.0)
    _approx(s["p95_ms"], 30.0)
    _approx(s["p99_ms"], 30.0)
    _approx(s["jitter_ms"], 10.0)  # sample stdev of [10,20,30]


def test_summarize_single_value_zero_jitter():
    s = r.summarize([15 * MS])
    assert s["n"] == 1
    _approx(s["median_ms"], 15.0)
    _approx(s["jitter_ms"], 0.0)
    _approx(s["min_ms"], 15.0)
    _approx(s["max_ms"], 15.0)


def test_summarize_empty_is_all_none():
    s = r.summarize([])
    assert s["n"] == 0
    for k in ("median_ms", "p95_ms", "p99_ms", "jitter_ms", "min_ms", "max_ms", "mean_ms"):
        assert s[k] is None, f"{k} should be None on empty"


def test_summarize_jitter_is_sample_stdev():
    s = r.summarize([10 * MS, 30 * MS])
    _approx(s["jitter_ms"], math.sqrt(200.0))  # sample stdev of [10,30] = sqrt(200)


# --------------------------------------------------------------------------- #
# run_summary — label + counts + undecoded fraction + flat stats
# --------------------------------------------------------------------------- #
def test_run_summary_flattens_counts_and_stats():
    recs = [
        {"frame_index": 0, "capture_ts_ns": 50 * MS, "emit_ts_ns": 40 * MS},  # 10 ms
        {"frame_index": 1, "capture_ts_ns": 70 * MS, "emit_ts_ns": 40 * MS},  # 30 ms
        {"frame_index": 2, "capture_ts_ns": 90 * MS, "emit_ts_ns": None},     # undecoded
    ]
    s = r.run_summary("DORMANT", recs)
    assert s["label"] == "DORMANT"
    assert s["n_frames"] == 3
    assert s["n_decoded"] == 2
    assert s["n_undecoded"] == 1
    _approx(s["undecoded_frac"], 1.0 / 3.0)
    _approx(s["median_ms"], 20.0)


def test_run_summary_empty_frac_is_zero():
    s = r.run_summary("ENABLED", [])
    assert s["n_frames"] == 0
    _approx(s["undecoded_frac"], 0.0)
    assert s["median_ms"] is None


# --------------------------------------------------------------------------- #
# delta_table — DORMANT − ENABLED, delta = enabled − dormant
# --------------------------------------------------------------------------- #
def test_delta_table_signs_enabled_minus_dormant():
    dormant = r.run_summary(
        "DORMANT",
        [{"frame_index": i, "capture_ts_ns": 30 * MS, "emit_ts_ns": 0} for i in range(3)],
    )  # every latency 30 ms
    enabled = r.run_summary(
        "ENABLED",
        [{"frame_index": i, "capture_ts_ns": 20 * MS, "emit_ts_ns": 0} for i in range(3)],
    )  # every latency 20 ms
    rows = r.delta_table(dormant, enabled)
    by_metric = {row["metric"]: row for row in rows}
    assert "median_ms" in by_metric
    med = by_metric["median_ms"]
    _approx(med["dormant_ms"], 30.0)
    _approx(med["enabled_ms"], 20.0)
    _approx(med["delta_ms"], -10.0)  # enabled − dormant; negative = ENABLED lower latency
    for m in ("median_ms", "p95_ms", "p99_ms", "jitter_ms"):
        assert m in by_metric, f"delta table missing metric {m}"


def test_delta_table_none_when_a_side_is_empty():
    dormant = r.run_summary("DORMANT", [{"frame_index": 0, "capture_ts_ns": 30 * MS, "emit_ts_ns": 0}])
    enabled = r.run_summary("ENABLED", [])  # no decoded frames -> all None
    rows = r.delta_table(dormant, enabled)
    med = {row["metric"]: row for row in rows}["median_ms"]
    assert med["enabled_ms"] is None
    assert med["delta_ms"] is None


# --------------------------------------------------------------------------- #
# select_run_id — reuse the shared dominant-run-id, exclude RESERVED aux ticks
# --------------------------------------------------------------------------- #
def test_select_run_id_picks_dominant_non_reserved():
    pfm = [{9: 100, 911013: 0}, {9: 200, 911013: 0}, {5: 5, 911013: 0}]
    assert r.select_run_id(pfm) == 9  # 911013 (aux tick) is reserved, excluded


def test_select_run_id_override_wins():
    pfm = [{9: 100}, {9: 200}]
    assert r.select_run_id(pfm, override=42) == 42


# --------------------------------------------------------------------------- #
# _per_frame_map — reuse mv_skew_snapshot.tick_map, exclude RESERVED aux ticks
# --------------------------------------------------------------------------- #
def _payload(run_id, frame_id, gen_ts_ns):
    import zlib
    body = "%d.%d.%d" % (run_id, frame_id, gen_ts_ns)
    return "P%s.%d" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)


def test_per_frame_map_newest_per_run_and_excludes_reserved():
    texts = [_payload(9, 1, 100), _payload(9, 2, 200), _payload(911013, 1, 0)]
    m = r._per_frame_map(texts)
    assert m == {9: 200}  # newest gen_ts_ns per run_id; 911013 (aux tick) reserved -> excluded


def test_per_frame_map_drops_crc_bad_and_malformed():
    texts = ["not-a-payload", "P9.1.100.999999", _payload(7, 1, 50)]  # bad CRC + junk dropped
    assert r._per_frame_map(texts) == {7: 50}


# --------------------------------------------------------------------------- #
# impure-glue static anchors — the two 🔴 fixes have no local ffmpeg test path,
# so pin them in the source text (the CLAUDE.md "extra review rigor" net).
# --------------------------------------------------------------------------- #
def test_py_extract_frames_has_input_before_fps_mode():
    text = (_SCRIPTS / "drm_latency_report.py").read_text()
    i_idx = text.find('"-i", capture_path')
    fps_idx = text.find('"-fps_mode"')
    assert i_idx != -1 and fps_idx != -1
    assert i_idx < fps_idx, "-fps_mode (output opt) must come AFTER -i, else ffmpeg errors"


def test_py_ffprobe_has_epoch_lost_guard():
    text = (_SCRIPTS / "drm_latency_report.py").read_text()
    assert "epoch lost" in text.lower(), "the -copyts epoch-lost sanity guard must be present"


# --------------------------------------------------------------------------- #
# format_* — human output (substring assertions)
# --------------------------------------------------------------------------- #
def test_format_run_summary_names_label_and_stats():
    s = r.run_summary("DORMANT", [{"frame_index": 0, "capture_ts_ns": 25 * MS, "emit_ts_ns": 5 * MS}])
    out = r.format_run_summary(s)
    assert "DORMANT" in out
    assert "median" in out.lower()
    assert "p95" in out.lower()
    assert "p99" in out.lower()
    assert "jitter" in out.lower()


def test_format_delta_table_has_three_columns_and_metric_rows():
    dormant = r.run_summary("DORMANT", [{"frame_index": 0, "capture_ts_ns": 30 * MS, "emit_ts_ns": 0}])
    enabled = r.run_summary("ENABLED", [{"frame_index": 0, "capture_ts_ns": 20 * MS, "emit_ts_ns": 0}])
    out = r.format_delta_table(dormant, enabled)
    assert "DORMANT" in out
    assert "ENABLED" in out
    assert "DELTA" in out.upper()
    assert "median" in out.lower()


# --------------------------------------------------------------------------- #
# scripts/drm-latency-measure.sh — static-anchor sanity for the orchestrator
# --------------------------------------------------------------------------- #
def _source_and_run(builder_call):
    """Source the orchestrator (source-guard stops before main) and run a pure builder.

    Mirrors tests/deploy_genlock_fleet.rs::run_sourced — `set -uo pipefail` (deliberately NO -e)
    then `set +e` after sourcing, so the sourced script's own `set -euo pipefail` never leaks -e
    into this harness.
    """
    harness = 'set -uo pipefail\n. "%s"\nset +e\n%s' % (SH, builder_call)
    out = subprocess.run(["bash", "-c", harness], capture_output=True, text=True)
    return out


def test_sh_exists_and_parses():
    assert SH.is_file(), f"{SH} missing"
    p = subprocess.run(["bash", "-n", str(SH)], capture_output=True, text=True)
    assert p.returncode == 0, f"bash -n failed:\n{p.stderr}"


def test_sh_shellcheck_clean():
    if not _which("shellcheck"):
        return  # shellcheck not installed here; CI's shellcheck job covers it
    p = subprocess.run(["shellcheck", "-S", "warning", str(SH)], capture_output=True, text=True)
    assert p.returncode == 0, f"shellcheck warnings:\n{p.stdout}\n{p.stderr}"


def test_sh_cam2_program_builder_is_pure_and_shaped():
    out = _source_and_run(
        'drm_latency_cam2_program /dev/video0 mjpeg 1920x1080 60 8 /tmp/drm-lat-x.nut DORMANT'
    )
    assert out.returncode == 0, f"builder failed: {out.stderr}"
    prog = out.stdout
    assert "systemctl stop camera-box" in prog
    assert "ffmpeg" in prog
    assert "/dev/video0" in prog
    assert "use_wallclock_as_timestamps" in prog
    assert "trap" in prog  # the always-restore trap
    assert "systemctl restart camera-box" in prog
    assert "/tmp/drm-lat-x.nut" in prog


def test_sh_burn_builder_uses_obs_burn_filter():
    out = _source_and_run('drm_latency_burn_cmd add 10.77.9.182 "CAM1 (usb)"')
    assert out.returncode == 0, out.stderr
    assert "obs_burn_filter.py" in out.stdout
    assert " add " in out.stdout
    assert "--host 10.77.9.182" in out.stdout
    assert "CAM1 (usb)" in out.stdout


def test_sh_scp_builder_pulls_capture_back():
    out = _source_and_run('drm_latency_scp_cmd root 10.77.9.62 /tmp/drm-lat-x.nut /tmp/local-x.nut')
    assert out.returncode == 0, out.stderr
    assert "scp" in out.stdout
    assert "10.77.9.62" in out.stdout
    assert "/tmp/drm-lat-x.nut" in out.stdout


def test_sh_default_mode_is_plan_and_touches_nothing():
    # The DEFAULT (NO --plan, NO --execute) must be plan/dry-run and touch nothing — assert the real
    # default, not an explicitly-passed --plan.
    p = subprocess.run(
        ["bash", str(SH), "--label", "DORMANT", "--imag-input", "CAM1 (usb)"],
        capture_output=True, text=True,
    )
    assert p.returncode == 0, f"default mode failed: {p.stderr}"
    low = (p.stdout + p.stderr).lower()
    assert "plan" in low or "dry" in low
    # No ssh/scp actually executed in plan mode — the plan only PRINTS the commands.
    assert "obs_burn_filter.py" in p.stdout
    assert "ffmpeg" in p.stdout


def test_sh_never_writes_drm_output_json():
    # M3 tooling must NEVER flip ~/.camera-box/drm-output.json (that is the M4 supervisor runbook
    # step). Assert no WRITE-shaped pattern targets it (a bare mention in a comment/echo is fine —
    # a redirect / tee / cp / mv / sed -i into it is not). The vacuous "or 'never' in text" check
    # the first cut used would pass for ANY future edit, so pin the write shapes instead.
    import re
    text = SH.read_text()
    write_pats = [
        r'>\s*\S*drm-output\.json',
        r'>>\s*\S*drm-output\.json',
        r'\btee\b[^\n]*drm-output\.json',
        r'\b(?:cp|mv|install)\b[^\n]*drm-output\.json',
        r'\bsed\b\s+-i[^\n]*drm-output\.json',
    ]
    for pat in write_pats:
        assert not re.search(pat, text), f"script appears to WRITE drm-output.json (pattern {pat!r})"


def test_sh_grab_uses_copyts_and_is_bounded_by_timeout():
    # The wallclock epoch must survive the copy-mux (-copyts) or every latency is garbage; and the
    # grab must be bounded (a wedged V4L2 read must not hang the campaign with cam2 stopped).
    out = _source_and_run(
        'drm_latency_cam2_program /dev/video0 mjpeg 1920x1080 60 8 /tmp/drm-lat-x.nut DORMANT')
    assert out.returncode == 0, out.stderr
    assert "-copyts" in out.stdout, "grab must use ffmpeg -copyts to preserve the wallclock epoch"
    assert "timeout" in out.stdout, "the remote grab must be bounded by timeout (wedge guard)"


def test_sh_rejects_a_label_with_a_space():
    # A label is interpolated into paths + remote text; a space/slash must be rejected up front.
    p = subprocess.run(
        ["bash", str(SH), "--plan", "--label", "bad label", "--imag-input", "X"],
        capture_output=True, text=True,
    )
    assert p.returncode != 0, "a label containing a space must be rejected"
    assert "label" in (p.stdout + p.stderr).lower()


def _which(name):
    from shutil import which
    return which(name)


if __name__ == "__main__":
    import types
    mod = sys.modules[__name__]
    failures = 0
    for name in sorted(dir(mod)):
        if name.startswith("test_"):
            fn = getattr(mod, name)
            if isinstance(fn, types.FunctionType):
                try:
                    fn()
                    print(f"ok   {name}")
                except Exception as e:  # noqa: BLE001
                    failures += 1
                    print(f"FAIL {name}: {e}")
    sys.exit(1 if failures else 0)
