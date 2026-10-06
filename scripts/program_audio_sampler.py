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
* Logging: a line per verdict CHANGE, the first NDI error frame / bad sample rate of a run, and a
  summary every LOG_SUMMARY_S -- never a line per 2 s window (~43 000 a day).

Usage (systemd/program-audio-sampler.service):
  program_audio_sampler.py [--source "STREAM-SNV (stream)"] [--serve-dir DIR] [--lib PATH]
"""
from __future__ import annotations

import argparse
import os
import shutil
import signal
import sys
import tempfile
import time
from datetime import datetime, timezone
from typing import Callable

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

DEFAULT_SOURCE = "STREAM-SNV (stream)"
SOURCE_ENV = "PROGRAM_AUDIO_SOURCE"
NO_AUDIO_TIMEOUT_S = 5.0
CAPTURE_TIMEOUT_MS = 500
LOG_SUMMARY_S = 600.0


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


def run(receiver, serve_dir: str, *, source: str, mono: Callable[[], float] = time.monotonic,
        max_loops: int | None = None, on_write: Callable[[dict], None] | None = None,
        log: Callable[[str], None] = log, sleep: Callable[[float], None] = time.sleep,
        capture_timeout_ms: int = CAPTURE_TIMEOUT_MS, no_audio_timeout_s: float = NO_AUDIO_TIMEOUT_S,
        window_s: float = pa.WINDOW_S, should_stop: Callable[[], bool] = lambda: False) -> None:
    """The sampler loop. `receiver` has capture(timeout_ms) -> AudioBlock|None (raises
    ConnectionError on an NDI error frame) and connections(). Every written payload also goes
    through `on_write` (tests)."""
    latch = {"last_foreign": None}

    def write(verdict, rms, outside, reason=None):
        now = datetime.now(timezone.utc)
        if verdict == "FOREIGN":
            latch["last_foreign"] = rsf.format_ts_utc(now)
        payload = pa.build_payload(verdict, rms, outside, now=now, window_s=window_s, source=source,
                                   reason=reason, last_foreign_ts_utc=latch["last_foreign"])
        pa.write_payload(serve_dir, payload)
        if on_write is not None:
            on_write(payload)
        return payload

    acc = WindowAccumulator(window_s)
    write("UNKNOWN", None, None, reason="sampler starting")
    log(f"program-audio sampler: source={source!r} serve_dir={serve_dir} window={window_s}s "
        f"band={pa.BAND_LO_HZ:.0f}-{pa.BAND_HI_HZ:.0f}Hz tone_lines={pa.MEASUREMENT_TONE_LINES_HZ} "
        f"foreign>={pa.FOREIGN_OUTSIDE_BAND_PCT}% silent<{pa.SILENT_RMS_DBFS}dBFS")
    last_audio = mono()
    last_unknown = mono()
    last_verdict = "UNKNOWN"
    last_summary = mono()
    counts = {v: 0 for v in pa.VERDICTS}
    errors = bad_rate = 0
    in_error = False
    bad_rate_logged = False
    loops = 0
    while not should_stop() and (max_loops is None or loops < max_loops):
        loops += 1
        try:
            block = receiver.capture(capture_timeout_ms)
        except ConnectionError as exc:
            errors += 1
            if not in_error:
                log(f"program-audio sampler: {exc} -- the SDK reconnects by itself")
                in_error = True
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
            last_audio = now
            for win, sr in acc.push(block.samples, block.sample_rate):
                rms, outside = pa.analyse(win, sr)
                verdict = pa.classify(rms, outside)
                write(verdict, rms, outside)
                counts[verdict] += 1
                if verdict != last_verdict:
                    log(f"program-audio verdict {last_verdict} -> {verdict} rms={rms:.1f} dBFS "
                        f"outside_band={'-' if outside is None else f'{outside:.1f}'}% sr={sr} "
                        f"channels={win.shape[1]}")
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
            log("program-audio summary (last %.0f s): %s error_frames=%d bad_rate_frames=%d" % (
                now - last_summary, " ".join(f"{k}={v}" for k, v in counts.items()), errors, bad_rate))
            counts = {v: 0 for v in pa.VERDICTS}
            errors = bad_rate = 0
            last_summary = now


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
            run(receiver, args.serve_dir, source=args.source, should_stop=lambda: stop["signal"] is not None)
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
