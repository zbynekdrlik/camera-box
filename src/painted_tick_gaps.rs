//! #625 — order-independent REAL-DROP ("gap") detection for the all-cambox painted-tick window
//! continuity check (`probe::recording_segments::window_segment`), extracted here so the fix is
//! RED->GREEN-verifiable on DEFAULT features. Mirrors the `reannounce.rs` / `colour_scale.rs` /
//! `imag_tick_gate.rs` Tier-0 seam pattern: the WHOLE `probe` module is `#[cfg(feature = "probe")]`
//! (CI-only), so a pure decision that lives there can never be verified locally.
//!
//! ## The bug (live evidence, run 1783530925, 2026-07-08)
//!
//! The all-cambox 1800s sweep reported `all_cambox_continuity` FAILING on every single ~30s
//! window (e.g. `CAM1 frames=846 undecodable=4 copies=17 gaps=287`), even though the SAME
//! recording's per-node digital burn check (`full_chain`) proved `0 REAL DROP` across the WHOLE
//! run (`136283 BURN-UNREADABLE` only) and the run's `all_cambox_continuity.frames_without_anchor`
//! was `0` (every physical frame WAS attributed to a window). The first/last painted tick of a
//! typical window (e.g. CAM1's first window: `first_tick=9231 last_tick=10920` over 846 frames at
//! the by-design step 2) nets out to `1689` — within 1 tick of the expected `1690` — proving there
//! is NO real net loss over the window as a whole, yet the per-transition walk reported 287 gaps.
//!
//! ## The root cause
//!
//! `window_segment` (pre-#625) walked the painted tick in RECORDED order and treated any backward
//! step (`t < prev`) as an unconditional gap, plus charged the excess of any oversized forward
//! step. This is EXACTLY the class of over-count `probe::burn_contiguity::
//! burn_contiguity_in_window_with_step` was hardened against for cam1's OWN burn (#216/#356):
//! the stream recording is repeatedly documented (`#133`/`#196`/`#216`) to be "softened" — an
//! extra NDI hop + HEVC re-encode occasionally delivers a frame "out of order" (a one-frame-late
//! 60->30 straddle) — and that module's own doc says plainly: "a reordered-but-present id is just
//! the softened recording delivering it late, never a fault." `window_segment`'s recorded-order
//! walk had no such reorder tolerance: ONE benign swap (`a, a+4, a+2, a+6` instead of
//! `a, a+2, a+4, a+6`) manufactures THREE phantom gaps (a backward jump, then TWO oversized
//! forward jumps) for ZERO actual loss.
//!
//! ## The fix
//!
//! [`painted_tick_gaps`] walks the DISTINCT present tick VALUES in SORTED (not recorded) order.
//! Since the underlying painted tick is a monotonically-increasing counter at the SOURCE
//! (cam2's painter), sorting recovers the true delivery-order-independent sequence; a benign
//! reorder collapses to zero extra gaps (sorting a swapped-but-complete run yields the SAME
//! sorted sequence as the unswapped run). The existing decimation-aware tolerance
//! (`delta / expected_step - 1`, crediting up to `undecodable` frames) is UNCHANGED — only the
//! walk order changes — so a genuinely missing tick value is still caught (never masked): sorting
//! cannot make an absent value appear, it can only stop a present-but-reordered value from being
//! misread as absent-then-duplicated.

/// Compute the number of genuinely-missing painted-tick slots ("gaps") from the DISTINCT present
/// tick values observed in one all-cambox schedule window, crediting up to `undecodable` (frames
/// that reached the recording but whose painted tick did not decode — BURN-UNREADABLE, never a
/// lost frame) against the count. `present_ticks` may be given in ANY order (recorded order,
/// reversed, shuffled) — the function sorts internally, so a benign stream-recording reorder
/// (`#133`/`#196`/`#216`) can never manufacture a phantom gap. `expected_step` is the by-design
/// decimation step (2 for the 60Hz painter read from a 30fps stream recording); values `<= 0`
/// floor to `1` (no decimation).
pub fn painted_tick_gaps(present_ticks: &[u32], undecodable: u32, expected_step: i64) -> u32 {
    let expected_step = expected_step.max(1);

    let mut sorted: Vec<u32> = present_ticks.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut raw_gaps: i64 = 0;
    for w in sorted.windows(2) {
        let delta = i64::from(w[1]) - i64::from(w[0]);
        let lost = (delta / expected_step) - 1;
        if lost > 0 {
            raw_gaps += lost;
        }
    }

    // Each BURN-UNREADABLE (undecodable) frame reached the recording but its painted tick did not
    // decode — it may have occupied exactly one candidate slot, so credit up to `undecodable`
    // against the raw gap count (never below zero; never MORE than what was actually charged).
    let credited = raw_gaps - i64::from(undecodable);
    credited.max(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_step_2_sequence_has_no_gaps() {
        let present = [1000, 1002, 1004, 1006, 1008];
        assert_eq!(painted_tick_gaps(&present, 0, 2), 0);
    }

    #[test]
    fn reordered_clean_sequence_has_no_gaps_625() {
        // THE #625 regression: the same clean step-2 sequence as
        // `clean_step_2_sequence_has_no_gaps`, but delivered "softened"/out of order (a
        // one-frame-late 60->30 straddle: 1002 and 1004 arrive swapped). A recorded-order walk
        // (the pre-#625 behavior) sees 1000->1004 (an oversized forward jump), 1004->1002 (a
        // backward jump), 1002->1006 (another oversized forward jump) = 3 phantom gaps for ZERO
        // actual loss. The order-independent walk must report ZERO.
        let present = [1000, 1004, 1002, 1006, 1008];
        assert_eq!(
            painted_tick_gaps(&present, 0, 2),
            0,
            "a benign delivery reorder must not manufacture a gap"
        );
    }

    #[test]
    fn reordered_sequence_with_a_real_drop_still_reports_it_625() {
        // Reordering must never MASK a genuine drop either: 1004 is truly missing (never
        // delivered), on top of the same 1002/1006 reorder as above.
        let present = [1000, 1006, 1002, 1008];
        assert_eq!(
            painted_tick_gaps(&present, 0, 2),
            1,
            "the genuinely-missing 1004 must still be counted, reorder or not"
        );
    }

    #[test]
    fn genuine_missing_value_is_still_a_gap() {
        // 102 is genuinely absent (not just reordered) — mirrors
        // `non_adjacent_freeze_hiding_a_real_drop_still_fails` in recording_segments.rs.
        let present = [100, 101, 103];
        assert_eq!(painted_tick_gaps(&present, 0, 1), 1);
    }

    #[test]
    fn undecodable_frames_credit_against_gaps() {
        // 501 is missing from the present set, but exactly one delivered frame in this window
        // was undecodable — it may have carried the missing value, so it is credited.
        let present = [500, 502];
        assert_eq!(painted_tick_gaps(&present, 1, 1), 0);
    }

    #[test]
    fn undecodable_credit_never_goes_negative() {
        // A clean, gapless sequence with undecodable frames elsewhere in the window (e.g. near a
        // boundary) must never underflow into a bogus non-zero/negative result.
        let present = [500, 501, 502];
        assert_eq!(painted_tick_gaps(&present, 5, 1), 0);
    }

    #[test]
    fn duplicate_values_dedup_without_manufacturing_gaps() {
        // A literal repeated value (the "copies"/stale-frame signal, tracked separately by the
        // caller) must not, by itself, register as a gap once deduped.
        let present = [500, 500, 501];
        assert_eq!(painted_tick_gaps(&present, 0, 1), 0);
    }

    #[test]
    fn empty_input_reports_zero_gaps() {
        assert_eq!(painted_tick_gaps(&[], 0, 2), 0);
    }

    #[test]
    fn single_value_reports_zero_gaps() {
        assert_eq!(painted_tick_gaps(&[42], 0, 2), 0);
    }

    #[test]
    fn expected_step_floors_to_one() {
        let present = [10, 11, 12];
        assert_eq!(
            painted_tick_gaps(&present, 0, 0),
            painted_tick_gaps(&present, 0, 1)
        );
        assert_eq!(
            painted_tick_gaps(&present, 0, -5),
            painted_tick_gaps(&present, 0, 1)
        );
    }
}
