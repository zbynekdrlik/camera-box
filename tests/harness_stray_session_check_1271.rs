//! issue 1271 — the read-only stray recording/streaming guard must run immediately BEFORE EVERY
//! `recording-e2e.sh` rig-MUTATION step, not once. Two live incidents: run 33571774966 mutated the
//! fleet ([0/8] parity auto-align) while the stream box was broadcasting because the check ran
//! AFTER; run 33573594588 started a broadcast DURING the [1/8] build (after an early [0/8] check
//! passed) and [2/8]/[2b/8] then deployed to all 7 cams while live. Fix: ONE shared
//! scripts/lib/stray-session-check.sh (`stray_session_check_assert`, the #675 sourced-helper
//! pattern) is called immediately before the bkshading-relay pause, the [0/8] parity/painter
//! auto-align, the [2/8] cam1 deploy, and the [2b/8] ALL_CAMBOX loop; it REUSES the shared
//! obs_phase2.py rig-busy-check that the job-start rig-busy-gate.sh uses. The existing pre-[4/8]
//! reroute re-check stays (pinned by harness_rig_busy_recheck.rs).
//!
//! Two layers locked here:
//!  1. WIRING/ORDERING — static reads of scripts/recording-e2e.sh: a guard precedes each mutation,
//!     every guard call is a bare statement, and the old inline loop is gone.
//!  2. BEHAVIOR — source scripts/lib/stray-session-check.sh with a fake obs_phase2.py on `$HERE`
//!     and prove idle→proceed, streaming/recording→abort (naming WHAT streams, key-free), a partial
//!     outage with a readable busy box→abort, and a fully-unreadable rig→fail-OPEN (WARN + proceed).

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
// 1. WIRING + ORDERING (static)
// ---------------------------------------------------------------------------

/// The whole point of issue 1271: a `stray_session_check_assert` guard sits immediately before
/// EVERY rig-mutation step — the bkshading-relay pause, the [0/8] camera-box/painter parity
/// auto-align, the [2/8] cam1 deploy, and the [2b/8] ALL_CAMBOX deploy loop — so a broadcast that
/// starts in ANY gap is caught before that mutation.
#[test]
fn a_stray_session_guard_precedes_every_fleet_mutation_1271() {
    let s = recording_e2e_text();
    let guard = "stray_session_check_assert \"$HERE\"";
    let guards: Vec<usize> = s.match_indices(guard).map(|(i, _)| i).collect();
    // The mutation sites, in file order (name, unique anchor).
    let muts: [(&str, &str); 4] = [
        (
            "bkshading-relay pause",
            "bkshading_e2e_pause_stop \"$CAMERA_NAME\"",
        ),
        (
            "[0/8] camera-box parity auto-align",
            "cambox_parity_align_before_gate \"$CAMBOX_VERSION_LINUX\"",
        ),
        ("[2/8] cam1 camera-box deploy", "echo \"[2/8] $CAMERA_NAME"),
        ("[2b/8] ALL_CAMBOX deploy loop", "echo \"[2b/8] $_cn"),
    ];
    assert!(
        guards.len() >= muts.len(),
        "issue 1271: expected at least {} stray_session_check_assert guards (one before each fleet \
         mutation); found {}",
        muts.len(),
        guards.len()
    );

    let mut_offsets: Vec<usize> = muts
        .iter()
        .map(|(name, anchor)| {
            s.find(anchor).unwrap_or_else(|| {
                panic!("issue 1271: mutation anchor not found: {name} ({anchor})")
            })
        })
        .collect();

    for (idx, (name, _)) in muts.iter().enumerate() {
        let m = mut_offsets[idx];
        // The nearest guard strictly before this mutation.
        let g = guards
            .iter()
            .copied()
            .filter(|&g| g < m)
            .max()
            .unwrap_or_else(|| {
                panic!(
                    "issue 1271: no stray_session_check_assert guard precedes the {name} mutation"
                )
            });
        // The nearest OTHER mutation strictly before this one (0 = start, for the first mutation).
        let prev_m = mut_offsets
            .iter()
            .copied()
            .filter(|&x| x < m)
            .max()
            .unwrap_or(0);
        assert!(
            g > prev_m,
            "issue 1271: a FRESH stray_session_check_assert guard must sit between the previous \
             mutation and the {name} mutation (a broadcast can start in that gap). guard_offset={g} \
             prev_mutation_offset={prev_m} this_mutation_offset={m}"
        );
    }
}

/// recording-e2e.sh sources the lib and every guard call is a BARE statement (never `$(...)`/a
/// pipe/an `if` condition) so its `exit 1` propagates to the harness — the discipline the adjacent
/// #860 optical preflight uses.
#[test]
fn recording_e2e_sources_and_calls_the_stray_session_check_lib_1271() {
    let s = recording_e2e_text();
    assert!(
        s.contains(". \"$HERE/lib/stray-session-check.sh\""),
        "issue 1271: recording-e2e.sh must source scripts/lib/stray-session-check.sh"
    );
    let call_lines: Vec<&str> = s
        .lines()
        .filter(|l| l.trim_start().starts_with("stray_session_check_assert "))
        .collect();
    assert!(
        call_lines.len() >= 4,
        "issue 1271: expected at least 4 stray_session_check_assert call lines; found {}",
        call_lines.len()
    );
    for l in &call_lines {
        let t = l.trim();
        assert!(
            !t.contains("$(") && !t.contains('|') && !t.starts_with("if "),
            "issue 1271: every stray_session_check_assert call must be a plain statement so its \
             `exit 1` propagates (never $()/pipe/if). Got: {l:?}"
        );
    }
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

/// Fake obs_phase2.py. The lib REUSES the shared `rig-busy-check` + `stream-detail`, so the fake
/// answers those two actions, driven by FAKE_BUSY:
///   idle        — busy:false, both boxes idle
///   recording   — busy:true, strih recording (a stray recording, streaming OFF)
///   streaming   — busy:true, stream streaming+recording (a REAL broadcast)
///   partial     — busy:null + exit 3 (strih WS-unreachable) BUT the readable stream box IS
///                 streaming (issue 1271 🟡4 — must still refuse)
///   unreachable — busy:null + exit 3, NO diagnostics (both unreachable) → fail-OPEN
/// FAKE_STREAM_DETAIL is the (already key-free) line `stream-detail` prints.
fn write_fake_obs(dir: &PathBuf) {
    let fake = r#"#!/usr/bin/env python3
import sys, os, json
args = sys.argv[1:]
if "rig-busy-check" in args:
    mode = os.environ.get("FAKE_BUSY", "idle")
    if mode == "recording":
        print(json.dumps({
            "busy": True,
            "reasons": ["strih is recording (GetRecordStatus.outputActive=true, outputTimecode=00:05:00)"],
            "diagnostics": [
                {"host": "strih", "streaming": False, "recording": True, "recordTimecode": "00:05:00"},
                {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None}],
            "hint": "strih: our own stray recording (recording ON, streaming OFF)"}))
    elif mode == "streaming":
        print(json.dumps({
            "busy": True,
            "reasons": ["stream is streaming (GetStreamStatus.outputActive=true)"],
            "diagnostics": [
                {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
                {"host": "stream", "streaming": True, "recording": True, "recordTimecode": "00:12:34"}],
            "hint": "stream: REAL broadcast (streaming+recording)"}))
    elif mode == "partial":
        print(json.dumps({
            "busy": None,
            "reasons": ["strih (10.0.0.2) unreachable: boom"],
            "diagnostics": [
                {"host": "stream", "streaming": True, "recording": True, "recordTimecode": "00:03:00"}]}))
        sys.exit(3)
    elif mode == "unreachable":
        print(json.dumps({"busy": None, "reasons": ["both boxes unreachable"]}))
        sys.exit(3)
    else:
        print(json.dumps({
            "busy": False,
            "reasons": [],
            "diagnostics": [
                {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
                {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None}]}))
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
fn run_check(dir: &PathBuf, busy: &str) -> (bool, String) {
    let snippet = format!(
        "set -euo pipefail\n. \"{lib}\"\nstray_session_check_assert \"{here}\" 10.0.0.2 10.0.0.4 \"the test mutation\"\n",
        lib = lib_script().display(),
        here = dir.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .env("FAKE_BUSY", busy)
        .env(
            "FAKE_STREAM_DETAIL",
            "active=True server=rtmp://127.0.0.1:1234/live duration_ms=754123 timecode=00:12:34",
        )
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn stray_check_passes_when_strih_and_stream_are_idle_1271() {
    let d = scratch("idle");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "idle");
    assert!(
        ok,
        "issue 1271: an idle strih+stream must pass the stray-session check (exit 0). stderr:\n{err}"
    );
}

#[test]
fn stray_check_aborts_on_an_active_recording_1271() {
    let d = scratch("rec");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "recording");
    assert!(!ok, "issue 1271: an active recording must ABORT (exit 1)");
    assert!(
        err.contains("ALREADY recording/streaming"),
        "issue 1271: the refusal must say the rig is ALREADY recording/streaming. stderr:\n{err}"
    );
    // Must surface the shared rig-busy-check reason/hint naming the box — NOT satisfiable by the
    // generic ERROR line alone (issue 1271 review 🔵2).
    assert!(
        err.contains("hint:") && err.contains("our own stray recording"),
        "issue 1271: the refusal must surface the shared rig-busy-check hint naming the recording \
         box. stderr:\n{err}"
    );
}

#[test]
fn stray_check_aborts_on_active_stream_and_names_what_is_streaming_without_a_key_1271() {
    let d = scratch("stream");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "streaming");
    assert!(!ok, "issue 1271: an active stream must ABORT (exit 1)");
    assert!(
        err.contains("ALREADY recording/streaming"),
        "issue 1271: the refusal must say the rig is ALREADY recording/streaming. stderr:\n{err}"
    );
    assert!(
        err.contains("server=rtmp://127.0.0.1:1234/live") && err.contains("duration_ms=754123"),
        "issue 1271: on a streaming refusal the log must show WHAT is streaming — the ingest server \
         url + GetStreamStatus.outputDuration (so a LIVE production broadcast is obvious). stderr:\n{err}"
    );
    // A stream KEY must NEVER leak, even partially — the real stream-detail redacts it upstream
    // (covered directly in tests/python/test_obs_phase2_stream_detail_1271.py::redact_*).
    assert!(
        !err.contains("SUPERSECRETKEY"),
        "issue 1271: the stream key must never appear in the refusal. stderr:\n{err}"
    );
}

/// issue 1271 🟡4: rig-busy-check is all-or-nothing — one box WS-unreachable yields busy=None (a
/// rig-busy-check exit-3). If the box it COULD read is busy, the guard must still REFUSE (the
/// pre-1271 loop refused if EITHER box was active), never fail-open into a fleet mutation live.
#[test]
fn stray_check_refuses_on_a_partial_outage_when_a_readable_box_is_busy_1271() {
    let d = scratch("partial");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "partial");
    assert!(
        !ok,
        "issue 1271 🟡4: a partial outage with a readable STREAMING box must ABORT (exit 1). stderr:\n{err}"
    );
    assert!(
        err.contains("ALREADY recording/streaming") && err.contains("server=rtmp://127.0.0.1:1234/live"),
        "issue 1271 🟡4: the partial-outage refusal must still name what is streaming. stderr:\n{err}"
    );
}

/// The fail-OPEN semantics claim: a fully-unreadable rig (busy=None, NO readable busy box) must
/// proceed (exit 0) with a WARNING — the job-start rig-busy-gate.sh already fail-closed a live
/// broadcast; a momentary WS blip here must not newly abort a healthy run (issue 1271 review 🔵3).
#[test]
fn stray_check_fails_open_when_the_rig_state_is_unreadable_1271() {
    let d = scratch("unreach");
    write_fake_obs(&d);
    let (ok, err) = run_check(&d, "unreachable");
    assert!(
        ok,
        "issue 1271: a fully-unreadable rig must fail-OPEN (exit 0), not abort a healthy run. stderr:\n{err}"
    );
    assert!(
        err.contains("WARNING") && err.contains("could not read rig-busy state"),
        "issue 1271: the fail-open path must WARN loudly. stderr:\n{err}"
    );
}
