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


def test_a_local_file_is_used_as_is(tmp_path):
    f = tmp_path / "vod.mp4"
    f.write_bytes(b"x")
    assert v.fetch_vod(str(f), str(tmp_path)) == str(f)
