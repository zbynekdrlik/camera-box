#!/usr/bin/env python3
"""#1302 — the CG_CHAIN=1 E2E profile's OBS scene helper (cg OBS program + ONE strih CG window).

Two scene changes let the SongPlayer-originated content reach every hop the verdict judges
(SongPlayer -> cg OBS (RESOLUME-SNV) -> strih -> stream):

  program     cut cg OBS program to the scene that carries the SongPlayer output (``sp-fast``)
              BEFORE its StartRecord, so the cg OBS recording carries the SongPlayer burn.
  strih-solo  the ONE tail CG window on strih: find the ONE scene that carries the ``CG-obs`` NDI
              input, show ONLY that item for the window (on the live rig the item is disabled and
              a ``CG-presenter`` browser overlay sits on top of it), then cut program to it, so the
              strih and stream recordings carry the CG chain, and check the cut scene is not black.
  restore     put everything a snapshot recorded back (program scene first, then each item's
              enabled state, then the previous transition) and retire the snapshot, so a second
              cleanup() pass is a no-op.

Every program cut runs under the box's CUT transition (switched to it, the previous one
snapshotted and restored last): a Fade would blend the old program into the new one, and the
blended frames would read as undecodable in the recordings (cg OBS runs a 2 s Fade live).

Every mutating subcommand WRITES its restore snapshot to ``--state-file`` BEFORE it changes
anything, so an abort mid-change is still restored by cleanup() (the #246/#844 leak-guard class).
A missing scene / input fails LOUD (exit 2) and changes nothing — never a silent wrong cut.

The pure decisions (which scene, which items change, which restore calls) take plain data; the WS
glue takes an ``rpc(rtype, rdata)`` callable, so both are pytest-Tier-0 testable with no OBS
(tests/python/test_cg_chain_scene_1302.py). The WebSocket transport is obs_phase2's own
``_conn``/``_rpc`` (the #328 bounded request loop), never a second client.
"""
import argparse
import json
import os
import sys
import time


# ---- pure decisions -----------------------------------------------------------------------------


def scenes_carrying_input(items_by_scene, input_name):
    """The scenes (in the given order) whose item list contains a source named ``input_name``."""
    return [
        scene
        for scene, items in items_by_scene.items()
        if any(i.get("sourceName") == input_name for i in items)
    ]


def choose_scene(candidates, input_name, override=""):
    """The ONE scene to cut to. Fails loud on none, and on more than one unless ``override`` names
    one of them — never a guess between two scenes."""
    if override:
        if override not in candidates:
            raise ValueError(
                f"scene {override!r} does not carry input {input_name!r} "
                f"(scenes that do: {candidates or 'none'})"
            )
        return override
    if not candidates:
        raise ValueError(f"no scene carries input {input_name!r}")
    if len(candidates) > 1:
        raise ValueError(
            f"more than one scene carries input {input_name!r} ({candidates}) — "
            f"name one with CG_CHAIN_STRIH_SCENE"
        )
    return candidates[0]


def solo_plan(items, input_name):
    """The ``(sceneItemId, enabled)`` changes that show ONLY ``input_name`` in this scene. Items
    already in the wanted state are left out. Fails loud when the input is not in the scene."""
    if not any(i.get("sourceName") == input_name for i in items):
        raise ValueError(f"input {input_name!r} is not in the scene")
    plan = []
    for i in items:
        want = i.get("sourceName") == input_name
        if bool(i.get("sceneItemEnabled")) != want:
            plan.append((int(i["sceneItemId"]), want))
    return plan


def snapshot_items(items):
    """Every item's current enabled state, for the restore."""
    return [{"id": int(i["sceneItemId"]), "enabled": bool(i.get("sceneItemEnabled"))} for i in items]


def cut_transition_name(listing):
    """The name of the box's cut transition (kind ``cut_transition``) in a GetSceneTransitionList
    answer — found by KIND, since the name can be localized. Fails loud when there is none."""
    for t in listing.get("transitions") or []:
        if t.get("transitionKind") == "cut_transition":
            return t["transitionName"]
    raise ValueError("no cut transition on this box — a program cut would blend frames")


def restore_calls(state):
    """The WS requests that undo a snapshot: the previous program scene first, then each item's
    recorded enabled state, then the previous transition (so the restore itself still cuts)."""
    calls = []
    if state.get("prev_program"):
        calls.append(("SetCurrentProgramScene", {"sceneName": state["prev_program"]}))
    for item in state.get("items") or []:
        calls.append(
            (
                "SetSceneItemEnabled",
                {
                    "sceneName": state["scene"],
                    "sceneItemId": item["id"],
                    "sceneItemEnabled": item["enabled"],
                },
            )
        )
    if state.get("prev_transition"):
        calls.append(("SetCurrentSceneTransition", {"transitionName": state["prev_transition"]}))
    return calls


# ---- WS glue (rpc = callable(rtype, rdata=None) -> responseData) --------------------------------


def _write_state(path, state):
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f)
    os.replace(tmp, path)


def _items_by_scene(rpc):
    scenes = [s["sceneName"] for s in rpc("GetSceneList")["scenes"]]
    return {s: rpc("GetSceneItemList", {"sceneName": s})["sceneItems"] for s in scenes}


def _read_cut_context(rpc):
    """(previous program scene, previous transition, the box's cut transition) — all READ before
    any change, so the snapshot can carry them."""
    prev_program = rpc("GetCurrentProgramScene").get("currentProgramSceneName", "")
    prev_transition = rpc("GetCurrentSceneTransition").get("transitionName", "")
    cut = cut_transition_name(rpc("GetSceneTransitionList"))
    return prev_program, prev_transition, cut


def _cut_program(rpc, scene, prev_transition, cut):
    """Cut program to ``scene`` under the cut transition (switching to it only when needed)."""
    if prev_transition != cut:
        rpc("SetCurrentSceneTransition", {"transitionName": cut})
    rpc("SetCurrentProgramScene", {"sceneName": scene})


def strih_solo(rpc, host, input_name, override, state_path, after_cut=None):
    """Show ONLY ``input_name`` in the one scene carrying it and cut program to that scene under the
    Cut transition. ``after_cut(scene)`` runs right after the cut (the CLI passes the obs_phase2
    non-black check). Returns the scene name. Snapshot written before the first change."""
    by_scene = _items_by_scene(rpc)
    scene = choose_scene(scenes_carrying_input(by_scene, input_name), input_name, override)
    items = by_scene[scene]
    plan = solo_plan(items, input_name)
    prev_program, prev_transition, cut = _read_cut_context(rpc)
    _write_state(
        state_path,
        {
            "host": host,
            "scene": scene,
            "prev_program": prev_program,
            "prev_transition": prev_transition,
            "items": snapshot_items(items),
        },
    )
    for item_id, enabled in plan:
        rpc(
            "SetSceneItemEnabled",
            {"sceneName": scene, "sceneItemId": item_id, "sceneItemEnabled": enabled},
        )
    _cut_program(rpc, scene, prev_transition, cut)
    if after_cut is not None:
        after_cut(scene)
    return scene


def program_select(rpc, host, scene, state_path):
    """Cut program to ``scene`` under the Cut transition (fails loud if it does not exist).
    Snapshot written first."""
    scenes = [s["sceneName"] for s in rpc("GetSceneList")["scenes"]]
    if scene not in scenes:
        raise ValueError(f"scene {scene!r} does not exist on {host} (scenes: {scenes})")
    prev_program, prev_transition, cut = _read_cut_context(rpc)
    _write_state(
        state_path,
        {
            "host": host,
            "scene": None,
            "prev_program": prev_program,
            "prev_transition": prev_transition,
            "items": [],
        },
    )
    _cut_program(rpc, scene, prev_transition, cut)


def restore(rpc_for_host, state_path):
    """Undo a snapshot, then retire it (renamed ``.restored``). Returns False when there is no
    snapshot (already restored / never taken) — a second cleanup() pass changes nothing."""
    if not os.path.exists(state_path):
        return False
    with open(state_path, encoding="utf-8") as f:
        state = json.load(f)
    rpc = rpc_for_host(state["host"])
    for rtype, rdata in restore_calls(state):
        rpc(rtype, rdata)
    os.replace(state_path, f"{state_path}.restored")
    return True


# ---- CLI -----------------------------------------------------------------------------------------


def build_parser():
    ap = argparse.ArgumentParser(description="#1302 CG_CHAIN scene helper (cg program + strih CG window)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    solo = sub.add_parser("strih-solo", help="show ONLY the CG input in its scene and cut to it")
    solo.add_argument("--host", required=True)
    solo.add_argument("--input", required=True)
    solo.add_argument("--scene", default="", help="pick this scene when several carry the input")
    solo.add_argument("--state-file", required=True)
    prog = sub.add_parser("program", help="cut program to a scene (snapshot the previous one)")
    prog.add_argument("--host", required=True)
    prog.add_argument("--scene", required=True)
    prog.add_argument("--state-file", required=True)
    rest = sub.add_parser("restore", help="undo a snapshot written by strih-solo / program")
    rest.add_argument("--state-file", required=True)
    return ap


def _obs_phase2():
    """obs_phase2 (the ONE bounded WS client), imported lazily so the pure half never needs
    websocket-client."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import obs_phase2  # noqa: E402  (lazy: pure tests never import the WS client)

    return obs_phase2


def _ws_session(host):
    """(ws, rpc) over obs_phase2's own bounded WS client (no password: the rig boxes run OBS-WS
    without auth). The caller closes ``ws``."""
    o = _obs_phase2()
    ws = o._conn(host, os.environ.get("CG_CHAIN_OBS_PASSWORD", ""))
    return ws, (lambda rtype, rdata=None: o._rpc(ws, rtype, rdata))


def _nonblack_check(ws, host):
    """The obs_phase2 polled non-black check for the CG window's cut scene. min_mean=0 = peak-only:
    SongPlayer content (an idle song frame) can legitimately be dark; a dead CG-obs input is not."""
    def check(scene):
        _obs_phase2()._assert_program_nonblack(
            ws, host, scene, "#1302 CG window",
            "The cg OBS -> strih CG-obs input renders nothing; the strih/stream CG hops will be empty.",
            min_mean=0,
        )
    return check


def main(argv=None):
    a = build_parser().parse_args(argv)
    opened = []

    def session(host):
        ws, rpc = _ws_session(host)
        opened.append(ws)
        return ws, rpc

    try:
        if a.cmd == "strih-solo":
            ws, rpc = session(a.host)
            scene = strih_solo(
                rpc, a.host, a.input, a.scene, a.state_file, after_cut=_nonblack_check(ws, a.host))
            # The cut instant on dev1's CLOCK_REALTIME (the burn gen_ts_ns timeline) is taken AFTER
            # the non-black check, so it can only be LATE (never inside the camera part), then the
            # scene.
            print(f"{time.time_ns()}\t{scene}")
        elif a.cmd == "program":
            program_select(session(a.host)[1], a.host, a.scene, a.state_file)
            print(a.scene)
        else:
            if restore(lambda host: session(host)[1], a.state_file):
                print(f"restored {a.state_file}")
            else:
                print(f"nothing to restore ({a.state_file} absent)")
    except ValueError as e:
        sys.stderr.write(f"[cg_chain_scene] {e}\n")
        return 2
    finally:
        for ws in opened:
            ws.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
