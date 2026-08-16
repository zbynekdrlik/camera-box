//! #774 — strih's `NL_STARTUP.ahk` AutoHotkey auto-respawn watcher must be VERSIONED in the repo
//! (it was live-only, versioned by nobody — the real bug behind the ~20-min-no-OBS event incident)
//! and its OBS respawn path must be robust.
//!
//! These are pure TEXT guards over the committed `scripts/strih/NL_STARTUP.ahk` — they need no
//! Windows host and no AHK runtime. They pin, so a future edit (or a re-capture from the live box)
//! cannot silently regress the respawn contract:
//!
//!   * OBS is respawned via the box's Start-Menu SHORTCUT (`OBS Studio.lnk`), never a bare
//!     `obs64.exe` — a bare respawn drops strih's `--enable-media-stream --verbose` params and
//!     renders the interkom VDO.ninja Browser source "Permissions denied" (the #775 incident).
//!   * The OBS window match is PROCESS-based (`ahk_exe obs64.exe`), never a title match — so an OBS
//!     title change (e.g. "newlevel.media build unknown") can NEVER break the respawn (the ticket's
//!     original "title changed" theory was wrong; this guard makes the correct behavior permanent).
//!   * `#SingleInstance Force` is present — a second launch cleanly REPLACES the first (the ticket's
//!     named "chvíľu 2 procesy" double-start footgun) AND re-runs the startup block, which resets
//!     the SafeLoop respawn guard back to on, self-healing a latched-off state.
//!   * The SafeLoop respawn loop actually respawns OBS (app1) when its window is gone.

use std::path::PathBuf;

fn ahk_source() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/strih/NL_STARTUP.ahk");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("#774: strih NL_STARTUP.ahk must be versioned at {} — it was a live-only script nobody versions. read error: {e}", p.display()))
}

/// The OBS app slot (app1) launches via the Start-Menu SHORTCUT, carrying the box's own params.
#[test]
fn obs_slot_respawns_via_shortcut_not_bare_exe() {
    let s = ahk_source();
    let app1_path = s
        .lines()
        .find(|l| l.trim_start().starts_with("app1_path"))
        .expect("#774: an app1_path (OBS slot) line must exist");
    assert!(
        app1_path.contains("OBS Studio.lnk"),
        "#774: OBS (app1) must respawn via the box's 'OBS Studio.lnk' shortcut (per-box params \
         preserved), not a bare exe. app1_path line:\n{app1_path}"
    );
    assert!(
        !app1_path.contains("obs64.exe") && !app1_path.to_lowercase().contains("bin\\64bit"),
        "#774: the OBS respawn PATH must be the .lnk, never a bare obs64.exe / bin\\64bit path — a \
         bare respawn drops strih's --enable-media-stream and breaks the interkom Browser source \
         (#775). app1_path line:\n{app1_path}"
    );
}

/// The OBS window match is by PROCESS (ahk_exe obs64.exe), so a title change never breaks respawn.
#[test]
fn obs_window_match_is_process_based_not_title() {
    let s = ahk_source();
    assert!(
        s.contains("app1_name := \"ahk_exe obs64.exe\""),
        "#774: the OBS window match must be process-based (ahk_exe obs64.exe) so a title change can \
         never stop the respawn. Source:\n{s}"
    );
}

/// `#SingleInstance Force` — no stuck double-start; a relaunch also re-arms the SafeLoop guard.
#[test]
fn single_instance_force_present() {
    let s = ahk_source();
    assert!(
        s.contains("#SingleInstance Force"),
        "#774: `#SingleInstance Force` must be present — a second launch must cleanly REPLACE the \
         first (the 'chvíľu 2 procesy' double-start footgun) and re-run the startup block. Source:\n{s}"
    );
}

/// The SafeLoop respawn loop actually respawns the OBS slot when its window is gone.
#[test]
fn safeloop_respawns_dead_obs() {
    let s = ahk_source();
    assert!(
        s.contains("While(SafeLoop)"),
        "#774: the respawn engine must be the `While(SafeLoop)` loop. Source:\n{s}"
    );
    // The app1 (OBS) respawn condition inside the loop — window gone => relaunch.
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("if (app1_run) and not WinExist(app1_name) app1()"),
        "#774: the loop must respawn OBS (app1) when its window is gone. Source:\n{s}"
    );
}
