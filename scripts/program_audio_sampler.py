#!/usr/bin/env python3
"""issue 1404 -- the stream program-audio sampler (a long-running --user service on strih-lx).

Receives the stream program's audio from stream OBS's own NDI program output (the
`genlock-ndi-output 'stream'` sender, NDI name `STREAM-SNV (stream)` at 10.77.9.204 -- read live
with avahi on 6.10.2026), audio-only, read-only (a receiver like any NDI monitor; nothing on the
rig changes). Every WINDOW_S (2 s) of audio it computes `rms_dbfs` + `outside_band_pct`, classifies
MEASUREMENT | FOREIGN | SILENT | UNKNOWN (scripts/program_audio.py) and atomically rewrites
`<serve dir>/program-audio.json`, which the sampler serves itself (program_audio_http.py,
`http://<host>:8891/program-audio.json`, ages recomputed per request). Consumers call
`scripts/program_audio_guard.py` and stop the YouTube broadcast on anything but MEASUREMENT/SILENT.

The rules below were found and measured while the sampler ran on dev1 (until 8.10.2026), so their
text says "dev1". It runs only on strih-lx now (the fleet's date master), where "dev1's arrival
time" and "dev1's own wall-clock step" mean strih-lx's own.

* FOREIGN LATCH: every payload carries `last_foreign_ts_utc`, the newest FOREIGN window, so a gate
  that polls every ~10 s still sees a FOREIGN window that fell between two of its polls.
* UNKNOWN whenever it is not sampling: at start, after NO_AUDIO_TIMEOUT_S without audio (sender
  down, the rig away at an event, mDNS not seeing it; rewritten every window so the file never
  reads fresh while it holds an old verdict), when libndi cannot be loaded, and on stop.
* mDNS ONLY, enforced: the receiver runs with a private, empty NDI_CONFIG_DIR, so no NDI extra-IP
  list can make it open a TCP discovery connection into a sender (.claude/rules/ndi-discovery.md).
* Logging: a line per verdict CHANGE, the first NDI error frame / bad sample rate of a run, a line
  per span restart (`audio timeline discontinuity`, the no-timestamp `receive gap`), per late
  burst the timeline proved continuous and per bridged hole (`audio timeline hole ... bridged with N
  zero samples`), and a summary every LOG_SUMMARY_S (timeline_breaks, late_bursts, receive_gaps,
  max_offset_ms = the largest |offset| of a frame that continued, the margin to the tolerance,
  holes_bridged and bridged_ms; a restart after an NDI error frame shows as error_frames) -- never
  a line per 2 s window (~43 000 a day).

* MARKER REQUIREMENT (ROZHODNUTÉ issue 1404 comments 6026577906 + 6026826572): MEASUREMENT also
  needs the cam2 QPSK marker itself -- a timecode chain of >= pa.MARKER_CHAIN_MIN markers over the
  trailing 4 s of contiguous non-silent audio, decoded by the dock's own decoder through the
  `scripts/qpsk_guard_shim.cpp` library (scripts/program_audio_marker.py). Until 4 s of audio
  arrived since the start or a span restart nothing reads MEASUREMENT: a window whose spectrum alone
  says FOREIGN reads FOREIGN and starts the latch (ROZHODNUTÉ 6027706292 item 1), every other one
  reads UNKNOWN. A missing / unloadable library = UNKNOWN and exit 1, like a missing libndi.
* SPAN RESTARTS (design issue 1404 comment 6030385284): continuity is judged on the SENDER's NDI
  audio timeline (pa.frame_continues over each frame's SDK timestamp), never on dev1's arrival
  time: a late burst after dev1 starved the sampler keeps the span; a hole AHEAD of the timeline up
  to pa.HOLE_BRIDGE_MAX_MS (lost frames: live, two NDI frames at a time while dev1 was loaded) is
  BRIDGED (design issue 1404 comment 6036098516): round(offset * sr) zeros go into the window before
  the frame and the span is kept, the marker chain decoded over the real samples only
  (decode_real_samples); a frame behind the timeline beyond the tolerance, a hole over 250 ms, a
  sender restart and a larger dantesync date step restart it whatever the arrival time
  (one warm-up). Only a frame with no timestamp falls back to the arrival gap (no audio block for
  over RECEIVE_GAP_S), and an NDI error frame always restarts the span.
* program-audio.json carries the additive `holes_bridged` / `bridged_ms` / `queue_drops` (since the
  sampler started; null while it is not sampling).
* CAPTURE THREAD (design issue 1404 comment 6037613222): main() runs the NDI capture in its own
  thread (scripts/program_audio_capture.py), which only calls NDIlib_recv_capture_v3 and queues; this
  loop is the consumer. A full queue's drop is a hole of known length (`queue overflow` line,
  `queue_drops`). run(capture=None) keeps the capture call in the loop (SyncCapture): the
  calibration CLI and the loop tests.
* DATE_STEP: a forward timestamp jump that matches dev1's own wall-clock step (read once per block
  by the capture path) is the fleet date step, nothing lost: no zeros, no restart.
* HOLED SPAN (ROZHODNUTÉ issue 1404 comment 6037765523): a short chain over a span holding bridged
  audio reads UNKNOWN, never FOREIGN on its own.
* SENDER STALL (ROZHODNUTÉ on issue 1404, Design-question 6037861831): a frame AHEAD of the timeline
  (no matching wall step) is held with up to pa.STALL_LOOKAHEAD_FRAMES following frames (HeldFrames,
  pa.resolve_ahead). Back within the tolerance = the stream OBS stamped late after an audio-thread
  stall, nothing lost: no zeros, the span kept, counted (`sender_stalls`). Otherwise the hole is the
  smallest offset over them: bridged or a restart, as before.

Usage (systemd/program-audio-sampler.strih-lx.service, installed by setup-strih step 16e):
  program_audio_sampler.py [--source "STREAM-SNV (stream)"] [--serve-dir DIR] [--lib PATH]
                           [--marker-shim PATH] [--http-port 8891] [--http-bind 0.0.0.0]
"""
from __future__ import annotations

import argparse
import os
import shutil
import signal
import sys
import tempfile
import time
from collections import deque
from datetime import datetime, timezone
from typing import Callable, NamedTuple

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402
import program_audio_capture as pac  # noqa: E402
import program_audio_http as pah  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

DEFAULT_SOURCE = "STREAM-SNV (stream)"
SOURCE_ENV = "PROGRAM_AUDIO_SOURCE"
# The sampler's own read-only endpoint (program_audio_http.py; host-agnostic, on strih-lx): port 0 =
# no endpoint. The serve dir is the sampler's own: $PROGRAM_AUDIO_SERVE_DIR, else
# $XDG_RUNTIME_DIR/program-audio-sampler (tmpfs, created 0700). Never the dev1 lease server's serve
# dir: that one served the verdict at http://dev1:8890 until the route was retired (8.10.2026).
DEFAULT_HTTP_PORT = 8891
DEFAULT_HTTP_BIND = "0.0.0.0"
HTTP_PORT_ENV = "PROGRAM_AUDIO_HTTP_PORT"
HTTP_BIND_ENV = "PROGRAM_AUDIO_HTTP_BIND"
SERVE_DIR_ENV = "PROGRAM_AUDIO_SERVE_DIR"
SERVE_DIR_NAME = "program-audio-sampler"
NO_AUDIO_TIMEOUT_S = 5.0
CAPTURE_TIMEOUT_MS = 500
LOG_SUMMARY_S = 600.0
# The FALLBACK continuity rule, for a frame whose sender timestamp is undefined (pa.UNKNOWN_TS): no
# audio block for longer than this between two blocks = a receive gap, and the audio around it is
# never stitched into one marker span (the chain would read the gap as a jump in the index clock).
# With timestamps the sender timeline decides instead (pa.frame_continues): on a busy dev1 the
# sampler was starved for 1.0-2.0 s and the SDK's queued audio arrived as one late burst, which
# this arrival rule read as 57 spurious restarts in 6 h (issue 1404, 7.10.2026).
RECEIVE_GAP_S = 1.0


def log(msg: str) -> None:
    # A service context can hand this a dead stdout pipe (the bundle-state-server #829 class) --
    # logging must never take the sampler down, and it cannot log its own failure (stdout is the
    # broken resource).
    # airuleset:script-ok a dead stdout is the one resource this swallow is for; it cannot be logged
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", flush=True)
    except (OSError, ValueError):
        pass


class WindowAccumulator:
    """Collects NDI-sized audio blocks into exact WINDOW_S windows. A sample-rate or channel-count
    change drops the partial window, so no window ever mixes two formats. Samples pushed with
    `real=False` are the zeros that bridge a sender-timeline hole (design issue 1404 comment
    6036098516): each window comes with a mask of its REAL samples (None when every sample is
    real), so the marker chain is decoded over real samples only."""

    def __init__(self, window_s: float):
        self.window_s = window_s
        self.reset()

    def reset(self) -> None:
        """Drop the partial window (a span restart: audio before and after it is never mixed)."""
        self._parts: list[np.ndarray] = []
        self._real: list[np.ndarray] = []
        self._have = 0
        self._fmt: tuple[int, int] | None = None

    def push(self, samples: np.ndarray, sample_rate: int,
             real: bool = True) -> list[tuple[np.ndarray, int, np.ndarray | None]]:
        """Add a block; every window it completes as (samples, sample_rate, real mask or None)."""
        if samples.ndim != 2:
            raise ValueError(f"WindowAccumulator: samples must be (n, channels), got {samples.shape}")
        if sample_rate <= 0:
            raise ValueError(f"WindowAccumulator: sample_rate={sample_rate} is not a rate")
        fmt = (int(sample_rate), int(samples.shape[1]))
        if fmt != self._fmt:
            self._parts, self._real, self._have, self._fmt = [], [], 0, fmt
        self._parts.append(samples)
        self._real.append(np.full(samples.shape[0], bool(real)))
        self._have += samples.shape[0]
        need = int(round(self.window_s * fmt[0]))
        out = []
        while self._have >= need:
            buf = np.concatenate(self._parts, axis=0)
            mask = np.concatenate(self._real)
            win_real = mask[:need]
            out.append((buf[:need], fmt[0], None if win_real.all() else win_real))
            rest, rest_real = buf[need:], mask[need:]
            self._parts = [rest] if rest.shape[0] else []
            self._real = [rest_real] if rest.shape[0] else []
            self._have = rest.shape[0]
        return out


class MarkerSpan:
    """The trailing `span_s` of contiguous NON-SILENT audio the marker chain is read over, and the
    warm-up: the seconds of audio received since the start, a span restart or a format change.

    * Warm-up: until `span_s` of audio arrived, nothing reads MEASUREMENT (ROZHODNUTÉ issue 1404
      comment 6026577906); a window whose spectrum alone says FOREIGN reads FOREIGN (6027706292
      item 1), every other one UNKNOWN.
    * A SILENT window empties the span: silence carries no marker, so the window after it holds only
      its own markers and must not be judged against the full span (it reads UNKNOWN until the span
      is full again; the s3-A-vod fixture is this start-of-stream case).
    * `reset()` = a span restart (a sender-timeline discontinuity, a receive gap without a
      timestamp, an NDI error frame): both start over."""

    def __init__(self, window_s: float = pa.WINDOW_S, span_s: float = pa.MARKER_SPAN_S):
        k = span_s / window_s
        if k < 1 or abs(k - round(k)) > 1e-9:
            raise ValueError(f"MarkerSpan: span {span_s} s is not a whole number of {window_s} s windows")
        self.window_s = window_s
        self.span_s = span_s
        self.windows = int(round(k))
        self.reset()

    def reset(self) -> None:
        self._wins: deque[np.ndarray] = deque(maxlen=self.windows)
        self._reals: deque[np.ndarray | None] = deque(maxlen=self.windows)
        self._fmt: tuple[int, int] | None = None
        self.audio_s = 0.0
        self.real: np.ndarray | None = None  # the last full span's real-sample mask (None = all real)

    @property
    def warm(self) -> bool:
        return self.audio_s >= self.span_s - 1e-9

    @property
    def non_silent_s(self) -> float:
        return len(self._wins) * self.window_s

    def push(self, win: np.ndarray, sample_rate: int, silent: bool,
             real: np.ndarray | None = None) -> np.ndarray | None:
        """Add one window (`real`: its real-sample mask, None = all real); the full span (oldest
        first) once it holds `span_s` of non-silent audio. `self.real` is then that span's
        real-sample mask, None when every sample of it is real (no bridged hole)."""
        fmt = (int(sample_rate), int(win.shape[1]))
        if fmt != self._fmt:
            self.reset()
            self._fmt = fmt
        self.audio_s += self.window_s
        self.real = None
        if silent:
            self._wins.clear()
            self._reals.clear()
            return None
        self._wins.append(win)
        self._reals.append(real)
        if len(self._wins) < self.windows:
            return None
        if any(r is not None for r in self._reals):
            self.real = np.concatenate([np.ones(w.shape[0], dtype=bool) if r is None else r
                                        for w, r in zip(self._wins, self._reals)])
        return np.concatenate(list(self._wins), axis=0)


class HeldFrames:
    """The frames the sender-stall look-ahead holds for pa.resolve_ahead: the frame AHEAD of the
    sender's timeline first, then up to pa.STALL_LOOKAHEAD_FRAMES frames that followed it, each as
    (Captured item, audio block). Pure bookkeeping: run() decides and takes them in."""

    def __init__(self):
        self._frames: list[tuple] = []

    def __bool__(self) -> bool:
        return bool(self._frames)

    def __len__(self) -> int:
        return len(self._frames)

    def hold(self, item, block) -> None:
        self._frames.append((item, block))

    def can_join(self, item, block) -> bool:
        """A frame joins only when it continues the same stream: a sender stamp, the first frame's
        rate and channel count, and no known queue drop of its own (that hole is judged on its own,
        after the held frames are decided)."""
        _item0, block0 = self._frames[0]
        return (item.dropped_frames == 0 and pa.timestamp_defined(block.timestamp)
                and (block.sample_rate, block.samples.shape[1])
                == (block0.sample_rate, block0.samples.shape[1]))

    def resolve(self, prev, complete: bool):
        """pa.resolve_ahead over the held frames after the previous frame `prev` = (timestamp,
        samples, sample_rate, channels); None while undecided."""
        p_ts, p_n, p_sr, _ch = prev
        frames = [(block.timestamp, block.samples.shape[0], item.dropped_100ns if k == 0 else 0.0)
                  for k, (item, block) in enumerate(self._frames)]
        return pa.resolve_ahead(p_ts, p_n, p_sr, frames, pa.continuity_tolerance_100ns(p_n, p_sr),
                                complete=complete)

    def take(self) -> list[tuple]:
        frames, self._frames = self._frames, []
        return frames


def run(receiver, serve_dir: str, *, source: str, decoder, mono: Callable[[], float] = time.monotonic,
        max_loops: int | None = None, on_write: Callable[[dict], None] | None = None,
        log: Callable[[str], None] = log, sleep: Callable[[float], None] = time.sleep,
        capture_timeout_ms: int = CAPTURE_TIMEOUT_MS, no_audio_timeout_s: float = NO_AUDIO_TIMEOUT_S,
        receive_gap_s: float = RECEIVE_GAP_S, window_s: float = pa.WINDOW_S,
        should_stop: Callable[[], bool] = lambda: False, capture=None,
        wall_offset: Callable[[], int | None] | None = None) -> None:
    """The sampler's consumer loop. `receiver` has capture(timeout_ms) -> AudioBlock|None (raises
    ConnectionError on an NDI error frame) and connections(); `decoder` has
    decode(samples, sample_rate) -> CRC-valid words per channel (program_audio_marker.MarkerDecoder).
    `capture`: the capture side (program_audio_capture.CaptureThread, started by the caller); None =
    a SyncCapture that calls the receiver in this loop (the calibration and the loop tests), reading
    dev1's wall offset through `wall_offset` once per block -- None (the default) reads no clock, so
    a real dantesync step never decides a test or a calibration run; the dev1 service's
    CaptureThread reads the clock itself. Every written payload also goes through `on_write`
    (tests)."""
    latch = {"last_foreign": None}
    bridged_total = {"holes": 0, "ms": 0.0}  # since the start: the JSON's holes_bridged / bridged_ms
    drops_total = {"frames": 0}              # since the start: the JSON's queue_drops
    stalls_total = {"n": 0}                  # since the start: the JSON's sender_stalls
    cap = capture if capture is not None else pac.SyncCapture(
        receiver, mono=mono, wall_offset=wall_offset if wall_offset is not None else (lambda: None),
        sleep=sleep)
    lag = {"ms": None}        # the consumer's lag behind the capture for the window being written
    walls = pa.WallSteps()

    def write(verdict, rms, outside, reason=None, markers=None, chain=None):
        now = datetime.now(timezone.utc)
        if verdict == "FOREIGN":
            latch["last_foreign"] = rsf.format_ts_utc(now)
        payload = pa.build_payload(verdict, rms, outside, now=now, window_s=window_s, source=source,
                                   reason=reason, last_foreign_ts_utc=latch["last_foreign"],
                                   markers_decoded=markers, marker_chain=chain,
                                   holes_bridged=bridged_total["holes"],
                                   bridged_ms=bridged_total["ms"], queue_drops=drops_total["frames"],
                                   lag_ms=lag["ms"], sender_stalls=stalls_total["n"])
        pa.write_payload(serve_dir, payload)
        if on_write is not None:
            on_write(payload)
        return payload

    acc = WindowAccumulator(window_s)
    span = MarkerSpan(window_s, pa.MARKER_SPAN_S)
    write("UNKNOWN", None, None, reason="sampler starting")
    log(f"program-audio sampler: source={source!r} serve_dir={serve_dir} window={window_s}s "
        f"band={pa.BAND_LO_HZ:.0f}-{pa.BAND_HI_HZ:.0f}Hz tone_lines={pa.MEASUREMENT_TONE_LINES_HZ} "
        f"foreign>={pa.FOREIGN_OUTSIDE_BAND_PCT}% silent<{pa.SILENT_RMS_DBFS}dBFS "
        f"marker_chain>={pa.MARKER_CHAIN_MIN} over {pa.MARKER_SPAN_S:g}s continuity=sender timeline "
        f"(frame+{pa.CONTINUITY_SLACK_S * 1e3:g}ms, holes up to {pa.HOLE_BRIDGE_MAX_MS:g}ms bridged, "
        f"date steps matched to dev1's own wall step +-{pa.DATE_STEP_MATCH_MS:g}ms within "
        f"{pa.DATE_STEP_WINDOW_S:g}s, a frame ahead looked ahead over {pa.STALL_LOOKAHEAD_FRAMES} "
        f"frames for a sender stall) receive_gap>{receive_gap_s:g}s without a timestamp "
        f"capture={'thread' if capture is not None else 'in-loop'}")
    last_audio = mono()
    have_audio = False
    last_unknown = mono()
    last_verdict = "UNKNOWN"
    last_summary = mono()
    counts = {v: 0 for v in pa.VERDICTS}
    errors = bad_rate = gaps = breaks = late_bursts = bridged = drops = date_steps = stalls = 0
    bridged_ms = 0.0     # the audio this summary interval bridged with zeros
    max_offset_ms = 0.0  # the largest |offset| of a frame that continued the sender timeline
    max_lag_ms = 0.0     # the oldest captured item the consumer took (its backlog behind the capture)
    max_stall_ms = 0.0   # the largest offset ahead of a frame the look-ahead read as a sender stall
    # (timestamp, samples, sample_rate, channels) of the previous frame taken in; the timestamp is its
    # place on the sender's timeline (a stalled frame's own late stamp never is)
    prev = None
    held = HeldFrames()  # a frame AHEAD of the timeline and the frames after it (the look-ahead)
    now = mono()
    rebase_windows = 0  # windows still to come whose span holds a DATE_STEP (judged as holed)
    in_error = False
    bad_rate_logged = False
    loops = 0

    def restart_span(why: str) -> None:
        nonlocal rebase_windows
        acc.reset()
        span.reset()
        rebase_windows = 0
        if why:
            log(f"program-audio sampler: {why} -- the marker span starts over (no MEASUREMENT for "
                f"{pa.MARKER_SPAN_S:g} s)")

    def consume(samples: np.ndarray, sample_rate: int, real: bool = True) -> None:
        """Push audio (a frame, or with real=False the zeros that bridge a hole before it) into the
        window accumulator; classify, write and log every window it completes."""
        nonlocal last_verdict, rebase_windows
        for win, sr, win_real in acc.push(samples, sample_rate, real):
            verdict, rms, outside, reason, markers, chain = classify_window(win, sr, span, decoder,
                                                                            win_real,
                                                                            rebased=rebase_windows > 0)
            rebase_windows = max(0, rebase_windows - 1)
            write(verdict, rms, outside, reason, markers, chain)
            counts[verdict] += 1
            if verdict != last_verdict:
                log(f"program-audio verdict {last_verdict} -> {verdict} rms={_fmt(rms)} dBFS "
                    f"outside_band={_fmt(outside)}% marker_chain={_fmt_count(chain)} "
                    f"markers={_fmt_count(markers)} sr={sr} channels={win.shape[1]}"
                    + (f": {reason}" if reason else ""))
                last_verdict = verdict

    def place(item, block, stamp: int, missing: int = 0) -> None:
        """Take a decided frame in: `missing` zeros before it (a bridged hole, counted), then its
        samples. The next frame is judged against `stamp`, its place on the sender's timeline."""
        nonlocal have_audio, last_audio, prev, bridged, bridged_ms
        if missing:
            hole_ms = missing * 1e3 / block.sample_rate
            bridged += 1
            bridged_ms += hole_ms
            bridged_total["holes"] += 1
            bridged_total["ms"] += hole_ms
        have_audio = True
        last_audio = item.arrival_s
        lag["ms"] = (now - item.arrival_s) * 1e3
        prev = (stamp, block.samples.shape[0], block.sample_rate, block.samples.shape[1])
        if missing:
            # The hole's samples, as silence, at their place on the sender's timeline: every
            # later sample keeps its timeline position, so the marker chain stays on its line.
            consume(np.zeros((missing, block.samples.shape[1]), dtype=block.samples.dtype),
                    block.sample_rate, real=False)
        consume(block.samples, block.sample_rate)

    def admit(item, block) -> None:
        """Judge a frame against the previous one and take it in -- or, AHEAD of the timeline, hold
        it for the sender-stall look-ahead."""
        nonlocal breaks, gaps, late_bursts, date_steps, rebase_windows, max_offset_ms
        missing = 0
        if have_audio:
            j = judge_continuity(prev, block, item.arrival_s - last_audio, receive_gap_s,
                                 dropped_frames=item.dropped_frames, dropped_100ns=item.dropped_100ns,
                                 wall_steps=walls.recent(item.arrival_s))
            if j.offset_ms is not None and j.kind in ("continue", "late_burst"):
                max_offset_ms = max(max_offset_ms, abs(j.offset_ms))
            if j.kind == "ahead":
                held.hold(item, block)
                return
            if j.kind == "timeline_break":
                breaks += 1
                restart_span(j.detail)
            elif j.kind == "receive_gap":
                gaps += 1
                restart_span(j.detail)
            elif j.kind == "late_burst":
                late_bursts += 1
                log(f"program-audio sampler: {j.detail}")
            elif j.kind == "date_step":
                date_steps += 1
                walls.consume(j.wall_step_100ns)
                # The window that holds the step and the next one (a span is span.windows
                # windows): their short chain is never FOREIGN on its own (rule A), since the
                # +-20 ms match can absorb a small real loss.
                rebase_windows = span.windows
                missing = j.missing_samples
                log(f"program-audio sampler: {j.detail}")
            elif j.kind == "bridge":
                missing = j.missing_samples
                log(f"program-audio sampler: {j.detail}")
        place(item, block, block.timestamp, missing)

    def decide(res, frames) -> None:
        """Act on the look-ahead's answer for the held `frames` (pa.Lookahead): file the first
        len(res.stamps) at their timeline place, then judge every later one normally."""
        nonlocal stalls, max_stall_ms, breaks
        item0, _block0 = frames[0]
        detail = lookahead_detail(res, len(frames) - 1, prev[1], prev[2], item0.dropped_frames,
                                  item0.dropped_100ns)
        if res.kind == pa.SENDER_STALL:
            stalls += 1
            stalls_total["n"] += 1
            max_stall_ms = max(max_stall_ms, res.first_offset_100ns * 1e3 / pa.NDI_TIME_UNITS_PER_S)
            if detail:
                log(f"program-audio sampler: {detail}")
        elif res.kind == pa.BRIDGE:
            log(f"program-audio sampler: {detail}")
        else:
            breaks += 1
            restart_span(detail)
        for k, stamp in enumerate(res.stamps):
            place(*frames[k], stamp, res.missing_samples if k == 0 else 0)
        for it, blk in frames[len(res.stamps):]:
            feed(it, blk)

    def settle(complete: bool) -> None:
        res = held.resolve(prev, complete)
        if res is not None:
            decide(res, held.take())

    def feed(item, block) -> None:
        """The entry for every audio frame: it joins the held look-ahead when one is open (a frame
        that cannot join first closes it), else it is judged."""
        while held and not held.can_join(item, block):
            settle(complete=True)
        if held:
            held.hold(item, block)
            settle(complete=False)
        else:
            admit(item, block)

    def flush() -> None:
        """Decide every held frame with what arrived (the capture went quiet, an error, the stop)."""
        while held:
            settle(complete=True)

    while not should_stop() and (max_loops is None or loops < max_loops):
        loops += 1
        item = cap.get(capture_timeout_ms)
        now = mono()
        block = None
        if item is not None and item.dropped_frames:
            # The sampler's own capture queue was full: those frames are a hole of known length
            # (judged below with the next block), counted, never silent.
            drops += item.dropped_frames
            drops_total["frames"] += item.dropped_frames
        if item is not None and item.error is not None:
            flush()  # the frames held before the error are real audio of the old span
            errors += 1
            if not in_error:
                log(f"program-audio sampler: {item.error} -- the SDK reconnects by itself")
                in_error = True
            if have_audio:
                restart_span("")  # audio after an error frame never continues the span
            have_audio = False
        elif item is not None:
            block = item.block
            walls.observe(item.arrival_s, item.wall_offset_ns)
        if block is not None and block.sample_rate <= 0:
            bad_rate += 1
            if not bad_rate_logged:
                log(f"program-audio sampler: dropping an NDI audio frame with sample_rate={block.sample_rate}")
                bad_rate_logged = True
            block = None
        if block is not None and block.samples.shape[0] > 0:
            in_error = False
            max_lag_ms = max(max_lag_ms, (now - item.arrival_s) * 1e3)
            feed(item, block)
        else:
            if item is None:
                flush()  # the capture went quiet: no more frame can join the look-ahead
            if now - last_audio >= no_audio_timeout_s and now - last_unknown >= window_s:
                reason = (f"no audio from {source!r} for {now - last_audio:.1f} s "
                          f"(connections={receiver.connections()})")
                lag["ms"] = None  # no window judged: no lag to report
                write("UNKNOWN", None, None, reason=reason)
                counts["UNKNOWN"] += 1
                last_unknown = now
                if last_verdict != "UNKNOWN":
                    log(f"program-audio verdict {last_verdict} -> UNKNOWN: {reason}")
                    last_verdict = "UNKNOWN"
        if now - last_summary >= LOG_SUMMARY_S:
            log("program-audio summary (last %.0f s): %s error_frames=%d bad_rate_frames=%d "
                "queue_drops=%d max_lag_ms=%.0f date_steps=%d sender_stalls=%d max_stall_ms=%.1f "
                "timeline_breaks=%d late_bursts=%d receive_gaps=%d max_offset_ms=%.1f "
                "holes_bridged=%d bridged_ms=%.1f"
                % (now - last_summary, " ".join(f"{k}={v}" for k, v in counts.items()),
                   errors, bad_rate, drops, max_lag_ms, date_steps, stalls, max_stall_ms, breaks,
                   late_bursts, gaps, max_offset_ms, bridged, bridged_ms))
            counts = {v: 0 for v in pa.VERDICTS}
            errors = bad_rate = gaps = breaks = late_bursts = bridged = drops = date_steps = stalls = 0
            bridged_ms = max_offset_ms = max_lag_ms = max_stall_ms = 0.0
            last_summary = now
    flush()  # the stop: decide the frames still held, so no taken-in audio is left behind


class Judgement(NamedTuple):
    """judge_continuity's answer: `kind` (below), the log `detail`, the frame's offset from the
    sender timeline in ms (None without timestamps), for "bridge" / "date_step" the missing samples
    (the zeros to insert), and for "date_step" the dev1 wall step (100 ns) it used."""
    kind: str
    detail: str
    offset_ms: float | None
    missing_samples: int = 0
    wall_step_100ns: float | None = None


def judge_continuity(prev, block, arrival_gap_s: float, receive_gap_s: float, *, dropped_frames: int = 0,
                     dropped_100ns: float = 0.0, wall_steps=()) -> Judgement:
    """How `block` follows the previous audio block `prev` = (timestamp, samples, sample_rate,
    channels); `dropped_frames` / `dropped_100ns`: frames the sampler's own capture queue dropped
    between the two (a hole of known length); `wall_steps`: dev1's own recent wall-clock steps
    (pa.WallSteps.recent). The kind is one of
      "continue"        on the sender timeline, arrival within receive_gap_s: nothing to say
      "late_burst"      on the sender timeline after an arrival gap over receive_gap_s: the span is
                        kept (the SDK queued the audio while dev1 starved the sampler)
      "date_step"       a forward jump that matches dev1's own wall step (pa.DATE_STEP): the fleet
                        date step, nothing lost; no zeros (only a known queue drop's), the span kept
      "ahead"           more than the tolerance AHEAD of the timeline beyond any known drop, with no
                        matching wall step and no format change: the caller holds it for the
                        sender-stall look-ahead (HeldFrames, pa.resolve_ahead), which decides it
      "bridge"          a hole AHEAD of the timeline up to pa.HOLE_BRIDGE_MAX_MS (pa.BRIDGE), or a
                        queue drop: the caller inserts `missing_samples` zeros before the frame and
                        keeps the span, whatever the arrival gap (design issue 1404 comment 6036098516)
      "timeline_break"  off the sender timeline (pa.DISCONTINUITY, a queue drop over the bridge
                        limit, or a hole whose frame changed the sample rate or the channel count):
                        the span restarts
      "receive_gap"     a timestamp is undefined (pa.UNKNOWN_TS) and the arrival gap is over
                        receive_gap_s: the fallback restarts the span"""
    p_ts, p_n, p_sr, p_ch = prev
    tol = pa.continuity_tolerance_100ns(p_n, p_sr)
    tol_ms = tol * 1e3 / pa.NDI_TIME_UNITS_PER_S
    decision = pa.frame_continues(p_ts, p_n, p_sr, block.timestamp, tol, dropped_100ns=dropped_100ns,
                                  wall_steps=wall_steps)
    fmt_changed = (block.sample_rate, block.samples.shape[1]) != (p_sr, p_ch)
    drop_ms = dropped_100ns * 1e3 / pa.NDI_TIME_UNITS_PER_S
    queue = (f"queue overflow: {dropped_frames} frames ({drop_ms:.1f} ms) dropped by the sampler's own "
             "capture queue -- " if dropped_frames else "")
    defined = pa.timestamp_defined(p_ts) and pa.timestamp_defined(block.timestamp)
    if not defined and arrival_gap_s > receive_gap_s:
        # The arrival fallback, a known queue drop or not: the drop cannot vouch for what else the
        # arrival gap lost when no timestamp tells.
        return Judgement("receive_gap", (f"{queue}receive gap of {arrival_gap_s:.1f} s (no NDI sender "
                                         "timestamp, the arrival-time fallback)"), None)
    if decision.kind == pa.UNKNOWN_TS:
        return Judgement("continue", "", None)
    if defined:
        off_ms = pa.timeline_offset_100ns(p_ts, p_n, p_sr, block.timestamp) * 1e3 / pa.NDI_TIME_UNITS_PER_S
    else:
        off_ms = None  # only with a queue drop: frame_continues judged the known drop alone
    if (defined and not fmt_changed and decision.kind in (pa.BRIDGE, pa.DISCONTINUITY)
            and pa.ahead_of_timeline(p_ts, p_n, p_sr, block.timestamp, tol, dropped_100ns=dropped_100ns)):
        return Judgement("ahead", "", off_ms)
    if decision.kind in (pa.BRIDGE, pa.DATE_STEP) and decision.missing_samples and fmt_changed:
        return Judgement("timeline_break", (f"{queue}audio timeline hole of {_fmt_ms(off_ms, drop_ms)} at a "
                                            f"format change ({p_sr} Hz x {p_ch} -> {block.sample_rate} Hz x "
                                            f"{block.samples.shape[1]} channels, arrival gap "
                                            f"{arrival_gap_s:.1f} s)"), off_ms)
    if decision.kind == pa.DATE_STEP:
        rest = pa.timeline_offset_100ns(p_ts, p_n, p_sr, block.timestamp) - dropped_100ns
        step = pa.matching_wall_step(rest, wall_steps)  # the same call frame_continues matched with
        n = decision.missing_samples
        return Judgement("date_step", (f"{queue}audio timeline date step: the frame sits {off_ms:+.1f} ms ahead "
                                       f"of the sender's timeline and dev1's own wall clock stepped "
                                       f"{step / 1e4:+.1f} ms within {pa.DATE_STEP_WINDOW_S:g} s -- the "
                                       "fleet date step, nothing lost: the timeline is re-based, no zeros"
                                       + (f" (only the {n} dropped samples)" if n else "")
                                       + ", the marker span is kept"), off_ms, n, step)
    if decision.kind == pa.BRIDGE and dropped_frames:
        n = decision.missing_samples
        return Judgement("bridge", (f"{queue}bridged with {n} zero samples ({n * 1e3 / p_sr:.1f} ms; the "
                                    f"frame sits {_fmt_ms(off_ms, drop_ms)} off the sender's timeline, "
                                    f"arrival gap {arrival_gap_s:.1f} s), the marker span is kept"), off_ms, n)
    if decision.kind == pa.DISCONTINUITY and dropped_frames:
        return Judgement("timeline_break", (f"{queue}a hole of {_fmt_ms(off_ms, drop_ms)} (over the "
                                            f"{pa.HOLE_BRIDGE_MAX_MS:g} ms bridge, or off the sender's "
                                            f"timeline beyond +-{tol_ms:.1f} ms)"), off_ms)
    if decision.kind == pa.BRIDGE:
        n = decision.missing_samples
        return Judgement("bridge", (f"audio timeline hole: the frame sits {off_ms:+.1f} ms ahead of the "
                                    f"sender's timeline (tolerance +-{tol_ms:.1f} ms, bridge up to "
                                    f"{pa.HOLE_BRIDGE_MAX_MS:g} ms, arrival gap {arrival_gap_s:.1f} s) -- "
                                    f"bridged with {n} zero samples ({n * 1e3 / p_sr:.1f} ms), the marker "
                                    "span is kept"), off_ms, n)
    if decision.kind == pa.DISCONTINUITY:
        return Judgement("timeline_break", (f"audio timeline discontinuity: the frame sits {off_ms:+.1f} ms "
                                            f"off the sender's timeline (tolerance +-{tol_ms:.1f} ms, "
                                            f"arrival gap {arrival_gap_s:.1f} s)"), off_ms)
    if arrival_gap_s > receive_gap_s:
        return Judgement("late_burst", (f"late burst after {arrival_gap_s:.1f} s without audio: the sender "
                                        f"timeline continues ({off_ms:+.1f} ms), the marker span is kept"),
                         off_ms)
    return Judgement("continue", "", off_ms)


def lookahead_detail(res, held_n: int, prev_samples: int, sample_rate: int, dropped_frames: int = 0,
                     dropped_100ns: float = 0.0) -> str:
    """The log text for the look-ahead's answer `res` (pa.Lookahead) over the first held frame and
    `held_n` frames after it; the previous frame was `prev_samples` long at `sample_rate`, and the
    first held frame carried a known queue drop of `dropped_frames` / `dropped_100ns`. Empty for a
    plain sender stall: those are counted (summary + JSON), never logged one by one (~2 a minute live)."""
    units = pa.NDI_TIME_UNITS_PER_S
    tol_ms = pa.continuity_tolerance_100ns(prev_samples, sample_rate) * 1e3 / units
    off_ms = res.first_offset_100ns * 1e3 / units
    hole_ms = res.hole_100ns * 1e3 / units
    drop_ms = dropped_100ns * 1e3 / units
    n = res.missing_samples
    queue = (f"queue overflow: {dropped_frames} frames ({drop_ms:.1f} ms) dropped by the sampler's own "
             "capture queue -- " if dropped_frames else "")
    if res.kind == pa.SENDER_STALL:
        if not n:
            return ""
        return (f"{queue}bridged with {n} zero samples ({n * 1e3 / sample_rate:.1f} ms; the frame sat "
                f"{off_ms:+.1f} ms beyond the dropped audio and the next {held_n} frame(s) came back "
                f"within +-{tol_ms:.1f} ms: a sender stall), the marker span is kept")
    if res.kind == pa.BRIDGE:
        return (f"{queue}audio timeline hole: the frame sits {off_ms:+.1f} ms ahead of the sender's "
                f"timeline and the next {held_n} frame(s) never came back within +-{tol_ms:.1f} ms (the "
                f"smallest offset {hole_ms:+.1f} ms is the hole, bridge up to {pa.HOLE_BRIDGE_MAX_MS:g} ms) "
                f"-- bridged with {n} zero samples ({n * 1e3 / sample_rate:.1f} ms), the marker span is kept")
    return (f"{queue}audio timeline discontinuity: the frame sits {off_ms:+.1f} ms off the sender's "
            f"timeline (the smallest offset over the next {held_n} frame(s) {hole_ms:+.1f} ms, with the "
            f"dropped audio over the {pa.HOLE_BRIDGE_MAX_MS:g} ms bridge; tolerance +-{tol_ms:.1f} ms)")


def _fmt_ms(off_ms: float | None, drop_ms: float) -> str:
    """A hole for a log line: the frame's sender-timeline offset, or the dropped audio when the
    frames carry no timestamp."""
    return f"{off_ms:+.1f} ms" if off_ms is not None else f"{drop_ms:.1f} ms (no sender timestamp)"


def _fmt(v) -> str:
    return "-" if not pa.is_number(v) else f"{v:.1f}"


def _fmt_count(v) -> str:
    return "-" if v is None else str(v)


def decode_real_samples(decoder, samples: np.ndarray, sr: int, real: np.ndarray | None):
    """The decoder's CRC-valid words per channel over the REAL samples of `samples` only: with no
    mask the whole buffer in one call (what every span without a bridged hole gets, unchanged);
    with one, each run of real samples on its own, its word times moved to the run's position.
    The zeros that bridge a hole are never handed to the decoder, so no word, and no edge between
    audio and inserted silence, can come from them (issue 1404 review of design 6036098516)."""
    if real is None:
        return decoder.decode(samples, sr)
    words = [[] for _ in range(samples.shape[1])]
    for start, stop in pa.real_runs(real):
        for c, ch in enumerate(decoder.decode(samples[start:stop], sr)):
            words[c].extend((t + start / sr, index) for t, index in ch)
    return words


def classify_window(win: np.ndarray, sr: int, span: MarkerSpan, decoder, real: np.ndarray | None = None,
                    rebased: bool = False):
    """One 2 s window -> (verdict, rms, outside, reason, markers_decoded, marker_chain): the spectral
    measurement of this window and the marker chain over the trailing span (module doc of
    program_audio.py). A decode failure leaves the chain unknown, which never reads MEASUREMENT.
    `real` is the window's real-sample mask (None = no bridged zeros in it); `rebased`: the span holds
    a DATE_STEP, which counts as holed for the short-chain rule (ROZHODNUTÉ 6037765523)."""
    rms, outside = pa.analyse(win, sr, real)
    silent = pa.is_number(rms) and rms < pa.SILENT_RMS_DBFS
    full = span.push(win, sr, silent, real)
    if not span.warm:
        if pa.spectral_foreign(rms, outside):
            # ROZHODNUTÉ issue 1404 comment 6027706292 item 1: a spectral FOREIGN needs no marker
            # span, so it is reported (and latched) in the warm-up too; only MEASUREMENT waits.
            return "FOREIGN", rms, outside, None, None, None
        return ("UNKNOWN", rms, outside,
                f"warming up: {span.audio_s:g} of {pa.MARKER_SPAN_S:g} s of audio since the start or a "
                "span restart", None, None)
    markers = chain = None
    reason = None
    if full is not None:
        try:
            markers, chain = pa.span_markers(decode_real_samples(decoder, full, sr, span.real))
        except (pam.DecodeError, ValueError) as exc:
            reason = f"marker decode failed: {exc}"
    elif not silent:
        reason = (f"marker span: {span.non_silent_s:g} of {pa.MARKER_SPAN_S:g} s of non-silent audio "
                  "since the last silent window")
    holed = full is not None and (span.real is not None or rebased)
    verdict = pa.classify(rms, outside, chain, holed=holed)
    if verdict != "UNKNOWN":
        reason = None
    elif reason is None and holed and isinstance(chain, int) and chain < pa.MARKER_CHAIN_MIN:
        held = []
        if span.real is not None:
            held.append(f"{int(np.count_nonzero(~span.real)) * 1e3 / sr:.1f} ms of bridged audio")
        if rebased:
            held.append("a date step")
        reason = (f"marker chain {chain} < {pa.MARKER_CHAIN_MIN} over a span holding {' and '.join(held)} -- "
                  "a chain cut short by a hole is never FOREIGN on its own "
                  "(ROZHODNUTÉ issue 1404 comment 6037765523)")
    elif reason is None:
        reason = "the window's level or spectrum is not a number"
    return verdict, rms, outside, reason, markers, chain


def cpu_list(cpus) -> str:
    """A cpu set as a compact list: {12, 13, 14, 15} -> "12-15", {0, 2, 3, 5} -> "0,2-3,5"."""
    out, run = [], []
    for c in sorted(cpus):
        if run and c == run[-1] + 1:
            run.append(c)
            continue
        if run:
            out.append(str(run[0]) if len(run) == 1 else f"{run[0]}-{run[-1]}")
        run = [c]
    if run:
        out.append(str(run[0]) if len(run) == 1 else f"{run[0]}-{run[-1]}")
    return ",".join(out)


def scheduling_state(cgroup_file: str = "/proc/self/cgroup",
                     cgroup_root: str = "/sys/fs/cgroup") -> tuple[int, str, int | None]:
    """(nice, the cpus this process may run on, its cgroup's cpu.weight or None when unreadable),
    for the one start line (scheduling_line)."""
    nice = os.getpriority(os.PRIO_PROCESS, 0)
    weight = None
    try:
        with open(cgroup_file, encoding="utf-8") as fh:
            path = next((line.split("::", 1)[1].strip() for line in fh if line.startswith("0::")), None)
        if path is not None:
            with open(os.path.join(cgroup_root, path.lstrip("/"), "cpu.weight"), encoding="utf-8") as fh:
                weight = int(fh.read().strip())
    except (OSError, ValueError):
        weight = None  # reported as "unreadable" in the start line, never guessed
    return nice, cpu_list(os.sched_getaffinity(0)), weight


def scheduling_line(nice: int, cpus: str, weight: int | None) -> str:
    """The one start line about where and how the sampler runs: its nice, its cpus (strih-lx pins the
    E-cores through the unit's CPUAffinity=) and its cgroup's cpu.weight. Both units run it at normal
    priority (no Nice=, no CPUWeight=); a positive nice is a WARNING: a deprioritised sampler stops
    calling the NDI capture under load, and the SDK drops audio after ~1.3 s (issue 1404 STEP 0)."""
    w = "unreadable" if weight is None else str(weight)
    line = f"scheduling nice={nice} cpus={cpus or '?'} cpu.weight={w}"
    if nice > 0:
        return f"program-audio sampler: WARNING {line} -- deprioritised, it can starve under load"
    return f"program-audio sampler: {line}"


def default_serve_dir() -> str:
    """$PROGRAM_AUDIO_SERVE_DIR, else $XDG_RUNTIME_DIR/program-audio-sampler (the user's runtime
    tmpfs: the file is rewritten every 2 s). The dev1 lease server's $RIG_LEASE_SERVE_DIR never moves
    it (issue 1404: the sampler has no dev1 shape left)."""
    return rsf.runtime_serve_dir(SERVE_DIR_ENV, SERVE_DIR_NAME)


def private_ndi_config_dir() -> str:
    """A fresh, empty NDI config dir: libndi then uses its defaults = mDNS discovery only."""
    return tempfile.mkdtemp(prefix="program-audio-sampler-ndi-")


def build_parser() -> argparse.ArgumentParser:
    """The sampler's CLI; every default can come from its environment variable (the unit's private
    env file), so the same unit runs on any Linux node."""
    ap = argparse.ArgumentParser(description="issue 1404 -- the stream program-audio sampler")
    ap.add_argument("--source", default=os.environ.get(SOURCE_ENV) or DEFAULT_SOURCE,
                    help=f"NDI source name (default ${SOURCE_ENV} or {DEFAULT_SOURCE!r})")
    ap.add_argument("--serve-dir", default=default_serve_dir(),
                    help=f"where program-audio.json goes and is served from (default ${SERVE_DIR_ENV}, "
                         f"else $XDG_RUNTIME_DIR/{SERVE_DIR_NAME}; never the lease dir)")
    ap.add_argument("--lib", default=None, help="libndi path (default $NDI_LIB_PATH or /usr/lib/ndi/libndi.so.6)")
    ap.add_argument("--marker-shim", default=None,
                    help=f"the QPSK marker decoder shim (default ${pam.SHIM_ENV} or {pam.DEFAULT_SHIM_PATH}; "
                         "built by scripts/build-qpsk-guard-shim.sh)")
    ap.add_argument("--http-port", type=int, default=os.environ.get(HTTP_PORT_ENV) or DEFAULT_HTTP_PORT,
                    help=f"the sampler's own read-only /program-audio.json endpoint (default ${HTTP_PORT_ENV} "
                         f"or {DEFAULT_HTTP_PORT}; 0 = no endpoint)")
    ap.add_argument("--http-bind", default=os.environ.get(HTTP_BIND_ENV) or DEFAULT_HTTP_BIND,
                    help=f"its bind address (default ${HTTP_BIND_ENV} or {DEFAULT_HTTP_BIND})")
    return ap


def main(argv=None) -> int:
    ap = build_parser()
    args = ap.parse_args(argv)
    if not 0 <= args.http_port <= 65535:
        ap.error(f"--http-port {args.http_port} is not a port (0 = no endpoint)")

    import program_audio_ndi as pan  # libndi only here, so the pure parts import without it

    try:
        rsf.ensure_serve_dir(args.serve_dir, rsf.default_lease_dir())
    except (ValueError, OSError) as exc:
        log(f"program-audio sampler: FATAL serve dir: {exc}")
        return 2

    def set_unknown(reason: str) -> None:
        # Never leave a fresh-looking verdict behind a sampler that is not sampling.
        pa.write_payload(args.serve_dir, pa.build_payload(
            "UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=pa.WINDOW_S,
            source=args.source, reason=reason))

    # The marker decoder BEFORE the receiver: without it no window can be MEASUREMENT, so the sampler
    # must not run at all (fail closed, loud), never a spectral-only MEASUREMENT.
    try:
        decoder = pam.MarkerDecoder(args.marker_shim)
    except pam.DecoderUnavailable as exc:
        set_unknown(f"sampler cannot classify: {exc}")
        log(f"program-audio sampler: FATAL {exc}")
        return 1
    log(f"program-audio sampler: marker decoder {decoder.path} params={decoder.params} "
        f"sources_sha256={decoder.built_sha256[:12]}")
    if decoder.sources_stale():
        log(f"program-audio sampler: WARNING the marker decoder {decoder.path} was built from other "
            f"sources than this checkout ({decoder.built_sha256[:12]} != {pam.sources_sha256()[:12]}) -- "
            "rebuild it with scripts/build-qpsk-guard-shim.sh and restart")

    ndi_config_dir = private_ndi_config_dir()
    prev_ndi_config_dir = os.environ.get("NDI_CONFIG_DIR")
    os.environ["NDI_CONFIG_DIR"] = ndi_config_dir
    try:
        try:
            receiver = pan.NdiAudioReceiver(args.source, lib_path=args.lib)
        except (OSError, RuntimeError) as exc:
            set_unknown(f"sampler cannot receive: {exc}")
            log(f"program-audio sampler: FATAL cannot create the NDI receiver: {exc}")
            return 1
        http = None
        if args.http_port:
            try:
                server = pah.make_server(args.http_bind, args.http_port, args.serve_dir)
            except OSError as exc:
                receiver.close()
                set_unknown(f"sampler cannot serve http on {args.http_bind}:{args.http_port}: {exc}")
                log(f"program-audio sampler: FATAL cannot serve http on {args.http_bind}:{args.http_port}: {exc}")
                return 1
            http = (server, pah.serve_in_thread(server))
            log(f"program-audio sampler: serving http://{args.http_bind}:{args.http_port}/program-audio.json "
                f"from {args.serve_dir}")
        else:
            log("program-audio sampler: no http endpoint (--http-port 0)")
        stop = {"signal": None}

        def _stop(signum, _frame):
            stop["signal"] = signum  # only a flag: no I/O inside a signal handler

        signal.signal(signal.SIGTERM, _stop)
        signal.signal(signal.SIGINT, _stop)
        log(scheduling_line(*scheduling_state()))
        # The capture thread only calls the SDK and queues; the window work below never delays the
        # next capture call (design issue 1404 comment 6037613222).
        cap = pac.CaptureThread(receiver, timeout_ms=CAPTURE_TIMEOUT_MS)
        cap.start()
        try:
            run(receiver, args.serve_dir, source=args.source, decoder=decoder, capture=cap,
                should_stop=lambda: stop["signal"] is not None)
        finally:
            if cap.stop():
                receiver.close()
            else:
                log("program-audio sampler: ERROR the capture thread is still inside the NDI capture call -- "
                    "the receiver is left to the process exit (destroying it under a running capture "
                    "would crash the SDK)")
            set_unknown("sampler stopped")
            if http is not None:
                pah.stop(*http)
            log(f"program-audio sampler: stopped (signal {stop['signal']}), program-audio.json set to UNKNOWN")
    finally:
        shutil.rmtree(ndi_config_dir, ignore_errors=True)
        if prev_ndi_config_dir is None:
            os.environ.pop("NDI_CONFIG_DIR", None)
        else:
            os.environ["NDI_CONFIG_DIR"] = prev_ndi_config_dir
    return 0


if __name__ == "__main__":
    sys.exit(main())
