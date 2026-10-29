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
//! Every 5 s it logs `jxs send: frames= slices= bytes= dropped_busy= in_flight=max/cap
//! convert_ms_avg/max= first_slice_ms_avg/max= encode_ms_avg/max= send_errors= encode_errors=
//! (issue 1387)`.
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

/// `UDP_SEGMENT` / `UDP_GRO` (linux/udp.h; spelled here so an older `libc` cannot lack them).
pub const UDP_SEGMENT: libc::c_int = 103;
pub const UDP_GRO: libc::c_int = 104;

/// A connected UDP socket that sends whole slices with GSO.
pub struct JxsSocket {
    sock: UdpSocket,
    gso: AtomicBool,
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
        Ok(Self {
            sock,
            gso: AtomicBool::new(gso),
        })
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
    tracing::info!(
        "jxs send: link ON to {} (camera id {camera_id}, UDP GSO {}, send buffer {} bytes) -- JPEG XS {bpp} bpp at {} fps (the strih grid, NDI stays at its emit rate), latency profile, {} frames in flight, encoder on cores {cores:?} with {} slice threads (capture core {capture:?}, capture-IRQ core {reserved:?} untouched), beside NDI (issue 1387)",
        socket
            .peer()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| target.clone()),
        if socket.gso() { "on" } else { "OFF: one send per datagram" },
        socket.send_buffer(),
        jxs_link::JXS_FPS,
        FRAMES_IN_FLIGHT,
        slice_threads_for(&cores),
    );
    handoff::spawn(socket, camera_id, cores, bpp)
}

#[cfg(feature = "jxs")]
mod handoff {
    use super::{
        jxs_link, slice_threads_for, JxsFrame, JxsSocket, Stats, FRAMES_IN_FLIGHT, STATUS_PERIOD,
    };
    use crate::jxs_link::{PacketHeader, SendWindow};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};
    use svt_jpegxs_sys::{open_pipelined, EncoderInput, EncoderOutput, EncoderSettings, Picture};

    /// A converted frame on its way to the encoder.
    pub(super) struct Ready {
        pub(super) pic: Picture,
        pub(super) timecode_100ns: i64,
        /// The hand-off instant: the frame is converted and queued for the encoder.
        pub(super) handed_off: Instant,
    }

    /// What the feeder tells the packet thread, in submit order.
    enum Job {
        /// The frames that follow belong to this (new) session.
        Session(EncoderOutput),
        /// One submitted frame: read its packets.
        Frame {
            timecode_100ns: i64,
            handed_off: Instant,
            submitted: Instant,
        },
    }

    /// The capture thread's handle. Dropping it stops and joins both sender threads.
    pub struct JxsHandoff {
        /// Free pictures (the capture thread converts into one).
        free_rx: Receiver<Picture>,
        /// Puts a picture back when the hand-off cannot take it.
        pub(super) free_tx: SyncSender<Picture>,
        ready_tx: Option<SyncSender<Ready>>,
        pub(super) stats: Arc<Stats>,
        workers: Vec<JoinHandle<()>>,
    }

    impl JxsHandoff {
        /// Hand one emitted frame over: the capture thread converts it straight into a free
        /// picture (no separate copy) and queues it. Never blocks: with every picture in use the
        /// frame is dropped and counted (`dropped_busy`). Allocation-free once each picture has
        /// grown to the frame size.
        pub fn offer(&self, frame: &JxsFrame<'_>) {
            let JxsFrame {
                data,
                width,
                height,
                stride,
                yuyv,
                timecode_100ns,
                emit_fps,
            } = *frame;
            if !jxs_link::on_jxs_grid(timecode_100ns, emit_fps) {
                return;
            }
            if !yuyv {
                self.stats.not_yuyv.store(true, Ordering::Relaxed);
                return;
            }
            let Ok(mut pic) = self.free_rx.try_recv() else {
                self.stats.dropped_busy.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let started = Instant::now();
            if pic.planes().width() != width || pic.planes().height() != height {
                pic = Picture::new(width, height);
            }
            let ok = {
                let (y, u, v) = pic.planes_mut().planes_mut();
                jxs_link::yuyv_to_planar422(
                    data,
                    stride as usize,
                    width as usize,
                    height as usize,
                    y,
                    u,
                    v,
                )
            };
            let handed_off = Instant::now();
            self.stats
                .note_convert(handed_off.duration_since(started).as_nanos() as u64);
            if !ok {
                self.stats.convert_errors.fetch_add(1, Ordering::Relaxed);
                let _ = self.free_tx.try_send(pic);
                return;
            }
            let ready = Ready {
                pic,
                timecode_100ns,
                handed_off,
            };
            let Some(tx) = &self.ready_tx else { return };
            match tx.try_send(ready) {
                Ok(()) => {}
                // the queue holds every picture there is, so it is never full; a gone feeder
                // (the thread died) just keeps the picture out of the pool: the link goes quiet
                Err(TrySendError::Full(r)) | Err(TrySendError::Disconnected(r)) => {
                    self.stats.dropped_busy.fetch_add(1, Ordering::Relaxed);
                    let _ = self.free_tx.try_send(r.pic);
                }
            }
        }

        /// Frames dropped because both pictures were in use, since the last status line.
        pub fn dropped_busy(&self) -> u64 {
            self.stats.dropped_busy.load(Ordering::Relaxed)
        }

        /// A hand-off with no sender threads (tests): offers fill the pictures, nobody drains.
        #[cfg(test)]
        pub(super) fn detached() -> (Self, Receiver<Ready>) {
            let (free_tx, free_rx) = mpsc::sync_channel(FRAMES_IN_FLIGHT);
            for _ in 0..FRAMES_IN_FLIGHT {
                free_tx.try_send(Picture::new(0, 0)).expect("room");
            }
            let (ready_tx, ready_rx) = mpsc::sync_channel(FRAMES_IN_FLIGHT);
            (
                Self {
                    free_rx,
                    free_tx,
                    ready_tx: Some(ready_tx),
                    stats: Arc::new(Stats::default()),
                    workers: Vec::new(),
                },
                ready_rx,
            )
        }
    }

    impl Drop for JxsHandoff {
        fn drop(&mut self) {
            // closing the ready queue ends the feeder, which closes the job queue and ends the
            // packet thread after the frames it still holds
            self.ready_tx = None;
            for w in self.workers.drain(..) {
                let _ = w.join();
            }
        }
    }

    pub(super) fn spawn(
        socket: JxsSocket,
        camera_id: u8,
        cores: Vec<usize>,
        bpp: u32,
    ) -> Option<JxsHandoff> {
        let stats = Arc::new(Stats::default());
        let (free_tx, free_rx) = mpsc::sync_channel(FRAMES_IN_FLIGHT);
        for _ in 0..FRAMES_IN_FLIGHT {
            free_tx.try_send(Picture::new(0, 0)).expect("room");
        }
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Ready>(FRAMES_IN_FLIGHT);
        // a session change + every frame in flight, with room to spare
        let (job_tx, job_rx) = mpsc::sync_channel::<Job>(FRAMES_IN_FLIGHT * 4);
        let mut workers = Vec::new();
        {
            let (feed_stats, feed_free, feed_cores) =
                (Arc::clone(&stats), free_tx.clone(), cores.clone());
            match std::thread::Builder::new()
                .name("jxs-feed".into())
                .spawn(move || feed(ready_rx, job_tx, feed_free, feed_stats, feed_cores, bpp))
            {
                Ok(h) => workers.push(h),
                Err(e) => {
                    tracing::error!("jxs send: thread spawn failed: {e} (issue 1387)");
                    return None;
                }
            }
        }
        {
            let (send_stats, send_free) = (Arc::clone(&stats), free_tx.clone());
            match std::thread::Builder::new()
                .name("jxs-send".into())
                .spawn(move || send(job_rx, socket, camera_id, send_free, send_stats, cores, bpp))
            {
                Ok(h) => workers.push(h),
                Err(e) => {
                    tracing::error!("jxs send: thread spawn failed: {e} (issue 1387)");
                    // the feeder ends once the hand-off below is dropped
                    drop(JxsHandoff {
                        free_rx,
                        free_tx,
                        ready_tx: Some(ready_tx),
                        stats,
                        workers,
                    });
                    return None;
                }
            }
        }
        Some(JxsHandoff {
            free_rx,
            free_tx,
            ready_tx: Some(ready_tx),
            stats,
            workers,
        })
    }

    fn pin(label: &str, cores: &[usize]) {
        #[cfg(target_os = "linux")]
        if !crate::affinity::pin_current_thread_to(cores) {
            tracing::warn!("jxs send: {label} could not be pinned to {cores:?} (issue 1387)");
        }
    }

    /// `jxs-feed`: opens the encoder (its threads inherit this thread's cores) and hands every
    /// converted picture to it the moment it is ready, so the next frame starts while the
    /// previous one's slices still go out.
    fn feed(
        ready_rx: Receiver<Ready>,
        job_tx: SyncSender<Job>,
        free_tx: SyncSender<Picture>,
        stats: Arc<Stats>,
        cores: Vec<usize>,
        bpp: u32,
    ) {
        pin("jxs-feed", &cores);
        let threads = slice_threads_for(&cores);
        let mut input: Option<EncoderInput> = None;
        // a geometry the encoder refused: never re-opened (and re-logged) per frame
        let mut refused: Option<(u32, u32)> = None;
        let mut warned_submit = false;
        while let Ok(r) = ready_rx.recv() {
            let (w, h) = (r.pic.planes().width(), r.pic.planes().height());
            if input
                .as_ref()
                .is_none_or(|i| (i.width(), i.height()) != (w, h))
            {
                input = None;
                if refused == Some((w, h)) {
                    stats.convert_errors.fetch_add(1, Ordering::Relaxed);
                    let _ = free_tx.try_send(r.pic);
                    continue;
                }
                let mut settings = EncoderSettings::issue_1387(w, h).with_slice_threads(threads);
                settings.bpp_numerator = bpp;
                match open_pipelined(settings) {
                    Ok((i, o)) => {
                        tracing::info!(
                            "jxs send: encoder open {w}x{h}, {bpp} bpp, {} bytes per frame, {threads} slice threads on cores {cores:?} (issue 1387)",
                            i.bytes_per_frame()
                        );
                        if job_tx.send(Job::Session(o)).is_err() {
                            return;
                        }
                        input = Some(i);
                    }
                    Err(e) => {
                        tracing::error!(
                            "jxs send: {e} -- {w}x{h} frames are not sent (issue 1387)"
                        );
                        refused = Some((w, h));
                        stats.convert_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = free_tx.try_send(r.pic);
                        continue;
                    }
                }
            }
            let i = input.as_ref().expect("opened above");
            let submitted = Instant::now();
            match i.submit(r.pic) {
                Ok(()) => {
                    let now = stats.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
                    stats.in_flight_max.fetch_max(now, Ordering::Relaxed);
                    let job = Job::Frame {
                        timecode_100ns: r.timecode_100ns,
                        handed_off: r.handed_off,
                        submitted,
                    };
                    if job_tx.send(job).is_err() {
                        return;
                    }
                }
                Err((pic, e)) => {
                    stats.convert_errors.fetch_add(1, Ordering::Relaxed);
                    let _ = free_tx.try_send(pic);
                    if !warned_submit {
                        tracing::error!("jxs send: {e} -- re-opening the encoder (issue 1387)");
                        warned_submit = true;
                    }
                    // a closed session (the packet thread abandoned it) re-opens on the next frame
                    input = None;
                }
            }
        }
    }

    /// `jxs-send`: reads each submitted frame's packets as the encoder produces them and puts
    /// every slice on the wire at once; gives the picture back after the last one; logs the
    /// status line every [`STATUS_PERIOD`].
    fn send(
        job_rx: Receiver<Job>,
        socket: JxsSocket,
        camera_id: u8,
        free_tx: SyncSender<Picture>,
        stats: Arc<Stats>,
        cores: Vec<usize>,
        bpp: u32,
    ) {
        pin("jxs-send", &cores);
        let mut output: Option<EncoderOutput> = None;
        let mut window = SendWindow::default();
        let mut last_status = Instant::now();
        let mut datagrams: Vec<u8> = Vec::with_capacity(70_000);
        let mut warned_format = false;
        let mut warned_encode = false;
        loop {
            if last_status.elapsed() >= STATUS_PERIOD {
                stats.drain_into(&mut window);
                window.window_s = last_status.elapsed().as_secs_f64();
                window.bpp = bpp;
                if stats.not_yuyv.load(Ordering::Relaxed) && !warned_format {
                    tracing::warn!(
                        "jxs send: the capture is not YUYV -- frames are not sent (issue 1387)"
                    );
                    warned_format = true;
                }
                tracing::info!("{}", window.status_line());
                window = SendWindow::default();
                last_status = Instant::now();
            }
            let job = match job_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(j) => j,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let (timecode, handed_off, submitted) = match job {
                Job::Session(o) => {
                    output = Some(o);
                    continue;
                }
                Job::Frame {
                    timecode_100ns,
                    handed_off,
                    submitted,
                } => (timecode_100ns, handed_off, submitted),
            };
            stats.in_flight.fetch_sub(1, Ordering::Relaxed);
            let Some(out) = output.as_ref() else { continue };
            let count = jxs_link::packets_per_frame(out.height());
            let mut slices = 0u64;
            let mut bytes = 0u64;
            let mut errors = 0u64;
            let mut packet_errors = 0u64;
            let mut first_sent: Option<Instant> = None;
            let result = out.recv_frame(|index, packet, _last| {
                if index >= count || count > 255 {
                    packet_errors += 1;
                    return;
                }
                let base = PacketHeader {
                    camera_id,
                    timecode_100ns: timecode.max(0) as u64,
                    slice_index: index as u8,
                    slice_count: count as u8,
                    offset: 0,
                    slice_len: 0,
                };
                match jxs_link::build_slice_datagrams(base, packet, &mut datagrams) {
                    Ok(_) => {
                        errors += socket.send_slice(&datagrams);
                        first_sent.get_or_insert_with(Instant::now);
                        slices += 1;
                        bytes += packet.len() as u64;
                    }
                    Err(_) => packet_errors += 1,
                }
            });
            match result {
                Ok((pic, n)) => {
                    let _ = free_tx.try_send(pic);
                    if n == count && packet_errors == 0 {
                        let first = first_sent
                            .map(|t| t.duration_since(handed_off).as_secs_f64() * 1000.0)
                            .unwrap_or(0.0);
                        window.note_frame(
                            slices,
                            bytes,
                            first,
                            submitted.elapsed().as_secs_f64() * 1000.0,
                        );
                    } else {
                        window.encode_errors += 1;
                        if !warned_encode {
                            tracing::error!(
                                "jxs send: the encoder made {n} packets, the wire format expects {count} (issue 1387)"
                            );
                            warned_encode = true;
                        }
                    }
                }
                // a frame of a session already abandoned: its picture came back with it
                Err(e) if e.is_session_state() => {}
                Err(e) => {
                    window.encode_errors += 1;
                    if !warned_encode {
                        tracing::error!("jxs send: {e} -- re-opening the encoder (issue 1387)");
                        warned_encode = true;
                    }
                    for pic in out.abandon() {
                        let _ = free_tx.try_send(pic);
                    }
                }
            }
            window.send_errors += errors;
        }
    }
}

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
        let (h, ready) = JxsHandoff::detached();
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
        let (h, ready) = JxsHandoff::detached();
        let (w, ht) = (64u32, 8u32);
        let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i % 251) as u8).collect();
        let t = std::time::Instant::now();
        for k in [0, 2, 4] {
            h.offer(&jf(&frame, w, ht, tc(k), true));
        }
        assert!(
            t.elapsed() < std::time::Duration::from_millis(50),
            "never waits"
        );
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
        let (h, ready) = JxsHandoff::detached();
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
        let port = rx.local_addr().unwrap().port();
        let h = JxsSender::start(Some(&format!("127.0.0.1:{port}")), "CAM5").expect("link on");
        let (w, ht) = (1920u32, 1080u32);
        let frame: Vec<u8> = (0..w * ht * 2).map(|i| (i % 249) as u8).collect();
        h.offer(&jf(&frame, w, ht, tc(0), true));
        h.offer(&jf(&frame, w, ht, tc(2), true));
        assert_eq!(h.dropped_busy(), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut b = [0u8; 2048];
        while h.stats.in_flight.load(Ordering::Relaxed) > 0 && std::time::Instant::now() < deadline
        {
            let _ = rx.recv(&mut b);
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
        let (h, ready) = JxsHandoff::detached();
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
