//! #985 — `rig-mode.sh test` must not PARK the rig on the desynced `PHASE2-PROBE` scene.
//!
//! `verify_stream_program_phase2()` (issue 901 gap 2) asserts+sets stream's PROGRAM to
//! `PHASE2-PROBE` to prove the probe path is alive, but nothing ever switches it back — TEST mode
//! is the rig's STANDING state, so the rig now parks indefinitely on a scene whose backing input
//! (`phase2-probe-src`) runs OBS's build-default 3ms `genlock_latency_ms_src` while the certified
//! prod input (`NDI 2ME PGM`, scene `PRO`) runs a ~948ms calibrated A/V-align hold — a
//! ~945ms A/V-desync-by-construction left on the parked operator monitor.
//!
//! These are STATIC-ANCHOR tests only (the repo's established pattern for rig-mode.sh — see the
//! project CLAUDE.md GOTCHA on the shared textual-collision risk): they assert the new
//! constant/function exists and is CALLED from `do_test()`, never execute a live OBS-WS call.
//!
//! RED before this work exists (the constant/function are absent, every test fails); GREEN after.

use std::fs;
use std::path::PathBuf;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/rig-mode.sh")
}

fn read() -> String {
    fs::read_to_string(script()).expect("read rig-mode.sh")
}

/// The text between the literal `do_test()` and `do_event()` markers — the same slicing
/// convention `tests/rig_mode.rs` / `tests/harness_rig_mode_chain_verify_901.rs` already use.
fn do_test_body(s: &str) -> &str {
    s.split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("")
}

#[test]
fn defines_stream_prog_scene_matching_recording_e2e_convention() {
    let s = read();
    assert!(
        s.contains(r#"STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-PRO}""#),
        "#985: rig-mode.sh must define STREAM_PROG_SCENE (default 'PRO' -- the SAME convention \
         scripts/recording-e2e.sh:1291 already uses for this exact box/scene)"
    );
}

#[test]
fn do_test_restores_stream_program_to_pro_after_proving_phase2_probe_alive() {
    let s = read();
    assert!(
        s.contains("restore_stream_program_pro"),
        "#985: rig-mode.sh must define + call restore_stream_program_pro"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("restore_stream_program_pro"),
        "#985: do_test must call restore_stream_program_pro (the rig must not stay parked on \
         PHASE2-PROBE)"
    );
    // Ordering: the restore must happen AFTER verify_stream_program_phase2 proves the probe path
    // alive (issue 901 gap 2) -- restoring before that would prove nothing.
    let probe_pos = body
        .find("verify_stream_program_phase2")
        .expect("#901: verify_stream_program_phase2 must still be called from do_test");
    let restore_pos = body
        .find("restore_stream_program_pro")
        .expect("#985: restore_stream_program_pro must be called from do_test");
    assert!(
        restore_pos > probe_pos,
        "#985: restore_stream_program_pro must run AFTER verify_stream_program_phase2 in \
         do_test, not before"
    );

    // The function itself must use obs_phase2.py's `switch` action (SetCurrentProgramScene +
    // its #312 non-black self-check) against STREAM_IP + the STREAM_PROG_SCENE constant -- the
    // SAME mechanism verify_stream_program_phase2 already uses, no new OBS plumbing.
    let def = s
        .find("restore_stream_program_pro() {")
        .expect("restore_stream_program_pro must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(
        fn_body.contains("obs_phase2.py"),
        "restore_stream_program_pro must call obs_phase2.py: {fn_body}"
    );
    assert!(
        fn_body.contains("switch"),
        "restore_stream_program_pro must use the `switch` action (SetCurrentProgramScene + \
         non-black self-check), not a new mechanism: {fn_body}"
    );
    assert!(
        fn_body.contains("STREAM_IP"),
        "restore_stream_program_pro must target STREAM_IP: {fn_body}"
    );
    assert!(
        fn_body.contains("STREAM_PROG_SCENE"),
        "restore_stream_program_pro must switch to $STREAM_PROG_SCENE (default PRO), not a \
         hardcoded literal: {fn_body}"
    );
}
