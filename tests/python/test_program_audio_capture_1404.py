"""issue 1404 -- the program-audio sampler's capture thread (design issue 1404 comment 6037613222).

STEP 0 (7.10.2026, comment 6037861831): two read-only probes on the live `STREAM-SNV (stream)` under
the natural dev1 load. The single loop did not call NDIlib_recv_capture_v3 for 1.74-2.03 s four times
in 383 s, and each time the SDK, which holds about 1.3 s of audio, dropped the oldest 0.36-0.83 s: a
span restart = an UNKNOWN window. A capture thread that does nothing but capture and queue never went
over 0.5 s between two captures and lost nothing.

Covered here:
  * the capture thread keeps up while the consumer is stalled for 2 s (no hole); the same stall in
    the single loop loses audio (the control that keeps the test honest);
  * a full queue drops frames, counts them as `queue_drops` (summary + JSON) and the consumer treats
    them as a hole of exactly their duration: bridged up to 250 ms, a span restart beyond;
  * the capture thread never runs the marker decode or the FFT;
  * SIGTERM stops both threads, the receiver is closed only after the capture thread left the SDK
    call, and program-audio.json reads UNKNOWN;
  * dev1's wall-minus-monotonic offset is ONE bracketed read per block.
"""
from __future__ import annotations

import json
import os
import pathlib
import signal
import sys
import threading
import time

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for _p in (str(_SCRIPTS), str(_HERE)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import program_audio as pa  # noqa: E402
import program_audio_capture as pac  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402
from qpsk_guard_shim_1404 import FixedChain, NoMarkers, build_shim  # noqa: E402
from test_program_audio_timeline_1404 import _Block, _run, _verdicts  # noqa: E402

REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
SR = 48000
FRAME = 1024
FRAME_S = FRAME / SR
TS0 = 17_913_444_397_535_105
NDI_UNITS = 10_000_000


@pytest.fixture(scope="session")
def rec_clip():
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == SR and x.shape[1] == 2
    return np.ascontiguousarray(x, dtype=np.float32)


@pytest.fixture(scope="session")
def shim_path(tmp_path_factory):
    return build_shim(tmp_path_factory.mktemp("qpsk-guard-shim-capture"))


def _stamp(i):
    return TS0 + (i * FRAME * NDI_UNITS) // SR


class _PacedSdk:
    """An NDI receiver the way the SDK behaves: a frame becomes available every FRAME / SR seconds
    of wall time from the first capture call, the SDK holds at most `hold_s` of audio and drops the
    OLDEST beyond that (live: 0.36-0.83 s lost after 1.74-2.03 s without a capture call), and
    capture(timeout) waits up to the timeout for the next frame like NDIlib_recv_capture_v3."""

    def __init__(self, audio, hold_s):
        self.audio = audio
        self.n = audio.shape[0] // FRAME
        self.hold = max(1, int(hold_s / FRAME_S))
        self.t0 = None
        self.next = 0
        self.lost = 0
        self.threads = set()

    @property
    def exhausted(self):
        return self.next >= self.n

    def _available(self, now):
        return min(self.n, int((now - self.t0) / FRAME_S) + 1)

    def capture(self, timeout_ms):
        self.threads.add(threading.current_thread().name)
        now = time.monotonic()
        if self.t0 is None:
            self.t0 = now
        deadline = now + timeout_ms / 1000.0
        while True:
            avail = self._available(time.monotonic())
            if avail - self.next > self.hold:
                self.lost += avail - self.hold - self.next
                self.next = avail - self.hold
            if self.next < avail:
                i = self.next
                self.next += 1
                return _Block(SR, self.audio[i * FRAME:(i + 1) * FRAME], _stamp(i))
            if self.exhausted:
                time.sleep(min(0.01, timeout_ms / 1000.0))
                return None
            wait = min(self.t0 + self.next * FRAME_S, deadline) - time.monotonic()
            if wait <= 0:
                return None
            time.sleep(wait)

    def connections(self):
        return 1


class _GilStall:
    """The consumer's work stalls ONCE for `seconds`: a pure-Python busy loop that holds the GIL
    (the interpreter still switches threads every few ms, as during the real FFT + decode)."""

    def __init__(self, seconds):
        self.seconds = seconds
        self.done = False

    def __call__(self, _payload):
        if self.done or _payload.get("reason") == "sampler starting":
            return
        self.done = True
        end = time.monotonic() + self.seconds
        while time.monotonic() < end:
            pass


def _hole_lines(lines):
    return [line for line in lines if "timeline hole" in line or "timeline discontinuity" in line
            or "queue overflow" in line]


def test_the_capture_thread_keeps_up_while_the_consumer_stalls_2_s(rec_clip, tmp_path):
    """The consumer is stalled for 2 s by GIL-holding work right after its first window. The capture
    thread keeps calling the SDK, the queue absorbs the 2 s, nothing is lost: no hole, no restart,
    no queue drop, and the verdicts are the unbroken clip's."""
    audio = rec_clip[: int(5.5 * SR)]
    sdk = _PacedSdk(audio, hold_s=1.3)
    cap = pac.CaptureThread(sdk, timeout_ms=100)
    payloads, lines = [], []
    stall = _GilStall(2.0)

    def on_write(p):
        payloads.append(p)
        stall(p)

    cap.start()
    try:
        pas.run(sdk, str(tmp_path), source="S", decoder=FixedChain(), capture=cap,
                on_write=on_write, log=lines.append, capture_timeout_ms=100,
                should_stop=lambda: sdk.exhausted and cap.qsize() == 0)
    finally:
        cap.stop()
    assert stall.done
    assert sdk.lost == 0, (sdk.lost, lines)
    assert _hole_lines(lines) == [], lines
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT"], (_verdicts(payloads), lines)
    assert payloads[-1]["queue_drops"] == 0 and payloads[-1]["holes_bridged"] == 0
    assert sdk.threads == {pac.CAPTURE_THREAD_NAME}


def test_the_same_stall_in_the_single_loop_loses_audio(rec_clip, tmp_path):
    """The control: the same 2 s stall with the capture call in the consumer loop (no thread). The
    SDK holds 1.3 s, drops the rest, and the span restarts on the hole."""
    audio = rec_clip[: int(5.5 * SR)]
    sdk = _PacedSdk(audio, hold_s=1.3)
    payloads, lines = [], []
    stall = _GilStall(2.0)

    def on_write(p):
        payloads.append(p)
        stall(p)

    pas.run(sdk, str(tmp_path), source="S", decoder=FixedChain(), on_write=on_write,
            log=lines.append, capture_timeout_ms=100, should_stop=lambda: sdk.exhausted)
    assert sdk.lost > 0
    assert any("timeline discontinuity" in line for line in lines), lines


class _GatedRx:
    """Phase 1 (`first` frames) is captured as fast as it can, before the consumer starts; phase 2
    flows once `gate` is set, one frame at a time and only while the capture queue is empty, so
    the only drops are phase 1's (deterministic). A queue sized below phase 1 drops its tail."""

    def __init__(self, audio, first):
        self.blocks = [_Block(SR, audio[i * FRAME:(i + 1) * FRAME], _stamp(i))
                       for i in range(audio.shape[0] // FRAME)]
        self.first = first
        self.i = 0
        self.gate = threading.Event()
        self.first_done = threading.Event()
        self.cap = None

    @property
    def exhausted(self):
        return self.i >= len(self.blocks)

    def capture(self, timeout_ms):
        if self.i >= self.first and not self.gate.is_set():
            self.first_done.set()
            self.gate.wait(timeout_ms / 1000.0)
            return None
        if self.exhausted:
            time.sleep(0.005)
            return None
        if self.i >= self.first and self.cap is not None and self.cap.qsize() > 0:
            time.sleep(0.001)
            return None
        b = self.blocks[self.i]
        self.i += 1
        return b

    def connections(self):
        return 1


def _run_gated(audio, first, max_queue_s, tmp_path, decoder):
    rx = _GatedRx(audio, first)
    cap = pac.CaptureThread(rx, timeout_ms=50, max_queue_s=max_queue_s)
    rx.cap = cap
    payloads, lines = [], []
    cap.start()
    try:
        assert rx.first_done.wait(10)
        drops_before_consumer = cap.drops_total
        rx.gate.set()
        pas.run(rx, str(tmp_path), source="S", decoder=decoder, capture=cap, on_write=payloads.append,
                log=lines.append, capture_timeout_ms=50,
                should_stop=lambda: rx.exhausted and cap.qsize() == 0)
    finally:
        cap.stop()
    return payloads, lines, drops_before_consumer


def test_a_queue_overflow_is_counted_and_bridged_as_a_hole_of_its_exact_length(rec_clip, tmp_path):
    """The queue holds 0.5 s; phase 1 is 0.5 s + 4 frames, so the 4 newest frames of it are dropped
    (85.3 ms). The consumer reads them as a hole of exactly 4 * 1024 samples, bridged, the span
    kept; `queue_drops` reaches the JSON and the overflow is logged."""
    keep = int(0.5 / FRAME_S)
    payloads, lines, drops = _run_gated(rec_clip[: 8 * SR], keep + 4, 0.5, tmp_path, NoMarkers())
    assert drops == 4
    overflow = [line for line in lines if "queue overflow" in line]
    assert len(overflow) == 1 and "4 frames" in overflow[0] and "bridged with 4096 zero samples" in overflow[0], lines
    assert payloads[-1]["queue_drops"] == 4
    assert payloads[-1]["holes_bridged"] == 1
    assert payloads[-1]["bridged_ms"] == pytest.approx(4 * FRAME_S * 1e3, abs=0.06)
    assert not any("starts over" in line for line in lines), lines


def test_a_queue_overflow_over_250_ms_restarts_the_span(rec_clip, tmp_path):
    """0.5 s kept, 1 s of phase 1 dropped: a hole over HOLE_BRIDGE_MAX_MS restarts the span."""
    keep = int(0.5 / FRAME_S)
    drop = int(1.0 / FRAME_S)
    payloads, lines, drops = _run_gated(rec_clip[: 8 * SR], keep + drop, 0.5, tmp_path, FixedChain())
    assert drops == drop
    assert any("queue overflow" in line and "starts over" in line for line in lines), lines
    assert payloads[-1]["queue_drops"] == drop
    assert payloads[-1]["holes_bridged"] == 0


def test_the_queue_drops_reach_the_summary_line(rec_clip, tmp_path, monkeypatch):
    monkeypatch.setattr(pas, "LOG_SUMMARY_S", 0.0)
    keep = int(0.5 / FRAME_S)
    _payloads, lines, _drops = _run_gated(rec_clip[: 4 * SR], keep + 3, 0.5, tmp_path, NoMarkers())
    summary = [line for line in lines if "program-audio summary" in line]
    assert any("queue_drops=3" in line for line in summary), summary
    assert sum(int(line.split("queue_drops=")[1].split()[0]) for line in summary) == 3


def test_the_capture_thread_never_runs_the_decode_or_the_fft(rec_clip, tmp_path, monkeypatch):
    """The capture thread only calls the SDK and queues: the marker decode and the FFT (analyse)
    run on the consumer thread, never on it -- the bound that keeps its scheduling cheap."""
    seen = {"decode": set(), "analyse": set()}
    analyse = pa.analyse

    def spy_analyse(*a, **k):
        seen["analyse"].add(threading.current_thread().name)
        return analyse(*a, **k)

    class _SpyDecoder(FixedChain):
        def decode(self, samples, sample_rate):
            seen["decode"].add(threading.current_thread().name)
            return super().decode(samples, sample_rate)

    monkeypatch.setattr(pas.pa, "analyse", spy_analyse)
    rx = _GatedRx(rec_clip[: 6 * SR], 0)
    rx.gate.set()
    cap = pac.CaptureThread(rx, timeout_ms=50)
    rx.cap = cap
    cap.start()
    try:
        pas.run(rx, str(tmp_path), source="S", decoder=_SpyDecoder(), capture=cap, log=lambda m: None,
                capture_timeout_ms=50, should_stop=lambda: rx.exhausted and cap.qsize() == 0)
    finally:
        cap.stop()
    assert seen["decode"] and seen["analyse"]
    assert pac.CAPTURE_THREAD_NAME not in seen["decode"] | seen["analyse"], seen
    assert seen["decode"] | seen["analyse"] == {threading.current_thread().name}


def test_a_capture_thread_that_dies_takes_the_sampler_down_never_a_silent_unknown(tmp_path):
    """An unexpected exception in the capture call (a frame the binding cannot read) must not leave
    a live consumer that reads 'no audio' forever: run() raises, main() writes UNKNOWN and exits
    non-zero, and systemd restarts the unit (as the single loop did)."""

    class _Broken:
        def capture(self, timeout_ms):
            raise ValueError("NDI audio FourCC 0x1 is not FLTP")

        def connections(self):
            return 1

    cap = pac.CaptureThread(_Broken(), timeout_ms=20)
    cap.start()
    try:
        with pytest.raises(RuntimeError, match="capture thread died"):
            pas.run(_Broken(), str(tmp_path), source="S", decoder=NoMarkers(), capture=cap,
                    log=lambda m: None, capture_timeout_ms=20, max_loops=200)
    finally:
        cap.stop()


class _MainRx:
    """A receiver for main(): continuous 1024-sample frames in real time; close() records whether
    a capture call was still in flight (closing the SDK receiver under a running capture is a
    use-after-free)."""

    instances = []

    def __init__(self, source, lib_path=None):
        self.i = 0
        self.in_capture = False
        self.closed_during_capture = None
        self.t0 = time.monotonic()
        _MainRx.instances.append(self)

    def capture(self, timeout_ms):
        self.in_capture = True
        try:
            due = self.t0 + self.i * FRAME_S
            time.sleep(max(0.0, min(due - time.monotonic(), timeout_ms / 1000.0)))
            x = np.full((FRAME, 2), 0.01, dtype=np.float32)
            b = pan.AudioBlock(SR, x, _stamp(self.i))
            self.i += 1
            return b
        finally:
            self.in_capture = False

    def connections(self):
        return 1

    def close(self):
        self.closed_during_capture = self.in_capture


def test_sigterm_stops_both_threads_and_writes_unknown(tmp_path, monkeypatch, shim_path):
    monkeypatch.setattr(pan, "NdiAudioReceiver", _MainRx)
    _MainRx.instances.clear()
    before = (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGINT))
    timer = threading.Timer(1.5, os.kill, args=(os.getpid(), signal.SIGTERM))
    timer.start()
    try:
        rc = pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "S", "--marker-shim", shim_path])
    finally:
        timer.cancel()
        signal.signal(signal.SIGTERM, before[0])
        signal.signal(signal.SIGINT, before[1])
    assert rc == 0
    payload = json.loads((tmp_path / "serve" / rsf.PROGRAM_AUDIO_NAME).read_text(encoding="utf-8"))
    assert payload["verdict"] == "UNKNOWN" and payload["reason"] == "sampler stopped"
    assert pac.CAPTURE_THREAD_NAME not in {t.name for t in threading.enumerate()}
    rx = _MainRx.instances[-1]
    assert rx.i > 10                              # the capture thread did capture
    assert rx.closed_during_capture is False      # closed only after the thread left the SDK call


# ---------------------------------------------------------------------------------------------
# dev1's own wall clock: ONE bracketed read per block
# ---------------------------------------------------------------------------------------------


def _clocks(mono, wall):
    m, w = iter(mono), iter(wall)
    return (lambda: next(m)), (lambda: next(w))


def test_the_wall_offset_is_read_at_the_middle_of_a_monotonic_bracket():
    mono_ns, wall_ns = _clocks([1_000, 1_400], [5_000_000])
    assert pac.read_wall_offset_ns(mono_ns=mono_ns, wall_ns=wall_ns) == 5_000_000 - 1_200


def test_a_preempted_bracket_is_retried_and_none_when_never_clean():
    wide = pac.WALL_READ_MAX_BRACKET_NS + 1
    mono_ns, wall_ns = _clocks([0, wide, 10_000, 10_100], [7_000, 9_000_000])
    assert pac.read_wall_offset_ns(mono_ns=mono_ns, wall_ns=wall_ns) == 9_000_000 - 10_050
    mono_ns, wall_ns = _clocks([0, wide] * pac.WALL_READ_ATTEMPTS, [1] * pac.WALL_READ_ATTEMPTS)
    assert pac.read_wall_offset_ns(mono_ns=mono_ns, wall_ns=wall_ns) is None


def test_the_real_clocks_give_a_wall_offset_close_to_time_minus_monotonic():
    got = pac.read_wall_offset_ns()
    want = time.time_ns() - time.monotonic_ns()
    assert got is not None and abs(got - want) < 50_000_000


def test_each_audio_block_gets_exactly_one_wall_read(tmp_path):
    """The sampler reads the clock pair once per captured block, in the capture path, and nowhere
    else: one reader, so a reading cannot race a second one."""
    calls = []

    def wall_offset():
        calls.append(threading.current_thread().name)
        return 123_456_789

    blocks = [_Block(SR, np.full((FRAME, 2), 0.01, dtype=np.float32), _stamp(i)) for i in range(20)]
    state = {"i": 0}

    class Rx:
        def capture(self, _t):
            if state["i"] >= len(blocks):
                return None
            state["i"] += 1
            return blocks[state["i"] - 1]

        def connections(self):
            return 1

    pas.run(Rx(), str(tmp_path), source="S", decoder=NoMarkers(), mono=lambda: 0.0, max_loops=25,
            log=lambda m: None, wall_offset=wall_offset)
    assert len(calls) == len(blocks)


def test_the_sync_path_still_drives_the_existing_loop_tests(tmp_path):
    """The calibration and the loop tests drive run() without a thread (no `capture` given): the same
    consumer, the capture call in the loop -- a smoke check that nothing there needs a thread."""
    x = np.full((6 * SR, 2), 0.01, dtype=np.float32)
    blocks = [_Block(SR, x[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR) for i in range(0, x.shape[0], FRAME)]
    payloads, _lines = _run(blocks, tmp_path, NoMarkers())
    assert payloads[0]["reason"] == "sampler starting"
    assert payloads[-1]["queue_drops"] == 0


# ---------------------------------------------------------------------------------------------
# the CPU priority: the unit's CPUWeight, the one start line
# ---------------------------------------------------------------------------------------------


def test_the_unit_gives_the_sampler_a_cpu_weight_and_no_nice():
    """CPUWeight=1000 against the lanes in the user's app.slice; no Nice= at all: the old Nice=10 put
    the sampler behind every lane, and a --user unit cannot lower it (Nice=-5 runs at 0 on dev1)."""
    import re

    s = (_ROOT / "systemd" / "program-audio-sampler.service").read_text(encoding="utf-8")
    assert re.search(r"^CPUWeight=1000$", s, re.M)
    assert not re.search(r"^Nice=", s, re.M)
    assert pas.CPU_WEIGHT_WANTED == 1000


@pytest.mark.parametrize("nice, weight, warn, says", [
    (0, 1000, True, "its priority is the cgroup's cpu.weight=1000 only"),
    (10, 1000, True, "nice=10"),
    (0, 100, True, "can starve under load"),
    (0, None, True, "cpu.weight=unreadable"),
    (-5, 1000, False, "nice=-5 cpu.weight=1000"),
])
def test_the_scheduling_line_warns_once_when_the_nice_is_not_lowered(nice, weight, warn, says):
    line = pas.scheduling_line(nice, weight, 0)
    assert ("WARNING" in line) is warn
    assert says in line
    assert line.count("\n") == 0


def test_scheduling_state_reads_the_cgroup_weight(tmp_path):
    cg = tmp_path / "cgroup"
    cg.write_text("0::/user.slice/x.service\n", encoding="utf-8")
    d = tmp_path / "root" / "user.slice" / "x.service"
    d.mkdir(parents=True)
    (d / "cpu.weight").write_text("1000\n", encoding="utf-8")
    nice, weight, _lim = pas.scheduling_state(str(cg), str(tmp_path / "root"))
    assert weight == 1000 and nice == os.getpriority(os.PRIO_PROCESS, 0)
    assert pas.scheduling_state(str(cg), str(tmp_path / "nowhere"))[1] is None
