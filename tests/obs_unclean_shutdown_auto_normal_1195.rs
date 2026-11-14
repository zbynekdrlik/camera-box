//! Patch-presence guard for #1195 — the genlocked OBS rig build NEVER stops on the
//! stock "Run in Safe Mode?" crash modal at launch.
//!
//! Background: upstream `OBSApp::checkForUncleanShutdown()` calls `handleUncleanShutdown()`
//! unconditionally after an unclean shutdown, which runs a BLOCKING `QMessageBox::exec()`
//! ("OBS Studio did not properly shut down. Run in Safe Mode?"). On an unattended broadcast
//! box (strih/stream) a crash + AHK/wrapper respawn lands OBS on that modal and it stays
//! dead until a human clicks it (~30 min of dead OBS — the owner's #1195 report). There is
//! no config key (`DisableSafeModePrompt` does not exist in this 32.x tree) or CLI flag
//! (`--disable-shutdown-check` is unparsed) to suppress it, so the fix is at the vendored
//! source: remove the modal machinery and auto-select a NORMAL launch (safe_mode left
//! untouched so an explicit CLI `--safe-mode` is still honored; crash-report upload skipped).
//!
//! This is a SOURCE-level guard, not a runtime test (the vendored C++ compiles only on CI,
//! per the project's Tier-0 policy). The risk it defends against is a future
//! `git subtree pull --squash` upstream release-bump (`/update-av-stack`, #44) silently
//! re-importing upstream's blocking modal. If that happens, CI fails loudly here. The pwsh
//! source-text mirror it once had in the Windows genlock workflows went with those workflows
//! (retired by issue 1505: the Windows boxes run the pinned obs release).

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. an upstream merge re-indenting a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_APP: &str = "vendor/obs-studio/frontend/OBSApp.cpp";
const STRIH_AHK: &str = "scripts/strih/NL_STARTUP.ahk";

/// The unique, ASCII-only marker phrase from the #1195 auto-normal WARNING log line.
/// Kept ASCII (no em-dash) so the C++ narrow literal and this Rust anchor are byte-identical
/// on every compiler.
const AUTO_NORMAL_MARKER: &str = "auto-selecting NORMAL launch (rig build: never block on a modal)";

#[test]
fn check_for_unclean_shutdown_auto_selects_normal_launch() {
    let app = squish(&repo_file(OBS_APP));

    // The #1195 patch: checkForUncleanShutdown() logs the auto-normal WARNING instead of
    // showing the modal.
    assert!(
        app.contains(AUTO_NORMAL_MARKER),
        "{OBS_APP}: the #1195 auto-normal WARNING ('{AUTO_NORMAL_MARKER}') is missing — \
         checkForUncleanShutdown() must log it and proceed with a NORMAL launch instead of \
         showing the Safe-Mode modal. A subtree pull likely restored the upstream modal; \
         re-apply the #1195 patch."
    );
}

#[test]
fn upstream_blocking_safe_mode_modal_is_gone() {
    let app = squish(&repo_file(OBS_APP));

    // The whole modal machinery must be removed. Each of these is a distinct token from
    // upstream's handleUncleanShutdown() / its call site; any one reappearing means the
    // blocking modal is back.
    for banned in [
        // the modal handler function definition
        "UncleanLaunchAction handleUncleanShutdown(bool enableCrashUpload)",
        // its call site inside checkForUncleanShutdown()
        "handleUncleanShutdown(hasNewCrashLog)",
        // the QMessageBox modal itself + its blocking exec()
        "QMessageBox crashWarning;",
        "crashWarning.exec()",
        // the log line only reachable via the interactive modal's Normal path
        "[Safe Mode] Normal launch selected",
    ] {
        assert!(
            !app.contains(banned),
            "{OBS_APP}: upstream's blocking Safe-Mode modal is BACK — found '{banned}'. \
             A subtree pull re-imported it; re-apply the #1195 patch (remove the modal, \
             auto-select a NORMAL launch)."
        );
    }
}

#[test]
fn strih_ahk_clears_sentinels_before_launching_obs() {
    let ahk = repo_file(STRIH_AHK);
    let squished = squish(&ahk);

    // Belt & braces: the strih respawn AHK must clear the OBS crash sentinels before it
    // launches OBS, so even a not-yet-redeployed (pre-#1195) binary never hits the modal.
    let sentinel_clear = r#"FileDelete A_AppData "\obs-studio\.sentinel\*""#;
    assert!(
        squished.contains(sentinel_clear),
        "{STRIH_AHK}: the #1195 sentinel-clear ('{sentinel_clear}') is missing — app1() must \
         clear %APPDATA%\\obs-studio\\.sentinel\\* before launching OBS (same cleanup as \
         launch-obs-genlock.sh)."
    );

    // ...and it must run BEFORE the OBS launch, not after (order matters). Check on the raw
    // text: the sentinel clear precedes the `Run app1_path` launch.
    let clear_at = ahk
        .find(".sentinel")
        .expect("sentinel-clear line not found in the AHK");
    let launch_at = ahk
        .find("Run app1_path")
        .expect("`Run app1_path` OBS launch not found in the AHK");
    assert!(
        clear_at < launch_at,
        "{STRIH_AHK}: the sentinel clear must come BEFORE `Run app1_path` (clearing after the \
         launch cannot stop the modal for that launch)."
    );
}
