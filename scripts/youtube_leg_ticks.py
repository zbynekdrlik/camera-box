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
QR_TOP_FRACTION = 0.62  # the painter's two big QRs sit in the top 62 % of the frame
DECODE_SCALE = 0.5
PHASE_RADIUS = 60  # frames: the local capture phase comes from both-halves frames this near
DECODER_VERSION = 2  # part of the tick-cache key: a map from another decoder is never reused
PROBE_COUNT_TIMEOUT_S = 600


def painter_payload(text):
    """(run, tick) of a valid painter payload (CRC-checked like Payload::decode), else None."""
    m = PAINTER_QR.match(text or "")
    if not m:
        return None
    run, tick, gen, crc = (int(g) for g in m.groups())
    if zlib.crc32(f"{run}.{tick}.{gen}".encode()) != crc or NODE_BURN_RUN.match(str(run)):
        return None
    return run, tick


def painter_tick(texts):
    """The painter tick among decoded QR texts (the lowest valid painter payload), or None."""
    ticks = [p[1] for p in map(painter_payload, texts or ()) if p is not None]
    return min(ticks) if ticks else None


def _qr_tick(det, plane, scale):
    import cv2

    if scale != 1.0:
        plane = cv2.resize(plane, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA)
    try:
        ok, texts, _, _ = det.detectAndDecodeMulti(plane)
    except cv2.error as e:  # one unreadable half is an undecodable half, never a crash
        print(f"youtube_leg_ticks: QR detector error on a frame half: {e}", file=sys.stderr)
        return None
    return painter_tick(texts) if ok else None


def _half_tick(det, img, scale):
    """Painter tick of one QR half: gray first, then the blue channel (the mid-transition colour)."""
    import cv2

    tick = _qr_tick(det, cv2.cvtColor(img, cv2.COLOR_BGR2GRAY), scale)
    if tick is None:
        tick = _qr_tick(det, np.ascontiguousarray(img[:, :, 0]), scale)
    return tick


def band_ticks(band, det, scale=DECODE_SCALE):
    """(left tick | None, right tick | None) of the QR band (the top 62 % of a frame)."""
    w = band.shape[1]
    return _half_tick(det, band[:, : w // 2], scale), _half_tick(det, band[:, w // 2:], scale)


def half_ticks(frame, det, scale=DECODE_SCALE):
    """(left tick | None, right tick | None) of one BGR frame."""
    return band_ticks(frame[0:int(frame.shape[0] * QR_TOP_FRACTION)], det, scale)


def _pair_phase(left, right):
    """1 = odd capture tick (right = left + 1), 0 = even (right = left - 1), None = no consistent pair."""
    if left is None or right is None or abs(right - left) != 1:
        return None
    return 1 if right > left else 0


def resolve_ticks(raw, radius=PHASE_RADIUS):
    """Per-frame (index, pts, even tick | None, half) from raw (index, pts, left, right) halves.

    half: 'B' both halves agree with the local phase, tick = left; 'L' left only, tick = left;
          'R' right only, tick = right - 1 (local phase odd) or right + 1 (even);
          'x' both halves read but contradict the local phase on both sides (a stale fresh half or a
              one-frame capture slip, which read the same): no tick;
          'r' right only and no local phase (none near, or a phase step between the sides): no tick;
          ''  nothing read.
    The local phase of a frame comes from the nearest both-halves frames before and after it (within
    `radius` frames, the frame itself excluded): one phase if they agree or only one exists."""
    own = {i: _pair_phase(left, right) for i, _, left, right in raw}
    known = sorted(i for i, ph in own.items() if ph is not None)

    def local_phase(i):
        k = bisect.bisect_left(known, i)
        before = next((known[j] for j in range(k - 1, -1, -1) if known[j] != i), None)
        after = next((known[j] for j in range(k, len(known)) if known[j] != i), None)
        near = {own[j] for j in (before, after) if j is not None and abs(j - i) <= radius}
        if len(near) != 1:
            return None
        both_sides = before is not None and after is not None and abs(before - i) <= radius and abs(after - i) <= radius
        return near.pop(), both_sides

    rows = []
    for i, p, left, right in raw:
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
    """Raw (index, pts, left, right) of frames [start, end) (end None = to the end of the file).
    Returns (rows, seek_ok, hit_end): a seek that does not land on `start` is reported, never
    decoded; hit_end = the file ran out before `end` (always, for end None)."""
    import cv2

    path, start, end, scale = job
    cap = cv2.VideoCapture(path)
    cap.set(cv2.CAP_PROP_POS_FRAMES, start)
    if int(cap.get(cv2.CAP_PROP_POS_FRAMES)) != start:
        cap.release()
        return [], False, False
    det = cv2.QRCodeDetector()
    out, hit_end = [], False
    i = start
    while end is None or i < end:
        ok, frame = cap.read()
        if not ok:
            hit_end = True
            break
        out.append((i, cap.get(cv2.CAP_PROP_POS_MSEC) / 1000.0) + half_ticks(frame, det, scale))
        i += 1
    if not hit_end and not cap.grab():  # the file ends exactly here (a frame count estimated too high)
        hit_end = True
    cap.release()
    return out, True, hit_end


def container_frames(path):
    """The video stream's packet count as ffprobe reads it (one packet per frame)."""
    r = run_bounded(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                     "stream=nb_read_packets", "-of", "csv=p=0", path], PROBE_COUNT_TIMEOUT_S, text=True, check=True)
    return int(r.stdout.strip().split(",")[0])


class FrameCountMismatch(RuntimeError):
    """The decode does not hold exactly the frames the container holds."""


def _merge_chunks(parts):
    """The rows of every chunk in pts order, numbered 0..n-1 again.

    OpenCV seeks a frame number by its TIMESTAMP, so behind a timestamp gap that is really in the
    file a chunk starts one frame early: that frame is read by two chunks (the same pts) and kept
    once. A chunk that started late leaves a frame no chunk read: the count check catches it. A
    chunk's own frame numbers are never trusted, they are where the seek was aimed, not where it
    landed."""
    by_pts = {}
    for rows, _, _ in parts:
        for r in rows:
            by_pts.setdefault(r[1], r)
    return [(k,) + r[1:] for k, r in enumerate(sorted(by_pts.values(), key=lambda r: r[1]))]


def _check_decode(path, jobs, parts):
    """The decode of `path` from its chunks, or RuntimeError.

    A missed seek is an error, a short read is allowed only once an earlier chunk reached the end of
    the file, and the decode must hold exactly the container's own packet count (ffprobe), else
    FrameCountMismatch. A gap that is really in the file (an encoder that skipped a frame) is kept:
    the timeline module judges it. A single pass is taken in its own frame order (no pts merge)."""
    raw = _merge_chunks(parts) if len(parts) > 1 else list(parts[0][0])
    if not raw:
        raise RuntimeError(f"no frame decoded from {path}")
    ended = None  # the start of the first chunk that ran into the end of the file
    for job, (rows, seek_ok, hit_end) in zip(jobs, parts):
        if ended is not None:
            if rows:
                raise RuntimeError(f"{path}: frames decoded after the end of the file at chunk {job[1]}")
            continue
        if not seek_ok:
            raise RuntimeError(f"{path}: seek to frame {job[1]} failed")
        if hit_end:
            ended = job[1]
    if ended is None:
        raise RuntimeError(f"{path}: the decode never reached the end of the file")
    n = container_frames(path)
    if len(raw) != n:
        what = "frames missing" if len(raw) < n else "frames read twice"
        raise FrameCountMismatch(f"{path}: {len(raw)} frames decoded, the container holds {n} ({what})")
    return raw


def decode_raw(path, workers=4, scale=DECODE_SCALE):
    """Raw (index, pts, left, right) of EVERY frame of a video file, or RuntimeError.

    The file is cut into chunks by its frame count, the last chunk reads to the end of the file (a
    frame count is an estimate in some containers), and the chunks are merged by pts. When the merge
    does not hold exactly the container's frames (a seek that landed late left a frame unread), the
    file is decoded once more in ONE pass from frame 0 (no seek) and judged again."""
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
    jobs = [(str(path), s, s + chunk if k + 1 < len(starts) else None, scale) for k, s in enumerate(starts)]
    if workers == 1:
        parts = [_decode_range(j) for j in jobs]
    else:
        import multiprocessing

        with multiprocessing.Pool(workers, initializer=cv2.setNumThreads, initargs=(1,)) as pool:
            parts = pool.map(_decode_range, jobs)  # one OpenCV thread per worker: no oversubscription
    try:
        return _check_decode(path, jobs, parts)
    except FrameCountMismatch as e:
        if len(jobs) == 1:
            raise
        print(f"youtube_leg_ticks: {e}; decoding again in one pass (no seek)", file=sys.stderr)
        one = [(str(path), 0, None, scale)]
        return _check_decode(path, one, [_decode_range(one[0])])


def decode_ticks(path, workers=4, scale=DECODE_SCALE):
    """Per-frame (index, pts, tick | None, half, left, right) of a video file."""
    raw = decode_raw(path, workers, scale)
    return [res + raw_row[2:] for res, raw_row in zip(resolve_ticks(raw), raw)]


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


def load_raw(path):
    """The raw halves a decode_ticks TSV keeps -> [(index, pts, left, right)] (resolve_ticks input)."""
    out = []
    for c in _rows(path):
        if len(c) < 6:
            raise ValueError(f"{path}: no raw left/right columns")
        out.append((int(c[0]), float(c[1]), _int(c[4]), _int(c[5])))
    return out
