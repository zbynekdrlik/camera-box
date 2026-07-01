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

/// 20-bit payload word: bits[19:16]=0xF preamble, bits[15:8]=index, bits[7:4]=0, bits[3:0]=CRC4.
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
    let word = payload_word(index);
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

/// Detect QPSK markers in mono f32 audio → `(audio_ts_s at signal start, index)` per marker.
///
/// The norihiro demod (`sync-test-output.cpp::st_raw_audio_decode_data`): IQ-demodulate each symbol
/// against the carrier (`Z = Σ signal·e^{-iθ}`, computed O(1) via prefix sums), derotate by the
/// known preamble phasor (absorbs any carrier-phase / sub-sample-alignment error — the reason plain
/// cross-correlation fails on a carrier), read the two bits per symbol from the derotated real/imag
/// signs, then CRC-gate. A cheap normalized preamble-magnitude screen finds onsets; only CRC-valid
/// candidates with a correctly-decoded 0xF preamble are accepted. `threshold` is the preamble screen
/// (≈0.7 for a clean aligned marker; 0.4 is a robust default). Self-consistent with `marker_signal`.
pub fn decode_markers(samples: &[f32], p: &AudioParams, threshold: f64) -> Vec<(f64, u8)> {
    let ar = p.sample_rate as f64;
    let f = p.carrier_hz as f64;
    let c = p.c.max(1) as f64;
    let sps = ar * c / f; // samples per symbol (fractional)
    let sig_len = signal_len(p);
    let n = samples.len();
    if sig_len == 0 || n < sig_len || sps < 1.0 {
        return Vec::new();
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
            if (word >> 16) & 0xF == PREAMBLE_NIBBLE && crc4_check(word, N_PAYLOAD_BITS) == 0 {
                out.push((base as f64 / ar, ((word >> 4) & 0xFF) as u8));
                i = base + sig_len; // markers are far apart; skip past this one
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Serialize the emitter's marker log as CSV `index,frame_id,emit_ts_ns`, one row per marker.
/// recording-verdict reads this to map a decoded audio `index` → the dual-QR `frame_id` shown when
/// it played, then to that frame's video time. Pure so the round-trip is Tier-0 tested.
pub fn serialize_qpsk_marker_log(markers: &[(u8, u32, i64)]) -> String {
    let mut s = String::from("index,frame_id,emit_ts_ns\n");
    for (idx, fid, ts) in markers {
        s.push_str(&format!("{idx},{fid},{ts}\n"));
    }
    s
}

/// Parse `serialize_qpsk_marker_log` output back to `(index, frame_id, emit_ts_ns)`. The header row
/// and any blank/malformed line are skipped (robust to a truncated file).
pub fn parse_qpsk_marker_log(csv: &str) -> Vec<(u8, u32, i64)> {
    let mut out = Vec::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("index") {
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
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// A/V offset from index-paired `(video_ts_s, audio_ts_s)` markers. The QPSK index pairs each audio
/// marker to its exact dual-QR frame, so each pair yields a direct offset (video − audio); median +
/// MAD reject the odd mis-decode. `None` if fewer than `min_matched` pairs.
pub fn av_offset_ms(pairs: &[(f64, f64)], min_matched: usize) -> Option<AvOffset> {
    if pairs.len() < min_matched.max(1) {
        return None;
    }
    let off: Vec<f64> = pairs.iter().map(|(v, a)| (v - a) * 1000.0).collect();
    let offset_ms = median(&mut off.clone());
    let mut dev: Vec<f64> = off.iter().map(|x| (x - offset_ms).abs()).collect();
    let mad_ms = median(&mut dev);
    Some(AvOffset {
        offset_ms,
        matched: pairs.len(),
        mad_ms,
    })
}

/// Required genlock video-delay to zero the measured offset. `offset_ms = video − audio`; positive
/// (video lags) → REDUCE the delay. Clamped to the genlock range [3, 2000] ms.
pub fn required_delay_ms(current_delay_ms: i32, offset_ms: f64) -> i32 {
    ((current_delay_ms as f64 - offset_ms).round() as i32).clamp(3, 2000)
}

/// Align time-ordered decoded audio markers `(audio_ts_s, index)` to the ordered emit log
/// `(index, frame_id, _)`, returning `(frame_id, audio_ts_s)` per matched marker.
///
/// The QPSK index wraps every 256, so a global index→frame_id map is ambiguous over a long run.
/// Both sequences follow the SAME wrapping 0..255 order (the emitter increments; the recording plays
/// them in time), so we walk them in lockstep: for each audio marker, find the next emit row with a
/// matching index within one lap. This tolerates a DROPPED audio marker (skips its emit row) and a
/// spurious/false audio decode (no match within a lap ⇒ that audio marker is skipped, `j` unmoved).
pub fn pair_audio_to_frameids(emit_log: &[(u8, u32, i64)], audio: &[(f64, u8)]) -> Vec<(u32, f64)> {
    let mut out = Vec::new();
    let mut j = 0usize;
    for &(ts, idx) in audio {
        let end = (j + 300).min(emit_log.len()); // ≤ ~1 lap of look-ahead
        let mut k = j;
        while k < end && emit_log[k].0 != idx {
            k += 1;
        }
        if k < end && emit_log[k].0 == idx {
            out.push((emit_log[k].1, ts));
            j = k + 1;
        }
    }
    out
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
    fn pairs_audio_to_frameids_in_order_with_drops_and_wrap() {
        // emit log: indices 254,255,0,1,2 → frame_ids 1000..1004 (wraps 255→0)
        let emit = vec![
            (254u8, 1000u32, 0i64),
            (255, 1001, 0),
            (0, 1002, 0),
            (1, 1003, 0),
            (2, 1004, 0),
        ];
        // audio: markers 254,255,1,2 in time order (index 0 DROPPED)
        let audio = vec![(1.0f64, 254u8), (6.0, 255), (16.0, 1), (21.0, 2)];
        let pairs = pair_audio_to_frameids(&emit, &audio);
        // dropped index 0 must not misalign the rest
        assert_eq!(
            pairs,
            vec![(1000, 1.0), (1001, 6.0), (1003, 16.0), (1004, 21.0)]
        );
        // a spurious audio index not in the log is skipped without breaking alignment
        let audio2 = vec![(1.0f64, 254u8), (2.0, 99), (6.0, 255)];
        assert_eq!(
            pair_audio_to_frameids(&emit, &audio2),
            vec![(1000, 1.0), (1001, 6.0)]
        );
    }

    #[test]
    fn av_offset_from_index_pairs() {
        // video at 1,5,9,13 s; audio 300 ms earlier each → +300 ms (video lags audio).
        let pairs = [(1.0, 0.7), (5.0, 4.7), (9.0, 8.7), (13.0, 12.7)];
        let e = av_offset_ms(&pairs, 4).unwrap();
        assert!((e.offset_ms - 300.0).abs() < 1e-6, "offset {}", e.offset_ms);
        assert_eq!(e.matched, 4);
        assert!(e.mad_ms < 1e-6);
        // one wild mis-decode must not move the median
        let noisy = [
            (1.0, 0.7),
            (5.0, 4.7),
            (9.0, 8.7),
            (13.0, 12.7),
            (17.0, 0.0),
        ];
        let e2 = av_offset_ms(&noisy, 4).unwrap();
        assert!(
            (e2.offset_ms - 300.0).abs() < 1.0,
            "median moved: {}",
            e2.offset_ms
        );
        assert!(av_offset_ms(&pairs, 5).is_none()); // too few
    }

    #[test]
    fn required_delay_sign_and_clamp() {
        assert_eq!(required_delay_ms(1000, 120.0), 880); // video lags → reduce
        assert_eq!(required_delay_ms(1000, -120.0), 1120); // video leads → increase
        assert_eq!(required_delay_ms(1000, 5000.0), 3); // clamp low
        assert_eq!(required_delay_ms(1000, -5000.0), 2000); // clamp high
    }

    #[test]
    fn qpsk_marker_log_round_trips() {
        let log = vec![
            (0u8, 123u32, 1_000_000i64),
            (200, 456, -42),
            (255, 4_000_000, 9_999_999_999),
        ];
        let parsed = parse_qpsk_marker_log(&serialize_qpsk_marker_log(&log));
        assert_eq!(parsed, log);
        // header-only / blank input → empty, no panic
        assert!(parse_qpsk_marker_log("index,frame_id,emit_ts_ns\n\n").is_empty());
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
}
