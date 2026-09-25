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
//! Activating the manifest re-arms MORE than obs.dll on each box: drift-guard also makes the
//! `genlock_capability` check gate-blocking (an obs.dll-only FAST manifest leaves distroav SKIPPED,
//! not UNKNOWN — by design), so the enforce surface is obs.dll bytes + genlock_capability. The live
//! precondition covered both keys on both boxes, so the flip is GREEN today.
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
/// `manifest_autosource_state_has_key` reporting guard — the ENFORCE flip runs it unconditionally so
/// a box that stops reporting obs_dll_sha256 becomes a gate-blocking UNKNOWN, not a silent skip
/// (#1100). Anchored on the guard's OWN usage shape (the helper keyed on the per-box `$VERSION_*_STATE`
/// files) rather than the bare helper name, so it stays a precise "the opt-in guard is gone" assertion
/// and does not forbid a future, legitimately-opt-in facet from reusing the (kept + unit-tested)
/// `manifest_autosource_state_has_key` LIB function elsewhere in the script.
#[test]
fn windows_manifest_autosource_is_enforced_not_opt_in_gated() {
    let s = recording_e2e();
    assert!(
        !s.contains("manifest_autosource_state_has_key \"$VERSION_STRIH_STATE\"")
            && !s.contains("manifest_autosource_state_has_key \"$VERSION_STREAM_STATE\""),
        "#1100: the Windows FAST-manifest auto-source must NOT gate on the #1082 opt-in reporting \
         guard (manifest_autosource_state_has_key against the per-box $VERSION_STRIH_STATE / \
         $VERSION_STREAM_STATE files) — the ENFORCE flip runs it unconditionally so an un-reporting \
         box flips to a gate-blocking UNKNOWN, not a silent skip."
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

/// #1346: the two Windows CI builds are not byte-reproducible, so the FAST manifest alone refuses a
/// correct FULL-bundle deploy of the same build. recording-e2e.sh must ALSO fetch the FULL bundle's
/// manifest (via the lib helper, keyed on the same strih marker sha) -- but only when the operator
/// did not pin VERSION_GATE_MANIFEST (an operator pin is never widened) -- and pass it to BOTH gate
/// invocations as the conditional `--alt-manifest` (omitted when the fetch failed, so the FAST
/// manifest is then judged exactly as before).
#[test]
fn windows_full_bundle_manifest_is_fetched_and_passed_as_the_alternate_1346() {
    let s = recording_e2e();
    assert!(
        s.contains(
            "[ -z \"${VERSION_GATE_MANIFEST:-}\" ] && AUTO_WIN_ALT_MANIFEST=\"$(manifest_autosource_fetch_win_full \
             \"$VERSION_GATE_REPO\""
        ),
        "#1346: recording-e2e.sh must fetch the FULL bundle manifest through \
         manifest_autosource_fetch_win_full, skipped when VERSION_GATE_MANIFEST is pinned"
    );
    assert!(
        s.contains("\"$(genlock_build_sha_state_read \"$VERSION_STRIH_STATE\")\" \"$OUTDIR/win-full-manifest.json\""),
        "#1346: the FULL manifest must be keyed on the same strih marker sha as the FAST one"
    );
    assert_eq!(
        s.matches("${AUTO_WIN_ALT_MANIFEST:+--alt-manifest \"$AUTO_WIN_ALT_MANIFEST\"}")
            .count(),
        2,
        "#1346: both version-integrity-gate.sh invocations (imag-acked + normal) must pass the \
         alternate conditionally"
    );
}

/// #1346 main ruling: after both fetches, recording-e2e.sh resolves the pair through the lib's
/// win_manifest_pair_resolve (full manifest alone only for a full-only build; a fast fetch outage
/// omits the byte pin) -- once, reading both variables back, before the gate invocations.
#[test]
fn windows_manifest_pair_is_resolved_by_the_main_ruling_1346() {
    let s = recording_e2e();
    let call = "{ IFS= read -r AUTO_WIN_MANIFEST; IFS= read -r AUTO_WIN_ALT_MANIFEST; } < <(win_manifest_pair_resolve \"$VERSION_GATE_REPO\"";
    assert_eq!(
        s.matches(call).count(),
        1,
        "#1346: recording-e2e.sh must resolve the fast/full pair exactly once via win_manifest_pair_resolve"
    );
    let at = s.find(call).unwrap();
    let full_fetch = s
        .find("AUTO_WIN_ALT_MANIFEST=\"$(manifest_autosource_fetch_win_full")
        .expect("the full-manifest fetch must exist");
    let gate = s[at..]
        .find("${AUTO_WIN_ALT_MANIFEST:+--alt-manifest")
        .map(|p| at + p)
        .expect("the gate invocation must follow the resolution");
    assert!(
        full_fetch < at && at < gate,
        "#1346: resolve after the full fetch and before the gate invocations"
    );
}
