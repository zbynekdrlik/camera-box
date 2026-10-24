"""Issue 1404, ROZHODNUTE 6389130309 (main, 24.10.2026) -- the A/V step reads a 1080p cut of each
recording window, never the 4K original, and runs the windows on a bounded pool on dev2.

Why: one window's 4K read (the x264 cut + `recording-verdict --av-sync`) took ~230 s on dev2, ten
windows 50-80 min, past the tool's 1500 s bound; the same window cut at 1080p read the same 50/50
markers and an offset 0.56 ms away in 61 s. Pinned here:
  - a recording above 1080 lines is cut at 1080 (`scale=-2:1080`, a scale of the same frames); a
    source at or below 1080 (the VOD) is cut as before;
  - a scaled read with fewer markers than its window holds is an error (the window's A/V UNKNOWN),
    never a silent re-read of the 4K original;
  - the real windows of E2E run 40110304642 (tests/fixtures/youtube_leg_1404/run40110304642-*.avsync.out,
    the probe's output for the same window at 4K and at 1080p) read the same marker count and an
    offset within AV_SCALED_OFFSET_TOLERANCE_MS;
  - the windows' A/V runs on a pool of AV_POOL_MAX, sized down under the host's load (never below
    AV_POOL_MIN), and really runs concurrently.
"""
import pathlib
import subprocess
import sys
import threading
import time

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from youtube_leg_fakes_1404 import FIX, ylv  # noqa: E402

RUN_FIX = sorted(FIX.glob("run40110304642-*-4k.avsync.out"))


def _probe_json(matched, off=10.0, audio=50):
    return {"av_offset_ms": off, "mad_ms": 4.0, "matched": matched, "audio_markers_decoded": audio}


class _Cuts(list):
    """The cuts av_window made [(source name, scale_to)], plus `clips` = the clip names probed."""

    def __init__(self):
        super().__init__()
        self.clips = []


def _fake_reads(monkeypatch, heights, matched=None):
    """av_window's outside steps faked: the source heights, the cuts (recorded), the probe."""
    cuts = _Cuts()
    monkeypatch.setattr(ylv, "video_height", lambda path: heights[pathlib.Path(path).name])

    def cut(src, start_s, dur_s, out, scale_to=None):
        cuts.append((pathlib.Path(src).name, scale_to))
        pathlib.Path(out).write_bytes(b"clip")

    def avsync(probe, clip, markers, av_run=None):
        cuts.clips.append(pathlib.Path(clip).name)
        return _probe_json((matched or {}).get("rec" if "av-rec-" in str(clip) else "vod", 50))

    monkeypatch.setattr(ylv, "_cut_clip", cut)
    monkeypatch.setattr(ylv, "_avsync", avsync)
    return cuts


def test_a_4k_recording_window_is_read_at_1080p_and_a_1080p_vod_as_it_is(tmp_path, monkeypatch):
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 2160, "vod.mp4": 1080})
    av = ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv",
                       "probe", 25.0, str(tmp_path))
    assert cuts == [("rec.mp4", ylv.AV_READ_HEIGHT), ("vod.mp4", None)]
    assert ylv.AV_READ_HEIGHT == 1080
    assert av["rec_read"] == "1080p of 2160p" and av["vod_read"] == "1080p"
    assert av["delta_ms"] == 0.0 and av["markers_rec"] == 50


def test_a_1080p_read_short_of_markers_is_an_error_never_a_4k_reread(tmp_path, monkeypatch):
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 2160, "vod.mp4": 1080}, matched={"rec": 48})
    with pytest.raises(ValueError, match="1080p read of the 2160p recording matched 48 of the 50 audio markers"):
        ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv",
                      "probe", 25.0, str(tmp_path))
    assert cuts == [("rec.mp4", 1080), ("vod.mp4", None)]  # no second, 4K cut
    # one marker at the clip's edge is allowed: 25 s hold 50 markers, a read of 49 is whole
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 2160, "vod.mp4": 1080}, matched={"rec": 49})
    assert ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv",
                         "probe", 25.0, str(tmp_path))["markers_rec"] == 49
    # an unscaled read keeps the old floor (half the window's markers)
    _fake_reads(monkeypatch, {"rec.mp4": 1080, "vod.mp4": 1080}, matched={"rec": 30})
    assert ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv",
                         "probe", 25.0, str(tmp_path))["rec_read"] == "1080p"


def test_the_1080p_cut_is_a_scale_of_the_same_frames(tmp_path):
    src = tmp_path / "uhd.mp4"
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                    "testsrc2=size=3840x2160:rate=30:duration=2", "-f", "lavfi", "-i", "sine=duration=2",
                    "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest", str(src)], check=True)
    assert ylv.video_height(src) == 2160
    full, scaled = tmp_path / "full.mp4", tmp_path / "scaled.mp4"
    ylv._cut_clip(src, 0.5, 1.0, full)
    ylv._cut_clip(src, 0.5, 1.0, scaled, scale_to=1080)
    assert ylv.video_height(scaled) == 1080 and ylv.video_height(full) == 2160
    counts = [ylv.container_frames(str(p)) for p in (full, scaled)]
    assert counts[0] == counts[1] == 30


@pytest.mark.parametrize("path", RUN_FIX, ids=[p.name for p in RUN_FIX])
def test_the_real_windows_read_the_same_at_1080p_and_4k(path):
    uhd = ylv.parse_avsync_output(path.read_text())
    hd = ylv.parse_avsync_output(path.with_name(path.name.replace("-4k.", "-1080p.")).read_text())
    assert hd["matched"] == uhd["matched"] >= 49
    assert abs(hd["av_offset_ms"] - uhd["av_offset_ms"]) <= ylv.AV_SCALED_OFFSET_TOLERANCE_MS
    assert ylv.AV_SCALED_OFFSET_TOLERANCE_MS < 0.05 * ylv.AV_TOLERANCE_MS  # far inside the gate's bar
    # the 1080p read passes the scaled-read guard on its own window
    ylv.av_from_outputs(hd, hd, 25.0, scaled=("recording",))


def test_the_real_fixture_set_is_there():
    assert len(RUN_FIX) >= 3


@pytest.mark.parametrize("load, cores, size", [(0.0, 8, 4), (8.0, 8, 4), (12.0, 8, 3), (16.0, 8, 2),
                                                 (36.0, 8, 2), (3.0, 4, 4), (None, 8, 4)])
def test_the_av_pool_is_four_and_shrinks_under_load(load, cores, size):
    assert ylv.av_pool_size(load, cores) == size
    assert (ylv.AV_POOL_MAX, ylv.AV_POOL_MIN) == (4, 2)


def test_the_windows_av_runs_concurrently_on_the_pool():
    live, peak, lock = [0], [0], threading.Lock()

    def job(k):
        def run():
            with lock:
                live[0] += 1
                peak[0] = max(peak[0], live[0])
            time.sleep(0.3)
            with lock:
                live[0] -= 1
            if k == 2:
                raise RuntimeError("probe failed")
            return {"delta_ms": float(k)}
        return run

    t0 = time.monotonic()
    out = ylv.run_av_jobs([job(k) for k in range(6)], 3)
    assert time.monotonic() - t0 < 1.5  # 6 jobs x 0.3 s on 3 at once, not 1.8 s one after the other
    assert peak[0] == 3
    assert [o.get("delta_ms") for o in out] == [0.0, 1.0, None, 3.0, 4.0, 5.0]  # in window order
    assert "probe failed" in out[2]["error"]  # a failed window is its own error, never the others'


# ---------------------------------------------------------------- review round 3


def test_the_guard_counts_against_the_reads_own_audio_markers_not_a_theoretical_number():
    """A scale can only lose VIDEO ticks: the audio track is the same, so a scaled read must pair every
    audio marker it decoded (one at the edge). A real 40 s recording read (base-rec-B: 79 audio markers,
    78 paired, unscaled) holds a theoretical 80, so a count against 80 would fail it."""
    b = ylv.parse_avsync_output((FIX / "base-rec-B.avsync.out").read_text())
    assert (b["audio_markers_decoded"], b["matched"]) == (79, 78)
    ylv.av_from_outputs(b, b, 40.0, scaled=("recording",), heights={"recording": 2160})
    short = dict(b, matched=70)
    with pytest.raises(ValueError, match="matched 70 of the 79 audio markers it decoded"):
        ylv.av_from_outputs(short, b, 40.0, scaled=("recording",), heights={"recording": 2160})
    # without the audio count (an older probe): the window's markers minus one at each edge
    bare = {k: v for k, v in b.items() if k != "audio_markers_decoded"}
    ylv.av_from_outputs(bare, b, 40.0, scaled=("recording",))
    with pytest.raises(ValueError):
        ylv.av_from_outputs(dict(bare, matched=77), b, 40.0, scaled=("recording",))


def test_every_committed_real_read_passes_the_scaled_guard():
    reads = sorted(FIX.glob("*-rec-*.avsync.out")) + sorted(FIX.glob("run40110304642-*.avsync.out"))
    assert len(reads) == 15  # the 9 session recordings (40 s) + the run's 6 windows (25 s)
    for path in reads:
        j = ylv.parse_avsync_output(path.read_text())
        dur = 25.0 if path.name.startswith("run40110304642-") else 40.0
        ylv.av_from_outputs(j, j, dur, scaled=("recording",))


def test_each_windows_clip_names_carry_the_window(tmp_path, monkeypatch):
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 2160, "vod.mp4": 1080})
    ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv", "probe", 25.0,
                  str(tmp_path), tag="w03-CAM3")
    assert cuts.clips == ["av-rec-w03-CAM3-10.000.mp4", "av-vod-w03-CAM3-20.000.mp4"]


def test_a_window_name_is_made_safe_for_a_file_name(tmp_path, monkeypatch):
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 1080, "vod.mp4": 1080})
    ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv", "probe", 25.0,
                  str(tmp_path), tag="cg/../w 1")
    assert cuts.clips == ["av-rec-cg_.._w_1-10.000.mp4", "av-vod-cg_.._w_1-20.000.mp4"]


def test_only_the_recording_read_carries_the_strict_scaled_guard(tmp_path, monkeypatch):
    """A VOD is cut at 1080 too when yt-dlp's last fallback returns more lines, but an unscaled VOD
    read already loses markers (s2-vod-A paired 79 of 81, base-vod-R 69 of 79): the VOD keeps the old
    floor (half the window's markers); the ROZHODNUTE guard is on the recording read."""
    for name in ("s2-vod-A", "base-vod-R"):
        j = ylv.parse_avsync_output((FIX / f"{name}.avsync.out").read_text())
        assert j["matched"] < j["audio_markers_decoded"] - 1
    cuts = _fake_reads(monkeypatch, {"rec.mp4": 2160, "vod.mp4": 1440}, matched={"vod": 40})
    av = ylv.av_window(tmp_path / "rec.mp4", tmp_path / "vod.mp4", 10.0, 20.0, tmp_path / "m.csv", "probe", 25.0,
                       str(tmp_path))
    assert cuts == [("rec.mp4", 1080), ("vod.mp4", 1080)]
    assert av["markers_vod"] == 40 and av["vod_read"] == "1080p of 1440p"


def test_an_explicit_pool_of_one_is_honoured():
    assert ylv.av_pool_size(0.0, 8, 1) == 1 and ylv.av_pool_size(40.0, 8, 1) == 1
    assert ylv.av_pool_size(40.0, 8, 3) == 2


def test_a_stop_cancels_the_queued_windows():
    started, lock = [], threading.Lock()

    def job(k):
        def run():
            with lock:
                started.append(k)
            if k == 0:
                raise SystemExit(143)  # what the tool's SIGTERM handler raises
            time.sleep(0.2)
            return {"delta_ms": 0.0}
        return run

    with pytest.raises(SystemExit):
        ylv.run_av_jobs([job(k) for k in range(8)], 2)
    time.sleep(0.5)
    assert len(started) <= 3, started  # the windows still queued never start


def test_no_child_starts_once_the_tool_is_stopping(monkeypatch):
    ylp = sys.modules["youtube_leg_proc"]
    monkeypatch.setattr(ylp, "_STOPPING", True)
    with pytest.raises(RuntimeError, match="stopping"):
        ylp.run_bounded(["true"], 5)
