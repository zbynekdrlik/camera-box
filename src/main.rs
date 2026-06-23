use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal;
use tracing_subscriber::EnvFilter;

use camera_box::capture::VideoCapture;
use camera_box::config::Config;
use camera_box::intercom;
use camera_box::ndi::NdiSender;
use camera_box::ndi_display::{self, NdiDisplayConfig};

/// Wall-clock time in ns (CLOCK_REALTIME — the DanteSync-disciplined clock the
/// cluster genlock aligns to). Used by the genlock decimation gate.
fn wall_clock_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// Apply real-time optimizations to the current thread for lowest latency
/// Based on media-bridge's extreme low-latency settings
fn apply_realtime_optimizations() {
    // 1. Set real-time SCHED_FIFO scheduling with high priority
    apply_realtime_scheduling();

    // 2. Lock all memory to prevent page faults
    apply_memory_locking();

    // 3. Set CPU affinity (optional - pin to core 1)
    apply_cpu_affinity();
}

/// Set SCHED_FIFO real-time scheduling with priority 90
fn apply_realtime_scheduling() {
    unsafe {
        let param = libc::sched_param { sched_priority: 90 };
        let result = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);

        if result == 0 {
            tracing::info!("Real-time SCHED_FIFO priority 90 enabled");
        } else {
            tracing::warn!(
                "Could not set real-time priority (need CAP_SYS_NICE). \
                Run: sudo setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box"
            );
        }
    }
}

/// Lock all memory to prevent page faults during capture
fn apply_memory_locking() {
    unsafe {
        // MCL_CURRENT: Lock all pages currently mapped
        // MCL_FUTURE: Lock all pages that will be mapped in the future
        let result = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);

        if result == 0 {
            tracing::info!("Memory locked (mlockall) - no page faults possible");
        } else {
            tracing::warn!(
                "Could not lock memory (need CAP_IPC_LOCK). \
                Run: sudo setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box"
            );
        }
    }
}

/// Set CPU affinity to pin capture thread to a specific core
fn apply_cpu_affinity() {
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();

        // Pin to CPU core 1 (leave core 0 for system tasks)
        libc::CPU_SET(1, &mut cpuset);

        let result = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);

        if result == 0 {
            tracing::info!("CPU affinity set to core 1");
        } else {
            // Not critical - just a hint to the scheduler
            tracing::debug!("Could not set CPU affinity (non-critical)");
        }
    }
}

/// Simple USB video capture to NDI streaming appliance
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/camera-box/config.toml")]
    config: PathBuf,

    /// Override video device path
    #[arg(short, long)]
    device: Option<String>,

    /// NDI source to display on HDMI (e.g., "STRIH-SNV (interkom)")
    #[arg(long = "display")]
    display_source: Option<String>,

    /// Framebuffer device for display output
    #[arg(long, default_value = "/dev/fb0")]
    fb_device: String,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    /// Enable VBAN intercom (stream name, e.g., "cam1")
    #[arg(long = "intercom")]
    intercom_stream: Option<String>,

    /// VBAN intercom target host (default: strih.lan)
    #[arg(long, default_value = "strih.lan")]
    intercom_target: String,

    /// #105 node 2 — cam1 GRAB-RECORD (TEST mode, OFF by default). Stream the gray8
    /// luma of each EMITTED frame to `tcp://HOST:PORT` (dev1, which encodes ffv1) so
    /// the recording-based 4-node verdict can decode cam1's grab WITHOUT an NDI tap.
    /// Normal operation is unaffected when unset; pulls in no decode deps.
    #[arg(long)]
    record_grab: Option<String>,

    /// #105 node 2 — path for the cam1 grab-timestamp sidecar CSV
    /// (`frame_index,grab_ts_ns`, CLOCK_REALTIME) written alongside --record-grab.
    /// Required when --record-grab is set; scp it back to dev1 after the run.
    #[arg(long, default_value = "/tmp/cam1-grab-ts.csv")]
    record_grab_ts: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter = if args.debug {
        EnvFilter::new("camera_box=debug,grafton_ndi=debug")
    } else {
        EnvFilter::new("camera_box=info")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!("camera-box starting...");

    // Load configuration
    let config = Config::load(&args.config)?;
    tracing::info!("Hostname: {}", config.hostname);

    // Determine device path
    let device_path = if let Some(ref device) = args.device {
        device.clone()
    } else {
        config.device_path()?
    };

    // Determine display source (CLI overrides config)
    let display_config = if let Some(ref source) = args.display_source {
        Some(NdiDisplayConfig {
            source_name: source.clone(),
            fb_device: args.fb_device.clone(),
            find_timeout_secs: 30,
        })
    } else {
        config.display.as_ref().map(|display| NdiDisplayConfig {
            source_name: display.source.clone(),
            fb_device: display.fb_device.clone(),
            find_timeout_secs: 30,
        })
    };

    // Determine intercom config (CLI overrides config)
    let intercom_config = if let Some(ref stream) = args.intercom_stream {
        Some(intercom::IntercomConfig {
            stream_name: stream.clone(),
            target_host: args.intercom_target.clone(),
            sample_rate: 48000,
            channels: 2,
            sidetone_gain: 100.0,
            mic_gain: 12.0,       // +22dB boost for outbound mic
            headphone_gain: 15.0, // Headphone volume from network
            limiter_enabled: true,
            limiter_threshold: 0.5, // -6dB ceiling
        })
    } else {
        config.intercom.as_ref().map(|ic| intercom::IntercomConfig {
            stream_name: ic.stream.clone(),
            target_host: ic.target.clone(),
            sample_rate: ic.sample_rate,
            channels: ic.channels,
            sidetone_gain: ic.sidetone_gain,
            mic_gain: ic.mic_gain,
            headphone_gain: ic.headphone_gain,
            limiter_enabled: ic.limiter_enabled,
            limiter_threshold: ic.limiter_threshold,
        })
    };

    // Run the capture loop with optional display and intercom
    run_capture_loop(
        &device_path,
        &config.ndi_name,
        display_config,
        intercom_config,
        args.record_grab.clone(),
        args.record_grab_ts.clone(),
    )
    .await
}

async fn run_capture_loop(
    device_path: &str,
    ndi_name: &str,
    display_config: Option<NdiDisplayConfig>,
    intercom_config: Option<intercom::IntercomConfig>,
    record_grab: Option<String>,
    record_grab_ts: String,
) -> Result<()> {
    // Shared flag for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));

    // Start display thread if configured (LOW PRIORITY - different core)
    let display_handle = if let Some(config) = display_config {
        let running_clone = Arc::clone(&running);
        tracing::info!("Starting NDI display for source: {}", config.source_name);

        Some(std::thread::spawn(move || {
            // Apply low priority settings BEFORE doing anything
            ndi_display::apply_low_priority();

            if let Err(e) = ndi_display::run_display_loop(config, running_clone) {
                tracing::error!("NDI display error: {}", e);
            }
        }))
    } else {
        None
    };

    // Start intercom thread if configured
    let intercom_handle = if let Some(config) = intercom_config {
        let running_clone = Arc::clone(&running);
        tracing::info!(
            "Starting VBAN intercom: stream={}, target={}",
            config.stream_name,
            config.target_host
        );

        Some(std::thread::spawn(move || {
            if let Err(e) = intercom::run_intercom(config, running_clone) {
                tracing::error!("Intercom error: {}", e);
            }
        }))
    } else {
        None
    };

    // Open capture device at 1920x1080 @ 60fps.
    //
    // #156 durable fix: when grab-recording (or when CAMERA_BOX_CAPTURE_CONTROLS is
    // set) apply the certified V4L2 picture controls (saturation=0, contrast=75) so a
    // camera-box restart can NEVER silently revert the device to its soft defaults and
    // collapse the cam1 grab QR decode. The env overrides the default; --record-grab
    // alone implies the certified set. Production (no grab, no env) leaves controls
    // untouched.
    let capture_controls: Vec<camera_box::capture::CaptureControl> =
        match std::env::var("CAMERA_BOX_CAPTURE_CONTROLS") {
            Ok(spec) => camera_box::capture::parse_capture_controls(&spec),
            Err(_) if record_grab.is_some() => camera_box::capture::certified_cam1_controls(),
            Err(_) => Vec::new(),
        };
    if !capture_controls.is_empty() {
        tracing::info!(
            "applying {} certified capture control(s) for a sharp grab (#156)",
            capture_controls.len()
        );
    }
    let mut capture = VideoCapture::open_with_controls(device_path, &capture_controls)?;
    let (width, height) = capture.dimensions();
    let frame_rate = capture.frame_rate();
    tracing::info!("Capturing at {}x{}", width, height);

    // Genlock #11: optionally emit NDI at a target broadcast rate, decimating the
    // faster capture onto DanteSync wall-clock boundaries, so a downstream
    // genlocked OBS (genlock_fifo) consumes exactly one frame per render tick =
    // zero loss. CAMERA_BOX_GENLOCK_FPS=30 makes a 60fps capture emit 30fps
    // wall-paced (matching a 1080p30 broadcast). Unset = send every captured
    // frame at the capture rate (legacy behavior).
    let genlock_fps: Option<u32> = std::env::var("CAMERA_BOX_GENLOCK_FPS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&f| f > 0);
    let send_rate = match genlock_fps {
        Some(f) => {
            tracing::info!(
                "GENLOCK: emitting {} fps wall-paced (capture {}/{} fps)",
                f,
                frame_rate.numerator,
                frame_rate.denominator
            );
            camera_box::capture::FrameRate {
                numerator: f,
                denominator: 1,
            }
        }
        None => frame_rate,
    };

    // #174 cam1-capture render-time QR burn — TEST MODE ONLY. Gated behind
    // CAMERA_BOX_BURN_RUN_ID (mirrors the strih/stream DistroAV burn run_id env): when
    // UNSET the burn is OFF and the live NDI feed stays completely CLEAN (zero-copy send
    // untouched, no QR on the broadcast). When set, BEFORE NDIlib_send_send_video_v2 a
    // small QR carrying (run_id, per-emit frame_id, cam1-capture wall-clock gen_ts_ns) is
    // drawn into the EMITTED frame's bottom-center luma so the full chain cam1→strih→
    // stream pairs on a clean digital burn-id from the single stream recording. The burn
    // module is `probe`-feature-gated (kept out of the production binary); the e2e harness
    // deploys a probe-featured camera-box to cam1 for the test run.
    #[cfg(feature = "probe")]
    let burn_run_id: Option<u32> = std::env::var("CAMERA_BOX_BURN_RUN_ID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&id| id > 0);
    #[cfg(feature = "probe")]
    if let Some(id) = burn_run_id {
        tracing::info!(
            "#174 CAM1-CAPTURE BURN ACTIVE (TEST MODE): run_id={} — bottom-center QR burned into the emitted frame (production feed is OFF unless CAMERA_BOX_BURN_RUN_ID is set)",
            id
        );
    }

    // Create NDI sender with configured name and the (genlock or capture) rate
    let mut sender = NdiSender::new(ndi_name, send_rate)?;
    if genlock_fps.is_some() {
        sender.set_external_pacing(true);
    }
    tracing::info!("NDI sender ready, streaming as '{}'", ndi_name);
    tracing::info!("ZERO-COPY mode: AVX2 SIMD + sync send for lowest latency");

    // #105 node 2 — cam1 grab recorder (TEST mode). Connect BEFORE the loop so the
    // dev1 ffmpeg listener is bound and the sidecar header is written; a connect
    // failure is fatal (the operator asked to record — don't silently NDI-only).
    let (grab_width, grab_height) = capture.dimensions();
    let grab_stride = capture.frame_info().stride;
    let mut grab_recorder = match &record_grab {
        Some(dest) => {
            let rec = camera_box::grab_record::GrabRecorder::connect(
                dest,
                std::path::Path::new(&record_grab_ts),
                grab_width,
                grab_height,
                grab_stride,
            )?;
            tracing::info!(
                "cam1 GRAB-RECORD active (#105 node 2): {} → {}",
                record_grab_ts,
                dest
            );
            Some(rec)
        }
        None => None,
    };

    // cam2→cam1 LOSS sidecar (TEST mode): when CAMERA_BOX_CAPTURE_STATS is set to a path,
    // the capture loop writes cam1's V4L2 capture-drop count there on shutdown. The
    // recording-verdict reads it as the cam2→cam1 loss (the camera leg — a dropped capture
    // = a lost frame), NOT a painter-tick optical compare. UNSET ⇒ no sidecar (production).
    let capture_stats_path: Option<String> = std::env::var("CAMERA_BOX_CAPTURE_STATS")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(p) = &capture_stats_path {
        tracing::info!(
            "cam2→cam1 LOSS sidecar active (TEST MODE): cam1 V4L2 capture-drop stats → {} on shutdown",
            p
        );
    }

    // Spawn capture loop in blocking task - minimal overhead for lowest latency
    let running_capture = Arc::clone(&running);
    let capture_handle = tokio::task::spawn_blocking(move || {
        // Apply real-time optimizations BEFORE entering the capture loop
        apply_realtime_optimizations();

        let mut frame_count: u64 = 0; // frames captured this report window
        let mut emit_count: u64 = 0; // frames actually sent to NDI this window
        let mut last_report = std::time::Instant::now();
        // #174 cam1 burn: a monotonic per-EMITTED-frame counter — the cam1 render-tick id
        // burned into each frame. A clean 1:1 digital key the downstream verdict pairs on
        // (unlike the optical 60→30 beat). Only advanced when the burn is active.
        #[cfg(feature = "probe")]
        let mut burn_frame_id: u32 = 0;

        // Genlock decimation state: emit the first capture at/after each target
        // wall-clock boundary, skip the rest. interval_ns 0 => decimation off.
        let out_interval_ns: u64 = genlock_fps
            .map(|f| 1_000_000_000u64 / f as u64)
            .unwrap_or(0);
        let mut next_boundary_ns: u64 = 0;

        while running_capture.load(Ordering::Relaxed) {
            // ZERO-COPY: Process frame directly from mmap buffer without copying
            let result = capture.process_frame(|data, info| {
                if out_interval_ns > 0 {
                    // Genlock decimation: emit only the capture at/after each
                    // wall-clock boundary (pure logic in ndi::genlock_emit_gate).
                    let (emit, next) = camera_box::ndi::genlock_emit_gate(
                        wall_clock_ns(),
                        next_boundary_ns,
                        out_interval_ns,
                    );
                    next_boundary_ns = next;
                    if !emit {
                        return; // between boundaries — decimate (don't send)
                    }
                }
                // #174 TEST-MODE burn: when CAMERA_BOX_BURN_RUN_ID is set, copy the frame,
                // draw the cam1 render-time QR into its luma, and send the COPY. When unset
                // (production), the zero-copy send is taken verbatim — no allocation, no QR,
                // feed stays clean. `burned` keeps the copy alive across the send call.
                #[cfg(feature = "probe")]
                let mut burned: Vec<u8>;
                #[cfg(feature = "probe")]
                let send_data: &[u8] = match burn_run_id {
                    Some(run_id) if info.fourcc.str().unwrap_or("") == "YUYV" => {
                        burned = data.to_vec();
                        let payload = camera_box::probe::payload::Payload {
                            run_id,
                            frame_id: burn_frame_id,
                            gen_ts_ns: wall_clock_ns() as i64,
                        };
                        camera_box::probe::qr::burn_qr_yuyv(
                            &mut burned,
                            info.width,
                            info.height,
                            info.stride,
                            &payload,
                            camera_box::probe::qr::CAM1_BURN_QR_PX,
                        );
                        burn_frame_id = burn_frame_id.wrapping_add(1);
                        &burned
                    }
                    _ => data,
                };
                #[cfg(not(feature = "probe"))]
                let send_data: &[u8] = data;

                if let Err(e) = sender.send_frame_zero_copy(send_data, info) {
                    tracing::error!("Failed to send frame: {}", e);
                }
                emit_count += 1; // reached only when the frame passed the gate

                // #105 node 2 — tee the EMITTED frame to the cam1 grab recording. Stamp
                // the grab instant on the SAME wall clock (CLOCK_REALTIME) the cam2
                // painter uses, so the recording-verdict computes the real cam2→cam1
                // latency. A broken grab stream stops recording but NEVER the NDI send
                // (the measured pipeline must not be disturbed by a recorder fault).
                if let Some(rec) = grab_recorder.as_mut() {
                    if let Err(e) = rec.write_frame(data, wall_clock_ns() as i64) {
                        tracing::error!("grab-record write failed, stopping recorder: {}", e);
                        grab_recorder = None;
                    }
                }
            });

            match result {
                Ok(()) => {
                    frame_count += 1;

                    // Report fps every 5 seconds. Under genlock decimation the
                    // emit rate (frames actually sent) differs from the capture
                    // rate — log both so a decimation regression (e.g. emitting
                    // 0 or 60 instead of 30) is visible on-device.
                    let elapsed = last_report.elapsed();
                    if elapsed.as_secs() >= 5 {
                        let secs = elapsed.as_secs_f64();
                        let cap_fps = frame_count as f64 / secs;
                        let dropped = capture.dropped_captures();
                        if out_interval_ns > 0 {
                            let emit_fps = emit_count as f64 / secs;
                            tracing::info!(
                                "Streaming: {:.1} fps emitted / {:.1} fps captured ({} sent, {} captured, {} capture-dropped)",
                                emit_fps,
                                cap_fps,
                                emit_count,
                                frame_count,
                                dropped
                            );
                        } else {
                            tracing::info!(
                                "Streaming: {:.1} fps ({} frames, {} capture-dropped)",
                                cap_fps,
                                frame_count,
                                dropped
                            );
                        }
                        frame_count = 0;
                        emit_count = 0;
                        last_report = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to capture frame: {}", e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        // Loop exited (shutdown): flush + close the grab recording so the last frames
        // and sidecar rows reach dev1/disk.
        if let Some(rec) = grab_recorder.take() {
            rec.finish();
        }
        // cam2→cam1 LOSS sidecar: write cam1's final V4L2 capture-drop count so the verdict
        // reports the camera-leg loss (a non-fatal best effort — a write failure only means
        // the verdict can't report cam2→cam1 loss, it must not abort the shutdown).
        if let Some(path) = &capture_stats_path {
            match capture.write_capture_stats(path) {
                Ok(()) => tracing::info!(
                    "cam2→cam1 LOSS sidecar written: {} ({} V4L2 capture-drops over {} captured)",
                    path,
                    capture.dropped_captures(),
                    capture.frames_captured()
                ),
                Err(e) => tracing::error!("failed to write cam2→cam1 capture-stats sidecar: {e:#}"),
            }
        }
    });

    // Wait for shutdown signal
    tracing::info!("Streaming started. Press Ctrl+C to stop.");
    signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");

    // Signal all threads to stop
    running.store(false, Ordering::Relaxed);

    // Wait for capture loop (with timeout)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), capture_handle).await;

    // Wait for display thread if running
    if let Some(handle) = display_handle {
        let _ = handle.join();
    }

    // Wait for intercom thread if running
    if let Some(handle) = intercom_handle {
        let _ = handle.join();
    }

    tracing::info!("camera-box stopped");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn test_args_parse_default() {
        // Test that default values are correct
        let args = Args::try_parse_from(["camera-box"]).unwrap();
        assert_eq!(args.config, PathBuf::from("/etc/camera-box/config.toml"));
        assert!(args.device.is_none());
        assert!(args.display_source.is_none());
        assert_eq!(args.fb_device, "/dev/fb0");
        assert!(!args.debug);
        assert!(args.intercom_stream.is_none());
        assert_eq!(args.intercom_target, "strih.lan");
    }

    #[test]
    fn test_args_parse_with_device() {
        let args = Args::try_parse_from(["camera-box", "--device", "/dev/video2"]).unwrap();
        assert_eq!(args.device, Some("/dev/video2".to_string()));
    }

    #[test]
    fn test_args_parse_with_config() {
        let args = Args::try_parse_from(["camera-box", "-c", "/custom/config.toml"]).unwrap();
        assert_eq!(args.config, PathBuf::from("/custom/config.toml"));
    }

    #[test]
    fn test_args_parse_with_display() {
        let args =
            Args::try_parse_from(["camera-box", "--display", "STRIH-SNV (interkom)"]).unwrap();
        assert_eq!(
            args.display_source,
            Some("STRIH-SNV (interkom)".to_string())
        );
    }

    #[test]
    fn test_args_parse_with_intercom() {
        let args = Args::try_parse_from([
            "camera-box",
            "--intercom",
            "cam1",
            "--intercom-target",
            "192.168.1.100",
        ])
        .unwrap();
        assert_eq!(args.intercom_stream, Some("cam1".to_string()));
        assert_eq!(args.intercom_target, "192.168.1.100");
    }

    #[test]
    fn test_args_parse_debug_flag() {
        let args = Args::try_parse_from(["camera-box", "--debug"]).unwrap();
        assert!(args.debug);
    }

    #[test]
    fn test_args_record_grab_off_by_default() {
        // #105 node 2: --record-grab is OFF unless given (normal operation unaffected).
        let args = Args::try_parse_from(["camera-box"]).unwrap();
        assert!(args.record_grab.is_none());
        assert_eq!(args.record_grab_ts, "/tmp/cam1-grab-ts.csv");
    }

    #[test]
    fn test_args_record_grab_tcp_dest_and_sidecar() {
        let args = Args::try_parse_from([
            "camera-box",
            "--record-grab",
            "tcp://10.77.9.21:9099",
            "--record-grab-ts",
            "/tmp/grab.csv",
        ])
        .unwrap();
        assert_eq!(args.record_grab.as_deref(), Some("tcp://10.77.9.21:9099"));
        assert_eq!(args.record_grab_ts, "/tmp/grab.csv");
    }

    #[test]
    fn test_args_parse_fb_device() {
        let args = Args::try_parse_from(["camera-box", "--fb-device", "/dev/fb1"]).unwrap();
        assert_eq!(args.fb_device, "/dev/fb1");
    }

    #[test]
    fn test_args_command_valid() {
        // Ensure the command can be built
        Args::command().debug_assert();
    }

    #[test]
    fn test_args_all_options() {
        let args = Args::try_parse_from([
            "camera-box",
            "-c",
            "/custom/config.toml",
            "-d",
            "/dev/video3",
            "--display",
            "NDI Source",
            "--fb-device",
            "/dev/fb1",
            "--debug",
            "--intercom",
            "cam2",
            "--intercom-target",
            "host.lan",
        ])
        .unwrap();

        assert_eq!(args.config, PathBuf::from("/custom/config.toml"));
        assert_eq!(args.device, Some("/dev/video3".to_string()));
        assert_eq!(args.display_source, Some("NDI Source".to_string()));
        assert_eq!(args.fb_device, "/dev/fb1");
        assert!(args.debug);
        assert_eq!(args.intercom_stream, Some("cam2".to_string()));
        assert_eq!(args.intercom_target, "host.lan");
    }
}
