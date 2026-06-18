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
    let expected_step = (cfg.refresh_hz / cfg.capture_fps).round() as i64;

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
    // frames for the pixel-proof report. The PASS/FAIL gate itself is `beat_balanced`
    // (surplus == 0) — see `is_pass` — so even a GRADUAL imbalance that never breaks
    // the beat's per-step bounds still FAILS (then `avg_step != expected` is the
    // evidence and no individual frame is singled out, which is honest). The frames
    // named here are the UNAMBIGUOUS offenders: steps outside the balanced beat's
    // natural range {min_balanced..=max_balanced}. For a symmetric beat around the
    // expected step that range is `[1, 2*expected-1]` (e.g. {1,2,3} for expected 2):
    // a step above `2*expected-1` is a clear gap, a step below `1` (i.e. <= 0, a held
    // or backward tick) is a clear stale copy.
    let max_balanced = 2 * expected_step - 1;
    let min_balanced = 1;
    if surplus > 0 {
        // NET gaps: name the clear overshoots (step > max_balanced), largest first;
        // ties broken by frame order for determinism.
        let mut over: Vec<(u64, i64)> = steps
            .iter()
            .copied()
            .filter(|(_, s)| *s > max_balanced)
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
        // NET copies: name the clear stalls (step < min_balanced, i.e. <= 0 — a held
        // tick or backward repeat), smallest/most-negative first.
        let deficit = -surplus;
        let mut stalls: Vec<(u64, i64)> = steps
            .iter()
            .copied()
            .filter(|(_, s)| *s < min_balanced)
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
    /// Camera-frame indices compared (the overlap of the two recordings).
    pub compared_frames: usize,
    /// Indices where strih and stream resolved ticks differ — real hop faults.
    pub divergent_frames: Vec<u64>,
}

impl StrihStreamVerdict {
    /// PASS = the two outputs agree on every compared camera frame.
    pub fn is_pass(&self) -> bool {
        self.divergent_frames.is_empty() && self.compared_frames > 0
    }
}

/// Compare the strih and stream per-frame tick sequences directly. Pairs frames by
/// `frame_index` (capture position); a frame present at one output and absent at
/// the other, or decoded differently, is divergent.
pub fn strih_stream_verdict(
    strih: &[FrameTick],
    stream: &[FrameTick],
    _cfg: &VerdictConfig,
) -> StrihStreamVerdict {
    use std::collections::BTreeMap;
    let stream_by_idx: BTreeMap<u64, Option<u32>> =
        stream.iter().map(|f| (f.frame_index, f.tick)).collect();

    let mut compared = 0usize;
    let mut divergent_frames: Vec<u64> = Vec::new();
    for f in strih {
        if let Some(&s_tick) = stream_by_idx.get(&f.frame_index) {
            compared += 1;
            if s_tick != f.tick {
                divergent_frames.push(f.frame_index);
            }
        }
    }
    divergent_frames.sort_unstable();
    StrihStreamVerdict {
        compared_frames: compared,
        divergent_frames,
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
    /// strih ticks the painter never displayed — real corruption / phantom ids.
    pub unknown_ticks: Vec<u32>,
    /// Plain-language statement of what the strih recording cannot prove.
    pub limitation: String,
}

/// Compare strih's recorded ticks against the cam2 painter's displayed-tick set
/// (`painter_ticks` = every logical tick the painter actually put on the monitor).
pub fn cam_strih_assessment(
    strih: &[FrameTick],
    painter_ticks: &[u32],
    _cfg: &VerdictConfig,
) -> CamStrihAssessment {
    use std::collections::BTreeSet;
    let painted: BTreeSet<u32> = painter_ticks.iter().copied().collect();
    let mut unknown: BTreeSet<u32> = BTreeSet::new();
    for f in strih {
        if let Some(t) = f.tick {
            if !painted.contains(&t) {
                unknown.insert(t);
            }
        }
    }
    CamStrihAssessment {
        claims_zero_loss: false,
        unknown_ticks: unknown.into_iter().collect(),
        limitation: "cam→strih zero-loss is NOT provable from the strih recording \
            alone: the free-running 60→30 camera beat overlaps loss without a clean \
            per-frame cam-side reference (a frame the camera never captured and a \
            frame strih dropped both present as a missing painted tick). Only a \
            strih tick the painter never displayed (unknown_ticks) is a provable \
            cam→strih fault; absence of those is necessary but NOT sufficient for a \
            zero-loss claim."
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
    fn gradual_imbalance_fails_via_beat_balanced_even_with_no_named_frame() {
        // CRITICAL: a NET imbalance whose every step stays inside the beat's
        // natural {1,2,3} range (more 3s than 1s ⇒ surplus > 0, but NO step > 3)
        // must still FAIL — `beat_balanced` is the authoritative gate, not the
        // named-frame lists. Here: steps 3,3,3 ⇒ surplus = 3 over expected, no
        // step exceeds the ceiling so `real_gap_frames` is empty, yet is_pass()
        // MUST be false (kills an is_pass that only checks the frame lists).
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
            v.real_gap_frames.is_empty(),
            "no single step exceeds the beat ceiling — gradual drift"
        );
        assert!(
            !v.is_pass(),
            "a gradual net imbalance must FAIL even with no individual frame named"
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
        assert_eq!(v.compared_frames, 3);

        let mut diff = strih.clone();
        diff[1].tick = Some(99);
        let v2 = strih_stream_verdict(&strih, &diff, &VerdictConfig::default());
        assert!(!v2.is_pass());
        assert_eq!(v2.divergent_frames, vec![1]);
    }

    #[test]
    fn strih_stream_compares_only_overlapping_indices() {
        // stream is missing index 2 entirely; only 0,1 overlap and they agree.
        let strih = vec![ft(0, Some(2)), ft(1, Some(4)), ft(2, Some(6))];
        let stream = vec![ft(0, Some(2)), ft(1, Some(4))];
        let v = strih_stream_verdict(&strih, &stream, &VerdictConfig::default());
        assert_eq!(v.compared_frames, 2);
        assert!(v.is_pass());
    }

    #[test]
    fn strih_stream_empty_overlap_is_not_pass() {
        let v = strih_stream_verdict(&[], &[], &VerdictConfig::default());
        assert!(!v.is_pass(), "no compared frames must not vacuously pass");
    }

    #[test]
    fn cam_strih_never_claims_zero_loss_and_flags_phantom() {
        let strih = vec![ft(0, Some(10)), ft(1, Some(12)), ft(2, Some(777))];
        let painter: Vec<u32> = (10..=20).collect(); // 777 was never painted
        let a = cam_strih_assessment(&strih, &painter, &VerdictConfig::default());
        assert!(!a.claims_zero_loss);
        assert_eq!(a.unknown_ticks, vec![777]);
        assert!(!a.limitation.is_empty());
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
