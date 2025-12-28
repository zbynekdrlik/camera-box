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
    run_capture_loop(&device_path).await
}

async fn run_capture_loop(device_path: &str) -> Result<()> {
    // Open capture device
    let mut capture = VideoCapture::open(device_path)?;
    let (width, height) = capture.dimensions();
    tracing::info!("Capturing at {}x{}", width, height);

    // Create NDI sender
    let mut sender = NdiSender::new()?;
    tracing::info!("NDI sender ready, streaming as 'usb'");

    // Spawn capture loop in blocking task
    let capture_handle = tokio::task::spawn_blocking(move || {
        loop {
            match capture.next_frame() {
                Ok(frame) => {
                    if let Err(e) = sender.send_frame(&frame) {
                        tracing::error!("Failed to send frame: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to capture frame: {}", e);
                    // Small delay before retry
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
