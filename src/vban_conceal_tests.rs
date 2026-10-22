//! Issue 1439 — the unit tests of `vban_conceal` (the sample math of a concealed slot, ROZHODNUTÉ
//! 6352253147 item (b)), a test-only `#[path]` child so the module stays the math.

use super::*;

const A: f64 = 0.5;
const RATE: u32 = 48_000;

/// `A sin(2 pi f n / RATE + phase)` for `n` in `from .. from + len`.
fn sine(f: f64, phase: f64, from: u32, len: u32) -> Vec<f64> {
    (from..from + len)
        .map(|n| A * (2.0 * PI * f * f64::from(n) / f64::from(RATE) + phase).sin())
        .collect()
}

fn max_step(s: &[f64]) -> f64 {
    s.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f64::max)
}

/// A run of `run` concealed slots of `frames` frames after the packet `prev`, the last one fading
/// toward `next` (the packet that ends the run), every slot fading in from the last sample handed
/// out -- exactly what the source hands to libobs.
fn conceal_run(prev: &[f64], next: f64, run: u32) -> Vec<f64> {
    let frames = prev.len() as u32;
    let fade = fade_frames(frames, RATE);
    let mut out = Vec::new();
    let mut last = prev[prev.len() - 1];
    for slot in 0..run {
        let toward = slot + 1 == run;
        for i in 0..frames {
            out.push(sample(
                prev[i as usize],
                last,
                next,
                toward,
                i,
                frames,
                fade,
            ));
        }
        last = out[out.len() - 1];
    }
    out
}

#[test]
fn the_taylor_cosine_matches_cos_within_1e_14() {
    // the documented bound (the series: 1.2e-15 measured); a series one term short is 4e-13 off
    for k in 0..=100_000u32 {
        let u = f64::from(k) / 100_000.0;
        let d = (cospi(u) - (PI * u).cos()).abs();
        assert!(d < 1e-14, "cos(pi {u}): off by {d:e}");
    }
    assert_eq!(cospi(0.0), 1.0);
    assert_eq!(cospi(1.0), -1.0);
    assert!(cospi(0.5).abs() < 1e-15);
}

/// The crossfade is RAISED-COSINE (ROZHODNUTÉ 6352253147 (b)), not merely some rising curve: a
/// linear ramp passes every other weight test but leaves the slot's last sample 2 % of the bridged
/// gap short of the next packet.
#[test]
fn the_weights_are_the_raised_cosine() {
    for n in [1u32, 2, 3, 12, 48, 59] {
        for i in 0..n {
            let want = 0.5 - 0.5 * (PI * f64::from(i + 1) / f64::from(n + 1)).cos();
            let d = (weight(i, n) - want).abs();
            assert!(d < 1e-14, "weight({i}, {n}) off the raised cosine by {d:e}");
        }
    }
}

#[test]
fn the_weights_rise_strictly_inside_0_1_mirror_and_step_under_the_slope() {
    for n in [1u32, 2, 3, 12, 32, 48, 59, 64] {
        let w: Vec<f64> = (0..n).map(|i| weight(i, n)).collect();
        assert!(w.iter().all(|&x| x > 0.0 && x < 1.0), "n {n}: {w:?}");
        assert!(w.windows(2).all(|p| p[1] > p[0]), "n {n}: rising {w:?}");
        for i in 0..n as usize {
            let s = w[i] + w[n as usize - 1 - i];
            assert!((s - 1.0).abs() < 1e-12, "n {n}: w[{i}] + its mirror = {s}");
        }
        let slope = PI / (2.0 * f64::from(n + 1));
        assert!(w[0] < slope && 1.0 - w[n as usize - 1] < slope, "n {n}");
        assert!(max_step(&w) < slope, "n {n}: {w:?}");
    }
}

#[test]
fn the_fade_is_1_ms_at_most_a_quarter_of_the_slot_and_at_least_one_frame() {
    for (frames, rate, want) in [
        (239u32, 48_000u32, 48u32),
        (256, 48_000, 48),
        (128, 48_000, 32),
        (256, 44_100, 44),
        (64, 8_000, 8),
        (3, 48_000, 1),
        (1, 48_000, 1),
        (256, 500, 1),
    ] {
        assert_eq!(fade_frames(frames, rate), want, "{frames} frames at {rate}");
    }
}

/// Between the two fades the slot IS the previous packet, exactly; the first sample sits next to the
/// last one handed out, the last next to the packet that follows; without `toward_next` the slot
/// ends on the repeat itself.
#[test]
fn a_concealed_slot_is_the_previous_packet_between_its_two_fades() {
    let prev = sine(1_000.0, 0.3, 0, 239);
    let next = sine(1_000.0, 0.3, 478, 1)[0];
    let fade = fade_frames(239, RATE);
    let last = prev[238];
    let c: Vec<f64> = (0..239)
        .map(|i| sample(prev[i as usize], last, next, true, i, 239, fade))
        .collect();
    for i in fade..239 - fade {
        assert_eq!(
            c[i as usize].to_bits(),
            prev[i as usize].to_bits(),
            "frame {i}"
        );
    }
    assert!((c[0] - last).abs() <= weight(0, fade) * (prev[0] - last).abs() + 1e-15);
    assert!(
        (c[238] - next).abs() <= (1.0 - weight(fade - 1, fade)) * (prev[238] - next).abs() + 1e-15
    );
    let open: Vec<f64> = (0..239)
        .map(|i| sample(prev[i as usize], last, next, false, i, 239, fade))
        .collect();
    assert_eq!(
        open[238].to_bits(),
        prev[238].to_bits(),
        "no fade toward a missing slot"
    );
    // a constant packet repeated after itself is that constant exactly (the fade of equal values)
    let flat = [0.25f64; 239];
    for i in 0..239 {
        assert_eq!(
            sample(flat[i as usize], 0.25, 0.25, true, i, 239, fade),
            0.25
        );
    }
}

/// The point of the concealment (no click): a run of one, two or three concealed slots never steps
/// more than the signal's own largest step plus the fades' slope over the gap they bridge (2A at
/// most), from the previous packet through the run into the packet that ends it -- while silence
/// steps by the whole sample value at both edges.
#[test]
fn a_concealed_run_never_steps_more_than_the_signal_plus_the_fade_slope() {
    let frames = 239u32;
    let fade = fade_frames(frames, RATE);
    for f in [100.0, 440.0, 1_000.0, 3_000.0, 6_000.0] {
        for phase in [0.0, 1.1, 2.5] {
            for run in 1..=3u32 {
                let prev = sine(f, phase, 0, frames);
                let next = sine(f, phase, (run + 1) * frames, frames);
                let truth = sine(f, phase, 0, (run + 2) * frames);
                let mut s = prev.clone();
                s.extend(conceal_run(&prev, next[0], run));
                s.extend(&next);
                assert_eq!(s.len(), truth.len());
                let bound = max_step(&truth) + 2.0 * A * PI / (2.0 * f64::from(fade + 1));
                let got = max_step(&s);
                assert!(
                    got <= bound + 1e-12,
                    "{f} Hz, phase {phase}, run {run}: a step of {got} over the bound {bound}"
                );
            }
        }
    }
}
