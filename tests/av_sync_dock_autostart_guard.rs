//! #690 — the A/V-sync dock output must AUTO-START on OBS load, never sit as a forgettable manual
//! toggle.
//!
//! Live finding (2026-08-01): after the #806 full-bundle deploy restarted OBS, the dock panel
//! reverted to its default-constructed "stopped" state (dashes) — nobody had clicked Start again.
//! Per the standing HARD rule (a needed feature is always ON by default or a real GUI control,
//! never a forgettable one-off toggle), the fix is to start the `sync-test-output` automatically
//! once OBS has finished loading (`OBS_FRONTEND_EVENT_FINISHED_LOADING`), not to rely on an
//! operator noticing and clicking the button.
//!
//! `sync-test-dock.cpp`/`.hpp` include Qt + `obs-frontend-api.h` + the full plugin headers, so
//! (like the rest of the vendored dock UI, and unlike the dependency-free `camera-box-audio.hpp` /
//! `camera-box-video.hpp` mirrors) they are NOT compiled by this repo's Linux CI — only the
//! separate ~150-min `windows-genlock-fast.yml` compile gate builds the real plugin. This is a
//! SOURCE-presence guard in the same convention as `av_sync_dock_qr_patch_guard.rs`
//! (`genlock_release_cadence.rs` / `distroav_genlock_lockdown.rs`): reads the vendored C++ as text
//! on default features, fails loudly if a future `git subtree pull` of av-sync-dock (or a careless
//! edit) silently drops the auto-start wiring — the dock would then again start dark on OBS
//! relaunch until an operator noticed, with the compile gate staying green either way.

use std::path::PathBuf;

const DOCK_HPP: &str = "vendor/av-sync-dock/src/sync-test-dock.hpp";
const DOCK_CPP: &str = "vendor/av-sync-dock/src/sync-test-dock.cpp";

fn vendor_file(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the assertions survive
/// reformatting (e.g. a future clang-format pass re-wrapping a line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn dock_header_declares_the_frontend_event_hook() {
    let hdr = squish(&vendor_file(DOCK_HPP));
    for marker in [
        "void start();",
        "static void cb_frontend_event(enum obs_frontend_event event, void *param);",
    ] {
        assert!(
            hdr.contains(marker),
            "{DOCK_HPP}: #690 marker `{marker}` is gone — the dock lost its programmatic \
             start()/auto-start hook. Re-apply."
        );
    }
}

#[test]
fn dock_registers_and_unregisters_the_frontend_event_callback() {
    let src = squish(&vendor_file(DOCK_CPP));
    for marker in [
        "obs_frontend_add_event_callback(cb_frontend_event, this)",
        "obs_frontend_remove_event_callback(cb_frontend_event, this)",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_CPP}: #690 marker `{marker}` is gone — the dock no longer registers/cleans up \
             its OBS frontend-event hook. Re-apply."
        );
    }
}

#[test]
fn dock_auto_starts_on_finished_loading_and_only_if_not_already_running() {
    let src = squish(&vendor_file(DOCK_CPP));
    for marker in [
        "if (event != OBS_FRONTEND_EVENT_FINISHED_LOADING) return;",
        "if (!dock->sync_test) dock->start();",
    ] {
        assert!(
            src.contains(marker),
            "{DOCK_CPP}: #690 marker `{marker}` is gone — the dock no longer auto-starts \
             (idempotently) once OBS finishes loading. Re-apply (the #690 fix for the dock \
             silently sitting stopped after every OBS relaunch)."
        );
    }
}
