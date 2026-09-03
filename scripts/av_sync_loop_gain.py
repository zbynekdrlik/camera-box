#!/usr/bin/env python3
"""#1265 -- the FIXED loop-gain damping term for the #856 rig-wide A/V controller.

## Why (the divergent oscillation)

`recording-e2e.sh`'s #856 controller auto-tunes `NDI 2ME PGM`'s genlock latency toward
`-residual` (the median measured A/V offset). The measured PLANT gain is ~2.31 ms of residual per
ms of pin -- collinear across three consecutive green runs (925->960 slope 2.314, 960->913 slope
2.314) -- so applying `-residual` (loop gain 1) gives an EFFECTIVE loop gain of 2.31 > 1 and the
pin oscillates with GROWING amplitude (|33.6| -> |47.4| -> |61.4| ms). A control loop is stable
only when |effective loop gain| < 1.

## What this does

Damps the combined offset by a fixed gain (default 0.4) BEFORE the existing +/-50 ms/run clamp
(`av_sync_calibrate.py::required_delay_ms`) AND before the #1265 apply guard sees `proposed_offset`
(`av_sync_apply_guard.py`), so the effective loop gain is 0.4*2.31 = 0.92 < 1 -- it converges
(one step: 913 + 0.4*61.4 = 938 ~ the predicted null 940; even if the plant gain were really 1 it
converges at 0.6/run). Robust to the unknown-physics slope, one constant, no history dependence.

Env override `AV_SYNC_LOOP_GAIN` (a value in (0, 1]); a non-numeric / <=0 / >1 value falls back to
the default 0.4 with a LOUD stderr line (never a silent wrong gain).

This is a PURE module (no OBS/ssh/network, no file I/O) so it is fully Tier-0 testable off-rig.

Usage (the #856 controller path, [8/8g] of recording-e2e.sh):
    av_sync_loop_gain.py damp --combined-ms <combined_median_ms>
Resolves the gain from AV_SYNC_LOOP_GAIN, prints `<damped>\t<gain>` (tab-separated) on stdout for
the caller to split, the validation warning (if any) on stderr, and ALWAYS exits 0 -- it runs on
the path into the cleanup() EXIT trap, so it must never abort the run. An unparseable combined value
prints an EMPTY damped field so the caller's `[ -n ... ]` skips the apply (fail-safe: no correction
rather than a wrong one).
"""
import argparse
import os
import sys

# The fixed loop gain. 0.4 * the measured plant gain 2.31 = 0.92 < 1 (stable). See module doc.
DEFAULT_LOOP_GAIN = 0.4


def resolve_gain(env=None):
    """Return the loop gain to use: the AV_SYNC_LOOP_GAIN env value when it is a valid number in
    (0, 1], else DEFAULT_LOOP_GAIN. A non-numeric / out-of-range value prints a LOUD stderr line
    (never a silent wrong gain); unset/empty falls back silently (it is the intended default, not
    an override mistake)."""
    env = os.environ if env is None else env
    raw = env.get("AV_SYNC_LOOP_GAIN")
    if raw is None or str(raw).strip() == "":
        return DEFAULT_LOOP_GAIN
    try:
        gain = float(raw)
    except (ValueError, TypeError):
        sys.stderr.write(
            f"[av-sync] WARNING: AV_SYNC_LOOP_GAIN={raw!r} is not a number -- "
            f"using default loop gain {DEFAULT_LOOP_GAIN}\n"
        )
        return DEFAULT_LOOP_GAIN
    if not (0.0 < gain <= 1.0):
        sys.stderr.write(
            f"[av-sync] WARNING: AV_SYNC_LOOP_GAIN={gain} is out of range (0, 1] -- "
            f"using default loop gain {DEFAULT_LOOP_GAIN}\n"
        )
        return DEFAULT_LOOP_GAIN
    return gain


def damped_offset(combined_ms, gain):
    """The damped correction: combined_ms * gain (sign-preserving)."""
    return combined_ms * gain


def _main(argv):
    ap = argparse.ArgumentParser(description="#1265 #856 loop-gain damping")
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("damp", help="print <damped>\\t<gain> (empty damped on a bad input); exits 0")
    d.add_argument("--combined-ms", default="", help="the combined (raw median) A/V offset in ms")
    ns = ap.parse_args(argv)

    if ns.cmd == "damp":
        gain = resolve_gain(os.environ)
        try:
            combined = float(str(ns.combined_ms).strip())
        except (ValueError, TypeError):
            # fail-safe: no parseable combined -> empty damped so the caller skips the apply; never
            # abort the run (this feeds the cleanup EXIT trap path).
            sys.stderr.write(
                f"[av-sync] WARNING: loop-gain damp got a non-numeric combined offset "
                f"{ns.combined_ms!r} -- skipping (no correction)\n"
            )
            print(f"\t{gain:.4f}")
            return 0
        damped = damped_offset(combined, gain)
        sys.stderr.write(
            f"[av-sync] loop gain {gain:.2f} damps combined {combined:.2f}ms -> {damped:.4f}ms\n"
        )
        print(f"{damped:.4f}\t{gain:.4f}")
        return 0
    return 2


def main(argv=None):
    return _main(sys.argv[1:] if argv is None else argv)


if __name__ == "__main__":
    sys.exit(main())
