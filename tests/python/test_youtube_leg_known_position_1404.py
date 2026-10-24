"""Issue 1404 (main design 6387919956, part a) -- the known-position QR read of the tick decoder.

The painter's two QRs sit at fixed positions in a recording, so after one full detection the decode
reads every next frame at the stored corner points (`QRCodeDetector.decode`) and runs the full
detector only when that read gives no valid painter payload (the fallback, which refreshes the
points). Pinned here:
  - every half the full detector reads reads the same, and the read at the stored points only ADDS
    halves the finder search missed, each the half's true tick: on the shared synthetic recording
    (with its blue-only and right-only stretches) and on a recording whose framing jumps (every
    half falls back once and reads on). Measured on the real yu1zFSHFdu8 VOD the same way: 309 of
    26940 halves added, 5 more frames with a tick, no tick changed;
  - the REAL session-3 pixels read exactly like the full detector in order through one detector
    (the stale fresh half of VOD frame 20174, the colour-coded half of recording frame 2100);
  - the fallback is counted: the decode reports its known-position reads, the fallbacks and the
    full detections, and a plain cv2 detector passed in by a caller keeps the old full read.
"""
import pathlib
import subprocess
import sys

import numpy as np
import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from youtube_leg_fakes_1404 import FIX, FPS, payload, qr_frame, shared_rec_video, ylv  # noqa: E402

ylt = sys.modules["youtube_leg_ticks"]


def _jpg(name):
    import cv2

    return cv2.imread(str(FIX / f"{name}.jpg"))


@pytest.fixture(scope="module")
def rec_video(tmp_path_factory):
    return shared_rec_video(tmp_path_factory)


def _full_rows(path, monkeypatch):
    """The reference: the same decode with the known-position read switched off."""
    monkeypatch.setattr(ylt, "KNOWN_POSITION_READ", False)
    try:
        return ylt.decode_raw(path, workers=1)
    finally:
        monkeypatch.setattr(ylt, "KNOWN_POSITION_READ", True)


def _reads_on_top_of(full, fast, truth):
    """Every half the full detector read reads the same in `fast`; the known-position read only ADDS
    halves the full detector missed, each its true tick (`truth(index, column)`). The resolved
    ticks agree wherever the full decode resolved one. Returns the number of halves added."""
    assert [r[:2] for r in fast] == [r[:2] for r in full]
    added = 0
    for f, k in zip(full, fast):
        for col in (2, 3):
            if f[col] is not None:
                assert k[col] == f[col], (f, k)
            elif k[col] is not None:
                assert k[col] == truth(f[0], col), (f, k)
                added += 1
    for f, k in zip(ylt.resolve_ticks(full), ylt.resolve_ticks(fast)):
        if f[2] is not None:
            assert k[2] == f[2], (f, k)
        assert k[3] != "x" or f[3] == "x", (f, k)  # an added half never makes a contradiction frame
    return added


def test_the_known_position_decode_reads_what_the_full_detector_reads(rec_video, monkeypatch):
    full = _full_rows(rec_video, monkeypatch)
    reads = {}
    fast = ylt.decode_raw(rec_video, workers=1, reads=reads)
    # the shared recording: tick 1000 + 2k on the left, captured on even ticks (right = left - 1),
    # no left QR in frames 120-149
    _reads_on_top_of(full, fast, lambda i, col: 1000 + 2 * i - (col - 2))
    assert sum(r[2] is not None for r in fast) >= sum(r[2] is not None for r in full)
    frames = len(full)
    # the stored points carry the read: far more planes are read at the known position than detected
    assert reads["frames"] == frames
    assert reads["known"] >= frames  # two halves a frame, nearly all read at the stored points
    assert reads["full"] < frames // 2
    assert reads["known"] + reads["full"] >= 2 * frames  # every half is read somehow, and counted
    assert reads["fallback"] >= 1  # the right-only stretch: the left's stored points read nothing
    # a fallback is a half whose stored points read nothing AND that the full detector then read:
    # never more than the full detections; a point read that missed is counted on its own
    assert reads["fallback"] <= reads["full"] and reads["misses"] >= reads["fallback"]


def _write_jumping_video(path, frames=90, jump_at=30, shift=(20, 60)):
    """A 960x540 FFV1 clip of the painter's two QRs whose whole picture moves `shift` = (down, right)
    px at frame `jump_at` (a cut to another framing): the stored points no longer hold a QR, and
    both QRs stay inside their half of the QR band."""
    ff = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24",
                           "-s", "960x540", "-r", str(FPS), "-i", "-", "-c:v", "ffv1", "-pix_fmt", "bgr0", str(path)],
                          stdin=subprocess.PIPE)
    for k in range(frames):
        f = qr_frame(payload(2000 + 2 * k), payload(2001 + 2 * k))
        if k >= jump_at:
            moved = np.full_like(f, 255)
            moved[shift[0]:, shift[1]:] = f[:-shift[0], :-shift[1]]
            f = moved
        ff.stdin.write(f.tobytes())
    ff.stdin.close()
    assert ff.wait() == 0


def test_a_known_position_read_that_fails_falls_back_and_reads_on_at_the_new_points(tmp_path, monkeypatch):
    video = tmp_path / "jump.mkv"
    _write_jumping_video(video)
    full = _full_rows(video, monkeypatch)
    reads = {}
    fast = ylt.decode_raw(video, workers=1, reads=reads)
    _reads_on_top_of(full, fast, lambda i, col: 2000 + 2 * i + (col - 2))
    assert all(r[2] is not None and r[3] is not None for r in fast)  # every frame read after the jump too
    # after the jump the old points read nothing: the full detector runs again for both halves
    # (counted), then the new points carry the rest of the clip
    assert reads["fallback"] >= 2
    # 90 frames in 4 chunks: each chunk's first frame detects both halves, the jump's frame both again
    assert reads["known"] >= 2 * 90 - 2 * 4 - 2 - 4


def test_real_session3_pixels_read_in_order_through_one_known_position_detector():
    import cv2

    got = {}
    for names in (["s3-vod-frame20173", "s3-vod-frame20174", "s3-vod-frame20176"],
                  ["s3-rec_a-frame6000", "s3-rec_a-frame2100"]):
        known = ylt.KnownPositionDetector()
        for name in names:
            band = _jpg(name)
            got[name] = ylv.band_ticks(band, known, 1.0)
            full = ylv.band_ticks(band, cv2.QRCodeDetector(), 1.0)
            for k, f in zip(got[name], full):  # a half the full detector reads reads the same
                assert f is None or k == f, (name, got[name], full)
        assert known.stats()["known"] >= 1  # the later frames really read at the stored points
    # the stale fresh half of VOD frame 20174 reads exactly as before (the 'x' frame stays an 'x')
    assert got["s3-vod-frame20174"] == ylv.band_ticks(_jpg("s3-vod-frame20174"), cv2.QRCodeDetector(), 1.0)
    # frame 2100: the full detector reads the colour-coded left only (through blue); at the points
    # frame 6000 left, the settled right half reads too, one below the left (an even capture tick)
    assert got["s3-rec_a-frame2100"] == (2196754, 2196753)
    assert ylv.band_ticks(_jpg("s3-rec_a-frame2100"), cv2.QRCodeDetector(), 1.0) == (2196754, None)


def test_a_plain_cv2_detector_keeps_the_full_read():
    import cv2

    frame = qr_frame(payload(1000), payload(1001))
    det = cv2.QRCodeDetector()
    assert ylv.half_ticks(frame, det) == (1000, 1001)
    known = ylt.KnownPositionDetector()
    assert ylv.half_ticks(frame, known) == (1000, 1001)
    assert ylv.half_ticks(frame, known) == (1000, 1001)
    st = known.stats()
    assert (st["full"], st["known"], st["fallback"]) == (2, 2, 0)


def test_the_decode_reports_its_reads_on_stderr(rec_video, capsys):
    ylt.decode_raw(rec_video, workers=1)
    err = capsys.readouterr().err
    assert "known-position reads" in err and "full detections" in err and "fallback" in err
    assert "point reads that missed" in err


def test_the_early_decode_primes_the_cache_the_verdict_run_reads(rec_video, tmp_path, monkeypatch):
    """--prime-ticks (the E2E's early decode) writes the map the verdict run's cached_ticks loads: the
    same file name, the same key (the recording's absolute path), and its read counters."""
    out = tmp_path / "verdict"
    assert ylv.main(["--prime-ticks", str(rec_video), "--out", str(out), "--workers", "1"]) == ylv.EXIT_PASS
    assert (out / "ticks-rec-1.tsv").exists() and (out / "ticks-rec-1.tsv.reads.json").exists()

    def no_decode(*a, **kw):
        raise AssertionError("the verdict run decoded the recording again")

    monkeypatch.setattr(ylv, "decode_ticks", no_decode)
    rows = ylv.cached_ticks(str(rec_video), str(out / "ticks-rec-1.tsv"), 1)
    assert len(rows) == FPS * 8 and rows[0][2] == 1000


def test_a_failed_early_decode_exits_unknown_and_leaves_no_map(tmp_path):
    bad = tmp_path / "not-a-video.mp4"
    bad.write_bytes(b"no video here")
    out = tmp_path / "verdict"
    assert ylv.main(["--prime-ticks", str(bad), "--out", str(out), "--workers", "1"]) == ylv.EXIT_UNKNOWN
    assert not (out / "ticks-rec-1.tsv").exists()
