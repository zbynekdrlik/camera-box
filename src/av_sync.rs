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
}
