//! #461 — burn-less optical zero-loss gate for a node that intentionally carries NO digital
//! node-burn (imag-nb: the new 60fps low-latency IMAG box, EPIC #466 Topology v2).
//!
//! Every OTHER node's zero-loss proof (`probe::burn_contiguity::burn_contiguity`) is a
//! first..=last CONTIGUITY check over a DIGITAL burn id we inject at the OBS render tick. imag
//! has no burn yet (911003 is RESERVED for it, wired in a later ticket, #463) — but it does NOT
//! need one: imag records the cam2 painter's 60Hz dual-QR at 60fps, 1:1, with NO 60->30 beat (the
//! beat that forces strih/stream to treat the optical read as diagnostic-only lives at the
//! cam->strih / strih->stream hops, not here). So the cam2 OPTICAL tick's own first..=last
//! contiguity (step=1) IS a genuine zero-loss proof for imag: every painted tick that imag's
//! camera captured maps 1:1 onto a recorded frame, so a missing tick integer in the analyzed
//! span means imag's camera FAILED to capture that instant — the same "any gap in the span is a
//! candidate drop" logic `burn_contiguity` applies to a digital id, applied here to the optical
//! tick instead.
//!
//! This is a SIBLING of `probe::burn_contiguity::burn_contiguity`, deliberately duplicated as a
//! crate-root, non-probe module rather than reused from `probe::` — the WHOLE `probe` module is
//! `#[cfg(feature = "probe")]` (CI-only), so a pure decision that lives there can never be
//! RED->GREEN-verified locally. Mirrors the `reannounce.rs` / `colour_scale.rs` /
//! `recording_span_gate.rs` Tier-0 seam pattern: this module holds ONLY primitive-typed pure
//! logic (`&[u32]` in, a plain result struct out) so it compiles + tests on DEFAULT features; the
//! probe-gated `bin/recording-verdict` extracts `RecordingFrame::tick` and calls in.

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
}
