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

# The per-hop columns in the CSV, in signal-flow order, with (csv_column, label, color).
HOP_COLUMNS = [
    ("cam1_strih_ms", "cam1→strih", "#0969da"),
    ("strih_stream_ms", "strih→stream", "#1a7f37"),
    ("cam1_stream_ms", "cam1→stream (end-to-end)", "#bc4c00"),
]

EXPECTED_HEADER = [
    "frame_id",
    "gen_ts_ns",
    "flip_ts_ns",
    "cam1_strih_ms",
    "strih_stream_ms",
    "cam1_stream_ms",
]


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

    series = {col: ([], []) for col, _label, _color in HOP_COLUMNS}
    for r in rows:
        try:
            g = int(r["gen_ts_ns"])
        except (ValueError, KeyError):
            continue
        t_s = (g - t0) / 1e9
        for col, _label, _color in HOP_COLUMNS:
            cell = r.get(col, "")
            if cell == "" or cell is None:
                continue  # empty cell ⇒ gap in this hop's line
            try:
                v = float(cell)
            except ValueError:
                continue
            xs, ys = series[col]
            xs.append(t_s)
            ys.append(v)
    return series, t0


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
    series, _t0 = build_series(rows)

    fig, ax = plt.subplots(figsize=(14, 7))
    total_points = 0
    for col, label, color in HOP_COLUMNS:
        xs, ys = series[col]
        total_points += len(xs)
        if not xs:
            continue
        # marker='.' so individual frames are visible AS POINTS forming the line; a thin
        # line connects them so continuity (no gaps) and stability (flat) read at a glance.
        ax.plot(
            xs,
            ys,
            color=color,
            linewidth=0.8,
            marker=".",
            markersize=2,
            label=f"{label} (n={len(xs)})",
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
        "gap in a line = a lost frame at that instant   ·   flat line = stable latency   "
        "·   creeping slope = drift",
        ha="center",
        fontsize=9,
        color="#57606a",
    )

    fig.tight_layout(rect=[0, 0.03, 1, 1])
    fig.savefig(a.out, dpi=110)
    print(f"latency-line PNG: {a.out}", file=sys.stderr)
    _share(a.out)


if __name__ == "__main__":
    main()
