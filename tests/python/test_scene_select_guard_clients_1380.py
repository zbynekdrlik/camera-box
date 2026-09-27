"""issue 1380 slice 2 -- every standalone OBS-WS client refuses the production scene `PRO`.

Owner hard rule, 27.9.2026, verbatim: "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!"

Slice 1 put the ONE guard (`obs_phase2._refuse_forbidden_scene`: `PRO` and any `sceneUuid` on a
program/preview selection) at `obs_phase2._rpc`. The standalone clients below talk to OBS through
their own transport, or select a scene before the guarded cut, so each one reuses that guard (never
a copy of the rule) and refuses before anything is sent:

  * glk_wire.py, frozen-camera-gate.py, imag_scenes.py -- own transport: the guard runs at it;
  * cg_chain_scene.py -- rides obs_phase2._rpc, and refuses the target before its snapshot/writes;
  * warm_cam_scenes.py -- rides obs_phase2._rpc.

A restore of a recorded state never re-selects `PRO` either: it skips that one request with a named
line and restores the rest (the slice-1 teardown contract).

Hermetic: every OBS connection is a scripted fake; nothing reaches a box.
"""
import importlib.util
import json
import pathlib
import re
import sys
import types

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
_SELECTS = ("SetCurrentProgramScene", "SetCurrentPreviewScene")


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _is_refusal(exc):
    """The guard's error, whichever loaded copy of obs_phase2 raised it."""
    return type(exc).__name__ == "ForbiddenSceneError"


class ScriptedWS:
    """A fake obs-websocket. Records every request it is SENT and answers each with the scripted
    responseData for its requestType (echoing the requestId). `handshake=True` queues the hello +
    identified frames a client constructor reads first."""

    def __init__(self, responses=None, handshake=False):
        self.responses = responses or {}
        self.sent = []
        self._pending = []
        if handshake:
            self._pending += [{"op": 0, "d": {"rpcVersion": 1}}, {"op": 2, "d": {}}]

    def send(self, raw):
        msg = json.loads(raw)
        if msg.get("op") != 6:
            return
        d = msg["d"]
        self.sent.append((d["requestType"], dict(d.get("requestData") or {})))
        self._pending.append({"op": 7, "d": {
            "requestId": d["requestId"], "requestType": d["requestType"],
            "requestStatus": {"result": True},
            "responseData": dict(self.responses.get(d["requestType"], {}))}})

    def recv(self):
        return json.dumps(self._pending.pop(0))

    def close(self):
        pass


def _selects_of(sent, scene=None):
    return [(t, d) for t, d in sent
            if t in _SELECTS and (scene is None or d.get("sceneName") == scene)]


# ---- glk_wire.py (own transport) ----------------------------------------------------------------


def _glk():
    return _load(SCRIPTS / "glk_wire.py", "glk_wire_pro_guard_1380")


def test_glk_wire_rpc_refuses_the_production_scene():
    glk = _glk()
    for request in _SELECTS:
        for ignore_err in (False, True):
            ws = ScriptedWS()
            with pytest.raises(RuntimeError) as exc:
                glk._rpc(ws, request, {"sceneName": "PRO"}, ignore_err=ignore_err)
            assert _is_refusal(exc.value), exc.value
            assert "PRO" in str(exc.value)
            assert ws.sent == [], "the refused request must never reach OBS"


def test_glk_wire_rpc_refuses_a_scene_uuid_selection():
    glk = _glk()
    ws = ScriptedWS()
    with pytest.raises(RuntimeError) as exc:
        glk._rpc(ws, "SetCurrentProgramScene", {"sceneName": glk.SCENE, "sceneUuid": "pro-uuid"})
    assert _is_refusal(exc.value)
    assert ws.sent == []


def _glk_responses(scene):
    return {
        "GetSceneList": {"scenes": [{"sceneName": scene}]},
        "GetInputList": {"inputs": [{"inputName": "genlock-in"}]},
        "GetSceneItemList": {"sceneItems": [{"sceneItemId": 1, "sourceName": "genlock-in"}]},
        "GetCurrentProgramScene": {"currentProgramSceneName": scene},
    }


def test_glk_wire_refuses_a_production_scene_target_before_connecting(monkeypatch):
    glk = _glk()
    monkeypatch.setattr(glk, "SCENE", "PRO")
    opened = []

    def conn(host, port):
        ws = ScriptedWS(_glk_responses("PRO"))
        opened.append(ws)
        return ws

    monkeypatch.setattr(glk, "_conn", conn)
    monkeypatch.setattr(sys, "argv", ["glk_wire.py", "--host", "10.77.9.204", "--port", "4455",
                                      "--upstream", "X", "--canvas-w", "1920", "--canvas-h", "1080"])
    with pytest.raises(SystemExit) as exc:
        glk.main()
    assert exc.value.code not in (0, None)
    assert "PRO" in str(exc.value.code)
    assert opened == [], "a production-scene target is refused before any connection or write"


def test_glk_wire_still_programs_its_own_scene(monkeypatch):
    glk = _glk()
    ws = ScriptedWS(_glk_responses(glk.SCENE))
    monkeypatch.setattr(glk, "_conn", lambda host, port: ws)
    monkeypatch.setattr(sys, "argv", ["glk_wire.py", "--host", "10.77.9.202", "--port", "4471",
                                      "--upstream", "X", "--canvas-w", "1920", "--canvas-h", "1080"])
    glk.main()
    assert _selects_of(ws.sent) == [("SetCurrentProgramScene", {"sceneName": glk.SCENE})]


# ---- frozen-camera-gate.py (own transport; exit 2 = ERROR, never 1 = FROZEN) --------------------


def _gate():
    return _load(SCRIPTS / "frozen-camera-gate.py", "frozen_camera_gate_pro_guard_1380")


def test_frozen_gate_rpc_refuses_the_production_scene():
    gate = _gate()
    for request in _SELECTS:
        ws = ScriptedWS()
        with pytest.raises(RuntimeError) as exc:
            gate._rpc(ws, request, {"sceneName": "PRO"}, ignore_err=True)
        assert _is_refusal(exc.value)
        assert ws.sent == []
    ws = ScriptedWS()
    with pytest.raises(RuntimeError) as exc:
        gate._rpc(ws, "SetCurrentPreviewScene", {"sceneUuid": "pro-uuid"}, ignore_err=True)
    assert _is_refusal(exc.value)
    assert ws.sent == []


def _run_gate(gate, monkeypatch, capsys, preview):
    ws = ScriptedWS({
        "GetStudioModeEnabled": {"studioModeEnabled": True},
        "GetCurrentPreviewScene": {"currentPreviewSceneName": preview},
    })
    monkeypatch.setattr(gate, "_conn", lambda host, password="": ws)
    monkeypatch.setattr(gate, "_screenshot_hash", lambda ws_, src: "h")
    monkeypatch.setattr(gate.time, "sleep", lambda s: None)
    monkeypatch.setattr(gate, "_run_verdict", lambda timeline, binary, threshold: (0, "PASS"))
    monkeypatch.setattr(sys, "argv", ["frozen-camera-gate.py", "--host", "10.77.9.202",
                                      "--sources", "NDI cam1", "--samples", "1",
                                      "--verdict-bin", "/nonexistent", "--warm-settle", "0.5"])
    with pytest.raises(SystemExit) as exc:
        gate.main()
    return exc.value.code, ws, capsys.readouterr().err


def test_frozen_gate_refused_warm_target_is_an_error_never_a_frozen_verdict(monkeypatch, capsys):
    gate = _gate()
    monkeypatch.setattr(gate, "_scene_for_input", lambda source: "PRO")
    code, ws, err = _run_gate(gate, monkeypatch, capsys, preview="Multiview")
    assert code == 2, "a refused selection is an ERROR (2), never read as a frozen camera (1)"
    assert _selects_of(ws.sent, "PRO") == []
    assert "PRO" in err
    # the operator's own preview is still put back
    assert _selects_of(ws.sent, "Multiview") == [("SetCurrentPreviewScene",
                                                   {"sceneName": "Multiview"})]


def test_frozen_gate_a_broken_guard_import_is_an_error_never_frozen(monkeypatch, capsys):
    # review round 1: ANY failure to load the guard exits 2 (ERROR); 1 would read as FROZEN.
    gate = _gate()

    def broken():
        raise RuntimeError("obs_phase2 failed to load")

    monkeypatch.setattr(gate, "_obs_phase2", broken)
    monkeypatch.setattr(gate, "_conn", lambda host, password="": pytest.fail("connected"))
    monkeypatch.setattr(sys, "argv", ["frozen-camera-gate.py", "--host", "10.77.9.202",
                                      "--verdict-bin", "/nonexistent"])
    with pytest.raises(SystemExit) as exc:
        gate.main()
    assert exc.value.code == 2
    assert "obs_phase2" in capsys.readouterr().err


def test_frozen_gate_never_restores_a_production_preview(monkeypatch, capsys):
    gate = _gate()
    code, ws, err = _run_gate(gate, monkeypatch, capsys, preview="PRO")
    assert code == 0, "the verdict still stands"
    assert _selects_of(ws.sent, "PRO") == []
    assert _selects_of(ws.sent, "Cam 1") == [("SetCurrentPreviewScene", {"sceneName": "Cam 1"})]
    assert "never" in err and "PRO" in err


# ---- imag_scenes.py (own transport Obs.req; the --bootstrap program restore) --------------------


def _imag():
    return _load(SCRIPTS / "imag_scenes.py", "imag_scenes_pro_guard_1380")


def _imag_obs(imag, monkeypatch, responses=None):
    ws = ScriptedWS(responses, handshake=True)
    monkeypatch.setattr(imag, "create_connection", lambda url, timeout=10: ws)
    return imag.Obs("10.77.9.187", 4455, None), ws


def test_imag_obs_req_refuses_the_production_scene(monkeypatch):
    imag = _imag()
    obs, ws = _imag_obs(imag, monkeypatch)
    for request in _SELECTS:
        for ignore_err in (False, True):
            with pytest.raises(SystemExit) as exc:
                obs.req(request, {"sceneName": "PRO"}, ignore_err=ignore_err)
            assert exc.value.code not in (0, None)
            assert "PRO" in str(exc.value.code)
    with pytest.raises(SystemExit) as exc:
        obs.req("SetCurrentProgramScene", {"sceneName": "Cam 1", "sceneUuid": "pro-uuid"},
                ignore_err=True)
    assert "sceneUuid" in str(exc.value.code)
    assert ws.sent == [], "a refused request never reaches OBS"


def test_imag_obs_req_still_selects_an_imag_scene(monkeypatch):
    imag = _imag()
    obs, ws = _imag_obs(imag, monkeypatch)
    obs.req("SetCurrentProgramScene", {"sceneName": "Cam 3"}, ignore_err=True)
    assert ws.sent == [("SetCurrentProgramScene", {"sceneName": "Cam 3"})]


class SeedObs:
    """Records req() calls for seed(); every seed-owned scene and item already exists."""

    def __init__(self):
        self.calls = []

    def req(self, rtype, payload=None, ignore_err=False):
        self.calls.append((rtype, dict(payload or {})))
        if rtype == "GetSceneItemId":
            return {"sceneItemId": 7}
        if rtype == "GetVideoSettings":
            return {"fpsNumerator": 60, "fpsDenominator": 1, "baseWidth": 1920,
                    "baseHeight": 1080, "outputWidth": 1920, "outputHeight": 1080}
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in
                               [f"Cam {n}" for n in range(1, 8)]
                               + [f"MV Cam {n}" for n in range(1, 8)]]}
        return {}


def _bootstrap_seed(imag, monkeypatch, tmp_path, saved):
    home = tmp_path / "home"
    (home / ".config").mkdir(parents=True)
    (home / ".config" / "imag-last-program").write_text(saved + "\n")
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setattr(imag, "BOOTSTRAP", True)
    obs = SeedObs()
    imag.seed(obs)
    return [(t, d) for t, d in obs.calls if t in _SELECTS]


def test_imag_bootstrap_never_restores_a_saved_production_scene(monkeypatch, tmp_path, capsys):
    imag = _imag()
    selects = _bootstrap_seed(imag, monkeypatch, tmp_path, "PRO")
    assert selects == [("SetCurrentProgramScene", {"sceneName": "Cam 1"})], (
        "a saved production scene falls back to the seed's own default, never selected")
    assert "production scene" in capsys.readouterr().out


def test_imag_bootstrap_still_restores_a_saved_operator_scene(monkeypatch, tmp_path):
    imag = _imag()
    selects = _bootstrap_seed(imag, monkeypatch, tmp_path, "Cam 3")
    assert selects == [("SetCurrentProgramScene", {"sceneName": "Cam 3"})]


def test_imag_bootstrap_selects_nothing_when_the_guard_is_unavailable(monkeypatch, tmp_path,
                                                                     capsys):
    imag = _imag()
    monkeypatch.setattr(imag, "_obs_phase2_module", lambda: None)
    selects = _bootstrap_seed(imag, monkeypatch, tmp_path, "Cam 3")
    assert selects == [], "an unguardable scene selection is never sent"
    assert "guard" in capsys.readouterr().out


def _pre_guard_obs_phase2():
    """An obs_phase2.py from before the guard (a half-finished setup-imag fetch, a hand copy): it
    carries the name-heal policy but not the production-scene guard."""
    return types.SimpleNamespace(REENFORCE_HEALED="healed",
                                 reenforce_ndi_name=lambda ws, inp, name: "offline")


def test_imag_an_obs_phase2_without_the_guard_never_breaks_a_request(monkeypatch):
    # review round 1: an importable obs_phase2 WITHOUT the guard must read as "no guard", never
    # crash every Obs.req (a crashed boot seed Restart-loops the imag OBS, the issue-1156 class).
    imag = _imag()
    monkeypatch.setattr(imag, "_obs_phase2_module", _pre_guard_obs_phase2)
    obs, ws = _imag_obs(imag, monkeypatch, {"GetVideoSettings": {"fpsNumerator": 60}})
    assert obs.req("GetVideoSettings") == {"fpsNumerator": 60}
    assert ws.sent == [("GetVideoSettings", {})]


def test_imag_bootstrap_selects_nothing_with_an_obs_phase2_without_the_guard(monkeypatch,
                                                                            tmp_path, capsys):
    imag = _imag()
    monkeypatch.setattr(imag, "_obs_phase2_module", _pre_guard_obs_phase2)
    selects = _bootstrap_seed(imag, monkeypatch, tmp_path, "Cam 3")
    assert selects == [], "an unguardable scene selection is never sent"
    assert "guard" in capsys.readouterr().out


# ---- cg_chain_scene.py (rides obs_phase2._rpc; refuses the target before its snapshot) ----------


def _cg():
    return _load(SCRIPTS / "cg_chain_scene.py", "cg_chain_scene_pro_guard_1380")


class CgRpc:
    """A scripted rpc(rtype, rdata) for cg_chain_scene's glue; records every call."""

    def __init__(self, items_by_scene, program="Cam 1"):
        self.items = items_by_scene
        self.program = program
        self.calls = []

    def __call__(self, rtype, rdata=None):
        rdata = dict(rdata or {})
        self.calls.append((rtype, rdata))
        if rtype == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.items]}
        if rtype == "GetSceneItemList":
            return {"sceneItems": [dict(i) for i in self.items[rdata["sceneName"]]]}
        if rtype == "GetCurrentProgramScene":
            return {"currentProgramSceneName": self.program}
        if rtype == "GetCurrentSceneTransition":
            return {"transitionName": "Fade"}
        if rtype == "GetSceneTransitionList":
            return {"transitions": [{"transitionName": "Cut", "transitionKind": "cut_transition"}]}
        return {}

    def writes(self):
        return [c for c in self.calls if c[0].startswith(("Set", "Create", "Remove"))]


def test_cg_program_select_refuses_the_production_scene_before_any_request(tmp_path):
    cg = _cg()
    rpc = CgRpc({"PRO": [], "sp-fast": []})
    state = tmp_path / "cg.json"
    with pytest.raises(RuntimeError) as exc:
        cg.program_select(rpc, "10.77.9.204", "PRO", str(state))
    assert _is_refusal(exc.value)
    assert rpc.calls == [], "refused before the snapshot and before any request"
    assert not state.exists()


def test_cg_strih_solo_refuses_a_production_scene_before_any_write(tmp_path):
    cg = _cg()
    rpc = CgRpc({"PRO": [{"sceneItemId": 1, "sourceName": "CG-obs", "sceneItemEnabled": False}]})
    state = tmp_path / "strih.json"
    with pytest.raises(RuntimeError) as exc:
        cg.strih_solo(rpc, "10.77.9.204", "CG-obs", "", str(state))
    assert _is_refusal(exc.value)
    assert rpc.writes() == []
    assert not state.exists()


def test_cg_cli_refusal_exits_2_and_closes_its_session(monkeypatch, tmp_path, capsys):
    cg = _cg()
    rpc = CgRpc({"PRO": []})
    closed = []
    fake_ws = types.SimpleNamespace(close=lambda: closed.append(True))
    monkeypatch.setattr(cg, "_ws_session", lambda host: (fake_ws, rpc))
    rc = cg.main(["program", "--host", "10.77.9.204", "--scene", "PRO",
                  "--state-file", str(tmp_path / "cg.json")])
    assert rc == 2
    assert "PRO" in capsys.readouterr().err
    assert rpc.writes() == []
    assert closed == [True]


def test_cg_session_transport_refuses_a_scene_uuid(monkeypatch):
    cg = _cg()
    ws = ScriptedWS()
    monkeypatch.setattr(cg._obs_phase2(), "_conn", lambda host, password="": ws)
    _, rpc = cg._ws_session("10.77.9.204")
    with pytest.raises(RuntimeError) as exc:
        rpc("SetCurrentProgramScene", {"sceneName": "sp-fast", "sceneUuid": "pro-uuid"})
    assert _is_refusal(exc.value)
    assert ws.sent == []


def test_cg_restore_never_reselects_a_production_program_and_restores_the_rest(tmp_path, capsys):
    cg = _cg()
    state = tmp_path / "strih.json"
    state.write_text(json.dumps({
        "host": "10.77.9.204", "scene": "CG bridge", "prev_program": "PRO",
        "prev_transition": "Fade", "items": [{"id": 7, "enabled": False}],
    }))
    rpc = CgRpc({"CG bridge": []})
    assert cg.restore(lambda host: rpc, str(state)) is True
    assert _selects_of(rpc.calls, "PRO") == []
    assert ("SetSceneItemEnabled", {"sceneName": "CG bridge", "sceneItemId": 7,
                                    "sceneItemEnabled": False}) in rpc.calls
    assert ("SetCurrentSceneTransition", {"transitionName": "Fade"}) in rpc.calls
    assert not state.exists(), "the snapshot is still retired"
    err = capsys.readouterr().err
    assert "never" in err and "PRO" in err


def test_cg_restore_calls_drop_only_a_forbidden_program():
    cg = _cg()
    state = {"scene": None, "prev_program": "PRO", "prev_transition": "Fade", "items": []}
    assert cg.restore_calls(state, frozenset({"PRO"})) == [
        ("SetCurrentSceneTransition", {"transitionName": "Fade"})]
    state["prev_program"] = "sp-slow"
    assert cg.restore_calls(state, frozenset({"PRO"}))[0] == (
        "SetCurrentProgramScene", {"sceneName": "sp-slow"})


# ---- warm_cam_scenes.py (rides obs_phase2._rpc; its preview restore) ----------------------------


def test_warm_cam_scenes_never_restores_a_production_preview(monkeypatch, capsys):
    warm = _load(SCRIPTS / "warm_cam_scenes.py", "warm_cam_scenes_pro_guard_1380")
    calls = []

    def guarded(ws, rtype, rdata=None, ignore_err=False):
        warm.op._refuse_forbidden_scene(rtype, rdata)
        calls.append((rtype, dict(rdata or {})))
        if rtype == "GetStudioModeEnabled":
            return {"studioModeEnabled": True}
        if rtype == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": "PRO"}
        return {}

    monkeypatch.setattr(warm.op, "_rpc", guarded)
    monkeypatch.setattr(warm.time, "sleep", lambda s: None)
    assert warm.warm_all(object(), ["Cam 1"], 0.0) == ["Cam 1"]
    assert _selects_of(calls, "PRO") == []
    assert "never" in capsys.readouterr().err


# ---- the rule lives in obs_phase2 only; every scene-selecting client is covered -----------------

_CLIENTS = ("glk_wire.py", "frozen-camera-gate.py", "imag_scenes.py", "cg_chain_scene.py",
            "warm_cam_scenes.py")


@pytest.mark.parametrize("name", _CLIENTS)
def test_client_reuses_the_one_guard_and_holds_no_copy_of_the_rule(name):
    src = (SCRIPTS / name).read_text()
    assert not re.search(r"""["']PRO["']""", src), f"{name} retypes the production scene name"
    assert "NEVER_PROGRAM_SCENES =" not in src and "_SCENE_SELECTING_REQUESTS" not in src
    assert "_refuse_forbidden_scene" in src or "NEVER_PROGRAM_SCENES" in src, (
        f"{name} must reuse obs_phase2's guard")


# Every request that can put a scene on program/preview: the obs-websocket v5 selections, the v4
# names, and the Studio Mode transition (it moves the preview onto program). A request name only
# counts QUOTED, as a client sends it; the shell scripts only mention them in comments.
_SCENE_SELECT_REQUEST_RE = re.compile(
    r"""["'](SetCurrentProgramScene|SetCurrentPreviewScene|SetCurrentScene|SetPreviewScene"""
    r"""|TransitionToProgram|TriggerStudioModeTransition)["']""")
_SCRIPT_SUFFIXES = (".py", ".sh", ".ps1", ".psm1", ".js", ".ahk", ".cmd", ".bat")


def test_every_scene_selecting_script_is_a_covered_client():
    # A new OBS-WS client that selects a scene must join this list (and reuse the guard; a
    # TriggerStudioModeTransition user must also be checked against the rule by hand).
    covered = set(_CLIENTS) | {"obs_phase2.py", "stream_dev_scene.py"}
    senders = {p.name for p in SCRIPTS.rglob("*")
               if p.is_file() and p.suffix in _SCRIPT_SUFFIXES
               and _SCENE_SELECT_REQUEST_RE.search(p.read_text(errors="replace"))}
    assert senders <= covered, f"uncovered scene-selecting scripts: {sorted(senders - covered)}"
    assert senders == covered, f"a covered client stopped selecting scenes: {covered - senders}"
