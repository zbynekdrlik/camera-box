//! recording-probe: offline recorded-file → rqrr QR analysis (#106).
//!
//! Reads a recorded OBS program-output file (.mkv/.mp4), decodes every frame's
//! QR(s) via the repo's rqrr decoder (NO NDI tap, NO lz4 spool — the loss verdict
//! must come only from the recorded file per #105 acceptance #1), and emits a
//! per-frame CSV for the downstream #107 verdict.
//!
//! CSV columns: `frame_index,n_qr,tick,run_id,frame_ids`
//!   - `n_qr`     decoded QR count in the frame (2 = both dual-QR halves sharp)
//!   - `tick`     effective Vernier tick = max decoded frame_id (empty if none)
//!   - `run_id`   the decoded run_id (first payload; empty if none)
//!   - `frame_ids` `|`-joined decoded frame_ids (empty if none)

use anyhow::Result;
use camera_box::probe::recording::analyze_recording;
use clap::Parser;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about = "Offline recorded-file QR analysis (rqrr, no NDI tap / no spool)")]
struct Args {
    /// Recorded OBS program-output file to analyze (.mkv / .mp4).
    #[arg(long)]
    file: PathBuf,
    /// Per-frame CSV output path. `-` (default) writes to stdout.
    #[arg(long, default_value = "-")]
    out: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    tracing::info!(file = %args.file.display(), out = %args.out, "recording-probe start");

    let frames = analyze_recording(&args.file)?;

    let mut sink: Box<dyn Write> = if args.out == "-" {
        Box::new(BufWriter::new(std::io::stdout().lock()))
    } else {
        Box::new(BufWriter::new(std::fs::File::create(&args.out)?))
    };

    writeln!(sink, "frame_index,n_qr,tick,run_id,frame_ids")?;
    let mut decoded_frames = 0u64;
    for f in &frames {
        if !f.payloads.is_empty() {
            decoded_frames += 1;
        }
        let tick = f.tick.map(|t| t.to_string()).unwrap_or_default();
        let run_id = f
            .payloads
            .first()
            .map(|p| p.run_id.to_string())
            .unwrap_or_default();
        let frame_ids = f
            .payloads
            .iter()
            .map(|p| p.frame_id.to_string())
            .collect::<Vec<_>>()
            .join("|");
        writeln!(
            sink,
            "{},{},{},{},{}",
            f.frame_index,
            f.payloads.len(),
            tick,
            run_id,
            frame_ids
        )?;
    }
    sink.flush()?;

    tracing::info!(
        total = frames.len(),
        with_qr = decoded_frames,
        "recording-probe done"
    );
    // Diagnostic summary to stderr so a `--out -` CSV on stdout stays clean.
    eprintln!(
        "ANALYZED file={} frames={} with_qr={}",
        args.file.display(),
        frames.len(),
        decoded_frames
    );
    Ok(())
}
