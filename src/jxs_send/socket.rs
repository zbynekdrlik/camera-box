//! Issue 1387: the JPEG XS sender's UDP socket ([`JxsSocket`]): GSO slice sends, the optional
//! pace schedule, the egress mark, and the receiver reports read back on the connected socket.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{
    monotonic_ns, sleep_until_monotonic_ns, JXS_IP_TOS, JXS_SOCKET_MARK, SEND_BUFFER_BYTES,
    UDP_SEGMENT,
};
use crate::jxs_link::{self, DATAGRAM_LEN};

/// A connected UDP socket that sends whole slices with GSO.
pub struct JxsSocket {
    pub(super) sock: UdpSocket,
    gso: AtomicBool,
    /// `None` = the kernel accepted [`JXS_SOCKET_MARK`] (or it was not asked for: `mark_asked`);
    /// else the errno it answered.
    pub(super) mark_error: Option<i32>,
    /// Whether the mark was set at all ([`Self::connect_with`]: the strih OBS output, a box with
    /// no cambox egress pacing, never asks).
    mark_asked: bool,
    /// Socket errors (a receiver that is not listening: ECONNREFUSED) [`Self::take_report`]
    /// consumed instead of the next send.
    read_errors: AtomicU64,
}

impl JxsSocket {
    /// Connect to `target` (`host:port`, resolved once now), with the egress mark (the cambox).
    pub fn connect(target: &str) -> std::io::Result<Self> {
        Self::connect_with(target, true)
    }

    /// [`Self::connect`]; `egress_mark` = set [`JXS_SOCKET_MARK`] (only a cambox has the egress
    /// pacing it exempts the flow from).
    pub fn connect_with(target: &str, egress_mark: bool) -> std::io::Result<Self> {
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
        // DSCP AF41 on every datagram (issue 1387, `JXS_IP_TOS`); an unprivileged socket may set
        // it, a refusal leaves the default (best effort) and `tos()` shows it
        let _ = JXS_IP_TOS;
        // SO_MARK needs CAP_NET_ADMIN or CAP_NET_RAW (camera-box.service runs as root). Without it
        // the flow is still sent, only paced with NDI: `marked()` reports it, the start WARNs with
        // the errno and the "link ON" line names it.
        let mark_error = if !egress_mark
            || set_int_opt(
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
            mark_asked: egress_mark,
            read_errors: AtomicU64::new(0),
        })
    }

    /// The socket errors [`Self::take_report`] consumed since the last call (they belong in
    /// `send_errors`: on a connected UDP socket an ICMP "port unreachable" from a receiver that
    /// is not listening is reported by whichever call comes first, a send or this read).
    pub fn take_read_errors(&self) -> u64 {
        self.read_errors.swap(0, Ordering::Relaxed)
    }

    /// Whether the kernel accepted [`JXS_SOCKET_MARK`] (`SO_MARK`), i.e. the egress pacing's fw
    /// filter sends this flow at line rate.
    pub fn marked(&self) -> bool {
        self.mark_asked && self.mark_error.is_none()
    }

    /// The "link ON" line's egress words.
    #[cfg_attr(not(feature = "jxs"), allow(dead_code))]
    pub(super) fn egress_text(&self) -> String {
        if self.marked() {
            format!("{JXS_SOCKET_MARK:#x}: line rate past the NDI pacing")
        } else if self.mark_asked {
            "NONE: paced with NDI".to_string()
        } else {
            "not used (no cambox egress pacing here)".to_string()
        }
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

    /// The `IP_TOS` byte the datagrams leave with (-1 = unreadable).
    pub fn tos(&self) -> i64 {
        get_int_opt(self.sock.as_raw_fd(), libc::IPPROTO_IP, libc::IP_TOS)
    }

    /// The interface the connected socket sends on (the one holding its local address), the one
    /// whose link speed the pace compares with the receiver's (issue 1387). `None` = unknown.
    pub fn local_iface(&self) -> Option<String> {
        match self.sock.local_addr().ok()? {
            SocketAddr::V4(a) => jxs_link::link_speed::iface_of_local_ipv4(*a.ip()),
            SocketAddr::V6(_) => None,
        }
    }

    /// Drain every datagram waiting on the socket without blocking (the connected socket takes
    /// only the receiver's) and return the newest valid receiver report about `camera_id`
    /// (issue 1387 auto bpp); anything else (a garbled datagram, a pending ICMP error) is
    /// skipped. Bounded: at most 64 datagrams per call.
    pub fn take_report(&self, camera_id: u8) -> Option<jxs_link::ReceiverReport> {
        let mut buf = [0u8; 64];
        let mut newest: Option<jxs_link::ReceiverReport> = None;
        for _ in 0..64 {
            // SAFETY: `buf` is valid for its length; MSG_DONTWAIT never blocks.
            let n = unsafe {
                libc::recv(
                    self.sock.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if n < 0 {
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno == Some(libc::EAGAIN) || errno == Some(libc::EWOULDBLOCK) {
                    break;
                }
                // ECONNREFUSED & co: the queued ICMP error a send would have reported is consumed
                // here, so it is counted for the status line's send_errors ([`Self::take_read_errors`])
                self.read_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if let Ok(r) = jxs_link::ReceiverReport::decode(&buf[..n as usize]) {
                if r.camera_id == camera_id {
                    newest = Some(r);
                }
            }
        }
        newest
    }

    /// Send one slice built by `jxs_link::build_slice_datagrams`. Returns the number of send
    /// calls that failed (a receiver that is not listening answers with ICMP, which the next
    /// send reports as `ECONNREFUSED`; the link never waits for it).
    pub fn send_slice(&self, datagrams: &[u8]) -> u64 {
        self.send_slice_paced(datagrams, None).errors
    }

    /// [`Self::send_slice`] on a pace schedule (issue 1387, `jxs_link::pace`): `None` = line rate
    /// (up to 40 datagrams per send, at once); `Some` = at most `PACE_BURST_SEGMENTS` datagrams per
    /// send, each send held on THIS thread until its departure time (an absolute
    /// `clock_nanosleep` on `CLOCK_MONOTONIC`).
    pub fn send_slice_paced(
        &self,
        datagrams: &[u8],
        mut pace: Option<&mut jxs_link::pace::PaceSchedule>,
    ) -> SliceSent {
        let mut out = SliceSent::default();
        let segments = jxs_link::pace::segments_per_send(pace.is_some());
        for r in jxs_link::pace::send_ranges(datagrams.len(), segments) {
            if let Some(p) = pace.as_deref_mut() {
                let now = monotonic_ns();
                let at = p.depart(now, jxs_link::pace::wire_bytes(r.len()));
                out.paced_ns += at.saturating_sub(now);
                sleep_until_monotonic_ns(at);
            }
            out.first.get_or_insert_with(std::time::Instant::now);
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
                        out.errors += 1;
                        continue;
                    }
                }
            }
            for d in batch.chunks(DATAGRAM_LEN) {
                if self.sock.send(d).is_err() {
                    out.errors += 1;
                }
            }
        }
        out
    }
}

/// What [`JxsSocket::send_slice_paced`] did with one slice.
#[derive(Debug, Default, Clone, Copy)]
pub struct SliceSent {
    /// Send calls that failed.
    pub errors: u64,
    /// When the first send went, after its wait (`None` = an empty buffer).
    pub first: Option<std::time::Instant>,
    /// Nanoseconds the schedule held the sends back (0 at line rate): the auto bpp leaves this
    /// out of its encode time, the wire time is not a cost of the codec.
    pub paced_ns: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

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
