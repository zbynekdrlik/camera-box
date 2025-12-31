use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::capture::{Frame, FrameRate};

// NDI SDK type definitions (minimal subset for video sending and receiving)
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
#[allow(dead_code)]
const NDILIBD_FOURCC_BGRA: u32 = u32::from_le_bytes([b'B', b'G', b'R', b'A']);
#[allow(dead_code)]
const NDILIBD_FOURCC_BGRX: u32 = u32::from_le_bytes([b'B', b'G', b'R', b'X']);

// Frame format types
const NDILIB_FRAME_FORMAT_TYPE_PROGRESSIVE: c_int = 1;

// NDI receiver types
#[repr(C)]
struct NDIlib_find_create_t {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NDIlib_source_t {
    pub p_ndi_name: *const c_char,
    p_url_address: *const c_char,
}

#[repr(C)]
struct NDIlib_recv_create_v3_t {
    source_to_connect_to: NDIlib_source_t,
    color_format: c_int,
    bandwidth: c_int,
    allow_video_fields: bool,
    p_ndi_recv_name: *const c_char,
}

#[repr(C)]
pub struct NDIlib_video_frame_v2_recv_t {
    pub xres: c_int,
    pub yres: c_int,
    pub fourcc: u32,
    pub frame_rate_n: c_int,
    pub frame_rate_d: c_int,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: c_int,
    pub timecode: i64,
    pub p_data: *mut u8,
    pub line_stride_in_bytes: c_int,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

// Frame types returned by recv_capture
#[allow(dead_code)]
const NDILIB_FRAME_TYPE_NONE: c_int = 0;
const NDILIB_FRAME_TYPE_VIDEO: c_int = 1;
#[allow(dead_code)]
const NDILIB_FRAME_TYPE_AUDIO: c_int = 2;
#[allow(dead_code)]
const NDILIB_FRAME_TYPE_METADATA: c_int = 3;
#[allow(dead_code)]
const NDILIB_FRAME_TYPE_ERROR: c_int = 4;

// Color formats
const NDILIB_RECV_COLOR_FORMAT_UYVY_BGRA: c_int = 0;
#[allow(dead_code)]
const NDILIB_RECV_COLOR_FORMAT_BGRX_BGRA: c_int = 1;

// Bandwidth
const NDILIB_RECV_BANDWIDTH_HIGHEST: c_int = 100;

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

// Receiver function types
#[allow(non_camel_case_types)]
type NDIlib_find_create_v2_fn = unsafe extern "C" fn(*const NDIlib_find_create_t) -> *mut c_void;
#[allow(non_camel_case_types)]
type NDIlib_find_destroy_fn = unsafe extern "C" fn(*mut c_void);
#[allow(non_camel_case_types)]
type NDIlib_find_wait_for_sources_fn = unsafe extern "C" fn(*mut c_void, u32) -> bool;
#[allow(non_camel_case_types)]
type NDIlib_find_get_current_sources_fn =
    unsafe extern "C" fn(*mut c_void, *mut u32) -> *const NDIlib_source_t;
#[allow(non_camel_case_types)]
type NDIlib_recv_create_v3_fn = unsafe extern "C" fn(*const NDIlib_recv_create_v3_t) -> *mut c_void;
#[allow(non_camel_case_types)]
type NDIlib_recv_destroy_fn = unsafe extern "C" fn(*mut c_void);
#[allow(non_camel_case_types)]
type NDIlib_recv_capture_v3_fn = unsafe extern "C" fn(
    *mut c_void,
    *mut NDIlib_video_frame_v2_recv_t,
    *mut c_void, // audio frame (null)
    *mut c_void, // metadata frame (null)
    u32,
) -> c_int;
#[allow(non_camel_case_types)]
type NDIlib_recv_free_video_v2_fn =
    unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_recv_t);

/// NDI library wrapper with dynamic loading
struct NdiLib {
    _library: Library,
    destroy: NDIlib_destroy_fn,
    // Sender functions
    send_create: NDIlib_send_create_fn,
    send_destroy: NDIlib_send_destroy_fn,
    send_send_video_v2: NDIlib_send_send_video_v2_fn,
    #[allow(dead_code)] // Keep for potential future async mode
    send_send_video_async_v2: NDIlib_send_send_video_async_v2_fn,
    // Receiver functions
    find_create_v2: NDIlib_find_create_v2_fn,
    find_destroy: NDIlib_find_destroy_fn,
    find_wait_for_sources: NDIlib_find_wait_for_sources_fn,
    find_get_current_sources: NDIlib_find_get_current_sources_fn,
    recv_create_v3: NDIlib_recv_create_v3_fn,
    recv_destroy: NDIlib_recv_destroy_fn,
    recv_capture_v3: NDIlib_recv_capture_v3_fn,
    recv_free_video_v2: NDIlib_recv_free_video_v2_fn,
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

            // Sender functions
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

            // Receiver functions
            let find_create_v2: NDIlib_find_create_v2_fn = *library
                .get::<NDIlib_find_create_v2_fn>(b"NDIlib_find_create_v2")
                .context("NDIlib_find_create_v2 not found")?;
            let find_destroy: NDIlib_find_destroy_fn = *library
                .get::<NDIlib_find_destroy_fn>(b"NDIlib_find_destroy")
                .context("NDIlib_find_destroy not found")?;
            let find_wait_for_sources: NDIlib_find_wait_for_sources_fn = *library
                .get::<NDIlib_find_wait_for_sources_fn>(b"NDIlib_find_wait_for_sources")
                .context("NDIlib_find_wait_for_sources not found")?;
            let find_get_current_sources: NDIlib_find_get_current_sources_fn = *library
                .get::<NDIlib_find_get_current_sources_fn>(b"NDIlib_find_get_current_sources")
                .context("NDIlib_find_get_current_sources not found")?;
            let recv_create_v3: NDIlib_recv_create_v3_fn = *library
                .get::<NDIlib_recv_create_v3_fn>(b"NDIlib_recv_create_v3")
                .context("NDIlib_recv_create_v3 not found")?;
            let recv_destroy: NDIlib_recv_destroy_fn = *library
                .get::<NDIlib_recv_destroy_fn>(b"NDIlib_recv_destroy")
                .context("NDIlib_recv_destroy not found")?;
            let recv_capture_v3: NDIlib_recv_capture_v3_fn = *library
                .get::<NDIlib_recv_capture_v3_fn>(b"NDIlib_recv_capture_v3")
                .context("NDIlib_recv_capture_v3 not found")?;
            let recv_free_video_v2: NDIlib_recv_free_video_v2_fn = *library
                .get::<NDIlib_recv_free_video_v2_fn>(b"NDIlib_recv_free_video_v2")
                .context("NDIlib_recv_free_video_v2 not found")?;

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
                find_create_v2,
                find_destroy,
                find_wait_for_sources,
                find_get_current_sources,
                recv_create_v3,
                recv_destroy,
                recv_capture_v3,
                recv_free_video_v2,
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

// ============================================================================
// NDI Receiver
// ============================================================================

/// Video frame received from NDI source
pub struct ReceivedFrame {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    #[allow(dead_code)]
    pub stride: u32,
    pub data: Vec<u8>,
}

/// NDI receiver wrapper - receives video from an NDI source
pub struct NdiReceiver {
    lib: Arc<NdiLib>,
    receiver: *mut c_void,
    source_name: String,
}

// SAFETY: NdiReceiver uses thread-safe NDI operations
unsafe impl Send for NdiReceiver {}

impl NdiReceiver {
    /// Find and connect to an NDI source by name
    /// Blocks until the source is found (with timeout)
    pub fn connect(source_name: &str, timeout_secs: u32) -> Result<Self> {
        let lib = Arc::new(NdiLib::load()?);

        tracing::info!("Searching for NDI source: {}", source_name);

        // Create finder
        let find_create = NDIlib_find_create_t {
            show_local_sources: true,
            p_groups: ptr::null(),
            p_extra_ips: ptr::null(),
        };

        let finder = unsafe { (lib.find_create_v2)(&find_create) };
        if finder.is_null() {
            anyhow::bail!("Failed to create NDI finder");
        }

        // Search for source with timeout
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs as u64);
        let mut found_source: Option<NDIlib_source_t> = None;

        while start.elapsed() < timeout {
            // Wait for sources (1 second intervals)
            unsafe { (lib.find_wait_for_sources)(finder, 1000) };

            // Get current sources
            let mut num_sources: u32 = 0;
            let sources = unsafe { (lib.find_get_current_sources)(finder, &mut num_sources) };

            if num_sources > 0 && !sources.is_null() {
                for i in 0..num_sources {
                    let source = unsafe { *sources.add(i as usize) };
                    if !source.p_ndi_name.is_null() {
                        let name = unsafe { CStr::from_ptr(source.p_ndi_name) }
                            .to_string_lossy()
                            .to_string();
                        tracing::debug!("Found NDI source: {}", name);

                        if name.contains(source_name) {
                            tracing::info!("Found matching source: {}", name);
                            found_source = Some(source);
                            break;
                        }
                    }
                }
            }

            if found_source.is_some() {
                break;
            }
        }

        let source = match found_source {
            Some(s) => s,
            None => {
                unsafe { (lib.find_destroy)(finder) };
                anyhow::bail!("NDI source '{}' not found within timeout", source_name);
            }
        };

        // Create receiver and connect BEFORE destroying finder (source pointers are owned by finder)
        let recv_name = CString::new("camera-box-display").unwrap();
        let recv_create = NDIlib_recv_create_v3_t {
            source_to_connect_to: source,
            color_format: NDILIB_RECV_COLOR_FORMAT_UYVY_BGRA,
            bandwidth: NDILIB_RECV_BANDWIDTH_HIGHEST,
            allow_video_fields: false,
            p_ndi_recv_name: recv_name.as_ptr(),
        };

        let receiver = unsafe { (lib.recv_create_v3)(&recv_create) };
        if receiver.is_null() {
            // Cleanup finder before error
            unsafe { (lib.find_destroy)(finder) };
            anyhow::bail!("Failed to create NDI receiver");
        }

        // NOW we can cleanup finder - receiver has copied the source info
        unsafe { (lib.find_destroy)(finder) };

        tracing::info!("NDI receiver connected to source");

        Ok(Self {
            lib,
            receiver,
            source_name: source_name.to_string(),
        })
    }

    /// Capture next video frame (blocking with timeout)
    /// Returns None if no frame available within timeout
    pub fn capture_frame(&mut self, timeout_ms: u32) -> Result<Option<ReceivedFrame>> {
        let mut video_frame: NDIlib_video_frame_v2_recv_t = unsafe { std::mem::zeroed() };

        let frame_type = unsafe {
            (self.lib.recv_capture_v3)(
                self.receiver,
                &mut video_frame,
                ptr::null_mut(), // no audio
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        // Debug: log frame type occasionally
        static mut FRAME_TYPE_LOG_COUNT: u64 = 0;
        unsafe {
            FRAME_TYPE_LOG_COUNT += 1;
            if FRAME_TYPE_LOG_COUNT <= 5 || FRAME_TYPE_LOG_COUNT.is_multiple_of(100) {
                tracing::debug!(
                    "NDI recv frame_type={} (0=none, 1=video, 2=audio, 3=meta, 4=error)",
                    frame_type
                );
            }
        }

        if frame_type != NDILIB_FRAME_TYPE_VIDEO {
            return Ok(None);
        }

        // Copy frame data (receiver may reuse buffer)
        let data_size = (video_frame.line_stride_in_bytes * video_frame.yres) as usize;
        let data = if !video_frame.p_data.is_null() && data_size > 0 {
            unsafe { std::slice::from_raw_parts(video_frame.p_data, data_size).to_vec() }
        } else {
            return Ok(None);
        };

        let frame = ReceivedFrame {
            width: video_frame.xres as u32,
            height: video_frame.yres as u32,
            fourcc: video_frame.fourcc,
            stride: video_frame.line_stride_in_bytes as u32,
            data,
        };

        // Free the NDI frame
        unsafe {
            (self.lib.recv_free_video_v2)(self.receiver, &video_frame);
        }

        Ok(Some(frame))
    }

    /// Get source name
    #[allow(dead_code)]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }
}

impl Drop for NdiReceiver {
    fn drop(&mut self) {
        if !self.receiver.is_null() {
            unsafe {
                (self.lib.recv_destroy)(self.receiver);
            }
        }
    }
}

// ============================================================================
// Standalone conversion functions for testing (without NDI library dependency)
// ============================================================================

/// Convert YUYV to UYVY using scalar method (standalone for testing)
/// YUYV: Y0 U0 Y1 V0 -> UYVY: U0 Y0 V0 Y1
pub fn convert_yuyv_to_uyvy_scalar(yuyv: &[u8]) -> Vec<u8> {
    let mut uyvy = Vec::with_capacity(yuyv.len());
    for chunk in yuyv.chunks_exact(4) {
        uyvy.push(chunk[1]); // U0
        uyvy.push(chunk[0]); // Y0
        uyvy.push(chunk[3]); // V0
        uyvy.push(chunk[2]); // Y1
    }
    uyvy
}

/// Convert YUYV to UYVY using AVX2 SIMD (standalone for testing)
///
/// # Safety
/// This function requires AVX2 CPU support. The caller must verify AVX2 is available
/// using `has_avx2()` before calling. Calling on a CPU without AVX2 is undefined behavior.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub unsafe fn convert_yuyv_to_uyvy_avx2(yuyv: &[u8]) -> Vec<u8> {
    let total_bytes = yuyv.len();
    let avx_bytes = (total_bytes / 64) * 64;

    let mut uyvy = vec![0u8; total_bytes];
    let dst = uyvy.as_mut_ptr();

    // Shuffle mask to convert YUYV to UYVY
    let shuffle_mask = _mm256_setr_epi8(
        1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14, 1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10,
        13, 12, 15, 14,
    );

    let mut i = 0;
    while i < avx_bytes {
        let data0 = _mm256_loadu_si256(yuyv.as_ptr().add(i) as *const __m256i);
        let data1 = _mm256_loadu_si256(yuyv.as_ptr().add(i + 32) as *const __m256i);

        let result0 = _mm256_shuffle_epi8(data0, shuffle_mask);
        let result1 = _mm256_shuffle_epi8(data1, shuffle_mask);

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

    uyvy
}

/// Convert NV12 to UYVY (standalone for testing)
pub fn convert_nv12_to_uyvy(nv12: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_size = width * height;
    let mut uyvy = Vec::with_capacity(width * height * 2);

    let y_plane = &nv12[..y_size.min(nv12.len())];
    let uv_plane = if nv12.len() > y_size {
        &nv12[y_size..]
    } else {
        &[]
    };

    for row in 0..height {
        let uv_row = row / 2;
        for col in (0..width).step_by(2) {
            let y0 = y_plane.get(row * width + col).copied().unwrap_or(128);
            let y1 = y_plane.get(row * width + col + 1).copied().unwrap_or(128);
            let uv_idx = uv_row * width + col;
            let u = uv_plane.get(uv_idx).copied().unwrap_or(128);
            let v = uv_plane.get(uv_idx + 1).copied().unwrap_or(128);

            uyvy.push(u);
            uyvy.push(y0);
            uyvy.push(v);
            uyvy.push(y1);
        }
    }

    uyvy
}

/// Convert BGRA to UYVY (standalone for testing)
pub fn convert_bgra_to_uyvy(bgra: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut uyvy = Vec::with_capacity(width * height * 2);

    for row in 0..height {
        for col in (0..width).step_by(2) {
            let idx0 = (row * width + col) * 4;
            let idx1 = (row * width + col + 1) * 4;

            let (b0, g0, r0) = (
                bgra.get(idx0).copied().unwrap_or(0) as i32,
                bgra.get(idx0 + 1).copied().unwrap_or(0) as i32,
                bgra.get(idx0 + 2).copied().unwrap_or(0) as i32,
            );
            let (b1, g1, r1) = (
                bgra.get(idx1).copied().unwrap_or(0) as i32,
                bgra.get(idx1 + 1).copied().unwrap_or(0) as i32,
                bgra.get(idx1 + 2).copied().unwrap_or(0) as i32,
            );

            let y0 = ((66 * r0 + 129 * g0 + 25 * b0 + 128) >> 8) + 16;
            let y1 = ((66 * r1 + 129 * g1 + 25 * b1 + 128) >> 8) + 16;

            let r = (r0 + r1) / 2;
            let g = (g0 + g1) / 2;
            let b = (b0 + b1) / 2;
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;

            uyvy.push(u.clamp(0, 255) as u8);
            uyvy.push(y0.clamp(16, 235) as u8);
            uyvy.push(v.clamp(0, 255) as u8);
            uyvy.push(y1.clamp(16, 235) as u8);
        }
    }

    uyvy
}

/// Check if AVX2 is available (for testing)
#[cfg(target_arch = "x86_64")]
pub fn has_avx2() -> bool {
    is_x86_feature_detected!("avx2")
}

#[cfg(not(target_arch = "x86_64"))]
pub fn has_avx2() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yuyv_to_uyvy_scalar_basic() {
        // YUYV: Y0=10, U=20, Y1=30, V=40
        let yuyv = vec![10, 20, 30, 40];
        let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);

        // Expected UYVY: U=20, Y0=10, V=40, Y1=30
        assert_eq!(uyvy, vec![20, 10, 40, 30]);
    }

    #[test]
    fn test_yuyv_to_uyvy_scalar_multiple_pixels() {
        // Two sets of pixel pairs
        let yuyv = vec![
            10, 20, 30, 40, // First pair
            50, 60, 70, 80, // Second pair
        ];
        let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);

        assert_eq!(uyvy.len(), 8);
        assert_eq!(uyvy[0..4], [20, 10, 40, 30]); // First pair
        assert_eq!(uyvy[4..8], [60, 50, 80, 70]); // Second pair
    }

    #[test]
    fn test_yuyv_to_uyvy_scalar_all_values() {
        // Test with all byte values 0-255 (cycling)
        let yuyv: Vec<u8> = (0..=255).cycle().take(256).collect();
        let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);

        assert_eq!(uyvy.len(), 256);
        // Verify swapping pattern
        for i in (0..256).step_by(4) {
            assert_eq!(uyvy[i], yuyv[i + 1], "U should be from position 1");
            assert_eq!(uyvy[i + 1], yuyv[i], "Y0 should be from position 0");
            assert_eq!(uyvy[i + 2], yuyv[i + 3], "V should be from position 3");
            assert_eq!(uyvy[i + 3], yuyv[i + 2], "Y1 should be from position 2");
        }
    }

    #[test]
    fn test_yuyv_to_uyvy_length_preserved() {
        for size in [4, 8, 64, 256, 1024, 1920 * 2] {
            let yuyv: Vec<u8> = vec![128; size];
            let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);
            assert_eq!(
                uyvy.len(),
                size,
                "Length should be preserved for size {}",
                size
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_yuyv_to_uyvy_avx2_matches_scalar() {
        if !has_avx2() {
            println!("Skipping AVX2 test - CPU doesn't support AVX2");
            return;
        }

        // Test with various sizes including AVX2 chunk boundaries
        for size in [64, 128, 256, 512, 1024, 1920 * 2, 1920 * 1080 * 2] {
            let yuyv: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

            let scalar_result = convert_yuyv_to_uyvy_scalar(&yuyv);
            let avx2_result = unsafe { convert_yuyv_to_uyvy_avx2(&yuyv) };

            assert_eq!(scalar_result, avx2_result, "AVX2 mismatch at size {}", size);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_yuyv_to_uyvy_avx2_non_aligned() {
        if !has_avx2() {
            return;
        }

        // Sizes that don't align with 64-byte AVX2 chunks
        for size in [68, 100, 132, 200] {
            let yuyv: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

            let scalar_result = convert_yuyv_to_uyvy_scalar(&yuyv);
            let avx2_result = unsafe { convert_yuyv_to_uyvy_avx2(&yuyv) };

            assert_eq!(
                scalar_result, avx2_result,
                "AVX2 non-aligned mismatch at size {}",
                size
            );
        }
    }

    #[test]
    fn test_nv12_to_uyvy_basic() {
        // Simple 2x2 NV12 frame
        // Y plane: 4 bytes (2x2)
        // UV plane: 2 bytes (1x2, interleaved)
        let nv12 = vec![
            100, 110, // Y row 0
            120, 130, // Y row 1
            64, 192, // UV (U=64, V=192)
        ];
        let uyvy = convert_nv12_to_uyvy(&nv12, 2, 2);

        assert_eq!(uyvy.len(), 8); // 2x2 * 2 bytes per pixel
                                   // First row: U=64, Y0=100, V=192, Y1=110
        assert_eq!(uyvy[0], 64); // U
        assert_eq!(uyvy[1], 100); // Y0
        assert_eq!(uyvy[2], 192); // V
        assert_eq!(uyvy[3], 110); // Y1
    }

    #[test]
    fn test_nv12_to_uyvy_output_size() {
        // Full HD NV12
        let width = 1920usize;
        let height = 1080usize;
        let y_size = width * height;
        let uv_size = width * height / 2;
        let nv12 = vec![128u8; y_size + uv_size];

        let uyvy = convert_nv12_to_uyvy(&nv12, width, height);
        assert_eq!(uyvy.len(), width * height * 2);
    }

    #[test]
    fn test_bgra_to_uyvy_black() {
        // Black pixel: BGRA = (0, 0, 0, 255)
        let bgra = vec![0, 0, 0, 255, 0, 0, 0, 255]; // 2 black pixels
        let uyvy = convert_bgra_to_uyvy(&bgra, 2, 1);

        assert_eq!(uyvy.len(), 4);
        // Y should be ~16 (video black), U and V should be ~128 (neutral)
        assert_eq!(uyvy[1], 16, "Y0 should be video black (16)");
        assert_eq!(uyvy[3], 16, "Y1 should be video black (16)");
        assert!((uyvy[0] as i32 - 128).abs() < 5, "U should be neutral");
        assert!((uyvy[2] as i32 - 128).abs() < 5, "V should be neutral");
    }

    #[test]
    fn test_bgra_to_uyvy_white() {
        // White pixel: BGRA = (255, 255, 255, 255)
        let bgra = vec![255, 255, 255, 255, 255, 255, 255, 255];
        let uyvy = convert_bgra_to_uyvy(&bgra, 2, 1);

        assert_eq!(uyvy.len(), 4);
        // Y should be 235 (video white)
        assert_eq!(uyvy[1], 235, "Y0 should be video white (235)");
        assert_eq!(uyvy[3], 235, "Y1 should be video white (235)");
    }

    #[test]
    fn test_bgra_to_uyvy_output_size() {
        for (width, height) in [(2, 1), (4, 2), (1920, 1080)] {
            let bgra = vec![128u8; width * height * 4];
            let uyvy = convert_bgra_to_uyvy(&bgra, width, height);
            assert_eq!(uyvy.len(), width * height * 2);
        }
    }

    #[test]
    fn test_detect_avx2() {
        // This just verifies the function works - result depends on CPU
        let result = has_avx2();
        println!("AVX2 support detected: {}", result);
        // No assertion - just ensure it doesn't panic
    }

    #[test]
    fn test_yuyv_to_uyvy_empty() {
        let yuyv: Vec<u8> = vec![];
        let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);
        assert!(uyvy.is_empty());
    }

    #[test]
    fn test_fourcc_constants() {
        assert_eq!(
            NDILIBD_FOURCC_UYVY,
            u32::from_le_bytes([b'U', b'Y', b'V', b'Y'])
        );
        assert_eq!(
            NDILIBD_FOURCC_BGRA,
            u32::from_le_bytes([b'B', b'G', b'R', b'A'])
        );
    }

    #[test]
    fn test_received_frame_construction() {
        let frame = ReceivedFrame {
            width: 1920,
            height: 1080,
            fourcc: NDILIBD_FOURCC_UYVY,
            stride: 3840,
            data: vec![0u8; 1920 * 1080 * 2],
        };
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.stride, 3840);
        assert_eq!(frame.data.len(), 1920 * 1080 * 2);
    }

    #[test]
    fn test_yuyv_to_uyvy_1080p_frame() {
        // Full 1080p frame
        let yuyv = vec![128u8; 1920 * 1080 * 2];
        let uyvy = convert_yuyv_to_uyvy_scalar(&yuyv);
        assert_eq!(uyvy.len(), 1920 * 1080 * 2);
    }
}
