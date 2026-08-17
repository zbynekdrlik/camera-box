//! #771 — MV fps observability: parse the `multiview-audit:` OBS log line + apply the floor.
//!
//! `render_display()` (vendored libobs, obs-display.c) emits, every ~5s per throttleable
//! Multiview projector, a line carrying its ACTUAL measured render cadence:
//!
//! ```text
//! multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=1920 cy=1080
//! ```
//!
//! so the multiview fps is VISIBLE in the OBS log and can be alarmed on a collapse (the user's
//! binding "multiview musí byť plynulé a merané" requirement). This module is the pure Tier-0
//! authority (default features, no probe/OBS/rig) for:
//!   - `mv_floor_fps` — the `canvas/2 − tolerance` alarm floor, byte-identical to the C
//!     `obs_multiview_floor_fps()` in `obs-display-budget.h` so the emitter, the E2E gate, and
//!     drift-guard all apply the SAME threshold;
//!   - `parse_audit_line` — a mutually-non-substring `multiview-audit:` marker + `key=value`
//!     token scan (the `jitter_audit.rs` parser-family convention), rejecting the genlock lines;
//!   - `classify` / `gate_log` — the pass/alarm decision the `mv-fps-gate` bin exposes to the
//!     E2E preflight / drift-guard.
//!
//! The receive-side NDI cadence is a SEPARATE, already-covered layer (`genlock-fifo audit
//! received=/consumed=`, `jitter_audit.rs`); this module is strictly the MV RENDER cadence.

/// fps jitter band subtracted below the `canvas/2` floor. Same 2 fps band as
/// `render_budget::FPS_TOLERANCE`; byte-identical to `MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS` in
/// `obs-display-budget.h`.
pub const MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS: f64 = 2.0;

/// The literal log-line marker. Mutually non-substring with every `genlock-*` audit marker, so
/// all parser families can run over one log independently.
pub const MARKER: &str = "multiview-audit:";

/// The MV-fps alarm floor for a canvas rate: `canvas_fps/2 − tolerance` (#771 design), clamped
/// to `>= 0`. Byte-identical to the C `obs_multiview_floor_fps()` — the emitter prints this and
/// the gate re-derives it, so they can never diverge. The `canvas/2` model is the design
/// default; the exact floor is calibrated against a measured-healthy rig (the ticket flags this).
pub fn mv_floor_fps(canvas_fps: f64) -> f64 {
    let floor = canvas_fps / 2.0 - MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS;
    if floor < 0.0 {
        0.0
    } else {
        floor
    }
}

/// Measured render cadence = real renders / window seconds. `window_ns == 0` → 0.0 (no window).
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
    fn floor_matches_the_canvas_over_two_minus_tolerance_model() {
        // strih 30fps canvas -> 30/2 - 2 = 13
        assert_eq!(mv_floor_fps(30.0), 13.0);
        // imag 60fps canvas -> 60/2 - 2 = 28
        assert_eq!(mv_floor_fps(60.0), 28.0);
        // degenerate low canvas never yields a negative floor
        assert_eq!(mv_floor_fps(1.0), 0.0);
        assert_eq!(mv_floor_fps(0.0), 0.0);
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
        assert!(classify(30.0, 13.0).is_pass());
        assert!(classify(13.0, 13.0).is_pass()); // exactly at the floor passes
        assert!(!classify(12.9, 13.0).is_pass());
        assert!(!classify(0.0, 13.0).is_pass()); // frozen
        assert!(!classify(f64::NAN, 13.0).is_pass()); // non-finite alarms
    }

    #[test]
    fn parse_a_real_emitted_line() {
        let line = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=1920 cy=1080";
        let s = parse_audit_line(line).expect("should parse");
        assert_eq!(s.monitor, 1);
        assert_eq!(s.divisor, 1);
        assert!((s.rendered_fps - 30.0).abs() < 1e-9);
        assert!((s.target_fps - 30.0).abs() < 1e-9);
        assert!((s.floor_fps - 13.0).abs() < 1e-9);
        assert_eq!(s.cx, 1920);
        assert_eq!(s.cy, 1080);
        // canvas reconstructed from target*divisor, and the printed floor matches mv_floor_fps
        assert!((s.canvas_fps() - 30.0).abs() < 1e-9);
        assert!((mv_floor_fps(s.canvas_fps()) - s.floor_fps).abs() < 1e-9);
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
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=30.0 target=30 floor=28.0 cx=1280 cy=720
multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=13.0 cx=1920 cy=1080
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
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720
";
        assert!(matches!(gate_log(clean), GateOutcome::Clean(v) if v.len() == 2));

        // monitor=1 collapses to 9fps (< floor 13) on its latest sample -> Breach.
        let breach = "\
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=13.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720
multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=13.0 cx=1920 cy=1080
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
}
