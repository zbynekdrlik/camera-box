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
    done = threading.Event()

    def _term_once_capturing():
        # signal only once main() is capturing (its handler is installed before the capture thread
        # starts), never on a fixed timer that a slow runner could beat
        end = time.monotonic() + 20.0
        while not done.is_set() and time.monotonic() < end:
            if _MainRx.instances and _MainRx.instances[-1].i > 10:
                os.kill(os.getpid(), signal.SIGTERM)
                return
            time.sleep(0.05)

    killer = threading.Thread(target=_term_once_capturing, daemon=True)
    killer.start()
    try:
        rc = pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "S", "--marker-shim", shim_path,
                       "--http-port", "0"])  # no endpoint: never bind the default port in a test
    finally:
        done.set()
        killer.join(5)
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
# the CPU priority: normal priority, no host tuning (coordinator, 7.10.2026: the sampler moves off
# dev1, so no dev1-specific CPUWeight/Nice); the one start line reports what the sampler runs at
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("unit", ["program-audio-sampler.strih-lx.service"])
def test_the_units_run_the_sampler_at_normal_priority(unit):
    """No Nice= (the old Nice=10 put the sampler behind every other process, and a --user unit cannot
    lower it anyway) and no CPUWeight= (on strih-lx it would rank the sampler ahead of OBS in the same
    slice): normal priority (the dev1 unit was retired, issue 1404 8.10.2026)."""
    import re

    s = (_ROOT / "systemd" / unit).read_text(encoding="utf-8")
    assert not re.search(r"^Nice=", s, re.M)
    assert not re.search(r"^CPUWeight=", s, re.M)


@pytest.mark.parametrize("nice, cpus, weight, warn, says", [
    (0, "12-15", 100, False, "scheduling nice=0 cpus=12-15 cpu.weight=100"),
    (0, "0-15", None, False, "cpus=0-15 cpu.weight=unreadable"),
    (10, "0-3", 100, True, "nice=10"),
    (-5, "12-15", None, False, "nice=-5"),
])
def test_the_scheduling_line_reports_nice_cpus_and_weight(nice, cpus, weight, warn, says):
    line = pas.scheduling_line(nice, cpus, weight)
    assert ("WARNING" in line) is warn
    assert says in line
    assert line.count("\n") == 0


@pytest.mark.parametrize("cpus, want", [
    ({12, 13, 14, 15}, "12-15"), ({0, 2, 3, 5}, "0,2-3,5"), ({7}, "7"), (set(), ""),
])
def test_cpu_list_is_compact(cpus, want):
    assert pas.cpu_list(cpus) == want


def test_scheduling_state_reads_the_cgroup_weight_and_the_affinity(tmp_path):
    cg = tmp_path / "cgroup"
    cg.write_text("0::/user.slice/x.service\n", encoding="utf-8")
    d = tmp_path / "root" / "user.slice" / "x.service"
    d.mkdir(parents=True)
    (d / "cpu.weight").write_text("100\n", encoding="utf-8")
    nice, cpus, weight = pas.scheduling_state(str(cg), str(tmp_path / "root"))
    assert weight == 100 and nice == os.getpriority(os.PRIO_PROCESS, 0)
    assert cpus == pas.cpu_list(os.sched_getaffinity(0))
    assert pas.scheduling_state(str(cg), str(tmp_path / "nowhere"))[2] is None


# ---------------------------------------------------------------------------------------------
# review round 1: the queue drop in the pure decision, a drop on an item the consumer never judges,
# the arrival fallback with a drop, the sync path reads no clock, lag_ms, a date-step span is holed
# ---------------------------------------------------------------------------------------------

DROP4_100NS = 4 * FRAME * NDI_UNITS / SR


def _tol():
    return pa.continuity_tolerance_100ns(FRAME, SR)


def _after_drop(extra_ms):
    """The stamp of the frame after one frame and 4 dropped frames, `extra_ms` off that point."""
    return TS0 + round(FRAME * NDI_UNITS / SR + DROP4_100NS + extra_ms * 10_000)


@pytest.mark.parametrize("extra_ms, want", [
    (0.0, (pa.BRIDGE, 4096)),
    (10.0, (pa.BRIDGE, 4096)),      # send jitter: exactly the dropped audio, never round(offset * sr)
    (-15.0, (pa.BRIDGE, 4096)),
    (100.0, (pa.BRIDGE, 8896)),     # more missing than the drop: the whole offset (185.3 ms)
    (-45.0, (pa.DISCONTINUITY, 0)),  # behind the dropped audio beyond the tolerance
    (180.0, (pa.DISCONTINUITY, 0)),  # 265.3 ms in all: over the bridge limit
])
def test_a_known_drop_is_a_hole_of_exactly_the_dropped_audio(extra_ms, want):
    assert pa.frame_continues(TS0, FRAME, SR, _after_drop(extra_ms), _tol(), dropped_100ns=DROP4_100NS) == want


def test_a_known_drop_without_timestamps_is_still_a_hole():
    undefined = pa.NDI_TIMESTAMP_UNDEFINED
    assert pa.frame_continues(undefined, FRAME, SR, undefined, _tol(), dropped_100ns=DROP4_100NS) \
        == (pa.BRIDGE, 4096)
    assert pa.frame_continues(undefined, FRAME, SR, undefined, _tol(), dropped_100ns=3_000_000) \
        == (pa.DISCONTINUITY, 0)
    assert pa.frame_continues(undefined, FRAME, SR, undefined, _tol()) == (pa.UNKNOWN_TS, 0)


def test_a_date_step_with_a_known_drop_bridges_only_the_drop():
    step = 2_000_000  # 200 ms
    assert pa.frame_continues(TS0, FRAME, SR, _after_drop(200.0), _tol(), dropped_100ns=DROP4_100NS,
                              wall_steps=(step,)) == (pa.DATE_STEP, 4096)
    assert pa.frame_continues(TS0, FRAME, SR, _after_drop(200.0) + 3_000_000 - round(DROP4_100NS), _tol(),
                              dropped_100ns=3_000_000, wall_steps=(step,)) == (pa.DISCONTINUITY, 0)


def test_the_overflow_bridges_the_dropped_audio_even_when_the_next_frame_is_late(rec_clip, tmp_path):
    """The frame after the dropped 4 is stamped 10 ms late (send jitter): the bridge is still exactly
    the 4096 dropped samples, never the 95.3 ms offset the timeline alone would give."""
    keep = int(0.5 / FRAME_S)
    rx = _GatedRx(rec_clip[: 6 * SR], keep + 4)
    k = keep + 4
    rx.blocks[k] = rx.blocks[k]._replace(timestamp=rx.blocks[k].timestamp + 100_000)
    cap = pac.CaptureThread(rx, timeout_ms=50, max_queue_s=0.5)
    rx.cap = cap
    payloads, lines = [], []
    cap.start()
    try:
        assert rx.first_done.wait(10)
        rx.gate.set()
        pas.run(rx, str(tmp_path), source="S", decoder=NoMarkers(), capture=cap, on_write=payloads.append,
                log=lines.append, capture_timeout_ms=50,
                should_stop=lambda: rx.exhausted and cap.qsize() == 0)
    finally:
        cap.stop()
    overflow = [line for line in lines if "queue overflow" in line]
    assert len(overflow) == 1 and "bridged with 4096 zero samples" in overflow[0], lines
    assert payloads[-1]["bridged_ms"] == pytest.approx(4 * FRAME_S * 1e3, abs=0.06)


def _audio_block(i, ts=None):
    x = np.full((FRAME, 2), 0.01, dtype=np.float32)
    return _Block(SR, x, _stamp(i) if ts is None else ts)


def test_a_drop_never_rides_on_an_item_the_consumer_does_not_judge():
    """The drop goes to the next AUDIO block that gets in, never to an error item or an empty block
    (those are never judged, so the hole would be lost)."""
    cap = pac.CaptureThread(object(), timeout_ms=10, max_queue_s=0.05)
    first = pac.Captured(_audio_block(0), None, 1.0, 0)
    cap._put(first)
    cap._put(pac.Captured(_audio_block(1), None, 1.02, 0))
    cap._put(pac.Captured(_audio_block(2), None, 1.04, 0))   # 3 frames > 50 ms: dropped
    cap._put(pac.Captured(None, ConnectionError("lost"), 1.05, None))
    empty = _Block(SR, np.zeros((0, 2), dtype=np.float32), _stamp(3))
    cap._put(pac.Captured(empty, None, 1.06, 0))
    got = [cap.get(10) for _ in range(4)]
    assert [g.dropped_frames for g in got] == [0, 0, 0, 0]
    cap._put(pac.Captured(_audio_block(4), None, 1.08, 0))   # the queue has room again
    last = cap.get(10)
    assert last.dropped_frames == 1 and last.dropped_100ns == pytest.approx(FRAME * NDI_UNITS / SR)


class _Scripted:
    """A capture side that hands the consumer a fixed list of Captured items (no thread, no clock)."""

    def __init__(self, items):
        self.items = list(items)

    def get(self, _timeout_ms):
        return self.items.pop(0) if self.items else None

    def qsize(self):
        return len(self.items)


def test_without_timestamps_a_drop_after_an_arrival_gap_restarts_like_the_fallback(tmp_path):
    """No sender timestamps and an arrival gap over RECEIVE_GAP_S: the arrival fallback restarts the
    span, as without a drop -- the known drop cannot vouch for what else the gap lost."""
    u = pa.NDI_TIMESTAMP_UNDEFINED
    items = [pac.Captured(_audio_block(i, u), None, 100.0 + i * FRAME_S, None) for i in range(5)]
    items.append(pac.Captured(_audio_block(9, u), None, 100.0 + 5 * FRAME_S + 1.5, None,
                              dropped_frames=4, dropped_100ns=DROP4_100NS))
    lines = []
    pas.run(object(), str(tmp_path), source="S", decoder=NoMarkers(), capture=_Scripted(items),
            mono=lambda: 200.0, max_loops=len(items), log=lines.append)
    assert any("receive gap of 1.5 s" in line and "starts over" in line for line in lines), lines
    assert not any("bridged with" in line for line in lines), lines


def test_without_timestamps_a_drop_with_no_arrival_gap_is_bridged(tmp_path):
    u = pa.NDI_TIMESTAMP_UNDEFINED
    items = [pac.Captured(_audio_block(i, u), None, 100.0 + i * FRAME_S, None) for i in range(5)]
    items.append(pac.Captured(_audio_block(9, u), None, 100.0 + 5 * FRAME_S, None,
                              dropped_frames=4, dropped_100ns=DROP4_100NS))
    lines = []
    pas.run(object(), str(tmp_path), source="S", decoder=NoMarkers(), capture=_Scripted(items),
            mono=lambda: 200.0, max_loops=len(items), log=lines.append)
    assert any("queue overflow" in line and "bridged with 4096 zero samples" in line for line in lines), lines


def test_the_in_loop_capture_reads_no_clock_by_default(tmp_path, monkeypatch):
    """run() without a capture thread (the calibration CLI, the loop tests) reads no wall clock unless
    one is passed: a real dantesync date step during a test or a calibration must never decide it."""
    import inspect

    assert inspect.signature(pas.run).parameters["wall_offset"].default is None
    seen = []
    observe = pa.WallSteps.observe

    def spy(self, arrival_s, offset_ns):
        seen.append(offset_ns)
        return observe(self, arrival_s, offset_ns)

    monkeypatch.setattr(pa.WallSteps, "observe", spy)
    blocks = [_audio_block(i) for i in range(10)]
    state = {"i": 0}

    class Rx:
        def capture(self, _t):
            if state["i"] >= len(blocks):
                return None
            state["i"] += 1
            return blocks[state["i"] - 1]

        def connections(self):
            return 1

    pas.run(Rx(), str(tmp_path), source="S", decoder=NoMarkers(), mono=lambda: 0.0, max_loops=12,
            log=lambda m: None)
    assert seen == [None] * len(blocks)


def test_the_payload_carries_the_consumer_lag(rec_clip, tmp_path):
    """lag_ms (additive): how old the newest audio of the judged window was when it was written. The
    consumer stall shows up there, so a lagging sampler is never mistaken for a fresh one. Only the
    block that completes a window sets it: with 2 s windows a 3.5 s stall that starts at the first
    window leaves the second window's block waiting ~1.5 s."""
    audio = rec_clip[: int(5.5 * SR)]
    sdk = _PacedSdk(audio, hold_s=1.3)
    cap = pac.CaptureThread(sdk, timeout_ms=100)
    payloads = []
    stall = _GilStall(3.5)  # the window-2 block arrives ~2 s into the stall: lag ~1.5 s

    def on_write(p):
        payloads.append(p)
        stall(p)

    cap.start()
    try:
        pas.run(sdk, str(tmp_path), source="S", decoder=FixedChain(), capture=cap, on_write=on_write,
                log=lambda m: None, capture_timeout_ms=100,
                should_stop=lambda: sdk.exhausted and cap.qsize() == 0)
    finally:
        cap.stop()
    assert payloads[0]["lag_ms"] is None                     # the start payload: no window yet
    lags = [p["lag_ms"] for p in payloads[1:]]
    assert all(isinstance(v, (int, float)) and v >= 0 for v in lags), lags
    assert max(lags) >= 1000.0, lags                          # the window right after the stall
    assert lags[0] < 500.0, lags


def test_the_lag_is_null_when_not_sampling():
    from datetime import datetime, timezone

    p = pa.build_payload("UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=2.0, source="S")
    assert p["lag_ms"] is None
    q = pa.build_payload("MEASUREMENT", -35.0, 15.0, now=datetime.now(timezone.utc), window_s=2.0, source="S",
                         marker_chain=7, lag_ms=12.34)
    assert q["lag_ms"] == 12.3


class _ShortChain:
    """A decoder that finds 2 markers on one line in any span: a chain of 2."""

    def decode(self, samples, sample_rate):
        ch = samples.shape[1]
        return [[(0.1, 17), (0.6, 47)]] + [[] for _ in range(ch - 1)]


def test_a_span_holding_a_date_step_is_holed_for_the_short_chain_rule():
    """A DATE_STEP inserts no zeros, yet its +-20 ms match can absorb a small real loss: a short chain
    over a span that holds one reads UNKNOWN, never FOREIGN on its own."""
    win = np.full((2 * SR, 2), 0.01, dtype=np.float32)
    rng = np.random.default_rng(5)
    win[:, 0] = (0.03 * np.sin(2 * np.pi * 442 * np.arange(2 * SR) / SR)
                 + 0.001 * rng.standard_normal(2 * SR)).astype(np.float32)
    win[:, 1] = win[:, 0]
    for rebased, want in ((False, "FOREIGN"), (True, "UNKNOWN")):
        span = pas.MarkerSpan()
        pas.classify_window(win, SR, span, _ShortChain())
        verdict, _rms, outside, reason, _m, chain = pas.classify_window(win, SR, span, _ShortChain(),
                                                                         rebased=rebased)
        assert outside < pa.FOREIGN_OUTSIDE_BAND_PCT and chain == 2
        assert verdict == want, (rebased, verdict, reason)
        if rebased:
            assert "date step" in reason


def test_the_two_windows_around_a_date_step_are_marked(rec_clip, tmp_path, monkeypatch):
    """The window that holds the date step and the next one (the 4 s span is two windows) are judged
    as holding it; a later window is not."""
    marks = []
    real = pas.classify_window

    def spy(win, sr, span, decoder, real_mask=None, rebased=False):
        marks.append(rebased)
        return real(win, sr, span, decoder, real_mask, rebased=rebased)

    monkeypatch.setattr(pas, "classify_window", spy)
    k = int(round(6.1 * SR)) // FRAME
    blocks = [_Block(SR, rec_clip[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR + (2_000_000 if i // FRAME >= k else 0))
              for i in range(0, rec_clip.shape[0], FRAME)]
    clock = {"t": 100.0}
    state = {"i": 0}

    class Rx:
        def capture(self, _t):
            i = state["i"]
            if i >= len(blocks):
                return None
            clock["t"] += FRAME / SR
            state["i"] = i + 1
            return blocks[i]

        def connections(self):
            return 1

    def wall():
        return 1_000_000_000 + (200_000_000 if state["i"] - 1 >= k else 0)

    lines = []
    pas.run(Rx(), str(tmp_path), source="S", decoder=NoMarkers(), mono=lambda: clock["t"],
            max_loops=len(blocks), log=lines.append, wall_offset=wall)
    assert any("timeline date step" in line for line in lines), lines
    assert marks == [False, False, False, True, True], marks
