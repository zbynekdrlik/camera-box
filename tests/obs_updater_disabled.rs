//! Patch-presence guard for #43 — the genlocked OBS build ships with its built-in
//! updater permanently disabled.
//!
//! Background: the stock OBS update dialog previously let anyone overwrite the custom
//! genlocked OBS on the production boxes (strih/stream) with a non-genlocked stock
//! release. Our vendored build (#41) bakes the OBS-native `--disable-updater` mechanism
//! to default-ON by flipping the `opt_disable_updater` global's initializer to `true`,
//! so no timed check, menu action, or whatsnew prompt can ever run — the only update
//! path is the camera-box controlled release-bump flow.
//!
//! This is a SOURCE-level guard, not a runtime test: the genlock patch lives in the
//! vendored C++ (`git log -- vendor/` is the patch series, per vendor/README.md). The
//! risk this test defends against is a future `git subtree pull --squash` upstream
//! release-bump (the `/update-av-stack` flow, #44) silently re-importing upstream's
//! `opt_disable_updater = false` default and re-enabling the updater on the genlocked
//! boxes. If that happens, CI fails loudly here — exactly the "report conflicts loudly"
//! contract of the monorepo. Same vendored-source-assertion convention as
//! tests/av_stack_update.rs.

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_MAIN: &str = "vendor/obs-studio/frontend/obs-main.cpp";
const OBS_APP: &str = "vendor/obs-studio/frontend/OBSApp.cpp";
const OBS_UPDATER: &str = "vendor/obs-studio/frontend/widgets/OBSBasic_Updater.cpp";
const OBS_BASIC: &str = "vendor/obs-studio/frontend/widgets/OBSBasic.cpp";

#[test]
fn updater_global_defaults_to_disabled() {
    let src = squish(&vendor_file(OBS_MAIN));

    // The genlock patch: the `opt_disable_updater` definition defaults to `true`.
    assert!(
        src.contains("bool opt_disable_updater = true;"),
        "{OBS_MAIN}: genlock #43 patch missing — `opt_disable_updater` must default to \
         `true` so the built-in OBS updater is OFF in this vendored build. A `git subtree \
         pull` upstream bump likely reverted it; re-apply the genlock patch."
    );

    // And the upstream default (`= false`) must be gone — catches a silent revert.
    assert!(
        !src.contains("bool opt_disable_updater = false;"),
        "{OBS_MAIN}: upstream `opt_disable_updater = false` default is back — the genlock \
         #43 updater-disable patch was reverted by an upstream merge. Re-apply it."
    );
}

#[test]
fn updater_disabled_flag_gates_the_updater() {
    // The global only matters if `IsUpdaterDisabled()` still reads it. If an upstream
    // refactor renames/removes this wiring, our default-true patch would silently stop
    // disabling the updater — fail loudly so the patch is re-applied correctly.
    let app = squish(&vendor_file(OBS_APP));
    assert!(
        app.contains("bool OBSApp::IsUpdaterDisabled() { return opt_disable_updater; }"),
        "{OBS_APP}: `IsUpdaterDisabled()` no longer returns `opt_disable_updater` — the \
         #43 disable mechanism is broken. Re-verify how upstream gates the updater and \
         re-apply the genlock patch at the new chokepoint."
    );

    // Auto/timed update check must consult IsUpdaterDisabled() and bail when disabled.
    let updater = squish(&vendor_file(OBS_UPDATER));
    assert!(
        updater.contains("if (App()->IsUpdaterDisabled()) return;"),
        "{OBS_UPDATER}: TimedCheckForUpdates no longer early-returns on \
         IsUpdaterDisabled() — the auto-update chokepoint changed upstream; re-verify."
    );

    // Defense-in-depth: CheckForUpdates() (the single entry to the update dialog) must
    // itself no-op when disabled, so no caller can spawn an update prompt regardless of
    // the menu action's enabled state (e.g. the settings forceUpdateCheck path).
    assert!(
        updater.contains(
            "void OBSBasic::CheckForUpdates(bool manualUpdate) { if (App()->IsUpdaterDisabled()) return;"
        ),
        "{OBS_UPDATER}: CheckForUpdates() no longer force-returns when the updater is \
         disabled — the #43 defense-in-depth guard was reverted/refactored; re-apply it."
    );

    // The manual "Help -> Check For Updates" menu action must be disabled when the
    // updater is off, so it can't reach CheckForUpdates() directly.
    let basic = squish(&vendor_file(OBS_BASIC));
    assert!(
        basic.contains(
            "if (App()->IsUpdaterDisabled()) { ui->actionCheckForUpdates->setEnabled(false);"
        ),
        "{OBS_BASIC}: the Check-For-Updates menu action is no longer disabled when \
         IsUpdaterDisabled() is true — the manual-update chokepoint changed upstream; \
         re-verify."
    );
}
