//! YUYV to UYVY conversion for the NDI send path, with the issue-1430 full-to-limited range
//! scaling fused in.
//!
//! YUYV packs `Y0 U Y1 V` per 4 bytes (2 pixels); NDI wants UYVY (`U Y0 V Y1`). A limited-range
//! capture (`CaptureRange::Limited`) is only reordered, byte-identical to the pre-1430 code. A
//! full-range capture (`CaptureRange::Full`, the Cam Link 4K) is also scaled to legal limited
//! range by `crate::capture_range::{full_to_limited_luma, full_to_limited_chroma}`, the scalar
//! reference the AVX2 kernel matches bit for bit (tested over every byte value and tail length).
//!
//! The NDI sender (`crate::ndi::NdiSender`) calls [`convert_into`] with its reused frame buffer.
//! Std-only and every OS (the AVX2 kernel is `x86_64`-only, runtime-detected), so the kernels
//! unit-test on default features.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::capture_range::{full_to_limited_chroma, full_to_limited_luma, CaptureRange};

/// Is AVX2 available on this CPU (runtime check)?
#[cfg(target_arch = "x86_64")]
pub fn has_avx2() -> bool {
    is_x86_feature_detected!("avx2")
}

/// Is AVX2 available on this CPU (never, off `x86_64`)?
#[cfg(not(target_arch = "x86_64"))]
pub fn has_avx2() -> bool {
    false
}

/// Convert `yuyv` into `dst` (cleared and resized, its allocation reused). Only whole 4-byte
/// pixel pairs are converted; a trailing partial pair is dropped (a YUYV frame never has one).
/// `use_avx2` must only be true when [`has_avx2`] is.
pub fn convert_into(yuyv: &[u8], dst: &mut Vec<u8>, range: CaptureRange, use_avx2: bool) {
    #[cfg(target_arch = "x86_64")]
    if use_avx2 {
        // SAFETY: the caller passes `use_avx2` only when the CPU supports AVX2.
        unsafe { convert_avx2_into(yuyv, dst, range) };
        return;
    }
    let _ = use_avx2;
    convert_scalar_into(yuyv, dst, range);
}

/// Allocating convenience over [`convert_into`] (tests, benches, the probe tools).
pub fn convert(yuyv: &[u8], range: CaptureRange) -> Vec<u8> {
    let mut out = Vec::with_capacity(yuyv.len());
    convert_into(yuyv, &mut out, range, has_avx2());
    out
}

/// The scalar reference conversion.
pub fn convert_scalar_into(yuyv: &[u8], dst: &mut Vec<u8>, range: CaptureRange) {
    dst.clear();
    dst.reserve(yuyv.len());
    let pairs = yuyv.as_chunks::<4>().0;
    match range {
        CaptureRange::Limited => {
            for p in pairs {
                dst.extend_from_slice(&[p[1], p[0], p[3], p[2]]);
            }
        }
        CaptureRange::Full => {
            for p in pairs {
                dst.extend_from_slice(&[
                    full_to_limited_chroma(p[1]),
                    full_to_limited_luma(p[0]),
                    full_to_limited_chroma(p[3]),
                    full_to_limited_luma(p[2]),
                ]);
            }
        }
    }
}

/// The AVX2 conversion: 64 bytes (32 pixels) per iteration, the scalar code for the tail.
///
/// Full range: after the reorder, every 16-bit lane of a zero-extended byte pair holds a chroma
/// byte (even) then a luma byte (odd). Each lane computes `(k x + offset) / 255` with `k` 224 or
/// 219 and the rounding offsets of `crate::capture_range` (the numerator stays below 61 216, so
/// it fits u16), and divides by 255 exactly as `mulhi_epu16(n, 0x8081) >> 7` (exact for every
/// u16 `n`: the 0.4 % over-estimate of 2^23 / 255 never crosses an integer below 65 536).
///
/// # Safety
/// The CPU must support AVX2 ([`has_avx2`]).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub unsafe fn convert_avx2_into(yuyv: &[u8], dst: &mut Vec<u8>, range: CaptureRange) {
    let total = yuyv.len() - yuyv.len() % 4;
    let simd = (total / 64) * 64;
    // Every byte of `[0, total)` is written below, so the buffer is not zero-filled first (the
    // pre-1430 kernel paid a 4 MB memset per 1080p frame for it); its length is set at the end.
    dst.clear();
    dst.reserve(total);
    let src = yuyv.as_ptr();
    let out = dst.as_mut_ptr();

    // YUYV `Y0 U Y1 V` (0,1,2,3) -> UYVY `U Y0 V Y1` (1,0,3,2), per 128-bit lane.
    let shuffle = _mm256_setr_epi8(
        1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14, 1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10,
        13, 12, 15, 14,
    );

    let mut i = 0;
    match range {
        CaptureRange::Limited => {
            while i < simd {
                let a = _mm256_loadu_si256(src.add(i) as *const __m256i);
                let b = _mm256_loadu_si256(src.add(i + 32) as *const __m256i);
                _mm256_storeu_si256(out.add(i) as *mut __m256i, _mm256_shuffle_epi8(a, shuffle));
                _mm256_storeu_si256(
                    out.add(i + 32) as *mut __m256i,
                    _mm256_shuffle_epi8(b, shuffle),
                );
                i += 64;
            }
        }
        CaptureRange::Full => {
            let k = FullRangeConsts::new();
            while i < simd {
                let a =
                    _mm256_shuffle_epi8(_mm256_loadu_si256(src.add(i) as *const __m256i), shuffle);
                let b = _mm256_shuffle_epi8(
                    _mm256_loadu_si256(src.add(i + 32) as *const __m256i),
                    shuffle,
                );
                _mm256_storeu_si256(out.add(i) as *mut __m256i, scale_full_to_limited(a, &k));
                _mm256_storeu_si256(
                    out.add(i + 32) as *mut __m256i,
                    scale_full_to_limited(b, &k),
                );
                i += 64;
            }
        }
    }

    // Tail: the scalar mapping, pair by pair.
    while i < total {
        let (y0, u, y1, v) = (
            *src.add(i),
            *src.add(i + 1),
            *src.add(i + 2),
            *src.add(i + 3),
        );
        let px = match range {
            CaptureRange::Limited => [u, y0, v, y1],
            CaptureRange::Full => [
                full_to_limited_chroma(u),
                full_to_limited_luma(y0),
                full_to_limited_chroma(v),
                full_to_limited_luma(y1),
            ],
        };
        std::ptr::copy_nonoverlapping(px.as_ptr(), out.add(i), 4);
        i += 4;
    }
    // SAFETY: the capacity is at least `total` and the loops above wrote every byte below it.
    dst.set_len(total);
}

/// The constant vectors of the full-to-limited scale.
#[cfg(target_arch = "x86_64")]
struct FullRangeConsts {
    gain: __m256i,
    offset: __m256i,
    div255: __m256i,
}

#[cfg(target_arch = "x86_64")]
impl FullRangeConsts {
    /// After the reorder each 16-bit lane of a zero-extended byte pair is (chroma, luma), so a
    /// 32-bit lane holds chroma's constant low and luma's high (little-endian).
    #[target_feature(enable = "avx2")]
    fn new() -> Self {
        use crate::capture_range::{CHROMA_OFFSET, LUMA_OFFSET};
        Self {
            gain: _mm256_set1_epi32((219 << 16) | 224),
            offset: _mm256_set1_epi32(((LUMA_OFFSET as i32) << 16) | CHROMA_OFFSET as i32),
            div255: _mm256_set1_epi16(0x8081u16 as i16),
        }
    }
}

/// Scale 32 reordered UYVY bytes from full to limited range (see [`convert_avx2_into`]).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
fn scale_full_to_limited(v: __m256i, k: &FullRangeConsts) -> __m256i {
    let zero = _mm256_setzero_si256();
    let lo = _mm256_unpacklo_epi8(v, zero);
    let hi = _mm256_unpackhi_epi8(v, zero);
    let lo = _mm256_add_epi16(_mm256_mullo_epi16(lo, k.gain), k.offset);
    let hi = _mm256_add_epi16(_mm256_mullo_epi16(hi, k.gain), k.offset);
    let lo = _mm256_srli_epi16::<7>(_mm256_mulhi_epu16(lo, k.div255));
    let hi = _mm256_srli_epi16::<7>(_mm256_mulhi_epu16(hi, k.div255));
    _mm256_packus_epi16(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (Y, C) combination at every position of a pair: 256 x 256 pairs whose four bytes
    /// cycle through all values, so each byte slot sees every value against varied neighbours.
    fn exhaustive_input() -> Vec<u8> {
        let mut v = Vec::with_capacity(256 * 256 * 4);
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                v.extend_from_slice(&[a, b, b.wrapping_add(a), a.wrapping_mul(3)]);
            }
        }
        v
    }

    fn scalar(yuyv: &[u8], range: CaptureRange) -> Vec<u8> {
        let mut out = Vec::new();
        convert_scalar_into(yuyv, &mut out, range);
        out
    }

    #[test]
    fn limited_passthrough_only_reorders() {
        let out = scalar(&[10, 20, 30, 40, 50, 60, 70, 80], CaptureRange::Limited);
        assert_eq!(out, vec![20, 10, 40, 30, 60, 50, 80, 70]);
    }

    #[test]
    fn full_range_scales_every_byte_by_its_channel() {
        // Y0=0 U=0 Y1=255 V=255 -> U'=16 Y0'=16 V'=240 Y1'=235
        assert_eq!(
            scalar(&[0, 0, 255, 255], CaptureRange::Full),
            vec![16, 16, 240, 235]
        );
        // neutral chroma stays neutral, mid luma moves
        assert_eq!(
            scalar(&[128, 128, 128, 128], CaptureRange::Full),
            vec![
                128,
                full_to_limited_luma(128),
                128,
                full_to_limited_luma(128)
            ]
        );
    }

    #[test]
    fn full_range_scalar_maps_all_256_values_in_every_slot() {
        let input: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, v]).collect();
        let out = scalar(&input, CaptureRange::Full);
        for v in 0..=255usize {
            let px = &out[v * 4..v * 4 + 4];
            let (c, y) = (
                full_to_limited_chroma(v as u8),
                full_to_limited_luma(v as u8),
            );
            assert_eq!(px, &[c, y, c, y], "value {v}");
        }
    }

    #[test]
    fn a_trailing_partial_pair_is_dropped() {
        assert_eq!(
            scalar(&[1, 2, 3, 4, 5, 6], CaptureRange::Limited),
            vec![2, 1, 4, 3]
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_matches_scalar_bit_exact_for_every_value_and_every_tail_length() {
        if !has_avx2() {
            eprintln!("AVX2 not available on this CPU: the scalar path is the only path");
            return;
        }
        let input = exhaustive_input();
        for range in [CaptureRange::Full, CaptureRange::Limited] {
            // Lengths around the 64-byte block: every tail of 0..=15 pairs, plus a long run.
            for len in (0..=256).step_by(4).chain([input.len() - 36]) {
                for start in [0usize, 4, 36] {
                    let src = &input[start..start + len];
                    let mut avx = Vec::new();
                    unsafe { convert_avx2_into(src, &mut avx, range) };
                    assert_eq!(avx, scalar(src, range), "{range:?} len {len} start {start}");
                }
            }
            // Odd (non-multiple-of-4) lengths: both drop the partial pair identically.
            for len in [1usize, 2, 3, 5, 63, 65, 66, 67, 129] {
                let mut avx = Vec::new();
                unsafe { convert_avx2_into(&input[..len], &mut avx, range) };
                assert_eq!(avx, scalar(&input[..len], range), "{range:?} odd len {len}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_full_range_hits_the_endpoints() {
        if !has_avx2() {
            eprintln!("AVX2 not available on this CPU: the scalar path is the only path");
            return;
        }
        let mut input = Vec::new();
        for _ in 0..32 {
            input.extend_from_slice(&[0, 0, 255, 255]);
        }
        let mut avx = Vec::new();
        unsafe { convert_avx2_into(&input, &mut avx, CaptureRange::Full) };
        for px in avx.as_chunks::<4>().0 {
            assert_eq!(px, &[16, 16, 240, 235]);
        }
    }

    #[test]
    fn limited_passthrough_is_byte_identical_to_the_pre_1430_reorder() {
        let input = exhaustive_input();
        let reference: Vec<u8> = input
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[1], p[0], p[3], p[2]])
            .collect();
        assert_eq!(convert(&input, CaptureRange::Limited), reference);
        assert_eq!(scalar(&input, CaptureRange::Limited), reference);
    }

    #[test]
    fn convert_into_reuses_the_buffer_and_overwrites_old_content() {
        let mut buf = vec![9u8; 100];
        convert_into(&[0, 0, 255, 255], &mut buf, CaptureRange::Full, has_avx2());
        assert_eq!(buf, vec![16, 16, 240, 235]);
    }
}
