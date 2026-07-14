#!/usr/bin/env python3
"""strih OBS per-camera low-bandwidth multiview twin scenes (#730).

**Repeatedly requested by the user**: strih never got the optimized per-camera multiview scenes
imag-nb already has ("MV Cam N" — dedicated low-cost thumbnail scenes feeding the multiview,
instead of the full program-grade sources rendering in the multiview grid, #501). This script
replicates that pattern on strih over OBS WebSocket.

strih already has 7 full-bandwidth camera scenes "Cam 1".."Cam 7" (#753, 2026-07-14: cam7 is a
NEW, direct/non-inverted pin — its scene/input share the same "7"), each wrapping ONE NDI input
"NDI cam<n>" bound to a real fleet NDI source (the genlock skill's documented INVERTED-label
mapping for cam1..cam6 — e.g. live-verified 2026-07-13: "NDI cam1" carries "CAM3 (usb)", not
CAM1). This script
NEVER hardcodes that mapping: it reads each existing input's LIVE `ndi_source_name` over WS and
wraps that EXACT same value in a new "MV Cam <n>" twin input, `genlock_monitor=true` (the #501
pattern — the vendored DistroAV genlock lockdown forces LOW-bandwidth NDI receive, ~9x cheaper,
for a source flagged this way). The real "Cam N" inputs/scenes are never modified.

Two independent render-cost sinks get wired to the cheap twins (both cost full-bandwidth decode
for every tile today, live-verified via GetStats before/after — see the genlock skill #730 note):

  1. The BUILT-IN OBS Multiview PROJECTOR (the one the #276/#278/#293 render-budget decouple
     hardened) — via the per-scene `show_in_multiview` private setting (mirrors OBSBasic_Scenes
     .cpp's "ShowInMultiview" context-menu action): real "Cam N" -> hidden, "MV Cam N" -> shown.
  2. strih's own hand-built "Multiview" SCENE (a plain scene whose scene items are references to
     other scenes — NOT the built-in projector) — its items that reference a real "Cam N" scene
     are swapped for the matching "MV Cam N" twin, preserving the EXACT scene-item transform
     (position/scale/bounds) so the operator's layout never visibly changes.

Respects decouple-dont-rebuild (#508): this only re-points OBS's OWN existing scene/source
mechanisms at cheaper feeds — no custom renderer, no new multiview mechanism.

Usage:
  strih_mv_scenes.py --host 10.77.9.202 --password PW               # seed the twins + rewire
  strih_mv_scenes.py --host 10.77.9.202 --password PW --stats 15    # ad-hoc GetStats before/after
                                                                      # render-cost delta (no seed)
"""

import argparse
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2 as op  # reuse the repo's ONE obs-websocket client (_conn/_rpc) — never a 4th one

CAMS = range(1, 8)  # #753 (2026-07-14): fleet growth 6->7, cam7 is real + provisioned now
_CAM_SCENE_RE = re.compile(r"^Cam (\d+)$")

# obs-websocket v5 SceneItemTransform: only these fields are SETTABLE via SetSceneItemTransform.
# GetSceneItemList also returns read-only computed fields (width/height/sourceWidth/sourceHeight)
# that must be stripped before echoing a transform back, or the request can be rejected/ignored.
_SETTABLE_TRANSFORM_FIELDS = frozenset({
    "positionX", "positionY", "rotation", "scaleX", "scaleY", "alignment",
    "boundsType", "boundsAlignment", "boundsWidth", "boundsHeight",
    "cropLeft", "cropTop", "cropRight", "cropBottom",
})


# --- PURE functions (no network — unit-tested from tests/python/test_strih_mv_scenes.py) --------

def is_cam_scene(name: str) -> bool:
    """True for strih's real per-camera scene names ('Cam 1'..'Cam N'), false for anything else
    (utility/overlay scenes like 'Control', 'Multiview' itself, 'PHASE2-PROBE', ...)."""
    return bool(_CAM_SCENE_RE.match(name))


def mv_scene_name(cam_scene_name: str) -> str:
    """'Cam 5' -> 'MV Cam 5'. Raises on a non-'Cam N' input — callers must filter with
    is_cam_scene() first; silently mangling an unrelated scene name is worse than failing loud."""
    if not is_cam_scene(cam_scene_name):
        raise ValueError(f"not a 'Cam N' scene name: {cam_scene_name!r}")
    return f"MV {cam_scene_name}"


def cam_input_name(cam_scene_name: str) -> str:
    """'Cam 5' -> 'NDI cam5' — strih's existing per-camera input-naming convention."""
    m = _CAM_SCENE_RE.match(cam_scene_name)
    if not m:
        raise ValueError(f"not a 'Cam N' scene name: {cam_scene_name!r}")
    return f"NDI cam{m.group(1)}"


def settable_transform_fields(transform: dict) -> dict:
    """Strip a GetSceneItemList-returned sceneItemTransform down to the fields
    SetSceneItemTransform actually accepts (drops read-only computed fields like width/height)."""
    return {k: v for k, v in (transform or {}).items() if k in _SETTABLE_TRANSFORM_FIELDS}


def mv_replacement_plan(scene_items: list) -> list:
    """Given the custom 'Multiview' scene's GetSceneItemList sceneItems, return the plan of which
    items to swap: every item that is itself a real 'Cam N' scene reference gets paired with its
    low-bandwidth twin name + a settable-only copy of its current transform + its sceneItemId (so
    the caller can add the new item at the same spot, then remove the old one). Any item that is
    NOT a 'Cam N' scene (an overlay, a utility source, anything the operator added by hand) is left
    OUT of the plan untouched — this script only ever touches the camera tiles it owns."""
    plan = []
    for item in scene_items:
        name = item.get("sourceName", "")
        if not is_cam_scene(name):
            continue
        plan.append({
            "old_name": name,
            "new_name": mv_scene_name(name),
            "old_item_id": item["sceneItemId"],
            "transform": settable_transform_fields(item.get("sceneItemTransform", {})),
        })
    return plan


def stats_delta(before: dict, after: dict) -> dict:
    """Pure GetStats before/after -> a render-cost delta report. renderSkippedFrames/
    renderTotalFrames are the RENDER health signal (obs-render-health-metric.md) — the encoder-side
    outputSkippedFrames stays green even when the render loop chokes, so both are reported."""
    d_render_skip = after["renderSkippedFrames"] - before["renderSkippedFrames"]
    d_render_total = after["renderTotalFrames"] - before["renderTotalFrames"]
    d_out_skip = after["outputSkippedFrames"] - before["outputSkippedFrames"]
    d_out_total = after["outputTotalFrames"] - before["outputTotalFrames"]
    return {
        "activeFps": after.get("activeFps"),
        "averageFrameRenderTime": after.get("averageFrameRenderTime"),
        "renderSkipped_delta": d_render_skip,
        "renderTotal_delta": d_render_total,
        "renderSkip_pct": round(100.0 * d_render_skip / d_render_total, 2) if d_render_total else 0.0,
        "outputSkipped_delta": d_out_skip,
        "outputTotal_delta": d_out_total,
    }


# --- live (WS) functions --------------------------------------------------------------------

def seed(obs) -> tuple:
    """Create/refresh the 'MV Cam N' twin input+scene for every real 'Cam N' scene strih actually
    has, wrapping each twin around the SAME live ndi_source_name the real input already uses.
    Idempotent — CreateScene/CreateInput 'already exists' errors are ignored, settings are
    re-applied every run (self-healing, same philosophy as the genlock lockdown)."""
    video = op._rpc(obs, "GetVideoSettings")
    cw, ch = video["baseWidth"], video["baseHeight"]
    scenes = [s["sceneName"] for s in op._rpc(obs, "GetSceneList")["scenes"]]

    created, skipped = [], []
    for n in CAMS:
        cam_scene = f"Cam {n}"
        if cam_scene not in scenes:
            skipped.append(cam_scene)
            continue
        cam_input = cam_input_name(cam_scene)
        settings = op._rpc(obs, "GetInputSettings", {"inputName": cam_input}, ignore_err=True)
        ndi_name = (settings or {}).get("inputSettings", {}).get("ndi_source_name")
        if not ndi_name:
            skipped.append(cam_scene)
            continue

        mv_scene = mv_scene_name(cam_scene)
        mv_input = f"MV {cam_input}"
        op._rpc(obs, "CreateScene", {"sceneName": mv_scene}, ignore_err=True)
        twin_settings = {"ndi_source_name": ndi_name, "latency": 1, "genlock_monitor": True}
        op._rpc(obs, "CreateInput", {
            "sceneName": mv_scene, "inputName": mv_input, "inputKind": "ndi_source",
            "inputSettings": twin_settings,
        }, ignore_err=True)
        # re-apply every run (self-healing) — mirrors imag_scenes.py's own convention.
        op._rpc(obs, "SetInputSettings",
                {"inputName": mv_input, "inputSettings": twin_settings}, ignore_err=True)
        op._rpc(obs, "SetInputMute", {"inputName": mv_input, "inputMuted": True}, ignore_err=True)
        item = op._rpc(obs, "GetSceneItemId",
                        {"sceneName": mv_scene, "sourceName": mv_input}, ignore_err=True)
        if item.get("sceneItemId") is not None:
            op._rpc(obs, "SetSceneItemTransform", {
                "sceneName": mv_scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
                    "boundsWidth": cw, "boundsHeight": ch, "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

        # #501 built-in-multiview membership: hide the full-bw real scene, show the cheap twin.
        op._rpc(obs, "SetSourcePrivateSettings", {
            "sourceName": cam_scene, "sourceSettings": {"show_in_multiview": False},
        }, ignore_err=True)
        op._rpc(obs, "SetSourcePrivateSettings", {
            "sourceName": mv_scene, "sourceSettings": {"show_in_multiview": True},
        }, ignore_err=True)
        created.append(mv_scene)

    return created, skipped


def rewire_multiview_scene(obs, multiview_scene: str = "Multiview") -> list:
    """Swap the custom 'Multiview' scene's 'Cam N' item references for their 'MV Cam N' twins,
    preserving each item's exact transform. Adds the new item BEFORE removing the old one, so the
    live production output never drops to fewer tiles than before mid-operation (hot-apply, no OBS
    restart — strih is production). No-op if the box has no such scene."""
    scenes = [s["sceneName"] for s in op._rpc(obs, "GetSceneList")["scenes"]]
    if multiview_scene not in scenes:
        return []
    items = op._rpc(obs, "GetSceneItemList", {"sceneName": multiview_scene})["sceneItems"]
    plan = mv_replacement_plan(items)

    rewired = []
    for entry in plan:
        op._rpc(obs, "CreateSceneItem", {
            "sceneName": multiview_scene, "sourceName": entry["new_name"],
            "sceneItemEnabled": True,
        }, ignore_err=True)
        new_item = op._rpc(obs, "GetSceneItemId", {
            "sceneName": multiview_scene, "sourceName": entry["new_name"],
        }, ignore_err=True)
        if new_item.get("sceneItemId") is not None and entry["transform"]:
            op._rpc(obs, "SetSceneItemTransform", {
                "sceneName": multiview_scene, "sceneItemId": new_item["sceneItemId"],
                "sceneItemTransform": entry["transform"],
            }, ignore_err=True)
        op._rpc(obs, "RemoveSceneItem", {
            "sceneName": multiview_scene, "sceneItemId": entry["old_item_id"],
        }, ignore_err=True)
        rewired.append(entry["new_name"])
    return rewired


def measure_stats(obs, seconds: float) -> dict:
    """Live wrapper around stats_delta(): sample GetStats now, wait `seconds`, sample again."""
    before = op._rpc(obs, "GetStats")
    time.sleep(seconds)
    after = op._rpc(obs, "GetStats")
    return stats_delta(before, after)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--multiview-scene", default="Multiview",
                     help="name of strih's custom multiview-grid scene to rewire (default: Multiview)")
    ap.add_argument("--stats", type=float, default=None, metavar="SECONDS",
                     help="print a GetStats render-cost delta over SECONDS and exit — no seeding")
    args = ap.parse_args()

    obs = op._conn(args.host, args.password)
    try:
        if args.stats is not None:
            d = measure_stats(obs, args.stats)
            print(f"render-cost over {args.stats:.0f}s: activeFps={d['activeFps']} "
                  f"avgRenderMs={d['averageFrameRenderTime']:.2f} "
                  f"renderSkipped={d['renderSkipped_delta']}/{d['renderTotal_delta']} "
                  f"({d['renderSkip_pct']}%) "
                  f"outputSkipped={d['outputSkipped_delta']}/{d['outputTotal_delta']}")
            return

        created, skipped = seed(obs)
        print(f"MV twin scenes: {len(created)}/{len(list(CAMS))} created/refreshed "
              f"({created})" + (f" — SKIPPED (no live 'Cam N'/NDI source): {skipped}" if skipped else ""))
        rewired = rewire_multiview_scene(obs, args.multiview_scene)
        if rewired:
            print(f"'{args.multiview_scene}' scene: rewired {len(rewired)} tile(s) -> {rewired}")
        else:
            print(f"'{args.multiview_scene}' scene: nothing to rewire (scene absent, or no "
                  f"'Cam N' items in it)")
    finally:
        obs.close()


if __name__ == "__main__":
    main()
