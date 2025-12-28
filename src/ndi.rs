use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;
use std::ptr;

use crate::capture::Frame;

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
type NDIlib_send_send_video_v2_fn = unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t);

/// NDI library wrapper with dynamic loading
struct NdiLib {
    _library: Library,
    destroy: NDIlib_destroy_fn,
    send_create: NDIlib_send_create_fn,
    send_destroy: NDIlib_send_destroy_fn,
    send_send_video_v2: NDIlib_send_send_video_v2_fn,
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
                            return Self::init_from_library(lib)
                                .with_context(|| format!("Failed to initialize NDI from {:?}", lib_path));
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

/// NDI sender wrapper
pub struct NdiSender {
    lib: NdiLib,
    sender: *mut c_void,
    #[allow(dead_code)]
    ndi_name: CString, // Keep CString alive while sender exists
    frame_count: u64,
    uyvy_buffer: Vec<u8>,
}

// SAFETY: NdiSender uses thread-safe NDI operations
unsafe impl Send for NdiSender {}

impl NdiSender {
    /// Create a new NDI sender with source name "usb"
    pub fn new() -> Result<Self> {
        let lib = NdiLib::load()?;

        let ndi_name = CString::new("usb").unwrap();

        let create_settings = NDIlib_send_create_t {
            p_ndi_name: ndi_name.as_ptr(),
            p_groups: ptr::null(),
            clock_video: true,
            clock_audio: false,
        };

        let sender = unsafe { (lib.send_create)(&create_settings) };
        if sender.is_null() {
            anyhow::bail!("Failed to create NDI sender");
        }

        tracing::info!("NDI sender created: usb");

        Ok(Self {
            lib,
            sender,
            ndi_name,
            frame_count: 0,
            uyvy_buffer: Vec::new(),
        })
    }

    /// Send video frame
    pub fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        // Get frame data in UYVY format
        let fourcc_str = frame.fourcc.str()?;
        let data = match fourcc_str.as_ref() {
            "UYVY" => &frame.data,
            "YUYV" => {
                // Convert YUYV to UYVY
                self.uyvy_buffer.clear();
                self.uyvy_buffer.reserve(frame.data.len());
                for chunk in frame.data.chunks_exact(4) {
                    self.uyvy_buffer.push(chunk[1]); // U0
                    self.uyvy_buffer.push(chunk[0]); // Y0
                    self.uyvy_buffer.push(chunk[3]); // V0
                    self.uyvy_buffer.push(chunk[2]); // Y1
                }
                &self.uyvy_buffer
            }
            format => {
                anyhow::bail!("Unsupported video format: {}", format);
            }
        };

        let video_frame = NDIlib_video_frame_v2_t {
            xres: frame.width as c_int,
            yres: frame.height as c_int,
            fourcc: NDILIBD_FOURCC_UYVY,
            frame_rate_n: 30000,
            frame_rate_d: 1001,
            picture_aspect_ratio: 0.0, // Use default
            frame_format_type: NDILIB_FRAME_FORMAT_TYPE_PROGRESSIVE,
            timecode: i64::MAX, // Use current time
            p_data: data.as_ptr(),
            line_stride_in_bytes: frame.stride as c_int,
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        unsafe {
            (self.lib.send_send_video_v2)(self.sender, &video_frame);
        }

        self.frame_count += 1;

        if self.frame_count % 300 == 0 {
            tracing::debug!("Sent {} frames", self.frame_count);
        }

        Ok(())
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
