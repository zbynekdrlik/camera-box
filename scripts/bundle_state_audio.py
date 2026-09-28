#!/usr/bin/env python3
"""The OBS audio facet parsers of the bundle-state gather: the `audio-telemetry #800` lag +
freshness (#1226 / #1231), the reference-source ts_lag band (#1265), the buffered_ms drift/step
shape (#1325), and the audio MIXER real-time facet (issue 1381 / 1385). The obs-vban pacer
loss facet read beside the mixer lives in `bundle_state_vban`.

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import math
import re

from bundle_state_av_offset import AV_OFFSET_RECENT_WINDOW_S
from bundle_state_log import (
    LOG_BOUNDED_READ_SEPARATOR,
    _log_line_seconds,
    _recency_gap_s,
    timestamped_tail_lines,
)


# #1226 — the audio-timeline-lag telemetry line vendored OBS emits every 60 s per audio source
# (vendor/obs-studio/libobs/obs-audio.c:698): `audio-telemetry #800 '<src>': ts_lag_ms=<int64> ...`.
# The name is captured up to the next `'` (a rig source name — "ASIO Input Capture", "mbc",
# "post video", "test-audio" — never contains an apostrophe; a hypothetical apostrophe-carrying name
# simply fails to match and is skipped, never a fabricated reading). The trailing `: ts_lag_ms=`
# anchor makes the summary line `audio-telemetry #800: total_buffering=...` (no quoted name) never
# match. ts_lag_ms may be negative (-1 == audio_ts==0, i.e. no audio timeline yet).
_AUDIO_TS_LAG_RE = re.compile(r"audio-telemetry #800 '([^']*)': ts_lag_ms=(-?\d+)")

# camera-box #1325 — the `buffered_ms` field of the SAME #800 line (obs-audio.c:698). It is the
# honest signal for the mbc (Dante/ASIO) source's audio-timeline drift against the OBS mix clock:
# on the stream box `buffered_ms` drains ~1.1 ms/min (the ≈ −18 ppm Dante-GM-vs-UTC floor the ASRC
# fails to hold out of the buffer) then JUMPS +20…+57 ms when OBS re-buffers — a sawtooth that
# makes every dock/E2E A/V-offset reading wander ±30–50 ms. Captures (src, buffered_ms) so the
# per-reference-source drift/step verdict can watch ONE named source (default mbc), never a
# max-across-sources scalar (a per-source drift is invisible in a global max).
_AUDIO_800_BUFFERED_RE = re.compile(
    r"audio-telemetry #800 '([^']*)': ts_lag_ms=-?\d+ buffered_ms=(-?\d+)")
BUFFERED_MS_DEFAULT_SRC = "mbc"


# #1231 — freshness/recency for the audio-lag facet (follow-up to the #1226 review finding W1). The
# #1226 facet took the LAST reading PER source with NO age bound, so a source removed/renamed while
# LAGGING kept its stale-high line winning the MAX until the log rotated (concern a), and a telemetry
# tick that STOPPED while the OBS log kept advancing read as healthy (concern b). We add a purely
# IN-LOG relative recency (the ndi_halving_decision.ts_to_seconds + midnight-wrap precedent, MIRRORED
# here so the box's gather never imports a dev1-only decision module): each source's newest #800 line
# is aged against the newest parseable timestamp of ANY line in the tail (the log's current write
# head). No wall clock is injected, so this stays a pure fixture-testable parser and never
# mis-compares a date-less OBS timestamp against a foreign clock (issue 1231 design Prístup 1).
AUDIO_TS_LAG_STALE_AFTER_S = 180  # ~3x the 60 s emit period: a source silent this long is stale.

# #1265 — the per-REFERENCE-source ts_lag BAND facet. The #1226/#1231 facet is a single
# MAX-across-sources scalar graded at a 5000 ms page threshold, which is structurally blind to the
# A/V-gate reference source (`mbc`) going BIMODAL (flat ~107 ms then flapping 107↔180 ms, high mode
# creeping up) — a 23×-under-threshold drift that still shifts the measured A/V residual past the
# ±90 gate (issue 1265). `audio_ref_band_from_log` reads the SAME #1222 bounded head+tail log ONCE
# and, for the named reference source, computes the band SHAPE at tens-of-ms resolution; the dev1
# `classify_band` decision thresholds it, ships DISABLED like every sibling watchdog.
AUDIO_REF_BAND_DEFAULT_SRC = "mbc"     # the stream A/V-gate audio reference (cam2 HDMI -> hand1 mic)
AUDIO_REF_BAND_DUTY_MARGIN_MS = 20     # a tail reading > baseline + this counts as "high mode"
AUDIO_REF_BAND_WINDOW_CAP = 120        # bound the tail-window readings considered (recent state)


def audio_telemetry_from_log(text, stale_after_s=AUDIO_TS_LAG_STALE_AFTER_S):
    """#1231 — the audio-timeline facet WITH a freshness dimension. Returns
    `(max_fresh_lag_ms_str, src, age_s_str)`:

    * `max_fresh_lag_ms_str`/`src` — the MAX per-source lag exactly as #1226, but EXCLUDING any
      source whose newest #800 line sits more than `stale_after_s` behind the tail's newest line of
      ANY kind (concern a: a removed/renamed lagging source no longer drives the reading). `("","")`
      when no FRESH positive reading remains. `ts_lag_ms=-1` (no audio timeline yet) is still
      excluded; the tie-break stays deterministic (alphabetically-first source) so the value never
      flaps the watchdog dedup key.

    * `age_s_str` — the whole-second in-log age of the freshest #800 line behind the tail's newest
      line of any kind (concern b: telemetry that stopped while the log advanced). `""` ONLY when
      there is NO #800 line at all (absent -> UNKNOWN downstream, never a fabricated age). A fresh
      box reports `"0"`; a stalled tick reports a large value -> the dev1 decision surfaces STALE.

    Why this facet (the 2026-08-30 incident, #1226): stream OBS's audio pipeline fell ~24 s/min
    behind realtime under stream load; every audio source lagging EQUALLY = a global audio-tick/mix
    pipeline behind realtime (mbc peaked at 1 672 741 ms / 27,9 min), which desynced the YouTube
    stream's A/V for a whole service. This line SCREAMED it the whole hour but nothing read it.

    Reads ONLY the TAIL slice of the #1222 bounded head+separator+tail read (a stale HIGH value that
    survives only in the head is never reported; a small whole-file log is scanned entirely), in ONE
    pass (no second log read)."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    last_per_source = {}   # name -> (ts_or_None, lag_int) : the NEWEST #800 line per source
    # The OBS log is APPEND-ONLY, so FILE ORDER IS TIME ORDER: the log's current write head is the
    # LAST parseable line, and the freshest telemetry is the LAST #800 line — NOT the max
    # seconds-of-day (which, across midnight, anchors to a pre-midnight line and reads a genuinely
    # stale source as fresh; issue 1231 review W1). Overwriting as we iterate takes the file-order
    # last; `_recency_gap_s` then corrects a single midnight wrap, so the gap is the TRUE elapsed
    # time (mod 24h) — a real multi-minute/hour stall is reported honestly, never snapped to fresh.
    log_newest_ts = None   # ts of the LAST parseable line in file order (the log write head)
    last_800_ts = None     # ts of the LAST #800 line in file order (the freshest telemetry line)
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _AUDIO_TS_LAG_RE.search(line)
        if m:
            last_per_source[m.group(1)] = (ts, int(m.group(2)))
            if ts is not None:
                last_800_ts = ts
    if not last_per_source:
        return ("", "", "")   # no #800 line at all -> absent (UNKNOWN downstream)

    # (concern b) age of the freshest #800 line behind the log head. `_recency_gap_s` is in [0,86400)
    # by construction (a single +86400 wrap correction), so no upper clamp is needed or wanted — a
    # >1h stall is a REAL fault to surface, never a "wrap artifact" to hide. "0" only when neither
    # timestamp is parseable (a pathological prefix-less log), the conservative unmeasurable case.
    gap = _recency_gap_s(log_newest_ts, last_800_ts)
    age_s = "0" if gap is None else str(round(gap))

    # (concern a) per-source staleness filter for the MAX: drop any source whose newest #800 line is
    # more than stale_after_s behind the log head.
    candidates = []
    for name, (ts, lag) in last_per_source.items():
        if lag < 0:
            continue           # -1 == no audio timeline yet, never a lag
        g = _recency_gap_s(log_newest_ts, ts)
        if g is not None and g > stale_after_s:
            continue           # this source went silent while the log advanced -> stale, drop it
        candidates.append((lag, name))
    if not candidates:
        return ("", "", age_s)   # no FRESH positive reading; the age carries the staleness signal
    # max lag; deterministic tie-break by source name (asc) so the reported src is stable.
    candidates.sort(key=lambda kv: (-kv[0], kv[1]))
    maxv, maxname = candidates[0]
    return (str(maxv), maxname, age_s)


def audio_ts_lag_ms_from_log(text):
    """#1226 — the MAX per-source audio-timeline lag `(max_lag_ms_str, src)`, `("", "")` when none.
    A thin wrapper over `audio_telemetry_from_log` (#1231) that drops the freshness age. Behaviour is
    unchanged EXCEPT that a source gone stale in the tail (silent > a few emit periods while the log
    advanced) is now excluded from the max (concern a). See `audio_telemetry_from_log` for the full
    contract."""
    lag, src, _age = audio_telemetry_from_log(text)
    return (lag, src)


def _median_int(values):
    """Plain sorted median of a non-empty int list, rounded to int. (No numpy — small lists.)"""
    s = sorted(values)
    n = len(s)
    mid = n // 2
    return int(s[mid]) if n % 2 else int(round((s[mid - 1] + s[mid]) / 2.0))


def _percentile_nearest_rank(sorted_vals, pct):
    """Nearest-rank percentile of a NON-EMPTY sorted list; `pct` in [0,100]. index =
    ceil(pct/100 * n) - 1, clamped to [0, n-1]. No interpolation. NOTE: for a small n the p90 index
    IS the max (n<=9 -> ceil(0.9n)-1 == n-1), so this only ignores a lone top spike once the window
    has enough samples — the dev1 decision's BAND_MIN_SAMPLES (10) is what guarantees p90!=max, not
    this function alone (issue 1265 review finding 2)."""
    n = len(sorted_vals)
    k = max(0, min(n - 1, int(math.ceil(pct / 100.0 * n)) - 1))
    return sorted_vals[k]


def _ref_readings(text, ref_src):
    """Every non-negative `ts_lag_ms` reading for `ref_src` in `text`, in FILE ORDER. A negative
    reading (-1 == no audio timeline yet) never contributes (matching the #1226 max-side filter)."""
    out = []
    for line in (text or "").splitlines():
        m = _AUDIO_TS_LAG_RE.search(line)
        if m and m.group(1) == ref_src:
            v = int(m.group(2))
            if v >= 0:
                out.append(v)
    return out


def audio_ref_band_from_log(text, ref_src=AUDIO_REF_BAND_DEFAULT_SRC,
                            duty_margin_ms=AUDIO_REF_BAND_DUTY_MARGIN_MS,
                            window_cap=AUDIO_REF_BAND_WINDOW_CAP):
    """#1265 — the BAND SHAPE of one reference source's `ts_lag_ms` over the #1222 bounded log.
    Returns `(src, base_ms, high_ms, low_ms, duty_pct, n)` as STRINGS (the omit-when-empty facet
    contract), or `("", "", "", "", "", "")` when `ref_src` has no readings at all.

    * `base_ms` — the flat-start baseline: median of `ref_src`'s readings in the HEAD (startup)
      region of the bounded read (the log's own beginning — "the instance's own flat start", the
      issue's wording). `""` when there is no separator (a small whole log with no distinct startup
      region) or the head carried no `ref_src` reading — the dev1 decision then falls back to the
      tail low as its deviation baseline, so a within-window bimodal flap is still caught.
    * `high_ms`/`low_ms` — p90/p10 (nearest-rank) of the FRESH tail-window readings (the last
      `window_cap`), the current high/low modes. p90 ignores a lone top spike only once the window
      has >= ~10 samples; the dev1 decision's BAND_MIN_SAMPLES guards the small-n case (finding 2).
    * `duty_pct` — % of the tail window sitting above `baseline + duty_margin_ms`, where
      `baseline = min(base, low)` (the flat-start median AND the current low mode, whichever is
      LOWER). Using the MIN matters when the head is ALSO in the high mode (a restart straight into
      the bad state): a `base` that is itself elevated would mask the drift, so the tail low keeps
      the duty honest (issue 1265 review finding 3). This separates a genuine bimodal flap (duty
      ~50%) from a single transient spike (duty ~few %).
    * `n` — the fresh tail-window sample count (too few -> the dev1 decision reads UNKNOWN, never a
      false page).

    Reads the SAME #1222 head+separator+tail bounded text in ONE pass — no second log read. When
    the separator is present the region BEFORE it is the head (flat start) and the region AFTER it
    is the tail (current window); the window is the TAIL only — if the tail carries no `ref_src`
    reading (mbc telemetry stopped hours ago while the log advanced, the #1231 STALE case) the band
    is all-empty (UNKNOWN downstream), never the hours-old head reported as the current window. A
    small whole-file log (no separator) has no distinct head, so its whole content is the tail and
    `base_ms` is empty."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        head_text, tail_text = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)
    else:
        head_text, tail_text = "", t
    head_vals = _ref_readings(head_text, ref_src)
    tail_vals = _ref_readings(tail_text, ref_src)
    window = tail_vals[-window_cap:]
    if not window:
        return ("", "", "", "", "", "")
    sw = sorted(window)
    n = len(window)
    high = _percentile_nearest_rank(sw, 90)
    low = _percentile_nearest_rank(sw, 10)
    base = _median_int(head_vals) if head_vals else None
    # baseline = the LOWER of the flat-start median and the current tail low (finding 3): a head
    # that is itself elevated must never mask a drifting tail.
    baseline = min(base, low) if base is not None else low
    thresh = baseline + duty_margin_ms
    high_count = sum(1 for v in window if v > thresh)
    duty_pct = int(round(100.0 * high_count / n))
    return (
        ref_src,
        str(base) if base is not None else "",
        str(high),
        str(low),
        str(duty_pct),
        str(n),
    )


def buffered_ms_series_from_log(text, ref_src=BUFFERED_MS_DEFAULT_SRC,
                                recent_window_s=AV_OFFSET_RECENT_WINDOW_S):
    """camera-box #1325 — the `buffered_ms` DRIFT/STEP shape for ONE named #800 source (default mbc),
    the honest signal for the audio-timeline drift the ASRC servo fails to hold out of the mix buffer.
    Returns `(slope_ms_per_min_str, max_step_ms_str, n_str, age_s_str)`, all "" when the ref source
    has < 2 buffered readings in the recent window (UNKNOWN downstream, never a fabricated 0):

    * slope_ms_per_min — the linear DRIFT of buffered_ms over the recent window, in ms/min (1 dp).
      Computed as (last − first) / span_minutes over the freshest recent_window_s of readings; a
      steady drain reads a small NEGATIVE slope (tonight ≈ −1.1). It is deliberately the endpoint
      slope over a long window (not a per-step delta) so the +20…+57 ms refill JUMPS do not swamp
      the underlying drain trend (the STEP term below carries the jumps).
    * max_step_ms — the LARGEST single SIGNED delta between consecutive readings in the window. A
      large POSITIVE value is the OBS re-buffer refill step (tonight +21…+49) — the decision gates
      STEP on `max_step_ms >= BUFFERED_STEP_MS`, so a pure drain (only negative deltas -> a negative
      max) never trips STEP. A large value here = OBS is periodically re-buffering the source, the
      sawtooth the dock/E2E inherit.
    * n — buffered readings in the recent window; span guards the slope (see the decision module).

    Reads ONLY the #1222 bounded TAIL, `_recency_gap_s` recency (file order IS time order), one pass,
    no wall clock, no second log read. `ts_lag_ms=-1` lines (no audio timeline) still carry a real
    buffered_ms and are kept — buffered_ms is a queue depth, valid regardless of the timeline state."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    samples = []          # (ts_or_None, buffered_int) for the ref source, in file order
    log_newest_ts = None
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _AUDIO_800_BUFFERED_RE.search(line)
        if m and m.group(1) == ref_src:
            samples.append((ts, int(m.group(2))))
    # Keep only the freshest recent_window_s of readings (drop stale head-region readings the same
    # way the quality/series parsers do — a settled steady state is what we judge, not startup).
    recent = []
    for ts, buffered in samples:
        g = _recency_gap_s(log_newest_ts, ts)
        if g is None or g > recent_window_s:
            continue
        recent.append((ts, buffered))
    if len(recent) < 2:
        return ("", "", "", "")
    first_ts, first_buf = recent[0]
    last_ts, last_buf = recent[-1]
    # span in seconds between the oldest and newest recent reading (midnight-wrap corrected). The
    # freshness age is the newest reading's own gap behind the log head.
    span_s = _recency_gap_s(last_ts, first_ts)
    slope_str = ""
    if span_s is not None and span_s > 0:
        slope_str = f"{(last_buf - first_buf) / (span_s / 60.0):.1f}"
    max_step = None
    prev = None
    for _ts, buffered in recent:
        if prev is not None:
            step = buffered - prev
            if max_step is None or step > max_step:
                max_step = step
        prev = buffered
    age_gap = _recency_gap_s(log_newest_ts, last_ts)
    age_s = "0" if age_gap is None else str(round(age_gap))
    return (
        slope_str,
        "" if max_step is None else str(max_step),
        str(len(recent)),
        age_s,
    )


# issue 1381 -- the audio MIXER real-time facet + the obs-vban PACER loss facet. On 27.9.2026 the
# resolume cg OBS mixer left real time from 06:00 and both VBAN outputs lost audio for over an hour
# before FOH heard it; both signals were already in the log, read by nothing.
#
# `audio-stall #1367:` (obs-audio.c) is dumped from the audio callback once >= 60 s have passed on
# the audio thread's clock, with that window's tick count, then reset. Real time at 48 kHz is
# 2812.5 ticks/min; the first dump after an OBS start is partial (ticks=1), so a reading needs the
# previous dump too (the window it closes).
_AUDIO_STALL_RE = re.compile(
    r"audio-stall #1367: tick_gap_max_ms=\S+ callback_max_ms=\S+ ticks=(\d+) ticks_over=(\d+) "
    r"tick_ms=([\d.]+)")


def audio_mixer_from_log(text, tail=None):
    """issue 1381 -- `(ticks, ticks_over, window_ms, tick_ms, age_s)` of the NEWEST
    `audio-stall #1367` dump in the tail, or five `""` when fewer than two dumps are there (the
    first dump after an OBS start is partial, so a normal start reads absent -> UNKNOWN, never
    BEHIND). The dump's own tick count is already the per-minute rate (its window is 60 s on the
    audio thread's clock); `window_ms` is only the WALL-clock log interval since the previous dump,
    reported as context and never used to rescale the count (dantesync steps the wall clock).
    `age_s` is the newest dump's in-log age behind the log head (a stopped audio thread stops
    dumping while the log advances). `tail` = a precomputed `timestamped_tail_lines(text)`.

    issue 1385 -- a stopped thread in a LONG session: once the last dumps leave the 5 MB tail,
    the count is gone but the age is not. A bounded read (the file is larger than head + tail, so
    the session is far past its start) whose HEAD slice holds a dump proves this build dumps; a
    tail with fewer than two dumps then reports ONLY `age_s`: the newest tail dump's age, or,
    with none left, the whole tail span (the newest dump is at least that old). The dev1 decision
    grades that age as STALE / STALLED, so a thread that stays dead keeps paging instead of
    decaying to UNKNOWN. A whole-file log (a normal start) and a build without the probe (no dump
    anywhere) stay absent."""
    stamped, head = tail if tail is not None else timestamped_tail_lines(text)
    prev = last = None
    for pos, line in stamped:
        if "audio-stall #1367" not in line:
            continue
        m = _AUDIO_STALL_RE.search(line)
        if m:
            prev, last = last, (pos, m.group(1), m.group(2), m.group(3))
    if prev is None and head is not None and _audio_stall_dump_in_head(text):
        newest = last[0] if last is not None else 0.0
        return ("", "", "", "", str(round(head - newest)))
    if prev is None or last is None or last[0] <= prev[0]:
        return ("", "", "", "", "")
    return (last[1], last[2], str(round((last[0] - prev[0]) * 1000.0)), last[3],
            str(round(head - last[0])))


def _audio_stall_dump_in_head(text):
    """True only for a BOUNDED read whose head slice (the session's first 2 MB, before the
    separator) holds a complete `audio-stall #1367` dump line: this build has the probe and its
    audio thread dumped earlier in the session (issue 1385)."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR not in t:
        return False
    head_slice = t.partition(LOG_BOUNDED_READ_SEPARATOR)[0]
    return _AUDIO_STALL_RE.search(head_slice) is not None
