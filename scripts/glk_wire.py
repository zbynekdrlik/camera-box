#!/usr/bin/env python3
"""Wire one genlocked OBS instance for a Phase-B validation run (#42).

Idempotently ensures the PHASE2-GENLOCK scene + 'genlock-in' ndi_source exist,
points the input at --upstream with genlock_fifo enabled, pins the scene-item
transform to fill the canvas (SCALE_INNER — a stale transform from a previous
source resolution otherwise shrinks the QR below decodability), and switches
program to the scene. Never touches any other scene (the own-scene rule).

issue 1380: every scene selection goes through obs_phase2's production-scene guard, and a
production-scene target is refused before connecting (owner hard rule 27.9.2026).

Usage:
  glk_wire.py --host 10.77.9.202 --port 4471 --upstream "DEVELBOX (SYNTH)" \
              --canvas-w 1920 --canvas-h 1080
"""
import argparse
import json
import os
import sys

try:
    from websocket import create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

SCENE = "PHASE2-GENLOCK"
INPUT = "genlock-in"


def _obs_phase2():
    """issue 1380: the sibling obs_phase2.py holds the ONE production-scene guard
    (`_refuse_forbidden_scene`, the owner hard rule "nemas ti nikdy v stream obs davat do programu
    scenu PRO!!!!!"). Imported lazily with its own sys.path insert, the same pattern as
    obs_phase2._dev_scene_module(); never a copy of the rule here."""
    here = os.path.dirname(os.path.abspath(__file__))
    if here not in sys.path:
        sys.path.insert(0, here)
    import obs_phase2  # noqa: E402  (sibling module; needs the sys.path insert just above)
    return obs_phase2


def _conn(host, port):
    ws = create_connection(f"ws://{host}:{port}", timeout=10)
    json.loads(ws.recv())
    ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1}}))
    json.loads(ws.recv())
    return ws


def _rpc(ws, rtype, rdata=None, ignore_err=False):
    # issue 1380: a production-scene (or sceneUuid) selection is refused before it is sent.
    _obs_phase2()._refuse_forbidden_scene(rtype, rdata)
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    while True:
        m = json.loads(ws.recv())
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"] and not ignore_err:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--canvas-w", type=int, required=True)
    ap.add_argument("--canvas-h", type=int, required=True)
    a = ap.parse_args()
    guard = _obs_phase2()
    try:
        # issue 1380: refuse a production-scene target before connecting or writing anything.
        guard._refuse_forbidden_scene("SetCurrentProgramScene", {"sceneName": SCENE})
        _wire(a)
    except guard.ForbiddenSceneError as e:
        sys.exit(f"[glk_wire] {a.host}:{a.port}: {e}")


def _wire(a):
    ws = _conn(a.host, a.port)
    scenes = [s["sceneName"] for s in _rpc(ws, "GetSceneList")["scenes"]]
    if SCENE not in scenes:
        _rpc(ws, "CreateScene", {"sceneName": SCENE})
    inputs = [i["inputName"] for i in _rpc(ws, "GetInputList")["inputs"]]
    settings = {"ndi_source_name": a.upstream, "ndi_bw_mode": 0, "genlock_fifo": True}
    if INPUT not in inputs:
        _rpc(ws, "CreateInput", {"sceneName": SCENE, "inputName": INPUT,
                                 "inputKind": "ndi_source", "inputSettings": settings})
    else:
        _rpc(ws, "SetInputSettings", {"inputName": INPUT, "inputSettings": settings,
                                      "overlay": True})
    items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE})["sceneItems"]
    ids = [it["sceneItemId"] for it in items if it["sourceName"] == INPUT]
    if not ids:
        _rpc(ws, "CreateSceneItem", {"sceneName": SCENE, "sourceName": INPUT})
        items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE})["sceneItems"]
        ids = [it["sceneItemId"] for it in items if it["sourceName"] == INPUT]
    _rpc(ws, "SetSceneItemTransform", {"sceneName": SCENE, "sceneItemId": ids[0],
        "sceneItemTransform": {
            "positionX": 0, "positionY": 0, "rotation": 0,
            "boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
            "boundsWidth": a.canvas_w, "boundsHeight": a.canvas_h,
            "cropLeft": 0, "cropRight": 0, "cropTop": 0, "cropBottom": 0}})
    _rpc(ws, "SetCurrentProgramScene", {"sceneName": SCENE})
    prog = _rpc(ws, "GetCurrentProgramScene")["currentProgramSceneName"]
    print(f"{a.host}:{a.port} wired: ingest='{a.upstream}' genlock_fifo=True "
          f"bounds={a.canvas_w}x{a.canvas_h} program={prog}")
    ws.close()


if __name__ == "__main__":
    main()
