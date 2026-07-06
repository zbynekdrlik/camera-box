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

// --- #568: color block deduped onto scripts/lib/cli-log.sh --------------------------------------
//
// setup-device.sh used to declare its own RED/GREEN/YELLOW/NC block (no log()/info()/warn()/err()
// functions -- it uses raw `echo -e "${COLOR}...${NC}"` everywhere, plus its own exit-on-call
// `fail()`). #568 replaces that local block with a `. "$HERE/lib/cli-log.sh"` source, keeping
// `fail()` itself untouched (different message shape / behavior from cli-log.sh's `err()`, so it
// stays script-local per the issue's own guidance). These tests prove the color values -- and
// therefore every existing `echo -e "${RED}...${NC}"` call site -- render EXACTLY the same bytes
// as the old local declaration (RED='\033[0;31m', GREEN='\033[0;32m', YELLOW='\033[1;33m',
// NC='\033[0m').

#[test]
fn color_vars_are_byte_identical_to_the_pre_568_local_declaration() {
    // Route through `echo -e` (like every real call site in the script), NOT a raw `printf '%s'`
    // -- RED/GREEN/YELLOW/NC hold LITERAL backslash-octal text (`'\033[0;31m'`, single-quoted, no
    // ANSI-C `$'...'` quoting); only `echo -e`'s own escape processing turns that into the actual
    // ESC byte at render time, exactly as fail() and every `echo -e "${RED}...${NC}"` line do.
    let (code, out, err) = run_sourced(r#"echo -en "${RED}|${GREEN}|${YELLOW}|${NC}""#);
    assert_eq!(
        code, 0,
        "sourcing setup-device.sh must succeed. stderr: {err}"
    );
    assert_eq!(
        out, "\x1b[0;31m|\x1b[0;32m|\x1b[1;33m|\x1b[0m",
        "RED/GREEN/YELLOW/NC must keep the exact escape codes setup-device.sh declared locally \
         before #568, now sourced from scripts/lib/cli-log.sh instead"
    );
}

#[test]
fn fail_renders_the_exact_pre_568_bytes_after_sourcing_the_shared_lib() {
    let (code, _out, err) = run_sourced("fail 'boom'");
    assert_eq!(code, 1, "fail() must still exit 1");
    assert_eq!(
        err, "\x1b[0;31mFAIL: boom\x1b[0m\n",
        "fail()'s rendered bytes must be unchanged by #568 (RED + 'FAIL: msg' + NC, to stderr)"
    );
}

// --- #528: config_toml_display_section (per-cam HDMI cameraman preview) ------------------------
//
// setup-device.sh's `#450`-canonical ExecStart must stay PLAIN and identical on every box
// (`tests/setup_device_provisioner_hardening.rs::setup_device_execstart_is_canonical_plain`) —
// #528's preview source therefore does NOT bake into ExecStart. Instead it's an optional
// `[display]` section appended to the per-box `config.toml` (the SAME file that already carries
// per-box variance today, e.g. the VBAN stream name) via this pure, sourced-and-unit-tested
// function — the same seam convention as `resolve_device_name` above.

/// config_toml_display_section must be a no-op (empty stdout) for a box with NO configured
/// preview (cam2-7 today — see scripts/camera-set.sh for why cam2 is deliberately excluded) —
/// camera-box then runs with no [display] section, i.e. no preview, exactly as every box behaved
/// before #528.
#[test]
fn config_toml_display_section_is_empty_for_no_source() {
    let (code, out, err) = run_sourced(r#"config_toml_display_section """#);
    assert_eq!(
        code, 0,
        "config_toml_display_section '' must not fail. stderr: {err}"
    );
    assert_eq!(
        out, "",
        "config_toml_display_section '' must emit nothing (no [display] section) — a box with \
         no CAMERA_DISPLAY_SOURCE table entry must not gain one; got: {out:?}"
    );
}

/// config_toml_display_section must emit a `[display]` TOML section with the given source when
/// one is configured (cam1 today, #528) — table-derived, not hardcoded. `fb_device` is
/// deliberately OMITTED — src/config.rs's DisplayConfig already serde-defaults it to "/dev/fb0"
/// when absent, so baking the literal here would just be a second, driftable copy of that default.
#[test]
fn config_toml_display_section_emits_display_section_for_a_configured_source() {
    let (code, out, err) = run_sourced(r#"config_toml_display_section "STRIH-SNV (interkom)""#);
    assert_eq!(
        code, 0,
        "config_toml_display_section must succeed for a configured source. stderr: {err}"
    );
    assert!(
        out.contains("[display]"),
        "config_toml_display_section must emit a [display] TOML section header; got: {out:?}"
    );
    assert!(
        out.contains(r#"source = "STRIH-SNV (interkom)""#),
        "config_toml_display_section must wire the given source into `source = \"...\"`; got: {out:?}"
    );
    assert!(
        !out.contains("fb_device"),
        "config_toml_display_section must NOT bake fb_device -- config.rs's DisplayConfig already \
         serde-defaults it to /dev/fb0 when the key is absent; got: {out:?}"
    );
}

/// A source containing a double-quote or backslash must be ESCAPED before landing in the TOML
/// string literal — an unescaped `"` would terminate the string mid-line and corrupt the rest of
/// config.toml. No table entry contains one today, but scripts/camera-set.sh's own comment invites
/// adding future entries, so this landmine must be defused now, not discovered at boot time later.
#[test]
fn config_toml_display_section_escapes_quotes_and_backslashes() {
    let (code, out, err) = run_sourced(r#"config_toml_display_section 'NDI "Weird" Source\path'"#);
    assert_eq!(
        code, 0,
        "config_toml_display_section must not fail on a quote/backslash source. stderr: {err}"
    );
    assert!(
        out.contains(r#"source = "NDI \"Weird\" Source\\path""#),
        "config_toml_display_section must escape embedded \\ and \" so the TOML string stays a \
         single well-formed literal; got: {out:?}"
    );
    // The whole emitted section, parsed as TOML, must decode back to the ORIGINAL source
    // (round-trip proof, not just a substring match) -- confirms the escaping is TOML-correct,
    // not merely something that looks plausible.
    let parsed: toml::Value = out
        .parse()
        .expect("emitted [display] section must parse as TOML");
    assert_eq!(
        parsed["display"]["source"].as_str(),
        Some("NDI \"Weird\" Source\\path"),
        "round-tripping the emitted TOML must decode back to the ORIGINAL unescaped source; \
         parsed: {parsed:?}"
    );
}
