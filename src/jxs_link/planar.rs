//! Issue 1387: pixel helpers of the JPEG XS link (pure, allocation-free).

/// Deinterleave a packed YUYV (Y0 U Y1 V) picture into planar 4:2:2: `y` is `width x height`,
/// `u`/`v` are `width/2 x height`. `stride` is the source row length in bytes (>= 2 x width).
/// Returns `false` (and writes nothing) when a buffer is too small or the width is odd.
///
/// Uses the AVX2 path when the CPU has it (every cambox: the N100/N150), else the scalar one;
/// both write the same bytes.
pub fn yuyv_to_planar422(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> bool {
    match yuyv_to_planar422_avx2(src, stride, width, height, y, u, v) {
        Some(ok) => ok,
        None => yuyv_to_planar422_scalar(src, stride, width, height, y, u, v),
    }
}

/// Whether [`yuyv_to_planar422`] takes the AVX2 path on this CPU (std caches the CPUID read).
pub fn yuyv_avx2_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Whether the buffers fit the geometry (the one check both paths make before writing).
fn yuyv_geometry_ok(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    y: &[u8],
    u: &[u8],
    v: &[u8],
) -> bool {
    let cw = width / 2;
    width != 0
        && width.is_multiple_of(2)
        && stride >= width * 2
        && src.len() >= stride * (height.saturating_sub(1)) + width * 2
        && y.len() >= width * height
        && u.len() >= cw * height
        && v.len() >= cw * height
}

/// The AVX2 conversion. `None` when the CPU has no AVX2 (or the target is not x86_64), else
/// what [`yuyv_to_planar422_scalar`] returns, with the same bytes written.
pub fn yuyv_to_planar422_avx2(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> Option<bool> {
    if !yuyv_avx2_available() {
        return None;
    }
    if !yuyv_geometry_ok(src, stride, width, height, y, u, v) {
        return Some(false);
    }
    #[cfg(target_arch = "x86_64")]
    {
        let cw = width / 2;
        for row in 0..height {
            let s = &src[row * stride..row * stride + width * 2];
            let yr = &mut y[row * width..(row + 1) * width];
            let ur = &mut u[row * cw..(row + 1) * cw];
            let vr = &mut v[row * cw..(row + 1) * cw];
            // SAFETY: AVX2 was detected above; the row slices have exactly the lengths
            // `avx2::row` requires (s = 2 x width, yr = width, ur = vr = width / 2).
            unsafe { avx2::row(s, yr, ur, vr) };
        }
    }
    Some(true)
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    /// Pixels per AVX2 block: 128 source bytes -> 64 Y, 32 U, 32 V.
    const BLOCK_PX: usize = 64;

    /// One row: `s` = `2 * n` YUYV bytes, `y` = `n`, `u` = `v` = `n / 2` (n even).
    ///
    /// Per block, four 32-byte loads `a b c d` are read as 16-bit words `[Yk | C << 8]`:
    /// - Y: the low bytes, `packus(a & 0xFF, b & 0xFF)` (packus works per 128-bit lane, so
    ///   `permute4x64(0, 2, 1, 3)` puts the four 8-byte quarters back in order);
    /// - chroma: the high bytes the same way gives `U0 V0 U1 V1 ...`, which splits once more into
    ///   its low bytes (U) and high bytes (V).
    ///
    /// The tail (fewer than 64 pixels) is the scalar loop.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn row(s: &[u8], y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
        let n = y.len();
        debug_assert!(s.len() == 2 * n && u.len() == n / 2 && v.len() == n / 2);
        let blocks = n / BLOCK_PX;
        let mask = _mm256_set1_epi16(0x00FF);
        // packus leaves lane order [a.lo, b.lo, a.hi, b.hi]: quarters 0 2 1 3
        const ORDER: i32 = 0b11_01_10_00;
        for b in 0..blocks {
            let sp = s.as_ptr().add(b * 128) as *const __m256i;
            let a = _mm256_loadu_si256(sp);
            let bb = _mm256_loadu_si256(sp.add(1));
            let c = _mm256_loadu_si256(sp.add(2));
            let d = _mm256_loadu_si256(sp.add(3));
            let y0 = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_and_si256(a, mask),
                _mm256_and_si256(bb, mask),
            ));
            let y1 = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_and_si256(c, mask),
                _mm256_and_si256(d, mask),
            ));
            let c0 = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_srli_epi16::<8>(a),
                _mm256_srli_epi16::<8>(bb),
            ));
            let c1 = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_srli_epi16::<8>(c),
                _mm256_srli_epi16::<8>(d),
            ));
            let uu = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_and_si256(c0, mask),
                _mm256_and_si256(c1, mask),
            ));
            let vv = _mm256_permute4x64_epi64::<ORDER>(_mm256_packus_epi16(
                _mm256_srli_epi16::<8>(c0),
                _mm256_srli_epi16::<8>(c1),
            ));
            let yp = y.as_mut_ptr().add(b * BLOCK_PX) as *mut __m256i;
            _mm256_storeu_si256(yp, y0);
            _mm256_storeu_si256(yp.add(1), y1);
            _mm256_storeu_si256(u.as_mut_ptr().add(b * BLOCK_PX / 2) as *mut __m256i, uu);
            _mm256_storeu_si256(v.as_mut_ptr().add(b * BLOCK_PX / 2) as *mut __m256i, vv);
        }
        let done = blocks * BLOCK_PX;
        super::scalar_row(
            &s[done * 2..],
            &mut y[done..],
            &mut u[done / 2..],
            &mut v[done / 2..],
        );
    }
}

/// One scalar row (the reference): `s` = `2 * n` YUYV bytes into `y` (n), `u`/`v` (n / 2).
fn scalar_row(s: &[u8], y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    for (i, px) in s.as_chunks::<4>().0.iter().enumerate() {
        y[2 * i] = px[0];
        u[i] = px[1];
        y[2 * i + 1] = px[2];
        v[i] = px[3];
    }
}

/// The scalar reference conversion.
pub fn yuyv_to_planar422_scalar(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> bool {
    if !yuyv_geometry_ok(src, stride, width, height, y, u, v) {
        return false;
    }
    let cw = width / 2;
    for row in 0..height {
        scalar_row(
            &src[row * stride..row * stride + width * 2],
            &mut y[row * width..(row + 1) * width],
            &mut u[row * cw..(row + 1) * cw],
            &mut v[row * cw..(row + 1) * cw],
        );
    }
    true
}

/// PSNR in dB between two equal-length 8-bit planes (`f64::INFINITY` when identical).
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "psnr over planes of different size");
    if a.is_empty() {
        return f64::INFINITY;
    }
    let se: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = *x as i64 - *y as i64;
            (d * d) as u64
        })
        .sum();
    if se == 0 {
        return f64::INFINITY;
    }
    let mse = se as f64 / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

/// Convert planar 4:2:2 (BT.709 limited range) to packed RGB8 into `rgb` (`3 x width x height`).
pub fn planar422_to_rgb(y: &[u8], u: &[u8], v: &[u8], width: usize, height: usize, rgb: &mut [u8]) {
    let cw = width / 2;
    for row in 0..height {
        for col in 0..width {
            let yy = y[row * width + col] as f32 - 16.0;
            let cb = u[row * cw + col / 2] as f32 - 128.0;
            let cr = v[row * cw + col / 2] as f32 - 128.0;
            let yy = 1.164 * yy;
            let r = yy + 1.793 * cr;
            let g = yy - 0.213 * cb - 0.533 * cr;
            let b = yy + 2.112 * cb;
            let o = (row * width + col) * 3;
            rgb[o] = r.round().clamp(0.0, 255.0) as u8;
            rgb[o + 1] = g.round().clamp(0.0, 255.0) as u8;
            rgb[o + 2] = b.round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_deinterleaves_with_a_padded_stride() {
        // 4x2 picture, stride 12 (4 bytes of padding per row)
        let src: Vec<u8> = vec![
            10, 20, 11, 30, 12, 21, 13, 31, 0xEE, 0xEE, 0xEE, 0xEE, //
            14, 22, 15, 32, 16, 23, 17, 33, 0xEE, 0xEE, 0xEE, 0xEE,
        ];
        let (mut y, mut u, mut v) = ([0u8; 8], [0u8; 4], [0u8; 4]);
        assert!(yuyv_to_planar422(&src, 12, 4, 2, &mut y, &mut u, &mut v));
        assert_eq!(y, [10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(u, [20, 21, 22, 23]);
        assert_eq!(v, [30, 31, 32, 33]);
    }

    #[test]
    fn yuyv_refuses_short_buffers_and_odd_widths() {
        let src = [0u8; 16];
        let (mut y, mut u, mut v) = ([0u8; 8], [0u8; 4], [0u8; 4]);
        assert!(!yuyv_to_planar422(
            &src[..15],
            8,
            4,
            2,
            &mut y,
            &mut u,
            &mut v
        ));
        assert!(!yuyv_to_planar422(&src, 8, 3, 2, &mut y, &mut u, &mut v));
        assert!(!yuyv_to_planar422(&src, 6, 4, 2, &mut y, &mut u, &mut v));
        assert!(!yuyv_to_planar422(
            &src,
            8,
            4,
            2,
            &mut y[..7],
            &mut u,
            &mut v
        ));
        // the last row needs no trailing padding
        assert!(yuyv_to_planar422(
            &src[..8 + 8],
            8,
            4,
            2,
            &mut y,
            &mut u,
            &mut v
        ));
    }

    /// Every byte value at every byte position of the YUYV group, widths with every tail length
    /// the 64-pixel AVX2 block leaves (and narrower than one block), padded strides: the AVX2 path
    /// writes exactly the scalar path's bytes, and never past the planes' rows.
    #[test]
    fn avx2_conversion_is_bit_exact_with_the_scalar_one() {
        #[cfg(target_arch = "x86_64")]
        assert!(
            yuyv_avx2_available(),
            "every cambox and CI runner has AVX2: the fast path must be the one in use"
        );
        let mut cases = 0;
        for width in (2..=200).step_by(2).chain([1918, 1920, 3840]) {
            for pad in [0usize, 6, 64] {
                let height = if width > 200 { 3 } else { 5 };
                let stride = width * 2 + pad;
                // byte i of the source = (i * 7 + width) mod 256: every value lands on each of the
                // four YUYV positions across the widths
                let src: Vec<u8> = (0..stride * height)
                    .map(|i| ((i * 7 + width) % 256) as u8)
                    .collect();
                let cw = width / 2;
                // one guard byte past every plane: a write past the picture shows
                let (mut ys, mut us, mut vs) = (
                    vec![0xA5u8; width * height + 1],
                    vec![0xA5u8; cw * height + 1],
                    vec![0xA5u8; cw * height + 1],
                );
                let (mut ya, mut ua, mut va) = (ys.clone(), us.clone(), vs.clone());
                let n = width * height;
                let c = cw * height;
                assert!(yuyv_to_planar422_scalar(
                    &src,
                    stride,
                    width,
                    height,
                    &mut ys[..n],
                    &mut us[..c],
                    &mut vs[..c]
                ));
                let got = yuyv_to_planar422_avx2(
                    &src,
                    stride,
                    width,
                    height,
                    &mut ya[..n],
                    &mut ua[..c],
                    &mut va[..c],
                );
                #[cfg(target_arch = "x86_64")]
                assert_eq!(got, Some(true), "width {width} pad {pad}");
                if got.is_some() {
                    assert_eq!(ya, ys, "Y width {width} pad {pad}");
                    assert_eq!(ua, us, "U width {width} pad {pad}");
                    assert_eq!(va, vs, "V width {width} pad {pad}");
                }
                cases += 1;
            }
        }
        assert_eq!(cases, 103 * 3);
        // all 256 values on all 4 positions: a 512-pixel row holding 0..=255 four times, rotated
        for rot in 0..4usize {
            let width = 512;
            let src: Vec<u8> = (0..width * 2)
                .map(|i| ((i / 4 + rot * 64 + i % 4 * 64) % 256) as u8)
                .collect();
            let (mut ys, mut us, mut vs) =
                (vec![0u8; width], vec![0u8; width / 2], vec![0u8; width / 2]);
            let (mut ya, mut ua, mut va) = (ys.clone(), us.clone(), vs.clone());
            assert!(yuyv_to_planar422_scalar(
                &src,
                width * 2,
                width,
                1,
                &mut ys,
                &mut us,
                &mut vs
            ));
            let got = yuyv_to_planar422_avx2(&src, width * 2, width, 1, &mut ya, &mut ua, &mut va);
            if got.is_some() {
                assert_eq!((ya, ua, va), (ys, us, vs), "rotation {rot}");
            }
        }
    }

    #[test]
    fn the_dispatching_conversion_refuses_what_the_scalar_one_refuses() {
        let src = [0u8; 16];
        let (mut y, mut u, mut v) = ([0u8; 8], [0u8; 4], [0u8; 4]);
        assert_eq!(
            yuyv_to_planar422_avx2(&src, 8, 3, 2, &mut y, &mut u, &mut v),
            if yuyv_avx2_available() {
                Some(false)
            } else {
                None
            }
        );
        assert!(!yuyv_to_planar422(
            &src[..15],
            8,
            4,
            2,
            &mut y,
            &mut u,
            &mut v
        ));
        assert!(yuyv_to_planar422(&src, 8, 4, 2, &mut y, &mut u, &mut v));
    }

    #[test]
    fn psnr_is_infinite_when_equal_and_known_for_a_unit_error() {
        assert_eq!(psnr(&[1, 2, 3], &[1, 2, 3]), f64::INFINITY);
        // every sample off by 1: mse 1 -> 20 log10(255) = 48.13 dB
        let p = psnr(&[10, 20], &[11, 19]);
        assert!((p - 48.1308).abs() < 1e-3, "{p}");
    }

    #[test]
    fn rgb_of_limited_range_white_black_and_grey() {
        let mut rgb = [0u8; 6];
        planar422_to_rgb(&[235, 16], &[128], &[128], 2, 1, &mut rgb);
        assert_eq!(rgb, [255, 255, 255, 0, 0, 0]);
        planar422_to_rgb(&[126, 126], &[128], &[128], 2, 1, &mut rgb);
        assert!(rgb.iter().all(|c| (127..=129).contains(c)), "{rgb:?}");
    }
}
