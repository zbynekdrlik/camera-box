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

/// Review round 1: a LONE absorbed step (live once: a -6.3 ms re-lock among the sawtooth's
/// catch-ups) is not reset away; the source line walks onto the new phase over the window, the
/// least-squares end value overshooting it by up to a third of the step. Pinned bounds: the step
/// moves the camera one slot earlier here, and the stamp makes exactly ONE crossing for it (no
/// back-and-forth), the source estimate never more than the step + 0.3 ms after the capture (the
/// sender contract's stamp-before-send margin is the ~11 ms capture-to-send latency), never more
/// than a third of the step + 0.3 ms before it, and no re-seed.
#[test]
fn a_lone_absorbed_step_walks_the_source_onto_the_new_phase_in_one_crossing_1482() {
    let step_ns: i64 = -6_300_000;
    let mut frames = camera(60 * 420, 0.0, 30_000.0, 3_000_000.0, 0x1482_5a05);
    for f in frames.iter_mut().skip(60 * 120) {
        f.ts = (f.ts as i64 + step_ns) as u64;
        f.truth += step_ns as f64;
    }
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let (mut late, mut early) = (0.0f64, 0.0f64);
    for (k, f) in frames.iter().enumerate() {
        p.stamp_frame(f.seq, f.ts, offset, I60);
        if k > after_switch(1) {
            let est = p.tracker.source_prediction_ns().expect("ready") as f64;
            late = late.max(est - f.truth);
            early = early.max(f.truth - est);
        }
    }
    eprintln!(
        "lone step {step_ns}: crossings {} phase_steps {} reseeds {} late {:.0} us early {:.0} us",
        p.crossings(),
        p.phase_steps(),
        p.reseeds(),
        late / 1000.0,
        early / 1000.0
    );
    assert_eq!((p.phase_steps(), p.reseeds()), (1, 0));
    assert_eq!(p.crossings(), 1, "one slot move, one crossing");
    let s = step_ns.unsigned_abs() as f64;
    assert!(late <= s + 300_000.0, "late {:.0} us", late / 1000.0);
    assert!(
        early <= s / 3.0 + 300_000.0,
        "overshoot {:.0} us",
        early / 1000.0
    );
}

/// E2E 42361059715 (7.11.2026): cam2's card stepped LATER by +2..+8 ms every 1-4 s for a while
/// (`phase_steps` 58 -> 61 in 10 s) and the 180 s source line, which averages its steps, trailed
/// the capture by ~45 ms (`handoff_age` 63/70 ms vs `capture_age` 18 ms): frames left the box
/// stamped two slots old, the strih FIFO at its 36 ms pin underran and dropped (a copy + a gap on
/// the stream recording). A BURST of steps is a real phase move, not a sawtooth to average: the
/// stamped slot must stay within a slot of the frame's own capture instant on every frame.
#[test]
fn a_burst_of_card_steps_never_leaves_the_stamp_a_slot_behind_the_capture_1482() {
    let mut frames = camera(60 * 240, 0.0, 30_000.0, 4_000_000.0, 0x1482_5a06);
    // Five +6 ms catch-ups, 3 s apart, starting 30 s after the source fit drives the stamp.
    let first = after_switch(30);
    for burst in 0..5 {
        for f in frames.iter_mut().skip(first + burst * 180) {
            f.ts += 6_000_000;
            f.truth += 6_000_000.0;
        }
    }
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let mut worst = 0.0f64;
    for (k, f) in frames.iter().enumerate() {
        let slot = p.stamp_frame(f.seq, f.ts, offset, I60);
        if k > after_switch(1) {
            let s = slot.unwrap_or_else(|| panic!("frame {k}: stamp-driven"));
            let mid = s as f64 + I60 as f64 / 2.0;
            let capture = f.truth + offset as f64;
            worst = worst.max((mid - capture).abs());
        }
    }
    eprintln!("burst: worst {:.1} ms", worst / 1e6);
    assert_eq!((p.phase_steps(), p.reseeds()), (5, 0));
    let r = p.source_reanchors();
    assert!((1..=5).contains(&r), "{r} re-anchors for five steps");
    // The bound is the requirement, not the result: a stamp a slot old is what underran the strih
    // FIFO live. Expected worst: half a slot (the slot middle, 8.3 ms) + the re-anchor band
    // (a quarter slot, 4.2 ms: the source line may sit that far off before it moves) + the
    // hysteresis (0.5 ms) + jitter = ~13 ms; measured 13.7 ms here (14.7 ms on the sawtooth card,
    // 7.11.2026). Two slots old (the unclamped line, 25.7 ms) fails it.
    assert!(
        worst <= I60 as f64,
        "the stamped slot's middle sat {:.1} ms from the capture (bound {:.1} ms)",
        worst / 1e6,
        I60 as f64 / 1e6
    );
}

/// Review of the burst clamp (7.11.2026): a clamp to the band edge left the stamp the short fit
/// +- a quarter slot, so it moved with the card's teeth. On cam2's sawtooth card a burst must still
/// move the stamp only by the burst's own slots, never back and forth with a tooth: every slot
/// advance is 1 or 2 (a real forward move), never 0 right after a 2 (a dup after a gap) or the
/// reverse, and the crossings stay within the burst's slot count + 1.
#[test]
fn a_burst_on_a_sawtooth_card_moves_the_stamp_only_by_its_own_slots_1482() {
    let start = I60 as f64 - 1_000_000.0;
    let mut frames = retiming_card(
        60 * 240,
        0.0,
        100.0,
        4_000_000.0,
        start,
        30_000.0,
        0x1482_5a07,
    );
    let first = after_switch(30);
    for burst in 0..5 {
        for f in frames.iter_mut().skip(first + burst * 180) {
            f.ts += 6_000_000;
            f.source += 6_000_000.0;
        }
    }
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let mut prev: Option<u64> = None;
    let mut last_odd: Option<(usize, u64)> = None;
    let mut back_and_forth = Vec::new();
    let mut worst = 0.0f64;
    let mut crossings_at_switch = 0;
    for (k, f) in frames.iter().enumerate() {
        let slot = p.stamp_frame(f.seq, f.ts, offset, I60);
        if k == after_switch(15) {
            crossings_at_switch = p.crossings();
        }
        if k > after_switch(15) {
            let s = slot.unwrap_or_else(|| panic!("frame {k}: stamp-driven"));
            let mid = s as f64 + I60 as f64 / 2.0;
            worst = worst.max((mid - (f.source + offset as f64)).abs());
            if let Some(q) = prev {
                let adv = grid_steps_between(q, s, I60);
                if adv != 1 {
                    if let Some((j, a)) = last_odd {
                        if a != adv && k - j < 60 {
                            back_and_forth.push(k);
                        }
                    }
                    last_odd = Some((k, adv));
                }
            }
        }
        prev = slot;
    }
    let crossings = p.crossings() - crossings_at_switch;
    eprintln!(
        "sawtooth burst: crossings {crossings} phase_steps {} reseeds {} worst {:.1} ms",
        p.phase_steps(),
        p.reseeds(),
        worst / 1e6
    );
    assert_eq!(p.reseeds(), 0);
    assert!(
        back_and_forth.is_empty(),
        "the stamp went back and forth with a tooth at frames {back_and_forth:?}"
    );
    // 5 x 6 ms = 30 ms = 1.8 slots: at most 2 real moves + 1 edge settle.
    assert!(crossings <= 2, "{crossings} crossings for a 1.8-slot burst");
    let r = p.source_reanchors();
    assert!((1..=5).contains(&r), "{r} re-anchors for five steps");
    assert!(worst <= I60 as f64, "worst {:.1} ms", worst / 1e6);
}

/// The re-anchor band: within a quarter slot (never under 3 ms) nothing moves; past it the
/// distance onto the short fit, signed; an unrepresentable distance moves nothing.
#[test]
fn the_source_reanchor_band_and_sign_1482() {
    let b = I60 / SOURCE_MAX_OFFSET_DIVISOR;
    let t = 5_000_000_000u64;
    assert_eq!(source_reanchor_ns(t + b, t, I60), None);
    assert_eq!(source_reanchor_ns(t - b, t, I60), None);
    assert_eq!(
        source_reanchor_ns(t + b + 1, t, I60),
        Some(-((b + 1) as i64))
    );
    assert_eq!(source_reanchor_ns(t - b - 1, t, I60), Some((b + 1) as i64));
    // 120 fps: a quarter slot is 2.08 ms, the floor keeps a 2.5 ms tooth inside the band.
    let i120 = 8_333_333u64;
    assert_eq!(source_reanchor_ns(t + 2_500_000, t, i120), None);
    assert_eq!(
        source_reanchor_ns(t + SOURCE_REANCHOR_MIN_BAND_NS + 1, t, i120),
        Some(-((SOURCE_REANCHOR_MIN_BAND_NS + 1) as i64))
    );
    assert_eq!(source_reanchor_ns(u64::MAX, 0, I60), None);
}

/// A shift moves the source prediction by exactly the delta and keeps the rate.
#[test]
fn a_source_shift_moves_the_intercept_only_1482() {
    let frames = camera(60 * 90, 15.9, 30_000.0, 8_000_000.0, 0x1482_5a08);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    let before = t.source.predict_ns(t.last_x).expect("ready");
    let period = t.source.period_ns().unwrap();
    for delta in [7_000_000i64, -11_000_000] {
        let p0 = t.source.predict_ns(t.last_x).unwrap();
        t.source.shift(delta);
        let p1 = t.source.predict_ns(t.last_x).unwrap();
        assert_eq!(p1 as i64 - p0 as i64, delta);
        assert!((t.source.period_ns().unwrap() - period).abs() < 1e-6);
    }
    assert_eq!(
        t.source.predict_ns(t.last_x).unwrap() as i64,
        before as i64 - 4_000_000
    );
}
