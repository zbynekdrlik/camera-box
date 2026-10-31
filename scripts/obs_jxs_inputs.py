#!/usr/bin/env python3
"""issue 1387 -- the JPEG XS inputs (camera_box_jxs_source) a strih / stream OBS box's FACTS declare,
seeded at every OBS launch and graded by verify-strih.

Until this module the JPEG XS inputs were hand state (the slice-2/3 runbooks created them over
obs-websocket): their 36 ms camera pin lived only in the running OBS (the saved collection still read
3), and a new box got none. setup-strih step 6 now renders the box's declared inputs from its fact file
into /opt/camera-box/jxs-seed.json (scripts/lib/strih-jxs-provision.sh: `JXS camN src` per
STRIH_JXS_CAMERAS entry on the strih role, `JXS PGM src` on the stream role), and strih-obs-start.sh
runs `--bootstrap` after the strih_scenes seed:

  * an existing input of the JPEG XS kind gets the DECLARED settings healed onto it (update-only:
    one SetInputSettings overlay of the keys that differ; a matching input is a pure read);
  * an existing input of ANOTHER kind under the declared name is never touched (reported);
  * an ABSENT input is created in its own holding scene (`JXS camN` / `JXS PGM`, the slice-2/3 runbook
    shape), never in an operator scene: the scene arrangement (Cam N, Moderatori, MULTIVIEW, PRO) is
    the operator's, and the production scene PRO is never written by a tool;
  * `genlock_burn` is never written (the burn sweeps own it), nor any other key.

`--verify` prints PASS / FAIL / NOTE rows for verify-strih item 49 (read-only): every declared input
present, of the JPEG XS kind, with the declared settings, plus a report-only NOTE naming the scenes it
is shown in. Pure given an injected obs-websocket `rpc(request_type, data=None, ignore_err=False)`;
main() owns the connection (the strih_scenes.Obs client installed beside this file).
"""
import argparse
import collections
import json
import os
import sys

INPUT_KIND = "camera_box_jxs_source"
MANIFEST_PATH = "/opt/camera-box/jxs-seed.json"
INT_KEYS = ("jxs_port", "jxs_camera", "jxs_max_bpp", "genlock_latency_ms_src")
BOOL_KEYS = ("genlock_fifo", "genlock_grid")
DECLARED_KEYS = INT_KEYS + BOOL_KEYS

Spec = collections.namedtuple("Spec", "name scene settings")


class ManifestError(Exception):
    """The manifest is not the shape setup-strih renders: fail loud, never guess an input."""


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def parse_manifest(text):
    """The declared inputs of a jxs-seed.json text, as a list of Spec. Raises ManifestError on any
    shape problem: not an object with an `inputs` list, an entry without a non-empty name / scene, a
    scene equal to its input (OBS refuses it, code 601), settings that are not exactly DECLARED_KEYS
    with int / bool values, a name or a UDP port declared twice."""
    try:
        obj = json.loads(text)
    except ValueError as e:
        raise ManifestError(f"not JSON ({e})") from None
    if not isinstance(obj, dict) or not isinstance(obj.get("inputs"), list):
        raise ManifestError("not an object with an `inputs` list")
    specs, names, ports = [], set(), set()
    for i, ent in enumerate(obj["inputs"]):
        if not isinstance(ent, dict):
            raise ManifestError(f"inputs[{i}] is not an object")
        name, scene, st = ent.get("name"), ent.get("scene"), ent.get("settings")
        if not isinstance(name, str) or not name.strip() or not isinstance(scene, str) or not scene.strip():
            raise ManifestError(f"inputs[{i}] needs a non-empty name and scene")
        if scene == name:
            raise ManifestError(f"inputs[{i}]: the scene must differ from the input name '{name}'")
        if not isinstance(st, dict) or set(st) != set(DECLARED_KEYS):
            raise ManifestError(f"inputs[{i}] '{name}': settings must be exactly {', '.join(DECLARED_KEYS)}")
        for k in INT_KEYS:
            if not _is_int(st[k]):
                raise ManifestError(f"inputs[{i}] '{name}': {k} must be an integer")
        for k in BOOL_KEYS:
            if not isinstance(st[k], bool):
                raise ManifestError(f"inputs[{i}] '{name}': {k} must be true or false")
        if name in names:
            raise ManifestError(f"the input '{name}' is declared twice")
        if st["jxs_port"] in ports:
            raise ManifestError(f"UDP port {st['jxs_port']} is declared twice (one port = one input)")
        names.add(name)
        ports.add(st["jxs_port"])
        specs.append(Spec(name, scene, {k: st[k] for k in DECLARED_KEYS}))
    return specs


def setting_diffs(effective, want):
    """[(key, have, want)] for every declared key whose EFFECTIVE value (the kind's defaults under the
    explicit settings) differs. A bool key must read a bool, an int key an integer (a JSON 36.0 is 36);
    a missing key is a difference."""
    out = []
    for k in DECLARED_KEYS:
        have = (effective or {}).get(k)
        if k in BOOL_KEYS:
            same = isinstance(have, bool) and have == want[k]
        else:
            same = (_is_int(have) or isinstance(have, float)) and have == want[k]
        if not same:
            out.append((k, have, want[k]))
    return out


def plan_input(spec, kinds, effective):
    """("create", None) | ("refuse", other_kind) | ("update", diffs) | ("ok", None) for one spec.
    `kinds` = {inputName: inputKind} (GetInputList); `effective` = that input's effective settings."""
    if spec.name not in kinds:
        return ("create", None)
    if kinds[spec.name] != INPUT_KIND:
        return ("refuse", kinds[spec.name])
    diffs = setting_diffs(effective, spec.settings)
    return ("update", diffs) if diffs else ("ok", None)


def _kinds(rpc):
    resp = rpc("GetInputList", {}, ignore_err=True) or {}
    return {i.get("inputName"): i.get("inputKind") for i in resp.get("inputs", []) or []}


def _effective(rpc, name):
    explicit = (rpc("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings") or {}
    defaults = (rpc("GetInputDefaultSettings", {"inputKind": INPUT_KIND}, ignore_err=True) or {}).get(
        "defaultInputSettings") or {}
    return {**defaults, **explicit}


def _fmt(v):
    return json.dumps(v)


def bootstrap(rpc, specs):
    """Apply the plan of every spec. Returns (lines, ok): one log line per input, ok False when an input
    was refused. Writes only CreateScene / CreateInput (an absent input, in its holding scene) and
    SetInputSettings (the differing declared keys, overlay); a failed write raises through rpc."""
    kinds = _kinds(rpc)
    scenes = {s.get("sceneName") for s in (rpc("GetSceneList", {}, ignore_err=True) or {}).get("scenes", []) or []}
    lines, ok = [], True
    for spec in specs:
        eff = _effective(rpc, spec.name) if kinds.get(spec.name) == INPUT_KIND else {}
        action, detail = plan_input(spec, kinds, eff)
        if action == "create":
            if spec.scene not in scenes:
                rpc("CreateScene", {"sceneName": spec.scene})
                scenes.add(spec.scene)
            rpc("CreateInput", {"sceneName": spec.scene, "inputName": spec.name, "inputKind": INPUT_KIND,
                                "inputSettings": dict(spec.settings), "sceneItemEnabled": True})
            lines.append(f"jxs inputs: created '{spec.name}' in its own scene '{spec.scene}' (the operator "
                         f"places it in the operator scenes)")
        elif action == "refuse":
            ok = False
            lines.append(f"jxs inputs: REFUSED '{spec.name}': it exists as {detail}, not {INPUT_KIND} -- left "
                         f"alone, rename one of them")
        elif action == "update":
            rpc("SetInputSettings", {"inputName": spec.name,
                                     "inputSettings": {k: want for k, _, want in detail}, "overlay": True})
            healed = ", ".join(f"{k} {_fmt(have)} -> {_fmt(want)}" for k, have, want in detail)
            lines.append(f"jxs inputs: healed '{spec.name}': {healed}")
        else:
            lines.append(f"jxs inputs: '{spec.name}' matches its declared settings")
    return lines, ok


def _summary(spec):
    s = spec.settings
    bpp = "no limit" if s["jxs_max_bpp"] == 0 else f"max {s['jxs_max_bpp']} bpp"
    return (f"port {s['jxs_port']} camera {s['jxs_camera']} pin {s['genlock_latency_ms_src']} ms grid "
            f"{'on' if s['genlock_grid'] else 'off'} genlock_fifo {'on' if s['genlock_fifo'] else 'off'} {bpp}")


def _shown_in(rpc, scene_names, name):
    out = []
    for sc in scene_names:
        items = (rpc("GetSceneItemList", {"sceneName": sc}, ignore_err=True) or {}).get("sceneItems", []) or []
        if any(it.get("sourceName") == name and it.get("sceneItemEnabled", True) for it in items):
            out.append(sc)
    return out


def verify_rows(rpc, specs):
    """[(level, text)] for verify-strih item 49's config rows: PASS / FAIL per declared input (present,
    the JPEG XS kind, the declared settings) + a report-only NOTE naming the scenes it is shown in."""
    kinds = _kinds(rpc)
    scenes = [s.get("sceneName") for s in (rpc("GetSceneList", {}, ignore_err=True) or {}).get("scenes", []) or []]
    rows = []
    for spec in specs:
        eff = _effective(rpc, spec.name) if kinds.get(spec.name) == INPUT_KIND else {}
        action, detail = plan_input(spec, kinds, eff)
        if action == "create":
            rows.append(("FAIL", f"(jxs-inputs) '{spec.name}' is missing -- strih-obs-start.sh creates it in the "
                                 f"scene '{spec.scene}' at the next OBS start (or obs_jxs_inputs.py --bootstrap "
                                 f"now), then the operator places it (issue 1387)"))
            continue
        if action == "refuse":
            rows.append(("FAIL", f"(jxs-inputs) '{spec.name}' is a {detail}, not a {INPUT_KIND} -- rename one "
                                 f"of them (issue 1387)"))
            continue
        if action == "update":
            bad = ", ".join(f"{k} {_fmt(have)} != {_fmt(want)}" for k, have, want in detail)
            rows.append(("FAIL", f"(jxs-inputs) '{spec.name}': {bad} -- the launch seed heals it at the next "
                                 f"OBS start, or obs_jxs_inputs.py --bootstrap now (issue 1387)"))
        else:
            rows.append(("PASS", f"(jxs-inputs) '{spec.name}': {_summary(spec)}"))
        shown = _shown_in(rpc, scenes, spec.name)
        where = ", ".join(shown) if shown else "no scene (the operator places it)"
        rows.append(("NOTE", f"(jxs-place) '{spec.name}' is shown in: {where} (report-only, the scenes are "
                             f"the operator's)"))
    return rows


def _client(args):
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import strih_scenes  # the minimal OBS-WS client installed beside this file
    return strih_scenes.Obs(args.host, args.port, args.password)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD") or None)
    ap.add_argument("--manifest", default=MANIFEST_PATH)
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--bootstrap", action="store_true", help="create / heal the declared inputs")
    mode.add_argument("--verify", action="store_true", help="read-only: print PASS/FAIL/NOTE rows")
    args = ap.parse_args(argv)
    try:
        with open(args.manifest) as fh:
            text = fh.read()
    except FileNotFoundError:
        if args.verify:
            print("NOTE (jxs-inputs) this box declares no JPEG XS input (no manifest: STRIH_JXS_CAMERAS / "
                  "STRIH_JXS_PROGRAM_PORT none) -- nothing to grade (issue 1387)")
        else:
            print("jxs inputs: this box declares no JPEG XS input -- nothing to seed")
        return 0
    try:
        specs = parse_manifest(text)
    except ManifestError as e:
        if args.verify:
            print(f"FAIL (jxs-inputs) the manifest {args.manifest} is not valid: {e} -- re-run setup-strih.sh "
                  f"step 6 (issue 1387)")
            return 0
        print(f"jxs inputs: FAIL the manifest {args.manifest} is not valid: {e}", file=sys.stderr)
        return 2
    try:
        obs = _client(args)
    except Exception as e:  # noqa: BLE001 -- OBS not running is a state to report, never a crash
        if args.verify:
            print(f"NOTE (jxs-inputs) OBS WebSocket {args.host}:{args.port} not reachable ({e}) -- OBS not "
                  f"running? the JPEG XS inputs are not graded (issue 1387)")
            return 0
        print(f"jxs inputs: FAIL OBS WebSocket {args.host}:{args.port} not reachable ({e})", file=sys.stderr)
        return 1
    rpc = obs.req
    if args.verify:
        for level, text in verify_rows(rpc, specs):
            print(f"{level} {text}")
        return 0
    lines, ok = bootstrap(rpc, specs)
    for ln in lines:
        print(ln)
    return 0 if ok else 3


if __name__ == "__main__":
    sys.exit(main())
