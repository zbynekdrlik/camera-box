//! multitap-probe: subscribe to N NDI taps on dev1, difference adjacent pairs,
//! emit one JSON artifact with a per-hop frame-loss + latency report, exit
//! non-zero on any real per-hop drop/reorder. Painter runs separately on the
//! camera box (frame-probe --paint-only) with the same --run-id.

use anyhow::{bail, Result};
use camera_box::probe::differ::{diff_hop, HopInput, HopReport};
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

#[derive(Serialize)]
struct MultiTapReport {
    run_id: u32,
    duration_secs: u64,
    taps: Vec<TapSummary>,
    hops: Vec<HopReport>,
    absolute_latency: String,
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
    /// QR-decode-failure floor — frames that ARRIVED but tore. Comparing a
    /// downstream tap's `captured` against the upstream tap's output proves
    /// whether id-level `dropped_ids` is true hop loss or just tap decode tears.
    decoded: u64,
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
    let stop_ns = start.elapsed().as_nanos() as i64;
    for h in handles {
        h.join().expect("tap thread panicked")?;
    }

    // Snapshot + trim the trailing settle window (in-flight frames are not drops).
    let cutoff_ns = stop_ns - (args.settle_ms as i64) * 1_000_000;
    let trimmed: Vec<Vec<_>> = results
        .iter()
        .map(|r| {
            r.observed
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.recv_ts_ns <= cutoff_ns)
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();

    // Per-hop gate bounds, keyed by the hop's DOWNSTREAM tap name (the tap the
    // hop feeds). Absent ⇒ None ⇒ report-only.
    let p99_bounds: std::collections::HashMap<String, f64> =
        args.max_p99_latency_ms.iter().cloned().collect();
    let freeze_bounds: std::collections::HashMap<String, f64> =
        args.max_freeze_periods.iter().cloned().collect();

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
        })
        .collect();

    let verdict_pass = hops.iter().all(|h| h.pass);
    let report = MultiTapReport {
        run_id: args.run_id,
        duration_secs: args.duration_secs,
        taps: tap_summaries,
        hops,
        absolute_latency: "UNAVAILABLE — clock not synced (Phase 3 / #8)".to_string(),
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
        println!(
            "HOP {} {} up_unique={} down_unique={} dropped={} reorders={} freezes={}",
            h.name,
            if h.pass { "PASS" } else { "FAIL" },
            h.upstream_unique,
            h.downstream_unique,
            h.dropped_ids.len(),
            h.reorders.len(),
            h.freezes.len(),
        );
        if let Some(l) = &h.latency {
            println!(
                "  REL_LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={})",
                l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
            );
        }
    }
    println!(
        "VERDICT={} ARTIFACT={}",
        if verdict_pass { "PASS" } else { "FAIL" },
        args.out
    );

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
    fn parse_bound_rejects_non_numeric_and_empty_name() {
        assert!(parse_bound("strih=abc").is_err());
        assert!(parse_bound("=130").is_err());
        assert_eq!(
            parse_bound("stream=220").unwrap(),
            ("stream".to_string(), 220.0)
        );
    }
}
