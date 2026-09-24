"""issue 1242 — the strih BANDWIDTH ROLES: program-path cameras connect only while shown, the built-in
multiview renders always-connected low-bandwidth `MV` twins (each standing for its program scene), and
an E2E run holds every program-path input connected for the measurement.

Owner ruling 24.9.2026: strih-lx pulls FULL bandwidth only for cameras that are shown (PVW, PGM, a
projector, the visible item of the Grading NDI-output scene); the multiview uses `MV Cam N` twins.

Covers:
  * scripts/strih_bandwidth_roles.py pure planners + apply_bandwidth_roles against a fake OBS
    (twin sizing, nested scenes, drift, operator-wins membership, retirement, empty/colliding names);
  * the vendored OBS multiview cell-target key (python <-> C++ literal pin);
  * scripts/strih_scenes.py --apply-roles delegation + the launch path;
  * scripts/obs_phase2.py `connect-on-show` hold/restore (the E2E precondition) + hidden_by_design;
  * scripts/set-ndi-mapping.py --verify-live never calls a parked input FROZEN;
  * scripts/recording-e2e.sh + scripts/lib/connect-on-show-hold.sh wiring (a stable state path, a
    guarded hold after the cleanup trap arms, a bounded wait for the held inputs to deliver, restore
    inside cleanup).
"""
import copy
import importlib.util
import json
import pathlib
import subprocess
import sys
import textwrap

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
roles = _load("strih_bandwidth_roles_1242", SCRIPTS / "strih_bandwidth_roles.py")
op = _load("obs_phase2_1242", SCRIPTS / "obs_phase2.py")
snm = _load("set_ndi_mapping_1242", SCRIPTS / "set-ndi-mapping.py")

PLAN = ss.seed_inputs([
    {"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"},
    {"sender": "CAM3 (usb)", "input": "NDI cam3", "scene": "Cam 3"},
    {"sender": "STRIH-LX (2ME PVW)", "input": "NDI 2ME PVW", "scene": "2ME PVW"},
    {"sender": "RESOLUME-SNV (cg-obs)", "input": "cg", "scene": "CG"},
], 3)

CANVAS = (1920, 1080)
# a main placed by SCALE with no bounds (the no-bounds case a lower-resolution twin would shrink in)
T_SCALE = {"positionX": 0.0, "positionY": 0.0, "scaleX": 1.0, "scaleY": 1.0,
           "boundsType": "OBS_BOUNDS_NONE", "width": 1920.0, "height": 1080.0}
SCENE = "OBS_SOURCE_TYPE_SCENE"


def _item(iid, name, enabled=True, kind="ndi_source", stype="OBS_SOURCE_TYPE_INPUT", transform=None):
    return {"sceneItemId": iid, "sourceName": name, "sceneItemEnabled": enabled,
            "sceneItemTransform": dict(transform or T_SCALE), "inputKind": kind, "sourceType": stype}


# ------------------------------------------------------------------------------------------------
# pure planners
# ------------------------------------------------------------------------------------------------

def test_program_path_is_the_fleet_camera_inputs_only():
    # cameras only: never the 2ME feedback pair, never the cg input (a CG cut-in must stay instant).
    assert roles.program_path_inputs(PLAN) == ["NDI cam1", "NDI cam3"]
    assert roles.is_program_path_sender("CAM7 (usb)")
    assert not roles.is_program_path_sender("RESOLUME-SNV (cg-obs)")
    assert not roles.is_program_path_sender("STRIH-LX (2ME PGM)")


def test_role_settings():
    assert roles.main_role_settings() == {"genlock_connect_on_show": True}
    main = {"ndi_source_name": "CAM3 (usb)", "genlock_latency_ms_src": 6}
    role = {"genlock_fifo": True, "ndi_sync": 2, "genlock_latency_ms_src": 6, "genlock_monitor": True,
            "genlock_connect_on_show": False, "ndi_audio": False}
    assert roles.twin_role_settings(main) == role
    assert roles.twin_input_settings(main) == dict(role, ndi_source_name="CAM3 (usb)")
    assert roles.twin_name("Cam 3") == "MV Cam 3" and roles.twin_name("NDI cam3") == "MV NDI cam3"


def test_twin_transform_pins_bounds_so_a_proxy_fills_the_main_footprint():
    # no bounds -> SCALE_INNER bounds at the main item's on-canvas size (the proxy never shrinks)
    t = roles.twin_transform(T_SCALE, CANVAS)
    assert t["boundsType"] == "OBS_BOUNDS_SCALE_INNER"
    assert (t["boundsWidth"], t["boundsHeight"]) == (1920.0, 1080.0)
    assert "width" not in t and "height" not in t  # read-only computed fields never echoed
    half = dict(T_SCALE, scaleX=0.5, scaleY=0.5, width=960.0, height=540.0, positionX=960.0)
    t = roles.twin_transform(half, CANVAS)
    assert (t["boundsWidth"], t["boundsHeight"], t["positionX"]) == (960.0, 540.0, 960.0)
    # no computed size at all -> the canvas
    t = roles.twin_transform({"boundsType": "OBS_BOUNDS_NONE"}, CANVAS)
    assert (t["boundsWidth"], t["boundsHeight"]) == (1920.0, 1080.0)
    # a main that already uses bounds keeps its transform
    bounded = {"boundsType": "OBS_BOUNDS_STRETCH", "boundsWidth": 640.0, "boundsHeight": 360.0,
               "width": 640.0, "height": 360.0}
    assert roles.twin_transform(bounded, CANVAS) == {"boundsType": "OBS_BOUNDS_STRETCH",
                                                     "boundsWidth": 640.0, "boundsHeight": 360.0}


def test_scenes_needing_twins_is_recursive_and_skips_output_scenes():
    prog = ["NDI cam1", "NDI cam3"]
    items = {
        "Cam 1": [_item(1, "NDI cam1"), _item(2, "ASIO zvuk", kind="pulse_input_capture")],
        "Cam 3": [_item(1, "NDI cam3")],
        "Moderatori": [_item(1, "NDI cam3"), _item(2, "Image", kind="image_source")],
        "Two cams": [_item(1, "Cam 1", kind=None, stype=SCENE), _item(2, "Cam 3", kind=None, stype=SCENE)],
        "Grading": [_item(1, "Cam 1", False, kind=None, stype=SCENE)],
        "Interkom": [_item(1, "NDI 2ME PVW")],
        "MULTIVIEW": [_item(1, "NDI cam1")],
        "MV Cam 1": [_item(1, "MV NDI cam1")],
        "Loop A": [_item(1, "Loop B", kind=None, stype=SCENE)],
        "Loop B": [_item(1, "Loop A", kind=None, stype=SCENE)],
    }
    got = roles.scenes_needing_twins(items, prog, output_scenes={"Grading", "Interkom"})
    # direct holders + a scene that only NESTS them; never an output scene, the grid, a twin, a cycle
    assert got == {"Cam 1", "Cam 3", "Moderatori", "Two cams"}


def test_twin_scene_items_swap_inputs_and_nested_scenes_and_drop_audio_only():
    prog = ["NDI cam3"]
    items = [_item(1, "NDI cam3"), _item(2, "ASIO zvuk", kind="pulse_input_capture"),
             _item(3, "Odpocet", False, kind="browser_source"),
             _item(4, "Cam 1", kind=None, stype=SCENE)]
    got = roles.twin_scene_items(items, prog, {"Cam 1": "MV Cam 1"}, CANVAS)
    assert [(i["sourceName"], i["sceneItemEnabled"]) for i in got] == [
        ("MV NDI cam3", True), ("Odpocet", False), ("MV Cam 1", True)]
    assert got[0]["sceneItemTransform"]["boundsType"] == "OBS_BOUNDS_SCALE_INNER"  # swapped -> pinned
    assert got[1]["sceneItemTransform"]["boundsType"] == "OBS_BOUNDS_NONE"         # reused as-is


def test_twin_items_match_includes_transform_drift():
    desired = roles.twin_scene_items([_item(1, "NDI cam3")], ["NDI cam3"], {}, CANVAS)
    current = [dict(_item(9, "MV NDI cam3"), sceneItemTransform=dict(desired[0]["sceneItemTransform"]))]
    assert roles.twin_items_match(current, desired)
    drifted = copy.deepcopy(current)
    drifted[0]["sceneItemTransform"]["positionX"] = 300.0
    assert not roles.twin_items_match(drifted, desired)
    assert not roles.twin_items_match([dict(current[0], sceneItemEnabled=False)], desired)
    assert not roles.twin_items_match([], desired)


def test_custom_multiview_swap_plan():
    prog = ["NDI cam1", "NDI cam3"]
    items = [_item(1, "NDI cam1"), _item(2, "NDI 2ME PGM (mv)"),
             _item(3, "Cam 3", kind=None, stype=SCENE)]
    plan = roles.multiview_swap_plan(items, prog, {"Cam 3": "MV Cam 3"}, CANVAS)
    assert [(p["old_item_id"], p["new_name"]) for p in plan] == [(1, "MV NDI cam1"), (3, "MV Cam 3")]
    assert plan[0]["transform"]["boundsType"] == "OBS_BOUNDS_SCALE_INNER"
    assert roles.is_custom_multiview_scene("MULTIVIEW") and roles.is_custom_multiview_scene("Multiview")
    assert not roles.is_custom_multiview_scene("MV Cam 1")


def test_membership_on_create():
    # the twin takes over the cell the original OR an already-shown twin held
    assert roles.membership_on_create(True, False) == (False, True)
    assert roles.membership_on_create(False, True) == (False, True)   # the migrated #501/#761 layout
    assert roles.membership_on_create(True, True) == (False, True)
    assert roles.membership_on_create(False, False) == (False, False)  # a nested-only twin never shown


def test_twin_transform_of_a_parked_main_uses_the_nominal_camera_size_and_drops_crop():
    # a parked / not-yet-delivering main reports width/height/sourceWidth 0 (async_active off); the
    # twin must still get the SAME footprint as when it is live, or the twin rebuilds every launch
    parked_half = {"positionX": 960.0, "positionY": 0.0, "scaleX": 0.5, "scaleY": 0.5,
                   "boundsType": "OBS_BOUNDS_NONE", "width": 0.0, "height": 0.0,
                   "sourceWidth": 0.0, "sourceHeight": 0.0, "cropLeft": 480, "cropRight": 0}
    live_half = dict(parked_half, width=960.0, height=540.0, sourceWidth=1920.0, sourceHeight=1080.0)
    a = roles.twin_transform(parked_half, CANVAS)
    b = roles.twin_transform(live_half, CANVAS)
    assert (a["boundsWidth"], a["boundsHeight"]) == (960.0, 540.0) == (b["boundsWidth"], b["boundsHeight"])
    # main-pixel crop values would over-crop the lower-resolution proxy: never mirrored
    assert "cropLeft" not in a and "cropRight" not in a


def test_e2e_hold_marker_keeps_the_mains_connected(tmp_path):
    marker = tmp_path / "connect-on-show-e2e-hold"
    marker.write_text("")
    now = marker.stat().st_mtime
    assert roles.e2e_hold_active(str(marker), now + 60, 4 * 3600)
    assert not roles.e2e_hold_active(str(marker), now + 5 * 3600, 4 * 3600)  # a stale marker expires
    assert not roles.e2e_hold_active(str(tmp_path / "absent"), now, 4 * 3600)
    obs = FakeObs()
    # the launch before the run applied the roles (mains connect-on-show) ...
    roles.apply_bandwidth_roles(obs, PLAN, hold_marker=str(tmp_path / "absent"), now=now)
    assert obs.inputs["NDI cam1"]["settings"]["genlock_connect_on_show"] is True
    # ... then OBS is relaunched mid-run while the marker is fresh
    summary = roles.apply_bandwidth_roles(obs, PLAN, hold_marker=str(marker), now=now + 60)
    assert summary["e2e_hold"] is True
    # an OBS relaunch DURING an E2E run must not re-park the held mains
    assert obs.inputs["NDI cam1"]["settings"]["genlock_connect_on_show"] is False
    assert obs.inputs["NDI cam3"]["settings"]["genlock_connect_on_show"] is False
    # ... while the twins, twin scenes and multiview membership are still applied
    assert "MV NDI cam3" in obs.inputs and _mv(obs, "MV Cam 3") is True


def test_bandwidth_role_problems_is_a_report():
    actual = {
        "NDI cam1": {"genlock_connect_on_show": True},
        "MV NDI cam1": {"genlock_monitor": True},
        "NDI cam3": {},
    }
    probs = roles.bandwidth_role_problems(actual, ["NDI cam1", "NDI cam3"])
    assert probs == ["'NDI cam3' not connect-on-show", "'MV NDI cam3' twin MISSING"]


def test_multiview_target_key_matches_the_vendored_frontend():
    cpp = (REPO / "vendor/obs-studio/frontend/components/Multiview.cpp").read_text()
    assert f'obs_data_get_string(priv, "{roles.MULTIVIEW_TARGET_KEY}")' in cpp


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
            "Grading": [_item(1, "Cam 1", False, kind=None, stype=SCENE),
                        _item(2, "Cam 3", True, kind=None, stype=SCENE)],
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
        if rt == "GetVideoSettings":
            return {"baseWidth": 1920, "baseHeight": 1080}
        if rt == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": v["kind"]} for n, v in self.inputs.items()]}
        if rt == "GetInputSettings":
            i = self.inputs.get(data["inputName"])
            return {"inputSettings": dict(i["settings"])} if i else {}
        if rt == "GetInputDefaultSettings":
            return {"defaultInputSettings": {"ndi_sync": 2, "genlock_monitor": False, "ndi_audio": True,
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
            self.scenes[data["sceneName"]].append(_item(iid, data["inputName"],
                                                        data.get("sceneItemEnabled", True)))
            return {"sceneItemId": iid}
        if rt == "CreateSceneItem":
            iid = self._new_id()
            kind = self.inputs.get(data["sourceName"], {}).get("kind")
            stype = SCENE if data["sourceName"] in self.scenes else "OBS_SOURCE_TYPE_INPUT"
            it = _item(iid, data["sourceName"], data.get("sceneItemEnabled", True), kind, stype)
            self.scenes[data["sceneName"]].append(it)
            return {"sceneItemId": iid}
        if rt == "RemoveSceneItem":
            self.scenes[data["sceneName"]] = [i for i in self.scenes[data["sceneName"]]
                                              if i["sceneItemId"] != data["sceneItemId"]]
            return {}
        if rt == "SetSceneItemTransform":
            for i in self.scenes[data["sceneName"]]:
                if i["sceneItemId"] == data["sceneItemId"]:
                    i["sceneItemTransform"].update(data["sceneItemTransform"])
            return {}
        if rt == "SetInputMute":
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


def _target(obs, scene):
    return obs.private.get(scene, {}).get(roles.MULTIVIEW_TARGET_KEY)


def _writes(obs):
    return [c for c in obs.calls if c[0].startswith(("Set", "Create", "Remove"))]


def test_apply_bandwidth_roles_on_the_live_strih_shape():
    obs = FakeObs()
    roles.apply_bandwidth_roles(obs, PLAN)
    # program-path mains connect only while shown; the cg / 2ME inputs are untouched
    assert obs.inputs["NDI cam1"]["settings"]["genlock_connect_on_show"] is True
    assert obs.inputs["NDI cam3"]["settings"]["genlock_connect_on_show"] is True
    assert "genlock_connect_on_show" not in obs.inputs["NDI 2ME PVW"]["settings"]
    # the twins: always connected, low bandwidth, bound to the main's LIVE sender + pin
    tw = obs.inputs["MV NDI cam3"]["settings"]
    assert tw["genlock_monitor"] is True and tw["genlock_connect_on_show"] is False
    assert tw["ndi_source_name"] == "CAM3 (usb)" and tw["genlock_latency_ms_src"] == 6
    # twin scenes for every multiview scene that holds a program input; the swapped item is pinned
    assert [i["sourceName"] for i in obs.scenes["MV Cam 3"]] == ["MV NDI cam3"]
    assert obs.scenes["MV Cam 3"][0]["sceneItemTransform"]["boundsType"] == "OBS_BOUNDS_SCALE_INNER"
    assert [i["sourceName"] for i in obs.scenes["MV Moderatori"]] == ["MV NDI cam3", "Image"]
    # built-in multiview membership: originals out, twins in, each twin STANDS FOR its original
    for orig in ("Cam 1", "Cam 3", "Moderatori"):
        assert _mv(obs, orig) is False and _mv(obs, "MV " + orig) is True
        assert _target(obs, "MV " + orig) == orig
    # NDI-output scenes stay shown as-is and are never twinned
    assert _mv(obs, "Grading") is True and _mv(obs, "Interkom") is True
    assert "MV Grading" not in obs.scenes and "MV Interkom" not in obs.scenes
    # the custom MULTIVIEW grid renders twins, never the full inputs
    assert [i["sourceName"] for i in obs.scenes["MULTIVIEW"]] == ["MV NDI cam1", "MV NDI cam3"]
    # the built-in multiview was refreshed (a scene-list change) and the scratch scene is gone
    assert not any(n.startswith("__") for n in obs.scenes)
    assert any(c[0] == "RemoveScene" for c in obs.calls)


def test_apply_bandwidth_roles_is_idempotent():
    obs = FakeObs()
    roles.apply_bandwidth_roles(obs, PLAN)
    obs.calls.clear()
    roles.apply_bandwidth_roles(obs, PLAN)
    assert _writes(obs) == [], f"a second apply over a correct collection must be read-only: {_writes(obs)}"


def test_operator_multiview_choice_is_never_reimposed():
    obs = FakeObs()
    roles.apply_bandwidth_roles(obs, PLAN)
    # the operator puts `Cam 3` back into the multiview by hand
    obs.private["Cam 3"]["show_in_multiview"] = True
    obs.calls.clear()
    roles.apply_bandwidth_roles(obs, PLAN)
    assert _mv(obs, "Cam 3") is True, "an adopted twin's membership is never re-imposed (operator wins)"
    assert _writes(obs) == []


def test_twin_drift_is_healed():
    obs = FakeObs()
    roles.apply_bandwidth_roles(obs, PLAN)
    obs.scenes["MV Cam 3"][0]["sceneItemTransform"]["positionX"] = 500.0
    roles.apply_bandwidth_roles(obs, PLAN)
    assert obs.scenes["MV Cam 3"][0]["sceneItemTransform"]["positionX"] == 0.0


def test_a_twin_whose_original_lost_its_camera_is_retired():
    obs = FakeObs()
    roles.apply_bandwidth_roles(obs, PLAN)
    obs.scenes["Moderatori"] = [_item(1, "Image", kind="image_source")]  # the operator removed cam3
    roles.apply_bandwidth_roles(obs, PLAN)
    assert _mv(obs, "Moderatori") is True and _mv(obs, "MV Moderatori") is False
    assert "MV Moderatori" in obs.scenes, "a retired twin scene is left in place, never deleted"
    assert not _target(obs, "MV Moderatori"), "a retired twin releases its adoption"
    # the camera comes back -> the twin takes the cell over again
    obs.scenes["Moderatori"] = [_item(1, "NDI cam3"), _item(2, "Image", kind="image_source")]
    roles.apply_bandwidth_roles(obs, PLAN)
    assert _mv(obs, "Moderatori") is False and _mv(obs, "MV Moderatori") is True
    assert _target(obs, "MV Moderatori") == "Moderatori"


def test_a_migrated_windows_twin_layout_keeps_its_camera_cells():
    # the #501/#761 Windows collection: `MV Cam 3` already shown, `Cam 3` hidden, the twin scene
    # holding the MAIN input (the same-source pivot) -- adoption must keep the cell, never hide both
    obs = FakeObs()
    obs.scenes["MV Cam 3"] = [_item(50, "NDI cam3")]
    obs.private["MV Cam 3"] = {"show_in_multiview": True}
    obs.private["Cam 3"] = {"show_in_multiview": False}
    roles.apply_bandwidth_roles(obs, PLAN)
    assert _mv(obs, "MV Cam 3") is True and _mv(obs, "Cam 3") is False
    assert _target(obs, "MV Cam 3") == "Cam 3"
    assert [i["sourceName"] for i in obs.scenes["MV Cam 3"]] == ["MV NDI cam3"]


def test_a_scene_that_nests_a_camera_scene_gets_a_twin_nesting_the_twin():
    obs = FakeObs()
    obs.scenes["Two cams"] = [_item(1, "Cam 1", kind=None, stype=SCENE),
                              _item(2, "Cam 3", kind=None, stype=SCENE)]
    roles.apply_bandwidth_roles(obs, PLAN)
    assert [i["sourceName"] for i in obs.scenes["MV Two cams"]] == ["MV Cam 1", "MV Cam 3"]
    assert _mv(obs, "Two cams") is False and _target(obs, "MV Two cams") == "Two cams"


def test_a_main_without_a_sender_gets_no_twin():
    obs = FakeObs()
    obs.inputs["NDI cam3"]["settings"]["ndi_source_name"] = ""
    summary = roles.apply_bandwidth_roles(obs, PLAN)
    assert "MV NDI cam3" not in obs.inputs, "an empty sender name would stop the twin's receiver"
    assert any("NDI cam3" in p for p in summary["problems"])
    assert "MV NDI cam1" in obs.inputs


def test_a_colliding_non_ndi_twin_name_is_left_alone():
    obs = FakeObs()
    obs.inputs["MV NDI cam1"] = {"kind": "image_source", "settings": {}}
    summary = roles.apply_bandwidth_roles(obs, PLAN)
    assert obs.inputs["MV NDI cam1"]["kind"] == "image_source"
    assert any("MV NDI cam1" in p for p in summary["problems"])


# ------------------------------------------------------------------------------------------------
# strih_scenes.py delegation + the launch path
# ------------------------------------------------------------------------------------------------

def test_strih_obs_start_applies_the_roles_after_the_seed_best_effort():
    s = (SCRIPTS / "strih-obs-start.sh").read_text()
    boot = s.find('python3 "$SCN" --bootstrap')
    rls = s.find('python3 "$SCN" --apply-roles')
    wait = s.find('wait "$OBS_PID"')
    assert boot != -1 and rls != -1 and boot < rls < wait
    line = [ln for ln in s.splitlines() if 'python3 "$SCN" --apply-roles' in ln][0]
    assert line.lstrip().startswith("if "), "a role-apply failure must never abort the unit (OBS is live)"


def test_apply_roles_cli_delegates_to_the_roles_module(monkeypatch, tmp_path):
    man = tmp_path / "seed.json"
    man.write_text(json.dumps({"mode": "update-only", "inputs": [
        {"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"}], "camera_latency_ms": 3}))
    seen = {}

    class Stub:
        @staticmethod
        def apply_bandwidth_roles(obs, plan):
            seen["plan"] = plan
            return {}

    monkeypatch.setattr(ss, "Obs", lambda *a, **k: FakeObs())
    monkeypatch.setattr(ss, "_roles_module", lambda: Stub)
    monkeypatch.setattr(sys, "argv", ["strih_scenes.py", "--apply-roles", "--manifest", str(man)])
    ss.main()
    assert [p["input"] for p in seen["plan"]] == ["NDI cam1"]


def test_setup_strih_installs_the_roles_module_next_to_the_seeder():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    assert 'install -m 0755 "${HERE}/strih_bandwidth_roles.py" /usr/local/bin/strih_bandwidth_roles.py' in s


def test_strih_mv_scenes_shares_the_one_settable_transform_list():
    src = (SCRIPTS / "strih_mv_scenes.py").read_text()
    assert "from strih_bandwidth_roles import SETTABLE_TRANSFORM_FIELDS" in src


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
    sf = tmp_path / "sub" / "hold.json"  # the state dir is created on demand
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


def test_connect_on_show_restore_treats_a_vanished_input_as_done(tmp_path, monkeypatch):
    state = {"calls": [], "showing": {}, "inputs": {"NDI cam1": {"genlock_connect_on_show": False}}}
    monkeypatch.setattr(op, "_rpc", _fake_rpc(state))
    sf = tmp_path / "hold.json"
    sf.write_text(json.dumps(["NDI cam1", "NDI cam9"]))  # cam9 was deleted/renamed since the hold
    restored, failed = op.connect_on_show_restore(FakeWs(), str(sf))
    assert restored == ["NDI cam1"] and failed == []
    assert not sf.exists(), "a vanished input must never keep the state file (and its WARN) alive"


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
# recording-e2e.sh wiring + the hold lib
# ------------------------------------------------------------------------------------------------

E2E = (SCRIPTS / "recording-e2e.sh").read_text()
LIB = SCRIPTS / "lib" / "connect-on-show-hold.sh"
PARK_LIB = SCRIPTS / "lib" / "genlock-park.sh"


def test_e2e_holds_connect_on_show_after_the_trap_behind_a_rig_busy_guard():
    trap = E2E.index("\ntrap cleanup EXIT HUP INT TERM\n")
    hold = E2E.index('connect_on_show_e2e_hold "$HERE"')
    guard = E2E.rindex('stray_session_check_assert "$HERE"', 0, hold)
    wait = E2E.index('connect_on_show_e2e_wait_live "$HERE"')
    first_deploy = E2E.index('echo "[2/8] $CAMERA_NAME')
    assert trap < guard < hold < wait < first_deploy
    assert '. "$HERE/lib/connect-on-show-hold.sh"' in E2E and '. "$HERE/lib/genlock-park.sh"' in E2E
    line = [ln for ln in E2E.splitlines() if 'connect_on_show_e2e_hold "$HERE"' in ln][0]
    assert "|| exit 1" in line, "a failed hold must abort the run (the measurement would be wrong)"


def test_e2e_hold_state_path_is_stable_across_runs():
    assert ('CONNECT_ON_SHOW_HOLD_STATE="${CONNECT_ON_SHOW_HOLD_STATE:-$HOME/.camera-box/'
            'connect-on-show-hold.json}"') in E2E
    assert 'CONNECT_ON_SHOW_HOLD_STATE="$OUTDIR' not in E2E


def test_e2e_cleanup_restores_connect_on_show():
    body = E2E[E2E.index("\ncleanup() {\n"):E2E.index("\ntrap cleanup EXIT HUP INT TERM\n")]
    assert 'connect_on_show_e2e_restore "$HERE"' in body


def _bash_lib(body, env=None, extra=""):
    # bounded: a wait loop that never terminates must FAIL the test, never hang the suite
    return subprocess.run(["bash", "-c", f"set -euo pipefail; . '{PARK_LIB}'; . '{LIB}'; {extra}{body}"],
                          capture_output=True, text=True, check=False, timeout=60,
                          env=env or {"PATH": "/usr/bin:/bin", "HOME": "/tmp"})


def test_hold_lib_fails_loud_and_restore_never_aborts(tmp_path):
    fake = tmp_path / "obs_phase2.py"
    fake.write_text("import sys\nprint('ARGS', sys.argv[1:])\nsys.exit(int(__import__('os').environ.get('RC','0')))\n")
    ok = _bash_lib(f"connect_on_show_e2e_hold '{tmp_path}' 10.0.0.1 /tmp/s.json")
    assert ok.returncode == 0 and "--hold" in ok.stdout
    bad = _bash_lib(f"connect_on_show_e2e_hold '{tmp_path}' 10.0.0.1 /tmp/s.json",
                    env={"RC": "1", "PATH": "/usr/bin:/bin", "HOME": "/tmp"})
    assert bad.returncode != 0
    rest = _bash_lib(f"connect_on_show_e2e_restore '{tmp_path}' 10.0.0.1 /tmp/s.json; echo DONE",
                     env={"RC": "1", "PATH": "/usr/bin:/bin", "HOME": "/tmp"})
    assert rest.returncode == 0 and "DONE" in rest.stdout, "restore must never abort cleanup()"


def _log_reader(tmp_path, script_body):
    r = tmp_path / "reader.sh"
    r.write_text("#!/usr/bin/env bash\n" + textwrap.dedent(script_body))
    r.chmod(0o755)
    return r


def test_wait_live_returns_once_every_held_input_delivers(tmp_path):
    state = tmp_path / "hold.json"
    state.write_text(json.dumps(["NDI cam1", "NDI cam3"]))
    cnt = tmp_path / "n"
    # read 1: cam3 still parked (stuck); read 2+: both unparked + advancing
    reader = _log_reader(tmp_path, f"""\
        n=$(cat '{cnt}' 2>/dev/null || echo 0); n=$((n+1)); printf '%s' "$n" > '{cnt}'
        printf "12:00:00.000: genlock-fifo audit 'NDI cam1': received=%s consumed=1\\n" "$((100 + n * 60))"
        if [ "$n" -le 1 ]; then
          printf "12:00:00.100: genlock-park 'NDI cam3': state=parked parked_s=9 (x)\\n"
          printf "12:00:00.200: genlock-fifo audit 'NDI cam3': received=50 consumed=1\\n"
        else
          printf "12:00:00.100: genlock-park 'NDI cam3': state=unparked parked_s=9 (x)\\n"
          printf "12:00:00.200: genlock-fifo audit 'NDI cam3': received=%s consumed=1\\n" "$((50 + n * 60))"
        fi
    """)
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "CONNECT_ON_SHOW_LOG_READ_CMD": str(reader),
           "CONNECT_ON_SHOW_LIVE_POLL_S": "0", "CONNECT_ON_SHOW_LIVE_WAIT_S": "10"}
    out = _bash_lib(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert out.returncode == 0 and "RC=0" in out.stdout, out.stderr
    assert "every held input is delivering again" in out.stdout
    assert "WARNING" not in out.stderr


def test_wait_live_is_bounded_and_fail_open(tmp_path):
    state = tmp_path / "hold.json"
    state.write_text(json.dumps(["NDI cam3"]))
    reader = _log_reader(tmp_path, """\
        printf "12:00:00.100: genlock-park 'NDI cam3': state=parked parked_s=9 (x)\\n"
    """)
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "CONNECT_ON_SHOW_LOG_READ_CMD": str(reader),
           "CONNECT_ON_SHOW_LIVE_POLL_S": "1", "CONNECT_ON_SHOW_LIVE_WAIT_S": "2"}
    out = _bash_lib(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert out.returncode == 0 and "RC=0" in out.stdout
    assert "not yet delivering after 2s: NDI cam3" in out.stderr
    # the budget is WALL time: a zero poll interval must still terminate
    env.update({"CONNECT_ON_SHOW_LIVE_POLL_S": "0", "CONNECT_ON_SHOW_LIVE_WAIT_S": "1"})
    out = _bash_lib(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert "RC=0" in out.stdout and "not yet delivering" in out.stderr


def test_wait_live_accepts_a_present_counter_when_the_first_read_had_none(tmp_path):
    state = tmp_path / "hold.json"
    state.write_text(json.dumps(["NDI cam3"]))
    cnt = tmp_path / "n"
    # read 1: no audit line at all for cam3 (out of the tail); read 2: unparked + a counter
    reader = _log_reader(tmp_path, f"""\
        n=$(cat '{cnt}' 2>/dev/null || echo 0); n=$((n+1)); printf '%s' "$n" > '{cnt}'
        if [ "$n" -ge 2 ]; then
          printf "12:00:00.200: genlock-fifo audit 'NDI cam3': received=77 consumed=1\\n"
        fi
    """)
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "CONNECT_ON_SHOW_LOG_READ_CMD": str(reader),
           "CONNECT_ON_SHOW_LIVE_POLL_S": "0", "CONNECT_ON_SHOW_LIVE_WAIT_S": "10"}
    out = _bash_lib(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert "every held input is delivering again" in out.stdout, out.stdout + out.stderr


def test_hold_and_restore_set_and_clear_the_strih_side_marker(tmp_path):
    fake = tmp_path / "obs_phase2.py"
    fake.write_text("import sys\nprint('ARGS', sys.argv[1:])\n")
    log = tmp_path / "marker.log"
    marker_cmd = _log_reader(tmp_path, f"""\
        echo "$@" >> '{log}'
    """)
    state = tmp_path / "hold.json"
    state.write_text("[]")
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "CONNECT_ON_SHOW_MARKER_CMD": str(marker_cmd)}
    out = _bash_lib(f"connect_on_show_e2e_hold '{tmp_path}' 10.0.0.1 '{state}'; "
                    f"connect_on_show_e2e_restore '{tmp_path}' 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert "RC=0" in out.stdout, out.stderr
    assert log.read_text().split("\n")[:2] == ["set 10.0.0.1", "clear 10.0.0.1"]


def test_wait_live_without_a_state_file_is_a_no_op(tmp_path):
    out = _bash_lib(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{tmp_path}/absent.json'; echo RC=$?")
    assert out.returncode == 0 and "RC=0" in out.stdout
