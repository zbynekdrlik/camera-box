#!/usr/bin/env python3
"""issue 1404 -- the CG arm of the stream program-audio guard: listen to the CG chain itself.

WHY (the 3.11.2026 Content ID claim, "Praise - Elevation Worship" on the E2E VOD 138687756): a song
played on the CG box (resolume-snv: Resolume Arena + SongPlayer + the cg OBS) reached the stream
program audio `mbc` 15-20 dB under the cam2 QPSK marker. The spectral share test
(program_audio.classify) read 13.6-21.3 % outside the marker band -- inside the calibrated
measurement range (up to 25.3 %) -- with the marker chain standing, so every window read
MEASUREMENT. A song under the marker is invisible to the stream-side test, so the guard also listens
to the CG chain's OWN NDI audio: the owner rule is that only the cam2 sync signal may reach the
stream, so ANY CG audio above the silence bar is foreign.

The one declaration of the CG sources: the CG box (`CG_BOX_DEFAULT`, env `PROGRAM_AUDIO_CG_BOX`, a
fleet NAME of scripts/lib/obs-fleet.sh) and its NDI outputs (`CG_NDI_OUTPUTS`: the SongPlayer
program sender `SP-program` and the cg OBS output `cg-obs`). The NDI machine name is the box's
declared computer name (obs-fleet.sh `OBS_FLEET_IDENTITY`, read through scripts/obs_fleet_table.py),
so a source is `<identity> (<output>)` = `RESOLUME-SNV (SP-program)` / `RESOLUME-SNV (cg-obs)`.
`PROGRAM_AUDIO_CG_SOURCES` (`|`-separated full NDI names) overrides the list for ops and tests.

The decision (`overlay`, pure), applied to every window the sampler writes:
  * any CG source's loudest audio block over the trailing LEVEL_WINDOW_S at or over
    program_audio.SILENT_RMS_DBFS (-60) -> FOREIGN, reason `cg-audio: '<source>' at <x> dBFS ...`,
    home or away: audio the sampler really RECEIVES from a CG sender is on this LAN (the home probe
    reads the cg OBS's OBS-WS, and SongPlayer can play while that OBS is down; the probe also lags by
    up to HOME_PROBE_INTERVAL_S). The sampler's write() latches every FOREIGN (`last_foreign_ts_utc`),
    so the guard keeps refusing for its latch hold;
    EXCEPT a source whose reading carries an ABSENT correlation (program_audio_cg_corr.judge, the
    issue-1404 refinement after the owner cleaned mbc): its audio was MEASURED not to be in the stream
    program over the lag-searched path, so it is no FOREIGN (status `not-in-program`). PRESENT names
    the ncc and lag in the reason; UNDECIDED (not computable) stays FOREIGN with its why (fail closed);
    a reading with no correlation (no program ring: `CgArm.state()` without one) is the strict rule;
  * the CG box home (obs_fleet_is_home semantics: a traveling box is home only when its host
    resolves AND its OBS-WS :4455 answers; not probed yet = home, fail closed) and a CG source with no
    audio frame for CG_NO_AUDIO_S (5 s, the sampler's own no-audio timeout) -> UNKNOWN over a
    MEASUREMENT/SILENT window (the guard fails closed, exit 2): a CG source we cannot hear cannot be
    proven silent. A stream FOREIGN or UNKNOWN keeps its own verdict. AWAY, this rule is not graded
    (a traveling box at another venue is not receivable at all); the payload names it;
  * nothing to say -> the window's verdict and reason unchanged, byte for byte.

The CG box's third NDI output, `RESOLUME-SNV (CG OVERLAY)`, is NOT declared: read 3.11.2026 from dev1 it
connects and delivers no audio frame at all, so it would read unreceivable for good.

The level is NOT the stream-side `analyse()` level: no tone line is removed (any CG audio is
foreign, the measurement clip's 1 kHz bed included), and it is the LOUDEST NDI block (10 log10 of
the block's mean square over every sample and channel), so a short phrase cannot hide in a quiet
window average.

The thread side (`CgSourceMonitor`, `HomeProbe`, `CgArm`) only captures, keeps per-block levels and
probes; `CgArm.state()` is what the sampler's consumer loop reads once per window.
"""
from __future__ import annotations

import math
import os
import sys
import threading
import time
from collections import deque
from typing import Callable, NamedTuple

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import obs_fleet_table as oft  # noqa: E402
import program_audio as pa  # noqa: E402
import program_audio_cg_corr as pcc  # noqa: E402

CG_BOX_DEFAULT = "resolume-snv"
CG_BOX_ENV = "PROGRAM_AUDIO_CG_BOX"
CG_SOURCES_ENV = "PROGRAM_AUDIO_CG_SOURCES"
# The CG box's NDI outputs that can carry its audio: the SongPlayer program sender and the cg OBS
# program output (.claude/rules/cg-burn-node-role.md, resolume-cg-obs.md).
CG_NDI_OUTPUTS = ("SP-program", "cg-obs")
REASON_CG_AUDIO = "cg-audio"
STATUS_NOT_IN_PROGRAM = "not-in-program"
LEVEL_WINDOW_S = pa.WINDOW_S
CG_NO_AUDIO_S = 5.0           # = program_audio_sampler.NO_AUDIO_TIMEOUT_S (pinned by a test)
HOME_PROBE_INTERVAL_S = 30.0
CAPTURE_TIMEOUT_MS = 500
THREAD_PREFIX = "program-audio-cg"


def cg_box() -> str:
    return os.environ.get(CG_BOX_ENV) or CG_BOX_DEFAULT


def cg_sources(box: str | None = None) -> list[str]:
    """The NDI names of the CG box's audio-carrying outputs. Raises ValueError when the box has no
    declared computer name (the NDI machine name) and no override is set: fail loud, never guess."""
    override = os.environ.get(CG_SOURCES_ENV)
    if override:
        names = [n.strip() for n in override.split("|") if n.strip()]
        if names:
            return names
    box = box or cg_box()
    ident = oft.fleet_expected_identity(box)
    if not ident:
        raise ValueError(f"the CG box {box!r} has no declared computer name in OBS_FLEET_IDENTITY "
                         "(scripts/lib/obs-fleet.sh) -- its NDI source names are unknown")
    return [f"{ident} ({out})" for out in CG_NDI_OUTPUTS]


def block_level_dbfs(samples) -> float:
    """10 log10 of the block's mean square over every sample and channel; DIGITAL_SILENCE_DBFS for
    an all-zero block, NaN for a non-finite sample (never silent)."""
    x = np.asarray(samples, dtype=np.float64)
    if x.size == 0:
        return pa.DIGITAL_SILENCE_DBFS
    ms = float(np.mean(x * x))
    if not math.isfinite(ms):
        return float("nan")
    return 10.0 * math.log10(ms) if ms > 0.0 else pa.DIGITAL_SILENCE_DBFS


class CgReading(NamedTuple):
    """One CG source at the moment the window is written: `level_dbfs` = the loudest block over
    the trailing LEVEL_WINDOW_S (None: no block in it; NaN: a non-finite block), `audio_age_s` =
    seconds since its last audio block (None: never one), `connections` = the receiver's NDI
    connection count (None: unreadable), `error` = why the source cannot be read (None: fine)."""
    source: str
    level_dbfs: float | None
    audio_age_s: float | None
    connections: int | None = None
    error: str | None = None
    correlation: pcc.Correlation | None = None  # is it IN the stream program? None = not asked


class CgState(NamedTuple):
    """The CG arm at one window: the box, whether it is home (True / False / None = not probed yet,
    which counts as home: fail closed) and one reading per source."""
    box: str
    home: bool | None
    readings: tuple


def _loud(r: CgReading) -> bool:
    return pa.is_number(r.level_dbfs) and r.level_dbfs >= pa.SILENT_RMS_DBFS


def _absent(r: CgReading) -> bool:
    """Heard, and MEASURED not in the stream program (the correlation's ABSENT)."""
    return r.correlation is not None and r.correlation.kind == pcc.ABSENT


def _foreign_text(r: CgReading) -> str:
    head = f"{r.source!r} at {r.level_dbfs:.1f} dBFS"
    c = r.correlation
    if c is None:
        return head
    if c.kind == pcc.PRESENT:
        return (f"{head} is IN the stream program (ncc {c.ncc:.3f} >= {pcc.DETECT_NCC:g} at lag "
                f"{c.lag_ms:+.0f} ms)")
    return f"{head} not proven absent from the stream program ({c.why})"


def _unreceivable(r: CgReading) -> str | None:
    """Why a source cannot be proven silent, or None."""
    if r.error:
        return r.error
    if r.audio_age_s is None or not pa.is_number(r.audio_age_s) or r.audio_age_s > CG_NO_AUDIO_S:
        age = "never" if r.audio_age_s is None else f"for {r.audio_age_s:.1f} s"
        return f"no audio {age} (connections={r.connections})"
    if r.level_dbfs is not None and not pa.is_number(r.level_dbfs):
        return "a non-finite audio sample"
    return None


def graded(state: CgState | None) -> bool:
    """Whether the not-receivable rule applies: the CG box is home or not probed yet. CG audio
    actually received is FOREIGN either way."""
    return state is not None and state.home is not False


def overlay(verdict: str, reason: str | None, state: CgState | None) -> tuple[str, str | None]:
    """The window's (verdict, reason) after the CG arm (rules in the module doc)."""
    if state is None:
        return verdict, reason
    loud = [r for r in state.readings if _loud(r) and not _absent(r)]
    if loud:
        text = (f"{REASON_CG_AUDIO}: "
                + ", ".join(_foreign_text(r) for r in loud)
                + f" -- the CG box {state.box} carries audio; only the cam2 marker may reach the stream")
        return "FOREIGN", (reason if verdict == "FOREIGN" and reason else text)
    if graded(state) and verdict in ("MEASUREMENT", "SILENT"):
        deaf = [(r.source, why) for r in state.readings if (why := _unreceivable(r))]
        if not state.readings:
            deaf = [("(none)", "no CG source declared")]
        if deaf:
            return "UNKNOWN", ("cg source not receivable while the CG box "
                               f"{state.box} is home: "
                               + "; ".join(f"{s!r} {why}" for s, why in deaf)
                               + " -- it cannot be proven silent (fail closed)")
    return verdict, reason


def _round1(v):
    return round(float(v), 1) if pa.is_number(v) else None


def payload_block(state: CgState | None) -> dict | None:
    """The additive `cg` object of program-audio.json (None without a CG arm)."""
    if state is None:
        return None
    if any(_loud(r) and not _absent(r) for r in state.readings):
        status = REASON_CG_AUDIO
    elif any(_loud(r) for r in state.readings):
        status = STATUS_NOT_IN_PROGRAM
    elif state.home is False:
        status = "away"
    elif not state.readings or any(_unreceivable(r) for r in state.readings):
        status = "unreceivable"
    else:
        status = "silent"
    return {
        "box": state.box,
        "home": state.home,
        "graded": graded(state),
        "status": status,
        "silent_below_dbfs": pa.SILENT_RMS_DBFS,
        "sources": [{"name": r.source, "level_dbfs": _round1(r.level_dbfs),
                     "audio_age_s": _round1(r.audio_age_s), "connections": r.connections,
                     **({"error": r.error} if r.error else {}),
                     **_correlation_fields(r.correlation)}
                    for r in state.readings],
    }


def _correlation_fields(c: pcc.Correlation | None) -> dict:
    """The additive per-source keys: `in_program` PRESENT / ABSENT / UNDECIDED (absent = not asked:
    the source was not heard), its peak `ncc`, `lag_ms` and, undecided, `why`."""
    if c is None:
        return {}
    out = {"in_program": c.kind,
           "ncc": None if c.ncc is None else round(float(c.ncc), 3),
           "lag_ms": None if c.lag_ms is None else round(float(c.lag_ms), 1)}
    if c.why:
        out["why"] = c.why
    return out


# ------------------------------------------------------------------ the thread side

class CgSourceMonitor:
    """One CG source's audio-only receiver in its own thread: it only captures and keeps each
    block's level (block_level_dbfs) with its arrival time. `reading(now)` is the loudest level over
    the trailing window and the age of the last block. An NDI error frame is recorded (the SDK
    reconnects by itself); any other exception is kept as the source's error (it reads unreceivable,
    so the guard fails closed) and the thread waits one timeout before the next try."""

    def __init__(self, receiver, source: str, *, mono: Callable[[], float] = time.monotonic,
                 window_s: float = LEVEL_WINDOW_S, timeout_ms: int = CAPTURE_TIMEOUT_MS,
                 sleep: Callable[[float], None] = time.sleep):
        self.receiver = receiver
        self.source = source
        self._mono = mono
        self._window_s = window_s
        self._timeout_ms = timeout_ms
        self._sleep = sleep
        self._lock = threading.Lock()
        self._levels: deque[tuple[float, float]] = deque()
        self._last_audio: float | None = None
        self._error: str | None = None
        self.ring = pcc.AudioRing(mono=True)  # the audio itself, for the in-program correlation
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def poll_once(self) -> None:
        """One capture call (the thread's loop body; tests call it directly)."""
        try:
            block = self.receiver.capture(self._timeout_ms)
        except ConnectionError as exc:
            with self._lock:
                self._error = str(exc)
            self._sleep(self._timeout_ms / 1000.0)
            return
        except Exception as exc:  # noqa: BLE001 -- kept as the source's error: it reads unreceivable
            with self._lock:
                self._error = f"capture failed: {exc!r}"
            self._sleep(self._timeout_ms / 1000.0)
            return
        if block is None or block.sample_rate <= 0 or block.samples.shape[0] == 0:
            return
        level = block_level_dbfs(block.samples)
        push_error = None
        try:
            self.ring.push(getattr(block, "timestamp", pa.NDI_TIMESTAMP_UNDEFINED), block.samples,
                           block.sample_rate)
        except Exception as exc:  # noqa: BLE001 -- never kill the capture thread (review round 1)
            push_error = f"ring push failed: {exc!r}"
        t = self._mono()
        with self._lock:
            # The level is kept whatever the ring did (review round 2): a loud block is CG audio heard,
            # FOREIGN home or away; the error makes its correlation UNDECIDED (CgArm.state).
            self._error = push_error
            self._last_audio = t
            self._levels.append((t, level))
            self._trim(t)

    def _trim(self, now: float) -> None:
        while self._levels and self._levels[0][0] < now - self._window_s:
            self._levels.popleft()

    def reading(self, now: float | None = None) -> CgReading:
        now = self._mono() if now is None else now
        try:
            conns = int(self.receiver.connections())
        except Exception:  # noqa: BLE001 -- a count is diagnostics only
            conns = None
        with self._lock:
            self._trim(now)
            levels = [lv for _t, lv in self._levels]
            last, error = self._last_audio, self._error
        if not levels:
            level = None
        elif any(not pa.is_number(lv) for lv in levels):
            level = float("nan")
        else:
            level = max(levels)
        return CgReading(self.source, level, None if last is None else now - last, conns, error)

    def _run(self) -> None:
        while not self._stop.is_set():
            self.poll_once()

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name=f"{THREAD_PREFIX}-capture", daemon=True)
        self._thread.start()

    def stop(self, join_s: float = 3.0) -> bool:
        """Stop the thread; True once it left the capture call (only then may the receiver be closed)."""
        self._stop.set()
        if self._thread is None:
            return True
        self._thread.join(join_s)
        return not self._thread.is_alive()


class HomeProbe:
    """obs_fleet_is_home(box) every HOME_PROBE_INTERVAL_S in its own thread (a DNS lookup and a TCP
    connect must never stall the consumer). `home` is None until the first probe answered."""

    def __init__(self, box: str, is_home: Callable[[str], bool] = oft.fleet_is_home,
                 interval_s: float = HOME_PROBE_INTERVAL_S):
        self.box = box
        self._is_home = is_home
        self._interval_s = interval_s
        self.home: bool | None = None
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def probe_once(self) -> bool | None:
        try:
            self.home = bool(self._is_home(self.box))
        except Exception:  # noqa: BLE001 -- an unreadable probe counts as home (fail closed)
            self.home = None
        return self.home

    def _run(self) -> None:
        while not self._stop.is_set():
            self.probe_once()
            self._stop.wait(self._interval_s)

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name=f"{THREAD_PREFIX}-home", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()


def open_arm(box: str, names: list[str], make_receiver: Callable[[str], object]) -> "CgArm":
    """Create one receiver per CG source (`make_receiver(name)`, the sampler passes the audio-only
    NdiAudioReceiver) and the arm over them, NOT started. A receiver that fails to create closes the
    ones made before it and the error propagates."""
    receivers = []
    try:
        for name in names:
            receivers.append(make_receiver(name))
    except BaseException:
        for r in receivers:
            r.close()
        raise
    return CgArm(box, [CgSourceMonitor(r, n) for r, n in zip(receivers, names)], HomeProbe(box))


def close_arm(arm: "CgArm", log: Callable[[str], None]) -> None:
    """Stop the arm and close each receiver whose capture thread left the SDK call (never one under
    a running capture: that would crash the SDK; such a receiver is left to the process exit)."""
    stopped = arm.stop()
    for m in arm.monitors:
        if m in stopped:
            m.receiver.close()
        else:
            log(f"program-audio sampler: ERROR the CG capture thread of {m.source!r} is still inside the NDI "
                "capture call -- its receiver is left to the process exit")


def start_line(arm: "CgArm") -> str:
    """The sampler's one start line about the CG arm."""
    return (f"program-audio sampler: CG arm box={arm.box} sources={arm.sources} foreign at >= "
            f"{pa.SILENT_RMS_DBFS:g} dBFS (the loudest block over {LEVEL_WINDOW_S:g} s, home or away) "
            f"unless measured not in the stream program (whitened ncc < {pcc.DETECT_NCC:g} over "
            f"{pcc.PASS_LO_HZ:g}-{pcc.PASS_HI_HZ:g} Hz, lag {pcc.LAG_MIN_MS:+g}..{pcc.LAG_MAX_MS:+g} ms, "
            f"{pcc.PROGRAM_SEGMENT_S:g} s), unknown after {CG_NO_AUDIO_S:g} s without audio while "
            "obs_fleet_is_home")


def _correlate(prog, monitor) -> pcc.Correlation:
    """pcc.judge of one monitor's ring against the program snapshot. Any exception is UNDECIDED (the
    strict FOREIGN, fail closed), never a crash of the consumer loop (review round 1)."""
    try:
        cg_frames, cg_bad = monitor.ring.snapshot()
        return pcc.judge(prog[0], cg_frames, program_bad_stamp=prog[1], cg_bad_stamp=cg_bad)
    except Exception as exc:  # noqa: BLE001 -- the window must still be written
        return pcc.Correlation(pcc.UNDECIDED, why=f"correlation failed: {exc!r}")


class CgArm:
    """The box, its home probe and one monitor per source; `state()` is what the sampler reads."""

    def __init__(self, box: str, monitors: list, home_probe):
        self.box = box
        self.monitors = list(monitors)
        self.home_probe = home_probe
        self._last = {}  # source -> its recent RAW correlations (program_audio_cg_corr.confirm / remember)

    @property
    def sources(self) -> list[str]:
        return [m.source for m in self.monitors]

    def state(self, now: float | None = None, program: pcc.AudioRing | None = None) -> CgState:
        """Every source's reading; with the stream program's ring, each source heard at or over the
        silence bar also carries its in-program correlation (program_audio_cg_corr.judge)."""
        readings = []
        prog = None if program is None else program.snapshot()
        for m in self.monitors:
            r = m.reading(now)
            if prog is not None and _loud(r):
                raw = (pcc.Correlation(pcc.UNDECIDED, why=f"the CG source: {r.error}")
                       if r.error else _correlate(prog, m))
                hist = self._last.get(m.source, ())
                r = r._replace(correlation=pcc.confirm(hist, raw))
                self._last[m.source] = pcc.remember(hist, raw)
            elif prog is None:
                self._last.pop(m.source, None)
            # a quiet window keeps the memory: it ages by the judged segments' end stamps
            # (CONFIRM_SPAN_S), and a passage under the silence bar must not reset a song's lag chain
            readings.append(r)
        return CgState(self.box, self.home_probe.home, tuple(readings))

    def start(self) -> None:
        self.home_probe.start()
        for m in self.monitors:
            m.start()

    def stop(self) -> list:
        """Stop everything; the monitors whose thread left the capture call (safe to close)."""
        self.home_probe.stop()
        return [m for m in self.monitors if m.stop()]
