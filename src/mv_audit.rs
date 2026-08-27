//! #771 — MV fps observability: parse the `multiview-audit:` OBS log line + apply the floor.
//!
//! `render_display()` (vendored libobs, obs-display.c) emits, every ~5s per throttleable
//! Multiview projector, a line carrying its ACTUAL measured render cadence:
//!
//! ```text
//! multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080
//! ```
//!
//! so the multiview fps is VISIBLE in the OBS log and can be alarmed on a collapse (the user's
//! binding "multiview musí byť plynulé a merané" requirement). This module is the pure Tier-0
//! authority (default features, no probe/OBS/rig) for:
//!   - `mv_floor_fps` — the `target − tolerance` alarm floor (target = canvas/effective_divisor,
//!     the ~30fps-cell rate the projector actually renders at post-#776), byte-identical to the C
//!     `obs_multiview_floor_fps()` in `obs-display-budget.h` so the emitter, the E2E gate, and
//!     drift-guard all apply the SAME threshold;
//!   - `parse_audit_line` — a mutually-non-substring `multiview-audit:` marker + `key=value`
//!     token scan (the `jitter_audit.rs` parser-family convention), rejecting the genlock lines;
//!   - `classify` / `gate_log` — the pass/alarm decision the `mv-fps-gate` bin exposes to the
//!     E2E preflight / drift-guard.
//!
//! The receive-side NDI cadence is a SEPARATE, already-covered layer (`genlock-fifo audit
//! received=/consumed=`, `jitter_audit.rs`); this module is strictly the MV RENDER cadence.

/// fps jitter band subtracted below the target floor. Same 2 fps band as
/// `render_budget::FPS_TOLERANCE`; byte-identical to `MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS` in
/// `obs-display-budget.h`.
pub const MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS: f64 = 2.0;

/// #1110: the largest multiview render AREA (px) for which an fps alarm floor is CALIBRATED —
/// exactly 1080p (1920×1080 = 2_073_600). It is the only area class with a proven-healthy floor
/// (imag live: ~30fps over floor 28). A LARGER multiview (strih's 4K, 3840×2160 = 8_294_400 px)
/// is throttled by the #278/#776 budget gate to protect the 60/30fps program and cannot sustain
/// the same fps, so its floor is a non-gating report-only sentinel (0.0) pending calibration — an
/// fps-only floor would false-alarm forever (strih healthy ~16–19fps < 28). Byte-identical to
/// `MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX` in `obs-display-budget.h`.
pub const MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX: u64 = 1920 * 1080;

/// The literal log-line marker. Mutually non-substring with every `genlock-*` audit marker, so
/// all parser families can run over one log independently.
pub const MARKER: &str = "multiview-audit:";

/// The MV-fps alarm floor for a projector's TARGET rate AND render AREA (#1110): `target_fps −
/// tolerance`, clamped to `>= 0`, BUT only for a render area at or below the one CALIBRATED class
/// (1080p, `MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX`). Above it the floor is a non-gating
/// report-only sentinel `0.0`. `target_fps = canvas_fps / effective_divisor` — the ~30fps-cell rate
/// the projector actually renders at. Byte-identical to the C `obs_multiview_floor_fps()` — the
/// emitter prints this (feeding it the same `target_fps` + `cx`/`cy` it computed) and the gate
/// reads it back off the line, so they can never diverge.
///
/// #1110: a 4K multiview (3840×2160) is budget-throttled (#278/#776) and cannot hold the 1080p
/// fps, so an fps-only floor false-alarms forever (strih healthy ~16–19fps < 28) and makes the
/// mv-fps watchdog signal worthless on that box. So the floor is piecewise on area: today's floor
/// at/below 1080p (no behaviour change — imag live 29.8–30.0 over 28), a report-only sentinel above
/// it so `rendered_fps` stays measured + logged but is not gated, until a real large-area floor is
/// calibrated from ≥N samples. Only ONE 4K data point exists, so no 4K number is invented. The
/// calibrate-then-flip of the large-area floor is tracked in issue 1212.
///
/// #776 (unchanged for the calibrated class): the floor tracks the TARGET, not `canvas/2`. The
/// pre-#776 `canvas/2` model assumed every throttleable projector used divisor 2 (MV = canvas/2);
/// once #879 derives the divisor from the canvas rate, a 30fps-canvas box renders MV at divisor
/// 1 = 30fps, so `canvas/2` (= 13) is half the real target and a genuine collapse to ~14–27fps
/// would slip under it unalarmed.
pub fn mv_floor_fps(target_fps: f64, cx: u32, cy: u32) -> f64 {
    // #1110: above the one calibrated area class -> a report-only sentinel, never a false alarm.
    if (cx as u64) * (cy as u64) > MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX {
        return 0.0;
    }
    let floor = target_fps - MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS;
    if floor < 0.0 {
        0.0
    } else {
        floor
    }
}

/// Measured render cadence = real renders / window seconds. `window_ns == 0` → 0.0 (no window).
///
/// SPEC ANCHOR (#771): this is the Rust mirror of the EXACT `rendered_fps` computation the C
/// emit does inline in `render_display()` (obs-display.c):
/// `(double)display->render_audit_render_count / ((double)audit_elapsed / 1e9)`. It is not called
/// by the gate (which reads the already-emitted `rendered_fps=` off the line) — it exists, like
/// `mv_floor_fps` mirrors the C floor, so the emit's fps math has a unit-tested Rust spec. The
/// `measured_fps_is_renders_over_window_seconds` test is that spec assertion.
pub fn measured_fps(render_count: u64, window_ns: u64) -> f64 {
    if window_ns == 0 {
        return 0.0;
    }
    render_count as f64 / (window_ns as f64 / 1_000_000_000.0)
}

/// One parsed `multiview-audit:` line.
#[derive(Debug, Clone, PartialEq)]
pub struct MvAuditSample {
    pub monitor: u32,
    pub divisor: u32,
    pub rendered_fps: f64,
    pub target_fps: f64,
    pub floor_fps: f64,
    pub cx: u32,
    pub cy: u32,
}

impl MvAuditSample {
    /// The canvas rate this projector runs on, reconstructed from the line: `target × divisor`
    /// (the emitter computed `target = canvas / divisor`). `divisor == 0` → `target` unchanged.
    pub fn canvas_fps(&self) -> f64 {
        if self.divisor == 0 {
            self.target_fps
        } else {
            self.target_fps * self.divisor as f64
        }
    }
}

/// Alarm verdict for one projector's measured cadence.
#[derive(Debug, Clone, PartialEq)]
pub enum MvVerdict {
    Pass,
    /// The measured render fps fell below the floor (freeze / budget starvation / collapse).
    Below {
        rendered_fps: f64,
        floor_fps: f64,
    },
}

impl MvVerdict {
    pub fn is_pass(&self) -> bool {
        matches!(self, MvVerdict::Pass)
    }
}

/// A rendered fps at or above the floor passes; anything below (or non-finite) alarms.
pub fn classify(rendered_fps: f64, floor_fps: f64) -> MvVerdict {
    if rendered_fps.is_finite() && rendered_fps >= floor_fps {
        MvVerdict::Pass
    } else {
        MvVerdict::Below {
            rendered_fps,
            floor_fps,
        }
    }
}

/// Parse ONE line. Returns `Some` only for a genuine `multiview-audit:` line carrying the
/// required numeric fields; every other line (including the `genlock-*` audit lines and plain
/// noise) returns `None`. Unrecognized `key=value` tokens are ignored (the jitter_audit token-scan
/// convention), so the emitter can add fields later without breaking this parser.
///
/// The float fields use Rust's `.`-decimal `f64::parse` (#771, review): the C emitter always writes
/// `%.1f`/`%.0f` under OBS's C locale, so the decimal separator is always `.` — a match. A field
/// that ever failed to parse degrades safely (the whole line → `None` → `NoSamples`, never a wrong
/// number).
pub fn parse_audit_line(line: &str) -> Option<MvAuditSample> {
    let mark_at = line.find(MARKER)?;
    let rest = &line[mark_at + MARKER.len()..];

    let mut monitor: Option<u32> = None;
    let mut divisor: Option<u32> = None;
    let mut rendered_fps: Option<f64> = None;
    let mut target_fps: Option<f64> = None;
    let mut floor_fps: Option<f64> = None;
    let mut cx: Option<u32> = None;
    let mut cy: Option<u32> = None;

    for tok in rest.split_whitespace() {
        let Some((key, val)) = tok.split_once('=') else {
            continue;
        };
        match key {
            "monitor" => monitor = val.parse().ok(),
            "divisor" => divisor = val.parse().ok(),
            "rendered_fps" => rendered_fps = val.parse().ok(),
            "target" => target_fps = val.parse().ok(),
            "floor" => floor_fps = val.parse().ok(),
            "cx" => cx = val.parse().ok(),
            "cy" => cy = val.parse().ok(),
            _ => {}
        }
    }

    Some(MvAuditSample {
        monitor: monitor?,
        divisor: divisor?,
        rendered_fps: rendered_fps?,
        target_fps: target_fps?,
        floor_fps: floor_fps?,
        cx: cx?,
        cy: cy?,
    })
}

/// The LATEST `multiview-audit` sample per monitor id, in ascending monitor order — the current
/// state each projector last reported over the log.
pub fn latest_per_monitor(log_text: &str) -> Vec<MvAuditSample> {
    // Preserve last-seen per monitor.
    let mut by_monitor: Vec<MvAuditSample> = Vec::new();
    for line in log_text.lines() {
        if let Some(s) = parse_audit_line(line) {
            if let Some(existing) = by_monitor.iter_mut().find(|e| e.monitor == s.monitor) {
                *existing = s;
            } else {
                by_monitor.push(s);
            }
        }
    }
    by_monitor.sort_by_key(|s| s.monitor);
    by_monitor
}

/// Outcome of gating a whole OBS log's MV-fps audit lines.
#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    /// No `multiview-audit:` line found at all (the emitter never ran / wrong log).
    NoSamples,
    /// Every projector's latest sample is at or above its floor.
    Clean(Vec<MvAuditSample>),
    /// One or more projectors' latest sample fell below its floor.
    Breach(Vec<MvAuditSample>),
}

/// Gate a whole OBS log: take each projector's LATEST sample and alarm if any is below its own
/// printed floor. This is the decision the `mv-fps-gate` bin (E2E preflight / drift-guard
/// consumer) exposes.
///
/// FRESHNESS ASSUMPTION (#771, review): this gates the LATEST sample regardless of its age — the
/// caller is expected to pass a CURRENT log (the E2E preflight / drift-guard read the newest OBS
/// log at gate time). It carries no epoch, so a full graphics-thread STALL (OBS frozen, no new
/// audit line emitted at all) leaves the last-good sample reading PASS here; that stall class is
/// the render-liveness watchdog's job (`renderTotalFrames` advancement, `#391` /
/// obs-liveness-render-signal.md), not this per-projector cadence floor. The LIVE always-on
/// wiring + any freshness/heartbeat check is tracked in the #771 follow-up ticket.
pub fn gate_log(log_text: &str) -> GateOutcome {
    let latest = latest_per_monitor(log_text);
    if latest.is_empty() {
        return GateOutcome::NoSamples;
    }
    let breaches: Vec<MvAuditSample> = latest
        .iter()
        .filter(|s| !classify(s.rendered_fps, s.floor_fps).is_pass())
        .cloned()
        .collect();
    if breaches.is_empty() {
        GateOutcome::Clean(latest)
    } else {
        GateOutcome::Breach(breaches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_tracks_the_effective_target_not_canvas_over_two() {
        // #776: after the canvas-rate divisor derivation, BOTH broadcast boxes render
        // Multiview cells at 30fps -- strih (30fps canvas, effective divisor 1) and imag
        // (60fps canvas, effective divisor 2). The alarm floor must track that ACTUAL target
        // (30fps), NOT the pre-#776 `canvas/2` assumption -- which put strih's floor at
        // 30/2 - 2 = 13 (half the real 30fps target), so a collapse of the 30fps strih MV to
        // ~14-27fps slipped under the floor unalarmed. mv_floor_fps takes the TARGET fps.
        use crate::render_budget::effective_render_divisor;

        // strih: 30fps canvas (interval 33.3ms) -> effective divisor 1 -> target 30fps.
        let strih_target = 30.0 / effective_render_divisor(2, 33_333_333) as f64;
        // imag: 60fps canvas (interval 16.7ms) -> effective divisor 2 -> target 30fps.
        let imag_target = 60.0 / effective_render_divisor(2, 16_666_667) as f64;
        assert_eq!(strih_target, 30.0);
        assert_eq!(imag_target, 30.0);

        // Both boxes now floor at target - tolerance = 28 (the already-proven-healthy imag
        // floor; strih was wrongly 13 while its MV renders 30fps). This asserts the #776
        // TARGET-tracking within the CALIBRATED 1080p area class (area-awareness is tested
        // separately in floor_is_area_aware_report_only_above_the_calibrated_class_1110).
        assert_eq!(mv_floor_fps(strih_target, 1920, 1080), 28.0);
        assert_eq!(mv_floor_fps(imag_target, 1920, 1080), 28.0);

        // Degenerate: never a negative floor (at the calibrated 1080p area).
        assert_eq!(mv_floor_fps(2.0, 1920, 1080), 0.0); // 2 - 2 = 0 exactly
        assert_eq!(mv_floor_fps(1.0, 1920, 1080), 0.0); // clamped
        assert_eq!(mv_floor_fps(0.0, 1920, 1080), 0.0);
    }

    #[test]
    fn floor_is_area_aware_report_only_above_the_calibrated_class_1110() {
        // #1110: the alarm floor must account for the multiview's render AREA, not target fps
        // alone. A 4K multiview (3840x2160 = 8_294_400 px) cannot sustain the same fps as a 1080p
        // one (1920x1080 = 2_073_600 px) under the #278/#776 budget gate that throttles the MV to
        // protect the 60/30fps program -- strih's healthy 4K MV renders ~16-19fps (program healthy)
        // and would sit PERMANENTLY below an fps-only floor of 28, making the mv-fps watchdog signal
        // worthless on that box. So the floor is piecewise on area:
        //   - at or below the ONE calibrated area class (1080p) -> today's floor EXACTLY (28);
        //   - above it -> a report-only sentinel (0.0), so the un-calibrated large-area MV stops
        //     false-alarming while its rendered_fps stays measured + logged. No 4K number is
        //     invented (exactly one 4K data point exists -- see the #1110 design comment).

        // The baseline area constant names the only calibrated class (1920x1080).
        assert_eq!(MULTIVIEW_FLOOR_MAX_CALIBRATED_AREA_PX, 1920 * 1080);

        // 1080p (imag live: cx=1920 cy=1080) -> today's floor, byte-identical (no behaviour change).
        assert_eq!(mv_floor_fps(30.0, 1920, 1080), 28.0);
        // A smaller multiview (720p) is within the calibrated class -> today's floor unchanged.
        assert_eq!(mv_floor_fps(30.0, 1280, 720), 28.0);
        // EXACTLY the baseline area (1920*1080) -> still calibrated (the boundary is inclusive).
        assert_eq!(mv_floor_fps(30.0, 1920, 1080), 28.0);

        // 4K (strih live: cx=3840 cy=2160) -> report-only sentinel, never false-alarms.
        assert_eq!(mv_floor_fps(30.0, 3840, 2160), 0.0);
        // ONE pixel of area above the baseline -> sentinel (the boundary is strict `>`).
        assert_eq!(mv_floor_fps(30.0, 1921, 1080), 0.0);

        // A collapsed rendered_fps against a calibrated 1080p floor still alarms (classify below).
        assert!(!classify(9.0, mv_floor_fps(30.0, 1920, 1080)).is_pass());
        // The same collapse against a 4K sentinel floor is report-only (passes -- no false alarm).
        assert!(classify(9.0, mv_floor_fps(30.0, 3840, 2160)).is_pass());
    }

    #[test]
    fn measured_fps_is_renders_over_window_seconds() {
        assert!((measured_fps(150, 5_000_000_000) - 30.0).abs() < 1e-9);
        assert!((measured_fps(90, 3_000_000_000) - 30.0).abs() < 1e-9);
        assert_eq!(measured_fps(150, 0), 0.0); // no window
        assert_eq!(measured_fps(0, 5_000_000_000), 0.0); // frozen -> 0 fps
    }

    #[test]
    fn classify_passes_at_or_above_floor_and_alarms_below() {
        assert!(classify(30.0, 28.0).is_pass());
        assert!(classify(28.0, 28.0).is_pass()); // exactly at the floor passes
        assert!(!classify(27.9, 28.0).is_pass());
        assert!(!classify(0.0, 28.0).is_pass()); // frozen
        assert!(!classify(f64::NAN, 28.0).is_pass()); // non-finite alarms
    }

    #[test]
    fn parse_a_real_emitted_line() {
        let line = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080";
        let s = parse_audit_line(line).expect("should parse");
        assert_eq!(s.monitor, 1);
        assert_eq!(s.divisor, 1);
        assert!((s.rendered_fps - 30.0).abs() < 1e-9);
        assert!((s.target_fps - 30.0).abs() < 1e-9);
        assert!((s.floor_fps - 28.0).abs() < 1e-9);
        assert_eq!(s.cx, 1920);
        assert_eq!(s.cy, 1080);
        // canvas reconstructed from target*divisor; the printed floor matches mv_floor_fps applied
        // to the TARGET + this line's AREA (post-#776 the floor tracks target, not canvas -- here
        // canvas==target==30 because divisor==1; #1110: this 1920x1080 line is the calibrated area
        // class, so the floor is the ordinary target - tol = 28, byte-identical to the printed one).
        assert!((s.canvas_fps() - 30.0).abs() < 1e-9);
        assert!((mv_floor_fps(s.target_fps, s.cx, s.cy) - s.floor_fps).abs() < 1e-9);
    }

    #[test]
    fn parser_rejects_the_genlock_lines_and_noise_771() {
        // The two directions the jitter_audit family requires: the MV parser must reject the
        // genlock lines, and (asserted in jitter_audit's own tests) they reject ours — the
        // markers are mutually non-substring.
        assert!(parse_audit_line(
            "genlock-fifo audit 'Cam 1': received=300 consumed=150 underruns=0 ts_present=123"
        )
        .is_none());
        assert!(parse_audit_line(
            "genlock-ndi-output audit 'PGM': offered=300 sent=300 send_wait_ms=0"
        )
        .is_none());
        assert!(parse_audit_line("some unrelated obs log line").is_none());
        // A multiview-audit line missing a required field is not a usable sample.
        assert!(parse_audit_line("multiview-audit: monitor=1 divisor=1").is_none());
        // Marker must be substring-distinct: our marker never appears inside a genlock line.
        assert!(!"genlock-fifo audit 'x':".contains(MARKER));
    }

    #[test]
    fn latest_per_monitor_keeps_the_last_sample_per_projector() {
        let log = "\
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=30.0 target=30 floor=28.0 cx=1280 cy=720
multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=28.0 cx=1920 cy=1080
";
        let latest = latest_per_monitor(log);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].monitor, 1);
        assert!((latest[0].rendered_fps - 9.0).abs() < 1e-9); // the LAST monitor=1 sample
        assert_eq!(latest[1].monitor, 2);
    }

    #[test]
    fn gate_log_reports_nosamples_clean_and_breach() {
        assert_eq!(
            gate_log("nothing here\nanother line"),
            GateOutcome::NoSamples
        );

        let clean = "\
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720
";
        assert!(matches!(gate_log(clean), GateOutcome::Clean(v) if v.len() == 2));

        // monitor=1 collapses to 9fps (< floor 28) on its latest sample -> Breach.
        let breach = "\
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720
multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=28.0 cx=1920 cy=1080
";
        match gate_log(breach) {
            GateOutcome::Breach(b) => {
                assert_eq!(b.len(), 1);
                assert_eq!(b[0].monitor, 1);
                assert!((b[0].rendered_fps - 9.0).abs() < 1e-9);
            }
            other => panic!("expected Breach, got {other:?}"),
        }
    }

    // #1212 helpers: build a monitor's worth of `multiview-audit:` lines from a list of
    // rendered_fps values (all 4K, floor 28, in log order — latest last).
    #[cfg(test)]
    fn log_4k_monitor1(rendered: &[f64]) -> String {
        let mut s = String::new();
        for r in rendered {
            s.push_str(&format!(
                "multiview-audit: monitor=1 divisor=1 rendered_fps={r:.1} target=30 floor=28.0 cx=3840 cy=2160\n"
            ));
        }
        s
    }

    #[test]
    fn gate_does_not_false_alarm_on_a_single_bursty_dip_1212() {
        // A 4K multiview whose window MEDIAN is a healthy 30.0 but whose LATEST sample dipped to
        // 14.9 (the bursty-4K reality: individual samples dip into the teens inside a median-30
        // window). The single-latest-sample gate false-alarms; the windowed-median gate must stay
        // CLEAN. RED against the pre-#1212 single-sample gate_log (latest 14.9 < floor 28 → Breach).
        let mut fps = vec![30.0; 11];
        fps.push(14.9); // the latest sample dipped
        assert!(
            matches!(gate_log(&log_4k_monitor1(&fps)), GateOutcome::Clean(_)),
            "a single bursty dip must not breach a median-30 window (#1212)"
        );
    }

    #[test]
    fn gate_catches_a_sustained_collapse_even_when_the_latest_sample_recovered_1212() {
        // The window is a sustained collapse (11 samples at 15.0) with only the LATEST sample
        // recovered to 30.0. The single-latest-sample gate MISSES this (latest 30.0 ≥ floor 28 →
        // Clean); the windowed-median gate must BREACH (median 15.0 < 28). RED against the
        // pre-#1212 gate_log in the OPPOSITE direction — the median catches a real collapse a lone
        // recovered sample would hide.
        let mut fps = vec![15.0; 11];
        fps.push(30.0); // latest sample bounced back
        assert!(
            matches!(gate_log(&log_4k_monitor1(&fps)), GateOutcome::Breach(_)),
            "a sustained below-floor window must breach even if the latest sample recovered (#1212)"
        );
    }
}
