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
//!   the picture back. With a pace rate (`SenderOptions::pace_mbps`, [`PACE_ENV`] on a cambox,
//!   default 0 = line rate) it holds each send until its departure on the pace schedule
//!   (`crate::jxs_link::pace`): the waiting happens on this thread only.
//!
//! Every 5 s it logs the `jxs send: fps= ...` status line (its fields:
//! `jxs_link::SendWindow::status_line`). A frame in flight with no packet for [`WEDGE_MS`] logs one
//! `CRITICAL jxs send: encoder wedged` line from the feeder (the status line lives on the packet
//! thread, which is the one stuck).
//!
//! The codec needs the `jxs` cargo feature (CI's deployed `camera-box-linux-amd64` carries it).
//! A build without it logs one ERROR when the target is set and stays off.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

use crate::jxs_link;

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

/// The `IP_TOS` byte of every JPEG XS datagram (and of the receiver reports): DSCP AF41 (34, the
/// byte 34 x 4 = 0x88, ECN bits 0), the interactive-video class of RFC 4594 and RFC 8837 (WebRTC
/// video). Not EF (46): that is the voice class, and a switch serving EF from a strict-priority
/// queue would starve every other flow under a 500 Mb/s video stream. A marking only: a switch
/// that trusts DSCP queues the link apart from bulk traffic, one that does not ignores it
/// (issue 1387; beside `SO_MARK` [`JXS_SOCKET_MARK`], which never leaves the host).
pub const JXS_IP_TOS: libc::c_int = 0x88;
/// The DSCP code point inside [`JXS_IP_TOS`] (its upper six bits).
pub const JXS_DSCP: u8 = 34;
const _: () = assert!(JXS_IP_TOS == JXS_DSCP as libc::c_int * 4);

/// `UDP_SEGMENT` / `UDP_GRO` (linux/udp.h; spelled here so an older `libc` cannot lack them).
pub const UDP_SEGMENT: libc::c_int = 103;
pub const UDP_GRO: libc::c_int = 104;

/// Frames the sender holds at once (slice 1b): one whose slices are coming out, one the encoder
/// already started. A third frame while both are busy is `dropped_busy`.
pub const FRAMES_IN_FLIGHT: usize = 2;
/// The environment variable that names the encoder's cores explicitly (a cpulist, `0-1`); it must
/// lie inside the off-capture cores (issue 1198 split), else it is ignored with a WARN.
pub const CORES_ENV: &str = "CAMERA_BOX_JXS_CORES";
/// The most slice-pack threads the encoder runs (a 1080p frame has 68 slices).
pub const MAX_SLICE_THREADS: usize = 4;
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
    /// Frames dropped busy since the automatic bpp's last one-second tick (issue 1387).
    dropped_tick: AtomicU64,
    /// The hand-off ages of this status window: `[timecode -> hand-off, capture instant ->
    /// hand-off]`. The capture thread only `try_lock`s it (a sample is skipped rather than ever
    /// waiting); the status line holds it for a copy.
    handoff_age: std::sync::Mutex<[jxs_link::HandoffAge; 2]>,
    /// The stamp of the last frame the hand-off TOOK (0 = none yet): the capture thread's only
    /// state for [`jxs_link::jxs_fill_slot`] (issue 1387).
    last_taken_100ns: AtomicI64,
    /// Frames sent on a 30 fps slot the capture skipped, since the last status line.
    slot_fills: AtomicU64,
}

#[cfg_attr(not(feature = "jxs"), allow(dead_code))]
impl Stats {
    /// A frame dropped because every picture was busy (the status line and the auto bpp).
    fn note_dropped(&self) {
        self.dropped_busy.fetch_add(1, Ordering::Relaxed);
        self.dropped_tick.fetch_add(1, Ordering::Relaxed);
    }

    /// The frame stamped `timecode_100ns`, captured at `capture_100ns` (0 = unknown), is handed
    /// over at wall clock `now_100ns` (the capture thread; never blocks, never allocates).
    fn note_handoff(&self, timecode_100ns: i64, capture_100ns: i64, now_100ns: i64) {
        if let Ok(mut a) = self.handoff_age.try_lock() {
            a[0].note(jxs_link::handoff_age_100ns(timecode_100ns, now_100ns));
            if capture_100ns > 0 {
                a[1].note(jxs_link::handoff_age_100ns(capture_100ns, now_100ns));
            }
        }
    }

    fn note_convert(&self, ns: u64) {
        self.convert_count.fetch_add(1, Ordering::Relaxed);
        self.convert_ns_sum.fetch_add(ns, Ordering::Relaxed);
        self.convert_ns_max.fetch_max(ns, Ordering::Relaxed);
    }

    /// Move the shared counters into `w` and reset them (the in-flight max restarts from now).
    fn drain_into(&self, w: &mut jxs_link::SendWindow) {
        w.dropped_busy = self.dropped_busy.swap(0, Ordering::Relaxed);
        w.slot_fills = self.slot_fills.swap(0, Ordering::Relaxed);
        w.encode_errors += self.convert_errors.swap(0, Ordering::Relaxed);
        w.convert_count = self.convert_count.swap(0, Ordering::Relaxed);
        w.convert_ms_sum = self.convert_ns_sum.swap(0, Ordering::Relaxed) as f64 / 1e6;
        w.convert_ms_max = self.convert_ns_max.swap(0, Ordering::Relaxed) as f64 / 1e6;
        let now = self.in_flight.load(Ordering::Relaxed);
        w.in_flight_max = self.in_flight_max.swap(now, Ordering::Relaxed) as u64;
        w.in_flight_cap = FRAMES_IN_FLIGHT as u64;
        if let Ok(mut a) = self.handoff_age.lock() {
            let [stamp, capture] = std::mem::take(&mut *a);
            w.handoff_age = stamp;
            w.capture_age = capture;
        }
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

    pub fn offer_raw(&self, _frame: &RawFrame<'_>) -> Offered {
        match self.never {}
    }

    pub fn take_window(&self) -> Option<(u64, jxs_link::SendWindow)> {
        match self.never {}
    }
}

/// The cambox link as the capture loop holds it ([`JxsSender::start_from_env`]): the hand-off
/// once the link socket connected. A socket that cannot connect yet (no route while the network
/// comes up after a power cycle or a move) is retried on its own thread, never given up (issue
/// 1387, 1.11.2026: every SNV camera came back black on JPEG XS after a reboot while NDI
/// recovered). Until then every frame is skipped; NDI and the capture never wait for it.
pub struct JxsLink {
    ready: std::cell::OnceCell<JxsHandoff>,
    pending: Option<std::sync::mpsc::Receiver<JxsHandoff>>,
}

impl JxsLink {
    #[cfg_attr(not(feature = "jxs"), allow(dead_code))]
    fn ready(h: JxsHandoff) -> Self {
        let ready = std::cell::OnceCell::new();
        let _ = ready.set(h);
        Self {
            ready,
            pending: None,
        }
    }

    #[cfg_attr(not(feature = "jxs"), allow(dead_code))]
    fn pending(rx: std::sync::mpsc::Receiver<JxsHandoff>) -> Self {
        Self {
            ready: std::cell::OnceCell::new(),
            pending: Some(rx),
        }
    }

    /// The hand-off once the link is up; `None` while its socket still retries (one channel
    /// read, never a wait).
    pub fn handoff(&self) -> Option<&JxsHandoff> {
        if let Some(h) = self.ready.get() {
            return Some(h);
        }
        let h = self.pending.as_ref()?.try_recv().ok()?;
        Some(self.ready.get_or_init(|| h))
    }

    /// [`JxsHandoff::offer`] once the link is up; before that the frame is skipped.
    pub fn offer(&self, frame: &JxsFrame<'_>) {
        if let Some(h) = self.handoff() {
            h.offer(frame);
        }
    }
}

/// How often a link socket that cannot connect yet is tried again.
pub const CONNECT_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether a failed link-socket connect is worth another try: everything but a target the link
/// can never use (an IPv6 address, `Unsupported`). No route, no address yet (the network or the
/// resolver still coming up) and every other socket error retry.
pub fn connect_error_retries(e: &std::io::Error) -> bool {
    e.kind() != std::io::ErrorKind::Unsupported
}

/// One emitted frame as the capture loop hands it over.
#[derive(Debug, Clone, Copy)]
pub struct JxsFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub yuyv: bool,
    /// The genlock timecode the NDI frame carries (the emit-grid boundary AT OR BEFORE the
    /// capture instant).
    pub timecode_100ns: i64,
    /// The frame's RAW capture instant (its V4L2 stamp mapped to the same wall clock, never the
    /// phase tracker's slot middle; 0 = unknown): with the timecode it splits `handoff_age_ms`
    /// into where the capture sits against its stamped slot and the capture -> hand-off time.
    pub capture_realtime_100ns: i64,
    /// The NDI emit rate (0 = genlock off): selects the [`jxs_link::JXS_FPS`] frames.
    pub emit_fps: u32,
}

/// Entry point for the capture loop.
pub struct JxsSender;

impl JxsSender {
    /// Read [`TARGET_ENV`]: unset/blank = `None` (nothing created). Set = start the sender
    /// threads; a socket that cannot connect yet retries every [`CONNECT_RETRY`] ([`JxsLink`]),
    /// any other failure is logged and the link stays off (`None`), never the capture loop.
    pub fn start_from_env(hostname: &str) -> Option<JxsLink> {
        let env = |k: &str| std::env::var(k).ok();
        let value = env(TARGET_ENV);
        if jxs_link::parse_target(value.as_deref()) == Ok(None) {
            return None;
        }
        let (bpp, warnings) = parse_bpp_mode(
            env(BPP_ENV).as_deref(),
            env(BPP_MIN_ENV).as_deref(),
            env(BPP_MAX_ENV).as_deref(),
        );
        for w in warnings {
            tracing::warn!("jxs send: {w} (issue 1387)");
        }
        let fifo = parse_fifo_prio(env(FIFO_ENV).as_deref()).unwrap_or_else(|e| {
            tracing::warn!("jxs send: {e} -- the encoder stays SCHED_OTHER (issue 1387)");
            None
        });
        let pace = parse_pace_mbps(env(PACE_ENV).as_deref()).unwrap_or_else(|e| {
            tracing::warn!("jxs send: {e} -- sending at line rate (issue 1387)");
            0
        });
        Self::start_retrying(
            value.as_deref(),
            hostname,
            SenderOptions::new(bpp, fifo).with_pace(pace),
            JxsSocket::connect,
            CONNECT_RETRY,
        )
    }

    /// [`Self::start_from_env`] with the target passed in and the default bpp (tests).
    pub fn start(value: Option<&str>, hostname: &str) -> Option<JxsHandoff> {
        Self::start_with(value, hostname, None)
    }

    /// The target and [`BPP_ENV`] passed in (automatic bpp when it is `None`), SCHED_OTHER.
    pub fn start_with(
        value: Option<&str>,
        hostname: &str,
        bpp_value: Option<&str>,
    ) -> Option<JxsHandoff> {
        let (bpp, warnings) = parse_bpp_mode(bpp_value, None, None);
        for w in warnings {
            tracing::warn!("jxs send: {w} (issue 1387)");
        }
        Self::start_with_options(value, hostname, SenderOptions::new(bpp, None))
    }

    /// The target and the options passed in.
    pub fn start_with_options(
        value: Option<&str>,
        hostname: &str,
        opts: SenderOptions,
    ) -> Option<JxsHandoff> {
        let target = match jxs_link::parse_target(value) {
            Ok(None) => return None,
            Ok(Some(t)) => t,
            Err(e) => {
                tracing::error!("{e} -- the JPEG XS link stays off (issue 1387)");
                return None;
            }
        };
        start_worker(target, jxs_link::camera_id_from_hostname(hostname), opts)
    }

    /// The cambox entry: like [`Self::start_with_options`], but a socket that cannot connect yet
    /// is tried again every `retry` on its own thread ([`JxsLink`]); `connect` is
    /// [`JxsSocket::connect`] in production (a test injects a failing one).
    pub fn start_retrying<C>(
        value: Option<&str>,
        hostname: &str,
        opts: SenderOptions,
        connect: C,
        retry: std::time::Duration,
    ) -> Option<JxsLink>
    where
        C: FnMut(&str) -> std::io::Result<JxsSocket> + Send + 'static,
    {
        let target = match jxs_link::parse_target(value) {
            Ok(None) => return None,
            Ok(Some(t)) => t,
            Err(e) => {
                tracing::error!("{e} -- the JPEG XS link stays off (issue 1387)");
                return None;
            }
        };
        start_link_worker(
            target,
            jxs_link::camera_id_from_hostname(hostname),
            opts,
            connect,
            retry,
        )
    }
}

#[cfg(not(feature = "jxs"))]
fn start_worker(target: String, _camera_id: u8, _opts: SenderOptions) -> Option<JxsHandoff> {
    tracing::error!(
        "{TARGET_ENV}={target} is set, but this camera-box was built without the `jxs` feature -- \
         the JPEG XS link stays off (issue 1387)"
    );
    None
}

#[cfg(not(feature = "jxs"))]
fn start_link_worker<C>(
    target: String,
    camera_id: u8,
    opts: SenderOptions,
    _connect: C,
    _retry: std::time::Duration,
) -> Option<JxsLink> {
    start_worker(target, camera_id, opts).map(JxsLink::ready)
}

#[cfg(feature = "jxs")]
fn start_worker(target: String, camera_id: u8, opts: SenderOptions) -> Option<JxsHandoff> {
    let socket = match JxsSocket::connect(&target) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "{TARGET_ENV}={target}: {e} -- the JPEG XS link stays off (issue 1387)"
            );
            return None;
        }
    };
    start_with_socket(socket, &target, camera_id, opts)
}

#[cfg(feature = "jxs")]
fn start_link_worker<C>(
    target: String,
    camera_id: u8,
    opts: SenderOptions,
    mut connect: C,
    retry: std::time::Duration,
) -> Option<JxsLink>
where
    C: FnMut(&str) -> std::io::Result<JxsSocket> + Send + 'static,
{
    let first = match connect(&target) {
        Ok(socket) => {
            return start_with_socket(socket, &target, camera_id, opts).map(JxsLink::ready)
        }
        Err(e) if !connect_error_retries(&e) => {
            tracing::error!(
                "{TARGET_ENV}={target}: {e} -- the JPEG XS link stays off (issue 1387)"
            );
            return None;
        }
        Err(e) => e,
    };
    tracing::warn!(
        "{TARGET_ENV}={target}: {first} -- the JPEG XS link retries every {} ms until it connects, NDI is not affected (issue 1387)",
        retry.as_millis()
    );
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let shown = target.clone();
    let spawned = std::thread::Builder::new()
        .name("jxs-connect".to_string())
        .spawn(move || {
            let mut last = first.to_string();
            let mut tries: u64 = 1;
            loop {
                std::thread::sleep(retry);
                tries += 1;
                match connect(&target) {
                    Ok(socket) => {
                        tracing::info!(
                            "jxs send: {target} connected after {tries} tries (issue 1387)"
                        );
                        if let Some(h) = start_with_socket(socket, &target, camera_id, opts) {
                            // a dropped link (camera-box stopping) drops the hand-off here
                            let _ = tx.send(h);
                        }
                        return;
                    }
                    Err(e) if !connect_error_retries(&e) => {
                        tracing::error!(
                            "{TARGET_ENV}={target}: {e} -- the JPEG XS link stays off (issue 1387)"
                        );
                        return;
                    }
                    Err(e) => {
                        let now = e.to_string();
                        if now != last {
                            tracing::warn!(
                                "{TARGET_ENV}={target}: {now} -- still retrying ({tries} tries) (issue 1387)"
                            );
                            last = now;
                        }
                    }
                }
            }
        });
    match spawned {
        Ok(_) => Some(JxsLink::pending(rx)),
        Err(e) => {
            tracing::error!(
                "{TARGET_ENV}={shown}: cannot start the retry thread: {e} -- the JPEG XS link stays off (issue 1387)"
            );
            None
        }
    }
}

/// The threads of a link whose socket connected.
#[cfg(feature = "jxs")]
fn start_with_socket(
    socket: JxsSocket,
    target: &str,
    camera_id: u8,
    opts: SenderOptions,
) -> Option<JxsHandoff> {
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
        "jxs send: link ON to {} (camera id {camera_id}, UDP GSO {}, egress mark {}, send buffer {} bytes) -- JPEG XS {} at {} fps (the strih grid, NDI stays at its emit rate), latency profile, {} frames in flight, encoder on cores {cores:?} with {} slice threads ({}) (capture core {capture:?}, capture-IRQ core {reserved:?} untouched), send {}, beside NDI (issue 1387)",
        socket
            .peer()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| target.to_string()),
        if socket.gso() { "on" } else { "OFF: one send per datagram" },
        socket.egress_text(),
        socket.send_buffer(),
        bpp_mode_text(opts.bpp),
        jxs_link::JXS_FPS,
        FRAMES_IN_FLIGHT,
        slice_threads_for(&cores),
        opts.fifo
            .map(|p| format!(
                "SCHED_FIFO {p}, below the capture's {}",
                crate::affinity::CAPTURE_FIFO_PRIORITY
            ))
            .unwrap_or_else(|| "SCHED_OTHER".to_string()),
        pace_text(opts.pace_mbps),
    );
    handoff::spawn(socket, camera_id, cores, opts)
}

#[cfg(feature = "jxs")]
mod control;
#[cfg(feature = "jxs")]
mod handoff;
#[cfg(feature = "jxs")]
mod raw_intake;

mod options;
pub use options::*;

mod raw;
pub use raw::{Offered, RawFrame, RawLayout};

mod link;
pub use link::LinkConfig;

mod socket;
pub(crate) use socket::{get_int_opt, set_int_opt};
pub use socket::{JxsSocket, SliceSent};

mod pace_control;
pub use pace_control::PaceControl;

mod pacing;
#[cfg(feature = "jxs")]
use pacing::tighten_timer_slack;
pub use pacing::{monotonic_ns, sleep_until_monotonic_ns};

#[cfg(test)]
mod auto_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "jxs")]
    use std::net::UdpSocket;
    #[cfg(feature = "jxs")]
    use std::os::fd::AsRawFd;
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
            capture_realtime_100ns: 0,
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

    /// Issue 1387, the 1.11.2026 night soak: cam2's source runs slower than the grid, so the
    /// capture skips a 60 fps slot at every edge crossing and NDI gets ONE starvation repeat of
    /// the current frame stamped on it. A skipped EVEN slot left the 30 fps link without a
    /// frame and strih repeated the previous one (`stamp_gap` + `late_holds`). The odd-slot frame
    /// right after the hole fills it, stamped on the hole, like the NDI repeat.
    #[cfg(feature = "jxs")]
    #[test]
    fn the_frame_after_a_skipped_even_slot_fills_it() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(0), true));
        h.offer(&jf(&frame, 64, 8, tc(1), true));
        assert_eq!(ready.try_recv().unwrap().timecode_100ns, tc(0));
        assert!(
            ready.try_recv().is_err(),
            "an odd slot after a taken even one never goes"
        );
        // slot 2 skipped by the capture; slot 3 arrives
        h.offer(&jf(&frame, 64, 8, tc(3), true));
        let filled = ready.try_recv().expect("the hole is filled");
        assert_eq!(filled.timecode_100ns, tc(2), "stamped on the skipped slot");
        // the next even slot goes as before, and its odd neighbour never does
        h.free_tx.try_send(filled.pic).unwrap();
        h.offer(&jf(&frame, 64, 8, tc(4), true));
        h.offer(&jf(&frame, 64, 8, tc(5), true));
        assert_eq!(ready.try_recv().unwrap().timecode_100ns, tc(4));
        assert!(ready.try_recv().is_err());
        assert_eq!(h.dropped_busy(), 0);
    }

    /// Every fill is counted on the 5 s status line (`slot_fills=`), a busy-dropped even frame's
    /// slot is filled like a skipped one, and a fill that finds every picture busy is no fill.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_fill_is_counted_and_a_busy_dropped_even_frame_is_filled_too() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(0), true));
        h.offer(&jf(&frame, 64, 8, tc(2), true));
        // both pictures in flight: slot 4 is dropped busy; one comes back, slot 5 fills slot 4
        h.offer(&jf(&frame, 64, 8, tc(4), true));
        assert_eq!(h.dropped_busy(), 1);
        let a = ready.try_recv().unwrap();
        assert_eq!(a.timecode_100ns, tc(0));
        h.free_tx.try_send(a.pic).unwrap();
        h.offer(&jf(&frame, 64, 8, tc(5), true));
        let b = ready.try_recv().unwrap();
        let c = ready.try_recv().unwrap();
        assert_eq!(b.timecode_100ns, tc(2));
        assert_eq!(
            c.timecode_100ns,
            tc(4),
            "the busy-dropped even slot is filled"
        );
        // both out again: slot 6 dropped busy, and slot 7's fill finds no picture either
        h.offer(&jf(&frame, 64, 8, tc(6), true));
        h.offer(&jf(&frame, 64, 8, tc(7), true));
        assert_eq!(h.dropped_busy(), 3);
        assert!(ready.try_recv().is_err(), "a busy fill is no fill");
        // pictures back; a capture-skipped slot 10 is filled from slot 11
        h.free_tx.try_send(b.pic).unwrap();
        h.free_tx.try_send(c.pic).unwrap();
        h.offer(&jf(&frame, 64, 8, tc(8), true));
        h.offer(&jf(&frame, 64, 8, tc(11), true));
        let d = ready.try_recv().unwrap();
        let e = ready.try_recv().unwrap();
        assert_eq!((d.timecode_100ns, e.timecode_100ns), (tc(8), tc(10)));
        let mut w = jxs_link::SendWindow::default();
        h.stats.drain_into(&mut w);
        assert_eq!(w.slot_fills, 2, "the two fills that were taken");
        assert!(
            w.status_line().contains(" slot_fills=2 "),
            "{}",
            w.status_line()
        );
        let mut again = jxs_link::SendWindow::default();
        h.stats.drain_into(&mut again);
        assert_eq!(again.slot_fills, 0, "the window resets it");
    }

    /// A fill keeps `handoff_age` about the frame's OWN slot (review of the slot fill): sampled
    /// against the hole's stamp, a fill read one emit slot (16.7 ms) older, so `handoff_age -
    /// capture_age` (the capture's phase in its slot) left its 0..16.7 ms band for that sample.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_fill_samples_the_handoff_age_against_the_frames_own_slot() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(0), true));
        // slot 2 skipped; slot 3 fills it
        h.offer(&jf(&frame, 64, 8, tc(3), true));
        assert_eq!(ready.try_recv().unwrap().timecode_100ns, tc(0));
        assert_eq!(ready.try_recv().unwrap().timecode_100ns, tc(2));
        let mut w = jxs_link::SendWindow::default();
        h.stats.drain_into(&mut w);
        assert_eq!(w.handoff_age.count, 2);
        // the two offers run microseconds apart: their ages differ by the two frames' own
        // stamps (slots 0 and 3, 50 ms), never by slot 0 and the hole (33.3 ms)
        let spread = w.handoff_age.max_100ns - w.handoff_age.min_100ns;
        assert!(
            (spread - 500_000).abs() < 50_000,
            "age spread {spread} x100 ns, want ~500000 (the frame's own slot 3)"
        );
    }

    /// No fill without a taken frame exactly one 30 fps slot before the hole: a start, a longer
    /// hole (a re-latch, a device hole) or a clock step keeps today's behaviour.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_start_or_a_longer_hole_is_never_filled() {
        let (h, ready) = JxsHandoff::unstarted();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&jf(&frame, 64, 8, tc(3), true));
        assert!(
            ready.try_recv().is_err(),
            "nothing taken yet: no fill at a start"
        );
        h.offer(&jf(&frame, 64, 8, tc(4), true));
        let first = ready.try_recv().unwrap();
        assert_eq!(first.timecode_100ns, tc(4));
        h.free_tx.try_send(first.pic).unwrap();
        // slots 5..=8 skipped: slot 9's even slot 8 is two 30 fps slots after the last taken
        h.offer(&jf(&frame, 64, 8, tc(9), true));
        assert!(ready.try_recv().is_err(), "a longer hole is not filled");
        // a clock step back
        h.offer(&jf(&frame, 64, 8, tc(-57), true));
        assert!(ready.try_recv().is_err());
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
        assert!(h.start_threads(
            ready_rx,
            socket,
            5,
            vec![0, 1],
            SenderOptions::new(BppMode::Fixed(DEFAULT_BPP), None)
        ));
        // wait for both frames' last slice on the wire
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut b = [0u8; 2048];
        let mut done = std::collections::BTreeSet::new();
        while done.len() < 2 && std::time::Instant::now() < deadline {
            if let Ok(n) = rx.recv(&mut b) {
                if b[0] == jxs_link::fec::FEC_MAGIC {
                    continue;
                }
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

    /// Issue 1387, 1.11.2026 SNV event: the camboxes booted before their network, the link
    /// socket's connect failed with "Network is unreachable" and the link stayed off for good
    /// (every strih camera black on JPEG XS while NDI recovered). A target that cannot connect
    /// yet must be tried again until it connects, and its frames must then reach the receiver.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_target_that_cannot_connect_yet_is_retried_until_its_frames_arrive() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        set_int_opt(rx.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, 4 << 20);
        let target = format!("127.0.0.1:{}", rx.local_addr().unwrap().port());
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let tries = attempts.clone();
        let link = JxsSender::start_retrying(
            Some(&target),
            "CAM5",
            SenderOptions::new(BppMode::Fixed(DEFAULT_BPP), None),
            move |t: &str| {
                if tries.fetch_add(1, Ordering::SeqCst) < 3 {
                    Err(std::io::Error::from_raw_os_error(libc::ENETUNREACH))
                } else {
                    JxsSocket::connect(t)
                }
            },
            std::time::Duration::from_millis(10),
        )
        .expect("a target that cannot connect yet keeps the link waiting, never off");
        let (w, ht) = (640u32, 128u32);
        let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i % 249) as u8).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut b = [0u8; 2048];
        let mut k = 0;
        let mut got = None;
        while got.is_none() && std::time::Instant::now() < deadline {
            link.offer(&jf(&frame, w, ht, tc(k), true));
            k += 2;
            if let Ok(n) = rx.recv(&mut b) {
                if b[0] != jxs_link::fec::FEC_MAGIC {
                    got = Some(jxs_link::PacketHeader::decode(&b[..n]).unwrap());
                }
            }
        }
        let p = got.expect("frames reach the receiver once the target connects");
        assert_eq!(p.camera_id, 5);
        assert!(
            attempts.load(Ordering::SeqCst) >= 4,
            "three failed tries, then the one that connected"
        );
        assert!(link.handoff().is_some());
    }

    /// The cpus a thread of this process may run on (`/proc/self/task/<tid>/status`).
    #[cfg(feature = "jxs")]
    fn threads_named(name: &str) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            let comm = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
            if comm.trim() != name {
                continue;
            }
            let status = std::fs::read_to_string(t.path().join("status")).unwrap_or_default();
            if let Some(l) = status.lines().find(|l| l.starts_with("Cpus_allowed_list:")) {
                out.push(crate::affinity::parse_cpulist(
                    l.trim_start_matches("Cpus_allowed_list:"),
                ));
            }
        }
        out
    }

    /// Review of the reconnect fix: while the link waits for its target, the start never holds
    /// the capture loop back (`start_retrying` returns at once, `offer` skips the frame), and
    /// the `jxs-connect` thread runs OFF the capture core. camera-box.service starts the whole
    /// process on the capture core (CPUAffinity=3), and this thread outlives the start, so it
    /// would otherwise wake every second next to the FIFO capture and spawn the encoder there.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_waiting_link_never_holds_the_capture_back_and_retries_off_the_capture_core() {
        let (expected, capture, _) = crate::affinity::off_capture_split();
        assert!(
            capture.is_some() && !expected.is_empty(),
            "needs a box with more than one core: {expected:?} {capture:?}"
        );
        let open = std::sync::Arc::new(AtomicBool::new(false));
        let gate = open.clone();
        let started = std::time::Instant::now();
        let link = JxsSender::start_retrying(
            Some("127.0.0.1:9"),
            "CAM5",
            SenderOptions::new(BppMode::Fixed(DEFAULT_BPP), None),
            move |t: &str| {
                if gate.load(Ordering::SeqCst) {
                    JxsSocket::connect(t)
                } else {
                    Err(std::io::Error::from_raw_os_error(libc::ENETUNREACH))
                }
            },
            std::time::Duration::from_millis(10),
        )
        .expect("a waiting link");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "the start returned while the target still cannot connect"
        );
        assert!(
            link.handoff().is_none(),
            "no link before the target connects"
        );
        let frame = vec![1u8; 64 * 8 * 2];
        link.offer(&jf(&frame, 64, 8, tc(0), true));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut seen = threads_named("jxs-connect");
        while seen.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            seen = threads_named("jxs-connect");
        }
        assert!(!seen.is_empty(), "the retry thread runs");
        for cpus in &seen {
            assert_eq!(cpus, &expected, "jxs-connect runs off the capture core");
        }
        open.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while link.handoff().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            link.handoff().is_some(),
            "the link comes up once the target connects"
        );
    }

    /// A target the link can never use (IPv6: `Unsupported`) is not retried: one try, off.
    #[cfg(feature = "jxs")]
    #[test]
    fn a_target_the_link_can_never_use_is_tried_once_and_stays_off() {
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let tries = attempts.clone();
        let link = JxsSender::start_retrying(
            Some("127.0.0.1:9"),
            "CAM5",
            SenderOptions::new(BppMode::Fixed(DEFAULT_BPP), None),
            move |_: &str| {
                tries.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "IPv6 only",
                ))
            },
            std::time::Duration::from_millis(10),
        );
        assert!(link.is_none());
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_an_unusable_target_stops_the_retry() {
        use std::io::{Error, ErrorKind};
        assert!(connect_error_retries(&Error::from_raw_os_error(
            libc::ENETUNREACH
        )));
        assert!(connect_error_retries(&Error::from_raw_os_error(
            libc::EHOSTUNREACH
        )));
        assert!(connect_error_retries(&Error::from_raw_os_error(
            libc::EADDRNOTAVAIL
        )));
        assert!(connect_error_retries(&Error::new(
            ErrorKind::NotFound,
            "no address"
        )));
        assert!(!connect_error_retries(&Error::new(
            ErrorKind::Unsupported,
            "IPv6"
        )));
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

    #[cfg(not(feature = "jxs"))]
    #[test]
    fn a_build_without_the_codec_stays_off_even_with_a_target() {
        assert!(JxsSender::start(Some("127.0.0.1:9"), "CAM5").is_none());
    }
}
