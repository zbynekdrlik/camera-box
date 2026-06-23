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

# #63/#149: the probe ndi_source MUST be configured EXACTLY like the live, certified, proven-
# working genlock camera inputs (NDI cam1/3/5 on strih) so the harness measures the SAME config
# that ships in production — never a divergent one. _LOCKED_BASELINE_KEYS below are asserted to
# equal the matching prod genlock input before any measurement (the #149 self-verify guard), so
# the MACHINE catches a drift between this harness and prod, not a human after a misconfigured run.
#   - genlock_fifo=True  -> the wall-clock-slaved render tick consumes exactly one queued
#                           frame per tick (camera-box #42 FIFO bypass). Without it the probe
#                           takes the normal async timestamp-cursor path, which can't be
#                           reconciled against the disciplined tick -> frames discarded.
#   - ndi_sync=2         -> NDI_SOURCE_TIMECODE (SOURCE timing). This is the #149 fix. The prod
#                           genlock cam inputs ALL run ndi_sync=2 (verified live: NDI cam1/3/5 =
#                           ndi_sync=2), and #136 timestamp-aligned release REQUIRES the frame to
#                           carry the wall-clock SOURCE timecode (is_wallclock_ts on
#                           next_frame->timestamp, src/ndi.rs). With ndi_sync=1
#                           (PROP_SYNC_NDI_TIMESTAMP, the NDI *receiver*-side monotonic
#                           timestamp) the frame carries the receiver's monotonic ts, so the
#                           #136 ts-align path silently NO-OPS — the harness then "proves" a
#                           code path it never actually exercised. ndi_sync MUST be 2 to mirror
#                           prod and to drive the #136 ts-align release the test claims to verify.
#                           (Was 1 here pre-#149 — a STALE pre-#136 value whose old justifying
#                           comment claimed the camera-box boundary timecode went out-of-bounds
#                           vs the monotonic cursor; that is obsolete now that #136 ships and
#                           prod itself runs ndi_sync=2.)
#   - ndi_bw_mode=0      -> highest bandwidth (full quality), matching prod.
# Merged FIRST in each settings dict so the per-call ndi_source_name still overrides cleanly.
# latency=0 (Normal) MIRRORS the live, proven cam inputs (NDI cam1/3/5 are all latency=0 on
# strih) and IS THE CERTIFIED low-latency zero-loss ingest mode (#84): the A/B measurement
# found the DistroAV receive buffer is NOT a real latency lever once genlock is active — the
# wall-clock render tick dominates emit timing, and Normal(0) gives a ~33 ms LOWER strih
# abs_emit p50 than Lowest(2) while staying zero-loss. The genlock FIFO preload
# (OBS_GENLOCK_PRELOAD_FRAMES) is the jitter buffer that matters. The probe MUST run at the
# pinned 0 (vendor/README.md ndi_input_latency) so this harness measures the certified config,
# not a different one. (Was latency=2 pre-#84, before the A/B re-pin to Normal(0).)
_PROBE_NDI_SETTINGS = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 0}

# #149: the locked baseline keys that the probe ingest MUST share BIT-FOR-BIT with the certified
# prod genlock input under test. These define the timing/quality regime the harness measures;
# if ANY of them diverges from prod, the harness is no longer measuring the production config and
# the run is invalid. The per-source genlock_preload (the #97 copied tuning, _GENLOCK_COPY_KEYS)
# is DELIBERATELY excluded — it is allowed to differ per source. The self-verify guard
# (_diverging_locked_keys + _assert_probe_matches_prod) asserts equality on exactly these keys
# before measuring and FAILS FAST with a precise per-key diagnostic on any mismatch.
_LOCKED_BASELINE_KEYS = ("ndi_sync", "genlock_fifo", "ndi_bw_mode", "latency")

# Per-source genlock TUNING to copy from a production input onto the probe input so the
# probe measures the SAME delay behaviour as the live chain. Copy ONLY the per-source
# preload (the #97 video-delay / FIFO depth) — NOT the #63-critical baseline
# (genlock_fifo / ndi_sync), which MUST stay pinned in _PROBE_NDI_SETTINGS so a prod input
# with a different value can never send the probe black. We read prod read-only and never
# touch the prod input or its scene; on read failure we just use the baseline (logged).
_GENLOCK_COPY_KEYS = ("genlock_preload",)


def _effective_input_settings(ws, input_name):
    """#149: return an input's EFFECTIVE settings = its TYPE DEFAULTS overlaid with the
    explicitly-persisted (non-default) settings.

    This is REQUIRED for a sound self-verify comparison. obs-websocket's GetInputSettings
    returns ONLY explicitly-saved (non-default) settings — it omits any key left at its
    default (the obs-websocket RequestHandler docstring states this verbatim). And THREE of
    the four #149 locked baseline values ARE the DistroAV ndi_source defaults
    (ndi_sync=2 SOURCE_TIMECODE, ndi_bw_mode=0 HIGHEST, latency=0 NORMAL). So a prod genlock
    input that left any of them at default would have that key ABSENT from GetInputSettings,
    making a raw dict comparison see None-vs-2 and FALSE-FAIL a perfectly valid run. Merging
    GetInputDefaultSettings underneath makes the dict reflect the input's TRUE effective
    config — the same effective config the compositor actually runs — so the guard compares
    like-for-like. Best-effort: a defaults read failure falls back to the explicit-only dict
    (the prior, brittle behaviour) rather than raising."""
    explicit = _rpc(ws, "GetInputSettings", {"inputName": input_name},
                    ignore_err=True).get("inputSettings", {})
    defaults = _rpc(ws, "GetInputDefaultSettings", {"inputKind": "ndi_source"},
                    ignore_err=True).get("defaultInputSettings", {})
    return {**defaults, **explicit}


def _is_genlock_prod_input(settings):
    """#149: True iff a prod ndi_source input's settings look like a CERTIFIED GENLOCK
    cam input (genlock_fifo enabled) rather than a non-genlock NDI input. The non-genlock
    inputs on a box (e.g. 'NDI 2ME PVW', 'NDI Bible' — preview/graphics feeds) run
    ndi_sync=1 and have no genlock_fifo; matching one of THOSE for the baseline would make
    the guard demand ndi_sync=1 and re-introduce the #149 bug. Only an input that is itself
    genlocked (genlock_fifo truthy) is a valid certified baseline for the probe.

    NOTE: genlock_fifo is a DistroAV addition that is NOT in the type defaults, so it only
    appears here when it was explicitly persisted — which is exactly when the input is a
    genlock input. So this stays correct whether settings come from the raw GetInputSettings
    or the defaults-merged effective dict."""
    return bool(settings.get("genlock_fifo"))


def _find_prod_genlock_input(ws, host, upstream_ndi_name):
    """#149: locate the CERTIFIED prod GENLOCK input whose ndi_source_name matches
    *upstream_ndi_name* (exact or substring) on *host*, and return
    ``(input_name, full_settings_dict)`` for it — or ``(None, {})`` if none is found / a
    read fails.

    The returned full settings drive BOTH (a) the per-source preload copy (_GENLOCK_COPY_KEYS)
    and (b) the #149 self-verify guard (_LOCKED_BASELINE_KEYS), so we read the prod input
    ONCE. NEVER modifies any prod input or scene. Best-effort: exceptions are caught and the
    caller falls back gracefully (with NO certified baseline → the guard cannot run).
    """
    try:
        inputs = _rpc(ws, "GetInputList", ignore_err=True).get("inputs", [])
        ndi_inputs = [
            i["inputName"] for i in inputs
            if i.get("inputKind") == "ndi_source" and i["inputName"] != INPUT
        ]
        # Find the prod GENLOCK input whose ndi_source_name matches the upstream we are
        # ingesting (e.g. "CAM1 (usb)" on strih, or the strih NDI name on stream).
        # Skip non-genlock inputs (they share the same source name family but run a
        # different timing regime — they are NOT a valid certified baseline, #149).
        # Collect ALL genlock matches and PREFER an EXACT ndi_source_name match over a
        # mere substring match, so a longer name that merely CONTAINS the upstream can't
        # be picked ahead of the exact source (which would copy the wrong genlock_preload).
        exact = None
        substring = None
        for inp_name in ndi_inputs:
            try:
                s = _effective_input_settings(ws, inp_name)
                if not _is_genlock_prod_input(s):
                    continue
                src = s.get("ndi_source_name", "")
                if src == upstream_ndi_name:
                    exact = (inp_name, s)
                    break  # exact match is the best possible — stop
                if substring is None and upstream_ndi_name and upstream_ndi_name in src:
                    substring = (inp_name, s)
            except Exception:
                continue
        chosen = exact or substring
        if chosen:
            inp_name, s = chosen
            how = "exact" if exact else "substring"
            sys.stderr.write(
                f"[obs] {host}: certified prod genlock input '{inp_name}' "
                f"(ndi_source_name='{s.get('ndi_source_name', '')}') matched ({how}) "
                f"for '{upstream_ndi_name}'\n"
            )
            return inp_name, s
        sys.stderr.write(
            f"[obs] {host}: WARN could not find a certified prod GENLOCK ndi_source "
            f"input matching '{upstream_ndi_name}'; probe will use default genlock "
            f"settings and the #149 self-verify guard cannot assert against prod\n"
        )
        return None, {}
    except Exception as e:
        sys.stderr.write(
            f"[obs] {host}: WARN reading prod genlock settings failed ({e}); "
            f"probe will use default genlock settings\n"
        )
        return None, {}


def _diverging_locked_keys(certified, probe, keys=_LOCKED_BASELINE_KEYS):
    """#149 self-verify CORE (pure, testable): given the CERTIFIED prod genlock input's
    settings *certified* and the PROBE ingest's effective settings *probe*, return the
    sorted list of *keys* whose values differ between them (or are missing from probe).

    A returned list that is non-empty means the harness would measure a config that
    DIVERGES from the certified production config — the caller must FAIL FAST. An empty
    list means the probe's locked baseline mirrors prod exactly. This compares ONLY the
    locked baseline keys; per-source tuning (genlock_preload) is intentionally not here.

    Each diverging entry is a dict: {"key", "expected" (prod), "actual" (probe)}."""
    diverging = []
    for k in keys:
        exp = certified.get(k)
        act = probe.get(k)
        if exp != act:
            diverging.append({"key": k, "expected": exp, "actual": act})
    return diverging


def _assert_probe_matches_prod(host, prod_input_name, certified, probe_effective):
    """#149 self-verify GUARD: assert the probe ingest's effective locked baseline equals
    the CERTIFIED prod genlock input's, EXACTLY, before any measurement. On ANY mismatch
    raise SystemExit with a precise per-key diagnostic (which key, prod-expected vs
    probe-actual) so a config drift between the harness and prod can NEVER again produce a
    silently-invalid measurement that a human has to catch. The machine guards the config.

    If no certified prod genlock input was found (certified is falsy / empty), the guard
    cannot assert and aborts loudly — measuring against an UNKNOWN prod config is exactly
    the failure mode #149 closes, so we never proceed without a baseline to verify against."""
    if not certified:
        raise SystemExit(
            f"[obs] {host}: #149 self-verify ABORT — no certified prod GENLOCK input found "
            f"to verify the probe against. Refusing to measure a config that cannot be "
            f"confirmed to mirror production. Ensure the prod genlock cam input for this "
            f"source is configured (genlock_fifo on) and discoverable over obs-websocket."
        )
    diverging = _diverging_locked_keys(certified, probe_effective)
    if diverging:
        lines = "\n".join(
            f"    - {d['key']}: prod(certified)={d['expected']!r}  probe={d['actual']!r}"
            for d in diverging
        )
        raise SystemExit(
            f"[obs] {host}: #149 self-verify FAIL — probe ingest config DIVERGES from the "
            f"certified prod genlock input '{prod_input_name}' on these locked baseline "
            f"keys:\n{lines}\n"
            f"  The harness MUST measure the exact production config. Aborting before any "
            f"measurement (a divergent config would silently invalidate the proof). Fix "
            f"_PROBE_NDI_SETTINGS in scripts/obs_phase2.py to mirror prod, or fix the prod "
            f"input, then re-run."
        )
    sys.stderr.write(
        f"[obs] {host}: #149 self-verify OK — probe locked baseline "
        f"{ {k: probe_effective.get(k) for k in _LOCKED_BASELINE_KEYS} } matches certified "
        f"prod genlock input '{prod_input_name}'\n"
    )


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

    # Read the CERTIFIED production genlock input ONCE (its name + full settings). This
    # ensures the probe runs with the SAME certified genlock config as the live prod inputs
    # (e.g. the cam1 NDI input on strih, or the strih NDI input on stream) without ever
    # touching those prod inputs or their scenes, AND gives the #149 self-verify guard the
    # baseline to assert against. Falls back to _PROBE_NDI_SETTINGS if the prod read fails.
    prod_input_name, prod_settings = _find_prod_genlock_input(ws, a.host, a.upstream)
    # Per-source preload copy: ONLY the _GENLOCK_COPY_KEYS (no overlap with the locked
    # baseline), so the #63/#149 baseline in _PROBE_NDI_SETTINGS is never overridden.
    prod_genlock = {k: prod_settings[k] for k in _GENLOCK_COPY_KEYS if k in prod_settings}
    if prod_genlock:
        sys.stderr.write(
            f"[obs] {a.host}: copying per-source genlock tuning from prod input "
            f"'{prod_input_name}': {prod_genlock}\n"
        )
    # _PROBE_NDI_SETTINGS is the certified #63/#149 baseline (genlock_fifo/ndi_sync/
    # ndi_bw_mode/latency); prod_genlock adds ONLY the per-source preload (no key overlap),
    # so the baseline is never overridden. Both are spread into BOTH the create and the reuse
    # call below — the #63 regression guard requires _PROBE_NDI_SETTINGS reach both paths.
    # ndi_source_name is always set explicitly so it is never inherited from prod.

    # Ensure the ONE stable scene+input exist, then reuse them (#22). Creating per run is
    # what made the fork's un-removable ndi_source inputs pile up.
    if SCENE not in scenes:
        _rpc(ws, "CreateScene", {"sceneName": SCENE}, ignore_err=True)
    inputs = [i.get("inputName") for i in _rpc(ws, "GetInputList").get("inputs", [])]
    if INPUT not in inputs:
        _rpc(ws, "CreateInput", {
            "sceneName": SCENE, "inputName": INPUT, "inputKind": "ndi_source",
            "inputSettings": {**_PROBE_NDI_SETTINGS, **prod_genlock, "ndi_source_name": a.upstream},
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
        # Latency churn on a live source). Spreads _PROBE_NDI_SETTINGS (the #63 baseline)
        # plus prod_genlock (the per-source preload copy) so the probe mirrors prod.
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {**_PROBE_NDI_SETTINGS, **prod_genlock, "ndi_source_name": a.upstream},
            "overlay": True,
        }, ignore_err=True)
        # ... and make sure it is an item of the stable scene (re-add if the scene was
        # recreated above, or a prior run left the input orphaned).
        items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE},
                     ignore_err=True).get("sceneItems", [])
        if not any(it.get("sourceName") == INPUT for it in items):
            _rpc(ws, "CreateSceneItem",
                 {"sceneName": SCENE, "sourceName": INPUT}, ignore_err=True)

    # Make the probe source FILL the canvas so a centered QR stays centered (and full
    # size) in this box's program / Main Output. Without this OBS renders the ndi_source
    # at its native size/position, so a centered single QR lands off-center downstream and
    # the centered decode ROI misses it (decode_failed=100% at strih/stream). STRETCH the
    # item to the base canvas from (0,0). Touches ONLY the probe scene item, never prod.
    vs = _rpc(ws, "GetVideoSettings", ignore_err=True)
    base_w = int(vs.get("baseWidth") or 1920)
    base_h = int(vs.get("baseHeight") or 1080)
    item_id = _rpc(ws, "GetSceneItemId", {"sceneName": SCENE, "sourceName": INPUT},
                   ignore_err=True).get("sceneItemId")
    if item_id is not None:
        _rpc(ws, "SetSceneItemTransform", {
            "sceneName": SCENE,
            "sceneItemId": item_id,
            "sceneItemTransform": {
                "boundsType": "OBS_BOUNDS_STRETCH",
                "boundsAlignment": 0,
                "boundsWidth": base_w,
                "boundsHeight": base_h,
                "positionX": 0,
                "positionY": 0,
                "alignment": 5,
            },
        }, ignore_err=True)

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
    # #91: the own-output resolution gate protects the NEXT OBS hop from ingesting a
    # dead bare name — but only a NON-terminal box HAS a next OBS hop. On the TERMINAL
    # box (stream) the box's own OBS can NEVER self-discover its own Main Output (NDI
    # suppresses self/loopback discovery of an output on the same machine), so a full
    # poll is GUARANTEED to exhaust its timeout doing nothing, and the abort would
    # always fire — blocking the strih→stream hop measurement. The terminal box's
    # output is tapped DIRECTLY by dev1; there is no next OBS hop to protect.
    out_full = _resolve_own_output(ws, a.host, ndi_name, terminal=a.terminal)

    # #149 SELF-VERIFY GUARD — the MACHINE guards the config, never the human. Read back the
    # probe ingest's EFFECTIVE locked baseline (after every SetInputSettings above) and assert
    # it equals the certified prod genlock input's EXACTLY. On any divergence — or no certified
    # prod baseline found — FAIL FAST with a precise per-key diagnostic, before switching the
    # program scene, so the production program is left UNTOUCHED and a config that diverges from
    # prod can never be silently measured. This runs LAST (after the resolved-name re-point) so
    # it verifies the input's true final state, mirroring the resolution gates' fail-fast order.
    # Read the probe's EFFECTIVE settings (defaults-merged) so the comparison is symmetric
    # with the certified prod settings (also defaults-merged) — see _effective_input_settings.
    probe_effective = _effective_input_settings(ws, INPUT)
    _assert_probe_matches_prod(a.host, prod_input_name, prod_settings, probe_effective)

    # Everything resolved AND self-verified — NOW switch program to the probe scene (kept to
    # the last step so any failure above leaves the live program where it was).
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


def _resolve_full(ws, inp, bare, timeout=45.0, interval=1.0):
    """Resolve `bare` to its full 'MACHINE (name)' NDI form, POLLING DistroAV discovery
    until it appears (or timeout). An OBS ndi_source binds by the full network name;
    binding the BARE Main-Output name (e.g. '2ME PGM') connects to nothing → black render
    → 0 decode on the next hop. Cold discovery may not list a just-started upstream/own
    output for a few seconds, so we wait for it rather than racing it with a fixed sleep
    (#22 verification exposed this on strih→stream). Names that are already full (contain
    '(') bind directly and pass through. Returns (full_or_bare, last_vals).

    timeout=45s (was 20s): after the harness stops cam2's camera-box + re-points the OBS
    ingest, the NDI discovery landscape reshuffles and strih's own Main Output ('2ME PGM')
    intermittently took >20s to re-appear in dev1's finder, aborting otherwise-good runs.
    This is an async network-advertisement wait, not a processing timeout."""
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


def _resolve_own_output(ws, host, ndi_name, terminal):
    """#91: resolve THIS box's own Main Output NDI name to the form the next consumer
    binds against, and decide whether a non-resolution is fatal.

    NON-terminal box: its output feeds the NEXT OBS hop, which binds the full
    'MACHINE (name)' network name — a bare name connects to nothing (black render,
    0 decode downstream). So poll discovery for the full form and ABORT (before
    touching the program scene) if it never resolves: a dead next-hop ingest is a
    fatal misconfiguration.

    TERMINAL box (stream): its output is tapped DIRECTLY by dev1, and the box's own
    OBS can NEVER self-discover its own output (NDI suppresses self/loopback
    discovery of an output on the same machine) — so a full poll is guaranteed to
    exhaust its timeout for nothing and there is no next OBS hop to protect. We do a
    SHORT best-effort resolve (in case the box's OBS happens to list it), and on
    failure emit the PARENTHESIZED suffix form '(ndi_name)' rather than the bare,
    generic name. dev1's tap matches by substring (ndi.rs `name.contains`), so the
    '(name)' suffix — the codebase's canonical full-name discriminator (see
    _match_full's `endswith(f"({bare})")`) — anchors the match on the exact source
    'MACHINE (ndi_name)' instead of letting a short generic bare word (e.g. 'stream')
    collide with any other LAN source whose name merely contains it (livestream, …).
    Returns the name string to print on stdout for the next consumer to tap/chain."""
    if not terminal:
        out_full, _ = _resolve_full(ws, INPUT, ndi_name)
        if "(" not in out_full:
            raise SystemExit(
                f"[obs] {host}: own Main Output '{ndi_name}' did not resolve to a full NDI "
                f"name (the next hop would ingest a dead name); aborting before touching the "
                f"program scene."
            )
        return out_full
    # Terminal box: short best-effort resolve (its own OBS almost never lists its own
    # output, so don't burn the full 20s poll), then fall back to the precise suffix.
    out_full, _ = _resolve_full(ws, INPUT, ndi_name, timeout=2.0)
    if "(" in out_full:
        # Self-resolved to the full 'MACHINE (name)', OR ndi_name was already
        # parenthesised — either way it contains '(' and binds directly.
        return out_full
    # ndi_name here cannot contain '(' (that path returned above), so the bare name is
    # a plain token (e.g. 'stream'); wrap it as the parenthesised suffix discriminator.
    suffix_form = f"({ndi_name})"
    sys.stderr.write(
        f"[obs] {host}: WARN terminal box's own Main Output '{ndi_name}' not "
        f"self-discoverable via its own OBS (NDI loopback suppression); no downstream "
        f"OBS hop to protect — emitting the suffix form '{suffix_form}' so dev1's tap "
        f"binds the exact 'MACHINE {suffix_form}' source (not a generic substring).\n"
    )
    return suffix_form


# #163: a recorded-black program is the failure this whole action exists to prevent
# (a probe ingest that received no NDI, or a scene wired to a dead source, renders pure
# black -> every recorded frame undecodable -> a wasted multi-minute run). The pure
# decision below is the fail-fast self-check: a frame whose PEAK luma is 0 is genuinely
# black (no signal at all); any non-zero peak means the source is rendering real content.
# We key on the PEAK (max), NOT the mean: a legitimately dark-but-live camera frame can
# have a very low mean (the live 'Cam 5' read mean ~30, but a darker scene could be ~1)
# while still carrying a decodable QR — only an all-zero frame (max==0) is truly black.
def _luma_is_black(luma_max):
    """#163 fail-fast self-check (pure): True iff the rendered program frame is black
    (no signal). Black == peak luma 0; any non-zero peak is real content. The decision
    is on the PEAK only — the caller logs the mean separately for diagnostics, but the
    mean never gates this (a dark-but-live frame has a low mean and a non-zero peak)."""
    return int(luma_max) == 0


def _program_luma(ws, scene_name):
    """Read the RENDERED program frame of *scene_name* via GetSourceScreenshot and
    return (max_luma, mean_luma) over a small downscaled PNG. Best-effort: returns
    (None, None) if the screenshot request fails or PIL is unavailable, so the caller
    can decide whether to proceed (we never BLOCK a run on the self-check being unable
    to run — only on it positively finding black)."""
    import base64
    import io

    res = _rpc(ws, "GetSourceScreenshot", {
        "sourceName": scene_name,
        "imageFormat": "png",
        "imageWidth": 320,
        "imageHeight": 180,
    }, ignore_err=True)
    data = res.get("imageData")
    if not data:
        return None, None
    try:
        from PIL import Image
    except ImportError:
        return None, None
    try:
        b64 = data.split(",", 1)[1] if data.startswith("data:") else data
        im = Image.open(io.BytesIO(base64.b64decode(b64))).convert("L")
        px = list(im.getdata())
        if not px:
            return None, None
        return max(px), sum(px) / len(px)
    except Exception:
        return None, None


def _restore_target(prev, target, ephemeral, scenes, saved_prev=None):
    """#163 (pure, testable): decide the scene teardown should restore PROGRAM to, given
    the program scene seen at prod-scene time (*prev*), the record *target*, whether the
    target is *ephemeral* (a scene we built — must never be restored to), the box's
    *scenes* list, and the last good run's *saved_prev* (for crash recovery).

    - ephemeral target: never restore to it; if prev was the target, recover saved_prev,
      else fall back to any other existing scene (the stream temp scene case).
    - real prod target: faithful restore — keep whatever was program, INCLUDING the target
      itself if it was already live program (don't bump the box off a legit prod scene)."""
    if ephemeral:
        if prev == target:
            prev = saved_prev or prev
        if not prev or prev == target:
            prev = next((s for s in scenes if s != target), None)
        return prev
    # Real prod scene: keep what was shown (target included). Only fill a missing prev.
    return prev or next((s for s in scenes), None)


def prod_scene(a):
    """#163: route this box's OBS PROGRAM to a CERTIFIED PRODUCTION scene and verify it
    is rendering NON-BLACK, so the recording-based E2E records the REAL production scene
    program (the same pixels a viewer sees) instead of a probe ndi_source that collides
    with the always-on prod input on the same NDI source-name (which records pure black).

    On strih the prod scene (e.g. 'Cam 5') already shows cam1 via the genlock-certified
    'NDI cam5' input; on stream a full-screen scene (e.g. 'REC-STRIH-TMP') already shows
    strih's feed via 'NDI 2ME PGM'. Either way: NO second receiver, NO source-name
    collision — the bug #163 closes.

    Steps (mirrors setup()'s safety order):
      1. Record prev program + (studio) prev preview to STATE so teardown restores them
         (reuses the SAME state keys setup uses, so teardown is unchanged).
      2. If --ensure-source is given, ensure a full-screen scene over that source exists
         (the stream temp scene). Touches ONLY that scene, never any prod scene.
      3. Read this box's DistroAV Main Output ndi_name (printed on stdout for chaining).
      4. Switch program to the named prod scene.
      5. FAIL FAST if the program renders black (GetSourceScreenshot luma peak 0) — a
         black program means the source isn't delivering; abort before StartRecord
         wastes a full run (the #163 'never waste a run on a black ingest' guard).
    """
    ws = _conn(a.host, a.password)
    try:
        prev = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")
        studio = bool(_rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
        prev_preview = (
            _rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get("currentPreviewSceneName")
            if studio else None
        )

        scenes = [s.get("sceneName") for s in _rpc(ws, "GetSceneList").get("scenes", [])]
        target = a.program_scene
        # A scene we BUILD (--ensure-source, the stream temp scene) is EPHEMERAL: it must
        # never be a restore target (restoring program to the throwaway record scene would
        # strand it as live program). A plain --program-scene is a real existing prod scene
        # (strih 'Cam 5'): if it WAS the live program, restoring it to ITSELF is the most
        # faithful restore — don't bump the box off a legit prod scene onto an arbitrary one.
        ephemeral = bool(a.ensure_source)
        # ONE state read: reused both for the crash-recovery fallbacks and the write-back.
        state = _load_state()
        saved = state.get(a.host, {})
        prev = _restore_target(prev, target, ephemeral, scenes, saved.get("prev_scene"))
        # Preview restore mirrors the program decision (never strand the ephemeral scene
        # in preview either; for a real prod scene keep what was shown, falling back to the
        # restored program scene).
        prev_preview = _restore_target(
            prev_preview, target, ephemeral, scenes, saved.get("prev_preview")
        ) or prev
        if ephemeral and prev is None:
            sys.stderr.write(
                f"[obs] {a.host}: WARN prior program unknown/was the ephemeral record "
                f"scene; will restore to '{prev_preview}'\n"
            )
        state[a.host] = {"prev_scene": prev, "prev_preview": prev_preview}
        _save_state(state)

        # #163: optionally ensure a dedicated full-screen scene over a source (the stream
        # temp scene). It references an EXISTING prod input (e.g. 'NDI 2ME PGM') — never a
        # new receiver — so there is no source-name collision; we just stretch it to fill
        # the canvas so the recorded program is full-frame. On strih --ensure-source is
        # unused (the prod 'Cam 5' scene already exists and is full-screen).
        if a.ensure_source:
            if target not in scenes:
                _rpc(ws, "CreateScene", {"sceneName": target}, ignore_err=True)
            items = _rpc(ws, "GetSceneItemList", {"sceneName": target},
                         ignore_err=True).get("sceneItems", [])
            if not any(it.get("sourceName") == a.ensure_source for it in items):
                _rpc(ws, "CreateSceneItem",
                     {"sceneName": target, "sourceName": a.ensure_source}, ignore_err=True)
            vs = _rpc(ws, "GetVideoSettings", ignore_err=True)
            base_w = int(vs.get("baseWidth") or 1920)
            base_h = int(vs.get("baseHeight") or 1080)
            item_id = _rpc(ws, "GetSceneItemId",
                           {"sceneName": target, "sourceName": a.ensure_source},
                           ignore_err=True).get("sceneItemId")
            if item_id is not None:
                _rpc(ws, "SetSceneItemTransform", {
                    "sceneName": target, "sceneItemId": item_id,
                    "sceneItemTransform": {
                        "boundsType": "OBS_BOUNDS_STRETCH", "boundsAlignment": 0,
                        "boundsWidth": base_w, "boundsHeight": base_h,
                        "positionX": 0, "positionY": 0, "alignment": 5,
                    },
                }, ignore_err=True)
        elif target not in scenes:
            raise SystemExit(
                f"[obs] {a.host}: program scene '{target}' does not exist (and no "
                f"--ensure-source given to build it). Pass an existing certified prod "
                f"scene, or --ensure-source <prod input> to build a full-screen scene."
            )

        # This box's DistroAV Main Output NDI name — printed for the next consumer to
        # chain/tap (same stdout contract as setup()). The output must be enabled.
        out = _rpc(ws, "GetOutputSettings", {"outputName": MAIN_OUTPUT}, ignore_err=True)
        ndi_name = (out.get("outputSettings") or {}).get("ndi_name")
        if not ndi_name:
            raise SystemExit(
                f"[obs] {a.host}: DistroAV '{MAIN_OUTPUT}' is not enabled — enable it in "
                f"OBS and set its NDI name, then re-run."
            )

        # Switch program to the prod recording scene.
        _rpc(ws, "SetCurrentProgramScene", {"sceneName": target})
        # In Studio Mode also set it to preview so the rendered program is the prod scene
        # (a stale preview scene keeps render-ticking and can confuse a viewer, but does
        # not affect the recorded PROGRAM output).
        if studio:
            _rpc(ws, "SetCurrentPreviewScene", {"sceneName": target}, ignore_err=True)

        # #163 FAIL-FAST non-black self-check: wait a moment for the scene to render, then
        # read the program luma. If it is black, ABORT before StartRecord — a black
        # program records all-undecodable and wastes the whole run (the exact #163 waste).
        time.sleep(2.0)
        luma_max, luma_mean = _program_luma(ws, target)
        if luma_max is None:
            sys.stderr.write(
                f"[obs] {a.host}: WARN could not read program luma for the non-black "
                f"self-check (GetSourceScreenshot/PIL unavailable) — proceeding without "
                f"it; recording-verdict will still catch an all-black recording.\n"
            )
        elif _luma_is_black(luma_max):
            raise SystemExit(
                f"[obs] {a.host}: #163 self-check FAIL — program scene '{target}' renders "
                f"BLACK (luma peak={luma_max}, mean={luma_mean:.1f}). The source is not "
                f"delivering frames; aborting BEFORE StartRecord so a black recording never "
                f"wastes a full run. Check the certified prod input feeding '{target}' is "
                f"receiving NDI."
            )
        else:
            sys.stderr.write(
                f"[obs] {a.host}: #163 self-check OK — program '{target}' NON-BLACK "
                f"(luma peak={luma_max}, mean={luma_mean:.1f})\n"
            )

        sys.stderr.write(
            f"[obs] {a.host}: program -> {target} (prod scene); Main Output NDI "
            f"'{ndi_name}'\n"
        )
        print(ndi_name)  # stdout = the FULL/own NDI name to chain/tap for this program
    finally:
        ws.close()


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


def record(a):
    """#105 recording-based E2E: control OBS program recording over the WebSocket.

    --action start : begin recording the program output (the probe scene is on
                     program from setup()). Prints nothing on success.
    --action stop  : stop recording; prints the recorded file's ABSOLUTE PATH on the
                     OBS host (StopRecord returns outputPath in obs-ws 5.1+). The
                     harness then downloads that file from the host to dev1.
    --action status: prints `active=<bool> path=<current>` (for diagnostics).

    Recording uses OBS's CONFIGURED recording output (format/encoder/dir set in
    OBS Settings > Output). The harness sets the program scene to the probe scene via
    setup() first, so what is recorded is exactly the program a viewer would see —
    the #105 acceptance #1 "delivered only when OBS shows it in program out".
    """
    ws = _conn(a.host, a.password)
    try:
        if a.action == "start":
            st = _rpc(ws, "GetRecordStatus").get("outputActive", False)
            if st:
                # Already recording (a prior run's leftover) — stop it first so this
                # run gets a clean single file, never appended to a stale one.
                _rpc(ws, "StopRecord", ignore_err=True)
                time.sleep(1.0)
            _rpc(ws, "StartRecord")
            sys.stderr.write(f"[obs] {a.host}: recording STARTED\n")
        elif a.action == "stop":
            out = _rpc(ws, "StopRecord", ignore_err=True)
            path = (out or {}).get("outputPath", "")
            if not path:
                # Fallback: some obs-ws builds return no path on StopRecord — read the
                # record directory + last status so the caller still gets a location.
                sys.stderr.write(
                    f"[obs] {a.host}: StopRecord returned no outputPath; "
                    f"check the OBS record directory\n"
                )
            print(path)  # the ONLY stdout line: the recorded file path on the host
            sys.stderr.write(f"[obs] {a.host}: recording STOPPED -> {path}\n")
        elif a.action == "status":
            s = _rpc(ws, "GetRecordStatus")
            print(f"active={s.get('outputActive', False)} "
                  f"path={s.get('outputPath', '')}")
        else:
            raise SystemExit(f"unknown --action {a.action!r}")
    finally:
        ws.close()


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("setup", "teardown", "record", "prod-scene"):
        p = sub.add_parser(name)
        p.add_argument("--host", required=True)
        p.add_argument("--password", default="")
        if name == "record":
            p.add_argument(
                "--action", required=True, choices=("start", "stop", "status")
            )
        if name == "prod-scene":
            # #163: route program to a CERTIFIED PROD scene and record IT (no colliding
            # probe ndi_source). --program-scene is the existing prod scene to record;
            # --ensure-source (optional) builds a full-screen scene over that EXISTING
            # prod input when --program-scene is a dedicated temp scene (the stream box).
            p.add_argument("--program-scene", required=True)
            p.add_argument("--ensure-source", default="")
        if name == "setup":
            p.add_argument("--upstream", required=True)
            # #91: mark a TERMINAL box — one whose Main Output feeds NO downstream OBS
            # hop (it is tapped directly by dev1). For such a box the own-output
            # self-resolution abort is spurious (the box's own OBS can't self-discover
            # its own output via NDI loopback suppression) and there is no next hop to
            # protect, so setup() skips that abort and emits the bare name (dev1's tap
            # resolves the full NDI name itself). NON-terminal boxes (strih) keep the
            # protective abort. Defaults False — strih and teardown are non-terminal.
            p.add_argument("--terminal", action="store_true")
    a = ap.parse_args()
    {"setup": setup, "teardown": teardown, "record": record,
     "prod-scene": prod_scene}[a.cmd](a)


if __name__ == "__main__":
    main()
