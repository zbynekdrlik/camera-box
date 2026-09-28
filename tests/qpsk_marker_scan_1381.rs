//! Issue 1381 — the QPSK demod kernel was reworked (a sliding-window refine instead of ~223
//! recomputed preamble magnitudes per passing position, and a start position for the live dock's
//! incremental scan). This pins that every decoded marker AND every counter is unchanged: the
//! current `decode_markers_with_stats` against a FROZEN copy of the pre-1381 kernel (below, verbatim
//! from `src/qpsk_marker.rs` at 1.7.0-dev.716), over markers on silence, on white / pink noise, on a
//! music-like mix (which decodes false markers too), on a 442 Hz tone (every position passes the
//! screen), every one of the 256 indices, a non-finite sample, and both channels of the real stereo
//! mbc fixture.
//!
//! Default features, std only.

use camera_box::qpsk_marker::{
    crc4_check, decode_markers_with_stats, marker_signal, signal_len, AudioParams, DecodeStats,
    N_PAYLOAD_BITS, N_SYMBOLS, PREAMBLE_NIBBLE,
};
use std::f64::consts::PI;
use std::path::Path;

const FIXTURE: &str = "tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav";
const THRESHOLDS: [f64; 2] = [0.35, 0.5];

// The pre-1381 kernel's complex helpers, verbatim.
fn cadd(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 + b.0, a.1 + b.1)
}
fn cmag(a: (f64, f64)) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}
fn cdiv(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let d = b.0 * b.0 + b.1 * b.1 + 1e-12;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
fn cmul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// The pre-1381 `decode_markers_with_stats`, verbatim (only renamed).
fn reference_decode(
    samples: &[f32],
    p: &AudioParams,
    threshold: f64,
) -> (Vec<(f64, u8)>, DecodeStats) {
    let mut stats = DecodeStats::default();
    let ar = p.sample_rate as f64;
    let f = p.carrier_hz as f64;
    let c = p.c.max(1) as f64;
    let sps = ar * c / f; // samples per symbol (fractional)
    let sig_len = signal_len(p);
    let n = samples.len();
    if sig_len == 0 || n < sig_len || sps < 1.0 {
        return (Vec::new(), stats);
    }
    // Prefix sums (f64): signal·cos, signal·sin, signal² — absolute carrier phase. Any window's
    // IQ and energy are then O(1). Absolute-vs-relative phase differs only by a constant rotation,
    // which the preamble derotation cancels.
    let w = 2.0 * PI * f / ar;
    let mut pc = vec![0f64; n + 1];
    let mut ps = vec![0f64; n + 1];
    let mut pe = vec![0f64; n + 1];
    for m in 0..n {
        let ph = m as f64 * w;
        let x = samples[m] as f64;
        // #1153: a non-finite input sample would otherwise contaminate every prefix sum after it,
        // silently killing decode for the REST of the window; treat it as silence instead.
        let x = if x.is_finite() { x } else { 0.0 };
        pc[m + 1] = pc[m] + x * ph.cos();
        ps[m + 1] = ps[m] + x * ph.sin();
        pe[m + 1] = pe[m] + x * x;
    }
    // Z over [a,b): e^{-iθ} = cosθ - i·sinθ ⇒ (re = Σ signal·cos, im = -Σ signal·sin).
    let z = |a: usize, b: usize| -> (f64, f64) {
        let a = a.min(n);
        let b = b.min(n);
        (pc[b] - pc[a], -(ps[b] - ps[a]))
    };
    let sym_win = |base: usize, k: usize| -> (usize, usize) {
        (
            base + (k as f64 * sps).round() as usize,
            base + ((k + 1) as f64 * sps).round() as usize,
        )
    };
    let preamble = |base: usize| -> (f64, f64) {
        let (a0, b0) = sym_win(base, 0);
        let (a1, b1) = sym_win(base, 1);
        cadd(z(a0, b0), z(a1, b1))
    };
    // Cauchy-Schwarz normaliser for the 2-symbol preamble window: |Z| ≤ e·√N.
    let two_sym = (2.0 * sps).round() as usize;
    let norm_at = |base: usize| -> f64 {
        let e = (pe[(base + two_sym).min(n)] - pe[base.min(n)])
            .max(0.0)
            .sqrt();
        e * (two_sym as f64).sqrt() + 1e-12
    };

    let mut out = Vec::new();
    let mut i = 0usize;
    while i + sig_len <= n {
        let refph = preamble(i);
        if cmag(refph) / norm_at(i) >= threshold {
            stats.preamble_screens_passed += 1;
            // The screen crosses threshold on the RISING edge, up to ~one symbol before the true
            // onset. Search forward across the whole preamble span (+ a few back) for the max
            // preamble magnitude — the true onset, where the 2-symbol window aligns with the 0xF
            // preamble. A too-narrow refine locks onto a misaligned base that can still CRC-pass to
            // a wrong index (observed: onset 65 samples early → 200 misread as 98).
            let span = (2.0 * sps).ceil() as usize;
            let lo = i.saturating_sub(4);
            let mut base = i;
            let mut bestm = cmag(refph);
            for cand in lo..=(i + span) {
                if cand + sig_len <= n {
                    let m = cmag(preamble(cand));
                    if m > bestm {
                        bestm = m;
                        base = cand;
                    }
                }
            }
            // Rotate the preamble reference by −45° (× (1,−1)) so the on-axis symbol constellation
            // becomes diagonal (±0.5, ±0.5); then sign-of-real and sign-of-imag are each a robust
            // bit, tolerant of the ~45° phasor rotation the single-cycle edge taper introduces at
            // c=1 (norihiro `x *= (1,-1)` then quadrant sign test). Canonical after rotation:
            // sym3→(+,+), sym0→(−,−), sym1→(+,−), sym2→(−,+); sym = 2·(im>0) | 1·(re>0).
            let refp = cmul(preamble(base), (1.0, -1.0));
            let mut word = 0u32;
            for k in 0..N_SYMBOLS as usize {
                let (a, b) = sym_win(base, k);
                let (re, im) = cdiv(z(a, b), refp);
                let sym = (if im > 0.0 { 2u32 } else { 0 }) | (if re > 0.0 { 1 } else { 0 });
                word |= sym << (N_PAYLOAD_BITS - 2 - 2 * k as u32);
            }
            // #1153: the emitter ALWAYS sends the zero nibble (bits[15:12]) == 0, but only the
            // preamble nibble + CRC-4 (8 bits) were ever checked — leaving the CRC-passing
            // accept-space 16x too large (256 valid vs 3840 "poison" words a music mix decodes
            // from noise). Enforcing the zero nibble reclaims those 4 bits of built-in
            // redundancy and cuts the false-decode flood ~16x, with no real marker lost (a real
            // marker's zero nibble is 0 and is already covered by the CRC).
            if (word >> 16) & 0xF == PREAMBLE_NIBBLE
                && (word >> 12) & 0xF == 0
                && crc4_check(word, N_PAYLOAD_BITS) == 0
            {
                stats.crc_ok += 1;
                out.push((base as f64 / ar, ((word >> 4) & 0xFF) as u8));
                i = base + sig_len; // markers are far apart; skip past this one
                continue;
            }
            stats.crc_fail += 1;
        }
        i += 1;
    }
    (out, stats)
}

/// A deterministic pseudo-random stream.
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// `seconds` of a background: silence, white or pink noise, a music-like chord over a pink bed, or
/// a 442 Hz tone.
fn background(kind: &str, seconds: f64, seed: u64) -> Vec<f32> {
    let n = (48_000.0 * seconds) as usize;
    let mut rng = Lcg(seed);
    let (mut b0, mut b1, mut b2) = (0.0f64, 0.0f64, 0.0f64);
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let w = rng.unit();
            b0 = 0.99765 * b0 + w * 0.099_046;
            b1 = 0.963 * b1 + w * 0.296_516_4;
            b2 = 0.57 * b2 + w * 1.052_691_3;
            let v = match kind {
                "white" => 0.1 * w,
                "pink" => 0.05 * (b0 + b1 + b2 + w * 0.1848),
                "music" => {
                    let env = 0.6 + 0.4 * (2.0 * PI * 2.0 * t).sin();
                    let mut chord = 0.0;
                    for (k, f) in [220.0, 277.18, 329.63, 440.0].iter().enumerate() {
                        for h in 1..=4 {
                            chord +=
                                0.04 / h as f64 * (2.0 * PI * f * h as f64 * t + k as f64).sin();
                        }
                    }
                    env * chord + 0.02 * (b0 + b1)
                }
                "tone" => 0.3 * (2.0 * PI * 442.0 * t).sin() + 0.02 * w,
                _ => 0.0,
            };
            v as f32
        })
        .collect()
}

/// Markers every ~0.7 s (index stepping by 180), scaled by `gain`, added into `x`.
fn add_markers(x: &mut [f32], gain: f32) {
    let p = AudioParams::rig60();
    let mut idx = 189u8;
    let mut at = 3_001usize;
    while at + signal_len(&p) < x.len() {
        for (j, s) in marker_signal(idx, &p).iter().enumerate() {
            x[at + j] += gain * s;
        }
        idx = idx.wrapping_add(180);
        at += 33_607;
    }
}

fn assert_same(name: &str, x: &[f32]) -> (usize, DecodeStats) {
    let p = AudioParams::rig60();
    let mut found = 0;
    let mut stats = DecodeStats::default();
    for thr in THRESHOLDS {
        let now = decode_markers_with_stats(x, &p, thr);
        let before = reference_decode(x, &p, thr);
        assert_eq!(
            now, before,
            "{name} at threshold {thr}: markers or counters changed"
        );
        found += now.0.len();
        stats.preamble_screens_passed += now.1.preamble_screens_passed;
    }
    (found, stats)
}

#[test]
fn the_kernel_decodes_what_the_pre_1381_kernel_decoded() {
    let mut screened = 0;
    for (k, kind) in ["silence", "white", "pink", "music", "tone"]
        .iter()
        .enumerate()
    {
        for gain in [0.0f32, 0.1, 0.4, 1.0] {
            let mut x = background(kind, 1.0, 11 + k as u64);
            add_markers(&mut x, gain);
            let (_, stats) = assert_same(&format!("{kind} + markers x{gain}"), &x);
            screened += stats.preamble_screens_passed;
        }
    }
    assert!(
        screened > 100_000,
        "the tone and the music must exercise the refine: {screened}"
    );
    // decodes happen on both kinds of input: clean markers and the music's false ones
    let mut x = background("silence", 1.5, 1);
    add_markers(&mut x, 1.0);
    assert!(assert_same("clean markers", &x).0 >= 3);
    assert!(assert_same("music", &background("music", 2.0, 5)).0 > 0);
}

#[test]
fn every_index_and_a_non_finite_sample_decode_as_before() {
    let p = AudioParams::rig60();
    for idx in 0..=255u8 {
        let mut x = vec![0.0f32; 12_000];
        for (j, s) in marker_signal(idx, &p).iter().enumerate() {
            x[4_801 + idx as usize * 7 + j] = *s;
        }
        if idx % 32 == 0 {
            x[100] = f32::NAN;
            x[101] = f32::INFINITY;
        }
        let (found, _) = assert_same(&format!("index {idx}"), &x);
        assert_eq!(found, THRESHOLDS.len(), "index {idx}");
    }
}

/// The committed 48 kHz stereo s16 fixture as two f32 channels.
fn fixture_channels() -> [Vec<f32>; 2] {
    let b = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE))
        .expect("read the stereo fixture");
    let mut pos = 12;
    loop {
        assert!(pos + 8 <= b.len(), "no data chunk");
        let len = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        if &b[pos..pos + 4] == b"data" {
            let data = &b[pos + 8..(pos + 8 + len).min(b.len())];
            let mut ch = [Vec::new(), Vec::new()];
            for (k, s) in data.as_chunks::<2>().0.iter().enumerate() {
                ch[k % 2].push(i16::from_le_bytes(*s) as f32 / 32768.0);
            }
            return ch;
        }
        pos += 8 + len + (len & 1);
    }
}

#[test]
fn the_real_stereo_fixture_decodes_as_before() {
    let [l, r] = fixture_channels();
    assert!(assert_same("mbc fixture L", &l).0 > 0);
    assert!(assert_same("mbc fixture R", &r).0 > 0);
}
