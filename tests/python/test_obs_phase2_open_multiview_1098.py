"""#1098 -- unit tests for obs_phase2.py's `open-multiview` action: after a force-kill restart of
strih OBS (the issue-1093 receiver-wedge escalation) the operator's standing FULLSCREEN Multiview
projector is not restored (strih's SaveProjectors=true but SavedProjectors is EMPTY, and a
force-kill never repopulates it, so OBS restores nothing on the AHK respawn). `open-multiview`
actively re-opens it over OBS WebSocket.

Distinct from `open-projectors` (imag-nb dual-monitor: panel=Multiview + HDMI=Program, FAILS LOUD
without BOTH): strih is SINGLE-monitor and has NO Program projector, so this opens ONLY the
Multiview, on the DERIVED single monitor (#840 derive-not-hardcode), and NEVER fails loud on a
missing HDMI monitor. Same mocking pattern as test_obs_phase2_open_projectors_758.py: patch
`_rpc`/`_conn`, capture calls, assert on the DECISION.
"""
import argparse
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_open_multiview", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_open_multiview"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# The live strih topology (verified 2026-08-18): a SINGLE monitor at index 0, NON-HDMI name.
_STRIH_SINGLE = [{"monitorIndex": 0, "monitorName": "U27P2G6B(0)",
                  "monitorPositionX": 0, "monitorPositionY": 0,
                  "monitorWidth": 2560, "monitorHeight": 1440}]


def _patch(monkeypatch, monitors):
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        if op == "GetMonitorList":
            return {"monitors": monitors}
        return {}

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    return calls


def _args(**kw):
    kw.setdefault("monitor_index", -999)  # the "derive" sentinel
    return argparse.Namespace(**kw)


# ---- the pure monitor-index derivation --------------------------------------------------------

def test_multiview_monitor_index_picks_the_single_monitor():
    assert obs_phase2._multiview_monitor_index(_STRIH_SINGLE) == 0


def test_multiview_monitor_index_prefers_the_primary_at_origin_over_list_order():
    mons = [{"monitorIndex": 0, "monitorPositionX": 1920, "monitorPositionY": 0},
            {"monitorIndex": 1, "monitorPositionX": 0, "monitorPositionY": 0}]
    assert obs_phase2._multiview_monitor_index(mons) == 1, "the origin/primary monitor wins, not index 0"


def test_multiview_monitor_index_override_wins_including_windowed_minus_one():
    assert obs_phase2._multiview_monitor_index(_STRIH_SINGLE, -1) == -1
    assert obs_phase2._multiview_monitor_index(_STRIH_SINGLE, 3) == 3


def test_multiview_monitor_index_empty_list_never_crashes():
    assert obs_phase2._multiview_monitor_index([]) == 0


def test_multiview_monitor_index_falls_back_to_first_when_none_at_origin():
    mons = [{"monitorIndex": 3, "monitorPositionX": 100, "monitorPositionY": 0},
            {"monitorIndex": 5, "monitorPositionX": 2560, "monitorPositionY": 0}]
    assert obs_phase2._multiview_monitor_index(mons) == 3


# ---- the subcommand ---------------------------------------------------------------------------

def test_opens_only_the_multiview_projector_on_the_derived_single_monitor(monkeypatch, capsys):
    calls = _patch(monkeypatch, _STRIH_SINGLE)
    obs_phase2.open_multiview(_args(host="10.77.9.202", password=""))

    assert calls[0] == ("GetMonitorList", {})
    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 1, "single-monitor box: EXACTLY one projector (Multiview only, no Program)"
    assert open_calls[0][1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"
    assert open_calls[0][1]["monitorIndex"] == 0, "must open on the derived single monitor (index 0)"
    # Never a Program projector on strih -- that is the open_projectors path, which strih must not use.
    assert not any(c[1].get("videoMixType") == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"
                   for c in open_calls)
    out = capsys.readouterr().out
    assert "monitorIndex 0" in out and "U27P2G6B(0)" in out


def test_never_fails_loud_on_a_single_non_hdmi_monitor(monkeypatch):
    # open_projectors would RAISE "no HDMI projector monitor detected" here; open_multiview must NOT
    # -- strih legitimately has one non-HDMI monitor and no Program projector.
    calls = _patch(monkeypatch, _STRIH_SINGLE)
    obs_phase2.open_multiview(_args(host="10.77.9.202", password=""))  # no exception
    assert any(c[0] == "OpenVideoMixProjector" for c in calls)


def test_explicit_monitor_index_overrides_the_derivation(monkeypatch):
    calls = _patch(monkeypatch, _STRIH_SINGLE)
    obs_phase2.open_multiview(_args(host="10.77.9.202", password="", monitor_index=-1))
    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert open_calls[0][1]["monitorIndex"] == -1, "an explicit --monitor-index (windowed -1 here) wins"
