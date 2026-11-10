#!/usr/bin/env python3
"""The program-audio RECEIVE loss facet of the bundle-state gather (issue 1460): per receiver, how
much its loss counters grew in the last window, read from the receivers' own 10 s status lines.

Two receivers feed an OBS program today (`.claude/rules/obs-vban-receive.md`,
`.claude/rules/strih-dante-input.md`):

  * the obs-vban SOURCE (stream-pp, strih-pp): `obs-vban receive: received=N lost=N late=N
    re_anchors=N stale=N reordered=N concealed=N silence_ms=X ... emit_missed=N ... source='<input>'`
    (vendor/obs-vban/src/vban-source.c `vban_src_log_status`), written by the receive callback, so
    it stops while no packet comes; counters cumulative since the source was created;
  * the Dante `dante_input` source (strih-snv, stream-snv): `dante-input status: ... rx_loss=N
    rx_holes=N underruns=N ...` every 10 s, totals since the device started (the obs release's
    `plugins/dante-input`, the line `scripts/dante_input_continuity.py` grades in the E2E).

The facet is NESTED (`audio_rx`, attached by the server after `build_bundle_state`, like
`genlock_lock`): one entry per receiver, keyed `obs-vban:<input>` / `dante`. A delta is counted only
between two status lines of ONE receiver that both lie inside the window (two dev1 passes + slack,
the obs-vban pacer's), so a sender that paused for longer than the window never reads its whole
pause as one loss. A counter that went DOWN, or a re-init line between the two (an obs-vban
`receive anchored:` / `receive worker: ... stopped` of that source, a `dante-input start:`), makes
that pair a REBASE (counted, never a loss): the counters restarted.

GATED counters are the losses (`events` = their delta sum): obs-vban `lost` (concealed slots
included: a concealed slot is a LOST slot, ROZHODNUTE 6359382787), `re_anchors` (the held second of
a re-anchor is never counted in `lost`) and `emit_missed`; Dante the E2E gate's GATED tuple. NAMED
counters are reported only (an obs-vban late packet's slot is already in `lost`; Dante rx_late /
overruns / late_blocks are timing only).

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module. Ships in the :8899 server tree declared in
`scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import re

from bundle_state_log import timestamped_tail_lines

AUDIO_RX_WINDOW_S = 660
VBAN_RX_GATED = ("lost", "re_anchors", "emit_missed")
VBAN_RX_NAMED = ("late", "stale", "reordered", "concealed", "silence_ms")
# pinned equal to scripts/dante_input_continuity.py GATED (the E2E gate's loss counters)
DANTE_RX_GATED = ("rx_loss", "rx_holes", "underruns", "rx_gaps", "socket_drops", "rebootstraps",
                  "rx_written_late", "re_anchors", "obs_dropped")
DANTE_RX_NAMED = ("rx_late", "overruns", "late_blocks")
# also read for the went-down (restart) check, never reported
_VBAN_RX_TOTALS = ("received",)
_DANTE_RX_TOTALS = ("rx_packets", "delivered_frames")

_VBAN_STATUS_RE = re.compile(r"obs-vban receive: (.*?) source='(.*)'\s*$")
_VBAN_REINIT_RE = re.compile(
    r"obs-vban receive (?:anchored: source '(.*?)' frame=|worker: source '(.*)' stopped)")
_KV_RE = re.compile(r"(?:^|\s)([a-z_]+)=(-?\d+(?:\.\d+)?)(?=\s|$)")
DANTE_RX_KEY = "dante"


def _numbers(text, keys):
    """The numeric `key=value` fields of `text` that are in `keys` -> {key: int|float}."""
    out = {}
    for k, v in _KV_RE.findall(text):
        if k in keys:
            out[k] = float(v) if "." in v else int(v)
    return out


class _Receiver:
    """The window state of one receiver."""
    __slots__ = ("kind", "name", "gated", "named", "prev", "prev_pos", "reinit", "deltas",
                 "pairs", "rebases", "gap_max", "newest")

    def __init__(self, kind, name, gated, named):
        self.kind, self.name, self.gated, self.named = kind, name, gated, named
        self.prev = None
        self.prev_pos = None
        self.reinit = False
        self.deltas = {}
        self.pairs = 0
        self.rebases = 0
        self.gap_max = 0.0
        self.newest = None

    def feed(self, pos, counters, window_start):
        prev, prev_pos, reinit = self.prev, self.prev_pos, self.reinit
        self.prev, self.prev_pos, self.reinit, self.newest = counters, pos, False, pos
        if prev is None or prev_pos < window_start:
            return
        common = [k for k in counters if k in prev]
        if reinit or any(counters[k] < prev[k] for k in common):
            self.rebases += 1
            return
        self.pairs += 1
        self.gap_max = max(self.gap_max, pos - prev_pos)
        for k in self.gated + self.named:
            if k in counters and k in prev:
                self.deltas[k] = self.deltas.get(k, 0) + counters[k] - prev[k]

    def entry(self, head):
        deltas = {k: (round(v, 1) if isinstance(v, float) else v) for k, v in self.deltas.items()}
        return {"kind": self.kind, "name": self.name,
                "events": sum(v for k, v in self.deltas.items() if k in self.gated),
                "deltas": deltas, "pairs": self.pairs, "rebases": self.rebases,
                "gap_max_s": round(self.gap_max), "age_s": round(head - self.newest)}


def audio_rx_from_log(text, window_s=AUDIO_RX_WINDOW_S, tail=None):
    """issue 1460 -- the nested `audio_rx` facet: `{key: entry}` for every program-audio receiver
    with a status line in the tail, or None when there is none (no receiver on this box -> the
    facet is omitted, UNKNOWN downstream, never a fake 0). An entry: `kind` (`obs-vban` / `dante`),
    `name` (the obs-vban input / `dante-input`), `events` (the gated delta sum), `deltas` (every
    gated and named counter seen on both sides of a counted pair), `pairs` (counted pairs),
    `rebases`, `gap_max_s` (the longest gap between two counted status lines: a sender pause shows
    there) and `age_s` (the newest status line behind the log head). `tail` = a precomputed
    `timestamped_tail_lines(text)`."""
    stamped, head = tail if tail is not None else timestamped_tail_lines(text)
    if head is None:
        return None
    window_start = head - window_s
    rx = {}
    for pos, line in stamped:
        if "obs-vban receive" in line:
            m = _VBAN_STATUS_RE.search(line.rstrip("\r"))
            if m:
                key = f"obs-vban:{m.group(2)}"
                r = rx.get(key)
                if r is None:
                    r = rx[key] = _Receiver("obs-vban", m.group(2), VBAN_RX_GATED, VBAN_RX_NAMED)
                r.feed(pos, _numbers(m.group(1), VBAN_RX_GATED + VBAN_RX_NAMED + _VBAN_RX_TOTALS),
                       window_start)
                continue
            m = _VBAN_REINIT_RE.search(line)
            if m:
                r = rx.get(f"obs-vban:{m.group(1) if m.group(1) is not None else m.group(2)}")
                if r is not None:
                    r.reinit = True
            continue
        if "dante-input " not in line:
            continue
        i = line.find("dante-input status:")
        if i >= 0:
            r = rx.get(DANTE_RX_KEY)
            if r is None:
                r = rx[DANTE_RX_KEY] = _Receiver("dante", "dante-input", DANTE_RX_GATED,
                                                 DANTE_RX_NAMED)
            r.feed(pos, _numbers(line[i + len("dante-input status:"):].rstrip("\r"),
                                 DANTE_RX_GATED + DANTE_RX_NAMED + _DANTE_RX_TOTALS),
                   window_start)
        elif "dante-input start:" in line and DANTE_RX_KEY in rx:
            rx[DANTE_RX_KEY].reinit = True
    if not rx:
        return None
    return {key: rx[key].entry(head) for key in sorted(rx)}
