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
    /// Open capture device and start streaming
    pub fn open(device_path: &str) -> Result<Self> {
        tracing::info!("Opening capture device: {}", device_path);

        let device = Device::with_path(device_path)
            .with_context(|| format!("Failed to open device: {}", device_path))?;

        // Query device capabilities
        let caps = device.query_caps()?;
        tracing::info!("Device: {} ({})", caps.card, caps.driver);

        // Get current format (auto-negotiated by driver)
        let format = Capture::format(&device)?;
        let width = format.width;
        let height = format.height;
        let fourcc = format.fourcc;
        let stride = format.stride;

        tracing::info!(
            "Capture format: {}x{} {} (stride: {})",
            width,
            height,
            fourcc,
            stride
        );

        // Try to set preferred formats in order of preference for NDI
        // UYVY is native NDI format, YUYV needs simple byte swap
        let preferred_formats = [
            FourCC::new(b"UYVY"),
            FourCC::new(b"YUYV"),
            FourCC::new(b"NV12"),
        ];

        let mut final_format = format;
        for preferred in preferred_formats {
            let mut try_format = final_format;
            try_format.fourcc = preferred;
            if let Ok(set_format) = Capture::set_format(&device, &try_format) {
                if set_format.fourcc == preferred {
                    final_format = set_format;
                    tracing::info!("Set preferred format: {}", preferred);
                    break;
                }
            }
        }

        let width = final_format.width;
        let height = final_format.height;
        let fourcc = final_format.fourcc;
        let stride = final_format.stride;

        // Get frame rate from device parameters
        let frame_rate = match Capture::params(&device) {
            Ok(params) => {
                let interval = params.interval;
                // V4L2 gives us frame interval (seconds per frame) as numerator/denominator
                // We need frame rate (frames per second), so we swap them
                let frame_rate = FrameRate {
                    numerator: interval.denominator,
                    denominator: interval.numerator,
                };
                tracing::info!(
                    "Detected frame rate: {}/{} ({:.2} fps)",
                    frame_rate.numerator,
                    frame_rate.denominator,
                    frame_rate.numerator as f64 / frame_rate.denominator as f64
                );
                frame_rate
            }
            Err(e) => {
                tracing::warn!("Could not get frame rate from device: {}, using default", e);
                FrameRate::default()
            }
        };

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
