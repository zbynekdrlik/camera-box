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

/// `lipsync_subtract_baseline` must return the RIG-ADDED delta (raw aggregated offset minus the
/// asset's own intrinsic baseline) -- issue 930 supervisor decision (issuecomment-5153948268):
/// the pinned asset measures its own -80ms intrinsic offset, so the raw SyncNet-on-rig-recording
/// mean is `intrinsic_asset + rig_chain_delta` until this subtraction isolates the rig's part.
#[test]
fn subtract_baseline_isolates_the_rig_added_delta_930() {
    let cmd = run_sourced("lipsync_subtract_baseline -75.0 -80.0");
    let val: f64 = cmd.trim().parse().expect("numeric output");
    assert_eq!(val, 5.0, "930: -75.0 - (-80.0) must be 5.0: {cmd}");
}

/// A positive aggregated offset with a positive baseline.
#[test]
fn subtract_baseline_positive_values_930() {
    let cmd = run_sourced("lipsync_subtract_baseline 90.0 80.0");
    let val: f64 = cmd.trim().parse().expect("numeric output");
    assert_eq!(val, 10.0, "930: 90.0 - 80.0 must be 10.0: {cmd}");
}

/// A negative aggregated offset with a zero baseline must pass through unchanged.
#[test]
fn subtract_baseline_negative_aggregated_zero_baseline_930() {
    let cmd = run_sourced("lipsync_subtract_baseline -37.5 0");
    let val: f64 = cmd.trim().parse().expect("numeric output");
    assert_eq!(val, -37.5, "930: -37.5 - 0 must be -37.5: {cmd}");
}

/// The final verdict call must carry the aggregated SyncNet offset through issue 930's own
/// `--syncnet-offset-ms` flag on the EXISTING `--av-sync`/`--av-marker-log` mode. Uses the `=`
/// form (`--syncnet-offset-ms=<value>`) as belt-and-braces alongside the recording-verdict-side
/// `allow_negative_numbers` fix -- the `=` form parses a leading `-` even on an older binary.
#[test]
fn verdict_cmd_wires_syncnet_offset_ms_onto_av_sync_mode() {
    let cmd = run_sourced(
        "lipsync_verdict_cmd /repo/target/debug/recording-verdict /tmp/qrqpsk.mp4 /tmp/markers.csv 37.5",
    );
    assert!(cmd.contains("--av-sync"));
    assert!(cmd.contains("/tmp/qrqpsk.mp4"));
    assert!(cmd.contains("--av-marker-log"));
    assert!(cmd.contains("/tmp/markers.csv"));
    assert!(cmd.contains("--syncnet-offset-ms=37.5"));
}

/// A NEGATIVE SyncNet offset (video earlier than audio -- half the real outcome space) must
/// survive into the built command string in the SAME `=` form, never dropped or mangled.
#[test]
fn verdict_cmd_wires_a_negative_syncnet_offset_ms_930() {
    let cmd = run_sourced(
        "lipsync_verdict_cmd /repo/target/debug/recording-verdict /tmp/qrqpsk.mp4 /tmp/markers.csv -37.5",
    );
    assert!(cmd.contains("--syncnet-offset-ms=-37.5"), "cmd: {cmd}");
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
/// SyncNet/recording-verdict. All required flags given (incl. --verdict-bin) so this test
/// exercises the FILE-EXISTENCE check specifically, not an earlier missing-flag check.
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
        .arg("--verdict-bin")
        .arg("/nonexistent/recording-verdict")
        .arg("--asset-baseline-ms")
        .arg("-80")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("lipsync.mp4") || stderr.contains("not found"),
        "930: must fail on the missing recording, not some earlier unrelated check: {stderr}"
    );
}

// --------------------------------------------------------------------------------------------- //
// End-to-end `main()` fakes -- issue 930 findings 5/6/7 (stale-chunk cleanup, <2-measured fails
// loud, stdout carries ONLY the final JSON). Real ffmpeg/python3/recording-verdict are replaced
// with tiny fakes so these prove the ORCHESTRATION behavior without any real media/SyncNet/probe
// dependency -- mirrors `tests/harness_lipsync_asset.rs`'s fake-ffmpeg-on-PATH pattern.
// --------------------------------------------------------------------------------------------- //

struct MainFakes {
    tmp: tempfile::TempDir,
}

impl MainFakes {
    /// Sets up: a fake `ffmpeg` on PATH that writes exactly 2 chunk files matching whatever
    /// `%03d`-style segment pattern it's given; a fake "measure" script (run via `bash`, not
    /// `python3`, through `LIPSYNC_PYTHON=bash`) that logs which media path it was called on to
    /// `measured-list.txt` and either succeeds or fails per `$FAKE_MEASURE_FAIL`; a fake
    /// "aggregate" script that writes a fixed valid report JSON; and a fake `recording-verdict`
    /// binary that prints one fixed JSON line to stdout. All prose the real Python tools would
    /// print goes to the fakes' OWN stdout so the redirect-to-stderr fix (finding 7) is provable.
    fn setup() -> Self {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        fs::write(
            bin.join("ffmpeg"),
            r#"#!/usr/bin/env bash
pattern=""
for a in "$@"; do
  case "$a" in *%03d*) pattern="$a" ;; esac
done
[ -n "$pattern" ] || { echo "fake ffmpeg: no %03d pattern in argv: $*" >&2; exit 1; }
printf -v f0 "$pattern" 0
printf -v f1 "$pattern" 1
: > "$f0"
: > "$f1"
"#,
        )
        .unwrap();

        let measure = tmp.path().join("fake-measure.sh");
        fs::write(
            &measure,
            r#"#!/usr/bin/env bash
media="" cal=""
while [ $# -gt 0 ]; do
  case "$1" in
    --media) media="$2"; shift 2 ;;
    --calibration-log) cal="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo "measure prose for $media"
echo "$media" >> "$(dirname "$cal")/measured-list.txt"
if [ -n "${FAKE_MEASURE_FAIL:-}" ]; then exit 1; fi
echo '{"offset_ms": 12.5}' >> "$cal"
"#,
        )
        .unwrap();

        let aggregate = tmp.path().join("fake-aggregate.sh");
        fs::write(
            &aggregate,
            r#"#!/usr/bin/env bash
report=""
while [ $# -gt 0 ]; do
  case "$1" in
    --report-json) report="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo "aggregate prose"
printf '%s' '{"n": 2, "n_total": 2, "mean_offset_ms": 12.5, "stdev_ms": 1.0, "ci95_ms": 2.0}' > "$report"
"#,
        )
        .unwrap();

        let verdict = tmp.path().join("fake-recording-verdict");
        fs::write(
            &verdict,
            "#!/usr/bin/env bash\necho '{\"fake_verdict\": true}'\n",
        )
        .unwrap();

        for exe in [
            bin.join("ffmpeg"),
            measure.clone(),
            aggregate.clone(),
            verdict.clone(),
        ] {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        Self { tmp }
    }

    fn measure(&self) -> PathBuf {
        self.tmp.path().join("fake-measure.sh")
    }
    fn aggregate(&self) -> PathBuf {
        self.tmp.path().join("fake-aggregate.sh")
    }
    fn verdict(&self) -> PathBuf {
        self.tmp.path().join("fake-recording-verdict")
    }
    fn bin_dir(&self) -> PathBuf {
        self.tmp.path().join("bin")
    }
}

fn run_main(
    fakes: &MainFakes,
    workdir: &std::path::Path,
    fail_measure: bool,
) -> std::process::Output {
    let lipsync = workdir.join("lipsync.mp4");
    let qrqpsk = workdir.join("qrqpsk.mp4");
    let markers = workdir.join("markers.csv");
    for f in [&lipsync, &qrqpsk, &markers] {
        fs::write(f, b"x").unwrap();
    }
    let path_env = format!(
        "{}:{}",
        fakes.bin_dir().display(),
        std::env::var("PATH").unwrap()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .arg("--lipsync-recording")
        .arg(&lipsync)
        .arg("--qrqpsk-recording")
        .arg(&qrqpsk)
        .arg("--qrqpsk-marker-log")
        .arg(&markers)
        .arg("--verdict-bin")
        .arg(fakes.verdict())
        .arg("--asset-baseline-ms")
        .arg("-80")
        .arg("--workdir")
        .arg(workdir)
        .env("PATH", path_env)
        .env("LIPSYNC_PYTHON", "bash")
        .env("LIPSYNC_AV_SYNC_MEASURE", fakes.measure())
        .env("LIPSYNC_AV_SYNC_CALIBRATE", fakes.aggregate());
    if fail_measure {
        cmd.env("FAKE_MEASURE_FAIL", "1");
    }
    cmd.output().expect("spawn bash")
}

/// A pre-existing `chunk-999.mp4` left in an explicit, reused `--workdir` from a PRIOR run must
/// be removed before the new segmenting/measuring pass -- it must never end up in the measured
/// list alongside (or instead of) the fresh chunks (930 finding 5).
#[test]
fn main_removes_stale_chunks_from_a_reused_workdir_930() {
    let fakes = MainFakes::setup();
    let workdir = tempfile::tempdir().expect("workdir");
    fs::write(workdir.path().join("chunk-999.mp4"), b"stale").unwrap();

    let out = run_main(&fakes, workdir.path(), false);
    assert!(
        out.status.success(),
        "930: expected success: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !workdir.path().join("chunk-999.mp4").exists(),
        "930: stale chunk-999.mp4 must be removed before segmenting"
    );
    let measured_list =
        fs::read_to_string(workdir.path().join("measured-list.txt")).unwrap_or_default();
    assert!(
        !measured_list.contains("chunk-999.mp4"),
        "930: stale chunk must never be measured: {measured_list}"
    );
}

/// When fewer than 2 chunks measure successfully, `main` must FAIL LOUD (non-zero exit, clear
/// message) rather than silently proceeding with `|| true` into a bogus-precision aggregate
/// (930 finding 6 -- `src/lipsync_cross_check.rs`'s tolerance math assumes >=2 confident windows).
#[test]
fn main_fails_loud_when_fewer_than_two_chunks_measure_930() {
    let fakes = MainFakes::setup();
    let workdir = tempfile::tempdir().expect("workdir");

    let out = run_main(&fakes, workdir.path(), true);
    assert!(
        !out.status.success(),
        "930: must fail loud when every chunk measurement fails"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("only 0/2") || stderr.contains("need >=2"),
        "930: stderr must clearly explain the insufficient-measurement failure: {stderr}"
    );
}

/// stdout must carry ONLY the final recording-verdict JSON -- the intermediate measure/aggregate
/// tools' own prose must land on stderr, never mixed into stdout (930 finding 7, the header's own
/// documented contract).
#[test]
fn main_stdout_carries_only_the_final_json_930() {
    let fakes = MainFakes::setup();
    let workdir = tempfile::tempdir().expect("workdir");

    let out = run_main(&fakes, workdir.path(), false);
    assert!(
        out.status.success(),
        "930: expected success: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        r#"{"fake_verdict": true}"#,
        "930: stdout must be exactly the final verdict JSON, nothing else"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("measure prose") && stderr.contains("aggregate prose"),
        "930: the fakes' own prose must appear on STDERR instead: {stderr}"
    );
}

/// `--verdict-bin` must be REQUIRED with no local-build default -- recording-verdict needs
/// `--features probe` (Tier 0 forbids building that locally; CI/download only).
#[test]
fn verdict_bin_is_required_with_no_local_build_default() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--lipsync-recording")
        .arg("/tmp/a.mp4")
        .arg("--qrqpsk-recording")
        .arg("/tmp/b.mp4")
        .arg("--qrqpsk-marker-log")
        .arg("/tmp/c.csv")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--verdict-bin"),
        "930: missing --verdict-bin must fail with a clear message: {stderr}"
    );
}

/// `--asset-baseline-ms` must be REQUIRED -- the pinned asset has its own intrinsic A/V offset
/// (930 supervisor decision, issuecomment-5153948268) that must be subtracted before the verdict.
#[test]
fn asset_baseline_ms_is_required() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--lipsync-recording")
        .arg("/tmp/a.mp4")
        .arg("--qrqpsk-recording")
        .arg("/tmp/b.mp4")
        .arg("--qrqpsk-marker-log")
        .arg("/tmp/c.csv")
        .arg("--verdict-bin")
        .arg("/tmp/recording-verdict")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--asset-baseline-ms"),
        "930: missing --asset-baseline-ms must fail with a clear message: {stderr}"
    );
}
