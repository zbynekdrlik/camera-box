#!/usr/bin/env python3
"""imag-nb OBS profile + scene seeding over WebSocket (#458, #501).

Idempotent — CreateScene/CreateInput "already exists" errors are ignored, settings are
re-applied on every run (self-healing, same philosophy as the genlock lockdown).

Seeds (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md, Phase 1):
  - video: 1920x1080 canvas+output @ 60fps
  - scenes "Cam 1".."Cam 6", each with one NDI input "NDI CAM<n>" -> "CAM<n> (usb)"
    (low-latency mode, muted audio, bounds-scaled to fill the canvas) — FULL-bandwidth,
    what the Stream Deck cuts to program.
  - scenes "MV Cam 1".."MV Cam 6" (#501), each with one NDI input "MV CAM<n>" bound to the
    SAME fleet NDI name but flagged genlock_monitor=true, so the vendor/distroav genlock
    lockdown forces LOW-bandwidth NDI receive (~9x cheaper) for these monitor-only twins.
    Root cause (issue #501, runtime-proven with an eglSwapBuffers-counting shim): the
    built-in 6-cell multiview costs ~80ms/render on imag's Linux/OpenGL build because every
    cell synchronously uploads ALL 6 cameras' FULL-1080p NDI textures (their async upload
    otherwise only happens when something renders them). Feeding the multiview from these
    low-bandwidth twins instead fits the #276/#278/#293 render-budget decouple back inside
    the 16.6ms tick.
  - built-in OBS multiview membership (per-scene `show_in_multiview` private setting, set
    via obs-websocket `SetSourcePrivateSettings` — mirrors OBSBasic_Scenes.cpp's
    "ShowInMultiview" context-menu action, which reads/writes the SAME key on
    `obs_source_get_private_settings(sceneSource)`): the 6 "MV Cam N" twins are shown, the 6
    real "Cam N" scenes (and everything else) are hidden — the cutter's Stream Deck still
    cuts the real full-bw scenes to program, the built-in multiview only ever renders the
    cheap low-bw twins.
  - Studio Mode ON, program parked on "Cam 1"
  --projector: fullscreen PROGRAM projector on the external (non-eDP/non-primary) monitor

Usage:
  imag_scenes.py --host 10.77.9.182 [--password PW] [--projector]
"""

import argparse
import base64
import hashlib
import json
import sys

from websocket import create_connection

CAMS = range(1, 7)
CANVAS_W, CANVAS_H, FPS = 1920, 1080, 60


class Obs:
    def __init__(self, host: str, port: int, password: str | None):
        self.ws = create_connection(f"ws://{host}:{port}", timeout=10)
        hello = json.loads(self.ws.recv())["d"]
        ident = {"op": 1, "d": {"rpcVersion": 1}}
        if "authentication" in hello:
            if not password:
                sys.exit("FAIL: OBS WS requires auth but no --password given")
            auth = hello["authentication"]
            secret = base64.b64encode(
                hashlib.sha256((password + auth["salt"]).encode()).digest()
            ).decode()
            ident["d"]["authentication"] = base64.b64encode(
                hashlib.sha256((secret + auth["challenge"]).encode()).digest()
            ).decode()
        self.ws.send(json.dumps(ident))
        json.loads(self.ws.recv())
        self._rid = 0

    def req(self, req_type: str, data: dict | None = None, ignore_err: bool = False):
        self._rid += 1
        rid = str(self._rid)
        self.ws.send(json.dumps({"op": 6, "d": {
            "requestType": req_type, "requestId": rid, "requestData": data or {}}}))
        while True:
            msg = json.loads(self.ws.recv())
            if msg["op"] == 7 and msg["d"]["requestId"] == rid:
                st = msg["d"]["requestStatus"]
                if not st["result"] and not ignore_err:
                    sys.exit(f"FAIL: {req_type} -> {st.get('code')} {st.get('comment', '')}")
                return msg["d"].get("responseData", {})


def seed(obs: Obs) -> None:
    obs.req("SetVideoSettings", {
        "baseWidth": CANVAS_W, "baseHeight": CANVAS_H,
        "outputWidth": CANVAS_W, "outputHeight": CANVAS_H,
        "fpsNumerator": FPS, "fpsDenominator": 1,
    }, ignore_err=True)  # fails only while an output is active — report at verify

    for n in CAMS:
        scene, inp, ndi_name = f"Cam {n}", f"NDI CAM{n}", f"CAM{n} (usb)"
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        obs.req("CreateInput", {
            "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
            "inputSettings": {"ndi_source_name": ndi_name, "latency": 1},  # 1 = Low latency
        }, ignore_err=True)
        # re-apply source binding + low latency every run (self-healing)
        obs.req("SetInputSettings", {
            "inputName": inp,
            "inputSettings": {"ndi_source_name": ndi_name, "latency": 1},
        }, ignore_err=True)
        obs.req("SetInputMute", {"inputName": inp, "inputMuted": True}, ignore_err=True)
        item = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                       ignore_err=True)
        if item.get("sceneItemId") is not None:
            obs.req("SetSceneItemTransform", {
                "sceneName": scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER",
                    "boundsAlignment": 0,
                    "boundsWidth": CANVAS_W, "boundsHeight": CANVAS_H,
                    "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

    # #501: low-bandwidth "MV Cam N" monitor-only twins. Same fleet NDI names as the real
    # "Cam N" scenes above, but genlock_monitor=true tells the vendor/distroav genlock lockdown
    # to force LOW-bandwidth NDI receive for these sources (~9x cheaper) instead of the
    # certified HIGHEST used by every full-bw source. Feeds the built-in multiview ONLY —
    # never bound to program.
    for n in CAMS:
        scene, inp, ndi_name = f"MV Cam {n}", f"MV CAM{n}", f"CAM{n} (usb)"
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        obs.req("CreateInput", {
            "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
            "inputSettings": {
                "ndi_source_name": ndi_name, "latency": 1, "genlock_monitor": True,
            },
        }, ignore_err=True)
        # re-apply source binding + low-bandwidth monitor flag every run (self-healing)
        obs.req("SetInputSettings", {
            "inputName": inp,
            "inputSettings": {
                "ndi_source_name": ndi_name, "latency": 1, "genlock_monitor": True,
            },
        }, ignore_err=True)
        obs.req("SetInputMute", {"inputName": inp, "inputMuted": True}, ignore_err=True)
        item = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                       ignore_err=True)
        if item.get("sceneItemId") is not None:
            obs.req("SetSceneItemTransform", {
                "sceneName": scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER",
                    "boundsAlignment": 0,
                    "boundsWidth": CANVAS_W, "boundsHeight": CANVAS_H,
                    "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

    obs.req("SetStudioModeEnabled", {"studioModeEnabled": True}, ignore_err=True)
    obs.req("SetCurrentProgramScene", {"sceneName": "Cam 1"}, ignore_err=True)

    v = obs.req("GetVideoSettings")
    ok = (v["fpsNumerator"], v["baseWidth"], v["baseHeight"]) == (FPS, CANVAS_W, CANVAS_H)
    scenes = [s["sceneName"] for s in obs.req("GetSceneList")["scenes"]]
    missing = [f"Cam {n}" for n in CAMS if f"Cam {n}" not in scenes]
    mv_missing = [f"MV Cam {n}" for n in CAMS if f"MV Cam {n}" not in scenes]

    # #501: built-in-multiview membership — show ONLY the low-bw "MV Cam N" twins; hide the
    # real full-bw "Cam N" scenes (and everything else) so the Stream Deck still cuts the real
    # scenes to program while the built-in multiview only ever renders the cheap twins. Mirrors
    # OBSBasic_Scenes.cpp's "ShowInMultiview" context-menu action (same `show_in_multiview` key
    # on obs_source_get_private_settings(sceneSource)), applied here over obs-websocket's
    # SetSourcePrivateSettings (which overlays onto the same per-source private-settings store).
    for name in scenes:
        obs.req("SetSourcePrivateSettings", {
            "sourceName": name,
            "sourceSettings": {"show_in_multiview": name.startswith("MV Cam ")},
        }, ignore_err=True)

    print(f"video: {v['baseWidth']}x{v['baseHeight']}@{v['fpsNumerator']}"
          f"/{v['fpsDenominator']} {'OK' if ok else 'MISMATCH (output active? retry idle)'}")
    print(f"scenes: {len([s for s in scenes if s.startswith('Cam ')])}/6"
          + (f" MISSING {missing}" if missing else " OK"))
    print(f"MV scenes: {len([s for s in scenes if s.startswith('MV Cam ')])}/6 (multiview, low-bw)"
          + (f" MISSING {mv_missing}" if mv_missing else " OK"))
    if not ok or missing or mv_missing:
        sys.exit(1)


def projector(obs: Obs) -> None:
    mons = obs.req("GetMonitorList")["monitors"]
    ext = [m for m in mons if "eDP" not in m.get("monitorName", "")]
    if not ext:
        sys.exit("FAIL: no external monitor detected — connect the HDMI monitor first "
                 f"(monitors: {[m.get('monitorName') for m in mons]})")
    target = ext[0]
    obs.req("OpenVideoMixProjector", {
        "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM",
        "monitorIndex": target["monitorIndex"],
    })
    print(f"program projector -> monitor {target['monitorIndex']} "
          f"({target.get('monitorName')}) — persisted via SaveProjectors")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", required=True)
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=None)
    ap.add_argument("--projector", action="store_true",
                    help="only open the fullscreen PROGRAM projector on the external monitor")
    args = ap.parse_args()
    obs = Obs(args.host, args.port, args.password)
    if args.projector:
        projector(obs)
    else:
        seed(obs)


if __name__ == "__main__":
    main()
