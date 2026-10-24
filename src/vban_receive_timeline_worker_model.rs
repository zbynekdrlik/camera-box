//! Issue 1439 — the playout worker of the obs-vban receive source as the shipped source runs it,
//! ONE model for the unit tests' `Rig` and the bench's `Replay` (review round 5: the same loop had
//! grown in both). The parity test keeps its own driver: it mirrors the C test driver line for line.
//!
//! The shipped worker (`vendor/obs-vban/src/vban-source.c` `vban_src_worker_main`) wakes only at a
//! time it scheduled itself -- the decision's `next_ns`, at most [`SLEEP_MAX_NS`] ahead -- or, while
//! it is idle (nothing to emit), when the receive thread stores a packet and signals it. At every
//! wake it takes the timeline's decisions until [`Due::None`]. A packet's arrival never runs it
//! otherwise (ROZHODNUTÉ 6347872706: the source emits on its own clock, never on a packet's arrival).

use super::*;

/// The C `VBAN_SRC_WORKER_SLEEP_MAX_NS`: the worker never sleeps longer at a time.
pub(super) const SLEEP_MAX_NS: u64 = 20 * NS_PER_MS;

/// What the worker drives: the caller's store of in-time packets and where its emissions go.
pub(super) trait Sink {
    /// The position of the oldest stored packet, `None` when the store is empty.
    fn head(&self) -> Option<u64>;
    /// One emission handed to libobs at `at` (a [`Due::Packet`] takes the store's head).
    fn emit(&mut self, at: u64, d: DueDecision);
    /// How late the worker really wakes for a wake due at `at` (its scheduling latency), 0 = on
    /// time.
    fn wake_late(&mut self, _at: u64) -> u64 {
        0
    }
}

/// The worker's state: when it wakes next.
#[derive(Debug, Default)]
pub(super) struct Worker {
    /// The next wake, `None` = idle (only a stored packet wakes it).
    wake: Option<u64>,
}

impl Worker {
    /// Every wake strictly before `until` (and at `until` itself when `inclusive`).
    pub(super) fn run_until(
        &mut self,
        t: &mut Timeline,
        sink: &mut dyn Sink,
        until: u64,
        inclusive: bool,
    ) {
        while let Some(at) = self.wake {
            if !(at < until || (inclusive && at == until)) {
                break;
            }
            self.step(t, sink, at);
        }
    }

    /// When the worker wakes next (`None` = idle).
    pub(super) fn next_wake(&self) -> Option<u64> {
        self.wake
    }

    /// The receive thread stored a packet at `now`: an idle worker is signalled and wakes.
    pub(super) fn stored(&mut self, sink: &mut dyn Sink, now: u64) {
        if self.wake.is_none() {
            self.wake = Some(now + sink.wake_late(now));
        }
    }

    /// One wake at `at`: every decision until there is nothing due, then the next wake.
    fn step(&mut self, t: &mut Timeline, sink: &mut dyn Sink, at: u64) {
        loop {
            let d = t.due(at, sink.head());
            if d.kind != Due::None {
                sink.emit(at, d);
                continue;
            }
            self.wake = (d.next_ns != 0).then(|| {
                let next = d.next_ns.min(at + SLEEP_MAX_NS);
                next + sink.wake_late(next)
            });
            return;
        }
    }
}
