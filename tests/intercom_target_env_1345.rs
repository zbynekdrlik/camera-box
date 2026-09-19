//! #1345 M1b — unit tests for the appliance-side intercom-target env override
//! (`camera_box::intercom_target::resolve_intercom_target`).
//!
//! The dev cambox root filesystem is READ-ONLY, so `/etc/camera-box/config.toml` cannot be edited to
//! repoint the VBAN intercom at the new Linux strih-lx hub. camera-box therefore honours a
//! `CAMERA_BOX_INTERCOM_TARGET` env override (env > CLI flag > config.toml), carried by a `/run`
//! systemd drop-in (`scripts/lib/intercom-target-dropin.sh`). This module pins the PURE resolution:
//! a non-empty trimmed env value wins and yields a log note naming both hosts; `None`/empty/
//! whitespace leaves the resolved (CLI/config) host untouched with no note.
//!
//! Pure std logic → Tier-0 (runs on the Linux `test` CI job, default features).

use camera_box::intercom_target::{resolve_intercom_target, INTERCOM_TARGET_ENV};

#[test]
fn env_none_keeps_resolved_and_has_no_note() {
    let (target, note) = resolve_intercom_target(None, "strih.lan");
    assert_eq!(target, "strih.lan");
    assert!(note.is_none(), "no env override must produce no note");
}

#[test]
fn env_empty_string_keeps_resolved_and_has_no_note() {
    let (target, note) = resolve_intercom_target(Some(""), "strih.lan");
    assert_eq!(target, "strih.lan");
    assert!(note.is_none(), "an empty env value is treated as unset");
}

#[test]
fn env_whitespace_only_keeps_resolved_and_has_no_note() {
    let (target, note) = resolve_intercom_target(Some("   \t "), "strih.lan");
    assert_eq!(target, "strih.lan");
    assert!(note.is_none(), "a whitespace-only env value is treated as unset");
}

#[test]
fn env_override_replaces_target_and_notes() {
    let (target, note) = resolve_intercom_target(Some("strih-lx.lan"), "strih.lan");
    assert_eq!(target, "strih-lx.lan", "the env value overrides the resolved host");
    let note = note.expect("an override must produce a log note");
    assert!(
        note.contains(INTERCOM_TARGET_ENV),
        "note must name the env var: {note}"
    );
    assert!(note.contains("strih-lx.lan"), "note must name the NEW host: {note}");
    assert!(note.contains("strih.lan"), "note must name the OLD host: {note}");
}

#[test]
fn env_override_is_trimmed() {
    let (target, note) = resolve_intercom_target(Some("  strih-lx.lan\n"), "strih.lan");
    assert_eq!(target, "strih-lx.lan", "surrounding whitespace is trimmed off the override");
    assert!(note.is_some(), "a (trimmed) non-empty override still produces a note");
}

#[test]
fn note_names_both_hosts_distinctly() {
    let (target, note) = resolve_intercom_target(Some("newhost"), "oldhost");
    assert_eq!(target, "newhost");
    let note = note.expect("override note");
    assert!(note.contains("newhost"), "note names the new host: {note}");
    assert!(note.contains("oldhost"), "note names the old host: {note}");
    assert!(note.contains(INTERCOM_TARGET_ENV), "note names the env var: {note}");
}

#[test]
fn env_var_constant_is_the_documented_name() {
    assert_eq!(INTERCOM_TARGET_ENV, "CAMERA_BOX_INTERCOM_TARGET");
}
