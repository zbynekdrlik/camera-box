//! #1233 — the [4c/8] frozen-camera gate's ABORT signal moves from a PIXEL-HASH of strih preview
//! screenshots to strih's `genlock-fifo audit '<input>': received=` counter DELTA per input.
//!
//! Root cause (#1233): the old gate (`frozen-camera-gate.py`) sampled PIXEL HASHES via
//! GetSourceScreenshot; a strih DistroAV receiver holding the LAST frame during the [2b/8] cambox
//! deploy wave (a re-attaching receiver) produces IDENTICAL hashes even though the leg is
//! delivering frames — so with 7 cameras the gate lands inside the wave and false-aborts a ~40-min
//! run (run 33311702636 attempt 3: FROZEN on cam1,3,4,5,6,7 while every box captured 60 fps colour
//! and the QR sweep decoded the live painter minutes later). The `received=` counter is
//! content-independent: a source whose counter ADVANCES across the window is ALIVE regardless of
//! static screenshot content.
//!
//! Same convention as tests/harness_frozen_input_health_1052.rs: source the REAL lib (source-only,
//! no side effects) and exercise its pure functions directly, plus content-assert the recording-e2e
//! wiring. The pure lib REUSES mv_reverify_extract_received (mv-reverify-escalate.sh, #1093) +
//! frozen_input_classify (frozen-input-health.sh, #1052) — it does not re-implement them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &str) -> String {
    let path = manifest_dir().join(p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/frozen-cam-received.sh");
    assert!(s.exists(), "{} not found (#1233)", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
/// `set -uo pipefail` (no -e), mirroring harness_frozen_input_health_1052.rs's run_sourced.
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// Two-line strih OBS-log tail fixtures as bash ANSI-C ($'...') literals: the audit line format is
// `genlock-fifo audit '<src>': received=N ...`, exactly what mv_reverify_extract_received greps.
const R0: &str = r"$'genlock-fifo audit \'NDI cam1\': received=100 dropped=0\ngenlock-fifo audit \'NDI cam3\': received=200 dropped=0'";
const R1_ALIVE: &str = r"$'genlock-fifo audit \'NDI cam1\': received=180 dropped=0\ngenlock-fifo audit \'NDI cam3\': received=290 dropped=0'";
const R1_FROZEN_CAM3: &str = r"$'genlock-fifo audit \'NDI cam1\': received=180 dropped=0\ngenlock-fifo audit \'NDI cam3\': received=200 dropped=0'";
const R1_NOLINE_CAM3: &str = r"$'genlock-fifo audit \'NDI cam1\': received=180 dropped=0'";
const R1_RESET_CAM3: &str = r"$'genlock-fifo audit \'NDI cam1\': received=180 dropped=0\ngenlock-fifo audit \'NDI cam3\': received=5 dropped=0'";

fn classify(csv: &str, r0: &str, r1: &str) -> String {
    stdout_of(&format!(
        "frozen_cam_received_classify_raw '{csv}' {r0} {r1} 2>/dev/null"
    ))
}

fn should_abort(ok: &str, verdict: &str) -> String {
    stdout_of(&format!("frozen_cam_gate_should_abort {ok} '{verdict}'"))
}

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions + the I/O wrapper must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_functions() {
    for f in [
        "frozen_cam_received_classify_raw",
        "frozen_cam_gate_should_abort",
        "frozen_cam_received_read_and_verdict",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib (#1233)");
    }
}

#[test]
fn lib_reuses_shared_building_blocks_not_reimplemented() {
    // Sourcing the lib must pull in the reused pure fns (guarded chain-source), never a re-impl.
    for f in ["frozen_input_classify", "mv_reverify_extract_received"] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(
            out, "DEFINED",
            "{f} (reused building block) not available after sourcing"
        );
    }
    let src = read("scripts/lib/frozen-cam-received.sh");
    assert!(
        src.contains("frozen_input_classify") && src.contains("mv_reverify_extract_received"),
        "#1233: the lib must REUSE frozen_input_classify + mv_reverify_extract_received"
    );
}

// ---------------------------------------------------------------------------------------------
// frozen_cam_received_classify_raw <csv> <raw0> <raw1> -> ALIVE | FROZEN:x | INCONCLUSIVE:x | READ_FAIL
// ---------------------------------------------------------------------------------------------
#[test]
fn classify_all_sources_advancing_is_alive() {
    assert_eq!(classify("NDI cam1,NDI cam3", R0, R1_ALIVE), "ALIVE");
}

#[test]
fn classify_a_stuck_counter_is_frozen_naming_the_source() {
    // The abort signal: cam3's received= did not move across the window while cam1's did.
    assert_eq!(
        classify("NDI cam1,NDI cam3", R0, R1_FROZEN_CAM3),
        "FROZEN:NDI cam3"
    );
}

#[test]
fn classify_a_missing_audit_line_is_inconclusive_not_frozen() {
    // cam3 has NO audit line in sample1 -> UNKNOWN -> INCONCLUSIVE (never a false FROZEN/abort).
    assert_eq!(
        classify("NDI cam1,NDI cam3", R0, R1_NOLINE_CAM3),
        "INCONCLUSIVE:NDI cam3"
    );
}

#[test]
fn classify_a_counter_reset_is_inconclusive_not_frozen() {
    // curr < prev (OBS restarted between samples) -> UNKNOWN -> INCONCLUSIVE, never a false FROZEN.
    assert_eq!(
        classify("NDI cam1,NDI cam3", R0, R1_RESET_CAM3),
        "INCONCLUSIVE:NDI cam3"
    );
}

#[test]
fn classify_both_samples_empty_is_read_fail() {
    // A healthy tail is never empty; both empty => the log READ failed, NOT a freeze.
    assert_eq!(
        stdout_of("frozen_cam_received_classify_raw 'NDI cam1' '' '' 2>/dev/null"),
        "READ_FAIL"
    );
}

#[test]
fn classify_empty_source_list_is_inconclusive_never_false_alive() {
    assert_eq!(classify("", R0, R1_ALIVE), "INCONCLUSIVE:no-sources");
}

#[test]
fn classify_trims_whitespace_around_csv_members() {
    // Source names keep their internal space; only leading/trailing whitespace is trimmed.
    assert_eq!(classify(" NDI cam1 , NDI cam3 ", R0, R1_ALIVE), "ALIVE");
}

#[test]
fn classify_frozen_wins_over_inconclusive() {
    // cam3 stuck (FROZEN) + a third source with no line (UNKNOWN) -> FROZEN precedence (abort wins).
    let r0 = r"$'genlock-fifo audit \'NDI cam1\': received=100\ngenlock-fifo audit \'NDI cam3\': received=200'";
    let r1 = r"$'genlock-fifo audit \'NDI cam1\': received=180\ngenlock-fifo audit \'NDI cam3\': received=200'";
    assert_eq!(
        classify("NDI cam1,NDI cam3,NDI cam4", r0, r1),
        "FROZEN:NDI cam3"
    );
}

// ---------------------------------------------------------------------------------------------
// frozen_cam_gate_should_abort <frozen_ok> <final_verdict> -> PASS | ABORT | WARN_PASS
// ---------------------------------------------------------------------------------------------
#[test]
fn should_abort_pass_when_any_attempt_proved_alive() {
    assert_eq!(should_abort("1", "FROZEN:NDI cam3"), "PASS");
    assert_eq!(should_abort("1", "ALIVE"), "PASS");
}

#[test]
fn should_abort_aborts_only_on_a_proven_freeze() {
    // #365 preserved: the caller feeds the PROVEN-freeze verdict (frozen_proven) when any attempt
    // proved one, so a FROZEN:* verdict with frozen_ok=0 aborts.
    assert_eq!(should_abort("0", "FROZEN:NDI cam3"), "ABORT");
}

#[test]
fn should_abort_warn_pass_on_inconclusive_or_read_fail_never_false_abort() {
    // Never false-abort a ~40-min CI run on absence of evidence; the leg is re-proven downstream.
    assert_eq!(should_abort("0", "INCONCLUSIVE:NDI cam3"), "WARN_PASS");
    assert_eq!(should_abort("0", "READ_FAIL"), "WARN_PASS");
    assert_eq!(should_abort("0", ""), "WARN_PASS");
}

// ---------------------------------------------------------------------------------------------
// recording-e2e.sh [4c/8] wiring — the abort keys on received=, pixel-hash is report-only
// ---------------------------------------------------------------------------------------------
#[test]
fn recording_e2e_sources_and_calls_the_received_gate() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("lib/frozen-cam-received.sh"),
        "#1233: recording-e2e.sh [4c/8] must source scripts/lib/frozen-cam-received.sh"
    );
    assert!(
        s.contains("frozen_cam_received_read_and_verdict"),
        "#1233: [4c/8] must read leg liveness via frozen_cam_received_read_and_verdict (received= delta)"
    );
    assert!(
        s.contains("frozen_cam_gate_should_abort"),
        "#1233: [4c/8] must decide abort via frozen_cam_gate_should_abort (received= only)"
    );
    // #1233 review 🟡: the abort must key on the PROVEN-freeze verdict (tracked across attempts),
    // not the final read alone — so a last-attempt glitch can't erase an earlier proven freeze.
    assert!(
        s.contains("frozen_proven") && s.contains("${frozen_proven:-$frozen_recv_verdict}"),
        "#1233: [4c/8] must track frozen_proven and feed ${{frozen_proven:-$frozen_recv_verdict}} to \
         should_abort, so a proven freeze aborts even if the final attempt's read glitches"
    );
}

#[test]
fn recording_e2e_keeps_the_pixel_hash_gate_as_report_only() {
    let s = read("scripts/recording-e2e.sh");
    // The old pixel-hash gate is DEMOTED, not removed — still invoked (warm-up + diagnostic) …
    assert!(
        s.contains("frozen-camera-gate.py"),
        "#1233: the pixel-hash gate (frozen-camera-gate.py) stays referenced as report-only detail"
    );
    // … and explicitly labelled report-only so it can never be mistaken for the abort signal.
    assert!(
        s.contains("pixel-hash REPORT-ONLY"),
        "#1233: the pixel-hash gate's log line must mark it REPORT-ONLY (not the abort signal)"
    );
}

#[test]
fn recording_e2e_keeps_the_bounded_retry_and_exclude_anchors() {
    let s = read("scripts/recording-e2e.sh");
    // The #365/#399 bounded-retry + painter-self-feed exclusion invariants survive the signal swap.
    assert!(
        s.contains("FROZEN_CAM_ATTEMPTS"),
        "#1233: the received= gate must keep the bounded retry (FROZEN_CAM_ATTEMPTS)"
    );
    assert!(
        s.contains("FROZEN_CAM_EXCLUDE_SENDER"),
        "#1233: the derived source list must still exclude the painter box's own NDI sender"
    );
}

// ---------------------------------------------------------------------------------------------
// #1233 review 🔵: the I/O wrapper + set -euo pipefail safety, and the single-empty-raw asymmetry.
// ---------------------------------------------------------------------------------------------

/// raw0 empty but raw1 present (a healthy second read) is NOT a whole-read failure: the source is
/// UNKNOWN (prev unreadable) -> INCONCLUSIVE, never READ_FAIL and never a false FROZEN. Only BOTH
/// samples empty is READ_FAIL.
#[test]
fn classify_single_empty_raw_is_inconclusive_not_read_fail() {
    assert_eq!(
        classify("NDI cam1", "''", R1_ALIVE),
        "INCONCLUSIVE:NDI cam1"
    );
    // symmetric: raw0 present, raw1 empty -> curr unreadable -> UNKNOWN -> INCONCLUSIVE.
    assert_eq!(classify("NDI cam1", R0, "''"), "INCONCLUSIVE:NDI cam1");
}

/// The I/O wrapper (frozen_cam_received_read_and_verdict + _frozen_cam_received_read_tail via the
/// FROZEN_CAM_RECEIVED_CMD / FROZEN_CAM_RECEIVED_GAP_S seams) must run cleanly under FULL
/// `set -euo pipefail` (the caller invokes it inside a `$(...)` assignment in recording-e2e.sh's
/// strict-mode body) — a stateful fake reader whose received= advances between the two reads yields
/// ALIVE with exit 0, pinning the #1133-class set-e safety nothing else covers.
#[test]
fn wrapper_advancing_reader_is_alive_under_strict_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = dir.path().join("reader.sh");
    let cnt = dir.path().join("cnt");
    // 1st call -> received=100, every later call -> received=181 (advanced) -> ALIVE.
    fs::write(
        &stub,
        "#!/usr/bin/env bash\n\
         n=0; [ -f \"$STUB_CNT\" ] && n=\"$(cat \"$STUB_CNT\")\"\n\
         n=$((n + 1)); echo \"$n\" > \"$STUB_CNT\"\n\
         if [ \"$n\" -eq 1 ]; then printf \"genlock-fifo audit 'NDI cam1': received=100 dropped=0\\n\"\n\
         else printf \"genlock-fifo audit 'NDI cam1': received=181 dropped=0\\n\"; fi\n",
    )
    .unwrap();
    let mut perm = fs::metadata(&stub).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&stub, perm).unwrap();

    // FULL strict mode (set -euo pipefail, no `set +e`) — exactly the caller's context.
    let harness = "set -euo pipefail\n. \"$LIB\"\n\
         frozen_cam_received_read_and_verdict 10.0.0.1 'NDI cam1' 2>/dev/null";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .env("STUB_CNT", &cnt)
        .env("FROZEN_CAM_RECEIVED_CMD", &stub)
        .env("FROZEN_CAM_RECEIVED_GAP_S", "0")
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run strict-mode harness");
    assert_eq!(
        out.status.code(),
        Some(0),
        "wrapper must exit 0 under set -euo pipefail; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ALIVE");
}
