//! #405 / EPIC #406 — pure OBS render-budget verdict (Tier-0, default features).
//!
//! The strict gate signal for "is the OBS program render loop actually holding its
//! frame deadline". This is the REAL render-health signal — `activeFps` +
//! `averageFrameRenderTime` + `renderSkipped` (the graphics/composite loop) — NOT the
//! encoder `outputFps`, which DUPLICATES the last composite to hit the target rate and
//! stays green even when the render loop chokes. The 2026-07-02 strih 60→27fps
//! regression read green on `outputFps` while the render loop was 36 ms / 27 fps (a
//! measurement burn left ON — the full-frame readback in #404). No automatic gate
//! caught it (#405). This is that gate's logic core.
//!
//! Pure so it unit-tests on default features (Tier-0) and is the single source of
//! truth the rig E2E (recording-e2e.sh, live OBS WS `GetStats`) calls to pass/fail
//! render health.

/// A render-loop measurement taken over a delta window from OBS WS `GetStats`.
#[derive(Debug, Clone, Copy)]
pub struct RenderSample {
    /// `activeFps` — the composite/graphics loop rate (NOT the encoder `outputFps`).
    pub active_fps: f64,
    /// `averageFrameRenderTime` in ms — time to composite one frame.
    pub avg_render_time_ms: f64,
    /// `renderSkipped / renderTotal` over the window (0.0..=1.0).
    pub render_skipped_frac: f64,
}

/// Strict render-health verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderVerdict {
    Pass,
    /// One human-readable reason per failed condition.
    Fail(Vec<String>),
}

impl RenderVerdict {
    pub fn is_pass(&self) -> bool {
        matches!(self, RenderVerdict::Pass)
    }
}

/// Classify a render sample against a target fps.
///
/// STUB (RED): not yet implemented — returns Pass unconditionally so the choke/over-budget
/// tests fail until the real logic lands.
pub fn classify(_sample: RenderSample, _target_fps: f64) -> RenderVerdict {
    RenderVerdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_60fps_passes() {
        let v = classify(
            RenderSample { active_fps: 60.0, avg_render_time_ms: 11.3, render_skipped_frac: 0.0 },
            60.0,
        );
        assert!(v.is_pass(), "healthy 60fps/11ms should pass, got {v:?}");
    }

    #[test]
    fn choked_27fps_fails() {
        // the 2026-07-02 strih regression: burn ON → 27fps / 36ms / 55% skip.
        let v = classify(
            RenderSample { active_fps: 27.5, avg_render_time_ms: 36.0, render_skipped_frac: 0.55 },
            60.0,
        );
        assert!(!v.is_pass(), "27fps/36ms choke MUST fail the render budget");
    }

    #[test]
    fn healthy_30fps_stream_passes() {
        let v = classify(
            RenderSample { active_fps: 30.0, avg_render_time_ms: 1.4, render_skipped_frac: 0.0 },
            30.0,
        );
        assert!(v.is_pass(), "healthy 30fps stream should pass, got {v:?}");
    }

    #[test]
    fn render_time_over_budget_fails_even_if_fps_ok() {
        // The encoder can show target fps while render time exceeds the deadline → still a fail.
        let v = classify(
            RenderSample { active_fps: 60.0, avg_render_time_ms: 20.0, render_skipped_frac: 0.0 },
            60.0,
        );
        assert!(!v.is_pass(), "20ms > 16.6ms budget must fail even at 60 activeFps");
    }

    #[test]
    fn any_render_skip_fails() {
        let v = classify(
            RenderSample { active_fps: 60.0, avg_render_time_ms: 10.0, render_skipped_frac: 0.01 },
            60.0,
        );
        assert!(!v.is_pass(), "any render skip in the window must fail");
    }
}
