"""#758/#840 -- unit tests for obs_phase2.py's `open-projectors` action: imag-nb's Multiview AND
Program projectors must be OPEN before ANY run starts (the user's explicit, binding requirement --
a run must NEVER begin with Multiview closed). obs-websocket 5.x has no "is a projector open"
introspection request, so open_projectors ALWAYS (idempotently) opens both via
OpenVideoMixProjector rather than check-then-open.

#840 root-cause rewrite: the monitor mapping used to be HARDCODED (`monitorIndex 0 = DP-0 ->
Multiview`, `monitorIndex 1 = HDMI-0 -> Program`) -- it only worked "by luck" because the index
ORDER happened to match on the incumbent box. The replacement notebook (10.77.9.187) enumerates
`eDP-1`/`HDMI-1` instead of `DP-0`/`HDMI-0`, and nothing here actually checked the connector TYPE,
so a box that ever enumerates HDMI as index 0 would silently send the Program feed to the panel
and Multiview to the projector. The fix mirrors `imag_scenes.py::projector()`'s existing, already-
correct selection rule: pick the monitor whose `monitorName` CONTAINS "HDMI" for the Program
projector, and the one that does NOT for the Multiview projector -- derived from a live
`GetMonitorList` call, never a literal index. Unlike `imag_scenes.py::projector()` (an operator
convenience script that only WARNs on a missing panel), this function is a preflight/verify GATE
(recording-e2e.sh `[0/8]`, verify-imag.sh) -- it FAILS LOUD (raises) when EITHER expected output is
absent, never silently continues.

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


# #840: deliberately NOT index 0/1 -- the panel is index 2 and HDMI is index 5, proving the
# selection is driven by connector TYPE, never by position in the list.
_MONITORS_REPLACEMENT_NOTEBOOK = [
    {"monitorIndex": 2, "monitorName": "eDP-1"},
    {"monitorIndex": 5, "monitorName": "HDMI-1"},
]


def _patch(monkeypatch, monitors=None, rpc_side_effect=None):
    """Patches `_rpc`/`_conn` to avoid a real websocket connection. Returns the captured calls
    list. `monitors`, if given, is the list returned for a GetMonitorList request. `rpc_side_effect`
    (called with (op, payload) for every OTHER _rpc call) may raise to simulate a failed request.

    issue 1152 M4 follow-up (review finding): also stubs `_drm_lease_connector_for_host` to the
    dormant "" -- WITHOUT this, open_projectors calls the real helper, which shells out over ssh
    (via imag_scenes._drm_output_config_text) to read the box's OWN live drm-output.json. This
    file's tests are about the DORMANT monitor-selection contract and must never depend on, or
    reach out to, live rig state -- every test here stays dormant unless it explicitly overrides
    this stub."""
    calls = []
    mons = monitors if monitors is not None else _MONITORS_REPLACEMENT_NOTEBOOK

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        if op == "GetMonitorList":
            return {"monitors": mons}
        if rpc_side_effect is not None:
            rpc_side_effect(op, payload or {})
        return {}

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "_drm_lease_connector_for_host", lambda host: "")
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


def test_derives_monitor_indices_from_connector_type_never_a_hardcoded_literal(monkeypatch, capsys):
    calls = _patch(monkeypatch)
    obs_phase2.open_projectors(_args(host="10.77.9.187", password=""))

    assert calls[0] == ("GetMonitorList", {})
    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 2

    multiview_calls = [c for c in open_calls
                       if c[1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"]
    program_calls = [c for c in open_calls
                     if c[1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"]
    assert len(multiview_calls) == 1 and multiview_calls[0][1]["monitorIndex"] == 2, (
        "Multiview must open on the PANEL monitor (non-HDMI) -- index 2 here, never a literal 0"
    )
    assert len(program_calls) == 1 and program_calls[0][1]["monitorIndex"] == 5, (
        "Program must open on the HDMI monitor -- index 5 here, never a literal 1"
    )

    out = capsys.readouterr().out
    assert "monitorIndex 2" in out and "eDP-1" in out
    assert "monitorIndex 5" in out and "HDMI-1" in out


def test_fails_loud_when_no_hdmi_monitor_is_present(monkeypatch):
    _patch(monkeypatch, monitors=[{"monitorIndex": 0, "monitorName": "eDP-1"}])
    with pytest.raises(RuntimeError, match="(?i)hdmi"):
        obs_phase2.open_projectors(_args(host="10.77.9.187", password=""))


def test_fails_loud_when_no_panel_monitor_is_present(monkeypatch):
    # Every reported monitor is HDMI -- there is nothing left for the Multiview projector.
    _patch(monkeypatch, monitors=[{"monitorIndex": 0, "monitorName": "HDMI-1"}])
    with pytest.raises(RuntimeError, match="(?i)panel"):
        obs_phase2.open_projectors(_args(host="10.77.9.187", password=""))


def test_a_failed_open_request_propagates_never_silently_continues(monkeypatch):
    def boom(op, payload):
        if op == "OpenVideoMixProjector" and payload.get("videoMixType") == \
                "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW":
            raise RuntimeError("OpenVideoMixProjector failed: {'result': False}")

    _patch(monkeypatch, rpc_side_effect=boom)
    with pytest.raises(RuntimeError):
        obs_phase2.open_projectors(_args(host="10.77.9.187", password=""))


def test_connection_failure_is_labelled_handshake_auth_never_a_bare_traceback(monkeypatch):
    """#882: the imag-nb outage this issue investigates showed a hardcoded, WRONG fallback message
    ("check DP-0/HDMI-0 are connected monitors") on ANY failure, including a connection-level one
    that has nothing to do with monitors at all. A failure to even establish the WebSocket session
    (process down, port not listening, wrong password) must be raised with a message that clearly
    names it as a CONNECTION/handshake failure -- never conflated with the later "no matching
    monitor" RuntimeErrors (which already correctly name the real connector types)."""

    def boom_conn(host, password=""):
        raise ConnectionRefusedError("[Errno 111] Connection refused")

    monkeypatch.setattr(obs_phase2, "_conn", boom_conn)
    with pytest.raises(RuntimeError, match="(?i)handshake|auth"):
        obs_phase2.open_projectors(_args(host="10.77.9.182", password=""))


def test_program_projector_never_attempted_after_multiview_raised(monkeypatch):
    # A failure on the Multiview open must abort immediately -- Program must never be attempted
    # in a way that hides the Multiview failure (no swallow-and-continue).
    calls = []
    mons = _MONITORS_REPLACEMENT_NOTEBOOK

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        if op == "GetMonitorList":
            return {"monitors": mons}
        raise RuntimeError("OBS unreachable")

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())

    with pytest.raises(RuntimeError):
        obs_phase2.open_projectors(_args(host="10.77.9.187", password=""))
    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 1, "must abort on the FIRST OpenVideoMixProjector failure, never continue to the second"
