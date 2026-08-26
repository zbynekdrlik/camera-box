use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::capture::{Frame, FrameRate};

/// 100-nanosecond units per second (genlock boundary math base).
const UNITS_PER_SECOND: i64 = 10_000_000;

/// Get current wall clock time in 100-nanosecond intervals since Unix epoch.
/// This is the format NDI expects for timecodes.
/// Using explicit SystemTime ensures we always get the current time,
/// unlike i64::MAX which uses NDI's cached time from library initialization.
#[inline]
fn get_wall_clock_100ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_nanos() / 100) as i64)
        .unwrap_or(0)
}

/// Pure boundary math: given the current wall-clock time (100ns units) and a
/// frame rate, return the timecode of the next aligned frame boundary. Split
/// out from the sleeping wrapper so the genlock pacing is deterministically
/// unit-testable at any fps (30, 60, ...) without touching the real clock.
///
/// Boundaries are calculated relative to each second to avoid drift. Spacing is
/// `1/fps` seconds, so the rate is parameterized — at 60 fps boundaries fall
/// every 16.667 ms (frame 0 = 0.000 ms, frame 1 = 16.667 ms, ... frame 59 =
/// 983.333 ms); at 30 fps every 33.333 ms.
pub(crate) fn next_boundary_100ns(now_100ns: i64, fps: i64) -> i64 {
    if fps <= 0 {
        return now_100ns;
    }

    // Find the start of the current second
    let current_second_100ns = (now_100ns / UNITS_PER_SECOND) * UNITS_PER_SECOND;
    let offset_in_second = now_100ns - current_second_100ns;

    // Calculate which frame we're in within this second (0 to fps-1)
    // Using multiplication before division to avoid precision loss
    let frame_in_second = (offset_in_second * fps) / UNITS_PER_SECOND;

    // Calculate next frame boundary
    let next_frame_in_second = frame_in_second + 1;
    if next_frame_in_second >= fps {
        // Next frame is at the start of the next second (exactly X:XX:XX.000)
        current_second_100ns + UNITS_PER_SECOND
    } else {
        // Boundary = second_start + (frame_num * UNITS_PER_SECOND / fps)
        // Multiply before divide to maintain precision
        current_second_100ns + (next_frame_in_second * UNITS_PER_SECOND / fps)
    }
}

/// #1009 — the frame boundary AT OR BEFORE `now_100ns` (the floor twin of
/// [`next_boundary_100ns`]): same per-second grid, same multiply-then-divide precision,
/// WITHOUT the `+1` ceil step. STAMPS use this; PACING keeps the ceil twin.
///
/// Why the split matters (the 2026-08-07 overnight −900 ms collapse, issue 1007): a stamp
/// computed as the strictly-NEXT boundary is 0..1 interval in the RECEIVER'S FUTURE at the
/// emit instant by construction, leaving only network delay as margin against the
/// receiver's backward-step guard — a few ms of inter-box clock skew then reads as "frame
/// from the future" in NORMAL operation. The boundary at-or-before the instant preserves
/// the shared grid (two cameras capturing the same instant still stamp identically) while
/// guaranteeing stamps are never future-dated. Sleeping to the NEXT boundary
/// ([`wait_for_next_boundary_100ns`]) is inherently ceil and unaffected — by the time the
/// sleep ends, its boundary is at-or-before "now" anyway.
///
/// Mirror of the DistroAV sender's `genlock_floor_boundary_100ns`
/// (vendor/distroav/src/ndi-output.cpp) — keep both in lock-step.
pub(crate) fn floor_boundary_100ns(now_100ns: i64, fps: i64) -> i64 {
    if fps <= 0 {
        return now_100ns;
    }
    let current_second_100ns = (now_100ns / UNITS_PER_SECOND) * UNITS_PER_SECOND;
    let offset_in_second = now_100ns - current_second_100ns;
    // Which frame slot the instant falls in (0..fps-1); its own boundary is at-or-before.
    let mut frame_in_second = (offset_in_second * fps) / UNITS_PER_SECOND;
    // #1009 review fix: a boundary b_k = floor(k*UNITS/fps) can sit up to one unit BELOW
    // the exact rational k*UNITS/fps (b_1 @30fps = 333_333, not 333_333.33), so the slot
    // recovery above under-counts by one for an instant exactly ON such a boundary —
    // promote when the NEXT slot's boundary is still at-or-before the instant (the
    // under-count is provably at most one slot, so a single promotion suffices).
    let next_slot_boundary = ((frame_in_second + 1) * UNITS_PER_SECOND) / fps;
    if next_slot_boundary <= offset_in_second {
        frame_in_second += 1;
    }
    // Multiply before divide to maintain precision (same as the ceil twin).
    current_second_100ns + (frame_in_second * UNITS_PER_SECOND / fps)
}

/// Block until the next aligned frame boundary and return its timecode. All
/// cameras with NTP/PTP-synchronized clocks send frames at the same wall-clock
/// boundaries, enabling software genlock across devices. Pure boundary math is
/// in [`next_boundary_100ns`]; this wrapper only adds the real-clock sleep.
/// (The returned stamp is at-or-before "now" by the time the sleep ends — a
/// slept-to boundary never future-stamps, so this pacing path is NOT part of
/// the issue-1009 ceil-bias defect.)
#[inline]
fn wait_for_next_boundary_100ns(fps: i64) -> i64 {
    let now_100ns = get_wall_clock_100ns();
    // next_boundary_100ns already guards fps <= 0 (returns now -> zero wait).
    let next_boundary = next_boundary_100ns(now_100ns, fps);

    // Sleep until next boundary. A genlock wait is always < one frame interval,
    // so clamp to one second: a clock jump (or a bad boundary) must never park
    // the send thread for an unbounded time.
    let wait_100ns = (next_boundary - now_100ns).clamp(0, UNITS_PER_SECOND);
    if wait_100ns > 0 {
        let wait_duration = std::time::Duration::from_nanos((wait_100ns * 100) as u64);
        std::thread::sleep(wait_duration);
    }

    next_boundary
}

/// Derive the integer genlock pacing rate (frames per second) from a rational
/// NDI frame rate (`numerator/denominator`). NDI advertises the exact rational
/// (e.g. NTSC 59.94 = 60000/1001, 29.97 = 30000/1001) but the boundary math in
/// [`next_boundary_100ns`] paces on a whole-number fps. Rounding — not
/// truncating — keeps a non-integer source on its nearest whole-frame cadence
/// (59.94 -> 60, 29.97 -> 30) instead of silently dropping to 59 / 29, which
/// would drift the genlock cadence against the advertised rate. `numerator` /
/// `denominator` are the `u32` `FrameRate` fields; a zero denominator yields 0,
/// which [`next_boundary_100ns`] treats as a genlock no-op (never divides by
/// zero).
fn fps_from_frame_rate(numerator: u32, denominator: u32) -> i64 {
    if denominator == 0 {
        return 0;
    }
    (numerator as f64 / denominator as f64).round() as i64
}

/// #275b — the genlock emit-boundary NDI timecode for the CURRENT wall clock at `fps`, computed
/// WITHOUT blocking (the non-sleeping twin of [`wait_for_next_boundary_100ns`]). The async
/// cam1-burn pipeline computes this ON THE CAPTURE THREAD at the genlock emit-gate instant and
/// carries it to the burn thread's send (via [`NdiSender::send_frame_data_with_timecode`]), so
/// the stamped timecode is the EMITTED frame's gate boundary — immune to the burn-thread queue
/// jitter that moving the send off the capture thread would otherwise inject into the genlock
/// pacing. `fps` is the integer genlock rate; 0 yields the raw wall clock (a genlock no-op,
/// matching [`floor_boundary_100ns`]). #1009: the stamp is the boundary AT-OR-BEFORE the gate
/// instant — the gate just passed its boundary, so floor names THAT boundary; the old ceil
/// dated the frame one interval into the receiver's future.
pub fn boundary_timecode_100ns(fps: u32) -> i64 {
    floor_boundary_100ns(get_wall_clock_100ns(), fps as i64)
}

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
const NDILIBD_FOURCC_UYVY: u32 = u32::from_le_bytes(*b"UYVY");
#[allow(dead_code)]
const NDILIBD_FOURCC_BGRA: u32 = u32::from_le_bytes(*b"BGRA");
#[allow(dead_code)]
const NDILIBD_FOURCC_BGRX: u32 = u32::from_le_bytes(*b"BGRX");

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

// Color formats — values per vendor/distroav/lib/ndi/Processing.NDI.Recv.h: BGRX_BGRA = 0,
// UYVY_BGRA = 1 (the two names were historically swapped here; value 0 = BGRX/BGRA is what
// this receiver has always requested and handles — label-only fix, behavior unchanged).
const NDILIB_RECV_COLOR_FORMAT_BGRX_BGRA: c_int = 0;
#[allow(dead_code)]
const NDILIB_RECV_COLOR_FORMAT_UYVY_BGRA: c_int = 1;

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
#[repr(C)]
struct NDIlib_metadata_frame_t {
    length: c_int,
    timecode: i64,
    p_data: *const c_char,
}
#[allow(non_camel_case_types)]
type NDIlib_recv_send_metadata_fn =
    unsafe extern "C" fn(*mut c_void, *const NDIlib_metadata_frame_t) -> bool;
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
    recv_send_metadata: NDIlib_recv_send_metadata_fn,
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
            let recv_send_metadata: NDIlib_recv_send_metadata_fn = *library
                .get::<NDIlib_recv_send_metadata_fn>(b"NDIlib_recv_send_metadata")
                .context("NDIlib_recv_send_metadata not found")?;

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
                recv_send_metadata,
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

/// #317 — test seam at the NDI sender FFI boundary.
///
/// `NdiLib` embeds a live `libloading::Library`, so an `NdiSender` cannot be constructed in a
/// unit test without loading a real NDI `.so`. This trait abstracts the ONLY two FFI calls the
/// re-announce dance makes — `NDIlib_send_create` and `NDIlib_send_destroy` — so
/// [`reannounce_dance`] can be driven in a test with a FAKE function table that records call
/// ORDER. The production impl ([`NdiLib`]) is a thin, inlined pass-through to the loaded raw
/// function pointers, so the live path stays byte-identical to the hand-written calls it
/// replaces.
///
/// The methods are safe to CALL: the single caller ([`reannounce_dance`]) upholds the FFI
/// preconditions (a non-null, not-yet-destroyed handle for `send_destroy`; a `settings` whose
/// `p_ndi_name` outlives the call for `send_create`), and each impl encapsulates the `unsafe`
/// it needs.
trait NdiSendOps {
    /// Create a same-name NDI sender; returns the new handle, or null on failure — notably the
    /// SDK refusing a SECOND sender whose name is already live (the create-first trap of #297).
    fn send_create(&self, settings: &NDIlib_send_create_t) -> *mut c_void;
    /// Destroy an NDI sender handle. Never called with null by [`reannounce_dance`].
    fn send_destroy(&self, sender: *mut c_void);
}

impl NdiSendOps for NdiLib {
    #[inline]
    fn send_create(&self, settings: &NDIlib_send_create_t) -> *mut c_void {
        // SAFETY: thin wrapper over the loaded NDIlib_send_create fn pointer; `settings` (and the
        // `p_ndi_name` it borrows) is owned by the caller and valid for this call's duration.
        unsafe { (self.send_create)(settings) }
    }
    #[inline]
    fn send_destroy(&self, sender: *mut c_void) {
        // SAFETY: thin wrapper over the loaded NDIlib_send_destroy fn pointer; the caller passes
        // only a non-null handle previously returned by `send_create`.
        unsafe { (self.send_destroy)(sender) }
    }
}

/// #297/#317 — the re-announce dance, parameterized over [`NdiSendOps`] so the load-bearing
/// destroy-BEFORE-create ordering AND the null-handle safety are unit-testable with a fake
/// function table (no real NDI / no `libloading::Library`).
///
/// DESTROY the old handle FIRST: the NDI SDK refuses a same-name create while the old sender is
/// still live, so create-first ALWAYS returns null (the #297 infinite re-announce loop). Then
/// create the fresh same-name sender. The `sender` slot is nulled in the same step as the
/// destroy, so a failed create leaves a valid NULL handle (guarded by the emit path) rather than
/// a dangling/destroyed pointer; a RETRY after a prior failed create has an already-null slot,
/// and `send_destroy(null)` is the case [`Drop`] avoids — so the null is guarded here.
///
/// On SUCCESS `*sender` becomes the new live handle and `trigger` advances to `current` (a stable
/// poll then no longer fires — no loop). On a null create `*sender` stays NULL and `trigger` is
/// left UNCHANGED so the next poll RETRIES. Returns whether the create succeeded.
fn reannounce_dance<O: NdiSendOps>(
    ops: &O,
    sender: &mut *mut c_void,
    settings: &NDIlib_send_create_t,
    trigger: &mut crate::reannounce::ReannounceState,
    current: crate::reannounce::NetworkSignature,
) -> bool {
    let old = std::mem::replace(sender, ptr::null_mut());
    if !old.is_null() {
        ops.send_destroy(old);
    }
    let new_sender = ops.send_create(settings);
    let created_ok = !new_sender.is_null();
    // Advance the trigger ONLY on success; on failure it stays put so the next poll retries.
    trigger.record_reannounce_attempt(current, created_ok);
    if created_ok {
        *sender = new_sender;
    }
    created_ok
}

/// #297 — read the host's current usable (up, non-loopback, IPv4) network addresses into a
/// canonical [`crate::reannounce::NetworkSignature`]. This is the Linux IO half of the
/// re-announce trigger (the pure decision lives in `crate::reannounce`). A `getifaddrs`
/// failure yields an EMPTY signature, which [`crate::reannounce::should_reannounce`] treats as
/// "network down → do nothing" — so a transient read error never spuriously re-creates the
/// sender. IPv4 only: NDI discovery on this LAN is IPv4 mDNS.
fn current_network_signature() -> crate::reannounce::NetworkSignature {
    let mut addrs: Vec<String> = Vec::new();
    // SAFETY: getifaddrs allocates a linked list we free with freeifaddrs; every pointer is
    // null-checked before deref and we never retain a pointer past freeifaddrs.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 || ifap.is_null() {
            tracing::warn!("#297 getifaddrs failed; treating network as down this cycle");
            return crate::reannounce::NetworkSignature::default();
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() {
                continue;
            }
            if (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue; // IPv4 only
            }
            let flags = ifa.ifa_flags as i32;
            if flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 {
                continue; // skip down + loopback interfaces
            }
            // Keep only the real LAN NIC(s); drop docker/virbr/veth/tailscale/tun/… so an
            // unrelated virtual interface flapping can't trigger a re-announce (#297 review).
            if !ifa.ifa_name.is_null() {
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy();
                if !crate::reannounce::is_discoverable_interface(&name) {
                    continue;
                }
            }
            let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
            // s_addr is in network byte order; from_be → host order, then Ipv4Addr renders the
            // dotted-quad (idiomatic, no hand-rolled shift/mask to re-verify for endianness).
            let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            addrs.push(ip.to_string());
        }
        libc::freeifaddrs(ifap);
    }
    crate::reannounce::NetworkSignature::from_addrs(addrs)
}

pub struct NdiSender {
    lib: NdiLib,
    sender: *mut c_void,
    ndi_name: CString, // Keep CString alive while sender exists; reused on re-announce (#297)
    frame_rate: FrameRate,
    frame_count: u64,
    // #297 — re-announce trigger state: the usable-network signature the sender was last
    // announced on + whether the network has been seen down since. A change (an address appeared
    // / flapped / recovered) re-registers the sender so the OBS NDI finder rediscovers it. The
    // convergence + retry contract is unit-tested in `crate::reannounce`.
    reannounce: crate::reannounce::ReannounceState,
    last_reannounce_check: std::time::Instant,
    // Single buffer for sync sending (no double buffer needed)
    uyvy_buffer: Vec<u8>,
    // AVX2 support flag
    has_avx2: bool,
    // Genlock #11: when true, the caller paces the sends (decimating the capture
    // to the target rate on wall-clock boundaries), so send_frame stamps the
    // boundary timecode WITHOUT the internal blocking wait — blocking here would
    // back-pressure the faster capture loop and pile up V4L2 buffers.
    external_pacing: bool,
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
            // #297 — seed the trigger with the network the sender was just announced on; the
            // capture loop calls `maybe_reannounce()` and re-registers if this set later changes.
            // When the network is already up at creation this is the live signature, so a clean
            // boot does NOT trigger a spurious first re-announce; during a boot race it is empty
            // and the first poll with a real address fires.
            reannounce: crate::reannounce::ReannounceState::new(current_network_signature()),
            last_reannounce_check: std::time::Instant::now(),
            uyvy_buffer: Vec::with_capacity(1920 * 1080 * 2), // Pre-allocate for 1080p
            has_avx2,
            external_pacing: false,
        })
    }

    /// Genlock #11: enable external pacing — the caller decimates the capture to
    /// the target rate on wall-clock boundaries and `send_frame` stamps the
    /// boundary timecode without blocking. Use when the sender's `frame_rate` is
    /// the genlock/broadcast rate (e.g. 30) but capture runs faster (e.g. 60).
    ///
    /// INVARIANT (caller-enforced): `frame_rate` MUST already equal the emit rate
    /// the caller decimates to before this is enabled. Enabling it while
    /// `frame_rate` still reflects the capture rate stamps wrong timecodes on
    /// every frame. The single caller (`main.rs`) sets the genlock rate and this
    /// flag together; any new caller must uphold the same ordering.
    pub fn set_external_pacing(&mut self, enabled: bool) {
        self.external_pacing = enabled;
        tracing::info!(
            "NDI sender: external pacing {} (genlock decimation by caller)",
            if enabled { "ENABLED" } else { "disabled" }
        );
    }

    /// #297 — re-announce the NDI sender if the host's usable network changed since it was
    /// last announced, so the OBS/DistroAV NDI finder rediscovers a box whose network came up
    /// after start (boot race) or flapped to a new address. Throttled to
    /// [`crate::reannounce::REANNOUNCE_POLL_INTERVAL`]; call it freely from the capture loop.
    ///
    /// Returns `Ok(true)` if it re-created (re-registered) the sender, `Ok(false)` if nothing
    /// was needed. A re-create briefly drops any connected receiver — but that only happens on
    /// a real network change (already a disruption), NEVER in steady state, because
    /// [`crate::reannounce::should_reannounce`] returns false for an unchanged address set.
    pub fn maybe_reannounce(&mut self) -> Result<bool> {
        // The poll interval is the deliberate sampling window: a network state shorter than it
        // (a sub-2s flap that returns the same IP between two polls) is not observed — accepted
        // for a LAN appliance where real changes are seconds-scale. A genuinely flapping NIC
        // re-creates the sender at most once per interval (no tighter hysteresis is needed).
        if self.last_reannounce_check.elapsed() < crate::reannounce::REANNOUNCE_POLL_INTERVAL {
            return Ok(false);
        }
        self.last_reannounce_check = std::time::Instant::now();
        let current = current_network_signature();
        let reannounce = self.reannounce.should_reannounce(&current);
        if current.is_empty() {
            // Network down — remember the outage so the recovery re-announces even if the same
            // address returns; nothing to announce on right now.
            self.reannounce.mark_down();
        }
        if !reannounce {
            return Ok(false);
        }
        tracing::warn!(
            "#297 NDI sender '{}' re-announce: network changed {:?} -> {:?} (saw_down={}), re-registering",
            self.ndi_name.to_string_lossy(),
            self.reannounce.announced().addrs(),
            current.addrs(),
            self.reannounce.saw_down()
        );
        self.reannounce_now(current)
    }

    /// Re-register the NDI sender on the CURRENT network: DESTROY the old handle FIRST, then
    /// create a fresh sender with the same name + settings.
    ///
    /// The order is load-bearing (#297): the NDI SDK refuses to register a SECOND sender whose
    /// name is already live in this process, so a same-name `NDIlib_send_create` while the old
    /// handle still exists ALWAYS returns null. The shipped dev.139 created-first, so re-announce
    /// could never succeed — it bailed every 2s without ever advancing the trigger (infinite
    /// WARN loop, box never rediscovered). The brief same-name gap created by destroying first is
    /// acceptable: re-announce only fires on a real network change, which already disrupted the
    /// feed.
    ///
    /// On the now-genuinely-rare null create AFTER the destroy, the sender is left NULL: the emit
    /// path skips a null handle (a frame is dropped, not UB), the trigger state is deliberately
    /// left unchanged (via `record_reannounce_attempt(_, false)`) so the next poll RETRIES the
    /// create, and this returns Err for the caller to log.
    fn reannounce_now(&mut self, current: crate::reannounce::NetworkSignature) -> Result<bool> {
        let create_settings = NDIlib_send_create_t {
            p_ndi_name: self.ndi_name.as_ptr(),
            p_groups: ptr::null(),
            clock_video: false, // match new(): no internal pacing
            clock_audio: false,
        };
        // #297/#317 — destroy-FIRST then same-name create, via the test-seam dance so the
        // load-bearing ordering + null-handle safety are locked by unit tests (no real NDI). On
        // success the dance has already swapped `self.sender` to the new handle and advanced the
        // trigger; on failure it left `self.sender` NULL and the trigger unchanged (retry next).
        let created_ok = reannounce_dance(
            &self.lib,
            &mut self.sender,
            &create_settings,
            &mut self.reannounce,
            current,
        );
        if !created_ok {
            anyhow::bail!(
                "#297 re-announce: NDIlib_send_create returned null after destroy; sender absent, retrying next poll"
            );
        }
        tracing::info!(
            "#297 NDI sender '{}' re-announced on {:?}",
            self.ndi_name.to_string_lossy(),
            self.reannounce.announced().addrs()
        );
        Ok(true)
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
        for chunk in yuyv.as_chunks::<4>().0 {
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
    /// Uses SYNCHRONOUS send for lowest latency - blocks until NDI accepts frame.
    ///
    /// Computes the NDI timecode itself (boundary timecode under external pacing, else a blocking
    /// wait to the boundary) then delegates to [`send_frame_data_with_timecode`]. The async
    /// cam1-burn thread (#275b) calls that variant directly with a gate-stamped timecode instead.
    #[inline]
    pub fn send_frame_data(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        fourcc: v4l::FourCC,
        stride: u32,
    ) -> Result<()> {
        let fps = fps_from_frame_rate(self.frame_rate.numerator, self.frame_rate.denominator);
        let timecode = if self.external_pacing {
            // Caller already gated this send to a wall-clock boundary; stamp it without
            // sleeping. #1009: FLOOR — the boundary the gate just passed is at-or-before
            // "now"; the old ceil stamp here dated the frame one interval into the future.
            floor_boundary_100ns(get_wall_clock_100ns(), fps)
        } else {
            wait_for_next_boundary_100ns(fps)
        };
        self.send_frame_data_with_timecode(data, width, height, fourcc, stride, timecode)
    }

    /// #275b — send a video frame with a CALLER-SUPPLIED NDI `timecode_100ns` (100ns since
    /// epoch). The async cam1-burn thread uses this so the stamped timecode is the EMITTED
    /// frame's genlock boundary computed on the capture thread at the gate instant
    /// ([`boundary_timecode_100ns`]) — NOT a value re-derived later on the burn thread (which
    /// would inject queue jitter into the genlock pacing). [`send_frame_data`] is the
    /// timecode-computing wrapper for the normal capture-thread path.
    #[inline]
    pub fn send_frame_data_with_timecode(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        fourcc: v4l::FourCC,
        stride: u32,
        timecode_100ns: i64,
    ) -> Result<()> {
        // #297 — the sender is null only in the rare window after a re-announce destroyed the old
        // handle and the same-name re-create returned null; `maybe_reannounce` retries the create
        // on the next poll (≤ REANNOUNCE_POLL_INTERVAL). Calling NDIlib_send_send_video_v2 with a
        // null handle is UB — drop this frame instead. Bounded, self-healing, and only reachable
        // after a genuine network change plus a create failure, so no per-frame log spam.
        if self.sender.is_null() {
            return Ok(());
        }
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
            // The boundary/paced timecode resolved by the caller (or by send_frame_data's
            // wrapper above) — stamped verbatim so the async cam1-burn (#275b) preserves the
            // EMITTED frame's gate timecode.
            timecode: timecode_100ns,
            p_data: uyvy_ptr,
            line_stride_in_bytes: uyvy_stride as c_int,
            p_metadata: ptr::null(),
            timestamp: 0,
        };

        // SYNCHRONOUS send - blocks until NDI accepts frame (lowest latency)
        //
        // #707 — time this exact blocking call. The #656/#663/#665/#666 emit-rate-deficit family
        // has only ever measured the downstream 5s-averaged symptom (emitted fps < configured
        // send fps); this pinpoints, per-call, whether the SDK's own blocking send is where the
        // time actually goes (network/receiver backpressure) — see `crate::send_stall`.
        let send_started = std::time::Instant::now();
        unsafe {
            (self.lib.send_send_video_v2)(self.sender, &video_frame);
        }
        let send_duration_ms = send_started.elapsed().as_secs_f64() * 1000.0;
        let configured_fps =
            fps_from_frame_rate(self.frame_rate.numerator, self.frame_rate.denominator) as f64;
        if configured_fps > 0.0 {
            let frame_interval_ms = 1000.0 / configured_fps;
            if crate::send_stall::is_send_stall(send_duration_ms, frame_interval_ms) {
                tracing::warn!(
                    "{}",
                    crate::send_stall::send_stall_warning(
                        &self.ndi_name.to_string_lossy(),
                        send_duration_ms,
                        frame_interval_ms,
                        configured_fps,
                    )
                );
            }
        }

        self.frame_count += 1;

        if self.frame_count.is_multiple_of(300) {
            tracing::debug!("Sent {} frames", self.frame_count);
        }

        Ok(())
    }

    /// Zero-copy send from FrameInfo (callback-compatible).
    ///
    /// #286 — when `external_pacing` (genlock) is ON, stamps the caller-supplied
    /// `capture_timecode_100ns` (the frame's real CAPTURE-instant boundary, from
    /// [`crate::genlock_stamp::genlock_emit_timecode_100ns`]) instead of re-deriving an
    /// ARRIVAL-based boundary from the current wall clock at send time — so a grabber
    /// card's photon->dequeue latency `d_X` can no longer leak into the emitted genlock
    /// timecode. When `external_pacing` is OFF, falls back to the EXACT same self-paced
    /// blocking wait as [`Self::send_frame_data`] — that path paces off arrival/wall-clock
    /// by design (no genlock decimation cadence to align a capture instant to) and is
    /// deliberately unaffected by #286.
    #[inline]
    pub fn send_frame_zero_copy(
        &mut self,
        data: &[u8],
        info: crate::capture::FrameInfo,
        capture_timecode_100ns: i64,
    ) -> Result<()> {
        let timecode = if self.external_pacing {
            capture_timecode_100ns
        } else {
            let fps = fps_from_frame_rate(self.frame_rate.numerator, self.frame_rate.denominator);
            wait_for_next_boundary_100ns(fps)
        };
        self.send_frame_data_with_timecode(
            data,
            info.width,
            info.height,
            info.fourcc,
            info.stride,
            timecode,
        )
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
    /// The NDI frame `timecode`, in 100ns units since the Unix epoch, as stamped
    /// by the EMITTING node (camera-box sender stamps the DanteSync wall clock at
    /// the genlock boundary; OBS/DistroAV re-emit regenerates it from the emitting
    /// OBS node's clock). With the whole cluster DanteSync-locked sub-ms, this is a
    /// per-node EMIT time on a shared clock — the basis for exact per-hop latency
    /// (the probe taps read it to compute `downstream_emit − upstream_emit`).
    /// May be 0 or the SDK sentinel on sources that do not stamp a real timecode.
    pub timecode_100ns: i64,
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
            color_format: NDILIB_RECV_COLOR_FORMAT_BGRX_BGRA,
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
    /// #797 — send receiver metadata (e.g. the DistroAV hw-accel request
    /// `<ndi_video_codec type="hardware"/>`), verbatim what the vendored ndi-source does.
    pub fn send_metadata(&mut self, xml: &str) -> Result<()> {
        let c = CString::new(xml)?;
        let frame = NDIlib_metadata_frame_t {
            length: xml.len() as c_int,
            timecode: 0,
            p_data: c.as_ptr(),
        };
        unsafe { (self.lib.recv_send_metadata)(self.receiver, &frame) };
        Ok(())
    }

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
            // Per-node EMIT time stamped by the source (100ns since epoch). Read
            // BEFORE recv_free_video_v2 frees the frame below.
            timecode_100ns: video_frame.timecode,
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
    for chunk in yuyv.as_chunks::<4>().0 {
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
    fn boundary_60fps_from_second_start() {
        // At a clean second start, the next 60fps boundary is frame 1 = 1/60 s.
        let b = next_boundary_100ns(0, 60);
        assert_eq!(b, UNITS_PER_SECOND / 60); // 166_666
    }

    #[test]
    fn boundary_60fps_mid_frame0_targets_frame1() {
        // 1 unit into frame 0 still targets frame 1.
        let b = next_boundary_100ns(1, 60);
        assert_eq!(b, UNITS_PER_SECOND / 60);
    }

    #[test]
    fn boundary_60fps_last_frame_wraps_to_next_second() {
        // Inside frame 59 (>= 59/60 s), the next boundary is the next second.
        let now = 9_900_000; // 0.99 s, within frame 59
        let b = next_boundary_100ns(now, 60);
        assert_eq!(b, UNITS_PER_SECOND);
    }

    #[test]
    fn boundary_30fps_still_correct() {
        // 60fps must not break the legacy 30fps pacing.
        assert_eq!(next_boundary_100ns(0, 30), UNITS_PER_SECOND / 30); // 333_333
    }

    /// #1009 — the floor twin, unit-tested directly (review finding: coverage was only
    /// indirect through genlock_stamp). The exact-boundary case is the subtle one:
    /// b_1 @30fps = 333_333 (the integer floor of 10^7/30 sits just UNDER the exact
    /// rational), so a naive slot recovery (off*fps/UNITS) under-counts by one for an
    /// instant exactly ON such a boundary and wrongly returns the PREVIOUS boundary.
    #[test]
    fn floor_boundary_is_identity_on_an_exact_boundary_1009() {
        assert_eq!(floor_boundary_100ns(0, 30), 0);
        assert_eq!(floor_boundary_100ns(333_333, 30), 333_333); // exactly ON b_1
        assert_eq!(floor_boundary_100ns(333_332, 30), 0); // just under b_1
        assert_eq!(floor_boundary_100ns(333_334, 30), 333_333); // just over b_1
        assert_eq!(floor_boundary_100ns(UNITS_PER_SECOND, 30), UNITS_PER_SECOND);
        // Last slot of the second floors to b_29, never wraps forward.
        assert_eq!(
            floor_boundary_100ns(9_999_999, 30),
            (29 * UNITS_PER_SECOND) / 30
        );
    }

    /// #1009 — floor guards degenerate fps and never stamps the future or falls a full
    /// interval behind, at both rig rates.
    #[test]
    fn floor_boundary_bounds_and_degenerate_fps_1009() {
        assert_eq!(floor_boundary_100ns(12_345, 0), 12_345);
        assert_eq!(floor_boundary_100ns(12_345, -5), 12_345);
        for fps in [30i64, 60] {
            let step = UNITS_PER_SECOND / fps;
            let mut off = 0i64;
            while off < UNITS_PER_SECOND {
                let b = floor_boundary_100ns(off, fps);
                assert!(
                    b <= off && off - b <= step,
                    "fps {fps} off {off}: floor boundary {b} out of [off-interval, off]"
                );
                off += 97_531;
            }
        }
    }

    #[test]
    fn boundary_60fps_is_denser_than_30fps() {
        // 60fps boundaries are ~half the spacing of 30fps (twice the density).
        let s60 = next_boundary_100ns(0, 60);
        let s30 = next_boundary_100ns(0, 30);
        assert!(s60 < s30);
        assert!((s30 - 2 * s60).abs() <= 2, "s30={s30} s60={s60}");
    }

    #[test]
    fn boundary_zero_fps_is_noop() {
        assert_eq!(next_boundary_100ns(12_345, 0), 12_345);
    }

    #[test]
    fn fps_from_frame_rate_exact_60() {
        assert_eq!(fps_from_frame_rate(60, 1), 60);
    }

    #[test]
    fn fps_from_frame_rate_exact_30() {
        assert_eq!(fps_from_frame_rate(30, 1), 30);
    }

    #[test]
    fn fps_from_frame_rate_ntsc_5994_rounds_to_60() {
        // NTSC 59.94 = 60000/1001. Integer truncation gives 59 (silent genlock
        // drift vs the advertised rational); the nearest whole frame is 60.
        assert_eq!(fps_from_frame_rate(60000, 1001), 60);
    }

    #[test]
    fn fps_from_frame_rate_ntsc_2997_rounds_to_30() {
        // NTSC 29.97 = 30000/1001. Truncation gives 29; nearest whole frame 30.
        assert_eq!(fps_from_frame_rate(30000, 1001), 30);
    }

    #[test]
    fn fps_from_frame_rate_zero_denominator_is_noop() {
        // A bad negotiation must never divide by zero -> 0 (genlock no-op).
        assert_eq!(fps_from_frame_rate(60, 0), 0);
    }

    #[test]
    fn wait_for_next_boundary_returns_real_recent_boundary() {
        // Exercises the sleeping wrapper: it must sleep until and return a real
        // wall-clock boundary timecode (not 0/1/-1, not a future value).
        let b = wait_for_next_boundary_100ns(60);
        let now = get_wall_clock_100ns();
        // A real 100ns-since-epoch timecode is huge -> kills ->0 / ->1 / ->-1.
        assert!(b > 1_000_000, "boundary must be a real timecode, got {b}");
        // We slept until the boundary, so it is now-or-just-past, never future.
        assert!(
            b <= now,
            "boundary {b} must not be in the future (now {now})"
        );
        // ...and within ~one 60fps frame of now (kills skip-the-sleep mutants).
        assert!(
            now - b < UNITS_PER_SECOND / 60 + 100_000,
            "boundary {b} too stale vs now {now}"
        );
    }

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
        assert_eq!(NDILIBD_FOURCC_UYVY, u32::from_le_bytes(*b"UYVY"));
        assert_eq!(NDILIBD_FOURCC_BGRA, u32::from_le_bytes(*b"BGRA"));
    }

    #[test]
    fn test_received_frame_construction() {
        let frame = ReceivedFrame {
            width: 1920,
            height: 1080,
            fourcc: NDILIBD_FOURCC_UYVY,
            stride: 3840,
            data: vec![0u8; 1920 * 1080 * 2],
            timecode_100ns: 0,
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

#[cfg(test)]
mod ffi_seam_tests {
    //! #317 — lock the NDI sender re-announce FFI ORDERING (destroy-before-create) by driving
    //! [`reannounce_dance`] with a FAKE function table. This catches a revert to create-first
    //! that every pure `crate::reannounce` test would miss (those never call the FFI). The fake
    //! models the real SDK rule that caused #297: only ONE sender per name may be live at a
    //! time, so a create while the old same-name handle is still live returns null.
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A recorded FFI call. `Destroy` carries the freed handle so the test asserts the OLD sender
    /// (not some other handle) is the one destroyed.
    #[derive(Debug, PartialEq, Eq)]
    enum Op {
        Create,
        Destroy(usize),
    }

    /// Fake NDI function table: records call order and models the SDK's one-live-sender-per-name
    /// rule (a create while a same-name sender is live returns null — the #297 create-first trap).
    struct FakeNdi {
        calls: RefCell<Vec<Op>>,
        live: Cell<bool>,         // a same-name sender is currently registered
        next_handle: Cell<usize>, // mints distinct non-null handles
        fail_create: bool,        // also fail a create even once the name is free (genuine failure)
    }

    impl FakeNdi {
        /// Seed with a sender ALREADY live (steady state before a re-announce). Returns the fake
        /// plus the "old" live handle to feed in as the current `self.sender`.
        fn with_live_sender(fail_create: bool) -> (Self, *mut c_void) {
            let old_handle = 0x1000usize;
            (
                Self {
                    calls: RefCell::new(Vec::new()),
                    live: Cell::new(true),
                    next_handle: Cell::new(0xA000),
                    fail_create,
                },
                old_handle as *mut c_void,
            )
        }
    }

    impl NdiSendOps for FakeNdi {
        fn send_create(&self, _settings: &NDIlib_send_create_t) -> *mut c_void {
            self.calls.borrow_mut().push(Op::Create);
            // SDK rule: refuse a second same-name sender while one is still live → null. This is
            // exactly why create-first could never succeed (#297). Also honor an injected failure.
            if self.live.get() || self.fail_create {
                return ptr::null_mut();
            }
            self.live.set(true);
            let handle = self.next_handle.get();
            self.next_handle.set(handle + 1);
            handle as *mut c_void
        }

        fn send_destroy(&self, sender: *mut c_void) {
            self.calls.borrow_mut().push(Op::Destroy(sender as usize));
            self.live.set(false);
        }
    }

    fn sig(addrs: &[&str]) -> crate::reannounce::NetworkSignature {
        crate::reannounce::NetworkSignature::from_addrs(addrs.iter().copied())
    }

    fn dummy_settings() -> NDIlib_send_create_t {
        // The dance only forwards this struct to the ops; the fake never dereferences its pointers.
        NDIlib_send_create_t {
            p_ndi_name: ptr::null(),
            p_groups: ptr::null(),
            clock_video: false,
            clock_audio: false,
        }
    }

    #[test]
    fn reannounce_destroys_old_before_creating_new_and_advances_trigger() {
        // Acceptance (a)+(c): destroy(old) BEFORE create; a successful create swaps the handle and
        // advances the trigger (announced_sig). A revert to create-first FAILS this test two ways:
        // the recorded order flips, AND the create returns null (name still live) so created_ok is
        // false and the trigger never advances — exactly the #297 infinite re-announce loop.
        let (fake, old) = FakeNdi::with_live_sender(false);
        let up = sig(&["10.77.9.62"]);
        // Boot-race seed: created before the net was up, so a poll once an address appears fires.
        let mut trigger =
            crate::reannounce::ReannounceState::new(crate::reannounce::NetworkSignature::default());
        assert!(trigger.should_reannounce(&up), "boot-race poll must fire");

        let mut sender = old;
        let settings = dummy_settings();
        let created_ok = reannounce_dance(&fake, &mut sender, &settings, &mut trigger, up.clone());

        // (a) ORDERING — the load-bearing #297 invariant.
        assert_eq!(
            *fake.calls.borrow(),
            vec![Op::Destroy(old as usize), Op::Create],
            "re-announce MUST destroy the old sender BEFORE creating the same-name sender"
        );
        // (c) a successful create swaps the handle and advances the trigger.
        assert!(created_ok, "create after destroy must succeed (name freed)");
        assert!(
            !sender.is_null(),
            "successful create installs a live handle"
        );
        assert_ne!(
            sender as usize, old as usize,
            "handle is swapped, not the old one"
        );
        assert!(
            !trigger.should_reannounce(&up),
            "a successful re-announce advances the trigger so a stable poll does not re-fire"
        );
        assert_eq!(
            trigger.announced().addrs(),
            up.addrs(),
            "announced_sig advanced to the current network"
        );
    }

    #[test]
    fn null_create_after_destroy_leaves_sender_null_and_trigger_unchanged() {
        // Acceptance (b): a null create AFTER the destroy leaves self.sender NULL and the trigger
        // unchanged, so the box RETRIES on the next poll instead of stranding with no sender.
        let (fake, old) = FakeNdi::with_live_sender(true); // create fails even after the destroy
        let up = sig(&["10.77.9.62"]);
        let mut trigger =
            crate::reannounce::ReannounceState::new(crate::reannounce::NetworkSignature::default());
        assert!(trigger.should_reannounce(&up));

        let mut sender = old;
        let settings = dummy_settings();
        let created_ok = reannounce_dance(&fake, &mut sender, &settings, &mut trigger, up.clone());

        // Destroy STILL happened first — the old handle is freed before the create is attempted.
        assert_eq!(
            *fake.calls.borrow(),
            vec![Op::Destroy(old as usize), Op::Create],
            "the destroy must precede the (failing) create"
        );
        assert!(!created_ok, "create returned null");
        assert!(
            sender.is_null(),
            "sender is left NULL after a failed re-create"
        );
        assert!(
            trigger.should_reannounce(&up),
            "a failed re-create leaves the trigger firing so the next poll RETRIES"
        );
    }

    #[test]
    fn retry_after_failed_create_then_succeeds() {
        // The recovery sequence end-to-end: poll 1's create fails (sender NULL, trigger still
        // firing); poll 2 retries against the now-null slot — the destroy is SKIPPED (already
        // null, the Drop-null guard) and the create succeeds, converging the trigger.
        let up = sig(&["10.77.9.62"]);
        let mut trigger =
            crate::reannounce::ReannounceState::new(crate::reannounce::NetworkSignature::default());

        // Poll 1: create fails after the destroy.
        let (fake1, old) = FakeNdi::with_live_sender(true);
        let mut sender = old;
        assert!(!reannounce_dance(
            &fake1,
            &mut sender,
            &dummy_settings(),
            &mut trigger,
            up.clone()
        ));
        assert!(sender.is_null());
        assert!(
            trigger.should_reannounce(&up),
            "still firing → retry next poll"
        );

        // Poll 2: the slot is null (no live sender) → destroy skipped, create succeeds.
        let fake2 = FakeNdi {
            calls: RefCell::new(Vec::new()),
            live: Cell::new(false), // the previous create never registered one
            next_handle: Cell::new(0xB000),
            fail_create: false,
        };
        assert!(reannounce_dance(
            &fake2,
            &mut sender,
            &dummy_settings(),
            &mut trigger,
            up.clone()
        ));
        assert_eq!(
            *fake2.calls.borrow(),
            vec![Op::Create],
            "a retry on a null slot skips the destroy (no NDIlib_send_destroy(null)) and creates"
        );
        assert!(!sender.is_null(), "retry installs a live handle");
        assert!(
            !trigger.should_reannounce(&up),
            "retry converges the trigger"
        );
    }
}
