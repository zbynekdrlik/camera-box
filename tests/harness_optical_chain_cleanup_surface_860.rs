//! #860 — the #712 cleanup restore-failure must not stay a BURIED stderr WARNING.
//!
//! Live incident 2026-08-14: a chain of FAILED E2E runs whose cleanups each logged
//! `WARNING #712: cam2/painter restore failed/timed out` left the painter DEAD with NO
//! CI-visible surface — the failures were only in stderr, invisible in the run summary, so
//! consecutive gate runs were silently poisoned by a dark cam2 monitor.
//!
//! This pins the additive surface in `scripts/lib/cambox-parallel-restore.sh`:
//!   (1) `cambox_parallel_wait_and_report` records failed labels into
//!       `CAMBOX_PARALLEL_FAILED_LABELS` (in ADDITION to the existing per-box WARNING #712 +
//!       non-zero return — never a signature change),
//!   (2) `cambox_parallel_surface_painter_failure` turns a cam2/painter failure into a LOUD
//!       GitHub Actions `::error::` annotation (visible in the run summary), other boxes into a
//!       `::warning::` — WITHOUT ever `exit`ing (cleanup's always-runs semantics are preserved).
//!
//! RED before the two additions exist; GREEN after. No rig, no ssh — the helper is driven with
//! real backgrounded jobs exactly like harness_cambox_parallel_restore_712.rs.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/cambox-parallel-restore.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn run(script: &str) -> (String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run driver");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A run where cam2/painter's restore FAILS records it into CAMBOX_PARALLEL_FAILED_LABELS and the
/// surface helper emits a LOUD ::error:: annotation naming the painter.
#[test]
fn painter_restore_failure_is_surfaced_as_a_github_error_annotation() {
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 0) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam1 (source, 10.77.9.61)\")\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, 10.77.9.62\")\n\
         cambox_parallel_wait_and_report || true\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n\
         cambox_parallel_surface_painter_failure\n"
    );
    let (stdout, stderr) = run(&script);
    assert!(
        stdout.contains("FAILED_COUNT=1"),
        "the wait helper must record exactly the one failed label. stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("::error"),
        "a cam2/painter restore failure must surface as a GitHub ::error:: annotation. stderr:\n{stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("painter"),
        "the ::error:: annotation must name the painter. stderr:\n{stderr}"
    );
}

/// A run where every box's restore SUCCEEDS records NO failed labels and the surface helper emits
/// NO ::error:: annotation.
#[test]
fn all_success_surfaces_no_error() {
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 0) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam1 (source, 10.77.9.61)\")\n\
         (exit 0) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam2/painter, 10.77.9.62\")\n\
         cambox_parallel_wait_and_report || true\n\
         echo \"FAILED_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}\"\n\
         cambox_parallel_surface_painter_failure\n"
    );
    let (stdout, stderr) = run(&script);
    assert!(
        stdout.contains("FAILED_COUNT=0"),
        "no failures must record no labels. stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("::error"),
        "a clean restore must not surface any ::error:: annotation. stderr:\n{stderr}"
    );
}

/// A NON-painter box failure surfaces as a ::warning:: (not ::error::) — the painter is the
/// dead-monitor incident this ticket targets; other boxes stay a milder surface.
#[test]
fn non_painter_failure_is_a_warning_not_an_error() {
    let path = lib_path();
    let script = format!(
        "source '{path}'\n\
         CAMBOX_PARALLEL_PIDS=()\n\
         CAMBOX_PARALLEL_LABELS=()\n\
         (exit 1) & CAMBOX_PARALLEL_PIDS+=(\"$!\"); CAMBOX_PARALLEL_LABELS+=(\"cam3 (10.77.9.63)\")\n\
         cambox_parallel_wait_and_report || true\n\
         cambox_parallel_surface_painter_failure\n"
    );
    let (_stdout, stderr) = run(&script);
    assert!(
        stderr.contains("::warning"),
        "a non-painter box failure should surface as a ::warning::. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("::error"),
        "a non-painter box failure must NOT be escalated to ::error::. stderr:\n{stderr}"
    );
}

/// The lib must never `exit` — cleanup()'s trap must always run to completion (the #649/#675/#712
/// warn-only discipline), including the new surface helper.
#[test]
fn lib_never_exits() {
    let s = std::fs::read_to_string(lib_path()).expect("read lib");
    assert!(
        !s.contains("\nexit ")
            && !s.contains(" exit ")
            && !s.contains(";exit ")
            && !s.contains("; exit "),
        "cambox-parallel-restore.sh must never `exit` (cleanup trap must always complete)"
    );
}
