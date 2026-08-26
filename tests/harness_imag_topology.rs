//! #462 (EPIC #466 Topology v2, Phase 4 script integration) — imag-nb as a THIRD recorded+decoded
//! node in `scripts/recording-e2e.sh` + `scripts/render-budget-gate.py`'s docstring example.
//!
//! The recording-verdict SIDE (`--imag`, the 6th NodeSpec, `imag_capture_fps`, the optical-AND-burn
//! gate) was already wired by #461/#463 — these guards lock the SCRIPT wiring that was genuinely
//! missing: the [0/8] reachability preflight, the [4d/8] render-budget-gate call site, the
//! [5/8]..[8/8] record→decode→merge pipeline, and the shared BURN_TARGETS safety array. Pure static
//! (`fs::read_to_string` + substring/ordering asserts) — mirrors harness_recording_e2e_paths.rs and
//! harness_render_budget_gate.rs's style; no OBS, no ssh, no live rig.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// ---------------------------------------------------------------------------
// [0/8] reachability preflight + host lists (recording-e2e.sh:144, per the issue text).
// ---------------------------------------------------------------------------

/// imag-nb must be a named topology constant (resolved via scripts/imag-host.sh, #832) and must
/// appear in the [0/8] reachability preflight's host list alongside cam1/cam2/strih/stream.
#[test]
fn recording_e2e_defines_imag_ip_and_checks_it_reachable() {
    let s = read("scripts/recording-e2e.sh");
    // #832: IMAG_IP's default is no longer an independently hardcoded literal here -- it is
    // derived by sourcing scripts/imag-host.sh (the ONE declared imag host, reversible between
    // the incumbent .182 and the replacement .187 — see tests/harness_imag_host.rs).
    assert!(
        s.contains(". \"$HERE/imag-host.sh\""),
        "#832: recording-e2e.sh must source scripts/imag-host.sh to derive IMAG_IP."
    );
    let preflight = s
        .find("reachability preflight")
        .expect("#462: recording-e2e.sh must have a [0/8] reachability preflight banner");
    // Scope to the preflight `for` loop line itself.
    let loop_end = preflight
        + s[preflight..]
            .find("done")
            .expect("preflight loop must close with `done`");
    let region = &s[preflight..loop_end];
    assert!(
        region.contains("imag=$IMAG_IP"),
        "#462: the [0/8] reachability preflight must include imag alongside cam1/cam2/strih/stream. \
         Got:\n{region}"
    );
}

/// #462: imag-nb's OWN capture rate (60fps, its own low-latency IMAG box — never strih's/stream's)
/// must be a named, env-overridable constant feeding `--imag-capture-fps`, mirroring
/// STRIH_CAPTURE_FPS/STREAM_CAPTURE_FPS exactly.
#[test]
fn recording_e2e_defines_imag_capture_fps_default_60() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("IMAG_CAPTURE_FPS=\"${IMAG_CAPTURE_FPS:-60}\""),
        "#462/#461: IMAG_CAPTURE_FPS must default to 60 (imag-nb's own low-latency rate)."
    );
}

// ---------------------------------------------------------------------------
// [4d/8] render-budget-gate call site — `--box imag=10.77.9.182:60` (issue text point 3).
//
// issue 888 (2026-07-30, temporary user-directed relaxation) SPLIT this from one joint 3-box
// call into TWO separate render-budget-gate.py invocations: strih+stream stay strict in their
// own call, imag is measured by its OWN separate, report-only call (no `exit 1`, a loud WARN
// naming issue 888/886). See tests/harness_render_budget_imag_report_only_888.rs for the full
// lock on the split's non-aborting/WARN/no-env-knob shape — this file only guards that strih and
// stream keep their 30fps boxes and that imag is no longer folded into their same call/window.
// ---------------------------------------------------------------------------

/// strih/stream MUST keep their own `--box …:30` args in their own (still-strict) call, and imag
/// must NOT be part of that same call/window any more (issue 888 split it out into its own
/// separate, report-only call — locked in tests/harness_render_budget_imag_report_only_888.rs).
#[test]
fn render_budget_gate_strih_stream_call_site_no_longer_includes_imag_888() {
    let s = read("scripts/recording-e2e.sh");
    // Anchor on `--box "strih=` (NOT the bare "render-budget-gate.py" script name) -- #758 added
    // an EARLIER, imag-only render-budget-gate.py preflight call (an [1/8] render-health check,
    // before ANY box is deployed), so `.find("render-budget-gate.py")` would now latch onto that
    // one-box call instead of this [4d/8] call site. `--box "strih=` is unique to this call (the
    // [1/8] preflight never measures strih) and is therefore anchor-stable regardless of how many
    // OTHER render-budget-gate.py invocations get added elsewhere in the future.
    let call = s
        .find("--box \"strih=")
        .expect("recording-e2e.sh must invoke render-budget-gate.py with a strih box");
    // Scope to the actual invocation block (a handful of lines around the strih box arg).
    let window = &s[call.saturating_sub(200)..(call + 500).min(s.len())];
    assert!(
        window.contains("--box \"strih=${STRIH}:${RENDER_TARGET_FPS_STRIH:-30}\""),
        "render-budget-gate call must keep the strih=…:30 box. Got:\n{window}"
    );
    assert!(
        window.contains("--box \"stream=${STREAM}:${RENDER_TARGET_FPS_STREAM:-30}\""),
        "render-budget-gate call must keep the stream=…:30 box. Got:\n{window}"
    );
    assert!(
        !window.contains("--box \"imag="),
        "issue 888: imag must no longer be folded into the strih/stream call/window -- it must \
         be measured by its OWN separate, report-only call. Got:\n{window}"
    );
}

/// The render-budget-gate.py docstring's usage example must show all THREE boxes (imag at 60fps),
/// not just strih+stream — the doc must not silently drift from what the harness actually calls.
#[test]
fn render_budget_gate_docstring_example_includes_imag() {
    let s = read("scripts/render-budget-gate.py");
    let doc_end = s.find("\"\"\"\nimport").unwrap_or(s.len().min(2000));
    let doc = &s[..doc_end];
    assert!(
        doc.contains("--box imag=10.77.9.182:60"),
        "#462: render-budget-gate.py's docstring usage example must include the imag box \
         (--box imag=10.77.9.182:60). Got:\n{doc}"
    );
}

// ---------------------------------------------------------------------------
// [5/8]..[7/8] — StartRecord / StopRecord on imag alongside strih + stream.
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_starts_and_stops_recording_on_imag() {
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must StartRecord on strih+stream");
    let stop = s
        .find("StopRecord + download")
        .expect("recording-e2e.sh must have a [7/8] StopRecord step");
    let start_region = &s[start..stop];
    assert!(
        start_region.contains("record --host \"$IMAG_IP\" --action start"),
        "#462: [5/8] must StartRecord on imag alongside strih+stream. Got:\n{start_region}"
    );
    let stop_region_end = s[stop..].find("[8/8]").map(|i| stop + i).unwrap_or(s.len());
    let stop_region = &s[stop..stop_region_end];
    assert!(
        stop_region.contains("IMAG_HOST_PATH=$(python3 \"$HERE/obs_phase2.py\" record --host \"$IMAG_IP\" --action stop)"),
        "#462: [7/8] must StopRecord on imag and capture its host path (IMAG_HOST_PATH). Got:\n{stop_region}"
    );
}

/// cleanup() must ALSO stop any leftover imag recording (the same safety net strih/stream get) so
/// an aborted mid-flight run never leaves an un-finalized recording on imag.
#[test]
fn recording_e2e_cleanup_stops_imag_recording_too() {
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
        body.contains("record --host \"$IMAG_IP\" --action stop"),
        "#462: cleanup() must StopRecord on imag as a safety net (mirrors strih+stream). Got:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// #462: imag joins the shared BURN_TARGETS array (#252's "a third box" — anticipated by the
// existing design comment) — the [4b/8] pre-record burn-ON gate and the #246 cleanup() burn-clear
// loop both iterate it, so imag's 911003 burn can never be left ON after a run/abort.
// ---------------------------------------------------------------------------

#[test]
fn burn_targets_array_includes_imag() {
    let s = read("scripts/recording-e2e.sh");
    let def = s
        .find("BURN_TARGETS=(")
        .expect("recording-e2e.sh must define BURN_TARGETS");
    let decl_end = def + s[def..].find(')').expect("BURN_TARGETS=( must close");
    let decl = &s[def..decl_end];
    assert!(
        decl.contains("$STRIH=") && decl.contains("$STREAM=") && decl.contains("$IMAG_IP="),
        "#462: BURN_TARGETS must cover strih, stream, AND imag (the third box the #252 design \
         comment anticipated). Got: {decl}"
    );
    // Still defined exactly once, and still BEFORE the cleanup trap (the #252 set -u safety).
    assert_eq!(
        s.matches("BURN_TARGETS=(").count(),
        1,
        "BURN_TARGETS must remain defined exactly once."
    );
    let trap = s
        .find("trap cleanup")
        .expect("recording-e2e.sh must install the cleanup trap");
    assert!(
        def < trap,
        "#462: BURN_TARGETS (now including imag) must still be defined BEFORE `trap cleanup`."
    );
}

/// The imag program-feeding NDI source used as the burn target must be an overridable var DERIVED
/// from the camera-under-test (issue 1204: it was hard-pinned to 'NDI CAM1' and diverged from the
/// program route the moment CAMERA_NAME != cam1 -- cam1 offline-acked, active set = cam3 -> imag
/// recorded zero 911003 anchors). It must now resolve via imag_source_for_camera "$CAMERA_NAME",
/// the SAME camera-under-test resolution the program SCENE uses (imag_scene_for_camera).
#[test]
fn imag_prog_source_constant_is_defined() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(
            "IMAG_PROG_SOURCE=\"${IMAG_PROG_SOURCE:-$(imag_source_for_camera \"$CAMERA_NAME\")}\""
        ),
        "#1204: IMAG_PROG_SOURCE must default to $(imag_source_for_camera \"$CAMERA_NAME\") (the \
         input backing imag's routed program scene), never a hard-pinned 'NDI CAM1'."
    );
}

/// #526: pin the VERIFIED physical camera <-> NDI-name mapping. Live-checked 2026-07-05 (all 6
/// boxes up): box 10.77.9.61 -> "CAM1 (usb)" ... .66 -> "CAM6 (usb)" — a clean 1:1 by box number,
/// so the multiview tile order (scene list MV Cam 1..6) matches the physical camera numbering the
/// cutter expects. Guard the 1:1 binding in imag_scenes.py so a silent reorder can't drift it.
#[test]
fn imag_scenes_pins_verified_1to1_camera_mapping_526() {
    let s = read("scripts/imag_scenes.py");
    assert!(
        s.contains(r#"f"CAM{n} (usb)""#),
        "#526: imag_scenes.py must bind each scene 1:1 to \"CAM{{n}} (usb)\" (the verified \
         physical box-number mapping)."
    );
    assert!(
        s.contains(r#"f"MV Cam {n}""#),
        "#526: the low-bw twin scenes must be named \"MV Cam {{n}}\" so the multiview tile order \
         = physical camera order 1..6."
    );
    assert!(
        s.contains("VERIFIED physical camera") && s.contains("10.77.9.61"),
        "#526: the verified box<->NDI mapping must be documented in imag_scenes.py so the pin is \
         auditable (not a bare magic 1:1)."
    );
}

/// #502/#847: imag must run on a named ADVANCED profile with a native-1080p60 mkv recording, not
/// the naive default Simple profile (x264 @ 6 Mbps 'Stream' quality, which softens the E2E
/// QR/burns). imag records its own program for the topology-v2 verdict, so the recording must be
/// native-resolution + high quality. Verified live 2026-07-05: produces a 1920x1080@60 h264-NVENC
/// .mkv on the INCUMBENT (RTX 5050) box.
///
/// #847 SUPERSEDES this test's original premise: the recording encoder is no longer a hardcoded
/// `obs_nvenc_h264_tex` literal -- the replacement notebook (10.77.9.187, Intel iGPU only) never
/// initializes NVENC, so a hardcoded-NVENC seed silently produced 0-byte recordings there (live-
/// diagnosed, see the #847 issue). `seed_profile()` now takes `has_discrete_nvidia` and derives
/// the encoder via `select_rec_encoder()`: NVENC when a discrete NVIDIA GPU is present (byte-for-
/// byte unchanged for the incumbent box); since #1143 the selection delegates to the Tier-0 pure
/// `imag_record_encoder.choose_record_encoder` (VAAPI-texture default on the Intel bundle,
/// x264 fallback) -- NEVER qsv
/// (live-tested and confirmed unreliable on this hardware/build, see the #847 design comment).
/// Pin the NEW hardware-aware contract instead of the old hardcoded one.
#[test]
fn imag_scenes_seeds_advanced_hardware_aware_recording_profile_847() {
    let s = read("scripts/imag_scenes.py");
    for needle in [
        r#""profileName": "imag-60fps""#,
        r#"("Output", "Mode", "Advanced")"#,
        r#"("AdvOut", "RecEncoder", rec_encoder)"#,
        r#"("AdvOut", "RecRescale", "false")"#,
        r#"("AdvOut", "RecFormat2", "mkv")"#,
        "rec_encoder = select_rec_encoder(has_discrete_nvidia)",
        "def select_rec_encoder(has_discrete_nvidia: bool, available_encoders=None) -> str:",
        "return imag_record_encoder.choose_record_encoder(has_discrete_nvidia, available_encoders)",
    ] {
        assert!(
            s.contains(needle),
            "#847: imag_scenes.py must contain `{needle}` (Advanced, hardware-aware-encoder, \
             native-1080p mkv recording profile, applied before the scene seed)"
        );
    }
    // Never qsv as an actual RecEncoder VALUE (a quoted string literal) -- live-tested and
    // confirmed unreliable on this hardware/build (MFX_ERR_UNSUPPORTED at Init()); shipping it
    // would silently reproduce the exact zero-bytes failure #847 exists to fix. The design-
    // rationale comment above mentions the bare (unquoted) name "obs_qsv11_v2" for context, which
    // is fine -- only a QUOTED literal (something the code could actually return/assign) is banned.
    assert!(
        !s.contains("\"obs_qsv11"),
        "#847: imag_scenes.py must never select a qsv encoder id as a string literal -- \
         live-proven unreliable on 10.77.9.187"
    );
}

// ---------------------------------------------------------------------------
// [8/8] per-box decode-in-place: imag is extracted DIRECTLY (ssh/scp — no MCP plan needed) and
// folded into the printed dev1 merge command as a THIRD partial.
// ---------------------------------------------------------------------------

/// The on-imag helper must exist, be executable, and be invoked from the default per-box path —
/// UNLIKE the strih/stream planners, its invocation is not merely printed: recording-e2e.sh runs
/// it directly (imag is a plain-ssh Linux box), so $IMAG_PARTIAL is real by the time the merge
/// command is printed.
#[test]
fn recording_e2e_extracts_the_imag_partial_via_the_on_imag_helper() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("\"$HERE/recording-verdict-on-imag.sh\""),
        "#462: the default per-box path must invoke scripts/recording-verdict-on-imag.sh."
    );
    assert!(
        s.contains("--extract-partial imag --imag \"$IMAG_HOST_PATH\""),
        "#462: the on-imag invocation must extract-partial imag against its own recording."
    );
    assert!(
        s.contains("--imag-capture-fps \"$IMAG_CAPTURE_FPS\""),
        "#462: the on-imag extract must pass --imag-capture-fps (the #373 span-floor rate)."
    );
}

/// The final MERGE_ARGS (the command the win-* MCP holder runs after pulling strih+stream
/// partials) must fold in `--merge-partials imag=$IMAG_PARTIAL` — but only WHEN the imag extract
/// actually produced a partial (an `if [ -f ]` guard, #178 resilience: a failed/skipped imag leg
/// must never abort the merge of the other two nodes).
#[test]
fn merge_args_includes_imag_partial_conditionally() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("MERGE_ARGS+=(--merge-partials \"imag=$IMAG_PARTIAL\")"),
        "#462: the merge command must fold in the imag partial when present."
    );
    // It must be `if [ -f "$IMAG_PARTIAL" ]` — never an unguarded append (which would print a
    // dangling --merge-partials imag=<nonexistent> when the imag leg failed/was skipped).
    let fold = s
        .find("MERGE_ARGS+=(--merge-partials \"imag=$IMAG_PARTIAL\")")
        .unwrap();
    let head = &s[..fold];
    let guard = head
        .rfind("if [ -f \"$IMAG_PARTIAL\" ]")
        .expect("#462: the imag merge-arg fold must be guarded by `if [ -f \"$IMAG_PARTIAL\" ]`");
    // No unrelated `fi` between the guard and the fold (i.e. it's the SAME if-block).
    let between = &s[guard..fold];
    assert!(
        !between.contains("\nfi\n"),
        "#462: the guard and the fold must be in the SAME if-block. Got:\n{between}"
    );
}

/// `--imag-capture-fps` must ALSO be threaded into the dev1 merge's own arguments (the #373
/// duration-floor rate applies at merge time too, not just at per-box extract time).
#[test]
fn merge_args_threads_imag_capture_fps() {
    let s = read("scripts/recording-e2e.sh");
    let merge_args_def = s
        .find("MERGE_ARGS=(--merge-partials")
        .expect("recording-e2e.sh must build MERGE_ARGS");
    let merge_args_end = merge_args_def
        + s[merge_args_def..]
            .find(")\n")
            .expect("MERGE_ARGS=( ... ) must close");
    let region = &s[merge_args_def..merge_args_end];
    assert!(
        region.contains("--imag-capture-fps \"$IMAG_CAPTURE_FPS\""),
        "#462: MERGE_ARGS must pass --imag-capture-fps. Got:\n{region}"
    );
}

/// #462/#178: the [8/8c] imag extract call MUST be resilient — this region runs under `set -e`
/// (re-enabled at the top of the VERDICT_ON_STREAM=1 branch), so an UNGUARDED failing invocation
/// of recording-verdict-on-imag.sh (imag unreachable, a stale binary, a transient ssh hiccup)
/// would set -e-abort the WHOLE script, including the strih/stream plan the operator still needs
/// to run below. Locks the exact `#178`-style resilient pattern this region's own fix uses,
/// mirroring `stoprecord_to_verdict_reaches_verdict_despite_a_failing_step`'s isolated-snippet
/// behavioral proof for the StopRecord→verdict region.
#[test]
fn imag_extract_failure_never_aborts_the_rest_of_the_per_box_plan() {
    let s = read("scripts/recording-e2e.sh");
    // Static: the invocation is followed by `|| echo "WARNING...` (or an equivalent guard) on the
    // SAME statement, not a bare command that a `set -e` fires on.
    let call = s
        .find("\"$HERE/recording-verdict-on-imag.sh\"")
        .expect("#462: recording-e2e.sh must invoke recording-verdict-on-imag.sh");
    let window = &s[call..(call + 900).min(s.len())];
    assert!(
        window.contains("|| echo \"WARNING"),
        "#462/#178: the recording-verdict-on-imag.sh invocation must be guarded with `|| echo \
         \"WARNING...\"` (or equivalent) so a failure degrades gracefully instead of set -e-aborting \
         the whole per-box plan. Got:\n{window}"
    );

    // Behavioral: the EXACT resilient pattern the fix uses (`cmd && echo ... || echo WARNING`),
    // isolated in a bash snippet under `set -euo pipefail`, must reach a marker AFTER a failing
    // command instead of aborting — proving the guard actually works, not just that its text
    // happens to be present.
    let script = r#"
set -euo pipefail
REACHED=no
false \
&& echo "would only print on success" \
|| echo "WARNING: simulated imag failure (non-fatal)" >&2
REACHED=yes
echo "REACHED_MERGE_STEP reached=${REACHED}"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("run the resilient imag-extract pattern");
    assert!(
        out.status.success(),
        "#462/#178: the resilient `cmd && ok || WARNING` pattern must exit 0 despite the \
         simulated failure; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("REACHED_MERGE_STEP reached=yes"),
        "#462/#178: the step AFTER the guarded failing command must still be reached (never \
         set -e-aborted). stdout={stdout:?}"
    );
}
