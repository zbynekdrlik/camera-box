#!/usr/bin/env python3
"""#1331 -- the VERIFIED A/V-sync SESSION report as ONE pure, single-source-of-truth decider.

WHY THIS FILE EXISTS (owner request 18.9.2026): on 17.9. a 90-minute live produced only TWO raw
one-clip messages on Discord (one at conf 3.2, below the SyncNet usability floor), with no statement
that anything was VERIFIED and no aggregation. The owner asked: the live A/V measurement must report
that it VERIFIED (measured continuously across the whole broadcast) and whether it FITS (a median
verdict), not a lone unaggregated clip.

The measurer (scripts/avsync-measure-dev2.sh, dev2, ~90 s cadence) now appends EVERY pass's heartbeat
record to a per-day TSV (`~/avsync/measurements-<YYYY-MM-DD>.tsv`, `<epoch>\\t<status>`). The dev1
watchdog (scripts/avsync-heartbeat-alert-watchdog.sh, ~5 min cadence) fetches those rows and hands
them to THIS decider, which aggregates a live SESSION and returns the Discord messages to post plus
the new report state. The raw one-clip forward (maybe_forward_verdict) is REPLACED by this report.

Architecture: PURE (no ssh, no I/O beyond the CLI wrapper) -- the "pure decision library" shape of
scripts/avsync_lineup.py / scripts/avsync_freshness.py. `decide(rows, state, now_epoch)` takes the
already-fetched rows + the previous report state and returns `(messages, new_state)`; it is
IDEMPOTENT -- calling it again with the same rows emits nothing new. The confidence floor and the
in-band alarm threshold are IMPORTED from avsync_lineup.py (never retyped -- one source of truth).

Vocabulary (from avsync-watchdog.ps1 / av_sync_measure.py, verified in avsync_lineup.py's header):
  a ROW is `<epoch>\\t<status>`; a MEASURED row's status starts with "measured: " (grab succeeded);
  a CONFIDENT clip parses "AV offset ±N fr (±X ms) conf C" with C >= OFFSET_CONF_FLOOR (4.0);
  everything else in a session (UNMEASURABLE band/graphics windows, low-conf clips) is "nemerateľné".

CLI: avsync_report.py --state <json file> --rows <tsv file> [--now EPOCH]
  Reads the rows TSV + the JSON state, prints one message per line (UTF-8), REWRITES the state file
  in place, exits 0. A missing/garbled state file starts fresh; a missing rows file = no rows.
"""

import argparse
import json
import math
import os
import sys
from datetime import datetime

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

# Single source of truth for the confidence floor + the in-band threshold + the offset parser --
# imported from the #813 decider, NEVER retyped (a divergence here would grade clips differently
# from the run-time alarm arm that shares the same heartbeats).
from avsync_lineup import (  # noqa: E402
    OFFSET_ALARM_MS_DEFAULT,
    OFFSET_CONF_FLOOR,
    is_measured_heartbeat,
    parse_offset,
)

try:
    from zoneinfo import ZoneInfo

    _TZ = ZoneInfo("Europe/Bratislava")
except Exception:  # pragma: no cover -- no tzdata: fall back to naive local time (still HH:MM)
    _TZ = None

# A session boundary: a gap of this many seconds WITHOUT a measured row starts a new session and
# (after >= 1 measured row) ends the current one. 600 s = 10 min, comfortably above the ~90 s
# measurement cadence, so an ordinary between-clip gap never splits a live broadcast.
SESSION_GAP_S = 600
# The trailing window the periodic/change verdict is computed over (20 min).
WINDOW_S = 1200
# The window verdict needs at least this many CONFIDENT clips to be a real verdict (never a lone clip).
MIN_CONFIDENT = 3

DEFAULT_STATE = {
    "session_start": None,       # epoch of the current session's first measured row (None = idle)
    "start_posted": False,       # the START message has been posted for the current session
    "last_measured_epoch": None, # epoch of the last measured row seen (session-end detection)
    "last_periodic_epoch": None, # tick anchor of the last PERIODIC message (== session_start at start)
    "last_verdict": None,        # "SEDI" | "NESEDI" | None -- last DEFINITE window verdict evaluated
    "end_posted": True,          # the END message has been posted for the current/last session
    # Observability only: the max row epoch processed so far. NOT consumed for dedup (idempotency
    # comes entirely from the session state machine above, which re-derives from all fetched rows);
    # it is available as a fetch cursor for a future "fetch only rows > cursor" optimization (review F3).
    "last_row_epoch": None,
}


def _hm(epoch):
    """HH:MM in Europe/Bratislava local time from a UNIX epoch."""
    if _TZ is not None:
        return datetime.fromtimestamp(int(epoch), _TZ).strftime("%H:%M")
    return datetime.fromtimestamp(int(epoch)).strftime("%H:%M")


def _percentile(sorted_vals, p):
    """PURE: the p-th percentile (0..100) of an already-sorted numeric list, linear interpolation
    between closest ranks (numpy's default 'linear' method). None for an empty list."""
    n = len(sorted_vals)
    if n == 0:
        return None
    if n == 1:
        return float(sorted_vals[0])
    k = (n - 1) * (p / 100.0)
    f = int(math.floor(k))
    c = int(math.ceil(k))
    if f == c:
        return float(sorted_vals[f])
    return sorted_vals[f] * (c - k) + sorted_vals[c] * (k - f)


def _median(vals):
    if not vals:
        return None
    return _percentile(sorted(vals), 50)


def _advice(median_ms):
    """PURE: the operator knob advice, byte-parity with av_sync_measure.py's own verdict text --
    audio ahead (median > 0) -> "ZNIZ '2ME PGM' latency o <median>"; video ahead (median < 0) ->
    "ZVYS '2ME PGM' latency o <|median|>". No diacritics -- matches av_sync_measure.py exactly so a
    ZNIZ/ZVYS grep and the operator's muscle memory read the same string everywhere."""
    m = int(round(median_ms))
    if m > 0:
        return "ZNIZ '2ME PGM' latency o {}".format(m)
    if m < 0:
        return "ZVYS '2ME PGM' latency o {}".format(abs(m))
    return "A/V sync OK"


class _Row:
    __slots__ = ("epoch", "status", "measured", "offset_ms", "conf", "confident")

    def __init__(self, epoch, status):
        self.epoch = int(epoch)
        self.status = status
        self.measured = is_measured_heartbeat(status)
        self.offset_ms, self.conf = parse_offset(status)
        self.confident = (
            self.measured
            and self.offset_ms is not None
            and self.conf is not None
            and self.conf >= OFFSET_CONF_FLOOR
        )


def _parse_rows(rows):
    """PURE: coerce (epoch, status) pairs into sorted, epoch-deduped _Row objects (a later duplicate
    epoch wins -- overlapping fetches never double-count)."""
    by_epoch = {}
    for epoch, status in rows:
        try:
            e = int(epoch)
        except (TypeError, ValueError):
            continue
        by_epoch[e] = _Row(e, status)
    return [by_epoch[e] for e in sorted(by_epoch)]


def _split_sessions(measured_rows):
    """PURE: split measured rows (sorted by epoch) into sessions on a >= SESSION_GAP_S gap."""
    sessions = []
    cur = []
    for r in measured_rows:
        if cur and (r.epoch - cur[-1].epoch) >= SESSION_GAP_S:
            sessions.append(cur)
            cur = []
        cur.append(r)
    if cur:
        sessions.append(cur)
    return sessions


def _counts(session_rows):
    """PURE: (N total measured, K nemerateľných, [confident offset_ms...]) for a list of measured rows."""
    n = len(session_rows)
    confident = [r.offset_ms for r in session_rows if r.confident]
    k = n - len(confident)
    return n, k, confident


def _window_verdict(offsets):
    """PURE: (verdict, median) for a list of confident offsets in a window. verdict is
    "SEDI"/"NESEDI" when there are >= MIN_CONFIDENT clips, else None (median may still be returned
    for context, or None when there are no clips)."""
    med = _median(offsets)
    if len(offsets) < MIN_CONFIDENT:
        return None, med
    return ("SEDI" if abs(med) <= OFFSET_ALARM_MS_DEFAULT else "NESEDI"), med


def _verdict_tail(verdict, median):
    """PURE: the ' -> SEDÍ' / ' -> NESEDÍ o X ms -> <advice>' suffix for a definite verdict."""
    if verdict == "SEDI":
        return " → SEDÍ"
    return " → NESEDÍ o {} ms → {}".format(abs(int(round(median))), _advice(median))


def _median_paren(offsets):
    """PURE: 'medián +X ms (p10 +A … p90 +B)' for a non-empty confident list."""
    s = sorted(offsets)
    med = _percentile(s, 50)
    p10 = _percentile(s, 10)
    p90 = _percentile(s, 90)
    return "medián {:+d} ms (p10 {:+d} … p90 {:+d})".format(
        int(round(med)), int(round(p10)), int(round(p90))
    )


def _end_message(session_rows):
    """PURE: the end-of-broadcast summary line for a whole session."""
    ss = session_rows[0].epoch
    se = session_rows[-1].epoch
    n, k, confident = _counts(session_rows)
    head = "📐 A/V počas vysielania {}–{}: {} meraní ({} nemerateľných)".format(
        _hm(ss), _hm(se), n, k
    )
    if not confident:
        return head + " — žiadne spoľahlivé meranie, bez verdiktu"
    verdict, median = _window_verdict(confident)
    body = ", " + _median_paren(confident)
    if verdict is None:
        # < 3 confident over the WHOLE session -- report the median but no SEDÍ/NESEDÍ verdict.
        return head + body + " — < 3 spoľahlivé, bez verdiktu"
    return head + body + _verdict_tail(verdict, median)


def decide(rows, state, now_epoch):
    """PURE: given the fetched rows [(epoch, status)...], the previous report state (a dict), and the
    evaluation time now_epoch, return (messages, new_state). IDEMPOTENT: a second call with the same
    rows and the returned state emits nothing new. See the module docstring for the message kinds."""
    st = dict(DEFAULT_STATE)
    if state:
        st.update({k: state.get(k, DEFAULT_STATE[k]) for k in DEFAULT_STATE})
    now = int(now_epoch)
    messages = []

    parsed = _parse_rows(rows)
    if parsed:
        st["last_row_epoch"] = parsed[-1].epoch
    measured = [r for r in parsed if r.measured]
    if not measured:
        return messages, st

    sessions = _split_sessions(measured)
    last_session = sessions[-1]
    ss = last_session[0].epoch
    se = last_session[-1].epoch
    live = (now - se) < SESSION_GAP_S

    # (1) A PREVIOUS session that ended without an END yet (a new session has since started).
    if st["session_start"] is not None and not st["end_posted"] and st["session_start"] != ss:
        prev = next((s for s in sessions if s[0].epoch == st["session_start"]), None)
        if prev is not None:
            messages.append(_end_message(prev))
        st["end_posted"] = True

    # (2) Enter/refresh the current session's tracking on a session switch.
    if st["session_start"] != ss:
        st["session_start"] = ss
        st["start_posted"] = False
        st["last_periodic_epoch"] = ss
        st["last_verdict"] = None
        st["end_posted"] = False
    st["last_measured_epoch"] = se

    n_sess, _k_sess, confident_sess = _counts(last_session)

    # (3) START -- the first confident clip of the session.
    if not st["start_posted"] and confident_sess:
        first = next(r for r in last_session if r.confident)
        messages.append(
            "📐 A/V overené {} (začiatok vysielania): 1. meranie {:+d} ms conf {:.1f}"
            " — verdikt po ≥ 3 meraniach".format(_hm(first.epoch), first.offset_ms, first.conf)
        )
        st["start_posted"] = True

    if not live:
        # (4a) Session ended -> END (once). No further periodic/change for a finished broadcast.
        if not st["end_posted"]:
            messages.append(_end_message(last_session))
            st["end_posted"] = True
        return messages, st

    # (4b) Live session -> CHANGE (verdict flip, at once) + PERIODIC (every 20 min).
    window = [r for r in last_session if now - WINDOW_S <= r.epoch <= now]
    w_conf = [r.offset_ms for r in window if r.confident]
    verdict, median = _window_verdict(w_conf)

    if verdict is not None and st["last_verdict"] is not None and verdict != st["last_verdict"]:
        tail = _verdict_tail(verdict, median)
        messages.append(
            "📐 A/V overené {}: verdikt sa zmenil{} (medián {:+d} ms, {} spoľahlivých)".format(
                _hm(now), tail, int(round(median)), len(w_conf)
            )
        )
    if verdict is not None:
        st["last_verdict"] = verdict

    if now >= st["last_periodic_epoch"] + WINDOW_S:
        n_win, k_win, _cw = _counts(window)
        head = "📐 A/V overené {}: za 20 min {} meraní ({} nemerateľných)".format(
            _hm(now), n_win, k_win
        )
        if verdict is None:
            messages.append(head + " — zatiaľ < 3 spoľahlivé, bez verdiktu")
        else:
            messages.append(head + ", " + _median_paren(w_conf) + _verdict_tail(verdict, median))
        # Advance past every missed tick (dev1 down / gaps) so a backlog posts ONE summary, not many.
        missed = (now - st["last_periodic_epoch"]) // WINDOW_S
        st["last_periodic_epoch"] = st["last_periodic_epoch"] + missed * WINDOW_S

    return messages, st


def _read_rows(path):
    rows = []
    if not path or not os.path.exists(path):
        return rows
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n").rstrip("\r")
            if not line:
                continue
            parts = line.split("\t", 1)
            if len(parts) != 2:
                continue
            epoch, status = parts
            try:
                rows.append((int(epoch), status))
            except (TypeError, ValueError):
                continue
    return rows


def _read_state(path):
    if not path or not os.path.exists(path):
        return {}
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def _write_state(path, state):
    tmp = "{}.tmp.{}".format(path, os.getpid())
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, ensure_ascii=False, sort_keys=True)
    os.replace(tmp, path)


def main(argv=None):
    import time

    ap = argparse.ArgumentParser(description="#1331 verified A/V-sync session report decider")
    ap.add_argument("--state", required=True, help="path to the JSON report-state file (rewritten)")
    ap.add_argument("--rows", required=True, help="path to the TSV rows file (<epoch>\\t<status>)")
    ap.add_argument("--now", type=int, default=None, help="evaluation epoch (default: time.time())")
    args = ap.parse_args(argv)

    now = args.now if args.now is not None else int(time.time())
    rows = _read_rows(args.rows)
    state = _read_state(args.state)
    messages, new_state = decide(rows, state, now)
    for m in messages:
        sys.stdout.write(m + "\n")
    _write_state(args.state, new_state)
    return 0


if __name__ == "__main__":
    sys.exit(main())
