//! #1345 M1b — behavioral guard for `scripts/lib/intercom-target-dropin.sh`, the supervisor tool
//! that repoints ONE dev cambox's VBAN intercom target at the Linux strih-lx hub via a `/run`
//! systemd drop-in (`Environment=CAMERA_BOX_INTERCOM_TARGET=<host>`). The cambox root fs is
//! READ-ONLY, so this env override is the only appliance-side seam. This lib is a SUPERVISOR tool:
//! it is NOT wired into `recording-e2e.sh`/`rig-mode.sh`.
//!
//! Same PURE-BUILDER model as `tests/rig_mode.rs`: source the REAL lib and call its pure
//! `*_cmds` remote-bash builders, asserting the emitted text + the safety properties (the
//! `Environment=` line, `daemon-reload`, `restart camera-box`, every statement `;`-terminated so the
//! `$(...)` newline-strip can never glue the last statement onto a caller's next command, an invalid
//! host failing loud). A functional leg runs the emitted set/clear text against a temp drop-in path
//! with a fake `systemctl` on PATH: the file is written then removed. No ssh, no live rig.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/intercom-target-dropin.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the lib (it is source-only — no `main`) and run `body`, applying the extra env pairs.
/// Returns (exit_code, stdout, stderr). Does NOT assert success — the invalid-host contract
/// intentionally returns non-zero.
fn run_sourced(body: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("LIB", lib());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every non-blank emitted line must be `;`-terminated (the CLAUDE.md `$(...)` trailing-newline
/// gotcha: an unterminated last statement gets glued onto whatever the caller concatenates after
/// `$( ... )`).
fn assert_all_statements_semicolon_terminated(out: &str) {
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            line.trim_end().ends_with(';'),
            "statement not `;`-terminated: {line:?}"
        );
    }
}

const PROD_DROPIN: &str = "/run/systemd/system/camera-box.service.d/zz-intercom-target.conf";

#[test]
fn set_cmds_emit_environment_reload_restart_readback() {
    let (code, out, err) = run_sourced(
        "intercom_target_dropin_set_cmds strih-lx.lan",
        &[("INTERCOM_TARGET_DROPIN", PROD_DROPIN)],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("Environment=CAMERA_BOX_INTERCOM_TARGET=strih-lx.lan"),
        "must set the env var to the host: {out}"
    );
    assert!(
        out.contains("systemctl daemon-reload"),
        "must daemon-reload: {out}"
    );
    assert!(
        out.contains("systemctl restart camera-box"),
        "must restart camera-box: {out}"
    );
    assert!(
        out.contains("CAMERA_BOX_INTERCOM_TARGET=strih-lx.lan"),
        "must read the effective env back: {out}"
    );
    assert!(
        out.contains(PROD_DROPIN),
        "must write the configured drop-in path: {out}"
    );
    assert_all_statements_semicolon_terminated(&out);
}

#[test]
fn clear_cmds_emit_rm_reload_restart() {
    let (code, out, err) = run_sourced(
        "intercom_target_dropin_clear_cmds",
        &[("INTERCOM_TARGET_DROPIN", PROD_DROPIN)],
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(out.contains("rm -f"), "must remove the drop-in file: {out}");
    assert!(
        out.contains(PROD_DROPIN),
        "must target the configured drop-in path: {out}"
    );
    assert!(
        out.contains("systemctl daemon-reload"),
        "must daemon-reload: {out}"
    );
    assert!(
        out.contains("systemctl restart camera-box"),
        "must restart camera-box: {out}"
    );
    assert_all_statements_semicolon_terminated(&out);
}

#[test]
fn invalid_host_fails_nonzero_and_emits_nothing() {
    // whitespace in the host
    let (code, out, _err) = run_sourced("intercom_target_dropin_set_cmds 'bad host'", &[]);
    assert_ne!(code, 0, "a host with whitespace must fail loud");
    assert!(
        out.trim().is_empty(),
        "a rejected host must emit no remote text: {out}"
    );

    // empty host
    let (code2, out2, _e2) = run_sourced("intercom_target_dropin_set_cmds ''", &[]);
    assert_ne!(code2, 0, "an empty host must fail loud");
    assert!(
        out2.trim().is_empty(),
        "a rejected host must emit no remote text: {out2}"
    );

    // quote in the host
    let (code3, _out3, _e3) = run_sourced("intercom_target_dropin_set_cmds \"strih'lx\"", &[]);
    assert_ne!(code3, 0, "a host with a quote must fail loud");
}

#[test]
fn set_then_clear_writes_then_removes_the_dropin_functionally() {
    // A private temp workspace: the drop-in file + a fake `systemctl` on PATH.
    let base = std::env::temp_dir().join(format!(
        "intercom_target_1345_{}_{}",
        std::process::id(),
        line!()
    ));
    let bin = base.join("bin");
    fs::create_dir_all(&bin).expect("mkdir temp bin");
    let dropin = base.join("service.d").join("zz-intercom-target.conf");

    // Fake systemctl: `daemon-reload`/`restart` are no-op successes; `show -p Environment --value`
    // echoes the drop-in's Environment value (like the real tool) so the readback grep is real.
    let fake = bin.join("systemctl");
    fs::write(
        &fake,
        "#!/usr/bin/env bash\n\
         if [ \"$1\" = show ]; then\n\
         \x20 [ -f \"$FAKE_DROPIN\" ] && sed -n 's/^Environment=//p' \"$FAKE_DROPIN\"\n\
         fi\n\
         exit 0\n",
    )
    .expect("write fake systemctl");
    let mut perm = fs::metadata(&fake).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&fake, perm).unwrap();

    let dropin_s = dropin.to_str().unwrap();
    let bin_s = bin.to_str().unwrap();

    // Run the emitted SET text, then assert the file exists with the right content.
    let body = "export PATH=\"$FAKEBIN:$PATH\"\n\
                eval \"$(intercom_target_dropin_set_cmds strih-lx.lan)\"\n\
                test -f \"$INTERCOM_TARGET_DROPIN\" && echo SET_FILE_OK\n\
                grep -q 'Environment=CAMERA_BOX_INTERCOM_TARGET=strih-lx.lan' \"$INTERCOM_TARGET_DROPIN\" && echo SET_CONTENT_OK\n\
                eval \"$(intercom_target_dropin_clear_cmds)\"\n\
                test ! -f \"$INTERCOM_TARGET_DROPIN\" && echo CLEARED_OK\n";
    let (code, out, err) = run_sourced(
        body,
        &[
            ("INTERCOM_TARGET_DROPIN", dropin_s),
            ("FAKE_DROPIN", dropin_s),
            ("FAKEBIN", bin_s),
        ],
    );

    let _ = fs::remove_dir_all(&base);

    assert_eq!(code, 0, "functional run failed. stdout={out}\nstderr={err}");
    assert!(
        out.contains("SET_FILE_OK"),
        "drop-in file not written: {out}"
    );
    assert!(
        out.contains("SET_CONTENT_OK"),
        "drop-in content wrong: {out}"
    );
    assert!(
        out.contains("CLEARED_OK"),
        "drop-in file not removed by clear: {out}"
    );
}
