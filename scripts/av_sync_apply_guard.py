#!/usr/bin/env python3
"""#1265 task 3 — the PURE refusal predicate that protects the #856 rig-wide A/V controller from
walking the prod pin when THIS run's audio timeline was unstable.

WHY: `recording-e2e.sh`'s #856 step computes a rig-wide A/V correction
(`av_sync_combine_offsets.py`, the median of this run's `verdict=="measured"` cameras) and applies
it to `NDI 2ME PGM`'s genlock latency in `cleanup()` (`av_sync_calibrate.py --apply`). Its only
guards are <2 measured cams / >100 ms spread. On 2026-09-01 the stream box's `mbc` audio timeline
went bimodal (107↔180 ms), which shifted the measured A/V residual to -77/-126 ms (past the ±90
gate) with a rig-wide-CONSISTENT (small-spread) shape — so both existing guards passed and the
controller walked the pin 926->976 toward noise. This predicate HOLDs the apply on any of three
independent, fail-safe signals, so the controller composes with a live restart instead of chasing a
flapping timeline. It never runs OBS/ssh/network — the sourced `scripts/lib/av-sync-apply-guard.sh`
does the I/O gather (verdict residual, last-applied offset, the stream band verdict) and calls this.

The three HOLD conditions (checked in order; the FIRST match wins the reason):
  1. band DRIFTING   — the run's stream reference-source (mbc) ts_lag band verdict (task 2,
                       gathered from :8899 at [8/8g]) is DRIFTING: the A/V measurement is corrupted
                       by the flapping audio timeline. The ROOT-CAUSE signal.
  2. residual ceiling — |residual_median_ms| exceeds a sanity ceiling (default 60 ms). The green
                       series measured within ±33 ms; the two failed runs measured -77/-126 ms, so
                       this cleanly separates "a real small drift to correct" from "an anomalous
                       off-baseline measurement to hold", with NO history needed. Works even before
                       the box band facet is deployed (band verdict UNKNOWN).
  3. jump vs last     — |proposed_offset_ms - last_applied_offset_ms| exceeds a jump threshold
                       (default 90 ms) vs the last applied value (~/.camera-box/av-sync-last.json):
                       the shared-path offset shifted a lot since the last calibration, not a steady
                       drift. Dormant until the reference file has been populated (last-applied None).

`residual_spread_ms` is accepted + surfaced (the combiner already hard-refuses a >100 ms spread, so
it is not re-gated here — it is included in the HOLD reason context only). Every numeric input is
parsed fail-safe: an unparseable value SKIPS its own condition (never a crash, never a false hold).

Usage:
    av_sync_apply_guard.py decide --residual-median-ms X --residual-spread-ms Y \\
        --band-verdict Z --last-applied-offset-ms W --proposed-offset-ms P \\
        [--jump-threshold-ms 90] [--residual-ceiling-ms 60]
Prints exactly one `hold_reason=<...>` line (empty when the apply should PROCEED) and ALWAYS exits 0
(a guard that crashed must never take down the E2E cleanup trap around it).
"""
import argparse
import sys

# The green A/V series measured residuals within ±33 ms; the two failed runs measured -77/-126 ms.
# 60 ms sits comfortably above the green band and below both failures, so it separates a real small
# drift (correct) from an anomalous off-baseline measurement (hold) with no history.
DEFAULT_RESIDUAL_CEILING_MS = 60.0
# A proposed correction this far from the last-applied value is a SHIFT, not a steady drift the
# controller should track. 90 ms mirrors the ±90 A/V gate tolerance.
DEFAULT_JUMP_THRESHOLD_MS = 90.0


def _num(x):
    """`x` (str/float/int/None) -> float, or None for None/empty/unparseable. Fail-safe: the caller
    SKIPS a condition whose input is None, so a garbage value never crashes and never false-holds."""
    if x is None:
        return None
    s = str(x).strip()
    if s == "":
        return None
    try:
        return float(s)
    except (ValueError, TypeError):
        return None


def hold_reason(residual_median_ms, residual_spread_ms, band_verdict,
                last_applied_offset_ms, proposed_offset_ms,
                jump_threshold_ms=DEFAULT_JUMP_THRESHOLD_MS,
                residual_ceiling_ms=DEFAULT_RESIDUAL_CEILING_MS):
    """Return a non-empty HOLD reason (the apply should be HELD) or "" (proceed). See module doc for
    the three conditions; the FIRST matching one wins."""
    resid = _num(residual_median_ms)
    spread = _num(residual_spread_ms)
    last = _num(last_applied_offset_ms)
    proposed = _num(proposed_offset_ms)
    bv = (band_verdict or "").strip().upper()
    spread_ctx = f", spread {spread:.1f}ms" if spread is not None else ""

    # (1) the run's audio timeline was DRIFTING -> the A/V measurement is corrupted.
    if bv == "DRIFTING":
        return (
            "stream mbc ts_lag band DRIFTING during the run -- the A/V measurement is corrupted by "
            f"the flapping audio timeline (issue 1265){spread_ctx}; not walking the prod pin from it"
        )

    # (2) the residual is beyond the sanity ceiling (jumped vs the green series).
    if resid is not None and abs(resid) > residual_ceiling_ms:
        return (
            f"run residual median {resid:.1f}ms exceeds the +/-{residual_ceiling_ms:.0f}ms sanity "
            f"band (green series was within +/-33ms){spread_ctx} -- likely an unstable-timeline "
            "measurement, not a real drift to chase"
        )

    # (3) the proposed correction jumps far from the last-applied value.
    if last is not None and proposed is not None and abs(proposed - last) > jump_threshold_ms:
        return (
            f"proposed correction {proposed:.1f}ms jumps {abs(proposed - last):.1f}ms from the last "
            f"applied {last:.1f}ms (> {jump_threshold_ms:.0f}ms) -- the shared-path offset shifted, "
            "not a steady drift"
        )

    return ""


def _main(argv):
    ap = argparse.ArgumentParser(description="#1265 #856 apply stability guard")
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("decide", help="print hold_reason=<...> (empty = proceed); always exits 0")
    d.add_argument("--residual-median-ms", default="")
    d.add_argument("--residual-spread-ms", default="")
    d.add_argument("--band-verdict", default="")
    d.add_argument("--last-applied-offset-ms", default="")
    d.add_argument("--proposed-offset-ms", default="")
    d.add_argument("--jump-threshold-ms", type=float, default=DEFAULT_JUMP_THRESHOLD_MS)
    d.add_argument("--residual-ceiling-ms", type=float, default=DEFAULT_RESIDUAL_CEILING_MS)
    ns = ap.parse_args(argv)

    if ns.cmd == "decide":
        try:
            reason = hold_reason(
                ns.residual_median_ms, ns.residual_spread_ms, ns.band_verdict,
                ns.last_applied_offset_ms, ns.proposed_offset_ms,
                ns.jump_threshold_ms, ns.residual_ceiling_ms,
            )
        except Exception:  # noqa: BLE001 - a guard must NEVER crash the E2E cleanup trap around it
            reason = ""
        print(f"hold_reason={reason}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
