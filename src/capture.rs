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
    ///
    /// Parameters:
    /// - `device_path`: Path to V4L2 device
    /// - `req_width`: Requested width (0 = auto, try highest)
    /// - `req_height`: Requested height (0 = auto, try highest)
    /// - `req_fps`: Requested frame rate (0 = auto, try highest)
    pub fn open(device_path: &str, req_width: u32, req_height: u32, req_fps: u32) -> Result<Self> {
        tracing::info!("Opening capture device: {}", device_path);

        let device = Device::with_path(device_path)
            .with_context(|| format!("Failed to open device: {}", device_path))?;

        // Query device capabilities
        let caps = device.query_caps()?;
        tracing::info!("Device: {} ({})", caps.card, caps.driver);

        // Get current format as starting point
        let format = Capture::format(&device)?;
        tracing::info!(
            "Default format: {}x{} {} (stride: {})",
            format.width,
            format.height,
            format.fourcc,
            format.stride
        );

        // Preferred pixel formats for NDI (UYVY native, YUYV simple swap, NV12 convert)
        let preferred_formats = [
            FourCC::new(b"UYVY"),
            FourCC::new(b"YUYV"),
            FourCC::new(b"NV12"),
        ];

        // Build resolution list based on config
        let resolutions: Vec<(u32, u32)> = if req_width > 0 && req_height > 0 {
            // User specified resolution - try only that
            vec![(req_width, req_height)]
        } else {
            // Auto: try highest resolutions first
            vec![
                (1920, 1080),
                (1280, 720),
                (720, 576),
                (640, 480),
            ]
        };

        // Try to set resolution with preferred format
        let mut final_format = format.clone();
        let mut found_format = false;

        'resolution: for (target_width, target_height) in &resolutions {
            for preferred_fourcc in &preferred_formats {
                let mut try_format = format.clone();
                try_format.width = *target_width;
                try_format.height = *target_height;
                try_format.fourcc = *preferred_fourcc;

                if let Ok(set_format) = Capture::set_format(&device, &try_format) {
                    // Check if we got what we requested
                    if set_format.width == *target_width && set_format.height == *target_height {
                        final_format = set_format;
                        found_format = true;
                        tracing::info!(
                            "Set format: {}x{} {} (stride: {})",
                            final_format.width,
                            final_format.height,
                            final_format.fourcc,
                            final_format.stride
                        );
                        break 'resolution;
                    }
                }
            }
        }

        if !found_format {
            // Fall back to whatever the device accepts
            tracing::warn!("Could not set preferred format, using driver default");
            final_format = Capture::format(&device)?;
        }

        let width = final_format.width;
        let height = final_format.height;
        let fourcc = final_format.fourcc;
        let stride = final_format.stride;

        // Build frame rate list based on config
        let frame_rates: Vec<(u32, u32)> = if req_fps > 0 {
            // User specified frame rate
            vec![(req_fps, 1)]
        } else {
            // Auto: try highest frame rates first
            vec![
                (60, 1),
                (50, 1),
                (30, 1),
            ]
        };

        for (fps_num, fps_den) in frame_rates {
            let mut params = match Capture::params(&device) {
                Ok(p) => p,
                Err(_) => continue,
            };
            // V4L2 uses frame interval (1/fps), so swap numerator/denominator
            params.interval.numerator = fps_den;
            params.interval.denominator = fps_num;
            if Capture::set_params(&device, &params).is_ok() {
                tracing::info!("Requested frame rate: {} fps", fps_num);
                break;
            }
        }

        // Get actual frame rate from device parameters
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
                    "Active frame rate: {}/{} ({:.2} fps)",
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
