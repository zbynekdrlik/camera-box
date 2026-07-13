//! #723 — the rig-test LEDGER: anything a test/worker starts on the rig (a painter, a burn, an
//! override) MUST register durably, so `rig-mode.sh event` can kill/clear it BY LEDGER — never by
//! guessing a process NAME PATTERN, which the 2026-07-12 incident (#721) proved is not enough: a
//! worker launched a RENAMED painter (`cam2-painter`, 24h duration) that every name-based cleanup
//! missed. A ledger tracks by PID/unit, which cannot be evaded by a rename.
//!
//! These tests source the REAL `scripts/lib/rig-test-ledger.sh` (never re-implement the logic)
//! and, for the terminate path, actually SPAWN real child processes (one cooperative, one that
//! ignores SIGTERM) under ARBITRARY/renamed argv[0] — the exact #721 incident class — and prove
//! the ledger cleanup kills them by PID regardless of their process name.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::sleep;
use std::time::Duration;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lib/rig-test-ledger.sh")
}

struct Run {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_sourced(body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", script());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Pure calc: the max-duration safety cap.
// ---------------------------------------------------------------------------

#[test]
fn default_cap_is_3600_seconds() {
    let r = run_sourced("rig_test_ledger_default_max_duration_secs");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "3600");
}

#[test]
fn effective_max_duration_clamps_without_a_reason() {
    let r = run_sourced("rig_test_ledger_effective_max_duration 86400");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(
        r.stdout, "3600",
        "an un-reasoned 24h request must clamp to the 3600s safety cap (the #721 incident class)"
    );
}

#[test]
fn effective_max_duration_honors_an_explicit_reason() {
    let r = run_sourced(
        "rig_test_ledger_effective_max_duration 7200 'rig-mode TEST measurement window'",
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(
        r.stdout, "7200",
        "a JUSTIFIED override (a reason given) must pass through verbatim, even above the cap"
    );
}

#[test]
fn effective_max_duration_passes_through_short_requests_unchanged() {
    let r = run_sourced("rig_test_ledger_effective_max_duration 120");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "120");
}

// ---------------------------------------------------------------------------
// Pure calc: expiry.
// ---------------------------------------------------------------------------

#[test]
fn is_expired_true_once_start_plus_max_duration_has_passed() {
    // start=1000, max_duration=3600 -> expires at 4600; now=4600 is AT the boundary (>=).
    let r = run_sourced("rig_test_ledger_is_expired 1000 3600 4600");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "1");
}

#[test]
fn is_expired_false_before_the_boundary() {
    let r = run_sourced("rig_test_ledger_is_expired 1000 3600 4599");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "0");
}

// ---------------------------------------------------------------------------
// Pure builder: the JSONL entry shape.
// ---------------------------------------------------------------------------

#[test]
fn entry_json_carries_every_required_field() {
    let r = run_sourced(
        "rig_test_ledger_entry_json 'frame-probe (TEST)' 12345 cam2 'rig-mode.sh test' 3600 1000000",
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    for expect in [
        "\"what\":\"frame-probe (TEST)\"",
        "\"pid_or_unit\":\"12345\"",
        "\"box\":\"cam2\"",
        "\"started_by\":\"rig-mode.sh test\"",
        "\"max_duration_secs\":3600",
        "\"start_epoch\":1000000",
    ] {
        assert!(
            r.stdout.contains(expect),
            "entry JSON missing {expect:?}, got: {}",
            r.stdout
        );
    }
}

// ---------------------------------------------------------------------------
// Real execution: register -> read -> clear, against a tmp ledger file (no real ssh needed --
// the remote-command builders are plain bash, directly executable locally).
// ---------------------------------------------------------------------------

#[test]
fn register_read_clear_round_trip_against_a_real_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = tmp.path().join("rig-tests.jsonl");

    let register = run_sourced(&format!(
        "eval \"$(rig_test_ledger_register_remote_cmds 'frame-probe (TEST)' 999 cam2 'rig-mode.sh test' 3600 {:?})\"",
        ledger.display()
    ));
    assert_eq!(register.exit_code, 0, "stderr={}", register.stderr);
    let contents = fs::read_to_string(&ledger).expect("ledger file must exist after registration");
    assert!(
        contents.contains("\"pid_or_unit\":\"999\""),
        "ledger file content: {contents}"
    );
    assert!(
        contents.contains("\"box\":\"cam2\""),
        "ledger file content: {contents}"
    );

    let read = run_sourced(&format!(
        "eval \"$(rig_test_ledger_read_remote_cmds {:?})\"",
        ledger.display()
    ));
    assert_eq!(read.exit_code, 0, "stderr={}", read.stderr);
    assert!(read.stdout.contains("\"pid_or_unit\":\"999\""));

    let clear = run_sourced(&format!(
        "eval \"$(rig_test_ledger_clear_remote_cmds {:?})\"",
        ledger.display()
    ));
    assert_eq!(clear.exit_code, 0, "stderr={}", clear.stderr);
    let after_clear = fs::read_to_string(&ledger).unwrap_or_default();
    assert!(
        after_clear.trim().is_empty(),
        "ledger must be EMPTY (not deleted) after clear, got: {after_clear:?}"
    );
}

#[test]
fn read_of_a_missing_ledger_is_empty_not_an_error() {
    let r = run_sourced(
        "eval \"$(rig_test_ledger_read_remote_cmds /nonexistent/path/rig-tests.jsonl)\"",
    );
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert_eq!(r.stdout, "");
}

// ---------------------------------------------------------------------------
// THE #721 fixture: a renamed-binary ledger entry MUST be fully cleaned. Real child processes,
// spawned under an ARBITRARY argv[0] (mirroring the incident's renamed `cam2-painter` painter),
// killed BY PID via the ledger's terminate builder -- proving the cleanup does not care what the
// process calls itself.
// ---------------------------------------------------------------------------

/// A spawned fixture process, kept PROMPTLY reaped by a background thread that polls
/// `try_wait()` every 20ms for as long as the process lives (bounded to 10s as a safety valve).
///
/// WHY this exists: `rig_test_ledger_terminate_entry_cmds`'s own `kill -0 "$target"` polling
/// loop checks process EXISTENCE, and a zombie (a dead-but-not-yet-reaped child) STILL counts as
/// "exists" to `kill -0` until its ACTUAL PARENT calls `wait()`/`waitpid()` on it. In a real
/// deployment the target's parent is a login shell / systemd / init, all of which reap promptly
/// — so `kill -0` correctly reflects death within milliseconds there. But a bare
/// `std::process::Command::spawn()` in a test does NOT auto-reap; if the test only calls
/// `try_wait()` once at the very end, the terminate script's OWN `kill -0` loop spins through
/// its entire ~5s escalation window seeing a live-looking zombie and WRONGLY reports
/// `KILL_NEEDED=1` even for a process that died from the very first SIGTERM (confirmed via
/// `ExitStatus`'s raw wait-status decoding signal 15 = SIGTERM, not 9 = SIGKILL, when this
/// artifact was root-caused). This background reaper closes that test-only gap so the fixture
/// behaves like a REAL rig process for the purposes of this test, without changing anything
/// about how `rig_test_ledger_terminate_entry_cmds` itself works.
struct Fixture {
    pid: u32,
    child: Arc<Mutex<Child>>,
    alive: Arc<Mutex<bool>>,
}

impl Fixture {
    fn spawn(child: Child) -> Self {
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let alive = Arc::new(Mutex::new(true));
        let child_bg = Arc::clone(&child);
        let alive_bg = Arc::clone(&alive);
        thread::spawn(move || {
            for _ in 0..500 {
                // 500 * 20ms = 10s safety bound
                {
                    let mut c = child_bg.lock().expect("reaper: child mutex poisoned");
                    if matches!(c.try_wait(), Ok(Some(_))) {
                        *alive_bg.lock().expect("reaper: alive mutex poisoned") = false;
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Self { pid, child, alive }
    }

    fn is_alive(&self) -> bool {
        *self.alive.lock().expect("alive mutex poisoned")
    }

    fn force_kill(&self) {
        let mut c = self.child.lock().expect("child mutex poisoned");
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Spawn `bash -c 'exec -a NAME sleep 9999'` (a cooperative long-lived process under an
/// arbitrary renamed argv[0]), promptly-reaped (see `Fixture`).
fn spawn_renamed_cooperative(name: &str) -> Fixture {
    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg(format!("exec -a '{name}' sleep 9999"))
        .spawn()
        .expect("spawn renamed sleep");
    Fixture::spawn(child)
}

/// Spawn a renamed process that IGNORES SIGTERM (forces the terminate builder to escalate to
/// SIGKILL) — the harder half of the #721 fixture. Promptly-reaped (see `Fixture`).
fn spawn_renamed_sigterm_ignoring(name: &str) -> Fixture {
    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg(format!(
            "exec -a '{name}' /bin/bash -c 'trap \"\" TERM; while true; do sleep 1; done'"
        ))
        .spawn()
        .expect("spawn renamed sigterm-ignoring process");
    Fixture::spawn(child)
}

#[test]
fn renamed_cooperative_process_is_killed_by_pid_via_sigterm() {
    let fixture = spawn_renamed_cooperative("cam2-painter-totally-not-frame-probe");
    let pid = fixture.pid;
    sleep(Duration::from_millis(200));
    assert!(
        fixture.is_alive(),
        "fixture process must be alive before the test"
    );

    // eval — rig_test_ledger_terminate_entry_cmds is a STRING BUILDER (mirrors every other
    // `_cmds` function in this file's convention): it PRINTS a remote command; the caller must
    // eval/execute the printed text to actually run it (production embeds it via `$(...)` into
    // an `ssh` call). Calling the function directly here would only assert on its own SOURCE
    // TEXT (which happens to literally contain the string "KILL_NEEDED=0" in its else-branch —
    // a false-positive trap).
    let r = run_sourced(&format!(
        "eval \"$(rig_test_ledger_terminate_entry_cmds {pid} pid)\""
    ));
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("KILL_NEEDED=0"),
        "a cooperative process should die on SIGTERM alone, got: {}",
        r.stdout
    );
    assert!(
        !fixture.is_alive(),
        "the RENAMED process (argv[0]='cam2-painter-totally-not-frame-probe') must be DEAD after \
         terminate-by-pid -- kill-by-PID never cares what the process calls itself (the #721 fix)"
    );
    fixture.force_kill();
}

#[test]
fn renamed_sigterm_ignoring_process_escalates_to_sigkill_and_dies() {
    let fixture = spawn_renamed_sigterm_ignoring("cam2-painter-stubborn");
    let pid = fixture.pid;
    sleep(Duration::from_millis(300));
    assert!(
        fixture.is_alive(),
        "fixture process must be alive before the test"
    );

    let r = run_sourced(&format!(
        "eval \"$(rig_test_ledger_terminate_entry_cmds {pid} pid)\""
    ));
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(
        r.stdout.contains("KILL_NEEDED=1"),
        "a SIGTERM-ignoring process must report KILL_NEEDED=1 (escalation happened), got: {}",
        r.stdout
    );
    assert!(
        !fixture.is_alive(),
        "the RENAMED SIGTERM-ignoring process must still end up DEAD via the SIGKILL escalation"
    );
    fixture.force_kill();
}

#[test]
fn terminate_of_an_already_dead_pid_is_a_success_not_a_failure() {
    // A PID that no longer exists (e.g. the process already exited on its own) — cleanup's job
    // is "make sure it's gone", which is already true; must not error. (eval — see the comment
    // on the renamed-process tests above for why this can't call the builder directly.)
    let r = run_sourced("eval \"$(rig_test_ledger_terminate_entry_cmds 999999 pid)\"");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("KILL_NEEDED=0"));
}

#[test]
fn terminate_unit_kind_uses_systemctl_stop_and_is_active_check() {
    // No real systemd unit available in a test sandbox -- content-level check that the builder
    // targets the RIGHT mechanism for a systemd-unit-kind ledger entry.
    let r = run_sourced("rig_test_ledger_terminate_entry_cmds cam2-painter.service unit");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("systemctl stop 'cam2-painter.service'"));
    assert!(r.stdout.contains("systemctl is-active"));
}

#[test]
fn clean_paint_fallback_targets_the_given_fb_device_with_a_raw_zero_write() {
    let r = run_sourced("rig_test_ledger_clean_paint_fallback_cmds /dev/fb0");
    assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
    assert!(r.stdout.contains("/dev/fb0"));
    assert!(r.stdout.contains("if=/dev/zero"));
}

// ---------------------------------------------------------------------------
// Static wiring: rig-mode.sh + recording-e2e.sh must actually GO THROUGH the ledger, not just
// have it sitting unused in scripts/lib/.
// ---------------------------------------------------------------------------

#[test]
fn rig_mode_sources_and_uses_the_ledger() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap();
    assert!(
        text.contains("lib/rig-test-ledger.sh"),
        "rig-mode.sh must source scripts/lib/rig-test-ledger.sh"
    );
    assert!(
        text.contains("rig_test_ledger_register_remote_cmds"),
        "painter_launch_remote (the sanctioned TEST-mode painter launch) must register into the \
         ledger"
    );
    assert!(
        text.contains("event_mode_ledger_cleanup")
            || text.contains("rig_test_ledger_terminate_entry_cmds"),
        "do_event() must clean the ledger (terminate every registered entry) as part of the \
         EVENT-mode switch"
    );
}

#[test]
fn recording_e2e_sources_and_uses_the_ledger() {
    let text = std::fs::read_to_string(manifest_dir().join("scripts/recording-e2e.sh")).unwrap();
    assert!(
        text.contains("lib/rig-test-ledger.sh"),
        "recording-e2e.sh must source scripts/lib/rig-test-ledger.sh"
    );
    assert!(
        text.contains("rig_test_ledger_register_remote_cmds"),
        "recording-e2e.sh's own painter launch must register into the ledger too -- any \
         harness/worker-started painter must be ledger-tracked, not just rig-mode.sh's"
    );
}
