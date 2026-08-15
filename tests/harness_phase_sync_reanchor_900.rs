//! #900 — phase-sync RE-ANCHOR establisher: wiring + ordering guards on scripts/recording-e2e.sh.
//!
//! The pure decision layer (load / restrict-with-coverage-fail / no-op-vs-apply) is unit-tested in
//! `tests/python/test_phase_sync_reanchor.py`. These guards lock the HARNESS wiring — that the
//! re-anchor actually runs, runs BEFORE the [4h/8] floor gate it establishes, is ON by default, and
//! FAILS LOUD — mirroring `harness_phase_sync_active_floor_gate_893.rs`'s own wiring guard for the
//! gate it feeds. Substring (`.find`) checks only, so an unrelated edit to the surrounding block
//! cannot false-fail them.

use std::fs;
use std::path::Path;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn reanchor_script_exists() {
    let path = format!(
        "{}/scripts/phase_sync_reanchor.py",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "scripts/phase_sync_reanchor.py not found (#900) — the re-anchor establisher."
    );
}

#[test]
fn recording_e2e_sh_wires_the_reanchor() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("phase_sync_reanchor.py"),
        "scripts/recording-e2e.sh must invoke the #900 phase-sync re-anchor so the [4h/8] \
         active-floor gate always has an automatic establisher."
    );
}

#[test]
fn reanchor_runs_before_the_active_floor_gate() {
    // The establisher MUST run before the gate it establishes, else the gate reads pins nobody set.
    let s = read("scripts/recording-e2e.sh");
    let reanchor = s
        .find("phase_sync_reanchor.py")
        .expect("re-anchor must be wired (#900)");
    let gate = s
        .find("phase_sync_active_floor_check.py")
        .expect("active-floor gate must be wired (#893)");
    assert!(
        reanchor < gate,
        "#900 re-anchor (offset {reanchor}) must run BEFORE the [4h/8] active-floor gate \
         (offset {gate}) — the establisher runs first, then the gate checks its result."
    );
}

#[test]
fn reanchor_is_on_by_default() {
    // ON by default (opt-out PHASE_REANCHOR=0) — unlike the #757 [4g/8] auto-pin which is off.
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("PHASE_REANCHOR=\"${PHASE_REANCHOR:-1}\""),
        "#900 re-anchor must default ON (PHASE_REANCHOR:-1) — it is the establisher the gate needs \
         on every run, not an opt-in like the demoted #757 [4g/8] auto-pin."
    );
}

#[test]
fn reanchor_fails_loud() {
    // FAIL-LOUD (never best-effort like [4g/8]): a missing/uncovered calibration basis must exit
    // the run before StartRecord, not be silently skipped into the gate.
    let s = read("scripts/recording-e2e.sh");
    let reanchor = s
        .find("phase_sync_reanchor.py")
        .expect("re-anchor must be wired (#900)");
    let gate = s
        .find("phase_sync_active_floor_check.py")
        .expect("active-floor gate must be wired (#893)");
    // the re-anchor's own failure handler (before the next step) must exit the run.
    let block = &s[reanchor..gate];
    assert!(
        block.contains("exit 1"),
        "#900 re-anchor must FAIL LOUD (exit 1 on a missing/uncovered basis or apply failure), \
         never best-effort like the [4g/8] auto-pin."
    );
}
