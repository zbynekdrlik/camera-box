//! frame-probe: cam2 HDMI-loopback frame-loss/latency probe (Phase 1).

use anyhow::Result;
use camera_box::probe::analyzer::PaintMode;
use camera_box::probe::run::{run, RunConfig};
use clap::Parser;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(about = "QR HDMI-loopback frame-loss/latency probe")]
struct Args {
    /// coverage = clean zero-loss gate; full-rate = realistic stress
    #[arg(long, default_value = "coverage")]
    mode: String,
    /// NDI source substring to receive (e.g. "usb (CAM2)")
    #[arg(long, default_value = "usb (CAM2)")]
    source: String,
    /// Framebuffer device (HDMI out)
    #[arg(long, default_value = "/dev/fb0")]
    fb_device: String,
    /// Run duration in seconds
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    /// Painter rate (defaults: coverage 24, full-rate 30)
    #[arg(long)]
    paint_fps: Option<f64>,
    /// Expected capture rate
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// QR pixel size on the canvas
    #[arg(long, default_value_t = 700)]
    qr_size: u32,
    /// Freeze threshold: flag a stall when the same id is captured for more than
    /// this many consecutive frames. Above the normal dup run (capture/paint
    /// ratio, ~2.5 at coverage 12 fps) so steady dups are not false freezes.
    #[arg(long, default_value_t = 6.0)]
    freeze_periods: f64,
    /// NDI connect timeout (seconds)
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u32,
    /// Trailing settle window (ms): frames painted this close to the end are
    /// excluded from the loss check (pipeline latency). Must exceed max latency.
    #[arg(long, default_value_t = 500)]
    settle_ms: u64,
    /// JSON artifact output path
    #[arg(long, default_value = "/tmp/frame-probe.json")]
    out: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let mode = match args.mode.as_str() {
        "coverage" => PaintMode::Coverage,
        "full-rate" | "fullrate" => PaintMode::FullRate,
        other => anyhow::bail!("unknown mode '{}' (use coverage|full-rate)", other),
    };
    // Coverage default 12 fps: each id is displayed ~83 ms (>= 2.5 capture
    // periods at 30 fps), guaranteeing >= 2 capture samples per id. The
    // framebuffer is written once per id (~0.8 ms), and captures are ~33 ms
    // apart, so at most one sample per id can be torn -> >= 1 clean sample
    // always exists -> tearing cannot cause a false loss. (Full-rate runs at
    // capture rate to stress the real path; it is report-only for loss.)
    let paint_fps = args.paint_fps.unwrap_or(match mode {
        PaintMode::Coverage => 12.0,
        PaintMode::FullRate => 30.0,
    });
    let run_id = (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32;

    tracing::info!(
        "frame-probe start: mode={:?} run_id={} source={:?} paint_fps={} dur={}s",
        mode,
        run_id,
        args.source,
        paint_fps,
        args.duration_secs
    );

    let report = run(RunConfig {
        mode,
        run_id,
        source: args.source,
        fb_device: args.fb_device,
        duration: Duration::from_secs(args.duration_secs),
        paint_fps,
        capture_fps: args.capture_fps,
        canvas_w: 1920,
        canvas_h: 1080,
        qr_size: args.qr_size,
        freeze_periods: args.freeze_periods,
        connect_timeout_secs: args.connect_timeout_secs,
        settle_ms: args.settle_ms,
    })?;

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&args.out, &json)?;

    println!(
        "VERDICT={} emitted={} observed={} unique={} missing={} reorders={} freezes={}",
        if report.verdict_pass { "PASS" } else { "FAIL" },
        report.emitted_count,
        report.observed_count,
        report.unique_observed,
        report.missing_ids.len(),
        report.reorders.len(),
        report.freezes.len(),
    );
    if let Some(l) = &report.latency {
        println!(
            "LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={})",
            l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
        );
    }
    println!("ARTIFACT={}", args.out);

    if report.verdict_pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
