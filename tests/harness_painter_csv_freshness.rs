//! #359 — behavioral tests for the painter-CSV freshness verdict (`scripts/lib/painter-csv-freshness.sh`).
//!
//! The recording-e2e.sh fail-loud gate trusts this verdict to reject a STALE/absent cam2 painter
//! ground-truth before recording-verdict runs (run 354002 pulled a 14.9h-offset, 40s-span stale CSV
//! and emitted a fake catastrophic FAIL). Here we EXECUTE the real awk threshold logic against
//! synthetic CSVs — not just assert the gate's presence in the script — so a future edit that
//! breaks the span/offset thresholds fails loudly in CI. Pure shell, no rig, no ssh.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/painter-csv-freshness.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("painter-freshness-{}-{}", std::process::id(), name));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a painter CSV (header + the given `(tick, gen_ts_ns)` rows; flip_ts_ns = gen_ts_ns) and
/// return its path.
fn write_csv(name: &str, rows: &[(u64, i128)]) -> PathBuf {
    let dir = scratch(name);
    let path = dir.join("painter.csv");
    let mut s = String::from("tick,gen_ts_ns,flip_ts_ns\n");
    for (tick, gen) in rows {
        s.push_str(&format!("{tick},{gen},{gen}\n"));
    }
    fs::write(&path, s).unwrap();
    path
}

/// Call `painter_csv_freshness <csv> <run_start> <dur>` and return (exit_code, "verdict span offset").
fn freshness(csv: &str, run_start: i128, dur: u64) -> (i32, String) {
    let script = format!(
        r#"set -u
. "{lib}"
painter_csv_freshness "{csv}" {run_start} {dur}
"#,
        lib = lib().display(),
        csv = csv,
        run_start = run_start,
        dur = dur,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run painter_csv_freshness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

const RUN_START: i128 = 1_751_000_000; // a fixed epoch-seconds run start for the synthetic CSVs
const NS: i128 = 1_000_000_000;
const DUR: u64 = 1800;

#[test]
fn fresh_csv_passes_with_ok_and_zero_exit() {
    // span ≈ DURATION, first gen_ts == run start → OK, exit 0.
    let csv = write_csv(
        "fresh",
        &[(0, RUN_START * NS), (108_000, (RUN_START + 1800) * NS)],
    );
    let (code, out) = freshness(csv.to_str().unwrap(), RUN_START, DUR);
    assert_eq!(
        code, 0,
        "fresh CSV must exit 0 — got code={code} out={out:?}"
    );
    assert!(
        out.starts_with("OK "),
        "fresh CSV verdict must be OK — got {out:?}"
    );
}

#[test]
fn short_span_csv_is_rejected_with_short_and_nonzero_exit() {
    // The stale-leftover signature: a tiny 40s span against a 1800s run → SHORT, exit 1.
    let csv = write_csv(
        "short",
        &[(0, RUN_START * NS), (2_400, (RUN_START + 40) * NS)],
    );
    let (code, out) = freshness(csv.to_str().unwrap(), RUN_START, DUR);
    assert_eq!(
        code, 1,
        "a too-short CSV must FAIL LOUD (exit 1) — got code={code} out={out:?}"
    );
    assert!(
        out.starts_with("SHORT "),
        "a 40s span vs 1800s run must be SHORT — got {out:?}"
    );
}

#[test]
fn hours_offset_csv_is_rejected_with_offset_and_nonzero_exit() {
    // The run-354002 signature: full span but first gen_ts 14.9h before run start → OFFSET, exit 1.
    let off = RUN_START - 53_640; // 14.9h earlier
    let csv = write_csv("offset", &[(0, off * NS), (108_000, (off + 1800) * NS)]);
    let (code, out) = freshness(csv.to_str().unwrap(), RUN_START, DUR);
    assert_eq!(
        code, 1,
        "an hours-offset CSV must FAIL LOUD (exit 1) — got code={code} out={out:?}"
    );
    assert!(
        out.starts_with("OFFSET "),
        "a 14.9h gen_ts offset must be OFFSET — got {out:?}"
    );
}

#[test]
fn header_only_csv_is_rejected_with_empty_and_nonzero_exit() {
    // The painter started but painted ~nothing (header only) → EMPTY, exit 1.
    let csv = write_csv("emptyrows", &[]);
    let (code, out) = freshness(csv.to_str().unwrap(), RUN_START, DUR);
    assert_eq!(
        code, 1,
        "a header-only CSV must FAIL LOUD (exit 1) — got code={code} out={out:?}"
    );
    assert!(
        out.starts_with("EMPTY "),
        "a no-data CSV must be EMPTY — got {out:?}"
    );
}

#[test]
fn missing_or_empty_file_is_rejected_with_missing_and_nonzero_exit() {
    // No CSV at all (the painter never wrote one) → MISSING, exit 1.
    let (code, out) = freshness("/nonexistent/painter.csv", RUN_START, DUR);
    assert_eq!(
        code, 1,
        "an absent CSV must FAIL LOUD (exit 1) — got code={code} out={out:?}"
    );
    assert!(
        out.starts_with("MISSING "),
        "an absent CSV must be MISSING — got {out:?}"
    );
}

#[test]
fn lib_is_sourceable_without_side_effects() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(r#". "{}"; echo ok"#, lib().display()))
        .output()
        .expect("source lib");
    assert!(
        out.status.success(),
        "sourcing the lib must be a no-op success"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
}
