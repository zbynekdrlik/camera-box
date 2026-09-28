#!/usr/bin/env python3
"""strih OBS helper: the sender-bounce RE-ATTACH of a camera input (#758 item 2) and an ad-hoc
render-cost GetStats delta (#730).

  strih_mv_scenes.py --host 10.77.9.202 --password PW --reattach N   # CLEAR-then-SET 'NDI camN'
  strih_mv_scenes.py --host 10.77.9.202 --password PW --stats 15     # GetStats render-cost delta

issue 1242 (28.9.2026, owner order "cize vycistis strih obs aby tam neboli tie low bandwith sceny"):
strih has no low-bandwidth multiview twins any more -- the Windows-era seed of per-camera twin
scenes over their own low-bandwidth monitor inputs (#730, the #501 pattern) and the
custom-multiview rewire are removed. Every strih camera input stays connected at full bandwidth
and the built-in multiview renders the program scenes themselves. imag keeps its own multiview
scenes (scripts/imag_scenes.py) -- a different box.
"""

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_phase2 as op  # reuse the repo's ONE obs-websocket client (_conn/_rpc) — never a 4th one

# #795/#759 — reattach() sentinel: the input HAS a bound ndi_source_name to re-apply, but that name
# was NOT present in the DistroAV finder list after the bounded wait (an empty list, or a non-empty
# list that does not offer THIS source — the sender is not currently discoverable), so the re-apply
# was SKIPPED. SetInputSettings with a name absent from the combo's live item list MANGLES it (event
# review 2026-07-18: "mangles names when OBS's NDI finder list is empty"). Distinct from None (input
# missing / never seeded), so a caller — especially the WARN-only #759 cleanup path — can tell
# "skipped to avoid mangling" from "no name to re-apply".
NDI_SOURCE_NOT_DISCOVERABLE = object()


# --- PURE functions (no network — unit-tested from tests/python/test_strih_mv_scenes.py) --------

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

    #761: targets `f"NDI cam{cam_n}"` — the MAIN camera input, the SAME input the sender-bounce
    liveness probe this function backs (recording-e2e.sh's preflight_mv_reverify) checks. It is
    always connected and rendered (the built-in OBS Multiview shows its program scene), so a stuck
    receiver here is a genuine sender-bounce symptom.

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
            # issue 1197 review 🔵-1: the baseline WAS discoverable (reenforce_ndi_name only reaches
            # VERIFY_FAILED after SETTING a name present in the finder), so the input already holds
            # that just-set (mangled) non-empty value -- NOT the stopped-thread empty state. Return
            # here and leave it as-is: blind-setting the KNOWN-ABSENT original name over it (the
            # restore below) would be a pointless #795 mangle-set that discards the discoverable-
            # baseline attempt. The finder-warm poll re-enforces the baseline once it re-appears.
            print(f"#1158 auto-revive: {input_name!r} re-enforce of baseline {baseline!r} FAILED "
                  f"read-back (possible #795 mangle) — left as-is (non-empty, not restoring the "
                  f"absent original over it)", file=sys.stderr)
            return NDI_SOURCE_NOT_DISCOVERABLE
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
    ap.add_argument("--stats", type=float, default=None, metavar="SECONDS",
                     help="print a GetStats render-cost delta over SECONDS and exit")
    ap.add_argument("--reattach", type=int, default=None, metavar="CAM_N",
                     help="#758 item 2: re-apply 'NDI cam<CAM_N>'s OWN current ndi_source_name "
                          "(forces an NDI receive reconnect) and exit")
    args = ap.parse_args()
    if args.reattach is None and args.stats is None:
        ap.error("specify a mode: --reattach CAM_N or --stats SECONDS")

    obs = op._conn(args.host, args.password)
    try:
        if args.reattach is not None:
            ndi_name = reattach(obs, args.reattach)
            if ndi_name is NDI_SOURCE_NOT_DISCOVERABLE:
                # #795/#759: had a name to re-apply but it never appeared in the DistroAV finder
                # list — SKIPPED the set to avoid mangling it. Distinct exit code (2) from the
                # no-name-to-reattach case (1); the preflight_mv_reverify caller swallows both with
                # `|| true` and lets the pixel re-sample decide, so this is informational only.
                print(f"REATTACH SKIPPED: NDI cam{args.reattach}'s bound source is not in the "
                      f"DistroAV finder list — NOT re-applying ndi_source_name (would mangle it); "
                      f"left bound as-is")
                sys.exit(2)
            elif ndi_name:
                print(f"reattached NDI cam{args.reattach} -> ndi_source_name={ndi_name!r}")
            else:
                print(f"REATTACH FAILED: NDI cam{args.reattach} has no ndi_source_name to "
                      f"re-apply (input missing or never seeded)")
                sys.exit(1)
            return

        d = measure_stats(obs, args.stats)
        print(f"render-cost over {args.stats:.0f}s: activeFps={d['activeFps']} "
              f"avgRenderMs={d['averageFrameRenderTime']:.2f} "
              f"renderSkipped={d['renderSkipped_delta']}/{d['renderTotal_delta']} "
              f"({d['renderSkip_pct']}%) "
              f"outputSkipped={d['outputSkipped_delta']}/{d['outputTotal_delta']}")
    finally:
        obs.close()


if __name__ == "__main__":
    main()
