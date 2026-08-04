"""#901 gap 3 — unit tests for the obs_phase2 `program-rendered-input` subcommand.

rig-mode.sh's fixed burn targets (STRIH_PROG_SOURCE="NDI cam1", STREAM_PROG_SOURCE="NDI 2ME PGM")
can silently diverge from whatever scene is ACTUALLY live on program (live evidence, 2026-08-04:
strih's program scene was 'Cam 2' -> 'NDI cam2' rendered, but the burn landed on 'NDI cam1' since
that was the fixed default). `program-rendered-input` resolves the source/input name OBS is
ACTUALLY rendering in the current (or a given) program scene, via GetSceneItemList, so the caller
can burn/verify the input that is genuinely visible right now, not a hardcoded guess.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_pri", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_pri"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# --- pure helper -----------------------------------------------------------------------------

def test_first_enabled_scene_item_source_picks_the_enabled_one():
    items = [
        {"sourceName": "NDI cam2", "sceneItemEnabled": True, "sceneItemIndex": 0},
        {"sourceName": "overlay-graphic", "sceneItemEnabled": False, "sceneItemIndex": 1},
    ]
    assert obs_phase2._first_enabled_scene_item_source(items) == "NDI cam2"


def test_first_enabled_scene_item_source_skips_disabled_items():
    items = [
        {"sourceName": "dead-source", "sceneItemEnabled": False, "sceneItemIndex": 0},
        {"sourceName": "NDI cam4", "sceneItemEnabled": True, "sceneItemIndex": 1},
    ]
    assert obs_phase2._first_enabled_scene_item_source(items) == "NDI cam4"


def test_first_enabled_scene_item_source_defaults_enabled_true_when_key_absent():
    # A real GetSceneItemList response always carries sceneItemEnabled, but be defensive: an
    # item missing the key must be treated as enabled (never silently skipped).
    items = [{"sourceName": "NDI cam1"}]
    assert obs_phase2._first_enabled_scene_item_source(items) == "NDI cam1"


def test_first_enabled_scene_item_source_returns_none_when_nothing_enabled():
    items = [{"sourceName": "dead-source", "sceneItemEnabled": False}]
    assert obs_phase2._first_enabled_scene_item_source(items) is None


def test_first_enabled_scene_item_source_returns_none_on_empty_list():
    assert obs_phase2._first_enabled_scene_item_source([]) is None


# --- CLI wiring --------------------------------------------------------------------------------

def test_program_rendered_input_function_exists():
    assert callable(obs_phase2.program_rendered_input)


def test_program_rendered_input_subcommand_parses_and_dispatches(monkeypatch):
    captured = {}

    def fake(a):
        captured["host"] = a.host
        captured["scene"] = a.scene

    monkeypatch.setattr(obs_phase2, "program_rendered_input", fake)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "program-rendered-input", "--host", "10.77.9.202", "--scene", "Cam 2"],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "scene": "Cam 2"}


def test_program_rendered_input_scene_is_optional(monkeypatch):
    captured = {}

    def fake(a):
        captured["scene"] = a.scene

    monkeypatch.setattr(obs_phase2, "program_rendered_input", fake)
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "program-rendered-input", "--host", "h"])
    obs_phase2.main()
    assert captured["scene"] == ""


# --- handler, live RPC path mocked --------------------------------------------------------------

class _FakeWS:
    def close(self):
        pass


def _fake_conn_rpc(current_program_scene, scene_items_by_scene):
    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "GetCurrentProgramScene":
            return {"currentProgramSceneName": current_program_scene}
        if rtype == "GetSceneItemList":
            scene = (rdata or {}).get("sceneName")
            return {"sceneItems": scene_items_by_scene.get(scene, [])}
        raise AssertionError(f"unexpected rpc {rtype}")

    return fake_conn, fake_rpc


def test_program_rendered_input_resolves_current_program_scene_when_scene_omitted(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_conn_rpc(
        "Cam 2", {"Cam 2": [{"sourceName": "NDI cam2", "sceneItemEnabled": True}]}
    )
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="")
    obs_phase2.program_rendered_input(a)
    out = capsys.readouterr().out.strip()
    assert out == "NDI cam2"


def test_program_rendered_input_uses_given_scene_when_provided(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_conn_rpc(
        "Cam 1",  # current program is Cam 1, but caller explicitly asked about Cam 4
        {"Cam 4": [{"sourceName": "NDI cam4", "sceneItemEnabled": True}]},
    )
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="Cam 4")
    obs_phase2.program_rendered_input(a)
    out = capsys.readouterr().out.strip()
    assert out == "NDI cam4"


def test_program_rendered_input_fails_loud_when_scene_has_no_enabled_item(monkeypatch):
    fake_conn, fake_rpc = _fake_conn_rpc(
        "Cam 5", {"Cam 5": [{"sourceName": "dead", "sceneItemEnabled": False}]}
    )
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="")
    with pytest.raises(SystemExit):
        obs_phase2.program_rendered_input(a)


def test_first_enabled_scene_item_source_skips_audio_only_inputs():
    # Live evidence 2026-08-04 (first hardware run of the #901 chain-verify): strih's 'Cam 2'
    # scene lists 'ASIO zvuk' (asio_input_capture) BEFORE 'NDI cam2' -- the resolver returned the
    # audio input and the burn filter could not attach ("[burn] FAIL: burn filter did not attach
    # to 'ASIO zvuk'"). An audio-only input can never carry the video burn; the resolver must
    # return the first enabled item that can actually RENDER.
    items = [
        {"sourceName": "ASIO zvuk", "sceneItemEnabled": True, "inputKind": "asio_input_capture"},
        {"sourceName": "NDI cam2", "sceneItemEnabled": True, "inputKind": "ndi_source"},
    ]
    assert obs_phase2._first_enabled_scene_item_source(items) == "NDI cam2"


def test_first_enabled_scene_item_source_falls_back_when_only_audio_is_enabled():
    # Defensive: a scene with ONLY audio items still returns SOMETHING (the caller's burn attach
    # then warns loudly) rather than None (which would abort the whole chain-verify).
    items = [
        {"sourceName": "ASIO zvuk", "sceneItemEnabled": True, "inputKind": "asio_input_capture"},
    ]
    assert obs_phase2._first_enabled_scene_item_source(items) == "ASIO zvuk"
