#!/usr/bin/env python3
"""#1152 M3 — offline decode + latency report for the DRM-lease-vs-X measurement (Approach 1).

The cam2 grabber physically taps imag's HDMI (projection-tap, issue 781/1196). During a measurement
window imag's Program carries the existing QR burn whose `gen_ts_ns` field is the emit wall clock
(genlock CLOCK_REALTIME ns; wire format `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}`, src/probe/
payload.rs). `scripts/drm-latency-measure.sh` captures a short raw-V4L2 clip off cam2 with per-frame
capture wall-ts (ffmpeg `-use_wallclock_as_timestamps 1`) and scp's it to dev1; this tool decodes
each frame's burn QR and pairs the capture wall-ts against the decoded emit-ts. Per-frame latency =
capture_ts_ns − emit_ts_ns; the distribution (median/p95/p99 + jitter) is produced for a DORMANT and
an ENABLED run, and the DORMANT-vs-ENABLED delta (delta = ENABLED − DORMANT) cancels the grabber's
fixed systematic offset (we want the delta, not the absolute number).

Design split (Tier-0 #557: cargo cannot run locally, so the decision logic is PURE python):
  * PURE core (unit-tested in tests/python/test_drm_latency_report_1152.py, stdlib only):
    build_records / pair_latencies / percentile / summarize / run_summary / delta_table /
    select_run_id / format_*.
  * IMPURE glue (records_from_capture and its ffmpeg/ffprobe/cv2 helpers): needs the rig capture +
    ffmpeg + cv2; NOT unit-tested here — exercised by the supervisor rig campaign. It REUSES the
    proven decoders (`qr_screenshot_check.decode_qr_codes_from_image_bytes` + the CRC-validating
    `mv_skew_snapshot.parse_payload`), never a second copy. All heavy imports are LOCAL so this
    module stays importable with only the stdlib.

Usage:
  # decode a captured clip -> per-run summary JSON (impure; needs ffmpeg + cv2 on dev1):
  drm_latency_report.py run --label DORMANT --capture drm-lat-DORMANT.nut --out dormant.json
  drm_latency_report.py run --label ENABLED --capture drm-lat-ENABLED.nut --out enabled.json
  # DORMANT vs ENABLED delta table from two summaries (pure):
  drm_latency_report.py delta --dormant dormant.json --enabled enabled.json
  # replay from pre-decoded records (pure; no ffmpeg/cv2 — testing / re-report):
  drm_latency_report.py run --label DORMANT --records records.json
"""
import argparse
import glob
import json
import math
import os
import statistics
import sys

MS_PER_NS = 1_000_000.0

# The stats compared in the DORMANT vs ENABLED delta table.
DELTA_METRICS = ("median_ms", "p95_ms", "p99_ms", "jitter_ms")


# --------------------------------------------------------------------------- #
# PURE core — no I/O, no ffmpeg, no cv2 (unit-tested under Tier-0)
# --------------------------------------------------------------------------- #
def build_records(per_frame_maps, capture_ts_ns_list, run_id):
    """Zip per-frame decoded {run_id: gen_ts_ns} maps with the parallel capture wall-ts list into
    latency records. Each record picks `run_id`'s gen_ts_ns as the emit-ts (None = that frame did
    not decode the target burn -> undecodable). Truncates to the shorter of the two inputs (a
    frame/pts count mismatch is truncated, never fabricated)."""
    n = min(len(per_frame_maps), len(capture_ts_ns_list))
    records = []
    for i in range(n):
        m = per_frame_maps[i] or {}
        emit = m.get(run_id)
        records.append({
            "frame_index": i,
            "capture_ts_ns": int(capture_ts_ns_list[i]),
            "emit_ts_ns": (int(emit) if emit is not None else None),
        })
    return records


def pair_latencies(records):
    """Per-frame latency = capture_ts_ns − emit_ts_ns for every decoded frame. Returns the raw
    latency list (ns) plus decoded/undecoded counts."""
    latencies_ns = []
    decoded = 0
    for rec in records:
        emit = rec.get("emit_ts_ns")
        if emit is not None:
            latencies_ns.append(int(rec["capture_ts_ns"]) - int(emit))
            decoded += 1
    n = len(records)
    return {
        "latencies_ns": latencies_ns,
        "n_frames": n,
        "n_decoded": decoded,
        "n_undecoded": n - decoded,
    }


def percentile(values, p):
    """The p-th percentile by the nearest-rank method on an ascending sort:
    idx = ceil(p/100 * n) − 1, clamped to [0, n−1]. Deterministic and exact-testable — the right
    shape for a measurement report (never interpolates a value that was not observed)."""
    if not values:
        raise ValueError("percentile of an empty sequence")
    ordered = sorted(values)
    n = len(ordered)
    rank = math.ceil((p / 100.0) * n)
    idx = min(max(rank - 1, 0), n - 1)
    return float(ordered[idx])


def summarize(latencies_ns):
    """Distribution of a latency list (ns in, ms out). `jitter_ms` is the SAMPLE standard deviation
    (statistics.stdev, n−1; 0.0 for a single sample). Empty -> n=0 and every stat None."""
    n = len(latencies_ns)
    if n == 0:
        return {"n": 0, "median_ms": None, "p95_ms": None, "p99_ms": None,
                "jitter_ms": None, "min_ms": None, "max_ms": None, "mean_ms": None}
    ms = [v / MS_PER_NS for v in latencies_ns]
    jitter = statistics.stdev(ms) if n >= 2 else 0.0
    return {
        "n": n,
        "median_ms": float(statistics.median(ms)),
        "p95_ms": percentile(ms, 95),
        "p99_ms": percentile(ms, 99),
        "jitter_ms": float(jitter),
        "min_ms": float(min(ms)),
        "max_ms": float(max(ms)),
        "mean_ms": float(statistics.fmean(ms)),
    }


def run_summary(label, records):
    """A full per-run summary: label + frame/decoded/undecoded counts + undecoded fraction + the
    flattened distribution stats. This is the JSON one measurement run persists."""
    paired = pair_latencies(records)
    stats = summarize(paired["latencies_ns"])
    n_frames = paired["n_frames"]
    frac = (paired["n_undecoded"] / n_frames) if n_frames else 0.0
    out = {
        "label": label,
        "n_frames": n_frames,
        "n_decoded": paired["n_decoded"],
        "n_undecoded": paired["n_undecoded"],
        "undecoded_frac": frac,
    }
    out.update(stats)
    return out


def delta_table(dormant, enabled, metrics=DELTA_METRICS):
    """DORMANT vs ENABLED rows. `delta_ms = enabled − dormant` (negative = the ENABLED path has the
    LOWER latency, i.e. the DRM-lease output saves latency); None when either side is None."""
    rows = []
    for metric in metrics:
        d = dormant.get(metric)
        e = enabled.get(metric)
        delta = (e - d) if (d is not None and e is not None) else None
        rows.append({"metric": metric, "dormant_ms": d, "enabled_ms": e, "delta_ms": delta})
    return rows


def select_run_id(per_frame_maps, override=None):
    """The burn run_id to pair on: an explicit override, else the DOMINANT non-reserved run_id
    across all frames (reuses mv_skew_snapshot.dominant_run_id, which already excludes the reserved
    node-burn / aux-tick ids — e.g. AUX_TICK_RUN_ID 911013 whose gen_ts_ns is always 0). Returns
    None when nothing decodable was found."""
    if override is not None:
        return int(override)
    from mv_skew_snapshot import dominant_run_id  # lazy: pure fn, but keep the import local
    return dominant_run_id(per_frame_maps)


def _fmt(value):
    return "n/a" if value is None else "%.2f ms" % value


def format_run_summary(summary):
    """Human-readable per-run block (also the substring contract the Tier-0 test pins)."""
    lines = [
        "[drm-latency] run=%s  frames=%d decoded=%d undecoded=%d (%.1f%%)" % (
            summary["label"], summary["n_frames"], summary["n_decoded"],
            summary["n_undecoded"], summary["undecoded_frac"] * 100.0),
        "  median=%s  p95=%s  p99=%s  jitter(stdev)=%s" % (
            _fmt(summary["median_ms"]), _fmt(summary["p95_ms"]),
            _fmt(summary["p99_ms"]), _fmt(summary["jitter_ms"])),
        "  min=%s  max=%s  mean=%s" % (
            _fmt(summary["min_ms"]), _fmt(summary["max_ms"]), _fmt(summary["mean_ms"])),
    ]
    if summary.get("burn_run_id") is not None:
        lines.append("  burn_run_id=%s" % summary["burn_run_id"])
    return "\n".join(lines)


def format_delta_table(dormant, enabled, metrics=DELTA_METRICS):
    """Human-readable DORMANT vs ENABLED delta table."""
    rows = delta_table(dormant, enabled, metrics)
    header = "%-12s %14s %14s %16s" % ("metric", "DORMANT", "ENABLED", "DELTA (E-D)")
    out = [
        "[drm-latency] DORMANT vs ENABLED delta (delta = ENABLED - DORMANT; "
        "negative = ENABLED lower latency)",
        "  DORMANT=%s (n_decoded=%d)   ENABLED=%s (n_decoded=%d)" % (
            dormant.get("label", "?"), dormant.get("n_decoded", 0),
            enabled.get("label", "?"), enabled.get("n_decoded", 0)),
        "  " + header,
    ]
    for row in rows:
        out.append("  " + "%-12s %14s %14s %16s" % (
            row["metric"], _fmt(row["dormant_ms"]), _fmt(row["enabled_ms"]), _fmt(row["delta_ms"])))
    return "\n".join(out)


# --------------------------------------------------------------------------- #
# IMPURE glue — decode a captured clip into records (needs ffmpeg + cv2 + rig)
# --------------------------------------------------------------------------- #
def _ffprobe_capture_ts_ns(capture_path):
    """Per-frame capture wall-ts (ns), in stream order, read from the wallclock-stamped clip's PTS
    (ffmpeg `-use_wallclock_as_timestamps 1` writes each packet's PTS as the CLOCK_REALTIME instant
    the frame arrived at cam2 — a dantesync-synced clock, ~89µs offset from the genlock master)."""
    import subprocess
    proc = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "frame=pts_time", "-of", "csv=p=0", capture_path],
        capture_output=True, text=True, check=True)
    out = []
    for line in proc.stdout.splitlines():
        token = line.strip().rstrip(",")
        if not token:
            continue
        try:
            out.append(int(round(float(token) * 1e9)))
        except ValueError:
            # a non-numeric ffprobe row (e.g. "N/A" for a frame with no PTS): keep POSITION with a
            # None placeholder (never drop it silently — dropping shifts every later frame's
            # timestamp by one, a ~16.7ms systematic error). records_from_capture excludes a None ts.
            sys.stderr.write("[drm-latency] WARNING: non-numeric pts_time %r -> None placeholder\n" % token)
            out.append(None)
    # Epoch sanity: with `-copyts` the capture's PTS are CLOCK_REALTIME epoch seconds (~1.78e9 in
    # 2026). A first valid PTS below year-2001 means ffmpeg rebased the epoch to ~0 (the grab was
    # made WITHOUT -copyts) and every latency would be garbage — fail LOUD, never report nonsense.
    first = next((v for v in out if v is not None), None)
    if first is not None and first < 1_000_000_000_000_000_000:  # < 1e9 s expressed in ns
        raise RuntimeError(
            "[drm-latency] capture epoch lost (first pts=%.3fs < 1e9s) — the grab was made WITHOUT "
            "ffmpeg -copyts, so the wallclock timestamps are meaningless. Re-grab with -copyts." %
            (first / 1e9))
    return out


def _extract_frames(capture_path, frames_dir):
    """Extract every frame to a PNG in stream order (`-fps_mode passthrough` keeps 1:1, no dup/drop
    so the Nth PNG lines up with the Nth ffprobe pts entry)."""
    import subprocess
    os.makedirs(frames_dir, exist_ok=True)
    pattern = os.path.join(frames_dir, "f-%06d.png")
    # `-fps_mode passthrough` is an OUTPUT option — it MUST come after `-i` (before -i is a fatal
    # ffmpeg error: "Option fps_mode cannot be applied to input url"). It keeps 1:1 (no dup/drop)
    # so the Nth PNG lines up with the Nth ffprobe pts entry.
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error",
         "-i", capture_path, "-fps_mode", "passthrough", pattern],
        check=True)
    return sorted(glob.glob(os.path.join(frames_dir, "f-*.png")))


def _decode_frame_qrs(png_path):
    """All QR texts in one PNG, via the SAME proven decoder qr_screenshot_check uses (cv2
    QRCodeDetector) — never a second decoder copy."""
    from qr_screenshot_check import decode_qr_codes_from_image_bytes  # local: pulls cv2 only here
    with open(png_path, "rb") as f:
        return decode_qr_codes_from_image_bytes(f.read())


def _per_frame_map(qr_texts):
    """{run_id: newest gen_ts_ns} for one frame, minus the reserved node-burn / aux-tick ids.
    Reuses mv_skew_snapshot.tick_map (the shared newest-gen_ts_ns-per-run_id fold over CRC-valid
    payloads) — never a second parser/fold copy — then drops RESERVED_RUN_IDS (esp. AUX_TICK_RUN_ID
    911013, whose gen_ts_ns is always 0)."""
    from mv_skew_snapshot import tick_map, RESERVED_RUN_IDS  # local import (pure, no cv2)
    return {run_id: gen_ts_ns for run_id, gen_ts_ns in tick_map(qr_texts).items()
            if run_id not in RESERVED_RUN_IDS}


def records_from_capture(capture_path, frames_dir=None, run_id_override=None):
    """Decode a captured clip into latency records + the chosen burn run_id. IMPURE (ffmpeg +
    ffprobe + cv2). Returns (records, run_id). A frame with no capture timestamp (a None pts
    placeholder) is excluded from pairing but never shifts the others (positional alignment)."""
    import shutil
    import tempfile
    created = frames_dir is None
    work = frames_dir or tempfile.mkdtemp(prefix="drm-lat-frames-")
    try:
        frames = _extract_frames(capture_path, work)
        capture_ts = _ffprobe_capture_ts_ns(capture_path)
        n = min(len(frames), len(capture_ts))
        if len(frames) != len(capture_ts):
            sys.stderr.write(
                "[drm-latency] WARNING: %d frames vs %d pts entries — truncating to %d "
                "(fps_mode passthrough should keep them 1:1)\n" % (len(frames), len(capture_ts), n))
        per_frame_maps = [_per_frame_map(_decode_frame_qrs(f)) for f in frames[:n]]
        capture_ts = capture_ts[:n]
        run_id = select_run_id(per_frame_maps, run_id_override)
        # Keep POSITIONAL alignment: drop only the frames with a None capture ts, in lock-step, and
        # remember each survivor's ORIGINAL frame index for traceability.
        maps_ok, ts_ok, idx_ok = [], [], []
        for i in range(n):
            if capture_ts[i] is None:
                continue
            maps_ok.append(per_frame_maps[i])
            ts_ok.append(capture_ts[i])
            idx_ok.append(i)
        if run_id is None:
            sys.stderr.write(
                "[drm-latency] ERROR: no non-reserved burn run_id decoded in any frame — was the "
                "burn ON on the imag program input during the capture window?\n")
            return ([{"frame_index": idx_ok[k], "capture_ts_ns": ts_ok[k], "emit_ts_ns": None}
                     for k in range(len(idx_ok))], None)
        records = build_records(maps_ok, ts_ok, run_id)
        for rec, original_index in zip(records, idx_ok):
            rec["frame_index"] = original_index
        return records, run_id
    finally:
        # Clean up the ~hundreds of full-res PNGs we extracted (GBs at 60fps) — but only when WE
        # created the temp dir; a caller-supplied --frames-dir is left for inspection.
        if created:
            shutil.rmtree(work, ignore_errors=True)


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def _cmd_run(args):
    if args.records:
        with open(args.records) as f:
            records = json.load(f)
        run_id = args.burn_run_id
    else:
        records, run_id = records_from_capture(args.capture, args.frames_dir, args.burn_run_id)
    summary = run_summary(args.label, records)
    if run_id is not None:
        summary["burn_run_id"] = run_id
    print(format_run_summary(summary))
    if args.out_records:
        with open(args.out_records, "w") as f:
            json.dump(records, f, indent=2)
        print("[drm-latency] wrote %s (decoded records — replay with --records)" % args.out_records)
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=2)
        print("[drm-latency] wrote %s" % args.out)
    return 0


def _cmd_delta(args):
    with open(args.dormant) as f:
        dormant = json.load(f)
    with open(args.enabled) as f:
        enabled = json.load(f)
    print(format_delta_table(dormant, enabled))
    if args.out:
        with open(args.out, "w") as f:
            json.dump({"dormant": dormant, "enabled": enabled,
                       "delta": delta_table(dormant, enabled)}, f, indent=2)
        print("[drm-latency] wrote %s" % args.out)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description="#1152 M3 DRM-latency offline decode + report")
    sub = ap.add_subparsers(dest="action", required=True)

    prun = sub.add_parser("run", help="decode a capture (or replay records) into a run summary")
    prun.add_argument("--label", required=True, help="rig state label for this run (e.g. DORMANT / ENABLED)")
    prun.add_argument("--capture", help="captured clip (wallclock-stamped) to decode")
    prun.add_argument("--records", help="pre-decoded records JSON (pure replay; skips ffmpeg/cv2)")
    prun.add_argument("--frames-dir", help="dir for extracted frames (default: a temp dir)")
    prun.add_argument("--burn-run-id", type=int, help="override the auto-selected burn run_id")
    prun.add_argument("--out", help="write the run summary JSON here")
    prun.add_argument("--out-records", help="also persist the decoded per-frame records JSON "
                                            "(replay later with --records, no ffmpeg/cv2 re-decode)")
    prun.set_defaults(func=_cmd_run)

    pdel = sub.add_parser("delta", help="DORMANT vs ENABLED delta table from two run summaries")
    pdel.add_argument("--dormant", required=True, help="DORMANT run summary JSON")
    pdel.add_argument("--enabled", required=True, help="ENABLED run summary JSON")
    pdel.add_argument("--out", help="write the combined delta JSON here")
    pdel.set_defaults(func=_cmd_delta)

    args = ap.parse_args(argv)
    if args.action == "run" and not args.capture and not args.records:
        ap.error("run needs --capture <clip> or --records <json>")
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
