//! issue 930 — `scripts/lipsync-cross-check.sh`'s pure command-builder functions (ffmpeg segment
//! split, the SyncNet per-chunk measure + aggregate calls reusing issue 917/805's ALREADY-TESTED
//! Python engine, the final recording-verdict --av-sync --syncnet-offset-ms call, and the tiny
//! report-JSON field extractor), sourced and called directly. NEVER touches the network or a
//! real recording decode -- mirrors `tests/harness_lipsync_asset.rs` / `_test_mode.rs`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lipsync-cross-check.sh")
}

fn run_sourced(call: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$1\"; {call}"))
        .arg("bash")
        .arg(script())
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "sourced call `{call}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn lipsync_cross_check_script_exists_and_is_executable() {
    let meta = fs::metadata(script())
        .unwrap_or_else(|e| panic!("scripts/lipsync-cross-check.sh missing: {e}"));
    assert!(meta.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "scripts/lipsync-cross-check.sh must be executable"
        );
    }
}

/// The segment command must split without re-encoding (`-c copy`) and use the SAME ~20s window
/// av_sync_measure.py's own `--secs 20` default already establishes -- never a second, drifted
/// windowing convention.
#[test]
fn segment_cmd_uses_stream_copy_and_the_given_window() {
    let cmd = run_sourced("lipsync_segment_cmd /tmp/lipsync.mp4 20 /tmp/chunk-%03d.mp4");
    assert!(cmd.contains("ffmpeg"));
    assert!(cmd.contains("-c copy"), "930: must not re-encode: {cmd}");
    assert!(cmd.contains("-f segment"));
    assert!(cmd.contains("-segment_time 20"));
    assert!(cmd.contains("/tmp/lipsync.mp4"));
    assert!(cmd.contains("/tmp/chunk-%03d.mp4"));
}

/// Each chunk-measure call must reuse av_sync_measure.py's EXISTING `--media`/`--calibration-log`
/// flags verbatim (issue 917's engine) -- never a second, parallel measurement path.
#[test]
fn measure_chunk_cmd_reuses_av_sync_measure_py_verbatim() {
    let cmd = run_sourced(
        "lipsync_measure_chunk_cmd python3 /repo/scripts/av_sync_measure.py /tmp/chunk-000.mp4 /tmp/cal.jsonl",
    );
    assert!(cmd.contains("av_sync_measure.py"));
    assert!(cmd.contains("--media"));
    assert!(cmd.contains("/tmp/chunk-000.mp4"));
    assert!(cmd.contains("--calibration-log"));
    assert!(cmd.contains("/tmp/cal.jsonl"));
}

/// The aggregate call must reuse av_sync_calibrate.py's EXISTING `--calibrate`/`--report-json`
/// flags (issue 805's SEM-shrinking aggregator, already unit-tested) -- zero new math here.
#[test]
fn aggregate_cmd_reuses_av_sync_calibrate_py_verbatim() {
    let cmd = run_sourced(
        "lipsync_aggregate_cmd python3 /repo/scripts/av_sync_calibrate.py /tmp/cal.jsonl /tmp/agg.json",
    );
    assert!(cmd.contains("av_sync_calibrate.py"));
    assert!(cmd.contains("--calibrate"));
    assert!(cmd.contains("/tmp/cal.jsonl"));
    assert!(cmd.contains("--report-json"));
    assert!(cmd.contains("/tmp/agg.json"));
}

/// The final verdict call must carry the aggregated SyncNet offset through issue 930's own
/// `--syncnet-offset-ms` flag on the EXISTING `--av-sync`/`--av-marker-log` mode.
#[test]
fn verdict_cmd_wires_syncnet_offset_ms_onto_av_sync_mode() {
    let cmd = run_sourced(
        "lipsync_verdict_cmd /repo/target/debug/recording-verdict /tmp/qrqpsk.mp4 /tmp/markers.csv 37.5",
    );
    assert!(cmd.contains("--av-sync"));
    assert!(cmd.contains("/tmp/qrqpsk.mp4"));
    assert!(cmd.contains("--av-marker-log"));
    assert!(cmd.contains("/tmp/markers.csv"));
    assert!(cmd.contains("--syncnet-offset-ms"));
    assert!(cmd.contains("37.5"));
}

/// The report-JSON field extractor must pull the exact `mean_offset_ms` field
/// av_sync_calibrate.py --calibrate's --report-json writes (a real fixture shape, not guessed).
#[test]
fn mean_offset_extractor_reads_the_real_report_json_shape() {
    let json =
        r#"{"n": 3, "n_total": 4, "mean_offset_ms": -12.5, "stdev_ms": 4.2, "ci95_ms": 8.1}"#;
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". \"$1\"; lipsync_mean_offset_from_report_json '{json}'"
        ))
        .arg("bash")
        .arg(script())
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let val: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert_eq!(val, -12.5);
}

/// `main` must fail loud (never silently proceed) when a required flag is missing, before
/// touching the filesystem/network at all.
#[test]
fn main_fails_loud_on_missing_required_args() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--lipsync-recording")
        .arg("/tmp/does-not-matter.mp4")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: missing --qrqpsk-recording/--qrqpsk-marker-log must fail loud"
    );
}

/// `main` must fail loud when the given recordings don't exist, before ever calling ffmpeg/
/// SyncNet/recording-verdict.
#[test]
fn main_fails_loud_on_nonexistent_recordings() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--lipsync-recording")
        .arg("/nonexistent/lipsync.mp4")
        .arg("--qrqpsk-recording")
        .arg("/nonexistent/qrqpsk.mp4")
        .arg("--qrqpsk-marker-log")
        .arg("/nonexistent/markers.csv")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
}
