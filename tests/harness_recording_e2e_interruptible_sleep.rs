//! #657 — a gate run cancelled mid-recording must not defer the cleanup trap for the whole
//! remaining recording duration.
//!
//! ## The bug (live investigation, 2026-07-10)
//!
//! recording-e2e.sh's recording window was a single, plain, foreground `sleep "$(( DURATION +
//! RECORD_PAD ))"` (300-1810s). A bash shell blocked in a plain foreground `sleep N` defers ALL
//! signal handling — trapped OR default — until that `wait4()` syscall returns on its own, i.e.
//! until the sleep completes naturally. This is documented bash trap behavior, not an orphaning
//! artifact, and was empirically proven live against the REAL self-hosted dev1 Actions runner
//! (see the #657 investigation commits + `.claude/skills/e2e` playbook): a `gh run cancel`
//! delivered SIGINT (then SIGTERM ~7.5s later, then an untrappable "kill entire process tree"
//! ~2.5s after that — the runner's OWN documented escalation, confirmed via its Worker log)
//! while the harness was inside this bare `sleep`, and the EXIT/HUP/INT/TERM `cleanup()` trap
//! (armed via `trap cleanup EXIT HUP INT TERM`, and already StopRecord-first per #649) NEVER
//! got a chance to run at all — the whole process was killed by the runner's escalation before
//! the deferred trap could ever fire. #649's ordering fix inside cleanup() was therefore moot
//! for the cancellation path specifically: cleanup() never started running in the first place.
//!
//! ## The fix these tests lock
//!
//! `wait` (unlike directly awaiting a foreground external command) IS documented — and here
//! empirically verified against the real runner — to return immediately once a trapped signal
//! arrives, even mid-wait, with an exit status greater than 128. `interruptible_sleep()`
//! backgrounds the sleep and `wait`s on it instead of blocking on it directly; if `wait` returns
//! EARLY (an abnormal exit status), the trap has ALREADY run (synchronously, before `wait`
//! returns control) so this function kills the now-superfluous background sleep and `exit`s
//! immediately — rather than letting the script blunder on into the rest of the harness
//! (re-StopRecording an already-stopped box, downloading/decoding a run that was never meant to
//! complete) or leaving cleanup() to potentially run a SECOND time via the EXIT trap when that
//! explicit `exit` fires.
//!
//! These tests execute the REAL extracted `interruptible_sleep()` function body (no rig, no
//! ssh, no OBS) under a driver script that mirrors recording-e2e.sh's own trap shape, proving
//! the actual behavior — not just that the text is present.

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Extract the `interruptible_sleep() { ... }` function body (including the signature and
/// closing brace) as a standalone, sourceable snippet. The function body uses only `if...fi`
/// (no nested `{}` braces), so a plain search for the next top-level `\n}\n` after the opening
/// `{` correctly locates the end.
fn interruptible_sleep_snippet(s: &str) -> String {
    let start = s
        .find("interruptible_sleep()")
        .expect("#657: scripts/recording-e2e.sh must define interruptible_sleep()");
    let rel_end = s[start..]
        .find("\n}\n")
        .expect("#657: interruptible_sleep() must have a closing brace");
    s[start..start + rel_end + 2].to_string()
}

#[test]
fn interruptible_sleep_function_exists() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("interruptible_sleep()"),
        "#657: scripts/recording-e2e.sh must define an interruptible_sleep() helper"
    );
}

/// Both the ALL_CAMBOX per-segment wait and the steady-state recording wait must go through
/// interruptible_sleep — a lingering bare `sleep "$SEGMENT_SECS"` or
/// `sleep "$(( DURATION + RECORD_PAD ))"` would silently keep the #657 deferred-trap bug alive
/// on that path even after the function is introduced elsewhere.
#[test]
fn both_recording_window_waits_use_interruptible_sleep() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("interruptible_sleep \"$SEGMENT_SECS\""),
        "#657: the ALL_CAMBOX per-segment wait must call interruptible_sleep, not a bare sleep"
    );
    assert!(
        s.contains("interruptible_sleep \"$(( DURATION + RECORD_PAD ))\""),
        "#657: the steady-state recording wait must call interruptible_sleep, not a bare sleep"
    );
    // "not interruptible_sleep" — i.e. a BARE `sleep "..."` call for either window — would show
    // up as the literal 4-space/2-space indented form with nothing but whitespace before `sleep`
    // (as opposed to `interruptible_sleep "..."`, which never matches these patterns since
    // "sleep" there is immediately preceded by "_", not whitespace/newline).
    assert!(
        !s.contains("    sleep \"$SEGMENT_SECS\"\n"),
        "#657: no bare `sleep \"$SEGMENT_SECS\"` may remain — it must go through interruptible_sleep"
    );
    assert!(
        !s.contains("  sleep \"$(( DURATION + RECORD_PAD ))\"\n"),
        "#657: no bare `sleep \"$(( DURATION + RECORD_PAD ))\"` may remain — it must go through \
         interruptible_sleep"
    );
}

/// The REAL behavioral proof: extract interruptible_sleep(), run it under a driver script that
/// (like recording-e2e.sh) arms a trap for INT/TERM, send SIGINT ~0.3s in, and assert:
///   1. the driver exits promptly (well under the 30s sleep duration — bounded to 10s here,
///      comfortably inside the Actions runner's own ~10s SIGINT->SIGTERM->kill escalation);
///   2. the trap actually ran (proves the signal wasn't deferred for the whole sleep);
///   3. execution did NOT fall through to the line after interruptible_sleep (proves the
///      function itself exits on interruption rather than returning normally).
#[test]
fn interruptible_sleep_returns_promptly_and_exits_on_sigint() {
    let snippet = interruptible_sleep_snippet(&read("scripts/recording-e2e.sh"));
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("log.txt");
    let driver_path = dir.path().join("driver.sh");

    let driver = format!(
        "#!/usr/bin/env bash\nset -uo pipefail\nLOG={log:?}\n{snippet}\n\
         trap 'echo TRAP_RAN >> \"$LOG\"' INT TERM\n\
         echo CALLING >> \"$LOG\"\n\
         interruptible_sleep 30\n\
         echo RETURNED_NORMALLY >> \"$LOG\"\n",
        log = log_path.to_string_lossy(),
        snippet = snippet,
    );
    fs::write(&driver_path, driver).expect("write driver.sh");

    let mut child = Command::new("bash")
        .arg(&driver_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn driver.sh");
    let pid = child.id();

    std::thread::sleep(Duration::from_millis(300));
    let kill_status = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("send SIGINT");
    assert!(
        kill_status.success(),
        "kill -INT must be able to signal the driver process"
    );

    let start = Instant::now();
    let bound = Duration::from_secs(10); // comfortably under the 30s sleep; the runner's own escalation is ~10s
    let exited = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break Some(status);
        }
        if start.elapsed() > bound {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(
        exited.is_some(),
        "#657: interruptible_sleep must return/exit promptly on SIGINT — driver was still \
         running after {bound:?} (the OLD bare `sleep 30` would defer the trap for the full \
         30s, proving the regression)"
    );

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("TRAP_RAN"),
        "#657: the INT trap must have fired promptly — log:\n{log}"
    );
    assert!(
        !log.contains("RETURNED_NORMALLY"),
        "#657: interruptible_sleep must exit on interruption, not fall through to the next \
         script line as if the sleep had completed normally — log:\n{log}"
    );
}

/// A plain, unmodified `sleep N &` + `wait` pair with NO interruption must still behave like a
/// normal sleep — completing after roughly its full duration and letting execution continue.
/// This guards against an interruptible_sleep() that (over-eagerly) always exits early even
/// when nothing ever signals it.
#[test]
fn interruptible_sleep_completes_normally_when_never_signaled() {
    let snippet = interruptible_sleep_snippet(&read("scripts/recording-e2e.sh"));
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("log.txt");
    let driver_path = dir.path().join("driver.sh");

    let driver = format!(
        "#!/usr/bin/env bash\nset -uo pipefail\nLOG={log:?}\n{snippet}\n\
         trap 'echo TRAP_RAN >> \"$LOG\"' INT TERM\n\
         interruptible_sleep 1\n\
         echo RETURNED_NORMALLY >> \"$LOG\"\n",
        log = log_path.to_string_lossy(),
        snippet = snippet,
    );
    fs::write(&driver_path, driver).expect("write driver.sh");

    let status = Command::new("bash")
        .arg(&driver_path)
        .status()
        .expect("run driver.sh");
    assert!(
        status.success(),
        "an unsignaled interruptible_sleep must exit 0"
    );

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("RETURNED_NORMALLY"),
        "#657: an interruptible_sleep that is never signaled must return normally and let \
         execution continue — log:\n{log}"
    );
    assert!(
        !log.contains("TRAP_RAN"),
        "#657: the trap must not fire when nothing ever signals the process — log:\n{log}"
    );
}

/// The `full-path-e2e.yml` step that runs recording-e2e.sh must invoke it via `exec` — a plain
/// `run: bash scripts/recording-e2e.sh` forks a NESTED bash the Actions runner never signals
/// directly (it signals only the single top-level step PID it started); the empirically
/// confirmed live behavior was that the outer wrapper eventually dies to the signal (itself
/// deferred, ~10s) and orphans the inner trap-holding bash (reparented to pid 1) BEFORE the
/// runner's "kill entire process tree" sweep — so the orphan survives that sweep and is only
/// reaped later by an untrappable kill, and the trap NEVER runs. `exec` replaces the step's
/// process image with recording-e2e.sh's own bash — no separate child exists to orphan, and
/// the runner's direct-PID signal lands on the actual trap-holding process.
#[test]
fn full_path_e2e_workflow_execs_recording_e2e_sh() {
    let path = format!(
        "{}/.github/workflows/full-path-e2e.yml",
        env!("CARGO_MANIFEST_DIR")
    );
    let s = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(
        s.contains("exec bash scripts/recording-e2e.sh"),
        "#657: full-path-e2e.yml must invoke recording-e2e.sh via `exec bash \
         scripts/recording-e2e.sh` (no nested child bash to orphan). Full file:\n{s}"
    );
}

/// cleanup() can now be invoked twice in the interrupted-sleep path — synchronously via the
/// INT/HUP/TERM trap (fired while interruptible_sleep's `wait` is interrupted), and again via
/// the EXIT trap once interruptible_sleep's own `exit` call actually terminates the shell. Both
/// point at the SAME function, so cleanup() must guard re-entry: the second invocation must be
/// a safe no-op, never re-running the (possibly slower/riskier) teardown steps twice.
#[test]
fn cleanup_guards_against_re_entry() {
    let s = read("scripts/recording-e2e.sh");
    let start = s.find("cleanup() {").expect("cleanup() must be defined");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("cleanup() must be followed by the trap installation");
    let body = &s[start..end];
    assert!(
        body.contains("CLEANUP_HAS_RUN") || body.contains("CLEANUP_ALREADY_RAN"),
        "#657: cleanup() must guard against being invoked twice (once via INT/TERM/HUP, once \
         via EXIT) with an idempotency flag — a second invocation must return immediately \
         instead of re-running the teardown. cleanup() body:\n{body}"
    );
}
