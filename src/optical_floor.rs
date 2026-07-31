//! #881 (via #854/#707) — the TEMPORARY calibrated floor for the all-cambox segment continuity's
//! optical `undecodable` term.
//!
//! ## Why this exists
//!
//! The all-cambox painted-tick continuity check (`probe::recording_segments::window_segment`)
//! decodes a dual-QR Vernier off a 60Hz monitor filmed by the test camera (cam2's optical
//! injection leg). Issue #854 traced the residual `undecodable` rate that survives after every
//! real chain-loss bug was fixed (the presenter's silent KMS→fbdev fallback, the Vernier's
//! unstable "settled" half) down to a **temporal tear**: the camera's exposure window
//! occasionally straddles two 60Hz painter refreshes, so one recorded frame carries two
//! internally-valid QR codewords in a single grid. Reed-Solomon (already at `EcLevel::H`, the
//! maximum, in both render paths — `probe::qr` and `vendor/distroav/src/burn-qr.hpp`) cannot
//! reconcile two valid halves; there is no error-correction headroom left to spend. This is a
//! property of the 60Hz panel the test camera films, NOT of the cam→strih→stream delivery chain
//! under test — measured at a stable 0.023-0.035% across independent runs (issue 854 comments
//! 5128278951 / 5128509160), with `copies == 0` and `gaps == 0` on every window of the primary
//! calibration run (`1039420389`): zero real chain loss, only the optical read misses a handful
//! of ticks.
//!
//! The user's direction (issue 854, 2026-07-30): raise the optical term's threshold NOW so the
//! gate can go green and unblock the backlog, and file **#881** (connect cam2's monitor to a
//! 120Hz panel — halving the redraw time roughly halves the tear window, and the dual-QR
//! Vernier's design already guarantees at least one half is painted wholly within a single
//! refresh at that rate) as the ticket that restores the term to an absolute `undecodable == 0`
//! and deletes this whole module. **This floor is temporary by construction** — every constant
//! and test here is commented with #881 as what removes it.
//!
//! **Not relaxed, now or ever, and untouched by this module:** `copies == 0`, `gaps == 0`, and
//! `frame_count > 0` at the call site (`probe::recording_segments`). Those measure the chain
//! under test; only the test instrument's own optical read gets a floor.
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! The whole `probe` module is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/`drm`, which
//! balloon the shared dev1 `target/` — CLAUDE.md's Local Build Policy). This module is the PURE
//! decision seam — the same pattern as `src/reannounce.rs`, `src/colour_scale.rs`, and
//! `src/presentation_cadence.rs`: no probe deps, so it unit-tests Tier-0 (default features, no
//! framebuffer, no QR decode). `probe::recording_segments::window_segment` /
//! `segment_continuity` only CALL these two functions; they never re-derive the thresholds.
//!
//! ## Two terms, not one
//!
//! A per-window allowance alone is too weak: 10 windows x an allowance of 4 would tolerate 40
//! undecodable frames in one run — MORE than the pre-#707 regression level (10) this whole gate
//! was written to catch. So the floor has both a per-window AND a run-wide (summed across every
//! window) term; a gate that would pass the bug it was written after is not a gate.

/// Per-window optical `undecodable` allowance. The observed max on a single window across the
/// calibration runs is 2 (issue 854 comment 5128509160's table); 4 is 2x that headroom. Treating
/// the tear as binomial with n≈846 (a typical ~30s window's frame count) and p≈3.5e-4 (the
/// measured rate), `P(X>=4) ≈ 0.03%` per window — the floor costs roughly one spurious red per
/// ~300 runs while still catching the physical rate doubling.
///
/// TEMPORARY — deleted together with this module when **#881** (connect cam2's 120Hz monitor,
/// restore the optical term to absolute zero) lands.
pub const PER_WINDOW_UNDECODABLE_FLOOR: u32 = 4;

/// Run-wide (summed across every window in a recording) optical `undecodable` allowance. The
/// calibration run (`1039420389`) measured 3 total; 8 is 2.7x that headroom while staying BELOW
/// the pre-#707 regression level (10 undecodable across 10 windows) — the load-bearing half of
/// the floor: a per-window-only check would let 10 windows at 1-each through untouched.
///
/// TEMPORARY — deleted together with this module when **#881** (connect cam2's 120Hz monitor,
/// restore the optical term to absolute zero) lands.
pub const RUN_UNDECODABLE_FLOOR: u32 = 8;

/// Is this ONE window's optical `undecodable` count within the #881 calibrated floor?
///
/// `frame_count` is checked defensively (a zero-frame window is never "within floor" regardless
/// of `undecodable`, matching the call site's own `frame_count > 0` requirement) even though the
/// call site also asserts it independently — this function is correct in isolation, not only in
/// combination with its caller.
pub fn window_within_floor(undecodable: u32, frame_count: u32) -> bool {
    frame_count > 0 && undecodable <= PER_WINDOW_UNDECODABLE_FLOOR
}

/// Is the WHOLE run's summed optical `undecodable` count (across every window) within the #881
/// calibrated floor? This is the term that catches a localized-but-widespread degradation (many
/// windows each individually within [`window_within_floor`]) that the per-window term alone
/// cannot see — see the module doc's "Two terms, not one".
pub fn run_within_floor(total_undecodable: u32) -> bool {
    total_undecodable <= RUN_UNDECODABLE_FLOOR
}

/// #915 (2026-08-01, user decision -- mirrors the issue-914 pattern for `SelfHealAttributionReport
/// ::overall_pass_contribution`): whether [`window_within_floor`]/[`run_within_floor`] fold into
/// the fused verdict's relaxed/overall pass. Both functions above are UNCHANGED — still fully
/// computed and reported (feeding the STRICT `CamboxSegment::pass` field byte-for-byte); only the
/// CALLERS (`crate::window_gate::decide` for the per-window term, `probe::recording_segments::
/// segment_continuity` for the run-wide term) stop folding their result into the relaxed/overall
/// verdict when this returns `false`.
///
/// STUB (pre-915 behavior, kept strict on purpose): this is the RED commit — the body still
/// returns the OLD strict decision (`true`, i.e. the floor still gates) so the new test below
/// (which expects the RELAXED, always-`false` behavior) fails. The GREEN commit flips the body to
/// a hardcoded `false`.
pub fn gates_overall_pass() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- window_within_floor -------------------------------------------------------------

    #[test]
    fn real_run_1039420389_every_window_within_per_window_floor() {
        // issue 854 comment 5128509160's table: undecodable 0,0,0,0,0,1,0,0,0,2 across 10
        // windows, frame counts 846,846,847,847,847,847,848,846,846,844. Every one must be
        // within the per-window floor on its own.
        let undecodable = [0u32, 0, 0, 0, 0, 1, 0, 0, 0, 2];
        let frame_counts = [846u32, 846, 847, 847, 847, 847, 848, 846, 846, 844];
        for (u, f) in undecodable.iter().zip(frame_counts.iter()) {
            assert!(
                window_within_floor(*u, *f),
                "undecodable={u} frame_count={f} must be within the per-window floor"
            );
        }
    }

    #[test]
    fn real_run_1039420389_sum_within_run_wide_floor() {
        let undecodable = [0u32, 0, 0, 0, 0, 1, 0, 0, 0, 2];
        let total: u32 = undecodable.iter().sum();
        assert_eq!(total, 3, "sanity: the calibration run measured 3 total");
        assert!(run_within_floor(total));
    }

    #[test]
    fn per_window_floor_boundary_four_passes_five_fails() {
        // Acceptance criterion 3 (issue 854 design): a single window at 5 undecodable must FAIL.
        assert!(
            window_within_floor(4, 846),
            "4 is exactly the floor -> within"
        );
        assert!(
            !window_within_floor(5, 846),
            "5 exceeds the per-window floor of 4 -> FAIL"
        );
    }

    #[test]
    fn frame_count_zero_never_within_floor_even_with_zero_undecodable() {
        // Acceptance criterion 5: frame_count == 0 must FAIL, regardless of undecodable.
        assert!(!window_within_floor(0, 0));
    }

    // --- run_within_floor ------------------------------------------------------------------

    #[test]
    fn run_wide_floor_boundary_eight_passes_nine_fails() {
        assert!(
            run_within_floor(8),
            "8 is exactly the run-wide floor -> within"
        );
        assert!(
            !run_within_floor(9),
            "9 exceeds the run-wide floor of 8 -> FAIL"
        );
    }

    #[test]
    fn pre_707_regression_level_fails_the_run_wide_cap_even_though_each_window_alone_passes() {
        // THE most important test (issue 854 design, acceptance criterion 2). #707's own
        // before/after was 10 undecodable (the regression) -> 3 (the fix). 10 spread as 1-per-
        // window across 10 windows would pass the PER-WINDOW term individually (1 <= 4 every
        // time) -- proving the per-window term ALONE is not enough, and that the run-wide cap is
        // the one that actually catches the pre-#707 regression level. A gate that would have
        // passed the bug it was written after is not a gate.
        let per_window_undecodable = 1u32;
        for _ in 0..10 {
            assert!(
                window_within_floor(per_window_undecodable, 846),
                "each individual window (1 undecodable) is within the per-window floor"
            );
        }
        let total = per_window_undecodable * 10;
        assert_eq!(total, 10, "sanity: the pre-#707 regression level");
        assert!(
            !run_within_floor(total),
            "10 total undecodable (the pre-#707 regression level) must FAIL the run-wide cap \
             even though every individual window passed its own per-window floor"
        );
    }

    // --- gates_overall_pass (issue 915) ---------------------------------------------------

    #[test]
    fn gates_overall_pass_is_report_only_915() {
        // #915 (2026-08-01, user decision): cam1's ShadowCast 2 grabber defect (issue 909) trips
        // the run-wide floor on a hardware fault unrelated to any PR's own diff (run 30671860323:
        // 10 undecodable, all in CAM1 windows, CAM2/CAM4 measured 0). The floor's own math
        // (`window_within_floor`/`run_within_floor` above) stays UNCHANGED; only whether it can
        // fail the run changes.
        assert!(
            !gates_overall_pass(),
            "#915: the optical undecodable floor must be report-only while issue 909 (cam1 \
             grabber) + issue 881 (120Hz monitor) remain unresolved"
        );
    }
}
