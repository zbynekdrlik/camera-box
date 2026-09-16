#!/usr/bin/env python3
"""issue 1242 -- per-window copies/gaps + cadence-uniformity walk-down mining tool.

WHAT: run over a set of FINISHED E2E run directories (`/tmp/recording-e2e-<RUN_ID>/`) and print
ONE distribution table -- per run, per camera -- of the two blocking continuity signals this
ticket walks back: the per-window `copies`/`gaps` (the residual FIFO churn) and the per-window
`beat_corrected_uniform_fraction` (the issue-1142 cadence-uniformity floor, the BEAT-AWARE field
the gate actually reads since #1250, NOT `derived_uniform_fraction`). Runs are SEGREGATED by the
rig-verified genlock bundle SHA (`version-strih.json.genlock_build_sha`) into PRE-fix vs POST-fix
of the issue-1318/1320 render-freeze cure (bundle `02b53180b`), so the walk-down decision is made
on the data the cured rig produced, not on prose timestamps.

Pure decision core (no I/O below the CLI, no ssh, no OBS, no rig) -- fixture-driven under Tier-0
#557, the arrival_floor_decompose.py / audio_lag_decision.py python-mirror precedent. Wired into NO
gate and drives NO rig: a SUPERVISOR mining instrument that reproduces the ticket's table.
"""
import glob
import json
import os
import sys

# The issue-1318/1320 render-freeze cure landed as this genlock bundle (camera-box commit
# `style(#1320): ...`, 15.9 21:02, deployed ~21:36). A run is POST-fix iff its rig-verified
# genlock_build_sha is in the post set (this bundle or any later one the caller lists).
FIX_BUNDLE = "02b53180b"


def summarize_verdict(verdict):
    """Pure: the per-run + per-camera continuity summary from a parsed verdict JSON dict.

    Returns a dict with the run-level counts (`wcg`, `w_over_tol`, `w_fail_strict`, `undec`), the
    worst (min) per-window uniformity on BOTH the gated beat-corrected field and the diagnostic
    derived field, and `per_cam` = {CAMx: [(copies, gaps, beat_uniform), ...]} for every window."""
    c = verdict.get("all_cambox_continuity", {}) or {}
    cu = c.get("cadence_uniformity_gate", {}) or {}
    per_cam = {}
    for seg in c.get("segments", []) or []:
        pc = seg.get("presentation_cadence") or {}
        beat = pc.get("beat_corrected_uniform_fraction")
        per_cam.setdefault(seg.get("cambox", "?"), []).append(
            (int(seg.get("copies", 0)), int(seg.get("gaps", 0)), beat)
        )
    return {
        "overall_pass": verdict.get("overall_pass"),
        "wcg": c.get("windows_with_copies_or_gaps"),
        "w_over_tol": c.get("windows_over_copies_gaps_tolerance"),
        "w_fail_strict": c.get("windows_failed_report_only"),
        "undec": c.get("total_undecodable"),
        "worst_beat_unif": cu.get("worst_uniform_fraction"),
        "worst_derived_unif": cu.get("worst_derived_uniform_fraction"),
        "per_cam": per_cam,
    }


def classify_era(genlock_sha, post_bundles):
    """Pure: 'POST' iff `genlock_sha` is one of the cure-or-later bundles, else 'PRE'."""
    return "POST" if genlock_sha in post_bundles else "PRE"


def nonzero_windows(summary):
    """Pure: [(cam, copies, gaps, beat)] for every window with copies>0 or gaps>0."""
    out = []
    for cam, wins in sorted(summary["per_cam"].items()):
        for copies, gaps, beat in wins:
            if copies > 0 or gaps > 0:
                out.append((cam, copies, gaps, beat))
    return out


def distribution_table(rows):
    """Pure: a markdown table from [(run, genlock_sha, era, summary)] rows."""
    hdr = ("| run | genlock | era | wcg | w>tol | w_fail_strict | undec | worst BEAT-unif "
           "| nonzero windows |")
    sep = "|---|---|---|---|---|---|---|---|---|"
    lines = [hdr, sep]
    for run, sha, era, s in rows:
        nz = "; ".join(f"{c} {cp}/{gp}" for c, cp, gp, _ in nonzero_windows(s)) or "all 0/0"
        wb = "" if s["worst_beat_unif"] is None else f"{s['worst_beat_unif']:.4f}"
        lines.append(
            f"| {run} | {sha[:9]} | {era} | {s['wcg']} | {s['w_over_tol']} "
            f"| {s['w_fail_strict']} | {s['undec']} | {wb} | {nz} |"
        )
    return "\n".join(lines)


def _genlock_sha(run_dir):
    for vf in ("version-strih.json", "version-stream.json"):
        p = os.path.join(run_dir, vf)
        if os.path.exists(p):
            try:
                with open(p) as fh:
                    return json.load(fh).get("genlock_build_sha", "?")
            except (OSError, ValueError):
                pass
    return "?"


def main(argv):
    post = set(argv[1:]) or {FIX_BUNDLE}
    dirs = sorted(glob.glob("/tmp/recording-e2e-*/"), key=os.path.getmtime)
    rows = []
    for d in dirs:
        vjs = glob.glob(os.path.join(d, "verdict-*.json"))
        if not vjs:
            continue
        try:
            with open(vjs[0]) as fh:
                v = json.load(fh)
        except (OSError, ValueError):
            continue
        run = os.path.basename(vjs[0])[len("verdict-"):-len(".json")]
        sha = _genlock_sha(d)
        rows.append((run, sha, classify_era(sha, post), summarize_verdict(v)))
    print(distribution_table(rows))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
