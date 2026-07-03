//! #109 — restart-survival ZERO-LOSS verdict (Tier-0, default features).
//!
//! Part of #105's Step 4: "the final [zero-loss] test must re-pass after OBS restart AND PC
//! restart." `recording-verdict --json` already computes the run's single trustworthy binary
//! delivery verdict (#186) — `overall_pass` at the top level and, underneath it,
//! `full_chain.zero_loss` (every node's burn-id sequence CONTIGUOUS, `NodeVerdict::is_zero`, AND
//! its analyzed optical span cleared the duration floor, `NodeVerdict::span_ok` — see
//! `src/bin/recording-verdict.rs`'s `build_and_print_verdict` / `node_verdict_json`). This module
//! is the strict PASS/FAIL/UNKNOWN kernel that takes a BEFORE-restart and an AFTER-restart
//! `recording-verdict --json` report and asserts BOTH were a genuine zero-loss PASS — #109's
//! restart-survival requirement applied to the #186 zero-loss signal, the exact sibling of
//! `av_restart_sync::classify` (#137's A/V-sync restart-survival gate).
//!
//! **Fail-closed on internally-inconsistent JSON**: a measurement claiming `overall_pass: true`
//! while `full_chain.zero_loss: false` is IMPOSSIBLE from a real `recording-verdict` run — see
//! `build_and_print_verdict`'s `all_pass &= nv.is_zero() && span_ok` accumulation, which can only
//! ever make `overall_pass` a STRICTER AND of `full_chain.zero_loss`, never a weaker one. Likewise
//! a measurement claiming `full_chain.zero_loss: true` while carrying non-zero
//! `real_drops`/`burn_unreadable` is impossible from `NodeVerdict::is_zero` (which requires the
//! burn-id sequence fully CONTIGUOUS — no missing id at all, see `src/probe/burn_contiguity.rs`).
//! Either combination proves the JSON is NOT a real `recording-verdict` report — corrupted,
//! hand-edited, or schema-mismatched. This gate never trusts it enough to manufacture a PASS *or*
//! a FAIL from it; it returns `Unknown` instead (same fail-closed shape as
//! `av_restart_sync::classify`'s measurement-quality handling).
//!
//! Mirrors `av_restart_sync.rs`'s shape (pure `classify`, Tier-0 unit-tested, single source of
//! truth for the decision) so the rig wiring (`scripts/recording-e2e.sh`'s optional
//! `ZERO_LOSS_RESTART_GATE` step + the thin `src/bin/zero-loss-restart-gate.rs` CLI) never
//! re-implements it. The live restart-survival rig proof (a real OBS restart AND a real PC
//! reboot of strih+stream, each bracketed by two `recording-verdict --json` measurements) is
//! supervisor-driven, not exercised here — this module is unit-tested Tier-0 with synthetic
//! measurements per `regression-test-first.md`.
//!
//! Pure so it unit-tests on default features (Tier-0) — no OBS, no ffmpeg, no MCP, no `probe`
//! feature.

/// One zero-loss delivery verdict, straight off the top-level fields of a
/// `recording-verdict --json` report (see `src/bin/recording-verdict.rs`):
/// `overall_pass`, `full_chain.zero_loss`, `full_chain.real_drops`, `full_chain.burn_unreadable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroLossMeasurement {
    /// `report["overall_pass"]` — the run's single trustworthy binary verdict (#186): every
    /// gated check (full-chain zero-loss, cam2→cam1 capture drops, switch-schedule continuity
    /// when gated) ANDed together. The strongest claim a report can make.
    pub overall_pass: bool,
    /// `report["full_chain"]["zero_loss"]` — the frame-DELIVERY gate #109 specifically cares
    /// about: every node's burn-id sequence CONTIGUOUS AND its analyzed optical span cleared
    /// the duration floor.
    pub full_chain_zero_loss: bool,
    /// `report["full_chain"]["real_drops"]` — summed REAL DROP count across all nodes. A
    /// genuine `full_chain_zero_loss: true` can only ever carry 0 here.
    pub real_drops: u64,
    /// `report["full_chain"]["burn_unreadable"]` — summed BURN-UNREADABLE count. Same
    /// zero-only-when-true constraint as `real_drops`.
    pub burn_unreadable: u64,
}

/// Strict restart-survival verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZeroLossRestartVerdict {
    /// Both the before AND the after measurement were a genuine, internally-consistent
    /// zero-loss PASS.
    Pass,
    /// Both measurements were trustworthy, but at least one was NOT zero-loss — the real #109
    /// failure mode (a restart that broke frame delivery, or a baseline that was never clean).
    Fail(Vec<String>),
    /// At least one measurement was internally inconsistent (a claim `recording-verdict` could
    /// never actually produce) — never reported as `Pass` or `Fail`, kept distinct from both
    /// (a corrupt/untrustworthy JSON is not proof the restart broke anything, nor proof it
    /// didn't).
    Unknown(Vec<String>),
}

impl ZeroLossRestartVerdict {
    /// True ONLY for `Pass` — both `Fail` and `Unknown` are "do not ship this restart".
    pub fn is_pass(&self) -> bool {
        matches!(self, ZeroLossRestartVerdict::Pass)
    }

    pub fn label(&self) -> &'static str {
        match self {
            ZeroLossRestartVerdict::Pass => "PASS",
            ZeroLossRestartVerdict::Fail(_) => "FAIL",
            ZeroLossRestartVerdict::Unknown(_) => "UNKNOWN",
        }
    }

    pub fn reasons(&self) -> Vec<String> {
        match self {
            ZeroLossRestartVerdict::Pass => Vec::new(),
            ZeroLossRestartVerdict::Fail(r) | ZeroLossRestartVerdict::Unknown(r) => r.clone(),
        }
    }
}

/// Trust-check one measurement for internal consistency; empty vec = trusted. A real
/// `recording-verdict --json` report can NEVER produce either contradiction checked here (see
/// the module doc) — finding one means the JSON is not a genuine report.
fn trust_reasons(label: &str, m: &ZeroLossMeasurement) -> Vec<String> {
    let mut reasons = Vec::new();
    if m.overall_pass && !m.full_chain_zero_loss {
        reasons.push(format!(
            "{label} measurement is internally inconsistent: overall_pass=true but \
             full_chain.zero_loss=false (overall_pass can never be true when the full-chain \
             delivery gate failed) — not a genuine recording-verdict report"
        ));
    }
    if m.full_chain_zero_loss && (m.real_drops > 0 || m.burn_unreadable > 0) {
        reasons.push(format!(
            "{label} measurement is internally inconsistent: full_chain.zero_loss=true but \
             real_drops={} burn_unreadable={} (a zero-loss verdict cannot carry any missing id) \
             — not a genuine recording-verdict report",
            m.real_drops, m.burn_unreadable
        ));
    }
    reasons
}

/// Classify a BEFORE/AFTER zero-loss measurement pair. STRICT:
///
/// 1. Either measurement failing its trust check (an internally-inconsistent combination a real
///    `recording-verdict` report could never produce) FAILS CLOSED to `Unknown` — never `Pass`,
///    never `Fail`, regardless of what the raw fields would otherwise say.
/// 2. Both trusted: `Pass` iff BOTH `overall_pass` AND `full_chain_zero_loss` are true on BOTH
///    measurements. Otherwise `Fail`, naming every measurement that was not zero-loss (a
///    baseline that was never clean fails the same way a restart-broken "after" does — restart
///    survival can only be claimed from a clean baseline).
pub fn classify(before: ZeroLossMeasurement, after: ZeroLossMeasurement) -> ZeroLossRestartVerdict {
    let mut untrusted = trust_reasons("before", &before);
    untrusted.extend(trust_reasons("after", &after));
    if !untrusted.is_empty() {
        return ZeroLossRestartVerdict::Unknown(untrusted);
    }

    let mut fail_reasons = Vec::new();
    if !(before.overall_pass && before.full_chain_zero_loss) {
        fail_reasons.push(format!(
            "before measurement was NOT zero-loss (overall_pass={}, full_chain.zero_loss={}, \
             real_drops={}, burn_unreadable={}) — the baseline itself must be clean before a \
             restart can be judged to survive it",
            before.overall_pass,
            before.full_chain_zero_loss,
            before.real_drops,
            before.burn_unreadable
        ));
    }
    if !(after.overall_pass && after.full_chain_zero_loss) {
        fail_reasons.push(format!(
            "after measurement was NOT zero-loss (overall_pass={}, full_chain.zero_loss={}, \
             real_drops={}, burn_unreadable={}) — the restart broke zero-loss delivery (#109)",
            after.overall_pass, after.full_chain_zero_loss, after.real_drops, after.burn_unreadable
        ));
    }
    if !fail_reasons.is_empty() {
        return ZeroLossRestartVerdict::Fail(fail_reasons);
    }

    ZeroLossRestartVerdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> ZeroLossMeasurement {
        ZeroLossMeasurement {
            overall_pass: true,
            full_chain_zero_loss: true,
            real_drops: 0,
            burn_unreadable: 0,
        }
    }

    fn broken(real_drops: u64, burn_unreadable: u64) -> ZeroLossMeasurement {
        ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: false,
            real_drops,
            burn_unreadable,
        }
    }

    #[test]
    fn both_healthy_zero_loss_passes() {
        let v = classify(healthy(), healthy());
        assert!(v.is_pass(), "both zero-loss should PASS, got {v:?}");
        assert_eq!(v.label(), "PASS");
        assert!(v.reasons().is_empty());
    }

    #[test]
    fn after_broke_zero_loss_fails() {
        // The exact #109 failure mode: a restart that broke frame delivery.
        let v = classify(healthy(), broken(3, 0));
        match &v {
            ZeroLossRestartVerdict::Fail(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("after")));
            }
            other => panic!("a broken 'after' MUST FAIL, got {other:?}"),
        }
        assert_eq!(v.label(), "FAIL");
    }

    #[test]
    fn before_was_never_zero_loss_fails() {
        // Restart survival can never be claimed from a baseline that was already broken.
        let v = classify(broken(0, 2), healthy());
        match &v {
            ZeroLossRestartVerdict::Fail(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("before")));
            }
            other => panic!("a broken 'before' baseline MUST FAIL, got {other:?}"),
        }
    }

    #[test]
    fn both_broken_fails_with_both_reasons() {
        let v = classify(broken(1, 0), broken(0, 1));
        match &v {
            ZeroLossRestartVerdict::Fail(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("before")));
                assert!(reasons.iter().any(|r| r.contains("after")));
                assert_eq!(reasons.len(), 2);
            }
            other => panic!("both broken MUST FAIL with 2 reasons, got {other:?}"),
        }
    }

    #[test]
    fn full_chain_false_with_zero_drops_is_a_valid_fail_not_unknown() {
        // #373: a COLLAPSED optical span (span_ok=false) can make full_chain.zero_loss=false
        // with 0 real drops and 0 burn-unreadable ids — a legitimate, non-contradictory report.
        // Must FAIL (not zero-loss), never UNKNOWN (this is not an inconsistency).
        let collapsed = ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: false,
            real_drops: 0,
            burn_unreadable: 0,
        };
        let v = classify(healthy(), collapsed);
        assert_eq!(
            v.label(),
            "FAIL",
            "a collapsed-span (false, 0, 0) 'after' must FAIL, not UNKNOWN, got {v:?}"
        );
    }

    #[test]
    fn inconsistent_overall_pass_true_zero_loss_false_is_unknown() {
        let inconsistent = ZeroLossMeasurement {
            overall_pass: true,
            full_chain_zero_loss: false,
            real_drops: 0,
            burn_unreadable: 0,
        };
        let v = classify(inconsistent, healthy());
        assert_eq!(v.label(), "UNKNOWN");
        assert!(v
            .reasons()
            .iter()
            .any(|r| r.contains("before") && r.contains("overall_pass")));
    }

    #[test]
    fn inconsistent_zero_loss_true_with_real_drops_is_unknown() {
        let inconsistent = ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: true,
            real_drops: 5,
            burn_unreadable: 0,
        };
        let v = classify(healthy(), inconsistent);
        assert_eq!(v.label(), "UNKNOWN");
        assert!(v
            .reasons()
            .iter()
            .any(|r| r.contains("after") && r.contains("real_drops")));
    }

    #[test]
    fn inconsistent_zero_loss_true_with_burn_unreadable_is_unknown() {
        let inconsistent = ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: true,
            real_drops: 0,
            burn_unreadable: 4,
        };
        let v = classify(inconsistent, healthy());
        assert_eq!(v.label(), "UNKNOWN");
        assert!(v
            .reasons()
            .iter()
            .any(|r| r.contains("before") && r.contains("burn_unreadable")));
    }

    #[test]
    fn both_inconsistent_reports_both_reasons() {
        let bad_before = ZeroLossMeasurement {
            overall_pass: true,
            full_chain_zero_loss: false,
            real_drops: 0,
            burn_unreadable: 0,
        };
        let bad_after = ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: true,
            real_drops: 1,
            burn_unreadable: 0,
        };
        let v = classify(bad_before, bad_after);
        assert_eq!(v.label(), "UNKNOWN");
        let reasons = v.reasons();
        assert!(reasons.iter().any(|r| r.contains("before")));
        assert!(reasons.iter().any(|r| r.contains("after")));
        assert_eq!(reasons.len(), 2);
    }

    #[test]
    fn inconsistency_takes_priority_over_a_would_be_fail() {
        // A measurement that is BOTH internally inconsistent AND would independently look like
        // a "fail" candidate must still surface as UNKNOWN, never FAIL — the trust check runs
        // first and is authoritative.
        let inconsistent_and_dropping = ZeroLossMeasurement {
            overall_pass: false,
            full_chain_zero_loss: true,
            real_drops: 9,
            burn_unreadable: 0,
        };
        let v = classify(healthy(), inconsistent_and_dropping);
        assert_eq!(
            v.label(),
            "UNKNOWN",
            "an inconsistent measurement must never resolve to FAIL, got {v:?}"
        );
    }

    #[test]
    fn is_pass_and_label_reflect_every_variant() {
        assert!(ZeroLossRestartVerdict::Pass.is_pass());
        assert_eq!(ZeroLossRestartVerdict::Pass.label(), "PASS");
        assert!(ZeroLossRestartVerdict::Pass.reasons().is_empty());

        let f = ZeroLossRestartVerdict::Fail(vec!["x".to_string()]);
        assert!(!f.is_pass());
        assert_eq!(f.label(), "FAIL");
        assert_eq!(f.reasons(), vec!["x".to_string()]);

        let u = ZeroLossRestartVerdict::Unknown(vec!["y".to_string()]);
        assert!(!u.is_pass());
        assert_eq!(u.label(), "UNKNOWN");
        assert_eq!(u.reasons(), vec!["y".to_string()]);
    }
}
