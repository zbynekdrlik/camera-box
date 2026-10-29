//! Issue 1430 -- which video RANGE the capture card delivers, and the full-to-limited mapping.
//!
//! The Elgato Cam Link 4K grabbers deliver FULL-range YUYV (live 28.10.2026: luma p1 = 0, p99 =
//! 255, 1.6-1.7 % of samples below 16 and 4.4-5.1 % above 235). UVC carries no range flag, so
//! V4L2 cannot tell us. NDI UYVY is BT.709 LIMITED range and every receiver reads it so (the
//! genlock OBS forces `yuv_range=partial`), which crushes the blacks and clips the whites. The
//! cambox therefore scales a full-range grabber to legal limited range inside the YUYV to UYVY
//! conversion (`crate::yuyv_to_uyvy`).
//!
//! This module is the pure decision (range from the detected V4L2 card name, plus the
//! `CAMERA_BOX_CAPTURE_RANGE=full|limited` override) and the scalar reference mapping the SIMD
//! kernel must match bit for bit. Std-only, every OS, so it unit-tests on default features.

use crate::capture_rate_health::{grabber_model_from_card_name, GrabberModel};

/// The environment variable that overrides the card-name decision (`full` or `limited`).
pub const CAPTURE_RANGE_ENV: &str = "CAMERA_BOX_CAPTURE_RANGE";

/// The video range the capture card delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureRange {
    /// Full range (Y and C use 0-255): scaled to limited range on the way out.
    Full,
    /// Limited range (Y 16-235, C 16-240): passed through byte-for-byte, as before issue 1430.
    #[default]
    Limited,
}

/// Where the decision came from (for the one start-up log line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSource {
    /// `CAMERA_BOX_CAPTURE_RANGE` set to a valid value.
    Override,
    /// The detected card is a known full-range grabber.
    Card,
    /// No override and no known full-range card: limited passthrough (today's behaviour).
    Default,
}

/// The resolved capture range plus how it was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRangeDecision {
    pub range: CaptureRange,
    pub source: RangeSource,
    /// A set but unrecognised override value (ignored, the caller warns once).
    pub invalid_override: Option<String>,
}

/// Parse an override value: `full` or `limited`, case-insensitive, surrounding blanks ignored.
pub fn parse_range_override(value: &str) -> Option<CaptureRange> {
    match value.trim().to_ascii_lowercase().as_str() {
        "full" => Some(CaptureRange::Full),
        "limited" => Some(CaptureRange::Limited),
        _ => None,
    }
}

/// The range a grabber delivers, from its V4L2 `card` string. Only a card known to deliver full
/// range returns `Full`; anything else (unknown, absent) keeps the limited passthrough, so an
/// unrecognised grabber is never rescaled on a guess.
pub fn range_from_card(card: Option<&str>) -> CaptureRange {
    match card.map(grabber_model_from_card_name) {
        Some(GrabberModel::CamLink4k) => CaptureRange::Full,
        _ => CaptureRange::Limited,
    }
}

/// Resolve the capture range: a valid override wins, else the detected card decides.
pub fn resolve_capture_range(
    card: Option<&str>,
    override_value: Option<&str>,
) -> CaptureRangeDecision {
    let mut invalid_override = None;
    if let Some(raw) = override_value {
        match parse_range_override(raw) {
            Some(range) => {
                return CaptureRangeDecision {
                    range,
                    source: RangeSource::Override,
                    invalid_override: None,
                }
            }
            None => invalid_override = Some(raw.to_string()),
        }
    }
    let range = range_from_card(card);
    let source = if range == CaptureRange::Full {
        RangeSource::Card
    } else {
        RangeSource::Default
    };
    CaptureRangeDecision {
        range,
        source,
        invalid_override,
    }
}

impl CaptureRangeDecision {
    /// The one start-up journal line: `capture range: <full->limited|limited passthrough>
    /// (issue 1430)`, then where the decision came from.
    pub fn log_line(&self, card: Option<&str>) -> String {
        let what = match self.range {
            CaptureRange::Full => "full->limited",
            CaptureRange::Limited => "limited passthrough",
        };
        let why = match self.source {
            RangeSource::Override => format!("{CAPTURE_RANGE_ENV} override"),
            RangeSource::Card => format!("card {card:?}"),
            RangeSource::Default => format!("card {card:?} not a known full-range grabber"),
        };
        format!("capture range: {what} (issue 1430) source={why}")
    }
}

/// Full-range luma to limited range: `Y' = 16 + Y * 219 / 255`, rounded to nearest.
/// As one non-negative integer: `(219 Y + 16 * 255 + 127) / 255` (no ties exist: 255 is odd).
#[inline]
pub fn full_to_limited_luma(y: u8) -> u8 {
    ((219 * y as u32 + LUMA_OFFSET) / 255) as u8
}

/// Full-range chroma to limited range: `C' = 128 + (C - 128) * 224 / 255`, rounded to nearest.
/// As one non-negative integer: `(224 C + 128 * 31 + 127) / 255` (no ties).
#[inline]
pub fn full_to_limited_chroma(c: u8) -> u8 {
    ((224 * c as u32 + CHROMA_OFFSET) / 255) as u8
}

/// `16 * 255 + 127`: the limited black level plus the round-to-nearest half divisor.
pub const LUMA_OFFSET: u32 = 16 * 255 + 127;
/// `128 * (255 - 224) + 127`: keeps 128 at 128, plus the round-to-nearest half divisor.
pub const CHROMA_OFFSET: u32 = 128 * (255 - 224) + 127;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_endpoints_map_to_the_limited_black_and_white() {
        assert_eq!(full_to_limited_luma(0), 16);
        assert_eq!(full_to_limited_luma(255), 235);
    }

    #[test]
    fn chroma_endpoints_and_neutral_map_to_limited_range() {
        assert_eq!(full_to_limited_chroma(0), 16);
        assert_eq!(full_to_limited_chroma(128), 128);
        assert_eq!(full_to_limited_chroma(255), 240);
    }

    #[test]
    fn integer_mapping_equals_the_rounded_real_formula_for_every_value() {
        for v in 0..=255u8 {
            let y = 16.0 + v as f64 * 219.0 / 255.0;
            let c = 128.0 + (v as f64 - 128.0) * 224.0 / 255.0;
            assert_eq!(full_to_limited_luma(v), y.round() as u8, "luma {v}");
            assert_eq!(full_to_limited_chroma(v), c.round() as u8, "chroma {v}");
        }
    }

    #[test]
    fn mapping_is_monotonic_and_stays_in_the_legal_range() {
        for v in 0..255u8 {
            assert!(full_to_limited_luma(v) <= full_to_limited_luma(v + 1));
            assert!(full_to_limited_chroma(v) <= full_to_limited_chroma(v + 1));
        }
        for v in 0..=255u8 {
            assert!((16..=235).contains(&full_to_limited_luma(v)));
            assert!((16..=240).contains(&full_to_limited_chroma(v)));
        }
    }

    #[test]
    fn the_live_double_spaced_cam_link_card_is_full_range() {
        assert_eq!(
            range_from_card(Some("CAM  LINK 4K: CAM  LINK 4K")),
            CaptureRange::Full
        );
        assert_eq!(range_from_card(Some("Cam Link 4K")), CaptureRange::Full);
    }

    #[test]
    fn other_or_unknown_cards_keep_the_limited_passthrough() {
        for card in [
            Some("ShadowCast 2: ShadowCast 2"),
            Some("Elgato 4K S: Elgato 4K S"),
            Some("Some Brand New Grabber"),
            None,
        ] {
            assert_eq!(range_from_card(card), CaptureRange::Limited, "{card:?}");
        }
    }

    #[test]
    fn a_valid_override_wins_over_the_card() {
        let d = resolve_capture_range(Some("CAM  LINK 4K"), Some("limited"));
        assert_eq!(d.range, CaptureRange::Limited);
        assert_eq!(d.source, RangeSource::Override);
        let d = resolve_capture_range(Some("ShadowCast 2"), Some(" FULL "));
        assert_eq!(d.range, CaptureRange::Full);
        assert_eq!(d.source, RangeSource::Override);
    }

    #[test]
    fn an_invalid_override_is_reported_and_the_card_decides() {
        let d = resolve_capture_range(Some("CAM  LINK 4K"), Some("partial"));
        assert_eq!(d.range, CaptureRange::Full);
        assert_eq!(d.source, RangeSource::Card);
        assert_eq!(d.invalid_override.as_deref(), Some("partial"));
        let d = resolve_capture_range(None, None);
        assert_eq!(d.range, CaptureRange::Limited);
        assert_eq!(d.source, RangeSource::Default);
        assert_eq!(d.invalid_override, None);
    }

    #[test]
    fn the_log_line_names_the_chosen_range() {
        let card = Some("CAM  LINK 4K: CAM  LINK 4K");
        let line = resolve_capture_range(card, None).log_line(card);
        assert!(
            line.starts_with("capture range: full->limited (issue 1430)"),
            "{line}"
        );
        let line = resolve_capture_range(None, None).log_line(None);
        assert!(
            line.starts_with("capture range: limited passthrough (issue 1430)"),
            "{line}"
        );
    }
}
