//! NDI Display - receives NDI stream and displays on local HDMI output
//!
//! This module provides a simple NDI receiver that displays video on the local
//! framebuffer. Designed to run at low priority to not interfere with the
//! camera capture/send pipeline.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::display::{any_connector_connected, FramebufferDisplay};
use crate::ndi::NdiReceiver;

/// DRM sysfs class dir whose connector `status` files report monitor presence (#135).
const DRM_CLASS_DIR: &str = "/sys/class/drm";

/// Re-poll the connector status every N delivered frames (~1s at 30fps). A plain
/// sysfs read is cheap, but no need to do it every frame.
const CONNECTOR_RECHECK_FRAMES: u64 = 30;

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

/// Severity for a "no frames received" gap on the display NDI receiver.
///
/// #130: on a camera with no NDI display feed (e.g. cam2, where the display source
/// never delivers), the receiver legitimately polls `Ok(None)` for long stretches
/// while capture/emit run at a steady 30 fps. The old code unconditionally logged
/// `WARN: NDI display: No frames received for 5 seconds`, flooding the journal during
/// perfectly normal operation. A no-frame gap is only worth a WARN if the display had
/// previously been receiving frames and then STALLED (a genuine total-stall signal);
/// a receiver that has never delivered a frame is simply "source not feeding this
/// display" — a benign DEBUG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoFrameLevel {
    /// Benign: never received a frame on this connection — log at DEBUG.
    Debug,
    /// Genuine stall: was receiving frames, then stopped — log at WARN.
    Warn,
}

/// Decide the log severity for a "no frames" gap. WARN only when the receiver had
/// actually been delivering frames before the gap (a real stall); otherwise DEBUG
/// (the source simply isn't feeding this display — normal on a monitor-less cam).
pub fn no_frame_log_level(frames_received_on_connection: u64) -> NoFrameLevel {
    if frames_received_on_connection > 0 {
        NoFrameLevel::Warn
    } else {
        NoFrameLevel::Debug
    }
}

/// Run the NDI display loop with automatic reconnection
/// This should be called from a low-priority thread
pub fn run_display_loop(config: NdiDisplayConfig, running: Arc<AtomicBool>) -> Result<()> {
    tracing::info!(
        "NDI display starting, searching for source: {}",
        config.source_name
    );

    // Open framebuffer (retry indefinitely until display is connected)
    let mut display;
    let mut attempt = 0u32;
    loop {
        if !running.load(Ordering::Relaxed) {
            anyhow::bail!("Shutdown requested");
        }
        attempt = attempt.saturating_add(1);
        match FramebufferDisplay::open(&config.fb_device) {
            Ok(d) => {
                tracing::info!("Framebuffer opened successfully");
                display = d;
                break;
            }
            Err(e) => {
                // Log every 30 seconds (15 attempts * 2 seconds)
                if attempt % 15 == 1 {
                    tracing::warn!(
                        "Waiting for display (attempt {}): {} - will keep retrying...",
                        attempt,
                        e
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
    let (fb_width, fb_height) = display.dimensions();

    // Outer reconnection loop - keeps trying to connect/reconnect
    while running.load(Ordering::Relaxed) {
        // Try to connect to NDI source
        tracing::info!(
            "NDI display: connecting to source '{}'...",
            config.source_name
        );
        let mut receiver = match NdiReceiver::connect(&config.source_name, config.find_timeout_secs)
        {
            Ok(r) => {
                tracing::info!(
                    "NDI display ready: {} -> framebuffer {}x{}",
                    config.source_name,
                    fb_width,
                    fb_height
                );
                r
            }
            Err(e) => {
                tracing::warn!("Failed to connect to NDI source: {}, retrying in 5s...", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        let mut frame_count: u64 = 0;
        let mut last_report = std::time::Instant::now();
        let mut no_frame_count: u64 = 0;
        let mut first_frame = true;
        // Frames delivered on THIS connection (does NOT reset with the 10s fps
        // window). #130: only escalate a no-frame gap to WARN once the receiver has
        // actually been feeding frames (a real stall); before that it's a benign
        // DEBUG ("source not delivering to this display" — normal on a cam with no
        // display feed, e.g. cam2 — which previously flooded the journal).
        let mut frames_this_connection: u64 = 0;
        // #135: connector-presence gate. When the DRM connector reports no monitor
        // (a disconnected/latched "phantom" fb after hot-unplug), SKIP rendering — the
        // fb still opens and writes fine, so this is the only signal that there is no
        // real monitor. Re-checked every ~1s. `None` (unknown sysfs layout) → render
        // (never silently go dark). Polled BEFORE the first frame so a phantom fb never
        // gets even one heavy write.
        let mut connector_present = any_connector_connected(DRM_CLASS_DIR).unwrap_or(true);
        // Last connector presence we LOGGED (`None` = nothing logged yet). The initial
        // state — connected OR disconnected — is logged once on the first frame, then
        // only on a change. This guarantees a boot-with-monitor-unplugged run still
        // emits the "no monitor — skipping render" diagnostic.
        let mut logged_connector_state: Option<bool> = None;

        // Inner display loop - runs until source disappears
        while running.load(Ordering::Relaxed) {
            // Capture frame with 100ms timeout
            match receiver.capture_frame(100) {
                Ok(Some(frame)) => {
                    no_frame_count = 0;
                    frames_this_connection += 1;

                    // #135: re-poll connector presence ~once/sec.
                    if frames_this_connection.is_multiple_of(CONNECTOR_RECHECK_FRAMES) {
                        connector_present = any_connector_connected(DRM_CLASS_DIR).unwrap_or(true);
                    }

                    // Debug: log fourcc on first frame
                    if first_frame {
                        let fourcc_bytes = frame.fourcc.to_le_bytes();
                        let fourcc_str = std::str::from_utf8(&fourcc_bytes).unwrap_or("????");
                        tracing::info!(
                            "NDI display: first frame fourcc={} (0x{:08x}), size={}x{}, data_len={}",
                            fourcc_str,
                            frame.fourcc,
                            frame.width,
                            frame.height,
                            frame.data.len()
                        );
                        first_frame = false;
                    }

                    // #135: only render when a monitor is actually connected. A
                    // disconnected connector with a latched/phantom fb would otherwise
                    // get a heavy software upscale to no real screen (the pre-event
                    // incident: 1080→4K → 99.9% CPU). When disconnected, skip the
                    // render+upscale entirely; capture/emit are unaffected.
                    // Log the connector state once initially, then on every change.
                    if logged_connector_state != Some(connector_present) {
                        if connector_present {
                            tracing::info!("NDI display: monitor connected — rendering");
                        } else {
                            tracing::info!(
                                "NDI display: no monitor connected (DRM status=disconnected) — skipping render to avoid upscaling to a phantom framebuffer"
                            );
                        }
                        logged_connector_state = Some(connector_present);
                    }

                    if connector_present {
                        // Display the frame (ignore errors - display may be disconnected)
                        if let Err(e) = display.display_frame(
                            &frame.data,
                            frame.width,
                            frame.height,
                            frame.fourcc,
                        ) {
                            // Only log occasionally to avoid spam
                            if frame_count.is_multiple_of(300) {
                                tracing::warn!(
                                    "Display write failed (monitor disconnected?): {}",
                                    e
                                );
                            }
                        }
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

                    // After 10 seconds (100 * 100ms) with no frames, reconnect.
                    // #130: WARN only if frames had actually been flowing (a real
                    // stall); otherwise DEBUG — the source simply isn't feeding this
                    // display, normal on a monitor-less cam, and must not flood logs.
                    if no_frame_count >= 100 {
                        match no_frame_log_level(frames_this_connection) {
                            NoFrameLevel::Warn => tracing::warn!(
                                "NDI display: No frames for 10 seconds, reconnecting..."
                            ),
                            NoFrameLevel::Debug => tracing::debug!(
                                "NDI display: no frames for 10s (source not feeding), reconnecting..."
                            ),
                        }
                        break; // Exit inner loop to reconnect
                    }

                    if no_frame_count == 50 {
                        match no_frame_log_level(frames_this_connection) {
                            NoFrameLevel::Warn => {
                                tracing::warn!("NDI display: No frames received for 5 seconds")
                            }
                            NoFrameLevel::Debug => tracing::debug!(
                                "NDI display: no frames for 5s (source not feeding this display)"
                            ),
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("NDI display: capture error: {}, reconnecting...", e);
                    break; // Exit inner loop to reconnect
                }
            }
        }

        // Receiver will be dropped here, then we retry connection in outer loop
        if running.load(Ordering::Relaxed) {
            tracing::info!("NDI display: disconnected, will reconnect in 2s...");
            std::thread::sleep(std::time::Duration::from_secs(2));
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

    // Set CPU affinity to core 0 (camera uses isolated core 3, intercom uses core 1)
    unsafe {
        let mut cpuset: libc::cpu_set_t = std::mem::zeroed();

        // Use core 0 for display
        libc::CPU_SET(0, &mut cpuset);

        let result = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);

        if result == 0 {
            tracing::info!("NDI display: CPU affinity set to core 0");
        } else {
            tracing::debug!("NDI display: Could not set CPU affinity (non-critical)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ndi_display_config_default() {
        let config = NdiDisplayConfig::default();
        assert!(config.source_name.is_empty());
        assert_eq!(config.fb_device, "/dev/fb0");
        assert_eq!(config.find_timeout_secs, 30);
    }

    #[test]
    fn test_ndi_display_config_custom() {
        let config = NdiDisplayConfig {
            source_name: "STRIH-SNV (interkom)".to_string(),
            fb_device: "/dev/fb1".to_string(),
            find_timeout_secs: 60,
        };
        assert_eq!(config.source_name, "STRIH-SNV (interkom)");
        assert_eq!(config.fb_device, "/dev/fb1");
        assert_eq!(config.find_timeout_secs, 60);
    }

    #[test]
    fn no_frame_gap_is_debug_when_never_received() {
        // #130: a display receiver that has NEVER delivered a frame on this connection
        // (cam2: the display NDI source isn't feeding it) must NOT escalate a no-frame
        // gap to WARN — that floods the journal during normal monitor-less operation.
        assert_eq!(no_frame_log_level(0), NoFrameLevel::Debug);
    }

    #[test]
    fn no_frame_gap_is_warn_after_a_real_stall() {
        // If frames WERE flowing and then stopped, that's a genuine total-stall signal
        // and stays a WARN — we must not suppress a real stall.
        assert_eq!(no_frame_log_level(1), NoFrameLevel::Warn);
        assert_eq!(no_frame_log_level(900), NoFrameLevel::Warn);
    }

    #[test]
    fn test_ndi_display_config_fields() {
        let config = NdiDisplayConfig {
            source_name: "test".to_string(),
            fb_device: "/dev/fb0".to_string(),
            find_timeout_secs: 10,
        };
        // Verify all fields are accessible
        assert!(!config.source_name.is_empty());
        assert!(!config.fb_device.is_empty());
        assert!(config.find_timeout_secs > 0);
    }
}
