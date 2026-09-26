//! Issue 1367 -- the multi-source window decision against the REAL windows of release PR 1373's two
//! E2E runs (`tests/fixtures/multi-source-1367/cam_windows.tsv`, copied from the dev1 verdict JSONs).
//!
//! - The four CAM2 windows that film the strih-lx multiview read multi-source, and their copies/gaps,
//!   cadence and `frozen_leg` fold report-only, so they pass on their node burn.
//! - Every other window stays single-source and byte-identical in behaviour; a CAM3-style
//!   single-source window carrying copies/gaps still FAILS.
//! - The node-burn contiguity + hold folds in `recording-verdict.rs` stay unconditional, so a
//!   multi-source window with a burn gap still FAILS (the probe-gated end-to-end twin is
//!   `multi_source_window_is_judged_by_its_node_burn_1367` in `src/bin/recording-verdict.rs`).

use camera_box::frozen_leg::FrozenLeg;
use camera_box::multi_source_window::{
    gating_items, multi_source_tag, partition_frozen_legs, scoped_continuity_term,
    window_check_scope, window_is_multi_source, WindowCheckScope, WindowKey, MULTI_SOURCE_TAG,
};
use camera_box::presentation_cadence::{
    cadence_judder_gate_pass, cadence_uniformity_gate_pass, PAIRED_FRACTION_JUDDER_MAX,
    UNIFORM_FRACTION_MIN,
};

const FIXTURE: &str = include_str!("fixtures/multi-source-1367/cam_windows.tsv");

#[derive(Debug, Clone)]
struct Window {
    run: String,
    cambox: String,
    start_ns: i64,
    frames: u32,
    undecodable: u32,
    copies: u32,
    gaps: u32,
    tolerance: u32,
    fraction: f64,
    paired: f64,
    beat_uniform: f64,
}

fn load() -> (Vec<Window>, Vec<(String, FrozenLeg)>) {
    let mut windows = Vec::new();
    let mut frozen = Vec::new();
    for line in FIXTURE.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split('\t').collect();
        match c[0] {
            "window" => windows.push(Window {
                run: c[1].to_string(),
                cambox: c[2].to_string(),
                start_ns: c[3].parse().unwrap(),
                frames: c[5].parse().unwrap(),
                undecodable: c[6].parse().unwrap(),
                copies: c[7].parse().unwrap(),
                gaps: c[8].parse().unwrap(),
                tolerance: c[9].parse().unwrap(),
                fraction: c[10].parse().unwrap(),
                paired: c[11].parse().unwrap(),
                beat_uniform: c[12].parse().unwrap(),
            }),
            "frozen" => frozen.push((
                c[1].to_string(),
                FrozenLeg {
                    cambox: c[2].to_string(),
                    since_ns: c[3].parse().unwrap(),
                    copies: c[4].parse().unwrap(),
                    approx_stale_secs: 0.0,
                    density: 0.0,
                },
            )),
            other => panic!("unknown fixture row kind {other:?}"),
        }
    }
    (windows, frozen)
}

fn runs(windows: &[Window]) -> Vec<String> {
    let mut r: Vec<String> = windows.iter().map(|w| w.run.clone()).collect();
    r.dedup();
    r
}

#[test]
fn fixture_holds_both_runs_with_two_cam2_windows_each_1367() {
    let (windows, frozen) = load();
    assert_eq!(runs(&windows), vec!["386740541", "2059624745"]);
    for run in runs(&windows) {
        let n = windows
            .iter()
            .filter(|w| w.run == run && w.cambox == "CAM2")
            .count();
        assert_eq!(n, 2, "run {run}");
    }
    assert_eq!(frozen.len(), 3);
}

#[test]
fn the_real_cam2_windows_are_multi_source_and_pass_their_continuity_term_1367() {
    let (windows, _) = load();
    for w in windows.iter().filter(|w| w.cambox == "CAM2") {
        assert!(window_is_multi_source(w.fraction), "{w:?}");
        let scope = window_check_scope(w.fraction);
        assert_eq!(scope, WindowCheckScope::MultiSource, "{w:?}");
        assert!(
            scoped_continuity_term(
                scope,
                w.frames,
                w.undecodable,
                w.copies,
                w.gaps,
                w.tolerance
            ),
            "a multi-source CAM2 window is judged by its burn, not its copies/gaps: {w:?}"
        );
        // The pre-1367 single-feed term fails the very same window.
        assert!(
            !scoped_continuity_term(
                WindowCheckScope::Full,
                w.frames,
                w.undecodable,
                w.copies,
                w.gaps,
                w.tolerance
            ),
            "{w:?}"
        );
        let tag = multi_source_tag(w.fraction).expect("tag");
        assert_eq!(tag.tag, MULTI_SOURCE_TAG);
        assert_eq!(tag.multi_path_suspect_fraction, w.fraction);
    }
}

#[test]
fn every_other_real_window_is_single_source_and_unchanged_1367() {
    let (windows, _) = load();
    for w in windows.iter().filter(|w| w.cambox != "CAM2") {
        assert_eq!(
            window_check_scope(w.fraction),
            WindowCheckScope::Full,
            "{w:?}"
        );
        assert!(multi_source_tag(w.fraction).is_none(), "{w:?}");
        assert_eq!(
            scoped_continuity_term(
                WindowCheckScope::Full,
                w.frames,
                w.undecodable,
                w.copies,
                w.gaps,
                w.tolerance
            ),
            camera_box::window_gate::decide_with_tolerance(
                w.frames,
                w.undecodable,
                w.copies,
                w.gaps,
                w.tolerance
            )
            .overall_pass_term,
            "{w:?}"
        );
    }
}

#[test]
fn a_cam3_style_single_source_window_with_copies_gaps_still_fails_1367() {
    let (windows, _) = load();
    let cam2 = windows
        .iter()
        .find(|w| w.run == "2059624745" && w.cambox == "CAM2")
        .unwrap();
    let cam3 = windows
        .iter()
        .find(|w| w.run == "2059624745" && w.cambox == "CAM3")
        .unwrap();
    // CAM3's real single-source window, carrying CAM2's copies/gaps.
    let scope = window_check_scope(cam3.fraction);
    assert_eq!(scope, WindowCheckScope::Full);
    assert!(!scoped_continuity_term(
        scope,
        cam3.frames,
        cam3.undecodable,
        cam2.copies,
        cam2.gaps,
        cam3.tolerance
    ));
}

#[test]
fn the_cadence_gates_fold_only_single_source_windows_1367() {
    let (windows, _) = load();
    for run in runs(&windows) {
        let ws: Vec<&Window> = windows.iter().filter(|w| w.run == run).collect();
        let scopes: Vec<WindowCheckScope> =
            ws.iter().map(|w| window_check_scope(w.fraction)).collect();
        let paired: Vec<f64> = ws.iter().map(|w| w.paired).collect();
        let uniform: Vec<f64> = ws.iter().map(|w| w.beat_uniform).collect();

        let worst_all_paired = paired
            .iter()
            .copied()
            .fold(None::<f64>, |a, p| Some(a.map_or(p, |m| m.max(p))));
        let worst_gated_paired = gating_items(&paired, &scopes)
            .copied()
            .fold(None::<f64>, |a, p| Some(a.map_or(p, |m| m.max(p))));
        let worst_all_uniform = uniform
            .iter()
            .copied()
            .fold(None::<f64>, |a, u| Some(a.map_or(u, |m| m.min(u))));
        let worst_gated_uniform = gating_items(&uniform, &scopes)
            .copied()
            .fold(None::<f64>, |a, u| Some(a.map_or(u, |m| m.min(u))));

        // Pre-1367: the multiview windows fail both cadence gates.
        assert!(
            !cadence_judder_gate_pass(worst_all_paired, Some(PAIRED_FRACTION_JUDDER_MAX))
                || !cadence_uniformity_gate_pass(worst_all_uniform, Some(UNIFORM_FRACTION_MIN)),
            "run {run}"
        );
        // 1367: only single-source windows fold, and they are clean.
        assert!(
            cadence_judder_gate_pass(worst_gated_paired, Some(PAIRED_FRACTION_JUDDER_MAX)),
            "run {run}: {worst_gated_paired:?}"
        );
        assert!(
            cadence_uniformity_gate_pass(worst_gated_uniform, Some(UNIFORM_FRACTION_MIN)),
            "run {run}: {worst_gated_uniform:?}"
        );
    }
}

#[test]
fn the_real_cam2_frozen_legs_are_report_only_and_a_single_source_one_still_gates_1367() {
    let (windows, frozen) = load();
    for run in runs(&windows) {
        let keys: Vec<WindowKey<'_>> = windows
            .iter()
            .filter(|w| w.run == run)
            .map(|w| WindowKey {
                cambox: &w.cambox,
                start_ns: w.start_ns,
                scope: window_check_scope(w.fraction),
            })
            .collect();
        let real: Vec<FrozenLeg> = frozen
            .iter()
            .filter(|(r, _)| *r == run)
            .map(|(_, f)| f.clone())
            .collect();
        let n_real = real.len();
        assert!(n_real > 0, "run {run}");
        let (gating, report_only) = partition_frozen_legs(real, &keys);
        assert!(gating.is_empty(), "run {run}: {gating:?}");
        assert_eq!(report_only.len(), n_real, "run {run}");

        // A CAM3 window of the same run that froze still gates.
        let cam3 = windows
            .iter()
            .find(|w| w.run == run && w.cambox == "CAM3")
            .unwrap();
        let cam3_frozen = FrozenLeg {
            cambox: "CAM3".to_string(),
            since_ns: cam3.start_ns,
            copies: 100,
            approx_stale_secs: 3.3,
            density: 0.12,
        };
        let (gating, report_only) = partition_frozen_legs(vec![cam3_frozen.clone()], &keys);
        assert_eq!(gating, vec![cam3_frozen]);
        assert!(report_only.is_empty());
    }
}

/// The node-burn folds are per node over the whole recording and never consult the window scope,
/// so a burn gap inside a multi-source window still fails the run. Pinned on the verdict source
/// (probe-gated, CI-only to compile) so a future edit cannot quietly scope them.
#[test]
fn the_node_burn_folds_stay_unconditional_in_the_verdict_1367() {
    let src = include_str!("../src/bin/recording-verdict.rs");
    for fold in [
        "all_pass &= nv.is_zero_within_allowance(real_drops_allowance) && span_ok;",
        "all_pass &= hold_within || !camera_box::burn_hold::gates_overall_pass();",
    ] {
        assert_eq!(src.matches(fold).count(), 1, "{fold}");
    }
    // The continuity fold reads the scoped per-window term, and the scope comes from the tear
    // detector's multi-path fraction.
    assert!(src.contains("segment_continuity_scoped("));
    assert!(src.contains("camera_box::multi_source_window::window_check_scope("));
    assert!(src.contains("camera_box::multi_source_window::partition_frozen_legs("));
}
