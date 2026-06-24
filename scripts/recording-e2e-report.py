#!/usr/bin/env python3
"""
recording-e2e-report.py — 2-graph report for the recording-based 4-node full-path
E2E (#105/#7), rendered from recording-verdict's `--json` summary.

Consumes:
  --json  the recording-verdict 4-node report JSON (per-node verdict + per-hop loss
          + per-hop latency). Produced by `recording-verdict --json <path>`.
  --out   output PNG path (default /tmp/recording-e2e-report.png)

Produces ONE PNG with two panels:
  1. Per-hop LOSS — bars per hop (cam2→cam1 optical, cam1→strih strict, strih→stream
     strict), dropped + phantom counts; strict hops coloured red on ANY loss, the
     optical hop shown as honest-assessment (unknown/out-of-range), with the PASS/FAIL
     verdict per hop.
  2. Per-hop LATENCY — p50/p95/p99 per available hop (cam2→cam1, cam→strih,
     strih→stream); hops with no samples (e.g. cam1→strih absolute, needs #111) are
     labelled RELATIVE/UNAVAILABLE rather than drawn as zero.

Then runs `python3 ~/devel/airuleset/airuleset.py share <png>` and prints the LAN URL.
"""
import matplotlib
matplotlib.use("Agg")  # headless, before pyplot

import argparse
import json
import os
import subprocess
import sys

import matplotlib.pyplot as plt


# The hops, in signal-flow order, with (json_key, label, strict?).
HOPS = [
    ("cam2_cam1", "cam2→cam1\n(optical+grab)", False),
    ("cam1_strih", "cam1→strih\n(STRICT)", True),
    ("strih_stream", "strih→stream\n(STRICT)", True),
]
# Latency hops in flow order, with (json_key, label). #174: cam1→strih + strih→stream
# come from the CLEAN co-located burn-id pairing (overlaid above) when the burns are
# present; cam1→stream is the end-to-end burn-id latency.
LAT_HOPS = [
    # #175 PART 2: cam2→cam1 is the TEST-INJECTION hop (cam2 monitor → cam1 camera optical
    # + v4l2 capture + grab), NOT a production camera latency — labelled honestly on the bar.
    ("cam2_cam1", "cam2→cam1\n(TEST-INJECTION:\nmonitor→cam)"),
    ("cam1_strih", "cam1→strih"),
    ("strih_stream", "strih→stream"),
    ("cam1_stream", "cam1→stream\n(end-to-end)"),
]


def _share(png):
    """Share the PNG via the airuleset file-drop and print the LAN URL."""
    airuleset = os.path.expanduser("~/devel/airuleset/airuleset.py")
    try:
        out = subprocess.run(
            ["python3", airuleset, "share", png],
            capture_output=True, text=True, timeout=30, check=True,
        )
        url = out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""
        if url:
            print(url)
        else:
            print(f"(share produced no URL; PNG at {png})", file=sys.stderr)
    except Exception as e:  # never fail the run on a share hiccup
        print(f"WARNING: share failed ({e}); PNG at {png}", file=sys.stderr)


def main():
    p = argparse.ArgumentParser(description="Recording-based 4-node E2E report")
    p.add_argument("--json", required=True, help="recording-verdict --json summary")
    p.add_argument("--out", default="/tmp/recording-e2e-report.png")
    a = p.parse_args()

    with open(a.json) as f:
        r = json.load(f)
    # #174: prefer the CLEAN burn-id full-chain hops (from the stream recording alone,
    # paired on the digital burn id) when present — they replace the fragile optical-tick
    # hops that produced the 259-vs-1 loss artifact. Fall back to the optical hops only
    # when the burns were not in the recording.
    full_chain = r.get("full_chain", {})
    fc_loss = full_chain.get("loss", {})
    fc_lat = full_chain.get("latency", {})
    hops = dict(r.get("hops", {}))
    lat = dict(r.get("latency", {}))
    # Overlay the burn-id hops over the optical ones under the same keys the panels use.
    for k in ("cam1_strih", "strih_stream"):
        if k in fc_loss:
            hk = dict(fc_loss[k])
            hk["strict"] = True
            hk["burn_id"] = True
            hops[k] = hk
        if k in fc_lat and fc_lat[k] is not None:
            lat[k] = fc_lat[k]
    if fc_lat.get("cam1_stream") is not None:
        lat["cam1_stream"] = fc_lat["cam1_stream"]
    nodes = r.get("nodes", {})
    overall = r.get("overall_pass")

    fig, (ax_loss, ax_lat) = plt.subplots(2, 1, figsize=(12, 11))
    title_pass = "PASS" if overall else "FAIL"
    title_col = "#1a7f37" if overall else "#cf222e"
    fig.suptitle(
        f"Recording-based full-path E2E (#105/#7) — OVERALL: {title_pass}   "
        f"[nodes: cam2 paint · cam1 grab · strih PGM · stream PGM]",
        fontsize=14, fontweight="bold", color=title_col,
    )

    # --- Panel 1: per-hop loss ---
    labels, dropped, phantom, colors, verdicts = [], [], [], [], []
    for key, label, strict in HOPS:
        h = hops.get(key)
        if h is None:
            continue
        labels.append(label)
        d = int(h.get("dropped", h.get("unknown_ticks", 0)) or 0)
        ph = int(h.get("phantom", h.get("out_of_painter_range_ticks", 0)) or 0)
        dropped.append(d)
        phantom.append(ph)
        if strict:
            passed = bool(h.get("pass"))
            colors.append("#1a7f37" if passed else "#cf222e")
            verdicts.append("PASS" if passed else "FAIL")
        else:
            # optical hop: honest assessment — unknown (in-range never-painted) is the
            # provable fault; out-of-range is uncertain (shown amber).
            unk = int(h.get("unknown_ticks", 0) or 0)
            colors.append("#bf8700" if unk == 0 else "#cf222e")
            verdicts.append("assess" if unk == 0 else "FAULT")

    x = range(len(labels))
    if labels:
        b1 = ax_loss.bar([i - 0.2 for i in x], dropped, width=0.38,
                         label="dropped / unknown", color=colors)
        ax_loss.bar([i + 0.2 for i in x], phantom, width=0.38,
                    label="phantom / out-of-range", color="#8250df", alpha=0.7)
        ax_loss.set_xticks(list(x))
        ax_loss.set_xticklabels(labels, fontsize=10)
        ymax = max([1] + dropped + phantom)
        ax_loss.set_ylim(0, ymax * 1.45)  # headroom so the verdict text never collides
        for i, (d, ph, v) in enumerate(zip(dropped, phantom, verdicts)):
            ax_loss.text(i, max(d, ph) + ymax * 0.05, f"{v}\nlost={d} phantom={ph}",
                         ha="center", fontsize=9, fontweight="bold")
        ax_loss.set_ylabel("frames", fontsize=11)
        ax_loss.legend(loc="upper right", fontsize=9)
    ax_loss.set_title(
        "Per-hop LOSS — burn-id contiguity (#186/#198): cam1 from the CLEAN 1080p strih "
        "recording (#133), strih/stream from the stream recording; cam2→cam1 = honest optical",
        fontsize=11)
    ax_loss.grid(axis="y", alpha=0.3)

    # --- Panel 2: per-hop latency (p50/p95/p99) ---
    llabels, p50s, p95s, p99s, missing = [], [], [], [], []
    for key, label in LAT_HOPS:
        h = lat.get(key)
        llabels.append(label)
        if h is None:
            p50s.append(0); p95s.append(0); p99s.append(0); missing.append(True)
        else:
            p50s.append(float(h.get("p50_ms", 0)))
            p95s.append(float(h.get("p95_ms", 0)))
            p99s.append(float(h.get("p99_ms", 0)))
            missing.append(False)
    # A hop present but with 0 samples (no burn paired) is ALSO unavailable, not "0 ms".
    for i, (key, _label) in enumerate(LAT_HOPS):
        h = lat.get(key)
        if h is not None and int(h.get("samples", 0) or 0) == 0:
            p50s[i] = p95s[i] = p99s[i] = 0
            missing[i] = True
    lx = range(len(llabels))
    ax_lat.bar([i - 0.25 for i in lx], p50s, width=0.25, label="p50", color="#0969da")
    ax_lat.bar([i for i in lx], p95s, width=0.25, label="p95", color="#1f883d")
    ax_lat.bar([i + 0.25 for i in lx], p99s, width=0.25, label="p99", color="#bc4c00")
    ax_lat.set_xticks(list(lx))
    ax_lat.set_xticklabels(llabels, fontsize=10)
    for i, (h, miss) in enumerate(zip([lat.get(k) for k, _ in LAT_HOPS], missing)):
        if miss or h is None:
            ax_lat.text(i, 0.5, "UNAVAILABLE\n(no burn in\nrecording)",
                        ha="center", fontsize=8, color="#57606a", fontweight="bold")
        else:
            ax_lat.text(i, float(h.get("p99_ms", 0)) + 0.5,
                        f"p99={h.get('p99_ms', 0):.1f}ms\njitter={h.get('jitter_ms', 0):.1f} "
                        f"n={h.get('samples', 0)}", ha="center", fontsize=8)
    ax_lat.set_ylabel("latency (ms)", fontsize=11)
    ax_lat.set_title(
        "Per-hop LATENCY (p50/p95/p99) — cam1→strih, strih→stream & cam1→stream from the "
        "co-located burn stamps in the stream recording (#174, no cam2-tick pairing outliers)",
        fontsize=11)
    ax_lat.legend(loc="upper left", fontsize=9)
    ax_lat.grid(axis="y", alpha=0.3)

    # Footer: per-node analyzed span (the ≥300 s gate).
    spans = []
    for n in ("cam1", "strih", "stream"):
        nd = nodes.get(n)
        if nd:
            spans.append(f"{n}={nd.get('analyzed_secs', 0):.0f}s")
    fig.text(0.5, 0.005, "analyzed span: " + "  ".join(spans) +
             f"   (≥{r.get('min_secs', 300):.0f}s gate)",
             ha="center", fontsize=9, color="#57606a")

    fig.tight_layout(rect=[0, 0.02, 1, 0.96])
    fig.savefig(a.out, dpi=110)
    print(f"report PNG: {a.out}", file=sys.stderr)
    _share(a.out)


if __name__ == "__main__":
    main()
