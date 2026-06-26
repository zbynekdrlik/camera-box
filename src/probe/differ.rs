//! Per-hop NDI-output differencing: detect real single-frame loss between two
//! NDI taps sharing one upstream capture. Pure / unit-tested.
//!
//! Both taps are downstream of the same HDMI/ShadowCast resample, so the
//! resample cancels: `IDs(upstream) − IDs(downstream)` is a clean count of the
//! frames that the hop between the two taps actually dropped — the single-frame
//! accuracy Phase 1's single point could not provide.

use crate::probe::analyzer::{
    detect_freezes, detect_reorders, latency_freeze_gate_pass, latency_stats, Freeze, LatencyStats,
    Observed,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub struct HopInput<'a> {
    pub name: String,
    pub upstream: &'a [Observed],
    pub downstream: &'a [Observed],
    pub capture_fps: f64,
    pub freeze_periods: f64,
    /// A tap that saw fewer than this many run_id-matching frames is treated as
    /// disconnected — the hop FAILS rather than vacuously passing on no data.
    pub min_frames: usize,
    /// Hard gate: fail this hop if its per-hop relative-latency `p99_ms` exceeds
    /// this. `None` ⇒ report-only (the Phase-2 default), mirroring the Phase-1
    /// #10 `max_p99_latency_ms` convention. Strict `>` — a value exactly at the
    /// bound passes.
    pub max_p99_latency_ms: Option<f64>,
    /// Hard gate: fail this hop if any detected freeze's `repeat_count` exceeds
    /// this. `None` ⇒ report-only. Strict `>`.
    pub max_freeze_periods_gate: Option<f64>,
    /// Loss-gate mode. `None` ⇒ STRICT zero-loss: the hop fails on ANY in-span
    /// dropped id (the Phase-2 default, correct for genlocked / local hops).
    /// `Some(pct)` ⇒ DOCUMENTED-BOUND mode: the hop is judged by its
    /// oversample-independent single-copy frame-loss percentage and passes while
    /// that stays `<= pct`. This is for hops with a known, quantified, currently
    /// irreducible loss (e.g. strih→stream's OBS render-clock drop pending
    /// genlock, #8) — it accepts the documented floor yet still catches a
    /// regression past it. `dropped_ids` is always reported either way.
    pub max_loss_pct: Option<f64>,
    /// Oversample-masking guard (#29): the minimum number of single-copy
    /// (oversample-independent) frames the run must contain before a passed loss
    /// gate may be CERTIFIED. The painter runs sub-fps, so each unique id is
    /// oversampled and a dropped id only counts when ALL its copies are lost —
    /// a high-oversample run can show `dropped_ids` empty (or single-copy loss
    /// under bound) while the pipeline is really dropping frames. When the loss
    /// gate passes but `single_copy_total < min_single_copy`, there is too little
    /// oversample-independent evidence to trust the green, so the verdict is
    /// `Inconclusive`, not `Pass`. `0` disables the guard (the default — a hop
    /// that did not opt in keeps its prior pass/fail behaviour).
    pub min_single_copy: usize,
}

/// A hop's certification outcome. Three states, because "not a clean pass" is not
/// the same as "a proven regression": a `Fail` means the hop genuinely dropped
/// frames / reordered / breached a bound; an `Inconclusive` means every gate
/// passed but the run lacked enough oversample-independent (single-copy) frames
/// to TRUST a zero/within-bound loss verdict (#29). Both keep the hop out of a
/// green CI run (`is_pass()` is false for either), but an operator must be able
/// to tell "the pipeline is broken" from "the harness needs more samples".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HopVerdict {
    Pass,
    Fail,
    Inconclusive,
}

impl HopVerdict {
    /// True only for `Pass` — a certified clean hop. `Fail` and `Inconclusive`
    /// both return false, so neither can sneak into a green overall verdict.
    pub fn is_pass(self) -> bool {
        matches!(self, HopVerdict::Pass)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HopReport {
    pub name: String,
    pub upstream_unique: usize,
    pub downstream_unique: usize,
    pub dropped_ids: Vec<u32>,
    pub reorders: Vec<(u32, u32)>,
    pub freezes: Vec<Freeze>,
    /// Per-hop latency from dev1 `recv−recv` first-occurrence. RETAINED for
    /// continuity but NOT trustworthy: it conflates real transit with per-stream
    /// NDI-receiver buffer jitter and can go negative. Prefer `emit_latency`.
    pub latency: Option<LatencyStats>,
    /// Per-hop latency from per-NODE EMIT timecodes (`per_hop_emit_latency`) — the
    /// trustworthy figure when both taps' sources stamp the NDI timecode on the
    /// shared DanteSync clock. `None` when the upstream/downstream source does not
    /// stamp a usable timecode (no id pairs with stamps on both sides).
    pub emit_latency: Option<LatencyStats>,
    /// Upstream ids within the downstream active span carried by exactly ONE
    /// frame (no oversample redundancy). Their loss rate is the unmasked,
    /// oversample-independent estimate of the hop's per-frame drop probability —
    /// the figure that would apply to real 60 fps content where every frame is
    /// unique. A trustworthy zero-loss verdict needs a meaningful count here; a
    /// run with very few single-copy frames cannot certify zero-loss.
    pub single_copy_total: usize,
    /// Of `single_copy_total`, how many are absent downstream.
    pub single_copy_dropped: usize,
    /// `Pass` / `Fail` / `Inconclusive` — see `HopVerdict`. Replaces the old
    /// `pass: bool` so an oversample-masked, under-sampled run reads as
    /// `Inconclusive` rather than a falsely green `Pass` (#29).
    pub verdict: HopVerdict,
}

/// Trailing settle-window cutoff for the tap-recv timestamps.
///
/// `stop_ns` MUST be in the SAME clock domain as each `Observed.recv_ts_ns`
/// (both monotonic for the Phase-1/relative path, or both CLOCK_REALTIME epoch
/// for the #7 wall-clock path). Frames received within `settle_ms` of the stop
/// are still in flight through the pipeline and are NOT drops, so they are
/// excluded from the loss check by keeping only `recv_ts_ns <= cutoff`.
///
/// Bug #63 fix: the binary previously derived `stop_ns` from `Instant::elapsed`
/// (monotonic, ~1e10 ns) while wall-clock taps stamp `recv_ts_ns` on
/// CLOCK_REALTIME (epoch, ~1.8e18 ns). The mismatched cutoff rejected EVERY
/// wall-clock frame, zeroing `unique` even when frames decoded fine — making a
/// fixed live OBS ingest still read as 0 decoded. The cutoff must be taken in
/// the recv-timestamp domain.
pub fn settle_cutoff_ns(stop_ns: i64, settle_ms: u64) -> i64 {
    stop_ns - (settle_ms as i64) * 1_000_000
}

/// Keep only the frames received at or before `cutoff_ns` (drop the trailing
/// in-flight settle window). Pure so the clock-domain contract above is testable
/// without a live NDI rig.
pub fn trim_to_settle(observed: &[Observed], cutoff_ns: i64) -> Vec<Observed> {
    observed
        .iter()
        .filter(|o| o.recv_ts_ns <= cutoff_ns)
        .cloned()
        .collect()
}

/// #68 Task C: the global earliest `recv_ts_ns` across ALL taps — the instant the
/// run's very first frame landed at any tap. The leading-discard window is
/// measured from this single origin (not per-tap), so every tap drops the SAME
/// wall-clock prefix and the hops stay comparable. `None` when no tap saw a
/// frame. Same clock-domain contract as `settle_cutoff_ns`: every `recv_ts_ns`
/// must share one domain (all monotonic, or all CLOCK_REALTIME with
/// `--wall-clock`).
pub fn earliest_recv_ns(taps: &[&[Observed]]) -> Option<i64> {
    taps.iter()
        .flat_map(|t| t.iter())
        .map(|o| o.recv_ts_ns)
        .min()
}

/// #68 Task C: the leading-discard cutoff — frames received BEFORE this instant
/// are the post-reset prime window and are dropped. `earliest_ns` is the run's
/// global earliest recv (`earliest_recv_ns`); `discard_ms` is the window length in
/// MILLISECONDS (mirrors `settle_cutoff_ns`'s ms unit). A frame received exactly
/// at the cutoff is already steady-state (kept by `trim_after`'s inclusive `>=`).
pub fn lead_cutoff_ns(earliest_ns: i64, discard_ms: u64) -> i64 {
    earliest_ns + (discard_ms as i64) * 1_000_000
}

/// #68 Task C: keep only the frames received AT or AFTER `cutoff_ns` (drop the
/// leading post-reset prime window). The mirror of `trim_to_settle` (which drops
/// the trailing in-flight window); applied together they leave only the
/// steady-state middle. Inclusive `>=` so a frame exactly at the cutoff — the
/// first steady-state frame — is kept. `cutoff_ns == 0` (zero discard, or no
/// earliest) keeps everything, since all real recv timestamps are positive. Pure
/// so the clock-domain contract is testable without a live NDI rig.
pub fn trim_after(observed: &[Observed], cutoff_ns: i64) -> Vec<Observed> {
    observed
        .iter()
        .filter(|o| o.recv_ts_ns >= cutoff_ns)
        .cloned()
        .collect()
}

fn first_recv(observed: &[Observed]) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for o in observed {
        m.entry(o.frame_id).or_insert(o.recv_ts_ns);
    }
    m
}

/// Earliest per-NODE EMIT timecode (ns since epoch) for each id at this tap,
/// ignoring frames with no usable stamp (`node_emit_tc_ns == 0`). The earliest
/// emit of an id is the instant the node first put that frame on the wire; pairing
/// the two taps' earliest emits gives the first-delivery transit for the hop.
fn first_emit_tc(observed: &[Observed]) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for o in observed {
        if o.node_emit_tc_ns == 0 {
            continue;
        }
        m.entry(o.frame_id)
            .and_modify(|e| {
                if o.node_emit_tc_ns < *e {
                    *e = o.node_emit_tc_ns;
                }
            })
            .or_insert(o.node_emit_tc_ns);
    }
    m
}

/// Per-hop latency from per-NODE EMIT timecodes — the TRUSTWORTHY method.
///
/// For each id present at BOTH taps with a usable emit stamp,
/// `downstream_emit − upstream_emit` on the shared DanteSync clock = true
/// node-to-node transit. Unlike the dev1 `recv−recv` delta (`diff_hop`'s
/// `latency`), this cannot go negative from per-stream NDI-receiver buffer
/// asymmetry, because both numbers are EMIT instants stamped at the nodes
/// themselves, not arrival instants at a shared central receiver — a negative
/// here would be a genuine cross-node clock-sync breach. Returns `None` when no
/// id pairs with usable stamps on both sides (e.g. a source that never stamps a
/// timecode), so a tap without timecodes degrades to "no per-hop latency", never
/// a wrong number.
pub fn per_hop_emit_latency(
    upstream: &[Observed],
    downstream: &[Observed],
) -> Option<LatencyStats> {
    let up = first_emit_tc(upstream);
    let down = first_emit_tc(downstream);
    let mut deltas: Vec<f64> = Vec::new();
    for (id, d_emit) in &down {
        if let Some(u_emit) = up.get(id) {
            deltas.push((d_emit - u_emit) as f64 / 1_000_000.0);
        }
    }
    latency_stats(&deltas)
}

/// Per-TAP ABSOLUTE emit latency: for each decoded frame, `node_emit_tc_ns −
/// gen_ts_ns` — the time from the painter's PAINT instant (`gen_ts`, carried in
/// the QR, identical at every tap) to THIS node's EMIT of that frame, both on the
/// shared DanteSync clock. Anchored on the per-frame `gen_ts`, so it needs NO
/// cross-tap pairing and is immune to the oversample first-occurrence ambiguity
/// that corrupts dev1 `recv−recv`. The cam tap's value is paint→camera-NDI-emit
/// (rig); strih's is paint→strih-emit; stream's is paint→stream-emit. A clean
/// per-hop latency is the difference of two adjacent taps' stats here. Skips
/// frames with no emit stamp (`node_emit_tc_ns == 0`) or no gen stamp
/// (`gen_ts_ns == 0`); `None` when none qualify.
///
/// REQUIRES the wall-clock domain (`gen_ts` stamped on CLOCK_REALTIME via the
/// painter's `--wall-clock`): `node_emit_tc_ns` is always NDI epoch-100ns, so a
/// monotonic `gen_ts` (the non-wall-clock path) would make the difference
/// meaningless. Callers gate this on `--wall-clock` (the `0`-is-unstamped sentinel
/// is also only safe for the huge epoch-domain stamps, never a monotonic ~0).
pub fn abs_emit_latency(observed: &[Observed]) -> Option<LatencyStats> {
    let deltas: Vec<f64> = observed
        .iter()
        .filter(|o| o.node_emit_tc_ns != 0 && o.gen_ts_ns != 0)
        .map(|o| (o.node_emit_tc_ns - o.gen_ts_ns) as f64 / 1_000_000.0)
        .collect();
    latency_stats(&deltas)
}

pub fn diff_hop(input: HopInput) -> HopReport {
    let up_unique: HashSet<u32> = input.upstream.iter().map(|o| o.frame_id).collect();
    let down_unique: HashSet<u32> = input.downstream.iter().map(|o| o.frame_id).collect();

    // A frame counts as dropped at this hop only if it falls within the
    // downstream tap's *active span* [first id, last id] yet is absent there.
    // The two taps connect and disconnect independently — the OBS-forwarded tap
    // establishes its NDI receive seconds after the direct tap — so ids the
    // downstream tap had no chance to see (before its first decode or after its
    // last) are tap start/stop skew, not hop drops. This is the id-granular
    // complement to the trailing settle-window the orchestrator already applies.
    // A real mid-stream drop still lies inside [lo, hi] and is still flagged.
    let dropped_ids: Vec<u32> = match (down_unique.iter().min(), down_unique.iter().max()) {
        (Some(&lo), Some(&hi)) => {
            let mut v: Vec<u32> = up_unique
                .iter()
                .copied()
                .filter(|id| *id >= lo && *id <= hi && !down_unique.contains(id))
                .collect();
            v.sort_unstable();
            v
        }
        _ => Vec::new(),
    };

    // Single-copy (no-oversample) per-frame loss: of the upstream ids carried by
    // exactly one frame AND lying within the downstream active span, how many are
    // absent downstream. This is the oversample-independent per-frame drop
    // estimate — multi-copy ids survive whenever any one copy lands on a render
    // tick, so only single-copy ids expose the true drop probability.
    let mut up_mult: HashMap<u32, usize> = HashMap::new();
    for o in input.upstream {
        *up_mult.entry(o.frame_id).or_insert(0) += 1;
    }
    let (single_copy_total, single_copy_dropped) =
        match (down_unique.iter().min(), down_unique.iter().max()) {
            (Some(&lo), Some(&hi)) => {
                let singles = up_mult
                    .iter()
                    .filter(|(id, m)| **m == 1 && **id >= lo && **id <= hi);
                let total = singles.clone().count();
                let dropped = singles.filter(|(id, _)| !down_unique.contains(id)).count();
                (total, dropped)
            }
            _ => (0, 0),
        };
    let single_copy_loss_pct = if single_copy_total > 0 {
        100.0 * single_copy_dropped as f64 / single_copy_total as f64
    } else {
        0.0
    };

    let reorders = detect_reorders(input.downstream);
    let freezes = detect_freezes(input.downstream, input.capture_fps, input.freeze_periods);

    // Per-hop latency: downstream arrival − upstream arrival on dev1's single
    // clock, per id present in both taps. First occurrence of each id.
    let up_first = first_recv(input.upstream);
    let down_first = first_recv(input.downstream);
    let mut deltas: Vec<f64> = Vec::new();
    for (id, d_recv) in &down_first {
        if let Some(u_recv) = up_first.get(id) {
            deltas.push((d_recv - u_recv) as f64 / 1_000_000.0);
        }
    }
    let latency = latency_stats(&deltas);
    // The trustworthy per-hop latency: per-node EMIT timecode differences on the
    // shared DanteSync clock (None when the sources don't stamp a timecode).
    let emit_latency = per_hop_emit_latency(input.upstream, input.downstream);

    // Loss gate: STRICT zero (any in-span drop fails) by default; or, when a
    // documented per-hop bound is set, judged by the oversample-independent
    // single-copy loss percentage staying `<= max_loss_pct` (strict `<=`).
    // Reorder is always hard; latency-p99 and freeze are gated only when a
    // per-hop bound is set (None ⇒ report-only).
    let loss_ok = match input.max_loss_pct {
        None => dropped_ids.is_empty(),
        Some(bound) => single_copy_loss_pct <= bound,
    };
    let gates_ok = up_unique.len() >= input.min_frames
        && down_unique.len() >= input.min_frames
        && loss_ok
        && reorders.is_empty()
        && latency_freeze_gate_pass(
            &latency,
            &freezes,
            input.max_p99_latency_ms,
            input.max_freeze_periods_gate,
        );

    // Verdict (#29): a real gate breach is a `Fail`. Only when every gate passes
    // do we ask whether the green is TRUSTWORTHY — a passed loss gate certified
    // on too few oversample-independent (single-copy) frames is `Inconclusive`,
    // never `Pass`, so masking cannot manufacture a false green. `min_single_copy
    // == 0` keeps the guard off (default), so a hop that did not opt in still
    // reports `Pass`/`Fail` exactly as before.
    let verdict = if !gates_ok {
        HopVerdict::Fail
    } else if single_copy_total < input.min_single_copy {
        HopVerdict::Inconclusive
    } else {
        HopVerdict::Pass
    };

    HopReport {
        name: input.name,
        upstream_unique: up_unique.len(),
        downstream_unique: down_unique.len(),
        dropped_ids,
        reorders,
        freezes,
        latency,
        emit_latency,
        single_copy_total,
        single_copy_dropped,
        verdict,
    }
}

/// #7 Phase 3: the HEADLINE source→endpoint aggregate. Unlike a chain of
/// adjacent `diff_hop`s, this differences the SOURCE tap directly against the
/// final ENDPOINT tap, so the number is "did every frame the source tap saw
/// reach the last endpoint", not a sum of per-hop diffs that could each look
/// clean while the chain as a whole lost a frame (a drop at hop A whose id is
/// masked by oversample on the next hop but is genuinely absent end-to-end).
///
/// The full span is itself just a hop (source = upstream, endpoint = downstream),
/// so its verdict obeys the SAME contract as every other hop: strict zero-loss by
/// default, the documented single-copy bound when `max_loss_pct` is set, the
/// `min_frames` non-vacuous floor, and the `min_single_copy` INCONCL guard (#29).
/// This keeps the headline consistent with the per-hop diffs and, critically,
/// makes the documented-loss escape hatch (`--max-loss-pct`) apply end-to-end
/// instead of a strict full-span gate silently overriding a deliberately-relaxed
/// per-hop budget.
#[derive(Debug, Clone, Serialize)]
pub struct FullSpanReport {
    /// Unique source-emitted ids decoded at the source tap.
    pub source_unique: usize,
    /// Unique ids decoded at the final endpoint tap.
    pub endpoint_unique: usize,
    /// Source ids absent at the endpoint, clipped to the endpoint's active span
    /// (the same start/stop-skew handling as `diff_hop` — a frame the endpoint
    /// tap had no chance to see before its first or after its last decode is tap
    /// skew, not a chain drop). A real mid-stream loss still lies inside [lo,hi].
    pub dropped_ids: Vec<u32>,
    /// Source→endpoint single-copy (oversample-independent) frames within the span.
    pub single_copy_total: usize,
    /// Of `single_copy_total`, how many are absent at the endpoint.
    pub single_copy_dropped: usize,
    /// The full-span verdict, computed by `diff_hop` so it honours the SAME
    /// loss/min_frames/min_single_copy contract as the per-hop gates. `is_pass()`
    /// is the headline "every source frame reached the endpoint" certification.
    pub verdict: HopVerdict,
}

/// Bounds for the full-span (source→endpoint) gate. Mirrors the relevant
/// `diff_hop` knobs so the headline obeys the same contract; defaults (`None` /
/// `0`) reproduce strict zero-loss with no INCONCL guard.
pub struct FullSpanBounds {
    /// Non-vacuous floor: a source or endpoint tap with fewer than this many
    /// run-id frames FAILS rather than certifying off near-zero data.
    pub min_frames: usize,
    /// `None` ⇒ STRICT zero-loss. `Some(pct)` ⇒ documented-bound: judged by the
    /// oversample-independent single-copy loss percentage staying `<= pct` — the
    /// same escape hatch the per-hop endpoint gate uses for the OBS render-clock
    /// drop pending genlock (#8).
    pub max_loss_pct: Option<f64>,
    /// `#29` oversample-masking guard: certify (Pass) only with at least this many
    /// single-copy source→endpoint frames; below it the verdict is Inconclusive.
    pub min_single_copy: usize,
}

/// Difference the source tap against the final endpoint tap (the full span),
/// delegating to `diff_hop` (source = upstream, endpoint = downstream) so the
/// span-clip, single-copy, documented-bound and INCONCL semantics are IDENTICAL
/// to the per-hop diffs by construction — not a hand-kept parallel copy.
/// Latency/freeze are report-only here (`None` bounds): the full span's meaningful
/// latency is the ABSOLUTE one (`absolute_latency_stats`), not a relative recv−recv
/// delta, and freezes are localised per hop.
pub fn full_span_diff(
    source: &[Observed],
    endpoint: &[Observed],
    bounds: &FullSpanBounds,
) -> FullSpanReport {
    let hop = diff_hop(HopInput {
        name: "source→endpoint".to_string(),
        upstream: source,
        downstream: endpoint,
        // Freezes are localised per hop and never gated/reported for the full
        // span; a never-tripped threshold keeps detect_freezes a no-op without
        // touching the verdict. (1.0 fps avoids a 1/0 period; MAX is unreachable.)
        capture_fps: 1.0,
        freeze_periods: f64::MAX,
        min_frames: bounds.min_frames,
        max_p99_latency_ms: None,
        max_freeze_periods_gate: None,
        max_loss_pct: bounds.max_loss_pct,
        min_single_copy: bounds.min_single_copy,
    });
    FullSpanReport {
        source_unique: hop.upstream_unique,
        endpoint_unique: hop.downstream_unique,
        dropped_ids: hop.dropped_ids,
        single_copy_total: hop.single_copy_total,
        single_copy_dropped: hop.single_copy_dropped,
        verdict: hop.verdict,
    }
}

/// #68 Task B: the ENDPOINT tap's sequence checked against the GENERATOR's known
/// contiguous emission — not against another tap. The painter emits a strictly
/// monotonic, CONTIGUOUS id sequence (`0,1,2,…`), so for the span the endpoint
/// actually decoded `[first_id..=last_id]`, EVERY integer in that range was
/// generated. Therefore an integer absent from the endpoint is a real
/// generator→endpoint drop — and crucially this catches generator→cam loss that
/// the source-tap-vs-endpoint-tap difference (`full_span_diff`) can never see,
/// because that diff can only flag ids the SOURCE tap decoded. Out-of-order ids
/// (an id that arrives after a strictly higher id already passed) are real
/// reorders. Together this is the "every frame the QR generator emitted has
/// exactly one delivered counterpart at the OBS output, in order" check.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointSequenceReport {
    /// Lowest id decoded at the endpoint (start of the verified contiguous span).
    pub first_id: u32,
    /// Highest id decoded at the endpoint (end of the verified contiguous span).
    pub last_id: u32,
    /// Count of integers in `[first_id..=last_id]` — every one was generated.
    pub expected_count: usize,
    /// Distinct ids actually present at the endpoint within the span.
    pub delivered_count: usize,
    /// Generated ids in `[first_id..=last_id]` that NEVER reached the endpoint —
    /// real generator→endpoint drops (incl. generator→cam loss). Sorted ascending.
    pub missing_ids: Vec<u32>,
    /// Ids decoded out of monotonic order: each id that appears after a strictly
    /// higher id already passed (a genuine reorder, not an oversample duplicate of
    /// the current/previous id). Sorted ascending.
    pub out_of_order_ids: Vec<u32>,
}

impl EndpointSequenceReport {
    /// A clean endpoint: no internal gap, no reorder, and a NON-VACUOUS span
    /// (at least two distinct ids — a 0- or 1-id span cannot demonstrate
    /// contiguity, so it never certifies zero-loss).
    pub fn is_clean(&self) -> bool {
        self.delivered_count >= 2 && self.missing_ids.is_empty() && self.out_of_order_ids.is_empty()
    }
}

/// Check the endpoint tap's decoded stream against the generator's contiguous
/// emission (#68 Task B). Pure / unit-tested: the painter's contiguity is the
/// source of truth, so the implied generated set is exactly the integers in
/// `[min_decoded..=max_decoded]` and any absentee is a drop. `observed` is the
/// endpoint tap's frames IN CAPTURE ORDER (the order matters for reorder
/// detection); duplicates (oversample) and held ids are fine.
pub fn endpoint_sequence_check(observed: &[Observed]) -> EndpointSequenceReport {
    let present: HashSet<u32> = observed.iter().map(|o| o.frame_id).collect();
    let (first_id, last_id) = match (present.iter().min(), present.iter().max()) {
        (Some(&lo), Some(&hi)) => (lo, hi),
        _ => {
            return EndpointSequenceReport {
                first_id: 0,
                last_id: 0,
                expected_count: 0,
                delivered_count: 0,
                missing_ids: Vec::new(),
                out_of_order_ids: Vec::new(),
            };
        }
    };

    // Every integer in [first..=last] was generated (painter contiguity); flag the
    // absentees. `last - first + 1` is the count of generated ids in the span.
    let expected_count = (last_id - first_id) as usize + 1;
    let mut missing_ids: Vec<u32> = (first_id..=last_id)
        .filter(|id| !present.contains(id))
        .collect();
    missing_ids.sort_unstable();

    // Reorder: an id that arrives strictly below the highest id seen so far. An
    // oversample duplicate (== current running max) or a held id is NOT a reorder.
    // Each offending id is reported once.
    let mut out_of_order: HashSet<u32> = HashSet::new();
    let mut running_max: Option<u32> = None;
    for o in observed {
        match running_max {
            Some(m) if o.frame_id < m => {
                out_of_order.insert(o.frame_id);
            }
            _ => {
                running_max = Some(running_max.map_or(o.frame_id, |m| m.max(o.frame_id)));
            }
        }
    }
    let mut out_of_order_ids: Vec<u32> = out_of_order.into_iter().collect();
    out_of_order_ids.sort_unstable();

    EndpointSequenceReport {
        first_id,
        last_id,
        expected_count,
        delivered_count: present.len(),
        missing_ids,
        out_of_order_ids,
    }
}

/// #68: split the endpoint's missing ids (from `endpoint_sequence_check`) into the
/// two physically-distinct loss classes, using the SOURCE tap's decoded set:
///
/// - **emission artifact** — ids ABSENT at the source tap too: the QR rig never
///   put them on the source NDI in the first place. On the fb-loopback this is the
///   discrete-painter-into-genlock-decimation phase artifact (the painter writes a
///   new id every ~33 ms but the 60 Hz HDMI scanout + 60→30 wall-clock decimator
///   sample on their own boundaries, so ~5-13% of painted ids are never captured
///   into NDI). This is NOT pipeline loss and must not fail a pipeline zero-loss
///   gate. (On a synth-ndi source — no fb, no capture, no decimation — this set is
///   empty, proven live.)
/// - **pipeline loss** — ids PRESENT at the source tap but absent at the endpoint:
///   they reached the source NDI and then vanished downstream. THIS is real
///   source→endpoint loss, the only kind a zero-loss gate must fail on (and it
///   equals `full_span_diff`'s `dropped_ids` by construction).
///
/// Returns `(emission_artifact, pipeline_loss)`, each sorted ascending. Pure /
/// unit-tested so the harness can be HONEST: it reports the artifact (a QR-rig
/// limitation, not a stream defect) separately from genuine pipeline drops.
pub fn decompose_missing(endpoint_missing: &[u32], source: &[Observed]) -> (Vec<u32>, Vec<u32>) {
    let src_present: HashSet<u32> = source.iter().map(|o| o.frame_id).collect();
    let mut emission_artifact: Vec<u32> = Vec::new();
    let mut pipeline_loss: Vec<u32> = Vec::new();
    for &id in endpoint_missing {
        if src_present.contains(&id) {
            pipeline_loss.push(id); // was at source, lost downstream → real loss
        } else {
            emission_artifact.push(id); // never at source → rig emission artifact
        }
    }
    emission_artifact.sort_unstable();
    pipeline_loss.sort_unstable();
    (emission_artifact, pipeline_loss)
}

/// #7 Phase 3: ABSOLUTE end-to-end latency = `recv_ts(endpoint) − gen_ts(source)`
/// paired by `frame_id`. SOUND ONLY when both timestamps live on one synced wall
/// clock (DanteSync CLOCK_REALTIME, strih = master) — the painter must emit
/// `gen_ts_ns` as a wall-clock stamp and the endpoint tap must record
/// `recv_ts_ns` as a wall-clock stamp; this function is the pure arithmetic that
/// assumes the caller arranged that shared origin. It takes the SOURCE tap's
/// `gen_ts` for each frame (the true emission instant) and the ENDPOINT tap's
/// first `recv_ts` for the same frame. An id present at only one tap yields no
/// pair. `None` when no frame is common to both taps.
pub fn absolute_latency_stats(source: &[Observed], endpoint: &[Observed]) -> Option<LatencyStats> {
    // First gen_ts per id at the source (the emission instant), first recv_ts per
    // id at the endpoint (its arrival instant). First-occurrence so an oversample
    // duplicate cannot skew the pairing.
    let mut src_gen: HashMap<u32, i64> = HashMap::new();
    for o in source {
        src_gen.entry(o.frame_id).or_insert(o.gen_ts_ns);
    }
    let ep_recv = first_recv(endpoint);

    let mut lat_ms: Vec<f64> = Vec::new();
    for (id, recv) in &ep_recv {
        if let Some(gen) = src_gen.get(id) {
            lat_ms.push((recv - gen) as f64 / 1_000_000.0);
        }
    }
    latency_stats(&lat_ms)
}

/// Hard gate on the absolute end-to-end p99. `None` bound ⇒ report-only (always
/// passes), mirroring the per-hop `max_p99_latency_ms` convention. A requested
/// bound with NO samples FAILS — a gate that could not run must never report
/// green (test-strictness). Strict `>` so a p99 exactly at the bound passes.
///
/// A NEGATIVE measured latency (`min_ms < 0`, recv before gen) is physically
/// impossible — it can only mean the cluster wall clocks desynced past the
/// transit time (the camera clock ahead of dev1), so the whole measurement is
/// untrustworthy and the gate FAILS rather than passing on a number that may be a
/// large true latency masquerading as a small/negative one. The rig clock-sync
/// pre-flight (the DanteSync gate the e2e harness runs) is the first line of
/// defence; this is the backstop for a probe invoked directly without it.
pub fn absolute_latency_gate_pass(latency: &Option<LatencyStats>, max_p99_ms: Option<f64>) -> bool {
    match (max_p99_ms, latency) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(_), Some(l)) if l.min_ms < 0.0 => false,
        (Some(bound), Some(l)) => l.p99_ms <= bound,
    }
}

/// The overall run verdict, folding the per-hop verdicts, the source→endpoint
/// full-span verdict, and the absolute-latency gate into ONE outcome. This is the
/// central pass/fail logic the whole gate hinges on — kept pure and testable
/// rather than inline in `main()`, because an `&&`/`||` slip here (or a dropped
/// term) would let a run exit green while a gate is red. A run PASSES only when
/// EVERY per-hop verdict is `Pass`, the full-span verdict is `Pass`, and the
/// absolute-latency gate passes. Otherwise it FAILS if any hop or the full span
/// is a hard `Fail` or the absolute-latency gate failed (a proven regression),
/// else it is `Inconclusive` (gates passed but a hop/full-span lacked enough
/// single-copy evidence — #29; "need a longer/denser run", not "broken").
pub fn overall_verdict(
    hops: &[HopReport],
    full_span: &FullSpanReport,
    abs_gate_pass: bool,
) -> HopVerdict {
    let all_pass =
        hops.iter().all(|h| h.verdict.is_pass()) && full_span.verdict.is_pass() && abs_gate_pass;
    if all_pass {
        return HopVerdict::Pass;
    }
    let hard_fail = hops.iter().any(|h| h.verdict == HopVerdict::Fail)
        || full_span.verdict == HopVerdict::Fail
        || !abs_gate_pass;
    if hard_fail {
        HopVerdict::Fail
    } else {
        HopVerdict::Inconclusive
    }
}

/// Per-output 30 fps grid-continuity report. Checks a single output's decoded id
/// sequence over the span `[first_id..=last_id]`, stepping by `stride`, to detect
/// two failure classes:
///
/// - **missing_slots** — stride-aligned ids in the span absent from `observed`:
///   the output did not decode that slot at all (output starvation).
/// - **repeated_ids** — ids decoded more than once by this output (FIFO underrun:
///   the same frame was served on two consecutive decode calls).
///
/// `is_clean()` = no missing slots, no repeats, and at least 2 ids present
/// (a 0- or 1-id span cannot demonstrate continuity, so it never certifies clean).
#[derive(Debug, Clone, Serialize)]
pub struct GridContinuity {
    pub first_id: u32,
    pub last_id: u32,
    /// Number of stride-aligned slots in `[first_id..=last_id]`.
    pub expected: usize,
    /// Number of distinct ids present in `observed`.
    pub present: usize,
    /// Stride-aligned ids absent from `observed` (output starvation). Sorted ascending.
    pub missing_slots: Vec<u32>,
    /// Ids decoded more than once (FIFO underrun). Sorted ascending.
    pub repeated_ids: Vec<u32>,
}

impl GridContinuity {
    /// True only when there are no missing slots, no repeated ids, and at least
    /// two distinct ids — a 0- or 1-id span cannot certify continuity.
    pub fn is_clean(&self) -> bool {
        self.missing_slots.is_empty() && self.repeated_ids.is_empty() && self.present >= 2
    }
}

/// Check a single output's decoded id sequence for starvation (missing slots) and
/// FIFO underrun (repeated ids). `stride` is the logical-id step the output advances
/// per frame (1 for a 30 fps output on a 30 fps id sequence; 2 on a 60 Hz dual-QR
/// counter). Only multiples of `stride` within the span are checked for absence.
pub fn grid_continuity(observed: &[Observed], stride: u32) -> GridContinuity {
    if observed.is_empty() || stride == 0 {
        return GridContinuity {
            first_id: 0,
            last_id: 0,
            expected: 0,
            present: 0,
            missing_slots: Vec::new(),
            repeated_ids: Vec::new(),
        };
    }

    // Count occurrences of each id.
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for obs in observed {
        *counts.entry(obs.frame_id).or_insert(0) += 1;
    }

    let first_id = *counts.keys().min().unwrap();
    let last_id = *counts.keys().max().unwrap();

    // Stride-aligned slots: starting from first_id, every `stride` steps up to last_id.
    let slots: Vec<u32> = (first_id..=last_id).step_by(stride as usize).collect();
    let expected = slots.len();

    let mut missing_slots: Vec<u32> = slots
        .iter()
        .copied()
        .filter(|id| !counts.contains_key(id))
        .collect();
    missing_slots.sort_unstable();

    let mut repeated_ids: Vec<u32> = counts
        .iter()
        .filter(|(_, &cnt)| cnt > 1)
        .map(|(&id, _)| id)
        .collect();
    repeated_ids.sort_unstable();

    GridContinuity {
        first_id,
        last_id,
        expected,
        present: counts.len(),
        missing_slots,
        repeated_ids,
    }
}

/// Dual-QR Vernier net-loss proof (#105). The camera and capture card run at a
/// slightly different phase AND frequency, so the un-genlocked optical sampling of a
/// 60 Hz painter at a ~30 fps chain BEATS: consecutive endpoint frames step the decoded
/// id by ~2 on average, but with ±1 jitter as the sampling phase walks across the
/// painter's vblank boundary. The single-copy metric mistakes that beat for ~50% loss.
/// This metric SEPARATES the beat from real loss the way the scheme was designed for:
///
/// - **`avg_step` ≈ 2.0** — the chain samples every OTHER 60 Hz tick. A value below 2
///   means duplication/held frames advancing the id less than expected; above 2 means
///   skips. Real NET loss pulls the average AWAY from 2.0.
/// - **balanced jitter** — for each deviation `k` from step 2, `count(step = 2+k)` must
///   equal `count(step = 2−k)`. The +1/−1 (and ±2 …) deviations are the sampling phase
///   walking forward then back; if they CANCEL, no NET frame was lost, only the optical
///   sampling beat reordered which tick was caught. A one-sided imbalance (more +k than
///   −k) is the signature of REAL loss: the id leapt forward without a matching catch-up.
///
/// `net_imbalance` is the signed sum `Σ over k>0 of (count(2+k) − count(2−k)) · k` —
/// exactly the number of NET ids the endpoint advanced BEYOND `2·(advances)`. Zero ⇒
/// every forward beat was matched by an equal backward beat ⇒ ZERO net loss. It is
/// computed directly as `(last_id − first_id) − 2·advances`, which is identically the
/// balance sum (telescoping), so the two views agree by construction.
///
/// `observed` is the tap's frames IN CAPTURE ORDER. HELD ids (an id equal to the
/// running value — oversample of the same painted frame) are NOT steps and are
/// collapsed; only a CHANGE in the decoded id is a step. Steps may be negative (a
/// reorder/backward beat) — those are kept in the histogram (they are the −k side of
/// the balance) and are what makes a balanced run net to zero.
#[derive(Debug, Clone, Serialize)]
pub struct StepDistribution {
    /// First decoded id (start of the measured span).
    pub first_id: u32,
    /// Last decoded id (end of the measured span).
    pub last_id: u32,
    /// Number of id CHANGES between consecutive captured frames (held ids excluded).
    /// This is the count of forward+backward steps that make up the histogram.
    pub advances: usize,
    /// Mean id step per advance. ≈ 2.0 on a clean 60 Hz→30 fps dual-QR chain.
    pub avg_step: f64,
    /// Step histogram: `(step, count)` sorted by step ascending. `step` is the signed
    /// id delta between consecutive distinct captured ids (negative = backward beat).
    pub histogram: Vec<(i64, usize)>,
    /// Signed NET id advance beyond `2·advances`: `(last_id − first_id) − 2·advances`.
    /// 0 ⇒ perfectly balanced jitter ⇒ ZERO net loss. Positive ⇒ net forward drift
    /// (real loss: ids skipped without a matching backward beat). Negative ⇒ net
    /// duplication (the chain advanced the id LESS than every-other-tick).
    pub net_imbalance: i64,
    /// Per-deviation jitter balance: one row `(k, count(2+k), count(2−k))` for each
    /// k>0 present in the histogram. A row where the two counts are EQUAL is a balanced
    /// beat (no net loss at that deviation); an unequal row is one-sided drift.
    pub balance: Vec<(i64, usize, usize)>,
    /// `net_imbalance == 0`: the jitter is perfectly balanced, proving ZERO NET loss
    /// over the span — the residual ±step jitter is the un-genlocked optical sampling
    /// beat, not chain loss. The dual-QR Vernier PASS criterion.
    pub balanced: bool,
}

/// Collapse a tap's capture-order observations to the sequence of DISTINCT decoded ids
/// (held/repeated ids removed) — the input to the step analysis. A "held" id is one
/// equal to the immediately-preceding decoded value (the chain served the same painted
/// frame again, i.e. oversample), which is NOT a step. Keeps the order; a backward
/// change (reorder/beat) IS kept as a (negative) step.
fn distinct_id_sequence(observed: &[Observed]) -> Vec<u32> {
    let mut seq: Vec<u32> = Vec::new();
    for o in observed {
        if seq.last() != Some(&o.frame_id) {
            seq.push(o.frame_id);
        }
    }
    seq
}

/// Compute the dual-QR Vernier step distribution over a tap's capture-order frames.
/// Pure / unit-tested: this is the #105 net-loss proof — it judges zero NET loss from
/// the avg-step (≈2.0) + balanced jitter, NOT from single-copy counting (which the
/// 60→30 optical beat confounds). See [`StepDistribution`].
pub fn step_distribution(observed: &[Observed]) -> StepDistribution {
    let seq = distinct_id_sequence(observed);
    let (first_id, last_id) = match (seq.first(), seq.last()) {
        (Some(&f), Some(&l)) => (f, l),
        _ => {
            return StepDistribution {
                first_id: 0,
                last_id: 0,
                advances: 0,
                avg_step: 0.0,
                histogram: Vec::new(),
                net_imbalance: 0,
                balance: Vec::new(),
                balanced: true,
            };
        }
    };

    // Signed steps between consecutive distinct ids.
    let mut hist: HashMap<i64, usize> = HashMap::new();
    for w in seq.windows(2) {
        let step = w[1] as i64 - w[0] as i64;
        *hist.entry(step).or_insert(0) += 1;
    }
    let advances: usize = hist.values().sum();
    let step_sum: i64 = hist.iter().map(|(&s, &c)| s * c as i64).sum();
    let avg_step = if advances > 0 {
        step_sum as f64 / advances as f64
    } else {
        0.0
    };

    let mut histogram: Vec<(i64, usize)> = hist.iter().map(|(&s, &c)| (s, c)).collect();
    histogram.sort_unstable_by_key(|&(s, _)| s);

    // net_imbalance = total id advance beyond 2·advances. Equals Σ_{k>0} (count(2+k) −
    // count(2−k))·k by telescoping, so the histogram-balance view and this closed form
    // agree by construction; computed in closed form to avoid a re-summation mutant.
    let net_imbalance = step_sum - 2 * advances as i64;

    // Per-deviation balance rows for every k>0 that appears as 2+k or 2−k.
    let count = |s: i64| hist.get(&s).copied().unwrap_or(0);
    let mut ks: Vec<i64> = hist
        .keys()
        .map(|&s| (s - 2).abs())
        .filter(|&k| k > 0)
        .collect();
    ks.sort_unstable();
    ks.dedup();
    let balance: Vec<(i64, usize, usize)> = ks
        .iter()
        .map(|&k| (k, count(2 + k), count(2 - k)))
        .collect();

    StepDistribution {
        first_id,
        last_id,
        advances,
        avg_step,
        histogram,
        net_imbalance,
        balance,
        balanced: net_imbalance == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(frame_id: u32, recv_ms: i64) -> Observed {
        Observed {
            frame_id,
            gen_ts_ns: 0,
            recv_ts_ns: recv_ms * 1_000_000,
            node_emit_tc_ns: 0,
        }
    }

    /// Like [`o`], but also sets the per-node emit timecode (ms) — for the
    /// per-hop-from-emit-timecode tests.
    fn oe(frame_id: u32, recv_ms: i64, emit_ms: i64) -> Observed {
        Observed {
            frame_id,
            gen_ts_ns: 0,
            recv_ts_ns: recv_ms * 1_000_000,
            node_emit_tc_ns: emit_ms * 1_000_000,
        }
    }

    /// Sets the paint `gen_ts` (ms) and the per-node emit timecode (ms) — for the
    /// per-tap absolute-emit-latency tests.
    fn oe_gen(frame_id: u32, gen_ms: i64, emit_ms: i64) -> Observed {
        Observed {
            frame_id,
            gen_ts_ns: gen_ms * 1_000_000,
            recv_ts_ns: 0,
            node_emit_tc_ns: emit_ms * 1_000_000,
        }
    }

    fn input<'a>(up: &'a [Observed], down: &'a [Observed]) -> HopInput<'a> {
        HopInput {
            name: "cam→strih".to_string(),
            upstream: up,
            downstream: down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct: None,
            min_single_copy: 0,
        }
    }

    #[test]
    fn per_hop_emit_latency_is_downstream_minus_upstream_emit() {
        // up emits id1@100ms id2@200ms; down (next node) emits both +30ms.
        let up = [oe(1, 0, 100), oe(2, 0, 200)];
        let down = [oe(1, 0, 130), oe(2, 0, 230)];
        let l = per_hop_emit_latency(&up, &down).expect("paired stamps");
        assert_eq!(l.min_ms, 30.0);
        assert_eq!(l.p50_ms, 30.0);
        assert_eq!(l.max_ms, 30.0);
        // Emit-based latency is true transit: never negative for a real hop.
        assert!(l.min_ms >= 0.0);
    }

    #[test]
    fn per_hop_emit_latency_takes_earliest_emit_per_id() {
        // id1 oversampled upstream: emitted at 100 then 116 (earliest = 100);
        // downstream earliest at 140 -> transit = 40, not 24.
        let up = [oe(1, 0, 116), oe(1, 0, 100)];
        let down = [oe(1, 0, 140), oe(1, 0, 156)];
        let l = per_hop_emit_latency(&up, &down).expect("paired stamps");
        assert_eq!(l.min_ms, 40.0);
    }

    #[test]
    fn per_hop_emit_latency_ignores_unstamped_and_unpaired() {
        // id1 stamped both sides (+40); id2 unstamped downstream (o => emit_tc 0);
        // id3 only upstream. Only id1 contributes.
        let up = [oe(1, 0, 100), oe(3, 0, 100)];
        let down = [oe(1, 0, 140), o(2, 0)];
        let l = per_hop_emit_latency(&up, &down).expect("id1 pairs");
        assert_eq!(l.min_ms, 40.0);
        assert_eq!(l.max_ms, 40.0);
    }

    #[test]
    fn abs_emit_latency_is_node_emit_minus_gen() {
        // gen_ts 0 is the "unstamped" sentinel, so use non-zero paint times:
        // id1 gen 5ms emit 75ms -> 70; id2 gen 10ms emit 100ms -> 90.
        let obs = [oe_gen(1, 5, 75), oe_gen(2, 10, 100)];
        let l = abs_emit_latency(&obs).expect("stamped");
        assert_eq!(l.min_ms, 70.0);
        assert_eq!(l.max_ms, 90.0);
    }

    #[test]
    fn abs_emit_latency_skips_unstamped() {
        // o() has emit_tc 0 -> skipped; nothing qualifies.
        let obs = [o(1, 0), o(2, 0)];
        assert!(abs_emit_latency(&obs).is_none());
    }

    #[test]
    fn per_hop_emit_latency_none_when_no_stamps() {
        // No source stamps a timecode (all emit_tc 0) -> no per-hop emit latency,
        // never a wrong number.
        let up = [o(1, 0), o(2, 0)];
        let down = [o(1, 0), o(2, 0)];
        assert!(per_hop_emit_latency(&up, &down).is_none());
    }

    #[test]
    fn diff_hop_populates_emit_latency_from_timecodes() {
        let up = [oe(1, 0, 100), oe(2, 0, 200), oe(3, 0, 300)];
        let down = [oe(1, 0, 133), oe(2, 0, 233), oe(3, 0, 333)];
        let r = diff_hop(HopInput {
            name: "cam→strih".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct: None,
            min_single_copy: 0,
        });
        let e = r.emit_latency.expect("emit latency present");
        assert_eq!(e.p50_ms, 33.0);
        assert!(e.min_ms >= 0.0);
    }

    /// A hop whose downstream per-id relative latency p99 == `hi_ms`: four ids at
    /// 10 ms and one at `hi_ms`, so nearest-rank p99 over n=5 lands on `hi_ms`.
    /// `max_p99` is the latency gate under test; the freeze gate is left off.
    fn hop_with_p99(hi_ms: i64, max_p99: Option<f64>) -> HopReport {
        let up = vec![o(0, 0), o(1, 0), o(2, 0), o(3, 0), o(4, 0)];
        let down = vec![o(0, 10), o(1, 10), o(2, 10), o(3, 10), o(4, hi_ms)];
        diff_hop(HopInput {
            name: "cam→strih".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: max_p99,
            max_freeze_periods_gate: None,
            max_loss_pct: None,
            min_single_copy: 0,
        })
    }

    /// A hop whose downstream holds id 2 for `repeats` consecutive frames — a
    /// freeze of `repeat_count == repeats` (freeze_periods = 3 < repeats). No
    /// drop, no reorder, so the freeze gate is the only possible failure cause.
    fn hop_with_freeze(repeats: usize, max_freeze: Option<f64>) -> HopReport {
        let up = vec![o(0, 0), o(1, 1), o(2, 2), o(3, 3)];
        let mut down = vec![o(0, 10), o(1, 11)];
        for k in 0..repeats {
            down.push(o(2, 12 + k as i64));
        }
        down.push(o(3, 99));
        diff_hop(HopInput {
            name: "cam→strih".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: max_freeze,
            max_loss_pct: None,
            min_single_copy: 0,
        })
    }

    #[test]
    fn per_hop_latency_p99_over_bound_fails() {
        // p99 = 300 ms, bound 250 ms → FAIL despite zero loss/reorder.
        let r = hop_with_p99(300, Some(250.0));
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 300.0);
        assert!(r.dropped_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(
            !r.verdict.is_pass(),
            "p99 300 > bound 250 must FAIL the hop"
        );
    }

    #[test]
    fn per_hop_latency_p99_at_bound_passes() {
        // p99 = 250 ms, bound 250 ms → PASS (strict `>`, not `>=`).
        let r = hop_with_p99(250, Some(250.0));
        assert_eq!(r.latency.as_ref().unwrap().p99_ms, 250.0);
        assert!(
            r.verdict.is_pass(),
            "p99 250 == bound 250 must PASS (strict >)"
        );
    }

    #[test]
    fn per_hop_freeze_over_bound_fails() {
        // freeze repeat_count 6, bound 5 → FAIL despite zero loss/reorder.
        let r = hop_with_freeze(6, Some(5.0));
        assert_eq!(r.freezes.len(), 1);
        assert_eq!(r.freezes[0].repeat_count, 6);
        assert!(r.dropped_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(
            !r.verdict.is_pass(),
            "freeze repeat_count 6 > bound 5 must FAIL the hop"
        );
    }

    #[test]
    fn per_hop_freeze_at_bound_passes() {
        // freeze repeat_count 5, bound 5 → PASS (strict `>`, not `>=`).
        let r = hop_with_freeze(5, Some(5.0));
        assert_eq!(r.freezes[0].repeat_count, 5);
        assert!(
            r.verdict.is_pass(),
            "freeze repeat_count 5 == bound 5 must PASS (strict >)"
        );
    }

    #[test]
    fn none_bounds_are_report_only_phase2_default() {
        // High latency AND a real freeze, but both bounds None → report-only;
        // the hop still PASSes, preserving the Phase-2 loss+reorder-only default.
        let r_lat = hop_with_p99(9999, None);
        assert!(r_lat.latency.as_ref().unwrap().p99_ms > 250.0);
        assert!(
            r_lat.verdict.is_pass(),
            "None latency bound must stay report-only"
        );
        let r_frz = hop_with_freeze(99, None);
        assert!(!r_frz.freezes.is_empty());
        assert!(
            r_frz.verdict.is_pass(),
            "None freeze bound must stay report-only"
        );
    }

    /// Build a hop with one heavily-oversampled anchor id (3 copies, always
    /// delivered) plus `n` single-copy upstream ids, of which the first `drop_k`
    /// are absent downstream (their sole frame dropped). Models the real pipeline:
    /// per-frame loss is only exposed on frames that lack oversample redundancy,
    /// so the masking-aware metric must count exactly those.
    fn hop_single_copy(
        n: usize,
        drop_k: usize,
        max_loss_pct: Option<f64>,
        min_single_copy: usize,
    ) -> HopReport {
        let mut up = vec![o(0, 0), o(0, 1), o(0, 2)]; // oversampled anchor, never lost
        for k in 1..=n {
            up.push(o(k as u32, (2 + k) as i64));
        }
        let mut down = vec![o(0, 10)];
        for k in 1..=n {
            if k > drop_k {
                down.push(o(k as u32, (12 + k) as i64));
            }
        }
        diff_hop(HopInput {
            name: "strih→stream".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct,
            min_single_copy,
        })
    }

    #[test]
    fn single_copy_loss_is_counted_unmasked() {
        // 10 single-copy ids, 2 dropped. The 3-copy anchor id 0 survives and must
        // NOT inflate the single-copy denominator.
        let r = hop_single_copy(10, 2, None, 0);
        assert_eq!(r.single_copy_total, 10);
        assert_eq!(r.single_copy_dropped, 2);
    }

    #[test]
    fn loss_pct_bound_accepts_documented_irreducible_loss() {
        // 2/100 single-copy frames dropped = 2%; bound 5% → PASS even though
        // dropped_ids is non-empty. This is the documented-bound gate mode (#21):
        // strih→stream's genlock-bound per-frame loss is accepted up to the bound.
        let r = hop_single_copy(100, 2, Some(5.0), 0);
        assert!(!r.dropped_ids.is_empty());
        assert_eq!(r.single_copy_total, 100);
        assert_eq!(r.single_copy_dropped, 2);
        assert!(
            r.verdict.is_pass(),
            "2% single-copy loss under a 5% bound must PASS"
        );
    }

    #[test]
    fn loss_pct_bound_fails_on_regression_above_bound() {
        // 8/100 = 8% > 5% bound → FAIL (catches regression past the documented bound).
        let r = hop_single_copy(100, 8, Some(5.0), 0);
        assert!(
            !r.verdict.is_pass(),
            "8% single-copy loss over a 5% bound must FAIL"
        );
    }

    #[test]
    fn none_loss_bound_keeps_strict_zero_loss() {
        // Default (None) keeps the strict dropped_ids-empty gate: ANY drop FAILs,
        // preserving the Phase-2 zero-loss default for hops without a bound.
        let r = hop_single_copy(10, 1, None, 0);
        assert!(
            !r.verdict.is_pass(),
            "None bound must stay strict zero-loss"
        );
    }

    #[test]
    fn loss_pct_bound_passes_when_no_single_copy_frames() {
        // Every upstream id is oversampled (×2) and delivered → single_copy_total=0.
        // The documented-bound gate must PASS on 0 loss, NOT divide 0/0 into a NaN
        // that fails the comparison. Pins `single_copy_total > 0` (not `>= 0`).
        let up = vec![o(0, 0), o(0, 1), o(1, 2), o(1, 3), o(2, 4), o(2, 5)];
        let down = vec![o(0, 10), o(1, 12), o(2, 14)];
        let r = diff_hop(HopInput {
            name: "strih→stream".to_string(),
            upstream: &up,
            downstream: &down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
            max_p99_latency_ms: None,
            max_freeze_periods_gate: None,
            max_loss_pct: Some(5.0),
            min_single_copy: 0,
        });
        assert_eq!(r.single_copy_total, 0);
        assert!(
            r.verdict.is_pass(),
            "no single-copy frames + zero loss under bound must PASS, not NaN-fail"
        );
    }

    #[test]
    fn loss_pct_bound_at_exact_bound_passes() {
        // 5/100 single-copy frames dropped = exactly 5.0%; bound 5.0 → PASS
        // (strict `<=`, not `<`). Pins the bound comparison boundary.
        let r = hop_single_copy(100, 5, Some(5.0), 0);
        assert_eq!(r.single_copy_dropped, 5);
        assert_eq!(r.single_copy_total, 100);
        assert!(
            r.verdict.is_pass(),
            "single-copy loss exactly at the bound must PASS (<=)"
        );
    }

    // ---- #29 oversample-masking guard: min single-copy-sample sufficiency ----

    #[test]
    fn insufficient_single_copy_is_inconclusive_not_pass() {
        // Strict zero-loss met (0 dropped) but only 5 single-copy frames against a
        // guard of 100 → the green is oversample-masking-suspect. Verdict must be
        // INCONCLUSIVE — neither a clean Pass (untrustworthy) nor a Fail (no
        // regression was proven).
        let r = hop_single_copy(5, 0, None, 100);
        assert!(r.dropped_ids.is_empty());
        assert_eq!(r.single_copy_total, 5);
        assert_eq!(r.verdict, HopVerdict::Inconclusive);
        assert!(!r.verdict.is_pass(), "Inconclusive must not count as pass");
    }

    #[test]
    fn enough_single_copy_certifies_pass() {
        // 200 single-copy frames, 0 dropped, guard 100 satisfied → trustworthy
        // zero-loss → PASS.
        let r = hop_single_copy(200, 0, None, 100);
        assert_eq!(r.single_copy_total, 200);
        assert_eq!(r.verdict, HopVerdict::Pass);
    }

    #[test]
    fn single_copy_at_exact_min_certifies_pass() {
        // single_copy_total == min_single_copy (100 == 100) → PASS. Pins the guard
        // boundary as `single_copy_total < min` (strict `<`), so a run with exactly
        // the required samples certifies; kills a `<`→`<=` mutant.
        let r = hop_single_copy(100, 0, None, 100);
        assert_eq!(r.single_copy_total, 100);
        assert_eq!(r.verdict, HopVerdict::Pass);
    }

    #[test]
    fn one_below_min_single_copy_is_inconclusive() {
        // single_copy_total == min_single_copy - 1 (99 < 100) → INCONCLUSIVE. The
        // other half of the boundary; together with the exact-min test, pins the
        // comparison so neither `<`→`<=` nor `<`→`>` mutants survive.
        let r = hop_single_copy(99, 0, None, 100);
        assert_eq!(r.single_copy_total, 99);
        assert_eq!(r.verdict, HopVerdict::Inconclusive);
    }

    #[test]
    fn guard_zero_disables_inconclusive_back_compat() {
        // min_single_copy == 0 (default) → guard off → a clean hop with very few
        // single-copy frames still PASSes, preserving pre-#29 behaviour for hops
        // that did not opt in.
        let r = hop_single_copy(3, 0, None, 0);
        assert_eq!(r.verdict, HopVerdict::Pass);
    }

    #[test]
    fn real_loss_is_fail_not_inconclusive_even_below_guard() {
        // A genuine drop (2 of 5 single-copy ids absent) with single_copy_total
        // below the guard must read as FAIL, not INCONCLUSIVE — a proven
        // regression outranks the sample-sufficiency check. Pins that the guard
        // only ever downgrades a would-be Pass, never a Fail.
        let r = hop_single_copy(5, 2, None, 100);
        assert_eq!(r.single_copy_dropped, 2);
        assert_eq!(r.verdict, HopVerdict::Fail);
    }

    #[test]
    fn guard_applies_in_documented_bound_mode_too() {
        // Documented-bound mode: single-copy loss 0% is under the 5% bound (loss
        // gate passes), but only 10 single-copy frames against a guard of 100 →
        // INCONCLUSIVE. The guard protects the bound-mode green as well as strict.
        let r = hop_single_copy(10, 0, Some(5.0), 100);
        assert_eq!(r.single_copy_total, 10);
        assert_eq!(r.verdict, HopVerdict::Inconclusive);
    }

    #[test]
    fn clean_hop_passes_no_drops() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(r.verdict.is_pass());
        assert!(r.dropped_ids.is_empty());
        assert_eq!(r.upstream_unique, 4);
        assert_eq!(r.downstream_unique, 4);
    }

    #[test]
    fn single_frame_drop_downstream_is_detected() {
        // id 2 present upstream, absent downstream → the hop dropped it.
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.verdict.is_pass());
        assert_eq!(r.dropped_ids, vec![2]);
    }

    #[test]
    fn resample_dups_present_in_both_are_not_drops() {
        // id 1 duplicated by the resample at both taps → no drop, PASS.
        let up = vec![o(0, 0), o(1, 33), o(1, 40), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(1, 50), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(r.verdict.is_pass());
        assert!(r.dropped_ids.is_empty());
    }

    #[test]
    fn unequal_dup_counts_are_not_a_drop() {
        // id 1 twice upstream, once downstream — still PRESENT downstream, so it
        // is not a drop. Pins set-difference (membership) semantics: a multiset /
        // count-based differ would wrongly flag id 1 here.
        let up = vec![o(0, 0), o(1, 33), o(1, 40), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(r.verdict.is_pass());
        assert!(r.dropped_ids.is_empty());
    }

    #[test]
    fn reorder_on_downstream_fails() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(2, 43), o(1, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.verdict.is_pass());
        assert_eq!(r.reorders, vec![(2, 1)]);
    }

    #[test]
    fn empty_downstream_fails_min_frames_not_vacuous() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down: Vec<Observed> = vec![];
        let r = diff_hop(input(&up, &down));
        assert!(!r.verdict.is_pass());
        assert_eq!(r.downstream_unique, 0);
    }

    #[test]
    fn per_hop_latency_is_downstream_minus_upstream() {
        // each id arrives 10 ms later downstream → mean 10 ms.
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        let l = r.latency.unwrap();
        assert_eq!(l.samples, 3);
        assert!((l.mean_ms - 10.0).abs() < 0.001);
        assert!((l.max_ms - 10.0).abs() < 0.001);
    }

    #[test]
    fn startup_skew_before_downstream_first_id_is_not_a_drop() {
        // Downstream tap (OBS-forwarded) connected late: it never saw ids 0..4.
        // Those are start skew, not drops — only ids within [5, 9] are judged.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(5, 15), o(6, 16), o(7, 17), o(8, 18), o(9, 19)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.verdict.is_pass());
    }

    #[test]
    fn shutdown_skew_after_downstream_last_id_is_not_a_drop() {
        // Downstream stopped at id 5 (in-flight tail); ids 6..9 are end skew.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(0, 10), o(1, 11), o(2, 12), o(3, 13), o(4, 14), o(5, 15)];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.verdict.is_pass());
    }

    #[test]
    fn real_drop_inside_active_span_still_fails() {
        // Skew at both ends (down starts at 2, ends at 8) AND a genuine drop of
        // id 5 in the middle. The skew is excluded; the real drop is caught.
        let up = vec![
            o(0, 0),
            o(1, 1),
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
            o(9, 9),
        ];
        let down = vec![o(2, 12), o(3, 13), o(4, 14), o(6, 16), o(7, 17), o(8, 18)];
        let r = diff_hop(input(&up, &down));
        assert_eq!(r.dropped_ids, vec![5]);
        assert!(!r.verdict.is_pass());
    }

    // ---- #68 Task B: endpoint sequence vs the generator's contiguity + order ----

    #[test]
    fn endpoint_seq_contiguous_in_order_is_clean() {
        let ep = vec![o(3, 30), o(4, 40), o(5, 50), o(6, 60)];
        let r = endpoint_sequence_check(&ep);
        assert_eq!(r.first_id, 3);
        assert_eq!(r.last_id, 6);
        assert_eq!(r.expected_count, 4);
        assert_eq!(r.delivered_count, 4);
        assert!(r.missing_ids.is_empty());
        assert!(r.out_of_order_ids.is_empty());
        assert!(r.is_clean());
    }

    #[test]
    fn endpoint_seq_flags_internal_gap() {
        // id 4 absent inside [3..=6] → a real generator→endpoint drop.
        let ep = vec![o(3, 30), o(5, 50), o(6, 60)];
        let r = endpoint_sequence_check(&ep);
        assert_eq!(r.missing_ids, vec![4]);
        assert_eq!(r.expected_count, 4);
        assert_eq!(r.delivered_count, 3);
        assert!(!r.is_clean());
    }

    #[test]
    fn endpoint_seq_flags_reorder_only() {
        // 3,4,6,5,7 — id 5 after the higher id 6 → reorder; nothing missing.
        let ep = vec![o(3, 30), o(4, 40), o(6, 50), o(5, 60), o(7, 70)];
        let r = endpoint_sequence_check(&ep);
        assert!(r.missing_ids.is_empty());
        assert_eq!(r.out_of_order_ids, vec![5]);
        assert!(!r.is_clean());
    }

    #[test]
    fn endpoint_seq_oversample_dups_are_clean() {
        // Held/duplicated ids (==running max) are not reorders, not gaps.
        let ep = vec![o(3, 30), o(3, 31), o(4, 40), o(4, 41), o(5, 50)];
        let r = endpoint_sequence_check(&ep);
        assert!(r.missing_ids.is_empty());
        assert!(r.out_of_order_ids.is_empty());
        assert!(r.is_clean());
    }

    #[test]
    fn endpoint_seq_empty_is_not_clean() {
        let r = endpoint_sequence_check(&[]);
        assert_eq!(r.delivered_count, 0);
        assert_eq!(r.expected_count, 0);
        assert!(!r.is_clean());
    }

    #[test]
    fn endpoint_seq_single_frame_is_not_clean() {
        // A length-1 span (delivered_count==1) cannot demonstrate contiguity.
        // Pins is_clean's `>= 2` floor (kills a `>= 1` / `> 0` mutant).
        let r = endpoint_sequence_check(&[o(9, 90)]);
        assert_eq!(r.first_id, 9);
        assert_eq!(r.last_id, 9);
        assert_eq!(r.expected_count, 1);
        assert_eq!(r.delivered_count, 1);
        assert!(r.missing_ids.is_empty());
        assert!(r.out_of_order_ids.is_empty());
        assert!(!r.is_clean());
    }

    #[test]
    fn endpoint_seq_two_frame_contiguous_is_clean() {
        // delivered_count == 2 is the minimum non-vacuous span → clean. The other
        // side of the is_clean `>= 2` boundary (kills a `> 2` mutant).
        let ep = vec![o(7, 70), o(8, 80)];
        let r = endpoint_sequence_check(&ep);
        assert_eq!(r.delivered_count, 2);
        assert!(r.is_clean());
    }

    #[test]
    fn endpoint_seq_gap_and_reorder_both_reported() {
        // 3,5,4,7 over [3..=7]: id 6 missing AND id 4 out of order (after 5).
        let ep = vec![o(3, 30), o(5, 40), o(4, 50), o(7, 70)];
        let r = endpoint_sequence_check(&ep);
        assert_eq!(r.missing_ids, vec![6]);
        assert_eq!(r.out_of_order_ids, vec![4]);
        assert!(!r.is_clean());
    }

    #[test]
    fn decompose_missing_classifies_by_source_presence() {
        // {7,8} missing at endpoint; source has 8 (→pipeline loss) not 7 (→artifact).
        let src = vec![o(6, 60), o(8, 80), o(9, 90)];
        let (artifact, pipeline) = decompose_missing(&[7, 8], &src);
        assert_eq!(artifact, vec![7]);
        assert_eq!(pipeline, vec![8]);
    }

    #[test]
    fn decompose_missing_all_artifact_when_source_lacks_all() {
        let src = vec![o(5, 50), o(6, 60)];
        let (artifact, pipeline) = decompose_missing(&[7, 8, 9], &src);
        assert_eq!(artifact, vec![7, 8, 9]);
        assert!(pipeline.is_empty());
    }

    #[test]
    fn decompose_missing_all_pipeline_when_source_has_all() {
        let src = vec![o(7, 70), o(8, 80), o(9, 90)];
        let (artifact, pipeline) = decompose_missing(&[7, 8, 9], &src);
        assert!(artifact.is_empty());
        assert_eq!(pipeline, vec![7, 8, 9]);
    }

    #[test]
    fn decompose_missing_empty_is_empty() {
        let src = vec![o(1, 10)];
        let (artifact, pipeline) = decompose_missing(&[], &src);
        assert!(artifact.is_empty());
        assert!(pipeline.is_empty());
    }

    // ---- #105 dual-QR Vernier net-loss proof: step distribution ----

    /// Build capture-order observations from a list of decoded ids (recv ts = index).
    fn seq_obs(ids: &[u32]) -> Vec<Observed> {
        ids.iter()
            .enumerate()
            .map(|(i, &id)| o(id, i as i64))
            .collect()
    }

    #[test]
    fn step_dist_clean_every_other_tick_is_balanced() {
        // Perfect 60→30 sampling: ids 0,2,4,6,8 — every step is exactly 2, avg 2.0,
        // no jitter, net_imbalance 0 → balanced (zero net loss).
        let r = step_distribution(&seq_obs(&[0, 2, 4, 6, 8]));
        assert_eq!(r.first_id, 0);
        assert_eq!(r.last_id, 8);
        assert_eq!(r.advances, 4);
        assert!((r.avg_step - 2.0).abs() < 1e-9, "avg_step={}", r.avg_step);
        assert_eq!(r.histogram, vec![(2, 4)]);
        assert_eq!(r.net_imbalance, 0);
        assert!(r.balanced);
    }

    #[test]
    fn step_dist_balanced_jitter_nets_to_zero() {
        // The un-genlocked beat: the sampling phase walks across the vblank so some
        // steps are 1 and some are 3, but they CANCEL (equal counts) — avg still 2.0,
        // net_imbalance 0 → balanced. ids: 0,1,4,5,8  steps: 1,3,1,3 (two 1s, two 3s).
        let r = step_distribution(&seq_obs(&[0, 1, 4, 5, 8]));
        assert_eq!(r.advances, 4);
        assert!((r.avg_step - 2.0).abs() < 1e-9);
        assert_eq!(r.histogram, vec![(1, 2), (3, 2)]);
        // balance row for k=1: count(3)=2 == count(1)=2 → balanced.
        assert_eq!(r.balance, vec![(1, 2, 2)]);
        assert_eq!(r.net_imbalance, 0);
        assert!(r.balanced, "equal +1/−1 deviations must net to zero loss");
    }

    #[test]
    fn step_dist_one_sided_drift_is_real_loss() {
        // A real drop: an id is skipped with NO matching backward beat. ids 0,2,5,7,9
        // steps 2,3,2,2 — one extra step-3 (the lost frame) not balanced by a step-1.
        // avg_step 2.25 > 2.0, net_imbalance +1 → NOT balanced (one real net loss).
        let r = step_distribution(&seq_obs(&[0, 2, 5, 7, 9]));
        assert_eq!(r.advances, 4);
        assert!((r.avg_step - 2.25).abs() < 1e-9, "avg_step={}", r.avg_step);
        // k=1: count(3)=1 vs count(1)=0 → imbalanced.
        assert_eq!(r.balance, vec![(1, 1, 0)]);
        assert_eq!(r.net_imbalance, 1);
        assert!(!r.balanced, "an unmatched forward skip is real net loss");
    }

    #[test]
    fn step_dist_collapses_held_ids() {
        // Oversample/held frames (an id repeated by the chain) are NOT steps: the same
        // id served twice is removed before stepping. ids 0,0,2,2,2,4 → distinct 0,2,4
        // → two clean step-2s, balanced.
        let r = step_distribution(&seq_obs(&[0, 0, 2, 2, 2, 4]));
        assert_eq!(r.advances, 2);
        assert_eq!(r.histogram, vec![(2, 2)]);
        assert!((r.avg_step - 2.0).abs() < 1e-9);
        assert!(r.balanced);
    }

    #[test]
    fn step_dist_backward_beat_is_kept_and_balances() {
        // A backward beat (reorder from the optical sampling) is a NEGATIVE step and is
        // the −k side that cancels a +k. ids 0,2,4,3,5,7: steps 2,2,-1,2,2 — wait, that
        // has a lone -1; use 0,3,2,5,4,7 → steps 3,-1,3,-1,3: three +1 (step3), two -1
        // → not balanced. Use a symmetric one: 0,3,2,5 → steps 3,-1,3 → net (3-1+3)=5,
        // 2*3=6 → -1. Construct a truly balanced reorder: 1,3,0,2 distinct → wait first
        // must be lowest decoded. Use ids 2,4,3,5 (steps 2,-1,2): sum 3, 2*3=6 → -3.
        // Simplest balanced backward case: 0,4,2,6 steps 4,-2,4 sum6 2*3=6 net0.
        let r = step_distribution(&seq_obs(&[0, 4, 2, 6]));
        assert_eq!(r.advances, 3);
        assert_eq!(r.histogram, vec![(-2, 1), (4, 2)]);
        // k=2: count(4)=2 vs count(0)=0; but the -2 step contributes to k via |s-2|=4.
        // The closed-form net is what gates: sum=6, 2*3=6 → 0 → balanced.
        assert_eq!(r.net_imbalance, 0);
        assert!(
            r.balanced,
            "a backward beat that restores the span nets to zero"
        );
    }

    #[test]
    fn step_dist_empty_and_single_are_trivially_balanced() {
        let r = step_distribution(&[]);
        assert_eq!(r.advances, 0);
        assert_eq!(r.net_imbalance, 0);
        assert!(r.balanced);
        let r1 = step_distribution(&seq_obs(&[7]));
        assert_eq!(r1.first_id, 7);
        assert_eq!(r1.last_id, 7);
        assert_eq!(r1.advances, 0);
        assert!(r1.balanced);
    }

    #[test]
    fn step_dist_net_imbalance_equals_span_minus_2x_advances() {
        // The closed form is identically (last-first) - 2*advances; pin it against a
        // re-summation mutant. ids 0,1,4,5,8: span 8, advances 4, 8-8=0.
        let r = step_distribution(&seq_obs(&[0, 1, 4, 5, 8]));
        assert_eq!(
            r.net_imbalance,
            (r.last_id as i64 - r.first_id as i64) - 2 * r.advances as i64
        );
    }

    #[test]
    fn grid_continuity_flags_starvation_and_underrun() {
        // ids 10..=14 each once, stride 1 -> clean.
        let clean: Vec<Observed> = (10..=14).map(|i| o(i, i as i64)).collect();
        let g = grid_continuity(&clean, 1);
        assert!(g.missing_slots.is_empty() && g.repeated_ids.is_empty());
        assert!(g.is_clean());

        // id 12 missing (starvation), id 13 repeated (underrun).
        let bad = vec![o(10, 0), o(11, 1), o(13, 2), o(13, 3), o(14, 4)];
        let g = grid_continuity(&bad, 1);
        assert_eq!(g.missing_slots, vec![12]);
        assert_eq!(g.repeated_ids, vec![13]);
        assert!(!g.is_clean());
    }

    #[test]
    fn endpoint_seq_expected_count_spans_full_range() {
        // [10..=20] = 11 generated ids; pins expected_count = last-first+1 against
        // an off-by-one mutant. All present except 15 → 1 missing.
        let mut ep: Vec<Observed> = (10u32..=20).map(|i| o(i, i as i64 * 10)).collect();
        ep.retain(|o| o.frame_id != 15);
        let r = endpoint_sequence_check(&ep);
        assert_eq!(r.first_id, 10);
        assert_eq!(r.last_id, 20);
        assert_eq!(r.expected_count, 11);
        assert_eq!(r.delivered_count, 10);
        assert_eq!(r.missing_ids, vec![15]);
    }

    #[test]
    fn drop_exactly_at_span_boundaries_is_excluded() {
        // Pins the inclusive bounds: ids equal to lo (2) and hi (8) that are
        // present downstream are not drops; the `>= lo` / `<= hi` comparisons
        // must stay inclusive (kills boundary mutants).
        let up = vec![
            o(2, 2),
            o(3, 3),
            o(4, 4),
            o(5, 5),
            o(6, 6),
            o(7, 7),
            o(8, 8),
        ];
        let down = vec![
            o(2, 12),
            o(3, 13),
            o(4, 14),
            o(5, 15),
            o(6, 16),
            o(7, 17),
            o(8, 18),
        ];
        let r = diff_hop(input(&up, &down));
        assert!(r.dropped_ids.is_empty());
        assert!(r.verdict.is_pass());
    }
}
