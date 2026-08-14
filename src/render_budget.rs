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

/// fps jitter tolerance: a healthy 60fps box measures ~59.9 activeFps over a short delta
/// window, so allow a 2 fps band. This does NOT weaken the gate — the render-time budget
/// below is the hard physical deadline, and a real choke (27 fps) misses the fps bar by
/// ~31 fps and the time budget by ~2×.
const FPS_TOLERANCE: f64 = 2.0;

/// Render-skip tolerance: a healthy box shows a tiny INCIDENTAL skip from OS scheduling
/// jitter (measured ~1/360 = 0.28% on strih at a clean 60fps/11ms). Gating at zero would
/// false-abort the E2E on that noise (a flaky gate — itself banned). 5% cleanly separates
/// incidental jitter (<1%) from a real choke (the 2026-07-02 regression skipped ~55%). This
/// is CALIBRATION above the physical artifact, not weakening — the render-time budget is
/// still the hard deadline, and any genuine spike-storm (>5%) still FAILS.
const RENDER_SKIP_TOLERANCE: f64 = 0.05;

/// Classify a render sample against a target fps. STRICT: a missed frame-time deadline, a
/// sustained fps shortfall, or a render-skip rate above incidental jitter FAILS.
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
    if !sample.render_skipped_frac.is_finite() || sample.render_skipped_frac > RENDER_SKIP_TOLERANCE
    {
        reasons.push(format!(
            "render skipped {:.2}% of frames in window (> {:.0}% tolerance — real spike-storm, \
             not incidental jitter)",
            sample.render_skipped_frac * 100.0,
            RENDER_SKIP_TOLERANCE * 100.0
        ));
    }

    if reasons.is_empty() {
        RenderVerdict::Pass
    } else {
        RenderVerdict::Fail(reasons)
    }
}

/// #879 — canvas-rate EFFECTIVE render divisor for a throttleable monitoring/aux surface.
///
/// The frontend's configured divisor (2, calibrated for 60fps-class canvases → 30fps cells) is
/// treated as a throttleable MARKER plus an UPPER BOUND. The effective cadence divisor is derived
/// from the canvas frame interval targeting ~30fps cells: `round(33.3ms / interval)`, clamped to
/// `[1, configured]`. A 60fps canvas → 2 (unchanged); a 30fps canvas → 1 (aux renders every tick,
/// so it is PURELY budget-gated — degrades only under real pressure, never unconditionally).
///
/// This is the Tier-0 authority mirrored byte-for-byte by `obs_effective_render_divisor()` in
/// `vendor/obs-studio/libobs/obs-display-budget.h` (the exact derivation `render_display()` in
/// `obs-display.c` computes inline for the projector path, #776). `interval_ns == 0` (video not
/// running) leaves the configured value untouched, matching `render_display()`.
pub fn effective_render_divisor(configured_divisor: u32, frame_interval_ns: u64) -> u32 {
    // #879 [red]: faithful pre-fix stub — no canvas-rate derivation (ignores the interval),
    // which is exactly the un-throttled behaviour before this ticket. Replaced by the real
    // derivation in the [green] commit.
    let _ = frame_interval_ns;
    configured_divisor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_60fps_with_incidental_jitter_passes() {
        // The rig-measured clean prod state: 60fps / 11.3ms with a tiny INCIDENTAL skip
        // (~1/360 = 0.28% from OS scheduling). This MUST pass — gating at zero here would
        // false-abort the E2E on a healthy box (a flaky gate).
        let v = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 11.3,
                render_skipped_frac: 0.0028,
            },
            60.0,
        );
        assert!(
            v.is_pass(),
            "healthy 60fps/11ms with 0.28% incidental skip should pass, got {v:?}"
        );
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
    fn high_render_skip_fails() {
        // A real spike-storm (20% of frames skipped) FAILS even if the average looks OK —
        // this catches spike-induced judder above the incidental-jitter tolerance.
        let v = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 10.0,
                render_skipped_frac: 0.20,
            },
            60.0,
        );
        assert!(!v.is_pass(), "20% render-skip spike-storm must fail");
    }

    #[test]
    fn skip_just_over_tolerance_fails_and_just_under_passes() {
        let over = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 10.0,
                render_skipped_frac: 0.06,
            },
            60.0,
        );
        assert!(!over.is_pass(), "6% skip (> 5% tolerance) must fail");
        let under = classify(
            RenderSample {
                active_fps: 60.0,
                avg_render_time_ms: 10.0,
                render_skipped_frac: 0.04,
            },
            60.0,
        );
        assert!(under.is_pass(), "4% skip (< 5% tolerance) must pass");
    }
}

#[cfg(test)]
mod effective_divisor_879 {
    use super::effective_render_divisor;

    /// The #776 target: ~30fps cells. A 30fps canvas (33.33ms interval) with the frontend's
    /// configured divisor 2 must resolve to an EFFECTIVE divisor of 1 — no unconditional cadence
    /// skip, so the aux surface is purely budget-gated on strih. (RED with the stub: returns 2.)
    #[test]
    fn thirty_fps_canvas_configured_2_resolves_to_1() {
        assert_eq!(effective_render_divisor(2, 33_333_333), 1);
    }

    /// A 60fps canvas (16.667ms) keeps the configured divisor 2 (→ 30fps cells).
    #[test]
    fn sixty_fps_canvas_configured_2_stays_2() {
        assert_eq!(effective_render_divisor(2, 16_666_666), 2);
    }

    /// The effective divisor never EXCEEDS the configured upper bound even on a very slow canvas.
    #[test]
    fn slow_canvas_is_clamped_to_configured_upper_bound() {
        // 10fps canvas (100ms): round(33.3/100)=0 → clamped up to 1, and 1 <= configured 3.
        assert_eq!(effective_render_divisor(3, 100_000_000), 1);
    }

    /// A configured divisor of 3 on a 90fps canvas: round(33.3/11.11)=3 → min(3,3)=3.
    #[test]
    fn ninety_fps_canvas_configured_3_stays_3() {
        assert_eq!(effective_render_divisor(3, 11_111_111), 3);
    }

    /// interval 0 (video not running) leaves the configured value untouched.
    #[test]
    fn zero_interval_returns_configured() {
        assert_eq!(effective_render_divisor(2, 0), 2);
    }

    /// The program marker (divisor 0) is never changed — min(0, derived) == 0.
    #[test]
    fn program_divisor_zero_unchanged() {
        assert_eq!(effective_render_divisor(0, 33_333_333), 0);
        assert_eq!(effective_render_divisor(0, 16_666_666), 0);
    }

    /// A configured divisor of 1 stays 1 on any canvas (min(1, derived>=1) == 1).
    #[test]
    fn configured_one_stays_one() {
        assert_eq!(effective_render_divisor(1, 33_333_333), 1);
        assert_eq!(effective_render_divisor(1, 16_666_666), 1);
    }
}
