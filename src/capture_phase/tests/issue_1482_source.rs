//! Issue 1482 step 2 — the stamp follows the SOURCE phase: a long-horizon fit across a re-timing
//! capture card's sawtooth (cam2's Cam Link, live 27.10.2026), never each tooth.

use super::*;

/// One frame of a re-timing card plus the source's true capture instant (both CLOCK_MONOTONIC ns).
struct CardFrame {
    seq: u32,
    ts: u64,
    source: f64,
}

/// `n` frames of a SOURCE running `src_ppm` fast, re-timed by a card whose own clock runs
/// `card_ppm` fast: the card's offset to the source drifts at the rate difference and the card
/// catches up by `step_ns` whenever the offset leaves `[-step/2, +step/2]`. The sawtooth is
/// bounded by construction, so the long-term rate of the card timestamps IS the source rate:
/// local rate minus the step contribution. `sigma_ns` of Gaussian timestamp jitter.
fn retiming_card(
    n: usize,
    src_ppm: f64,
    card_ppm: f64,
    step_ns: f64,
    start_ns: f64,
    sigma_ns: f64,
    seed: u64,
) -> Vec<CardFrame> {
    let p_src = NOMINAL_60 / (1.0 + src_ppm * 1e-6);
    let p_card = NOMINAL_60 / (1.0 + card_ppm * 1e-6);
    let drift = p_card - p_src;
    let mut rng = Rng(seed);
    let mut offset = 0.0f64;
    (0..n)
        .map(|k| {
            if k > 0 {
                offset += drift;
                if offset < -step_ns / 2.0 {
                    offset += step_ns;
                } else if offset > step_ns / 2.0 {
                    offset -= step_ns;
                }
            }
            let source = MONO0 as f64 + start_ns + k as f64 * p_src;
            CardFrame {
                seq: 1000 + k as u32,
                ts: (source + offset + sigma_ns * rng.gauss()).round() as u64,
                source,
            }
        })
        .collect()
}

/// The first frame index at which the source fit spans [`SOURCE_FIT_MIN_SECONDS`] (plus a margin
/// for the switch itself).
fn after_switch(margin_s: u64) -> usize {
    ((SOURCE_FIT_MIN_SECONDS + margin_s) * 60) as usize
}

/// cam2's shape: a card whose clock runs +100 ppm fast over a 0 ppm source, catching up in +4 ms
/// steps (one per 40 s), the source phase 1 ms before a slot edge. The card's teeth reach 2 ms past
/// the source, so a stamp that follows the teeth crosses the edge twice per tooth; the source phase
/// never crosses. Once the source fit drives the stamp, every slot advances by exactly one.
#[test]
fn the_stamp_follows_the_source_phase_not_the_card_sawtooth_1482() {
    let start = I60 as f64 - 1_000_000.0;
    let frames = retiming_card(
        25_200,
        0.0,
        100.0,
        4_000_000.0,
        start,
        30_000.0,
        0x1482_5a01,
    );
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let mut prev: Option<u64> = None;
    let mut off_slot = Vec::new();
    let mut crossings_at_switch = None;
    for (k, f) in frames.iter().enumerate() {
        let slot = p.stamp_frame(f.seq, f.ts, offset, I60);
        if k == after_switch(15) {
            crossings_at_switch = Some(p.crossings());
        }
        if k > after_switch(15) {
            let s = slot.unwrap_or_else(|| panic!("frame {k}: stamp-driven"));
            if let Some(q) = prev {
                if grid_steps_between(q, s, I60) != 1 {
                    off_slot.push(k);
                }
            }
        }
        prev = slot;
    }
    assert!(
        p.phase_steps() >= 8,
        "the card stepped: {}",
        p.phase_steps()
    );
    assert_eq!(p.reseeds(), 0);
    assert!(
        off_slot.is_empty(),
        "the stamp followed a tooth at frames {off_slot:?}"
    );
    assert_eq!(Some(p.crossings()), crossings_at_switch);
    let src = p.source_rate_ppm(I60).expect("the source fit has a rate");
    assert!(src.abs() < 5.0, "source rate {src:+.1} ppm, expected ~0");
    let local = p.rate_ppm(I60).expect("the short fit has a rate");
    assert!(
        (local - 100.0).abs() < 30.0,
        "the short fit keeps the card's local rate: {local:+.1}"
    );
    // The source fit estimates the source instant within a fraction of the hysteresis.
    let last = frames.last().unwrap();
    let est = p.tracker.source.predict_ns(p.tracker.last_x).unwrap() as f64;
    assert!(
        (est - last.source).abs() < 300_000.0,
        "source estimate {:.0} us off the source",
        (est - last.source) / 1000.0
    );
}

/// A pass-through card (cam3/cam5: no steps): the source fit and the short fit agree within
/// jitter, so the stamp is today's.
#[test]
fn a_pass_through_card_stamps_as_before_1482() {
    let frames = camera(9_000, 15.9, 60_000.0, 8_000_000.0, 0x1482_5a02);
    let mut t = CapturePhaseTracker::new();
    let mut worst = 0.0f64;
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k >= after_switch(1) {
            let short = obs.smoothed_mono_ns.expect("locked") as f64;
            let src = t.source.predict_ns(t.last_x).expect("ready") as f64;
            worst = worst.max((src - short).abs());
        }
    }
    assert!(worst > 0.0, "the source fit was consulted");
    assert!(
        worst < 60_000.0,
        "source vs short prediction differ by {:.0} us",
        worst / 1000.0
    );
    let mut p = CapturePhase::new();
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    for f in &frames {
        p.stamp_frame(f.seq, f.ts, offset, I60);
    }
    let (local, src) = (p.rate_ppm(I60).unwrap(), p.source_rate_ppm(I60).unwrap());
    assert!(
        (local - src).abs() < 6.0 && (src - 15.9).abs() < 3.0,
        "src {src:+.1} vs phase {local:+.1}"
    );
    assert_eq!((p.crossings(), p.reseeds()), (0, 0));
    let line = p.status_tokens(I60);
    assert!(
        line.contains(&format!(" src_ppm={src:+.1} ")),
        "the #707 line carries the source rate: {line}"
    );
}

/// The first [`SOURCE_FIT_MIN_SECONDS`] after a start fall back to the short fit; then the source
/// fit is ready, and its window stays bounded (time span and decimated point count).
#[test]
fn the_first_minute_falls_back_and_the_window_stays_bounded_1482() {
    let frames = camera(60 * 400, 15.9, 30_000.0, 8_000_000.0, 0x1482_5a03);
    let mut t = CapturePhaseTracker::new();
    let mut ready_at = None;
    for (k, f) in frames.iter().enumerate() {
        t.observe(f.seq, f.ts);
        if ready_at.is_none() && t.source.ready() {
            ready_at = Some(k);
        }
        if ready_at.is_none() {
            assert!(t.source.predict_ns(t.last_x).is_none(), "frame {k}");
        }
    }
    let k = ready_at.expect("the source fit becomes ready");
    let s = k as f64 / 60.0;
    assert!(
        (SOURCE_FIT_MIN_SECONDS as f64..SOURCE_FIT_MIN_SECONDS as f64 + 1.0).contains(&s),
        "ready after {s:.2} s"
    );
    let max_span = SOURCE_FIT_SECONDS * NS_PER_SECOND;
    assert!(t.source.span_ns() <= max_span, "{}", t.source.span_ns());
    assert!(t.source.span_ns() + NS_PER_SECOND > max_span);
    let per_window = (SOURCE_FIT_SECONDS as usize * 60) / SOURCE_DECIMATION_FRAMES as usize;
    assert!(
        (per_window - 2..=per_window + 1).contains(&t.source.points()),
        "{} points",
        t.source.points()
    );
}

/// A real discontinuity (a device re-open, a half-period step) starts the source fit over; an
/// absorbed card step does not.
#[test]
fn a_real_discontinuity_resets_the_source_fit_and_an_absorbed_step_does_not_1482() {
    let mut frames = camera(60 * 140, 45.0, 30_000.0, 7_000_000.0, 0x1482_5a04);
    // An absorbed card step of +2 ms at 80 s.
    for f in frames.iter_mut().skip(60 * 80) {
        f.ts += 2_000_000;
    }
    let mut t = CapturePhaseTracker::new();
    for f in frames.iter().take(60 * 100) {
        t.observe(f.seq, f.ts);
    }
    assert_eq!((t.phase_steps(), t.reseeds()), (1, 0));
    assert!(t.source.ready(), "an absorbed step keeps the source fit");
    assert!(t.source.span_ns() > 95 * NS_PER_SECOND);
    // A half-period step at 100 s: a re-seed, and the source fit starts over.
    for f in frames.iter().skip(60 * 100).take(60 * 20) {
        t.observe(f.seq, f.ts + 9_000_000);
    }
    assert_eq!(t.reseeds(), 1);
    assert!(!t.source.ready(), "a re-seed starts the source fit over");
    assert!(t.source.span_ns() < 21 * NS_PER_SECOND);
    // A device re-open (sequence from 0) does too.
    let last = frames[60 * 120 - 1].ts + 9_000_000;
    for (k, f) in frames.iter().skip(60 * 120).enumerate() {
        t.observe(
            k as u32,
            last + 50_000_000 + k as u64 * 16_666_667 + f.ts % 7,
        );
    }
    assert_eq!(t.reseeds(), 2);
    assert!(t.source.span_ns() < 21 * NS_PER_SECOND);
}
