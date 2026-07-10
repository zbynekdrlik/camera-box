//! #649 — a cancelled full-path-e2e run must never leave strih/stream/imag recording ON.
//!
//! ## The bug (live incident, 2026-07-10 ~04:30)
//!
//! The pull_request-triggered full-path-e2e gate run was CANCELLED (a same-PR push superseded it,
//! `cancel-in-progress: true`) while strih+stream were mid-recording. GitHub Actions sends
//! SIGINT, then SIGKILLs after a short grace window. The OLD cleanup() reached its
//! `obs_phase2.py record --action stop` calls only AFTER the heartbeat/marker clears + the cam1
//! ssh restore + the cam2 ssh restore (each up to CLEANUP_SSH_TIMEOUT=30s) — so a SIGKILL landing
//! inside the grace window killed the trap before it ever stopped strih/stream/imag's recording.
//! The orphaned recording then self-deadlocked EVERY later gate run as RIG_BUSY: rig-busy-gate.sh
//! could not tell "our own leftover test recording" from a real broadcast, and someone had to
//! manually SSH in and StopRecord both boxes before any PR could merge again.
//!
//! ## The fix these tests lock (static read of the shell script — no rig, no ssh)
//!
//! cleanup() now (1) StopRecords strih/stream/imag as the VERY FIRST action — before even the
//! heartbeat/marker clears, and well before the #328 cam-device-free block — so the one action
//! that prevents the self-inflicted RIG_BUSY deadlock lands even in a very short SIGKILL grace
//! window, and (2) does so ONLY for boxes THIS harness itself started recording on (tracked via
//! `*_RECORDING_STARTED` flags flipped right after each box's own StartRecord succeeds at [5/8]) —
//! so an early abort (before StartRecord ever ran) can never blind-StopRecord a box that might be
//! recording a REAL broadcast. Same PURE-string model as
//! tests/harness_recording_e2e_cleanup_resilient.rs (#328): read the real script, assert
//! ordering + structure.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The body of cleanup() — from `cleanup()` to the `\ntrap ` that installs it (same slice the
/// sibling #328 cleanup tests use).
fn cleanup_body(s: &str) -> String {
    let start = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let end = s[start..]
        .find("\ntrap ")
        .map(|i| start + i)
        .expect("recording-e2e.sh must install the cleanup trap after cleanup()");
    s[start..end].to_string()
}

/// #649: StopRecord (strih/stream/imag) must be the FIRST thing cleanup() does — before EVEN the
/// rig-heartbeat/e2e-marker clears, and well before the #328 cam-device-free block or the OBS
/// teardown calls. This is the actual fix: a SIGKILL landing in a short cancellation grace window
/// must hit StopRecord before anything slower.
#[test]
fn cleanup_stoprecord_runs_before_heartbeat_marker_device_free_and_teardown() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));

    let stoprecord = body
        .find("record --host \"$STRIH\" --action stop")
        .expect("#649: cleanup() must StopRecord strih");
    let heartbeat_stop = body
        .find("rig_heartbeat_stop")
        .expect("cleanup() must still stop the rig-active heartbeat (#281)");
    let marker_clear = body
        .find("rig_e2e_marker_clear")
        .expect("cleanup() must still clear the e2e marker (#353)");
    let device_free = body
        .find("rm -f /tmp/camera-box-burn-*")
        .expect("#328: cleanup() must still free /dev/video0 on cam1");
    let teardown = body
        .find("obs_phase2.py\" teardown")
        .expect("cleanup() must still run obs_phase2 teardown");

    assert!(
        stoprecord < heartbeat_stop,
        "#649: cleanup()'s StopRecord must run BEFORE the heartbeat stop — the heartbeat/marker \
         clears are cheap, but ordering them first (as before this fix) wastes the SIGKILL grace \
         window on the wrong action first."
    );
    assert!(
        stoprecord < marker_clear,
        "#649: cleanup()'s StopRecord must run BEFORE the e2e-marker clear."
    );
    assert!(
        stoprecord < device_free,
        "#649: cleanup()'s StopRecord must run BEFORE the #328 cam-device-free block — that block \
         can take up to CLEANUP_SSH_TIMEOUT*2 seconds (cam1 + cam2 restores), which is exactly \
         what ate the SIGKILL grace window in the live incident."
    );
    assert!(
        stoprecord < teardown,
        "#649: cleanup()'s StopRecord must run BEFORE the OBS scene teardown."
    );
}

/// #649 item 2: each of the three StopRecord calls in cleanup() MUST be gated on its OWN
/// `*_RECORDING_STARTED` flag — never a blind, unconditional StopRecord. A blind stop could kill a
/// REAL broadcast's recording if this harness never even reached its own StartRecord step.
#[test]
fn cleanup_stoprecord_calls_are_guarded_by_started_flags() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));

    for (label, flag, call) in [
        (
            "strih",
            "STRIH_RECORDING_STARTED",
            "record --host \"$STRIH\" --action stop",
        ),
        (
            "stream",
            "STREAM_RECORDING_STARTED",
            "record --host \"$STREAM\" --action stop",
        ),
        (
            "imag",
            "IMAG_RECORDING_STARTED",
            "record --host \"$IMAG_IP\" --action stop",
        ),
    ] {
        let call_pos = body
            .find(call)
            .unwrap_or_else(|| panic!("#649: cleanup() must StopRecord {label}"));
        // The guard must appear in the SAME if-block: look for the flag check in the ~200 bytes
        // immediately preceding the call (comfortably covers `if [ "${FLAG:-0}" = "1" ]; then`).
        // The comments in this file use multi-byte UTF-8 (em dashes), so a raw byte offset can
        // land mid-character — walk back to the nearest valid char boundary.
        let mut window_start = call_pos.saturating_sub(200);
        while window_start > 0 && !body.is_char_boundary(window_start) {
            window_start -= 1;
        }
        let window = &body[window_start..call_pos];
        assert!(
            window.contains(flag),
            "#649: the {label} StopRecord call must be guarded by an `if` checking \
             ${{{flag}:-0}} = \"1\" immediately before it — a blind, unconditional StopRecord \
             could kill a REAL broadcast's recording if this harness never started recording on \
             {label}. Preceding text: {window:?}"
        );
    }
}

/// Every StopRecord call added by #649 must still be `timeout`-bounded (the #328 discipline this
/// whole file lives inside) so a hung obs-websocket op can never block the trap.
#[test]
fn cleanup_stoprecord_calls_are_timeout_wrapped() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    for call in [
        "record --host \"$STRIH\" --action stop",
        "record --host \"$STREAM\" --action stop",
        "record --host \"$IMAG_IP\" --action stop",
    ] {
        let pos = body
            .find(call)
            .unwrap_or_else(|| panic!("#649: cleanup() must contain {call:?}"));
        let line_start = body[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = body[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(body.len());
        let line = &body[line_start..line_end];
        assert!(
            line.contains("timeout "),
            "#649/#328: every StopRecord call in cleanup() must be wrapped in `timeout` so a hung \
             obs-websocket op can't block the trap. Unbounded line: {line:?}"
        );
    }
}

/// #649: cleanup() must call StopRecord on each box EXACTLY ONCE — no leftover second,
/// unconditional call further down (that would defeat the guard: a second unguarded call would
/// still blind-stop a box the harness never started recording on).
#[test]
fn cleanup_stoprecord_appears_exactly_once_per_box() {
    let body = cleanup_body(&read("scripts/recording-e2e.sh"));
    for (label, call) in [
        ("strih", "record --host \"$STRIH\" --action stop"),
        ("stream", "record --host \"$STREAM\" --action stop"),
        ("imag", "record --host \"$IMAG_IP\" --action stop"),
    ] {
        let count = body.matches(call).count();
        assert_eq!(
            count, 1,
            "#649: cleanup() must StopRecord {label} exactly once (the guarded #649 block) — \
             found {count} occurrences of {call:?}. A second, unconditional call would defeat \
             the #649 item-2 guard."
        );
    }
}

/// The `*_RECORDING_STARTED` flags must be declared with a default of 0 BEFORE the cleanup trap
/// is armed (mirrors the established `STRIH_PROG_SCENE`-before-trap convention, #246) — so an
/// early abort before [5/8] ever runs still finds every flag safely unset in cleanup().
#[test]
fn recording_started_flags_declared_before_trap_arms_default_zero() {
    let s = read("scripts/recording-e2e.sh");
    let trap_pos = s
        .find("\ntrap cleanup EXIT")
        .expect("recording-e2e.sh must arm the cleanup trap");
    for flag in [
        "STRIH_RECORDING_STARTED",
        "STREAM_RECORDING_STARTED",
        "IMAG_RECORDING_STARTED",
    ] {
        let decl = format!("{flag}=0");
        let pos = s
            .find(&decl)
            .unwrap_or_else(|| panic!("#649: {flag} must be declared with a default of 0"));
        assert!(
            pos < trap_pos,
            "#649: {flag}=0 must be declared BEFORE the cleanup trap arms, so an early abort \
             (before [5/8] StartRecord ever runs) still finds it safely unset."
        );
    }
}

/// Each `*_RECORDING_STARTED` flag must flip to 1 immediately after ITS OWN box's StartRecord
/// call succeeds at [5/8] — and strictly before the NEXT box's StartRecord call, so a failure on
/// box N+1 never lets box N's flag go unset (it must already be 1) nor lets box N+1's flag be
/// wrongly set (its own StartRecord hasn't succeeded yet if the script aborts mid-line).
///
/// NOTE: the script also defines a SEPARATE `zero_loss_record_and_emit_plan()` helper function
/// (its own independent record start/stop cycle for a zero-loss-restart verification pass,
/// textually EARLIER in the file even though it's only invoked later) that reuses the same
/// `record --host ... --action start` call shape without touching these flags — deliberately, it
/// is not part of the main [5/8] harness recording. So this test slices from the `[5/8]
/// StartRecord on strih + stream` banner onward, never matching that unrelated helper.
#[test]
fn recording_started_flags_flip_immediately_after_their_own_startrecord() {
    let s = read("scripts/recording-e2e.sh");
    let five_of_eight = s
        .find("[5/8] StartRecord on strih + stream")
        .expect("recording-e2e.sh must have the [5/8] StartRecord step");
    let region = &s[five_of_eight..];

    let strih_start = region
        .find("record --host \"$STRIH\"  --action start")
        .expect("[5/8] must StartRecord on strih");
    let strih_flag = region
        .find("STRIH_RECORDING_STARTED=1")
        .expect("#649: STRIH_RECORDING_STARTED must flip to 1 after strih's StartRecord");
    let stream_start = region
        .find("record --host \"$STREAM\" --action start")
        .expect("[5/8] must StartRecord on stream");
    let stream_flag = region
        .find("STREAM_RECORDING_STARTED=1")
        .expect("#649: STREAM_RECORDING_STARTED must flip to 1 after stream's StartRecord");
    let imag_start = region
        .find("record --host \"$IMAG_IP\" --action start")
        .expect("[5/8] must StartRecord on imag");
    let imag_flag = region
        .find("IMAG_RECORDING_STARTED=1")
        .expect("#649: IMAG_RECORDING_STARTED must flip to 1 after imag's StartRecord");

    assert!(
        strih_start < strih_flag && strih_flag < stream_start,
        "#649: STRIH_RECORDING_STARTED=1 must sit strictly between strih's own [5/8] StartRecord \
         and stream's [5/8] StartRecord (strih_start={strih_start}, strih_flag={strih_flag}, \
         stream_start={stream_start})."
    );
    assert!(
        stream_start < stream_flag && stream_flag < imag_start,
        "#649: STREAM_RECORDING_STARTED=1 must sit strictly between stream's own [5/8] \
         StartRecord and imag's [5/8] StartRecord (stream_start={stream_start}, \
         stream_flag={stream_flag}, imag_start={imag_start})."
    );
    assert!(
        imag_start < imag_flag,
        "#649: IMAG_RECORDING_STARTED=1 must sit right after imag's own [5/8] StartRecord \
         (imag_start={imag_start}, imag_flag={imag_flag})."
    );
}
