//! Pure Tier-0 logic for the cam2 QR-synced audio → A/V-sync calibration (#188/#145).
//!
//! One source of truth for the chirp template (used BOTH by the painter to EMIT and by
//! recording-verdict to DETECT), the cross-correlation onset detector, the offset estimator,
//! and the controller delay math. No I/O, no probe deps — unit-tested on default features.

/// Generate a Hann-windowed linear chirp, peak-normalized to ±1.0.
/// Used as the emitted marker AND the cross-correlation template — one source of truth.
pub fn generate_chirp(sample_rate: u32, dur_ms: u32, f0_hz: f32, f1_hz: f32) -> Vec<f32> {
    let n = (sample_rate as u64 * dur_ms as u64 / 1000) as usize;
    if n == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f32;
    let dur_s = n as f32 / sr;
    let k = (f1_hz - f0_hz) / dur_s; // sweep rate Hz/s
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / sr;
        // instantaneous phase of a linear sweep
        let phase = 2.0 * std::f32::consts::PI * (f0_hz * t + 0.5 * k * t * t);
        // Hann window
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos();
        out.push(phase.sin() * w);
    }
    // peak-normalize to ±1.0
    let peak = out.iter().cloned().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 0.0 {
        for s in &mut out {
            *s /= peak;
        }
    }
    out
}

/// Normalized cross-correlation onset detector.
/// Returns the start-sample index of each template match whose normalized correlation
/// (0..=1) is >= `threshold`, keeping peaks at least `min_spacing` samples apart
/// (highest-correlation-wins within a spacing window).
pub fn detect_chirp_onsets(
    samples: &[f32],
    template: &[f32],
    threshold: f32,
    min_spacing: usize,
) -> Vec<usize> {
    let m = template.len();
    if m == 0 || samples.len() < m {
        return Vec::new();
    }
    let t_energy: f32 = template.iter().map(|x| x * x).sum::<f32>().sqrt();
    if t_energy == 0.0 {
        return Vec::new();
    }
    // normalized cross-correlation at each lag
    let last = samples.len() - m;
    let mut corr = vec![0.0f32; last + 1];
    for lag in 0..=last {
        let win = &samples[lag..lag + m];
        let mut dot = 0.0f32;
        let mut w_energy = 0.0f32;
        for i in 0..m {
            dot += win[i] * template[i];
            w_energy += win[i] * win[i];
        }
        let denom = t_energy * w_energy.sqrt();
        corr[lag] = if denom > 0.0 {
            (dot / denom).abs()
        } else {
            0.0
        };
    }
    // greedy peak pick: strongest first, suppress neighbours within min_spacing
    let mut idx: Vec<usize> = (0..corr.len()).filter(|&i| corr[i] >= threshold).collect();
    idx.sort_by(|&a, &b| corr[b].partial_cmp(&corr[a]).unwrap());
    let mut picked: Vec<usize> = Vec::new();
    for cand in idx {
        if picked
            .iter()
            .all(|&p| (p as i64 - cand as i64).unsigned_abs() as usize >= min_spacing)
        {
            picked.push(cand);
        }
    }
    picked.sort_unstable();
    picked
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmittedMarker {
    pub frame_id: u32,
    pub video_time_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvOffsetEstimate {
    pub offset_ms: f64,
    pub matched: usize,
    pub mad_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct OffsetSearch {
    pub min_ms: f64,
    pub max_ms: f64,
    pub step_ms: f64,
    pub tol_ms: f64,
    pub min_matches: usize,
    pub max_mad_ms: f64,
}

pub fn default_search() -> OffsetSearch {
    OffsetSearch {
        min_ms: -500.0,
        max_ms: 2500.0,
        step_ms: 5.0,
        tol_ms: 60.0,
        min_matches: 4,
        max_mad_ms: 40.0,
    }
}

pub fn estimate_av_offset_ms(
    emitted: &[EmittedMarker],
    onsets_s: &[f64],
    search: &OffsetSearch,
) -> Option<AvOffsetEstimate> {
    if emitted.is_empty() || onsets_s.is_empty() {
        return None;
    }
    let tol_s = search.tol_ms / 1000.0;
    let mut best_d_ms = search.min_ms;
    let mut best_votes = 0usize;

    let steps = ((search.max_ms - search.min_ms) / search.step_ms)
        .round()
        .max(0.0) as i64;
    for k in 0..=steps {
        let d_ms = search.min_ms + k as f64 * search.step_ms;
        let d_s = d_ms / 1000.0;
        let mut votes = 0usize;
        for m in emitted {
            let expected_audio = m.video_time_s - d_s;
            if onsets_s
                .iter()
                .any(|&o| (o - expected_audio).abs() <= tol_s)
            {
                votes += 1;
            }
        }
        if votes > best_votes {
            best_votes = votes;
            best_d_ms = d_ms;
        }
    }
    if best_votes < search.min_matches {
        return None;
    }

    // refine: median of per-marker residuals at the winning D, nearest-onset matched
    let best_d_s = best_d_ms / 1000.0;
    let mut residuals_ms: Vec<f64> = Vec::new();
    for m in emitted {
        let expected_audio = m.video_time_s - best_d_s;
        if let Some(&nearest) = onsets_s.iter().min_by(|a, b| {
            (**a - expected_audio)
                .abs()
                .partial_cmp(&(**b - expected_audio).abs())
                .unwrap()
        }) {
            if (nearest - expected_audio).abs() <= tol_s {
                // actual offset for this marker = video - audio
                residuals_ms.push((m.video_time_s - nearest) * 1000.0);
            }
        }
    }
    if residuals_ms.len() < search.min_matches {
        return None;
    }
    let offset_ms = median(&mut residuals_ms.clone());
    let mut abs_dev: Vec<f64> = residuals_ms.iter().map(|r| (r - offset_ms).abs()).collect();
    let mad_ms = median(&mut abs_dev);
    if mad_ms > search.max_mad_ms {
        return None;
    }
    Some(AvOffsetEstimate {
        offset_ms,
        matched: residuals_ms.len(),
        mad_ms,
    })
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

/// Required genlock video-delay to zero the measured offset.
/// `offset_ms = video_time - audio_time`; positive = video lags audio → REDUCE the delay.
pub fn required_delay_ms(current_delay_ms: i32, offset_ms: f64) -> i32 {
    let raw = (current_delay_ms as f64 - offset_ms).round() as i32;
    raw.clamp(3, 2000)
}

/// Chirp shape parameters (shared between the painter emitter and the
/// recording-verdict onset detector — one source of truth).
#[derive(Debug, Clone, Copy)]
pub struct ChirpParams {
    /// Chirp duration in milliseconds.
    pub dur_ms: u32,
    /// Start frequency in Hz.
    pub f0_hz: f32,
    /// End frequency in Hz.
    pub f1_hz: f32,
}

impl Default for ChirpParams {
    fn default() -> Self {
        Self {
            dur_ms: 50,
            f0_hz: 1000.0,
            f1_hz: 3000.0,
        }
    }
}

/// Returns `true` when the painter should emit a marker on this `refresh_tick`.
/// Fires on every non-zero multiple of `cadence_ticks`; never on tick 0 (the very
/// first frame) and never when `cadence_ticks` is 0 (disabled).
pub fn should_emit_marker(refresh_tick: u64, cadence_ticks: u64) -> bool {
    cadence_ticks > 0 && refresh_tick > 0 && refresh_tick.is_multiple_of(cadence_ticks)
}

/// Serialize `(frame_id, emit_wall_ts_ns)` pairs as a CSV string.
/// Round-trips with [`parse_marker_log`].
pub fn serialize_marker_log(entries: &[(u32, i64)]) -> String {
    let mut s = String::from("frame_id,emit_wall_ts_ns\n");
    for (fid, ts) in entries {
        s.push_str(&format!("{fid},{ts}\n"));
    }
    s
}

/// Parse the frame-id column from a marker-log CSV produced by
/// [`serialize_marker_log`]. Returns the `frame_id` of every data row.
pub fn parse_marker_log(csv: &str) -> Vec<u32> {
    csv.lines()
        .skip(1) // skip header
        .filter_map(|line| {
            let mut cols = line.splitn(2, ',');
            cols.next()?.trim().parse::<u32>().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chirp_is_peak_normalized_and_correct_length() {
        let c = generate_chirp(48_000, 50, 1000.0, 3000.0);
        assert_eq!(c.len(), 48_000 * 50 / 1000); // 2400 samples
        let peak = c.iter().cloned().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 1.0).abs() < 1e-3, "peak {peak} not ~1.0");
        // Hann window → ends taper to ~0
        assert!(c[0].abs() < 0.05 && c[c.len() - 1].abs() < 0.05);
    }

    #[test]
    fn detects_two_chirps_at_known_positions() {
        let sr = 48_000;
        let template = generate_chirp(sr, 50, 1000.0, 3000.0);
        let mut buf = vec![0.0f32; sr as usize * 4]; // 4 s: p2=150_000+template fits (brief had 3 s which overflows)
        let p1 = 20_000usize;
        let p2 = 150_000usize;
        for (i, &s) in template.iter().enumerate() {
            buf[p1 + i] += s * 0.6;
            buf[p2 + i] += s * 0.6;
        }
        let onsets = detect_chirp_onsets(&buf, &template, 0.5, template.len());
        assert_eq!(onsets.len(), 2);
        assert!((onsets[0] as i64 - p1 as i64).abs() <= 5);
        assert!((onsets[1] as i64 - p2 as i64).abs() <= 5);
    }

    #[test]
    fn survives_noise_and_gain() {
        let sr = 48_000;
        let template = generate_chirp(sr, 50, 1000.0, 3000.0);
        let mut buf = vec![0.0f32; sr as usize * 2];
        let p = 44_100usize;
        // low-amplitude marker + additive pseudo-noise (deterministic, no rng)
        for (i, &s) in template.iter().enumerate() {
            let noise = (((i as f32) * 12.9898).sin() * 43758.545).fract() * 0.1 - 0.05;
            buf[p + i] += s * 0.3 + noise;
        }
        let onsets = detect_chirp_onsets(&buf, &template, 0.4, template.len());
        assert_eq!(onsets.len(), 1);
        assert!((onsets[0] as i64 - p as i64).abs() <= 10);
    }

    #[test]
    fn silence_yields_no_onsets() {
        let template = generate_chirp(48_000, 50, 1000.0, 3000.0);
        let buf = vec![0.0f32; 48_000];
        assert!(detect_chirp_onsets(&buf, &template, 0.4, template.len()).is_empty());
    }

    fn markers(times: &[f64]) -> Vec<EmittedMarker> {
        times
            .iter()
            .enumerate()
            .map(|(i, &t)| EmittedMarker {
                frame_id: i as u32,
                video_time_s: t,
            })
            .collect()
    }

    #[test]
    fn recovers_constant_offset() {
        // video at 1,5,9,13,17 s; audio 300 ms EARLIER (video lags by +300 ms)
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        let onsets: Vec<f64> = em.iter().map(|m| m.video_time_s - 0.300).collect();
        let est = estimate_av_offset_ms(&em, &onsets, &default_search()).unwrap();
        assert!(
            (est.offset_ms - 300.0).abs() <= 5.0,
            "offset {}",
            est.offset_ms
        );
        assert_eq!(est.matched, 5);
        assert!(est.mad_ms <= 5.0);
    }

    #[test]
    fn tolerates_one_missed_onset_and_one_spurious() {
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        let mut onsets: Vec<f64> = em.iter().map(|m| m.video_time_s - 0.300).collect();
        onsets.remove(2); // a missed marker
        onsets.push(3.7); // a spurious onset
        let est = estimate_av_offset_ms(&em, &onsets, &default_search()).unwrap();
        assert!((est.offset_ms - 300.0).abs() <= 10.0);
        assert!(est.matched >= 4);
    }

    #[test]
    fn too_few_matches_returns_none() {
        let em = markers(&[1.0, 5.0]);
        let onsets = vec![0.7, 4.7];
        assert!(estimate_av_offset_ms(&em, &onsets, &default_search()).is_none());
    }

    #[test]
    fn inconsistent_markers_return_none() {
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        // onsets scattered — no constant offset aligns >= min_matches within tol
        let onsets = vec![0.1, 4.9, 8.2, 13.9, 15.5];
        assert!(estimate_av_offset_ms(&em, &onsets, &default_search()).is_none());
    }

    #[test]
    fn required_delay_sign_and_clamp() {
        // video lags audio (+120 ms) → reduce delay
        assert_eq!(required_delay_ms(1000, 120.0), 880);
        // video leads audio (-120 ms) → increase delay
        assert_eq!(required_delay_ms(1000, -120.0), 1120);
        // clamp low / high
        assert_eq!(required_delay_ms(1000, 5000.0), 3);
        assert_eq!(required_delay_ms(1000, -5000.0), 2000);
        assert_eq!(required_delay_ms(3, 0.0), 3);
    }

    #[test]
    fn cadence_fires_on_multiples_only() {
        assert!(!should_emit_marker(0, 300));
        assert!(!should_emit_marker(299, 300));
        assert!(should_emit_marker(300, 300));
        assert!(should_emit_marker(600, 300));
        assert!(!should_emit_marker(300, 0)); // disabled
    }

    #[test]
    fn marker_log_round_trips() {
        let e = vec![(300u32, 111i64), (600u32, 222i64)];
        let s = serialize_marker_log(&e);
        assert_eq!(parse_marker_log(&s), vec![300u32, 600u32]);
    }
}
