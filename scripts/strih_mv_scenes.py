#!/usr/bin/env python3
"""strih OBS per-camera low-bandwidth multiview twin scenes (#730).

**Repeatedly requested by the user**: strih never got the optimized per-camera multiview scenes
imag-nb already has ("MV Cam N" — dedicated low-cost thumbnail scenes feeding the multiview,
instead of the full program-grade sources rendering in the multiview grid, #501). This script
replicates that pattern on strih over OBS WebSocket.

strih already has 7 full-bandwidth camera scenes "Cam 1".."Cam 7" (#753, 2026-07-14: cam7 is a
NEW, direct/non-inverted pin — its scene/input share the same "7"), each wrapping ONE NDI input
"NDI cam<n>" bound to a real fleet NDI source (#753 1:1 mapping since 2026-07-14: "NDI cam<n>"
carries "CAM<n> (usb)" for every n — the pre-2026-07-14 INVERTED offset, e.g. "NDI cam1"→"CAM3
(usb)", is HISTORY; the canonical fact table is set-ndi-mapping.py's FULL_MAP). This script
NEVER hardcodes that mapping: it reads each existing input's LIVE `ndi_source_name` over WS and
wraps that EXACT same value in a new "MV Cam <n>" twin input, `genlock_monitor=true` (the #501
pattern — the vendored DistroAV genlock lockdown forces LOW-bandwidth NDI receive, ~9x cheaper,
for a source flagged this way). The real "Cam N" inputs/scenes are never modified.

Two independent render-cost sinks get wired to the cheap twins (both cost full-bandwidth decode
for every tile today, live-verified via GetStats before/after — see the genlock skill #730 note):

  1. The BUILT-IN OBS Multiview PROJECTOR (the one the #276/#278/#293 render-budget decouple
     hardened) — via the per-scene `show_in_multiview` private setting (mirrors OBSBasic_Scenes
     .cpp's "ShowInMultiview" context-menu action): real "Cam N" -> hidden, "MV Cam N" -> shown.
  2. strih's own hand-built "Multiview" SCENE (a plain scene whose scene items are references to
     other scenes — NOT the built-in projector) — its items that reference a real "Cam N" scene
     are swapped for the matching "MV Cam N" twin, preserving the EXACT scene-item transform
     (position/scale/bounds) so the operator's layout never visibly changes.

Respects decouple-dont-rebuild (#508): this only re-points OBS's OWN existing scene/source
mechanisms at cheaper feeds — no custom renderer, no new multiview mechanism.

Usage:
  strih_mv_scenes.py --host 10.77.9.202 --password PW               # seed the twins + rewire
  strih_mv_scenes.py --host 10.77.9.202 --password PW --stats 15    # ad-hoc GetStats before/after
                                                                      # render-cost delta (no seed)
"""

import argparse
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2 as op  # reuse the repo's ONE obs-websocket client (_conn/_rpc) — never a 4th one

CAMS = range(1, 8)  # #753 (2026-07-14): fleet growth 6->7, cam7 is real + provisioned now
_CAM_SCENE_RE = re.compile(r"^Cam (\d+)$")

# #795/#759 — reattach() sentinel: the input HAS a bound ndi_source_name to re-apply, but that name
# was NOT present in the DistroAV finder list after the bounded wait (an empty list, or a non-empty
# list that does not offer THIS source — the sender is not currently discoverable), so the re-apply
# was SKIPPED. SetInputSettings with a name absent from the combo's live item list MANGLES it (event
# review 2026-07-18: "mangles names when OBS's NDI finder list is empty"). Distinct from None (input
# missing / never seeded), so a caller — especially the WARN-only #759 cleanup path — can tell
# "skipped to avoid mangling" from "no name to re-apply".
NDI_SOURCE_NOT_DISCOVERABLE = object()

# obs-websocket v5 SceneItemTransform: only these fields are SETTABLE via SetSceneItemTransform.
# GetSceneItemList also returns read-only computed fields (width/height/sourceWidth/sourceHeight)
# that must be stripped before echoing a transform back, or the request can be rejected/ignored.
_SETTABLE_TRANSFORM_FIELDS = frozenset({
    "positionX", "positionY", "rotation", "scaleX", "scaleY", "alignment",
    "boundsType", "boundsAlignment", "boundsWidth", "boundsHeight",
    "cropLeft", "cropTop", "cropRight", "cropBottom",
})


# --- PURE functions (no network — unit-tested from tests/python/test_strih_mv_scenes.py) --------

def is_cam_scene(name: str) -> bool:
    """True for strih's real per-camera scene names ('Cam 1'..'Cam N'), false for anything else
    (utility/overlay scenes like 'Control', 'Multiview' itself, 'PHASE2-PROBE', ...)."""
    return bool(_CAM_SCENE_RE.match(name))


def mv_scene_name(cam_scene_name: str) -> str:
    """'Cam 5' -> 'MV Cam 5'. Raises on a non-'Cam N' input — callers must filter with
    is_cam_scene() first; silently mangling an unrelated scene name is worse than failing loud."""
    if not is_cam_scene(cam_scene_name):
        raise ValueError(f"not a 'Cam N' scene name: {cam_scene_name!r}")
    return f"MV {cam_scene_name}"


def cam_input_name(cam_scene_name: str) -> str:
    """'Cam 5' -> 'NDI cam5' — strih's existing per-camera input-naming convention."""
    m = _CAM_SCENE_RE.match(cam_scene_name)
    if not m:
        raise ValueError(f"not a 'Cam N' scene name: {cam_scene_name!r}")
    return f"NDI cam{m.group(1)}"


def settable_transform_fields(transform: dict) -> dict:
    """Strip a GetSceneItemList-returned sceneItemTransform down to the fields
    SetSceneItemTransform actually accepts (drops read-only computed fields like width/height)."""
    return {k: v for k, v in (transform or {}).items() if k in _SETTABLE_TRANSFORM_FIELDS}


def mv_replacement_plan(scene_items: list) -> list:
    """Given the custom 'Multiview' scene's GetSceneItemList sceneItems, return the plan of which
    items to swap: every item that is itself a real 'Cam N' scene reference gets paired with its
    low-bandwidth twin name + a settable-only copy of its current transform + its sceneItemId (so
    the caller can add the new item at the same spot, then remove the old one). Any item that is
    NOT a 'Cam N' scene (an overlay, a utility source, anything the operator added by hand) is left
    OUT of the plan untouched — this script only ever touches the camera tiles it owns."""
    plan = []
    for item in scene_items:
        name = item.get("sourceName", "")
        if not is_cam_scene(name):
            continue
        plan.append({
            "old_name": name,
            "new_name": mv_scene_name(name),
            "old_item_id": item["sceneItemId"],
            "transform": settable_transform_fields(item.get("sceneItemTransform", {})),
        })
    return plan


def stats_delta(before: dict, after: dict) -> dict:
    """Pure GetStats before/after -> a render-cost delta report. renderSkippedFrames/
    renderTotalFrames are the RENDER health signal (obs-render-health-metric.md) — the encoder-side
    outputSkippedFrames stays green even when the render loop chokes, so both are reported."""
    d_render_skip = after["renderSkippedFrames"] - before["renderSkippedFrames"]
    d_render_total = after["renderTotalFrames"] - before["renderTotalFrames"]
    d_out_skip = after["outputSkippedFrames"] - before["outputSkippedFrames"]
    d_out_total = after["outputTotalFrames"] - before["outputTotalFrames"]
    return {
        "activeFps": after.get("activeFps"),
        "averageFrameRenderTime": after.get("averageFrameRenderTime"),
        "renderSkipped_delta": d_render_skip,
        "renderTotal_delta": d_render_total,
        "renderSkip_pct": round(100.0 * d_render_skip / d_render_total, 2) if d_render_total else 0.0,
        "outputSkipped_delta": d_out_skip,
        "outputTotal_delta": d_out_total,
    }


# --- live (WS) functions --------------------------------------------------------------------

def seed(obs) -> tuple:
    """Create/refresh the 'MV Cam N' twin input+scene for every real 'Cam N' scene strih actually
    has, wrapping each twin around the SAME live ndi_source_name the real input already uses.
    Idempotent — CreateScene/CreateInput 'already exists' errors are ignored, settings are
    re-applied every run (self-healing, same philosophy as the genlock lockdown)."""
    video = op._rpc(obs, "GetVideoSettings")
    cw, ch = video["baseWidth"], video["baseHeight"]
    scenes = [s["sceneName"] for s in op._rpc(obs, "GetSceneList")["scenes"]]

    created, skipped = [], []
    for n in CAMS:
        cam_scene = f"Cam {n}"
        if cam_scene not in scenes:
            skipped.append(cam_scene)
            continue
        cam_input = cam_input_name(cam_scene)
        settings = op._rpc(obs, "GetInputSettings", {"inputName": cam_input}, ignore_err=True)
        ndi_name = (settings or {}).get("inputSettings", {}).get("ndi_source_name")
        if not ndi_name:
            skipped.append(cam_scene)
            continue

        mv_scene = mv_scene_name(cam_scene)
        mv_input = f"MV {cam_input}"
        op._rpc(obs, "CreateScene", {"sceneName": mv_scene}, ignore_err=True)
        twin_settings = {"ndi_source_name": ndi_name, "latency": 1, "genlock_monitor": True}
        op._rpc(obs, "CreateInput", {
            "sceneName": mv_scene, "inputName": mv_input, "inputKind": "ndi_source",
            "inputSettings": twin_settings,
        }, ignore_err=True)
        # re-apply every run (self-healing) — mirrors imag_scenes.py's own convention.
        op._rpc(obs, "SetInputSettings",
                {"inputName": mv_input, "inputSettings": twin_settings}, ignore_err=True)
        op._rpc(obs, "SetInputMute", {"inputName": mv_input, "inputMuted": True}, ignore_err=True)
        item = op._rpc(obs, "GetSceneItemId",
                        {"sceneName": mv_scene, "sourceName": mv_input}, ignore_err=True)
        if item.get("sceneItemId") is not None:
            op._rpc(obs, "SetSceneItemTransform", {
                "sceneName": mv_scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
                    "boundsWidth": cw, "boundsHeight": ch, "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

        # #501 built-in-multiview membership: hide the full-bw real scene, show the cheap twin.
        op._rpc(obs, "SetSourcePrivateSettings", {
            "sourceName": cam_scene, "sourceSettings": {"show_in_multiview": False},
        }, ignore_err=True)
        op._rpc(obs, "SetSourcePrivateSettings", {
            "sourceName": mv_scene, "sourceSettings": {"show_in_multiview": True},
        }, ignore_err=True)
        created.append(mv_scene)

    return created, skipped


def rewire_multiview_scene(obs, multiview_scene: str = "Multiview") -> list:
    """Swap the custom 'Multiview' scene's 'Cam N' item references for their 'MV Cam N' twins,
    preserving each item's exact transform. Adds the new item BEFORE removing the old one, so the
    live production output never drops to fewer tiles than before mid-operation (hot-apply, no OBS
    restart — strih is production). No-op if the box has no such scene."""
    scenes = [s["sceneName"] for s in op._rpc(obs, "GetSceneList")["scenes"]]
    if multiview_scene not in scenes:
        return []
    items = op._rpc(obs, "GetSceneItemList", {"sceneName": multiview_scene})["sceneItems"]
    plan = mv_replacement_plan(items)

    rewired = []
    for entry in plan:
        op._rpc(obs, "CreateSceneItem", {
            "sceneName": multiview_scene, "sourceName": entry["new_name"],
            "sceneItemEnabled": True,
        }, ignore_err=True)
        new_item = op._rpc(obs, "GetSceneItemId", {
            "sceneName": multiview_scene, "sourceName": entry["new_name"],
        }, ignore_err=True)
        if new_item.get("sceneItemId") is not None and entry["transform"]:
            op._rpc(obs, "SetSceneItemTransform", {
                "sceneName": multiview_scene, "sceneItemId": new_item["sceneItemId"],
                "sceneItemTransform": entry["transform"],
            }, ignore_err=True)
        op._rpc(obs, "RemoveSceneItem", {
            "sceneName": multiview_scene, "sceneItemId": entry["old_item_id"],
        }, ignore_err=True)
        rewired.append(entry["new_name"])
    return rewired


def measure_stats(obs, seconds: float) -> dict:
    """Live wrapper around stats_delta(): sample GetStats now, wait `seconds`, sample again."""
    before = op._rpc(obs, "GetStats")
    time.sleep(seconds)
    after = op._rpc(obs, "GetStats")
    return stats_delta(before, after)


def _baseline_sender_for(input_name):
    """#1158: the CANONICAL #399 baseline NDI sender for a strih input (e.g. 'NDI cam3' ->
    'CAM3 (usb)'), or None if it is not in the mapping fact table. Delegates to set-ndi-mapping.py's
    FULL_MAP (the SINGLE source of truth) via a lazy importlib load — never a hardcoded
    'CAM{N} (usb)' duplicate here that could drift from #399. Lazy because it is used ONLY on the
    rare reattach vanished-branch, and set-ndi-mapping.py imports websocket lazily so this stays
    import-light."""
    import importlib.util
    import pathlib
    p = pathlib.Path(__file__).resolve().parent / "set-ndi-mapping.py"
    spec = importlib.util.spec_from_file_location("set_ndi_mapping_1158", p)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.baseline_sender_for(input_name)


def reattach(obs, cam_n: int, *, finder_retries: int = 6, finder_wait_s: float = 1.0,
             reset_settle_s: float = 0.25, sleep=time.sleep):
    """#758 item 2 — sender-bounce re-attach: re-read an input's OWN current ndi_source_name and
    force OBS to tear down and re-establish its DistroAV NDI receive for that source via a
    CLEAR-then-SET of ndi_source_name (issue 1114). After a [2/8]/[2b/8] service->burn-unit
    swap (or during a cleanup restore), a camera's NDI sender can come back up with a receiver
    that never re-locks on its own — this nudges the input's OWN bound source (never inventing a
    new one) to reconnect. Returns the ndi_source_name that was re-applied, or None if the input
    doesn't exist / has no ndi_source_name set (caller then treats this as "cannot re-attach,
    still dead -> fail loud", never silently invents a fallback source name).

    issue 1114 (E2E burn-deploy handover race): re-applying the SAME ndi_source_name via
    SetInputSettings is a NO-OP for the receiver — vendored ndi_source_update() computes
    reset_ndi_receiver from a NAME CHANGE (safe_strcmp(config.ndi_source_name, new) != 0), so an
    unchanged name leaves reset_ndi_receiver=false and (the receiver thread being alive after the
    issue-1096 retry-in-place) the update does nothing. The receiver stays stuck on the DEAD
    pre-bounce sender until the passive ~2min fresh-finder timer, which the [2/8] ~52s reverify
    budget never covers → false "camera leg dead" + the heavy issue-1093 strih-OBS force-kill. The
    cure is a CLEAR-then-SET: first SetInputSettings {ndi_source_name: ""} (the empty-name branch
    of ndi_source_update ALWAYS calls ndi_source_thread_stop, behaviour-independent → s->running
    =false, the stuck receiver torn down cleanly), settle one render tick, THEN set it back to the
    real name (thread not running → ndi_source_thread_start under the KEEP_ACTIVE default, which
    sets reset_ndi_receiver=true → a FRESH receiver thread whose issue-1096 fresh finder resolves
    the live post-bounce sender BY URL). This is the same clear-then-set idle discipline
    obs_phase2._quiesce_probe_input uses, and the targeted per-input equivalent of the issue-1093
    OBS force-kill — without killing the operator's whole OBS.

    #761 (2026-07-15, user-directed, KEPT): strih's "MV Cam N" scenes were switched to
    SAME-SOURCE — they now render the MAIN "NDI camN" input, and the old "MV NDI camN"
    low-bandwidth clone items are DISABLED in those scenes. The sender-bounce liveness probe
    this function backs (recording-e2e.sh's preflight_mv_reverify) was switched to match — it
    now checks the MAIN "NDI camN" input's liveness, so this reattach must target the SAME
    input the probe actually checks (re-attaching the now-unused, disabled clone would fix
    nothing the probe cares about). Targets `f"NDI cam{cam_n}"` — the main, always-rendered
    input (per #761's own reasoning: it's continuously shown via the built-in OBS Multiview
    grid projector, so a stuck receiver here is a genuine sender-bounce symptom, same as it
    always was for the clone).

    #795/#759 (event review 2026-07-18): re-applying ndi_source_name via SetInputSettings MANGLES
    the value whenever the target name is absent from OBS's DistroAV finder list — an empty list, OR
    a non-empty list that does not (yet) offer THIS source because its sender is still bouncing. So
    before re-applying, wait (bounded: finder_retries x finder_wait_s) for the bound name to APPEAR
    in the finder list; if it never does, SKIP the set entirely — leave the input bound as-is — and
    return the NDI_SOURCE_NOT_DISCOVERABLE sentinel. This matters most for the WARN-only cleanup()
    reattach (#759): it never fails the run loud, so a mangled name here would silently point a
    camera leg at garbage until the next run's [0/8] preflight caught it."""
    input_name = f"NDI cam{cam_n}"
    settings = op._rpc(obs, "GetInputSettings", {"inputName": input_name}, ignore_err=True)
    ndi_name = (settings or {}).get("inputSettings", {}).get("ndi_source_name")
    if not ndi_name:
        return None
    # #795/#759: only re-apply once the bound name is actually PRESENT in the finder list (mere
    # non-emptiness is not enough — a list lacking THIS name would still mangle it on set).
    for attempt in range(max(1, finder_retries)):
        if ndi_name in op._ndi_source_list(obs, input_name):
            break
        if attempt < finder_retries - 1:
            sleep(finder_wait_s)
    else:
        return NDI_SOURCE_NOT_DISCOVERABLE
    # issue 1114: CLEAR the name to "" (→ ndi_source_thread_stop: tears the stuck receiver down
    # cleanly, s->running=false), settle one render tick for the av_thread to exit, THEN set it
    # back (→ ndi_source_thread_start: a fresh receiver whose issue-1096 fresh finder resolves the
    # live post-bounce sender). A same-name re-apply would be a no-op (no reset_ndi_receiver). The
    # SET-back is still guarded by the #795 finder-list check above (mangle protection); clearing
    # to "" never mangles (it is the valid "no source selected" state).
    op._rpc(obs, "SetInputSettings",
            {"inputName": input_name, "inputSettings": {"ndi_source_name": ""}},
            ignore_err=True)
    sleep(reset_settle_s)
    # issue 1114 review (#795 window): the clear + settle above widened the mangle window between
    # the up-front finder-list check and this set-back. Re-verify the bound name is STILL
    # discoverable right before re-applying it. When it IS, re-apply it (the normal reconnect nudge).
    if ndi_name in op._ndi_source_list(obs, input_name):
        op._rpc(obs, "SetInputSettings",
                {"inputName": input_name, "inputSettings": {"ndi_source_name": ndi_name}},
                ignore_err=True)
        return ndi_name
    # issue 1158: the bound sender VANISHED during the clear-settle, so the input is now cleared to
    # "" -- and #1114 used to STOP HERE, leaving it "". But an empty ndi_source_name STOPS the
    # DistroAV receiver thread ("No NDI Source selected; Requesting Source Thread Stop"), so the
    # in-loop #767/#1096 auto-rebind watchdogs can NEVER revive it: "" is a PERMANENT wedge until a
    # human/enforce re-applies a name (the exact "nesmie sa to stat" incident, live-confirmed on
    # strih 2026-08-20 where cam1 sat "" from 23:12 until the owner's manual set-ndi-mapping at
    # 23:38). So re-enforce the CANONICAL #399 BASELINE sender (NOT the just-vanished bound name,
    # which may be stale saved-scene drift -- cam1's was 'CAM1 (30p)', garbage that would not have
    # recovered; only the baseline 'CAM1 (usb)' did) when the baseline IS discoverable, via the
    # shared read-back-verified reenforce_ndi_name (a #795 mangle becomes a LOUD detected failure,
    # never silent corruption). If the baseline is ALSO offline, leave "" but SCREAM #1158 so the
    # [4c/8] self-heal / cleanup check / dev1 alert owns it -- an offline baseline is a real rig
    # degradation, not a silent retry.
    baseline = _baseline_sender_for(input_name)
    if baseline:
        status = op.reenforce_ndi_name(obs, input_name, baseline)
        if status == op.REENFORCE_HEALED:
            print(f"#1158 auto-revive: {input_name!r} was left EMPTY by the clear-then-set "
                  f"(bound {ndi_name!r} vanished mid-settle); re-enforced #399 baseline "
                  f"{baseline!r} (read-back verified)", file=sys.stderr)
            return baseline
        if status == op.REENFORCE_VERIFY_FAILED:
            print(f"#1158 auto-revive: {input_name!r} re-enforce of baseline {baseline!r} FAILED "
                  f"read-back (possible #795 mangle) — left as-is", file=sys.stderr)
    # issue 1197 (smoking gun, gh run 32743557703): the CLEAR above already STOPPED the receiver
    # thread ("No NDI Source selected; Requesting Source Thread Stop"). Returning now with the name
    # still "" is the self-inflicted PERMANENT wedge — the in-loop #767/#1096 watchdogs can never
    # revive an empty name (.claude/rules/ndi-name-recovery.md). So RESTORE the ORIGINAL bound name
    # instead of leaving it EMPTY: a non-empty name -> ndi_source_thread_start, so the receiver thread
    # RESTARTS and the input ends bound exactly as it started (never worse). Its own #1096 finder + the
    # harness bounded finder-warm poll (set-ndi-mapping.py --heal-wait) then re-resolve / re-enforce
    # the #399 baseline once the sender re-appears. Restoring a just-vanished name risks the #795
    # DRIFT, but a drift is RECOVERABLE (#1096 rebind / the baseline re-enforce) whereas "" is a
    # GUARANTEED stopped-thread wedge — the strictly-lesser evil, and never left empty.
    op._rpc(obs, "SetInputSettings",
            {"inputName": input_name, "inputSettings": {"ndi_source_name": ndi_name}},
            ignore_err=True)
    print(f"#1197 reattach: {input_name!r} bound {ndi_name!r} AND #399 baseline {baseline!r} both "
          f"absent from the DistroAV finder (sender mid-bounce?) — RESTORED the original bound name "
          f"rather than leaving it EMPTY (a stopped-receiver-thread wedge); the finder-warm poll "
          f"re-enforces the baseline once the sender re-appears", file=sys.stderr)
    return NDI_SOURCE_NOT_DISCOVERABLE


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--multiview-scene", default="Multiview",
                     help="name of strih's custom multiview-grid scene to rewire (default: Multiview)")
    ap.add_argument("--stats", type=float, default=None, metavar="SECONDS",
                     help="print a GetStats render-cost delta over SECONDS and exit — no seeding")
    ap.add_argument("--reattach", type=int, default=None, metavar="CAM_N",
                     help="#758 item 2: re-apply 'MV NDI cam<CAM_N>'s OWN current ndi_source_name "
                          "(forces an NDI receive reconnect) and exit — no seeding")
    args = ap.parse_args()

    obs = op._conn(args.host, args.password)
    try:
        if args.reattach is not None:
            ndi_name = reattach(obs, args.reattach)
            if ndi_name is NDI_SOURCE_NOT_DISCOVERABLE:
                # #795/#759: had a name to re-apply but it never appeared in the DistroAV finder
                # list — SKIPPED the set to avoid mangling it. Distinct exit code (2) from the
                # no-name-to-reattach case (1); the preflight_mv_reverify caller swallows both with
                # `|| true` and lets the pixel re-sample decide, so this is informational only.
                print(f"REATTACH SKIPPED: MV NDI cam{args.reattach}'s bound source is not in the "
                      f"DistroAV finder list — NOT re-applying ndi_source_name (would mangle it); "
                      f"left bound as-is")
                sys.exit(2)
            elif ndi_name:
                print(f"reattached MV NDI cam{args.reattach} -> ndi_source_name={ndi_name!r}")
            else:
                print(f"REATTACH FAILED: MV NDI cam{args.reattach} has no ndi_source_name to "
                      f"re-apply (input missing or never seeded)")
                sys.exit(1)
            return

        if args.stats is not None:
            d = measure_stats(obs, args.stats)
            print(f"render-cost over {args.stats:.0f}s: activeFps={d['activeFps']} "
                  f"avgRenderMs={d['averageFrameRenderTime']:.2f} "
                  f"renderSkipped={d['renderSkipped_delta']}/{d['renderTotal_delta']} "
                  f"({d['renderSkip_pct']}%) "
                  f"outputSkipped={d['outputSkipped_delta']}/{d['outputTotal_delta']}")
            return

        created, skipped = seed(obs)
        print(f"MV twin scenes: {len(created)}/{len(list(CAMS))} created/refreshed "
              f"({created})" + (f" — SKIPPED (no live 'Cam N'/NDI source): {skipped}" if skipped else ""))
        rewired = rewire_multiview_scene(obs, args.multiview_scene)
        if rewired:
            print(f"'{args.multiview_scene}' scene: rewired {len(rewired)} tile(s) -> {rewired}")
        else:
            print(f"'{args.multiview_scene}' scene: nothing to rewire (scene absent, or no "
                  f"'Cam N' items in it)")
    finally:
        obs.close()


if __name__ == "__main__":
    main()
