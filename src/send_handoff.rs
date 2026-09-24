//! #1242 — the capture → send-thread hand-off that owns the per-camera NDI send stagger wait.
//!
//! # Why
//!
//! The send stagger (`crate::send_stagger`: camera N hands its frame to the NDI SDK
//! (N−1) × 1.2 ms after its emit-gate decision) measurably spreads the seven-camera burst off the
//! strih-lx 2.5 GbE switch port (≈ 48 vs ≈ 790 tail-dropped packets per minute). The first
//! version waited INSIDE the capture loop, with a sleep before the synchronous send. A per-frame
//! work spike (~15 ms) plus CAM7's 7.2 ms offset then overran the 16.7 ms capture slot: `OVER
//! BUDGET` windows, late frames and a 112-relock burst on the strih-lx receiver (24.9.2026).
//!
//! # What this module does
//!
//! The capture loop no longer waits. For every emitted iteration it copies the frame off the
//! V4L2 mmap, which is only valid inside the capture callback, into an owned buffer. It then
//! hands a [`SendJob`] to a single-slot [`HandoffSlot`] and returns to capture at once. The job
//! carries the frame, every NDI timecode of that iteration (the starvation repeats first, then
//! the current frame, all computed on the capture thread, so they never move) and an ABSOLUTE
//! deadline: the emit-gate decision instant plus this camera's offset, on the monotonic clock.
//!
//! A dedicated send thread runs [`run_send_loop`]. It takes the job, waits until the deadline
//! (never "sleep after the work", so the per-frame work before the hand-off never shifts the send)
//! and then calls the NDI send once per timecode, in order. The NDI send is the SYNCHRONOUS
//! `NDIlib_send_send_video_v2`, so the SDK is done with the buffer when the call returns. The
//! buffer is recycled only after that ([`run_send_loop`]'s `after_job`), so the send has no
//! use-after-free window.
//!
//! # Overlap policy
//!
//! - A NEWER job arriving while the send thread still waits on an older job's deadline makes the
//!   older job go out immediately (counted as `expedited`). A catch-up burst (the capture loop
//!   draining buffered frames back to back) is therefore sent in order and nothing is lost.
//! - A job that is still UNTAKEN when the next one arrives means the send thread is inside a send
//!   and has fallen behind. The newer job wins, and the older one is handed back to the caller
//!   ([`Offer::Replaced`]) so its buffer is recycled and the loss is counted. That is never
//!   silent: the 5 s [`window_summary`] line WARNs `REPLACED`.
//!
//! # The 5 s line
//!
//! [`window_summary`] reports the CAPTURE loop's worst per-frame work against the capture
//! interval. The offset is no longer in that budget, because the capture loop does not wait for
//! it. It also reports the send thread's side: how many jobs waited for their deadline, how many
//! were already past it, how many were expedited, the worst lateness (send start minus deadline),
//! the worst send duration, and how many jobs were replaced.
//!
//! Std-only and free of any NDI type (the frame is generic), so all of it is Tier-0 testable with
//! a fake send closure.

#![allow(unused_variables, unused_mut, dead_code, unreachable_code)]
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One emitted iteration handed from the capture thread to the send thread.
#[derive(Debug)]
pub struct SendJob<F> {
    /// The owned frame (an off-mmap copy plus whatever the sender needs to describe it).
    pub frame: F,
    /// The NDI timecodes to send this frame with, in send order: the starvation repeats (earliest
    /// slot first), then the current frame. Computed on the capture thread and never re-derived.
    pub timecodes: Vec<i64>,
    /// When the first send may start: the emit-gate decision instant plus this camera's offset.
    pub deadline: Instant,
}

/// The absolute send deadline for a frame: the emit-gate decision `anchor` plus the camera's
/// `offset`. One addition, pinned by a test so the anchor never silently becomes "now".
pub fn send_deadline(anchor: Instant, offset: Duration) -> Instant {
    todo!("#1242 RED: not implemented yet")
}

/// The result of [`HandoffSlot::offer`].
#[derive(Debug)]
pub enum Offer<F> {
    /// The slot was empty; the job is now pending.
    Accepted,
    /// The slot still held an untaken older job. The newer job replaced it, and the older one is
    /// handed back unsent so the caller can recycle its buffer and count the loss.
    Replaced(SendJob<F>),
    /// The slot is closed (shutdown). The job is handed back unsent.
    Closed(SendJob<F>),
}

/// The result of [`HandoffSlot::take`].
#[derive(Debug)]
pub enum Take<F> {
    Job(SendJob<F>),
    /// Nothing arrived within the idle timeout (the caller does its housekeeping).
    Idle,
    /// The slot is closed and empty: the send thread ends.
    Closed,
}

/// The result of [`HandoffSlot::wait_until`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The deadline passed with no newer job pending.
    DeadlineReached,
    /// A newer job arrived while waiting: send the held one now.
    NewerPending,
    /// The slot was closed while waiting: send the held one now (shutdown drain).
    Closed,
}

struct SlotState<F> {
    pending: Option<SendJob<F>>,
    closed: bool,
}

/// A single-slot, newest-wins hand-off between the capture thread and the send thread. `offer`
/// never blocks on the send thread (it only takes the slot's own short lock).
pub struct HandoffSlot<F> {
    state: Mutex<SlotState<F>>,
    changed: Condvar,
}

impl<F> Default for HandoffSlot<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F> HandoffSlot<F> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                pending: None,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    // No user code ever runs under this lock, so a poisoned lock still holds a consistent state.
    fn lock(&self) -> MutexGuard<'_, SlotState<F>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hand a job to the send thread. Returns at once; see [`Offer`] for the outcomes.
    pub fn offer(&self, job: SendJob<F>) -> Offer<F> {
        todo!("#1242 RED: not implemented yet")
    }

    /// Close the slot (shutdown). A pending job is still handed to the send thread by the next
    /// [`take`](Self::take), so the last frame drains before the thread ends.
    pub fn close(&self) {
        todo!("#1242 RED: not implemented yet")
    }

    /// Block until a job is pending (returned even after `close`, so it drains), the slot is
    /// closed and empty, or `idle` passes with nothing to do.
    pub fn take(&self, idle: Duration) -> Take<F> {
        todo!("#1242 RED: not implemented yet")
    }

    /// Wait (the send thread, holding a taken job) until `deadline`, returning early when a newer
    /// job arrives or the slot closes. A newer job is checked FIRST, so a catch-up burst never
    /// waits out an older frame's offset.
    pub fn wait_until(&self, deadline: Instant) -> Wait {
        todo!("#1242 RED: not implemented yet")
    }
}

/// How a job's first send started relative to its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStart {
    /// The send thread waited for the deadline (the normal staggered case).
    Waited,
    /// The deadline had already passed when the job was taken (offset 0, or a late job).
    PastDeadline,
    /// Sent before its deadline because a newer job arrived (or the slot closed).
    Expedited,
}

/// How long after its `deadline` a send `started` (zero when it started on time or early).
pub fn send_lateness(deadline: Instant, started: Instant) -> Duration {
    todo!("#1242 RED: not implemented yet")
}

/// The send thread's per-5 s window, shared with the capture loop's report through a mutex.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SendWindow {
    pub waited: u64,
    pub past_deadline: u64,
    pub expedited: u64,
    /// The worst lateness (send start minus deadline), in ms.
    pub max_lateness_ms: f64,
    /// The worst duration of one job's sends (all its timecodes), in ms.
    pub max_send_ms: f64,
}

impl SendWindow {
    /// Record one job: how its first send started, its lateness and how long its sends took.
    pub fn note_job(&mut self, start: SendStart, lateness: Duration, send: Duration) {
        todo!("#1242 RED: not implemented yet")
    }

    /// Jobs recorded this window.
    pub fn jobs(&self) -> u64 {
        todo!("#1242 RED: not implemented yet")
    }

    /// Drain the window (returns it and resets to empty).
    pub fn take(&mut self) -> SendWindow {
        todo!("#1242 RED: not implemented yet")
    }
}

/// Lock a shared [`SendWindow`]; a poisoned lock still holds plain counters, so keep using it.
pub fn lock_window(w: &Mutex<SendWindow>) -> MutexGuard<'_, SendWindow> {
    w.lock().unwrap_or_else(|e| e.into_inner())
}

/// The capture loop's own per-5 s window: its worst per-frame work and the jobs the send thread
/// never got to (newest-wins replacements).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaptureWindow {
    /// The worst emitted iteration's work in the capture callback, in ms.
    pub max_work_ms: f64,
    /// Jobs replaced unsent in the slot.
    pub replaced_jobs: u64,
    /// Frames (timecodes) those replaced jobs carried.
    pub replaced_frames: u64,
}

impl CaptureWindow {
    /// Record one emitted iteration's callback work. A non-finite or negative reading is ignored.
    pub fn note_work(&mut self, work_ms: f64) {
        todo!("#1242 RED: not implemented yet")
    }

    /// Record one job replaced unsent, carrying `frames` timecodes.
    pub fn note_replaced(&mut self, frames: usize) {
        todo!("#1242 RED: not implemented yet")
    }

    /// Drain the window (returns it and resets to empty).
    pub fn take(&mut self) -> CaptureWindow {
        todo!("#1242 RED: not implemented yet")
    }
}

/// A send that starts this fraction of a capture interval past its deadline WARNs `LATE`: the
/// frame's arrival at the receiver moved by more than half a frame.
pub const LATE_WARN_FRACTION: f64 = 0.5;

/// The 5 s summary line and whether it is a WARN. WARN flags:
///
/// - `OVER BUDGET`: the capture loop's worst per-frame work reached a capture interval;
/// - `SEND OVER BUDGET`: one job's sends took a whole capture interval (the send thread cannot keep
///   up at that rate);
/// - `LATE`: a send started [`LATE_WARN_FRACTION`] of a capture interval past its deadline;
/// - `REPLACED`: a job was replaced unsent (newest-wins: the send thread fell behind).
///
/// `capture_interval_ms <= 0` never WARNs on the three interval terms.
pub fn window_summary(
    capture: &CaptureWindow,
    send: &SendWindow,
    offset_us: u64,
    capture_interval_ms: f64,
) -> (String, bool) {
    todo!("#1242 RED: not implemented yet")
}

/// For a thread that already owns its queue (the E2E burn thread, fed by a blocking ring that never
/// drops): sleep until `deadline` if it is still ahead, and say how the send started.
pub fn sleep_until(deadline: Instant) -> SendStart {
    todo!("#1242 RED: not implemented yet")
}

/// What the send thread does with a job. One owner (`&mut self`) holds the NDI sender, so the send
/// and the housekeeping (the #297 re-announce) can both use it.
pub trait SendSink<F> {
    /// One NDI send of `frame` with `timecode_100ns`; `true` when the send was accepted.
    fn send(&mut self, frame: &F, timecode_100ns: i64) -> bool;
    /// Called once per job after ALL its sends returned. Recycle the buffer here (the synchronous
    /// send is done with it), and stamp the emit-liveness heartbeat only when `any_ok`.
    fn after_job(&mut self, frame: F, any_ok: bool);
    /// Called after every job and on every idle timeout.
    fn housekeeping(&mut self);
}

/// The send thread's loop: take a job, wait until its absolute deadline (returning early when a
/// newer job arrives), send every timecode in order, record the window, recycle. `idle` bounds how
/// long the thread sleeps with nothing to do before it runs `housekeeping`. Ends when the slot is
/// closed and drained.
pub fn run_send_loop<F, S: SendSink<F>>(
    slot: &HandoffSlot<F>,
    window: &Mutex<SendWindow>,
    idle: Duration,
    sink: &mut S,
) {
    todo!("#1242 RED: not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn job(frame: u32, timecodes: &[i64], deadline: Instant) -> SendJob<u32> {
        SendJob {
            frame,
            timecodes: timecodes.to_vec(),
            deadline,
        }
    }

    #[test]
    fn the_deadline_is_the_gate_anchor_plus_the_offset_1242() {
        let anchor = Instant::now();
        let d = send_deadline(anchor, Duration::from_micros(7200));
        assert_eq!(d.duration_since(anchor), Duration::from_micros(7200));
        assert_eq!(send_deadline(anchor, Duration::ZERO), anchor);
    }

    #[test]
    fn an_untaken_job_is_replaced_by_the_newer_one_and_handed_back_1242() {
        let slot = HandoffSlot::new();
        let now = Instant::now();
        assert!(matches!(slot.offer(job(1, &[10], now)), Offer::Accepted));
        match slot.offer(job(2, &[20, 21], now)) {
            Offer::Replaced(old) => {
                assert_eq!(old.frame, 1, "the OLDER job comes back for recycling");
                assert_eq!(old.timecodes, vec![10]);
            }
            other => panic!("expected Replaced, got {other:?}"),
        }
        match slot.take(Duration::from_millis(10)) {
            Take::Job(j) => {
                assert_eq!(j.frame, 2, "the newest job wins");
                assert_eq!(j.timecodes, vec![20, 21]);
            }
            other => panic!("expected the newest job, got {other:?}"),
        }
    }

    #[test]
    fn take_times_out_idle_and_ends_after_close_but_drains_first_1242() {
        let slot: HandoffSlot<u32> = HandoffSlot::new();
        assert!(matches!(slot.take(Duration::from_millis(5)), Take::Idle));
        assert!(matches!(
            slot.offer(job(7, &[1], Instant::now())),
            Offer::Accepted
        ));
        slot.close();
        assert!(
            matches!(slot.take(Duration::from_millis(5)), Take::Job(j) if j.frame == 7),
            "a job pending at close still drains"
        );
        assert!(matches!(slot.take(Duration::from_millis(5)), Take::Closed));
        assert!(matches!(
            slot.offer(job(8, &[1], Instant::now())),
            Offer::Closed(j) if j.frame == 8
        ));
    }

    #[test]
    fn wait_until_reaches_the_deadline_when_nothing_newer_arrives_1242() {
        let slot: HandoffSlot<u32> = HandoffSlot::new();
        let deadline = Instant::now() + Duration::from_millis(15);
        assert_eq!(slot.wait_until(deadline), Wait::DeadlineReached);
        assert!(Instant::now() >= deadline);
        // A deadline in the past returns at once.
        assert_eq!(
            slot.wait_until(Instant::now() - Duration::from_millis(1)),
            Wait::DeadlineReached
        );
    }

    #[test]
    fn wait_until_returns_early_for_a_newer_job_or_a_close_1242() {
        let slot = Arc::new(HandoffSlot::new());
        let deadline = Instant::now() + Duration::from_secs(5);
        let s = Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _ = s.offer(job(2, &[2], Instant::now()));
        });
        assert_eq!(slot.wait_until(deadline), Wait::NewerPending);
        assert!(Instant::now() < deadline);
        t.join().unwrap();

        let slot: Arc<HandoffSlot<u32>> = Arc::new(HandoffSlot::new());
        let s = Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            s.close();
        });
        assert_eq!(slot.wait_until(deadline), Wait::Closed);
        t.join().unwrap();
    }

    /// A fake sink recording `(frame, timecode, send instant)`, the recycled `(frame, any_ok)` and
    /// the housekeeping ticks.
    struct FakeSink {
        send_ok: bool,
        sent: Arc<Mutex<Vec<(u32, i64, Instant)>>>,
        recycled: Arc<Mutex<Vec<(u32, bool)>>>,
        housekeeping: Arc<Mutex<u32>>,
    }

    impl SendSink<u32> for FakeSink {
        fn send(&mut self, frame: &u32, timecode_100ns: i64) -> bool {
            self.sent
                .lock()
                .unwrap()
                .push((*frame, timecode_100ns, Instant::now()));
            self.send_ok
        }
        fn after_job(&mut self, frame: u32, any_ok: bool) {
            self.recycled.lock().unwrap().push((frame, any_ok));
        }
        fn housekeeping(&mut self) {
            *self.housekeeping.lock().unwrap() += 1;
        }
    }

    type Sent = Arc<Mutex<Vec<(u32, i64, Instant)>>>;
    type Recycled = Arc<Mutex<Vec<(u32, bool)>>>;
    type Spawned = (
        Arc<HandoffSlot<u32>>,
        Arc<Mutex<SendWindow>>,
        Sent,
        Recycled,
        Arc<Mutex<u32>>,
        std::thread::JoinHandle<()>,
    );

    /// Runs the real `run_send_loop` on its own thread with a [`FakeSink`].
    fn spawn_loop(send_ok: bool) -> Spawned {
        let slot = Arc::new(HandoffSlot::new());
        let window = Arc::new(Mutex::new(SendWindow::default()));
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let recycled: Recycled = Arc::new(Mutex::new(Vec::new()));
        let ticks = Arc::new(Mutex::new(0u32));
        let mut sink = FakeSink {
            send_ok,
            sent: Arc::clone(&sent),
            recycled: Arc::clone(&recycled),
            housekeeping: Arc::clone(&ticks),
        };
        let (s, w) = (Arc::clone(&slot), Arc::clone(&window));
        let h = std::thread::spawn(move || {
            run_send_loop(&s, &w, Duration::from_millis(5), &mut sink);
        });
        (slot, window, sent, recycled, ticks, h)
    }

    #[test]
    fn the_send_waits_for_its_deadline_then_sends_every_timecode_in_order_1242() {
        let (slot, window, sent, recycled, _ticks, h) = spawn_loop(true);
        let deadline = Instant::now() + Duration::from_millis(30);
        assert!(matches!(
            slot.offer(job(1, &[100, 101, 102], deadline)),
            Offer::Accepted
        ));
        // Generous: a loaded CI runner must not turn the close into an early (expedited) send.
        std::thread::sleep(Duration::from_millis(400));
        slot.close();
        h.join().unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(
            sent.iter().map(|s| s.1).collect::<Vec<_>>(),
            vec![100, 101, 102],
            "the starvation repeats first, then the current frame"
        );
        assert!(
            sent[0].2 >= deadline,
            "the first send must not start before its deadline"
        );
        assert_eq!(*recycled.lock().unwrap(), vec![(1, true)]);
        let w = window.lock().unwrap();
        assert_eq!((w.waited, w.past_deadline, w.expedited), (1, 0, 0));
    }

    #[test]
    fn a_job_already_past_its_deadline_is_sent_at_once_1242() {
        let (slot, window, sent, _recycled, _ticks, h) = spawn_loop(true);
        let offered = Instant::now();
        assert!(matches!(
            slot.offer(job(1, &[5], offered - Duration::from_millis(3))),
            Offer::Accepted
        ));
        std::thread::sleep(Duration::from_millis(60));
        slot.close();
        h.join().unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
        let w = window.lock().unwrap();
        assert_eq!((w.waited, w.past_deadline, w.expedited), (0, 1, 0));
        assert!(w.max_lateness_ms >= 3.0, "{}", w.max_lateness_ms);
    }

    #[test]
    fn a_newer_frame_expedites_the_held_one_so_a_burst_is_sent_in_order_1242() {
        let (slot, window, sent, recycled, _ticks, h) = spawn_loop(true);
        let far = Instant::now() + Duration::from_millis(1500);
        assert!(matches!(slot.offer(job(1, &[1], far)), Offer::Accepted));
        // Let the send thread take job 1 and start waiting on its deadline.
        std::thread::sleep(Duration::from_millis(150));
        let far2 = Instant::now() + Duration::from_millis(100);
        assert!(
            matches!(slot.offer(job(2, &[2], far2)), Offer::Accepted),
            "the held job was already taken, so the slot is free: nothing is replaced"
        );
        std::thread::sleep(Duration::from_millis(500));
        slot.close();
        h.join().unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(
            sent.iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![1, 2],
            "both frames are sent, oldest first"
        );
        assert!(
            sent[0].2 < far,
            "the held frame goes out at once instead of waiting out its offset"
        );
        assert!(
            sent[1].2 >= far2,
            "the newer frame still waits for its own deadline"
        );
        assert_eq!(recycled.lock().unwrap().len(), 2);
        let w = window.lock().unwrap();
        assert_eq!((w.waited, w.expedited), (1, 1));
    }

    #[test]
    fn the_hand_off_never_waits_for_the_send_thread_1242() {
        // The send thread holds a job with a far deadline; the capture side keeps offering. Every
        // offer returns at once (the capture loop never waits for the stagger any more).
        let (slot, _window, _sent, _recycled, _ticks, h) = spawn_loop(true);
        let far = Instant::now() + Duration::from_millis(300);
        for i in 0..5u32 {
            let t = Instant::now();
            let _ = slot.offer(job(i, &[i as i64], far));
            assert!(
                t.elapsed() < Duration::from_millis(20),
                "offer {i} blocked for {:?}",
                t.elapsed()
            );
        }
        slot.close();
        h.join().unwrap();
    }

    #[test]
    fn a_failed_send_is_not_reported_as_ok_1242() {
        let (slot, _window, sent, recycled, _ticks, h) = spawn_loop(false);
        let _ = slot.offer(job(9, &[1, 2], Instant::now()));
        std::thread::sleep(Duration::from_millis(40));
        slot.close();
        h.join().unwrap();
        assert_eq!(
            sent.lock().unwrap().len(),
            2,
            "both timecodes were attempted"
        );
        assert_eq!(
            *recycled.lock().unwrap(),
            vec![(9, false)],
            "the buffer is recycled, but the emit heartbeat must not advance"
        );
    }

    #[test]
    fn housekeeping_runs_while_idle_and_after_each_job_1242() {
        let (slot, _window, _sent, _recycled, ticks, h) = spawn_loop(true);
        std::thread::sleep(Duration::from_millis(60));
        let idle_ticks = *ticks.lock().unwrap();
        assert!(
            idle_ticks >= 2,
            "idle housekeeping ran {idle_ticks} time(s)"
        );
        let _ = slot.offer(job(1, &[1], Instant::now()));
        std::thread::sleep(Duration::from_millis(60));
        slot.close();
        h.join().unwrap();
        assert!(*ticks.lock().unwrap() > idle_ticks);
    }

    #[test]
    fn sleep_until_waits_only_for_a_future_deadline_1242() {
        let deadline = Instant::now() + Duration::from_millis(10);
        assert_eq!(sleep_until(deadline), SendStart::Waited);
        assert!(Instant::now() >= deadline);
        let t = Instant::now();
        assert_eq!(
            sleep_until(t - Duration::from_millis(1)),
            SendStart::PastDeadline
        );
        assert!(t.elapsed() < Duration::from_millis(5));
    }

    #[test]
    fn send_window_counts_and_drains_1242() {
        let mut w = SendWindow::default();
        w.note_job(
            SendStart::Waited,
            Duration::ZERO,
            Duration::from_micros(5200),
        );
        w.note_job(
            SendStart::PastDeadline,
            Duration::from_micros(800),
            Duration::from_micros(4000),
        );
        w.note_job(
            SendStart::Expedited,
            Duration::ZERO,
            Duration::from_micros(6100),
        );
        assert_eq!(w.jobs(), 3);
        let d = w.take();
        assert_eq!((d.waited, d.past_deadline, d.expedited), (1, 1, 1));
        assert!((d.max_lateness_ms - 0.8).abs() < 1e-9);
        assert!((d.max_send_ms - 6.1).abs() < 1e-9);
        assert_eq!(w, SendWindow::default());
    }

    #[test]
    fn capture_window_counts_and_drains_1242() {
        let mut c = CaptureWindow::default();
        c.note_work(3.0);
        c.note_work(f64::NAN);
        c.note_work(-1.0);
        c.note_work(2.0);
        c.note_replaced(3);
        let d = c.take();
        assert_eq!(d.max_work_ms, 3.0);
        assert_eq!((d.replaced_jobs, d.replaced_frames), (1, 3));
        assert_eq!(c, CaptureWindow::default());
    }

    #[test]
    fn the_offset_is_no_longer_in_the_capture_budget_1242() {
        // The live 24.9. OVER BUDGET: 15.3 ms of work + 7.2 ms offset vs 16.7 ms. With the wait on
        // the send thread only the work counts: 15.3 ms < 16.7 ms is INFO.
        let capture = CaptureWindow {
            max_work_ms: 15.3,
            ..CaptureWindow::default()
        };
        let send = SendWindow {
            waited: 300,
            max_lateness_ms: 0.05,
            max_send_ms: 6.0,
            ..SendWindow::default()
        };
        let (line, warn) = window_summary(&capture, &send, 7200, 16.667);
        assert!(!warn, "{line}");
        assert_eq!(
            line,
            "#1242 send stagger: offset=7200 us, capture loop max work 15.3 ms vs capture interval 16.7 ms; send thread 300 job(s) (300 waited for the offset / 0 already past it / 0 expedited by a newer frame), max lateness 0.05 ms, max send 6.0 ms, 0 replaced"
        );
    }

    #[test]
    fn window_summary_warns_on_each_budget_term_1242() {
        let ok_send = SendWindow {
            waited: 300,
            max_send_ms: 5.0,
            ..SendWindow::default()
        };
        let full = CaptureWindow {
            max_work_ms: 16.7,
            ..CaptureWindow::default()
        };
        let (line, warn) = window_summary(&full, &ok_send, 7200, 16.667);
        assert!(
            warn && line.contains("OVER BUDGET: the capture loop"),
            "{line}"
        );

        let slow = SendWindow {
            max_send_ms: 17.0,
            ..ok_send.clone()
        };
        let (line, warn) = window_summary(&CaptureWindow::default(), &slow, 7200, 16.667);
        assert!(warn && line.contains("SEND OVER BUDGET"), "{line}");

        let late = SendWindow {
            max_lateness_ms: 8.4,
            ..ok_send.clone()
        };
        let (line, warn) = window_summary(&CaptureWindow::default(), &late, 7200, 16.667);
        assert!(warn && line.contains("LATE:"), "{line}");

        let replaced = CaptureWindow {
            replaced_jobs: 1,
            replaced_frames: 2,
            ..CaptureWindow::default()
        };
        let (line, warn) = window_summary(&replaced, &ok_send, 7200, 16.667);
        assert!(
            warn && line.contains("REPLACED: 2 frame(s) never sent"),
            "{line}"
        );

        // No capture interval known: only REPLACED can WARN.
        let (_, warn) = window_summary(&full, &slow, 7200, 0.0);
        assert!(!warn);
    }
}
