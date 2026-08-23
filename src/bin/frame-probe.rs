//! frame-probe: cam2 HDMI-loopback frame-loss/latency probe (Phase 1).

use anyhow::Result;
use camera_box::probe::analyzer::PaintMode;
use camera_box::probe::run::{run, RunConfig};
use clap::Parser;
use std::path::PathBuf;
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
    /// Framebuffer device (HDMI out, fbdev presenter fallback)
    #[arg(long, default_value = "/dev/fb0")]
    fb_device: String,
    /// DRM card device for the KMS page-flip presenter (#79). The tear-free 1:1
    /// vblank-locked path; falls back to the fbdev framebuffer when KMS can't
    /// take DRM master (see --presenter).
    #[arg(long, default_value = "/dev/dri/card1")]
    drm_device: String,
    /// Presenter: auto (KMS page-flip, fall back to fbdev) | kms (force DRM,
    /// tear-free 1:1) | fbdev (force the #68 single-buffer vsync-gated write).
    /// KMS paces the painter on the HDMI vblank (one id per flip, 60fps 1:1) so
    /// --paint-fps is ignored under kms/auto-on-kms.
    #[arg(long, default_value = "auto")]
    presenter: String,
    /// Run duration in seconds
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    /// Painter rate. Default (when omitted) = `default_paint_fps`: the capture rate on the
    /// real-presenter / paint-only paths (#290), 12 fps for the fbdev single-box loopback coverage
    /// gate and the presenter-less synth-ndi reference, capture rate for full-rate mode.
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
    /// With --paint-only: write the painter's emitted-tick CSV (`tick,gen_ts_ns`)
    /// to this path — the cam→strih GROUND TRUTH (#105) consumed by
    /// `recording-verdict --painter`. scp it back to dev1 after the run. Omitted
    /// ⇒ no log written (the painted-tick set is then unavailable to the verdict).
    #[arg(long)]
    paint_log: Option<String>,
    /// Stamp each frame's `gen_ts_ns` on CLOCK_REALTIME (the DanteSync-disciplined
    /// wall clock) instead of this process's monotonic clock. Set this for the #7
    /// multi-node ABSOLUTE end-to-end latency path (paint-only on the camera; the
    /// recorded-file verdict (recording-verdict) reads each node's burn gen_ts on
    /// the same wall clock, so the per-hop latency is a true absolute latency).
    /// Leave OFF for the Phase-1 single-box loopback,
    /// where painter+reader share one process clock. Requires the cluster to be
    /// clock-synced (verify with scripts/clock-offset-guard.sh).
    #[arg(long, default_value_t = false)]
    wall_clock: bool,
    /// Paint two QR codes side-by-side (Vernier dual-QR path) and decode from both
    /// halves on receive. At least one half is always sharp on a mid-transition
    /// capture, eliminating the false-loss artifact from the single-QR path.
    /// Painter and reader both switch; the recorded-file verdict decodes both
    /// halves the same way.
    #[arg(long, default_value_t = false)]
    dual_qr: bool,
    /// Paint the fixed colour-reference scale (#367) along the bottom of the canvas,
    /// alongside the dual-QR, so colours are checkable BY EYE on the monitor AND
    /// sampled per-patch from the recording (the #364 per-camera colour gate).
    /// Default: ON in --paint-only mode (the permanent cam2 painter shows it), OFF
    /// otherwise. Force with --colour-scale / --colour-scale=true, disable with
    /// --colour-scale=false.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    colour_scale: Option<bool>,
    /// Paint the #751 constant-velocity motion sweep (a bright ball sweeping the bottom band) so
    /// judder is visible BY EYE on the monitor, multiview, and recording. It stays fully outside
    /// the dual-QR and colour-scale zones (a fused run still decodes clean). Defaults ON in
    /// `--paint-only` mode (the permanent cam2 painter shows it) and OFF otherwise; override with
    /// `--motion-sweep` or `--motion-sweep=false`.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    motion_sweep: Option<bool>,
    /// Paint QR frames DIRECTLY into an NDI sender with this name (no
    /// framebuffer, no capture hardware) at an exact --paint-fps. The
    /// software-only source for genlock validation (#42) and the OBS-bypass
    /// golden-reference runs: zero hardware in the loop.
    #[arg(long)]
    synth_ndi: Option<String>,
    /// Canvas width (the painter/synth-ndi canvas; overridable as a whole via --display-mode)
    #[arg(long, default_value_t = 1920)]
    canvas_w: u32,
    /// Canvas height (see --canvas-w)
    #[arg(long, default_value_t = 1080)]
    canvas_h: u32,
    /// #1179: explicit HDMI display-mode override "WxH@RR" (e.g. "2560x1080@100" — the
    /// LG 34U511A-B native max, issue 881's experiment without a 120 Hz monitor swap). Overrides
    /// the canvas to WxH, scales the dual-QR proportionally from the 1920/700 baseline, and hands
    /// RR (Hz, fractional OK) to pick_mode as the mode-SELECTION target. Omitted ⇒ byte-identical
    /// to today (canvas from --canvas-w/-h, 60.000 Hz selection). A non-60 Hz mode is honestly
    /// reported NOT 1:1 phase-locked (100 is not an integer multiple of the 60 fps capture).
    #[arg(long)]
    display_mode: Option<String>,
    /// #188/#984: emit the QR-based (QPSK, norihiro-compatible) A/V-sync audio marker on the cam2
    /// HDMI audio output at the marker cadence. The emitted (index, frame_id, emit_ts_ns) rows are
    /// written to --marker-log so recording-verdict can pair a decoded audio index → its dual-QR
    /// frame → the A/V offset. #984: default ON under --paint-only (hard-locked the way genlock
    /// was in issue 257 — the SAME shape --colour-scale/--motion-sweep already use), OFF
    /// otherwise. Force with --audio-marker / --audio-marker=true, disable with
    /// --audio-marker=false.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    audio_marker: Option<bool>,
    /// ALSA device string for the QPSK A/V-sync marker. #984: when omitted AND the marker is
    /// enabled, resolved LIVE from `aplay -l` (the first HDMI playback device that carries a
    /// genuine, non-generic monitor name — mirrors scripts/lib/marker-device-resolve.sh exactly),
    /// falling back to the pinned cam2 default (card0 USB is the intercom, held exclusively by
    /// camera-box) only when nothing resolves. An explicit value always wins verbatim (never
    /// re-resolved).
    #[arg(long)]
    audio_marker_device: Option<String>,
    /// Emit the QPSK marker every N painter refresh ticks (~5 s @ 60 Hz with the default 300).
    #[arg(long, default_value_t = 300)]
    audio_marker_cadence_ticks: u64,
    /// With --audio-marker: write the emitted-marker CSV (`index,frame_id,emit_ts_ns`) to this
    /// path. scp it back to dev1 so recording-verdict can pair audio index → frame → A/V offset.
    /// Omitted ⇒ no log written.
    #[arg(long)]
    marker_log: Option<PathBuf>,
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
    // --wall-clock only affects the painted gen_ts (paint-only / synth-ndi). The
    // Phase-1 loopback `run()` is forced monotonic (painter+reader share one
    // process clock), so --wall-clock there is silently inert — bail rather than
    // let a user believe they enabled wall-clock stamping when they did not.
    if args.wall_clock && !args.paint_only && args.synth_ndi.is_none() {
        anyhow::bail!(
            "--wall-clock only applies with --paint-only or --synth-ndi (the multi-node #7 \
             absolute-latency path); the single-box loopback run is always monotonic"
        );
    }
    // --paint-log records the painter's emitted ticks; only the paint-only path
    // (run_paint_only) writes it. Fail loudly so the user isn't misled into
    // believing the ground-truth CSV was produced on a non-paint-only run.
    if args.paint_log.is_some() && !args.paint_only {
        anyhow::bail!(
            "--paint-log only applies with --paint-only (it records the painter's emitted ticks)"
        );
    }
    // #984: audio_marker is now Option<bool> (None ⇒ resolved by default policy below), so an
    // EXPLICIT --audio-marker=false must never trip this bail — only an explicit true (or
    // --marker-log) without --paint-only is a real misuse.
    if (args.audio_marker == Some(true) || args.marker_log.is_some()) && !args.paint_only {
        anyhow::bail!(
            "--audio-marker / --marker-log only apply with --paint-only (the rig A/V-sync path)"
        );
    }
    let presenter = camera_box::probe::presenter::PresenterKind::parse(&args.presenter)?;
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
    //
    // The default painter rate (when --paint-fps is omitted) is computed by the
    // single source of truth `default_paint_fps` (testable; see #290). A path that
    // drives a real HDMI presenter defaults to the full capture rate; the fbdev
    // single-box loopback gate keeps the sub-capture coverage default. An explicit
    // --paint-fps always wins.
    let paint_fps = args.paint_fps.unwrap_or_else(|| {
        camera_box::probe::run::default_paint_fps(
            mode,
            args.capture_fps,
            presenter,
            args.paint_only,
            args.synth_ndi.is_some(),
        )
    });
    let run_id = match args.run_id {
        Some(r) => r,
        None => (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32,
    };
    // #367: the colour scale defaults ON in --paint-only mode (so the permanent cam2
    // painter shows it without a flag change) and OFF otherwise; an explicit
    // --colour-scale[=bool] always wins.
    let colour_scale = args.colour_scale.unwrap_or(args.paint_only);
    // #751: the motion sweep defaults ON in --paint-only mode (the permanent cam2 painter shows
    // it without a flag change) and OFF otherwise; an explicit --motion-sweep[=bool] always wins.
    let motion_sweep = args.motion_sweep.unwrap_or(args.paint_only);
    // #984: the QPSK audio marker defaults ON in --paint-only mode (so the permanent cam2 painter
    // is audible without a flag change -- the bug this issue fixes) and OFF otherwise; an
    // explicit --audio-marker[=bool] always wins. audio_marker_soft is true iff the marker was
    // NOT explicitly requested -- a soft-enabled marker degrades (logs + keeps painting) instead
    // of aborting the run when its PCM device is unavailable; an explicit request keeps the
    // original hard-fail contract (issue 936).
    let audio_marker_explicit = args.audio_marker.is_some();
    let audio_marker = args.audio_marker.unwrap_or_else(|| {
        camera_box::audio_marker_policy::audio_marker_default_enabled(args.paint_only)
    });
    let audio_marker_soft = !audio_marker_explicit;
    // #984/#725: an explicit --audio-marker-device always wins verbatim; otherwise, when the
    // marker is actually enabled, resolve it LIVE from `aplay -l` (never a hardcoded pin -- any
    // HDMI renegotiation can move which DEV=N the physical monitor lands on).
    let audio_marker_device = if let Some(explicit) = args.audio_marker_device.clone() {
        explicit
    } else if audio_marker {
        resolve_audio_marker_device()
    } else {
        // Never actually used (the marker is disabled) -- keep a harmless placeholder so
        // RunConfig's field stays a plain String.
        camera_box::audio_marker_policy::FALLBACK_MARKER_DEVICE.to_string()
    };

    tracing::info!(
        "frame-probe start: mode={:?} run_id={} source={:?} paint_fps={} dur={}s",
        mode,
        run_id,
        args.source,
        paint_fps,
        args.duration_secs
    );

    // #1179: resolve the optional --display-mode override into the canvas params + the
    // mode-SELECTION refresh. With NO override this is byte-identical to today: the CLI canvas/qr
    // defaults and the 60.000 Hz capture rate as the selection target. With an override the canvas
    // follows WxH, the QR scales proportionally from the 1920 baseline, and RR feeds pick_mode.
    let display_override = match args.display_mode.as_deref() {
        Some(s) => {
            Some(camera_box::painter_mode::parse_display_mode(s).map_err(anyhow::Error::msg)?)
        }
        None => None,
    };
    let resolved_canvas = camera_box::painter_mode::resolve_canvas(
        display_override,
        args.canvas_w,
        args.canvas_h,
        args.qr_size,
        camera_box::probe::kms::TARGET_REFRESH_MHZ,
    );

    let cfg = RunConfig {
        mode,
        run_id,
        source: args.source,
        fb_device: args.fb_device,
        drm_device: args.drm_device,
        presenter,
        duration: Duration::from_secs(args.duration_secs),
        paint_fps,
        capture_fps: args.capture_fps,
        canvas_w: resolved_canvas.canvas_w,
        canvas_h: resolved_canvas.canvas_h,
        qr_size: resolved_canvas.qr_size,
        mode_refresh_mhz: resolved_canvas.mode_refresh_mhz,
        freeze_periods: args.freeze_periods,
        connect_timeout_secs: args.connect_timeout_secs,
        settle_ms: args.settle_ms,
        max_p99_latency_ms: args.max_p99_latency_ms,
        max_freeze_periods_gate: args.max_freeze_periods,
        wall_clock: args.wall_clock,
        dual_qr: args.dual_qr,
        colour_scale,
        motion_sweep,
        paint_log: args.paint_log.clone(),
        audio_marker,
        audio_marker_device,
        audio_marker_cadence_ticks: args.audio_marker_cadence_ticks,
        audio_marker_soft,
        marker_log: args.marker_log.clone(),
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

/// #984/#725: resolve the QPSK audio-marker's ALSA device LIVE from this box's own `aplay -l`
/// (never a hardcoded pin — any HDMI renegotiation can move which `DEV=N` the physical monitor
/// lands on). Pure text-parsing decision lives in `camera_box::audio_marker_policy` (Tier-0
/// unit-tested); this function is the thin I/O glue (exec `aplay`, log the outcome) that has no
/// local verification path (frame-probe.rs is `required-features = ["probe"]`). Falls back to
/// the pinned [`camera_box::audio_marker_policy::FALLBACK_MARKER_DEVICE`] — loudly — when `aplay`
/// itself fails to run, or no HDMI device in its output carries a genuine monitor name. Mirrors
/// `scripts/lib/marker-device-resolve.sh`'s `resolve_marker_device` (the bash TEST-mode
/// equivalent) so both paths make the identical decision.
fn resolve_audio_marker_device() -> String {
    let aplay_text = match std::process::Command::new("aplay").arg("-l").output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "#984: failed to run `aplay -l` for live audio-marker device resolution -- \
                 falling back to the pinned default"
            );
            String::new()
        }
    };
    match camera_box::audio_marker_policy::resolve_marker_device_from_aplay(&aplay_text) {
        Some(device) => {
            tracing::info!(device = %device, "#984/#725: resolved audio-marker device from live `aplay -l`");
            device
        }
        None => {
            tracing::warn!(
                fallback = camera_box::audio_marker_policy::FALLBACK_MARKER_DEVICE,
                "#984/#725: no HDMI device in live `aplay -l` carries a genuine monitor name -- \
                 falling back to the pinned default (last resort)"
            );
            camera_box::audio_marker_policy::FALLBACK_MARKER_DEVICE.to_string()
        }
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
            // Wall-clock gen_ts (#7) when requested so a dev1 endpoint tap's
            // wall-clock recv − this gen is true absolute latency; else monotonic.
            gen_ts_ns: camera_box::probe::clock_ns(start, cfg.wall_clock),
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
