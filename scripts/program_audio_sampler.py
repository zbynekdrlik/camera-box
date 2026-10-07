#!/usr/bin/env python3
"""issue 1404 -- the dev1 stream program-audio sampler (a long-running --user service).

Receives the stream program's audio from stream OBS's own NDI program output (the
`genlock-ndi-output 'stream'` sender, NDI name `STREAM-SNV (stream)` at 10.77.9.204 -- read live
with avahi on 6.10.2026), audio-only, read-only (a receiver like any NDI monitor; nothing on the
rig changes). Every WINDOW_S (2 s) of audio it computes `rms_dbfs` + `outside_band_pct`, classifies
MEASUREMENT | FOREIGN | SILENT | UNKNOWN (scripts/program_audio.py) and atomically rewrites
`<serve dir>/program-audio.json`, which the rig-lease server serves at
`http://dev1:8890/program-audio.json` (ages recomputed per request). Consumers call
`scripts/program_audio_guard.py` and stop the YouTube broadcast on anything but MEASUREMENT/SILENT.

* FOREIGN LATCH: every payload carries `last_foreign_ts_utc`, the newest FOREIGN window, so a gate
  that polls every ~10 s still sees a FOREIGN window that fell between two of its polls.
* UNKNOWN whenever it is not sampling: at start, after NO_AUDIO_TIMEOUT_S without audio (sender
  down, the rig away at an event, mDNS not seeing it; rewritten every window so the file never
  reads fresh while it holds an old verdict), when libndi cannot be loaded, and on stop.
* mDNS ONLY, enforced: the receiver runs with a private, empty NDI_CONFIG_DIR, so no NDI extra-IP
  list can make it open a TCP discovery connection into a sender (.claude/rules/ndi-discovery.md).
* Logging: a line per verdict CHANGE, the first NDI error frame / bad sample rate of a run, a line
  per span restart (`audio timeline discontinuity`, the no-timestamp `receive gap`) and per late
  burst the timeline proved continuous, and a summary every LOG_SUMMARY_S (timeline_breaks,
  late_bursts, receive_gaps; a restart after an NDI error frame shows as error_frames) -- never a
  line per 2 s window (~43 000 a day).

* MARKER REQUIREMENT (ROZHODNUTÉ issue 1404 comments 6026577906 + 6026826572): MEASUREMENT also
  needs the cam2 QPSK marker itself -- a timecode chain of >= pa.MARKER_CHAIN_MIN markers over the
  trailing 4 s of contiguous non-silent audio, decoded by the dock's own decoder through the
  `scripts/qpsk_guard_shim.cpp` library (scripts/program_audio_marker.py). Until 4 s of audio
  arrived since the start or a span restart nothing reads MEASUREMENT: a window whose spectrum alone
  says FOREIGN reads FOREIGN and starts the latch (ROZHODNUTÉ 6027706292 item 1), every other one
  reads UNKNOWN. A missing / unloadable library = UNKNOWN and exit 1, like a missing libndi.
* SPAN RESTARTS (design issue 1404 comment 6030385284): continuity is judged on the SENDER's NDI
  audio timeline (pa.frame_continues over each frame's SDK timestamp), never on dev1's arrival
  time: a late burst after dev1 starved the sampler keeps the span; a timeline hole (lost audio, a
  sender restart, a dantesync date step: one warm-up) restarts it whatever the arrival time. Only a
  frame with no timestamp falls back to the arrival gap (no audio block for over RECEIVE_GAP_S),
  and an NDI error frame always restarts the span.

Usage (systemd/program-audio-sampler.service):
  program_audio_sampler.py [--source "STREAM-SNV (stream)"] [--serve-dir DIR] [--lib PATH]
                           [--marker-shim PATH]
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
from typing import Callable

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

DEFAULT_SOURCE = "STREAM-SNV (stream)"
SOURCE_ENV = "PROGRAM_AUDIO_SOURCE"
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
    change drops the partial window, so no window ever mixes two formats."""

    def __init__(self, window_s: float):
        self.window_s = window_s
        self.reset()

    def reset(self) -> None:
        """Drop the partial window (a span restart: audio before and after it is never mixed)."""
        self._parts: list[np.ndarray] = []
        self._have = 0
        self._fmt: tuple[int, int] | None = None

    def push(self, samples: np.ndarray, sample_rate: int) -> list[tuple[np.ndarray, int]]:
        if samples.ndim != 2:
            raise ValueError(f"WindowAccumulator: samples must be (n, channels), got {samples.shape}")
        if sample_rate <= 0:
            raise ValueError(f"WindowAccumulator: sample_rate={sample_rate} is not a rate")
        fmt = (int(sample_rate), int(samples.shape[1]))
        if fmt != self._fmt:
            self._parts, self._have, self._fmt = [], 0, fmt
        self._parts.append(samples)
        self._have += samples.shape[0]
        need = int(round(self.window_s * fmt[0]))
        out = []
        while self._have >= need:
            buf = np.concatenate(self._parts, axis=0)
            out.append((buf[:need], fmt[0]))
            rest = buf[need:]
            self._parts = [rest] if rest.shape[0] else []
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
        self._fmt: tuple[int, int] | None = None
        self.audio_s = 0.0

    @property
    def warm(self) -> bool:
        return self.audio_s >= self.span_s - 1e-9

    @property
    def non_silent_s(self) -> float:
        return len(self._wins) * self.window_s

    def push(self, win: np.ndarray, sample_rate: int, silent: bool) -> np.ndarray | None:
        """Add one window; the full span (oldest first) once it holds `span_s` of non-silent audio."""
        fmt = (int(sample_rate), int(win.shape[1]))
        if fmt != self._fmt:
            self.reset()
            self._fmt = fmt
        self.audio_s += self.window_s
        if silent:
            self._wins.clear()
            return None
        self._wins.append(win)
        if len(self._wins) < self.windows:
            return None
        return np.concatenate(list(self._wins), axis=0)


def run(receiver, serve_dir: str, *, source: str, decoder, mono: Callable[[], float] = time.monotonic,
        max_loops: int | None = None, on_write: Callable[[dict], None] | None = None,
        log: Callable[[str], None] = log, sleep: Callable[[float], None] = time.sleep,
        capture_timeout_ms: int = CAPTURE_TIMEOUT_MS, no_audio_timeout_s: float = NO_AUDIO_TIMEOUT_S,
        receive_gap_s: float = RECEIVE_GAP_S, window_s: float = pa.WINDOW_S,
        should_stop: Callable[[], bool] = lambda: False) -> None:
    """The sampler loop. `receiver` has capture(timeout_ms) -> AudioBlock|None (raises
    ConnectionError on an NDI error frame) and connections(); `decoder` has
    decode(samples, sample_rate) -> CRC-valid words per channel (program_audio_marker.MarkerDecoder).
    Every written payload also goes through `on_write` (tests)."""
    latch = {"last_foreign": None}

    def write(verdict, rms, outside, reason=None, markers=None, chain=None):
        now = datetime.now(timezone.utc)
        if verdict == "FOREIGN":
            latch["last_foreign"] = rsf.format_ts_utc(now)
        payload = pa.build_payload(verdict, rms, outside, now=now, window_s=window_s, source=source,
                                   reason=reason, last_foreign_ts_utc=latch["last_foreign"],
                                   markers_decoded=markers, marker_chain=chain)
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
        f"(frame+{pa.CONTINUITY_SLACK_S * 1e3:g}ms) receive_gap>{receive_gap_s:g}s without a timestamp")
    last_audio = mono()
    have_audio = False
    last_unknown = mono()
    last_verdict = "UNKNOWN"
    last_summary = mono()
    counts = {v: 0 for v in pa.VERDICTS}
    errors = bad_rate = gaps = breaks = late_bursts = 0
    prev = None  # (timestamp, samples, sample_rate) of the previous audio block
    in_error = False
    bad_rate_logged = False
    loops = 0

    def restart_span(why: str) -> None:
        acc.reset()
        span.reset()
        if why:
            log(f"program-audio sampler: {why} -- the marker span starts over (no MEASUREMENT for "
                f"{pa.MARKER_SPAN_S:g} s)")

    while not should_stop() and (max_loops is None or loops < max_loops):
        loops += 1
        try:
            block = receiver.capture(capture_timeout_ms)
        except ConnectionError as exc:
            errors += 1
            if not in_error:
                log(f"program-audio sampler: {exc} -- the SDK reconnects by itself")
                in_error = True
            if have_audio:
                restart_span("")  # audio after an error frame never continues the span
            have_audio = False
            sleep(capture_timeout_ms / 1000.0)  # an error frame can come back at once: never spin
            block = None
        now = mono()
        if block is not None and block.sample_rate <= 0:
            bad_rate += 1
            if not bad_rate_logged:
                log(f"program-audio sampler: dropping an NDI audio frame with sample_rate={block.sample_rate}")
                bad_rate_logged = True
            block = None
        if block is not None and block.samples.shape[0] > 0:
            in_error = False
            if have_audio:
                kind, detail = judge_continuity(prev, block, now - last_audio, receive_gap_s)
                if kind == "timeline_break":
                    breaks += 1
                    restart_span(detail)
                elif kind == "receive_gap":
                    gaps += 1
                    restart_span(detail)
                elif kind == "late_burst":
                    late_bursts += 1
                    log(f"program-audio sampler: {detail}")
            have_audio = True
            last_audio = now
            prev = (block.timestamp, block.samples.shape[0], block.sample_rate)
            for win, sr in acc.push(block.samples, block.sample_rate):
                verdict, rms, outside, reason, markers, chain = classify_window(win, sr, span, decoder)
                write(verdict, rms, outside, reason, markers, chain)
                counts[verdict] += 1
                if verdict != last_verdict:
                    log(f"program-audio verdict {last_verdict} -> {verdict} rms={_fmt(rms)} dBFS "
                        f"outside_band={_fmt(outside)}% marker_chain={_fmt_count(chain)} "
                        f"markers={_fmt_count(markers)} sr={sr} channels={win.shape[1]}"
                        + (f": {reason}" if reason else ""))
                    last_verdict = verdict
        elif now - last_audio >= no_audio_timeout_s and now - last_unknown >= window_s:
            reason = (f"no audio from {source!r} for {now - last_audio:.1f} s "
                      f"(connections={receiver.connections()})")
            write("UNKNOWN", None, None, reason=reason)
            counts["UNKNOWN"] += 1
            last_unknown = now
            if last_verdict != "UNKNOWN":
                log(f"program-audio verdict {last_verdict} -> UNKNOWN: {reason}")
                last_verdict = "UNKNOWN"
        if now - last_summary >= LOG_SUMMARY_S:
            log("program-audio summary (last %.0f s): %s error_frames=%d bad_rate_frames=%d "
                "timeline_breaks=%d late_bursts=%d receive_gaps=%d"
                % (now - last_summary, " ".join(f"{k}={v}" for k, v in counts.items()),
                   errors, bad_rate, breaks, late_bursts, gaps))
            counts = {v: 0 for v in pa.VERDICTS}
            errors = bad_rate = gaps = breaks = late_bursts = 0
            last_summary = now


def judge_continuity(prev, block, arrival_gap_s: float, receive_gap_s: float) -> tuple[str, str]:
    """How `block` follows the previous audio block `prev` = (timestamp, samples, sample_rate):
    (kind, log detail), kind one of
      "continue"        on the sender timeline, arrival within receive_gap_s: nothing to say
      "late_burst"      on the sender timeline after an arrival gap over receive_gap_s: the span is
                        kept (the SDK queued the audio while dev1 starved the sampler)
      "timeline_break"  off the sender timeline (pa.DISCONTINUITY): the span restarts
      "receive_gap"     a timestamp is undefined (pa.UNKNOWN_TS) and the arrival gap is over
                        receive_gap_s: the fallback restarts the span"""
    p_ts, p_n, p_sr = prev
    tol = pa.continuity_tolerance_100ns(p_n, p_sr)
    verdict = pa.frame_continues(p_ts, p_n, p_sr, block.timestamp, tol)
    if verdict == pa.UNKNOWN_TS:
        if arrival_gap_s > receive_gap_s:
            return "receive_gap", (f"receive gap of {arrival_gap_s:.1f} s (no NDI sender timestamp, "
                                   "the arrival-time fallback)")
        return "continue", ""
    off_ms = pa.timeline_offset_100ns(p_ts, p_n, p_sr, block.timestamp) * 1e3 / pa.NDI_TIME_UNITS_PER_S
    if verdict == pa.DISCONTINUITY:
        return "timeline_break", (f"audio timeline discontinuity: the frame sits {off_ms:+.1f} ms off "
                                  f"the sender's timeline (tolerance +-{tol * 1e3 / pa.NDI_TIME_UNITS_PER_S:.1f} ms, "
                                  f"arrival gap {arrival_gap_s:.1f} s)")
    if arrival_gap_s > receive_gap_s:
        return "late_burst", (f"late burst after {arrival_gap_s:.1f} s without audio: the sender "
                              f"timeline continues ({off_ms:+.1f} ms), the marker span is kept")
    return "continue", ""


def _fmt(v) -> str:
    return "-" if not pa.is_number(v) else f"{v:.1f}"


def _fmt_count(v) -> str:
    return "-" if v is None else str(v)


def classify_window(win: np.ndarray, sr: int, span: MarkerSpan, decoder):
    """One 2 s window -> (verdict, rms, outside, reason, markers_decoded, marker_chain): the spectral
    measurement of this window and the marker chain over the trailing span (module doc of
    program_audio.py). A decode failure leaves the chain unknown, which never reads MEASUREMENT."""
    rms, outside = pa.analyse(win, sr)
    silent = pa.is_number(rms) and rms < pa.SILENT_RMS_DBFS
    full = span.push(win, sr, silent)
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
            markers, chain = pa.span_markers(decoder.decode(full, sr))
        except (pam.DecodeError, ValueError) as exc:
            reason = f"marker decode failed: {exc}"
    elif not silent:
        reason = (f"marker span: {span.non_silent_s:g} of {pa.MARKER_SPAN_S:g} s of non-silent audio "
                  "since the last silent window")
    verdict = pa.classify(rms, outside, chain)
    if verdict != "UNKNOWN":
        reason = None
    elif reason is None:
        reason = "the window's level or spectrum is not a number"
    return verdict, rms, outside, reason, markers, chain


def private_ndi_config_dir() -> str:
    """A fresh, empty NDI config dir: libndi then uses its defaults = mDNS discovery only."""
    return tempfile.mkdtemp(prefix="program-audio-sampler-ndi-")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="issue 1404 -- the dev1 stream program-audio sampler")
    ap.add_argument("--source", default=os.environ.get(SOURCE_ENV) or DEFAULT_SOURCE,
                    help=f"NDI source name (default ${SOURCE_ENV} or {DEFAULT_SOURCE!r})")
    ap.add_argument("--serve-dir", default=rsf.default_serve_dir(),
                    help=f"where program-audio.json goes (default ${rsf.SERVE_DIR_ENV} or "
                         "$XDG_RUNTIME_DIR/rig-lease-serve; never the lease dir)")
    ap.add_argument("--lib", default=None, help="libndi path (default $NDI_LIB_PATH or /usr/lib/ndi/libndi.so.6)")
    ap.add_argument("--marker-shim", default=None,
                    help=f"the QPSK marker decoder shim (default ${pam.SHIM_ENV} or {pam.DEFAULT_SHIM_PATH}; "
                         "built by scripts/build-qpsk-guard-shim.sh)")
    args = ap.parse_args(argv)

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
        stop = {"signal": None}

        def _stop(signum, _frame):
            stop["signal"] = signum  # only a flag: no I/O inside a signal handler

        signal.signal(signal.SIGTERM, _stop)
        signal.signal(signal.SIGINT, _stop)
        try:
            run(receiver, args.serve_dir, source=args.source, decoder=decoder,
                should_stop=lambda: stop["signal"] is not None)
        finally:
            receiver.close()
            set_unknown("sampler stopped")
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
