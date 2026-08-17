//! #811 — end-to-end proof of the resolume playback verdict over the REAL
//! public pipeline: `genlock-fifo audit '<src>':` log text
//! → `jitter_audit::parse_audit_lines` → `summarize_all` → one `AuditSummary`
//! → mapped to a `PlaybackWindow` → `resolume_playback::evaluate`.
//!
//! This is the integration counterpart to the pure per-field unit tests in
//! `src/resolume_playback.rs` (they pin the verdict logic) and the bin-glue
//! tests in `src/bin/genlock-jitter-report.rs` (they pin arg-parsing + exit
//! codes). Here we prove the WHOLE chain on real audit lines, using only the
//! crate's public API — the same fields the `--verdict-source` mode maps.
//!
//! Tier-0: `cargo test --no-run --test resolume_verdict_pipeline` then run the
//! compiled binary directly (the #477 pattern — `# airuleset:build-ok` is a
//! no-op here).

use camera_box::jitter_audit::{parse_audit_lines, summarize_all, AuditSummary};
use camera_box::resolume_playback::{evaluate, PlaybackBounds, PlaybackWindow};

/// The SAME mapping the `genlock-jitter-report --verdict-source` mode uses.
fn window_from_summary(s: &AuditSummary) -> PlaybackWindow {
    PlaybackWindow {
        source: s.source.clone(),
        samples: s.samples,
        latency_ms: s.latency_ms,
        max_abs_head_skew_ms: s.max_abs_head_skew_ms,
        delta_dropped_due: s.delta_dropped_due,
        delta_underruns: s.delta_underruns,
        delta_relocks: s.delta_relocks,
        delta_late_holds: s.delta_late_holds,
        delta_backward_regime_ticks: s.delta_backward_regime_ticks,
    }
}

fn verdict_for(log: &str, source: &str) -> Option<(bool, Vec<String>)> {
    let summaries = summarize_all(&parse_audit_lines(log));
    let s = summaries.iter().find(|s| s.source == source)?;
    let v = evaluate(&window_from_summary(s), &PlaybackBounds::default());
    Some((v.pass, v.reasons))
}

// A healthy resolume window: two audit ticks, small skew both directions, every
// pathology counter identical across the window (delta 0). Note holds/overruns
// MOVE (2->5) — those are NOT gated (non-60 cadence adaptation), so this still
// PASSes.
const CLEAN_LOG: &str = "\
genlock-fifo audit 'cg': received=100 consumed=100 underruns=0 holds=2 overruns=1 dropped_due=0 relocks=0 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=6 backward_regime_ticks=10
genlock-fifo audit 'cg': received=200 consumed=200 underruns=0 holds=5 overruns=3 dropped_due=0 relocks=0 late_holds=0 locked=1 depth=3 peak=5 latency_ms=3 ts_head_skew_ms=-8 backward_regime_ticks=10
";

// A faulty window: real drops accrue, the FIFO relocks, and a skew excursion
// blows past the 20 ms bound.
const FAULTY_LOG: &str = "\
genlock-fifo audit 'cg': received=100 consumed=100 dropped_due=0 relocks=0 underruns=0 late_holds=0 latency_ms=3 ts_head_skew_ms=6 backward_regime_ticks=10
genlock-fifo audit 'cg': received=200 consumed=197 dropped_due=3 relocks=1 underruns=0 late_holds=0 latency_ms=3 ts_head_skew_ms=45 backward_regime_ticks=12
";

#[test]
fn clean_resolume_window_passes_end_to_end() {
    let (pass, reasons) = verdict_for(CLEAN_LOG, "cg").expect("cg summary present");
    assert!(pass, "clean window must pass, got reasons {reasons:?}");
    assert!(reasons.is_empty());
}

#[test]
fn faulty_resolume_window_fails_with_drop_relock_skew_and_jump() {
    let (pass, reasons) = verdict_for(FAULTY_LOG, "cg").expect("cg summary present");
    assert!(!pass);
    let joined = reasons.join(" | ");
    assert!(joined.contains("drop"), "expected drop reason in {joined}");
    assert!(
        joined.contains("relock"),
        "expected relock reason in {joined}"
    );
    assert!(joined.contains("skew"), "expected skew reason in {joined}");
    // backward_regime_ticks moved 10->12 => a frame-jump reason too.
    assert!(
        joined.contains("jump") || joined.contains("backward"),
        "expected a frame-jump reason in {joined}"
    );
}

#[test]
fn absent_source_has_no_summary() {
    // A source name that never appears in the log yields no summary at all —
    // the bin's `--verdict-source` mode reports this as ABSENT.
    assert!(verdict_for(CLEAN_LOG, "NDI obs hudba").is_none());
}
