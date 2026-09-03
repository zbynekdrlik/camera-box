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

/// #1212: how many of a monitor's most recent `multiview-audit:` samples the gate takes the
/// median over — ~60 s at the ~5 s emit cadence. A multiview render is BURSTY (individual samples
/// dip into the teens inside a window whose median is a healthy 30.0), so gating one latest sample
/// false-alarms; the median of the recent window is the honest steady-state signal. Verified
/// against the mined 4K distribution: N = 12 classifies the healthy windows (medians 28.8–30.0)
/// CLEAN and the live-collapse window (median 17.5) BREACH.
pub const MV_GATE_MEDIAN_WINDOW: usize = 12;

/// The literal log-line marker. Mutually non-substring with every `genlock-*` audit marker, so
/// all parser families can run over one log independently.
pub const MARKER: &str = "multiview-audit:";

/// The MV-fps alarm floor for a projector's TARGET rate: `target_fps − tolerance`, clamped to
/// `>= 0`. `target_fps = canvas_fps / effective_divisor` — the ~30fps-cell rate the projector
/// actually renders at (both broadcast boxes: strih 30fps canvas / divisor 1, imag 60fps canvas /
/// divisor 2, both → target 30 → floor 28). Byte-identical to the C `obs_multiview_floor_fps()` —
/// the emitter prints this (feeding it the same `target_fps` it computed) and the gate reads it
/// back off the line, so they can never diverge.
///
/// #1212: the floor is AREA-INDEPENDENT — it is the same `target − tol` at every render area,
/// including strih's 4K (3840×2160) multiview. The issue-1110 report-only sentinel above 1080p was
/// RETIRED once the full log history showed strih's 4K MV median `rendered_fps` is 29.8–30.0 in
/// every window (max 30.0) — floor 28 IS achievable at 4K. The bursty single-sample noise that
/// motivated the sentinel is handled where it belongs, in the gate (`gate_log` judges the median of
/// the recent window, not one sample), not by un-gating a whole area class.
///
/// #776: the floor tracks the TARGET, not `canvas/2`. The pre-#776 `canvas/2` model assumed every
/// throttleable projector used divisor 2 (MV = canvas/2); once #879 derives the divisor from the
/// canvas rate, a 30fps-canvas box renders MV at divisor 1 = 30fps, so `canvas/2` (= 13) is half
/// the real target and a genuine collapse to ~14–27fps would slip under it unalarmed.
pub fn mv_floor_fps(target_fps: f64) -> f64 {
    let floor = target_fps - MULTIVIEW_AUDIT_FLOOR_TOLERANCE_FPS;
    if floor < 0.0 {
        0.0
    } else {
        floor
    }
}

/// The median `rendered_fps` over the TAIL (most recent `n`) of `samples`. Fewer than `n`
/// available → the median of what exists; empty → `f64::NAN` (which `classify` treats as a breach,
/// so an empty window never silently passes). Even count → the mean of the two middle values.
///
/// #1212: this is the value `gate_log` classifies against the floor, instead of one latest sample —
/// a bursty multiview dips into the teens on individual samples inside a window whose median is a
/// healthy 30.0, so the median is the honest steady-state signal and a lone dip no longer alarms.
pub fn median_recent_rendered_fps(samples: &[MvAuditSample], n: usize) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let start = samples.len().saturating_sub(n);
    let mut recent: Vec<f64> = samples[start..].iter().map(|s| s.rendered_fps).collect();
    recent.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let k = recent.len();
    if k % 2 == 1 {
        recent[k / 2]
    } else {
        (recent[k / 2 - 1] + recent[k / 2]) / 2.0
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
    /// camera-box #1260 lever (1) — per-item MV render instrumentation, ALL optional and
    /// APPEND-only: a pre-#1260 `multiview-audit:` line carries none of these and parses with
    /// every one `None` (proven by `parse_of_a_pre_1260_line_leaves_cell_fields_none`). The
    /// emitter (`render_display()`, obs-display.c) appends `mv_cells= mv_cell_ms= mv_cell_max_ms=
    /// mv_top1= mv_top2=` after `budget_ms=`; the frontend (`Multiview::Render`) times EVERY draw
    /// (scene cells + the preview/program big cells + labels) on the graphics thread and publishes
    /// the aggregate.
    ///
    /// The DECISIVE derived signal is `mv_ewma_ms − cell_ms` (the reader computes it): because
    /// `cell_ms` covers every timed draw, a small `cell_ms` under a large `mv_ewma_ms` = the
    /// UNtimed tail (begin/clear/region-setup + present/GPU-sync, i.e. the GPU/thermal path), and
    /// a `cell_ms` near `mv_ewma_ms` = per-item CPU draw-submission bound. `top1`/`top2` name the
    /// two fattest timed items of the window's worst render (a scene name, or
    /// `preview`/`program`/`labels`) so the reduction lever can target the right one.
    pub cells: Option<u32>,
    /// Window-mean of the per-render sum of EVERY timed draw's CPU time, ms (directly comparable to
    /// `mv_ewma_ms`). Covers scene cells + preview + program + labels, not just scene cells.
    pub cell_ms: Option<f64>,
    /// Window-max of the per-render timed-draw CPU sum, ms (the worst render's draw cost).
    pub cell_max_ms: Option<f64>,
    /// `(name, ms)` of the fattest single timed item in the window's worst render. `name` is the
    /// emitter-sanitized item name (any non-printable-ASCII / `=` / `:` byte → `_`, so the
    /// space-tokenized line stays parseable) — a scene name, or `preview`/`program`/`labels`; an
    /// absent item (`-`) parses to `None`.
    pub top1: Option<(String, f64)>,
    /// `(name, ms)` of the second-fattest timed item in that same worst render.
    pub top2: Option<(String, f64)>,
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
    // camera-box #1260: optional per-cell MV instrumentation tokens (append-only).
    let mut cells: Option<u32> = None;
    let mut cell_ms: Option<f64> = None;
    let mut cell_max_ms: Option<f64> = None;
    let mut top1: Option<(String, f64)> = None;
    let mut top2: Option<(String, f64)> = None;

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
            "mv_cells" => cells = val.parse().ok(),
            "mv_cell_ms" => cell_ms = val.parse().ok(),
            "mv_cell_max_ms" => cell_max_ms = val.parse().ok(),
            "mv_top1" => top1 = parse_top_cell(val),
            "mv_top2" => top2 = parse_top_cell(val),
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
        cells,
        cell_ms,
        cell_max_ms,
        top1,
        top2,
    })
}

/// Parse an `mv_top1=`/`mv_top2=` value `name:ms` into `(name, ms)`. camera-box #1260: the emitter
/// sanitizes the cell name (space/`=`/`:` → `_`) so the value carries exactly ONE `:` separating a
/// `:`-free name from the ms float; split on that LAST `:`. The `-` placeholder (emitted when zero
/// cells rendered) and an unparseable/empty value yield `None`, so a reader never mistakes the
/// placeholder for a real cell.
fn parse_top_cell(val: &str) -> Option<(String, f64)> {
    let (name, ms) = val.rsplit_once(':')?;
    if name.is_empty() || name == "-" {
        return None;
    }
    let ms: f64 = ms.parse().ok()?;
    Some((name.to_string(), ms))
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

/// One monitor's gate verdict: its latest reported sample (carried for display) plus the
/// windowed-median `rendered_fps` the PASS/FAIL decision was actually made on (#1212).
#[derive(Debug, Clone, PartialEq)]
pub struct MvMonitorGate {
    /// The monitor's LATEST `multiview-audit:` sample — the per-line reported values (cx/cy,
    /// target, divisor, the latest rendered_fps + the printed floor), kept for display.
    pub latest: MvAuditSample,
    /// The median `rendered_fps` over the monitor's most recent `MV_GATE_MEDIAN_WINDOW` samples —
    /// the value classified against the floor (`mv_floor_fps` = `target − tol`, a per-line
    /// constant, read from `latest.floor_fps`). A single-latest-sample decision over a bursty 4K
    /// signal false-alarms; the median is the honest steady-state signal.
    pub median_fps: f64,
    /// How many samples the median was actually taken over (≤ `MV_GATE_MEDIAN_WINDOW`).
    pub window_len: usize,
}

/// Outcome of gating a whole OBS log's MV-fps audit lines.
#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    /// No `multiview-audit:` line found at all (the emitter never ran / wrong log).
    NoSamples,
    /// Every projector's windowed-median cadence is at or above its floor (carries ALL monitors).
    Clean(Vec<MvMonitorGate>),
    /// One or more projectors' windowed-median cadence fell below its floor (carries only the
    /// breached monitors).
    Breach(Vec<MvMonitorGate>),
}

/// Gate a whole OBS log: for each projector, take the MEDIAN `rendered_fps` over its most recent
/// `MV_GATE_MEDIAN_WINDOW` samples and alarm if that median is below the projector's own printed
/// floor. This is the decision the `mv-fps-gate` bin (E2E preflight / drift-guard consumer)
/// exposes.
///
/// #1212: the decision moved from the SINGLE latest sample to the windowed MEDIAN. A multiview
/// render is bursty (individual samples dip into the teens inside a median-30 window), so a
/// latest-sample decision false-alarmed; the median tolerates the bursts while still catching a
/// SUSTAINED collapse (and a lone recovered latest sample no longer hides one). The trade-off is a
/// slower alarm on a genuinely fast freeze (~N/2 samples must fall below floor first) — acceptable
/// because that fast-freeze class is the render-liveness watchdog's job (see below), and this gate
/// runs behind a 2-pass confirm at a ~5-min watchdog cadence, never as a sub-second detector. The
/// `latest` sample (its reported values) is still carried in each `MvMonitorGate` for display.
///
/// FRESHNESS ASSUMPTION (#771, review): this gates the recent WINDOW regardless of its age — the
/// caller is expected to pass a CURRENT log (the E2E preflight / drift-guard read the newest OBS
/// log at gate time). It carries no epoch, so a full graphics-thread STALL (OBS frozen, no new
/// audit line emitted at all) leaves the last-good window reading PASS here; that stall class is
/// the render-liveness watchdog's job (`renderTotalFrames` advancement, `#391` /
/// obs-liveness-render-signal.md), not this per-projector cadence floor. The LIVE always-on
/// wiring + any freshness/heartbeat check is tracked in the #771 follow-up ticket.
pub fn gate_log(log_text: &str) -> GateOutcome {
    // Group each monitor's samples in log order (latest last).
    let mut per_monitor: Vec<(u32, Vec<MvAuditSample>)> = Vec::new();
    for line in log_text.lines() {
        if let Some(s) = parse_audit_line(line) {
            if let Some(entry) = per_monitor.iter_mut().find(|(m, _)| *m == s.monitor) {
                entry.1.push(s);
            } else {
                per_monitor.push((s.monitor, vec![s]));
            }
        }
    }
    if per_monitor.is_empty() {
        return GateOutcome::NoSamples;
    }
    per_monitor.sort_by_key(|(m, _)| *m);

    let gates: Vec<MvMonitorGate> = per_monitor
        .into_iter()
        .map(|(_m, samples)| {
            let latest = samples
                .last()
                .expect("a monitor entry is created with >= 1 sample")
                .clone();
            let median_fps = median_recent_rendered_fps(&samples, MV_GATE_MEDIAN_WINDOW);
            let window_len = samples.len().min(MV_GATE_MEDIAN_WINDOW);
            MvMonitorGate {
                latest,
                median_fps,
                window_len,
            }
        })
        .collect();

    let breaches: Vec<MvMonitorGate> = gates
        .iter()
        .filter(|g| !classify(g.median_fps, g.latest.floor_fps).is_pass())
        .cloned()
        .collect();
    if breaches.is_empty() {
        GateOutcome::Clean(gates)
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

        // Both boxes floor at target - tolerance = 28 (the already-proven-healthy imag floor;
        // strih was wrongly 13 while its MV renders 30fps). #1212: the floor is area-INDEPENDENT —
        // the same value at every render area, so 4K gets floor 28 too.
        assert_eq!(mv_floor_fps(strih_target), 28.0);
        assert_eq!(mv_floor_fps(imag_target), 28.0);

        // Degenerate: never a negative floor.
        assert_eq!(mv_floor_fps(2.0), 0.0); // 2 - 2 = 0 exactly
        assert_eq!(mv_floor_fps(1.0), 0.0); // clamped
        assert_eq!(mv_floor_fps(0.0), 0.0);
    }

    #[test]
    fn floor_is_area_independent_1212() {
        // #1212: the alarm floor is the SAME target - tol at every render area — the issue-1110
        // report-only sentinel above 1080p is retired. strih's 4K (3840x2160) multiview holds a
        // healthy median 30fps (mined: 29.8-30.0 in every window), so floor 28 is achievable at 4K
        // and there is no separate area class. The bursty single-sample noise the sentinel papered
        // over is handled in the gate (windowed median), not by un-gating a whole area class.
        assert_eq!(mv_floor_fps(30.0), 28.0); // any target 30 -> 28, regardless of area
                                              // A collapsed rendered_fps against the (now area-independent) 4K floor 28 ALARMS via
                                              // classify — a real 4K collapse is caught again, not masked by a sentinel.
        assert!(!classify(9.0, mv_floor_fps(30.0)).is_pass());
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
        // to the TARGET (post-#776 the floor tracks target, not canvas -- here canvas==target==30
        // because divisor==1, but for a divisor=2 imag line canvas=60/target=30 and only target
        // yields the correct floor). #1212: the floor is area-independent, so cx/cy don't enter it.
        assert!((s.canvas_fps() - 30.0).abs() < 1e-9);
        assert!((mv_floor_fps(s.target_fps) - s.floor_fps).abs() < 1e-9);
    }

    #[test]
    fn parse_tolerates_appended_1260_budget_fields() {
        // camera-box #1260: the emitter appends budget-gate phase-split fields
        // (pre_mv_ms/pre_mv_max_ms/mv_ewma_ms/budget_ms) after cx/cy so which phase eats the
        // per-tick budget is readable from the log. The parser ignores unknown keys (the
        // jitter_audit token-scan convention, `_ => {}`), so an enriched line still parses to the
        // SAME seven core fields the gate reads — this PROVES the append is backward-compatible
        // rather than asserting it in prose.
        let line = "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=7.5 target=30 floor=28.0 cx=3840 cy=2160 pre_mv_ms=24.70 pre_mv_max_ms=31.20 mv_ewma_ms=12.40 budget_ms=30.00";
        let s = parse_audit_line(line).expect("an enriched line must still parse the core fields");
        assert_eq!(s.monitor, 1);
        assert_eq!(s.divisor, 1);
        assert!((s.rendered_fps - 7.5).abs() < 1e-9);
        assert!((s.target_fps - 30.0).abs() < 1e-9);
        assert!((s.floor_fps - 28.0).abs() < 1e-9);
        assert_eq!(s.cx, 3840);
        assert_eq!(s.cy, 2160);
        // The appended fields never affect the gate: this enriched line still classifies BELOW floor.
        assert!(!classify(s.rendered_fps, s.floor_fps).is_pass());
    }

    #[test]
    fn parse_populates_the_1260_per_cell_cell_tokens() {
        // camera-box #1260 lever (1): the emitter appends per-cell MV instrumentation
        // (mv_cells / mv_cell_ms / mv_cell_max_ms / mv_top1=name:ms / mv_top2=name:ms) after the
        // #1260 budget tokens. This RED→GREEN proves the parser READS them (unknown-key tolerance
        // means an old line ignores them; this asserts a NEW line actually populates them). The
        // names are emitter-sanitized (spaces/`=`/`:` → `_`) so the space-tokenized line stays
        // parseable — "CG bridge" arrives as "CG_bridge".
        let line =
            "20:15:03.123: multiview-audit: monitor=1 divisor=1 rendered_fps=14.0 target=30 \
                    floor=28.0 cx=3840 cy=2160 pre_mv_ms=8.00 pre_mv_max_ms=9.00 mv_ewma_ms=21.50 \
                    budget_ms=30.00 mv_cells=15 mv_cell_ms=18.40 mv_cell_max_ms=24.10 \
                    mv_top1=Ableset:6.20 mv_top2=CG_bridge:4.10";
        let s = parse_audit_line(line).expect("enriched line must still parse the core fields");
        // core fields unaffected
        assert_eq!(s.monitor, 1);
        assert_eq!(s.cx, 3840);
        // per-cell fields populated
        assert_eq!(s.cells, Some(15));
        assert!((s.cell_ms.expect("mv_cell_ms") - 18.40).abs() < 1e-9);
        assert!((s.cell_max_ms.expect("mv_cell_max_ms") - 24.10).abs() < 1e-9);
        let (n1, m1) = s.top1.as_ref().expect("mv_top1 must parse");
        assert_eq!(n1, "Ableset");
        assert!((m1 - 6.20).abs() < 1e-9);
        let (n2, m2) = s.top2.as_ref().expect("mv_top2 must parse");
        assert_eq!(n2, "CG_bridge");
        assert!((m2 - 4.10).abs() < 1e-9);
        // The DECISIVE derived signal the ticket needs is `mv_ewma_ms − cell_ms`: here
        // 21.50 − 18.40 = 3.10 ms → mostly per-cell CPU-bound, not a GPU-wait/present tail.
    }

    #[test]
    fn parse_of_a_pre_1260_line_leaves_the_cell_fields_none() {
        // A pre-#1260 line (no mv_cells/... tokens) parses with every per-cell field None — the
        // append is backward-compatible (no consumer that ignores the fields is affected).
        let line = "multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080";
        let s = parse_audit_line(line).expect("a pre-#1260 line must still parse");
        assert_eq!(s.cells, None);
        assert_eq!(s.cell_ms, None);
        assert_eq!(s.cell_max_ms, None);
        assert!(s.top1.is_none());
        assert!(s.top2.is_none());
    }

    #[test]
    fn parse_of_mv_top_dash_placeholder_is_none() {
        // The emitter prints `mv_top1=-:0.00` when zero cells rendered (a degenerate frame). The
        // parser treats the "-" placeholder as an ABSENT cell (None), never a cell literally
        // named "-", so a reader never mistakes the placeholder for a real fat cell.
        let line = "multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 \
                    cx=1920 cy=1080 mv_cells=0 mv_cell_ms=0.00 mv_cell_max_ms=0.00 mv_top1=-:0.00 mv_top2=-:0.00";
        let s = parse_audit_line(line).expect("must parse");
        assert_eq!(s.cells, Some(0));
        assert!(s.top1.is_none());
        assert!(s.top2.is_none());
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

        // monitor=1's window median is 19.5 ((30 + 9)/2) < floor 28 -> Breach; the breach carries
        // the LATEST sample (rendered_fps 9.0) for display.
        let breach = "\
multiview-audit: monitor=1 divisor=1 rendered_fps=30.0 target=30 floor=28.0 cx=1920 cy=1080
multiview-audit: monitor=2 divisor=2 rendered_fps=29.0 target=30 floor=28.0 cx=1280 cy=720
multiview-audit: monitor=1 divisor=1 rendered_fps=9.0 target=30 floor=28.0 cx=1920 cy=1080
";
        match gate_log(breach) {
            GateOutcome::Breach(b) => {
                assert_eq!(b.len(), 1);
                assert_eq!(b[0].latest.monitor, 1);
                assert!((b[0].latest.rendered_fps - 9.0).abs() < 1e-9);
                assert!((b[0].median_fps - 19.5).abs() < 1e-9);
                assert_eq!(b[0].window_len, 2);
            }
            other => panic!("expected Breach, got {other:?}"),
        }
    }

    // #1212 helpers: build a monitor's worth of `multiview-audit:` lines from a list of
    // rendered_fps values (all 4K, floor 28, in log order — latest last).
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

    #[test]
    fn gate_median_matches_the_mined_4k_distribution_1212() {
        // The mined strih 4K windows (supervisor, 6 newest OBS logs). N=12 must classify the
        // healthy windows CLEAN and the live-collapse window BREACH — floor 28 at 4K.

        // 17:43 window: medLast12 = 30.0 -> CLEAN. Twelve 30.0 with two teens dips that don't move
        // the median (the bursty-but-healthy reality).
        let mut healthy_3000 = vec![30.0; 10];
        healthy_3000.push(19.8);
        healthy_3000.push(14.9);
        assert!((median_recent_rendered_fps(&mk_samples(&healthy_3000), 12) - 30.0).abs() < 1e-9);
        assert!(matches!(
            gate_log(&log_4k_monitor1(&healthy_3000)),
            GateOutcome::Clean(_)
        ));

        // 10:38 window: medLast12 = 28.8 -> CLEAN (the TIGHTEST clean median, only 0.8 over floor).
        // six 28.6 + six 29.0 -> median = (28.6 + 29.0)/2 = 28.8.
        let mut tight_2880 = vec![28.6; 6];
        tight_2880.extend(vec![29.0; 6]);
        assert!((median_recent_rendered_fps(&mk_samples(&tight_2880), 12) - 28.8).abs() < 1e-9);
        assert!(matches!(
            gate_log(&log_4k_monitor1(&tight_2880)),
            GateOutcome::Clean(_)
        ));

        // 23:32 window (LIVE collapse): medLast12 = 17.5 -> BREACH.
        // six 17.0 + six 18.0 -> median = (17.0 + 18.0)/2 = 17.5.
        let mut collapse_1750 = vec![17.0; 6];
        collapse_1750.extend(vec![18.0; 6]);
        assert!((median_recent_rendered_fps(&mk_samples(&collapse_1750), 12) - 17.5).abs() < 1e-9);
        assert!(matches!(
            gate_log(&log_4k_monitor1(&collapse_1750)),
            GateOutcome::Breach(_)
        ));

        // Fewer than N available -> median of what exists (never silently passes / never requires
        // a full window). A 4-sample collapse still breaches.
        let short = vec![10.0, 11.0, 12.0, 13.0];
        assert_eq!(median_recent_rendered_fps(&mk_samples(&short), 12), 11.5);
        assert!(matches!(
            gate_log(&log_4k_monitor1(&short)),
            GateOutcome::Breach(_)
        ));
    }

    // #1212 helper: MvAuditSample list from rendered_fps values (4K, floor 28) for the pure median.
    fn mk_samples(rendered: &[f64]) -> Vec<MvAuditSample> {
        rendered
            .iter()
            .map(|&r| MvAuditSample {
                monitor: 1,
                divisor: 1,
                rendered_fps: r,
                target_fps: 30.0,
                floor_fps: 28.0,
                cx: 3840,
                cy: 2160,
                cells: None,
                cell_ms: None,
                cell_max_ms: None,
                top1: None,
                top2: None,
            })
            .collect()
    }
}
