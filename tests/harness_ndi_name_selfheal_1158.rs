//! #1158 — `scripts/lib/ndi-name-selfheal.sh`'s `ndi_name_selfheal_run` is the [4c/8]
//! frozen-camera-gate self-heal for an emptied/drifted `ndi_source_name` (which STOPS/misroutes the
//! DistroAV receiver, unreachable by the in-loop #767/#1096 watchdogs). It delegates to
//! `set-ndi-mapping.py --heal`, returning that exit code (0 iff >=1 input healed).
//!
//! These tests exercise the env-seam (`NDI_NAME_SELFHEAL_CMD`) path — the default path shells
//! `python3 set-ndi-mapping.py --heal` against a live OBS, so it is not offline-testable, but the
//! seam proves (a) the function forwards host/active/scripts correctly and (b) it returns the
//! command's exit code, AND (c) — the #1133 class — that a NON-ZERO return, when the caller invokes
//! it in an `if`-condition under `set -euo pipefail`, does NOT abort the caller.

use std::path::PathBuf;
use std::process::Command;

fn lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/ndi-name-selfheal.sh")
}

/// Source the lib, run `ndi_name_selfheal_run strih_host active_set scripts_dir` with the given
/// `NDI_NAME_SELFHEAL_CMD` seam, and return (rc, captured stdout).
fn run(seam: &str, host: &str, active: &str, scripts: &str) -> (i32, String) {
    let script = lib();
    assert!(script.exists(), "{} not found", script.display());
    let harness = r#"
set -uo pipefail
. "$SCRIPT"
ndi_name_selfheal_run "$HOST" "$ACTIVE" "$SCRIPTS"
echo "RC:$?"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("HOST", host)
        .env("ACTIVE", active)
        .env("SCRIPTS", scripts)
        .env("NDI_NAME_SELFHEAL_CMD", seam)
        .output()
        .expect("failed to run selfheal harness");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let rc = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RC:"))
        .and_then(|n| n.trim().parse::<i32>().ok())
        .unwrap_or_else(|| panic!("no RC line in: {stdout}"));
    (rc, stdout)
}

#[test]
fn forwards_host_active_and_scripts_to_the_seam() {
    // The seam echoes what it was handed; assert the function passed each through the documented
    // NDI_NAME_SELFHEAL_{HOST,ACTIVE,SCRIPTS} env vars.
    let seam = r#"echo "H=$NDI_NAME_SELFHEAL_HOST A=$NDI_NAME_SELFHEAL_ACTIVE S=$NDI_NAME_SELFHEAL_SCRIPTS"; exit 0"#;
    let (rc, out) = run(seam, "10.77.9.202", "cam2 cam3", "/scr");
    assert_eq!(rc, 0, "healed -> exit 0; got {out}");
    assert!(
        out.contains("H=10.77.9.202 A=cam2 cam3 S=/scr"),
        "seam saw wrong args: {out}"
    );
}

#[test]
fn returns_the_seam_exit_code_healed_zero() {
    let (rc, _) = run("exit 0", "h", "cam2", "/s");
    assert_eq!(rc, 0);
}

#[test]
fn returns_the_seam_exit_code_nothing_healable_nonzero() {
    // set-ndi-mapping.py --heal exits 3 when nothing was healable; the function must propagate it.
    let (rc, _) = run("exit 3", "h", "cam2", "/s");
    assert_eq!(rc, 3);
}

#[test]
fn nonzero_return_in_an_if_does_not_abort_a_set_e_caller_1133() {
    // The [4c/8] caller runs under `set -euo pipefail` and calls this in an `if`-condition, so a
    // non-zero return must NOT abort — the caller's trailing line must still run.
    let script = lib();
    let harness = r#"
set -euo pipefail
. "$SCRIPT"
if ndi_name_selfheal_run "h" "cam2" "/s"; then
  echo "HEALED"
fi
echo "REACHED_TRAILING_LINE"
"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("NDI_NAME_SELFHEAL_CMD", "exit 3") // nothing-healable
        .output()
        .expect("failed to run set-e harness");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "set-e caller aborted on a non-zero heal: {stdout}"
    );
    assert!(
        stdout.contains("REACHED_TRAILING_LINE"),
        "trailing line not reached: {stdout}"
    );
    assert!(
        !stdout.contains("HEALED"),
        "should not claim healed on exit 3: {stdout}"
    );
}
