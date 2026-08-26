//! #1204 — the fail-closed cross-check helper `scripts/lib/imag-burn-verify.sh`.
//!
//! recording-e2e.sh [4a/8] reads the input imag ACTUALLY renders in program (obs_phase2.py
//! program-rendered-input) and, via these PURE functions, asserts it equals the burn target
//! (IMAG_PROG_SOURCE). A mismatch (or an unreadable/empty rendered input) FAILS the run LOUD BEFORE
//! recording, so a burn can never again land on a non-program input (run 32908274448: imag recorded
//! zero 911003 anchors because the burn was on 'NDI CAM1' while program rendered 'NDI CAM3').
//!
//! Tier-0: pure shell functions, no ssh/OBS/rig — exercised by sourcing the lib in a bash subshell,
//! the SAME pattern the sibling `harness_imag_scene_route_682.rs` uses for imag_scene_for_camera.

use std::process::Command;

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/imag-burn-verify.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Run `. lib; <expr>` under a strict bash and return (success, stdout, stderr).
fn run(expr: &str) -> (bool, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("set -uo pipefail; . '{}'; {}", lib_path(), expr))
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn matches_when_rendered_equals_target() {
    let (ok, _, err) = run("imag_burn_target_matches_program 'NDI CAM3' 'NDI CAM3'");
    assert!(
        ok,
        "#1204: identical rendered/target must MATCH (rc 0). stderr={err}"
    );
}

#[test]
fn mismatch_when_rendered_differs_from_target() {
    let (ok, _, _) = run("imag_burn_target_matches_program 'NDI CAM3' 'NDI CAM1'");
    assert!(
        !ok,
        "#1204: rendered 'NDI CAM3' vs target 'NDI CAM1' must be a MISMATCH (rc != 0) -- the exact \
         run 32908274448 split"
    );
}

#[test]
fn empty_rendered_is_a_mismatch_fail_closed() {
    // An unreadable program-rendered-input (obs down / no enabled scene item) yields an empty
    // string -- it must FAIL CLOSED (mismatch), never silently "match".
    let (ok, _, _) = run("imag_burn_target_matches_program '' 'NDI CAM3'");
    assert!(
        !ok,
        "#1204: an EMPTY rendered input must be a mismatch (fail-closed), never a silent match"
    );
}

#[test]
fn empty_target_is_a_mismatch_fail_closed() {
    let (ok, _, _) = run("imag_burn_target_matches_program 'NDI CAM3' ''");
    assert!(
        !ok,
        "#1204: an EMPTY burn target must be a mismatch (fail-closed)"
    );
}

#[test]
fn mismatch_message_names_the_target_and_rendered_and_ticket() {
    let (_, out, _) = run("imag_burn_mismatch_message 'NDI CAM3' 'NDI CAM1' 'Cam 3'");
    let m = out.trim();
    assert!(
        m.contains("1204"),
        "#1204: message must cite the ticket: {m}"
    );
    assert!(
        m.contains("NDI CAM1"),
        "#1204: message must name the (wrong) burn target: {m}"
    );
    assert!(
        m.contains("NDI CAM3"),
        "#1204: message must name what imag actually renders: {m}"
    );
    let low = m.to_lowercase();
    assert!(
        low.contains("911003") || low.contains("anchor"),
        "#1204: message must explain the zero-anchor consequence: {m}"
    );
}

#[test]
fn mismatch_message_handles_the_empty_rendered_case_distinctly() {
    // Empty rendered = "could not read", a distinct, clearer diagnostic than "wrong input".
    let (_, out, _) = run("imag_burn_mismatch_message '' 'NDI CAM3' 'Cam 3'");
    let low = out.to_lowercase();
    assert!(
        low.contains("could not read")
            || low.contains("unverifiable")
            || low.contains("no ") && low.contains("render"),
        "#1204: empty-rendered message must say the rendered input could not be read: {out}"
    );
    assert!(
        out.contains("1204"),
        "#1204: message must cite the ticket: {out}"
    );
}

/// Source-only lib discipline (mirrors imag-scene-route.sh): sourcing it must run NOTHING and leak
/// no `set` state -- it only defines functions.
#[test]
fn lib_is_source_only_no_top_level_output() {
    let (ok, out, err) = run("true");
    assert!(
        ok,
        "#1204: sourcing imag-burn-verify.sh must succeed. stderr={err}"
    );
    assert!(
        out.trim().is_empty(),
        "#1204: sourcing the lib must print nothing (source-only, defines functions). stdout={out:?}"
    );
}
