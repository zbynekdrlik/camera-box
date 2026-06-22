use anyhow::{Context, Result};
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// Video frame metadata (data passed separately as zero-copy reference)
#[derive(Clone, Copy)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub fourcc: FourCC,
    pub stride: u32,
}

/// Video frame data with metadata (for compatibility, still used for owned data)
pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fourcc: FourCC,
    pub stride: u32,
}

/// Frame rate as numerator/denominator
#[derive(Debug, Clone, Copy)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl Default for FrameRate {
    fn default() -> Self {
        // Default to 30000/1001 (29.97 fps) if detection fails
        Self {
            numerator: 30000,
            denominator: 1001,
        }
    }
}

/// Derive a frame rate (fps) from a V4L2 capture interval.
///
/// V4L2 expresses the capture interval as a PERIOD — seconds per frame
/// (`numerator/denominator` s). Frames-per-second is the reciprocal, so a
/// `1/60` interval is 60 fps and a `1001/60000` interval is 59.94 fps. A zero
/// numerator or denominator means the device reported no usable interval and
/// falls back to the NTSC-safe default. Deriving the rate from the negotiated
/// interval (instead of hard-coding it) keeps the NDI-advertised rate and the
/// genlock pacing honest about what the capture device actually delivers.
pub fn frame_rate_from_interval(interval_numerator: u32, interval_denominator: u32) -> FrameRate {
    if interval_numerator == 0 || interval_denominator == 0 {
        return FrameRate::default();
    }
    // fps = 1 / period = denominator / numerator
    FrameRate {
        numerator: interval_denominator,
        denominator: interval_numerator,
    }
}

/// The v4l2 capture-interval denominator (frames/sec) to request. The rig runs a
/// true-30 fps chain (no 60→30 decimation), so `CAMERA_BOX_CAPTURE_FPS=30` lets the
/// device negotiate native 30; unset / 0 / invalid keeps the 60 fps default (#11).
pub fn requested_capture_denominator(override_fps: Option<u32>) -> u32 {
    override_fps.filter(|&f| f > 0).unwrap_or(60)
}

/// Number of frames the CAPTURE DEVICE silently dropped between two consecutive
/// delivered buffers, from their V4L2 `sequence` numbers (the kernel increments
/// `sequence` once per CAPTURED frame, skipping the value of any frame the driver
/// could not deliver). Consecutive frames (`cur == prev + 1`) ⇒ 0. A jump ⇒ the
/// skipped count. `u32` wrapping is handled (`wrapping_sub`), and `cur == prev`
/// (a duplicate/no-advance, never expected) ⇒ 0. This is capture-card loss —
/// distinct from genlock-pipeline loss — that the QR instrument was previously
/// blind to (the `sequence` was discarded at the `stream.next()` call sites).
pub fn sequence_gap(prev: u32, cur: u32) -> u32 {
    cur.wrapping_sub(prev).saturating_sub(1)
}

/// Extract the gray8 (luma) plane from a packed YUYV (YUY2) capture buffer.
///
/// YUYV packs `Y0 U0 Y1 V0` per 4 bytes = 2 pixels: the luma is every EVEN byte
/// (`Y` at 0,2,4,…), chroma the odd bytes. This returns one luma byte per pixel
/// (`width*height` bytes), honoring `stride` (bytes per row; YUYV stride = `2*width`
/// for a tightly packed frame but a device may pad). This is the cam1 GRAB-RECORD
/// extraction (#105 node 2): a prod-clean, dependency-free luma plane the
/// `--record-grab` mode streams to dev1 for ffv1 encoding + QR decode. It deliberately
/// drops chroma — the QR the camera filmed is fully readable from luma, and gray8
/// halves the wire bytes vs full YUYV (so the grab-stream perturbs the measured
/// pipeline less). Rows/cols beyond the buffer are skipped (defensive against a short
/// final buffer); a too-small input yields a zero-padded plane rather than panicking.
pub fn yuyv_to_gray8(data: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let stride = stride as usize;
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            // luma byte for pixel x is at row + 2*x (even byte in the YUYV pair).
            let idx = row + 2 * x;
            if idx < data.len() {
                out[y * w + x] = data[idx];
            }
        }
    }
    out
}

/// V4L2 video capture wrapper
pub struct VideoCapture {
    stream: Stream<'static>,
    width: u32,
    height: u32,
    fourcc: FourCC,
    stride: u32,
    frame_rate: FrameRate,
    /// V4L2 `sequence` of the last delivered buffer, for capture-drop detection
    /// ([`sequence_gap`]). `None` until the first frame.
    last_sequence: Option<u32>,
    /// Cumulative count of frames the capture device dropped over this stream's life.
    dropped_captures: u64,
}

impl VideoCapture {
    /// Open capture device and start streaming at 1920x1080 @ 60fps
    pub fn open(device_path: &str) -> Result<Self> {
        tracing::info!("Opening capture device: {}", device_path);

        let device = Device::with_path(device_path)
            .with_context(|| format!("Failed to open device: {}", device_path))?;

        // Query device capabilities
        let caps = device.query_caps()?;
        tracing::info!("Device: {} ({})", caps.card, caps.driver);

        // Get current format as starting point
        let mut format = Capture::format(&device)?;

        // Set 1920x1080 YUYV (best for NDI conversion)
        format.width = 1920;
        format.height = 1080;
        format.fourcc = FourCC::new(b"YUYV");

        let final_format =
            Capture::set_format(&device, &format).context("Failed to set 1920x1080 YUYV format")?;

        tracing::info!(
            "Capture format: {}x{} {} (stride: {})",
            final_format.width,
            final_format.height,
            final_format.fourcc,
            final_format.stride
        );

        let width = final_format.width;
        let height = final_format.height;
        let fourcc = final_format.fourcc;
        let stride = final_format.stride;

        // Request frame rate for the genlock/NDI pipeline (#11 quality bar). The
        // frame rate is derived from the rate the driver actually negotiates,
        // not hard-coded — so NDI metadata and genlock pacing stay honest about
        // what the capture device delivers.
        let frame_rate = match Capture::params(&device) {
            Ok(mut params) => {
                params.interval.numerator = 1;
                let req = requested_capture_denominator(
                    std::env::var("CAMERA_BOX_CAPTURE_FPS")
                        .ok()
                        .and_then(|s| s.parse().ok()),
                );
                params.interval.denominator = req;
                let negotiated = Capture::set_params(&device, &params).unwrap_or(params);
                frame_rate_from_interval(
                    negotiated.interval.numerator,
                    negotiated.interval.denominator,
                )
            }
            Err(_) => frame_rate_from_interval(1, 60),
        };
        tracing::info!(
            "Frame rate: {:.3} fps ({}/{})",
            frame_rate.numerator as f64 / frame_rate.denominator as f64,
            frame_rate.numerator,
            frame_rate.denominator
        );

        // Create memory-mapped stream with enough buffers to avoid frame drops
        // 4 buffers to handle processing time variance
        let stream = Stream::with_buffers(&device, Type::VideoCapture, 4)
            .context("Failed to create capture stream")?;

        // Leak the device to get 'static lifetime (it lives for program duration)
        let stream = unsafe { std::mem::transmute::<Stream<'_>, Stream<'static>>(stream) };

        Ok(Self {
            stream,
            width,
            height,
            fourcc,
            stride,
            frame_rate,
            last_sequence: None,
            dropped_captures: 0,
        })
    }

    /// Record a delivered buffer's V4L2 `sequence`, accounting for any frames the
    /// capture device dropped since the previous buffer ([`sequence_gap`]). Logs
    /// each gap with the surrounding sequence numbers and keeps a running total.
    fn record_sequence(&mut self, seq: u32) {
        if let Some(prev) = self.last_sequence {
            let gap = sequence_gap(prev, seq);
            if gap > 0 {
                self.dropped_captures += gap as u64;
                tracing::warn!(
                    "capture device dropped {} frame(s): v4l2 sequence {} -> {} (total dropped {})",
                    gap,
                    prev,
                    seq,
                    self.dropped_captures
                );
            }
        }
        self.last_sequence = Some(seq);
    }

    /// Total frames the capture device has dropped over this stream's life
    /// (cumulative [`sequence_gap`]). Capture-card loss, not pipeline loss.
    /// Surfaced in the periodic streaming report (`main.rs`).
    pub fn dropped_captures(&self) -> u64 {
        self.dropped_captures
    }

    /// Capture next frame (blocking) - COPIES DATA
    #[allow(dead_code)]
    pub fn next_frame(&mut self) -> Result<Frame> {
        let (buffer, metadata) = self.stream.next()?;
        let seq = metadata.sequence;

        // Copy frame data (zero-copy would require unsafe lifetime tricks)
        let data = buffer.to_vec();
        // `buffer`/`metadata` borrow self.stream; record AFTER that borrow ends.
        self.record_sequence(seq);

        Ok(Frame {
            data,
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        })
    }

    /// Process next frame with zero-copy callback (FAST PATH)
    /// The callback receives a direct reference to the mmap buffer - no copying!
    /// Buffer is automatically requeued after callback returns.
    #[inline]
    pub fn process_frame<F>(&mut self, mut callback: F) -> Result<()>
    where
        F: FnMut(&[u8], FrameInfo),
    {
        let (buffer, metadata) = self.stream.next()?;
        let seq = metadata.sequence;

        let info = FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        };

        // Zero-copy: pass buffer slice directly to callback
        #[allow(clippy::needless_borrow)]
        callback(&buffer, info);

        // `buffer`/`metadata` borrow self.stream; record AFTER that borrow ends
        // (after the callback) so the capture-drop accounting can take &mut self.
        self.record_sequence(seq);

        // Buffer automatically requeued when it goes out of scope
        Ok(())
    }

    /// Get frame info without capturing
    #[allow(dead_code)]
    pub fn frame_info(&self) -> FrameInfo {
        FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        }
    }

    /// Get frame dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Get pixel format
    #[allow(dead_code)]
    pub fn fourcc(&self) -> FourCC {
        self.fourcc
    }

    /// Get frame rate
    pub fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_from_interval_60fps() {
        // 1080p60: V4L2 interval 1/60 s/frame -> 60 fps.
        let r = frame_rate_from_interval(1, 60);
        assert_eq!(r.numerator, 60);
        assert_eq!(r.denominator, 1);
        let fps = r.numerator as f64 / r.denominator as f64;
        assert!((fps - 60.0).abs() < 1e-9, "expected 60 fps, got {fps}");
    }

    #[test]
    fn frame_rate_from_interval_30fps() {
        // Legacy 30 fps interval still derives correctly.
        let r = frame_rate_from_interval(1, 30);
        assert_eq!(r.numerator, 30);
        assert_eq!(r.denominator, 1);
    }

    #[test]
    fn frame_rate_from_interval_5994fps() {
        // NTSC 59.94: interval 1001/60000 s/frame.
        let r = frame_rate_from_interval(1001, 60000);
        let fps = r.numerator as f64 / r.denominator as f64;
        assert!((fps - 59.94).abs() < 0.01, "expected 59.94 fps, got {fps}");
    }

    #[test]
    fn frame_rate_from_interval_invalid_falls_back() {
        // A zero numerator/denominator is not a usable interval -> default.
        assert_eq!(frame_rate_from_interval(0, 0).numerator, 30000);
        assert_eq!(frame_rate_from_interval(1, 0).denominator, 1001);
        assert_eq!(frame_rate_from_interval(0, 60).numerator, 30000);
    }

    #[test]
    fn test_frame_rate_default() {
        let rate = FrameRate::default();
        assert_eq!(rate.numerator, 30000);
        assert_eq!(rate.denominator, 1001);
        // 30000/1001 = ~29.97 fps
        let fps = rate.numerator as f64 / rate.denominator as f64;
        assert!((fps - 29.97).abs() < 0.01);
    }

    #[test]
    fn test_frame_rate_as_f64() {
        let rate = FrameRate {
            numerator: 60,
            denominator: 1,
        };
        let fps = rate.numerator as f64 / rate.denominator as f64;
        assert!((fps - 60.0).abs() < 0.001);

        let rate_ntsc = FrameRate {
            numerator: 60000,
            denominator: 1001,
        };
        let fps_ntsc = rate_ntsc.numerator as f64 / rate_ntsc.denominator as f64;
        assert!((fps_ntsc - 59.94).abs() < 0.01);
    }

    #[test]
    fn test_frame_rate_clone() {
        let rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        let cloned = rate;
        assert_eq!(rate.numerator, cloned.numerator);
        assert_eq!(rate.denominator, cloned.denominator);
    }

    #[test]
    fn test_frame_info_clone_copy() {
        let info = FrameInfo {
            width: 1920,
            height: 1080,
            fourcc: FourCC::new(b"YUYV"),
            stride: 3840,
        };
        // Test Copy trait
        let copied = info;
        assert_eq!(info.width, copied.width);
        assert_eq!(info.height, copied.height);
        assert_eq!(info.stride, copied.stride);
    }

    #[test]
    fn test_frame_info_fields() {
        let info = FrameInfo {
            width: 1280,
            height: 720,
            fourcc: FourCC::new(b"MJPG"),
            stride: 2560,
        };
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 720);
        assert_eq!(info.stride, 2560);
    }

    #[test]
    fn test_frame_construction() {
        let frame = Frame {
            data: vec![0u8; 1920 * 1080 * 2],
            width: 1920,
            height: 1080,
            fourcc: FourCC::new(b"YUYV"),
            stride: 3840,
        };
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.stride, 3840);
        assert_eq!(frame.data.len(), 1920 * 1080 * 2);
    }

    #[test]
    fn test_fourcc_formatting() {
        let fourcc = FourCC::new(b"YUYV");
        let display = format!("{}", fourcc);
        assert!(display.contains('Y') || display.len() == 4);
    }

    #[test]
    fn test_frame_rate_debug() {
        let rate = FrameRate {
            numerator: 30,
            denominator: 1,
        };
        let debug = format!("{:?}", rate);
        assert!(debug.contains("FrameRate"));
        assert!(debug.contains("30"));
    }

    #[test]
    fn sequence_gap_consecutive_is_zero() {
        assert_eq!(sequence_gap(10, 11), 0);
        assert_eq!(sequence_gap(0, 1), 0);
    }

    #[test]
    fn sequence_gap_counts_skipped_frames() {
        assert_eq!(sequence_gap(10, 12), 1); // frame 11 dropped
        assert_eq!(sequence_gap(10, 15), 4); // frames 11..14 dropped
    }

    #[test]
    fn sequence_gap_handles_u32_wrap() {
        // wraparound at u32::MAX -> 0 with no intervening drop is consecutive.
        assert_eq!(sequence_gap(u32::MAX, 0), 0);
        assert_eq!(sequence_gap(u32::MAX - 1, 1), 2); // MAX and 0 dropped
    }

    #[test]
    fn sequence_gap_same_or_no_advance_is_zero() {
        // A duplicate/no-advance (never expected) must not report a giant gap.
        assert_eq!(sequence_gap(10, 10), 0);
    }

    #[test]
    fn yuyv_to_gray8_picks_even_luma_bytes() {
        // 2x1 YUYV: Y0=10 U0=200 Y1=20 V0=201 → gray8 = [10, 20] (the even bytes).
        let yuyv = [10u8, 200, 20, 201];
        let gray = yuyv_to_gray8(&yuyv, 2, 1, 4);
        assert_eq!(gray, vec![10, 20], "luma is the even (Y) bytes only");
    }

    #[test]
    fn yuyv_to_gray8_honors_stride_padding() {
        // 2x2 with a padded stride of 6 bytes/row (4 luma+chroma + 2 pad). Row 0:
        // Y=1,Y=2; row 1: Y=3,Y=4. The 2 pad bytes per row must be skipped.
        let row0 = [1u8, 0, 2, 0, 0, 0]; // Y0=1 U Y1=2 V pad pad
        let row1 = [3u8, 0, 4, 0, 0, 0];
        let mut data = Vec::new();
        data.extend_from_slice(&row0);
        data.extend_from_slice(&row1);
        let gray = yuyv_to_gray8(&data, 2, 2, 6);
        assert_eq!(
            gray,
            vec![1, 2, 3, 4],
            "stride padding skipped, luma row-major"
        );
    }

    #[test]
    fn yuyv_to_gray8_short_buffer_zero_pads_no_panic() {
        // A truncated final buffer must not panic; missing pixels are 0.
        let yuyv = [9u8, 0]; // only 1 luma byte available for a 2x1 request
        let gray = yuyv_to_gray8(&yuyv, 2, 1, 4);
        assert_eq!(gray, vec![9, 0], "missing pixel zero-padded, no panic");
    }

    #[test]
    fn yuyv_to_gray8_output_is_one_byte_per_pixel() {
        let yuyv = vec![128u8; 4 * 4]; // 4 pixels (2x2) packed YUYV
        let gray = yuyv_to_gray8(&yuyv, 2, 2, 4);
        assert_eq!(gray.len(), 4, "one luma byte per pixel = width*height");
        assert!(gray.iter().all(|&b| b == 128));
    }

    #[test]
    fn capture_denominator_defaults_to_60_and_honors_override() {
        assert_eq!(requested_capture_denominator(None), 60);
        assert_eq!(requested_capture_denominator(Some(0)), 60); // 0 is invalid -> default
        assert_eq!(requested_capture_denominator(Some(30)), 30);
        assert_eq!(requested_capture_denominator(Some(60)), 60);
    }
}
