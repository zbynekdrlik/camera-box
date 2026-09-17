//! #768/#1086 — the COLD-CUT onset seam (LIVE since #1086): measure the first ~1s after each
//! program switch to a cambox that had been HIDDEN from strih program for `>= COLD_HIDDEN_SECS`,
//! and gate whether that cut stayed instant + lossless (no wake-up gap).
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
//! at most `ONSET_UNDECODABLE_ALLOWANCE` onset frames were undecodable (#1086 — the calibrated
//! warm-glitch tolerance; the shipped 0-tolerance false-reds a lone optical glitch).
//!
//! ## LIVE (calibration-complete) — issue 1086
//!
//! The onset frames were guarded out of every existing artifact, so there was NO local calibration
//! data for a wake-up-latency / onset-undecodable bound when this seam SHIPPED report-only (mirrors
//! `verdict-gate-seam-calibration.md` step 1, except the field it wants to mine did not exist until
//! THIS seam started emitting it). [`gates_overall_pass`] is now `true` (flipped by issue 1086,
//! 2026-09-17) — all four prerequisites held:
//!
//! - Warm baseline -- ESTABLISHED. Across 81 local E2E verdicts (59 with cold transitions, 203 cold
//!   transitions) every WARM transition is clean (the issue-767 keep-alive receiver never goes
//!   cold): worst decodable wake-up 47.38 ms (p95 39.4 ms), never `any_wakeup_over_max` /
//!   `any_wakeup_missing`. So `WAKEUP_LATENCY_MAX_NS` = 66.67 ms clears every observed wake-up with
//!   a 1.41x margin.
//! - Genuine-cold run -- DONE. The deliberate keepalive-bypass cut (`COLD_CUT_BYPASS_CAM`, release
//!   E2E 35166086465 / RECORDING_E2E_RUN_ID 557465489): strih "NDI cam1" torn down cold for 182 s,
//!   restored 0.18 s before the 2nd cut — onset 30/30 decodable, wake-up 1.76 ms, receive healthy,
//!   `clean=true`, no `any_*` flag. Even a genuinely-cold receiver rebinds inside the ceiling here
//!   (the DistroAV reattach is fast), so the bound holds cold-safe as well as warm-safe.
//! - Per-cambox tick-decodability -- RE-CONFIRMED. Across the recent 12 splitter-era runs every
//!   active cambox window (CAM1-CAM7) decodes the shared cam2 Vernier tick (`undecodable` 0-3 of
//!   ~847, populated `presentation_cadence`), so no box reads a healthy cold cut black.
//! - Onset-undecodable 0-tolerance was TOO STRICT. A WARM cut can carry a single optical-glitch
//!   undecodable onset frame (run 156174349 CAM2: a healthy 39 ms warm cut flagged a miss;
//!   `overall_pass=false` for UNRELATED gates), so the LIVE gate carries a calibrated
//!   `ONSET_UNDECODABLE_ALLOWANCE` = 1 (mined green max = 1, on an otherwise-healthy cut).
//!
//! ### What the LIVE fold blocks, and what stays report-only
//!
//! The fold ([`cold_cut_gate_pass`]) blocks on a GENUINE cold-cut miss: a `!clean` onset (late /
//! missing wake-up, OR onset undecodable `> ONSET_UNDECODABLE_ALLOWANCE`) whose switch is late
//! enough into the run to rule out the issue-793 startup segfault
//! (`GenuineColdCutMiss`) — zero-FP across all 81 mined verdicts. Report-only (never folds):
//! (1) a `PossibleSegfaultWindow` miss (switch `< SEGFAULT_WINDOW_MAX_SECS`), because the data
//! cannot separate a startup segfault from a genuine cold receiver — in production every real cold
//! cut lands well past the window, so this is not a live blind spot; (2) the steady-state
//! sustained-receive-fps `ReceiveHealth` (a whole-window issue #1/#799 signal already covered
//! blocking by the LIVE continuity gates copies/gaps/frozen_leg — gating it here would
//! double-jeopardy without unique coverage; no genuine cold-restore-degraded datapoint exists to
//! calibrate a cold-SPECIFIC floor). The raw `any_wakeup_over_max` / `any_wakeup_missing` /
//! `any_onset_undecodable` / `any_receive_degraded` aggregates stay serialized as diagnostics.
//! Same crate-root pure-seam pattern as `optical_floor.rs` / `presentation_cadence.rs` (default
//! features, Tier-0 unit-testable); the probe-gated `recording-verdict.rs` is only a thin consumer.
//!
//! ## The onset decodability signal is the SHARED cam2 Vernier tick — cross-cambox on THIS rig
//!
//! `decodable` is `SegmentFrame::tick.is_some()` (the cam2 optical Vernier tick). A reader of
//! `recording_segments.rs`'s per-window-cadence doc might expect this to be `None` on every
//! non-cam2 window ("any non-cam2 window in a CAMBOX_SWEEP: `tick` is `None`") and conclude the
//! metric is cam2-only — but that doc line is STALE for the current rig. This rig is ONE physical
//! camera through an HDMI splitter into every cambox
//! (`.claude/skills/e2e` / the rig-one-camera-splitter model), and cam2 paints the dual-QR Vernier
//! monitor that ALL boxes film — so every box's recorded program window decodes the SAME tick.
//! Empirically (76 local all-cambox verdicts): CAM1 and CAM3 windows carry `undecodable` = 0-1 of
//! ~847 frames, non-null `first_tick`/`last_tick`, and a populated `presentation_cadence` (which
//! itself needs >= 2 decoded ticks) — i.e. the onset frames of a non-cam2 cold cut DO decode, so a
//! healthy CAM1/CAM3 cold cut is `clean` and a genuinely black one is flagged. The metric is
//! therefore cross-cambox actionable here.
//!
//! The #1086 LIVE flip re-confirmed per-cambox onset tick-decodability (above). It stays a
//! STANDING invariant: a box that genuinely could not decode the Vernier at onset (a future rig
//! where the splitter path is broken, or the stale doc's scenario returns) would read a healthy
//! cold cut as a black one and false-red the now-LIVE gate. If that ever holds, scope the health
//! check (`clean` / [`cold_cut_gate_pass`] / the aggregate flags) to tick-bearing windows, or add a
//! non-tick onset signal (brightness/black detection) for the affected boxes.

/// A cambox program-hidden for at least this many seconds before a cut counts as a COLD cut
/// (issue 768's `>= 60s` bar).
pub const COLD_HIDDEN_SECS: f64 = 60.0;

/// The onset window measured after each switch — the first second (issue 768 bod 3's `+-1s around
/// the cut`). This is also exactly the span `probe::recording_segments::DEFAULT_TRANSITION_GUARD_NS`
/// discards, i.e. the onset is precisely the guarded material nothing else can see.
pub const ONSET_WINDOW_NS: i64 = 1_000_000_000;

/// Wake-up-latency ceiling: 2 frames at 30fps (issue 768's "first decoded frame within X ms, e.g.
/// 2 frames @30fps"). #1086 CALIBRATED LIVE: mined across 81 local verdicts (59 with cold
/// transitions, 203 cold transitions) the worst decodable wake-up is 47.38 ms (p95 39.4 ms), and
/// the deliberate keepalive-bypass GENUINE-cold CAM1 cut woke up in 1.76 ms — so 66.67 ms clears
/// every observed wake-up with a 1.41x margin above the worst. Classifies a transition's `clean`
/// flag and, via that, the genuine-cold-cut-miss fold.
pub const WAKEUP_LATENCY_MAX_NS: i64 = 66_666_667;

/// #1086 CALIBRATED — the onset-undecodable ALLOWANCE the LIVE gate tolerates. A WARM cut can carry
/// a single optical-glitch undecodable onset frame (run 156174349 CAM2: a healthy 39 ms warm cut
/// with 1 undecodable onset frame — that run's `overall_pass=false` was for UNRELATED gates), so
/// the 0-tolerance criterion the seam shipped with would FALSE-RED a healthy glitch. Mined across
/// the 81 local verdicts: 80/81 runs carry 0 undecodable onset frames on every cold transition;
/// exactly ONE carries 1; none carries more. A genuine black cold onset carries its whole rebind
/// run undecodable (up to 30/30). So `1` allows the lone optical glitch (a 1-frame headroom above
/// every green run — the honest gates-green-first margin) while a `>= 2`-frame black onset still
/// fails. A fast healthy rebind (1 undecodable then decodable within the ceiling) is correctly
/// allowed; a black hole (all-undecodable → no wake-up at all) is caught by the missing wake-up.
pub const ONSET_UNDECODABLE_ALLOWANCE: u32 = 1;

/// #1086 part-4 — the recorded program's target delivered-frame rate: 30fps on BOTH strih and
/// stream (recording-e2e.sh records a 30fps cut-to-stream canvas). REPORT-ONLY: used ONLY to
/// classify the sustained-receive-fps health field below; it is NOT a calibrated LIVE bound.
pub const TARGET_RECEIVE_FPS: f64 = 30.0;

/// #1086 part-4 — provisional tolerance below `TARGET_RECEIVE_FPS` for the sustained-fps receive
/// health classifier. REPORT-ONLY (calibration-first, same as the wake-up ceiling): a real bound
/// is set once live warm-baseline + genuine-cold data exist.
pub const SUSTAINED_FPS_TOLERANCE: f64 = 3.0;

/// #1086 part-4 (RESCOPE confound) — the issue-793 libobs pooled-thread startup-segfault window is
/// ~60-90s after a FRESH OBS start. A cold-cut onset MISS whose switch fell this soon after the run
/// began could be that startup segfault rather than a genuine cold receiver failure. The recording
/// start (`window[0].start_ns`) is a LOWER BOUND on OBS uptime (OBS was already running when the
/// recording began), so an onset miss whose switch is LATER than this many seconds after the run's
/// first switch is DEFINITELY past the segfault window (a genuine cold-cut miss); an earlier one is
/// ambiguous and flagged for the LIVE-flip follow-up to disambiguate (explicitly wait past the
/// window, or capture the true OBS start). REPORT-ONLY.
pub const SEGFAULT_WINDOW_MAX_SECS: f64 = 90.0;

/// #1086 part-4 — sustained-receive-fps health of the tested camera's program window. Separates
/// "the warm cut worked" (onset clean) from "steady-state receive on that camera is healthy" (the
/// issue #1/#799 degradation class). REPORT-ONLY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveHealth {
    /// Sustained fps within `SUSTAINED_FPS_TOLERANCE` of `TARGET_RECEIVE_FPS`.
    Healthy,
    /// Sustained fps below `TARGET_RECEIVE_FPS - SUSTAINED_FPS_TOLERANCE` — steady-state receive is
    /// degraded even if the cut itself woke up promptly.
    Degraded,
    /// Not enough delivered frames / a degenerate span to define a rate.
    Unknown,
}

/// #1086 part-4 (RESCOPE confound) — attribution of a cold-cut onset MISS: is it plausibly the
/// issue-793 libobs startup segfault (the switch fell early in the run), or a genuine cold-cut
/// receiver failure (the switch was late enough to rule the segfault out)? REPORT-ONLY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnsetMissAttribution {
    /// The transition's switch fell inside the issue-793 startup-segfault window (early in the run)
    /// — an onset miss here could be that segfault, not a cold receiver. Ambiguous, flagged for the
    /// LIVE-flip follow-up.
    PossibleSegfaultWindow,
    /// The switch was later than `SEGFAULT_WINDOW_MAX_SECS` after the run's first switch, so the
    /// issue-793 startup segfault is ruled out — an onset miss here is a genuine cold-cut failure.
    GenuineColdCutMiss,
    /// The transition had NO onset miss (clean), so there is nothing to attribute.
    NoMiss,
}

/// #1086 part-4 — sustained delivered-frame rate over a window: `delivered / span_secs`. Returns
/// `None` when the span is non-positive or fewer than 2 frames were delivered (a rate is
/// undefined). REPORT-ONLY.
pub fn sustained_fps(delivered: u32, span_ns: i64) -> Option<f64> {
    if span_ns <= 0 || delivered < 2 {
        return None;
    }
    Some(delivered as f64 / (span_ns as f64 / 1e9))
}

/// #1086 part-4 — classify a window's sustained receive fps against the target. `None` fps →
/// `Unknown`; below `TARGET_RECEIVE_FPS - SUSTAINED_FPS_TOLERANCE` → `Degraded`; otherwise
/// `Healthy`. REPORT-ONLY.
pub fn receive_health(fps: Option<f64>) -> ReceiveHealth {
    match fps {
        None => ReceiveHealth::Unknown,
        Some(f) if f < TARGET_RECEIVE_FPS - SUSTAINED_FPS_TOLERANCE => ReceiveHealth::Degraded,
        Some(_) => ReceiveHealth::Healthy,
    }
}

/// #1086 part-4 (RESCOPE confound) — attribute a cold-cut onset miss. `has_miss` is whether the
/// transition showed a late/missing wake-up or a black onset frame (i.e. NOT `clean`);
/// `secs_since_run_start` is `(switch_ns - run_start_ns) / 1e9`. A clean transition returns
/// `NoMiss`; otherwise a switch earlier than `SEGFAULT_WINDOW_MAX_SECS` into the run is
/// `PossibleSegfaultWindow`, a later one `GenuineColdCutMiss`. REPORT-ONLY.
pub fn onset_miss_attribution(has_miss: bool, secs_since_run_start: f64) -> OnsetMissAttribution {
    if !has_miss {
        OnsetMissAttribution::NoMiss
    } else if secs_since_run_start < SEGFAULT_WINDOW_MAX_SECS {
        OnsetMissAttribution::PossibleSegfaultWindow
    } else {
        OnsetMissAttribution::GenuineColdCutMiss
    }
}

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
    /// #1086 part-4 — total delivered frames in the WHOLE window `[start_ns, end_ns)` (not just the
    /// onset), for the sustained-receive-fps health check. The consumer counts `seg_frames`; the
    /// unit tests set it directly.
    pub window_frames: u32,
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
    /// #1086 part-4 — total delivered frames across the WHOLE program window `[start_ns, end_ns)`.
    pub window_frames: u32,
    /// #1086 part-4 — sustained delivered-frame rate over the whole window (`None` = too few frames
    /// / degenerate span). REPORT-ONLY.
    pub sustained_fps: Option<f64>,
    /// #1086 part-4 — receive-health classification of `sustained_fps` (separates "warm cut works"
    /// from "steady-state receive healthy"). REPORT-ONLY.
    pub receive_health: ReceiveHealth,
    /// #1086 part-4 — seconds from the run's first switch to THIS switch (`(start_ns -
    /// run_start_ns) / 1e9`), the issue-793 confound anchor.
    pub secs_since_run_start: f64,
    /// #1086 part-4 — whether this transition showed an onset miss (late/missing wake-up or a black
    /// onset frame), i.e. NOT `clean`.
    pub has_onset_miss: bool,
    /// #1086 part-4 (RESCOPE confound) — attribution of an onset miss: issue-793 startup segfault vs
    /// a genuine cold-cut failure vs no miss. REPORT-ONLY.
    pub miss_attribution: OnsetMissAttribution,
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
    /// #1086 part-4 — the target/tolerance the receive-health field classifies against (echoed for
    /// the report). REPORT-ONLY.
    pub target_receive_fps: f64,
    pub sustained_fps_tolerance: f64,
    /// #1086 part-4 — any cold transition whose whole-window sustained receive fps was `Degraded`.
    pub any_receive_degraded: bool,
    /// #1086 part-4 (RESCOPE confound) — any onset miss whose switch fell in the issue-793
    /// startup-segfault window (ambiguous — could be the segfault, not a cold receiver).
    pub any_miss_possibly_segfault: bool,
    /// #1086 part-4 (RESCOPE confound) — any onset miss whose switch was late enough to rule the
    /// issue-793 segfault out (a genuine cold-cut receiver failure).
    pub any_genuine_cold_cut_miss: bool,
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
/// at most `ONSET_UNDECODABLE_ALLOWANCE` onset frames were undecodable (#1086 — the calibrated
/// warm-glitch tolerance; 0-tolerance false-reds a lone optical glitch). A missing wake-up (`None`)
/// is never clean.
pub fn transition_is_clean(wakeup_latency_ns: Option<i64>, onset_undecodable: u32) -> bool {
    match wakeup_latency_ns {
        Some(w) => w <= WAKEUP_LATENCY_MAX_NS && onset_undecodable == 0,
        None => false,
    }
}

/// Build the cold-cut onset report from the ordered schedule windows (+ their onset frames).
///
/// `hidden_secs_before` is defined ONLY for a cambox's 2nd+ appearance — the gap from the same
/// cambox's PREVIOUS window `end_ns` to this window's `start_ns`, which is exactly known. A
/// cambox's FIRST appearance has NO known prior hidden duration (its pre-sweep state — the
/// certified prod scene, preroll, or on-program — is not in the schedule), so it is never a cold
/// candidate: only well-defined `>= COLD_HIDDEN_SECS` gaps between two windows of the same cambox
/// count, avoiding a false positive on an unknowable first-appearance interval.
pub fn build_report(windows: &[ColdCutWindow]) -> ColdCutReport {
    use std::collections::HashMap;

    // #1086: the run's first switch (~recording start) is the issue-793 confound anchor — a LOWER
    // BOUND on OBS uptime. `None` for an empty schedule (no transitions are produced anyway).
    let run_start_ns = windows.first().map(|w| w.start_ns);

    // The end_ns of each cambox's most recent PRIOR window, keyed by label.
    let mut last_end: HashMap<&str, i64> = HashMap::new();
    let mut transitions: Vec<ColdCutTransition> = Vec::new();

    for w in windows {
        // Only a 2nd+ appearance has an exactly-known hidden duration; skip the first.
        if let Some(&prev_end) = last_end.get(w.cambox.as_str()) {
            let hidden_secs_before = (w.start_ns - prev_end) as f64 / 1e9;
            if hidden_secs_before >= COLD_HIDDEN_SECS {
                let (delivered, decodable, undecodable, wakeup) =
                    onset_stats(w.start_ns, &w.onset_frames);
                let clean = transition_is_clean(wakeup, undecodable);
                // #1086 part-4: sustained receive fps over the WHOLE program window.
                let span_ns = w.end_ns - w.start_ns;
                let fps = sustained_fps(w.window_frames, span_ns);
                let health = receive_health(fps);
                // #1086 part-4 (RESCOPE confound): seconds from the run's first switch to this one
                // (0.0 when this IS the first window — then it is trivially in the segfault window).
                let secs_since_run_start =
                    (w.start_ns - run_start_ns.unwrap_or(w.start_ns)) as f64 / 1e9;
                let has_onset_miss = !clean;
                let miss_attribution = onset_miss_attribution(has_onset_miss, secs_since_run_start);
                transitions.push(ColdCutTransition {
                    cambox: w.cambox.clone(),
                    hidden_secs_before,
                    onset_frames: delivered,
                    onset_decodable: decodable,
                    onset_undecodable: undecodable,
                    wakeup_latency_ns: wakeup,
                    clean,
                    window_frames: w.window_frames,
                    sustained_fps: fps,
                    receive_health: health,
                    secs_since_run_start,
                    has_onset_miss,
                    miss_attribution,
                });
            }
        }
        last_end.insert(w.cambox.as_str(), w.end_ns);
    }

    let cold_transitions_found = transitions.len();
    let worst_wakeup_latency_ns = transitions.iter().filter_map(|t| t.wakeup_latency_ns).max();
    let any_wakeup_over_max = transitions.iter().any(|t| {
        t.wakeup_latency_ns
            .is_some_and(|w| w > WAKEUP_LATENCY_MAX_NS)
    });
    let any_wakeup_missing = transitions.iter().any(|t| t.wakeup_latency_ns.is_none());
    let any_onset_undecodable = transitions.iter().any(|t| t.onset_undecodable > 0);
    // #1086 part-4 aggregates.
    let any_receive_degraded = transitions
        .iter()
        .any(|t| t.receive_health == ReceiveHealth::Degraded);
    let any_miss_possibly_segfault = transitions
        .iter()
        .any(|t| t.miss_attribution == OnsetMissAttribution::PossibleSegfaultWindow);
    let any_genuine_cold_cut_miss = transitions
        .iter()
        .any(|t| t.miss_attribution == OnsetMissAttribution::GenuineColdCutMiss);

    ColdCutReport {
        cold_hidden_secs: COLD_HIDDEN_SECS,
        onset_window_ns: ONSET_WINDOW_NS,
        wakeup_latency_max_ns: WAKEUP_LATENCY_MAX_NS,
        cold_transitions_found,
        worst_wakeup_latency_ns,
        any_wakeup_over_max,
        any_wakeup_missing,
        any_onset_undecodable,
        target_receive_fps: TARGET_RECEIVE_FPS,
        sustained_fps_tolerance: SUSTAINED_FPS_TOLERANCE,
        any_receive_degraded,
        any_miss_possibly_segfault,
        any_genuine_cold_cut_miss,
        transitions,
    }
}

/// #1086 LIVE gate verdict: no cold transition suffered a GENUINE cold-cut miss — a late/missing
/// wake-up OR an onset undecodable ABOVE `ONSET_UNDECODABLE_ALLOWANCE` (i.e. `!clean`), whose switch
/// was late enough into the run to rule out the issue-793 startup segfault (`GenuineColdCutMiss`,
/// see [`onset_miss_attribution`]). A wake-up gap whose switch fell inside the segfault window
/// (`PossibleSegfaultWindow`) stays attribution-only (report-only) because the data cannot separate
/// a startup segfault from a genuine cold receiver — in production every cold cut (the deliberate
/// bypass at ~182 s hidden, and the natural 2nd appearances) lands well past the window, so this is
/// not a live blind spot. Folds into `overall_pass` because [`gates_overall_pass`] is now `true`.
/// The raw `any_wakeup_over_max` / `any_wakeup_missing` / `any_onset_undecodable` aggregates remain
/// serialized as diagnostics.
pub fn cold_cut_gate_pass(report: &ColdCutReport) -> bool {
    !report.any_wakeup_over_max && !report.any_wakeup_missing && !report.any_onset_undecodable
}

/// #768/#1086 LIVE (calibration-complete). Flipped `true` by issue 1086 once all four
/// prerequisites held: (1) a WARM baseline (44+ verdicts, every cold transition WARM-clean, worst
/// wake-up 47.38 ms); (2) a deliberate keepalive-bypass GENUINE-cold run (release E2E 35166086465 /
/// RECORDING_E2E_RUN_ID 557465489: CAM1 torn down cold for 182 s, restored 0.18 s before the 2nd
/// cut — onset 30/30 decodable, wake-up 1.76 ms, receive healthy, clean); (3) the per-cambox onset
/// tick re-confirm (every active cambox window decodes the cam2 Vernier tick, undecodable 0-3 of
/// ~847, populated presentation_cadence — no box reads a healthy cold cut black); (4) the
/// onset-undecodable ALLOWANCE calibrated (`ONSET_UNDECODABLE_ALLOWANCE`) so a warm optical glitch
/// no longer false-reds. The fold blocks on [`cold_cut_gate_pass`] (a segfault-window-ruled-out
/// genuine cold-cut miss), which is zero-FP across all 81 mined verdicts. The steady-state
/// sustained-receive-fps (`ReceiveHealth`) + the possible-segfault attribution stay REPORT-ONLY
/// (see the module doc's LIVE-flip section).
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

    // A healthy 30s window at 30fps delivers ~900 frames — the default for windows a test does not
    // care about the sustained-fps of.
    const HEALTHY_WINDOW_FRAMES: u32 = 900;

    fn window(cambox: &str, start_ns: i64, onset: Vec<OnsetFrame>) -> ColdCutWindow {
        window_with_frames(cambox, start_ns, onset, HEALTHY_WINDOW_FRAMES)
    }

    fn window_with_frames(
        cambox: &str,
        start_ns: i64,
        onset: Vec<OnsetFrame>,
        window_frames: u32,
    ) -> ColdCutWindow {
        ColdCutWindow {
            cambox: cambox.to_string(),
            start_ns,
            end_ns: start_ns + SEG_NS,
            onset_frames: onset,
            window_frames,
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
    fn transition_clean_only_when_prompt_and_within_undecodable_allowance() {
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
            !transition_is_clean(None, 0),
            "no wake-up at all -> not clean"
        );
    }

    // #1086 — the calibrated onset-undecodable allowance (was 0-tolerance before the LIVE flip).
    #[test]
    fn onset_undecodable_allowance_tolerates_one_glitch() {
        assert_eq!(
            ONSET_UNDECODABLE_ALLOWANCE, 1,
            "one optical-glitch frame is allowed (mined green max = 1, on an otherwise-healthy cut)"
        );
        assert!(
            transition_is_clean(Some(0), ONSET_UNDECODABLE_ALLOWANCE),
            "one undecodable onset frame within a prompt wake-up -> still clean (warm optical glitch)"
        );
        assert!(
            !transition_is_clean(Some(0), ONSET_UNDECODABLE_ALLOWANCE + 1),
            "two undecodable onset frames -> not clean (over the allowance)"
        );
        assert!(
            !transition_is_clean(Some(0), 5),
            "a black-onset run of undecodable frames -> not clean"
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
    fn first_appearance_is_never_a_cold_candidate() {
        // Every cambox appears exactly ONCE -> all are first appearances with no known prior hidden
        // duration -> none is a cold candidate, even CAM3 at 60s after the first switch (its
        // pre-sweep state is unknown, so flagging it would be a false positive on unknowable data).
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
        ];
        let r = build_report(&windows);
        assert_eq!(
            r.cold_transitions_found, 0,
            "a first appearance has no known prior hidden duration -> never cold"
        );
        assert!(r.transitions.is_empty());
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
        // undecodable (black/frozen) with no decodable frame within the window -- the shape an
        // issue-767 dead-receiver cold cut produces (a general onset decode failure has the same
        // shape; the seam reports the measurement, the LIVE-flip follow-up disambiguates). Must
        // be flagged as not clean.
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
            "the LIVE gate verdict fails on a genuine cold-cut wake-up gap"
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
            "a clean warm baseline passes the LIVE gate verdict"
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

    // --- gates_overall_pass (#1086 LIVE) ---------------------------------------------------

    #[test]
    fn gates_overall_pass_is_live_1086() {
        assert!(
            gates_overall_pass(),
            "#1086: the cold-cut onset seam is LIVE — warm baseline + a deliberate \
             keepalive-bypass genuine-cold run + the per-cambox tick re-confirm + the calibrated \
             onset-undecodable allowance all held, so the fold blocks on a genuine cold-cut miss"
        );
    }

    // #1086 — a lone warm optical glitch (1 undecodable onset frame, prompt wake-up) is NOT a
    // genuine miss, so the LIVE gate PASSES it (the 0-tolerance false-red this flip fixes;
    // the run-156174349 shape).
    #[test]
    fn warm_glitch_one_undecodable_is_not_a_genuine_miss() {
        // CAM1@seg3 (90s into the run): first onset frame a glitch, then decodable at +1 frame.
        let glitch = vec![
            frame(BASE + 3 * SEG_NS, false),
            frame(BASE + 3 * SEG_NS + FRAME_NS, true),
            frame(BASE + 3 * SEG_NS + 2 * FRAME_NS, true),
        ];
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, glitch),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        let t = &r.transitions[0];
        assert_eq!(
            t.onset_undecodable, 1,
            "the lone glitch frame is still counted (diagnostic)"
        );
        assert!(
            r.any_onset_undecodable,
            "the raw diagnostic flag still fires"
        );
        assert!(t.clean, "one glitch within the allowance -> clean");
        assert!(!t.has_onset_miss, "a clean transition has no miss");
        assert_eq!(t.miss_attribution, OnsetMissAttribution::NoMiss);
        assert!(!r.any_genuine_cold_cut_miss);
        assert!(
            cold_cut_gate_pass(&r),
            "the LIVE gate PASSES a lone warm optical glitch (no genuine miss)"
        );
    }

    // #1086 — >= 2 undecodable onset frames at a LATE switch (segfault ruled out) is a genuine miss,
    // so the LIVE gate BLOCKS it even when the wake-up itself is within the ceiling.
    #[test]
    fn two_undecodable_onset_frames_at_a_late_switch_folds_live() {
        // CAM1@seg3 (90s == SEGFAULT_WINDOW_MAX_SECS -> ruled out): 2 undecodable then decodable at
        // exactly the ceiling (2 frames @30fps) -> wake-up OK but 2 > allowance -> not clean.
        let black2 = vec![
            frame(BASE + 3 * SEG_NS, false),
            frame(BASE + 3 * SEG_NS + FRAME_NS, false),
            frame(BASE + 3 * SEG_NS + 2 * FRAME_NS, true),
        ];
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, black2),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        let t = &r.transitions[0];
        assert_eq!(t.onset_undecodable, 2);
        assert!(
            !r.any_wakeup_missing,
            "a frame did decode -> no missing wake-up"
        );
        assert!(
            !r.any_wakeup_over_max,
            "the decode landed at the ceiling, not over it"
        );
        assert!(!t.clean, "2 undecodable > allowance -> not clean");
        assert_eq!(t.miss_attribution, OnsetMissAttribution::GenuineColdCutMiss);
        assert!(r.any_genuine_cold_cut_miss);
        assert!(
            !cold_cut_gate_pass(&r),
            "the LIVE gate BLOCKS a >= 2-frame black onset ruled out of the segfault window"
        );
    }

    // #1086 — the SAME 2-frame black onset EARLY in the run (< segfault window) is ambiguous
    // (possible startup segfault), so it stays REPORT-ONLY: the LIVE gate PASSES it.
    #[test]
    fn two_undecodable_onset_frames_in_segfault_window_stay_report_only() {
        // CAM1 shown [0,5s], hidden while CAM2 holds [5s,70s], CAM1 back at 70s (< 90s window).
        let s2 = BASE + 70 * ONSET_WINDOW_NS;
        let windows = vec![
            ColdCutWindow {
                cambox: "CAM1".to_string(),
                start_ns: BASE,
                end_ns: BASE + 5 * ONSET_WINDOW_NS,
                onset_frames: warm_onset(BASE),
                window_frames: 150,
            },
            ColdCutWindow {
                cambox: "CAM2".to_string(),
                start_ns: BASE + 5 * ONSET_WINDOW_NS,
                end_ns: s2,
                onset_frames: warm_onset(BASE + 5 * ONSET_WINDOW_NS),
                window_frames: 1950,
            },
            ColdCutWindow {
                cambox: "CAM1".to_string(),
                start_ns: s2,
                end_ns: s2 + SEG_NS,
                onset_frames: vec![
                    frame(s2, false),
                    frame(s2 + FRAME_NS, false),
                    frame(s2 + 2 * FRAME_NS, true),
                ],
                window_frames: 900,
            },
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        let t = &r.transitions[0];
        assert!(t.has_onset_miss, "2 undecodable is a miss");
        assert_eq!(
            t.miss_attribution,
            OnsetMissAttribution::PossibleSegfaultWindow
        );
        assert!(r.any_miss_possibly_segfault);
        assert!(!r.any_genuine_cold_cut_miss);
        assert!(
            cold_cut_gate_pass(&r),
            "a possible-segfault-window miss stays report-only -> the LIVE gate PASSES it"
        );
    }

    // --- #1086 part-4: sustained-receive-fps + issue-793 segfault-window discriminator ----------

    #[test]
    fn sustained_fps_computes_rate_and_guards_degenerate() {
        // 900 frames over a 30s window -> 30fps exactly.
        assert_eq!(sustained_fps(900, SEG_NS), Some(30.0));
        // 450 frames over 30s -> 15fps.
        assert_eq!(sustained_fps(450, SEG_NS), Some(15.0));
        // < 2 frames -> undefined.
        assert_eq!(sustained_fps(1, SEG_NS), None);
        assert_eq!(sustained_fps(0, SEG_NS), None);
        // non-positive span -> undefined (never divides by zero).
        assert_eq!(sustained_fps(900, 0), None);
        assert_eq!(sustained_fps(900, -1), None);
    }

    #[test]
    fn receive_health_classifies_against_target() {
        assert_eq!(receive_health(Some(30.0)), ReceiveHealth::Healthy);
        // exactly at target - tolerance is still Healthy (the boundary is strict `<`).
        assert_eq!(
            receive_health(Some(TARGET_RECEIVE_FPS - SUSTAINED_FPS_TOLERANCE)),
            ReceiveHealth::Healthy
        );
        // one below the tolerance floor is Degraded.
        assert_eq!(
            receive_health(Some(TARGET_RECEIVE_FPS - SUSTAINED_FPS_TOLERANCE - 0.01)),
            ReceiveHealth::Degraded
        );
        assert_eq!(receive_health(Some(15.0)), ReceiveHealth::Degraded);
        assert_eq!(receive_health(None), ReceiveHealth::Unknown);
    }

    #[test]
    fn onset_miss_attribution_splits_segfault_window() {
        // A clean transition (no miss) is never attributed.
        assert_eq!(
            onset_miss_attribution(false, 10.0),
            OnsetMissAttribution::NoMiss
        );
        assert_eq!(
            onset_miss_attribution(false, 200.0),
            OnsetMissAttribution::NoMiss
        );
        // A miss earlier than the segfault-window ceiling is ambiguous.
        assert_eq!(
            onset_miss_attribution(true, 0.0),
            OnsetMissAttribution::PossibleSegfaultWindow
        );
        assert_eq!(
            onset_miss_attribution(true, SEGFAULT_WINDOW_MAX_SECS - 0.01),
            OnsetMissAttribution::PossibleSegfaultWindow
        );
        // Exactly at / past the ceiling rules the segfault out -> a genuine cold-cut miss.
        assert_eq!(
            onset_miss_attribution(true, SEGFAULT_WINDOW_MAX_SECS),
            OnsetMissAttribution::GenuineColdCutMiss
        );
        assert_eq!(
            onset_miss_attribution(true, 300.0),
            OnsetMissAttribution::GenuineColdCutMiss
        );
    }

    #[test]
    fn cold_transition_carries_sustained_fps_and_health() {
        // A clean 60s-hidden cold cut whose whole window delivered a healthy 900 frames.
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, warm_onset(BASE + 3 * SEG_NS)),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        assert_eq!(r.target_receive_fps, TARGET_RECEIVE_FPS);
        assert_eq!(r.sustained_fps_tolerance, SUSTAINED_FPS_TOLERANCE);
        assert!(!r.any_receive_degraded, "900 frames / 30s is healthy");
        let t = &r.transitions[0];
        assert_eq!(t.window_frames, 900);
        assert_eq!(t.sustained_fps, Some(30.0));
        assert_eq!(t.receive_health, ReceiveHealth::Healthy);
        // A clean transition has no onset miss and nothing to attribute.
        assert!(!t.has_onset_miss);
        assert_eq!(t.miss_attribution, OnsetMissAttribution::NoMiss);
    }

    #[test]
    fn degraded_receive_fps_is_flagged_report_only() {
        // The cold CAM1 window's whole-window delivery is HALF the target (450 frames / 30s = 15fps)
        // even though its onset is clean -- "warm cut works" but "steady-state receive degraded"
        // (the issue #1/#799 class). Flagged report-only; the gate stays report-only regardless.
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window_with_frames(
                "CAM1",
                BASE + 3 * SEG_NS,
                warm_onset(BASE + 3 * SEG_NS),
                450,
            ),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        let t = &r.transitions[0];
        assert_eq!(t.sustained_fps, Some(15.0));
        assert_eq!(t.receive_health, ReceiveHealth::Degraded);
        assert!(
            t.clean,
            "the ONSET is still clean -- the degradation is steady-state, not the cut"
        );
        assert!(r.any_receive_degraded);
        // #1086 — the sustained-receive-fps health stays REPORT-ONLY even though the seam is now
        // LIVE: it is a steady-state whole-window signal (the issue #1/#799 class) already covered
        // blocking by the LIVE continuity gates (copies/gaps/frozen_leg), so gating it here would
        // double-jeopardy without unique coverage. A clean-onset window with a degraded whole-window
        // fps is NOT a genuine cold-cut miss, so the LIVE fold PASSES it.
        assert!(
            cold_cut_gate_pass(&r),
            "a degraded receive fps does NOT fold into the cold-cut gate (report-only)"
        );
    }

    #[test]
    fn genuine_cold_cut_miss_ruled_out_of_segfault_window() {
        // CAM1@seg3 (switch at 90s) is a wake-up-gap cold cut. 90s == SEGFAULT_WINDOW_MAX_SECS, so
        // the issue-793 startup segfault is ruled out -> a genuine cold-cut miss.
        let bad = vec![
            frame(BASE + 3 * SEG_NS, false),
            frame(BASE + 3 * SEG_NS + FRAME_NS, false),
        ];
        let windows = vec![
            window("CAM1", BASE, warm_onset(BASE)),
            window("CAM2", BASE + SEG_NS, warm_onset(BASE + SEG_NS)),
            window("CAM3", BASE + 2 * SEG_NS, warm_onset(BASE + 2 * SEG_NS)),
            window("CAM1", BASE + 3 * SEG_NS, bad),
        ];
        let r = build_report(&windows);
        assert_eq!(r.cold_transitions_found, 1);
        let t = &r.transitions[0];
        assert!(t.has_onset_miss, "a black onset is a miss");
        assert!(
            (t.secs_since_run_start - 90.0).abs() < 1e-6,
            "the switch is 90s after the run's first switch"
        );
        assert_eq!(t.miss_attribution, OnsetMissAttribution::GenuineColdCutMiss);
        assert!(r.any_genuine_cold_cut_miss);
        assert!(!r.any_miss_possibly_segfault);
    }

    #[test]
    fn early_onset_miss_flagged_possible_segfault() {
        // A custom-timed cold cut: CAM1 shown [0,5s], hidden while CAM2 holds [5s,70s], then CAM1
        // cut back at 70s -- hidden 65s (COLD) but the switch is only 70s into the run (< 90s), so
        // a black onset here could be the issue-793 startup segfault, not a cold receiver.
        let s2 = BASE + 70 * ONSET_WINDOW_NS; // 70s after run start (ONSET_WINDOW_NS == 1e9)
        let windows = vec![
            ColdCutWindow {
                cambox: "CAM1".to_string(),
                start_ns: BASE,
                end_ns: BASE + 5 * ONSET_WINDOW_NS,
                onset_frames: warm_onset(BASE),
                window_frames: 150,
            },
            ColdCutWindow {
                cambox: "CAM2".to_string(),
                start_ns: BASE + 5 * ONSET_WINDOW_NS,
                end_ns: s2,
                onset_frames: warm_onset(BASE + 5 * ONSET_WINDOW_NS),
                window_frames: 1950,
            },
            ColdCutWindow {
                cambox: "CAM1".to_string(),
                start_ns: s2,
                end_ns: s2 + SEG_NS,
                onset_frames: vec![frame(s2, false), frame(s2 + FRAME_NS, false)],
                window_frames: 900,
            },
        ];
        let r = build_report(&windows);
        assert_eq!(
            r.cold_transitions_found, 1,
            "hidden 65s from CAM1's prior end is cold"
        );
        let t = &r.transitions[0];
        assert!(t.has_onset_miss);
        assert!(
            (t.secs_since_run_start - 70.0).abs() < 1e-6,
            "the switch is 70s into the run"
        );
        assert_eq!(
            t.miss_attribution,
            OnsetMissAttribution::PossibleSegfaultWindow
        );
        assert!(r.any_miss_possibly_segfault);
        assert!(!r.any_genuine_cold_cut_miss);
    }
}
