//! #1083 — pure-function + integration guard for the dev1-side MV-fps alert watchdog
//! (`scripts/lib/mv-fps-health.sh` + `scripts/mv-fps-alert-watchdog.sh`).
//!
//! Root cause (issue 1083, continuation of issue 771): issue 771 shipped the observability CORE —
//! vendored `render_display()` emits `multiview-audit: monitor=N divisor=D rendered_fps=X target=Z
//! floor=F …` ~every 5 s, `src/mv_audit.rs` parses+gates it, and the `mv-fps-gate` bin is an
//! E2E-preflight / drift-guard consumer. But NOTHING read it LIVE — a MV render collapse (measured
//! live: imag monitor-3 to ~12fps for 5 min, strih 4K MV to 9–11fps under contention) went
//! unalarmed unless someone ran `mv-fps-gate` by hand. This watchdog closes that: a dev1-side
//! systemd --user timer reads each box's newest OBS log, runs `mv-fps-gate` over its latest
//! `multiview-audit:` samples, and pages Discord on a sustained below-floor collapse — imag +
//! strih, autostart-aware, with the SAME shared confirm/throttle the issue-391/1001/1052 siblings
//! use. Ships DISABLED; the supervisor enables it after a live verify.
//!
//! Same convention as `tests/harness_frozen_input_health_1052.rs` /
//! `tests/harness_network_reach_health_1001.rs`: source the REAL lib/script (source-only, no side
//! effects) and exercise the pure functions + the sourced watchdog directly. RED before the lib /
//! watchdog exist (sourcing fails, every test fails); GREEN after.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn health_lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/mv-fps-health.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/mv-fps-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL health lib and run `body` against its pure functions. Returns (exit, stdout).
fn run_health(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", health_lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn health_stdout(body: &str) -> String {
    let (rc, out, err) = run_health(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// -------------------------------------------------------------------------------------------
// health lib — shape
// -------------------------------------------------------------------------------------------
#[test]
fn health_lib_defines_the_pure_functions() {
    for f in [
        "mv_fps_verdict",
        "mv_fps_restart_reset",
        "mv_fps_recovery_decision",
        "mv_fps_alert_detail",
    ] {
        let out = health_stdout(&format!("type {f} >/dev/null 2>&1 && echo OK"));
        assert_eq!(out, "OK", "{f} is not defined by the health lib");
    }
}

// -------------------------------------------------------------------------------------------
// mv_fps_verdict — gate exit code -> PASS/BELOW/UNKNOWN (fail-safe on anything but 0/1)
// -------------------------------------------------------------------------------------------
#[test]
fn verdict_maps_gate_exit_codes() {
    assert_eq!(health_stdout("mv_fps_verdict 0"), "PASS");
    assert_eq!(health_stdout("mv_fps_verdict 1"), "BELOW");
    // 2 = no samples / read error -> UNKNOWN, never page.
    assert_eq!(health_stdout("mv_fps_verdict 2"), "UNKNOWN");
    // any other / empty / non-numeric -> fail-safe UNKNOWN.
    assert_eq!(health_stdout("mv_fps_verdict 3"), "UNKNOWN");
    assert_eq!(health_stdout("mv_fps_verdict ''"), "UNKNOWN");
    assert_eq!(health_stdout("mv_fps_verdict x"), "UNKNOWN");
}

// -------------------------------------------------------------------------------------------
// mv_fps_restart_reset — autostart-aware: a CHANGED OBS log identity resets the confirm streak
// -------------------------------------------------------------------------------------------
#[test]
fn restart_reset_only_on_a_changed_nonempty_logid() {
    // OBS restarted (new log file) -> reset the pending confirm (fresh instance warm-up must not
    // inherit a stale below-floor confirmation).
    assert_eq!(health_stdout("mv_fps_restart_reset logA logB"), "reset=1");
    // Same instance -> keep the streak.
    assert_eq!(health_stdout("mv_fps_restart_reset logA logA"), "reset=0");
    // First pass ever (no prior id) -> nothing to reset.
    assert_eq!(health_stdout("mv_fps_restart_reset '' logA"), "reset=0");
    // Current read failed (empty id) -> do NOT reset on a failed read.
    assert_eq!(health_stdout("mv_fps_restart_reset logA ''"), "reset=0");
    assert_eq!(health_stdout("mv_fps_restart_reset '' ''"), "reset=0");
}

// -------------------------------------------------------------------------------------------
// mv_fps_recovery_decision — one "recovered" ping only for a box we actually paged for
// -------------------------------------------------------------------------------------------
#[test]
fn recovery_only_when_was_alerted_and_now_pass() {
    assert_eq!(health_stdout("mv_fps_recovery_decision 1 1"), "recover=1");
    assert_eq!(health_stdout("mv_fps_recovery_decision 1 0"), "recover=0");
    assert_eq!(health_stdout("mv_fps_recovery_decision 0 1"), "recover=0");
    assert_eq!(health_stdout("mv_fps_recovery_decision 0 0"), "recover=0");
    assert_eq!(health_stdout("mv_fps_recovery_decision x y"), "recover=0");
}

// -------------------------------------------------------------------------------------------
// mv_fps_alert_detail — pull the gate's FAIL line(s) into one Discord body line
// -------------------------------------------------------------------------------------------
#[test]
fn alert_detail_extracts_the_gate_fail_lines() {
    let gate_out = "FAIL monitor=1 divisor=1 rendered_fps=9.0 < floor=13.0 (target 30, 3840x2160)";
    let out = health_stdout(&format!(
        "mv_fps_alert_detail imag {}",
        shell_single_quote(gate_out)
    ));
    assert!(out.contains("imag"), "detail must name the box: {out}");
    assert!(
        out.contains("monitor=1") && out.contains("rendered_fps=9.0") && out.contains("floor=13.0"),
        "detail must carry the failing monitor + fps + floor: {out}"
    );
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// -------------------------------------------------------------------------------------------
// watchdog — source-only shape + CLI
// -------------------------------------------------------------------------------------------
fn run_watchdog(args: &[&str], envs: &[(&str, &str)], extra_body: &str) -> (i32, String, String) {
    // Source the watchdog (defines functions, main runs only when executed directly), then run any
    // extra body. Passing NO args to the source keeps `$1` unset so the arg-parse case never fires.
    let harness = format!(
        "set -uo pipefail\n. \"$WD\" {args}\n{extra}",
        args = args
            .iter()
            .map(|a| shell_single_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
        extra = extra_body
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("WD", watchdog())
        .current_dir(manifest_dir());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run watchdog harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn watchdog_sourcing_defines_functions_and_runs_nothing() {
    // Sourcing with no args must NOT run main (no ssh, no alert) and must define the pass fns.
    let (rc, _out, err) = run_watchdog(
        &[],
        &[],
        "type main >/dev/null 2>&1 && type handle_box >/dev/null 2>&1 && echo DEFS",
    );
    assert_eq!(rc, 0, "sourcing failed: {err}");
    let (_rc2, out2, _e2) = run_watchdog(&[], &[], "echo READY");
    // The `DEFS` echo above and this READY prove main did not auto-run its own 'pass start' log.
    assert!(
        out2.contains("READY"),
        "sourcing should be side-effect free"
    );
    let (_rc3, out3, _e3) = run_watchdog(
        &[],
        &[],
        "type main >/dev/null 2>&1 && type handle_box >/dev/null 2>&1 && echo DEFS",
    );
    assert!(
        out3.contains("DEFS"),
        "watchdog must define main + handle_box when sourced"
    );
}

#[test]
fn watchdog_help_exits_zero() {
    let out = Command::new("bash")
        .arg(watchdog())
        .arg("--help")
        .current_dir(manifest_dir())
        .output()
        .expect("run --help");
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.to_lowercase().contains("watchdog"),
        "help should describe the watchdog"
    );
}

// -------------------------------------------------------------------------------------------
// watchdog integration — fake probe + fake gate, per-test state dir, --dry-run (never pages)
// -------------------------------------------------------------------------------------------
//
// The watchdog reads each box's OBS log via `MV_FPS_PROBE_CMD <ip> <os>` (stdout = raw probe text)
// and runs the gate via `MV_FPS_GATE_BIN` (a command reading the audit lines on stdin, exiting
// 0/1/2). Both are overridable exactly so this test can drive the decision core with no ssh and no
// Rust binary. A per-test STATE dir + a single BOX keep passes independent.

fn temp_dir(tag: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!(
        "cbox-mvfps-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fake gate that ignores stdin, prints $FAKE_GATE_OUT, exits $FAKE_GATE_EXIT.
fn write_fake_gate(dir: &Path) -> PathBuf {
    let p = dir.join("fake-gate.sh");
    std::fs::write(
        &p,
        "#!/usr/bin/env bash\ncat >/dev/null 2>&1\n[ -n \"${FAKE_GATE_OUT:-}\" ] && printf '%s\\n' \"$FAKE_GATE_OUT\"\nexit \"${FAKE_GATE_EXIT:-0}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

/// A fake probe that ignores its <ip> <os> args and prints $FAKE_PROBE_OUT.
fn write_fake_probe(dir: &Path) -> PathBuf {
    let p = dir.join("fake-probe.sh");
    std::fs::write(
        &p,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"${FAKE_PROBE_OUT:-}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

fn base_env<'a>(state: &'a str, gate: &'a str, probe: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        // ONE box, so per-pass reasoning is unambiguous.
        ("MV_FPS_BOXES", "imag|10.0.0.1|linux"),
        ("MV_FPS_ALERT_STATE_FILE", state),
        ("MV_FPS_GATE_BIN", gate),
        ("MV_FPS_PROBE_CMD", probe),
        // Confirm 2 (default) — one below-floor pass must NOT page.
        ("MV_FPS_ALERT_CONFIRM_THRESHOLD", "2"),
        // Empty netreach state -> box treated reachable (no double-page skip).
        ("MV_FPS_NETREACH_STATE_FILE", "/nonexistent/netreach.state"),
    ]
}

#[test]
fn below_floor_needs_two_confirms_before_it_would_page() {
    let dir = temp_dir("confirm");
    let gate = write_fake_gate(&dir);
    let probe = write_fake_probe(&dir);
    let state = dir.join("state").to_string_lossy().into_owned();
    let gate_s = gate.to_string_lossy().into_owned();
    let probe_s = probe.to_string_lossy().into_owned();
    let below_log = "MVFPS_LOGID:log-1\n00:00:00.000: multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=13.0 cx=3840 cy=2160";

    let mut env = base_env(&state, &gate_s, &probe_s);
    env.push(("FAKE_GATE_EXIT", "1"));
    env.push((
        "FAKE_GATE_OUT",
        "FAIL monitor=1 rendered_fps=9.0 < floor=13.0",
    ));
    env.push(("FAKE_PROBE_OUT", below_log));

    // Pass 1: below floor, but only 1 confirm -> HOLD, never "WOULD alert".
    let (rc1, _o1, e1) = run_watchdog(&["--dry-run"], &env, "main");
    assert_eq!(rc1, 0, "pass1 failed: {e1}");
    assert!(
        !e1.contains("WOULD alert"),
        "pass1 must not page on a single below-floor read:\n{e1}"
    );

    // Pass 2: still below floor, same log id -> confirmed -> WOULD alert (dry-run only logs).
    let (rc2, _o2, e2) = run_watchdog(&["--dry-run"], &env, "main");
    assert_eq!(rc2, 0, "pass2 failed: {e2}");
    assert!(
        e2.contains("WOULD alert"),
        "pass2 must page once confirmed across 2 passes:\n{e2}"
    );
}

#[test]
fn healthy_pass_never_pages() {
    let dir = temp_dir("healthy");
    let gate = write_fake_gate(&dir);
    let probe = write_fake_probe(&dir);
    let state = dir.join("state").to_string_lossy().into_owned();
    let gate_s = gate.to_string_lossy().into_owned();
    let probe_s = probe.to_string_lossy().into_owned();
    let ok_log = "MVFPS_LOGID:log-1\n00:00:00.000: multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=3840 cy=2160";

    let mut env = base_env(&state, &gate_s, &probe_s);
    env.push(("FAKE_GATE_EXIT", "0"));
    env.push(("FAKE_PROBE_OUT", ok_log));

    for _ in 0..3 {
        let (rc, _o, e) = run_watchdog(&["--dry-run"], &env, "main");
        assert_eq!(rc, 0, "healthy pass failed: {e}");
        assert!(
            !e.contains("WOULD alert"),
            "a healthy (PASS) pass must never page:\n{e}"
        );
    }
}

#[test]
fn an_obs_restart_between_passes_resets_the_confirm_streak() {
    // Pass 1 below-floor on log-1, then OBS restarts (log-2) and pass 2 is ALSO below-floor: the
    // restart-reset means pass 2 starts a FRESH streak (confirm 1), so it must NOT page yet — a
    // fresh OBS start's warm-up below-floor read can never page on its own.
    let dir = temp_dir("restart");
    let gate = write_fake_gate(&dir);
    let probe = write_fake_probe(&dir);
    let state = dir.join("state").to_string_lossy().into_owned();
    let gate_s = gate.to_string_lossy().into_owned();
    let probe_s = probe.to_string_lossy().into_owned();

    let mut env = base_env(&state, &gate_s, &probe_s);
    env.push(("FAKE_GATE_EXIT", "1"));
    env.push((
        "FAKE_GATE_OUT",
        "FAIL monitor=1 rendered_fps=9.0 < floor=13.0",
    ));

    // Pass 1: below on log-1 (confirm -> 1).
    let mut env1 = env.clone();
    env1.push(("FAKE_PROBE_OUT", "MVFPS_LOGID:log-1\n00: multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=13.0 cx=3840 cy=2160"));
    let (_r1, _o1, e1) = run_watchdog(&["--dry-run"], &env1, "main");
    assert!(!e1.contains("WOULD alert"), "pass1: {e1}");

    // Pass 2: below on log-2 (OBS restarted) -> restart-reset -> confirm restarts at 1 -> HOLD.
    let mut env2 = env.clone();
    env2.push(("FAKE_PROBE_OUT", "MVFPS_LOGID:log-2\n00: multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=13.0 cx=3840 cy=2160"));
    let (_r2, _o2, e2) = run_watchdog(&["--dry-run"], &env2, "main");
    assert!(
        !e2.contains("WOULD alert"),
        "an OBS restart must reset the confirm streak so a fresh-start below-floor read never pages:\n{e2}"
    );
}

#[test]
fn an_unreadable_box_is_unknown_and_never_pages() {
    // Empty probe output (ssh/log read failed) -> UNKNOWN -> the reachability/#882 watchdogs own a
    // down box; MV-fps must never page on it, even repeatedly.
    let dir = temp_dir("unknown");
    let gate = write_fake_gate(&dir);
    let probe = write_fake_probe(&dir);
    let state = dir.join("state").to_string_lossy().into_owned();
    let gate_s = gate.to_string_lossy().into_owned();
    let probe_s = probe.to_string_lossy().into_owned();

    let mut env = base_env(&state, &gate_s, &probe_s);
    env.push(("FAKE_PROBE_OUT", "")); // read failed

    for _ in 0..3 {
        let (rc, _o, e) = run_watchdog(&["--dry-run"], &env, "main");
        assert_eq!(rc, 0, "unknown pass failed: {e}");
        assert!(
            !e.contains("WOULD alert"),
            "an unreadable box must never page:\n{e}"
        );
    }
}
