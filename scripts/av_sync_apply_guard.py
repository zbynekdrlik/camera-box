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

IMPORTANT — the residual anomaly, NOT the ts_lag flap, is the PRIMARY gate (supervisor finding
2026-09-01): the mbc ts_lag flap is a real OBS audio-timeline HEALTH issue but does NOT by itself
explain the A/V residuals — a run AFTER the stream-OBS restart, with a FLAT ~85 ms ts_lag band, still
measured residual -111.5 ms (a real upstream-audio-latency STEP, oscillating, confirmed by the
av-sync dock). So the residual conditions gate on the RESIDUAL and are checked REGARDLESS of the band
verdict (including a HEALTHY/flat band); scoping them to a non-healthy band would let this real case
straight through.

CRITICAL — the residual conditions must NOT make a GENUINE SUSTAINED step un-appliable forever
(supervisor 2026-09-02): a real upstream step (2026-09-01: -77 -> -126 -> -111 across three runs,
agreeing within ~25 ms while the pin stayed at 926 — the #856 926->976 step was CORRECT) reads
|residual| > 60 AND jump > 90 on EVERY run, so a naive "hold on off-baseline" would leave the rig
~90 ms mis-aligned until a human hand-edits the pin — INVERTING the #856 contract (the gate aligns,
never the operator). The fix is a two-run SUSTAINED confirmation: an off-baseline residual HOLDs on
the FIRST anomalous run (outlier protection), but once the PREVIOUS run's persisted residual agrees
with this one within SUSTAINED_TOL_MS (a confirmed real step, not a one-run outlier or an
oscillation) the residual conditions STAND DOWN and the apply proceeds — the existing #856 ±50 ms/run
clamp still bounds each step, so a confirmed step converges over a few runs instead of never.

The HOLD conditions (checked in order; the FIRST match wins the reason):
  1. band DRIFTING   — the run's stream reference-source (mbc) ts_lag band verdict (task 2,
                       gathered from :8899 at [8/8g]) is DRIFTING: the audio timeline is UNSTABLE, so
                       defer tuning until it settles. A supplementary/conservative hold — NOT a claim
                       that the flap explains the residual (it does not; see above), and it HOLDs even
                       when the residual is sustained (never tune during a flapping timeline).
                       Absent/UNKNOWN/HEALTHY band never holds via this condition on its own.
  2. residual ceiling — |residual_median_ms| exceeds a sanity ceiling (default 60 ms), checked
                       REGARDLESS of the band verdict — BUT only HOLDs when the step is NOT yet
                       SUSTAINED (see below). The green series measured within ±33 ms; the failed runs
                       measured -77/-111/-126 ms. First anomalous run -> HOLD (outlier protection);
                       a 2nd consistent run -> PROCEED (let #856 apply, ±50-clamped).
  3. jump vs last     — |proposed_offset_ms - last_applied_offset_ms| exceeds a jump threshold
                       (default 90 ms) vs the last applied value (~/.camera-box/av-sync-last.json).
                       An anti-oscillation / step guard, ALSO gated by SUSTAINED: an abrupt swing
                       HOLDs the first run, a confirmed sustained step proceeds. Dormant until the
                       reference file has been populated (last-applied None).

SUSTAINED = the previous run's persisted residual exists, is <= PREV_MAX_AGE_S old (default 24 h),
and |residual_now - residual_prev| <= SUSTAINED_TOL_MS (default 33 ms = one 30 fps frame, the FIFO
1027/993 quantization quantum). When SUSTAINED, conditions 2 and 3 do NOT hold. Condition 1 (band
DRIFTING) is independent of SUSTAINED and always holds. NOT sustained (first off-baseline run, prev
disagrees > tol, or a stale/missing prev) -> HOLD "awaiting a 2nd consistent run" (outlier
protection). The caller persists EVERY run's residual (held OR applied) to
~/.camera-box/av-sync-residual-last.json so the NEXT run has the prev to compare against.

`residual_spread_ms` is accepted + surfaced (the combiner already hard-refuses a >100 ms spread, so
it is not re-gated here — it is included in the HOLD reason context only). Every numeric input is
parsed fail-safe: an unparseable value SKIPS its own condition (never a crash, never a false hold).

Usage:
    av_sync_apply_guard.py decide --residual-median-ms X --residual-spread-ms Y \\
        --band-verdict Z --last-applied-offset-ms W --proposed-offset-ms P \\
        [--prev-residual-ms R] [--prev-residual-age-s A] \\
        [--jump-threshold-ms 90] [--residual-ceiling-ms 60] \\
        [--sustained-tol-ms 33] [--prev-max-age-s 86400]
Prints exactly one `hold_reason=<...>` line (empty when the apply should PROCEED) and ALWAYS exits 0
(a guard that crashed must never take down the E2E cleanup trap around it).
"""
import argparse
import sys

# The green A/V series measured residuals within ±33 ms; the two failed runs measured -77/-126 ms.
# 60 ms sits comfortably above the green band and below both failures, so it separates a real small
# drift (correct) from an anomalous off-baseline measurement (hold) with no history.
DEFAULT_RESIDUAL_CEILING_MS = 60.0
# A combined offset this far from the last-applied value is an abrupt step/oscillation, not a steady
# drift the controller should track incrementally. 90 ms mirrors the ±90 A/V gate tolerance.
DEFAULT_JUMP_THRESHOLD_MS = 90.0
# Two consecutive runs whose residuals agree within this = a CONFIRMED sustained step (not a one-run
# outlier or an oscillation). 33 ms = one 30 fps frame, the FIFO 1027/993 quantization quantum.
DEFAULT_SUSTAINED_TOL_MS = 33.0
# A persisted previous residual older than this is stale -> not a valid confirmation basis (24 h).
DEFAULT_PREV_MAX_AGE_S = 86400.0


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
                prev_residual_ms=None, prev_residual_age_s=None,
                jump_threshold_ms=DEFAULT_JUMP_THRESHOLD_MS,
                residual_ceiling_ms=DEFAULT_RESIDUAL_CEILING_MS,
                sustained_tol_ms=DEFAULT_SUSTAINED_TOL_MS,
                prev_max_age_s=DEFAULT_PREV_MAX_AGE_S):
    """Return a non-empty HOLD reason (the apply should be HELD) or "" (proceed). See module doc for
    the conditions + the SUSTAINED two-run confirmation; the FIRST matching one wins."""
    resid = _num(residual_median_ms)
    spread = _num(residual_spread_ms)
    last = _num(last_applied_offset_ms)
    proposed = _num(proposed_offset_ms)
    prev = _num(prev_residual_ms)
    prev_age = _num(prev_residual_age_s)
    bv = (band_verdict or "").strip().upper()
    spread_ctx = f", spread {spread:.1f}ms" if spread is not None else ""

    # (1) the run's audio timeline was DRIFTING -> UNSTABLE, defer tuning (supplementary/conservative;
    # NOT a claim the flap explains the residual -- see module doc). Independent of SUSTAINED: never
    # tune during a flapping timeline, even a confirmed one.
    if bv == "DRIFTING":
        return (
            "stream mbc ts_lag band DRIFTING during the run -- audio timeline UNSTABLE, deferring the "
            f"tune until it settles (issue 1265){spread_ctx}; not walking the prod pin during a flap"
        )

    # SUSTAINED = the previous run's persisted residual exists, is fresh (<= prev_max_age_s), and
    # agrees with THIS run within sustained_tol_ms -> a CONFIRMED real step, not a one-run outlier or
    # an oscillation. When sustained, the off-baseline conditions (2, 3) STAND DOWN so a genuine step
    # is not un-appliable forever (supervisor 2026-09-02); the #856 ±50 ms/run clamp bounds each step.
    sustained = (
        prev is not None and prev_age is not None and prev_age <= prev_max_age_s
        and resid is not None and abs(resid - prev) <= sustained_tol_ms
    )
    # A human-readable "why not sustained yet" for the HOLD reason.
    if prev is None:
        prev_ctx = "no prior run recorded"
    elif prev_age is None or prev_age > prev_max_age_s:
        prev_ctx = f"prior run stale ({'?' if prev_age is None else f'{prev_age:.0f}s'} old)"
    elif resid is not None and abs(resid - prev) > sustained_tol_ms:
        prev_ctx = f"prior residual {prev:.1f}ms disagrees by {abs(resid - prev):.1f}ms > {sustained_tol_ms:.0f}ms"
    else:
        prev_ctx = "prior residual unavailable"

    off_ceiling = resid is not None and abs(resid) > residual_ceiling_ms
    off_jump = last is not None and proposed is not None and abs(proposed - last) > jump_threshold_ms

    if off_ceiling or off_jump:
        if sustained:
            # A 2nd consistent run CONFIRMS a real step -> let #856 apply it (the ±50 clamp bounds it).
            return ""
        if off_ceiling:
            # (2) residual beyond the sanity ceiling, not yet confirmed by a 2nd run -> HOLD (outlier
            # protection). Checked REGARDLESS of the band (a flat/HEALTHY band still measured -111.5ms,
            # a real upstream step) -- but a CONFIRMED step above proceeds.
            return (
                f"run residual median {resid:.1f}ms exceeds the +/-{residual_ceiling_ms:.0f}ms sanity "
                f"band (green series was within +/-33ms){spread_ctx} -- HOLDING, awaiting a 2nd "
                f"consistent run to confirm a real step vs an outlier ({prev_ctx})"
            )
        # (3) the combined offset SWUNG far from the last-applied value, not yet confirmed -> HOLD.
        return (
            f"proposed correction {proposed:.1f}ms swung {abs(proposed - last):.1f}ms from the last "
            f"applied {last:.1f}ms (> {jump_threshold_ms:.0f}ms) -- HOLDING, awaiting a 2nd consistent "
            f"run to confirm a real step vs an oscillation ({prev_ctx})"
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
    d.add_argument("--prev-residual-ms", default="")
    d.add_argument("--prev-residual-age-s", default="")
    d.add_argument("--jump-threshold-ms", type=float, default=DEFAULT_JUMP_THRESHOLD_MS)
    d.add_argument("--residual-ceiling-ms", type=float, default=DEFAULT_RESIDUAL_CEILING_MS)
    d.add_argument("--sustained-tol-ms", type=float, default=DEFAULT_SUSTAINED_TOL_MS)
    d.add_argument("--prev-max-age-s", type=float, default=DEFAULT_PREV_MAX_AGE_S)
    ns = ap.parse_args(argv)

    if ns.cmd == "decide":
        try:
            reason = hold_reason(
                ns.residual_median_ms, ns.residual_spread_ms, ns.band_verdict,
                ns.last_applied_offset_ms, ns.proposed_offset_ms,
                prev_residual_ms=ns.prev_residual_ms, prev_residual_age_s=ns.prev_residual_age_s,
                jump_threshold_ms=ns.jump_threshold_ms, residual_ceiling_ms=ns.residual_ceiling_ms,
                sustained_tol_ms=ns.sustained_tol_ms, prev_max_age_s=ns.prev_max_age_s,
            )
        except Exception:  # noqa: BLE001 - a guard must NEVER crash the E2E cleanup trap around it
            reason = ""
        print(f"hold_reason={reason}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
