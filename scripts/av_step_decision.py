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
  LOW_QUALITY -- (#1319 P3) the dock estimator's recent window is too noisy/thin to judge (median
             MAD > 15 ms OR min matched < 30) OR carries a non-finite mad -> the median delta is not
             trustworthy, so the step is NOT paged (log-only). The SAME band_quality_ok() bar the
             BAND arm applies, single-sourced. Decided AFTER REPIN and BEFORE STEP/HEALTHY. An ABSENT
             quality facet (older box) is None (not False) -> proceed to the step judgement, so a
             genuine step is never swallowed. Closes the 16.9.2026 false-page: the STEP arm paged
             13:44/15:09 on readings whose medians swung ±1000 ms within 5-min passes (mad 31 > 15),
             which the BAND arm already rejected as LOW_QUALITY in the same passes.
  HEALTHY -- |recent_med - base_med| <= step_threshold_ms.
  STEP    -- |recent_med - base_med| > step_threshold_ms (a sustained upstream A/V shift at a constant
             pin). The watchdog pages a report-only ⚠️ after a 2-pass confirm.
"""
import argparse
import json
import math
import sys

# Normal 10-min dock medians wander ±30 ms within an hour; the 2026-09-01 step was ≈ −60…−90 ms
# sustained ≥20 min. 45 ms cleanly separates the two. env-overridable at the watchdog (AV_STEP_THRESHOLD_MS).
DEFAULT_STEP_THRESHOLD_MS = 45
# The dock emits ~2/min; require ≥6 samples (≈3 min) in EACH window so a median is robust and a thin
# tail (a fresh session) reads UNKNOWN, never a false step.
DEFAULT_MIN_SAMPLES = 6
# A dock series whose freshest line is older than this (in-log seconds behind the OBS log head) has
# STOPPED while the log advanced -> STALE. ~10x the ~30 s SUGGESTED cadence. The box-side parser
# reports the raw age; this dev1 threshold is the single place the STALE bound lives.
DEFAULT_STALE_THRESHOLD_S = 300

# #1325 — the freshness window for the dock's own QUALITY (UPDATED/LOCKED matched/mad) line. When the
# quality facet is ABSENT (recent_mad_ms None) but the dock has emitted NO quality line within this
# many seconds, its decoder has stopped (the QPSK marker cadence 0.5 s incident, 16.9.2026) and the
# SUGGESTED offsets it is still emitting are untrustworthy -> LOW_QUALITY (no page). A FRESH quality
# age (dock actively measuring, just no cluster in THIS recent window) OR an ABSENT age (older box
# with no quality line at all) keeps #1319's "absent quality -> proceed, never swallow a real drift".
# Matches DEFAULT_STALE_THRESHOLD_S so an absent-quality reading is trusted for the same window a
# present series is, before it is judged dead.
DEFAULT_QUALITY_STALE_S = 300


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
                     stale_threshold_s=DEFAULT_STALE_THRESHOLD_S,
                     recent_mad_ms=None, recent_matched_min=None,
                     quality_age_s=None, quality_stale_s=DEFAULT_QUALITY_STALE_S):
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
      band_quality_ok(...) is False          -> LOW_QUALITY (#1319 P3: the dock estimator's recent
                                                        window is too noisy/thin (median MAD > 15 ms
                                                        OR min matched < 30) OR carries a non-finite
                                                        mad -> the median delta is not judgeable, so
                                                        the step is not paged. The SAME predicate the
                                                        BAND arm applies, single-sourced (its module
                                                        defaults). An ABSENT quality facet returns
                                                        None (NOT False) -> proceed to today's step
                                                        judgement, so an older box is unchanged and a
                                                        genuine step is never swallowed.)
      |recent_med - base_med| > threshold    -> STEP
      otherwise                              -> HEALTHY

    Verdict order (mirrors the module docstring and classify_av_band): SKIP -> STALE -> UNKNOWN ->
    REPIN -> quality (LOW_QUALITY) -> STEP/HEALTHY. Quality sits AFTER REPIN (a pin move still wins)
    and BEFORE the step decision, so a noisy reading never false-pages as a step.
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
    # #1319 P3 — consult the SAME dock-measurement quality bar the BAND arm uses (band_quality_ok,
    # DEFAULT_BAND_QUALITY_* — single-sourced, never a retyped 15/30). None (facet absent, older box)
    # is NOT False, so the step still judges; a non-finite mad -> False -> LOW_QUALITY (a corrupt
    # reading is untrustworthy). See the 16.9.2026 false-page incident.
    q = band_quality_ok(recent_mad_ms, recent_matched_min)
    if q is False:
        return "LOW_QUALITY"
    # #1325 — the quality facet is ABSENT (q is None) AND the dock's last quality line is STALE ->
    # its decoder has stopped, so the SUGGESTED offsets driving recent_med/base_med are untrustworthy
    # -> LOW_QUALITY (no page). A FRESH or ABSENT quality age preserves #1319's absent->proceed.
    if q is None and quality_age_s is not None and quality_age_s > quality_stale_s:
        return "LOW_QUALITY"
    if abs(recent_med - base_med) > step_threshold_ms:
        return "STEP"
    return "HEALTHY"


def recovered_to_baseline(recent_med, recovery_base, step_threshold_ms=DEFAULT_STEP_THRESHOLD_MS):
    """Given the watchdog ALERTED on a step and FROZE the pre-step baseline (`recovery_base`), has the
    offset PHYSICALLY returned to it? `True` iff both are present and `|recent - recovery_base| <=
    step_threshold`, else `False`; `None` when either is absent (no recovery judgement possible).

    WHY (the #1267 review 🟡): the box-side baseline is a ROLLING 10-40 min window, so a PERSISTENT
    step self-normalizes — ~baseline_window_s after onset the rolling baseline has absorbed the step
    and the box reports HEALTHY (recent ≈ rolling-base) even though the offset never came back.
    Judging recovery against the ROLLING baseline would then falsely log "back to normal" and clear
    the alert. Freezing the pre-step baseline at alert time and comparing the CURRENT recent median
    against THAT makes recovery mean "the physical offset actually returned", never "the step became
    the new normal"."""
    if recent_med is None or recovery_base is None:
        return None
    return abs(recent_med - recovery_base) <= step_threshold_ms


def analyze(bundle_json_text, box_reachable, step_threshold_ms=DEFAULT_STEP_THRESHOLD_MS,
            min_samples=DEFAULT_MIN_SAMPLES, stale_threshold_s=DEFAULT_STALE_THRESHOLD_S,
            recovery_base=None):
    """Fetch-result -> the FULL decision dict (`verdict`, `recent_med_ms`, `base_med_ms`, `pin`,
    `step_ms`, `age_s`, `pin_stable`, `n_recent`, `n_base`, `recovered`). This is the ONE parse ->
    classify path both the pure tests and the shell (`_main` prints this dict) use, so the tested
    path is the production path. When the box was not reachable, returns SKIP WITHOUT parsing the
    (empty) body (all fields None), mirroring the caller's no-double-page guard.

    `recovered` is `1`/`0` ONLY when `recovery_base` is supplied (the watchdog passes the FROZEN
    pre-step baseline while a box is in the alerted state), else `None` — see `recovered_to_baseline`."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "recent_med_ms": None, "base_med_ms": None, "pin": None,
                "step_ms": None, "age_s": None, "pin_stable": None, "n_recent": None,
                "n_base": None, "recovered": None}
    obj = _loads_obj(bundle_json_text)
    (recent_med, base_med, pin, pin_stable, age_s, n_recent, n_base) = _from_obj(obj)
    # #1319 P3 — read the SAME dock-measurement quality facets the band arm reads (same
    # _float_or_none/_int_or_none path), so the STEP arm's classify_av_step can gate an
    # untrustworthy reading to LOW_QUALITY instead of paging a phantom step. Absent facets read as
    # None -> band_quality_ok None -> today's legacy step judgement (older box unchanged).
    recent_mad_ms = _float_or_none(obj.get("av_offset_recent_mad_ms")) if isinstance(obj, dict) else None
    recent_matched_min = _int_or_none(obj.get("av_offset_recent_matched_min")) if isinstance(obj, dict) else None
    # #1325 — the freshest dock-quality-line age; gates an ABSENT quality facet to LOW_QUALITY when
    # the dock stopped decoding (age stale), else proceeds (older box / dock actively measuring).
    quality_age_s = _int_or_none(obj.get("av_offset_quality_age_s")) if isinstance(obj, dict) else None
    verdict = classify_av_step(recent_med, base_med, pin_stable, age_s, n_recent, n_base,
                               box_reachable, step_threshold_ms, min_samples, stale_threshold_s,
                               recent_mad_ms=recent_mad_ms, recent_matched_min=recent_matched_min,
                               quality_age_s=quality_age_s)
    step_ms = None
    if recent_med is not None and base_med is not None:
        step_ms = round(recent_med - base_med, 1)
    recovered = None
    if recovery_base is not None:
        r = recovered_to_baseline(recent_med, recovery_base, step_threshold_ms)
        recovered = None if r is None else (1 if r else 0)
    return {"verdict": verdict, "recent_med_ms": recent_med, "base_med_ms": base_med, "pin": pin,
            "step_ms": step_ms, "age_s": age_s, "pin_stable": pin_stable, "n_recent": n_recent,
            "n_base": n_base, "recovered": recovered}


# #1319 — the ABSOLUTE-BAND arm. The #1267 STEP term measures the CHANGE-rate of the 10-min median
# vs a rolling baseline, so it is structurally blind to a SLOW absolute drift (the owner's 15.9.2026
# +13->+47 ms wander at a constant pin never crosses a 45 ms adjacent-median delta, and the rolling
# baseline self-normalizes it). The band term pages when the recent median offset leaves ±band of a
# FIXED E2E-aligned reference. The reference is resolved dev1-side (env / a ~/.camera-box file / a
# stated 0 ms fallback) and passed in — this pure kernel never does I/O.
DEFAULT_BAND_MS = 30
DEFAULT_BAND_REFERENCE_MS = 0.0

# #1319 Part 2 — the dock-estimator measurement-QUALITY bar the band verdict requires. The overnight
# 78-page false alarm judged a dock reading whose per-sample scatter (MAD 9-31 ms) was as wide as
# the +-30 ms band against a recording-based reference. A band this tight cannot be judged from an
# estimator that noisy, so an OUT_OF_BAND page now requires the recent window's median MAD to be
# <= 15 ms AND its min cluster size (matched) to be >= 30; otherwise -> LOW_QUALITY (log-only).
DEFAULT_BAND_QUALITY_MAX_MAD_MS = 15.0
DEFAULT_BAND_QUALITY_MIN_MATCHED = 30


def band_quality_ok(recent_mad_ms, recent_matched_min,
                    max_mad_ms=DEFAULT_BAND_QUALITY_MAX_MAD_MS,
                    min_matched=DEFAULT_BAND_QUALITY_MIN_MATCHED):
    """Is the dock estimator's recent-window measurement trustworthy enough to page a band excursion?

      recent_mad_ms is None OR recent_matched_min is None -> None (UNJUDGEABLE: no quality facet,
          e.g. an older box or no LOCKED/UPDATED line in the window. The band decision treats None
          as "proceed" -- NOT LOW_QUALITY -- so a genuine sustained offset with no recent cluster
          line still pages; the safe direction is never SWALLOWING a real drift.)
      recent_mad_ms present but NON-FINITE (NaN/Inf)     -> False (#1319 review: a corrupt reading is
          untrustworthy, distinct from ABSENT -- treat it as failing quality, never as "proceed").
      mad <= max_mad_ms AND matched >= min_matched                 -> True  (trustworthy)
      otherwise (present AND (mad too wide OR cluster too small))  -> False (-> LOW_QUALITY, no page)
    """
    if recent_mad_ms is None or recent_matched_min is None:
        return None
    if not math.isfinite(recent_mad_ms):
        return False
    return recent_mad_ms <= max_mad_ms and recent_matched_min >= min_matched


def classify_av_band(recent_med, pin_stable, n_recent, dock_live_age_s, box_reachable,
                     band_reference_ms=DEFAULT_BAND_REFERENCE_MS, band_ms=DEFAULT_BAND_MS,
                     min_samples=DEFAULT_MIN_SAMPLES, stale_threshold_s=DEFAULT_STALE_THRESHOLD_S,
                     recent_mad_ms=None, recent_matched_min=None,
                     quality_max_mad_ms=DEFAULT_BAND_QUALITY_MAX_MAD_MS,
                     quality_min_matched=DEFAULT_BAND_QUALITY_MIN_MATCHED,
                     quality_age_s=None, quality_stale_s=DEFAULT_QUALITY_STALE_S):
    """One box's ABSOLUTE-BAND verdict (`box_reachable` is 1 iff the JSON was fetched this pass):

      box_reachable != 1                          -> SKIP          (defer #732/#1001; never our page)
      n_recent >= min_samples (enough offset samples in the recent window):
          pin_stable != "1"                       -> REPIN         (a pin move — the offset<->pin
                                                                    settling lag means the band is
                                                                    judged only at a CONSTANT pin,
                                                                    no page; a missing flag is not
                                                                    "1", never masks a drift)
          |recent_med - band_reference_ms| > band -> OUT_OF_BAND   (page after a 2-pass confirm)
          otherwise                               -> IN_BAND       (healthy)
      too few / no recent offset samples:
          dock_live_age_s is None                 -> UNKNOWN       (no dock heartbeat at all — can't
                                                                    judge; never a false anything)
          dock_live_age_s > stale_threshold_s     -> STALE         (the dock's LIVE line itself is
                                                                    stale — the dock stopped; never
                                                                    a page)
          otherwise                               -> IN_BAND_QUIET (dock LIVE + offset in the dock's
                                                                    suggestion dead band = healthy;
                                                                    this is the #1267 false STALE
                                                                    the freshness facet fixes)

    Judged against a FIXED anchor (`band_reference_ms`, the E2E-aligned value), so — unlike the
    STEP arm's rolling baseline — recovery is a plain return into band, no frozen-baseline latch.
    Band samples, when present, are judged even if the dock-live facet is absent (an older box)."""
    if box_reachable != 1:
        return "SKIP"
    if recent_med is not None and n_recent is not None and n_recent >= min_samples:
        if pin_stable != "1":
            return "REPIN"
        # #1319 Part 2 — a measurement too noisy/thin to trust (present AND out of the quality bar)
        # is LOW_QUALITY: the OUT_OF_BAND page requires a trustworthy reading. band_quality_ok
        # returns None when the quality facet is absent (older box / no cluster line in the window),
        # which is NOT False, so the band still judges -- a real sustained offset is never swallowed.
        q = band_quality_ok(recent_mad_ms, recent_matched_min, quality_max_mad_ms,
                            quality_min_matched)
        if q is False:
            return "LOW_QUALITY"
        # #1325 — quality facet ABSENT but the dock's last quality line is STALE (its decoder stopped,
        # the 16.9.2026 marker-cadence incident): the SUGGESTED offsets are untrustworthy -> LOW_QUALITY
        # (no page). A fresh/absent quality age keeps #1319's absent->proceed (real drift never swallowed).
        if q is None and quality_age_s is not None and quality_age_s > quality_stale_s:
            return "LOW_QUALITY"
        if abs(recent_med - band_reference_ms) > band_ms:
            return "OUT_OF_BAND"
        return "IN_BAND"
    if dock_live_age_s is None:
        return "UNKNOWN"
    if dock_live_age_s > stale_threshold_s:
        return "STALE"
    return "IN_BAND_QUIET"


def analyze_band(bundle_json_text, box_reachable, band_reference_ms=DEFAULT_BAND_REFERENCE_MS,
                 band_ms=DEFAULT_BAND_MS, min_samples=DEFAULT_MIN_SAMPLES,
                 stale_threshold_s=DEFAULT_STALE_THRESHOLD_S):
    """Fetch-result -> the FULL band decision dict (`verdict`, `recent_med_ms`, `band_reference_ms`,
    `band_delta_ms`, `dock_live_age_s`, `pin`, `pin_stable`, `n_recent`). ONE parse->classify path
    (the tested path is the production path); SKIP returns WITHOUT parsing the empty body."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "recent_med_ms": None, "band_reference_ms": band_reference_ms,
                "band_delta_ms": None, "dock_live_age_s": None, "pin": None, "pin_stable": None,
                "n_recent": None, "recent_mad_ms": None, "recent_matched_min": None,
                "quality_ok": None}
    obj = _loads_obj(bundle_json_text)
    (recent_med, _base, pin, pin_stable, _age, n_recent, _nb) = _from_obj(obj)
    dock_live_age_s = _int_or_none(obj.get("av_offset_dock_live_age_s")) if isinstance(obj, dict) else None
    # #1319 Part 2 — the dock estimator's recent-window measurement quality (median MAD + min matched).
    # A non-finite mad (NaN/Inf) is handled INSIDE band_quality_ok (-> False -> LOW_QUALITY), distinct
    # from an ABSENT facet (None -> proceed), so it is read here as a plain float-or-None.
    recent_mad_ms = _float_or_none(obj.get("av_offset_recent_mad_ms")) if isinstance(obj, dict) else None
    recent_matched_min = _int_or_none(obj.get("av_offset_recent_matched_min")) if isinstance(obj, dict) else None
    # #1325 — the freshest dock-quality-line age (LOW_QUALITY gate on an absent-but-stale reading).
    quality_age_s = _int_or_none(obj.get("av_offset_quality_age_s")) if isinstance(obj, dict) else None
    verdict = classify_av_band(recent_med, pin_stable, n_recent, dock_live_age_s, box_reachable,
                               band_reference_ms, band_ms, min_samples, stale_threshold_s,
                               recent_mad_ms=recent_mad_ms, recent_matched_min=recent_matched_min,
                               quality_age_s=quality_age_s)
    band_delta_ms = None if recent_med is None else round(recent_med - band_reference_ms, 1)
    q = band_quality_ok(recent_mad_ms, recent_matched_min)
    return {"verdict": verdict, "recent_med_ms": recent_med, "band_reference_ms": band_reference_ms,
            "band_delta_ms": band_delta_ms, "dock_live_age_s": dock_live_age_s, "pin": pin,
            "pin_stable": pin_stable, "n_recent": n_recent, "recent_mad_ms": recent_mad_ms,
            "recent_matched_min": recent_matched_min,
            "quality_ok": None if q is None else (1 if q else 0)}


# #1319 Part 2 — the DOCK-NATIVE reference the [4i/8] A/V-align persist step records so the band
# alarm compares DOCK-to-DOCK. The overnight bug: the band judged the dock's live estimator (bias
# ~ +85 ms) against the RECORDING-based residual (-35.3 ms), an ~120 ms frame mismatch that paged
# every pass. Recording the dock's OWN post-align median as the reference cancels that fixed bias
# (the recording residual stays the E2E GATE's truth; only the ALARM's reference changes). The
# reference is only recorded when the dock reading is trustworthy -- same quality bar as the band.
def dock_reference(bundle_json_text, box_reachable,
                   quality_max_mad_ms=DEFAULT_BAND_QUALITY_MAX_MAD_MS,
                   quality_min_matched=DEFAULT_BAND_QUALITY_MIN_MATCHED,
                   min_samples=DEFAULT_MIN_SAMPLES):
    """Fetch-result -> the quality-gated dock-native reference dict (`quality_ok`, `median_ms`, `n`,
    `mad_ms`, `matched_min`, `pin_stable`). `quality_ok` is 1 ONLY when the box was reachable, the
    recent window has >= min_samples offset samples, the pin was stable, AND the recent-window
    quality bar is met; else 0 (the persist step records NO dock reference, so the band falls back
    to the recording residual). SKIP-reachability returns quality_ok 0 WITHOUT parsing the body."""
    if box_reachable != 1:
        return {"quality_ok": 0, "median_ms": None, "n": None, "mad_ms": None,
                "matched_min": None, "pin_stable": None}
    obj = _loads_obj(bundle_json_text)
    (recent_med, _base, _pin, pin_stable, _age, n_recent, _nb) = _from_obj(obj)
    recent_mad_ms = _float_or_none(obj.get("av_offset_recent_mad_ms")) if isinstance(obj, dict) else None
    recent_matched_min = _int_or_none(obj.get("av_offset_recent_matched_min")) if isinstance(obj, dict) else None
    q = band_quality_ok(recent_mad_ms, recent_matched_min, quality_max_mad_ms, quality_min_matched)
    # #1319 review: a non-finite median must never be recorded as the dock-native reference (it would
    # blind the band arm on read); require a FINITE median for quality_ok, mirroring the mad guard.
    median_finite = recent_med is not None and math.isfinite(recent_med)
    ok = (median_finite and n_recent is not None and n_recent >= min_samples
          and pin_stable == "1" and q is True)
    return {"quality_ok": 1 if ok else 0, "median_ms": recent_med, "n": n_recent,
            "mad_ms": recent_mad_ms, "matched_min": recent_matched_min, "pin_stable": pin_stable}


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
    # The watchdog passes the FROZEN pre-step baseline (from state) while a box is in the alerted
    # state, so a `recovered=1/0` line can drive an HONEST recovery (offset physically returned) that
    # the rolling-baseline self-normalization cannot fake. Omitted -> `recovered=` blank.
    a.add_argument("--recovery-base", type=float, default=None)

    # #1319 — the ABSOLUTE-BAND arm: read /bundle-state.json on stdin -> the band verdict vs a FIXED
    # E2E-aligned reference (resolved dev1-side, passed here). Separate subcommand so the #1267 step
    # `analyze` and its tests are untouched.
    b = sub.add_parser(
        "analyze-band",
        help="read /bundle-state.json on stdin -> band verdict + recent_med + delta + dock_live_age")
    b.add_argument("--box-reachable", type=int, required=True)
    b.add_argument("--band-reference-ms", type=float, default=DEFAULT_BAND_REFERENCE_MS)
    b.add_argument("--band-ms", type=float, default=DEFAULT_BAND_MS)
    b.add_argument("--min-samples", type=int, default=DEFAULT_MIN_SAMPLES)
    b.add_argument("--stale-threshold-s", type=int, default=DEFAULT_STALE_THRESHOLD_S)

    # #1319 Part 2 — the quality-gated DOCK-NATIVE reference: read /bundle-state.json on stdin ->
    # the dock's recent-window median (+ n/mad/matched_min/quality_ok), the persist step records
    # when quality_ok=1 so the band compares dock-to-dock.
    r = sub.add_parser(
        "dock-reference",
        help="read /bundle-state.json on stdin -> quality-gated dock median + n + mad + quality_ok")
    r.add_argument("--box-reachable", type=int, required=True)
    r.add_argument("--quality-max-mad-ms", type=float, default=DEFAULT_BAND_QUALITY_MAX_MAD_MS)
    r.add_argument("--quality-min-matched", type=int, default=DEFAULT_BAND_QUALITY_MIN_MATCHED)
    r.add_argument("--min-samples", type=int, default=DEFAULT_MIN_SAMPLES)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The bundle-state body is well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway
        # (the audio_lag #1231 precedent: a strict read that raised was swallowed by the caller's
        # 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        # ONE parse->classify path: `analyze()` is exactly what the pure tests exercise (#1267 review
        # 🔵 -- the tested path is the production path).
        d = analyze(text, ns.box_reachable, ns.step_threshold_ms, ns.min_samples,
                    ns.stale_threshold_s, recovery_base=ns.recovery_base)
        for k in ("verdict", "recent_med_ms", "base_med_ms", "pin", "step_ms", "age_s",
                  "pin_stable", "n_recent", "n_base", "recovered"):
            print(f"{k}={_fmt(d[k])}")
        return 0

    if ns.cmd == "analyze-band":
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        d = analyze_band(text, ns.box_reachable, ns.band_reference_ms, ns.band_ms, ns.min_samples,
                         ns.stale_threshold_s)
        for k in ("verdict", "recent_med_ms", "band_reference_ms", "band_delta_ms",
                  "dock_live_age_s", "pin", "pin_stable", "n_recent", "recent_mad_ms",
                  "recent_matched_min", "quality_ok"):
            print(f"{k}={_fmt(d[k])}")
        return 0

    if ns.cmd == "dock-reference":
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        d = dock_reference(text, ns.box_reachable, ns.quality_max_mad_ms, ns.quality_min_matched,
                           ns.min_samples)
        for k in ("quality_ok", "median_ms", "n", "mad_ms", "matched_min", "pin_stable"):
            print(f"{k}={_fmt(d[k])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
