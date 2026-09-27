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
    same-scene hazard); the EVENT flags `--prod-floor` (the ONE prod floor, #677),
    `--black-report-only` and `--replace-preview`;
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

def _switch_args(scene, prod_floor=False, black_report_only=False, replace_preview="",
                 only_from=""):
    return types.SimpleNamespace(host="10.77.9.204", password="", program_scene=scene,
                                 prod_floor=prod_floor, black_report_only=black_report_only,
                                 replace_preview=replace_preview, only_from=only_from)


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
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.delenv("OBS_NONBLACK_MIN_MEAN_PROD", raising=False)
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("PRO", prod_floor=True))
    assert [c["data"] for c in calls if c["op"] == "SetCurrentProgramScene"] == [
        {"sceneName": "PRO"}]
    assert state["program"] == "PRO"
    assert seen["min_mean"] == 5.0


def test_the_prod_floor_is_the_one_prod_scene_env_knob(monkeypatch):
    fake, _, _ = _fake_obs(["PRO"], {}, program="PRO")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setenv("OBS_NONBLACK_MIN_MEAN_PROD", "7.5")
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("PRO", prod_floor=True))
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


def test_switch_black_report_only_warns_and_still_succeeds(monkeypatch, capsys):
    # EVENT mode: a legitimately dark production scene (cameras not powered yet) must not fail the
    # EVENT switch once the scene itself is set; a real set/transport failure still fails.
    fake, _, state = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    _spy_nonblack(monkeypatch, {}, raise_black=True)
    obs_phase2.switch(_switch_args("PRO", prod_floor=True, black_report_only=True))
    out = capsys.readouterr()
    assert state["program"] == "PRO"
    assert out.out.strip().isdigit()
    assert "WARNING" in out.err and "BLACK" in out.err


def test_switch_black_without_report_only_still_fails(monkeypatch):
    fake, _, _ = _fake_obs(["PRO"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    _spy_nonblack(monkeypatch, {}, raise_black=True)
    with pytest.raises(SystemExit):
        obs_phase2.switch(_switch_args("PRO", prod_floor=True))


def test_switch_black_hint_names_the_program_not_a_cambox(monkeypatch):
    fake, _, _ = _fake_obs(["Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("Development"))
    assert "'Development'" in seen["hint"]


def test_switch_moves_a_stale_development_preview_to_the_target(monkeypatch):
    # Studio Mode: an E2E leaves the development scene in PREVIEW; after EVENT a Transition click
    # must never put the development scene back on program.
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                                   studio=True, preview="Development")
    monkeypatch.setattr(obs_phase2, "PREVIEW_SWAP_MARGIN_S", 0.0)
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    _spy_nonblack(monkeypatch, {})
    obs_phase2.switch(_switch_args("PRO", replace_preview="Development"))
    assert state["preview"] == "PRO"
    assert [c["data"] for c in calls if c["op"] == "SetCurrentPreviewScene"] == [
        {"sceneName": "PRO"}]


def test_switch_leaves_an_operator_preview_alone(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "PRE", "Development"], {}, program="Development",
                                   studio=True, preview="PRE")
    monkeypatch.setattr(obs_phase2, "PREVIEW_SWAP_MARGIN_S", 0.0)
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    _spy_nonblack(monkeypatch, {})
    obs_phase2.switch(_switch_args("PRO", replace_preview="Development"))
    assert state["preview"] == "PRE"
    assert not any(c["op"] == "SetCurrentPreviewScene" for c in calls)


def _clock():
    clock = [0.0]

    def sleep(dt):
        clock[0] += dt

    return sleep, (lambda: clock[0])


def test_preview_reassert_waits_out_the_studio_mode_swap():
    # The real post-TEST state: program Development, preview PRO. EVENT's cut to PRO makes OBS put
    # Development into the preview when the transition ENDS -- after a one-shot check would run.
    # The cursor reads the previous transition's 1.0 first, then this 300 ms fade.
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                                   studio=True, preview="PRO", swap_reads=4,
                                   cursor_seq=[1.0, 0.3, 0.8, 1.0])
    fake(None, "SetCurrentProgramScene", {"sceneName": "PRO"})
    sleep, now = _clock()
    moved = _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=0.5,
                                          poll_s=0.25, sleep=sleep, now=now)
    assert state["preview"] == "PRO"
    assert state["pending"] is None
    assert moved == 1
    assert [c["data"] for c in calls if c["op"] == "SetCurrentPreviewScene"] == [
        {"sceneName": "PRO"}]


def test_preview_reassert_outlasts_a_long_fixed_transition():
    # A stinger is a FIXED transition: GetCurrentSceneTransition reports no duration, and it can run
    # for seconds. The re-assert follows the observed cursor to 1.0, then the margin -- a
    # configured-duration window (null -> margin only) would exit before the swap lands.
    fake, _, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                               studio=True, preview="PRO", swap_reads=15,
                               cursor_seq=[0.05 * i for i in range(1, 14)] + [1.0])
    fake(None, "SetCurrentProgramScene", {"sceneName": "PRO"})
    sleep, now = _clock()
    moved = _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=0.5,
                                          poll_s=0.25, sleep=sleep, now=now)
    for _ in range(3):
        fake(None, "GetCurrentPreviewScene")
    assert state["pending"] is None
    assert state["preview"] == "PRO"
    assert moved == 1


def test_preview_reassert_stops_at_the_hard_cap():
    fake, _, _ = _fake_obs(["PRO", "Development"], {}, program="PRO", studio=True,
                           preview="PRO", cursor_seq=[0.5])
    sleep, now = _clock()
    _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=0.5,
                                  poll_s=0.25, cap_s=3.0, sleep=sleep, now=now)
    assert 3.0 <= now() <= 3.5


def test_preview_reassert_without_a_cursor_waits_the_margin_only():
    fake, _, _ = _fake_obs(["PRO", "Development"], {}, program="PRO", studio=True,
                           preview="PRO")
    sleep, now = _clock()
    _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=1.0,
                                  poll_s=0.25, sleep=sleep, now=now)
    assert 1.0 <= now() <= 1.25


def test_preview_reassert_waits_for_a_transition_that_starts_late():
    # The cursor still reads the PREVIOUS transition's 1.0 for a moment after the cut is requested;
    # the re-assert must not take that for "already ended" before the start timeout.
    fake, _, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                               studio=True, preview="PRO", swap_reads=8,
                               cursor_seq=[1.0, 1.0, 0.2, 0.5, 0.9, 1.0])
    fake(None, "SetCurrentProgramScene", {"sceneName": "PRO"})
    sleep, now = _clock()
    _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=0.5,
                                  poll_s=0.25, start_timeout_s=1.0, sleep=sleep, now=now)
    for _ in range(3):
        fake(None, "GetCurrentPreviewScene")
    assert state["preview"] == "PRO"


def test_preview_reassert_is_bounded_and_idle_outside_studio_mode():
    fake, calls, _ = _fake_obs(["PRO", "Development"], {}, program="PRO", studio=False)
    moved = _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=5.0,
                                          poll_s=0.25, sleep=lambda dt: None, now=lambda: 0.0)
    assert moved == 0
    assert not any(c["op"] == "GetCurrentPreviewScene" for c in calls)


def test_preview_reassert_fails_loud_when_the_preview_set_fails():
    fake, _, _ = _fake_obs(["PRO", "Development"], {}, program="PRO", studio=True,
                           preview="Development", fail_preview_set=True)
    sleep, now = _clock()
    with pytest.raises(RuntimeError):
        _sds().reassert_stale_preview(fake, FakeWS(), "Development", "PRO", margin_s=0.5,
                                      poll_s=0.25, sleep=sleep, now=now)


def test_switch_preview_failure_is_not_swallowed_by_black_report_only(monkeypatch):
    fake, _, _ = _fake_obs(["PRO", "Development"], {}, program="Development", studio=True,
                           preview="Development", fail_preview_set=True)
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "PREVIEW_SWAP_MARGIN_S", 0.0)
    _spy_nonblack(monkeypatch, {})
    with pytest.raises(RuntimeError):
        obs_phase2.switch(_switch_args("PRO", prod_floor=True, black_report_only=True,
                                       replace_preview="Development"))


def test_switch_fixes_the_preview_after_the_studio_mode_swap(monkeypatch):
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development",
                                   studio=True, preview="PRO", swap_reads=2)
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    monkeypatch.setattr(obs_phase2, "PREVIEW_SWAP_MARGIN_S", 0.2)
    monkeypatch.setattr(obs_phase2, "PREVIEW_POLL_S", 0.01)
    _spy_nonblack(monkeypatch, {})
    obs_phase2.switch(_switch_args("PRO", replace_preview="Development"))
    # The transition has ended by now: any swap still queued lands on these reads.
    for _ in range(3):
        fake(None, "GetCurrentPreviewScene")
    assert state["program"] == "PRO"
    assert state["pending"] is None
    assert state["preview"] == "PRO"


def test_switch_transport_failure_still_fails_under_black_report_only(monkeypatch):
    fake, _, _ = _fake_obs(["PRO"], {}, program="Development")

    def failing(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "SetCurrentProgramScene":
            raise TimeoutError("obs-websocket request 'SetCurrentProgramScene' got no response")
        return fake(ws, rtype, rdata, ignore_err, timeout_s)

    monkeypatch.setattr(obs_phase2, "_rpc", failing)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    _spy_nonblack(monkeypatch, {})
    with pytest.raises(TimeoutError):
        obs_phase2.switch(_switch_args("PRO", prod_floor=True, black_report_only=True))


def test_stale_preview_decision_is_pure():
    f = _sds().stale_preview_target
    assert f(True, "Development", "Development", "PRO") == "PRO"
    assert f(True, "PRE", "Development", "PRO") is None
    assert f(False, "Development", "Development", "PRO") is None
    assert f(True, "Development", "", "PRO") is None
    assert f(True, "PRO", "PRO", "PRO") is None


def test_switch_only_from_switches_when_program_is_the_development_scene(monkeypatch):
    # ROZHODNUTIE 27.9.2026: EVENT undoes development -- program Development -> PRO.
    fake, calls, state = _fake_obs(["PRO", "Development"], {}, program="Development")
    monkeypatch.setattr(obs_phase2, "_rpc", fake)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    seen = {}
    _spy_nonblack(monkeypatch, seen)
    obs_phase2.switch(_switch_args("PRO", only_from="Development"))
    assert state["program"] == "PRO"
    assert "min_mean" in seen


def test_switch_only_from_leaves_an_operator_scene_alone(monkeypatch, capsys):
    # The operator is on PRE (or POST, or PRO already): EVENT never cuts it -- no scene set, no
    # preview write, no black proof of a scene development never touched.
    for live in ("PRE", "POST", "PRO"):
        fake, calls, state = _fake_obs(["PRO", "PRE", "POST", "Development"], {}, program=live,
                                       studio=True, preview="Development")
        monkeypatch.setattr(obs_phase2, "_rpc", fake)
        monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
        seen = {}
        _spy_nonblack(monkeypatch, seen)
        obs_phase2.switch(_switch_args("PRO", prod_floor=True, black_report_only=True,
                                       replace_preview="Development", only_from="Development"))
        assert state["program"] == live
        assert _writes(calls) == []
        assert seen == {}
        err = capsys.readouterr().err
        assert "left alone" in err and live in err


def test_switch_cli_accepts_the_event_flags(monkeypatch):
    captured = {}
    monkeypatch.setattr(obs_phase2, "switch", lambda a: captured.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "switch", "--host", "h",
                                      "--program-scene", "PRO", "--prod-floor",
                                      "--black-report-only", "--replace-preview", "Development",
                                      "--only-from", "Development"])
    obs_phase2.main()
    assert captured["only_from"] == "Development"
    assert captured["prod_floor"] is True
    assert captured["black_report_only"] is True
    assert captured["replace_preview"] == "Development"


# --- program-rendered-input descends into a nested scene -----------------------------------------

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
