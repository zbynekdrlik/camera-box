#!/usr/bin/env python3
"""Issue 1404 -- the cam2 painter tick of every frame of a video, for the YouTube-leg verdict.

The painter's dual-QR Vernier (src/probe/painter.rs `vernier_ids`) paints the latest EVEN tick on the
LEFT half and the latest ODD tick on the RIGHT half, each as `P{run}.{tick}.{gen_ts_ns}.{crc32}`
(src/probe/payload.rs). A frame captured on painter tick T therefore reads right = left + 1 when T is
odd and right = left - 1 when T is even (its capture PHASE); the half that changed at T is the FRESH
half (left when T is even), the other is SETTLED. A frame's tick is the even value (left's).

Measured on the real session-3 files (issue 1404):
  - the camera captures the fresh half mid-transition as a green/blue pattern the gray detector
    cannot read; its blue channel reads it (part 1: left 0/120 gray, 99/120 blue);
  - the capture phase moves during a session (odd early, even later), so a fixed "right - 1" for a
    right-only frame reads whole stretches 2 ticks low; a right-only frame takes the phase of the
    nearest frames where both halves read;
  - the fresh half can read STALE (VOD frames 20174 / 20210: left = previous frame's tick, right =
    left + 1, between even-phase neighbours). Such a frame reads exactly like a camera frame captured
    one tick late, so it is left undecoded ('x'), never guessed: a guess would be a false dup + skip.

Why not the repo's Rust Vernier decoder (src/probe/recording_decode.rs): the probe feature compiles in
CI only (Tier-0), every iteration would cost a CI cycle, and the tool is shared with restreamer's
gate; the Rust binary is still used for the A/V measurement (design comment 6016521807).
"""
import bisect
import gzip
import os
import re
import sys
import zlib

import numpy as np

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_proc import run_bounded  # noqa: E402

PAINTER_QR = re.compile(r"^P(\d+)\.(\d+)\.(-?\d+)\.(\d+)$")
NODE_BURN_RUN = re.compile(r"^9110\d\d$")  # reserved node/origin burn ids 911001..911099, never the painter
# The camera-box measurement clip (scripts/gen_measurement_clip.py) paints the painter's dual-QR
# Vernier under this reserved id: the CG segments' tick. It is read as a tick ONLY when the caller asks
# for it (`runs=CLIP_RUNS`): the clip restarts its tick on every play, so a run-scoped decode keeps each
# frame's run (decode_ticks' 7th column) and the timeline judges every run segment on its own tick
# line (youtube_leg_timeline RunTimeline, issue 1404 Task 5 part b). A decode that passes `runs` keys
# its tick cache on them (youtube_leg_verdict.tick_cache_key). The default decode is unchanged. The Rust
# recording decode never reads it as the cam2 Vernier tick (NODE_BURN_RUN_IDS); `recording-verdict
# --av-sync --av-run 911016` pairs the clip's own tick with its own marker.
MEASUREMENT_CLIP_RUN_ID = 911016
CLIP_RUNS = (MEASUREMENT_CLIP_RUN_ID,)
QR_TOP_FRACTION = 0.62  # the painter's two big QRs sit in the top 62 % of the frame
DECODE_SCALE = 0.5
PHASE_RADIUS = 60  # frames: the local capture phase comes from both-halves frames this near
# Part of the tick-cache key. Still 2 with the known-position read (issue 1404, design 6387919956):
# it reads every half the full detector reads, the same, and only adds halves the finder missed (a
# superset, measured on the real files), so a v2 map of either decoder is a valid map of the file.
DECODER_VERSION = 2
PROBE_COUNT_TIMEOUT_S = 600
# The worker pool's bound: about 4x the slowest measured decode (~0.27 s a frame per worker for the
# 1080p session files on a loaded dev1), never under 10 min. A worker the OOM killer took leaves
# multiprocessing's map waiting forever; the bound turns that into an error (UNKNOWN), not a hang.
DECODE_S_PER_FRAME = 1.0
DECODE_TIMEOUT_FLOOR_S = 600
# Read each frame at the painter QRs' known corner points first, the full detector only as the
# fallback (KnownPositionDetector, design 6387919956 part a). Off = the full detector on every
# plane, the reference the tests compare against (an in-process decode, workers=1).
KNOWN_POSITION_READ = True
_RANGE_READS = {}  # per process: the KnownPositionDetector counters of the last _decode_range
LAST_READS = {}  # abspath -> the read counters of the last decode_raw of that file (cached_ticks)


def painter_payload(text, runs=()):
    """(run, tick) of a valid painter payload (CRC-checked like Payload::decode), else None. A reserved
    9110xx id is refused unless the caller lists it in `runs` (the measurement clip: CLIP_RUNS)."""
    m = PAINTER_QR.match(text or "")
    if not m:
        return None
    run, tick, gen, crc = (int(g) for g in m.groups())
    if zlib.crc32(f"{run}.{tick}.{gen}".encode()) != crc:
        return None
    if NODE_BURN_RUN.match(str(run)) and run not in runs:
        return None
    return run, tick


def painter_tick(texts, runs=()):
    """The painter tick among decoded QR texts (the lowest valid painter payload), or None."""
    ticks = [p[1] for p in (painter_payload(t, runs) for t in texts or ()) if p is not None]
    return min(ticks) if ticks else None


def painter_tick_run(texts, runs=()):
    """(tick, run) of the lowest valid painter payload among decoded QR texts, or None: a run-scoped
    decode keeps the run each tick was read under (painter_payload)."""
    found = [(p[1], p[0]) for p in (painter_payload(t, runs) for t in texts or ()) if p is not None]
    return min(found) if found else None


class KnownPositionDetector:
    """A cv2.QRCodeDetector that remembers where the painter's QRs are (issue 1404, design
    6387919956 part a).

    The painter's two QRs sit at fixed positions in a recording, and a full `detectAndDecodeMulti`
    (finder-pattern search over the whole plane) is most of the decode's time. So per half and per
    plane (gray / blue) this keeps the corner points of the painter QR the full detector last read,
    and `_half_read` reads the next frame's planes at those points first (`QRCodeDetector.decode`;
    gray, then blue), and runs the full detector (gray, then blue) only when neither gives a valid
    painter payload (a cut to another framing, a frame without the QR): the FALLBACK, which stores
    the new points. Counted: `known` (planes read at the stored points), `misses` (point reads that
    gave no painter payload), `fallback` (halves whose stored points all missed, so the full detector
    ran), `full` (every full detection, fallbacks included). A caller that passes a plain cv2 detector
    keeps the full read on every plane.

    What it changes, measured on real files (issue 1404, 24.10.2026): the decode at the stored points
    also reads a QR the finder search missed, so a few more halves read (the yu1zFSHFdu8 VOD: 309 of
    26940 halves, 5 more frames with a tick; the run's 4K stream recording: 380 of 18632 halves, 6
    frames; each added tick the +2 step of its neighbours); no tick the full detector read changes,
    and no 'x' frame appears or goes. A committed real crop reads a half more, never a half
    differently (tests/python/test_youtube_leg_known_position_1404.py)."""

    def __init__(self):
        import cv2

        self.det = cv2.QRCodeDetector()
        self.points = {}
        self.known = self.misses = self.fallback = self.full = 0

    def stats(self):
        return {"known": self.known, "misses": self.misses, "fallback": self.fallback, "full": self.full}

    def read_known(self, plane, key, pick):
        """The pick of the QR read at the stored points of `key`, or None (no points, or no valid
        painter payload there: counted as a miss)."""
        import cv2

        pts = self.points.get(key)
        if pts is None:
            return None
        try:
            text, _ = self.det.decode(plane, pts)
        except cv2.error:  # points that no longer fit this frame: the full detector decides
            text = ""
        found = pick([text]) if text else None
        if found is None:
            self.misses += 1
            return None
        self.known += 1
        return found

    def remember(self, key, texts, points, pick, found):
        """Store the corner points of the QR whose text gave `found`."""
        for text, pts in zip(texts, points):
            if text and pick([text]) == found:
                self.points[key] = np.ascontiguousarray(pts, dtype=np.float32).reshape(1, 4, 2)
                return


def _qr_detect(det, plane):
    """(texts, points) of the QRs the full detector reads in one plane, or (None, None)."""
    import cv2

    try:
        ok, texts, points, _ = det.detectAndDecodeMulti(plane)
    except cv2.error as e:  # one unreadable half is an undecodable half, never a crash
        print(f"youtube_leg_ticks: QR detector error on a frame half: {e}", file=sys.stderr)
        return None, None
    return (texts, points) if ok else (None, None)


def _scaled(plane, scale):
    import cv2

    return cv2.resize(plane, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA) if scale != 1.0 else plane


def _plane_read(det, plane, scale, pick):
    """pick(texts) of one plane read by the full detector (a plain cv2 detector)."""
    texts = _qr_detect(det, _scaled(plane, scale))[0]
    return pick(texts) if texts is not None else None


def _qr_tick(det, plane, scale, runs=()):
    return _plane_read(det, plane, scale, lambda texts: painter_tick(texts, runs))


def _half_planes(img):
    """A QR half's planes in reading order: gray first, then the blue channel (the mid-transition
    colour), each made only when the one before it read nothing."""
    import cv2

    yield cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    yield np.ascontiguousarray(img[:, :, 0])


def _half_read(det, img, scale, pick, side):
    """pick(texts) of one QR half, gray first, then the blue channel (the mid-transition colour).

    A KnownPositionDetector (`side` "L" / "R" keys its points) reads the planes at their stored
    points first (gray, then blue), and only when neither gives a painter payload runs the full
    detector (gray, then blue: the fallback, which stores the new points). Any other detector runs
    the full detector on each plane, as before."""
    if not isinstance(det, KnownPositionDetector) or side is None:
        for plane in _half_planes(img):
            found = _plane_read(det, plane, scale, pick)
            if found is not None:
                return found
        return None
    planes = []

    def plane(k):  # the scaled planes, made on first use
        while len(planes) <= k:
            planes.append(_scaled(next(gen), scale))
        return planes[k]

    gen = _half_planes(img)
    tried = False
    for k in (0, 1):
        if (side, k) in det.points:
            tried = True
            found = det.read_known(plane(k), (side, k), pick)
            if found is not None:
                return found
    det.fallback += tried  # the stored points read nothing: the full detector decides this half
    for k in (0, 1):
        det.full += 1
        texts, points = _qr_detect(det.det, plane(k))
        found = pick(texts) if texts is not None else None
        if found is not None:
            det.remember((side, k), texts, points, pick, found)
            return found
    return None


def _half_tick(det, img, scale, runs=(), side=None):
    """Painter tick of one QR half (_half_read)."""
    return _half_read(det, img, scale, lambda texts: painter_tick(texts, runs), side)


def _half_tick_run(det, img, scale, runs, side=None):
    """(tick, run) of one QR half for a run-scoped decode (_half_read), or None."""
    return _half_read(det, img, scale, lambda texts: painter_tick_run(texts, runs), side)


def band_ticks(band, det, scale=DECODE_SCALE, runs=()):
    """(left tick | None, right tick | None) of the QR band (the top 62 % of a frame)."""
    w = band.shape[1]
    return (_half_tick(det, band[:, : w // 2], scale, runs, "L"),
            _half_tick(det, band[:, w // 2:], scale, runs, "R"))


def half_ticks(frame, det, scale=DECODE_SCALE, runs=()):
    """(left tick | None, right tick | None) of one BGR frame."""
    return band_ticks(frame[0:int(frame.shape[0] * QR_TOP_FRACTION)], det, scale, runs)


def half_ticks_run(frame, det, scale=DECODE_SCALE, runs=()):
    """(left tick | None, right tick | None, run | None) of one BGR frame for a run-scoped decode: the
    run both halves were read under. Halves of two different runs (a frame blending two sources at a
    cut) read as nothing: one frame shows one run's tick line or no tick."""
    band = frame[0:int(frame.shape[0] * QR_TOP_FRACTION)]
    w = band.shape[1]
    left = _half_tick_run(det, band[:, : w // 2], scale, runs, "L")
    right = _half_tick_run(det, band[:, w // 2:], scale, runs, "R")
    if left is not None and right is not None and left[1] != right[1]:
        return None, None, None
    run = (left or right or (None, None))[1]
    return (left[0] if left else None), (right[0] if right else None), run


def _pair_phase(left, right):
    """1 = odd capture tick (right = left + 1), 0 = even (right = left - 1), None = no consistent pair."""
    if left is None or right is None or abs(right - left) != 1:
        return None
    return 1 if right > left else 0


def resolve_ticks(raw, radius=PHASE_RADIUS):
    """Per-frame (index, pts, even tick | None, half) from raw (index, pts, left, right[, run]) halves.

    half: 'B' both halves agree with the local phase, tick = left; 'L' left only, tick = left;
          'R' right only, tick = right - 1 (local phase odd) or right + 1 (even);
          'x' both halves read but contradict the local phase on both sides (a stale fresh half or a
              one-frame capture slip, which read the same): no tick;
          'r' right only and no local phase (none near, or a phase step between the sides): no tick;
          ''  nothing read.
    The local phase of a frame comes from the nearest both-halves frames before and after it (within
    `radius` frames, the frame itself excluded): one phase if they agree or only one exists. A
    run-scoped raw row (a `runs` decode) takes its phase from frames of its own run only: the clip and
    the painter tick on independent phases."""
    own = {r[0]: _pair_phase(r[2], r[3]) for r in raw}
    run_of = {r[0]: (r[4] if len(r) > 4 else None) for r in raw}
    by_run = {}
    for i in sorted(i for i, ph in own.items() if ph is not None):
        by_run.setdefault(run_of[i], []).append(i)

    def local_phase(i):
        known = by_run.get(run_of[i], [])
        k = bisect.bisect_left(known, i)
        before = next((known[j] for j in range(k - 1, -1, -1) if known[j] != i), None)
        after = next((known[j] for j in range(k, len(known)) if known[j] != i), None)
        near = {own[j] for j in (before, after) if j is not None and abs(j - i) <= radius}
        if len(near) != 1:
            return None
        both_sides = before is not None and after is not None and abs(before - i) <= radius and abs(after - i) <= radius
        return near.pop(), both_sides

    rows = []
    for i, p, left, right, *_ in raw:
        loc = local_phase(i)
        if own[i] is not None:
            if loc is not None and loc[1] and loc[0] != own[i]:
                rows.append((i, p, None, "x"))
            else:
                rows.append((i, p, left, "B"))
        elif left is not None:
            rows.append((i, p, left, "L"))
        elif right is not None and loc is not None:
            rows.append((i, p, right - 1 if loc[0] else right + 1, "R"))
        else:
            rows.append((i, p, None, "r" if right is not None else ""))
    return rows


def _decode_range(job):
    """Raw (index, pts, left, right) of frames [start, end) (end None = to the end of the file); a
    run-scoped job (field 5 = the reserved ids also read as a tick) adds the run: (..., run).
    Returns (rows, seek_ok, hit_end): a seek that does not land on `start` is reported, never
    decoded; hit_end = the file ran out before `end` (always, for end None)."""
    import cv2

    path, start, end, scale = job[:4]
    runs = tuple(job[4]) if len(job) > 4 else ()  # the reserved ids read as a tick (decode_raw `runs`)
    cap = cv2.VideoCapture(path)
    cap.set(cv2.CAP_PROP_POS_FRAMES, start)
    if int(cap.get(cv2.CAP_PROP_POS_FRAMES)) != start:
        cap.release()
        return [], False, False
    det = KnownPositionDetector() if KNOWN_POSITION_READ else cv2.QRCodeDetector()
    out, hit_end = [], False
    i = start
    while end is None or i < end:
        ok, frame = cap.read()
        if not ok:
            hit_end = True
            break
        # a default decode keeps the 3-argument call (the decode-mechanics tests swap half_ticks); a
        # run-scoped one reads the run of each frame too (issue 1404 Task 5 part b)
        halves = half_ticks_run(frame, det, scale, runs) if runs else half_ticks(frame, det, scale)
        out.append((i, cap.get(cv2.CAP_PROP_POS_MSEC) / 1000.0) + halves)
        i += 1
    if not hit_end and not cap.grab():  # the file ends exactly here (a frame count estimated too high)
        hit_end = True
    cap.release()
    _RANGE_READS.clear()
    if isinstance(det, KnownPositionDetector):
        _RANGE_READS.update(det.stats())
    return out, True, hit_end


def _decode_job(job):
    """_decode_range + the read counters of its detector: (rows, seek_ok, hit_end, reads)."""
    _RANGE_READS.clear()
    rows, seek_ok, hit_end = _decode_range(job)
    return rows, seek_ok, hit_end, dict(_RANGE_READS)


def container_frames(path):
    """The video stream's packet count as ffprobe reads it (one packet per frame)."""
    r = run_bounded(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                     "stream=nb_read_packets", "-of", "csv=p=0", path], PROBE_COUNT_TIMEOUT_S, text=True, check=True)
    return int(r.stdout.strip().split(",")[0])


class ChunkedDecodeError(RuntimeError):
    """A chunked (seeking) decode that does not tile the file; one sequential pass may still read it."""


def _merge_chunks(parts):
    """The rows of every chunk in pts order, numbered 0..n-1 again.

    OpenCV seeks a frame number by its TIMESTAMP, so behind a timestamp gap that is really in the
    file a chunk starts one frame early: that frame is read by two chunks (the same pts) and kept
    once. A chunk that started late leaves a frame no chunk read: the count check catches it. A
    chunk's own frame numbers are never trusted, they are where the seek was aimed, not where it
    landed."""
    by_pts = {}
    for rows, *_ in parts:
        for r in rows:
            by_pts.setdefault(r[1], r)
    return [(k,) + r[1:] for k, r in enumerate(sorted(by_pts.values(), key=lambda r: r[1]))]


def _check_decode(path, jobs, parts):
    """The decode of `path` from its chunks, or an error.

    The chunks must be in order: every seek landed, a chunk that ran into the end of the file is
    followed only by empty chunks, the pts rise strictly inside each chunk (frame order is read
    order; a merge by misreported pts would reorder frames silently), and the merge holds exactly the
    container's own packet count (ffprobe). Any of these failing in a chunked decode raises
    ChunkedDecodeError (one sequential pass may still read the file); in a single pass it is a plain
    RuntimeError. A gap that is really in the file (an encoder that skipped a frame) is kept: the
    timeline module judges it. A single pass is taken in its own frame order (no pts merge)."""
    err = ChunkedDecodeError if len(parts) > 1 else RuntimeError
    ended = None  # the start of the first chunk that ran into the end of the file
    for job, (rows, seek_ok, hit_end, *_) in zip(jobs, parts):
        if ended is not None:
            if rows:
                raise err(f"{path}: frames decoded after the end of the file at chunk {job[1]}")
            continue
        if not seek_ok:
            raise err(f"{path}: seek to frame {job[1]} failed")
        if len(parts) > 1 and any(r1[1] <= r0[1] for r0, r1 in zip(rows, rows[1:])):
            raise err(f"{path}: pts do not rise inside the chunk at frame {job[1]}")
        if hit_end:
            ended = job[1]
    if ended is None:
        raise err(f"{path}: the decode never reached the end of the file")
    raw = _merge_chunks(parts) if len(parts) > 1 else list(parts[0][0])
    if not raw:
        raise err(f"no frame decoded from {path}")
    n = container_frames(path)
    if len(raw) != n:
        what = "frames missing" if len(raw) < n else "frames read twice"
        raise err(f"{path}: {len(raw)} frames decoded, the container holds {n} ({what})")
    return raw


def _worker_init():
    import cv2

    cv2.setNumThreads(1)  # one OpenCV thread per worker: no oversubscription


def _run_jobs(path, jobs, workers, frames):
    """`_decode_range` over `jobs`: in this process for one worker, else in a SPAWNED pool, bounded.

    Never a forked pool: a decode in this process (workers=1, or the one-pass fallback) starts
    OpenCV's own threads here, and a forked child inherits their locked mutexes and waits forever.
    A pool that does not finish within its bound (a worker the OOM killer took, a decoder that hangs)
    is a RuntimeError, never a hang; leaving the `with` terminates the workers. A spawned worker
    imports the caller's main module, so a calling SCRIPT keeps its work under
    `if __name__ == "__main__":` (the tool's CLI does); without it every worker dies at start-up and
    the pool runs into its bound."""
    if workers == 1:
        return [_decode_job(j) for j in jobs]
    import multiprocessing

    n = min(workers, len(jobs))
    limit = max(DECODE_TIMEOUT_FLOOR_S, DECODE_S_PER_FRAME * frames / n)
    with multiprocessing.get_context("spawn").Pool(n, initializer=_worker_init) as pool:
        try:
            return pool.map_async(_decode_job, jobs).get(limit)
        except multiprocessing.TimeoutError:
            raise RuntimeError(f"{path}: the decode did not finish in {limit:.0f} s") from None


def _report_reads(path, raw, parts, reads):
    """Sum the chunks' read counters, print them, fill `reads` and LAST_READS[path]."""
    total = {"frames": len(raw), "known": 0, "misses": 0, "fallback": 0, "full": 0}
    for part in parts:
        for k, v in (part[3] if len(part) > 3 else {}).items():
            total[k] = total.get(k, 0) + v
    print(f"youtube_leg_ticks: {path}: {total['frames']} frames, {total['known']} known-position reads, "
          f"{total['full']} full detections ({total['fallback']} of them a fallback after the stored points "
          f"read nothing; {total['misses']} point reads that missed)", file=sys.stderr)
    LAST_READS[os.path.abspath(str(path))] = dict(total)
    if reads is not None:
        reads.update(total)
    return raw


def decode_raw(path, workers=4, scale=DECODE_SCALE, runs=(), reads=None):
    """Raw (index, pts, left, right) of EVERY frame of a video file, or RuntimeError. `runs`: the
    reserved ids also read as a tick (CLIP_RUNS for a measurement-clip file; default none). `reads`
    (a dict) receives the read counters: frames, known, fallback, full (KnownPositionDetector).

    The file is cut into chunks by its frame count, the last chunk reads to the end of the file (a
    frame count is an estimate in some containers), and the chunks are merged by pts. When the
    chunks do not tile the file (a seek that missed or landed late, pts that fall inside a chunk),
    the file is decoded once more in ONE pass from frame 0 (no seek), which must hold exactly the
    container's frames too."""
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
    starts = list(range(0, n, chunk))
    extra = (tuple(runs),) if runs else ()  # a default decode keeps the 4-field job
    jobs = [(str(path), s, s + chunk if k + 1 < len(starts) else None, scale) + extra
            for k, s in enumerate(starts)]
    try:
        parts = _run_jobs(path, jobs, workers, n)
        return _report_reads(path, _check_decode(path, jobs, parts), parts, reads)
    except ChunkedDecodeError as e:
        print(f"youtube_leg_ticks: {e}; decoding again in one pass (no seek)", file=sys.stderr)
    one = [(str(path), 0, None, scale) + extra]
    parts = _run_jobs(path, one, workers, n)
    return _report_reads(path, _check_decode(path, one, parts), parts, reads)


def decode_ticks(path, workers=4, scale=DECODE_SCALE, runs=()):
    """Per-frame (index, pts, tick | None, half, left, right) of a video file (`runs`: decode_raw).

    A run-scoped decode (`runs` given) appends the frame's run id, (..., left, right, run), None
    where no tick resolved: the timeline keeps every run's tick line apart (issue 1404 Task 5 part
    b). The default decode's rows are exactly what they were before."""
    raw = decode_raw(path, workers, scale, runs)
    if not runs:
        return [res + raw_row[2:] for res, raw_row in zip(resolve_ticks(raw), raw)]
    return [res + raw_row[2:4] + (raw_row[4] if res[2] is not None else None,)
            for res, raw_row in zip(resolve_ticks(raw), raw)]


def _open_text(path, mode="rt", gz=None):
    gz = str(path).endswith(".gz") if gz is None else gz
    return gzip.open(path, mode) if gz else open(path, mode.replace("t", ""))


def _cell(v):
    return "" if v is None else str(v)


def write_ticks(path, rows, header=None):
    """A tick map TSV (gzip when the name ends .gz): index, pts, tick, half[, raw left, raw right]."""
    tmp = f"{path}.tmp"
    with _open_text(tmp, "wt", gz=str(path).endswith(".gz")) as f:
        if header:
            f.write(f"# {header}\n")
        for r in rows:
            f.write("\t".join([str(r[0]), f"{r[1]:.3f}"] + [_cell(v) for v in r[2:]]) + "\n")
    os.replace(tmp, path)


def _rows(path):
    with _open_text(path) as f:
        for line in f:
            if not line.startswith("#") and line.strip():
                yield line.rstrip("\n").split("\t")


def _int(cell):
    return int(cell) if cell not in ("", "None") else None


def load_ticks(path, pts_offset=0.0, idx_offset=0):
    """A tick map TSV ('#' comments, .gz ok) -> [(index, pts, tick | None)]."""
    return [(int(c[0]) + idx_offset, float(c[1]) + pts_offset, _int(c[2]) if len(c) > 2 else None)
            for c in _rows(path)]


def load_run_ticks(path, pts_offset=0.0, idx_offset=0):
    """A run-scoped tick map TSV (a `runs` decode: index, pts, tick, half, left, right, run) ->
    [(index, pts, tick | None, run | None)], the timeline's run-scoped rows."""
    out = []
    for c in _rows(path):
        if len(c) < 7:
            raise ValueError(f"{path}: no run column (not a run-scoped tick map)")
        out.append((int(c[0]) + idx_offset, float(c[1]) + pts_offset, _int(c[2]), _int(c[6])))
    return out


def load_raw(path):
    """The raw halves a decode_ticks TSV keeps -> [(index, pts, left, right)] (resolve_ticks input)."""
    out = []
    for c in _rows(path):
        if len(c) < 6:
            raise ValueError(f"{path}: no raw left/right columns")
        out.append((int(c[0]), float(c[1]), _int(c[4]), _int(c[5])))
    return out
