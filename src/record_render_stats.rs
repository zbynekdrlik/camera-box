//! #1143 — OBS record-session render accounting (report-only observer-effect surface).
//!
//! The imag E2E records its OBS PROGRAM for the topology-v2 zero-loss verdict. Recording it with the
//! SOFTWARE x264 encoder overloaded the 30W-PL1-clamped imag-nb: the OBS graphics thread ran past
//! its 16.67ms budget → OBS logged ~18.4% "lagged" frames → the recording repeated ~19.5% of frames
//! (observer effect, #1130). The fix moves the record encoder to the Intel iGPU HW `ffmpeg_vaapi_tex`
//! (render held ~4ms / ~0% lagged); THIS struct is the ongoing proof it stays fixed. The harness
//! captures OBS's own stop-stats from the imag OBS log around the record window and carries them here
//! through the imag [`crate::probe::recording_partial::RecordingPartial`] (report-only — it never
//! gates `overall_pass`; a high `lagged_pct` self-attributes the confound to the RECORDER, never to
//! the delivery chain).
//!
//! This is a crate-root (default-features) module so it stays Tier-0 testable, mirroring
//! [`crate::colour_verify::NodeColourSummary`]'s carry-through-the-partial role.

use serde::{Deserialize, Serialize};

/// OBS's own record-session render stats for ONE E2E recording (report-only).
///
/// NOTE: NOT `Eq` — `lagged_pct`/`max_render_ms` are `f64` (NaN has no total order), so only
/// `PartialEq` is derived. Every use only needs `PartialEq` (`assert_eq!` et al.); a probe-gated
/// struct that gains an `f64`-carrying field must never keep an `Eq` derive (#726).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordRenderStats {
    /// OBS `Total drawn frames: N` — the frames OBS actually drew during the record.
    pub drawn_frames: u64,
    /// The `M` in OBS `Total drawn frames: N (M attempted)`; equals `drawn_frames` when OBS omits
    /// the `(… attempted)` suffix (the VAAPI-clean shape, where render kept up).
    pub attempted_frames: u64,
    /// The `L` in OBS `Number of lagged frames due to rendering lag/stalls: L (P%)`; `0` when OBS
    /// omits that line entirely — which it does when there are ~no lagged frames (the fixed state).
    pub lagged_frames: u64,
    /// OBS's own reported lag percentage (`… (P%)`); `0.0` when the lagged line is absent. This is
    /// the headline observer-effect number: ~18.4% under x264, ~0% under VAAPI-tex.
    pub lagged_pct: f64,
    /// #1143 Task 4 — the max `program-render-audit avg_frame_ms` seen DURING the record window
    /// (the render budget measured WHILE recording, not the idle preflight). `None` when the
    /// captured log slice held no audit line.
    #[serde(default)]
    pub max_render_ms: Option<f64>,
}

impl RecordRenderStats {
    /// Build from the harness-parsed counts. When `lagged_pct` is `None` it is derived from
    /// `lagged_frames / attempted_frames` (OBS's own reported percentage is preferred when present,
    /// since it carries OBS's own rounding).
    pub fn new(
        drawn_frames: u64,
        attempted_frames: u64,
        lagged_frames: u64,
        lagged_pct: Option<f64>,
        max_render_ms: Option<f64>,
    ) -> Self {
        let lagged_pct = lagged_pct.unwrap_or_else(|| {
            if attempted_frames == 0 {
                0.0
            } else {
                100.0 * (lagged_frames as f64) / (attempted_frames as f64)
            }
        });
        RecordRenderStats {
            drawn_frames,
            attempted_frames,
            lagged_frames,
            lagged_pct,
            max_render_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_lagged_pct_when_obs_percentage_absent() {
        let s = RecordRenderStats::new(964, 964, 0, None, Some(4.83));
        assert_eq!(s.lagged_frames, 0);
        assert_eq!(s.lagged_pct, 0.0);
        assert_eq!(s.max_render_ms, Some(4.83));
    }

    #[test]
    fn keeps_obs_reported_percentage_when_given() {
        let s = RecordRenderStats::new(15740, 19297, 3557, Some(18.4), Some(20.4));
        assert_eq!(s.attempted_frames, 19297);
        assert_eq!(s.lagged_pct, 18.4);
    }

    #[test]
    fn round_trips_through_json_the_harness_shape() {
        // The exact key set scripts/imag_record_encoder.parse_obs_record_stats emits.
        let j = r#"{"drawn_frames":964,"attempted_frames":964,"lagged_frames":0,
                    "lagged_pct":0.0,"max_render_ms":4.83}"#;
        let s: RecordRenderStats = serde_json::from_str(j).unwrap();
        assert_eq!(
            s,
            RecordRenderStats::new(964, 964, 0, Some(0.0), Some(4.83))
        );
    }

    #[test]
    fn max_render_ms_defaults_to_none_when_absent() {
        let s: RecordRenderStats = serde_json::from_str(
            r#"{"drawn_frames":10,"attempted_frames":10,"lagged_frames":0,"lagged_pct":0.0}"#,
        )
        .unwrap();
        assert_eq!(s.max_render_ms, None);
    }
}
