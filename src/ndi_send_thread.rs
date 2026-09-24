//! #1242 — the production NDI send thread.
//!
//! It owns the [`NdiSender`], so the capture loop never waits for a send or for the per-camera
//! send stagger. The capture loop calls [`NdiSendThread::hand_off`] once per emitted iteration:
//! the frame is copied off the V4L2 mmap into a pooled buffer and handed over with its timecodes
//! and its absolute deadline (`crate::send_handoff`). The thread then waits for that deadline and
//! sends it. The thread-side logic is the pure, Tier-0 tested `send_handoff::run_send_loop`; this
//! file is only the NDI glue:
//!
//! - one synchronous `send_frame_zero_copy` per timecode. It stamps the capture-based genlock
//!   timecode computed on the capture thread (#286). A failed send logs the same
//!   `Failed to send frame:` line the capture loop used to log.
//! - the #944 emit-liveness heartbeat, stamped only after a CONFIRMED send. A wedged or
//!   persistently failing send now trips the emit-freeze watchdog, because the capture loop keeps
//!   returning while the heartbeat goes stale.
//! - the #297 re-announce check, after every job and on every idle timeout.
//!
//! The thread is pinned to the isolated capture core (#289) and raised to SCHED_FIFO one step BELOW
//! the capture thread (`affinity::RtThreadRole::Send`, like the E2E burn thread): the capture
//! thread always preempts it, so neither the stagger wait nor a long SpeedHQ encode can delay the
//! next capture, and the send still preempts every SCHED_OTHER task. It takes each job when the
//! capture thread blocks on its next dequeue. A wedged send now shows as the #944 emit-freeze
//! (exit 81: the capture thread keeps returning, the emit heartbeat goes stale), not as the #945
//! capture wedge (exit 79).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::FrameInfo;
use crate::frame_buffer_pool::BufferPool;
use crate::ndi::NdiSender;
use crate::send_handoff::{HandoffSlot, Offer, SendJob, SendSink, SendWindow};

/// One emitted frame's owned copy (the V4L2 mmap is only valid inside the capture callback).
pub struct SendFrame {
    pub buf: Vec<u8>,
    pub info: FrameInfo,
}

/// Buffers ever in flight: one being sent, one pending in the slot, one being filled.
pub const SEND_POOL_CAP: usize = 3;

/// How long the send thread idles with no frame before it runs the #297 re-announce check.
pub const SEND_IDLE_POLL: Duration = Duration::from_millis(500);

struct NdiSendSink {
    sender: NdiSender,
    pool: Arc<BufferPool>,
    emit_heartbeat_ns: Arc<AtomicU64>,
    heartbeat_epoch: Instant,
}

impl SendSink<SendFrame> for NdiSendSink {
    fn send(&mut self, frame: &SendFrame, timecode_100ns: i64) -> bool {
        match self
            .sender
            .send_frame_zero_copy(&frame.buf, frame.info, timecode_100ns)
        {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("Failed to send frame: {}", e);
                false
            }
        }
    }

    fn after_job(&mut self, frame: SendFrame, any_ok: bool) {
        // #944 — only a CONFIRMED send proves the NDI output is live (same shared epoch as the
        // #945 capture heartbeat, so the watchdog can subtract them).
        if any_ok {
            self.emit_heartbeat_ns.store(
                self.heartbeat_epoch.elapsed().as_nanos() as u64,
                Ordering::Relaxed,
            );
        }
        // The synchronous send has returned, so the SDK is done with the buffer.
        self.pool.put(frame.buf);
    }

    fn housekeeping(&mut self) {
        // #297 — throttled internally; a stable network is a no-op.
        if let Err(e) = self.sender.maybe_reannounce() {
            tracing::warn!("#297 NDI sender re-announce check failed: {}", e);
        }
    }
}

/// The running send thread plus the capture side of its hand-off.
pub struct NdiSendThread {
    slot: Arc<HandoffSlot<SendFrame>>,
    pool: Arc<BufferPool>,
    handle: Option<JoinHandle<()>>,
}

impl NdiSendThread {
    /// Move `sender` onto a new `ndi-send` thread. `window` is the shared 5 s send accounting;
    /// `emit_heartbeat_ns` + `heartbeat_epoch` are the #944 emit-liveness heartbeat and its epoch.
    pub fn spawn(
        sender: NdiSender,
        window: Arc<Mutex<SendWindow>>,
        emit_heartbeat_ns: Arc<AtomicU64>,
        heartbeat_epoch: Instant,
    ) -> std::io::Result<Self> {
        let slot = Arc::new(HandoffSlot::new());
        let pool = Arc::new(BufferPool::new(SEND_POOL_CAP));
        let thread_slot = Arc::clone(&slot);
        let mut sink = NdiSendSink {
            sender,
            pool: Arc::clone(&pool),
            emit_heartbeat_ns,
            heartbeat_epoch,
        };
        let handle = std::thread::Builder::new()
            .name("ndi-send".into())
            .spawn(move || {
                crate::affinity::pin_capture_thread();
                crate::affinity::set_current_thread_realtime(crate::affinity::RtThreadRole::Send);
                crate::send_handoff::run_send_loop(
                    &thread_slot,
                    &window,
                    SEND_IDLE_POLL,
                    &mut sink,
                );
            })?;
        Ok(Self {
            slot,
            pool,
            handle: Some(handle),
        })
    }

    /// Copy `data` into a pooled buffer and hand it to the send thread with its `timecodes` (send
    /// order) and absolute `deadline`. Never waits for the send thread. Returns how many frames
    /// (timecodes) an older, still untaken job carried when this one replaced it (0 = nothing lost).
    pub fn hand_off(
        &self,
        data: &[u8],
        info: FrameInfo,
        timecodes: Vec<i64>,
        deadline: Instant,
    ) -> usize {
        let mut buf = self.pool.take();
        buf.clear();
        buf.extend_from_slice(data);
        match self.slot.offer(SendJob {
            frame: SendFrame { buf, info },
            timecodes,
            deadline,
        }) {
            Offer::Accepted => 0,
            Offer::Replaced(older) => {
                let n = older.timecodes.len();
                self.pool.put(older.frame.buf);
                n
            }
            Offer::Closed(job) => {
                self.pool.put(job.frame.buf);
                0
            }
        }
    }

    /// Close the hand-off and join the thread. The last pending frame is still sent, and the
    /// sender is destroyed on the send thread when its loop ends.
    pub fn shutdown(mut self) {
        self.slot.close();
        if let Some(h) = self.handle.take() {
            if let Err(e) = h.join() {
                tracing::error!("#1242 ndi-send thread panicked during shutdown: {:?}", e);
            }
        }
    }
}

impl Drop for NdiSendThread {
    /// Dropped without [`NdiSendThread::shutdown`] (the capture loop unwinding): close the slot so
    /// the send thread's loop ends and the sender is destroyed instead of staying announced. No
    /// join here — it may run during an unwind.
    fn drop(&mut self) {
        self.slot.close();
    }
}
