//! Issue 1430: raw luma levels of the captured YUYV frame.
//!
//! The PP cameras (BMPCC -> Cam Link 4K -> cambox) need about twice the ISO for a normal picture.
//! The lead hypothesis is a video-range mismatch: a grabber that delivers FULL-range YUV (0-255)
//! while every receiver reads it as limited (16-235). Nothing measured the raw grabber levels:
//! `capture::mean_chroma` reads only U/V and `capture::luma_roughness` only |Y0 - Y1|. This module
//! reads the luma bytes themselves and reports the share outside the limited range plus p1 / p99.
//! A full-range source on a normal scene shows a real share below 16 / above 235; a limited one
//! shows about 0. Std-only, so the tests run without the probe feature.

/// Every Nth macropixel of a row is sampled, the subsampling `capture::mean_chroma` uses.
const LUMA_SAMPLE_STRIDE: usize = 64;

/// The lowest luma code of the limited ("video") range.
pub const LIMITED_RANGE_BLACK: u8 = 16;

/// The highest luma code of the limited ("video") range.
pub const LIMITED_RANGE_WHITE: u8 = 235;

/// The luma levels of one sampled frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LumaLevels {
    /// The 1st percentile of the sampled luma codes.
    pub p1: u8,
    /// The 99th percentile of the sampled luma codes.
    pub p99: u8,
    /// Percent of the sampled luma codes below [`LIMITED_RANGE_BLACK`].
    pub below_black_pct: f32,
    /// Percent of the sampled luma codes above [`LIMITED_RANGE_WHITE`].
    pub above_white_pct: f32,
    /// How many luma codes were sampled.
    pub samples: u64,
}

impl LumaLevels {
    /// The `capture luma:` log text.
    pub fn describe(&self) -> String {
        format!(
            "p1={} p99={} below16={:.1}% above235={:.1}% samples={}",
            self.p1, self.p99, self.below_black_pct, self.above_white_pct, self.samples
        )
    }
}

/// The luma levels of a YUYV422 frame (`Y0 U Y1 V` per 4-byte macropixel).
///
/// Honours `stride` (bytes per row) so a row-padded device is sampled on real pixels only, samples
/// every [`LUMA_SAMPLE_STRIDE`] macropixels of each row and reads both luma bytes of a sampled
/// macropixel. A macropixel whose bytes fall outside the buffer is skipped. `None` when nothing
/// was sampled (zero dimensions or an undersized buffer).
pub fn luma_levels(frame: &[u8], width: usize, height: usize, stride: usize) -> Option<LumaLevels> {
    let macropixels_per_row = width / 2;
    if macropixels_per_row == 0 || height == 0 {
        return None;
    }
    let mut hist = [0u64; 256];
    let mut samples: u64 = 0;
    for y in 0..height {
        let row_start = y * stride;
        let mut mp = 0usize;
        while mp < macropixels_per_row {
            let idx = row_start + mp * 4;
            if idx + 3 < frame.len() {
                hist[frame[idx] as usize] += 1;
                hist[frame[idx + 2] as usize] += 1;
                samples += 2;
            }
            mp += LUMA_SAMPLE_STRIDE;
        }
    }
    if samples == 0 {
        return None;
    }
    let below: u64 = hist[..LIMITED_RANGE_BLACK as usize].iter().sum();
    let above: u64 = hist[LIMITED_RANGE_WHITE as usize + 1..].iter().sum();
    Some(LumaLevels {
        p1: percentile(&hist, samples, 1),
        p99: percentile(&hist, samples, 99),
        below_black_pct: below as f32 * 100.0 / samples as f32,
        above_white_pct: above as f32 * 100.0 / samples as f32,
        samples,
    })
}

/// The smallest code whose cumulative count reaches `pct` percent of `total` (nearest rank).
fn percentile(hist: &[u64; 256], total: u64, pct: u64) -> u8 {
    let rank = (total * pct).div_ceil(100).max(1);
    let mut cum = 0u64;
    for (code, n) in hist.iter().enumerate() {
        cum += n;
        if cum >= rank {
            return code as u8;
        }
    }
    255
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tightly packed YUYV frame whose luma walks `lo..=hi` row by row (every pixel of row `y` is
    /// `lo + y % span`), chroma neutral, so the subsampled pixels of each row share one code.
    fn ramp(width: usize, height: usize, lo: u8, hi: u8) -> Vec<u8> {
        let mut f = vec![128u8; width * 2 * height];
        let span = (hi - lo) as usize + 1;
        for (y, row) in f.chunks_exact_mut(width * 2).enumerate() {
            for px in row.chunks_exact_mut(2) {
                px[0] = lo + (y % span) as u8;
            }
        }
        f
    }

    #[test]
    fn a_limited_range_picture_has_nothing_outside_16_to_235() {
        let f = ramp(1920, 1080, 16, 235);
        let l = luma_levels(&f, 1920, 1080, 1920 * 2).expect("sampled");
        assert_eq!(l.below_black_pct, 0.0);
        assert_eq!(l.above_white_pct, 0.0);
        assert!(l.p1 >= 16 && l.p99 <= 235, "{l:?}");
    }

    #[test]
    fn a_full_range_picture_shows_its_share_below_16_and_above_235() {
        let f = ramp(1920, 1024, 0, 255); // 4 whole cycles of the 256 codes
        let l = luma_levels(&f, 1920, 1024, 1920 * 2).expect("sampled");
        // 16 of 256 codes below 16 (6.25 %), 20 of 256 above 235 (7.8 %) on a uniform ramp.
        assert!((l.below_black_pct - 6.25).abs() < 1.0, "{l:?}");
        assert!((l.above_white_pct - 7.81).abs() < 1.0, "{l:?}");
        assert!(l.p1 < 16 && l.p99 > 235, "{l:?}");
    }

    #[test]
    fn row_padding_is_never_sampled() {
        let (w, h, stride) = (128usize, 4usize, 128 * 2 + 64);
        let mut f = vec![0u8; stride * h]; // padding = 0, a code below 16
        for y in 0..h {
            for x in 0..w * 2 {
                f[y * stride + x] = if x % 2 == 0 { 100 } else { 128 };
            }
        }
        let l = luma_levels(&f, w, h, stride).expect("sampled");
        assert_eq!(l.below_black_pct, 0.0, "{l:?}");
        assert_eq!((l.p1, l.p99), (100, 100));
    }

    #[test]
    fn an_empty_or_undersized_frame_reads_nothing() {
        assert_eq!(luma_levels(&[], 1920, 1080, 3840), None);
        assert_eq!(luma_levels(&[16, 128, 16], 2, 1, 4), None);
        assert_eq!(luma_levels(&[16, 128, 16, 128], 0, 1, 4), None);
    }

    #[test]
    fn the_log_text_names_every_term() {
        let l = LumaLevels {
            p1: 4,
            p99: 250,
            below_black_pct: 3.25,
            above_white_pct: 1.5,
            samples: 32400,
        };
        assert_eq!(
            l.describe(),
            "p1=4 p99=250 below16=3.2% above235=1.5% samples=32400"
        );
    }
}
