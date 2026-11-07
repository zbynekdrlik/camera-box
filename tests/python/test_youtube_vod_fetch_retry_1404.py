"""Issue 1404: restreamer reports the VOD done once YouTube reads it `uploaded` (restreamer
0.29.57), which can come before the 1080p + audio formats are listed. fetch_vod retries the
download until its bounded ready deadline, never once-and-fail, and never past the deadline."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "scripts"))
import youtube_leg_verdict as v  # noqa: E402

VOD = "vr2JmFSu8dc"


class FakeClock:
    def __init__(self):
        self.t = 0.0

    def now(self):
        return self.t

    def sleep(self, s):
        self.t += s


def test_a_vod_whose_formats_come_late_is_fetched_on_a_retry(tmp_path, monkeypatch):
    calls = []

    def fake_run(cmd, timeout, text=False, check=False):
        calls.append(cmd)
        if len(calls) < 3:
            raise RuntimeError("yt-dlp exited 1: ERROR: Requested format is not available")
        open(cmd[cmd.index("-o") + 1], "wb").close()

    monkeypatch.setattr(v, "run_bounded", fake_run)
    clk = FakeClock()
    path = v.fetch_vod(VOD, str(tmp_path), ready_deadline_s=600, sleep=clk.sleep, clock=clk.now)
    assert os.path.isfile(path)
    assert len(calls) == 3
    assert clk.t == 2 * v.YTDLP_RETRY_S


def test_a_vod_that_never_becomes_fetchable_fails_at_the_deadline(tmp_path, monkeypatch):
    calls = []

    def fake_run(cmd, timeout, text=False, check=False):
        calls.append(cmd)
        raise RuntimeError("yt-dlp exited 1: ERROR: Requested format is not available")

    monkeypatch.setattr(v, "run_bounded", fake_run)
    clk = FakeClock()
    with pytest.raises(RuntimeError, match="not available"):
        v.fetch_vod(VOD, str(tmp_path), ready_deadline_s=100, sleep=clk.sleep, clock=clk.now)
    assert clk.t <= 100
    assert len(calls) == 100 // v.YTDLP_RETRY_S + 1


def _fake_fetch(calls, durations):
    """yt-dlp writes a file; the n-th fetch's file lasts durations[n] s (the last value repeats)."""
    lengths = {}

    def fake_run(cmd, timeout, text=False, check=False):
        calls.append(cmd)
        if cmd[0] == "yt-dlp":
            path = cmd[cmd.index("-o") + 1]
            n = sum(1 for c in calls if c[0] == "yt-dlp") - 1
            lengths[path] = durations[min(n, len(durations) - 1)]
            open(path, "wb").close()
            return None
        assert cmd[0] == "ffprobe", cmd
        return type("R", (), {"stdout": f"{lengths[cmd[-1]]}\n"})()

    return fake_run


def test_a_vod_youtube_has_not_finished_is_fetched_again_1482(tmp_path, monkeypatch):
    # E2E 42314759130: `done` came while YouTube served 209 s of a 414 s broadcast
    calls = []
    monkeypatch.setattr(v, "run_bounded", _fake_fetch(calls, [209.4, 209.4, 469.5]))
    clk = FakeClock()
    path = v.fetch_vod(VOD, str(tmp_path), ready_deadline_s=600, sleep=clk.sleep, clock=clk.now,
                       min_duration_s=414.2 - v.VOD_SHORT_SLACK_S)
    assert os.path.isfile(path)
    assert [c[0] for c in calls].count("yt-dlp") == 3
    assert clk.t == 2 * v.YTDLP_RETRY_S


def test_a_vod_that_stays_short_fails_at_the_deadline_naming_both_durations_1482(tmp_path, monkeypatch):
    calls = []
    monkeypatch.setattr(v, "run_bounded", _fake_fetch(calls, [209.4]))
    clk = FakeClock()
    with pytest.raises(RuntimeError, match=r"209\.4 s.*324\.2 s"):
        v.fetch_vod(VOD, str(tmp_path), ready_deadline_s=100, sleep=clk.sleep, clock=clk.now,
                    min_duration_s=324.2)
    assert clk.t <= 100
    assert not os.path.exists(os.path.join(str(tmp_path), f"vod-{VOD}.mp4"))


def test_the_broadcast_bound_is_the_publish_to_unpublish_span_less_the_slack_1482():
    assert v.vod_min_duration([100.0, 150.0], [514.2]) == pytest.approx(414.2 - v.VOD_SHORT_SLACK_S)
    assert v.vod_min_duration([], [514.2]) is None
    assert v.vod_min_duration([100.0], []) is None
    assert v.VOD_SHORT_SLACK_S == 90.0


def test_a_local_file_is_used_as_is(tmp_path):
    f = tmp_path / "vod.mp4"
    f.write_bytes(b"x")
    assert v.fetch_vod(str(f), str(tmp_path)) == str(f)
