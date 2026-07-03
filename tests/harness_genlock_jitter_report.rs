//! #272 — genlock-jitter-report CLI: end-to-end behavioural lock through the actual
//! compiled binary (mirrors `tests/harness_render_budget_gate.rs`'s pattern for a "thin
//! gate binary" over a Tier-0 pure kernel).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const SAMPLE_LOG_CAM1_A: &str = "14:00:00.001: genlock-fifo audit 'NDI cam1': \
    received=1000 consumed=999 underruns=2 holds=5 overruns=0 backward_steps=0 \
    dropped_due=0 relocks=0 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 \
    (\u{2248}1 frames @ 30.000fps) src_latency_ms=0 global_latency_ms=3 preload=1 \
    (=33 ms) reserve_ms=3 cap=5 empty_run=0 (re-arm@10) ts_present=123456789012 \
    ts_due=987 ts_head_skew_ms=-2 (#70/#97/#126/#147/#148/#184/#235/#245/#401)";

const SAMPLE_LOG_CAM1_B: &str = "14:00:05.001: genlock-fifo audit 'NDI cam1': \
    received=1150 consumed=1149 underruns=3 holds=8 overruns=0 backward_steps=0 \
    dropped_due=1 relocks=0 late_holds=1 locked=1 depth=3 peak=6 latency_ms=3 \
    (\u{2248}1 frames @ 30.000fps) src_latency_ms=0 global_latency_ms=3 preload=1 \
    (=33 ms) reserve_ms=3 cap=5 empty_run=0 (re-arm@10) ts_present=123456839012 \
    ts_due=990 ts_head_skew_ms=7 (#70/#97/#126/#147/#148/#184/#235/#245/#401)";

/// The Rust binary source must exist.
#[test]
fn genlock_jitter_report_bin_src_exists() {
    let path = format!(
        "{}/src/bin/genlock-jitter-report.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/genlock-jitter-report.rs not found (#272)."
    );
}

/// A two-sample window for one source reports the correct DELTA counters and the
/// header, through the actual compiled binary reading real stdin bytes.
#[test]
fn reports_delta_counters_and_header_for_a_real_log_window() {
    let bin = env!("CARGO_BIN_EXE_genlock-jitter-report");
    let log = format!("{SAMPLE_LOG_CAM1_A}\n{SAMPLE_LOG_CAM1_B}\n");
    let (code, stdout) = run(bin, &log);
    assert_eq!(code, 0, "a log with audit lines must exit 0");
    assert!(stdout.contains("source"), "must print the header row");
    assert!(stdout.contains("NDI cam1"), "must report the source name");
    // underruns 2->3 = delta 1, holds 5->8 = delta 3, dropped_due 0->1 = delta 1,
    // late_holds 0->1 = delta 1, max |skew| = max(2,7) = 7.
    let data_line = stdout
        .lines()
        .find(|l| l.contains("NDI cam1"))
        .expect("data line for NDI cam1");
    let fields: Vec<&str> = data_line.split_whitespace().collect();
    // source samples latency_ms d_underrun d_hold d_dropped_due d_relock d_latehold max_abs_skew_ms mean_abs_skew_ms peak_depth
    assert_eq!(fields[0], "NDI");
    assert_eq!(fields[1], "cam1");
    assert_eq!(fields[2], "2", "2 samples in the window");
    assert_eq!(fields[3], "3", "latency_ms=3");
    assert_eq!(fields[4], "1", "delta_underruns = 3-2");
    assert_eq!(fields[5], "3", "delta_holds = 8-5");
    assert_eq!(fields[6], "1", "delta_dropped_due = 1-0");
    assert_eq!(fields[7], "0", "delta_relocks = 0-0");
    assert_eq!(fields[8], "1", "delta_late_holds = 1-0");
    assert_eq!(fields[9], "7", "max_abs_head_skew_ms = max(|-2|,|7|)");
}

/// A log with no `genlock-fifo audit` lines at all fails closed (exit 2), never a
/// silent empty PASS.
#[test]
fn no_audit_lines_fails_closed() {
    let bin = env!("CARGO_BIN_EXE_genlock-jitter-report");
    let (code, _stdout) = run(bin, "just some unrelated OBS startup banner\n");
    assert_eq!(code, 2, "no audit lines must exit 2 (fail closed)");
}

/// `--file <path>` reads from the given path instead of stdin.
#[test]
fn reads_from_file_arg() {
    let bin = env!("CARGO_BIN_EXE_genlock-jitter-report");
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "camera-box-jitter-report-test-{}.log",
        std::process::id()
    ));
    std::fs::write(&path, format!("{SAMPLE_LOG_CAM1_A}\n")).expect("write temp log");

    let out = Command::new(bin)
        .arg("--file")
        .arg(&path)
        .output()
        .expect("run genlock-jitter-report --file");
    let _ = std::fs::remove_file(&path);

    assert_eq!(out.status.code(), Some(0), "must exit 0 for a real file");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("NDI cam1"));
}

fn run(bin: &str, stdin: &str) -> (i32, String) {
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn genlock-jitter-report");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}
