#!/usr/bin/env python3
"""
latency-line-report.py — the LITERAL continuous-line latency proof (#209).

The user's original ask: SEE the per-frame latency as a CONTINUOUS LINE over the whole
run (points forming a line, latency not fluctuating) — not just summary p50/p99 numbers.
This script reads recording-verdict's per-frame CSV (`--latency-csv`) and draws, per hop,
the literal time-series line:

  * x-axis = wall-clock time since the run's first frame (seconds)
  * y-axis = per-hop latency (ms)
  * one LINE per hop: cam1→strih, strih→stream, cam1→stream

A GAP in a line = a dropped frame at that instant (no row, or that hop's cell empty).
A FLAT line = stable latency (no fluctuation). A creeping slope = drift.

Consumes the CSV columns (single source of truth = recording_latency::LatencyCsvRow::HEADER):
  frame_id,gen_ts_ns,flip_ts_ns,cam1_strih_ms,strih_stream_ms,cam1_stream_ms

Usage:
  scripts/latency-line-report.py --csv latency-per-frame.csv [--out /tmp/latency-line.png]

Then runs `python3 ~/devel/airuleset/airuleset.py share <png>` and prints the LAN URL.
"""
import matplotlib

matplotlib.use("Agg")  # headless, before pyplot

import argparse
import csv
import os
import subprocess
import sys

import matplotlib.pyplot as plt

# The BURN-based hops (real digital-burn measurements) — these draw CONTINUOUSLY: the burns
# ride through the chain even when the cam2 optical read fails, so a gap here would be a real
# chain loss. (csv_column, label, color, is_optical).
HOP_COLUMNS = [
    ("cam1_strih_ms", "cam1→strih", "#0969da", False),
    ("strih_stream_ms", "strih→stream", "#1a7f37", False),
    ("cam1_stream_ms", "cam1→stream (end-to-end)", "#bc4c00", False),
    # The cam2→cam1 OPTICAL-INJECTION leg (cam1 camera filming the cam2 monitor QR). This is
    # NOT a burn — it depends on the cam1 camera OPTICALLY READING the cam2 QR. Where that read
    # fails (an optical-injection dropout), the cell is empty → this line GAPS HONESTLY and the
    # gap is ANNOTATED (#216). It is NEVER drawn across — a read failure is shown, not hidden.
    ("cam2_cam1_ms", "cam2→cam1 (optical-injection, test-rig)", "#8250df", True),
]

EXPECTED_HEADER = [
    "frame_id",
    "gen_ts_ns",
    "flip_ts_ns",
    "cam1_strih_ms",
    "strih_stream_ms",
    "cam1_stream_ms",
    "cam2_cam1_ms",
]

# An optical-read gap shorter than this is normal QR-decode jitter, not a dropout worth
# annotating. The 30-min run's real dropout was ~175 s; a few-frame blink (<2 s) is noise.
MIN_OPTICAL_GAP_S = 2.0


def _share(png):
    """Share the PNG via the airuleset file-drop and print the LAN URL."""
    airuleset = os.path.expanduser("~/devel/airuleset/airuleset.py")
    try:
        out = subprocess.run(
            ["python3", airuleset, "share", png],
            capture_output=True,
            text=True,
            timeout=30,
            check=True,
        )
        url = out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""
        if url:
            print(url)
        else:
            print(f"(share produced no URL; PNG at {png})", file=sys.stderr)
    except Exception as e:  # never fail the run on a share hiccup
        print(f"WARNING: share failed ({e}); PNG at {png}", file=sys.stderr)


def read_rows(csv_path):
    """Read the CSV into a list of dict rows; validate the header is what we expect."""
    with open(csv_path, newline="") as f:
        reader = csv.DictReader(f)
        if reader.fieldnames != EXPECTED_HEADER:
            raise SystemExit(
                f"unexpected CSV header {reader.fieldnames!r}; "
                f"expected {EXPECTED_HEADER!r} (from recording-verdict --latency-csv)"
            )
        return list(reader)


def build_series(rows):
    """
    Build, per hop, the (time_s, latency_ms) point lists for the continuous line.

    Time axis = (gen_ts_ns - t0) / 1e9 (seconds since the first frame). A row whose hop
    cell is empty contributes NO point to that hop's line (a gap = a lost/unpaired frame),
    which is exactly the visual proof the user wants. matplotlib draws a contiguous run as
    a solid line and breaks where points are missing only if we insert NaN; instead we
    keep per-hop lists so each hop is plotted only where it has data.
    """
    # Origin = the smallest valid gen_ts_ns across all rows (run start).
    t0 = None
    for r in rows:
        try:
            g = int(r["gen_ts_ns"])
        except (ValueError, KeyError):
            continue
        if g > 0 and (t0 is None or g < t0):
            t0 = g
    if t0 is None:
        t0 = 0

    series = {col: ([], []) for col, _label, _color, _opt in HOP_COLUMNS}
    # Track whether the previous row had a point for each hop, so we can insert a NaN BREAK at
    # the first empty cell after a run of points. A NaN tells matplotlib to LIFT THE PEN — the
    # line truly BREAKS across the gap (#216: the optical-read dropout must show as a real gap,
    # never a faint connector drawn across the failure).
    had_point = {col: False for col, _l, _c, _o in HOP_COLUMNS}
    for r in rows:
        try:
            g = int(r["gen_ts_ns"])
        except (ValueError, KeyError):
            continue
        t_s = (g - t0) / 1e9
        for col, _label, _color, _opt in HOP_COLUMNS:
            cell = r.get(col, "")
            empty = cell == "" or cell is None
            xs, ys = series[col]
            if empty:
                if had_point[col]:
                    # First empty cell after data ⇒ insert ONE NaN to break the line here.
                    xs.append(t_s)
                    ys.append(float("nan"))
                    had_point[col] = False
                continue
            try:
                v = float(cell)
            except ValueError:
                continue
            xs.append(t_s)
            ys.append(v)
            had_point[col] = True
    return series, t0


def optical_gaps(rows, t0, col="cam2_cam1_ms"):
    """
    Find the WINDOWS where the cam2→cam1 OPTICAL read failed — runs of consecutive rows whose
    `col` cell is empty, longer than MIN_OPTICAL_GAP_S. Returns [(start_s, end_s, dur_s)].

    These are the optical-injection dropouts (#216): the cam1 camera could not read the cam2
    monitor QR for that stretch — a real READABILITY failure on the cam2→cam1 leg, NOT a chain
    frame loss. The plotter draws this line broken across each gap and annotates it here.
    """
    gaps = []
    run_start = None
    last_t = None
    for r in rows:
        try:
            g = int(r["gen_ts_ns"])
        except (ValueError, KeyError):
            continue
        if g <= 0:
            continue
        t_s = (g - t0) / 1e9
        cell = r.get(col, "")
        empty = cell == "" or cell is None
        if empty:
            if run_start is None:
                run_start = last_t if last_t is not None else t_s
        else:
            if run_start is not None and last_t is not None:
                dur = last_t - run_start
                if dur >= MIN_OPTICAL_GAP_S:
                    gaps.append((run_start, last_t, dur))
            run_start = None
        last_t = t_s
    # A trailing gap (recording ended mid-dropout).
    if run_start is not None and last_t is not None and (last_t - run_start) >= MIN_OPTICAL_GAP_S:
        gaps.append((run_start, last_t, last_t - run_start))
    return gaps


def main():
    p = argparse.ArgumentParser(
        description="Per-frame latency continuous-line proof (#209)"
    )
    p.add_argument(
        "--csv", required=True, help="recording-verdict --latency-csv per-frame CSV"
    )
    p.add_argument("--out", default="/tmp/latency-line.png")
    p.add_argument(
        "--title",
        default="Per-frame latency over the run — the continuous line (#209)",
    )
    a = p.parse_args()

    rows = read_rows(a.csv)
    series, t0 = build_series(rows)
    opt_gaps = optical_gaps(rows, t0)

    fig, ax = plt.subplots(figsize=(14, 7))
    total_points = 0
    for col, label, color, is_optical in HOP_COLUMNS:
        xs, ys = series[col]
        # Count only REAL points (NaN entries are pen-lift markers, not data).
        n_real = sum(1 for y in ys if y == y)  # NaN != NaN
        total_points += n_real
        if not xs:
            continue
        # The optical line is drawn slightly thinner + dashed so the eye reads it as the
        # SEPARATE optical-injection leg (it gaps where the read fails); the burn lines are
        # solid (continuous real data). marker='.' so individual frames read AS POINTS.
        ax.plot(
            xs,
            ys,
            color=color,
            linewidth=0.7 if is_optical else 0.8,
            linestyle="--" if is_optical else "-",
            marker=".",
            markersize=2,
            label=f"{label} (n={n_real})",
        )

    # #216 HONEST GAP — shade + label every cam2→cam1 OPTICAL-READ dropout window. The line is
    # already broken there (empty cells contribute no points); this makes the BREAK explicit so
    # it can NEVER be mistaken for a continuous read. A dropout is a cam2→cam1 OPTICAL read
    # failure, NOT a chain frame loss — the burn hops keep drawing through it.
    ymax = 0.0
    for _col, _label, _color, _opt in HOP_COLUMNS:
        _xs, ys = series[_col]
        real = [y for y in ys if y == y]  # drop NaN pen-lift markers
        if real:
            ymax = max(ymax, max(real))
    for i, (gs, ge, dur) in enumerate(opt_gaps):
        ax.axvspan(gs, ge, color="#cf222e", alpha=0.10, zorder=0)
        ax.annotate(
            f"cam1 camera could not read cam2 QR\nfor {dur:.0f}s (optical-injection\ndropout, NOT a frame loss)",
            xy=((gs + ge) / 2, ymax * 0.92 if ymax else 1.0),
            ha="center",
            va="top",
            fontsize=8,
            color="#cf222e",
            fontweight="bold",
            bbox=dict(boxstyle="round,pad=0.3", fc="#fff1f0", ec="#cf222e", lw=0.8),
        )

    ax.set_xlabel("time since run start (s)", fontsize=12)
    ax.set_ylabel("per-hop latency (ms)", fontsize=12)
    ax.set_title(a.title, fontsize=14, fontweight="bold")
    ax.grid(True, alpha=0.3)
    if total_points:
        ax.legend(loc="upper right", fontsize=10)
    else:
        ax.text(
            0.5,
            0.5,
            "no per-frame latency points in CSV\n(no co-located burns in the recording?)",
            transform=ax.transAxes,
            ha="center",
            va="center",
            fontsize=12,
            color="#cf222e",
        )

    fig.text(
        0.5,
        0.01,
        "solid burn lines (cam1→strih, strih→stream, cam1→stream) = the real digital chain — "
        "a gap there is a lost frame   ·   dashed cam2→cam1 = the OPTICAL-injection test leg — "
        "a shaded gap = the cam1 camera could not READ the cam2 QR (a read dropout, NOT a chain loss)",
        ha="center",
        fontsize=8,
        color="#57606a",
    )

    fig.tight_layout(rect=[0, 0.03, 1, 1])
    fig.savefig(a.out, dpi=110)
    print(f"latency-line PNG: {a.out}", file=sys.stderr)
    _share(a.out)


if __name__ == "__main__":
    main()
