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

/// fps jitter tolerance: a healthy 60fps box measures ~59.88 activeFps over a short
/// delta window, so allow a 1 fps band. This does NOT weaken the gate — the render-time
/// budget below is the hard physical deadline, and a real choke (27 fps) misses the fps
/// bar by ~32 fps and the time budget by ~2×.
const FPS_TOLERANCE: f64 = 1.0;

/// Classify a render sample against a target fps. STRICT: any missed frame-time deadline,
/// any sustained fps shortfall, or any skipped render frame in the window FAILS.
///
/// The frame-time budget (`1000 / target_fps` ms) is the physical deadline: exceeding it
/// means the compositor could not produce a fresh frame in time, so the encoder duplicated
/// the previous one (judder) — regardless of a green `outputFps`. Non-finite inputs fail
/// closed.
pub fn classify(sample: RenderSample, target_fps: f64) -> RenderVerdict {
    if !(target_fps.is_finite() && target_fps > 0.0) {
        return RenderVerdict::Fail(vec![format!("invalid target_fps {target_fps}")]);
    }
    let budget_ms = 1000.0 / target_fps;
    let mut reasons = Vec::new();

    if !sample.avg_render_time_ms.is_finite() || sample.avg_render_time_ms > budget_ms {
        reasons.push(format!(
            "avg render time {:.2}ms exceeds {:.2}ms frame budget @ {:.0}fps \
             (compositor missed the deadline → duplicated/juddered frames)",
            sample.avg_render_time_ms, budget_ms, target_fps
        ));
    }
    if !sample.active_fps.is_finite() || sample.active_fps < target_fps - FPS_TOLERANCE {
        reasons.push(format!(
            "active render fps {:.2} below target {:.0} (render loop not keeping up)",
            sample.active_fps, target_fps
        ));
    }
    if !sample.render_skipped_frac.is_finite() || sample.render_skipped_frac > 0.0 {
        reasons.push(format!(
            "render skipped {:.2}% of frames in window (any lagged render frame fails)",
            sample.render_skipped_frac * 100.0
        ));
    }

    if reasons.is_empty() {
        RenderVerdict::Pass
    } else {
        RenderVerdict::Fail(reasons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_60fps_passes() {
        let v = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 11.3,
                render_skipped_frac: 0.0,
            },
            60.0,
        );
        assert!(v.is_pass(), "healthy 60fps/11ms should pass, got {v:?}");
    }

    #[test]
    fn choked_27fps_fails() {
        // the 2026-07-02 strih regression: burn ON → 27fps / 36ms / 55% skip.
        let v = classify(
            RenderSample {
                active_fps: 27.5,
                avg_render_time_ms: 36.0,
                render_skipped_frac: 0.55,
            },
            60.0,
        );
        assert!(!v.is_pass(), "27fps/36ms choke MUST fail the render budget");
    }

    #[test]
    fn healthy_30fps_stream_passes() {
        let v = classify(
            RenderSample {
                active_fps: 30.0,
                avg_render_time_ms: 1.4,
                render_skipped_frac: 0.0,
            },
            30.0,
        );
        assert!(v.is_pass(), "healthy 30fps stream should pass, got {v:?}");
    }

    #[test]
    fn render_time_over_budget_fails_even_if_fps_ok() {
        // The encoder can show target fps while render time exceeds the deadline → still a fail.
        let v = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 20.0,
                render_skipped_frac: 0.0,
            },
            60.0,
        );
        assert!(
            !v.is_pass(),
            "20ms > 16.6ms budget must fail even at 60 activeFps"
        );
    }

    #[test]
    fn any_render_skip_fails() {
        let v = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 10.0,
                render_skipped_frac: 0.01,
            },
            60.0,
        );
        assert!(!v.is_pass(), "any render skip in the window must fail");
    }
}
