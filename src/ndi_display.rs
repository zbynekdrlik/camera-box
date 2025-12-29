//! NDI Display - receives NDI stream and displays on local HDMI output
//!
//! This module provides a simple NDI receiver that displays video on the local
//! framebuffer. Designed to run at low priority to not interfere with the
//! camera capture/send pipeline.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::display::FramebufferDisplay;
use crate::ndi::NdiReceiver;

/// NDI display configuration
pub struct NdiDisplayConfig {
    /// NDI source name to search for (partial match)
    pub source_name: String,
    /// Framebuffer device path
    pub fb_device: String,
    /// Timeout for finding NDI source (seconds)
    pub find_timeout_secs: u32,
}

impl Default for NdiDisplayConfig {
    fn default() -> Self {
        Self {
            source_name: String::new(),
            fb_device: "/dev/fb0".to_string(),
            find_timeout_secs: 30,
        }
    }
}

/// Run the NDI display loop
/// This should be called from a low-priority thread
pub fn run_display_loop(config: NdiDisplayConfig, running: Arc<AtomicBool>) -> Result<()> {
    tracing::info!(
        "NDI display starting, searching for source: {}",
        config.source_name
    );

    // Connect to NDI source
    let mut receiver = match NdiReceiver::connect(&config.source_name, config.find_timeout_secs) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to connect to NDI source: {}", e);
            return Err(e);
        }
    };

    // Open framebuffer
    let mut display = match FramebufferDisplay::open(&config.fb_device) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to open framebuffer: {}", e);
            return Err(e);
        }
    };

    let (fb_width, fb_height) = display.dimensions();
    tracing::info!(
        "NDI display ready: {} -> framebuffer {}x{}",
        config.source_name,
        fb_width,
        fb_height
    );

    let mut frame_count: u64 = 0;
    let mut last_report = std::time::Instant::now();

    let mut no_frame_count: u64 = 0;

    // Main display loop
    while running.load(Ordering::Relaxed) {
        // Capture frame with 100ms timeout
        match receiver.capture_frame(100) {
            Ok(Some(frame)) => {
                no_frame_count = 0;

                // Debug: log fourcc on first frame
                if frame_count == 0 {
                    let fourcc_bytes = frame.fourcc.to_le_bytes();
                    let fourcc_str = std::str::from_utf8(&fourcc_bytes).unwrap_or("????");
                    tracing::info!("NDI display: first frame fourcc={} (0x{:08x}), size={}x{}, data_len={}",
                        fourcc_str, frame.fourcc, frame.width, frame.height, frame.data.len());
                }

                // Display the frame
                if let Err(e) =
                    display.display_frame(&frame.data, frame.width, frame.height, frame.fourcc)
                {
                    tracing::error!("Failed to display frame: {}", e);
                }

                frame_count += 1;

                // Report fps every 10 seconds (less frequent than camera)
                let elapsed = last_report.elapsed();
                if elapsed.as_secs() >= 10 {
                    let fps = frame_count as f64 / elapsed.as_secs_f64();
                    tracing::info!(
                        "NDI display: {:.1} fps ({}x{} -> {}x{})",
                        fps,
                        frame.width,
                        frame.height,
                        fb_width,
                        fb_height
                    );
                    frame_count = 0;
                    last_report = std::time::Instant::now();
                }
            }
            Ok(None) => {
                // No frame available
                no_frame_count += 1;
                if no_frame_count == 50 {
                    tracing::warn!("NDI display: No frames received for 5 seconds");
                }
                if no_frame_count % 100 == 0 {
                    tracing::debug!("NDI display: waiting for frames... ({})", no_frame_count);
                }
            }
            Err(e) => {
                tracing::error!("Failed to capture frame: {}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    tracing::info!("NDI display stopped");
    Ok(())
}

/// Apply low-priority settings for the display thread
/// This ensures the display doesn't interfere with camera capture
pub fn apply_low_priority() {
    // Set nice value to lowest priority (19)
    unsafe {
        let result = libc::nice(19);
        if result != -1 {
            tracing::info!("NDI display: nice value set to 19 (lowest priority)");
        }
    }

    // Set CPU affinity to different core than camera (core 0 or 2)
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();

        // Use core 0 (camera uses core 1)
        libc::CPU_SET(0, &mut cpuset);

        let result = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);

        if result == 0 {
            tracing::info!("NDI display: CPU affinity set to core 0");
        } else {
            tracing::debug!("NDI display: Could not set CPU affinity (non-critical)");
        }
    }
}
