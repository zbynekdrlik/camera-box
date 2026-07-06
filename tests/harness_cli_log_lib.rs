//! Regression guard for `scripts/lib/cli-log.sh` (#559) — the shared CLI color/log-line helper
//! block (`RED=...`/`log()`/`info()`/`warn()`/`err()`) that `scripts/deploy-fleet.sh`,
//! `scripts/upgrade-fleet-ndi.sh`, and `scripts/verify-fleet.sh` each carried a byte-identical
//! copy of before this fix (`scripts/verify-fleet.sh`, #552, was the third copy). Same discipline
//! as `scripts/lib/ndi-alive.sh` / `tests/harness_ndi_alive_lib.rs` (#451): the shared file gets
//! its own coverage independent of which script sources it, AND each script is pinned to source
//! it rather than re-defining the block locally.
//!
//! These tests exercise the shared file DIRECTLY (source it, call the functions, check the exact
//! rendered bytes) so a future edit to the log format cannot silently drift out from under any of
//! the three scripts.

use std::path::PathBuf;
use std::process::Command;

fn lib_script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/cli-log.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the shared lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
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
fn cli_log_lib_exists_and_is_shebanged() {
    let p = lib_script();
    let bytes = std::fs::read(&p).unwrap();
    assert!(
        bytes.starts_with(b"#!"),
        "cli-log.sh must start with a shebang"
    );
}

// ---------------------------------------------------------------------------------------------
// Exact rendered-byte checks -- the original block, byte for byte, so a de-dup can never
// silently change the color codes, the bracket labels, or the stdout/stderr routing.
// ---------------------------------------------------------------------------------------------

#[test]
fn log_renders_green_plus_bracket_to_stdout() {
    let out = run_sourced("log 'hello'");
    assert_eq!(out, "\x1b[0;32m[+]\x1b[0m hello\n");
}

#[test]
fn info_renders_blue_star_bracket_to_stdout() {
    let out = run_sourced("info 'hello'");
    assert_eq!(out, "\x1b[0;34m[*]\x1b[0m hello\n");
}

#[test]
fn warn_renders_yellow_bang_bracket_to_stdout() {
    let out = run_sourced("warn 'hello'");
    assert_eq!(out, "\x1b[1;33m[!]\x1b[0m hello\n");
}

#[test]
fn err_renders_red_error_bracket_to_stderr_not_stdout() {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\nerr 'boom'";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "err() must write nothing to stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "\x1b[0;31m[ERROR]\x1b[0m boom\n"
    );
}

// ---------------------------------------------------------------------------------------------
// Every consumer script sources the shared lib -- and none re-defines the block locally (the
// actual de-dup, not just an extra copy left behind alongside the new shared one).
// ---------------------------------------------------------------------------------------------

#[test]
fn deploy_fleet_sources_shared_cli_log_lib_and_defines_it_only_once() {
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("lib/cli-log.sh"),
        "deploy-fleet.sh must source scripts/lib/cli-log.sh (#559), not redefine log helpers locally"
    );
    assert!(
        !s.contains("RED='\\033"),
        "deploy-fleet.sh must not keep its own copy of the color/log block after sourcing the shared lib"
    );
}

#[test]
fn upgrade_fleet_ndi_sources_shared_cli_log_lib_and_defines_it_only_once() {
    let s = read("scripts/upgrade-fleet-ndi.sh");
    assert!(
        s.contains("lib/cli-log.sh"),
        "upgrade-fleet-ndi.sh must source scripts/lib/cli-log.sh (#559), not redefine log helpers locally"
    );
    assert!(
        !s.contains("RED='\\033"),
        "upgrade-fleet-ndi.sh must not keep its own copy of the color/log block after sourcing the shared lib"
    );
}

#[test]
fn verify_fleet_sources_shared_cli_log_lib_and_defines_it_only_once() {
    let s = read("scripts/verify-fleet.sh");
    assert!(
        s.contains("lib/cli-log.sh"),
        "verify-fleet.sh must source scripts/lib/cli-log.sh (#559), not redefine log helpers locally"
    );
    assert!(
        !s.contains("RED='\\033"),
        "verify-fleet.sh must not keep its own copy of the color/log block after sourcing the shared lib"
    );
}

// ---------------------------------------------------------------------------------------------
// #568 -- setup-device.sh and verify-device.sh carried their OWN, non-identical color-variable
// blocks (no log()/info()/warn()/err() functions in either -- they use raw `echo -e`/`printf`
// with the color vars directly, plus their own script-specific fail()/ok() helpers). Harmonizing
// means: dedup the shared ANSI color block onto scripts/lib/cli-log.sh, and leave each script's
// own genuinely-different helper (setup-device.sh's exit-on-call `fail()`; verify-device.sh's
// FAILS-counting `ok()`/`fail()`/`warn()`) local, unchanged.
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_device_sources_shared_cli_log_lib_and_defines_it_only_once() {
    let s = read("scripts/setup-device.sh");
    assert!(
        s.contains("lib/cli-log.sh"),
        "setup-device.sh must source scripts/lib/cli-log.sh (#568), not redefine the color block locally"
    );
    assert!(
        !s.contains("RED='\\033"),
        "setup-device.sh must not keep its own copy of the color block after sourcing the shared lib"
    );
}

#[test]
fn verify_device_sources_shared_cli_log_lib_and_defines_it_only_once() {
    let s = read("scripts/verify-device.sh");
    assert!(
        s.contains("lib/cli-log.sh"),
        "verify-device.sh must source scripts/lib/cli-log.sh (#568), not redefine the color block locally"
    );
    assert!(
        !s.contains("RED='\\033"),
        "verify-device.sh must not keep its own copy of the color block after sourcing the shared lib"
    );
}
