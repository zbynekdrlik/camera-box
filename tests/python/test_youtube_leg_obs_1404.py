"""Issue 1404 -- scripts/youtube_leg_obs.py: the stream OBS output for the YouTube stage, through an
injected obs-websocket `rpc` (the CLI binds obs_phase2's)."""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
import youtube_leg_obs as ylo  # noqa: E402


class FakeObs:
    def __init__(self, active=False, activates_after=1, stops_after=1, record=None):
        self.active, self.activates_after, self.stops_after = active, activates_after, stops_after
        self.record = record or {"outputActive": False}
        self.requests, self._pending = [], None

    def __call__(self, rtype, rdata=None, ignore_err=False):
        self.requests.append(rtype)
        if rtype == "GetStreamStatus":
            if self._pending is not None:
                self._pending[1] -= 1
                if self._pending[1] < 0:
                    self.active, self._pending = self._pending[0], None
            return {"outputActive": self.active}
        if rtype == "StartStream":
            self._pending = [True, self.activates_after]
        elif rtype == "StopStream":
            self._pending = [False, self.stops_after]
        elif rtype == "GetRecordStatus":
            return self.record
        return {}


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t

    def sleep(self, s):
        self.t += s


def test_stream_start_returns_the_publish_time_once_active():
    obs, c = FakeObs(activates_after=2), Clock()
    out = ylo.stream_start(obs, now=lambda: 1234.5, clock=c, sleep=c.sleep)
    assert out == {"publish_utc": 1234.5}
    assert obs.requests[:2] == ["GetStreamStatus", "StartStream"]
    assert "StopStream" not in obs.requests


def test_stream_start_never_takes_over_a_stream_that_is_already_running():
    obs = FakeObs(active=True)
    with pytest.raises(ylo.Refused) as e:
        ylo.stream_start(obs)
    assert e.value.extra == {"foreign": True}
    assert obs.requests == ["GetStreamStatus"]


def test_stream_start_times_out_when_the_output_never_activates():
    obs, c = FakeObs(activates_after=10 ** 6), Clock()
    with pytest.raises(ylo.Refused) as e:
        ylo.stream_start(obs, clock=c, sleep=c.sleep)
    assert "not active within" in str(e.value) and not e.value.extra


def test_stream_stop_stops_and_reports_and_is_a_noop_when_idle():
    obs, c = FakeObs(active=True, stops_after=1), Clock()
    assert ylo.stream_stop(obs, now=lambda: 9.0, clock=c, sleep=c.sleep) == {"unpublish_utc": 9.0, "was_active": True}
    assert "StopStream" in obs.requests
    idle = FakeObs(active=False)
    assert ylo.stream_stop(idle, now=lambda: 3.0) == {"unpublish_utc": 3.0, "was_active": False}
    assert "StopStream" not in idle.requests


def test_stream_stop_refuses_when_the_output_stays_active():
    obs, c = FakeObs(active=True, stops_after=10 ** 6), Clock()
    with pytest.raises(ylo.Refused):
        ylo.stream_stop(obs, clock=c, sleep=c.sleep)


def test_record_start_is_now_minus_the_output_duration():
    obs = FakeObs(record={"outputActive": True, "outputDuration": 12500})
    assert ylo.record_start(obs, now=lambda: 1000.0) == {"record_start_utc": 987.5}
    with pytest.raises(ylo.Refused):
        ylo.record_start(FakeObs(record={"outputActive": False}))
    with pytest.raises(ylo.Refused):
        ylo.record_start(FakeObs(record={"outputActive": True}))


def test_the_tool_never_selects_a_scene():
    src = (Path(ylo.__file__)).read_text()
    for banned in ("SetCurrentProgramScene", "SetCurrentPreviewScene", "StartRecord\"", "StopRecord\""):
        assert banned not in src
