"""#758 preflight -- unit tests for the NEW obs_phase2.py `open-projectors` action: imag-nb's
Multiview AND Program projectors must be OPEN before ANY run starts (the user's explicit, binding
requirement -- a run must NEVER begin with Multiview closed). obs-websocket 5.x has no "is a
projector open" introspection request, so open_projectors ALWAYS (idempotently) opens both via
OpenVideoMixProjector rather than check-then-open.

Same mocking pattern as tests/python/test_obs_phase2_event_assert_actions.py: patch `_rpc`/`_conn`
to avoid a live OBS connection, capture every call, assert on the DECISION (which requests are
sent, with which params, and that a failure propagates rather than being swallowed).
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_open_projectors", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_open_projectors"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def _patch(monkeypatch, rpc_side_effect=None):
    """Patches `_rpc`/`_conn` to avoid a real websocket connection. Returns the captured calls
    list. `rpc_side_effect`, if given, is called with (op, payload) for each _rpc call and may
    raise (to simulate a failed OBS request)."""
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        if rpc_side_effect is not None:
            rpc_side_effect(op, payload or {})
        return {}

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


def test_opens_both_multiview_and_program_projectors_on_the_documented_monitors(monkeypatch, capsys):
    calls = _patch(monkeypatch)
    obs_phase2.open_projectors(_args(host="10.77.9.182", password=""))

    assert len(calls) == 2
    assert calls[0] == (
        "OpenVideoMixProjector",
        {"videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW", "monitorIndex": 0},
    )
    assert calls[1] == (
        "OpenVideoMixProjector",
        {"videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM", "monitorIndex": 1},
    )

    out = capsys.readouterr().out
    assert "Multiview" in out and "monitorIndex 0" in out
    assert "Program" in out and "monitorIndex 1" in out


def test_a_failed_open_request_propagates_never_silently_continues(monkeypatch):
    def boom(op, payload):
        if op == "OpenVideoMixProjector" and payload.get("videoMixType") == \
                "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW":
            raise RuntimeError("OpenVideoMixProjector failed: {'result': False}")

    _patch(monkeypatch, rpc_side_effect=boom)
    with pytest.raises(RuntimeError):
        obs_phase2.open_projectors(_args(host="10.77.9.182", password=""))


def test_program_projector_still_attempted_would_not_matter_if_multiview_already_raised(monkeypatch):
    # A failure on the FIRST call (Multiview) must abort immediately -- Program must never be
    # silently skipped in a way that hides the Multiview failure, but the caller must also never
    # believe Program succeeded when the whole preflight action raised. Confirms only ONE call
    # happened before the raise (no swallow-and-continue).
    calls = []

    def boom(op, payload):
        raise RuntimeError("OBS unreachable")

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        boom(op, payload or {})

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())

    with pytest.raises(RuntimeError):
        obs_phase2.open_projectors(_args(host="10.77.9.182", password=""))
    assert len(calls) == 1, "must abort on the FIRST failure, never continue to the second call"
