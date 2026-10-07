"""Issue 1404 Task 5 part b -- the run-scoped YouTube-leg timeline (design comment 6048239795).

The measurement clip (run 911016, scripts/gen_measurement_clip.py) plays in the CG segments beside the
cam2 painter's tick line and restarts its tick on every play. A decode with `runs=CLIP_RUNS` keeps each
frame's run; the timeline splits a session into run segments (a seam at every run change), gives each
its own TickClock and resolves every VOD row to the segment it shows, so:
  - a camera window next to a CG segment reads no false dup (the one-line timeline read a replay of
    every window tick at the painter -> clip cut: 300 dups on the repro below);
  - a CG window is judged on its own tick line, also when the clip plays more than once, and on the
    committed fixture cut from the generated clip (tests/fixtures/measurement-clip-1404/clip-v1-4s.mp4)
    it reads 0 dup / 0 skip with every frame cadence-proven;
  - the default decode (no runs) is unchanged: DECODER_VERSION 2, 6 columns, the cache key without runs,
    legacy rows through the old code (restreamer's gate decodes without runs).
Tier-0: pytest + ffmpeg + OpenCV, no cargo.
"""
import json
import pathlib
import subprocess
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from youtube_leg_fakes_1404 import FIX, FPS, ROOT, SCRIPT, payload, qr_frame, ylv  # noqa: E402

CLIP = 911016
PAINTER = 123456789
CLIP_FIX = ROOT / "tests" / "fixtures" / "measurement-clip-1404" / "clip-v1-4s.mp4"
CLIP_TICKS = ROOT / "tests" / "fixtures" / "measurement-clip-1404" / "clip-v1-4s.ticks.tsv"


def painter_rows(first_frame, n, first_tick=100000):
    """n painter rows at 30 fps from frame `first_frame` (the 2-tick cadence of a 60 Hz painter)."""
    return [(first_frame + i, (first_frame + i) / 30.0, first_tick + 2 * (first_frame + i), PAINTER) for i in range(n)]


def clip_rows(first_frame, n, first_tick=0):
    """n rows of one clip play from frame `first_frame`: the clip's own tick line, from `first_tick`."""
    return [(first_frame + k, (first_frame + k) / 30.0, first_tick + 2 * k, CLIP) for k in range(n)]


def vod_of(rows, start=5.0):
    """A VOD that shows exactly the recording's frames (its own pts timeline)."""
    return [(k, start + k / 30.0) + tuple(r[2:]) for k, r in enumerate(rows)]


def legacy(rows):
    return [r[:3] for r in rows]


# a session: camera 20 s, clip 10 s, camera 10 s, the clip again 10 s, camera 10 s
SESSION = painter_rows(0, 600) + clip_rows(600, 300) + painter_rows(900, 300) + clip_rows(1200, 300) + \
    painter_rows(1500, 300)
CAMERA_W, CG1_W, CG2_W = (5.0, 15.0), (21.0, 29.0), (41.0, 49.0)


# ---------------------------------------------------------------- the false dups next to a CG segment

def test_a_camera_window_next_to_a_cg_segment_reads_no_false_dups():
    rec = painter_rows(0, 600) + clip_rows(600, 300)
    vod = vod_of(rec)
    # the one-tick-line timeline: the painter -> clip cut reads as a replay of every window tick
    old = ylv.dupskip(legacy(rec), legacy(vod), 0.0, *CAMERA_W)
    assert old["dup"] == 300, old
    r = ylv.dupskip(rec, vod, 0.0, *CAMERA_W)
    assert (r.get("error"), r["dup"], r["skip"], r["run"]) == (None, 0, 0, PAINTER), r
    assert r["unanchored_s"] == 0.0 and r["unjudged"] == 0
    cov = ylv.coverage(rec, vod, 0.0, *CAMERA_W)
    assert (cov["rec_cadence_pct"], cov["vod_cadence_pct"], cov["vod_events"]) == (100.0, 100.0, 0), cov


def test_every_window_of_a_session_with_two_clip_plays_is_judged_on_its_own_tick_line():
    vod = vod_of(SESSION)
    for name, (a, b), run in (("camera", CAMERA_W, PAINTER), ("cg1", CG1_W, CLIP), ("cg2", CG2_W, CLIP),
                              ("camera2", (31.0, 39.0), PAINTER), ("camera3", (51.0, 59.0), PAINTER)):
        r = ylv.dupskip(SESSION, vod, 0.0, a, b)
        assert (r.get("error"), r["dup"], r["skip"], r["run"]) == (None, 0, 0, run), (name, r)
        cov = ylv.coverage(SESSION, vod, 0.0, a, b)
        assert cov["rec_cadence_pct"] == cov["vod_cadence_pct"] == 100.0, (name, cov)
    tl = ylv.run_timeline(SESSION, vod, 0.0)
    assert [(s.run, s.k0, s.k1) for s in tl.segs] == [(PAINTER, 0, 600), (CLIP, 600, 900), (PAINTER, 900, 1200),
                                                      (CLIP, 1200, 1500), (PAINTER, 1500, 1800)]
    # its own TickClock per play: clip tick 0 is 20 s into the session in play 1, 40 s in play 2
    assert tl.clocks[1].time_of(0) == pytest.approx(20.0) and tl.clocks[3].time_of(0) == pytest.approx(40.0)
    assert all(j is not None for j in tl.vod_seg), "every VOD frame maps to the play it shows"
    assert [tl.vod_seg[k] for k in (700, 1300)] == [1, 3]


def test_a_vod_dup_in_one_clip_play_is_counted_in_that_play_only():
    vod = vod_of(SESSION)
    dup_at = 700  # the VOD repeats a frame of the FIRST play (and is one frame longer from there on)
    vod = vod[:dup_at + 1] + [(k + 1, v[1] + 1 / 30.0) + tuple(v[2:]) for k, v in enumerate(vod[dup_at:], dup_at)]
    first = ylv.dupskip(SESSION, vod, 0.0, *CG1_W)
    second = ylv.dupskip(SESSION, vod, 0.0, *CG2_W)
    assert (first["dup"], first["skip"]) == (1, 0), first
    assert (second.get("error"), second["dup"], second["skip"]) == (None, 0, 0), second
    for w in (CAMERA_W, (31.0, 39.0), (51.0, 59.0)):
        r = ylv.dupskip(SESSION, vod, 0.0, *w)
        assert (r["dup"], r["skip"]) == (0, 0), (w, r)


def test_a_vod_skip_inside_a_cg_window_fails_it():
    vod = vod_of(SESSION)
    lost = 1350  # the VOD lost one frame of the second play
    vod = vod[:lost] + [(k - 1, v[1] - 1 / 30.0) + tuple(v[2:]) for k, v in enumerate(vod[lost + 1:], lost + 1)]
    r = ylv.dupskip(SESSION, vod, 0.0, *CG2_W)
    assert (r["dup"], r["skip"], r["run"]) == (0, 1, CLIP), r


def test_a_window_holding_a_cut_or_a_restart_is_an_error_never_a_verdict():
    vod = vod_of(SESSION)
    cut = ylv.dupskip(SESSION, vod, 0.0, 15.0, 25.0)
    assert cut["dup"] is None and "cut from run 123456789 to run 911016" in cut["error"], cut
    # a 40 s clip played twice back to back (a loop: tick 2398 -> 0, more than RESTART_TICKS back)
    looped = painter_rows(0, 300) + clip_rows(300, 1200) + clip_rows(1500, 1200) + painter_rows(2700, 300)
    lvod = vod_of(looped)
    r = ylv.dupskip(looped, lvod, 0.0, 45.0, 55.0)  # the loop point is at 50 s
    assert r["dup"] is None and "restarted inside the window" in r["error"], r
    for w in ((20.0, 40.0), (60.0, 80.0)):  # each play alone is clean, on its own clock
        r = ylv.dupskip(looped, lvod, 0.0, *w)
        assert (r.get("error"), r["dup"], r["skip"], r["run"]) == (None, 0, 0, CLIP), (w, r)


def test_a_vod_that_starts_inside_the_second_play_maps_it_to_the_second_play():
    vod = vod_of(SESSION[1300:])  # YouTube's VOD starts at its own live transition, in play 2
    tl = ylv.run_timeline(SESSION, vod, 0.0)
    assert {tl.vod_seg[k] for k in range(200)} == {3}
    r = ylv.dupskip(SESSION, vod, 0.0, 44.0, 49.0)
    assert (r.get("error"), r["dup"], r["skip"]) == (None, 0, 0), r
    assert ylv.dupskip(SESSION, vod, 0.0, *CG1_W)["error"] == "the VOD has no frame of this window"


def test_plays_the_content_order_cannot_pin_stay_unmapped():
    # the recording holds two plays back to back between the same camera stretches, the VOD only one:
    # which play the VOD shows cannot be told, so neither window gets a verdict from it
    rec = painter_rows(0, 300) + clip_rows(300, 1200) + clip_rows(1500, 1200) + painter_rows(2700, 300)
    vod = vod_of(painter_rows(0, 300) + clip_rows(300, 1200) + painter_rows(2700, 300))
    tl = ylv.run_timeline(rec, vod, 0.0)
    assert [s.run for s in tl.segs] == [PAINTER, CLIP, CLIP, PAINTER] and tl.segs[2].restart
    assert all(tl.vod_seg[k] is None for k in range(300, 1500))
    assert tl.vod_seg[0] == 0 and tl.vod_seg[1500] == 3, "the camera stretches still map"
    for w in ((20.0, 40.0), (60.0, 80.0)):
        r = ylv.dupskip(rec, vod, 0.0, *w)
        assert r["dup"] is None and r["error"], (w, r)


def test_coverage_counts_undecodable_frames_at_a_cut_as_unproven_never_as_an_event():
    rec = painter_rows(0, 600) + clip_rows(600, 300)
    rec = [r if not 590 <= r[0] < 600 else (r[0], r[1], None, None) for r in rec]  # a fade nobody decodes
    vod = vod_of(rec)
    cov = ylv.coverage(rec, vod, 0.0, 15.0, 25.0)
    assert cov["rec_events"] == 0 and cov["rec_frames"] == 300, cov
    # 290 of 300 proven: each run's own decoded frames, the 10 fade frames proven by no run
    assert cov["rec_cadence_pct"] == pytest.approx(100.0 * 290 / 300, abs=0.05), cov
    # a window that starts inside the fade: its undecoded frames show no run's tick, so the window
    # is the clip's (the fade still counts in its coverage as frames nobody proved)
    r = ylv.dupskip(rec, vod, 0.0, 19.75, 25.0)
    assert (r.get("error"), r["dup"], r["skip"], r["run"]) == (None, 0, 0, CLIP), r
    assert ylv.coverage(rec, vod, 0.0, 19.75, 25.0)["rec_cadence_pct"] < 100.0


def test_publish_joins_and_av_starts_read_the_run_segments():
    vod = vod_of(SESSION)
    pubs = ylv.publish_gaps(SESSION, vod, 0.0, [25.0])
    assert pubs[0]["gap_s"] == 0.0 and pubs[0]["judged"], pubs
    rec_p, vod_p = ylv.vod_pts_for(SESSION, vod, 0.0, 41.0)  # the second play's start, not the first's
    assert rec_p == pytest.approx(41.0) and vod_p == pytest.approx(5.0 + 41.0), (rec_p, vod_p)


# ---------------------------------------------------------------- legacy rows: unchanged

@pytest.mark.parametrize("sess", ["s2", "s3"])
def test_one_run_reads_a_real_session_exactly_like_the_legacy_timeline(sess):
    from test_youtube_leg_verdict_1404 import S2_T0, S2_WINDOWS, S3_A_START, S3_B_START, S3_WINDOWS

    if sess == "s2":
        rec, t0, windows = ylv.load_ticks(FIX / "s2-rec-ticks.tsv.gz"), S2_T0, S2_WINDOWS
        vod = ylv.load_ticks(FIX / "s2-vod-ticks.tsv.gz")
    else:
        rec, t0 = ylv.join_parts([(FIX / "s3-rec_a-ticks.tsv.gz", S3_A_START), (FIX / "s3-rec_b-ticks.tsv.gz",
                                                                                 S3_B_START)])
        vod, windows = ylv.load_ticks(FIX / "s3-vod-ticks.tsv.gz"), S3_WINDOWS
    run = [r + ((PAINTER if r[2] is not None else None),) for r in rec]
    vrun = [r + ((PAINTER if r[2] is not None else None),) for r in vod]
    for name, a, b in windows:
        old, new = ylv.dupskip(rec, vod, t0, a, b), ylv.dupskip(run, vrun, t0, a, b)
        assert new.pop("run") == PAINTER
        assert new == old, (sess, name)
        assert ylv.coverage(run, vrun, t0, a, b) == ylv.coverage(rec, vod, t0, a, b), (sess, name)


def test_the_default_cache_key_is_unchanged_and_a_run_scoped_one_names_its_runs(tmp_path):
    src = tmp_path / "x.mkv"
    src.write_bytes(b"0" * 10)
    key = ylv.tick_cache_key(str(src))
    assert ylv.DECODER_VERSION == 2
    assert key.endswith(f"decoder=v2 scale={ylv.DECODE_SCALE} phase_radius={ylv.PHASE_RADIUS} "
                        f"opencv={ylv._cv2_version()}"), key
    assert "runs" not in key
    assert ylv.tick_cache_key(str(src), ylv.CLIP_RUNS) == key + " runs=911016"


def test_a_cached_map_of_one_decode_is_never_read_by_the_other(tmp_path, monkeypatch):
    src = tmp_path / "x.mkv"
    src.write_bytes(b"0" * 10)
    calls = []

    def fake_decode(path, workers=4, scale=0.5, runs=()):
        calls.append(tuple(runs))
        return [(0, 0.0, 10, "B", 10, 9) + ((CLIP,) if runs else ())]

    monkeypatch.setattr(ylv, "decode_ticks", fake_decode)
    cache = tmp_path / "ticks.tsv"
    assert ylv.cached_ticks(str(src), str(cache), 1) == [(0, 0.0, 10)]
    assert ylv.cached_ticks(str(src), str(cache), 1, ylv.CLIP_RUNS) == [(0, 0.0, 10, CLIP)]
    assert ylv.cached_ticks(str(src), str(cache), 1, ylv.CLIP_RUNS) == [(0, 0.0, 10, CLIP)]  # cached
    assert ylv.cached_ticks(str(src), str(cache), 1) == [(0, 0.0, 10)]
    assert calls == [(), (CLIP,), ()]


# ---------------------------------------------------------------- the decoder: runs on real frames

def _write_two_run_video(path, painter_frames=60, clip_frames=60, tail_frames=0):
    """A lossless 960x540 clip, no audio: `painter_frames` painter frames (run PAINTER, odd capture
    phase), `clip_frames` frames of the measurement clip (run 911016, tick 2f from 0, even phase), then
    `tail_frames` painter frames again (the painter counted on during the clip)."""
    ff = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24",
                           "-s", "960x540", "-r", str(FPS), "-i", "-", "-c:v", "ffv1", "-pix_fmt", "bgr0", str(path)],
                          stdin=subprocess.PIPE)

    def painter(k):
        t = 5000 + 2 * k
        ff.stdin.write(qr_frame(payload(t, run=PAINTER), payload(t + 1, run=PAINTER)).tobytes())

    for k in range(painter_frames):
        painter(k)
    for f in range(clip_frames):
        t = 2 * f
        ff.stdin.write(qr_frame(payload(t, run=CLIP, gen=t), payload(t - 1, run=CLIP, gen=t) if t else None).tobytes())
    for k in range(tail_frames):
        painter(painter_frames + clip_frames + k)
    ff.stdin.close()
    assert ff.wait() == 0


@pytest.fixture(scope="module")
def two_run_video(tmp_path_factory):
    path = tmp_path_factory.mktemp("ylv-runs") / "two-runs.mkv"
    _write_two_run_video(path)
    return path


def test_a_run_scoped_decode_carries_each_frames_run_and_the_default_decode_is_unchanged(two_run_video):
    rows = ylv.decode_ticks(str(two_run_video), workers=2, runs=ylv.CLIP_RUNS)
    assert len(rows) == 120 and all(len(r) == 7 for r in rows)
    assert [(r[2], r[6]) for r in rows[:60]] == [(5000 + 2 * k, PAINTER) for k in range(60)]
    assert [(r[2], r[6]) for r in rows[60:]] == [(2 * f, CLIP) for f in range(60)]
    default = ylv.decode_ticks(str(two_run_video), workers=2)
    assert all(len(r) == 6 for r in default)
    assert default[:60] == [r[:6] for r in rows[:60]], "the painter frames read exactly as before"
    assert all(r[2] is None for r in default[60:]), "the default decode still refuses the clip's id"


def test_a_frame_whose_halves_read_two_runs_reads_nothing():
    import cv2

    det = cv2.QRCodeDetector()
    mixed = qr_frame(payload(100, run=PAINTER), payload(101, run=CLIP))
    assert ylv.half_ticks_run(mixed, det, runs=ylv.CLIP_RUNS) == (None, None, None)
    same = qr_frame(payload(100, run=CLIP), payload(99, run=CLIP))
    assert ylv.half_ticks_run(same, det, runs=ylv.CLIP_RUNS) == (100, 99, CLIP)
    assert ylv.half_ticks_run(same, det) == (None, None, None)  # without runs the clip id is refused


def test_a_right_only_frame_takes_the_phase_of_its_own_run_only():
    # raw (index, pts, left, right, run): the painter frames read on ODD ticks (right = left + 1), the
    # clip's on EVEN ones (right = left - 1); the clip's first frame shows only its right half
    raw = [(k, k / 30.0, 5000 + 2 * k, 5001 + 2 * k, PAINTER) for k in range(10)]
    raw += [(10, 10 / 30.0, None, 21, CLIP)] + [(10 + f, (10 + f) / 30.0, 20 + 2 * f, 19 + 2 * f, CLIP)
                                                for f in range(1, 10)]
    res = ylv.resolve_ticks(raw)
    assert res[10][2:] == (22, "R"), res[10]  # right 21 with the clip's EVEN phase: tick 22, never 20


# ---------------------------------------------------------------- the committed clip fixture

def test_a_cg_window_on_the_committed_clip_fixture_reads_clean():
    rows = ylv.decode_ticks(str(CLIP_FIX), workers=2, runs=ylv.CLIP_RUNS)
    assert [(r[0], r[2], r[6]) for r in rows] == [(f, 2 * f, CLIP) for f in range(120)]
    committed = ylv.load_run_ticks(CLIP_TICKS)  # write_ticks keeps the pts to the ms
    assert [(r[0], round(r[1], 3), r[2], r[6]) for r in rows] == committed, "the committed tick map is this decode"
    clip = [(300 + r[0], 10.0 + r[1], r[2], r[6]) for r in rows]  # the clip played 10 s into a session
    rec = painter_rows(0, 300) + clip + painter_rows(420, 300)
    vod = vod_of(rec, start=3.0)
    r = ylv.dupskip(rec, vod, 0.0, 10.0, 14.0)
    assert (r.get("error"), r["dup"], r["skip"], r["run"]) == (None, 0, 0, CLIP), r
    assert r["unanchored_s"] <= 0.1 and r["unjudged"] == 0 and r["vod_blind_s"] == 0.0, r
    cov = ylv.coverage(rec, vod, 0.0, 10.0, 14.0)
    assert (cov["rec_cadence_pct"], cov["vod_cadence_pct"]) == (100.0, 100.0), cov
    tl = ylv.run_timeline(rec, vod, 0.0)
    j = tl.window_segment(10.0, 14.0)[0]
    assert tl.segs[j].run == CLIP and tl.clocks[j].time_of(0) == pytest.approx(10.0)


# ---------------------------------------------------------------- the CLI with --runs

FAKE_PROBE = r'''#!/usr/bin/env python3
import json, sys
clip = sys.argv[sys.argv.index("--av-sync") + 1]
with open(clip + ".argv", "w") as f:
    json.dump(sys.argv[1:], f)
off = 30.0 if "av-rec-" in clip else 25.0
print("{\n" + json.dumps({"av_offset_ms": off, "mad_ms": 4.0, "matched": 12}, indent=2)[1:], file=sys.stderr)
'''


@pytest.fixture(scope="module")
def runs_cli(tmp_path_factory):
    d = tmp_path_factory.mktemp("ylv-runs-cli")
    raw = d / "raw.mkv"
    _write_two_run_video(raw, painter_frames=120, clip_frames=120, tail_frames=90)  # 4 s + 4 s + 3 s
    rec = d / "rec.mkv"  # with an audio track (the audio criterion locks on 4 s of it from the window start)
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(raw), "-f", "lavfi", "-i",
                    "anoisesrc=d=12:c=pink:r=48000:a=0.05:seed=7", "-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy",
                    "-c:a", "aac", "-b:a", "192k", "-shortest", str(rec)], check=True)
    probe = d / "fake-recording-verdict"
    probe.write_text(FAKE_PROBE)
    probe.chmod(0o755)
    (d / "markers.csv").write_text("index,frame_id,emit_ts_ns\n")
    (d / "clip-markers.csv").write_text("index,frame_id,emit_ts_ns\n30,30,500000000\n")
    return d


def _run_cli(d, out, extra):
    t0 = 1791250000.0
    cmd = [sys.executable, str(SCRIPT), "--vod", str(d / "rec.mkv"), "--recording", f"{d / 'rec.mkv'}@{t0}",
           "--markers", str(d / "markers.csv"), "--windows", f"CAM:{t0 + 0.5}:{t0 + 3.5}",
           "--windows", f"CG:{t0 + 4.5}:{t0 + 7.5}", "--out", str(out), "--probe-bin", str(d / "fake-recording-verdict"),
           "--workers", "2", *extra]
    r = subprocess.run(cmd, capture_output=True, text=True)
    return r, json.loads((out / "youtube-leg-verdict.json").read_text())


def test_the_cli_judges_a_camera_and_a_cg_window_each_on_its_own_run(runs_cli, tmp_path):
    out = tmp_path / "out"
    r, j = _run_cli(runs_cli, out, ["--runs", "911016", "--clip-markers", str(runs_cli / "clip-markers.csv")])
    assert r.returncode == 0 and j["overall"] == "PASS", (r.stdout, r.stderr, j["reasons"])
    assert j["tool"]["runs"] == [CLIP]
    cam, cg = j["windows"]
    assert (cam["run"], cg["run"]) == (PAINTER, CLIP)
    for w in (cam, cg):
        assert (w["dupskip"]["dup"], w["dupskip"]["skip"]) == (0, 0), w
        assert w["coverage"]["rec_cadence_pct"] == 100.0, w
    argvs = {p.name: json.loads(p.read_text()) for p in out.glob("av-*.argv")}
    assert len(argvs) == 4, argvs
    for name, argv in argvs.items():
        cg_clip = "--av-run" in argv
        assert argv[argv.index("--av-marker-log") + 1] == str(runs_cli / ("clip-markers.csv" if cg_clip else "markers.csv"))
        if cg_clip:
            assert argv[argv.index("--av-run") + 1] == "911016"
    assert sum("--av-run" in a for a in argvs.values()) == 2, "exactly the CG window's two clips pair the clip's run"
    assert (out / "ticks-rec-1.tsv").read_text().splitlines()[0].endswith(" runs=911016")


def test_the_cli_without_runs_writes_the_verdict_it_always_wrote(runs_cli, tmp_path):
    out = tmp_path / "out"
    r, j = _run_cli(runs_cli, out, [])
    assert "runs" not in j["tool"] and all("run" not in w for w in j["windows"])
    assert "runs=" not in (out / "ticks-rec-1.tsv").read_text().splitlines()[0]
    assert j["windows"][0]["dupskip"]["dup"] == 0  # the camera window reads as before
    assert j["windows"][1]["errors"], "the clip stretch is undecodable without --runs: never a verdict"
    assert r.returncode == 2 and j["overall"] == "UNKNOWN"
