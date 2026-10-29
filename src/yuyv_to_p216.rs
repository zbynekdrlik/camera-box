//! Issue 1387 — the NDI `P216` send format: YUYV 8-bit expanded to 16-bit semi-planar 4:2:2.
//!
//! The standard NDI SDK 6 sender has no quality or bitrate setting; the one sender-side lever is
//! the pixel format. Today camera-box sends `UYVY` (8-bit 4:2:2). `P216` is the SDK's 16 bpp
//! 4:2:2 format, documented in `vendor/distroav/lib/ndi/Processing.NDI.structs.h:70-75`:
//!
//! > YCbCr color space using 4:2:2 in 16bpp. In memory this is a semi-planar format. This is
//! > identical to a 16bpp version of the NV16 format. The first buffer is a 16bpp luminance
//! > buffer. Immediately after this is an interleaved buffer of 16bpp Cb, Cr pairs.
//!
//! and `line_stride_in_bytes` (`:237-239`) is "the inter-line stride of the video data in bytes",
//! defaulting to `sizeof(one pixel) * xres`. For P216 one luma sample is 2 bytes, so the stride is
//! `width * 2`; a CbCr row holds `width / 2` Cb,Cr pairs of 2 x 2 bytes = the same `width * 2`.
//!
//! The frame is ONE contiguous `u16` buffer: `width * height` luma samples, then
//! `width * height` interleaved chroma samples (Cb0 Cr0 Cb1 Cr1 ...). Each 8-bit sample `v`
//! becomes `v << 8 | v` (`v * 257`), so 0 maps to 0 and 255 to 65535. The capture's
//! limited-range signal stays where it is: the conversion re-scales nothing (issue 1430).
//!
//! The default send stays `UYVY`, byte-identical to before: the switch `CAMERA_BOX_NDI_FORMAT`
//! is read once at sender creation ([`parse_send_format`]), and only a `YUYV` capture (the
//! Cam Link path) has a P216 conversion ([`select_send_path`]); every other input keeps UYVY.
//!
//! Pure + std-only (plus `core::arch` on x86_64), so it unit-tests Tier-0; `crate::ndi` wires it.

/// The NDI FourCC for P216 (`NDI_LIB_FOURCC('P','2','1','6')`, little-endian byte order).
pub const NDI_FOURCC_P216: u32 = u32::from_le_bytes(*b"P216");

/// The env switch read once at NDI sender creation.
pub const NDI_FORMAT_ENV: &str = "CAMERA_BOX_NDI_FORMAT";

/// The pixel format the NDI sender is configured to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdiSendFormat {
    /// 8-bit 4:2:2 UYVY — the default, unchanged since before issue 1387.
    Uyvy,
    /// 16-bit semi-planar 4:2:2 P216 (issue 1387), for a YUYV capture.
    P216,
}

impl NdiSendFormat {
    /// The lowercase name used in the env value and the start line.
    pub fn name(self) -> &'static str {
        match self {
            NdiSendFormat::Uyvy => "uyvy",
            NdiSendFormat::P216 => "p216",
        }
    }
}

/// Parse the `CAMERA_BOX_NDI_FORMAT` value. Unset or empty = `Uyvy`. Case and surrounding
/// whitespace are ignored. An unknown value returns `Uyvy` plus the one warning to log.
pub fn parse_send_format(value: Option<&str>) -> (NdiSendFormat, Option<String>) {
    let raw = value.unwrap_or("");
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "uyvy" => (NdiSendFormat::Uyvy, None),
        "p216" => (NdiSendFormat::P216, None),
        _ => (
            NdiSendFormat::Uyvy,
            Some(format!(
                "ndi send format: unknown {NDI_FORMAT_ENV} value '{raw}' (expected uyvy or p216), \
                 sending uyvy (issue 1387)"
            )),
        ),
    }
}

/// The one start line logged at sender creation.
pub fn send_format_start_line(format: NdiSendFormat) -> String {
    format!("ndi send format: {} (issue 1387)", format.name())
}

/// Which conversion one frame takes on its way to the NDI SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendPath {
    /// The pre-issue-1387 UYVY path (passthrough or conversion), unchanged.
    Uyvy,
    /// YUYV -> P216 ([`convert_yuyv_to_p216`]).
    P216,
}

/// Decide the send path for a frame of V4L2 FourCC `input_fourcc` under `format`.
/// Only a `YUYV` input has a P216 conversion; anything else keeps UYVY.
pub fn select_send_path(format: NdiSendFormat, input_fourcc: &str) -> SendPath {
    match (format, input_fourcc) {
        (NdiSendFormat::P216, "YUYV") => SendPath::P216,
        _ => SendPath::Uyvy,
    }
}

/// The WARN logged once when P216 is configured but the input has no P216 conversion.
pub fn p216_fallback_line(input_fourcc: &str) -> String {
    format!(
        "ndi send format: p216 requested, but the capture delivers {input_fourcc} and only a \
         YUYV capture has a P216 conversion; sending UYVY (issue 1387)"
    )
}

/// Number of `u16` samples in one P216 frame: the luma plane plus the CbCr plane.
pub fn p216_samples(width: usize, height: usize) -> usize {
    width * height * 2
}

/// The NDI `line_stride_in_bytes` of a P216 frame (the luma plane stride; the CbCr plane has
/// the same one).
pub fn p216_line_stride_bytes(width: usize) -> usize {
    width * 2
}

/// What `NdiSender` hands the SDK for a converted P216 frame: the FourCC and
/// `line_stride_in_bytes`. One place, so the send wiring is pinned by a test.
pub fn p216_send_params(width: usize) -> (u32, usize) {
    let _ = width;
    (0, 0)
}

/// Why a frame could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P216Error {
    /// YUYV carries pixel pairs, so the width must be even and non-zero.
    BadWidth(usize),
    /// The source stride is shorter than one YUYV row (`width * 2` bytes).
    StrideTooShort { stride: usize, row_bytes: usize },
    /// The source buffer is shorter than `height` rows at `stride`.
    SourceTooShort { len: usize, needed: usize },
}

impl std::fmt::Display for P216Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            P216Error::BadWidth(w) => write!(f, "YUYV->P216: width {w} must be even and non-zero"),
            P216Error::StrideTooShort { stride, row_bytes } => write!(
                f,
                "YUYV->P216: source stride {stride} is shorter than one row ({row_bytes} bytes)"
            ),
            P216Error::SourceTooShort { len, needed } => write!(
                f,
                "YUYV->P216: source is {len} bytes, the frame needs {needed}"
            ),
        }
    }
}

impl std::error::Error for P216Error {}

/// Which inner loop converts the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertPath {
    Scalar,
    /// AVX2 when the CPU has it, else the scalar loop (never undefined behaviour).
    Avx2,
}

/// True when this CPU can run the AVX2 path.
pub fn avx2_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Convert one YUYV frame (`src_stride` bytes per row) into `out` as P216.
///
/// `out` is reused across frames: it is resized only when the frame size changes, never
/// cleared or zero-filled per frame, because every sample is overwritten.
pub fn convert_yuyv_to_p216(
    yuyv: &[u8],
    width: usize,
    height: usize,
    src_stride: usize,
    out: &mut Vec<u16>,
    path: ConvertPath,
) -> Result<(), P216Error> {
    if width == 0 || !width.is_multiple_of(2) {
        return Err(P216Error::BadWidth(width));
    }
    let row_bytes = width * 2;
    if src_stride < row_bytes {
        return Err(P216Error::StrideTooShort {
            stride: src_stride,
            row_bytes,
        });
    }
    // The last row only needs its own `width * 2` bytes, not a full padded stride.
    let needed = if height == 0 {
        0
    } else {
        src_stride * (height - 1) + row_bytes
    };
    if yuyv.len() < needed {
        return Err(P216Error::SourceTooShort {
            len: yuyv.len(),
            needed,
        });
    }

    let samples = p216_samples(width, height);
    if out.len() != samples {
        // Only on the first frame or a resolution change: every sample is overwritten below.
        out.resize(samples, 0);
    }
    let (luma, chroma) = out.split_at_mut(width * height);
    let use_avx2 = path == ConvertPath::Avx2 && avx2_available();

    for r in 0..height {
        let row = &yuyv[r * src_stride..r * src_stride + row_bytes];
        let y = &mut luma[r * width..(r + 1) * width];
        let c = &mut chroma[r * width..(r + 1) * width];
        #[cfg(target_arch = "x86_64")]
        if use_avx2 {
            // SAFETY: `avx2_available()` confirmed AVX2; the slice lengths are checked inside.
            unsafe { avx2_row(row, y, c) };
            continue;
        }
        let _ = use_avx2;
        scalar_row(row, y, c);
    }
    Ok(())
}

/// One YUYV row (`Y0 U0 Y1 V0 ...`, `2 * y.len()` bytes) into its luma row and CbCr row.
///
/// Read as little-endian `u16` lanes, a YUYV row is `Y | C << 8` per lane, where C walks
/// `U0 V0 U1 V1 ...`, exactly the P216 CbCr order. So lane `i` gives luma `i` and chroma `i`.
#[inline]
pub fn scalar_row(row: &[u8], y: &mut [u16], c: &mut [u16]) {
    let n = y.len().min(c.len()).min(row.len() / 2);
    for i in 0..n {
        let luma = u16::from(row[2 * i]);
        let cbcr = u16::from(row[2 * i + 1]);
        y[i] = (luma << 8) | luma;
        c[i] = (cbcr << 8) | cbcr;
    }
}

/// AVX2 row kernel, bit-exact with [`scalar_row`]: 16 lanes (32 YUYV bytes) per step,
/// `y = (lane & 0xFF) * 257`, `c = (lane >> 8) * 257`, no shuffle (the order is already right),
/// and the tail through [`scalar_row`].
///
/// # Safety
/// The CPU must support AVX2.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub unsafe fn avx2_row(row: &[u8], y: &mut [u16], c: &mut [u16]) {
    use std::arch::x86_64::{
        __m256i, _mm256_and_si256, _mm256_loadu_si256, _mm256_or_si256, _mm256_set1_epi16,
        _mm256_slli_epi16, _mm256_srli_epi16, _mm256_storeu_si256,
    };
    let n = y.len().min(c.len()).min(row.len() / 2);
    let blocks = n / 16;
    let low = _mm256_set1_epi16(0x00FF);
    let src = row.as_ptr();
    let yd = y.as_mut_ptr();
    let cd = c.as_mut_ptr();
    for b in 0..blocks {
        // SAFETY: lane block `b` covers bytes [32b, 32b + 32) of `row` and samples
        // [16b, 16b + 16) of `y` and `c`, all inside `n` (checked above); unaligned load/store.
        unsafe {
            let lanes = _mm256_loadu_si256(src.add(b * 32) as *const __m256i);
            let yl = _mm256_and_si256(lanes, low);
            let cl = _mm256_srli_epi16(lanes, 8);
            let y16 = _mm256_or_si256(yl, _mm256_slli_epi16(yl, 8));
            let c16 = _mm256_or_si256(cl, _mm256_slli_epi16(cl, 8));
            _mm256_storeu_si256(yd.add(b * 16) as *mut __m256i, y16);
            _mm256_storeu_si256(cd.add(b * 16) as *mut __m256i, c16);
        }
    }
    let done = blocks * 16;
    scalar_row(&row[done * 2..n * 2], &mut y[done..n], &mut c[done..n]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dup(v: u8) -> u16 {
        (u16::from(v) << 8) | u16::from(v)
    }

    /// A YUYV frame whose bytes walk every value 0..=255 (offset per frame so rows differ).
    fn pattern(width: usize, height: usize, stride: usize, seed: usize) -> Vec<u8> {
        let mut v = vec![0xA5u8; stride * height];
        for r in 0..height {
            for b in 0..width * 2 {
                v[r * stride + b] = ((r * 131 + b * 7 + seed) % 256) as u8;
            }
        }
        v
    }

    /// The reference P216 layout written out longhand.
    fn reference(yuyv: &[u8], width: usize, height: usize, stride: usize) -> Vec<u16> {
        let mut out = vec![0u16; width * height * 2];
        for r in 0..height {
            let row = &yuyv[r * stride..r * stride + width * 2];
            for k in 0..width / 2 {
                out[r * width + 2 * k] = dup(row[4 * k]);
                out[r * width + 2 * k + 1] = dup(row[4 * k + 2]);
                out[width * height + r * width + 2 * k] = dup(row[4 * k + 1]);
                out[width * height + r * width + 2 * k + 1] = dup(row[4 * k + 3]);
            }
        }
        out
    }

    #[test]
    fn sample_expansion_maps_full_scale_to_full_scale_1387() {
        // A 2x1 frame: Y0=0, U=128, Y1=255, V=16.
        let mut out = Vec::new();
        convert_yuyv_to_p216(&[0, 128, 255, 16], 2, 1, 4, &mut out, ConvertPath::Scalar).unwrap();
        assert_eq!(out, vec![0x0000, 0xFFFF, 0x8080, 0x1010]);
    }

    #[test]
    fn every_byte_value_expands_to_v_times_257_1387() {
        // 256 byte values as one row of 128 pixels: each value lands in its plane as v*257.
        let row: Vec<u8> = (0..=255u8).collect();
        for path in [ConvertPath::Scalar, ConvertPath::Avx2] {
            let mut out = Vec::new();
            convert_yuyv_to_p216(&row, 128, 1, 256, &mut out, path).unwrap();
            for (j, &v) in row.iter().enumerate() {
                let sample = if j % 2 == 0 {
                    out[j / 2]
                } else {
                    out[128 + j / 2]
                };
                assert_eq!(sample, u16::from(v) * 257, "{path:?} byte {j}");
            }
        }
    }

    #[test]
    fn plane_layout_and_size_match_the_sdk_p216_definition_1387() {
        let (w, h) = (1920, 1080);
        assert_eq!(p216_samples(w, h) * 2, w * h * 2 + w * h * 2);
        assert_eq!(p216_line_stride_bytes(w), w * 2);
        let src = pattern(w, h, w * 2, 3);
        let mut out = Vec::new();
        convert_yuyv_to_p216(&src, w, h, w * 2, &mut out, ConvertPath::Scalar).unwrap();
        assert_eq!(out.len(), p216_samples(w, h));
        assert_eq!(out, reference(&src, w, h, w * 2));
    }

    #[test]
    fn scalar_and_avx2_are_bit_exact_over_odd_tails_and_strides_1387() {
        // Widths that are not a multiple of the 16-pixel AVX2 block, plus padded strides.
        for &w in &[
            2usize, 4, 6, 14, 16, 18, 30, 32, 34, 62, 66, 126, 130, 1918, 1920,
        ] {
            for &pad in &[0usize, 2, 64] {
                let (h, stride) = (3, w * 2 + pad);
                let src = pattern(w, h, stride, w + pad);
                let mut scalar = Vec::new();
                let mut avx = Vec::new();
                convert_yuyv_to_p216(&src, w, h, stride, &mut scalar, ConvertPath::Scalar).unwrap();
                convert_yuyv_to_p216(&src, w, h, stride, &mut avx, ConvertPath::Avx2).unwrap();
                assert_eq!(
                    scalar,
                    reference(&src, w, h, stride),
                    "scalar w={w} pad={pad}"
                );
                assert_eq!(avx, scalar, "avx2 != scalar w={w} pad={pad}");
            }
        }
    }

    #[test]
    fn the_avx2_kernel_itself_is_bit_exact_with_scalar_1387() {
        // Directly against the AVX2 kernel (the dispatcher would fall back on a CPU without
        // it, so a missing AVX2 must FAIL here, never pass silently).
        #[cfg(target_arch = "x86_64")]
        assert!(
            avx2_available(),
            "AVX2 is required to verify the AVX2 kernel"
        );
        #[cfg(target_arch = "x86_64")]
        for &w in &[16usize, 18, 32, 46, 1920] {
            let src = pattern(w, 2, w * 2, 11);
            let mut y = vec![0u16; w];
            let mut c = vec![0u16; w];
            // SAFETY: avx2_available() checked above.
            unsafe { avx2_row(&src[..w * 2], &mut y, &mut c) };
            let r = reference(&src, w, 1, w * 2);
            assert_eq!(&y[..], &r[..w], "luma w={w}");
            assert_eq!(&c[..], &r[w..w * 2], "chroma w={w}");
        }
    }

    #[test]
    fn the_output_buffer_is_reused_without_reallocation_1387() {
        let (w, h) = (64, 4);
        let src = pattern(w, h, w * 2, 5);
        let mut out = Vec::new();
        convert_yuyv_to_p216(&src, w, h, w * 2, &mut out, ConvertPath::Avx2).unwrap();
        let ptr = out.as_ptr();
        let cap = out.capacity();
        let src2 = pattern(w, h, w * 2, 77);
        convert_yuyv_to_p216(&src2, w, h, w * 2, &mut out, ConvertPath::Avx2).unwrap();
        assert_eq!(out.as_ptr(), ptr, "same allocation");
        assert_eq!(out.capacity(), cap);
        assert_eq!(out, reference(&src2, w, h, w * 2), "fully overwritten");
    }

    #[test]
    fn bad_geometry_is_an_error_never_a_panic_1387() {
        let mut out = Vec::new();
        let p = ConvertPath::Scalar;
        assert_eq!(
            convert_yuyv_to_p216(&[0; 8], 3, 1, 8, &mut out, p),
            Err(P216Error::BadWidth(3))
        );
        assert_eq!(
            convert_yuyv_to_p216(&[0; 8], 0, 1, 8, &mut out, p),
            Err(P216Error::BadWidth(0))
        );
        assert_eq!(
            convert_yuyv_to_p216(&[0; 8], 4, 1, 6, &mut out, p),
            Err(P216Error::StrideTooShort {
                stride: 6,
                row_bytes: 8
            })
        );
        // The last row only needs width*2 bytes, not a full stride.
        assert!(convert_yuyv_to_p216(&[0; 16 + 8], 4, 2, 16, &mut out, p).is_ok());
        assert_eq!(
            convert_yuyv_to_p216(&[0; 16 + 7], 4, 2, 16, &mut out, p),
            Err(P216Error::SourceTooShort {
                len: 23,
                needed: 24
            })
        );
    }

    #[test]
    fn the_switch_defaults_to_uyvy_and_parses_p216_1387() {
        assert_eq!(parse_send_format(None), (NdiSendFormat::Uyvy, None));
        assert_eq!(parse_send_format(Some("")), (NdiSendFormat::Uyvy, None));
        assert_eq!(parse_send_format(Some("uyvy")), (NdiSendFormat::Uyvy, None));
        assert_eq!(
            parse_send_format(Some(" P216 ")),
            (NdiSendFormat::P216, None)
        );
        assert_eq!(parse_send_format(Some("p216")), (NdiSendFormat::P216, None));
        let (f, warn) = parse_send_format(Some("v210"));
        assert_eq!(f, NdiSendFormat::Uyvy);
        let warn = warn.expect("an unknown value warns");
        assert!(warn.contains("'v210'") && warn.contains("uyvy"), "{warn}");
        assert!(
            warn.contains(NDI_FORMAT_ENV) && warn.contains("issue 1387"),
            "{warn}"
        );
    }

    #[test]
    fn the_start_line_names_the_format_1387() {
        assert_eq!(
            send_format_start_line(NdiSendFormat::Uyvy),
            "ndi send format: uyvy (issue 1387)"
        );
        assert_eq!(
            send_format_start_line(NdiSendFormat::P216),
            "ndi send format: p216 (issue 1387)"
        );
    }

    #[test]
    fn the_default_format_keeps_every_input_on_the_uyvy_path_1387() {
        for fourcc in ["YUYV", "UYVY", "NV12", "MJPG", "BGRA", "BGR4", "RX24"] {
            assert_eq!(
                select_send_path(NdiSendFormat::Uyvy, fourcc),
                SendPath::Uyvy
            );
        }
    }

    #[test]
    fn p216_applies_only_to_a_yuyv_capture_1387() {
        assert_eq!(
            select_send_path(NdiSendFormat::P216, "YUYV"),
            SendPath::P216
        );
        for fourcc in ["UYVY", "NV12", "MJPG", "BGRA", "BGR4", "RX24"] {
            assert_eq!(
                select_send_path(NdiSendFormat::P216, fourcc),
                SendPath::Uyvy
            );
            let line = p216_fallback_line(fourcc);
            assert!(line.contains(fourcc) && line.contains("UYVY"), "{line}");
            assert!(line.contains("issue 1387"), "{line}");
        }
    }

    #[test]
    fn the_send_params_are_the_p216_fourcc_and_the_luma_stride_1387() {
        assert_eq!(p216_send_params(1920), (NDI_FOURCC_P216, 3840));
        assert_eq!(p216_send_params(2), (NDI_FOURCC_P216, 4));
        // A padded V4L2 source stride never leaks into the P216 stride: it depends on width only.
        assert_eq!(p216_send_params(1280).1, p216_line_stride_bytes(1280));
    }

    #[test]
    fn the_avx2_dispatch_is_really_exercised_on_x86_64_1387() {
        // The bit-exact tests compare ConvertPath::Avx2 against Scalar; on x86_64 the AVX2
        // path must really run, or they would compare scalar with itself.
        #[cfg(target_arch = "x86_64")]
        assert!(
            avx2_available(),
            "AVX2 is required on x86_64 for the bit-exact tests"
        );
    }

    #[test]
    fn the_fourcc_is_the_sdk_p216_value_1387() {
        assert_eq!(NDI_FOURCC_P216, u32::from_le_bytes(*b"P216"));
        // NDI_LIB_FOURCC(a,b,c,d) = a | b<<8 | c<<16 | d<<24.
        let sdk = u32::from(b'P')
            | (u32::from(b'2') << 8)
            | (u32::from(b'1') << 16)
            | (u32::from(b'6') << 24);
        assert_eq!(NDI_FOURCC_P216, sdk);
    }
}
