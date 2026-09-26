#!/usr/bin/env python3
"""Regenerate the issue-1367 Discord-report fixture: the REAL run-2059624745 verdict re-folded exactly as
recording-verdict.rs folds it after the multi-source decision (the same keys and entry shapes the
Rust `json!` bodies emit). Nothing else in the verdict is touched.

Usage: regen_multi_source_1367.py /tmp/recording-e2e-2059624745/verdict-2059624745.json \
    tests/python/fixtures/e2e_discord_report/verdict_multi_source_pass_2059624745_1367.json
Keep the shapes in step with the `json!` bodies in src/bin/recording-verdict.rs (grep
`multi_source_report_only`) and `MultiSourceTag` in src/multi_source_window.rs."""
import json
import sys

TAG = "multi-source (report-only by #1367 decision)"
CEIL = 0.1

if len(sys.argv) != 3:
    sys.exit("usage: regen_multi_source_1367.py <real verdict.json> <out fixture.json>")
v = json.load(open(sys.argv[1]))
ac = v["all_cambox_continuity"]
fractions = [w["multi_path_suspect_fraction"] for w in ac["tear"]["windows"]]
multi = [f > CEIL for f in fractions]


def tag(f):
    return {
        "tag": TAG,
        "multi_path_suspect_fraction": f,
        "ceiling": CEIL,
        "report_only_checks": ["copies_gaps", "cadence", "frozen_leg"],
        "blocking_checks": ["node_burn_contiguity", "node_burn_hold"],
    }


segs = ac["segments"]
for s, f, m in zip(segs, fractions, multi):
    if m:
        s["multi_source"] = tag(f)
ac["windows_multi_source"] = sum(multi)
ac["windows_over_copies_gaps_tolerance"] = sum(
    1 for s, m in zip(segs, multi)
    if not m and (s["copies"] > s["copies_gaps_tolerance"] or s["gaps"] > s["copies_gaps_tolerance"])
)
ac["overall_pass"] = True


def worst(key, pick):
    vals = [s["presentation_cadence"][key] for s, m in zip(segs, multi)
            if not m and s.get("presentation_cadence")]
    return pick(vals) if vals else None


# recording-verdict.rs `cadence_multi_source` — ONE list attached to both cadence gates.
cadence_ms = [
    {
        "cambox": s["cambox"],
        "multi_path_suspect_fraction": s["multi_source"]["multi_path_suspect_fraction"],
        "paired_fraction": (s.get("presentation_cadence") or {}).get("paired_fraction"),
        "uniform_fraction": (s.get("presentation_cadence") or {}).get("beat_corrected_uniform_fraction"),
        "tag": TAG,
    }
    for s in segs if "multi_source" in s
]
cj = ac["cadence_judder_gate"]
cj["worst_paired_fraction"] = worst("paired_fraction", max)
cj["pass"] = cj["worst_paired_fraction"] <= cj["bound_paired_fraction"]
cj["multi_source_report_only"] = cadence_ms
cu = ac["cadence_uniformity_gate"]
cu["worst_uniform_fraction"] = worst("beat_corrected_uniform_fraction", min)
cu["worst_derived_uniform_fraction"] = worst("derived_uniform_fraction", min)
cu["worst_raw_uniform_fraction"] = worst("uniform_fraction", min)
cu["pass"] = cu["worst_uniform_fraction"] >= cu["min_uniform_fraction"]
cu["multi_source_report_only"] = cadence_ms

# recording-verdict.rs `dup_multi_source`; masked_windows counts gating windows only, the raw worst
# stays whole-run (unchanged).
dmc = ac["duplication_masked_cadence"]
dmc["multi_source_report_only"] = [
    {
        "cambox": s["cambox"],
        "multi_path_suspect_fraction": s["multi_source"]["multi_path_suspect_fraction"],
        "duplicate_fraction": (w["dup_cadence"] or {}).get("duplicate_fraction"),
        "duplication_masked": (w["dup_cadence"] or {}).get("duplication_masked"),
        "tag": TAG,
    }
    for w, s in zip(dmc["windows"], segs) if "multi_source" in s
]
dmc["masked_windows"] = sum(
    1 for w, m in zip(dmc["windows"], multi)
    if not m and (w["dup_cadence"] or {}).get("duplication_masked")
)

# frozen_leg: a multi-source window's hard-frozen entry moves to multi_source_report_only.
fl = v["frozen_leg"]
by_start = {(s["cambox"], s["start_ns"]): s for s in segs}
keep, ro = [], []
for e in fl["frozen"]:
    s = by_start.get((e["cambox"], e["since_ns"]))
    if s is not None and "multi_source" in s:
        ro.append(dict(e, multi_source=s["multi_source"]))
    else:
        keep.append(e)
fl["frozen"] = keep
fl["multi_source_report_only"] = ro

assert cj["pass"] and cu["pass"], (cj, cu)
v["overall_pass"] = True
json.dump(v, open(sys.argv[2], "w"), indent=1, sort_keys=True)
print("windows_multi_source", ac["windows_multi_source"], "frozen", len(keep), "ro", len(ro))
