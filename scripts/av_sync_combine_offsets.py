#!/usr/bin/env python3
"""#856 -- combine `all_cambox_av_sync`'s per-camera MEASURED offsets into ONE rig-wide A/V
correction to feed `av_sync_calibrate.py --apply` (see that script's own docstring for the OBS
apply + read-back/rollback safety this reuses UNCHANGED -- this script only decides WHAT number
to apply; it never touches OBS itself, and never runs any network call).

## Why a rig-wide constant, not four independent numbers

`all_cambox_av_sync` reports one offset PER CAMERA, but the correction lands on ONE SHARED
stream-side knob (`NDI 2ME PGM`'s `genlock_latency_ms_src`). The 2026-07-28 fused-run
measurement (issue #856) showed all four live cameras clustering within a ~28ms band around one
constant (-297.56..-269.35ms) -- one shared-path offset, not per-camera noise -- so this module
derives ONE number: the **median** across cameras whose verdict is EXACTLY "measured" (robust to
one outlier camera, unlike a mean).

## Fail-closed guards (recorded design decision -- posted to the #856 issue before this code)

A correction computed from a degenerate measurement is worse than none, since this feeds a
hardware-changing apply on the live rig:

- `MIN_MEASURED_CAMS`: fewer measured cameras than this and there is nothing to corroborate a
  shared rig-wide constant against -- refuse.
- `MAX_SPREAD_MS`: measured offsets spanning more than this many ms no longer look like ONE
  constant (per-camera noise, a mid-run glitch, a partial re-genlock) -- refuse rather than
  paper over disagreement with a median that hides it.

`verdict=="excluded"` (an operator-acked offline box, #855) and `verdict=="derived"`/`"unknown"`
(#714's cam2-anchored estimate for a sample-starved camera, or no measurement at all) NEVER
contribute -- only a camera's OWN independently measured offset counts.

**Rejected alternative:** feed every camera's `effective_offset_ms` (which folds in #714's
DERIVED estimates) instead of restricting to `"measured"`. A derived estimate is already built
FROM cam2's own measured offset plus a delivery-latency delta -- feeding it back into a
rig-wide correction would let one camera's real measurement count twice (once directly, once
through every derivation built on it), and would apply a correction partly built on an
ASSUMPTION rather than an independent reading. Restricting to genuinely independent "measured"
entries keeps the correction traceable to real audio/video decodes only.

Usage:
    av_sync_combine_offsets.py --verdict-json <recording-verdict JSON with all_cambox_av_sync>

Prints the combined offset (ms, e.g. "-283.44") to stdout and exits 0 on success. On refusal
(missing all_cambox_av_sync, too few measured cameras, or the measured offsets don't look like
ONE rig-wide constant) prints the reason(s) to stderr and exits 2 -- NEVER prints a guessed
number.
"""
import argparse
import json
import sys

# Fewer measured cameras than this and there's nothing to corroborate a shared rig-wide
# constant against -- refuse rather than apply a correction derived from a single reading.
MIN_MEASURED_CAMS = 2

# Measured offsets spanning more than this many ms no longer look like ONE rig-wide constant
# (see module doc). Chosen well above the ~28ms band the #856 issue's own healthy measurement
# showed (so a normal run's natural per-camera spread never trips it) and well below the
# ~200-300ms restart-drift magnitude src/av_restart_sync.rs (#137) already treats as a real
# problem (so a genuinely broken/inconsistent measurement IS caught).
MAX_SPREAD_MS = 100.0


def median(values):
    """Plain sorted-median; no numpy dependency needed for this small a list."""
    s = sorted(values)
    n = len(s)
    mid = n // 2
    if n % 2:
        return s[mid]
    return (s[mid - 1] + s[mid]) / 2.0


def measured_offsets(all_cambox_av_sync):
    """Extract [(camera, offset_ms), ...] for every entry whose verdict=="measured" AND
    av_offset_ms is a real number. Skips "derived"/"unknown"/"excluded" cameras (see the
    module doc's rejected-alternative note) and the block's own meta keys
    (expected_ms/gate_tolerance_ms/gate_pass/gate), which are not dicts."""
    out = []
    for cam, entry in all_cambox_av_sync.items():
        if not isinstance(entry, dict):
            continue
        if entry.get("verdict") != "measured":
            continue
        off = entry.get("av_offset_ms")
        if isinstance(off, (int, float)):
            out.append((cam, float(off)))
    return out


def combine(all_cambox_av_sync, min_cams=MIN_MEASURED_CAMS, max_spread_ms=MAX_SPREAD_MS):
    """The real #856 decision. Returns (offset_ms, cams_used) on success, or (None, reasons)
    on refusal -- see the module doc for the two fail-closed guards."""
    pairs = measured_offsets(all_cambox_av_sync)
    if len(pairs) < min_cams:
        return None, [
            f"only {len(pairs)} camera(s) reached verdict==\"measured\" this run "
            f"(need >= {min_cams} to corroborate a shared rig-wide constant)"
        ]
    offsets = [o for _, o in pairs]
    spread = max(offsets) - min(offsets)
    if spread > max_spread_ms:
        return None, [
            f"measured offsets spread {spread:.1f}ms (> {max_spread_ms:.0f}ms) -- doesn't look "
            "like one rig-wide constant, refusing to apply"
        ]
    cams_used = sorted(cam for cam, _ in pairs)
    return median(offsets), cams_used


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--verdict-json", required=True)
    ap.add_argument("--min-cams", type=int, default=MIN_MEASURED_CAMS)
    ap.add_argument("--max-spread-ms", type=float, default=MAX_SPREAD_MS)
    args = ap.parse_args()

    with open(args.verdict_json) as f:
        verdict = json.load(f)

    av_sync = verdict.get("all_cambox_av_sync")
    if not isinstance(av_sync, dict):
        sys.stderr.write(
            f"[av-sync-combine] {args.verdict_json}: no 'all_cambox_av_sync' object -- "
            "nothing to combine\n"
        )
        sys.exit(2)

    offset, info = combine(av_sync, args.min_cams, args.max_spread_ms)
    if offset is None:
        for reason in info:
            sys.stderr.write(f"[av-sync-combine] refusing: {reason}\n")
        sys.exit(2)

    sys.stderr.write(f"[av-sync-combine] combined offset={offset:.2f}ms from cameras {info}\n")
    print(f"{offset:.2f}")


if __name__ == "__main__":
    main()
