//! issue 1317 — `scripts/lib/ndi-runtime.sh` is the ONE shared NDI 6.3.2 runtime install recipe,
//! reused by `setup-imag.sh` (step 10) and `setup-strih.sh` (its NDI step) so the two never drift.
//!
//! Tier-0 (no cargo compile of the appliance): these SOURCE the source-only lib and assert the TEXT
//! `ndi_runtime_install_cmds` EMITS (it prints on-box statements for the caller to `eval`, the same
//! pattern as `strih_lx_chrome_sandbox_fix_cmd`), plus that the emitted recipe is valid bash. Same
//! `run_sourced` convention as `tests/strih_provision_pure_functions.rs`.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/ndi-runtime.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the lib and run `body`; returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn lib_is_source_only_and_defines_the_emitter() {
    let (code, _o, err) = run_sourced("type ndi_runtime_install_cmds >/dev/null");
    assert_eq!(code, 0, "sourcing the lib must succeed; stderr={err}");
}

/// The emitted recipe carries the fleet-convention `/usr/local/lib/libndi.so.6` symlink DistroAV's
/// Linux loader scans — without it the plugin loads UI-only with ERR-404 (live on imag-nb + strih-lx).
#[test]
fn emits_the_usr_local_lib_symlink_distroav_scans() {
    let (code, out, _e) = run_sourced("ndi_runtime_install_cmds 10.77.9.61 secretpw newlevel");
    assert_eq!(code, 0);
    assert!(
        out.contains("/usr/local/lib/libndi.so.6"),
        "the emitted recipe must create the /usr/local/lib/libndi.so.6 symlink: {out}"
    );
    assert!(
        out.contains(
            "ln -sf \"$(readlink -f \"$__ndir\"/libndi.so.6)\" /usr/local/lib/libndi.so.6"
        ),
        "the symlink must resolve the real libndi.so.6 target: {out}"
    );
}

/// The recipe normalizes the copied runtime files root:root a+rX — a 0600 copy gives the desktop obs
/// user `Permission denied` at dlopen (the issue-1236 / issue-1317 perms shape).
#[test]
fn emits_the_root_root_a_plus_rx_perms_normalize() {
    let (_c, out, _e) = run_sourced("ndi_runtime_install_cmds 10.77.9.61 secretpw newlevel");
    assert!(
        out.contains("chown root:root \"$__ndir\"/libndi.so*"),
        "the emitted recipe must chown the runtime root:root: {out}"
    );
    assert!(
        out.contains("chmod a+rX \"$__ndir\"/libndi.so*"),
        "the emitted recipe must chmod the runtime a+rX (never a 0600 dlopen-blocking copy): {out}"
    );
}

/// The recipe writes the ld path + `ldconfig`, but NEVER `grep -q` on the pipe — `-q`'s early close
/// SIGPIPEs `ldconfig` under the caller's `set -o pipefail` (the documented step-4 footgun).
#[test]
fn emits_ldconfig_with_no_grep_q_sigpipe_footgun() {
    let (_c, out, _e) = run_sourced("ndi_runtime_install_cmds 10.77.9.61 secretpw newlevel");
    assert!(
        out.contains("/etc/ld.so.conf.d/ndi.conf"),
        "the emitted recipe must register /etc/ld.so.conf.d/ndi.conf: {out}"
    );
    assert!(
        out.contains("ldconfig -p | grep libndi >/dev/null"),
        "the linker-cache check must read the FULL output (grep …>/dev/null), never `grep -q`: {out}"
    );
    assert!(
        !out.contains("grep -q"),
        "the recipe must NOT use `grep -q` on the ldconfig pipe (SIGPIPE under pipefail): {out}"
    );
    assert!(
        out.contains("avahi-daemon"),
        "the emitted recipe must install/enable avahi-daemon (mDNS NDI discovery): {out}"
    );
}

/// The peer, user and password are baked into the emitted copy statement.
#[test]
fn bakes_the_peer_user_and_password_into_the_copy() {
    let (_c, out, _e) = run_sourced("ndi_runtime_install_cmds 10.77.9.63 hunter2 david");
    assert!(
        out.contains("10.77.9.63"),
        "the peer must be baked in: {out}"
    );
    assert!(
        out.contains("david@10.77.9.63"),
        "the scp login must be user@peer: {out}"
    );
    assert!(
        out.contains("hunter2"),
        "the password must be baked into the sshpass call: {out}"
    );
    // The copy is idempotent: it only fetches when the runtime is not already present.
    assert!(
        out.contains("if [ ! -e \"$__ndir/libndi.so.6\" ]; then"),
        "the fetch must be guarded on the runtime being absent (idempotent): {out}"
    );
}

/// The emitted recipe must be VALID bash — the glue-check that a mid-string `$(...)` embedding never
/// merged two statements (the `scripts/lib/v4l2-neutral.sh` `_cmd`-helper gotcha).
#[test]
fn emitted_recipe_is_valid_bash() {
    let (code, _o, err) =
        run_sourced("ndi_runtime_install_cmds 10.77.9.61 'pw with spaces' newlevel | bash -n");
    assert_eq!(
        code, 0,
        "the emitted recipe must parse as bash (bash -n); stderr={err}"
    );
}

/// A default NDI dir (/usr/lib/ndi) is used when the 4th arg is omitted; the 4th arg overrides it.
#[test]
fn ndi_dir_defaults_to_usr_lib_ndi_and_is_overridable() {
    let (_c, out, _e) = run_sourced("ndi_runtime_install_cmds 10.77.9.61 pw newlevel");
    assert!(
        out.contains("__ndir=/usr/lib/ndi"),
        "default NDI dir must be /usr/lib/ndi: {out}"
    );
    let (_c2, out2, _e2) = run_sourced("ndi_runtime_install_cmds 10.77.9.61 pw newlevel /opt/ndi");
    assert!(
        out2.contains("__ndir=/opt/ndi"),
        "the 4th arg must override the NDI dir: {out2}"
    );
}
