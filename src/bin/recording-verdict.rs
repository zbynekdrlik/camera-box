//! recording-verdict: #107 hard-fail loss verdict from recorded OBS program files.
//!
//! Consumes the #106 recorded-file decode (NOT an NDI tap, NOT the lz4 spool) and
//! produces a HARD-FAIL zero-loss verdict with NO thresholds:
//!
//!   PASS = 0 undecodable AND 0 net copy AND 0 net gap AND analyzed span ≥ 300 s.
//!
//! The free-running 60→30 camera-sampling beat (mean step exactly 2.0, symmetric)
//! is recognized and NOT counted as loss; only the NET imbalance is real loss.
//!
//! Usage:
//!   recording-verdict --strih strih.mkv [--stream stream.mkv] \
//!       [--painter painter.csv] [--out-dir run-dir] [--min-secs 300]
//!
//! - `--strih` the strih OBS-program recording (the strict hop-1 endpoint).
//! - `--stream` the stream OBS-program recording (the headline endpoint). When
//!   present, the strih→stream hop is verdicted by a direct per-frame tick compare
//!   (the camera beat is common, so it cancels).
//! - `--painter` a CSV of the cam2 painter's displayed logical ticks (one `tick`
//!   per line or the recording-probe `frame_index,n_qr,tick,...` CSV). Enables the
//!   honest cam→strih assessment (no false zero claim).
//! - `--out-dir` where pixel-proof PNGs of every flagged frame are written.
//!
//! Exit code: 0 on PASS for every verdict, non-zero on ANY fail.

use anyhow::{Context, Result};
use camera_box::probe::recording::{
    analyze_recording, extract_frames_png, select_frames_to_extract, RecordingFrame,
    DEFAULT_MAX_PIXEL_PROOF,
};
use camera_box::probe::recording_4node::{cam1_optical_assessment, cam1_strih_verdict};
use camera_box::probe::recording_latency::{
    burn_ids_in, cam2_cam1_samples, cam_strih_samples, chain_hop_loss_from_stream,
    chain_hop_samples_from_stream, hop_latency, strih_stream_samples,
    strih_stream_samples_from_stream, HopLatency, RunIds, BURN_RUN_ID_CAM1, BURN_RUN_ID_STREAM,
    BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, strih_stream_verdict, verdict, BurnHopVerdict, FrameTick,
    RecordingVerdict, VerdictConfig,
};
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(about = "Hard-fail zero-loss verdict from recorded OBS program files (#107)")]
struct Args {
    /// strih OBS-program recording (.mkv / .mp4) — the strict hop-1 endpoint.
    /// Optional: omit it for a cam1-ONLY optical-readability check (the fast PART-B
    /// loop — `--cam1 grab.mkv --painter painter.csv --cam2-run-id N`), which decodes
    /// just the cam1 grab and reports the cam2→cam1 decode rate without a 4-node run.
    #[arg(long)]
    strih: Option<PathBuf>,
    /// stream OBS-program recording — the headline endpoint. Enables strih→stream.
    #[arg(long)]
    stream: Option<PathBuf>,
    /// cam1 GRAB recording (#105 node 2) — the camera-box `--record-grab` mkv of
    /// cam1's filmed frames. Enables the STRICT cam1→strih hop verdict and the HONEST
    /// cam2→cam1 optical assessment (and, with --cam1-grab-ts, the cam2→cam1 latency).
    #[arg(long)]
    cam1: Option<PathBuf>,
    /// cam1 grab-timestamp SIDECAR CSV (`frame_index,grab_ts_ns`) the `--record-grab`
    /// mode writes — cam1's per-frame GRAB instant on the DanteSync wall clock. With
    /// --cam1 it yields the REAL cam2→cam1 optical+grab latency (no #111 burn needed).
    #[arg(long)]
    cam1_grab_ts: Option<PathBuf>,
    /// CSV of the cam2 painter's displayed ticks (enables the cam→strih assessment).
    #[arg(long)]
    painter: Option<PathBuf>,
    /// Directory for pixel-proof PNGs of flagged frames.
    #[arg(long, default_value = "recording-verdict-run")]
    out_dir: PathBuf,
    /// Minimum analyzed span (s) before a zero-loss PASS may be declared.
    #[arg(long, default_value_t = 300.0)]
    min_secs: f64,
    /// Cap on pixel-proof PNGs written per recording (the first N flagged frames by
    /// index). The verdict needs only a handful of visual examples; extracting
    /// thousands of PNGs was a large slice of the runtime (#166). `0` = no cap.
    #[arg(long, default_value_t = DEFAULT_MAX_PIXEL_PROOF)]
    max_pixel_proof: usize,
    /// Camera capture fps (for the duration gate).
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// Monitor refresh Hz of the painted logical counter.
    #[arg(long, default_value_t = 60.0)]
    refresh_hz: f64,
    /// #108 per-hop ABSOLUTE latency: the strih node's burn-QR run_id (the value
    /// `OBS_BURN_RUN_ID` was set to on strih; default mirrors the #111 burn filter).
    /// When present in the strih recording, cam→strih latency is computed
    /// (strih_burn.gen_ts_ns − cam2.gen_ts_ns).
    #[arg(long, default_value_t = BURN_RUN_ID_STRIH)]
    burn_strih_run_id: u32,
    /// #108 per-hop ABSOLUTE latency: the stream node's burn-QR run_id. When both
    /// recordings carry their node burn, strih→stream latency is computed
    /// (stream_burn.gen_ts_ns − strih_burn.gen_ts_ns, paired by cam2 tick).
    #[arg(long, default_value_t = BURN_RUN_ID_STREAM)]
    burn_stream_run_id: u32,
    /// #174: the cam1-CAPTURE burn run_id (the value `CAMERA_BOX_BURN_RUN_ID` was set to
    /// on cam1). cam1's render-time burn rides through NDI into strih's program and on into
    /// stream's, so the SINGLE stream recording carries it; when present, the full chain
    /// cam1→strih→stream is verdicted by the CLEAN digital burn-id (loss + latency), with
    /// no 60→30 optical-beat ambiguity. When absent in the recording these hops report no
    /// samples (never a wrong number). Default mirrors the cam1 burn's reserved id.
    #[arg(long, default_value_t = BURN_RUN_ID_CAM1)]
    burn_cam1_run_id: u32,
    /// #108: cam2's painter run_id (the `--run-id` the cam2 painter used). When set,
    /// cam2's QR is matched EXACTLY by this run_id, so the strih burn forwarded into
    /// the stream recording can NEVER be mistaken for cam2. Strongly recommended for
    /// strih→stream. Unset (0) ⇒ cam2 = the first non-burn QR (safe for the strih
    /// recording, which has no foreign burn).
    #[arg(long, default_value_t = 0)]
    cam2_run_id: u32,
    /// #105 4-node report: write a machine-readable JSON summary (per-node verdict +
    /// per-hop loss + per-hop latency) to this path, consumed by
    /// scripts/recording-e2e-report.py to render the 2-graph report PNG.
    #[arg(long)]
    json: Option<PathBuf>,
}

/// Reduce a HopLatency option to a compact JSON object (or null) for the report.
fn hop_lat_json(h: &Option<HopLatency>) -> serde_json::Value {
    match h {
        Some(h) => serde_json::json!({
            "samples": h.samples,
            "p50_ms": h.stats.p50_ms, "p95_ms": h.stats.p95_ms, "p99_ms": h.stats.p99_ms,
            "min_ms": h.stats.min_ms, "mean_ms": h.stats.mean_ms, "max_ms": h.stats.max_ms,
            "jitter_ms": h.jitter_ms, "drift_ms_per_min": h.drift_ms_per_min,
        }),
        None => serde_json::Value::Null,
    }
}

/// Print a clean burn-id hop verdict (#174) and return its pass.
fn report_burn_hop(v: &BurnHopVerdict) {
    println!(
        "  [{}] LOSS (burn-id): compared_ids={} dropped={} phantom={} -> {}",
        v.hop,
        v.compared_ids,
        v.dropped_ids.len(),
        v.phantom_ids.len(),
        if v.is_pass() { "PASS" } else { "FAIL" }
    );
    if !v.dropped_ids.is_empty() {
        let shown: Vec<u32> = v.dropped_ids.iter().copied().take(20).collect();
        println!("    dropped burn ids (first 20, hop lost these): {shown:?}");
    }
    if !v.phantom_ids.is_empty() {
        let shown: Vec<u32> = v.phantom_ids.iter().copied().take(20).collect();
        println!("    phantom burn ids (first 20, downstream-only): {shown:?}");
    }
}

/// Reduce a BurnHopVerdict to a compact JSON object for the report.
fn burn_hop_json(v: &BurnHopVerdict) -> serde_json::Value {
    serde_json::json!({
        "pass": v.is_pass(), "compared_ids": v.compared_ids,
        "dropped": v.dropped_ids.len(), "phantom": v.phantom_ids.len(),
    })
}

/// Parse the painter ground-truth ticks from any of the THREE shapes the harness
/// produces, selecting the tick column from the HEADER:
///
/// - `--paint-log` CSV (the cam2 painter ground truth, [`serialize_painter_log`]):
///   header `tick,gen_ts_ns` ⇒ tick is column 0.
/// - recording-probe CSV: header `frame_index,n_qr,tick,run_id,frame_ids` ⇒ tick is
///   column 2.
/// - a bare one-`tick`-per-line file (no header, no comma) ⇒ the whole line is the tick.
///
/// A comma-containing data row with too few columns for the detected layout is a
/// MALFORMED CSV — error loudly (a silently-shrunk painter set would manufacture false
/// phantom faults). Pure (operates on the file text) so the column-detection is
/// unit-testable without a file.
fn parse_painter_ticks_str(text: &str) -> Result<Vec<u32>> {
    // Detect the tick column from the first non-blank line if it is a known header.
    let header = text.lines().map(str::trim).find(|l| !l.is_empty());
    let tick_col: usize = match header {
        Some(h) if h.starts_with("tick,") => 0, // --paint-log: tick,gen_ts_ns
        Some(h) if h.starts_with("frame_index") => 2, // recording-probe: ..,..,tick,..
        _ => 0,                                 // bare one-tick-per-line
    };
    let mut ticks = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        // Skip blanks and either known header line.
        if line.is_empty() || line.starts_with("frame_index") || line.starts_with("tick,") {
            continue;
        }
        let field = if line.contains(',') {
            line.split(',').nth(tick_col).with_context(|| {
                format!(
                    "painter CSV row at line {} has too few columns for the detected \
                     tick column {tick_col}: {line:?}",
                    lineno + 1
                )
            })?
        } else {
            line // bare file: the whole line is the tick
        };
        let field = field.trim();
        if field.is_empty() {
            continue; // an undecodable recording-probe row has an empty tick column
        }
        let t: u32 = field
            .parse()
            .with_context(|| format!("painter tick not a u32 at line {}: {field:?}", lineno + 1))?;
        ticks.push(t);
    }
    Ok(ticks)
}

/// Read + parse the painter ground-truth ticks from a file (see
/// [`parse_painter_ticks_str`] for the accepted shapes).
fn parse_painter_ticks(path: &Path) -> Result<Vec<u32>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read painter ticks {}", path.display()))?;
    let ticks = parse_painter_ticks_str(&text)
        .with_context(|| format!("parse painter ticks {}", path.display()))?;
    tracing::info!(file = %path.display(), ticks = ticks.len(), "painter ticks parsed");
    Ok(ticks)
}

/// Parse the cam1 grab-timestamp sidecar CSV (`frame_index,grab_ts_ns`, header
/// `frame_index,grab_ts_ns`) the `--record-grab` mode writes into a
/// `frame_index → grab_ts_ns` map. A malformed row (wrong column count, non-integer)
/// errors loudly rather than silently dropping — a silently-shrunk grab-ts map would
/// drop legitimate cam2→cam1 latency samples without any signal.
fn parse_grab_ts(path: &Path) -> Result<std::collections::HashMap<u64, i64>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read cam1 grab-ts sidecar {}", path.display()))?;
    let mut m = std::collections::HashMap::new();
    // Kill-time partial-row tolerance (cam1 --record-grab is a BufWriter killed at teardown
    // with NO flush, so the file is cut at an arbitrary byte boundary). A COMPLETE row always
    // ends with '\n' (the writeln! emits the newline LAST, after the full `idx,ts` payload),
    // so a file that does NOT end in '\n' has exactly one partial final line — of ANY shape
    // ("8874,", "8874", "8874,17820"-truncated). That final fragment is skipped, whatever it
    // is; every earlier (newline-terminated) row is parsed STRICTLY. A newline-terminated
    // malformed row is genuine corruption (not a kill cut) and still errors loudly — a
    // silently-shrunk grab-ts map would drop / corrupt real cam2→cam1 latency samples.
    let has_trailing_newline = text.ends_with('\n');
    // The byte-offset line index of the LAST non-blank, non-header data line (only meaningful
    // when there is NO trailing newline — then THIS line is the partial fragment to skip).
    let last_data_line = text
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with("frame_index")
        })
        .map(|(i, _)| i)
        .last();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("frame_index") {
            continue; // header / blank
        }
        // A no-trailing-newline file's final data line is a partial kill-time fragment — skip
        // it whatever its shape (empty ts, no comma, truncated digits). Everything else parses.
        if !has_trailing_newline && Some(lineno) == last_data_line {
            tracing::warn!(
                line = lineno + 1,
                fragment = %line,
                "grab-ts final row has no trailing newline (partial kill-time write) — skipped"
            );
            continue;
        }
        let mut it = line.split(',');
        let idx_s = it
            .next()
            .with_context(|| format!("grab-ts row at line {} is empty: {line:?}", lineno + 1))?;
        let ts_s = it.next().with_context(|| {
            format!(
                "grab-ts row at line {} has <2 columns (expected frame_index,grab_ts_ns): {line:?}",
                lineno + 1
            )
        })?;
        let idx: u64 = idx_s.trim().parse().with_context(|| {
            format!(
                "grab-ts frame_index not a u64 at line {}: {idx_s:?}",
                lineno + 1
            )
        })?;
        let ts: i64 = ts_s.trim().parse().with_context(|| {
            format!(
                "grab-ts grab_ts_ns not an i64 at line {}: {ts_s:?}",
                lineno + 1
            )
        })?;
        m.insert(idx, ts);
    }
    tracing::info!(file = %path.display(), entries = m.len(), "cam1 grab-ts sidecar parsed");
    Ok(m)
}

/// Analyze a recording into the per-frame tick stream (the #106 decode → #107 input).
fn ticks_of(path: &Path) -> Result<(Vec<RecordingFrame>, Vec<FrameTick>)> {
    let frames =
        analyze_recording(path).with_context(|| format!("analyze recording {}", path.display()))?;
    let ticks = FrameTick::from_recording_frames(&frames);
    Ok((frames, ticks))
}

/// Print a per-recording verdict and extract pixel proof for its flagged frames.
/// Returns whether the verdict PASSed.
fn report_recording(
    label: &str,
    path: &Path,
    v: &RecordingVerdict,
    out_dir: &Path,
    max_pixel_proof: usize,
) -> Result<bool> {
    println!("=== {label} verdict ({}) ===", path.display());
    println!(
        "  frames={} analyzed={:.1}s duration_ok={} avg_step={:.4} beat_balanced={}",
        v.total_frames, v.analyzed_secs, v.duration_ok, v.avg_step, v.beat_balanced
    );
    if v.lead_in_trimmed > 0 || v.lead_out_trimmed > 0 {
        println!(
            "  leading-discard: {} pre-signal (console lead-in) + {} post-signal (teardown) \
             frames trimmed — NOT counted as undecodable",
            v.lead_in_trimmed, v.lead_out_trimmed
        );
    }
    println!(
        "  undecodable={} real_copy={} real_gap={}",
        v.undecodable_frames.len(),
        v.real_copy_frames.len(),
        v.real_gap_frames.len()
    );
    if !v.duration_ok {
        println!(
            "  DURATION GATE: analyzed span {:.1}s < {:.1}s — zero-loss PASS refused",
            v.analyzed_secs, v.min_secs
        );
    }

    // Extract pixel proof for EVERY flagged frame (undecodable + real loss).
    let undecodable: HashSet<u64> = v.undecodable_frames.iter().copied().collect();
    let mut flagged: Vec<u64> = v
        .undecodable_frames
        .iter()
        .chain(v.real_copy_frames.iter())
        .chain(v.real_gap_frames.iter())
        .copied()
        .collect();
    flagged.sort_unstable();
    flagged.dedup();

    if !flagged.is_empty() {
        let png_dir = out_dir.join(label);
        let (_selected, dropped) = select_frames_to_extract(&flagged, max_pixel_proof);
        if dropped > 0 {
            println!(
                "  PIXEL-PROOF CAP: {} flagged frames, extracting only the first {} PNGs ({} not \
                 extracted — verdict counts above are COMPLETE; raise --max-pixel-proof or pass 0 \
                 for all)",
                flagged.len(),
                flagged.len() - dropped,
                dropped
            );
        }
        let extracted =
            extract_frames_png(path, &flagged, &undecodable, &png_dir, max_pixel_proof)?;
        for e in &extracted {
            if e.sharp_qr_but_flagged_undecodable {
                println!(
                    "  DECODER BUG (Step-1/#106 regression): frame {} flagged undecodable but a \
                     SHARP QR decodes in the pixels -> {}",
                    e.frame_index,
                    e.png_path.display()
                );
            } else {
                println!(
                    "  LOSS PROOF: frame {} -> {} (black/garbage = real loss)",
                    e.frame_index,
                    e.png_path.display()
                );
            }
        }
    }

    let pass = v.is_pass();
    println!("  RESULT: {}", if pass { "PASS" } else { "FAIL" });
    Ok(pass)
}

/// Print one #108 per-hop ABSOLUTE latency block (p50, p99, jitter, drift). Returns
/// whether a non-empty hop was computed (so a recording carrying no burn QR is
/// reported as such rather than silently omitted).
fn report_hop_latency(h: &Option<HopLatency>, label: &str, anchor: &str) -> bool {
    match h {
        Some(h) => {
            println!("=== {label} per-hop ABSOLUTE latency (#108, anchor: {anchor}) ===");
            println!(
                "  samples={} p50={:.2}ms p99={:.2}ms jitter(p99-p50)={:.2}ms drift={:+.4}ms/min",
                h.samples, h.stats.p50_ms, h.stats.p99_ms, h.jitter_ms, h.drift_ms_per_min
            );
            println!(
                "  (min={:.2} mean={:.2} p95={:.2} max={:.2} ms)",
                h.stats.min_ms, h.stats.mean_ms, h.stats.p95_ms, h.stats.max_ms
            );
            true
        }
        None => {
            println!(
                "=== {label} per-hop ABSOLUTE latency (#108) ===\n  NO SAMPLES — no node burn QR \
                 paired in the recording(s). Enable the #111 burn (OBS_BURN_QR) on the PROBE scene \
                 and pass the matching --burn-*-run-id."
            );
            false
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    tracing::info!(
        strih = ?args.strih.as_ref().map(|p| p.display().to_string()),
        stream = ?args.stream.as_ref().map(|p| p.display().to_string()),
        painter = ?args.painter.as_ref().map(|p| p.display().to_string()),
        out_dir = %args.out_dir.display(),
        min_secs = args.min_secs,
        "recording-verdict start"
    );

    let cfg = VerdictConfig {
        capture_fps: args.capture_fps,
        min_secs: args.min_secs,
        refresh_hz: args.refresh_hz,
    };

    let mut all_pass = true;
    // #105 4-node machine-readable report (per-node verdict + per-hop loss + latency),
    // built incrementally and written to --json for the 2-graph report renderer.
    let mut report = serde_json::json!({ "nodes": {}, "hops": {}, "latency": {} });

    // strih recording verdict (strict hop-1 endpoint). Keep the decoded frames so the
    // #108 per-hop latency engine can read each frame's cam2 + node-burn gen_ts_ns.
    // `--strih` is OPTIONAL: when omitted (the cam1-only optical-readability loop) the
    // strih-dependent hops (cam→strih, strih→stream) are skipped and only the cam1
    // grab is decoded/assessed.
    let strih_data: Option<(Vec<RecordingFrame>, Vec<FrameTick>)> = match &args.strih {
        Some(strih_path) => {
            let (strih_frames, strih_ticks) = ticks_of(strih_path)?;
            let strih_v = verdict(&strih_ticks, &cfg);
            all_pass &= report_recording(
                "strih",
                strih_path,
                &strih_v,
                &args.out_dir,
                args.max_pixel_proof,
            )?;
            report["nodes"]["strih"] = serde_json::json!({
                "pass": strih_v.is_pass(), "frames": strih_v.total_frames,
                "analyzed_secs": strih_v.analyzed_secs, "undecodable": strih_v.undecodable_frames.len(),
                "avg_step": strih_v.avg_step, "beat_balanced": strih_v.beat_balanced,
            });
            Some((strih_frames, strih_ticks))
        }
        None => {
            println!(
                "=== strih: SKIPPED (no --strih) — cam1-only optical-readability mode; \
                 cam→strih and strih→stream hops are unavailable ==="
            );
            None
        }
    };

    // stream recording verdict (headline endpoint) + strih→stream direct compare.
    let mut stream_frames_opt: Option<Vec<RecordingFrame>> = None;
    if let Some(stream_path) = &args.stream {
        let (stream_frames, stream_ticks) = ticks_of(stream_path)?;
        let stream_v = verdict(&stream_ticks, &cfg);
        all_pass &= report_recording(
            "stream",
            stream_path,
            &stream_v,
            &args.out_dir,
            args.max_pixel_proof,
        )?;

        report["nodes"]["stream"] = serde_json::json!({
            "pass": stream_v.is_pass(), "frames": stream_v.total_frames,
            "analyzed_secs": stream_v.analyzed_secs,
            "undecodable": stream_v.undecodable_frames.len(),
        });
        // strih→stream direct compare needs the strih recording; skip it in cam1-only mode.
        if let Some((_, strih_ticks)) = &strih_data {
            let ss = strih_stream_verdict(strih_ticks, &stream_ticks, &cfg);
            println!(
                "=== strih→stream hop verdict (direct tick-SEQUENCE compare, offset-immune) ==="
            );
            println!(
                "  compared_ticks={} strih_only(stream dropped)={} stream_only(reorder/phantom)={}",
                ss.compared_ticks,
                ss.strih_only_ticks.len(),
                ss.stream_only_ticks.len()
            );
            if !ss.strih_only_ticks.is_empty() {
                let shown: Vec<u32> = ss.strih_only_ticks.iter().copied().take(20).collect();
                println!("  strih-only ticks (first 20, = stream dropped these): {shown:?}");
            }
            if !ss.stream_only_ticks.is_empty() {
                let shown: Vec<u32> = ss.stream_only_ticks.iter().copied().take(20).collect();
                println!("  stream-only ticks (first 20): {shown:?}");
            }
            println!("  RESULT: {}", if ss.is_pass() { "PASS" } else { "FAIL" });
            all_pass &= ss.is_pass();
            report["hops"]["strih_stream"] = serde_json::json!({
                "strict": true, "pass": ss.is_pass(), "compared_ticks": ss.compared_ticks,
                "dropped": ss.strih_only_ticks.len(), "phantom": ss.stream_only_ticks.len(),
            });
        }
        stream_frames_opt = Some(stream_frames);
    }

    // cam1 GRAB node (#105 node 2): per-recording continuity verdict, the STRICT
    // cam1→strih hop, and the HONEST cam2→cam1 optical assessment + latency. cam1's
    // grab and strih's program carry the SAME camera frames, so cam1→strih is a strict
    // offset-immune tick-SET compare (the camera beat cancels) — a cam1 tick absent at
    // strih is a real cam1→strih DROP.
    let mut cam1_frames_opt: Option<Vec<RecordingFrame>> = None;
    if let Some(cam1_path) = &args.cam1 {
        let (cam1_frames, cam1_ticks) = ticks_of(cam1_path)?;

        // cam1's own per-recording continuity (undecodable / net copy/gap WITHIN cam1).
        let cam1_v = verdict(&cam1_ticks, &cfg);
        all_pass &= report_recording(
            "cam1",
            cam1_path,
            &cam1_v,
            &args.out_dir,
            args.max_pixel_proof,
        )?;
        report["nodes"]["cam1"] = serde_json::json!({
            "pass": cam1_v.is_pass(), "frames": cam1_v.total_frames,
            "analyzed_secs": cam1_v.analyzed_secs, "undecodable": cam1_v.undecodable_frames.len(),
            "avg_step": cam1_v.avg_step, "beat_balanced": cam1_v.beat_balanced,
        });

        // STRICT cam1→strih hop. The returned verdict's `strih_only_ticks` are the
        // CAM1-only ticks (strih dropped them) and `stream_only_ticks` are the
        // STRIH-only ticks (phantom/reorder) — relabelled here for the cam1→strih hop.
        // Needs the strih recording; skipped in cam1-only optical-readability mode.
        if let Some((_, strih_ticks)) = &strih_data {
            let cs = cam1_strih_verdict(&cam1_ticks, strih_ticks, &cfg);
            println!(
                "=== cam1→strih hop verdict (STRICT, direct tick-SEQUENCE compare, offset-immune) ==="
            );
            println!(
                "  compared_ticks={} cam1_only(strih dropped)={} strih_only(phantom/reorder)={}",
                cs.compared_ticks,
                cs.strih_only_ticks.len(),
                cs.stream_only_ticks.len()
            );
            if !cs.strih_only_ticks.is_empty() {
                let shown: Vec<u32> = cs.strih_only_ticks.iter().copied().take(20).collect();
                println!(
                    "  cam1-only ticks (first 20, = strih DROPPED these on cam1→strih): {shown:?}"
                );
            }
            if !cs.stream_only_ticks.is_empty() {
                let shown: Vec<u32> = cs.stream_only_ticks.iter().copied().take(20).collect();
                println!("  strih-only ticks (first 20, = phantom/reorder at strih): {shown:?}");
            }
            println!("  RESULT: {}", if cs.is_pass() { "PASS" } else { "FAIL" });
            all_pass &= cs.is_pass();
            report["hops"]["cam1_strih"] = serde_json::json!({
                "strict": true, "pass": cs.is_pass(), "compared_ticks": cs.compared_ticks,
                "dropped": cs.strih_only_ticks.len(), "phantom": cs.stream_only_ticks.len(),
            });
        }

        // HONEST cam2→cam1 optical assessment (vs painter ground truth, never a zero claim).
        if let Some(painter_path) = &args.painter {
            let painter_ticks = parse_painter_ticks(painter_path)?;
            let a = cam1_optical_assessment(&cam1_ticks, &painter_ticks, &cfg);
            println!("=== cam2→cam1 OPTICAL assessment (honest, NOT a zero-loss claim) ===");
            println!(
                "  unknown_ticks (in-range, never painted = real optical fault)={}",
                a.unknown_ticks.len()
            );
            println!(
                "  out_of_painter_range_ticks (painter CSV didn't cover)={}",
                a.out_of_painter_range_ticks.len()
            );
            if !a.unknown_ticks.is_empty() {
                let shown: Vec<u32> = a.unknown_ticks.iter().copied().take(20).collect();
                println!("  unknown ticks (first 20): {shown:?}");
                all_pass = false; // an in-range never-painted tick is a provable fault
            }
            report["hops"]["cam2_cam1"] = serde_json::json!({
                "strict": false, "claims_zero_loss": a.claims_zero_loss,
                "unknown_ticks": a.unknown_ticks.len(),
                "out_of_painter_range_ticks": a.out_of_painter_range_ticks.len(),
            });
        }
        cam1_frames_opt = Some(cam1_frames);
    }

    // cam→strih honest assessment (no false zero claim). Needs both strih + painter;
    // skipped in cam1-only optical-readability mode.
    let mut cam_strih_clean: Option<bool> = None;
    if let (Some((_, strih_ticks)), Some(painter_path)) = (&strih_data, &args.painter) {
        let painter_ticks = parse_painter_ticks(painter_path)?;
        let a = cam_strih_assessment(strih_ticks, &painter_ticks, &cfg);
        println!("=== cam→strih assessment (honest, NOT a zero-loss claim) ===");
        println!("  claims_zero_loss={}", a.claims_zero_loss);
        println!(
            "  unknown_ticks (in-range, never painted = real fault)={}",
            a.unknown_ticks.len()
        );
        println!(
            "  out_of_painter_range_ticks (uncertain, painter CSV didn't cover)={}",
            a.out_of_painter_range_ticks.len()
        );
        if !a.unknown_ticks.is_empty() {
            let shown: Vec<u32> = a.unknown_ticks.iter().copied().take(20).collect();
            println!("  unknown ticks (first 20): {shown:?}");
            all_pass = false; // an in-range never-painted tick is a provable fault
        }
        println!("  LIMITATION: {}", a.limitation);
        cam_strih_clean = Some(a.unknown_ticks.is_empty());
        report["hops"]["cam_strih"] = serde_json::json!({
            "strict": false, "claims_zero_loss": a.claims_zero_loss,
            "unknown_ticks": a.unknown_ticks.len(),
            "out_of_painter_range_ticks": a.out_of_painter_range_ticks.len(),
        });
    }

    // #108 — per-hop ABSOLUTE latency from the in-frame node-burn + cam2 gen_ts_ns
    // stamps (the #111 burn must be live on the boxes for these to be non-empty). NO
    // networked record_start, NO idx/30 — every number is a difference of two stamps
    // that already share the DanteSync wall clock. Reported, never gated (a latency
    // gate is a separate decision; #108 asks for the stable, defined numbers).
    println!();
    let cam2_pin = if args.cam2_run_id != 0 {
        Some(args.cam2_run_id)
    } else {
        None
    };
    // strih recording: node burn = strih; no foreign burn forwarded INTO strih.
    let strih_ids = RunIds {
        node_burn: args.burn_strih_run_id,
        cam2: cam2_pin,
        other_burns: vec![],
    };
    // cam→strih ABSOLUTE latency needs the strih recording (its in-frame strih-burn +
    // cam2 stamps). Skipped in cam1-only optical-readability mode.
    if let Some((strih_frames, _)) = &strih_data {
        let cam_strih_lat = hop_latency("cam→strih", &cam_strih_samples(strih_frames, &strih_ids));
        report_hop_latency(&cam_strih_lat, "cam→strih", "cam2 paint gen_ts_ns");
        report["latency"]["cam_strih"] = hop_lat_json(&cam_strih_lat);
    }

    // cam2→cam1 OPTICAL+GRAB latency (#105 node 2) — REAL, no #111 burn needed.
    // grab_ts (cam1 grab instant, sidecar) − cam2 paint gen_ts, both wall clock.
    if let Some(cam1_frames) = &cam1_frames_opt {
        match &args.cam1_grab_ts {
            Some(grab_ts_path) => {
                let grab_ts = parse_grab_ts(grab_ts_path)?;
                let cam2_pin_c1 = if args.cam2_run_id != 0 {
                    Some(args.cam2_run_id)
                } else {
                    None
                };
                let c1_lat = hop_latency(
                    "cam2→cam1",
                    &cam2_cam1_samples(cam1_frames, &grab_ts, cam2_pin_c1),
                );
                report_hop_latency(&c1_lat, "cam2→cam1 (optical+grab)", "cam2 paint gen_ts_ns");
                report["latency"]["cam2_cam1"] = hop_lat_json(&c1_lat);
            }
            None => println!(
                "=== cam2→cam1 (optical+grab) per-hop ABSOLUTE latency (#105 node 2) ===\n  \
                 RELATIVE/UNAVAILABLE — pass --cam1-grab-ts <sidecar.csv> (the --record-grab \
                 grab-timestamp log) to compute it (grab_ts − cam2 paint gen_ts, both wall clock)."
            ),
        }
        // cam1→strih ABSOLUTE latency needs strih's #111 burn paired against cam1's grab
        // instant; the #111 burn is NOT deployed, so this hop's absolute latency is marked
        // unavailable rather than faked. cam2→cam1 (above) and cam→strih (cam2→strih) ARE
        // available; cam1→strih = (cam→strih) − (cam2→cam1) once the burn lands.
        println!(
            "=== cam1→strih per-hop ABSOLUTE latency (#105 node 2) ===\n  \
             RELATIVE/UNAVAILABLE — needs the #111 strih burn QR (not deployed) paired with \
             cam1's grab instant. Derivable as (cam→strih) − (cam2→cam1) once #111 is live."
        );
    }
    if let Some(stream_frames) = &stream_frames_opt {
        // stream recording: node burn = stream; strih's burn is FOREIGN (forwarded in
        // the program feed) and MUST be excluded so it is never read as cam2.
        let stream_ids = RunIds {
            node_burn: args.burn_stream_run_id,
            cam2: cam2_pin,
            other_burns: vec![args.burn_strih_run_id],
        };
        // #111 PART A: prefer the WHOLE strih→stream hop from the STREAM recording
        // ALONE — the stream frames carry the FORWARDED strih burn + stream's own burn,
        // paired per cam2 tick, so the hop needs no separate strih recording (the
        // dispatch's "whole per-hop analysis from the single stream recording"). Fall
        // back to the two-recording method only when the strih burn is NOT forwarded
        // into the stream program (the from-stream pairing then yields no samples).
        let from_stream = strih_stream_samples_from_stream(
            stream_frames,
            cam2_pin,
            args.burn_strih_run_id,
            args.burn_stream_run_id,
        );
        let (ss_samples, source) = if !from_stream.is_empty() {
            (from_stream, "stream recording alone (forwarded strih burn)")
        } else if let Some((strih_frames, _)) = &strih_data {
            (
                strih_stream_samples(strih_frames, stream_frames, &strih_ids, &stream_ids),
                "two recordings (strih burn not forwarded into stream)",
            )
        } else {
            (
                Vec::new(),
                "unavailable (no forwarded strih burn, no --strih)",
            )
        };
        println!("  strih→stream latency source: {source}");
        let ss_lat = hop_latency("strih→stream", &ss_samples);
        report_hop_latency(&ss_lat, "strih→stream", "strih render gen_ts_ns");
        report["latency"]["strih_stream"] = hop_lat_json(&ss_lat);
        report["latency"]["strih_stream_source"] = serde_json::json!(source);
    }

    // ===================================================================================
    // #174 — FULL-CHAIN per-hop verdict from the SINGLE stream recording, paired on the
    // CLEAN DIGITAL BURN IDs. The cam1-capture burn (run_id = burn_cam1_run_id) rides
    // through NDI into strih's program and on into stream's, so ONE stream recording
    // carries every mark: cam2 optical dual-QR + cam1 burn + strih burn + stream burn.
    // Each hop pairs on the burn `frame_id` (the SAME integer end-to-end) — NOT the
    // 60→30 optical beat — so the 259-dropped-vs-real_gap=1 loss artifact and the
    // p99=3.4s latency outliers of run 1530670109 cannot recur. Computed ONLY when the
    // stream recording actually carries the burns (else each hop reports no samples).
    // ===================================================================================
    if let Some(stream_frames) = &stream_frames_opt {
        let cam1_ids = burn_ids_in(stream_frames, args.burn_cam1_run_id);
        let strih_ids_seq = burn_ids_in(stream_frames, args.burn_strih_run_id);
        let stream_ids_seq = burn_ids_in(stream_frames, args.burn_stream_run_id);
        let any_burn =
            !cam1_ids.is_empty() || !strih_ids_seq.is_empty() || !stream_ids_seq.is_empty();
        if any_burn {
            println!();
            println!(
                "=== #174 FULL-CHAIN per-hop verdict from the STREAM recording ALONE (clean burn-id pairing) ==="
            );
            println!(
                "  burn ids in stream recording: cam1={} strih={} stream={}",
                cam1_ids.len(),
                strih_ids_seq.len(),
                stream_ids_seq.len()
            );
            report["full_chain"]["burn_ids_present"] = serde_json::json!({
                "cam1": cam1_ids.len(), "strih": strih_ids_seq.len(), "stream": stream_ids_seq.len(),
            });

            // --- per-hop LOSS paired by the SHARED cam2 source tick (#181) ---
            // Each node stamps its OWN independent burn counter, so a set-equality
            // compare of the two burn-id SEQUENCES finds zero overlap (compared_ids=0
            // despite the burns being present — the #181 symptom). Instead pair on the
            // cam2 source tick every recorded frame carries (the SAME key the latency
            // pairing uses): a hop survived iff both endpoints' burns appear on a frame
            // for that cam2 tick; dropped = upstream present / downstream absent;
            // phantom = downstream present / upstream absent.
            // cam1→strih: cam1's forwarded burn vs strih's forwarded burn.
            if !cam1_ids.is_empty() && !strih_ids_seq.is_empty() {
                let v = chain_hop_loss_from_stream(
                    "cam1→strih",
                    stream_frames,
                    cam2_pin,
                    args.burn_cam1_run_id,
                    args.burn_strih_run_id,
                );
                report_burn_hop(&v);
                all_pass &= v.is_pass();
                report["full_chain"]["loss"]["cam1_strih"] = burn_hop_json(&v);
            }
            // strih→stream: strih's forwarded burn vs stream's own burn.
            if !strih_ids_seq.is_empty() && !stream_ids_seq.is_empty() {
                let v = chain_hop_loss_from_stream(
                    "strih→stream",
                    stream_frames,
                    cam2_pin,
                    args.burn_strih_run_id,
                    args.burn_stream_run_id,
                );
                report_burn_hop(&v);
                all_pass &= v.is_pass();
                report["full_chain"]["loss"]["strih_stream"] = burn_hop_json(&v);
            }

            // --- per-hop LATENCY co-located in one stream frame (no cam2-tick pairing) ---
            if !cam1_ids.is_empty() && !strih_ids_seq.is_empty() {
                let lat = hop_latency(
                    "cam1→strih",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_cam1_run_id,
                        args.burn_strih_run_id,
                    ),
                );
                report_hop_latency(&lat, "cam1→strih (burn-id)", "cam1 capture gen_ts_ns");
                report["full_chain"]["latency"]["cam1_strih"] = hop_lat_json(&lat);
            }
            if !strih_ids_seq.is_empty() && !stream_ids_seq.is_empty() {
                let lat = hop_latency(
                    "strih→stream",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_strih_run_id,
                        args.burn_stream_run_id,
                    ),
                );
                report_hop_latency(&lat, "strih→stream (burn-id)", "strih render gen_ts_ns");
                report["full_chain"]["latency"]["strih_stream"] = hop_lat_json(&lat);
            }
            // cam1→stream END-TO-END latency (cam1 capture → stream render), one frame.
            if !cam1_ids.is_empty() && !stream_ids_seq.is_empty() {
                let lat = hop_latency(
                    "cam1→stream",
                    &chain_hop_samples_from_stream(
                        stream_frames,
                        args.burn_cam1_run_id,
                        args.burn_stream_run_id,
                    ),
                );
                report_hop_latency(
                    &lat,
                    "cam1→stream (end-to-end, burn-id)",
                    "cam1 capture gen_ts_ns",
                );
                report["full_chain"]["latency"]["cam1_stream"] = hop_lat_json(&lat);
            }
        } else {
            println!(
                "=== #174 FULL-CHAIN burn-id verdict: SKIPPED — no cam1/strih/stream burn QR in the \
                 stream recording. Set CAMERA_BOX_BURN_RUN_ID on cam1 + OBS_BURN_QR on strih/stream \
                 (+ --burn-*-run-id) and re-run for the clean per-hop loss + latency."
            );
        }
    }

    // Record the headline verdict and write the machine-readable report (BEFORE any
    // exit, so a FAIL run still produces the JSON the report renderer consumes).
    report["overall_pass"] = serde_json::Value::Bool(all_pass);
    report["cam_strih_clean"] = match cam_strih_clean {
        Some(b) => serde_json::Value::Bool(b),
        None => serde_json::Value::Null,
    };
    report["min_secs"] = serde_json::json!(args.min_secs);
    if let Some(json_path) = &args.json {
        std::fs::write(json_path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("write report json {}", json_path.display()))?;
        tracing::info!(path = %json_path.display(), "4-node report JSON written");
    }

    // Headline. Be HONEST about scope: a clean strih(+stream) verdict proves the
    // DIGITAL path (per-output continuity + strih→stream hop) is zero-loss, but the
    // strih recording ALONE cannot certify cam→strih zero-loss (the camera beat
    // overlaps loss without a clean cam-side reference). Only qualify as full
    // cam→endpoint zero-loss when a painter ground truth was supplied AND clean.
    println!();
    if !all_pass {
        println!("OVERALL: FAIL");
        std::process::exit(1);
    }
    match cam_strih_clean {
        Some(true) => println!(
            "OVERALL: PASS — digital path zero-loss; cam→strih shows no in-range phantom \
             (necessary, NOT sufficient — see cam→strih limitation)"
        ),
        _ => println!(
            "OVERALL: PASS (DIGITAL PATH ONLY) — per-output continuity + strih→stream hop are \
             zero-loss. This is NOT a cam→strih / end-to-end zero-loss claim: the strih \
             recording alone cannot certify the optical hop. Supply --painter for the cam→strih \
             assessment."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_grab_ts, parse_painter_ticks_str};
    use std::io::Write;

    #[test]
    fn painter_ticks_parse_paint_log_format_tick_first_column() {
        // REGRESSION (#105 integration): the --paint-log ground truth is `tick,gen_ts_ns`
        // (tick in column 0). parse_painter_ticks must read column 0 for this header — the
        // bug the live smoke caught was it forcing column 2 ("too few columns" on the
        // header) and discarding the entire painter set.
        let csv = "tick,gen_ts_ns\n0,1782000000000\n1,1782000016000\n2,1782000033000\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![0, 1, 2], "paint-log tick is column 0");
    }

    #[test]
    fn painter_ticks_parse_recording_probe_format_tick_third_column() {
        // recording-probe CSV: frame_index,n_qr,tick,run_id,frame_ids ⇒ tick is column 2.
        let csv = "frame_index,n_qr,tick,run_id,frame_ids\n0,2,100,7,100;99\n1,2,102,7,102;101\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![100, 102], "recording-probe tick is column 2");
    }

    #[test]
    fn painter_ticks_parse_bare_one_per_line() {
        // A bare file (no header, no comma): the whole line is the tick.
        let ticks = parse_painter_ticks_str("10\n11\n12\n").unwrap();
        assert_eq!(ticks, vec![10, 11, 12]);
    }

    #[test]
    fn painter_ticks_skip_empty_recording_probe_tick_column() {
        // An undecodable recording-probe row has an empty tick column → skipped, not error.
        let csv = "frame_index,n_qr,tick,run_id,frame_ids\n0,2,100,7,x\n1,0,,,\n2,2,104,7,y\n";
        let ticks = parse_painter_ticks_str(csv).unwrap();
        assert_eq!(ticks, vec![100, 104], "empty tick column skipped");
    }

    #[test]
    fn painter_ticks_malformed_row_errors_loudly() {
        // A paint-log header but a data row with a non-numeric tick must error, not
        // silently drop (a shrunk painter set manufactures false phantom faults).
        let csv = "tick,gen_ts_ns\nnotanumber,123\n";
        assert!(parse_painter_ticks_str(csv).is_err());
    }

    #[test]
    fn grab_ts_sidecar_parses_frame_index_to_grab_ts() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n1,1782000033000\n"
        )
        .unwrap();
        let m = parse_grab_ts(f.path()).unwrap();
        assert_eq!(m.get(&0), Some(&1782000000000));
        assert_eq!(m.get(&1), Some(&1782000033000));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn grab_ts_sidecar_malformed_row_errors() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "frame_index,grab_ts_ns\n0\n").unwrap(); // <2 columns
        assert!(parse_grab_ts(f.path()).is_err());
    }

    #[test]
    fn grab_ts_sidecar_tolerates_any_trailing_partial_row() {
        // REGRESSION (#111 deploy + deep review): the cam1 --record-grab BufWriter is killed
        // at teardown mid-write with NO flush, so the file is cut at an arbitrary byte
        // boundary — the surviving trailing fragment (no terminating '\n') can be ANY shape:
        //   "2,"        empty timestamp           -> must skip
        //   "2"         no comma at all           -> must skip (was: <2 columns ABORT)
        //   "2,17820"   timestamp truncated mid-digits, parses as a valid i64
        //               -> must skip (was: silently inserts a WRONG latency sample)
        // A complete row ALWAYS ends in '\n' (writeln! writes the '\n' last). So: a file that
        // does NOT end in '\n' has a partial final line -> skip it, whatever its shape. The
        // earlier good rows still parse. This must never crash the verdict / block the report.
        for partial in ["2,", "2", "2,17820", "garbage"] {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            write!(
                f,
                "frame_index,grab_ts_ns\n0,1782000000000\n1,1782000033000\n{partial}"
            )
            .unwrap();
            let m = parse_grab_ts(f.path())
                .unwrap_or_else(|e| panic!("partial {partial:?} should be tolerated, got {e:?}"));
            assert_eq!(m.get(&0), Some(&1782000000000));
            assert_eq!(m.get(&1), Some(&1782000033000));
            assert_eq!(
                m.len(),
                2,
                "the partial trailing row {partial:?} (no trailing newline) is skipped, not parsed"
            );
        }
    }

    #[test]
    fn grab_ts_sidecar_complete_final_row_with_newline_still_parsed() {
        // A complete final row (ends in '\n') is NOT a partial fragment — it must still parse,
        // and a genuinely malformed COMPLETE final row still errors (it's not a kill artifact).
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n2,1782000066000\n"
        )
        .unwrap();
        let m = parse_grab_ts(f.path()).unwrap();
        assert_eq!(m.get(&2), Some(&1782000066000), "complete final row parses");
        assert_eq!(m.len(), 2);
        // A complete (newline-terminated) malformed final row is corruption, not a kill cut.
        let mut f2 = tempfile::NamedTempFile::new().unwrap();
        write!(f2, "frame_index,grab_ts_ns\n0,1782000000000\n2,\n").unwrap();
        assert!(
            parse_grab_ts(f2.path()).is_err(),
            "an empty-ts row terminated by a newline is complete-and-corrupt -> error"
        );
    }

    #[test]
    fn grab_ts_sidecar_empty_ts_midfile_still_errors() {
        // A row with an empty timestamp that is NOT the last line is genuine corruption
        // (a silently-shrunk grab-ts map would drop real latency samples) — still errors.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "frame_index,grab_ts_ns\n0,1782000000000\n1,\n2,1782000066000\n"
        )
        .unwrap();
        assert!(parse_grab_ts(f.path()).is_err());
    }
}
