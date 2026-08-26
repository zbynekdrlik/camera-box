"""Issue 1152 M4 -- imag_scenes.py projector() must tolerate the DRM-lease mode.

With ~/.camera-box/drm-output.json ENABLED the vendored OBS leases the HDMI connector OUT of the
X layout and page-flips the Program onto it directly -- so there is NO HDMI monitor for the X
Program projector and none is wanted. Before this fix projector() sys.exit'd ("FAIL: no HDMI
projector monitor detected"), which under imag-obs-start.sh's `set -euo pipefail` failed the
imag-obs.service unit and CRASH-LOOPED a healthy OBS on the live projection (the live 2026-08-26
M1 runbook gotcha: restart counter climbing 5->8 every ~13 s).

Covers, with NO live OBS/rig/X (the same fake-ws + importlib convention as
test_imag_scenes_projector_idempotent_769.py):
  a. drm_output_lease_enabled() -- pure truth table over the config JSON text.
  b. _drm_output_config_text() -- reads the BOX's own config: local open on the loopback path,
     the _ssh_base transport for a dev1 --host <ip> call (NEVER dev1's own file); any failure
     degrades to "" (dormant default).
  c. projector() in lease mode -- opens ONLY the panel Multiview, closes EVERY restored X
     "Projector - Program" stray, prints the loud mode line, and NEVER raises SystemExit.
  d. projector() dormant -- behaviour unchanged, incl. the genuinely-unplugged-HDMI fail-exit.
"""
import importlib.util
import pathlib
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _scenes_module():
    return _load(REPO / "scripts" / "imag_scenes.py", "imag_scenes_drm_lease_1152_under_test")


ENABLED = '{"enabled":true,"connector":"HDMI-1","argb":2105376}\n'
DISABLED = '{"enabled":false,"connector":"HDMI-1"}\n'

# The leased reality: OBS's GetMonitorList sees ONLY the panel (HDMI-1 left the X layout).
_PANEL_ONLY = [{"monitorIndex": 0, "monitorName": "eDP-1(0)"}]

# A launch-restore stray: OBS recreated the saved Program projector WINDOWED on the panel.
_WMCTRL_WITH_RESTORED_PROGRAM = (
    "0x01a00003  0 imag-nb Ubuntu\n"
    "0x00c00006  0 imag-nb OBS Studio 32.1.2\n"
    "0x00c0c883  0 imag-nb Projector - Multiview\n"
    "0x00c0c88a  0 imag-nb Projector - Program\n"
    "0x00c0c99b  0 imag-nb Projector - Program\n"
)


class LeasedBoxObs:
    """Panel-only monitor list (the leased state). Records OpenVideoMixProjector calls."""

    def __init__(self, monitors=None):
        self.opens = []
        self.monitors = _PANEL_ONLY if monitors is None else monitors

    def req(self, rtype, payload=None, ignore_err=False):
        if rtype == "GetMonitorList":
            return {"monitors": self.monitors}
        if rtype == "OpenVideoMixProjector":
            self.opens.append((payload["videoMixType"], payload["monitorIndex"]))
        return {}


# ---------------------------------------------------------------------------
# a. drm_output_lease_enabled -- pure truth table
# ---------------------------------------------------------------------------

def test_lease_enabled_truth_table():
    mod = _scenes_module()
    assert mod.drm_output_lease_enabled(ENABLED) is True
    assert mod.drm_output_lease_enabled(DISABLED) is False
    assert mod.drm_output_lease_enabled("") is False
    assert mod.drm_output_lease_enabled(None) is False
    assert mod.drm_output_lease_enabled("{not json") is False
    assert mod.drm_output_lease_enabled('{"connector":"HDMI-1"}') is False
    # a STRING "true" is not the boolean contract the C module reads -- stay dormant
    assert mod.drm_output_lease_enabled('{"enabled":"true"}') is False
    # a non-object JSON body must degrade to dormant, never raise
    assert mod.drm_output_lease_enabled("[1,2]") is False
    # the C contract requires a non-empty "connector" too (obs-drm-output.c is dormant without
    # one) -- enabled-without-connector must NOT arm (review: the wrapper once defaulted HDMI-1
    # here and blanked a connector the C never took over -> dark projector)
    assert mod.drm_output_lease_enabled('{"enabled":true}') is False


def test_lease_connector_truth_table():
    """The ONE decision grammar every consumer shares (wrapper + seeder), mirroring the C."""
    mod = _scenes_module()
    assert mod.drm_output_lease_connector(ENABLED) == "HDMI-1"
    assert mod.drm_output_lease_connector(DISABLED) == ""
    assert mod.drm_output_lease_connector('{"enabled":true}') == ""
    assert mod.drm_output_lease_connector('{"enabled":true,"connector":""}') == ""
    assert mod.drm_output_lease_connector('{"enabled":true,"connector":7}') == ""
    # the review's exact divergence vector: invalid JSON carrying the grep substring -- the C
    # parser is dormant, so the shared classifier must be dormant too (a bash-grep reading here
    # once armed the wrapper alone and crash-looped the unit)
    assert mod.drm_output_lease_connector('{"enabled":true,}') == ""
    # a pretty-printed MULTI-LINE but VALID config arms (json is not line-based)
    assert mod.drm_output_lease_connector(
        '{\n  "enabled": true,\n  "connector": "HDMI-1"\n}\n') == "HDMI-1"


# ---------------------------------------------------------------------------
# b. _drm_output_config_text -- reads the BOX's file, local vs remote transport
# ---------------------------------------------------------------------------

def test_config_text_local_reads_the_file(monkeypatch, tmp_path):
    mod = _scenes_module()
    conf = tmp_path / "drm-output.json"
    conf.write_text(ENABLED)
    monkeypatch.setattr(mod, "DRM_OUTPUT_CONF", str(conf))
    assert mod._drm_output_config_text("127.0.0.1") == ENABLED


def test_config_text_local_missing_file_is_dormant(monkeypatch, tmp_path):
    mod = _scenes_module()
    monkeypatch.setattr(mod, "DRM_OUTPUT_CONF", str(tmp_path / "absent.json"))
    assert mod._drm_output_config_text("127.0.0.1") == ""


def test_config_text_remote_uses_the_ssh_transport_never_the_local_file(monkeypatch):
    mod = _scenes_module()
    seen = {}

    class FakeDone:
        returncode = 0
        stdout = ENABLED
        stderr = ""

    def fake_run(argv, **kw):
        seen["argv"] = argv
        return FakeDone()

    monkeypatch.setattr(mod.subprocess, "run", fake_run)
    assert mod._drm_output_config_text("10.77.9.182") == ENABLED
    # routed through the SAME sshpass transport the wmctrl helpers use, cat-ing the BOX's file
    assert "sshpass" in seen["argv"][0]
    assert any("drm-output.json" in a for a in seen["argv"])


def test_config_text_remote_failure_degrades_to_dormant(monkeypatch):
    mod = _scenes_module()

    def boom(argv, **kw):
        raise OSError("no ssh here")

    monkeypatch.setattr(mod.subprocess, "run", boom)
    assert mod._drm_output_config_text("10.77.9.182") == ""


# ---------------------------------------------------------------------------
# c. projector() in LEASE mode -- Multiview-only, Program strays closed, loud, never exits
# ---------------------------------------------------------------------------

def test_projector_lease_mode_opens_only_the_panel_multiview(monkeypatch, capsys):
    mod = _scenes_module()
    obs = LeasedBoxObs()
    closed = []
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: ENABLED)
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: _WMCTRL_WITH_RESTORED_PROGRAM)
    monkeypatch.setattr(mod, "_wmctrl_close_local", lambda win_id: closed.append(win_id))
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod.projector(obs, "127.0.0.1")  # must NOT raise SystemExit despite no HDMI monitor
    assert obs.opens == [("OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW", 0)], (
        "lease mode must open ONLY the panel Multiview -- the Program lives on the DRM scanout"
    )
    # BOTH restored X Program projector windows are strays in lease mode -- closed, none kept
    assert "0x00c0c88a" in closed and "0x00c0c99b" in closed
    out = capsys.readouterr().out
    assert "drm-lease mode ENABLED" in out, "the skip must be LOUD, never silent"


def test_projector_lease_mode_missing_wmctrl_warns_and_never_raises(monkeypatch, capsys):
    mod = _scenes_module()
    obs = LeasedBoxObs()
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: ENABLED)
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: None)
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod.projector(obs, "127.0.0.1")  # must NOT raise
    assert obs.opens == [("OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW", 0)]
    out = capsys.readouterr().out
    assert "wmctrl" in out, "a missing wmctrl warns by NAME (never read as 'no windows')"


def test_projector_lease_mode_no_panel_warns_and_still_never_raises(monkeypatch, capsys):
    mod = _scenes_module()
    # degenerate: NO monitor at all (headless debug) -- still no SystemExit on the start path
    obs = LeasedBoxObs(monitors=[])
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: ENABLED)
    monkeypatch.setattr(mod, "_wmctrl_list_local", lambda: "")
    monkeypatch.setattr(mod.time, "sleep", lambda *_a, **_k: None)
    mod.projector(obs, "127.0.0.1")
    assert obs.opens == []
    out = capsys.readouterr().out
    assert "WARN" in out


# ---------------------------------------------------------------------------
# d. projector() DORMANT -- unchanged behaviour, incl. the genuine no-HDMI fail
# ---------------------------------------------------------------------------

def test_projector_dormant_no_hdmi_still_fails_loud(monkeypatch):
    mod = _scenes_module()
    obs = LeasedBoxObs()  # no HDMI monitor, but the config is dormant -> genuinely unplugged
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: "")
    with pytest.raises(SystemExit):
        mod.projector(obs, "127.0.0.1")


def test_projector_dormant_with_hdmi_opens_both_as_before(monkeypatch):
    mod = _scenes_module()
    obs = LeasedBoxObs(monitors=[
        {"monitorIndex": 0, "monitorName": "DP-0(0)"},
        {"monitorIndex": 1, "monitorName": "HDMI-0(1)"},
    ])
    healed = []
    monkeypatch.setattr(mod, "_drm_output_config_text", lambda host: "")
    monkeypatch.setattr(mod, "_heal_projector_strays", lambda host, kinds: healed.extend(kinds))
    mod.projector(obs, "127.0.0.1")
    kinds = {t for t, _ in obs.opens}
    assert kinds == {
        "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM",
        "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW",
    }
    assert healed == ["Program", "Multiview"]
