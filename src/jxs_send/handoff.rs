//! Issue 1387 slice 1b: the JPEG XS sender's hand-off and its two threads (`jxs-feed`,
//! `jxs-send`); see the parent module doc. Built only with the `jxs` feature.

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

    /// A hand-off with its pool of pictures and no threads yet: offers fill the pictures
    /// into the returned ready queue, nobody drains it until [`Self::start_threads`].
    pub(super) fn unstarted() -> (Self, Receiver<Ready>) {
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

    /// Start `jxs-feed` on `ready_rx` and `jxs-send`; `false` (logged) when a thread could
    /// not be spawned.
    pub(super) fn start_threads(
        &mut self,
        ready_rx: Receiver<Ready>,
        socket: JxsSocket,
        camera_id: u8,
        cores: Vec<usize>,
        bpp: u32,
    ) -> bool {
        // a session change + every frame in flight, with room to spare
        let (job_tx, job_rx) = mpsc::sync_channel::<Job>(FRAMES_IN_FLIGHT * 4);
        let (feed_stats, feed_free, feed_cores) =
            (Arc::clone(&self.stats), self.free_tx.clone(), cores.clone());
        let feeder = std::thread::Builder::new()
            .name("jxs-feed".into())
            .spawn(move || feed(ready_rx, job_tx, feed_free, feed_stats, feed_cores, bpp));
        match feeder {
            Ok(h) => self.workers.push(h),
            Err(e) => {
                tracing::error!("jxs send: thread spawn failed: {e} (issue 1387)");
                return false;
            }
        }
        let (send_stats, send_free) = (Arc::clone(&self.stats), self.free_tx.clone());
        let sender = std::thread::Builder::new()
            .name("jxs-send".into())
            .spawn(move || send(job_rx, socket, camera_id, send_free, send_stats, cores, bpp));
        match sender {
            Ok(h) => {
                self.workers.push(h);
                true
            }
            Err(e) => {
                tracing::error!("jxs send: thread spawn failed: {e} (issue 1387)");
                false
            }
        }
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
    let (mut h, ready_rx) = JxsHandoff::unstarted();
    // a failed start drops `h`: the closed ready queue ends a feeder already started
    h.start_threads(ready_rx, socket, camera_id, cores, bpp)
        .then_some(h)
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
                    tracing::error!("jxs send: {e} -- {w}x{h} frames are not sent (issue 1387)");
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
        let Some(out) = output.as_ref() else {
            stats.in_flight.fetch_sub(1, Ordering::Relaxed);
            continue;
        };
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
        // in flight from its submit to its last packet (or its failure)
        stats.in_flight.fetch_sub(1, Ordering::Relaxed);
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
