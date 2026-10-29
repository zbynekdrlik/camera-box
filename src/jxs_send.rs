//! Issue 1387 -- the cambox JPEG XS sender (slice 1: one configured receiver, no OBS side yet).
//!
//! OFF by default: unless `CAMERA_BOX_JXS_TARGET=host:port` is set, [`JxsSender::start_from_env`]
//! returns `None`, no thread, socket or encoder exists, and the capture loop's NDI path is
//! byte-identical (its only new statement is an `if let Some(..)` on that `None`).
//!
//! When set, a dedicated `jxs-send` thread, pinned OFF the capture core onto the display cores
//! (`affinity::pin_off_capture_core`, the #1198 split), owns the SVT-JPEG-XS encoder (its worker
//! threads inherit that CPU set) and the UDP socket. The capture thread hands it the SAME frame
//! and genlock timecode the emit gate sends to NDI through [`JxsHandoff::offer`]: one memcpy into
//! the one reusable frame buffer, never a wait. If the encoder still works on the previous frame,
//! the frame is dropped and counted (`dropped_busy`); one frame in flight, never a queue.
//!
//! The worker converts YUYV to planar 4:2:2 and encodes with slice packetization (latency
//! profile, decomp_v 2, 4 bpp: issue comment 6489868243). Every packet goes on the wire the
//! moment the encoder produces it: one `send` per slice of consecutive datagrams with
//! `UDP_SEGMENT` set on the socket (UDP GSO), so the kernel cuts the slice into MTU datagrams
//! (wire format: `crate::jxs_link`). Every 5 s it logs `jxs send: frames= slices= bytes=
//! dropped_busy= encode_ms_avg/max= send_errors= encode_errors= (issue 1387)`.
//!
//! The codec needs the `jxs` cargo feature (CI's deployed `camera-box-linux-amd64` carries it).
//! A build without it logs one ERROR when the target is set and stays off.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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

/// The one frame handed from the capture thread to the sender thread.
#[derive(Debug, Default)]
struct Slot {
    raw: Vec<u8>,
    width: u32,
    height: u32,
    stride: u32,
    yuyv: bool,
    timecode_100ns: i64,
}

#[derive(Debug, Default)]
struct Shared {
    /// The worker owns the slot (a frame is waiting or being encoded).
    busy: AtomicBool,
    stop: AtomicBool,
    dropped_busy: AtomicU64,
    slot: Mutex<Slot>,
}

/// The capture thread's handle. Dropping it stops and joins the sender thread.
pub struct JxsHandoff {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl JxsHandoff {
    /// Hand one emitted frame over. Never blocks: a busy sender drops it and counts it.
    /// Allocation-free once the buffer has grown to the frame size.
    pub fn offer(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        yuyv: bool,
        timecode_100ns: i64,
    ) {
        if self.shared.busy.load(Ordering::Acquire) {
            self.shared.dropped_busy.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Not busy = the worker does not hold the lock; try_lock keeps the never-wait promise.
        let Ok(mut slot) = self.shared.slot.try_lock() else {
            self.shared.dropped_busy.fetch_add(1, Ordering::Relaxed);
            return;
        };
        slot.raw.clear();
        slot.raw.extend_from_slice(data);
        slot.width = width;
        slot.height = height;
        slot.stride = stride;
        slot.yuyv = yuyv;
        slot.timecode_100ns = timecode_100ns;
        drop(slot);
        self.shared.busy.store(true, Ordering::Release);
        if let Some(w) = &self.worker {
            w.thread().unpark();
        }
    }

    /// Frames dropped because the sender was busy, since the last status line.
    pub fn dropped_busy(&self) -> u64 {
        self.shared.dropped_busy.load(Ordering::Relaxed)
    }

    /// A hand-off with no sender thread: every offer after the first finds it busy (tests).
    #[cfg(test)]
    fn detached() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            worker: None,
        }
    }
}

impl Drop for JxsHandoff {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(w) = self.worker.take() {
            w.thread().unpark();
            let _ = w.join();
        }
    }
}

/// Entry point for the capture loop.
pub struct JxsSender;

impl JxsSender {
    /// Read [`TARGET_ENV`]: unset/blank = `None` (nothing created). Set = start the sender
    /// thread; any failure is logged and the link stays off (`None`), never the capture loop.
    pub fn start_from_env(hostname: &str) -> Option<JxsHandoff> {
        let value = std::env::var(TARGET_ENV).ok();
        Self::start(value.as_deref(), hostname)
    }

    /// [`Self::start_from_env`] with the value passed in (tests).
    pub fn start(value: Option<&str>, hostname: &str) -> Option<JxsHandoff> {
        let target = match jxs_link::parse_target(value) {
            Ok(None) => return None,
            Ok(Some(t)) => t,
            Err(e) => {
                tracing::error!("{e} -- the JPEG XS link stays off (issue 1387)");
                return None;
            }
        };
        start_worker(target, jxs_link::camera_id_from_hostname(hostname))
    }
}

#[cfg(not(feature = "jxs"))]
fn start_worker(target: String, _camera_id: u8) -> Option<JxsHandoff> {
    tracing::error!(
        "{TARGET_ENV}={target} is set, but this camera-box was built without the `jxs` feature -- \
         the JPEG XS link stays off (issue 1387)"
    );
    None
}

#[cfg(feature = "jxs")]
fn start_worker(target: String, camera_id: u8) -> Option<JxsHandoff> {
    let socket = match JxsSocket::connect(&target) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "{TARGET_ENV}={target}: {e} -- the JPEG XS link stays off (issue 1387)"
            );
            return None;
        }
    };
    tracing::info!(
        "jxs send: link ON to {} (camera id {camera_id}, UDP GSO {}, send buffer {} bytes) -- JPEG XS 4 bpp, latency profile, beside NDI (issue 1387)",
        socket
            .peer()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| target.clone()),
        if socket.gso() { "on" } else { "OFF: one send per datagram" },
        socket.send_buffer()
    );
    let shared = Arc::new(Shared::default());
    let worker_shared = Arc::clone(&shared);
    let worker = std::thread::Builder::new()
        .name("jxs-send".into())
        .spawn(move || worker::run(worker_shared, socket, camera_id))
        .map_err(|e| tracing::error!("jxs send: thread spawn failed: {e} (issue 1387)"))
        .ok()?;
    Some(JxsHandoff {
        shared,
        worker: Some(worker),
    })
}

#[cfg(feature = "jxs")]
mod worker {
    use super::{JxsSocket, Shared, STATUS_PERIOD};
    use crate::jxs_link::{self, PacketHeader, SendWindow};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use svt_jpegxs_sys::{Encoder, EncoderSettings};

    pub(super) fn run(shared: Arc<Shared>, socket: JxsSocket, camera_id: u8) {
        // Before the encoder exists: its worker threads inherit this CPU set.
        #[cfg(target_os = "linux")]
        crate::affinity::pin_off_capture_core("jxs-send");
        let mut enc: Option<Encoder> = None;
        let mut window = SendWindow::default();
        let mut last_status = Instant::now();
        let mut datagrams: Vec<u8> = Vec::with_capacity(70_000);
        let mut warned_format = false;
        let mut warned_encode = false;
        // A geometry the encoder refused: never re-opened (and re-logged) on every frame.
        let mut refused: Option<(u32, u32)> = None;
        loop {
            if !shared.busy.load(Ordering::Acquire) {
                std::thread::park_timeout(Duration::from_millis(100));
            }
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            if last_status.elapsed() >= STATUS_PERIOD {
                window.dropped_busy = shared.dropped_busy.swap(0, Ordering::Relaxed);
                tracing::info!("{}", window.status_line());
                window = SendWindow::default();
                last_status = Instant::now();
            }
            if !shared.busy.load(Ordering::Acquire) {
                continue;
            }
            let timecode = {
                let slot = shared.slot.lock().unwrap_or_else(|e| e.into_inner());
                if !slot.yuyv {
                    if !warned_format {
                        tracing::warn!(
                            "jxs send: the capture is not YUYV -- frames are not sent (issue 1387)"
                        );
                        warned_format = true;
                    }
                    drop(slot);
                    shared.busy.store(false, Ordering::Release);
                    continue;
                }
                let geometry_changed = enc.as_mut().is_none_or(|e| {
                    let p = e.planes_mut();
                    p.width() != slot.width || p.height() != slot.height
                });
                if geometry_changed && refused != Some((slot.width, slot.height)) {
                    enc = match Encoder::open(EncoderSettings::issue_1387(slot.width, slot.height))
                    {
                        Ok(e) => {
                            tracing::info!(
                                "jxs send: encoder open {}x{}, {} bytes per frame (issue 1387)",
                                slot.width,
                                slot.height,
                                e.bytes_per_frame()
                            );
                            Some(e)
                        }
                        Err(e) => {
                            tracing::error!(
                                "jxs send: {e} -- {}x{} frames are not sent (issue 1387)",
                                slot.width,
                                slot.height
                            );
                            refused = Some((slot.width, slot.height));
                            None
                        }
                    };
                }
                let Some(e) = enc.as_mut() else {
                    window.encode_errors += 1;
                    drop(slot);
                    shared.busy.store(false, Ordering::Release);
                    continue;
                };
                let p = e.planes_mut();
                let (w, h) = (p.width() as usize, p.height() as usize);
                let (y, u, v) = p.planes_mut();
                let ok =
                    jxs_link::yuyv_to_planar422(&slot.raw, slot.stride as usize, w, h, y, u, v);
                if !ok {
                    window.encode_errors += 1;
                    drop(slot);
                    shared.busy.store(false, Ordering::Release);
                    continue;
                }
                slot.timecode_100ns
            };
            let enc_ref = enc.as_mut().expect("opened above");
            let count = jxs_link::packets_per_frame(enc_ref.planes_mut().height());
            let mut slices = 0u64;
            let mut bytes = 0u64;
            let mut errors = 0u64;
            let mut packet_errors = 0u64;
            // encode_ms = the picture handed to the encoder -> its last packet sent.
            let started = Instant::now();
            let result = enc_ref.encode_frame(|index, packet, _last| {
                let base = PacketHeader {
                    camera_id,
                    timecode_100ns: timecode.max(0) as u64,
                    slice_index: index.min(255) as u8,
                    slice_count: count.min(255) as u8,
                    offset: 0,
                    slice_len: 0,
                };
                if index >= count || count > 255 {
                    packet_errors += 1;
                    return;
                }
                match jxs_link::build_slice_datagrams(base, packet, &mut datagrams) {
                    Ok(_) => {
                        errors += socket.send_slice(&datagrams);
                        slices += 1;
                        bytes += packet.len() as u64;
                    }
                    Err(_) => packet_errors += 1,
                }
            });
            match result {
                Ok(n) if n == count => {
                    window.note_frame(slices, bytes, started.elapsed().as_secs_f64() * 1000.0);
                }
                Ok(n) => {
                    window.encode_errors += 1;
                    if !warned_encode {
                        tracing::error!(
                            "jxs send: the encoder made {n} packets, the wire format expects {count} (issue 1387)"
                        );
                        warned_encode = true;
                    }
                }
                Err(e) => {
                    window.encode_errors += 1;
                    if !warned_encode {
                        tracing::error!("jxs send: {e} -- re-opening the encoder (issue 1387)");
                        warned_encode = true;
                    }
                    enc = None;
                }
            }
            window.send_errors += errors;
            window.encode_errors += packet_errors;
            shared.busy.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_is_off_unless_the_target_is_set() {
        assert!(JxsSender::start(None, "CAM5").is_none());
        assert!(JxsSender::start(Some(""), "CAM5").is_none());
        assert!(JxsSender::start(Some("  "), "CAM5").is_none());
        // a malformed value is logged and stays off, never a panic
        assert!(JxsSender::start(Some("strih.lan"), "CAM5").is_none());
    }

    #[test]
    fn a_busy_sender_drops_and_counts_the_next_frame_without_waiting() {
        let h = JxsHandoff::detached();
        let frame = vec![7u8; 1920 * 1080 * 2];
        h.offer(&frame, 1920, 1080, 3840, true, 1);
        assert_eq!(h.dropped_busy(), 0, "the first frame is taken");
        let t = std::time::Instant::now();
        h.offer(&frame, 1920, 1080, 3840, true, 2);
        h.offer(&frame, 1920, 1080, 3840, true, 3);
        assert_eq!(h.dropped_busy(), 2, "a busy sender drops and counts");
        assert!(
            t.elapsed() < std::time::Duration::from_millis(5),
            "no wait, no copy"
        );
        // the taken frame is the first one, untouched by the dropped offers
        let slot = h.shared.slot.lock().unwrap();
        assert_eq!(slot.timecode_100ns, 1);
        assert_eq!(slot.raw.len(), frame.len());
    }

    #[test]
    fn the_handoff_buffer_is_reused_between_frames() {
        let h = JxsHandoff::detached();
        let frame = vec![1u8; 64 * 8 * 2];
        h.offer(&frame, 64, 8, 128, true, 1);
        let cap = h.shared.slot.lock().unwrap().raw.capacity();
        h.shared.busy.store(false, Ordering::Release);
        h.offer(&frame, 64, 8, 128, true, 2);
        let slot = h.shared.slot.lock().unwrap();
        assert_eq!(slot.raw.capacity(), cap);
        assert_eq!(slot.timecode_100ns, 2);
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
