//! forensic-dump: per-frame FULL payload dump for #175 per-loss forensics.
//!
//! `recording-probe` emits only the FIRST payload's run_id per frame, which is
//! useless for forensics — to classify a per-hop loss (real drop vs burn-decode
//! miss vs measurement artifact) the analyst needs EVERY decoded QR payload in
//! each frame (run_id + frame_id + gen_ts_ns), not just the first. This bin dumps
//! that, one JSON object per frame, so the #175 forensic classifier can:
//!   - map a loss cam2-tick to the stream frame(s) carrying it,
//!   - see exactly which burns (cam1 / strih / stream) were present/readable there,
//!   - and target the ffmpeg PNG extraction at the right frame_index.
//!
//! NOT part of the verdict path — a forensic/analysis tool only.

use anyhow::Result;
use camera_box::probe::recording::analyze_recording;
use clap::Parser;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about = "Per-frame FULL QR payload dump (JSONL) for #175 forensics")]
struct Args {
    /// Recorded file to analyze (.mkv / .mp4).
    #[arg(long)]
    file: PathBuf,
    /// JSONL output path (one frame per line).
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    tracing::info!(file = %args.file.display(), out = %args.out.display(), "forensic-dump start");

    let frames = analyze_recording(&args.file)?;
    let mut sink = BufWriter::new(std::fs::File::create(&args.out)?);

    for f in &frames {
        // Compact JSON: {"i":<frame_index>,"tick":<max_frame_id|null>,"p":[[run_id,frame_id,gen_ts_ns],...]}
        let payloads: Vec<String> = f
            .payloads
            .iter()
            .map(|p| format!("[{},{},{}]", p.run_id, p.frame_id, p.gen_ts_ns))
            .collect();
        let tick = f
            .tick
            .map(|t| t.to_string())
            .unwrap_or_else(|| "null".to_string());
        writeln!(
            sink,
            "{{\"i\":{},\"tick\":{},\"p\":[{}]}}",
            f.frame_index,
            tick,
            payloads.join(",")
        )?;
    }
    sink.flush()?;
    eprintln!(
        "forensic-dump: {} frames written to {}",
        frames.len(),
        args.out.display()
    );
    Ok(())
}
