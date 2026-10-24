#!/usr/bin/env python3
"""strih_scenes.py -- strih-snv OBS input/scene/Studio-Mode seeder (issue 1317).

The strih-snv notebook's OBS boots EMPTY (no scenes, no inputs) -- it cannot receive or switch any
of the fleet's NDI sources until something seeds it. imag has scripts/imag_scenes.py, seeded on
every launch by imag-obs-start.sh; this is the focused strih-snv sibling (design comment on issue
1317, Prístup 1). It reuses the on-box obs_phase2.py certified-genlock primitives rather than
importing imag's 70KB DRM-lease/encoder/picom machinery that strih-snv does not need.

SCOPE: the 10 NDI INPUTS + one per-input scene + Studio Mode, from /opt/camera-box/strih-seed.json
(setup-strih.sh step 6). The 5 STRIH-SNV NDI OUTPUTS are a SEPARATE ticket -- this seeder never
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
  --audio-monitor-type   issue 1436, read-only: print the OBS monitoring type of the program-audio input
                         (`OBS_MONITORING_TYPE_NONE` ...), `absent`, or `unreachable: <why>` with rc 2
                         when the OBS websocket does not answer (verify-strih's stream-monitor row).
  --pulse-capture-inputs issue 1465, read-only: list every pulse capture input (`pulse capture inputs:
                         none`), rc 2 when OBS does not answer (verify-strih's operator-audio row).
  --browser-audio        issue 1440, read-only: `browser audio: OK (...)` when every browser source reroutes
                         its audio into OBS and is muted, else the problems (rc 1); rc 2 when OBS does not
                         answer (verify-strih's browser-audio row).
  --projector T         issue 1346: set the HDMI output VIEW (program|multiview) in
                         ~/.camera-box/drm-output.json -- the in-OBS DRM-lease output reads it at the
                         next OBS start. The operator's live switch is in OBS (Tools menu); this is
                         the scripted twin. Never opens a projector window and never talks to OBS.

The pure helpers (parse_seed_manifest / seed_inputs / certified_genlock_settings / scene_order /
input_parity_problems, and for issue 1346 drm_output_view_of / drm_output_lease_connector /
drm_output_view_token / read_drm_view / write_drm_view) carry NO WebSocket dependency, so they are
Tier-0 testable with no rig (tests/python/test_strih_scenes_1317.py +
tests/python/test_strih_drm_output_1346.py).

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

SEED_MANIFEST_PATH = "/opt/camera-box/strih-seed.json"
# The genlock latency floor every camera input rides (strih_lx_camera_latency_ms; the rig floor).
# obs_phase2._PROBE_NDI_SETTINGS uses latency 0 (the probe default); strih-snv rides the manifest's
# floor 3 -- so the certified settings here override ONLY latency, keeping the #63/#149 genlock keys.
DEFAULT_CAMERA_LATENCY_MS = 3
# issue 1317: the 2ME PGM/PVW FEEDBACK inputs are NOT genlocked. They are the strih's own post-render
# 30 fps program/preview outputs received back as monitoring feedback; a post-render output is off the
# camera boundary grid and every program CUT is a timecode discontinuity, so a genlock FIFO underruns
# and relocks on every cut (110,222 underruns / 899 relocks measured live on strih-snv 19.9.). The
# Windows strih receives these NON-genlocked (light.json NDI 2ME PGM/PVW: ndi_sync=1, latency=1, no
# genlock_fifo); the feedback class mirrors that. `latency` here is the stock DistroAV receive-buffer
# MODE enum (1 = LOW), NOT milliseconds -- FIXED at 1 (a feedback monitor, not a camera on the
# aligned grid), independent of the camera manifest floor (which rides genlock_latency_ms_src).
FEEDBACK_LATENCY_MODE = 1

# issue 1424 part 2: the HDMI output config grammar + writers live in drm_output_config.py (installed
# next to this file); every name stays reachable as strih_scenes.<name>.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drm_output_config import (  # noqa: E402,F401
    DRM_OUTPUT_BACKENDS,
    DRM_OUTPUT_CONF,
    DRM_OUTPUT_CONNECTOR_MAX,
    DRM_OUTPUT_DEFAULT_ARGB,
    DRM_OUTPUT_MAX_OUTPUTS,
    DRM_OUTPUT_VIEWS,
    drm_json_loads,
    drm_output_backend_of,
    drm_output_backend_token,
    drm_output_config_text,
    drm_output_instances,
    drm_output_lease_connector,
    drm_output_lease_connectors,
    drm_output_outputs_entry,
    drm_output_spec_of,
    drm_output_view_of,
    drm_output_view_token,
    read_drm_view,
    write_drm_backend,
    write_drm_outputs,
    write_drm_view,
)
# issue 1439: the stream role's program-audio input is the obs-vban receive source (installed next to
# this file); its settings and the keep-the-operator-state re-create live in ONE module the collection
# port shares.
from program_audio_vban import (  # noqa: E402,F401
    VBAN_SOURCE_KIND,
    browser_audio_report,
    drop_pulse_capture_inputs,
    input_create_verified,
    input_recreate,
    input_state_read,
    kind_available,
    listed_text,
    mute_browser_sources,
    parse_browser_audio_rule,
    parse_program_audio_vban,
    parse_pulse_capture_rule,
    pulse_capture_report,
    settings_differences,
)


def certified_genlock_settings(latency):
    """The certified per-input genlock settings (obs_phase2._PROBE_NDI_SETTINGS / _LOCKED_BASELINE_KEYS:
    ndi_bw_mode=0 HIGHEST, genlock_fifo=True, ndi_sync=2 SOURCE_TIMECODE) with the manifest floor on
    `genlock_latency_ms_src` -- the REAL per-source genlock ms knob (issue 235 single knob, floor 3,
    the key latency_pins_verify.py reads). NOT the stock DistroAV `latency` key: that is a
    receive-buffer MODE enum which the genlock build's certified coercion forces back to 0 (NORMAL) on
    every genlock_fifo input, so a seeded `latency: 3` read back 0 forever and made --bootstrap
    re-write + name-re-enforce every camera on every launch (live strih-snv finding, 19.9.2026).
    ndi_source_name is NOT included here -- it is a per-input top-level field the seed merges in.
    Returned fresh each call (never a shared mutable default)."""
    return {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "genlock_latency_ms_src": latency}


def program_settings():
    """issue 1412: the stream box's genlocked strih PROGRAM input (`NDI 2ME PGM`, manifest class
    "program"): genlock_fifo + SOURCE_TIMECODE + full bandwidth like a camera, but WITHOUT
    genlock_latency_ms_src -- the stream A/V delay is the operator's knob, the seeder never writes it.
    Returned fresh each call."""
    return {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2}


# issue 1412: the classes a manifest object entry may name explicitly ("class"); an entry without one
# is classified by its names (input_class_for).
ENTRY_CLASSES = ("camera", "feedback", "program")


def settings_for_class(cls, latency):
    """issue 1412: the seed settings of an input CLASS."""
    if cls == "feedback":
        return feedback_settings()
    if cls == "program":
        return program_settings()
    return certified_genlock_settings(latency)


def entry_class(entry, sender, input_name):
    """issue 1412: the class of ONE manifest entry -- an object's explicit "class" (one of
    ENTRY_CLASSES) wins; otherwise the name rule (either name carrying the 2ME marker -> feedback)."""
    if isinstance(entry, dict) and entry.get("class") in ENTRY_CLASSES:
        return entry["class"]
    if input_class_for(sender) == "feedback" or input_class_for(input_name) == "feedback":
        return "feedback"
    return "camera"


def parse_program_audio_input(text):
    """issue 1412: the OBS program-audio input NAME from the manifest's `program_audio_input` (the
    ported stream collection names it `Zvuk z mixu`), else AUDIO_INPUT_NAME. Lenient, never raises."""
    try:
        d = json.loads(text)
    except ValueError:
        return AUDIO_INPUT_NAME
    name = d.get("program_audio_input") if isinstance(d, dict) else None
    return name if isinstance(name, str) and name else AUDIO_INPUT_NAME


# issue 1436: the stream role's manifest declares `"program_audio_monitor": "off"`. The stream box's
# headphones play the real outgoing stream (strih-stream-monitor.service), so the program-audio input
# must not ALSO be monitored to the jack (stream-pp had `Zvuk z mixu` on Monitor and Output).
AUDIO_MONITOR_NONE = "OBS_MONITORING_TYPE_NONE"


def parse_program_audio_monitor(text):
    """issue 1436: `"off"` when the manifest declares `"program_audio_monitor": "off"`, else None (the
    strih role: the seeder never touches monitoring). Lenient, never raises."""
    try:
        d = json.loads(text)
    except ValueError:
        return None
    v = d.get("program_audio_monitor") if isinstance(d, dict) else None
    return "off" if v == "off" else None


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


def program_audio_input_kind_from_inputs(inputs, name=AUDIO_INPUT_NAME):
    """Pure: the OBS inputKind of the program-audio input NAME (default `ASIO zvuk`) from a GetInputList
    `inputs` list, or None when it is absent. Used by --audio-input-kind + verify-strih's derived audio
    verdict."""
    for i in inputs or []:
        if i.get("inputName") == name:
            return i.get("inputKind")
    return None


def audio_input_action(exists, current_kind, want_kind=AUDIO_INPUT_KIND):
    """Pure: the ONE allowed action for the program-audio input (WANT_KIND: the pulse capture, or on the
    stream role the obs-vban source, issue 1439).
    - absent -> 'create' (CreateInput WANT_KIND)
    - present but asio_input_capture (the un-creatable Windows kind) -> 'replace' (Remove + Create)
    - present as WANT_KIND -> 'ok' (nothing to do)
    - present as some OTHER kind -> 'replace' (heal it to WANT_KIND)."""
    if not exists:
        return "create"
    if current_kind == want_kind:
        return "ok"
    return "replace"


def input_class_for(name):
    """issue 1317: the settings CLASS for an input, keyed by name. Any name CARRYING the 2ME marker
    `2ME PGM` / `2ME PVW` (as a SUBSTRING) is a post-render FEEDBACK monitor -> 'feedback'. This
    matches BOTH the parallel-phase sender form `STRIH-SNV (2ME PGM)` AND the OPERATOR collection's
    explicit input names `NDI 2ME PVW` / `NDI 2ME PGM (mv)` (this lane) -- regardless of the
    STRIH-<box> prefix or the `(mv)` suffix, neither of which ENDS in `(2ME PGM)`. Everything
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
    """Pure: parse /opt/camera-box/strih-seed.json text -> (inputs, outputs, latency).

    inputs/outputs are the non-empty string entries in manifest order; latency is camera_latency_ms
    coerced to int (falling back to DEFAULT_CAMERA_LATENCY_MS on a missing/garbage value). Raises
    ValueError on non-JSON / a non-object top level (a corrupt manifest must fail loud, never seed a
    silently-empty collection)."""
    d = json.loads(text)
    if not isinstance(d, dict):
        raise ValueError("strih-snv seed manifest must be a JSON object, got %r" % type(d).__name__)
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
        cls = entry_class(entry, sender, input_name)
        settings = settings_for_class(cls, latency)
        plan.append({
            "scene": scene,
            "input": input_name,
            "ndi_source_name": sender,
            "settings": settings,
        })
    return plan


def scene_order(inputs):
    """Pure: the stable, deterministic scene order (the scene names in manifest order, de-duplicated
    by INPUT name). strih-snv has no operator-tuned order to preserve; the seed creates scenes in this
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
    ncam = nfb = nprog = 0
    for entry in inputs:
        fields = _entry_fields(entry)
        if fields is None:
            continue
        sender, input_name, _scene = fields
        if input_name in seen:
            continue
        seen.add(input_name)
        cls = entry_class(entry, sender, input_name)
        pairs.append("%s=%s" % (sender, cls))
        if cls == "feedback":
            nfb += 1
        elif cls == "program":
            nprog += 1
        else:
            ncam += 1
    if nprog:
        return "%d camera, %d feedback, %d program (%s)" % (ncam, nfb, nprog, ", ".join(pairs))
    return "%d camera, %d feedback (%s)" % (ncam, nfb, ", ".join(pairs))


def program_retake(host, port, password):
    """issue 1462: re-take the program scene (obs_phase2.retake_program_scene, behind the production-scene
    guard) after a seed removed an item. Never fatal to the boot seed: no obs_phase2, an older one, or a
    failed request prints one line."""
    op = _obs_phase2_module()
    if op is None or not hasattr(op, "retake_program_scene"):
        print("program refresh: skipped (obs_phase2 without retake_program_scene on this host)")
        return
    try:
        ws = op._conn(host, password or "", port)
        try:
            program = op._rpc(ws, "GetCurrentProgramScene", {}).get("currentProgramSceneName")
            msg = op.retake_program_scene(ws, program)
        finally:
            ws.close()
    except Exception as e:  # noqa: BLE001 -- a refresh failure must never kill the launch seed
        print("program refresh: FAILED (%s) -- the program keeps the removed item until the next cut" % e)
        return
    if msg:
        print("program refresh: %s" % msg)


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
    """Minimal OBS-WebSocket 5.x client (sibling of imag_scenes.Obs). no-auth on strih-snv
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


def seed_program_audio_input(obs, name=AUDIO_INPUT_NAME, vban=None):
    """The ONE allowed create/replace (issue 1344): ensure the program-audio input NAME exists as
    pulse_input_capture on strih-program-source (the OBS program capture), or, given VBAN (the obs-vban
    source settings of the stream role's manifest, issue 1439), as the obs-vban receive source. A missing
    input is CREATED; the un-creatable Windows asio_input_capture (or any other kind) is REMOVED +
    recreated keeping the operator's state (every scene and the item's visibility, volume, mute,
    monitoring, tracks, balance, sync offset, filters); an input of the right kind is left alone (its
    settings are the operator's; a difference from the box facts is named in the status). An obs-vban
    kind OBS has not loaded (a bundle without the plugin) never removes anything. Returns a short status
    string for the log."""
    want_kind = VBAN_SOURCE_KIND if vban else AUDIO_INPUT_KIND
    want_settings = dict(vban) if vban else program_audio_input_settings()
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
    exists = any(i.get("inputName") == name for i in inputs)
    kind = program_audio_input_kind_from_inputs(inputs, name)
    action = audio_input_action(exists, kind, want_kind)
    if action == "ok":
        if not vban:
            return "matched"
        current = (obs.req("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings")
        diff = settings_differences(current, want_settings)
        return "matched" if not diff else "matched (the operator's settings differ from the box facts: %s)" % (
            "; ".join(diff))
    if vban and not kind_available(obs, want_kind):
        return "kept %s: OBS has no %s source loaded (deploy the strih bundle with obs-vban, issue 1439)" % (
            kind or "absent", want_kind)
    scene = _current_scene_name(obs)
    if scene is None:
        return "no-scene (deferred)"
    # issue 1439 review: every create is read back from GetInputList (one retry); a create or re-create
    # OBS never lists as the wanted kind is a loud FAILED, never "created" / "replaced".
    if action == "create":
        _resp, listed = input_create_verified(obs, scene, name, want_kind, want_settings)
        if listed != want_kind:
            return "create FAILED: %s after two CreateInput -- restart OBS (the launch seed runs again)" % (
                listed_text(name, listed))
        return "created"
    # issue 1412: a replace keeps the input's place in EVERY scene and its sync offset (a ported
    # collection carries it in scenes the seed plan does not name); issue 1439: and its mixer state.
    state = input_state_read(obs, name)
    ok, info = input_recreate(obs, name, want_kind, want_settings, state, scene)
    if not ok:
        return ("replace FAILED (was %s): %s after RemoveInput + two CreateInput -- restart OBS (the launch "
                "seed runs again)" % (kind or "absent", listed_text(name, info)))
    return "replaced (was %s; kept in %d scene(s))" % (kind or "absent", info)


def audio_monitor_type_of(obs, name=AUDIO_INPUT_NAME):
    """issue 1436: the OBS monitoring type of the input NAME (`OBS_MONITORING_TYPE_*`), `absent` when no
    such input exists, `unreadable` when OBS answers without a type."""
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
    if not any(i.get("inputName") == name for i in inputs or []):
        return "absent"
    cur = (obs.req("GetInputAudioMonitorType", {"inputName": name}, ignore_err=True) or {}).get("monitorType")
    return cur if isinstance(cur, str) and cur else "unreadable"


def ensure_audio_monitor_off(obs, name=AUDIO_INPUT_NAME):
    """issue 1436, update-only: set the existing input NAME to Monitor Off when OBS monitors it, read it
    back. Returns the status for the seed line: `monitor off`, `monitor off (was <type>)`, or a refusal
    named by what OBS still reports."""
    cur = (obs.req("GetInputAudioMonitorType", {"inputName": name}, ignore_err=True) or {}).get("monitorType")
    if not cur:
        return "monitor unreadable"
    if cur == AUDIO_MONITOR_NONE:
        return "monitor off"
    obs.req("SetInputAudioMonitorType", {"inputName": name, "monitorType": AUDIO_MONITOR_NONE}, ignore_err=True)
    back = (obs.req("GetInputAudioMonitorType", {"inputName": name}, ignore_err=True) or {}).get("monitorType")
    if back == AUDIO_MONITOR_NONE:
        return "monitor off (was %s)" % cur
    return "monitor still %s (SetInputAudioMonitorType refused)" % (back or cur)


def ensure_program_audio_in_every_scene(obs, plan, name=AUDIO_INPUT_NAME):
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
        if any(i.get("sourceName") == name for i in items):
            continue
        obs.req("CreateSceneItem", {
            "sceneName": scene, "sourceName": name,
        }, ignore_err=True)
        added += 1
    return added


def bootstrap(obs, plan, studio=True, update_only=False, audio_name=AUDIO_INPUT_NAME, audio_monitor=None,
              audio_vban=None, pulse_capture=None, browser_audio=None):
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

    issue 1436: AUDIO_MONITOR "off" (the stream manifest's `program_audio_monitor`) then sets the
    program-audio input to Monitor Off when OBS monitors it (`ensure_audio_monitor_off`, named in its
    status); None never touches monitoring.

    issue 1439: AUDIO_VBAN (the stream manifest's `program_audio_vban`, obs-vban source settings) makes
    the program-audio input the obs-vban receive source instead of the pulse capture; a STRING is the
    reason the manifest's block could not be read, and the input is then left untouched (never turned
    back into the pulse capture).

    issue 1465 item 3: PULSE_CAPTURE "forbid" (the stream manifest's `pulse_capture`) removes every scene
    pulse capture input except the program-audio input (`drop_pulse_capture_inputs`, status under the key
    "pulse capture"); the stream operator's headphone mix must never reach the program.

    issue 1440: BROWSER_AUDIO "reroute-mute" (the strih manifest's `browser_audio`) sets reroute_audio + mute
    on every browser source (`mute_browser_sources`, status key "browser audio"): no page sound reaches a
    device on a box without a sound server.

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
    if isinstance(audio_vban, str):
        audio_status = "untouched: the manifest's program_audio_vban is unreadable (%s), issue 1439" % audio_vban
    else:
        audio_status = seed_program_audio_input(obs, audio_name, audio_vban)
    # issue 1344 item 3: once the input exists, make sure it is a scene item in EVERY declared
    # scene (never only the one it happened to be created/matched in) -- skip when the input itself
    # could not be resolved (no scene existed at all: "no-scene (deferred)"), nothing to attach to.
    if not audio_status.startswith("no-scene"):
        added = ensure_program_audio_in_every_scene(obs, plan, audio_name)
        if added:
            audio_status += " (+%d scene items)" % added
        if audio_monitor == "off":
            audio_status += "; " + ensure_audio_monitor_off(obs, audio_name)
    result[audio_name] = audio_status
    if pulse_capture == "forbid":
        result["pulse capture"] = drop_pulse_capture_inputs(obs, keep=audio_name)
    if browser_audio == "reroute-mute":
        result["browser audio"] = mute_browser_sources(obs)
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
    print("strih ndi inputs: " + ("; ".join(problems) if problems else "OK"))
    if problems:
        sys.exit(1)


def _read_manifest(path):
    with open(path) as fh:
        return fh.read()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=None)
    ap.add_argument("--manifest", default=SEED_MANIFEST_PATH,
                    help="path to strih-seed.json (default %s)" % SEED_MANIFEST_PATH)
    ap.add_argument("--bootstrap", action="store_true",
                    help="seed scenes/inputs with the certified genlock settings + Studio Mode")
    ap.add_argument("--verify-parity", action="store_true",
                    help="read-only: report whether the 10 seed inputs exist as genlock_fifo sources")
    ap.add_argument("--audio-input-kind", action="store_true",
                    help="issue 1344: print the OBS inputKind of the `ASIO zvuk` program-audio input "
                         "(or `absent`) — read-only, for verify-strih's derived audio verdict")
    ap.add_argument("--audio-monitor-type", action="store_true",
                    help="issue 1436: print the OBS monitoring type of the program-audio input (or "
                         "`absent`; `unreachable: <why>` + rc 2 when OBS does not answer) -- read-only")
    ap.add_argument("--pulse-capture-inputs", action="store_true",
                    help="issue 1465: print every pulse capture input OBS lists (`pulse capture inputs: none` "
                         "when there is none; `unreachable: <why>` + rc 2 when OBS does not answer) -- read-only")
    ap.add_argument("--browser-audio", action="store_true",
                    help="issue 1440: print whether every browser source reroutes its audio into OBS and is "
                         "muted (`unreachable: <why>` + rc 2 when OBS does not answer) -- read-only")
    ap.add_argument("--projector", choices=["program", "multiview"], default=None,
                    help="issue 1346: set the HDMI output view (program|multiview) in "
                         "~/.camera-box/drm-output.json -- the in-OBS DRM-lease output reads it at the "
                         "next OBS start (the live switch is OBS Tools > HDMI vystup)")
    ap.add_argument("--connector", default=None,
                    help="issue 1424 part 2: with --projector, the output whose view to set (required when "
                         "the config drives several outputs, e.g. HDMI-0)")
    args = ap.parse_args()

    # An explicit mode is REQUIRED (issue 1317 review): a bare invocation must never silently connect
    # and MUTATE OBS. --bootstrap seeds; --verify-parity is read-only; --projector sets the HDMI
    # output view (issue 1346).
    if (not args.bootstrap and not args.verify_parity and args.projector is None
            and not args.audio_input_kind and not args.audio_monitor_type and not args.pulse_capture_inputs
            and not args.browser_audio):
        ap.error("specify a mode: --bootstrap (seed), --verify-parity (read-only), "
                 "--audio-input-kind / --pulse-capture-inputs / --browser-audio (read-only), "
                 "or --projector program|multiview")

    # --audio-input-kind (issue 1344): read-only print of the `ASIO zvuk` input kind. Does NOT read
    # the seed manifest.
    if args.audio_input_kind:
        # issue 1412: the input NAME follows the manifest's program_audio_input when it is readable.
        try:
            audio_name = parse_program_audio_input(_read_manifest(args.manifest))
        except OSError:
            audio_name = AUDIO_INPUT_NAME
        obs = Obs(args.host, args.port, args.password)
        inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])
        print(program_audio_input_kind_from_inputs(inputs, audio_name) or "absent")
        return

    # --audio-monitor-type (issue 1436): read-only, for verify-strih's stream-monitor row. An OBS that
    # does not answer is rc 2 (verify reads it as a NOTE: OBS not running), never a monitoring verdict.
    if args.audio_monitor_type:
        try:
            audio_name = parse_program_audio_input(_read_manifest(args.manifest))
        except OSError:
            audio_name = AUDIO_INPUT_NAME
        try:
            obs = Obs(args.host, args.port, args.password)
        except Exception as e:  # noqa: BLE001 -- refused / reset / handshake: any of them = OBS not answering
            print("unreachable: %s: %s" % (type(e).__name__, e))
            sys.exit(2)
        print(audio_monitor_type_of(obs, audio_name))
        return

    # --pulse-capture-inputs (issue 1465): read-only, for verify-strih's (operator-audio-obs) row.
    if args.pulse_capture_inputs:
        try:
            obs = Obs(args.host, args.port, args.password)
        except Exception as e:  # noqa: BLE001 -- refused / reset / handshake: any of them = OBS not answering
            print("unreachable: %s: %s" % (type(e).__name__, e))
            sys.exit(2)
        print(pulse_capture_report(obs))
        return

    # --browser-audio (issue 1440): read-only, for verify-strih's (browser-audio) row.
    if args.browser_audio:
        try:
            obs = Obs(args.host, args.port, args.password)
        except Exception as e:  # noqa: BLE001 -- refused / reset / handshake: any of them = OBS not answering
            print("unreachable: %s: %s" % (type(e).__name__, e))
            sys.exit(2)
        line = browser_audio_report(obs)
        print(line)
        if not line.startswith("browser audio: OK"):
            sys.exit(1)
        return

    # --projector (issue 1346): persist the HDMI output view. Touches only drm-output.json -- never
    # the input seed manifest and never OBS (the running OBS switches live from its own Tools menu).
    if args.projector is not None:
        try:
            write_drm_view(args.projector, DRM_OUTPUT_CONF, args.connector)
        except ValueError as e:
            sys.exit("FAIL issue 1346: %s" % e)
        print("HDMI output view set to %s%s in %s -- takes effect at the next OBS start (switch it live in "
              "OBS Tools > HDMI vystup)" % (args.projector, " on %s" % args.connector if args.connector else "",
                                             DRM_OUTPUT_CONF))
        return

    manifest_text = _read_manifest(args.manifest)

    obs = Obs(args.host, args.port, args.password)
    if args.verify_parity:
        verify_parity(obs, manifest_text)
        return
    # --bootstrap: seed the collection. `mode` (issue 1317) selects update-only (the OPERATOR
    # collection: heal the certified class onto EXISTING inputs, never CreateScene/CreateInput) vs
    # create (the parallel/imag shape).
    inputs, _outputs, latency = parse_seed_manifest(manifest_text)
    mode = parse_seed_mode(manifest_text)
    plan = seed_inputs(inputs, latency)
    try:
        audio_vban = parse_program_audio_vban(manifest_text)   # issue 1439: None on the strih role
    except ValueError as e:
        audio_vban = str(e)
    statuses = bootstrap(obs, plan, studio=True, update_only=(mode == "update-only"),
                         audio_name=parse_program_audio_input(manifest_text),
                         audio_monitor=parse_program_audio_monitor(manifest_text),
                         pulse_capture=parse_pulse_capture_rule(manifest_text),
                         browser_audio=parse_browser_audio_rule(manifest_text),
                         audio_vban=audio_vban)
    print("strih seed (%s): %d inputs, latency %d ms; ndi names: %s"
          % (mode, len(plan), latency,
             ", ".join("%s=%s" % (k, v) for k, v in sorted(statuses.items()))))
    print("scene order: " + " | ".join(scene_order(inputs)))
    # issue 1380: a stream-role manifest carries the YouTube-safe Development scene (stream_dev_safe.py).
    removed = "removed '" in statuses.get("pulse capture", "")
    if '"dev_scene"' in manifest_text:
        import stream_dev_safe
        spec = stream_dev_safe.parse_dev_scene(manifest_text)
        done = stream_dev_safe.apply_dev_safe(obs, spec)
        removed = removed or any(a.startswith("remove_item ") for a in done)
        print("dev-scene seed: %s" % (", ".join(done) or "present (no change)"))
    # issue 1462: the nested-pro design (stream-snv) only keeps the measurement clip out of Development.
    elif '"dev_scene_nested"' in manifest_text:
        import stream_dev_safe
        nested = stream_dev_safe.parse_dev_scene_nested(manifest_text)
        done = stream_dev_safe.apply_dev_nested(obs, nested)
        removed = removed or bool(done)
        print("dev-scene seed (nested-pro): %s" % (", ".join(done) or "present (no change)"))
    # issue 1462: in Studio Mode the program is a duplicate made at load, so a removed item stays on program
    # until the scene is taken again; obs_phase2 re-takes it behind the production-scene guard.
    if removed:
        program_retake(args.host, args.port, args.password)


if __name__ == "__main__":
    main()
