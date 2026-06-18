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
use camera_box::probe::recording::{analyze_recording, extract_frames_png, RecordingFrame};
use camera_box::probe::recording_latency::{
    cam_strih_samples, hop_latency, strih_stream_samples, HopLatency, BURN_RUN_ID_STREAM,
    BURN_RUN_ID_STRIH,
};
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, strih_stream_verdict, verdict, FrameTick, RecordingVerdict, VerdictConfig,
};
use clap::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(about = "Hard-fail zero-loss verdict from recorded OBS program files (#107)")]
struct Args {
    /// strih OBS-program recording (.mkv / .mp4) — the strict hop-1 endpoint.
    #[arg(long)]
    strih: PathBuf,
    /// stream OBS-program recording — the headline endpoint. Enables strih→stream.
    #[arg(long)]
    stream: Option<PathBuf>,
    /// CSV of the cam2 painter's displayed ticks (enables the cam→strih assessment).
    #[arg(long)]
    painter: Option<PathBuf>,
    /// Directory for pixel-proof PNGs of flagged frames.
    #[arg(long, default_value = "recording-verdict-run")]
    out_dir: PathBuf,
    /// Minimum analyzed span (s) before a zero-loss PASS may be declared.
    #[arg(long, default_value_t = 300.0)]
    min_secs: f64,
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
}

/// Parse the painter ticks from either a bare one-`tick`-per-line file or the
/// recording-probe `frame_index,n_qr,tick,run_id,frame_ids` CSV (the `tick` column).
fn parse_painter_ticks(path: &Path) -> Result<Vec<u32>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read painter ticks {}", path.display()))?;
    let mut ticks = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("frame_index") {
            continue; // header / blank
        }
        // recording-probe CSV: the tick is the 3rd column; a bare file has it 1st.
        // A comma-containing row with fewer than 3 columns is a MALFORMED CSV — error
        // loudly rather than silently dropping it (a silently-shrunk painter set
        // would manufacture false phantom faults in cam_strih_assessment).
        let field = if line.contains(',') {
            line.split(',').nth(2).with_context(|| {
                format!(
                    "painter CSV row at line {} has fewer than 3 columns (expected \
                     frame_index,n_qr,tick,...): {line:?}",
                    lineno + 1
                )
            })?
        } else {
            line
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
    tracing::info!(file = %path.display(), ticks = ticks.len(), "painter ticks parsed");
    Ok(ticks)
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
) -> Result<bool> {
    println!("=== {label} verdict ({}) ===", path.display());
    println!(
        "  frames={} analyzed={:.1}s duration_ok={} avg_step={:.4} beat_balanced={}",
        v.total_frames, v.analyzed_secs, v.duration_ok, v.avg_step, v.beat_balanced
    );
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
        let extracted = extract_frames_png(path, &flagged, &undecodable, &png_dir)?;
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
        strih = %args.strih.display(),
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

    // strih recording verdict (strict hop-1 endpoint). Keep the decoded frames so the
    // #108 per-hop latency engine can read each frame's cam2 + node-burn gen_ts_ns.
    let (strih_frames, strih_ticks) = ticks_of(&args.strih)?;
    let strih_v = verdict(&strih_ticks, &cfg);
    all_pass &= report_recording("strih", &args.strih, &strih_v, &args.out_dir)?;

    // stream recording verdict (headline endpoint) + strih→stream direct compare.
    let mut stream_frames_opt: Option<Vec<RecordingFrame>> = None;
    if let Some(stream_path) = &args.stream {
        let (stream_frames, stream_ticks) = ticks_of(stream_path)?;
        let stream_v = verdict(&stream_ticks, &cfg);
        all_pass &= report_recording("stream", stream_path, &stream_v, &args.out_dir)?;

        let ss = strih_stream_verdict(&strih_ticks, &stream_ticks, &cfg);
        println!("=== strih→stream hop verdict (direct tick-SEQUENCE compare, offset-immune) ===");
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
        stream_frames_opt = Some(stream_frames);
    }

    // cam→strih honest assessment (no false zero claim).
    let mut cam_strih_clean: Option<bool> = None;
    if let Some(painter_path) = &args.painter {
        let painter_ticks = parse_painter_ticks(painter_path)?;
        let a = cam_strih_assessment(&strih_ticks, &painter_ticks, &cfg);
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
    }

    // #108 — per-hop ABSOLUTE latency from the in-frame node-burn + cam2 gen_ts_ns
    // stamps (the #111 burn must be live on the boxes for these to be non-empty). NO
    // networked record_start, NO idx/30 — every number is a difference of two stamps
    // that already share the DanteSync wall clock. Reported, never gated (a latency
    // gate is a separate decision; #108 asks for the stable, defined numbers).
    println!();
    let cam_strih_lat = hop_latency(
        "cam→strih",
        &cam_strih_samples(&strih_frames, args.burn_strih_run_id),
    );
    report_hop_latency(&cam_strih_lat, "cam→strih", "cam2 paint gen_ts_ns");
    if let Some(stream_frames) = &stream_frames_opt {
        let ss_lat = hop_latency(
            "strih→stream",
            &strih_stream_samples(
                &strih_frames,
                stream_frames,
                args.burn_strih_run_id,
                args.burn_stream_run_id,
            ),
        );
        report_hop_latency(&ss_lat, "strih→stream", "strih render gen_ts_ns");
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
