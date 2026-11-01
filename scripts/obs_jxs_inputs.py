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
  * `genlock_burn` is never written (the burn sweeps own it), nor any other key;
  * nothing at all is written while OBS streams or records (or when that cannot be read): the owner's
    rule is that a latency never changes on air. At the launch nothing is live yet.

`--verify` prints PASS / FAIL / NOTE rows for verify-strih item 49 (read-only): every declared input
present, of the JPEG XS kind, with the declared settings, plus a report-only NOTE naming the scenes it
is shown in; with `--output-target` (the strih) also the RUNNING program output (`camera-box JPEG XS
program`: exists, active, the declared target + stream id). Pure given an injected obs-websocket
`rpc(request_type, data=None, ignore_err=False)`; main() owns the connection (the strih_scenes.Obs client
installed beside this file).

Pin ownership: the declared pin is the box FACT. A tool that writes another pin onto a JPEG XS input
over obs-websocket (an E2E measurement profile, a hand calibration) is reverted at the next OBS start;
a new value is a fact change (STRIH_JXS_CAMERA_PIN_MS / STRIH_JXS_PROGRAM_PIN_MS).

The camera is the OPERATOR's (owner 1.11.2026, design 6558038959): the input's "Kamera" list (jxs-source.cpp)
writes the jxs_port + jxs_camera PAIR of another declared camera, like picking an NDI source. --bootstrap
keeps such a pair (it heals an undeclared pair and every other key), --verify reports it as a NOTE, and
`--enforce-mapping` (rig-mode TEST and EVENT, next to the strih NDI mapping: the issue-399 parity) puts the
declared 1:1 pair back on every declared input and reads it back, never while OBS streams or records.
"""
import argparse
import collections
import json
import os
import sys

INPUT_KIND = "camera_box_jxs_source"
OUTPUT_NAME = "camera-box JPEG XS program"
MANIFEST_PATH = "/opt/camera-box/jxs-seed.json"
INT_KEYS = ("jxs_port", "jxs_camera", "jxs_max_bpp", "genlock_latency_ms_src")
BOOL_KEYS = ("genlock_fifo", "genlock_grid")
DECLARED_KEYS = INT_KEYS + BOOL_KEYS
# the camera an input shows: the operator's pick (the input's "Kamera" list writes both)
PAIR_KEYS = ("jxs_port", "jxs_camera")

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


def _num(v):
    """An integer setting as int (a JSON 36.0 is 36), else None."""
    if _is_int(v):
        return v
    if isinstance(v, float) and v.is_integer():
        return int(v)
    return None


def declared_pair(spec):
    return (spec.settings["jxs_port"], spec.settings["jxs_camera"])


def pair_of(effective):
    """(port, camera) an input listens on, from its effective settings (None for a non-integer)."""
    eff = effective or {}
    return (_num(eff.get("jxs_port")), _num(eff.get("jxs_camera")))


def camera_label(pair):
    """The input's "Kamera" list label of a pair (jxs-source-decide.h cb_jxs_pick_label)."""
    return f"CAM{pair[1]} (UDP {pair[0]})"


def picked_camera(spec, specs, effective):
    """The OTHER declared spec whose camera this input shows (its pair is that spec's declared pair: the
    operator's pick in the "Kamera" list), else None. Its own declared pair is no pick."""
    have = pair_of(effective)
    if have == declared_pair(spec):
        return None
    for other in specs:
        if other.name != spec.name and declared_pair(other) == have:
            return other
    return None


def plan_input(spec, kinds, effective, specs=()):
    """("create", None) | ("refuse", other_kind) | ("update", diffs) | ("ok", None) for one spec.
    `kinds` = {inputName: inputKind} (GetInputList); `effective` = that input's effective settings;
    `specs` = every declared spec: a pair that names another declared camera is the operator's pick and
    never a diff (without `specs` every key is graded, the pre-pick contract)."""
    if spec.name not in kinds:
        return ("create", None)
    if kinds[spec.name] != INPUT_KIND:
        return ("refuse", kinds[spec.name])
    diffs = setting_diffs(effective, spec.settings)
    if picked_camera(spec, specs, effective) is not None:
        diffs = [d for d in diffs if d[0] not in PAIR_KEYS]
    return ("update", diffs) if diffs else ("ok", None)


def _pick_note(spec, pick):
    return (f"it shows the operator's pick {camera_label(declared_pair(pick))} ({pick.name}'s camera, "
            f"its own is {camera_label(declared_pair(spec))}) -- kept")


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


def obs_live(rpc):
    """True when OBS streams or records, or when either state cannot be read (fail closed): a seeder
    never changes an input while OBS is live (owner rule: no latency change during operation)."""
    for req in ("GetStreamStatus", "GetRecordStatus"):
        resp = rpc(req, {}, ignore_err=True) or {}
        if resp.get("outputActive") is not False:
            return True
    return False


def bootstrap(rpc, specs, live=False):
    """Apply the plan of every spec. Returns (lines, ok): one log line per input, ok False when an input
    was refused. Writes only CreateScene / CreateInput (an absent input, in its holding scene) and
    SetInputSettings (the differing declared keys, overlay); a failed write raises through rpc. With
    `live` (OBS streams or records) nothing is written: every needed write is refused by name."""
    kinds = _kinds(rpc)
    scenes = {s.get("sceneName") for s in (rpc("GetSceneList", {}, ignore_err=True) or {}).get("scenes", []) or []}
    lines, ok = [], True
    for spec in specs:
        eff = _effective(rpc, spec.name) if kinds.get(spec.name) == INPUT_KIND else {}
        action, detail = plan_input(spec, kinds, eff, specs)
        pick = picked_camera(spec, specs, eff) if action in ("update", "ok") else None
        kept = f"; {_pick_note(spec, pick)}" if pick else ""
        if live and action in ("create", "update"):
            ok = False
            lines.append(f"jxs inputs: REFUSED to {action} '{spec.name}' while OBS streams or records -- an input "
                         f"never changes on air; the next OBS start heals it")
            continue
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
            lines.append(f"jxs inputs: healed '{spec.name}': {healed}{kept}")
        else:
            lines.append(f"jxs inputs: '{spec.name}' matches its declared settings{kept}")
    return lines, ok


def _summary(spec, pair=None):
    s = spec.settings
    port, camera = pair or declared_pair(spec)
    bpp = "no limit" if s["jxs_max_bpp"] == 0 else f"max {s['jxs_max_bpp']} bpp"
    return (f"port {port} camera {camera} pin {s['genlock_latency_ms_src']} ms grid "
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
    effs = {spec.name: _effective(rpc, spec.name) for spec in specs if kinds.get(spec.name) == INPUT_KIND}
    rows = []
    for spec in specs:
        eff = effs.get(spec.name, {})
        action, detail = plan_input(spec, kinds, eff, specs)
        pick = picked_camera(spec, specs, eff) if action in ("update", "ok") else None
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
            rows.append(("PASS", f"(jxs-inputs) '{spec.name}': {_summary(spec, declared_pair(pick) if pick else None)}"))
        if pick:
            port = declared_pair(pick)[0]
            also = [n for n, e in effs.items() if n != spec.name and pair_of(e)[0] == port]
            black = (f"; {', '.join(repr(n) for n in also)} listens on UDP {port} too: one port binds one input, "
                     f"the second stays black" if also else "")
            rows.append(("NOTE", f"(jxs-pick) '{spec.name}' shows {camera_label(declared_pair(pick))}, the operator's "
                                 f"pick in its Kamera list (its declared camera is {camera_label(declared_pair(spec))})"
                                 f"{black} -- rig-mode TEST / EVENT puts the declared camera back (report-only, "
                                 f"issue 1387)"))
        shown = _shown_in(rpc, scenes, spec.name)
        where = ", ".join(shown) if shown else "no scene (the operator places it)"
        rows.append(("NOTE", f"(jxs-place) '{spec.name}' is shown in: {where} (report-only, the scenes are "
                             f"the operator's)"))
    return rows


def enforce_mapping(rpc, specs, live=False):
    """Put the declared 1:1 camera back on every declared input: jxs_port + jxs_camera only, ONE overlay of
    the pair keys that differ, then read back (rig-mode TEST and EVENT, next to the strih NDI mapping: the
    issue-399 parity). Never creates an input (the launch seed does), never touches another kind or any
    other key, and writes nothing while OBS streams or records (`live`). Returns (lines, ok): a line per
    input, FAIL-prefixed when the mapping is not proven."""
    kinds = _kinds(rpc)
    lines, ok = [], True
    for spec in specs:
        want = declared_pair(spec)
        if spec.name not in kinds:
            ok = False
            lines.append(f"FAIL jxs mapping: '{spec.name}' is missing -- the launch seed creates it at the next OBS "
                         f"start; the mapping is not proven")
            continue
        if kinds[spec.name] != INPUT_KIND:
            ok = False
            lines.append(f"FAIL jxs mapping: '{spec.name}' is a {kinds[spec.name]}, not a {INPUT_KIND} -- left alone")
            continue
        have = pair_of(_effective(rpc, spec.name))
        if have == want:
            lines.append(f"jxs mapping: '{spec.name}' already on {camera_label(want)}")
            continue
        if live:
            ok = False
            lines.append(f"FAIL jxs mapping: REFUSED to put '{spec.name}' back on {camera_label(want)} (it shows "
                         f"{camera_label(have)}) while OBS streams or records -- an input never changes on air")
            continue
        fix = {k: v for k, v, h in zip(PAIR_KEYS, want, have) if v != h}
        rpc("SetInputSettings", {"inputName": spec.name, "inputSettings": fix, "overlay": True})
        back = pair_of(_effective(rpc, spec.name))
        if back != want:
            ok = False
            lines.append(f"FAIL jxs mapping: '{spec.name}' reads {camera_label(back)} after the write, not "
                         f"{camera_label(want)}")
            continue
        lines.append(f"jxs mapping: put '{spec.name}' back on {camera_label(want)} (it showed {camera_label(have)}), "
                     f"read back")
    return lines, ok


def output_rows(rpc, target, stream_id):
    """[(level, text)] for the strih's RUNNING program output (vendor/distroav/src/jxs-output.cpp creates
    OUTPUT_NAME from jxs-output.json at FINISHED_LOADING): it exists, is active, and runs the declared
    target + stream id. The file alone is not proof: a stop or a WS re-target lives in OBS only."""
    status = rpc("GetOutputStatus", {"outputName": OUTPUT_NAME}, ignore_err=True) or {}
    if "outputActive" not in status:
        return [("FAIL", f"(jxs-output-live) OBS has no output '{OUTPUT_NAME}' -- the strih DistroAV build without "
                         f"the JPEG XS output, or it failed to load (read the OBS log's jxs-output lines) (issue 1387)")]
    if status.get("outputActive") is not True:
        return [("FAIL", f"(jxs-output-live) '{OUTPUT_NAME}' is not running -- the program is not on the JPEG XS link; "
                         f"restart OBS (it starts from jxs-output.json) or StartOutput (issue 1387)")]
    st = (rpc("GetOutputSettings", {"outputName": OUTPUT_NAME}, ignore_err=True) or {}).get("outputSettings") or {}
    have = (st.get("jxs_target"), st.get("jxs_stream_id"))
    if have != (target, stream_id):
        return [("FAIL", f"(jxs-output-live) '{OUTPUT_NAME}' runs to {have[0]} stream id {have[1]}, not {target} stream "
                         f"id {stream_id} -- a WS change jxs-output.json does not carry; restart OBS (issue 1387)")]
    return [("PASS", f"(jxs-output-live) '{OUTPUT_NAME}' running: the program to {target} (stream id {stream_id})")]


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
    ap.add_argument("--output-target", default=None,
                    help="with --verify on a strih: also grade the RUNNING program output against this target")
    ap.add_argument("--output-stream-id", type=int, default=None)
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--bootstrap", action="store_true", help="create / heal the declared inputs")
    mode.add_argument("--verify", action="store_true", help="read-only: print PASS/FAIL/NOTE rows")
    mode.add_argument("--enforce-mapping", action="store_true",
                      help="put the declared camera (jxs_port + jxs_camera) back on every declared input")
    args = ap.parse_args(argv)
    if (args.output_target is None) != (args.output_stream_id is None):
        ap.error("--output-target and --output-stream-id go together")
    specs = []
    try:
        with open(args.manifest) as fh:
            text = fh.read()
    except FileNotFoundError:
        if args.verify:
            print("NOTE (jxs-inputs) this box declares no JPEG XS input (no manifest: STRIH_JXS_CAMERAS / "
                  "STRIH_JXS_PROGRAM_PORT none) -- nothing to grade (issue 1387)")
        elif args.enforce_mapping:
            print("jxs mapping: this box declares no JPEG XS input -- nothing to put back")
            return 0
        else:
            print("jxs inputs: this box declares no JPEG XS input -- nothing to seed")
            return 0
    else:
        try:
            specs = parse_manifest(text)
        except ManifestError as e:
            if not args.verify:
                print(f"jxs inputs: FAIL the manifest {args.manifest} is not valid: {e}", file=sys.stderr)
                return 2
            print(f"FAIL (jxs-inputs) the manifest {args.manifest} is not valid: {e} -- re-run setup-strih.sh "
                  f"step 6 (issue 1387)")
    if args.verify and not specs and args.output_target is None:
        return 0
    try:
        obs = _client(args)
    except Exception as e:  # noqa: BLE001 -- OBS not running is a state to report, never a crash
        if args.verify:
            print(f"NOTE (jxs-inputs) OBS WebSocket {args.host}:{args.port} not reachable ({e}) -- OBS not "
                  f"running? the JPEG XS inputs and output are not graded (issue 1387)")
            return 0
        print(f"jxs inputs: FAIL OBS WebSocket {args.host}:{args.port} not reachable ({e})", file=sys.stderr)
        return 1
    rpc = obs.req
    if args.enforce_mapping:
        lines, ok = enforce_mapping(rpc, specs, live=obs_live(rpc))
        for ln in lines:
            print(ln)
        return 0 if ok else 3
    if args.verify:
        rows = verify_rows(rpc, specs)
        if args.output_target is not None:
            rows += output_rows(rpc, args.output_target, args.output_stream_id)
        for level, text in rows:
            print(f"{level} {text}")
        return 0
    lines, ok = bootstrap(rpc, specs, live=obs_live(rpc))
    for ln in lines:
        print(ln)
    return 0 if ok else 3


if __name__ == "__main__":
    sys.exit(main())
