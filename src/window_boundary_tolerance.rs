//! #903 — the boundary TOLERANCE for deciding whether a backward burn-id jump between two
//! present frames crosses a `--switch-schedule` program-switch boundary.
//!
//! strih's OWN 911002 burn is six INDEPENDENT per-source DistroAV filter instances multiplexed
//! onto program output (see `probe::burn_contiguity`'s own doc), so a program switch legitimately
//! hands the recording a numerically UNRELATED value from a different counter instance — a large
//! backward jump that is NOT a real drop. #708/#741 already confirm this exception when the
//! previous and current present frame's `gen_ts_ns` resolve to two DIFFERENT `--switch-schedule`
//! windows via `raw_window_index` — an EXACT, unguarded interval test.
//!
//! That exact test has NO tolerance. The schedule boundary instant is stamped on dev1's clock;
//! a frame's `gen_ts_ns` is stamped on the painter's clock. The only guarantee those two clocks'
//! agreement carries is the `#326` clock-offset gate, which merely bounds their disagreement under
//! 200 ms — nowhere near the microsecond resolution an exact-instant `>=`/`<` comparison demands.
//! So a genuine switch whose frame timestamps straddle the boundary by less than that clock
//! disagreement can land BOTH sides in the SAME window (by the exact test), hiding the crossing —
//! confirmed live in run 30637408198: one of six genuine counter-instance switches landed its
//! frame's `gen_ts_ns` 30 microseconds on the OLD side of its boundary, and was wrongly charged as
//! a `real_drop`, while the other five (13.6–28.3 ms on the correct side) were suppressed fine.
//!
//! [`near_any_boundary`] answers a narrower, SYMMETRIC question instead of re-deriving a window
//! index: is this frame's `gen_ts_ns` within tolerance of ANY schedule boundary at all? The caller
//! (`probe::burn_contiguity`) uses this as an ADDITIONAL confirmation signal alongside the existing
//! exact-window-index check — never as a replacement for it, and never when either side's window
//! is unknown (that invariant is unaffected by this module; it stays entirely the caller's job).
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as `recording_span_gate.rs` / `phase_sync.rs` / `colour_scale.rs`: the whole
//! `probe` module is `#[cfg(feature = "probe")]` (CLAUDE.md's Local Build Policy — the probe deps
//! balloon the shared dev1 `target/`), so a change confined to `probe::burn_contiguity` has ZERO
//! local verification path, not even a compile check. This module is the PURE decision seam;
//! `probe::burn_contiguity::burn_contiguity_in_window_with_step_and_schedule` only calls it.

/// #903 — the tolerance (ns) within which a frame's `gen_ts_ns` is treated as "near" a
/// `--switch-schedule` boundary instant.
///
/// 200 ms — tied directly to the `#326` dev1<->painter clock-offset gate, the ONLY guarantee the
/// two clocks' relative agreement actually carries (it passes anything under 200 ms; there is no
/// promise of anything finer). Comfortably ABOVE the jitter this run measured (worst case
/// ~28.3 ms) so it recognises every real near-boundary artifact seen so far with margin to spare,
/// and comfortably BELOW the ~30 s spacing between scheduled switches in this rig's harness, so it
/// can never bridge two genuinely separate switches into one.
pub const DEFAULT_BOUNDARY_TOLERANCE_NS: i64 = 200_000_000;

/// True when `gen_ts_ns` lies within `tolerance_ns` of ANY of `boundaries` (each entry is one
/// `--switch-schedule` window's `start_ns` or `end_ns` instant — the caller supplies the flat
/// list). A negative `tolerance_ns` is floored to 0, never a negative tolerance — mirrors
/// `probe::recording_segments::place_frame_in_window`'s own `guard_ns.max(0)`. An empty
/// `boundaries` slice is never near anything.
pub fn near_any_boundary(gen_ts_ns: i64, boundaries: &[i64], tolerance_ns: i64) -> bool {
    let tolerance_ns = tolerance_ns.max(0);
    boundaries
        .iter()
        .any(|&b| (gen_ts_ns - b).abs() <= tolerance_ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    // This run's real boundary spacing (~30s) and the exact six offsets from #903's own table
    // (run 30637408198): one wrongly-charged jump 30us BEFORE its boundary, five correctly-
    // suppressed jumps 13.6-28.3ms AFTER theirs.
    const BOUNDARIES_NS: [i64; 6] = [
        30_000_000_000,
        60_000_000_000,
        90_000_000_000,
        120_000_000_000,
        150_000_000_000,
        180_000_000_000,
    ];

    #[test]
    fn charged_jump_30us_before_boundary_is_now_recognised_near_903() {
        // THE #903 bug: this run's ONE incorrectly-charged real_drop landed 30 MICROSECONDS
        // before its nearest schedule boundary -- inside a defensible tolerance, but strictly on
        // the "wrong" (old) side of an exact-instant test.
        let boundary = BOUNDARIES_NS[0];
        let gen_ts_ns = boundary - 30_000; // 0.030 ms = 30_000 ns before
        assert!(
            near_any_boundary(gen_ts_ns, &BOUNDARIES_NS, DEFAULT_BOUNDARY_TOLERANCE_NS),
            "a jump 30us before a scheduled boundary must be recognised as near it (#903)"
        );
    }

    #[test]
    fn already_suppressed_jumps_after_their_boundary_are_also_recognised_near_903() {
        // The other five backward jumps in this run already suppressed correctly via the exact
        // #708/#741 check (their gen_ts landed AFTER the boundary). Confirm the tolerant signal
        // recognises them too -- this run's real offsets -- so the new mechanism is consistent
        // with the already-working cases, not just the one that was broken.
        for (idx, offset_ms) in [
            (1usize, 13.599_f64),
            (2, 24.841),
            (3, 15.951),
            (4, 28.264),
            (5, 15.336),
        ] {
            let boundary = BOUNDARIES_NS[idx];
            let gen_ts_ns = boundary + (offset_ms * 1_000_000.0) as i64;
            assert!(
                near_any_boundary(gen_ts_ns, &BOUNDARIES_NS, DEFAULT_BOUNDARY_TOLERANCE_NS),
                "boundary {boundary} + {offset_ms}ms must read as near it (#903)"
            );
        }
    }

    #[test]
    fn a_jump_far_from_every_boundary_is_never_near_903() {
        // The hard constraint: a genuine fault occurring deep inside a window (far from any
        // scheduled switch) must NOT be recognised as near a boundary -- it must still be
        // charged as a real drop. 15s away from the nearest boundary is far outside 200ms.
        let gen_ts_ns = BOUNDARIES_NS[0] + 15_000_000_000; // 15s after the first boundary
        assert!(
            !near_any_boundary(gen_ts_ns, &BOUNDARIES_NS, DEFAULT_BOUNDARY_TOLERANCE_NS),
            "a jump far from every boundary must NOT read as near one (#903)"
        );
    }

    #[test]
    fn exactly_at_the_tolerance_edge_is_inclusive_one_ns_past_is_not() {
        let at_edge = BOUNDARIES_NS[0] + DEFAULT_BOUNDARY_TOLERANCE_NS;
        assert!(
            near_any_boundary(at_edge, &BOUNDARIES_NS, DEFAULT_BOUNDARY_TOLERANCE_NS),
            "exactly at the tolerance edge must still count as near"
        );
        let past_edge = at_edge + 1;
        assert!(
            !near_any_boundary(past_edge, &BOUNDARIES_NS, DEFAULT_BOUNDARY_TOLERANCE_NS),
            "one ns past the tolerance edge must NOT count as near"
        );
    }

    #[test]
    fn negative_tolerance_is_floored_to_zero() {
        // A negative tolerance must never widen into "always near" -- floored to 0 (exact match
        // only), mirroring place_frame_in_window's own guard_ns.max(0).
        let one_ns_off = BOUNDARIES_NS[0] + 1;
        assert!(
            !near_any_boundary(one_ns_off, &BOUNDARIES_NS, -100),
            "a negative tolerance must floor to 0, not become permissive"
        );
        assert!(
            near_any_boundary(BOUNDARIES_NS[0], &BOUNDARIES_NS, -100),
            "an EXACT match must still be near even at a floored-to-0 tolerance"
        );
    }

    #[test]
    fn empty_boundaries_is_never_near() {
        assert!(!near_any_boundary(123, &[], DEFAULT_BOUNDARY_TOLERANCE_NS));
    }
}
