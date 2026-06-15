//! multitap-probe: subscribe to N NDI taps on dev1, difference adjacent pairs,
//! emit one JSON artifact with a per-hop frame-loss + latency report, exit
//! non-zero on any real per-hop drop/reorder. Painter runs separately on the
//! camera box (frame-probe --paint-only) with the same --run-id.

use anyhow::{bail, Result};
use camera_box::probe::analyzer::LatencyStats;
use camera_box::probe::clock_ns;
use camera_box::probe::differ::{
    abs_emit_latency, absolute_latency_gate_pass, absolute_latency_stats, decompose_missing,
    diff_hop, earliest_recv_ns, endpoint_sequence_check, full_span_diff, lead_cutoff_ns,
    overall_verdict, settle_cutoff_ns, trim_after, trim_to_settle, EndpointSequenceReport,
    FullSpanBounds, FullSpanReport, HopInput, HopReport, HopVerdict,
};
use camera_box::probe::multi_reader::{spawn_tap, TapResult, TapSpec};
use clap::Parser;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "Multi-tap NDI per-hop frame-loss/latency probe (Phase 2)")]
struct Args {
    /// Shared run id (must match the frame-probe painter's --run-id).
    #[arg(long)]
    run_id: u32,
    /// A tap as NAME=NDI_SOURCE_SUBSTRING. Repeat; adjacent taps are
    /// differenced in the order given (e.g. cam="CAM2 (usb)" strih="2ME PGM"
    /// stream="<stream program NDI>" → hops cam→strih, strih→stream). The OBS
    /// program NDI names are whatever each box's DistroAV Main Output advertises.
    #[arg(long = "tap", value_parser = parse_tap)]
    taps: Vec<(String, String)>,
    /// Run duration in seconds.
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    /// Expected capture rate (for freeze duration math).
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// QR pixel size on the canvas (decode ROI = qr_size + 120).
    #[arg(long, default_value_t = 700)]
    qr_size: u32,
    /// Freeze threshold in capture periods.
    #[arg(long, default_value_t = 6.0)]
    freeze_periods: f64,
    /// NDI connect timeout (seconds).
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u32,
    /// Trailing settle window (ms): frames received this close to the end are
    /// trimmed so in-flight frames are not counted as hop drops.
    #[arg(long, default_value_t = 500)]
    settle_ms: u64,
    /// #68 Task C — leading-discard window (seconds): frames received within this
    /// many seconds of the run's FIRST decoded frame (across all taps) are trimmed
    /// before the loss/contiguity check. The harness re-points the OBS receiver
    /// right before a run, which transiently primes the genlock FIFO so the first
    /// seconds look cleaner than steady state and can MASK the steady-state loss
    /// the persistence test sees. Discard 30-60 s to measure only steady state.
    /// Default 0 = no leading trim (the prior behaviour); set it for a
    /// reset-confound-free zero-loss run.
    #[arg(long, default_value_t = 0)]
    lead_discard_secs: u64,
    /// A tap with fewer than this many run_id-matching frames FAILS (not vacuous).
    #[arg(long, default_value_t = 100)]
    min_frames: usize,
    /// Per-hop latency gate as DOWNSTREAM_TAP=MS (repeat per hop, e.g.
    /// `--max-p99-latency-ms strih=130 --max-p99-latency-ms stream=220`). The hop
    /// feeding tap X FAILs if its relative-latency p99 exceeds X's bound (strict
    /// `>`). An omitted hop is report-only — the Phase-2 default. Bounds are
    /// rig-specific; baseline with a report-only run first, then ratchet.
    #[arg(long = "max-p99-latency-ms", value_parser = parse_bound)]
    max_p99_latency_ms: Vec<(String, f64)>,
    /// Per-hop freeze gate as DOWNSTREAM_TAP=PERIODS (repeat per hop). The hop
    /// feeding tap X FAILs if any freeze's repeat_count exceeds X's bound (strict
    /// `>`). An omitted hop is report-only (default).
    #[arg(long = "max-freeze-periods", value_parser = parse_bound)]
    max_freeze_periods: Vec<(String, f64)>,
    /// Per-hop documented-bound loss gate as DOWNSTREAM_TAP=PCT (repeat per hop).
    /// When set, the hop is judged by its oversample-independent single-copy
    /// frame-loss percentage staying `<= PCT` instead of the strict
    /// any-drop-fails default. For hops with a known, quantified, currently
    /// irreducible loss (strih→stream's OBS render-clock drop pending genlock,
    /// #8): accepts the documented floor, still fails on regression past it.
    #[arg(long = "max-loss-pct", value_parser = parse_bound)]
    max_loss_pct: Vec<(String, f64)>,
    /// Oversample-masking guard (#29) as DOWNSTREAM_TAP=COUNT (repeat per hop, like
    /// the other gates). The hop feeding tap X is only CERTIFIED (verdict PASS)
    /// when it carried at least COUNT single-copy (oversample-independent) frames;
    /// below that its verdict is INCONCL (which, like FAIL, makes the run exit
    /// non-zero) instead of a false-green PASS. The painter is sub-fps, so a unique
    /// id is only "dropped" when ALL its copies are lost and a high-oversample run
    /// can show zero loss while the pipeline really drops frames; single-copy ids
    /// expose the real per-frame drop. Per-hop because the yield differs sharply by
    /// hop: cam→strih reliably gives ~50-68, but strih→stream is starved (2-63,
    /// often too few) until the full-fps painter (#32) — so guard the hop that has
    /// the evidence and leave the starved one ungated. An omitted hop is ungated.
    #[arg(long = "min-single-copy", value_parser = parse_bound)]
    min_single_copy: Vec<(String, f64)>,
    /// #7 ABSOLUTE end-to-end latency gate (ms): FAIL the run if the source→
    /// endpoint p99 of `recv_ts(endpoint) − gen_ts(source)` exceeds this. The
    /// first tap is the source, the last tap is the endpoint. REQUIRES
    /// `--wall-clock` (so gen and recv share the DanteSync wall clock) — without
    /// it the absolute number is meaningless and this flag is rejected. Omitted ⇒
    /// absolute latency is report-only (still WRITTEN to the artifact, no gate).
    #[arg(long)]
    max_abs_latency_ms: Option<f64>,
    /// Record every tap's `recv_ts_ns` on CLOCK_REALTIME (the DanteSync-disciplined
    /// wall clock) instead of dev1's monotonic clock, so the source→endpoint
    /// ABSOLUTE latency (recv(endpoint) − gen(source)) is sound. The painter must
    /// run with the matching `frame-probe --wall-clock`. Per-hop RELATIVE latency
    /// stays valid either way (both taps use the same domain). Verify the cluster
    /// offset first with scripts/clock-offset-guard.sh. Default off (Phase-2
    /// relative-latency behaviour, unchanged).
    #[arg(long, default_value_t = false)]
    wall_clock: bool,
    /// JSON artifact output path.
    #[arg(long, default_value = "/tmp/multitap-probe.json")]
    out: String,
    /// Optional raw per-frame dump (JSONL: {tap,frame_id,recv_ts_ns}) of every
    /// decoded observation, untrimmed. Diagnostic for root-causing which ids drop
    /// and their oversample multiplicity; off unless set.
    #[arg(long)]
    dump_raw: Option<String>,
}

fn parse_tap(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, src)) if !name.is_empty() && !src.is_empty() => {
            Ok((name.to_string(), src.to_string()))
        }
        _ => Err(format!("tap must be NAME=NDI_SOURCE (got '{s}')")),
    }
}

/// Parse a per-hop bound `DOWNSTREAM_TAP=VALUE` (value is the latency-ms or
/// freeze-periods threshold for the hop feeding that tap).
fn parse_bound(s: &str) -> Result<(String, f64), String> {
    match s.split_once('=') {
        Some((name, val)) if !name.is_empty() => val
            .parse::<f64>()
            .map(|v| (name.to_string(), v))
            .map_err(|e| format!("bound value must be a number (got '{val}': {e})")),
        _ => Err(format!("bound must be DOWNSTREAM_TAP=VALUE (got '{s}')")),
    }
}

/// Fail loudly if any bound keys a tap that is not a hop downstream. A bound is
/// applied by exact name match against the downstream tap, so a typo (`striih=`)
/// or a renamed tap would otherwise silently turn an intended gate into a
/// report-only no-op — the opposite of what an operator asked for.
fn validate_bound_keys(
    bounds: &[(String, f64)],
    downstream_taps: &std::collections::HashSet<&str>,
    flag: &str,
) -> Result<()> {
    for (key, _) in bounds {
        if !downstream_taps.contains(key.as_str()) {
            let mut valid: Vec<&str> = downstream_taps.iter().copied().collect();
            valid.sort_unstable();
            bail!("{flag} key '{key}' matches no downstream tap (valid: {valid:?})");
        }
    }
    Ok(())
}

/// A single-copy guard is a frame COUNT. The shared f64 `parse_bound` accepts
/// negatives and fractions, but `strih=-5` would saturate to 0 (= ungated) — the
/// opposite of an operator tightening the gate — and `strih=2.7` would silently
/// truncate. Reject both loudly so the gate can never weaken by typo.
fn validate_count_bounds(bounds: &[(String, f64)], flag: &str) -> Result<()> {
    for (key, val) in bounds {
        if *val < 0.0 || val.fract() != 0.0 {
            bail!("{flag} {key}={val} must be a non-negative integer (frame count)");
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct MultiTapReport {
    run_id: u32,
    duration_secs: u64,
    /// #68 Task C: the leading-discard window (seconds) trimmed from the start of
    /// the run before the loss/contiguity check, so a post-reset genlock-FIFO prime
    /// cannot mask steady-state loss. 0 = no leading trim. The artifact records it
    /// so a "zero loss" verdict is self-describing about WHICH window it certified.
    lead_discard_secs: u64,
    taps: Vec<TapSummary>,
    hops: Vec<HopReport>,
    /// #7: source→endpoint full-span zero-loss aggregate (first tap vs last tap),
    /// the HEADLINE number — "did every camera frame reach the last endpoint",
    /// not a sum of adjacent-hop diffs.
    full_span: FullSpanReport,
    /// #68 Task B: the ENDPOINT tap's sequence checked against the GENERATOR's
    /// contiguous emission (every integer in the endpoint's decoded span was
    /// painted), not against the source tap. Reports any internal gap and any
    /// reorder. The gap is decomposed below into emission-artifact vs pipeline-loss.
    endpoint_sequence: EndpointSequenceReport,
    /// #68: of `endpoint_sequence.missing_ids`, the ids ALSO absent at the SOURCE
    /// tap — a generator→source-NDI EMISSION ARTIFACT of the QR rig (the discrete
    /// painter through the 60→30 genlock decimation never put them on NDI), NOT a
    /// pipeline drop. Reported, never gated. (Empty on a synth-ndi source.)
    endpoint_missing_emission_artifact: Vec<u32>,
    /// #68: of `endpoint_sequence.missing_ids`, the ids PRESENT at the SOURCE tap
    /// but absent at the endpoint — REAL source→endpoint pipeline loss (== full_span
    /// dropped_ids). The HARD gate term: `verdict_pass` fails if this is non-empty
    /// (strict mode). This is what makes the harness agree with reality without
    /// falsely failing on the QR rig's own source-emission artifact.
    endpoint_pipeline_loss: Vec<u32>,
    /// #7: ABSOLUTE end-to-end latency `recv_ts(endpoint) − gen_ts(source)` on the
    /// DanteSync wall clock. Present (and gated when `--max-abs-latency-ms` is set)
    /// only with `--wall-clock`; otherwise `None` with `absolute_latency_note`
    /// explaining why. Replaces the old hard-coded "UNAVAILABLE" string.
    absolute_latency: Option<LatencyStats>,
    /// Human-readable status for `absolute_latency` (why it is/ isn't available,
    /// and the gate outcome) — so the artifact is self-describing.
    absolute_latency_note: String,
    verdict_pass: bool,
}

#[derive(Serialize)]
struct TapSummary {
    name: String,
    unique_frames: usize,
    /// Raw NDI frames pulled off the wire (decoded or not), over the whole run.
    captured: u64,
    /// Raw frames whose QR decoded with a matching run_id (includes oversample
    /// duplicates of the same id). `captured - decoded` is this tap's
    /// decode-miss floor — frames that ARRIVED but did not yield a matching-run_id
    /// QR: torn/un-decodable, or (≈0 in a single-run probe) a QR from a different
    /// run_id. Comparing a downstream tap's `captured` against the upstream tap's
    /// output proves whether id-level `dropped_ids` is true hop loss or just tap
    /// decode misses.
    decoded: u64,
    /// Per-tap ABSOLUTE emit latency `node_emit_tc − gen_ts` (ms) on the shared
    /// DanteSync clock: paint→this-node's-emit. gen_ts-anchored, oversample-immune.
    /// cam tap = paint→camera-NDI-emit (rig); strih/stream = paint→their emit.
    /// Adjacent taps' difference is a clean per-hop latency. `None` if this tap's
    /// source stamped no usable NDI timecode.
    abs_emit_latency: Option<LatencyStats>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    if args.taps.len() < 2 {
        bail!(
            "need >= 2 taps to difference at least one hop (got {})",
            args.taps.len()
        );
    }
    if args.settle_ms >= args.duration_secs.saturating_mul(1000) {
        bail!(
            "--settle-ms ({}) must be less than the run duration ({} s)",
            args.settle_ms,
            args.duration_secs
        );
    }
    // The leading-discard + trailing-settle windows must leave a non-empty
    // steady-state middle, or there is nothing left to certify (and the loss check
    // would vacuously pass on zero frames). Reject the combination up front.
    if args
        .lead_discard_secs
        .saturating_mul(1000)
        .saturating_add(args.settle_ms)
        >= args.duration_secs.saturating_mul(1000)
    {
        bail!(
            "--lead-discard-secs ({} s) + --settle-ms ({} ms) must leave a non-empty \
             steady-state window within the run duration ({} s)",
            args.lead_discard_secs,
            args.settle_ms,
            args.duration_secs
        );
    }
    // The absolute-latency gate is only sound on the shared wall clock. Reject the
    // combination that would silently gate a meaningless number (gen on the
    // camera's clock, recv on dev1's monotonic clock) rather than no-op the gate.
    if args.max_abs_latency_ms.is_some() && !args.wall_clock {
        bail!(
            "--max-abs-latency-ms requires --wall-clock (absolute latency = \
             recv(endpoint) − gen(source) is only valid when both stamps share the \
             DanteSync wall clock; verify with scripts/clock-offset-guard.sh)"
        );
    }
    // Every hop's downstream is a tap after the first; a bound must key one of
    // them or it silently no-ops. Reject orphan keys before doing any work.
    let downstream_taps: std::collections::HashSet<&str> =
        args.taps.iter().skip(1).map(|(n, _)| n.as_str()).collect();
    validate_bound_keys(
        &args.max_p99_latency_ms,
        &downstream_taps,
        "--max-p99-latency-ms",
    )?;
    validate_bound_keys(
        &args.max_freeze_periods,
        &downstream_taps,
        "--max-freeze-periods",
    )?;
    validate_bound_keys(&args.max_loss_pct, &downstream_taps, "--max-loss-pct")?;
    validate_bound_keys(&args.min_single_copy, &downstream_taps, "--min-single-copy")?;
    validate_count_bounds(&args.min_single_copy, "--min-single-copy")?;

    let decode_crop = (args.qr_size + 120).min(1080);

    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));

    // Spawn one reader thread per tap.
    let mut handles = Vec::new();
    let mut results: Vec<TapResult> = Vec::new();
    for (name, source) in &args.taps {
        let (h, r) = spawn_tap(
            TapSpec {
                name: name.clone(),
                source: source.clone(),
                run_id: args.run_id,
                connect_timeout_secs: args.connect_timeout_secs,
                decode_crop,
                wall_clock: args.wall_clock,
            },
            start,
            stop.clone(),
        );
        handles.push(h);
        results.push(r);
    }

    // Run for the duration, short-circuit if any tap thread dies.
    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);
    while Instant::now() < deadline {
        if handles.iter().any(|h| h.is_finished()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    // #63: stop_ns MUST share the clock domain of each tap's recv_ts_ns. Taps
    // stamp recv_ts_ns via clock_ns(start, wall_clock); deriving stop from
    // start.elapsed() (always monotonic) made the wall-clock settle cutoff
    // ~1.8e18 ns too small, trimming EVERY frame to unique=0 even when they
    // decoded. Take stop in the same domain the taps recorded in.
    let stop_ns = clock_ns(start, args.wall_clock);
    for h in handles {
        h.join().expect("tap thread panicked")?;
    }

    // Snapshot + trim the trailing settle window (in-flight frames are not drops).
    let cutoff_ns = settle_cutoff_ns(stop_ns, args.settle_ms);
    let settled: Vec<Vec<_>> = results
        .iter()
        .map(|r| trim_to_settle(&r.observed.lock().unwrap(), cutoff_ns))
        .collect();

    // #68 Task C: also trim the LEADING post-reset prime window so a transiently
    // primed genlock FIFO can't mask steady-state loss. Measure the window from the
    // run's global earliest recv (across all settled taps) so every tap drops the
    // same wall-clock prefix and the hops stay comparable. `lead_discard_secs == 0`
    // ⇒ cutoff 0 ⇒ keeps everything (prior behaviour).
    let lead_cut_ns = earliest_recv_ns(&settled.iter().map(|t| t.as_slice()).collect::<Vec<_>>())
        .map(|e| lead_cutoff_ns(e, args.lead_discard_secs * 1000))
        .unwrap_or(0);
    let trimmed: Vec<Vec<_>> = settled.iter().map(|t| trim_after(t, lead_cut_ns)).collect();
    if args.lead_discard_secs > 0 {
        tracing::info!(
            "#68 Task C: discarded leading {} s (cutoff_ns={}); steady-state per-tap frames: {:?}",
            args.lead_discard_secs,
            lead_cut_ns,
            trimmed.iter().map(|t| t.len()).collect::<Vec<_>>()
        );
    }

    // Per-hop gate bounds, keyed by the hop's DOWNSTREAM tap name (the tap the
    // hop feeds). Absent ⇒ None ⇒ report-only.
    let p99_bounds: std::collections::HashMap<String, f64> =
        args.max_p99_latency_ms.iter().cloned().collect();
    let freeze_bounds: std::collections::HashMap<String, f64> =
        args.max_freeze_periods.iter().cloned().collect();
    let loss_bounds: std::collections::HashMap<String, f64> =
        args.max_loss_pct.iter().cloned().collect();
    // Per-hop single-copy guard counts, keyed by downstream tap. Absent ⇒ 0 ⇒
    // ungated. Parsed as f64 (shared parser) then floored to a frame count.
    let sc_bounds: std::collections::HashMap<String, f64> =
        args.min_single_copy.iter().cloned().collect();

    // Difference each adjacent pair.
    let mut hops: Vec<HopReport> = Vec::new();
    for i in 0..trimmed.len() - 1 {
        let down_name = results[i + 1].name.clone();
        let name = format!("{}→{}", results[i].name, down_name);
        hops.push(diff_hop(HopInput {
            name,
            upstream: &trimmed[i],
            downstream: &trimmed[i + 1],
            capture_fps: args.capture_fps,
            freeze_periods: args.freeze_periods,
            min_frames: args.min_frames,
            max_p99_latency_ms: p99_bounds.get(&down_name).copied(),
            max_freeze_periods_gate: freeze_bounds.get(&down_name).copied(),
            max_loss_pct: loss_bounds.get(&down_name).copied(),
            min_single_copy: sc_bounds.get(&down_name).copied().unwrap_or(0.0) as usize,
        }));
    }

    let tap_summaries: Vec<TapSummary> = results
        .iter()
        .zip(&trimmed)
        .map(|(r, obs)| TapSummary {
            name: r.name.clone(),
            unique_frames: obs
                .iter()
                .map(|o| o.frame_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            captured: r.captured.load(Ordering::Relaxed),
            // Raw (untrimmed) decoded-frame count for this tap — the run_id QR
            // frames it actually decoded, oversample dups included.
            decoded: r.observed.lock().unwrap().len() as u64,
            // paint→this-node-emit on the shared DanteSync clock (over the trimmed,
            // steady-state observations). Adjacent taps' difference = per-hop.
            abs_emit_latency: abs_emit_latency(obs),
        })
        .collect();

    // #7 HEADLINE: source→endpoint full-span aggregate — first tap is the source
    // (camera-box NDI), last tap is the endpoint (stream's OBS program NDI). This
    // is the "every source frame reached the last endpoint" number, not a sum of
    // adjacent-hop diffs (a drop can hide per-hop yet be absent end-to-end). The
    // full span is itself a hop, so it obeys the SAME contract as the per-hop
    // gates: the documented loss bound (`--max-loss-pct`), the min_frames floor,
    // and the #29 single-copy INCONCL guard — all keyed to the ENDPOINT tap (the
    // last hop's downstream), so a deliberately-relaxed per-hop budget is NOT
    // silently overridden by a strict full-span gate. >= 2 taps is already
    // enforced above, so first/last are distinct.
    let source = &trimmed[0];
    let endpoint = &trimmed[trimmed.len() - 1];
    let endpoint_name = results[results.len() - 1].name.as_str();
    let full_span = full_span_diff(
        source,
        endpoint,
        &FullSpanBounds {
            min_frames: args.min_frames,
            max_loss_pct: loss_bounds.get(endpoint_name).copied(),
            min_single_copy: sc_bounds.get(endpoint_name).copied().unwrap_or(0.0) as usize,
        },
    );

    // #7 ABSOLUTE end-to-end latency: recv(endpoint) − gen(source) on the shared
    // wall clock. Only meaningful with --wall-clock (gen on the camera and recv on
    // dev1 both DanteSync-disciplined). Always WRITTEN to the artifact; gated only
    // when --max-abs-latency-ms is set (which already required --wall-clock).
    let absolute_latency = if args.wall_clock {
        absolute_latency_stats(source, endpoint)
    } else {
        None
    };
    let abs_gate_pass = absolute_latency_gate_pass(&absolute_latency, args.max_abs_latency_ms);
    let absolute_latency_note = if !args.wall_clock {
        "report-only — run with --wall-clock (+ frame-probe --wall-clock) for the \
         DanteSync-disciplined absolute end-to-end latency (#7/#8)"
            .to_string()
    } else {
        match (&absolute_latency, args.max_abs_latency_ms) {
            (None, _) => "wall-clock on but NO source↔endpoint frame pair decoded \
                          (cannot compute absolute latency)"
                .to_string(),
            (Some(l), None) => format!(
                "wall-clock absolute end-to-end p99={:.1} ms (n={}) — report-only (no --max-abs-latency-ms)",
                l.p99_ms, l.samples
            ),
            (Some(l), Some(b)) => format!(
                "wall-clock absolute end-to-end p99={:.1} ms (n={}) vs bound {:.1} ms → {}",
                l.p99_ms,
                l.samples,
                b,
                if abs_gate_pass { "PASS" } else { "FAIL" }
            ),
        }
    };
    // A negative absolute latency is physically impossible (recv before gen) —
    // it can only mean the source camera's wall clock is AHEAD of dev1's by more
    // than the transit time, i.e. the cluster desynced past what the e2e
    // pre-flight (clock-offset-guard) would have caught, or this probe was run
    // directly without that pre-flight. Flag it loudly so the number is not
    // trusted; the guard, not this arithmetic, is the sync gate.
    let absolute_latency_note = match &absolute_latency {
        Some(l) if l.min_ms < 0.0 => format!(
            "{absolute_latency_note} — WARNING: min={:.1} ms < 0 (impossible) → cluster clock \
             desync; re-run scripts/clock-offset-guard.sh, do NOT trust this latency",
            l.min_ms
        ),
        _ => absolute_latency_note,
    };

    // #68 Task B: check the ENDPOINT tap's decoded stream against the generator's
    // KNOWN contiguity (the painter emits 0,1,2,… so every integer in the
    // endpoint's decoded span was generated). A gap here is a real
    // generator→endpoint drop — including generator→cam loss the source-vs-endpoint
    // diff cannot see — and a reorder is a real reorder. Computed on the endpoint
    // tap (last tap), in its CAPTURE order (results[].observed is push-order), so
    // reorder detection is meaningful. The settle-trimmed endpoint is used (same
    // window as full_span) so trailing in-flight frames are not false gaps.
    let endpoint_sequence = endpoint_sequence_check(endpoint);
    // #68: decompose the endpoint's missing ids using the SOURCE tap. An id absent
    // at the source too is a generator→source-NDI EMISSION ARTIFACT of the QR rig
    // (the discrete fb painter through the 60→30 genlock decimation never put it on
    // NDI — ~5-13% on the fb-loopback, ZERO on synth-ndi), NOT a stream defect. An
    // id present at the source but absent at the endpoint is REAL source→endpoint
    // PIPELINE LOSS. Only the latter may fail a zero-loss gate — gating on the
    // artifact would make the harness falsely RED on a perfectly-fine pipeline.
    let (endpoint_missing_emission_artifact, endpoint_pipeline_loss) =
        decompose_missing(&endpoint_sequence.missing_ids, source);

    // The sequence GATE is active only in strict mode (the genlock zero-loss path
    // #68 targets). When the operator deliberately opts the endpoint into the #21
    // documented-loss bound (`--max-loss-pct endpoint=N`), the gap is EXPECTED and
    // stays REPORT-ONLY, exactly as full_span honours the same opt-out. In strict
    // mode the gate fails on REAL pipeline loss (present-at-source, lost
    // downstream) and on any reorder — but NOT on the emission artifact.
    let endpoint_seq_gated = !loss_bounds.contains_key(endpoint_name);
    let endpoint_seq_pass = !endpoint_seq_gated
        || (endpoint_pipeline_loss.is_empty() && endpoint_sequence.out_of_order_ids.is_empty());

    // Fold per-hop + full-span + abs-latency into ONE verdict (pure, unit-tested
    // in differ::overall_verdict — Pass / Fail / Inconclusive). INCONCL counts as
    // NOT-pass like FAIL (#29: a run that cannot certify must not exit green).
    // #68: the endpoint-sequence check adds a hard gate (strict mode) on REAL
    // source→endpoint pipeline loss + reorder — the generator→endpoint guarantee
    // the tap-vs-tap diff was blind to — while the QR rig's source-emission
    // artifact is reported, not gated, so the harness stays honest AND trustworthy.
    let overall_v = overall_verdict(&hops, &full_span, abs_gate_pass);
    let verdict_pass = overall_v.is_pass() && endpoint_seq_pass;
    let report = MultiTapReport {
        run_id: args.run_id,
        duration_secs: args.duration_secs,
        lead_discard_secs: args.lead_discard_secs,
        taps: tap_summaries,
        hops,
        full_span,
        endpoint_sequence,
        endpoint_missing_emission_artifact,
        endpoint_pipeline_loss,
        absolute_latency,
        absolute_latency_note,
        verdict_pass,
    };

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&args.out, &json)?;

    if let Some(path) = &args.dump_raw {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        for r in &results {
            let name = &r.name;
            for o in r.observed.lock().unwrap().iter() {
                writeln!(
                    f,
                    "{{\"tap\":\"{}\",\"frame_id\":{},\"recv_ts_ns\":{}}}",
                    name, o.frame_id, o.recv_ts_ns
                )?;
            }
        }
    }

    for t in &report.taps {
        let fail = t.captured.saturating_sub(t.decoded);
        let pct = if t.captured > 0 {
            100.0 * fail as f64 / t.captured as f64
        } else {
            0.0
        };
        println!(
            "TAP {} captured={} decoded={} decode_failed={} ({:.2}% torn)",
            t.name, t.captured, t.decoded, fail, pct
        );
    }
    for h in &report.hops {
        let sc_pct = if h.single_copy_total > 0 {
            100.0 * h.single_copy_dropped as f64 / h.single_copy_total as f64
        } else {
            0.0
        };
        println!(
            "HOP {} {} up_unique={} down_unique={} dropped={} reorders={} freezes={} \
             single_copy_loss={}/{} ({:.2}% per-frame, oversample-independent)",
            h.name,
            match h.verdict {
                HopVerdict::Pass => "PASS",
                HopVerdict::Fail => "FAIL",
                HopVerdict::Inconclusive => "INCONCL",
            },
            h.upstream_unique,
            h.downstream_unique,
            h.dropped_ids.len(),
            h.reorders.len(),
            h.freezes.len(),
            h.single_copy_dropped,
            h.single_copy_total,
            sc_pct,
        );
        if let Some(l) = &h.latency {
            println!(
                "  REL_LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={})",
                l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
            );
        }
    }
    // #7 HEADLINE source→endpoint full-span aggregate (first tap → last tap).
    let fs = &report.full_span;
    let fs_sc_pct = if fs.single_copy_total > 0 {
        100.0 * fs.single_copy_dropped as f64 / fs.single_copy_total as f64
    } else {
        0.0
    };
    println!(
        "FULL_SPAN {}→{} {} source_unique={} endpoint_unique={} dropped={} \
         single_copy_loss={}/{} ({:.2}% per-frame, oversample-independent)",
        report.taps.first().map(|t| t.name.as_str()).unwrap_or("?"),
        report.taps.last().map(|t| t.name.as_str()).unwrap_or("?"),
        match fs.verdict {
            HopVerdict::Pass => "ZERO-LOSS",
            HopVerdict::Fail => "LOSS",
            HopVerdict::Inconclusive => "INCONCL",
        },
        fs.source_unique,
        fs.endpoint_unique,
        fs.dropped_ids.len(),
        fs.single_copy_dropped,
        fs.single_copy_total,
        fs_sc_pct,
    );
    // #7 ABSOLUTE end-to-end latency line — the value that replaced "UNAVAILABLE".
    match &report.absolute_latency {
        Some(l) => println!(
            "ABS_LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={}) — {}",
            l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples,
            report.absolute_latency_note,
        ),
        None => println!("ABS_LATENCY=UNAVAILABLE — {}", report.absolute_latency_note),
    }

    // #68 Task B: endpoint sequence vs the generator's contiguity + order. The
    // "every emitted frame has exactly one delivered counterpart at the OBS
    // output, in order" check — gaps here are real generator→endpoint drops the
    // tap-vs-tap diff cannot see, so the first 20 of each are printed for triage.
    let es = &report.endpoint_sequence;
    // Headline reflects the GATED criterion (real pipeline loss + reorder), with
    // the QR-rig source-emission artifact reported separately so a green pipeline
    // is never falsely RED on the discrete-painter-into-decimation artifact.
    let pipeline = &report.endpoint_pipeline_loss;
    let artifact = &report.endpoint_missing_emission_artifact;
    let seq_state = if !pipeline.is_empty() || !es.out_of_order_ids.is_empty() {
        "PIPELINE-LOSS"
    } else if artifact.is_empty() {
        "CONTIGUOUS"
    } else {
        "PIPELINE-CLEAN (source-emission artifact only)"
    };
    println!(
        "ENDPOINT_SEQ {} span=[{}..={}] expected={} delivered={} \
         missing={} (pipeline_loss={} emission_artifact={}) out_of_order={}{}",
        seq_state,
        es.first_id,
        es.last_id,
        es.expected_count,
        es.delivered_count,
        es.missing_ids.len(),
        pipeline.len(),
        artifact.len(),
        es.out_of_order_ids.len(),
        if endpoint_seq_gated {
            ""
        } else {
            " (report-only — endpoint in documented-loss-bound mode)"
        },
    );
    if !pipeline.is_empty() {
        println!(
            "  PIPELINE_LOSS_IDS (present at source, lost downstream — REAL, first 20): {:?}",
            &pipeline[..pipeline.len().min(20)]
        );
    }
    if !artifact.is_empty() {
        println!(
            "  EMISSION_ARTIFACT_IDS (never on source NDI — QR-rig decimation, not a drop; first 20): {:?}",
            &artifact[..artifact.len().min(20)]
        );
    }
    if !es.out_of_order_ids.is_empty() {
        println!(
            "  OUT_OF_ORDER_IDS (first 20): {:?}",
            &es.out_of_order_ids[..es.out_of_order_ids.len().min(20)]
        );
    }

    // Top-level label distinguishes a proven regression (a real FAIL — any hop
    // FAIL, a source→endpoint full-span loss, an absolute-latency breach, or a
    // #68 endpoint-sequence gap/reorder) from an untrustworthy green (only a hop
    // INCONCL: gates passed but too few single-copy samples). Both exit non-zero
    // via `verdict_pass`; the label tells the operator which so an INCONCL reads as
    // "need a longer/denser run", not "the pipeline broke". An unclean endpoint
    // sequence is a hard FAIL (a real drop/reorder the tap-vs-tap diff missed), so
    // it overrides an otherwise-INCONCL/Pass overall_v to FAIL.
    let overall = if !endpoint_seq_pass {
        "FAIL"
    } else {
        match overall_v {
            HopVerdict::Pass => "PASS",
            HopVerdict::Fail => "FAIL",
            HopVerdict::Inconclusive => "INCONCL",
        }
    };
    println!("VERDICT={overall} ARTIFACT={}", args.out);

    if verdict_pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn taps() -> HashSet<&'static str> {
        ["strih", "stream"].into_iter().collect()
    }

    #[test]
    fn orphan_bound_key_is_rejected() {
        // A typo'd / renamed key must FAIL loudly, not silently no-op the gate.
        let err = validate_bound_keys(
            &[("striih".to_string(), 130.0)],
            &taps(),
            "--max-p99-latency-ms",
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("striih"));
    }

    #[test]
    fn matching_bound_keys_pass() {
        let bounds = [("strih".to_string(), 130.0), ("stream".to_string(), 220.0)];
        assert!(validate_bound_keys(&bounds, &taps(), "--max-p99-latency-ms").is_ok());
    }

    #[test]
    fn no_bounds_pass() {
        assert!(validate_bound_keys(&[], &taps(), "--max-freeze-periods").is_ok());
    }

    #[test]
    fn min_single_copy_rejects_negative_and_fractional() {
        // A frame-count guard must be a non-negative integer; a typo'd negative or
        // fractional value must FAIL loudly, not silently saturate/truncate to a
        // weaker-or-disabled gate.
        assert!(
            validate_count_bounds(&[("strih".to_string(), -5.0)], "--min-single-copy").is_err()
        );
        assert!(validate_count_bounds(&[("strih".to_string(), 2.7)], "--min-single-copy").is_err());
        assert!(validate_count_bounds(&[("strih".to_string(), 20.0)], "--min-single-copy").is_ok());
        assert!(validate_count_bounds(&[("strih".to_string(), 0.0)], "--min-single-copy").is_ok());
    }

    #[test]
    fn parse_bound_rejects_non_numeric_and_empty_name() {
        assert!(parse_bound("strih=abc").is_err());
        assert!(parse_bound("=130").is_err());
        assert_eq!(
            parse_bound("stream=220").unwrap(),
            ("stream".to_string(), 220.0)
        );
    }
}
