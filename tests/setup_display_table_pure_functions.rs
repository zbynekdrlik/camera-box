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

/// A FETCH failure (network blip, GitHub rate-limit, DNS not yet converged) is a genuine anomaly
/// -- UNLIKE an unrecognized hostname, it must warn on stderr (matching the warn() convention
/// every other network fetch in this script uses) so it isn't silently indistinguishable in the
/// provisioning log from "this box legitimately has no preview" (#557-review finding).
#[test]
fn resolve_display_source_warns_and_returns_empty_on_fetch_failure() {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\n\
        CAMERA_SET_SOURCE=/definitely/does/not/exist/camera-set.sh\n\
        resolve_display_source cam1\n\
        echo \"<END>\"";
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", script())
        // Deliberately do NOT set CAMERA_SET_SOURCE here -- the body overrides it to a bogus
        // local path so fetch_camera_set's `cp` arm fails without touching the network.
        .output()
        .expect("failed to run bash harness");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        code, 0,
        "a fetch failure must not abort the caller. stderr: {stderr}"
    );
    assert_eq!(
        stdout.trim(),
        "<END>",
        "a fetch failure must resolve to empty (no preview), same as an unrecognized hostname"
    );
    assert!(
        stderr.contains("could not fetch"),
        "a fetch failure must warn on stderr; stderr was: {stderr:?}"
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
// strip_config_toml_display_section TEXT -- the idempotency half of the #557-review fix: a
// re-run of install_camera_box's [display] reconciliation must never duplicate or leave a stale
// section behind, regardless of what config.toml already contained.
// ---------------------------------------------------------------------------------------------

const CONFIG_WITH_DISPLAY: &str = "# Camera-Box Configuration - CAM1\n\nndi_name = \"usb\"\ndevice = \"auto\"\n\n[intercom]\nstream = \"cam1\"\ntarget = \"strih.lan\"\n\n# HDMI cameraman preview (#528/#557 -- CAMERA_DISPLAY_SOURCE table, scripts/camera-set.sh)\n[display]\nsource = \"STRIH-SNV (interkom)\"\n";

const CONFIG_NO_DISPLAY: &str = "# Camera-Box Configuration - CAM2\n\nndi_name = \"usb\"\ndevice = \"auto\"\n\n[intercom]\nstream = \"cam2\"\ntarget = \"strih.lan\"\n";

#[test]
fn strip_config_toml_display_section_removes_an_existing_section() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nstrip_config_toml_display_section \"$TEXT\"",
        CONFIG_WITH_DISPLAY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("[display]"),
        "must strip the [display] header; got: {out:?}"
    );
    assert!(
        !out.contains("source ="),
        "must strip the source line; got: {out:?}"
    );
    assert!(
        !out.contains("HDMI cameraman preview"),
        "must strip the preceding comment; got: {out:?}"
    );
    assert!(
        out.contains("[intercom]"),
        "must NOT strip unrelated sections; got: {out:?}"
    );
    assert!(
        out.contains(r#"stream = "cam1""#),
        "must NOT strip unrelated content; got: {out:?}"
    );
}

/// Deep-review finding: a hand-edited config.toml with a trailing inline comment on the
/// `[display]` header (`[display]  # note`, valid TOML) must STILL be recognized and stripped --
/// an exact-whole-line match would silently miss it, leaving the OLD section in place while the
/// caller's subsequent unconditional append writes a SECOND `[display]` table. That duplicate
/// table is invalid TOML and fails `toml::from_str` in src/config.rs, crash-looping camera-box
/// (Restart=always/RestartSec=3) until someone SSHes in and hand-fixes the file.
#[test]
fn strip_config_toml_display_section_recognizes_a_header_with_a_trailing_comment() {
    const CONFIG_WITH_COMMENTED_HEADER: &str = "# Camera-Box Configuration - CAM1\n\nndi_name = \"usb\"\n\n[intercom]\nstream = \"cam1\"\n\n[display]  # hand-edited note\nsource = \"STRIH-SNV (interkom)\"\n";
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nstrip_config_toml_display_section \"$TEXT\"",
        CONFIG_WITH_COMMENTED_HEADER.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !out.contains("[display]"),
        "must strip a [display] header even with a trailing inline comment; got: {out:?}"
    );
    assert!(
        !out.contains("source ="),
        "must strip the source line under a commented header; got: {out:?}"
    );
    assert!(
        out.contains("[intercom]") && out.contains(r#"stream = "cam1""#),
        "must NOT strip unrelated content; got: {out:?}"
    );
}

#[test]
fn strip_config_toml_display_section_is_a_noop_when_no_display_section_present() {
    let (code, out, err) = run_sourced(&format!(
        "TEXT='{}'\nstrip_config_toml_display_section \"$TEXT\"",
        CONFIG_NO_DISPLAY.replace('\'', "'\\''")
    ));
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out.trim_end(),
        CONFIG_NO_DISPLAY.trim_end(),
        "a config with no [display] section must be left byte-identical (modulo trailing \
         newline); got: {out:?}"
    );
}

/// Round-trip property: strip(append(strip(X))) == strip(X) -- stripping fully undoes a prior
/// append, proving the strip-then-append reconciliation in install_camera_box never accumulates
/// cruft (duplicate/stale [display] sections) no matter how many times it re-runs.
#[test]
fn strip_after_append_recovers_the_stripped_base_config() {
    let harness = format!(
        "set -uo pipefail\n. \"$SCRIPT\"\n\
         TEXT='{}'\n\
         BASE=\"$(strip_config_toml_display_section \"$TEXT\")\"\n\
         APPENDED=\"$(printf '%s\\n' \"$BASE\"; config_toml_display_section 'STRIH-SNV (interkom)')\"\n\
         RESTRIPPED=\"$(strip_config_toml_display_section \"$APPENDED\")\"\n\
         if [ \"$BASE\" = \"$RESTRIPPED\" ]; then echo MATCH; else echo MISMATCH; fi",
        CONFIG_NO_DISPLAY.replace('\'', "'\\''")
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .env("CAMERA_SET_SOURCE", camera_set_path())
        .output()
        .expect("failed to run bash harness");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout.trim(),
        "MATCH",
        "stripping after appending must recover the exact stripped base config, proving \
         strip+append never accumulates cruft across repeated runs. stderr: {stderr}"
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

/// The [display] config.toml section must actually be CALLED from install_camera_box's config
/// generation -- a dead pure function that's never invoked would still pass every test above.
/// Searches for the exact CALL-SITE substrings (function name + its real argument), not a bare
/// `body.contains("config_toml_display_section")` -- that bare form is tautological, since it
/// ALSO matches the function's own `config_toml_display_section() {` definition line and would
/// keep passing even if install_camera_box's call to it were deleted entirely (the #557-review
/// finding this test was rewritten to close; mirrors verify_device_pure_functions.rs's
/// check_p_is_wired_into_the_live_flow_and_usage_doc, which slices past a marker for the same
/// reason).
#[test]
fn setup_sh_wires_display_section_into_config_toml_generation() {
    let body = std::fs::read_to_string(script()).unwrap();
    assert!(
        body.contains(r#"config_toml_display_section "$display_source""#),
        "install_camera_box must CALL config_toml_display_section with the resolved source to \
         append the [display] section to config.toml (#557); got no call-site reference"
    );
    assert!(
        body.contains(r#"resolve_display_source "$DEVICE_HOSTNAME""#),
        "install_camera_box must CALL resolve_display_source to look up this box's \
         CAMERA_DISPLAY_SOURCE table entry (#557); got no call-site reference"
    );
}

/// The idempotency half of the #557-review fix: re-running install_camera_box on an
/// already-provisioned box (config.toml already exists) must still reconcile the [display]
/// section -- not skip it the way the base config.toml content above is skipped. Proven by
/// checking the reconciliation call sites live OUTSIDE the `if [[ ! -f ... ]]` guard that gates
/// the base config.toml generation.
#[test]
fn display_section_reconciliation_runs_outside_the_first_install_guard() {
    let body = std::fs::read_to_string(script()).unwrap();
    let guard = "if [[ ! -f \"$CONFIG_DIR/config.toml\" ]]; then";
    let guard_pos = body
        .find(guard)
        .expect("the first-install config.toml guard must still be present");
    // Find the matching `fi` that closes this guard block: the next line, at the SAME
    // indentation (4 spaces) as the `if`, that is exactly "    fi".
    let after_guard = &body[guard_pos..];
    let fi_offset = after_guard
        .find("\n    fi\n")
        .expect("could not find the closing `fi` of the first-install guard");
    let after_fi = &body[guard_pos + fi_offset..];

    // The critical assertion: the reconciliation call sites must NOT be nested inside the
    // first-install `if` block (i.e. they appear AFTER its closing `fi`, at the same or shallower
    // indentation) -- if they were still inside the guard, a re-run on an already-provisioned box
    // would skip them entirely, exactly reproducing the #557-review regression.
    let inside_guard = &body[guard_pos..guard_pos + fi_offset];
    assert!(
        !inside_guard.contains("resolve_display_source"),
        "the [display] reconciliation must NOT be nested inside the first-install-only guard -- \
         a re-run on an already-provisioned box (config.toml already exists) would then silently \
         skip it, permanently losing the HDMI preview (#557-review finding). Guard body was: {inside_guard:?}"
    );
    assert!(
        after_fi.contains(r#"resolve_display_source "$DEVICE_HOSTNAME""#),
        "expected the reconciliation call site AFTER the first-install guard's closing `fi`"
    );
}
