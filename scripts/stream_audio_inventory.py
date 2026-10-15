#!/usr/bin/env python3
"""issue 1421 -- READ-ONLY inventory of the stream OBS audio, printed when the E2E refuses a program
that carries more than the measurement signal: every input with a level meter, its peak over 10 s
(the obs-websocket InputVolumeMeters event), its mute, its monitor type, and whether the program
scene tree reaches it (nested scenes and groups, enabled items only). It never changes OBS.

  stream_audio_inventory.py <stream host> [port]
"""
import json
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2 as op  # noqa: E402
import measurement_audio_meter_probe as mp  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "10.77.9.204"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 4455
ws = op._conn(HOST, "")  # the rig OBS boxes run without auth
prog = op._rpc(ws, "GetCurrentProgramScene", {}, ignore_err=True).get("currentProgramSceneName")
scenes = [s["sceneName"] for s in op._rpc(ws, "GetSceneList", {}, ignore_err=True).get("scenes", [])]


def tree(scene, seen=None):
    seen = seen if seen is not None else set()
    if scene in seen:
        return set()
    seen.add(scene)
    out = set()
    items = op._rpc(ws, "GetSceneItemList", {"sceneName": scene}, ignore_err=True).get("sceneItems", [])
    for it in items:
        if not it.get("sceneItemEnabled", True):
            continue
        name = it.get("sourceName")
        out.add(name)
        if name in scenes:
            out |= tree(name, seen)
        if it.get("isGroup"):
            g = op._rpc(ws, "GetGroupSceneItemList", {"sceneName": name}, ignore_err=True).get("sceneItems", [])
            for gi in g:
                if gi.get("sceneItemEnabled", True):
                    out.add(gi.get("sourceName"))
    return out


in_prog = tree(prog)
# global audio devices (Desktop Audio / Mic) are always mixed
special = op._rpc(ws, "GetSpecialInputs", {}, ignore_err=True)

mws = mp._connect(HOST, PORT, "", 8)
peaks = {}
end = time.monotonic() + 10
mws.settimeout(0.5)
while time.monotonic() < end:
    try:
        m = json.loads(mws.recv())
    except Exception:
        continue
    d = m.get("d", {})
    if m.get("op") != 5 or d.get("eventType") != "InputVolumeMeters":
        continue
    for inp in d["eventData"].get("inputs", []):
        best = 0.0
        for ch in inp.get("inputLevelsMul", []):
            for v in ch:
                best = max(best, float(v))
        n = inp["inputName"]
        peaks[n] = max(peaks.get(n, 0.0), best)

print("   program scene:", prog)
print("   global audio inputs:", {k: v for k, v in special.items() if v and k != "requestId"})
print(f"   {'input':34} {'peak dBFS':>9} {'muted':>6} {'in program':>10} {'monitor':>26} kind")
for n in sorted(peaks, key=lambda k: -peaks[k]):
    mute = op._rpc(ws, "GetInputMute", {"inputName": n}, ignore_err=True).get("inputMuted")
    mon = op._rpc(ws, "GetInputAudioMonitorType", {"inputName": n}, ignore_err=True).get("monitorType")
    kind = op._rpc(ws, "GetInputSettings", {"inputName": n}, ignore_err=True).get("inputKind")
    db = 20 * math.log10(peaks[n]) if peaks[n] > 0 else -999
    inprog = n in in_prog or n in special.values()
    if inprog and not mute and db > -60:
        n = "* " + n  # audible on the program right now
    print(f"   {n:34} {db:9.1f} {str(mute):>6} {str(inprog):>10} {str(mon):>26} {kind}")
