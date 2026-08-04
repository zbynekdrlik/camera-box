#!/usr/bin/env python3
"""OBS setup/teardown for Phase-2 NDI taps via obs-websocket v5.

Matches the LIVE vocab on the production OBS boxes (verified 2026-06-08):
  - NDI source input kind is `ndi_source`; the source name field is
    `ndi_source_name` (NOT `distroav_*`).
  - The program is re-emitted by the DistroAV "NDI Main Output" output. We do
    NOT create it — we read its configured `ndi_name` so the caller knows which
    NDI source to tap. **The DistroAV NDI Main Output must already be enabled in
    OBS (Tools menu) on each host; setup fails loudly if it is not.**

Per host, `setup` records the current program scene, ensures ONE stable-named probe
scene+`ndi_source` exists (reused across runs), re-points it at this run's upstream NDI
name, sets it to program, and prints that host's Main Output `ndi_name` on stdout.
`teardown` restores the prior program scene and IDLES the receiver (clears
`ndi_source_name`) but KEEPS the scene+input for the next run.

Why stable reuse (#22): the production DistroAV fork cannot delete an `ndi_source` input
over the websocket API, so the old per-run PID-suffixed inputs were never cleaned up —
they accumulated and cluttered the OBS audio mixer (24 stuck inputs observed). Reusing one
fixed name leaves exactly one dormant probe artifact per box, forever — never per-run
growth.

Requires: pip install websocket-client. OBS WebSocket :4455 (pass --password if a
host requires auth; LAN boxes here use none).
"""
import argparse
import json
import os
import re
import sys
import time

try:
    from websocket import WebSocketTimeoutException, create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

PORT = 4455
STATE = "/tmp/obs_phase2_state.json"
MAIN_OUTPUT = "NDI Main Output"
# #22: ONE stable-named scene+input per box, reused across every run. Per-run pid-suffixed
# names made DistroAV ndi_source inputs accumulate (the fork's RemoveInput no-ops), so we
# fix the names and keep the artifacts dormant between runs instead of recreating them.
SCENE = "PHASE2-PROBE"
INPUT = "phase2-probe-src"

# #355: bound for waiting an orphan recording's output to FINALIZE (outputActive=False)
# after StopRecord, before this run's StartRecord. A large MP4 (the live 24.5 GB stream-box
# orphan) takes many seconds to finalize; a flat sleep(1.0) was too short, so StartRecord ran
# while the output was still active and OBS returned {code:500} → the whole capstone run
# aborted. POLL until idle, FAIL LOUD on timeout. Env-overridable for a pathological disk.
RECORD_FINALIZE_TIMEOUT_S = float(os.environ.get("OBS_RECORD_FINALIZE_TIMEOUT_S", "120"))
RECORD_FINALIZE_POLL_S = float(os.environ.get("OBS_RECORD_FINALIZE_POLL_S", "2"))

# #627: a stream-box StartRecord call reported success ("recording STARTED" logged), but
# GetRecordStatus immediately after StopRecord ~1800s later showed outputActive=false,
# outputBytes=0 — no file was EVER written. The failure was discovered only at fetch time,
# after burning the entire run duration. This is NOT a root-cause fix (the cause of that one
# silent StartRecord failure is unproven — possibly correlated with the #358
# genlock_latency_ms_src force-set that runs immediately before StartRecord in the same
# script step; that needs a live-rig reproduction, tracked separately on #627) — it is a
# fail-fast DETECTION: poll GetRecordStatus a few seconds after StartRecord and abort loudly
# if the output isn't genuinely active + writing growing bytes, instead of silently
# proceeding into a multi-minute sleep that ends in a 0-byte file. Env-overridable.
RECORD_LIVENESS_SAMPLES = int(os.environ.get("OBS_RECORD_LIVENESS_SAMPLES", "2"))
RECORD_LIVENESS_POLL_S = float(os.environ.get("OBS_RECORD_LIVENESS_POLL_S", "2"))
# #767 follow-up (live incident, run 29417639968): imag's NVENC cold-init takes ~3s from
# StartRecord to the muxer actually writing, and outputBytes lags a further beat -- the fixed
# 2x2s window alone saw active=[False, True] bytes=[0, 0] and aborted a HEALTHY recording (the
# file grew to 296MB until cleanup stopped it). When the last fixed-window sample is ACTIVE with
# bytes still 0, keep polling at 1s inside this bounded grace budget and pass as soon as bytes
# appear; an output still at 0 bytes after the whole budget is genuinely dead (#627) and aborts.
RECORD_LIVENESS_BYTES_GRACE_S = float(os.environ.get("OBS_RECORD_LIVENESS_BYTES_GRACE_S", "8"))

# #63/#149: the probe ndi_source MUST be configured EXACTLY like the live, certified, proven-
# working genlock camera inputs (NDI cam1/3/5 on strih) so the harness measures the SAME config
# that ships in production — never a divergent one. _LOCKED_BASELINE_KEYS below are asserted to
# equal the matching prod genlock input before any measurement (the #149 self-verify guard), so
# the MACHINE catches a drift between this harness and prod, not a human after a misconfigured run.
#   - genlock_fifo=True  -> the wall-clock-slaved render tick consumes exactly one queued
#                           frame per tick (camera-box #42 FIFO bypass). Without it the probe
#                           takes the normal async timestamp-cursor path, which can't be
#                           reconciled against the disciplined tick -> frames discarded.
#   - ndi_sync=2         -> NDI_SOURCE_TIMECODE (SOURCE timing). This is the #149 fix. The prod
#                           genlock cam inputs ALL run ndi_sync=2 (verified live: NDI cam1/3/5 =
#                           ndi_sync=2), and #136 timestamp-aligned release REQUIRES the frame to
#                           carry the wall-clock SOURCE timecode (is_wallclock_ts on
#                           next_frame->timestamp, src/ndi.rs). With ndi_sync=1
#                           (PROP_SYNC_NDI_TIMESTAMP, the NDI *receiver*-side monotonic
#                           timestamp) the frame carries the receiver's monotonic ts, so the
#                           #136 ts-align path silently NO-OPS — the harness then "proves" a
#                           code path it never actually exercised. ndi_sync MUST be 2 to mirror
#                           prod and to drive the #136 ts-align release the test claims to verify.
#                           (Was 1 here pre-#149 — a STALE pre-#136 value whose old justifying
#                           comment claimed the camera-box boundary timecode went out-of-bounds
#                           vs the monotonic cursor; that is obsolete now that #136 ships and
#                           prod itself runs ndi_sync=2.)
#   - ndi_bw_mode=0      -> highest bandwidth (full quality), matching prod.
# Merged FIRST in each settings dict so the per-call ndi_source_name still overrides cleanly.
# latency=0 (Normal) MIRRORS the live, proven cam inputs (NDI cam1/3/5 are all latency=0 on
# strih) and IS THE CERTIFIED low-latency zero-loss ingest mode (#84): the A/B measurement
# found the DistroAV receive buffer is NOT a real latency lever once genlock is active — the
# wall-clock render tick dominates emit timing, and Normal(0) gives a ~33 ms LOWER strih
# abs_emit p50 than Lowest(2) while staying zero-loss. The genlock FIFO preload
# (OBS_GENLOCK_PRELOAD_FRAMES) is the jitter buffer that matters. The probe MUST run at the
# pinned 0 (vendor/README.md ndi_input_latency) so this harness measures the certified config,
# not a different one. (Was latency=2 pre-#84, before the A/B re-pin to Normal(0).)
_PROBE_NDI_SETTINGS = {"ndi_bw_mode": 0, "genlock_fifo": True, "ndi_sync": 2, "latency": 0}

# #149: the locked baseline keys that the probe ingest MUST share BIT-FOR-BIT with the certified
# prod genlock input under test. These define the timing/quality regime the harness measures;
# if ANY of them diverges from prod, the harness is no longer measuring the production config and
# the run is invalid. The per-source genlock_preload (the #97 copied tuning, _GENLOCK_COPY_KEYS)
# is DELIBERATELY excluded — it is allowed to differ per source. The self-verify guard
# (_diverging_locked_keys + _assert_probe_matches_prod) asserts equality on exactly these keys
# before measuring and FAILS FAST with a precise per-key diagnostic on any mismatch.
_LOCKED_BASELINE_KEYS = ("ndi_sync", "genlock_fifo", "ndi_bw_mode", "latency")

# Per-source genlock TUNING to copy from a production input onto the probe input so the
# probe measures the SAME delay behaviour as the live chain. Copy ONLY the per-source
# preload (the #97 video-delay / FIFO depth) — NOT the #63-critical baseline
# (genlock_fifo / ndi_sync), which MUST stay pinned in _PROBE_NDI_SETTINGS so a prod input
# with a different value can never send the probe black. We read prod read-only and never
# touch the prod input or its scene; on read failure we just use the baseline (logged).
_GENLOCK_COPY_KEYS = ("genlock_preload",)


def _effective_input_settings(ws, input_name):
    """#149: return an input's EFFECTIVE settings = its TYPE DEFAULTS overlaid with the
    explicitly-persisted (non-default) settings.

    This is REQUIRED for a sound self-verify comparison. obs-websocket's GetInputSettings
    returns ONLY explicitly-saved (non-default) settings — it omits any key left at its
    default (the obs-websocket RequestHandler docstring states this verbatim). And THREE of
    the four #149 locked baseline values ARE the DistroAV ndi_source defaults
    (ndi_sync=2 SOURCE_TIMECODE, ndi_bw_mode=0 HIGHEST, latency=0 NORMAL). So a prod genlock
    input that left any of them at default would have that key ABSENT from GetInputSettings,
    making a raw dict comparison see None-vs-2 and FALSE-FAIL a perfectly valid run. Merging
    GetInputDefaultSettings underneath makes the dict reflect the input's TRUE effective
    config — the same effective config the compositor actually runs — so the guard compares
    like-for-like. Best-effort: a defaults read failure falls back to the explicit-only dict
    (the prior, brittle behaviour) rather than raising."""
    explicit = _rpc(ws, "GetInputSettings", {"inputName": input_name},
                    ignore_err=True).get("inputSettings", {})
    defaults = _rpc(ws, "GetInputDefaultSettings", {"inputKind": "ndi_source"},
                    ignore_err=True).get("defaultInputSettings", {})
    return {**defaults, **explicit}


def _is_genlock_prod_input(settings):
    """#149: True iff a prod ndi_source input's settings look like a CERTIFIED GENLOCK
    cam input (genlock_fifo enabled) rather than a non-genlock NDI input. The non-genlock
    inputs on a box (e.g. 'NDI 2ME PVW', 'NDI Bible' — preview/graphics feeds) run
    ndi_sync=1 and have no genlock_fifo; matching one of THOSE for the baseline would make
    the guard demand ndi_sync=1 and re-introduce the #149 bug. Only an input that is itself
    genlocked (genlock_fifo truthy) is a valid certified baseline for the probe.

    NOTE: genlock_fifo is a DistroAV addition that is NOT in the type defaults, so it only
    appears here when it was explicitly persisted — which is exactly when the input is a
    genlock input. So this stays correct whether settings come from the raw GetInputSettings
    or the defaults-merged effective dict."""
    return bool(settings.get("genlock_fifo"))


def _find_prod_genlock_input(ws, host, upstream_ndi_name):
    """#149: locate the CERTIFIED prod GENLOCK input whose ndi_source_name matches
    *upstream_ndi_name* (exact or substring) on *host*, and return
    ``(input_name, full_settings_dict)`` for it — or ``(None, {})`` if none is found / a
    read fails.

    The returned full settings drive BOTH (a) the per-source preload copy (_GENLOCK_COPY_KEYS)
    and (b) the #149 self-verify guard (_LOCKED_BASELINE_KEYS), so we read the prod input
    ONCE. NEVER modifies any prod input or scene. Best-effort: exceptions are caught and the
    caller falls back gracefully (with NO certified baseline → the guard cannot run).
    """
    try:
        inputs = _rpc(ws, "GetInputList", ignore_err=True).get("inputs", [])
        ndi_inputs = [
            i["inputName"] for i in inputs
            if i.get("inputKind") == "ndi_source" and i["inputName"] != INPUT
        ]
        # Find the prod GENLOCK input whose ndi_source_name matches the upstream we are
        # ingesting (e.g. "CAM1 (usb)" on strih, or the strih NDI name on stream).
        # Skip non-genlock inputs (they share the same source name family but run a
        # different timing regime — they are NOT a valid certified baseline, #149).
        # Collect ALL genlock matches and PREFER an EXACT ndi_source_name match over a
        # mere substring match, so a longer name that merely CONTAINS the upstream can't
        # be picked ahead of the exact source (which would copy the wrong genlock_preload).
        exact = None
        substring = None
        for inp_name in ndi_inputs:
            try:
                s = _effective_input_settings(ws, inp_name)
                if not _is_genlock_prod_input(s):
                    continue
                src = s.get("ndi_source_name", "")
                if src == upstream_ndi_name:
                    exact = (inp_name, s)
                    break  # exact match is the best possible — stop
                if substring is None and upstream_ndi_name and upstream_ndi_name in src:
                    substring = (inp_name, s)
            except Exception:
                continue
        chosen = exact or substring
        if chosen:
            inp_name, s = chosen
            how = "exact" if exact else "substring"
            sys.stderr.write(
                f"[obs] {host}: certified prod genlock input '{inp_name}' "
                f"(ndi_source_name='{s.get('ndi_source_name', '')}') matched ({how}) "
                f"for '{upstream_ndi_name}'\n"
            )
            return inp_name, s
        sys.stderr.write(
            f"[obs] {host}: WARN could not find a certified prod GENLOCK ndi_source "
            f"input matching '{upstream_ndi_name}'; probe will use default genlock "
            f"settings and the #149 self-verify guard cannot assert against prod\n"
        )
        return None, {}
    except Exception as e:
        sys.stderr.write(
            f"[obs] {host}: WARN reading prod genlock settings failed ({e}); "
            f"probe will use default genlock settings\n"
        )
        return None, {}


# STATE key under which a prod input's pre-test genlock_preload is saved so teardown can
# restore it (#183). One entry per host: {"input": <name>, "preload": <prod value>}.
_TEST_PRELOAD_STATE_KEY = "test_preload_saved"


def _force_test_preload(ws, host, upstream, test_preload, state):
    """#183: FORCE the recorded prod GENLOCK input's genlock_preload to `test_preload` (1)
    for the test recording window, and SAVE its prior prod value into `state[host]` so
    teardown restores it. Production audio-sync uses a deep preload (≈31 ≈ 1s video delay)
    that is IRRELEVANT noise for the lowest-latency zero-loss TEST — at preload=1 the test
    measures the TRUE genlock hop (~33ms) instead of the prod audio-delay.

    Touches ONLY the genlock_preload of the one certified prod input feeding this scene
    (found by ndi_source_name == upstream), nothing else — the #63/#149 locked baseline
    (genlock_fifo/ndi_sync/ndi_bw_mode/latency) is left exactly as prod has it. Best-effort:
    a read/find failure logs a warning and leaves prod untouched (the test then measures
    prod's preload, the prior behaviour) rather than aborting. Returns True iff it forced.

    Mutates `state` in place and persists it (so a crash between force and teardown still
    leaves the saved value on disk for a later teardown to restore)."""
    if not upstream:
        return False
    inp_name, prod_s = _find_prod_genlock_input(ws, host, upstream)
    if not inp_name:
        sys.stderr.write(
            f"[obs] {host}: #183 WARN no certified prod genlock input for upstream "
            f"'{upstream}'; leaving prod genlock_preload untouched (test measures prod's "
            f"preload, not {test_preload})\n"
        )
        return False
    prod_preload = prod_s.get("genlock_preload")
    if prod_preload == test_preload:
        sys.stderr.write(
            f"[obs] {host}: #183 prod input '{inp_name}' already at genlock_preload="
            f"{test_preload}; nothing to force\n"
        )
        # Still record that we did NOT change it (so teardown won't wrongly restore).
        host_state = state.setdefault(host, {})
        host_state.pop(_TEST_PRELOAD_STATE_KEY, None)
        _save_state(state)
        return False
    # Save the prod value FIRST + persist, THEN force — so a crash after the force still has
    # the saved value on disk for teardown to restore.
    host_state = state.setdefault(host, {})
    host_state[_TEST_PRELOAD_STATE_KEY] = {"input": inp_name, "preload": prod_preload}
    _save_state(state)
    _rpc(ws, "SetInputSettings", {
        "inputName": inp_name,
        "inputSettings": {"genlock_preload": test_preload},
        "overlay": True,
    }, ignore_err=True)
    sys.stderr.write(
        f"[obs] {host}: #183 FORCED prod input '{inp_name}' genlock_preload "
        f"{prod_preload} -> {test_preload} for the test (will restore on teardown)\n"
    )
    return True


def _restore_test_preload(ws, host, state):
    """#183: restore the prod input's genlock_preload saved by _force_test_preload (called
    from teardown). No-op when nothing was forced. Best-effort; clears the STATE entry so a
    re-run never double-restores."""
    host_state = state.get(host, {})
    saved = host_state.get(_TEST_PRELOAD_STATE_KEY)
    if not saved:
        return
    inp_name = saved.get("input")
    preload = saved.get("preload")
    if inp_name and preload is not None:
        _rpc(ws, "SetInputSettings", {
            "inputName": inp_name,
            "inputSettings": {"genlock_preload": preload},
            "overlay": True,
        }, ignore_err=True)
        sys.stderr.write(
            f"[obs] {host}: #183 RESTORED prod input '{inp_name}' genlock_preload -> "
            f"{preload} (prod audio-sync untouched after the test)\n"
        )
    host_state.pop(_TEST_PRELOAD_STATE_KEY, None)
    _save_state(state)


# ---------------------------------------------------------------------------
# #358: genlock-latency-delivery hard gate
# ---------------------------------------------------------------------------

# STATE key under which 'NDI 2ME PGM' per-source latency + gpu_delay filter states
# are saved so teardown can restore them exactly (#358).
# Entry per host: {"input": <name>, "latency_ms": <prod value>, "render_delays": [...]}.
_TEST_LATENCY_STATE_KEY = "test_latency_saved"

# OBS property name for the per-source genlock latency (PROP_GENLOCK_LATENCY_MS_SRC in
# ndi-source.cpp, DistroAV fork — the "Latency (ms)" slider per source, default 3ms,
# range [3, 2000]; prod 'NDI 2ME PGM' on stream runs 450ms for A/V-align).
_GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"

# Render-Delay filter kind (OBS filter id "gpu_delay" — gpu-delay.c).
_GPU_DELAY_KIND = "gpu_delay"


def _latency_delivery_ok(set_ms: int, delivered_ms: int, tolerance_ms: int = 100) -> bool:
    """#358 PURE decision: did the genlock FIFO actually HOLD the configured latency?

    Returns True iff `delivered_ms >= set_ms - tolerance_ms`, i.e. the effective held
    latency is within `tolerance_ms` below the set value.

    The #292 bug caused the FIFO to be force-drained to ~3-50ms even when 1000ms was
    configured (the drop-cap was budgeted at canvas fps instead of source arrival fps).
    A force-drained FIFO at 3-50ms clearly fails this gate (1000-100=900ms threshold).

    Pure function — no I/O, no OBS calls. Tier-0 testable on default features."""
    return delivered_ms >= set_ms - tolerance_ms


def _parse_latency_ms_from_audit_line(line: str):
    """#358 PURE: parse the EFFECTIVE held latency from a genlock-fifo audit log line.

    Returns the integer value of `latency_ms=N` from a line matching:
        genlock-fifo audit 'SOURCE': ... latency_ms=N (≈F frames @ fps) src_latency_ms=...
    or None if the line is not a genlock-fifo audit line.

    NOTE: matches `latency_ms=` NOT `src_latency_ms=` (underscore prefix) — the effective
    held value (what the FIFO actually delivers) vs the per-source setting stored in OBS.
    The sed pattern in drift-guard.sh:292 uses the same disambiguation."""
    # Match ' latency_ms=N' (space before) so it does NOT match 'src_latency_ms='.
    m = re.search(r"genlock-fifo audit '([^']+)'.*? latency_ms=(\d+)", line)
    if m:
        return int(m.group(2))
    return None


# #691: the box's own CURRENT genlock_latency_ms_src, at or above this floor, is already
# well past the #292 >450ms cap this gate exercises — so there is no need to FORCE any
# change at all when the caller left --test-latency-ms unset. 500ms comfortably clears the
# 450ms prod A/V-align baseline while staying below the #358 gate's original 1000ms.
DEFAULT_TEST_LATENCY_CURRENT_FLOOR_MS = 500

# #691: fallback test latency (ms) used ONLY when the box's current value is BELOW the
# floor above (so nothing already exercises the #292 regression) and the caller did not
# explicitly request a specific value. This is the ORIGINAL #358 default.
DEFAULT_TEST_LATENCY_FALLBACK_MS = 1000


def _int_env_or_none(name):
    """Read an int-valued env var, or None if unset/empty.

    #691: used for both --test-latency-ms (distinguishes an EXPLICIT operator override
    from "derive a smart default from the box's current value", see
    resolve_test_latency_ms) and --calibrated-latency-ms (an OPTIONAL cross-check value —
    absent by default, never a hard requirement)."""
    v = os.environ.get(name, "")
    return int(v) if v else None


def resolve_test_latency_ms(
    requested_ms,
    current_ms,
    floor_ms=DEFAULT_TEST_LATENCY_CURRENT_FLOOR_MS,
    fallback_ms=DEFAULT_TEST_LATENCY_FALLBACK_MS,
):
    """#691 PURE decision: the EFFECTIVE #358 delivery-verify test latency to apply.

    `requested_ms` is `None` when the caller (recording-e2e.sh) did not explicitly set
    `GENLOCK_TEST_LATENCY_MS` — an EXPLICIT value always wins verbatim (an operator/
    supervisor override is never second-guessed).

    Otherwise: if the box's CURRENT (pre-test) `genlock_latency_ms_src` already sits at or
    above `floor_ms` (500ms — comfortably above the #292 >450ms cap this gate exercises),
    use that CURRENT value AS-IS. This is the actual #691 fix: a box already calibrated to
    (e.g.) 925ms needs NO forced change at all to exercise the regression check — which
    eliminates the #691 stomp risk entirely for that run (there is nothing to restore
    because nothing was ever changed).

    Only when the current value is BELOW the floor (nothing already exercises the cap)
    does this fall back to `fallback_ms` (1000ms, the original #358 default) so the gate
    still meaningfully tests the FIFO's ability to hold a latency deep past the cap.

    Pure function — no I/O. Tier-0 testable on default features."""
    if requested_ms is not None:
        return requested_ms
    if current_ms >= floor_ms:
        return current_ms
    return fallback_ms


def _snapshot_and_set_test_latency(ws, host, source_name, requested_test_latency_ms, state):
    """#358/#691: snapshot the per-source genlock latency + gpu_delay filter states on
    `source_name` ('NDI 2ME PGM'), set the (possibly auto-derived, see
    `resolve_test_latency_ms`) test latency, and disable any gpu_delay filters so they
    don't mask the effective FIFO depth in the audit log.

    Saves the snapshot to `state[host][_TEST_LATENCY_STATE_KEY]` (persisted to disk)
    BEFORE making changes — crash-safe, mirrors the #183 preload pattern. No-op and no
    state entry when `source_name` is empty. Returns True iff it changed anything.

    #691 FIX: the snapshot is now saved UNCONDITIONALLY, even when nothing needs to
    change (current value already equals the effective test value). The OLD behavior
    skipped saving state in that case AND actively discarded any existing saved state —
    which silently destroyed the one piece of information that could recover a box left
    stuck by an EARLIER run whose own restore never completed (e.g. a crash before
    cleanup() ran). Live incident: the stream box's 'NDI 2ME PGM' got stuck at the 1000ms
    test value; every SUBSEQUENT run's `prod_latency (1000) == test_latency_ms (1000)`
    short-circuit treated that as "already correct" and silently perpetuated the stomp —
    the calibrated 925ms A/V-align value was never restored until a human intervened over
    the WebSocket directly. Always snapshotting THIS run's own observed value means
    teardown always has something accurate (as of THIS run's start) to restore to."""
    if not source_name:
        return False

    # Read current per-source latency.
    inp_settings = _rpc(ws, "GetInputSettings", {"inputName": source_name},
                        ignore_err=True).get("inputSettings", {})
    prod_latency = inp_settings.get(_GENLOCK_SRC_LATENCY_KEY, 3)  # 3ms floor default

    # #691: resolve the EFFECTIVE test latency from the box's OWN current value when the
    # caller did not explicitly request one.
    test_latency_ms = resolve_test_latency_ms(requested_test_latency_ms, prod_latency)

    # Read current gpu_delay filters.
    filter_list = _rpc(ws, "GetSourceFilterList", {"sourceName": source_name},
                       ignore_err=True).get("filters", [])
    render_delays = [
        {
            "filterName": f["filterName"],
            "was_enabled": f.get("filterEnabled", True),
            "delay_ms": f.get("filterSettings", {}).get("delay_ms", 0),
        }
        for f in filter_list
        if f.get("filterKind") == _GPU_DELAY_KIND
    ]

    # #691: save the snapshot UNCONDITIONALLY (see the function docstring) — BEFORE any
    # change is applied, crash-safe, mirrors the #183 preload pattern.
    host_state = state.setdefault(host, {})
    host_state[_TEST_LATENCY_STATE_KEY] = {
        "input": source_name,
        "latency_ms": prod_latency,
        "render_delays": render_delays,
    }
    _save_state(state)

    if prod_latency == test_latency_ms:
        sys.stderr.write(
            f"[obs] {host}: #358/#691 '{source_name}' already at genlock_latency_ms_src="
            f"{test_latency_ms}; nothing to force (snapshot saved so teardown still "
            f"restores it)\n"
        )
        return False

    # Set test latency.
    _rpc(ws, "SetInputSettings", {
        "inputName": source_name,
        "inputSettings": {_GENLOCK_SRC_LATENCY_KEY: test_latency_ms},
        "overlay": True,
    }, ignore_err=True)

    # Disable gpu_delay filters so they don't mask the FIFO depth in audit lines.
    for rd in render_delays:
        if rd["was_enabled"]:
            _rpc(ws, "SetSourceFilterEnabled", {
                "sourceName": source_name,
                "filterName": rd["filterName"],
                "filterEnabled": False,
            }, ignore_err=True)

    sys.stderr.write(
        f"[obs] {host}: #358 FORCED '{source_name}' genlock_latency_ms_src "
        f"{prod_latency} -> {test_latency_ms} for delivery-verify test "
        f"(will restore on teardown)\n"
    )
    return True


def _restore_test_latency(ws, host, state, calibrated_latency_ms=None):
    """#358/#691: restore the per-source genlock latency + gpu_delay filters saved by
    _snapshot_and_set_test_latency. No-op when nothing was snapshotted. Emits a LOUD
    warning (mirroring #246 burn-verify) if the read-back after restore ≠ snapshot —
    prod A/V-align depends on exact restore. Best-effort; clears state entry always.

    `calibrated_latency_ms` (#691 belt-and-braces, OPTIONAL): the known-good prod value
    from `av-sync-last.json` on the OBS box's own ProgramData, gathered by the operator/
    agent and passed in — mirrors drift-guard.sh's `av_sync_calibrated_ms` best-effort
    cross-check for the SAME file, since this function itself has no ssh/scp path of its own to
    read the Windows box's filesystem directly (`av_sync_calibrate.py`'s own `remote_push_plan`
    prints a win-* MCP plan instead -- #701 proved plain scp/ssh reaches strih/stream, but
    that script still has no MCP/ssh access of its own). When supplied, the FINAL restored
    value is cross-checked against it and a LOUD warn fires on mismatch — this catches
    the case the snapshot-vs-restore check above CANNOT: the snapshot itself already
    being wrong (e.g. this run's snapshot captured a value a PRIOR run's incomplete
    restore left behind, not the true calibrated prod value). Silently skipped (no
    check) when not supplied — never a hard requirement."""
    host_state = state.get(host, {})
    saved = host_state.get(_TEST_LATENCY_STATE_KEY)
    if not saved:
        return

    source_name = saved.get("input", "")
    prod_latency = saved.get("latency_ms")
    render_delays = saved.get("render_delays", [])
    final_value = None

    if source_name and prod_latency is not None:
        # Restore the per-source latency.
        _rpc(ws, "SetInputSettings", {
            "inputName": source_name,
            "inputSettings": {_GENLOCK_SRC_LATENCY_KEY: prod_latency},
            "overlay": True,
        }, ignore_err=True)

        # Verify the read-back matches (#246 pattern: LOUD warn on mismatch).
        readback = _rpc(ws, "GetInputSettings", {"inputName": source_name},
                        ignore_err=True).get("inputSettings", {})
        actual = readback.get(_GENLOCK_SRC_LATENCY_KEY)
        final_value = actual
        if actual != prod_latency:
            sys.stderr.write(
                f"[obs] {host}: #358 WARN mismatch after restore — "
                f"'{source_name}' genlock_latency_ms_src read-back={actual!r} "
                f"expected={prod_latency}; prod A/V-align may be off! "
                f"Manual check required.\n"
            )
        else:
            sys.stderr.write(
                f"[obs] {host}: #358 RESTORED '{source_name}' genlock_latency_ms_src "
                f"-> {prod_latency} (prod A/V-align 450ms restored)\n"
            )

        # #691 belt-and-braces: cross-check the FINAL value against the CALIBRATED prod
        # source of truth, when supplied (see docstring above).
        if calibrated_latency_ms is not None and final_value != calibrated_latency_ms:
            sys.stderr.write(
                f"[obs] {host}: #691 WARN calibrated-value mismatch — "
                f"'{source_name}' genlock_latency_ms_src={final_value!r} after restore, "
                f"but the calibrated prod value (av-sync-last.json) is "
                f"{calibrated_latency_ms}ms; prod A/V-align may be OFF even though the "
                f"restore itself matched its own snapshot. Manual check required.\n"
            )

    # Re-enable gpu_delay filters that were enabled before the test.
    for rd in render_delays:
        if rd.get("was_enabled"):
            _rpc(ws, "SetSourceFilterEnabled", {
                "sourceName": source_name,
                "filterName": rd["filterName"],
                "filterEnabled": True,
            }, ignore_err=True)

    host_state.pop(_TEST_LATENCY_STATE_KEY, None)
    _save_state(state)


def _diverging_locked_keys(certified, probe, keys=_LOCKED_BASELINE_KEYS):
    """#149 self-verify CORE (pure, testable): given the CERTIFIED prod genlock input's
    settings *certified* and the PROBE ingest's effective settings *probe*, return the
    sorted list of *keys* whose values differ between them (or are missing from probe).

    A returned list that is non-empty means the harness would measure a config that
    DIVERGES from the certified production config — the caller must FAIL FAST. An empty
    list means the probe's locked baseline mirrors prod exactly. This compares ONLY the
    locked baseline keys; per-source tuning (genlock_preload) is intentionally not here.

    Each diverging entry is a dict: {"key", "expected" (prod), "actual" (probe)}."""
    diverging = []
    for k in keys:
        exp = certified.get(k)
        act = probe.get(k)
        if exp != act:
            diverging.append({"key": k, "expected": exp, "actual": act})
    return diverging


def _assert_probe_matches_prod(host, prod_input_name, certified, probe_effective):
    """#149 self-verify GUARD: assert the probe ingest's effective locked baseline equals
    the CERTIFIED prod genlock input's, EXACTLY, before any measurement. On ANY mismatch
    raise SystemExit with a precise per-key diagnostic (which key, prod-expected vs
    probe-actual) so a config drift between the harness and prod can NEVER again produce a
    silently-invalid measurement that a human has to catch. The machine guards the config.

    If no certified prod genlock input was found (certified is falsy / empty), the guard
    cannot assert and aborts loudly — measuring against an UNKNOWN prod config is exactly
    the failure mode #149 closes, so we never proceed without a baseline to verify against."""
    if not certified:
        raise SystemExit(
            f"[obs] {host}: #149 self-verify ABORT — no certified prod GENLOCK input found "
            f"to verify the probe against. Refusing to measure a config that cannot be "
            f"confirmed to mirror production. Ensure the prod genlock cam input for this "
            f"source is configured (genlock_fifo on) and discoverable over obs-websocket."
        )
    diverging = _diverging_locked_keys(certified, probe_effective)
    if diverging:
        lines = "\n".join(
            f"    - {d['key']}: prod(certified)={d['expected']!r}  probe={d['actual']!r}"
            for d in diverging
        )
        raise SystemExit(
            f"[obs] {host}: #149 self-verify FAIL — probe ingest config DIVERGES from the "
            f"certified prod genlock input '{prod_input_name}' on these locked baseline "
            f"keys:\n{lines}\n"
            f"  The harness MUST measure the exact production config. Aborting before any "
            f"measurement (a divergent config would silently invalidate the proof). Fix "
            f"_PROBE_NDI_SETTINGS in scripts/obs_phase2.py to mirror prod, or fix the prod "
            f"input, then re-run."
        )
    sys.stderr.write(
        f"[obs] {host}: #149 self-verify OK — probe locked baseline "
        f"{ {k: probe_effective.get(k) for k in _LOCKED_BASELINE_KEYS} } matches certified "
        f"prod genlock input '{prod_input_name}'\n"
    )


def _load_state():
    """Read the per-host prev-scene state. Tolerates a MISSING or CORRUPT/truncated file
    (a crash mid-write can leave partial JSON) — returns {} rather than raising, so a bad
    state file can never make teardown raise before it restores the prior program scene
    (which would strand the probe scene as live program on a production OBS)."""
    try:
        with open(STATE) as f:
            return json.load(f)
    except (FileNotFoundError, ValueError):  # ValueError covers JSONDecodeError
        return {}


def _save_state(state):
    """Write state ATOMICALLY (tmp + os.replace) so a crash mid-write can never leave the
    corrupt file that _load_state would otherwise have to recover from."""
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


def _conn(host, password=""):
    import base64
    import hashlib

    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())
    # #331: subscribe to ZERO obs-websocket events. This client is pure request/response (op-6 ->
    # op-7 in _rpc); it consumes no events. Omitting eventSubscriptions defaults the session to
    # EventSubscription::All, so a COLD `NDI 2ME PGM` reactivation on the stream box makes DistroAV
    # flood op-5 events and _rpc's read loop drains them forever while the matching op-7 never
    # arrives -> the #328 wall-clock timeout that hung prod-scene+teardown and failed #312 runs
    # 312006/312007. Subscribing to nothing makes the client structurally immune to the entire
    # event-flood class (this source and any future one); the #328 cap stays as the loud backstop.
    ident = {"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())
    return ws


# #328: a HARD overall wall-clock deadline (seconds) for a single obs-websocket request. The
# _rpc read loop below drains op-5 EVENTS until the matching op-7 response arrives; while OBS
# renegotiates an NDI source it can emit a continuous event flood, so each recv() keeps succeeding
# within the socket timeout yet the response NEVER comes — and the loop then spins for as long as
# events keep arriving. That is the #328 ~28-min `prod-scene` (and then `teardown`) hang on stream:
# the WS was healthy, but the request never completed, so the whole #312 rig run blocked and the
# stuck teardown left a cam box's capture device held (#281 class). Bounding the TOTAL wall-clock per
# request makes any stuck OBS op FAIL LOUD (TimeoutError -> non-zero exit for prod-scene/setup/switch,
# or a logged warning inside teardown's best-effort guard) instead of blocking the run indefinitely.
# Named + env-overridable; set 0 (or negative) to disable the bound (wait indefinitely).
OBS_OP_TIMEOUT_S = float(os.environ.get("OBS_OP_TIMEOUT_S", "60"))


def _rpc_timed_out(elapsed_s, timeout_s):
    """#328 (pure, testable): True iff a single obs-websocket request has run longer than its hard
    wall-clock deadline and MUST fail loud rather than keep draining events. A non-positive
    *timeout_s* disables the bound (wait indefinitely) — the explicit opt-out."""
    return timeout_s > 0 and elapsed_s >= timeout_s


def _rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
    """Send an obs-websocket request and return its responseData, BOUNDED by a hard overall
    wall-clock deadline (OBS_OP_TIMEOUT_S; override per-call via *timeout_s*) so a stuck OBS op can
    never hang the run (#328). The loop drains op-5 events waiting for the matching op-7 response;
    during an NDI renegotiation OBS can flood events while the response never arrives, so once the
    deadline passes we raise TimeoutError (fail loud). *ignore_err* suppresses an OBS request-level
    error (a normal failed RPC), but NEVER the timeout — a hang is always fatal to the op."""
    deadline_s = OBS_OP_TIMEOUT_S if timeout_s is None else timeout_s
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if _rpc_timed_out(time.monotonic() - t0, deadline_s):
            raise TimeoutError(
                f"obs-websocket request {rtype!r} got no response within {deadline_s:.0f}s — the "
                f"connection is reachable but the request never completed (an NDI source "
                f"mid-renegotiation can flood events while the response never arrives, #328). "
                f"Failing loud instead of blocking the run; raise OBS_OP_TIMEOUT_S if a slow op "
                f"is legitimately needed."
            )
        try:
            m = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue  # no frame within the socket timeout; re-check the overall deadline
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"] and not ignore_err:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


# #93: how long to wait after idling the probe receiver for the DistroAV av_thread to
# fully exit its reset_ndi_receiver block before we re-point the source. One render tick
# is ~20 ms at 50 fps; 0.25 s is comfortably several ticks of margin (the av_thread polls
# its reset flag once per loop iteration, ~5–100 ms) without slowing the run meaningfully.
_QUIESCE_RENDER_TICK_S = 0.25


def _quiesce_probe_input(ws):
    """#93: idle the reused probe ndi_source BEFORE re-pointing it, so the re-point lands
    on a dormant receiver instead of racing a live av_thread. Clearing ndi_source_name
    makes DistroAV tear the receiver down cleanly (the same idle discipline teardown uses);
    genlock_fifo off stops the dormant input running the consume path against an empty queue
    (#70). Then wait one render tick for the av_thread to exit its reset block. Best-effort:
    a quiesce failure must not abort setup (the C++ config_mutex fix is the real guard)."""
    _rpc(ws, "SetInputSettings", {
        "inputName": INPUT,
        "inputSettings": {"ndi_source_name": "", "genlock_fifo": False},
        "overlay": True,
    }, ignore_err=True)
    time.sleep(_QUIESCE_RENDER_TICK_S)


def setup(a):
    ws = _conn(a.host, a.password)
    prev = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")
    # In Studio Mode the PREVIEW scene's sources stay active (rendered). If teardown leaves
    # our probe scene in preview, its idle ndi_source keeps render-ticking the genlock FIFO
    # with an empty queue -> perpetual underrun-audit spam that corrupts the cumulative FIFO
    # audit (#70). Record the prior preview so teardown can restore it.
    studio = bool(_rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
    prev_preview = (
        _rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get("currentPreviewSceneName")
        if studio
        else None
    )

    out = _rpc(ws, "GetOutputSettings", {"outputName": MAIN_OUTPUT}, ignore_err=True)
    ndi_name = (out.get("outputSettings") or {}).get("ndi_name")
    if not ndi_name:
        raise SystemExit(
            f"[obs] {a.host}: DistroAV '{MAIN_OUTPUT}' is not enabled — enable it in "
            f"OBS (Tools > DistroAV / NDI Output Settings, 'Main Output') and set its "
            f"NDI name, then re-run. Phase 2 taps the program NDI this output emits."
        )

    # Snapshot the scene list once — used both for the prev-scene sanitizer and the
    # idempotent scene-exists check below.
    scenes = [s.get("sceneName") for s in _rpc(ws, "GetSceneList").get("scenes", [])]

    # Never record our own probe scene as the restore target: if a prior run crashed with
    # the probe on program, recover the real prior scene from the last good run's saved
    # state; if THAT is also missing/the probe, fall back to any existing non-probe scene.
    # This guarantees teardown can never strand the probe scene as live program on a box.
    if prev == SCENE:
        prev = _load_state().get(a.host, {}).get("prev_scene") or prev
    if not prev or prev == SCENE:
        prev = next((s for s in scenes if s != SCENE), None)
        sys.stderr.write(
            f"[obs] {a.host}: WARN prior program unknown/was the probe scene; "
            f"will restore to '{prev}'\n"
        )
    # Same probe-scene guard for the preview target: never restore the probe scene into
    # preview. Fall back to the (already-sanitized) program scene when unknown.
    if prev_preview == SCENE:
        prev_preview = _load_state().get(a.host, {}).get("prev_preview") or prev
    if not prev_preview or prev_preview == SCENE:
        prev_preview = prev
    state = _load_state()
    state[a.host] = {"prev_scene": prev, "prev_preview": prev_preview}
    _save_state(state)

    # Read the CERTIFIED production genlock input ONCE (its name + full settings). This
    # ensures the probe runs with the SAME certified genlock config as the live prod inputs
    # (e.g. the cam1 NDI input on strih, or the strih NDI input on stream) without ever
    # touching those prod inputs or their scenes, AND gives the #149 self-verify guard the
    # baseline to assert against. Falls back to _PROBE_NDI_SETTINGS if the prod read fails.
    prod_input_name, prod_settings = _find_prod_genlock_input(ws, a.host, a.upstream)
    # Per-source preload copy: ONLY the _GENLOCK_COPY_KEYS (no overlap with the locked
    # baseline), so the #63/#149 baseline in _PROBE_NDI_SETTINGS is never overridden.
    prod_genlock = {k: prod_settings[k] for k in _GENLOCK_COPY_KEYS if k in prod_settings}
    if prod_genlock:
        sys.stderr.write(
            f"[obs] {a.host}: copying per-source genlock tuning from prod input "
            f"'{prod_input_name}': {prod_genlock}\n"
        )
    # _PROBE_NDI_SETTINGS is the certified #63/#149 baseline (genlock_fifo/ndi_sync/
    # ndi_bw_mode/latency); prod_genlock adds ONLY the per-source preload (no key overlap),
    # so the baseline is never overridden. Both are spread into BOTH the create and the reuse
    # call below — the #63 regression guard requires _PROBE_NDI_SETTINGS reach both paths.
    # ndi_source_name is always set explicitly so it is never inherited from prod.

    # Ensure the ONE stable scene+input exist, then reuse them (#22). Creating per run is
    # what made the fork's un-removable ndi_source inputs pile up.
    if SCENE not in scenes:
        _rpc(ws, "CreateScene", {"sceneName": SCENE}, ignore_err=True)
    inputs = [i.get("inputName") for i in _rpc(ws, "GetInputList").get("inputs", [])]
    if INPUT not in inputs:
        _rpc(ws, "CreateInput", {
            "sceneName": SCENE, "inputName": INPUT, "inputKind": "ndi_source",
            "inputSettings": {**_PROBE_NDI_SETTINGS, **prod_genlock, "ndi_source_name": a.upstream},
        }, ignore_err=True)
    else:
        # #93: QUIESCE before re-pointing a possibly-LIVE probe input. If a prior run
        # left the probe scene on program (a crash, or back-to-back runs), the
        # ndi_source receiver+av_thread are still live on the old upstream. Re-pointing
        # it in place (SetInputSettings → ndi_source_update) frees/reallocs the NDI
        # source-name string the av_thread is mid-read on → DistroAV heap corruption
        # (the strih OBS crash). The C++ config_mutex+owned-copies fix makes that race
        # safe, but the harness ALSO idles the receiver first (mirror teardown's idle
        # discipline) so the re-point lands on a dormant source: clear ndi_source_name
        # (DistroAV tears the receiver down cleanly) + genlock_fifo off, then wait one
        # render tick for the av_thread to fully exit its reset before re-pointing.
        _quiesce_probe_input(ws)
        # Reuse: re-point the now-idle input at this run's upstream, applying the full
        # certified probe settings idempotently in ONE update (no per-cycle HW-accel /
        # Latency churn on a live source). Spreads _PROBE_NDI_SETTINGS (the #63 baseline)
        # plus prod_genlock (the per-source preload copy) so the probe mirrors prod.
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {**_PROBE_NDI_SETTINGS, **prod_genlock, "ndi_source_name": a.upstream},
            "overlay": True,
        }, ignore_err=True)
        # ... and make sure it is an item of the stable scene (re-add if the scene was
        # recreated above, or a prior run left the input orphaned).
        items = _rpc(ws, "GetSceneItemList", {"sceneName": SCENE},
                     ignore_err=True).get("sceneItems", [])
        if not any(it.get("sourceName") == INPUT for it in items):
            _rpc(ws, "CreateSceneItem",
                 {"sceneName": SCENE, "sourceName": INPUT}, ignore_err=True)

    # Make the probe source FILL the canvas so a centered QR stays centered (and full
    # size) in this box's program / Main Output. Without this OBS renders the ndi_source
    # at its native size/position, so a centered single QR lands off-center downstream and
    # the centered decode ROI misses it (decode_failed=100% at strih/stream). STRETCH the
    # item to the base canvas from (0,0). Touches ONLY the probe scene item, never prod.
    vs = _rpc(ws, "GetVideoSettings", ignore_err=True)
    base_w = int(vs.get("baseWidth") or 1920)
    base_h = int(vs.get("baseHeight") or 1080)
    item_id = _rpc(ws, "GetSceneItemId", {"sceneName": SCENE, "sourceName": INPUT},
                   ignore_err=True).get("sceneItemId")
    if item_id is not None:
        _rpc(ws, "SetSceneItemTransform", {
            "sceneName": SCENE,
            "sceneItemId": item_id,
            "sceneItemTransform": {
                "boundsType": "OBS_BOUNDS_STRETCH",
                "boundsAlignment": 0,
                "boundsWidth": base_w,
                "boundsHeight": base_h,
                "positionX": 0,
                "positionY": 0,
                "alignment": 5,
            },
        }, ignore_err=True)

    # OBS ndi_source binds by the FULL "MACHINE (name)" network name; binding a bare name
    # (e.g. "2ME PGM") connects to nothing. Resolve BOTH the ingest source and this box's
    # own Main Output name to their full forms (polling discovery) BEFORE switching the
    # program scene, so a doomed run — a name that never resolves — fails fast with the
    # production program scene UNTOUCHED, never half-set-up.
    ingest_full, _ = _resolve_full(ws, INPUT, a.upstream)
    if "(" not in ingest_full:
        raise SystemExit(
            f"[obs] {a.host}: ingest source '{a.upstream}' did not resolve to a full NDI "
            f"name; aborting before touching the program scene."
        )
    if ingest_full != a.upstream:
        # Re-point to the resolved full NDI name only. overlay=True MERGES with the
        # existing settings, so the #63 genlock keys (genlock_fifo/ndi_sync) applied
        # above are PRESERVED — never set overlay=False here or this re-point would
        # full-replace the input and silently drop the genlock config (black render).
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": ingest_full},
            "overlay": True,
        }, ignore_err=True)
    # #91: the own-output resolution gate protects the NEXT OBS hop from ingesting a
    # dead bare name — but only a NON-terminal box HAS a next OBS hop. On the TERMINAL
    # box (stream) the box's own OBS can NEVER self-discover its own Main Output (NDI
    # suppresses self/loopback discovery of an output on the same machine), so a full
    # poll is GUARANTEED to exhaust its timeout doing nothing, and the abort would
    # always fire — blocking the strih→stream hop measurement. The terminal box's
    # output is tapped DIRECTLY by dev1; there is no next OBS hop to protect.
    out_full = _resolve_own_output(ws, a.host, ndi_name, terminal=a.terminal)

    # #149 SELF-VERIFY GUARD — the MACHINE guards the config, never the human. Read back the
    # probe ingest's EFFECTIVE locked baseline (after every SetInputSettings above) and assert
    # it equals the certified prod genlock input's EXACTLY. On any divergence — or no certified
    # prod baseline found — FAIL FAST with a precise per-key diagnostic, before switching the
    # program scene, so the production program is left UNTOUCHED and a config that diverges from
    # prod can never be silently measured. This runs LAST (after the resolved-name re-point) so
    # it verifies the input's true final state, mirroring the resolution gates' fail-fast order.
    # Read the probe's EFFECTIVE settings (defaults-merged) so the comparison is symmetric
    # with the certified prod settings (also defaults-merged) — see _effective_input_settings.
    probe_effective = _effective_input_settings(ws, INPUT)
    _assert_probe_matches_prod(a.host, prod_input_name, prod_settings, probe_effective)

    # Everything resolved AND self-verified — NOW switch program to the probe scene (kept to
    # the last step so any failure above leaves the live program where it was).
    _rpc(ws, "SetCurrentProgramScene", {"sceneName": SCENE})
    ws.close()
    sys.stderr.write(
        f"[obs] {a.host}: program -> {SCENE} ingest '{ingest_full}'; "
        f"Main Output NDI '{out_full}'\n"
    )
    print(out_full)  # stdout = the FULL NDI name to tap / chain for this program


def _ndi_source_list(ws, inp):
    """The full 'MACHINE (name)' NDI source names DistroAV has discovered on this
    box, read from the ndi_source_name property's item list."""
    items = _rpc(ws, "GetInputPropertiesListPropertyItems", {
        "inputName": inp, "propertyName": "ndi_source_name",
    }, ignore_err=True).get("propertyItems", [])
    return [it.get("itemValue") for it in items if it.get("itemValue")]


def _match_full(vals, bare):
    """Map a bare NDI name to its full 'MACHINE (name)' form from `vals`; returns
    `bare` unchanged if it is already full or no candidate matches."""
    for v in vals:  # already full/exact
        if v == bare:
            return v
    for v in vals:  # bare output name as the "(suffix)" of a full name
        if v.endswith(f"({bare})"):
            return v
    for v in vals:  # last resort: any substring match
        if bare in v:
            return v
    return bare


def _resolve_full(ws, inp, bare, timeout=45.0, interval=1.0):
    """Resolve `bare` to its full 'MACHINE (name)' NDI form, POLLING DistroAV discovery
    until it appears (or timeout). An OBS ndi_source binds by the full network name;
    binding the BARE Main-Output name (e.g. '2ME PGM') connects to nothing → black render
    → 0 decode on the next hop. Cold discovery may not list a just-started upstream/own
    output for a few seconds, so we wait for it rather than racing it with a fixed sleep
    (#22 verification exposed this on strih→stream). Names that are already full (contain
    '(') bind directly and pass through. Returns (full_or_bare, last_vals).

    timeout=45s (was 20s): after the harness stops cam2's camera-box + re-points the OBS
    ingest, the NDI discovery landscape reshuffles and strih's own Main Output ('2ME PGM')
    intermittently took >20s to re-appear in dev1's finder, aborting otherwise-good runs.
    This is an async network-advertisement wait, not a processing timeout."""
    if "(" in bare:  # already a full "MACHINE (name)" — binds directly, no discovery wait
        return bare, _ndi_source_list(ws, inp)
    end = time.time() + timeout
    while True:
        vals = _ndi_source_list(ws, inp)
        full = _match_full(vals, bare)
        if full != bare:
            return full, vals
        if time.time() >= end:
            sys.stderr.write(
                f"[obs] WARN: bare NDI name '{bare}' did not resolve to a full "
                f"'MACHINE (name)' within {timeout:.0f}s; binding bare (may not connect)\n"
            )
            return bare, vals
        time.sleep(interval)


def _resolve_own_output(ws, host, ndi_name, terminal):
    """#91: resolve THIS box's own Main Output NDI name to the form the next consumer
    binds against, and decide whether a non-resolution is fatal.

    NON-terminal box: its output feeds the NEXT OBS hop, which binds the full
    'MACHINE (name)' network name — a bare name connects to nothing (black render,
    0 decode downstream). So poll discovery for the full form and ABORT (before
    touching the program scene) if it never resolves: a dead next-hop ingest is a
    fatal misconfiguration.

    TERMINAL box (stream): its output is tapped DIRECTLY by dev1, and the box's own
    OBS can NEVER self-discover its own output (NDI suppresses self/loopback
    discovery of an output on the same machine) — so a full poll is guaranteed to
    exhaust its timeout for nothing and there is no next OBS hop to protect. We do a
    SHORT best-effort resolve (in case the box's OBS happens to list it), and on
    failure emit the PARENTHESIZED suffix form '(ndi_name)' rather than the bare,
    generic name. dev1's tap matches by substring (ndi.rs `name.contains`), so the
    '(name)' suffix — the codebase's canonical full-name discriminator (see
    _match_full's `endswith(f"({bare})")`) — anchors the match on the exact source
    'MACHINE (ndi_name)' instead of letting a short generic bare word (e.g. 'stream')
    collide with any other LAN source whose name merely contains it (livestream, …).
    Returns the name string to print on stdout for the next consumer to tap/chain."""
    if not terminal:
        out_full, _ = _resolve_full(ws, INPUT, ndi_name)
        if "(" not in out_full:
            raise SystemExit(
                f"[obs] {host}: own Main Output '{ndi_name}' did not resolve to a full NDI "
                f"name (the next hop would ingest a dead name); aborting before touching the "
                f"program scene."
            )
        return out_full
    # Terminal box: short best-effort resolve (its own OBS almost never lists its own
    # output, so don't burn the full 20s poll), then fall back to the precise suffix.
    out_full, _ = _resolve_full(ws, INPUT, ndi_name, timeout=2.0)
    if "(" in out_full:
        # Self-resolved to the full 'MACHINE (name)', OR ndi_name was already
        # parenthesised — either way it contains '(' and binds directly.
        return out_full
    # ndi_name here cannot contain '(' (that path returned above), so the bare name is
    # a plain token (e.g. 'stream'); wrap it as the parenthesised suffix discriminator.
    suffix_form = f"({ndi_name})"
    sys.stderr.write(
        f"[obs] {host}: WARN terminal box's own Main Output '{ndi_name}' not "
        f"self-discoverable via its own OBS (NDI loopback suppression); no downstream "
        f"OBS hop to protect — emitting the suffix form '{suffix_form}' so dev1's tap "
        f"binds the exact 'MACHINE {suffix_form}' source (not a generic substring).\n"
    )
    return suffix_form


# #163: a recorded-black program is the failure this whole action exists to prevent
# (a probe ingest that received no NDI, or a scene wired to a dead source, renders pure
# black -> every recorded frame undecodable -> a wasted multi-minute run). The pure
# decision below is the fail-fast self-check: a frame whose PEAK luma is 0 is genuinely
# black (no signal at all); any non-zero peak means the source is rendering real content.
# We key on the PEAK (max), NOT the mean: a legitimately dark-but-live camera frame can
# have a very low mean (the live 'Cam 5' read mean ~30, but a darker scene could be ~1)
# while still carrying a decodable QR — only an all-zero frame (max==0) is truly black.
#
# #312: peak-only is insufficient for a source that is mid-RENEGOTIATION (cam1's NDI right after
# its [2/8] camera-box restart). That renders an almost-entirely-black frame with a few moderately
# bright pixels — peak ~117 but mean ~2.7 (312006/312007) — which peak-only accepted instantly and
# recorded as a black program. So callers that route to a KNOWN-BRIGHT scene (the dual-QR monitor,
# mean ~105 when settled) pass a MEAN floor (min_mean>0): a frame below it is treated as not-yet-
# non-black so the existing POLL keeps WAITING until cam1 settles and delivers the real bright frame
# (a deterministic settle, not a fixed sleep). min_mean=0 (the default) keeps the original peak-only
# behavior for a legitimately dark-but-live prod camera.
def _luma_is_black(luma_max, luma_mean=None, min_mean=0):
    """#163 fail-fast self-check (pure): True iff the rendered program frame should be treated as
    black (not yet delivering real content). Black == peak luma 0 (no signal at all). Additionally,
    when *min_mean* > 0 and a *luma_mean* is given, a frame whose mean is below the floor is ALSO
    treated as black (#312: a mid-renegotiation frame has a high peak but a near-zero mean). With the
    default min_mean=0 the mean never gates (preserving the dark-but-live peak-only behavior)."""
    if int(luma_max) == 0:
        return True
    if min_mean > 0 and luma_mean is not None and luma_mean < min_mean:
        return True
    return False


def _blackcheck_verdict(luma_max, elapsed_s, timeout_s, luma_mean=None, min_mean=0):
    """#111 (pure, testable): poll-aware verdict for the #163 non-black self-check.

    The original check read the program luma ONCE, 2 s after switching to the record
    scene, and aborted on black. But a cold DistroAV NDI receiver (high genlock_preload,
    re-establishing the source from idle) needs longer than 2 s to fill its FIFO and
    render the first non-black frame — so the single read saw BLACK and aborted a fully
    healthy run (the #111 deploy incident: strih's feed reached stream fine, but the
    receiver was 2 s into a ~1 s-preload cold start). The fix is to POLL up to a timeout:

      - non-black peak              -> "OK"      proceed immediately (however early)
      - black, elapsed < timeout    -> "WAIT"    receiver may still be filling; keep polling
      - black, elapsed >= timeout   -> "BLACK"   genuinely dead source; abort the run
      - luma unreadable (None):
          elapsed < timeout         -> "WAIT"    retry the screenshot
          elapsed >= timeout        -> "UNKNOWN" never a silent OK; caller warns + proceeds
    """
    if luma_max is None:
        return "WAIT" if elapsed_s < timeout_s else "UNKNOWN"
    # #312: pass the mean + floor so a high-peak/near-black-mean renegotiation frame counts as black
    # (WAIT until the deadline) instead of falsely passing on peak alone. min_mean=0 -> peak-only.
    if not _luma_is_black(luma_max, luma_mean, min_mean):
        return "OK"
    return "WAIT" if elapsed_s < timeout_s else "BLACK"


def _program_luma(ws, scene_name):
    """Read the RENDERED program frame of *scene_name* via GetSourceScreenshot and
    return (max_luma, mean_luma) over a small downscaled PNG. Best-effort: returns
    (None, None) if the screenshot request fails or PIL is unavailable, so the caller
    can decide whether to proceed (we never BLOCK a run on the self-check being unable
    to run — only on it positively finding black)."""
    import base64
    import io

    res = _rpc(ws, "GetSourceScreenshot", {
        "sourceName": scene_name,
        "imageFormat": "png",
        "imageWidth": 320,
        "imageHeight": 180,
    }, ignore_err=True)
    data = res.get("imageData")
    if not data:
        return None, None
    try:
        from PIL import Image
    except ImportError:
        return None, None
    try:
        b64 = data.split(",", 1)[1] if data.startswith("data:") else data
        im = Image.open(io.BytesIO(base64.b64decode(b64))).convert("L")
        px = list(im.getdata())
        if not px:
            return None, None
        return max(px), sum(px) / len(px)
    except Exception:
        return None, None


def _assert_program_nonblack(ws, host, scene, label, black_hint, min_mean=None):
    """#163/#111 FAIL-FAST non-black self-check, POLLED — the ONE shared implementation used by
    both prod_scene() (step [4/8] routing) and switch() (the #312 all-cambox sweep). A black program
    records all-undecodable and wastes the whole run (#163); but a cold DistroAV receiver (high
    genlock_preload, re-establishing from idle) needs longer than a single read to render its first
    non-black frame (#111), so a black read BEFORE the deadline is WAIT (keep polling), not FAIL —
    only black AT the deadline is a genuine dead-source abort.

    *label* tags the log lines (e.g. '#163', '#312 switch'); *black_hint* is the context-specific
    guidance appended to the BLACK SystemExit. Returns on OK/UNKNOWN; raises SystemExit on a genuine
    BLACK at the deadline. Consolidating the loop in one place (was duplicated) keeps the #111/#163
    timeout-race tuning from drifting between the two call sites.

    *min_mean*, when given by the caller, OVERRIDES the env-resolved default below — see #677: the
    two call sites need DIFFERENT floors (switch()'s bright dual-QR monitor vs prod_scene()'s
    arbitrary, possibly dim, real production content), so each caller now owns its own floor instead
    of sharing one global default."""
    blackcheck_timeout = float(os.environ.get("OBS_BLACKCHECK_TIMEOUT_S", "20"))
    # #312: switch() (the ONLY caller that omits min_mean) routes to a KNOWN-BRIGHT scene (the
    # dual-QR monitor, mean ~105 when settled), so it needs a MEAN floor — a mid-renegotiation frame
    # (peak ~117 but mean ~2.7 right after cam1's [2/8] restart) then keeps the poll WAITING until
    # cam1's NDI settles, instead of falsely passing on peak and recording a black program.
    # Env-overridable; set 0 to restore pure peak-only. Default 20: well above the 2.7 garbage, below
    # the ~105 frame. #677: prod_scene() passes its OWN, looser floor explicitly (see its call site)
    # because it routes to ARBITRARY certified production content, which can legitimately be dim —
    # this default must never apply there.
    if min_mean is None:
        min_mean = float(os.environ.get("OBS_NONBLACK_MIN_MEAN", "20"))
    poll_interval = 1.0
    t0 = time.monotonic()
    while True:
        luma_max, luma_mean = _program_luma(ws, scene)
        elapsed = time.monotonic() - t0
        verdict = _blackcheck_verdict(
            luma_max, elapsed, blackcheck_timeout, luma_mean=luma_mean, min_mean=min_mean)
        if verdict == "OK":
            sys.stderr.write(
                f"[obs] {host}: {label} self-check OK — program '{scene}' NON-BLACK "
                f"(luma peak={luma_max}, mean={luma_mean:.1f}) after {elapsed:.1f}s\n"
            )
            return
        if verdict == "UNKNOWN":
            sys.stderr.write(
                f"[obs] {host}: WARN could not read program luma for the {label} non-black "
                f"self-check after {elapsed:.1f}s (GetSourceScreenshot/PIL unavailable) — "
                f"proceeding; recording-verdict still catches all-black.\n"
            )
            return
        if verdict == "BLACK":
            raise SystemExit(
                f"[obs] {host}: {label} self-check FAIL — program scene '{scene}' renders BLACK "
                f"(luma peak={luma_max}, mean={(luma_mean or 0):.1f}) after {elapsed:.1f}s. "
                f"{black_hint}"
            )
        # verdict == "WAIT": receiver may still be filling — keep polling.
        time.sleep(poll_interval)


def _restore_target(prev, target, ephemeral, scenes, saved_prev=None):
    """#163 (pure, testable): decide the scene teardown should restore PROGRAM to, given
    the program scene seen at prod-scene time (*prev*), the record *target*, whether the
    target is *ephemeral* (a scene we built — must never be restored to), the box's
    *scenes* list, and the last good run's *saved_prev* (for crash recovery).

    - ephemeral target: never restore to it; if prev was the target, recover saved_prev,
      else fall back to any other existing scene (the stream temp scene case).
    - real prod target: faithful restore — keep whatever was program, INCLUDING the target
      itself if it was already live program (don't bump the box off a legit prod scene)."""
    if ephemeral:
        if prev == target:
            prev = saved_prev or prev
        if not prev or prev == target:
            prev = next((s for s in scenes if s != target), None)
        return prev
    # Real prod scene: keep what was shown (target included). Only fill a missing prev.
    return prev or next((s for s in scenes), None)


def prod_scene(a):
    """#163: route this box's OBS PROGRAM to a CERTIFIED PRODUCTION scene and verify it
    is rendering NON-BLACK, so the recording-based E2E records the REAL production scene
    program (the same pixels a viewer sees) instead of a probe ndi_source that collides
    with the always-on prod input on the same NDI source-name (which records pure black).

    On strih the prod scene (e.g. 'Cam 5') already shows cam1 via the genlock-certified
    'NDI cam5' input; on stream the prod scene (e.g. 'PRO') already shows strih's feed via
    'NDI 2ME PGM' (#343: record that already-active scene, not a fresh ephemeral one).
    Either way: NO second receiver, NO source-name collision — the bug #163 closes.

    Steps (mirrors setup()'s safety order):
      1. Record prev program + (studio) prev preview to STATE so teardown restores them
         (reuses the SAME state keys setup uses, so teardown is unchanged).
      2. If --ensure-source is given, ensure a full-screen scene over that source exists
         (the stream temp scene). Touches ONLY that scene, never any prod scene.
      3. Read this box's DistroAV Main Output ndi_name (printed on stdout for chaining).
      4. Switch program to the named prod scene.
      5. FAIL FAST if the program renders black (GetSourceScreenshot luma peak 0) — a
         black program means the source isn't delivering; abort before StartRecord
         wastes a full run (the #163 'never waste a run on a black ingest' guard).
    """
    ws = _conn(a.host, a.password)
    try:
        prev = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")
        # #343: save the ACTUAL current program before _restore_target overwrites `prev` with
        # the teardown restore target. Used below to decide whether a SetCurrentProgramScene
        # is needed at all (already on target → skip; needs switch + ensure_source → pre-warm).
        curr_prog = prev
        studio = bool(_rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
        prev_preview = (
            _rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get("currentPreviewSceneName")
            if studio else None
        )

        scenes = [s.get("sceneName") for s in _rpc(ws, "GetSceneList").get("scenes", [])]
        target = a.program_scene
        # A scene we BUILD (--ensure-source, the stream temp scene) is EPHEMERAL: it must
        # never be a restore target (restoring program to the throwaway record scene would
        # strand it as live program). A plain --program-scene is a real existing prod scene
        # (strih 'Cam 5'): if it WAS the live program, restoring it to ITSELF is the most
        # faithful restore — don't bump the box off a legit prod scene onto an arbitrary one.
        ephemeral = bool(a.ensure_source)
        # ONE state read: reused both for the crash-recovery fallbacks and the write-back.
        state = _load_state()
        saved = state.get(a.host, {})
        prev = _restore_target(prev, target, ephemeral, scenes, saved.get("prev_scene"))
        # Preview restore mirrors the program decision (never strand the ephemeral scene
        # in preview either; for a real prod scene keep what was shown, falling back to the
        # restored program scene).
        prev_preview = _restore_target(
            prev_preview, target, ephemeral, scenes, saved.get("prev_preview")
        ) or prev
        if ephemeral and prev is None:
            sys.stderr.write(
                f"[obs] {a.host}: WARN prior program unknown/was the ephemeral record "
                f"scene; will restore to '{prev_preview}'\n"
            )
        state[a.host] = {"prev_scene": prev, "prev_preview": prev_preview}
        _save_state(state)

        # #163: optionally ensure a dedicated full-screen scene over a source (the stream
        # temp scene). It references an EXISTING prod input (e.g. 'NDI 2ME PGM') — never a
        # new receiver — so there is no source-name collision; we just stretch it to fill
        # the canvas so the recorded program is full-frame. On strih --ensure-source is
        # unused (the prod 'Cam 5' scene already exists and is full-screen).
        if a.ensure_source:
            if target not in scenes:
                _rpc(ws, "CreateScene", {"sceneName": target}, ignore_err=True)
            items = _rpc(ws, "GetSceneItemList", {"sceneName": target},
                         ignore_err=True).get("sceneItems", [])
            if not any(it.get("sourceName") == a.ensure_source for it in items):
                _rpc(ws, "CreateSceneItem",
                     {"sceneName": target, "sourceName": a.ensure_source}, ignore_err=True)
            vs = _rpc(ws, "GetVideoSettings", ignore_err=True)
            base_w = int(vs.get("baseWidth") or 1920)
            base_h = int(vs.get("baseHeight") or 1080)
            item_id = _rpc(ws, "GetSceneItemId",
                           {"sceneName": target, "sourceName": a.ensure_source},
                           ignore_err=True).get("sceneItemId")
            if item_id is not None:
                _rpc(ws, "SetSceneItemTransform", {
                    "sceneName": target, "sceneItemId": item_id,
                    "sceneItemTransform": {
                        "boundsType": "OBS_BOUNDS_STRETCH", "boundsAlignment": 0,
                        "boundsWidth": base_w, "boundsHeight": base_h,
                        "positionX": 0, "positionY": 0, "alignment": 5,
                    },
                }, ignore_err=True)
        elif target not in scenes:
            raise SystemExit(
                f"[obs] {a.host}: program scene '{target}' does not exist (and no "
                f"--ensure-source given to build it). Pass an existing certified prod "
                f"scene, or --ensure-source <prod input> to build a full-screen scene."
            )

        # This box's DistroAV Main Output NDI name — printed for the next consumer to
        # chain/tap (same stdout contract as setup()). The output must be enabled.
        out = _rpc(ws, "GetOutputSettings", {"outputName": MAIN_OUTPUT}, ignore_err=True)
        ndi_name = (out.get("outputSettings") or {}).get("ndi_name")
        if not ndi_name:
            raise SystemExit(
                f"[obs] {a.host}: DistroAV '{MAIN_OUTPUT}' is not enabled — enable it in "
                f"OBS and set its NDI name, then re-run."
            )

        # Switch program to the prod recording scene.
        # #343: two mitigations for the heavy-FIFO scene-switch blocking > OBS_OP_TIMEOUT_S:
        #   (a) already on target — skip SetCurrentProgramScene entirely (source is warm,
        #       zero activation cost, avoids re-triggering the FIFO init).
        #   (b) need to switch + --ensure-source (ephemeral scene, NDI 2ME PGM 450 ms FIFO)
        #       — use timeout_s=0 (no #328 deadline) so OBS can finish the FIFO init without
        #       being cut off at 60 s. This is the one legitimately unbounded op; all
        #       subsequent calls keep their OBS_OP_TIMEOUT_S cap.
        if curr_prog == target:
            pass  # (a) already on target: source is warm, switch is a no-op
        elif a.ensure_source:
            # (b) pre-warm: no deadline — let OBS activate the heavy FIFO without #328 cut-off
            _rpc(ws, "SetCurrentProgramScene", {"sceneName": target}, timeout_s=0)
        else:
            _rpc(ws, "SetCurrentProgramScene", {"sceneName": target})
        # In Studio Mode also set it to preview so the rendered program is the prod scene
        # (a stale preview scene keeps render-ticking and can confuse a viewer, but does
        # not affect the recorded PROGRAM output).
        if studio:
            _rpc(ws, "SetCurrentPreviewScene", {"sceneName": target}, ignore_err=True)

        # #183: FORCE the recorded prod genlock input to the test preload (1) so the run
        # measures the TRUE genlock hop (~33ms), not the prod audio-sync delay (preload≈31 ≈
        # 1s). Saved + restored on teardown so prod audio-sync is untouched after the test.
        # Only when --upstream identifies the prod input; omitted ⇒ prod preload left as-is.
        _force_test_preload(ws, a.host, getattr(a, "upstream", ""),
                            getattr(a, "test_preload", 1), state)

        # #358/#691: SNAPSHOT + SET the per-source genlock latency on the stream-box prod
        # input ('NDI 2ME PGM', passed via --test-latency-source). `test_latency_ms` is
        # `None` unless the caller explicitly set GENLOCK_TEST_LATENCY_MS — see
        # `resolve_test_latency_ms`'s doc: an unset value is now auto-derived from the
        # box's OWN current latency (current value if already >= 500ms, else the original
        # 1000ms fallback), not a blind forced 1000ms. Disables gpu_delay (Render-Delay)
        # filters for the test window so they don't mask the effective FIFO depth in audit
        # lines. Saved (unconditionally, #691) and restored in teardown (prod A/V-align
        # restored exactly). The delivery-verify gate (live FIFO audit log read vs set
        # value) is run by the supervisor against the live rig.
        _snapshot_and_set_test_latency(
            ws, a.host,
            getattr(a, "test_latency_source", ""),
            getattr(a, "test_latency_ms", None),
            state,
        )

        # #163/#111 FAIL-FAST non-black self-check, POLLED (shared helper): a black program records
        # all-undecodable and wastes the whole run; poll until non-black or the timeout (a cold
        # DistroAV receiver needs longer than a single read to fill its FIFO). Only black AT the
        # deadline aborts BEFORE StartRecord.
        # #677: unlike switch() (#312's bright dual-QR monitor), prod_scene() routes to whatever the
        # CERTIFIED production scene shows — arbitrary real camera content that can legitimately be
        # dim (live repro: peak=231, mean=18.0, genuinely non-black). Pass a LOOSER, dedicated floor
        # (default 5) instead of the #312-tuned shared default (20): still well above the ~2.7
        # mid-renegotiation garbage frame, but below any real dim content.
        _assert_program_nonblack(
            ws,
            a.host,
            target,
            "#163",
            f"The source is not delivering frames; aborting BEFORE StartRecord so a black recording "
            f"never wastes a full run. Check the certified prod input feeding '{target}' is "
            f"receiving NDI.",
            min_mean=float(os.environ.get("OBS_NONBLACK_MIN_MEAN_PROD", "5")),
        )

        sys.stderr.write(
            f"[obs] {a.host}: program -> {target} (prod scene); Main Output NDI "
            f"'{ndi_name}'\n"
        )
        print(ndi_name)  # stdout = the FULL/own NDI name to chain/tap for this program
    finally:
        ws.close()


def teardown(a):
    state = _load_state()  # corruption-safe: a bad state file must not stop the restore
    try:
        ws = _conn(a.host, a.password)
        # #358/#691: restore the prod stream input's per-source genlock latency (450ms
        # A/V-align) and re-enable any gpu_delay filters that were disabled for the test.
        # BEFORE preload restore and scene switches so prod is back on its A/V-align
        # config immediately. LOUD warn if read-back ≠ snapshot (mirrors #246 burn-verify
        # at recording-e2e.sh:316). `calibrated_latency_ms` (OPTIONAL, #691 belt-and-
        # braces) cross-checks the restored value against av-sync-last.json's known-good
        # prod value when the caller supplied one — see _restore_test_latency's doc.
        _restore_test_latency(
            ws, a.host, state, getattr(a, "calibrated_latency_ms", None)
        )
        # #183: restore the prod input's genlock_preload that prod-scene forced to the test
        # value, BEFORE anything else, so prod audio-sync is back to its production depth even
        # if a later step warns. No-op when nothing was forced.
        _restore_test_preload(ws, a.host, state)
        host_state = state.get(a.host, {})
        prev = host_state.get("prev_scene")
        # #343: a SetCurrentProgramScene to the scene ALREADY on program HANGS (>60s, no ws
        # response, #328) when that scene carries the heavy NDI 2ME PGM source mid-renegotiation —
        # the teardown half of the proof-blocking hang. prod_scene already skips a same-scene
        # switch; teardown must too. Only switch when the current program differs from the target.
        if prev:
            curr_prog = _rpc(ws, "GetCurrentProgramScene", ignore_err=True).get(
                "currentProgramSceneName")
            if curr_prog != prev:
                _rpc(ws, "SetCurrentProgramScene", {"sceneName": prev}, ignore_err=True)
        # Restore the prior PREVIEW too (Studio Mode): leaving the probe scene in preview
        # keeps its idle ndi_source active and render-ticking the genlock FIFO (#70 underrun
        # spam). Falls back to the program scene when no prior preview was recorded. Same #343
        # skip-if-already-there guard — a same-scene preview set hangs on the heavy source too.
        prev_preview = host_state.get("prev_preview") or prev
        if prev_preview:
            curr_preview = _rpc(ws, "GetCurrentPreviewScene", ignore_err=True).get(
                "currentPreviewSceneName")
            if curr_preview != prev_preview:
                _rpc(ws, "SetCurrentPreviewScene", {"sceneName": prev_preview}, ignore_err=True)
        # Idle the NDI receiver but KEEP the stable scene+input for the next run (#22).
        # Clearing ndi_source_name makes DistroAV tear the receiver down cleanly (destroying
        # an ndi_source while it is actively receiving the 1080p feed faults the NDI runtime
        # and crashes OBS). genlock_fifo is also turned OFF so the dormant input does not run
        # the genlock consume path against an empty queue -> the perpetual underrun-audit spam
        # that corrupted the cumulative FIFO audit (#70). setup re-applies _PROBE_NDI_SETTINGS
        # (genlock_fifo=True) on the next run. Reuse is what stops the per-run input
        # accumulation the fork caused.
        _rpc(ws, "SetInputSettings", {
            "inputName": INPUT,
            "inputSettings": {"ndi_source_name": "", "genlock_fifo": False},
            "overlay": True,
        }, ignore_err=True)
        ws.close()
        sys.stderr.write(
            f"[obs] {a.host}: restored program -> {prev}, preview -> {prev_preview}, "
            f"probe input idled (genlock off, reused next run)\n"
        )
    except Exception as e:  # teardown must never raise
        sys.stderr.write(f"[obs] {a.host}: teardown warning: {e}\n")


def _wait_record_idle(ws, host, timeout_s=None, poll_s=None):
    """#355: POLL GetRecordStatus until the recording output is idle (finalized), then return.

    A large orphan MP4 (the live 24.5 GB stream-box file) takes many seconds to FINALIZE after
    StopRecord; StartRecord returns {code:500} while the output is still active. Poll every
    *poll_s* until `outputActive` is False, bounded by *timeout_s*. If it never idles, FAIL LOUD
    (SystemExit) rather than charge ahead into a doomed StartRecord. `outputActive` defaults to
    True when absent so an unreadable status is treated as "still active" (never a silent pass).
    """
    timeout_s = RECORD_FINALIZE_TIMEOUT_S if timeout_s is None else timeout_s
    poll_s = RECORD_FINALIZE_POLL_S if poll_s is None else poll_s
    deadline = time.monotonic() + timeout_s
    while True:
        if not _rpc(ws, "GetRecordStatus").get("outputActive", True):
            return
        if time.monotonic() >= deadline:
            raise SystemExit(
                f"[obs] {host}: recording output still ACTIVE {timeout_s:.0f}s after StopRecord — "
                f"a stranded recording never finalized; aborting before StartRecord 500s on it "
                f"(#355). Free the orphan on the box, or raise OBS_RECORD_FINALIZE_TIMEOUT_S if a "
                f"very large file legitimately needs longer to finalize."
            )
        time.sleep(poll_s)


def _record_liveness_verdict(active_flags, byte_counts):
    """#627 pure decision: given the SEQUENCE of GetRecordStatus samples taken right after
    StartRecord, decide whether the recording output is genuinely LIVE or DEAD-ON-ARRIVAL.

    Never silently passes:
      - no samples at all -> DEAD (a caller bug, never treated as "live by default"),
      - outputActive never goes True during the whole window -> DEAD (never started),
      - outputActive goes False AFTER having been True -> DEAD (the output died during the
        check window — #627's original "started then died" symptom),
      - the LAST sample's outputBytes <= 0 -> DEAD (the exact #627 symptom: StartRecord
        reported success but the output wrote nothing for the whole run),
      - byte count did not GROW across the window (>=2 samples, last <= first) -> DEAD (the
        output started writing something then stalled/froze immediately — byte>0-only would
        miss this).

    #710 cold-start tolerance: a LEADING run of outputActive=False is tolerated (NOT treated
    as dead) as long as every sample from the first True onward stays True — a fresh OBS
    process's NVENC/CUDA cold-init can take slightly longer than one liveness poll to flip
    outputActive to True, even though the recording genuinely starts fine within the SAME
    liveness window's next sample (live repro: active=[False, True] bytes=[0, 623840] on a
    cold imag-nb OBS restart). This does NOT weaken the "started then died" or "never
    started" checks above — those keep failing hard, unchanged.

    Returns (is_live: bool, reason: str) — *reason* is empty when is_live is True, else it
    explains the failure for the caller's abort message.
    """
    if not active_flags:
        return False, "no GetRecordStatus samples were taken"
    first_true = next((i for i, active in enumerate(active_flags) if active), None)
    if first_true is None:
        return False, (
            f"outputActive stayed False for the whole liveness window (samples={active_flags}) "
            f"— recording never started"
        )
    if any(not active for active in active_flags[first_true:]):
        return False, (
            f"outputActive went False during the liveness window (samples={active_flags})"
        )
    if not byte_counts or byte_counts[-1] <= 0:
        got = byte_counts[-1] if byte_counts else "unknown"
        return False, (
            f"outputBytes stayed at {got} — the recording output is writing nothing (#627: "
            f"StartRecord can report success while the output silently writes 0 bytes for the "
            f"whole run)"
        )
    if len(byte_counts) >= 2 and byte_counts[-1] <= byte_counts[0]:
        return False, (
            f"outputBytes did not grow across the liveness window ({byte_counts}) — the "
            f"recording appears to have stalled immediately after starting"
        )
    return True, ""


def _assert_record_is_live(ws, host, samples=None, poll_s=None):
    """#627: poll GetRecordStatus a few times right after StartRecord and FAIL LOUD
    (SystemExit) if the output is not genuinely active + writing growing bytes — instead of
    silently proceeding into the caller's multi-minute sleep, which is how a dead-on-arrival
    recording (outputActive/outputBytes both wrong from the start) went undetected until the
    file was fetched at the END of a run, wasting the entire run duration.

    This is a fail-fast DETECTION, not a root-cause fix — see RECORD_LIVENESS_SAMPLES's
    module-level comment and #627 for what remains unproven.

    `outputBytes` defaults to -1 when absent from the response (an older obs-ws build) so an
    unreadable byte count is treated as DEAD rather than silently passing as "live".
    """
    samples = RECORD_LIVENESS_SAMPLES if samples is None else samples
    poll_s = RECORD_LIVENESS_POLL_S if poll_s is None else poll_s
    active_flags = []
    byte_counts = []
    for _ in range(samples):
        time.sleep(poll_s)
        status = _rpc(ws, "GetRecordStatus")
        active_flags.append(status.get("outputActive", False))
        byte_counts.append(status.get("outputBytes", -1))
    is_live, reason = _record_liveness_verdict(active_flags, byte_counts)
    # #767 byte-counter grace: the output IS active but bytes are still exactly 0 -- a slow
    # NVENC cold-init (imag) rather than a dead output. Keep polling at 1s inside the bounded
    # grace budget; pass as soon as bytes appear, abort per #627 if the whole budget stays 0.
    grace_used = 0.0
    while (
        not is_live
        and active_flags[-1]
        and byte_counts[-1] == 0
        and grace_used < RECORD_LIVENESS_BYTES_GRACE_S
    ):
        time.sleep(1)
        grace_used += 1.0
        status = _rpc(ws, "GetRecordStatus")
        active_flags.append(status.get("outputActive", False))
        byte_counts.append(status.get("outputBytes", -1))
        is_live, reason = _record_liveness_verdict(active_flags, byte_counts)
    if not is_live:
        raise SystemExit(
            f"[obs] {host}: recording FAILED the post-start liveness check — {reason}. "
            f"Aborting NOW instead of burning the full run on a dead-on-arrival recording "
            f"(#627). active={active_flags} bytes={byte_counts}"
        )
    cold_start_note = ""
    if grace_used > 0:
        cold_start_note = (
            f" (bytes appeared only after {grace_used:.0f}s grace -- slow NVENC cold-init)"
        )
    if active_flags and not active_flags[0]:
        # #710: surface when the cold-start tolerance actually fired, so a run log can be
        # grepped for how often the FIRST post-restart StartRecord needed the extra sample.
        # (+= so the #767 grace note above survives when BOTH fired -- the typical imag case.)
        cold_start_note += " (cold start — first sample was still warming up, tolerated per #710)"
    sys.stderr.write(
        f"[obs] {host}: recording liveness OK (active={active_flags} bytes={byte_counts})"
        f"{cold_start_note}\n"
    )


def record(a):
    """#105 recording-based E2E: control OBS program recording over the WebSocket.

    --action start : begin recording the program output (the probe scene is on
                     program from setup()). Prints nothing on success. #627: after
                     StartRecord, polls GetRecordStatus a few seconds and FAILS LOUD
                     (SystemExit) if the output isn't genuinely active + writing growing
                     bytes — never silently proceeds into a dead-on-arrival recording.
    --action stop  : stop recording; prints the recorded file's ABSOLUTE PATH on the
                     OBS host (StopRecord returns outputPath in obs-ws 5.1+). The
                     harness then downloads that file from the host to dev1.
    --action status: prints `active=<bool> path=<current>` (for diagnostics).
    --action guard : #524 pre-event stray-recording guard. If a recording is active,
                     StopRecord (KEEPS the file) + WARN loud naming the host, the
                     stray file, and its timecode; prints `stray=<bool> path=<file>`.
                     Never StartRecord's — this is a safety check, not a control op.

    Recording uses OBS's CONFIGURED recording output (format/encoder/dir set in
    OBS Settings > Output). The harness sets the program scene to the probe scene via
    setup() first, so what is recorded is exactly the program a viewer would see —
    the #105 acceptance #1 "delivered only when OBS shows it in program out".
    """
    ws = _conn(a.host, a.password)
    try:
        if a.action == "start":
            status = _rpc(ws, "GetRecordStatus")
            if status.get("outputActive", False):
                # Already recording (a prior run's leftover orphan) — stop it first so this
                # run gets a clean single file, never appended to a stale one. #355: a large
                # orphan (the live 24.5 GB stream-box file) takes many seconds to FINALIZE;
                # the old flat sleep(1.0) then ran StartRecord while the output was still
                # active → OBS {code:500} aborted the run. Log LOUD, then POLL to idle.
                tc = status.get("outputTimecode", "?")
                sys.stderr.write(
                    f"WARN: {a.host} had an orphan recording active tc={tc} — finalizing it "
                    f"first (#355)\n"
                )
                _rpc(ws, "StopRecord", ignore_err=True)
                _wait_record_idle(ws, a.host)
            _rpc(ws, "StartRecord")
            sys.stderr.write(f"[obs] {a.host}: recording STARTED\n")
            # #627: StartRecord succeeding is NOT proof the output is actually writing —
            # verify it before the caller sleeps through the whole run duration.
            _assert_record_is_live(ws, a.host)
        elif a.action == "stop":
            out = _rpc(ws, "StopRecord", ignore_err=True)
            path = (out or {}).get("outputPath", "")
            if not path:
                # Fallback: some obs-ws builds return no path on StopRecord — read the
                # record directory + last status so the caller still gets a location.
                sys.stderr.write(
                    f"[obs] {a.host}: StopRecord returned no outputPath; "
                    f"check the OBS record directory\n"
                )
            print(path)  # the ONLY stdout line: the recorded file path on the host
            sys.stderr.write(f"[obs] {a.host}: recording STOPPED -> {path}\n")
        elif a.action == "status":
            s = _rpc(ws, "GetRecordStatus")
            print(f"active={s.get('outputActive', False)} "
                  f"path={s.get('outputPath', '')}")
        elif a.action == "guard":
            # #524: pre-event stray-recording guard. strih's runaway 265.9 GiB recording
            # (~11h to full at 21 Mb/s) filled the disk mid-event, and a SECOND stray
            # recording (18.57 GiB) started the SAME event day — both were only caught and
            # stopped MANUALLY. rig-mode.sh's EVENT mode now calls this on every broadcast
            # box FIRST: if a recording is active, StopRecord (KEEPS the file — the operator
            # may still need it) and warn LOUD naming the host, the stray file, and how long
            # it had been running, so recurrence is caught automatically, not by luck.
            status = _rpc(ws, "GetRecordStatus")
            if status.get("outputActive", False):
                tc = status.get("outputTimecode", "?")
                out = _rpc(ws, "StopRecord", ignore_err=True)
                path = (out or {}).get("outputPath", "")
                sys.stderr.write(
                    f"WARN: {a.host} had a STRAY recording running (tc={tc}) — stopped it "
                    f"(#524); file kept at {path or '<unknown path>'}\n"
                )
                print(f"stray=true path={path}")
            else:
                print("stray=false path=")
        else:
            raise SystemExit(f"unknown --action {a.action!r}")
    finally:
        ws.close()


def stream_status(a):
    """#722 EVENT-mode CONTRACT item 4: read-only `GetStreamStatus`, printed the SAME
    "active=<bool> path=<...>" shape `record --action status` already uses (path is always
    empty for streaming -- there's no output file -- kept for a stable, easily-`grep`able
    two-field format across both actions). Never starts/stops anything -- this is a pure check,
    the streaming-side counterpart of `record --action status`."""
    ws = _conn(a.host, a.password)
    try:
        s = _rpc(ws, "GetStreamStatus")
        print(f"active={s.get('outputActive', False)} path=")
    finally:
        ws.close()


def latency_check(a):
    """#722 EVENT-mode CONTRACT item 6: is *a.source*'s `genlock_latency_ms_src` (on *a.host*)
    equal to the CALIBRATED value from av-sync-last.json (the #691 stomp-protection prod source
    of truth)? If not, RESTORE it to the calibrated value (a test window may have left it off --
    event mode must actively fix this, not just report it) and re-verify. Prints
    "current=<int|unknown> calibrated=<int> restored=<bool> final=<int|unknown>" and exits 0 iff
    the FINAL value (after any restore attempt) matches the calibrated value -- exits 1 if it
    could not be brought back in line (never silently reports success on an unrestored
    mismatch)."""
    ws = _conn(a.host, a.password)
    try:
        settings = _rpc(ws, "GetInputSettings", {"inputName": a.source}, ignore_err=True).get(
            "inputSettings", {}
        )
        current = settings.get(_GENLOCK_SRC_LATENCY_KEY)
        restored = False
        final = current
        if current != a.calibrated_ms:
            sys.stderr.write(
                f"[latency-check] {a.host}: '{a.source}' {_GENLOCK_SRC_LATENCY_KEY}={current!r} "
                f"!= calibrated={a.calibrated_ms} -- RESTORING to the calibrated value.\n"
            )
            _rpc(
                ws,
                "SetInputSettings",
                {
                    "inputName": a.source,
                    "inputSettings": {_GENLOCK_SRC_LATENCY_KEY: a.calibrated_ms},
                    "overlay": True,
                },
                ignore_err=True,
            )
            readback = _rpc(
                ws, "GetInputSettings", {"inputName": a.source}, ignore_err=True
            ).get("inputSettings", {})
            final = readback.get(_GENLOCK_SRC_LATENCY_KEY)
            restored = True
        ok = final == a.calibrated_ms
        print(f"current={current} calibrated={a.calibrated_ms} restored={restored} final={final}")
        if not ok:
            sys.stderr.write(
                f"FAIL: [latency-check] {a.host}: '{a.source}' could not be brought to the "
                f"calibrated value {a.calibrated_ms} (final={final!r}) -- manual check required.\n"
            )
        sys.exit(0 if ok else 1)
    finally:
        ws.close()


def switch(a):
    """#312 Phase-2 all-cambox sweep: cut strih PROGRAM to *a.program_scene* and confirm it
    renders NON-BLACK, then print the switch wall-clock epoch-ns (``time.time_ns()``) on stdout.

    The all-cambox sweep (scripts/recording-e2e.sh ALL_CAMBOX=1) cuts each active cambox into
    strih program for ~SEGMENT_SECS while ONE continuous stream recording runs; the printed ns is
    the switch BOUNDARY the harness records on the burn ``gen_ts_ns`` timeline (dev1 CLOCK_REALTIME,
    DanteSync-slaved to the painter) to build the verdict's --switch-schedule windows.

    LIGHTWEIGHT vs prod_scene(): NO STATE save, NO genlock_preload force, NO upstream-resolve /
    own-output dance — the prod scenes already exist and were routed by prod_scene() in step [4/8];
    this only re-points PROGRAM among them per segment. It DOES reuse the #163/#111 non-black
    self-check (the pure ``_program_luma`` + ``_blackcheck_verdict`` helpers, POLLED) so a segment
    that switches to a dead/black cambox scene fails LOUDLY instead of silently recording a black,
    all-undecodable ~30s window. The boundary ns is captured IMMEDIATELY after the switch lands (so
    it marks when the new cambox enters program); the black-check polls AFTER and never moves it."""
    ws = _conn(a.host, a.password)
    try:
        _rpc(ws, "SetCurrentProgramScene", {"sceneName": a.program_scene})
        switch_ns = time.time_ns()  # the boundary — right after the switch lands
        # Same POLLED non-black self-check prod_scene uses (shared helper) — a dead/black cambox
        # scene fails loud instead of silently recording a black, all-undecodable segment.
        _assert_program_nonblack(
            ws,
            a.host,
            a.program_scene,
            "#312 switch",
            "The cambox feeding it is not delivering frames; aborting the sweep so a black segment "
            "never wastes the run.",
        )
    finally:
        ws.close()
    print(switch_ns)  # stdout = the switch boundary epoch-ns (burn gen_ts_ns timeline)


def _rig_busy_partition(diagnostics):
    """#649/#657 (pure, testable — no I/O): partition per-box streaming/recording booleans into
    the three mutually-exclusive categories both _rig_busy_hint (the human-readable diagnosis)
    and _stray_recording_hosts (the #657 self-heal decision) key on:
      - recording=true, streaming=false on a box  -> matches a stray harness leftover (#649).
      - recording=true, streaming=true  on a box  -> matches a REAL broadcast; do NOT touch it.
      - streaming=true,  recording=false on a box -> doesn't match the harness's own pattern
                                                      either way; flag for a manual look.

    *diagnostics* is a list of ``{"host": str, "streaming": bool, "recording": bool,
    "recordTimecode": str|None}`` dicts (one per box). Returns
    ``(real_broadcast, recording_only, streaming_only)`` — each a list of host labels.
    """
    recording_only = [d["host"] for d in diagnostics if d["recording"] and not d["streaming"]]
    real_broadcast = [d["host"] for d in diagnostics if d["recording"] and d["streaming"]]
    streaming_only = [d["host"] for d in diagnostics if d["streaming"] and not d["recording"]]
    return real_broadcast, recording_only, streaming_only


def _stray_recording_hosts(diagnostics):
    """#657 (pure, testable — no I/O): hosts EXACTLY matching "our own stray recording"
    (recording ON, streaming OFF) — the same signature _rig_busy_hint calls out as "matches OUR
    OWN stray/E2E test recording" (#649). Exposed as its own function (not just baked into the
    hint STRING) so rig-busy-gate.sh's self-heal decision — StopRecord a box after it shows
    EXACTLY this signature for several consecutive polls — can act on the structured list
    directly, without re-parsing hint prose. NEVER includes a host that is also streaming (a real
    broadcast always streams+records together, per the same heuristic _rig_busy_hint uses) — so
    a caller iterating this list can never accidentally touch a real broadcast.
    """
    _real_broadcast, recording_only, _streaming_only = _rig_busy_partition(diagnostics)
    return recording_only


def _rig_busy_hint(diagnostics):
    """#649 item 3 (pure, testable — no I/O): turn per-box streaming/recording booleans into a
    short plain-English hint that distinguishes OUR OWN stray/leftover test recording from a real
    broadcast, so a future RIG_BUSY incident is a 2-minute diagnosis instead of a manual
    investigation.

    Live incident this codifies (2026-07-10, #649): a cancelled recording-e2e.sh CI run left strih
    AND stream recording (GetRecordStatus.outputActive=true) with NO streaming active, at 4 AM —
    every later gate run then saw RIG_BUSY with no way to tell "our own leftover" from "a real
    broadcast" apart from SSHing in and reading OBS by hand.

    The heuristic matches how this rig is actually used: a REAL broadcast streams (to FB/YouTube)
    AND records at the same time (see scripts/rig-mode.sh EVENT mode); recording-e2e.sh (this
    project's own harness) ONLY ever calls StartRecord — it never touches GetStreamStatus. See
    _rig_busy_partition for the shared category logic.

    *diagnostics* is a list of ``{"host": str, "streaming": bool, "recording": bool,
    "recordTimecode": str|None}`` dicts (one per box). Returns "" when nothing is busy (no hint
    needed).
    """
    real_broadcast, recording_only, streaming_only = _rig_busy_partition(diagnostics)
    parts = []
    if real_broadcast:
        parts.append(
            f"{'/'.join(real_broadcast)}: recording AND streaming both ON -- matches a REAL "
            f"broadcast; do NOT stop it automatically."
        )
    if recording_only:
        parts.append(
            f"{'/'.join(recording_only)}: recording ON but streaming OFF -- matches OUR OWN "
            f"stray/E2E test recording left by a cancelled recording-e2e.sh run (#649), not a "
            f"live broadcast (a real broadcast streams AND records together). Check the "
            f"outputTimecode above (a long timecode at an off-hour is a strong tell), then clear "
            f"it: python3 scripts/obs_phase2.py record --host <ip> --action stop (StopRecord "
            f"only -- keeps the file, never touches program routing)."
        )
    if streaming_only:
        parts.append(
            f"{'/'.join(streaming_only)}: streaming ON but recording OFF -- does not match the "
            f"harness's own record-only pattern; check manually."
        )
    return " ".join(parts)


# #651 live incident (2026-07-10, PR #647 run 29065733523): stream's OBS process restarted
# mid-broadcast (a legitimate event, not an outage) and the ONE WebSocket connection attempt hit
# `[Errno 111] Connection refused` during that ~1-2s window — rig_busy_check() had no retry, so
# it immediately reported that box unreachable (fail-closed, exit 3) and aborted the whole 30-min
# rig-busy-gate wait, discarding up to 25 minutes of correctly-observed "still busy" state. Retry
# a per-box connect+query a bounded number of times with a short backoff BEFORE counting it as
# unreachable — a single transient blip now recovers within the SAME check; a GENUINE persistent
# outage (every attempt fails) still fails closed exactly as before (never weakened to fail-open).
RIG_BUSY_QUERY_RETRIES = int(os.environ.get("RIG_BUSY_QUERY_RETRIES", "2"))
RIG_BUSY_QUERY_RETRY_SLEEP_S = float(os.environ.get("RIG_BUSY_QUERY_RETRY_SLEEP_S", "2"))


def _query_box_status(host, password):
    """Connect + GetStreamStatus/GetRecordStatus for one box, retrying a transient failure
    (connection refused, timeout, an RPC-level error) up to RIG_BUSY_QUERY_RETRIES extra times
    with a RIG_BUSY_QUERY_RETRY_SLEEP_S backoff between attempts (#651). Returns
    (stream_status, record_status) on the first success; re-raises the LAST exception once every
    attempt (1 + RIG_BUSY_QUERY_RETRIES total) has failed — the caller then correctly treats the
    box as genuinely unreachable and fails closed."""
    last_exc = None
    for attempt in range(RIG_BUSY_QUERY_RETRIES + 1):
        try:
            ws = _conn(host, password)
            try:
                return _rpc(ws, "GetStreamStatus"), _rpc(ws, "GetRecordStatus")
            finally:
                ws.close()
        except Exception as e:  # noqa: BLE001 - any connect/RPC failure is a retry candidate
            last_exc = e
            if attempt < RIG_BUSY_QUERY_RETRIES:
                time.sleep(RIG_BUSY_QUERY_RETRY_SLEEP_S)
    raise last_exc


def rig_busy_check(a):
    """#406/#312 item5: query BOTH strih and stream OBS WebSocket for GetStreamStatus.outputActive
    and GetRecordStatus.outputActive (4 booleans total) and report whether the rig is genuinely busy
    with a REAL broadcast/recording right now.

    This is the pre-flight signal the automatic `pull_request`-triggered full-path-e2e CI gate
    (scripts/rig-busy-gate.sh) uses before it reroutes strih/stream's production OBS program scenes
    to run the real E2E — driving the recording harness over a LIVE broadcast would be a genuine
    production incident, not just a wasted CI run.

    Prints ONE line of JSON to stdout:
    ``{"busy": bool, "reasons": [str, ...], "diagnostics": [...], "hint": str}``.
      - busy=false, reasons=[]     -> rig fully idle on both boxes, safe to proceed. Exit 0.
      - busy=true,  reasons=[...]  -> at least one box is streaming and/or recording; the caller must
                                      NOT run the E2E now (exit 0 — the caller decides retry/backoff).
      - WS unreachable on EITHER box -> never silently reported as busy=false (a rig we can't
                                         observe must FAIL CLOSED). Exit 3.

    #649 item 3: when busy, ``diagnostics`` carries per-box streaming/recording booleans + the
    recording's outputTimecode, and ``hint`` is a short plain-English pointer (via _rig_busy_hint,
    above) distinguishing a stray leftover test recording from a real broadcast — so a future
    RIG_BUSY incident is diagnosable straight from the CI log, no manual SSH/OBS inspection needed.
    """
    reasons = []
    errors = []
    diagnostics = []
    for label, host in (("strih", a.strih_host), ("stream", a.stream_host)):
        try:
            stream_status, record_status = _query_box_status(host, a.password)
        except Exception as e:
            # Every attempt (1 + RIG_BUSY_QUERY_RETRIES retries, #651) failed — a GENUINE
            # persistent outage, not a transient blip. Fail CLOSED (exit 3), never busy=false.
            errors.append(f"{label} ({host}) unreachable: {e}")
            continue
        stream_active = bool(stream_status.get("outputActive"))
        record_active = bool(record_status.get("outputActive"))
        record_tc = record_status.get("outputTimecode") if record_active else None
        diagnostics.append({
            "host": label, "streaming": stream_active, "recording": record_active,
            "recordTimecode": record_tc,
        })
        if stream_active:
            reasons.append(f"{label} is streaming (GetStreamStatus.outputActive=true)")
        if record_active:
            tc_suffix = f", outputTimecode={record_tc}" if record_tc else ""
            reasons.append(f"{label} is recording (GetRecordStatus.outputActive=true{tc_suffix})")

    if errors:
        print(json.dumps({"busy": None, "reasons": errors}))
        sys.exit(3)

    busy = bool(reasons)
    out = {"busy": busy, "reasons": reasons, "diagnostics": diagnostics}
    if busy:
        hint = _rig_busy_hint(diagnostics)
        if hint:
            out["hint"] = hint
        # #657: structured list of hosts matching "our own stray recording" (recording ON,
        # streaming OFF) — rig-busy-gate.sh's self-heal decision reads this directly instead of
        # re-parsing the hint prose.
        stray = _stray_recording_hosts(diagnostics)
        if stray:
            out["stray_hosts"] = stray
    print(json.dumps(out))


def open_projectors(a):
    """#758 preflight — imag-nb's Multiview AND Program projectors must be OPEN before ANY run
    starts (the user's explicit, binding requirement: "MULTIVIEW MUSI BYT ZAPNUTE ako podmienka
    preflight pred tym nez sa rozbehne akykolvek test" — a run must NEVER begin with Multiview
    closed).

    obs-websocket 5.x has NO "is a projector currently open" introspection request (no
    GetProjectorList equivalent exists in the protocol) — so this ALWAYS, idempotently, OPENS
    both projectors via OpenVideoMixProjector rather than trying to check-then-open. Opening an
    ALREADY-open projector on the same monitor just re-positions/replaces the same window
    (harmless) — this is how "auto-open if closed" works without a separate check step the API
    can't actually provide. `_rpc`'s default `ignore_err=False` means a failed request RAISES
    (propagates as a non-zero exit) — the caller's preflight step must never silently continue
    with a projector that failed to open.

    #840: monitor indices are DERIVED from a live GetMonitorList call, keyed on each monitor's
    connector TYPE (HDMI = the external Program projector; anything else = the internal panel
    driving Multiview) — NEVER hardcoded. The old `monitorIndex 0 = DP-0 -> Multiview /
    monitorIndex 1 = HDMI-0 -> Program` mapping only worked "by luck" because the index ORDER
    happened to match the incumbent box's topology; the replacement notebook enumerates
    eDP-1/HDMI-1 instead of DP-0/HDMI-0, and a box that ever enumerates HDMI as index 0 would have
    silently sent Program to the panel and Multiview to the projector. Mirrors
    imag_scenes.py::projector()'s existing, already-correct selection rule (#522/#488) — the two
    scripts have no shared module today (a separate #791-class scaffolding gap, out of THIS
    ticket's scope), so the small selection logic is duplicated here rather than imported. Unlike
    imag_scenes.py::projector() (an operator-convenience script that only WARNs on a missing
    panel), this function is a preflight/verify GATE (recording-e2e.sh's `[0/8]`,
    verify-imag.sh) — it FAILS LOUD (raises) when EITHER expected connector is absent, never
    silently continues.

    #882: a failure to even establish the WebSocket session (OBS process not accepting the
    handshake, wrong password, connection dropped mid-negotiation) is caught HERE and re-raised
    labelled as a connection/handshake failure — distinct from the "no matching monitor"
    RuntimeErrors below, which are a genuinely different cause (the connection succeeded; the
    box's reported monitors just don't include the expected connector type). The imag-nb outage
    this issue investigates showed a single generic fallback message for ANY failure ("check
    DP-0/HDMI-0 are connected monitors") even when the true cause was "OBS was not running at
    all" — recording-e2e.sh's own preflight now probes process/port liveness separately
    (scripts/lib/imag-obs-reachability.sh) BEFORE calling this, so by the time this raises, the
    remaining real causes are exactly: handshake/auth (this branch) or no matching monitor
    (below)."""
    try:
        ws = _conn(a.host, a.password)
    except Exception as e:
        raise RuntimeError(
            f"could not establish an OBS WebSocket handshake/auth session with {a.host} -- {e}"
        ) from e
    try:
        mons = _rpc(ws, "GetMonitorList").get("monitors", [])
        panel = [m for m in mons if "HDMI" not in m.get("monitorName", "")]
        hdmi = [m for m in mons if "HDMI" in m.get("monitorName", "")]

        if not panel:
            raise RuntimeError(
                "no panel (non-HDMI) monitor detected for the Multiview projector -- "
                f"(monitors: {[m.get('monitorName') for m in mons]})"
            )
        _rpc(ws, "OpenVideoMixProjector", {
            "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW",
            "monitorIndex": panel[0]["monitorIndex"],
        })
        print(f"opened/confirmed Multiview projector on monitorIndex {panel[0]['monitorIndex']} "
              f"({panel[0].get('monitorName')}) [panel]")

        if not hdmi:
            raise RuntimeError(
                "no HDMI projector monitor detected -- connect the HDMI monitor first "
                f"(monitors: {[m.get('monitorName') for m in mons]})"
            )
        _rpc(ws, "OpenVideoMixProjector", {
            "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM",
            "monitorIndex": hdmi[0]["monitorIndex"],
        })
        print(f"opened/confirmed Program projector on monitorIndex {hdmi[0]['monitorIndex']} "
              f"({hdmi[0].get('monitorName')}) [HDMI]")
    finally:
        ws.close()


def ensure_studio_mode_on(a):
    """#767 preflight — Studio Mode must be ON on EVERY broadcast box, imag included (user hard
    rule, 2026-07-15: without Studio Mode the multiview's Preview cell is DEAD — "studio mode je
    'MUST BE', NEMOZES HO PODLA NALADY VYPINAT"). This INVERTS the former #758
    ensure-studio-mode-off step, which was written when Studio ON measurably collapsed imag's
    render (38-42fps/~23ms on the pre-#767 distroav.so — receiver teardown churn on the preview
    scene's hide/show). With the #767 keep-alive DistroAV build the churn is gone and imag holds
    60.0fps / ~1.8ms render WITH Studio ON + Multiview + 7 cams + overlays (measured 2026-07-15,
    5x5s GetStats samples). The gate therefore now measures render health in the PRODUCTION
    state: Studio ON. ALWAYS (idempotently) turns it ON, never silently leaves a stale OFF state
    to hide a Studio-ON render regression from the render-health preflight."""
    ws = _conn(a.host, a.password)
    try:
        before = bool(_rpc(ws, "GetStudioModeEnabled", ignore_err=True).get("studioModeEnabled"))
        if before:
            print("imag Studio Mode already ON — ok (production state)")
        else:
            _rpc(ws, "SetStudioModeEnabled", {"studioModeEnabled": True})
            print(
                "imag Studio Mode was OFF — turned ON (production parity; the Preview cell in "
                "the multiview needs it — user hard rule 2026-07-15, #767)"
            )
    finally:
        ws.close()


def _first_enabled_scene_item_source(items):
    """#901 gap 3 (pure, testable — no I/O): given a GetSceneItemList `sceneItems` list, return
    the `sourceName` of the first ENABLED item, or None if the list is empty or nothing in it is
    enabled. A real GetSceneItemList response always carries `sceneItemEnabled`, but an item
    missing the key is treated as enabled (defensive default — never silently skipped).

    This is the "what is ACTUALLY rendered" resolution the fixed STRIH_PROG_SOURCE/
    STREAM_PROG_SOURCE burn-target constants in rig-mode.sh cannot provide: those name a scene's
    EXPECTED source, this reads what a scene's CURRENT program item genuinely is (live evidence,
    2026-08-04: strih's program scene rendered 'NDI cam2' while the fixed default was
    'NDI cam1' — the burn landed on the wrong, non-rendered input)."""
    for item in items:
        if bool(item.get("sceneItemEnabled", True)):
            return item.get("sourceName")
    return None


def program_rendered_input(a):
    """#901 gap 3: print (stdout) the source/input name OBS is ACTUALLY rendering in the current
    (or `a.scene`, if given) program scene — resolved via GetSceneItemList, not assumed from a
    fixed constant. Callers (rig-mode.sh) use this to burn/verify the input that is genuinely
    visible right now, in ADDITION to (never instead of) the pinned default target."""
    ws = _conn(a.host, a.password)
    try:
        scene = a.scene or _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName", "")
        if not scene:
            raise SystemExit(f"[obs] {a.host}: could not resolve a program scene to inspect")
        items = _rpc(ws, "GetSceneItemList", {"sceneName": scene}).get("sceneItems", [])
        src = _first_enabled_scene_item_source(items)
        if src is None:
            raise SystemExit(
                f"[obs] {a.host}: program scene '{scene}' has NO enabled scene item — cannot "
                f"resolve what is actually rendered."
            )
    finally:
        ws.close()
    print(src)


def assert_program_nonblack(a):
    """#901 gap 1: optical proof the current (or `a.scene`, if given) program scene is genuinely
    rendering non-black content — a READ-ONLY verification call, never a control op (no
    SetCurrentProgramScene, unlike switch()). Reuses the EXISTING `_assert_program_nonblack`
    helper — the same polled luma-peak self-check switch()/prod_scene() already use — so a caller
    that just wants proof "something real is on program right now" gets the identical, already-
    calibrated logic rather than a second, divergent black-check.

    Live evidence this closes (2026-08-04 supervisor comment on issue 901): a painter process can
    be alive, its pidfile correct, its marker CSV growing, and ALSA RUNNING, while the actual
    rendered program is BLACK for the whole run — process-alive is not QR-on-screen."""
    ws = _conn(a.host, a.password)
    try:
        scene = a.scene or _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName", "")
        if not scene:
            raise SystemExit(f"[obs] {a.host}: could not resolve a program scene to check")
        _assert_program_nonblack(
            ws, a.host, scene, a.label or "#901 chain-verify",
            "The camera/source feeding it is not delivering real frames — process-alive is not "
            "proof of QR-on-screen (issue 901).",
            min_mean=a.min_mean,
        )
    finally:
        ws.close()
    print(f"PASS: {a.host} program scene '{scene}' NON-BLACK")


DANTE_MBC_DEVICE_ID = "Dante Virtual Soundcard (x64)"  # issue 901 item 2: the expected device


def _mbc_transport_problem(device_id, muted, expected_device_id):
    """#901 original item 2 (pure, testable — no I/O): return a short problem string naming
    what's wrong with the mbc Dante transport, or None if it checks out. "A muted/rerouted/
    renamed input is exactly as fatal as a dead card and is a one-call read" (issue 901) — this
    is that one-call read's pure decision, checked over the OBS-WS input settings/mute state
    ALONE (no probe recording, no event subscription). Mute is checked FIRST and reported alone
    even when the device is ALSO wrong — one clear message, not a pile.

    Does NOT (cannot, from OBS-WS input state alone) confirm the Windows `dvs_service` is
    running — that needs a live Windows service-name check this code-only pass could not verify
    against real hardware; see the issue 901 design comment for the filed follow-up."""
    if muted:
        return "input is MUTED (mute must be OFF for the measurement chain to be usable)"
    if not device_id:
        return "input has NO device_id bound (device_id is empty/unset)"
    if device_id != expected_device_id:
        return f"input device_id is {device_id!r}, expected {expected_device_id!r} (rerouted/renamed?)"
    return None


def mbc_input_check(a):
    """#901 original item 2: hard-fail loud, BEFORE any measurement-audio probe, when the mbc
    Dante transport is unambiguously wrong (muted, or bound to something other than the Dante
    Virtual Soundcard device) — one cheap OBS-WS read, no probe recording needed."""
    ws = _conn(a.host, a.password)
    try:
        settings = _rpc(ws, "GetInputSettings", {"inputName": a.input}).get("inputSettings", {})
        device_id = settings.get("device_id", "")
        muted = bool(_rpc(ws, "GetInputMute", {"inputName": a.input}).get("inputMuted"))
    finally:
        ws.close()
    problem = _mbc_transport_problem(device_id, muted, a.expected_device_id)
    if problem:
        raise SystemExit(
            f"[obs] {a.host}: mbc Dante-transport check FAIL on '{a.input}' — {problem}. Check "
            f"the mbc Ableton mic channel + Dante routing into stream OBS (targets.md mbc row "
            f"has the checklist). This must be fixed before the measurement audio can ever "
            f"arrive (issue 901)."
        )
    print(f"PASS: {a.host} '{a.input}' Dante transport OK (device_id={device_id!r}, muted=False)")


def program_scene(a):
    """#281 Fix#3: print the current program scene name to stdout (one line).

    The rig-restore watchdog (scripts/rig-restore-watchdog.sh) reads the live OBS program scene to
    detect a stranded TEST state (program left on PHASE2-PROBE). Reusing _conn/_rpc here means the
    watchdog never re-implements the obs-websocket handshake/auth.
    """
    ws = _conn(a.host, a.password)
    try:
        scene = _rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName", "")
    finally:
        ws.close()
    print(scene)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in (
        "setup", "teardown", "record", "prod-scene", "switch", "program-scene",
        "stream-status", "latency-check", "open-projectors", "ensure-studio-mode-on",
        "program-rendered-input", "assert-program-nonblack", "mbc-input-check",
    ):
        p = sub.add_parser(name)
        p.add_argument("--host", required=True)
        p.add_argument("--password", default="")
        if name == "program-rendered-input":
            # #901 gap 3: which scene to inspect — omitted -> the CURRENT program scene.
            p.add_argument("--scene", default="")
        if name == "assert-program-nonblack":
            # #901 gap 1: which scene to check — omitted -> the CURRENT program scene (read-only,
            # never switches). --label tags the log lines; --min-mean overrides the shared
            # helper's env-resolved default floor (see _assert_program_nonblack's own docstring).
            p.add_argument("--scene", default="")
            p.add_argument("--label", default="")
            p.add_argument("--min-mean", type=float, default=None)
        if name == "mbc-input-check":
            # #901 original item 2: which OBS input carries the Dante measurement mic, and the
            # device_id it must be bound to.
            p.add_argument("--input", default="mbc")
            p.add_argument("--expected-device-id", default=DANTE_MBC_DEVICE_ID)
        if name == "record":
            p.add_argument(
                "--action", required=True,
                choices=("start", "stop", "status", "guard"),
            )
        if name == "latency-check":
            # #722 EVENT-mode CONTRACT item 6: the per-source input to check/restore (typically
            # the stream box's 'NDI 2ME PGM' program-genlock input) + the calibrated value from
            # av-sync-last.json (gathered by the caller, this process has no ssh/scp path of its
            # own to read that file directly -- same constraint drift-guard.sh and
            # _restore_test_latency already document for the SAME file).
            p.add_argument("--source", required=True)
            p.add_argument("--calibrated-ms", type=int, required=True)
        if name == "prod-scene":
            # #163: route program to a CERTIFIED PROD scene and record IT (no colliding
            # probe ndi_source). --program-scene is the existing prod scene to record;
            # --ensure-source (optional) builds a full-screen scene over that EXISTING
            # prod input when --program-scene is a dedicated temp scene (the stream box).
            p.add_argument("--program-scene", required=True)
            p.add_argument("--ensure-source", default="")
            # #183: the upstream NDI source-name of the certified prod GENLOCK input this
            # scene records (e.g. "CAM1 (usb)" on strih, the strih NDI name on stream). When
            # given with --test-preload, the harness FORCES that prod input's genlock_preload
            # to the test value for the recording window (saved to STATE, restored on teardown)
            # so the test measures the TRUE genlock latency (~33ms at preload=1) instead of the
            # prod audio-sync delay (preload=31 ≈ 1s). Omitted ⇒ prod preload is left untouched.
            p.add_argument("--upstream", default="")
            # #183: the genlock_preload to FORCE on the recorded prod input for the test
            # (default 1 = the true lowest-latency genlock hop). Only applied when --upstream
            # is also given. The prod value is saved and restored on teardown.
            p.add_argument("--test-preload", type=int, default=1)
            # #358: the OBS ndi_source input on the stream box whose per-source genlock
            # latency is SET to --test-latency-ms for the delivery-verify gate. Typically
            # 'NDI 2ME PGM' (prod stream box A/V-align at 450ms). Omitted ⇒ no latency set.
            # env: GENLOCK_TEST_LATENCY_SOURCE (set by recording-e2e.sh for the stream hop).
            p.add_argument(
                "--test-latency-source",
                default=os.environ.get("GENLOCK_TEST_LATENCY_SOURCE", ""),
            )
            # #358/#691: per-source genlock latency to SET for the delivery-verify test
            # window. Default is `None` (unset) unless GENLOCK_TEST_LATENCY_MS is
            # EXPLICITLY set — `resolve_test_latency_ms` then auto-derives the effective
            # value from the box's OWN current latency at call time (current value if
            # already >= 500ms, else the original 1000ms fallback) instead of a blind
            # forced 1000ms every run. env: GENLOCK_TEST_LATENCY_MS (set by
            # recording-e2e.sh only when an explicit override is requested).
            p.add_argument(
                "--test-latency-ms",
                type=int,
                default=_int_env_or_none("GENLOCK_TEST_LATENCY_MS"),
            )
        if name == "teardown":
            # #691 belt-and-braces (OPTIONAL): the known-good calibrated prod value from
            # av-sync-last.json on the OBS box's own ProgramData, gathered by the
            # operator/agent (this process has no ssh/scp path to read it directly — same
            # constraint drift-guard.sh's av_sync_calibrated_ms already documents for the
            # SAME file). When supplied, _restore_test_latency cross-checks the restored
            # value against it and warns loudly on mismatch. Absent by default — never a
            # hard requirement, an unattended CI run simply skips the check.
            p.add_argument(
                "--calibrated-latency-ms",
                type=int,
                default=_int_env_or_none("AV_SYNC_CALIBRATED_MS"),
            )
        if name == "setup":
            p.add_argument("--upstream", required=True)
            # #91: mark a TERMINAL box — one whose Main Output feeds NO downstream OBS
            # hop (it is tapped directly by dev1). For such a box the own-output
            # self-resolution abort is spurious (the box's own OBS can't self-discover
            # its own output via NDI loopback suppression) and there is no next hop to
            # protect, so setup() skips that abort and emits the bare name (dev1's tap
            # resolves the full NDI name itself). NON-terminal boxes (strih) keep the
            # protective abort. Defaults False — strih and teardown are non-terminal.
            p.add_argument("--terminal", action="store_true")
        if name == "switch":
            # #312 Phase-2 all-cambox sweep: cut PROGRAM to this scene + print the switch
            # epoch-ns boundary. Lightweight — no preload/upstream dance (prod_scene already
            # routed the scenes); just SetCurrentProgramScene + the non-black self-check.
            p.add_argument("--program-scene", required=True)
    # #406/#312 item5: `rig-busy-check` queries TWO hosts (strih + stream), not the single --host
    # every other subcommand takes above — its own parser, added separately.
    rbc = sub.add_parser("rig-busy-check")
    rbc.add_argument("--strih-host", default=os.environ.get("STRIH_HOST", "10.77.9.202"))
    rbc.add_argument("--stream-host", default=os.environ.get("STREAM_HOST", "10.77.9.204"))
    rbc.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    a = ap.parse_args()
    {"setup": setup, "teardown": teardown, "record": record,
     "prod-scene": prod_scene, "switch": switch,
     "program-scene": program_scene, "rig-busy-check": rig_busy_check,
     "stream-status": stream_status, "latency-check": latency_check,
     "open-projectors": open_projectors,
     "ensure-studio-mode-on": ensure_studio_mode_on,
     "program-rendered-input": program_rendered_input,
     "assert-program-nonblack": assert_program_nonblack,
     "mbc-input-check": mbc_input_check}[a.cmd](a)


if __name__ == "__main__":
    main()
