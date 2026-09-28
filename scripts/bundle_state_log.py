#!/usr/bin/env python3
"""The OBS-log READ + in-log time helpers every bundle-state facet family shares: the #1222
bounded head+tail read, the leading `HH:MM:SS.mmm` parse, the midnight-wrap-corrected recency
gap, the file-order elapsed positions + the timestamped tail (issue 1381), and the box clock +
log-head age (issue 1385).

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import math
import os
import re
import sys
import time


# #1222 — the strih bundle-state gather's latency grew LINEARLY with the live OBS log size: a
# ~13h session (75 MB log) made every *_from_log parser re-scan the WHOLE file on EVERY
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
# `\d+\.\d+\.\d+` / `fps:\s+\d+/` style pattern in the facet modules), no colon-prefixed keyword
# any parser scans for ("OBS ", "DistroAV (Version", "video settings reset:", "genlock:"), and
# newline-padded on both sides so a byte-cut mid-line on either side of the join can never merge
# into something a parser could mistake for a real one.
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
    it can never be mistaken for a real log line by any facet parser). Read in BINARY mode and
    decoded with `errors="replace"` — a byte-boundary cut mid multi-byte UTF-8 character degrades
    to a harmless U+FFFD, never a crash. Unlike the original whole-file text-mode read this
    replaces, a Windows CRLF line ending is NOT translated to a bare `\n` here; every facet parser
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


def _file_order_elapsed(ts_values):
    """Seconds each timestamp sits AFTER the first, walking the list in FILE ORDER (the append-only
    log's time order). A step back of more than 12 h is a date-less midnight wrap (+24 h); a smaller
    step back is two threads logging out of order and counts as 0. Unlike a single head-vs-line
    gap this stays right across several midnights in one tail."""
    out = []
    pos = 0.0
    prev = None
    for ts in ts_values:
        if prev is not None:
            d = ts - prev
            if d < -43200.0:
                d += 86400.0
            elif d < 0.0:
                d = 0.0
            pos += d
        out.append(pos)
        prev = ts
    return out


def timestamped_tail_lines(text):
    """The tail slice's lines that carry an OBS `HH:MM:SS.mmm` prefix, as `(pos_s, line)` in file
    order, where pos_s is `_file_order_elapsed` of their timestamps; plus the log head's pos_s
    (the LAST such line). `([], None)` when none. The server computes it ONCE per request and
    hands it to `audio_mixer_from_log` and `vban_pacer_loss_from_log` (`tail=`), so the tail
    is timestamp-parsed a single time."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    stamped = []
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            stamped.append((ts, line))
    if not stamped:
        return ([], None)
    pos = _file_order_elapsed([ts for ts, _ in stamped])
    return (list(zip(pos, (ln for _, ln in stamped))), pos[-1])


# issue 1385 -- the log head's age against the box's OWN wall clock: positive proof the OBS log is
# being written NOW. Every other `*_age_s` facet is measured behind the log head, so a log that
# stopped (OBS down, hung) keeps its old ages forever; the dev1 audio-mixer decision pages a
# stopped audio thread (STALLED) only while this age is small. OBS stamps each log line with its
# local HH:MM:SS.mmm and the gather runs on the same box, so the two clocks are one. The log has
# no date: a head AHEAD of "now" (read right after the log) can only be a wall-clock step back
# (dantesync steps ~50 ms) and reads 0 up to this slack; further ahead it is a previous day's line
# (+24 h), which the decision reads as not live. A log dead for a whole number of days still reads
# live for about 70 s once a day (this slack + the decision's 60 s), so at most one pass a day; the
# dev1 watchdog resets the mixer confirm on every STALE pass in between, so two such days never pair
# into a page (issue 1385 review).
LOG_HEAD_CLOCK_SLACK_S = 10.0


def local_seconds_of_day(epoch=None):
    """The box's local wall clock as seconds of the day (the clock OBS stamps its log lines with),
    at `epoch` (default: now)."""
    e = time.time() if epoch is None else float(epoch)
    lt = time.localtime(e)
    return lt.tm_hour * 3600 + lt.tm_min * 60 + lt.tm_sec + (e - math.floor(e))


def obs_log_head_age_s_from_log(text, now_s):
    """issue 1385 -- whole seconds from the newest timestamped line of the TAIL to `now_s` (the
    box's local seconds of the day, taken right after the log read), or `""` when the tail has no
    timestamped line (omit-when-empty -> the decision has no liveness proof). Lines are split on
    `\n` only, which is how OBS ends each stamped line; `timestamped_tail_lines` uses
    `splitlines()`, and the two differ only for a bare `\r` followed by timestamp-like text."""
    t = text or ""
    sep = t.rfind(LOG_BOUNDED_READ_SEPARATOR)
    floor = sep + len(LOG_BOUNDED_READ_SEPARATOR) if sep >= 0 else 0
    # Walk back line by line from the end (the head is in the last few lines): no copy of the tail.
    end = len(t)
    while end > floor:
        start = max(t.rfind("\n", floor, end), floor - 1) + 1
        ts = _log_line_seconds(t[start:end])
        if ts is not None:
            gap = float(now_s) - ts
            if gap < 0.0:
                gap = 0.0 if gap >= -LOG_HEAD_CLOCK_SLACK_S else gap + 86400.0
            return str(round(gap))
        end = start - 1
    return ""
