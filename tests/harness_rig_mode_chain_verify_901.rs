//! #901 — rig-mode.sh `test` mode must verify the WHOLE chain (measurement audio arrives, Dante
//! up, QR on screen as the camera sees it), not just the cam side. Today (before this PR) it
//! ends with "RESULT: TEST mode — cam side PASS, burns ON" and a printed-but-never-enforced
//! "NEXT: confirm the PHASE2-PROBE scene..." hint — proven false-positive live 2026-07-31 (the
//! mbc sound card was off; TEST mode still reported PASS) and again 2026-08-04 (painter alive +
//! marker CSV growing while the program was BLACK for 150s; stream left on 'PRO'; burns on
//! non-rendered inputs; an inactive camera's mangled NDI pin went uncaught).
//!
//! These are STATIC-ANCHOR tests only (the repo's established pattern for rig-mode.sh/
//! recording-e2e.sh — see the project CLAUDE.md GOTCHA on the shared textual-collision risk):
//! they assert the new constants/functions exist and are CALLED from do_test(), never execute a
//! live OBS-WS/ssh call. See tests/python/test_obs_phase2_program_rendered_input_901.py,
//! test_obs_phase2_assert_program_nonblack_901.py, test_obs_phase2_mbc_input_check_901.py, and
//! tests/harness_audio_presence_preflight_tier_901.rs for the LIVE-LOGIC-level tests of the
//! pieces this wiring calls.

use std::fs;
use std::path::PathBuf;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/rig-mode.sh")
}

fn read() -> String {
    fs::read_to_string(script()).expect("read rig-mode.sh")
}

/// The text between the literal `do_test()` and `do_event()` markers — the same slicing
/// convention tests/rig_mode.rs already uses for this file.
fn do_test_body(s: &str) -> &str {
    s.split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("")
}

// --- new constants + sourcing ------------------------------------------------------------------

#[test]
fn defines_stream_ssh_credentials_matching_recording_e2e_convention() {
    let s = read();
    assert!(
        s.contains(r#"STREAM_USER="${STREAM_USER:-newlevel}""#),
        "#901: rig-mode.sh must define STREAM_USER (same default as recording-e2e.sh)"
    );
    assert!(
        s.contains(r#"STREAM_PW="${STREAM_PW:-newlevel}""#),
        "#901: rig-mode.sh must define STREAM_PW (same default as recording-e2e.sh)"
    );
}

#[test]
fn defines_mbc_input_name_and_audio_chain_thresholds() {
    let s = read();
    for needle in [
        r#"MBC_INPUT_NAME="${MBC_INPUT_NAME:-mbc}""#,
        "AUDIO_CHAIN_PROBE_SECS=",
        r#"AUDIO_CHAIN_DEAD_DB="${AUDIO_CHAIN_DEAD_DB:--80}""#,
        r#"AUDIO_CHAIN_WARN_DB="${AUDIO_CHAIN_WARN_DB:--60}""#,
        "RIG_MODE_AUDIO_CHAIN_ENABLE=",
    ] {
        assert!(
            s.contains(needle),
            "#901: rig-mode.sh must define {needle:?}"
        );
    }
}

#[test]
fn sources_audio_presence_preflight_and_win_ssh_exec_libs() {
    let s = read();
    assert!(
        s.contains("lib/audio-presence-preflight.sh"),
        "#901: rig-mode.sh must source scripts/lib/audio-presence-preflight.sh (reuses its \
         tier functions for the measurement-audio-arrival check)"
    );
    assert!(
        s.contains("lib/win-ssh-exec.sh"),
        "#901: rig-mode.sh must source scripts/lib/win-ssh-exec.sh (win_ssh_run, for the probe \
         recording's ffmpeg volumedetect on stream)"
    );
}

// --- new functions defined + called from do_test() ----------------------------------------------

#[test]
fn do_test_asserts_and_sets_stream_program_to_phase2_probe() {
    let s = read();
    assert!(
        s.contains("verify_stream_program_phase2"),
        "#901 gap 2: rig-mode.sh must define + call verify_stream_program_phase2"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("verify_stream_program_phase2"),
        "#901 gap 2: do_test must call verify_stream_program_phase2 (was: a printed hint only)"
    );
    // The old prose-only hint must be GONE — this is now enforced, not recommended.
    assert!(
        !s.contains("NEXT: confirm the PHASE2-PROBE scene"),
        "#901 gap 2: the old unenforced hint must be removed now that it is actually asserted"
    );
    // The function itself must use obs_phase2.py's `switch` action (SetCurrentProgramScene + its
    // #312 non-black self-check) against STREAM_IP + the PHASE2-PROBE scene.
    let def = s
        .find("verify_stream_program_phase2() {")
        .expect("verify_stream_program_phase2 must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(fn_body.contains("obs_phase2.py"));
    assert!(fn_body.contains("switch"));
    assert!(fn_body.contains("STREAM_IP"));
    assert!(fn_body.contains("PHASE2-PROBE"));
}

#[test]
fn do_test_resolves_and_burns_the_actually_rendered_inputs() {
    let s = read();
    assert!(
        s.contains("resolve_and_burn_rendered_inputs"),
        "#901 gap 3: rig-mode.sh must define + call resolve_and_burn_rendered_inputs"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("resolve_and_burn_rendered_inputs"),
        "#901 gap 3: do_test must call resolve_and_burn_rendered_inputs"
    );
    let def = s
        .find("resolve_and_burn_rendered_inputs() {")
        .expect("resolve_and_burn_rendered_inputs must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(
        fn_body.contains("program-rendered-input"),
        "#901: resolve_and_burn_rendered_inputs must call the program-rendered-input subcommand"
    );
    assert!(
        fn_body.contains("obs_burn_filter.py") && fn_body.contains("add"),
        "#901: resolve_and_burn_rendered_inputs must be able to ALSO burn the resolved input"
    );
    // ADDITIVE ONLY: the existing fixed-target toggle_burn() call site (called earlier in
    // do_test, unchanged) must still be present and untouched.
    assert!(
        do_test_body(&s).contains("toggle_burn test"),
        "#901: the existing fixed-default burn call must remain -- this is EXTRA coverage, \
         never a replacement"
    );
}

#[test]
fn do_test_proves_the_program_is_optically_nonblack() {
    let s = read();
    assert!(
        s.contains("verify_optical_qr_visible"),
        "#901 gap 1: rig-mode.sh must define + call verify_optical_qr_visible"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("verify_optical_qr_visible"),
        "#901 gap 1: do_test must call verify_optical_qr_visible"
    );
    let def = s
        .find("verify_optical_qr_visible() {")
        .expect("verify_optical_qr_visible must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(
        fn_body.contains("assert-program-nonblack"),
        "#901: verify_optical_qr_visible must call the assert-program-nonblack subcommand \
         (process-alive is not QR-on-screen)"
    );
    assert!(fn_body.contains("STRIH_IP"));
}

#[test]
fn do_test_checks_mbc_dante_transport() {
    let s = read();
    assert!(
        s.contains("verify_mbc_dante_transport"),
        "#901 original item 2: rig-mode.sh must define + call verify_mbc_dante_transport"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("verify_mbc_dante_transport"),
        "#901 original item 2: do_test must call verify_mbc_dante_transport"
    );
    let def = s
        .find("verify_mbc_dante_transport() {")
        .expect("verify_mbc_dante_transport must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(fn_body.contains("mbc-input-check"));
    assert!(fn_body.contains("MBC_INPUT_NAME"));
    assert!(fn_body.contains("STREAM_IP"));
}

#[test]
fn do_test_checks_measurement_audio_arrives_end_to_end() {
    let s = read();
    assert!(
        s.contains("verify_measurement_audio_arrives"),
        "#901 original item 1 (the headline fix): rig-mode.sh must define + call \
         verify_measurement_audio_arrives"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("verify_measurement_audio_arrives"),
        "#901: do_test must call verify_measurement_audio_arrives"
    );
    let def = s
        .find("verify_measurement_audio_arrives() {")
        .expect("verify_measurement_audio_arrives must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    // Must use the NEW three-tier, non-blocking classification -- never the STRICT single-
    // threshold audio_preflight_is_silent (that stays reserved for recording-e2e.sh's real gate).
    assert!(
        fn_body.contains("audio_preflight_tier"),
        "#901: verify_measurement_audio_arrives must use audio_preflight_tier (dead/quiet/audible)"
    );
    assert!(fn_body.contains("audio_preflight_dead_message"));
    assert!(fn_body.contains("audio_preflight_quiet_message"));
    assert!(
        !fn_body.contains("audio_preflight_is_silent"),
        "#901: rig-mode.sh's own softer check must not reuse the STRICT single-threshold \
         classifier -- that would wedge every TEST restore on the known issue-976 degradation"
    );
    // Must actually make a probe recording on stream + read it back with ffmpeg volumedetect —
    // the SAME proven mechanism recording-e2e.sh's real gate uses, applied here on a shorter timer.
    assert!(fn_body.contains("obs_phase2.py") && fn_body.contains("record"));
    assert!(fn_body.contains("win_ssh_run"));
    assert!(fn_body.contains("audio_preflight_volumedetect_ps"));
    assert!(fn_body.contains("audio_preflight_parse_max_db"));
    // Hard fail on a genuinely DEAD chain, never a bare warning.
    assert!(
        fn_body.contains("\"dead\"") && fn_body.contains("exit 1"),
        "#901: a DEAD verdict must exit non-zero (hard fail) -- this is the original 2026-07-31 \
         'sound card off, TEST mode still PASSED' incident this ticket exists to close"
    );
    // The gate must be individually opt-outable, mirroring recording-e2e.sh's own
    // AUDIO_PREFLIGHT_ENABLE convention for the same class of preflight.
    assert!(fn_body.contains("RIG_MODE_AUDIO_CHAIN_ENABLE"));
}

// --- NDI mapping report-only full-table sweep (gap 4) --------------------------------------------

#[test]
fn enforce_strih_ndi_mapping_also_runs_a_report_only_full_table_sweep() {
    let s = read();
    let def = s
        .find("enforce_strih_ndi_mapping() {")
        .expect("enforce_strih_ndi_mapping must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    // The ORIGINAL active-set enforcement call must be untouched.
    assert!(
        fn_body.contains(r#"--active "$CAMERA_ACTIVE_SET""#),
        "#399/#827: the existing active-set enforcement call must remain unchanged"
    );
    // #901 gap 4: a SECOND call, --verify-only, across the FULL 7-camera table.
    assert!(
        fn_body.contains("--verify-only"),
        "#901 gap 4: enforce_strih_ndi_mapping must ALSO run a --verify-only sweep"
    );
    assert!(
        fn_body.contains("cam1 cam2 cam3 cam4 cam5 cam6 cam7"),
        "#901 gap 4: the report-only sweep must cover ALL 7 known cameras, not just the active \
         subset -- live evidence: an INACTIVE camera's mangled NDI pin ('NDI cam5' -> 'h') went \
         uncaught because only the active subset was ever checked"
    );
    // Report-only: the sweep's own non-zero exit must never be allowed to fail the function's
    // own returned rc (would wedge TEST mode over cosmetic drift on unused/offline hardware).
    let verify_pos = fn_body
        .find("--verify-only")
        .expect("--verify-only present (checked above)");
    let after_verify = &fn_body[verify_pos..];
    let next_stmt_end = after_verify.find('\n').unwrap_or(after_verify.len());
    // Scan forward a little further than just the same line, since the call is likely a
    // multi-line python3 invocation piped through sed.
    let window_end = (verify_pos + 400).min(fn_body.len());
    let window = &fn_body[verify_pos..window_end];
    let _ = next_stmt_end; // (kept for clarity; the real assertion is on `window` below)
    assert!(
        window.contains("|| true"),
        "#901 gap 4: the full-table verify-only sweep must never propagate its exit code -- \
         report only, must not hard-fail rig-mode.sh test on an inactive camera's drift. \
         window was: {window:?}"
    );
}
