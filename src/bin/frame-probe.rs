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
    /// Painter rate (defaults: coverage 12, full-rate = capture rate)
    #[arg(long)]
    paint_fps: Option<f64>,
    /// Expected capture rate (1080p60 pipeline default)
    #[arg(long, default_value_t = 60.0)]
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
    /// Hard latency gate (ms): fail the run if p99 latency exceeds this.
    /// Omitted ⇒ latency is report-only. Set from a rig baseline plus margin.
    #[arg(long)]
    max_p99_latency_ms: Option<f64>,
    /// Hard freeze gate: fail the run if any stall repeats more than this many
    /// consecutive frames. Omitted ⇒ freezes are report-only.
    #[arg(long)]
    max_freeze_periods: Option<f64>,
    /// JSON artifact output path
    #[arg(long, default_value = "/tmp/frame-probe.json")]
    out: String,
    /// Shared run id (default: derived from the clock). Set it so taps on other
    /// machines can filter to this painter's frames.
    #[arg(long)]
    run_id: Option<u32>,
    /// Only paint the framebuffer; do not receive/analyze NDI. Used on the
    /// camera box in Phase 2 (taps run on dev1).
    #[arg(long, default_value_t = false)]
    paint_only: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    if args.settle_ms >= args.duration_secs.saturating_mul(1000) {
        anyhow::bail!(
            "--settle-ms ({}) must be less than the run duration ({} s) — otherwise no frames are tested",
            args.settle_ms,
            args.duration_secs
        );
    }
    let mode = match args.mode.as_str() {
        "coverage" => PaintMode::Coverage,
        "full-rate" | "fullrate" => PaintMode::FullRate,
        other => anyhow::bail!("unknown mode '{}' (use coverage|full-rate)", other),
    };
    // Coverage default 12 fps: each id is displayed ~83 ms (~5 capture periods
    // at the 1080p60 pipeline rate, >= 2.5 even at 30 fps), guaranteeing >= 2
    // capture samples per id. The framebuffer is written once per id (~0.8 ms),
    // and captures are ~16.7 ms apart at 60 fps, so at most one sample per id
    // can be torn -> >= 1 clean sample always exists -> tearing cannot cause a
    // false loss. (Full-rate runs at the capture rate to stress the real path;
    // it is report-only for loss.)
    let paint_fps = args.paint_fps.unwrap_or(match mode {
        PaintMode::Coverage => 12.0,
        PaintMode::FullRate => args.capture_fps,
    });
    let run_id = match args.run_id {
        Some(r) => r,
        None => (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32,
    };

    tracing::info!(
        "frame-probe start: mode={:?} run_id={} source={:?} paint_fps={} dur={}s",
        mode,
        run_id,
        args.source,
        paint_fps,
        args.duration_secs
    );

    let cfg = RunConfig {
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
        max_p99_latency_ms: args.max_p99_latency_ms,
        max_freeze_periods_gate: args.max_freeze_periods,
    };

    if args.paint_only {
        let painted = camera_box::probe::run::run_paint_only(&cfg)?;
        println!("PAINT_ONLY run_id={} painted={}", run_id, painted);
        return Ok(());
    }

    let report = run(cfg)?;

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
