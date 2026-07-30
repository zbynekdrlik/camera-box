#!/usr/bin/env python3
"""#747 — pre-recording camera-scene warm-up.

Cycles EVERY strih "Cam N" scene (the per-camera scene convention — see
scripts/strih_mv_scenes.py) onto PREVIEW briefly, right before [5/8] StartRecord, so
DistroAV's raw NDI receivers for cameras not otherwise shown are already connected when
[6/8]'s ALL_CAMBOX sweep makes its very first program cut to each camera — avoiding a cold
receiver connect eating into that segment's first few frames.

This is the recording-e2e.sh companion to frozen-camera-gate.py's own per-source warm-up
(#747, [4c/8]): post-#730/#508 Multiview decoupling, a raw NDI main input ("NDI camN") that
is not currently SHOWING on any surface does not render at all until something puts it on
program/preview — the decoupled built-in Multiview now shows low-bandwidth "MV Cam N" twin
clones (#730), not these raw main inputs, so it no longer keeps them warm either way.

Usage:
  python3 scripts/warm_cam_scenes.py --host 10.77.9.202 [--settle 1.5]
"""
import argparse
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2 as op  # reuse the repo's ONE obs-websocket client (_conn/_rpc) — never a 4th one

_CAM_SCENE_RE = re.compile(r"^Cam (\d+)$")


# ─── pure helpers (unit-testable without OBS) ────────────────────────────────

def cam_scenes(scene_names: "list[str]") -> "list[str]":
    """Return the subset of *scene_names* that are strih's real per-camera 'Cam N' scenes
    (exact match only — 'MV Cam N' twins and anything else are excluded), sorted in
    ascending camera-NUMBER order (not lexical — 'Cam 10' must sort after 'Cam 2')."""
    found = [s for s in scene_names if _CAM_SCENE_RE.match(s)]
    return sorted(found, key=lambda s: int(_CAM_SCENE_RE.match(s).group(1)))


# ─── live (OBS WS) functions ──────────────────────────────────────────────────

def warm_all(ws, scenes: "list[str]", settle_s: float) -> "list[str]":
    """Cycle *scenes* onto PREVIEW one at a time (settling *settle_s* seconds between
    each) so each scene's raw NDI input opens its receiver connection; restore the
    original preview scene afterward, even if a call in the loop raises. Returns the list
    of scenes actually warmed. No-op (zero RPC calls) when *scenes* is empty; no-op
    (returns [] after ONE RPC call) when Studio Mode is off — there is nothing to warm
    into and nothing to restore."""
    if not scenes:
        return []
    studio = bool(op._rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
    if not studio:
        return []
    orig = op._rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get("currentPreviewSceneName")
    warmed = []
    try:
        for scene in scenes:
            op._rpc(ws, "SetCurrentPreviewScene", {"sceneName": scene}, ignore_err=True)
            warmed.append(scene)
            time.sleep(settle_s)
    finally:
        if orig:
            op._rpc(ws, "SetCurrentPreviewScene", {"sceneName": orig}, ignore_err=True)
    return warmed


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--host", required=True, help="strih OBS WebSocket host (e.g. 10.77.9.202)")
    ap.add_argument("--password", default="", help="OBS WS password (or set OBS_PASSWORD env var)")
    ap.add_argument("--settle", type=float, default=1.5,
                     help="seconds to hold each camera scene on preview before moving to "
                          "the next (default: 1.5)")
    args = ap.parse_args()
    password = os.environ.get("OBS_PASSWORD", args.password)

    ws = op._conn(args.host, password)
    try:
        scenes = [s.get("sceneName") for s in op._rpc(ws, "GetSceneList").get("scenes", [])]
        targets = cam_scenes(scenes)
        if not targets:
            print("warm-cam-scenes: no 'Cam N' scenes found on this box — nothing to warm")
            return
        warmed = warm_all(ws, targets, args.settle)
        print(f"warm-cam-scenes: warmed {len(warmed)}/{len(targets)} camera scene(s) -> {warmed}")
    finally:
        ws.close()


if __name__ == "__main__":
    main()
