use super::*;
use crate::genlock_grid::grid_steps_between;

/// The production emit interval (60 fps) and its exact nominal period.
const I60: u64 = NS_PER_SECOND / 60;
const NOMINAL_60: f64 = NS_PER_SECOND as f64 / 60.0;
/// 2026-09-23 17:16:33 UTC — a real date (the #1355 tests' second), far from 1970.
const SEC_2309: u64 = 1_790_176_593;
/// A plausible CLOCK_MONOTONIC origin (~ 3.5 days of uptime).
const MONO0: u64 = 300_000 * NS_PER_SECOND;

/// Deterministic noise (splitmix64); `gauss` is the Irwin-Hall sum of 12 uniforms (bounded
/// +-6 sigma, reproducible on every platform).
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }
}

/// One camera frame: its sequence number, the jittered V4L2 timestamp and the true capture
/// instant (both CLOCK_MONOTONIC ns).
#[derive(Clone, Copy)]
struct Frame {
    seq: u32,
    ts: u64,
    truth: f64,
}

/// `n` frames of a camera running `ppm` fast (positive = shorter period) with `sigma_ns` of
/// Gaussian timestamp jitter, starting at `MONO0 + start_ns`.
fn camera(n: usize, ppm: f64, sigma_ns: f64, start_ns: f64, seed: u64) -> Vec<Frame> {
    let period = NOMINAL_60 / (1.0 + ppm * 1e-6);
    let mut rng = Rng(seed);
    (0..n)
        .map(|k| {
            let truth = MONO0 as f64 + start_ns + k as f64 * period;
            Frame {
                seq: 1000 + k as u32,
                ts: (truth + sigma_ns * rng.gauss()).round() as u64,
                truth,
            }
        })
        .collect()
}

/// The exact sums over the tracker's current window, rebuilt from scratch.
fn fresh_sums(t: &CapturePhaseTracker) -> FitSums {
    let mut s = FitSums::default();
    let (x0, t0) = *t.window.front().unwrap();
    for &(x, ts) in &t.window {
        s.add(
            i128::from(x) - i128::from(x0),
            i128::from(ts) - i128::from(t0),
        );
    }
    s
}

#[test]
fn incremental_sums_equal_a_fresh_sum_over_the_window() {
    let mut frames = camera(900, 15.9, 60_000.0, 1_234_567.0, 0x1367_d201);
    // Drop a few frames (sequence gaps) so the window spans non-unit steps.
    for k in [100usize, 101, 400, 650] {
        frames[k].seq = u32::MAX; // marker: skipped below
    }
    let mut t = CapturePhaseTracker::new();
    for f in frames.iter().filter(|f| f.seq != u32::MAX) {
        t.observe(f.seq, f.ts);
        assert_eq!(
            t.sums,
            fresh_sums(&t),
            "the re-anchored running sums drifted from the window"
        );
    }
    assert_eq!(t.window.len(), FIT_WINDOW_FRAMES);
    assert_eq!(t.reseeds(), 0);
}

#[test]
fn seeding_gives_no_estimate_then_a_clean_camera_locks() {
    let frames = camera(400, 15.9, 60_000.0, 5_000_000.0, 0x1367_d202);
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k + 1 < LOCK_MIN_FRAMES {
            assert_eq!(obs.smoothed_mono_ns, None, "frame {k} must still seed");
        } else {
            let est = obs.smoothed_mono_ns.expect("locked");
            assert!(
                (est as f64 - f.truth).abs() < 30_000.0,
                "frame {k}: smoothed {est} vs truth {} (raw jitter 60 us)",
                f.truth
            );
        }
        if k > 0 {
            assert_eq!(obs.seq_advance, 1);
        }
    }
    assert!(t.locked());
}

#[test]
fn the_fit_reads_the_camera_rate_and_the_jitter() {
    for ppm in [15.9f64, -15.9, 0.0, 900.0] {
        let frames = camera(1_000, ppm, 60_000.0, 3_000_000.0, 0x1367_d203);
        let mut p = CapturePhase::new();
        for f in &frames {
            p.stamp_frame(f.seq, f.ts, 0, I60);
        }
        let measured = p.rate_ppm(I60).unwrap();
        assert!(
            (measured - ppm).abs() < 12.0,
            "rate {measured:.2} ppm vs the camera's {ppm} ppm (slope noise ~3 ppm at 60 us)"
        );
        let jitter = p.jitter_rms_us().unwrap();
        assert!(
            (45.0..75.0).contains(&jitter),
            "jitter {jitter:.1} us vs 60 us"
        );
    }
}

#[test]
fn a_dropped_frame_is_a_sequence_step_of_two_not_a_reseed() {
    let frames = camera(400, 15.9, 60_000.0, 7_000_000.0, 0x1367_d204);
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        if k == 300 {
            continue; // the device dropped this frame; its sequence number is never delivered
        }
        let obs = t.observe(f.seq, f.ts);
        if k == 301 {
            assert_eq!(obs.seq_advance, 2, "the frame after a drop advances two");
            let est = obs.smoothed_mono_ns.unwrap() as f64;
            assert!((est - f.truth).abs() < 30_000.0);
        }
    }
    assert_eq!(t.reseeds(), 0);
}

#[test]
fn one_late_timestamp_is_not_folded_and_does_not_reseed() {
    let mut frames = camera(400, 15.9, 60_000.0, 7_000_000.0, 0x1367_d205);
    frames[300].ts += 5_000_000; // a 5 ms late interrupt on one frame
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let before = t.sums;
        let obs = t.observe(f.seq, f.ts);
        if k == 300 {
            assert_eq!(
                t.sums, before,
                "the outlier must not be folded into the fit"
            );
            let est = obs.smoothed_mono_ns.expect("still locked") as f64;
            assert!(
                (est - f.truth).abs() < 30_000.0,
                "the outlier is stamped from the prediction"
            );
        }
    }
    assert_eq!(t.reseeds(), 0);
    assert!(t.locked());
}

#[test]
fn a_real_phase_step_reseeds_once_then_relocks_on_the_new_phase() {
    let mut frames = camera(700, 15.9, 60_000.0, 7_000_000.0, 0x1367_d206);
    for f in frames.iter_mut().skip(300) {
        f.ts += 3_000_000; // a grabber re-lock: every later frame 3 ms later
        f.truth += 3_000_000.0;
    }
    let mut t = CapturePhaseTracker::new();
    let mut unlocked_after_step = 0;
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k >= 300 && obs.smoothed_mono_ns.is_none() {
            unlocked_after_step += 1;
        }
        if k >= 300 + RESEED_CONSECUTIVE_OUTLIERS as usize + LOCK_MIN_FRAMES {
            let est = obs.smoothed_mono_ns.expect("relocked") as f64;
            assert!(
                (est - f.truth).abs() < 30_000.0,
                "frame {k} on the new phase"
            );
        }
    }
    assert_eq!(t.reseeds(), 1, "one phase step = one re-seed");
    assert_eq!(
        unlocked_after_step,
        LOCK_MIN_FRAMES - 1,
        "from the re-seed frame on, the fit seeds exactly like a fresh start (raw stamp)"
    );
}

#[test]
fn a_backward_or_a_huge_sequence_step_reseeds() {
    let frames = camera(300, 15.9, 60_000.0, 7_000_000.0, 0x1367_d207);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    assert!(t.locked());
    // A device re-open restarts the sequence at 0.
    let next = frames.last().unwrap().ts + 40_000_000;
    let obs = t.observe(0, next);
    assert_eq!(obs.smoothed_mono_ns, None);
    assert_eq!(t.reseeds(), 1);
    // A forward step above MAX_SEQ_ADVANCE also re-seeds.
    t.observe(1 + MAX_SEQ_ADVANCE + 1, next + 200_000_000);
    assert_eq!(t.reseeds(), 2);
    // MAX_SEQ_ADVANCE itself does not.
    let mut u = CapturePhaseTracker::new();
    u.observe(10, MONO0);
    u.observe(
        10 + MAX_SEQ_ADVANCE,
        MONO0 + u64::from(MAX_SEQ_ADVANCE) * I60,
    );
    assert_eq!(u.reseeds(), 0);
}

#[test]
fn a_frame_without_a_timestamp_changes_nothing() {
    let frames = camera(200, 15.9, 60_000.0, 7_000_000.0, 0x1367_d208);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    let before = (t.sums, t.window.len(), t.last_seq, t.last_x);
    let obs = t.observe(frames[199].seq + 1, 0);
    assert_eq!(obs.smoothed_mono_ns, None);
    assert_eq!(obs.seq_advance, 0);
    assert_eq!((t.sums, t.window.len(), t.last_seq, t.last_x), before);
}

// ── SlotHysteresis ───────────────────────────────────────────────────────────

/// Smoothed instants sweeping through a slot edge at `drift_ns` per frame (negative = camera
/// faster: the phase falls), each wobbling by up to `wobble_ns` either way. Returns the chosen
/// slots and events.
fn sweep(
    drift_ns: f64,
    wobble_ns: f64,
    start_phase_ns: f64,
    n: usize,
) -> Vec<(u64, SlotEvent, u64)> {
    let mut h = SlotHysteresis::default();
    let mut rng = Rng(0x1367_d2aa);
    let base = SEC_2309 * NS_PER_SECOND;
    (0..n)
        .map(|k| {
            // Relative time in f64 (ns precision), the 2026 base added as an integer.
            let rel = start_phase_ns
                + k as f64 * (NOMINAL_60 + drift_ns)
                + wobble_ns * (2.0 * rng.unit() - 1.0);
            let t = base + rel.round() as u64;
            let (slot, ev) = h.choose(t, 1, I60);
            (slot, ev, t)
        })
        .collect()
}

fn slot_steps(out: &[(u64, SlotEvent, u64)]) -> Vec<u64> {
    out.windows(2)
        .map(|w| grid_steps_between(w[0].0, w[1].0, I60))
        .collect()
}

#[test]
fn hysteresis_makes_a_fast_crossing_exactly_one_duplicate_slot() {
    // Phase starts 2 ms above an edge and falls 1 us per frame; +-200 us wobble (< the 500 us
    // hysteresis) would flip a plain floor for ~400 frames.
    let out = sweep(-1_000.0, 200_000.0, 2_000_000.0, 5_000);
    let crossings = out.iter().filter(|o| o.1 == SlotEvent::Crossing).count();
    assert_eq!(crossings, 1);
    let steps = slot_steps(&out);
    assert_eq!(
        steps.iter().filter(|&&s| s == 0).count(),
        1,
        "one duplicate"
    );
    assert_eq!(steps.iter().filter(|&&s| s == 1).count(), steps.len() - 1);
    assert!(out.iter().any(|o| o.1 == SlotEvent::Held));
    // Without the hysteresis the same instants flip the plain floor many times.
    let raw: Vec<u64> = out.iter().map(|o| grid_floor_ns(o.2, I60)).collect();
    let raw_flips = raw
        .windows(2)
        .filter(|w| grid_steps_between(w[0], w[1], I60) != 1)
        .count();
    assert!(
        raw_flips > 20,
        "the plain floor flipped only {raw_flips} times"
    );
}

#[test]
fn hysteresis_makes_a_slow_crossing_exactly_one_missing_slot() {
    let out = sweep(1_000.0, 200_000.0, NOMINAL_60 - 2_000_000.0, 5_000);
    let crossings = out.iter().filter(|o| o.1 == SlotEvent::Crossing).count();
    assert_eq!(crossings, 1);
    let steps = slot_steps(&out);
    assert_eq!(
        steps.iter().filter(|&&s| s == 2).count(),
        1,
        "one missing slot"
    );
    assert_eq!(steps.iter().filter(|&&s| s == 1).count(), steps.len() - 1);
}

#[test]
fn the_chosen_slot_stays_within_the_hysteresis_of_the_instant() {
    for drift in [-1_000.0f64, 1_000.0] {
        for (slot, _, t) in sweep(drift, 200_000.0, 8_000_000.0, 12_000) {
            assert!(
                slot <= t + SLOT_HYSTERESIS_NS,
                "slot {slot} more than the hysteresis after the instant {t}"
            );
            let next = grid_advance_ns(slot, 1, I60);
            assert!(
                t < next + SLOT_HYSTERESIS_NS,
                "instant {t} more than the hysteresis past its slot's end {next}"
            );
        }
    }
}

#[test]
fn a_sequence_advance_of_two_expects_two_slots_without_a_crossing() {
    let mut h = SlotHysteresis::default();
    let t0 = SEC_2309 * NS_PER_SECOND + 8_000_000;
    let (s0, _) = h.choose(t0, 1, I60);
    let (s1, ev) = h.choose(t0 + 2 * I60, 2, I60);
    assert_eq!(ev, SlotEvent::OnTime);
    assert_eq!(grid_steps_between(s0, s1, I60), 2);
}

#[test]
fn a_step_beyond_one_slot_is_a_jump_not_a_crossing() {
    let mut h = SlotHysteresis::default();
    let t0 = SEC_2309 * NS_PER_SECOND + 8_000_000;
    h.choose(t0, 1, I60);
    let (_, ev) = h.choose(t0 + I60 + 700_000_000, 1, I60);
    assert_eq!(ev, SlotEvent::Jump);
    let (_, ev) = h.choose(t0 + 2 * I60 + 700_000_000, 1, I60);
    assert_eq!(ev, SlotEvent::OnTime, "the chooser continues from the jump");
    let (_, ev) = h.choose(t0, 1, I60);
    assert_eq!(ev, SlotEvent::Jump, "a backward step is a jump too");
}

// ── CapturePhase ─────────────────────────────────────────────────────────────

#[test]
fn a_locked_one_to_one_camera_drives_the_stamp_with_consecutive_slots() {
    let frames = camera(600, 15.9, 60_000.0, 8_000_000.0, 0x1367_d209);
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let mut slots = Vec::new();
    for (k, f) in frames.iter().enumerate() {
        let s = p.stamp_frame(f.seq, f.ts, offset, I60);
        assert_eq!(s.is_some(), k + 1 >= LOCK_MIN_FRAMES, "frame {k}");
        slots.extend(s);
    }
    assert_eq!(p.mode(), PhaseMode::Stamp);
    assert!(slots
        .windows(2)
        .all(|w| grid_steps_between(w[0], w[1], I60) == 1));
    assert_eq!(p.crossings(), 0);
}

#[test]
fn an_over_rate_grabber_never_drives_the_stamp() {
    // 61.5 fps: ~24 600 ppm fast — the dupe_decimation over-rate regime, today's path.
    let frames = camera(600, 24_600.0, 60_000.0, 8_000_000.0, 0x1367_d20a);
    let mut p = CapturePhase::new();
    for f in &frames {
        assert_eq!(p.stamp_frame(f.seq, f.ts, 0, I60), None);
    }
    assert_eq!(p.mode(), PhaseMode::Band);
    let ppm = p.rate_ppm(I60).unwrap();
    assert!((ppm - 24_600.0).abs() < 50.0, "{ppm}");
}

#[test]
fn genlock_off_never_drives_the_stamp() {
    let frames = camera(300, 15.9, 60_000.0, 8_000_000.0, 0x1367_d20b);
    let mut p = CapturePhase::new();
    for f in &frames {
        assert_eq!(p.stamp_frame(f.seq, f.ts, 0, 0), None);
    }
    assert_eq!(p.mode(), PhaseMode::Off, "genlock off: nothing to drive");
    let tokens = p.status_tokens(0);
    assert!(tokens.starts_with(" phase_lock=off "), "{tokens}");
    assert!(tokens.contains(" phase_ppm=na "), "{tokens}");
}

/// The `key=value` pairs of an emitted status string, in order.
fn token_pairs(tokens: &str) -> Vec<(String, String)> {
    tokens
        .split_whitespace()
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| panic!("no '=' in {kv:?}"));
            (format!("{k}="), v.to_string())
        })
        .collect()
}

#[test]
fn status_tokens_parse_and_their_keys_are_mutually_non_substring() {
    let frames = camera(300, 15.9, 60_000.0, 8_000_000.0, 0x1367_d20c);
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    assert_eq!(
        p.status_tokens(I60),
        " phase_lock=seed phase_ppm=na jitter_us=na crossings=0 reseeds=0 hidden_drops=0"
    );
    for f in &frames {
        p.stamp_frame(f.seq, f.ts, offset, I60);
    }
    let emitted = p.status_tokens(I60);
    let pairs = token_pairs(&emitted);
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "phase_lock=",
            "phase_ppm=",
            "jitter_us=",
            "crossings=",
            "reseeds=",
            "hidden_drops="
        ]
    );
    assert_eq!(pairs[0].1, "stamp");
    let ppm: f64 = pairs[1].1.parse().expect("phase_ppm is a signed number");
    assert!((ppm - 15.9).abs() < 12.0, "{emitted}");
    let jitter: f64 = pairs[2].1.parse().expect("jitter_us is a number");
    assert!((40.0..80.0).contains(&jitter), "{emitted}");
    assert_eq!(
        (
            pairs[3].1.as_str(),
            pairs[4].1.as_str(),
            pairs[5].1.as_str()
        ),
        ("0", "0", "0")
    );
    // The emitted keys and the line's existing ones are mutually non-substring.
    let mut all: Vec<&str> = vec!["emit-1s:", "cap-1s:"];
    all.extend(keys.iter().copied());
    for a in &all {
        for b in &all {
            if a != b {
                assert!(!b.contains(a), "{a} is a substring of {b}");
            }
        }
    }
}

// ── review round 1 (issue 1367 D2) ───────────────────────────────────────────

/// A frame the DEVICE itself skipped (an HDMI hiccup): uvcvideo only counts frames the device
/// sent, so the next one arrives with a sequence step of 1 but two periods later. It must be
/// re-indexed as a two-frame advance (stamped on its true slot, the gap filled by the gate), never
/// stamped from the stale prediction one slot early.
#[test]
fn a_frame_the_device_skipped_is_re_indexed_not_stamped_one_slot_early() {
    let mut frames = camera(400, 15.9, 60_000.0, 7_000_000.0, 0x1367_d211);
    frames.remove(300);
    for f in frames.iter_mut().skip(300) {
        f.seq -= 1; // the sequence never showed the skipped frame
    }
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if (300..305).contains(&k) {
            assert_eq!(obs.seq_advance, if k == 300 { 2 } else { 1 }, "frame {k}");
            let est = obs.smoothed_mono_ns.expect("still locked") as f64;
            assert!(
                (est - f.truth).abs() < 30_000.0,
                "frame {k}: stamped {:.0} us off its true capture",
                (est - f.truth) / 1000.0
            );
        }
    }
    assert_eq!(t.reseeds(), 0);
    assert_eq!(t.hidden_drops(), 1);
}

/// A device pause the sequence does not show (seconds of nothing, then the next sequence
/// number): a real discontinuity, never stamped from the prediction — re-seed at once.
#[test]
fn a_pause_the_sequence_does_not_show_reseeds_at_once() {
    let frames = camera(300, 15.9, 60_000.0, 7_000_000.0, 0x1367_d212);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    let last = frames.last().unwrap();
    let obs = t.observe(last.seq + 1, last.ts + 1_500_000_000);
    assert_eq!(obs.smoothed_mono_ns, None);
    assert_eq!(t.reseeds(), 1);
}

/// A residual of half a frame or more is no timestamp jitter: re-seed at once instead of
/// stamping two frames from the old phase.
#[test]
fn a_half_frame_phase_step_reseeds_at_once() {
    let mut frames = camera(400, 15.9, 60_000.0, 7_000_000.0, 0x1367_d213);
    for f in frames.iter_mut().skip(300) {
        f.ts += 9_000_000;
    }
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k == 300 {
            assert_eq!(obs.smoothed_mono_ns, None, "no stamp from the old phase");
            assert_eq!(t.reseeds(), 1);
        }
    }
    assert_eq!(t.reseeds(), 1);
}

/// A stall while seeding (before any outlier gate) re-seeds, which also keeps the window's time
/// span, and so every i128 sum, bounded.
#[test]
fn a_stall_while_seeding_reseeds() {
    let frames = camera(20, 15.9, 60_000.0, 7_000_000.0, 0x1367_d214);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    let last = frames.last().unwrap();
    t.observe(last.seq + 1, last.ts + 60 * NS_PER_SECOND);
    assert_eq!(t.reseeds(), 1);
    assert_eq!(t.window.len(), 1, "the stalled sample starts a new seed");
}

/// A sub-millisecond phase step folded while seeding must not lock a tilted fit: every stamp the
/// tracker hands out stays on the camera's true phase.
#[test]
fn a_step_folded_while_seeding_never_locks_a_tilted_fit() {
    let mut frames = camera(700, 15.9, 60_000.0, 7_000_000.0, 0x1367_d215);
    for f in frames.iter_mut().skip(60) {
        f.ts += 800_000;
        f.truth += 800_000.0;
    }
    let mut t = CapturePhaseTracker::new();
    let mut locked_frames = 0;
    for (k, f) in frames.iter().enumerate() {
        if let Some(est) = t.observe(f.seq, f.ts).smoothed_mono_ns {
            locked_frames += 1;
            assert!(
                (est as f64 - f.truth).abs() < 100_000.0,
                "frame {k}: a tilted fit stamped {:.0} us off",
                (est as f64 - f.truth) / 1000.0
            );
        }
    }
    assert!(
        locked_frames > 200,
        "it locks once the step left the window"
    );
}

/// The capture loop's stamp instant (fed to `genlock_emit_timecode_100ns`): the slot middle while
/// the tracker drives, else today's raw capture instant in the realtime domain.
#[cfg(target_os = "linux")]
#[test]
fn the_stamp_instant_is_the_slot_middle_when_driven_else_the_raw_capture() {
    let slot = grid_floor_ns(SEC_2309 * NS_PER_SECOND + 123_456_789, I60);
    assert_eq!(
        stamp_instant_100ns(Some(slot), I60, 5_000, 70),
        slot_mid_realtime_100ns(slot, I60)
    );
    assert_eq!(stamp_instant_100ns(None, I60, 5_000, 70), 5_070);
}

#[test]
fn slot_helpers_round_trip_on_the_sender_grid_all_day() {
    for fps in [30u64, 60] {
        let interval = NS_PER_SECOND / fps;
        let day0 = SEC_2309 * NS_PER_SECOND;
        let mut rng = Rng(0x1367_d20d ^ fps);
        for _ in 0..20_000 {
            let t = day0 + rng.next_u64() % (86_400 * NS_PER_SECOND);
            let slot = grid_floor_ns(t, interval);
            let stamp = slot_stamp_100ns(slot, interval, fps);
            assert_eq!(stamp_slot_ns(stamp, interval), slot);
            // The sender stamp of any instant inside the slot is the slot's stamp — except the
            // last <= 99 ns, where the 100 ns sender grid already starts the next slot (a sender
            // stamp sits up to 99 ns before its slot's ns grid point, see genlock_grid).
            let late_ns = grid_advance_ns(slot, 1, interval) - 100;
            for inside in [slot, slot + 1, late_ns] {
                assert_eq!(
                    per_second_floor(inside / 100, fps, UNITS_100NS_PER_SECOND) as i64,
                    stamp
                );
            }
            let mid = slot_mid_realtime_100ns(slot, interval);
            assert_eq!(
                per_second_floor(mid as u64, fps, UNITS_100NS_PER_SECOND) as i64,
                stamp
            );
        }
    }
}

/// The capture loop stamps a tracked frame through `genlock_emit_timecode_100ns` of the slot
/// middle: that must be exactly the slot's sender stamp.
#[cfg(target_os = "linux")]
#[test]
fn the_capture_loop_timecode_of_a_tracked_slot_is_the_slot_stamp() {
    let mut rng = Rng(0x1367_d20e);
    for _ in 0..20_000 {
        let t = SEC_2309 * NS_PER_SECOND + rng.next_u64() % (86_400 * NS_PER_SECOND);
        let slot = grid_floor_ns(t, I60);
        let tc = crate::genlock_stamp::genlock_emit_timecode_100ns(
            slot_mid_realtime_100ns(slot, I60),
            0,
            60,
        );
        assert_eq!(tc, slot_stamp_100ns(slot, I60, 60));
    }
}

// ── review round 2 (issue 1367 D2) ───────────────────────────────────────────

/// A slow stream (8 sequence steps per frame, 100 ms per step) with two unfolded outliers between
/// every pushed sample: the window's sequence span reaches 256 x 24, three times the span the fit
/// was first sized for. The fit must stay exact and never overflow its `i128` arithmetic (a debug
/// build panics on overflow, a release build would wrap silently).
#[test]
fn a_slow_stream_with_outlier_runs_never_overflows_the_fit() {
    const STEP: u32 = MAX_SEQ_ADVANCE;
    const PER_STEP_NS: u64 = MAX_FRAME_PERIOD_NS;
    let mut t = CapturePhaseTracker::new();
    let mut seq = 1000u32;
    let mut x = 0u64;
    let at = |x: u64| MONO0 + x * PER_STEP_NS;
    // A clean full window first (its jitter is 0, so the outlier bound is the 1 ms floor).
    for _ in 0..FIT_WINDOW_FRAMES {
        t.observe(seq, at(x));
        seq += STEP;
        x += u64::from(STEP);
    }
    // Then: two samples 30 ms late (outliers under half a period, not folded, a run of two
    // never re-seeds), one clean sample (folded, run reset) — until the window holds only
    // samples 24 steps apart.
    for _ in 0..FIT_WINDOW_FRAMES {
        for late in [30_000_000u64, 30_000_000, 0] {
            t.observe(seq, at(x) + late);
            seq += STEP;
            x += u64::from(STEP);
        }
    }
    assert_eq!(t.reseeds(), 0);
    let span = t.window.back().unwrap().0 - t.window.front().unwrap().0;
    assert!(
        span > FIT_WINDOW_FRAMES as i64 * i64::from(MAX_SEQ_ADVANCE),
        "the scenario must exceed the naive span bound: {span}"
    );
    assert_eq!(
        t.jitter_rms_ns(),
        Some(0.0),
        "the clean samples fit exactly"
    );
    let period = t.period_ns().unwrap();
    assert!((period - PER_STEP_NS as f64).abs() < 1e-6, "{period}");
}

/// One timestamp a whole period late (a USB completion delayed by a frame) with the next frame on
/// time reads like a frame the device skipped: the stamp is one slot ahead once, then the next
/// frame's residual of minus one period re-seeds. The accepted cost (recorded in the rule): one
/// starvation repeat and ~2 s on today's raw path. This test pins it.
#[test]
fn a_single_timestamp_one_period_late_costs_one_hidden_drop_then_one_reseed() {
    let mut frames = camera(400, 15.9, 60_000.0, 7_000_000.0, 0x1367_d221);
    frames[300].ts += NOMINAL_60.round() as u64;
    let mut t = CapturePhaseTracker::new();
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k == 300 {
            assert_eq!(obs.seq_advance, 2, "read as a device skip");
        }
        if k == 301 {
            assert_eq!(obs.smoothed_mono_ns, None, "the contradiction re-seeds");
        }
    }
    assert_eq!(t.hidden_drops(), 1);
    assert_eq!(t.reseeds(), 1);
}

/// A CLOCK_REALTIME step (the mono-to-real offset moves) shifts the slot the instant maps to, but
/// it is no camera-edge crossing: `crossings=` must count only the camera drift. After the step
/// the slot follows the new realtime floor.
#[test]
fn a_realtime_step_is_never_counted_as_a_crossing() {
    // Mid-slot phase (8 ms into a slot), 15.9 ppm: no real crossing within 1000 frames.
    let frames = camera(1_000, 15.9, 20_000.0, 8_000_000.0, 0x1367_d222);
    let base = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let steps: [(usize, i64); 3] = [(400, 19_700_000), (600, 50_000_000), (800, -14_000_000)];
    let mut p = CapturePhase::new();
    let mut offset = base;
    for (k, f) in frames.iter().enumerate() {
        if let Some(&(_, d)) = steps.iter().find(|s| s.0 == k) {
            offset += d;
        }
        let slot = p.stamp_frame(f.seq, f.ts, offset, I60);
        if steps.iter().any(|s| s.0 == k) {
            let truth_real = (f.truth as i64 + offset) as u64;
            assert_eq!(
                slot,
                Some(grid_floor_ns(truth_real, I60)),
                "frame {k}: the slot follows the new realtime floor"
            );
        }
    }
    assert_eq!(p.reseeds(), 0);
    assert_eq!(p.crossings(), 0, "clock steps are not crossings");
}

/// Frames whose residuals alternate +-a, `a` ramping slowly (so no sample is an outlier).
fn alternating_jitter(amplitudes: &[f64], seed: u64) -> Vec<Frame> {
    let mut frames = camera(amplitudes.len(), 15.9, 0.0, 7_000_000.0, seed);
    for (k, f) in frames.iter_mut().enumerate() {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        f.ts = (f.truth + sign * amplitudes[k]).round() as u64;
    }
    frames
}

fn ramp(from: f64, to: f64, n: usize) -> Vec<f64> {
    (0..n)
        .map(|k| from + (to - from) * k as f64 / n as f64)
        .collect()
}

/// The lock has hysteresis: it is taken at `LOCK_MAX_JITTER_NS` and dropped only above
/// `LOCK_EXIT_JITTER_NS`, so a stream near the bound does not switch paths back and forth.
#[test]
fn a_locked_fit_stays_locked_between_the_lock_and_the_unlock_bound() {
    let mut amps = vec![100_000.0; 400];
    amps.extend(ramp(100_000.0, 1_500_000.0, 1_000));
    amps.extend(vec![1_500_000.0; 600]);
    let frames = alternating_jitter(&amps, 0x1367_d223);
    let mut t = CapturePhaseTracker::new();
    let mut unlocked_after_lock = 0;
    for (k, f) in frames.iter().enumerate() {
        let obs = t.observe(f.seq, f.ts);
        if k >= LOCK_MIN_FRAMES && obs.smoothed_mono_ns.is_none() {
            unlocked_after_lock += 1;
        }
    }
    assert!(
        t.jitter_rms_ns().unwrap() > LOCK_MAX_JITTER_NS as f64,
        "the hold is above the lock bound"
    );
    assert_eq!(
        unlocked_after_lock, 0,
        "1.5 ms of jitter keeps an existing lock"
    );
    assert_eq!(t.reseeds(), 0);

    // Above the unlock bound the fit lets go.
    let mut amps2 = amps.clone();
    amps2.extend(ramp(1_500_000.0, 3_000_000.0, 1_000));
    amps2.extend(vec![3_000_000.0; 600]);
    let frames = alternating_jitter(&amps2, 0x1367_d223);
    let mut t = CapturePhaseTracker::new();
    for f in &frames {
        t.observe(f.seq, f.ts);
    }
    assert!(!t.locked(), "3 ms of jitter unlocks");
}

/// A camera whose rate drifts from 1900 ppm towards 2700 ppm and back: the 1:1 band has
/// hysteresis, entered at `STAMP_MODE_MAX_RATE_PPM`, left only above `STAMP_MODE_EXIT_RATE_PPM`.
#[test]
fn the_stamp_band_has_hysteresis_at_its_edge() {
    // Phases of (ppm from, ppm to, frames).
    let phases: [(f64, f64, usize); 8] = [
        (1_900.0, 1_900.0, 400),
        (1_900.0, 2_300.0, 2_400),
        (2_300.0, 2_300.0, 400),
        (2_300.0, 2_700.0, 2_400),
        (2_700.0, 2_700.0, 400),
        (2_700.0, 2_300.0, 2_400),
        (2_300.0, 2_300.0, 400),
        (2_300.0, 1_900.0, 2_400),
    ];
    let mut rng = Rng(0x1367_d224);
    let offset = (SEC_2309 * NS_PER_SECOND - MONO0) as i64;
    let mut p = CapturePhase::new();
    let mut truth = MONO0 as f64 + 7_000_000.0;
    let mut seq = 1000u32;
    let mut modes = Vec::new();
    for (from, to, n) in phases {
        let mut phase_modes = Vec::new();
        for k in 0..n {
            let ppm = from + (to - from) * k as f64 / n as f64;
            truth += NOMINAL_60 / (1.0 + ppm * 1e-6);
            let ts = (truth + 10_000.0 * rng.gauss()).round() as u64;
            p.stamp_frame(seq, ts, offset, I60);
            seq += 1;
            phase_modes.push(p.mode());
        }
        modes.push(phase_modes);
    }
    let after_first_lock = &modes[0][LOCK_MIN_FRAMES..];
    assert!(after_first_lock.iter().all(|&m| m == PhaseMode::Stamp));
    for (i, phase) in modes[1..3].iter().enumerate() {
        assert!(
            phase.iter().all(|&m| m == PhaseMode::Stamp),
            "phase {}: 2300 ppm keeps an existing stamp mode",
            i + 1
        );
    }
    assert_eq!(
        *modes[4].last().unwrap(),
        PhaseMode::Band,
        "2700 ppm leaves it"
    );
    assert!(
        modes[6].iter().all(|&m| m == PhaseMode::Band),
        "2300 ppm does not re-enter it"
    );
    assert_eq!(
        *modes[7].last().unwrap(),
        PhaseMode::Stamp,
        "1900 ppm re-enters it"
    );
    assert_eq!(p.reseeds(), 0);
}
