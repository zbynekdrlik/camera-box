//! Pure Tier-0 QPSK audio-marker protocol for the cam2 A/V-sync marker (#188/#145).
//!
//! WHY crate root, not `src/probe/`: this is the byte-exact norihiro
//! obs-audio-video-sync-dock audio protocol (CRC-4 payload, QPSK symbols, carrier
//! math) with NO I/O and NO probe deps, so it compiles + unit-tests on DEFAULT
//! features (Tier-0). The probe-gated ALSA emitter (`src/probe/qpsk_emit.rs`, next)
//! and the recording-verdict decoder call INTO this module. Same pure-seam pattern as
//! `src/colour_scale.rs`: pure math here; the framebuffer/ALSA/ffmpeg glue lives in
//! probe-gated modules that CI builds.
//!
//! Reverse-engineered from `vendor/av-sync-dock/tool/videogen.py` (encoder) and
//! `vendor/av-sync-dock/src/sync-test-output.cpp` (decoder). The ENCODER stays
//! byte-compatible with norihiro so the (custom) dock can decode what cam2 emits; the
//! DECODER here is a self-consistent normalized-correlation demod used by
//! recording-verdict for the phone-free A/V-sync verdict. Supersedes the scrapped
//! chirp approach (`src/av_sync.rs`) — see
//! `docs/superpowers/plans/2026-07-01-av-sync-norihiro-dock-port.md`.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::f64::consts::PI;

/// Audio sample rate (Hz) — norihiro `--ar` default.
pub const AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;
/// Carrier frequency (Hz) — norihiro `f=` default.
pub const CARRIER_HZ_DEFAULT: u32 = 442;
/// Total payload bits: 4 preamble + 8 index + 4 zero + 4 CRC.
pub const N_PAYLOAD_BITS: u32 = 20;
/// QPSK symbols per marker (2 bits each) → 10.
pub const N_SYMBOLS: u32 = N_PAYLOAD_BITS / 2;
/// Preamble nibble bits[19:16] = 0b1111.
pub const PREAMBLE_NIBBLE: u32 = 0xF;
/// CRC-4/ITU polynomial x^4 + x + 1.
pub const CRC4_POLY: u32 = 0x13;
/// Amplitude scale (norihiro context default).
pub const AMPLITUDE: f64 = 0.8;
/// Raised-cosine smoothing window at symbol boundaries (in carrier cycles).
pub const AUDIO_CONTINUOUS_CYCLES: f64 = 0.25;
/// Frames per QR/sync segment (norihiro `q=` default).
pub const Q_FRAMES: u32 = 2;

/// CRC-4 encoder — matches `crc4()` in `vendor/av-sync-dock/tool/videogen.py`.
pub fn crc4(mut data: u32, size: u32) -> u32 {
    data <<= 4;
    let mut p = CRC4_POLY << (size - 1);
    let mut s = size as i64;
    while s > 0 {
        if data & (0x8u32 << s) != 0 {
            data ^= p;
        }
        s -= 1;
        p >>= 1;
    }
    data
}

/// CRC-4 residual check — matches `crc4_check()` in
/// `vendor/av-sync-dock/src/sync-test-output.cpp`. Returns 0 for a valid word.
pub fn crc4_check(mut data: u32, size: u32) -> u32 {
    let mut p = CRC4_POLY << (size - 5);
    let mut s = size;
    while s > 4 {
        if data & (1 << (s - 1)) != 0 {
            data ^= p;
        }
        s -= 1;
        p >>= 1;
    }
    data
}

/// 20-bit payload word: bits[19:16]=0xF preamble, bits[15:12]=0 (zero nibble), bits[11:4]=index, bits[3:0]=CRC4.
pub fn payload_word(index: u8) -> u32 {
    let data16 = 0xF000u32 | (index as u32 & 0xFF);
    (data16 << 4) | crc4(data16, 16)
}

/// The 10 QPSK symbols of a payload word, MSB-first (symbol 0 = bits[19:18]).
/// Mapping (norihiro): 0(00)→sin, 1(01)→cos, 2(10)→-cos, 3(11)→-sin.
pub fn symbols(word20: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    for i in 0..N_SYMBOLS {
        let shift = N_PAYLOAD_BITS - 2 - i * 2; // 18,16,...,0
        out[i as usize] = ((word20 >> shift) & 0b11) as u8;
    }
    out
}

/// Auto cycles-per-symbol `c` = q*f*vr_den // (vr_num*N_SYMBOLS). Integer floor (Python `//`).
/// Rig 60/1, q=2, f=442 → 1. 30fps → 2. Caller MUST assert the result > 0.
pub fn auto_c(q: u32, f_hz: u32, vr_num: u32, vr_den: u32) -> u32 {
    q * f_hz * vr_den / (vr_num * N_SYMBOLS)
}

/// norihiro video-QR content string: `q={q_ms},i={i},f={f},c={c},t={t},I={I}`.
pub fn qr_text(q_ms: u32, index: u8, f_hz: u32, c: u32, type_flags: u32, index_max: u32) -> String {
    format!("q={q_ms},i={index},f={f_hz},c={c},t={type_flags},I={index_max}")
}

/// Audio-marker generation parameters (one source of truth for emitter + any re-render check).
#[derive(Debug, Clone, Copy)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub carrier_hz: u32,
    /// Cycles per symbol (> 0; `auto_c` or caller).
    pub c: u32,
    pub amplitude: f64,
    /// `true` = raised-cosine boundary smoothing (norihiro default); `false` = rectangular.
    pub smoothing: bool,
    pub vr_num: u32,
    pub vr_den: u32,
    pub q: u32,
}

impl AudioParams {
    /// The rig default: 48 kHz, 442 Hz carrier, 60/1 fps, q=2 → c=1, smoothed.
    pub fn rig60() -> Self {
        Self {
            sample_rate: AUDIO_SAMPLE_RATE_HZ,
            carrier_hz: CARRIER_HZ_DEFAULT,
            c: auto_c(Q_FRAMES, CARRIER_HZ_DEFAULT, 60, 1),
            amplitude: AMPLITUDE,
            smoothing: true,
            vr_num: 60,
            vr_den: 1,
            q: Q_FRAMES,
        }
    }
}

fn symbol_wave(sym: u8, phase: f64) -> f64 {
    match sym {
        0 => phase.sin(),
        1 => phase.cos(),
        2 => -phase.cos(),
        3 => -phase.sin(),
        _ => 0.0,
    }
}

/// Number of samples in a marker's 10-symbol signal (no lead silence): `N_SYMBOLS * c * ar / f`.
pub fn signal_len(p: &AudioParams) -> usize {
    (N_SYMBOLS * p.c * p.sample_rate / p.carrier_hz) as usize
}

/// The 10-symbol QPSK waveform for `index`, mono f32 in [-1,1] (no lead silence, amplitude 1.0).
pub fn marker_signal(index: u8, p: &AudioParams) -> Vec<f32> {
    marker_signal_for_word(payload_word(index), p)
}

/// The 10-symbol QPSK waveform for an ARBITRARY 20-bit payload `word` (not just a valid marker),
/// so a test can synthesize a CRC-valid-but-structurally-invalid "poison" word (#1153). Real emit
/// always uses [`marker_signal`] (word = [`payload_word`], which has a zero nibble at bits[15:12]);
/// this shared renderer lets the decoder-hardening test drive a nonzero zero-nibble through it.
pub(crate) fn marker_signal_for_word(word: u32, p: &AudioParams) -> Vec<f32> {
    let s = symbols(word);
    let ar = p.sample_rate as f64;
    let f = p.carrier_hz as f64;
    let c = p.c.max(1);
    let n = signal_len(p);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let iu = i as u32;
        let phase = i as f64 * 2.0 * PI * f / ar;
        let k = ((iu * p.carrier_hz) / (p.sample_rate * c)).min(N_SYMBOLS - 1) as usize;
        let sym = s[k];
        let mut v = symbol_wave(sym, phase);
        if p.smoothing {
            // fractional position within the current symbol, in carrier cycles
            let f_sym = ((iu * p.carrier_hz) % (p.sample_rate * c)) as f64 / ar;
            // norihiro sentinel: sym_prev=-1 at the first symbol, sym_next=-1 at the last, so the
            // very first rising edge and very last falling edge always get the raised-cosine ramp
            // (marker-boundary envelope) — matches videogen.py, keeps the emit byte-exact.
            let prev: i32 = if k > 0 { s[k - 1] as i32 } else { -1 };
            let next: i32 = if k + 1 < N_SYMBOLS as usize {
                s[k + 1] as i32
            } else {
                -1
            };
            if f_sym < AUDIO_CONTINUOUS_CYCLES && sym as i32 != prev {
                v *= 0.5 - (f_sym / AUDIO_CONTINUOUS_CYCLES * PI).cos() * 0.5;
            } else if (c as f64 - f_sym) < AUDIO_CONTINUOUS_CYCLES && sym as i32 != next {
                v *= 0.5 - ((c as f64 - f_sym) / AUDIO_CONTINUOUS_CYCLES * PI).cos() * 0.5;
            }
        }
        out.push(v as f32);
    }
    out
}

/// Lead-silence samples that position symbol-0 at the vsync centre: `ar*(q*2)*vr_den // vr_num`.
pub fn lead_silence_len(p: &AudioParams) -> usize {
    (p.sample_rate as u64 * (p.q as u64 * 2) * p.vr_den as u64 / p.vr_num as u64) as usize
}

/// One marker's audio as mono f32 in [-1,1]: lead silence (minus `start_offset` already-emitted
/// lead samples, for continuous interleaving) then the 10 QPSK symbols.
pub fn render_marker_audio(index: u8, p: &AudioParams, start_offset: usize) -> Vec<f32> {
    let lead = lead_silence_len(p).saturating_sub(start_offset);
    let sig = marker_signal(index, p);
    let mut out = vec![0.0f32; lead];
    out.extend_from_slice(&sig);
    out
}

/// Mono f32 [-1,1] → interleaved stereo i16 LE (L==R), scaled by `amplitude`, clamped.
pub fn to_stereo_i16(mono: &[f32], amplitude: f64) -> Vec<i16> {
    let scale = amplitude * i16::MAX as f64;
    let mut out = Vec::with_capacity(mono.len() * 2);
    for &s in mono {
        let v = (s as f64 * scale)
            .round()
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(v);
        out.push(v);
    }
    out
}

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

/// Diagnostic counters for one [`decode_markers_with_stats`] call — the #690 live-dock ask: tell a
/// live session apart-and-not-guessing WHETHER the demod (a) sees no candidate onsets at all (bad
/// audio routing/level — `preamble_screens_passed == 0`), (b) sees candidates that never decode a
/// valid marker (garbage/noise on the mix — `crc_fail > 0`, `crc_ok == 0`), or (c) decodes valid
/// markers fine (`crc_ok > 0`) — in which case a still-empty live "Audio Index" points further
/// downstream (the ring lookup / rolling-cluster lock gates in `av_sync_dock.rs`, not the demod
/// itself). Pure counting, zero effect on [`decode_markers`]'s returned markers — mirrored
/// byte-for-byte into `camera-box-audio.hpp`'s `CbDecodeStats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeStats {
    /// Number of sample onsets whose 2-symbol preamble screen crossed `threshold` (a candidate the
    /// demod attempted to fully decode). Zero means the demod never saw anything resembling the
    /// marker's preamble in this window — the most likely cause is no/near-silent signal.
    pub preamble_screens_passed: u64,
    /// Of those candidates, how many decoded a valid marker (0xF preamble nibble + CRC-4 pass) —
    /// equals the length of the returned `Vec`.
    pub crc_ok: u64,
    /// Of those candidates, how many failed the preamble-nibble or CRC-4 check (screened onset, but
    /// not a real marker — normal on a music-laden mix; every real marker refines to CRC-valid).
    pub crc_fail: u64,
}

/// Detect QPSK markers in mono f32 audio → `(audio_ts_s at signal start, index)` per marker.
///
/// The norihiro demod (`sync-test-output.cpp::st_raw_audio_decode_data`): IQ-demodulate each symbol
/// against the carrier (`Z = Σ signal·e^{-iθ}`, computed O(1) via prefix sums), derotate by the
/// known preamble phasor (absorbs any carrier-phase / sub-sample-alignment error — the reason plain
/// cross-correlation fails on a carrier), read the two bits per symbol from the derotated real/imag
/// signs, then CRC-gate. A cheap normalized preamble-magnitude screen finds onsets; only CRC-valid
/// candidates with a correctly-decoded 0xF preamble are accepted. `threshold` is the preamble screen
/// (≈0.7 for a clean aligned marker; 0.4 is a robust default). Self-consistent with `marker_signal`.
///
/// Thin wrapper over [`decode_markers_with_stats`] (identical decode, stats discarded) — kept so
/// every existing caller/test is untouched by the #690 diagnostics addition.
pub fn decode_markers(samples: &[f32], p: &AudioParams, threshold: f64) -> Vec<(f64, u8)> {
    decode_markers_with_stats(samples, p, threshold).0
}

/// Same decode as [`decode_markers`], plus [`DecodeStats`] counting how many candidate onsets were
/// screened, and of those, how many decoded a valid marker vs. failed the preamble/CRC check. See
/// [`DecodeStats`] for why this exists (the #690 live-dock "audio index never locks" diagnosis).
pub fn decode_markers_with_stats(
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

/// The marker-log CSV header: a `#`-comment recording the emit `AudioParams` (the decoder MUST
/// demodulate with the same params, and persisting them makes a silent emitter/decoder drift
/// visible — the two sides otherwise hardcode `rig60()` independently) followed by the column
/// header row. Split out of `serialize_qpsk_marker_log` (#431) so the probe-gated incremental
/// writer (`src/probe/qpsk_emit.rs`) can write this ONCE up front, then append
/// [`qpsk_marker_log_row`] per emitted marker, without duplicating the format string.
pub fn qpsk_marker_log_header(p: &AudioParams) -> String {
    format!(
        "# qpsk-params sr={} carrier={} c={} q={} vr={}/{}\nindex,frame_id,emit_ts_ns\n",
        p.sample_rate, p.carrier_hz, p.c, p.q, p.vr_num, p.vr_den
    )
}

/// One marker-log CSV data row `index,frame_id,emit_ts_ns\n`. Split out of
/// `serialize_qpsk_marker_log` (#431) — the SAME row format used both by the final full-log write
/// (`run.rs`, on shutdown) and the probe-gated incremental per-emit append
/// (`src/probe/qpsk_emit.rs`), so the two paths can never drift on the row shape
/// `recording-verdict --av-sync` parses.
pub fn qpsk_marker_log_row(idx: u8, fid: u32, ts: i64) -> String {
    format!("{idx},{fid},{ts}\n")
}

/// Serialize the emitter's marker log as CSV `index,frame_id,emit_ts_ns`, one row per marker,
/// prefixed with a `#`-comment recording the emit `AudioParams` — the decoder MUST demodulate with
/// the same params, and persisting them in the log makes a silent emitter/decoder drift visible
/// (the two sides otherwise hardcode `rig60()` independently). recording-verdict reads this to map
/// a decoded audio `index` → the dual-QR `frame_id` shown when it played, then to that frame's
/// video time. Pure so the round-trip is Tier-0 tested.
pub fn serialize_qpsk_marker_log(markers: &[(u8, u32, i64)], p: &AudioParams) -> String {
    let mut s = qpsk_marker_log_header(p);
    for (idx, fid, ts) in markers {
        s.push_str(&qpsk_marker_log_row(*idx, *fid, *ts));
    }
    s
}

/// Parse `serialize_qpsk_marker_log` output back to `(index, frame_id, emit_ts_ns)`. The header
/// row, `#` comments, and any blank/malformed line are skipped (robust to a truncated file).
pub fn parse_qpsk_marker_log(csv: &str) -> Vec<(u8, u32, i64)> {
    let mut out = Vec::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("index") {
            continue;
        }
        let mut it = line.split(',');
        if let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) {
            if let (Ok(idx), Ok(fid), Ok(ts)) = (
                a.trim().parse::<u8>(),
                b.trim().parse::<u32>(),
                c.trim().parse::<i64>(),
            ) {
                out.push((idx, fid, ts));
            }
        }
    }
    out
}

/// (min, max) of an iterator of `u32` ticks/frame_ids, or `None` if the iterator was empty.
/// Private helper shared by [`marker_coverage_overlaps_video_ticks`] and
/// [`marker_coverage_gap_message`] so the two never compute the range differently.
fn frame_id_range(ids: impl Iterator<Item = u32>) -> Option<(u32, u32)> {
    let mut it = ids;
    let first = it.next()?;
    let (mut lo, mut hi) = (first, first);
    for v in it {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    Some((lo, hi))
}

/// #936 FAIL-CLOSED GUARD: does the marker emit-log's `frame_id` coverage overlap the recording's
/// own DECODED VIDEO tick range?
///
/// `frame_id` is a single monotonically-increasing counter for the lifetime of ONE painter
/// session (`probe::painter::vernier_ids` on the probe-gated side) — the emit-log's `frame_id`
/// column is stamped from that SAME running session's `current_id` at the moment each marker
/// fires, and a recording's decoded video `tick` (== the painted dual-QR `frame_id` shown on
/// screen) is read straight off the video, entirely independent of whether the marker thread is
/// alive. So if the marker thread died a while before (or the log/recording pair is simply
/// wrong), the emit-log's `frame_id` range and the recording's video tick range are two DISJOINT
/// slices of that same counter — no wall-clock alignment between the two machines is needed at
/// all: this overlap check alone proves whether the emit-log's coverage actually spans the moment
/// the recording captured.
///
/// Live incident (#936): a marker log covering frame_ids roughly `[0, 13_500]` (a session that
/// died after ~225s at 60fps) was still paired — via decoded AUDIO INDEX alone, which this
/// function does NOT look at — against a recording whose video tick range was roughly
/// `[97_200, 108_000]` (started ~27 minutes into the SAME session, 24 minutes after the marker
/// died). The existing plausibility thresholds (`matched >= 8`, `mad <= 20ms`) both passed on
/// pure CRC-4 false-decode scatter. This function returns `false` for exactly that shape —
/// disjoint ranges — so the caller refuses to measure instead of reporting a meaningless number.
///
/// Empty `emit_log` or `video_ticks` never overlaps (there is nothing to overlap with).
pub fn marker_coverage_overlaps_video_ticks(
    emit_log: &[(u8, u32, i64)],
    video_ticks: &[(u32, f64)],
) -> bool {
    let Some((e_lo, e_hi)) = frame_id_range(emit_log.iter().map(|(_, fid, _)| *fid)) else {
        return false;
    };
    let Some((v_lo, v_hi)) = frame_id_range(video_ticks.iter().map(|(t, _)| *t)) else {
        return false;
    };
    e_lo <= v_hi && v_lo <= e_hi
}

/// Render a `frame_id_range` result for a human-readable message: `[lo, hi]`, or a plain "no
/// ticks decoded at all" phrase for `None` — a bare `{:?}` of `None` read as an odd dangling word
/// ("...tick range None.") in [`marker_coverage_gap_message`]'s prose.
fn format_frame_id_range(r: Option<(u32, u32)>) -> String {
    match r {
        Some((lo, hi)) => format!("[{lo}, {hi}]"),
        None => "none (no ticks decoded at all)".to_string(),
    }
}

/// #936: the loud, self-diagnosing message `recording-verdict --av-sync` bails with when
/// [`marker_coverage_overlaps_video_ticks`] returns `false`. Pure string formatting so the wording
/// (and the actual computed ranges) is unit-tested here.
pub fn marker_coverage_gap_message(
    emit_log: &[(u8, u32, i64)],
    video_ticks: &[(u32, f64)],
) -> String {
    let emit_range = format_frame_id_range(frame_id_range(emit_log.iter().map(|(_, fid, _)| *fid)));
    let video_range = format_frame_id_range(frame_id_range(video_ticks.iter().map(|(t, _)| *t)));
    format!(
        "REFUSING to measure A/V-sync (#936 fail-closed guard): the marker emit-log's frame_id \
         coverage {emit_range} does NOT overlap the recording's own decoded video tick range \
         {video_range}. The marker thread most likely died (or the wrong emit-log/recording \
         pair was given) before or after this recording — pairing by decoded audio index alone \
         would otherwise produce a plausible-looking but MEANINGLESS offset from CRC-4 false \
         decodes (live incident: av_offset_ms=-544.8 matched=41 mad_ms=12.0, all against a marker \
         log that had already gone silent 24 minutes earlier). Re-fetch a marker log that was \
         actually alive during this recording."
    )
}

/// #936: the CRITICAL message `probe::run::run_paint_only` bails with when the QPSK marker emit
/// thread (`probe::qpsk_emit::QpskEmitter`) returns before the run's normal shutdown (`stop` was
/// requested) — e.g. an unrecoverable ALSA write failure. Pure string formatting so the wording is
/// unit-tested here; the actual detection (comparing the emitter's death-reason cell against
/// `stop`) lives in the probe-gated caller (no local test path — see CLAUDE.md).
///
/// Live incident (#936): three independent painter sessions on the SAME night died silently
/// mid-run (one after as little as 18s of a 220s+ session), each time leaving the painter itself
/// running fine while the marker log simply stopped growing — nothing failed loudly, so a LATER
/// recording measured against the resulting dead coverage window (see
/// [`marker_coverage_overlaps_video_ticks`]) still produced a plausible-looking verdict.
pub fn qpsk_marker_died_message(reason: &str) -> String {
    format!(
        "CRITICAL #936: QPSK A/V-sync marker thread DIED before the run's normal shutdown \
         (reason: {reason}) — the marker log stopped growing while the painter kept painting, \
         which would silently leave a LATER recording measured against a dead (non-emitting) \
         coverage window. Failing the whole --paint-only run now instead of completing the \
         configured duration on a truncated marker log."
    )
}

/// A/V offset estimate from index-paired (video_ts, audio_ts) samples (salvaged from the scrapped
/// chirp `av_sync`; the QPSK path pairs by exact decoded index, so no timing search is needed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvOffset {
    /// median(video_ts − audio_ts) in ms; > 0 = video LAGS audio (reduce genlock delay).
    pub offset_ms: f64,
    /// number of index-paired markers used.
    pub matched: usize,
    /// median absolute deviation (ms) — spread / confidence of the estimate.
    pub mad_ms: f64,
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Every candidate A/V offset (ms) from index-matching each decoded audio marker to its emit-log
/// frame_id(s) and interpolating that frame's video time. Robust to false audio decodes AND to the
/// index WRAP (an index can recur every 256 markers): an audio marker is matched to EVERY emit row
/// carrying its index (usually one; more only across laps), each producing one candidate. Real
/// markers land in a tight `video − audio` cluster; false decodes and wrong-lap matches scatter
/// across the recording. `cluster_offset_ms` then picks the dense cluster. This replaces the old
/// lockstep `pair_audio_to_frameids` walk, which a burst of false decodes (CRC-4 is only 4 bits, so
/// a sliding scan of a music-laden mix passes it ~1/16) could desync — advancing its monotonic
/// pointer past every real marker and yielding zero pairs (observed live: 1691 audio decodes, 0
/// paired). Cluster selection needs no assumption about how many decodes are real.
pub fn av_offset_candidates(
    emit_log: &[(u8, u32, i64)],
    audio: &[(f64, u8)],
    ticks: &[(u32, f64)],
) -> Vec<f64> {
    av_offset_candidates_with_fid(emit_log, audio, ticks)
        .into_iter()
        .map(|(off, _fid, _ts)| off)
        .collect()
}

/// #733 — [`av_offset_candidates`], but each candidate also carries the emit-log `frame_id` and
/// audio decode timestamp that produced it, so a caller can detect and collapse near-simultaneous
/// duplicate detections of the SAME marker before clustering (see
/// [`dedupe_same_fid_candidates`]). `av_offset_candidates` above is re-expressed in terms of this
/// function — identical matching, identical order, identical values; zero behavior change to its
/// existing callers/tests.
pub fn av_offset_candidates_with_fid(
    emit_log: &[(u8, u32, i64)],
    audio: &[(f64, u8)],
    ticks: &[(u32, f64)],
) -> Vec<(f64, u32, f64)> {
    let mut by_idx: HashMap<u8, Vec<u32>> = HashMap::new();
    for &(idx, fid, _) in emit_log {
        by_idx.entry(idx).or_default().push(fid);
    }
    let mut cand = Vec::new();
    for &(ts, idx) in audio {
        if let Some(fids) = by_idx.get(&idx) {
            for &fid in fids {
                if let Some(vts) = interp_video_ts(ticks, fid) {
                    cand.push(((vts - ts) * 1000.0, fid, ts));
                }
            }
        }
    }
    cand
}

/// #733 — evidence-based default window (seconds) for [`dedupe_same_fid_candidates`] /
/// [`av_offset_candidates_deduped`]. The real-data audit (2026-07-13, 3 full-path-e2e gate runs)
/// found duplicate same-fid detection gaps of 37-84ms; 0.2s gives comfortable margin on both
/// sides while staying well under the ~3s marker cadence (`AUDIO_MARKER_CADENCE_TICKS`), so a
/// genuinely separate re-emission that happens to share a wrapped index (only possible on a
/// recording long enough to wrap past 256 markers) is never mistakenly merged.
pub const DEDUPE_SAME_FID_WINDOW_S: f64 = 0.2;

/// #733 — collapse near-simultaneous duplicate decodes of the SAME emitted marker (identical
/// `frame_id`) into ONE candidate, keeping only the EARLIEST audio timestamp.
///
/// Real-data audit (2026-07-13): the winning cluster of a whole-recording cam2 A/V-sync
/// measurement sometimes contains TWO candidates sharing the identical `frame_id`, decoded 37-84ms
/// apart — algebraically, since `offset = (video_ts - audio_ts) * 1000` and `video_ts` is FIXED
/// for a given `frame_id`, the two candidates differ by EXACTLY the audio-ts gap. These are not two
/// independent measurements of the true offset: they are the SAME physical marker occurrence
/// detected twice (plausibly an acoustic echo/reverb tail off the mbc mastering chain the marker's
/// audio physically rides, or `decode_markers`'s own onset search re-triggering on a nearby but
/// distinct sample position). Treating both as independent, equally-weighted samples inflates the
/// apparent matched count and — because the later duplicate is by construction MORE NEGATIVE
/// (audio arriving later) — systematically pulls the cluster toward a more-negative median/wider
/// MAD whenever a duplicate happens to fall inside the winning window.
///
/// `window_s` bounds how close two same-fid detections' audio timestamps must be to be treated as
/// one event (see [`DEDUPE_SAME_FID_WINDOW_S`] for the calibrated default). Detections are merged
/// TRANSITIVELY along a chain (each within `window_s` of its immediate predecessor, not
/// necessarily of the very first) — a burst of 3+ re-triggers on one marker still collapses to a
/// single kept sample. Output order is NOT the input order (grouping is unordered internally);
/// sort the result if a deterministic ordering is needed.
pub fn dedupe_same_fid_candidates(
    candidates_with_fid: &[(f64, u32, f64)],
    window_s: f64,
) -> Vec<f64> {
    let mut by_fid: HashMap<u32, Vec<(f64, f64)>> = HashMap::new(); // fid -> [(ts, offset_ms)]
    for &(off, fid, ts) in candidates_with_fid {
        by_fid.entry(fid).or_default().push((ts, off));
    }
    let mut kept = Vec::new();
    for (_fid, mut items) in by_fid {
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut chain_ts: Option<f64> = None;
        for (ts, off) in items {
            match chain_ts {
                Some(prev_ts) if ts - prev_ts <= window_s => {
                    // Within window_s of the PREVIOUS detection in this fid's chain — same event,
                    // already kept the earliest; advance the chain anchor so a 3rd+ detection
                    // chains transitively off its immediate predecessor.
                    chain_ts = Some(ts);
                }
                _ => {
                    kept.push(off);
                    chain_ts = Some(ts);
                }
            }
        }
    }
    kept
}

/// #733 — [`av_offset_candidates_with_fid`] + [`dedupe_same_fid_candidates`] in one call: the SAME
/// `av_offset_candidates`-shaped `Vec<f64>` output, now robust to same-fid duplicate detections.
pub fn av_offset_candidates_deduped(
    emit_log: &[(u8, u32, i64)],
    audio: &[(f64, u8)],
    ticks: &[(u32, f64)],
    window_s: f64,
) -> Vec<f64> {
    dedupe_same_fid_candidates(
        &av_offset_candidates_with_fid(emit_log, audio, ticks),
        window_s,
    )
}

/// The robust A/V offset: the median of the DENSEST cluster of candidate offsets (window width
/// `2 * cluster_tol_ms`). The real markers pile into one narrow band (their `video − audio` is a
/// near-constant pipeline delay); false decodes and wrong-lap matches spread thinly across the whole
/// recording, so the densest window is the real one whenever the real markers outnumber the false
/// hits in ANY single band — which holds even at a high false rate (thousands of false hits spread
/// over a ~150 s recording are < 1 per 100 ms band, versus dozens of real markers in one band).
/// Returns the cluster median + MAD + the cluster size (`matched`). `None` if fewer than
/// `min_matched` candidates fall in the densest window.
pub fn cluster_offset_ms(
    candidates: &[f64],
    min_matched: usize,
    cluster_tol_ms: f64,
) -> Option<AvOffset> {
    let need = min_matched.max(1);
    if candidates.len() < need {
        return None;
    }
    let mut s: Vec<f64> = candidates.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let w = 2.0 * cluster_tol_ms;
    // Densest window of width `w` via a two-pointer sweep over the sorted offsets.
    let (mut best_lo, mut best_cnt) = (0usize, 0usize);
    let mut j = 0usize;
    for i in 0..s.len() {
        if j < i {
            j = i;
        }
        while j + 1 < s.len() && s[j + 1] - s[i] <= w {
            j += 1;
        }
        let cnt = j - i + 1;
        if cnt > best_cnt {
            best_cnt = cnt;
            best_lo = i;
        }
    }
    let (lo, hi) = (s[best_lo], s[best_lo] + w);
    let keep: Vec<f64> = s.into_iter().filter(|&x| x >= lo && x <= hi).collect();
    if keep.len() < need {
        return None;
    }
    let offset_ms = median(&mut keep.clone());
    let mut dev: Vec<f64> = keep.iter().map(|x| (x - offset_ms).abs()).collect();
    let mad_ms = median(&mut dev);
    Some(AvOffset {
        offset_ms,
        matched: keep.len(),
        mad_ms,
    })
}

/// The dual-QR frame_id DISPLAYED at the instant a queued marker actually SOUNDS. The continuous
/// ALSA feed keeps the ring nearly full, so a marker enqueued now plays `delay_frames` samples
/// later (~150–170 ms at the rig's 8192-frame ring) — during which the vblank-locked painter
/// advances `delay_frames · fps / sample_rate` frames (~10 @ 60 fps). Pairing the audio marker to
/// the ENQUEUE-instant frame_id (the pre-fix behavior) biases the measured A/V offset by exactly
/// that ring delay; this converts the ALSA delay into painter frames so the logged frame_id is the
/// one on screen when the marker is heard. Negative `delay_frames` (post-underrun readback) clamps
/// to 0 — never pair backwards.
pub fn playout_frame_id(
    fid: u32,
    delay_frames: i64,
    sample_rate: u32,
    vr_num: u32,
    vr_den: u32,
) -> u32 {
    if delay_frames <= 0 || sample_rate == 0 || vr_den == 0 {
        return fid;
    }
    let fps = vr_num as f64 / vr_den as f64;
    let adv = (delay_frames as f64 * fps / sample_rate as f64).round();
    // Both crystal clocks; over a ≤200 ms ring the 48k↔60fps conversion error is microscopic.
    fid.saturating_add(adv as u32)
}

/// #398 LIVE dock pairing: the QPSK marker's 8-bit `index` IS the dual-QR `frame_id`'s low byte at
/// playout time — no side channel needed. Offline (`recording-verdict --av-sync`) pairs via the
/// emit-log CSV's own `(index, frame_id)` rows (`av_offset_candidates`, index-derivation-agnostic —
/// unaffected by this choice); a LIVE receiver (the OBS dock) has no CSV, so the audio index must be
/// a value the video side can independently reproduce. Emitting `frame_id & 0xFF` lets the dock's
/// video decode (our dual-QR carries `frame_id` directly) look up "which recent frame had this low
/// byte" with zero extra plumbing. Safe against wrap because 256 low-byte values ≈ 4.3 s @ 60 fps
/// vastly exceeds the video→decode round-trip (~hundreds of ms).
pub fn frame_id_to_index(frame_id: u32) -> u8 {
    (frame_id & 0xFF) as u8
}

/// Number of slots in the live camera-box video↔audio ring (`cb_video_ts_ns` in
/// `vendor/av-sync-dock/src/sync-test-output.cpp`), keyed on `frame_id_to_index`.
pub const AV_SYNC_RING_SLOTS: u64 = 256;
/// Fixed camera-box painter rate the ring's slot count is defined against — NOT the dock's own
/// capture fps, which can differ (see the final-mixed-60-30 topology).
pub const AV_SYNC_SOURCE_FPS: u64 = 60;
/// One full lap of the ring at [`AV_SYNC_SOURCE_FPS`], in nanoseconds (floor; the sub-nanosecond
/// remainder is irrelevant at millisecond A/V-sync precision). ≈ 4266.667 ms.
pub const AV_SYNC_RING_CYCLE_NS: u64 = AV_SYNC_RING_SLOTS * 1_000_000_000 / AV_SYNC_SOURCE_FPS;

/// #398 review HIGH finding: the live ring keeps only the LATEST video write for a low byte, so by
/// the time a matching audio marker decodes, the stored value can be either the TRUE match (if its
/// video already arrived) or the PREVIOUS lap's write, one whole `cycle_ns` earlier — the expected
/// production regime, since the OBS program VIDEO track carries extra genlock A/V-alignment
/// latency (up to 2000 ms, the genlock `latency_ms` knob) the near-zero-latency QPSK AUDIO track
/// does not. A real A/V offset is always far smaller than half a cycle, so reducing the raw
/// difference modulo `cycle_ns` into `(-cycle_ns/2, +cycle_ns/2]` recovers the true offset
/// regardless of which side leads — no assumption about direction. Mirrors
/// `resolve_ring_lap_offset_ns` (same name) in
/// `vendor/av-sync-dock/src/sync-test-output.cpp::st_raw_audio_decode_data` — keep both in sync if
/// the ring size or source fps ever change.
pub fn resolve_ring_lap_offset_ns(audio_ts_ns: u64, stored_video_ts_ns: u64, cycle_ns: u64) -> i64 {
    debug_assert!(cycle_ns > 0);
    let cycle = cycle_ns as i64;
    let half = cycle / 2;
    let raw = audio_ts_ns as i64 - stored_video_ts_ns as i64;
    // Euclidean modulo always lands in [0, cycle); shift the upper half negative so the result
    // sits in (-half, +half] regardless of whether `raw` was already small or many cycles off.
    let mut r = raw.rem_euclid(cycle);
    if r > half {
        r -= cycle;
    }
    r
}

/// #398 review MEDIUM finding: CRC-4 is only 4 bits, so on real program audio a false accept is
/// roughly 1 in 16 decode attempts (the offline path guards against exactly this with
/// `cluster_offset_ms` above); the live dock previously displayed every raw pass, real or false.
/// Smooth by taking the MEDIAN of resolved offsets within `window_ns` of the latest sample
/// (dropping older ones first) — a single false blip cannot move a multi-sample median far, while
/// the real markers (sharing one near-constant pipeline delay) dominate. Mirrors
/// `cb_smooth_offset_ns` in `vendor/av-sync-dock/src/sync-test-output.cpp` — keep both in sync.
pub fn smoothed_offset_ns(
    history: &mut VecDeque<(u64, i64)>,
    sample_ts_ns: u64,
    sample_offset_ns: i64,
    window_ns: u64,
) -> i64 {
    history.push_back((sample_ts_ns, sample_offset_ns));
    while let Some(&(ts, _)) = history.front() {
        if sample_ts_ns.saturating_sub(ts) > window_ns {
            history.pop_front();
        } else {
            break;
        }
    }
    let mut vals: Vec<f64> = history.iter().map(|&(_, o)| o as f64).collect();
    median(&mut vals) as i64
}

/// `start_time` of a stream as reported by ffprobe (`-show_entries stream=start_time`), seconds.
/// A missing/`N/A`/unparsable value is 0.0 (streams starting at the container origin). Used to put
/// the recording's video and audio timelines on a COMMON origin before pairing — a non-zero
/// per-stream start (mux edit list, encoder priming) would otherwise shift the measured offset
/// silently by the difference.
pub fn parse_ffprobe_start_time(s: &str) -> f64 {
    match s.trim().parse::<f64>() {
        Ok(v) if v.is_finite() => v,
        _ => 0.0, // "N/A" / empty / garbage / non-finite → container origin
    }
}

/// Required genlock video-delay to zero the measured offset. `offset_ms = video − audio`; positive
/// (video lags) → REDUCE the delay. The per-run STEP is clamped to `current_delay_ms +/-
/// max_step_ms` BEFORE the hardware genlock range [3, 2000] ms clamp (#871): a single run may
/// only move the applied latency by a small correction, never the whole raw distance in one
/// step — the incident this fixes moved `genlock_latency_ms_src` 920 -> 1845ms in ONE run off a
/// measurement whose video timebase was corrupted by a delivery defect (#707). A genuinely large
/// real offset now converges over several runs instead of jumping to (or past) the hardware
/// ceiling/floor on a single bad measurement.
///
pub fn required_delay_ms(current_delay_ms: i32, offset_ms: f64, max_step_ms: i32) -> i32 {
    let raw = (current_delay_ms as f64 - offset_ms).round() as i32;
    let lo = current_delay_ms - max_step_ms;
    let hi = current_delay_ms + max_step_ms;
    raw.clamp(lo, hi).clamp(3, 2000)
}

/// Linear-interpolate the video time of a target cam2 `frame_id` from the recorded dual-QR
/// `(tick, video_ts_s)` samples (sorted ascending by tick, first occurrence per tick). video_ts
/// rises monotonically with tick, so interpolation recovers the time of a frame_id the (e.g. 30fps)
/// recording never sampled exactly — better than snapping to the nearest tick (±1 frame). `None` if
/// `fid` is outside the sampled range or there are no samples.
pub fn interp_video_ts(samples: &[(u32, f64)], fid: u32) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    match samples.binary_search_by_key(&fid, |&(t, _)| t) {
        Ok(i) => Some(samples[i].1),
        Err(i) => {
            if i == 0 || i >= samples.len() {
                return None; // out of the sampled range — don't extrapolate
            }
            let (t0, v0) = samples[i - 1];
            let (t1, v1) = samples[i];
            if t1 == t0 {
                return Some(v0);
            }
            let frac = (fid - t0) as f64 / (t1 - t0) as f64;
            Some(v0 + frac * (v1 - v0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interp_video_ts_exact_and_between() {
        let s = [(100u32, 1.0f64), (130, 2.0), (160, 3.0)]; // 30 ticks per 1.0 s
        assert_eq!(interp_video_ts(&s, 100), Some(1.0)); // exact
        assert_eq!(interp_video_ts(&s, 130), Some(2.0)); // exact
        assert_eq!(interp_video_ts(&s, 115), Some(1.5)); // halfway between 100 and 130
        assert_eq!(interp_video_ts(&s, 90), None); // below range
        assert_eq!(interp_video_ts(&s, 200), None); // above range
        assert!(interp_video_ts(&[], 100).is_none());
    }

    #[test]
    fn cluster_offset_recovers_true_offset_under_heavy_false_decodes() {
        // The live #188 failure: ~50 real markers + ~1600 false CRC-4 decodes on a music mix. The
        // real markers share a constant video−audio offset (+300 ms here); false decodes carry
        // arbitrary indices at arbitrary times, so their candidate offsets scatter across the whole
        // recording. cluster_offset_ms must still recover +300 ms — the OLD lockstep pair walk
        // returned 0 pairs on exactly this input.
        //
        // Emit log: 60 markers, index i (no wrap) at frame_id 100 + 10*i, emitted every 3 s.
        let mut emit = Vec::new();
        let mut ticks = Vec::new();
        for i in 0..60u32 {
            let fid = 100 + 10 * i;
            emit.push((i as u8, fid, 0i64));
            // video tick fid shown at video_ts = 3*i + 1.0 s (a clean monotonic mapping).
            ticks.push((fid, 3.0 * i as f64 + 1.0));
        }
        // Real audio markers: index i heard 300 ms BEFORE its video frame → offset video−audio=+300.
        let mut audio: Vec<(f64, u8)> = (0..60u32)
            .map(|i| (3.0 * i as f64 + 1.0 - 0.300, i as u8))
            .collect();
        // 1600 false decodes: deterministic pseudo-random index (0..60, in-range so they DO match an
        // emit row) at times spread across the 0..180 s recording. Their offsets scatter widely.
        for k in 0..1600u32 {
            let idx = ((k * 37) % 60) as u8;
            let ts = (k as f64 * 179.0) / 1600.0; // 0..179 s, unrelated to the index
            audio.push((ts, idx));
        }
        let cand = av_offset_candidates(&emit, &audio, &ticks);
        // Every real marker + every in-range false decode forms a candidate.
        assert!(cand.len() >= 60 + 1600, "candidates {}", cand.len());
        let e = cluster_offset_ms(&cand, 20, 50.0).expect("cluster found");
        assert!(
            (e.offset_ms - 300.0).abs() < 5.0,
            "offset {} (matched {}, mad {})",
            e.offset_ms,
            e.matched,
            e.mad_ms
        );
        // The dense cluster is the ~60 real markers, not the thin false scatter.
        assert!(e.matched >= 50, "cluster too small: {}", e.matched);
        assert!(e.mad_ms < 20.0, "cluster not tight: mad {}", e.mad_ms);
    }

    #[test]
    fn playout_frame_id_compensates_alsa_ring_delay() {
        // The rig bias this locks (live evidence: measured −194.5 ms carried ~160 ms of ring
        // delay): a marker enqueued while frame 100 is on screen SOUNDS delay_frames later —
        // 8192 samples @48 kHz ≈ 170.7 ms ≈ 10.24 painter frames @60 fps → frame 110 is displayed
        // when it is heard. Pairing must use 110, not 100.
        assert_eq!(playout_frame_id(100, 8192, 48_000, 60, 1), 110);
        // Exact half-frame rounds to nearest: 4000 samples = 5.0 frames.
        assert_eq!(playout_frame_id(100, 4000, 48_000, 60, 1), 105);
        // Zero delay → unchanged; negative (post-underrun readback) clamps to 0, never backwards.
        assert_eq!(playout_frame_id(100, 0, 48_000, 60, 1), 100);
        assert_eq!(playout_frame_id(100, -512, 48_000, 60, 1), 100);
        // 30 fps chain: same 8192-sample delay is ~5.12 frames → +5.
        assert_eq!(playout_frame_id(100, 8192, 48_000, 30, 1), 105);
        // Saturates instead of wrapping near u32::MAX.
        assert_eq!(
            playout_frame_id(u32::MAX - 2, 48_000, 48_000, 60, 1),
            u32::MAX
        );
    }

    #[test]
    fn frame_id_to_index_is_the_low_byte() {
        // #398: the LIVE dock derives "which video frame" purely from the audio index, so the
        // mapping must be the trivial, receiver-reproducible one — no hidden rounding/scaling.
        assert_eq!(frame_id_to_index(0), 0);
        assert_eq!(frame_id_to_index(255), 255);
        assert_eq!(frame_id_to_index(256), 0); // wraps exactly at 256
        assert_eq!(frame_id_to_index(0x1FF), 0xFF);
        assert_eq!(frame_id_to_index(0x100), 0);
        assert_eq!(frame_id_to_index(u32::MAX), 0xFF);
    }

    #[test]
    fn ffprobe_start_time_parses_or_defaults_to_zero() {
        assert!((parse_ffprobe_start_time("1.234000") - 1.234).abs() < 1e-9);
        assert!((parse_ffprobe_start_time("0.000000")).abs() < 1e-9);
        assert!((parse_ffprobe_start_time(" 0.021333 \n") - 0.021333).abs() < 1e-9);
        assert_eq!(parse_ffprobe_start_time("N/A"), 0.0);
        assert_eq!(parse_ffprobe_start_time(""), 0.0);
        assert_eq!(parse_ffprobe_start_time("garbage"), 0.0);
    }

    #[test]
    fn cluster_offset_needs_min_matched() {
        // Only 3 real candidates in the densest band → below min_matched(4) → None (no guess).
        let cand = vec![300.0, 301.0, 299.0, -5000.0, 8000.0];
        assert!(cluster_offset_ms(&cand, 4, 50.0).is_none());
        // With the same candidates but min_matched 3, the 3-wide cluster is reported.
        let e = cluster_offset_ms(&cand, 3, 50.0).unwrap();
        assert!((e.offset_ms - 300.0).abs() < 1e-6);
        assert_eq!(e.matched, 3);
    }

    #[test]
    fn candidates_handle_index_wrap_via_multiple_frameids() {
        // index 5 recurs across two laps (frame_ids 105 and 405); a decoded audio marker with index
        // 5 yields BOTH candidates — the wrong-lap one lands far from the cluster and is rejected.
        let emit = vec![(5u8, 105u32, 0i64), (5u8, 405u32, 0i64)];
        let ticks = vec![(105u32, 2.0f64), (405u32, 8.0f64)];
        // Real: heard at 1.7 s (frame 105 at 2.0 s → +300 ms). Both fids produce a candidate.
        let audio = vec![(1.7f64, 5u8)];
        let cand = av_offset_candidates(&emit, &audio, &ticks);
        assert_eq!(cand.len(), 2, "one per matching emit fid");
        // +300 ms (real, fid 105) and +6300 ms (wrong lap, fid 405).
        assert!(cand.iter().any(|&c| (c - 300.0).abs() < 1e-6));
        assert!(cand.iter().any(|&c| (c - 6300.0).abs() < 1e-6));
    }

    // -----------------------------------------------------------------
    // #733 — same-fid duplicate-detection dedup (real-data audit, 2026-07-13)
    // -----------------------------------------------------------------

    #[test]
    fn av_offset_candidates_with_fid_reproduces_av_offset_candidates_exactly() {
        // `av_offset_candidates` is now re-expressed in terms of the richer
        // `av_offset_candidates_with_fid` — same matching, same order, same values. Reuses the
        // index-wrap fixture above as a cross-check that the refactor changed nothing observable.
        let emit = vec![(5u8, 105u32, 0i64), (5u8, 405u32, 0i64)];
        let ticks = vec![(105u32, 2.0f64), (405u32, 8.0f64)];
        let audio = vec![(1.7f64, 5u8)];
        let plain = av_offset_candidates(&emit, &audio, &ticks);
        let with_fid = av_offset_candidates_with_fid(&emit, &audio, &ticks);
        assert_eq!(with_fid.len(), plain.len());
        let plain_from_with_fid: Vec<f64> = with_fid.iter().map(|&(off, _, _)| off).collect();
        assert_eq!(plain_from_with_fid, plain, "same offsets, same order");
    }

    #[test]
    fn dedupe_same_fid_candidates_collapses_a_near_simultaneous_duplicate_to_the_earliest() {
        // Mirrors the REAL live pattern found auditing 2026-07-13's 3 full-path-e2e gate runs
        // (#733): the winning cluster sometimes contains TWO candidates sharing the identical
        // `fid` (e.g. live run 142901047's fid=8667: audio_ts 98.155s -> offset -16.8ms, and
        // audio_ts 98.221s, 66ms later -> offset -82.6ms — the SAME video reference point, so the
        // two offsets differ by exactly the audio-ts gap). Only the EARLIER (direct-path) sample
        // should survive; the later one (likely an acoustic echo / reverb tail off the mbc
        // mastering chain, or the decoder's own onset search re-triggering) is dropped.
        let cand = vec![
            (-16.8, 8667u32, 98.155),
            (-82.6, 8667u32, 98.221), // 66ms after the first — same fid, gets collapsed
            (10.0, 9929u32, 119.198), // unrelated real candidate, different fid — untouched
        ];
        let deduped = dedupe_same_fid_candidates(&cand, 0.2);
        assert_eq!(
            deduped.len(),
            2,
            "the 66ms-apart same-fid pair collapses to one; the unrelated fid survives"
        );
        assert!(
            deduped.iter().any(|&o| (o - (-16.8)).abs() < 1e-9),
            "kept the EARLIER (direct-path) sample of the duplicate pair: {deduped:?}"
        );
        assert!(
            !deduped.iter().any(|&o| (o - (-82.6)).abs() < 1e-9),
            "dropped the LATER (echo-like) sample of the duplicate pair: {deduped:?}"
        );
        assert!(
            deduped.iter().any(|&o| (o - 10.0).abs() < 1e-9),
            "the unrelated different-fid candidate is untouched: {deduped:?}"
        );
    }

    #[test]
    fn dedupe_same_fid_candidates_does_not_merge_a_same_fid_pair_far_apart_in_time() {
        // A genuinely SEPARATE re-emission sharing a wrapped index (only possible on a recording
        // long enough to wrap past 256 markers) must NOT be treated as a duplicate — the window_s
        // bound (well under the ~3s marker cadence) protects this.
        let cand = vec![(300.0, 42u32, 5.0), (-150.0, 42u32, 800.0)]; // ~795s apart
        let deduped = dedupe_same_fid_candidates(&cand, 0.2);
        assert_eq!(
            deduped.len(),
            2,
            "far-apart same-fid entries are NOT merged"
        );
    }

    #[test]
    fn dedupe_same_fid_candidates_chains_three_close_detections_into_one() {
        // A chain of 3 detections each within window_s of its PREDECESSOR (not necessarily of the
        // first) still collapses to a single kept (earliest) sample — the merge must not require
        // every pair to be mutually within window_s of the very first timestamp.
        let cand = vec![
            (0.0, 1u32, 10.0),
            (-10.0, 1u32, 10.15),
            (-20.0, 1u32, 10.30),
        ];
        let deduped = dedupe_same_fid_candidates(&cand, 0.2);
        assert_eq!(
            deduped,
            vec![0.0],
            "chained duplicates collapse to the earliest"
        );
    }

    #[test]
    fn av_offset_candidates_deduped_matches_the_real_143901047_pattern_end_to_end() {
        // End-to-end: emit_log/audio/ticks shaped like the real fid=8667 duplicate (#733) alongside
        // a handful of other real markers plus scattered false decodes — cluster_offset_ms on the
        // deduped candidates must (a) still recover the real cluster's offset and (b) report FEWER
        // matched samples than the raw (non-deduped) path, since the inflated duplicate no longer
        // counts twice.
        let mut emit = Vec::new();
        let mut ticks = Vec::new();
        for i in 0..20u32 {
            let fid = 1000 + 100 * i;
            emit.push((i as u8, fid, 0i64));
            ticks.push((fid, 10.0 * i as f64));
        }
        let mut audio: Vec<(f64, u8)> = Vec::new();
        for i in 0..20u32 {
            // video_ts − audio_ts = −20ms ⇒ video LEADS audio by 20ms (this project's own sign
            // convention: "> 0 = video LAGS audio").
            audio.push((10.0 * i as f64 + 0.020, i as u8));
        }
        // Duplicate the marker at index 8 (mirrors the live fid=8667 pattern): a second decode of
        // the SAME index 15ms after the first — close enough to still land inside a ±25ms cluster
        // window (matching the live 37-84ms gaps at this project's OLD ±60ms/120ms-wide default),
        // but well within the DEDUPE_SAME_FID_WINDOW_S(0.2s) merge window.
        audio.push((10.0 * 8.0 + 0.020 + 0.015, 8u8));
        // False decodes parked far outside the real cluster's time range so they can never
        // accidentally land near the real -20ms cluster (deterministic, no flakiness).
        for k in 0..50u32 {
            let idx = (k % 20) as u8;
            audio.push((100_000.0 + k as f64 * 3.7, idx));
        }
        let raw = av_offset_candidates(&emit, &audio, &ticks);
        let deduped = av_offset_candidates_deduped(&emit, &audio, &ticks, DEDUPE_SAME_FID_WINDOW_S);
        assert!(
            deduped.len() < raw.len(),
            "dedup must strictly reduce the candidate count when a duplicate is present: raw={} deduped={}",
            raw.len(),
            deduped.len()
        );
        let raw_cluster = cluster_offset_ms(&raw, 8, 25.0).expect("raw clusters");
        let deduped_cluster = cluster_offset_ms(&deduped, 8, 25.0).expect("deduped clusters");
        assert!(
            (raw_cluster.offset_ms - (-20.0)).abs() < 1.0,
            "raw still recovers ~-20ms (median is robust to one outlier): {}",
            raw_cluster.offset_ms
        );
        assert!(
            (deduped_cluster.offset_ms - (-20.0)).abs() < 1.0,
            "deduped still recovers ~-20ms: {}",
            deduped_cluster.offset_ms
        );
        assert!(
            deduped_cluster.matched < raw_cluster.matched,
            "deduped cluster has fewer (non-inflated) matched samples: raw={} deduped={}",
            raw_cluster.matched,
            deduped_cluster.matched
        );
    }

    #[test]
    fn required_delay_sign_and_clamp() {
        // Within the per-run step budget (50ms) -- pure sign + rounding, unaffected by the #871
        // step clamp.
        assert_eq!(required_delay_ms(1000, 30.0, 50), 970); // video lags → reduce
        assert_eq!(required_delay_ms(1000, -30.0, 50), 1030); // video leads → increase
        assert_eq!(required_delay_ms(1000, 0.6, 50), 999); // rounds to nearest int
                                                           // The hardware [3, 2000] floor/ceiling still applies AFTER the step clamp, when the
                                                           // current value is already close enough to the edge that even one clamped step overshoots
                                                           // it (current near the floor/ceiling, not near 1000 -- a clamped step alone would NOT
                                                           // reach these edges from 1000).
        assert_eq!(required_delay_ms(10, 5000.0, 50), 3); // clamp low
        assert_eq!(required_delay_ms(1990, -5000.0, 50), 2000); // clamp high
    }

    #[test]
    fn required_delay_ms_step_clamp_871() {
        // #871: a single run may only move the applied latency by +/- max_step_ms, never jump
        // the whole raw distance in one step -- the incident this fixes moved
        // `genlock_latency_ms_src` 920 -> 1845ms (a ~925ms single-run correction) off a
        // measurement whose video timebase was corrupted by a delivery defect (#707).
        assert_eq!(required_delay_ms(1000, 925.0, 50), 950); // not 75 -- clamped to current-50
        assert_eq!(required_delay_ms(1000, -925.0, 50), 1050); // not 1925 -- clamped to current+50
        assert_eq!(required_delay_ms(1000, 925.0, 10), 990); // a tighter step budget clamps tighter
    }

    #[test]
    fn qpsk_marker_log_round_trips() {
        let log = vec![
            (0u8, 123u32, 1_000_000i64),
            (200, 456, -42),
            (255, 4_000_000, 9_999_999_999),
        ];
        let serialized = serialize_qpsk_marker_log(&log, &AudioParams::rig60());
        // the params comment is present and skipped by the parser
        assert!(serialized.starts_with("# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\n"));
        let parsed = parse_qpsk_marker_log(&serialized);
        assert_eq!(parsed, log);
        // header-only / blank input → empty, no panic
        assert!(parse_qpsk_marker_log("index,frame_id,emit_ts_ns\n\n").is_empty());
    }

    /// #431: `qpsk_marker_log_header` + `qpsk_marker_log_row` (used by the probe-gated incremental
    /// per-emit writer) must reassemble to EXACTLY the same bytes as `serialize_qpsk_marker_log` —
    /// the two writers (incremental during the run, full on shutdown) must never drift on format.
    #[test]
    fn header_plus_rows_reassembles_to_full_serialization() {
        let log = vec![(0u8, 123u32, 1_000_000i64), (7, 42, -1)];
        let params = AudioParams::rig60();
        let mut reassembled = qpsk_marker_log_header(&params);
        for (idx, fid, ts) in &log {
            reassembled.push_str(&qpsk_marker_log_row(*idx, *fid, *ts));
        }
        assert_eq!(reassembled, serialize_qpsk_marker_log(&log, &params));
    }

    // #936 — marker_coverage_overlaps_video_ticks / marker_coverage_gap_message /
    // qpsk_marker_died_message. RED marker: none of these three exist yet -- proven by compile
    // failure. Implemented in the immediately-following GREEN commit.

    #[test]
    fn marker_coverage_overlap_true_when_ranges_share_a_frame_id() {
        let emit_log = vec![(0u8, 100u32, 0i64), (1, 13_500, 225_000_000_000)];
        let video_ticks = vec![(13_000u32, 0.0f64), (13_600, 1.0)];
        assert!(marker_coverage_overlaps_video_ticks(
            &emit_log,
            &video_ticks
        ));
    }

    #[test]
    fn marker_coverage_overlap_false_on_the_live_936_incident_shape() {
        // Painter F (#936): marker log covered frame_ids ~[0, 13_500] (died after ~225s @ 60fps);
        // the recording's video tick range was ~[97_200, 108_000] (started ~27 min into the same
        // session, 24 min after the marker died) -- two disjoint slices of the SAME counter.
        let emit_log = vec![(0u8, 0u32, 0i64), (1, 13_500, 225_000_000_000)];
        let video_ticks = vec![(97_200u32, 1620.0f64), (108_000, 1800.0)];
        assert!(!marker_coverage_overlaps_video_ticks(
            &emit_log,
            &video_ticks
        ));
    }

    #[test]
    fn marker_coverage_overlap_touching_boundaries_count_as_overlap() {
        // Inclusive boundary: emit range ends exactly where the video range begins.
        let emit_log = vec![(0u8, 0u32, 0i64), (1, 1000, 1)];
        let video_ticks = vec![(1000u32, 0.0f64), (1100, 1.0)];
        assert!(marker_coverage_overlaps_video_ticks(
            &emit_log,
            &video_ticks
        ));
    }

    #[test]
    fn marker_coverage_overlap_empty_video_ticks_never_overlaps() {
        let emit_log = vec![(0u8, 0u32, 0i64)];
        assert!(!marker_coverage_overlaps_video_ticks(&emit_log, &[]));
    }

    #[test]
    fn marker_coverage_overlap_empty_emit_log_never_overlaps() {
        let video_ticks = vec![(0u32, 0.0f64)];
        assert!(!marker_coverage_overlaps_video_ticks(&[], &video_ticks));
    }

    #[test]
    fn marker_coverage_gap_message_carries_the_ranges_and_936() {
        let emit_log = vec![(0u8, 0u32, 0i64), (1, 13_500, 1)];
        let video_ticks = vec![(97_200u32, 0.0f64), (108_000, 1.0)];
        let msg = marker_coverage_gap_message(&emit_log, &video_ticks);
        assert!(msg.contains("#936"));
        assert!(msg.contains("REFUSING"));
        assert!(msg.contains("0"));
        assert!(msg.contains("13500"));
        assert!(msg.contains("97200"));
        assert!(msg.contains("108000"));
    }

    #[test]
    fn marker_coverage_gap_message_reads_cleanly_when_no_video_ticks_decoded_at_all() {
        // Review follow-up (#936): a bare `{:?}` of `None` used to read as a dangling word
        // ("...tick range None."). No video ticks decoded is a real case (the recording's dual-QR
        // never decoded at all) and must read as a plain sentence, not leak Rust's Option syntax.
        let emit_log = vec![(0u8, 0u32, 0i64), (1, 13_500, 1)];
        let msg = marker_coverage_gap_message(&emit_log, &[]);
        assert!(
            !msg.contains("None"),
            "must not leak Option's Debug syntax: {msg}"
        );
        assert!(msg.contains("no ticks decoded"));
        assert!(
            msg.contains("[0, 13500]"),
            "emit range still renders numerically: {msg}"
        );
    }

    #[test]
    fn qpsk_marker_died_message_carries_the_reason_and_936() {
        let msg = qpsk_marker_died_message("ALSA writei after recover: Broken pipe");
        assert!(msg.contains("#936"));
        assert!(msg.contains("CRITICAL"));
        assert!(msg.contains("ALSA writei after recover: Broken pipe"));
        assert!(msg.to_lowercase().contains("marker"));
    }

    #[test]
    fn crc4_roundtrip_matches_norihiro() {
        // Encoder (crc4) and decoder-check (crc4_check) must agree byte-for-byte with norihiro,
        // else the dock rejects our marker.
        for i in 0u8..=255 {
            assert_eq!(
                crc4_check(payload_word(i), N_PAYLOAD_BITS),
                0,
                "crc4 residual != 0 for index {i}"
            );
        }
    }

    #[test]
    fn preamble_symbols_are_neg_sin_neg_sin() {
        for i in [0u8, 1, 42, 255] {
            let s = symbols(payload_word(i));
            assert_eq!([s[0], s[1]], [3, 3], "preamble wrong for index {i}");
        }
    }

    #[test]
    fn auto_c_matches_python_floor_div() {
        assert_eq!(auto_c(2, 442, 60, 1), 1);
        assert_eq!(auto_c(2, 442, 30, 1), 2);
    }

    #[test]
    fn qr_text_exact_format() {
        assert_eq!(
            qr_text(33, 7, 442, 1, 0, 256),
            "q=33,i=7,f=442,c=1,t=0,I=256"
        );
    }

    #[test]
    fn index_field_recoverable_from_payload() {
        // norihiro layout: data16 = 0xF000 | index ⇒ word = data16<<4 | crc ⇒ index at word[11:4].
        for i in 0u8..=255 {
            assert_eq!(
                ((payload_word(i) >> 4) & 0xFF) as u8,
                i,
                "index field wrong for {i}"
            );
        }
    }

    #[test]
    fn stereo_i16_is_clamped_and_duplicated() {
        let mono = [0.0f32, 1.0, -1.0, 2.0, -2.0];
        let st = to_stereo_i16(&mono, 0.8);
        assert_eq!(st.len(), mono.len() * 2);
        for k in 0..mono.len() {
            assert_eq!(st[2 * k], st[2 * k + 1], "L==R at {k}");
        }
        // clamped within i16 even for out-of-range input
        assert!(st.iter().all(|&v| (i16::MIN..=i16::MAX).contains(&v)));
        // +1.0 * 0.8 * 32767 ≈ 26214
        assert!((st[2] - 26214).abs() <= 2);
    }

    #[test]
    fn round_trip_decode_recovers_index_and_time() {
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let mut buf = vec![0.0f32; sr * 20]; // 20 s
        let markers = [(1.0f64, 5u8), (6.0, 42), (11.0, 200)];
        for &(t, idx) in &markers {
            let sig = marker_signal(idx, &p);
            let start = (t * sr as f64) as usize;
            for (j, &s) in sig.iter().enumerate() {
                buf[start + j] += s;
            }
        }
        let found = decode_markers(&buf, &p, 0.4);
        assert_eq!(found.len(), markers.len(), "got {found:?}");
        for (k, &(t, idx)) in markers.iter().enumerate() {
            assert_eq!(found[k].1, idx, "index k={k}: {found:?}");
            assert!(
                (found[k].0 - t).abs() < 0.01,
                "time k={k}: {} vs {t}",
                found[k].0
            );
        }
    }

    #[test]
    fn round_trip_all_256_indices() {
        // The encoder↔decoder lock across the WHOLE index space: every index must render, then
        // decode back to itself. Catches any symbol-boundary / derotation edge case (like the
        // index-200 misdecode) for any bit pattern, not just the three sampled above.
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        for idx in 0u8..=255 {
            let mut buf = vec![0.0f32; sr / 2]; // 0.5 s
            let start = sr / 10; // 0.1 s of lead silence
            let sig = marker_signal(idx, &p);
            buf[start..start + sig.len()].copy_from_slice(&sig);
            let found = decode_markers(&buf, &p, 0.4);
            assert_eq!(found.len(), 1, "idx {idx}: got {found:?}");
            assert_eq!(found[0].1, idx, "idx {idx} decoded as {}", found[0].1);
        }
    }

    #[test]
    fn decode_stats_count_one_clean_marker_as_a_single_preamble_screen_and_crc_ok() {
        // #690: a clean, isolated marker screens its preamble exactly once and decodes CRC-clean —
        // the "demod sees the marker and decodes it fine" case (crc_ok>0, crc_fail==0).
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let idx = 77u8;
        let mut buf = vec![0.0f32; sr / 2];
        let start = sr / 10;
        let sig = marker_signal(idx, &p);
        buf[start..start + sig.len()].copy_from_slice(&sig);
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].1, idx);
        assert_eq!(
            stats.crc_ok, 1,
            "one clean marker decodes CRC-ok exactly once: {stats:?}"
        );
        assert!(
            stats.preamble_screens_passed >= stats.crc_ok,
            "every crc_ok decode started from a passed preamble screen: {stats:?}"
        );
        // decode_markers (the thin wrapper) must still return exactly the same markers.
        assert_eq!(decode_markers(&buf, &p, 0.4), found);
    }

    #[test]
    fn decode_stats_count_silence_as_zero_preamble_screens() {
        // #690: no signal at all -> the demod never sees a candidate onset (the "bad audio
        // routing/level" diagnosis — preamble_screens_passed == 0, not just crc_ok == 0).
        let p = AudioParams::rig60();
        let buf = vec![0.0f32; p.sample_rate as usize];
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert!(found.is_empty());
        assert_eq!(stats.preamble_screens_passed, 0, "{stats:?}");
        assert_eq!(stats.crc_ok, 0, "{stats:?}");
        assert_eq!(stats.crc_fail, 0, "{stats:?}");
    }

    #[test]
    fn decode_stats_count_a_corrupted_marker_as_a_screened_crc_failure() {
        // #690: a marker whose payload got corrupted (a real CRC-4-false-accept-adjacent case, or a
        // clipped/garbled capture) still screens its preamble (energy is there) but must NOT decode
        // — the "sees candidates but they're garbage" diagnosis (preamble_screens_passed>0,
        // crc_ok==0, crc_fail>0).
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let idx = 200u8;
        let mut buf = vec![0.0f32; sr / 2];
        let start = sr / 10;
        let mut sig = marker_signal(idx, &p);
        // Negate one WHOLE symbol window (≈1/10th of the signal, well past the 2-symbol preamble)
        // — this flips exactly that symbol's 2 decoded bits (both re/im signs invert under the
        // linear IQ sum), corrupting an index-field bit while leaving the preamble energy (and
        // hence the screen) intact. A partial-sample flip is not enough: the onset-refine search
        // self-corrects small localized corruption (confirmed empirically), so the corruption must
        // span a whole symbol to reliably break the CRC check.
        let sym5_start = sig.len() * 5 / 10;
        let sym5_end = sig.len() * 6 / 10;
        for s in sig[sym5_start..sym5_end].iter_mut() {
            *s = -*s;
        }
        buf[start..start + sig.len()].copy_from_slice(&sig);
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert!(
            found.is_empty(),
            "a corrupted marker must not decode a valid index: {found:?}"
        );
        assert!(
            stats.preamble_screens_passed > 0,
            "the demod must still see the corrupted marker's energy: {stats:?}"
        );
        assert_eq!(stats.crc_ok, 0, "{stats:?}");
        assert!(stats.crc_fail > 0, "{stats:?}");
    }

    #[test]
    fn round_trip_survives_noise_and_gain() {
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let mut buf = vec![0.0f32; sr * 8];
        let idx = 123u8;
        let start = sr * 2;
        let sig = marker_signal(idx, &p);
        for (j, &s) in sig.iter().enumerate() {
            // 0.3 gain + deterministic pseudo-noise (no rng)
            let noise = (((j as f32) * 12.9898).sin() * 43758.545).fract() * 0.1 - 0.05;
            buf[start + j] += s * 0.3 + noise;
        }
        let found = decode_markers(&buf, &p, 0.35);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].1, idx);
        assert!((found[0].0 - 2.0).abs() < 0.01);
    }

    #[test]
    fn ring_cycle_ns_matches_256_frames_at_60fps() {
        // #398: pin the constant used in production — a future edit to the ring size or the
        // assumed source fps must be caught here, not live on the rig.
        assert_eq!(AV_SYNC_RING_SLOTS, 256);
        assert_eq!(AV_SYNC_SOURCE_FPS, 60);
        assert_eq!(AV_SYNC_RING_CYCLE_NS, 256 * 1_000_000_000 / 60);
    }

    #[test]
    fn resolve_ring_lap_offset_recovers_true_offset_when_audio_leads_video_by_one_lap() {
        // #398 review-found bug: the ring only ever holds the LATEST write for a low byte. Once
        // the OBS program VIDEO carries more genlock A/V-align latency than the near-zero-latency
        // QPSK AUDIO (the expected production regime — audio decodes BEFORE its own matching video
        // reaches the dock), the stored slot value is the PREVIOUS lap's frame: one whole
        // 256-frame/60fps cycle (~4266.667 ms) earlier than the true match. A naive
        // `audio_ts - stored_video_ts` lookup then reports an offset off by a full cycle instead of
        // the true, small (sub-2-second) offset.
        let cycle_ns = AV_SYNC_RING_CYCLE_NS;
        let video_ts_true_ns: u64 = 10_000_000_000; // 10s into the recording
        let audio_ts_ns: u64 = video_ts_true_ns - 1_000_000_000; // audio arrives 1s BEFORE its video
        let stored_video_ts_ns = video_ts_true_ns - cycle_ns; // stale: previous lap's write
        let true_offset_ns: i64 = audio_ts_ns as i64 - video_ts_true_ns as i64; // -1_000_000_000

        // Sanity: confirm this scenario actually reproduces the aliasing (naive lookup is off by
        // more than half a cycle) — otherwise the test wouldn't be exercising the bug at all.
        let naive = audio_ts_ns as i64 - stored_video_ts_ns as i64;
        assert!(
            (naive - true_offset_ns).abs() > (cycle_ns as i64) / 2,
            "test setup should reproduce the aliasing: naive={naive} true={true_offset_ns}"
        );

        let resolved = resolve_ring_lap_offset_ns(audio_ts_ns, stored_video_ts_ns, cycle_ns);
        assert_eq!(resolved, true_offset_ns);
    }

    #[test]
    fn resolve_ring_lap_offset_handles_video_leading_audio_too() {
        // The general case cuts both ways: when video arrives BEFORE its audio (the originally
        // assumed regime), the ring already holds the correct, same-lap value — no wrap needed.
        let cycle_ns = AV_SYNC_RING_CYCLE_NS;
        let video_ts_ns: u64 = 5_000_000_000;
        let audio_ts_ns: u64 = video_ts_ns + 300_000_000; // audio 300ms AFTER video (video leads)
        let resolved = resolve_ring_lap_offset_ns(audio_ts_ns, video_ts_ns, cycle_ns);
        assert_eq!(resolved, 300_000_000);
    }

    #[test]
    fn smoothed_offset_ns_rejects_a_single_false_crc4_blip() {
        // #398 review MEDIUM finding: a lone false CRC-4 accept must not move the displayed number
        // to garbage. Feed a tight cluster of "real" samples around +300ms, then one wild false
        // blip; the median must stay near the real cluster, not jump to the blip.
        let mut history = VecDeque::new();
        let window_ns = 1_000_000_000; // 1s
        let mut last = 0i64;
        for i in 0..9u64 {
            last = smoothed_offset_ns(&mut history, i * 100_000_000, 300_000_000, window_ns);
        }
        assert_eq!(last, 300_000_000, "clean cluster should be exact");

        // One wild false decode arrives amid the real cluster.
        last = smoothed_offset_ns(&mut history, 900_000_000, -3_000_000_000, window_ns);
        assert!(
            (last - 300_000_000).abs() < 50_000_000,
            "a single false blip moved the smoothed offset to {last}, expected near +300ms"
        );
    }

    #[test]
    fn smoothed_offset_ns_prunes_samples_outside_the_window() {
        let mut history = VecDeque::new();
        let window_ns = 1_000_000_000; // 1s
        smoothed_offset_ns(&mut history, 0, 1_000_000_000, window_ns); // ancient false blip
        let last = smoothed_offset_ns(&mut history, 2_000_000_000, 300_000_000, window_ns);
        // The ancient sample (2s old, outside the 1s window) must be pruned before the median is
        // taken, else a stale value could keep polluting the display forever.
        assert_eq!(last, 300_000_000);
        assert_eq!(history.len(), 1, "stale sample should have been pruned");
    }

    // -----------------------------------------------------------------
    // #1153 — enforce the always-zero nibble (bits[15:12]) in the accept gate
    // -----------------------------------------------------------------

    #[test]
    fn decode_rejects_crc_valid_word_with_nonzero_zero_nibble_1153() {
        // The emitter ALWAYS sends the zero-nibble (bits[15:12]) == 0 (`payload_word`), but the
        // decoder historically checked only the preamble nibble (0xF) + CRC-4 = 8 bits, leaving the
        // CRC-passing accept-space 16x too large: of the 4096 words that pass preamble+CRC, only
        // 256 are valid markers and 3840 are "poison" (nonzero zero-nibble) decoded from noise. A
        // poison word must be REJECTED. 0xF1002: preamble=0xF, zero-nibble=0x1, crc-4 valid.
        let poison = 0xF1002u32;
        assert_eq!(
            (poison >> 16) & 0xF,
            PREAMBLE_NIBBLE,
            "poison has a valid preamble nibble"
        );
        assert_eq!(
            crc4_check(poison, N_PAYLOAD_BITS),
            0,
            "poison passes CRC-4 (that's the trap)"
        );
        assert_ne!(
            (poison >> 12) & 0xF,
            0,
            "poison has a NONZERO zero-nibble (what makes it fake)"
        );
        let p = AudioParams::rig60();
        let sig = marker_signal_for_word(poison, &p);
        let mut buf = vec![0f32; p.sample_rate as usize / 2]; // 0.5 s
        let start = p.sample_rate as usize / 10; // 0.1 s lead silence
        buf[start..start + sig.len()].copy_from_slice(&sig);
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert!(
            found.is_empty(),
            "poison word must not decode as a marker: {found:?}"
        );
        assert_eq!(
            stats.crc_ok, 0,
            "poison word must not count as crc_ok: {stats:?}"
        );
        // It WAS screened + fully attempted (valid preamble + CRC), then rejected on the zero-nibble.
        assert!(
            stats.crc_fail >= 1,
            "poison word is screened+attempted, then rejected: {stats:?}"
        );
    }

    #[test]
    fn decode_still_accepts_a_real_marker_and_every_index_has_zero_nibble_1153() {
        // The new conjunct must NEVER reject a REAL marker: every valid index's payload word carries
        // a zero nibble == 0 by construction, so the gate is free for all 256 real markers.
        for idx in 0u32..256 {
            assert_eq!(
                (payload_word(idx as u8) >> 12) & 0xF,
                0,
                "index {idx}: a valid marker's zero-nibble must be 0"
            );
        }
        let p = AudioParams::rig60();
        let sig = marker_signal(123, &p);
        let mut buf = vec![0f32; p.sample_rate as usize / 2];
        let start = p.sample_rate as usize / 10;
        buf[start..start + sig.len()].copy_from_slice(&sig);
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert_eq!(found.len(), 1, "a real marker still decodes: {found:?}");
        assert_eq!(found[0].1, 123, "recovers the right index");
        assert_eq!(stats.crc_ok, 1, "real marker: crc_ok == 1: {stats:?}");
    }

    // -----------------------------------------------------------------
    // #1153 (sticky-unlock recovery) — non-finite input samples must not
    // poison the prefix sums for the rest of the window
    // -----------------------------------------------------------------

    #[test]
    fn a_single_non_finite_sample_must_not_poison_the_rest_of_the_window_1153() {
        // The decode kernel builds cos/sin/energy PREFIX SUMS over the whole buffer; a single
        // NaN/Inf sample therefore contaminates EVERY sum after it, silently killing decode for
        // the remainder of the window (every preamble-screen comparison against NaN is false).
        // The live dock feeds this kernel a mono mixdown of the OBS program audio — an upstream
        // in-process poison (a wedged resampler channel emitting non-finite samples) must degrade
        // at most the poisoned samples, never the whole rolling window. Treat non-finite input as
        // silence.
        let p = AudioParams::rig60();
        let sr = p.sample_rate as usize;
        let idx = 42u8;
        let mut buf = vec![0.0f32; sr / 2]; // 0.5 s
        let start = sr / 10; // 0.1 s lead silence
        let sig = marker_signal(idx, &p);
        buf[start..start + sig.len()].copy_from_slice(&sig);
        // Non-finite garbage well BEFORE the marker — prefix sums past these indices would be
        // NaN/Inf without the sanitize, so the marker downstream would never decode.
        buf[10] = f32::NAN;
        buf[11] = f32::INFINITY;
        buf[12] = f32::NEG_INFINITY;
        let (found, stats) = decode_markers_with_stats(&buf, &p, 0.4);
        assert_eq!(
            found.len(),
            1,
            "a marker after a non-finite sample must still decode: {found:?} {stats:?}"
        );
        assert_eq!(found[0].1, idx, "recovers the right index");
        assert_eq!(stats.crc_ok, 1, "sanitized decode counts crc_ok: {stats:?}");

        // Control (must hold before AND after the fix): non-finite garbage strictly AFTER the
        // marker's own windows never affected it — the prefix-sum poison only flows forward.
        let mut buf2 = vec![0.0f32; sr / 2];
        buf2[start..start + sig.len()].copy_from_slice(&sig);
        let tail = start + sig.len() + 100;
        buf2[tail] = f32::NAN;
        let (found2, _) = decode_markers_with_stats(&buf2, &p, 0.4);
        assert_eq!(found2.len(), 1, "trailing garbage never hurt: {found2:?}");
        assert_eq!(found2[0].1, idx);
    }
}
