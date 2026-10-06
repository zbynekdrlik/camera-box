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

Tick maps are the per-frame maps cut to the windows (indices and pts kept); audio is 20 s 16 kHz mono
FLAC cut at tick-matched positions (audio-clips.json = each clip's start pts); *.avsync.out are the
saved `recording-verdict --av-sync` outputs. Every fail-open path the review found has a test here
(a VOD or its audio ending early, rows missing, a replay, a repeat behind an undecodable frame, a
painter restart, a hung probe, a crash). The decoder itself: test_youtube_leg_ticks_1404.py.
Tier-0: pytest + ffmpeg, no cargo, no rig, no network.
"""
import json
import pathlib
import subprocess
import sys

import numpy as np
import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from youtube_leg_fakes_1404 import FIX, FPS, SCRIPT, SECONDS, write_video, ylv  # noqa: E402


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
CLEAN = {"lag_jumps": 0, "low_corr": 0, "silent": 0, "level_drops": 0}
GOOD = {"coverage": {"rec_cadence_pct": 99.0, "vod_cadence_pct": 99.0}, "av": {"delta_ms": 10.0},
        "dupskip": {"dup": 0, "skip": 0, "unjudged": 0, "vod_frames": 1000}, "audio": dict(CLEAN)}


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


def steady(n, first=100, start=0.0):
    """n recording-like rows at 30 fps with the 2-tick cadence."""
    return [(i, start + i / 30.0, first + 2 * i) for i in range(n)]


def vod_of(ticks):
    return [(k, k / 30.0, t) for k, t in enumerate(ticks)]


# ---------------------------------------------------------------- criterion 2: dup/skip on real sessions

def test_session2_steady_windows_have_zero_downstream_dupskip(s2):
    rec, vod = s2
    frames = {"A": 14754, "R": 1309, "B": 10854}  # decoded recording frames, comment 6006986090
    for name, a, b in S2_WINDOWS:
        r = ylv.dupskip(rec, vod, S2_T0, a, b)
        assert (r["dup"], r["skip"]) == (0, 0), (name, r)
        assert r["unjudged"] <= 5 and r["rec_frames"] == frames[name]
        assert r["clamped_start_utc"] is None and r["clamped_end_utc"] is None


def test_baseline_dupskip_reproduces_the_session_counts(base):
    rec, vod = base
    counts = {name: ylv.dupskip(rec, vod, BASE_T0, a, b) for name, a, b in BASE_WINDOWS}
    # The session tool counted A 10/10, R 46/47, B 2/2 between ADJACENT decoded VOD frames. The tool
    # also judges pairs around an undecodable VOD frame by recording frame count (A: two more skips,
    # e.g. VOD 1058514, 1058514, ?, 1058520 against recording 1058514, ?, 1058518, 1058520) and the
    # pair straddling R's end (the VOD jumped over the window's last recording frame, tick 1090908).
    assert {k: (c["dup"], c["skip"]) for k, c in counts.items()} == {"A": (10, 12), "R": (48, 51), "B": (2, 2)}
    shorter = ylv.dupskip(rec, vod, BASE_T0, BASE_WINDOWS[1][1], BASE_WINDOWS[1][2] - 1.0 / 30)
    assert (shorter["dup"], shorter["skip"]) == (48, 50)  # without tick 1090908: exactly that skip less


def test_a_vod_jump_over_the_window_edge_counts():
    rec = steady(11)  # ticks 100..120
    r = ylv.dupskip(rec, vod_of([100, 102, 104, 106, 108, 110, 112, 118, 120]), 0.0, 0.0, 8.5 / 30.0)
    assert (r["dup"], r["skip"]) == (0, 2)  # 114 and 116 never shown; the window holds ticks 100..116
    r = ylv.dupskip(rec, vod_of([100, 102, 104, 104, 106, 108, 110, 112, 114, 116, 118, 120]), 0.0, 0.0, 8.5 / 30.0)
    assert (r["dup"], r["skip"]) == (1, 0)


def test_a_tick_the_rig_repeated_cancels_out():
    rec = [(0, 0.0, 100), (1, 1 / 30, 102), (2, 2 / 30, 102), (3, 3 / 30, 106), (4, 4 / 30, 108)]
    r = ylv.dupskip(rec, list(rec), 0.0, 0.0, 5 / 30)
    assert (r["dup"], r["skip"]) == (0, 0)


def test_a_vod_that_replays_content_is_counted():
    rec = steady(41)  # ticks 100..180
    replay = vod_of(list(range(100, 162, 2)) + list(range(100, 182, 2)))  # 30 frames shown again
    r = ylv.dupskip(rec, replay, 0.0, 0.0, 41 / 30)
    assert r["dup"] >= 30 and r["skip"] == 0
    assert ylv.verdict([dict(GOOD, dupskip=r)], [])["overall"] == "FAIL"


def test_dup_and_skip_hidden_behind_an_undecodable_vod_frame_are_counted():
    rec = steady(21)  # ticks 100..140
    hidden_dup = vod_of([100, 102, 104, None, 106, 108])  # the undecodable frame repeated 104
    assert (ylv.dupskip(rec, hidden_dup, 0.0, 0.0, 6 / 30)["dup"]) == 1
    hidden_skip = vod_of([100, 102, None, 108, 110])  # one of 104 / 106 never reached the VOD
    assert (ylv.dupskip(rec, hidden_skip, 0.0, 0.0, 6 / 30)["skip"]) == 1
    clean = vod_of([100, 102, None, 106, 108])
    r = ylv.dupskip(rec, clean, 0.0, 0.0, 5 / 30)
    assert (r["dup"], r["skip"], r["unjudged"]) == (0, 0, 0)


def test_a_painter_restart_inside_the_window_is_unknown_and_its_ticks_never_alias():
    restarted = steady(30, first=2_000_000) + [(30 + i, (30 + i) / 30.0, 2 * i) for i in range(30)]
    r = ylv.dupskip(restarted, list(restarted), 0.0, 0.0, 2.0)
    assert "error" in r and "restarted" in r["error"]
    # a tick the recording shows twice, a minute apart (two painter runs), maps to no content time
    twice = steady(5) + [(5000 + i, 100.0 + i / 30.0, 100 + 2 * i) for i in range(5)]
    assert ylv.TickClock(twice, 0.0).time_of(104) is None
    assert ylv.TickClock(steady(5), 0.0).time_of(104) == pytest.approx(2 / 30.0)


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
    assert (joined["dup"], joined["skip"]) == (0, 0)


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
        assert (r["dup"], r["skip"]) == (0, 0) and r["unjudged"] <= 5, (name, r)


# ---------------------------------------------------------------- Review Focus 3: the clamp

def test_window_starting_before_the_vod_clamps_and_reports(s3):
    rows, t0, vod = s3
    name, a, b = S3_WINDOWS[0]
    r = ylv.dupskip(rows, vod, t0, a, b)
    # the VOD opens with a 55 s QR-less, silent pre-roll; its first content frame shows 01:41:10.0
    assert r["clamped_start_utc"] is not None and abs(r["clamped_start_utc"] - hms(1, 41, 10.0)) < 0.1
    assert r["clamped_end_utc"] is None and r["vod_ends_early_s"] is None
    assert (r["dup"], r["skip"]) == (0, 0)
    cov = ylv.coverage(rows, vod, t0, r["start_utc"], r["end_utc"])
    assert cov["rec_cadence_pct"] >= 90 and cov["vod_cadence_pct"] >= 90


def test_a_window_outside_the_vod_or_the_recording_is_an_error_not_a_crash(s3):
    rows, t0, vod = s3
    r = ylv.dupskip(rows, vod, t0, hms(1, 40, 20), hms(1, 41, 0))  # entirely before the VOD
    assert "error" in r and r["dup"] is None
    r = ylv.dupskip(rows, vod, t0, hms(1, 39, 0), hms(1, 41, 30))  # starts before the recording
    assert "error" in r and "not inside the recording" in r["error"]


def test_a_vod_that_ends_before_the_window_end_fails(s3):
    rows, t0, vod = s3
    _, a, b = S3_WINDOWS[2]
    clock = ylv.TickClock(rows, t0)
    cut = next(k for k, v in enumerate(vod) if v[2] is not None and (clock.time_of(v[2]) or 0) > b - 120)
    r = ylv.dupskip(rows, vod[:cut], t0, a, b)
    assert (r["dup"], r["skip"]) == (0, 0) and r["vod_ends_early_s"] == pytest.approx(120, abs=1)
    v = ylv.verdict([dict(GOOD, dupskip=r)], [])
    assert v["overall"] == "FAIL" and any("VOD ends" in x for x in v["reasons"])


def test_rows_missing_from_a_map_lower_the_coverage(s3):
    rows, t0, vod = s3
    _, a, b = S3_WINDOWS[2]
    full = ylv.dupskip(rows, vod, t0, a, b)
    clock = ylv.TickClock(rows, t0)
    first = next(k for k, v in enumerate(vod) if v[2] is not None and (clock.time_of(v[2]) or 0) > a + 60)
    holed = vod[:first] + vod[first + 3000:]  # 100 s of decoded rows gone from the VOD map
    cov = ylv.coverage(rows, holed, t0, full["start_utc"], full["end_utc"])
    assert cov["vod_cadence_pct"] < 90
    v = ylv.verdict([dict(GOOD, coverage=cov)], [])
    assert v["overall"] == "UNKNOWN"


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
    assert ok["blocks"] == ok["expected_blocks"] >= 60 and ok["reliable_blocks"] == ok["signal_blocks"]
    assert ok["vod_ends_early_s"] == 0 and ok["corr_median"] > 0.95
    assert abs(ok["lag_vs_video_ms"] - (-5.9)) < 2.0  # session 2 R: VOD audio vs its video, 6014624254
    cut = ylv.drop_samples(vod, at_s=10.0, ms=23)  # one lost AAC frame
    bad = ylv.audio_blocks(rec, cut)
    assert bad["lag_jumps"] >= 1
    assert any(abs(ms + 23) < 2 for _, ms in bad["details"]["lag_jumps"])


def test_silence_and_a_level_drop_are_caught():
    rec, vod, _ = clips("s2-R")
    silent = vod.copy()
    silent[int(12.0 * ylv.SR): int(12.6 * ylv.SR)] = 0.0
    assert ylv.audio_blocks(rec, silent)["silent"] >= 2
    quiet = vod.copy()
    quiet[int(14.0 * ylv.SR):] *= 10 ** (-15 / 20)
    q = ylv.audio_blocks(rec, quiet)
    assert q["level_drops"] >= 4 and q["lag_jumps"] == 0


def test_vod_audio_ending_early_fails_and_unusable_recording_audio_is_unknown():
    rec, vod, _ = clips("s2-R")
    short = ylv.audio_blocks(rec, vod[: 8 * ylv.SR])  # the VOD audio stops after 8 of 20 s
    assert short["vod_ends_early_s"] > 10
    v = ylv.verdict([dict(GOOD, audio=short)], [])
    assert v["overall"] == "FAIL" and any("VOD audio ends" in x for x in v["reasons"])
    faint = ylv.audio_blocks(rec * 10 ** (-60 / 20), vod)  # the recording carries no usable signal
    assert faint["signal_blocks"] == 0
    assert ylv.verdict([dict(GOOD, audio=faint)], [])["overall"] == "UNKNOWN"


def test_baseline_audio_is_continuous_but_1370_ms_late():
    rec, vod, _ = clips("base-R")
    r = ylv.audio_blocks(rec, vod, end_s=18.0)  # the VOD clip lacks the last 1.37 s of this content
    assert abs(r["lag_vs_video_ms"] - 1370.0) < 5.0  # +1370 ms, comment 6003342015 / audio tables
    assert (r["lag_jumps"], r["silent"], r["level_drops"], r["vod_ends_early_s"]) == (0, 0, 0, 0)


def test_audio_window_clamps_to_the_vod_start(s3):
    rows, t0, vod_rows = s3
    rec, vod, meta = clips("s3-A")
    _, a, _ = S3_WINDOWS[0]
    r = ylv.audio_window(rec, vod, rows, vod_rows, t0, a, a + 42.0,  # 01:40:40 .. 01:41:22, inside the clips
                         rec_pts0=meta["rec_pts0"], vod_pts0=meta["vod_pts0"])
    assert "error" not in r, r
    assert abs(r["start_utc"] - hms(1, 41, 10.0)) < 0.2  # starts at the VOD's first content frame
    assert (r["lag_jumps"], r["low_corr"], r["silent"], r["level_drops"]) == (0, 0, 0, 0)
    assert r["signal_blocks"] >= 20 and r["reliable_blocks"] == r["signal_blocks"]


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


def test_baseline_session_fails(base):
    rec, vod = base
    r_rec, r_vod, _ = clips("base-R")
    audio = {"A": dict(CLEAN), "R": ylv.audio_blocks(r_rec, r_vod, end_s=18.0), "B": dict(CLEAN)}
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
    audio = {"A": ylv.audio_window(rec_a, vod_a, rows, vod, t0, a, a + 42.0, meta["rec_pts0"], meta["vod_pts0"]),
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
    good = GOOD
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
    many = dict(good, dupskip={"dup": 0, "skip": 0, "unjudged": 12, "vod_frames": 1000})
    assert ylv.verdict([many], [])["overall"] == "UNKNOWN"  # too many VOD pairs nobody could judge


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

T0 = 1791250000.0  # 2026-10-06 ~ (epoch); the recording's start

FAKE_PROBE = r'''#!/usr/bin/env python3
import json, sys, time
MODE = "@MODE@"
clip = sys.argv[sys.argv.index("--av-sync") + 1]
if MODE == "SLEEP":
    time.sleep(30)
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
    # the recording: captured on EVEN painter ticks with a colour-coded-left and a no-left stretch
    write_video(d / "rec.mkv", ticks, right_only=set(range(120, 150)), colour_left=set(range(60, 90)), even_phase=True)
    write_video(d / "vod.mkv", ticks)
    write_video(d / "vod-skip.mkv", ticks[:100] + ticks[101:])  # YouTube lost one frame
    for name, mode in (("fake-recording-verdict", "FAST"), ("sleepy-recording-verdict", "SLEEP")):
        probe = d / name
        probe.write_text(FAKE_PROBE.replace("@MODE@", mode))
        probe.chmod(0o755)
    (d / "markers.csv").write_text("index,frame_id,emit_ts_ns\n")
    return d


def _cli(synth, out, vod, probe=None, extra=()):
    cmd = [sys.executable, str(SCRIPT), "--vod", str(synth / vod), "--recording", f"{synth / 'rec.mkv'}@{T0}",
           "--markers", str(synth / "markers.csv"), "--windows", f"W1:{T0}:{T0 + SECONDS}", "--out", str(out),
           "--probe-bin", str(probe or synth / "fake-recording-verdict"), "--workers", "2", *extra]
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

    r, j = _cli(synth, out, "vod.mkv", extra=["--publish", str(T0 + 3)])  # a publish inside W1
    assert r.returncode == 2 and any("inside the window" in x for x in j["reasons"])

    r, j = _cli(synth, out, "vod-skip.mkv")
    assert r.returncode == 1 and j["overall"] == "FAIL"
    assert j["windows"][0]["dupskip"]["skip"] == 1


def test_a_hung_probe_times_out_and_leaves_no_clip(synth, tmp_path, monkeypatch):
    monkeypatch.setattr(ylv, "PROBE_TIMEOUT_S", 2)
    with pytest.raises(subprocess.TimeoutExpired):
        ylv.av_window(synth / "rec.mkv", synth / "vod.mkv", 0.0, 0.0, synth / "markers.csv",
                      synth / "sleepy-recording-verdict", 2.0, str(tmp_path))
    assert not list(tmp_path.glob("*.mp4"))


def test_cli_tool_error_and_a_crash_are_unknown(tmp_path):
    out = tmp_path / "out"
    r = subprocess.run([sys.executable, str(SCRIPT), "--vod", "not a file, not an id", "--recording", f"x.mp4@{T0}",
                        "--markers", "m.csv", "--windows", f"W1:{T0}:{T0 + 5}", "--out", str(out)],
                       capture_output=True, text=True)
    assert r.returncode == 2
    j = json.loads((out / "youtube-leg-verdict.json").read_text())
    assert j["overall"] == "UNKNOWN" and j["reasons"][0].startswith("tool error:")
    usage = subprocess.run([sys.executable, str(SCRIPT), "--out", str(out)], capture_output=True, text=True)
    assert usage.returncode == 2
    blocked = tmp_path / "a-file"
    blocked.write_text("not a directory")  # the verdict cannot even be written: still 2, never FAIL's 1
    crash = subprocess.run([sys.executable, str(SCRIPT), "--vod", "x", "--recording", f"x.mp4@{T0}", "--markers", "m",
                            "--windows", f"W1:{T0}:{T0 + 5}", "--out", str(blocked)], capture_output=True, text=True)
    assert crash.returncode == 2
