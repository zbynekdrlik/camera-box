#!/usr/bin/env python3
"""Issue 1404 -- one content timeline for a YouTube VOD and stream OBS's recording, by painter tick.

Rows are (frame index, pts s, painter tick | None) from youtube_leg_ticks. The recording's
(content time = record start + pts) is the clock; a VOD frame's content time is the time the
recording first showed its tick. Everything here is pure (no I/O): the multi-part join, the window
clamp, coverage, downstream dup/skip (criterion 2) and the join after a publish (criterion 3).
"""
import bisect
import collections

PAINTER_HZ = 60
FRAME_S = 1.0 / 30
MAX_JUDGED_GAP = 30  # frames: a longer decode gap is unjudged, never proven, never an event
PART_STRIDE = 10_000_000  # frame-index offset per recording part: no adjacency across a part seam
MAP_TICK_RADIUS = 120  # a tick maps to content time via a recording tick at most 2 s away
AMBIGUOUS_S = 2.0  # a tick the recording shows at two times this far apart (a painter restart) never maps
RESTART_TICKS = 1800  # a decoded tick that falls back this far (30 s) is a painter restart
END_SLACK_S = 2.0  # a window may reach this far past the recording's own first/last frame
NEAR_S = 1.0  # recording rows this near the window are the only ones its dup/skip reads
DETAIL_LIMIT = 20


def join_part_rows(parts, starts):
    """Join recording parts (each its own tick rows) on one content timeline.

    starts[k] is the part's record start (content seconds) or None for k >= 1: then it is placed by
    painter tick after the previous part. Returns (rows, t0, resolved starts); part k's rows get
    pts + (start_k - t0) and frame index + k * PART_STRIDE, so no adjacency crosses a seam."""
    if not parts:
        raise ValueError("no recording parts")
    if starts[0] is None:
        raise ValueError("the first recording part needs its record start (@utc)")
    t0, resolved, rows = float(starts[0]), [], []
    for k, (part, start) in enumerate(zip(parts, starts)):
        if start is None:
            prev = [r for r in parts[k - 1] if r[2] is not None]
            cur = [r for r in part if r[2] is not None]
            if not prev or not cur:
                raise ValueError(f"recording part {k + 1}: no decoded tick to place it by")
            dt = (cur[0][2] - prev[-1][2]) / PAINTER_HZ
            if dt <= 0:
                raise ValueError(f"recording part {k + 1}: painter tick went backwards, give @start")
            start = resolved[k - 1] + prev[-1][1] + dt - cur[0][1]
        resolved.append(float(start))
        off = resolved[k] - t0
        rows.extend((i + k * PART_STRIDE, p + off, t) for i, p, t in part)
    return rows, t0, resolved


class TickClock:
    """Content time of a painter tick: when the recording first showed it (t0 + pts). A tick shown
    at two times > AMBIGUOUS_S apart (a restarted painter counting again) maps to nothing."""

    def __init__(self, rec_rows, t0):
        first, ambiguous = {}, set()
        for _, p, t in rec_rows:
            if t is None:
                continue
            if t not in first:
                first[t] = t0 + p
            elif t0 + p - first[t] > AMBIGUOUS_S:
                ambiguous.add(t)
        self.ticks = sorted(first)
        self.times = [first[t] for t in self.ticks]
        self.ambiguous = ambiguous

    def time_of(self, tick):
        k = bisect.bisect_left(self.ticks, tick)
        near = [j for j in (k - 1, k) if 0 <= j < len(self.ticks) and abs(self.ticks[j] - tick) <= MAP_TICK_RADIUS]
        if not near:
            return None
        j = min(near, key=lambda j: abs(self.ticks[j] - tick))
        if self.ticks[j] in self.ambiguous:
            return None
        return self.times[j] + (tick - self.ticks[j]) / PAINTER_HZ


def vod_content_times(rec_rows, vod_rows, t0):
    """[(content time, vod row)] for every decoded VOD frame that maps to the recording."""
    clock = TickClock(rec_rows, t0)
    out = []
    for r in vod_rows:
        if r[2] is not None:
            c = clock.time_of(r[2])
            if c is not None:
                out.append((c, r))
    return out


def vod_pts_for(rec_rows, vod_rows, t0, at):
    """(rec pts, vod pts) of the first decoded recording frame at/after content time `at` and the VOD
    frame showing the same tick (nearest VOD tick within 40, corrected by the tick difference)."""
    by_tick = {}
    for _, p, t in vod_rows:
        if t is not None:
            by_tick.setdefault(t, p)
    for _, p, t in rec_rows:
        if t is None or t0 + p < at:
            continue
        for d in range(0, 40):
            for tt in (t + d, t - d):
                if tt in by_tick:
                    return p, by_tick[tt] - (tt - t) / PAINTER_HZ
        return p, None
    return None, None


def continuity(rows, step=2):
    """frames / decodable / cadence-proven frames / tick-step events of a contiguous row span.

    Frames are counted by frame-index span (per recording part), so rows missing from a decode count
    as frames nobody proved. Consecutive decoded frames prove the frames between them when the tick
    advanced by step x frame gap; a mismatch is an event; a gap over MAX_JUDGED_GAP frames (or a
    part seam) is unjudged and starts a new span, whose first decoded frame counts once."""
    spans = collections.defaultdict(list)
    for r in rows:
        spans[r[0] // PART_STRIDE].append(r[0])
    frames = sum(max(v) - min(v) + 1 for v in spans.values())
    dec = [r for r in rows if r[2] is not None]
    proven, events, unjudged = (1 if dec else 0), 0, 0
    for (i0, _, t0), (i1, _, t1) in zip(dec, dec[1:]):
        gap = i1 - i0
        if gap > MAX_JUDGED_GAP:
            unjudged += 1
            proven += 1
        elif t1 - t0 == step * gap:
            proven += gap
        else:
            events += 1
    return {"frames": frames, "decodable": len(dec), "cadence_proven": min(proven, frames),
            "events": events, "unjudged_gaps": unjudged}


def _pct(n, d):
    return round(100.0 * n / d, 1) if d else 0.0


def clamp_window(rec_rows, vod_rows, t0, a, b):
    """The part of [a, b) the VOD covers: (a', b', clamped_start | None, clamped_end | None)."""
    times = [c for c, _ in vod_content_times(rec_rows, vod_rows, t0)]
    if not times:
        return None
    first, last = min(times), max(times) + FRAME_S
    return max(a, first), min(b, last), (first if first > a else None), (last if last < b else None)


def painter_restarts(rows):
    """Frame indices where the decoded painter tick falls back more than RESTART_TICKS."""
    dec = [r for r in rows if r[2] is not None]
    return [r1[0] for r0, r1 in zip(dec, dec[1:]) if r0[2] - r1[2] > RESTART_TICKS]


def dupskip(rec_rows, vod_rows, t0, a, b):
    """Downstream dup/skip of the VOD against the recording over content window [a, b).

    Pairs of consecutive decoded VOD frames (at most MAX_JUDGED_GAP apart) are judged:
      - same tick: the VOD shows it (frame gap + 1) times; each showing more than the recording's is
        a dup, judged when the recording shows the tick once between two decoded, adjacent,
        different frames, or repeats it itself (a rig repeat cancels out);
      - tick forward: the recording frames between the two ticks (by recording frame index, so an
        undecodable recording frame still counts) against the VOD frames between them: more on the
        recording = skips, more on the VOD = dups (a repeat hidden behind an undecodable frame);
        an adjacent pair straddling the window edge counts the window's recording ticks it jumped;
      - tick backward: the VOD replays content; every window recording tick in the replayed range
        is a dup.
    The window is clamped to the content the VOD covers; a window outside the recording, or with a
    painter restart inside, is an error (never a verdict)."""
    if not rec_rows:
        return {"error": "no recording rows", "dup": None, "skip": None}
    rec_first, rec_last = t0 + rec_rows[0][1], t0 + rec_rows[-1][1] + FRAME_S
    if a < rec_first - END_SLACK_S or b > rec_last + END_SLACK_S:
        return {"error": f"the window is not inside the recording ({a - rec_first:+.1f} s / {b - rec_last:+.1f} s)",
                "dup": None, "skip": None}
    span = clamp_window(rec_rows, vod_rows, t0, a, b)
    if span is None or span[1] <= span[0]:
        return {"error": "the VOD has no frame of this window", "dup": None, "skip": None}
    a2, b2, cl_start, cl_end = span
    in_win = [r for r in rec_rows if a2 <= t0 + r[1] < b2]
    rw = [r for r in in_win if r[2] is not None]
    if not rw:
        return {"error": "no decoded recording frame in the window", "dup": None, "skip": None}
    if painter_restarts(in_win):
        return {"error": "the painter restarted inside the window", "dup": None, "skip": None}
    lo, hi = min(r[2] for r in rw), max(r[2] for r in rw)
    rec_sorted = sorted({r[2] for r in rw})
    pos = collections.defaultdict(list)  # tick -> recording row positions, only near the window
    for k, r in enumerate(rec_rows):
        if r[2] is not None and a2 - NEAR_S <= t0 + r[1] < b2 + NEAR_S:
            pos[r[2]].append(k)

    def judged_once(k, tick):
        i = rec_rows[k][0]
        prev_ok = k > 0 and rec_rows[k - 1][2] not in (None, tick) and rec_rows[k - 1][0] == i - 1
        next_ok = k + 1 < len(rec_rows) and rec_rows[k + 1][2] not in (None, tick) and rec_rows[k + 1][0] == i + 1
        return prev_ok and next_ok

    vdec = [v for v in vod_rows if v[2] is not None]
    dups, skips, unjudged = [], [], 0
    for (i0, _, a0), (i1, p1, a1) in zip(vdec, vdec[1:]):
        g = i1 - i0
        if g > MAX_JUDGED_GAP or (max(a0, a1) < lo or min(a0, a1) > hi):
            continue
        if a1 < a0:
            n = bisect.bisect_right(rec_sorted, a0) - bisect.bisect_left(rec_sorted, a1)
            if n > 0:
                dups.append({"vod_pts": round(p1, 3), "replay_from_tick": a0, "to_tick": a1, "frames": n})
        elif a1 == a0:
            # the VOD shows the tick at least g + 1 times; a tick the rig itself repeated cancels out
            ks = pos.get(a0, [])
            if len(ks) >= 2 or (len(ks) == 1 and judged_once(ks[0], a0)):
                if g + 1 > len(ks):
                    dups.append({"vod_pts": round(p1, 3), "tick": a0, "frames": g + 1 - len(ks)})
            else:
                unjudged += 1
        elif a1 > a0 and g == 1:
            n = bisect.bisect_left(rec_sorted, a1) - bisect.bisect_right(rec_sorted, a0)
            if n > 0:
                skips.append({"vod_pts": round(p1, 3), "from_tick": a0, "to_tick": a1, "frames": n})
        elif lo <= a0 and a1 <= hi and pos.get(a0) and pos.get(a1):
            k0, k1 = max(pos[a0]), min(pos[a1])
            rec_between = rec_rows[k1][0] - rec_rows[k0][0] - 1
            if not 0 <= rec_between <= MAX_JUDGED_GAP:
                unjudged += 1
                continue
            d = rec_between - (g - 1)
            if d > 0:
                skips.append({"vod_pts": round(p1, 3), "from_tick": a0, "to_tick": a1, "frames": d})
            elif d < 0:
                dups.append({"vod_pts": round(p1, 3), "tick": a0, "to_tick": a1, "frames": -d})
        else:
            unjudged += 1
    return {"dup": sum(d["frames"] for d in dups), "skip": sum(s["frames"] for s in skips), "unjudged": unjudged,
            "rec_frames": len(rw), "vod_frames": sum(1 for v in vdec if lo <= v[2] <= hi),
            "start_utc": a2, "end_utc": b2, "clamped_start_utc": cl_start, "clamped_end_utc": cl_end,
            "vod_ends_early_s": round(b - cl_end, 3) if cl_end is not None else None,
            "dups": dups[:DETAIL_LIMIT], "skips": skips[:DETAIL_LIMIT]}


def coverage(rec_rows, vod_rows, t0, a, b):
    """Decodable % and cadence-proven % of the recording and the VOD over content window [a, b)."""
    rw = [r for r in rec_rows if a <= t0 + r[1] < b]
    rc = continuity(rw)
    ticks = [r[2] for r in rw if r[2] is not None]
    vc = continuity([])
    if ticks:
        lo, hi = min(ticks), max(ticks)
        idx = [k for k, v in enumerate(vod_rows) if v[2] is not None and lo <= v[2] <= hi]
        if idx:
            vc = continuity(vod_rows[idx[0]: idx[-1] + 1])
    return {"rec_frames": rc["frames"], "rec_decodable_pct": _pct(rc["decodable"], rc["frames"]),
            "rec_cadence_pct": _pct(rc["cadence_proven"], rc["frames"]), "rec_events": rc["events"],
            "vod_frames": vc["frames"], "vod_decodable_pct": _pct(vc["decodable"], vc["frames"]),
            "vod_cadence_pct": _pct(vc["cadence_proven"], vc["frames"]), "vod_events": vc["events"]}


def publish_gaps(rec_rows, vod_rows, t0, publishes_utc):
    """Per publish: the last VOD frame before it, the first VOD frame at/after it (content time) and
    the gap. A publish with no VOD frame before it opened the VOD (YouTube starts a VOD at its own
    live transition, 24 s / 42 s after the first publish in sessions 2 and 3): judged False."""
    times = sorted(c for c, _ in vod_content_times(rec_rows, vod_rows, t0))
    out = []
    for p in publishes_utc:
        k = bisect.bisect_left(times, p)
        after = times[k] if k < len(times) else None
        before = times[k - 1] if k > 0 else None
        out.append({"utc": p, "last_vod_frame_before_utc": before, "first_vod_frame_utc": after,
                    "gap_s": None if after is None else round(after - p, 3), "judged": before is not None})
    return out
