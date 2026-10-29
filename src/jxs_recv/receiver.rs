//! Issue 1387 slice 2 -- the ONE JPEG XS receive loop: socket -> [`FrameAssembler`] ->
//! [`DecodeSink`] -> decoded frames, the per-window statistics and the receiver report back to
//! the sender (the automatic bpp's back channel).
//!
//! `jxs-recv-probe` and the strih OBS source (through `jxs/recv-ffi`, a C ABI over this type)
//! both run exactly this code, so the probe's measurements describe what OBS receives.
//!
//! [`JxsReceiver::poll`] blocks up to a timeout and returns the next decoded frame, borrowed
//! until the next call. Frames decoded faster than the caller polls queue up to
//! [`MAX_READY_FRAMES`]; past that the OLDEST is dropped and counted (`dropped_unread`), so a slow
//! caller always gets the newest pictures and the receive socket is never left unread. A frame
//! the decoder gave up on never reaches the caller: it is counted (`undecodable_frames`) and
//! reported to the sender as a loss, exactly as the probe always did.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{realtime_100ns, DecodeSink, JxsRecvSocket};
use crate::jxs_link::{
    self, FrameAssembler, FrameReport, PacketHeader, PushOutcome, RecvWindow, ReportWindow,
};
use svt_jpegxs_sys::Yuv422Planes;

/// Decoded frames held for a caller that has not polled yet; older ones are dropped.
pub const MAX_READY_FRAMES: usize = 3;
/// The longest single blocking receive inside [`JxsReceiver::poll`] (the report and the window
/// roll are looked at between two receives).
const RECV_SLICE: Duration = Duration::from_millis(50);

/// What a receiver listens for and what it reports back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReceiverConfig {
    pub listen: SocketAddr,
    /// Take only this camera id (`CAM<N>`); `None` = the first one heard.
    pub camera: Option<u8>,
    /// The highest bpp this receiver takes (0 = no limit), sent in every report.
    pub max_bpp: u8,
    /// A frame decoded more than this long after its timecode counts as late in the report.
    pub late_ms: f64,
    /// Send the receiver report (off = the sender's automatic bpp holds).
    pub send_reports: bool,
    /// The statistics window ([`JxsReceiver::take_window_line`]).
    pub window: Duration,
}

impl ReceiverConfig {
    /// The defaults the probe and the OBS source share: any camera, no bpp cap, late after
    /// 100 ms, reports on, a 5 s window.
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            camera: None,
            max_bpp: 0,
            late_ms: 100.0,
            send_reports: true,
            window: Duration::from_secs(5),
        }
    }
}

/// One decoded frame, borrowed from the receiver until the next [`JxsReceiver::poll`].
#[derive(Debug)]
pub struct DecodedFrame {
    pub camera_id: u8,
    /// The sender's genlock timecode, 100 ns since the Unix epoch.
    pub timecode_100ns: u64,
    /// Slices concealed from the previous frame.
    pub lost_slices: u32,
    /// Kernel receive time of the first / last datagram, and the decode time (100 ns, epoch).
    pub first_arrival_100ns: i64,
    pub last_arrival_100ns: i64,
    pub decoded_at_100ns: i64,
    pub planes: Yuv422Planes,
}

/// Cumulative counters since the receiver opened (the current window included).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverTotals {
    /// Frames decoded (concealed ones included).
    pub frames: u64,
    pub concealed_frames: u64,
    pub lost_slices: u64,
    pub undecodable_frames: u64,
    pub datagrams: u64,
    /// Payload bytes of the taken camera (headers excluded).
    pub bytes: u64,
    pub duplicate: u64,
    pub stale: u64,
    pub malformed: u64,
    pub resyncs: u64,
    pub foreign: u64,
    pub reports_sent: u64,
    pub report_errors: u64,
    /// Decoder reopens at a changed picture header (a sender bpp change).
    pub header_changes: u64,
    /// Decoded frames dropped because the caller did not poll in time.
    pub dropped_unread: u64,
}

impl ReceiverTotals {
    fn add_window(&mut self, w: &RecvWindow) {
        self.frames += w.frames;
        self.concealed_frames += w.concealed_frames;
        self.lost_slices += w.lost_slices;
        self.undecodable_frames += w.undecodable_frames;
        self.datagrams += w.datagrams;
        self.bytes += w.bytes;
        self.duplicate += w.duplicate;
        self.stale += w.stale;
        self.malformed += w.malformed;
        self.resyncs += w.resyncs;
        self.foreign += w.foreign;
        self.reports_sent += w.reports_sent;
    }
}

/// Filled by the decoder callback, drained by the loop (one thread; the lock is uncontended and
/// only makes the receiver `Send`).
#[derive(Default)]
struct Outbox {
    /// Every finished frame: its report, when it was decoded, whether it decoded.
    events: Vec<(FrameReport, i64, bool)>,
    ready: VecDeque<DecodedFrame>,
    /// Plane buffers to reuse.
    pool: Vec<Yuv422Planes>,
    dropped_unread: u64,
}

impl Outbox {
    fn take_planes(&mut self, w: u32, h: u32) -> Yuv422Planes {
        while let Some(p) = self.pool.pop() {
            if p.width() == w && p.height() == h {
                return p;
            }
        }
        Yuv422Planes::new(w, h)
    }

    fn decoded(&mut self, r: &FrameReport, at: i64, p: &Yuv422Planes) {
        let mut planes = self.take_planes(p.width(), p.height());
        let (y, u, v) = planes.planes_mut();
        y.copy_from_slice(p.y());
        u.copy_from_slice(p.u());
        v.copy_from_slice(p.v());
        self.ready.push_back(DecodedFrame {
            camera_id: r.camera_id,
            timecode_100ns: r.timecode,
            lost_slices: r.lost_slices,
            first_arrival_100ns: r.first_arrival,
            last_arrival_100ns: r.last_arrival,
            decoded_at_100ns: at,
            planes,
        });
        while self.ready.len() > MAX_READY_FRAMES {
            if let Some(old) = self.ready.pop_front() {
                self.pool.push(old.planes);
                self.dropped_unread += 1;
            }
        }
    }
}

type Callback = Box<dyn FnMut(&FrameReport, Option<&Yuv422Planes>) + Send>;

/// One camera's JPEG XS receiver (see the module doc).
pub struct JxsReceiver {
    cfg: ReceiverConfig,
    sock: JxsRecvSocket,
    assembler: FrameAssembler,
    sink: DecodeSink<Callback>,
    out: Arc<Mutex<Outbox>>,
    camera: Option<u8>,
    sender: Option<SocketAddr>,
    report: ReportWindow,
    last_report: Instant,
    window: RecvWindow,
    window_start: Instant,
    window_line: Option<String>,
    totals: ReceiverTotals,
    timeout: Option<Duration>,
    current: Option<DecodedFrame>,
}

fn lock(m: &Mutex<Outbox>) -> std::sync::MutexGuard<'_, Outbox> {
    // A panic while holding the lock leaves plain counters and buffers: keep using them.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl JxsReceiver {
    /// Bind the listening socket. Nothing is received until the first [`Self::poll`].
    pub fn bind(cfg: ReceiverConfig) -> std::io::Result<Self> {
        let sock = JxsRecvSocket::bind(cfg.listen)?;
        let out = Arc::new(Mutex::new(Outbox::default()));
        let sink_out = Arc::clone(&out);
        let callback: Callback = Box::new(move |report, planes| {
            let at = realtime_100ns();
            let mut o = lock(&sink_out);
            o.events.push((*report, at, planes.is_some()));
            if let Some(p) = planes {
                o.decoded(report, at, p);
            }
        });
        let now = Instant::now();
        Ok(Self {
            cfg,
            sock,
            assembler: FrameAssembler::new(),
            sink: DecodeSink::new(callback),
            out,
            camera: cfg.camera,
            sender: None,
            report: ReportWindow::default(),
            last_report: now,
            window: RecvWindow::default(),
            window_start: now,
            window_line: None,
            totals: ReceiverTotals::default(),
            timeout: None,
            current: None,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    pub fn socket(&self) -> &JxsRecvSocket {
        &self.sock
    }

    pub fn config(&self) -> &ReceiverConfig {
        &self.cfg
    }

    /// The camera id being taken (`None` until the first datagram when none was configured).
    pub fn camera(&self) -> Option<u8> {
        self.camera
    }

    /// The address the taken camera's slices come from (the report goes there).
    pub fn sender(&self) -> Option<SocketAddr> {
        self.sender
    }

    /// The counters since the receiver opened, the current window included.
    pub fn totals(&self) -> ReceiverTotals {
        let mut t = self.totals;
        t.add_window(&self.window);
        t.header_changes = self.sink.header_changes;
        t.dropped_unread = lock(&self.out).dropped_unread;
        t
    }

    /// The last decoder error, once.
    pub fn take_last_error(&mut self) -> Option<String> {
        self.sink.last_error.take()
    }

    /// The status line of the last finished window (`jxs recv: fps= ...`), once.
    pub fn take_window_line(&mut self) -> Option<String> {
        self.window_line.take()
    }

    /// Receive until a decoded frame is ready or `timeout` has passed. `Ok(None)` = none in time.
    /// The frame is borrowed until the next call. Reports and the statistics window are kept
    /// on time while it waits.
    pub fn poll(&mut self, timeout: Duration) -> std::io::Result<Option<&DecodedFrame>> {
        if let Some(done) = self.current.take() {
            lock(&self.out).pool.push(done.planes);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let next = lock(&self.out).ready.pop_front();
            if let Some(f) = next {
                self.current = Some(f);
                return Ok(self.current.as_ref());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let slice = (deadline - now)
                .min(RECV_SLICE)
                .max(Duration::from_millis(1));
            if self.timeout != Some(slice) {
                self.sock.set_timeout(Some(slice))?;
                self.timeout = Some(slice);
            }
            self.receive_once()?;
        }
    }

    /// One `recvmmsg` batch, then the frame bookkeeping, the report and the window roll.
    fn receive_once(&mut self) -> std::io::Result<()> {
        let Self {
            sock,
            assembler,
            sink,
            camera,
            sender,
            window,
            ..
        } = self;
        sock.recv_batch_from(|d, arrival, from| {
            window.datagrams += 1;
            let Ok(h) = PacketHeader::decode(d) else {
                window.malformed += 1;
                return;
            };
            match *camera {
                None => *camera = Some(h.camera_id),
                Some(c) if c != h.camera_id => {
                    window.foreign += 1;
                    return;
                }
                Some(_) => {}
            }
            if from.is_some() {
                *sender = from;
            }
            window.bytes += (d.len() - jxs_link::HEADER_LEN) as u64;
            match assembler.push(&h, &d[jxs_link::HEADER_LEN..], arrival, sink) {
                PushOutcome::Accepted => {}
                PushOutcome::Duplicate => window.duplicate += 1,
                PushOutcome::Stale => window.stale += 1,
                PushOutcome::Malformed => window.malformed += 1,
                PushOutcome::Resync => window.resyncs += 1,
            }
        })?;
        self.note_frames();
        self.maybe_report();
        self.maybe_roll_window();
        Ok(())
    }

    fn note_frames(&mut self) {
        let events = std::mem::take(&mut lock(&self.out).events);
        let ms = |t: i64| t as f64 / 10_000.0;
        for (r, decoded_at, ok) in &events {
            let tc = r.timecode as i64;
            if *ok {
                let latency_ms = ms(decoded_at - tc);
                self.window.note_frame(
                    latency_ms,
                    ms(r.first_arrival - tc),
                    ms(r.last_arrival - r.first_arrival),
                    r.lost_slices,
                );
                let decode_us = ((decoded_at - r.last_arrival).max(0) / 10) as u32;
                self.report.note_frame(
                    r.lost_slices,
                    r.timecode,
                    latency_ms > self.cfg.late_ms,
                    Some(decode_us),
                );
            } else {
                self.window.undecodable_frames += 1;
                self.window.lost_slices += r.lost_slices as u64;
                // a frame the decoder gave up on reached the screen as nothing: a loss
                self.report
                    .note_frame(r.lost_slices.max(1), r.timecode, false, None);
            }
        }
        // hand the (now empty) buffer back so the callback never reallocates it
        let mut events = events;
        events.clear();
        let mut o = lock(&self.out);
        if o.events.is_empty() {
            o.events = events;
        }
    }

    fn maybe_report(&mut self) {
        if self.last_report.elapsed() < Duration::from_millis(jxs_link::REPORT_PERIOD_MS) {
            return;
        }
        self.last_report = Instant::now();
        let r = self
            .report
            .take(self.camera.unwrap_or(0), self.cfg.max_bpp as u32);
        if let (true, Some(to), Some(_)) = (self.cfg.send_reports, self.sender, self.camera) {
            match self.sock.send_report(to, &r) {
                Ok(()) => self.window.reports_sent += 1,
                Err(e) => {
                    self.totals.report_errors += 1;
                    tracing::warn!("jxs recv: report to {to}: {e} (issue 1387)");
                }
            }
        }
    }

    fn maybe_roll_window(&mut self) {
        let el = self.window_start.elapsed();
        if el < self.cfg.window {
            return;
        }
        self.window_line = Some(self.window.status_line(el.as_secs_f64()));
        self.totals.add_window(&self.window);
        self.window.reset();
        self.window_start = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(tc: u64) -> FrameReport {
        FrameReport {
            timecode: tc,
            camera_id: 5,
            slice_count: 9,
            lost_slices: 0,
            first_arrival: 0,
            last_arrival: 0,
        }
    }

    fn planes(fill: u8) -> Yuv422Planes {
        let mut p = Yuv422Planes::new(64, 16);
        let (y, u, v) = p.planes_mut();
        y.fill(fill);
        u.fill(fill);
        v.fill(fill);
        p
    }

    #[test]
    fn a_slow_caller_keeps_the_newest_frames_and_the_drop_is_counted() {
        let mut o = Outbox::default();
        for i in 0..5u8 {
            o.decoded(&report(i as u64), 0, &planes(i));
        }
        assert_eq!(o.ready.len(), MAX_READY_FRAMES);
        assert_eq!(o.dropped_unread, 2);
        let tcs: Vec<u64> = o.ready.iter().map(|f| f.timecode_100ns).collect();
        assert_eq!(tcs, [2, 3, 4], "the oldest go, the newest stay");
        assert_eq!(o.ready[0].planes.y()[0], 2, "each frame holds its own copy");
        // the 4th frame's drop pooled a buffer that the 5th frame took again
        assert_eq!(o.pool.len(), 1, "the dropped buffers are reused");
    }

    #[test]
    fn a_pooled_buffer_of_another_geometry_is_never_reused() {
        let mut o = Outbox::default();
        o.pool.push(Yuv422Planes::new(32, 16));
        let p = o.take_planes(64, 16);
        assert_eq!((p.width(), p.height()), (64, 16));
        assert!(o.pool.is_empty(), "the stale buffer is dropped");
    }
}
