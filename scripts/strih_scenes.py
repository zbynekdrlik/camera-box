#!/usr/bin/env python3
"""strih_scenes.py -- strih-lx OBS input/scene/Studio-Mode seeder (issue 1317).

The strih-lx notebook's OBS boots EMPTY (no scenes, no inputs) -- it cannot receive or switch any
of the fleet's NDI sources until something seeds it. imag has scripts/imag_scenes.py, seeded on
every launch by imag-obs-start.sh; this is the focused strih-lx sibling (design comment on issue
1317, Prístup 1). It reuses the on-box obs_phase2.py certified-genlock primitives rather than
importing imag's 70KB DRM-lease/encoder/picom machinery that strih-lx does not need.

SCOPE: the 10 NDI INPUTS + one per-input scene + Studio Mode, from /opt/camera-box/strih-lx-seed.json
(setup-strih.sh step 6). The 5 STRIH-LX NDI OUTPUTS are a SEPARATE ticket -- this seeder never
touches outputs.

Idempotent: CreateScene/CreateInput "already exists" errors are ignored, and the certified genlock
settings are re-applied over the top of an existing input every run (overlay merge), so a
boot/relaunch always leaves the collection correct (the imag #785/#1156 launch-seed pattern -- a
segfault-relaunch of strih-obs.service must never leave OBS empty).

Modes:
  --bootstrap (default)  connect WS, CreateScene+CreateInput per input with the certified genlock
                         settings (genlock_fifo/ndi_sync=2/ndi_bw_mode=0/latency=<manifest floor 3>),
                         read-back-verify each ndi_source_name (the obs_phase2 #1158 name-enforce
                         shape when available), SetStudioModeEnabled true, a stable scene order.
  --verify-parity        read-only: report whether the 10 seed inputs exist as genlock_fifo sources
                         (the imag verify_parity grep-qxF whole-line shape; consumed report-only by
                         verify-strih.sh so a not-yet-launched box is never a hard FAIL).
  --projector T          issue 1346: rewrite /opt/camera-box/strih-lx-projector.json to T
                         (program|multiview) and (re)seed the fixed HDMI fullscreen projector. The
                         OBS UI projector menu stays the primary operator switch (SaveProjectors
                         persists it); this is the scripted twin. --bootstrap ALSO seeds the
                         projector (via seed_projector) after the input seed.

The pure helpers (parse_seed_manifest / seed_inputs / certified_genlock_settings / scene_order /
input_parity_problems, and for issue 1346 projector_type_to_mix / projector_monitor_index /
projector_already_saved / read_projector_type / write_projector_type) carry NO WebSocket/file
dependency, so they are Tier-0 testable with no rig (tests/python/test_strih_scenes_1317.py +
tests/python/test_strih_projector_1346.py).

#1156: the strih-obs-start.sh launch preflight `import strih_scenes` validates this import chain
(incl. the top-level `websocket` dep) BEFORE launching OBS, so a broken seed never Restart-loops a
live OBS. obs_phase2 is imported LAZILY (an older box may not carry it -> degrade to a direct set,
never crash the boot seed).
"""
import argparse
import json
import os
import sys

from websocket import create_connection

SEED_MANIFEST_PATH = "/opt/camera-box/strih-lx-seed.json"
# The genlock latency floor every camera input rides (strih_lx_camera_latency_ms; the rig floor).
# obs_phase2._PROBE_NDI_SETTINGS uses latency 0 (the probe default); strih-lx rides the manifest's
# floor 3 -- so the certified settings here override ONLY latency, keeping the #63/#149 genlock keys.
DEFAULT_CAMERA_LATENCY_MS = 3

# --- issue 1346: fixed HDMI fullscreen projector -------------------------------------------------
# The owner ROZHODNUTE (19.9.2026): the strih-lx HDMI output is an OBS fullscreen projector on the
# HDMI display, selectable between Program and Multiview, PERSISTED across relaunches (setup-strih.sh
# step 7 pre-seeds [BasicWindow] SaveProjectors=true). Default is Multiview -- the owner's current
# Windows strih setup (saved_projectors {monitor,type:4}). The OBS UI projector menu stays the
# primary operator switch; the strih_scenes.py --projector CLI is the scripted twin.
PROJECTOR_CONFIG_PATH = "/opt/camera-box/strih-lx-projector.json"
DEFAULT_PROJECTOR_TYPE = "multiview"
# obs-websocket 5 OpenVideoMixProjector videoMixType constants.
PROJECTOR_MIX_PROGRAM = "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"
PROJECTOR_MIX_MULTIVIEW = "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"
# OBS ProjectorType saved in a scene collection's saved_projectors: 3 = StudioProgram, 4 = Multiview.
# strih runs Studio Mode always, so a Program projector persists as StudioProgram (type 3).
PROJECTOR_TYPE_NUM = {"program": 3, "multiview": 4}
# The desktop OBS config dir (where user.ini + basic/scenes/<collection>.json live).
OBS_CONFIG_DIR = os.path.expanduser("~/.config/obs-studio")


def certified_genlock_settings(latency):
    """The certified per-input genlock settings (obs_phase2._PROBE_NDI_SETTINGS / _LOCKED_BASELINE_KEYS:
    ndi_bw_mode=0 HIGHEST, genlock_fifo=True, ndi_sync=2 SOURCE_TIMECODE) with `latency` from the
    manifest floor. ndi_source_name is NOT included here -- it is a per-input top-level field the
    seed merges in. Returned fresh each call (never a shared mutable default)."""
    return {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": latency}


def scene_name_for(src):
    """The per-input scene name the operator cuts to -- the source's own display name."""
    return src


def input_name_for(src):
    """The OBS input (source) name, kept DISTINCT from its scene name (an OBS scene + source with the
    same name is a UI ambiguity) -- the imag "NDI CAMx" convention generalised to the freeform strih
    source names."""
    return "NDI " + src


def parse_seed_manifest(text):
    """Pure: parse /opt/camera-box/strih-lx-seed.json text -> (inputs, outputs, latency).

    inputs/outputs are the non-empty string entries in manifest order; latency is camera_latency_ms
    coerced to int (falling back to DEFAULT_CAMERA_LATENCY_MS on a missing/garbage value). Raises
    ValueError on non-JSON / a non-object top level (a corrupt manifest must fail loud, never seed a
    silently-empty collection)."""
    d = json.loads(text)
    if not isinstance(d, dict):
        raise ValueError("strih-lx seed manifest must be a JSON object, got %r" % type(d).__name__)
    inputs = [s for s in (d.get("inputs") or []) if isinstance(s, str) and s]
    outputs = [s for s in (d.get("outputs") or []) if isinstance(s, str) and s]
    latency = d.get("camera_latency_ms", DEFAULT_CAMERA_LATENCY_MS)
    try:
        latency = int(latency)
    except (TypeError, ValueError):
        latency = DEFAULT_CAMERA_LATENCY_MS
    return inputs, outputs, latency


def seed_inputs(inputs, latency):
    """Pure: the seed PLAN -- one dict per input {scene, input, ndi_source_name, settings}. `settings`
    is the certified genlock dict (ndi_source_name lives at the top level, not inside settings, so a
    caller can CreateInput with {**settings, "ndi_source_name": src}). Duplicate/empty names are
    dropped so a manifest that repeats a name never double-creates a scene (the idempotency shape)."""
    plan = []
    seen = set()
    for src in inputs:
        if not src or src in seen:
            continue
        seen.add(src)
        plan.append({
            "scene": scene_name_for(src),
            "input": input_name_for(src),
            "ndi_source_name": src,
            "settings": certified_genlock_settings(latency),
        })
    return plan


def scene_order(inputs):
    """Pure: the stable, deterministic scene order (the scene names in manifest order, de-duplicated).
    strih-lx has no operator-tuned order to preserve; the seed creates scenes in this fixed order."""
    order = []
    seen = set()
    for src in inputs:
        if src and src not in seen:
            seen.add(src)
            order.append(scene_name_for(src))
    return order


def input_parity_problems(actual, expected_plan):
    """Pure: given `actual` = {inputName: inputSettings-dict} read over WS and `expected_plan` =
    seed_inputs(...), return a list of human-readable problem strings (empty list = every expected
    input present, a genlock_fifo source, bound to the right ndi_source_name, with the certified
    ndi_sync=2 SOURCE_TIMECODE). genlock_fifo and ndi_sync are the two certified genlock keys that
    define "a genlocked source" (obs_phase2 #149); the DistroAV `latency` mode field is deliberately
    NOT parity-checked here (a live receiver may normalise/clamp it, which would false-flag this
    report-only path). Used by --verify-parity and Tier-0-tested directly."""
    problems = []
    for item in expected_plan:
        inp = item["input"]
        src = item["ndi_source_name"]
        want_sync = item["settings"]["ndi_sync"]
        if inp not in actual:
            problems.append("MISSING %r" % inp)
            continue
        s = actual[inp] or {}
        if not s.get("genlock_fifo"):
            problems.append("%r not genlock_fifo" % inp)
        if s.get("ndi_source_name") != src:
            problems.append("%r ndi_source_name %r want %r" % (inp, s.get("ndi_source_name"), src))
        if s.get("ndi_sync") != want_sync:
            problems.append("%r ndi_sync %r want %r" % (inp, s.get("ndi_sync"), want_sync))
    return problems


# --- issue 1346: fixed HDMI projector pure helpers (no WS/file dependency -> Tier-0 testable) ------

def projector_type_to_mix(t):
    """Pure: map the persisted projector type string to the obs-websocket 5 videoMixType constant.
    'program' -> PROGRAM, 'multiview' -> MULTIVIEW; anything else raises ValueError (a corrupt
    strih-lx-projector.json must fail loud, never silently pick a default -- read_projector_type
    already applies the default; this maps a KNOWN type)."""
    if t == "program":
        return PROJECTOR_MIX_PROGRAM
    if t == "multiview":
        return PROJECTOR_MIX_MULTIVIEW
    raise ValueError("unknown projector type %r (want 'program' or 'multiview')" % (t,))


def projector_monitor_index(monitors):
    """Pure: the monitorIndex of the first EXTERNAL monitor -- the first whose monitorName does NOT
    start with 'eDP' (the internal notebook panel). None when only the eDP panel is present (or the
    list is empty). GetMonitorList shape: [{monitorName, monitorIndex}, ...]. The fixed HDMI output
    must NEVER fall back to the eDP panel (that would cover the operator's OBS UI) -- None tells
    seed_projector to SKIP and re-check on the next launch."""
    for m in monitors or []:
        name = ((m or {}).get("monitorName") or "")
        if not name.startswith("eDP"):
            return (m or {}).get("monitorIndex")
    return None


def projector_already_saved(saved_projectors, type_num, monitor_index):
    """Pure: True iff `saved_projectors` (the scene collection's list of {monitor, type} dicts OBS
    persists with SaveProjectors=true) already has an entry of ProjectorType `type_num` on
    `monitor_index`. Keeps the boot seed idempotent -- OBS re-opens saved projectors itself, so a
    second OpenVideoMixProjector would stack a DUPLICATE window (imag #756 class). Empty/None -> False
    (first boot: nothing saved yet -> the seed opens the projector)."""
    for p in saved_projectors or []:
        p = p or {}
        if p.get("type") == type_num and p.get("monitor") == monitor_index:
            return True
    return False


def read_projector_type(path=PROJECTOR_CONFIG_PATH):
    """Read the persisted projector type ('program'|'multiview') from strih-lx-projector.json. A
    missing / unreadable / non-JSON / unknown-type file -> the default 'multiview' (the owner's
    current Windows setup); the boot seed must never crash on an absent/garbage config."""
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError) as e:  # absent/garbage config is expected -> log + default, never crash
        print("projector: config %s unreadable (%s) -- defaulting to %s"
              % (path, e, DEFAULT_PROJECTOR_TYPE))
        return DEFAULT_PROJECTOR_TYPE
    t = (d or {}).get("type")
    if t in PROJECTOR_TYPE_NUM:
        return t
    print("projector: config %s has no known type (%r) -- defaulting to %s"
          % (path, t, DEFAULT_PROJECTOR_TYPE))
    return DEFAULT_PROJECTOR_TYPE


def write_projector_type(t, path=PROJECTOR_CONFIG_PATH):
    """Persist the projector type to strih-lx-projector.json (the --projector CLI writer). Rejects an
    unknown type with ValueError (never writes a value seed_projector could not map)."""
    if t not in PROJECTOR_TYPE_NUM:
        raise ValueError("unknown projector type %r (want 'program' or 'multiview')" % (t,))
    with open(path, "w") as fh:
        json.dump({"type": t}, fh)


def _obs_phase2_module():
    """Lazy import of the sibling obs_phase2.py (the SHARED #795-safe reenforce_ndi_name policy).
    Returns the module, or None when it is not importable on this host -- an older box may lack it, so
    the on-box read-back verify must DEGRADE to a direct set rather than crash the boot seed (the
    #1156 import-dependency class). Never imported at module load: strih-obs-start.sh's launch
    preflight only imports strih_scenes."""
    try:
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import obs_phase2  # noqa: E402
        return obs_phase2
    except Exception as e:  # noqa: BLE001 -- absence is expected on an older box; degrade, never crash
        print("#1156: obs_phase2 not importable (%s) -- strih NDI-name read-back uses a direct set "
              "(the discoverability gate is unavailable on this host)" % e)
        return None


class Obs:
    """Minimal OBS-WebSocket 5.x client (sibling of imag_scenes.Obs). no-auth on strih-lx
    (auth_required false, setup-strih.sh step 7); a password is still honoured if the server ever
    demands auth."""

    def __init__(self, host, port, password=None):
        self.ws = create_connection("ws://%s:%d" % (host, port), timeout=10)
        hello = json.loads(self.ws.recv())["d"]
        ident = {"op": 1, "d": {"rpcVersion": 1}}
        if "authentication" in hello:
            if not password:
                sys.exit("FAIL: OBS WS requires auth but no --password given")
            import base64
            import hashlib
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

    def req(self, req_type, data=None, ignore_err=False):
        self._rid += 1
        rid = str(self._rid)
        self.ws.send(json.dumps({"op": 6, "d": {
            "requestType": req_type, "requestId": rid, "requestData": data or {}}}))
        while True:
            msg = json.loads(self.ws.recv())
            if msg["op"] == 7 and msg["d"]["requestId"] == rid:
                st = msg["d"]["requestStatus"]
                if not st["result"] and not ignore_err:
                    sys.exit("FAIL: %s -> %s %s" % (req_type, st.get("code"), st.get("comment", "")))
                return msg["d"].get("responseData", {})


def _enforce_ndi_source_name(obs, op, input_name, desired_name):
    """Read-back-verify `input_name`'s ndi_source_name against `desired_name`. On the gated path
    (obs_phase2 available + a raw ws) reuse the #795-safe reenforce_ndi_name (discoverable -> set +
    read-back; offline -> left as-is, never a #795 mangle). Ungated (older box / a unit-test fake with
    no raw ws): a direct overlay set + read-back. Best-effort (ignore_err); never raises. Returns a
    short status string for the caller's log."""
    ws = getattr(obs, "ws", None)
    if op is not None and ws is not None:
        return op.reenforce_ndi_name(ws, input_name, desired_name)
    obs.req("SetInputSettings", {
        "inputName": input_name,
        "inputSettings": {"ndi_source_name": desired_name},
        "overlay": True,
    }, ignore_err=True)
    back = (obs.req("GetInputSettings", {"inputName": input_name}, ignore_err=True)
            .get("inputSettings", {}) or {}).get("ndi_source_name", "")
    return "healed" if back == desired_name else "verify_failed"


def bootstrap(obs, plan, studio=True):
    """Seed the collection from `plan` (seed_inputs output). Idempotent: CreateScene/CreateInput
    ignore "already exists", and the certified genlock settings are re-applied over the top of an
    existing input every run so genlock_fifo can never silently drift off across a relaunch (the
    features-default-on discipline -- genlock is not a forgettable toggle). Then SetStudioModeEnabled.
    Returns {input: name-status} for the log."""
    op = _obs_phase2_module()
    result = {}
    for item in plan:
        scene = item["scene"]
        inp = item["input"]
        src = item["ndi_source_name"]
        settings = item["settings"]
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        obs.req("CreateInput", {
            "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
            "inputSettings": dict(settings, ndi_source_name=src),
        }, ignore_err=True)
        # Re-arm the certified genlock settings on an EXISTING input too (CreateInput on an existing
        # input fails "already exists" -> ignored -> would NOT update settings). overlay:True merges,
        # leaving unrelated per-source keys (e.g. genlock_latency_ms_src) untouched.
        obs.req("SetInputSettings", {
            "inputName": inp,
            "inputSettings": dict(settings, ndi_source_name=src),
            "overlay": True,
        }, ignore_err=True)
        result[inp] = _enforce_ndi_source_name(obs, op, inp, src)
    if studio:
        obs.req("SetStudioModeEnabled", {"studioModeEnabled": True}, ignore_err=True)
    return result


def verify_parity(obs, manifest_text):
    """Read-only: print one whole-line verdict "strih ndi inputs: OK" (or the problem list) that
    verify-strih.sh greps with grep -qxF. Exit 1 on any problem, 0 when clean (the imag verify_parity
    exit contract). Never seeds/creates anything."""
    inputs, _outputs, latency = parse_seed_manifest(manifest_text)
    plan = seed_inputs(inputs, latency)
    actual = {}
    for inp in obs.req("GetInputList").get("inputs", []):
        if inp.get("inputKind") != "ndi_source":
            continue
        name = inp["inputName"]
        s = obs.req("GetInputSettings", {"inputName": name}, ignore_err=True)
        actual[name] = s.get("inputSettings", {})
    problems = input_parity_problems(actual, plan)
    print("strih ndi inputs: " + ("; ".join(problems) if problems else "OK"))
    if problems:
        sys.exit(1)


def _current_collection_saved_projectors(obs, cfg_dir):
    """Return the CURRENT scene collection's saved_projectors list (from the on-disk collection JSON
    OBS persists with SaveProjectors=true), or [] when it cannot be resolved/read (first boot: no
    saved file yet -> seed opens the projector). Resolves the collection name over
    GetSceneCollectionList (the design's approach) and reads
    <cfg_dir>/basic/scenes/<name>.json. A WS hiccup / missing / unreadable file -> [] (the seed
    proceeds and opens; a stacked duplicate is guarded only when a saved entry is genuinely present)."""
    name = ""
    try:
        name = ((obs.req("GetSceneCollectionList", ignore_err=True) or {})
                .get("currentSceneCollectionName") or "")
    except Exception as e:  # noqa: BLE001 -- a WS hiccup -> unresolved, seed proceeds; log it
        print("projector: could not read GetSceneCollectionList (%s) -- treating as unsaved" % e)
        return []
    if not name:
        return []
    path = os.path.join(cfg_dir, "basic", "scenes", name + ".json")
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError) as e:
        print("projector: collection file %s not readable (%s) -- treating as unsaved" % (path, e))
        return []
    sp = d.get("saved_projectors")
    return sp if isinstance(sp, list) else []


def seed_projector(obs, config_path=PROJECTOR_CONFIG_PATH, cfg_dir=None):
    """Seed the fixed HDMI fullscreen projector (issue 1346), run AFTER the input seed inside
    --bootstrap. Read the persisted type (strih-lx-projector.json, default multiview), resolve the
    EXTERNAL (non-eDP) HDMI monitor over GetMonitorList, and OpenVideoMixProjector there -- UNLESS the
    current scene collection already has a saved projector of that type on that monitor (OBS re-opens
    saved projectors itself with SaveProjectors=true, so a second open would DUPLICATE the window).
    NO external monitor -> log + SKIP, NEVER fall back to the eDP panel (that would cover the operator
    UI); the next launch re-checks. Best-effort (ignore_err on the WS calls); returns a short status
    string for the log."""
    if cfg_dir is None:
        cfg_dir = OBS_CONFIG_DIR
    ptype = read_projector_type(config_path)
    mons = (obs.req("GetMonitorList", ignore_err=True) or {}).get("monitors", []) or []
    idx = projector_monitor_index(mons)
    if idx is None:
        print("projector: no external monitor, skipping (monitors: %s; SaveProjectors will re-open "
              "one automatically once a display is plugged into HDMI)"
              % ([m.get("monitorName") for m in mons],))
        return "skipped-no-hdmi"
    type_num = PROJECTOR_TYPE_NUM[ptype]
    saved = _current_collection_saved_projectors(obs, cfg_dir)
    if projector_already_saved(saved, type_num, idx):
        print("projector: %s (ProjectorType %d) already saved on monitor %d -- OBS re-opens it, "
              "skipping (no duplicate window)" % (ptype, type_num, idx))
        return "already-saved"
    obs.req("OpenVideoMixProjector", {
        "videoMixType": projector_type_to_mix(ptype),
        "monitorIndex": idx,
    }, ignore_err=True)
    print("projector: opened %s fullscreen on monitor %d (SaveProjectors persists it)" % (ptype, idx))
    return "opened-%s" % ptype


def _read_manifest(path):
    with open(path) as fh:
        return fh.read()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=None)
    ap.add_argument("--manifest", default=SEED_MANIFEST_PATH,
                    help="path to strih-lx-seed.json (default %s)" % SEED_MANIFEST_PATH)
    ap.add_argument("--bootstrap", action="store_true",
                    help="seed scenes/inputs with the certified genlock settings + Studio Mode")
    ap.add_argument("--verify-parity", action="store_true",
                    help="read-only: report whether the 10 seed inputs exist as genlock_fifo sources")
    ap.add_argument("--projector", choices=["program", "multiview"], default=None,
                    help="issue 1346: rewrite strih-lx-projector.json to program|multiview and "
                         "(re)seed the fixed HDMI fullscreen projector (the OBS UI projector menu "
                         "stays the primary operator switch; SaveProjectors persists it)")
    args = ap.parse_args()

    # An explicit mode is REQUIRED (issue 1317 review): a bare invocation must never silently connect
    # and MUTATE OBS. --bootstrap seeds; --verify-parity is read-only; --projector sets the HDMI
    # projector (issue 1346).
    if not args.bootstrap and not args.verify_parity and args.projector is None:
        ap.error("specify a mode: --bootstrap (seed), --verify-parity (read-only), or "
                 "--projector program|multiview")

    # --projector (issue 1346): rewrite the persisted type + (re)seed the HDMI projector. Does NOT
    # read the input seed manifest -- it only touches the projector, not the input/scene seed.
    if args.projector is not None:
        write_projector_type(args.projector, PROJECTOR_CONFIG_PATH)
        obs = Obs(args.host, args.port, args.password)
        status = seed_projector(obs, PROJECTOR_CONFIG_PATH)
        print("projector set to %s (%s): %s" % (args.projector, PROJECTOR_CONFIG_PATH, status))
        return

    manifest_text = _read_manifest(args.manifest)

    obs = Obs(args.host, args.port, args.password)
    if args.verify_parity:
        verify_parity(obs, manifest_text)
        return
    # --bootstrap: seed the collection
    inputs, _outputs, latency = parse_seed_manifest(manifest_text)
    plan = seed_inputs(inputs, latency)
    statuses = bootstrap(obs, plan, studio=True)
    print("strih seed: %d inputs, latency %d ms; ndi names: %s"
          % (len(plan), latency,
             ", ".join("%s=%s" % (k, v) for k, v in sorted(statuses.items()))))
    print("scene order: " + " | ".join(scene_order(inputs)))
    # issue 1346: seed the fixed HDMI fullscreen projector AFTER the input seed (idempotent; SKIPs
    # cleanly when no external monitor is connected).
    seed_projector(obs)


if __name__ == "__main__":
    main()
