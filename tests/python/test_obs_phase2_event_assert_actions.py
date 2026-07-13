"""#722 -- unit tests for the two NEW obs_phase2.py actions the EVENT-mode CONTRACT's gather
step needs: `stream-status` (item 4, GetStreamStatus counterpart to the existing
`record --action status`) and `latency-check` (item 6, the #691 stomp-protection: is the
stream PGM's genlock_latency_ms_src == the CALIBRATED value from av-sync-last.json, and if not,
RESTORE it and re-verify -- never just report the mismatch).

Same mocking pattern as tests/python/test_obs_phase2_test_preload.py: patch `_rpc`/`_conn` to
avoid a live OBS connection, capture every call, and assert on the DECISION (what gets
read/written and the resulting exit code), not on the transport.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_event_assert", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_event_assert"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def _patch(monkeypatch, rpc_responses):
    """rpc_responses: list of return values, ONE per `_rpc` call in call order. Also patches
    `_conn` to avoid a real websocket connection. Returns the captured calls list."""
    calls = []
    responses = list(rpc_responses)

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        return responses.pop(0) if responses else {}

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


# ---------------------------------------------------------------------------
# stream_status
# ---------------------------------------------------------------------------


def test_stream_status_reports_active_true(monkeypatch, capsys):
    _patch(monkeypatch, [{"outputActive": True}])
    obs_phase2.stream_status(_args(host="10.77.9.204", password=""))
    out = capsys.readouterr().out.strip()
    assert out == "active=True path="


def test_stream_status_reports_active_false(monkeypatch, capsys):
    _patch(monkeypatch, [{"outputActive": False}])
    obs_phase2.stream_status(_args(host="10.77.9.204", password=""))
    out = capsys.readouterr().out.strip()
    assert out == "active=False path="


def test_stream_status_never_calls_start_or_stop_stream(monkeypatch):
    calls = _patch(monkeypatch, [{"outputActive": True}])
    obs_phase2.stream_status(_args(host="10.77.9.204", password=""))
    ops = [c[0] for c in calls]
    assert ops == ["GetStreamStatus"]


# ---------------------------------------------------------------------------
# latency_check
# ---------------------------------------------------------------------------


def test_latency_check_passes_when_already_at_calibrated_value(monkeypatch, capsys):
    calls = _patch(
        monkeypatch,
        [{"inputSettings": {"genlock_latency_ms_src": 925}}],
    )
    with pytest.raises(SystemExit) as exc:
        obs_phase2.latency_check(
            _args(host="10.77.9.204", password="", source="NDI 2ME PGM", calibrated_ms=925)
        )
    assert exc.value.code == 0
    # No SetInputSettings -- nothing needed changing.
    ops = [c[0] for c in calls]
    assert "SetInputSettings" not in ops
    out = capsys.readouterr().out
    assert "current=925 calibrated=925 restored=False final=925" in out


def test_latency_check_restores_a_drifted_value_and_passes(monkeypatch, capsys):
    calls = _patch(
        monkeypatch,
        [
            {"inputSettings": {"genlock_latency_ms_src": 1000}},  # initial read: drifted
            {},  # SetInputSettings response (ignored)
            {"inputSettings": {"genlock_latency_ms_src": 925}},  # readback: restored correctly
        ],
    )
    with pytest.raises(SystemExit) as exc:
        obs_phase2.latency_check(
            _args(host="10.77.9.204", password="", source="NDI 2ME PGM", calibrated_ms=925)
        )
    assert exc.value.code == 0
    ops = [c[0] for c in calls]
    assert ops == ["GetInputSettings", "SetInputSettings", "GetInputSettings"]
    set_call = calls[1]
    assert set_call[1]["inputSettings"]["genlock_latency_ms_src"] == 925
    out = capsys.readouterr().out
    assert "current=1000 calibrated=925 restored=True final=925" in out


def test_latency_check_fails_loud_when_restore_does_not_stick(monkeypatch, capsys):
    calls = _patch(
        monkeypatch,
        [
            {"inputSettings": {"genlock_latency_ms_src": 1000}},
            {},
            # Readback STILL wrong after the restore attempt -- must fail, never report success.
            {"inputSettings": {"genlock_latency_ms_src": 1000}},
        ],
    )
    with pytest.raises(SystemExit) as exc:
        obs_phase2.latency_check(
            _args(host="10.77.9.204", password="", source="NDI 2ME PGM", calibrated_ms=925)
        )
    assert exc.value.code == 1
    err = capsys.readouterr().err
    assert "FAIL" in err
    assert len(calls) == 3
