//! #1141 — parity + wiring guard for the head-end OPTICAL blur/shutter preflight.
//!
//! The pure crate-root `camera_box::optical_preflight` is the SOURCE OF TRUTH (thresholds +
//! classify decision + the NAMED Slovak abort message); `scripts/lib/optical-preflight.sh`
//! REPLICATES it and recording-e2e.sh invokes it with ONE call line (the #675 pattern). This test
//! pins the shell lib to the Rust module so the two can never drift, cross-checks the shell classify
//! against the Rust classify on shared fixtures, and asserts the recording-e2e.sh wiring is present.

use camera_box::optical_preflight::{
    classify, median, OpticalPreflightVerdict, OPTICAL_PREFLIGHT_ABORT_MESSAGE,
    OPTICAL_PREFLIGHT_MIN_SAMPLES, OPTICAL_PREFLIGHT_ROUGH_FLOOR,
};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn lib_script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/optical-preflight.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source the shared lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Pipe `stdin` into `optical_preflight_classify` and return its one-line verdict.
fn classify_shell(stdin: &str) -> String {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\noptical_preflight_classify";
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", lib_script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn bash");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build the shell classify input (a `rough=N` token per line) for a slice of samples, so the
/// SAME fixture drives both the Rust and the shell classifier.
fn journal_of(samples: &[f32]) -> String {
    samples
        .iter()
        .map(|r| format!("capture chroma: u_dev=6.3 v_dev=9.8 rough={r} -> colour"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- PARITY: the shell constants + message MUST equal the Rust source of truth ----

#[test]
fn shell_rough_floor_matches_rust_1141() {
    let shell: f32 = run_sourced("optical_preflight_rough_floor")
        .trim()
        .parse()
        .expect("floor is a number");
    assert_eq!(shell, OPTICAL_PREFLIGHT_ROUGH_FLOOR);
}

#[test]
fn shell_min_samples_matches_rust_1141() {
    let shell: usize = run_sourced("optical_preflight_min_samples")
        .trim()
        .parse()
        .expect("min is a usize");
    assert_eq!(shell, OPTICAL_PREFLIGHT_MIN_SAMPLES);
}

#[test]
fn shell_abort_message_matches_rust_byte_for_byte_1141() {
    let shell = run_sourced("optical_preflight_abort_message");
    assert_eq!(shell, OPTICAL_PREFLIGHT_ABORT_MESSAGE);
    // The operator-facing ownership + concrete camera settings must survive any future reword.
    assert!(
        shell.contains("FYZICKY"),
        "abort message must keep the physical-camera ownership"
    );
    assert!(
        shell.contains("1/500+"),
        "abort message must name the concrete shutter setting"
    );
}

// ---- CROSS-CHECK: the shell classify verdict MUST equal the Rust classify verdict ----

fn assert_agree(samples: &[f32]) {
    let rust = classify(samples);
    let shell = classify_shell(&journal_of(samples));
    let mut toks = shell.split_whitespace();
    let shell_state = toks.next().unwrap_or("");
    let expect = match rust {
        OpticalPreflightVerdict::Healthy => "HEALTHY",
        OpticalPreflightVerdict::SickBlur => "SICK_BLUR",
        OpticalPreflightVerdict::InsufficientData => "INSUFFICIENT",
    };
    assert_eq!(
        shell_state, expect,
        "shell/Rust classify disagree on {samples:?}: rust={rust:?} shell={shell:?}"
    );
    // A DECIDED verdict also prints the median — it must agree with the Rust median, so an awk
    // sort/median bug that kept the verdict on the correct side of the floor still can't slip
    // through (the #1141 review's median-cross-check hardening).
    if matches!(
        rust,
        OpticalPreflightVerdict::Healthy | OpticalPreflightVerdict::SickBlur
    ) {
        let shell_median: f32 = toks
            .next()
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(|| panic!("a decided shell verdict must carry a median: {shell:?}"));
        let rust_median = median(samples).expect("a decided verdict has a median");
        assert!(
            (shell_median - rust_median).abs() < 0.01,
            "shell/Rust median disagree on {samples:?}: rust={rust_median} shell={shell_median}"
        );
    }
}

#[test]
fn shell_and_rust_classify_agree_on_healthy_baseline_1141() {
    // The measured healthy fleet baseline (live CAM1 2026-08-20, 1/1000 shutter).
    assert_agree(&[7.8, 7.5, 7.7, 7.6, 7.9, 7.4, 7.6, 7.6]);
}

#[test]
fn shell_and_rust_classify_agree_on_sustained_blur_1141() {
    assert_agree(&[1.4, 1.1, 0.9, 1.3, 1.2, 1.0, 1.5]);
}

#[test]
fn shell_and_rust_classify_agree_on_blur_with_one_spike_1141() {
    assert_agree(&[1.2, 0.8, 7.6, 1.0, 1.1, 0.9]);
}

#[test]
fn shell_and_rust_classify_agree_on_thin_telemetry_1141() {
    assert_agree(&[1.0, 1.0]);
}

#[test]
fn shell_and_rust_classify_agree_at_floor_and_just_above_1141() {
    assert_agree(&[OPTICAL_PREFLIGHT_ROUGH_FLOOR; OPTICAL_PREFLIGHT_MIN_SAMPLES]);
    assert_agree(&[OPTICAL_PREFLIGHT_ROUGH_FLOOR + 0.5; OPTICAL_PREFLIGHT_MIN_SAMPLES]);
}

/// A bare number on an unrelated journal line (e.g. "NDI display: 16.0 fps") must NOT be counted
/// as a rough= sample — the shell extraction mirrors src/optical_preflight.rs::parse_rough_samples.
#[test]
fn shell_classify_ignores_bare_numbers_on_other_lines_1141() {
    let mixed = "\
capture chroma: u_dev=6.3 v_dev=10.4 rough=7.8 -> colour
NDI display: 16.0 fps (1920x1080 -> 1920x1080)
Streaming: 60.1 fps emitted / 61.1 fps captured
capture chroma: u_dev=6.4 v_dev=9.3 rough=1.0 -> colour";
    // Only two rough= samples (7.8, 1.0) — below the minimum → INSUFFICIENT, NOT a 16.0-fed HEALTHY.
    assert_eq!(
        classify_shell(mixed).split_whitespace().next(),
        Some("INSUFFICIENT")
    );
}

// ---- WIRING: recording-e2e.sh must source the lib AND invoke the assert (the #675 one-liner) ----

#[test]
fn recording_e2e_sources_the_optical_preflight_lib_1141() {
    let sh = read("scripts/recording-e2e.sh");
    assert!(
        sh.contains(". \"$HERE/lib/optical-preflight.sh\""),
        "recording-e2e.sh must source scripts/lib/optical-preflight.sh"
    );
}

#[test]
fn recording_e2e_invokes_the_optical_preflight_assert_1141() {
    let sh = read("scripts/recording-e2e.sh");
    assert!(
        sh.contains("optical_preflight_assert "),
        "recording-e2e.sh must invoke optical_preflight_assert as a plain [0/8] statement"
    );
}
