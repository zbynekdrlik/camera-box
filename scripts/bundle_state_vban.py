#!/usr/bin/env python3
"""The obs-vban PACER loss facet of the bundle-state gather (issue 1381): per destination (or
per stream on the shipped line) how much the loss counters grew in the last window, read from
the `obs-vban pacing:` status lines beside the audio mixer facet (`bundle_state_audio`).

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import itertools
import re

from bundle_state_log import timestamped_tail_lines


# `obs-vban pacing:` (vendor/obs-vban vban-output-thread.c, every 10 s per output). Two formats:
# the shipped issue-1372 line (underflows / overflows / trims, NO destination -- two outputs print
# identical-looking lines) and the issue-1381 fixed-timeline line (discontinuities / repays /
# resyncs / silence_ms / discarded_ms + dest=ip:port). All counters are cumulative since the output
# thread started. `pacing-config:` lines never match (the colon follows `pacing`).
_VBAN_PACING_RE = re.compile(r"obs-vban pacing: (.*)$")
_VBAN_KV_RE = re.compile(r"(\w+)=('[^']*'|\S+)")
VBAN_LEGACY_LOSS_EVENTS = ("underflows", "overflows", "trims")
VBAN_LOSS_EVENTS = ("discontinuities", "repays", "resyncs")
VBAN_LOSS_MS = ("silence_ms", "discarded_ms")   # `late_sends` is late but COMPLETE audio: no loss
# The loss window: two dev1 passes (5 min) + timer slack, so one burst survives the 2-pass confirm.
VBAN_LOSS_WINDOW_S = 660
# Each output logs once per >= 10 s (observed 10.0-10.2 s), so within TWO periods of a legacy
# key's first line in the tail every output has shown its current counters, even with a line late
# or a forward wall-clock step. Those lines only seed what is known.
VBAN_BASELINE_S = 20.5
VBAN_PREDECESSOR_SCAN = 64   # a loss is measured against the most recently seen tuples only
# One sender logs every >= 10.0 s. Two lines of one destination+stream key closer than this come
# from several senders (two hosts resolving to one receiver), so that key falls back to the
# identity-less tuple method.
VBAN_MULTI_SENDER_GAP_S = 8.0


def _vban_vector(fields):
    """A pacing line's `(key, loss vector, n_event_counters, has_ms, one_output)` or None for a line
    of neither format. The key is `<dest>/<stream>` on the fixed-timeline line (`one_output` True:
    presumed one sender, checked by `_VbanKey`; a VBAN receiver port takes many streams, so the
    destination alone is not a sender) or `stream=<name>` on the shipped line, which names no
    destination. The vector is the event counters then the ms counters."""
    stream = fields.get("stream", "").strip("'")
    try:
        if "discontinuities" in fields:
            vec = tuple(int(fields[k]) for k in VBAN_LOSS_EVENTS) + tuple(
                float(fields[k]) for k in VBAN_LOSS_MS)
            dest = fields.get("dest")
            key = f"{dest}/{stream}" if dest else f"stream={stream}"
            return (key, vec, len(VBAN_LOSS_EVENTS), True, bool(dest))
        if "underflows" in fields:
            vec = tuple(int(fields[k]) for k in VBAN_LEGACY_LOSS_EVENTS)
            return (f"stream={stream}", vec, len(VBAN_LEGACY_LOSS_EVENTS), False, False)
    except (KeyError, ValueError):
        return None
    return None


def _vban_growth(vec, old, n_events):
    """`(events, ms)` by which `vec` exceeds `old` (the caller ensures every counter is >=)."""
    return (sum(n - o for n, o in zip(vec[:n_events], old[:n_events])),
            sum(n - o for n, o in zip(vec[n_events:], old[n_events:])))


def _vban_increment(vec, seen, n_events):
    """How much a NEW legacy counter tuple grew: against the nearest earlier tuple it dominates
    (every counter >=), `(events, ms)`; None when it dominates none (an output restart reset its
    counters, or a smaller tuple of another output). `seen` is in LAST-SEEN order (a repeated tuple
    moves to the end), so each live output's own current tuple is among the newest
    VBAN_PREDECESSOR_SCAN searched."""
    best = None
    for old in itertools.islice(reversed(seen), VBAN_PREDECESSOR_SCAN):
        if len(old) != len(vec) or any(n < o for n, o in zip(vec, old)):
            continue
        inc = _vban_growth(vec, old, n_events)
        if best is None or inc < best:
            best = inc
    return best


class _VbanKey:
    """The loss state of one pacer key."""
    __slots__ = ("one_output", "has_ms", "first", "prev", "last_pos", "history", "seen", "events",
                 "loss_ms")

    def __init__(self, one_output, has_ms, pos):
        self.one_output = one_output
        self.has_ms = has_ms
        self.first = pos
        self.prev = None          # one-sender key: the previous line's vector
        self.last_pos = None      # one-sender key: the previous line's position
        self.history = []         # one-sender key: its lines, replayed if it proves multi-sender
        self.seen = {}            # tuple method: distinct tuples in last-seen order (dict order)
        self.events = 0
        self.loss_ms = 0.0

    def _add(self, inc):
        self.events += inc[0]
        self.loss_ms += inc[1]

    def feed(self, vec, n_events, pos, counts):
        """One status line; `counts` = it lies inside the loss window."""
        if self.one_output:
            if self.last_pos is not None and pos - self.last_pos < VBAN_MULTI_SENDER_GAP_S:
                self._become_multi_sender()
            else:
                self._feed_one_sender(vec, n_events, pos, counts)
                return
        self._feed_tuple(vec, n_events, pos, counts)

    def _feed_one_sender(self, vec, n_events, pos, counts):
        """The plain delta against this sender's previous line. A counter that went DOWN is a sender
        restart: the new thread started at 0, so its counts are losses since the restart."""
        self.history.append((vec, n_events, pos, counts))
        self.last_pos = pos
        prev, self.prev = self.prev, vec
        if prev is None or len(prev) != len(vec):
            return
        if any(n < o for n, o in zip(vec, prev)):
            prev = tuple(0 for _ in vec)
        if counts:
            self._add(_vban_growth(vec, prev, n_events))

    def _become_multi_sender(self):
        """Two lines of this key closer than one logging period: several senders share it. Forget
        the per-line deltas and replay the key's lines through the tuple method."""
        self.one_output = False
        self.events, self.loss_ms = 0, 0.0
        history, self.history = self.history, []
        for vec, n_events, pos, counts in history:
            self._feed_tuple(vec, n_events, pos, counts)

    def _feed_tuple(self, vec, n_events, pos, counts):
        """The identity-less method: a loss is a tuple never seen before that dominates one seen
        earlier; the key's first VBAN_BASELINE_S only seed."""
        if vec in self.seen:
            self.seen[vec] = self.seen.pop(vec)   # move to the end: last-seen order
            return
        if counts and pos - self.first > VBAN_BASELINE_S:
            inc = _vban_increment(vec, list(self.seen), n_events)
            if inc is not None:
                self._add(inc)
        self.seen[vec] = True


def vban_pacer_loss_from_log(text, window_s=VBAN_LOSS_WINDOW_S, tail=None):
    """issue 1381 -- `(loss_events, loss_ms, dest, age_s)` for the obs-vban pacer, or four `""` when
    the tail has no `obs-vban pacing:` status line (no VBAN output on this box).

    Per key, how much the loss counters grew over the last `window_s` of the log (legacy:
    underflows + overflows + trims; fixed-timeline: discontinuities + repays + resyncs, and
    silence_ms + discarded_ms as `loss_ms`). The worst key is reported (events first, then ms);
    `loss_ms` is `""` when that key's line carries no ms counters (the shipped format).

    - **A `dest=` line is keyed on destination + stream** (a VBAN receiver port takes many streams)
      and presumed to be one sender: its loss is the plain delta against the key's previous line,
      and a counter that went DOWN is a sender restart whose new counts (the thread starts at 0)
      are losses since the restart. The premise is CHECKED: two lines of the key closer than
      VBAN_MULTI_SENDER_GAP_S (one sender logs every >= 10 s) mean several senders share it (two
      hosts resolving to one PC), and the key is replayed through the tuple method below. Never
      an over-count; a loss hidden inside a restart (the old thread's last counts before it
      stopped logging) may be missed.
    - **The shipped line names no destination**, and the two resolume outputs print identical-looking
      lines, so an output's identity cannot be recovered. The counters only grow on a loss, so a loss
      is a tuple NEVER SEEN BEFORE for that key that dominates one seen earlier, counted by how much
      it exceeds the nearest such tuple. A clean output repeats its own tuple and adds nothing; a
      restarted output starts at 0 and dominates nothing it has not shown. The first VBAN_BASELINE_S
      (two logging periods) of the key in the tail only seed the known tuples.
      Residuals: a step that lands on a tuple already seen for the key (one output reaching the
      other output's current counters) is not counted, and a step's size is taken from the nearest
      dominated tuple, which may be the other output's. So the count can be LOWER than the true
      growth: a sustained fault still pages, an isolated single step onto the other output's value
      does not. The only over-count: a second output whose first line in the tail comes more than
      two periods after the first reads its counter gap as a loss once (the same holds for a
      multi-sender dest= key, which uses this method).
    `age_s` is the newest status line's in-log age. `tail` = a precomputed
    `timestamped_tail_lines(text)`."""
    stamped, head = tail if tail is not None else timestamped_tail_lines(text)
    keys = {}
    newest = None
    for pos, line in stamped:
        if "obs-vban pacing:" not in line:
            continue
        m = _VBAN_PACING_RE.search(line)
        if not m:
            continue
        parsed = _vban_vector(dict(_VBAN_KV_RE.findall(m.group(1))))
        if parsed is None:
            continue
        key, vec, n_events, has_ms, one_output = parsed
        newest = pos
        state = keys.get(key)
        if state is None:
            state = keys[key] = _VbanKey(one_output, has_ms, pos)
        state.feed(vec, n_events, pos, head - pos <= window_s)
    if newest is None:
        return ("", "", "", "")
    worst = None
    for key in sorted(keys):
        st = keys[key]
        if worst is None or (st.events, st.loss_ms) > (worst[1].events, worst[1].loss_ms):
            worst = (key, st)
    key, st = worst
    return (str(st.events), f"{st.loss_ms:.1f}" if st.has_ms else "", key,
            str(round(head - newest)))
