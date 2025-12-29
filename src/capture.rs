use anyhow::{Context, Result};
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// Video frame data with metadata
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

        // Set 60fps
        if let Ok(mut params) = Capture::params(&device) {
            params.interval.numerator = 1;
            params.interval.denominator = 60;
            let _ = Capture::set_params(&device, &params);
        }

        // Fixed frame rate: 60fps
        let frame_rate = FrameRate {
            numerator: 60,
            denominator: 1,
        };
        tracing::info!("Frame rate: 60 fps");

        // Create memory-mapped stream with minimal buffers for low latency
        // 4 buffers is minimum for stable streaming
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

    /// Capture next frame (blocking)
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
