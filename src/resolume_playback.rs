//! Frame-loss-free playback verdict for the resolume-snv (CG box) NDI input,
//! measured off a strih/stream OBS `genlock-fifo audit` window (#811).
//!
//! resolume-snv is being brought under the fleet maintenance umbrella
//! (dantesync clock discipline + fleet sync). #800 found its NDI source
//! `RESOLUME-SNV (cg-obs)` drifting ~+65 ms/h against the rig all day because
//! the box carried no dantesync. This module is the DETECTION half of the
//! ticket's acceptance criterion #4 ("24h skew-latency flat ±20 ms, no
//! drops/duplicates on the 'cg' / 'NDI obs hudba' inputs"): given ONE resolume
//! input's per-source [`jitter_audit::AuditSummary`] window (produced by the
//! existing `genlock-jitter-report` pipeline), it returns a PASS/FAIL verdict
//! against the acceptance bounds.
//!
//! DELIBERATELY CADENCE-AGNOSTIC. resolume is a NON-60 CG source (#787
//! "resolume-rate exemption" in `rig-health-audit.py`; a variable ~43 fps CG
//! feed), so this NEVER checks a target frame rate. It reads only the
//! genlock-FIFO health counters, which are cadence-independent:
//!   * `max_abs_head_skew_ms` — worst per-tick presentation skew across the
//!     window. The ticket's ±20 ms flatness bound applies here.
//!   * The PATHOLOGY delta set (all must be 0 over a healthy window):
//!     `delta_dropped_due` (genuine drops), `delta_underruns`,
//!     `delta_relocks` (clock-discipline instability), `delta_late_holds`,
//!     and `delta_backward_regime_ticks` (#1009 — the hold was BYPASSED, i.e.
//!     a frame jump/duplicate, the true "duplikát" signal).
//!
//! Raw `delta_holds`/`delta_overruns` are NOT gated: on a non-60 source those
//! reflect the FIFO's normal cadence adaptation, not loss — gating on them
//! would false-fail a perfectly healthy CG feed. `delta_backward_regime_ticks`
//! is the frame-jump signal that matters, and it IS gated.
//!
//! Pure `std` and SELF-CONTAINED (no `use crate::…`) so it is Tier-0 testable
//! with the standalone-rustc recipe (#1026,
//! `.claude/rules/jitter-audit-parser.md`):
//!   `rustc --test --edition 2021 src/resolume_playback.rs -o /tmp/t && /tmp/t`
//! The crate-facing caller (`src/bin/genlock-jitter-report.rs`'s
//! `--verdict-source` mode) maps each `AuditSummary` into a [`PlaybackWindow`]
//! and calls [`evaluate`].

/// Acceptance bounds for a resolume playback window. `Default` = the ticket's
/// stated criteria (±20 ms skew, zero pathology deltas, at least a real delta
/// window). The 24h acceptance run raises `min_samples` to demand a long
/// window; a shorter spot-check keeps the default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackBounds {
    /// Worst-case `|ts_head_skew_ms|` tolerated across the window (ms).
    pub skew_bound_ms: i64,
    /// Minimum number of ~5s audit samples required to trust a "flat" verdict.
    /// A single tick cannot demonstrate flatness, so the floor is 2 (a genuine
    /// first→last delta window).
    pub min_samples: usize,
}

impl Default for PlaybackBounds {
    fn default() -> Self {
        PlaybackBounds {
            skew_bound_ms: 20,
            min_samples: 2,
        }
    }
}

/// One resolume input's summarized genlock-FIFO audit window — the subset of
/// `jitter_audit::AuditSummary` fields the verdict reads. Field names mirror
/// `AuditSummary` exactly so the bin-side mapping is a trivial field copy.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackWindow {
    pub source: String,
    pub samples: usize,
    pub latency_ms: u32,
    pub max_abs_head_skew_ms: i64,
    pub delta_dropped_due: u64,
    pub delta_underruns: u64,
    pub delta_relocks: u64,
    pub delta_late_holds: u64,
    pub delta_backward_regime_ticks: u64,
}

/// The verdict for one resolume input. `pass` is true iff `reasons` is empty;
/// each reason is a human-readable, evidence-carrying failure line.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackVerdict {
    pub source: String,
    pub pass: bool,
    pub reasons: Vec<String>,
}

/// Evaluate one resolume playback window against `bounds`. Returns a
/// [`PlaybackVerdict`]; `pass` is true only when EVERY check holds. All checks
/// are independent — every failing check contributes its own reason, so the
/// operator sees the full picture in one pass, not just the first fault.
pub fn evaluate(w: &PlaybackWindow, bounds: &PlaybackBounds) -> PlaybackVerdict {
    let mut reasons = Vec::new();

    if w.samples < bounds.min_samples {
        reasons.push(format!(
            "too few audit samples ({} < {}) — window too short to confirm flat skew",
            w.samples, bounds.min_samples
        ));
    }
    if w.max_abs_head_skew_ms > bounds.skew_bound_ms {
        reasons.push(format!(
            "skew excursion {} ms > bound {} ms — presentation not flat",
            w.max_abs_head_skew_ms, bounds.skew_bound_ms
        ));
    }
    if w.delta_dropped_due > 0 {
        reasons.push(format!(
            "{} dropped frame(s) in window",
            w.delta_dropped_due
        ));
    }
    if w.delta_underruns > 0 {
        reasons.push(format!("{} FIFO underrun(s) in window", w.delta_underruns));
    }
    if w.delta_relocks > 0 {
        reasons.push(format!(
            "{} FIFO relock(s) — clock discipline unstable",
            w.delta_relocks
        ));
    }
    if w.delta_late_holds > 0 {
        reasons.push(format!("{} late hold(s) in window", w.delta_late_holds));
    }
    if w.delta_backward_regime_ticks > 0 {
        reasons.push(format!(
            "{} backward-regime tick(s) — hold bypassed / frame jump (duplicate)",
            w.delta_backward_regime_ticks
        ));
    }

    PlaybackVerdict {
        source: w.source.clone(),
        pass: reasons.is_empty(),
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> PlaybackWindow {
        PlaybackWindow {
            source: "cg".to_string(),
            samples: 30,
            latency_ms: 3,
            max_abs_head_skew_ms: 8,
            delta_dropped_due: 0,
            delta_underruns: 0,
            delta_relocks: 0,
            delta_late_holds: 0,
            delta_backward_regime_ticks: 0,
        }
    }

    #[test]
    fn clean_window_passes() {
        let v = evaluate(&clean(), &PlaybackBounds::default());
        assert!(v.pass, "clean window must pass, got {:?}", v.reasons);
        assert!(v.reasons.is_empty());
        assert_eq!(v.source, "cg");
    }

    #[test]
    fn skew_excursion_fails() {
        let mut w = clean();
        w.max_abs_head_skew_ms = 25; // > 20 ms bound
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        assert!(
            v.reasons.iter().any(|r| r.contains("skew")),
            "expected a skew reason, got {:?}",
            v.reasons
        );
    }

    #[test]
    fn skew_at_bound_passes() {
        let mut w = clean();
        w.max_abs_head_skew_ms = 20; // exactly the bound is OK
        assert!(evaluate(&w, &PlaybackBounds::default()).pass);
    }

    #[test]
    fn dropped_frames_fail() {
        let mut w = clean();
        w.delta_dropped_due = 3;
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        assert!(v.reasons.iter().any(|r| r.contains("drop")));
    }

    #[test]
    fn underruns_fail() {
        let mut w = clean();
        w.delta_underruns = 2;
        assert!(!evaluate(&w, &PlaybackBounds::default()).pass);
    }

    #[test]
    fn relocks_fail() {
        let mut w = clean();
        w.delta_relocks = 1;
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        assert!(v.reasons.iter().any(|r| r.contains("relock")));
    }

    #[test]
    fn late_holds_fail() {
        let mut w = clean();
        w.delta_late_holds = 1;
        assert!(!evaluate(&w, &PlaybackBounds::default()).pass);
    }

    #[test]
    fn backward_regime_fail() {
        let mut w = clean();
        w.delta_backward_regime_ticks = 4;
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        assert!(
            v.reasons
                .iter()
                .any(|r| r.contains("jump") || r.contains("backward")),
            "expected a frame-jump reason, got {:?}",
            v.reasons
        );
    }

    #[test]
    fn too_few_samples_fail() {
        let mut w = clean();
        w.samples = 1; // below the min_samples floor
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        assert!(v.reasons.iter().any(|r| r.contains("sample")));
    }

    #[test]
    fn custom_skew_bound_tolerates_higher() {
        let mut w = clean();
        w.max_abs_head_skew_ms = 30;
        let bounds = PlaybackBounds {
            skew_bound_ms: 40,
            ..PlaybackBounds::default()
        };
        assert!(evaluate(&w, &bounds).pass);
    }

    #[test]
    fn multiple_faults_all_reported() {
        let mut w = clean();
        w.max_abs_head_skew_ms = 50;
        w.delta_dropped_due = 2;
        w.delta_relocks = 1;
        let v = evaluate(&w, &PlaybackBounds::default());
        assert!(!v.pass);
        // skew + drop + relock => at least 3 distinct reasons.
        assert!(
            v.reasons.len() >= 3,
            "expected >=3 reasons, got {:?}",
            v.reasons
        );
    }
}
