//! #438 — phase-sync-gate CLI: single-source-of-truth guard.
//!
//! `src/phase_sync.rs::compute_phase_sync_offsets` (#286) is a pure Tier-0 kernel with 9 unit
//! tests, but before #438 had no production Rust caller and no `[[bin]]` CLI wrapper — unlike
//! its siblings (`render_budget`, `av_restart_sync`, `zero_loss_restart_survival`), each of
//! which has a thin `*-gate` binary that `scripts/*.py` shells out to. Without that wrapper,
//! `scripts/phase_sync_calibrate.py` reimplemented the offset math independently, so the two
//! could silently drift.
//!
//! This locks that `phase-sync-gate` (a) exists, (b) computes IDENTICAL offsets to the kernel's
//! own unit-test vectors (`src/phase_sync.rs`'s `slowest_camera_maps_to_the_floor` /
//! `faster_camera_gets_floor_plus_the_deficit` / `tie_at_the_max_...` / `clamp_rails_hold` /
//! `single_camera_...` / `all_equal_...` / `empty_input_...`), and (c) fails closed (never
//! silently drops a camera) on bad JSON, a non-object payload, a missing/non-numeric latency,
//! or a non-finite (NaN/inf) latency — a CLI boundary feeding a live-rig calibration apply must
//! never guess or return a partial result.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the compiled `phase-sync-gate` binary with `stdin_json` on stdin; return
/// `(exit_code, stdout, stderr)`.
fn run(stdin_json: &str) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_phase-sync-gate");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phase-sync-gate");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait phase-sync-gate");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Parse a `{"name": number, ...}` JSON object into a sorted `Vec<(String, u32)>` for
/// order-independent comparison.
fn parse_offsets(json: &str) -> Vec<(String, u32)> {
    let v: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("gate stdout is not valid JSON: {e} (stdout={json:?})"));
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("gate stdout is not a JSON object: {json:?}"));
    let mut out: Vec<(String, u32)> = obj
        .iter()
        .map(|(k, val)| {
            (
                k.clone(),
                val.as_u64()
                    .unwrap_or_else(|| panic!("offset for '{k}' is not a u64: {val:?}"))
                    as u32,
            )
        })
        .collect();
    out.sort();
    out
}

/// The binary source must exist.
#[test]
fn phase_sync_gate_bin_src_exists() {
    let path = format!("{}/src/bin/phase-sync-gate.rs", env!("CARGO_MANIFEST_DIR"));
    assert!(
        std::path::Path::new(&path).exists(),
        "src/bin/phase-sync-gate.rs not found (#438)."
    );
}

/// Mirrors `src/phase_sync.rs`'s `slowest_camera_maps_to_the_floor` test EXACTLY — the gate
/// binary must compute the IDENTICAL offsets the kernel's own unit test proves, never a
/// separately-derived value.
#[test]
fn matches_kernel_slowest_camera_maps_to_the_floor() {
    let (code, out, err) = run(r#"{"cam1": 50.0, "cam2": 80.0}"#);
    assert_eq!(code, 0, "expected exit 0, got {code} (stderr={err:?})");
    assert_eq!(
        parse_offsets(&out),
        vec![("cam1".to_string(), 33), ("cam2".to_string(), 3)]
    );
}

/// Mirrors the kernel's `faster_camera_gets_floor_plus_the_deficit` (3-camera case).
#[test]
fn matches_kernel_faster_camera_gets_floor_plus_the_deficit() {
    let (code, out, err) = run(r#"{"cam10": 100.0, "cam20": 90.0, "cam30": 80.0}"#);
    assert_eq!(code, 0, "expected exit 0, got {code} (stderr={err:?})");
    assert_eq!(
        parse_offsets(&out),
        vec![
            ("cam10".to_string(), 3),
            ("cam20".to_string(), 13),
            ("cam30".to_string(), 23),
        ]
    );
}

/// Mirrors the kernel's `tie_at_the_max_maps_every_tied_camera_to_the_floor`.
#[test]
fn matches_kernel_tie_at_the_max_maps_every_tied_camera_to_the_floor() {
    let (code, out, _) = run(r#"{"cam1": 80.0, "cam2": 80.0, "cam3": 50.0}"#);
    assert_eq!(code, 0);
    assert_eq!(
        parse_offsets(&out),
        vec![
            ("cam1".to_string(), 3),
            ("cam2".to_string(), 3),
            ("cam3".to_string(), 33),
        ]
    );
}

/// Mirrors the kernel's `clamp_rails_hold` — the 2000ms cap and 3ms floor must hold through
/// the CLI boundary exactly as they do in the pure kernel.
#[test]
fn matches_kernel_clamp_rails_hold() {
    let (code, out, _) = run(r#"{"cam1": 0.0, "cam2": 5000.0}"#);
    assert_eq!(code, 0);
    assert_eq!(
        parse_offsets(&out),
        vec![("cam1".to_string(), 2000), ("cam2".to_string(), 3)]
    );
}

/// Mirrors the kernel's `single_camera_is_its_own_slowest_and_gets_the_floor`.
#[test]
fn matches_kernel_single_camera_gets_the_floor() {
    let (code, out, _) = run(r#"{"only": 123.4}"#);
    assert_eq!(code, 0);
    assert_eq!(parse_offsets(&out), vec![("only".to_string(), 3)]);
}

/// Mirrors the kernel's `all_equal_latencies_all_map_to_the_floor`.
#[test]
fn matches_kernel_all_equal_latencies_all_map_to_the_floor() {
    let (code, out, _) = run(r#"{"a": 42.0, "b": 42.0, "c": 42.0}"#);
    assert_eq!(code, 0);
    assert_eq!(
        parse_offsets(&out),
        vec![
            ("a".to_string(), 3),
            ("b".to_string(), 3),
            ("c".to_string(), 3),
        ]
    );
}

/// Mirrors the kernel's `empty_input_yields_empty_output` — an empty object is a valid
/// degenerate case (no cameras, nothing to align), NOT an error.
#[test]
fn empty_input_yields_empty_output_and_exits_zero() {
    let (code, out, err) = run("{}");
    assert_eq!(
        code, 0,
        "empty input must exit 0, got {code} (stderr={err:?})"
    );
    assert!(
        parse_offsets(&out).is_empty(),
        "empty input must yield an empty offsets object, got {out:?}"
    );
}

/// Bad JSON must fail closed (exit 2), never silently produce an empty or partial result.
#[test]
fn malformed_json_fails_closed() {
    let (code, _, err) = run("{not json");
    assert_eq!(code, 2, "malformed JSON must exit 2, got {code}");
    assert!(!err.is_empty(), "must print an error to stderr");
}

/// A non-object JSON payload (e.g. a bare array) must fail closed.
#[test]
fn non_object_json_fails_closed() {
    let (code, _, err) = run("[1, 2, 3]");
    assert_eq!(code, 2, "a non-object payload must exit 2, got {code}");
    assert!(!err.is_empty(), "must print an error to stderr");
}

/// A non-numeric latency value must fail closed — never coerced or silently dropped.
#[test]
fn non_numeric_latency_fails_closed() {
    let (code, _, err) = run(r#"{"cam1": "not-a-number"}"#);
    assert_eq!(code, 2, "a non-numeric latency must exit 2, got {code}");
    assert!(!err.is_empty(), "must print an error to stderr");
}

/// A non-finite latency (e.g. an exponent large enough to overflow to +inf) must fail closed —
/// stricter than the pure kernel (which silently skips non-finite entries): a CLI caller
/// applying settings to a live rig must get a COMPLETE result or a loud failure, never a
/// partial one that would KeyError downstream.
#[test]
fn non_finite_latency_fails_closed() {
    let (code, _, err) = run(r#"{"cam1": 50.0, "cam2": 1e400}"#);
    assert_eq!(code, 2, "a non-finite latency must exit 2, got {code}");
    assert!(!err.is_empty(), "must print an error to stderr");
}

/// No stdin at all (immediate EOF) is the same as an empty object only if it parses as `{}`;
/// truly empty stdin is NOT valid JSON, so it must fail closed too.
#[test]
fn empty_stdin_fails_closed() {
    let (code, _, err) = run("");
    assert_eq!(
        code, 2,
        "empty stdin (not valid JSON) must exit 2, got {code}"
    );
    assert!(!err.is_empty(), "must print an error to stderr");
}
