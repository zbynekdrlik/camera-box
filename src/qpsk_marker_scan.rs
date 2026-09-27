//! The QPSK marker demod kernel (issue 1381 split it out of `qpsk_marker`).
//!
//! [`scan_markers`] is the ONE demod: [`crate::qpsk_marker::decode_markers_with_stats`] (the
//! offline `recording-verdict --av-sync`, the `[4b3/8]` probe, the fused A/V gate) scans a whole
//! buffer from 0, and the live dock's [`crate::av_sync_dock::StreamingMarkerDecoder`] scans only
//! the positions of its rolling window that are not final yet. Mirrored byte-for-byte by
//! `cb_scan_markers` in `vendor/av-sync-dock/src/camera-box-marker-scan.hpp`.
//!
//! issue 1381 made the refine a sliding-window maximum ([`RefineWindow`]): each position's preamble
//! magnitude is computed once per scan instead of once per refine that covers it (~223 per passing
//! position), and the window maximum lands on the same position the old linear refine did, so every
//! decoded marker and every counter of a batch decode (a scan from 0) is unchanged
//! (`tests/qpsk_marker_scan_1381.rs` checks the scan against a frozen copy of the old kernel). The
//! streaming decoder's markers match the old whole-window re-decode on every fixture tested; its
//! counters changed meaning (see [`crate::av_sync_dock::StreamingMarkerDecoder`]).

use crate::qpsk_marker::{
    crc4_check, signal_len, AudioParams, DecodeStats, N_PAYLOAD_BITS, N_SYMBOLS, PREAMBLE_NIBBLE,
};
use std::f64::consts::PI;

// Complex helpers (re, im) as f64 pairs — no external num-complex dep, Tier-0.
#[inline]
fn cadd(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 + b.0, a.1 + b.1)
}
#[inline]
fn cmag(a: (f64, f64)) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}
/// a / b for complex numbers.
#[inline]
fn cdiv(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let d = b.0 * b.0 + b.1 * b.1 + 1e-12;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
/// a * b for complex numbers.
#[inline]
fn cmul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// One [`scan_markers`] pass (issue 1381).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkerScan {
    /// `(start sample of the marker within the scanned samples, index)` per accepted marker.
    pub markers: Vec<(usize, u8)>,
    /// The screens, CRC-valid and CRC-failed candidates of this pass.
    pub stats: DecodeStats,
    /// Where a scan of the same audio extended by more samples must start to report the same
    /// markers: the first position visited whose screen passed while its refine range was cut by
    /// the end of the samples (a longer window may refine it differently), else where the scan
    /// stopped. Every position before it is final.
    pub resume: usize,
}

/// issue 1381 — the refine's sliding window over `|preamble(p)|` (mirrored by `CbRefineWindow` in
/// `vendor/av-sync-dock/src/camera-box-marker-scan.hpp`). The scan asks for windows `[from, to]`
/// whose ends never move back, so each position's magnitude is computed ONCE per scan (kept in a
/// ring) and the window maximum comes from a monotonic queue: the positions whose magnitude is not
/// exceeded by a later one, oldest first, magnitudes non-increasing. Its front is the LEFTMOST
/// position holding the maximum, which is where the old linear refine landed: it only moved on a
/// strictly larger magnitude. O(1) amortized per position.
struct RefineWindow {
    /// `mag[p % len]` = `|preamble(p)|` for `p` in `[lo, hi)`.
    mag: Vec<f64>,
    lo: usize,
    hi: usize,
    /// The monotonic queue, a ring of positions `q[head .. tail)`.
    q: Vec<usize>,
    head: usize,
    tail: usize,
}

impl RefineWindow {
    /// `ring` must be at least the widest window asked for.
    fn new(ring: usize) -> Self {
        Self {
            mag: vec![0.0; ring],
            lo: 0,
            hi: 0,
            q: vec![0; ring],
            head: 0,
            tail: 0,
        }
    }

    /// Slide to `[from, to]` (inclusive, `from <= to`); `magnitude(p)` computes a position not
    /// held yet.
    fn slide(&mut self, from: usize, to: usize, magnitude: impl Fn(usize) -> f64) {
        if from >= self.hi || from < self.lo {
            // no overlap with what is held: start over at `from`
            self.lo = from;
            self.hi = from;
            self.head = 0;
            self.tail = 0;
        } else {
            self.lo = from;
        }
        let n = self.q.len();
        while self.head != self.tail && self.q[self.head % n] < from {
            self.head += 1;
        }
        while self.hi <= to {
            let v = magnitude(self.hi);
            let m = self.mag.len();
            self.mag[self.hi % m] = v;
            while self.head != self.tail && self.at(self.q[(self.tail - 1) % n]) < v {
                self.tail -= 1;
            }
            self.q[self.tail % n] = self.hi;
            self.tail += 1;
            self.hi += 1;
        }
    }

    fn at(&self, p: usize) -> f64 {
        self.mag[p % self.mag.len()]
    }

    /// The leftmost position holding the maximum of `[lo, hi)`.
    fn argmax(&self) -> usize {
        self.q[self.head % self.q.len()]
    }

    /// The refine for the screen position `i`: slide to `[from, to]` (which holds `i`) and return
    /// the position of the maximum magnitude, `i` itself on a tie, else the leftmost maximum.
    fn refine(
        &mut self,
        i: usize,
        from: usize,
        to: usize,
        magnitude: impl Fn(usize) -> f64,
    ) -> usize {
        self.slide(from, to, magnitude);
        let best = self.argmax();
        if self.at(best) > self.at(i) {
            best
        } else {
            i
        }
    }
}

/// Detect QPSK markers in mono f32 audio, screening positions from `start` on (issue 1381: the ONE
/// kernel behind [`crate::qpsk_marker::decode_markers_with_stats`], which scans from 0, and the live dock's
/// [`crate::av_sync_dock::StreamingMarkerDecoder`], which scans only the positions not final yet).
///
/// The norihiro demod (`sync-test-output.cpp::st_raw_audio_decode_data`): IQ-demodulate each symbol
/// against the carrier (`Z = Σ signal·e^{-iθ}`, computed O(1) via prefix sums), derotate by the
/// known preamble phasor (absorbs any carrier-phase / sub-sample-alignment error — the reason plain
/// cross-correlation fails on a carrier), read the two bits per symbol from the derotated real/imag
/// signs, then gate. A cheap normalized preamble-magnitude screen finds onsets; only candidates
/// with a correctly-decoded 0xF preamble, a zero nibble and a valid CRC-4 are accepted.
///
/// The work is capped by the samples whatever the audio: at most `n - signal_len + 1` positions,
/// each one magnitude, one screen and O(1) amortized refine-window upkeep, plus one 10-symbol word
/// for a position that passes the screen. Mirrored byte-for-byte by `cb_scan_markers`.
pub fn scan_markers(samples: &[f32], p: &AudioParams, threshold: f64, start: usize) -> MarkerScan {
    let mut out = MarkerScan {
        resume: start,
        ..MarkerScan::default()
    };
    let ar = p.sample_rate as f64;
    let f = p.carrier_hz as f64;
    let c = p.c.max(1) as f64;
    let sps = ar * c / f; // samples per symbol (fractional)
    let sig_len = signal_len(p);
    let n = samples.len();
    if sig_len == 0 || n < sig_len || sps < 1.0 {
        return out;
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
    let magnitude = |base: usize| -> f64 { cmag(preamble(base)) };
    // Cauchy-Schwarz normaliser for the 2-symbol preamble window: |Z| ≤ e·√N.
    let two_sym = (2.0 * sps).round() as usize;
    let norm_at = |base: usize| -> f64 {
        let e = (pe[(base + two_sym).min(n)] - pe[base.min(n)])
            .max(0.0)
            .sqrt();
        e * (two_sym as f64).sqrt() + 1e-12
    };

    // The screen crosses threshold on the RISING edge, up to ~one symbol before the true onset.
    // The refine searches forward across the whole preamble span (+ a few back) for the max
    // preamble magnitude — the true onset, where the 2-symbol window aligns with the 0xF preamble.
    // A too-narrow refine locks onto a misaligned base that can still CRC-pass to a wrong index
    // (observed: onset 65 samples early → 200 misread as 98).
    let span = (2.0 * sps).ceil() as usize;
    let last = n - sig_len; // the last position a marker can start at
    let mut window = RefineWindow::new(span + 8);
    let mut resume_set = false;
    let mut i = start;
    while i + sig_len <= n {
        let lo = i.saturating_sub(4);
        window.slide(lo, i, magnitude);
        let mag_i = window.at(i);
        if mag_i / norm_at(i) >= threshold {
            out.stats.preamble_screens_passed += 1;
            let hi = (i + span).min(last);
            if !resume_set && i + span > last {
                out.resume = i;
                resume_set = true;
            }
            // The max magnitude over [i-4, i+span] (a candidate past `last` cannot hold a whole
            // marker); i itself wins a tie, else the leftmost maximum.
            let base = window.refine(i, lo, hi, magnitude);
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
                out.stats.crc_ok += 1;
                out.markers.push((base, ((word >> 4) & 0xFF) as u8));
                i = base + sig_len; // markers are far apart; skip past this one
                continue;
            }
            out.stats.crc_fail += 1;
        }
        i += 1;
    }
    if !resume_set {
        out.resume = i;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old linear refine over `[lo, hi]` with `i` as the starting best: it moved only on a
    /// strictly larger magnitude.
    fn linear_refine(mag: &[f64], i: usize, lo: usize, hi: usize) -> usize {
        let (mut base, mut bestm) = (i, mag[i]);
        for (cand, &m) in mag.iter().enumerate().take(hi + 1).skip(lo) {
            if m > bestm {
                bestm = m;
                base = cand;
            }
        }
        base
    }

    /// issue 1381: the sliding-window refine lands where the linear refine did, on magnitudes full
    /// of ties (values from a tiny alphabet), over the scan's own access pattern: a screen window
    /// `[i-4, i]`, a refine window `[i-4, min(i+span, last)]`, steps of one and marker-length jumps.
    #[test]
    fn refine_window_lands_where_the_linear_refine_did_1381() {
        let (span, last) = (218usize, 60_000usize);
        let mut seed: u64 = 0x1381_0000_5eed;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        let mag: Vec<f64> = (0..=last).map(|_| (next() % 4) as f64).collect();
        let mut window = RefineWindow::new(span + 8);
        let (mut i, mut refines) = (0usize, 0usize);
        while i <= last {
            let lo = i.saturating_sub(4);
            window.slide(lo, i, |p| mag[p]);
            assert_eq!(window.at(i), mag[i], "position {i}");
            if next() % 3 == 0 {
                let hi = (i + span).min(last);
                let base = window.refine(i, lo, hi, |p| mag[p]);
                assert_eq!(base, linear_refine(&mag, i, lo, hi), "refine at {i}");
                refines += 1;
                if next() % 97 == 0 {
                    i = base + 1085; // a decoded marker: skip past it
                    continue;
                }
            }
            i += 1;
        }
        assert!(refines > 1000, "{refines} refines exercised");
    }

    /// A scan of a longer window started at a shorter scan's `resume` reports the rest of what the
    /// whole scan reports.
    #[test]
    fn a_scan_from_resume_reports_the_rest_of_the_whole_scan_1381() {
        let p = AudioParams::rig60();
        let sig = signal_len(&p);
        let mut x = vec![0.0f32; 48_000];
        for (k, at) in [4_000usize, 20_000, 36_000].iter().enumerate() {
            for (j, s) in crate::qpsk_marker::marker_signal(40 + k as u8, &p)
                .iter()
                .enumerate()
            {
                x[at + j] = *s;
            }
        }
        let whole = scan_markers(&x, &p, 0.35, 0);
        assert_eq!(whole.markers.len(), 3);
        // The first 22 000 samples hold markers 1 and 2 (marker 2 ends at 21 085).
        let head = scan_markers(&x[..22_000], &p, 0.35, 0);
        assert_eq!(head.markers, whole.markers[..2].to_vec());
        // marker 2 skips the scan past its own end, which never lies past the samples
        assert!(head.resume > 20_000 + sig / 2 && head.resume <= 22_000);
        let rest = scan_markers(&x, &p, 0.35, head.resume);
        assert_eq!(rest.markers, whole.markers[2..].to_vec());
    }
}
