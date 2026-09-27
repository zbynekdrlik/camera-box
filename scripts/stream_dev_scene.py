#!/usr/bin/env python3
"""issue 1380 -- the stream OBS DEVELOPMENT scene: the seeder decision + apply. Pure given an
injected obs-websocket `rpc(ws, request_type, data=None, ignore_err=False)`
(scripts/obs_phase2.py passes its own `_rpc` and owns the connection + the CLI).

Owner request 27.9.2026: development never programs the owner's production scene `PRO` on the
stream OBS. It programs its own `Development` scene, whose one item is the scene `PRO` (the same
pixels, the same warm `NDI 2ME PGM` receiver, so the recording and the 911004 burn are unchanged),
and the Companion PRE/PRODUCTION/POST machine (keyed on program == "PRO") is not armed by
development. Owner hard rule 27.9.2026, verbatim:
    "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!"
No tool ever programs `PRO` (obs_phase2.py refuses it); EVENT mode leaves the stream program to the
owner, who cuts to it himself.

The seeder is idempotent and operator-wins: it only ever CREATES the development scene and its
nested production-scene item when they are missing, never edits an existing item or its transform,
and never reads or writes the production scene's own items.

The two scene names are declared in scripts/lib/stream-dev-scene.sh; the constants below are pinned
to those by tests/python/test_stream_dev_scene_1380.py.
"""
import collections

STREAM_DEV_SCENE = "Development"
STREAM_PRODUCTION_SCENE = "PRO"


class DevSceneError(Exception):
    """The development scene cannot be ensured (the production scene is missing, or the names are
    unusable). Fail loud; never guess another scene."""


DevScenePlan = collections.namedtuple("DevScenePlan", "actions nested_hidden")


def dev_scene_plan(scene_names, dev_items, dev_scene, nested_scene):
    """What the seeder must do. *scene_names* = GetSceneList names; *dev_items* = the development
    scene's GetSceneItemList `sceneItems` (None when the scene does not exist). Returns
    DevScenePlan(actions, nested_hidden) with actions a subset of ["create_scene", "add_nested"] in
    that order. `nested_hidden` reports an existing nested item the operator disabled: it is left
    alone (operator-wins) but the caller says why the development program would render black.
    Raises DevSceneError when the production scene is missing or the two names are empty or equal
    (nesting a scene in itself)."""
    if not dev_scene or not nested_scene:
        raise DevSceneError("the development and production scene names must both be non-empty")
    if dev_scene == nested_scene:
        raise DevSceneError(
            f"the development scene must differ from the production scene '{nested_scene}' "
            f"(development never programs the production scene itself)"
        )
    if nested_scene not in scene_names:
        raise DevSceneError(
            f"the production scene '{nested_scene}' does not exist on this OBS (scenes: "
            f"{sorted(scene_names)}); refusing to build '{dev_scene}' around a missing scene"
        )
    if dev_scene not in scene_names:
        return DevScenePlan(["create_scene", "add_nested"], False)
    nested = [it for it in (dev_items or []) if it.get("sourceName") == nested_scene]
    if not nested:
        return DevScenePlan(["add_nested"], False)
    hidden = not any(bool(it.get("sceneItemEnabled", True)) for it in nested)
    return DevScenePlan([], hidden)


def ensure_dev_scene(rpc, ws, dev_scene, nested_scene):
    """Apply dev_scene_plan over an open obs-websocket. Reads the scene list and ONLY the
    development scene's items; writes only CreateScene / CreateSceneItem on the development scene,
    never with ignore_err (a failed write fails loud). Returns the DevScenePlan it applied."""
    scenes = [s.get("sceneName") for s in rpc(ws, "GetSceneList").get("scenes", [])]
    dev_items = None
    if dev_scene in scenes:
        dev_items = rpc(ws, "GetSceneItemList", {"sceneName": dev_scene}).get("sceneItems", [])
    plan = dev_scene_plan(scenes, dev_items, dev_scene, nested_scene)
    for action in plan.actions:
        if action == "create_scene":
            rpc(ws, "CreateScene", {"sceneName": dev_scene})
        elif action == "add_nested":
            rpc(ws, "CreateSceneItem", {
                "sceneName": dev_scene, "sourceName": nested_scene, "sceneItemEnabled": True})
    return plan
