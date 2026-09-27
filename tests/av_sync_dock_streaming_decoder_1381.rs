//! Issue 1381 — the live dock's `StreamingMarkerDecoder` scans only the positions of its rolling
//! window that are not final yet, and must still report exactly what the old decoder reported when
//! it re-decoded the WHOLE window on every call.
//!
//! The dock ran `push()` on the OBS audio thread, and the whole-window re-decode screened every
//! position about twice. Every screen that passed re-computed ~223 preamble magnitudes. A 442 Hz
//! tone passes the screen at every position: 35 ms of CPU per stereo push on an N100, against a
//! 21.3 ms audio tick. The C++ twin (`camera-box-marker-scan.hpp`) is checked against a frozen copy
//! of the old C++ kernel by `tests/c/av_sync_dock_demod_bench_1381.cpp`, and against this Rust push
//! by push by `tests/qpsk_channel_pick_parity_1367.rs`.
//!
//! Default features, std only.

use camera_box::av_sync_dock::{StreamingMarkerDecoder, DOCK_QPSK_THRESHOLD};
use camera_box::qpsk_marker::{decode_markers_with_stats, marker_signal, signal_len, AudioParams};

/// The pre-1381 `push()`: re-decode the WHOLE window every call, report each marker once.
struct WholeWindowReference {
    params: AudioParams,
    buf: Vec<f32>,
    capacity: usize,
    origin: u64,
    last: Option<u64>,
    min_gap: u64,
}

impl WholeWindowReference {
    fn new(params: AudioParams) -> Self {
        let sig = signal_len(&params);
        Self {
            params,
            buf: Vec::new(),
            capacity: sig * 3,
            origin: 0,
            last: None,
            min_gap: sig as u64,
        }
    }

    fn push(&mut self, samples: &[f32]) -> Vec<(u64, u8)> {
        self.buf.extend_from_slice(samples);
        if self.buf.len() > self.capacity {
            let drop = self.buf.len() - self.capacity;
            self.buf.drain(0..drop);
            self.origin += drop as u64;
        }
        let sr = self.params.sample_rate as f64;
        let mut out = Vec::new();
        for (ts, idx) in decode_markers_with_stats(&self.buf, &self.params, DOCK_QPSK_THRESHOLD).0 {
            let abs = self.origin + (ts * sr).round() as u64;
            if self.last.is_none_or(|prev| abs > prev + self.min_gap) {
                self.last = Some(abs);
                out.push((abs, idx));
            }
        }
        out
    }
}

/// Push `x` in `chunk`-sample callbacks through the decoder and the whole-window reference; they
/// must return the same markers every callback. Returns how many markers came back.
fn assert_same(name: &str, x: &[f32], chunk: usize) -> usize {
    let p = AudioParams::rig60();
    let sig = signal_len(&p);
    let mut dec = StreamingMarkerDecoder::new(p, DOCK_QPSK_THRESHOLD, sig * 3, sig as u64);
    let mut reference = WholeWindowReference::new(p);
    let mut found = 0;
    for (k, c) in x.chunks(chunk).enumerate() {
        let got = dec.push(c);
        assert_eq!(got, reference.push(c), "{name} chunk {chunk} push {k}");
        found += got.len();
    }
    found
}

/// `push()` reports exactly what the whole-window decode reported, callback by callback: every
/// index (each at a different phase against the callback grid), a rig-cadence track, several
/// callback sizes.
#[test]
fn streaming_decoder_matches_the_whole_window_decode_1381() {
    let p = AudioParams::rig60();
    for idx in 0..=255u8 {
        let mut x = vec![0.0f32; 12_000];
        let at = 4_801 + idx as usize * 7;
        for (j, s) in marker_signal(idx, &p).iter().enumerate() {
            x[at + j] = *s;
        }
        let chunks: &[usize] = if idx % 16 == 0 {
            &[1024, 441, 256]
        } else {
            &[1024]
        };
        for &chunk in chunks {
            assert_eq!(
                assert_same(&format!("index {idx}"), &x, chunk),
                1,
                "index {idx}"
            );
        }
    }
    let mut track = vec![0.0f32; 48_000 * 4];
    for k in 0..3usize {
        let at = 12_345 + k * 62_503;
        for (j, s) in marker_signal(189u8.wrapping_add((k * 180) as u8), &p)
            .iter()
            .enumerate()
        {
            track[at + j] = 0.8 * s;
        }
    }
    for chunk in [1024usize, 441, 256] {
        assert_eq!(
            assert_same("track", &track, chunk),
            3,
            "track chunk {chunk}"
        );
    }
}

/// Each push screens only the positions not yet final: the new ones plus at most one refine span
/// that the window end cut. A 442 Hz tone passes the screen at every position, the worst case.
#[test]
fn streaming_decoder_screens_each_position_about_once_1381() {
    let p = AudioParams::rig60();
    let sig = signal_len(&p);
    let sr = p.sample_rate as f64;
    let n = 48_000 * 3;
    let tone: Vec<f32> = (0..n)
        .map(|i| (0.3 * (2.0 * std::f64::consts::PI * 442.0 * i as f64 / sr).sin()) as f32)
        .collect();
    let mut dec = StreamingMarkerDecoder::new(p, DOCK_QPSK_THRESHOLD, sig * 3, sig as u64);
    let mut pushes = 0u64;
    for c in tone.chunks(1024) {
        assert!(dec.push(c).is_empty(), "a pure tone is not a marker");
        pushes += 1;
    }
    let screens = dec.stats().preamble_screens_passed;
    let positions = (n - sig + 1) as u64;
    let span = (2.0 * sr / p.carrier_hz as f64).ceil() as u64;
    assert!(
        screens >= positions * 9 / 10,
        "a tone passes the screen nearly everywhere: {screens} of {positions}"
    );
    assert!(
        screens <= positions + pushes * (span + 1),
        "{screens} screens for {positions} positions in {pushes} pushes: the window is \
         re-screened instead of only the new positions (issue 1381)"
    );
}
