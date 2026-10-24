"""Issue 1404 (main design 6387919956, part a) -- the known-position QR read of the tick decoder.

The painter's two QRs sit at fixed positions in a recording, so after one full detection the decode
reads every next frame at the stored corner points (`QRCodeDetector.decode`) and runs the full
detector only when that read gives no valid painter payload (the fallback, which refreshes the
points). Pinned here:
  - the rows are exactly those of the full detector, on the shared synthetic recording (with its
    blue-only and right-only stretches), on a recording whose framing jumps (every half falls back
    once and reads on), and on the REAL session-3 pixels read in order through one detector (the
    stale fresh half of VOD frame 20174 and the colour-coded half of recording frame 2100);
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


def test_the_known_position_decode_reads_exactly_the_full_detectors_rows(rec_video, monkeypatch):
    full = _full_rows(rec_video, monkeypatch)
    reads = {}
    fast = ylt.decode_raw(rec_video, workers=1, reads=reads)
    assert fast == full
    frames = len(full)
    # the stored points carry the read: far more planes are read at the known position than detected
    assert reads["frames"] == frames
    assert reads["known"] >= frames  # two halves a frame, nearly all read at the stored points
    assert reads["full"] < frames // 2
    # the right-only stretch (120-149) has no left QR: each of its left halves falls back, counted
    assert reads["fallback"] >= 30


def _write_jumping_video(path, frames=90, jump_at=45, shift=60):
    """A 960x540 FFV1 clip of the painter's two QRs whose whole picture moves `shift` px right and
    down at frame `jump_at` (a cut to another framing): the stored points no longer hold a QR."""
    ff = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24",
                           "-s", "960x540", "-r", str(FPS), "-i", "-", "-c:v", "ffv1", "-pix_fmt", "bgr0", str(path)],
                          stdin=subprocess.PIPE)
    for k in range(frames):
        f = qr_frame(payload(2000 + 2 * k), payload(2001 + 2 * k))
        if k >= jump_at:
            moved = np.full_like(f, 255)
            moved[shift:, shift:] = f[:-shift, :-shift]
            f = moved
        ff.stdin.write(f.tobytes())
    ff.stdin.close()
    assert ff.wait() == 0


def test_a_known_position_read_that_fails_falls_back_and_reads_on_at_the_new_points(tmp_path, monkeypatch):
    video = tmp_path / "jump.mkv"
    _write_jumping_video(video)
    full = _full_rows(video, monkeypatch)
    assert all(r[2] is not None and r[3] is not None for r in full)  # the full detector reads every frame
    reads = {}
    assert ylt.decode_raw(video, workers=1, reads=reads) == full
    # after the jump the old points read nothing: the full detector runs again for both halves
    # (counted), then the new points carry the rest of the clip
    assert reads["fallback"] >= 2
    assert reads["known"] >= 2 * 90 - 10


def test_real_session3_pixels_read_in_order_through_one_known_position_detector():
    import cv2

    for names in (["s3-vod-frame20173", "s3-vod-frame20174", "s3-vod-frame20176"],
                  ["s3-rec_a-frame6000", "s3-rec_a-frame2100"]):
        known = ylt.KnownPositionDetector()
        for name in names:
            band = _jpg(name)
            assert ylv.band_ticks(band, known, 1.0) == ylv.band_ticks(band, cv2.QRCodeDetector(), 1.0), name
        assert known.stats()["known"] >= 1  # the later frames really read at the stored points


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
