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
    ///
    /// SEMANTICS — this is a NET (global) balance, the threshold-free discriminator
    /// proven on the rig (a real drop pushes avg step > 2.0, a real duplicate pulls
    /// it < 2.0). A single stale-copy that is EXACTLY cancelled by a distant dropped
    /// frame nets to surplus 0 and reads as balanced — two independent random faults
    /// cancelling exactly over a 300 s / 9000-frame window is vanishingly unlikely,
    /// and any non-cancelling residue still surfaces in `real_copy_frames` /
    /// `real_gap_frames`. A stronger gate would also assert the LOCAL 0↔4 / 1↔3 pair
    /// balance; the net gate is the rig-validated method and is what #107 specifies.
    pub beat_balanced: bool,
    /// Camera frames with no CRC-valid QR (undecodable) — always a FAIL, 0 tol.
    pub undecodable_frames: Vec<u64>,
    /// Camera frames carrying the NET stale-copy deficit (steps below the expected
    /// step, most-deviant first) — real duplication, FAIL. Names enough frames to
    /// account for the net deficit so the FAIL has pixel proof; the authoritative
    /// gate is `beat_balanced` (net), not this list's emptiness.
    pub real_copy_frames: Vec<u64>,
    /// Camera frames carrying the NET gap surplus (steps above the expected step, or
    /// a backward jump, most-deviant first) — real loss / reorder, FAIL. Same
    /// net-accounting + pixel-proof contract as `real_copy_frames`.
    pub real_gap_frames: Vec<u64>,
    /// Leading PRE-SIGNAL frames discarded before analysis: the run of `None`
    /// (no-QR) frames at the very FRONT of the recording, before the first decodable
    /// frame. These are the console lead-in (the painter has not yet taken cam2's
    /// monitor / strih's program does not yet carry the QR) — NOT pipeline loss. They
    /// are trimmed from the undecodable set and the analyzed span (the leading-discard
    /// window). Reported for honesty so a run can show how much lead-in was excluded.
    pub lead_in_trimmed: usize,
    /// Trailing POST-SIGNAL frames discarded before analysis: the run of `None` frames
    /// at the very END (teardown — the painter/source already removed while the
    /// recorder is still rolling). Symmetric to `lead_in_trimmed`.
    pub lead_out_trimmed: usize,
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
    // Leading-discard window (run-163163 regression): a recording always opens with
    // a few PRE-SIGNAL frames and may close with a few POST-SIGNAL ones. At the front
    // the painter has not yet taken cam2's monitor (the console is still showing) /
    // strih's program does not yet carry the QR; at the back the painter/source is
    // already removed while the recorder is still rolling (teardown). Those frames are
    // `None` (no QR), but they are NOT pipeline loss — they exist BEFORE/AFTER the
    // signal does. Trim the leading and trailing run of `None` frames so they are
    // never counted as undecodable faults nor inflate the analyzed span. An undecodable
    // hole INSIDE the signal body (between two decodable frames) is untouched and stays
    // a hard fault — only the pre/post-signal lead-in/out is discarded.
    let lead_in_trimmed = frames.iter().take_while(|f| f.tick.is_none()).count();
    let lead_out_trimmed = frames.iter().rev().take_while(|f| f.tick.is_none()).count();
    // The signal body. When the whole stream is `None` (no signal ever), both runs
    // cover it and the body is empty — handled below (no decodable frame ⇒ never PASS).
    let body: &[FrameTick] = if lead_in_trimmed >= frames.len() {
        &[]
    } else {
        &frames[lead_in_trimmed..frames.len() - lead_out_trimmed]
    };

    let total_frames = body.len();
    let analyzed_secs = if cfg.capture_fps > 0.0 {
        total_frames as f64 / cfg.capture_fps
    } else {
        0.0
    };
    let duration_ok = analyzed_secs >= cfg.min_secs;

    // Undecodable: any camera frame with no CRC-valid QR WITHIN the signal body
    // (interior holes only — the pre/post-signal lead-in/out is already trimmed).
    // Always a hard fault.
    let undecodable_frames: Vec<u64> = body
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
    for f in body {
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
        lead_in_trimmed,
        lead_out_trimmed,
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

/// A per-hop loss verdict paired on the **digital burn id** — the clean 1:1 common key
/// (#174). Unlike [`strih_stream_verdict`], which compares the OPTICAL cam2 Vernier tick
/// SETS (the un-genlocked 60→30 beat is NOT 1:1 across two independent samplings, so it
/// manufactured the 259-vs-1 "dropped" artifact in run 1530670109), this compares the
/// render-tick burn `frame_id` SETS. Each node stamps a monotonic per-render burn id; a
/// hop is lossless iff every upstream burn id within the overlap span reaches downstream.
/// The burn id is the SAME number end-to-end (the upstream burn rides through unchanged),
/// so the compare is an exact set equality with no beat ambiguity.
#[derive(Debug, Clone, Serialize)]
pub struct BurnHopVerdict {
    /// Hop label, e.g. `"cam1→strih"` or `"strih→stream"`.
    pub hop: String,
    /// Distinct upstream burn ids compared — the overlap span present in BOTH sides.
    pub compared_ids: usize,
    /// Upstream burn ids in the overlap span ABSENT downstream — frames the hop DROPPED.
    pub dropped_ids: Vec<u32>,
    /// Downstream burn ids in the overlap span ABSENT upstream — phantom / reorder ids
    /// downstream carries that upstream never rendered.
    pub phantom_ids: Vec<u32>,
    /// #175: flagged ticks RECLASSIFIED as single-frame burn-DECODE MISSES (not real hop
    /// drops) and removed from `dropped_ids`/`phantom_ids` — the absent node's burn counter
    /// was contiguous across the tick, proving it rendered (the corner QR just didn't decode
    /// in that one 4K-scaled recorded frame). These were folded back into `compared_ids`.
    /// Only [`crate::probe::recording_latency::chain_hop_loss_from_stream`] sets this; the
    /// raw burn-id [`burn_hop_verdict`] leaves it 0 (no per-node counter to test).
    #[serde(default)]
    pub decode_miss_excluded: usize,
}

impl BurnHopVerdict {
    /// PASS = the two sides carry the SAME burn-id set across their overlap, and the
    /// overlap is non-vacuous (≥2 ids — a 0/1-id overlap cannot demonstrate a clean hop).
    pub fn is_pass(&self) -> bool {
        self.dropped_ids.is_empty() && self.phantom_ids.is_empty() && self.compared_ids >= 2
    }
}

/// SHARED set-difference verdict over the OVERLAP span (the single source of truth for
/// the loss arithmetic — #181 review). Given the upstream and downstream id SETS (the
/// ids may be burn `frame_id`s — [`burn_hop_verdict`] — or shared cam2 source ticks —
/// [`crate::probe::recording_latency::chain_hop_loss_from_stream`]), restricts the
/// compare to `[max(first), min(last)]` so independent record start/stop skew is not
/// counted as loss, then: an upstream id absent downstream is a DROP; a downstream id
/// absent upstream is a phantom; an id present on BOTH sides is compared (survived).
/// Both callers build their own set and share THIS arithmetic so the two paths can never
/// diverge on the span / pass semantics.
pub fn overlap_set_verdict(
    hop: &str,
    up: &std::collections::BTreeSet<u32>,
    down: &std::collections::BTreeSet<u32>,
) -> BurnHopVerdict {
    let (lo, hi) = match (
        up.iter().next().max(down.iter().next()),
        up.iter().next_back().min(down.iter().next_back()),
    ) {
        (Some(&a), Some(&b)) if a <= b => (a, b),
        // Disjoint or one side empty: no overlap to compare. compared_ids=0 here means
        // "no shared span" — the caller logs the two set sizes so this is distinguishable
        // from a genuine all-dropped hop (#181 review: log which branch decided it).
        _ => {
            return BurnHopVerdict {
                hop: hop.to_string(),
                compared_ids: 0,
                dropped_ids: Vec::new(),
                phantom_ids: Vec::new(),
                decode_miss_excluded: 0,
            };
        }
    };
    let in_span = |t: u32| t >= lo && t <= hi;
    let dropped_ids: Vec<u32> = up
        .iter()
        .copied()
        .filter(|&t| in_span(t) && !down.contains(&t))
        .collect();
    let phantom_ids: Vec<u32> = down
        .iter()
        .copied()
        .filter(|&t| in_span(t) && !up.contains(&t))
        .collect();
    let compared_ids = up
        .iter()
        .filter(|&&t| in_span(t) && down.contains(&t))
        .count();
    BurnHopVerdict {
        hop: hop.to_string(),
        compared_ids,
        dropped_ids,
        phantom_ids,
        decode_miss_excluded: 0,
    }
}

/// Per-hop loss verdict by the clean digital burn id (#174). `upstream_ids` /
/// `downstream_ids` are the burn `frame_id`s decoded for the upstream and downstream node
/// (e.g. from the SINGLE stream recording: upstream = strih-burn ids, downstream =
/// stream-burn ids — both ride through into stream's program). Delegates to
/// [`overlap_set_verdict`] (shared span/drop/phantom arithmetic): an upstream id absent
/// downstream is a DROP; a downstream id absent upstream is a phantom. Offset-immune and
/// beat-free because the burn id is the SAME integer on both sides. NOTE: this is correct
/// ONLY when the two nodes share one id namespace; each node's INDEPENDENT burn counter
/// does NOT (#181) — for the from-stream full-chain hops use
/// [`crate::probe::recording_latency::chain_hop_loss_from_stream`], which keys on the
/// shared cam2 tick instead.
pub fn burn_hop_verdict(hop: &str, upstream_ids: &[u32], downstream_ids: &[u32]) -> BurnHopVerdict {
    use std::collections::BTreeSet;
    let up: BTreeSet<u32> = upstream_ids.iter().copied().collect();
    let down: BTreeSet<u32> = downstream_ids.iter().copied().collect();
    overlap_set_verdict(hop, &up, &down)
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
/// frame the stream output DROPPED; a stream-only tick is a frame stream has that
/// strih does not (a phantom id, or a reorder that carried a tick across the span
/// boundary). Undecodable frames (`None`) carry no tick and are excluded here — each
/// is already a hard fault in its own recording's [`verdict`].
///
/// SCOPE — this set compare catches DROPS and phantoms on the hop. Two cases it does
/// NOT catch, both handled elsewhere or unreachable on this pipeline: (1) a stream
/// FIFO underrun (stream serves the same tick twice) collapses in the set but is
/// caught by the stream recording's OWN [`verdict`] (its net surplus goes negative —
/// see the binary, which runs `verdict()` on the stream recording too); (2) a
/// same-span, same-magnitude REORDER yields neither a strih-only nor a stream-only
/// tick — but OBS/NDI program output is a strict in-order FIFO, not a reordering
/// packet network, so true frame reorder does not occur on this hop (the
/// per-recording backward-jump check covers any local reorder regardless).
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
    fn leading_console_lead_in_is_discarded_not_counted_undecodable() {
        // A recording always opens with a few PRE-SIGNAL frames: the painter has not
        // yet taken cam2's monitor (the console is still showing), or strih's program
        // does not yet carry the QR. Those frames are `None` (no QR), but they are NOT
        // pipeline loss — they are the lead-in BEFORE the signal exists. They must be
        // discarded (leading-discard window), never counted as undecodable faults.
        // Regression: run-163163 showed console lead-in frames as undecodable pixel
        // proof + FAIL, defeating the zero-loss verdict on a clean run.
        let frames = vec![
            ft(0, None), // console lead-in (painter not up)
            ft(1, None), // console lead-in
            ft(2, None), // console lead-in
            ft(3, Some(0)),
            ft(4, Some(2)),
            ft(5, Some(4)),
            ft(6, Some(6)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(
            v.undecodable_frames.is_empty(),
            "leading console lead-in must not be counted as undecodable: {:?}",
            v.undecodable_frames
        );
        assert_eq!(
            v.lead_in_trimmed, 3,
            "3 console frames trimmed from the front"
        );
        assert!((v.avg_step - 2.0).abs() < 1e-9);
        assert!(v.beat_balanced);
        assert!(
            v.is_pass(),
            "a clean body after a console lead-in must PASS"
        );
    }

    #[test]
    fn trailing_teardown_lead_out_is_discarded_too() {
        // Symmetric to the lead-in: the recording can capture a few `None` frames at
        // the END (teardown — the painter/source already removed but the recorder is
        // still rolling). Those are post-signal, not loss; trim them too.
        let frames = vec![
            ft(0, Some(0)),
            ft(1, Some(2)),
            ft(2, Some(4)),
            ft(3, None), // teardown lead-out
            ft(4, None), // teardown lead-out
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert!(
            v.undecodable_frames.is_empty(),
            "trailing teardown lead-out must not be counted undecodable: {:?}",
            v.undecodable_frames
        );
        assert_eq!(v.lead_out_trimmed, 2);
        assert!(v.is_pass());
    }

    #[test]
    fn interior_undecodable_still_fails_after_lead_in_trim() {
        // The trim is ONLY the leading/trailing PRE/POST-signal run. An undecodable
        // hole INSIDE the signal body is still a hard fault — the trim must not mask a
        // mid-run decode collapse.
        let frames = vec![
            ft(0, None), // lead-in (trimmed)
            ft(1, Some(0)),
            ft(2, None), // INTERIOR undecodable — a real fault
            ft(3, Some(4)),
            ft(4, Some(6)),
        ];
        let cfg = VerdictConfig {
            min_secs: 0.0,
            ..VerdictConfig::default()
        };
        let v = verdict(&frames, &cfg);
        assert_eq!(v.lead_in_trimmed, 1);
        assert_eq!(
            v.undecodable_frames,
            vec![2],
            "interior undecodable is still a fault"
        );
        assert!(!v.is_pass(), "an interior undecodable still fails");
    }

    #[test]
    fn analyzed_secs_excludes_trimmed_lead_frames() {
        // The analyzed span (duration gate) is measured over the SIGNAL body, not the
        // lead-in/lead-out — otherwise the console frames inflate the analyzed seconds.
        let mut frames = vec![ft(0, None), ft(1, None)];
        for i in 0..300u64 {
            frames.push(ft(2 + i, Some((i as u32) * 2)));
        }
        frames.push(ft(302, None)); // lead-out
        let cfg = VerdictConfig {
            capture_fps: 30.0,
            min_secs: 0.0,
            refresh_hz: 60.0,
        };
        let v = verdict(&frames, &cfg);
        // 300 signal frames @ 30 fps = 10.0 s (NOT 303/30 = 10.1).
        assert!(
            (v.analyzed_secs - 10.0).abs() < 1e-9,
            "analyzed_secs must be over the signal body only, got {}",
            v.analyzed_secs
        );
        assert_eq!(v.lead_in_trimmed, 2);
        assert_eq!(v.lead_out_trimmed, 1);
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

    // ---- #174 clean burn-id hop pairing (fixes the 60→30-beat loss artifacts) ----

    #[test]
    fn burn_hop_equal_id_sets_pass_offset_immune() {
        // Two nodes carrying the SAME render-tick burn ids (downstream started one id
        // later — record-start skew). The overlap id set is identical ⇒ clean hop, no
        // beat ambiguity because the burn id is the SAME integer on both sides.
        let up = vec![100, 101, 102, 103, 104];
        let down = vec![101, 102, 103, 104]; // started one render later
        let v = burn_hop_verdict("strih→stream", &up, &down);
        assert!(v.is_pass(), "identical overlap burn-id set ⇒ clean hop");
        assert!(v.dropped_ids.is_empty());
        assert!(v.phantom_ids.is_empty());
        assert!(v.compared_ids >= 2);
    }

    #[test]
    fn burn_hop_a_real_drop_is_named_not_a_beat_artifact() {
        // Downstream is MISSING burn id 102 that upstream rendered (within the overlap).
        // A REAL hop drop — named exactly, with NO inflation from the 60→30 oversample
        // (the artifact strih_stream_verdict produced: 259 dropped while real_gap=1).
        let up = vec![100, 101, 102, 103, 104];
        let down = vec![100, 101, 103, 104]; // 102 dropped
        let v = burn_hop_verdict("strih→stream", &up, &down);
        assert!(!v.is_pass(), "a real burn-id drop must FAIL");
        assert_eq!(v.dropped_ids, vec![102], "exactly the one dropped id");
        assert!(v.phantom_ids.is_empty());
    }

    #[test]
    fn burn_hop_oversampled_repeats_do_not_inflate_loss() {
        // THE artifact reproduction: the 60→30 beat places one render id on SEVERAL
        // recorded frames, and the two sides catch DIFFERENT members of each cluster.
        // With the optical tick set this manufactured hundreds of false "dropped".
        // With the burn id — the SAME integer regardless of how many frames carry it —
        // the set compare collapses the duplicates and reports ZERO loss.
        let up = vec![10, 10, 11, 11, 11, 12, 13, 13];
        let down = vec![10, 11, 12, 12, 13, 13, 13]; // different per-cluster sampling
        let v = burn_hop_verdict("strih→stream", &up, &down);
        assert!(
            v.is_pass(),
            "oversampled repeats of the SAME burn id are not loss: {v:?}"
        );
        assert!(v.dropped_ids.is_empty(), "no false drops from the beat");
        assert!(v.phantom_ids.is_empty());
    }

    #[test]
    fn burn_hop_phantom_downstream_id_fails() {
        // Downstream shows burn id 105 upstream never rendered, WITHIN the overlap span
        // (both sides span 100..=110, so 105 is inside) — a phantom / reorder. The strict
        // hop must FAIL and name it.
        let up = vec![100, 101, 102, 103, 110];
        let down = vec![100, 101, 105, 102, 103, 110]; // 105 phantom, inside [100,110]
        let v = burn_hop_verdict("cam1→strih", &up, &down);
        assert!(!v.is_pass(), "a phantom downstream id must FAIL");
        assert_eq!(v.phantom_ids, vec![105]);
    }

    #[test]
    fn burn_hop_disjoint_ranges_are_no_overlap_not_a_pass() {
        // cam1→strih in run 1530670109 collapsed to compared_ticks=1 because the optical
        // tick ranges barely overlapped. With burn ids, a genuinely disjoint range yields
        // compared_ids=0 (honest "no overlap"), never a misleading 1-id "compare".
        let up = vec![1, 2, 3];
        let down = vec![100, 101, 102];
        let v = burn_hop_verdict("cam1→strih", &up, &down);
        assert_eq!(v.compared_ids, 0, "disjoint ⇒ no overlap");
        assert!(!v.is_pass(), "no overlap cannot be a pass");
    }
}
