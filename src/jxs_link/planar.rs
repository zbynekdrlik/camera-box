//! Issue 1387: pixel helpers of the JPEG XS link (pure, allocation-free).

/// Deinterleave a packed YUYV (Y0 U Y1 V) picture into planar 4:2:2: `y` is `width x height`,
/// `u`/`v` are `width/2 x height`. `stride` is the source row length in bytes (>= 2 x width).
/// Returns `false` (and writes nothing) when a buffer is too small or the width is odd.
pub fn yuyv_to_planar422(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> bool {
    let cw = width / 2;
    if width == 0
        || !width.is_multiple_of(2)
        || stride < width * 2
        || src.len() < stride * (height.saturating_sub(1)) + width * 2
        || y.len() < width * height
        || u.len() < cw * height
        || v.len() < cw * height
    {
        return false;
    }
    for row in 0..height {
        let s = &src[row * stride..row * stride + width * 2];
        let yr = &mut y[row * width..(row + 1) * width];
        let ur = &mut u[row * cw..(row + 1) * cw];
        let vr = &mut v[row * cw..(row + 1) * cw];
        for (i, px) in s.as_chunks::<4>().0.iter().enumerate() {
            yr[2 * i] = px[0];
            ur[i] = px[1];
            yr[2 * i + 1] = px[2];
            vr[i] = px[3];
        }
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
