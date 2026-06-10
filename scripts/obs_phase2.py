#!/usr/bin/env python3
"""OBS setup/teardown for Phase-2 NDI taps via obs-websocket v5.

Matches the LIVE vocab on the production OBS boxes (verified 2026-06-08):
  - NDI source input kind is `ndi_source`; the source name field is
    `ndi_source_name` (NOT `distroav_*`).
  - The program is re-emitted by the DistroAV "NDI Main Output" output. We do
    NOT create it — we read its configured `ndi_name` so the caller knows which
    NDI source to tap. **The DistroAV NDI Main Output must already be enabled in
    OBS (Tools menu) on each host; setup fails loudly if it is not.**

Per host, `setup` records the current program scene, ensures ONE stable-named probe
scene+`ndi_source` exists (reused across runs), re-points it at this run's upstream NDI
name, sets it to program, and prints that host's Main Output `ndi_name` on stdout.
`teardown` restores the prior program scene and IDLES the receiver (clears
`ndi_source_name`) but KEEPS the scene+input for the next run.

Why stable reuse (#22): the production DistroAV fork cannot delete an `ndi_source` input
over the websocket API, so the old per-run PID-suffixed inputs were never cleaned up —
they accumulated and cluttered the OBS audio mixer (24 stuck inputs observed). Reusing one
fixed name leaves exactly one dormant probe artifact per box, forever — never per-run
growth.

Requires: pip install websocket-client. OBS WebSocket :4455 (pass --password if a
host requires auth; LAN boxes here use none).
"""
import argparse
import json
import sys

try:
    from websocket import create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

PORT = 4455
STATE = "/tmp/obs_phase2_state.json"
MAIN_OUTPUT = "NDI Main Output"
# #22: ONE stable-named scene+input per box, reused across every run. Per-run pid-suffixed
# names made DistroAV ndi_source inputs accumulate (the fork's RemoveInput no-ops), so we
# fix the names and keep the artifacts dormant between runs instead of recreating them.
SCENE = "PHASE2-PROBE"
INPUT = "phase2-probe-src"


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

    # If a prior run crashed without restoring, the current program may already be our
    # own probe scene — never record that as the restore target. Recover the real prior
    # scene from the last good run's saved state instead.
    if prev == SCENE:
        try:
            prev = json.load(open(STATE)).get(a.host, {}).get("prev_scene") or prev
        except FileNotFoundError:
            pass
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    state[a.host] = {"prev_scene": prev}
    json.dump(state, open(STATE, "w"))

    # Ensure the ONE stable scene+input exist, then reuse them (#22). Creating per run is
    # what made the fork's un-removable ndi_source inputs pile up.
    scenes = [s.get("sceneName") for s in _rpc(ws, "GetSceneList").get("scenes", [])]
    if SCENE not in scenes:
        _rpc(ws, "CreateScene", {"sceneName": SCENE}, ignore_err=True)
    inputs = [i.get("inputName") for i in _rpc(ws, "GetInputList").get("inputs", [])]
    if INPUT not in inputs:
        _rpc(ws, "CreateInput", {
            "sceneName": SCENE, "inputName": INPUT, "inputKind": "ndi_source",
            "inputSettings": {"ndi_source_name": a.upstream, "ndi_bw_mode": 0},
        }, ignore_err=True)
    else:
        # Reuse: re-point the existing dormant input at this run's upstream ...
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": a.upstream, "ndi_bw_mode": 0},
            "overlay": True,
        }, ignore_err=True)
        # ... and make sure it is an item of the stable scene (re-add if the scene was
        # recreated above, or a prior run left the input orphaned).
        items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE},
                     ignore_err=True).get("sceneItems", [])
        if not any(it.get("sourceName") == INPUT for it in items):
            _rpc(ws, "CreateSceneItem",
                 {"sceneName": SCENE, "sourceName": INPUT}, ignore_err=True)

    # OBS ndi_source matches the FULL "MACHINE (name)" network name; an OBS Main
    # Output's ndi_name is bare (e.g. "2ME PGM"), and ingesting the bare name
    # connects to nothing. Snapshot this box's DistroAV-discovered source list
    # ONCE, then resolve both the ingest source and this box's own program output
    # to their full names. A downstream box may not list an upstream OBS output
    # in its own discovery, so we resolve each output on the box that PRODUCES it
    # (its own list always contains it) and print the full name for the next hop.
    vals = _ndi_source_list(ws, INPUT)
    ingest_full = _match_full(vals, a.upstream)
    if ingest_full != a.upstream:
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": ingest_full},
            "overlay": True,
        }, ignore_err=True)
    _rpc(ws, "SetCurrentProgramScene", {"sceneName": SCENE})
    out_full = _match_full(vals, ndi_name)
    ws.close()
    sys.stderr.write(
        f"[obs] {a.host}: program -> {SCENE} ingest '{ingest_full}'; "
        f"Main Output NDI '{out_full}'\n"
    )
    print(out_full)  # stdout = the FULL NDI name to tap / chain for this program


def _ndi_source_list(ws, inp):
    """The full 'MACHINE (name)' NDI source names DistroAV has discovered on this
    box, read from the ndi_source_name property's item list."""
    items = _rpc(ws, "GetInputPropertiesListPropertyItems", {
        "inputName": inp, "propertyName": "ndi_source_name",
    }, ignore_err=True).get("propertyItems", [])
    return [it.get("itemValue") for it in items if it.get("itemValue")]


def _match_full(vals, bare):
    """Map a bare NDI name to its full 'MACHINE (name)' form from `vals`; returns
    `bare` unchanged if it is already full or no candidate matches."""
    for v in vals:  # already full/exact
        if v == bare:
            return v
    for v in vals:  # bare output name as the "(suffix)" of a full name
        if v.endswith(f"({bare})"):
            return v
    for v in vals:  # last resort: any substring match
        if bare in v:
            return v
    return bare


def teardown(a):
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    try:
        ws = _conn(a.host, a.password)
        prev = state.get(a.host, {}).get("prev_scene")
        if prev:
            _rpc(ws, "SetCurrentProgramScene", {"sceneName": prev}, ignore_err=True)
        # Idle the NDI receiver but KEEP the stable scene+input for the next run (#22).
        # Clearing ndi_source_name makes DistroAV tear the receiver down cleanly (destroying
        # an ndi_source while it is actively receiving the 1080p feed faults the NDI runtime
        # and crashes OBS). We deliberately keep the one stable scene+input dormant rather
        # than destroy and recreate them — reuse is exactly what stops the per-run
        # accumulation the fork caused.
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": ""},
            "overlay": True,
        }, ignore_err=True)
        ws.close()
        sys.stderr.write(
            f"[obs] {a.host}: restored program -> {prev}, probe input idled (reused next run)\n"
        )
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
