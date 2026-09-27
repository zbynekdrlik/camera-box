#!/usr/bin/env python3
"""issue 1380 -- the stream OBS DEVELOPMENT scene: the seeder decision + the Studio Mode preview
re-assert. Pure given an injected obs-websocket `rpc(ws, request_type, data=None, ignore_err=False)`
(scripts/obs_phase2.py passes its own `_rpc` and owns the connection + the CLI).

Owner request 27.9.2026: development never programs the owner's production scene `PRO` on the
stream OBS. It programs its own `Development` scene, whose one item is the scene `PRO` (the same
pixels, the same warm `NDI 2ME PGM` receiver, so the recording and the 911004 burn are unchanged),
and the Companion PRE/PRODUCTION/POST machine (keyed on program == "PRO") is not armed by
development. EVENT mode puts `PRO` back on program.

The seeder is idempotent and operator-wins: it only ever CREATES the development scene and its
nested production-scene item when they are missing, never edits an existing item or its transform,
and never reads or writes the production scene's own items.

The two scene names are declared in scripts/lib/stream-dev-scene.sh; the constants below are pinned
to those by tests/python/test_stream_dev_scene_1380.py.
"""
import collections
import time

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


def stale_preview_target(studio, preview, stale_scene, target):
    """The scene the Studio Mode PREVIEW must be set to, or None. Only when Studio Mode is on, a
    stale scene was named, it differs from the target, and the current preview IS that stale scene.
    An operator's own preview is left alone."""
    if not studio or not stale_scene or stale_scene == target or preview != stale_scene:
        return None
    return target


def _transition_cursor(rpc, ws):
    """The current scene transition's cursor (0.0..1.0), or None when OBS cannot report it."""
    value = rpc(ws, "GetCurrentSceneTransitionCursor", ignore_err=True).get("transitionCursor")
    return None if value is None else float(value)


def reassert_stale_preview(rpc, ws, stale_scene, target, margin_s, poll_s, start_timeout_s=1.0,
                           cap_s=30.0, sleep=time.sleep, now=time.monotonic):
    """Keep the Studio Mode PREVIEW off *stale_scene* until the program transition has ENDED plus
    *margin_s*.

    In Studio Mode with swap mode on (the OBS default, `SwapScenesMode`), SetCurrentProgramScene is
    a transition, and when it ENDS OBS puts the OLD program into the preview
    (OBSBasic_Transitions.cpp TransitionStopped). After development the program is `Development`,
    so EVENT's cut to `PRO` leaves `Development` in the preview once the transition finishes; a
    Transition click would then put it back on air and skip the Companion `PRODUCTION` trigger.

    The end is OBSERVED, never taken from the configured duration: a stinger is a FIXED transition
    (GetCurrentSceneTransition reports no duration for it) and a per-scene override duration is not
    reported at all. So poll GetCurrentSceneTransitionCursor: a value below 1.0 means the transition
    is running; 1.0 means ended -- but only once this transition was seen running or *start_timeout_s*
    passed (right after the request the cursor can still read the PREVIOUS transition's 1.0, and a
    cut is 1.0 at once). No cursor available -> the margin alone. Every poll moves the preview to
    *target* when it shows the stale scene (an operator's own preview is left alone). Bounded by
    *cap_s*, never a busy loop; a failed preview set fails loud. Returns how many times it moved it."""
    studio = bool(rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
    if not studio or not stale_scene or stale_scene == target:
        return 0
    moved = 0
    t0 = now()
    started = False
    end_at = None
    while True:
        preview = rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get(
            "currentPreviewSceneName")
        new_preview = stale_preview_target(True, preview, stale_scene, target)
        if new_preview:
            rpc(ws, "SetCurrentPreviewScene", {"sceneName": new_preview})
            moved += 1
        t = now()
        if end_at is None:
            cursor = _transition_cursor(rpc, ws)
            if cursor is None:
                end_at = t + max(0.0, margin_s)
            elif cursor < 1.0:
                started = True
            elif started or t - t0 >= start_timeout_s:
                end_at = t + max(0.0, margin_s)
        if (end_at is not None and t >= end_at) or t - t0 >= cap_s:
            return moved
        sleep(poll_s)
