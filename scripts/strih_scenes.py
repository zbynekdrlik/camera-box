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
import glob
import json
import os
import re
import sys

from websocket import create_connection

SEED_MANIFEST_PATH = "/opt/camera-box/strih-lx-seed.json"
# The genlock latency floor every camera input rides (strih_lx_camera_latency_ms; the rig floor).
# obs_phase2._PROBE_NDI_SETTINGS uses latency 0 (the probe default); strih-lx rides the manifest's
# floor 3 -- so the certified settings here override ONLY latency, keeping the #63/#149 genlock keys.
DEFAULT_CAMERA_LATENCY_MS = 3
# issue 1317: the 2ME PGM/PVW FEEDBACK inputs are NOT genlocked. They are the strih's own post-render
# 30 fps program/preview outputs received back as monitoring feedback; a post-render output is off the
# camera boundary grid and every program CUT is a timecode discontinuity, so a genlock FIFO underruns
# and relocks on every cut (110,222 underruns / 899 relocks measured live on strih-lx 19.9.). The
# Windows strih receives these NON-genlocked (light.json NDI 2ME PGM/PVW: ndi_sync=1, latency=1, no
# genlock_fifo); the feedback class mirrors that. `latency` here is the stock DistroAV receive-buffer
# MODE enum (1 = LOW), NOT milliseconds -- FIXED at 1 (a feedback monitor, not a camera on the
# aligned grid), independent of the camera manifest floor (which rides genlock_latency_ms_src).
FEEDBACK_LATENCY_MODE = 1

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
    ndi_bw_mode=0 HIGHEST, genlock_fifo=True, ndi_sync=2 SOURCE_TIMECODE) with the manifest floor on
    `genlock_latency_ms_src` -- the REAL per-source genlock ms knob (issue 235 single knob, floor 3,
    the key latency_pins_verify.py reads). NOT the stock DistroAV `latency` key: that is a
    receive-buffer MODE enum which the genlock build's certified coercion forces back to 0 (NORMAL) on
    every genlock_fifo input, so a seeded `latency: 3` read back 0 forever and made --bootstrap
    re-write + name-re-enforce every camera on every launch (live strih-lx finding, 19.9.2026).
    ndi_source_name is NOT included here -- it is a per-input top-level field the seed merges in.
    Returned fresh each call (never a shared mutable default)."""
    return {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "genlock_latency_ms_src": latency}


def feedback_settings():
    """issue 1317: the NON-genlock settings for a 2ME PGM/PVW feedback input, mirroring the Windows
    strih light.json (ndi_sync=1 SOURCE_TIMING, latency=1, ndi_bw_mode=0). genlock_fifo is EXPLICIT
    False -- NOT omitted -- so a SetInputSettings(overlay=True) heal actually CLEARS a mis-seeded
    genlock_fifo=True off an already-existing input (an omitted key would leave the stale True in
    place, since overlay merges). Returned fresh each call (never a shared mutable default)."""
    return {"ndi_bw_mode": 0, "genlock_fifo": False, "ndi_sync": 1, "latency": FEEDBACK_LATENCY_MODE}


# --- issue 1344: the OBS program-audio input (`ASIO zvuk` on the strih-program PipeWire sink) -----
# The migrated collection carries the Windows `ASIO zvuk` (asio_input_capture) which `Failed to create
# source` on Linux. The seeder's ONE allowed create in update-only mode: if `ASIO zvuk` is missing OR
# is the un-creatable asio_input_capture, create/replace it as pulse_input_capture on the
# strih-program null sink's monitor — KEEPING the name (scene items / mixer tracks / E2E selectors
# address it by that name).
AUDIO_INPUT_NAME = "ASIO zvuk"
AUDIO_INPUT_KIND = "pulse_input_capture"
# issue 1344 follow-up (20.9.2026 live diagnosis, issuecomment-5751113173): the null-sink monitor
# `strih-program.monitor` is NOT pulse-visible on Ubuntu 26.04 pipewire -- support.null-audio-sink
# created via context.objects never gets a pulse.monitor mapping there, so OBS's pulse_input_capture
# enumeration never lists it and binding it reads digital silence (proven live: pw-cat captures real
# audio off it, OBS shows nothing). strih_pipewire_program_loopback_conf (strih-provision.sh)
# republishes the sink via a libpipewire-module-loopback as a real Audio/Source node
# `strih-program-source`, which OBS correctly enumerates and captures.
AUDIO_MONITOR_DEVICE = "strih-program-source"


def program_audio_input_settings():
    """The pulse_input_capture settings binding `ASIO zvuk` to the strih-program-source loopback
    node (issue 1344 follow-up -- NOT the null-sink monitor directly, see AUDIO_MONITOR_DEVICE).
    Returned fresh each call (never a shared mutable default)."""
    return {"device_id": AUDIO_MONITOR_DEVICE}


def program_audio_input_kind_from_inputs(inputs):
    """Pure: the OBS inputKind of the `ASIO zvuk` input from a GetInputList `inputs` list, or None
    when it is absent. Used by --audio-input-kind + verify-strih's derived audio verdict."""
    for i in inputs or []:
        if i.get("inputName") == AUDIO_INPUT_NAME:
            return i.get("inputKind")
    return None


def audio_input_action(exists, current_kind):
    """Pure: the ONE allowed action for the `ASIO zvuk` program-audio input.
    - absent -> 'create' (CreateInput pulse_input_capture on strih-program-source)
    - present but asio_input_capture (the un-creatable Windows kind) -> 'replace' (Remove + Create)
    - present as pulse_input_capture -> 'ok' (nothing to do)
    - present as some OTHER kind -> 'replace' (heal it to the pulse capture)."""
    if not exists:
        return "create"
    if current_kind == AUDIO_INPUT_KIND:
        return "ok"
    return "replace"


def input_class_for(name):
    """issue 1317: the settings CLASS for an input, keyed by name. Any name CARRYING the 2ME marker
    `2ME PGM` / `2ME PVW` (as a SUBSTRING) is a post-render FEEDBACK monitor -> 'feedback'. This
    matches BOTH the parallel-phase sender form `STRIH-SNV (2ME PGM)` AND the OPERATOR collection's
    explicit input names `NDI 2ME PVW` / `NDI 2ME PGM (mv)` (this lane) -- regardless of the
    STRIH-SNV/STRIH-LX prefix or the `(mv)` suffix, neither of which ENDS in `(2ME PGM)`. Everything
    else -- the `CAMn (usb)` / `NDI camN` grabbers AND the `RESOLUME-SNV (cg-obs)` / `cg` / `CG-obs`
    genlocked sources (issue 1300) -- is a genlocked source -> 'camera'."""
    n = name or ""
    if "2ME PGM" in n or "2ME PVW" in n:
        return "feedback"
    return "camera"


def input_settings_for(name, latency):
    """issue 1317: the seed settings for `name` by its class -- the certified genlock dict (latency
    from the manifest floor) for a camera, the non-genlock feedback dict (fixed latency 1) for a 2ME
    feedback input. The one seam seed_inputs applies per input."""
    if input_class_for(name) == "feedback":
        return feedback_settings()
    return certified_genlock_settings(latency)


def scene_name_for(src):
    """The per-input scene name the operator cuts to -- the source's own display name."""
    return src


def input_name_for(src):
    """The OBS input (source) name, kept DISTINCT from its scene name (an OBS scene + source with the
    same name is a UI ambiguity) -- the imag "NDI CAMx" convention generalised to the freeform strih
    source names."""
    return "NDI " + src


def _entry_fields(entry):
    """Pure (issue 1317, this lane): normalize ONE manifest `inputs` entry to (sender, input_name,
    scene). A bare STRING keeps today's derived-name behaviour -- sender = the string, input =
    `NDI `+string, scene = the string (the parallel/imag shape). An OBJECT `{sender,input,scene}`
    carries EXPLICIT names (the OPERATOR collection: sender = the NDI source received, input = the OBS
    input the E2E tooling addresses, scene = the operator's scene). Returns None for an unusable entry
    (empty string; a non-str/non-dict; an object missing/empty any of the three string fields) so a
    bad entry is DROPPED, never half-seeded."""
    if isinstance(entry, str):
        return (entry, input_name_for(entry), scene_name_for(entry)) if entry else None
    if isinstance(entry, dict):
        sender = entry.get("sender")
        inp = entry.get("input")
        scene = entry.get("scene")
        if all(isinstance(x, str) and x for x in (sender, inp, scene)):
            return (sender, inp, scene)
        return None
    return None


def parse_seed_manifest(text):
    """Pure: parse /opt/camera-box/strih-lx-seed.json text -> (inputs, outputs, latency).

    inputs/outputs are the non-empty string entries in manifest order; latency is camera_latency_ms
    coerced to int (falling back to DEFAULT_CAMERA_LATENCY_MS on a missing/garbage value). Raises
    ValueError on non-JSON / a non-object top level (a corrupt manifest must fail loud, never seed a
    silently-empty collection)."""
    d = json.loads(text)
    if not isinstance(d, dict):
        raise ValueError("strih-lx seed manifest must be a JSON object, got %r" % type(d).__name__)
    # issue 1317 (this lane): an `inputs` entry may be a bare STRING (parallel/imag shape) OR an
    # explicit-name OBJECT {sender,input,scene} (the OPERATOR collection). Keep every usable entry in
    # its ORIGINAL shape (seed_inputs normalizes via _entry_fields); drop the junk (int/None/empty/a
    # malformed object) so a bad entry never half-seeds.
    inputs = [e for e in (d.get("inputs") or []) if _entry_fields(e) is not None]
    outputs = [s for s in (d.get("outputs") or []) if isinstance(s, str) and s]
    latency = d.get("camera_latency_ms", DEFAULT_CAMERA_LATENCY_MS)
    try:
        latency = int(latency)
    except (TypeError, ValueError):
        latency = DEFAULT_CAMERA_LATENCY_MS
    return inputs, outputs, latency


def parse_seed_mode(text):
    """Pure (issue 1317, this lane): the seed MODE from the manifest -- 'update-only' | 'create'.
    'update-only' (the OPERATOR collection) makes --bootstrap heal the certified genlock CLASS onto
    inputs that ALREADY exist and NEVER CreateScene/CreateInput -- the operator's migrated collection
    is authoritative, so a missing declared input is REPORTED, not created. Anything else (incl. an
    absent `mode`) is 'create' (the parallel/imag shape, unchanged). A non-JSON / non-object manifest
    -> 'create' (never crash: parse_seed_manifest raises loudly on the same text; this is a lenient
    read of a single key)."""
    try:
        d = json.loads(text)
    except ValueError:
        return "create"
    if not isinstance(d, dict):
        return "create"
    return "update-only" if d.get("mode") == "update-only" else "create"


def seed_inputs(inputs, latency):
    """Pure: the seed PLAN -- one dict per input {scene, input, ndi_source_name, settings}. Each
    manifest entry is normalized via _entry_fields (a bare string = derived names; an object =
    explicit names, issue 1317 this lane). `settings` is the per-CLASS dict (camera = certified
    genlock; 2ME feedback = non-genlock) -- ndi_source_name lives at the top level, not inside
    settings, so a caller can CreateInput with {**settings, "ndi_source_name": sender}. Class is
    resolved from the SENDER *or* the INPUT name (either carrying the 2ME marker -> feedback), so the
    operator's `NDI 2ME PVW` / `NDI 2ME PGM (mv)` inputs classify feedback even when the sender is a
    supervisor-confirmable guess. De-duplicated by INPUT name (the OBS identity) so a repeated input
    never double-creates."""
    plan = []
    seen = set()
    for entry in inputs:
        fields = _entry_fields(entry)
        if fields is None:
            continue
        sender, input_name, scene = fields
        if input_name in seen:
            continue
        seen.add(input_name)
        cls = "camera"
        if input_class_for(sender) == "feedback" or input_class_for(input_name) == "feedback":
            cls = "feedback"
        settings = feedback_settings() if cls == "feedback" else certified_genlock_settings(latency)
        plan.append({
            "scene": scene,
            "input": input_name,
            "ndi_source_name": sender,
            "settings": settings,
        })
    return plan


def scene_order(inputs):
    """Pure: the stable, deterministic scene order (the scene names in manifest order, de-duplicated
    by INPUT name). strih-lx has no operator-tuned order to preserve; the seed creates scenes in this
    fixed order. Handles both bare-string and object entries (issue 1317, this lane) via _entry_fields."""
    order = []
    seen = set()
    for entry in inputs:
        fields = _entry_fields(entry)
        if fields is None:
            continue
        _sender, input_name, scene = fields
        if input_name in seen:
            continue
        seen.add(input_name)
        order.append(scene)
    return order


def input_parity_problems(actual, expected_plan, check_source=True):
    """Pure: given `actual` = {inputName: inputSettings-dict} read over WS and `expected_plan` =
    seed_inputs(...), return a list of human-readable problem strings (empty list = every expected
    input present, the right genlock class + ndi_sync, bound to the right ndi_source_name). genlock_fifo
    and ndi_sync are the two certified genlock keys that define "a genlocked source" (obs_phase2 #149);
    the DistroAV `latency` mode field is deliberately NOT parity-checked here (a live receiver may
    normalise/clamp it, which would false-flag this report-only path).

    issue 1317 (this lane): `check_source=False` SKIPS the ndi_source_name check -- for the OPERATOR
    collection (update-only mode) the operator's real senders are authoritative, not the manifest DATA
    guess, so verify-parity checks only that the declared INPUTS exist with the right genlock class.
    Used by --verify-parity and Tier-0-tested directly."""
    problems = []
    for item in expected_plan:
        inp = item["input"]
        src = item["ndi_source_name"]
        want_sync = item["settings"]["ndi_sync"]
        # issue 1317: genlock is class-derived from the plan -- a camera input WANTS genlock_fifo,
        # a 2ME feedback input wants it OFF (a feedback input left genlocked is exactly the drift this
        # ticket fixes, so flag it too).
        want_genlock = bool(item["settings"].get("genlock_fifo"))
        if inp not in actual:
            problems.append("MISSING %r" % inp)
            continue
        s = actual[inp] or {}
        if want_genlock and not s.get("genlock_fifo"):
            problems.append("%r not genlock_fifo" % inp)
        elif not want_genlock and s.get("genlock_fifo"):
            problems.append("%r unexpectedly genlock_fifo (2ME feedback input must be non-genlock)" % inp)
        if check_source and s.get("ndi_source_name") != src:
            problems.append("%r ndi_source_name %r want %r" % (inp, s.get("ndi_source_name"), src))
        if s.get("ndi_sync") != want_sync:
            problems.append("%r ndi_sync %r want %r" % (inp, s.get("ndi_sync"), want_sync))
    return problems


def settings_update_needed(effective, desired):
    """Pure (issue 1317): True iff any key in `desired` differs from `effective` (a key missing from
    `effective` counts as differing, EXCEPT genlock_fifo -- see below). `desired` = the class settings
    merged with ndi_source_name; `effective` = the input's defaults-merged effective settings
    (obs_phase2 _effective_input_settings shape). Drives the --bootstrap UPDATE-only path: an existing
    input already matching its class emits no SetInputSettings; a mis-seeded one (e.g. a 2ME feedback
    input left genlock_fifo=True) differs on a class key and is healed."""
    for k, v in desired.items():
        ev = effective.get(k)
        if k == "genlock_fifo":
            # DistroAV's genlock_fifo is NOT in the ndi_source type defaults (obs_phase2 #149), so
            # obs-websocket OMITS it when it equals its (unregistered zero == False) default. A
            # correctly-seeded feedback input (genlock_fifo=False) therefore reads back with the key
            # ABSENT -- compare truthiness so absent == False is a pure read (not a needless re-write
            # every launch), while a mis-seeded genlock_fifo=True still differs from False and heals.
            if bool(ev) != bool(v):
                return True
            continue
        if ev != v:
            return True
    return False


def input_classes_summary(inputs):
    """Pure (issue 1317): a report line body describing each input's class for verify-strih.sh --
    "N camera, M feedback (name=class, ...)". Report-only; the whole-line verdict `strih ndi inputs:
    OK` that verify-strih.sh greps with grep -qxF is emitted SEPARATELY and unchanged. Entries are
    normalized via _entry_fields (a bare string OR an object {sender,input,scene}, this lane) -- it
    must NOT iterate raw entries (a dict is unhashable, which crashed verify_parity on the OPERATOR
    collection and killed its drift check). De-duplicated by INPUT name; class resolved from the
    SENDER *or* the input name (either carrying the 2ME marker -> feedback), matching seed_inputs. The
    per-input label keeps the SENDER name (the bare-string report shape)."""
    seen = set()
    pairs = []
    ncam = nfb = 0
    for entry in inputs:
        fields = _entry_fields(entry)
        if fields is None:
            continue
        sender, input_name, _scene = fields
        if input_name in seen:
            continue
        seen.add(input_name)
        cls = "camera"
        if input_class_for(sender) == "feedback" or input_class_for(input_name) == "feedback":
            cls = "feedback"
        pairs.append("%s=%s" % (sender, cls))
        if cls == "feedback":
            nfb += 1
        else:
            ncam += 1
    return "%d camera, %d feedback (%s)" % (ncam, nfb, ", ".join(pairs))


# --- issue 1242: the BANDWIDTH ROLES (pure helpers -> Tier-0 testable) -----------------------------
# Owner ruling 24.9.2026: strih pulls FULL bandwidth only for the cameras that are SHOWN -- preview,
# program, a projector, the visible item of the Grading NDI-output scene. Two receiver roles per camera:
#   * PROGRAM-PATH main (`NDI camN`, genlock_connect_on_show=True): the vendored DistroAV receiver PARKS
#     it (releases the NDI receiver) while nothing shows it and reconnects on show. A cold start in
#     preview is accepted by the owner.
#   * MONITOR twin (`MV NDI camN`, genlock_monitor=True): the #501 low-bandwidth receiver of the SAME
#     sender, ALWAYS connected, feeding the built-in multiview via an `MV <scene>` twin scene.
# The built-in multiview renders the twin scenes (show_in_multiview), never a scene holding a full
# program-path input -- otherwise the multiview itself would keep every camera shown (= connected).
# This reverses, for the strih role, the issue-761 same-source multiview and the issue-764 keep-alive
# of every genlocked input. Scoped by ROLE, never by platform: only the fleet camera senders become
# program-path (the cg inputs stay always-connected -- a CG cut-in must be instant; the 2ME feedback
# inputs are not genlocked); stream / resolume never run this lib.
CONNECT_ON_SHOW_KEY = "genlock_connect_on_show"
GENLOCK_MONITOR_KEY = "genlock_monitor"
TWIN_PREFIX = "MV "
# A fleet camera sender (`CAM<n> (usb)`, `CAM<n> (30p)`, ...) -- the only program-path senders.
PROGRAM_PATH_SENDER_RE = re.compile(r"^CAM\d+ \(")
# Audio-only inputs carry no picture; a multiview twin scene never needs them (the multiview only
# SHOWS a scene -- its audio is never mixed), so they are left out of a twin.
AUDIO_ONLY_INPUT_KINDS = frozenset({
    "pulse_input_capture", "pulse_output_capture", "alsa_input_capture", "jack_input_client",
    "asio_input_capture", "wasapi_input_capture", "wasapi_output_capture",
    "wasapi_process_output_capture", "coreaudio_input_capture", "coreaudio_output_capture",
})
# obs-websocket 5 SetSceneItemTransform accepts only these (GetSceneItemList also returns read-only
# computed width/height/sourceWidth/sourceHeight -- the strih_mv_scenes.py convention).
SETTABLE_TRANSFORM_FIELDS = frozenset({
    "positionX", "positionY", "rotation", "scaleX", "scaleY", "alignment",
    "boundsType", "boundsAlignment", "boundsWidth", "boundsHeight",
    "cropLeft", "cropTop", "cropRight", "cropBottom",
})
# The built-in multiview only re-reads scene membership on a scene-list change (OBS
# UpdateMultiviewProjectors on add/remove/rename); a create+remove of this scratch scene triggers it.
MULTIVIEW_REFRESH_SCENE = "__camera-box multiview refresh (issue 1242)"


def is_program_path_sender(sender):
    """True for a fleet camera sender (`CAM<n> (...)`) -- the only program-path (connect-on-show)
    inputs. The cg sender, the 2ME feedback pair and anything else stay always connected."""
    return bool(PROGRAM_PATH_SENDER_RE.match(sender or ""))


def program_path_inputs(plan):
    """The seed plan's program-path INPUT names (plan order): genlocked camera-class inputs whose
    sender is a fleet camera."""
    return [item["input"] for item in plan
            if item["settings"].get("genlock_fifo") and is_program_path_sender(item["ndi_source_name"])]


def twin_name(name):
    """'Cam 3' -> 'MV Cam 3', 'NDI cam3' -> 'MV NDI cam3' (the strih_mv_scenes.py convention)."""
    return TWIN_PREFIX + name


def main_role_settings():
    """The program-path role for a main camera input (fresh dict each call)."""
    return {CONNECT_ON_SHOW_KEY: True}


def twin_input_settings(main_effective):
    """The monitor-twin settings: the SAME live sender + pin as its main, genlocked, flagged
    genlock_monitor (the forcer narrows it to LOWEST bandwidth and it is never parked),
    connect-on-show explicitly OFF, NDI audio off (a camera twin never feeds the mixer)."""
    main_effective = main_effective or {}
    return {
        "ndi_source_name": main_effective.get("ndi_source_name", ""),
        "genlock_fifo": True,
        "ndi_sync": 2,
        "genlock_latency_ms_src": main_effective.get("genlock_latency_ms_src", DEFAULT_CAMERA_LATENCY_MS),
        GENLOCK_MONITOR_KEY: True,
        CONNECT_ON_SHOW_KEY: False,
        "ndi_audio": False,
    }


def _is_twin(name):
    return (name or "").startswith(TWIN_PREFIX)


def scene_needs_twin(scene_name, items, program_inputs, is_output_scene):
    """True iff `scene_name` must be replaced in the built-in multiview by an `MV` twin: it holds at
    least one program-path input DIRECTLY, it is not itself a twin, and it is not an NDI-output scene
    (an output scene -- Grading, Interkom -- must stay SHOWN as-is so its output is valid; Grading's
    visible nested camera is exactly the one full-bandwidth grading feed the owner wants)."""
    if is_output_scene or _is_twin(scene_name):
        return False
    prog = set(program_inputs)
    return any(i.get("sourceName") in prog for i in items or [])


def _settable_transform(transform):
    return {k: v for k, v in (transform or {}).items() if k in SETTABLE_TRANSFORM_FIELDS}


def twin_scene_items(items, program_inputs):
    """The desired twin-scene items, in the original stacking order: every program-path input is
    swapped for its monitor twin, audio-only inputs are dropped, every other item is reused as-is;
    enabled state and the settable transform are preserved."""
    prog = set(program_inputs)
    out = []
    for i in items or []:
        if i.get("inputKind") in AUDIO_ONLY_INPUT_KINDS:
            continue
        name = i.get("sourceName")
        out.append({
            "sourceName": twin_name(name) if name in prog else name,
            "sceneItemEnabled": bool(i.get("sceneItemEnabled", True)),
            "sceneItemTransform": _settable_transform(i.get("sceneItemTransform")),
        })
    return out


def twin_items_match(current_items, desired):
    """True iff the twin scene already carries exactly `desired` (source + enabled, in order)."""
    cur = [(i.get("sourceName"), bool(i.get("sceneItemEnabled", True))) for i in current_items or []]
    want = [(d["sourceName"], d["sceneItemEnabled"]) for d in desired]
    return cur == want


def is_custom_multiview_scene(name):
    """The operator's hand-built multiview GRID scene ('MULTIVIEW' on strih-lx, 'Multiview' on the
    old Windows strih) -- matched case-insensitively."""
    return (name or "").strip().lower() == "multiview"


def multiview_swap_plan(items, program_inputs, twinned_scenes):
    """For the custom multiview grid scene: every item that renders a program-path input directly, or
    a scene that got an `MV` twin, is swapped for its twin (same spot, same transform, same enabled
    state), so the grid never keeps a full input shown. `twinned_scenes` = {scene: twin scene}."""
    prog = set(program_inputs)
    plan = []
    for i in items or []:
        name = i.get("sourceName")
        new = twin_name(name) if name in prog else twinned_scenes.get(name)
        if not new:
            continue
        plan.append({
            "old_item_id": i["sceneItemId"],
            "new_name": new,
            "enabled": bool(i.get("sceneItemEnabled", True)),
            "transform": _settable_transform(i.get("sceneItemTransform")),
        })
    return plan


def bandwidth_role_problems(actual, program_inputs):
    """Report-only role check: `actual` = {inputName: settings}. A program-path main must carry
    connect-on-show, and its `MV` twin must exist as a monitor input. Returns problem strings."""
    problems = []
    for inp in program_inputs:
        if not (actual.get(inp) or {}).get(CONNECT_ON_SHOW_KEY):
            problems.append("%r not connect-on-show" % inp)
        tw = twin_name(inp)
        if tw not in actual:
            problems.append("%r twin MISSING" % tw)
        elif not (actual.get(tw) or {}).get(GENLOCK_MONITOR_KEY):
            problems.append("%r twin not genlock_monitor" % tw)
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


def _effective_input_settings(obs, input_name):
    """The input's EFFECTIVE settings = its ndi_source type DEFAULTS overlaid with the explicitly-saved
    settings (the obs_phase2._effective_input_settings shape, reusing THIS file's own Obs.req -- no
    second WS client). GetInputSettings returns only explicitly-persisted (non-default) keys, so
    merging GetInputDefaultSettings underneath lets settings_update_needed compare like-for-like (a
    key left at its DistroAV default -- e.g. ndi_sync=2 -- is not falsely seen as 'missing')."""
    explicit = (obs.req("GetInputSettings", {"inputName": input_name}, ignore_err=True)
                or {}).get("inputSettings", {}) or {}
    defaults = (obs.req("GetInputDefaultSettings", {"inputKind": "ndi_source"}, ignore_err=True)
                or {}).get("defaultInputSettings", {}) or {}
    return {**defaults, **explicit}


def _current_scene_name(obs):
    """The current program scene name (CreateInput needs a scene to attach the input's scene item),
    or the first scene, or None when no scene exists."""
    sl = obs.req("GetSceneList", ignore_err=True) or {}
    name = sl.get("currentProgramSceneName")
    if name:
        return name
    scenes = sl.get("scenes") or []
    return scenes[0].get("sceneName") if scenes else None


def seed_program_audio_input(obs):
    """The ONE allowed create/replace (issue 1344): ensure the `ASIO zvuk` program-audio input exists
    as pulse_input_capture on strih-program-source (the OBS program capture). A missing input is
    CREATED; the un-creatable Windows asio_input_capture (or any other kind) is REMOVED + recreated;
    a correct pulse_input_capture is left alone. Returns a short status string for the log."""
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
    exists = any(i.get("inputName") == AUDIO_INPUT_NAME for i in inputs)
    kind = program_audio_input_kind_from_inputs(inputs)
    action = audio_input_action(exists, kind)
    if action == "ok":
        return "matched"
    scene = _current_scene_name(obs)
    if scene is None:
        return "no-scene (deferred)"
    if action == "replace":
        obs.req("RemoveInput", {"inputName": AUDIO_INPUT_NAME}, ignore_err=True)
    obs.req("CreateInput", {
        "sceneName": scene,
        "inputName": AUDIO_INPUT_NAME,
        "inputKind": AUDIO_INPUT_KIND,
        "inputSettings": program_audio_input_settings(),
    }, ignore_err=True)
    return "created" if action == "create" else "replaced (was %s)" % (kind or "absent")


def ensure_program_audio_in_every_scene(obs, plan):
    """issue 1344 item 3 (20.9.2026 live diagnosis): the `ASIO zvuk` program-audio input must be a
    scene item in EVERY declared operator scene, not just the one it happened to be created in --
    OBS only plays a scene item's audio while its OWNING scene is active, so seeding it into a single
    scene silences program audio on every camera cut. The original migrated collection carried it in
    all 7 program scenes (Cam 1/2/3/5/6/7 + Moderatori). Adds a CreateSceneItem for AUDIO_INPUT_NAME
    to any scene named in `plan` that does not already carry it (scene names de-duplicated). Returns
    the number of scenes it added to (0 when every scene already has it) -- best-effort
    (ignore_err=True): a scene that no longer exists or the source not yet created must not abort the
    whole seed."""
    scenes = sorted({item["scene"] for item in plan})
    added = 0
    for scene in scenes:
        items = (obs.req("GetSceneItemList", {"sceneName": scene}, ignore_err=True) or {}).get(
            "sceneItems", [])
        if any(i.get("sourceName") == AUDIO_INPUT_NAME for i in items):
            continue
        obs.req("CreateSceneItem", {
            "sceneName": scene, "sourceName": AUDIO_INPUT_NAME,
        }, ignore_err=True)
        added += 1
    return added


# --- issue 1242: applying the BANDWIDTH ROLES over WS (the pure planners are above) -----------------

def _role_update_needed(effective, desired):
    """True iff `desired` differs from `effective`. A bool is compared by truthiness: obs-websocket
    omits a bool key that equals its (possibly unregistered) default, so absent == False must be a
    pure read, never a re-write every launch (the genlock_fifo lesson in settings_update_needed)."""
    for k, v in desired.items():
        ev = (effective or {}).get(k)
        if isinstance(v, bool):
            if bool(ev) != v:
                return True
        elif ev != v:
            return True
    return False


def _multiview_shown(obs, scene):
    """The scene's EFFECTIVE built-in-multiview membership (OBS defaults an absent key to true)."""
    s = (obs.req("GetSourcePrivateSettings", {"sourceName": scene}, ignore_err=True)
         or {}).get("sourceSettings") or {}
    v = s.get("show_in_multiview")
    return True if v is None else bool(v)


def _set_multiview_shown(obs, scene, show):
    """Set the scene's built-in-multiview membership only on drift. Returns True iff it wrote."""
    if _multiview_shown(obs, scene) == show:
        return False
    obs.req("SetSourcePrivateSettings", {
        "sourceName": scene, "sourceSettings": {"show_in_multiview": show},
    }, ignore_err=True)
    return True


def _is_ndi_output_scene(obs, scene):
    """True iff the scene publishes itself over NDI (an ENABLED DistroAV ndi_filter) -- such a scene
    (Grading, Interkom) must stay SHOWN as-is: the filter only sends while its parent is showing."""
    fl = (obs.req("GetSourceFilterList", {"sourceName": scene}, ignore_err=True) or {}).get("filters") or []
    return any(f.get("filterKind") == "ndi_filter" and f.get("filterEnabled") for f in fl)


def _scene_items(obs, scene):
    return (obs.req("GetSceneItemList", {"sceneName": scene}, ignore_err=True) or {}).get("sceneItems") or []


def _add_scene_item(obs, scene, source, enabled, transform, existing_inputs, twin_settings):
    """Add `source` to `scene`. A monitor twin input that does not exist yet is CREATED here (inside
    this scene) with its role settings and muted; anything else is a plain CreateSceneItem."""
    if source not in existing_inputs and source in twin_settings:
        res = obs.req("CreateInput", {
            "sceneName": scene, "inputName": source, "inputKind": "ndi_source",
            "inputSettings": twin_settings[source], "sceneItemEnabled": enabled,
        }, ignore_err=True) or {}
        existing_inputs.add(source)
        obs.req("SetInputMute", {"inputName": source, "inputMuted": True}, ignore_err=True)
    else:
        res = obs.req("CreateSceneItem", {
            "sceneName": scene, "sourceName": source, "sceneItemEnabled": enabled,
        }, ignore_err=True) or {}
    item_id = res.get("sceneItemId")
    if item_id is not None and transform:
        obs.req("SetSceneItemTransform", {
            "sceneName": scene, "sceneItemId": item_id, "sceneItemTransform": transform,
        }, ignore_err=True)
    return item_id


def apply_bandwidth_roles(obs, plan):
    """Apply the issue-1242 bandwidth roles to the live collection (idempotent; a correct collection
    is a pure read). Steps:
      1. every program-path main (`NDI camN`) gets genlock_connect_on_show=True;
      2. every existing `MV` twin input is healed to its role settings (same live sender + pin as its
         main, genlock_monitor=True, connect-on-show off); a missing twin is created in step 3;
      3. every multiview scene that holds a program-path input directly (not an NDI-output scene, not
         the custom grid, not a twin) gets an `MV <scene>` twin whose items mirror it with each main
         swapped for its twin; the original leaves the built-in multiview, the twin joins it;
      4. the custom multiview GRID scene (`MULTIVIEW`) renders twins instead of full inputs;
      5. when membership changed, the built-in multiview is refreshed (a scratch scene create+remove --
         OBS re-reads membership only on a scene-list change).
    ROLE-OWNED creates only (the twin inputs/scenes + the scratch refresh scene); the operator's own
    inputs/scenes are never created, renamed or removed. Returns a summary dict for the log."""
    summary = {"mains": [], "twins": [], "twin_scenes": [], "multiview_grid": [], "refreshed": False}
    inputs = {i.get("inputName"): i.get("inputKind")
              for i in (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])}
    prog = [p for p in program_path_inputs(plan) if p in inputs]
    if not prog:
        return summary
    existing = set(inputs)

    main_eff = {}
    for m in prog:
        main_eff[m] = _effective_input_settings(obs, m)
        if _role_update_needed(main_eff[m], main_role_settings()):
            obs.req("SetInputSettings", {
                "inputName": m, "inputSettings": main_role_settings(), "overlay": True,
            }, ignore_err=True)
            summary["mains"].append(m)

    twin_settings = {twin_name(m): twin_input_settings(main_eff[m]) for m in prog}
    for tw, st in twin_settings.items():
        if tw in existing and _role_update_needed(_effective_input_settings(obs, tw), st):
            obs.req("SetInputSettings", {"inputName": tw, "inputSettings": st, "overlay": True},
                    ignore_err=True)
            summary["twins"].append(tw)

    scenes = [s.get("sceneName") for s in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", [])]
    twinned = {}
    membership_changed = False
    for sc in scenes:
        if is_custom_multiview_scene(sc):
            continue
        items = _scene_items(obs, sc)
        if not scene_needs_twin(sc, items, prog, _is_ndi_output_scene(obs, sc)):
            continue
        tw_sc = twin_name(sc)
        tw_exists = tw_sc in scenes
        # Only a scene the operator shows in the multiview (or one already twinned) gets a twin.
        if not tw_exists and not _multiview_shown(obs, sc):
            continue
        twinned[sc] = tw_sc
        desired = twin_scene_items(items, prog)
        if not tw_exists:
            obs.req("CreateScene", {"sceneName": tw_sc}, ignore_err=True)
            membership_changed = True
        current = _scene_items(obs, tw_sc) if tw_exists else []
        if not twin_items_match(current, desired):
            for it in current:
                obs.req("RemoveSceneItem", {"sceneName": tw_sc, "sceneItemId": it["sceneItemId"]},
                        ignore_err=True)
            for d in desired:
                _add_scene_item(obs, tw_sc, d["sourceName"], d["sceneItemEnabled"],
                                d["sceneItemTransform"], existing, twin_settings)
            summary["twin_scenes"].append(tw_sc)
        membership_changed |= _set_multiview_shown(obs, sc, False)
        membership_changed |= _set_multiview_shown(obs, tw_sc, True)

    for sc in scenes:
        if not is_custom_multiview_scene(sc):
            continue
        for e in multiview_swap_plan(_scene_items(obs, sc), prog, twinned):
            # add the twin BEFORE removing the full input (the grid never drops a tile mid-swap)
            _add_scene_item(obs, sc, e["new_name"], e["enabled"], e["transform"], existing, twin_settings)
            obs.req("RemoveSceneItem", {"sceneName": sc, "sceneItemId": e["old_item_id"]}, ignore_err=True)
            summary["multiview_grid"].append(e["new_name"])

    if membership_changed:
        obs.req("CreateScene", {"sceneName": MULTIVIEW_REFRESH_SCENE}, ignore_err=True)
        obs.req("RemoveScene", {"sceneName": MULTIVIEW_REFRESH_SCENE}, ignore_err=True)
        summary["refreshed"] = True
    return summary


def bootstrap(obs, plan, studio=True, update_only=False):
    """Seed the collection from `plan` (seed_inputs output). Two modes:

    CREATE (default, the parallel/imag shape): idempotent CreateScene/CreateInput (ignore "already
    exists"), then a CONDITIONAL class UPDATE -- an ALREADY-EXISTING input whose effective settings
    differ from its class is healed (SetInputSettings overlay:True; a mis-seeded genlock_fifo=True on a
    2ME feedback input is cleared), re-enforcing the ndi_source_name ONLY after a real change (the
    #795-safe #1158 shape); a matching input is a pure read.

    UPDATE-ONLY (issue 1317, this lane -- the OPERATOR collection is authoritative): NEVER
    CreateScene/CreateInput. A declared input that does not exist on the box is REPORTED (never
    created). An existing input is healed to the certified genlock CLASS ONLY (the class keys, NO
    ndi_source_name -- the operator's source binding is authoritative, so the seeder never renames it),
    conditionally on drift. This is the fix for the live duplicate-receiver defect: the launch seed no
    longer creates `NDI CAMn (usb)` duplicates alongside the operator's `NDI camN` inputs.

    Then SetStudioModeEnabled. Returns {input: status} for the log."""
    op = _obs_phase2_module()
    result = {}
    existing = None
    if update_only:
        existing = {i.get("inputName")
                    for i in (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
                    if i.get("inputKind") == "ndi_source"}
    for item in plan:
        scene = item["scene"]
        inp = item["input"]
        src = item["ndi_source_name"]
        if update_only:
            if inp not in existing:
                # the operator collection is authoritative; a missing declared input is REPORTED, never
                # created (creating it would be a wrong-named duplicate of whatever the operator built).
                result[inp] = "missing (declared, not created)"
                continue
            # class keys ONLY -- never ndi_source_name (never rename the operator's source).
            desired_class = dict(item["settings"])
            effective = _effective_input_settings(obs, inp)
            if settings_update_needed(effective, desired_class):
                obs.req("SetInputSettings", {
                    "inputName": inp, "inputSettings": desired_class, "overlay": True,
                }, ignore_err=True)
                result[inp] = "healed-class"
            else:
                result[inp] = "matched"
            continue
        desired = dict(item["settings"], ndi_source_name=src)
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        # CreateInput seeds a NEW input with the class settings; on an existing input it fails
        # "already exists" -> ignored (its current settings are read below and healed only on drift).
        obs.req("CreateInput", {
            "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
            "inputSettings": desired,
        }, ignore_err=True)
        effective = _effective_input_settings(obs, inp)
        if settings_update_needed(effective, desired):
            # overlay:True merges the class settings, leaving unrelated per-source keys (e.g.
            # genlock_latency_ms_src) untouched; genlock_fifo=False in the feedback class CLEARS a
            # stale True. The ndi_source_name rides `desired`, and is then read-back-verified.
            obs.req("SetInputSettings", {
                "inputName": inp, "inputSettings": desired, "overlay": True,
            }, ignore_err=True)
            result[inp] = _enforce_ndi_source_name(obs, op, inp, src)
        else:
            result[inp] = "matched"
    # issue 1344: the ONE allowed create/replace — the `ASIO zvuk` program-audio input on the
    # strih-program-source loopback node (the OBS program capture that Failed to create on Linux).
    audio_status = seed_program_audio_input(obs)
    # issue 1344 item 3: once the input exists, make sure it is a scene item in EVERY declared
    # scene (never only the one it happened to be created/matched in) -- skip when the input itself
    # could not be resolved (no scene existed at all: "no-scene (deferred)"), nothing to attach to.
    if not audio_status.startswith("no-scene"):
        added = ensure_program_audio_in_every_scene(obs, plan)
        if added:
            audio_status += " (+%d scene items)" % added
    result[AUDIO_INPUT_NAME] = audio_status
    if studio:
        obs.req("SetStudioModeEnabled", {"studioModeEnabled": True}, ignore_err=True)
    return result


def verify_parity(obs, manifest_text):
    """Read-only: print one whole-line verdict "strih ndi inputs: OK" (or the problem list) that
    verify-strih.sh greps with grep -qxF. Exit 1 on any problem, 0 when clean (the imag verify_parity
    exit contract). Never seeds/creates anything."""
    inputs, _outputs, latency = parse_seed_manifest(manifest_text)
    mode = parse_seed_mode(manifest_text)
    plan = seed_inputs(inputs, latency)
    actual = {}
    for inp in obs.req("GetInputList").get("inputs", []):
        if inp.get("inputKind") != "ndi_source":
            continue
        name = inp["inputName"]
        s = obs.req("GetInputSettings", {"inputName": name}, ignore_err=True)
        actual[name] = s.get("inputSettings", {})
    # update-only (the OPERATOR collection) checks the declared inputs + genlock class, NOT the
    # ndi_source_name (the operator's real senders are authoritative, not the manifest DATA guess).
    problems = input_parity_problems(actual, plan, check_source=(mode != "update-only"))
    # issue 1317: report the per-input CLASS on its OWN line (report-only; verify-strih.sh notes it).
    # The verdict line below stays byte-identical ("strih ndi inputs: OK") for the grep -qxF anchor.
    print("strih ndi input classes: " + input_classes_summary(inputs))
    # issue 1242: the bandwidth-role state on its OWN report-only line (never changes the exit code --
    # a box launched before the role apply, or on an older DistroAV, is reported, not failed).
    roles = bandwidth_role_problems(actual, program_path_inputs(plan))
    print("strih bandwidth roles: " + ("; ".join(roles) if roles else "OK"))
    print("strih ndi inputs: " + ("; ".join(problems) if problems else "OK"))
    if problems:
        sys.exit(1)


def _userini_scene_collection_base(cfg_dir):
    """The authoritative CURRENT scene-collection base filename from user.ini `[Basic]
    SceneCollectionFile` -- OBS records the exact on-disk base there, robust against display-name
    slugification (a name with spaces/punctuation maps to a different filename). Returns the base
    (no .json) or None when user.ini is absent/unreadable/has no such key."""
    ini = os.path.join(cfg_dir, "user.ini")
    try:
        with open(ini) as fh:
            in_basic = False
            for line in fh:
                s = line.strip()
                if s.startswith("[") and s.endswith("]"):
                    in_basic = (s == "[Basic]")
                    continue
                if in_basic and s.startswith("SceneCollectionFile="):
                    return s.split("=", 1)[1].strip() or None
    except OSError:
        return None
    return None


def _read_saved_projectors_file(path):
    """The `saved_projectors` list from a scene collection JSON file, or [] when it is missing/
    unreadable/non-JSON or has no list (never raises)."""
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError):
        return []
    sp = d.get("saved_projectors")
    return sp if isinstance(sp, list) else []


def _current_collection_saved_projectors(obs, cfg_dir):
    """Return the CURRENT scene collection's saved_projectors list (from the on-disk collection JSON
    OBS persists with SaveProjectors=true), or [] when nothing is saved yet (first boot -> seed opens
    the projector). Resolution order: the authoritative user.ini `[Basic] SceneCollectionFile` base,
    then the GetSceneCollectionList name (the design's approach) -> `<cfg>/basic/scenes/<x>.json`.
    ONLY when NEITHER resolves to an existing file do we glob EVERY collection file -- a last-resort
    safety net against OBS slugifying the collection name to a different filename, so a saved projector
    we cannot locate by name never causes a DUPLICATE window (imag #756 class). The named-file case
    never over-suppresses (it reads that one collection only)."""
    scenes_dir = os.path.join(cfg_dir, "basic", "scenes")
    candidates = []
    base = _userini_scene_collection_base(cfg_dir)
    if base:
        candidates.append(os.path.join(scenes_dir, base + ".json"))
    try:
        name = ((obs.req("GetSceneCollectionList", ignore_err=True) or {})
                .get("currentSceneCollectionName") or "")
    except Exception as e:  # noqa: BLE001 -- a WS hiccup -> fall through to the file candidates; log it
        print("projector: GetSceneCollectionList read failed (%s) -- using on-disk collection files" % e)
        name = ""
    if name:
        p = os.path.join(scenes_dir, name + ".json")
        if p not in candidates:
            candidates.append(p)
    if not any(os.path.exists(p) for p in candidates):
        candidates = sorted(glob.glob(os.path.join(scenes_dir, "*.json")))
    for path in candidates:
        sp = _read_saved_projectors_file(path)
        if sp:
            return sp
    return []


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
    ap.add_argument("--audio-input-kind", action="store_true",
                    help="issue 1344: print the OBS inputKind of the `ASIO zvuk` program-audio input "
                         "(or `absent`) — read-only, for verify-strih's derived audio verdict")
    ap.add_argument("--apply-roles", action="store_true",
                    help="issue 1242: apply the bandwidth roles -- program-path cameras connect only "
                         "while shown (genlock_connect_on_show), the built-in multiview renders the "
                         "always-connected low-bandwidth `MV` twins (idempotent; strih-obs-start.sh "
                         "runs it on every launch after --bootstrap)")
    ap.add_argument("--projector", choices=["program", "multiview"], default=None,
                    help="issue 1346: rewrite strih-lx-projector.json to program|multiview and "
                         "(re)seed the fixed HDMI fullscreen projector (the OBS UI projector menu "
                         "stays the primary operator switch; SaveProjectors persists it)")
    args = ap.parse_args()

    # An explicit mode is REQUIRED (issue 1317 review): a bare invocation must never silently connect
    # and MUTATE OBS. --bootstrap seeds; --verify-parity is read-only; --projector sets the HDMI
    # projector (issue 1346).
    if (not args.bootstrap and not args.verify_parity and args.projector is None
            and not args.audio_input_kind and not args.apply_roles):
        ap.error("specify a mode: --bootstrap (seed), --verify-parity (read-only), "
                 "--audio-input-kind (read-only), --apply-roles, or --projector program|multiview")

    # --audio-input-kind (issue 1344): read-only print of the `ASIO zvuk` input kind. Does NOT read
    # the seed manifest.
    if args.audio_input_kind:
        obs = Obs(args.host, args.port, args.password)
        inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
        print(program_audio_input_kind_from_inputs(inputs) or "absent")
        return

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
    if args.apply_roles:
        # issue 1242: the bandwidth roles (a SEPARATE mode from --bootstrap so the seed's update-only
        # never-create contract stays exactly as issue 1317 pinned it; the role lib owns its twins).
        inputs, _outputs, latency = parse_seed_manifest(manifest_text)
        summary = apply_bandwidth_roles(obs, seed_inputs(inputs, latency))
        print("strih bandwidth roles applied (issue 1242): " + ", ".join(
            "%s=%s" % (k, summary[k]) for k in sorted(summary)))
        return
    # --bootstrap: seed the collection. `mode` (issue 1317) selects update-only (the OPERATOR
    # collection: heal the certified class onto EXISTING inputs, never CreateScene/CreateInput) vs
    # create (the parallel/imag shape).
    inputs, _outputs, latency = parse_seed_manifest(manifest_text)
    mode = parse_seed_mode(manifest_text)
    plan = seed_inputs(inputs, latency)
    statuses = bootstrap(obs, plan, studio=True, update_only=(mode == "update-only"))
    print("strih seed (%s): %d inputs, latency %d ms; ndi names: %s"
          % (mode, len(plan), latency,
             ", ".join("%s=%s" % (k, v) for k, v in sorted(statuses.items()))))
    print("scene order: " + " | ".join(scene_order(inputs)))
    # issue 1346: seed the fixed HDMI fullscreen projector AFTER the input seed (idempotent; SKIPs
    # cleanly when no external monitor is connected).
    seed_projector(obs)


if __name__ == "__main__":
    main()
