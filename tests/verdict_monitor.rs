//! Behavioral guard for `scripts/verdict-monitor.sh` — the #166 LIVENESS/TIMEOUT guard.
//!
//! The #166 night: the recording-verdict process decoded a 7.3 GB cam1 grab single-threaded
//! for >1 h, then DIED SILENTLY — and the e2e harness waited on a completion marker the
//! crashed process never wrote, so the whole run HUNG all night with no result.
//!
//! The monitor must detect BOTH failure modes and FAIL LOUDLY instead of hanging:
//!
//! 1. DEAD  — the verdict PID is gone but no exit marker was written (silent crash/kill).
//! 2. STALL — the verdict is alive but its output stopped growing for the stall timeout.
//!
//! …and on a clean completion it returns the verdict's OWN exit code.
//!
//! These tests spawn fake "verdict" processes (sleep/echo loops) and assert the monitor's
//! exit code + that it printed a clear diagnostic. They use short timeouts so the suite is
//! fast. This is the harness half of the #166 fix (the decode-parallelization half is unit
//! -tested in src/probe/recording.rs).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn monitor() -> PathBuf {
    let s = manifest_dir().join("scripts/verdict-monitor.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// A scratch dir unique to this test process; cleaned at the end of each test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "verdict-monitor-test-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the monitor against `pid`, watching `output` for progress and `exit_marker`
/// for completion. Returns (exit_code, stdout, stderr).
fn run_monitor(
    pid: u32,
    output: &Path,
    exit_marker: &Path,
    stall_timeout: &str,
    poll: &str,
) -> (i32, String, String) {
    let out = Command::new(monitor())
        .args([
            "--pid",
            &pid.to_string(),
            "--output",
            output.to_str().unwrap(),
            "--exit-marker",
            exit_marker.to_str().unwrap(),
            "--stall-timeout",
            stall_timeout,
            "--poll",
            poll,
            "--label",
            "verdict",
        ])
        .output()
        .expect("run verdict-monitor.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Spawn a long-lived fake verdict that NEVER writes the marker and NEVER produces
/// output — a process we can kill to simulate a silent crash.
fn spawn_silent_sleeper() -> Child {
    Command::new("sleep")
        .arg("600")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleeper")
}

#[test]
fn dead_process_without_marker_fails_loud_not_hang() {
    // The exact #166 failure: the verdict process is GONE but left no exit marker.
    // The monitor must FAIL FAST (non-zero) with a clear diagnostic — never hang.
    let dir = scratch("dead");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "[verdict] starting...\n").unwrap();

    let mut child = spawn_silent_sleeper();
    let pid = child.id();
    // Kill it immediately so it is dead before the monitor's first poll — it never
    // wrote the marker, mimicking a silent crash mid-decode.
    child.kill().expect("kill sleeper");
    child.wait().ok();

    // Generous stall timeout (300s) so we are proving the DEAD path, not the STALL
    // path — if the monitor hung, this test would itself time out the CI job.
    let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "300", "1");

    assert_ne!(
        code, 0,
        "dead-without-marker must fail, got 0\n{stdout}\n{stderr}"
    );
    assert_eq!(code, 126, "DEAD exit code (126)");
    let all = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        all.contains("died") && all.contains("exit marker"),
        "must print a clear silent-death diagnostic, got:\n{all}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stalled_process_with_no_output_progress_fails_loud() {
    // The verdict is ALIVE but wedged: its output stops growing. The monitor must
    // detect the stall (no progress for >= stall-timeout) and FAIL LOUDLY, killing
    // the wedged process — never wait forever on a hung verdict.
    let dir = scratch("stall");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "[verdict] decoding...\n").unwrap(); // never grows again

    let mut child = spawn_silent_sleeper(); // alive, produces nothing
    let pid = child.id();

    // stall-timeout = 2s, poll = 1s → the monitor sees no growth and fails within
    // a few seconds. Proves the STALL path independent of the DEAD path.
    let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "2", "1");

    assert_eq!(code, 124, "STALL exit code (124)\n{stdout}\n{stderr}");
    let all = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        all.contains("stall") && all.contains("no output progress"),
        "must print a clear stall diagnostic, got:\n{stdout}{stderr}"
    );

    // The monitor must have killed the wedged process. Reap it (the test owns the
    // child) and assert it terminated by signal — `kill -0` alone is unreliable
    // here because a killed-but-unreaped process is a zombie owned by THIS test, so
    // `kill -0 <zombie>` still returns success. `wait()` reaps it and tells us how
    // it died.
    std::thread::sleep(Duration::from_millis(300));
    let status = child.wait().expect("reap stalled child");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            status.signal().is_some(),
            "monitor must kill the stalled verdict (expected death-by-signal, got {status:?})"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clean_completion_returns_the_verdicts_own_exit_code() {
    // On clean completion the wrapper writes the verdict's exit code to the marker;
    // the monitor must return THAT code (so a real verdict FAIL still fails the run,
    // and a PASS still passes it).
    for expected in [0, 1, 7] {
        let dir = scratch(&format!("clean-{expected}"));
        let output = dir.join("verdict.out");
        let marker = dir.join("verdict.exit");
        std::fs::write(&output, "[verdict] done\n").unwrap();

        // A short-lived fake verdict that exits, then we write the marker (mimicking
        // the wrapper) BEFORE the monitor's first poll.
        let mut child = spawn_silent_sleeper();
        let pid = child.id();
        child.kill().ok();
        child.wait().ok();
        let mut f = std::fs::File::create(&marker).unwrap();
        write!(f, "{expected}").unwrap();
        drop(f);

        let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "300", "1");
        assert_eq!(
            code, expected,
            "monitor returns verdict exit code {expected}\n{stdout}\n{stderr}"
        );
        let all = format!("{stdout}{stderr}").to_lowercase();
        assert!(
            all.contains("finished"),
            "reports clean finish:\n{stdout}{stderr}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn missing_required_args_fails_with_usage_code() {
    let out = Command::new(monitor())
        .args(["--pid", "1"]) // missing --output and --exit-marker
        .output()
        .expect("run monitor");
    assert_eq!(out.status.code(), Some(2), "bad-args exit code is 2");
}

// ---- #166 review: a corrupt/empty/partial marker must NOT be a false exit-0 PASS ----

#[test]
fn empty_marker_on_dead_process_is_not_a_false_pass() {
    // BUG-2 regression: the process is DEAD and the marker EXISTS but is EMPTY (the
    // wrapper truncated it but crashed before the digits landed). The monitor must
    // NOT default to exit 0 ("verdict passed") — an empty marker is a corrupt result,
    // so it must FAIL LOUD via the DEAD path (126), never a silent false green.
    let dir = scratch("empty-marker");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "[verdict] ...\n").unwrap();
    std::fs::write(&marker, "").unwrap(); // present but empty = corrupt

    let mut child = spawn_silent_sleeper();
    let pid = child.id();
    child.kill().ok();
    child.wait().ok();

    let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "300", "1");
    assert_eq!(
        code, 126,
        "empty marker on a dead process must FAIL (DEAD), not exit 0\n{stdout}\n{stderr}"
    );
}

#[test]
fn non_numeric_marker_on_dead_process_is_not_a_false_pass() {
    // A marker with non-digit garbage is also corrupt → must fail, not exit 0.
    let dir = scratch("garbage-marker");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "[verdict] ...\n").unwrap();
    std::fs::write(&marker, "oops").unwrap();

    let mut child = spawn_silent_sleeper();
    let pid = child.id();
    child.kill().ok();
    child.wait().ok();

    let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "300", "1");
    assert_eq!(
        code, 126,
        "non-numeric marker on a dead process must FAIL, not exit 0\n{stdout}\n{stderr}"
    );
}

#[test]
fn marker_with_trailing_whitespace_parses_the_code() {
    // The e2e wrapper writes `echo "$?" > MARKER`, which appends a newline. A
    // complete numeric code with trailing whitespace must parse correctly.
    let dir = scratch("ws-marker");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "done\n").unwrap();
    std::fs::write(&marker, "1\n").unwrap(); // echo-style trailing newline

    let mut child = spawn_silent_sleeper();
    let pid = child.id();
    child.kill().ok();
    child.wait().ok();

    let (code, stdout, stderr) = run_monitor(pid, &output, &marker, "300", "1");
    assert_eq!(
        code, 1,
        "trailing newline must not corrupt the code\n{stdout}\n{stderr}"
    );
}

// ---- #166 review BUG-1: STALL kill must take down the verdict's CHILD, not orphan it ----

#[test]
fn stall_kill_takes_down_the_child_decode_not_just_the_wrapper() {
    // The real verdict runs as a CHILD of a wrapper (the e2e uses `setsid bash -c`).
    // On STALL the monitor must kill the WHOLE group so the heavy child decode is not
    // orphaned. Spawn a setsid wrapper whose child is a uniquely-named sleeper; after
    // the monitor reports STALL, assert NO process matching that unique name survives.
    let dir = scratch("group-kill");
    let output = dir.join("verdict.out");
    let marker = dir.join("verdict.exit");
    std::fs::write(&output, "[verdict] wedged...\n").unwrap(); // never grows

    // Unique marker arg so we can find the orphan if the group-kill fails. The child
    // `sleep` is exec'd by a bash that setsid put in its own process group; $! is the
    // group leader.
    let tag = format!("cbverdict_{}_{}", std::process::id(), "groupkill");
    let mut wrapper = Command::new("setsid")
        .args([
            "bash",
            "-c",
            // exec a long sleep carrying the unique tag in argv so pgrep can find it
            &format!("exec sleep 600 # {tag}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn setsid wrapper");
    let pid = wrapper.id();
    std::thread::sleep(Duration::from_millis(300)); // let the group settle

    let (code, _stdout, stderr) = run_monitor(pid, &output, &marker, "2", "1");
    assert_eq!(code, 124, "STALL exit code\n{stderr}");

    // Give the kill a moment to propagate, then assert NO process carries the tag.
    std::thread::sleep(Duration::from_millis(500));
    let survivors = Command::new("pgrep")
        .args(["-f", &tag])
        .output()
        .expect("pgrep");
    let out = String::from_utf8_lossy(&survivors.stdout);
    let live: Vec<&str> = out.split_whitespace().collect();
    assert!(
        live.is_empty(),
        "STALL must group-kill the wrapper AND its child — orphans survived: {live:?}\n{stderr}"
    );
    wrapper.kill().ok();
    wrapper.wait().ok();
    std::fs::remove_dir_all(&dir).ok();
}
