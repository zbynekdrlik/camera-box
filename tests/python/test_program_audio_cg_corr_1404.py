"""issue 1404 -- the CG arm refinement: a CG source is FOREIGN only when its audio is IN the stream
program (scripts/program_audio_cg_corr.py, wired through program_audio_cg.overlay).

The 3.11.2026 incident: a CG song reached the stream program 15-20 dB under the cam2 QPSK marker.
The fixtures here rebuild that shape synthetically (a marker-like QPSK signal inside 200-800 Hz,
a room floor, and a music-like signal 18 dB under the marker, 600 ms late) and check it reads
PRESENT, while the same CG signal ABSENT from the program reads ABSENT. Everything is hermetic:
numpy only, the committed real measurement clip, no network.
"""
from __future__ import annotations

import pathlib
import sys
import time

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio as pa  # noqa: E402
import program_audio_cg as pcg  # noqa: E402
import program_audio_cg_corr as pcc  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402

SR = 48000
FRAME = 1024
TS0 = 17_937_210_950_013_596   # a real sender stamp read live on 3.11.2026 (100 ns)
UNITS = 10_000_000
SECS = 10.0
REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
SP = "RESOLUME-SNV (SP-program)"
CGOBS = "RESOLUME-SNV (cg-obs)"


def _bandpass(x, lo, hi, sr=SR):
    spec = np.fft.rfft(x)
    f = np.fft.rfftfreq(x.shape[0], 1.0 / sr)
    spec[(f < lo) | (f > hi)] = 0.0
    return np.fft.irfft(spec, x.shape[0])


def _power(x):
    return float(np.mean(np.asarray(x, dtype=np.float64) ** 2))


def _scale(x, power):
    return x * np.sqrt(power / _power(x))


def marker_like(seed=1, secs=SECS, sr=SR):
    """A QPSK burst signal on a 442 Hz carrier, 110 baud, band-limited to 200-800 Hz (the cam2
    marker's band), at -16 dBFS: the loud part of the measurement program."""
    rng = np.random.default_rng(seed)
    n = int(secs * sr)
    baud = 110
    sym = rng.integers(0, 4, int(secs * baud) + 2)
    phase = np.repeat(sym * (np.pi / 2), sr // baud)[:n]
    t = np.arange(n) / sr
    x = np.cos(2 * np.pi * 442.0 * t + phase)
    return _scale(_bandpass(x, 200.0, 800.0), 10 ** (-16 / 10))


def music_like(seed=2, secs=SECS, sr=SR):
    """Chords that change every 0.5 s (harmonics to 4 kHz, decaying), a bass line and percussive
    noise bursts every 0.25 s: a broadband, music-shaped signal (unit power)."""
    rng = np.random.default_rng(seed)
    n = int(secs * sr)
    t = np.arange(n) / sr
    x = np.zeros(n)
    seg = int(0.5 * sr)
    for start in range(0, n, seg):
        idx = slice(start, min(n, start + seg))
        tt = t[idx]
        env = np.exp(-(tt - tt[0]) * 2.0)
        for f0 in rng.choice([196.0, 220.0, 247.0, 262.0, 294.0, 330.0, 349.0, 392.0], 3, replace=False):
            for h in range(1, 16):
                if f0 * h > 4000:
                    break
                x[idx] += env * np.sin(2 * np.pi * f0 * h * tt + rng.uniform(0, 6.28)) / h
        x[idx] += 0.8 * env * np.sin(2 * np.pi * rng.choice([55.0, 65.4, 73.4, 82.4]) * tt)
    hit = int(0.25 * sr)
    for start in range(0, n, hit):
        k = min(n - start, int(0.08 * sr))
        x[start:start + k] += 0.6 * rng.standard_normal(k) * np.exp(-np.arange(k) / (0.015 * sr))
    return _scale(x, 1.0)


def room_floor(seed=3, secs=SECS, sr=SR, power=10 ** (-16 / 10) * 0.15):
    """The measurement program's out-of-band floor: noise over 800-8000 Hz at ~15 % of the marker's
    power (live 3.11.2026: 12.4-15.6 % of the program outside the band)."""
    rng = np.random.default_rng(seed)
    return _scale(_bandpass(rng.standard_normal(int(secs * sr)), 800.0, 8000.0), power)


def frames(x, t0=TS0, block=FRAME, sr=SR):
    """NDI-sized frames at their sender stamps, as the rings hold them."""
    x = np.asarray(x, dtype=np.float32)
    if x.ndim == 1:
        x = x[:, None]
    return [(t0 + int(round(i * UNITS / sr)), x[i:i + block], sr)
            for i in range(0, x.shape[0] - block + 1, block)]


def delayed(x, delay_s, sr=SR):
    d = int(round(delay_s * sr))
    out = np.zeros_like(x)
    out[d:] = x[:x.shape[0] - d]
    return out


def program_with(cg_mono, rel_db=-18.0, delay_s=0.6, seed=1):
    """The stereo measurement program (marker + room floor) with `cg_mono` mixed in rel_db under the
    marker, delay_s late (None = not mixed in)."""
    marker = marker_like(seed)
    prog = marker + room_floor(seed + 10)
    if cg_mono is not None:
        prog = prog + delayed(_scale(cg_mono, _power(marker) * 10 ** (rel_db / 10)), delay_s)
    return np.stack([prog, delayed(prog, 0.0102)], axis=1)  # the marker on L and R ~10 ms apart


@pytest.fixture(scope="module")
def rec_clip():
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == SR and x.shape[1] == 2
    return np.ascontiguousarray(x, dtype=np.float32)


# ----------------------------------------------------------------- the 3.11.2026 incident shape

def test_a_song_18_db_under_the_marker_600_ms_late_reads_present():
    cg = music_like()
    c = pcc.judge(frames(program_with(cg)), frames(np.stack([cg, cg], axis=1) * 0.3))
    assert c.kind == pcc.PRESENT, c
    assert c.ncc >= 1.5 * pcc.DETECT_NCC
    assert c.lag_ms == pytest.approx(600.0, abs=1.0)


def test_the_same_cg_signal_absent_from_the_program_reads_absent():
    cg = music_like()
    c = pcc.judge(frames(program_with(None)), frames(cg * 0.3))
    assert c.kind == pcc.ABSENT, c
    assert c.ncc < pcc.DETECT_NCC / 2


def test_other_music_in_the_program_is_not_this_cg_source():
    c = pcc.judge(frames(program_with(music_like(seed=7))), frames(music_like(seed=2) * 0.3))
    assert c.kind == pcc.ABSENT, c


@pytest.mark.parametrize("seed", [6, 7, 8])
def test_a_cg_marker_of_its_own_never_correlates_with_the_cam2_marker(seed):
    """The measurement clip played on the CG box carries a QPSK marker of its own timing; the
    program's loud cam2 marker must not correlate with it (the band is notched and the program's
    segment edges never leak: with 50 ms tapers on both sides this read PRESENT at 0.40-0.43)."""
    prog = program_with(None, seed=5)
    c = pcc.judge(frames(prog), frames(marker_like(seed=seed)))
    assert c.kind == pcc.ABSENT, c
    assert c.ncc < pcc.DETECT_NCC * 0.75


@pytest.mark.parametrize("delay_s", [-0.95, -0.45, 0.0, 0.3, 1.0, 1.45, 2.5, 2.95])
def test_every_lag_of_the_declared_range_is_found(delay_s):
    cg = music_like(seed=4)
    if delay_s >= 0:
        prog = program_with(cg, delay_s=delay_s)
        c = pcc.judge(frames(prog), frames(cg))
    else:  # the CG's NDI copy stamped after the program carries it
        prog = program_with(cg, delay_s=0.0)
        c = pcc.judge(frames(prog), frames(cg, t0=TS0 - int(delay_s * UNITS)))
    assert c.kind == pcc.PRESENT, (delay_s, c)
    assert c.lag_ms == pytest.approx(delay_s * 1e3, abs=1.0)


def test_a_lag_outside_the_range_is_not_searched():
    cg = music_like(seed=4)
    prog = program_with(cg, delay_s=3.5)  # longer than LAG_MAX_MS: not the declared path
    assert pcc.judge(frames(prog), frames(cg)).kind == pcc.ABSENT


def test_the_lag_range_is_the_documented_path():
    assert pcc.LAG_MIN_MS == -1000.0 and pcc.LAG_MAX_MS == 3000.0   # review round 1: widened
    assert pcc.NOTCH_LO_HZ == pa.BAND_LO_HZ and pcc.NOTCH_HI_HZ == pa.BAND_HI_HZ
    assert pcc.PASS_LO_HZ >= pcc.NOTCH_HI_HZ   # nothing of the marker band is ever correlated


# ----------------------------------------------------------------- the real measurement program

def test_the_real_measurement_clip_with_the_song_under_it_reads_present(rec_clip):
    cg = music_like(seed=9, secs=rec_clip.shape[0] / SR)
    mono = _scale(cg, _power(rec_clip) * 10 ** (-18 / 10))
    prog = rec_clip + delayed(mono, 0.6)[:, None]
    c = pcc.judge(frames(prog), frames(cg))
    assert c.kind == pcc.PRESENT and c.lag_ms == pytest.approx(600.0, abs=1.0), c


def test_the_null_on_the_real_clip_stays_far_under_the_threshold(rec_clip):
    """The calibration's null, hermetic: music-like CG sources at many offsets against the real
    measurement clip (the live null of 3.11.2026 peaked at 0.024-0.038, the rule file)."""
    peaks = []
    for seed in range(6):
        cg = music_like(seed=20 + seed, secs=rec_clip.shape[0] / SR)
        c = pcc.judge(frames(rec_clip), frames(cg, t0=TS0 + seed * 3_000_000))
        assert c.kind == pcc.ABSENT, c
        peaks.append(c.ncc)
    assert max(peaks) < pcc.DETECT_NCC * 0.75, peaks


# ----------------------------------------------------------------- not computable = strict arm

def test_too_little_cg_audio_buffered_is_undecided():
    cg = music_like(secs=3.0)
    c = pcc.judge(frames(program_with(None)), frames(cg, t0=TS0 + int(6.5 * UNITS)))
    assert c.kind == pcc.UNDECIDED and "too little audio buffered" in c.why


def test_a_cg_that_lags_the_program_by_seconds_is_undecided():
    cg = music_like()
    c = pcc.judge(frames(program_with(None)), frames(cg[: int(4 * SR)]))
    assert c.kind == pcc.UNDECIDED and "behind the program" in c.why


# E2E 41703531109, 3.11.2026 19:49:39: an idle SongPlayer at -52 dBFS (above the -60 silence bar) read
# 'not proven absent' under the old -50 floor and the strict FOREIGN stopped the YouTube stage. The CG
# is a digital NDI source and the NCC is level-independent, so a quiet CG is decided like a loud one.
@pytest.mark.parametrize("cg_dbfs", [-52.0, -59.0])
def test_a_quiet_cg_above_the_silence_bar_is_decided_both_ways(cg_dbfs):
    cg = _scale(music_like(), 10 ** (cg_dbfs / 10))
    assert pcc.judge(frames(program_with(None)), frames(cg)).kind == pcc.ABSENT
    c = pcc.judge(frames(program_with(cg)), frames(cg))
    assert c.kind == pcc.PRESENT and c.lag_ms == pytest.approx(600.0, abs=1.0), c


def test_an_effectively_empty_cg_segment_never_decides():
    cg = _scale(music_like(), 10 ** (-85 / 10))
    c = pcc.judge(frames(program_with(None)), frames(cg))
    assert c.kind == pcc.UNDECIDED and "dBFS <" in c.why


def test_a_gap_in_the_cg_audio_is_undecided():
    cg = music_like()
    fr = frames(cg)
    del fr[len(fr) - 140:len(fr) - 100]   # ~0.85 s missing inside the searched stretch
    assert pcc.judge(frames(program_with(None)), fr).kind == pcc.UNDECIDED


def test_a_digitally_silent_program_is_undecided():
    c = pcc.judge(frames(np.zeros((int(SECS * SR), 2))), frames(music_like()))
    assert c.kind == pcc.UNDECIDED and "silent" in c.why


def test_no_program_or_no_cg_is_undecided():
    assert pcc.judge([], frames(music_like())).kind == pcc.UNDECIDED
    assert pcc.judge(frames(program_with(None)), []).kind == pcc.UNDECIDED


def test_a_frame_without_a_sender_stamp_is_undecided():
    ring = pcc.AudioRing(mono=True)
    for ts, x, sr in frames(music_like()):
        ring.push(ts, x, sr)
    ring.push(pa.NDI_TIMESTAMP_UNDEFINED, np.zeros((FRAME, 2), np.float32), SR)
    fr, bad = ring.snapshot()
    c = pcc.judge(frames(program_with(None)), fr, cg_bad_stamp=bad)
    assert bad and c.kind == pcc.UNDECIDED and "timestamp" in c.why


def test_a_non_finite_sample_is_undecided():
    cg = music_like()
    cg[int(8 * SR)] = np.nan
    assert pcc.judge(frames(program_with(None)), frames(cg)).kind == pcc.UNDECIDED


# ----------------------------------------------------------------- the rings

def test_the_ring_is_bounded_by_its_stamps_and_keeps_mono_for_the_cg():
    ring = pcc.AudioRing(seconds=3.0, mono=True)
    for ts, x, sr in frames(np.stack([music_like(), music_like(seed=3)], axis=1)):
        ring.push(ts, x, sr)
    fr, bad = ring.snapshot()
    first, end = pcc.span(fr)
    assert not bad and fr[0][1].shape[1] == 1
    assert 3.0 <= (end - first) / UNITS <= 3.0 + 2 * FRAME / SR


def test_the_ring_holds_the_whole_lag_search():
    assert pcc.RING_S >= pcc.PROGRAM_SEGMENT_S + (pcc.LAG_MAX_MS - pcc.LAG_MIN_MS) / 1e3 + pcc.MAX_SEGMENT_AGE_S


def test_one_judge_costs_well_under_a_window():
    cg = music_like()
    prog, cgf = frames(program_with(cg)), frames(cg)
    pcc.judge(prog, cgf)
    t = time.perf_counter()
    for _ in range(3):
        pcc.judge(prog, cgf)
    assert (time.perf_counter() - t) / 3 < 0.5   # measured ~20-30 ms on dev1; the window is 2 s


# ----------------------------------------------------------------- the overlay

def _reading(source, level, corr=None):
    return pcg.CgReading(source, level, 0.0, 1, None, corr)


def _st(*readings):
    return pcg.CgState("resolume-snv", True, tuple(readings))


def test_a_cg_measured_absent_keeps_the_window_and_reads_not_in_program():
    absent = pcc.Correlation(pcc.ABSENT, 0.031, 212.5, 0)
    st = _st(_reading(SP, -40.0, absent), _reading(CGOBS, -12.0, absent))
    assert pcg.overlay("MEASUREMENT", None, st) == ("MEASUREMENT", None)
    block = pcg.payload_block(st)
    assert block["status"] == pcg.STATUS_NOT_IN_PROGRAM
    assert block["sources"][1]["in_program"] == pcc.ABSENT and block["sources"][1]["ncc"] == 0.031
    assert "in_program_ncc_min" not in block and "in_program" not in pcg.payload_block(_st(_reading(SP, -70.0)))["sources"][0]


def test_a_cg_measured_present_is_foreign_with_its_ncc_and_lag():
    present = pcc.Correlation(pcc.PRESENT, 0.214, 601.0, 1)
    v, r = pcg.overlay("MEASUREMENT", None, _st(_reading(CGOBS, -12.0, present)))
    assert v == "FOREIGN" and r.startswith(pcg.REASON_CG_AUDIO)
    assert "is IN the stream program (ncc 0.214" in r and "+601 ms" in r


def test_an_undecided_correlation_keeps_the_strict_arm_and_names_why():
    und = pcc.Correlation(pcc.UNDECIDED, why="no CG audio buffered")
    v, r = pcg.overlay("MEASUREMENT", None, _st(_reading(SP, -35.0, und)))
    assert v == "FOREIGN" and "not proven absent from the stream program (no CG audio buffered)" in r
    assert pcg.payload_block(_st(_reading(SP, -35.0, und)))["status"] == pcg.REASON_CG_AUDIO


def test_one_present_source_is_foreign_although_the_other_is_absent():
    st = _st(_reading(SP, -40.0, pcc.Correlation(pcc.ABSENT, 0.02, 0.0, 0)),
             _reading(CGOBS, -12.0, pcc.Correlation(pcc.PRESENT, 0.3, 600.0, 0)))
    v, r = pcg.overlay("MEASUREMENT", None, st)
    assert v == "FOREIGN" and CGOBS in r and SP not in r


def test_an_absent_cg_does_not_hide_a_deaf_source_while_home():
    st = _st(_reading(CGOBS, -12.0, pcc.Correlation(pcc.ABSENT, 0.02, 0.0, 0)),
             pcg.CgReading(SP, None, None, 0, None))
    v, r = pcg.overlay("MEASUREMENT", None, st)
    assert v == "UNKNOWN" and SP in r


# ----------------------------------------------------------------- the arm computes it

class _Rx:
    def __init__(self, blocks):
        self.blocks = list(blocks)

    def capture(self, _timeout_ms):
        return self.blocks.pop(0) if self.blocks else None

    def connections(self):
        return 1


class _Block:
    def __init__(self, ts, samples, sr=SR):
        self.timestamp, self.samples, self.sample_rate = ts, samples, sr


class _Probe:
    home = True


def _monitor(source, x, t0=TS0):
    clock = {"t": 100.0}
    blocks = [_Block(ts, s, sr) for ts, s, sr in frames(x, t0)]
    m = pcg.CgSourceMonitor(_Rx(blocks), source, mono=lambda: clock["t"], sleep=lambda _s: None)
    for _ in blocks:
        m.poll_once()
    return m


def _program_ring(prog):
    ring = pcc.AudioRing(mono=False)
    for ts, x, sr in frames(prog):
        ring.push(ts, x, sr)
    return ring


def test_the_arm_correlates_each_heard_source_against_the_program():
    song = music_like()
    cg_in = _monitor(CGOBS, np.stack([song, song], axis=1) * 0.3)
    cg_out = _monitor(SP, np.stack([music_like(seed=8)] * 2, axis=1) * 0.05)
    arm = pcg.CgArm("resolume-snv", [cg_in, cg_out], _Probe())
    st = arm.state(now=100.0, program=_program_ring(program_with(song)))
    by = {r.source: r for r in st.readings}
    assert by[CGOBS].correlation.kind == pcc.PRESENT
    assert by[SP].correlation.kind == pcc.ABSENT
    v, r = pcg.overlay("MEASUREMENT", None, st)
    assert v == "FOREIGN" and CGOBS in r and SP not in r


# E2E 41742028800 (4.11.2026 00:57-01:01): ten isolated windows read PRESENT at |ncc| 0.071-0.083 and
# lags that jumped (+2734, -998, +623 ms) while mbc carried only the marker -- the null, not the song. A
# real CG in the program correlates at ONE lag window after window (the segments overlap). So a PRESENT
# under STRONG_NCC counts only when the previous window found the same lag; a strong one counts at once.
def _c(kind, ncc, lag):
    return pcc.Correlation(kind, ncc, lag, 0)


@pytest.mark.parametrize("ncc,lag", [(0.083, 2734.0), (0.071, -998.0), (0.072, 623.0)])
def test_an_isolated_weak_present_window_is_not_foreign(ncc, lag):
    c = pcc.confirm(None, _c(pcc.PRESENT, ncc, lag))
    assert c.kind == pcc.ABSENT and "unconfirmed" in (c.why or ""), c


def test_a_weak_present_at_the_same_lag_twice_is_present():
    first = pcc.confirm(None, _c(pcc.PRESENT, 0.072, 600.0))
    assert first.kind == pcc.ABSENT
    second = pcc.confirm(_c(pcc.PRESENT, 0.072, 600.0), _c(pcc.PRESENT, 0.075, 601.5))
    assert second.kind == pcc.PRESENT and second.lag_ms == 601.5


def test_a_weak_present_at_another_lag_stays_unconfirmed():
    c = pcc.confirm(_c(pcc.PRESENT, 0.08, 600.0), _c(pcc.PRESENT, 0.08, 2734.0))
    assert c.kind == pcc.ABSENT


def test_a_strong_present_counts_at_once_and_absent_or_undecided_pass_through():
    assert pcc.confirm(None, _c(pcc.PRESENT, pcc.STRONG_NCC, 600.0)).kind == pcc.PRESENT
    assert pcc.confirm(None, _c(pcc.PRESENT, -0.2, 600.0)).kind == pcc.PRESENT
    a = _c(pcc.ABSENT, 0.03, 10.0)
    assert pcc.confirm(_c(pcc.PRESENT, 0.08, 10.0), a) == a
    u = pcc.Correlation(pcc.UNDECIDED, why="x")
    assert pcc.confirm(None, u) == u


def test_the_arm_confirms_a_song_24_db_under_the_marker_in_the_second_window():
    song = music_like()
    cg = _monitor(CGOBS, np.stack([song, song], axis=1) * 0.3)
    arm = pcg.CgArm("resolume-snv", [cg], _Probe())
    ring = _program_ring(program_with(song, rel_db=-24.0))
    assert arm.state(now=100.0, program=ring).readings[0].correlation.kind == pcc.ABSENT
    st = arm.state(now=102.0, program=ring)
    assert st.readings[0].correlation.kind == pcc.PRESENT
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"


def test_the_arm_without_a_program_ring_is_the_strict_rule():
    m = _monitor(CGOBS, np.stack([music_like()] * 2, axis=1) * 0.3)
    st = pcg.CgArm("resolume-snv", [m], _Probe()).state(now=100.0)
    assert st.readings[0].correlation is None
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"


def test_a_silent_cg_source_is_never_correlated():
    m = _monitor(SP, np.zeros((int(SECS * SR), 2), np.float32))
    st = pcg.CgArm("resolume-snv", [m], _Probe()).state(now=100.0, program=_program_ring(program_with(None)))
    assert st.readings[0].correlation is None
    assert pcg.overlay("MEASUREMENT", None, st) == ("MEASUREMENT", None)


# ----------------------------------------------------------------- the sampler loop feeds the ring

def test_the_loop_hands_the_cg_arm_the_programs_own_frames_at_their_stamps(tmp_path):
    from test_program_audio_cg_1404 import _FakeArm, _run, _state

    class _RecArm(_FakeArm):
        def __init__(self, st):
            super().__init__(st)
            self.programs = []

        def state(self, now=None, program=None):
            self.programs.append(program)
            return self.st

    arm = _RecArm(_state())
    seen = _run(tmp_path, arm, seconds=12.0)
    assert len(arm.programs) == len(seen) - 1 and all(isinstance(r, pcc.AudioRing) for r in arm.programs)
    fr, bad = arm.programs[-1].snapshot()
    first, end = pcc.span(fr)
    assert not bad and fr[0][1].shape[1] == 2 and fr[0][2] == 48000
    assert pcc.RING_S - 0.1 <= (end - first) / UNITS <= pcc.RING_S + 0.1  # bounded by the stamps
    assert end == 10_000_000_000 + int(round(12.0 * UNITS))                # the program's sender timeline


# ----------------------------------------------------------------- review round 1

@pytest.mark.parametrize("delay_s", [0.6, 2.2])
def test_a_polarity_inverted_cg_in_the_program_reads_present(delay_s):
    """A console channel with its polarity flipped, a miswired balanced cable, an inverting hop:
    the song reaches the program with the sign flipped and must still read PRESENT (the peak is
    taken by magnitude, its sign reported)."""
    cg = music_like(seed=4)
    c = pcc.judge(frames(program_with(-cg, delay_s=delay_s)), frames(cg))
    assert c.kind == pcc.PRESENT, c
    assert c.ncc <= -pcc.DETECT_NCC and c.lag_ms == pytest.approx(delay_s * 1e3, abs=1.0)


def test_the_null_by_magnitude_stays_under_the_threshold_too(rec_clip):
    peaks = []
    for seed in range(6):
        c = pcc.judge(frames(rec_clip), frames(music_like(seed=40 + seed, secs=rec_clip.shape[0] / SR)))
        assert c.kind == pcc.ABSENT, c
        peaks.append(abs(c.ncc))
    assert max(peaks) < pcc.DETECT_NCC * 0.75, peaks


def test_a_frozen_sender_stamp_never_grows_the_ring_past_its_sample_cap():
    ring = pcc.AudioRing(mono=True)
    block = np.zeros((FRAME, 1), np.float32)
    for _ in range(5000):
        ring.push(TS0, block, SR)          # the same stamp forever: the stamp bound never pops
    fr, _bad = ring.snapshot()
    assert sum(x.shape[0] for _ts, x, _sr in fr) <= getattr(pcc, 'RING_MAX_SAMPLES', 0)
    assert getattr(pcc, 'RING_MAX_SAMPLES', 0) >= int(pcc.RING_S * 192000)  # never cuts a healthy 192 kHz ring short


def test_a_correlation_that_raises_is_undecided_never_a_crash(monkeypatch):
    m = _monitor(CGOBS, np.stack([music_like()] * 2, axis=1) * 0.3)

    def boom(*_a, **_k):
        raise MemoryError("numpy")

    monkeypatch.setattr(pcc, "judge", boom)
    st = pcg.CgArm("resolume-snv", [m], _Probe()).state(now=100.0, program=_program_ring(program_with(None)))
    c = st.readings[0].correlation
    assert c.kind == pcc.UNDECIDED and "correlation failed" in c.why and "MemoryError" in c.why
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"     # fail closed


def test_a_ring_push_that_raises_keeps_the_capture_thread_alive(monkeypatch):
    m = _monitor(CGOBS, np.zeros((FRAME * 2, 2), np.float32))

    def bad_push(*_a, **_k):
        raise ValueError("odd shape")

    monkeypatch.setattr(m.ring, "push", bad_push)
    m.receiver.blocks = [_Block(TS0, np.zeros((FRAME, 2), np.float32))]
    m.poll_once()                                                    # must not raise
    r = m.reading(100.0)
    assert r.error and "odd shape" in r.error


def test_the_correlation_never_calls_blas(monkeypatch):
    """np.dot goes through OpenBLAS, whose worker threads spun to 15x the judge's CPU under load on
    dev1; the sampler runs on a production box."""
    def no_blas(*_a, **_k):
        raise AssertionError("a BLAS call in the CG correlation")

    for name in ("dot", "matmul", "inner", "vdot", "einsum"):
        monkeypatch.setattr(np, name, no_blas)
    cg = music_like()
    assert pcc.judge(frames(program_with(cg)), frames(cg)).kind == pcc.PRESENT


def test_a_ring_push_that_raises_keeps_the_loud_level_and_reads_foreign_also_away(monkeypatch):
    """Review round 2: a failed ring write dropped the block's level too, so while the CG box was
    AWAY (the not-receivable rule not graded) a loud CG read silent and the window passed. The
    level is kept; the source's correlation is UNDECIDED; the window is FOREIGN."""
    m = _monitor(CGOBS, np.zeros((int(SECS * SR), 2), np.float32))   # silent until the pushes fail

    def bad_push(*_a, **_k):
        raise ValueError("odd shape")

    monkeypatch.setattr(m.ring, "push", bad_push)
    loud = (np.stack([music_like(seed=9)] * 2, axis=1) * 0.3).astype(np.float32)
    m.receiver.blocks = [_Block(TS0 + 10 * UNITS + i * 213_333, loud[i * FRAME:(i + 1) * FRAME])
                         for i in range(20)]
    for _ in range(20):
        m.poll_once()

    class _Away:
        home = False

    st = pcg.CgArm("resolume-snv", [m], _Away()).state(now=100.0, program=_program_ring(program_with(None)))
    r = st.readings[0]
    assert pa.is_number(r.level_dbfs) and r.level_dbfs > -30.0
    assert r.correlation.kind == pcc.UNDECIDED and "odd shape" in r.correlation.why
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"
