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
  --projector T          issue 1346: set the HDMI output VIEW (program|multiview) in
                         ~/.camera-box/drm-output.json -- the in-OBS DRM-lease output reads it at the
                         next OBS start. The operator's live switch is in OBS (Tools menu); this is
                         the scripted twin. Never opens a projector window and never talks to OBS.
  --apply-roles          issue 1242: apply the strih BANDWIDTH ROLES (program-path cameras connect
                         only while shown; the multiview renders always-connected low-bandwidth `MV`
                         twins) -- strih_bandwidth_roles.py, run by strih-obs-start.sh on every launch.

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

# --- issue 1346: the HDMI output = the in-OBS DRM-lease output, view Program / Multiview ------------
# Owner ROZHODNUTE (24.9.2026, supersedes the 19.9. OBS fullscreen projector): the strih-lx HDMI
# output is the SAME fixed hardware output imag has -- the vendored libobs DRM-lease output (issue
# 1152, .claude/rules/obs-drm-output.md) -- selectable between the Program and the BUILT-IN Multiview.
# It is never a projector window and never the desktop. The module's activation contract is ONE
# file of the OBS user: ~/.camera-box/drm-output.json ({"enabled":true,"connector":"HDMI-0",
# "argb":2105376,"view":"multiview"}, provisioned by setup-strih.sh step 6 only when an HDMI monitor
# is plugged in). The operator's switch lives in OBS (Tools > HDMI vystup: Program / Multiview),
# which switches live and writes "view" back; `--projector program|multiview` is the scripted twin
# (effective at the next OBS start).
DRM_OUTPUT_CONF = "~/.camera-box/drm-output.json"
DRM_OUTPUT_VIEWS = ("program", "multiview")
# issue 1346: how the output leaves the X desktop -- the X RandR lease (the default = the absent key) or
# the NVIDIA Vulkan direct display. Mirrors obs-drm-output-backend.c drm_output_parse_backend.
DRM_OUTPUT_BACKENDS = ("lease", "vk-direct")


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


# --- issue 1346: DRM-lease HDMI output helpers (pure + the file read/write; no WS dependency) --------

def drm_output_view_of(value):
    """Pure: the "view" JSON value -> "program" | "multiview" | None (unknown). Mirrors the vendored C
    grammar (obs-drm-output-view.c drm_output_parse_view) row for row -- ONE shared table,
    tests/fixtures/drm_output_view_parity.tsv. Absent (None) / empty / a non-string (which
    obs_data_get_string reads as "") -> "program" (the issue-1152 default, imag unchanged); only the
    exact lowercase names count."""
    if value is None or not isinstance(value, str) or value == "":
        return "program"
    return value if value in DRM_OUTPUT_VIEWS else None


def drm_output_lease_connector(config_text):
    """Pure: the connector IFF the drm-output config arms the in-OBS DRM output, else "".
    The C module's OWN contract (and imag_scenes.drm_output_lease_connector's, pinned equal by
    tests/python/test_strih_drm_output_1346.py): a full JSON parse, a boolean "enabled": true, a
    non-empty string "connector" AND (issue 1346) a known "backend" -- the C keeps the output dormant on
    an unknown one, so the wrapper must not take the connector out of X for it (a black HDMI).
    Empty / malformed / disabled / unknown backend -> "" (dormant), never a raise."""
    if not config_text:
        return ""
    try:
        cfg = json.loads(config_text)
        if cfg.get("enabled") is not True:
            return ""
        if drm_output_backend_of(cfg.get("backend")) is None:
            return ""
        connector = cfg.get("connector")
        return connector if isinstance(connector, str) and connector else ""
    except (ValueError, AttributeError):
        return ""


def drm_output_view_token(config_text):
    """Pure: the config's view as ONE token for verify-strih -- "program" | "multiview" | "unknown".
    An unreadable config is "program" (it arms nothing; the connector half reports that)."""
    try:
        cfg = json.loads(config_text) if config_text else {}
    except ValueError:
        return "program"
    if not isinstance(cfg, dict):
        return "program"
    return drm_output_view_of(cfg.get("view")) or "unknown"


def drm_output_backend_of(value):
    """Pure: the "backend" JSON value -> "lease" | "vk-direct" | None (unknown). Mirrors the vendored C
    grammar (obs-drm-output-backend.c drm_output_parse_backend) row for row -- ONE shared table,
    tests/fixtures/drm_output_backend_parity.tsv. Absent (None) / empty / a non-string (which
    obs_data_get_string reads as "") -> "lease" (the issue-1152 default, imag unchanged); only the exact
    lowercase names count. The C keeps the output DORMANT on an unknown value."""
    if value is None or not isinstance(value, str) or value == "":
        return "lease"
    return value if value in DRM_OUTPUT_BACKENDS else None


def drm_output_backend_token(config_text):
    """Pure: the config's backend as ONE token for verify-strih -- "lease" | "vk-direct" | "unknown". An
    unreadable config is "lease" (it arms nothing; the connector half reports that)."""
    try:
        cfg = json.loads(config_text) if config_text else {}
    except ValueError:
        return "lease"
    if not isinstance(cfg, dict):
        return "lease"
    return drm_output_backend_of(cfg.get("backend")) or "unknown"


def drm_output_config_text(path=DRM_OUTPUT_CONF):
    """The drm-output config text of THIS user (or `path`), "" when absent/unreadable (never raises
    -- the strih-obs-start.sh launch path must never abort on it)."""
    try:
        with open(os.path.expanduser(path)) as fh:
            return fh.read()
    except OSError:
        return ""


def read_drm_view(path=DRM_OUTPUT_CONF):
    """The persisted view ("program" | "multiview"), None when the config is absent/unreadable or
    its view is unknown."""
    text = drm_output_config_text(path)
    if not text:
        return None
    tok = drm_output_view_token(text)
    return None if tok == "unknown" else tok


def _rewrite_drm_config(path, mutate):
    """Rewrite an EXISTING drm-output config through `mutate(cfg)`: every other key kept in order, ONE
    compact line (the machine-written contract obs-drm-output.md pins), an atomic temp-file rename.
    Raises ValueError on an absent file (the output is not provisioned -- setup-strih.sh step 6 writes
    it only with an HDMI monitor plugged in) or a config that is not a JSON object (never rewritten)."""
    real = os.path.expanduser(path)
    try:
        with open(real) as fh:
            cfg = json.loads(fh.read())
    except OSError as e:
        raise ValueError("%s not readable (%s) -- the HDMI output is not provisioned; attach the HDMI "
                         "monitor and re-run setup-strih.sh" % (path, e))
    except ValueError as e:
        raise ValueError("%s is not valid JSON (%s) -- not rewriting it" % (path, e))
    if not isinstance(cfg, dict):
        raise ValueError("%s is not a JSON object -- not rewriting it" % path)
    mutate(cfg)
    tmp = real + ".tmp"
    with open(tmp, "w") as fh:
        fh.write(json.dumps(cfg, separators=(",", ":")) + "\n")
    os.replace(tmp, real)


def write_drm_view(view, path=DRM_OUTPUT_CONF):
    """Persist `view` into an EXISTING drm-output config (every other key kept -- see
    _rewrite_drm_config). Raises ValueError on an unknown view or an unprovisioned / broken config."""
    if view not in DRM_OUTPUT_VIEWS:
        raise ValueError("unknown HDMI output view %r (want 'program' or 'multiview')" % (view,))
    _rewrite_drm_config(path, lambda cfg: cfg.__setitem__("view", view))


def write_drm_backend(backend, path=DRM_OUTPUT_CONF):
    """issue 1346: persist the box's HDMI-output backend (the fact STRIH_HDMI_OUTPUT_BACKEND) into an
    EXISTING drm-output config, keeping the operator's view and every other key. `lease` is the ABSENT
    key (a lease config stays byte-identical to the pre-backend shape); `vk-direct` is written
    explicitly. Raises ValueError on an unknown backend or an unprovisioned / broken config."""
    if backend not in DRM_OUTPUT_BACKENDS:
        raise ValueError("unknown HDMI output backend %r (want 'lease' or 'vk-direct')" % (backend,))

    def mutate(cfg):
        if backend == "lease":
            cfg.pop("backend", None)
        else:
            cfg["backend"] = backend

    _rewrite_drm_config(path, mutate)


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


def _roles_module():
    """issue 1242: the strih BANDWIDTH ROLES live in the sibling strih_bandwidth_roles.py (installed next
    to this file by setup-strih.sh step 6). Imported LAZILY, only by --apply-roles and the report-only
    --verify-parity role line, so the strih-obs-start.sh launch preflight (`import strih_scenes`) and
    --bootstrap never depend on it. Returns None when it is not importable (an older box)."""
    try:
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import strih_bandwidth_roles  # noqa: E402
        return strih_bandwidth_roles
    except Exception as e:  # noqa: BLE001 -- absence is expected on an older box; report, never crash
        print("issue 1242: strih_bandwidth_roles not importable (%s)" % e)
        return None


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
    rm = _roles_module()
    if rm is not None:
        roles = rm.bandwidth_role_problems(actual, rm.program_path_inputs(plan))
        print("strih bandwidth roles: " + ("; ".join(roles) if roles else "OK"))
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
                    help="issue 1346: set the HDMI output view (program|multiview) in "
                         "~/.camera-box/drm-output.json -- the in-OBS DRM-lease output reads it at the "
                         "next OBS start (the live switch is OBS Tools > HDMI vystup)")
    args = ap.parse_args()

    # An explicit mode is REQUIRED (issue 1317 review): a bare invocation must never silently connect
    # and MUTATE OBS. --bootstrap seeds; --verify-parity is read-only; --projector sets the HDMI
    # output view (issue 1346).
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

    # --projector (issue 1346): persist the HDMI output view. Touches only drm-output.json -- never
    # the input seed manifest and never OBS (the running OBS switches live from its own Tools menu).
    if args.projector is not None:
        try:
            write_drm_view(args.projector, DRM_OUTPUT_CONF)
        except ValueError as e:
            sys.exit("FAIL issue 1346: %s" % e)
        print("HDMI output view set to %s in %s -- takes effect at the next OBS start (switch it live in "
              "OBS Tools > HDMI vystup)" % (args.projector, DRM_OUTPUT_CONF))
        return

    manifest_text = _read_manifest(args.manifest)

    obs = Obs(args.host, args.port, args.password)
    if args.verify_parity:
        verify_parity(obs, manifest_text)
        return
    if args.apply_roles:
        # issue 1242: the bandwidth roles (a SEPARATE mode from --bootstrap so the seed's update-only
        # never-create contract stays exactly as issue 1317 pinned it; the role lib owns its twins).
        rm = _roles_module()
        if rm is None:
            sys.exit("FAIL issue 1242: strih_bandwidth_roles.py missing next to strih_scenes.py "
                     "(re-run setup-strih.sh step 6)")
        inputs, _outputs, latency = parse_seed_manifest(manifest_text)
        summary = rm.apply_bandwidth_roles(obs, seed_inputs(inputs, latency))
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


if __name__ == "__main__":
    main()
