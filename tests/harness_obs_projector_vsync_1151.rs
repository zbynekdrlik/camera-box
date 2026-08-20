//! #1151 — pure-function + wiring guard for `scripts/lib/obs-projector-vsync.sh`, the SHARED reader
//! of the issue-1146 `projector-vsync: present-vsync ARMED` OBS-log marker.
//!
//! Root cause (#1151, follow-up to #1146): #1146 added the one-shot libobs marker but NO consumer.
//! This lib is that consumer core, sourced by BOTH scripts/drift-guard.sh (--check-imag facet) and
//! scripts/recording-e2e.sh (the E2E [0/8] preflight) — the split-lib pattern imag-display-path.sh /
//! imag-cmdline-isolation.sh use, so the marker string lives in exactly ONE place.
//!
//! STEP-0 validation (live 10.77.9.182, read-only 2026-08-20): the marker is deployed and emits on
//! every OBS session — `15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1;
//! no-op on D3D11)` — and no reader existed in `scripts/` yet.
//!
//! REPORT-ONLY: the verdict is only ever OK / UNKNOWN, never DRIFT — a missing marker is a healthy
//! ordering-dependent state (projector not (re)opened since OBS start), and per issue 781 the marker
//! only proves the mechanism is ENGAGED, never that scanout tearing is gone.
//!
//! Same convention as `tests/harness_imag_display_path_780.rs`: source the REAL lib (source-only, no
//! side effects) and exercise the pure functions directly. RED before the lib exists (sourcing
//! fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/obs-projector-vsync.sh")
}

fn recording_e2e() -> PathBuf {
    manifest_dir().join("scripts/recording-e2e.sh")
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Source the REAL lib and run `body`. `set_e` selects `set -euo pipefail` (the caller's production
/// context, #1133) vs `set -uo pipefail` (the pure-value context). Returns (exit_code, stdout).
fn run_sourced_with(set_e: bool, body: &str) -> (i32, String) {
    let flags = if set_e {
        "set -euo pipefail"
    } else {
        "set -uo pipefail"
    };
    let harness = format!("{flags}\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

fn run_sourced(body: &str) -> (i32, String) {
    run_sourced_with(false, body)
}

/// `projector_vsync_armed_from_log "<text>"` -> stdout ("1" or "").
fn armed_from_log(text: &str) -> String {
    let (rc, out) = run_sourced(&format!(
        "projector_vsync_armed_from_log {}",
        shell_quote(text)
    ));
    assert_eq!(rc, 0, "armed_from_log must exit 0 (pure); out={out:?}");
    out.trim().to_string()
}

/// `projector_vsync_verdict "<text>"` -> the single `projector_vsync|STATUS|detail` line.
fn verdict(text: &str) -> String {
    let (rc, out) = run_sourced(&format!("projector_vsync_verdict {}", shell_quote(text)));
    assert_eq!(rc, 0, "verdict must exit 0 (pure); out={out:?}");
    out.trim().to_string()
}

const ARMED_LINE: &str =
    "15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)";
const CLEARED_LINE: &str =
    "12:00:00.000: projector-vsync: present-vsync cleared (GL/EGL swap interval 0; no-op on D3D11)";

#[test]
fn armed_from_log_detects_the_armed_marker_only() {
    assert_eq!(armed_from_log(ARMED_LINE), "1", "ARMED marker -> 1");
    assert_eq!(
        armed_from_log(&format!("some noise\n{ARMED_LINE}\nmore noise")),
        "1",
        "ARMED marker anywhere in the log -> 1"
    );
    assert_eq!(armed_from_log(""), "", "empty log -> absent");
    assert_eq!(
        armed_from_log("multiview-audit: fps=60\nsome other OBS line"),
        "",
        "no marker -> absent"
    );
    assert_eq!(
        armed_from_log(CLEARED_LINE),
        "",
        "the `cleared` variant must NOT count as armed"
    );
}

#[test]
fn verdict_is_report_only_ok_or_unknown_never_drift() {
    // ARMED present -> OK, naming the mechanism.
    let ok = verdict(&format!("genlock: latency = 3 ms\n{ARMED_LINE}"));
    assert!(
        ok.starts_with("projector_vsync|OK|") && ok.contains("present-vsync armed"),
        "marker present -> OK naming the mechanism: {ok:?}"
    );
    // Empty log -> UNKNOWN (not read, #833), never OK, never DRIFT.
    let empty = verdict("");
    assert!(
        empty.starts_with("projector_vsync|UNKNOWN|") && empty.to_lowercase().contains("not read"),
        "empty log -> UNKNOWN (not read, fail-closed): {empty:?}"
    );
    // Non-empty, no marker -> UNKNOWN (projector not reopened / build predates the marker).
    let no_marker = verdict("genlock: latency = 3 ms\nStartup complete");
    assert!(
        no_marker.starts_with("projector_vsync|UNKNOWN|"),
        "read log with no marker -> UNKNOWN: {no_marker:?}"
    );
    for v in [&ok, &empty, &no_marker] {
        assert!(
            !v.contains("DRIFT"),
            "verdict must NEVER emit DRIFT (report-only): {v:?}"
        );
    }
}

#[test]
fn report_line_formats_status_and_detail() {
    let (rc, out) = run_sourced(&format!(
        "projector_vsync_report_line {}",
        shell_quote(&format!("x\n{ARMED_LINE}"))
    ));
    assert_eq!(rc, 0);
    assert!(
        out.trim_start().starts_with("OK  (") && out.contains("present-vsync armed"),
        "report_line OK form: {out:?}"
    );
    let (_rc, out2) = run_sourced("projector_vsync_report_line ''");
    assert!(
        out2.trim_start().starts_with("UNKNOWN  ("),
        "report_line UNKNOWN form on empty log: {out2:?}"
    );
}

#[test]
fn gather_remote_snippet_globs_txt_not_log() {
    // OBS names its logs `.txt`; the snippet must glob *.txt (like every other imag OBS-log reader),
    // most-recent-first, never *.log (which matches nothing on imag).
    let (rc, out) = run_sourced("projector_vsync_gather_remote_snippet");
    assert_eq!(rc, 0);
    assert!(
        out.contains("obs-studio/logs/") && out.contains("*.txt"),
        "snippet must read the OBS logs dir with a *.txt glob: {out:?}"
    );
    assert!(
        !out.contains("*.log"),
        "snippet must NOT glob *.log (#1151): {out:?}"
    );
    assert!(
        out.contains("ls -t") && out.contains("head -1"),
        "snippet must pick the most-recent log: {out:?}"
    );
}

#[test]
fn functions_never_abort_under_set_e_on_empty_or_no_match_1133() {
    // #1133: a report-only probe called under the caller's `set -euo pipefail` must NEVER abort the
    // run on a grep no-match / empty read. A `set -uo`-only test is structurally blind to this, so
    // this test sources the lib under the caller's EXACT `set -euo pipefail`.
    let body = r#"
        for L in "" "no marker here" "junk"; do
            projector_vsync_armed_from_log "$L" >/dev/null
            projector_vsync_verdict "$L" >/dev/null
            projector_vsync_report_line "$L" >/dev/null
        done
        # the EXACT recording-e2e.sh embed shape, empty log:
        _log=""
        echo "    [imag projector-vsync] $(projector_vsync_report_line "$_log")"
        echo "SURVIVED"
    "#;
    let (rc, out) = run_sourced_with(true, body);
    assert_eq!(
        rc, 0,
        "must survive set -e on empty/no-match input (#1133): out={out:?}"
    );
    assert!(
        out.contains("SURVIVED"),
        "must reach the end under set -e: {out:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// recording-e2e.sh [0/8] wiring — report-only, AFTER the projector-open/studio-mode steps.
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_shared_lib_and_uses_it_1151() {
    let s = read(recording_e2e());
    assert!(
        s.contains(r#". "$HERE/lib/obs-projector-vsync.sh""#),
        "recording-e2e.sh must source the shared obs-projector-vsync lib (#1151)"
    );
    assert!(
        s.contains("projector_vsync_gather_remote_snippet"),
        "the [0/8] preflight must gather the log via the shared snippet ($(fn) embed, #675)"
    );
    assert!(
        s.contains("projector_vsync_report_line"),
        "the [0/8] preflight must format via the shared report_line (single judgment site)"
    );
}

#[test]
fn recording_e2e_vsync_check_runs_after_studio_mode_1151() {
    // The marker is emitted at Program-projector OPEN (one-shot-on-change), so the check MUST run
    // AFTER the open-projectors + studio-mode steps, else it reads nothing after an OBS restart.
    let s = read(recording_e2e());
    let studio = s
        .find("imag Studio Mode must be ON")
        .expect("the #767 studio-mode-on preflight must still exist");
    let check = s
        .find("[0/8] imag Program present-vsync marker check")
        .expect("the #1151 present-vsync marker check must exist in the [0/8] preflight");
    assert!(
        studio < check,
        "the present-vsync marker check must run AFTER the projector-open/studio-mode steps \
         (studio={studio} check={check})"
    );
}

#[test]
fn recording_e2e_vsync_check_is_report_only_no_exit_1151() {
    // The block must NEVER fail the run — report-only. Slice from the check echo to the end of the
    // imag OBS-prep skip-guard and assert no `exit 1` was introduced in it.
    let s = read(recording_e2e());
    let start = s
        .find("[0/8] imag Program present-vsync marker check")
        .expect("the #1151 check must exist");
    let end_rel = s[start..]
        .find("end of the IMAG_OFFLINE_ACKED skip-guard")
        .expect("the imag OBS-prep skip-guard closer must follow the check");
    let block = &s[start..(start + end_rel)];
    assert!(
        !block.contains("exit 1") && !block.contains("exit 30"),
        "the present-vsync marker check is REPORT-ONLY — it must never exit/abort the run: {block:?}"
    );
}
