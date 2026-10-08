#!/usr/bin/env python3
"""issue 1404 -- the program-audio sampler's CAPTURE side: a thread that does nothing but call
NDIlib_recv_capture_v3 and queue what it got (design issue 1404 comment 6037613222, Approach 1).

WHY: the sampler used to run ONE loop that both called the NDI capture and did the per-window work
(the FFT, the ctypes marker decode at 17-33 ms per 2 s window, the JSON writes). On a loaded dev1 that
loop stopped calling the capture for up to 2 s, and the NDI SDK, which holds about 1.3 s of audio,
dropped the oldest audio: STEP 0 (7.10.2026, issue 1404 comment 6037861831) measured four such holes
of 0.36-0.83 s in 383 s on the live `STREAM-SNV (stream)`, each a span restart = an UNKNOWN window.
A capture thread next to the same consumer, under the same load, never went over 0.5 s between two
capture calls and lost nothing.

The capture thread:
  * only blocks in the receiver's capture call (ctypes releases the GIL there), stamps the block's
    arrival time and dev1's wall-minus-monotonic offset (ONE bracketed clock read per block, see
    read_wall_offset_ns), and appends to a bounded in-process queue. It never runs the FFT, the
    marker decode or a file write (pinned by tests/python/test_program_audio_capture_1404.py);
  * bounds the queue by queued AUDIO time (CAPTURE_QUEUE_MAX_S). When it is full the NEW frame is
    dropped and counted; the drop rides on the next queued AUDIO block (`dropped_frames`,
    `dropped_100ns`; never an error item or an empty block, which the consumer does not judge), so
    the consumer treats it as a hole of exactly that length, never silently;
  * on an NDI error frame queues an error item and waits one capture timeout (never a spin);
  * dies LOUDLY: any other exception in the capture call is handed to the consumer, whose next
    get() raises it, so the sampler exits non-zero and systemd restarts it (as the single loop
    did) instead of reading "no audio" forever.

SyncCapture is the same interface WITHOUT a thread: each get() calls the receiver itself. The
calibration CLI and the loop tests drive the sampler that way (deterministic, no wall time).
"""
from __future__ import annotations

import os
import sys
import threading
import time
from collections import deque
from typing import Callable, NamedTuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402

CAPTURE_THREAD_NAME = "program-audio-capture"
# The queue holds at most this much audio. The consumer normally drains it within one window; a
# consumer stalled longer than this loses the audio that does not fit (a counted queue drop). 10 s
# is far above the 2 s stalls STEP 0 saw and still bounds the memory (~4 MB of 48 kHz stereo float32).
CAPTURE_QUEUE_MAX_S = 10.0
# The wall clock is read between two monotonic reads; a bracket wider than this means the thread was
# preempted between them, so the wall reading cannot be placed and the read is retried.
WALL_READ_MAX_BRACKET_NS = 1_000_000
WALL_READ_ATTEMPTS = 3


class Captured(NamedTuple):
    """One item of the capture side, in capture order.
    `block`: the audio block (sample_rate, samples, timestamp), None for an error item;
    `error`: the ConnectionError of an NDI error frame, else None;
    `arrival_s`: the monotonic time (s) the capture call returned it;
    `wall_offset_ns`: dev1's wall-minus-monotonic offset read with it (None: no clean read);
    `dropped_frames` / `dropped_100ns`: audio frames the full queue dropped right before this item,
    and their duration in NDI 100 ns units."""
    block: object | None
    error: BaseException | None
    arrival_s: float
    wall_offset_ns: int | None
    dropped_frames: int = 0
    dropped_100ns: float = 0.0


def read_wall_offset_ns(mono_ns: Callable[[], int] = time.monotonic_ns,
                        wall_ns: Callable[[], int] = time.time_ns,
                        attempts: int = WALL_READ_ATTEMPTS) -> int | None:
    """dev1's CLOCK_REALTIME minus CLOCK_MONOTONIC in ns, from ONE bracketed read: monotonic, wall,
    monotonic, the wall reading placed at the bracket's middle. A bracket wider than
    WALL_READ_MAX_BRACKET_NS (the thread was preempted between the reads, a reading off by up to
    half the bracket) is retried up to `attempts` times; None when none was clean. Frequency
    slewing moves both clocks alike, so this offset changes only when the wall clock is STEPPED
    (a dantesync date step) -- the signal program_audio.WallSteps reads."""
    for _ in range(attempts):
        m1 = mono_ns()
        w = wall_ns()
        m2 = mono_ns()
        if 0 <= m2 - m1 <= WALL_READ_MAX_BRACKET_NS:
            return w - (m1 + m2) // 2
    return None


def block_duration_100ns(block) -> float:
    """The audio length of a block in NDI 100 ns units; 0 for a block that is not audio (a sample
    rate <= 0 or no samples), which the consumer drops anyway."""
    try:
        n = int(block.samples.shape[0])
        sr = int(block.sample_rate)
    except (AttributeError, TypeError, ValueError, IndexError):
        return 0.0
    if n <= 0 or sr <= 0:
        return 0.0
    return n * pa.NDI_TIME_UNITS_PER_S / sr


class SyncCapture:
    """The capture side without a thread: each get() calls the receiver itself (the calibration CLI
    and the loop tests). The same items as CaptureThread; never a queue drop."""

    def __init__(self, receiver, *, mono: Callable[[], float],
                 wall_offset: Callable[[], int | None], sleep: Callable[[float], None]):
        self._rx = receiver
        self._mono = mono
        self._wall_offset = wall_offset
        self._sleep = sleep

    def get(self, timeout_ms: int) -> Captured | None:
        try:
            block = self._rx.capture(timeout_ms)
        except ConnectionError as exc:
            self._sleep(timeout_ms / 1000.0)  # an error frame can come back at once: never spin
            return Captured(None, exc, self._mono(), None)
        if block is None:
            return None
        return Captured(block, None, self._mono(), self._wall_offset())

    def qsize(self) -> int:
        return 0


class CaptureThread:
    """The capture side as its own thread (the sampler service). start() it before handing it to
    program_audio_sampler.run(capture=...), stop() it before the receiver is closed: stop() waits
    for the thread to leave the SDK call, and the receiver must never be destroyed under a running
    capture."""

    def __init__(self, receiver, *, timeout_ms: int = 500, max_queue_s: float = CAPTURE_QUEUE_MAX_S,
                 mono: Callable[[], float] = time.monotonic,
                 wall_offset: Callable[[], int | None] = read_wall_offset_ns):
        if not max_queue_s > 0:
            raise ValueError(f"CaptureThread: max_queue_s {max_queue_s!r} must be > 0")
        self._rx = receiver
        self._timeout_ms = int(timeout_ms)
        self._max_100ns = max_queue_s * pa.NDI_TIME_UNITS_PER_S
        self._mono = mono
        self._wall_offset = wall_offset
        self._items: deque[Captured] = deque()
        self._queued_100ns = 0.0
        self._cond = threading.Condition()
        self._stop = threading.Event()
        self._pending_frames = 0
        self._pending_100ns = 0.0
        self._failure: BaseException | None = None
        self.drops_total = 0
        self._thread = threading.Thread(target=self._loop, name=CAPTURE_THREAD_NAME, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def is_alive(self) -> bool:
        return self._thread.is_alive()

    def _loop(self) -> None:
        try:
            while not self._stop.is_set():
                try:
                    block = self._rx.capture(self._timeout_ms)
                except ConnectionError as exc:
                    self._put(Captured(None, exc, self._mono(), None))
                    self._stop.wait(self._timeout_ms / 1000.0)  # never spin on a repeated error frame
                    continue
                if block is None:
                    continue
                self._put(Captured(block, None, self._mono(), self._wall_offset()))
        except BaseException as exc:  # noqa: BLE001 -- handed to the consumer, which raises it
            with self._cond:
                self._failure = exc
                self._cond.notify_all()

    def _put(self, item: Captured) -> None:
        dur = block_duration_100ns(item.block) if item.block is not None else 0.0
        with self._cond:
            if dur > 0 and self._items and self._queued_100ns + dur > self._max_100ns:
                # Full: the NEW frame is dropped (the queued audio stays contiguous) and the hole is
                # handed to the next item that gets in.
                self._pending_frames += 1
                self._pending_100ns += dur
                self.drops_total += 1
                return
            if self._pending_frames and dur > 0:
                # Only an audio block carries the drop: the consumer never judges an error item or
                # an empty block, so a hole handed to one of those would be lost.
                item = item._replace(dropped_frames=self._pending_frames, dropped_100ns=self._pending_100ns)
                self._pending_frames, self._pending_100ns = 0, 0.0
            self._items.append(item)
            self._queued_100ns += dur
            self._cond.notify()

    def get(self, timeout_ms: int) -> Captured | None:
        """The oldest queued item, waiting up to `timeout_ms` for one; None when none came. Raises
        RuntimeError when the capture thread died (its exception chained)."""
        with self._cond:
            if not self._items and self._failure is None:
                self._cond.wait(timeout_ms / 1000.0)
            if self._items:
                item = self._items.popleft()
                if item.block is not None:
                    self._queued_100ns = max(0.0, self._queued_100ns - block_duration_100ns(item.block))
                return item
            if self._failure is not None:
                raise RuntimeError(f"program-audio capture thread died: {self._failure!r}") from self._failure
            return None

    def qsize(self) -> int:
        with self._cond:
            return len(self._items)

    def stop(self, join_s: float | None = None) -> bool:
        """Stop the thread and wait for it to leave the capture call. True when it stopped; False
        when it is still inside the SDK call after `join_s` (then the receiver must NOT be closed)."""
        self._stop.set()
        with self._cond:
            self._cond.notify_all()
        if self._thread.ident is None:
            return True
        self._thread.join(join_s if join_s is not None else self._timeout_ms / 1000.0 + 2.0)
        return not self._thread.is_alive()
