//! Issue 1482 — a capture card that re-times the frames on its own clock (cam2's Cam Link) catches
//! up in constant phase steps: absorbed as a phase step with the lock kept, never a re-seed.

use super::*;

/// A locked camera whose frames all move by `step_ns` from frame `from` on (a capture card that
/// re-times frames on its own clock and catches up in one step): the step is held for the outlier
/// run, then absorbed with the lock kept, and from the next frame on the prediction sits on the
/// NEW phase.
fn assert_step_absorbed(step_ns: i64, sigma_ns: f64, seed: u64) {
    let mut frames = camera(900, 45.0, sigma_ns, 7_000_000.0, seed);
    for f in frames.iter_mut().skip(300) {
        f.ts = (f.ts as i64 + step_ns) as u64;
        f.truth += step_ns as f64;
    }
    let run_end = 300 + RESEED_CONSECUTIVE_OUTLIERS as usize - 1;
    let mut t = CapturePhaseTracker::new();
    let mut steps_noted = Vec::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if let Some(n) = t.note() {
            steps_noted.push((k, n.clone()));
        }
        if k >= 200 {
            let est = obs
                .smoothed_mono_ns
                .unwrap_or_else(|| panic!("frame {k}: the lock is kept through the step"))
                as f64;
            if k > run_end {
                // The prediction error at the newest frame is ~2 sigma / sqrt(n): 5 x that.
                let bound = (10.0 * sigma_ns / (FIT_WINDOW_FRAMES as f64).sqrt()).max(5_000.0);
                assert!(
                    (est - f.truth).abs() < bound,
                    "frame {k}: the prediction follows the new phase ({:.0} us off)",
                    (est - f.truth) / 1000.0
                );
            }
        }
    }
    assert_eq!(
        t.reseeds(),
        0,
        "step {step_ns}: a constant step is never a re-seed"
    );
    assert_eq!(t.phase_steps(), 1, "step {step_ns}: one step absorbed");
    assert!(t.locked());
    assert_eq!(t.sums, fresh_sums(&t), "the moved window's sums stay exact");
    assert_eq!(steps_noted.len(), 1, "{steps_noted:?}");
    let (k, note) = &steps_noted[0];
    assert_eq!(*k, run_end, "absorbed on the run's last frame");
    let PhaseNote::PhaseStep { step_ns: got } = note else {
        panic!("expected a phase step, got {note:?}");
    };
    assert!(
        ((got - step_ns) as f64).abs() < 50_000.0 + 2.0 * sigma_ns,
        "the noted step {got} is the run's median ~ {step_ns}"
    );
}

#[test]
fn a_card_retiming_step_is_absorbed_with_the_lock_kept_1482() {
    // cam2's own jitter (~3 us) and the 1371 tests' 60 us (the live line reads ~41 us).
    assert_step_absorbed(2_124_000, 3_000.0, 0x1482_0001);
    assert_step_absorbed(2_124_000, 60_000.0, 0x1482_0011);
}

#[test]
fn a_negative_card_retiming_step_is_absorbed_too_1482() {
    assert_step_absorbed(-6_300_000, 3_000.0, 0x1482_0002);
    assert_step_absorbed(-6_300_000, 60_000.0, 0x1482_0012);
}

#[test]
fn a_ramp_outlier_run_still_reseeds_1482() {
    // The run's residuals grow from +1.5 ms by 0.15 ms per frame (a drift, not one offset).
    let mut frames = camera(700, 45.0, 3_000.0, 7_000_000.0, 0x1482_0003);
    for (i, f) in frames.iter_mut().skip(300).enumerate() {
        let d = 1_500_000 + 150_000 * i.min(40) as u64;
        f.ts += d;
    }
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    assert_eq!(t.phase_steps(), 0, "a ramp is never absorbed as a step");
    assert!(t.reseeds() >= 1, "a ramp still re-seeds");
}

#[test]
fn a_scattered_outlier_run_of_thirty_still_reseeds_1482() {
    // Thirty outliers in a row that disagree by ~3.5 ms: scatter, not a constant offset.
    let mut frames = camera(500, 45.0, 3_000.0, 7_000_000.0, 0x1482_0004);
    for (i, f) in frames
        .iter_mut()
        .skip(300)
        .take(RESEED_CONSECUTIVE_OUTLIERS as usize)
        .enumerate()
    {
        f.ts += if i % 2 == 0 { 1_500_000 } else { 5_000_000 };
    }
    let mut t = CapturePhaseTracker::new();
    let got = notes(&mut t, &frames);
    assert_eq!(t.phase_steps(), 0);
    assert_eq!(t.reseeds(), 1);
    assert!(got
        .iter()
        .flatten()
        .any(|n| matches!(n, PhaseNote::OutlierRun { .. })));
}

#[test]
fn a_half_period_step_still_reseeds_not_absorbed_1482() {
    let mut frames = camera(400, 45.0, 3_000.0, 7_000_000.0, 0x1482_0005);
    for f in frames.iter_mut().skip(300) {
        f.ts += 9_000_000;
    }
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    assert_eq!((t.reseeds(), t.phase_steps()), (1, 0));
}

#[test]
fn a_constant_delay_shorter_than_the_run_is_held_not_absorbed_1482() {
    // Issue 1371 stays: 29 frames delivered a constant +2 ms late, then back on phase. Held on
    // the prediction, never folded, never a step, never a re-seed.
    let mut frames = camera(600, 45.0, 3_000.0, 7_000_000.0, 0x1482_0006);
    for f in frames
        .iter_mut()
        .skip(300)
        .take(RESEED_CONSECUTIVE_OUTLIERS as usize - 1)
    {
        f.ts += 2_000_000; // delivered late; the sensor's phase (truth) never moved
    }
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k >= 300 {
            let est = obs.smoothed_mono_ns.expect("still locked") as f64;
            assert!(
                (est - f.truth).abs() < 30_000.0,
                "frame {k}: on the true phase"
            );
        }
    }
    assert_eq!((t.reseeds(), t.phase_steps()), (0, 0));
}

#[test]
fn the_step_note_logs_its_own_line_1482() {
    assert_eq!(
        PhaseNote::PhaseStep { step_ns: 2_124_400 }.log_line(),
        "#1482 capture phase step: +2124 us (card re-timing), lock kept"
    );
    assert_eq!(
        PhaseNote::PhaseStep {
            step_ns: -6_300_900
        }
        .log_line(),
        "#1482 capture phase step: -6300 us (card re-timing), lock kept"
    );
    assert_eq!(
        PhaseNote::HalfPeriod {
            resid_ns: -9_400_123
        }
        .log_line(),
        "#1371 capture phase re-seed: reason=half-period resid_us=-9400"
    );
}

#[test]
fn the_constant_offset_tolerance_edges_1482() {
    // A clean stream: the tolerance is the floor, RESEED_FLOOR_NS / 2 around the median.
    let half = RESEED_FLOOR_NS as i64 / 2;
    let around = |spread: i64| -> Vec<i64> {
        (0..RESEED_CONSECUTIVE_OUTLIERS as i64)
            .map(|i| 2_000_000 + if i % 2 == 0 { -spread } else { spread })
            .collect()
    };
    assert_eq!(constant_step_ns(&around(half), 3_000.0), Some(2_000_000));
    assert_eq!(constant_step_ns(&around(half + 1), 3_000.0), None);
    // A noisy fit widens it to 4 x the RMS.
    assert_eq!(
        constant_step_ns(&around(700_000), 200_000.0),
        Some(2_000_000)
    );
    assert_eq!(constant_step_ns(&around(800_001), 200_000.0), None);
    // An odd count takes the middle residual; an empty run is never a step.
    assert_eq!(
        constant_step_ns(&[1_900_000, 2_100_000, 2_000_000], 3_000.0),
        Some(2_000_000)
    );
    assert_eq!(constant_step_ns(&[], 3_000.0), None);
}

/// Design step 3 (comment 6452540834): a locked camera whose card catches up in a STAIRCASE —
/// every `(frame, step_ns)` moves that frame and all later ones (summed). Returns the tracker
/// and every note it wrote, with the frame index.
fn run_staircase(
    steps: &[(usize, i64)],
    n: usize,
    seed: u64,
) -> (
    CapturePhaseTracker,
    Vec<(usize, PhaseNote)>,
    Option<(f64, f64)>,
) {
    let mut frames = camera(n, 45.0, 3_000.0, 7_000_000.0, seed);
    for &(from, step_ns) in steps {
        for f in frames.iter_mut().skip(from) {
            f.ts = (f.ts as i64 + step_ns) as u64;
            f.truth += step_ns as f64;
        }
    }
    let mut t = CapturePhaseTracker::new();
    let mut noted = Vec::new();
    let mut last = None;
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if let Some(note) = t.note() {
            noted.push((k, note.clone()));
        }
        last = obs.smoothed_mono_ns.map(|e| (e as f64, f.truth));
        if k >= 200 && t.reseeds() == 0 {
            assert!(
                obs.smoothed_mono_ns.is_some(),
                "frame {k}: the lock is kept through the staircase"
            );
        }
    }
    (t, noted, last)
}

/// The staircase is absorbed as ONE step to its last plateau: no re-seed, the lock kept, the
/// window's sums exact and the prediction on the final phase.
fn assert_staircase_absorbed(steps: &[(usize, i64)], seed: u64) {
    let total: i64 = steps.iter().map(|s| s.1).sum();
    let (t, noted, last) = run_staircase(steps, 900, seed);
    assert_eq!(
        t.reseeds(),
        0,
        "{steps:?}: a staircase is not a re-seed ({noted:?})"
    );
    assert_eq!(
        t.phase_steps(),
        1,
        "{steps:?}: one step absorbed ({noted:?})"
    );
    assert!(t.locked());
    assert_eq!(t.sums, fresh_sums(&t), "the moved window's sums stay exact");
    let got: Vec<i64> = noted
        .iter()
        .filter_map(|(_, n)| match n {
            PhaseNote::PhaseStep { step_ns } => Some(*step_ns),
            _ => None,
        })
        .collect();
    assert_eq!(got.len(), 1, "{noted:?}");
    assert!(
        (got[0] - total).abs() < 60_000,
        "the step absorbed is the LAST plateau: {} vs {total}",
        got[0]
    );
    let (est, truth) = last.expect("locked on the last frame");
    assert!(
        (est - truth).abs() < 60_000.0,
        "the prediction sits on the final phase ({:.0} us off)",
        (est - truth) / 1000.0
    );
}

#[test]
fn a_double_card_step_inside_one_run_is_absorbed_to_its_last_plateau_1482() {
    // cam2 live 27.10.2026: +1414 us x19, then +3244 us x11 in one 30-frame run.
    assert_staircase_absorbed(&[(300, 1_414_000), (319, 1_830_000)], 0x1482_d001);
    // +2623 us x21, then +5227 us x9.
    assert_staircase_absorbed(&[(300, 2_623_000), (321, 2_604_000)], 0x1482_d002);
    // The second step only 9 frames in: +1858 us x9, then +3663 us x21.
    assert_staircase_absorbed(&[(300, 1_858_000), (309, 1_805_000)], 0x1482_d003);
}

#[test]
fn the_live_triple_plateau_run_is_absorbed_1482() {
    // cam2 live: +2325 us x16, +2386 us x8 (inside the tolerance: the same plateau), +4688 us x6.
    assert_staircase_absorbed(
        &[(300, 2_325_000), (316, 61_000), (324, 2_302_000)],
        0x1482_d004,
    );
}

#[test]
fn a_last_plateau_too_short_is_held_then_absorbed_1482() {
    // The second step lands on the run's 29th frame: a 2-frame plateau is not yet a phase. The
    // run is held (lock kept, never folded) until the plateau is long enough, then absorbed.
    assert_staircase_absorbed(&[(300, 1_600_000), (328, 1_700_000)], 0x1482_d005);
}

#[test]
fn four_plateaus_still_reseed_1482() {
    let (t, _, _) = run_staircase(
        &[
            (300, 1_200_000),
            (307, 1_200_000),
            (314, 1_200_000),
            (321, 1_200_000),
        ],
        700,
        0x1482_d006,
    );
    assert_eq!(
        (t.reseeds(), t.phase_steps()),
        (1, 0),
        "4 plateaus are not a card step"
    );
}

#[test]
fn an_up_then_down_staircase_still_reseeds_1482() {
    // A card only catches up one way: +3 ms, then back to +1.5 ms is not a stack of steps.
    let (t, _, _) = run_staircase(&[(300, 3_000_000), (315, -1_500_000)], 700, 0x1482_d007);
    assert_eq!((t.reseeds(), t.phase_steps()), (1, 0));
}
