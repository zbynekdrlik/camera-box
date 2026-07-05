//! Functional (execution) guard for `scripts/setup-device.sh`'s pure #450 name-resolution helper
//! (`resolve_device_name`).
//!
//! `tests/setup_device_provisioner_hardening.rs` pins the CONTRACT textually (sourcing
//! camera-set.sh, calling camera_resolve, no baked positional args) but a purely textual guard
//! cannot catch a silent LOGIC bug (e.g. the uppercase/lowercase normalization flipped, or the
//! wrong camera-set.sh field copied into DEVICE_IP/VBAN_STREAM). This file closes that gap by
//! actually SOURCING the real script and RUNNING `resolve_device_name` against the REAL
//! `scripts/camera-set.sh` fleet map -- same convention as `tests/setup_imag_pure_functions.rs` /
//! `tests/genlock_manifest.rs::run_sourced`.
//!
//! setup-device.sh's `BASH_SOURCE[0] != $0` guard (right after the pure function definitions)
//! makes sourcing safe: everything below the guard (the destructive one-shot provisioning flow --
//! `[ "$EUID" -eq 0 ] || fail ...`, `apt-get`, `netplan`, grub edits, etc.) is skipped, and only
//! `fail()` / `resolve_device_name()` / the sourced `camera-set.sh` functions are defined in the
//! sourcing shell.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup-device.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard skips the destructive provisioning
/// flow) and run `body` against its pure functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// resolve_device_name must accept the canonical uppercase hostname form and derive IP / VBAN
/// stream / genlock FPS from the real camera-set.sh fleet map (cam5 -> 10.77.9.65, #451).
#[test]
fn resolve_device_name_resolves_uppercase_name() {
    let (code, out, err) = run_sourced(
        r#"resolve_device_name CAM5
           printf 'NAME=%s IP=%s STREAM=%s FPS=%s\n' "$DEVICE_NAME" "$DEVICE_IP" "$VBAN_STREAM" "$CAMERA_GENLOCK_FPS""#,
    );
    assert_eq!(
        code, 0,
        "resolve_device_name CAM5 should succeed. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "NAME=CAM5 IP=10.77.9.65 STREAM=cam5 FPS=60",
        "resolve_device_name must derive DEVICE_NAME (uppercase hostname) / DEVICE_IP / \
         VBAN_STREAM (lowercase stream) / CAMERA_GENLOCK_FPS from the real camera-set.sh map"
    );
}

/// Case-insensitivity: a lowercase or mixed-case input must resolve identically to the uppercase
/// form -- the historical hostname convention is uppercase, but camera-set.sh's own table keys
/// are lowercase (cam1..cam7).
#[test]
fn resolve_device_name_is_case_insensitive() {
    for input in ["cam3", "Cam3", "CAM3", "cAm3"] {
        let (code, out, err) = run_sourced(&format!(
            r#"resolve_device_name {input}
               printf 'NAME=%s IP=%s STREAM=%s\n' "$DEVICE_NAME" "$DEVICE_IP" "$VBAN_STREAM""#
        ));
        assert_eq!(
            code, 0,
            "resolve_device_name {input} should succeed. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            "NAME=CAM3 IP=10.77.9.63 STREAM=cam3",
            "resolve_device_name {input} must resolve identically regardless of input case"
        );
    }
}

/// An unknown camera name must fail loud (non-zero exit), never silently fall through to a
/// default box -- camera-set.sh's own fail-closed `case` rejects it and resolve_device_name must
/// propagate that as a hard exit, not let a careless caller ignore a nonzero return.
#[test]
fn resolve_device_name_fails_loud_on_unknown_name() {
    let (code, out, _err) = run_sourced(
        r#"resolve_device_name bogus9
           echo "UNREACHABLE"
           echo "$DEVICE_NAME""#,
    );
    assert_ne!(
        code, 0,
        "resolve_device_name bogus9 must exit non-zero on an unknown camera name"
    );
    assert!(
        !out.contains("UNREACHABLE"),
        "resolve_device_name must exit immediately on an unknown name -- no code after the call \
         should run. stdout: {out}"
    );
}

/// An empty name must also fail loud (never silently resolve to some default camera).
#[test]
fn resolve_device_name_fails_loud_on_empty_name() {
    let (code, out, _err) = run_sourced(
        r#"resolve_device_name ""
           echo "UNREACHABLE""#,
    );
    assert_ne!(code, 0, "resolve_device_name '' must exit non-zero");
    assert!(!out.contains("UNREACHABLE"));
}

/// Every fleet camera (cam1-cam7) must resolve through the real setup-device.sh + camera-set.sh
/// pairing -- a broad sweep so a future fleet-map edit (#451 added cam5-7) can't silently break
/// one name while the others still pass.
#[test]
fn resolve_device_name_resolves_the_whole_fleet() {
    let expected = [
        ("cam1", "CAM1", "10.77.9.61"),
        ("cam2", "CAM2", "10.77.9.62"),
        ("cam3", "CAM3", "10.77.9.63"),
        ("cam4", "CAM4", "10.77.9.64"),
        ("cam5", "CAM5", "10.77.9.65"),
        ("cam6", "CAM6", "10.77.9.66"),
        ("cam7", "CAM7", "10.77.9.67"),
    ];
    for (input, want_name, want_ip) in expected {
        let (code, out, err) = run_sourced(&format!(
            r#"resolve_device_name {input}
               printf '%s %s\n' "$DEVICE_NAME" "$DEVICE_IP""#
        ));
        assert_eq!(
            code, 0,
            "resolve_device_name {input} should succeed. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            format!("{want_name} {want_ip}"),
            "resolve_device_name {input} resolved incorrectly"
        );
    }
}
