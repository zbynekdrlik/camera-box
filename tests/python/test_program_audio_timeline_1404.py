"""issue 1404 -- the program-audio sampler judges a receive gap on the sender's NDI audio timeline.

Live (7.10.2026): the dev1 sampler logged 57 `MEASUREMENT -> UNKNOWN` transitions in 6 h, every one a
`receive gap of 1.0-2.0 s` measured on ARRIVAL time. While dev1 is loaded the sampler process is
starved for ~1 s and the NDI SDK hands the queued audio over in one burst: no sample is lost, yet the
span restarted and the guard read UNKNOWN (restreamer's watchdog stops a session on that). The
design (issue 1404 comment 6030385284, Approach 1): every NDI audio frame carries the SDK
`timestamp` (100 ns, the sender's submission time) and its sample count, so

  expected = prev_timestamp + prev_samples / sample_rate
  |timestamp - expected| <= one frame duration + 20 ms  -> CONTINUE (whatever the arrival time)
  otherwise                                              -> DISCONTINUITY (the span restarts)
  timestamp undefined (INT64_MAX / 0)                    -> UNKNOWN_TS (the old arrival-gap rule)

Covered here:
  * the pure decision `program_audio.frame_continues` + its single-sourced tolerance;
  * the binding exposes the SDK timestamp on the block;
  * the sampler: a late burst keeps the span, a timestamp hole with no arrival gap restarts it, a
    dantesync date step costs exactly one warm-up (never FOREIGN), an undefined timestamp falls back
    to the arrival gap, an error frame still restarts the span;
  * ROZHODNUTÉ 6027706292 item 1: a spectrally FOREIGN window during the warm-up reads FOREIGN and
    starts the latch (only MEASUREMENT needs the marker chain);
  * the calibration CLI feeds the sampler a continuous sender timeline.
"""
from __future__ import annotations

import ctypes
import pathlib
import sys
from typing import NamedTuple

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for _p in (str(_SCRIPTS), str(_HERE)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import program_audio as pa  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
from qpsk_guard_shim_1404 import FixedChain, NoMarkers, build_shim  # noqa: E402

FIX = _ROOT / "tests" / "fixtures" / "youtube_leg_1404"
SDK_STRUCTS_H = _ROOT / "vendor" / "distroav" / "lib" / "ndi" / "Processing.NDI.structs.h"
SR = 48000
TS0 = 17_913_444_397_535_105   # a real sender timestamp read live on 7.10.2026 (100 ns units)
FRAME = 1024                   # an OBS audio tick, the size NDI delivers
FRAME_100NS = FRAME * 10_000_000 / SR
INT64_MAX = 2**63 - 1          # NDIlib_recv_timestamp_undefined
NDI_UNITS = 10_000_000         # NDI timestamps count 100 ns


class _Block(NamedTuple):
    """The shape program_audio_ndi.AudioBlock has (sample_rate, samples, timestamp); a local type,
    so the sampler tests below exercise the loop and not the binding."""
    sample_rate: int
    samples: np.ndarray
    timestamp: int


@pytest.fixture(scope="session")
def decoder(tmp_path_factory):
    return pam.MarkerDecoder(build_shim(tmp_path_factory.mktemp("qpsk-guard-shim")))


# ---------------------------------------------------------------------------------------------
# the pure decision
# ---------------------------------------------------------------------------------------------


def test_the_undefined_timestamp_is_the_sdk_constant():
    """NDIlib_recv_timestamp_undefined = INT64_MAX in the vendored SDK header."""
    text = SDK_STRUCTS_H.read_text(encoding="utf-8")
    assert "static const int64_t NDIlib_recv_timestamp_undefined = INT64_MAX;" in text
    assert pa.NDI_TIMESTAMP_UNDEFINED == INT64_MAX
    assert pa.NDI_TIME_UNITS_PER_S == 10_000_000


def test_the_tolerance_is_one_frame_plus_20_ms_single_sourced():
    assert pa.CONTINUITY_SLACK_S == 0.020
    assert pa.continuity_tolerance_100ns(FRAME, SR) == pytest.approx(FRAME_100NS + 200_000)
    assert pa.continuity_tolerance_100ns(480, 48000) == pytest.approx(100_000 + 200_000)
    with pytest.raises(ValueError):
        pa.continuity_tolerance_100ns(0, SR)
    with pytest.raises(ValueError):
        pa.continuity_tolerance_100ns(FRAME, 0)


def _tol():
    return pa.continuity_tolerance_100ns(FRAME, SR)


def test_the_next_frame_on_the_timeline_continues():
    nxt = TS0 + round(FRAME_100NS)
    assert pa.frame_continues(TS0, FRAME, SR, nxt, _tol()) == pa.CONTINUE
    assert pa.timeline_offset_100ns(TS0, FRAME, SR, nxt) == pytest.approx(0.0, abs=1.0)


@pytest.mark.parametrize("off_ms", [-21.8, -10.0, 10.7, 21.9, 41.0, -41.0])
def test_the_senders_submission_jitter_continues(off_ms):
    """STEP 0 (7.10.2026, the live stream program): the sender's submission jitter reached
    +-21.9 ms; anything inside one frame + 20 ms (41.3 ms) is the same timeline."""
    ts = TS0 + round(FRAME_100NS + off_ms * 10_000)
    assert pa.frame_continues(TS0, FRAME, SR, ts, _tol()) == pa.CONTINUE


@pytest.mark.parametrize("off_ms", [42.0, -42.0, 100.0, 2500.0, -300.0])
def test_a_timeline_jump_beyond_the_tolerance_is_a_discontinuity(off_ms):
    ts = TS0 + round(FRAME_100NS + off_ms * 10_000)
    assert pa.frame_continues(TS0, FRAME, SR, ts, _tol()) == pa.DISCONTINUITY


@pytest.mark.parametrize("prev, ts", [
    (INT64_MAX, TS0),
    (TS0, INT64_MAX),
    (0, TS0),
    (TS0, 0),
    (None, TS0),
    (TS0, None),
])
def test_an_undefined_timestamp_is_unknown_ts(prev, ts):
    assert pa.frame_continues(prev, FRAME, SR, ts, _tol()) == pa.UNKNOWN_TS


def test_numpy_integer_timestamps_are_judged_like_ints():
    nxt = np.int64(TS0 + round(FRAME_100NS))
    assert pa.frame_continues(np.int64(TS0), FRAME, SR, nxt, _tol()) == pa.CONTINUE


def test_the_decision_refuses_a_frame_that_is_not_one():
    with pytest.raises(ValueError):
        pa.frame_continues(TS0, 0, SR, TS0, _tol())
    with pytest.raises(ValueError):
        pa.frame_continues(TS0, FRAME, 0, TS0, _tol())
    with pytest.raises(ValueError):
        pa.frame_continues(TS0, FRAME, SR, TS0, -1)


# ---------------------------------------------------------------------------------------------
# the binding exposes the SDK timestamp
# ---------------------------------------------------------------------------------------------


def test_the_block_carries_the_timestamp_and_defaults_to_undefined():
    b = pan.AudioBlock(SR, np.zeros((FRAME, 2), dtype=np.float32))
    assert b.timestamp == pa.NDI_TIMESTAMP_UNDEFINED
    assert pan.AudioBlock(SR, np.zeros((FRAME, 2), dtype=np.float32), TS0).timestamp == TS0


def test_capture_exposes_the_frames_sdk_timestamp():
    """capture() copies the frame's `timestamp` (the SDK's 100 ns submission time) onto the block."""
    data = (ctypes.c_float * (FRAME * 2))(*([0.25] * FRAME + [-0.5] * FRAME))
    freed = []

    class FakeLib:
        def NDIlib_recv_capture_v3(self, _recv, _video, frame_ref, _meta, _timeout):
            f = frame_ref._obj
            f.sample_rate, f.no_channels, f.no_samples = SR, 2, FRAME
            f.timecode, f.timestamp = TS0 - 13, TS0
            f.FourCC = pan.FOURCC_FLTP
            f.p_data = ctypes.cast(data, ctypes.c_void_p)
            f.channel_stride_in_bytes = FRAME * 4
            return pan.FRAME_TYPE_AUDIO

        def NDIlib_recv_free_audio_v3(self, _recv, _frame_ref):
            freed.append(True)

    rx = object.__new__(pan.NdiAudioReceiver)
    rx._lib, rx._recv, rx.source_name = FakeLib(), 1, "S"
    block = rx.capture(10)
    assert block.timestamp == TS0
    assert block.sample_rate == SR and block.samples.shape == (FRAME, 2)
    assert block.samples[0, 0] == pytest.approx(0.25) and block.samples[0, 1] == pytest.approx(-0.5)
    assert freed == [True]


# ---------------------------------------------------------------------------------------------
# the sampler
# ---------------------------------------------------------------------------------------------


def _fixture(name):
    """A committed 16 kHz mono clip as stereo float32."""
    x, sr = cal.load_audio(str(FIX / f"{name}.flac"))
    return np.ascontiguousarray(np.concatenate([x, x], axis=1), dtype=np.float32), sr


def block_frames(sr):
    """20 ms blocks, as close to the live 1024-at-48-kHz (21.3 ms) frame as divides a second, so the
    tolerance (one frame + 20 ms = 40 ms) matches the live one and 6 s is a whole block count."""
    return sr // 50


def at_s(seconds, sr):
    """The index of the block that starts `seconds` into the audio."""
    return int(seconds * sr) // block_frames(sr)


def _blocks(stereo, sr, n=None, ts0=TS0, jumps=None, timestamps=True):
    """NDI-sized blocks on a continuous sender timeline (ts0 + sample offset); `jumps` maps a block
    index to a step (100 ns) added to its timestamp and every later one."""
    n = n or block_frames(sr)
    jumps = jumps or {}
    out, shift = [], 0
    for k, i in enumerate(range(0, stereo.shape[0], n)):
        shift += jumps.get(k, 0)
        ts = ts0 + (i * NDI_UNITS) // sr + shift if timestamps else INT64_MAX
        out.append(_Block(sr, stereo[i:i + n], ts))
    return out


class _Clock:
    def __init__(self):
        self.t = 100.0

    def mono(self):
        return self.t


def _run(blocks, tmp_path, decoder, arrival_gaps=None):
    """Drive pas.run over `blocks`: the arrival clock advances by each block's duration, plus
    `arrival_gaps[k]` seconds before block k (the sampler starved, the SDK queueing)."""
    clock = _Clock()
    payloads, lines = [], []
    arrival_gaps = arrival_gaps or {}
    state = {"i": 0}

    class Rx:
        def capture(self, _timeout_ms):
            i = state["i"]
            if i >= len(blocks):
                return None
            clock.t += arrival_gaps.get(i, 0.0) + blocks[i].samples.shape[0] / blocks[i].sample_rate
            state["i"] = i + 1
            return blocks[i]

        def connections(self):
            return 1

    pas.run(Rx(), str(tmp_path), source="S", decoder=decoder, mono=clock.mono,
            max_loops=len(blocks), on_write=payloads.append, log=lines.append)
    return payloads, lines


def _verdicts(payloads):
    return [p["verdict"] for p in payloads]


def test_a_late_burst_keeps_the_span(decoder, tmp_path):
    """The live failure: 1.5 s without audio on dev1, then the queued audio in one burst. The sender
    timeline is continuous, so nothing restarts: no UNKNOWN after the start-up warm-up."""
    stereo, sr = _fixture("base-R-rec")
    blocks = _blocks(stereo[: 10 * sr], sr)
    payloads, lines = _run(blocks, tmp_path, decoder, arrival_gaps={at_s(6, sr): pas.RECEIVE_GAP_S + 0.5})
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 4
    assert any("late burst" in line for line in lines), lines
    assert not any("starts over" in line for line in lines), lines


def test_a_timestamp_hole_with_no_arrival_gap_restarts_the_span(decoder, tmp_path):
    """Real sample loss with no arrival gap: 0.2 s of audio is cut out, the blocks keep arriving at
    their normal pace, and the sender timestamps after the hole sit 0.2 s later. The span restarts:
    the audio around the hole is never stitched into one chain."""
    stereo, sr = _fixture("base-R-rec")
    cut = slice(6 * sr, 6 * sr + sr // 5)
    keep = np.concatenate([stereo[: cut.start], stereo[cut.stop: cut.stop + 4 * sr]])
    blocks = _blocks(keep, sr, jumps={at_s(6, sr): (sr // 5) * NDI_UNITS // sr})
    payloads, lines = _run(blocks, tmp_path, decoder)
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "UNKNOWN",
                                   "MEASUREMENT"]
    assert "warming up" in payloads[4]["reason"]
    assert any("timeline discontinuity" in line and "+200.0 ms" in line for line in lines), lines


@pytest.mark.parametrize("step_s", [2.5, -0.3, 0.05])
def test_a_date_step_costs_one_warm_up_never_foreign(decoder, tmp_path, step_s):
    """A dantesync date step moves the sender's wall clock (and so its NDI timestamps) once. It
    reads as ONE discontinuity: one UNKNOWN warm-up window, never FOREIGN, then MEASUREMENT."""
    stereo, sr = _fixture("base-R-rec")
    blocks = _blocks(stereo[: 10 * sr], sr, jumps={at_s(6, sr): round(step_s * NDI_UNITS)})
    payloads, lines = _run(blocks, tmp_path, decoder)
    verdicts = _verdicts(payloads)
    assert verdicts == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "UNKNOWN", "MEASUREMENT"]
    assert "FOREIGN" not in verdicts and payloads[-1]["last_foreign_ts_utc"] is None
    assert sum("timeline discontinuity" in line for line in lines) == 1


def test_a_micro_correction_inside_the_tolerance_keeps_the_span(tmp_path):
    """dantesync's micro-corrections (a few ms) stay inside one frame + 20 ms."""
    stereo, sr = _fixture("base-R-rec")
    blocks = _blocks(stereo[: 10 * sr], sr, jumps={at_s(6, sr): 150_000})  # +15 ms
    payloads, _lines = _run(blocks, tmp_path, FixedChain())
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 4


@pytest.mark.parametrize("undefined", [INT64_MAX, 0])
def test_an_undefined_timestamp_falls_back_to_the_arrival_gap(tmp_path, undefined):
    stereo, sr = _fixture("base-R-rec")
    blocks = [b._replace(timestamp=undefined) for b in _blocks(stereo[: 10 * sr], sr)]
    payloads, lines = _run(blocks, tmp_path, FixedChain(), arrival_gaps={at_s(6, sr): pas.RECEIVE_GAP_S + 0.5})
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "UNKNOWN",
                                   "MEASUREMENT"]
    assert any("receive gap of 1.5 s" in line for line in lines), lines
    payloads, _lines = _run(blocks, tmp_path, FixedChain(), arrival_gaps={at_s(6, sr): pas.RECEIVE_GAP_S - 0.5})
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 4


def test_an_error_frame_still_restarts_the_span_whatever_the_timestamps(tmp_path):
    """An NDI error frame (connection lost) keeps restarting the span: audio after a reconnect never
    continues the span, even when its timestamp would."""
    stereo, sr = _fixture("base-R-rec")
    blocks = _blocks(stereo[: 10 * sr], sr)
    payloads = []
    state = {"i": 0, "errored": False}
    clock = _Clock()

    class Rx:
        def capture(self, _timeout_ms):
            i = state["i"]
            if i == at_s(6, sr) and not state["errored"]:
                state["errored"] = True
                raise ConnectionError("NDI receive error (connection lost)")
            if i >= len(blocks):
                return None
            clock.t += blocks[i].samples.shape[0] / sr
            state["i"] = i + 1
            return blocks[i]

        def connections(self):
            return 1

    pas.run(Rx(), str(tmp_path), source="S", decoder=FixedChain(), mono=clock.mono,
            max_loops=len(blocks) + 1, on_write=payloads.append, log=lambda m: None,
            sleep=lambda s: None)
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "UNKNOWN",
                                   "MEASUREMENT"]


# ---------------------------------------------------------------------------------------------
# ROZHODNUTÉ 6027706292 item 1: a spectral FOREIGN is reported during the warm-up too
# ---------------------------------------------------------------------------------------------


def _pink_stereo(seconds, dbfs, sr=SR, seed=1404):
    rng = np.random.default_rng(seed)
    n = int(seconds * sr)
    s = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1.0 / sr)
    s[1:] /= np.sqrt(f[1:])
    s[0] = 0
    x = np.fft.irfft(s, n)
    x *= 10 ** (dbfs / 20.0) / np.sqrt(np.mean(x * x))
    return np.ascontiguousarray(np.stack([x, x], axis=1), dtype=np.float32)


def test_broadband_music_in_the_warm_up_is_foreign_and_latches(tmp_path):
    """Music from the first window on: the warm-up window already reads FOREIGN and starts the
    FOREIGN latch; it never waits for the marker span."""
    payloads, _lines = _run(_blocks(_pink_stereo(6.0, -20.0), SR), tmp_path, NoMarkers())
    assert _verdicts(payloads) == ["UNKNOWN", "FOREIGN", "FOREIGN", "FOREIGN"]
    assert payloads[1]["marker_chain"] is None and "reason" not in payloads[1]
    assert payloads[1]["last_foreign_ts_utc"] is not None


def test_a_music_burst_right_after_a_restart_is_foreign_not_hidden(tmp_path):
    """The review probe of 6027557132 (Y3): measurement, then 1.5 s with no audio at all (the sender
    timeline moves on 3 s), 2 s of broadband music, then measurement again. The span restarts, so the
    music window sits inside the warm-up; it reads FOREIGN and latches."""
    stereo, sr = _fixture("base-R-rec")
    music = _pink_stereo(2.0, -20.0, sr)
    audio = np.concatenate([stereo[: 6 * sr], music, stereo[6 * sr: 10 * sr]])
    blocks = _blocks(audio, sr, jumps={at_s(6, sr): 30_000_000})
    payloads, _lines = _run(blocks, tmp_path, FixedChain(), arrival_gaps={at_s(6, sr): 1.5})
    verdicts = _verdicts(payloads)
    assert verdicts[:5] == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "FOREIGN"]
    assert payloads[-1]["last_foreign_ts_utc"] is not None


def test_measurement_audio_in_the_warm_up_stays_unknown(tmp_path):
    """Only MEASUREMENT needs the marker chain: in-band audio in the warm-up stays UNKNOWN."""
    stereo, sr = _fixture("s2-R-rec")
    payloads, _lines = _run(_blocks(stereo[: 6 * sr], sr), tmp_path, FixedChain())
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT"]
    assert "warming up" in payloads[1]["reason"]


def test_spectral_foreign_is_single_sourced_with_classify():
    assert pa.spectral_foreign(-20.0, pa.FOREIGN_OUTSIDE_BAND_PCT)
    assert not pa.spectral_foreign(-20.0, pa.FOREIGN_OUTSIDE_BAND_PCT - 0.1)
    assert not pa.spectral_foreign(pa.SILENT_RMS_DBFS - 0.1, 90.0)   # silent first
    assert not pa.spectral_foreign(None, 90.0) and not pa.spectral_foreign(-20.0, None)
    assert not pa.spectral_foreign(float("nan"), 90.0)
    for rms, outside in ((-20.0, 90.0), (-20.0, 10.0), (-70.0, 90.0), (None, 90.0)):
        assert (pa.classify(rms, outside, None) == "FOREIGN") == pa.spectral_foreign(rms, outside)


# ---------------------------------------------------------------------------------------------
# the calibration CLI runs on the same timeline path
# ---------------------------------------------------------------------------------------------


def test_the_calibration_feeds_a_continuous_sender_timeline():
    """The calibration bars are computed through the real sampler loop; its receiver now carries the
    sender timeline too, so the bars exercise the timestamp path the dev1 service runs."""
    stereo, sr = _fixture("s2-R-rec")
    rx = cal._FileReceiver(stereo, sr)
    blocks = [rx.capture(0) for _ in range(len(rx))]
    assert all(pa.timestamp_defined(b.timestamp) for b in blocks)
    for a, b in zip(blocks, blocks[1:]):
        tol = pa.continuity_tolerance_100ns(a.samples.shape[0], a.sample_rate)
        assert pa.frame_continues(a.timestamp, a.samples.shape[0], a.sample_rate, b.timestamp, tol) == pa.CONTINUE
        assert abs(pa.timeline_offset_100ns(a.timestamp, a.samples.shape[0], a.sample_rate, b.timestamp)) <= 1
