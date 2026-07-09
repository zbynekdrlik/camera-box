//! Regression guards for scripts/recording-e2e.sh path/host correctness (#7/#105/#156/#179).
//!
//! #179 made the harness TRUE STREAM-ONLY: the 7.3GB cam1 grab is GONE (it was the repeated
//! ~15-40 min decode sink that stalled every proof run and OOM-crashed the full 4-node run,
//! #187). The cam1-capture burn (#174) already rides cam1's id + CAPTURE wall-clock ts into
//! the stream recording, so the grab is redundant. The two grab-ts path tests this file used
//! to carry (guarding the cam1-local vs dev1 sidecar path) are therefore replaced below by
//! the inverse guard: the grab record/download is ABSENT and the cam1 capture burn is still
//! ENABLED, so a future refactor cannot silently re-introduce the slow grab path.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source recording-fetch-windows.sh and call `urlencode_name <arg>`, returning its
/// stdout. The script's `main` is guarded by a sourced-vs-executed check, so sourcing it
/// runs only the function definitions.
fn urlencode_name(arg: &str) -> String {
    let script = format!(
        "{}/scripts/recording-fetch-windows.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; urlencode_name \"$2\"")
        .arg("bash") // $0
        .arg(&script) // $1 — the script to source
        .arg(arg) // $2 — the name to encode
        .output()
        .expect("run urlencode_name");
    assert!(
        out.status.success(),
        "urlencode_name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// #179: the harness is TRUE STREAM-ONLY — it must NOT record or stream the 7.3GB cam1
/// grab any more. No `--record-grab` / `--record-grab-ts`, no dev1 ffmpeg grab listener,
/// no grab-stream port. (The grab was the repeated ~15-40 min decode sink, #187.)
#[test]
fn recording_e2e_does_not_record_the_cam1_grab() {
    let s = read("scripts/recording-e2e.sh");
    // The cam1 launch must NOT pass --record-grab / --record-grab-ts (only comments may
    // mention them historically; assert on the actual flag invocations).
    assert!(
        !s.contains("--record-grab "),
        "#179: the cam1 launch must NOT pass --record-grab (the grab is dropped)."
    );
    assert!(
        !s.contains("--record-grab-ts "),
        "#179: the cam1 launch must NOT pass --record-grab-ts (the grab sidecar is dropped)."
    );
    // No dev1 ffmpeg grab listener is started any more.
    assert!(
        !s.contains("listen=1"),
        "#179: the dev1 ffmpeg grab listener must be gone (no tcp listen for the grab stream)."
    );
}

/// #179: the cam1 capture burn (#174) must stay ENABLED — that burn is what rides cam1's
/// id + CAPTURE wall-clock ts into the stream recording, which REPLACES the grab. cam1 must
/// still launch the probe-featured camera-box with CAMERA_BOX_BURN_RUN_ID set.
#[test]
fn recording_e2e_keeps_the_cam1_capture_burn_enabled() {
    let s = read("scripts/recording-e2e.sh");
    // #24 item 1: the deploy step now sets CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID -- resolved
    // per the SOURCE camera ($CAMERA_NAME), defaulting to $BURN_CAM1_RUN_ID's value for cam1 (the
    // unset default) -- rather than the fixed $BURN_CAM1_RUN_ID, so cam3/cam4 can also be the
    // deployed camera. The burn is still unconditionally enabled; only WHICH reserved id is used
    // is now resolved. See recording_e2e_deploy_uses_the_burn_id_matching_the_resolved_camera.
    assert!(
        s.contains("CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID"),
        "#179/#174/#24: the SOURCE camera must launch with CAMERA_BOX_BURN_RUN_ID set (resolved \
         via $SRC_BURN_RUN_ID) so the capture burn rides into the stream recording (the mark \
         that lets the grab be dropped)."
    );
    assert!(
        s.contains("--burn-cam1-run-id"),
        "#179/#174: the verdict must be given --burn-cam1-run-id to pair the cam1 burn."
    );
}

/// #194 / cam2→cam1 LOSS: cam1 must emit its V4L2 capture-drop sidecar
/// (CAMERA_BOX_CAPTURE_STATS) and the verdict must receive it as --cam1-capture-stats, so the
/// cam2→cam1 loss is the camera-leg capture-drop count (NOT a painter-tick optical compare).
/// cam1 must be stopped GRACEFULLY (SIGINT) so the shutdown handler writes the sidecar.
#[test]
fn recording_e2e_wires_the_cam2_cam1_capture_drop_loss() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("CAMERA_BOX_CAPTURE_STATS=/tmp/cam1-capture-stats.txt"),
        "cam1 must launch with CAMERA_BOX_CAPTURE_STATS so it writes the V4L2 capture-drop \
         sidecar (the cam2→cam1 loss signal)."
    );
    assert!(
        s.contains("pkill -INT"),
        "cam1 must be stopped with SIGINT (graceful) so the shutdown handler writes the \
         capture-stats sidecar before exit."
    );
    assert!(
        s.contains("VERDICT_ARGS+=(--cam1-capture-stats"),
        "the verdict must be passed --cam1-capture-stats so cam2→cam1 loss = V4L2 capture-drop."
    );
}

/// #179: the verdict must be invoked TRUE STREAM-ONLY — never with --cam1 / --cam1-grab-ts
/// (which would decode the 7.3GB grab). The full chain + cam2→cam1 come from the stream
/// recording's burns alone.
#[test]
fn recording_e2e_verdict_is_stream_only_no_cam1_grab_args() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains("--cam1 "),
        "#179: the verdict must NOT be passed --cam1 (no 7.3GB grab decode)."
    );
    assert!(
        !s.contains("--cam1-grab-ts "),
        "#179: the verdict must NOT be passed --cam1-grab-ts (no grab sidecar)."
    );
    // It MUST still pass --stream (the headline recording the whole analysis reads).
    assert!(
        s.contains("VERDICT_ARGS+=(--stream"),
        "#179: the verdict must be passed --stream (the stream-only analysis input)."
    );
}

/// #178: the StopRecord→verdict region must be RESILIENT — a single transient failure
/// (a non-zero OBS StopRecord, an ssh hiccup, a missing recording so a `[ -f ]` guard is
/// false) must NOT `set -e`-abort the script before the verdict runs. The verdict IS the
/// whole point of the run; run 172046073 aborted straight to the cleanup trap after
/// StopRecord and never produced the verdict.
///
/// The fix brackets the region with `set +e` and re-enables `set -e` only at the verdict
/// run (which already manages its own exit via verdict-monitor.sh → GATE). This test guards
/// the resilience constructs are present so a refactor cannot silently re-introduce the abort.
#[test]
fn stoprecord_to_verdict_region_is_set_e_resilient() {
    let s = read("scripts/recording-e2e.sh");
    // The region opens with `set +e` (disable abort-on-error for the orchestration) — there
    // must be a `set +e` somewhere AFTER the "[7/8]" StopRecord banner and BEFORE the verdict
    // run section. (The cleanup trap already uses `set +e`; we need one for the main region.)
    let stop_banner = s
        .find("[7/8]")
        .expect("#178: the [7/8] StopRecord banner is missing");
    let verdict_run = s
        .find("LIVENESS-GUARDED verdict run")
        .expect("#178: the verdict-run section marker is missing");
    assert!(
        stop_banner < verdict_run,
        "#178: StopRecord must precede the verdict run"
    );
    let region = &s[stop_banner..verdict_run];
    assert!(
        region.contains("set +e"),
        "#178: the StopRecord→verdict region must `set +e` so a transient StopRecord/ssh/fetch \
         failure can't abort before the verdict runs (run 172046073 aborted to cleanup)."
    );
    // The verdict-arg guards must NOT use the `[ -f X ] && VERDICT_ARGS+=(...)` form: under
    // `set -e` a FALSE `[ -f ]` makes the `&&` list return non-zero and aborts the script.
    // They must be `if [ -f X ]; then VERDICT_ARGS+=(...); fi` (an `if` condition is exempt
    // from `set -e`), so a missing optional recording degrades gracefully, never aborts.
    assert!(
        !region.contains("] && VERDICT_ARGS+=("),
        "#178: `[ -f ... ] && VERDICT_ARGS+=(...)` is a set -e abort trap when the file is \
         absent — use `if [ -f ... ]; then VERDICT_ARGS+=(...); fi` instead."
    );
    // The StopRecord captures must be guarded so a non-zero stop can't abort the `$(...)`
    // capture under set -e (the prime #178 suspect). Either the region is under `set +e`
    // (covered above) AND/OR the captures carry a `|| true`/`|| echo` fallback.
    assert!(
        region.contains("StopRecord") || region.contains("record --action stop"),
        "#178: the StopRecord step must be in the resilient region"
    );
}

/// #178 (behavioral): a region that mirrors the FIXED StopRecord→verdict structure must
/// REACH the verdict step even when StopRecord returns non-zero and an optional recording is
/// absent — under `set -euo pipefail`. This is the contract the script fix must satisfy: the
/// `set +e` bracket + `if`-form arg guards + guarded captures never abort before the verdict.
#[test]
fn stoprecord_to_verdict_reaches_verdict_despite_a_failing_step() {
    // The resilient region pattern, in isolation: a StopRecord that FAILS (rc 1), an optional
    // recording that is ABSENT (so its `if [ -f ]` guard is false), all under set -euo
    // pipefail. The fixed pattern must still echo REACHED_VERDICT at the end.
    let script = r#"
set -euo pipefail
REACHED=no
# --- resilient StopRecord→verdict region (mirrors the script fix) ---
set +e
STRIH_HOST_PATH=$(false)   # StopRecord returns non-zero — must NOT abort
echo "stop rc was $? (non-fatal in the region)" >/dev/null
VERDICT_ARGS=(--strih /tmp/strih.mkv)
if [ -f /nonexistent-stream-recording.mkv ]; then
  VERDICT_ARGS+=(--stream /nonexistent-stream-recording.mkv)
fi
set -e
# --- verdict step (always reached) ---
REACHED=yes
echo "REACHED_VERDICT args=${#VERDICT_ARGS[@]}"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("run resilient-region pattern");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "#178: the resilient region must exit 0 despite a failing StopRecord + absent optional \
         recording; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("REACHED_VERDICT"),
        "#178: the verdict step must be reached despite the failing step; stdout={stdout}"
    );
}

/// The gate must ALWAYS receive a --win-status for strih AND stream (NOT conditional on the
/// status file existing). If the fetch failed and the file is absent, the gate must mark that
/// node UNKNOWN and FAIL — never silently drop it and certify only cam1+cam2 (the review bug).
#[test]
fn gate_always_passes_win_status_for_both_windows_nodes() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("--win-status \"strih=$DANTE_STRIH_STATUS\""),
        "the gate must ALWAYS be given strih=--win-status (so a missing file -> UNKNOWN -> fail)"
    );
    assert!(
        s.contains("--win-status \"stream=$DANTE_STREAM_STATUS\""),
        "the gate must ALWAYS be given stream=--win-status (so a missing file -> UNKNOWN -> fail)"
    );
    // The previous bug gated the --win-status args on `[ -s "$DANTE_..._STATUS" ]` (file exists),
    // which dropped a node whose fetch failed. That conditional must be gone.
    assert!(
        !s.contains("[ -s \"$DANTE_STRIH_STATUS\" ]  && GATE_WIN_ARGS")
            && !s.contains("GATE_WIN_ARGS+=(--win-status \"strih="),
        "the --win-status args must NOT be conditional on the status file existing"
    );
}

/// #220: the harness must PRINT a camera pre-run checklist (shutter/focus/exposure) before the
/// run starts — the cam1 optical settings it CANNOT auto-set (camera-box reads the ShadowCast
/// /dev/video0, which does not expose the BMPCC's shutter/focus/exposure). A 1/60 shutter caused
/// the #216 ~175s optical-read gap; the banner reminds the operator to set a fast shutter (≥1/500),
/// manual focus, and fixed exposure BEFORE the run.
#[test]
fn recording_e2e_prints_camera_pre_run_checklist() {
    let s = read("scripts/recording-e2e.sh");
    let banner = s
        .find("CAMERA PRE-RUN CHECKLIST")
        .expect("#220: recording-e2e.sh must print a CAMERA PRE-RUN CHECKLIST banner");
    // The checklist must name all three operator-set camera controls.
    assert!(
        s.contains("SHUTTER") && (s.contains("1/500") || s.contains("1/1000")),
        "#220: the checklist must call out a FAST shutter ≥1/500 (the #216 conclusion; freezes the \
         60Hz monitor QR so the dual-QR Vernier does not smear)."
    );
    assert!(
        s.contains("FOCUS") && s.contains("MANUAL"),
        "#220: the checklist must call out MANUAL focus locked on the cam2 monitor (no autofocus hunt)."
    );
    assert!(
        s.contains("EXPOSURE") && (s.contains("FIXED") || s.contains("manual gain")),
        "#220: the checklist must call out FIXED exposure / manual gain (no auto-exposure drift)."
    );
    // It is a PRE-run reminder: it must be printed BEFORE recording starts.
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        banner < start_record,
        "#220: the camera pre-run checklist must be printed BEFORE StartRecord (it is a pre-run \
         reminder — fix the camera, then run)."
    );
}

/// #195: the harness must SET+VERIFY burns are ON before recording — assert a pre-record
/// burn-ON gate that runs `obs_burn_filter.py check` on BOTH boxes and ABORTS the run on
/// pass-through (OBS not launched with OBS_BURN_QR → kind_registered=false → strih/stream burns
/// silently absent → the whole proof run wasted, the exact missed-env #195 documents). The gate
/// must sit in the MAIN flow (after the cleanup trap) and BEFORE StartRecord — NOT only in
/// cleanup() (which clears burns AFTER the run, #246).
#[test]
fn recording_e2e_asserts_burns_on_before_recording() {
    let s = read("scripts/recording-e2e.sh");
    // The gate lives in the main flow: after `trap cleanup` (so cleanup()'s own burn check,
    // defined earlier, is excluded) and before the [5/8] StartRecord step.
    let trap = s
        .find("trap cleanup")
        .expect("recording-e2e.sh must install the cleanup trap");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        trap < start_record,
        "the cleanup trap must be installed before StartRecord"
    );
    let region = &s[trap..start_record];
    // A burn-ON CHECK must run in this pre-record region.
    assert!(
        region.contains("obs_burn_filter.py") && region.contains("check"),
        "#195: a pre-record burn-ON gate must run `obs_burn_filter.py check` BEFORE StartRecord \
         (so a burns-OFF/pass-through OBS is caught before a full run is wasted), not only in \
         cleanup() after the run."
    );
    // #257: it must key on `burn_on` (genlock_burn=true AND the renderer filter present) — the
    // authoritative tell that the recording will actually carry the burn (no OBS_BURN_QR env now).
    assert!(
        region.contains("burn_on=True"),
        "#195/#257: the pre-record gate must inspect `burn_on=True` (genlock_burn on) — the tell \
         that distinguishes a live burn from a pass-through that records NO burns."
    );
    // It must FAIL FAST (abort) on pass-through, not just warn.
    assert!(
        region.contains("exit 1"),
        "#195: the pre-record gate must ABORT (exit 1) on burns-OFF — a warn-and-continue would \
         still waste the whole run (the failure mode #195 exists to kill)."
    );
    // It must cover BOTH boxes.
    assert!(
        region.contains("$STRIH") && region.contains("$STREAM"),
        "#195: the pre-record burn-ON gate must check BOTH strih and stream."
    );
}

/// #195/#257 (behavioral): the pre-record burn-ON gate's parse+abort logic must PROCEED only when
/// `burn_on=True` (genlock_burn on AND the renderer filter present), and ABORT otherwise — across
/// burns-on, burns-off (genlock_burn=false), filter-absent (burn_on=False), and OBS-unreachable
/// (the check command's error text). This locks the actual abort BEHAVIOR (the #195 regression:
/// burns-off must STOP the run, not waste it), not just the gate's presence. recording-e2e.sh runs
/// top-to-bottom (no source guard), so this exercises the gate's grep in an isolated bash snippet.
#[test]
fn recording_e2e_burn_gate_proceeds_only_when_burns_on() {
    // The gate's decision in isolation — the SAME grep recording-e2e.sh runs per box (#257).
    let gate = r#"
set -euo pipefail
burn_gate_ok() {
  _chk="$1"
  printf '%s' "$_chk" | grep -q 'burn_on=True' || return 1
  return 0
}
if burn_gate_ok "$1"; then echo PROCEED; else echo ABORT; fi
"#;
    // (check output text, expect PROCEED?)
    let cases = [
        (
            "[burn] burn_on=True genlock_burn=True filter_on_input=True kind_registered=True input='NDI cam5'",
            true,
        ),
        (
            "[burn] burn_on=False genlock_burn=False filter_on_input=True kind_registered=True input='NDI cam5'",
            false,
        ),
        (
            "[burn] burn_on=False genlock_burn=True filter_on_input=False kind_registered=True input='NDI cam5'",
            false,
        ),
        (
            "Traceback (most recent call last): ConnectionRefusedError",
            false,
        ),
    ];
    for (chk, expect_ok) in cases {
        let out = Command::new("bash")
            .arg("-c")
            .arg(gate)
            .arg("bash") // $0
            .arg(chk) // $1 — the check output, passed as an arg (no quoting hazard)
            .output()
            .expect("run burn-gate snippet");
        let stdout = String::from_utf8_lossy(&out.stdout);
        if expect_ok {
            assert!(
                stdout.contains("PROCEED") && !stdout.contains("ABORT"),
                "#195: burns-ON (kind_registered+filter_on_input) must PROCEED; got {stdout:?} for {chk:?}"
            );
        } else {
            assert!(
                stdout.contains("ABORT"),
                "#195: burns-off/unattached/unreachable must ABORT the run; got {stdout:?} for {chk:?}"
            );
        }
    }
}

/// The DanteSync NTP+PTP gate must be the FIRST hard step (#7): it must appear in the
/// script BEFORE the cam1/cam2 launch and the OBS recording start, so a not-locked cluster
/// fails fast before any measurement.
#[test]
fn dantesync_gate_runs_before_any_recording() {
    let s = read("scripts/recording-e2e.sh");
    let gate = s
        .find("dantesync-gate.sh")
        .expect("recording-e2e.sh must invoke dantesync-gate.sh");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        gate < start_record,
        "the DanteSync NTP+PTP gate must run BEFORE StartRecord (fail fast on an unlocked cluster)"
    );
}

/// #326: the all-cambox sweep stamps each program-switch window boundary on dev1's CLOCK_REALTIME,
/// while the verdict partitions by the painter (cam2) burn gen_ts_ns. recording-e2e.sh MUST assert
/// the dev1<->painter clock offset is within the switch-guard before the sweep — and only for the
/// ALL_CAMBOX path (the default single-hold run doesn't stamp windows on dev1's clock). The gate
/// must run BEFORE the all-cambox sweep step and BEFORE StartRecord (fail fast, no long sweep with
/// untrustworthy attribution).
#[test]
fn painter_clock_offset_gate_runs_before_the_all_cambox_sweep() {
    let s = read("scripts/recording-e2e.sh");
    let gate = s
        .find("clock-offset-painter-gate.sh")
        .expect("recording-e2e.sh must invoke clock-offset-painter-gate.sh (#326)");
    // It must be gated to the ALL_CAMBOX path: the line invoking the gate is inside an
    // `if [ "${ALL_CAMBOX:-0}" = "1" ]` block. Assert the nearest preceding ALL_CAMBOX guard.
    let head = &s[..gate];
    assert!(
        head.contains(r#"if [ "${ALL_CAMBOX:-0}" = "1" ]"#),
        "the painter clock-offset gate must be conditioned on ALL_CAMBOX (only the sweep stamps \
         windows on dev1's clock)"
    );
    let sweep = s
        .find("ALL-CAMBOX sweep:")
        .expect("recording-e2e.sh must have the all-cambox sweep step");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        gate < start_record && gate < sweep,
        "the dev1<->painter clock-offset gate must run BEFORE StartRecord and the sweep \
         (fail fast on a misaligned clock, never mid-sweep)"
    );
}

// ---------------------------------------------------------------------------
// #163 — record the PRODUCTION scene program, never a colliding probe input.
//
// Root cause (live-confirmed strih 2026-06-22): the harness routed program to the
// PHASE2-PROBE scene whose `phase2-probe-src` ndi_source was pointed at "CAM1 (usb)" —
// the SAME source-name the ALWAYS-ON prod input `NDI cam5` already holds. DistroAV
// allows ONE receiver per (source-name) on a host, so the probe input received NO NDI
// and the probe scene recorded pure BLACK (luma min=max=0; every frame undecodable),
// blocking the strict cam1→strih + strih→stream measurement.
//
// Fix: record the EXISTING certified prod scene program directly — strih's prod scene
// already shows cam1 via `NDI cam5` (genlock-certified), and stream's full-screen scene
// already shows strih via `NDI 2ME PGM`. No second receiver, no source-name collision.
// recording-e2e.sh must therefore route program via the `prod-scene` action (not the
// colliding probe `setup`).
// ---------------------------------------------------------------------------

/// recording-e2e.sh must route the strih + stream OBS PROGRAM via `obs_phase2.py
/// prod-scene` (record the certified production scene program), NOT via the probe
/// `setup` action that points the colliding `phase2-probe-src` ndi_source at a
/// source-name the always-on prod input already holds (→ black recording, #163).
#[test]
fn recording_e2e_records_prod_scene_not_colliding_probe() {
    let s = read("scripts/recording-e2e.sh");
    // The new prod-scene program routing must be used for BOTH boxes.
    assert!(
        s.contains("obs_phase2.py\" prod-scene") || s.contains("obs_phase2.py prod-scene"),
        "#163: recording-e2e.sh must route the OBS program via the `prod-scene` action \
         (record the certified prod scene program), not the colliding probe setup."
    );
    // The colliding probe `setup` action must NOT be used by the recording harness any
    // more — that is what created the second receiver on the prod source-name → black.
    assert!(
        !s.contains("obs_phase2.py\" setup") && !s.contains("obs_phase2.py setup"),
        "#163: recording-e2e.sh must NOT use `obs_phase2.py setup` (the probe-input path \
         that collides with the always-on prod NDI input and records black). Record the \
         prod scene program via `prod-scene` instead."
    );
}

/// The harness must name the certified PROD scenes it records: strih's prod scene that
/// already shows cam1 via the certified genlock input (`Cam 5`), and stream's
/// full-screen scene that shows strih's feed (`NDI 2ME PGM`). These are the scenes
/// proven (live + the prior 3-node run) to record NON-black; the probe scene records
/// black.
#[test]
fn recording_e2e_names_the_certified_prod_scenes() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("STRIH_PROG_SCENE") && s.contains("Cam 5"),
        "#163: the strih program scene must be the certified prod scene 'Cam 5' (already \
         shows cam1 via the genlock-certified `NDI cam5` input — no probe receiver)."
    );
    assert!(
        s.contains("STREAM_PROG_SCENE"),
        "#163: the stream program scene must be a named full-screen scene showing strih's \
         feed (`NDI 2ME PGM`), recorded directly — not the colliding probe scene."
    );
}

// ---------------------------------------------------------------------------
// #343 — the stream prod-scene step must record the ALREADY-ACTIVE prod scene, never
// cold-re-activate the heavy 450 ms-FIFO NDI 2ME PGM source.
//
// Root cause (live-confirmed stream 2026-06-29): the harness routed the stream PROGRAM to a
// fresh ephemeral scene `REC-STRIH-TMP` built via `--ensure-source "NDI 2ME PGM"`. Because that
// scene is NEVER already-program, prod_scene fell to the `SetCurrentProgramScene` switch, which
// COLD-ACTIVATES the NDI 2ME PGM source on OBS's graphics thread; that source carries a 450 ms
// genlock FIFO (#257) whose preload-to-reserve on activation blocks the graphics thread > 60 s,
// so the obs-websocket response never arrives — the #328 timeout fires and the #312 proof can't
// run. The merged #346 mitigation (skip-if-on-target) never triggered because program was never
// already on the ephemeral scene. Fix: record the ALREADY-ACTIVE prod scene `PRO` (NDI 2ME PGM
// already warm) and DROP `--ensure-source` for the stream box, so `curr_prog == target` → the
// switch is skipped entirely → no cold re-activation → no hang (and the teardown hang too).
// ---------------------------------------------------------------------------

/// #343: the stream prod-scene step must record the already-active prod scene `PRO` and must NOT
/// re-activate the heavy NDI 2ME PGM source — i.e. STREAM_PROG_SCENE defaults to `PRO` (not the
/// ephemeral `REC-STRIH-TMP`), there is no `REC-STRIH-TMP` reference left in the harness, and the
/// stream prod-scene invocation passes NO `--ensure-source` (which forces the hanging activation).
#[test]
fn recording_e2e_stream_records_already_active_prod_scene_no_reactivation() {
    let s = read("scripts/recording-e2e.sh");
    // (a) The stream program scene default is the already-active prod scene `PRO` — never the
    // ephemeral REC-STRIH-TMP (which is never already-program → forces a cold re-activation).
    assert!(
        s.contains("STREAM_PROG_SCENE=\"${STREAM_PROG_SCENE:-PRO}\""),
        "#343: STREAM_PROG_SCENE must default to the already-active prod scene 'PRO' (NDI 2ME PGM \
         already warm) so prod_scene's `curr_prog == target` branch fires and NO \
         SetCurrentProgramScene runs (the >60s graphics-thread hang path)."
    );
    assert!(
        !s.contains("REC-STRIH-TMP"),
        "#343: the harness must NOT route the stream program to the ephemeral REC-STRIH-TMP scene \
         — it is never already-program, so switching to it cold-re-activates the 450ms-FIFO NDI \
         2ME PGM on the graphics thread and SetCurrentProgramScene blocks >60s (#328 timeout)."
    );
    // (b) The stream prod-scene invocation must NOT pass --ensure-source (which builds the
    // ephemeral scene + forces the heavy source activation). Scope to the stream prod-scene block.
    let stream_call = s.find("prod-scene --host \"$STREAM\"").expect(
        "#343: recording-e2e.sh must route the stream program via `prod-scene --host \"$STREAM\"`",
    );
    // Scope to exactly the `$(...)` command (up to its closing paren) — not a fixed-width window
    // that could overrun into a later unrelated command's args.
    let block_end = stream_call
        + s[stream_call..]
            .find(')')
            .expect("#343: the stream prod-scene $(...) command must close with ')'");
    let block = &s[stream_call..block_end];
    assert!(
        !block.contains("--ensure-source"),
        "#343: the stream prod-scene call must NOT pass --ensure-source — forcing source \
         activation cold-loads the 450ms NDI 2ME PGM FIFO on the graphics thread and hangs \
         SetCurrentProgramScene. Record the already-active PRO scene (source warm). Got: {block:?}"
    );
}

/// Teardown must restore the prior PROGRAM scenes on BOTH boxes (it routed them to the
/// prod recording scene for the run). The prod-scene action records prev program/preview
/// to state; teardown restores it — never strands the recording scene as live program.
#[test]
fn recording_e2e_teardown_restores_program_scenes() {
    let s = read("scripts/recording-e2e.sh");
    // teardown still runs for both boxes in cleanup (restoring prev program scene).
    let cleanup = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[cleanup..]
        .find("\ntrap ")
        .map(|i| cleanup + i)
        .unwrap_or(s.len());
    let body = &s[cleanup..end];
    assert!(
        body.contains("teardown --host \"$STREAM\"") && body.contains("teardown --host \"$STRIH\""),
        "#163: cleanup() must teardown (restore prior program scene) on BOTH strih and \
         stream after recording the prod scene program."
    );
}

/// #246: cleanup() must CLEAR + VERIFY-OFF the OBS burn on BOTH boxes after every run (incl.
/// failure/abort), so a QR test-burn can never linger onto the live broadcast. The harness has no
/// SSH to the Windows boxes, so it cannot clear Machine-scope OBS_BURN_* env (that is the job of
/// drift-guard --compare burn_env= + rig-mode event); what it CAN and MUST do is remove the
/// obs-websocket-reachable burn filter (clear) and `check` it off (verify) on strih + stream.
#[test]
fn recording_e2e_cleanup_clears_and_verifies_burns_off() {
    let s = read("scripts/recording-e2e.sh");
    let cleanup = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[cleanup..]
        .find("\ntrap ")
        .map(|i| cleanup + i)
        .unwrap_or(s.len());
    let body = &s[cleanup..end];
    assert!(
        body.contains("obs_burn_filter.py"),
        "#246: cleanup() must clear/verify the OBS burn via obs_burn_filter.py (the \
         websocket-reachable burn surface), so a test burn can't linger after a run/abort."
    );
    assert!(
        body.contains("remove") && body.contains("check"),
        "#246: cleanup() must both REMOVE the burn (clear) AND `check` it (verify-off)."
    );
    assert!(
        body.contains("$STRIH") && body.contains("$STREAM"),
        "#246: the burn clear+verify must run on BOTH strih and stream."
    );
}

/// #246 (regression): cleanup()'s burn-clear loop references $STRIH_PROG_SOURCE / $STREAM_PROG_SOURCE.
/// The script runs `set -euo pipefail`, so those vars MUST be defined BEFORE `trap cleanup` — otherwise
/// any early abort (failed prebuilt-probe check, cargo build, cam scp/ssh, or Ctrl-C before the later
/// definition) fires the trap and the loop dies on a `set -u` unbound-variable, SKIPPING the burn
/// clear+verify in the exact failure/abort window the CRITICAL #246 guard must cover.
#[test]
fn recording_e2e_burn_source_vars_defined_before_cleanup_trap() {
    let s = read("scripts/recording-e2e.sh");
    let trap = s
        .find("trap cleanup")
        .expect("recording-e2e.sh must install the cleanup trap");
    for var in ["STRIH_PROG_SOURCE=", "STREAM_PROG_SOURCE="] {
        let def = s
            .find(var)
            .unwrap_or_else(|| panic!("recording-e2e.sh must define {var}"));
        assert!(
            def < trap,
            "#246: {var} must be defined BEFORE `trap cleanup` — else cleanup()'s burn-clear loop \
             hits a `set -u` unbound-variable on an early abort and the burn is NOT cleared in the \
             failure/abort window the guard must cover."
        );
    }
}

/// #252: the #195 pre-record burn-ON gate and the #246 cleanup() burn-clear loop MUST iterate one
/// shared `BURN_TARGETS` array, not two hand-synced inline triple-lists — otherwise a third box (or
/// a triple-structure change) can green-light a set the cleanup does not clear (the #246
/// linger-onto-live-broadcast hazard). The array MUST be defined BEFORE `trap cleanup` (so
/// cleanup()'s `"${BURN_TARGETS[@]}"` is never an unbound `set -u` var on an early abort) and MUST
/// cover BOTH strih and stream.
#[test]
fn recording_e2e_burn_targets_is_one_shared_array() {
    let s = read("scripts/recording-e2e.sh");

    // The array is declared exactly once, and covers both boxes.
    let def = s
        .find("BURN_TARGETS=(")
        .expect("#252: recording-e2e.sh must define a single BURN_TARGETS array");
    assert_eq!(
        s.matches("BURN_TARGETS=(").count(),
        1,
        "#252: BURN_TARGETS must be defined exactly once (single source of truth)."
    );
    let decl_end = def + s[def..].find(')').expect("BURN_TARGETS=( must close");
    let decl = &s[def..decl_end];
    // Match `$STRIH=` / `$STREAM=` (the triple separator), not a bare `$STRIH` — so the
    // `$STRIH_PROG_SOURCE` / `$STREAM_PROG_SOURCE` substrings can't satisfy "both boxes".
    assert!(
        decl.contains("$STRIH=") && decl.contains("$STREAM="),
        "#252: the shared BURN_TARGETS array must cover BOTH strih and stream \
         (each as a host=ip=source triple)."
    );

    // It is defined BEFORE the cleanup trap (set -u safety on an early abort).
    let trap = s
        .find("trap cleanup")
        .expect("recording-e2e.sh must install the cleanup trap");
    assert!(
        def < trap,
        "#252: BURN_TARGETS must be defined BEFORE `trap cleanup` — else cleanup()'s \
         `\"${{BURN_TARGETS[@]}}\"` is an unbound `set -u` var on an early abort."
    );

    // BOTH consumers iterate the shared array — the cleanup() burn-clear loop AND the #195
    // pre-record burn-ON gate. Two `for _hbs in "${BURN_TARGETS[@]}"` loop headers prove the
    // inline triple-lists are gone (the dedup #252 asks for); anchoring on the loop header (not
    // the bare expansion) excludes any prose mention of the array in comments.
    assert_eq!(
        s.matches("for _hbs in \"${BURN_TARGETS[@]}\"").count(),
        2,
        "#252: both the #195 pre-record gate and the #246 cleanup() loop must iterate \
         `for _hbs in \"${{BURN_TARGETS[@]}}\"` — neither may keep an inline triple-list."
    );
}

// ---------------------------------------------------------------------------
// #163: recording-fetch-windows.sh must URL-ENCODE the OBS recording filename.
//
// OBS's default recording name is `YYYY-MM-DD HH-MM-SS.ext` — it contains a SPACE.
// python http.server serves it only at the percent-encoded path (`.../...%20...`). The
// fetch built the URL from the RAW name, so curl sent a malformed request and the fetch
// FAILED — the strih/stream recordings (recorded fine, prod-scene program, NON-black)
// never reached the verdict, so the strict cam1→strih + strih→stream hops were
// unmeasured. RED on the raw-name URL; GREEN once the name is URL-encoded.
// ---------------------------------------------------------------------------

/// The fetch must URL-encode the recording filename's spaces (a raw space breaks curl).
#[test]
fn fetch_windows_url_encodes_spaces_in_the_recording_name() {
    // The exact OBS default name shape that broke the run.
    assert_eq!(
        urlencode_name("2026-06-22 20-58-26.mkv"),
        "2026-06-22%2020-58-26.mkv",
        "#163: recording-fetch-windows.sh must percent-encode spaces in the OBS recording \
         filename (python http.server serves it only at the %20-encoded path); a raw space \
         makes curl send a malformed request and the fetch fails."
    );
    // A name with no spaces is unchanged (no double-encoding, no corruption).
    assert_eq!(urlencode_name("clip.mp4"), "clip.mp4");
}

/// The URL must be built from the ENCODED name, never the raw `${name}` (the bug). Static
/// guard so a regression that drops the encode step fails even where bash isn't run.
#[test]
fn fetch_windows_builds_url_from_encoded_name_not_raw() {
    let s = read("scripts/recording-fetch-windows.sh");
    assert!(
        s.contains("urlencode_name"),
        "#163: recording-fetch-windows.sh must URL-encode the filename via urlencode_name."
    );
    // The url must use the encoded var, not the raw ${name}.
    assert!(
        s.contains("${WIN_HTTP_PORT}/${enc}"),
        "#163: the fetch URL must use the ENCODED name (${{enc}}), not the raw ${{name}} \
         (a raw space in the path makes curl fail)."
    );
    assert!(
        !s.contains("${WIN_HTTP_PORT}/${name}"),
        "#163 regression: the fetch URL must NOT be built from the raw ${{name}} (unencoded \
         spaces break the request)."
    );
}

// ---------------------------------------------------------------------------
// #166: the verdict step must be LIVENESS-GUARDED, never a bare blocking call.
//
// The #166 night: recording-verdict decoded a 7.3 GB cam1 grab single-threaded for
// >1 h, then DIED SILENTLY, and a monitor waited on a completion marker the crashed
// process never wrote — the whole run hung all night with no result. The fix runs the
// verdict in the background behind verdict-monitor.sh, which fails LOUDLY on a dead or
// stalled process. These static guards stop a future refactor from silently reverting
// to the synchronous-hang pattern.
// ---------------------------------------------------------------------------

/// recording-e2e.sh must drive the verdict through verdict-monitor.sh (the liveness guard).
#[test]
fn verdict_step_uses_the_liveness_monitor() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("verdict-monitor.sh"),
        "#166: the verdict step must run behind scripts/verdict-monitor.sh so a dead/stalled \
         verdict fails the run instead of hanging forever."
    );
    // The verdict must be backgrounded with an exit marker the monitor reads.
    assert!(
        s.contains("VERDICT_EXIT_MARKER") && s.contains("--exit-marker"),
        "#166: the verdict must write an exit marker that the monitor watches for completion."
    );
    assert!(
        s.contains("--stall-timeout"),
        "#166: the monitor must be given a stall timeout (no-progress → fail fast)."
    );
}

/// The liveness monitor script must exist and be executable.
#[test]
fn verdict_monitor_script_exists() {
    let path = format!("{}/scripts/verdict-monitor.sh", env!("CARGO_MANIFEST_DIR"));
    let meta = fs::metadata(&path).unwrap_or_else(|e| panic!("verdict-monitor.sh missing: {e}"));
    assert!(meta.is_file(), "verdict-monitor.sh must be a file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "verdict-monitor.sh must be executable"
        );
    }
}

// ---------------------------------------------------------------------------
// #193 — DECODE WHERE THE VIDEO IS: the verdict runs ON stream.lan, NOT on dev1.
//
// The OLD harness DOWNLOADED the 0.7-6 GB OBS recordings over the LAN to dev1 (a slow PC
// meant only to run Claude) and decoded + rqrr'd them there — the root of the slow transfers,
// the dev1 OOM (#187), the 14GB+ disk fill, and the repeated stalls. The fix runs the decode
// IN PLACE on the powerful stream box (10.77.9.204) where the recording already lives, and
// brings back ONLY the small verdict JSON + a few pixel-proof PNGs. These guards stop a
// refactor from silently re-introducing the multi-GB download-to-dev1 decode.
// ---------------------------------------------------------------------------

/// The harness must DEFAULT to running the verdict ON stream.lan (VERDICT_ON_STREAM=1), and
/// in that default mode must NOT download the recordings to dev1 (the fetch is gated to the
/// legacy VERDICT_ON_STREAM=0 path).
#[test]
fn recording_e2e_defaults_to_decode_on_stream_not_dev1() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("VERDICT_ON_STREAM=\"${VERDICT_ON_STREAM:-1}\""),
        "#193: the harness must DEFAULT VERDICT_ON_STREAM to 1 (decode ON stream.lan, not dev1)."
    );
    // The dev1 fetch (recording-fetch-windows.sh — the multi-GB download) must be GATED so it
    // does NOT run in the default on-stream mode. It must sit inside the VERDICT_ON_STREAM=0
    // (else) branch, not unconditionally.
    let fetch = s
        .find("recording-fetch-windows.sh\" \\")
        .or_else(|| s.find("recording-fetch-windows.sh"))
        .expect("#193: the legacy dev1 fetch must still exist for the VERDICT_ON_STREAM=0 path");
    // Walk backward from the fetch to the nearest VERDICT_ON_STREAM branch keyword; it must be
    // the `else` of the on-stream guard (i.e. the fetch is NOT in the on-stream default path).
    let before = &s[..fetch];
    let last_if = before
        .rfind("if [ \"$VERDICT_ON_STREAM\" = \"1\" ]")
        .unwrap_or(0);
    let last_else = before.rfind("else").unwrap_or(0);
    assert!(
        last_else > last_if && last_if > 0,
        "#193: the multi-GB recording-fetch-windows.sh download must be inside the \
         VERDICT_ON_STREAM=0 (else) branch — never run in the default decode-on-stream mode."
    );
}

/// In the on-stream path the harness must invoke recording-verdict-on-stream.sh (the planner
/// that emits the win-stream-snv upload→run-on-box→pull-back-JSON plan) — proving the verdict
/// is run on the box, not decoded on dev1.
#[test]
fn recording_e2e_runs_the_verdict_on_stream_via_the_planner() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("recording-verdict-on-stream.sh"),
        "#193: the on-stream path must invoke scripts/recording-verdict-on-stream.sh (run the \
         verdict ON the box, pull back only the small JSON+PNGs)."
    );
    // The on-stream branch must NOT decode a multi-GB recording on dev1: the dev1
    // verdict-monitor.sh / setsid recording-verdict run must be in the LEGACY (else) path only.
    let on_stream = s
        .find("if [ \"$VERDICT_ON_STREAM\" = \"1\" ]; then\n  set -e")
        .expect("#193: the on-stream guard must exist in the [8/8] section");
    // After the on-stream guard opens, it must reach `exit 0` BEFORE the dev1 setsid verdict run
    // (so the on-stream path never falls through to a dev1 decode).
    let region = &s[on_stream..];
    let exit0 = region
        .find("\n  exit 0\nfi")
        .expect("#193: on-stream branch must exit 0");
    let dev1_run = region
        .find("setsid bash -c")
        .expect("#193: the legacy dev1 verdict run must still exist");
    assert!(
        exit0 < dev1_run,
        "#193: the on-stream branch must exit BEFORE the dev1 setsid recording-verdict run, so \
         the default path never decodes a multi-GB recording on dev1."
    );
}

/// The on-stream planner script must exist and be executable.
#[test]
fn recording_verdict_on_stream_script_exists() {
    let path = format!(
        "{}/scripts/recording-verdict-on-stream.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let meta = fs::metadata(&path)
        .unwrap_or_else(|e| panic!("#193: recording-verdict-on-stream.sh missing: {e}"));
    assert!(
        meta.is_file(),
        "recording-verdict-on-stream.sh must be a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "recording-verdict-on-stream.sh must be executable"
        );
    }
}

/// The planner must build a PowerShell command that runs the verdict EXE on the box against
/// the recording's box-local Windows path — preserving single backslashes (a Windows path),
/// not bash-doubling them, and never referencing a dev1 path. Behavioral: source the script
/// and call build_onbox_command.
#[test]
fn on_stream_planner_builds_a_valid_windows_command() {
    let script = format!(
        "{}/scripts/recording-verdict-on-stream.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; build_onbox_command \"$2\" --stream \"$3\" --min-secs 300")
        .arg("bash") // $0
        .arg(&script) // $1 — the script to source
        .arg("C:\\camera-box\\recording-verdict.exe") // $2 — exe
        .arg("C:\\OBS\\stream-7.mp4") // $3 — the box-local recording
        .output()
        .expect("run build_onbox_command");
    assert!(
        out.status.success(),
        "build_onbox_command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cmd = String::from_utf8_lossy(&out.stdout);
    // Single backslashes preserved (a valid Windows path), NOT doubled.
    assert!(
        cmd.contains("C:\\OBS\\stream-7.mp4") && !cmd.contains("C:\\\\OBS"),
        "#193: the on-box command must keep single backslashes in the Windows recording path \
         (not bash-double them): {cmd:?}"
    );
    // It runs the verdict EXE on the box and decodes the box-local recording — no dev1 path.
    assert!(
        cmd.contains("recording-verdict.exe") && cmd.contains("--stream"),
        "#193: the on-box command must run recording-verdict.exe against the local --stream \
         recording: {cmd:?}"
    );
    assert!(
        !cmd.contains("/tmp/") && !cmd.contains("/home/"),
        "#193: the on-box command must NOT reference a dev1 (Linux) path — the decode is ON \
         the box: {cmd:?}"
    );
}

// ---------------------------------------------------------------------------
// #208 — PER-BOX decode-in-place (refines #193). The verdict needs the STRIH recording (cam1
// contiguity #133 + cam→strih) AND the STREAM recording (the full chain). The old on-stream
// flow ran a SINGLE fused verdict on the stream box, which forced the ~700 MB strih .mkv to be
// copied strih→stream first. #208: decode the strih recording ON the strih box and the stream
// recording ON the stream box, each in place, and merge the SMALL partial JSONs on dev1 — a
// recording is NEVER copied box-to-box (nor to dev1). These guards lock that flow.
// ---------------------------------------------------------------------------

/// The on-strih planner script must exist and be executable (mirror of on-stream).
#[test]
fn recording_verdict_on_strih_script_exists() {
    let path = format!(
        "{}/scripts/recording-verdict-on-strih.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let meta = fs::metadata(&path)
        .unwrap_or_else(|e| panic!("#208: recording-verdict-on-strih.sh missing: {e}"));
    assert!(
        meta.is_file(),
        "recording-verdict-on-strih.sh must be a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "recording-verdict-on-strih.sh must be executable"
        );
    }
}

/// The on-strih planner must target the STRIH box (win-strih MCP, 10.77.9.202) and run the
/// verdict against the box-LOCAL strih recording — it must NEVER copy that recording off-box.
#[test]
fn on_strih_planner_targets_the_strih_box() {
    let s = read("scripts/recording-verdict-on-strih.sh");
    assert!(
        s.contains("win-strih"),
        "#208: the on-strih planner must target the win-strih MCP (the strih box)."
    );
    assert!(
        s.contains("10.77.9.202"),
        "#208: the on-strih planner must reference the strih box IP 10.77.9.202."
    );
    // It must NOT reference the stream box — it only decodes the strih recording in place.
    assert!(
        !s.contains("win-stream") && !s.contains("10.77.9.204"),
        "#208: the on-strih planner must NOT touch the stream box (it decodes strih in place only)."
    );
    // It must state the strih recording stays on the box (never copied).
    assert!(
        s.to_lowercase().contains("never copied") || s.to_lowercase().contains("stays on"),
        "#208: the on-strih planner must state the strih recording stays on the box (never copied)."
    );
}

/// recording-e2e.sh [8/8] default path must EXTRACT the strih partial ON the strih box (via the
/// on-strih planner) and the stream partial ON the stream box (via the on-stream planner), then
/// MERGE the two small JSONs on dev1.
#[test]
fn recording_e2e_extracts_each_partial_on_its_own_box_and_merges() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("recording-verdict-on-strih.sh"),
        "#208: the default path must invoke recording-verdict-on-strih.sh (strih partial ON the \
         strih box)."
    );
    assert!(
        s.contains("recording-verdict-on-stream.sh"),
        "#208: the default path must invoke recording-verdict-on-stream.sh (stream partial ON the \
         stream box)."
    );
    assert!(
        s.contains("--extract-partial strih"),
        "#208: the strih box must run `recording-verdict --extract-partial strih` against its \
         LOCAL strih recording."
    );
    assert!(
        s.contains("--extract-partial stream"),
        "#208: the stream box must run `recording-verdict --extract-partial stream` against its \
         LOCAL stream recording."
    );
    assert!(
        s.contains("--merge-partials"),
        "#208: dev1 must MERGE the two small partial JSONs (`recording-verdict --merge-partials \
         strih=… stream=…`) into the final verdict — no recording on dev1."
    );
}

/// The CORE #208 guard: a recording is NEVER copied box-to-box. The strih recording is decoded on
/// the STRIH box (the on-strih extract legitimately takes `--strih "$STRIH_REC_WIN"`); the strih
/// recording must NOT be handed to the STREAM box's extract (the old fused on-stream flow forwarded
/// it there, which required the strih .mkv to be copied strih→stream first). So `--strih
/// "$STRIH_REC_WIN"` must appear in the on-STRIH block ONLY, and there must be no PowerShell
/// recording-copy mechanism between boxes. (The on-stream-never-strih guard is the separate test.)
#[test]
fn recording_e2e_never_copies_a_recording_box_to_box() {
    let s = read("scripts/recording-e2e.sh");
    // Anchor on the actual [8/8a]/[8/8b] planner INVOCATIONS — the line-continuation calls
    // `"$HERE/recording-verdict-on-*.sh" \` — NOT the first textual mention of the filename.
    // The #137 AV_RESTART_GATE mode legitimately SOURCES recording-verdict-on-stream.sh
    // earlier (to reuse build_onbox_command for Windows-path quoting), so a plain
    // `.find("recording-verdict-on-stream.sh")` would land on that source/comment, not the
    // invocation. The `.sh" \` suffix uniquely identifies the executed planner call.
    let strih_call = s
        .find("recording-verdict-on-strih.sh\" \\")
        .expect("#208: the on-strih planner invocation must exist");
    let stream_call = s
        .find("recording-verdict-on-stream.sh\" \\")
        .expect("#208: the on-stream planner invocation must exist");
    // The strih recording is decoded ON the strih box: `--strih "$STRIH_REC_WIN"` belongs to the
    // on-STRIH extract block (between the on-strih call and the on-stream call), proving it is
    // decoded in place there, not copied to the stream box.
    let strih_marker = s
        .find("--strih \"$STRIH_REC_WIN\"")
        .expect("#208: the strih box must decode its own recording (--strih \"$STRIH_REC_WIN\")");
    assert!(
        strih_call < strih_marker && strih_marker < stream_call,
        "#208: `--strih \"$STRIH_REC_WIN\"` must be in the on-STRIH extract block (decoded on the \
         strih box), NEVER handed to the on-STREAM extract (which would force a box-to-box copy)."
    );
    // No box-to-box recording copy mechanism (PowerShell Copy-Item / New-PSDrive of a .mkv/.mp4).
    let lower = s.to_lowercase();
    assert!(
        !lower.contains("copy-item") && !lower.contains("new-psdrive"),
        "#208: there must be no PowerShell Copy-Item / New-PSDrive recording copy between boxes."
    );
}

/// The stream-box partial extract must be passed ONLY --stream (its own recording), never a
/// strih recording path — proving the strih recording is decoded on the strih box, not the
/// stream box. Asserted on the on-stream planner invocation block in the default path.
#[test]
fn recording_e2e_stream_extract_is_stream_only_never_strih() {
    let s = read("scripts/recording-e2e.sh");
    // Find the on-stream planner INVOCATION in the default path and confirm its forwarded args
    // (after `--`) carry --extract-partial stream + --stream, and NO --strih recording. Anchor
    // on the invocation (`...on-stream.sh" \`), not the first filename mention: the #137
    // AV_RESTART_GATE mode sources recording-verdict-on-stream.sh earlier for build_onbox_command
    // reuse, so a bare `.find("recording-verdict-on-stream.sh")` would land on that source.
    let call = s
        .find("recording-verdict-on-stream.sh\" \\")
        .expect("#208: the on-stream planner invocation must exist");
    // Look at a generous window after the invocation (the forwarded args span several lines).
    let window = &s[call..(call + 800).min(s.len())];
    assert!(
        window.contains("--extract-partial stream"),
        "#208: the on-stream planner must forward `--extract-partial stream`: {window:?}"
    );
    // Forbid the strih RECORDING flag (`--strih "<path>"`) — that would force a box-to-box copy.
    // `--strih-emit-fps "<n>"` (#11, a scalar rate for the decimation step, not a recording) is
    // legitimately allowed, so match the recording flag's `--strih "` form specifically rather than
    // the bare `--strih` substring (which `--strih-emit-fps` would false-trip).
    assert!(
        !window.contains("--strih \""),
        "#208: the stream-box extract must NEVER be passed the --strih <recording> (the strih \
         recording is decoded on the strih box): {window:?}"
    );
}

// ---------------------------------------------------------------------------
// #186/#208 — the per-box extract must PRODUCE + PULL BACK the pixel proofs.
//
// BLOCKER (review): the per-box flow decoded each recording into a partial JSON but NEVER wrote the
// #186 pixel proofs, while merge mode CLAIMED they were "written on the recording's own box during
// --extract-partial" — pointing the operator at PNGs that never existed. The fix writes them into a
// `<partial>-pixels` dir on the box and pulls that dir back beside the partial on dev1, so a FAIL's
// #186 "SEE the missing frame" guarantee resolves to a real dev1 path. These guards lock the
// pull-back wiring (the Rust side — extract_partial writing + run_merge referencing them — is
// covered by recording-verdict.rs's probe-feature unit tests).
// ---------------------------------------------------------------------------

/// The on-strih planner must pull back the `<partial>-pixels` dir (the on-box #186 pixel proofs),
/// not only the partial JSON — derived from the forwarded `--out <partial>`.
#[test]
fn on_strih_planner_pulls_back_the_pixel_proof_dir() {
    let s = read("scripts/recording-verdict-on-strih.sh");
    assert!(
        s.contains("-pixels"),
        "#186/#208: the on-strih planner must derive + pull back the <partial>-pixels dir."
    );
    assert!(
        s.to_lowercase().contains("pixel proof") || s.to_lowercase().contains("pixel-proof"),
        "#186/#208: the on-strih planner must name the pixel-proof pull-back in its plan."
    );
}

/// The on-stream planner must pull back the `<partial>-pixels` dir in per-box extract mode.
#[test]
fn on_stream_planner_pulls_back_the_pixel_proof_dir() {
    let s = read("scripts/recording-verdict-on-stream.sh");
    assert!(
        s.contains("-pixels"),
        "#186/#208: the on-stream planner must derive + pull back the <partial>-pixels dir."
    );
    assert!(
        s.contains("${PIXELS_DIR}") || s.contains("$PIXELS_DIR"),
        "#186/#208: the on-stream planner must reference the derived PIXELS_DIR in its STEP 3."
    );
}

/// recording-e2e.sh per-box flow must pull EACH box's `<partial>-pixels` dir back to $OUTDIR beside
/// its partial JSON — so the dev1 merge (which derives `<partial>-pixels`) finds the #186 PNGs.
#[test]
fn recording_e2e_pulls_pixel_proof_dirs_to_outdir() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("$OUTDIR/strih-partial-${RUN_ID}-pixels")
            && s.contains("$OUTDIR/stream-partial-${RUN_ID}-pixels"),
        "#186/#208: the per-box flow must define each box's pixel-proof dir under $OUTDIR (beside \
         its partial) so the merge can locate the #186 PNGs on dev1."
    );
    assert!(
        s.contains("STRIH_PIXELS_WIN") && s.contains("STREAM_PIXELS_WIN"),
        "#186/#208: the per-box flow must define the box-local pixel-proof dirs to pull back."
    );
    // The plan must instruct pulling each pixel dir back (FileDownload of the *-pixels dir).
    assert!(
        s.contains("FileDownload $STRIH_PIXELS_WIN")
            && s.contains("FileDownload $STREAM_PIXELS_WIN"),
        "#186/#208: the plan must FileDownload each box's pixel-proof dir to dev1."
    );
}

// ---------------------------------------------------------------------------
// #123 — pre-rig-test VERSION-INTEGRITY gate wiring.
//
// Every rig-test entry script that brings up the strih+stream genlocked OBS stack must, BEFORE
// recording, run scripts/version-integrity-gate.sh against BOTH Windows boxes — so a drifted /
// randomly-deployed / stock OBS build (the #119 wrong-bytes-right-version) is caught and the run
// is REFUSED before any worthless measurement. These guards lock the wiring so a refactor cannot
// silently drop the gate. (loopback-e2e.sh is single-box and never touches that stack, so it is
// intentionally NOT gated — asserted separately.)
// ---------------------------------------------------------------------------

/// recording-e2e.sh must invoke the version-integrity gate with --win-state for BOTH boxes (so a
/// missing state file -> UNKNOWN -> the gate refuses, never a silent pass on an unverified build).
#[test]
fn recording_e2e_runs_the_version_integrity_gate_for_both_boxes() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("version-integrity-gate.sh"),
        "#123: recording-e2e.sh must invoke version-integrity-gate.sh"
    );
    assert!(
        s.contains("--win-state \"strih=$VERSION_STRIH_STATE\""),
        "#123: the gate must ALWAYS be given strih=--win-state (missing -> UNKNOWN -> refuse)"
    );
    assert!(
        s.contains("--win-state \"stream=$VERSION_STREAM_STATE\""),
        "#123: the gate must ALWAYS be given stream=--win-state (missing -> UNKNOWN -> refuse)"
    );
}

/// The version-integrity gate must run BEFORE StartRecord (#123) — fail fast on a drifted stack
/// before any measurement, exactly like the DanteSync gate.
#[test]
fn version_integrity_gate_runs_before_any_recording() {
    let s = read("scripts/recording-e2e.sh");
    let gate = s
        .find("version-integrity-gate.sh")
        .expect("recording-e2e.sh must invoke version-integrity-gate.sh");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        gate < start_record,
        "#123: the version-integrity gate must run BEFORE StartRecord (fail fast on a drifted stack)"
    );
}

/// loopback-e2e.sh is a SINGLE-BOX cam->NDI->tap test that never touches the strih/stream genlock
/// stack, so the strih/stream version-integrity gate does NOT apply — it must NOT be wired in (and
/// the header must say so, so a future reader does not add it "to match" the other scripts).
#[test]
fn loopback_e2e_is_intentionally_not_version_gated() {
    let s = read("scripts/loopback-e2e.sh");
    assert!(
        !s.contains("version-integrity-gate.sh"),
        "#123: loopback is single-box and must NOT run the strih/stream version-integrity gate"
    );
    assert!(
        s.contains("NO version-integrity gate here (#123)"),
        "#123: loopback must document WHY it is intentionally not gated (so it is not added later)"
    );
}

/// Topology v2 (#459, EPIC #466, SUPERSEDES the #11 mixed-60/30 framing this test used to lock):
/// strih dropped from the 60fps LED-wall IMAG role to a 30fps cut-to-stream-only box (the 60fps
/// IMAG role moved to the new imag-nb box, #458/#463). Cam boxes still emit 60fps NDI (cam1 still
/// films cam2's 60Hz-painted monitor at 60fps for the optical proof), so the harness must STILL
/// default the cam1 emit rate (GENLOCK_FPS → CAMERA_BOX_GENLOCK_FPS) to 60 — only the RECORDED
/// PROGRAM rate changes: STRIH_CAPTURE_FPS now defaults to 30 (matching strih's own 30fps
/// cut-to-stream canvas, not the old 60fps IMAG canvas), STREAM_CAPTURE_FPS stays 30 (unchanged —
/// stream now receives an ALREADY-30fps feed, plain pass-through, no further decimation). The
/// decimation LOSS step is gap-ignore for every node regardless of these rates (#360); the
/// --strih-emit-fps / --stream-capture-fps CLI threading below is RETAINED for provenance,
/// DECOUPLED from the diagnostic --capture-fps so it is always correct regardless of which
/// recording's rate is in effect.
#[test]
fn recording_e2e_defaults_to_the_v2_topology_fps_and_threads_per_box_fps_459() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("GENLOCK_FPS=\"${GENLOCK_FPS:-60}\""),
        "#459: cam1 NDI emit rate (CAMERA_BOX_GENLOCK_FPS) must stay 60 -- cameras are unaffected \
         by strih's fps move."
    );
    assert!(
        s.contains("STRIH_CAPTURE_FPS=\"${STRIH_CAPTURE_FPS:-30}\""),
        "#459: the strih recording fps (diagnostic span + optical step) must now default to 30 \
         -- strih records its own 30fps cut-to-stream canvas, not the old 60fps IMAG canvas."
    );
    assert!(
        s.contains("STREAM_CAPTURE_FPS=\"${STREAM_CAPTURE_FPS:-30}\""),
        "#459: the stream recording fps must stay 30 -- stream now receives an already-30fps \
         feed, plain pass-through."
    );
    assert!(
        s.contains("PAINT_FPS=\"${PAINT_FPS:-60}\""),
        "#459: the painter rate must stay 60 -- cam2's monitor refresh is unaffected by strih's \
         fps move (moot under KMS vblank, correct on fallback)."
    );
    // The cam1 launch must pass the emit rate through to camera-box.
    assert!(
        s.contains("CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS"),
        "cam1 must launch with CAMERA_BOX_GENLOCK_FPS=$GENLOCK_FPS."
    );
    // The stream on-box extract decodes the 30fps stream recording → --capture-fps STREAM_CAPTURE_FPS.
    assert!(
        s.contains("--capture-fps \"$STREAM_CAPTURE_FPS\""),
        "the stream on-box extract must use --capture-fps \"$STREAM_CAPTURE_FPS\" (30)."
    );
    // The strih extract, the dev1 merge, and the legacy fused verdict read the (now 30fps) strih
    // recording (cam1 burn) → --capture-fps STRIH_CAPTURE_FPS (>=3 sites).
    let strih_fps = s.matches("--capture-fps \"$STRIH_CAPTURE_FPS\"").count();
    assert!(
        strih_fps >= 3,
        "--capture-fps \"$STRIH_CAPTURE_FPS\" (30) must be threaded into the strih extract, the \
         merge, and the fused verdict (>=3 sites); found {strih_fps}."
    );
    // The decimation LOSS step is rig-pinned + decoupled from the diagnostic --capture-fps.
    // Threaded on the stream extract, the merge, AND the fused verdict (the burns the stream
    // recording carries) -- unchanged by the #459 topology move (node_render_step is gap-ignore
    // for every node regardless of these values, #360).
    let emit = s.matches("--strih-emit-fps \"$STRIH_CAPTURE_FPS\"").count();
    let dec = s
        .matches("--stream-capture-fps \"$STREAM_CAPTURE_FPS\"")
        .count();
    assert!(
        emit >= 3 && dec >= 3,
        "the decimation step (--strih-emit-fps + --stream-capture-fps) must be threaded into the \
         stream extract, the merge, and the fused verdict (>=3 each); found emit={emit} dec={dec}."
    );
}

/// #332: the env-gated all-cambox sweep must run the per-cambox verdict on the DEFAULT decode-on-
/// stream path (VERDICT_ON_STREAM=1, #193). The old guard that FORCED VERDICT_ON_STREAM=0 (which
/// pulled the multi-GB decode onto dev1) is GONE; the per-box `--merge-partials` step now consumes
/// `--switch-schedule`, so the all_cambox continuity is computed on the stream box. The legacy
/// decode-on-dev1 path (VERDICT_ON_STREAM=0) still wires it via VERDICT_ARGS. The DEFAULT (non-
/// sweep) single-scene hold (`sleep "$DURATION"`) stays intact for non-sweep runs.
#[test]
fn recording_e2e_all_cambox_sweep_runs_on_stream_box() {
    let s = read("scripts/recording-e2e.sh");
    // (a) the #332 guard that forced VERDICT_ON_STREAM=0 for ALL_CAMBOX must be REMOVED — the sweep
    //     must NOT error out / force the dev1 decode any more.
    assert!(
        !s.contains("ALL_CAMBOX=1 requires VERDICT_ON_STREAM=0"),
        "#332: the guard forcing VERDICT_ON_STREAM=0 for ALL_CAMBOX must be removed — the all-cambox \
         verdict now runs on the stream box via the merge path (#193, decode on stream.lan)."
    );
    assert!(
        !s.contains(r#"if [ "${VERDICT_ON_STREAM:-1}" != "0" ]; then"#),
        "#332: the VERDICT_ON_STREAM!=0 fail-fast guard for ALL_CAMBOX must be gone."
    );
    // (b) the per-box MERGE path (the default VERDICT_ON_STREAM=1 decode-on-stream) must now append
    //     --switch-schedule to MERGE_ARGS, gated on ALL_CAMBOX + the schedule file (if-form, #178).
    assert!(
        s.contains("MERGE_ARGS+=(--switch-schedule"),
        "#332: --switch-schedule must be appended to MERGE_ARGS so the all_cambox continuity is \
         computed in the per-box merge (the default decode-on-stream path)."
    );
    assert!(
        s.contains(r#"if [ "${ALL_CAMBOX:-0}" = "1" ] && [ -f "$SWITCH_SCHEDULE_JSON" ]; then"#),
        "#332: the --switch-schedule append must be gated on ALL_CAMBOX=1 AND the schedule file \
         existing, in if-form (not `] && MERGE_ARGS+=(` which set -e-aborts when absent, #178)."
    );
    // (c) the LEGACY decode-on-dev1 path (VERDICT_ON_STREAM=0) keeps its VERDICT_ARGS wiring.
    assert!(
        s.contains("VERDICT_ARGS+=(--switch-schedule"),
        "#332: the legacy decode-on-dev1 path must still wire --switch-schedule via VERDICT_ARGS."
    );
    // (d) the DEFAULT (no ALL_CAMBOX) path must keep the single steady-state hold — since
    // #11/#373 padded by RECORD_PAD so the analyzed span can reach the --min-secs DURATION
    // floor (an exactly-DURATION window always missed it: run 7020001, 299.9 s < 300.0).
    // The #312 intent (ONE steady hold, not the sweep's segment loop) is unchanged.
    assert!(
        s.contains(r#"sleep "$(( DURATION + RECORD_PAD ))""#),
        "#312/#11: the default (non-sweep) path must keep the single padded steady-state hold \
         `sleep \"$(( DURATION + RECORD_PAD ))\"`."
    );
}

/// #312 CORRECTS #333/#399: the all-cambox DEFAULT sweep no longer excludes the dual-QR painter
/// box (CAM2 / .62). #333's original exclusion reasoning — "while painting the monitor, cam2
/// does NOT capture/emit its own camera NDI (#179 'cam2 paints, NO grab')" — went STALE the
/// moment #291 (closed 2026-06-28) landed: cam2's camera-box daemon keeps CAPTURING + EMITTING
/// its own NDI feed throughout a TEST run (only its framebuffer is freed for the separate
/// frame-probe painter process, via a transient CAMERA_BOX_NO_DISPLAY=1 relaunch —
/// `scripts/rig-mode.sh` verbatim: "display output is the ONLY thing that grabs fb0; /dev/video0
/// capture + NDI emit do not, so cam2 stays a MEASURABLE camera during the test"). CAMBOX_SWEEP
/// and CAMERA_UNDER_TEST_NODES (src/bin/recording-verdict.rs) were never updated after #291
/// landed — #312 fixes both: cam2 is now included, deployed via the `[2b/8]` loop with its OWN
/// reserved digital capture-burn id (mirroring cam1/cam3/cam4/cam5/cam6 exactly) and
/// CAMERA_BOX_NO_DISPLAY=1 so the frame-probe painter can still own /dev/fb0.
///
/// Under the #399 canonical NDI mapping (NDI cam5→CAM1, cam1→CAM3, cam3→CAM4, cam2→CAM2,
/// cam4→CAM5, cam6→CAM6; scene names follow the input labels), cam2 is scene "Cam 2"; #312 also
/// wires in cam5 (scene "Cam 4") and cam6 (scene "Cam 6") — fleet growth 4→6, #451. So the
/// default sweeps ALL SIX cameras, still overridable via $CAMBOX_SWEEP.
///
/// The ORIGINAL #328/#440 concern this exclusion partly guarded against — the PERMANENT
/// `cam2-painter.service` and the transient TEST-mode painter BOTH fighting over `/dev/fb0` — is
/// UNRELATED to the sweep-inclusion question and still needs its own guard: see
/// `recording_e2e_cam2_deploy_stops_the_permanent_painter_service_before_launching_the_transient_probe`
/// below.
#[test]
fn recording_e2e_default_sweep_covers_all_six_cameras_including_cam2() {
    let s = read("scripts/recording-e2e.sh");
    let line = s
        .lines()
        .find(|l| l.contains("CAMBOX_SWEEP=\"${CAMBOX_SWEEP:-"))
        .expect("#333: recording-e2e.sh must define a default CAMBOX_SWEEP");
    assert!(
        line.contains("Cam 2:CAM2"),
        "#312: the default CAMBOX_SWEEP must include cam2 (scene 'Cam 2') — #291 made it a \
         MEASURABLE camera during a TEST run, so excluding it is now stale: {line}"
    );
    for (scene, label) in [
        ("Cam 5:CAM1", "CAM1"),
        ("Cam 1:CAM3", "CAM3"),
        ("Cam 3:CAM4", "CAM4"),
        ("Cam 4:CAM5", "CAM5"),
        ("Cam 6:CAM6", "CAM6"),
    ] {
        assert!(
            line.contains(scene),
            "#312: the default sweep must still cover {label} via '{scene}': {line}"
        );
    }
    assert!(
        line.contains("${CAMBOX_SWEEP:-"),
        "#333: CAMBOX_SWEEP must remain env-overridable: {line}"
    );
}

/// #312/#440: the ORIGINAL concern behind excluding cam2 — a PERMANENT `cam2-painter.service`
/// and the TRANSIENT TEST-mode frame-probe painter BOTH holding /dev/fb0 at once (they'd fight
/// over the framebuffer and the displayed QR would alternate between the two painters' run_ids,
/// breaking `--av-sync` frame_id pairing, #440) — is a SEPARATE, still-real risk from the
/// sweep-inclusion question above. This locks that the `[2b/8]` ALL_CAMBOX deploy loop's cam2
/// branch stops the permanent service BEFORE launching cam2's manually nohup'd probe-featured
/// binary, mirroring `[3/8]`'s own painter launch (which does the same for the plain
/// single-camera path).
#[test]
fn recording_e2e_cam2_deploy_stops_the_permanent_painter_service_before_launching_the_transient_probe(
) {
    let s = read("scripts/recording-e2e.sh");
    // The [2b/8] loop body: find the block between the `for _cn_ip_burn in` loop header and its
    // matching `done`, and confirm it stops cam2-painter.service unconditionally for every box in
    // the loop (harmless no-op on cam3/cam4/cam5/cam6) BEFORE it stops/replaces camera-box.
    //
    // The slice is BOUNDED to the loop's own `done` (the first one after the header — this loop
    // has no nested loop of its own) — NOT left open to end-of-file. recording-e2e.sh contains
    // near-identical "systemctl stop cam2-painter" / "systemctl stop camera-box; pkill -x
    // camera-box" text elsewhere (the plain single-camera [3/8] painter launch, and the
    // unrelated av_restart_record_and_emit_plan() helper) — an unbounded slice would let those
    // LATER occurrences silently satisfy these assertions even if the real [2b/8] loop body were
    // broken, so bounding to the matching `done` is load-bearing, not cosmetic.
    let loop_start = s
        .find("for _cn_ip_burn in")
        .expect("#312: recording-e2e.sh must define the [2b/8] ALL_CAMBOX deploy loop");
    let loop_end = s[loop_start..]
        .find("\n  done\n")
        .map(|i| loop_start + i)
        .expect("#312: the [2b/8] loop must be closed by its own `done`");
    let loop_body = &s[loop_start..loop_end];
    let stop_painter_pos = loop_body
        .find("systemctl stop cam2-painter")
        .expect("#312/#440: the [2b/8] loop must stop the PERMANENT cam2-painter.service");
    let stop_camerabox_pos = loop_body
        .find("systemctl stop camera-box; pkill -x camera-box")
        .expect("#312: the [2b/8] loop must stop/replace the deployed camera-box service");
    assert!(
        stop_painter_pos < stop_camerabox_pos,
        "#312/#440: cam2-painter.service must be stopped BEFORE camera-box is stopped/replaced \
         in the [2b/8] loop — reversing the order risks the two painters fighting over /dev/fb0"
    );
    // The loop must also carry cam2's own NO_DISPLAY opt-out, so its manually-launched binary
    // never re-grabs fb0 out from under the separate frame-probe painter launched next.
    assert!(
        loop_body.contains("CAMERA_BOX_NO_DISPLAY=1"),
        "#312: the [2b/8] loop must launch cam2's binary with CAMERA_BOX_NO_DISPLAY=1 (the #291 \
         opt-out) so it never competes with the frame-probe painter for /dev/fb0"
    );
}

/// #312 (BUG, regression-test-first) — cam2's cleanup restore block force-kills its manually
/// deployed `/tmp/camera-box-burn-cam2-<RUN_ID>` binary (added alongside cam3/cam4/cam5/cam6's
/// identical restore, mirroring #626's digit-anchored kill) but originally never REMOVED it from
/// disk — unlike every other node's cleanup block, which pairs the kill with `rm -f
/// /tmp/camera-box-burn-*`. Every ALL_CAMBOX=1 run left a fresh multi-MB binary behind on cam2's
/// disk-constrained `/tmp`, accumulating across repeated runs.
#[test]
fn recording_e2e_cam2_cleanup_removes_its_deployed_burn_binary_from_disk_312() {
    let s = read("scripts/recording-e2e.sh");
    // Isolate cam2's OWN cleanup restore block (targets $PAINTER_IP) from the rest of cleanup().
    let cam2_restore_start = s
        .find("root@\"$PAINTER_IP\" \"pkill -x frame-probe")
        .expect("#312: cleanup() must define cam2's own restore block (root@\"$PAINTER_IP\")");
    let cam2_restore_end = s[cam2_restore_start..]
        .find("systemctl start cam2-painter")
        .map(|i| cam2_restore_start + i)
        .expect("#312: cam2's restore block must (re)start cam2-painter.service");
    let cam2_restore = &s[cam2_restore_start..cam2_restore_end];
    assert!(
        cam2_restore.contains("pkill -9 -f 'camera-box-burn-[a-z0-9]'"),
        "#312/#626/#628: cam2's restore block must force-kill its manually deployed burn binary \
         with the anchored pattern (widened to [a-z0-9] by #628 -- cam2's own burn is \
         camname-infixed, camera-box-burn-cam2-<run_id>, which the original digit-only pattern \
         never matched): {cam2_restore:?}"
    );
    assert!(
        cam2_restore.contains("rm -f /tmp/camera-box-burn-*"),
        "#312: cam2's restore block must ALSO remove its deployed burn binary from disk (rm -f \
         /tmp/camera-box-burn-*), mirroring cam1/cam3/cam4/cam5/cam6's identical cleanup — \
         otherwise every ALL_CAMBOX=1 run leaks a fresh multi-MB binary on cam2's /tmp: {cam2_restore:?}"
    );
}

// --- #24 item 1: the single-node full-path launch must be parameterized over which physical
// camera plays the SOURCE-camera role (cam1/cam3/cam4), not hard-coded to cam1. -----------------

#[test]
fn recording_e2e_source_camera_ip_resolves_via_camera_set_not_hardcoded() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains(r#"CAM1_IP="${CAM1_IP:-10.77.9.61}""#),
        "#24: CAM1_IP must no longer default to the literal cam1 IP -- it must resolve through \
         camera_resolve()/camera-set.sh so CAM=cam3 or CAM=cam4 deploys THAT box instead."
    );
    assert!(
        s.contains(r#"CAM1_IP="${CAM1_IP:-$CAMERA_IP}""#),
        "#24: CAM1_IP must default to $CAMERA_IP (the resolved SOURCE camera's IP, set by \
         camera_resolve() from CAM=cam1|cam3|cam4), so an explicit CAM1_IP override still wins."
    );
}

#[test]
fn recording_e2e_routes_the_source_camera_via_camera_strih_route() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(r#"camera_strih_route "$CAMERA_NAME""#),
        "#24: recording-e2e.sh must call camera_strih_route(\"$CAMERA_NAME\") to resolve which \
         strih scene/NDI-input shows the selected SOURCE camera -- rejecting cam2/cam5/cam6 \
         loudly instead of silently certifying the wrong box."
    );
}

#[test]
fn recording_e2e_strih_scene_and_source_derive_from_the_resolved_camera() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains(r#"STRIH_PROG_SCENE="${STRIH_PROG_SCENE:-Cam 5}""#),
        "#24: STRIH_PROG_SCENE must no longer hard-code scene 'Cam 5' (cam1-only) -- it must \
         default to $CAMERA_STRIH_SCENE (resolved per the selected SOURCE camera)."
    );
    assert!(
        s.contains(r#"STRIH_PROG_SCENE="${STRIH_PROG_SCENE:-$CAMERA_STRIH_SCENE}""#),
        "#24: STRIH_PROG_SCENE must default to $CAMERA_STRIH_SCENE so an explicit override \
         still wins."
    );
    assert!(
        !s.contains(r#"STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-NDI cam5}""#),
        "#24: STRIH_PROG_SOURCE must no longer hard-code 'NDI cam5' (cam1-only)."
    );
    assert!(
        s.contains(r#"STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-$CAMERA_STRIH_SOURCE}""#),
        "#24: STRIH_PROG_SOURCE must default to $CAMERA_STRIH_SOURCE so an explicit override \
         still wins."
    );
}

#[test]
fn recording_e2e_strih_upstream_ndi_derives_from_the_resolved_camera_source() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        !s.contains(r#"STRIH_UPSTREAM_NDI="${STRIH_UPSTREAM_NDI:-CAM1 (usb)}""#),
        "#24: STRIH_UPSTREAM_NDI must no longer hard-code cam1's NDI name -- it must default to \
         $CAMERA_SOURCE (the selected SOURCE camera's own NDI name, e.g. 'CAM3 (usb)')."
    );
    assert!(
        s.contains(r#"STRIH_UPSTREAM_NDI="${STRIH_UPSTREAM_NDI:-$CAMERA_SOURCE}""#),
        "#24: STRIH_UPSTREAM_NDI must default to $CAMERA_SOURCE so an explicit override still wins."
    );
}

#[test]
fn recording_e2e_deploy_uses_the_burn_id_matching_the_resolved_camera() {
    let s = read("scripts/recording-e2e.sh");
    // The [2/8] deploy step must launch the burn binary with the id belonging to WHICHEVER
    // physical camera was resolved ($CAMERA_NAME) -- never unconditionally cam1's id, or
    // deploying cam3/cam4 as the primary source would still carry cam1's reserved id (911001),
    // silently mislabeling the frames and (if ALL_CAMBOX ever ran alongside) colliding with
    // cam1's own real burn.
    assert!(
        s.contains("CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID"),
        "#24: the [2/8] deploy step must set CAMERA_BOX_BURN_RUN_ID=$SRC_BURN_RUN_ID -- the \
         burn id resolved from BURN_CAM1_RUN_ID/BURN_CAM3_RUN_ID/BURN_CAM4_RUN_ID matching \
         $CAMERA_NAME -- not the fixed cam1-only $BURN_CAM1_RUN_ID."
    );
    assert!(
        !s.contains("CAMERA_BOX_BURN_RUN_ID=$BURN_CAM1_RUN_ID"),
        "#24: the [2/8] deploy step must no longer hard-wire cam1's burn id unconditionally."
    );
}

#[test]
fn recording_e2e_rejects_all_cambox_combined_with_a_non_cam1_source_camera() {
    // ALL_CAMBOX=1's OWN secondary-camera deploy loop ([2b/8]) unconditionally deploys cam3 AND
    // cam4 at their FIXED physical IPs (CAM3_IP/CAM4_IP) alongside the primary. If CAM=cam3 (or
    // cam4) is ALSO picked as the primary SOURCE camera, [2/8] would deploy the SAME physical
    // box a second time under a different burn binary -- a real device/process conflict, not
    // just a labeling nit. This combination must fail loudly instead of silently double-deploying.
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(r#"[ "${ALL_CAMBOX:-0}" = "1" ] && [ "$CAMERA_NAME" != "cam1" ]"#),
        "#24: recording-e2e.sh must reject ALL_CAMBOX=1 combined with a non-cam1 CAM -- ALL_CAMBOX's \
         own [2b/8] loop already deploys cam3/cam4 at their fixed IPs; picking one of them as the \
         primary SOURCE camera too would double-deploy the same physical box."
    );
}
