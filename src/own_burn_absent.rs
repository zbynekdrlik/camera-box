//! issue 1247 — per-camera "own digital burn absent" REPORT-ONLY verdict seam.
//!
//! WHY (the gap this closes): the `#133` unmeasured-camera safeguard in
//! `src/bin/recording-verdict.rs` warns only when EVERY camera-under-test burn is absent
//! (OR-logic). Under the ALL-CAMBOX sweep a SINGLE scheduled cam whose OWN digital burn is
//! entirely absent (`full_chain.burn_ids_present.<cam> == 0`) — because its leg was live but
//! served by production `camera-box.service`, which emits NO digital burn (the still-open
//! issue-1246 cam2-painter-deadman symptom) — produced no warning at all, and the per-segment
//! optical-tick verdict (which measures a cam via the SHARED painter tick + its schedule window,
//! never its own burn — the documented issue-312 Phase-1 limitation) can read that cam as a clean
//! pass. On run 1635844760 that yielded `overall_pass: true` with `burn_ids_present.cam2 == 0` —
//! a durable artifact overstating cam2 as a clean pass.
//!
//! This is the PURE decision kernel (no probe deps, no serde — unit-tests Tier-0 on default
//! features, the project's CLAUDE.md "Local Build Policy" mandate): given the run's DEPLOYED
//! cambox set (the distinct `cambox` labels from the `--switch-schedule`, NOT `expected_burns` —
//! which lists all cams regardless of deployment) and the per-cambox decoded digital-burn COUNT,
//! it returns the scheduled cams whose own burn was entirely absent. `src/bin/recording-verdict.rs`
//! is the sole caller: it serializes `full_chain.own_burn_absent_gate` and folds the term
//! report-only.
//!
//! ## [`gates_overall_pass`] — REPORT-ONLY (issue 1247, ROZHODNUTÉ: option 1)
//!
//! [`evaluate`] is always computed + fully serialized, but the caller NEVER folds it into
//! `overall_pass` (`all_pass &= gate_pass || !gates_overall_pass()` is a NO-OP while this is
//! `false`), so PASS/FAIL is unchanged — exactly the decision. Mirrors the seam shape of
//! `crate::optical_floor::gates_overall_pass` / `crate::tear_detect::gates_overall_pass`
//! (hardcoded `false`). A one-line flip to `true` makes an absent scheduled-cam burn FAIL the run
//! — deliberately NOT done: it would duplicate the LIVE `[7b/8]` burn-unit run-integrity check
//! (`scripts/recording-e2e.sh`, issue 894), which already fails such a run.

/// Report-only seam (issue 1247): the pure decision in [`evaluate`] is always computed + reported,
/// but the caller never folds it into any pass/fail exit code yet. Hardcoded `false` — mirrors
/// `crate::optical_floor::gates_overall_pass`. Flip to `true` (one line) to make an absent
/// scheduled-cam own burn FAIL `overall_pass`; NOT done per the decision (redundant with the LIVE
/// `[7b/8]` run-integrity check).
pub fn gates_overall_pass() -> bool {
    false
}

/// Per-scheduled-cambox digital-burn presence — the report-only gate's structured result.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnBurnPresence {
    /// The scheduled camboxes actually ASSESSED (scheduled ∩ known burn-count keys), canonical
    /// lowercase, sorted, deduped. A scheduled cambox with no matching burn-count key (e.g.
    /// `"imag"`, measured by its own leg gate) is OUT of this gate's scope and excluded.
    pub scheduled_cams: Vec<String>,
    /// Per assessed cambox: was its OWN digital burn entirely absent (count == 0)? `(cam, absent)`,
    /// sorted by cam. This is the source of the per-camera `own_burn_absent` map in the verdict.
    pub per_cam_absent: Vec<(String, bool)>,
    /// Run-level list of scheduled camboxes whose own digital burn was entirely absent (the WARN
    /// set). Sorted, canonical lowercase. Empty ⇒ the gate passes.
    pub absent_cams: Vec<String>,
}

impl OwnBurnPresence {
    /// The report-only gate PASSES iff no scheduled cam had its own digital burn absent.
    pub fn pass(&self) -> bool {
        self.absent_cams.is_empty()
    }
}

/// Pure decision (issue 1247): given the run's DEPLOYED/scheduled cambox set (the distinct `cambox`
/// labels from the switch-schedule) and the per-cambox decoded digital-burn COUNT
/// (`full_chain.burn_ids_present.<cam>`), find every SCHEDULED cambox whose OWN digital burn was
/// ENTIRELY ABSENT (count == 0) from the recording.
///
/// Matching is case-insensitive — schedule labels are UPPERCASE (`"CAM2"`, from
/// `scripts/switch_schedule.py`'s sweep spec) while `burn_counts` keys are lowercase (`"cam2"`,
/// mirroring the verdict's own `burn_ids_present` keys); output labels are canonical lowercase to
/// match those keys. ONLY scheduled cams that HAVE a matching burn-count entry are assessed — an
/// unknown/unmeasured cambox proves nothing about its own burn, so it is excluded (never a false
/// warning). The scheduled set is de-duplicated (the sweep cycles a cam across several windows).
pub fn evaluate(_scheduled_camboxes: &[String], _burn_counts: &[(&str, usize)]) -> OwnBurnPresence {
    // #1247 RED stub — the real decision lands in the GREEN commit. Returns an empty result so the
    // "should-flag" tests fail (RED) while the "clean / empty schedule" tests already hold.
    OwnBurnPresence {
        scheduled_cams: Vec::new(),
        per_cam_absent: Vec::new(),
        absent_cams: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduction fixture: burn counts mirroring the two real runs
    /// (`verdict-1635844760.json` / `verdict-1347045170.json`) — cam2's own burn is entirely
    /// absent while every other scheduled cam carries a non-zero count.
    fn repro_counts() -> Vec<(&'static str, usize)> {
        vec![
            ("cam1", 4535),
            ("cam2", 0),
            ("cam3", 1817),
            ("cam4", 907),
            ("cam5", 907),
            ("cam6", 907),
            ("cam7", 906),
        ]
    }

    fn all_seven() -> Vec<String> {
        ["CAM1", "CAM2", "CAM3", "CAM4", "CAM5", "CAM6", "CAM7"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn gates_overall_pass_is_report_only_1247() {
        assert!(
            !gates_overall_pass(),
            "issue 1247 ships REPORT-ONLY (ROZHODNUTÉ option 1) — the [7b/8] run-integrity check \
             already fails such a run; this term must never change PASS/FAIL"
        );
    }

    #[test]
    fn cam2_scheduled_with_zero_burn_is_flagged_1247() {
        // The exact reproduction: CAM1..CAM7 scheduled, cam2's own digital burn absent.
        let p = evaluate(&all_seven(), &repro_counts());
        assert_eq!(
            p.absent_cams,
            vec!["cam2".to_string()],
            "cam2 is scheduled yet its own burn count is 0 — it MUST be flagged"
        );
        assert!(!p.pass(), "an absent scheduled-cam own burn must fail the (report-only) gate");
        assert!(
            p.per_cam_absent.iter().any(|(c, a)| c == "cam2" && *a),
            "per-cam map must carry cam2=true"
        );
        assert!(
            p.per_cam_absent.iter().any(|(c, a)| c == "cam1" && !*a),
            "per-cam map must carry cam1=false (its burn was present)"
        );
    }

    #[test]
    fn all_burns_present_passes_1247() {
        let scheduled = vec!["CAM1".to_string(), "CAM2".to_string()];
        let counts = vec![("cam1", 10usize), ("cam2", 20usize)];
        let p = evaluate(&scheduled, &counts);
        assert!(p.absent_cams.is_empty());
        assert!(p.pass());
    }

    #[test]
    fn empty_schedule_flags_nothing_1247() {
        // No switch-schedule (single-camera mode) ⇒ nothing scheduled ⇒ nothing to warn about
        // (the #133 all-absent WARN covers that mode).
        let p = evaluate(&[], &repro_counts());
        assert!(p.scheduled_cams.is_empty());
        assert!(p.absent_cams.is_empty());
        assert!(p.pass());
    }

    #[test]
    fn case_insensitive_uppercase_schedule_lowercase_keys_1247() {
        // schedule labels are UPPERCASE, burn-count keys lowercase — must still match.
        let scheduled = vec!["CAM2".to_string()];
        let counts = vec![("cam2", 0usize)];
        let p = evaluate(&scheduled, &counts);
        assert_eq!(p.absent_cams, vec!["cam2".to_string()]);
        assert_eq!(p.scheduled_cams, vec!["cam2".to_string()]);
    }

    #[test]
    fn scheduled_cam_without_a_burn_count_key_is_not_assessed_1247() {
        // "imag" is scheduled but has no burn-count entry -> OUT of this gate's scope, never a
        // false warning; cam2 (present, count 0) is still flagged.
        let scheduled = vec!["imag".to_string(), "CAM2".to_string()];
        let counts = vec![("cam2", 0usize)];
        let p = evaluate(&scheduled, &counts);
        assert_eq!(p.scheduled_cams, vec!["cam2".to_string()]);
        assert_eq!(p.absent_cams, vec!["cam2".to_string()]);
        assert!(!p.scheduled_cams.contains(&"imag".to_string()));
    }

    #[test]
    fn repeated_schedule_windows_dedup_the_cam_1247() {
        // the sweep cycles a cam across multiple windows -> it appears exactly once in the output.
        let scheduled = vec!["CAM2".to_string(), "CAM1".to_string(), "CAM2".to_string()];
        let counts = vec![("cam1", 5usize), ("cam2", 0usize)];
        let p = evaluate(&scheduled, &counts);
        assert_eq!(p.scheduled_cams, vec!["cam1".to_string(), "cam2".to_string()]);
        assert_eq!(p.absent_cams, vec!["cam2".to_string()]);
        assert_eq!(
            p.per_cam_absent.iter().filter(|(c, _)| c == "cam2").count(),
            1,
            "cam2 must appear exactly once despite two scheduled windows"
        );
    }
}
