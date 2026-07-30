//! #867 — `scripts/lib/ahk-watchdog.sh`'s `ahk_resolve_and_relaunch_ps()`, the SINGLE SOURCE OF
//! TRUTH both `scripts/obs-self-heal-install.sh` and `scripts/launch-obs-genlock.sh` embed to
//! robustly relaunch + VERIFY strih's NL_STARTUP.ahk AutoHotkey64 auto-respawn watcher.
//!
//! ## The bug (comment #5121884098 on #867, correcting the issue body's wrong premise)
//!
//! AutoHotkey IS installed on strih (v2.0.19, user-scoped under
//! `%LOCALAPPDATA%\Programs\AutoHotkey\v2\AutoHotkey64.exe`) — it is simply NOT on PATH. A prior
//! script (the obs.dll swap) stopped it before touching obs64 and tried to restart it afterward
//! via a BARE `Start-Process -FilePath 'AutoHotkey64.exe'`, which can never resolve without PATH
//! — and then unconditionally logged success anyway. `scripts/obs-self-heal-install.sh`'s own
//! `ahk_start_block` had the exact same bug; `scripts/launch-obs-genlock.sh`'s `ahk_restart_ps`
//! launched the Startup shortcut (which DOES resolve) but never verified the PROCESS came back.
//!
//! These tests source the pure lib directly (no rig, no Windows) and assert the emitted
//! PowerShell (a) never launches AutoHotkey64 by a bare exe name, (b) probes the real user-scoped
//! install locations in order, and (c) polls `Get-Process AutoHotkey64` to set
//! `$ahkRelaunchVerified` for the caller to act on.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/ahk-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the lib and call `ahk_resolve_and_relaunch_ps`. Returns stdout.
fn relaunch_ps() -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nahk_resolve_and_relaunch_ps";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn never_launches_ahk_by_bare_exe_name() {
    let p = relaunch_ps();
    assert!(
        !p.contains("-FilePath 'AutoHotkey64.exe'")
            && !p.contains("-FilePath \"AutoHotkey64.exe\""),
        "#867: must never rely on PATH to resolve AutoHotkey64.exe. Program:\n{p}"
    );
}

#[test]
fn probes_candidate_install_paths_in_order_then_shortcut_then_path() {
    let p = relaunch_ps();
    let localappdata_pos = p
        .find("LOCALAPPDATA")
        .expect("must probe the user-scoped %LOCALAPPDATA% install location first");
    let programfiles_v2_pos = p[localappdata_pos..]
        .find("ProgramFiles 'AutoHotkey\\v2\\AutoHotkey64.exe'")
        .map(|off| off + localappdata_pos)
        .expect("must probe %ProgramFiles%\\AutoHotkey\\v2\\ next");
    let programfiles_v1_pos = p[programfiles_v2_pos..]
        .find("ProgramFiles 'AutoHotkey\\AutoHotkey64.exe'")
        .map(|off| off + programfiles_v2_pos)
        .expect("must probe %ProgramFiles%\\AutoHotkey\\ (v1 layout) next");
    let shortcut_pos = p[programfiles_v1_pos..]
        .find("NL_STARTUP")
        .map(|off| off + programfiles_v1_pos)
        .expect("must fall back to the NL_STARTUP Startup shortcut");
    let path_fallback_pos = p[shortcut_pos..]
        .find("Get-Command AutoHotkey64.exe")
        .map(|off| off + shortcut_pos)
        .expect("must fall back to Get-Command (PATH) last");
    let _ = path_fallback_pos;
}

#[test]
fn polls_get_process_and_sets_verified_and_target_vars() {
    let p = relaunch_ps();
    assert!(
        p.contains("$ahkRelaunchVerified = $false"),
        "must initialize the verify flag before the poll loop. Program:\n{p}"
    );
    assert!(
        p.contains("Get-Process AutoHotkey64 -ErrorAction SilentlyContinue"),
        "must poll for the real AutoHotkey64 process. Program:\n{p}"
    );
    assert!(
        p.contains("$ahkRelaunchVerified = $true"),
        "the poll loop must be able to flip verified to true. Program:\n{p}"
    );
    assert!(
        p.contains("$ahkRelaunchTarget"),
        "must record which path/shortcut was used, for the caller's log line. Program:\n{p}"
    );
}

/// Both real generator scripts must actually SOURCE this lib (never re-derive the logic).
#[test]
fn both_callers_source_the_shared_lib() {
    let self_heal =
        std::fs::read_to_string(manifest_dir().join("scripts/obs-self-heal-install.sh"))
            .expect("read obs-self-heal-install.sh");
    let launch = std::fs::read_to_string(manifest_dir().join("scripts/launch-obs-genlock.sh"))
        .expect("read launch-obs-genlock.sh");
    assert!(
        self_heal.contains("lib/ahk-watchdog.sh"),
        "obs-self-heal-install.sh must source scripts/lib/ahk-watchdog.sh"
    );
    assert!(
        launch.contains("lib/ahk-watchdog.sh"),
        "launch-obs-genlock.sh must source scripts/lib/ahk-watchdog.sh"
    );
}
