"""Issue 1404 Task 1 -- the YouTube-leg measurement tool `scripts/youtube_leg_verdict.py`.

The tool decides, per measured window, whether the YouTube VOD shows what stream OBS sent: painter-tick
dup/skip, the late join after every publish, A/V (recording-verdict --av-sync, VOD - recording) and
audio continuity (0.25 s block cross-correlation). It is a release gate (camera-box + restreamer), so
it is pinned on REAL data: the three manual sessions of 5./6.10.2026 (issue 1404 comments 6003342015 /
6004613512 baseline, 6006986090 session 2, 6008636005 session 3), cut into tests/fixtures/youtube_leg_1404:

  base-*  restreamer 0.29.27 (the bug): A/V -1368 ms after the republish, 46 dup / 47+ skip, late join
  s2-*    restreamer 0.29.28, suspend + Stop/Start: everything clean
  s3-*    0.29.28 + a stream-OBS restart: recording in TWO parts, VOD content starting 30 s after
          window A's start (behind a 55 s QR-less, silent pre-roll); recording part 1 (s3-rec_a) and
          the VOD (s3-vod) are decoded with BOTH QR halves by the tool itself (the left-only session
          decoder read part 1 76 % decodable / 83.5 % cadence-proven in window A)

Tick maps are the original per-frame maps cut to the windows (indices and pts kept); audio is 20 s
16 kHz mono FLAC cut at tick-matched positions (audio-clips.json = each clip's start pts); *.avsync.out
are the saved `recording-verdict --av-sync` outputs. Synthetic tests (generated QR frames / videos, a
fake probe) cover the both-halves decoder and the CLI contract end to end. Tier-0: pytest + ffmpeg,
no cargo, no rig.
"""
import importlib.util
import json
import pathlib
import subprocess
import sys

import numpy as np
import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "youtube_leg_verdict.py"
FIX = ROOT / "tests" / "fixtures" / "youtube_leg_1404"

_spec = importlib.util.spec_from_file_location("youtube_leg_verdict", SCRIPT)
ylv = importlib.util.module_from_spec(_spec)
sys.modules["youtube_leg_verdict"] = ylv
_spec.loader.exec_module(ylv)


def hms(h, m, s):
    return h * 3600 + m * 60 + s


# Record starts (content seconds of day, UTC) and windows, as in the session comments.
BASE_T0 = hms(20, 22, 43)
BASE_WINDOWS = [("A", hms(20, 23, 13), hms(20, 30, 0)), ("R", hms(20, 33, 35), hms(20, 34, 15)),
                ("B", hms(20, 36, 53), hms(20, 37, 33))]
S2_T0 = hms(23, 22, 22.405)
S2_WINDOWS = [("A", hms(23, 23, 0), hms(23, 31, 25)), ("R", hms(23, 32, 5), hms(23, 32, 50)),
              ("B", hms(23, 33, 10), hms(23, 39, 25))]
S3_A_START, S3_B_START = hms(1, 40, 15.901), hms(1, 51, 53.503)
S3_WINDOWS = [("A", hms(1, 40, 40), hms(1, 49, 28)), ("R", hms(1, 50, 8), hms(1, 51, 4)),
              ("B", hms(1, 52, 10), hms(1, 58, 5))]


@pytest.fixture(scope="module")
def base():
    return ylv.load_ticks(FIX / "base-rec-ticks.tsv.gz"), ylv.load_ticks(FIX / "base-vod-ticks.tsv.gz")


@pytest.fixture(scope="module")
def s2():
    return ylv.load_ticks(FIX / "s2-rec-ticks.tsv.gz"), ylv.load_ticks(FIX / "s2-vod-ticks.tsv.gz")


@pytest.fixture(scope="module")
def s3():
    rows, t0 = ylv.join_parts([(FIX / "s3-rec_a-ticks.tsv.gz", S3_A_START), (FIX / "s3-rec_b-ticks.tsv.gz", S3_B_START)])
    return rows, t0, ylv.load_ticks(FIX / "s3-vod-ticks.tsv.gz")


def clips(name):
    meta = json.loads((FIX / "audio-clips.json").read_text())[name]
    return ylv.load_audio(FIX / f"{name}-rec.flac"), ylv.load_audio(FIX / f"{name}-vod.flac"), meta


def av(sess, window):
    return ylv.av_from_outputs(ylv.parse_avsync_output((FIX / f"{sess}-rec-{window}.avsync.out").read_text()),
                               ylv.parse_avsync_output((FIX / f"{sess}-vod-{window}.avsync.out").read_text()))


# ---------------------------------------------------------------- criterion 2: dup/skip on real sessions

def test_session2_steady_windows_have_zero_downstream_dupskip(s2):
    rec, vod = s2
    frames = {"A": 14754, "R": 1309, "B": 10854}  # decoded recording frames, comment 6006986090
    for name, a, b in S2_WINDOWS:
        r = ylv.dupskip(rec, vod, S2_T0, a, b)
        assert (r["dup"], r["skip"], r["unjudged"]) == (0, 0, 0), (name, r)
        assert r["rec_frames"] == frames[name]
        assert r["clamped_start_utc"] is None and r["clamped_end_utc"] is None


def test_baseline_dupskip_reproduces_the_session_counts(base):
    rec, vod = base
    counts = {name: ylv.dupskip(rec, vod, BASE_T0, a, b) for name, a, b in BASE_WINDOWS}
    assert (counts["A"]["dup"], counts["A"]["skip"]) == (10, 10)
    assert (counts["B"]["dup"], counts["B"]["skip"]) == (2, 2)
    # R, after the republish: the session tool counted 46 / 47. The tool also judges the VOD pair that
    # straddles the window END: the VOD jumped from tick 1090906 to 1090910 over the recording's last
    # window frame 1090908, which the session tool's in-window VOD filter could not see.
    r = counts["R"]
    assert (r["dup"], r["skip"]) == (46, 48)
    # the window ending one frame earlier no longer holds tick 1090908: back to the session's 47
    _, a, b = BASE_WINDOWS[1]
    shorter = ylv.dupskip(rec, vod, BASE_T0, a, b - 1.0 / 30)
    assert (shorter["dup"], shorter["skip"]) == (46, 47)


def test_dupskip_counts_a_vod_jump_over_the_window_edge():
    # recording: ticks 100..120 step 2 at 30 fps; VOD shows 100..112, then jumps 112 -> 118 (two
    # recording frames lost) with the jump straddling the window END at content time of tick 116.
    rec = [(i, i / 30.0, 100 + 2 * i) for i in range(11)]
    vod = [(k, k / 30.0, t) for k, t in enumerate([100, 102, 104, 106, 108, 110, 112, 118, 120])]
    r = ylv.dupskip(rec, vod, 0.0, 0.0, 8.5 / 30.0)  # window holds ticks 100..116
    assert (r["dup"], r["skip"]) == (0, 2)
    rep = [(k, k / 30.0, t) for k, t in enumerate([100, 102, 104, 104, 106, 108, 110, 112, 114, 116])]
    assert (ylv.dupskip(rec, rep, 0.0, 0.0, 8.5 / 30.0)["dup"]) == 1


def test_a_tick_the_rig_repeated_cancels_out():
    rec = [(0, 0.0, 100), (1, 1 / 30, 102), (2, 2 / 30, 102), (3, 3 / 30, 106), (4, 4 / 30, 108)]
    vod = [(0, 0.0, 100), (1, 1 / 30, 102), (2, 2 / 30, 102), (3, 3 / 30, 106), (4, 4 / 30, 108)]
    r = ylv.dupskip(rec, vod, 0.0, 0.0, 1.0)
    assert (r["dup"], r["skip"]) == (0, 0)


# ---------------------------------------------------------------- Review Focus 2: the two-part join

def test_session3_two_part_join_has_no_seam_artefact(s3):
    rows, t0, vod = s3
    a, b = ylv.load_ticks(FIX / "s3-rec_a-ticks.tsv.gz"), ylv.load_ticks(FIX / "s3-rec_b-ticks.tsv.gz")
    ca, cb, cj = ylv.continuity(a), ylv.continuity(b), ylv.continuity(rows)
    assert cj["frames"] == ca["frames"] + cb["frames"]
    assert cj["events"] == ca["events"] + cb["events"]  # the seam adds no event ...
    assert cj["cadence_proven"] == ca["cadence_proven"] + cb["cadence_proven"]  # ... and proves nothing
    assert cj["unjudged_gaps"] == ca["unjudged_gaps"] + cb["unjudged_gaps"] + 1  # the seam: unjudged
    # The VOD skips the OBS-down time, but no window spans it: B judged on the joined rows equals B
    # judged on part 2 alone.
    _, wa, wb = S3_WINDOWS[2]
    joined = ylv.dupskip(rows, vod, t0, wa, wb)
    alone = ylv.dupskip(b, vod, S3_B_START, wa, wb)
    for k in ("dup", "skip", "unjudged", "rec_frames", "vod_frames"):
        assert joined[k] == alone[k], k
    assert (joined["dup"], joined["skip"], joined["unjudged"]) == (0, 0, 2)  # 2 VOD repeats unjudged


def test_a_part_without_start_is_placed_by_painter_tick():
    a, b = ylv.load_ticks(FIX / "s3-rec_a-ticks.tsv.gz"), ylv.load_ticks(FIX / "s3-rec_b-ticks.tsv.gz")
    _, _, starts = ylv.join_part_rows([a, b], [S3_A_START, None])
    assert abs(starts[1] - S3_B_START) < 0.01  # 01:51:53.501 by tick vs 01:51:53.503 from the OBS log
    with pytest.raises(ValueError):
        ylv.join_part_rows([a, b], [None, S3_B_START])  # the first part needs its start
    with pytest.raises(ValueError):
        ylv.join_part_rows([b, a], [S3_B_START, None])  # tick backwards: cannot place by tick


def test_every_session3_window_is_clean(s3):
    rows, t0, vod = s3
    for name, a, b in S3_WINDOWS:
        r = ylv.dupskip(rows, vod, t0, a, b)
        assert (r["dup"], r["skip"]) == (0, 0), (name, r)


# ---------------------------------------------------------------- Review Focus 3: the clamp

def test_window_starting_before_the_vod_clamps_and_reports(s3):
    rows, t0, vod = s3
    name, a, b = S3_WINDOWS[0]
    r = ylv.dupskip(rows, vod, t0, a, b)
    assert r["clamped_start_utc"] is not None
    # the VOD opens with a 55 s QR-less, silent pre-roll; its first content frame shows 01:41:10.0
    assert abs(r["clamped_start_utc"] - hms(1, 41, 10.0)) < 0.1
    assert r["clamped_end_utc"] is None
    assert (r["dup"], r["skip"]) == (0, 0)
    cov = ylv.coverage(rows, vod, t0, r["start_utc"], r["end_utc"])
    assert cov["rec_cadence_pct"] >= 90 and cov["vod_cadence_pct"] >= 90


def test_a_window_outside_the_vod_is_an_error_not_a_crash(s3):
    rows, t0, vod = s3
    r = ylv.dupskip(rows, vod, t0, hms(1, 40, 20), hms(1, 41, 0))  # entirely before the VOD
    assert "error" in r and r["dup"] is None


# ---------------------------------------------------------------- Review Focus 5: both halves

def test_both_halves_decode_keeps_session3_part1_cadence_above_90_percent():
    a = ylv.load_ticks(FIX / "s3-rec_a-ticks.tsv.gz")
    _, wa, wb = S3_WINDOWS[0]
    win = [r for r in a if wa <= S3_A_START + r[1] < wb]
    c = ylv.continuity(win)
    assert 100.0 * c["cadence_proven"] / c["frames"] >= 90.0
    # frames 1565..4124 (52-137 s): the session's left-only gray decoder read NONE of them (its one
    # 2561-frame decode gap, rec_a-ticks.err); the colour-coded left now reads through blue
    gap = [r for r in a if 1565 <= r[0] <= 4124]
    assert len(gap) == 2560 and sum(r[2] is not None for r in gap) >= 0.9 * len(gap)


# BGR module colours of EQUAL gray (226): gray sees no QR at all, the blue channel sees 0 vs 255.
# A stand-in for the mid-transition colour pattern the camera captures on the half just repainted.
COLOUR_DARK, COLOUR_LIGHT = (0, 255, 255), (255, 255, 158)


def _qr_frame(left, right, w=960, h=540, size=280, left_colours=((0, 0, 0), (255, 255, 255))):
    """The painter's two QRs on white; `left_colours` = (dark, light) module colours of the left QR."""
    import cv2

    enc = cv2.QRCodeEncoder.create()
    f = np.full((h, w, 3), 255, np.uint8)
    for k, text in enumerate((left, right)):
        if text is None:
            continue
        q = cv2.resize(enc.encode(text), (size, size), interpolation=cv2.INTER_NEAREST)
        x0 = (w // 2) * k + (w // 2 - size) // 2
        dark, light = left_colours if k == 0 else ((0, 0, 0), (255, 255, 255))
        block = np.empty((size, size, 3), np.uint8)
        block[:] = light
        block[q < 128] = dark
        f[20:20 + size, x0:x0 + size] = block
    return f


def test_half_ticks_reads_both_halves_and_a_colour_half_through_blue():
    import cv2

    det = cv2.QRCodeDetector()
    assert ylv.half_ticks(_qr_frame("P123456.1000.17.42", "P123456.1001.17.42"), det) == (1000, 1001)
    assert ylv.half_ticks(_qr_frame(None, "P123456.1001.17.42"), det) == (None, 1001)
    # a node burn is never the painter tick
    assert ylv.half_ticks(_qr_frame("P911001.5.17.42", "P123456.1003.17.42"), det) == (None, 1003)
    assert ylv.half_ticks(_qr_frame(None, None), det) == (None, None)
    colour = _qr_frame("P123456.1000.17.42", "P123456.999.17.42", left_colours=(COLOUR_DARK, COLOUR_LIGHT))
    gray = cv2.cvtColor(colour[0:335, :480], cv2.COLOR_BGR2GRAY)
    assert ylv._qr_tick(det, gray, ylv.DECODE_SCALE) is None  # gray alone cannot read it ...
    assert ylv.half_ticks(colour, det) == (1000, 999)  # ... its blue channel can


def test_a_right_only_frame_takes_the_local_capture_phase():
    # odd phase (right = left + 1): right-only 1005 -> 1004; even phase (right = left - 1): 1005 -> 1006
    odd = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1003), (2, 0.067, 1004, 1005)]
    even = [(0, 0.0, 1000, 999), (1, 0.033, None, 1001), (2, 0.067, 1004, 1003)]
    assert [r[2:] for r in ylv.resolve_ticks(odd)] == [(1000, "B"), (1002, "R"), (1004, "B")]
    assert [r[2:] for r in ylv.resolve_ticks(even)] == [(1000, "B"), (1002, "R"), (1004, "B")]
    # a repeated frame stays a repeat (the phase does not come from the frame's own cadence)
    dup = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1001), (2, 0.067, 1002, 1003)]
    assert [r[2] for r in ylv.resolve_ticks(dup)] == [1000, 1000, 1002]
    # a phase step between the two sides, or no both-halves frame near: no tick, never a guess
    step = [(0, 0.0, 1000, 1001), (1, 0.033, None, 1003), (2, 0.067, 1004, 1003)]
    assert ylv.resolve_ticks(step)[1][2:] == (None, "r")
    far = [(0, 0.0, 1000, 1001), (500, 16.7, None, 1999)]
    assert ylv.resolve_ticks(far)[1][2:] == (None, "r")
    # a pair that disagrees (|right - left| != 1) gives no phase; the left still counts
    assert ylv.resolve_ticks([(0, 0.0, 1000, 1007)])[0][2:] == (1000, "L")


def test_painter_tick_takes_the_lowest_painter_payload():
    assert ylv.painter_tick(["P123456.204.1.1", "P123456.202.1.1", "P911014.9.1.1", "junk"]) == 202
    assert ylv.painter_tick(["P911001.5.1.1"]) is None


# ---------------------------------------------------------------- criterion 3: publishes

def test_publish_gap_session3_restart_is_under_half_a_second(s3):
    rows, t0, vod = s3
    g = ylv.publish_gaps(rows, vod, t0, [hms(1, 52, 8.12), hms(1, 50, 7.60)])
    assert g[0]["judged"] and g[0]["gap_s"] <= 0.5 and abs(g[0]["gap_s"] - 0.08) < 0.02
    assert abs(g[0]["last_vod_frame_before_utc"] - hms(1, 51, 4.70)) < 0.02
    assert g[1]["judged"] and g[1]["gap_s"] <= 0.5


def test_baseline_fresh_publish_is_a_late_join(base):
    rec, vod = base
    g = ylv.publish_gaps(rec, vod, BASE_T0, [hms(20, 34, 32.7)])[0]
    assert g["judged"] and g["gap_s"] > 3.5  # first VOD frame 20:34:36.67 (subscribe 5.49 s late)
    v = ylv.verdict([], [g])
    assert any("late" in r for r in v["reasons"])


def test_a_publish_before_the_vod_start_is_reported_not_judged(s2):
    rec, vod = s2
    g = ylv.publish_gaps(rec, vod, S2_T0, [hms(23, 22, 34.28)])[0]  # the first StartStream
    assert g["judged"] is False and g["gap_s"] > 20  # YouTube started the VOD at 23:22:58
    assert g["last_vod_frame_before_utc"] is None


# ---------------------------------------------------------------- criterion 4: audio

def test_audio_clean_window_passes_and_a_cut_block_is_a_lag_jump():
    rec, vod, _ = clips("s2-R")
    ok = ylv.audio_blocks(rec, vod)
    assert ok["lag_jumps"] == 0 and ok["silent"] == 0 and ok["level_drops"] == 0 and ok["low_corr"] == 0
    assert ok["blocks"] >= 60 and ok["corr_median"] > 0.95
    assert abs(ok["lag_vs_video_ms"] - (-5.9)) < 2.0  # session 2 R: VOD audio vs its video, 6014624254
    cut = ylv.drop_samples(vod, at_s=10.0, ms=23)  # one lost AAC frame
    bad = ylv.audio_blocks(rec, cut)
    assert bad["lag_jumps"] >= 1
    assert any(abs(ms + 23) < 2 for _, ms in bad["details"]["lag_jumps"])


def test_silence_and_a_level_drop_are_caught():
    rec, vod, _ = clips("s2-R")
    silent = vod.copy()
    silent[int(12.0 * ylv.SR): int(12.6 * ylv.SR)] = 0.0
    r = ylv.audio_blocks(rec, silent)
    assert r["silent"] >= 2
    quiet = vod.copy()
    quiet[int(14.0 * ylv.SR):] *= 10 ** (-15 / 20)
    q = ylv.audio_blocks(rec, quiet)
    assert q["level_drops"] >= 4 and q["lag_jumps"] == 0


def test_baseline_audio_is_continuous_but_1370_ms_late():
    rec, vod, _ = clips("base-R")
    r = ylv.audio_blocks(rec, vod)
    assert abs(r["lag_vs_video_ms"] - 1370.0) < 5.0  # +1370 ms, comment 6003342015 / audio tables
    assert (r["lag_jumps"], r["silent"], r["level_drops"]) == (0, 0, 0)


def test_audio_window_clamps_to_the_vod_start(s3):
    rows, t0, vod_rows = s3
    rec, vod, meta = clips("s3-A")
    _, a, _ = S3_WINDOWS[0]
    r = ylv.audio_window(rec, vod, rows, vod_rows, t0, a, a + 60.0,  # 01:40:40 .. 01:41:40
                         rec_pts0=meta["rec_pts0"], vod_pts0=meta["vod_pts0"])
    assert "error" not in r, r
    assert abs(r["start_utc"] - hms(1, 41, 10.0)) < 0.2  # starts at the VOD's first content frame
    assert (r["lag_jumps"], r["low_corr"], r["silent"], r["level_drops"]) == (0, 0, 0, 0)
    assert r["blocks"] >= 20


def test_ncc_best_finds_a_known_offset():
    rng = np.random.default_rng(7)
    seg = rng.standard_normal(20000).astype(np.float32)
    k, r = ylv.ncc_best(seg[5000:9000], seg)
    assert k == 5000 and r > 0.999


# ---------------------------------------------------------------- criterion 1: A/V

def test_avsync_outputs_parse_and_reproduce_the_session_deltas():
    expect = {"base": {"A": 41.7, "R": -1367.7, "B": -849.9}, "s2": {"A": -20.4, "R": 6.0, "B": -7.7},
              "s3": {"A": 5.1, "R": -4.2, "B": -1.6}}
    for sess, windows in expect.items():
        for w, delta in windows.items():
            assert av(sess, w)["delta_ms"] == pytest.approx(delta, abs=0.15), (sess, w)
    assert av("base", "R")["markers_vod"] == 69


def test_av_needs_enough_markers():
    j = {"av_offset_ms": 30.0, "matched": 10, "mad_ms": 5.0}
    with pytest.raises(ValueError):
        ylv.av_from_outputs(j, j)
    with pytest.raises(ValueError):
        ylv.parse_avsync_output("no json here\n")


# ---------------------------------------------------------------- the verdict

def _session_windows(rec, vod, t0, windows, sess, audio):
    out = []
    for name, a, b in windows:
        ds = ylv.dupskip(rec, vod, t0, a, b)
        out.append({"name": name, "dupskip": ds, "coverage": ylv.coverage(rec, vod, t0, ds["start_utc"], ds["end_utc"]),
                    "av": av(sess, name), "audio": audio[name]})
    return out


CLEAN = {"lag_jumps": 0, "low_corr": 0, "silent": 0, "level_drops": 0}


def test_baseline_session_fails(base):
    rec, vod = base
    audio = {"A": dict(CLEAN), "R": ylv.audio_blocks(*clips("base-R")[:2]), "B": dict(CLEAN)}
    w = _session_windows(rec, vod, BASE_T0, BASE_WINDOWS, "base", audio)
    pubs = ylv.publish_gaps(rec, vod, BASE_T0, [hms(20, 34, 32.7)])
    v = ylv.verdict(w, pubs)
    assert v["overall"] == "FAIL"
    assert v["criteria"]["av"] == "FAIL" and v["criteria"]["dupskip"] == "FAIL" and v["criteria"]["publish"] == "FAIL"
    assert any("R: A/V" in r and "-1367.7" in r for r in v["reasons"])


def test_session2_passes(s2):
    rec, vod = s2
    # audio: R measured from the clips; A and B are the session measurement (audio tables: 2019 / 1499
    # blocks, 0 lag jumps, 0 low-corr, 0 silent, 0 level drops)
    audio = {"A": dict(CLEAN), "R": ylv.audio_blocks(*clips("s2-R")[:2]), "B": dict(CLEAN)}
    w = _session_windows(rec, vod, S2_T0, S2_WINDOWS, "s2", audio)
    pubs = ylv.publish_gaps(rec, vod, S2_T0, [hms(23, 22, 34.28), hms(23, 32, 4.12), hms(23, 33, 7.42)])
    v = ylv.verdict(w, pubs)
    assert v["overall"] == "PASS", v["reasons"]


def test_session3_passes(s3):
    rows, t0, vod = s3
    rec_a, vod_a, meta = clips("s3-A")
    _, a, _ = S3_WINDOWS[0]
    audio = {"A": ylv.audio_window(rec_a, vod_a, rows, vod, t0, a, a + 60.0, meta["rec_pts0"], meta["vod_pts0"]),
             "R": dict(CLEAN), "B": dict(CLEAN)}
    w = _session_windows(rows, vod, t0, S3_WINDOWS, "s3", audio)
    pubs = ylv.publish_gaps(rows, vod, t0, [hms(1, 40, 27.93), hms(1, 50, 7.60), hms(1, 52, 8.12)])
    v = ylv.verdict(w, pubs)
    assert v["overall"] == "PASS", v["reasons"]


def test_verdict_unknown_when_coverage_below_90_percent():
    w = {"coverage": {"rec_cadence_pct": 83.5, "vod_cadence_pct": 96.3}, "av": {"delta_ms": 0},
         "dupskip": {"dup": 0, "skip": 0}, "audio": dict(CLEAN)}
    assert ylv.verdict([w], [])["overall"] == "UNKNOWN"


def test_verdict_fail_wins_over_unknown_and_nothing_measured_is_unknown():
    good = {"coverage": {"rec_cadence_pct": 99.0, "vod_cadence_pct": 99.0}, "av": {"delta_ms": 10.0},
            "dupskip": {"dup": 0, "skip": 0}, "audio": dict(CLEAN)}
    assert ylv.verdict([good], [])["overall"] == "PASS"
    assert ylv.verdict([], [])["overall"] == "UNKNOWN"
    unknown = dict(good, av={"error": "probe missing"})
    failing = dict(good, dupskip={"dup": 1, "skip": 0})
    assert ylv.verdict([unknown], [])["overall"] == "UNKNOWN"
    assert ylv.verdict([unknown, failing], [])["overall"] == "FAIL"
    # A/V is relative to the FIRST window: +140 ms off it passes, +160 ms fails
    assert ylv.verdict([good, dict(good, av={"delta_ms": 150.0})], [])["overall"] == "PASS"
    assert ylv.verdict([good, dict(good, av={"delta_ms": 170.0})], [])["overall"] == "FAIL"
    assert ylv.verdict([dict(good, covered_fraction=0.4)], [])["overall"] == "UNKNOWN"
    assert ylv.verdict([dict(good, errors=["the window spans two recording parts"])], [])["overall"] == "UNKNOWN"
    assert ylv.verdict([good], [{"utc": 1.0, "gap_s": None, "judged": True}])["overall"] == "FAIL"
    assert ylv.verdict([good], [{"utc": 1.0, "gap_s": 30.0, "judged": False}])["overall"] == "PASS"


# ---------------------------------------------------------------- CLI argument forms

def test_timestamps_windows_and_recordings_parse():
    t = ylv.parse_utc("2026-10-06T01:40:15.901Z")
    assert ylv.parse_utc("20261006T014015.901Z") == pytest.approx(t)
    assert ylv.parse_utc(f"{t:.3f}") == pytest.approx(t)
    assert ylv.parse_window("A:2026-10-06T01:40:40Z:2026-10-06T01:49:28Z") == ("A", pytest.approx(t + 24.099),
                                                                                pytest.approx(t + 552.099))
    assert ylv.parse_window(f"B:{t}:{t + 10}") == ("B", t, t + 10)
    with pytest.raises(ValueError):
        ylv.parse_window("C:2026-10-06T01:49:28Z:2026-10-06T01:40:40Z")  # end before start
    with pytest.raises(ValueError):
        ylv.parse_utc("2026-10-06T01:40:15")  # no time zone
    assert ylv.parse_recording("/r/2026-10-06 03-40-15.mp4@2026-10-06T01:40:15.901Z") == (
        "/r/2026-10-06 03-40-15.mp4", pytest.approx(t))
    assert ylv.parse_recording("/r/part2.mp4") == ("/r/part2.mp4", None)


# ---------------------------------------------------------------- CLI end to end (synthetic)

FPS = 30
SECONDS = 8
T0 = 1791250000.0  # 2026-10-06 ~ (epoch); the recording's start


def _write_video(path, ticks, right_only=(), colour_left=(), even_phase=False):
    """A lossless (FFV1, RGB) 960x540 30 fps clip with the painter's two QRs (left = even tick,
    right = tick + 1, or tick - 1 when captured on even painter ticks) and a deterministic
    pink-noise audio track (identical in every generated file). Frames in `right_only` have no left
    QR, frames in `colour_left` a left QR only the blue channel reads."""
    ff = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24",
                           "-s", "960x540", "-r", str(FPS), "-i", "-", "-f", "lavfi", "-i",
                           f"anoisesrc=d={SECONDS + 1}:c=pink:r=48000:a=0.05:seed=7", "-map", "0:v:0",
                           "-map", "1:a:0", "-c:v", "ffv1", "-pix_fmt", "bgr0", "-c:a", "aac", "-b:a", "192k",
                           "-shortest", str(path)], stdin=subprocess.PIPE)
    for k, t in enumerate(ticks):
        left = None if k in right_only else f"P123456.{t}.17.42"
        colours = (COLOUR_DARK, COLOUR_LIGHT) if k in colour_left else ((0, 0, 0), (255, 255, 255))
        right = f"P123456.{t - 1 if even_phase else t + 1}.17.42"
        ff.stdin.write(_qr_frame(left, right, left_colours=colours).tobytes())
    ff.stdin.close()
    assert ff.wait() == 0


FAKE_PROBE = r'''#!/usr/bin/env python3
import json, sys
clip = sys.argv[sys.argv.index("--av-sync") + 1]
off = 30.0 if "av-rec-" in clip else 25.0
print("\x1b[2mlog line before the JSON\x1b[0m", file=sys.stderr)
print("{\n" + json.dumps({"av_offset_ms": off, "mad_ms": 4.0, "matched": 12, "audio_markers_decoded": 12,
                          "video_ticks": 180}, indent=2)[1:], file=sys.stderr)
print("A/V-sync offset measured", file=sys.stderr)
'''


@pytest.fixture(scope="module")
def synth(tmp_path_factory):
    d = tmp_path_factory.mktemp("ylv-synth")
    ticks = [1000 + 2 * k for k in range(FPS * SECONDS)]
    # the recording: captured on EVEN painter ticks, a 30-frame colour-coded-left stretch (read
    # through blue) and a 30-frame no-left stretch (a fixed "right - 1" would read it 2 ticks low)
    _write_video(d / "rec.mkv", ticks, right_only=set(range(120, 150)), colour_left=set(range(60, 90)),
                 even_phase=True)
    _write_video(d / "vod.mkv", ticks)
    _write_video(d / "vod-skip.mkv", ticks[:100] + ticks[101:])  # YouTube lost one frame
    probe = d / "fake-recording-verdict"
    probe.write_text(FAKE_PROBE)
    probe.chmod(0o755)
    (d / "markers.csv").write_text("index,frame_id,emit_ts_ns\n")
    return d


def test_decode_ticks_reads_colour_and_right_only_stretches(synth):
    rows = ylv.decode_ticks(synth / "rec.mkv", workers=1)
    assert len(rows) == FPS * SECONDS
    # a detector miss leaves a frame undecoded; a decoded frame is never wrong (the phase rule)
    want = [1000 + 2 * k for k in range(FPS * SECONDS)]
    assert all(r[2] in (None, w) for r, w in zip(rows, want))
    assert sum(r[2] is not None for r in rows) >= 0.95 * len(rows)
    colour, right_only = rows[60:90], rows[120:150]
    assert sum(r[3] == "B" for r in colour) >= 27  # the colour-coded left read through blue
    assert sum(r[3] == "R" for r in right_only) >= 27  # right only, captured on even ticks: right + 1
    assert {r[3] for r in right_only} <= {"R", ""}
    assert rows[30][1] - rows[0][1] == pytest.approx(1.0, abs=0.002)  # pts: the container's timeline
    c = ylv.continuity([r[:3] for r in rows])
    assert c["cadence_proven"] == len(rows) and c["events"] == 0


def _cli(synth, out, vod, probe=None):
    cmd = [sys.executable, str(SCRIPT), "--vod", str(synth / vod), "--recording", f"{synth / 'rec.mkv'}@{T0}",
           "--markers", str(synth / "markers.csv"), "--windows", f"W1:{T0}:{T0 + SECONDS}", "--out", str(out),
           "--probe-bin", str(probe or synth / "fake-recording-verdict"), "--workers", "2"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    return r, json.loads((out / "youtube-leg-verdict.json").read_text())


def test_cli_pass_fail_and_unknown_exit_codes(synth, tmp_path):
    out = tmp_path / "out"
    r, j = _cli(synth, out, "vod.mkv")
    assert r.returncode == 0, (r.stdout, r.stderr, j["reasons"])
    assert j["schema"] == 1 and j["overall"] == "PASS"
    w = j["windows"][0]
    assert w["name"] == "W1" and (w["dupskip"]["dup"], w["dupskip"]["skip"]) == (0, 0)
    assert w["av"]["delta_ms"] == -5.0 and w["coverage"]["rec_cadence_pct"] == 100.0
    assert w["audio"]["lag_jumps"] == 0 and w["audio"]["blocks"] >= 4
    assert not list(out.glob("av-*.mp4")) and len(list(out.glob("av-*.avsync.out"))) == 2
    assert (out / "ticks-rec-1.tsv").exists() and (out / "ticks-vod.tsv").exists()

    r, j = _cli(synth, out, "vod.mkv", probe=synth / "no-such-recording-verdict")  # cached ticks reused
    assert r.returncode == 2 and j["overall"] == "UNKNOWN"
    assert any("A/V not measured" in x for x in j["reasons"])

    r, j = _cli(synth, out, "vod-skip.mkv")
    assert r.returncode == 1 and j["overall"] == "FAIL"
    assert j["windows"][0]["dupskip"]["skip"] == 1


def test_cli_tool_error_is_unknown_and_written(tmp_path):
    out = tmp_path / "out"
    r = subprocess.run([sys.executable, str(SCRIPT), "--vod", "not a file, not an id", "--recording", f"x.mp4@{T0}",
                        "--markers", "m.csv", "--windows", f"W1:{T0}:{T0 + 5}", "--out", str(out)],
                       capture_output=True, text=True)
    assert r.returncode == 2
    j = json.loads((out / "youtube-leg-verdict.json").read_text())
    assert j["overall"] == "UNKNOWN" and j["reasons"][0].startswith("tool error:")
    usage = subprocess.run([sys.executable, str(SCRIPT), "--out", str(out)], capture_output=True, text=True)
    assert usage.returncode == 2
