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

import csv
import hashlib
import io
import json
import math
import os
import re
import shutil
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


# #1299 — the fleet-visible genlock LOCK facet. The #1298 statusbar widget is the ONE place the
# three genlock producers (per-source FIFO counters, the NDI output's wall-stamping flag, the
# dantesync :8898 clock facet) are joined into one decided verdict; it emits that verdict as a
# versioned `genlock-lock-json: {…} (#1299)` line (a heartbeat + on-change, so the #1222 bounded
# TAIL always holds a fresh one). This parser reads the NEWEST such line and reshapes the widget's
# payload into the nested `genlock_lock` facet the dev1 watchdog + rig-status read. Reusing the
# SAME already-bounded log_text as every other facet (no second read, no subprocess -> no new
# #1222 cache needed). "" / a stock OBS with no such line -> None (facet OMITTED downstream, never a
# false UNLOCKED).
_GENLOCK_LOCK_JSON_MARKER = "genlock-lock-json:"


def genlock_lock_facet_from_log(text):
    """The nested `genlock_lock` facet dict from the NEWEST `genlock-lock-json:` line in *text*, or
    None when the line is absent / unparseable (a stock OBS, or no such line in the bounded window
    yet — UNKNOWN downstream, NEVER a fabricated UNLOCKED).

    Shape:
      {state, reason, n_inputs, n_locked, n_absent, latency_ms, recent_event, qpc_drift_ms,
       qpc_drift_ppm, qpc_expected_ppm, qpc_step,
       clock:{state}, output:{present, stamping_wallclock},
       inputs:{<name>:{locked, connected, latency_ms, underruns, relocks, late_holds, depth}},
       [recent_event_inputs:[{name, events}]], [audio_unexpected_inputs:[{name}]], source:"log"}

    #1299 (schema v5, Part 4): `qpc_drift_ppm` (measured windowed drift rate), `qpc_expected_ppm` (the
    dantesync-reported slew the verdict compares against) and `qpc_step` (a single-sample wall STEP
    tripped) are report-only telemetry — the qpc_drift VERDICT is folded into `state` by the widget.
    All three default to None for a v1-v4 line from an older build (the cumulative `qpc_drift_ms` stays).

    #1299 (schema v3, Part 3): `recent_event_inputs` is the top recent-event offender (name+count),
    present ONLY when the v3 line carries a non-empty list (a DEGRADED/recent_event page names it).
    Omitted for a v1/v2 line from an older build, or an empty list, so an older line never fabricates
    an attribution.

    #1303 (schema v4): `audio_unexpected_inputs` is a silent-by-contract source found AUDIBLE (the
    double-audio hazard), present ONLY when the v4 line carries a non-empty list. Omitted for a
    v1/v2/v3 line, or an empty list.

    #1299 (schema v2): `n_absent` (senderless input count) and per-input `connected` distinguish an
    idle NDI input (no sender) from a connected-but-unlocked one so the fleet watchdog never
    false-pages a legitimately-idle input. Both degrade gracefully for a v1 line from an older
    build (`n_absent`->None, `connected`->True).

    `state`/`reason` are the verdict the widget ALREADY decided (so the facet can never disagree
    with the statusbar). The per-input array is keyed by name; a duplicate name keeps the last."""
    t = text or ""
    if _GENLOCK_LOCK_JSON_MARKER not in t:
        return None
    # Newest line wins (the widget heartbeats this, so the last occurrence is the current state).
    newest = None
    for line in t.splitlines():
        if _GENLOCK_LOCK_JSON_MARKER in line:
            newest = line
    if newest is None:
        return None
    # The payload is a JSON object; slice from its first "{" to its last "}" so a leading log-time
    # prefix and the trailing " (#1299)" tag are both ignored. A malformed line -> None.
    start = newest.find("{")
    end = newest.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        payload = json.loads(newest[start:end + 1])
    except (ValueError, TypeError):
        return None
    if not isinstance(payload, dict):
        return None

    # Reshape the widget payload into the facet. Every field is tolerant of absence (a future
    # widget that drops a field must degrade, never crash this gather).
    clock_str = payload.get("clock")
    output_str = payload.get("output")
    inputs_map = {}
    raw_inputs = payload.get("inputs")
    if isinstance(raw_inputs, list):
        for row in raw_inputs:
            if not isinstance(row, dict):
                continue
            name = row.get("name")
            if not isinstance(name, str) or not name:
                continue
            inputs_map[name] = {
                "locked": bool(row.get("locked")),
                # #1299 (schema v2): whether the DistroAV receiver has a live NDI connection. Default
                # True for a v1 line from an older build (no `connected` key) so a senderless-but-
                # unreported input reads connected, exactly as the pre-#1299 behaviour.
                "connected": bool(row.get("connected", True)),
                "latency_ms": row.get("latency_ms"),
                "underruns": row.get("underruns"),
                "relocks": row.get("relocks"),
                "late_holds": row.get("late_holds"),
                "depth": row.get("depth"),
            }

    facet = {
        "state": payload.get("state"),
        "reason": payload.get("reason"),
        "n_inputs": payload.get("n_inputs"),
        "n_locked": payload.get("n_locked"),
        # #1299 (schema v2): senderless (no-NDI-connection) input count. None for a v1 line from an
        # older build -> the decision treats absent as 0, i.e. the pre-#1299 all-connected reading.
        "n_absent": payload.get("n_absent"),
        "latency_ms": payload.get("latency_ms"),
        "recent_event": bool(payload.get("recent_event")),
        "qpc_drift_ms": payload.get("qpc_drift_ms"),
        # #1299 Part 4 (schema v5): windowed wall-vs-QPC drift telemetry (report-only). The qpc_drift
        # VERDICT now keys on the RATE (`qpc_drift_ppm`) vs the dantesync-reported slew
        # (`qpc_expected_ppm`) + a STEP (`qpc_step`), not the unbounded cumulative `qpc_drift_ms` above
        # (kept as raw telemetry). All three default to None for a v1-v4 line from an older build.
        "qpc_drift_ppm": payload.get("qpc_drift_ppm"),
        "qpc_expected_ppm": payload.get("qpc_expected_ppm"),
        "qpc_step": payload.get("qpc_step"),
        "clock": {"state": clock_str} if isinstance(clock_str, str) else {},
        "output": {
            "present": output_str != "absent",
            "stamping_wallclock": output_str == "stamping",
        } if isinstance(output_str, str) else {},
        "inputs": inputs_map,
        "source": "log",
    }

    # #1299 (schema v3, Part 3): the top recent-event offender(s) — [{name, events}] — so a
    # DEGRADED/recent_event page can NAME the offending input (reason=recent_event:<name>). Omit the
    # key entirely when absent (a v1/v2 line from an older build) or empty (no offender), so an older
    # line never fabricates an attribution. Each entry is tolerant: a malformed row is skipped.
    raw_rei = payload.get("recent_event_inputs")
    if isinstance(raw_rei, list):
        rei = []
        for row in raw_rei:
            if not isinstance(row, dict):
                continue
            name = row.get("name")
            if not isinstance(name, str) or not name:
                continue
            rei.append({"name": name, "events": row.get("events")})
        if rei:
            facet["recent_event_inputs"] = rei

    # #1303 (schema v4): the audio-unexpected offender(s) — [{name}] — a silent-by-contract source
    # found AUDIBLE, so a DEGRADED/audio_unexpected page can NAME it (reason=audio_unexpected:<name>).
    # Omit the key entirely when absent (a v1/v2/v3 line from an older build) or empty (no offender),
    # so an older line never fabricates an attribution. Each entry is tolerant: a malformed row is
    # skipped. Names-only (no events count — unlike recent_event, an unexpected-audio input is a
    # binary condition, not a cumulative counter).
    raw_aui = payload.get("audio_unexpected_inputs")
    if isinstance(raw_aui, list):
        aui = []
        for row in raw_aui:
            if not isinstance(row, dict):
                continue
            name = row.get("name")
            if not isinstance(name, str) or not name:
                continue
            aui.append({"name": name})
        if aui:
            facet["audio_unexpected_inputs"] = aui

    return facet


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

# #1320 — the PROGRAM-render freeze signal. `program-render-audit:` (obs-video.c
# obs_graphics_thread_loop, ~5 s) carries the PROGRAM output's render cadence; `lagged` ==
# renderSkipped in that window, so a `lagged>0` window is a render-thread freeze.
_PROGRAM_RENDER_LAGGED_RE = re.compile(r"program-render-audit:.*?\blagged=(\d+)\b")

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


def program_render_lagged_from_log(text):
    """#1320 — the strih PROGRAM-render freeze signal `(max_lagged_str, age_s_str)`, `("", "")` when
    no `program-render-audit:` line exists (absent -> UNKNOWN downstream, never a fabricated 0).

    `program-render-audit:` (obs-video.c obs_graphics_thread_loop, ~5 s) reports the PROGRAM output's
    render cadence; `lagged` == renderSkipped in that window, so a `lagged>0` window is a render-
    thread freeze. Issue 1320: a scene-switch-coincident DistroAV reattach whose blocking
    NDIlib_recv_destroy ran on the graphics thread froze the PROGRAM render ~7.5 s (lagged=228
    avg_frame_ms=782) -> 2ME PGM starved -> stream FIFO underrun -> relock storm -> presented video
    +2/+3 frames late ~40 min. This facet exposes the MAX `lagged` across the tail's
    program-render-audit lines + the in-log age (whole seconds) of the MOST RECENT window achieving
    that max, so the dev1 watchdog can page on a RECENT freeze (not one that scrolled out of the
    tail). `"0"` (a healthy tail: render telemetry live, no freeze) is a truthy string and is KEPT;
    `""` (no telemetry at all) is dropped by the omit-when-empty filter.

    Reads ONLY the TAIL slice of the #1222 bounded head+separator+tail read (a freeze surviving only
    in the head is never reported; a small whole-file log is scanned entirely), in ONE pass (no
    second log read). File order is time order (append-only log), so `_recency_gap_s` corrects a
    single midnight wrap on the date-less OBS timestamps."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    log_newest_ts = None   # ts of the LAST parseable line in file order (the log write head)
    max_lagged = None
    max_ts = None          # ts of the most-recent line achieving max_lagged
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _PROGRAM_RENDER_LAGGED_RE.search(line)
        if m:
            lagged = int(m.group(1))
            if max_lagged is None or lagged > max_lagged:
                max_lagged = lagged
                max_ts = ts
            elif lagged == max_lagged and ts is not None:
                max_ts = ts   # a LATER window at the same max -> report the fresher age
    if max_lagged is None:
        return ("", "")
    gap = _recency_gap_s(log_newest_ts, max_ts)
    age_s = "0" if gap is None else str(round(gap))
    return (str(max_lagged), age_s)


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


# #1295 — the minimum RAM (KB) an OBS process must report to count as a LIVE instance. A real OBS
# sits in the hundreds of MB; a DEAD/mid-exit Get-Process/zombie handle reads ~0-45 KB (the live
# 2026-09-12 RESOLUME-SNV pid-58560: WorkingSet64 ~45 KB, 0 threads). tasklist has NO HasExited /
# thread column, so the honest liveness proxy available from a tasklist row is its Mem Usage; a row
# at/below this floor is a zombie, never a live obs64. 1 MB is a wide, safe separator (a live OBS
# never sits below ~45 MB; the zombie was 45 KB), and this limitation is documented because tasklist
# cannot distinguish a truly-exited process from a live one any other way.
OBS_LIVE_MIN_MEM_KB = 1024


def tasklist_mem_kb(field):
    """#1295 — parse a `tasklist /FO CSV /NH` Mem-Usage field ("512,000 K", "45 K", "N/A", "") to
    an int of KB, or None when it carries no usable number (N/A / blank / unparseable). tasklist
    prints memory in KB with a thousands separator and a trailing " K"."""
    if not isinstance(field, str):
        return None
    s = field.strip().replace(" ", " ").rstrip("Kk").replace(",", "").replace(" ", "")
    if not s or not s.lstrip("-").isdigit():
        return None
    return int(s)


def tasklist_row_is_live_obs(mem_field, min_kb=OBS_LIVE_MIN_MEM_KB):
    """#1295 — True iff a tasklist obs-row's Mem-Usage field proves a LIVE instance (>= min_kb KB).
    An unparseable/absent Mem (None) reads NOT-live: a live OBS always reports a real Mem value, so
    excluding an ambiguous row is the fail-safe that keeps a zombie handle from inflating the
    'exactly one obs64' health signal (#1296). tasklist's limitation (no HasExited column, Mem is
    the only liveness proxy) is documented at OBS_LIVE_MIN_MEM_KB."""
    kb = tasklist_mem_kb(mem_field)
    return kb is not None and kb >= min_kb


# #1227 — VB-Audio Matrix presence, for the `vb_matrix_running` facet the dev1 VB-Matrix alert
# watchdog reads. The process image name after its `.exe` is stripped (tasklist prints e.g.
# `VBAudioMatrix_x64.exe`); the pattern enumerates the actual HOSTS — the stream build
# `VBAudioMatrix_x64` and strih's `VBAudioMatrixCoconut_x64` (+ their non-x64 variants), NOT a
# left-open `VBAudioMatrix_Setup` installer that shares the same folder (case-insensitive, anchored
# at both ends so `NotVBAudioMatrix…` / `…_Setup` never match). The exe pattern is derived from the
# SAME base so the two can never drift apart (a #1222c-style DRY finding).
_VB_MATRIX_NAME_BASE = r"(?i)^VBAudioMatrix(Coconut)?(_x64)?"
VB_MATRIX_PROCESS_NAME_RE = re.compile(_VB_MATRIX_NAME_BASE + r"$")
VB_MATRIX_EXE_RE = re.compile(_VB_MATRIX_NAME_BASE + r"\.exe$")


def vb_matrix_process_from_listing(text):
    """#1227 — the running VB-Matrix process from `tasklist /FO CSV /NH` output *text* (each row
    `"Image Name","PID","Session Name","Session#","Mem Usage"`). A `csv.reader` is REQUIRED — the
    Mem Usage column carries a thousands separator INSIDE its quotes (`"18,236 K"`), so a naive
    comma split would mis-column the PID. The image name has its `.exe` stripped before matching
    `VB_MATRIX_PROCESS_NAME_RE`, so a returned `name` is e.g. `VBAudioMatrix_x64`.

    THREE-state return so the caller never reads a FAILED read as a measured absence (issue 1227
    review 🔴, the #833 / `obs_process_count_from_listing` class):
      None       -- the listing is UNREADABLE (empty/whitespace text = a tasklist subprocess
                    failure, since a live box always lists SOME processes; or a `csv.Error`). The
                    caller must treat this as UNKNOWN (facet omitted), NEVER a DOWN.
      ("", "")   -- a VALID listing with no VB-Matrix HOST row (genuinely absent -> the caller reads
                    DOWN when the install is present on disk).
      (name,pid) -- the first VB-Matrix host process found."""
    if not (text or "").strip():
        return None
    try:
        for row in csv.reader(io.StringIO(text)):
            if not row:
                continue
            image_name = row[0]
            base = image_name[:-4] if image_name.lower().endswith(".exe") else image_name
            if VB_MATRIX_PROCESS_NAME_RE.match(base):
                pid = row[1].strip() if len(row) > 1 else ""
                return (base, pid)
    except csv.Error:
        return None
    return ("", "")


def vb_matrix_install_present_under(scan_dirs):
    """#1227 — True iff any `VBAudioMatrix*.exe` exists (recursively) under any of *scan_dirs* — the
    disk-install gate that distinguishes a box that HAS VB-Matrix but its process is dead (stream
    after a reboot with no host -> the facet must read running="0", a real DOWN) from a box that
    never had VB-Matrix at all (imag -> the facet is omitted, never a false negative). Mirrors
    `obs_installs_under`'s walk-and-match shape. False for a missing/empty dir list (never guessed)."""
    for root in scan_dirs or []:
        if not root or not os.path.isdir(root):
            continue
        for _dirpath, _dirnames, filenames in os.walk(root):
            for name in filenames:
                if VB_MATRIX_EXE_RE.match(name):
                    return True
    return False


def vb_matrix_running_facet(install_present, proc):
    """#1227 — the 3-state `(running, name, pid)` facet composition from the disk-install gate + the
    `vb_matrix_process_from_listing` result *proc* (None | ("", "") | (name, pid)):

      install_present False        -> ("", "", "")     (no VB-Matrix box, e.g. imag: OMITTED
                                                        downstream -> UNKNOWN, never a page)
      proc is None                 -> ("", "", "")     (the tasklist read FAILED — UNKNOWN, NEVER a
                                                        false DOWN off a failed read; issue 1227 🔴)
      install_present, proc ("","")-> ("0", "", "")    (a good read, host genuinely absent -> present
                                                        in JSON as running="0" -> DOWN -> page)
      install_present, (name,pid)  -> ("1", name, pid) (RUNNING)

    `"0"` is a truthy string, so `build_bundle_state`'s omit-when-empty filter KEEPS it (DOWN must
    surface); only the not-installed / unread `""` is dropped."""
    if not install_present:
        return ("", "", "")
    if proc is None:
        return ("", "", "")
    proc_name, proc_pid = proc
    if proc_name:
        return ("1", proc_name, proc_pid or "")
    return ("0", "", "")


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
    # #1276: the volume's FREE space — the owner-ruled (14.9.2026) WARNING signal is "<= 50 GB of
    # FREE space left on the recordings volume", not the sum of recording files. Read via
    # shutil.disk_usage on the SAME local record dir already scanned above (no new transport;
    # works on Windows and imag-Linux). Degrades to None (UNKNOWN downstream — never a false
    # low-space WARN) on any read failure, mirroring the zero-degrade of the file scan above.
    free_bytes = None
    try:
        free_bytes = shutil.disk_usage(record_dir).free
    except OSError as e:
        print(
            f"WARNING: record_dir_stats: could not read free space of {record_dir!r}: {e}",
            file=sys.stderr,
        )
    return {
        "total_bytes": total_bytes,
        "file_count": file_count,
        "oldest_mtime": oldest_mtime,
        "free_bytes": free_bytes,
    }


def recordings_free_verdict(free_bytes, min_free_gb):
    """#1276 — the E2E recordings-retention free-space WARNING verdict, the python mirror of the
    canonical Rust ``recordings_retention::free_space_verdict``. Owner ruling (14.9.2026, verbatim
    "B varovanie ma byt ked 50gb uz len ostava miesta!!!"): warn when the recordings VOLUME has at
    most ``min_free_gb`` of FREE space left, NOT when the sum of recording files exceeds a budget.

    ``free_bytes`` is the volume's free space (from ``record_dir_stats``'s ``free_bytes``), or
    ``None`` when it could not be read. Returns "WARN" iff the free space is STRICTLY below
    ``min_free_gb`` (so exactly ``min_free_gb`` free is still "OK" — the spec's "free >= threshold
    -> no warn"), "UNKNOWN" for ``None`` (never a false low-space WARN from an unreadable stat),
    else "OK". Threshold + comparison in decimal GB (1e9 bytes), the same unit the existing warning
    and the owner's "50gb" meant."""
    if free_bytes is None:
        return "UNKNOWN"
    free_gb = free_bytes / 1e9
    return "WARN" if free_gb < min_free_gb else "OK"


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
    audio_ref_lag_src="",
    audio_ref_lag_base_ms="",
    audio_ref_lag_high_ms="",
    audio_ref_lag_low_ms="",
    audio_ref_lag_duty_pct="",
    audio_ref_lag_n="",
    av_offset_recent_med_ms="",
    av_offset_base_med_ms="",
    av_offset_pin="",
    av_offset_pin_stable="",
    av_offset_age_s="",
    av_offset_n_recent="",
    av_offset_n_base="",
    av_offset_dock_live_age_s="",
    av_offset_recent_mad_ms="",
    av_offset_recent_matched_min="",
    vb_matrix_running="",
    vb_matrix_name="",
    vb_matrix_pid="",
    vb_matrix_start="",
    program_render_lagged="",
    program_render_lagged_age_s="",
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
        # #1265 — the per-REFERENCE-source (mbc on stream) ts_lag BAND SHAPE (base/high/low/duty/n),
        # from `audio_ref_band_from_log`. Same omit-when-empty rule; the dev1 audio-lag watchdog's
        # BAND arm reads these to catch a tens-of-ms bimodal/creeping drift the 5000 ms MAX-facet is
        # blind to, and recording-e2e.sh's #856 apply reads the derived verdict to HOLD when the run's
        # audio timeline was unstable.
        "audio_ref_lag_src": audio_ref_lag_src,
        "audio_ref_lag_base_ms": audio_ref_lag_base_ms,
        "audio_ref_lag_high_ms": audio_ref_lag_high_ms,
        "audio_ref_lag_low_ms": audio_ref_lag_low_ms,
        "audio_ref_lag_duty_pct": audio_ref_lag_duty_pct,
        "audio_ref_lag_n": audio_ref_lag_n,
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
        # #1319 — the dock-LIVE freshness age (in-log seconds behind the log head of the freshest
        # `av-sync-dock: diag ... locked=yes` heartbeat). Lets the dev1 band decision read
        # IN_BAND_QUIET (dock LIVE, offset in the suggestion dead band) instead of a false STALE.
        # Same omit-when-empty rule (absent == UNKNOWN downstream, never a fake 0).
        "av_offset_dock_live_age_s": av_offset_dock_live_age_s,
        # #1319 Part 2 — the dock estimator's recent-window measurement QUALITY (median MAD +
        # min matched), from av_offset_quality_from_log. The dev1 band arm reads LOW_QUALITY
        # (log-only, never a page) unless recent_mad_ms <= 15 AND recent_matched_min >= 30, so a
        # noisy/biased dock reading no longer trips the +-30 ms band on its own scatter. Same
        # omit-when-empty rule (absent == quality unjudgeable -> the band proceeds, never a fake 0).
        "av_offset_recent_mad_ms": av_offset_recent_mad_ms,
        "av_offset_recent_matched_min": av_offset_recent_matched_min,
        # #1227 — the VB-Matrix presence facet the dev1 VB-Matrix alert watchdog reads. Same
        # omit-when-empty rule: running="0" (installed but the VBAudioMatrix* process is DEAD) is a
        # truthy string and is KEPT (surfaces as DOWN); running="" (a box with no VB-Matrix install,
        # e.g. imag) is dropped -> UNKNOWN downstream, never a false negative. name/pid/start are
        # context only (pid free from the tasklist parse; start best-effort, PID-keyed-cached CIM).
        "vb_matrix_running": vb_matrix_running,
        "vb_matrix_name": vb_matrix_name,
        "vb_matrix_pid": vb_matrix_pid,
        "vb_matrix_start": vb_matrix_start,
        # #1320 — the strih PROGRAM-render freeze facet the dev1 render-freeze watchdog reads: the
        # MAX `program-render-audit lagged` over the tail + the in-log age (s) of the most recent
        # window achieving it. Same omit-when-empty rule: "0" (render telemetry live, no freeze) is
        # a truthy string and is KEPT; "" (no program-render-audit line at all) is dropped ->
        # UNKNOWN downstream, never a fabricated 0. From `program_render_lagged_from_log`.
        "program_render_lagged": program_render_lagged,
        "program_render_lagged_age_s": program_render_lagged_age_s,
    }
    return {k: v for k, v in values.items() if v}
