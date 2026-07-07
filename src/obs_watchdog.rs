//! #391 — pure broadcast-OBS liveness/wedge verdict (Tier-0, default features).
//!
//! Stream OBS (10.77.9.204) was hung "(Not Responding)" for ~25 hours (obs64 pegged at
//! ~168% CPU, `Responding=False`, 16.0% frames-missed-due-to-render-lag) and NOTHING
//! detected it — the user found it manually a day later. This module is the strict
//! detection kernel for the watchdog that closes that gap: fed a `GetStats`-derived
//! sample (always available from a dev1 systemd timer over OBS WebSocket — no ssh/MCP
//! needed) plus OPTIONAL process-level signals (obs64 count / `Responding` / CPU%,
//! which need the win-* MCP and are therefore agent-only, not timer-only), it returns
//! one of the five verdicts named in #391: HEALTHY / WEDGED-RENDER-LAG / WS-DEAD /
//! FPS-ZERO / OBS-COUNT-WRONG.
//!
//! Mirrors `render_budget.rs`'s shape (pure `classify`, Tier-0 unit-tested, single
//! source of truth for the threshold) so the watchdog's Python/bash front is a thin
//! measure-and-pipe layer, never re-implementing the decision.
//!
//! Pure so it unit-tests on default features (Tier-0) — no OBS, no network, no MCP.

/// A liveness/wedge sample for one broadcast OBS box, gathered by the watchdog front.
///
/// `ws_reachable` + the `GetStats` fields are ALWAYS obtainable from a plain dev1
/// systemd timer (OBS WebSocket is network-reachable, no ssh/MCP required). The
/// `obs64_count` / `responding` / `cpu_percent` fields are Windows PROCESS signals —
/// only available when an agent samples them via the win-* MCP — so they are
/// `Option`: `None` means "not sampled this pass", never "known healthy".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObsHealthSample {
    /// Could the watchdog connect + authenticate to OBS WebSocket and get a `GetStats`
    /// reply at all this pass.
    pub ws_reachable: bool,
    /// `activeFps` from `GetStats` — the composite/graphics loop rate (NOT the encoder
    /// `outputFps`, which duplicates to target and stays green while render chokes).
    pub active_fps: Option<f64>,
    /// `averageFrameRenderTime` in ms from `GetStats`.
    pub avg_render_time_ms: Option<f64>,
    /// `renderSkipped / renderTotal` delta fraction (0.0..=1.0) over the measurement
    /// window (mirrors `render_budget::RenderSample::render_skipped_frac`).
    pub render_skipped_frac: Option<f64>,
    /// Windows process count of `obs64.exe` on this box. Agent/MCP-only signal.
    pub obs64_count: Option<u32>,
    /// Win32 "IsHungAppWindow"-style Responding flag for the obs64 main window.
    /// Agent/MCP-only signal. The #391 incident's headline signature.
    pub responding: Option<bool>,
    /// Sustained obs64.exe CPU percent (e.g. `Get-Process obs64 | Select CPU` deltas).
    /// Agent/MCP-only signal. The #391 incident measured ~168%.
    pub cpu_percent: Option<f64>,
    /// True when this pass's OBS-log audit found the DXGI device-lost signature (#89 —
    /// `crate::dxgi_device_lost::line_is_device_lost`, the SAME matcher `probe::obs_log_audit`
    /// uses for the #81 harness-side diagnosis). Agent/MCP-only signal (reading the OBS log
    /// needs the win-* MCP or a local read on the self-heal box) — `None` means "not sampled
    /// this pass", never "known absent".
    pub dxgi_device_lost: Option<bool>,
}

/// Strict liveness verdict — the five states named in #391.
#[derive(Debug, Clone, PartialEq)]
pub enum ObsHealthVerdict {
    Healthy,
    /// obs64's render/UI loop is wedged: `Responding=False`, CPU pegged, a render-skip
    /// spike, or an avg-render-time that blows the frame budget. One reason per cause.
    WedgedRenderLag(Vec<String>),
    /// OBS WebSocket did not respond to `GetStats` this pass — box unreachable/wedged
    /// hard enough that even the WS thread is gone, or the box/network is down.
    WsDead(String),
    /// WS answered but `activeFps` is ~0 — the render loop has stalled while the
    /// websocket thread is still alive (a distinct wedge mode from a slow-but-live render).
    FpsZero(String),
    /// The exactly-one-obs64 invariant is broken (0 or >1 processes).
    ObsCountWrong(String),
    /// The OBS log carries the DXGI device-lost signature (#89 — a GPU TDR / driver-internal-
    /// error). Distinct from `WedgedRenderLag`: obs64's process/render-loop signals can look
    /// perfectly healthy while the GPU device itself is gone, and — critically — the correct
    /// recovery is NOT the same (a plain OBS restart typically does NOT clear this; a full PC
    /// reboot is required, unlike a `WedgedRenderLag` process wedge).
    GpuDeviceRemoved(String),
}

impl ObsHealthVerdict {
    pub fn is_healthy(&self) -> bool {
        matches!(self, ObsHealthVerdict::Healthy)
    }

    /// The exact label vocabulary from the #391 issue text — used verbatim by the
    /// binary/CLI front and the Discord alert body.
    pub fn label(&self) -> &'static str {
        match self {
            ObsHealthVerdict::Healthy => "HEALTHY",
            ObsHealthVerdict::WedgedRenderLag(_) => "WEDGED-RENDER-LAG",
            ObsHealthVerdict::WsDead(_) => "WS-DEAD",
            ObsHealthVerdict::FpsZero(_) => "FPS-ZERO",
            ObsHealthVerdict::ObsCountWrong(_) => "OBS-COUNT-WRONG",
            ObsHealthVerdict::GpuDeviceRemoved(_) => "GPU-DEVICE-REMOVED",
        }
    }

    /// Human-readable reasons (empty for `Healthy`).
    pub fn reasons(&self) -> Vec<String> {
        match self {
            ObsHealthVerdict::Healthy => Vec::new(),
            ObsHealthVerdict::WedgedRenderLag(r) => r.clone(),
            ObsHealthVerdict::WsDead(r)
            | ObsHealthVerdict::FpsZero(r)
            | ObsHealthVerdict::ObsCountWrong(r)
            | ObsHealthVerdict::GpuDeviceRemoved(r) => vec![r.clone()],
        }
    }
}

/// obs64 CPU percent considered "pegged" (sustained, single-thread-wedge class of load).
/// The #391 incident measured ~168%; 120% cleanly separates a genuinely wedged UI/render
/// thread from normal encode+render load (a healthy box on this rig runs single-digit to
/// low-double-digit percent per the obs-ops skill's "Healthy OBS Proof").
const CPU_PEGGED_PERCENT: f64 = 120.0;

/// Render-skip tolerance — same calibration as `render_budget::RENDER_SKIP_TOLERANCE`
/// (incidental OS-scheduling jitter measures well under 1% on a healthy box; the #391
/// incident measured 16.0% frames-missed-due-to-render-lag).
const RENDER_SKIP_TOLERANCE: f64 = 0.05;

/// A render time this many times the frame budget is a real choke, not incidental
/// jitter (mirrors the intent of `render_budget::classify`'s hard frame-deadline check,
/// loosened 2x here since this gate also fires on the WEDGED-process signals and must
/// not double-flag a merely-busy-but-fine render pass).
const RENDER_TIME_BUDGET_MULTIPLIER: f64 = 2.0;

/// `activeFps` below this while WS is reachable = the render loop has stalled.
const FPS_ZERO_THRESHOLD: f64 = 1.0;

/// Classify one box's liveness sample. STRICT: any of the #391/#89 detection signals
/// (`Responding=False`, pegged CPU, render-skip spike, blown render-time budget,
/// WS unreachable, wrong obs64 process count, stalled activeFps, DXGI device-lost log
/// signature) FAILS. Priority order mirrors how decisive each signal is: a wrong process
/// count is checked first (it can be sampled even when WS is fully dead), then WS
/// reachability, then the GPU device-removed log signature (#89 — decisive BEFORE the
/// process-level wedge bundle, since it needs a DIFFERENT recovery and a GPU-gone box can
/// still look process-healthy), then the wedge signals, then the fps-stall signal.
/// Non-finite optional numeric inputs are treated as absent (never silently coerced to a
/// passing value).
pub fn classify(sample: ObsHealthSample, target_fps: f64) -> ObsHealthVerdict {
    // 1. The exactly-one-obs64 invariant — decisive on its own, checked first because it
    //    can be known even when the WS side is completely dead (0 processes).
    if let Some(count) = sample.obs64_count {
        if count != 1 {
            return ObsHealthVerdict::ObsCountWrong(format!(
                "obs64 process count = {count} (expected exactly 1)"
            ));
        }
    }

    // 2. WS unreachable this pass — no render telemetry at all.
    if !sample.ws_reachable {
        return ObsHealthVerdict::WsDead(
            "OBS WebSocket unreachable / GetStats did not respond this pass".to_string(),
        );
    }

    // 3. GPU device-removed (DXGI TDR / driver-internal-error) log signature (#89) —
    //    decisive on its own, checked BEFORE the process-level wedge bundle below. Once the
    //    GPU is gone, obs64's process/render-loop signals can still look perfectly healthy
    //    (Responding=True, normal CPU) while every render call fails — so this must win before
    //    the wedge bundle could otherwise report WEDGED-RENDER-LAG, which is the WRONG
    //    diagnosis for this cause: WedgedRenderLag implies a plain OBS restart likely fixes it,
    //    but an OBS-only restart typically does NOT clear a DXGI device-removed GPU (a full PC
    //    reboot is required — see `probe::obs_log_audit::ObsLogAudit::diagnosis`).
    if sample.dxgi_device_lost == Some(true) {
        return ObsHealthVerdict::GpuDeviceRemoved(
            "DXGI device-lost signature (887A0005/6/7) found in the OBS log — GPU TDR / \
             driver-internal-error; an OBS-only restart typically does NOT clear this, a full \
             PC reboot is required (#89)"
                .to_string(),
        );
    }

    // 4. Process-level + render-loop wedge signals. Collected together because they are
    //    all evidence of the SAME underlying condition (the #391 incident showed all
    //    three at once: Responding=False + pegged CPU + a render-skip spike).
    let mut wedge_reasons = Vec::new();

    if sample.responding == Some(false) {
        wedge_reasons.push("obs64 Responding=False (UI/render thread wedged)".to_string());
    }

    if let Some(cpu) = sample.cpu_percent {
        if cpu.is_finite() && cpu >= CPU_PEGGED_PERCENT {
            wedge_reasons.push(format!(
                "obs64 CPU {cpu:.0}% >= {CPU_PEGGED_PERCENT:.0}% pegged threshold"
            ));
        }
    }

    if let Some(frac) = sample.render_skipped_frac {
        if frac.is_finite() && frac > RENDER_SKIP_TOLERANCE {
            wedge_reasons.push(format!(
                "render skipped {:.1}% of frames in window (> {:.0}% tolerance)",
                frac * 100.0,
                RENDER_SKIP_TOLERANCE * 100.0
            ));
        }
    }

    if let Some(ms) = sample.avg_render_time_ms {
        if target_fps.is_finite() && target_fps > 0.0 && ms.is_finite() {
            let budget_ms = 1000.0 / target_fps;
            let hard_budget_ms = budget_ms * RENDER_TIME_BUDGET_MULTIPLIER;
            if ms > hard_budget_ms {
                wedge_reasons.push(format!(
                    "avg render time {ms:.1}ms > {hard_budget_ms:.1}ms ({RENDER_TIME_BUDGET_MULTIPLIER:.0}x frame budget @ {target_fps:.0}fps)"
                ));
            }
        }
    }

    if !wedge_reasons.is_empty() {
        return ObsHealthVerdict::WedgedRenderLag(wedge_reasons);
    }

    // 5. FPS stalled while WS is still answering — a distinct wedge mode (the render
    //    loop stopped producing frames but the websocket thread is alive).
    if let Some(fps) = sample.active_fps {
        if fps.is_finite() && fps < FPS_ZERO_THRESHOLD {
            return ObsHealthVerdict::FpsZero(format!(
                "activeFps {fps:.2} < {FPS_ZERO_THRESHOLD:.1} while WS reachable"
            ));
        }
    }

    ObsHealthVerdict::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_ws_only(
        active_fps: f64,
        avg_render_time_ms: f64,
        render_skipped_frac: f64,
    ) -> ObsHealthSample {
        ObsHealthSample {
            ws_reachable: true,
            active_fps: Some(active_fps),
            avg_render_time_ms: Some(avg_render_time_ms),
            render_skipped_frac: Some(render_skipped_frac),
            obs64_count: None,
            responding: None,
            cpu_percent: None,
            dxgi_device_lost: None,
        }
    }

    #[test]
    fn healthy_ws_only_sample_passes() {
        // A dev1 timer with NO process-level (agent/MCP) signals — only GetStats.
        let v = classify(healthy_ws_only(60.0, 11.3, 0.003), 60.0);
        assert!(
            v.is_healthy(),
            "healthy 60fps WS-only sample should pass, got {v:?}"
        );
        assert_eq!(v.label(), "HEALTHY");
    }

    #[test]
    fn healthy_30fps_stream_target_passes() {
        let v = classify(healthy_ws_only(30.0, 1.4, 0.0), 30.0);
        assert!(
            v.is_healthy(),
            "healthy 30fps sample should pass, got {v:?}"
        );
    }

    #[test]
    fn real_25h_stream_wedge_incident_fails_wedged_render_lag() {
        // The actual #391 incident: obs64 pegged ~168% CPU, Responding=False, 16.0%
        // frames-missed-due-to-render-lag, WS still answering GetStats (that's how the
        // 16.0% figure was observed at all).
        let sample = ObsHealthSample {
            ws_reachable: true,
            active_fps: Some(29.5),
            avg_render_time_ms: Some(9.0),
            render_skipped_frac: Some(0.16),
            obs64_count: Some(1),
            responding: Some(false),
            cpu_percent: Some(168.0),
            dxgi_device_lost: None,
        };
        let v = classify(sample, 30.0);
        match &v {
            ObsHealthVerdict::WedgedRenderLag(reasons) => {
                assert!(reasons.iter().any(|r| r.contains("Responding=False")));
                assert!(reasons.iter().any(|r| r.contains("CPU")));
                assert!(reasons.iter().any(|r| r.contains("render skipped")));
            }
            other => {
                panic!("real #391 incident sample MUST classify WEDGED-RENDER-LAG, got {other:?}")
            }
        }
        assert_eq!(v.label(), "WEDGED-RENDER-LAG");
    }

    #[test]
    fn ws_unreachable_fails_ws_dead() {
        let sample = ObsHealthSample {
            ws_reachable: false,
            active_fps: None,
            avg_render_time_ms: None,
            render_skipped_frac: None,
            obs64_count: None,
            responding: None,
            cpu_percent: None,
            dxgi_device_lost: None,
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "WS-DEAD");
    }

    #[test]
    fn obs_count_zero_fails_before_ws_check() {
        // 0 processes implies WS would also be unreachable, but the count check must win
        // (it is the more decisive, agent-verified signal) and report OBS-COUNT-WRONG,
        // not WS-DEAD.
        let sample = ObsHealthSample {
            ws_reachable: false,
            obs64_count: Some(0),
            ..Default::default()
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "OBS-COUNT-WRONG");
    }

    #[test]
    fn obs_count_two_fails_even_with_healthy_ws() {
        let sample = ObsHealthSample {
            obs64_count: Some(2),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "OBS-COUNT-WRONG");
    }

    #[test]
    fn fps_near_zero_while_ws_alive_fails_fps_zero() {
        let sample = ObsHealthSample {
            ws_reachable: true,
            active_fps: Some(0.0),
            avg_render_time_ms: Some(0.0),
            render_skipped_frac: Some(0.0),
            obs64_count: None,
            responding: None,
            cpu_percent: None,
            dxgi_device_lost: None,
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "FPS-ZERO");
    }

    #[test]
    fn responding_false_alone_fails_even_with_clean_render_stats() {
        // The headline #391 signal in isolation — WS looks otherwise clean, only the
        // process-level Responding flag is bad.
        let sample = ObsHealthSample {
            responding: Some(false),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "WEDGED-RENDER-LAG");
    }

    #[test]
    fn cpu_pegged_alone_fails_even_with_clean_render_stats() {
        let sample = ObsHealthSample {
            cpu_percent: Some(150.0),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert_eq!(v.label(), "WEDGED-RENDER-LAG");
    }

    #[test]
    fn cpu_just_under_threshold_passes_and_just_over_fails() {
        let under = classify(
            ObsHealthSample {
                cpu_percent: Some(119.9),
                ..healthy_ws_only(60.0, 10.0, 0.0)
            },
            60.0,
        );
        assert!(
            under.is_healthy(),
            "119.9% CPU (< 120% threshold) should pass, got {under:?}"
        );

        let over = classify(
            ObsHealthSample {
                cpu_percent: Some(120.1),
                ..healthy_ws_only(60.0, 10.0, 0.0)
            },
            60.0,
        );
        assert!(
            !over.is_healthy(),
            "120.1% CPU (>= 120% threshold) must fail"
        );
    }

    #[test]
    fn render_skip_just_over_tolerance_fails_and_just_under_passes() {
        let over = classify(healthy_ws_only(60.0, 10.0, 0.06), 60.0);
        assert!(
            !over.is_healthy(),
            "6% render-skip (> 5% tolerance) must fail"
        );
        let under = classify(healthy_ws_only(60.0, 10.0, 0.04), 60.0);
        assert!(
            under.is_healthy(),
            "4% render-skip (< 5% tolerance) should pass"
        );
    }

    #[test]
    fn render_time_over_2x_budget_fails() {
        // 2x the 60fps (16.6ms) budget = 33.3ms.
        let v = classify(healthy_ws_only(60.0, 34.0, 0.0), 60.0);
        assert!(!v.is_healthy(), "34ms (> 2x 16.6ms budget) must fail");
    }

    #[test]
    fn render_time_under_2x_budget_passes() {
        let v = classify(healthy_ws_only(60.0, 20.0, 0.0), 60.0);
        assert!(
            v.is_healthy(),
            "20ms (< 2x 16.6ms budget @ 60fps) should pass, got {v:?}"
        );
    }

    #[test]
    fn non_finite_optional_fields_never_silently_pass_when_decisive() {
        // A NaN/inf CPU reading must never be treated as "healthy" by falling through —
        // it is simply excluded from the wedge check (fail-closed lives in the
        // reachability/count checks; a garbage optional gauge does not by itself flip a
        // genuinely healthy box to unhealthy, but it must never mask a real problem).
        let sample = ObsHealthSample {
            cpu_percent: Some(f64::NAN),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert!(
            v.is_healthy(),
            "a NaN gauge alongside otherwise-clean stats should pass, got {v:?}"
        );
    }

    #[test]
    fn default_sample_with_no_signals_is_healthy() {
        // ws_reachable defaults to false in Default, so this documents the one
        // exception: an all-absent sample must be treated as WS-DEAD, never HEALTHY —
        // "no data" is not "known healthy".
        let v = classify(ObsHealthSample::default(), 60.0);
        assert_eq!(
            v.label(),
            "WS-DEAD",
            "an empty/no-signal sample must never read as HEALTHY"
        );
    }

    #[test]
    fn dxgi_device_lost_signature_fails_gpu_device_removed_even_with_clean_process_stats() {
        // #89: a GPU device-removed/TDR can leave obs64's OWN process/render-loop signals
        // looking perfectly healthy (the process is alive, CPU/render stats are clean) while
        // the compositor cannot create textures at all. The OBS-log DXGI audit is the ONLY
        // signal that catches this — it must be decisive on its own.
        let sample = ObsHealthSample {
            dxgi_device_lost: Some(true),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert_eq!(
            v.label(),
            "GPU-DEVICE-REMOVED",
            "a confirmed DXGI device-lost log signature must classify GPU-DEVICE-REMOVED even \
             with otherwise-clean render/process stats, got {v:?}"
        );
    }

    #[test]
    fn dxgi_device_lost_wins_over_process_level_wedge_signals() {
        // The GPU-device-removed diagnosis must be checked BEFORE the process-level wedge
        // bundle: a WedgedRenderLag verdict implies a plain OBS restart likely fixes it, which
        // is WRONG guidance for a GPU TDR (a full PC reboot is required) — so when both signals
        // are present, GpuDeviceRemoved must win, never get masked by WedgedRenderLag.
        let sample = ObsHealthSample {
            dxgi_device_lost: Some(true),
            responding: Some(false),
            cpu_percent: Some(168.0),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        let v = classify(sample, 60.0);
        assert_eq!(
            v.label(),
            "GPU-DEVICE-REMOVED",
            "dxgi_device_lost must win over process-level wedge signals, got {v:?}"
        );
    }

    #[test]
    fn dxgi_device_lost_false_or_absent_does_not_trigger_gpu_device_removed() {
        let false_sample = ObsHealthSample {
            dxgi_device_lost: Some(false),
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        assert!(classify(false_sample, 60.0).is_healthy());

        let absent_sample = ObsHealthSample {
            dxgi_device_lost: None,
            ..healthy_ws_only(60.0, 10.0, 0.0)
        };
        assert!(classify(absent_sample, 60.0).is_healthy());
    }
}
