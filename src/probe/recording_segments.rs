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
//! loss). The remaining in-window frames run the SAME continuity check the per-node burn verdict
//! uses ([`burn_contiguity_in_window_with_step`]) on the PAINTED TICK (the cam2 optical Vernier
//! tick, common to every cambox through the splitter). We report per cambox:
//!
//! - `frames`: in-window delivered frames after the guard discard.
//! - `undecodable`: delivered frames whose painted tick did not decode.
//! - `copies`: stale/frozen frames (the painted tick repeated — it MUST advance per frame).
//! - `gaps`: REAL DROPs the reused check found (a forward skip beyond the step, or a backward jump).
//! - `pass`: `frames > 0 && undecodable == 0 && copies == 0 && gaps == 0`.
//!
//! The painted tick increments PER PAINTED FRAME and is captured at the cambox rate, so its
//! by-design step in the recording is the decimation factor (`expected_step`): cam→strih 60fps
//! capture of the 60Hz painter ⇒ step 1; strih→stream 60→30 ⇒ step 2. `expected_step` is a
//! PARAMETER (the binary derives it from the configured fps, the harness can override) — the
//! logic bakes NO 30-vs-60 assumption; the reused check already detects gaps/copies rate-agnostically.
//!
//! A window with ZERO in-window frames FAILS (an absent cambox proves nothing — never read as a
//! pass), so the verdict can "clearly report which cameras were covered and which were not"
//! (#312 acceptance) without ever implying full coverage when a box was absent (e.g. CAM3 down #301).
//!
//! This module is PURE (no I/O beyond reading the schedule file) and unit-tested with synthetic
//! frame sequences; the binary glue (extracting [`SegmentFrame`]s from decoded [`crate::probe::
//! recording::RecordingFrame`]s) lives in `recording-verdict`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::probe::burn_contiguity::{
    burn_contiguity_in_window_with_step, BurnRate, InWindowMissingKind, RecordedBurnFrame,
};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// REAL DROPs from the reused continuity check (forward skip beyond the decimation step,
    /// or a backward jump).
    pub gaps: u32,
    /// First / last painted tick seen in-window (informational; `None` ⇒ no readable tick).
    pub first_tick: Option<u32>,
    pub last_tick: Option<u32>,
    /// This cambox's segment is clean ⇔ `frames > 0 && undecodable == 0 && copies == 0 && gaps == 0`.
    pub pass: bool,
}

/// The whole-recording segmented-continuity verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentedContinuity {
    /// One verdict per schedule window, in schedule order.
    pub segments: Vec<CamboxSegment>,
    /// PASS ⇔ the schedule is non-empty AND EVERY window is clean (every covered cambox passed).
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

    // STUB (RED): the gen_ts partition is not implemented yet — every window gets NO frames, so
    // each cambox FAILs (frames == 0) and the behaviour tests are RED until the real partition +
    // per-cambox painted-tick continuity lands in the GREEN commit.
    let _ = frames;
    let window_frames: Vec<Vec<SegmentFrame>> = vec![Vec::new(); schedule.len()];
    let discarded_guard_frames: u32 = 0;
    let unplaceable_frames: u32 = 0;

    let mut overall_pass = !schedule.is_empty();
    let mut segments = Vec::with_capacity(schedule.len());
    for (wi, w) in schedule.iter().enumerate() {
        let seg = window_segment(
            &w.cambox,
            w.start_ns,
            w.end_ns,
            &window_frames[wi],
            expected_step,
        );
        overall_pass &= seg.pass;
        segments.push(seg);
    }

    SegmentedContinuity {
        segments,
        overall_pass,
        guard_ns,
        expected_step,
        discarded_guard_frames,
        unplaceable_frames,
    }
}

/// The per-window continuity, on the painted tick. `undecodable` is the count of `None`-tick
/// delivered frames (robust even when EVERY frame is undecodable, which the reused check returns
/// as an empty missing-slot set); `gaps` is the REAL-DROP count from
/// [`burn_contiguity_in_window_with_step`] (the SAME check the per-node burn verdict uses, so a
/// by-design decimation step is not loss but a true skip/backward-jump is); `copies` is the count
/// of stale repeats of the painted tick — a metric the burn check (a free-running counter never
/// repeats) does not surface but the painted tick (which MUST advance per frame) requires.
fn window_segment(
    cambox: &str,
    start_ns: i64,
    end_ns: i64,
    frames: &[SegmentFrame],
    expected_step: i64,
) -> CamboxSegment {
    let frame_count = frames.len() as u32;

    let undecodable = frames.iter().filter(|f| f.tick.is_none()).count() as u32;

    // copies (stale): a delivered frame whose painted tick equals the immediately preceding
    // PRESENT tick — a frozen/duplicate frame. The painter advances the tick per painted frame,
    // so a repeat is a held frame, never a by-design step (which the gap check handles).
    let mut copies: u32 = 0;
    let mut prev_present: Option<u32> = None;
    for f in frames {
        if let Some(t) = f.tick {
            if prev_present == Some(t) {
                copies = copies.saturating_add(1);
            }
            prev_present = Some(t);
        }
    }

    // gaps: reuse the existing in-window continuity check on the painted tick (PerRenderTick +
    // expected_step — a forward gap == step is the by-design decimation, a larger gap charges the
    // excess as a REAL DROP, a backward jump is a fault). undecodable (`None`) frames are charged
    // BURN-UNREADABLE there and credited against the gap math, so they never double-count as gaps.
    let recorded: Vec<RecordedBurnFrame> = frames
        .iter()
        .map(|f| RecordedBurnFrame {
            frame_index: f.frame_index,
            burn_id: f.tick,
        })
        .collect();
    let in_window = burn_contiguity_in_window_with_step(
        cambox,
        &recorded,
        BurnRate::PerRenderTick,
        expected_step,
    );
    let gaps = in_window
        .missing_slots
        .iter()
        .filter(|s| s.kind == InWindowMissingKind::RealDrop)
        .count() as u32;

    let first_tick = frames.iter().find_map(|f| f.tick);
    let last_tick = frames.iter().rev().find_map(|f| f.tick);

    let pass = frame_count > 0 && undecodable == 0 && copies == 0 && gaps == 0;
    CamboxSegment {
        cambox: cambox.to_string(),
        start_ns,
        end_ns,
        frames: frame_count,
        undecodable,
        copies,
        gaps,
        first_tick,
        last_tick,
        pass,
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
    fn gap_in_one_window_fails_that_cambox_others_pass_overall_fail() {
        // cam1 clean; cam2 has a tick that skips by 3 (a real drop at step 1) → cam2 FAILs.
        let schedule = vec![win("cam1", 0, 1000), win("cam2", 1000, 2000)];
        let mut frames = clean_frames(0, 100, 6, 1, 100);
        // cam2: 500,501,504,505 — 502,503 absent (a real gap), step 1.
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
                tick: Some(504),
            },
            SegmentFrame {
                frame_index: 103,
                gen_ts_ns: 1400,
                tick: Some(505),
            },
        ]);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(!v.overall_pass, "a gap ⇒ overall FAIL: {v:?}");
        assert!(v.segments[0].pass, "cam1 still clean: {:?}", v.segments[0]);
        assert!(
            !v.segments[1].pass,
            "cam2 has the gap → FAIL: {:?}",
            v.segments[1]
        );
        assert!(
            v.segments[1].gaps >= 1,
            "cam2 gap counted: {:?}",
            v.segments[1]
        );
        assert_eq!(v.segments[1].undecodable, 0);
        assert_eq!(v.segments[1].copies, 0);
    }

    #[test]
    fn undecodable_frame_in_one_window_fails_that_cambox() {
        // cam2 has one delivered frame with no painted tick (None) → undecodable → FAIL.
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
        assert!(!v.overall_pass);
        assert!(v.segments[0].pass);
        assert!(
            !v.segments[1].pass,
            "undecodable ⇒ FAIL: {:?}",
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
    fn copy_stale_frame_in_one_window_fails_that_cambox() {
        // cam2 repeats a painted tick (500,500,501) → a stale/frozen copy → FAIL.
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
        assert!(!v.overall_pass);
        assert!(v.segments[0].pass);
        assert!(!v.segments[1].pass, "a copy ⇒ FAIL: {:?}", v.segments[1]);
        assert_eq!(
            v.segments[1].copies, 1,
            "exactly one copy: {:?}",
            v.segments[1]
        );
        assert_eq!(v.segments[1].gaps, 0);
        assert_eq!(v.segments[1].undecodable, 0);
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
        let schedule = vec![win("cam1", 0, 100_000)];
        let frames = clean_frames(0, 1000, 10, 2, 1000);
        let v = segment_continuity(&frames, &schedule, 0, 1);
        assert!(
            !v.overall_pass,
            "step-2 data at expected_step=1 ⇒ gaps: {v:?}"
        );
        assert!(v.segments[0].gaps >= 1, "gaps flagged: {:?}", v.segments[0]);
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
}
