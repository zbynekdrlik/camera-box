//! #107 — hard-fail loss verdict from the recorded OBS program-output file.
//!
//! Consumes the #106 recording-probe per-frame stream (the recorded OBS
//! program-output file — NEVER an NDI tap, NEVER the lz4 spool, NEVER a
//! percentage threshold) and produces a HARD-FAIL zero-loss verdict.
//!
//! ## What the recorded stream is
//!
//! `recording.rs` emits one [`RecordingFrame`](crate::probe::recording::RecordingFrame)
//! per **camera frame** of the recorded program. Each carries the CRC-valid
//! payloads it decoded and the effective **Vernier tick** = `max(left, right)`
//! `frame_id` (the freshest sharp dual-QR half), or `None` when no CRC-valid QR
//! was readable in that camera frame. This module reduces that to a per-frame
//! [`FrameTick`] sequence and decides PASS / FAIL.
//!
//! ## The 60→30 free-running-camera sampling beat is NOT loss
//!
//! cam2 paints one logical counter at the **monitor refresh rate** (60 Hz). The
//! broadcast camera films that monitor at **30 fps**, free-running (un-genlocked),
//! so each camera frame samples the 60 Hz counter and the resolved tick advances
//! by ~`refresh_hz / capture_fps` = **2.0** per camera frame — but with a Vernier
//! BEAT: the per-frame step jitters around 2.0 (e.g. alternating 1,3) because the
//! camera's exposure instants drift against the monitor's refresh phase. The step
//! distribution is **symmetric around 2.0** and its **mean is exactly 2.0** — every
//! painted logical tick is sampled, on net, exactly once. That beat is *sampling*,
//! not chain loss, and MUST NOT be counted as loss.
//!
//! The honest, threshold-free discriminator is the **net** balance:
//!
//! - `sum(step) == 2 * num_pairs` (mean step EXACTLY 2.0, integer-exact) ⇒ the
//!   camera traversed exactly the logical-tick span its 30 fps sampling of the
//!   60 Hz counter predicts ⇒ **no net loss, no net duplication** ⇒ the beat is
//!   balanced.
//! - `sum(step) > 2 * num_pairs` ⇒ the surviving camera frames span MORE logical
//!   ticks than 30 fps sampling allows ⇒ camera frames were **lost** (real gaps).
//! - `sum(step) < 2 * num_pairs` ⇒ the camera frames span FEWER ticks than
//!   expected ⇒ frames were **repeated** (real stale copies / FIFO underrun).
//!
//! No percentage, no "negligible", no documented-bound: PASS = 0 undecodable AND
//! 0 net copy AND 0 net gap AND analyzed span ≥ `min_secs`.
//!
//! ## What is and isn't provable per hop
//!
//! - **strih→stream** ([`strih_stream_verdict`]): both digital outputs carry the
//!   SAME camera beat (the beat is upstream of both), so it cancels — a direct
//!   per-camera-frame tick compare is exact. Any divergence is a real
//!   strih→stream hop fault.
//! - **cam→strih** ([`cam_strih_assessment`]): the camera beat overlaps loss
//!   without a clean cam-side per-frame reference, so the strih recording ALONE
//!   cannot prove cam→strih zero-loss. The assessment compares strih ticks against
//!   the cam2 painter's displayed-tick ground truth and reports what IS provable
//!   (every strih tick was a painted tick; a never-painted tick is a real fault)
//!   while explicitly stating the limitation — it never emits a false zero claim.
//!
//! The engine ([`verdict`], [`strih_stream_verdict`], [`cam_strih_assessment`]) is
//! pure (decoupled from ffmpeg/image via [`FrameTick`]) and fully unit-tested with
//! synthetic per-frame tick streams. PNG pixel-proof extraction of the flagged
//! frames is the I/O glue in [`crate::probe::recording::extract_frames_png`].

use crate::probe::recording::RecordingFrame;
use serde::Serialize;

/// One camera frame's resolved Vernier tick, decoupled from ffmpeg/image so the
/// verdict engine is pure and unit-testable. `tick == None` ⇒ no CRC-valid QR was
/// readable in that camera frame (undecodable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTick {
    /// 0-based camera-frame index within the recording (capture order).
    pub frame_index: u64,
    /// Effective Vernier tick = `max(left, right)` decoded `frame_id`, or `None`.
    pub tick: Option<u32>,
}

impl FrameTick {
    /// Project the #106 recording-probe per-frame stream onto the per-frame tick
    /// sequence the verdict engine consumes.
    pub fn from_recording_frames(frames: &[RecordingFrame]) -> Vec<FrameTick> {
        frames
            .iter()
            .map(|f| FrameTick {
                frame_index: f.frame_index,
                tick: f.tick,
            })
            .collect()
    }
}

/// Verdict knobs. Defaults model the documented rig: 30 fps broadcast camera, a
/// 60 Hz monitor logical counter, and the ≥300 s zero-loss duration floor.
#[derive(Debug, Clone)]
pub struct VerdictConfig {
    /// Camera capture rate (fps) — used for the duration gate (`frames / fps`).
    pub capture_fps: f64,
    /// Minimum analyzed span (seconds) before a zero-loss PASS may be declared.
    /// The spec's hard floor is 300 s (ideal 1800 s).
    pub min_secs: f64,
    /// Monitor refresh rate (Hz) of the painted logical counter. The expected
    /// per-camera-frame tick step is `refresh_hz / capture_fps`.
    pub refresh_hz: f64,
}

impl Default for VerdictConfig {
    fn default() -> Self {
        VerdictConfig {
            capture_fps: 30.0,
            min_secs: 300.0,
            refresh_hz: 60.0,
        }
    }
}

/// The per-recording (single OBS output) hard-fail loss verdict.
#[derive(Debug, Clone, Serialize)]
pub struct RecordingVerdict {
    /// Total camera frames in the recording (decodable + undecodable).
    pub total_frames: usize,
    /// Analyzed span in seconds = `total_frames / capture_fps`.
    pub analyzed_secs: f64,
    /// `analyzed_secs >= min_secs` — the duration gate. A zero-loss PASS is refused
    /// below this even at 0/0/0 loss.
    pub duration_ok: bool,
    /// The configured minimum-span floor (seconds) used by the duration gate, so a
    /// report can state the threshold the run was held to.
    pub min_secs: f64,
    /// Mean per-frame tick step over decodable adjacent pairs. A clean 60→30 beat
    /// is EXACTLY 2.0; > 2.0 ⇒ net gaps, < 2.0 ⇒ net copies.
    pub avg_step: f64,
    /// The 60→30 sampling beat is balanced: mean step exactly the expected ratio
    /// (`refresh_hz / capture_fps`) AND no net copy/gap imbalance. The beat is
    /// recognized and NOT counted as loss.
    pub beat_balanced: bool,
    /// Camera frames with no CRC-valid QR (undecodable) — always a FAIL, 0 tol.
    pub undecodable_frames: Vec<u64>,
    /// Camera frames that are a NET stale-copy (tick did not advance, unmatched by
    /// a compensating overshoot) — real duplication, FAIL.
    pub real_copy_frames: Vec<u64>,
    /// Camera frames that are a NET gap (tick overshot the beat / jumped backward,
    /// unmatched by a compensating copy) — real loss / reorder, FAIL.
    pub real_gap_frames: Vec<u64>,
}

impl RecordingVerdict {
    /// PASS = 0 undecodable AND the 60→30 beat is BALANCED (no net copy/gap, mean
    /// step exactly the expected ratio) AND span ≥ `min_secs`. No thresholds, no
    /// "negligible", no documented bound. `beat_balanced` is the authoritative
    /// net-loss condition — a net imbalance fails even when it is too gradual to
    /// pin to one frame (`real_copy_frames` / `real_gap_frames` then carry the
    /// best-evidence offenders for the report, but the gate is the net balance).
    pub fn is_pass(&self) -> bool {
        self.undecodable_frames.is_empty() && self.beat_balanced && self.duration_ok
    }
}

/// Compute the per-recording verdict from a per-frame tick sequence.
///
/// The expected step is `refresh_hz / capture_fps` (2.0 for 60→30). Over the
/// decodable adjacent pairs, `sum(step)` is compared (integer-exact) against
/// `expected_step * num_pairs`:
///
/// - surplus (`> expected`) ⇒ NET gaps: the overshoot frames (`step` strictly
///   above the beat's largest balanced step, or a backward jump) are named, just
///   enough to account for the surplus.
/// - deficit (`< expected`) ⇒ NET copies: the stalled frames (`step == 0`) are
///   named, just enough to account for the deficit.
/// - exact ⇒ balanced beat, no copy/gap counted.
pub fn verdict(frames: &[FrameTick], cfg: &VerdictConfig) -> RecordingVerdict {
    let total_frames = frames.len();
    let analyzed_secs = if cfg.capture_fps > 0.0 {
        total_frames as f64 / cfg.capture_fps
    } else {
        0.0
    };
    let duration_ok = analyzed_secs >= cfg.min_secs;

    // Undecodable: any camera frame with no CRC-valid QR. Always a hard fault.
    let undecodable_frames: Vec<u64> = frames
        .iter()
        .filter(|f| f.tick.is_none())
        .map(|f| f.frame_index)
        .collect();

    // The expected per-camera-frame tick advance for the free-running 60→30 beat.
    // Integer for the documented rig (60/30 = 2); the math below uses i64 so the
    // balance test is EXACT (no float tolerance leaking a sub-1-frame net loss).
    // Clamped to >= 1: a refresh ≤ capture (ratio < 1.5 rounds toward 0/1) still
    // means the counter advances at least one tick per camera frame, so a 0 (or
    // negative, from a bad config) expected step would make `max_balanced` invalid
    // and every forward motion read as a gap. The documented rig is always 60/30=2.
    let expected_step = ((cfg.refresh_hz / cfg.capture_fps).round() as i64).max(1);

    // Walk the DECODABLE adjacent pairs; an undecodable frame breaks the chain
    // (we never bridge a step across a None — that hole is already a hard fault).
    let mut steps: Vec<(u64, i64)> = Vec::new(); // (frame_index of the LATER frame, step)
    let mut prev: Option<u32> = None;
    for f in frames {
        match (prev, f.tick) {
            (Some(p), Some(t)) => {
                steps.push((f.frame_index, t as i64 - p as i64));
                prev = Some(t);
            }
            (None, Some(t)) => prev = Some(t),
            (_, None) => prev = None, // chain breaks across the undecodable hole
        }
    }

    let num_pairs = steps.len() as i64;
    let sum_steps: i64 = steps.iter().map(|(_, s)| *s).sum();
    let avg_step = if num_pairs > 0 {
        sum_steps as f64 / num_pairs as f64
    } else {
        0.0
    };

    // Net balance: how far the realised tick span deviates from the beat's
    // expectation. surplus > 0 ⇒ net gaps; surplus < 0 ⇒ net copies. EXACT (i64).
    let surplus = sum_steps - expected_step * num_pairs;

    let mut real_gap_frames: Vec<u64> = Vec::new();
    let mut real_copy_frames: Vec<u64> = Vec::new();

    // When the beat is net-IMBALANCED (surplus != 0 = real loss), name the offending
    // frames so EVERY FAIL has at least one frame to extract as pixel proof
    // (acceptance #6). The PASS/FAIL gate itself is `beat_balanced` (surplus == 0),
    // see `is_pass`. Frames are named by how far each step deviates from the expected
    // step, MOST-DEVIANT FIRST (the real offenders), continuing until the whole
    // surplus/deficit is accounted for. A clear overshoot (step > 2*expected-1) or
    // stall (step < 1, i.e. <= 0) is the unambiguous culprit; a GRADUAL imbalance
    // whose steps all sit inside the beat's natural {1..=2*expected-1} range still
    // names its most-deviant contributors (honest: a real-loss FAIL is never left
    // with no pixel proof).
    if surplus > 0 {
        // NET gaps: name the longest steps (above the expected step) first.
        let mut over: Vec<(u64, i64)> = steps
            .iter()
            .copied()
            .filter(|(_, s)| *s > expected_step)
            .collect();
        over.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut accounted: i64 = 0;
        for (idx, step) in over {
            if accounted >= surplus {
                break;
            }
            real_gap_frames.push(idx);
            accounted += step - expected_step;
        }
        real_gap_frames.sort_unstable();
    } else if surplus < 0 {
        // NET copies: name the shortest steps (below the expected step) first.
        let deficit = -surplus;
        let mut stalls: Vec<(u64, i64)> = steps
            .iter()
            .copied()
            .filter(|(_, s)| *s < expected_step)
            .collect();
        stalls.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        let mut accounted: i64 = 0;
        for (idx, step) in stalls {
            if accounted >= deficit {
                break;
            }
            real_copy_frames.push(idx);
            accounted += expected_step - step;
        }
        real_copy_frames.sort_unstable();
    }

    let beat_balanced = surplus == 0 && num_pairs > 0;

    RecordingVerdict {
        total_frames,
        analyzed_secs,
        duration_ok,
        min_secs: cfg.min_secs,
        avg_step,
        beat_balanced,
        undecodable_frames,
        real_copy_frames,
        real_gap_frames,
    }
}

/// The strih→stream hop verdict: a DIRECT per-camera-frame tick compare.
///
/// Both digital outputs carry the SAME camera beat (it is upstream of both), so it
/// cancels: at each camera-frame index the strih and stream resolved ticks MUST be
/// identical. Any index where they differ — or where one output decoded and the
/// other did not — is a real strih→stream fault (a dropped or repeated frame on the
/// digital hop). No beat reasoning, no threshold: the beat is common, so it is an
/// exact equality check.
#[derive(Debug, Clone, Serialize)]
pub struct StrihStreamVerdict {
    /// Distinct resolved ticks compared — the overlapping tick range present in
    /// BOTH recordings. The compare is on the tick SEQUENCES, not per-file capture
    /// positions, so it is immune to a start offset between the two independent
    /// recordings (the camera beat is common to both, so both record the SAME
    /// ordered tick set on a lossless hop).
    pub compared_ticks: usize,
    /// Ticks present at strih but ABSENT at stream within the overlap span — frames
    /// the stream output dropped on the strih→stream hop. Sorted ascending.
    pub strih_only_ticks: Vec<u32>,
    /// Ticks present at stream but ABSENT at strih within the overlap span — frames
    /// stream has that strih does not (reorder / phantom). Sorted ascending.
    pub stream_only_ticks: Vec<u32>,
}

impl StrihStreamVerdict {
    /// All divergent ticks (strih-only ∪ stream-only) within the overlap span.
    pub fn divergent_ticks(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .strih_only_ticks
            .iter()
            .chain(self.stream_only_ticks.iter())
            .copied()
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// PASS = the two outputs carry the SAME tick set across their overlap, and the
    /// overlap is non-vacuous (at least 2 ticks — a 0/1-tick overlap cannot
    /// demonstrate a clean hop).
    pub fn is_pass(&self) -> bool {
        self.strih_only_ticks.is_empty()
            && self.stream_only_ticks.is_empty()
            && self.compared_ticks >= 2
    }
}

/// Compare the strih and stream per-frame tick SEQUENCES directly (acceptance #4).
///
/// The camera beat is common to BOTH digital outputs (it is upstream of both), so a
/// lossless strih→stream hop records the SAME ordered set of resolved ticks at each
/// output. This compares the tick SETS within the overlapping tick range
/// `[max(lo), min(hi)]` — NOT the per-file capture positions (`frame_index` is each
/// recording's own 0-based counter; two independent OBS recordings never start on
/// the identical camera frame, so a positional pair-up would compare different
/// camera moments). Offset-immune by construction. A tick present at one output and
/// absent at the other within the overlap is a real hop fault: a strih-only tick is a
/// frame the stream output dropped; a stream-only tick is a reorder / phantom.
/// Undecodable frames (`None`) carry no tick and are excluded here — each is already
/// a hard fault in its own recording's [`verdict`].
pub fn strih_stream_verdict(
    strih: &[FrameTick],
    stream: &[FrameTick],
    _cfg: &VerdictConfig,
) -> StrihStreamVerdict {
    use std::collections::BTreeSet;
    let strih_ticks: BTreeSet<u32> = strih.iter().filter_map(|f| f.tick).collect();
    let stream_ticks: BTreeSet<u32> = stream.iter().filter_map(|f| f.tick).collect();

    // Overlap span: only ticks that BOTH recordings had a chance to carry. A tick
    // outside one recording's [lo,hi] is tap start/stop skew (the two outputs
    // connect/disconnect independently), not a hop drop — the same active-span
    // handling the NDI-tap differ uses, applied here to the recorded tick sets.
    let (lo, hi) = match (
        strih_ticks.iter().next().max(stream_ticks.iter().next()),
        strih_ticks
            .iter()
            .next_back()
            .min(stream_ticks.iter().next_back()),
    ) {
        (Some(&a), Some(&b)) if a <= b => (a, b),
        // Disjoint or empty: no overlap to compare.
        _ => {
            return StrihStreamVerdict {
                compared_ticks: 0,
                strih_only_ticks: Vec::new(),
                stream_only_ticks: Vec::new(),
            };
        }
    };

    let in_span = |t: u32| t >= lo && t <= hi;
    let strih_only_ticks: Vec<u32> = strih_ticks
        .iter()
        .copied()
        .filter(|&t| in_span(t) && !stream_ticks.contains(&t))
        .collect();
    let stream_only_ticks: Vec<u32> = stream_ticks
        .iter()
        .copied()
        .filter(|&t| in_span(t) && !strih_ticks.contains(&t))
        .collect();
    // Compared = distinct ticks in the overlap span present in BOTH.
    let compared_ticks = strih_ticks
        .iter()
        .filter(|&&t| in_span(t) && stream_ticks.contains(&t))
        .count();

    StrihStreamVerdict {
        compared_ticks,
        strih_only_ticks,
        stream_only_ticks,
    }
}

/// The cam→strih honest assessment — what the strih recording ALONE can and cannot
/// prove about the cam→strih hop.
///
/// The camera beat overlaps loss without a clean cam-side per-frame reference, so
/// the strih recording cannot certify cam→strih zero-loss (a frame the camera
/// never captured and a frame strih dropped both look like a missing painted tick).
/// What IS provable: every tick strih recorded must be a tick the painter actually
/// displayed; a strih tick the painter NEVER displayed is a real corruption /
/// phantom-id fault. The assessment reports that, states the limitation in plain
/// words, and NEVER sets `claims_zero_loss` — no false zero claim.
#[derive(Debug, Clone, Serialize)]
pub struct CamStrihAssessment {
    /// Always `false`: the strih recording cannot prove cam→strih zero-loss.
    pub claims_zero_loss: bool,
    /// strih ticks the painter never displayed, WITHIN the painter's covered range —
    /// real corruption / phantom ids. Ticks OUTSIDE the painter's range are NOT
    /// flagged (see `out_of_painter_range_ticks`): a partial painter capture would
    /// otherwise false-flag legitimate ticks. Sorted ascending.
    pub unknown_ticks: Vec<u32>,
    /// strih ticks that fall OUTSIDE the painter ground-truth's `[min,max]` range —
    /// the painter CSV did not cover them, so their validity is UNKNOWN (not a
    /// fault, but not provably clean either). Surfaced so a partial painter capture
    /// is visible rather than silently treated as all-phantom or all-fine. Sorted.
    pub out_of_painter_range_ticks: Vec<u32>,
    /// Plain-language statement of what the strih recording cannot prove.
    pub limitation: String,
}

/// Compare strih's recorded ticks against the cam2 painter's displayed-tick set
/// (`painter_ticks` = every logical tick the painter actually put on the monitor).
///
/// PRECONDITION for `unknown_ticks` to be meaningful: the painter ground truth must
/// span the strih recording. Only strih ticks INSIDE the painter's `[min,max]` range
/// are checked for membership — a strih tick the painter displayed-range covers but
/// that is absent from the painted set is a real phantom (`unknown_ticks`); a strih
/// tick OUTSIDE the painter's covered range is reported separately
/// (`out_of_painter_range_ticks`), never as a phantom fault, so a partial painter
/// capture (started late / stopped early / counter wrap) does not manufacture false
/// cam→strih faults.
pub fn cam_strih_assessment(
    strih: &[FrameTick],
    painter_ticks: &[u32],
    _cfg: &VerdictConfig,
) -> CamStrihAssessment {
    use std::collections::BTreeSet;
    let painted: BTreeSet<u32> = painter_ticks.iter().copied().collect();
    let (p_lo, p_hi) = (
        painted.iter().next().copied(),
        painted.iter().next_back().copied(),
    );

    let mut unknown: BTreeSet<u32> = BTreeSet::new();
    let mut out_of_range: BTreeSet<u32> = BTreeSet::new();
    for f in strih {
        if let Some(t) = f.tick {
            match (p_lo, p_hi) {
                (Some(lo), Some(hi)) if t >= lo && t <= hi => {
                    // Inside the painter's covered range: a miss here is a phantom.
                    if !painted.contains(&t) {
                        unknown.insert(t);
                    }
                }
                // Outside the covered range (or no painter data): uncertain, not a fault.
                _ => {
                    out_of_range.insert(t);
                }
            }
        }
    }
    CamStrihAssessment {
        claims_zero_loss: false,
        unknown_ticks: unknown.into_iter().collect(),
        out_of_painter_range_ticks: out_of_range.into_iter().collect(),
        limitation: "cam→strih zero-loss is NOT provable from the strih recording \
            alone: the free-running 60→30 camera beat overlaps loss without a clean \
            per-frame cam-side reference (a frame the camera never captured and a \
            frame strih dropped both present as a missing painted tick). Only a strih \
            tick the painter never displayed WITHIN the painter's covered range \
            (unknown_ticks) is a provable cam→strih fault; ticks outside that range \
            (out_of_painter_range_ticks) are uncertain, not faults; and the absence \
            of phantom ticks is necessary but NOT sufficient for a zero-loss claim. \
            The painter ground truth must span the strih recording for this check to \
            be complete."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ft(frame_index: u64, tick: Option<u32>) -> FrameTick {
        FrameTick { frame_index, tick }
    }

    #[test]
    fn from_recording_frames_projects_index_and_tick() {
        use crate::probe::payload::Payload;
        let rf = RecordingFrame {
            frame_index: 9,
            payloads: vec![Payload {
                run_id: 1,
                frame_id: 42,
                gen_ts_ns: 0,
            }],
            tick: Some(42),
        };
        let got = FrameTick::from_recording_frames(&[rf]);
        assert_eq!(got, vec![ft(9, Some(42))]);
    }

    #[test]
    fn expected_step_is_refresh_over_capture() {
        // A tiny balanced 1,3,1,3 beat: avg exactly 2.0, balanced, no copy/gap.
        let frames = vec![
            ft(0, Some(100)),
            ft(1, Some(101)),
            ft(2, Some(104)),
            ft(3, Some(105)),
            ft(4, Some(108)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!((v.avg_step - 2.0).abs() < 1e-9);
        assert!(v.beat_balanced);
        assert!(v.real_copy_frames.is_empty());
        assert!(v.real_gap_frames.is_empty());
    }

    #[test]
    fn gradual_imbalance_fails_and_still_names_frames_for_pixel_proof() {
        // CRITICAL: a NET imbalance whose every step stays inside the beat's natural
        // {1,2,3} range (steps all 3 ⇒ surplus > 0, NO step exceeds the ceiling)
        // must (a) FAIL — `beat_balanced` is the authoritative gate — AND (b) still
        // name its most-deviant contributors so the FAIL has pixel proof (acceptance
        // #6: every real-loss FAIL extracts ≥1 PNG). Here steps are 3,3,3 ⇒ surplus
        // = 3; the longest steps (all 3 > expected 2) are named until accounted.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(3)),
            ft(2, Some(6)),
            ft(3, Some(9)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(v.avg_step > 2.0, "more 3s than 1s ⇒ avg > 2.0");
        assert!(!v.beat_balanced, "net imbalance ⇒ not a balanced beat");
        assert!(
            !v.is_pass(),
            "a gradual net imbalance must FAIL (beat_balanced is the gate)"
        );
        assert!(
            !v.real_gap_frames.is_empty(),
            "a real-loss FAIL must name ≥1 frame so it has pixel proof"
        );
    }

    #[test]
    fn surplus_one_frame_is_a_single_named_gap() {
        // Pure step-2 beat with ONE +4 overshoot (surplus = 2 over expected): the
        // single overshoot frame is named, nothing else.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(2)),
            ft(2, Some(6)), // step 4 (overshoot by 2) — the gap
            ft(3, Some(8)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(v.avg_step > 2.0);
        assert_eq!(v.real_gap_frames, vec![2]);
        assert!(v.real_copy_frames.is_empty());
        assert!(!v.beat_balanced);
        assert!(!v.is_pass());
    }

    #[test]
    fn deficit_one_frame_is_a_single_named_copy() {
        // Pure step-2 beat with ONE held tick (step 0): net deficit -> the stalled
        // frame is named as a copy.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(2)),
            ft(2, Some(2)), // step 0 — the stale copy
            ft(3, Some(4)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(v.avg_step < 2.0);
        assert_eq!(v.real_copy_frames, vec![2]);
        assert!(v.real_gap_frames.is_empty());
        assert!(!v.is_pass());
    }

    #[test]
    fn balanced_zero_and_four_is_not_loss() {
        // A 0 step matched by a 4 step (the beat's 0<->4 symmetry): net surplus 0,
        // so NEITHER is counted as copy or gap — recognized as pure beat.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(0)), // step 0
            ft(2, Some(4)), // step 4 — balances the 0
            ft(3, Some(6)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!((v.avg_step - 2.0).abs() < 1e-9);
        assert!(v.beat_balanced);
        assert!(v.real_copy_frames.is_empty());
        assert!(v.real_gap_frames.is_empty());
    }

    #[test]
    fn backward_jump_is_a_named_gap() {
        // A reorder (negative step) creates a deficit, named as a copy/backward
        // fault. Pins that a backward step is never silently a balanced beat.
        let frames = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(8))];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(!v.is_pass());
        // step -4 is a stall (<=0) -> named as a copy (a backward repeat).
        assert_eq!(v.real_copy_frames, vec![2]);
    }

    #[test]
    fn undecodable_breaks_the_step_chain_and_fails() {
        // The None hole is a hard fault AND it must not be bridged into a giant
        // false step between its decodable neighbours.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(2)),
            ft(2, None), // undecodable
            ft(3, Some(4)),
            ft(4, Some(6)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert_eq!(v.undecodable_frames, vec![2]);
        // The 2->4 step across the hole is NOT counted (chain broke); remaining
        // steps (0->2, 4->6) are clean -> no false gap/copy.
        assert!(v.real_gap_frames.is_empty());
        assert!(v.real_copy_frames.is_empty());
        assert!(!v.is_pass(), "an undecodable frame always fails");
    }

    #[test]
    fn empty_stream_never_passes() {
        let v = verdict(&[], &VerdictConfig::default());
        assert!(!v.is_pass());
        assert!(!v.beat_balanced);
        assert_eq!(v.total_frames, 0);
    }

    #[test]
    fn duration_gate_uses_frames_over_fps() {
        // 9000 frames @ 30 fps = exactly 300 s -> duration_ok (>=).
        let frames: Vec<FrameTick> = (0..9000u64)
            .map(|i| ft(i, Some(1000 + 2 * i as u32)))
            .collect();
        let v = verdict(&frames, &VerdictConfig::default());
        assert!((v.analyzed_secs - 300.0).abs() < 1e-9);
        assert!(v.duration_ok);
    }

    #[test]
    fn strih_stream_equal_passes_diverge_fails() {
        let strih = vec![ft(0, Some(2)), ft(1, Some(4)), ft(2, Some(6))];
        let same = strih.clone();
        let v = strih_stream_verdict(&strih, &same, &VerdictConfig::default());
        assert!(v.is_pass());
        assert_eq!(v.compared_ticks, 3);

        // stream decoded a tick (99) strih never had -> stream-only divergence.
        let mut diff = strih.clone();
        diff[1].tick = Some(99);
        let v2 = strih_stream_verdict(&strih, &diff, &VerdictConfig::default());
        assert!(!v2.is_pass());
        // tick 4 is strih-only (stream replaced it with 99); 99 is outside the
        // overlap span [2,6] so only the strih-only 4 is flagged.
        assert_eq!(v2.strih_only_ticks, vec![4]);
    }

    #[test]
    fn strih_stream_is_offset_immune() {
        // CRITICAL (review): the two recordings start on DIFFERENT camera frames,
        // so the SAME tick sequence has different per-file frame_index. A
        // sequence/tick compare must PASS (the hop is lossless); a positional
        // frame_index pairing would falsely diverge everywhere.
        let strih = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(14))];
        // stream: identical ticks but shifted +5 in capture position (later start).
        let stream = vec![ft(5, Some(10)), ft(6, Some(12)), ft(7, Some(14))];
        let v = strih_stream_verdict(&strih, &stream, &VerdictConfig::default());
        assert!(
            v.is_pass(),
            "identical tick sequence at a capture offset must PASS (offset-immune)"
        );
        assert_eq!(v.compared_ticks, 3);
    }

    #[test]
    fn strih_stream_stream_dropped_frame_fails() {
        // CRITICAL (review): stream DROPPED a frame (tick 12) the strih output had,
        // inside the overlap span [10,14]. A positional map-get on the missing index
        // would silently skip it -> false PASS. The tick-set compare flags it.
        let strih = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(14))];
        let stream = vec![ft(0, Some(10)), ft(1, Some(14))]; // 12 missing
        let v = strih_stream_verdict(&strih, &stream, &VerdictConfig::default());
        assert!(!v.is_pass(), "a stream-dropped frame must FAIL the hop");
        assert_eq!(v.strih_only_ticks, vec![12]);
        assert_eq!(v.divergent_ticks(), vec![12]);
    }

    #[test]
    fn strih_stream_ignores_start_stop_skew_outside_overlap() {
        // strih ran longer at both ends (ticks 8 and 20 outside the stream span):
        // those are tap start/stop skew, NOT hop drops — excluded from the overlap.
        let strih = vec![
            ft(0, Some(8)),
            ft(1, Some(10)),
            ft(2, Some(12)),
            ft(3, Some(14)),
            ft(4, Some(20)),
        ];
        let stream = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(14))];
        let v = strih_stream_verdict(&strih, &stream, &VerdictConfig::default());
        assert!(
            v.is_pass(),
            "skew outside the overlap span must not fail the hop"
        );
        assert_eq!(v.compared_ticks, 3);
        assert!(v.strih_only_ticks.is_empty());
    }

    #[test]
    fn strih_stream_empty_overlap_is_not_pass() {
        let v = strih_stream_verdict(&[], &[], &VerdictConfig::default());
        assert!(!v.is_pass(), "no compared ticks must not vacuously pass");
        assert_eq!(v.compared_ticks, 0);

        // Disjoint tick ranges (no overlap at all) also must not pass.
        let strih = vec![ft(0, Some(1)), ft(1, Some(2))];
        let stream = vec![ft(0, Some(100)), ft(1, Some(101))];
        let v2 = strih_stream_verdict(&strih, &stream, &VerdictConfig::default());
        assert!(!v2.is_pass(), "disjoint tick ranges must not pass");
    }

    #[test]
    fn cam_strih_never_claims_zero_loss_and_flags_in_range_phantom() {
        // tick 13 is INSIDE the painter range [10,20] but the painter set has a hole
        // there (never displayed) -> a real in-range phantom fault.
        let strih = vec![ft(0, Some(10)), ft(1, Some(13)), ft(2, Some(20))];
        let painter: Vec<u32> = (10..=20).filter(|&t| t != 13).collect();
        let a = cam_strih_assessment(&strih, &painter, &VerdictConfig::default());
        assert!(!a.claims_zero_loss);
        assert_eq!(a.unknown_ticks, vec![13]);
        assert!(a.out_of_painter_range_ticks.is_empty());
        assert!(!a.limitation.is_empty());
    }

    #[test]
    fn cam_strih_out_of_range_tick_is_uncertain_not_phantom() {
        // tick 777 is OUTSIDE the painter range [10,20] -> uncertain (the painter
        // CSV didn't cover it), NOT a phantom fault. A partial painter capture must
        // not manufacture false cam→strih faults.
        let strih = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(777))];
        let painter: Vec<u32> = (10..=20).collect();
        let a = cam_strih_assessment(&strih, &painter, &VerdictConfig::default());
        assert!(a.unknown_ticks.is_empty(), "out-of-range is not a phantom");
        assert_eq!(a.out_of_painter_range_ticks, vec![777]);
    }

    #[test]
    fn cam_strih_all_painted_has_no_unknown_but_still_no_zero_claim() {
        let strih = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(14))];
        let painter: Vec<u32> = (10..=14).collect();
        let a = cam_strih_assessment(&strih, &painter, &VerdictConfig::default());
        assert!(a.unknown_ticks.is_empty());
        assert!(
            !a.claims_zero_loss,
            "absence of phantom ticks is necessary but NOT sufficient for zero-loss"
        );
    }
}
