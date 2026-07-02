//! #137 — OBS-restart A/V-sync SURVIVAL verdict (Tier-0, default features).
//!
//! Reopened issue #137: an OBS stop→start SOMETIMES drifts the video↔audio offset by
//! ~200–300 ms and destroys lipsync ("niekedy sa nam rozsišiel o 200-300ms úplne
//! zlikvidovalo lipsync") — silently, with nothing automatic to catch it. The #188
//! machinery already MEASURES the video↔audio offset from a recording (cam2's QPSK
//! audio marker vs its dual-QR video tick, via `recording-verdict --av-sync`, which
//! reports `av_offset_ms` / `matched` / `mad_ms`). This module is the strict
//! PASS/FAIL/UNKNOWN kernel that compares a BEFORE-restart and an AFTER-restart
//! measurement and asserts the offset held within a tight tolerance — #109's
//! restart-survival requirement applied to #188's A/V-sync signal.
//!
//! **Fail-closed on measurement quality**: a measurement with too few clustered
//! markers or too scattered a cluster (high `mad_ms`) is UNTRUSTWORTHY — the offset it
//! reports could be noise, not truth. This gate NEVER lets an untrustworthy measurement
//! manufacture a false PASS; it returns `Unknown` instead (same fail-closed shape as
//! `render_budget::classify` / `obs_watchdog::classify`'s non-finite handling).
//!
//! Mirrors the `render_budget.rs` / `obs_watchdog.rs` shape (pure `classify`, Tier-0
//! unit-tested, single source of truth for every threshold) so the rig wiring
//! (`scripts/recording-e2e.sh`'s optional `AV_RESTART_GATE` step + the thin
//! `src/bin/av-restart-sync-gate.rs` CLI) never re-implements the decision. The live
//! two-recording rig proof (a real OBS stop→start on the stream box bracketed by two
//! `recording-verdict --av-sync` measurements) is supervisor-driven, not exercised
//! here — this module is unit-tested Tier-0 with synthetic + rig-recipe-derived
//! measurements per `regression-test-first.md`.
//!
//! Pure so it unit-tests on default features (Tier-0) — no OBS, no ffmpeg, no MCP.

/// One A/V-sync measurement, straight off a `recording-verdict --av-sync` JSON report
/// (`av_offset_ms` / `matched` / `mad_ms` — see `.claude/skills/av-sync/SKILL.md`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvSyncMeasurement {
    /// video − audio offset in ms (the #188 convention; negative = video leads audio).
    pub offset_ms: f64,
    /// Clustered-marker count backing the offset (`AvOffset::matched`). Too few means
    /// the offset was estimated from a handful of candidates — not trustworthy.
    pub matched: usize,
    /// Median absolute deviation (ms) of the offset cluster (`AvOffset::mad_ms`). A
    /// high MAD means the "cluster" is actually scattered — not a clean single peak.
    pub mad_ms: f64,
}

/// Strict restart-survival verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum RestartSyncVerdict {
    /// The A/V offset held within tolerance across the restart.
    Pass,
    /// Both measurements were trustworthy, but the offset drifted beyond tolerance —
    /// the real #137 failure mode (a restart that breaks lipsync).
    Fail(Vec<String>),
    /// At least one measurement was NOT trustworthy enough to judge — never reported
    /// as Pass (a noisy "no drift" reading proves nothing) and kept distinct from Fail
    /// (a measurement-quality problem is not proof the restart itself broke sync).
    Unknown(Vec<String>),
}

impl RestartSyncVerdict {
    /// True ONLY for `Pass` — both `Fail` and `Unknown` are "do not ship this restart".
    pub fn is_pass(&self) -> bool {
        matches!(self, RestartSyncVerdict::Pass)
    }

    pub fn label(&self) -> &'static str {
        match self {
            RestartSyncVerdict::Pass => "PASS",
            RestartSyncVerdict::Fail(_) => "FAIL",
            RestartSyncVerdict::Unknown(_) => "UNKNOWN",
        }
    }

    pub fn reasons(&self) -> Vec<String> {
        match self {
            RestartSyncVerdict::Pass => Vec::new(),
            RestartSyncVerdict::Fail(r) | RestartSyncVerdict::Unknown(r) => r.clone(),
        }
    }
}

/// Minimum clustered markers (`AvOffset::matched`) a measurement must carry to be
/// TRUSTED. `recording-verdict --av-sync`'s OWN floor (`--av-min-matched`, default 4)
/// is the bare minimum to report ANY offset at all; this gate needs a stricter margin
/// because a false PASS here directly risks shipping broken lipsync. Doubling the
/// floor to 8 stays well under the ~32 matched observed on a healthy 150 s/96-marker
/// rig run (`.claude/skills/av-sync/SKILL.md`: "matched ~⅓ of emitted markers at
/// 30 fps") while rejecting a marginal, barely-above-floor decode.
pub const MIN_TRUSTED_MATCHED: usize = 8;

/// Maximum MAD (ms) a measurement's cluster may show to be TRUSTED. The rig recipe
/// notes a good measurement's `mad_ms` should be "≤ ~15"; 20 adds ~33% slack above
/// that so legitimate runs are never falsely flagged UNKNOWN, while a genuinely
/// scattered cluster (mad_ms well above the healthy band) still fails the trust check.
pub const MAX_TRUSTED_MAD_MS: f64 = 20.0;

/// Default drift tolerance (ms) for a trusted before/after pair. Chosen an order of
/// magnitude below the reported 200–300 ms restart drift (so a real #137 failure can
/// never hide inside the tolerance band) while sitting comfortably above the
/// post-trust-gate measurement noise floor (both measurements already have
/// `mad_ms <= MAX_TRUSTED_MAD_MS`, so their difference's noise is well under 50 ms).
pub const DEFAULT_TOLERANCE_MS: f64 = 50.0;

/// Trust-check one measurement; empty vec = trusted. Non-finite `offset_ms`/`mad_ms`
/// fails closed (never silently treated as "0 drift" / "tight cluster").
fn trust_reasons(label: &str, m: &AvSyncMeasurement) -> Vec<String> {
    let mut reasons = Vec::new();
    if !m.offset_ms.is_finite() {
        reasons.push(format!(
            "{label} measurement offset_ms is non-finite ({})",
            m.offset_ms
        ));
    }
    if !m.mad_ms.is_finite() {
        reasons.push(format!("{label} measurement mad_ms is non-finite"));
    } else if m.mad_ms > MAX_TRUSTED_MAD_MS {
        reasons.push(format!(
            "{label} measurement mad_ms {:.1} > {:.1} trust threshold (cluster too scattered to trust)",
            m.mad_ms, MAX_TRUSTED_MAD_MS
        ));
    }
    if m.matched < MIN_TRUSTED_MATCHED {
        reasons.push(format!(
            "{label} measurement matched {} < {} trust threshold (too few clustered markers)",
            m.matched, MIN_TRUSTED_MATCHED
        ));
    }
    reasons
}

/// Classify a BEFORE/AFTER A/V-sync measurement pair against `tolerance_ms`. STRICT:
///
/// 1. Either measurement failing its trust check (non-finite, too few `matched`, too
///    high `mad_ms`) FAILS CLOSED to `Unknown` — never `Pass`, regardless of what the
///    raw delta would say (an untrustworthy reading can never manufacture a PASS).
/// 2. Both trusted: `|after.offset_ms - before.offset_ms| > tolerance_ms` → `Fail`
///    (the real #137 signature — a restart that drifted lipsync out of tolerance).
/// 3. Otherwise `Pass`.
///
/// An invalid `tolerance_ms` (non-finite or negative) also fails closed to `Unknown`.
pub fn classify(
    before: AvSyncMeasurement,
    after: AvSyncMeasurement,
    tolerance_ms: f64,
) -> RestartSyncVerdict {
    let mut untrusted = trust_reasons("before", &before);
    untrusted.extend(trust_reasons("after", &after));
    if !untrusted.is_empty() {
        return RestartSyncVerdict::Unknown(untrusted);
    }

    if !tolerance_ms.is_finite() || tolerance_ms < 0.0 {
        return RestartSyncVerdict::Unknown(vec![format!(
            "invalid tolerance_ms {tolerance_ms} (must be finite and >= 0)"
        )]);
    }

    let delta = (after.offset_ms - before.offset_ms).abs();
    if delta > tolerance_ms {
        return RestartSyncVerdict::Fail(vec![format!(
            "A/V offset drifted {delta:.1}ms across the OBS restart (before {:.1}ms, \
             after {:.1}ms) — exceeds the {tolerance_ms:.1}ms tolerance; lipsync would be \
             destroyed (#137)",
            before.offset_ms, after.offset_ms
        )]);
    }

    RestartSyncVerdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(offset_ms: f64) -> AvSyncMeasurement {
        AvSyncMeasurement {
            offset_ms,
            matched: 32,
            mad_ms: 8.0,
        }
    }

    #[test]
    fn small_delta_within_tolerance_passes() {
        let before = trusted(-70.2);
        let after = trusted(-58.7); // 11.5ms drift — normal measurement noise
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(v.is_pass(), "small in-tolerance drift should PASS, got {v:?}");
        assert_eq!(v.label(), "PASS");
    }

    #[test]
    fn two_hundred_ms_drift_fails() {
        // The exact #137 user report: "niekedy sa nam rozsišiel o 200-300ms".
        let before = trusted(-70.0);
        let after = trusted(-270.0); // 200ms drift
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        match &v {
            RestartSyncVerdict::Fail(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("200.0")));
            }
            other => panic!("200ms restart drift MUST FAIL, got {other:?}"),
        }
    }

    #[test]
    fn three_hundred_ms_drift_fails() {
        let before = trusted(0.0);
        let after = trusted(300.0);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(!v.is_pass(), "300ms restart drift MUST FAIL, got {v:?}");
        assert_eq!(v.label(), "FAIL");
    }

    #[test]
    fn boundary_just_over_tolerance_fails() {
        let before = trusted(0.0);
        let after = trusted(DEFAULT_TOLERANCE_MS + 0.5);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(!v.is_pass(), "just-over-tolerance drift must FAIL, got {v:?}");
    }

    #[test]
    fn boundary_just_under_tolerance_passes() {
        let before = trusted(0.0);
        let after = trusted(DEFAULT_TOLERANCE_MS - 0.5);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(v.is_pass(), "just-under-tolerance drift must PASS, got {v:?}");
    }

    #[test]
    fn exactly_at_tolerance_passes() {
        let before = trusted(0.0);
        let after = trusted(DEFAULT_TOLERANCE_MS);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(v.is_pass(), "delta == tolerance is inclusive PASS, got {v:?}");
    }

    #[test]
    fn low_matched_before_is_unknown_even_with_zero_delta() {
        // Fail-closed: an untrustworthy "before" must NEVER be papered over by a
        // suspiciously-perfect zero delta.
        let before = AvSyncMeasurement {
            offset_ms: -70.0,
            matched: 2, // below MIN_TRUSTED_MATCHED
            mad_ms: 5.0,
        };
        let after = trusted(-70.0);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(!v.is_pass(), "untrustworthy 'before' must never PASS, got {v:?}");
        assert_eq!(v.label(), "UNKNOWN");
        assert!(v.reasons().iter().any(|r| r.contains("before") && r.contains("matched")));
    }

    #[test]
    fn high_mad_after_is_unknown() {
        let before = trusted(-70.0);
        let after = AvSyncMeasurement {
            offset_ms: -70.0,
            matched: 32,
            mad_ms: MAX_TRUSTED_MAD_MS + 5.0,
        };
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert_eq!(v.label(), "UNKNOWN");
        assert!(v.reasons().iter().any(|r| r.contains("after") && r.contains("mad_ms")));
    }

    #[test]
    fn untrustworthy_measurement_never_masks_a_real_drift_as_pass() {
        // A large apparent drift PLUS an untrustworthy measurement must still not be
        // silently swallowed as Pass — it must surface as Unknown (never Fail-hidden
        // as Pass, and the trust problem itself is reported).
        let before = AvSyncMeasurement {
            offset_ms: -70.0,
            matched: 1,
            mad_ms: 50.0,
        };
        let after = trusted(-270.0);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(!v.is_pass(), "untrustworthy + large delta must never PASS, got {v:?}");
    }

    #[test]
    fn non_finite_offset_is_unknown() {
        let before = AvSyncMeasurement {
            offset_ms: f64::NAN,
            matched: 32,
            mad_ms: 8.0,
        };
        let after = trusted(-70.0);
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert_eq!(v.label(), "UNKNOWN");
    }

    #[test]
    fn invalid_tolerance_is_unknown() {
        let v = classify(trusted(0.0), trusted(0.0), f64::NAN);
        assert_eq!(v.label(), "UNKNOWN");
        let v = classify(trusted(0.0), trusted(0.0), -1.0);
        assert_eq!(v.label(), "UNKNOWN");
    }

    #[test]
    fn real_rig_recipe_measurement_passes_within_tolerance() {
        // The proven 2026-07-02 rig recipe measured -70.2ms +/-10 @ 1000ms NDI latency
        // (.claude/skills/av-sync/SKILL.md). A restart that lands within the measured
        // noise band must PASS.
        let before = AvSyncMeasurement {
            offset_ms: -70.2,
            matched: 32,
            mad_ms: 10.0,
        };
        let after = AvSyncMeasurement {
            offset_ms: -64.5,
            matched: 30,
            mad_ms: 9.5,
        };
        let v = classify(before, after, DEFAULT_TOLERANCE_MS);
        assert!(v.is_pass(), "rig-recipe-derived healthy pair should PASS, got {v:?}");
    }

    #[test]
    fn is_pass_and_label_reflect_every_variant() {
        assert!(RestartSyncVerdict::Pass.is_pass());
        assert_eq!(RestartSyncVerdict::Pass.label(), "PASS");
        assert!(RestartSyncVerdict::Pass.reasons().is_empty());

        let f = RestartSyncVerdict::Fail(vec!["x".to_string()]);
        assert!(!f.is_pass());
        assert_eq!(f.label(), "FAIL");
        assert_eq!(f.reasons(), vec!["x".to_string()]);

        let u = RestartSyncVerdict::Unknown(vec!["y".to_string()]);
        assert!(!u.is_pass());
        assert_eq!(u.label(), "UNKNOWN");
        assert_eq!(u.reasons(), vec!["y".to_string()]);
    }
}
