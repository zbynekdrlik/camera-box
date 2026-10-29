//! Issue 1387: the pure helpers of `jxs-encode-bench` (synthetic content + the percentile line),
//! here so they unit-test without the codec feature.

/// Fill planar 8-bit 4:2:2 planes (`y` = `w x h`, `u`/`v` = `w/2 x h`) with a synthetic picture.
///
/// - `noise`: a diagonal luma gradient + per-pixel sensor-like noise (+-24) and a fine
///   checkerboard texture, chroma ramps + noise (+-12). The worst case for the entropy coder: the
///   codec's cost grows with the coefficients it has to code, and noise keeps them all significant.
/// - `smooth`: the same gradients without noise or texture (the best case).
///
/// Deterministic (a fixed xorshift seed), so two runs code the same frame.
pub fn fill_pattern(
    pattern: &str,
    w: usize,
    h: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> Result<(), String> {
    let noisy = match pattern {
        "noise" => true,
        "smooth" => false,
        other => return Err(format!("unknown --pattern '{other}' (noise | smooth)")),
    };
    let cw = w / 2;
    if y.len() != w * h || u.len() != cw * h || v.len() != cw * h {
        return Err(format!("planes do not match {w}x{h} 4:2:2"));
    }
    let mut state: u32 = 0x1387_2026;
    let mut rnd = |amp: i32| -> i32 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        if amp == 0 {
            0
        } else {
            (state % (2 * amp as u32 + 1)) as i32 - amp
        }
    };
    let (ya, ca) = if noisy { (24, 12) } else { (0, 0) };
    for r in 0..h {
        for c in 0..w {
            let base = 16 + ((c + r) * 219 / (w + h).max(1)) as i32;
            let tex = if noisy && ((c / 2 + r / 2) % 2 == 0) {
                12
            } else {
                0
            };
            y[r * w + c] = (base + tex + rnd(ya)).clamp(16, 235) as u8;
        }
        for c in 0..cw {
            let ub = 64 + (c * 128 / cw.max(1)) as i32;
            let vb = 64 + (r * 128 / h.max(1)) as i32;
            u[r * cw + c] = (ub + rnd(ca)).clamp(16, 240) as u8;
            v[r * cw + c] = (vb + rnd(ca)).clamp(16, 240) as u8;
        }
    }
    Ok(())
}

/// p50 / p99 / max of `v` (nearest rank; sorts `v`). Empty = zeros.
pub fn p50_p99_max(v: &mut [f64]) -> (f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let rank = |q: f64| v[(((v.len() as f64) * q).ceil() as usize).clamp(1, v.len()) - 1];
    (rank(0.50), rank(0.99), v[v.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noise_pattern_is_deterministic_and_noisier_than_smooth() {
        let (w, h) = (64, 8);
        let mk = |p: &str| {
            let (mut y, mut u, mut v) = (vec![0; w * h], vec![0; w / 2 * h], vec![0; w / 2 * h]);
            fill_pattern(p, w, h, &mut y, &mut u, &mut v).unwrap();
            (y, u, v)
        };
        assert_eq!(mk("noise"), mk("noise"), "same frame every run");
        let tv = |y: &[u8]| -> i64 {
            y.windows(2)
                .map(|p| (p[0] as i64 - p[1] as i64).abs())
                .sum()
        };
        assert!(tv(&mk("noise").0) > 2 * tv(&mk("smooth").0));
        let (y, _, _) = mk("smooth");
        assert!(y.iter().all(|&s| (16..=235).contains(&s)), "limited range");
        let (mut y, mut u, mut v) = (vec![0; 10], vec![0; 2], vec![0; 2]);
        assert!(fill_pattern("plaid", 4, 2, &mut y, &mut u, &mut v).is_err());
        assert!(fill_pattern("noise", 4, 2, &mut y, &mut u, &mut v).is_err());
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let mut v: Vec<f64> = (1..=100).map(f64::from).collect();
        v.reverse();
        assert_eq!(p50_p99_max(&mut v), (50.0, 99.0, 100.0));
        assert_eq!(p50_p99_max(&mut [7.0]), (7.0, 7.0, 7.0));
        assert_eq!(p50_p99_max(&mut []), (0.0, 0.0, 0.0));
    }
}
