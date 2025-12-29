mod capture;
mod config;
mod ndi;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tokio::signal;
use tracing_subscriber::EnvFilter;

use crate::capture::VideoCapture;
use crate::config::Config;
use crate::ndi::NdiSender;

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

    /// Enable debug logging
    #[arg(long)]
    debug: bool,
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

    // Run the capture loop
    run_capture_loop(&device_path, &config.ndi_name).await
}

async fn run_capture_loop(device_path: &str, ndi_name: &str) -> Result<()> {
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
    let capture_handle = tokio::task::spawn_blocking(move || {
        // Apply real-time optimizations BEFORE entering the capture loop
        apply_realtime_optimizations();

        let mut frame_count: u64 = 0;
        let mut last_report = std::time::Instant::now();

        loop {
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

    // Abort capture loop
    capture_handle.abort();
    tracing::info!("camera-box stopped");

    Ok(())
}
