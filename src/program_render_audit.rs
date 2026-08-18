//! #1029 — PROGRAM-output render observability: parse the vendored libobs
//! `program-render-audit:` OBS-log line.
//!
//! The `multiview-audit:` line (`mv_audit.rs`, #771) covers ONLY the throttleable MONITORING
//! surfaces (`render_display()`, `divisor>1`). The PROGRAM output — the mix that feeds the imag
//! HDMI fullscreen projector where the measurement burn square lives — renders EVERY graphics
//! tick (`divisor<=1`, untouched by the #278 budget gate) and had NO durable render-cadence
//! signal in the OBS log at all: `renderSkipped` lived only in the transient WS GetStats, and the
//! WS `activeFps` gauge LIES during a render stall (it returns the configured canvas fps even when
//! the render loop is fully frozen, #935 / obs-liveness-render-signal.md).
//!
//! #1029 adds, every ~5s from `obs_graphics_thread_loop()` (obs-video.c), a line carrying the
//! HONEST program-render cadence taken from the real frame counters:
//!
//! ```text
//! program-render-audit: render_fps=59.9 target_fps=60.0 avg_frame_ms=4.80 lagged=0 total=300
//! ```
//!
//! - `render_fps` = `total`(delta) / window_seconds — the ACTUAL rendered-frame rate over the
//!   window, computed from `obs->video.total_frames` (NOT the lying `activeFps`);
//! - `target_fps` = canvas fps (`1e9 / video_frame_interval_ns`);
//! - `avg_frame_ms` = the latest ~1s `video_avg_frame_time_ns` (mean graphics-tick render cost);
//! - `lagged` = `obs->video.lagged_frames` delta over the window = the renderSkipped count
//!   (`count-1` per late `video_sleep` wake) — a burst here is exactly a burn-square forward JUMP
//!   of the same magnitude;
//! - `total` = `obs->video.total_frames` delta over the window.
//!
//! This is the pure Tier-0 authority (default features, no probe/OBS/rig) for parsing that line
//! and for the `is_render_path_jump` discriminator acceptance-criterion-1 asks for: given a
//! window with a burn-square jump, `lagged>0` attributes it to the RENDER path (renderSkipped),
//! while a clean render (`lagged==0`) with a clean `genlock-fifo audit` window points the origin
//! at the display/scanout path instead. It is strictly REPORT-ONLY: there is NO floor / gate here
//! (the gate for this class is issue 798) — the receive-side NDI cadence is the SEPARATE
//! already-covered `genlock-fifo audit` layer (`jitter_audit.rs`).

/// The literal log-line marker. Mutually non-substring with `multiview-audit:` and every
/// `genlock-*` audit marker, so all parser families can run over one log independently.
pub const MARKER: &str = "program-render-audit:";

/// Measured render cadence = rendered frames / window seconds. `window_ns == 0` → 0.0.
///
/// SPEC ANCHOR (#1029): the Rust mirror of the EXACT `render_fps` computation the C emit does
/// inline in `obs_graphics_thread_loop()` (obs-video.c):
/// `(double)total_delta / ((double)audit_elapsed / 1e9)`. Like `mv_audit::measured_fps`, it exists
/// so the emit's fps math has a unit-tested Rust spec, not because the gate calls it (the parser
/// reads the already-emitted `render_fps=` off the line).
pub fn measured_fps(total_delta: u32, window_ns: u64) -> f64 {
    if window_ns == 0 {
        return 0.0;
    }
    total_delta as f64 / (window_ns as f64 / 1_000_000_000.0)
}

/// One parsed `program-render-audit:` line.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramRenderSample {
    /// Actual rendered-frame rate over the window (honest — from the real total_frames delta).
    pub render_fps: f64,
    /// Canvas fps the loop targets (`1e9 / video_frame_interval_ns`).
    pub target_fps: f64,
    /// Mean graphics-tick render cost over the latest ~1s window, in ms.
    pub avg_frame_ms: f64,
    /// renderSkipped count over the window (`lagged_frames` delta) — the forward-jump magnitude.
    pub lagged: u32,
    /// Rendered-frame count over the window (`total_frames` delta).
    pub total: u32,
}

/// Did the render path cause a forward JUMP in this window? `lagged > 0` = at least one
/// renderSkipped, i.e. the program projector held a frame and the burn id leapt — the RENDER-path
/// origin (#1029 acceptance criterion 1). `lagged == 0` means the render loop kept cadence, so a
/// jump observed in that same window originates DOWNSTREAM (display/scanout) or in the FIFO
/// (read the paired `genlock-fifo audit` window). Report-only discriminator, never a gate.
pub fn is_render_path_jump(lagged: u32) -> bool {
    lagged > 0
}

/// Parse ONE line. Returns `Some` only for a genuine `program-render-audit:` line carrying every
/// required numeric field; every other line (the `multiview-audit:` / `genlock-*` lines and plain
/// noise) returns `None`. Unrecognized `key=value` tokens are ignored (the `jitter_audit`
/// token-scan convention), so the emitter can add fields later without breaking this parser.
///
/// The float fields use Rust's `.`-decimal `f64::parse`: the C emitter always writes `%.1f`/`%.2f`
/// under OBS's C locale, so the decimal separator is always `.`. A field that ever failed to parse
/// degrades safely (the whole line → `None`, never a wrong number).
pub fn parse_audit_line(line: &str) -> Option<ProgramRenderSample> {
    let mark_at = line.find(MARKER)?;
    let rest = &line[mark_at + MARKER.len()..];

    let mut render_fps: Option<f64> = None;
    let mut target_fps: Option<f64> = None;
    let mut avg_frame_ms: Option<f64> = None;
    let mut lagged: Option<u32> = None;
    let mut total: Option<u32> = None;

    for tok in rest.split_whitespace() {
        let Some((key, val)) = tok.split_once('=') else {
            continue;
        };
        match key {
            "render_fps" => render_fps = val.parse().ok(),
            "target_fps" => target_fps = val.parse().ok(),
            "avg_frame_ms" => avg_frame_ms = val.parse().ok(),
            "lagged" => lagged = val.parse().ok(),
            "total" => total = val.parse().ok(),
            _ => {}
        }
    }

    Some(ProgramRenderSample {
        render_fps: render_fps?,
        target_fps: target_fps?,
        avg_frame_ms: avg_frame_ms?,
        lagged: lagged?,
        total: total?,
    })
}

/// The LATEST `program-render-audit` sample over the log (the current program-render state the
/// loop last reported), or `None` if the emitter never ran / wrong log.
pub fn latest(log_text: &str) -> Option<ProgramRenderSample> {
    let mut last: Option<ProgramRenderSample> = None;
    for line in log_text.lines() {
        if let Some(s) = parse_audit_line(line) {
            last = Some(s);
        }
    }
    last
}

/// Total renderSkipped (`lagged`) summed across every `program-render-audit` window in the log —
/// the render-path forward-jump burden over the whole session. Report-only.
pub fn total_lagged(log_text: &str) -> u64 {
    log_text
        .lines()
        .filter_map(parse_audit_line)
        .map(|s| s.lagged as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_fps_is_frames_over_window_seconds() {
        assert!((measured_fps(300, 5_000_000_000) - 60.0).abs() < 1e-9);
        assert!((measured_fps(150, 5_000_000_000) - 30.0).abs() < 1e-9);
        assert_eq!(measured_fps(300, 0), 0.0); // no window
        assert_eq!(measured_fps(0, 5_000_000_000), 0.0); // frozen render -> 0 fps
    }

    #[test]
    fn is_render_path_jump_iff_lagged_positive() {
        assert!(!is_render_path_jump(0)); // render kept cadence -> jump (if any) is downstream/FIFO
        assert!(is_render_path_jump(1)); // one renderSkipped -> a one-frame forward jump
        assert!(is_render_path_jump(9)); // a burst -> a nine-frame leap
    }

    #[test]
    fn parse_a_real_emitted_line() {
        let line = "17:08:43.887: program-render-audit: render_fps=59.9 target_fps=60.0 avg_frame_ms=4.80 lagged=0 total=300";
        let s = parse_audit_line(line).expect("should parse");
        assert!((s.render_fps - 59.9).abs() < 1e-9);
        assert!((s.target_fps - 60.0).abs() < 1e-9);
        assert!((s.avg_frame_ms - 4.80).abs() < 1e-9);
        assert_eq!(s.lagged, 0);
        assert_eq!(s.total, 300);
        assert!(!is_render_path_jump(s.lagged));
    }

    #[test]
    fn parse_a_jump_window() {
        // A throttle burst: render fell to ~48fps, 12 frames skipped -> a 12-frame burn-square leap.
        let line = "program-render-audit: render_fps=48.0 target_fps=60.0 avg_frame_ms=18.30 lagged=12 total=240";
        let s = parse_audit_line(line).expect("should parse");
        assert_eq!(s.lagged, 12);
        assert!(is_render_path_jump(s.lagged));
        assert!(s.avg_frame_ms > 16.67); // over the 60fps budget -> the throttle signature
    }

    #[test]
    fn parser_rejects_multiview_and_genlock_lines_and_noise_1029() {
        // Mutually non-substring markers: our parser rejects the sibling audit lines...
        assert!(parse_audit_line(
            "multiview-audit: monitor=1 divisor=2 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080"
        )
        .is_none());
        assert!(parse_audit_line(
            "genlock-fifo audit 'NDI CAM1': received=300 consumed=300 underruns=0 lagged=0 ts_present=123"
        )
        .is_none());
        assert!(parse_audit_line("some unrelated obs log line").is_none());
        // A program-render-audit line missing a required field is not a usable sample.
        assert!(
            parse_audit_line("program-render-audit: render_fps=60.0 target_fps=60.0").is_none()
        );
        // ...and our marker never appears inside the sibling lines (the non-substring guarantee).
        assert!(!"multiview-audit: monitor=1".contains(MARKER));
        assert!(!"genlock-fifo audit 'x':".contains(MARKER));
    }

    #[test]
    fn latest_keeps_the_last_program_render_sample() {
        let log = "\
program-render-audit: render_fps=60.0 target_fps=60.0 avg_frame_ms=4.70 lagged=0 total=300
multiview-audit: monitor=2 divisor=2 rendered_fps=30.0 target=30 floor=28.0 cx=1280 cy=720
program-render-audit: render_fps=51.0 target_fps=60.0 avg_frame_ms=15.10 lagged=9 total=255
";
        let s = latest(log).expect("has a program-render sample");
        assert_eq!(s.lagged, 9); // the LAST program-render-audit line, not the multiview one
        assert_eq!(s.total, 255);
        assert!(is_render_path_jump(s.lagged));
    }

    #[test]
    fn total_lagged_sums_the_render_path_burden() {
        let log = "\
program-render-audit: render_fps=60.0 target_fps=60.0 avg_frame_ms=4.70 lagged=0 total=300
program-render-audit: render_fps=52.0 target_fps=60.0 avg_frame_ms=14.0 lagged=8 total=260
program-render-audit: render_fps=57.0 target_fps=60.0 avg_frame_ms=9.0 lagged=3 total=285
";
        assert_eq!(total_lagged(log), 11);
        assert_eq!(total_lagged("nothing here\nanother line"), 0);
    }
}
