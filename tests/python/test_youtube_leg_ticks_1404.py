"""Issue 1404 Task 1 -- the painter-tick decoder of the YouTube-leg tool (scripts/youtube_leg_ticks.py).

The dual-QR Vernier paints the latest EVEN tick on the left and the latest ODD tick on the right
(src/probe/painter.rs vernier_ids). Pinned here, on generated frames and on REAL session-3 pixels
(tests/fixtures/youtube_leg_1404/s3-*-frame*.jpg: the QR band of the frame, scaled 0.5 like the
decoder, JPEG q95 that decodes exactly like the lossless crop):
  - the colour-coded half the gray detector cannot read is read through the blue channel;
  - a right-only frame takes the LOCAL capture phase (it moves during a session);
  - a frame whose two halves contradict the phase of both neighbours (a stale fresh half: VOD frames
    20174 / 20210) is left undecoded, never turned into a false repeat + skip;
  - the parallel chunks are merged by pts: a frame two chunks read (a seek that landed early) is kept
    once, a frame no chunk read sends the file through one sequential pass; a decode that does not
    hold exactly the container's frames, or a missed seek (also of the last chunk), is an error,
    never a quietly shorter or mislabelled map.
Expected ticks come from the session's own left-only decode (qrticks.py, an independent run) where it
decoded the frame; for frame 2100, which it could not read, only its bracketing anchors are
independent (see the test).
"""
import gzip
import pathlib
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from youtube_leg_fakes_1404 import (COLOUR_DARK, COLOUR_LIGHT, FIX, FPS, SECONDS, drop_frame,  # noqa: E402
                                    payload, qr_frame, shared_rec_video, ylv)


def _jpg(name):
    import cv2

    return cv2.imread(str(FIX / f"{name}.jpg"))


def test_painter_payload_checks_the_crc_and_the_reserved_burn_ids():
    assert ylv.painter_payload(payload(1000)) == (123456, 1000)
    good = payload(1000)
    assert ylv.painter_payload(good[:-1] + str((int(good[-1]) + 1) % 10)) is None  # a wrong CRC
    assert ylv.painter_payload(payload(5, run=911001)) is None  # a node burn, never the painter
    assert ylv.painter_payload(payload(5, run=911014)) is None  # the SongPlayer origin burn
    assert ylv.painter_payload(payload(7, run=911042817)) == (911042817, 7)  # a 9-digit E2E run id
    assert ylv.painter_tick([payload(204), payload(202), payload(9, run=911014), "junk"]) == 202


def test_half_ticks_reads_both_halves_and_a_colour_half_through_blue():
    import cv2

    det = cv2.QRCodeDetector()
    assert ylv.half_ticks(qr_frame(payload(1000), payload(1001)), det) == (1000, 1001)
    assert ylv.half_ticks(qr_frame(None, payload(1001)), det) == (None, 1001)
    assert ylv.half_ticks(qr_frame(payload(5, run=911001), payload(1003)), det) == (None, 1003)
    assert ylv.half_ticks(qr_frame(None, None), det) == (None, None)
    colour = qr_frame(payload(1000), payload(999), left_colours=(COLOUR_DARK, COLOUR_LIGHT))
    gray = cv2.cvtColor(colour[0:335, :480], cv2.COLOR_BGR2GRAY)
    assert ylv._qr_tick(det, gray, ylv.DECODE_SCALE) is None  # gray alone cannot read it ...
    assert ylv.half_ticks(colour, det) == (1000, 999)  # ... its blue channel can


def test_a_right_only_frame_takes_the_local_capture_phase():
    # odd phase (right = left + 1): right-only 1003 -> 1002; even phase (right = left - 1): 1001 -> 1002
    odd = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1003), (2, 0.067, 1004, 1005)]
    even = [(0, 0.0, 1000, 999), (1, 0.033, None, 1001), (2, 0.067, 1004, 1003)]
    assert [r[2:] for r in ylv.resolve_ticks(odd)] == [(1000, "B"), (1002, "R"), (1004, "B")]
    assert [r[2:] for r in ylv.resolve_ticks(even)] == [(1000, "B"), (1002, "R"), (1004, "B")]
    # a repeated frame stays a repeat (the phase never comes from the frame's own cadence)
    dup = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1001), (2, 0.067, 1002, 1003)]
    assert [r[2] for r in ylv.resolve_ticks(dup)] == [1000, 1000, 1002]
    # a phase step between the two sides, or no both-halves frame near: no tick, never a guess
    step = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1003), (2, 0.067, 1004, 1003)]
    assert ylv.resolve_ticks(step)[1][2:] == (None, "r")
    assert ylv.resolve_ticks([(0, 0.0, 1000, 1001), (500, 16.7, None, 1999)])[1][2:] == (None, "r")
    # a pair that disagrees (|right - left| != 1) gives no phase; the left still counts
    assert ylv.resolve_ticks([(0, 0.0, 1000, 1007)])[0][2:] == (1000, "L")


def test_a_pair_that_contradicts_the_phase_on_both_sides_is_left_undecoded():
    # even-phase neighbours; the middle frame reads left = previous tick, right = left + 1
    raw = [(0, 0.0, 2238770, 2238769), (1, 0.033, 2238772, 2238771), (2, 0.067, 2238772, 2238773),
           (3, 0.1, 2238776, 2238775), (4, 0.133, 2238778, 2238777)]
    assert [r[2:] for r in ylv.resolve_ticks(raw)][2] == (None, "x")
    # a real phase change (the phases differ on the two sides) keeps the frame's own reading
    flip = [(0, 0.0, 1000, 999), (1, 0.033, 1002, 1001), (2, 0.067, 1004, 1005), (3, 0.1, 1006, 1007)]
    assert [r[2] for r in ylv.resolve_ticks(flip)] == [1000, 1002, 1004, 1006]


def test_real_colour_coded_halves_read_through_blue():
    import cv2

    det = cv2.QRCodeDetector()
    # part 1 frame 2100, the colour-coded-LEFT period: the session's gray left decoder read nothing in
    # frames 1565..4124. Its anchors 1564 = 2195680 and 4125 = 2200804 hold exactly one +2 step more
    # than the 2-tick cadence, so frame 2100 is 2195680 + 2 * 536 or that + 2, depending on where the
    # step is (this decoder's own halves put a capture phase change at 1565: the + 2).
    band = _jpg("s3-rec_a-frame2100")
    gray_left = cv2.cvtColor(band[:, : band.shape[1] // 2], cv2.COLOR_BGR2GRAY)
    assert ylv._qr_tick(det, gray_left, 1.0) is None  # gray cannot read the colour-coded left ...
    assert ylv.band_ticks(band, det, 1.0)[0] in (2196752, 2196754)  # ... blue reads it, inside the anchors
    # part 1 frame 6000, the colour-coded-RIGHT period (odd phase): the session's left value + 1
    assert ylv.band_ticks(_jpg("s3-rec_a-frame6000"), det, 1.0) == (2204552, 2204553)


def test_real_stale_fresh_half_is_left_undecoded():
    import cv2

    det = cv2.QRCodeDetector()
    raw = [(i, i / 30.0) + ylv.band_ticks(_jpg(f"s3-vod-frame{i}"), det, 1.0) for i in (20173, 20174, 20176)]
    assert raw[1][2:] == (2238772, 2238773)  # the fresh left read stale: the session map says 2238772 twice
    rows = ylv.resolve_ticks(raw)
    assert [r[2:] for r in rows] == [(2238772, "B"), (None, "x"), (2238778, "B")]


def test_the_tick_map_keeps_the_raw_halves(tmp_path):
    rows = [(0, 0.0, 1000, "B", 1000, 999), (1, 0.033, None, "x", 1000, 1001), (2, 0.067, None, "", None, None)]
    p = tmp_path / "t.tsv.gz"
    ylv.write_ticks(p, rows, header="test")
    assert ylv.load_ticks(p) == [(0, 0.0, 1000), (1, 0.033, None), (2, 0.067, None)]
    assert ylv.load_raw(p) == [(0, 0.0, 1000, 999), (1, 0.033, 1000, 1001), (2, 0.067, None, None)]


@pytest.fixture(scope="module")
def rec_video(tmp_path_factory):
    return shared_rec_video(tmp_path_factory)


@pytest.fixture
def cheap_halves(monkeypatch):
    """Decode MECHANICS only (chunks, seeks, counts): a content fingerprint of the frame stands in
    for the QR read (~60 ms a frame), so a row still proves which frame it came from."""
    monkeypatch.setattr(sys.modules["youtube_leg_ticks"], "half_ticks",
                        lambda frame, det, scale: (int(frame[::5, ::5].sum()), None))


def test_decode_ticks_reads_colour_and_right_only_stretches(rec_video):
    rows = ylv.decode_ticks(rec_video, workers=1)
    assert len(rows) == FPS * SECONDS
    # a detector miss leaves a frame undecoded; a decoded frame is never wrong
    want = [1000 + 2 * k for k in range(FPS * SECONDS)]
    assert all(r[2] in (None, w) for r, w in zip(rows, want))
    assert sum(r[2] is not None for r in rows) >= 0.95 * len(rows)
    assert sum(r[3] == "B" for r in rows[60:90]) >= 27  # the colour-coded left read through blue
    assert sum(r[3] == "R" for r in rows[120:150]) >= 27  # right only, even ticks: right + 1
    assert {r[3] for r in rows[120:150]} <= {"R", ""}
    assert rows[30][1] - rows[0][1] == pytest.approx(1.0, abs=0.002)  # pts: the container's timeline
    c = ylv.continuity([r[:3] for r in rows])
    assert c["cadence_proven"] == len(rows) and c["events"] == 0


def test_decode_raw_refuses_a_hole_or_a_missed_seek(rec_video, cheap_halves, monkeypatch):
    ticks_mod = sys.modules["youtube_leg_ticks"]
    real = ticks_mod._decode_range

    assert len(ylv.decode_raw(rec_video, workers=1)) == FPS * SECONDS  # the real decode tiles the file

    def short_middle(job):  # the decoder stops 3 frames early, in the chunks AND in the one-pass decode
        rows, ok, _ = real(job)
        return (rows[:-3], ok, job[2] is None) if job[1] == 0 else (rows, ok, job[2] is None)

    monkeypatch.setattr(ticks_mod, "_decode_range", short_middle)
    with pytest.raises(RuntimeError, match="frames missing"):
        ylv.decode_raw(rec_video, workers=1)

    def missed_seek(job):
        return ([], False, False) if job[1] > 0 and job[2] is not None else real(job)

    monkeypatch.setattr(ticks_mod, "_decode_range", missed_seek)
    with pytest.raises(RuntimeError, match="seek"):
        ylv.decode_raw(rec_video, workers=1)

    def last_seek_missed(job):  # every earlier chunk read fully, the last one never started
        return ([], False, False) if job[2] is None else real(job)

    monkeypatch.setattr(ticks_mod, "_decode_range", last_seek_missed)
    with pytest.raises(RuntimeError, match="seek"):
        ylv.decode_raw(rec_video, workers=1)

    def skipped_frame(job):  # a decoder that drops a frame inside a chunk: the pts jump two intervals
        rows, ok, end = real(job)
        if job[1] == 0:
            rows = [(i, p + (1 / FPS if i >= 10 else 0.0), lt, rt) for i, p, lt, rt in rows]
        return rows, ok, end

    monkeypatch.setattr(ticks_mod, "_decode_range", skipped_frame)
    monkeypatch.setattr(ticks_mod, "container_frames", lambda path: FPS * SECONDS + 1)  # the file has one more
    with pytest.raises(RuntimeError, match="frames missing"):
        ylv.decode_raw(rec_video, workers=1)


def _one_pass(path):
    return sys.modules["youtube_leg_ticks"]._decode_range((str(path), 0, None, ylv.DECODE_SCALE))[0]


def test_a_seek_that_lands_a_frame_early_never_mislabels_the_frames_behind_it(rec_video, cheap_halves, monkeypatch):
    ticks_mod = sys.modules["youtube_leg_ticks"]
    real = ticks_mod._decode_range
    truth = _one_pass(rec_video)
    first = {}

    def early(job):  # the second chunk starts one frame early: one frame read twice, one never read,
        path, s, e, scale = job  # the same total as a clean decode
        if s > 0 and e is not None and first.setdefault("s", s) == s:
            rows, ok, end = real((path, s - 1, e - 1, scale))
            return [(s + k,) + r[1:] for k, r in enumerate(rows)], ok, end
        return real(job)

    monkeypatch.setattr(ticks_mod, "_decode_range", early)
    assert ylv.decode_raw(rec_video, workers=1) == truth


@pytest.fixture(scope="module")
def gap_video(rec_video, tmp_path_factory):
    gap = tmp_path_factory.mktemp("ylv-gap") / "gap.mkv"  # frame 120 dropped, the other timestamps kept:
    drop_frame(rec_video, gap, 120, keep_timestamps=True)  # an encoder that skipped a frame
    return gap


def test_a_timestamp_gap_really_in_the_file_is_kept_and_a_chunk_ending_with_the_file_is_the_end(rec_video, gap_video,
                                                                                              cheap_halves, monkeypatch):
    ticks_mod = sys.modules["youtube_leg_ticks"]
    real = ticks_mod._decode_range
    jobs = []
    monkeypatch.setattr(ticks_mod, "_decode_range", lambda job: jobs.append(job[1:3]) or real(job))
    raw = ylv.decode_raw(gap_video, workers=1)
    # OpenCV seeks by timestamp, so behind the gap a chunk starts one frame early: that frame is kept
    # once and the parallel decode stands, no whole-file second pass (minutes on a real recording)
    assert len(jobs) > 1 and (0, None) not in jobs
    monkeypatch.setattr(ticks_mod, "_decode_range", real)
    assert raw == _one_pass(gap_video)
    gap = gap_video
    assert len(raw) == FPS * SECONDS - 1 == ylv.container_frames(gap)
    assert ylv.timestamp_gaps([(i, p, lt) for i, p, lt, _ in raw]) == [120]
    # a chunk whose end is exactly the last frame reports the end of the file (one more grab fails)
    rows, seek_ok, hit_end = sys.modules["youtube_leg_ticks"]._decode_range((str(rec_video), 200, FPS * SECONDS, 0.5))
    assert (len(rows), seek_ok, hit_end) == (40, True, True)
    rows, seek_ok, hit_end = sys.modules["youtube_leg_ticks"]._decode_range((str(rec_video), 200, FPS * SECONDS - 1, 0.5))
    assert (len(rows), hit_end) == (39, False)


def test_the_committed_session3_maps_carry_their_raw_halves():
    for name in ("s3-rec_a-ticks.tsv.gz", "s3-vod-ticks.tsv.gz"):
        with gzip.open(FIX / name, "rt") as f:
            head = f.readline()
        assert f"decoder v{ylv.DECODER_VERSION}" in head
        raw = ylv.load_raw(FIX / name)
        stored = ylv.load_ticks(FIX / name)
        again = ylv.resolve_ticks(raw)
        # the stored ticks are what the decoder's resolution gives from the stored halves (frames
        # within PHASE_RADIUS of a cut edge of the fixture lost a neighbour, so they are left out)
        edges = [x for r0, r in zip(raw, raw[1:]) if r[0] != r0[0] + 1 for x in (r0[0], r[0])]
        far = [k for k, r in enumerate(raw) if all(abs(r[0] - e) > ylv.PHASE_RADIUS for e in edges)]
        assert len(far) > 0.9 * len(raw)
        assert [again[k][2] for k in far] == [stored[k][2] for k in far]
