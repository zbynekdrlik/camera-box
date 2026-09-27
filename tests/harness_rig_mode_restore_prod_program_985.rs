//! #985 — `rig-mode.sh test` must not PARK the rig on a desynced measurement-only scene.
//!
//! Originally `verify_stream_program_phase2()` (issue 901 gap 2) set stream's PROGRAM to the
//! probe scene `PHASE2-PROBE` and nothing switched it back — TEST mode is the rig's STANDING
//! state, so the rig parked on a scene whose probe input ran OBS's build-default 3ms
//! `genlock_latency_ms_src` while the certified prod input (`NDI 2ME PGM`) runs a ~948ms
//! calibrated A/V-align hold. Since issue 1380 gap 2 proves the stream program on the stream
//! DEVELOPMENT scene (the production scene nested inside it, the same certified input), and
//! `park_stream_program_dev` re-asserts that scene as the last OBS step of `do_test`.
//!
//! These are STATIC-ANCHOR tests only (the repo's established pattern for rig-mode.sh — see the
//! project CLAUDE.md GOTCHA on the shared textual-collision risk): they assert the new
//! constant/function exists and is CALLED from `do_test()`, never execute a live OBS-WS call.

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
    // issue 1380: the parked scene is the stream DEVELOPMENT scene (the production scene `PRO`
    // nested inside it) -- the SAME lib default scripts/recording-e2e.sh uses for this box.
    assert!(
        s.contains(r#"STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}""#),
        "#985/issue 1380: rig-mode.sh must define STREAM_PROG_SCENE defaulting to the stream \
         development scene (the SAME convention scripts/recording-e2e.sh uses for this box)"
    );
}

#[test]
fn do_test_parks_stream_program_on_the_dev_scene_after_proving_it_alive() {
    let s = read();
    assert!(
        s.contains("park_stream_program_dev"),
        "#985: rig-mode.sh must define + call park_stream_program_dev"
    );
    let body = do_test_body(&s);
    assert!(
        body.contains("park_stream_program_dev"),
        "#985: do_test must call park_stream_program_dev (the rig must end parked on \
         the development scene)"
    );
    // Ordering: the park must happen AFTER verify_stream_program_dev proves the program path
    // alive (issue 901 gap 2, since issue 1380 on the development scene) -- restoring before that would prove nothing.
    let probe_pos = body
        .find("verify_stream_program_dev")
        .expect("#901 gap 2: verify_stream_program_dev must still be called from do_test");
    let restore_pos = body
        .find("park_stream_program_dev")
        .expect("#985: park_stream_program_dev must be called from do_test");
    assert!(
        restore_pos > probe_pos,
        "#985: park_stream_program_dev must run AFTER verify_stream_program_dev in \
         do_test, not before"
    );

    // The function itself must use obs_phase2.py's `switch` action (SetCurrentProgramScene +
    // its #312 non-black self-check) against STREAM_IP + the STREAM_PROG_SCENE constant -- the
    // SAME mechanism verify_stream_program_dev already uses, no new OBS plumbing.
    let def = s
        .find("park_stream_program_dev() {")
        .expect("park_stream_program_dev must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let fn_body = &s[def..body_end];
    assert!(
        fn_body.contains("obs_phase2.py"),
        "park_stream_program_dev must call obs_phase2.py: {fn_body}"
    );
    assert!(
        fn_body.contains("switch"),
        "park_stream_program_dev must use the `switch` action (SetCurrentProgramScene + \
         non-black self-check), not a new mechanism: {fn_body}"
    );
    assert!(
        fn_body.contains("STREAM_IP"),
        "park_stream_program_dev must target STREAM_IP: {fn_body}"
    );
    assert!(
        fn_body.contains("STREAM_PROG_SCENE"),
        "park_stream_program_dev must switch to $STREAM_PROG_SCENE (default: the development scene), not a \
         hardcoded literal: {fn_body}"
    );
}
