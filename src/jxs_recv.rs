//! Issue 1387 -- the receive side of the JPEG XS link: a `recvmmsg` + UDP GRO socket with kernel
//! receive timestamps, and (with the `jxs` feature) the slice-wise decoder with concealment that
//! the `jxs-recv-probe` bin and the loopback test use.
//!
//! Loss is concealed, never waited for (owner decision 6489239017): a packet the assembler hands
//! on as lost is replaced by the SAME packet index of the previous frame. JPEG XS slices are coded
//! independently and carry their own index, so the previous frame's bytes decode as that
//! region of the previous picture, and the decoder keeps working packet by packet.

use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;

use crate::jxs_send::{set_int_opt, UDP_GRO};

/// Datagram buffers per `recvmmsg` call.
pub const RECV_BATCH: usize = 32;
/// One GRO buffer (the kernel merges at most 64 KB).
pub const RECV_BUF_LEN: usize = 65_536;
/// Socket receive buffer asked for (the kernel caps it at `net.core.rmem_max`).
pub const RECV_BUFFER_BYTES: i32 = 16 * 1024 * 1024;

/// The current CLOCK_REALTIME in 100 ns since the epoch (the sender timecode's clock).
pub fn realtime_100ns() -> i64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_nanos() / 100) as i64
}

/// A bound receive socket with GRO + kernel timestamps, buffers allocated once.
pub struct JxsRecvSocket {
    sock: UdpSocket,
    bufs: Vec<u8>,
    ctrl: Vec<u8>,
    gro: bool,
    timestamps: bool,
}

const CTRL_LEN: usize = 128;

impl JxsRecvSocket {
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(addr)?;
        let fd = sock.as_raw_fd();
        set_int_opt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, RECV_BUFFER_BYTES);
        let gro = set_int_opt(fd, libc::SOL_UDP, UDP_GRO, 1);
        let timestamps = set_int_opt(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPNS, 1);
        Ok(Self {
            sock,
            bufs: vec![0; RECV_BATCH * RECV_BUF_LEN],
            ctrl: vec![0; RECV_BATCH * CTRL_LEN],
            gro,
            timestamps,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// Whether UDP GRO and kernel receive timestamps were accepted.
    pub fn features(&self) -> (bool, bool) {
        (self.gro, self.timestamps)
    }

    /// The effective receive buffer (bytes, as the kernel reports it).
    pub fn recv_buffer(&self) -> i64 {
        let mut v: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: valid out-pointers sized for a c_int.
        let ok = unsafe {
            libc::getsockopt(
                self.sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
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

    pub fn set_timeout(&self, t: Option<std::time::Duration>) -> std::io::Result<()> {
        self.sock.set_read_timeout(t)
    }

    /// One `recvmmsg` call (blocking up to the read timeout). `f(datagram, arrival_100ns)` is
    /// called for every datagram, GRO buffers split back into datagrams. `arrival_100ns` is the
    /// kernel receive time (CLOCK_REALTIME) when timestamps are on, else the time the call
    /// returned. Returns the number of datagrams; a timeout returns 0.
    pub fn recv_batch(&mut self, mut f: impl FnMut(&[u8], i64)) -> std::io::Result<usize> {
        // SAFETY (whole block): every iovec/msghdr points into `self.bufs` / `self.ctrl`, which
        // live for the call and are sized RECV_BATCH x RECV_BUF_LEN / CTRL_LEN; the kernel
        // writes only within the given lengths.
        let mut iov: [libc::iovec; RECV_BATCH] = unsafe { std::mem::zeroed() };
        let mut msgs: [libc::mmsghdr; RECV_BATCH] = unsafe { std::mem::zeroed() };
        for i in 0..RECV_BATCH {
            iov[i].iov_base = self.bufs[i * RECV_BUF_LEN..].as_mut_ptr().cast();
            iov[i].iov_len = RECV_BUF_LEN;
            msgs[i].msg_hdr.msg_iov = &mut iov[i];
            msgs[i].msg_hdr.msg_iovlen = 1;
            msgs[i].msg_hdr.msg_control = self.ctrl[i * CTRL_LEN..].as_mut_ptr().cast();
            msgs[i].msg_hdr.msg_controllen = CTRL_LEN as _;
        }
        let n = unsafe {
            libc::recvmmsg(
                self.sock.as_raw_fd(),
                msgs.as_mut_ptr(),
                RECV_BATCH as libc::c_uint,
                libc::MSG_WAITFORONE,
                std::ptr::null_mut(),
            )
        };
        let returned = realtime_100ns();
        if n < 0 {
            let e = std::io::Error::last_os_error();
            return match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Ok(0),
                _ if e.raw_os_error() == Some(libc::EINTR) => Ok(0),
                _ => Err(e),
            };
        }
        let mut count = 0;
        for (i, m) in msgs.iter().enumerate().take(n as usize) {
            let (gso, ts) = unsafe { parse_cmsgs(&m.msg_hdr) };
            let arrival = ts.unwrap_or(returned);
            let len = m.msg_len as usize;
            let buf = &self.bufs[i * RECV_BUF_LEN..i * RECV_BUF_LEN + len];
            for d in crate::jxs_link::split_gro(buf, gso) {
                f(d, arrival);
                count += 1;
            }
        }
        Ok(count)
    }
}

/// Read the GRO segment size and the SO_TIMESTAMPNS time out of a received message's control
/// data.
///
/// # Safety
/// `h` must be a msghdr the kernel just filled (its control pointer and length valid).
unsafe fn parse_cmsgs(h: &libc::msghdr) -> (Option<usize>, Option<i64>) {
    let mut gso = None;
    let mut ts = None;
    let mut c = libc::CMSG_FIRSTHDR(h);
    while !c.is_null() {
        let level = (*c).cmsg_level;
        let kind = (*c).cmsg_type;
        let data = libc::CMSG_DATA(c);
        if level == libc::SOL_UDP && kind == UDP_GRO {
            let v = std::ptr::read_unaligned(data.cast::<libc::c_int>());
            if v > 0 {
                gso = Some(v as usize);
            }
        } else if level == libc::SOL_SOCKET && kind == libc::SCM_TIMESTAMPNS {
            let t = std::ptr::read_unaligned(data.cast::<libc::timespec>());
            ts = Some(t.tv_sec * 10_000_000 + t.tv_nsec / 100);
        }
        c = libc::CMSG_NXTHDR(h, c);
    }
    (gso, ts)
}

#[cfg(feature = "jxs")]
pub use decode::DecodeSink;

#[cfg(feature = "jxs")]
mod decode {
    use crate::jxs_link::{FrameReport, SliceSink};
    use svt_jpegxs_sys::{Decoder, Yuv422Planes};

    /// The receiver's [`SliceSink`]: decodes packet by packet, conceals lost packets from the
    /// previous frame, and reports every decoded frame to `on_frame`.
    pub struct DecodeSink<F: FnMut(&FrameReport, Option<&Yuv422Planes>)> {
        dec: Option<Decoder>,
        /// The last good bytes of every packet index (the concealment source), reused.
        prev: Vec<Vec<u8>>,
        has_prev: Vec<bool>,
        /// The packets of the frame before the decoder is open (its init reads a whole frame).
        pending: Vec<Vec<u8>>,
        pending_ok: bool,
        whole: Vec<u8>,
        count: usize,
        frame_failed: bool,
        on_frame: F,
        /// Decoder errors (a frame that could not be decoded).
        pub errors: u64,
        pub last_error: Option<String>,
    }

    impl<F: FnMut(&FrameReport, Option<&Yuv422Planes>)> DecodeSink<F> {
        /// `on_frame(report, planes)`: `planes` is `None` for a frame that could not be decoded.
        pub fn new(on_frame: F) -> Self {
            Self {
                dec: None,
                prev: Vec::new(),
                has_prev: Vec::new(),
                pending: Vec::new(),
                pending_ok: true,
                whole: Vec::new(),
                count: 0,
                frame_failed: false,
                on_frame,
                errors: 0,
                last_error: None,
            }
        }

        fn remember(&mut self, index: usize, bytes: &[u8]) {
            if self.prev.len() <= index {
                self.prev.resize_with(index + 1, Vec::new);
                self.has_prev.resize(index + 1, false);
            }
            self.prev[index].clear();
            self.prev[index].extend_from_slice(bytes);
            self.has_prev[index] = true;
        }

        fn fail(&mut self, e: impl std::fmt::Display) {
            self.frame_failed = true;
            self.last_error = Some(e.to_string());
        }
    }

    impl<F: FnMut(&FrameReport, Option<&Yuv422Planes>)> SliceSink for DecodeSink<F> {
        fn packet(&mut self, _tc: u64, index: u8, count: u8, bytes: Option<&[u8]>) {
            let index = index as usize;
            if index == 0 {
                self.frame_failed = false;
                if count as usize != self.count {
                    // A new geometry (or the first frame): start the decoder over.
                    self.count = count as usize;
                    self.dec = None;
                    self.has_prev.iter_mut().for_each(|h| *h = false);
                }
                if self.dec.is_none() {
                    self.pending.resize_with(self.count, Vec::new);
                    self.pending_ok = true;
                }
            }
            if self.dec.is_none() {
                // Collect the whole first frame for the decoder's init.
                match bytes {
                    Some(b) if index < self.pending.len() => {
                        self.pending[index].clear();
                        self.pending[index].extend_from_slice(b);
                    }
                    _ => self.pending_ok = false,
                }
                return;
            }
            if self.frame_failed {
                if let Some(b) = bytes {
                    self.remember(index, b);
                }
                return;
            }
            let concealed;
            let data: &[u8] = match bytes {
                Some(b) => b,
                None if self.has_prev.get(index).copied().unwrap_or(false) => {
                    concealed = std::mem::take(&mut self.prev[index]);
                    let r = self.dec.as_mut().expect("open").send_packet(&concealed);
                    self.prev[index] = concealed;
                    if let Err(e) = r {
                        self.fail(e);
                    }
                    return;
                }
                None => {
                    self.fail("a lost packet with no previous frame to conceal it from");
                    return;
                }
            };
            let r = self.dec.as_mut().expect("open").send_packet(data);
            if let Err(e) = r {
                self.fail(e);
            }
            self.remember(index, data);
        }

        fn frame_done(&mut self, report: &FrameReport) {
            if self.dec.is_none() {
                if !self.pending_ok {
                    (self.on_frame)(report, None);
                    return;
                }
                self.whole.clear();
                for p in &self.pending[..self.count] {
                    self.whole.extend_from_slice(p);
                }
                match Decoder::open(&self.whole) {
                    Ok(mut d) => {
                        let mut ok = true;
                        for p in &self.pending[..self.count] {
                            if let Err(e) = d.send_packet(p) {
                                self.last_error = Some(e.to_string());
                                ok = false;
                                break;
                            }
                        }
                        for i in 0..self.count {
                            let p = std::mem::take(&mut self.pending[i]);
                            self.remember(i, &p);
                            self.pending[i] = p;
                        }
                        if ok {
                            match d.finish_frame() {
                                Ok(planes) => (self.on_frame)(report, Some(planes)),
                                Err(e) => {
                                    self.errors += 1;
                                    self.last_error = Some(e.to_string());
                                    (self.on_frame)(report, None);
                                }
                            }
                            self.dec = Some(d);
                        } else {
                            self.errors += 1;
                            (self.on_frame)(report, None);
                        }
                    }
                    Err(e) => {
                        self.errors += 1;
                        self.last_error = Some(e.to_string());
                        (self.on_frame)(report, None);
                    }
                }
                return;
            }
            if self.frame_failed {
                // The decoder is mid-frame in an unknown state: open it again from the next
                // whole frame.
                self.errors += 1;
                self.dec = None;
                (self.on_frame)(report, None);
                return;
            }
            match self.dec.as_mut().expect("open").finish_frame() {
                Ok(planes) => (self.on_frame)(report, Some(planes)),
                Err(e) => {
                    self.errors += 1;
                    self.last_error = Some(e.to_string());
                    self.dec = None;
                    (self.on_frame)(report, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jxs_link::{self, PacketHeader, DATAGRAM_LEN};
    use crate::jxs_send::JxsSocket;

    #[test]
    fn recvmmsg_with_gro_returns_every_datagram_of_a_gso_slice() {
        let mut rx = JxsRecvSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        rx.set_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        assert_eq!(
            rx.features(),
            (true, true),
            "GRO + kernel timestamps accepted"
        );
        let port = rx.local_addr().unwrap().port();
        let tx = JxsSocket::connect(&format!("127.0.0.1:{port}")).unwrap();
        let slice: Vec<u8> = (0..30_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let mut buf = Vec::new();
        let base = PacketHeader {
            camera_id: 9,
            timecode_100ns: 1_234_567,
            slice_index: 4,
            slice_count: 9,
            offset: 0,
            slice_len: 0,
        };
        let n = jxs_link::build_slice_datagrams(base, &slice, &mut buf).unwrap();
        let before = realtime_100ns();
        assert_eq!(tx.send_slice(&buf), 0);
        let mut rebuilt = vec![0u8; slice.len()];
        let mut got = 0;
        while got < n {
            let k = rx
                .recv_batch(|d, at| {
                    assert!(d.len() <= DATAGRAM_LEN);
                    assert!(at >= before - 10_000_000, "arrival on the wall clock");
                    let h = PacketHeader::decode(d).unwrap();
                    assert_eq!((h.camera_id, h.slice_index), (9, 4));
                    let o = h.offset as usize;
                    rebuilt[o..o + d.len() - jxs_link::HEADER_LEN]
                        .copy_from_slice(&d[jxs_link::HEADER_LEN..]);
                })
                .unwrap();
            assert!(k > 0, "timed out after {got} of {n}");
            got += k;
        }
        assert_eq!(got, n);
        assert_eq!(rebuilt, slice);
    }
}
