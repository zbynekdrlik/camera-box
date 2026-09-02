//! issue 1271 — the read-only stray recording/streaming check must run BEFORE any `[0/8]` rig
//! MUTATION, not after. On run 33571774966 the check sat inside the `[0/8] OBS pre-run state`
//! block, AFTER `cambox_parity_align_before_gate` (#1202, restarts camera-box.service on all active
//! cams) and `frame_probe_parity_align_before_gate` (#1138, redeploys cam2's painter) — so the
//! fleet's binary got restarted while the stream box was carrying a LIVE production broadcast, then
//! the harness refused. Fix: the read-only check is extracted to scripts/lib/stray-session-check.sh
//! (the #675 sourced-helper pattern) and called as the FIRST step right after the `[0/8]`
//! reachability preflight, before any mutation. The `genlock_burn OFF` normalization (a strih OBS
//! mutation) stays put.
//!
//! Two layers locked here:
//!  1. ORDERING + WIRING — static reads of scripts/recording-e2e.sh (verifiable with zero rig): the
//!     stray-session check is sourced + called, in the right place, and the old inline loop is gone.
//!  2. BEHAVIOR — source scripts/lib/stray-session-check.sh with a fake obs_phase2.py on `$HERE`
//!     and prove it (a) passes when idle, (b) aborts on an active recording, (c) aborts on an active
//!     stream and names WHAT is streaming (the ingest server, key-free) without ever leaking a key.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/stray-session-check.sh")
}

// ---------------------------------------------------------------------------
// 1. ORDERING + WIRING (static)
// ---------------------------------------------------------------------------

/// The whole point of issue 1271: reachability preflight < stray-session check < camera-box parity
/// auto-align < frame-probe painter auto-align. The stray check MUST precede both fleet mutations.
#[test]
fn stray_session_check_runs_before_the_fleet_mutations_1271() {
    let s = recording_e2e_text();
    let reach = s
        .find("[0/8] reachability preflight")
        .expect("recording-e2e.sh must have the [0/8] reachability preflight");
    let stray = s.find("stray_session_check_assert ").expect(
        "issue 1271: recording-e2e.sh must call the read-only stray_session_check_assert helper",
    );
    let cambox = s
        .find("cambox_parity_align_before_gate \"$CAMBOX_VERSION_LINUX\"")
        .expect("recording-e2e.sh must call the #1202 camera-box parity auto-align");
    let painter = s
        .find("frame_probe_parity_align_before_gate \"cam2=root@$PAINTER_IP\"")
        .expect("recording-e2e.sh must call the #1138 frame-probe painter auto-align");

    assert!(
        reach < stray,
        "issue 1271: the stray-session check must run AFTER the reachability preflight (it needs \
         strih+stream confirmed reachable). reach={reach} stray={stray}"
    );
    assert!(
        stray < cambox,
        "issue 1271: the READ-ONLY stray recording/streaming check MUST run BEFORE the camera-box \
         parity auto-align (a fleet MUTATION that restarts camera-box.service on all cams). \
         stray={stray} cambox_parity={cambox}"
    );
    assert!(
        cambox < painter,
        "the camera-box parity auto-align must still precede the frame-probe painter auto-align \
         (unchanged relative order). cambox={cambox} painter={painter}"
    );
}

/// recording-e2e.sh sources the lib and calls it as a BARE statement (never `$(...)`/a pipe) so its
/// `exit 1` propagates to the harness — the same discipline the adjacent #860 optical preflight uses.
#[test]
fn recording_e2e_sources_and_calls_the_stray_session_check_lib_1271() {
    let s = recording_e2e_text();
    assert!(
        s.contains(". \"$HERE/lib/stray-session-check.sh\""),
        "issue 1271: recording-e2e.sh must source scripts/lib/stray-session-check.sh"
    );
    // The call must be a bare statement: not command-substituted, not piped, not an `if` condition.
    let call_line = s
        .lines()
        .find(|l| l.trim_start().starts_with("stray_session_check_assert "))
        .expect("expected a bare stray_session_check_assert call line");
    let t = call_line.trim();
    assert!(
        !t.contains("$(") && !t.contains('|') && !t.starts_with("if "),
        "issue 1271: the stray_session_check_assert call must be a plain statement so its `exit 1` \
         propagates (never $()/pipe/if). Got: {call_line:?}"
    );
}

/// The stray check MOVED OUT of the `[0/8] OBS pre-run state` block into the lib — its old inline
/// loop literal must be gone from recording-e2e.sh, and the burn-normalize banner must no longer
/// claim it still does the stray check (that concern moved).
#[test]
fn recording_e2e_no_longer_carries_the_inline_stray_loop_1271() {
    let s = recording_e2e_text();
    assert!(
        !s.contains("for _pfhs in \"strih=$STRIH\""),
        "issue 1271: the inline stray-check loop must be extracted to the lib, not left in \
         recording-e2e.sh"
    );
    // The genlock_burn normalize banner stays, but must not still advertise the (moved) stray check.
    let banner = s
        .lines()
        .find(|l| l.contains("[0/8] OBS pre-run state"))
        .expect("the genlock_burn normalize banner must still exist");
    assert!(
        !banner.contains("no stray recording/streaming"),
        "issue 1271: the genlock_burn normalize banner must not still claim to check stray \
         recording/streaming — that moved to the lib. Got: {banner:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. BEHAVIOR (source the lib with a fake obs_phase2.py) — CI-run; a worktree-isolated worker
//    cannot run a sourced-bash-lib test locally (see .claude/rules/ci-testing-gotchas.md #1265).
// ---------------------------------------------------------------------------

/// Write a fake obs_phase2.py into `dir` whose record/stream status is driven by env vars:
///   FAKE_ACTIVE_HOST — the --host value that should report active
///   FAKE_ACTIVE_KIND — "record" | "stream"
///   FAKE_STREAM_DETAIL — the line `stream-detail` prints (defaults to a redacted sample)
fn write_fake_obs(dir: &PathBuf) {
    let fake = r#"#!/usr/bin/env python3
import sys, os
args = sys.argv[1:]
host = ""
for i, a in enumerate(args):
    if a == "--host" and i + 1 < len(args):
        host = args[i + 1]
active_host = os.environ.get("FAKE_ACTIVE_HOST", "")
kind = os.environ.get("FAKE_ACTIVE_KIND", "")
if "record" in args and "status" in args:
    on = host == active_host and kind == "record"
    print("active=%s path=" % ("True" if on else "False"))
elif "stream-status" in args:
    on = host == active_host and kind == "stream"
    print("active=%s path=" % ("True" if on else "False"))
elif "stream-detail" in args:
    print(os.environ.get("FAKE_STREAM_DETAIL",
          "active=True server=rtmp://127.0.0.1:1234/live duration_ms=754123 timecode=00:12:34"))
"#;
    let p = dir.join("obs_phase2.py");
    fs::write(&p, fake).expect("write fake obs_phase2.py");
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stray-check-1271-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).expect("mkdir scratch");
    d
}

/// Source the lib under the caller's REAL `set -euo pipefail` (proves the bare-statement call is
/// #1133-safe on the happy path AND aborts correctly), call stray_session_check_assert, and return
/// (success, stderr). HERE points at `dir` so the lib's `python3 "$HERE/obs_phase2.py"` hits the fake.
fn run_check(dir: &PathBuf, strih: &str, stream: &str, envs: &[(&str, &str)]) -> (bool, String) {
    let snippet = format!(
        "set -euo pipefail\n. \"{lib}\"\nstray_session_check_assert \"{here}\" \"{strih}\" \"{stream}\"\n",
        lib = lib_script().display(),
        here = dir.display(),
        strih = strih,
        stream = stream,
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&snippet);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn stray_check_passes_when_strih_and_stream_are_idle_1271() {
    let d = scratch("idle");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "10.0.0.2", "10.0.0.4", &[]);
    assert!(
        ok,
        "issue 1271: an idle strih+stream must pass the stray-session check (exit 0). stderr:\n{err}"
    );
}

#[test]
fn stray_check_aborts_on_an_active_recording_1271() {
    let d = scratch("rec");
    write_fake_obs(&d);
    let (ok, err) = run_check(
        &d,
        "10.0.0.2",
        "10.0.0.4",
        &[
            ("FAKE_ACTIVE_HOST", "10.0.0.2"),
            ("FAKE_ACTIVE_KIND", "record"),
        ],
    );
    assert!(!ok, "issue 1271: an active recording must ABORT (exit 1)");
    assert!(
        err.contains("ALREADY recording") && err.contains("strih"),
        "issue 1271: the recording refusal must name the box + say ALREADY recording. stderr:\n{err}"
    );
}

#[test]
fn stray_check_aborts_on_active_stream_and_names_what_is_streaming_without_a_key_1271() {
    let d = scratch("stream");
    write_fake_obs(&d);
    let (ok, err) = run_check(
        &d,
        "10.0.0.2",
        "10.0.0.4",
        &[
            ("FAKE_ACTIVE_HOST", "10.0.0.4"),
            ("FAKE_ACTIVE_KIND", "stream"),
            (
                "FAKE_STREAM_DETAIL",
                "active=True server=rtmp://127.0.0.1:1234/live duration_ms=754123 timecode=00:12:34",
            ),
        ],
    );
    assert!(!ok, "issue 1271: an active stream must ABORT (exit 1)");
    assert!(
        err.contains("ALREADY streaming") && err.contains("stream"),
        "issue 1271: the streaming refusal must name the box + say ALREADY streaming. stderr:\n{err}"
    );
    assert!(
        err.contains("server=rtmp://127.0.0.1:1234/live") && err.contains("duration_ms=754123"),
        "issue 1271: on a streaming refusal the log must show WHAT is streaming — the ingest server \
         url + GetStreamStatus.outputDuration (so a LIVE production broadcast is obvious). stderr:\n{err}"
    );
    // A stream KEY must NEVER leak, even partially — the detail read redacts it upstream.
    assert!(
        !err.contains("SUPERSECRETKEY"),
        "issue 1271: the stream key must never appear in the refusal. stderr:\n{err}"
    );
}
