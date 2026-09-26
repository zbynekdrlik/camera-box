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
    // #879 [green]: derive the ~30fps-cell cadence from the canvas rate, clamped to the
    // configured upper bound. interval 0 (video not running) leaves the configured value
    // untouched, matching render_display() which skips the derivation when interval == 0.
    if frame_interval_ns == 0 {
        return configured_divisor;
    }
    const TARGET_CELL_INTERVAL_NS: u64 = 33_333_333; // ~30fps cells
    let derived = ((TARGET_CELL_INTERVAL_NS + frame_interval_ns / 2) / frame_interval_ns) as u32;
    let derived = if derived < 1 { 1 } else { derived };
    if derived < configured_divisor {
        derived
    } else {
        configured_divisor
    }
}

/// issue 1346 — `OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS` (`obs-display-budget.h`, the #293
/// anti-starvation cap): an over-budget monitoring surface renders on the (K+1)-th tick.
pub const MAX_CONSECUTIVE_SKIPS: u32 = 3;

/// issue 1346 — Tier-0 mirror of the pure `obs_display_should_skip()` decision in
/// `vendor/obs-studio/libobs/obs-display-budget.h` (#278/#293/#756/#776), which both the Multiview
/// projector and the aux/monitoring-surface gate below delegate to. `true` = skip this tick.
///
/// Divisor 0 (program/preview) never skips; a not-warmed surface (`ewma_ns == 0`) renders once to
/// measure; a warmed throttleable surface skips off-cadence ticks; a tick whose `elapsed + ewma`
/// fits the budget renders; an over-budget tick skips until `MAX_CONSECUTIVE_SKIPS` in a row.
/// `wrapping_add` is the C `uint64_t` sum (no overflow in the ns domain; exact parity).
pub fn display_should_skip(
    render_divisor: u32,
    frame_counter: u32,
    ewma_ns: u64,
    elapsed_ns: u64,
    budget_ns: u64,
    consecutive_skips: u32,
) -> bool {
    if render_divisor < 1 || ewma_ns == 0 {
        return false;
    }
    if render_divisor > 1 && !frame_counter.is_multiple_of(render_divisor) {
        return true;
    }
    if elapsed_ns.wrapping_add(ewma_ns) <= budget_ns {
        return false;
    }
    consecutive_skips < MAX_CONSECUTIVE_SKIPS
}

/// issue 1346 — the graphics-tick clock the aux gate reads: `obs->video.video_frame_interval_ns`,
/// `obs->video.graphics_frame_start_ns`, `os_gettime_ns()` and `obs->video.last_tick_total_ns`
/// (the PREVIOUS tick's completed total, #1063).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxTickClock {
    pub interval_ns: u64,
    pub tick_start_ns: u64,
    pub now_ns: u64,
    pub last_tick_total_ns: u64,
}

/// issue 1346 — the budget's "already consumed" term of the aux gate:
/// `max(elapsed, last_tick_total − self_last)` with a saturating subtraction.
///
/// `last_tick_total` is the WHOLE previous graphics tick, and that includes the caller's OWN render
/// of the previous tick when it rendered. The caller's cost is already its `ewma`, so counting it
/// again in `consumed` made a view that fits the budget skip every other tick (the vk-direct HDMI
/// Multiview at 15 fps instead of 30). `self_last_ns == 0` is the #1063 term unchanged.
pub fn aux_consumed_ns(elapsed_ns: u64, last_tick_total_ns: u64, self_last_ns: u64) -> u64 {
    let previous_tick_rest = last_tick_total_ns.saturating_sub(self_last_ns);
    elapsed_ns.max(previous_tick_rest)
}

/// issue 1346 — Tier-0 mirror of `obs_aux_sender_should_skip_excluding()` in
/// `vendor/obs-studio/libobs/obs.c` (`obs_aux_sender_should_skip()` is its `self_last_ns = 0`
/// case, the #879 aux ndi_filter senders). Never-warmed / no interval / not ticking -> render;
/// else the canvas-rate effective divisor, `consumed` from [`aux_consumed_ns`] and a 90 % budget
/// go to [`display_should_skip`]. Parity with the shipped C: `tests/drm_output_view_mv_budget_1346.rs`.
pub fn aux_sender_should_skip_excluding(
    render_divisor: u32,
    frame_counter: u32,
    ewma_ns: u64,
    consecutive_skips: u32,
    self_last_ns: u64,
    clock: AuxTickClock,
) -> bool {
    if ewma_ns == 0 || clock.interval_ns == 0 || clock.tick_start_ns == 0 {
        return false;
    }
    let effective_divisor = effective_render_divisor(render_divisor, clock.interval_ns);
    let elapsed = clock.now_ns.saturating_sub(clock.tick_start_ns);
    let consumed = aux_consumed_ns(elapsed, clock.last_tick_total_ns, self_last_ns);
    let budget = clock.interval_ns - clock.interval_ns / 10; // 90% safety margin
    display_should_skip(
        effective_divisor,
        frame_counter,
        ewma_ns,
        consumed,
        budget,
        consecutive_skips,
    )
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

#[cfg(test)]
mod aux_self_exclusion_1346 {
    use super::{
        aux_consumed_ns, aux_sender_should_skip_excluding, AuxTickClock, MAX_CONSECUTIVE_SKIPS,
    };

    const IV30: u64 = 33_333_333;
    const MS: u64 = 1_000_000;

    /// The DRM-output view's per-tick loop (`drm_output_view_frame`): the gate sees `elapsed = pre`
    /// (the tick's work before the view), the PREVIOUS tick's total (`pre + mv` when it rendered),
    /// the measured `ewma = mv`, and — with `exclude_self` — its own previous render handed over
    /// once (0 after a skip). Returns the renders over `ticks`.
    fn simulate_view(pre_ns: u64, mv_ns: u64, ticks: u32, exclude_self: bool) -> u32 {
        let (mut last_total, mut last_render, mut fc, mut cs, mut renders) =
            (0u64, 0u64, 0u32, 0u32, 0u32);
        let mut t = 1000u64;
        for _ in 0..ticks {
            fc += 1;
            let self_last = if exclude_self { last_render } else { 0 };
            last_render = 0;
            let clock = AuxTickClock {
                interval_ns: IV30,
                tick_start_ns: t,
                now_ns: t + pre_ns,
                last_tick_total_ns: last_total,
            };
            let skip = aux_sender_should_skip_excluding(2, fc, mv_ns, cs, self_last, clock);
            let mut total = pre_ns;
            if skip {
                cs += 1;
            } else {
                renders += 1;
                cs = 0;
                total += mv_ns;
                last_render = mv_ns;
            }
            last_total = total;
            t += IV30;
        }
        renders
    }

    /// The live strih-lx numbers (25.9.2026): pre 15 ms + a 14 ms Multiview fits the 30 ms budget,
    /// so the view renders EVERY tick = 30 fps, not the render/skip alternation (15 fps).
    #[test]
    fn live_numbers_render_every_tick_when_the_own_render_is_excluded() {
        assert_eq!(simulate_view(15 * MS, 14 * MS, 30, true), 30);
    }

    /// The unchanged #1063 term (`self_last = 0`, the aux ndi_filter senders) is what produced the
    /// live 15 fps — pinned so the fix is visibly the self-exclusion, nothing else.
    #[test]
    fn without_self_exclusion_the_live_numbers_alternate() {
        assert_eq!(simulate_view(15 * MS, 14 * MS, 30, false), 15);
    }

    /// A genuinely heavy tick (pre 25 ms + mv 14 ms = 39 ms > 30 ms) still skips: only the #293
    /// floor renders, every (K+1)-th tick, with or without the self-exclusion.
    #[test]
    fn a_heavy_tick_still_skips_to_the_anti_starvation_floor() {
        let floor = 30 / (MAX_CONSECUTIVE_SKIPS + 1);
        assert_eq!(simulate_view(25 * MS, 14 * MS, 30, true), floor);
        assert_eq!(simulate_view(25 * MS, 14 * MS, 30, false), floor);
    }

    #[test]
    fn consumed_subtracts_only_the_own_previous_render() {
        assert_eq!(aux_consumed_ns(15 * MS, 29 * MS, 14 * MS), 15 * MS);
        // elapsed still wins when this tick is already heavier than the rest of the previous one
        assert_eq!(aux_consumed_ns(20 * MS, 29 * MS, 14 * MS), 20 * MS);
        // saturating: a self cost above the recorded total never wraps to a huge consumed
        assert_eq!(aux_consumed_ns(3 * MS, 10 * MS, 14 * MS), 3 * MS);
        // self 0 is the #1063 max term, byte-identical
        for (e, l) in [(0, 0), (3 * MS, 28 * MS), (28 * MS, 3 * MS), (5, 5)] {
            assert_eq!(aux_consumed_ns(e, l, 0), e.max(l));
        }
    }

    /// Program priority survives: divisor 0, a not-warmed surface and a stopped clock never skip,
    /// whatever the self term.
    #[test]
    fn program_warmup_and_stopped_clock_never_skip() {
        let heavy = AuxTickClock {
            interval_ns: IV30,
            tick_start_ns: 1000,
            now_ns: 1000 + 28 * MS,
            last_tick_total_ns: 40 * MS,
        };
        for self_last in [0, 14 * MS, 80 * MS] {
            assert!(!aux_sender_should_skip_excluding(
                0,
                1,
                5 * MS,
                0,
                self_last,
                heavy
            ));
            assert!(!aux_sender_should_skip_excluding(
                2, 1, 0, 0, self_last, heavy
            ));
            let stopped = AuxTickClock {
                tick_start_ns: 0,
                ..heavy
            };
            assert!(!aux_sender_should_skip_excluding(
                2,
                1,
                5 * MS,
                0,
                self_last,
                stopped
            ));
            let no_video = AuxTickClock {
                interval_ns: 0,
                ..heavy
            };
            assert!(!aux_sender_should_skip_excluding(
                2,
                1,
                5 * MS,
                0,
                self_last,
                no_video
            ));
        }
    }
}
