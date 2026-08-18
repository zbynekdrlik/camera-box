//! #1100 — the [0/8] Windows obs.dll byte-parity facet is ENFORCED fleet-wide.
//!
//! #1082 landed the Windows FAST-manifest auto-source OPT-IN: recording-e2e.sh only auto-sourced
//! the FAST BUNDLE_MANIFEST when BOTH strih AND stream already reported obs_dll_sha256 +
//! genlock_capability (via `manifest_autosource_state_has_key`), so a box that did NOT report its
//! deployed obs.dll sha was a SILENT skip, never a refuse. That was the correct #756-shape opt-in
//! while the on-box byte gather (bundle-state-server) was not yet deployed fleet-wide.
//!
//! Precondition 1 for the ENFORCE flip — strih+stream actually serving obs_dll_sha256/
//! distroav_dll_sha256 on :8899 — is a LIVE-Windows property no worktree worker can assume; it was
//! verified live before this flip (both boxes serve the keys at the fleet marker SHA, and the CI
//! FAST manifest at that SHA carries the same obs.dll). The ENFORCE (#1100, the #758-shape second
//! step of the #756-shape opt-in) removes that guard: the auto-source runs UNCONDITIONALLY, so a box
//! that stops reporting its bytes flips to a gate-blocking UNKNOWN — every box is REQUIRED to report.
//! Same 756->758 second step #1067 applied to `port4455_identity`.
//!
//! Static-text guard on scripts/recording-e2e.sh (the same model tests/harness_recording_e2e_*.rs
//! use): it runs on every push, on any host, with no rig. The DEFINITIVE proof is a green Full-path
//! E2E [0/8] log showing obs.dll byte parity OK on both Windows boxes.

use std::fs;

fn recording_e2e() -> String {
    let p = format!("{}/scripts/recording-e2e.sh", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// The Windows FAST-manifest auto-source must NO LONGER gate on the #1082 opt-in
/// `manifest_autosource_state_has_key` guard — the ENFORCE flip runs it unconditionally so a box
/// that stops reporting obs_dll_sha256 becomes a gate-blocking UNKNOWN, not a silent skip (#1100).
/// (The `manifest_autosource_state_has_key` LIB function is kept + unit-tested; only its USE as the
/// recording-e2e.sh opt-in guard is removed — so its total absence from THIS script is the anchor.)
#[test]
fn windows_manifest_autosource_is_enforced_not_opt_in_gated() {
    let s = recording_e2e();
    assert!(
        !s.contains("manifest_autosource_state_has_key"),
        "#1100: the Windows FAST-manifest auto-source must NOT gate on \
         manifest_autosource_state_has_key (the #1082 opt-in guard) — the ENFORCE flip runs it \
         unconditionally so an un-reporting box flips to a gate-blocking UNKNOWN, not a silent skip."
    );
}

/// The auto-source itself must STILL run (unconditionally, when VERSION_GATE_MANIFEST is unset): the
/// enforce REMOVES the opt-in guard, it does not remove the byte-parity auto-source — the FAST
/// BUNDLE_MANIFEST must still be fetched so obs.dll byte parity is asserted on every box, every real
/// run (#1100). Regression guard: the enforce must not accidentally delete the fetch itself.
#[test]
fn windows_manifest_autosource_still_fetches_the_fast_manifest() {
    let s = recording_e2e();
    assert!(
        s.contains(
            "manifest_autosource_fetch \"$VERSION_GATE_REPO\" windows-genlock-fast.yml \
             obs-genlock-fast-dll"
        ),
        "#1100: recording-e2e.sh must still auto-source the Windows FAST BUNDLE_MANIFEST \
         (manifest_autosource_fetch ... windows-genlock-fast.yml obs-genlock-fast-dll) — the enforce \
         removes only the opt-in guard, not the byte-parity auto-source itself."
    );
}
