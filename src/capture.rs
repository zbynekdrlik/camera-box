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
pub fn frame_rate_from_interval(_interval_numerator: u32, _interval_denominator: u32) -> FrameRate {
    // STUB — real body lands in the GREEN commit.
    FrameRate::default()
}

/// V4L2 video capture wrapper
pub struct VideoCapture {
    stream: Stream<'static>,
    width: u32,
    height: u32,
    fourcc: FourCC,
    stride: u32,
    frame_rate: FrameRate,
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

        // Set 30fps for genlock synchronization
        if let Ok(mut params) = Capture::params(&device) {
            params.interval.numerator = 1;
            params.interval.denominator = 30;
            let _ = Capture::set_params(&device, &params);
        }

        // Fixed frame rate: 30fps
        let frame_rate = FrameRate {
            numerator: 30,
            denominator: 1,
        };
        tracing::info!("Frame rate: 30 fps");

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
        })
    }

    /// Capture next frame (blocking) - COPIES DATA
    #[allow(dead_code)]
    pub fn next_frame(&mut self) -> Result<Frame> {
        let (buffer, _metadata) = self.stream.next()?;

        // Copy frame data (zero-copy would require unsafe lifetime tricks)
        let data = buffer.to_vec();

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
        let (buffer, _metadata) = self.stream.next()?;

        let info = FrameInfo {
            width: self.width,
            height: self.height,
            fourcc: self.fourcc,
            stride: self.stride,
        };

        // Zero-copy: pass buffer slice directly to callback
        #[allow(clippy::needless_borrow)]
        callback(&buffer, info);

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
}
