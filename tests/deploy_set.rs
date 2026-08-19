//! Tier-0 guard for `scripts/lib/deploy-set.sh` (#1136) — the "active set MINUS acked-offline"
//! composition the push-to-main auto-deploy CI job uses to choose which cam boxes to deploy to.
//! Delegates the membership test to `cambox-offline-ack.sh` (the SAME exclusion mechanism the
//! version/parity gates use), so these tests also lock that it is an EXACT-name match, never a
//! substring, and that an all-acked set collapses to empty (deploy nothing) rather than erroring.

use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    let s = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/deploy-set.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the lib (a pure source-only file) and run `body`, returning stdout.
fn run(body: &str, env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn all_active_kept_when_none_acked() {
    let out = run(r#"deploy_set_active_minus_acked "cam1 cam2 cam3" """#, &[]);
    assert_eq!(out.trim(), "cam1 cam2 cam3");
}

#[test]
fn acked_box_dropped_via_explicit_arg() {
    let out = run(
        r#"deploy_set_active_minus_acked "cam1 cam2 cam3" "cam2:powered-off""#,
        &[],
    );
    assert_eq!(out.trim(), "cam1 cam3");
}

#[test]
fn acked_box_dropped_via_ambient_env() {
    let out = run(
        r#"deploy_set_active_minus_acked "cam1 cam2""#,
        &[("CAMBOX_OFFLINE_ACK", "cam1:on-air-elsewhere")],
    );
    assert_eq!(out.trim(), "cam2");
}

#[test]
fn exact_name_match_never_substring() {
    // cam7 acked must NOT drop cam70 (exact-name match, mirroring cambox_offline_ack_is_acked).
    let out = run(
        r#"deploy_set_active_minus_acked "cam7 cam70" "cam7:off""#,
        &[],
    );
    assert_eq!(out.trim(), "cam70");
}

#[test]
fn all_acked_yields_empty_set() {
    // Every active box acked -> deploy nothing (the caller then deploys to no box, never an error).
    let out = run(
        r#"deploy_set_active_minus_acked "cam1 cam2" "cam1:a,cam2:b""#,
        &[],
    );
    assert_eq!(out.trim(), "");
}
