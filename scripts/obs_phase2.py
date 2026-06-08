#!/usr/bin/env python3
"""OBS setup/teardown for Phase 2 NDI taps via obs-websocket v5.

Setup: on each OBS, ensure a dedicated Phase-2 scene with an NDI *source* (the
upstream node's NDI name), make it the program scene, and enable the DistroAV
"Main Output" so OBS re-emits an NDI *output* with the given name. Teardown:
restore the previously-active scene and disable the Phase-2 NDI output.

State to restore is written to /tmp/obs_phase2_state.json between setup/teardown.

Requires: pip install websocket-client. OBS WebSocket on :4455 (no auth on LAN;
pass --password if your boxes require one). DistroAV provides the
'distroav_main_output' / NDI source input kind 'distroav_ndi_source'.
"""
import argparse
import json
import sys

try:
    from websocket import create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

STATE = "/tmp/obs_phase2_state.json"
PORT = 4455


def _rpc(host, requests, password=""):
    """Minimal obs-websocket v5 client: connect, (no-auth) identify, run reqs."""
    import base64, hashlib
    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())  # op 0
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
    json.loads(ws.recv())  # op 2 Identified
    out = []
    for rtype, rdata in requests:
        rid = rtype
        ws.send(json.dumps({"op": 6, "d": {"requestType": rtype,
                                           "requestId": rid, "requestData": rdata}}))
        while True:
            msg = json.loads(ws.recv())
            if msg["op"] == 7 and msg["d"]["requestId"] == rid:
                out.append(msg["d"])
                break
    ws.close()
    return out


def setup(args):
    state = {}
    for host, scene, ndi_source, out_name in (
        (args.strih, "PHASE2", args.cam_source, args.strih_out),
        (args.stream, "PHASE2", args.strih_out, args.stream_out),
    ):
        cur = _rpc(host, [("GetCurrentProgramScene", {})], args.password)[0]
        state[host] = {"prev_scene": cur["responseData"].get("currentProgramSceneName")}
        _rpc(host, [
            ("CreateScene", {"sceneName": scene}),
        ], args.password)  # ignore "already exists" via responseData status below
        _rpc(host, [
            ("CreateInput", {
                "sceneName": scene, "inputName": f"src-{ndi_source}",
                "inputKind": "distroav_ndi_source",
                "inputSettings": {"ndi_source_name": ndi_source},
            }),
            ("SetCurrentProgramScene", {"sceneName": scene}),
            ("SetOutputSettings", {
                "outputName": "distroav_main_output",
                "outputSettings": {"ndi_name": out_name},
            }),
            ("StartOutput", {"outputName": "distroav_main_output"}),
        ], args.password)
        print(f"[obs] {host}: scene {scene} ingest '{ndi_source}' -> NDI out '{out_name}'")
    with open(STATE, "w") as f:
        json.dump(state, f)


def teardown(args):
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    for host in (args.strih, args.stream):
        try:
            _rpc(host, [("StopOutput", {"outputName": "distroav_main_output"})], args.password)
            prev = state.get(host, {}).get("prev_scene")
            if prev:
                _rpc(host, [("SetCurrentProgramScene", {"sceneName": prev})], args.password)
            print(f"[obs] {host}: NDI output stopped, scene restored to {prev}")
        except Exception as e:  # teardown must never raise
            print(f"[obs] {host}: teardown warning: {e}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("setup", "teardown"):
        p = sub.add_parser(name)
        p.add_argument("--strih", required=True)
        p.add_argument("--stream", required=True)
        p.add_argument("--password", default="")
        if name == "setup":
            p.add_argument("--cam-source", required=True)
            p.add_argument("--strih-out", required=True)
            p.add_argument("--stream-out", required=True)
        else:
            p.add_argument("--strih-out", default="")
            p.add_argument("--stream-out", default="")
    args = ap.parse_args()
    (setup if args.cmd == "setup" else teardown)(args)


if __name__ == "__main__":
    main()
