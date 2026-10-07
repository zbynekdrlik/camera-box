#!/usr/bin/env python3
"""Issue 1404 -- one content timeline for a YouTube VOD and stream OBS's recording, by painter tick.

Rows are (frame index, pts s, painter tick | None) from youtube_leg_ticks. The recording's
(content time = record start + pts) is the clock; a VOD frame's content time is the time the
recording first showed its tick. Everything here is pure (no I/O): the multi-part join, the window
clamp, coverage, downstream dup/skip (criterion 2) and the join after a publish (criterion 3).

Run-scoped rows (frame index, pts s, tick | None, run | None), from a decode that also reads a
reserved run (the measurement clip 911016 in the CG segments: youtube_leg_ticks `runs`), carry more
than one tick line: the clip restarts its tick on every play beside the painter's. Each line is kept
apart (issue 1404 Task 5 part b, design comment 6048239795): the recording splits into run segments
with a seam at every run change (`run_segments`), every segment has its own TickClock, every VOD row
is resolved to the segment it shows (`RunTimeline`), and a window is judged on its one segment
(TickClock, vpos, dup/skip, coverage per run and window). Legacy 3-column rows take the old code
unchanged (restreamer's gate decodes without `runs`).
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
BLIND_MIN_RUN = 2  # VOD-only undecodable runs from this many frames count as blind (clean data: only 1)
RUN = 3  # the run column of a run-scoped row (index, pts, tick, run)


def carries_runs(rows):
    """True for run-scoped rows (index, pts, tick, run) from a decode that also read a reserved run.
    Legacy rows (index, pts, tick) are read exactly as before."""
    return bool(rows) and len(rows[0]) == RUN + 1


def join_part_rows(parts, starts):
    """Join recording parts (each its own tick rows) on one content timeline.

    starts[k] is the part's record start (content seconds) or None for k >= 1: then it is placed by
    painter tick after the previous part. Returns (rows, t0, resolved starts); part k's rows get
    pts + (start_k - t0) and frame index + k * PART_STRIDE, so no adjacency crosses a seam. A
    run-scoped row keeps its run; such a part is placed by the previous part's last run only."""
    if not parts:
        raise ValueError("no recording parts")
    if starts[0] is None:
        raise ValueError("the first recording part needs its record start (@utc)")
    t0, resolved, rows = float(starts[0]), [], []
    for k, (part, start) in enumerate(zip(parts, starts)):
        if start is None:
            prev = [r for r in parts[k - 1] if r[2] is not None]
            cur = [r for r in part if r[2] is not None]
            if prev and carries_runs(prev):  # another run's ticks say nothing about this run's time
                cur = [r for r in cur if r[RUN] == prev[-1][RUN]]
            if not prev or not cur:
                raise ValueError(f"recording part {k + 1}: no decoded tick to place it by")
            dt = (cur[0][2] - prev[-1][2]) / PAINTER_HZ
            if dt <= 0:
                raise ValueError(f"recording part {k + 1}: painter tick went backwards, give @start")
            start = resolved[k - 1] + prev[-1][1] + dt - cur[0][1]
        resolved.append(float(start))
        off = resolved[k] - t0
        rows.extend((r[0] + k * PART_STRIDE, r[1] + off) + tuple(r[2:]) for r in part)
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


Segment = collections.namedtuple("Segment", "run k0 k1 restart")
Segment.__doc__ = """One run segment: rows [k0, k1) of one run; restart = it began where the run's tick fell
back (a restarted painter, the clip played again), not at a run change."""


def run_segments(rows):
    """The run segments of run-scoped rows, in order: a seam at the first decoded row of another run
    and at a tick that falls back more than RESTART_TICKS within a run (painter_restarts' rule).
    Undecoded rows stay with the segment before them (leading ones with the first)."""
    segs, cur = [], None  # cur = [run, k0, last decoded tick, restart]
    for k, r in enumerate(rows):
        if r[2] is None:
            continue
        if cur is None:
            cur = [r[RUN], 0, r[2], False]
        elif r[RUN] != cur[0] or cur[2] - r[2] > RESTART_TICKS:
            segs.append(Segment(cur[0], cur[1], k, cur[3]))
            cur = [r[RUN], k, r[2], r[RUN] == cur[0]]
        else:
            cur[2] = r[2]
    if cur is not None:
        segs.append(Segment(cur[0], cur[1], len(rows), cur[3]))
    return segs


def _consecutive_groups(items):
    """[[a, a+1, ...], ...] of a sorted list of ints."""
    groups = []
    for x in items:
        if groups and x == groups[-1][-1] + 1:
            groups[-1].append(x)
        else:
            groups.append([x])
    return groups


class RunTimeline:
    """Run-scoped rows of a recording and its VOD, every tick line kept apart.

    The recording splits into run segments (`run_segments`), each with its own TickClock. A decoded
    VOD row maps through the one segment of its run that shows its tick: the segment whose tick span
    holds it (else, at a segment edge, the one whose clock maps it within MAP_TICK_RADIUS). The
    painter counts on across its segments, so that is unique. The measurement clip restarts its tick
    on every play, so a clip tick sits in EVERY play's span: a VOD segment holding such a tick is
    resolved as a whole, by content ORDER (the VOD is a copy of the program, so its order is content
    order), against the recording segments of its run between the VOD rows already mapped around it.
    A group the order cannot pin (a different number of plays on the two sides between the same
    mapped neighbours) stays unmapped: its windows read UNKNOWN, never another play's verdict."""

    def __init__(self, rec_rows, vod_rows, t0):
        self.rec, self.vod, self.t0 = rec_rows, vod_rows, t0
        # a segment holds one run, so its rows go to the one-tick-line code as (index, pts, tick)
        self.rec3, self.vod3 = [r[:RUN] for r in rec_rows], [r[:RUN] for r in vod_rows]
        self.segs = run_segments(rec_rows)
        self.clocks = [TickClock(self.rec3[s.k0:s.k1], t0) for s in self.segs]
        self.spans = []
        for s in self.segs:
            ticks = [r[2] for r in rec_rows[s.k0:s.k1] if r[2] is not None]
            self.spans.append((min(ticks), max(ticks)))
        self.seg_of = [None] * len(rec_rows)  # recording row position -> segment
        self.by_run = {}  # run -> its segments
        for j, s in enumerate(self.segs):
            self.seg_of[s.k0:s.k1] = [j] * (s.k1 - s.k0)
            self.by_run.setdefault(s.run, []).append(j)
        self.vod_seg = [None] * len(vod_rows)  # decoded VOD row position -> the segment it shows
        self._resolve()

    def _segments_showing(self, run, tick):
        """The recording segments of `run` that show `tick` (see the class doc)."""
        same = self.by_run.get(run, [])
        held = [j for j in same if self.spans[j][0] <= tick <= self.spans[j][1]]
        return [j for j in (held or same) if self.clocks[j].time_of(tick) is not None]

    def _seg_times(self, j):
        s = self.segs[j]
        return self.t0 + self.rec[s.k0][1], self.t0 + self.rec[s.k1 - 1][1]

    def _mapped_time(self, ks):
        """Content time of the first mapped VOD row among positions `ks`, or None."""
        for k in ks:
            j = self.vod_seg[k]
            if j is not None:
                return self.clocks[j].time_of(self.vod[k][2])
        return None

    def _resolve(self):
        vsegs = run_segments(self.vod)
        ambiguous = []
        for v, vs in enumerate(vsegs):
            shown = [(k, self._segments_showing(vs.run, self.vod[k][2]))
                     for k in range(vs.k0, vs.k1) if self.vod[k][2] is not None]
            if any(len(js) > 1 for _, js in shown):
                ambiguous.append(v)
                continue
            for k, js in shown:
                if js:
                    self.vod_seg[k] = js[0]
        for group in _consecutive_groups(ambiguous):
            first, last = vsegs[group[0]], vsegs[group[-1]]
            lo = self._mapped_time(range(first.k0 - 1, -1, -1))
            hi = self._mapped_time(range(last.k1, len(self.vod)))
            for v, j in zip(group, self._order_match([vsegs[v] for v in group], lo, hi)):
                for k in range(vsegs[v].k0, vsegs[v].k1):
                    t = self.vod[k][2]
                    if t is not None and self.clocks[j].time_of(t) is not None:
                        self.vod_seg[k] = j

    def _order_match(self, group, lo, hi):
        """The recording segments a group of consecutive ambiguous VOD segments shows, in order, or []
        when the order cannot pin them. The candidates are the segments of the group's runs between the
        content times `lo` / `hi` of the mapped VOD rows around the group (None = the VOD starts / ends
        there: YouTube's start clamp drops earlier content, an early end later content)."""
        runs = {vs.run for vs in group}
        cands = sorted((j for j, s in enumerate(self.segs) if s.run in runs
                        and (lo is None or self._seg_times(j)[0] >= lo - AMBIGUOUS_S)
                        and (hi is None or self._seg_times(j)[1] <= hi + AMBIGUOUS_S)),
                       key=lambda j: self._seg_times(j)[0])
        n = len(group)
        if len(cands) != n:
            if lo is None and hi is not None and len(cands) > n:
                cands = cands[-n:]  # the VOD starts inside this stretch: it lost the earlier plays
            elif hi is None and lo is not None and len(cands) > n:
                cands = cands[:n]  # the VOD ends inside it: it lost the later plays
            else:
                return []
        if any(self.segs[j].run != vs.run for j, vs in zip(cands, group)):
            return []
        return cands

    def window_segment(self, a, b):
        """(segment, None) of the one run segment whose ticks the recording shows in content window
        [a, b), or (None, reason): a window must not hold a seam (a cut between runs, a restarted
        run). Undecoded frames at a cut say nothing about a run and do not count here (coverage
        counts them as unproven)."""
        js = sorted({self.seg_of[k] for k, r in enumerate(self.rec) if r[2] is not None and a <= self.t0 + r[1] < b})
        if not js:
            return None, "no decoded recording frame of any run in the window"
        if len(js) > 1:
            s = self.segs[js[1]]
            if s.restart:
                return None, f"the painter restarted inside the window (run {s.run}, frame {self.rec[s.k0][0]})"
            return None, (f"the window holds a cut from run {self.segs[js[0]].run} to run {s.run} at frame "
                          f"{self.rec[s.k0][0]} (one run per window)")
        return js[0], None

    def segment_at(self, at):
        """The segment of the first decoded recording row at/after content time `at`, or None."""
        for k, r in enumerate(self.rec):
            if r[2] is not None and self.t0 + r[1] >= at:
                return self.seg_of[k]
        return None

    def rec_rows_of(self, j):
        """Segment j's recording rows as (index, pts, tick)."""
        s = self.segs[j]
        return self.rec3[s.k0:s.k1]

    def vod_rows_of(self, j):
        """The VOD rows that show segment j, as (index, pts, tick): its mapped rows and the undecoded
        rows between them (frame indices kept, so adjacency and frame counts read the VOD's frames)."""
        ks = [k for k, s in enumerate(self.vod_seg) if s == j]
        if not ks:
            return []
        return [r for k, r in enumerate(self.vod3[ks[0]:ks[-1] + 1], ks[0]) if r[2] is None or self.vod_seg[k] == j]

    def content_times(self):
        """[(content time, vod row)] for every decoded VOD row mapped to a segment."""
        return [(self.clocks[j].time_of(r[2]), r) for r, j in zip(self.vod, self.vod_seg) if j is not None]


_TIMELINE = []  # [(rec rows, vod rows, t0, RunTimeline)]: the last session's, rebuilt only for new rows


def run_timeline(rec_rows, vod_rows, t0):
    """The RunTimeline of these rows (the verdict asks for it once per criterion and window)."""
    if _TIMELINE and _TIMELINE[0][0] is rec_rows and _TIMELINE[0][1] is vod_rows and _TIMELINE[0][2] == t0:
        return _TIMELINE[0][3]
    tl = RunTimeline(rec_rows, vod_rows, t0)
    _TIMELINE[:] = [(rec_rows, vod_rows, t0, tl)]
    return tl


def vod_content_times(rec_rows, vod_rows, t0):
    """[(content time, vod row)] for every decoded VOD frame that maps to the recording (run-scoped
    rows: through the segment it shows)."""
    if carries_runs(rec_rows):
        return run_timeline(rec_rows, vod_rows, t0).content_times()
    return _vod_content_times(rec_rows, vod_rows, t0)


def _vod_content_times(rec_rows, vod_rows, t0):
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
    frame showing the same tick (nearest VOD tick within 40, corrected by the tick difference); for
    run-scoped rows, within the run segment of that recording frame."""
    if carries_runs(rec_rows):
        tl = run_timeline(rec_rows, vod_rows, t0)
        j = tl.segment_at(at)
        if j is None:
            return None, None
        return _vod_pts_for(tl.rec_rows_of(j), tl.vod_rows_of(j), t0, at)
    return _vod_pts_for(rec_rows, vod_rows, t0, at)


def _vod_pts_for(rec_rows, vod_rows, t0, at):
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
    """The part of [a, b) the VOD covers: (a', b', clamped_start | None, clamped_end | None); for
    run-scoped rows, by the VOD frames of the window's run segment (None when the window has none)."""
    if carries_runs(rec_rows):
        tl = run_timeline(rec_rows, vod_rows, t0)
        j, _ = tl.window_segment(a, b)
        return None if j is None else _clamp_window(tl.rec_rows_of(j), tl.vod_rows_of(j), t0, a, b)
    return _clamp_window(rec_rows, vod_rows, t0, a, b)


def _clamp_window(rec_rows, vod_rows, t0, a, b):
    times = [c for c, _ in _vod_content_times(rec_rows, vod_rows, t0)]
    if not times:
        return None
    first, last = min(times), max(times) + FRAME_S
    return max(a, first), min(b, last), (first if first > a else None), (last if last < b else None)


def painter_restarts(rows):
    """Frame indices where the decoded painter tick falls back more than RESTART_TICKS."""
    dec = [r for r in rows if r[2] is not None]
    return [r1[0] for r0, r1 in zip(dec, dec[1:]) if r0[2] - r1[2] > RESTART_TICKS]


def _single_anchors(rows, positions):
    """{tick: frame index} of the ticks a file shows exactly once, between two decoded, index-adjacent
    frames that show other ticks: an unambiguous point to count frames from."""
    out = {}
    for tick, ks in positions.items():
        if len(ks) != 1:
            continue
        k = ks[0]
        i = rows[k][0]
        if (0 < k < len(rows) - 1 and rows[k - 1][0] == i - 1 and rows[k + 1][0] == i + 1
                and rows[k - 1][2] not in (None, tick) and rows[k + 1][2] not in (None, tick)):
            out[tick] = i
    return out


def _adjacent_events(rec_rows, rw_ticks, pos, vdec, lo, hi):
    """dup / skip between ADJACENT decoded VOD frames (the session tool's judgement), each tagged with
    the VOD frame index it ends at; and the number of same-tick pairs nobody could judge."""
    rec_sorted = sorted(rw_ticks)

    def judged_once(k, tick):
        i = rec_rows[k][0]
        prev_ok = k > 0 and rec_rows[k - 1][2] not in (None, tick) and rec_rows[k - 1][0] == i - 1
        next_ok = k + 1 < len(rec_rows) and rec_rows[k + 1][2] not in (None, tick) and rec_rows[k + 1][0] == i + 1
        return prev_ok and next_ok

    events, unjudged = [], 0
    for (i0, _, a0), (i1, p1, a1) in zip(vdec, vdec[1:]):
        if i1 - i0 != 1 or max(a0, a1) < lo or min(a0, a1) > hi:
            continue
        if a1 < a0:  # the VOD replays content: every window recording tick it shows again is a dup
            n = bisect.bisect_right(rec_sorted, a0) - bisect.bisect_left(rec_sorted, a1)
            if n:
                events.append({"kind": "dup", "vod_i": i1, "vod_pts": round(p1, 3), "replay_from_tick": a0,
                               "to_tick": a1, "frames": n})
        elif a1 == a0:
            ks = pos.get(a0, [])
            if len(ks) >= 2:
                continue  # the rig repeated this tick itself: cancels out
            if len(ks) == 1 and judged_once(ks[0], a0):
                events.append({"kind": "dup", "vod_i": i1, "vod_pts": round(p1, 3), "tick": a0, "frames": 1})
            else:
                unjudged += 1
        else:
            n = bisect.bisect_left(rec_sorted, a1) - bisect.bisect_right(rec_sorted, a0)
            if n:
                events.append({"kind": "skip", "vod_i": i1, "vod_pts": round(p1, 3), "from_tick": a0,
                               "to_tick": a1, "frames": n})
    return events, unjudged


def _segment_balance(rec_anchor, vod_anchor, events):
    """Hidden dup / skip events from the frame-count balance between consecutive anchors.

    Between two ticks both files show unambiguously, the VOD must hold exactly as many frames as the
    recording: a rig repeat or skip is in both and cancels, an undecodable frame still counts. The
    difference minus what the adjacent judgement already counted there is a dup (more VOD frames) or
    skip (fewer) hidden behind undecodable VOD frames, however long the undecodable stretch."""
    both = sorted(set(rec_anchor) & set(vod_anchor))
    hidden, segments = [], 0
    for ta, tb in zip(both, both[1:]):
        ra, rb, va, vb = rec_anchor[ta], rec_anchor[tb], vod_anchor[ta], vod_anchor[tb]
        if vb <= va or ra // PART_STRIDE != rb // PART_STRIDE:
            continue
        segments += 1
        local = sum((e["frames"] if e["kind"] == "dup" else -e["frames"]) for e in events if va < e["vod_i"] <= vb)
        residual = (vb - va) - (rb - ra) - local
        if residual:
            hidden.append({"kind": "dup" if residual > 0 else "skip", "vod_i": vb, "hidden": True,
                           "between_ticks": [ta, tb], "frames": abs(residual)})
    return hidden, segments, (both[0], both[-1]) if both else None


def _blind_runs(rec_rows, pos, vdec, lo, hi):
    """Total seconds of VOD runs (BLIND_MIN_RUN frames or more) that decode nothing where the
    recording's same content decodes (>= 80 %): a VOD that lost the picture (black, a slate, a flash)
    while keeping its frame count. In the clean real windows every such run is one frame."""
    total = 0
    for (i0, _, a0), (i1, _, a1) in zip(vdec, vdec[1:]):
        run = i1 - i0 - 1
        if run < BLIND_MIN_RUN or not (lo <= a0 < a1 <= hi) or not pos.get(a0) or not pos.get(a1):
            continue
        k0, k1 = max(pos[a0]), min(pos[a1])
        between = rec_rows[k0 + 1:k1]
        if between and sum(r[2] is not None for r in between) >= 0.8 * len(between):
            total += run
    return round(total * FRAME_S, 3)


def timestamp_gaps(rows):
    """Frame indices where the pts step leaves 0.5..1.5 x the median: a frame the encoder skipped."""
    steps = [(r1[0], r1[1] - r0[1]) for r0, r1 in zip(rows, rows[1:]) if r1[0] == r0[0] + 1]
    if not steps:
        return []
    med = sorted(s for _, s in steps)[len(steps) // 2]
    return [i for i, s in steps if not 0.5 * med <= s <= 1.5 * med]


def dupskip(rec_rows, vod_rows, t0, a, b):
    """Downstream dup/skip of the VOD against the recording over content window [a, b).

    Two judgements, both by painter tick:
      - adjacent decoded VOD frames (the session tool's): a tick shown twice is a dup when the
        recording shows it once between two decoded, adjacent, different frames (a tick the rig
        itself repeated cancels out); a tick jump skips every window recording tick in between; a
        backward jump replays content, every window recording tick in the replayed range is a dup;
      - the frame-count balance between consecutive anchors (ticks both files show unambiguously):
        whatever the adjacent judgement could not see behind undecodable VOD frames, however many.
    Also reported: the longest stretch the VOD alone cannot decode, how early the VOD ends against
    the recording's own last decoded frame, and the seconds of the window no anchor pair covers.
    The window is clamped to the content the VOD covers; a window outside the recording, or with a
    painter restart inside, is an error (never a verdict).

    Run-scoped rows: the window must lie in ONE run segment (a cut between runs or a restarted run
    inside is an error, like a restart), and both judgements run on that segment's recording rows
    and the VOD rows that show it, through its own TickClock. The result names the run."""
    out = _window_check(rec_rows, t0, a, b)
    if out is not None:
        return out
    if carries_runs(rec_rows):
        tl = run_timeline(rec_rows, vod_rows, t0)
        j, why = tl.window_segment(a, b)
        if j is None:
            return {"error": why, "dup": None, "skip": None, "run": None}
        return dict(_dupskip(tl.rec_rows_of(j), tl.vod_rows_of(j), t0, a, b), run=tl.segs[j].run)
    return _dupskip(rec_rows, vod_rows, t0, a, b)


def _window_check(rec_rows, t0, a, b):
    """The error of a window that is not inside the recording, or None."""
    if not rec_rows:
        return {"error": "no recording rows", "dup": None, "skip": None}
    rec_first, rec_last = t0 + rec_rows[0][1], t0 + rec_rows[-1][1] + FRAME_S
    if a < rec_first - END_SLACK_S or b > rec_last + END_SLACK_S:
        return {"error": f"the window is not inside the recording ({a - rec_first:+.1f} s / {b - rec_last:+.1f} s)",
                "dup": None, "skip": None}
    return None


def _dupskip(rec_rows, vod_rows, t0, a, b):
    """dupskip on one tick line; the caller checked the window is inside the recording."""
    rec_last = t0 + rec_rows[-1][1] + FRAME_S
    span = _clamp_window(rec_rows, vod_rows, t0, a, b)
    if span is None or span[1] <= span[0]:
        return {"error": "the VOD has no frame of this window", "dup": None, "skip": None}
    a2, b2, cl_start, cl_end = span
    in_win = [r for r in rec_rows if a2 <= t0 + r[1] < b2]
    rw = [r for r in in_win if r[2] is not None]
    if not rw:
        return {"error": "no decoded recording frame in the window", "dup": None, "skip": None}
    if painter_restarts(in_win):
        return {"error": "the painter restarted inside the window", "dup": None, "skip": None}
    gaps = timestamp_gaps(in_win)
    if gaps:  # the recording's own encoder skipped a frame the stream encoder may have kept
        return {"error": f"the recording has a timestamp gap at frame {gaps[0]} (a frame its encoder skipped)",
                "dup": None, "skip": None}
    lo, hi = min(r[2] for r in rw), max(r[2] for r in rw)
    pos = collections.defaultdict(list)  # tick -> recording row positions, only near the window
    for k, r in enumerate(rec_rows):
        if r[2] is not None and a2 - NEAR_S <= t0 + r[1] < b2 + NEAR_S:
            pos[r[2]].append(k)
    vpos = collections.defaultdict(list)  # tick -> VOD row positions, the window's ticks only
    for k, v in enumerate(vod_rows):
        if v[2] is not None and lo <= v[2] <= hi:
            vpos[v[2]].append(k)
    vdec = [v for v in vod_rows if v[2] is not None]
    events, unjudged = _adjacent_events(rec_rows, {r[2] for r in rw}, pos, vdec, lo, hi)
    rec_anchor = {t: i for t, i in _single_anchors(rec_rows, pos).items() if lo <= t <= hi}
    hidden, segments, ends = _segment_balance(rec_anchor, _single_anchors(vod_rows, vpos), events)
    if not segments:
        return {"error": "no two frames both files show unambiguously in the window", "dup": None, "skip": None}
    first_t, last_t = (t0 + rec_rows[pos[t][0]][1] for t in ends)
    rec_dec_last = max(t0 + r[1] for r in rec_rows if r[2] is not None and a <= t0 + r[1] < b) + FRAME_S
    early = None
    if cl_end is not None:
        early = round(max(0.0, min(b, rec_dec_last) - cl_end), 3)
    allev = events + hidden
    return {"dup": sum(e["frames"] for e in allev if e["kind"] == "dup"),
            "skip": sum(e["frames"] for e in allev if e["kind"] == "skip"),
            "hidden": sum(e["frames"] for e in hidden), "unjudged": unjudged, "segments": segments,
            "unanchored_s": round(max(0.0, first_t - a2) + max(0.0, b2 - FRAME_S - last_t), 3),
            "vod_blind_s": _blind_runs(rec_rows, pos, vdec, lo, hi),
            "rec_frames": len(rw), "vod_frames": sum(1 for v in vdec if lo <= v[2] <= hi),
            "start_utc": a2, "end_utc": b2, "coverage_end_utc": min(b, rec_last),
            "clamped_start_utc": cl_start, "clamped_end_utc": cl_end, "vod_ends_early_s": early,
            "dups": [e for e in allev if e["kind"] == "dup"][:DETAIL_LIMIT],
            "skips": [e for e in allev if e["kind"] == "skip"][:DETAIL_LIMIT]}


def coverage(rec_rows, vod_rows, t0, a, b):
    """Decodable % and cadence-proven % of the recording and the VOD over content window [a, b).

    Run-scoped rows: counted per run segment the window touches (each through the VOD rows that
    show it) and summed, so a cut between two tick lines is never read as a cadence event and the
    frames at a cut no run decodes count as unproven."""
    if not carries_runs(rec_rows):
        return _coverage_pct(*_coverage_counts(rec_rows, vod_rows, t0, a, b))
    tl = run_timeline(rec_rows, vod_rows, t0)
    rc, vc = continuity([]), continuity([])
    for j in sorted({j for j, r in zip(tl.seg_of, rec_rows) if j is not None and a <= t0 + r[1] < b}):
        rj, vj = _coverage_counts(tl.rec_rows_of(j), tl.vod_rows_of(j), t0, a, b)
        rc = {k: rc[k] + rj[k] for k in rc}
        vc = {k: vc[k] + vj[k] for k in vc}
    return _coverage_pct(rc, vc)


def _coverage_counts(rec_rows, vod_rows, t0, a, b):
    """The continuity counts of the recording's window rows and of the VOD stretch showing their ticks."""
    rw = [r for r in rec_rows if a <= t0 + r[1] < b]
    rc = continuity(rw)
    ticks = [r[2] for r in rw if r[2] is not None]
    vc = continuity([])
    if ticks:
        lo, hi = min(ticks), max(ticks)
        idx = [k for k, v in enumerate(vod_rows) if v[2] is not None and lo <= v[2] <= hi]
        if idx:
            vc = continuity(vod_rows[idx[0]: idx[-1] + 1])
    return rc, vc


def _coverage_pct(rc, vc):
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
