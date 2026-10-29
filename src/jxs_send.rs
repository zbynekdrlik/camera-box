//! Issue 1387 -- the cambox JPEG XS sender (slice 1: one configured receiver, no OBS side yet;
//! slice 1b: two frames in flight, the hand-off before the NDI send, AVX2 conversion).
//!
//! OFF by default: unless `CAMERA_BOX_JXS_TARGET=host:port` is set, [`JxsSender::start_from_env`]
//! returns `None`, no thread, socket or encoder exists, and the capture loop's NDI path is
//! byte-identical (its only new statement is an `if let Some(..)` on that `None`).
//!
//! When set, the capture thread hands each emitted frame over through [`JxsHandoff::offer`] right
//! after its genlock timecode is fixed and BEFORE the NDI send: it converts the YUYV frame
//! straight into one of [`FRAMES_IN_FLIGHT`] reusable planar pictures (AVX2, no separate copy) and
//! queues it, never waiting. With both pictures in use the frame is dropped and counted
//! (`dropped_busy`). The timecode is the SAME value the NDI frame carries.
//!
//! Two threads, pinned to the encoder's explicit core set (the off-capture cores of the #1198
//! split, or [`CORES_ENV`] inside them; the capture core and its IRQ core are never used):
//! - `jxs-feed` opens the SVT-JPEG-XS encoder (latency profile, decomp_v 2, 4 bpp: issue comment
//!   6489868243; one slice-pack thread per core, its threads inherit the core set) and submits
//!   each converted picture at once, so frame N+1 is encoding while frame N's last slices go out;
//! - `jxs-send` reads each frame's packets the moment the encoder produces them and sends every
//!   slice as one `send` with `UDP_SEGMENT` (UDP GSO; wire format: `crate::jxs_link`), then gives
//!   the picture back.
//!
//! Every 5 s it logs `jxs send: fps= bpp= frames= slices= bytes= dropped_busy= in_flight=max/cap
//! convert_ms_avg/max= first_slice_ms_avg/max= encode_ms_avg/max= send_errors= encode_errors=
//! (issue 1387)`. A frame in flight with no packet for [`WEDGE_MS`] logs one
//! `CRITICAL jxs send: encoder wedged` line from the feeder (the status line lives on the packet
//! thread, which is the one stuck).
//!
//! The codec needs the `jxs` cargo feature (CI's deployed `camera-box-linux-amd64` carries it).
//! A build without it logs one ERROR when the target is set and stays off.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::jxs_link::{self, DATAGRAM_LEN};

/// The environment variable that turns the link on.
pub const TARGET_ENV: &str = "CAMERA_BOX_JXS_TARGET";
/// Status line period.
pub const STATUS_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);
/// Socket send buffer: a whole 1080p frame at 4 bpp (~1.04 MB) plus headers, twice.
pub const SEND_BUFFER_BYTES: i32 = 4 * 1024 * 1024;

/// The `SO_MARK` the JPEG XS socket carries: the cambox egress pacing (issue 1242) paces every
/// flow at 400 Mb/s in an `fq`, and its `fw` filter on this mark sends the JPEG XS flow past that
/// fq at line rate (issue 1387). The ONE declaration is `CAMBOX_EGRESS_PACING_JXS_MARK` in
/// `scripts/lib/cambox-egress-pacing.sh`; a pytest pins this value equal to it.
pub const JXS_SOCKET_MARK: u32 = 0x1387;

/// `UDP_SEGMENT` / `UDP_GRO` (linux/udp.h; spelled here so an older `libc` cannot lack them).
pub const UDP_SEGMENT: libc::c_int = 103;
pub const UDP_GRO: libc::c_int = 104;

/// A connected UDP socket that sends whole slices with GSO.
pub struct JxsSocket {
    sock: UdpSocket,
    gso: AtomicBool,
    /// `None` = the kernel accepted [`JXS_SOCKET_MARK`]; else the errno it answered.
    mark_error: Option<i32>,
}

impl JxsSocket {
    /// Connect to `target` (`host:port`, resolved once now).
    pub fn connect(target: &str) -> std::io::Result<Self> {
        let addr: SocketAddr = target.to_socket_addrs()?.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{target}: no address"),
            )
        })?;
        // The 1472-byte datagram is the IPv4 budget of a 1500-byte MTU; over IPv6 it would be
        // fragmented (and GSO refuses it), so the link is IPv4 only.
        if !addr.is_ipv4() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("{target} resolves to {addr}: the JPEG XS link is IPv4 only"),
            ));
        }
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect(addr)?;
        let fd = sock.as_raw_fd();
        set_int_opt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, SEND_BUFFER_BYTES);
        let gso = set_int_opt(fd, libc::SOL_UDP, UDP_SEGMENT, DATAGRAM_LEN as libc::c_int);
        // SO_MARK needs CAP_NET_ADMIN or CAP_NET_RAW (camera-box.service runs as root). Without it
        // the flow is still sent, only paced with NDI: `marked()` reports it, the start WARNs with
        // the errno and the "link ON" line names it.
        let mark_error = if set_int_opt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_MARK,
            JXS_SOCKET_MARK as libc::c_int,
        ) {
            None
        } else {
            Some(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        };
        Ok(Self {
            sock,
            gso: AtomicBool::new(gso),
            mark_error,
        })
    }

    /// Whether the kernel accepted [`JXS_SOCKET_MARK`] (`SO_MARK`), i.e. the egress pacing's fw
    /// filter sends this flow at line rate.
    pub fn marked(&self) -> bool {
        self.mark_error.is_none()
    }

    /// Whether GSO is in use (the kernel accepted `UDP_SEGMENT`, and no GSO send has failed with
    /// EIO/EINVAL since; else every datagram is its own send).
    pub fn gso(&self) -> bool {
        self.gso.load(Ordering::Relaxed)
    }

    /// The socket send buffer the kernel granted (it caps the request at `net.core.wmem_max`).
    pub fn send_buffer(&self) -> i64 {
        get_int_opt(self.sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF)
    }

    pub fn peer(&self) -> std::io::Result<SocketAddr> {
        self.sock.peer_addr()
    }

    /// Send one slice built by `jxs_link::build_slice_datagrams`. Returns the number of send
    /// calls that failed (a receiver that is not listening answers with ICMP, which the next
    /// send reports as `ECONNREFUSED`; the link never waits for it).
    pub fn send_slice(&self, datagrams: &[u8]) -> u64 {
        let mut errors = 0;
        for r in jxs_link::gso_send_ranges(datagrams.len()) {
            let batch = &datagrams[r];
            if self.gso() {
                match self.sock.send(batch) {
                    Ok(_) => continue,
                    // A NIC without checksum offload (EIO) or a path the kernel cannot segment
                    // (EINVAL): fall back to one send per datagram for good, said once.
                    Err(e) if matches!(e.raw_os_error(), Some(libc::EIO) | Some(libc::EINVAL)) => {
                        self.gso.store(false, Ordering::Relaxed);
                        // The kernel checks the segment size and checksum offload whenever the
                        // socket option is set, whatever the send's length: clear it.
                        set_int_opt(self.sock.as_raw_fd(), libc::SOL_UDP, UDP_SEGMENT, 0);
                        tracing::warn!(
                            "jxs send: a GSO send failed ({e}) -- one send per datagram from now on (issue 1387)"
                        );
                    }
                    Err(_) => {
                        errors += 1;
                        continue;
                    }
                }
            }
            for d in batch.chunks(DATAGRAM_LEN) {
                if self.sock.send(d).is_err() {
                    errors += 1;
                }
            }
        }
        errors
    }
}

/// Set an int socket option; `true` on success.
pub(crate) fn set_int_opt(
    fd: libc::c_int,
    level: libc::c_int,
    name: libc::c_int,
    value: libc::c_int,
) -> bool {
    // SAFETY: `value` is a valid c_int for the call's duration; the fd is an open socket.
    unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (&value as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        ) == 0
    }
}

/// Read an int socket option (-1 on failure).
pub(crate) fn get_int_opt(fd: libc::c_int, level: libc::c_int, name: libc::c_int) -> i64 {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: valid out-pointers sized for a c_int; the fd is an open socket.
    let ok = unsafe {
        libc::getsockopt(
            fd,
            level,
            name,
            (&mut v as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if ok == 0 {
        v as i64
    } else {
        -1
    }
}

/// Frames the sender holds at once (slice 1b): one whose slices are coming out, one the encoder
/// already started. A third frame while both are busy is `dropped_busy`.
pub const FRAMES_IN_FLIGHT: usize = 2;
/// The environment variable that names the encoder's cores explicitly (a cpulist, `0-1`); it must
/// lie inside the off-capture cores (issue 1198 split), else it is ignored with a WARN.
pub const CORES_ENV: &str = "CAMERA_BOX_JXS_CORES";
/// The most slice-pack threads the encoder runs (a 1080p frame has 68 slices).
pub const MAX_SLICE_THREADS: usize = 4;
/// The environment variable that sets the encoder's bits per pixel (owner decision 29.10.2026).
pub const BPP_ENV: &str = "CAMERA_BOX_JXS_BPP";
/// The default bits per pixel (comment 6489868243: ~250 Mb/s at 30 fps).
pub const DEFAULT_BPP: u32 = 4;
/// The highest bpp the wire format carries: a 16-line 1080p slice at 12 bpp is ~46 KB, under the
/// 64 KB slice length field.
pub const MAX_BPP: u32 = 12;

/// The encoder's bits per pixel from [`BPP_ENV`]: unset or blank = [`DEFAULT_BPP`]; a whole
/// number 1..=[`MAX_BPP`] is taken; anything else is refused (`Err`, the caller logs it and keeps
/// the default).
pub fn parse_bpp(value: Option<&str>) -> Result<u32, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_BPP);
    };
    match v.parse::<u32>() {
        Ok(b) if (1..=MAX_BPP).contains(&b) => Ok(b),
        _ => Err(format!(
            "{BPP_ENV}='{v}' is not a whole number 1..={MAX_BPP}"
        )),
    }
}

/// The encoder's core set: [`CORES_ENV`] when it is set and inside `allowed` (the off-capture
/// cores), else `allowed`. `Err` names why an override was refused (the caller logs it and uses
/// `allowed`).
pub fn select_encoder_cores(
    override_list: Option<&str>,
    allowed: &[usize],
) -> Result<Vec<usize>, String> {
    let Some(raw) = override_list.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(allowed.to_vec());
    };
    let cores = crate::affinity::parse_cpulist(raw);
    if cores.is_empty() {
        return Err(format!("{CORES_ENV}='{raw}' is not a cpulist"));
    }
    if let Some(c) = cores.iter().find(|c| !allowed.contains(c)) {
        return Err(format!(
            "{CORES_ENV}='{raw}' names core {c}, outside the off-capture cores {allowed:?} (the capture core and its IRQ core stay untouched)"
        ));
    }
    Ok(cores)
}

/// The slice-pack threads for a core set: one per core, at least 1, at most
/// [`MAX_SLICE_THREADS`].
pub fn slice_threads_for(cores: &[usize]) -> u32 {
    cores.len().clamp(1, MAX_SLICE_THREADS) as u32
}

/// No packet for this long while a frame is in flight = the encoder is wedged (~60 frames at
/// 30 fps; one frame's budget is 33.3 ms). Alert-only: the link cannot recover a hung library
/// call, the capture loop and NDI are never touched.
pub const WEDGE_MS: u64 = 2_000;
// far above one frame's 33.3 ms budget: a slow frame is never an alarm
const _: () = assert!(WEDGE_MS > 30 * 34);
/// How often the feeder looks at the packet thread's progress.
pub const WEDGE_CHECK_PERIOD: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether the encoder is wedged: a frame in flight and no packet progress for [`WEDGE_MS`].
pub fn encoder_wedged(in_flight: usize, ms_since_progress: u64) -> bool {
    in_flight > 0 && ms_since_progress >= WEDGE_MS
}

/// Milliseconds since the first call in this process (the progress clock).
#[cfg_attr(not(feature = "jxs"), allow(dead_code))]
fn monotonic_ms() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Counters the capture thread and the two sender threads share (read and reset by the status
/// line).
#[derive(Debug, Default)]
#[cfg_attr(not(feature = "jxs"), allow(dead_code))]
struct Stats {
    dropped_busy: AtomicU64,
    not_yuyv: AtomicBool,
    convert_errors: AtomicU64,
    convert_count: AtomicU64,
    convert_ns_sum: AtomicU64,
    convert_ns_max: AtomicU64,
    /// Frames submitted to the encoder and not yet received back.
    in_flight: AtomicUsize,
    in_flight_max: AtomicUsize,
    /// [`monotonic_ms`] of the packet thread's last sign of life (a loop turn or a packet).
    progress_ms: AtomicU64,
}

#[cfg_attr(not(feature = "jxs"), allow(dead_code))]
impl Stats {
    fn note_convert(&self, ns: u64) {
        self.convert_count.fetch_add(1, Ordering::Relaxed);
        self.convert_ns_sum.fetch_add(ns, Ordering::Relaxed);
        self.convert_ns_max.fetch_max(ns, Ordering::Relaxed);
    }

    /// Move the shared counters into `w` and reset them (the in-flight max restarts from now).
    fn drain_into(&self, w: &mut jxs_link::SendWindow) {
        w.dropped_busy = self.dropped_busy.swap(0, Ordering::Relaxed);
        w.encode_errors += self.convert_errors.swap(0, Ordering::Relaxed);
        w.convert_count = self.convert_count.swap(0, Ordering::Relaxed);
        w.convert_ms_sum = self.convert_ns_sum.swap(0, Ordering::Relaxed) as f64 / 1e6;
        w.convert_ms_max = self.convert_ns_max.swap(0, Ordering::Relaxed) as f64 / 1e6;
        let now = self.in_flight.load(Ordering::Relaxed);
        w.in_flight_max = self.in_flight_max.swap(now, Ordering::Relaxed) as u64;
        w.in_flight_cap = FRAMES_IN_FLIGHT as u64;
    }
}

#[cfg(feature = "jxs")]
pub use handoff::JxsHandoff;

/// The capture thread's handle without the codec: never constructed (the link stays off).
#[cfg(not(feature = "jxs"))]
pub struct JxsHandoff {
    never: std::convert::Infallible,
}

#[cfg(not(feature = "jxs"))]
impl JxsHandoff {
    pub fn offer(&self, _frame: &JxsFrame<'_>) {
        match self.never {}
    }

    pub fn dropped_busy(&self) -> u64 {
        match self.never {}
    }
}

/// One emitted frame as the capture loop hands it over.
#[derive(Debug, Clone, Copy)]
pub struct JxsFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub yuyv: bool,
    /// The genlock timecode the NDI frame carries.
    pub timecode_100ns: i64,
    /// The NDI emit rate (0 = genlock off): selects the [`jxs_link::JXS_FPS`] frames.
    pub emit_fps: u32,
}

/// Entry point for the capture loop.
pub struct JxsSender;

impl JxsSender {
    /// Read [`TARGET_ENV`]: unset/blank = `None` (nothing created). Set = start the sender
    /// threads; any failure is logged and the link stays off (`None`), never the capture loop.
    pub fn start_from_env(hostname: &str) -> Option<JxsHandoff> {
        let value = std::env::var(TARGET_ENV).ok();
        let bpp = std::env::var(BPP_ENV).ok();
        Self::start_with(value.as_deref(), hostname, bpp.as_deref())
    }

    /// [`Self::start_from_env`] with the target passed in and the default bpp (tests).
    pub fn start(value: Option<&str>, hostname: &str) -> Option<JxsHandoff> {
        Self::start_with(value, hostname, None)
    }

    /// [`Self::start_from_env`] with both values passed in.
    pub fn start_with(
        value: Option<&str>,
        hostname: &str,
        bpp_value: Option<&str>,
    ) -> Option<JxsHandoff> {
        let target = match jxs_link::parse_target(value) {
            Ok(None) => return None,
            Ok(Some(t)) => t,
            Err(e) => {
                tracing::error!("{e} -- the JPEG XS link stays off (issue 1387)");
                return None;
            }
        };
        let bpp = parse_bpp(bpp_value).unwrap_or_else(|e| {
            tracing::warn!("{e} -- using {DEFAULT_BPP} bpp (issue 1387)");
            DEFAULT_BPP
        });
        start_worker(target, jxs_link::camera_id_from_hostname(hostname), bpp)
    }
}

#[cfg(not(feature = "jxs"))]
fn start_worker(target: String, _camera_id: u8, _bpp: u32) -> Option<JxsHandoff> {
    tracing::error!(
        "{TARGET_ENV}={target} is set, but this camera-box was built without the `jxs` feature -- \
         the JPEG XS link stays off (issue 1387)"
    );
    None
}

#[cfg(feature = "jxs")]
fn start_worker(target: String, camera_id: u8, bpp: u32) -> Option<JxsHandoff> {
    let socket = match JxsSocket::connect(&target) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "{TARGET_ENV}={target}: {e} -- the JPEG XS link stays off (issue 1387)"
            );
            return None;
        }
    };
    let (allowed, capture, reserved) = crate::affinity::off_capture_split();
    let env_cores = std::env::var(CORES_ENV).ok();
    let cores = match select_encoder_cores(env_cores.as_deref(), &allowed) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("jxs send: {e} -- using {allowed:?} (issue 1387)");
            allowed.clone()
        }
    };
    if let Some(errno) = socket.mark_error {
        tracing::warn!(
            "jxs send: SO_MARK {JXS_SOCKET_MARK:#x} refused ({}; needs CAP_NET_ADMIN or CAP_NET_RAW) -- the cambox egress pacing paces this JPEG XS flow at 400 Mb/s with NDI instead of line rate (issue 1387)",
            std::io::Error::from_raw_os_error(errno)
        );
    }
    tracing::info!(
        "jxs send: link ON to {} (camera id {camera_id}, UDP GSO {}, egress mark {}, send buffer {} bytes) -- JPEG XS {bpp} bpp at {} fps (the strih grid, NDI stays at its emit rate), latency profile, {} frames in flight, encoder on cores {cores:?} with {} slice threads (capture core {capture:?}, capture-IRQ core {reserved:?} untouched), beside NDI (issue 1387)",
        socket
            .peer()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| target.clone()),
        if socket.gso() { "on" } else { "OFF: one send per datagram" },
        if socket.marked() {
            format!("{JXS_SOCKET_MARK:#x}: line rate past the NDI pacing")
        } else {
            "NONE: paced with NDI".to_string()
        },
        socket.send_buffer(),
        jxs_link::JXS_FPS,
        FRAMES_IN_FLIGHT,
        slice_threads_for(&cores),
    );
    handoff::spawn(socket, camera_id, cores, bpp)
}

#[cfg(feature = "jxs")]
mod handoff;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "jxs")]
    use std::sync::atomic::Ordering;

    #[test]
    fn the_link_is_off_unless_the_target_is_set() {
        assert!(JxsSender::start(None, "CAM5").is_none());
        assert!(JxsSender::start(Some(""), "CAM5").is_none());
        assert!(JxsSender::start(Some("  "), "CAM5").is_none());
        // a malformed value is logged and stays off, never a panic
        assert!(JxsSender::start(Some("strih.lan"), "CAM5").is_none());
    }

    #[test]
    fn the_encoder_cores_are_the_off_capture_cores_unless_named_inside_them() {
        // a cambox: capture 3, capture-IRQ 2, the display cores 0-1
        let allowed = [0usize, 1];
        assert_eq!(select_encoder_cores(None, &allowed), Ok(vec![0, 1]));
        assert_eq!(select_encoder_cores(Some(" "), &allowed), Ok(vec![0, 1]));
        assert_eq!(select_encoder_cores(Some("1"), &allowed), Ok(vec![1]));
        assert_eq!(
            select_encoder_cores(Some("0-1\n"), &allowed),
            Ok(vec![0, 1])
        );
        // the capture core and its IRQ core are refused, naming the core
        let e = select_encoder_cores(Some("1-2"), &allowed).unwrap_err();
        assert!(e.contains("core 2"), "{e}");
        let e = select_encoder_cores(Some("3"), &allowed).unwrap_err();
        assert!(e.contains("core 3"), "{e}");
        assert!(select_encoder_cores(Some("x"), &allowed).is_err());
    }

    #[test]
    fn the_bpp_is_a_setting_with_a_default_and_a_bound() {
        assert_eq!(parse_bpp(None), Ok(4));
        assert_eq!(parse_bpp(Some(" ")), Ok(4));
        assert_eq!(parse_bpp(Some("6")), Ok(6));
        assert_eq!(parse_bpp(Some(" 8\n")), Ok(8));
        assert_eq!(parse_bpp(Some("12")), Ok(12));
        for bad in ["0", "13", "4.5", "-4", "x"] {
            let e = parse_bpp(Some(bad)).unwrap_err();
            assert!(e.contains(BPP_ENV), "{bad}: {e}");
        }
        // a refused value keeps the link (and the default), never turns it off
        assert!(
            JxsSender::start_with(None, "CAM5", Some("x")).is_none(),
            "no target: off"
        );
    }

    #[test]
    fn a_frame_in_flight_with_no_packet_for_two_seconds_is_a_wedged_encoder() {
        assert!(
            !encoder_wedged(0, 60_000),
            "nothing in flight: an idle link is no wedge"
        );
        assert!(!encoder_wedged(1, WEDGE_MS - 1));
        assert!(encoder_wedged(1, WEDGE_MS));
        assert!(encoder_wedged(2, 10 * WEDGE_MS));
    }

    #[test]
    fn one_slice_thread_per_encoder_core_within_bounds() {
        assert_eq!(
            slice_threads_for(&[0, 1]),
            2,
            "the N150: 2 cores, 2 slice threads"
        );
        assert_eq!(slice_threads_for(&[]), 1);
        assert_eq!(
            slice_threads_for(&[0, 1, 2, 3, 4, 5, 6, 7]),
            MAX_SLICE_THREADS as u32
        );
    }

    #[test]
    fn the_status_counters_drain_into_the_window_and_reset() {
        let s = Stats::default();
        s.dropped_busy.store(3, Ordering::Relaxed);
        s.note_convert(400_000);
        s.note_convert(600_000);
        s.in_flight.store(1, Ordering::Relaxed);
        s.in_flight_max.store(2, Ordering::Relaxed);
        let mut w = jxs_link::SendWindow::default();
        s.drain_into(&mut w);
        assert_eq!(w.dropped_busy, 3);
        assert_eq!((w.convert_count, w.convert_ms_max), (2, 0.6));
        assert!((w.convert_ms_sum - 1.0).abs() < 1e-9);
        assert_eq!(
            (w.in_flight_max, w.in_flight_cap),
            (2, FRAMES_IN_FLIGHT as u64)
        );
        let mut again = jxs_link::SendWindow::default();
        s.drain_into(&mut again);
        assert_eq!(again.dropped_busy, 0);
        assert_eq!(again.convert_count, 0);
        assert_eq!(
            again.in_flight_max, 1,
            "the next window starts from what is in flight now"
        );
    }

    /// A 60 fps emit stamp of slot `k` in second 1_761_730_000 (29.10.2026).
    #[cfg(feature = "jxs")]
    fn tc(k: i64) -> i64 {
        1_761_730_000 * 10_000_000 + k * 10_000_000 / 60
    }

    #[cfg(feature = "jxs")]
    fn jf(data: &[u8], w: u32, h: u32, timecode_100ns: i64, yuyv: bool) -> JxsFrame<'_> {
        JxsFrame {
            data,
            width: w,
            height: h,
            stride: w * 2,
            yuyv,
            timecode_100ns,
            emit_fps: 60,
        }
    }

    #[cfg(feature = "jxs")]
    #[test]
    fn only_the_frames_on_the_strih_30_fps_grid_are_taken() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        for k in [1, 3] {
            h.offer(&jf(&frame, 64, 8, tc(k), true));
        }
        assert!(ready.try_recv().is_err(), "odd slots never go");
        assert_eq!(
            h.stats.convert_count.load(Ordering::Relaxed),
            0,
            "not even converted"
        );
        h.offer(&jf(&frame, 64, 8, tc(2), true));
        assert_eq!(ready.try_recv().unwrap().timecode_100ns, tc(2));
        assert_eq!(h.dropped_busy(), 0, "a skipped odd frame is no drop");
    }

    #[cfg(feature = "jxs")]
    #[test]
    fn two_frames_are_taken_and_the_third_is_dropped_without_waiting() {
        let (h, ready) = JxsHandoff::unstarted();
        let (w, ht) = (64u32, 8u32);
        let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i % 251) as u8).collect();
        // the ready queue is never drained here: a waiting offer would hang the test
        for k in [0, 2, 4] {
            h.offer(&jf(&frame, w, ht, tc(k), true));
        }
        assert_eq!(
            h.dropped_busy(),
            1,
            "two pictures in flight, the third frame dropped"
        );
        let a = ready.try_recv().expect("first");
        let b = ready.try_recv().expect("second");
        assert_eq!((a.timecode_100ns, b.timecode_100ns), (tc(0), tc(2)));
        // converted on hand-off: planar Y is every other source byte
        assert_eq!(
            a.pic.planes().y()[..4],
            [frame[0], frame[2], frame[4], frame[6]]
        );
        assert_eq!(a.pic.planes().u()[0], frame[1]);
        assert_eq!(a.pic.planes().v()[0], frame[3]);
        assert!(b.handed_off >= a.handed_off);
    }

    #[cfg(feature = "jxs")]
    #[test]
    fn a_returned_picture_is_reused_without_reallocating() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(0), true));
        let first = ready.try_recv().unwrap();
        let ptr = first.pic.planes().y().as_ptr();
        h.free_tx.try_send(first.pic).unwrap();
        // the other picture is taken first (FIFO), then the returned one
        h.offer(&jf(&frame, 64, 8, tc(2), true));
        h.offer(&jf(&frame, 64, 8, tc(4), true));
        let _second = ready.try_recv().unwrap();
        let third = ready.try_recv().unwrap();
        assert_eq!(third.timecode_100ns, tc(4));
        assert_eq!(
            third.pic.planes().y().as_ptr(),
            ptr,
            "same buffer, no new allocation"
        );
        assert_eq!(h.dropped_busy(), 0);
    }

    /// `in_flight` counts a frame from its submit to its LAST packet (the slice-1b dev1 flood
    /// bench read `in_flight=1/2` while two frames were encoding: the count dropped when the
    /// packet thread picked the frame up, not when it was done).
    #[cfg(feature = "jxs")]
    #[test]
    fn two_frames_handed_over_back_to_back_are_both_in_flight() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        // room for both frames' bursts on loopback (the kernel caps it at net.core.rmem_max)
        set_int_opt(rx.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, 4 << 20);
        let port = rx.local_addr().unwrap().port();
        // both frames are converted and queued BEFORE the threads start, so the feeder hands
        // the second over microseconds after the first, whatever a debug build's conversion costs
        let (mut h, ready_rx) = JxsHandoff::unstarted();
        let (w, ht) = (1920u32, 544u32);
        let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i % 249) as u8).collect();
        h.offer(&jf(&frame, w, ht, tc(0), true));
        h.offer(&jf(&frame, w, ht, tc(2), true));
        assert_eq!(h.dropped_busy(), 0);
        let socket = JxsSocket::connect(&format!("127.0.0.1:{port}")).unwrap();
        assert!(h.start_threads(ready_rx, socket, 5, vec![0, 1], DEFAULT_BPP));
        // wait for both frames' last slice on the wire
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut b = [0u8; 2048];
        let mut done = std::collections::BTreeSet::new();
        while done.len() < 2 && std::time::Instant::now() < deadline {
            if let Ok(n) = rx.recv(&mut b) {
                let p = jxs_link::PacketHeader::decode(&b[..n]).unwrap();
                if p.slice_index + 1 == p.slice_count {
                    done.insert(p.timecode_100ns);
                }
            }
        }
        assert_eq!(done.len(), 2, "both frames sent");
        // the packet thread counts a frame out right after its last send
        while h.stats.in_flight.load(Ordering::Relaxed) > 0 && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            h.stats.in_flight.load(Ordering::Relaxed),
            0,
            "both frames finished"
        );
        assert_eq!(
            h.stats.in_flight_max.load(Ordering::Relaxed),
            2,
            "the second frame was submitted while the first one's slices were still going out"
        );
    }

    #[cfg(feature = "jxs")]
    #[test]
    fn a_frame_that_is_not_yuyv_or_does_not_fit_never_holds_a_picture() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(0), false));
        h.offer(&jf(&frame[..100], 64, 8, tc(2), true));
        assert!(ready.try_recv().is_err());
        assert!(h.stats.not_yuyv.load(Ordering::Relaxed));
        assert_eq!(h.stats.convert_errors.load(Ordering::Relaxed), 1);
        // both pictures are still free
        h.offer(&jf(&frame, 64, 8, tc(4), true));
        h.offer(&jf(&frame, 64, 8, tc(6), true));
        assert_eq!(h.dropped_busy(), 0);
    }

    /// Whether this process may set SO_MARK: CAP_NET_ADMIN (bit 12) or CAP_NET_RAW (bit 13) in its
    /// effective set, read from /proc/self/status (the kernel's own check, never the euid).
    fn may_set_socket_mark() -> bool {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let hex = status
            .lines()
            .find_map(|l| l.strip_prefix("CapEff:"))
            .expect("CapEff line")
            .trim();
        let caps = u64::from_str_radix(hex, 16).unwrap();
        caps & ((1 << 12) | (1 << 13)) != 0
    }

    #[test]
    fn the_socket_carries_the_egress_mark_whenever_the_kernel_allows_it_1387() {
        let tx = JxsSocket::connect("127.0.0.1:9").unwrap();
        let got = get_int_opt(tx.sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_MARK);
        // The report matches the socket either way: marked == the mark is really on it.
        assert_eq!(
            tx.marked(),
            got == JXS_SOCKET_MARK as i64,
            "SO_MARK read {got}"
        );
        if may_set_socket_mark() {
            // camera-box.service (root) holds the capability: the mark is never missing there.
            assert!(tx.marked(), "a capable socket carries the mark");
        } else {
            // The setsockopt WAS attempted and the kernel refused it: a connect that never tried
            // the mark would leave no error here (review round 1: the unprivileged CI runner).
            assert_eq!(
                tx.mark_error,
                Some(libc::EPERM),
                "SO_MARK attempted and refused"
            );
        }
    }

    #[test]
    fn an_ipv6_target_is_refused() {
        assert!(JxsSocket::connect("[::1]:7000").is_err());
    }

    #[cfg(not(feature = "jxs"))]
    #[test]
    fn a_build_without_the_codec_stays_off_even_with_a_target() {
        assert!(JxsSender::start(Some("127.0.0.1:9"), "CAM5").is_none());
    }

    #[test]
    fn a_slice_crosses_loopback_as_mtu_datagrams() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = rx.local_addr().unwrap().port();
        let tx = JxsSocket::connect(&format!("127.0.0.1:{port}")).unwrap();
        assert!(tx.gso(), "UDP_SEGMENT is accepted (Linux >= 4.18)");
        let slice: Vec<u8> = (0..20_000u32).map(|i| (i % 253) as u8).collect();
        let mut buf = Vec::new();
        let base = jxs_link::PacketHeader {
            camera_id: 2,
            timecode_100ns: 42,
            slice_index: 1,
            slice_count: 2,
            offset: 0,
            slice_len: 0,
        };
        let n = jxs_link::build_slice_datagrams(base, &slice, &mut buf).unwrap();
        assert_eq!(tx.send_slice(&buf), 0);
        let mut rebuilt = vec![0u8; slice.len()];
        let mut got = 0;
        let mut d = [0u8; 2048];
        for _ in 0..n {
            let len = rx.recv(&mut d).unwrap();
            assert!(
                len <= DATAGRAM_LEN,
                "{len}: the receiver sees MTU datagrams"
            );
            let h = jxs_link::PacketHeader::decode(&d[..len]).unwrap();
            let o = h.offset as usize;
            rebuilt[o..o + len - jxs_link::HEADER_LEN]
                .copy_from_slice(&d[jxs_link::HEADER_LEN..len]);
            got += 1;
        }
        assert_eq!(got, n);
        assert_eq!(rebuilt, slice);
    }
}
