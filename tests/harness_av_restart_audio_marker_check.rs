//! #421 — the `#137` AV_RESTART_GATE painter (recording-e2e.sh's
//! `av_restart_record_and_emit_plan()`) must AUDIBLY verify its own QPSK marker is actually
//! producing sound before proceeding to record — the SAME risk class `#420` already fixed for
//! `scripts/rig-mode.sh`'s TEST-mode painter launch.
//!
//! ## The bug
//!
//! `av_restart_record_and_emit_plan()` launches cam2's painter with `--audio-marker
//! --audio-marker-device ... --audio-marker-cadence-ticks ... --marker-log ...` but never checks
//! that the marker's ALSA PCM actually came up. If a flag were ever dropped/mistyped, or the
//! device were busy, this mode would silently record an unmeasured before/after pair — exactly
//! the #420 failure class, just in a different file.
//!
//! ## The fix (what these tests lock)
//!
//! The `#420` audible self-check (parse `hw:CARD=<id>,DEV=<n>` -> `/proc/asound/<id>/pcm<n>p/
//! sub0/status`, poll for `state: RUNNING`, fail loud otherwise) is extracted out of
//! `scripts/rig-mode.sh` into a shared, source-only helper `scripts/lib/audio-marker-check.sh`
//! (mirrors the `#309` `scripts/lib/rig-test-dropin.sh` shape exactly: pure string builders, no
//! ssh, no side effects at source time). Both `rig-mode.sh` (the original #420 fix) and
//! `recording-e2e.sh` (`#421`) source it and call `audio_marker_check_cmds`, so the two painter
//! launches can never drift on what "audible" means.
//!
//! Same PURE-STRING model as `tests/rig_mode.rs` / `tests/harness_rig_test_dropin_cleanup.rs` /
//! `tests/harness_av_restart_sync_gate.rs`: source the real helper and inspect its emitted text,
//! or read the real orchestration scripts and assert on their source — no ssh to a live rig.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const LIB_REL: &str = "scripts/lib/audio-marker-check.sh";

/// Source ONLY the shared helper (no orchestration script) and run `body`, returning stdout.
/// Asserts the harness itself exited 0 — the pure builders never fail.
fn run_sourced(body: &str) -> String {
    let lib = manifest_dir().join(LIB_REL);
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "#421: sourced harness exited non-zero (lib={}).\nstdout={:?}\nstderr={:?}",
        lib.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The shared helper must exist as its own source-only file (DRY-extracted out of rig-mode.sh).
#[test]
fn shared_audio_marker_check_lib_exists() {
    let lib = manifest_dir().join(LIB_REL);
    assert!(
        lib.exists(),
        "#421: {} must exist — the #420 audible self-check must be extracted into a shared \
         helper, not stay inline-only in scripts/rig-mode.sh",
        lib.display()
    );
}

/// `audio_marker_alsa_status_path` is the pure CARD/DEV parser: given a `hw:CARD=<id>,DEV=<n>`
/// ALSA device string it must derive exactly `/proc/asound/<id>/pcm<n>p/sub0/status` — the real
/// kernel-reported PCM status path #420 keys its RUNNING check on.
#[test]
fn alsa_status_path_derived_from_device_string() {
    let out = run_sourced(r#"audio_marker_alsa_status_path "hw:CARD=PCH,DEV=3""#);
    assert_eq!(
        out.trim(),
        "/proc/asound/PCH/pcm3p/sub0/status",
        "#421: default cam2 device must resolve to the PCH/pcm3p status path. Got:\n{out}"
    );

    let out2 = run_sourced(r#"audio_marker_alsa_status_path "hw:CARD=USB,DEV=0""#);
    assert_eq!(
        out2.trim(),
        "/proc/asound/USB/pcm0p/sub0/status",
        "#421: an overridden device must resolve to ITS OWN status path, not a hardcoded \
         default. Got:\n{out2}"
    );
}

/// The headline behavioral guard: `audio_marker_check_cmds` must emit a REMOTE bash snippet that
/// polls the derived ALSA status path for `state: RUNNING` and FAILS LOUD (never a silent
/// pass-through) — identifying itself as the `#420` check — when the marker never comes up.
#[test]
fn check_cmds_poll_running_state_and_fail_loud_when_silent() {
    let p = run_sourced(
        r#"audio_marker_check_cmds "hw:CARD=PCH,DEV=3" 'pkill -x frame-probe 2>/dev/null || true' "cadence=180 ticks""#,
    );
    assert!(
        p.contains("/proc/asound/PCH/pcm3p/sub0/status"),
        "#421: the self-check must read the REAL ALSA PCM status file — a genuine kernel \
         signal, never a stub. Got:\n{p}"
    );
    assert!(
        p.contains("state: RUNNING"),
        "#421: the self-check must assert the PCM is in the RUNNING state (actively streaming, \
         not merely opened/prepared). Got:\n{p}"
    );
    assert!(
        p.contains("is NOT RUNNING") && p.contains("#420"),
        "#421: a silent marker must FAIL LOUD identifying itself as the #420-class check (never \
         a silent pass-through). Got:\n{p}"
    );
    // The caller-supplied teardown (2nd arg) must run in the FAIL branch, before the exit.
    let fail_pos = p
        .find("is NOT RUNNING")
        .expect("#421: expected the silent-marker FAIL branch");
    let cleanup_pos = p[fail_pos..]
        .find("pkill -x frame-probe")
        .map(|i| i + fail_pos)
        .expect(
            "#421: the caller-supplied cleanup command must run in the FAIL branch (a silent \
             marker leaves a stray, unmeasured painter process behind otherwise)",
        );
    let exit_pos = p[cleanup_pos..]
        .find("exit 1")
        .map(|i| i + cleanup_pos)
        .expect("#421: the FAIL branch must exit non-zero AFTER running the cleanup command");
    assert!(cleanup_pos > fail_pos && exit_pos > cleanup_pos);
}

/// `recording-e2e.sh` must source the shared helper (mirrors the `#309` single-source guard —
/// never re-derive the ALSA CARD/DEV parsing independently).
#[test]
fn recording_e2e_sources_the_shared_audio_marker_check_lib() {
    assert!(
        read("scripts/recording-e2e.sh").contains("audio-marker-check.sh"),
        "#421: scripts/recording-e2e.sh must source scripts/lib/audio-marker-check.sh (single \
         source of truth shared with rig-mode.sh's #420 fix)"
    );
}

/// `rig-mode.sh` must ALSO source the shared helper post-refactor — the DRY extraction moved the
/// #420 logic OUT of rig-mode.sh's inline heredoc, not just copy-pasted it into a second file.
#[test]
fn rig_mode_sources_the_shared_audio_marker_check_lib() {
    assert!(
        read("scripts/rig-mode.sh").contains("audio-marker-check.sh"),
        "#421: scripts/rig-mode.sh must source scripts/lib/audio-marker-check.sh (DRY-extracted \
         out of its own inline #420 self-check, not duplicated)"
    );
}

/// The headline reachability guard: the AV_RESTART_GATE block (which defines and calls
/// `av_restart_record_and_emit_plan`) must WIRE the shared self-check, and the call must land
/// AFTER the cam2 painter is launched but BEFORE the gate starts OBS recording — a self-check
/// that ran after recording already started would be too late to prevent an unmeasured run.
#[test]
fn av_restart_painter_calls_the_shared_self_check_before_recording_starts() {
    let s = read("scripts/recording-e2e.sh");
    let gate_pos = s
        .find("AV_RESTART_GATE:-0")
        .expect("#137 AV_RESTART_GATE block must exist");
    let start_record_pos = s
        .find("[5/8] StartRecord")
        .expect("[5/8] StartRecord step must exist");
    let block = &s[gate_pos..start_record_pos];

    let launch_pos = block
        .find("--audio-marker-device $AV_RESTART_MARKER_DEVICE")
        .expect("#421: expected the existing AV_RESTART_GATE painter launch with --audio-marker");
    let check_pos = block.find("audio_marker_check_cmds").expect(
        "#421: av_restart_record_and_emit_plan() must call the shared audio_marker_check_cmds \
         self-check (same risk class as #420 — a dropped/mistyped flag or a busy ALSA device \
         must abort the AV_RESTART_GATE run, never silently record an unmeasured pair)",
    );
    assert!(
        check_pos > launch_pos,
        "#421: the self-check must run AFTER the painter launch. Got block:\n{block}"
    );

    // The self-check call must be inside av_restart_record_and_emit_plan() itself (before the
    // function's closing brace), i.e. it gates BOTH the 'before' and 'after' recordings — not a
    // one-off check bolted on only at the call site.
    let record_start_local = block
        .find("python3 \"$HERE/obs_phase2.py\" record")
        .expect("#421: expected the OBS StartRecord call inside av_restart_record_and_emit_plan");
    assert!(
        check_pos < record_start_local,
        "#421: the self-check must run BEFORE the gate starts OBS recording — a check that \
         fires after recording already started is too late to prevent an unmeasured run. \
         Got block:\n{block}"
    );
}
