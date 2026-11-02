#!/usr/bin/env python3
"""Issue 1387: grade one background-load bench from its OBS log excerpts.

Pure and offline. `scripts/background-load-bench.sh` runs three phases on one Linux OBS box and saves
the `program-render-audit:` lines each phase added to the OBS log:

  baseline  no load: proves the box is quiet, so a lagged frame below belongs to the load
  inside    CPU load on every CPU, inside rig-background.slice
  outside   the same load outside the slice, at nice 19 + ionice idle (how the backup ran)

PASS needs a quiet baseline, a load that bites outside the slice and zero lagged frames inside it.
A load that never bites proves nothing, so it is UNKNOWN and never a pass. So is a phase with too few
audit lines (the audit line comes every ~5 s) and a load not proven to have run where it should: the
remote `timeout` must end it with rc 124, and a read during the phase must count its sha256sum processes
as all inside the slice (inside) or none (outside). OBS's own CPU mask moving during the run is a FAIL:
the cpuset controller must never widen OBS. A PASS is CPU immunity: the load touches no disk.

Usage: background_load_decision.py --baseline F --inside F --outside F
           [--min-windows N] [--mask-before LIST] [--mask-after LIST]
           --inside-load RC:PROCS:IN_SLICE --outside-load RC:PROCS:IN_SLICE
Prints one JSON object. Exit: 0 PASS, 1 FAIL, 2 UNKNOWN.
"""
import argparse
import json
import re
import sys

AUDIT_RE = re.compile(
    r"program-render-audit: render_fps=(?P<fps>[0-9.]+) target_fps=[0-9.]+ "
    r"avg_frame_ms=(?P<ms>[0-9.]+) lagged=(?P<lagged>\d+) total=\d+"
)


def summarize(text):
    """One phase's windows, summed lagged frames, lowest render fps and highest avg frame time."""
    windows = lagged = 0
    fps = []
    ms = []
    for match in AUDIT_RE.finditer(text):
        windows += 1
        lagged += int(match["lagged"])
        fps.append(float(match["fps"]))
        ms.append(float(match["ms"]))
    return {
        "windows": windows,
        "lagged": lagged,
        "min_render_fps": min(fps) if fps else None,
        "max_avg_frame_ms": max(ms) if ms else None,
    }


def load_problem(name, proof, want_in_slice):
    """Why a phase's `rc:procs:in_slice` proof fails, or None. An unparsable proof is a problem."""
    try:
        rc, procs, in_slice = (int(part) for part in proof.split(":"))
    except ValueError:
        return f"{name}: unreadable load proof {proof!r}"
    if rc != 124:
        return f"{name}: the load did not run to its timeout (rc {rc})"
    if procs < 1:
        return f"{name}: no load process seen during the phase"
    if in_slice != (procs if want_in_slice else 0):
        return f"{name}: {in_slice} of {procs} load processes in the slice"
    return None


def grade(baseline, inside, outside, min_windows, mask_before="", mask_after="", inside_load="", outside_load=""):
    """(verdict, reason) over the three phase summaries and the two load proofs."""
    if mask_before != mask_after:
        return "FAIL", f"the OBS CPU mask moved during the run: {mask_before!r} -> {mask_after!r}"
    for name, phase in (("baseline", baseline), ("inside", inside), ("outside", outside)):
        if phase["windows"] < min_windows:
            return "UNKNOWN", f"{name}: {phase['windows']} audit lines, need {min_windows}"
    for problem in (load_problem("inside", inside_load, True), load_problem("outside", outside_load, False)):
        if problem:
            return "UNKNOWN", problem
    if baseline["lagged"] > 0:
        return "UNKNOWN", f"the box was not quiet before the load: baseline lagged={baseline['lagged']}"
    if inside["lagged"] > 0:
        return "FAIL", f"load inside the slice lagged the render: {inside['lagged']} frames"
    if outside["lagged"] == 0:
        return "UNKNOWN", "the load outside the slice lagged nothing, so this run proves nothing"
    return "PASS", f"CPU load: lagged inside 0, outside {outside['lagged']}"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--inside", required=True)
    parser.add_argument("--outside", required=True)
    parser.add_argument("--min-windows", type=int, default=6)
    parser.add_argument("--mask-before", default="")
    parser.add_argument("--mask-after", default="")
    parser.add_argument("--inside-load", default="")
    parser.add_argument("--outside-load", default="")
    args = parser.parse_args(argv)
    phases = {}
    for name in ("baseline", "inside", "outside"):
        with open(getattr(args, name), encoding="utf-8", errors="replace") as handle:
            phases[name] = summarize(handle.read())
    verdict, reason = grade(
        phases["baseline"],
        phases["inside"],
        phases["outside"],
        args.min_windows,
        args.mask_before,
        args.mask_after,
        args.inside_load,
        args.outside_load,
    )
    print(json.dumps({"verdict": verdict, "reason": reason, **phases}))
    return {"PASS": 0, "FAIL": 1}.get(verdict, 2)


if __name__ == "__main__":
    sys.exit(main())
