use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;
use std::ptr;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::capture::{Frame, FrameRate};

// NDI SDK type definitions (minimal subset for video sending)
#[repr(C)]
struct NDIlib_send_create_t {
    p_ndi_name: *const c_char,
    p_groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct NDIlib_video_frame_v2_t {
    xres: c_int,
    yres: c_int,
    fourcc: u32,
    frame_rate_n: c_int,
    frame_rate_d: c_int,
    picture_aspect_ratio: f32,
    frame_format_type: c_int,
    timecode: i64,
    p_data: *const u8,
    line_stride_in_bytes: c_int,
    p_metadata: *const c_char,
    timestamp: i64,
}

// FourCC codes
const NDILIBD_FOURCC_UYVY: u32 = u32::from_le_bytes([b'U', b'Y', b'V', b'Y']);

// Frame format types
const NDILIB_FRAME_FORMAT_TYPE_PROGRESSIVE: c_int = 1;

#[allow(non_camel_case_types)]
type NDIlib_initialize_fn = unsafe extern "C" fn() -> bool;
#[allow(non_camel_case_types)]
type NDIlib_destroy_fn = unsafe extern "C" fn();
#[allow(non_camel_case_types)]
type NDIlib_send_create_fn = unsafe extern "C" fn(*const NDIlib_send_create_t) -> *mut c_void;
#[allow(non_camel_case_types)]
type NDIlib_send_destroy_fn = unsafe extern "C" fn(*mut c_void);
#[allow(non_camel_case_types)]
type NDIlib_send_send_video_v2_fn =
    unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t);
#[allow(non_camel_case_types)]
type NDIlib_send_send_video_async_v2_fn =
    unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t);

/// NDI library wrapper with dynamic loading
struct NdiLib {
    _library: Library,
    destroy: NDIlib_destroy_fn,
    send_create: NDIlib_send_create_fn,
    send_destroy: NDIlib_send_destroy_fn,
    #[allow(dead_code)]
    send_send_video_v2: NDIlib_send_send_video_v2_fn,
    send_send_video_async_v2: NDIlib_send_send_video_async_v2_fn,
}

impl NdiLib {
    fn load() -> Result<Self> {
        // Search paths for NDI library
        let search_paths = [
            // Environment variable paths
            std::env::var("NDI_RUNTIME_DIR_V6").ok(),
            std::env::var("NDI_RUNTIME_DIR_V5").ok(),
            std::env::var("NDI_RUNTIME_DIR").ok(),
            // Standard paths
            Some("/usr/lib/ndi".to_string()),
            Some("/usr/local/lib/ndi".to_string()),
            Some("/opt/ndi/lib".to_string()),
            // Current directory
            Some(".".to_string()),
        ];

        let lib_names = ["libndi.so.6", "libndi.so.5", "libndi.so"];

        let mut last_error = None;

        for path in search_paths.iter().flatten() {
            for lib_name in &lib_names {
                let lib_path = Path::new(path).join(lib_name);
                if lib_path.exists() {
                    tracing::debug!("Trying NDI library: {:?}", lib_path);
                    match unsafe { Library::new(&lib_path) } {
                        Ok(lib) => {
                            return Self::init_from_library(lib).with_context(|| {
                                format!("Failed to initialize NDI from {:?}", lib_path)
                            });
                        }
                        Err(e) => {
                            last_error = Some(e);
                        }
                    }
                }
            }
        }

        // Try system-wide library search
        for lib_name in &lib_names {
            tracing::debug!("Trying system NDI library: {}", lib_name);
            match unsafe { Library::new(*lib_name) } {
                Ok(lib) => {
                    return Self::init_from_library(lib)
                        .context("Failed to initialize NDI from system library");
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .map(|e| anyhow::anyhow!("Failed to load NDI library: {}", e))
            .unwrap_or_else(|| anyhow::anyhow!("NDI library not found")))
    }

    fn init_from_library(library: Library) -> Result<Self> {
        unsafe {
            // Load required symbols and extract raw function pointers immediately
            let initialize: NDIlib_initialize_fn = *library
                .get::<NDIlib_initialize_fn>(b"NDIlib_initialize")
                .context("NDIlib_initialize not found")?;
            let destroy: NDIlib_destroy_fn = *library
                .get::<NDIlib_destroy_fn>(b"NDIlib_destroy")
                .context("NDIlib_destroy not found")?;
            let send_create: NDIlib_send_create_fn = *library
                .get::<NDIlib_send_create_fn>(b"NDIlib_send_create")
                .context("NDIlib_send_create not found")?;
            let send_destroy: NDIlib_send_destroy_fn = *library
                .get::<NDIlib_send_destroy_fn>(b"NDIlib_send_destroy")
                .context("NDIlib_send_destroy not found")?;
            let send_send_video_v2: NDIlib_send_send_video_v2_fn = *library
                .get::<NDIlib_send_send_video_v2_fn>(b"NDIlib_send_send_video_v2")
                .context("NDIlib_send_send_video_v2 not found")?;
            let send_send_video_async_v2: NDIlib_send_send_video_async_v2_fn = *library
                .get::<NDIlib_send_send_video_async_v2_fn>(b"NDIlib_send_send_video_async_v2")
                .context("NDIlib_send_send_video_async_v2 not found")?;

            // Initialize NDI
            if !initialize() {
                anyhow::bail!("NDIlib_initialize failed");
            }

            tracing::info!("NDI library loaded successfully");

            Ok(Self {
                _library: library,
                destroy,
                send_create,
                send_destroy,
                send_send_video_v2,
                send_send_video_async_v2,
            })
        }
    }
}

impl Drop for NdiLib {
    fn drop(&mut self) {
        unsafe {
            (self.destroy)();
        }
    }
}

/// NDI sender wrapper - optimized for low latency
pub struct NdiSender {
    lib: NdiLib,
    sender: *mut c_void,
    #[allow(dead_code)]
    ndi_name: CString, // Keep CString alive while sender exists
    frame_rate: FrameRate,
    frame_count: u64,
    // Single buffer for sync sending (no double buffer needed)
    uyvy_buffer: Vec<u8>,
    // AVX2 support flag
    has_avx2: bool,
}

// SAFETY: NdiSender uses thread-safe NDI operations
unsafe impl Send for NdiSender {}

impl NdiSender {
    /// Create a new NDI sender with the specified source name and frame rate
    pub fn new(name: &str, frame_rate: FrameRate) -> Result<Self> {
        let lib = NdiLib::load()?;

        let ndi_name = CString::new(name).unwrap();

        let create_settings = NDIlib_send_create_t {
            p_ndi_name: ndi_name.as_ptr(),
            p_groups: ptr::null(),
            clock_video: false, // Disable for lowest latency (no frame pacing)
            clock_audio: false,
        };

        let sender = unsafe { (lib.send_create)(&create_settings) };
        if sender.is_null() {
            anyhow::bail!("Failed to create NDI sender");
        }

        // Detect AVX2 support for SIMD optimization
        let has_avx2 = Self::detect_avx2();
        if has_avx2 {
            tracing::info!("NDI sender: AVX2 SIMD enabled for YUYV→UYVY conversion");
        } else {
            tracing::info!("NDI sender: Using scalar YUYV→UYVY conversion");
        }

        tracing::info!(
            "NDI sender created: {} (sync mode, clock_video=false)",
            name
        );

        Ok(Self {
            lib,
            sender,
            ndi_name,
            frame_rate,
            frame_count: 0,
            uyvy_buffer: Vec::with_capacity(1920 * 1080 * 2), // Pre-allocate for 1080p
            has_avx2,
        })
    }

    /// Detect AVX2 CPU support
    #[cfg(target_arch = "x86_64")]
    fn detect_avx2() -> bool {
        is_x86_feature_detected!("avx2")
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn detect_avx2() -> bool {
        false
    }

    // --- Format conversion functions ---

    /// Convert YUYV to UYVY - uses AVX2 SIMD when available
    fn convert_yuyv_to_uyvy(&mut self, yuyv: &[u8]) {
        self.uyvy_buffer.clear();
        self.uyvy_buffer.reserve(yuyv.len());

        #[cfg(target_arch = "x86_64")]
        if self.has_avx2 {
            // SAFETY: We checked for AVX2 support
            unsafe { self.convert_yuyv_to_uyvy_avx2(yuyv) };
            return;
        }

        // Scalar fallback
        self.convert_yuyv_to_uyvy_scalar(yuyv);
    }

    /// Scalar YUYV to UYVY conversion (fallback)
    #[inline]
    fn convert_yuyv_to_uyvy_scalar(&mut self, yuyv: &[u8]) {
        // YUYV: Y0 U0 Y1 V0 -> UYVY: U0 Y0 V0 Y1
        for chunk in yuyv.chunks_exact(4) {
            self.uyvy_buffer.push(chunk[1]); // U0
            self.uyvy_buffer.push(chunk[0]); // Y0
            self.uyvy_buffer.push(chunk[3]); // V0
            self.uyvy_buffer.push(chunk[2]); // Y1
        }
    }

    /// AVX2 SIMD YUYV to UYVY conversion - processes 32 pixels (64 bytes) per iteration
    /// This is ~16x faster than scalar for 1080p frames
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn convert_yuyv_to_uyvy_avx2(&mut self, yuyv: &[u8]) {
        let total_bytes = yuyv.len();
        let avx_bytes = (total_bytes / 64) * 64;

        // Pre-size buffer
        self.uyvy_buffer.resize(total_bytes, 0);
        let dst = self.uyvy_buffer.as_mut_ptr();

        // Shuffle mask to convert YUYV to UYVY
        // YUYV: Y0 U0 Y1 V0 (indices 0,1,2,3) -> UYVY: U0 Y0 V0 Y1 (indices 1,0,3,2)
        let shuffle_mask = _mm256_setr_epi8(
            1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14, 1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11,
            10, 13, 12, 15, 14,
        );

        let mut i = 0;
        while i < avx_bytes {
            // Load 64 bytes (32 pixels in YUYV format)
            let data0 = _mm256_loadu_si256(yuyv.as_ptr().add(i) as *const __m256i);
            let data1 = _mm256_loadu_si256(yuyv.as_ptr().add(i + 32) as *const __m256i);

            // Shuffle to convert YUYV to UYVY
            let result0 = _mm256_shuffle_epi8(data0, shuffle_mask);
            let result1 = _mm256_shuffle_epi8(data1, shuffle_mask);

            // Store results
            _mm256_storeu_si256(dst.add(i) as *mut __m256i, result0);
            _mm256_storeu_si256(dst.add(i + 32) as *mut __m256i, result1);

            i += 64;
        }

        // Handle remaining bytes with scalar code
        while i < total_bytes {
            let y0 = *yuyv.get_unchecked(i);
            let u = *yuyv.get_unchecked(i + 1);
            let y1 = *yuyv.get_unchecked(i + 2);
            let v = *yuyv.get_unchecked(i + 3);

            *dst.add(i) = u;
            *dst.add(i + 1) = y0;
            *dst.add(i + 2) = v;
            *dst.add(i + 3) = y1;

            i += 4;
        }
    }

    fn convert_nv12_to_uyvy(&mut self, nv12: &[u8], width: usize, height: usize) {
        // NV12: Y plane followed by interleaved UV plane
        let y_size = width * height;
        self.uyvy_buffer.clear();
        self.uyvy_buffer.reserve(width * height * 2);

        let y_plane = &nv12[..y_size];
        let uv_plane = &nv12[y_size..];

        for row in 0..height {
            let uv_row = row / 2;
            for col in (0..width).step_by(2) {
                let y0 = y_plane[row * width + col];
                let y1 = y_plane[row * width + col + 1];
                let uv_idx = uv_row * width + col;
                let u = uv_plane.get(uv_idx).copied().unwrap_or(128);
                let v = uv_plane.get(uv_idx + 1).copied().unwrap_or(128);

                // UYVY: U Y0 V Y1
                self.uyvy_buffer.push(u);
                self.uyvy_buffer.push(y0);
                self.uyvy_buffer.push(v);
                self.uyvy_buffer.push(y1);
            }
        }
    }

    fn decode_mjpeg_to_uyvy(&mut self, mjpeg: &[u8], _width: usize, _height: usize) -> Result<()> {
        // Simple MJPEG decoder using system libjpeg via turbojpeg would be ideal,
        // but for simplicity we'll use a pure-Rust approach
        // For now, fail gracefully - full MJPEG support would need additional dependency
        use std::io::Write;
        use std::process::Command;

        // Use ffmpeg as external decoder (commonly available)
        let mut child = Command::new("ffmpeg")
            .args([
                "-f",
                "mjpeg",
                "-i",
                "pipe:0",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "uyvy422",
                "-frames:v",
                "1",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("MJPEG decode requires ffmpeg. Install with: apt install ffmpeg")?;

        {
            let stdin = child.stdin.as_mut().unwrap();
            stdin.write_all(mjpeg)?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            anyhow::bail!("ffmpeg MJPEG decode failed");
        }

        self.uyvy_buffer = output.stdout;
        Ok(())
    }

    fn convert_bgra_to_uyvy(&mut self, bgra: &[u8], width: usize, height: usize) {
        self.uyvy_buffer.clear();
        self.uyvy_buffer.reserve(width * height * 2);

        for row in 0..height {
            for col in (0..width).step_by(2) {
                let idx0 = (row * width + col) * 4;
                let idx1 = (row * width + col + 1) * 4;

                // BGRA to YUV conversion (BT.601)
                let (b0, g0, r0) = (
                    bgra[idx0] as i32,
                    bgra[idx0 + 1] as i32,
                    bgra[idx0 + 2] as i32,
                );
                let (b1, g1, r1) = (
                    bgra.get(idx1).copied().unwrap_or(0) as i32,
                    bgra.get(idx1 + 1).copied().unwrap_or(0) as i32,
                    bgra.get(idx1 + 2).copied().unwrap_or(0) as i32,
                );

                let y0 = ((66 * r0 + 129 * g0 + 25 * b0 + 128) >> 8) + 16;
                let y1 = ((66 * r1 + 129 * g1 + 25 * b1 + 128) >> 8) + 16;

                // Average for U/V
                let r = (r0 + r1) / 2;
                let g = (g0 + g1) / 2;
                let b = (b0 + b1) / 2;
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;

                // UYVY: U Y0 V Y1
                self.uyvy_buffer.push(u.clamp(0, 255) as u8);
                self.uyvy_buffer.push(y0.clamp(16, 235) as u8);
                self.uyvy_buffer.push(v.clamp(0, 255) as u8);
                self.uyvy_buffer.push(y1.clamp(16, 235) as u8);
            }
        }
    }

    /// Send video frame (legacy method with owned data)
    #[allow(dead_code)]
    pub fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        self.send_frame_data(
            &frame.data,
            frame.width,
            frame.height,
            frame.fourcc,
            frame.stride,
        )
    }

    /// Send video frame with zero-copy from buffer slice (FAST PATH)
    /// Uses SYNCHRONOUS send for lowest latency - blocks until NDI accepts frame
    #[inline]
    pub fn send_frame_data(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        fourcc: v4l::FourCC,
        stride: u32,
    ) -> Result<()> {
        let fourcc_str = fourcc.str()?;

        // Convert to UYVY, get stride
        let (uyvy_ptr, uyvy_stride) = match fourcc_str {
            "UYVY" => {
                // Direct passthrough - no conversion needed!
                (data.as_ptr(), stride)
            }
            "YUYV" => {
                self.convert_yuyv_to_uyvy(data);
                (self.uyvy_buffer.as_ptr(), width * 2)
            }
            "NV12" => {
                self.convert_nv12_to_uyvy(data, width as usize, height as usize);
                (self.uyvy_buffer.as_ptr(), width * 2)
            }
            "MJPG" => {
                self.decode_mjpeg_to_uyvy(data, width as usize, height as usize)?;
                (self.uyvy_buffer.as_ptr(), width * 2)
            }
            "BGRA" | "BGR4" | "RX24" => {
                self.convert_bgra_to_uyvy(data, width as usize, height as usize);
                (self.uyvy_buffer.as_ptr(), width * 2)
            }
            format => {
                anyhow::bail!(
                    "Unsupported video format: {}. Supported: UYVY, YUYV, NV12, MJPG, BGRA",
                    format
                );
            }
        };

        let video_frame = NDIlib_video_frame_v2_t {
            xres: width as c_int,
            yres: height as c_int,
            fourcc: NDILIBD_FOURCC_UYVY,
            frame_rate_n: self.frame_rate.numerator as c_int,
            frame_rate_d: self.frame_rate.denominator as c_int,
            picture_aspect_ratio: 0.0, // Use default
            frame_format_type: NDILIB_FRAME_FORMAT_TYPE_PROGRESSIVE,
            timecode: i64::MAX, // Use current time
            p_data: uyvy_ptr,
            line_stride_in_bytes: uyvy_stride as c_int,
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        // SYNCHRONOUS send - blocks until NDI accepts frame (lowest latency)
        unsafe {
            (self.lib.send_send_video_v2)(self.sender, &video_frame);
        }

        self.frame_count += 1;

        if self.frame_count.is_multiple_of(300) {
            tracing::debug!("Sent {} frames", self.frame_count);
        }

        Ok(())
    }

    /// Zero-copy send from FrameInfo (callback-compatible)
    #[inline]
    pub fn send_frame_zero_copy(
        &mut self,
        data: &[u8],
        info: crate::capture::FrameInfo,
    ) -> Result<()> {
        self.send_frame_data(data, info.width, info.height, info.fourcc, info.stride)
    }

    /// Get number of frames sent
    #[allow(dead_code)]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
}

impl Drop for NdiSender {
    fn drop(&mut self) {
        if !self.sender.is_null() {
            unsafe {
                (self.lib.send_destroy)(self.sender);
            }
        }
    }
}
