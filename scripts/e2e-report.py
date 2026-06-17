#!/usr/bin/env python3
"""
e2e-report.py — E2E delivery-continuity + latency graphs + per-hop table → LAN URL.

Consumes:
  --json   MultiTapReport JSON artifact from multitap-probe
  --series per-frame JSONL ({tap, frame_id, recv_ts_ns, node_emit_tc_ns})
  --out    output PNG path (default /tmp/e2e-report.png)

Produces one PNG and then runs:
  python3 ~/devel/airuleset/airuleset.py share <png>
printing the resulting URL.
"""

import matplotlib
matplotlib.use("Agg")  # headless, must come before pyplot import

import argparse
import json
import os
import subprocess
import sys
from collections import defaultdict

import matplotlib.pyplot as plt
import matplotlib.gridspec as gridspec
import matplotlib.patches as mpatches
import numpy as np


# ──────────────────────────────────────────────────────────────────────────────
# Argument parsing
# ──────────────────────────────────────────────────────────────────────────────

def parse_args():
    p = argparse.ArgumentParser(description="E2E report: delivery-continuity + latency graphs + per-hop table")
    p.add_argument("--json",   required=True, help="MultiTapReport JSON artifact")
    p.add_argument("--series", required=True, help="Per-frame JSONL series file")
    p.add_argument("--out",    default="/tmp/e2e-report.png", help="Output PNG path")
    return p.parse_args()


# ──────────────────────────────────────────────────────────────────────────────
# Data loading
# ──────────────────────────────────────────────────────────────────────────────

def load_json(path):
    with open(path) as f:
        return json.load(f)


def load_series(path):
    """Return a list of raw observation rows (dicts: tap, frame_id, recv_ts_ns, node_emit_tc_ns)."""
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rows.append(json.loads(line))
    return rows


# ──────────────────────────────────────────────────────────────────────────────
# Helper: latency stats from hop dict
# ──────────────────────────────────────────────────────────────────────────────

def hop_latency_label(hop):
    """Return p50/p99 label string from a hop dict (emit_latency preferred, else latency)."""
    for key in ("emit_latency", "latency"):
        lat = hop.get(key)
        if lat is not None:
            return f"p50={lat['p50_ms']:.1f}ms p99={lat['p99_ms']:.1f}ms"
    return "latency: n/a"


def hop_p50_p99(hop):
    """Return (p50_ms, p99_ms) or (None, None) from a hop dict."""
    for key in ("emit_latency", "latency"):
        lat = hop.get(key)
        if lat is not None:
            return lat["p50_ms"], lat["p99_ms"]
    return None, None


def tap_p50_p99(tap_summary):
    """Return (p50_ms, p99_ms) from a tap's abs_emit_latency or None."""
    lat = tap_summary.get("abs_emit_latency")
    if lat is not None:
        return lat["p50_ms"], lat["p99_ms"]
    return None, None


# ──────────────────────────────────────────────────────────────────────────────
# Graph 1: Delivery continuity scatter
# ──────────────────────────────────────────────────────────────────────────────

def plot_continuity(ax, series_rows, tap_names):
    """
    Scatter plot: x=frame_id, y=tap lane index.
    One dot per delivered (tap, frame_id).  Gaps in a lane = dropped frames.
    """
    # Build per-tap delivered sets (unique frame_ids in series)
    tap_delivered = defaultdict(set)
    for row in series_rows:
        tap_delivered[row["tap"]].add(row["frame_id"])

    colors = plt.cm.tab10(np.linspace(0, 0.7, max(len(tap_names), 1)))
    patches = []

    for lane_idx, tap_name in enumerate(tap_names):
        fids = sorted(tap_delivered.get(tap_name, set()))
        if fids:
            ax.scatter(fids, [lane_idx] * len(fids),
                       s=3, color=colors[lane_idx], linewidths=0, alpha=0.7)
        count = len(fids)
        label = f"{tap_name} ({count} delivered)"
        patches.append(mpatches.Patch(color=colors[lane_idx], label=label))

    ax.set_yticks(range(len(tap_names)))
    ax.set_yticklabels(tap_names)
    ax.set_xlabel("frame_id")
    ax.set_title("Graph 1 — Delivery continuity  (gap = dropped frame)")
    ax.legend(handles=patches, loc="lower right", fontsize=8, markerscale=3)
    ax.grid(axis="x", linestyle="--", alpha=0.3)


# ──────────────────────────────────────────────────────────────────────────────
# Graph 2: Per-frame latency from node_emit_tc_ns deltas
# ──────────────────────────────────────────────────────────────────────────────

def plot_latency(ax, series_rows, tap_names, hops_json):
    """
    For each adjacent tap pair, compute per-frame transit from node_emit_tc_ns deltas.
    y = ms.  Also annotate p50/p99 from the JSON hop stats.
    """
    if len(tap_names) < 2:
        ax.set_title("Graph 2 — Per-frame latency (< 2 taps)")
        return

    # Build per-tap dict: frame_id -> first node_emit_tc_ns (ns)
    tap_emit = defaultdict(dict)  # tap_name -> {frame_id: emit_ns}
    for row in series_rows:
        tap = row["tap"]
        fid = row["frame_id"]
        emit_ns = row.get("node_emit_tc_ns", 0)
        if emit_ns == 0:
            continue
        if fid not in tap_emit[tap] or emit_ns < tap_emit[tap][fid]:
            tap_emit[tap][fid] = emit_ns

    hop_colors = plt.cm.Set1(np.linspace(0, 0.8, max(len(tap_names) - 1, 1)))
    any_data = False

    for hop_idx in range(len(tap_names) - 1):
        up_tap = tap_names[hop_idx]
        dn_tap = tap_names[hop_idx + 1]
        hop_label = f"{up_tap}→{dn_tap}"

        up_emit = tap_emit.get(up_tap, {})
        dn_emit = tap_emit.get(dn_tap, {})

        common_ids = sorted(set(up_emit) & set(dn_emit))
        if not common_ids:
            continue

        deltas_ms = [(dn_emit[fid] - up_emit[fid]) / 1e6 for fid in common_ids]
        ax.plot(common_ids, deltas_ms, linewidth=0.7,
                color=hop_colors[hop_idx], label=hop_label, alpha=0.8)
        any_data = True

        # Annotate p50/p99 from JSON hop stats (matching by name pattern)
        matched_hop = None
        for h in hops_json:
            h_name = h.get("name", "")
            if up_tap in h_name and dn_tap in h_name:
                matched_hop = h
                break
        if matched_hop:
            p50, p99 = hop_p50_p99(matched_hop)
            if p50 is not None and p99 is not None:
                ax.axhline(p50, color=hop_colors[hop_idx], linestyle=":",
                           linewidth=1, alpha=0.6)
                ax.axhline(p99, color=hop_colors[hop_idx], linestyle="--",
                           linewidth=1, alpha=0.6)
                ax.text(common_ids[0], p99 + 0.5,
                        f"{hop_label} p50={p50:.1f} p99={p99:.1f}ms",
                        fontsize=7, color=hop_colors[hop_idx])

    if not any_data:
        ax.text(0.5, 0.5, "No paired emit-tc stamps available\n(run with --wall-clock for latency data)",
                transform=ax.transAxes, ha="center", va="center", fontsize=9)

    ax.set_xlabel("frame_id")
    ax.set_ylabel("latency (ms)")
    ax.set_title("Graph 2 — Per-frame hop latency from node_emit_tc_ns deltas")
    ax.legend(fontsize=8)
    ax.grid(axis="both", linestyle="--", alpha=0.3)


# ──────────────────────────────────────────────────────────────────────────────
# Footer table
# ──────────────────────────────────────────────────────────────────────────────

def verdict_str(v):
    """Normalize a HopVerdict (enum string) to display form."""
    mapping = {"Pass": "PASS", "Fail": "FAIL", "Inconclusive": "INCONCL"}
    return mapping.get(v, str(v))


def build_footer_text(report):
    """Return a multi-line string: per-hop table + overall verdict."""
    lines = []
    lines.append("Per-hop summary")
    lines.append(f"{'Hop':<22}  {'Dropped':>7}  {'SC loss%':>8}  {'p50(ms)':>8}  {'p99(ms)':>8}  {'Verdict':>8}")
    lines.append("-" * 72)

    for h in report.get("hops", []):
        name      = h.get("name", "?")[:22]
        dropped   = len(h.get("dropped_ids", []))
        sc_total  = h.get("single_copy_total", 0)
        sc_drop   = h.get("single_copy_dropped", 0)
        sc_pct    = (100.0 * sc_drop / sc_total) if sc_total > 0 else 0.0
        p50, p99  = hop_p50_p99(h)
        p50_s     = f"{p50:.1f}" if p50 is not None else "n/a"
        p99_s     = f"{p99:.1f}" if p99 is not None else "n/a"
        verd      = verdict_str(h.get("verdict", "?"))
        lines.append(f"{name:<22}  {dropped:>7}  {sc_pct:>7.2f}%  {p50_s:>8}  {p99_s:>8}  {verd:>8}")

    # Full-span row
    fs = report.get("full_span", {})
    if fs:
        name     = "FULL SPAN"
        dropped  = len(fs.get("dropped_ids", []))
        sc_total = fs.get("single_copy_total", 0)
        sc_drop  = fs.get("single_copy_dropped", 0)
        sc_pct   = (100.0 * sc_drop / sc_total) if sc_total > 0 else 0.0
        verd     = verdict_str(fs.get("verdict", "?"))
        lines.append(f"{name:<22}  {dropped:>7}  {sc_pct:>7.2f}%  {'n/a':>8}  {'n/a':>8}  {verd:>8}")

    lines.append("")
    vp = report.get("verdict_pass", False)
    lines.append(f"Overall verdict: {'PASS' if vp else 'FAIL/INCONCL'}")
    return "\n".join(lines)


# ──────────────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────────────

def main():
    args = parse_args()

    report      = load_json(args.json)
    series_rows = load_series(args.series)

    # Tap order from the JSON (preserves probe order)
    tap_summaries = report.get("taps", [])
    tap_names     = [t["name"] for t in tap_summaries]
    hops_json     = report.get("hops", [])

    # ── layout: 2 graphs + footer text ──────────────────────────────────────
    fig = plt.figure(figsize=(14, 10))
    gs  = gridspec.GridSpec(3, 1, figure=fig, height_ratios=[2, 2, 1.2],
                            hspace=0.45)

    ax_continuity = fig.add_subplot(gs[0])
    ax_latency    = fig.add_subplot(gs[1])
    ax_footer     = fig.add_subplot(gs[2])

    plot_continuity(ax_continuity, series_rows, tap_names)
    plot_latency(ax_latency, series_rows, tap_names, hops_json)

    # Footer: monospace text table
    footer_text = build_footer_text(report)
    ax_footer.axis("off")
    ax_footer.text(0.01, 0.99, footer_text,
                   transform=ax_footer.transAxes,
                   fontsize=7.5, family="monospace",
                   verticalalignment="top")

    # Run info as figure suptitle
    run_id     = report.get("run_id", "?")
    dur        = report.get("duration_secs", "?")
    lead       = report.get("lead_discard_secs", 0)
    vp         = report.get("verdict_pass", False)
    fig.suptitle(
        f"E2E report  run_id={run_id}  duration={dur}s  lead_discard={lead}s  "
        f"verdict={'PASS' if vp else 'FAIL/INCONCL'}",
        fontsize=11, fontweight="bold"
    )

    out_path = args.out
    fig.savefig(out_path, dpi=120, bbox_inches="tight")
    plt.close(fig)
    print(f"PNG saved: {out_path}")

    # Share via airuleset file-drop and print URL (best-effort)
    share_script = os.path.expanduser("~/devel/airuleset/airuleset.py")
    if os.path.exists(share_script):
        try:
            result = subprocess.run(
                [sys.executable, share_script, "share", out_path],
                capture_output=True, text=True, timeout=30
            )
            if result.returncode == 0:
                for line in result.stdout.splitlines():
                    if line.startswith("http"):
                        print(f"URL: {line}")
                        break
                else:
                    print(result.stdout.strip())
            else:
                print(f"WARNING: share failed (exit {result.returncode}): {result.stderr.strip()}")
                print(f"Local path: {out_path}")
        except Exception as e:
            print(f"WARNING: share error: {e}")
            print(f"Local path: {out_path}")
    else:
        print(f"WARNING: share script not found at {share_script}")
        print(f"Local path: {out_path}")


if __name__ == "__main__":
    main()
