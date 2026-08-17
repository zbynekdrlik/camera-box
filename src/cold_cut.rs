//! #768 — the REPORT-ONLY COLD-CUT onset seam: measure the first ~1s after each program switch
//! to a cambox that had been HIDDEN from strih program for `>= COLD_HIDDEN_SECS`, and report
//! whether that cut stayed instant + lossless (no wake-up gap).
//!
//! ## The blind spot this closes
//!
//! Issue 767 (a genlocked NDI receiver that never rebinds after its sender restarts -> 41-min
//! silent black while off-program) survived every CI + E2E gate for one reason: **nothing measured
//! the TRANSITION ONSET** (the first ~1s right after a program cut). Three independent places hide
//! it in the existing verdict:
//!   1. `probe::recording_segments` applies a 1s `DEFAULT_TRANSITION_GUARD_NS` that DISCARDS the
//!      first second after every switch — so the onset never reaches the segmenter's metrics.
//!   2. The per-segment aggregate `copies`/`gaps` average a single wake-up gap over the whole ~30s
//!      window, diluting it below any per-window tolerance.
//!   3. `residual_events` (the only per-event data in an existing verdict) START after the guard —
//!      so across 364 historical `>= 60s`-hidden cold cuts every onset reads `copies=gaps=0`, but
//!      that 0 is the guard hiding them, not proof they were clean.
//!
//! The all-cambox sweep DOES produce genuinely `>= 60s`-hidden cuts (the active 3-box
//! CAM1/CAM2/CAM3 cycle at 30s segments hides each camera 2x30s between windows; measured median
//! `hidden_secs_before` = 60.4s across 76 local verdicts), so "manufacture a cold cut" is NOT what
//! was missing — the MEASUREMENT was.
//!
//! ## What this measures
//!
//! For each schedule window whose cambox was program-hidden `>= COLD_HIDDEN_SECS` before its switch
//! (derived purely from the ordered window sequence — the gap between this window's `start_ns` and
//! the same cambox's PREVIOUS window `end_ns`), the seam reads the raw decoded frames whose
//! `gen_ts_ns` falls in the first `ONSET_WINDOW_NS` after the switch (reading `seg_frames`
//! DIRECTLY bypasses the segmenter's guard) and reports:
//!
//! - `onset_frames` — frames delivered in the onset window.
//! - `onset_decodable` — of those, how many carried a decoded painted tick.
//! - `onset_undecodable` — the rest (black/frozen at onset).
//! - `wakeup_latency_ns` — offset of the FIRST decodable frame from the switch (`None` = the camera delivered no decodable frame within the onset window).
//!
//! A cold cut is `clean` when the first decodable frame arrives within `WAKEUP_LATENCY_MAX_NS` and
//! no onset frame was undecodable.
//!
//! ## Why report-only (calibration-first) — issue 768
//!
//! The onset frames were guarded out of every existing artifact, so there is NO local calibration
//! data for a wake-up-latency / onset-undecodable bound yet (mirrors
//! `verdict-gate-seam-calibration.md` step 1, except the field it wants to mine does not exist
//! until THIS seam starts emitting it). [`gates_overall_pass`] is therefore hardcoded `false`: the
//! seam SERIALIZES the onset measurement so the next real E2E runs establish the warm baseline, and
//! a follow-up flips it LIVE once (a) that baseline exists and (b) a deliberate keepalive-bypass
//! cold cut (issue 768 test-design bod 1) makes at least one cut GENUINELY cold — until then every
//! measured cut is warm (the CAM KEEPALIVE scene keeps its NDI receiver pulling off-program), so a
//! warm-only baseline cannot yet set a bound a real cold gap would breach. Same crate-root
//! pure-seam pattern as `optical_floor.rs` / `presentation_cadence.rs` (default features, Tier-0
//! unit-testable); the probe-gated `recording-verdict.rs` is only a thin consumer.

/// A cambox program-hidden for at least this many seconds before a cut counts as a COLD cut
/// (issue 768's `>= 60s` bar).
pub const COLD_HIDDEN_SECS: f64 = 60.0;

/// The onset window measured after each switch — the first second (issue 768 bod 3's `+-1s around
/// the cut`). This is also exactly the span `probe::recording_segments::DEFAULT_TRANSITION_GUARD_NS`
/// discards, i.e. the onset is precisely the guarded material nothing else can see.
pub const ONSET_WINDOW_NS: i64 = 1_000_000_000;

/// Provisional wake-up-latency ceiling: 2 frames at 30fps (issue 768's "first decoded frame within
/// X ms, e.g. 2 frames @30fps"). REPORT-ONLY — this is not yet a calibrated LIVE bound (see the
/// module doc); it only classifies a transition's reported `clean` flag.
pub const WAKEUP_LATENCY_MAX_NS: i64 = 66_666_667;

/// A decoded frame that landed inside a transition's onset window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnsetFrame {
    /// The frame's `gen_ts_ns` anchor, on the same burn/schedule timeline as the switch times.
    pub gen_ts_ns: i64,
    /// Whether the cam2 optical painted tick decoded on this delivered frame (`false` = black /
    /// frozen / undecodable at onset).
    pub decodable: bool,
}

/// One schedule window plus the decoded frames whose `gen_ts_ns` fell in its onset span. The
/// consumer builds these from `seg_frames` + the switch schedule; the unit tests build them
/// directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdCutWindow {
    /// Cambox label in program for this window (e.g. `"CAM1"`).
    pub cambox: String,
    /// Switch time (start of the window) on the burn `gen_ts_ns` timeline.
    pub start_ns: i64,
    /// End of the window on the burn `gen_ts_ns` timeline.
    pub end_ns: i64,
    /// The decoded frames in `[start_ns, start_ns + ONSET_WINDOW_NS)` (see [`in_onset_window`]).
    pub onset_frames: Vec<OnsetFrame>,
}

/// The onset report for ONE cold-cut transition.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ColdCutTransition {
    pub cambox: String,
    /// How long this cambox was program-hidden before this switch.
    pub hidden_secs_before: f64,
    /// Frames delivered in the onset window.
    pub onset_frames: u32,
    /// Of those, how many carried a decoded painted tick.
    pub onset_decodable: u32,
    /// The rest — black / frozen / undecodable at onset.
    pub onset_undecodable: u32,
    /// Offset of the FIRST decodable frame from the switch, or `None` if none decoded in the onset
    /// window (the worst wake-up signal).
    pub wakeup_latency_ns: Option<i64>,
    /// First decodable frame within `WAKEUP_LATENCY_MAX_NS` AND no onset frame undecodable.
    pub clean: bool,
}

/// The whole run's cold-cut onset report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ColdCutReport {
    pub cold_hidden_secs: f64,
    pub onset_window_ns: i64,
    pub wakeup_latency_max_ns: i64,
    /// Number of `>= COLD_HIDDEN_SECS`-hidden transitions found in the schedule.
    pub cold_transitions_found: usize,
    /// The largest wake-up latency across all cold transitions (`None` = none decoded / none found).
    pub worst_wakeup_latency_ns: Option<i64>,
    /// Any cold transition whose first decodable frame arrived after `WAKEUP_LATENCY_MAX_NS`.
    pub any_wakeup_over_max: bool,
    /// Any cold transition that delivered NO decodable frame in its onset window.
    pub any_wakeup_missing: bool,
    /// Any cold transition with a black/frozen (undecodable) frame at onset.
    pub any_onset_undecodable: bool,
    pub transitions: Vec<ColdCutTransition>,
}

/// Is a decoded frame's `gen_ts_ns` inside the onset window of a switch at `window_start_ns`? The
/// half-open `[start, start + ONSET_WINDOW_NS)` — a frame exactly at the switch counts, one exactly
/// one onset-window later does not.
pub fn in_onset_window(gen_ts_ns: i64, window_start_ns: i64) -> bool {
    gen_ts_ns >= window_start_ns && gen_ts_ns < window_start_ns.saturating_add(ONSET_WINDOW_NS)
}

/// Per-transition onset stats from the onset-window frames. Returns
/// `(delivered, decodable, undecodable, wakeup_latency_ns)` — `wakeup_latency_ns` is the smallest
/// non-negative offset of a decodable frame from `window_start_ns`, or `None` if none decoded.
pub fn onset_stats(
    window_start_ns: i64,
    onset_frames: &[OnsetFrame],
) -> (u32, u32, u32, Option<i64>) {
    let delivered = onset_frames.len() as u32;
    let decodable = onset_frames.iter().filter(|f| f.decodable).count() as u32;
    let undecodable = delivered - decodable;
    let wakeup = onset_frames
        .iter()
        .filter(|f| f.decodable)
        .map(|f| f.gen_ts_ns - window_start_ns)
        .filter(|d| *d >= 0)
        .min();
    (delivered, decodable, undecodable, wakeup)
}

/// A transition is clean when the first decodable frame arrived within `WAKEUP_LATENCY_MAX_NS` and
/// no onset frame was undecodable. A missing wake-up (`None`) is never clean.
pub fn transition_is_clean(wakeup_latency_ns: Option<i64>, onset_undecodable: u32) -> bool {
    match wakeup_latency_ns {
        Some(w) => w <= WAKEUP_LATENCY_MAX_NS && onset_undecodable == 0,
        None => false,
    }
}

/// Build the cold-cut onset report from the ordered schedule windows (+ their onset frames).
///
/// `hidden_secs_before` for a cambox's window is the gap from the same cambox's PREVIOUS window
/// end; for a cambox's FIRST appearance it is anchored to the run's first switch
/// (`windows[0].start_ns`), which UNDER-estimates the true hidden time (the camera was also hidden
/// during the pre-sweep preroll) — deliberately conservative, so a first appearance is never
/// falsely flagged cold.
pub fn build_report(windows: &[ColdCutWindow]) -> ColdCutReport {
    // #768 RED stub — the real derivation lands in the GREEN commit. Returns an empty report so the
    // cold-transition tests (cold flagging + onset gap detection) fail until then.
    let _ = windows;
    ColdCutReport {
        cold_hidden_secs: COLD_HIDDEN_SECS,
        onset_window_ns: ONSET_WINDOW_NS,
        wakeup_latency_max_ns: WAKEUP_LATENCY_MAX_NS,
        cold_transitions_found: 0,
        worst_wakeup_latency_ns: None,
        any_wakeup_over_max: false,
        any_wakeup_missing: false,
        any_onset_undecodable: false,
        transitions: Vec::new(),
    }
}

/// Report-only gate verdict: no cold transition showed a wake-up gap (late/missing wake-up or a
/// black onset frame). Whether this actually folds into `overall_pass` is [`gates_overall_pass`].
pub fn cold_cut_gate_pass(report: &ColdCutReport) -> bool {
    !report.any_wakeup_over_max && !report.any_wakeup_missing && !report.any_onset_undecodable
}

/// #768 REPORT-ONLY (calibration-first) — hardcoded `false`. See the module doc: the onset was
/// never serialized before, so no bound is calibratable yet; a follow-up flips this to `true` once
/// a warm baseline exists AND a deliberate keepalive-bypass cold cut makes a real cold gap
/// possible. A one-line flip, exactly the shape this seam exists to make possible.
pub fn gates_overall_pass() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEG_NS: i64 = 30_000_000_000; // 30s segment
    const FRAME_NS: i64 = 33_333_333; // ~30fps
    const BASE: i64 = 1_000_000_000_000;

    fn frame(gen_ts_ns: i64, decodable: bool) -> OnsetFrame {
        OnsetFrame {
            gen_ts_ns,
            decodable,
        }
    }

    // A window whose onset is a healthy warm cut: decodable frames every ~33ms from the switch.
    fn warm_onset(start_ns: i64) -> Vec<OnsetFrame> {
        (0..30)
            .map(|i| frame(start_ns + i * FRAME_NS, true))
            .collect()
    }

    fn window(cambox: &str, start_ns: i64, onset: Vec<OnsetFrame>) -> ColdCutWindow {
        ColdCutWindow {
            cambox: cambox.to_string(),
            start_ns,
            end_ns: start_ns + SEG_NS,
            onset_frames: onset,
        }
    }

    // --- in_onset_window -------------------------------------------------------------------

    #[test]
    fn in_onset_window_is_half_open() {
        let s = BASE;
        assert!(
            in_onset_window(s, s),
            "the switch instant itself is in-onset"
        );
        assert!(
            in_onset_window(s + ONSET_WINDOW_NS - 1, s),
            "just before the end is in-onset"
        );
        assert!(
            !in_onset_window(s + ONSET_WINDOW_NS, s),
            "exactly one window later is NOT in-onset"
        );
        assert!(
            !in_onset_window(s - 1, s),
            "before the switch is NOT in-onset"
        );
    }

    // --- onset_stats -----------------------------------------------------------------------

    #[test]
    fn onset_stats_counts_and_first_decodable_wakeup() {
        let s = BASE;
        // first two frames undecodable, third decodable at +2 frames.
        let frames = vec![
            frame(s, false),
            frame(s + FRAME_NS, false),
            frame(s + 2 * FRAME_NS, true),
            frame(s + 3 * FRAME_NS, true),
        ];
        let (delivered, decodable, undecodable, wakeup) = onset_stats(s, &frames);
        assert_eq!(delivered, 4);
        assert_eq!(decodable, 2);
        assert_eq!(undecodable, 2);
        assert_eq!(
            wakeup,
            Some(2 * FRAME_NS),
            "wakeup = first DECODABLE frame's offset"
        );
    }

    #[test]
    fn onset_stats_all_undecodable_has_no_wakeup() {
        let s = BASE;
        let frames = vec![frame(s, false), frame(s + FRAME_NS, false)];
        let (delivered, decodable, undecodable, wakeup) = onset_stats(s, &frames);
        assert_eq!((delivered, decodable, undecodable), (2, 0, 2));
        assert_eq!(
            wakeup, None,
            "no decodable frame -> no wake-up latency (worst signal)"
        );
    }

    #[test]
    fn onset_stats_empty_window_delivers_nothing() {
        let (delivered, decodable, undecodable, wakeup) = onset_stats(BASE, &[]);
        assert_eq!((delivered, decodable, undecodable), (0, 0, 0));
        assert_eq!(wakeup, None);
    }

    // --- transition_is_clean ---------------------------------------------------------------

    #[test]
    fn transition_clean_only_when_prompt_and_fully_decodable() {
        assert!(
            transition_is_clean(Some(0), 0),
            "immediate decode, no undecodable -> clean"
        );
        assert!(
            transition_is_clean(Some(WAKEUP_LATENCY_MAX_NS), 0),
            "exactly at the ceiling -> clean"
        );
        assert!(
            !transition_is_clean(Some(WAKEUP_LATENCY_MAX_NS + 1), 0),
            "one ns over the ceiling -> not clean"
        );
        assert!(
            !transition_is_clean(Some(0), 1),
            "an undecodable onset frame -> not clean"
        );
        assert!(
            !transition_is_clean(None, 0),
            "no wake-up at all -> not clean"
        );
    }

    // --- build_report: hidden-duration derivation ------------------------------------------

    #[test]
    fn hidden_duration_derived_from_sequence_and_cold_flagged() {
        // 3-box CAM1/CAM2/CAM3 cycle at 30s -> a 2nd appearance is hidden 60s (COLD).
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, warm_onset(BASE + 3 * SEG_NS)),
            window("CAM2", BASE + 4 * SEG_NS, warm_onset(BASE + 4 * SEG_NS)),
        ];
        let r = build_report(&windows);
        // CAM1@seg3 (hidden 60s) and CAM2@seg4 (hidden 60s) are the two cold transitions.
        assert_eq!(
            r.cold_transitions_found, 2,
            "the two 60s-hidden 2nd appearances are cold"
        );
        for t in &r.transitions {
            assert!(t.hidden_secs_before >= COLD_HIDDEN_SECS);
            assert!(
                (t.hidden_secs_before - 60.0).abs() < 0.001,
                "60s hidden between windows"
            );
        }
    }

    #[test]
    fn first_appearance_is_anchored_to_first_switch_and_never_cold() {
        // CAM1's first appearance (window 0) is anchored to the first switch -> 0s hidden.
        // CAM3's first appearance at seg2 is 60s after the first switch, but a FIRST appearance is
        // anchored conservatively to windows[0].start_ns, so it is 60s -> COLD by the anchor rule,
        // which is acceptable (it WAS hidden >= 60s since the first switch). The point of the anchor
        // is only to never OVER-flag; here 60s is a real lower bound. window 0 must be 0s.
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
        ];
        let r = build_report(&windows);
        // window 0 (CAM1 first) is 0s hidden -> never cold; CAM2 (30s) not cold; CAM3 (60s) cold.
        assert!(
            !r.transitions.iter().any(|t| t.cambox == "CAM1"),
            "CAM1's only (first) appearance is 0s hidden, never cold"
        );
        assert!(
            !r.transitions.iter().any(|t| t.cambox == "CAM2"),
            "CAM2 first appearance is only 30s after the first switch, not cold"
        );
    }

    #[test]
    fn sub_threshold_hidden_is_not_counted_cold() {
        // 2-box cycle at 30s -> each 2nd appearance hidden only 30s -> never cold.
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM1", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM2", BASE + 3 * SEG_NS, warm_onset(BASE + 3 * SEG_NS)),
        ];
        let r = build_report(&windows);
        assert_eq!(
            r.cold_transitions_found, 0,
            "30s-hidden transitions are below the cold bar"
        );
    }

    // --- build_report: onset health (THE cold-cut gap detection) ---------------------------

    #[test]
    fn cold_cut_with_wakeup_gap_is_flagged() {
        // CAM1@seg3 is a 60s-hidden cold cut whose onset is a WAKE-UP GAP: all onset frames
        // undecodable (black/frozen), i.e. the exact issue-767 signature. It must be flagged.
        let bad = vec![
            frame(BASE + 3 * SEG_NS, false),
            frame(BASE + 3 * SEG_NS + FRAME_NS, false),
            frame(BASE + 3 * SEG_NS + 2 * FRAME_NS, false),
        ];
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, bad),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        assert!(
            r.any_onset_undecodable,
            "the black onset must set any_onset_undecodable"
        );
        assert!(
            r.any_wakeup_missing,
            "no decodable onset frame -> any_wakeup_missing"
        );
        let t = r
            .transitions
            .iter()
            .find(|t| t.cambox == "CAM1")
            .expect("the cold CAM1 transition");
        assert!(!t.clean, "a wake-up-gap cold cut is NOT clean");
        assert_eq!(t.onset_undecodable, 3);
        assert!(
            !cold_cut_gate_pass(&r),
            "the report-only gate verdict fails on a wake-up gap"
        );
    }

    #[test]
    fn warm_clean_cold_cut_passes_the_gate() {
        // A 60s-hidden cold cut whose onset is fully decodable from the switch -> clean, gate ok.
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, warm_onset(BASE + 3 * SEG_NS)),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        assert!(!r.any_onset_undecodable);
        assert!(!r.any_wakeup_missing);
        assert!(!r.any_wakeup_over_max);
        let t = r
            .transitions
            .iter()
            .find(|t| t.cambox == "CAM1")
            .expect("the cold CAM1 transition");
        assert!(t.clean, "an immediately-decodable cold cut is clean");
        assert_eq!(
            t.wakeup_latency_ns,
            Some(0),
            "first frame decodable at the switch instant"
        );
        assert!(
            cold_cut_gate_pass(&r),
            "a clean warm baseline passes the report-only gate verdict"
        );
    }

    #[test]
    fn late_wakeup_over_ceiling_is_flagged() {
        // First decodable frame arrives one frame past the 2-frame ceiling -> over-max, not clean.
        let late = vec![
            frame(BASE + 3 * SEG_NS, false),
            frame(BASE + 3 * SEG_NS + 3 * FRAME_NS, true), // ~100ms > 66.7ms ceiling
        ];
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, late),
        ];
        let r = build_report(&windows);
        assert!(
            r.any_wakeup_over_max,
            "a wake-up past the ceiling is flagged"
        );
        assert!(
            r.any_onset_undecodable,
            "the leading undecodable frame is also flagged"
        );
        assert_eq!(r.worst_wakeup_latency_ns, Some(3 * FRAME_NS));
    }

    // --- gates_overall_pass (issue 768 report-only) ----------------------------------------

    #[test]
    fn gates_overall_pass_is_report_only_768() {
        assert!(
            !gates_overall_pass(),
            "#768: the cold-cut onset seam is report-only (calibration-first) until a warm \
             baseline + a deliberate keepalive-bypass cold cut exist"
        );
    }
}
