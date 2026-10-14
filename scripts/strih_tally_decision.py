"""issue 1417: which camera a strih scene lights on the tally lights. Pure, no I/O.

Owner rule (12.10.2026): a scene lights camera n when its enabled content, nested scenes and groups
included, holds exactly ONE distinct camera n; anything else (no camera, several) lights nothing (0).
A camera is identified by the NDI source name `CAM<n> (...)` of an ndi_source input, never by the
OBS input name.

The service (scripts/strih_tally.py) builds the inputs from obs-websocket:
  * tree: scene OR group name -> its items, each {"name", "kind": "scene"|"group"|"input", "enabled"};
  * cameras: input name -> camera number, only for inputs whose NDI source is a camera.
"""
import re

_CAM = re.compile(r"^CAM(\d+) \(")


def camera_of_ndi_name(ndi_name):
    """`CAM<n> (...)` -> n, anything else (a program feed, a CG sender, None) -> 0."""
    m = _CAM.match(ndi_name or "")
    return int(m.group(1)) if m else 0


def cameras_in(scene, tree, cameras, _seen=None):
    """The set of distinct cameras the ENABLED content of `scene` holds, recursively.

    One `seen` set is shared across siblings on purpose: a scene reached twice holds the same
    cameras (counted once), and a scene cycle stops.
    """
    seen = set() if _seen is None else _seen
    if scene is None or scene in seen:
        return set()
    seen.add(scene)
    found = set()
    for it in tree.get(scene, []):
        if not it.get("enabled", True):
            continue
        if it["kind"] in ("scene", "group"):
            found |= cameras_in(it["name"], tree, cameras, seen)
        elif cameras.get(it["name"], 0):
            found.add(cameras[it["name"]])
    return found


def camera_of_scene(scene, tree, cameras):
    """Exactly one distinct camera -> it, else 0."""
    found = cameras_in(scene, tree, cameras)
    return next(iter(found)) if len(found) == 1 else 0


def state_of(program, preview, studio, tree, cameras):
    """(pgm, pvw). Studio mode off -> no preview (0)."""
    pvw = camera_of_scene(preview, tree, cameras) if studio else 0
    return (camera_of_scene(program, tree, cameras), pvw)


def full_messages(state):
    """The whole state, preview first: the firmware keeps the previous program camera on PGM."""
    pgm, pvw = state
    return [f"TALLY-PVW {pvw}", f"TALLY-PGM {pgm}"]


def messages(prev, new):
    """The texts for a state change, TALLY-PVW before TALLY-PGM; nothing when unchanged."""
    if prev is None:
        return full_messages(new)
    out = []
    if prev[1] != new[1]:
        out.append(f"TALLY-PVW {new[1]}")
    if prev[0] != new[0]:
        out.append(f"TALLY-PGM {new[0]}")
    return out


# The Poprad tally lights (owner 14.10.2026) run a firmware build that dials tally.lan and reads the
# 2ME form `TALLY-PVW2 <n>` / `TALLY-PGM2 <n>` (Companion there sends that form); the firmware in
# tally/esp reads `TALLY-PVW <n>` / `TALLY-PGM <n>` and ignores the other. Every PVW / PGM frame goes
# out in both forms, each right after its plain twin, so PVW still precedes PGM in both.
ME2_ALIASES = {"TALLY-PVW": "TALLY-PVW2", "TALLY-PGM": "TALLY-PGM2"}


def with_me2_aliases(texts):
    """Each `TALLY-PVW n` / `TALLY-PGM n` followed by its 2ME twin; every other text unchanged."""
    out = []
    for text in texts:
        out.append(text)
        cmd, sep, arg = text.partition(" ")
        if sep and cmd in ME2_ALIASES:
            out.append(f"{ME2_ALIASES[cmd]} {arg}")
    return out
