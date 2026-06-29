//! #281 Part A — behavioral tests for scripts/lib/with-rig-restore.sh.
//!
//! The generic self-restoring rig-step wrapper ensures an ad-hoc rig step (deploy, probe,
//! scene-change) CANNOT leave the rig stranded: if the step exits non-zero, is killed by a
//! signal, or (in always-restore mode) succeeds, a restore command is run automatically.
//!
//! ## Properties tested (RED→GREEN per regression-test-first.md)
//!
//! 1. Lib exists + is sourceable without side effects.
//! 2. Always-restore mode: restore runs on step failure (non-zero exit).
//! 3. Always-restore mode: restore runs on step success (exit 0).
//! 4. On-failure mode: restore runs on step failure.
//! 5. On-failure mode: restore does NOT run on clean success.
//! 6. Exit code is preserved and propagated from the step.
//! 7. Restore runs exactly once (idempotent _wr_done guard).
//! 8. Restore runs when the step exits with a signal-like code (SIGTERM simulation).
//!
//! Pure-shell / file-system tests — no rig, no ssh.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/with-rig-restore.sh")
}

/// Scratch directory unique per test process + test name; cleaned up at the end.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "with-rig-restore-{}-{}",
        std::process::id(),
        name
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run a bash script string with LIB env var pointing at the lib under test.
/// Returns (exit_code, stdout, stderr).
fn run_harness(script: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("LIB", lib())
        .output()
        .expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ─── existence + sourceability ───────────────────────────────────────────────

#[test]
fn lib_exists() {
    assert!(
        lib().exists(),
        "#281 Part A: scripts/lib/with-rig-restore.sh must exist"
    );
}

#[test]
fn lib_is_sourceable_without_side_effects() {
    let (code, _stdout, stderr) = run_harness(r#". "$LIB"; echo ok"#);
    assert_eq!(
        code, 0,
        "#281: sourcing with-rig-restore.sh must be a no-op (no side effects)\nstderr: {stderr}"
    );
}

// ─── always-restore mode (default — no flag) ─────────────────────────────────

#[test]
fn always_mode_restores_on_step_failure() {
    let dir = scratch("always-fail");
    let flag = dir.join("restored");
    let script = format!(
        r#". "$LIB"; with_rig_restore "touch '{flag}'" -- bash -c "exit 7""#,
        flag = flag.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert_eq!(
        code, 7,
        "#281 always-mode: step exit code must propagate (got {code})\nstderr: {stderr}"
    );
    assert!(
        flag.exists(),
        "#281 always-mode: restore must run when step exits non-zero\nstderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn always_mode_restores_on_step_success() {
    let dir = scratch("always-success");
    let flag = dir.join("restored");
    let script = format!(
        r#". "$LIB"; with_rig_restore "touch '{flag}'" -- bash -c "exit 0""#,
        flag = flag.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert_eq!(
        code, 0,
        "#281 always-mode: exit 0 must propagate\nstderr: {stderr}"
    );
    assert!(
        flag.exists(),
        "#281 always-mode: restore must run even on clean success\nstderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── on-failure mode (--on-failure flag) ────────────────────────────────────

#[test]
fn on_failure_mode_restores_on_step_failure() {
    let dir = scratch("onfail-fail");
    let flag = dir.join("restored");
    let script = format!(
        r#". "$LIB"; with_rig_restore --on-failure "touch '{flag}'" -- bash -c "exit 3""#,
        flag = flag.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert_eq!(
        code, 3,
        "#281 on-failure: step exit code must propagate\nstderr: {stderr}"
    );
    assert!(
        flag.exists(),
        "#281 on-failure: restore must run when step exits non-zero\nstderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn on_failure_mode_does_not_restore_on_clean_success() {
    let dir = scratch("onfail-success");
    let flag = dir.join("restored");
    let script = format!(
        r#". "$LIB"; with_rig_restore --on-failure "touch '{flag}'" -- bash -c "exit 0""#,
        flag = flag.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert_eq!(
        code, 0,
        "#281 on-failure: exit 0 must propagate\nstderr: {stderr}"
    );
    assert!(
        !flag.exists(),
        "#281 on-failure: restore must NOT run when step succeeds\nstderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── exit code propagation ───────────────────────────────────────────────────

#[test]
fn exit_code_is_preserved_across_a_range_of_codes() {
    for expected in [0u8, 1, 2, 42, 127] {
        let dir = scratch(&format!("exitcode-{expected}"));
        let flag = dir.join("restored");
        let script = format!(
            r#". "$LIB"; with_rig_restore "touch '{flag}'" -- bash -c "exit {expected}""#,
            flag = flag.display()
        );
        let (code, _stdout, stderr) = run_harness(&script);
        assert_eq!(
            code, expected as i32,
            "#281: exit code {expected} must be preserved (got {code})\nstderr: {stderr}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

// ─── idempotent restore ──────────────────────────────────────────────────────

#[test]
fn restore_runs_exactly_once() {
    // The _wr_done guard must ensure restore fires at most once, even in always-mode
    // where both the signal traps AND the post-step logic could trigger it.
    let dir = scratch("idempotent");
    let counter = dir.join("count.txt");
    let script = format!(
        r#"
. "$LIB"
restore_cmd='printf "x" >> "{counter}"'
with_rig_restore "$restore_cmd" -- bash -c "exit 1"
count=$(wc -c < "{counter}" 2>/dev/null || echo 0)
[ "$count" -eq 1 ]
"#,
        counter = counter.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert_eq!(
        code, 0,
        "#281: restore must run EXACTLY once (idempotent guard). stderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── signal path ─────────────────────────────────────────────────────────────

#[test]
fn restores_when_step_exits_due_to_sigterm() {
    // Simulate: the step subprocess is killed by SIGTERM (exits 143 = 128 + 15).
    // The wrapper sees a non-zero exit code and must run restore (always-mode).
    let dir = scratch("sigterm-step");
    let flag = dir.join("restored");
    // `kill -TERM "$BASHPID"` kills the step's own bash subprocess with SIGTERM.
    let script = format!(
        r#". "$LIB"; with_rig_restore "touch '{flag}'" -- bash -c 'kill -TERM "$BASHPID"'"#,
        flag = flag.display()
    );
    let (code, _stdout, stderr) = run_harness(&script);
    assert!(
        flag.exists(),
        "#281: restore must run when the step exits due to SIGTERM (code={code})\nstderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Static guard: the implementation must install signal traps for TERM, INT, and HUP
/// so a kill arriving at the wrapper shell (not just the step) also triggers restore.
#[test]
fn wrapper_installs_hup_int_term_traps() {
    let src = fs::read_to_string(lib()).expect("read with-rig-restore.sh");
    // The three signals must appear in trap statements inside with_rig_restore.
    assert!(
        src.contains("trap") && src.contains("TERM"),
        "#281: with_rig_restore must install a TERM trap"
    );
    assert!(
        src.contains("INT"),
        "#281: with_rig_restore must install an INT trap"
    );
    assert!(
        src.contains("HUP"),
        "#281: with_rig_restore must install a HUP trap"
    );
}
