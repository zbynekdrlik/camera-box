"""issue 1380 -- the stream OBS `Development` scene (the production scene `PRO` nested as a source).

Owner request 27.9.2026: development must never program the owner's production scene `PRO` on the
stream OBS. The tooling programs its own `Development` scene, which holds `PRO` as a nested scene
source (same pixels, the same warm `NDI 2ME PGM` receiver), and EVENT mode puts `PRO` back.

These tests pin, with a fake `_rpc` (no live OBS):
  * the pure seeder decision `dev_scene_plan` (missing scene / scene present / item present /
    production scene missing / the operator hid the nested item);
  * `ensure_dev_scene` writes ONLY to the development scene and never to `PRO`, and is a no-op on
    the live 27.9.2026 state (Development present, its one item = the scene PRO);
  * the `dev-scene` CLI wiring and its defaults, pinned to the bash lib defaults;
  * `switch` skips SetCurrentProgramScene when the target is already on program (the #343
    same-scene hazard), and takes an optional `--min-mean`;
  * `program-rendered-input` descends into a nested scene source, so the TEST burn resolves
    `NDI 2ME PGM` through `Development -> PRO` and never tries to burn the scene `PRO`;
  * the bash lib `stream_dev_scene_ensure` / `stream_program_scene_read` call shapes.
"""
import importlib.util
import os
import pathlib
import re
import subprocess
import sys
import types

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MOD_PATH = _ROOT / "scripts" / "obs_phase2.py"
_LIB = _ROOT / "scripts" / "lib" / "stream-dev-scene.sh"
_spec = importlib.util.spec_from_file_location("obs_phase2_dev_scene_1380", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_dev_scene_1380"] = obs_phase2
_spec.loader.exec_module(obs_phase2)

_WRITE_PREFIXES = ("Create", "Set", "Remove", "Duplicate")


class FakeWS:
    def close(self):
        pass


def _nested_item(name="PRO", enabled=True):
    return {"sourceName": name, "sourceType": "OBS_SOURCE_TYPE_SCENE", "inputKind": None,
            "sceneItemEnabled": enabled, "sceneItemId": 1}


def _input_item(name, kind, enabled=True):
    return {"sourceName": name, "sourceType": "OBS_SOURCE_TYPE_INPUT", "inputKind": kind,
            "sceneItemEnabled": enabled}


# The live stream PRO item list, read 27.9.2026 (bottom to top, the GetSceneItemList order).
_PRO_ITEMS = [
    _input_item("fallback repro", "asio_input_capture"),
    _input_item("mbc", "asio_input_capture"),
    _input_item("NDI 2ME PGM", "ndi_source"),
    _input_item("Zaloha kamera", "ndi_source"),
    _input_item("NDIA cg stream", "ndi_source"),
    _input_item("audio only", "image_source"),
]


def _fake_obs(scenes, items_by_scene, program="PRO"):
    """A fake `_rpc` over an in-memory OBS: scene list + per-scene items. Records every call."""
    state = {"scenes": list(scenes), "items": {k: list(v) for k, v in items_by_scene.items()},
             "program": program}
    calls = []

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        rdata = rdata or {}
        calls.append({"op": rtype, "data": rdata, "ignore_err": ignore_err})
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in state["scenes"]],
                    "currentProgramSceneName": state["program"]}
        if rtype == "GetSceneItemList":
            name = rdata["sceneName"]
            if name not in state["scenes"]:
                raise RuntimeError(f"GetSceneItemList failed: no scene {name}")
            return {"sceneItems": state["items"].get(name, [])}
        if rtype == "CreateScene":
            state["scenes"].append(rdata["sceneName"])
            state["items"][rdata["sceneName"]] = []
            return {}
        if rtype == "CreateSceneItem":
            state["items"][rdata["sceneName"]].append(_nested_item(rdata["sourceName"]))
            return {"sceneItemId": 7}
        if rtype == "GetCurrentProgramScene":
            return {"currentProgramSceneName": state["program"]}
        if rtype == "SetCurrentProgramScene":
            state["program"] = rdata["sceneName"]
            return {}
        return {}

    return fake_rpc, calls, state


def _writes(calls):
    return [c for c in calls if c["op"].startswith(_WRITE_PREFIXES)]


# --- the pure decision ---------------------------------------------------------------------------

def test_plan_creates_the_scene_and_the_nested_item_when_the_scene_is_missing():
    plan = obs_phase2.dev_scene_plan(["PRO", "POST", "PRE"], None, "Development", "PRO")
    assert plan.actions == ["create_scene", "add_nested"]
    assert plan.nested_hidden is False


def test_plan_adds_only_the_nested_item_when_the_scene_exists_without_it():
    plan = obs_phase2.dev_scene_plan(["PRO", "Development"], [], "Development", "PRO")
    assert plan.actions == ["add_nested"]


def test_plan_is_a_noop_when_the_scene_already_nests_the_production_scene():
    plan = obs_phase2.dev_scene_plan(
        ["PRO", "POST", "Development", "PRE"], [_nested_item()], "Development", "PRO")
    assert plan.actions == []
    assert plan.nested_hidden is False


def test_plan_never_touches_an_operator_hidden_nested_item_but_reports_it():
    # Operator-wins: a hidden PRO item is the operator's choice -- no write, but it is reported so
    # the caller can say why the development program would render black.
    plan = obs_phase2.dev_scene_plan(
        ["PRO", "Development"], [_nested_item(enabled=False)], "Development", "PRO")
    assert plan.actions == []
    assert plan.nested_hidden is True


def test_plan_keeps_extra_operator_items_untouched():
    items = [_input_item("timer", "text_ft2_source_v2"), _nested_item()]
    plan = obs_phase2.dev_scene_plan(["PRO", "Development"], items, "Development", "PRO")
    assert plan.actions == []


def test_plan_fails_loud_when_the_production_scene_is_missing():
    with pytest.raises(obs_phase2.DevSceneError, match="PRO"):
        obs_phase2.dev_scene_plan(["POST", "PRE"], None, "Development", "PRO")


def test_plan_refuses_a_development_scene_equal_to_the_production_scene():
    with pytest.raises(obs_phase2.DevSceneError):
        obs_phase2.dev_scene_plan(["PRO"], [], "PRO", "PRO")


def test_plan_refuses_empty_scene_names():
    with pytest.raises(obs_phase2.DevSceneError):
        obs_phase2.dev_scene_plan(["PRO"], None, "", "PRO")
    with pytest.raises(obs_phase2.DevSceneError):
        obs_phase2.dev_scene_plan(["PRO"], None, "Development", "")


# --- ensure_dev_scene over a fake OBS ------------------------------------------------------------

def test_ensure_is_a_noop_on_the_live_27_9_state(monkeypatch):
    fake, calls, _ = _fake_obs(["PRO", "POST", "Development", "PRE"],
                               {"PRO": _PRO_ITEMS, "Development": [_nested_item()]})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    plan = obs_phase2.ensure_dev_scene(FakeWS(), "Development", "PRO")
    assert plan.actions == []
    assert _writes(calls) == []


def test_ensure_creates_the_scene_and_nests_pro_writing_only_to_development(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "POST", "PRE"], {"PRO": _PRO_ITEMS})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    plan = obs_phase2.ensure_dev_scene(FakeWS(), "Development", "PRO")
    assert plan.actions == ["create_scene", "add_nested"]
    writes = _writes(calls)
    assert [w["op"] for w in writes] == ["CreateScene", "CreateSceneItem"]
    assert all(w["data"].get("sceneName") == "Development" for w in writes)
    assert writes[1]["data"]["sourceName"] == "PRO"
    # Fail loud: no write is sent with ignore_err.
    assert all(w["ignore_err"] is False for w in writes)
    # PRO's own item list is byte-identical afterwards.
    assert state["items"]["PRO"] == _PRO_ITEMS


def test_ensure_never_reads_or_writes_items_of_the_production_scene(monkeypatch):
    fake, calls, _ = _fake_obs(["PRO", "Development"], {"PRO": _PRO_ITEMS, "Development": []})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    obs_phase2.ensure_dev_scene(FakeWS(), "Development", "PRO")
    assert not any(c["data"].get("sceneName") == "PRO" for c in calls)


def test_ensure_fails_loud_without_any_write_when_pro_is_missing(monkeypatch):
    fake, calls, _ = _fake_obs(["POST", "PRE"], {})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    with pytest.raises(obs_phase2.DevSceneError):
        obs_phase2.ensure_dev_scene(FakeWS(), "Development", "PRO")
    assert _writes(calls) == []


def test_dev_scene_cli_exits_non_zero_when_pro_is_missing(monkeypatch):
    fake, _, _ = _fake_obs(["POST"], {})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "dev-scene", "--host", "10.77.9.204"])
    with pytest.raises(SystemExit) as exc:
        obs_phase2.main()
    assert exc.value.code not in (0, None)


def test_dev_scene_cli_prints_the_scene_it_ensured(monkeypatch, capsys):
    fake, _, _ = _fake_obs(["PRO", "Development"], {"Development": [_nested_item()]})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "dev-scene", "--host", "10.77.9.204"])
    obs_phase2.main()
    out = capsys.readouterr().out.strip().splitlines()
    assert out[-1] == "DEV_SCENE=Development created=0 nested_added=0"


def test_dev_scene_cli_defaults(monkeypatch):
    captured = {}
    monkeypatch.setattr(obs_phase2, "dev_scene", lambda a: captured.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "dev-scene", "--host", "h"])
    obs_phase2.main()
    assert captured["scene"] == "Development"
    assert captured["nested"] == "PRO"


def _lib_default(name):
    m = re.search(rf'^{name}="([^"]*)"$', _LIB.read_text(), re.M)
    assert m, f"{name} must be a plain assignment in {_LIB}"
    return m.group(1)


def test_python_defaults_match_the_bash_lib_defaults():
    assert obs_phase2.STREAM_DEV_SCENE == _lib_default("STREAM_DEV_SCENE_DEFAULT") == "Development"
    assert obs_phase2.STREAM_PRODUCTION_SCENE == _lib_default(
        "STREAM_PRODUCTION_SCENE_DEFAULT") == "PRO"


# --- switch: skip when already on target, optional --min-mean -----------------------------------

def _switch_args(scene, min_mean=None):
    return types.SimpleNamespace(host="10.77.9.204", password="", program_scene=scene,
                                 min_mean=min_mean)


def test_switch_skips_set_when_the_target_is_already_on_program(monkeypatch, capsys):
    fake, calls, _ = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack",
                        lambda ws, host, scene, label, hint, min_mean=None: seen.update(
                            scene=scene, min_mean=min_mean))
    obs_phase2.switch(_switch_args("Development"))
    assert not any(c["op"] == "SetCurrentProgramScene" for c in calls)
    assert seen["scene"] == "Development"  # the non-black proof still runs
    assert capsys.readouterr().out.strip().isdigit()


def test_switch_sets_program_when_it_differs_and_passes_min_mean(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack",
                        lambda ws, host, scene, label, hint, min_mean=None: seen.update(
                            min_mean=min_mean))
    obs_phase2.switch(_switch_args("PRO", min_mean=5.0))
    assert [c["data"] for c in calls if c["op"] == "SetCurrentProgramScene"] == [
        {"sceneName": "PRO"}]
    assert state["program"] == "PRO"
    assert seen["min_mean"] == 5.0


def test_switch_cli_accepts_min_mean(monkeypatch):
    captured = {}
    monkeypatch.setattr(obs_phase2, "switch", lambda a: captured.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "switch", "--host", "h",
                                      "--program-scene", "PRO", "--min-mean", "5"])
    obs_phase2.main()
    assert captured["min_mean"] == 5.0


# --- program-rendered-input descends into a nested scene -----------------------------------------

def test_rendered_input_resolves_through_the_nested_production_scene():
    items = {"Development": [_nested_item()], "PRO": _PRO_ITEMS}
    assert obs_phase2._resolve_rendered_input(lambda s: items[s], "Development") == "NDI 2ME PGM"


def test_rendered_input_of_a_plain_scene_is_unchanged():
    items = {"Cam 1": [_input_item("NDI cam1", "ndi_source")]}
    assert obs_phase2._resolve_rendered_input(lambda s: items[s], "Cam 1") == "NDI cam1"


def test_rendered_input_skips_a_hidden_nested_scene():
    items = {"Development": [_nested_item(enabled=False),
                             _input_item("timer", "text_ft2_source_v2")],
             "PRO": _PRO_ITEMS}
    assert obs_phase2._resolve_rendered_input(lambda s: items[s], "Development") == "timer"


def test_rendered_input_returns_none_for_an_empty_nested_scene():
    items = {"Development": [_nested_item()], "PRO": []}
    assert obs_phase2._resolve_rendered_input(lambda s: items[s], "Development") is None


def test_rendered_input_stops_on_a_nesting_cycle():
    items = {"A": [_nested_item("B")], "B": [_nested_item("A")]}
    assert obs_phase2._resolve_rendered_input(lambda s: items[s], "A") is None


def test_program_rendered_input_cli_prints_the_nested_input(monkeypatch, capsys):
    fake, _, _ = _fake_obs(["PRO", "Development"],
                           {"Development": [_nested_item()], "PRO": _PRO_ITEMS},
                           program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    obs_phase2.program_rendered_input(types.SimpleNamespace(host="h", password="", scene=""))
    assert capsys.readouterr().out.strip() == "NDI 2ME PGM"


# --- the bash lib call shapes --------------------------------------------------------------------

def _run_lib(tmp_path, snippet):
    fake_dir = tmp_path / "scripts"
    fake_dir.mkdir()
    log = tmp_path / "argv.log"
    (fake_dir / "obs_phase2.py").write_text(
        "import sys\n"
        f"open({str(log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
        "print('Development')\n"
    )
    script = f'set -euo pipefail\n. "{_LIB}"\n{snippet.replace("SCRIPTS", str(fake_dir))}\n'
    out = subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                         env={**os.environ}, check=True)
    return out.stdout, log.read_text()


def test_lib_ensure_calls_dev_scene_with_the_scene_names(tmp_path):
    _, argv = _run_lib(tmp_path, 'stream_dev_scene_ensure SCRIPTS 10.77.9.204 pw Development PRO')
    assert argv.strip() == (
        "dev-scene --host 10.77.9.204 --password pw --scene Development --nested PRO")


def test_lib_program_scene_read_prints_the_scene(tmp_path):
    out, argv = _run_lib(tmp_path, 'stream_program_scene_read SCRIPTS 10.77.9.204 pw')
    assert out.strip() == "Development"
    assert argv.strip() == "program-scene --host 10.77.9.204 --password pw"


def test_lib_program_scene_read_prints_nothing_when_obs_is_unreachable(tmp_path):
    fake_dir = tmp_path / "scripts"
    fake_dir.mkdir()
    (fake_dir / "obs_phase2.py").write_text("import sys\nsys.exit(3)\n")
    script = f'set -euo pipefail\n. "{_LIB}"\nstream_program_scene_read "{fake_dir}" h pw\necho rc=$?\n'
    out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, check=True)
    assert out.stdout.strip() == "rc=0"
