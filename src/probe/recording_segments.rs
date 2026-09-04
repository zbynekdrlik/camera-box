//! #312 Phase-1 — all-cambox per-SEGMENT continuity for the SINGLE continuous stream recording.
//!
//! ## What this proves
//!
//! The all-cambox E2E (issue #312, methodology decided 2026-06-29) switches each active cambox
//! into strih PROGRAM sequentially (~30s each). All camboxes capture the SAME painted source via
//! the HDMI splitter, so ONE continuous stream recording results that MUST stay continuity-clean
//! across the whole run — any cambox that drops shows as a continuity break in ITS ~30s window.
//!
//! Per-cambox attribution does NOT require a cambox-id in the pixels (that is the Phase-2
//! robustness upgrade — a DistroAV burn-filter change, held off the 2.5h windows-genlock build).
//! DanteSync gives µs-grade clock alignment across the rig, so the harness logs a SWITCH SCHEDULE
//! — an ordered list of `{cambox, start_ns, end_ns}` windows on the burn `gen_ts_ns` timeline —
//! and the verdict partitions the single recording's decoded frames into those windows.
//!
//! ## The check (rate-agnostic)
//!
//! For each window we discard a TRANSITION GUARD ([`DEFAULT_TRANSITION_GUARD_NS`], 1s) on EACH
//! side of every boundary — the program switch takes a few frames plus the 60→30 decimation +
//! latency to settle, so frames inside the guard are EXCLUDED from attribution (NOT counted as
//! loss). The remaining in-window frames run the painted-tick continuity check (the cam2 optical
//! Vernier tick, common to every cambox through the splitter) — see [`window_segment`], which
//! mirrors the per-node burn check's definitions (in [`crate::probe::burn_contiguity`] — the
//! `None`-credit and the integer-division decimation excess) but is painted-tick-specific (a
//! duplicate tick is a stale copy, never a misdecoded burn). We report per cambox:
//!
//! - `frames`: in-window delivered frames after the guard discard.
//! - `undecodable`: delivered frames whose painted tick did not decode.
//! - `copies`: stale/frozen frames (the painted tick repeated — it MUST advance per frame).
//! - `gaps`: real drops — a tick value genuinely absent from the window's observed set beyond the
//!   by-design step, computed ORDER-INDEPENDENTLY (#625: the stream recording occasionally
//!   delivers a frame "softened"/out of order, `#133`/`#196`/`#216` — a RECORDED-order walk
//!   misreads that benign reorder as a fault; see [`window_segment`] and
//!   [`crate::painted_tick_gaps::painted_tick_gaps`]).
//! - `pass`: `frames > 0 && <undecodable within the issue 881 calibrated floor> && copies == 0 &&
//!   gaps == 0` — the STRICT verdict, UNCHANGED meaning. See `crate::optical_floor` for the
//!   `undecodable` floor's rationale (a physical 60Hz temporal-tear artifact of the test camera's
//!   monitor, not chain loss) and why it is a permanent calibrated floor (issue 905 item 3).
//! - `relaxed_pass` (issue 889, 2026-07-30 user decision): originally `frames > 0` with
//!   `copies`/`gaps` NOT participating at all, and (issue 915, 2026-08-01 user decision) neither
//!   did the #881 optical floor — until issue 905 item 3 (2026-09-04) RE-GATED that floor
//!   (`crate::optical_floor::gates_overall_pass()` is `true` again).
//!   **2026-08-05 RE-GATE (ticket 889 comment 5196190653): `copies`/`gaps` re-joined this field,
//!   gated by a per-window tolerance** (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`,
//!   recalibrated 1 → 2 on 2026-08-06, ticket 889 comment 5198131539; recalibrated again 2 → 3
//!   later the same day, ticket 889 comment 5200533407) — at or under the tolerance
//!   the window still passes; over it, `relaxed_pass` fails again. This is the verdict
//!   `overall_pass` folds; `pass` stays strict and is never
//!   silently dropped (it drives the issue-889 per-window WARN). Computed by
//!   [`crate::window_gate::decide`] — see that module for the full decision record. The whole-run
//!   `overall_pass` ALSO computes the SUM of `undecodable` across every window against its own
//!   run-wide floor (see [`segment_continuity`] and
//!   [`SegmentedContinuity::run_wide_undecodable_within_floor`]) — issue 915 made that sum
//!   report-only, and issue 905 item 3 RE-GATED it: it FORCES a failure again while the same
//!   function returns `true`.
//!
//! The painted tick increments PER PAINTED FRAME and is captured at the cambox rate, so its
//! by-design step in the recording is the decimation factor (`expected_step`): cam→strih 60fps
//! capture of the 60Hz painter ⇒ step 1; strih→stream 60→30 ⇒ step 2. `expected_step` is a
//! PARAMETER (the binary derives it from the configured fps, the harness can override) — the
//! logic bakes NO 30-vs-60 assumption.
//!
//! A window with ZERO in-window frames FAILS (an absent cambox proves nothing — never read as a
//! pass), so the verdict can "clearly report which cameras were covered and which were not"
//! (#312 acceptance) without ever implying full coverage when a box was absent (e.g. CAM3 down #301).
//!
//! ## Phase-1 limitation — attribution needs a `gen_ts` anchor
//!
//! A frame is placed on the schedule timeline by its burn `gen_ts_ns`. A frame carrying NO
//! decodable mark at all (no node burn AND no optical QR — e.g. a fully-corrupt/black frame) has
//! no anchor and CANNOT be attributed to a cambox window; the binary counts it (`frames_without_
//! anchor` in the verdict JSON) but it does not enter a per-cambox segment. The single continuous
//! stream recording's per-node BURN contiguity verdict (the #186 headline, which runs on the same
//! recording) is what catches such corrupt frames as a burn-id gap — the all-cambox segment
//! verdict GATES ALONGSIDE it, it does not replace it. Phase-2 (a cambox-id in the burn pixels)
//! removes the schedule-correlation dependency entirely.
//!
//! This module is PURE (no I/O beyond reading the schedule file) and unit-tested with synthetic
//! frame sequences; the binary glue (extracting [`SegmentFrame`]s from decoded [`crate::probe::
//! recording::RecordingFrame`]s) lives in `recording-verdict`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The transition guard discarded on EACH side of every schedule boundary, in nanoseconds.
/// 1s — the program switch takes a few frames plus the 60→30 + latency to settle; frames inside
/// the guard are excluded from attribution, NOT counted as loss. The binary exposes an override.
pub const DEFAULT_TRANSITION_GUARD_NS: i64 = 1_000_000_000;

/// One scheduled program window: `cambox` was in strih program for burn `gen_ts_ns` in the
/// half-open range `[start_ns, end_ns)`. Boundaries are on the burn `gen_ts_ns` timeline (the
/// DanteSync-disciplined wall clock the harness logs the switch wall-times on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchWindow {
    /// Cambox label that was in program for this window, e.g. `"cam1"`, `"cam2"`, `"cam4"`.
    pub cambox: String,
    /// Inclusive start of the window on the burn `gen_ts_ns` timeline.
    pub start_ns: i64,
    /// Exclusive end of the window on the burn `gen_ts_ns` timeline.
    pub end_ns: i64,
}

/// Minimal per-frame input the segmentation needs: the recorded frame's position, its `gen_ts_ns`
/// anchor (a node burn's gen_ts — the timeline the schedule is keyed on), and its painted tick
/// (`None` when the cam2 optical QR did not decode on that delivered frame). The binary builds
/// these from the decoded stream recording; the unit tests build them directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentFrame {
    /// 0-based recorded frame index (for slot location / pixel proof).
    pub frame_index: u64,
    /// The frame's attribution timestamp on the burn `gen_ts_ns` timeline (the schedule's domain).
    pub gen_ts_ns: i64,
    /// The painted (cam2 optical Vernier) tick decoded on this delivered frame, or `None`.
    pub tick: Option<u32>,
}

/// The per-segment continuity verdict for ONE schedule window.
// #726: `presentation_cadence` carries `f64` fractions, which have no `Eq` impl (NaN) -- this
// struct drops the `Eq` derive it used to carry (nothing outside this file relied on it; only
// `PartialEq` + `Debug`, both still derived, are used by the tests' `assert_eq!`s).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CamboxSegment {
    /// The cambox attributed to this window.
    pub cambox: String,
    /// The window bounds (echoed for the report / plotting).
    pub start_ns: i64,
    pub end_ns: i64,
    /// In-window delivered frames after the transition-guard discard.
    pub frames: u32,
    /// Delivered frames whose painted tick did not decode (BURN-UNREADABLE).
    pub undecodable: u32,
    /// Stale/frozen frames — the painted tick repeated the previous present tick.
    pub copies: u32,
    /// Real dropped painted frames (a forward skip beyond the by-design decimation step, or a
    /// backward jump). See [`window_segment`].
    pub gaps: u32,
    /// #1251: the per-window copies/gaps tolerance ACTUALLY applied to THIS window's `relaxed_pass`
    /// verdict — `crate::window_gate::copies_gaps_tolerance_for_cambox(cambox)`, i.e. the default
    /// [`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`] for every box EXCEPT one carrying a
    /// per-cambox override (CAM2 → 25 while its grabber HW is sick, issue 1249; walk-back on issue
    /// 1242). Serialized into the verdict JSON so the report shows a CAM2 window went through the
    /// override, and so `SegmentedContinuity::windows_over_copies_gaps_tolerance` /
    /// `recording-verdict.rs` compare each window against ITS OWN tolerance, not one run-wide value.
    pub copies_gaps_tolerance: u32,
    /// First / last painted tick seen in-window (informational; `None` ⇒ no readable tick).
    pub first_tick: Option<u32>,
    pub last_tick: Option<u32>,
    /// This cambox's segment is clean ⇔ `frames > 0 && crate::optical_floor::
    /// window_within_floor(undecodable, frames) && copies == 0 && gaps == 0`. #881: the optical
    /// `undecodable` term carries a permanent calibrated floor (a physical 60Hz temporal-tear
    /// artifact of the test camera's monitor, not chain loss) — see `crate::optical_floor` for the
    /// full rationale. **UNCHANGED, strict, byte-for-byte the same boolean it has always held —
    /// neither issue 889 NOR issue 915 redefines this field.** `overall_pass` no longer folds
    /// `pass` directly (see `relaxed_pass` below); `pass` still drives the issue-889 per-window
    /// WARN and the `windows_failed_report_only` count on [`SegmentedContinuity`].
    pub pass: bool,
    /// Issue 889 (2026-07-30 user decision on issue 883): the verdict actually folded into
    /// `overall_pass` — `frame_count > 0`, originally WITHOUT `copies`/`gaps` at all. **2026-08-05
    /// RE-GATE (ticket 889 comment 5196190653): `copies`/`gaps` re-joined this field, gated by a
    /// per-window tolerance** (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`, recalibrated
    /// 1 → 2 → 3 on 2026-08-06; 3 → 1 on 2026-08-14 (issue 1031)) — a window with `copies` or `gaps` at or under the tolerance still
    /// passes here; over it, this field fails again (see
    /// `SegmentedContinuity::windows_over_copies_gaps_tolerance`). Issue
    /// 915 (2026-08-01 user decision): the `<undecodable within the #881 floor>` term stopped
    /// participating here while `crate::optical_floor::gates_overall_pass()` was `false` — but
    /// issue 905 item 3 (2026-09-04) RE-GATED it (that function is `true` again), so the floor
    /// term participates once more. Computed by
    /// [`crate::window_gate::decide`]. `copies`/`gaps`/the floor stay computed and printed above
    /// (visible via `pass`/`windows_failed_report_only`, never silently dropped) regardless of
    /// this field's own value. The issue-915 part of the restore path (issue 909 cam1 card +
    /// issue 881/1179 monitor) is DONE; see `crate::window_gate` for the full decision record.
    /// **#1132 (owner mandate 2026-08-19): this field was made REPORTED-ONLY — `overall_pass`
    /// stopped folding it.** The run fold used `crate::window_gate::WindowGateDecision::
    /// overall_pass_term` instead (strict copies/gaps; the tolerance rescue disarmed). Kept
    /// computed for observability: a `relaxed_pass == true` window whose recomputed
    /// `overall_pass_term == false` is a disarmed rescue visibly doing nothing, never a hidden mask.
    ///
    /// **#1220 (owner mandate, 2026-08-29): the tolerance rescue is RE-ARMED — this field now
    /// EQUALS the recomputed `overall_pass_term` again** (see `crate::window_gate::
    /// copies_gaps_tolerance_gates_overall_pass` for the full decision record). Still kept as a
    /// separate field: a future walk-down step may disarm the seam again, at which point this
    /// field resumes reporting what the tolerance channel WOULD say.
    pub relaxed_pass: bool,
    /// #333: an explicit human diagnostic, populated ONLY for a `frames == 0` window — the most
    /// likely cause is the dual-QR PAINTER box (it does not emit its own camera NDI while painting,
    /// #179) or a down / non-emitting box, NOT a chain frame loss. Surfaced so an empty window is
    /// never mistaken for a continuity break. `None` on any covered window (`frames > 0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// #1169 (owner, 2026-08-22): the LOUD per-segment note when this window's nonzero
    /// `copies`/`gaps` were ABSORBED by the `<=1/<=1` singleton allowance
    /// (`crate::window_gate::segment_singleton_note`). `Some(..)` iff the allowance was consumed
    /// (`copies <= 1 && gaps <= 1` with at least one nonzero, while the `<=3` tolerance rescue is
    /// disarmed) -- the strict `pass` field STILL reads `false` for such a window (visible), so the
    /// absorption is never silent (the #1132 masking guard). `None` on a clean window and on an
    /// over-band window that still FAILS (it fails loudly on its own). Counted run-wide by
    /// [`SegmentedContinuity::windows_singleton_allowance_consumed`].
    ///
    /// **#1220 (owner mandate, 2026-08-29): ALWAYS `None` now** — the `<=3` tolerance rescue is
    /// RE-ARMED (see `crate::window_gate::copies_gaps_tolerance_gates_overall_pass`), which makes
    /// `segment_singleton_note`'s own internal guard permanently `false` while it stays armed. The
    /// mechanism is left fully wired (never deleted) as a graduated fallback for a future walk-down
    /// step that disarms the tolerance channel again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub singleton_allowance_note: Option<String>,
    /// #726: the presentation-cadence EVENNESS of this window's painted-tick sequence (RECORDED
    /// order) — `None` when this window carries no painted tick at all (any non-cam2 window in a
    /// CAMBOX_SWEEP: `tick` is `None` on every frame, so there is nothing to classify). REPORTED
    /// per-window only — this does NOT feed the per-window `pass` field. #1036 CALIBRATED the
    /// "15fps-judder" `paired_fraction` signature and now folds the WORST `paired_fraction` across
    /// all cadence-bearing windows into the RUN-level `overall_pass` (see
    /// `crate::presentation_cadence::cadence_judder_gate_pass`); this per-window field is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation_cadence: Option<crate::presentation_cadence::CadenceEvenness>,
    /// #707 EVENT-FORENSICS — the locatable per-event breakdown of THIS window's `copies`/`gaps`:
    /// one entry per detected residual copy/gap, each carrying its own recorded frame index, tick
    /// values, switch-schedule offset, and cross-reference key — see
    /// [`crate::residual_events::residual_events`] for the exact detection rule (a different,
    /// recorded-order/per-transition view than the netted `copies`/`gaps` totals above; the two
    /// are not expected to sum to the same total). Empty on a clean window.
    pub residual_events: Vec<crate::residual_events::ResidualEvent>,
}

/// The whole-recording segmented-continuity verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentedContinuity {
    /// One verdict per schedule window, in schedule order.
    pub segments: Vec<CamboxSegment>,
    /// PASS ⇔ the schedule is non-empty AND EVERY window's [`CamboxSegment::relaxed_pass`] holds
    /// AND [`Self::run_wide_undecodable_within_floor`] holds (LIVE-gating again since issue 905
    /// item 3; see below). The `undecodable` floor is a permanent, data-calibrated gate (no longer
    /// "temporary until #881" — the 60Hz baseline is permanent). **Issue 889 (2026-07-30 user decision
    /// on issue 883, superseded by #1132 2026-08-19, RE-ARMED by #1220 2026-08-29): this folds
    /// `crate::window_gate::WindowGateDecision::overall_pass_term` (recomputed per window from
    /// `window_gate::decide`), NOT the strict `pass`. **As of #1220 the calibrated `<=3`
    /// tolerance channel is ARMED again, so `overall_pass_term` now EQUALS `relaxed_pass` exactly**
    /// (the pre-#1132 fold) — a window's nonzero copies/gaps must exceed `WINDOW_COPIES_GAPS_
    /// TOLERANCE` to fail the run; the optical floor gates inside `overall_pass_term` via its OWN
    /// seam (LIVE since issue 905 item 3; untouched by either #1132 or #1220). See
    /// `windows_failed_report_only` below and `crate::window_gate` for the full decision record,
    /// including the graduated-fallback #1169 singleton band a future walk-down step re-engages.**
    /// **2026-08-05 RE-GATE: `relaxed_pass` itself now requires `copies`/`gaps` to stay within the
    /// per-window tolerance** (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`, recalibrated
    /// 1 → 2 → 3 on 2026-08-06; 3 → 1 on 2026-08-14 (issue 1031)) — the terms are no longer FULLY report-only, see
    /// `windows_over_copies_gaps_tolerance` below.
    /// **Issue 915 (2026-08-01) made the run-wide undecodable floor stop gating this field; issue
    /// 905 item 3 (2026-09-04) RE-GATED it — it forces a failure again (see
    /// `run_wide_undecodable_within_floor` and `crate::optical_floor::gates_overall_pass` for the
    /// decision record; `RUN_UNDECODABLE_FLOOR` recalibrated 8 → 6).**
    pub overall_pass: bool,
    /// The transition guard applied (ns).
    pub guard_ns: i64,
    /// The decimation step the painted-tick continuity used.
    pub expected_step: i64,
    /// Frames discarded because they fell inside a window's transition guard (not counted as loss).
    pub discarded_guard_frames: u32,
    /// Frames whose `gen_ts_ns` fell in NO scheduled window (recorded outside any cambox's
    /// program window — reported for honesty, not attributed to any cambox).
    pub unplaceable_frames: u32,
    /// Issue 889 visibility requirement 2 — the count of windows whose STRICT verdict
    /// ([`CamboxSegment::pass`]) is `false`, i.e. how many windows would have FAILED under the
    /// pre-889 rule. Always serialized (never skipped), even at 0, so silence is never mistaken
    /// for strictness — the caller (`recording-verdict`) prints the matching loud WARN block
    /// unconditionally too. A nonzero count here with `overall_pass == true` is #889's relaxation
    /// visibly doing its job, not a hidden regression.
    pub windows_failed_report_only: u32,
    /// Issue 915 visibility requirement — the SUM of `undecodable` across every window (the
    /// run-wide half of the #881 calibrated floor's input). UNCHANGED computation from before
    /// issue 915; always serialized so the run-wide reading stays visible — see
    /// [`Self::run_wide_undecodable_within_floor`].
    pub total_undecodable: u32,
    /// Issue 915 visibility requirement — was [`Self::total_undecodable`] within the #881
    /// run-wide floor (`crate::optical_floor::run_within_floor`, `RUN_UNDECODABLE_FLOOR` = 6)? This
    /// is the run-wide term, UNCHANGED in its own computation — it is LIVE GATING again since issue
    /// 905 item 3 (2026-09-04): a `false` here forces `overall_pass` to fail while
    /// `crate::optical_floor::gates_overall_pass()` is `true` (see that function's doc). Always
    /// serialized, even when `true`, so a passing run's headroom is visible too — mirrors
    /// `windows_failed_report_only`'s issue-889 visibility precedent.
    pub run_wide_undecodable_within_floor: bool,
    /// 2026-08-05 re-gate (ticket 889 comment 5196190653, recalibrated 1 → 2 → 3 on 2026-08-06,
    /// walked 3 → 5 on 2026-08-31 issue 1243) — the DEFAULT/base per-window tolerance
    /// (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`), echoed here so the verdict JSON is
    /// self-describing without needing the binary's source. Always serialized.
    ///
    /// **#1251: this is the DEFAULT only.** The tolerance ACTUALLY applied to each window lives on
    /// `CamboxSegment::copies_gaps_tolerance` (per-window), because a per-cambox override can widen
    /// it for one box (CAM2 → 25 while its grabber HW is sick, issue 1249). Consumers deciding
    /// whether a specific window is over tolerance MUST read the per-segment field, not this one.
    pub copies_gaps_tolerance: u32,
    /// 2026-08-05 re-gate — how many windows exceed the per-window tolerance on `copies` AND/OR
    /// `gaps`. **#1251: measured against EACH window's OWN applied tolerance
    /// (`CamboxSegment::copies_gaps_tolerance`), not the run-wide default** — so a CAM2 window
    /// within its per-cambox override (25) is correctly NOT counted while a default-5 box over 5 is.
    /// These are the windows OVER their tolerance. **#1132 (2026-08-19): under the then-disarmed rescue ANY
    /// nonzero copies/gaps window failed `overall_pass`, so this over-tolerance count was a SUBSET
    /// of the windows that gated.** **#1220 (owner mandate, 2026-08-29): the tolerance is RE-ARMED,
    /// so this is now the EXACT set of windows failing on copies/gaps (modulo the independent
    /// `frame_count == 0` case)** — `overall_pass_term` fails on copies/gaps iff a window is
    /// counted here. As distinct from [`Self::windows_failed_report_only`] (which counts the
    /// STRICT absolute-zero failures that stay report-only regardless of the tolerance). Always
    /// serialized, even at 0, mirroring `windows_failed_report_only`'s issue-889 visibility
    /// precedent — a nonzero `windows_failed_report_only` with a ZERO
    /// `windows_over_copies_gaps_tolerance` is the tolerance visibly absorbing a bounded residual,
    /// not a hidden regression; a nonzero `windows_over_copies_gaps_tolerance` is a real, loud,
    /// gating failure.
    pub windows_over_copies_gaps_tolerance: u32,
    /// #1169 (owner, 2026-08-22) — how many windows had their nonzero `copies`/`gaps` ABSORBED by
    /// the `<=1/<=1` singleton allowance (`crate::window_gate::segment_singleton_allowance_consumed`),
    /// i.e. windows that FAIL the strict absolute-zero bar but PASS `overall_pass` under #1169's
    /// soft-release. Always serialized, even at 0, mirroring `windows_failed_report_only`'s
    /// issue-889 visibility precedent -- a nonzero count here with `overall_pass == true` is the
    /// singleton allowance visibly absorbing the designed paced-trickle residual (each such window
    /// also carries a `CamboxSegment::singleton_allowance_note`), never a hidden mask. Re-tighten to
    /// absolute zero (flip `segment_singleton_allowance_gates_overall_pass()` to `false`) makes any
    /// such window gate again -- issue 1169 owns that trail.
    ///
    /// **#1220 (owner mandate, 2026-08-29): ALWAYS `0` now** — the `<=3` tolerance rescue is
    /// RE-ARMED, so `segment_singleton_allowance_consumed` (this count's own per-window guard)
    /// reads permanently `false` while it stays armed. The field and the underlying mechanism stay
    /// wired (never deleted) as the graduated fallback for a future walk-down step.
    pub windows_singleton_allowance_consumed: u32,
    /// #707 EVENT-FORENSICS — every segment's [`CamboxSegment::residual_events`], concatenated in
    /// schedule order, for a caller that wants the whole run's residual events without walking
    /// `segments` itself (e.g. the Discord report / the collector script).
    pub residual_events: Vec<crate::residual_events::ResidualEvent>,
}

/// Validate a switch schedule: non-empty, each window has a non-empty label and `start_ns <
/// end_ns`, and windows are ORDERED + NON-OVERLAPPING (`window[i].start_ns >= window[i-1].end_ns`).
/// Touching boundaries (`start == prev end`) are allowed — the transition guard covers the seam.
pub fn validate_schedule(schedule: &[SwitchWindow]) -> Result<()> {
    if schedule.is_empty() {
        bail!("switch schedule is empty — at least one cambox window is required");
    }
    for (i, w) in schedule.iter().enumerate() {
        if w.cambox.trim().is_empty() {
            bail!("switch-schedule window {i} has an empty cambox label");
        }
        if w.start_ns >= w.end_ns {
            bail!(
                "switch-schedule window {i} ({}) must have start_ns < end_ns: start_ns={} end_ns={}",
                w.cambox,
                w.start_ns,
                w.end_ns
            );
        }
        if i > 0 {
            let prev = &schedule[i - 1];
            if w.start_ns < prev.end_ns {
                bail!(
                    "switch-schedule windows must be ordered + non-overlapping: window {i} ({}) \
                     start_ns={} precedes/overlaps window {} ({}) end_ns={}",
                    w.cambox,
                    w.start_ns,
                    i - 1,
                    prev.cambox,
                    prev.end_ns
                );
            }
        }
    }
    Ok(())
}

/// Parse + validate a switch schedule from its JSON text (an array of `{cambox,start_ns,end_ns}`).
pub fn parse_switch_schedule(json: &str) -> Result<Vec<SwitchWindow>> {
    let schedule: Vec<SwitchWindow> = serde_json::from_str(json).context(
        "parse switch-schedule JSON (expected an array of {\"cambox\":<str>,\"start_ns\":<i64>,\"end_ns\":<i64>})",
    )?;
    validate_schedule(&schedule)?;
    Ok(schedule)
}

/// Read + parse + validate a switch schedule from a file.
pub fn load_switch_schedule(path: &Path) -> Result<Vec<SwitchWindow>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read switch-schedule {}", path.display()))?;
    parse_switch_schedule(&text)
}

/// Where a frame at `gen_ts_ns` lands on the schedule, AFTER the transition guard. The SINGLE
/// source of truth for how a frame is attributed to a `--switch-schedule` window, shared by
/// [`segment_continuity`] (the strict painted-tick sweep) and the #583 honest imag per-segment gate
/// (`bin/recording-verdict.rs::partition_frames_by_window`, which partitions the imag RecordingFrames
/// the SAME way, then routes each window through the honest `imag_tick_gate::imag_zero_loss` gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPlacement {
    /// In this window index, outside BOTH transition guards — attributed to that cambox.
    In(usize),
    /// Inside a boundary transition guard — excluded from attribution, NOT counted as loss.
    Guard,
    /// Outside every scheduled window — not attributed to any cambox.
    Outside,
}

/// Which schedule window's `[start_ns, end_ns)` interval `gen_ts_ns` falls into, with NO guard
/// applied — `None` only when genuinely outside every scheduled window. Windows are ordered +
/// non-overlapping, so a gen_ts can fall in at most one. Shared by [`place_frame_in_window`]
/// (which layers the settle-time guard on top, for CONTENT attribution) and #741's
/// `attribute_window_indices` (`src/bin/recording-verdict.rs`), which needs the UNGUARDED
/// placement instead: a genuine program switch changes the active render source within roughly
/// one render tick (~30ms) of the boundary, so the frames immediately before/after a REAL cut
/// are — by construction — almost always inside the (much larger, ~1s) settle-time guard band on
/// their respective sides. Deriving "did a schedule boundary occur between these two frames"
/// from the GUARD-filtered placement therefore can never see the boundary it exists to detect
/// (both sides read back `None`/`Guard`); the raw, guard-free placement here answers that
/// question correctly.
pub fn raw_window_index(gen_ts_ns: i64, schedule: &[SwitchWindow]) -> Option<usize> {
    schedule
        .iter()
        .position(|w| gen_ts_ns >= w.start_ns && gen_ts_ns < w.end_ns)
}

/// Which schedule window (post-transition-guard) a frame at `gen_ts_ns` belongs to. Windows are
/// ordered + non-overlapping, so a gen_ts can fall in at most one; a frame within `guard_ns` of
/// either boundary of its window is [`WindowPlacement::Guard`] (the switch takes a few frames to
/// settle — excluded, not loss).
pub fn place_frame_in_window(
    gen_ts_ns: i64,
    schedule: &[SwitchWindow],
    guard_ns: i64,
) -> WindowPlacement {
    let guard_ns = guard_ns.max(0);
    match raw_window_index(gen_ts_ns, schedule) {
        Some(wi) => {
            let w = &schedule[wi];
            let after_lead = gen_ts_ns >= w.start_ns.saturating_add(guard_ns);
            let before_trail = gen_ts_ns < w.end_ns.saturating_sub(guard_ns);
            if after_lead && before_trail {
                WindowPlacement::In(wi)
            } else {
                WindowPlacement::Guard
            }
        }
        None => WindowPlacement::Outside,
    }
}

/// Partition `frames` into the schedule's windows by `gen_ts_ns` (discarding `guard_ns` on each
/// side of every boundary) and run the per-window painted-tick continuity check. `expected_step`
/// is the by-design decimation step of the painted tick in this recording (1 = full-rate, 2 =
/// 60→30) — kept a parameter so the logic bakes no fps assumption.
pub fn segment_continuity(
    frames: &[SegmentFrame],
    schedule: &[SwitchWindow],
    guard_ns: i64,
    expected_step: i64,
) -> SegmentedContinuity {
    let guard_ns = guard_ns.max(0);
    let expected_step = expected_step.max(1);

    let mut window_frames: Vec<Vec<SegmentFrame>> = vec![Vec::new(); schedule.len()];
    let mut discarded_guard_frames: u32 = 0;
    let mut unplaceable_frames: u32 = 0;

    for f in frames {
        // #583 — the shared window-attribution source of truth (also used by the honest imag
        // per-segment gate), so the two paths can never disagree on which window a frame belongs to.
        match place_frame_in_window(f.gen_ts_ns, schedule, guard_ns) {
            WindowPlacement::In(wi) => window_frames[wi].push(*f),
            // Inside the transition guard on one side of a boundary — excluded, NOT loss.
            WindowPlacement::Guard => {
                discarded_guard_frames = discarded_guard_frames.saturating_add(1)
            }
            WindowPlacement::Outside => unplaceable_frames = unplaceable_frames.saturating_add(1),
        }
    }

    let mut overall_pass = !schedule.is_empty();
    let mut segments = Vec::with_capacity(schedule.len());
    let mut all_residual_events = Vec::new();
    for (wi, w) in schedule.iter().enumerate() {
        let seg = window_segment(
            &w.cambox,
            w.start_ns,
            w.end_ns,
            &window_frames[wi],
            expected_step,
        );
        // #1132 (owner mandate 2026-08-19): fold the BLOCKING verdict (`overall_pass_term`),
        // recomputed from the SAME raw counts `window_segment` already stored on the segment --
        // single source of truth, the identical re-derivation `windows_over_copies_gaps_tolerance`
        // below already does from `seg.copies`/`seg.gaps`. The optical undecodable floor stays
        // report-only inside `overall_pass_term` EXACTLY as in `relaxed_pass` (issue 915/905,
        // untouched by #1132 or #1220).
        //
        // #1220 (owner mandate, 2026-08-29): the copies/gaps tolerance rescue is RE-ARMED again --
        // `overall_pass_term` now EQUALS `relaxed_pass` exactly (see
        // `crate::window_gate::copies_gaps_tolerance_gates_overall_pass` for the full decision
        // record). The #1169 `<=1/<=1` singleton band stays wired as a graduated fallback.
        //
        // #1243 (walk-back: issue 1242): this IS the cambox per-segment blocking headline fold, and
        // it folds the RELAXED verdict (`overall_pass_term`, == `relaxed_pass` while #1220's
        // tolerance seam is armed) — NEVER the strict `pass`. The strict `pass`, copies, gaps and
        // residual_events stay COMPUTED and reported (dormant: `windows_failed_report_only` and each
        // `CamboxSegment`), per `.claude/rules/gate-allowance-restore-red-green.md`. Run 1629895310
        // (5/10 windows with 1 copy, all relaxed_pass=true) rides this to green. issue 1242
        // root-causes the ~0.06% residual FIFO churn and RESTORES the strict copies==0 fold.
        //
        // #1251: re-derive with the SAME per-window tolerance the segment already applied
        // (`seg.copies_gaps_tolerance`), so a per-cambox override (CAM2 → 25, issue 1249) folds the
        // blocking verdict against that window's OWN band — and the fold can never disagree with the
        // `relaxed_pass` the segment stored (same tolerance, same counts).
        overall_pass &= crate::window_gate::decide_with_tolerance(
            seg.frames,
            seg.undecodable,
            seg.copies,
            seg.gaps,
            seg.copies_gaps_tolerance,
        )
        .overall_pass_term;
        all_residual_events.extend(seg.residual_events.iter().cloned());
        segments.push(seg);
    }

    // #881 — the run-wide half of the calibrated optical-undecodable floor: the SUM across every
    // window (see `crate::optical_floor`'s "Two terms, not one" — a per-window-only check would
    // let the pre-#707 regression level through undetected). UNCHANGED computation from before
    // issue 915 (still summed, still compared against `RUN_UNDECODABLE_FLOOR` exactly as before).
    //
    // Issue 915 (2026-08-01) made this report-only; issue 905 item 3 (2026-09-04) RE-GATED it —
    // it FORCES `overall_pass` to fail again now that `crate::optical_floor::gates_overall_pass()`
    // is `true` (all physical blockers closed: cam1 grabber replaced, 120Hz/100Hz ruled out, 60Hz
    // baseline permanent; `RUN_UNDECODABLE_FLOOR` recalibrated 8 → 6). Re-disarm = flip that one
    // function back to `false`.
    let total_undecodable: u32 = segments.iter().map(|s| s.undecodable).sum();
    let run_wide_undecodable_within_floor =
        crate::optical_floor::run_within_floor(total_undecodable);
    overall_pass &=
        run_wide_undecodable_within_floor || !crate::optical_floor::gates_overall_pass();

    // Issue 889 visibility requirement 2 — how many windows would have FAILED under the pre-889
    // strict rule, regardless of whether the run-wide relaxed verdict passes. Always computed,
    // never gated on `overall_pass`'s own value.
    let windows_failed_report_only = segments.iter().filter(|s| !s.pass).count() as u32;

    // 2026-08-05 re-gate -- how many windows exceed the per-window tolerance on copies AND/OR
    // gaps (the windows that DO gate `overall_pass` again, per `seg.relaxed_pass` above). Computed
    // directly from the same counts `crate::window_gate::decide` already folded into
    // `relaxed_pass`, so this can never disagree with what actually gated the run.
    //
    // #1251: `copies_gaps_tolerance` (the run-wide field echoed into the JSON) is the DEFAULT/base
    // tolerance — kept back-compatible — but the OVER-tolerance count compares each window against
    // ITS OWN applied tolerance (`s.copies_gaps_tolerance`), so a CAM2 window within its 25 override
    // is correctly NOT counted while a default-5 box over 5 still is.
    let copies_gaps_tolerance = crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE;
    let windows_over_copies_gaps_tolerance = segments
        .iter()
        .filter(|s| s.copies > s.copies_gaps_tolerance || s.gaps > s.copies_gaps_tolerance)
        .count() as u32;

    // #1169 (owner, 2026-08-22) -- how many windows had their nonzero copies/gaps ABSORBED by the
    // `<=1/<=1` singleton allowance (windows that FAIL strict but PASS `overall_pass` under the
    // soft-release). Computed from the SAME pure seam `overall_pass_term` folded above, so this can
    // never disagree with what actually gated the run.
    // The `s.frames > 0` guard mirrors `overall_pass_term`'s own empty-window guard (defensive,
    // #1169 review) so an absent cambox can never be counted as a consumed singleton even if the
    // copies/gaps computation ever changes -- an empty window already yields 0/0 today.
    let windows_singleton_allowance_consumed = segments
        .iter()
        .filter(|s| {
            s.frames > 0
                && crate::window_gate::segment_singleton_allowance_consumed(s.copies, s.gaps)
        })
        .count() as u32;

    SegmentedContinuity {
        segments,
        overall_pass,
        windows_failed_report_only,
        total_undecodable,
        run_wide_undecodable_within_floor,
        copies_gaps_tolerance,
        windows_over_copies_gaps_tolerance,
        windows_singleton_allowance_consumed,
        guard_ns,
        residual_events: all_residual_events,
        expected_step,
        discarded_guard_frames,
        unplaceable_frames,
    }
}

/// The per-window painted-tick continuity. Reports three disjoint counts:
///
/// - `undecodable`: delivered frames whose painted tick did not decode (`tick == None`).
/// - `copies`: stale/frozen frames — the painted tick repeated the immediately-preceding
///   RECORDED present tick (a genuine repeated-image signal, order-dependent by design).
/// - `gaps`: real dropped painted frames — a tick value genuinely absent from the window's
///   observed set, beyond the by-design `expected_step` decimation (crediting up to `undecodable`
///   candidate slots). See [`crate::painted_tick_gaps::painted_tick_gaps`] (#625).
///
/// ## Why not a recorded-order walk, and not [`burn_contiguity_in_window_with_step`]
///
/// The painted tick is a per-painted-FRAME counter sampled at the cambox rate, NOT a free-running
/// render-tick counter, so `burn_contiguity_in_window_with_step` is the WRONG tool two ways: (1)
/// its `PerRenderTick` rate IGNORES forward gaps at step 1 (a render counter legitimately ticks
/// faster than frames), masking a real step-1 drop; (2) its `PerEmittedFrame` rate carries the
/// #226 "duplicate ⇒ BURN-UNREADABLE" reclassification — for a node burn a duplicate id means a
/// delivered-but-misdecoded frame (not a drop), but for the painted tick a duplicate is a
/// STALE/FROZEN copy and the tick missing behind a non-adjacent freeze is a REAL drop, which that
/// reclassification would silently clear (a FALSE PASS).
///
/// **#625 — `gaps` must ALSO be ORDER-INDEPENDENT.** A RECORDED-order walk (treating any backward
/// step as an unconditional fault, any oversized forward step as an excess) is exactly the class
/// of over-count `burn_contiguity_in_window_with_step` was hardened against for cam1's OWN burn
/// (#216/#356): the stream recording is documented (`#133`/`#196`/`#216`) to occasionally deliver
/// a frame "softened"/out of order (a one-frame-late 60→30 straddle) — "a reordered-but-present id
/// is just the softened recording delivering it late, never a fault." A recorded-order walk has no
/// such tolerance: ONE benign swap manufactures THREE phantom gaps for zero actual loss (proven
/// live — run 1783530925's `all_cambox_continuity` FAILED every ~30s window on `gaps` alone while
/// the SAME recording's `full_chain` proved `0 REAL DROP`). [`crate::painted_tick_gaps::
/// painted_tick_gaps`] walks the DISTINCT present values in SORTED order instead — the painter's
/// tick only ever increases at the SOURCE, so sorting recovers the true delivery-order-independent
/// sequence without masking a genuinely missing value (sorting cannot make an absent value
/// appear). `copies` stays a RECORDED-order, ADJACENT check — a genuinely repeated image is a real
/// signal regardless of surrounding order, so reordering can never manufacture or hide it.
fn window_segment(
    cambox: &str,
    start_ns: i64,
    end_ns: i64,
    frames: &[SegmentFrame],
    expected_step: i64,
) -> CamboxSegment {
    let frame_count = frames.len() as u32;
    let undecodable = frames.iter().filter(|f| f.tick.is_none()).count() as u32;

    // `copies`: the immediately-preceding RECORDED present tick repeated — a genuine
    // stale/frozen-content signal (the SAME image held across ≥2 physically consecutive
    // delivered frames), unaffected by delivery reordering.
    let mut copies: u32 = 0;
    let mut prev_recorded: Option<u32> = None;
    for f in frames {
        if let Some(t) = f.tick {
            if prev_recorded == Some(t) {
                copies = copies.saturating_add(1);
            }
            prev_recorded = Some(t);
        }
    }

    // `gaps` (#625): order-independent — see the function doc above.
    let present_ticks: Vec<u32> = frames.iter().filter_map(|f| f.tick).collect();
    let gaps =
        crate::painted_tick_gaps::painted_tick_gaps(&present_ticks, undecodable, expected_step);

    // #726: presentation-cadence EVENNESS — reuses the SAME `present_ticks` (RECORDED order,
    // unlike the sorted sequence `painted_tick_gaps` consumes above) + `expected_step` this
    // window already computed for the net-loss accounting; no extra decode work. `None` on a
    // window with fewer than 2 decoded ticks (incl. every non-cam2 window, whose `present_ticks`
    // is always empty).
    let presentation_cadence =
        crate::presentation_cadence::measure_cadence_evenness(&present_ticks, expected_step);

    // #707 EVENT-FORENSICS — the locatable per-event breakdown, computed from the SAME `frames`
    // (RECORDED order) this function already walked above. `residual_events` needs no `gen_ts_ns`
    // re-derivation: `SegmentFrame` and `crate::residual_events::TickSample` carry the identical
    // three fields, so this is a plain field-copy, not a re-decode.
    let tick_samples: Vec<crate::residual_events::TickSample> = frames
        .iter()
        .map(|f| crate::residual_events::TickSample {
            frame_index: f.frame_index,
            gen_ts_ns: f.gen_ts_ns,
            tick: f.tick,
        })
        .collect();
    let residual_events =
        crate::residual_events::residual_events(&tick_samples, start_ns, expected_step);
    // #883 — when the whole-window net-span `gaps` is non-zero but the walk above found NOTHING
    // to blame it on (every delta sat at/under the outlier ceiling, no backward jump), fall back
    // to locating the single largest recorded-order delta so a counted gap is never left
    // completely unlocatable. See `crate::residual_events::
    // locate_best_candidate_for_unattributed_gap` for the full rationale.
    let residual_events: Vec<crate::residual_events::ResidualEvent> =
        crate::residual_events::locate_best_candidate_for_unattributed_gap(
            &tick_samples,
            start_ns,
            gaps,
            residual_events,
        )
        .into_iter()
        .map(|mut e| {
            e.cambox = cambox.to_string();
            e
        })
        .collect();

    let first_tick = frames.iter().find_map(|f| f.tick);
    let last_tick = frames.iter().rev().find_map(|f| f.tick);

    // #881 — the per-window half of the calibrated optical-undecodable floor (permanent; a
    // physical 60Hz temporal-tear artifact of the test camera's monitor, not chain loss — see
    // `crate::optical_floor`). Issue 889 (2026-07-30 user decision on issue 883): `pass` keeps its
    // STRICT, UNCHANGED meaning (`copies == 0 && gaps == 0` still required); the RELAXED verdict
    // that actually feeds `overall_pass` is `relaxed_pass` below — see `crate::window_gate` for
    // the full decision record + restore path.
    // #1251: apply the per-cambox copies/gaps tolerance (default for every box EXCEPT one carrying
    // an override — CAM2 → 25 while its grabber HW is sick, issue 1249). The applied tolerance is
    // stored on the segment so `segment_continuity`'s fold + count and `recording-verdict.rs` all
    // judge this window against ITS OWN tolerance, never one run-wide value.
    let copies_gaps_tolerance = crate::window_gate::copies_gaps_tolerance_for_cambox(cambox);
    let gate = crate::window_gate::decide_with_tolerance(
        frame_count,
        undecodable,
        copies,
        gaps,
        copies_gaps_tolerance,
    );
    let pass = gate.strict_pass;
    let relaxed_pass = gate.relaxed_pass;
    // #333: a ZERO-frame window is empty by construction, not chain loss — flag it loudly so it is
    // not misread as a continuity break. The dominant cause is sweeping the dual-QR painter box
    // (it does not emit its own camera NDI while painting, #179) or a down / non-emitting box.
    let note = (frame_count == 0).then(|| {
        format!(
            "swept cambox {cambox} produced 0 frames in its window — is it the dual-QR painter / \
             not emitting NDI? Exclude it from CAMBOX_SWEEP."
        )
    });
    // #1169 (owner, 2026-08-22): the LOUD per-segment note when this window's nonzero copies/gaps
    // were ABSORBED by the `<=1/<=1` singleton allowance. `Some(..)` iff the allowance was consumed
    // (see `crate::window_gate::segment_singleton_note`) -- `pass` stays false for such a window, so
    // the absorption is never silent. `None` on a clean window and on an over-band window that still
    // fails (it fails loudly on its own). The `frame_count > 0` guard (defensive, #1169 review)
    // keeps an absent cambox noteless even if the copies/gaps computation ever changes.
    let singleton_allowance_note = if frame_count > 0 {
        crate::window_gate::segment_singleton_note(copies, gaps)
    } else {
        None
    };
    CamboxSegment {
        cambox: cambox.to_string(),
        start_ns,
        end_ns,
        frames: frame_count,
        undecodable,
        copies,
        gaps,
        copies_gaps_tolerance,
        first_tick,
        last_tick,
        pass,
        relaxed_pass,
        note,
        singleton_allowance_note,
        presentation_cadence,
        residual_events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a window's worth of clean frames: ticks stepping by `step`, gen_ts spaced by `dt`
    /// starting at `start_ns + dt` (so they sit strictly inside `[start_ns, end_ns)`).
    fn clean_frames(
        start_ns: i64,
        dt: i64,
        n: usize,
        step: u32,
        start_tick: u32,
    ) -> Vec<SegmentFrame> {
        (0..n)
            .map(|i| SegmentFrame {
                frame_index: i as u64,
                gen_ts_ns: start_ns + dt + (i as i64) * dt,
                tick: Some(start_tick + (i as u32) * step),
            })
            .collect()
    }

    fn win(cambox: &str, start_ns: i64, end_ns: i64) -> SwitchWindow {
        SwitchWindow {
            cambox: cambox.to_string(),
            start_ns,
            end_ns,
        }
    }

    /// #881 — build a window's worth of step-1 frames (gen_ts spaced by `dt`, strictly inside
    /// `[start_ns, end_ns)`) with `undecodable` of them (a single CONTIGUOUS block starting at
    /// the window's midpoint) carrying `tick: None` instead of their would-be value. The missing
    /// block is a genuine internal "hole" in the present-tick sequence, exactly credited by
    /// `painted_tick_gaps` against the `undecodable` count (see `crate::painted_tick_gaps`) — so
    /// this produces `gaps == 0` and `copies == 0` regardless of `undecodable`, isolating the
    /// optical `undecodable` term the #881 floor calibrates.
    fn window_frames_with_undecodable(
        start_ns: i64,
        dt: i64,
        n: usize,
        start_tick: u32,
        undecodable: usize,
    ) -> Vec<SegmentFrame> {
        let mid = n / 2;
        (0..n)
            .map(|i| SegmentFrame {
                frame_index: i as u64,
                gen_ts_ns: start_ns + dt + (i as i64) * dt,
                tick: if i >= mid && i < mid + undecodable {
                    None
                } else {
                    Some(start_tick + i as u32)
                },
            })
            .collect()
    }

    #[test]
    fn clean_multi_window_all_pass_overall_pass() {
        // Two camboxes, each ~clean step-1 sequence in its own window. ALL pass, overall PASS.
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 8, 1, 100); // gen_ts 100..800 in [0,1000)
        frames.extend(clean_frames(1000, 100, 8, 1, 500)); // gen_ts 1100..1800 in [1000,2000)
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(v.overall_pass, "clean multi-window ⇒ overall PASS: {v:?}");
        assert_eq!(v.segments.len(), 2);
        assert!(
            v.segments.iter().all(|s| s.pass),
            "every segment passes: {v:?}"
        );
        assert_eq!(v.segments[0].cambox, "cam1");
        assert_eq!(v.segments[0].frames, 8);
        assert_eq!(v.segments[0].undecodable, 0);
        assert_eq!(v.segments[0].copies, 0);
        assert_eq!(v.segments[0].gaps, 0);
        assert_eq!(v.segments[1].cambox, "cam2");
        assert_eq!(v.segments[1].frames, 8);
    }

    #[test]
    fn gap_of_six_exceeds_tolerance_fails_overall_pass_1243() {
        // cam1 clean; cam2 has a tick that skips by 7 (a real drop at step 1: 502..507 absent),
        // i.e. gaps=6. Issue 889 (2026-07-30 user decision on issue 883) originally made `gaps`
        // fully report-only here; the 2026-08-05 RE-GATE (ticket 889 comment 5196190653)
        // re-introduced a per-window tolerance (`crate::window_gate::
        // WINDOW_COPIES_GAPS_TOLERANCE`, recalibrated 1 -> 2 -> 3 on 2026-08-06, ticket 889
        // comments 5198131539 / 5200533407, walked 3 -> 5 on 2026-08-31 issue 1243) — gaps=6
        // EXCEEDS that tolerance, so this window (and therefore the run) now correctly FAILS
        // `overall_pass` again. Renamed from `gap_of_four_exceeds_tolerance_..._889_regate`
        // (itself renamed through `gap_of_three_...` / `gap_of_two_...` / `..._889_relaxes_
        // overall_pass`) — the literal gaps=6 sits comfortably over the walked-up tolerance (5),
        // still well over the 2026-08-14 3 -> 1 re-tightening too; the STRICT per-window `pass`
        // still catches it exactly as before (unchanged).
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 6, 1, 100);
        // cam2: 500,501,508,509 — 502,503,504,505,506,507 absent (a real gap), step 1.
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(501),
            },
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(508),
            },
            SegmentFrame {
                frame_index: 103,
                gen_ts_ns: 1400,
                tick: Some(509),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.segments[1].gaps, 6,
            "isolates a 6-gap window: {:?}",
            v.segments[1]
        );
        assert!(v.segments[0].pass, "cam1 still clean: {:?}", v.segments[0]);
        assert!(
            !v.segments[1].pass,
            "cam2's STRICT verdict still catches the gap (unchanged): {:?}",
            v.segments[1]
        );
        assert!(
            !v.segments[1].relaxed_pass,
            "889 re-gate: cam2's gaps=6 exceeds the tolerance -- relaxed must fail: {:?}",
            v.segments[1]
        );
        assert!(
            !v.overall_pass,
            "889 re-gate: an over-tolerance gap window must fail overall_pass again: {v:?}"
        );
        assert_eq!(v.segments[1].undecodable, 0);
        assert_eq!(v.segments[1].copies, 0);
        assert_eq!(
            v.windows_failed_report_only, 1,
            "889: exactly one window would have failed under the strict rule: {v:?}"
        );
        assert_eq!(
            v.windows_over_copies_gaps_tolerance, 1,
            "889 re-gate: exactly cam2's window exceeds the tolerance: {v:?}"
        );
    }

    #[test]
    fn single_undecodable_frame_within_calibrated_floor_passes_881() {
        // #881 calibrated floor: cam2 has ONE delivered frame with no painted tick (None) —
        // 1 undecodable is within the per-window floor (<=4) and the run-wide floor (<=8), and
        // copies/gaps are both 0, so this window (and the whole run) now PASSES. Before #881
        // this exact sequence FAILED on the optical `undecodable` term alone; it must not
        // anymore — the residual is a physical 60Hz temporal-tear artifact of the test camera's
        // monitor, not chain loss (issue 854 design).
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: None,
            },
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(502),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            v.overall_pass,
            "1 undecodable is within the #881 calibrated floor: {v:?}"
        );
        assert!(v.segments[0].pass);
        assert!(
            v.segments[1].pass,
            "1 undecodable, 0 copies, 0 gaps -> within floor -> PASS: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.segments[1].undecodable, 1,
            "exactly one undecodable: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.segments[1].gaps, 0,
            "the None is undecodable, not a gap (credited): {:?}",
            v.segments[1]
        );
    }

    #[test]
    fn single_window_five_undecodable_exceeds_per_window_floor_fails_overall_905() {
        // Acceptance criterion 3 (issue 854 design) for the STRICT verdict: 5 undecodable in ONE
        // window exceeds the #881 per-window floor (<=4) even though the window is otherwise
        // clean (copies=0, gaps=0). Issue 915 made the floor report-only (overall PASSED); issue
        // 905 item 3 (2026-09-04) RE-GATED it, so the per-window over-floor count now fails the
        // RELAXED verdict AND `overall_pass` too, not just STRICT.
        let schedule = vec![win("cam2", 0, 60_000)];
        let frames = window_frames_with_undecodable(0, 1000, 50, 1000, 5);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[0].undecodable, 5);
        assert_eq!(v.segments[0].copies, 0, "{:?}", v.segments[0]);
        assert_eq!(v.segments[0].gaps, 0, "{:?}", v.segments[0]);
        assert!(
            !v.segments[0].pass,
            "5 undecodable exceeds the per-window floor of 4 -- STRICT fails, unchanged: {:?}",
            v.segments[0]
        );
        assert!(
            !v.segments[0].relaxed_pass,
            "issue 905: the re-gated per-window floor now fails the relaxed verdict too: {:?}",
            v.segments[0]
        );
        assert!(
            !v.overall_pass,
            "issue 905: the optical floor gates again -- this run now FAILS overall: {v:?}"
        );
        assert_eq!(v.windows_failed_report_only, 1);
    }

    #[test]
    fn undecodable_within_floor_and_a_copy_present_now_passes_relaxed_but_still_fails_strict_889() {
        // The #881 floor governs ONLY the optical `undecodable` term. A copy in the SAME window
        // used to still fail the window (`copies == 0` / `gaps == 0` were "not relaxed, now or
        // ever" per issue 854's design) -- issue 889 (2026-07-30 user decision on issue 883)
        // supersedes exactly that framing for copies/gaps: the STRICT verdict is UNCHANGED
        // (still fails on the copy), but the RELAXED verdict that now feeds `overall_pass`
        // ignores it, since undecodable=1 is within the #881 floor on its own.
        // Issue 1220 (owner mandate, 2026-08-29): the single copy (within the re-armed <=3
        // tolerance channel) is ABSORBED into `overall_pass` via that channel -- the #1169
        // <=1/<=1 singleton band is now dormant (superseded by precedence), so the per-segment
        // note/run-level count it used to drive stay at their dormant (None/0) values; strict
        // `pass` still stays false/visible.
        let schedule = vec![win("cam2", 0, 10_000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(500),
            }, // copy
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: None,
            }, // 1 undecodable — within floor on its own
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(502),
            },
        ];
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[0].undecodable, 1);
        assert_eq!(v.segments[0].copies, 1);
        assert!(
            !v.segments[0].pass,
            "889: the STRICT verdict still fails on the copy, unchanged: {:?}",
            v.segments[0]
        );
        assert!(
            v.segments[0].relaxed_pass,
            "889: the RELAXED verdict passes -- copy is report-only, undecodable(1) within floor: {:?}",
            v.segments[0]
        );
        assert!(
            v.overall_pass,
            "issue 1220: the single copy is ABSORBED into overall_pass via the re-armed tolerance: {v:?}"
        );
        assert!(
            v.segments[0].singleton_allowance_note.is_none(),
            "issue 1220: the singleton mechanism is dormant -- the tolerance channel absorbed \
             this, not the singleton band: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "issue 1220: the singleton mechanism never fires while the tolerance is armed: {v:?}"
        );
        assert_eq!(
            v.windows_failed_report_only, 1,
            "issue 1220: the STRICT count still records the copy (report-only, visible): {v:?}"
        );
    }

    #[test]
    fn real_measured_run_1039420389_passes_with_calibrated_floor_881() {
        // Run 1039420389 (CI run 30521066155, dev @ d381b0aee) — the primary calibration source
        // for the #881 floor (issue 854 comment 5128509160). copies=0 and gaps=0 on EVERY
        // window; undecodable spread 0,0,0,0,0,1,0,0,0,2 across CAM1..CAM4 — 3 total / 8464
        // frames = 0.035%. Acceptance criterion 1 (issue 854 design).
        let frame_counts = [846u32, 846, 847, 847, 847, 847, 848, 846, 846, 844];
        let undecodable_counts = [0usize, 0, 0, 0, 0, 1, 0, 0, 0, 2];
        let camboxes = [
            "cam1", "cam2", "cam3", "cam4", "cam1", "cam2", "cam3", "cam4", "cam1", "cam2",
        ];
        let dt = 1000i64;
        let mut schedule = Vec::new();
        let mut frames = Vec::new();
        let mut cursor = 0i64;
        for (w, (&fc, &ud)) in frame_counts
            .iter()
            .zip(undecodable_counts.iter())
            .enumerate()
        {
            let n = fc as usize;
            let start = cursor;
            let end = start + (n as i64 + 2) * dt;
            schedule.push(win(camboxes[w], start, end));
            frames.extend(window_frames_with_undecodable(start, dt, n, 1000, ud));
            cursor = end;
        }
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.segments.iter().map(|s| s.undecodable).sum::<u32>(),
            3,
            "sanity: 3 undecodable total across the run: {v:?}"
        );
        assert!(
            v.segments.iter().all(|s| s.copies == 0 && s.gaps == 0),
            "every window clean on copies/gaps, matching the measured run: {v:?}"
        );
        assert!(
            v.overall_pass,
            "run 1039420389's exact numbers must PASS under the #881 calibrated floor: {v:?}"
        );
    }

    #[test]
    fn pre_707_regression_level_fails_overall_pass_again_905() {
        // THE most important test for issue 881's calibration (acceptance criterion 2, issue 854
        // design). EACH of 10 windows here carries exactly 1 undecodable frame (well within the
        // per-window floor of 4) and is individually clean on copies/gaps. The SUM across the
        // whole run is 10, the pre-#707 regression level (#707's own before/after: 10 -> 3
        // undecodable) — `run_wide_undecodable_within_floor` computes this as OVER the run-wide cap
        // (now 6). Issue 915 made the run report-only (it PASSED); issue 905 item 3 (2026-09-04)
        // RE-GATED the floor, so this run FAILS `overall_pass` again — the run-wide term catching
        // the pre-#707 regression is exactly what this gate exists to do.
        let n = 50;
        let dt = 1000i64;
        let window_span = (n as i64 + 2) * dt;
        let camboxes = ["cam1", "cam2", "cam3", "cam4"];
        let mut schedule = Vec::new();
        let mut frames = Vec::new();
        for w in 0..10 {
            let start = w as i64 * window_span;
            let end = start + window_span;
            schedule.push(win(camboxes[w % camboxes.len()], start, end));
            frames.extend(window_frames_with_undecodable(
                start,
                dt,
                n,
                1000 + (w as u32) * 1000,
                1,
            ));
        }
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.segments.iter().map(|s| s.undecodable).sum::<u32>(),
            10,
            "sanity: 10 windows x 1 undecodable each: {v:?}"
        );
        assert!(
            v.segments.iter().all(|s| s.copies == 0 && s.gaps == 0),
            "every window is clean on copies/gaps: {v:?}"
        );
        assert!(
            v.segments.iter().all(|s| s.undecodable <= 4),
            "every window individually is within the per-window floor: {v:?}"
        );
        assert_eq!(
            v.total_undecodable, 10,
            "#915: the run-wide sum stays correctly computed: {v:?}"
        );
        assert!(
            !v.run_wide_undecodable_within_floor,
            "the run-wide floor computation is UNCHANGED -- 10 > 6 reads as over-floor: {v:?}"
        );
        assert!(
            !v.overall_pass,
            "issue 905: the pre-#707 regression level (10 total) FAILS overall_pass again -- the \
             re-gated run-wide floor (6) catches it: {v:?}"
        );
    }

    #[test]
    fn copy_stale_frame_fails_strict_but_is_absorbed_by_the_1220_tolerance_channel() {
        // cam2 repeats a painted tick (500,500,501) → a stale/frozen copy → STRICT FAIL.
        // Renamed from `..._absorbed_by_the_1169_singleton_allowance_supersedes_1132`. #1132 made
        // a single copy FAIL overall_pass; #1169 refined that with a <=1/<=1 SINGLETON absorption;
        // #1220 (owner mandate, 2026-08-29) re-arms the WIDER already-calibrated <=3 tolerance
        // channel, which now absorbs this exact shape instead -- the singleton mechanism is
        // dormant (superseded by precedence), so its note/count read as if never consumed. strict
        // `pass` still stays false/visible; over the tolerance ceiling still fails, unchanged.
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // copy
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(501),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            v.overall_pass,
            "#1220: a single copy is now ABSORBED into overall_pass via the re-armed tolerance: {v:?}"
        );
        assert!(v.segments[0].pass);
        assert!(
            !v.segments[1].pass,
            "889: the STRICT verdict still catches the copy (visible): {:?}",
            v.segments[1]
        );
        assert!(
            v.segments[1].relaxed_pass,
            "the relaxed verdict still reports pass despite the copy (observability): {:?}",
            v.segments[1]
        );
        assert!(
            v.segments[1].singleton_allowance_note.is_none(),
            "#1220: the singleton mechanism is dormant -- the tolerance channel absorbed this: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "#1220: the singleton mechanism never fires while the tolerance channel is armed: {v:?}"
        );
        assert_eq!(
            v.segments[1].copies, 1,
            "889: still COMPUTED and printed, exactly one copy: {:?}",
            v.segments[1]
        );
        assert_eq!(v.segments[1].gaps, 0);
        assert_eq!(v.segments[1].undecodable, 0);
        assert_eq!(
            v.windows_failed_report_only, 1,
            "#1220: the STRICT count still records the copy (report-only, visible): {v:?}"
        );
    }

    #[test]
    fn non_adjacent_freeze_hiding_a_real_drop_still_fails_strict() {
        // REGRESSION (code-review finding): at expected_step=1 the per-node burn check's
        // PerEmittedFrame #226 logic would reclassify the dropped tick 102 as BURN-UNREADABLE
        // (a non-adjacent duplicate 100 sits in the gap) while a consecutive-only copy counter
        // misses that non-adjacent freeze → a FALSE PASS. The direct painted-tick walk must
        // STILL FAIL STRICT: ticks 100,101,100,103 carry a backward jump (the frozen 100 after
        // 101) AND a forward skip to 103 (102 dropped). Neither is silently cleared -- issue 889
        // (2026-07-30 user decision on issue 883) only changes whether this window's FAILURE
        // gates `overall_pass`; it does NOT touch whether `gaps` is correctly computed at all.
        // Issue 1220 (owner mandate, 2026-08-29) re-arms the calibrated <=3 tolerance channel:
        // the ONE counted drop is ABSORBED into `overall_pass` through it (the #1169 <=1/<=1
        // singleton band is dormant, superseded by precedence); strict `pass` stays false/visible.
        // The companion below (six drops, over the walked-up <=5 ceiling) still fails.
        let schedule = vec![win("cam1", 0, 10_000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(100),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(101),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(100),
            }, // non-adjacent freeze
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(103),
            }, // 102 dropped
        ];
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            !v.segments[0].pass,
            "a hidden drop behind a non-adjacent freeze must still fail STRICT: {v:?}"
        );
        assert!(
            v.segments[0].gaps >= 1,
            "the real drop is counted as a gap, not silently cleared: {:?}",
            v.segments[0]
        );
        // undecodable=0 and frame_count>0, so the RELAXED verdict (which absorbs gaps within
        // tolerance) still reports pass -- proving `gaps` is still computed correctly.
        assert!(
            v.segments[0].relaxed_pass,
            "relaxed verdict absorbs gaps within tolerance (reported): {v:?}"
        );
        assert!(
            v.segments[0].singleton_allowance_note.is_none(),
            "issue 1220: the singleton mechanism is dormant -- the tolerance channel absorbed \
             this drop, not the singleton band: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "issue 1220: the singleton mechanism never fires while the tolerance is armed: {v:?}"
        );
        assert_eq!(
            v.windows_failed_report_only, 1,
            "issue 1220: the STRICT count still records the hidden drop (report-only, visible): {v:?}"
        );
        assert!(
            v.overall_pass,
            "issue 1220: the single counted drop is ABSORBED into overall_pass via the re-armed \
             tolerance channel -- counted, never masked: {v:?}"
        );
        // Companion: the SAME freeze-hiding shape with SIX real drops (102, 104, 106, 108, 110
        // AND 112 missing) exceeds the walked-up (#1243, 2026-08-31) <=5 tolerance ceiling --
        // overall_pass still FAILS. Bumped from two (#1220 absorbs -- 2 <= 5) through four
        // (#1220-era, over the then-<=3 ceiling) to six so this stays a genuine over-ceiling
        // proof at the current tolerance.
        let frames_six_drops = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(100),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(101),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(100),
            }, // non-adjacent freeze
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(103),
            }, // 102 dropped
            SegmentFrame {
                frame_index: 4,
                gen_ts_ns: 500,
                tick: Some(105),
            }, // 104 dropped
            SegmentFrame {
                frame_index: 5,
                gen_ts_ns: 600,
                tick: Some(107),
            }, // 106 dropped
            SegmentFrame {
                frame_index: 6,
                gen_ts_ns: 700,
                tick: Some(109),
            }, // 108 dropped
            SegmentFrame {
                frame_index: 7,
                gen_ts_ns: 800,
                tick: Some(111),
            }, // 110 dropped
            SegmentFrame {
                frame_index: 8,
                gen_ts_ns: 900,
                tick: Some(113),
            }, // 112 dropped
        ];
        let v2 = segment_continuity(&frames_six_drops, &schedule, 0, 1);
        assert_eq!(
            v2.segments[0].gaps, 6,
            "all six real drops behind the freeze are counted, never masked: {:?}",
            v2.segments[0]
        );
        assert!(
            !v2.overall_pass,
            "issue 1243: six counted drops exceed the walked-up <=5 tolerance ceiling -- never \
             absorbed: {v2:?}"
        );
    }

    #[test]
    fn benign_delivery_reorder_does_not_fail_the_window_625() {
        // THE #625 regression (live evidence: run 1783530925 FAILED every ~30s all-cambox window
        // on `gaps` alone, even though the SAME recording's full_chain proved 0 REAL DROP). The
        // stream recording is documented (#133/#196/#216) to occasionally deliver a frame
        // "softened"/out of order — a one-frame-late 60→30 straddle. Here cam1's clean step-2
        // sequence 1000,1002,1004,1006,1008 is RECORDED with 1002/1004 swapped (a benign reorder,
        // zero real loss); a recorded-order walk would see 1000→1004 (oversized forward), 1004→
        // 1002 (backward), 1002→1006 (oversized forward) = 3 phantom gaps. The fix must report 0.
        let schedule = vec![win("cam1", 0, 10_000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(1000),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(1004),
            }, // recorded out of order — arrived before 1002
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(1002),
            },
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(1006),
            },
            SegmentFrame {
                frame_index: 4,
                gen_ts_ns: 500,
                tick: Some(1008),
            },
        ];
        let v = segment_continuity(&frames, &schedule, 0, 2);
        assert!(
            v.overall_pass,
            "a benign delivery reorder with zero real loss must PASS: {v:?}"
        );
        assert_eq!(
            v.segments[0].gaps, 0,
            "reordering must not manufacture a phantom gap: {:?}",
            v.segments[0]
        );
        assert_eq!(v.segments[0].undecodable, 0);
    }

    #[test]
    fn benign_delivery_reorder_gap_is_counted_and_absorbed_by_the_1220_tolerance_channel_625() {
        // The reorder-tolerance fix must never MASK a genuine drop either: 1004 is truly missing
        // (never delivered) on top of the same 1002/1006-adjacent reorder pattern. Issue 889
        // (2026-07-30 user decision on issue 883): `gaps` is report-only for `overall_pass` now,
        // but it must still be COMPUTED correctly -- the STRICT per-window `pass` still fails.
        // Issue 1220 (owner mandate, 2026-08-29): the re-armed <=3 tolerance channel now ABSORBS
        // this single COUNTED gap into `overall_pass` -- the #1169 <=1/<=1 singleton band is
        // dormant (superseded by precedence), so its note/count read as never consumed; strict
        // `pass` stays false/visible. The gap is counted, never masked; six missing ticks (the
        // sibling test below) still fail past the tolerance ceiling.
        let schedule = vec![win("cam1", 0, 10_000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(1000),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(1006),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(1002),
            },
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(1008),
            },
        ];
        let v = segment_continuity(&frames, &schedule, 0, 2);
        assert!(
            !v.segments[0].pass,
            "the genuinely-missing 1004 must still fail STRICT (visible): {v:?}"
        );
        assert_eq!(
            v.segments[0].gaps, 1,
            "exactly the one genuinely-missing tick, reorder or not: {:?}",
            v.segments[0]
        );
        // undecodable=0 here, so the relaxed verdict (which absorbs gaps within tolerance) still
        // reports pass -- proving the gap is still correctly located/counted.
        assert!(v.segments[0].relaxed_pass);
        assert!(
            v.segments[0].singleton_allowance_note.is_none(),
            "issue 1220: the singleton mechanism is dormant -- the tolerance channel absorbed \
             this gap, not the singleton band: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "issue 1220: the singleton mechanism never fires while the tolerance is armed: {v:?}"
        );
        assert_eq!(
            v.windows_failed_report_only, 1,
            "issue 1220: the STRICT count still records the gap (report-only, visible): {v:?}"
        );
        assert!(
            v.overall_pass,
            "issue 1220: a single counted gap is ABSORBED into overall_pass via the re-armed \
             tolerance channel -- counted, never masked: {v:?}"
        );
    }

    #[test]
    fn benign_delivery_reorder_six_missing_ticks_still_fail_625() {
        // Renamed from `..._four_missing_ticks_still_fail_625` (itself renamed from
        // `..._two_missing_ticks_still_fail_625`): #1220's <=3 tolerance absorbed two, so the
        // fixture was bumped to four; #1243 (2026-08-31) walked the tolerance 3 -> 5, so four is
        // no longer over the ceiling -- bumped again to SIX genuinely-missing ticks (1004, 1010,
        // 1014, 1018, 1022, 1026) to keep proving the never-mask guarantee ABOVE the walked-up
        // ceiling: present distinct ticks {1000,1002,1006,1008,1012,1016,1020,1024,1028} span the
        // step-2 range 1000..1028 (15 expected values), so exactly 6 are missing. Six counted
        // gaps exceed the <=5 tolerance, so the segment AND overall_pass both still FAIL.
        let schedule = vec![win("cam1", 0, 10_000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(1000),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(1006),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(1002),
            },
            SegmentFrame {
                frame_index: 3,
                gen_ts_ns: 400,
                tick: Some(1008),
            },
            SegmentFrame {
                frame_index: 4,
                gen_ts_ns: 500,
                tick: Some(1012),
            },
            SegmentFrame {
                frame_index: 5,
                gen_ts_ns: 600,
                tick: Some(1016),
            },
            SegmentFrame {
                frame_index: 6,
                gen_ts_ns: 700,
                tick: Some(1020),
            },
            SegmentFrame {
                frame_index: 7,
                gen_ts_ns: 800,
                tick: Some(1024),
            },
            SegmentFrame {
                frame_index: 8,
                gen_ts_ns: 900,
                tick: Some(1028),
            },
        ];
        let v = segment_continuity(&frames, &schedule, 0, 2);
        assert_eq!(
            v.segments[0].gaps, 6,
            "all six genuinely-missing ticks are counted, reorder or not: {:?}",
            v.segments[0]
        );
        assert!(
            !v.segments[0].pass,
            "six missing ticks must still fail STRICT: {v:?}"
        );
        assert!(
            !v.overall_pass,
            "issue 1243: six counted gaps exceed the walked-up <=5 tolerance -- never absorbed: {v:?}"
        );
    }

    #[test]
    fn frame_inside_transition_guard_is_excluded_not_counted_as_loss() {
        // A 2s guard. cam2's window [1000ms-domain]; a BAD (undecodable) frame sits within the
        // guard of the leading boundary → EXCLUDED → cam2 still PASSes; the bad frame is counted
        // in discarded_guard_frames, never as undecodable.
        let guard = 200; // ns guard for this synthetic ns scale
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        // cam1 clean, fully outside guards.
        let mut frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 400,
                tick: Some(100),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 500,
                tick: Some(101),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 600,
                tick: Some(102),
            },
        ];
        // cam2: one BAD undecodable frame at gen_ts 1100 (within [1000,1000+200) guard) → excluded.
        // Two clean frames at 1300,1500 (inside [1200,1800) post-guard core).
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: None,
            }, // in lead guard → excluded
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1300,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1500,
                tick: Some(501),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, guard, 1);
        assert!(
            v.overall_pass,
            "guard-excluded bad frame must not fail: {v:?}"
        );
        assert!(
            v.segments[1].pass,
            "cam2 passes (bad frame excluded): {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.segments[1].frames, 2,
            "only the 2 post-guard frames counted: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.segments[1].undecodable, 0,
            "the guard frame is NOT counted as undecodable"
        );
        assert_eq!(
            v.discarded_guard_frames, 1,
            "the bad frame is counted as a guard discard: {v:?}"
        );
    }

    #[test]
    fn trailing_guard_also_excludes() {
        // A frame within the guard of the TRAILING boundary is excluded too.
        let guard = 200;
        let schedule = vec![win("cam1", 0, 1000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 400,
                tick: Some(10),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 600,
                tick: Some(11),
            },
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 900,
                tick: None,
            }, // within [800,1000) trail guard
        ];
        let v = segment_continuity(&frames, &schedule, guard, 1);
        assert!(
            v.segments[0].pass,
            "trailing-guard bad frame excluded: {:?}",
            v.segments[0]
        );
        assert_eq!(v.segments[0].frames, 2);
        assert_eq!(v.discarded_guard_frames, 1);
    }

    #[test]
    fn absent_cambox_zero_frames_fails_coverage_honesty() {
        // cam2's window has NO frames (the box was down). It must FAIL — an absent cambox proves
        // nothing and must never read as a pass (#312 coverage honesty / #301 CAM3 down).
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let frames = clean_frames(0, 100, 4, 1, 100); // only cam1 frames
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(!v.overall_pass, "an absent cambox ⇒ overall FAIL: {v:?}");
        assert!(v.segments[0].pass);
        assert!(
            !v.segments[1].pass,
            "zero-frame window is NOT a pass: {:?}",
            v.segments[1]
        );
        assert_eq!(v.segments[1].frames, 0);
    }

    #[test]
    fn unplaceable_frames_are_reported_not_attributed() {
        // A frame whose gen_ts falls in NO window is unplaceable — counted, not charged to a cambox.
        let schedule = vec![win("cam1", 0, 1000)];
        let frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 400,
                tick: Some(10),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 5000,
                tick: None,
            }, // outside [0,1000)
        ];
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.unplaceable_frames, 1,
            "the out-of-window frame is unplaceable: {v:?}"
        );
        assert_eq!(
            v.segments[0].frames, 1,
            "only the in-window frame is attributed"
        );
        assert_eq!(
            v.segments[0].undecodable, 0,
            "the unplaceable None is NOT charged to cam1"
        );
    }

    #[test]
    fn expected_step_2_clean_decimation_is_pass_no_false_gap() {
        // The 60→30 stream recording: the painted tick steps by 2 by design. With expected_step=2
        // this is CLEAN — no false gap (the rate-agnostic step parameter is honored).
        let schedule = vec![win("cam1", 0, 100_000)];
        let frames = clean_frames(0, 1000, 10, 2, 1000); // ticks 1000,1002,...
        let v = segment_continuity(&frames, &schedule, 0, 2);
        assert!(
            v.overall_pass,
            "step-2 decimation is clean at expected_step=2: {v:?}"
        );
        assert_eq!(v.segments[0].gaps, 0);
    }

    #[test]
    fn step_2_data_with_expected_step_1_flags_gaps() {
        // The SAME step-2 data judged at expected_step=1 (no decimation expected) flags gaps —
        // proving the step parameter actually drives the check (a mutant fixing step→1 fails).
        // Issue 889 (2026-07-30 user decision on issue 883) originally made gaps fully
        // report-only. The 2026-08-05 RE-GATE (ticket 889 comment 5196190653, recalibrated
        // 1 -> 2 -> 3 on 2026-08-06) re-introduced a per-window tolerance
        // (`crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE`) — this fixture's gaps (9, far over
        // the tolerance either way) now correctly fails `overall_pass` again, exactly like the
        // STRICT per-window `pass` already did.
        let schedule = vec![win("cam1", 0, 100_000)];
        let frames = clean_frames(0, 1000, 10, 2, 1000);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            !v.segments[0].pass,
            "step-2 data at expected_step=1 ⇒ gaps ⇒ STRICT fail: {v:?}"
        );
        assert!(v.segments[0].gaps >= 1, "gaps flagged: {:?}", v.segments[0]);
        assert!(
            !v.segments[0].relaxed_pass,
            "889 re-gate: gaps=9 far exceeds the tolerance -- relaxed must fail: {v:?}"
        );
        assert!(
            !v.overall_pass,
            "889 re-gate: gaps far over tolerance must fail overall_pass again: {v:?}"
        );
    }

    #[test]
    fn validate_rejects_overlapping_windows() {
        let s = vec![win("cam1", 0, 1500), win("cam2", 1000, 2000)]; // overlap (1000 < 1500)
        assert!(validate_schedule(&s).is_err(), "overlap must be rejected");
    }

    #[test]
    fn validate_rejects_unordered_windows() {
        let s = vec![win("cam2", 1000, 2000), win("cam1", 0, 1000)]; // descending
        assert!(
            validate_schedule(&s).is_err(),
            "out-of-order must be rejected"
        );
    }

    #[test]
    fn validate_rejects_start_ge_end() {
        assert!(
            validate_schedule(&[win("cam1", 1000, 1000)]).is_err(),
            "start==end rejected"
        );
        assert!(
            validate_schedule(&[win("cam1", 2000, 1000)]).is_err(),
            "start>end rejected"
        );
    }

    #[test]
    fn validate_rejects_empty_schedule_and_empty_label() {
        assert!(validate_schedule(&[]).is_err(), "empty schedule rejected");
        assert!(
            validate_schedule(&[win("  ", 0, 1000)]).is_err(),
            "blank label rejected"
        );
    }

    #[test]
    fn validate_allows_touching_boundaries() {
        let s = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)]; // touch at 1000 — OK
        assert!(
            validate_schedule(&s).is_ok(),
            "touching boundaries are allowed"
        );
    }

    #[test]
    fn parse_switch_schedule_roundtrips_valid_json() {
        let json = r#"[
            {"cambox":"cam1","start_ns":0,"end_ns":30000000000},
            {"cambox":"cam2","start_ns":30000000000,"end_ns":60000000000}
        ]"#;
        let s = parse_switch_schedule(json).expect("valid schedule parses");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].cambox, "cam1");
        assert_eq!(s[1].start_ns, 30_000_000_000);
        assert_eq!(s[1].end_ns, 60_000_000_000);
    }

    #[test]
    fn parse_switch_schedule_rejects_invalid_json_and_invalid_schedule() {
        assert!(
            parse_switch_schedule("not json").is_err(),
            "garbage rejected"
        );
        // Well-formed JSON but an overlapping schedule is rejected by validation.
        let overlap = r#"[
            {"cambox":"cam1","start_ns":0,"end_ns":100},
            {"cambox":"cam2","start_ns":50,"end_ns":200}
        ]"#;
        assert!(
            parse_switch_schedule(overlap).is_err(),
            "overlap rejected at parse"
        );
    }

    #[test]
    fn default_transition_guard_is_one_second() {
        assert_eq!(DEFAULT_TRANSITION_GUARD_NS, 1_000_000_000);
    }

    #[test]
    fn guard_and_step_floors_are_applied() {
        // Negative guard floors to 0; a 0/negative step floors to 1 (echoed in the verdict).
        let schedule = vec![win("cam1", 0, 1000)];
        let frames = clean_frames(0, 100, 3, 1, 1);
        let v = segment_continuity(&frames, &schedule, -5, 0);
        assert_eq!(v.guard_ns, 0, "negative guard floored to 0");
        assert_eq!(v.expected_step, 1, "zero step floored to 1");
        assert!(v.segments[0].pass);
    }

    // ---- #726: presentation_cadence wiring ------------------------------------------------

    #[test]
    fn cam2_window_with_uniform_step_reports_perfect_evenness() {
        // A clean cam2 window at the real 60fps->30fps decimation ratio (step 2): every recorded
        // frame's painted tick advances by exactly 2 -> the "smooth 30" reference shape.
        let schedule = vec![win("cam2", 0, 10_000)];
        let frames = clean_frames(0, 100, 20, 2, 1000); // ticks 1000,1002,...,1038
        let v = segment_continuity(&frames, &schedule, 0, 2);
        let seg = &v.segments[0];
        assert!(seg.pass, "clean uniform sequence must still pass: {seg:?}");
        let pc = seg
            .presentation_cadence
            .as_ref()
            .expect("cam2 window with >=2 decoded ticks must report cadence evenness");
        assert_eq!(pc.evenness_score, 1.0);
        assert_eq!(pc.duplicate_steps, 0);
        assert_eq!(pc.paired_events, 0);
        assert_eq!(pc.expected_step, 2);
    }

    #[test]
    fn cam2_window_with_paired_judder_reports_low_evenness() {
        // The "15fps-like" signature: every source frame held for two recorded frames then a
        // compensating double jump. copies>0 already fails `pass` via the existing gate (a
        // duplicate IS a stale/frozen-frame fault) -- presentation_cadence additionally reports
        // the PATTERN (paired, not scattered) and a fractional severity score.
        let schedule = vec![win("cam2", 0, 10_000)];
        let mut frames = Vec::new();
        for k in 0..10i64 {
            let t = 1000 + (k as u32) * 4;
            frames.push(SegmentFrame {
                frame_index: (k * 2) as u64,
                gen_ts_ns: 100 + k * 200,
                tick: Some(t),
            });
            frames.push(SegmentFrame {
                frame_index: (k * 2 + 1) as u64,
                gen_ts_ns: 100 + k * 200 + 100,
                tick: Some(t), // held -- same tick presented twice
            });
        }
        let v = segment_continuity(&frames, &schedule, 0, 2);
        let seg = &v.segments[0];
        assert!(
            seg.copies > 0,
            "the existing copies gate must already flag the held frames: {seg:?}"
        );
        let pc = seg
            .presentation_cadence
            .as_ref()
            .expect("cam2 window with >=2 decoded ticks must report cadence evenness");
        assert_eq!(
            pc.uniform_steps, 0,
            "no delta is ever the on-cadence step 2"
        );
        assert!(
            pc.paired_events > 0,
            "must detect the paired duplicate+catchup shape: {pc:?}"
        );
        assert!(pc.evenness_score < 1.0);
    }

    #[test]
    fn non_cam2_window_with_no_painted_tick_reports_no_cadence() {
        // A swept non-cam2 cambox: frames are present (frame_count > 0) but every tick is None --
        // presentation_cadence must be None (nothing to classify), never a spurious 0.0.
        let schedule = vec![win("cam1", 0, 1000)];
        let frames: Vec<SegmentFrame> = (0..5usize)
            .map(|i| SegmentFrame {
                frame_index: i as u64,
                gen_ts_ns: 100 + (i as i64) * 100,
                tick: None,
            })
            .collect();
        let v = segment_continuity(&frames, &schedule, 0, 2);
        let seg = &v.segments[0];
        assert_eq!(seg.frames, 5);
        assert!(
            seg.presentation_cadence.is_none(),
            "a window with zero decoded ticks must report no cadence verdict: {seg:?}"
        );
    }

    // ---- #707 EVENT-FORENSICS: residual_events wiring ------------------------------------

    #[test]
    fn copy_event_is_reported_with_the_owning_cambox_tag() {
        // The SAME duplicate-tick scenario as `copy_stale_frame_in_one_window_fails_that_cambox`
        // above, now asserting the located event carries the right cambox label + frame data.
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // copy
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(501),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.segments[0].residual_events.len(),
            0,
            "cam1 window is clean"
        );
        let cam2_events = &v.segments[1].residual_events;
        assert_eq!(cam2_events.len(), 1, "events: {cam2_events:?}");
        assert_eq!(
            cam2_events[0].kind,
            crate::residual_events::ResidualEventKind::Copy
        );
        assert_eq!(cam2_events[0].cambox, "cam2");
        assert_eq!(cam2_events[0].frame_index, 101);
        assert_eq!(cam2_events[0].gen_ts_ns, 1200);
        assert_eq!(
            cam2_events[0].window_offset_ns, 200,
            "1200 - the window's own start_ns (1000)"
        );
    }

    #[test]
    fn segmented_continuity_residual_events_flattens_across_windows_in_schedule_order() {
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(10),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(10),
            }, // cam1 copy
        ];
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // cam2 copy
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(
            v.residual_events.len(),
            2,
            "one event per window, flattened: {:?}",
            v.residual_events
        );
        assert_eq!(v.residual_events[0].cambox, "cam1");
        assert_eq!(v.residual_events[1].cambox, "cam2");
    }

    #[test]
    fn clean_window_reports_no_residual_events() {
        let schedule = vec![win("cam1", 0, 1000)];
        let frames = clean_frames(0, 100, 8, 1, 100);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            v.segments[0].residual_events.is_empty(),
            "{:?}",
            v.segments[0].residual_events
        );
        assert!(v.residual_events.is_empty());
    }

    #[test]
    fn residual_gap_events_can_be_nonzero_while_authoritative_gaps_stays_zero_852() {
        // #852 — reproduces run 1867252327's exact puzzle: `all_cambox_continuity` reported
        // EVERY segment's authoritative `gaps` field as 0, yet `residual_events` reported 388
        // Gap-kind events fleet-wide. Investigation (issue comment) found this is NOT a
        // contradiction: every one of the 388 real events had a recorded-order delta in [11,53]
        // (`missing_slots` 4-25) -- moderate jumps fully explained by that run's own ~64%
        // fleet-wide `undecodable` rate. `gaps` (order-independent, credits `undecodable` as
        // plausible slot-fillers -- #625/#681) correctly finds no PROVEN loss; `residual_events`
        // (deliberately UNCREDITED recorded-order forensics -- see its own module doc) correctly
        // flags each moderate jump as a locatable candidate anyway, because its outlier
        // threshold (`GAP_OUTLIER_ABS_DELTA=10`) was calibrated on the #707 anatomy investigation's
        // CLEAN sample recordings, where routine catch-up deltas never exceeded 8. Neither
        // metric is wrong; they intentionally diverge under high `undecodable`. This test locks
        // that divergence so a future change never wires `residual_events` into `pass`, and
        // never "corrects" `gaps` to match it (which would silently break the already-proven
        // #625/#681 credit logic).
        let schedule = vec![win("cam1", 0, 100_000)];
        let mut frames: Vec<SegmentFrame> = Vec::new();
        let mut idx: u64 = 0;
        let mut ts: i64 = 0;
        // 9 present ticks: two delta=14 transitions (beyond the |Δ|>10 outlier ceiling), the
        // rest routine step-2 advances -- mirrors the real run's own delta shape (median 15).
        for t in [1000u32, 1002, 1004, 1018, 1020, 1022, 1036, 1038, 1040] {
            ts += 100;
            frames.push(SegmentFrame {
                frame_index: idx,
                gen_ts_ns: ts,
                tick: Some(t),
            });
            idx += 1;
        }
        // 15 undecodable frames -- ample credit for the whole-window net-span deficit
        // ((1040-1000)/2 + 1 = 21 expected - 9 present = 12), so `gaps` nets to 0.
        for _ in 0..15 {
            ts += 100;
            frames.push(SegmentFrame {
                frame_index: idx,
                gen_ts_ns: ts,
                tick: None,
            });
            idx += 1;
        }
        let v = segment_continuity(&frames, &schedule, 0, 2);
        let seg = &v.segments[0];
        assert_eq!(seg.undecodable, 15);
        assert_eq!(
            seg.gaps, 0,
            "whole-window net span (12) is fully credited by 15 undecodable slots: {seg:?}"
        );
        let gap_events: Vec<_> = seg
            .residual_events
            .iter()
            .filter(|e| e.kind == crate::residual_events::ResidualEventKind::Gap)
            .collect();
        assert_eq!(
            gap_events.len(),
            2,
            "both delta=14 transitions exceed GAP_OUTLIER_ABS_DELTA(10) as UNCREDITED \
             recorded-order forensics, even though the credited `gaps` field is 0: {gap_events:?}"
        );
        for e in &gap_events {
            assert_eq!(
                e.missing_slots,
                Some(6),
                "delta 14 at expected_step 2 -> (14/2)-1 = 6 missing slots: {e:?}"
            );
        }
        // The segment still correctly FAILS: 15 undecodable is FAR beyond even #881's calibrated
        // per-window floor (<=4 -- a temporary allowance for a physical 60Hz temporal-tear
        // artifact, not a licence for a genuinely low-confidence window like this one) -- an
        // independent, deliberate `pass` criterion (real confidence in zero-loss, not just "no
        // PROVEN hole") -- `gaps==0` alone is necessary but not sufficient. This is NOT a false
        // negative.
        assert!(
            !seg.pass,
            "undecodable(15) far exceeds the #881 per-window floor, still fails pass, honestly \
             reflecting low decode confidence: {seg:?}"
        );
    }

    #[test]
    fn diffuse_gap_with_no_outlier_delta_is_still_located_end_to_end_883() {
        // #883 -- CAM1 window 9 of run 1412981627: gaps=2 with zero located events from the base
        // walk (delta histogram {1:11, 2:840, 3:9, 8:1}, max delta 8, under the outlier ceiling
        // -- residual_events reported [] fleet-wide for this run). This mirrors that shape
        // end-to-end through `segment_continuity` (a lone delta=6 catch-up amid clean step=2
        // sampling, undecodable=0, copies=0) -- a counted gap must never be left unlocatable.
        let schedule = vec![win("cam1", 0, 1_000_000)];
        let present = [1000u32, 1002, 1004, 1010, 1012, 1014];
        let frames: Vec<SegmentFrame> = present
            .iter()
            .enumerate()
            .map(|(i, &t)| SegmentFrame {
                frame_index: i as u64,
                gen_ts_ns: 100_000 + (i as i64) * 100_000,
                tick: Some(t),
            })
            .collect();
        let v = segment_continuity(&frames, &schedule, 0, 2);
        let seg = &v.segments[0];
        assert_eq!(seg.copies, 0, "{seg:?}");
        assert_eq!(seg.gaps, 2, "{seg:?}");
        assert_eq!(
            seg.residual_events.len(),
            1,
            "issue 883: a counted gap must never be left with zero located events: {seg:?}"
        );
        assert_eq!(
            seg.residual_events[0].kind,
            crate::residual_events::ResidualEventKind::Gap
        );
        assert_eq!(seg.residual_events[0].cambox, "cam1");
        assert_eq!(seg.residual_events[0].missing_slots, Some(2));
        // The run-level flattened list must carry it too (#707 wiring, unaffected by #883).
        assert_eq!(v.residual_events.len(), 1, "{:?}", v.residual_events);
    }

    // ---- issue 889 (2026-07-30 user decision on issue 883): copies/gaps become report-only ----

    #[test]
    fn windows_failed_report_only_counts_strict_failures_across_a_mixed_run_889() {
        // 3 windows: cam1 clean, cam2 has a copy only (fails strict), cam3 clean. Issue 1220
        // (owner mandate, 2026-08-29) re-arms the <=3 tolerance channel: cam2's single copy is
        // ABSORBED into `overall_pass` (true) through it, not the (now-dormant) #1169 <=1/<=1
        // singleton band; `windows_failed_report_only` still honestly counts the one window that
        // fails strict -- the counter counts STRICT failures even when absorbed.
        let schedule = vec![
            win("cam1", 0, 1000),
            win("cam2", 1000, 2000),
            win("cam3", 2000, 3000),
        ];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // cam2 copy
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(501),
            },
        ]);
        frames.extend(clean_frames(2000, 100, 4, 1, 900));
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            v.overall_pass,
            "issue 1220: cam2's single copy is ABSORBED into overall_pass via the re-armed \
             tolerance channel: {v:?}"
        );
        assert!(v.segments[0].pass, "cam1 clean");
        assert!(!v.segments[1].pass, "cam2 has the copy -> STRICT fail");
        assert!(v.segments[2].pass, "cam3 clean");
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "issue 1220: the singleton mechanism never fires while the tolerance is armed: {v:?}"
        );
        assert_eq!(
            v.windows_failed_report_only, 1,
            "exactly cam2's window would have failed under the strict rule: {v:?}"
        );
    }

    #[test]
    fn undecodable_over_per_window_floor_fails_overall_via_905_regate_1132() {
        // #1132 (owner mandate 2026-08-19) + issue 1220 (2026-08-29): the single copy is ABSORBED
        // into `overall_pass` via the re-armed <=3 tolerance channel (the #1169 <=1/<=1 singleton
        // band is dormant). The UNDECODABLE-over-floor term is on its OWN seam: issue 915 made it
        // report-only, issue 905 item 3 (2026-09-04) RE-GATED it. So this window's per-window count
        // (5 > 4) now fails the RELAXED verdict AND `overall_pass` -- via the FLOOR seam, not the
        // copy (which is still absorbed). The run-wide sum (5) is within the run-wide floor (6), so
        // the failure is the PER-WINDOW floor. STRICT still fails for BOTH reasons (visible).
        // 1000,1000(copy),1001, then 5x None (undecodable -- over the per-window floor of 4), then 1002.
        let schedule = vec![win("cam2", 0, 10_000)];
        let mut frames = vec![
            SegmentFrame {
                frame_index: 0,
                gen_ts_ns: 100,
                tick: Some(1000),
            },
            SegmentFrame {
                frame_index: 1,
                gen_ts_ns: 200,
                tick: Some(1000),
            }, // copy
            SegmentFrame {
                frame_index: 2,
                gen_ts_ns: 300,
                tick: Some(1001),
            },
        ];
        for i in 0..5u64 {
            frames.push(SegmentFrame {
                frame_index: 3 + i,
                gen_ts_ns: 400 + (i as i64) * 100,
                tick: None,
            });
        }
        frames.push(SegmentFrame {
            frame_index: 8,
            gen_ts_ns: 900,
            tick: Some(1002),
        });
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[0].undecodable, 5, "{:?}", v.segments[0]);
        assert_eq!(v.segments[0].copies, 1, "{:?}", v.segments[0]);
        assert!(!v.segments[0].pass, "strict fails: {:?}", v.segments[0]);
        assert!(
            !v.segments[0].relaxed_pass,
            "issue 905: the re-gated optical floor (undecodable=5 > 4) now fails the relaxed verdict: {:?}",
            v.segments[0]
        );
        assert!(
            v.run_wide_undecodable_within_floor,
            "sanity: the run-wide sum (5) is within the run-wide floor (6) -- the failure is the \
             PER-WINDOW floor, not the run-wide one: {v:?}"
        );
        assert!(
            !v.overall_pass,
            "issue 905: the copy is ABSORBED via the re-armed tolerance channel, but the re-gated \
             per-window optical floor (5 > 4) now fails overall: {v:?}"
        );
        assert!(
            v.segments[0].singleton_allowance_note.is_none(),
            "issue 1220: the singleton mechanism is dormant -- the tolerance channel absorbed \
             this copy: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "issue 1220: the singleton mechanism never fires while the tolerance is armed: {v:?}"
        );
        assert_eq!(v.windows_failed_report_only, 1);
    }

    // ---- issue 889 re-gate (2026-08-05 ROZHODNUTÉ, recalibrated 2026-08-06): copies/gaps
    // tolerance ----

    #[test]
    fn windows_over_copies_gaps_tolerance_889_regate() {
        // 3 windows: cam1 clean, cam2 has 6 copies (OVER the tolerance, walked 3 -> 5 on
        // 2026-08-31, issue 1243, walk-back tracked on issue 1242 -- must gate overall_pass
        // again), cam3 clean. The literal copies=6 stays genuinely over-tolerance across every
        // recalibration incl. issue 1031's 3 -> 1 re-tightening (2026-08-14) and the 2026-08-31
        // 3 -> 5 walk-up (issue 1243) -- 6 is well over 1 and just over 5.
        let schedule = vec![
            win("cam1", 0, 1000),
            win("cam2", 1000, 3000),
            win("cam3", 3000, 4000),
        ];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // cam2 copy #1 -- under the tolerance
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(500),
            }, // cam2 copy #2 -- under the tolerance
            SegmentFrame {
                frame_index: 103,
                gen_ts_ns: 1400,
                tick: Some(500),
            }, // cam2 copy #3 -- under the tolerance
            SegmentFrame {
                frame_index: 104,
                gen_ts_ns: 1500,
                tick: Some(500),
            }, // cam2 copy #4 -- under the tolerance
            SegmentFrame {
                frame_index: 105,
                gen_ts_ns: 1600,
                tick: Some(500),
            }, // cam2 copy #5 -- AT the tolerance
            SegmentFrame {
                frame_index: 106,
                gen_ts_ns: 1700,
                tick: Some(500),
            }, // cam2 copy #6 -- over the tolerance
            SegmentFrame {
                frame_index: 107,
                gen_ts_ns: 1800,
                tick: Some(501),
            },
        ]);
        frames.extend(clean_frames(3000, 100, 4, 1, 900));
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[1].copies, 6, "{:?}", v.segments[1]);
        assert_eq!(
            v.segments[1].gaps, 0,
            "isolates copies alone: {:?}",
            v.segments[1]
        );
        assert!(
            !v.segments[1].pass,
            "strict still fails on the copies: {:?}",
            v.segments[1]
        );
        assert!(
            !v.segments[1].relaxed_pass,
            "889 re-gate: 6 copies exceeds the tolerance -- relaxed must fail too: {:?}",
            v.segments[1]
        );
        assert!(
            !v.overall_pass,
            "889 re-gate: an over-tolerance window must fail overall_pass again: {v:?}"
        );
        assert_eq!(
            v.windows_over_copies_gaps_tolerance, 1,
            "exactly cam2's window exceeds the tolerance: {v:?}"
        );
        assert_eq!(
            v.copies_gaps_tolerance,
            crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE,
            "the tolerance value must be echoed in the JSON so it's self-describing: {v:?}"
        );
        // cam2's window still fails STRICT too (unaffected by the re-gate -- strict was always
        // absolute-zero), so it also counts toward the pre-existing report-only metric.
        assert_eq!(v.windows_failed_report_only, 1, "{v:?}");
    }

    #[test]
    fn a_single_copy_window_is_absorbed_by_the_1220_tolerance_channel() {
        // Renamed from `..._absorbed_by_the_1169_singleton_allowance_supersedes_1132`. A window
        // with exactly ONE copy is the designed issue-1167 paced-trickle + FIFO stale_replay
        // residual (post cam1 card swap), NOT a hardware-sick leg. #1132 made it FAIL overall_pass
        // ("every multi-frame event RED"); #1169 absorbed a <=1/<=1 SINGLETON specifically; #1220
        // (owner mandate, 2026-08-29) re-arms the WIDER already-calibrated <=3 tolerance channel,
        // which now absorbs this shape (and up to 3, per the sibling test below) instead -- the
        // #1169 singleton mechanism is dormant (superseded by precedence), so its note/count read
        // as never consumed. strict stays false/visible.
        let schedule = vec![
            win("cam1", 0, 1000),
            win("cam2", 1000, 2000),
            win("cam3", 2000, 3000),
        ];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // cam2 copy #1 -- within the re-armed <=3 tolerance channel
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(501),
            },
        ]);
        frames.extend(clean_frames(2000, 100, 4, 1, 900));
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[1].copies, 1, "{:?}", v.segments[1]);
        assert!(
            v.segments[1].relaxed_pass,
            "the relaxed verdict still reports the copy within tolerance (observability): {:?}",
            v.segments[1]
        );
        assert!(
            v.overall_pass,
            "#1220: a single copy is ABSORBED into overall_pass via the re-armed tolerance: {v:?}"
        );
        assert!(
            v.segments[1].singleton_allowance_note.is_none(),
            "#1220: the singleton mechanism is dormant -- the tolerance channel absorbed this: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "#1220: the singleton mechanism never fires while the tolerance channel is armed: {v:?}"
        );
        assert_eq!(
            v.windows_over_copies_gaps_tolerance, 0,
            "the window is WITHIN the reported tolerance (count stays 0): {v:?}"
        );
        assert_eq!(
            v.windows_failed_report_only, 1,
            "still strict-fails (report-only, visible): {v:?}"
        );
    }

    #[test]
    fn two_or_three_copies_in_one_window_now_pass_overall_1220() {
        // Renamed from `two_copies_in_one_window_still_fail_overall_1169`, INVERTED: #1132 made a
        // window with TWO copies (>1, over the #1169 singleton band) FAIL overall_pass; #1220
        // (owner mandate, 2026-08-29) re-arms the WIDER <=3 tolerance channel, so 2 (and 3) copies
        // now PASS -- exactly the live-verdict shapes (CAM2 2/2, CAM6 2/1, CAM7 2/3) issue 1220
        // was filed to fix. The singleton mechanism stays dormant (never consumed) regardless.
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 4, 1, 100);
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 1100,
                tick: Some(500),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 1200,
                tick: Some(500),
            }, // copy #1
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 1300,
                tick: Some(500),
            }, // copy #2 -- over the (now-dormant) singleton allowance, within the <=3 tolerance
            SegmentFrame {
                frame_index: 103,
                gen_ts_ns: 1400,
                tick: Some(501),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert_eq!(v.segments[1].copies, 2, "{:?}", v.segments[1]);
        assert!(
            v.overall_pass,
            "#1220: 2 copies sit within the re-armed tolerance -- must now PASS overall_pass: {v:?}"
        );
        assert!(
            v.segments[1].singleton_allowance_note.is_none(),
            "#1220: the singleton mechanism is dormant -- no window is ever counted through it \
             while the tolerance channel is armed: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.windows_singleton_allowance_consumed, 0,
            "#1220: the singleton mechanism never fires while the tolerance channel is armed: {v:?}"
        );
    }

    #[test]
    fn per_cambox_override_absorbs_cam2_starvation_but_not_other_boxes_1251() {
        // #1251: an UPPERCASE `CAM2` window carrying a starvation burst (copies=8 -- the shape of
        // run 1326320314's cam2 windows, over the default 5, under CAM2's 25 override) is ABSORBED,
        // while a `CAM3` window over the default 5 still FAILS. Uppercase labels on purpose:
        // production emits CAMN, and the lowercase-`cam2` fixtures above deliberately keep the
        // default so the override touches only the real rig.
        let schedule = vec![win("CAM2", 0, 2000), win("CAM3", 2000, 3000)];
        // CAM2: tick 500 repeated 9 times -> copies=8, gaps=0 (all-same value dedups to one span).
        let mut frames: Vec<SegmentFrame> = (0..9)
            .map(|i| SegmentFrame {
                frame_index: i,
                gen_ts_ns: 100 + i as i64 * 100,
                tick: Some(500),
            })
            .collect();
        // CAM3: 100,101,108,109 -> 102..107 absent -> gaps=6 (over the default 5).
        frames.extend([
            SegmentFrame {
                frame_index: 100,
                gen_ts_ns: 2100,
                tick: Some(100),
            },
            SegmentFrame {
                frame_index: 101,
                gen_ts_ns: 2200,
                tick: Some(101),
            },
            SegmentFrame {
                frame_index: 102,
                gen_ts_ns: 2300,
                tick: Some(108),
            },
            SegmentFrame {
                frame_index: 103,
                gen_ts_ns: 2400,
                tick: Some(109),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);

        // CAM2 window: the applied per-window tolerance is the 25 override, carried on the segment
        // (serialized into the verdict JSON as `copies_gaps_tolerance` so the report shows the
        // override).
        assert_eq!(v.segments[0].cambox, "CAM2");
        assert_eq!(
            v.segments[0].copies, 8,
            "CAM2 copies computed: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.segments[0].gaps, 0,
            "CAM2 gaps computed: {:?}",
            v.segments[0]
        );
        assert_eq!(
            v.segments[0].copies_gaps_tolerance, 25,
            "CAM2 applied tolerance = the 25 override: {:?}",
            v.segments[0]
        );
        assert!(
            v.segments[0].relaxed_pass,
            "CAM2 copies=8 is ABSORBED by its 25 override: {:?}",
            v.segments[0]
        );
        assert!(
            !v.segments[0].pass,
            "the STRICT verdict still records CAM2's copies (visible, never masked): {:?}",
            v.segments[0]
        );

        // CAM3 window: keeps the default tolerance and still FAILS on gaps=6.
        assert_eq!(v.segments[1].cambox, "CAM3");
        assert_eq!(
            v.segments[1].gaps, 6,
            "CAM3 gaps computed: {:?}",
            v.segments[1]
        );
        assert_eq!(
            v.segments[1].copies_gaps_tolerance,
            crate::window_gate::WINDOW_COPIES_GAPS_TOLERANCE,
            "CAM3 keeps the default tolerance: {:?}",
            v.segments[1]
        );
        assert!(
            !v.segments[1].relaxed_pass,
            "CAM3 gaps=6 over the default 5 still fails: {:?}",
            v.segments[1]
        );

        // The run still fails -- on the genuinely-broken box (CAM3), never masked by the CAM2 relax.
        assert!(
            !v.overall_pass,
            "run fails on CAM3, not CAM2 (the override never masks a real defect): {v:?}"
        );
        // `windows_over_copies_gaps_tolerance` uses each window's OWN tolerance: only CAM3 is over.
        assert_eq!(
            v.windows_over_copies_gaps_tolerance, 1,
            "only CAM3 exceeds its own tolerance; CAM2's 8 is within 25: {v:?}"
        );
    }
}
