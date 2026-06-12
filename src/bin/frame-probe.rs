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
    /// Paint QR frames DIRECTLY into an NDI sender with this name (no
    /// framebuffer, no capture hardware) at an exact --paint-fps. The
    /// software-only source for genlock validation (#42) and the OBS-bypass
    /// golden-reference runs: zero hardware in the loop.
    #[arg(long)]
    synth_ndi: Option<String>,
    /// Canvas size for --synth-ndi (the fb path is fixed 1920x1080)
    #[arg(long, default_value_t = 1920)]
    canvas_w: u32,
    #[arg(long, default_value_t = 1080)]
    canvas_h: u32,
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
        canvas_w: args.canvas_w,
        canvas_h: args.canvas_h,
        qr_size: args.qr_size,
        freeze_periods: args.freeze_periods,
        connect_timeout_secs: args.connect_timeout_secs,
        settle_ms: args.settle_ms,
        max_p99_latency_ms: args.max_p99_latency_ms,
        max_freeze_periods_gate: args.max_freeze_periods,
    };

    if let Some(name) = args.synth_ndi.as_deref() {
        let sent = synth_ndi_paint(name, &cfg)?;
        println!("SYNTH_NDI run_id={} sent={}", run_id, sent);
        return Ok(());
    }

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
    // #20 oversample discriminator: tells a real drop (confirmed) from a
    // torn/illegible-QR artifact (inconclusive, report-only) and flags
    // torn-prone 1-sample ids.
    let c = &report.coverage;
    println!(
        "COVERAGE oversample_p50={} (>={}? {}) confirmed_drops={} inconclusive_gaps={} low_coverage={}",
        c.oversample_p50,
        c.min_confirm_samples,
        c.run_oversampled,
        c.confirmed_drops.len(),
        c.inconclusive_gaps.len(),
        c.low_coverage_ids.len(),
    );
    if !c.inconclusive_gaps.is_empty() {
        // PASS can still hide isolated torn-QR gaps we could not confirm as real
        // loss — surface them so a degraded-capture run is not read as clean.
        println!(
            "WARN {} inconclusive (torn-prone) single-frame gap(s) — capture degraded, not a confirmed drop: {:?}",
            c.inconclusive_gaps.len(),
            &c.inconclusive_gaps[..c.inconclusive_gaps.len().min(20)],
        );
    }
    println!("ARTIFACT={}", args.out);

    if report.verdict_pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Paint QR ids straight into an NDI sender at an exact rate — no framebuffer,
/// no capture device, no HDMI loop. This is the all-software source for the
/// genlock validation rig (#42): every emitted frame is a unique id at exactly
/// --paint-fps, so any drop downstream is the pipeline's fault, never the
/// source's. UYVY passthrough (gray QR -> Y plane, neutral chroma).
fn synth_ndi_paint(name: &str, cfg: &camera_box::probe::run::RunConfig) -> Result<u64> {
    use camera_box::capture::FrameRate;
    use camera_box::ndi::NdiSender;
    use camera_box::probe::payload::Payload;
    use camera_box::probe::qr::render_qr_bgra;
    use std::time::Instant;

    anyhow::ensure!(
        cfg.qr_size <= cfg.canvas_h && cfg.qr_size <= cfg.canvas_w,
        "--qr-size {} does not fit the {}x{} canvas",
        cfg.qr_size,
        cfg.canvas_w,
        cfg.canvas_h
    );
    let fps = cfg.paint_fps;
    let mut sender = NdiSender::new(
        name,
        FrameRate {
            numerator: fps.round() as u32,
            denominator: 1,
        },
    )?;
    let (w, h) = (cfg.canvas_w, cfg.canvas_h);
    let mut uyvy = vec![0u8; (w * h * 2) as usize];
    let start = Instant::now();
    let mut frame_id: u32 = 0;

    // Pace on ABSOLUTE WALL-CLOCK frame boundaries, not the monotonic clock.
    // The genlocked OBS render tick consumes on the DanteSync-disciplined wall
    // clock; a monotonic-paced source drifts against it by the PTP frequency
    // correction (~10-20 ppm ≈ 1 frame per ~1-2 min), ratcheting the receiver
    // queue to its cap and forcing periodic flush bursts. Wall-boundary pacing
    // makes source and consumer tick at the same disciplined rate and phase
    // (the same scheme as camera-box's sender and the libobs genlock patch).
    let interval_ns: u64 = (1_000_000_000f64 / fps).round() as u64;
    let wall_ns = || -> u64 {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock before epoch");
        d.as_nanos() as u64
    };

    tracing::info!(
        "synth-ndi: sending '{}' {}x{} @{} fps (wall-boundary paced)",
        name,
        w,
        h,
        fps
    );
    while start.elapsed() < cfg.duration {
        let payload = Payload {
            run_id: cfg.run_id,
            frame_id,
            gen_ts_ns: start.elapsed().as_nanos() as i64,
        };
        let bgra = render_qr_bgra(&payload, w, h, cfg.qr_size);
        bgra_gray_to_uyvy(&bgra, &mut uyvy);
        sender.send_frame_data(&uyvy, w, h, v4l::FourCC::new(b"UYVY"), w * 2)?;
        frame_id = frame_id.wrapping_add(1);
        // sleep to the next absolute wall boundary
        let now = wall_ns();
        let next_wall = now - (now % interval_ns) + interval_ns;
        std::thread::sleep(Duration::from_nanos(next_wall - now));
    }
    tracing::info!("synth-ndi: sent {} frames", frame_id);
    Ok(frame_id as u64)
}

/// Gray BGRA -> UYVY: Y from the blue channel (R=G=B on the QR canvas),
/// neutral chroma (U=V=128).
fn bgra_gray_to_uyvy(bgra: &[u8], out: &mut [u8]) {
    let pairs = bgra.len() / 8;
    for i in 0..pairs {
        let y0 = bgra[i * 8];
        let y1 = bgra[i * 8 + 4];
        out[i * 4] = 128;
        out[i * 4 + 1] = y0;
        out[i * 4 + 2] = 128;
        out[i * 4 + 3] = y1;
    }
}

#[cfg(test)]
mod synth_tests {
    use super::bgra_gray_to_uyvy;

    #[test]
    fn gray_bgra_maps_luma_and_neutral_chroma() {
        // two pixels: black (0) and white (255), gray => B=G=R
        let bgra = [0u8, 0, 0, 255, 255u8, 255, 255, 255];
        let mut out = [0u8; 4];
        bgra_gray_to_uyvy(&bgra, &mut out);
        assert_eq!(out, [128, 0, 128, 255]); // U=128, Y0=0, V=128, Y1=255
    }
}
