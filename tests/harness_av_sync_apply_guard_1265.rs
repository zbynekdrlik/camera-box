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

fn gain_py() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/av_sync_loop_gain.py");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn history_py() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/av_sync_history.py");
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
        .env("GAIN", gain_py())
        .env("HIST", history_py())
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
// #1265b: a sustained real upstream step -- residual -111 both this run and the persisted prev.
const STEP_VERDICT: &str = r#"{"all_cambox_av_sync": {"residual_median_ms": -111.0, "residual_spread_ms": 25.0, "gate_pass": false, "gate_tolerance_ms": 90, "expected_ms": 0}}"#;

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

// -------- #1265b SUSTAINED two-run confirmation (supervisor 2026-09-02): persist EVERY run's
// residual, and let a confirmed real step PROCEED instead of holding forever. --------

#[test]
fn persist_residual_then_read_prev_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", STEP_VERDICT);
    let last = write(
        d.path(),
        "last.json",
        r#"{"source": "NDI 2ME PGM", "offset_ms": -283.0, "applied_latency_ms": 926}"#,
    );
    let rlast = d
        .path()
        .join("residual-last.json")
        .to_string_lossy()
        .into_owned();
    // persist THIS run's residual (reads residual from the verdict, pin from av-sync-last.json),
    // then read it back via av_sync_read_prev_residual -> "<residual>\t<age>".
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_residual '{v}' 'run-42' '{last}' '{rlast}'\n\
             echo \"pin=$(python3 -c \"import json;print(json.load(open('{rlast}')).get('pin_at_measure'))\")\"\n\
             pr=\"$(av_sync_read_prev_residual '{rlast}')\"\n\
             echo \"resid=$(printf '%s' \"$pr\" | cut -f1)\"\n\
             echo \"age_ok=$([ -n \"$(printf '%s' \"$pr\" | cut -f2)\" ] && echo yes || echo no)\"\necho END"
        ),
    );
    assert!(ok, "persist/read-prev must not abort under set -e: {out}");
    assert!(
        out.contains("resid=-111.0"),
        "the persisted residual must read back: {out}"
    );
    assert!(
        out.contains("pin=926"),
        "pin_at_measure must be recorded from av-sync-last.json: {out}"
    );
    assert!(
        out.contains("age_ok=yes"),
        "a persisted ts must yield a computable age: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn decide_sustained_via_the_persisted_prev_file_proceeds() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", STEP_VERDICT);
    let last = write(
        d.path(),
        "last.json",
        r#"{"source": "NDI 2ME PGM", "offset_ms": -283.0, "applied_latency_ms": 926}"#,
    );
    let rlast = d
        .path()
        .join("residual-last.json")
        .to_string_lossy()
        .into_owned();
    // 1) persist THIS run's residual (-111); 2) decide the NEXT run (also -111, so it agrees with the
    // just-persisted prev within tol, fresh ts) -> SUSTAINED -> PROCEED (empty), even though
    // |residual|>60. Proves the real-step convergence path end-to-end through the sourced lib.
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_residual '{v}' 'run-1' '{last}' '{rlast}'\n\
             r=\"$(av_sync_apply_guard_decide '{v}' 'HEALTHY' '-111.0' \"$GUARD\" '{last}' '{rlast}')\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(ok, "decide with prev must not abort under set -e: {out}");
    assert!(
        out.contains("hold=[]"),
        "a residual matching the persisted prev must PROCEED (sustained): {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn decide_first_run_no_prev_file_holds() {
    let d = tempfile::tempdir().unwrap();
    let v = write(d.path(), "verdict.json", STEP_VERDICT);
    // no residual-last file exists yet -> first off-baseline run -> HOLD (outlier protection).
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "r=\"$(av_sync_apply_guard_decide '{v}' 'HEALTHY' '-111.0' \"$GUARD\" /nope/last.json /nope/residual.json)\"\n\
             echo \"hold=[$r]\"\necho END"
        ),
    );
    assert!(
        ok,
        "decide with no prev file must not abort under set -e: {out}"
    );
    assert!(
        out.to_lowercase().contains("2nd consistent run"),
        "a first off-baseline run with no prev must HOLD awaiting confirmation: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn persist_residual_missing_verdict_is_a_noop_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let rlast = d
        .path()
        .join("residual-last.json")
        .to_string_lossy()
        .into_owned();
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_persist_residual '/nope/verdict.json' 'run-x' '/nope/last.json' '{rlast}'\n\
             echo \"wrote=$([ -f '{rlast}' ] && echo yes || echo no)\"\necho END"
        ),
    );
    assert!(ok, "a missing verdict must not abort under set -e: {out}");
    assert!(
        out.contains("wrote=no"),
        "no residual_median_ms -> no residual-last file written: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

// -------- #1265 loop-gain damping (av_sync_apply_loop_gain) + per-run history (av_sync_append_history):
// both run as bare statements / `$(...)` in the cleanup() EXIT trap, so they MUST be set -euo pipefail
// safe (the #1133 class) -- committed proof, not just a local bash run. --------

#[test]
fn apply_loop_gain_default_damps_and_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "unset AV_SYNC_LOOP_GAIN\n\
         p=\"$(av_sync_apply_loop_gain '-61.354' \"$GAIN\")\"\n\
         echo \"d=$(printf '%s' \"$p\" | cut -f1)\"\n\
         echo \"g=$(printf '%s' \"$p\" | cut -f2)\"\necho END",
    );
    assert!(ok, "apply_loop_gain must not abort under set -e: {out}");
    assert!(
        out.contains("d=-24.5416"),
        "default gain 0.4 must damp -61.354 -> -24.5416: {out}"
    );
    assert!(out.contains("g=0.4000"), "default gain 0.4: {out}");
    assert!(out.contains("END"), "{out}");
}

#[test]
fn apply_loop_gain_env_override_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "p=\"$(AV_SYNC_LOOP_GAIN=0.5 av_sync_apply_loop_gain '-61.354' \"$GAIN\")\"\n\
         echo \"g=$(printf '%s' \"$p\" | cut -f2)\"\necho END",
    );
    assert!(ok, "apply_loop_gain env override must not abort: {out}");
    assert!(
        out.contains("g=0.5000"),
        "AV_SYNC_LOOP_GAIN override must flow through: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn apply_loop_gain_missing_helper_is_empty_and_warns_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "p=\"$(av_sync_apply_loop_gain '-61.354' /nope/gain.py 2>\"$WORK/err\")\"\n\
         echo \"d=[$p]\"\n\
         grep -qi warning \"$WORK/err\" && echo WARNED || echo NOWARN\necho END",
    );
    assert!(
        ok,
        "a missing gain helper must not abort under set -e: {out}"
    );
    assert!(
        out.contains("d=[]"),
        "a missing helper -> empty damped (the apply is skipped): {out}"
    );
    assert!(
        out.contains("WARNED"),
        "a missing helper must WARN (never silently disable the #856 apply): {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn append_history_proceed_records_applied_pin_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let rlast = write(
        d.path(),
        "residual-last.json",
        r#"{"run_id": "run-9", "ts": 1788390000.0, "residual_median_ms": -61.35, "residual_spread_ms": 36.7, "pin_at_measure": 913.0}"#,
    );
    // the PER-RUN success file (exists ONLY on a landed apply) carries applied_latency_ms 976.
    let landed = write(d.path(), "av-sync-last-run.json", OUTDIR_SUCCESS_FILE);
    let dest = d
        .path()
        .join("history.jsonl")
        .to_string_lossy()
        .into_owned();
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_append_history 'run-9' '-24.54' '' '0.4' '-61.35' \"$HIST\" '{rlast}' '{landed}' '{dest}'\n\
             echo \"n=$(wc -l < '{dest}')\"\n\
             echo \"pin=$(python3 -c \"import json;print(json.loads(open('{dest}').read().strip()).get('applied_pin'))\")\"\necho END"
        ),
    );
    assert!(ok, "append_history must not abort under set -e: {out}");
    assert!(out.contains("n=1"), "exactly one history line: {out}");
    assert!(
        out.contains("pin=976"),
        "a landed apply records applied_pin from the per-run success file: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn append_history_no_landed_file_omits_applied_pin() {
    let d = tempfile::tempdir().unwrap();
    let rlast = write(
        d.path(),
        "residual-last.json",
        r#"{"run_id": "run-9", "ts": 1.0, "residual_median_ms": -61.35, "pin_at_measure": 913.0}"#,
    );
    let dest = d
        .path()
        .join("history.jsonl")
        .to_string_lossy()
        .into_owned();
    // last_applied points at a MISSING per-run file (a failed/pending apply) -> applied_pin omitted
    // (honest), never last run's stale pin (#1265 review 🟡).
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_append_history 'run-9' '-24.54' '' '0.4' '-61.35' \"$HIST\" '{rlast}' '/nope/landed.json' '{dest}'\n\
             echo \"has_pin=$(python3 -c \"import json;print('applied_pin' in json.loads(open('{dest}').read().strip()))\")\"\necho END"
        ),
    );
    assert!(ok, "append_history must not abort under set -e: {out}");
    assert!(
        out.contains("has_pin=False"),
        "a proceed with no landed per-run file must NOT record applied_pin: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn append_history_runid_mismatch_is_a_noop_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let rlast = write(
        d.path(),
        "residual-last.json",
        r#"{"run_id": "OLD", "ts": 1.0, "residual_median_ms": -1.0, "pin_at_measure": 900.0}"#,
    );
    let dest = d
        .path()
        .join("history.jsonl")
        .to_string_lossy()
        .into_owned();
    let (out, ok) = run_under_set_e(
        d.path(),
        &format!(
            "av_sync_append_history 'run-NEW' '' '' '' '' \"$HIST\" '{rlast}' '/nope/x.json' '{dest}'\n\
             echo \"wrote=$([ -f '{dest}' ] && echo yes || echo no)\"\necho END"
        ),
    );
    assert!(ok, "a run_id mismatch must not abort under set -e: {out}");
    assert!(
        out.contains("wrote=no"),
        "a run with no measurement (residual-last run_id mismatch) writes no history line: {out}"
    );
    assert!(out.contains("END"), "{out}");
}

#[test]
fn append_history_missing_helper_never_aborts() {
    let d = tempfile::tempdir().unwrap();
    let (out, ok) = run_under_set_e(
        d.path(),
        "av_sync_append_history 'r' '' '' '' '' /nope/history.py\necho END",
    );
    assert!(
        ok,
        "a missing history helper must not abort under set -e: {out}"
    );
    assert!(out.contains("END"), "{out}");
}
