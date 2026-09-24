#!/usr/bin/env python3
"""strih_bandwidth_roles.py -- the strih BANDWIDTH ROLES: full-bandwidth NDI only for SHOWN cameras.

issue 1242, owner ruling 24.9.2026: strih pulls FULL bandwidth only for the cameras that are shown --
preview, program, a projector, the visible item of the Grading NDI-output scene. A cold start in
preview is accepted. Two receiver roles per camera:

  * PROGRAM-PATH main (`NDI camN`, genlock_connect_on_show=True): the vendored DistroAV receiver PARKS
    it (releases the NDI receiver, blanks the source) while nothing shows it and reconnects on show.
  * MONITOR twin (`MV NDI camN`, genlock_monitor=True): the #501 low-bandwidth receiver of the SAME
    sender, ALWAYS connected, rendered by the built-in multiview through an `MV <scene>` twin scene.

The built-in multiview must never render a scene that holds a full program-path input (directly or
through a nested scene) -- `Multiview::Update` calls obs_source_inc_showing on every scene it renders,
so such a scene would keep its camera "shown" (= connected) forever. Each twin scene carries the
private setting MULTIVIEW_TARGET_KEY = its program scene's name; the vendored OBS frontend multiview
(`vendor/obs-studio/frontend/components/Multiview.cpp`) resolves a twin cell to that target for the
tally border, the label and a click, so the operator's multiview behaves exactly as before.

This reverses, for the strih role, the issue-761 same-source multiview and the issue-764 keep-alive of
every genlocked input. Scoped by ROLE, never by platform: only fleet camera senders become program-path
(the cg inputs stay always connected -- a CG cut-in must be instant; the 2ME feedback inputs are not
genlocked); stream / resolume never run this module.

Run on every strih OBS launch by `strih_scenes.py --apply-roles` (strih-obs-start.sh, after the
--bootstrap seed). Idempotent: a correct collection is a pure read. ROLE-OWNED creates only (the twin
inputs/scenes + a scratch refresh scene); the operator's own inputs/scenes are never created, renamed
or removed, and the multiview membership of an existing twin is never re-imposed (operator wins).
The pure planners carry no WebSocket dependency (tests/python/test_strih_bandwidth_roles_1242.py).
"""
import os
import re
import time

CONNECT_ON_SHOW_KEY = "genlock_connect_on_show"
# The strih-side E2E HOLD marker (scripts/lib/connect-on-show-hold.sh touches it over ssh at the hold,
# removes it at the restore). While it is FRESH, a launch-time role apply keeps every program-path main
# CONNECTED (connect-on-show off): a strih OBS relaunch in the middle of an E2E run (the #1093 wedge
# escalation) must never re-park the inputs the run is measuring. A marker older than the TTL (a run
# SIGKILLed before its restore) expires, so the roles come back on the next launch.
E2E_HOLD_MARKER = os.path.expanduser("~/.camera-box/connect-on-show-e2e-hold")
E2E_HOLD_TTL_S = 4 * 3600
E2E_HOLD_FUTURE_SKEW_S = 60
GENLOCK_MONITOR_KEY = "genlock_monitor"
TWIN_PREFIX = "MV "
# The private scene setting the vendored multiview reads: the twin cell stands in for this scene.
MULTIVIEW_TARGET_KEY = "camera_box_multiview_target"
# A fleet camera sender (`CAM<n> (usb)`, `CAM<n> (30p)`, ...) -- the only program-path senders.
PROGRAM_PATH_SENDER_RE = re.compile(r"^CAM\d+ \(")
SCENE_SOURCE_TYPE = "OBS_SOURCE_TYPE_SCENE"
# Audio-only inputs carry no picture; a twin scene never needs them (a multiview only SHOWS a scene --
# its audio is never mixed).
AUDIO_ONLY_INPUT_KINDS = frozenset({
    "pulse_input_capture", "pulse_output_capture", "alsa_input_capture", "jack_input_client",
    "asio_input_capture", "wasapi_input_capture", "wasapi_output_capture",
    "wasapi_process_output_capture", "coreaudio_input_capture", "coreaudio_output_capture",
})
# obs-websocket 5 SetSceneItemTransform accepts only these (GetSceneItemList also returns the read-only
# computed width/height/sourceWidth/sourceHeight). The ONE owner of this list: strih_mv_scenes.py
# imports it from here.
SETTABLE_TRANSFORM_FIELDS = frozenset({
    "positionX", "positionY", "rotation", "scaleX", "scaleY", "alignment",
    "boundsType", "boundsAlignment", "boundsWidth", "boundsHeight",
    "cropLeft", "cropTop", "cropRight", "cropBottom",
})
BOUNDS_NONE = "OBS_BOUNDS_NONE"
# OBS re-reads multiview membership only on a scene-list change (UpdateMultiviewProjectors on
# add/remove/rename); a create+remove of this scratch scene triggers it.
MULTIVIEW_REFRESH_SCENE = "__camera-box multiview refresh (issue 1242)"


# --- PURE planners (no WebSocket -> Tier-0 testable) -------------------------------------------------

def is_program_path_sender(sender):
    """True for a fleet camera sender (`CAM<n> (...)`) -- the only program-path inputs."""
    return bool(PROGRAM_PATH_SENDER_RE.match(sender or ""))


def program_path_inputs(plan):
    """The seed plan's program-path INPUT names (plan order): genlocked camera-class inputs whose
    sender is a fleet camera."""
    return [item["input"] for item in plan
            if item["settings"].get("genlock_fifo") and is_program_path_sender(item["ndi_source_name"])]


def twin_name(name):
    """'Cam 3' -> 'MV Cam 3', 'NDI cam3' -> 'MV NDI cam3' (the strih_mv_scenes.py convention)."""
    return TWIN_PREFIX + name


def is_twin(name):
    return (name or "").startswith(TWIN_PREFIX)


def main_role_settings(e2e_hold=False):
    """The program-path role for a main camera input (fresh dict each call): connect-on-show, unless
    an E2E run holds every program-path input connected."""
    return {CONNECT_ON_SHOW_KEY: not e2e_hold}


def e2e_hold_active(path, now, ttl_s):
    """True iff the strih-side E2E hold marker exists and is younger than `ttl_s` at `now`. A marker
    stamped in the FUTURE (the clock stepped back) counts only within E2E_HOLD_FUTURE_SKEW_S, so a
    stale marker can never outlive the TTL through a clock step."""
    try:
        age = now - os.stat(path).st_mtime
    except OSError:
        return False
    return -E2E_HOLD_FUTURE_SKEW_S <= age < ttl_s


def twin_role_settings(main_effective):
    """The monitor-twin ROLE settings WITHOUT the sender name (the name is created with the twin and
    otherwise healed through the #795-safe read-back-verified re-enforce): genlocked at the main's
    pin, genlock_monitor (the forcer narrows it to LOWEST bandwidth; never parked), connect-on-show
    explicitly OFF, NDI audio off (a camera twin never feeds the mixer)."""
    main_effective = main_effective or {}
    return {
        "genlock_fifo": True,
        "ndi_sync": 2,
        "genlock_latency_ms_src": main_effective.get("genlock_latency_ms_src", 3),
        GENLOCK_MONITOR_KEY: True,
        CONNECT_ON_SHOW_KEY: False,
        "ndi_audio": False,
    }


def twin_input_settings(main_effective):
    """The full settings a NEW twin input is created with: the role settings + the main's LIVE
    sender name."""
    return dict(twin_role_settings(main_effective),
                ndi_source_name=(main_effective or {}).get("ndi_source_name", ""))


def settable_transform_fields(transform):
    """Strip a GetSceneItemList transform down to the fields SetSceneItemTransform accepts."""
    return {k: v for k, v in (transform or {}).items() if k in SETTABLE_TRANSFORM_FIELDS}


CROP_FIELDS = ("cropLeft", "cropTop", "cropRight", "cropBottom")


def twin_transform(transform, canvas):
    """The transform for a twin item that REPLACES a full-bandwidth item. NDI 'lowest' is a
    lower-resolution proxy, so a main placed by scale (no bounds) would draw its twin as a small
    image in the corner: pin the twin to BOUNDS equal to the main item's on-canvas footprint so the
    drawn size no longer depends on the source resolution. The footprint is the main's NOMINAL size x
    its scale -- its sourceWidth when known, else the canvas (a fleet camera is canvas-sized): a
    parked or not-yet-delivering main reports a ZERO computed size (async inactive), and the twin
    must get the same footprint either way or it would be rebuilt on every launch. Crop values are in
    MAIN-source pixels and would over-crop the proxy, so they are never mirrored. A main that already
    uses bounds keeps its transform (minus crop)."""
    t = transform or {}
    out = {k: v for k, v in settable_transform_fields(t).items() if k not in CROP_FIELDS}
    if t.get("boundsType") not in (None, BOUNDS_NONE):
        return out
    cw, ch = canvas
    w = (t.get("sourceWidth") or cw) * (t.get("scaleX") or 1.0)
    h = (t.get("sourceHeight") or ch) * (t.get("scaleY") or 1.0)
    out.update({"boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
                "boundsWidth": float(w), "boundsHeight": float(h),
                "scaleX": 1.0, "scaleY": 1.0})
    return out


def has_crop(transform):
    """True iff a main item is cropped (a crop the twin cannot mirror -- reported)."""
    return any((transform or {}).get(k) for k in CROP_FIELDS)


def is_scene_item(item):
    return (item or {}).get("sourceType") == SCENE_SOURCE_TYPE


def is_custom_multiview_scene(name):
    """The operator's hand-built multiview GRID scene ('MULTIVIEW' on strih-lx) -- case-insensitive."""
    return (name or "").strip().lower() == "multiview"


def scenes_needing_twins(items_by_scene, program_inputs, output_scenes):
    """The set of scenes that must be replaced by an `MV` twin wherever the multiview (or a twin)
    renders them: a scene that holds a program-path input DIRECTLY, or NESTS such a scene (recursive,
    cycle-safe). Never an NDI-output scene (Grading, Interkom: the filter only sends while its parent
    is showing, and Grading's one enabled camera IS the wanted full grading feed), never a twin, never
    the custom multiview grid (swapped item-by-item instead)."""
    prog = set(program_inputs)
    memo = {}

    def needs(scene, stack):
        if scene in memo:
            return memo[scene]
        if (scene in stack or scene in output_scenes or is_twin(scene)
                or is_custom_multiview_scene(scene) or scene not in items_by_scene):
            return False
        stack = stack | {scene}
        result = False
        for it in items_by_scene[scene]:
            name = it.get("sourceName")
            if name in prog or (is_scene_item(it) and needs(name, stack)):
                result = True
                break
        memo[scene] = result
        return result

    return {s for s in items_by_scene if needs(s, frozenset())}


def twin_scene_items(items, program_inputs, twinned_scenes, canvas):
    """The desired twin-scene items, in the original stacking order: a program-path input becomes its
    monitor twin and a nested scene that has a twin becomes that twin (both with twin_transform);
    audio-only inputs are dropped; every other item is reused as-is. Enabled state is preserved."""
    prog = set(program_inputs)
    out = []
    for i in items or []:
        if i.get("inputKind") in AUDIO_ONLY_INPUT_KINDS:
            continue
        name = i.get("sourceName")
        new = twin_name(name) if name in prog else (twinned_scenes.get(name) if is_scene_item(i) else None)
        out.append({
            "sourceName": new or name,
            "sceneItemEnabled": bool(i.get("sceneItemEnabled", True)),
            "sceneItemTransform": (twin_transform(i.get("sceneItemTransform"), canvas) if new
                                   else settable_transform_fields(i.get("sceneItemTransform"))),
        })
    return out


def _round(v):
    return round(v, 2) if isinstance(v, float) else v


def twin_items_match(current_items, desired):
    """True iff the twin scene already carries exactly `desired`: source + enabled in order, and every
    desired transform field (floats compared at 0.01)."""
    cur = list(current_items or [])
    if len(cur) != len(desired):
        return False
    for c, d in zip(cur, desired):
        if c.get("sourceName") != d["sourceName"]:
            return False
        if bool(c.get("sceneItemEnabled", True)) != d["sceneItemEnabled"]:
            return False
        ct = c.get("sceneItemTransform") or {}
        for k, v in d["sceneItemTransform"].items():
            if _round(ct.get(k)) != _round(v):
                return False
    return True


def multiview_swap_plan(items, program_inputs, twinned_scenes, canvas):
    """For the custom multiview grid scene: every item that renders a program-path input directly, or
    a scene that has an `MV` twin, is swapped for its twin (same spot, same enabled state, the twin
    transform), so the grid never keeps a full input shown."""
    prog = set(program_inputs)
    plan = []
    for i in items or []:
        name = i.get("sourceName")
        new = twin_name(name) if name in prog else (twinned_scenes.get(name) if is_scene_item(i) else None)
        if not new:
            continue
        plan.append({
            "old_item_id": i["sceneItemId"],
            "new_name": new,
            "enabled": bool(i.get("sceneItemEnabled", True)),
            "transform": twin_transform(i.get("sceneItemTransform"), canvas),
        })
    return plan


def membership_on_create(orig_shown, twin_shown):
    """(original, twin) built-in-multiview membership applied ONCE, when a twin is created or first
    adopted: the twin takes over the cell the original -- or an already-shown twin (the migrated
    Windows #501/#761 layout) -- held; a twin of a scene nobody shows (only needed as a nested
    reference) is not shown on its own. Never re-imposed afterwards (operator wins)."""
    return (False, True) if (orig_shown or twin_shown) else (False, False)


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


def role_update_needed(effective, desired):
    """True iff `desired` differs from `effective`. A bool is compared by truthiness: obs-websocket
    omits a bool key that equals its (possibly unregistered) default, so absent == False must be a
    pure read, never a re-write every launch."""
    for k, v in desired.items():
        ev = (effective or {}).get(k)
        if isinstance(v, bool):
            if bool(ev) != v:
                return True
        elif ev != v:
            return True
    return False


# --- applying the roles over WS -----------------------------------------------------------------------

def _scenes_module():
    import os
    import sys
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import strih_scenes  # noqa: E402 -- sibling module (Obs client, effective settings, name re-enforce)
    return strih_scenes


def _private(obs, source):
    return (obs.req("GetSourcePrivateSettings", {"sourceName": source}, ignore_err=True)
            or {}).get("sourceSettings") or {}


def _multiview_shown(private):
    v = private.get("show_in_multiview")
    return True if v is None else bool(v)


def _set_private(obs, source, settings):
    obs.req("SetSourcePrivateSettings", {"sourceName": source, "sourceSettings": settings},
            ignore_err=True)


def _scene_items(obs, scene):
    return (obs.req("GetSceneItemList", {"sceneName": scene}, ignore_err=True) or {}).get("sceneItems") or []


def _is_ndi_output_scene(obs, scene):
    fl = (obs.req("GetSourceFilterList", {"sourceName": scene}, ignore_err=True) or {}).get("filters") or []
    return any(f.get("filterKind") == "ndi_filter" and f.get("filterEnabled") for f in fl)


def _canvas(obs):
    v = obs.req("GetVideoSettings", ignore_err=True) or {}
    return (v.get("baseWidth") or 1920, v.get("baseHeight") or 1080)


def _add_scene_item(obs, scene, source, enabled, transform, inputs, create_settings):
    """Add `source` to `scene`. A twin input that does not exist yet is CREATED here (inside this
    scene) with its settings and muted; anything else is a plain CreateSceneItem."""
    if source not in inputs and source in create_settings:
        res = obs.req("CreateInput", {
            "sceneName": scene, "inputName": source, "inputKind": "ndi_source",
            "inputSettings": create_settings[source], "sceneItemEnabled": enabled,
        }, ignore_err=True) or {}
        inputs[source] = "ndi_source"
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


def apply_bandwidth_roles(obs, plan, hold_marker=E2E_HOLD_MARKER, now=None):
    """Apply the roles to the live collection. Returns a summary dict for the log. Steps:
      1. every program-path main gets genlock_connect_on_show=True -- or False while a FRESH E2E hold
         marker exists (e2e_hold_active: an OBS relaunch in the middle of an E2E run must not re-park
         the inputs the run measures);
      2. every twin input is healed to its role settings (the sender name through the #795-safe
         read-back-verified re-enforce) -- skipped with a problem when the main has NO sender (an
         empty name would stop the twin's receiver thread) or a non-NDI input already owns the name;
      3. every scene that needs a twin (scenes_needing_twins) gets/keeps an `MV <scene>` twin mirroring
         it (rebuilt only on drift); a NEW or never-adopted twin gets the membership_on_create hand-off
         and its MULTIVIEW_TARGET_KEY; an existing adopted twin's membership is never re-imposed; a
         cropped camera item is reported (the proxy cannot mirror main-pixel crop);
      4. a twin whose original no longer needs one is RETIRED (original back in the multiview, twin
         out, the adoption key cleared so the twin takes the cell over again if the camera returns)
         -- the twin scene itself is left in place, never deleted;
      5. the custom multiview GRID scene renders twins instead of full inputs;
      6. when membership changed, the built-in multiview is refreshed (scratch-scene create+remove)."""
    ss = _scenes_module()
    op = ss._obs_phase2_module()
    hold = e2e_hold_active(hold_marker, time.time() if now is None else now, E2E_HOLD_TTL_S)
    summary = {"mains": [], "twins": [], "twin_scenes": [], "retired": [], "multiview_grid": [],
               "problems": [], "refreshed": False, "e2e_hold": hold}
    inputs = {i.get("inputName"): i.get("inputKind")
              for i in (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])}
    prog = [p for p in program_path_inputs(plan) if p in inputs]
    if not prog:
        return summary

    main_eff = {m: ss._effective_input_settings(obs, m) for m in prog}
    for m in prog:
        if role_update_needed(main_eff[m], main_role_settings(hold)):
            obs.req("SetInputSettings", {
                "inputName": m, "inputSettings": main_role_settings(hold), "overlay": True,
            }, ignore_err=True)
            summary["mains"].append(m)

    usable = []
    create_settings = {}
    for m in prog:
        tw = twin_name(m)
        sender = main_eff[m].get("ndi_source_name") or ""
        if not sender:
            summary["problems"].append("%r has no sender -- twin skipped" % m)
            continue
        if tw in inputs and inputs[tw] != "ndi_source":
            summary["problems"].append("%r exists as %r, not an NDI input -- twin skipped" % (tw, inputs[tw]))
            continue
        usable.append(m)
        create_settings[tw] = twin_input_settings(main_eff[m])
        if tw in inputs:
            eff = ss._effective_input_settings(obs, tw)
            if role_update_needed(eff, twin_role_settings(main_eff[m])):
                obs.req("SetInputSettings", {"inputName": tw, "inputSettings": twin_role_settings(main_eff[m]),
                                              "overlay": True}, ignore_err=True)
                summary["twins"].append(tw)
            if eff.get("ndi_source_name") != sender:
                ss._enforce_ndi_source_name(obs, op, tw, sender)
                summary["twins"].append(tw + " (sender)")
    if not usable:
        return summary

    canvas = _canvas(obs)
    scenes = [s.get("sceneName") for s in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", [])]
    items_by_scene = {s: _scene_items(obs, s) for s in scenes if not is_twin(s)}
    output_scenes = {s for s in items_by_scene if _is_ndi_output_scene(obs, s)}
    needing = scenes_needing_twins(items_by_scene, usable, output_scenes)
    twinned = {s: twin_name(s) for s in needing}
    membership_changed = False

    for sc in sorted(needing):
        tw_sc = twinned[sc]
        exists = tw_sc in scenes
        if not exists:
            obs.req("CreateScene", {"sceneName": tw_sc}, ignore_err=True)
            scenes.append(tw_sc)
        desired = twin_scene_items(items_by_scene[sc], usable, twinned, canvas)
        for it in items_by_scene[sc]:
            if it.get("sourceName") in usable and has_crop(it.get("sceneItemTransform")):
                summary["problems"].append("%r crops %r -- its twin shows the full frame" % (sc, it["sourceName"]))
        current = _scene_items(obs, tw_sc) if exists else []
        if not twin_items_match(current, desired):
            for it in current:
                obs.req("RemoveSceneItem", {"sceneName": tw_sc, "sceneItemId": it["sceneItemId"]},
                        ignore_err=True)
            for d in desired:
                _add_scene_item(obs, tw_sc, d["sourceName"], d["sceneItemEnabled"],
                                d["sceneItemTransform"], inputs, create_settings)
            summary["twin_scenes"].append(tw_sc)
        tw_priv = _private(obs, tw_sc)
        if tw_priv.get(MULTIVIEW_TARGET_KEY) != sc:
            # created now, or never adopted: the ONE membership hand-off + the target marker.
            orig_show, twin_show = membership_on_create(_multiview_shown(_private(obs, sc)),
                                                        _multiview_shown(tw_priv) if exists else False)
            _set_private(obs, sc, {"show_in_multiview": orig_show})
            _set_private(obs, tw_sc, {"show_in_multiview": twin_show, MULTIVIEW_TARGET_KEY: sc})
            membership_changed = True

    for tw_sc in [s for s in scenes if is_twin(s)]:
        target = _private(obs, tw_sc).get(MULTIVIEW_TARGET_KEY)
        if not target or target in needing or target not in items_by_scene:
            continue
        # the original no longer holds a program input: hand its multiview cell back and release
        # the adoption (a returning camera then hands the cell to the twin again).
        if _multiview_shown(_private(obs, tw_sc)):
            _set_private(obs, target, {"show_in_multiview": True})
            _set_private(obs, tw_sc, {"show_in_multiview": False})
            membership_changed = True
        _set_private(obs, tw_sc, {MULTIVIEW_TARGET_KEY: ""})
        summary["retired"].append(tw_sc)

    for sc in [s for s in items_by_scene if is_custom_multiview_scene(s)]:
        for e in multiview_swap_plan(items_by_scene[sc], usable, twinned, canvas):
            # add the twin BEFORE removing the full input (the grid never drops a tile mid-swap)
            _add_scene_item(obs, sc, e["new_name"], e["enabled"], e["transform"], inputs, create_settings)
            obs.req("RemoveSceneItem", {"sceneName": sc, "sceneItemId": e["old_item_id"]}, ignore_err=True)
            summary["multiview_grid"].append(e["new_name"])

    if membership_changed:
        obs.req("CreateScene", {"sceneName": MULTIVIEW_REFRESH_SCENE}, ignore_err=True)
        obs.req("RemoveScene", {"sceneName": MULTIVIEW_REFRESH_SCENE}, ignore_err=True)
        summary["refreshed"] = True
    return summary
