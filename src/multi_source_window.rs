//! Issue 1367 -- a cambox window whose captured content is MULTI-SOURCE is judged by its node burn,
//! not by single-feed test-pattern continuity. PURE, Tier-0 (default features, no probe/rig).
//!
//! ## Why
//!
//! Every per-window test-pattern check in the all-cambox sweep (copies/gaps, the cadence gates,
//! `frozen_leg`) assumes the window carries ONE painted feed: the cam2 dual-QR Vernier, sampled once
//! per recorded frame. cam2 now films the strih-lx vk-direct MULTIVIEW (finding
//! 5843418156 on issue 1367): one captured frame holds 1-5 generations of the painted pattern
//! (tiles ~248 / 413 / 546 ms old, incl. cam2's own recursion), and the multiview renders ~30 fps
//! off the program grid. `RecordingFrame::tick` takes the highest id per frame, so a missed newest
//! tile falls back to an older one -- balanced copies/gaps and a `frozen_leg`, while the cam2 leg
//! itself delivered every frame once (its node burn: zero loss, 1811 distinct ids / 1812 frames,
//! max hold 2 <= 4).
//!
//! ## The decision (ROZHODNUTÉ 5843424054, design 5843426470)
//!
//! A window is MULTI-SOURCE when the tear detector's EXISTING per-window multi-path fraction
//! ([`crate::tear_detect::TearStats::multi_path_suspect_fraction`], the share of frames carrying
//! more optical QRs than one tile can produce) exceeds the single-sourced
//! [`crate::tear_detect::MULTI_PATH_SUSPECT_CEILING`] (0.10). That same ceiling already makes the
//! tear gate call such a window unscoreable. For a multi-source window:
//!
//! - **REPORT-ONLY** (still computed, printed and in the verdict JSON): copies/gaps, the cadence
//!   gates (judder, uniformity, duplication-masked) and `frozen_leg`.
//! - **BLOCKING, unchanged**: the node-burn contiguity + max-hold folds (per node, over the whole
//!   recording -- never scoped by this module), the window's presence (`frames > 0`) and its optical
//!   undecodable floor, the tear gate, self-heal events.
//!
//! Every verdict names such a window with [`MULTI_SOURCE_TAG`] and its fraction. A single-source
//! window ([`WindowCheckScope::Full`]) is byte-identical in behaviour: every check still gates.
//!
//! Fail-closed: a NaN fraction, or a window with no scope at all (a length mismatch), reads
//! SINGLE-source, so nothing is relaxed that was not measured multi-source.
//!
//! Known limit (accepted in the decision): a repeat introduced on the HDMI/capture side that still
//! carries a fresh cambox burn is invisible while the window's content is multi-source. The planned
//! re-tightening is a run-scoped single-camera HDMI view (option 2 on issue 1367).

use serde::Serialize;

use crate::frozen_leg::FrozenLeg;
use crate::tear_detect::MULTI_PATH_SUSPECT_CEILING;

/// The fixed tag every verdict rendering (stdout, verdict JSON, both Discord report renderings)
/// uses to name a multi-source window.
pub const MULTI_SOURCE_TAG: &str = "multi-source (report-only by #1367 decision)";

/// The per-window test-pattern checks that fold REPORT-ONLY in a multi-source window.
pub const REPORT_ONLY_CHECKS: [&str; 3] = ["copies_gaps", "cadence", "frozen_leg"];

/// The checks that stay BLOCKING in a multi-source window: the node burn judges it.
pub const BLOCKING_CHECKS: [&str; 2] = ["node_burn_contiguity", "node_burn_hold"];

/// Is this window's captured content multi-source? `multi_path_suspect_fraction` is the window's
/// [`crate::tear_detect::TearStats::multi_path_suspect_fraction`]. Strictly ABOVE the ceiling, so a
/// window sitting exactly on it stays single-source; NaN reads single-source (fail-closed).
pub fn window_is_multi_source(multi_path_suspect_fraction: f64) -> bool {
    multi_path_suspect_fraction > MULTI_PATH_SUSPECT_CEILING
}

/// Which per-window checks gate a window's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCheckScope {
    /// Every per-window check gates -- every single-source window (the pre-1367 behaviour).
    Full,
    /// The window films multi-source content: its test-pattern checks are report-only and it is
    /// judged by its node burn.
    MultiSource,
}

impl WindowCheckScope {
    /// Whether the test-pattern checks ([`REPORT_ONLY_CHECKS`]) gate this window.
    pub fn test_pattern_checks_gate(self) -> bool {
        self == WindowCheckScope::Full
    }
}

/// The per-window check-scope decision from the window's multi-path fraction.
pub fn window_check_scope(multi_path_suspect_fraction: f64) -> WindowCheckScope {
    if window_is_multi_source(multi_path_suspect_fraction) {
        WindowCheckScope::MultiSource
    } else {
        WindowCheckScope::Full
    }
}

/// The scope of window `wi`, fail-closed to [`WindowCheckScope::Full`] when `scopes` has no entry.
pub fn scope_at(scopes: &[WindowCheckScope], wi: usize) -> WindowCheckScope {
    scopes.get(wi).copied().unwrap_or(WindowCheckScope::Full)
}

/// The machine-readable tag a multi-source window carries in the verdict JSON
/// (`all_cambox_continuity.segments[].multi_source`, `frozen_leg.multi_source_report_only[]`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultiSourceTag {
    /// Always [`MULTI_SOURCE_TAG`].
    pub tag: &'static str,
    /// The window's measured multi-path fraction.
    pub multi_path_suspect_fraction: f64,
    /// The ceiling it exceeded ([`MULTI_PATH_SUSPECT_CEILING`]).
    pub ceiling: f64,
    /// [`REPORT_ONLY_CHECKS`].
    pub report_only_checks: [&'static str; 3],
    /// [`BLOCKING_CHECKS`].
    pub blocking_checks: [&'static str; 2],
}

/// `Some(tag)` for a multi-source window, `None` for a single-source one (so a single-source
/// window's JSON carries no new key at all).
pub fn multi_source_tag(multi_path_suspect_fraction: f64) -> Option<MultiSourceTag> {
    window_is_multi_source(multi_path_suspect_fraction).then_some(MultiSourceTag {
        tag: MULTI_SOURCE_TAG,
        multi_path_suspect_fraction,
        ceiling: MULTI_PATH_SUSPECT_CEILING,
        report_only_checks: REPORT_ONLY_CHECKS,
        blocking_checks: BLOCKING_CHECKS,
    })
}

/// The one-line human form every stdout/report line uses:
/// `CAM2 multi-source (report-only by #1367 decision), fraction 0.43 > 0.10`.
pub fn tag_line(cambox: &str, multi_path_suspect_fraction: f64) -> String {
    format!(
        "{cambox} {MULTI_SOURCE_TAG}, fraction {multi_path_suspect_fraction:.2} > \
         {MULTI_PATH_SUSPECT_CEILING:.2}"
    )
}

/// The window's continuity term folded into `all_cambox_continuity.overall_pass`: the SAME
/// [`crate::window_gate::decide_with_tolerance`] `overall_pass_term` every window uses, with the
/// copies/gaps term dropped for a multi-source window. Presence (`frames > 0`) and the optical
/// undecodable floor still gate either way. For [`WindowCheckScope::Full`] this IS the pre-1367
/// term, byte-for-byte.
pub fn scoped_continuity_term(
    scope: WindowCheckScope,
    frames: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
    tolerance: u32,
) -> bool {
    let (copies, gaps) = if scope.test_pattern_checks_gate() {
        (copies, gaps)
    } else {
        (0, 0)
    };
    crate::window_gate::decide_with_tolerance(frames, undecodable, copies, gaps, tolerance)
        .overall_pass_term
}

/// Split per-window items into the ones whose test-pattern checks gate: `items[i]` belongs to
/// window `i`. Used for the run-level cadence worsts, which must only fold gating windows.
pub fn gating_items<'a, T>(
    items: &'a [T],
    scopes: &'a [WindowCheckScope],
) -> impl Iterator<Item = &'a T> + 'a {
    items
        .iter()
        .enumerate()
        .filter(move |(wi, _)| scope_at(scopes, *wi).test_pattern_checks_gate())
        .map(|(_, item)| item)
}

/// One schedule window's identity for [`partition_frozen_legs`]: a [`FrozenLeg`] carries its
/// window's `cambox` and `since_ns == start_ns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowKey<'a> {
    pub cambox: &'a str,
    pub start_ns: i64,
    pub scope: WindowCheckScope,
}

/// Split the hard-frozen windows into `(gating, report_only)`. A frozen entry whose window is
/// multi-source is report-only; every other entry -- including one that matches no window --
/// stays gating (fail-closed). Order is preserved within each list.
pub fn partition_frozen_legs(
    frozen: Vec<FrozenLeg>,
    windows: &[WindowKey<'_>],
) -> (Vec<FrozenLeg>, Vec<FrozenLeg>) {
    frozen.into_iter().partition(|f| {
        !windows.iter().any(|w| {
            w.scope == WindowCheckScope::MultiSource
                && w.cambox == f.cambox
                && w.start_ns == f.since_ns
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen(cambox: &str, since_ns: i64) -> FrozenLeg {
        FrozenLeg {
            cambox: cambox.to_string(),
            since_ns,
            copies: 88,
            approx_stale_secs: 3.1,
            density: 0.104,
        }
    }

    #[test]
    fn the_real_cam2_multiview_fractions_are_multi_source_1367() {
        // Run 2059624745 (0.4255, 0.6028) and run 386740541 (0.4912, 0.5896): the four CAM2
        // windows that film the strih-lx multiview.
        for f in [
            0.425531914893617,
            0.6028368794326241,
            0.4911660777385159,
            0.589622641509434,
        ] {
            assert!(window_is_multi_source(f), "{f} must read multi-source");
            assert_eq!(window_check_scope(f), WindowCheckScope::MultiSource);
        }
    }

    #[test]
    fn single_source_and_boundary_fractions_stay_full_1367() {
        for f in [0.0, 0.05, MULTI_PATH_SUSPECT_CEILING, f64::NAN] {
            assert!(!window_is_multi_source(f), "{f} must stay single-source");
            assert_eq!(window_check_scope(f), WindowCheckScope::Full);
        }
        assert!(window_is_multi_source(MULTI_PATH_SUSPECT_CEILING + 1e-9));
    }

    #[test]
    fn scope_gates_only_a_full_window_and_a_missing_scope_is_full_1367() {
        assert!(WindowCheckScope::Full.test_pattern_checks_gate());
        assert!(!WindowCheckScope::MultiSource.test_pattern_checks_gate());
        assert_eq!(scope_at(&[], 3), WindowCheckScope::Full);
        assert_eq!(
            scope_at(&[WindowCheckScope::MultiSource], 0),
            WindowCheckScope::MultiSource
        );
    }

    #[test]
    fn the_tag_is_absent_on_single_source_and_carries_the_fraction_1367() {
        assert_eq!(multi_source_tag(0.0), None);
        let t = multi_source_tag(0.6).expect("multi-source tag");
        assert_eq!(t.tag, "multi-source (report-only by #1367 decision)");
        assert_eq!(t.multi_path_suspect_fraction, 0.6);
        assert_eq!(t.ceiling, MULTI_PATH_SUSPECT_CEILING);
        assert_eq!(
            t.report_only_checks,
            ["copies_gaps", "cadence", "frozen_leg"]
        );
        assert_eq!(
            t.blocking_checks,
            ["node_burn_contiguity", "node_burn_hold"]
        );
        assert_eq!(
            tag_line("CAM2", 0.425531914893617),
            "CAM2 multi-source (report-only by #1367 decision), fraction 0.43 > 0.10"
        );
    }

    #[test]
    fn a_multi_source_window_drops_only_its_copies_gaps_term_1367() {
        // The real run-2 CAM2 window: frames 846, undecodable 2, copies 88, gaps 97, tolerance 2.
        let ms = WindowCheckScope::MultiSource;
        assert!(scoped_continuity_term(ms, 846, 2, 88, 97, 2));
        // The SAME counts on a single-source window still fail (the pre-1367 term).
        assert!(!scoped_continuity_term(
            WindowCheckScope::Full,
            846,
            2,
            88,
            97,
            2
        ));
        // Presence and the optical floor still gate a multi-source window.
        assert!(!scoped_continuity_term(ms, 0, 0, 0, 0, 2));
        assert!(!scoped_continuity_term(ms, 846, 500, 88, 97, 2));
    }

    #[test]
    fn a_full_window_is_byte_identical_to_the_window_gate_term_1367() {
        for (frames, undec, copies, gaps, tol) in [
            (847u32, 0u32, 0u32, 0u32, 2u32),
            (844, 0, 1, 1, 2),
            (846, 2, 88, 97, 2),
            (0, 0, 0, 0, 2),
            (846, 9, 0, 0, 2),
            (846, 0, 3, 0, 5),
        ] {
            assert_eq!(
                scoped_continuity_term(WindowCheckScope::Full, frames, undec, copies, gaps, tol),
                crate::window_gate::decide_with_tolerance(frames, undec, copies, gaps, tol)
                    .overall_pass_term,
                "({frames},{undec},{copies},{gaps},{tol})"
            );
        }
    }

    #[test]
    fn gating_items_skip_multi_source_windows_and_keep_unscoped_ones_1367() {
        let items = [0.0, 0.32, 0.0, 0.5];
        let scopes = [
            WindowCheckScope::Full,
            WindowCheckScope::MultiSource,
            WindowCheckScope::Full,
        ];
        let kept: Vec<f64> = gating_items(&items, &scopes).copied().collect();
        // window 3 has no scope: fail-closed, it gates.
        assert_eq!(kept, vec![0.0, 0.0, 0.5]);
    }

    #[test]
    fn frozen_legs_of_multi_source_windows_are_report_only_1367() {
        let windows = [
            WindowKey {
                cambox: "CAM2",
                start_ns: 100,
                scope: WindowCheckScope::MultiSource,
            },
            WindowKey {
                cambox: "CAM3",
                start_ns: 200,
                scope: WindowCheckScope::Full,
            },
        ];
        let (gating, report_only) = partition_frozen_legs(
            vec![
                frozen("CAM2", 100),
                frozen("CAM3", 200),
                frozen("CAM2", 999),
            ],
            &windows,
        );
        assert_eq!(report_only, vec![frozen("CAM2", 100)]);
        // CAM3 is single-source; the unmatched CAM2 entry fails closed.
        assert_eq!(gating, vec![frozen("CAM3", 200), frozen("CAM2", 999)]);
    }
}
