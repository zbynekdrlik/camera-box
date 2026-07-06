//! #557 — `scripts/setup.sh` (the standalone `curl | sudo bash` quick-install path) hardcoded
//! `ExecStart=/usr/local/bin/camera-box --display "STRIH-SNV (interkom)"` into EVERY box's
//! systemd unit, never sourcing `scripts/camera-set.sh`'s `CAMERA_DISPLAY_SOURCE` table (#528).
//! That violated BOTH the pre-existing #450 "canonical plain ExecStart, identical everywhere"
//! invariant AND the #528 single-source-of-truth claim: a box provisioned via this path (any
//! camera except cam1) got the interkom preview baked in anyway.
//!
//! The fix mirrors `scripts/setup-device.sh`'s REAL mechanism (verified by reading it directly,
//! not by trusting a paraphrase): ExecStart stays canonical PLAIN on every box (matching #450);
//! the HDMI cameraman preview lives entirely in `/etc/camera-box/config.toml`'s optional
//! `[display]` section, populated from `scripts/camera-set.sh`'s `CAMERA_DISPLAY_SOURCE` table —
//! the table stays the ONE source of truth (setup.sh downloads the real file rather than keeping
//! a second, driftable copy of it, since it has no local repo checkout when curl-piped).
//!
//! Same convention as `tests/setup_device_pure_functions.rs`: source the REAL script (its
//! `BASH_SOURCE` guard skips the destructive `main "$@"` install flow) and call its pure
//! functions directly against the REAL `scripts/camera-set.sh` fleet map (via `CAMERA_SET_SOURCE`
//! pointed at the local file — no network in tests).
//!
//! RED before this change (no guard --> sourcing runs `main "$@"` and exits before defining
//! anything; `resolve_display_source` / `config_toml_display_section` do not exist); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn camera_set_path() -> PathBuf {
    let p = manifest_dir().join("scripts/camera-set.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the REAL script (its `BASH_SOURCE != $0` guard must skip the destructive `main "$@"`
/// install flow) and run `body` against its pure functions. `CAMERA_SET_SOURCE` is pointed at the
/// REAL local `scripts/camera-set.sh` so no network call happens in tests, while production
/// curl-pipe installs still default to downloading it from GitHub. Returns (exit_code, stdout,
/// stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .env("CAMERA_SET_SOURCE", camera_set_path())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// Sourcing must NOT run the destructive install flow (the source-guard itself)
// ---------------------------------------------------------------------------------------------

/// Sourcing setup.sh must return cleanly (never call `main "$@"`, never `check_root`/exit-1,
/// never touch the disk) so it is safe to source from a non-root test process. This is the
/// precondition for every other test in this file.
#[test]
fn sourcing_setup_sh_does_not_run_main() {
    let (code, out, err) = run_sourced(r#"echo "SOURCED_OK""#);
    assert_eq!(
        code, 0,
        "sourcing scripts/setup.sh must succeed without running main() (stdout={out:?} stderr={err:?})"
    );
    assert_eq!(
        out.trim(),
        "SOURCED_OK",
        "sourcing must return before executing anything from main() -- stderr={err:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// resolve_display_source HOSTNAME -- table-driven, via the REAL camera-set.sh, never duplicated
// ---------------------------------------------------------------------------------------------

/// cam1 has a configured preview in the real fleet table ("STRIH-SNV (interkom)", #528).
#[test]
fn resolve_display_source_resolves_cam1_from_the_real_table() {
    let (code, out, err) = run_sourced(r#"resolve_display_source CAM1; echo"#);
    assert_eq!(
        code, 0,
        "resolve_display_source CAM1 must succeed. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "STRIH-SNV (interkom)",
        "resolve_display_source CAM1 must return the real camera-set.sh table entry"
    );
}

/// cam2 (and every other non-cam1 box) has NO configured preview -- must resolve to empty, never
/// fall back to cam1's interkom source (the exact #557 bug: every box got the same value).
#[test]
fn resolve_display_source_is_empty_for_cam2() {
    let (code, out, err) = run_sourced(r#"resolve_display_source cam2; echo "<END>""#);
    assert_eq!(
        code, 0,
        "resolve_display_source cam2 must succeed. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "<END>",
        "resolve_display_source cam2 must be empty -- cam2 has no CAMERA_DISPLAY_SOURCE table entry"
    );
}

/// Case-insensitive, matching setup-device.sh's resolve_device_name convention.
#[test]
fn resolve_display_source_is_case_insensitive() {
    for input in ["cam1", "Cam1", "CAM1", "cAm1"] {
        let (code, out, err) = run_sourced(&format!(r#"resolve_display_source {input}"#));
        assert_eq!(
            code, 0,
            "resolve_display_source {input} must succeed. stderr: {err}"
        );
        assert_eq!(
            out.trim(),
            "STRIH-SNV (interkom)",
            "resolve_display_source {input} must resolve identically regardless of case"
        );
    }
}

/// An UNRECOGNIZED hostname (the "camera-box" default, or any non-fleet name) must resolve to
/// EMPTY, not fail the whole install -- setup.sh's hostname argument is not required to be a
/// fleet camN name (unlike setup-device.sh's resolve_device_name, whose whole job is fleet
/// provisioning).
#[test]
fn resolve_display_source_is_empty_and_non_fatal_for_an_unrecognized_hostname() {
    let (code, out, err) = run_sourced(r#"resolve_display_source camera-box; echo "<END>""#);
    assert_eq!(
        code, 0,
        "an unrecognized hostname must not abort the caller. stderr: {err}"
    );
    assert_eq!(
        out.trim(),
        "<END>",
        "unrecognized hostname must resolve to empty"
    );
}

/// Sweep the whole real fleet map -- only cam1 has a configured preview today.
#[test]
fn resolve_display_source_sweeps_the_whole_fleet() {
    let expected = [
        ("cam1", "STRIH-SNV (interkom)"),
        ("cam2", ""),
        ("cam3", ""),
        ("cam4", ""),
        ("cam5", ""),
        ("cam6", ""),
        ("cam7", ""),
    ];
    for (input, want) in expected {
        let (code, out, err) =
            run_sourced(&format!(r#"resolve_display_source {input}; echo "<END>""#));
        assert_eq!(
            code, 0,
            "resolve_display_source {input} must succeed. stderr: {err}"
        );
        let got = out.trim().trim_end_matches("<END>");
        assert_eq!(
            got, want,
            "resolve_display_source {input} resolved incorrectly"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// config_toml_display_section SOURCE -- byte-identical contract to setup-device.sh's function
// ---------------------------------------------------------------------------------------------

#[test]
fn config_toml_display_section_is_empty_for_no_source() {
    let (code, out, err) = run_sourced(r#"config_toml_display_section """#);
    assert_eq!(code, 0, "must not fail. stderr: {err}");
    assert_eq!(
        out, "",
        "must emit nothing for an empty source; got: {out:?}"
    );
}

#[test]
fn config_toml_display_section_emits_display_section_for_a_configured_source() {
    let (code, out, err) = run_sourced(r#"config_toml_display_section "STRIH-SNV (interkom)""#);
    assert_eq!(code, 0, "must succeed. stderr: {err}");
    assert!(
        out.contains("[display]"),
        "must emit a [display] header; got: {out:?}"
    );
    assert!(
        out.contains(r#"source = "STRIH-SNV (interkom)""#),
        "must wire the given source; got: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The systemd unit written by install_camera_box must carry a canonical PLAIN ExecStart on
// EVERY box (#450) -- textual guard on the static heredoc, mirrors
// setup_device_provisioner_hardening.rs::setup_device_execstart_is_canonical_plain.
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_sh_execstart_is_canonical_plain() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        !body.contains(r#"--display "STRIH-SNV"#),
        "setup.sh must not bake a hardcoded --display flag into the systemd unit's ExecStart -- \
         the canonical unit must be identical everywhere (#450/#557)"
    );
    assert!(
        body.lines()
            .any(|l| l.trim() == "ExecStart=/usr/local/bin/camera-box"),
        "setup.sh must write a canonical PLAIN ExecStart=/usr/local/bin/camera-box (#450/#557)"
    );
}

/// The [display] config.toml section must actually be wired into install_camera_box's config
/// generation -- a dead pure function that's never called would still pass every test above.
#[test]
fn setup_sh_wires_display_section_into_config_toml_generation() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains("config_toml_display_section"),
        "install_camera_box must call config_toml_display_section to append the [display] \
         section to config.toml (#557); got no reference in the script body"
    );
    assert!(
        body.contains("resolve_display_source"),
        "install_camera_box must call resolve_display_source to look up this box's \
         CAMERA_DISPLAY_SOURCE table entry (#557); got no reference in the script body"
    );
}
