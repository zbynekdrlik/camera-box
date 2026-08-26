//! issue 1171 — every imag OBS leg in `scripts/rig-mode.sh` (`toggle_burn`, `set_imag_test_program`,
//! `event_mode_assert` item-3) must SKIP a legitimately-absent imag — operator-acked offline
//! (issue 1013 `rig-fleet.txt` / `CAMBOX_OFFLINE_ACK`) AND unreachable — instead of aborting the
//! whole TEST/EVENT switch on `OSError: [Errno 113] No route to host` (the live 2026-08-23 aborts on
//! `toggle_burn test` / `toggle_burn event`). A reachable-but-acked (stale ack) or not-acked imag is
//! NOT skipped — it runs the leg fail-closed exactly as before. Same seam the already-merged #789
//! `require_imag_genlock_current` gate uses; this extends it to the remaining sites.
//!
//! These tests source the REAL script (its `BASH_SOURCE != $0` guard skips main) and either read its
//! text (static anchors) or run a leg with a fake `python3`/`ping` on `PATH` (functional) — no rig.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/rig-mode.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn script_text() -> String {
    fs::read_to_string(script()).expect("read rig-mode.sh")
}

/// Write an executable fake at `dir/name` with the given bash body.
fn write_fake(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    fs::write(&p, body).expect("write fake");
    let mut perm = fs::metadata(&p).expect("stat fake").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&p, perm).expect("chmod fake");
}

/// Source rig-mode.sh (main skipped) with a fake `python3` (logging its argv) and, optionally, a
/// fake `ping` (exit `ping_rc`) prepended to PATH, run `body`, and return (stdout, python-call-log).
fn run_with_fakes(body: &str, env: &[(&str, &str)], ping_rc: Option<&str>) -> (String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("mkdir bin");
    let pylog = dir.path().join("pycalls.log");
    write_fake(
        &bin,
        "python3",
        &format!(
            "#!/usr/bin/env bash\necho \"PYCALL: $*\" >> \"{}\"\necho ok\n",
            pylog.display()
        ),
    );
    if let Some(rc) = ping_rc {
        write_fake(&bin, "ping", &format!("#!/usr/bin/env bash\nexit {rc}\n"));
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // `set +e` after the source neutralizes the sourced script's own `set -euo pipefail` leaking into
    // the harness (the run_sourced -e-leak documented in .claude/rules/ci-testing-gotchas.md).
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\nset +e\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .env("PATH", path);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    let log = fs::read_to_string(&pylog).unwrap_or_default();
    (String::from_utf8_lossy(&out.stdout).into_owned(), log)
}

// ---- static wiring anchors ------------------------------------------------------------------- //

#[test]
fn resolve_imag_offline_leg_is_defined_and_wired_into_both_paths() {
    let s = script_text();
    assert!(
        s.contains("resolve_imag_offline_leg()"),
        "1171: rig-mode.sh must define resolve_imag_offline_leg (the ONE per-switch imag offline-leg \
         decision)"
    );
    // It must delegate the skip/proceed decision to the already-tested pure function + probe reachability.
    let def = s
        .find("resolve_imag_offline_leg() {")
        .expect("resolve_imag_offline_leg must be defined");
    let body_end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    let body = &s[def..body_end];
    assert!(
        body.contains("cambox_offline_ack_reason") && body.contains("ping "),
        "1171: resolve_imag_offline_leg must read the imag ack + probe reachability. Got:\n{body}"
    );
    assert!(
        body.contains("imag_genlock_gate_offline_ack_action"),
        "1171: resolve_imag_offline_leg must reuse the tested pure decision function. Got:\n{body}"
    );
    // Called from BOTH do_test and do_event so every downstream imag leg reads the flag.
    let do_test = s
        .split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("");
    assert!(
        do_test.contains("resolve_imag_offline_leg"),
        "1171: do_test must call resolve_imag_offline_leg. Got:\n{do_test}"
    );
    let do_event = s
        .split("do_event()")
        .nth(1)
        .unwrap_or("")
        .split("\nmain()")
        .next()
        .unwrap_or("");
    assert!(
        do_event.contains("resolve_imag_offline_leg"),
        "1171: do_event must call resolve_imag_offline_leg. Got:\n{do_event}"
    );
}

#[test]
fn event_mode_assert_guards_both_imag_burn_legs() {
    let s = script_text();
    let def = s
        .find("event_mode_assert() {")
        .expect("event_mode_assert must be defined");
    let body_end = s[def..]
        .find("\ndo_event() {")
        .map(|i| def + i)
        .unwrap_or(s.len());
    let body = &s[def..body_end];
    // The pinned burn-check imag leg guard.
    assert!(
        body.contains("[ \"$box\" = imag ] && [ \"${IMAG_OFFLINE_ACKED:-0}\" = 1 ]"),
        "1171: event_mode_assert's pinned burn-check must skip an acked-offline imag. Got:\n{body}"
    );
    // The exhaustive sweep-check imag leg guard.
    assert!(
        body.contains("[ \"$_asbbox\" = imag ] && [ \"${IMAG_OFFLINE_ACKED:-0}\" = 1 ]"),
        "1171: event_mode_assert's sweep-check must skip an acked-offline imag. Got:\n{body}"
    );
    // The fail-closed sentinel for a NON-acked un-enumerable box must still exist (unchanged).
    assert!(
        body.contains("__sweep_unreachable__"),
        "1171: the fail-closed sweep sentinel must remain for the non-acked case. Got:\n{body}"
    );
}

// ---- functional: the imag leg is skipped only when acked+flagged ----------------------------- //

#[test]
fn toggle_burn_skips_imag_when_acked_offline_runs_strih_stream() {
    let (stdout, log) = run_with_fakes(
        "toggle_burn test",
        &[
            ("IMAG_OFFLINE_ACKED", "1"),
            ("IMAG_OFFLINE_ACK_REASON", "notebook-replacement-test"),
        ],
        None,
    );
    assert!(
        log.contains("10.77.9.202") && log.contains("10.77.9.204"),
        "1171: strih + stream burns must still run. python calls:\n{log}\nstdout:\n{stdout}"
    );
    assert!(
        !log.contains("10.77.9.182"),
        "1171: the imag (.182) burn leg must be SKIPPED when acked-offline. python calls:\n{log}"
    );
    assert!(
        stdout.contains("[imag burn] SKIP") && stdout.contains("1013"),
        "1171: the imag skip must be logged loudly, citing issue 1013. stdout:\n{stdout}"
    );
}

#[test]
fn toggle_burn_event_sweep_off_skips_imag_when_acked_offline() {
    let (stdout, log) = run_with_fakes(
        "toggle_burn event",
        &[
            ("IMAG_OFFLINE_ACKED", "1"),
            ("IMAG_OFFLINE_ACK_REASON", "notebook-replacement-test"),
        ],
        None,
    );
    // No imag call in EITHER the pinned remove loop or the exhaustive sweep-off loop.
    assert!(
        !log.contains("10.77.9.182"),
        "1171: neither the pinned remove nor the sweep-off may touch an acked-offline imag. calls:\n{log}"
    );
    assert!(
        stdout.contains("[imag burn-sweep] SKIP"),
        "1171: the imag sweep-off skip must be logged loudly. stdout:\n{stdout}"
    );
}

#[test]
fn toggle_burn_runs_imag_when_not_acked() {
    let (_stdout, log) = run_with_fakes("toggle_burn test", &[("IMAG_OFFLINE_ACKED", "0")], None);
    assert!(
        log.contains("10.77.9.182"),
        "1171: a NOT-acked imag must still run its burn leg (fail-closed as before). calls:\n{log}"
    );
}

#[test]
fn set_imag_test_program_skips_when_acked_offline() {
    let (stdout, log) = run_with_fakes(
        "set_imag_test_program",
        &[
            ("IMAG_OFFLINE_ACKED", "1"),
            ("IMAG_OFFLINE_ACK_REASON", "notebook-replacement-test"),
        ],
        None,
    );
    assert!(
        !log.contains("switch --host 10.77.9.182") && !log.contains("10.77.9.182"),
        "1171: set_imag_test_program must not scene-switch an acked-offline imag. calls:\n{log}"
    );
    assert!(
        stdout.contains("SKIP #462 route PROGRAM") && stdout.contains("1013"),
        "1171: the imag routing skip must be logged loudly, citing issue 1013. stdout:\n{stdout}"
    );
}

// ---- functional: resolve_imag_offline_leg's own decision matrix (fake ping) ------------------- //

#[test]
fn resolve_imag_offline_leg_decision_matrix() {
    // acked + UNREACHABLE (ping exit 1) -> ACKED=1 (the legit issue-1013 offline).
    let (stdout, _) = run_with_fakes(
        "resolve_imag_offline_leg >/dev/null; echo \"ACKED=$IMAG_OFFLINE_ACKED\"",
        &[("CAMBOX_OFFLINE_ACK", "imag:notebook-replacement")],
        Some("1"),
    );
    assert!(
        stdout.contains("ACKED=1"),
        "1171: acked + unreachable -> IMAG_OFFLINE_ACKED=1. stdout:\n{stdout}"
    );
    // acked + REACHABLE (ping exit 0) = STALE ack -> ACKED=0 (falls through, still gated).
    let (stdout, _) = run_with_fakes(
        "resolve_imag_offline_leg >/dev/null; echo \"ACKED=$IMAG_OFFLINE_ACKED\"",
        &[("CAMBOX_OFFLINE_ACK", "imag:notebook-replacement")],
        Some("0"),
    );
    assert!(
        stdout.contains("ACKED=0"),
        "1171: acked but reachable (stale) -> IMAG_OFFLINE_ACKED=0. stdout:\n{stdout}"
    );
    // NOT acked -> ACKED=0.
    let (stdout, _) = run_with_fakes(
        "resolve_imag_offline_leg >/dev/null; echo \"ACKED=$IMAG_OFFLINE_ACKED\"",
        &[("CAMBOX_OFFLINE_ACK", "")],
        Some("1"),
    );
    assert!(
        stdout.contains("ACKED=0"),
        "1171: not-acked -> IMAG_OFFLINE_ACKED=0. stdout:\n{stdout}"
    );
}
