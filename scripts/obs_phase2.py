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
import os
import sys
import time

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

# #63: the probe ndi_source MUST be configured exactly like the live, proven-working camera
# inputs (NDI cam1/3/5) so it renders on the GENLOCK OBS build (OBS_GENLOCK_WALL_CLOCK=1).
# Defaults are wrong for the genlock compositor and make the probe render BLACK (0 decoded):
#   - genlock_fifo=True  -> the wall-clock-slaved render tick consumes exactly one queued
#                           frame per tick (camera-box #42 FIFO bypass). Without it the probe
#                           takes the normal async timestamp-cursor path, which can't be
#                           reconciled against the disciplined tick -> frames discarded.
#   - ndi_sync=1         -> PROP_SYNC_NDI_TIMESTAMP (the NDI *receiver*-side, monotonic
#                           timestamp). The DistroAV default is 2 (NDI_SOURCE_TIMECODE), which
#                           binds the cursor to the camera-box sender's WALL-CLOCK-epoch
#                           boundary timecode (src/ndi.rs) -> out-of-bounds vs the monotonic
#                           compositor cursor -> BLACK.
#   - ndi_bw_mode=0      -> highest bandwidth (full quality), as before.
# Merged FIRST in each settings dict so the per-call ndi_source_name still overrides cleanly.
# latency=0 (Normal) MIRRORS the live, proven cam inputs (NDI cam1/3/5 are all latency=0 on
# strih) and IS THE CERTIFIED low-latency zero-loss ingest mode (#84): the A/B measurement
# found the DistroAV receive buffer is NOT a real latency lever once genlock is active — the
# wall-clock render tick dominates emit timing, and Normal(0) gives a ~33 ms LOWER strih
# abs_emit p50 than Lowest(2) while staying zero-loss. The genlock FIFO preload
# (OBS_GENLOCK_PRELOAD_FRAMES) is the jitter buffer that matters. The probe MUST run at the
# pinned 0 (vendor/README.md ndi_input_latency) so this harness measures the certified config,
# not a different one. (Was latency=2 pre-#84, before the A/B re-pin to Normal(0).)
_PROBE_NDI_SETTINGS = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 1, "latency": 0}


def _load_state():
    """Read the per-host prev-scene state. Tolerates a MISSING or CORRUPT/truncated file
    (a crash mid-write can leave partial JSON) — returns {} rather than raising, so a bad
    state file can never make teardown raise before it restores the prior program scene
    (which would strand the probe scene as live program on a production OBS)."""
    try:
        with open(STATE) as f:
            return json.load(f)
    except (FileNotFoundError, ValueError):  # ValueError covers JSONDecodeError
        return {}


def _save_state(state):
    """Write state ATOMICALLY (tmp + os.replace) so a crash mid-write can never leave the
    corrupt file that _load_state would otherwise have to recover from."""
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


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


# #93: how long to wait after idling the probe receiver for the DistroAV av_thread to
# fully exit its reset_ndi_receiver block before we re-point the source. One render tick
# is ~20 ms at 50 fps; 0.25 s is comfortably several ticks of margin (the av_thread polls
# its reset flag once per loop iteration, ~5–100 ms) without slowing the run meaningfully.
_QUIESCE_RENDER_TICK_S = 0.25


def _quiesce_probe_input(ws):
    """#93: idle the reused probe ndi_source BEFORE re-pointing it, so the re-point lands
    on a dormant receiver instead of racing a live av_thread. Clearing ndi_source_name
    makes DistroAV tear the receiver down cleanly (the same idle discipline teardown uses);
    genlock_fifo off stops the dormant input running the consume path against an empty queue
    (#70). Then wait one render tick for the av_thread to exit its reset block. Best-effort:
    a quiesce failure must not abort setup (the C++ config_mutex fix is the real guard)."""
    _rpc(ws, "SetInputSettings", {
        "inputName": INPUT,
        "inputSettings": {"ndi_source_name": "", "genlock_fifo": False},
        "overlay": True,
    }, ignore_err=True)
    time.sleep(_QUIESCE_RENDER_TICK_S)


def setup(a):
    ws = _conn(a.host, a.password)
    prev = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")
    # In Studio Mode the PREVIEW scene's sources stay active (rendered). If teardown leaves
    # our probe scene in preview, its idle ndi_source keeps render-ticking the genlock FIFO
    # with an empty queue -> perpetual underrun-audit spam that corrupts the cumulative FIFO
    # audit (#70). Record the prior preview so teardown can restore it.
    studio = bool(_rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
    prev_preview = (
        _rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get("currentPreviewSceneName")
        if studio
        else None
    )

    out = _rpc(ws, "GetOutputSettings", {"outputName": MAIN_OUTPUT}, ignore_err=True)
    ndi_name = (out.get("outputSettings") or {}).get("ndi_name")
    if not ndi_name:
        raise SystemExit(
            f"[obs] {a.host}: DistroAV '{MAIN_OUTPUT}' is not enabled — enable it in "
            f"OBS (Tools > DistroAV / NDI Output Settings, 'Main Output') and set its "
            f"NDI name, then re-run. Phase 2 taps the program NDI this output emits."
        )

    # Snapshot the scene list once — used both for the prev-scene sanitizer and the
    # idempotent scene-exists check below.
    scenes = [s.get("sceneName") for s in _rpc(ws, "GetSceneList").get("scenes", [])]

    # Never record our own probe scene as the restore target: if a prior run crashed with
    # the probe on program, recover the real prior scene from the last good run's saved
    # state; if THAT is also missing/the probe, fall back to any existing non-probe scene.
    # This guarantees teardown can never strand the probe scene as live program on a box.
    if prev == SCENE:
        prev = _load_state().get(a.host, {}).get("prev_scene") or prev
    if not prev or prev == SCENE:
        prev = next((s for s in scenes if s != SCENE), None)
        sys.stderr.write(
            f"[obs] {a.host}: WARN prior program unknown/was the probe scene; "
            f"will restore to '{prev}'\n"
        )
    # Same probe-scene guard for the preview target: never restore the probe scene into
    # preview. Fall back to the (already-sanitized) program scene when unknown.
    if prev_preview == SCENE:
        prev_preview = _load_state().get(a.host, {}).get("prev_preview") or prev
    if not prev_preview or prev_preview == SCENE:
        prev_preview = prev
    state = _load_state()
    state[a.host] = {"prev_scene": prev, "prev_preview": prev_preview}
    _save_state(state)

    # Ensure the ONE stable scene+input exist, then reuse them (#22). Creating per run is
    # what made the fork's un-removable ndi_source inputs pile up.
    if SCENE not in scenes:
        _rpc(ws, "CreateScene", {"sceneName": SCENE}, ignore_err=True)
    inputs = [i.get("inputName") for i in _rpc(ws, "GetInputList").get("inputs", [])]
    if INPUT not in inputs:
        _rpc(ws, "CreateInput", {
            "sceneName": SCENE, "inputName": INPUT, "inputKind": "ndi_source",
            "inputSettings": {**_PROBE_NDI_SETTINGS, "ndi_source_name": a.upstream},
        }, ignore_err=True)
    else:
        # #93: QUIESCE before re-pointing a possibly-LIVE probe input. If a prior run
        # left the probe scene on program (a crash, or back-to-back runs), the
        # ndi_source receiver+av_thread are still live on the old upstream. Re-pointing
        # it in place (SetInputSettings → ndi_source_update) frees/reallocs the NDI
        # source-name string the av_thread is mid-read on → DistroAV heap corruption
        # (the strih OBS crash). The C++ config_mutex+owned-copies fix makes that race
        # safe, but the harness ALSO idles the receiver first (mirror teardown's idle
        # discipline) so the re-point lands on a dormant source: clear ndi_source_name
        # (DistroAV tears the receiver down cleanly) + genlock_fifo off, then wait one
        # render tick for the av_thread to fully exit its reset before re-pointing.
        _quiesce_probe_input(ws)
        # Reuse: re-point the now-idle input at this run's upstream, applying the full
        # certified probe settings idempotently in ONE update (no per-cycle HW-accel /
        # Latency churn on a live source).
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {**_PROBE_NDI_SETTINGS, "ndi_source_name": a.upstream},
            "overlay": True,
        }, ignore_err=True)
        # ... and make sure it is an item of the stable scene (re-add if the scene was
        # recreated above, or a prior run left the input orphaned).
        items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE},
                     ignore_err=True).get("sceneItems", [])
        if not any(it.get("sourceName") == INPUT for it in items):
            _rpc(ws, "CreateSceneItem",
                 {"sceneName": SCENE, "sourceName": INPUT}, ignore_err=True)

    # OBS ndi_source binds by the FULL "MACHINE (name)" network name; binding a bare name
    # (e.g. "2ME PGM") connects to nothing. Resolve BOTH the ingest source and this box's
    # own Main Output name to their full forms (polling discovery) BEFORE switching the
    # program scene, so a doomed run — a name that never resolves — fails fast with the
    # production program scene UNTOUCHED, never half-set-up.
    ingest_full, _ = _resolve_full(ws, INPUT, a.upstream)
    if "(" not in ingest_full:
        raise SystemExit(
            f"[obs] {a.host}: ingest source '{a.upstream}' did not resolve to a full NDI "
            f"name; aborting before touching the program scene."
        )
    if ingest_full != a.upstream:
        # Re-point to the resolved full NDI name only. overlay=True MERGES with the
        # existing settings, so the #63 genlock keys (genlock_fifo/ndi_sync) applied
        # above are PRESERVED — never set overlay=False here or this re-point would
        # full-replace the input and silently drop the genlock config (black render).
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": ingest_full},
            "overlay": True,
        }, ignore_err=True)
    out_full, _ = _resolve_full(ws, INPUT, ndi_name)
    if "(" not in out_full:
        raise SystemExit(
            f"[obs] {a.host}: own Main Output '{ndi_name}' did not resolve to a full NDI "
            f"name (the next hop would ingest a dead name); aborting before touching the "
            f"program scene."
        )
    # Everything resolved — NOW switch program to the probe scene (kept to the last step so
    # any failure above leaves the live program where it was).
    _rpc(ws, "SetCurrentProgramScene", {"sceneName": SCENE})
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


def _resolve_full(ws, inp, bare, timeout=20.0, interval=1.0):
    """Resolve `bare` to its full 'MACHINE (name)' NDI form, POLLING DistroAV discovery
    until it appears (or timeout). An OBS ndi_source binds by the full network name;
    binding the BARE Main-Output name (e.g. '2ME PGM') connects to nothing → black render
    → 0 decode on the next hop. Cold discovery may not list a just-started upstream/own
    output for a few seconds, so we wait for it rather than racing it with a fixed sleep
    (#22 verification exposed this on strih→stream). Names that are already full (contain
    '(') bind directly and pass through. Returns (full_or_bare, last_vals)."""
    if "(" in bare:  # already a full "MACHINE (name)" — binds directly, no discovery wait
        return bare, _ndi_source_list(ws, inp)
    end = time.time() + timeout
    while True:
        vals = _ndi_source_list(ws, inp)
        full = _match_full(vals, bare)
        if full != bare:
            return full, vals
        if time.time() >= end:
            sys.stderr.write(
                f"[obs] WARN: bare NDI name '{bare}' did not resolve to a full "
                f"'MACHINE (name)' within {timeout:.0f}s; binding bare (may not connect)\n"
            )
            return bare, vals
        time.sleep(interval)


def teardown(a):
    state = _load_state()  # corruption-safe: a bad state file must not stop the restore
    try:
        ws = _conn(a.host, a.password)
        host_state = state.get(a.host, {})
        prev = host_state.get("prev_scene")
        if prev:
            _rpc(ws, "SetCurrentProgramScene", {"sceneName": prev}, ignore_err=True)
        # Restore the prior PREVIEW too (Studio Mode): leaving the probe scene in preview
        # keeps its idle ndi_source active and render-ticking the genlock FIFO (#70 underrun
        # spam). Falls back to the program scene when no prior preview was recorded.
        prev_preview = host_state.get("prev_preview") or prev
        if prev_preview:
            _rpc(ws, "SetCurrentPreviewScene", {"sceneName": prev_preview}, ignore_err=True)
        # Idle the NDI receiver but KEEP the stable scene+input for the next run (#22).
        # Clearing ndi_source_name makes DistroAV tear the receiver down cleanly (destroying
        # an ndi_source while it is actively receiving the 1080p feed faults the NDI runtime
        # and crashes OBS). genlock_fifo is also turned OFF so the dormant input does not run
        # the genlock consume path against an empty queue -> the perpetual underrun-audit spam
        # that corrupted the cumulative FIFO audit (#70). setup re-applies _PROBE_NDI_SETTINGS
        # (genlock_fifo=True) on the next run. Reuse is what stops the per-run input
        # accumulation the fork caused.
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": "", "genlock_fifo": False},
            "overlay": True,
        }, ignore_err=True)
        ws.close()
        sys.stderr.write(
            f"[obs] {a.host}: restored program -> {prev}, preview -> {prev_preview}, "
            f"probe input idled (genlock off, reused next run)\n"
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
