"""issue 1242 — the strih scene ROLE lib: program-path cameras connect only while shown, the built-in
multiview renders always-connected low-bandwidth `MV` twins, and an E2E run holds every program-path
input connected for the measurement.

Owner ruling 24.9.2026: strih-lx pulls FULL bandwidth only for cameras that are shown (PVW, PGM, a
projector, the visible item of the Grading NDI-output scene); the multiview uses `MV Cam N` twins.

Covers:
  * scripts/strih_scenes.py role planners (pure) + apply_bandwidth_roles against a fake OBS;
  * scripts/obs_phase2.py `connect-on-show` hold/restore (the E2E precondition) + hidden_by_design;
  * scripts/set-ndi-mapping.py --verify-live never calls a parked input FROZEN;
  * scripts/recording-e2e.sh + scripts/lib/connect-on-show-hold.sh wiring (guarded hold after the
    cleanup trap arms, restore inside cleanup).
"""
import copy
import importlib.util
import json
import pathlib
import subprocess
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"


def _load(name, path):
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


ss = _load("strih_scenes_1242", SCRIPTS / "strih_scenes.py")
op = _load("obs_phase2_1242", SCRIPTS / "obs_phase2.py")
snm = _load("set_ndi_mapping_1242", SCRIPTS / "set-ndi-mapping.py")

PLAN = ss.seed_inputs([
    {"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"},
    {"sender": "CAM3 (usb)", "input": "NDI cam3", "scene": "Cam 3"},
    {"sender": "STRIH-LX (2ME PVW)", "input": "NDI 2ME PVW", "scene": "2ME PVW"},
    {"sender": "RESOLUME-SNV (cg-obs)", "input": "cg", "scene": "CG"},
], 3)

T_FULL = {"positionX": 0, "positionY": 0, "scaleX": 1.0, "scaleY": 1.0}


# ------------------------------------------------------------------------------------------------
# pure planners
# ------------------------------------------------------------------------------------------------

def test_program_path_is_the_fleet_camera_inputs_only():
    # cameras only: never the 2ME feedback pair, never the cg input (a CG cut-in must stay instant).
    assert ss.program_path_inputs(PLAN) == ["NDI cam1", "NDI cam3"]
    assert ss.is_program_path_sender("CAM7 (usb)")
    assert not ss.is_program_path_sender("RESOLUME-SNV (cg-obs)")
    assert not ss.is_program_path_sender("STRIH-LX (2ME PGM)")


def test_role_settings():
    assert ss.main_role_settings() == {"genlock_connect_on_show": True}
    twin = ss.twin_input_settings({"ndi_source_name": "CAM3 (usb)", "genlock_latency_ms_src": 6})
    assert twin == {
        "ndi_source_name": "CAM3 (usb)",
        "genlock_fifo": True,
        "ndi_sync": 2,
        "genlock_latency_ms_src": 6,
        "genlock_monitor": True,
        "genlock_connect_on_show": False,
        "ndi_audio": False,
    }
    assert ss.twin_name("Cam 3") == "MV Cam 3" and ss.twin_name("NDI cam3") == "MV NDI cam3"


def _item(iid, name, enabled=True, kind="ndi_source", stype="OBS_SOURCE_TYPE_INPUT"):
    return {"sceneItemId": iid, "sourceName": name, "sceneItemEnabled": enabled,
            "sceneItemTransform": dict(T_FULL, width=1920, height=1080), "inputKind": kind,
            "sourceType": stype}


def test_scene_needs_twin():
    prog = ["NDI cam1", "NDI cam3"]
    cam3 = [_item(1, "NDI cam3"), _item(2, "ASIO zvuk", kind="pulse_input_capture")]
    moder = [_item(1, "NDI cam3"), _item(2, "Image", kind="image_source")]
    grading = [_item(1, "Cam 1", False, kind=None, stype="OBS_SOURCE_TYPE_SCENE")]
    interkom = [_item(1, "NDI 2ME PVW")]
    assert ss.scene_needs_twin("Cam 3", cam3, prog, is_output_scene=False)
    assert ss.scene_needs_twin("Moderatori", moder, prog, is_output_scene=False)
    # nested scene refs only -> not a direct program input -> no twin (Grading keeps its full input)
    assert not ss.scene_needs_twin("Grading", grading, prog, is_output_scene=True)
    # an NDI-output scene is never twinned (its output needs the scene SHOWN as-is)
    assert not ss.scene_needs_twin("Cam 3", cam3, prog, is_output_scene=True)
    assert not ss.scene_needs_twin("Interkom", interkom, prog, is_output_scene=True)
    # a twin is never twinned again
    assert not ss.scene_needs_twin("MV Cam 3", [_item(1, "MV NDI cam3")], prog, is_output_scene=False)


def test_twin_scene_items_swap_program_inputs_and_drop_audio_only():
    prog = ["NDI cam3"]
    items = [_item(1, "NDI cam3"), _item(2, "ASIO zvuk", kind="pulse_input_capture"),
             _item(3, "Odpocet", False, kind="browser_source")]
    got = ss.twin_scene_items(items, prog)
    assert [(i["sourceName"], i["sceneItemEnabled"]) for i in got] == [
        ("MV NDI cam3", True), ("Odpocet", False)]
    # only settable transform fields are carried (read-only width/height dropped)
    assert got[0]["sceneItemTransform"] == T_FULL


def test_twin_items_match_compares_source_and_enabled_in_order():
    desired = [{"sourceName": "MV NDI cam3", "sceneItemEnabled": True, "sceneItemTransform": T_FULL}]
    assert ss.twin_items_match([_item(9, "MV NDI cam3")], desired)
    assert not ss.twin_items_match([_item(9, "MV NDI cam3", False)], desired)
    assert not ss.twin_items_match([], desired)


def test_custom_multiview_swap_plan():
    prog = ["NDI cam1", "NDI cam3"]
    items = [_item(1, "NDI cam1"), _item(2, "NDI 2ME PGM (mv)"),
             _item(3, "Cam 3", kind=None, stype="OBS_SOURCE_TYPE_SCENE")]
    plan = ss.multiview_swap_plan(items, prog, {"Cam 3": "MV Cam 3"})
    assert [(p["old_item_id"], p["new_name"]) for p in plan] == [(1, "MV NDI cam1"), (3, "MV Cam 3")]
    assert ss.is_custom_multiview_scene("MULTIVIEW") and ss.is_custom_multiview_scene("Multiview")
    assert not ss.is_custom_multiview_scene("MV Cam 1")


def test_bandwidth_role_problems_is_a_report():
    actual = {
        "NDI cam1": {"genlock_connect_on_show": True},
        "MV NDI cam1": {"genlock_monitor": True},
        "NDI cam3": {},
    }
    probs = ss.bandwidth_role_problems(actual, ["NDI cam1", "NDI cam3"])
    assert probs == ["'NDI cam3' not connect-on-show", "'MV NDI cam3' twin MISSING"]


# ------------------------------------------------------------------------------------------------
# live apply against a fake OBS
# ------------------------------------------------------------------------------------------------

class FakeObs:
    """A minimal in-memory obs-websocket: inputs, scenes (ordered items), private settings, filters."""

    def __init__(self):
        self.inputs = {
            "NDI cam1": {"kind": "ndi_source", "settings": {"ndi_source_name": "CAM1 (usb)",
                                                          "genlock_fifo": True, "ndi_sync": 2,
                                                          "genlock_latency_ms_src": 3}},
            "NDI cam3": {"kind": "ndi_source", "settings": {"ndi_source_name": "CAM3 (usb)",
                                                          "genlock_fifo": True, "ndi_sync": 2,
                                                          "genlock_latency_ms_src": 6}},
            "NDI 2ME PVW": {"kind": "ndi_source", "settings": {"ndi_source_name": "STRIH-LX (2ME PVW)"}},
            "ASIO zvuk": {"kind": "pulse_input_capture", "settings": {}},
            "Image": {"kind": "image_source", "settings": {}},
        }
        self.scenes = {
            "Cam 1": [_item(1, "NDI cam1"), _item(2, "ASIO zvuk", kind="pulse_input_capture")],
            "Cam 3": [_item(1, "NDI cam3"), _item(2, "ASIO zvuk", kind="pulse_input_capture")],
            "Moderatori": [_item(1, "NDI cam3"), _item(2, "Image", kind="image_source")],
            "Grading": [_item(1, "Cam 1", False, kind=None, stype="OBS_SOURCE_TYPE_SCENE"),
                        _item(2, "Cam 3", True, kind=None, stype="OBS_SOURCE_TYPE_SCENE")],
            "Interkom": [_item(1, "NDI 2ME PVW")],
            "MULTIVIEW": [_item(1, "NDI cam1"), _item(2, "NDI cam3")],
        }
        self.private = {"MULTIVIEW": {"show_in_multiview": False}}
        self.filters = {"Grading": [{"filterKind": "ndi_filter", "filterEnabled": True}],
                        "Interkom": [{"filterKind": "ndi_filter", "filterEnabled": True}],
                        "MULTIVIEW": [{"filterKind": "ndi_filter", "filterEnabled": False}]}
        self.calls = []
        self._next_id = 100

    def _new_id(self):
        self._next_id += 1
        return self._next_id

    def req(self, rt, data=None, ignore_err=False):
        data = data or {}
        self.calls.append((rt, copy.deepcopy(data)))
        if rt == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": v["kind"]} for n, v in self.inputs.items()]}
        if rt == "GetInputSettings":
            i = self.inputs.get(data["inputName"])
            return {"inputSettings": dict(i["settings"])} if i else {}
        if rt == "GetInputDefaultSettings":
            return {"defaultInputSettings": {"ndi_sync": 2, "genlock_monitor": False,
                                             "genlock_connect_on_show": False}}
        if rt == "SetInputSettings":
            self.inputs[data["inputName"]]["settings"].update(data["inputSettings"])
            return {}
        if rt == "GetSceneList":
            return {"scenes": [{"sceneName": n} for n in reversed(list(self.scenes))]}
        if rt == "GetSceneItemList":
            return {"sceneItems": copy.deepcopy(self.scenes.get(data["sceneName"], []))}
        if rt == "CreateScene":
            self.scenes.setdefault(data["sceneName"], [])
            return {}
        if rt == "RemoveScene":
            self.scenes.pop(data["sceneName"], None)
            return {}
        if rt == "CreateInput":
            if data["inputName"] in self.inputs:
                return {}
            self.inputs[data["inputName"]] = {"kind": data["inputKind"],
                                              "settings": dict(data.get("inputSettings", {}))}
            iid = self._new_id()
            self.scenes[data["sceneName"]].append(_item(iid, data["inputName"]))
            return {"sceneItemId": iid}
        if rt == "CreateSceneItem":
            iid = self._new_id()
            kind = self.inputs.get(data["sourceName"], {}).get("kind")
            stype = "OBS_SOURCE_TYPE_SCENE" if data["sourceName"] in self.scenes else "OBS_SOURCE_TYPE_INPUT"
            it = _item(iid, data["sourceName"], data.get("sceneItemEnabled", True), kind, stype)
            self.scenes[data["sceneName"]].append(it)
            return {"sceneItemId": iid}
        if rt == "RemoveSceneItem":
            self.scenes[data["sceneName"]] = [i for i in self.scenes[data["sceneName"]]
                                              if i["sceneItemId"] != data["sceneItemId"]]
            return {}
        if rt == "SetSceneItemEnabled":
            for i in self.scenes[data["sceneName"]]:
                if i["sceneItemId"] == data["sceneItemId"]:
                    i["sceneItemEnabled"] = data["sceneItemEnabled"]
            return {}
        if rt in ("SetSceneItemTransform", "SetInputMute"):
            return {}
        if rt == "GetSourcePrivateSettings":
            return {"sourceSettings": dict(self.private.get(data["sourceName"], {}))}
        if rt == "SetSourcePrivateSettings":
            self.private.setdefault(data["sourceName"], {}).update(data["sourceSettings"])
            return {}
        if rt == "GetSourceFilterList":
            return {"filters": list(self.filters.get(data["sourceName"], []))}
        raise AssertionError(f"unexpected request {rt}")


def _mv(obs, scene):
    return obs.private.get(scene, {}).get("show_in_multiview", True)


def test_apply_bandwidth_roles_on_the_live_strih_shape():
    obs = FakeObs()
    ss.apply_bandwidth_roles(obs, PLAN)
    # program-path mains connect only while shown; the cg / 2ME inputs are untouched
    assert obs.inputs["NDI cam1"]["settings"]["genlock_connect_on_show"] is True
    assert obs.inputs["NDI cam3"]["settings"]["genlock_connect_on_show"] is True
    assert "genlock_connect_on_show" not in obs.inputs["NDI 2ME PVW"]["settings"]
    # the twins: always connected, low bandwidth, bound to the main's LIVE sender + pin
    tw = obs.inputs["MV NDI cam3"]["settings"]
    assert tw["genlock_monitor"] is True and tw["genlock_connect_on_show"] is False
    assert tw["ndi_source_name"] == "CAM3 (usb)" and tw["genlock_latency_ms_src"] == 6
    # twin scenes for every multiview scene that holds a program input directly
    assert [i["sourceName"] for i in obs.scenes["MV Cam 3"]] == ["MV NDI cam3"]
    assert [i["sourceName"] for i in obs.scenes["MV Moderatori"]] == ["MV NDI cam3", "Image"]
    # built-in multiview membership: originals out, twins in; NDI-output scenes stay shown
    for orig in ("Cam 1", "Cam 3", "Moderatori"):
        assert _mv(obs, orig) is False and _mv(obs, "MV " + orig) is True
    assert _mv(obs, "Grading") is True and _mv(obs, "Interkom") is True
    assert "MV Grading" not in obs.scenes and "MV Interkom" not in obs.scenes
    # the custom MULTIVIEW scene renders twins, never the full inputs
    assert [i["sourceName"] for i in obs.scenes["MULTIVIEW"]] == ["MV NDI cam1", "MV NDI cam3"]
    # the built-in multiview was refreshed (a scene-list change) and the temp scene is gone
    assert not any(n.startswith("__") for n in obs.scenes)
    assert any(c[0] == "RemoveScene" for c in obs.calls)


def test_apply_bandwidth_roles_is_idempotent():
    obs = FakeObs()
    ss.apply_bandwidth_roles(obs, PLAN)
    obs.calls.clear()
    ss.apply_bandwidth_roles(obs, PLAN)
    writes = [c for c in obs.calls if c[0].startswith(("Set", "Create", "Remove"))]
    assert writes == [], f"a second apply over a correct collection must be read-only: {writes}"


# ------------------------------------------------------------------------------------------------
# obs_phase2: the E2E connect-on-show hold + hidden_by_design
# ------------------------------------------------------------------------------------------------

class FakeWs:
    pass


def _fake_rpc(state):
    def rpc(ws, rt, rdata=None, ignore_err=False, timeout_s=None):
        rdata = rdata or {}
        state["calls"].append((rt, rdata))
        if rt == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": "ndi_source"} for n in state["inputs"]]}
        if rt == "GetInputSettings":
            return {"inputSettings": dict(state["inputs"][rdata["inputName"]])}
        if rt == "SetInputSettings":
            state["inputs"][rdata["inputName"]].update(rdata["inputSettings"])
            return {}
        if rt == "GetSourceActive":
            return {"videoActive": False, "videoShowing": state["showing"].get(rdata["sourceName"], True)}
        raise AssertionError(rt)
    return rpc


def test_connect_on_show_hold_and_restore(tmp_path, monkeypatch):
    state = {"calls": [], "showing": {}, "inputs": {
        "NDI cam1": {"genlock_connect_on_show": True},
        "NDI cam3": {"genlock_connect_on_show": True},
        "MV NDI cam3": {"genlock_monitor": True},
        "NDI 2ME PVW": {},
    }}
    monkeypatch.setattr(op, "_rpc", _fake_rpc(state))
    sf = tmp_path / "hold.json"
    held, failed = op.connect_on_show_hold(FakeWs(), str(sf))
    assert held == ["NDI cam1", "NDI cam3"] and failed == []
    assert state["inputs"]["NDI cam1"]["genlock_connect_on_show"] is False
    assert json.loads(sf.read_text()) == ["NDI cam1", "NDI cam3"]
    # a second hold (e.g. a crashed run left the state file) keeps the union -> restore catches all
    held2, _ = op.connect_on_show_hold(FakeWs(), str(sf))
    assert held2 == ["NDI cam1", "NDI cam3"]
    restored, failed = op.connect_on_show_restore(FakeWs(), str(sf))
    assert restored == ["NDI cam1", "NDI cam3"] and failed == []
    assert state["inputs"]["NDI cam3"]["genlock_connect_on_show"] is True
    assert not sf.exists(), "a clean restore removes the state file"
    # restore with no state file is a no-op
    assert op.connect_on_show_restore(FakeWs(), str(sf)) == ([], [])


def test_connect_on_show_subcommand_parses(monkeypatch):
    got = {}
    monkeypatch.setattr(op, "connect_on_show", lambda a: got.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "connect-on-show", "--host", "H", "--hold", "/x"])
    op.main()
    assert got["host"] == "H" and got["hold"] == "/x" and got["restore"] is None


def test_hidden_by_design():
    assert op.hidden_by_design({"genlock_fifo": True, "genlock_connect_on_show": True}, showing=False)
    assert not op.hidden_by_design({"genlock_fifo": True, "genlock_connect_on_show": True}, showing=True)
    assert not op.hidden_by_design({"genlock_fifo": True}, showing=False)  # issue-764 keep-alive role
    assert not op.hidden_by_design({"genlock_fifo": True, "genlock_connect_on_show": True,
                                    "genlock_monitor": True}, showing=False)  # a twin never parks
    assert not op.hidden_by_design({"genlock_connect_on_show": True}, showing=False)  # not genlocked


def test_verify_live_skips_a_hidden_by_design_input():
    logs = []
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    sampled = []

    def sampler(ws, inp):
        sampled.append(inp)
        return op.LIVENESS_FROZEN, "held frame"

    live, frozen, inc = snm.verify_live_mapping(op, None, want, sampler, logs.append,
                                                hidden=lambda ws, inp: inp == "NDI cam2")
    assert sampled == ["NDI cam1"], "a parked input is never screenshot-sampled"
    assert (live, frozen, inc) == (0, 1, 0)
    assert any("hidden by design" in m for m in logs)


# ------------------------------------------------------------------------------------------------
# recording-e2e.sh wiring
# ------------------------------------------------------------------------------------------------

E2E = (SCRIPTS / "recording-e2e.sh").read_text()
LIB = SCRIPTS / "lib" / "connect-on-show-hold.sh"


def test_e2e_holds_connect_on_show_after_the_trap_behind_a_rig_busy_guard():
    trap = E2E.index("\ntrap cleanup EXIT HUP INT TERM\n")
    hold = E2E.index('connect_on_show_e2e_hold "$HERE"')
    guard = E2E.rindex('stray_session_check_assert "$HERE"', 0, hold)
    first_deploy = E2E.index('echo "[2/8] $CAMERA_NAME')
    assert trap < guard < hold < first_deploy
    assert '. "$HERE/lib/connect-on-show-hold.sh"' in E2E
    line = [ln for ln in E2E.splitlines() if 'connect_on_show_e2e_hold "$HERE"' in ln][0]
    assert "|| exit 1" in line, "a failed hold must abort the run (the measurement would be wrong)"


def test_e2e_cleanup_restores_connect_on_show():
    body = E2E[E2E.index("\ncleanup() {\n"):E2E.index("\ntrap cleanup EXIT HUP INT TERM\n")]
    assert 'connect_on_show_e2e_restore "$HERE"' in body


def _bash_lib(body):
    return subprocess.run(["bash", "-c", f"set -euo pipefail; . '{LIB}'; {body}"],
                          capture_output=True, text=True, check=False)


def test_hold_lib_fails_loud_and_restore_never_aborts(tmp_path):
    fake = tmp_path / "obs_phase2.py"
    fake.write_text("import sys\nprint('ARGS', sys.argv[1:])\nsys.exit(int(__import__('os').environ.get('RC','0')))\n")
    ok = _bash_lib(f"HERE_PY='{tmp_path}'; connect_on_show_e2e_hold '{tmp_path}' 10.0.0.1 /tmp/s.json")
    assert ok.returncode == 0 and "--hold" in ok.stdout
    bad = subprocess.run(["bash", "-c", f"set -euo pipefail; . '{LIB}'; "
                          f"RC=1 connect_on_show_e2e_hold '{tmp_path}' 10.0.0.1 /tmp/s.json"],
                         capture_output=True, text=True, check=False, env={"RC": "1", "PATH": "/usr/bin:/bin"})
    assert bad.returncode != 0
    rest = subprocess.run(["bash", "-c", f"set -euo pipefail; . '{LIB}'; "
                           f"connect_on_show_e2e_restore '{tmp_path}' 10.0.0.1 /tmp/s.json; echo DONE"],
                          capture_output=True, text=True, check=False, env={"RC": "1", "PATH": "/usr/bin:/bin"})
    assert rest.returncode == 0 and "DONE" in rest.stdout, "restore must never abort cleanup()"
