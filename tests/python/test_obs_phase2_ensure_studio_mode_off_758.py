"""#758 render-health preflight follow-up -- unit tests for the NEW obs_phase2.py
`ensure-studio-mode-off` action: imag's Studio Mode is a SEPARATE render-budget consumer from
the Multiview (SKILL.md #278) -- live-caught 2026-07-14, a stale Studio-Mode-ON left over from an
earlier session intermittently failed the render-health preflight (activeFps down to ~57,
averageFrameRenderTime up to ~17ms) even with nothing else wrong. `ensure_studio_mode_off` must
ALWAYS (idempotently) turn it OFF -- never silently leave a stale ON state.

Same mocking pattern as tests/python/test_obs_phase2_open_projectors_758.py: patch `_rpc`/`_conn`
to avoid a live OBS connection, capture every call, assert on the DECISION.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_ensure_studio_mode_off", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_ensure_studio_mode_off"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def _patch(monkeypatch, get_response, rpc_side_effect=None):
    """Patches `_rpc`/`_conn`. *get_response* is the GetStudioModeEnabled response dict. Returns
    the captured calls list."""
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        if rpc_side_effect is not None:
            rpc_side_effect(op, payload or {})
        if op == "GetStudioModeEnabled":
            return get_response
        return {}

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


def test_turns_off_studio_mode_when_currently_on(monkeypatch, capsys):
    calls = _patch(monkeypatch, {"studioModeEnabled": True})
    obs_phase2.ensure_studio_mode_off(_args(host="10.77.9.182", password=""))

    assert ("GetStudioModeEnabled", {}) in calls
    assert ("SetStudioModeEnabled", {"studioModeEnabled": False}) in calls
    out = capsys.readouterr().out
    assert "was ON" in out and "turned OFF" in out


def test_leaves_studio_mode_alone_when_already_off(monkeypatch, capsys):
    calls = _patch(monkeypatch, {"studioModeEnabled": False})
    obs_phase2.ensure_studio_mode_off(_args(host="10.77.9.182", password=""))

    assert ("GetStudioModeEnabled", {}) in calls
    assert ("SetStudioModeEnabled", {"studioModeEnabled": False}) not in calls, (
        "must not call SetStudioModeEnabled when it is already off -- no-op, no needless RPC"
    )
    out = capsys.readouterr().out
    assert "already OFF" in out


def test_a_failed_set_request_propagates_never_silently_continues(monkeypatch):
    def boom(op, payload):
        if op == "SetStudioModeEnabled":
            raise RuntimeError("SetStudioModeEnabled failed: {'result': False}")

    _patch(monkeypatch, {"studioModeEnabled": True}, rpc_side_effect=boom)
    with pytest.raises(RuntimeError):
        obs_phase2.ensure_studio_mode_off(_args(host="10.77.9.182", password=""))
