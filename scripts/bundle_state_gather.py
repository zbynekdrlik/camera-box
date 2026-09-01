#!/usr/bin/env python3
"""#650 — PURE parsers + builders for the standing :8899 bundle-state HTTP service.

This module holds every piece of `scripts/bundle-state-server.py` that can be exercised
WITHOUT a live Windows box / live OBS — the same PURE-function-vs-flow split this repo already
uses in `scripts/drift-guard.sh` (parsers unit-tested by sourcing the script; the executed flow
is verified live on the rig) and `scripts/obs_burn_filter.py` (`compute_burn_on` pure, `cmd_check`/
`cmd_add` driven through a fake `_rpc` in tests/python/test_obs_burn_filter.py).

Why this exists (#650): `scripts/version-integrity-gate.sh --win-state` (#123/#119) and
`scripts/recording-e2e.sh`'s `fetch_box_state()` expect a live `http://<box>:8899/bundle-state.json`
serving the drift-guard `--compare` OBSERVED values (see `version-integrity-gate.sh`'s own doc
comment for the exact flat-JSON schema). Historically those values were gathered BY HAND, per
`.claude/commands/drift-guard.md` step 1/1b/1c, by an operator/agent holding the win-* MCP — fine
for an interactive `/drift-guard` run, but the automatic `pull_request`-triggered full-path-e2e CI
run (#406/#312 item5) has neither a human nor MCP access, so the gate always saw both boxes UNKNOWN
(exit 11) and refused. This module + `bundle-state-server.py` gather the SAME values, on-box,
so a standing service can answer the gate unattended.

Only the drift-guard `--compare` keys `version-integrity-gate.sh` MANDATORILY checks (i.e. the ones
that are NOT opt-in behind a `manifest=`/`burn_env=`/`genlock_source_latency=` key — see
`compare_observed()` in `scripts/drift-guard.sh`) are gathered here: `obs_version`,
`distroav_version`, `ndi_runtime`, `output_fps`, `genlock_wall_clock`, `ndi_input_latency`,
`distroav_dll_paths`. `genlock_capability` is ALSO gathered (harmless — it is only ever consulted
by the engine when a `manifest=` is supplied, which the current CI invocation never does) so the
bundle-state payload stays forward-compatible with the opt-in build-SHA facet without any schema
change later.
"""
from __future__ import annotations

import hashlib
import os
import re
import sys

# The three OBS module scan paths that can each shadow-load a `distroav.dll` (#124, EPIC #125) —
# mirrors `.claude/commands/drift-guard.md` step 1c EXACTLY (same three roots, same rationale: a
# second copy in any of these can silently shadow the intended genlock build, #119).
DISTROAV_SCAN_ROOTS = (
    r"C:\Program Files\obs-studio\obs-plugins\64bit",
    r"C:\ProgramData\obs-studio\plugins",
    # %APPDATA% is resolved by the caller (this module stays free of env lookups so it is
    # trivially testable against a tmp_path tree); see bundle-state-server.py's gather step.
)


def obs_version_from_log(text):
    """"OBS 32.1.2 (64-bit, windows)" -> "32.1.2". "" if the log never printed it (UNKNOWN, per
    drift-guard's never-a-false-clean contract — the caller simply omits the key)."""
    m = re.search(r"OBS (\d+\.\d+\.\d+)", text or "")
    return m.group(1) if m else ""


def distroav_version_from_log(text):
    """"DistroAV (Version 6.2.1)" -> "6.2.1". "" if absent."""
    m = re.search(r"DistroAV \(Version (\d+\.\d+\.\d+)\)", text or "")
    return m.group(1) if m else ""


def output_fps_from_log(text):
    """The `fps:` line INSIDE the first "video settings reset:" block -> "30". "" if either the
    reset block or its fps line is absent. Mirrors `.claude/commands/drift-guard.md` step 1's
    PowerShell block-scoped scan line-for-line (first reset block only, first fps line inside it)."""
    lines = (text or "").splitlines()
    for i, line in enumerate(lines):
        if "video settings reset:" in line:
            for later in lines[i:]:
                m = re.search(r"fps:\s+(\d+)/", later)
                if m:
                    return m.group(1)
            break  # found the reset block but no fps line followed it — UNKNOWN, don't scan past it
    return ""


def genlock_wall_clock_from_log(text):
    """"1" (render tick ENABLED), "0" (DISABLED), "" if the build never logged the marker at all
    (a stock OBS, or OBS never launched — UNKNOWN, never guessed)."""
    t = text or ""
    if re.search(r"genlock:.*render tick ENABLED", t):
        return "1"
    if re.search(r"genlock:.*render tick DISABLED", t):
        return "0"
    return ""


def genlock_capability_from_log(text):
    """Every `genlock:` capability-marker line (render tick ENABLED / sub-frame jitter reserve /
    timestamp-aligned release) joined with '\\n' — the #122 build-unique tell. "" if the build
    emits none (a stock OBS). Gathered for forward-compat with the opt-in manifest/capability
    facet in drift-guard.sh; harmless when no manifest= is supplied (the current CI invocation)."""
    pattern = re.compile(r"genlock:.*(render tick ENABLED|sub-frame jitter reserve|timestamp-aligned release)")
    matches = [line for line in (text or "").splitlines() if pattern.search(line)]
    return "\n".join(matches)


# #1222 — the strih bundle-state gather's latency grew LINEARLY with the live OBS log size: a
# ~13h session (75 MB log) made every *_from_log parser above re-scan the WHOLE file on EVERY
# /bundle-state.json request (~0.25 s/MB measured, +19 s at 75 MB), pushing gather past
# recording-e2e.sh's `curl --max-time 30` and refusing the [0/8] version-integrity gate. Every
# fact these parsers need lives at the EDGES of the log, never the middle: the startup banner
# (obs_version / distroav_version / the first "video settings reset:" fps line / the FIRST
# genlock capability markers) is written once at process start, and the "current state" a caller
# might care about (the newest genlock capability marker) is always in the most recent lines. So
# bound the read to a HEAD slice (startup banner) + a TAIL slice (newest state), independent of
# how large the file has grown.
LOG_HEAD_BYTES = 2 * 1024 * 1024  # ~2 MB — a wide margin over the startup banner (#1222 measured
                                   # it sitting in the first few KB of a real log in practice).
LOG_TAIL_BYTES = 5 * 1024 * 1024  # ~5 MB — the newest state a caller might need (e.g. the latest
                                   # genlock capability marker).
# A separator that can never fake a real log line: no digits (so it can never satisfy a
# `\d+\.\d+\.\d+` / `fps:\s+\d+/` style pattern above), no colon-prefixed keyword any parser
# scans for ("OBS ", "DistroAV (Version", "video settings reset:", "genlock:"), and newline-padded
# on both sides so a byte-cut mid-line on either side of the join can never merge into something a
# parser could mistake for a real one.
# #1222 review: verified digit-free by construction (a future unanchored `\d+`-style parser
# would violate the claim above otherwise) -- keep it that way if this text ever changes.
LOG_BOUNDED_READ_SEPARATOR = (
    "\n\n===== bounded log read: middle omitted (head+tail only) =====\n\n"
)


def read_bounded_log_text(path, head_bytes=LOG_HEAD_BYTES, tail_bytes=LOG_TAIL_BYTES):
    """#1222 — the raw text of *path*, bounded to at most `head_bytes + tail_bytes +
    len(LOG_BOUNDED_READ_SEPARATOR)` characters, regardless of the file's actual size. A file no
    larger than `head_bytes + tail_bytes` is returned WHOLE, byte-for-byte (no separator, no
    truncation) — the common case for a freshly-started OBS session and for every existing test
    fixture in this suite. A larger file returns its first `head_bytes` bytes joined to its last
    `tail_bytes` bytes via LOG_BOUNDED_READ_SEPARATOR (see that constant's own doc comment for why
    it can never be mistaken for a real log line by any parser above). Read in BINARY mode and
    decoded with `errors="replace"` — a byte-boundary cut mid multi-byte UTF-8 character degrades
    to a harmless U+FFFD, never a crash. Unlike the original whole-file text-mode read this
    replaces, a Windows CRLF line ending is NOT translated to a bare `\n` here; every parser above
    is CRLF-tolerant (`splitlines()` strips a trailing `\r`, and no regex here crosses a line
    boundary), so this has no observed behavioral effect, but it is a real difference worth
    knowing if a future parser is added.

    "" if *path* is missing/unreadable — the same UNKNOWN-downstream contract as the whole-file
    read this replaces (callers already treat an empty log text as every derived facet coming
    back empty)."""
    try:
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            if size <= head_bytes + tail_bytes:
                return f.read().decode("utf-8", errors="replace")
            head = f.read(head_bytes)
            f.seek(size - tail_bytes)
            tail = f.read(tail_bytes)
    except OSError as e:
        print(f"WARNING: read_bounded_log_text: could not read {path!r}: {e}", file=sys.stderr)
        return ""
    return (
        head.decode("utf-8", errors="replace")
        + LOG_BOUNDED_READ_SEPARATOR
        + tail.decode("utf-8", errors="replace")
    )


# #1226 — the audio-timeline-lag telemetry line vendored OBS emits every 60 s per audio source
# (vendor/obs-studio/libobs/obs-audio.c:698): `audio-telemetry #800 '<src>': ts_lag_ms=<int64> ...`.
# The name is captured up to the next `'` (a rig source name — "ASIO Input Capture", "mbc",
# "post video", "test-audio" — never contains an apostrophe; a hypothetical apostrophe-carrying name
# simply fails to match and is skipped, never a fabricated reading). The trailing `: ts_lag_ms=`
# anchor makes the summary line `audio-telemetry #800: total_buffering=...` (no quoted name) never
# match. ts_lag_ms may be negative (-1 == audio_ts==0, i.e. no audio timeline yet).
_AUDIO_TS_LAG_RE = re.compile(r"audio-telemetry #800 '([^']*)': ts_lag_ms=(-?\d+)")

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

_LOG_LINE_TS_RE = re.compile(r"^\s*(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?")


def _log_line_seconds(line):
    """The leading OBS-log `HH:MM:SS[.mmm]` prefix of *line* -> seconds-of-day float, or None when
    the line does not begin with a real clock time (a continuation/blank line -> no timestamp, never
    a guessed one). Mirror of ndi_halving_decision.ts_to_seconds, kept LOCAL so bundle_state_gather
    (which runs on the box) never imports a dev1-only decision module (issue 1231)."""
    m = _LOG_LINE_TS_RE.match(line)
    if not m:
        return None
    h, mm, s = int(m.group(1)), int(m.group(2)), int(m.group(3))
    if h > 23 or mm > 59 or s >= 60:
        return None
    frac = float("0." + m.group(4)) if m.group(4) else 0.0
    return h * 3600 + mm * 60 + s + frac


def _recency_gap_s(newest_ref, ts):
    """Seconds *ts* sits behind *newest_ref* (both seconds-of-day), midnight-wrap-corrected, or None
    when either is missing. A negative raw gap means the tail straddled midnight (date-less log), so
    +86400; an implausibly large result (a wrap artifact) is left for the caller to guard against."""
    if newest_ref is None or ts is None:
        return None
    gap = newest_ref - ts
    if gap < 0:
        gap += 86400.0
    return gap


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

# #1267 — rolling-window bounds, in-log seconds behind the log head. RECENT = the freshest 10 min;
# BASELINE = the 10..40 min region behind it (a rolling reference that predates the recent window).
AV_OFFSET_RECENT_WINDOW_S = 600
AV_OFFSET_BASELINE_WINDOW_S = 2400
# #1267 — a dock series whose freshest line sits more than this behind the log head has STOPPED while
# the log kept advancing -> STALE downstream (never a false step). ~10x the ~30 s SUGGESTED cadence.
AV_OFFSET_STALE_AFTER_S = 300


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


def distroav_dll_paths(scan_roots):
    """Every `distroav.dll` found (case-insensitive) under *scan_roots* (each walked recursively),
    comma-joined, in the order given. "" if none found anywhere (UNKNOWN — never a false clean;
    drift_check_plugin_paths in drift-guard.sh already treats an empty observed set this way)."""
    found = []
    for root in scan_roots:
        if not root or not os.path.isdir(root):
            continue
        for dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if name.lower() == "distroav.dll":
                    found.append(os.path.join(dirpath, name))
    return ",".join(found)


def ndi_input_latency_csv(ndi_inputs):
    """*ndi_inputs* is `{name: {"settings": {...}, ...}}` (the exact shape
    `~/.cache/obsprobe/obs_inputs.py` / `bundle-state-server.py`'s WS gather produces). Returns a
    sorted `"name=latency,..."` CSV of every GENLOCKED BROADCAST-PATH input — i.e. every NDI input
    whose settings carry `genlock_fifo: true` (the live marker for "this is a genlock-managed
    program/camera-ingest input", proven on strih + stream 2026-07-10: it selects exactly the
    camera ingests + program feed and excludes preview/CG/lyrics inputs, matching
    `.claude/commands/drift-guard.md`'s documented "genlocked broadcast-path inputs only" scope
    WITHOUT hardcoding scene/input names that would go stale as scenes are edited).
    An input with `genlock_fifo=true` but no readable `latency` setting is skipped (never a
    fabricated value) — drift_check_inputs then simply sees one fewer entry, not a wrong one.
    "" if there are no genlocked inputs at all (UNKNOWN downstream, never a silent clean)."""
    pairs = []
    for name, info in (ndi_inputs or {}).items():
        settings = (info or {}).get("settings") or {}
        if settings.get("genlock_fifo") is not True:
            continue
        if "latency" not in settings:
            continue
        pairs.append((name, str(settings["latency"])))
    pairs.sort(key=lambda kv: kv[0])
    return ",".join(f"{name}={latency}" for name, latency in pairs)


# #826 — filename pattern for a launchable OBS-shaped executable: `obs<digits>.exe` (obs64.exe,
# obs32.exe, the pinned genlock build's own name) OR a legacy `<name>ME.exe`-style build (the
# pre-genlock era's own naming, e.g. a literal "2ME.exe"). Case-insensitive — Windows filenames.
_OBS_EXE_RE = re.compile(r"(?i)^(obs\d*\.exe|\S*me\.exe)$")


def obs_installs_under(scan_roots):
    """#826 — every launchable OBS-shaped executable found under *scan_roots* (each walked
    recursively), sorted (case-insensitively) and comma-joined. Mirrors `distroav_dll_paths`'s
    walk-and-collect shape exactly (same "PURE, fed real filesystem roots" pattern already
    established in this module).

    A folder renamed aside (e.g. `D:\\_APPS\\_RETIRED_1ME-obs_2026-07-27`) is STILL walked and its
    exe is STILL reported — this is the whole point of the #826 acceptance: renaming a dormant
    install out of the way is not the same as removing it, and it can still be launched by hand
    (the exact 2026-07-27 incident: an agent ran a dead-variable-referenced `.lnk` and woke a
    year-old OBS 31.1.2, which then squatted TCP :4455 before the pinned genlock build could).

    "" when no *scan_roots* entry exists or none contains a match (never guessed)."""
    found = []
    for root in scan_roots:
        if not root or not os.path.isdir(root):
            continue
        for dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if _OBS_EXE_RE.match(name):
                    found.append(os.path.join(dirpath, name))
    return ",".join(sorted(found, key=str.lower))


# #826 / #1222c — the ONE canonical "is this an OBS-shaped process name" pattern (obs64, obs32,
# bare obs — case-insensitive), shared between obs_process_count_from_listing below and
# bundle-state-server.py's _parse_tasklist_obs_process_names (a #1222c review finding: the two
# used to carry independent copies of the identical regex, a DRY violation that could silently
# drift apart on a future rename).
OBS_PROCESS_NAME_RE = re.compile(r"(?i)^obs\d*$")


def obs_process_count_from_listing(text):
    """#826 — count of currently-running OBS-class processes, from a plain newline-separated list
    of process NAMES (no `.exe` suffix — the shape `Get-Process | Select-Object -ExpandProperty
    Name` produces on Windows). Matches `obs<digits>` case-insensitively (obs64, obs32, bare obs)
    via the shared OBS_PROCESS_NAME_RE above.

    "" (never "0") when *text* itself is empty/unread — an unreachable box must read UNKNOWN, not
    a false "zero processes confirmed running" (the same never-a-false-clean discipline every
    other facet in this module follows)."""
    if not (text or "").strip():
        return ""
    count = 0
    for line in text.splitlines():
        if OBS_PROCESS_NAME_RE.match(line.strip()):
            count += 1
    return str(count)


# #826 — NL_STARTUP.ahk's own variable syntax (confirmed live on strih, issue #826 comments):
#   app1_run  := 1
#   app1_path := "C:\ProgramData\...\OBS Studio.lnk"
#   app1_binarypath := "D:\_APPS\1ME-obs\1ME.lnk"     <- the dead leftover that caused the incident
#   app2_run  := 0
#   app2_path := "D:\_APPS\2ME-obs\2ME.lnk"
def ahk_app1_shortcut_path(text):
    """#826 — the `app1_path := "..."` shortcut NL_STARTUP.ahk launches. Only the FIRST match is
    used (AHK assigns each variable once). "" when absent — this box has no NL_STARTUP.ahk at all
    (only strih runs it; stream has none, per `.claude/skills/obs-ops`), or the text is unread."""
    m = re.search(r'app1_path\s*:=\s*"([^"]*)"', text or "")
    return m.group(1) if m else ""


def ahk_app1_run(text):
    """#826 — the `app1_run := N` flag: "1" enabled / "0" disabled / "" if the line is absent
    (no NL_STARTUP.ahk on this box, or unread)."""
    m = re.search(r"app1_run\s*:=\s*(\d+)", text or "")
    return m.group(1) if m else ""


def ahk_dead_config_present(text):
    """#826 — "1" when NL_STARTUP.ahk still carries the dead `app1_binarypath` leftover (the exact
    variable an agent mistook for the box's canonical launcher during the #826 incident) OR an
    ENABLED `app2_run := 1` block (the issue's "config states one truth" cleanup requirement).
    "0" when the text was read and neither leftover is present. "" (UNKNOWN, distinct from "read
    and clean") when there is no AHK text to read at all — e.g. this box has no NL_STARTUP.ahk."""
    t = text or ""
    if not t.strip():
        return ""
    has_dead_binarypath = "app1_binarypath" in t
    m = re.search(r"app2_run\s*:=\s*(\d+)", t)
    app2_enabled = bool(m and m.group(1) == "1")
    return "1" if (has_dead_binarypath or app2_enabled) else "0"


def record_dir_stats(record_dir):
    """#652: PURE, testable filesystem stats over the top-level files of *record_dir* (the OBS
    record directory) — powers the `/record-dir-stats.json` endpoint (bundle-state-server.py),
    which recording-e2e.sh's preflight curls to WARN (never fail) when a box's accumulated E2E
    test recordings exceed a disk budget. The live incident this addresses: strih accumulated
    ~500 GB / stream ~139 GB of forgotten test recordings (back to 2026-06-17), invisible until
    the disk nearly filled (17 GB free).

    Only the TOP-LEVEL files count (OBS records flat into this directory; a subdirectory is not
    this harness's business). Never raises: an unreadable or missing directory (unmounted, wrong
    path after a profile switch, permission error) returns the same zero result a genuinely empty
    directory would — a bogus large number is worse than under-reporting, since the caller could
    otherwise fire a false "over budget" WARN from a stat() crash it half-caught. Every degrade
    path is logged (comprehensive-logging.md) rather than silently swallowed.
    """
    total_bytes = 0
    file_count = 0
    oldest_mtime = None
    try:
        with os.scandir(record_dir) as it:
            for entry in it:
                try:
                    if not entry.is_file(follow_symlinks=False):
                        continue
                    st = entry.stat(follow_symlinks=False)
                except OSError as e:
                    # A single entry vanishing mid-scan (deleted while we're iterating, e.g. an
                    # in-progress OBS write finishing) is expected and harmless — skip just that
                    # entry, never abort the whole stats gather over one transient race.
                    print(
                        f"WARNING: record_dir_stats: skipping unreadable entry in "
                        f"{record_dir!r}: {e}", file=sys.stderr,
                    )
                    continue
                total_bytes += st.st_size
                file_count += 1
                if oldest_mtime is None or st.st_mtime < oldest_mtime:
                    oldest_mtime = st.st_mtime
    except OSError as e:
        # Missing/unmounted/permission-denied directory (e.g. a stale path after a profile
        # switch) — degrade to the same zero result an empty directory would report. A bogus
        # large number from a half-caught crash would be worse than under-reporting here.
        print(
            f"WARNING: record_dir_stats: could not read directory {record_dir!r}: {e}",
            file=sys.stderr,
        )
    return {"total_bytes": total_bytes, "file_count": file_count, "oldest_mtime": oldest_mtime}


def genlock_build_sha_from_file(path):
    """#756 — the box's DEPLOYED genlock build commit SHA, read from its `GENLOCK_BUILD_SHA.txt`
    (imag: `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt`; the Windows boxes: the SAME file in the
    deployed genlock bundle). This is the value the #756 CROSS-BOX parity gate compares across the
    fleet — a peer-parity assert (every box on ONE build) that catches the stale-imag skew the
    origin/main ref-compare misses during a long-lived dev train (#530/#756).

    Returns the stripped first non-empty line, or "" when the file is missing / unreadable / empty
    (UNKNOWN downstream — never a guessed or fabricated SHA; the parity engine treats an unread box
    as INCOMPLETE and refuses, per drift-guard's never-a-false-clean contract). Only the leading
    token of the first non-blank line is kept, so a stray trailing comment/newline in the marker
    file can never leak into the compared SHA."""
    if not path:
        return ""
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if line:
                    return line.split()[0]
    except OSError as e:
        print(
            f"WARNING: genlock_build_sha_from_file: could not read {path!r}: {e}",
            file=sys.stderr,
        )
    return ""


def component_sha256(path):
    """#770 — the lowercase 64-hex sha256 of the DEPLOYED file at *path* (a plugin/core binary such
    as the live `distroav.dll` / `obs.dll`), read in binary in bounded chunks. This is the BYTE
    identity the `[0/8]` version-integrity gate compares against the #120 BUNDLE_MANIFEST — the
    truth the hand-written `GENLOCK_BUILD_SHA.txt` MARKER only POINTS at. It closes the wrong
    direction of the #119/#767 stale-bytes hole: a marker advanced to build X while the DLL bytes
    are an older build passes the marker-only cross-box parity, but its real sha256 will not match
    build X's manifest.

    Returns "" when *path* is empty/None, is not a regular file (missing, or a directory), or
    cannot be read — UNKNOWN downstream, NEVER a fabricated/zero SHA that would let a missing plugin
    read as "clean" (the same never-a-false-clean discipline every other facet in this module
    follows). The read never raises: a transient I/O error degrades to "" with a WARNING, exactly
    like `genlock_build_sha_from_file` above."""
    if not path or not os.path.isfile(path):
        return ""
    try:
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1024 * 1024), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError as e:
        print(
            f"WARNING: component_sha256: could not read {path!r}: {e}",
            file=sys.stderr,
        )
        return ""


def build_bundle_state(
    *,
    obs_version="",
    distroav_version="",
    ndi_runtime="",
    output_fps="",
    genlock_wall_clock="",
    ndi_input_latency="",
    distroav_dll_paths="",
    genlock_capability="",
    obs_dll_sha256="",
    distroav_dll_sha256="",
    genlock_build_sha="",
    obs_installs="",
    port4455_owner_path="",
    port4455_owner_version="",
    obs_process_count="",
    ahk_app1_shortcut_path="",
    ahk_app1_run="",
    ahk_dead_config_present="",
    shortcut_target_path="",
    shortcut_workdir="",
    audio_ts_lag_ms="",
    audio_ts_lag_src="",
    audio_ts_lag_age_s="",
    av_offset_recent_med_ms="",
    av_offset_base_med_ms="",
    av_offset_pin="",
    av_offset_pin_stable="",
    av_offset_age_s="",
    av_offset_n_recent="",
    av_offset_n_base="",
):
    """Assemble the flat bundle-state dict `version-integrity-gate.sh --win-state`'s
    `compare_args_from_state()` parses. Every value is a STRING (its regex requires a quoted JSON
    string — a bare number/bool would silently fail to match and read as UNKNOWN, the opposite of
    what a present-but-unread value should mean). A key whose gather came back empty is OMITTED
    entirely (cleaner payload; compare_args_from_state treats an absent key and an empty-string
    value identically — both UNKNOWN — so this is a presentation choice, not a behavior one).

    #756: `genlock_build_sha` is the box's deployed genlock build SHA — the version-integrity gate
    reads it out of every box's state and runs the CROSS-BOX parity assert (fleet must be on ONE
    build). Same omit-when-empty rule as every other facet.

    #770: `obs_dll_sha256`/`distroav_dll_sha256` are the sha256 of the DEPLOYED core/plugin BYTES
    (via `component_sha256`) — the byte identity the version-integrity gate compares against the
    #120 BUNDLE_MANIFEST (drift-guard `--compare` already consumes these keys). They make the
    GENLOCK_BUILD_SHA.txt marker just a POINTER: the truth is the bytes, closing the wrong-direction
    #119/#767 hole (marker advanced, bytes stale) the marker-only cross-box parity cannot catch.
    Same omit-when-empty rule; opt-in (#756-shape) — a box not yet reporting the SHAs is silently
    skipped, never a false clean.

    #826: the strih OBS-identity machine-check facet — `obs_installs` (every launchable OBS-shaped
    exe found), `port4455_owner_path`/`port4455_owner_version` (the process actually owning TCP
    :4455, matched by PATH not just process name — the exact hole the 2026-07-27 incident exposed),
    `obs_process_count` (must be exactly one running), and the startup-chain facts read off
    NL_STARTUP.ahk (`ahk_app1_shortcut_path`/`ahk_app1_run`/`ahk_dead_config_present`) + the
    Start-Menu shortcut's own resolution (`shortcut_target_path`/`shortcut_workdir`). Same
    omit-when-empty rule; `version-integrity-gate.sh` treats the whole group as opt-in per box
    (skipped entirely until a box's bundle-state-server reports at least one of them).

    #1226: `audio_ts_lag_ms`/`audio_ts_lag_src` = the MAX per-source audio-timeline lag (ms behind
    the OS clock) parsed from the newest `audio-telemetry #800` line per source (see
    `audio_ts_lag_ms_from_log`). Same omit-when-empty rule; the dev1 audio-lag alert watchdog
    (#1226) reads it to page when a box's audio pipeline falls sustained behind realtime.

    #1231: `audio_ts_lag_age_s` = the in-log freshness age (seconds the freshest #800 line sits
    behind the log's newest line of any kind, from `audio_telemetry_from_log`). Present ("0" when
    fresh) whenever ANY #800 line exists, "" only when telemetry is absent; a large value lets the
    dev1 decision surface a STALE (telemetry-stopped-while-log-advancing) state distinctly. The
    #1226 `audio_ts_lag_ms` now also EXCLUDES sources gone stale in the tail (concern a).

    Note: `dantesync_version` was added here for #862, then REVERTED in its own follow-up fix —
    the deployed strih/stream servers never picked up the new key (half-wired), and the gate now
    reads every node's dantesync version uniformly via `dantesync --version` over SSH instead
    (scripts/dantesync-version-gate.sh), with no bundle-state involvement at all."""
    values = {
        "obs_version": obs_version,
        "distroav_version": distroav_version,
        "ndi_runtime": ndi_runtime,
        "output_fps": output_fps,
        "genlock_wall_clock": genlock_wall_clock,
        "ndi_input_latency": ndi_input_latency,
        "distroav_dll_paths": distroav_dll_paths,
        "genlock_capability": genlock_capability,
        "obs_dll_sha256": obs_dll_sha256,
        "distroav_dll_sha256": distroav_dll_sha256,
        "obs_installs": obs_installs,
        "port4455_owner_path": port4455_owner_path,
        "port4455_owner_version": port4455_owner_version,
        "obs_process_count": obs_process_count,
        "ahk_app1_shortcut_path": ahk_app1_shortcut_path,
        "ahk_app1_run": ahk_app1_run,
        "ahk_dead_config_present": ahk_dead_config_present,
        "shortcut_target_path": shortcut_target_path,
        "shortcut_workdir": shortcut_workdir,
        "genlock_build_sha": genlock_build_sha,
        # #1226 — the audio-timeline-lag facet the dev1 audio-lag watchdog reads; same
        # omit-when-empty rule (absent facet == UNKNOWN downstream, never a fake 0).
        "audio_ts_lag_ms": audio_ts_lag_ms,
        "audio_ts_lag_src": audio_ts_lag_src,
        # #1231 — the freshness age (in-log seconds the freshest #800 line sits behind the log head);
        # present ("0" when fresh) whenever ANY #800 line exists, "" only when telemetry is absent.
        # A large value -> the dev1 decision surfaces a STALE (stopped-while-log-advancing) state.
        "audio_ts_lag_age_s": audio_ts_lag_age_s,
        # #1267 — the av-sync dock measured-offset trend the dev1 upstream-step watchdog reads: the
        # RECENT-vs-BASELINE median offset (a sustained step = a physical upstream A/V shift), the
        # CURRENT genlock pin + a pin-stability flag (a pin move -> the dev1 REPIN hold, never a
        # false step), the in-log freshness age (-> STALE when the dock stops), and the per-window
        # sample counts (too few -> UNKNOWN). Same omit-when-empty rule (absent == UNKNOWN, never 0).
        "av_offset_recent_med_ms": av_offset_recent_med_ms,
        "av_offset_base_med_ms": av_offset_base_med_ms,
        "av_offset_pin": av_offset_pin,
        "av_offset_pin_stable": av_offset_pin_stable,
        "av_offset_age_s": av_offset_age_s,
        "av_offset_n_recent": av_offset_n_recent,
        "av_offset_n_base": av_offset_n_base,
    }
    return {k: v for k, v in values.items() if v}
