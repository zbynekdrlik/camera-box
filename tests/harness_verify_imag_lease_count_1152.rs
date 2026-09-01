//! issue 1152 M4 lease-tolerance slice — `scripts/verify-imag.sh` check (o)
//! (`imag_projector_counts_ok`) is the second of the two STILL-OPEN lease-blind gates named in
//! `.claude/rules/obs-drm-output.md`'s "Known boundary" section: it hard-requires exactly 1
//! Multiview + 1 Program X projector window, with no awareness of the in-OBS DRM-lease HDMI
//! output where Program is drawn by the vendored OBS DRM output directly onto the leased CRTC —
//! never an X window at all.
//!
//! Two things are locked here (same sourcing convention as `tests/verify_imag_pure_functions.rs`,
//! which already proves `imag_projector_counts_ok`'s own dormant 1+1 contract is UNCHANGED):
//!   1. The two NEW pure functions `imag_projector_counts_ok_lease` /
//!      `imag_projector_counts_ok_for_mode`.
//!   2. check (o)'s live flow derives `LEASE_CONNECTOR` via the SAME shared classifier
//!      (`imag_scenes.drm_output_lease_connector` / `_drm_output_config_text`) recording-e2e.sh's
//!      own M4 lease-tolerance slice uses, and consults `imag_projector_counts_ok_for_mode` at
//!      BOTH the before-restart and after-restart-poll call sites (never the raw
//!      `imag_projector_counts_ok` any more at either site).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/verify-imag.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the real script (its `BASH_SOURCE != $0` guard skips the live SSH/WS flow) and run
/// `body` against its pure functions — same harness shape as
/// `tests/verify_imag_pure_functions.rs::run_sourced`.
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// pure functions: imag_projector_counts_ok_lease / imag_projector_counts_ok_for_mode
// ---------------------------------------------------------------------------------------------

#[test]
fn dormant_contract_of_imag_projector_counts_ok_is_unchanged() {
    // Re-asserts the EXACT same cases tests/verify_imag_pure_functions.rs already locks, proving
    // this slice's new functions COMPOSE over the existing one rather than replace it.
    let cases = [
        ("1", "1", "YES"),
        ("0", "1", "NO"),
        ("2", "1", "NO"),
        ("1", "0", "NO"),
    ];
    for (mv, pgm, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_projector_counts_ok "{mv}" "{pgm}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_projector_counts_ok(mv={mv}, pgm={pgm})"
        );
    }
}

#[test]
fn imag_projector_counts_ok_lease_requires_one_multiview_zero_program() {
    let cases = [
        ("1", "0", "YES"),
        ("1", "1", "NO"),
        ("0", "0", "NO"),
        ("0", "1", "NO"),
    ];
    for (mv, pgm, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_projector_counts_ok_lease "{mv}" "{pgm}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_projector_counts_ok_lease(mv={mv}, pgm={pgm})"
        );
    }
}

#[test]
fn imag_projector_counts_ok_for_mode_dispatches_on_connector_presence() {
    let cases = [
        ("", "1", "1", "YES"),       // dormant, 1+1 -> ok
        ("", "1", "0", "NO"),        // dormant, 1+0 -> NOT ok (that's the lease shape)
        ("HDMI-1", "1", "0", "YES"), // lease, 1+0 -> ok
        ("HDMI-1", "1", "1", "NO"),  // lease, 1+1 -> NOT ok (stray Program window)
        ("DP-1", "1", "0", "YES"),   // any non-empty connector name selects the lease contract
    ];
    for (connector, mv, pgm, want) in cases {
        let (code, out, err) = run_sourced(&format!(
            r#"if imag_projector_counts_ok_for_mode "{connector}" "{mv}" "{pgm}"; then echo YES; else echo NO; fi"#
        ));
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            out.trim(),
            want,
            "imag_projector_counts_ok_for_mode(connector={connector:?}, mv={mv}, pgm={pgm})"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// live flow: check (o) derives LEASE_CONNECTOR via the shared classifier and consults
// imag_projector_counts_ok_for_mode at BOTH call sites.
// ---------------------------------------------------------------------------------------------

#[test]
fn check_o_derives_lease_connector_via_the_shared_imag_scenes_classifier() {
    let s = read("scripts/verify-imag.sh");
    assert!(
        s.contains("imag_scenes.drm_output_lease_connector")
            && s.contains("imag_scenes._drm_output_config_text"),
        "verify-imag.sh check (o) must derive the lease connector via imag_scenes' shared \
         classifier pair -- the SAME grammar obs_phase2.py::_drm_lease_connector_for_host / \
         imag-obs-start.sh / recording-e2e.sh's own M4 lease-tolerance slice already use, never \
         a second, divergent config reader (.claude/rules/obs-drm-output.md)"
    );
}

#[test]
fn check_o_calls_the_mode_aware_verdict_at_both_before_and_after_restart_sites() {
    let s = read("scripts/verify-imag.sh");
    // A CALL (a quote right after the name) -- excludes the function's own comment/definition
    // lines, which also contain the bare name.
    let n = s.matches("imag_projector_counts_ok_for_mode \"").count();
    assert_eq!(
        n, 2,
        "verify-imag.sh check (o) must call imag_projector_counts_ok_for_mode at EXACTLY the \
         before-restart count and the after-restart-poll count sites (never a raw \
         imag_projector_counts_ok call any more) -- found {n} call site(s)"
    );
}

#[test]
fn lease_connector_is_derived_before_the_890_restart_and_reused_for_both_reads() {
    // #884's own ordering rule: state that check (o) reads must be captured before its restart
    // replaces the tracked obs process. LEASE_CONNECTOR reads a STATIC config file (unaffected
    // by an OBS restart either way), but it must still sit before the restart call so the SAME
    // read is reused for both the before- and after-restart counts (never re-derived, never
    // read mid-restart).
    let s = read("scripts/verify-imag.sh");
    let lease_idx = s
        .find("LEASE_CONNECTOR=\"$(python3")
        .expect("LEASE_CONNECTOR must be derived via a python3 one-liner");
    let restart_idx = s
        .find("ssh_box_timeout \"$IMAG_OBS_RESTART_TIMEOUT\"")
        .expect("the bounded service-restart call must exist (#890)");
    // Search for a CALL (a quote right after the name), not the function's own DEFINITION
    // (`imag_projector_counts_ok_for_mode() {`, which sits much earlier, right after the
    // sibling imag_projector_counts_ok/imag_projector_counts_ok_lease definitions).
    let first_use_idx = s
        .find("imag_projector_counts_ok_for_mode \"")
        .expect("imag_projector_counts_ok_for_mode must be called");
    assert!(
        lease_idx < first_use_idx,
        "LEASE_CONNECTOR must be derived before its first use"
    );
    assert!(
        lease_idx < restart_idx,
        "LEASE_CONNECTOR must be derived before check (o)'s OBS restart (#884 ordering)"
    );
}

#[test]
fn raw_imag_projector_counts_ok_call_no_longer_appears_in_check_o() {
    // The two former direct call sites must now go through the mode-aware wrapper -- the bare
    // `imag_projector_counts_ok "..."` invocation (as opposed to the `_for_mode`/`_lease`
    // variants, or its own function DEFINITION) must be gone from the live flow.
    let s = read("scripts/verify-imag.sh");
    assert!(
        !s.contains("if imag_projector_counts_ok \"${MV_COUNT:-0}\""),
        "check (o)'s before-restart count must no longer call the raw imag_projector_counts_ok \
         directly -- it must go through imag_projector_counts_ok_for_mode"
    );
    assert!(
        !s.contains("if imag_projector_counts_ok \"${MV_COUNT2:-0}\""),
        "check (o)'s after-restart-poll count must no longer call the raw \
         imag_projector_counts_ok directly -- it must go through imag_projector_counts_ok_for_mode"
    );
}

#[test]
fn raw_sshpass_primitive_stays_exactly_one_despite_the_new_python3_lease_read() {
    // Locks the SAME invariant tests/verify_imag_pure_functions.rs's own
    // verify_imag_raw_ssh_primitive_is_bounded_and_singular_1058 asserts -- this slice's new
    // LEASE_CONNECTOR read must NOT introduce a second raw sshpass primitive into
    // verify-imag.sh's own body (its bounded ssh reads stay funneled through ssh_box/
    // ssh_box_timeout; the lease read's OWN remote ssh call, if any, lives inside
    // imag_scenes.py, a SEPARATE file, never duplicated here).
    let s = read("scripts/verify-imag.sh");
    let n = s.matches(r#"sshpass -p "$IMAG_PW" ssh"#).count();
    assert_eq!(
        n, 1,
        "verify-imag.sh must still have EXACTLY ONE raw sshpass primitive (#1058) -- found {n}"
    );
}
