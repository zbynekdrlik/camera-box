"""Issue 1460 (21.11.2026): the bench's test track is EXCLUSIVE while a run is set up.

Every strih input feeds tracks 1-6 (OBS's default) and, since issue 1485, the strih program audio is
the Dante input `ASIO zvuk`, audible in the program scene: the R = 80 ms run of 21.11.2026 recorded
the program audio on the test track (lead -52.9 / -30.4 dBFS, residual p99 -12.7 dB) and every
profile read INVALID. setup now takes the test track out of every OTHER input that feeds it, saving
each one's tracks in the state file BEFORE the change, and teardown puts the track back on exactly
those inputs. An input whose track the operator switched back on meanwhile is left alone.
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
import vban_bench_obs as vo  # noqa: E402
from test_vban_bench_obs_1460 import NAME, FakeObs, _setup  # noqa: E402


def _feeding(obs, track=6):
    return sorted(n for n, i in obs.inputs.items() if i["tracks"].get(str(track)))


def test_setup_leaves_the_test_input_alone_on_its_track(tmp_path):
    obs = FakeObs()
    obs.inputs["ASIO zvuk"]["tracks"] = {str(i): True for i in range(1, 7)}
    st = _setup(obs, tmp_path)
    assert _feeding(obs) == [NAME], obs.inputs
    assert obs.inputs["ASIO zvuk"]["tracks"]["1"] is True, "only the test track is taken away"
    saved = json.loads((tmp_path / "state.json").read_text())["feeder_tracks"]
    assert set(saved) == {"ASIO zvuk", "NDI cam1"} and st["feeder_tracks"] == saved


def test_the_saved_tracks_are_written_before_the_first_feeder_changes(tmp_path):
    obs = FakeObs()
    seen = {}
    real = obs.rpc

    def rpc(rtype, rdata=None, ignore_err=False):
        if rtype == "SetInputAudioTracks" and (rdata or {}).get("inputName") != NAME and not seen:
            seen["state"] = json.loads((tmp_path / "state.json").read_text())
        return real(rtype, rdata, ignore_err)
    obs.rpc = rpc
    _setup(obs, tmp_path)
    assert "NDI cam1" in seen["state"]["feeder_tracks"], seen


def test_teardown_puts_the_track_back_on_exactly_the_feeders(tmp_path):
    obs = FakeObs()
    obs.inputs["ASIO zvuk"]["tracks"] = {str(i): True for i in range(1, 7)}
    st = _setup(obs, tmp_path)
    res = vo.teardown(obs.rpc, st)
    assert res["ok"], res
    assert _feeding(obs) == ["ASIO zvuk", "NDI cam1"]
    assert vo.teardown(obs.rpc, st)["ok"]               # idempotent


def test_teardown_leaves_a_feeder_the_operator_changed_since(tmp_path):
    obs = FakeObs()
    st = _setup(obs, tmp_path)
    obs.inputs["NDI cam1"]["tracks"]["6"] = True        # switched back on by hand meanwhile
    res = vo.teardown(obs.rpc, st)
    assert res["ok"], res
    assert obs.inputs["NDI cam1"]["tracks"]["6"] is True


def test_a_feeder_whose_track_does_not_read_back_off_fails_after_a_change(tmp_path):
    obs = FakeObs()
    real = obs.rpc

    def rpc(rtype, rdata=None, ignore_err=False):
        if rtype == "SetInputAudioTracks" and (rdata or {}).get("inputName") == "NDI cam1":
            return {}                                   # OBS ignores it
        return real(rtype, rdata, ignore_err)
    obs.rpc = rpc
    try:
        _setup(obs, tmp_path)
    except vo.BenchError as e:
        assert "NDI cam1" in str(e)
    else:
        raise AssertionError("a feeder that keeps the test track must fail the setup")
