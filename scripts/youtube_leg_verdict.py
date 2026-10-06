#!/usr/bin/env python3
"""Issue 1404 -- the YouTube-leg verdict: does the YouTube VOD show what stream OBS sent?

Each measured window compares the YouTube VOD of a run against stream OBS's own program recording of
the same run. Both files carry the cam2 painter tick (60 Hz, dual-QR Vernier) on every frame and the
cam2 QPSK marker in the audio, so everything is compared by CONTENT (painter tick), never by a shared
clock. Ported from the three manual sessions (issue 1404 comments 6006986090, 6008636005) with two
changes: the tick decode reads BOTH QR halves (the painter puts a colour-coded QR on the left at
times, which the gray detector cannot read; the right QR then still decodes), and the window start
clamps to the first frame present in both files (YouTube starts a VOD at its own live transition).

Criteria (docs/superpowers/specs/2026-10-06-youtube-leg-e2e-design.md, the PASS bar):
  1. A/V: per window VOD - recording `recording-verdict --av-sync` offset within +/-150 ms of the
     run's first window (YouTube's own fixed term varies per session, so the bar is relative).
  2. 0 downstream dup/skip by painter tick (a tick the rig itself repeated cancels out; a skip counts
     only between adjacent decoded VOD frames; a dup only when the recording shows that tick exactly
     once with decoded neighbours).
  3. The first VOD frame after every (re)publish is within 0.5 s of the publish, in content time.
     A publish with no VOD frame before it opened the VOD (YouTube starts the VOD at its own live
     transition): its join is reported, not judged.
  4. Audio continuous: 0.25 s block cross-correlation, no lag jump > 1.5 ms, no low-correlation block
     (< 0.6) while the recording has signal, no silent VOD block, no level drop > 10 dB.
  5. Coverage: a window under 90 % cadence-proven frames (either file), or one the VOD covers under
     half of, is UNKNOWN, never PASS.
Overall PASS only when every window passes every criterion; a tool/decode/download error is UNKNOWN;
a proven FAIL anywhere wins over an UNKNOWN.

CLI (shared with restreamer's release gate):
  youtube_leg_verdict.py --vod <youtube id | local file> --recording <file>[@<record-start-utc>] ...
                         --markers <cam2 qpsk marker csv | http url> --windows <name:start:end> ...
                         --publish <utc> ... --out <dir> [--probe-bin <recording-verdict>]
  exit 0 = PASS, 1 = FAIL, 2 = UNKNOWN; writes <dir>/youtube-leg-verdict.json.
  Timestamps: epoch seconds, ISO 8601 (2026-10-06T01:40:15.901Z) or compact (20261006T014015.901Z).
  More than one --recording = parts of one session split by an OBS restart; a part without @start is
  placed after the previous one by painter tick. Windows must lie inside one publish span.
  `--decode-ticks <file>` (diagnostic) prints a file's per-frame tick map and exits.
"""
import argparse
import bisect
import collections
import datetime
import gzip
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request

import numpy as np

SCHEMA = 1
EXIT_PASS, EXIT_FAIL, EXIT_UNKNOWN = 0, 1, 2

PAINTER_HZ = 60
PAINTER_QR = re.compile(r"^P(\d{6,})\.(\d+)\.(\d+)\.\d+$")
NODE_BURN_PREFIX = "9110"  # node/origin burns (911001.., 911014..) are never the painter tick
QR_TOP_FRACTION = 0.62  # the painter's two big QRs sit in the top 62 % of the frame
DECODE_SCALE = 0.5
MAX_JUDGED_GAP = 30  # frames: a longer decode gap is unjudged, never proven, never an event
PHASE_RADIUS = 60  # frames: a right-only frame takes its capture phase from both-halves frames this near
PART_STRIDE = 10_000_000  # frame-index offset per recording part: no adjacency across a part seam
MAP_TICK_RADIUS = 120  # a tick maps to content time via a recording tick at most 2 s away
FRAME_S = 1.0 / 30

AV_TOLERANCE_MS = 150.0
AV_CLIP_S = 40.0
AV_MIN_MARKER_FRACTION = 0.5  # of the clip's expected markers (one per 0.5 s)
PUBLISH_GAP_MAX_S = 0.5
CADENCE_MIN_PCT = 90.0
MIN_WINDOW_COVERED = 0.5  # a window the VOD covers under half of is UNKNOWN

SR = 16000
BLOCK = int(0.25 * SR)
FIRST_SEARCH = int(0.20 * SR)
TRACK_SEARCH = int(0.040 * SR)
LOCK_REF = int(4.0 * SR)
LOCK_SEARCH = int(2.5 * SR)
LAG_JUMP = int(round(0.0015 * SR))
LOW_CORR = 0.6
REC_SIGNAL_DBFS = -50.0
SILENT_REC_DBFS = -60.0
SILENT_VOD_DBFS = -70.0
LEVEL_DROP_DB = 10.0
DETAIL_LIMIT = 20
AUDIO_KEYS = ("lag_jumps", "low_corr", "silent", "level_drops")


# ---------------------------------------------------------------- painter tick decode

def painter_tick(texts):
    """The painter tick among decoded QR texts (the lowest painter payload), or None."""
    best = None
    for t in texts or ():
        m = PAINTER_QR.match(t or "")
        if m and not m.group(1).startswith(NODE_BURN_PREFIX):
            tick = int(m.group(2))
            best = tick if best is None else min(best, tick)
    return best


def _qr_tick(det, plane, scale):
    import cv2

    if scale != 1.0:
        plane = cv2.resize(plane, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA)
    try:
        ok, texts, _, _ = det.detectAndDecodeMulti(plane)
    except cv2.error as e:  # one unreadable half is an undecodable half, never a crash
        print(f"youtube_leg_verdict: QR detector error on a frame half: {e}", file=sys.stderr)
        return None
    return painter_tick(texts) if ok else None


def _half_tick(det, img, scale):
    """Painter tick of one QR half: gray first, then the BLUE channel. The camera captures the half
    the painter just repainted mid-transition as a green/blue colour pattern the gray detector
    cannot read; its blue channel still reads (session 3 part 1: left 0/120 gray, 99/120 blue)."""
    import cv2

    tick = _qr_tick(det, cv2.cvtColor(img, cv2.COLOR_BGR2GRAY), scale)
    if tick is None:
        tick = _qr_tick(det, np.ascontiguousarray(img[:, :, 0]), scale)
    return tick


def half_ticks(frame, det, scale=DECODE_SCALE):
    """(left tick | None, right tick | None) of one BGR frame (the top QR band, split in halves)."""
    h, w = frame.shape[:2]
    top = frame[0:int(h * QR_TOP_FRACTION)]
    return _half_tick(det, top[:, : w // 2], scale), _half_tick(det, top[:, w // 2:], scale)


def resolve_ticks(raw, radius=PHASE_RADIUS):
    """Per-frame (index, pts, even tick | None, half) from raw (index, pts, left, right) halves.

    The Vernier paints the latest EVEN tick on the LEFT and the latest ODD tick on the RIGHT
    (painter::vernier_ids), so a frame showing painter tick T reads right = left + 1 when T is odd
    and right = left - 1 when T is even. The even tick (the left's value) is the frame's tick:
      'B' both halves agree (|right - left| = 1): tick = left, and the frame's PHASE is known;
      'L' left only: tick = left;
      'R' right only: tick = right - 1 (odd phase) or right + 1 (even phase), the phase read from
          the nearest both-halves frames within `radius` frames; the camera's capture phase moves
          over a session, so a fixed assumption is wrong for whole stretches;
      'r' right only and no phase (none near, or the two sides disagree: a phase step): no tick.
    """
    phase = {}
    for i, _, left, right in raw:
        if left is not None and right is not None and abs(right - left) == 1:
            phase[i] = 1 if right > left else 0
    known = sorted(phase)
    rows = []
    for i, p, left, right in raw:
        if left is not None:
            rows.append((i, p, left, "B" if i in phase else "L"))
            continue
        if right is None:
            rows.append((i, p, None, ""))
            continue
        k = bisect.bisect_left(known, i)
        near = {phase[known[j]] for j in (k - 1, k) if 0 <= j < len(known) and abs(known[j] - i) <= radius}
        if len(near) == 1:
            rows.append((i, p, right - 1 if near.pop() else right + 1, "R"))
        else:
            rows.append((i, p, None, "r"))
    return rows


def _decode_range(job):
    import cv2

    path, start, end, scale = job
    cap = cv2.VideoCapture(path)
    cap.set(cv2.CAP_PROP_POS_FRAMES, start)
    det = cv2.QRCodeDetector()
    out = []
    for i in range(start, end):
        ok, frame = cap.read()
        if not ok:
            break
        pts = cap.get(cv2.CAP_PROP_POS_MSEC) / 1000.0
        out.append((i, pts) + half_ticks(frame, det, scale))
    cap.release()
    return out


def decode_ticks(path, workers=4, scale=DECODE_SCALE):
    """Per-frame (index, pts s, even tick | None, half 'B'/'L'/'R'/'r'/'') of a video file."""
    import cv2

    cap = cv2.VideoCapture(str(path))
    if not cap.isOpened():
        raise RuntimeError(f"cannot open video {path}")
    n = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    cap.release()
    if n <= 0:
        raise RuntimeError(f"no frames in {path}")
    workers = max(1, int(workers))
    chunk = max(1, (n + workers * 4 - 1) // (workers * 4))
    jobs = [(str(path), s, min(n, s + chunk), scale) for s in range(0, n, chunk)]
    if workers == 1:
        parts = [_decode_range(j) for j in jobs]
    else:
        import multiprocessing

        with multiprocessing.Pool(workers, initializer=cv2.setNumThreads, initargs=(1,)) as pool:
            parts = pool.map(_decode_range, jobs)  # one OpenCV thread per worker: no oversubscription
    raw = sorted(r for part in parts for r in part)
    if not raw:
        raise RuntimeError(f"no frame decoded from {path}")
    return resolve_ticks(raw)


def _open_text(path, mode="rt"):
    return gzip.open(path, mode) if str(path).endswith(".gz") else open(path, mode.replace("t", ""))


def write_ticks(path, rows, header=None):
    tmp = f"{path}.tmp"
    with _open_text(tmp, "wt") as f:
        if header:
            f.write(f"# {header}\n")
        for r in rows:
            half = r[3] if len(r) > 3 else ""
            f.write(f"{r[0]}\t{r[1]:.3f}\t{'' if r[2] is None else r[2]}\t{half}\n")
    os.replace(tmp, path)


def load_ticks(path, pts_offset=0.0, idx_offset=0):
    """A tick map TSV (idx, pts, tick[, half]; '#' comments; .gz ok) -> [(idx, pts, tick|None)]."""
    rows = []
    with _open_text(path) as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            cols = line.rstrip("\n").split("\t")
            tick = cols[2] if len(cols) > 2 else ""
            rows.append((int(cols[0]) + idx_offset, float(cols[1]) + pts_offset,
                         int(tick) if tick not in ("", "None") else None))
    return rows


# ---------------------------------------------------------------- multi-part join + time mapping

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


def join_parts(parts):
    """parts = [(tick-map path, record start | None), ...] -> (joined rows, t0)."""
    rows, t0, _ = join_part_rows([load_ticks(p) for p, _ in parts], [s for _, s in parts])
    return rows, t0


class TickClock:
    """Content time of a painter tick: when the recording first showed it (t0 + pts)."""

    def __init__(self, rec_rows, t0):
        first = {}
        for _, p, t in rec_rows:
            if t is not None and t not in first:
                first[t] = t0 + p
        self.ticks = sorted(first)
        self.times = [first[t] for t in self.ticks]

    def time_of(self, tick):
        k = bisect.bisect_left(self.ticks, tick)
        near = [j for j in (k - 1, k) if 0 <= j < len(self.ticks) and abs(self.ticks[j] - tick) <= MAP_TICK_RADIUS]
        if not near:
            return None
        j = min(near, key=lambda j: abs(self.ticks[j] - tick))
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


# ---------------------------------------------------------------- continuity, dup/skip, publishes

def continuity(rows, step=2):
    """frames / decodable / cadence-proven frames / tick-step events of a contiguous row span.

    A pair of consecutive decoded frames proves the frames between them when the tick advanced by
    step x frame gap; a mismatch is an event; a gap over MAX_JUDGED_GAP frames (or a part seam) is
    unjudged and starts a new span. Each span's first decoded frame counts once."""
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
    return {"frames": len(rows), "decodable": len(dec), "cadence_proven": min(proven, len(rows)),
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


def dupskip(rec_rows, vod_rows, t0, a, b):
    """Downstream dup/skip of the VOD against the recording over content window [a, b)."""
    span = clamp_window(rec_rows, vod_rows, t0, a, b)
    if span is None or span[1] <= span[0]:
        return {"error": "the VOD has no frame of this window", "dup": None, "skip": None}
    a2, b2, cl_start, cl_end = span
    rw = [r for r in rec_rows if r[2] is not None and a2 <= t0 + r[1] < b2]
    if not rw:
        return {"error": "no decoded recording frame in the window", "dup": None, "skip": None}
    lo, hi = min(r[2] for r in rw), max(r[2] for r in rw)
    rec_sorted = sorted({r[2] for r in rw})
    rec_pos = collections.defaultdict(list)
    for k, r in enumerate(rec_rows):
        if r[2] is not None and lo <= r[2] <= hi:
            rec_pos[r[2]].append(k)

    def judged_once(k, tick):
        """The recording shows `tick` once, between two decoded, adjacent, different frames."""
        i = rec_rows[k][0]
        prev_ok = k > 0 and rec_rows[k - 1][2] not in (None, tick) and rec_rows[k - 1][0] == i - 1
        next_ok = k + 1 < len(rec_rows) and rec_rows[k + 1][2] not in (None, tick) and rec_rows[k + 1][0] == i + 1
        return prev_ok and next_ok

    vdec = [v for v in vod_rows if v[2] is not None]
    dups, skips, unjudged = [], [], 0
    for (i0, _, a0), (i1, p1, a1) in zip(vdec, vdec[1:]):
        if i1 - i0 != 1:
            continue
        if a1 == a0 and lo <= a0 <= hi:
            ks = rec_pos.get(a0, [])
            if len(ks) >= 2:
                continue  # the rig repeated this tick itself: cancels out
            if len(ks) == 1 and judged_once(ks[0], a0):
                dups.append({"vod_pts": round(p1, 3), "tick": a0})
            else:
                unjudged += 1
        elif a1 > a0 and a1 > lo and a0 < hi:
            n = bisect.bisect_left(rec_sorted, a1) - bisect.bisect_right(rec_sorted, a0)
            if n > 0:
                skips.append({"vod_pts": round(p1, 3), "from_tick": a0, "to_tick": a1, "frames": n})
    return {"dup": len(dups), "skip": sum(s["frames"] for s in skips), "unjudged": unjudged,
            "rec_frames": len(rw), "vod_frames": sum(1 for v in vdec if lo <= v[2] <= hi),
            "start_utc": a2, "end_utc": b2, "clamped_start_utc": cl_start, "clamped_end_utc": cl_end,
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
    the gap. A publish with no VOD frame before it opened the VOD: reported, judged False."""
    times = sorted(c for c, _ in vod_content_times(rec_rows, vod_rows, t0))
    out = []
    for p in publishes_utc:
        k = bisect.bisect_left(times, p)
        after = times[k] if k < len(times) else None
        before = times[k - 1] if k > 0 else None
        out.append({"utc": p, "last_vod_frame_before_utc": before, "first_vod_frame_utc": after,
                    "gap_s": None if after is None else round(after - p, 3), "judged": before is not None})
    return out


# ---------------------------------------------------------------- audio continuity

def load_audio(path, start_s=None, dur_s=None):
    """First audio track of a file as 16 kHz mono float32 (ffmpeg)."""
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error"]
    if start_s is not None:
        cmd += ["-ss", f"{start_s:.3f}"]
    cmd += ["-i", str(path)]
    if dur_s is not None:
        cmd += ["-t", f"{dur_s:.3f}"]
    cmd += ["-map", "0:a:0", "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"]
    raw = subprocess.run(cmd, capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def drop_samples(x, at_s, ms):
    """Test helper: `ms` of audio lost at `at_s` (a dropped AAC frame)."""
    i, n = int(at_s * SR), int(round(ms / 1000.0 * SR))
    return np.concatenate([x[:i], x[i + n:]])


def dbfs(x):
    if not len(x):
        return -150.0
    r = float(np.sqrt(np.mean(np.square(x.astype(np.float64)))))
    return 20 * np.log10(r) if r > 0 else -150.0


def ncc_best(ref, seg):
    """(offset into seg, normalised correlation) of the best match of ref inside seg (FFT)."""
    ref = ref.astype(np.float64) - float(np.mean(ref))
    seg = seg.astype(np.float64) - float(np.mean(seg))
    m, n = len(ref), len(seg)
    nr = float(np.sqrt(np.dot(ref, ref)))
    if nr == 0 or n < m:
        return 0, 0.0
    size = 1 << (n + m - 1).bit_length()
    c = np.fft.irfft(np.fft.rfft(seg, size) * np.conj(np.fft.rfft(ref, size)), size)[: n - m + 1]
    e = np.concatenate([[0.0], np.cumsum(seg * seg)])
    den = nr * np.sqrt(np.maximum(e[m:] - e[:-m], 1e-20))
    r = c / den
    k = int(np.argmax(r))
    return k, float(r[k])


def audio_blocks(rec, vod, lag_s=0.0, start_s=0.0, end_s=None):
    """Block-by-block continuity of `vod` against `rec` (16 kHz mono arrays).

    lag_s = the expected vod-array time minus rec-array time of the same content (from the video tick
    map). A 4 s reference locks the real lag within +/-2.5 s, then every 0.25 s block is found again
    within +/-40 ms of the tracked lag (+/-200 ms after an unreliable block). Only blocks where the
    recording has signal and the match is good update the lag, so one glitch is one low-corr block,
    never a cascade of lag jumps. The lock starts where its whole +/-2.5 s search lies inside the
    VOD audio (at most 2.5 s into a window that starts at the VOD's first sample)."""
    lag = int(round(lag_s * SR))
    i = max(int(round(start_s * SR)), LOCK_SEARCH - lag, 0)
    end = (len(rec) if end_s is None else min(len(rec), int(round(end_s * SR)))) - BLOCK
    ref4 = rec[i: i + LOCK_REF]
    lo4 = max(0, i + lag - LOCK_SEARCH)
    seg4 = vod[lo4: i + lag + LOCK_REF + LOCK_SEARCH]
    if len(ref4) < LOCK_REF or len(seg4) < LOCK_REF:
        return {"error": "audio window too short for the 4 s lock"}
    k4, r4 = ncc_best(ref4, seg4)
    lag = lock_lag = lo4 + k4 - i
    start = i / SR
    det = {"lag_jumps": [], "low_corr": [], "silent": []}
    lags, gains, corr, recdb = [], [], [], []
    search, reliable_prev = FIRST_SEARCH, None
    while i < end:
        ref = rec[i: i + BLOCK]
        lo, hi = i + lag - search, i + lag + search + BLOCK
        if lo < 0 or hi > len(vod):
            break
        k, r = ncc_best(ref, vod[lo:hi])
        new_lag = lo + k - i
        rec_db = dbfs(ref)
        reliable = rec_db > REC_SIGNAL_DBFS and r >= LOW_CORR
        at = i + (new_lag if reliable else lag)  # an unreliable match is no position: keep the tracked one
        vod_db = dbfs(vod[at: at + BLOCK])
        t = round(i / SR, 3)
        if rec_db > REC_SIGNAL_DBFS and r < LOW_CORR:
            det["low_corr"].append((t, round(r, 3), round(rec_db, 1)))
        if rec_db > SILENT_REC_DBFS and vod_db < SILENT_VOD_DBFS:
            det["silent"].append((t, round(rec_db, 1), round(vod_db, 1)))
        if reliable:
            if reliable_prev is not None and abs(new_lag - reliable_prev) > LAG_JUMP:
                det["lag_jumps"].append((t, round((new_lag - reliable_prev) / SR * 1000, 2)))
            reliable_prev, lag, search = new_lag, new_lag, TRACK_SEARCH
            lags.append(new_lag)
            gains.append((t, vod_db - rec_db))
        else:
            search = FIRST_SEARCH
        corr.append(r)
        recdb.append(rec_db)
        i += BLOCK
    if not corr:
        return {"error": "no audio block in the window"}
    med = float(np.median([g for _, g in gains])) if gains else 0.0
    drops = [(t, round(g - med, 1)) for t, g in gains if g - med < -LEVEL_DROP_DB]
    c = np.array(corr)
    return {"blocks": len(corr), "analysed_from_s": round(start, 3),
            "lag_jumps": len(det["lag_jumps"]), "low_corr": len(det["low_corr"]),
            "silent": len(det["silent"]), "level_drops": len(drops),
            "corr_median": round(float(np.median(c)), 3), "corr_p01": round(float(np.percentile(c, 1)), 3),
            "initial_lock_corr": round(r4, 3), "lag_vs_video_ms": round((lock_lag / SR - lag_s) * 1000, 1),
            "lag_range_ms": round((max(lags) - min(lags)) / SR * 1000, 2) if lags else None,
            "gain_db_median": round(med, 1), "rec_level_dbfs_median": round(float(np.median(recdb)), 1),
            "details": {"lag_jumps": det["lag_jumps"][:DETAIL_LIMIT], "low_corr": det["low_corr"][:DETAIL_LIMIT],
                        "silent": det["silent"][:DETAIL_LIMIT], "level_drops": drops[:DETAIL_LIMIT]}}


def audio_window(rec_audio, vod_audio, rec_rows, vod_rows, t0, a, b, rec_pts0=0.0, vod_pts0=0.0):
    """Audio continuity over content window [a, b), clamped to the part the VOD covers.

    rec_pts0 / vod_pts0 = the pts (recording / VOD timeline) of sample 0 of each audio array."""
    span = clamp_window(rec_rows, vod_rows, t0, a, b)
    if span is None or span[1] <= span[0]:
        return {"error": "the VOD has no frame of this window"}
    rec_p, vod_p = vod_pts_for(rec_rows, vod_rows, t0, span[0])
    if rec_p is None or vod_p is None:
        return {"error": "window start tick not found in the VOD"}
    res = audio_blocks(rec_audio, vod_audio, lag_s=(vod_p - vod_pts0) - (rec_p - rec_pts0),
                       start_s=rec_p - rec_pts0, end_s=(span[1] - t0) - rec_pts0)
    res["start_utc"] = t0 + rec_p
    return res


# ---------------------------------------------------------------- A/V (recording-verdict --av-sync)

def parse_avsync_output(text):
    """The JSON block of `recording-verdict --av-sync` output (log lines, then a line that is exactly
    `{`, the JSON, then more log text)."""
    lines = text.splitlines(keepends=True)
    for n, line in enumerate(lines):
        if line.rstrip("\r\n") == "{":
            j, _ = json.JSONDecoder().raw_decode("".join(lines[n:]))
            return j
    raise ValueError("no JSON block in the recording-verdict --av-sync output")


def _cut_clip(src, start_s, dur_s, out):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-ss", f"{start_s:.3f}",
                    "-i", str(src), "-t", f"{dur_s:.3f}", "-map", "0:v:0", "-map", "0:a:0",
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-c:a", "aac", "-b:a", "192k",
                    str(out)], check=True, capture_output=True)


def _avsync(probe_bin, clip, markers_csv):
    r = subprocess.run([str(probe_bin), "--stream", str(clip), "--av-sync", str(clip),
                        "--av-marker-log", str(markers_csv)], capture_output=True, text=True)
    text = r.stderr + r.stdout
    with open(f"{clip}.avsync.out", "w") as f:
        f.write(text)
    if r.returncode != 0:
        raise RuntimeError(f"recording-verdict --av-sync exited {r.returncode} on {clip}")
    return parse_avsync_output(text)


def av_from_outputs(rec_j, vod_j, dur_s=AV_CLIP_S):
    """The window's A/V block from the two parsed --av-sync results (sign: video - audio)."""
    floor = AV_MIN_MARKER_FRACTION * dur_s * 2
    for name, j in (("recording", rec_j), ("VOD", vod_j)):
        if j.get("av_offset_ms") is None:
            raise ValueError(f"{name}: no A/V offset measured")
        if (j.get("matched") or 0) < floor:
            raise ValueError(f"{name}: only {j.get('matched')} markers matched (< {floor:.0f})")
    return {"rec_ms": round(rec_j["av_offset_ms"], 1), "vod_ms": round(vod_j["av_offset_ms"], 1),
            "delta_ms": round(vod_j["av_offset_ms"] - rec_j["av_offset_ms"], 1),
            "markers_rec": rec_j.get("matched"), "markers_vod": vod_j.get("matched"),
            "mad_rec_ms": round(rec_j.get("mad_ms") or 0.0, 1), "mad_vod_ms": round(vod_j.get("mad_ms") or 0.0, 1)}


def av_window(rec_file, vod_file, rec_start_s, vod_start_s, markers_csv, probe_bin, dur_s=AV_CLIP_S, workdir=None):
    """Cut the same content from both files (tick-matched starts) and measure each clip's A/V. The
    clips are deleted afterwards; each clip's probe output is kept next to it as *.avsync.out."""
    workdir = workdir or tempfile.mkdtemp(prefix="ylv-av-")
    results = []
    for kind, src, start in (("rec", rec_file, rec_start_s), ("vod", vod_file, vod_start_s)):
        clip = os.path.join(workdir, f"av-{kind}-{start:.3f}.mp4")
        try:
            _cut_clip(src, start, dur_s, clip)
            results.append(_avsync(probe_bin, clip, markers_csv))
        finally:
            if os.path.exists(clip):
                os.remove(clip)
    return av_from_outputs(results[0], results[1], dur_s)


# ---------------------------------------------------------------- verdict

def verdict(windows, publishes):
    """{overall: PASS|FAIL|UNKNOWN, reasons, criteria}. A proven FAIL anywhere wins over an UNKNOWN."""
    fails, unknowns = [], []
    status = {k: "PASS" for k in ("av", "dupskip", "publish", "audio", "coverage")}

    def bad(crit, msg, kind):
        (fails if kind == "FAIL" else unknowns).append(msg)
        if status[crit] != "FAIL":
            status[crit] = kind

    if not windows:
        bad("coverage", "no window measured", "UNKNOWN")
    ref = next((w["av"]["delta_ms"] for w in windows if (w.get("av") or {}).get("delta_ms") is not None), None)
    for n, w in enumerate(windows):
        name = w.get("name", f"window {n + 1}")
        for e in w.get("errors") or ():
            bad("coverage", f"{name}: {e}", "UNKNOWN")
        ds = w.get("dupskip") or {}
        if ds.get("dup") is None or ds.get("skip") is None:
            if not w.get("errors"):
                bad("dupskip", f"{name}: dup/skip not measured ({ds.get('error', 'missing')})", "UNKNOWN")
            continue  # nothing of this window was measured
        if ds["dup"] or ds["skip"]:
            bad("dupskip", f"{name}: {ds['dup']} downstream dup / {ds['skip']} skip", "FAIL")
        cov = w.get("coverage") or {}
        for side in ("rec", "vod"):
            pct = cov.get(f"{side}_cadence_pct")
            if pct is None or pct < CADENCE_MIN_PCT:
                bad("coverage", f"{name}: {side} cadence-proven {pct} % < {CADENCE_MIN_PCT:.0f} %", "UNKNOWN")
        covered = w.get("covered_fraction")
        if covered is not None and covered < MIN_WINDOW_COVERED:
            bad("coverage", f"{name}: the VOD covers only {covered:.0%} of the window", "UNKNOWN")
        av = w.get("av") or {}
        if av.get("delta_ms") is None:
            bad("av", f"{name}: A/V not measured ({av.get('error', 'missing')})", "UNKNOWN")
        elif abs(av["delta_ms"] - ref) > AV_TOLERANCE_MS:
            bad("av", f"{name}: A/V VOD - recording {av['delta_ms']:+.1f} ms is "
                      f"{av['delta_ms'] - ref:+.1f} ms off the first window (> {AV_TOLERANCE_MS:.0f})", "FAIL")
        au = w.get("audio") or {}
        if any(au.get(k) is None for k in AUDIO_KEYS):
            bad("audio", f"{name}: audio not measured ({au.get('error', 'missing')})", "UNKNOWN")
        elif any(au[k] for k in AUDIO_KEYS):
            bad("audio", f"{name}: audio " + ", ".join(f"{k} {au[k]}" for k in AUDIO_KEYS if au[k]), "FAIL")
    for p in publishes:
        if not p.get("judged", True):
            continue
        when = fmt_utc(p["utc"]) if isinstance(p["utc"], (int, float)) else p["utc"]
        if p.get("gap_s") is None:
            bad("publish", f"publish {when}: no VOD frame after it", "FAIL")
        elif p["gap_s"] > PUBLISH_GAP_MAX_S:
            bad("publish", f"publish {when}: first VOD frame {p['gap_s']:.2f} s late "
                           f"(> {PUBLISH_GAP_MAX_S} s)", "FAIL")
    overall = "FAIL" if fails else ("UNKNOWN" if unknowns else "PASS")
    return {"overall": overall, "reasons": fails + unknowns, "criteria": status}


# ---------------------------------------------------------------- CLI

def parse_utc(s):
    """Epoch seconds, ISO 8601 or compact ISO (YYYYMMDDTHHMMSS[.fff]Z) -> epoch seconds."""
    s = s.strip()
    if re.fullmatch(r"\d+(\.\d*)?", s):
        return float(s)
    m = re.fullmatch(r"(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})(\.\d+)?Z", s)
    if m:
        g = m.groups()
        s = f"{g[0]}-{g[1]}-{g[2]}T{g[3]}:{g[4]}:{g[5]}{g[6] or ''}Z"
    d = datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))
    if d.tzinfo is None:
        raise ValueError(f"timestamp without a time zone: {s}")
    return d.timestamp()


def _is_utc(s):
    try:
        parse_utc(s)
    except ValueError:
        return False
    return True


def parse_window(arg):
    """name:start:end (start/end in any parse_utc form; ISO colons are fine)."""
    name, _, rest = arg.partition(":")
    found = [(parse_utc(rest[:m.start()]), parse_utc(rest[m.end():])) for m in re.finditer(":", rest)
             if _is_utc(rest[:m.start()]) and _is_utc(rest[m.end():])]
    if not name or len(found) != 1 or found[0][1] <= found[0][0]:
        raise ValueError(f"bad --windows value {arg!r} (want name:start:end)")
    return name, found[0][0], found[0][1]


def parse_recording(arg):
    """file[@record-start-utc] -> (file, start | None)."""
    head, sep, tail = arg.rpartition("@")
    if sep and head and _is_utc(tail):
        return head, parse_utc(tail)
    return arg, None


def fmt_utc(x):
    if x is None:
        return None
    return (datetime.datetime.fromtimestamp(x, tz=datetime.timezone.utc)
            .isoformat(timespec="milliseconds").replace("+00:00", "Z"))


def fetch_vod(vod, out):
    if os.path.isfile(vod):
        return vod
    if not re.fullmatch(r"[A-Za-z0-9_-]{11}", vod):
        raise ValueError(f"--vod {vod!r} is neither a file nor a YouTube id")
    path = os.path.join(out, f"vod-{vod}.mp4")
    if not os.path.isfile(path):
        subprocess.run(["yt-dlp", "-q", "--no-progress", "-f",
                        "bv*[height<=1080][fps<=30][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=1080][fps<=30]+ba/b",
                        "--merge-output-format", "mp4", "-o", path, f"https://www.youtube.com/watch?v={vod}"],
                       check=True)
    return path


def fetch_markers(src, out):
    if re.match(r"https?://", src):
        path = os.path.join(out, "markers.csv")
        with urllib.request.urlopen(src, timeout=60) as r, open(path, "wb") as f:
            shutil.copyfileobj(r, f)
        return path
    if not os.path.isfile(src):
        raise ValueError(f"marker log {src} not found")
    return src


def cached_ticks(src, cache, workers):
    """The file's tick map, decoded once per source (size + mtime key) and kept in the out dir."""
    st = os.stat(src)
    key = f"source={os.path.abspath(src)} size={st.st_size} mtime={int(st.st_mtime)}"
    if os.path.isfile(cache):
        with open(cache) as f:
            if f.readline().strip() == f"# {key}":
                return load_ticks(cache)
    rows = decode_ticks(src, workers)
    write_ticks(cache, rows, header=key)
    return [(i, p, t) for i, p, t, _ in rows]


def _guard(fn, *a, **kw):
    try:
        return fn(*a, **kw)
    except Exception as e:  # a measurement error is UNKNOWN for that criterion, never a crash
        print(f"youtube_leg_verdict: {fn.__name__} failed: {type(e).__name__}: {e}", file=sys.stderr)
        return {"error": f"{type(e).__name__}: {e}"}


def measure_window(ctx, name, a, b):
    """One window's dup/skip, coverage, audio and A/V; errors land in w['errors'] / each block."""
    rec_rows, vod_rows, t0, starts = ctx["rec_rows"], ctx["vod_rows"], ctx["t0"], ctx["starts"]
    w = {"name": name, "start": fmt_utc(a), "end": fmt_utc(b), "errors": []}
    ds = _guard(dupskip, rec_rows, vod_rows, t0, a, b)
    w["dupskip"] = ds
    if "error" in ds:
        w["errors"].append(ds["error"])
        return w
    a2, b2 = ds.pop("start_utc"), ds.pop("end_utc")
    w["clamped_start"], w["clamped_end"] = fmt_utc(ds.pop("clamped_start_utc")), fmt_utc(ds.pop("clamped_end_utc"))
    w["covered_fraction"] = round((b2 - a2) / (b - a), 3)
    w["coverage"] = coverage(rec_rows, vod_rows, t0, a2, b2)
    k = max([i for i, s in enumerate(starts) if s <= a2 + 1e-6] or [0])
    if k + 1 < len(starts) and b2 > starts[k + 1]:
        w["errors"].append("the window spans two recording parts")
        return w
    if k not in ctx["rec_audio"]:
        ctx["rec_audio"][k] = load_audio(ctx["recs"][k][0])
    w["audio"] = _guard(audio_window, ctx["rec_audio"][k], ctx["vod_audio"], rec_rows, vod_rows, t0, a2, b2,
                        rec_pts0=starts[k] - t0)
    w["audio"].pop("start_utc", None)
    rec_p, vod_p = vod_pts_for(rec_rows, vod_rows, t0, a2)
    if rec_p is None or vod_p is None:
        w["av"] = {"error": "window start tick not found in the VOD"}
    else:
        w["av"] = _guard(av_window, ctx["recs"][k][0], ctx["vod_file"], rec_p - (starts[k] - t0), vod_p,
                         ctx["markers"], ctx["probe_bin"], min(AV_CLIP_S, b2 - a2), ctx["out"])
    return w


def measure(args):
    os.makedirs(args.out, exist_ok=True)
    windows_in = [parse_window(s) for s in args.windows]
    publishes = [parse_utc(p) for p in args.publish]
    recs = [parse_recording(r) for r in args.recording]
    vod_file = fetch_vod(args.vod, args.out)
    ctx = {"recs": recs, "vod_file": vod_file, "markers": fetch_markers(args.markers, args.out),
           "probe_bin": args.probe_bin, "out": args.out, "rec_audio": {}}
    part_rows = [cached_ticks(p, os.path.join(args.out, f"ticks-rec-{k + 1}.tsv"), args.workers)
                 for k, (p, _) in enumerate(recs)]
    ctx["vod_rows"] = cached_ticks(vod_file, os.path.join(args.out, "ticks-vod.tsv"), args.workers)
    ctx["rec_rows"], ctx["t0"], ctx["starts"] = join_part_rows(part_rows, [s for _, s in recs])
    ctx["vod_audio"] = load_audio(vod_file)
    windows = [measure_window(ctx, name, a, b) for name, a, b in windows_in]
    pubs = publish_gaps(ctx["rec_rows"], ctx["vod_rows"], ctx["t0"], publishes)
    v = verdict(windows, pubs)
    for p in pubs:
        for key in ("utc", "first_vod_frame_utc", "last_vod_frame_before_utc"):
            p[key] = fmt_utc(p[key])
    return {"schema": SCHEMA, "overall": v["overall"],
            "criteria": {"status": v["criteria"], "av_tolerance_ms": AV_TOLERANCE_MS,
                         "publish_gap_max_s": PUBLISH_GAP_MAX_S, "cadence_min_pct": CADENCE_MIN_PCT,
                         "lag_jump_ms": 1000.0 * LAG_JUMP / SR, "low_corr": LOW_CORR,
                         "silent_vod_dbfs": SILENT_VOD_DBFS, "level_drop_db": LEVEL_DROP_DB},
            "recording_starts": [fmt_utc(s) for s in ctx["starts"]], "windows": windows, "publishes": pubs,
            "reasons": v["reasons"]}


def write_result(out, result):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, "youtube-leg-verdict.json")
    with open(f"{path}.tmp", "w") as f:
        json.dump(result, f, indent=1)
    os.replace(f"{path}.tmp", path)
    return path


def main(argv=None):
    ap = argparse.ArgumentParser(description="Issue 1404 YouTube-leg verdict (exit 0 PASS, 1 FAIL, 2 UNKNOWN)")
    ap.add_argument("--decode-ticks", help="diagnostic: print this file's tick map and exit")
    ap.add_argument("--vod")
    ap.add_argument("--recording", action="append", default=[])
    ap.add_argument("--markers")
    ap.add_argument("--windows", action="append", default=[])
    ap.add_argument("--publish", action="append", default=[])
    ap.add_argument("--out")
    ap.add_argument("--probe-bin", default=os.environ.get("RECORDING_VERDICT_BIN", "recording-verdict"))
    ap.add_argument("--workers", type=int, default=4)
    args = ap.parse_args(argv)
    if args.decode_ticks:
        for i, p, t, h in decode_ticks(args.decode_ticks, args.workers):
            print(f"{i}\t{p:.3f}\t{'' if t is None else t}\t{h}")
        return EXIT_PASS
    if not (args.vod and args.markers and args.out and args.recording and args.windows):
        ap.error("--vod, --recording, --markers, --windows and --out are required")
    try:
        result = measure(args)
    except Exception as e:  # fail closed: any tool error is UNKNOWN, written like any verdict
        print(f"youtube_leg_verdict: tool error: {type(e).__name__}: {e}", file=sys.stderr)
        result = {"schema": SCHEMA, "overall": "UNKNOWN", "criteria": {}, "windows": [], "publishes": [],
                  "reasons": [f"tool error: {type(e).__name__}: {e}"]}
    path = write_result(args.out, result)
    print(f"youtube-leg verdict: {result['overall']} -> {path}")
    for r in result["reasons"]:
        print(f"  - {r}")
    return {"PASS": EXIT_PASS, "FAIL": EXIT_FAIL}.get(result["overall"], EXIT_UNKNOWN)


if __name__ == "__main__":
    sys.exit(main())
