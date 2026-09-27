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
    same-scene hazard) and takes `--prod-floor` (the ONE prod floor, #677);
  * the owner's hard rule: `_rpc` refuses to put PRO on program/preview, switch/prod-scene to PRO
    exit non-zero, teardown never restores PRO;
  * `program-rendered-input` descends into a nested scene source (a group via its own item list),
    so the TEST burn resolves `NDI 2ME PGM` through `Development -> PRO` and never tries to burn the
    scene `PRO`;
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
_SDS_PATH = _ROOT / "scripts" / "stream_dev_scene.py"
_SDS = {}


def _sds():
    """review round 2: the seeder + the Studio Mode preview logic live in their own pure module
    (scripts/stream_dev_scene.py, rpc injected); obs_phase2.py keeps only the CLI wiring."""
    if "m" not in _SDS:
        spec = importlib.util.spec_from_file_location("stream_dev_scene_1380", _SDS_PATH)
        mod = importlib.util.module_from_spec(spec)
        sys.modules["stream_dev_scene_1380"] = mod
        spec.loader.exec_module(mod)
        _SDS["m"] = mod
    return _SDS["m"]


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


def _fake_obs(scenes, items_by_scene, program="PRO", studio=False, preview="PRE",
              swap_reads=None, cursor_seq=None, fail_preview_set=False):
    """A fake `_rpc` over an in-memory OBS: scene list + per-scene items, program, Studio Mode +
    preview. Records every call. `swap_reads` models OBS's Studio Mode SWAP (default on,
    OBSApp.cpp SwapScenesMode): a program change queues the OLD program for the preview, applied
    when the transition ends -- here, just before the `swap_reads`-th later preview read."""
    state = {"scenes": list(scenes), "items": {k: list(v) for k, v in items_by_scene.items()},
             "program": program, "studio": studio, "preview": preview, "pending": None}
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
            if state["studio"] and swap_reads is not None:
                state["pending"] = [state["program"], swap_reads]
            state["program"] = rdata["sceneName"]
            return {}
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": state["studio"]}
        if rtype == "GetCurrentPreviewScene":
            if state["pending"] is not None:
                state["pending"][1] -= 1
                if state["pending"][1] <= 0:
                    state["preview"] = state["pending"][0]
                    state["pending"] = None
            return {"currentPreviewSceneName": state["preview"]}
        if rtype == "SetCurrentPreviewScene":
            if fail_preview_set:
                raise RuntimeError("SetCurrentPreviewScene failed: {'result': False}")
            state["preview"] = rdata["sceneName"]
            return {}
        if rtype == "GetCurrentSceneTransitionCursor":
            if cursor_seq is None:
                return {}
            value = cursor_seq.pop(0) if len(cursor_seq) > 1 else cursor_seq[0]
            return {"transitionCursor": value}
        if rtype == "GetGroupSceneItemList":
            return {"sceneItems": state["items"][rdata["sceneName"]]}
        return {}

    return fake_rpc, calls, state


def _writes(calls):
    return [c for c in calls if c["op"].startswith(_WRITE_PREFIXES)]


# --- the pure decision ---------------------------------------------------------------------------

def test_plan_creates_the_scene_and_the_nested_item_when_the_scene_is_missing():
    plan = _sds().dev_scene_plan(["PRO", "POST", "PRE"], None, "Development", "PRO")
    assert plan.actions == ["create_scene", "add_nested"]
    assert plan.nested_hidden is False


def test_plan_adds_only_the_nested_item_when_the_scene_exists_without_it():
    plan = _sds().dev_scene_plan(["PRO", "Development"], [], "Development", "PRO")
    assert plan.actions == ["add_nested"]


def test_plan_is_a_noop_when_the_scene_already_nests_the_production_scene():
    plan = _sds().dev_scene_plan(
        ["PRO", "POST", "Development", "PRE"], [_nested_item()], "Development", "PRO")
    assert plan.actions == []
    assert plan.nested_hidden is False


def test_plan_never_touches_an_operator_hidden_nested_item_but_reports_it():
    # Operator-wins: a hidden PRO item is the operator's choice -- no write, but it is reported so
    # the caller can say why the development program would render black.
    plan = _sds().dev_scene_plan(
        ["PRO", "Development"], [_nested_item(enabled=False)], "Development", "PRO")
    assert plan.actions == []
    assert plan.nested_hidden is True


def test_plan_keeps_extra_operator_items_untouched():
    items = [_input_item("timer", "text_ft2_source_v2"), _nested_item()]
    plan = _sds().dev_scene_plan(["PRO", "Development"], items, "Development", "PRO")
    assert plan.actions == []


def test_plan_fails_loud_when_the_production_scene_is_missing():
    with pytest.raises(_sds().DevSceneError, match="PRO"):
        _sds().dev_scene_plan(["POST", "PRE"], None, "Development", "PRO")


def test_plan_refuses_a_development_scene_equal_to_the_production_scene():
    with pytest.raises(_sds().DevSceneError):
        _sds().dev_scene_plan(["PRO"], [], "PRO", "PRO")


def test_plan_refuses_empty_scene_names():
    with pytest.raises(_sds().DevSceneError):
        _sds().dev_scene_plan(["PRO"], None, "", "PRO")
    with pytest.raises(_sds().DevSceneError):
        _sds().dev_scene_plan(["PRO"], None, "Development", "")


# --- ensure_dev_scene over a fake OBS ------------------------------------------------------------

def test_ensure_is_a_noop_on_the_live_27_9_state(monkeypatch):
    fake, calls, _ = _fake_obs(["PRO", "POST", "Development", "PRE"],
                               {"PRO": _PRO_ITEMS, "Development": [_nested_item()]})
    plan = _sds().ensure_dev_scene(fake, FakeWS(), "Development", "PRO")
    assert plan.actions == []
    assert _writes(calls) == []


def test_ensure_creates_the_scene_and_nests_pro_writing_only_to_development(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "POST", "PRE"], {"PRO": _PRO_ITEMS})
    plan = _sds().ensure_dev_scene(fake, FakeWS(), "Development", "PRO")
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
    _sds().ensure_dev_scene(fake, FakeWS(), "Development", "PRO")
    assert not any(c["data"].get("sceneName") == "PRO" for c in calls)


def test_ensure_fails_loud_without_any_write_when_pro_is_missing(monkeypatch):
    fake, calls, _ = _fake_obs(["POST", "PRE"], {})
    with pytest.raises(_sds().DevSceneError):
        _sds().ensure_dev_scene(fake, FakeWS(), "Development", "PRO")
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


def test_dev_scene_cli_defaults_to_development_nesting_pro(monkeypatch, capsys):
    # With no --scene/--nested the CLI ensures the declared names (resolved from the pure module at
    # call time, so the other subcommands never import it).
    fake, calls, _ = _fake_obs(["PRO"], {"PRO": _PRO_ITEMS})
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "dev-scene", "--host", "h"])
    obs_phase2.main()
    writes = _writes(calls)
    assert [w["data"] for w in writes] == [
        {"sceneName": "Development"},
        {"sceneName": "Development", "sourceName": "PRO", "sceneItemEnabled": True}]
    assert capsys.readouterr().out.strip().splitlines()[-1] == (
        "DEV_SCENE=Development created=1 nested_added=1")


def test_obs_phase2_cli_works_where_only_obs_phase2_is_deployed(tmp_path):
    # setup-imag.sh / setup-strih.sh install obs_phase2.py ALONE on the boxes; no other subcommand
    # may need scripts/stream_dev_scene.py next to it.
    lone = tmp_path / "obs_phase2.py"
    lone.write_text(_MOD_PATH.read_text())
    out = subprocess.run([sys.executable, str(lone), "switch", "--help"], capture_output=True,
                         text=True)
    assert out.returncode == 0, out.stderr
    out = subprocess.run([sys.executable, str(lone), "program-scene", "--help"],
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr


def _lib_default(name):
    m = re.search(rf'^{name}="([^"]*)"$', _LIB.read_text(), re.M)
    assert m, f"{name} must be a plain assignment in {_LIB}"
    return m.group(1)


def test_python_defaults_match_the_bash_lib_defaults():
    assert _sds().STREAM_DEV_SCENE == _lib_default("STREAM_DEV_SCENE_DEFAULT") == "Development"
    assert _sds().STREAM_PRODUCTION_SCENE == _lib_default(
        "STREAM_PRODUCTION_SCENE_DEFAULT") == "PRO"


def test_obs_phase2_holds_no_copy_of_the_seeder():
    # One home for the seeder + the preview logic; obs_phase2.py only wires the CLI (file size).
    src = _MOD_PATH.read_text()
    for name in ("def dev_scene_plan", "class DevSceneError", "def ensure_dev_scene",
                 "def _stale_preview_target", "STREAM_PRODUCTION_SCENE = "):
        assert name not in src, name


# --- switch: skip when already on target, optional --min-mean -----------------------------------

def _switch_args(scene, prod_floor=False):
    return types.SimpleNamespace(host="10.77.9.204", password="", program_scene=scene,
                                 prod_floor=prod_floor)


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


def _spy_nonblack(monkeypatch, seen, raise_black=False):
    def spy(ws, host, scene, label, hint, min_mean=None):
        seen["min_mean"] = min_mean
        seen["hint"] = hint
        if raise_black:
            raise SystemExit(f"[obs] {host}: {label} program '{scene}' BLACK")
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", spy)


def test_switch_sets_program_when_it_differs_and_uses_the_prod_floor(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="PRE")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.delenv("OBS_NONBLACK_MIN_MEAN_PROD", raising=False)
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("Development", prod_floor=True))
    assert [c["data"] for c in calls if c["op"] == "SetCurrentProgramScene"] == [
        {"sceneName": "Development"}]
    assert state["program"] == "Development"
    assert seen["min_mean"] == 5.0


def test_the_prod_floor_is_the_one_prod_scene_env_knob(monkeypatch):
    fake, _, _ = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setenv("OBS_NONBLACK_MIN_MEAN_PROD", "7.5")
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("Development", prod_floor=True))
    assert seen["min_mean"] == 7.5
    assert obs_phase2._prod_nonblack_floor() == 7.5


def test_switch_without_prod_floor_keeps_the_312_default(monkeypatch):
    fake, _, _ = _fake_obs(["Cam 1"], {}, program="Cam 1")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("Cam 1"))
    assert seen["min_mean"] is None


def test_switch_black_hint_names_the_program_not_a_cambox(monkeypatch):
    fake, _, _ = _fake_obs(["Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("Development"))
    assert "'Development'" in seen["hint"]


def _fetch(items):
    return lambda name, is_group=False: items[name]


def test_rendered_input_resolves_through_the_nested_production_scene():
    items = {"Development": [_nested_item()], "PRO": _PRO_ITEMS}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "Development") == "NDI 2ME PGM"


def test_rendered_input_of_a_plain_scene_is_unchanged():
    items = {"Cam 1": [_input_item("NDI cam1", "ndi_source")]}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "Cam 1") == "NDI cam1"


def test_rendered_input_skips_a_hidden_nested_scene():
    items = {"Development": [_nested_item(enabled=False),
                             _input_item("timer", "text_ft2_source_v2")],
             "PRO": _PRO_ITEMS}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "Development") == "timer"


def test_rendered_input_returns_none_for_an_empty_nested_scene():
    items = {"Development": [_nested_item()], "PRO": []}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "Development") is None


def test_rendered_input_stops_on_a_nesting_cycle():
    items = {"A": [_nested_item("B")], "B": [_nested_item("A")]}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "A") is None


def test_rendered_input_strih_shaped_nested_first_item_resolves_the_camera_input():
    # The descent also applies to strih (rig-mode gap 3) and imag (the issue-1204 cross-check): a
    # program scene whose first enabled item is a nested scene now resolves the real camera input
    # instead of the nested scene's name.
    items = {"Cam 1": [_input_item("ASIO zvuk", "asio_input_capture"), _nested_item("Cam 1 base")],
             "Cam 1 base": [_input_item("NDI cam1", "ndi_source")]}
    assert obs_phase2._resolve_rendered_input(_fetch(items), "Cam 1") == "NDI cam1"


def test_rendered_input_reads_a_group_through_the_group_item_list():
    # An OBS group is also OBS_SOURCE_TYPE_SCENE (isGroup true); GetSceneItemList on a group name
    # fails, the group's items come from GetGroupSceneItemList.
    group = dict(_nested_item("Cams"), isGroup=True)
    seen = []

    def fetch(name, is_group=False):
        seen.append((name, is_group))
        return {"Main": [group], "Cams": [_input_item("NDI cam3", "ndi_source")]}[name]

    assert obs_phase2._resolve_rendered_input(fetch, "Main") == "NDI cam3"
    assert seen == [("Main", False), ("Cams", True)]


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


def test_lib_ensure_never_seeds_a_non_default_scene_override(tmp_path):
    # An override naming another (possibly the owner's) scene must never get a nested production
    # item added; the caller's prod-scene/switch then needs that scene to already exist.
    fake_dir = tmp_path / "scripts"
    fake_dir.mkdir()
    log = tmp_path / "argv.log"
    (fake_dir / "obs_phase2.py").write_text(
        f"import sys\nopen({str(log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n")
    script = (f'set -euo pipefail\n. "{_LIB}"\n'
              f'stream_dev_scene_ensure "{fake_dir}" h pw POST PRO\necho rc=$?\n')
    out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, check=True)
    assert out.stdout.strip().splitlines()[-1] == "rc=0"
    assert "not seeding" in out.stdout
    assert not log.exists()


def test_lib_ensure_refuses_an_override_naming_the_production_scene(tmp_path):
    script = f'. "{_LIB}"\nstream_dev_scene_ensure /nonexistent h pw PRO PRO\necho rc=$?\n'
    out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, check=True)
    assert out.stdout.strip() == "rc=1"
    assert "IS the production scene" in out.stderr


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


# --- the owner's hard rule: our tooling NEVER programs PRO ---------------------------------------
# Owner, 27.9.2026, verbatim: "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!"


class _SendWS:
    """A fake obs-websocket that records every request SENT and answers it with success."""

    def __init__(self):
        self.sent = []
        self._pending = []

    def send(self, raw):
        import json
        msg = json.loads(raw)
        self.sent.append(msg["d"])
        self._pending.append({"op": 7, "d": {"requestId": msg["d"]["requestId"],
                                             "requestStatus": {"result": True},
                                             "responseData": {}}})

    def recv(self):
        import json
        return json.dumps(self._pending.pop(0))

    def close(self):
        pass


def test_rpc_refuses_to_program_or_preview_the_production_scene():
    for request in ("SetCurrentProgramScene", "SetCurrentPreviewScene"):
        for ignore_err in (False, True):
            ws = _SendWS()
            with pytest.raises(obs_phase2.ForbiddenSceneError, match="PRO"):
                obs_phase2._rpc(ws, request, {"sceneName": "PRO"}, ignore_err=ignore_err)
            assert ws.sent == [], "the forbidden request must never reach OBS"


def test_rpc_still_programs_the_development_scene():
    ws = _SendWS()
    obs_phase2._rpc(ws, "SetCurrentProgramScene", {"sceneName": "Development"})
    assert [d["requestType"] for d in ws.sent] == ["SetCurrentProgramScene"]


def test_the_forbidden_scene_is_the_declared_production_scene():
    assert obs_phase2.NEVER_PROGRAM_SCENES == frozenset({_lib_default(
        "STREAM_PRODUCTION_SCENE_DEFAULT")})


def test_switch_to_pro_exits_non_zero_and_sends_nothing(monkeypatch):
    ws = _SendWS()
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": ws)
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "switch", "--host", "10.77.9.204",
                                      "--program-scene", "PRO"])
    with pytest.raises(SystemExit) as exc:
        obs_phase2.main()
    assert exc.value.code not in (0, None)
    assert "PRO" in str(exc.value.code)
    assert not any(d["requestType"].startswith("Set") for d in ws.sent)


def test_prod_scene_to_pro_exits_non_zero(monkeypatch):
    fake, calls, _ = _fake_obs(["PRO", "Development"], {}, program="Development")

    def guarded(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        obs_phase2._refuse_forbidden_scene(rtype, rdata)
        return fake(ws, rtype, rdata, ignore_err, timeout_s)

    monkeypatch.setattr(obs_phase2, "_rpc", guarded)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: {})
    monkeypatch.setattr(obs_phase2, "_save_state", lambda state: None)
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "prod-scene", "--host", "10.77.9.204",
                                      "--program-scene", "PRO"])
    with pytest.raises(SystemExit) as exc:
        obs_phase2.main()
    assert exc.value.code not in (0, None)
    assert not any(c["op"].startswith("SetCurrent") and c["data"].get("sceneName") == "PRO"
                   for c in calls)


def test_teardown_never_restores_pro_and_still_idles_the_probe_input(monkeypatch, capsys):
    # An E2E that started with PRO on program/preview: teardown leaves the development scene where
    # it is (the owner cuts to PRO himself) and still does the rest of its restore.
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                                   studio=True, preview="Development")

    def guarded(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        obs_phase2._refuse_forbidden_scene(rtype, rdata)
        return fake(ws, rtype, rdata, ignore_err, timeout_s)

    monkeypatch.setattr(obs_phase2, "_rpc", guarded)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: {
        "10.77.9.204": {"prev_scene": "PRO", "prev_preview": "PRO"}})
    monkeypatch.setattr(obs_phase2, "_restore_test_latency", lambda *a, **k: None)
    monkeypatch.setattr(obs_phase2, "_restore_measurement_pins", lambda *a, **k: None)
    monkeypatch.setattr(obs_phase2, "_restore_test_preload", lambda *a, **k: None)
    obs_phase2.teardown(types.SimpleNamespace(host="10.77.9.204", password="",
                                              calibrated_latency_ms=None))
    assert state["program"] == "Development"
    assert state["preview"] == "Development"
    assert any(c["op"] == "SetInputSettings" for c in calls), "the probe input idle must still run"
    err = capsys.readouterr().err
    assert "teardown warning" not in err
    assert "never programs" in err


def test_teardown_still_restores_an_operator_scene(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "PRE", "Development"], {}, program="Development",
                                   studio=True, preview="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "_load_state", lambda: {
        "10.77.9.204": {"prev_scene": "PRE", "prev_preview": "PRE"}})
    monkeypatch.setattr(obs_phase2, "_restore_test_latency", lambda *a, **k: None)
    monkeypatch.setattr(obs_phase2, "_restore_measurement_pins", lambda *a, **k: None)
    monkeypatch.setattr(obs_phase2, "_restore_test_preload", lambda *a, **k: None)
    obs_phase2.teardown(types.SimpleNamespace(host="10.77.9.204", password="",
                                              calibrated_latency_ms=None))
    assert state["program"] == "PRE"
    assert state["preview"] == "PRE"


def test_the_seeder_module_holds_no_preview_writer():
    src = _SDS_PATH.read_text()
    assert "SetCurrentPreviewScene" not in src
    assert "SetCurrentProgramScene" not in src
