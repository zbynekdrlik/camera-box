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
    )
    .await
}

async fn run_capture_loop(
    device_path: &str,
    ndi_name: &str,
    display_config: Option<NdiDisplayConfig>,
    intercom_config: Option<intercom::IntercomConfig>,
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

    // Open capture device at 1920x1080 @ 60fps
    let mut capture = VideoCapture::open(device_path)?;
    let (width, height) = capture.dimensions();
    let frame_rate = capture.frame_rate();
    tracing::info!("Capturing at {}x{}", width, height);

    // Create NDI sender with configured name and detected frame rate
    let mut sender = NdiSender::new(ndi_name, frame_rate)?;
    tracing::info!("NDI sender ready, streaming as '{}'", ndi_name);
    tracing::info!("ZERO-COPY mode: AVX2 SIMD + sync send for lowest latency");

    // Spawn capture loop in blocking task - minimal overhead for lowest latency
    let running_capture = Arc::clone(&running);
    let capture_handle = tokio::task::spawn_blocking(move || {
        // Apply real-time optimizations BEFORE entering the capture loop
        apply_realtime_optimizations();

        let mut frame_count: u64 = 0;
        let mut last_report = std::time::Instant::now();

        while running_capture.load(Ordering::Relaxed) {
            // ZERO-COPY: Process frame directly from mmap buffer without copying
            let result = capture.process_frame(|data, info| {
                if let Err(e) = sender.send_frame_zero_copy(data, info) {
                    tracing::error!("Failed to send frame: {}", e);
                }
            });

            match result {
                Ok(()) => {
                    frame_count += 1;

                    // Report fps every 5 seconds
                    let elapsed = last_report.elapsed();
                    if elapsed.as_secs() >= 5 {
                        let fps = frame_count as f64 / elapsed.as_secs_f64();
                        tracing::info!("Streaming: {:.1} fps ({} frames)", fps, frame_count);
                        frame_count = 0;
                        last_report = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to capture frame: {}", e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
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
