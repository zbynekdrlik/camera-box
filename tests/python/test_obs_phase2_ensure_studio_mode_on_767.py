"""#767 Studio-Mode gate inversion -- unit tests for the obs_phase2.py
`ensure-studio-mode-on` action: Studio Mode is MUST-BE-ON on every broadcast box, imag included
(user hard rule 2026-07-15 -- without Studio Mode the multiview's Preview cell is dead). This
INVERTS the former #758 `ensure-studio-mode-off` step: the old Studio-ON render collapse on imag
(38-42fps/~23ms) was the pre-#767 distroav.so receiver teardown churn, not the preview pass --
with the keep-alive build imag measures 60.0fps/~1.8ms WITH Studio ON. `ensure_studio_mode_on`
must ALWAYS (idempotently) turn it ON -- never silently leave a stale OFF state that would hide a
Studio-ON render regression from the render-health preflight.

Same mocking pattern as tests/python/test_obs_phase2_open_projectors_758.py: patch `_rpc`/`_conn`
to avoid a live OBS connection, capture every call, assert on the DECISION.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_ensure_studio_mode_on", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_ensure_studio_mode_on"] = obs_phase2
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


def test_turns_on_studio_mode_when_currently_off(monkeypatch, capsys):
    calls = _patch(monkeypatch, {"studioModeEnabled": False})
    obs_phase2.ensure_studio_mode_on(_args(host="10.77.9.182", password=""))

    assert ("GetStudioModeEnabled", {}) in calls
    assert ("SetStudioModeEnabled", {"studioModeEnabled": True}) in calls
    out = capsys.readouterr().out
    assert "was OFF" in out and "turned ON" in out


def test_leaves_studio_mode_alone_when_already_on(monkeypatch, capsys):
    calls = _patch(monkeypatch, {"studioModeEnabled": True})
    obs_phase2.ensure_studio_mode_on(_args(host="10.77.9.182", password=""))

    assert ("GetStudioModeEnabled", {}) in calls
    assert ("SetStudioModeEnabled", {"studioModeEnabled": True}) not in calls, (
        "must not call SetStudioModeEnabled when it is already on -- no-op, no needless RPC"
    )
    out = capsys.readouterr().out
    assert "already ON" in out


def test_never_turns_studio_mode_off(monkeypatch):
    """The inversion's whole point: this action must never emit a studioModeEnabled=False write
    under ANY input state (the banned pre-#767 behavior)."""
    for state in (True, False):
        calls = _patch(monkeypatch, {"studioModeEnabled": state})
        obs_phase2.ensure_studio_mode_on(_args(host="10.77.9.182", password=""))
        assert ("SetStudioModeEnabled", {"studioModeEnabled": False}) not in calls


def test_a_failed_set_request_propagates_never_silently_continues(monkeypatch):
    def boom(op, payload):
        if op == "SetStudioModeEnabled":
            raise RuntimeError("SetStudioModeEnabled failed: {'result': False}")

    _patch(monkeypatch, {"studioModeEnabled": False}, rpc_side_effect=boom)
    with pytest.raises(RuntimeError):
        obs_phase2.ensure_studio_mode_on(_args(host="10.77.9.182", password=""))
