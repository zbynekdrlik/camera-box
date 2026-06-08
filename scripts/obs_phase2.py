#!/usr/bin/env python3
"""OBS setup/teardown for Phase-2 NDI taps via obs-websocket v5.

Matches the LIVE vocab on the production OBS boxes (verified 2026-06-08):
  - NDI source input kind is `ndi_source`; the source name field is
    `ndi_source_name` (NOT `distroav_*`).
  - The program is re-emitted by the DistroAV "NDI Main Output" output. We do
    NOT create it — we read its configured `ndi_name` so the caller knows which
    NDI source to tap. **The DistroAV NDI Main Output must already be enabled in
    OBS (Tools menu) on each host; setup fails loudly if it is not.**

Per host, `setup` records the current program scene, makes a UNIQUELY-named temp
scene with an `ndi_source` pointing at the upstream NDI name, sets it to program,
and prints that host's Main Output `ndi_name` on stdout. `teardown` restores the
prior program scene and removes the temp scene + input. Unique per-run names mean
a leftover from a crashed run can never collide.

Requires: pip install websocket-client. OBS WebSocket :4455 (pass --password if a
host requires auth; LAN boxes here use none).
"""
import argparse
import json
import os
import sys

try:
    from websocket import create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

PORT = 4455
STATE = "/tmp/obs_phase2_state.json"
MAIN_OUTPUT = "NDI Main Output"


def _conn(host, password=""):
    import base64
    import hashlib

    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())
    ident = {"op": 1, "d": {"rpcVersion": 1}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())
    return ws


def _rpc(ws, rtype, rdata=None, ignore_err=False):
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    while True:
        m = json.loads(ws.recv())
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"] and not ignore_err:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def setup(a):
    ws = _conn(a.host, a.password)
    prev = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")

    out = _rpc(ws, "GetOutputSettings", {"outputName": MAIN_OUTPUT}, ignore_err=True)
    ndi_name = (out.get("outputSettings") or {}).get("ndi_name")
    if not ndi_name:
        raise SystemExit(
            f"[obs] {a.host}: DistroAV '{MAIN_OUTPUT}' is not enabled — enable it in "
            f"OBS (Tools > DistroAV / NDI Output Settings, 'Main Output') and set its "
            f"NDI name, then re-run. Phase 2 taps the program NDI this output emits."
        )

    suffix = os.getpid()
    scene = f"PHASE2-PROBE-{suffix}"
    inp = f"phase2-probe-src-{suffix}"
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    state[a.host] = {"prev_scene": prev, "scene": scene, "input": inp}
    json.dump(state, open(STATE, "w"))

    _rpc(ws, "CreateScene", {"sceneName": scene})
    _rpc(ws, "CreateInput", {
        "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
        "inputSettings": {"ndi_source_name": a.upstream, "ndi_bw_mode": 0},
    })
    _rpc(ws, "SetCurrentProgramScene", {"sceneName": scene})
    ws.close()
    sys.stderr.write(
        f"[obs] {a.host}: program -> {scene} ingest '{a.upstream}'; "
        f"Main Output NDI '{ndi_name}'\n"
    )
    print(ndi_name)  # stdout = the NDI name to tap for this host's program


def teardown(a):
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    try:
        ws = _conn(a.host, a.password)
        st = state.get(a.host, {})
        prev, scene, inp = st.get("prev_scene"), st.get("scene"), st.get("input")
        if prev:
            _rpc(ws, "SetCurrentProgramScene", {"sceneName": prev}, ignore_err=True)
        if inp:
            _rpc(ws, "RemoveInput", {"inputName": inp}, ignore_err=True)
        if scene:
            _rpc(ws, "RemoveScene", {"sceneName": scene}, ignore_err=True)
        ws.close()
        sys.stderr.write(f"[obs] {a.host}: restored program -> {prev}, temp scene removed\n")
    except Exception as e:  # teardown must never raise
        sys.stderr.write(f"[obs] {a.host}: teardown warning: {e}\n")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("setup", "teardown"):
        p = sub.add_parser(name)
        p.add_argument("--host", required=True)
        p.add_argument("--password", default="")
        if name == "setup":
            p.add_argument("--upstream", required=True)
    a = ap.parse_args()
    (setup if a.cmd == "setup" else teardown)(a)


if __name__ == "__main__":
    main()
