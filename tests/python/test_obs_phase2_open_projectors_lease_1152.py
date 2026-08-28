"""Issue 1152 M4 follow-up -- obs_phase2.py's open_projectors (the #758/#840 preflight GATE
called by recording-e2e.sh [0/8] -- NOT by verify-imag.sh, which has its own SEPARATE wmctrl-
based projector-count check (o) that is NOT touched by this fix, see the Review finding below)
must tolerate the DRM-lease mode.

With ~/.camera-box/drm-output.json ENABLED the vendored OBS leases the HDMI connector OUT of the
X layout and page-flips the Program onto it directly -- so GetMonitorList reports NO HDMI
monitor, and none is wanted (the Program is not an X window at all). Before this fix
open_projectors unconditionally required an HDMI monitor and raised
"no HDMI projector monitor detected" -- which is exactly the live [0/8] preflight failure this
ticket fixes (dispatch evidence: `monitors: ['eDP-1(0)']` after the owner's permanent DRM-lease
flip landed on imag-nb).

scripts/imag_scenes.py::projector() already carries this branch (M4, merged) -- this is the
SECOND, independent caller (obs_phase2.py::open_projectors) that never got it. The fix reuses
imag_scenes' OWN classifier pair (drm_output_lease_connector / _drm_output_config_text) via a
lazy import (the SAME pattern as the existing _measurement_pins_module() in obs_phase2.py) rather
than a second, divergent config-parsing grammar.

Covers:
  a. _drm_lease_connector_for_host() -- delegates to imag_scenes' real classifier, proving genuine
     reuse (not a re-implemented parser) by patching imag_scenes' OWN _drm_output_config_text.
  b. open_projectors() in lease mode, HDMI genuinely absent from X (the healthy lease state) --
     opens ONLY Multiview, never raises, logs plainly why no Program X projector is opened.
  c. open_projectors() lease-enabled but HDMI is STILL present in GetMonitorList -- a
     nonsensical/inconsistent state (the xrandr --off step never ran, or a stale config) -- MUST
     raise loud, never silently pass a state that could be a genuinely broken/dead projector.
  d. open_projectors() dormant (lease disabled/absent) -- behaviour BYTE-IDENTICAL to before,
     including the genuinely-unplugged-HDMI hard fail and the happy path with both projectors.

Same mocking pattern as tests/python/test_obs_phase2_open_projectors_758.py: patch `_rpc`/`_conn`
to avoid a live OBS connection, capture every call, assert on the DECISION.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
_MOD_PATH = REPO / "scripts" / "obs_phase2.py"
_SCENES_PATH = REPO / "scripts" / "imag_scenes.py"


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _obs_phase2_module():
    return _load(_MOD_PATH, "obs_phase2_open_projectors_lease_1152")


ENABLED = '{"enabled":true,"connector":"HDMI-1","argb":2105376}\n'
DISABLED = '{"enabled":false,"connector":"HDMI-1"}\n'

# The leased reality: GetMonitorList sees ONLY the panel (HDMI-1 left the X layout) -- the exact
# live signature from the dispatch's own preflight failure log.
_PANEL_ONLY = [{"monitorIndex": 0, "monitorName": "eDP-1(0)"}]

# #840: deliberately NOT index 0/1 -- proves the selection stays connector-TYPE driven even in
# the lease branch, not position-derived.
_MONITORS_REPLACEMENT_NOTEBOOK = [
    {"monitorIndex": 2, "monitorName": "eDP-1"},
    {"monitorIndex": 5, "monitorName": "HDMI-1"},
]


def _patch(monkeypatch, mod, monitors=None, rpc_side_effect=None, lease_connector=""):
    """Same shape as test_obs_phase2_open_projectors_758.py's _patch, plus a lease_connector
    knob that stubs _drm_lease_connector_for_host directly (isolates open_projectors' OWN
    decision logic from imag_scenes' transport/parsing, which is exercised separately in test a)."""
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

    monkeypatch.setattr(mod, "_rpc", fake_rpc)
    monkeypatch.setattr(mod, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(mod, "_drm_lease_connector_for_host", lambda host: lease_connector)
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


# ---------------------------------------------------------------------------
# a. _drm_lease_connector_for_host -- genuine reuse of imag_scenes' OWN classifier
# ---------------------------------------------------------------------------

def test_drm_lease_connector_for_host_reuses_imag_scenes_config_text(monkeypatch):
    mod = _obs_phase2_module()
    imag_scenes = mod._imag_scenes_module()
    monkeypatch.setattr(imag_scenes, "_drm_output_config_text", lambda host: ENABLED)
    assert mod._drm_lease_connector_for_host("10.77.9.182") == "HDMI-1"


def test_drm_lease_connector_for_host_dormant_when_disabled(monkeypatch):
    mod = _obs_phase2_module()
    imag_scenes = mod._imag_scenes_module()
    monkeypatch.setattr(imag_scenes, "_drm_output_config_text", lambda host: DISABLED)
    assert mod._drm_lease_connector_for_host("10.77.9.182") == ""


def test_drm_lease_connector_for_host_dormant_when_config_unreadable(monkeypatch):
    mod = _obs_phase2_module()
    imag_scenes = mod._imag_scenes_module()
    # _drm_output_config_text's own contract: any read failure degrades to "" -- confirm the
    # obs_phase2 wrapper never raises on top of that, it just forwards the dormant result.
    monkeypatch.setattr(imag_scenes, "_drm_output_config_text", lambda host: "")
    assert mod._drm_lease_connector_for_host("10.77.9.182") == ""


# ---------------------------------------------------------------------------
# b. open_projectors -- lease enabled, HDMI genuinely absent (the healthy state)
# ---------------------------------------------------------------------------

def test_lease_enabled_no_hdmi_opens_multiview_only_and_never_raises(monkeypatch, capsys):
    mod = _obs_phase2_module()
    calls = _patch(monkeypatch, mod, monitors=_PANEL_ONLY, lease_connector="HDMI-1")
    mod.open_projectors(_args(host="10.77.9.182", password=""))  # must NOT raise

    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 1, "lease mode must open ONLY the Multiview, never a Program X window"
    assert open_calls[0][1] == {
        "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW",
        "monitorIndex": 0,
    }
    out = capsys.readouterr().out
    assert "Multiview" in out
    assert "HDMI-1" in out and ("lease" in out.lower() or "DRM" in out), (
        "must log plainly WHY no Program X projector is opened"
    )


def test_lease_enabled_no_hdmi_still_requires_a_panel_monitor(monkeypatch):
    mod = _obs_phase2_module()
    _patch(monkeypatch, mod, monitors=[], lease_connector="HDMI-1")
    with pytest.raises(RuntimeError, match="(?i)panel"):
        mod.open_projectors(_args(host="10.77.9.182", password=""))


# ---------------------------------------------------------------------------
# c. open_projectors -- lease enabled but HDMI STILL in GetMonitorList (inconsistent -- fail loud)
# ---------------------------------------------------------------------------

def test_lease_enabled_but_hdmi_still_present_raises_loud(monkeypatch):
    mod = _obs_phase2_module()
    # Config says lease is ON, yet GetMonitorList still reports an HDMI monitor -- the connector
    # never actually left the X layout (xrandr --off step failed, or a stale config). Must NOT be
    # silently treated as "healthy lease" -- that is exactly the class of defect (something can
    # still land on HDMI) this ticket exists to prevent.
    calls = _patch(monkeypatch, mod, monitors=_MONITORS_REPLACEMENT_NOTEBOOK,
                   lease_connector="HDMI-1")
    with pytest.raises(RuntimeError, match="(?i)lease"):
        mod.open_projectors(_args(host="10.77.9.187", password=""))
    # Multiview must still have been opened (the panel is fine) before the inconsistency is caught.
    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 1
    assert open_calls[0][1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"


# ---------------------------------------------------------------------------
# d. open_projectors -- dormant (lease disabled/absent): behaviour BYTE-IDENTICAL to before
# ---------------------------------------------------------------------------

def test_dormant_derives_monitor_indices_from_connector_type_never_a_hardcoded_literal(
        monkeypatch):
    mod = _obs_phase2_module()
    calls = _patch(monkeypatch, mod, lease_connector="")
    mod.open_projectors(_args(host="10.77.9.187", password=""))

    open_calls = [c for c in calls if c[0] == "OpenVideoMixProjector"]
    assert len(open_calls) == 2
    multiview_calls = [c for c in open_calls
                       if c[1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"]
    program_calls = [c for c in open_calls
                     if c[1]["videoMixType"] == "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"]
    assert len(multiview_calls) == 1 and multiview_calls[0][1]["monitorIndex"] == 2
    assert len(program_calls) == 1 and program_calls[0][1]["monitorIndex"] == 5


def test_dormant_fails_loud_when_no_hdmi_monitor_is_present(monkeypatch):
    mod = _obs_phase2_module()
    _patch(monkeypatch, mod, monitors=[{"monitorIndex": 0, "monitorName": "eDP-1"}],
           lease_connector="")
    with pytest.raises(RuntimeError, match="(?i)hdmi"):
        mod.open_projectors(_args(host="10.77.9.187", password=""))


def test_dormant_fails_loud_when_no_panel_monitor_is_present(monkeypatch):
    mod = _obs_phase2_module()
    _patch(monkeypatch, mod, monitors=[{"monitorIndex": 0, "monitorName": "HDMI-1"}],
           lease_connector="")
    with pytest.raises(RuntimeError, match="(?i)panel"):
        mod.open_projectors(_args(host="10.77.9.187", password=""))


def test_dormant_connection_failure_still_labelled_handshake_auth(monkeypatch):
    mod = _obs_phase2_module()

    def boom_conn(host, password=""):
        raise ConnectionRefusedError("[Errno 111] Connection refused")

    monkeypatch.setattr(mod, "_conn", boom_conn)
    monkeypatch.setattr(mod, "_drm_lease_connector_for_host", lambda host: "")
    with pytest.raises(RuntimeError, match="(?i)handshake|auth"):
        mod.open_projectors(_args(host="10.77.9.182", password=""))
