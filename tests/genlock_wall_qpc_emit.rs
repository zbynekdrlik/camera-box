//! #800 — vendored-source guard for the wall-vs-QPC clock-drift audit term.
//!
//! The 2026-07-18 all-day audit (#800) proved the genlock FIFO held the configured latency
//! EXACTLY all day, so the ~550 ms A/V shift lived OUTSIDE the instrumented video chain. The
//! leading remaining candidate: the video release deadline is WALL-slaved
//! (`genlock_wall_now_ns()` = `GetSystemTimePreciseAsFileTime` on Windows), while the render tick
//! and audio capture ride the MONOTONIC clock (`os_gettime_ns()` = QPC). If those two clock
//! domains drift apart over a long event, wall-slaved video and QPC-slaved audio diverge — but the
//! audit line carried NO field measuring that drift, so the hypothesis stayed "Inconclusive".
//! #800 adds a `wall_qpc_drift_ms=%lld` term to the ~5 s `genlock-fifo audit` line (a `static`
//! helper `genlock_wall_qpc_drift_ms()` anchored at the first tick), so one grep of a captured
//! day answers the drift question offline. Parsed by the input-side `AuditSample` family in
//! `src/jitter_audit.rs`.
//!
//! This guard is STD-ONLY (no `use camera_box`, not probe-gated) so it gives an observable local
//! RED→GREEN via the standalone-rustc recipe (`.claude/rules/vendored-libobs-change-safety.md`,
//! #1026), even though the vendored C only compiles on CI:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/genlock_wall_qpc_emit.rs -o /tmp/t && /tmp/t
//! ```

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";

#[test]
fn audit_line_emits_the_wall_qpc_drift_term() {
    let src = read(OBS_SOURCE);
    // The exact format token the parser (`src/jitter_audit.rs` AuditSample.wall_qpc_drift_ms)
    // reads. Signed (`%lld` / long long), same shape as ts_head_skew_ms.
    assert!(
        src.contains("wall_qpc_drift_ms=%lld"),
        "{OBS_SOURCE}: #800 wall_qpc_drift_ms audit token gone — the wall-vs-QPC clock-domain \
         drift is no longer visible in the OBS log; the #800 A/V-shift diagnosis loses its one grep."
    );
}

#[test]
fn wall_qpc_drift_helper_present_and_wired_into_the_audit() {
    let src = read(OBS_SOURCE);
    // The measuring helper: reads both clocks back-to-back, anchored once at the first tick.
    assert!(
        src.contains("static long long genlock_wall_qpc_drift_ms(void)"),
        "{OBS_SOURCE}: #800 genlock_wall_qpc_drift_ms() helper gone — nothing computes the \
         wall(RTC)-vs-monotonic(QPC) drift delta-of-deltas."
    );
    // The helper differences the WALL clock against the MONOTONIC clock — both reads must be in
    // its body or it is measuring the wrong thing.
    assert!(
        src.contains("genlock_wall_now_ns()") && src.contains("os_gettime_ns()"),
        "{OBS_SOURCE}: #800 the drift helper must read genlock_wall_now_ns() (RTC) AND \
         os_gettime_ns() (QPC) — the whole point is the divergence of the two clock domains."
    );
    // And it must actually be emitted on the audit line (the last blog() argument).
    assert!(
        src.contains("genlock_wall_qpc_drift_ms());"),
        "{OBS_SOURCE}: #800 genlock_wall_qpc_drift_ms() is not passed to the genlock-fifo audit \
         blog() — the term would print a stale/garbage value or fail to compile."
    );
}
