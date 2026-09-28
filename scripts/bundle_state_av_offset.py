#!/usr/bin/env python3
"""The av-sync dock facet parsers of the bundle-state gather (#1267 / #1319 / #1325): the
measured-offset trend, the dock-live heartbeat age, and the estimator's quality + its age.

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import re

from bundle_state_log import LOG_BOUNDED_READ_SEPARATOR, _log_line_seconds, _recency_gap_s


# #1267 — the av-sync dock's measured-offset line, the UPSTREAM-audio-latency early-warning signal
# (issue 1265 follow-up). The stream box's dock runs monitor-only, so it logs the Suggest branch
# (vendor/av-sync-dock/src/sync-test-output.cpp:1484 -- verified LIVE 2026-09-02, ~2/min):
#   av-sync-dock: LOCK-CORRECT SUGGESTED genlock_latency_ms_src <pin> -> <new>ms (measured offset=<X>ms) [monitor-only ...]
# It carries BOTH the CURRENT genlock pin (int) AND the measured A/V offset (float ms) on ONE line.
# The `(?:SUGGESTED|requested)` alternation also matches a future actuation line; the OTHER
# LOCK-CORRECT variants (apply-skipped / read-back mismatch / pinned / unavailable) lack the
# `-> Nms (measured offset=` shape, so they never match. A sustained STEP in the median offset AT A
# CONSTANT PIN is a physical A/V shift into the DVS `mbc` source -- the 2026-09-01 incident, flagged
# ~3h before the first E2E A/V failure. The pin is a COVARIATE, NEVER subtracted: a live pin jump
# 976->1024 left the raw offset ~unchanged, so `offset - pin` reads a phantom step -- instead a pin
# change in the analyzed span sets pin_stable=0 and the dev1 decision HOLDs (REPIN, no page).
_AV_OFFSET_SUGGEST_RE = re.compile(
    r"av-sync-dock: LOCK-CORRECT (?:SUGGESTED|requested) genlock_latency_ms_src "
    r"(\d+) -> \d+ms \(measured offset=(-?\d+(?:\.\d+)?)ms\)"
)

# #1319 — the dock's per-10s heartbeat line, emitted whenever the dock is LOCKED regardless of the
# measured offset (`av-sync-dock: diag ... locked=yes state=LIVE`). NOTE (review F3): the diag line
# prints `locked=%s state=%s` INDEPENDENTLY, and this regex keys on `locked=yes` — NOT the `state`
# token. A dock that is `locked=no` (still acquiring) is therefore treated as non-live here, so the
# thin-sample branch reads STALE rather than IN_BAND_QUIET — the SAFE direction (no page; a genuine
# dock/lock loss is the genlock-lock/frozen-input watchdogs' job), and moot on the samples-present
# OUT_OF_BAND path. Its freshness (av_offset_dock_live_age_from_log) lets the dev1 band decision
# distinguish "dock LOCKED, offset in the suggestion dead band" (IN_BAND_QUIET, healthy) from "dock
# silent" (STALE) — the false-STALE the SUGGESTED-only age read during a dead-band quiet window.
_AV_OFFSET_DIAG_LOCKED_RE = re.compile(r"av-sync-dock: diag .*\blocked=yes\b")

# #1319 Part 2 — the dock's own cluster-QUALITY line (`av-sync-dock: {LOCKED,UPDATED} offset=Xms
# source=cluster matched=M mad=Dms`, sync-test-output.cpp:1378). It carries the estimator's
# per-lock cluster SIZE (`matched`) and per-sample SCATTER (`mad`) — the two things the band alarm
# needs to know a reading is trustworthy. The overnight 78-page false alarm judged a dock reading
# whose MAD (9-31 ms) was as wide as the +-30 ms band against a recording-based reference; the dev1
# band decision now reads LOW_QUALITY (log-only, never a page) unless the recent window's median MAD
# is <= 15 ms AND its min matched is >= 30. This is a SEPARATE facet from the offset SERIES
# (av_offset_series_from_log stays BYTE-IDENTICAL — the Part-1 decision that the raw UPDATED/LOCKED
# lines are NOT folded into the series holds; only their matched/mad feed this quality facet).
_AV_OFFSET_QUALITY_RE = re.compile(
    r"av-sync-dock: (?:LOCKED|UPDATED) offset=-?\d+(?:\.\d+)?ms source=cluster "
    r"matched=(\d+) mad=(\d+(?:\.\d+)?)ms"
)

# #1267 — rolling-window bounds, in-log seconds behind the log head. RECENT = the freshest 10 min;
# BASELINE = the 10..40 min region behind it (a rolling reference that predates the recent window).
# The BASELINE is bounded above by how far the #1222 bounded TAIL reaches (~50 min on a long
# session), which also bounds the detection window: a rolling baseline ABSORBS a persistent step
# after ~baseline_window_s, so a step is detectable only for a ~20-40 min window at onset (the
# dev1 watchdog freezes the pre-step baseline at alert time so it never mis-reads that absorption
# as a recovery -- av_step_decision.recovered_to_baseline).
AV_OFFSET_RECENT_WINDOW_S = 600
AV_OFFSET_BASELINE_WINDOW_S = 2400
# The box-side parser reports the raw in-log freshness age; the STALE threshold is applied dev1-side
# (av_step_decision.DEFAULT_STALE_THRESHOLD_S), so no box-side stale constant is needed here.


def _median(values):
    """Median of a list of floats (no numpy/statistics dependency). None for an empty list."""
    n = len(values)
    if n == 0:
        return None
    s = sorted(values)
    mid = n // 2
    if n % 2:
        return s[mid]
    return (s[mid - 1] + s[mid]) / 2.0


def av_offset_series_from_log(text, recent_window_s=AV_OFFSET_RECENT_WINDOW_S,
                              baseline_window_s=AV_OFFSET_BASELINE_WINDOW_S):
    """#1267 — the av-sync dock measured-offset trend, summarized as SCALARS for the dev1
    upstream-step watchdog. Returns
    `(recent_med_str, base_med_str, pin_str, pin_stable_str, age_s_str, n_recent_str, n_base_str)`,
    every field "" when absent (UNKNOWN downstream, never a fabricated reading):

    * recent_med / base_med — median measured offset (ms, 1 decimal) over the RECENT window (freshest
      recent_window_s of dock lines) and the BASELINE window (recent_window_s..baseline_window_s
      behind the head). A sustained upstream shift = |recent - base| beyond the dev1 step threshold.
    * pin — the CURRENT (freshest) genlock pin on a dock line.
    * pin_stable — "1" iff every windowed sample (baseline UNION recent) carries the SAME pin, else
      "0". A #856/operator/E2E pin move -> "0" -> the dev1 decision HOLDs (REPIN), never a false step
      (the pin is NOT subtracted; see the regex comment for why the naive subtraction was falsified).
    * age_s — in-log whole-second age of the freshest dock line behind the log's newest line of ANY
      kind (#1231 recency: file order IS time order, a single midnight wrap corrected; NEVER
      max(seconds-of-day)). "" only when there is NO dock line at all; a large value -> STALE.
    * n_recent / n_base — windowed sample counts. The dev1 decision needs enough of each to judge;
      too few -> UNKNOWN, never a false step.

    Reads ONLY the TAIL slice of the #1222 bounded head+separator+tail read (a stale value surviving
    only in the head is never reported; a small whole-file log is scanned entirely), in ONE pass over
    the SAME log_text every other _from_log parser uses (no second log read)."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    samples = []          # (ts_or_None, offset_ms_float, pin_int) in file order
    log_newest_ts = None  # ts of the LAST parseable line in file order (the log write head)
    last_dock_ts = None   # ts of the LAST dock line in file order (the freshest measured offset)
    latest_pin = None     # pin on the freshest dock line
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _AV_OFFSET_SUGGEST_RE.search(line)
        if m:
            pin = int(m.group(1))
            off = float(m.group(2))
            samples.append((ts, off, pin))
            latest_pin = pin
            if ts is not None:
                last_dock_ts = ts
    if not samples:
        return ("", "", "", "", "", "", "")

    age_s = ""
    gap = _recency_gap_s(log_newest_ts, last_dock_ts)
    if gap is not None:
        age_s = str(round(gap))

    # Partition the aged samples into the recent / baseline windows by in-log age behind the head.
    # A sample with no parseable ts cannot be aged, so it is dropped from the windows (it still fed
    # latest_pin above). pin_stability is judged over the SAME windowed span the medians use.
    recent_offs, base_offs, span_pins = [], [], []
    for ts, off, pin in samples:
        g = _recency_gap_s(log_newest_ts, ts)
        if g is None:
            continue
        if g <= recent_window_s:
            recent_offs.append(off)
            span_pins.append(pin)
        elif g <= baseline_window_s:
            base_offs.append(off)
            span_pins.append(pin)

    recent_med = _median(recent_offs)
    base_med = _median(base_offs)
    # "1" only when the whole windowed span shares one pin; an empty span -> "0" (but the dev1
    # decision reads UNKNOWN off the zero sample counts first, so pin_stable is moot there).
    pin_stable = "1" if span_pins and len(set(span_pins)) == 1 else "0"
    return (
        "" if recent_med is None else f"{recent_med:.1f}",
        "" if base_med is None else f"{base_med:.1f}",
        "" if latest_pin is None else str(latest_pin),
        pin_stable,
        age_s,
        str(len(recent_offs)),
        str(len(base_offs)),
    )


def av_offset_dock_live_age_from_log(text):
    """#1319 — the in-log whole-second age of the freshest `av-sync-dock: diag ... locked=yes` line
    behind the log's newest parseable line of ANY kind. Returns "" when there is NO such line.

    The dock emits this heartbeat (~every 10 s) whenever it is LIVE, INDEPENDENT of the measured
    offset — so a FRESH age here while the SUGGESTED offset series is silent means "dock LIVE, offset
    inside the suggestion dead band" (the dev1 band decision reads IN_BAND_QUIET, healthy), whereas a
    STALE age means the dock itself stopped (STALE). This closes the false-STALE the SUGGESTED-only
    `av_offset_age_s` read during a dead-band quiet window (the owner's 19:44-19:55 gap, 15.9.2026).

    Same recency model as av_offset_series_from_log (`_recency_gap_s`, midnight-wrap corrected, file
    order IS time order) over ONLY the #1222 bounded TAIL, one pass, no wall clock injected."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    log_newest_ts = None
    last_live_ts = None
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        if ts is not None and _AV_OFFSET_DIAG_LOCKED_RE.search(line):
            last_live_ts = ts
    if last_live_ts is None:
        return ""
    gap = _recency_gap_s(log_newest_ts, last_live_ts)
    return "" if gap is None else str(round(gap))


def av_offset_quality_from_log(text, recent_window_s=AV_OFFSET_RECENT_WINDOW_S):
    """#1319 Part 2 — the dock estimator's measurement QUALITY over the RECENT window, as SCALARS
    for the dev1 band alarm. Returns `(recent_mad_str, recent_matched_min_str)`, both "" when there
    is no `LOCKED/UPDATED offset= ... matched= mad=` line in the recent window (UNKNOWN downstream,
    never fabricated):

    * recent_mad — MEDIAN of the per-lock `mad=` scatter (ms, 1 decimal) over the freshest
      recent_window_s of dock quality lines. The band arm requires this <= 15 ms.
    * recent_matched_min — the MINIMUM `matched=` cluster size in that window (the worst-case
      trust). The band arm requires this >= 30. Min (not median) so a single thin lock in the
      window is enough to read LOW_QUALITY — the safe direction (never page off a thin cluster).

    Reads ONLY the TAIL slice of the #1222 bounded read, same recency model (`_recency_gap_s`,
    file order IS time order) as av_offset_series_from_log, one pass, no wall clock. This is a
    SEPARATE parser from the offset series (which stays byte-identical): it consumes the same
    LOCKED/UPDATED lines the series deliberately excludes, but ONLY for matched/mad — never their
    offset (a different sign/bias from the SUGGESTED series, #952)."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    samples = []          # (ts_or_None, matched_int, mad_float) in file order
    log_newest_ts = None
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _AV_OFFSET_QUALITY_RE.search(line)
        if m:
            samples.append((ts, int(m.group(1)), float(m.group(2))))
    recent_mads, recent_matched = [], []
    for ts, matched, mad in samples:
        g = _recency_gap_s(log_newest_ts, ts)
        if g is None or g > recent_window_s:
            continue
        recent_mads.append(mad)
        recent_matched.append(matched)
    if not recent_mads:
        return ("", "")
    med = _median(recent_mads)
    return (
        "" if med is None else f"{med:.1f}",
        str(min(recent_matched)),
    )


def av_offset_quality_age_from_log(text):
    """camera-box #1325 — the in-log whole-second age of the freshest dock QUALITY line
    (`av-sync-dock: {LOCKED,UPDATED} offset= … matched= mad=`, the _AV_OFFSET_QUALITY_RE lines)
    behind the log's newest parseable line of ANY kind. Returns "" when there is NO such line at all.

    Why (the 3× false page 16.9.2026): when the QPSK marker cadence dropped to 0.5 s the dock stopped
    decoding, so it emitted NO more UPDATED/LOCKED quality lines, yet its SUGGESTED offset SERIES kept
    producing (stale) offsets — so `av_offset_recent_mad_ms` read ABSENT (None) and the band arm's
    `band_quality_ok(None) -> proceed` (#1319: "never swallow a real drift") paged on an untrustworthy
    reading. This age lets the dev1 decision distinguish "dock actively measuring, just no cluster in
    THIS recent window" (fresh age -> keep proceeding) from "dock stopped measuring entirely"
    (stale/large age -> LOW_QUALITY, no page). Distinct from `av_offset_dock_live_age_s`, which ages
    the `diag … locked=yes` heartbeat (the dock's MONITOR loop keeps beating even with a dead decoder,
    so it stayed fresh through the incident and could not gate the estimator's own staleness).

    Same recency model as av_offset_dock_live_age_from_log (`_recency_gap_s`, midnight-wrap corrected,
    file order IS time order) over ONLY the #1222 bounded TAIL, one pass, no wall clock injected."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    log_newest_ts = None
    last_quality_ts = None
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        if ts is not None and _AV_OFFSET_QUALITY_RE.search(line):
            last_quality_ts = ts
    if last_quality_ts is None:
        return ""
    gap = _recency_gap_s(log_newest_ts, last_quality_ts)
    return "" if gap is None else str(round(gap))
