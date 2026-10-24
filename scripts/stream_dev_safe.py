#!/usr/bin/env python3
"""issue 1380 (ROZHODNUTE 13.10.2026) -- the YouTube-SAFE `Development` scene of a stream-ROLE box.

On SNV the Development scene nests the production scene `PRO` (scripts/stream_dev_scene.py). On a
stream-role box (stream-pp) `PRO` holds the live venue mixer (`Zvuk z mixu`, continuous program
audio, likely music), and an OBS mute is per INPUT, never per scene: nesting `PRO` would put the
mixer on YouTube. This Development holds exactly two items:
  * the measurement clip (scripts/gen_measurement_clip.py: the painter QR + the QPSK marker over a
    -30 dBFS tone bed, nothing copyrighted) as a looping local `ffmpeg_source`, at the BOTTOM;
  * the program video input (`NDI 2ME PGM`, muted by the operator) on top, covering the clip's
    picture. Its audio stays out because the input is muted; the verdict FAILs when it is not.

The seeder is operator-wins (the issue-1380 seeder's rules): it creates the scene, the clip input and
a missing item, never edits an existing item, never touches the production scene or any other scene,
never mutes/unmutes, never selects a program or preview scene. The one removal (issue 1462): an item
that is neither of the two (a nested production scene, a mixer capture) is removed from the
development scene, since it would put that content on YouTube.

The spec is the seed manifest's `dev_scene` object (scripts/lib/obs-box-role.sh writes it on a
stream box). `strih_scenes.py --bootstrap` seeds it on every OBS launch; verify-strih runs
`stream_dev_safe.py --verify`.
"""
import argparse
import json
import os
import sys

DEV_SCENE_KEY = "dev_scene"
SPEC_FIELDS = ("scene", "video_input", "audio_input", "audio_file")
PRODUCTION_SCENE = "PRO"
PROGRAM_AUDIO_INPUT = "Zvuk z mixu"
CLIP_INPUT_KIND = "ffmpeg_source"


# The spellings a ported collection may carry for a registered sign (U+00AE) in a source name, the
# same tuple as scripts/obs_collection_port.py (pinned by a pytest): live stream-pp kept the real sign,
# `NDI\u00ae 2ME PGM`, where the manifest declares `NDI 2ME PGM`.
REGISTERED_SIGN_SPELLINGS = ("\u00ae", "\u00c2\u00ae", "(R)", "Ar")


class DevSafeError(Exception):
    """The safe development scene cannot be planned (a malformed spec, or the video input missing).
    Fail loud; never guess another source."""


def parse_dev_scene(manifest_text):
    """The manifest's `dev_scene` spec as a dict of the four SPEC_FIELDS, or None without the key.
    Raises DevSafeError on a malformed spec (a field missing or empty, the production scene named as
    the development scene, the program audio input named as an item: PROGRAM_AUDIO_INPUT, and the
    manifest's own `program_audio_input`, the box fact -- issue 1462: stream-snv's is `mbc`)."""
    d = json.loads(manifest_text)
    spec = d.get(DEV_SCENE_KEY) if isinstance(d, dict) else None
    if spec is None:
        return None
    if not isinstance(spec, dict):
        raise DevSafeError("manifest dev_scene must be an object")
    out = {}
    for field in SPEC_FIELDS:
        value = spec.get(field)
        if not isinstance(value, str) or not value:
            raise DevSafeError(f"manifest dev_scene.{field} must be a non-empty string")
        out[field] = value
    if out["scene"] == PRODUCTION_SCENE:
        raise DevSafeError(f"the development scene must differ from the production scene '{PRODUCTION_SCENE}'")
    program_inputs = [PROGRAM_AUDIO_INPUT]
    declared = d.get("program_audio_input")
    if isinstance(declared, str) and declared:
        program_inputs.append(declared)
    for program in program_inputs:
        if program in (out["video_input"], out["audio_input"]):
            raise DevSafeError(f"the program audio input '{program}' is never a development item")
    if out["video_input"] == out["audio_input"]:
        raise DevSafeError("the video input and the clip input must differ")
    return out


def resolve_spec(spec, input_names):
    """*spec* with `video_input` resolved against the live input names: the declared name when it
    exists, else its registered-sign spelling after the leading `NDI` (`NDI 2ME PGM` ->
    `NDI\u00ae 2ME PGM` ...). Never any other source; the declared name stays when nothing matches."""
    declared = spec["video_input"]
    if declared in input_names or not declared.startswith("NDI "):
        return spec
    for sign in REGISTERED_SIGN_SPELLINGS:
        alt = "NDI" + sign + declared[3:]
        if alt in input_names:
            return dict(spec, video_input=alt)
    return spec


def clip_input_settings(path):
    """The ffmpeg_source settings of the measurement clip: the local file, looping, never closed."""
    return {"is_local_file": True, "local_file": path, "looping": True,
            "restart_on_activate": False, "close_when_inactive": False, "hw_decode": False}


def _item(items, name):
    for it in items or []:
        if it.get("sourceName") == name:
            return it
    return None


def dev_safe_plan(scene_names, input_names, dev_items, spec):
    """What the seeder must do, a list over ["create_scene", "create_clip_input", "add_clip_item",
    "lower_clip_item", "add_video_item"] in that order. *dev_items* = the development scene's
    GetSceneItemList `sceneItems` (None when the scene does not exist). An existing item, enabled or
    not, is left alone (operator-wins). Raises DevSafeError when the video input does not exist."""
    if spec["video_input"] not in input_names:
        raise DevSafeError(
            f"the program video input '{spec['video_input']}' does not exist on this OBS; refusing to "
            f"build '{spec['scene']}' without it")
    actions = []
    items = []
    if spec["scene"] not in scene_names:
        actions.append("create_scene")
    else:
        items = dev_items or []
    video_present = _item(items, spec["video_input"]) is not None
    if _item(items, spec["audio_input"]) is None:
        actions.append("create_clip_input" if spec["audio_input"] not in input_names else "add_clip_item")
        if video_present:
            actions.append("lower_clip_item")
    if not video_present:
        actions.append("add_video_item")
    return actions


def dev_safe_foreign_items(dev_items, spec):
    """[(sceneItemId, sourceName)] of every development-scene item that is neither the video input nor
    the clip: exactly what the verdict FAILs as `holds` (issue 1462: stream-snv's ported collection kept
    the nested production scene and a pulse capture there, on the program). YouTube safety wins over
    operator-wins here; the seed line names each removal."""
    allowed = {spec["video_input"], spec["audio_input"]}
    return [(it["sceneItemId"], it.get("sourceName")) for it in dev_items or []
            if it.get("sourceName") not in allowed and it.get("sceneItemId") is not None]


def dev_safe_verdict(scene_names, dev_items, video_muted, clip_settings, spec):
    """Grade the development scene: a list of (level, text) with level PASS / FAIL / NOTE.
    *video_muted* = GetInputMute of the video input (None when unreadable: fails closed);
    *clip_settings* = the clip input's inputSettings (None when the input is missing)."""
    scene = spec["scene"]
    if scene not in scene_names:
        return [("FAIL", f"(dev-scene) scene '{scene}' missing -- setup-strih step 6 + an OBS start seed it")]
    rows = []
    allowed = {spec["video_input"], spec["audio_input"]}
    for it in dev_items or []:
        name = it.get("sourceName")
        if name not in allowed:
            rows.append(("FAIL", f"(dev-scene) '{scene}' holds '{name}' -- only '{spec['video_input']}' "
                                 f"and '{spec['audio_input']}' may be in it (YouTube safety)"))
    video = _item(dev_items, spec["video_input"])
    clip = _item(dev_items, spec["audio_input"])
    if video is None:
        rows.append(("FAIL", f"(dev-scene) '{scene}' has no '{spec['video_input']}' item"))
    if video_muted is not True:
        state = "unreadable" if video_muted is None else "NOT muted"
        rows.append(("FAIL", f"(dev-scene) '{spec['video_input']}' is {state} -- its audio would reach "
                             f"YouTube from '{scene}'; it must stay muted"))
    if clip is None:
        rows.append(("FAIL", f"(dev-scene) '{scene}' has no '{spec['audio_input']}' item"))
    elif not clip.get("sceneItemEnabled", True):
        rows.append(("FAIL", f"(dev-scene) the '{spec['audio_input']}' item is disabled in '{scene}'"))
    if clip is not None:
        path = (clip_settings or {}).get("local_file")
        if path != spec["audio_file"]:
            rows.append(("FAIL", f"(dev-scene) '{spec['audio_input']}' plays '{path}', not the measurement "
                                 f"clip {spec['audio_file']}"))
    if video is not None and clip is not None:
        if clip.get("sceneItemIndex", 0) >= video.get("sceneItemIndex", 0):
            rows.append(("FAIL", f"(dev-scene) '{spec['audio_input']}' sits above '{spec['video_input']}' "
                                 f"in '{scene}' -- the clip picture would cover the program"))
    if not any(level == "FAIL" for level, _ in rows):
        rows.append(("PASS", f"(dev-scene) '{scene}' = '{spec['video_input']}' (muted) over the measurement "
                             f"clip {spec['audio_file']}; no production scene, no mixer"))
    return rows


def _carries_sign(name):
    """True when NAME carries the registered sign in any spelling a ported collection may hold
    (REGISTERED_SIGN_SPELLINGS; the bare "Ar" only right after "NDI", where the recon read printed it)."""
    return any(sign in name for sign in REGISTERED_SIGN_SPELLINGS[:3]) or name.startswith("NDIAr")


def source_name_rows(input_names, scene_names):
    """Owner 13.10.2026: the stream OBS source and scene names carry no registered sign (one spelling,
    the same names as SNV). One FAIL row naming every offender, else one PASS row."""
    bad = [n for n in list(input_names) + list(scene_names) if n and _carries_sign(n)]
    if bad:
        return [("FAIL", "(source-names) %s carry the registered sign -- rename them without it "
                         "(owner 13.10.2026), e.g. over obs-websocket SetInputName / SetSceneName"
                 % ", ".join("'%s'" % n for n in bad))]
    return [("PASS", "(source-names) %d inputs + %d scenes, none carries the registered sign"
             % (len(input_names), len(scene_names)))]


def _fit_canvas(obs, scene, item_id):
    vs = obs.req("GetVideoSettings")
    obs.req("SetSceneItemTransform", {
        "sceneName": scene, "sceneItemId": item_id,
        "sceneItemTransform": {"boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
                               "boundsWidth": float(vs.get("baseWidth", 1920)),
                               "boundsHeight": float(vs.get("baseHeight", 1080)),
                               "positionX": 0.0, "positionY": 0.0}})


def apply_dev_safe(obs, spec):
    """Apply dev_safe_plan over an obs-websocket client with `req(type, data=None, ignore_err=False)`
    (strih_scenes.Obs). First removes every foreign item of the development scene
    (dev_safe_foreign_items, issue 1462), then writes only CreateScene / CreateInput / CreateSceneItem /
    SetSceneItemTransform / SetSceneItemIndex on the development scene, never with ignore_err.
    Returns the removals (`remove_item <name>`) and the plan it applied."""
    scene = spec["scene"]
    scenes = [s.get("sceneName") for s in obs.req("GetSceneList").get("scenes", [])]
    inputs = [i.get("inputName") for i in obs.req("GetInputList").get("inputs", [])]
    spec = resolve_spec(spec, inputs)
    items = None
    if scene in scenes:
        items = obs.req("GetSceneItemList", {"sceneName": scene}).get("sceneItems", [])
    removed = []
    for item_id, name in dev_safe_foreign_items(items, spec):
        obs.req("RemoveSceneItem", {"sceneName": scene, "sceneItemId": item_id})
        removed.append("remove_item %s" % name)
    if removed:
        items = obs.req("GetSceneItemList", {"sceneName": scene}).get("sceneItems", [])
    plan = dev_safe_plan(scenes, inputs, items, spec)
    for action in plan:
        if action == "create_scene":
            obs.req("CreateScene", {"sceneName": scene})
        elif action == "create_clip_input":
            r = obs.req("CreateInput", {"sceneName": scene, "inputName": spec["audio_input"],
                                        "inputKind": CLIP_INPUT_KIND,
                                        "inputSettings": clip_input_settings(spec["audio_file"]),
                                        "sceneItemEnabled": True})
            _fit_canvas(obs, scene, r.get("sceneItemId"))
        elif action == "add_clip_item":
            r = obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": spec["audio_input"],
                                            "sceneItemEnabled": True})
            _fit_canvas(obs, scene, r.get("sceneItemId"))
        elif action == "lower_clip_item":
            item_id = obs.req("GetSceneItemId", {"sceneName": scene,
                                                 "sourceName": spec["audio_input"]}).get("sceneItemId")
            obs.req("SetSceneItemIndex", {"sceneName": scene, "sceneItemId": item_id,
                                          "sceneItemIndex": 0})
        elif action == "add_video_item":
            r = obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": spec["video_input"],
                                            "sceneItemEnabled": True})
            _fit_canvas(obs, scene, r.get("sceneItemId"))
    return removed + plan


# --- issue 1462: the nested-pro design (stream-snv) -------------------------------------------------------------
# The issue-1380 SNV Development nests the production scene, whose program audio input carries the cam2 QPSK
# marker the E2E measures; rig-mode TEST and the E2E (re)nest it (`obs_phase2.py dev-scene --nested`). The
# measurement clip has its own marker, so a clip item in that Development puts two marker streams on program
# (the PR 1472 `[4b3/8]` POLLUTED failure). This seed only removes such an item; it never adds the production
# scene and never touches another scene.
NESTED_KEY = "dev_scene_nested"
NESTED_FIELDS = ("scene", "production", "clip_input")


def parse_dev_scene_nested(manifest_text):
    """The manifest's `dev_scene_nested` spec as a dict of NESTED_FIELDS, or None without the key."""
    d = json.loads(manifest_text)
    spec = d.get(NESTED_KEY) if isinstance(d, dict) else None
    if spec is None:
        return None
    if not isinstance(spec, dict):
        raise DevSafeError("manifest dev_scene_nested must be an object")
    out = {}
    for field in NESTED_FIELDS:
        value = spec.get(field)
        if not isinstance(value, str) or not value:
            raise DevSafeError(f"manifest dev_scene_nested.{field} must be a non-empty string")
        out[field] = value
    if out["scene"] == out["production"]:
        raise DevSafeError("manifest dev_scene_nested.scene names the production scene")
    return out


def apply_dev_nested(obs, spec):
    """Remove every measurement-clip item from the nested Development scene; returns `remove_item <name>`
    entries. Writes only RemoveSceneItem on that scene, never with ignore_err."""
    scenes = [s.get("sceneName") for s in obs.req("GetSceneList").get("scenes", [])]
    if spec["scene"] not in scenes:
        return []
    items = obs.req("GetSceneItemList", {"sceneName": spec["scene"]}).get("sceneItems", [])
    done = []
    for it in items:
        if it.get("sourceName") == spec["clip_input"] and it.get("sceneItemId") is not None:
            obs.req("RemoveSceneItem", {"sceneName": spec["scene"], "sceneItemId": it["sceneItemId"]})
            done.append("remove_item %s" % spec["clip_input"])
    return done


def dev_nested_verdict(scene_names, dev_items, spec):
    """Grade the nested Development: FAIL when the scene is missing or holds the measurement clip; NOTE when
    the production scene is not nested yet (rig-mode TEST / the E2E nest it); else PASS."""
    scene = spec["scene"]
    if scene not in scene_names or dev_items is None:
        return [("FAIL", f"(dev-scene) scene '{scene}' missing -- rig-mode TEST / the E2E create it")]
    names = [it.get("sourceName") for it in dev_items]
    if spec["clip_input"] in names:
        return [("FAIL", f"(dev-scene) '{scene}' holds '{spec['clip_input']}' -- the nested-pro design never "
                         f"carries the measurement clip: two QPSK markers on program (issue 1462)")]
    if spec["production"] not in names:
        return [("NOTE", f"(dev-scene) '{scene}' does not nest '{spec['production']}' yet -- rig-mode TEST / the "
                         f"E2E nest it")]
    return [("PASS", f"(dev-scene) '{scene}' nests '{spec['production']}' and carries no measurement clip "
                     f"(nested-pro design)")]


def read_nested_verdict(obs, spec):
    """Read the live state over obs-websocket (read-only, ignore_err) and grade the nested design."""
    scenes = [s.get("sceneName") for s in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", [])]
    items = None
    if spec["scene"] in scenes:
        items = (obs.req("GetSceneItemList", {"sceneName": spec["scene"]}, ignore_err=True)
                 or {}).get("sceneItems", [])
    inputs = [i.get("inputName") for i in (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])]
    return dev_nested_verdict(scenes, items, spec) + source_name_rows(inputs, scenes)


def read_verdict(obs, spec):
    """Read the live state over obs-websocket (read-only, ignore_err) and grade it."""
    inputs = [i.get("inputName") for i in (obs.req("GetInputList", ignore_err=True) or {}).get("inputs", [])]
    spec = resolve_spec(spec, inputs)
    scenes = [s.get("sceneName") for s in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", [])]
    items = None
    if spec["scene"] in scenes:
        items = (obs.req("GetSceneItemList", {"sceneName": spec["scene"]}, ignore_err=True)
                 or {}).get("sceneItems", [])
    muted = (obs.req("GetInputMute", {"inputName": spec["video_input"]}, ignore_err=True)
             or {}).get("inputMuted")
    clip = (obs.req("GetInputSettings", {"inputName": spec["audio_input"]}, ignore_err=True)
            or {}).get("inputSettings")
    return dev_safe_verdict(scenes, items, muted, clip, spec) + source_name_rows(inputs, scenes)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=None)
    ap.add_argument("--manifest", default=None, help="default: strih_scenes.SEED_MANIFEST_PATH")
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--seed", action="store_true", help="create the safe Development scene (create-only)")
    mode.add_argument("--verify", action="store_true", help="read-only: print PASS/FAIL/NOTE rows")
    args = ap.parse_args()
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import strih_scenes  # the shared seed-manifest path + minimal OBS-WS client, installed beside this file
    with open(args.manifest or strih_scenes.SEED_MANIFEST_PATH) as fh:
        text = fh.read()
    spec = parse_dev_scene(text)
    nested = parse_dev_scene_nested(text)
    if spec is None and nested is None:
        print("NOTE (dev-scene) the seed manifest carries no dev_scene -- not a stream-role box")
        return 0
    obs = strih_scenes.Obs(args.host, args.port, args.password)
    if args.seed:
        done = apply_dev_safe(obs, spec) if spec is not None else apply_dev_nested(obs, nested)
        print("dev-scene seed: %s" % (", ".join(done) or "present (no change)"))
        return 0
    rows = read_verdict(obs, spec) if spec is not None else read_nested_verdict(obs, nested)
    for level, text in rows:
        print(f"{level} {text}")
    return 1 if any(level == "FAIL" for level, _ in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
