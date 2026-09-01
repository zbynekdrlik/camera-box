//! #1265 task 3 — `scripts/lib/av-sync-apply-guard.sh`, the I/O gather + orchestration for the
//! #856 rig-wide A/V apply STABILITY GUARD. The pure refusal decision is
//! `scripts/av_sync_apply_guard.py` (pytest Tier-0, test_av_sync_apply_guard_1265.py); this harness
//! proves the sourced BASH lib reads the inputs correctly AND — critically — is `set -euo pipefail`
//! safe (it runs in recording-e2e.sh's cleanup() EXIT trap, so a no-match/parse-failure that
//! returned non-zero would `set -e`-abort the whole run, the #1133 class).
//!
//! Tier-0 (camera-box #477/#557): local cargo compilation is blocked, so this harness RUNS ON CI
//! only; its assertions are pure bash+python the identical way they run live, and the load-bearing
//! python one-liners were verified locally standalone. `cargo fmt --all --check` proves this parses.

use std::path::PathBuf;
use std::process::Command;

fn lib_script() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/lib/av-sync-apply-guard.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn guard_py() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/av_sync_apply_guard.py");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the lib under the CALLER's EXACT `set -euo pipefail` context (what recording-e2e.sh uses)
/// and run `body`. Returns (stdout, success). A non-zero exit here means a function `set -e`-aborted
/// on some input — exactly the phantom-fail this harness exists to catch.
fn run_under_set_e(work: &std::path::Path, body: &str) -> (String, bool) {
    let harness = format!("set -euo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("GUARD", guard_py())
        .env("WORK", work)
        .output()
        .expect("failed to run bash harness");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn write(dir: &std::path::Path, name: &str, content: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p.to_string_lossy().into_owned()
}

const UNSTABLE_VERDICT: &str = r#"{"all_cambox_av_sync": {"residual_median_ms": -126.0, "residual_spread_ms": 20.8, "gate_pass": false, "gate_tolerance_ms": 90, "expected_ms": 0}}"#;
const STABLE_VERDICT: &str = r#"{"all_cambox_av_sync": {"residual_median_ms": 16.9, "residual_spread_ms": 20.0, "gate_pass": true, "gate_tolerance_ms": 90, "expected_ms": 0}}"#;

#[test]
fn read_verdict_residual_reads_median_and_spread() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", UNSTABLE_VERDICT);
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "echo \"m=$(av_sync_read_verdict_residual '{v}' residual_median_ms)\"\n\
             echo \"s=$(av_sync_read_verdict_residual '{v}' residual_spread_ms)\"\n\
             echo END"
        ),
    );
    assert!(ok, "must not set -e-abort: {out}");
    assert!(out.contains("m=-126.0"), "{out}");
    assert!(out.contains("s=20.8"), "{out}");
    assert!(out.contains("END"), "reached the end (no abort): {out}");
}

#[test]
fn read_verdict_residual_absent_file_is_empty_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "echo \"m=[$(av_sync_read_verdict_residual /nope/x.json residual_median_ms)]\"\necho END",
    );
    assert!(ok, "absent verdict must not abort under set -e: {out}");
    assert!(out.contains("m=[]"), "{out}");
    assert!(out.contains("END"), "{out}");
}

#[test]
fn read_last_applied_absent_is_empty_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "echo \"l=[$(av_sync_read_last_applied_offset /nope/last.json)]\"\necho END",
    );
    assert!(ok, "absent last-applied must not abort under set -e: {out}");
    assert!(out.contains("l=[]"), "{out}");
    assert!(out.contains("END"), "{out}");
}

#[test]
fn decide_holds_an_unstable_run() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", UNSTABLE_VERDICT);
    // band "" (pre-deploy), no last-applied -> the residual-ceiling condition must still HOLD.
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "r=\"$(av_sync_apply_guard_decide '{v}' '' '-283.0' \"$GUARD\" /nope/last.json)\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(ok, "decide must not abort under set -e: {out}");
    assert!(out.contains("residual"), "unstable run must HOLD: {out}");
    assert!(out.contains("END"), "{out}");
}

#[test]
fn decide_proceeds_a_stable_run() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", STABLE_VERDICT);
    let last = write(
        d.path(),
        "last.json",
        r#"{"source": "NDI 2ME PGM", "offset_ms": -285.0}"#,
    );
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "r=\"$(av_sync_apply_guard_decide '{v}' 'HEALTHY' '-283.0' \"$GUARD\" '{last}')\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(ok, "decide must not abort under set -e: {out}");
    assert!(
        out.contains("hold=[]"),
        "stable run must PROCEED (empty reason): {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn decide_holds_a_drifting_band() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", STABLE_VERDICT); // small residual...
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "r=\"$(av_sync_apply_guard_decide '{v}' 'DRIFTING' '-283.0' \"$GUARD\" /nope/last.json)\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(ok, "decide must not abort under set -e: {out}");
    // ...but a DRIFTING band HOLDs regardless of the small residual.
    assert!(
        out.to_lowercase().contains("drifting") || out.to_lowercase().contains("band"),
        "a DRIFTING band must HOLD: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

// #1265 finding 1: persist COPIES the calibrate-written success file (full schema, incl.
// applied_latency_ms) to the dev1 reference -- it does NOT re-write a {source,offset_ms,ts} schema.
const OUTDIR_SUCCESS_FILE: &str =
    r#"{"source": "NDI 2ME PGM", "offset_ms": -283.0, "applied_latency_ms": 976, "ts": 1.0}"#;

#[test]
fn persist_copies_the_full_schema_and_feeds_the_jump_condition() {
    let d = tempfile::tempdir().unwrap();
    let last = d.path().join("last.json").to_string_lossy().into_owned();
    let src = write(d.path(), "av-sync-last-run.json", OUTDIR_SUCCESS_FILE);
    let v = write(d.path(), "verdict.json", STABLE_VERDICT);
    // 1) persist by COPYING the OUTDIR success file; 2) read offset_ms back; 3) applied_latency_ms is
    // preserved (finding 1 -- the live data contract latency_pins_snapshot.py/rig-mode.sh/drift-guard
    // read); 4) a proposed value 200ms away now HOLDs (the jump/anti-oscillation condition).
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_applied_offset '{src}' '{last}'\n\
             echo \"lb=$(av_sync_read_last_applied_offset '{last}')\"\n\
             echo \"al=$(python3 -c \"import json;print(json.load(open('{last}')).get('applied_latency_ms'))\")\"\n\
             r=\"$(av_sync_apply_guard_decide '{v}' 'HEALTHY' '-83.0' \"$GUARD\" '{last}')\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(ok, "persist/read must not abort under set -e: {out}");
    assert!(
        out.contains("lb=-283.0"),
        "persisted offset must read back: {out}"
    );
    assert!(
        out.contains("al=976"),
        "finding 1: the full schema (applied_latency_ms) must be preserved by the copy: {out}"
    );
    assert!(
        out.to_lowercase().contains("swung") || out.to_lowercase().contains("last applied"),
        "a 200ms swing from the persisted last-applied must HOLD: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn persist_missing_or_empty_src_is_a_noop_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let last = d.path().join("last.json").to_string_lossy().into_owned();
    let empty = write(d.path(), "empty.json", "");
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_applied_offset '/nonexistent/src.json' '{last}'\n\
             av_sync_persist_applied_offset '{empty}' '{last}'\n\
             av_sync_persist_applied_offset '' '{last}'\n\
             echo \"exists=$([ -f '{last}' ] && echo yes || echo no)\"\necho END"
        ),
    );
    assert!(
        ok,
        "missing/empty-src persist must not abort under set -e: {out}"
    );
    assert!(
        out.contains("exists=no"),
        "a missing/empty src must NOT write the dest file: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn persist_hold_reason_writes_a_durable_file_finding6a() {
    let d = tempfile::tempdir().unwrap();
    let hold = d
        .path()
        .join("hold-last.txt")
        .to_string_lossy()
        .into_owned();
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_hold_reason 'run residual median -111.5ms exceeds ...' '{hold}'\n\
             echo \"hold_has=$(grep -c residual '{hold}' 2>/dev/null || echo 0)\"\n\
             av_sync_persist_hold_reason '' '{hold}2'\n\
             echo \"empty_wrote=$([ -f '{hold}2' ] && echo yes || echo no)\"\necho END"
        ),
    );
    assert!(
        ok,
        "durable hold-reason write must not abort under set -e: {out}"
    );
    assert!(
        out.contains("hold_has=1"),
        "the hold reason must be written durably: {out}"
    );
    assert!(
        out.contains("empty_wrote=no"),
        "an empty reason must NOT write a file: {out}"
    );
    assert!(out.contains("END"), "{out}");
}
