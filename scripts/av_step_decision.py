#!/usr/bin/env python3
"""#1267 — PURE decision core for the dev1 upstream-audio-latency STEP watchdog.

WHY: on 2026-09-01 the mastered Dante feed into the stream box's DVS `mbc` source got ≈ −50…−90 ms
later at 17:50–18:10 local (an UPSTREAM audio-chain latency STEP, NOT the stream-OBS ts_lag flap and
NOT the video path), while the genlock pin `NDI 2ME PGM` held 926 and strih had no reboot. The
stream av-sync dock already MEASURED it — its `LOCK-CORRECT SUGGESTED genlock_latency_ms_src <pin> ->
<new>ms (measured offset=<X>ms)` line (monitor-only, ~2/min) is a live, E2E-independent,
restart-independent A/V trend — but nothing off the box read it, so the shift was invisible until the
E2E A/V gate residual read −77/−126/−111, ~3 h later. bundle_state_gather now summarizes that dock
series into the `av_offset_*` facets on `:8899/bundle-state.json`; this module is the pure kernel of
the dev1 watchdog that reads them from the stream box and decides when to page a report-only ⚠️.

No I/O, no ssh, no OBS, no MCP — exhaustively unit-testable (pytest), the audio_lag #1226 / #1199
python-mirror precedent, so the decision RED->GREENs LOCALLY under Tier-0 (#557 kills cargo). The
orchestrator scripts/av-step-alert-watchdog.sh curls the JSON, calls `analyze` here, and drives
obs-watchdog-decision.sh's confirm/throttle + airuleset notify (--dedup-key #1206).

The COVARIATE, not subtracted: a live pin jump 976->1024 (E2E test-latency churn) left the raw
measured offset ~unchanged, so `offset - pin` reads a −48 ms PHANTOM step. Instead the box reports a
pin_stable flag; a pin move in the analyzed span -> REPIN (report-only, no page). So a step is only
ever judged across a CONSTANT-pin window — exactly the 2026-09-01 case (pin held 926 for hours).

Verdicts (classify_av_step):
  SKIP    -- box could not be fetched (:8899 down / box down). That page is #732 (bundle-state) /
             #1001 (network-reach) territory, never this watchdog's -- so paging requires a
             successfully fetched POSITIVE step reading, and a dev1-side outage can only produce
             SKIP (never a false page).
  STALE   -- box fetched OK, dock series PRESENT but the freshest line sits > stale_threshold_s behind
             the OBS log head (av_offset_age_s): the dock stopped emitting WHILE the log advanced.
             Surfaced DISTINCTLY (machine-channel log, NO phone page -- absence is never paged),
             decided BEFORE the step checks so a stale series is never a false STEP page.
  UNKNOWN -- box fetched OK but the facet is absent (box not upgraded, or no dock line in the tail
             yet), OR too few samples in either window to judge (never a false step off thin data).
  REPIN   -- the pin moved across the analyzed span (a #856/operator/E2E apply settling): report-only,
             NO page. Report-only alarms would be false during pin churn.
  HEALTHY -- |recent_med - base_med| <= step_threshold_ms.
  STEP    -- |recent_med - base_med| > step_threshold_ms (a sustained upstream A/V shift at a constant
             pin). The watchdog pages a report-only ⚠️ after a 2-pass confirm.
"""
import argparse
import json
import sys

# Normal 10-min dock medians wander ±30 ms within an hour; the 2026-09-01 step was ≈ −60…−90 ms
# sustained ≥20 min. 45 ms cleanly separates the two. env-overridable at the watchdog (AV_STEP_THRESHOLD_MS).
DEFAULT_STEP_THRESHOLD_MS = 45
# The dock emits ~2/min; require ≥6 samples (≈3 min) in EACH window so a median is robust and a thin
# tail (a fresh session) reads UNKNOWN, never a false step.
DEFAULT_MIN_SAMPLES = 6
# A dock series whose freshest line is older than this (in-log seconds behind the OBS log head) has
# STOPPED while the log advanced -> STALE. ~10x the ~30 s SUGGESTED cadence; matches the box-side
# bundle_state_gather.AV_OFFSET_STALE_AFTER_S.
DEFAULT_STALE_THRESHOLD_S = 300


def _loads_obj(bundle_json_text):
    """A /bundle-state.json body -> its dict, or None (empty/None input, non-JSON, or a non-object
    top level). The ONE json parse — `extract_av_step`/`analyze`/`_main` all route through it so a
    single pass never parses the body more than once."""
    if not bundle_json_text:
        return None
    try:
        obj = json.loads(bundle_json_text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def _float_or_none(raw):
    """A facet value -> float, or None for a missing/empty/non-numeric value (UNKNOWN — never a
    fabricated reading, matching the gather's omit-when-empty contract)."""
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return None
    try:
        return float(str(raw).strip())
    except (ValueError, TypeError):
        return None


def _int_or_none(raw):
    """A facet value -> int, or None for a missing/empty/non-integer value."""
    if raw is None or (isinstance(raw, str) and raw.strip() == ""):
        return None
    try:
        return int(str(raw).strip())
    except (ValueError, TypeError):
        return None


def _from_obj(obj):
    """`(recent_med, base_med, pin, pin_stable_str, age_s, n_recent, n_base)` from an already-parsed
    bundle dict (or None). Every field None/absent when the facet is missing (UNKNOWN downstream).
    `pin_stable` is kept as a string ("1"/"0"/None) — the classifier compares it to "1" exactly, so a
    missing flag (None) is never mistaken for stable."""
    if not isinstance(obj, dict):
        return (None, None, None, None, None, None, None)
    ps = obj.get("av_offset_pin_stable")
    return (
        _float_or_none(obj.get("av_offset_recent_med_ms")),
        _float_or_none(obj.get("av_offset_base_med_ms")),
        _int_or_none(obj.get("av_offset_pin")),
        (str(ps).strip() if ps is not None and str(ps).strip() != "" else None),
        _int_or_none(obj.get("av_offset_age_s")),
        _int_or_none(obj.get("av_offset_n_recent")),
        _int_or_none(obj.get("av_offset_n_base")),
    )


def extract_av_step(bundle_json_text):
    """Parse a /bundle-state.json body -> the #1267 av-offset fields (see `_from_obj`)."""
    return _from_obj(_loads_obj(bundle_json_text))


def classify_av_step(recent_med, base_med, pin_stable, age_s, n_recent, n_base, box_reachable,
                     step_threshold_ms=DEFAULT_STEP_THRESHOLD_MS, min_samples=DEFAULT_MIN_SAMPLES,
                     stale_threshold_s=DEFAULT_STALE_THRESHOLD_S):
    """One box's verdict. `box_reachable` is 1 iff the JSON was fetched this pass.

      box_reachable != 1                    -> SKIP    (defer #732/#1001; never our page)
      age_s > stale_threshold_s             -> STALE   (dock stopped while the log advanced; decided
                                                        BEFORE the step checks so a stale series is
                                                        never a false STEP page. age_s None — an old
                                                        box with no freshness facet — skips this)
      recent_med is None or base_med is None -> UNKNOWN (facet absent / no dock line in the tail)
      n_recent/n_base < min_samples          -> UNKNOWN (too few samples to judge — never a false step)
      pin_stable != "1"                      -> REPIN   (a #856/operator/E2E pin move; report-only, no
                                                        page. A missing flag (None) is NOT "1", so it
                                                        never masks a step off an unknown-pin span)
      |recent_med - base_med| > threshold    -> STEP
      otherwise                              -> HEALTHY
    """
    if box_reachable != 1:
        return "SKIP"
    if age_s is not None and age_s > stale_threshold_s:
        return "STALE"
    if recent_med is None or base_med is None:
        return "UNKNOWN"
    if n_recent is None or n_base is None or n_recent < min_samples or n_base < min_samples:
        return "UNKNOWN"
    if pin_stable != "1":
        return "REPIN"
    if abs(recent_med - base_med) > step_threshold_ms:
        return "STEP"
    return "HEALTHY"


def analyze(bundle_json_text, box_reachable, step_threshold_ms=DEFAULT_STEP_THRESHOLD_MS,
            min_samples=DEFAULT_MIN_SAMPLES, stale_threshold_s=DEFAULT_STALE_THRESHOLD_S):
    """Fetch-result -> `{"verdict","recent_med_ms","base_med_ms","pin","step_ms"}`. When the box was
    not reachable, returns SKIP WITHOUT parsing the (empty) body, mirroring the caller's
    no-double-page guard. The age / pin_stable / sample counts drive the verdict internally but are
    not all echoed in the dict (the shell reads them from separate CLI lines)."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "recent_med_ms": None, "base_med_ms": None, "pin": None,
                "step_ms": None}
    (recent_med, base_med, pin, pin_stable, age_s, n_recent, n_base) = extract_av_step(
        bundle_json_text)
    verdict = classify_av_step(recent_med, base_med, pin_stable, age_s, n_recent, n_base,
                               box_reachable, step_threshold_ms, min_samples, stale_threshold_s)
    step_ms = None
    if recent_med is not None and base_med is not None:
        step_ms = round(recent_med - base_med, 1)
    return {"verdict": verdict, "recent_med_ms": recent_med, "base_med_ms": base_med, "pin": pin,
            "step_ms": step_ms}


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure upstream-audio-latency step watchdog decisions (#1267)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser(
        "analyze",
        help="read /bundle-state.json on stdin -> verdict + recent/base med + pin + step + age + pin_stable")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--step-threshold-ms", type=int, default=DEFAULT_STEP_THRESHOLD_MS)
    a.add_argument("--min-samples", type=int, default=DEFAULT_MIN_SAMPLES)
    a.add_argument("--stale-threshold-s", type=int, default=DEFAULT_STALE_THRESHOLD_S)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The bundle-state body is well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway
        # (the audio_lag #1231 precedent: a strict read that raised was swallowed by the caller's
        # 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        obj = _loads_obj(text) if ns.box_reachable == 1 else None
        (recent_med, base_med, pin, pin_stable, age_s, n_recent, n_base) = _from_obj(obj)
        verdict = "SKIP" if ns.box_reachable != 1 else classify_av_step(
            recent_med, base_med, pin_stable, age_s, n_recent, n_base, ns.box_reachable,
            ns.step_threshold_ms, ns.min_samples, ns.stale_threshold_s)
        step_ms = None
        if recent_med is not None and base_med is not None:
            step_ms = round(recent_med - base_med, 1)
        for k, v in (("verdict", verdict), ("recent_med_ms", recent_med), ("base_med_ms", base_med),
                     ("pin", pin), ("step_ms", step_ms), ("age_s", age_s), ("pin_stable", pin_stable),
                     ("n_recent", n_recent), ("n_base", n_base)):
            print(f"{k}={_fmt(v)}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
