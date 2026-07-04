//! #461 — burn-less optical zero-loss gate for imag-nb (the 60fps low-latency IMAG box, EPIC
//! #466 Topology v2), extended by #463 to AND-in imag's OWN digital corner burn now that it has
//! one (run_id [`crate::probe::recording_latency::BURN_RUN_ID_IMAG`] = 911003, burned by the OBS
//! filter's new `Corner::BottomCenterLeft` — `vendor/distroav/src/burn-geom.hpp`).
//!
//! Every OTHER node's zero-loss proof (`probe::burn_contiguity::burn_contiguity`) is a
//! first..=last CONTIGUITY check over a DIGITAL burn id injected at the OBS render tick. Before
//! #463, imag had no burn — but it did not strictly need one: imag records the cam2 painter's
//! 60Hz dual-QR at 60fps, 1:1, with NO 60->30 beat (the beat that forces strih/stream to treat
//! the optical read as diagnostic-only lives at the cam->strih / strih->stream hops, not here).
//! So the cam2 OPTICAL tick's own first..=last contiguity (step=1) IS a genuine zero-loss proof
//! for imag on its own: every painted tick that imag's camera captured maps 1:1 onto a recorded
//! frame, so a missing tick integer in the analyzed span means imag's camera FAILED to capture
//! that instant — the same "any gap in the span is a candidate drop" logic `burn_contiguity`
//! applies to a digital id, applied here to the optical tick instead.
//!
//! **#463 — now imag ALSO carries a digital burn, so BOTH signals gate it (stricter, per the
//! strict-test mandate — never accept a weaker proof once a stronger one is available):**
//! [`ImagVerdict`] ANDs the optical tick contiguity with the burn's OWN first..=last contiguity
//! WHEN the burn is present in the recording. A recording with NO burn decoded at all (an older
//! recording, or a build not yet carrying the corner burn) falls back to the ORIGINAL
//! optical-only proof — see [`ImagVerdict::is_zero_loss`].
//!
//! This is a SIBLING of `probe::burn_contiguity::burn_contiguity`, deliberately duplicated as a
//! crate-root, non-probe module rather than reused from `probe::` — the WHOLE `probe` module is
//! `#[cfg(feature = "probe")]` (CI-only), so a pure decision that lives there can never be
//! RED->GREEN-verified locally. Mirrors the `reannounce.rs` / `colour_scale.rs` /
//! `recording_span_gate.rs` Tier-0 seam pattern: this module holds ONLY primitive-typed pure
//! logic (`&[u32]` in, a plain result struct out) so it compiles + tests on DEFAULT features; the
//! probe-gated `bin/recording-verdict` extracts `RecordingFrame::tick` (+ the burn ids via
//! `probe::recording_latency::burn_ids_in` / `probe::burn_contiguity::burn_contiguity`) and
//! calls in.

/// The tick-contiguity verdict for imag's optical proof: first..=last integer contiguity over
/// every distinct cam2 painted tick imag's recording decoded. Mirrors
/// `probe::burn_contiguity::NodeContiguity`'s shape (so the probe-gated caller can convert this
/// straight into the existing `NodeContiguity` / `NodeVerdict` reporting machinery) without
/// depending on that probe-gated type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickContiguity {
    /// First tick value seen (the start of the analyzed span). `None` ⇒ no tick decoded at all.
    pub first_tick: Option<u32>,
    /// Last tick value seen (the end of the analyzed span). `None` ⇒ no tick decoded at all.
    pub last_tick: Option<u32>,
    /// How many DISTINCT tick values were decoded.
    pub present_count: u32,
    /// How many tick values the contiguous span `first..=last` should contain.
    pub expected_count: u32,
    /// The exact tick values missing from `first..=last`, sorted ascending. Empty ⇒ contiguous.
    pub missing_ticks: Vec<u32>,
}

impl TickContiguity {
    /// ZERO loss ⇔ the optical tick sequence is contiguous (no missing tick value). A recording
    /// with NO decoded tick at all (`first_tick == None`) is NOT a pass — there is nothing proven
    /// zero on, mirroring `NodeContiguity::is_contiguous`'s same "empty is not a pass" rule.
    pub fn is_contiguous(&self) -> bool {
        self.first_tick.is_some() && self.missing_ticks.is_empty()
    }
}

/// #463 — imag's FULL zero-loss verdict: the optical tick contiguity ANDed with the digital
/// corner-burn contiguity, WHEN the burn is present in the recording. Kept as a separate pure
/// combinator (rather than folding the AND into the caller) so the "burn present but broken" vs
/// "burn absent, fall back to optical-only" decision is unit-tested here, Tier-0, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagVerdict {
    /// Whether the cam2 optical tick sequence was contiguous ([`TickContiguity::is_contiguous`]).
    pub optical_contiguous: bool,
    /// `None` ⇒ NO digital burn id was decoded anywhere in the recording at all — the pre-#463
    /// state (an older recording, or a build not yet carrying imag's corner burn). The verdict
    /// falls back to the optical proof alone, unchanged from pre-#463 behaviour.
    /// `Some(contiguous)` ⇒ the burn WAS decoded; `contiguous` is whether ITS OWN first..=last
    /// span was gap-free. #463's whole point: once a stronger (digital) proof exists, it must
    /// ALSO hold — a present-but-gappy burn now fails the node even if the optical read is clean.
    pub burn_contiguous: Option<bool>,
}

impl ImagVerdict {
    /// ZERO loss ⇔ the optical tick is contiguous AND (no burn was present in this recording OR
    /// the burn is ALSO contiguous). See the struct doc for the fallback rationale — this is
    /// STRICTER than pre-#463 (never looser): a node with a decoded-but-gappy burn now fails even
    /// though its optical read alone was clean, per the strict-test mandate.
    pub fn is_zero_loss(&self) -> bool {
        self.optical_contiguous && self.burn_contiguous.unwrap_or(true)
    }
}

/// Build imag's [`ImagVerdict`] from its optical [`TickContiguity`] and, separately, whether ANY
/// digital corner-burn id was decoded at all in the recording (`burn_present`) and — only when it
/// was — whether that burn's OWN span was contiguous (`burn_contiguous_if_present`). Splitting the
/// two burn booleans (rather than a single `Option<bool>` at the call site) keeps the caller
/// honest: it cannot accidentally collapse "burn absent" and "burn present and contiguous" into
/// the same `true`, which would silently hide a real burn from ever being checked.
pub fn imag_verdict(
    optical: &TickContiguity,
    burn_present: bool,
    burn_contiguous_if_present: bool,
) -> ImagVerdict {
    ImagVerdict {
        optical_contiguous: optical.is_contiguous(),
        burn_contiguous: burn_present.then_some(burn_contiguous_if_present),
    }
}

/// THE pure check: given every cam2 optical tick value decoded from imag's recording (duplicates
/// and any order allowed — a tick sampled twice, or frames in file order, are never loss), report
/// whether the first..=last integer span is contiguous and, if not, every missing tick value.
///
/// Identical algorithm to `probe::burn_contiguity::burn_contiguity`, intentionally duplicated (see
/// the module doc) so it is unit-testable without the `probe` feature.
///
/// An empty input ⇒ `first_tick == None` ⇒ NOT contiguous (nothing proven). A single-tick input is
/// trivially contiguous (a span of one; nothing can be missing inside it).
pub fn tick_contiguity(ticks: &[u32]) -> TickContiguity {
    use std::collections::BTreeSet;
    let present: BTreeSet<u32> = ticks.iter().copied().collect();
    let first_tick = present.iter().next().copied();
    let last_tick = present.iter().next_back().copied();
    let (first, last) = match (first_tick, last_tick) {
        (Some(f), Some(l)) => (f, l),
        _ => {
            return TickContiguity {
                first_tick: None,
                last_tick: None,
                present_count: 0,
                expected_count: 0,
                missing_ticks: Vec::new(),
            };
        }
    };
    // expected = last - first + 1 (size of the contiguous integer span). Saturating math so a
    // degenerate full-u32-range span (unreachable for a run-bounded tick counter, but defensive)
    // can never panic on a debug-build overflow — mirrors `burn_contiguity`'s own guard.
    let expected_count = last.saturating_sub(first).saturating_add(1);
    let missing_ticks: Vec<u32> = (first..=last).filter(|t| !present.contains(t)).collect();
    TickContiguity {
        first_tick: Some(first),
        last_tick: Some(last),
        present_count: present.len() as u32,
        expected_count,
        missing_ticks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_not_contiguous_461() {
        let tc = tick_contiguity(&[]);
        assert_eq!(tc.first_tick, None);
        assert_eq!(tc.last_tick, None);
        assert_eq!(tc.present_count, 0);
        assert_eq!(tc.expected_count, 0);
        assert!(tc.missing_ticks.is_empty());
        assert!(
            !tc.is_contiguous(),
            "no tick decoded at all must never read as a zero-loss pass"
        );
    }

    #[test]
    fn single_tick_is_trivially_contiguous_461() {
        let tc = tick_contiguity(&[42]);
        assert_eq!(tc.first_tick, Some(42));
        assert_eq!(tc.last_tick, Some(42));
        assert_eq!(tc.present_count, 1);
        assert_eq!(tc.expected_count, 1);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn a_clean_run_of_consecutive_ticks_is_contiguous_461() {
        let ticks: Vec<u32> = (100..=200).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(100));
        assert_eq!(tc.last_tick, Some(200));
        assert_eq!(tc.present_count, 101);
        assert_eq!(tc.expected_count, 101);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn a_single_missing_tick_in_the_middle_fails_461() {
        // 100..=200 minus 150 -> imag's camera failed to capture that one painted instant.
        let ticks: Vec<u32> = (100..=200).filter(|&t| t != 150).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(100));
        assert_eq!(tc.last_tick, Some(200));
        assert_eq!(tc.present_count, 100);
        assert_eq!(tc.expected_count, 101);
        assert_eq!(tc.missing_ticks, vec![150]);
        assert!(
            !tc.is_contiguous(),
            "a single missing tick value inside the span is a candidate dropped frame"
        );
    }

    #[test]
    fn multiple_missing_ticks_are_all_reported_sorted_461() {
        let ticks: Vec<u32> = (0..=10).filter(|t| ![3, 7, 8].contains(t)).collect();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.missing_ticks, vec![3, 7, 8]);
        assert!(!tc.is_contiguous());
    }

    #[test]
    fn duplicates_do_not_break_contiguity_461() {
        // A tick sampled on more than one recorded frame (e.g. a slow-shutter straddle) is never
        // loss -- the SAME painted instant, not a second one.
        let mut ticks: Vec<u32> = (0..=20).collect();
        ticks.extend([5, 5, 5, 12, 12]);
        let tc = tick_contiguity(&ticks);
        assert_eq!(
            tc.present_count, 21,
            "duplicates collapse to distinct ticks"
        );
        assert_eq!(tc.expected_count, 21);
        assert!(tc.missing_ticks.is_empty());
        assert!(tc.is_contiguous());
    }

    #[test]
    fn out_of_order_input_is_handled_identically_461() {
        // Recorded-frame order is irrelevant -- this is a SET membership test over first..=last.
        let mut ticks: Vec<u32> = (0..=50).collect();
        ticks.reverse();
        let tc = tick_contiguity(&ticks);
        assert_eq!(tc.first_tick, Some(0));
        assert_eq!(tc.last_tick, Some(50));
        assert!(tc.is_contiguous());
    }

    #[test]
    fn expected_count_saturates_instead_of_panicking_on_overflow() {
        // Defensive: `last - first + 1` must never PANIC on a debug-build overflow, mirroring
        // `burn_contiguity`'s own guard. A run-bounded tick counter never realistically reaches
        // u32::MAX, so this checks only the saturating ARITHMETIC in isolation — NOT the full
        // `tick_contiguity` (which additionally walks every integer in the span to find gaps;
        // walking a genuine 4-billion-wide span is intentionally never exercised, same as
        // `burn_contiguity` never tests it end-to-end either).
        let expected_count = u32::MAX.saturating_sub(0).saturating_add(1);
        assert_eq!(
            expected_count,
            u32::MAX,
            "saturates instead of overflowing to 0"
        );
    }

    // ---- #463 — imag's optical+burn AND gate (ImagVerdict) ----

    #[test]
    fn no_burn_present_falls_back_to_optical_only_463() {
        // Pre-#463 behaviour preserved: an older recording (or a build not yet carrying imag's
        // corner burn) has NO burn id decoded at all — the verdict must fall back to the optical
        // proof alone, in BOTH directions (optical clean passes; optical broken still fails).
        let clean = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&clean, false, false);
        assert_eq!(
            v.burn_contiguous, None,
            "no burn decoded ⇒ None, not Some(false)"
        );
        assert!(
            v.is_zero_loss(),
            "optical contiguous + no burn present ⇒ zero loss (unchanged pre-#463 behaviour)"
        );

        let broken = tick_contiguity(&(100..=159).filter(|&t| t != 130).collect::<Vec<_>>());
        let v2 = imag_verdict(&broken, false, false);
        assert!(
            !v2.is_zero_loss(),
            "a missing optical tick still fails even with no burn present"
        );
    }

    #[test]
    fn burn_present_and_contiguous_plus_optical_contiguous_is_zero_loss_463() {
        // #463: BOTH signals present and clean ⇒ zero loss — the stricter, doubly-proven pass.
        let optical = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, true);
        assert_eq!(v.burn_contiguous, Some(true));
        assert!(
            v.is_zero_loss(),
            "optical + burn both contiguous ⇒ zero loss"
        );
    }

    #[test]
    fn burn_present_but_not_contiguous_fails_even_though_optical_is_clean_463() {
        // #463's whole point: the optical tick alone is NOT enough once imag has a digital burn —
        // a present-but-gappy burn must FAIL the node even though the optical read is perfect
        // (never let a weaker proof override a stronger one that disagrees).
        let optical = tick_contiguity(&(100..=159).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, false);
        assert!(optical.is_contiguous(), "sanity: optical alone is clean");
        assert_eq!(v.burn_contiguous, Some(false));
        assert!(
            !v.is_zero_loss(),
            "a present-but-non-contiguous burn FAILS the node even with a clean optical read (#463)"
        );
    }

    #[test]
    fn optical_broken_fails_even_when_burn_is_contiguous_463() {
        // The optical read stays a HARD requirement (mirrors #363's stance for strih/stream): a
        // contiguous digital burn can never paper over a broken optical proof.
        let optical = tick_contiguity(&(100..=159).filter(|&t| t != 130).collect::<Vec<_>>());
        let v = imag_verdict(&optical, true, true);
        assert!(
            !v.is_zero_loss(),
            "a missing optical tick FAILS even when the digital burn is perfectly contiguous"
        );
    }
}
