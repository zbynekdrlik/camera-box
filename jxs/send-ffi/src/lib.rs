//! Issue 1387 slice 3: a C ABI over camera-box's JPEG XS SENDER.
//!
//! The strih OBS program output (`vendor/distroav/src/jxs-output.cpp`) calls the functions
//! declared in `include/camera_box_jxs_send.h` (written by hand; `tests/abi.rs` pins every size,
//! offset and name against this file). Every call wraps ONE existing type,
//! `camera_box::jxs_send::JxsHandoff`, the sender the cambox runs: the picture pool, the two
//! sender threads (`jxs-feed` / `jxs-send`), the slice packets with UDP GSO, the receiver-report
//! listener and the automatic bpp all live there. This crate only adds the raw planar intake's C
//! face and a log bridge. No encode or packet code here.
//!
//! The crate is an rlib: `jxs-recv-ffi` pulls it in, so the ONE static library DistroAV links
//! (`libjxs_recv_ffi.a`) carries both ABIs (two Rust static libraries in one shared object would
//! each carry std, camera-box and a second SVT-JPEG-XS).
//!
//! No panic crosses the boundary: each call runs under `catch_unwind`. A panic marks the sender
//! unusable (`-2` from then on); the caller closes it.
#![cfg(target_os = "linux")]

use std::ffi::{c_char, c_int, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::RwLock;

use camera_box::jxs_link::SendWindow;
use camera_box::jxs_send::{
    BppMode, JxsHandoff, JxsSender, LinkConfig, Offered, RawFrame, RawLayout, SenderOptions,
    DEFAULT_AUTO_MAX_BPP, DEFAULT_AUTO_MIN_BPP, MAX_BPP,
};

/// `CBJXS_SEND_ABI_VERSION` of `include/camera_box_jxs_send.h`.
pub const CBJXS_SEND_ABI_VERSION: u32 = 3;

/// `cbjxs_send_stats.pace_source` (`CBJXS_SEND_PACE_*`).
pub const CBJXS_SEND_PACE_LINE_RATE: u32 = 0;
pub const CBJXS_SEND_PACE_FIXED: u32 = 1;
pub const CBJXS_SEND_PACE_RECEIVER_LINK: u32 = 2;
pub const CBJXS_SEND_PACE_START_SPREAD: u32 = 3;
pub const CBJXS_SEND_PACE_CONGESTION: u32 = 4;

/// `cbjxs_send_stats.path_state` (`CBJXS_SEND_PATH_*`).
pub const CBJXS_SEND_PATH_UNKNOWN: u32 = 0;
pub const CBJXS_SEND_PATH_CLEAN: u32 = 1;
pub const CBJXS_SEND_PATH_HOLD: u32 = 2;
pub const CBJXS_SEND_PATH_OVERUSE: u32 = 3;

/// The C code of a path state.
pub fn path_state_code(s: camera_box::jxs_congestion::PathState) -> u32 {
    use camera_box::jxs_congestion::PathState as S;
    match s {
        S::Unknown => CBJXS_SEND_PATH_UNKNOWN,
        S::Clean => CBJXS_SEND_PATH_CLEAN,
        S::Hold => CBJXS_SEND_PATH_HOLD,
        S::Overuse => CBJXS_SEND_PATH_OVERUSE,
    }
}

/// The C code of a pace source.
pub fn pace_source_code(s: camera_box::jxs_link::pace_auto::PaceSource) -> u32 {
    use camera_box::jxs_link::pace_auto::PaceSource as P;
    match s {
        P::LineRate => CBJXS_SEND_PACE_LINE_RATE,
        P::Fixed => CBJXS_SEND_PACE_FIXED,
        P::ReceiverLink => CBJXS_SEND_PACE_RECEIVER_LINK,
        P::StartSpread => CBJXS_SEND_PACE_START_SPREAD,
        P::Congestion => CBJXS_SEND_PACE_CONGESTION,
    }
}

/// `cbjxs_send_picture.format` (`RawLayout::from_code`).
pub const CBJXS_SEND_I420: u32 = 1;
pub const CBJXS_SEND_I422: u32 = 2;
pub const CBJXS_SEND_I444: u32 = 3;
pub const CBJXS_SEND_NV12: u32 = 4;

/// `cbjxs_send_config`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CbjxsSendConfig {
    pub target: *const c_char,
    pub cores: *const c_char,
    pub emit_fps: u32,
    pub stream_id: u8,
    pub bpp_min: u8,
    pub bpp_max: u8,
    pub reserved: u8,
    /// ABI 2: the send pacing in Mb/s of wire; ABI 3: 0 = automatic (from the receiver's link).
    pub pace_mbps: u32,
    pub reserved2: u32,
}

/// `cbjxs_send_picture`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CbjxsSendPicture {
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub reserved: u32,
    pub planes: [*const u8; 3],
    pub strides: [u32; 3],
    pub reserved2: u32,
    pub timecode_100ns: i64,
}

/// `cbjxs_send_stats`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CbjxsSendStats {
    pub seq: u64,
    pub window_s: f64,
    pub frames: u64,
    pub bytes: u64,
    pub dropped_busy: u64,
    pub send_errors: u64,
    pub encode_errors: u64,
    pub reports: u64,
    pub encode_ms_avg: f64,
    pub encode_ms_max: f64,
    pub first_slice_ms_avg: f64,
    pub copy_ms_avg: f64,
    pub copy_ms_max: f64,
    pub report_age_s: f64,
    pub bpp: u32,
    pub auto_bpp: u8,
    pub reserved: [u8; 3],
    /// ABI 2: the pacing the window sent at (0 = line rate) and each whole frame's time on the
    /// wire, first send -> last send returned.
    pub pace_mbps: u32,
    /// ABI 3: `CBJXS_SEND_PACE_*` (was `reserved2`).
    pub pace_source: u32,
    pub wire_ms_avg: f64,
    pub wire_ms_max: f64,
    /// ABI 3: the receiver's link from its last report and the sender's own, Mb/s (0 = unknown).
    pub rx_link_mbps: u32,
    pub own_link_mbps: u32,
    /// ABI 3: the congestion control (`CBJXS_SEND_PATH_*`, the factor in percent, the receiver's
    /// newest queuing delay in us, `u32::MAX` = none).
    pub path_state: u32,
    pub congestion_pct: u32,
    pub queue_delay_us: u32,
    pub reserved3: u32,
}

fn avg(sum: f64, n: u64) -> f64 {
    if n > 0 {
        sum / n as f64
    } else {
        0.0
    }
}

impl CbjxsSendStats {
    /// The C view of one finished status window.
    pub fn from_window(seq: u64, w: &SendWindow) -> Self {
        Self {
            seq,
            window_s: w.window_s,
            frames: w.frames,
            bytes: w.bytes,
            dropped_busy: w.dropped_busy,
            send_errors: w.send_errors,
            encode_errors: w.encode_errors,
            reports: w.reports,
            encode_ms_avg: avg(w.encode_ms_sum, w.frames),
            encode_ms_max: w.encode_ms_max,
            first_slice_ms_avg: avg(w.first_slice_ms_sum, w.frames),
            copy_ms_avg: avg(w.convert_ms_sum, w.convert_count),
            copy_ms_max: w.convert_ms_max,
            report_age_s: w.report_age_s.unwrap_or(-1.0),
            bpp: w.bpp,
            auto_bpp: w.auto as u8,
            reserved: [0; 3],
            pace_mbps: w.pace_mbps,
            pace_source: pace_source_code(w.pace_source),
            wire_ms_avg: avg(w.wire_ms_sum, w.wire_frames),
            wire_ms_max: w.wire_ms_max,
            rx_link_mbps: w.rx_link_mbps,
            own_link_mbps: w.own_link_mbps,
            path_state: path_state_code(w.path_state),
            congestion_pct: w.congestion_pct,
            queue_delay_us: w.queue_delay_us.unwrap_or(u32::MAX),
            reserved3: 0,
        }
    }
}

/// `cbjxs_sender`: opaque to C.
pub struct CbjxsSender {
    tx: JxsHandoff,
    emit_fps: u32,
    error: Option<String>,
    poisoned: bool,
}

/// Copy `msg` into a C buffer, truncated, always NUL-terminated when `len > 0`. Returns the bytes
/// written without the NUL.
///
/// # Safety
/// `buf` is NULL or valid for `len` bytes.
unsafe fn write_c(buf: *mut c_char, len: usize, msg: &str) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let mut n = msg.len().min(len - 1);
    while !msg.is_char_boundary(n) {
        n -= 1;
    }
    std::ptr::copy_nonoverlapping(msg.as_ptr().cast::<c_char>(), buf, n);
    *buf.add(n) = 0;
    n
}

/// The bpp mode of a config: `0` keeps a default, `min == max` pins that bpp (auto off).
pub fn bpp_mode(min: u8, max: u8) -> Result<BppMode, String> {
    let lo = if min == 0 {
        DEFAULT_AUTO_MIN_BPP
    } else {
        min as u32
    };
    let hi = if max == 0 {
        DEFAULT_AUTO_MAX_BPP
    } else {
        max as u32
    };
    if hi > MAX_BPP || lo > hi {
        return Err(format!(
            "bpp {lo}..={hi} is not a range inside 1..={MAX_BPP}"
        ));
    }
    Ok(if lo == hi {
        BppMode::Fixed(lo)
    } else {
        BppMode::Auto { min: lo, max: hi }
    })
}

/// The config's C strings and numbers as a [`LinkConfig`].
///
/// # Safety
/// `target` / `cores` are NULL or NUL-terminated.
unsafe fn link_config(c: &CbjxsSendConfig) -> Result<LinkConfig, String> {
    let text = |p: *const c_char| -> Result<String, String> {
        if p.is_null() {
            return Ok(String::new());
        }
        CStr::from_ptr(p)
            .to_str()
            .map(str::to_string)
            .map_err(|_| "a setting is not UTF-8".to_string())
    };
    let target = text(c.target)?;
    if target.trim().is_empty() {
        return Err("no target (host:port)".to_string());
    }
    let cores_text = text(c.cores)?;
    let cores = camera_box::affinity::parse_cpulist(cores_text.trim());
    if !cores_text.trim().is_empty() && cores.is_empty() {
        return Err(format!("cores '{cores_text}' is not a cpulist"));
    }
    Ok(LinkConfig {
        target: target.trim().to_string(),
        camera_id: c.stream_id,
        cores,
        opts: SenderOptions::new(bpp_mode(c.bpp_min, c.bpp_max)?, None)
            .with_pace(camera_box::jxs_link::pace::check_pace_mbps(c.pace_mbps)?),
        egress_mark: false,
    })
}

#[no_mangle]
pub extern "C" fn cbjxs_send_abi_version() -> u32 {
    CBJXS_SEND_ABI_VERSION
}

/// The log callback (`None` = nothing forwarded). Read once per line (a few lines a minute).
/// A sender thread left detached after the bounded close (a wedged codec call) may still call it
/// later, also during OBS shutdown: the callback must stay callable for the process's life
/// (`obs_log` is).
static LOG_FN: RwLock<Option<LogFn>> = RwLock::new(None);
/// Whether this library's subscriber is the process's global one.
static LOG_INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

type LogFn = unsafe extern "C" fn(c_int, *const c_char);

/// One formatted event, handed to the callback when the formatter is done with it.
struct LineWriter {
    level: c_int,
    buf: Vec<u8>,
}

impl std::io::Write for LineWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        let Some(f) = *LOG_FN.read().unwrap_or_else(|e| e.into_inner()) else {
            return;
        };
        while self.buf.last().is_some_and(|b| *b == b'\n' || *b == b' ') {
            self.buf.pop();
        }
        self.buf.retain(|b| *b != 0);
        if self.buf.is_empty() {
            return;
        }
        self.buf.push(0);
        // SAFETY: `f` is the caller's `cbjxs_send_log_fn`; the line is NUL-terminated and lives
        // for the call.
        unsafe { f(self.level, self.buf.as_ptr().cast()) }
    }
}

struct MakeLine;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeLine {
    type Writer = LineWriter;

    fn make_writer(&'a self) -> LineWriter {
        LineWriter {
            level: 3,
            buf: Vec::new(),
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> LineWriter {
        let level = match *meta.level() {
            tracing::Level::ERROR => 1,
            tracing::Level::WARN => 2,
            _ => 3,
        };
        LineWriter {
            level,
            buf: Vec::with_capacity(256),
        }
    }
}

/// Route the library's tracing lines to `f` (NULL stops it). 1 = forwarding, 0 = another
/// subscriber owns the process.
#[no_mangle]
pub extern "C" fn cbjxs_send_set_log(f: Option<LogFn>) -> c_int {
    let installed = catch_unwind(|| {
        *LOG_INSTALLED.get_or_init(|| {
            let sub = tracing_subscriber::fmt()
                .with_writer(MakeLine)
                .with_ansi(false)
                .without_time()
                .with_target(false)
                .with_level(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            tracing::subscriber::set_global_default(sub).is_ok()
        })
    })
    .unwrap_or(false);
    *LOG_FN.write().unwrap_or_else(|e| e.into_inner()) = f;
    installed as c_int
}

/// # Safety
/// `cfg` is NULL or valid with NUL-terminated (or NULL) strings; `err` is NULL or valid for
/// `err_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_send_open(
    cfg: *const CbjxsSendConfig,
    err: *mut c_char,
    err_len: usize,
) -> *mut CbjxsSender {
    let Some(c) = cfg.as_ref() else {
        write_c(err, err_len, "no config");
        return std::ptr::null_mut();
    };
    let emit_fps = c.emit_fps;
    let opened = catch_unwind(AssertUnwindSafe(|| {
        let cfg = link_config(c)?;
        JxsSender::start_link(cfg)
    }));
    match opened {
        Ok(Ok(tx)) => Box::into_raw(Box::new(CbjxsSender {
            tx,
            emit_fps,
            error: None,
            poisoned: false,
        })),
        Ok(Err(msg)) => {
            write_c(err, err_len, &msg);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_c(err, err_len, "the sender panicked while opening");
            std::ptr::null_mut()
        }
    }
}

/// The byte length of a plane of `rows` rows of `row_len` bytes, `stride` apart; `None` when the
/// stride is shorter than the row.
fn plane_len(stride: u32, row_len: u32, rows: u32) -> Option<usize> {
    if stride < row_len || rows == 0 {
        return None;
    }
    Some(stride as usize * (rows as usize - 1) + row_len as usize)
}

/// Why a picture is refused before any plane is read (`None` = it may be read).
fn picture_problem(p: &CbjxsSendPicture) -> Option<String> {
    let Some(layout) = RawLayout::from_code(p.format) else {
        return Some(format!("unknown picture format {}", p.format));
    };
    if !layout.fits(p.width, p.height) {
        return Some(format!(
            "{}x{} does not fit {}",
            p.width,
            p.height,
            layout.label()
        ));
    }
    let (cw, ch) = layout.chroma_dims(p.width, p.height);
    let rows = [
        (p.width, p.height),
        (
            if layout == RawLayout::Nv12 {
                cw * 2
            } else {
                cw
            },
            ch,
        ),
        (cw, ch),
    ];
    for (i, (row_len, n)) in rows.iter().take(layout.plane_count()).enumerate() {
        if p.planes[i].is_null() {
            return Some(format!("{} plane {i} is NULL", layout.label()));
        }
        if plane_len(p.strides[i], *row_len, *n).is_none() {
            return Some(format!(
                "{} plane {i}: stride {} is shorter than its {row_len}-byte row",
                layout.label(),
                p.strides[i]
            ));
        }
    }
    None
}

/// # Safety
/// `tx` is NULL or a live sender; `pic` is NULL or valid, its planes readable for the rows the
/// format and strides name.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_send_frame(
    tx: *mut CbjxsSender,
    pic: *const CbjxsSendPicture,
) -> c_int {
    let (Some(s), Some(p)) = (tx.as_mut(), pic.as_ref()) else {
        return -2;
    };
    if s.poisoned {
        return -2;
    }
    if let Some(e) = picture_problem(p) {
        s.error = Some(format!("picture refused: {e}"));
        return -1;
    }
    let layout = RawLayout::from_code(p.format).expect("checked");
    let (cw, ch) = layout.chroma_dims(p.width, p.height);
    let rows = [
        (p.width, p.height),
        (
            if layout == RawLayout::Nv12 {
                cw * 2
            } else {
                cw
            },
            ch,
        ),
        (cw, ch),
    ];
    let mut planes: [&[u8]; 3] = [&[], &[], &[]];
    for (i, (row_len, n)) in rows.iter().take(layout.plane_count()).enumerate() {
        let len = plane_len(p.strides[i], *row_len, *n).expect("checked");
        // SAFETY: the caller's contract: plane i is readable for its rows at its stride.
        planes[i] = std::slice::from_raw_parts(p.planes[i], len);
    }
    let frame = RawFrame {
        layout,
        width: p.width,
        height: p.height,
        planes,
        strides: p.strides.map(|v| v as usize),
        timecode_100ns: p.timecode_100ns,
        emit_fps: s.emit_fps,
    };
    match catch_unwind(AssertUnwindSafe(|| s.tx.offer_raw(&frame))) {
        Ok(Offered::Taken) => 1,
        Ok(Offered::Busy) | Ok(Offered::OffGrid) => 0,
        Ok(Offered::Refused) => {
            s.error = Some(format!(
                "picture refused: {}x{} {} did not copy",
                p.width,
                p.height,
                layout.label()
            ));
            -1
        }
        Err(_) => {
            s.poisoned = true;
            s.error = Some("the sender panicked; close it".into());
            -2
        }
    }
}

/// # Safety
/// `tx` is NULL or a live sender; `out` is NULL or valid for one `CbjxsSendStats`.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_send_get_stats(
    tx: *mut CbjxsSender,
    out: *mut CbjxsSendStats,
) -> c_int {
    let (Some(s), Some(out)) = (tx.as_mut(), out.as_mut()) else {
        return -1;
    };
    match s.tx.take_window() {
        Some((seq, w)) => {
            *out = CbjxsSendStats::from_window(seq, &w);
            1
        }
        None => 0,
    }
}

/// # Safety
/// `tx` is NULL or a live sender; `buf` is NULL or valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_send_take_error(
    tx: *mut CbjxsSender,
    buf: *mut c_char,
    len: usize,
) -> usize {
    let Some(s) = tx.as_mut() else {
        return 0;
    };
    if buf.is_null() || len == 0 {
        return 0;
    }
    match s.error.take() {
        Some(e) => write_c(buf, len, &e),
        None => 0,
    }
}

/// # Safety
/// `tx` is NULL or a live sender from `cbjxs_send_open`, not used again afterwards.
#[no_mangle]
pub unsafe extern "C" fn cbjxs_send_close(tx: *mut CbjxsSender) {
    if !tx.is_null() {
        // dropping the hand-off stops its threads (bounded); no panic unwinds into C
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(tx))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bpp_range_defaults_pins_and_refuses() {
        assert_eq!(bpp_mode(0, 0), Ok(BppMode::Auto { min: 4, max: 8 }));
        assert_eq!(bpp_mode(5, 10), Ok(BppMode::Auto { min: 5, max: 10 }));
        assert_eq!(bpp_mode(6, 6), Ok(BppMode::Fixed(6)));
        assert!(bpp_mode(9, 5).is_err());
        assert!(bpp_mode(4, 13).is_err());
        assert!(bpp_mode(9, 0).is_err(), "a floor above the default ceiling");
    }

    fn pic(format: u32, w: u32, h: u32, strides: [u32; 3]) -> CbjxsSendPicture {
        static P: [u8; 1] = [0];
        CbjxsSendPicture {
            format,
            width: w,
            height: h,
            reserved: 0,
            planes: [P.as_ptr(); 3],
            strides,
            reserved2: 0,
            timecode_100ns: 0,
        }
    }

    #[test]
    fn a_picture_is_refused_before_a_plane_is_read() {
        assert!(picture_problem(&pic(1, 64, 16, [64, 32, 32])).is_none());
        assert!(
            picture_problem(&pic(4, 64, 16, [64, 64, 0])).is_none(),
            "NV12 needs two planes"
        );
        let e = picture_problem(&pic(9, 64, 16, [64, 32, 32])).unwrap();
        assert!(e.contains("unknown picture format 9"), "{e}");
        let e = picture_problem(&pic(1, 64, 15, [64, 32, 32])).unwrap();
        assert!(e.contains("does not fit I420"), "{e}");
        let e = picture_problem(&pic(4, 64, 16, [64, 63, 0])).unwrap();
        assert!(e.contains("plane 1: stride 63"), "{e}");
        let e = picture_problem(&pic(3, 64, 16, [64, 64, 32])).unwrap();
        assert!(e.contains("plane 2"), "{e}");
        let mut null = pic(2, 64, 16, [64, 32, 32]);
        null.planes[2] = std::ptr::null();
        assert!(picture_problem(&null).unwrap().contains("plane 2 is NULL"));
    }

    #[test]
    fn a_bad_config_names_its_problem_and_starts_nothing() {
        let open = |target: &CStr, cores: Option<&CStr>, min, max, pace_mbps| {
            let c = CbjxsSendConfig {
                target: target.as_ptr(),
                cores: cores.map_or(std::ptr::null(), CStr::as_ptr),
                emit_fps: 30,
                stream_id: 100,
                bpp_min: min,
                bpp_max: max,
                reserved: 0,
                pace_mbps,
                reserved2: 0,
            };
            let mut err = [0 as c_char; 128];
            let tx = unsafe { cbjxs_send_open(&c, err.as_mut_ptr(), err.len()) };
            assert!(tx.is_null());
            unsafe { CStr::from_ptr(err.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        assert!(open(c"", None, 0, 0, 0).contains("no target"));
        assert!(open(c"10.0.0.1", None, 0, 0, 0).contains("port"));
        assert!(open(c"127.0.0.1:9", Some(c"x"), 0, 0, 0).contains("cpulist"));
        assert!(open(c"127.0.0.1:9", None, 9, 4, 0).contains("bpp"));
        assert!(open(c"127.0.0.1:9", None, 0, 0, 50).contains("pace 50"));
        assert!(unsafe { cbjxs_send_open(std::ptr::null(), std::ptr::null_mut(), 0) }.is_null());
    }

    fn cfg_with_pace(pace_mbps: u32) -> CbjxsSendConfig {
        CbjxsSendConfig {
            target: c"127.0.0.1:9".as_ptr(),
            cores: std::ptr::null(),
            emit_fps: 30,
            stream_id: 100,
            bpp_min: 0,
            bpp_max: 0,
            reserved: 0,
            pace_mbps,
            reserved2: 0,
        }
    }

    #[test]
    fn the_config_pace_reaches_the_sender_options() {
        let l = unsafe { link_config(&cfg_with_pace(900)) }.unwrap();
        assert_eq!(l.opts.pace_mbps, 900);
        let l = unsafe { link_config(&cfg_with_pace(0)) }.unwrap();
        assert_eq!(l.opts.pace_mbps, 0, "0 = automatic");
        let e = unsafe { link_config(&cfg_with_pace(100_001)) }.unwrap_err();
        assert!(e.contains("pace 100001"), "{e}");
    }

    #[test]
    fn the_window_maps_onto_the_c_stats() {
        let w = SendWindow {
            frames: 4,
            encode_ms_sum: 40.0,
            encode_ms_max: 12.5,
            convert_count: 5,
            convert_ms_sum: 2.5,
            convert_ms_max: 0.9,
            bpp: 7,
            auto: true,
            reports: 5,
            window_s: 5.0,
            pace_mbps: 900,
            pace_source: camera_box::jxs_link::pace_auto::PaceSource::ReceiverLink,
            rx_link_mbps: 1000,
            own_link_mbps: 10_000,
            path_state: camera_box::jxs_congestion::PathState::Overuse,
            congestion_pct: 85,
            queue_delay_us: Some(6_100),
            // 4 frames, 2 of them with a wire time: the average divides by 2, not 4
            wire_frames: 2,
            wire_ms_sum: 40.0,
            wire_ms_max: 21.5,
            ..SendWindow::default()
        };
        let s = CbjxsSendStats::from_window(3, &w);
        assert_eq!(
            (s.pace_mbps, s.wire_ms_avg, s.wire_ms_max),
            (900, 20.0, 21.5)
        );
        assert_eq!(
            (s.pace_source, s.rx_link_mbps, s.own_link_mbps),
            (CBJXS_SEND_PACE_RECEIVER_LINK, 1000, 10_000)
        );
        // the C codes (the DistroAV output's cb_jxs_out_pace_source reads these numbers)
        use camera_box::jxs_link::pace_auto::PaceSource as P;
        assert_eq!(
            [
                P::LineRate,
                P::Fixed,
                P::ReceiverLink,
                P::StartSpread,
                P::Congestion
            ]
            .map(pace_source_code),
            [0, 1, 2, 3, 4]
        );
        assert_eq!(
            (s.path_state, s.congestion_pct, s.queue_delay_us),
            (CBJXS_SEND_PATH_OVERUSE, 85, 6_100)
        );
        use camera_box::jxs_congestion::PathState as S;
        assert_eq!(
            [S::Unknown, S::Clean, S::Hold, S::Overuse].map(path_state_code),
            [0, 1, 2, 3]
        );
        assert_eq!(
            CbjxsSendStats::from_window(1, &SendWindow::default()).queue_delay_us,
            u32::MAX,
            "none measured"
        );
        assert_eq!(
            (s.seq, s.frames, s.bpp, s.auto_bpp, s.reports),
            (3, 4, 7, 1, 5)
        );
        assert_eq!((s.encode_ms_avg, s.copy_ms_avg), (10.0, 0.5));
        assert_eq!(s.report_age_s, -1.0, "never a report");
    }
}
