"""issue 1404 -- the program-audio sampler bridges a small hole in the sender's audio timeline.

Live (7.10.2026, dev1 journal 11:30-12:35): 55 `audio timeline discontinuity` lines, every one a
POSITIVE offset, 42 of them +41.4...+52.6 ms (two NDI frames of 1024 samples = 42.7 ms, plus send
jitter) with an arrival gap of 0.1 s; one 10-min summary read `timeline_breaks=33 UNKNOWN=22`. Samples
were missing between two received frames, and every hole restarted the marker span (a 4 s warm-up =
UNKNOWN), which stopped restreamer's YouTube gate (dev CI 37602415434).

The design (issue 1404 comment 6036098516, Approach 1): a frame AHEAD of the timeline by
tolerance < offset <= HOLE_BRIDGE_MAX_MS (250 ms) is a BRIDGE carrying round(offset * sr) missing
samples; the sampler inserts that many zeros before the frame and keeps the span. A frame behind the
timeline beyond the tolerance, a hole over 250 ms and an undefined timestamp restart as before.

The safety checks restreamer asked for (coordinator, 7.10.2026): a bridged hole never turns FOREIGN
into MEASUREMENT and never delays the FOREIGN latch; a window that is mostly bridged zeros + music
never reads MEASUREMENT; the chain is decoded over REAL samples only (the zeros are never handed to
the decoder, so they cannot add a word); the spectral share stays a ratio of the real signal.
"""
from __future__ import annotations

import pathlib
import sys

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
import program_audio_sampler as pas  # noqa: E402
from qpsk_guard_shim_1404 import FixedChain, NoMarkers, build_shim, real_marker_words  # noqa: E402
from test_program_audio_1404 import _pink, _scale_to_dbfs, _speech_shaped  # noqa: E402
from test_program_audio_timeline_1404 import _Block, _run, _verdicts  # noqa: E402

REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
SR = 48000
FRAME = 1024                   # an OBS audio tick, the size NDI delivers live
FRAME_100NS = FRAME * 10_000_000 / SR
TS0 = 17_913_444_397_535_105   # a real sender timestamp read live on 7.10.2026 (100 ns units)
NDI_UNITS = 10_000_000

# The journal's 55 discontinuities of 7.10.2026 11:30-12:35, in order: (offset ms, arrival gap s).
LIVE_OFFSETS_7_10 = (
    (571.7, 0.6), (543.0, 0.8), (149.9, 0.0), (41.8, 0.1), (256.3, 1.6), (44.7, 0.1), (41.4, 0.1),
    (41.5, 0.1), (44.8, 0.1), (42.0, 0.1), (47.2, 0.1), (47.6, 0.1), (42.4, 0.0), (44.8, 0.1),
    (43.8, 0.1), (43.7, 0.1), (41.7, 0.1), (556.2, 0.4), (48.5, 0.1), (41.9, 0.1), (42.4, 0.1),
    (43.5, 0.1), (52.0, 0.1), (41.8, 0.1), (52.2, 0.1), (48.4, 0.1), (43.1, 0.1), (42.8, 0.1),
    (42.2, 0.1), (42.8, 0.1), (50.1, 0.1), (46.5, 0.1), (43.8, 0.1), (41.6, 0.1), (41.5, 0.1),
    (50.4, 0.1), (48.1, 0.1), (46.1, 0.1), (41.4, 0.1), (45.6, 0.1), (41.8, 0.1), (44.8, 0.1),
    (42.9, 0.1), (42.1, 0.1), (46.8, 0.1), (41.7, 0.1), (47.7, 0.1), (42.0, 0.1), (48.0, 0.1),
    (234.3, 1.6), (357.6, 0.4), (162.0, 0.1), (337.8, 0.2), (52.6, 1.4), (112.5, 1.5),
)


@pytest.fixture(scope="session")
def decoder(tmp_path_factory):
    return pam.MarkerDecoder(build_shim(tmp_path_factory.mktemp("qpsk-guard-shim-bridge")))


@pytest.fixture(scope="session")
def rec_clip():
    """The committed 48 kHz stereo measurement clip (rec3b at 290 s, the hardest recorded span)."""
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == SR and x.shape[1] == 2
    return np.ascontiguousarray(x, dtype=np.float32)


def _tol():
    return pa.continuity_tolerance_100ns(FRAME, SR)


def _ts_ahead(off_ms):
    """The stamp of a frame that sits `off_ms` ahead of where the timeline puts it."""
    return TS0 + round(FRAME_100NS + off_ms * 10_000)


def _frames(audio, drop=(), jitter=None, jumps=None, gaps=None):
    """NDI frames of FRAME samples stamped at their place on the sender timeline (TS0 + offset).
    `drop`: frame indices never delivered (a real hole); `jitter[k]`: 100 ns added to frame k's stamp
    only (send jitter); `jumps[k]`: 100 ns added to frame k's stamp and every later one (a timestamp
    step, nothing lost). Returns (blocks, arrival_gaps keyed by the index in the returned list)."""
    jitter, jumps, gaps = jitter or {}, jumps or {}, gaps or {}
    blocks, arrival, shift = [], {}, 0
    for k, i in enumerate(range(0, audio.shape[0], FRAME)):
        shift += jumps.get(k, 0)
        if k in drop:
            continue
        if k in gaps:
            arrival[len(blocks)] = gaps[k]
        ts = TS0 + (i * NDI_UNITS) // SR + shift + jitter.get(k, 0)
        blocks.append(_Block(SR, audio[i:i + FRAME], ts))
    return blocks, arrival


def _frame_at(seconds):
    return int(round(seconds * SR)) // FRAME


def _judged(payloads):
    return [p for p in payloads if p["marker_chain"] is not None]


SENTINEL = np.float32(1.2345678e-5)
MARKER_PERIOD_S = 0.5


def _with_timeline_markers(audio):
    """A copy of `audio` with a marker the _TimelineMarkers fake reads written into channel 0 every
    MARKER_PERIOD_S of the SENDER timeline: SENTINEL, then (index + 1) * 1e-6 with the emitter's
    index round(60 * t) mod 256. About -100 dBFS: the spectrum and the level never see it."""
    y = np.array(audio, dtype=np.float32, copy=True)
    for p in range(int(0.1 * SR), y.shape[0] - 1, int(MARKER_PERIOD_S * SR)):
        y[p, 0] = SENTINEL
        y[p + 1, 0] = np.float32((int(round(60 * p / SR)) % 256 + 1) * 1e-6)
    assert np.count_nonzero(y[:, 0] == SENTINEL) == len(range(int(0.1 * SR), y.shape[0] - 1, int(MARKER_PERIOD_S * SR)))
    return y


class _TimelineMarkers:
    """A decoder fake for loop tests whose subject is the continuity, not the decode: it finds the
    markers _with_timeline_markers wrote, at the positions the sampler hands them over -- so, like
    the real decoder, a marker shifted off its sender-timeline place falls off the chain's line,
    and a marker inside a missing stretch is gone. (FixedChain finds the same 8 words in every
    buffer it gets, so it cannot follow the per-stretch decode of a span with a bridged hole.)"""

    def decode(self, samples, sample_rate):
        ch0 = np.asarray(samples)[:, 0]
        pos = np.flatnonzero(ch0[:-1] == SENTINEL)
        words = [(p / sample_rate, int(round(float(ch0[p + 1]) / 1e-6)) - 1) for p in pos]
        return [words] + [[] for _ in range(samples.shape[1] - 1)]


# ---------------------------------------------------------------------------------------------
# the pure decision
# ---------------------------------------------------------------------------------------------


def test_the_bridge_limit_is_single_sourced():
    assert pa.HOLE_BRIDGE_MAX_MS == 250.0
    assert pa.hole_bridge_max_100ns() == pytest.approx(2_500_000)
    assert pa.BRIDGE == "BRIDGE"


@pytest.mark.parametrize("off_ms, missing", [
    (2 * FRAME * 1e3 / SR, 2048),   # exactly two frames: the live hole
    (41.4, 1987),                   # the smallest live offset, just over the 41.3 ms tolerance
    (52.2, 2506),                   # the largest live 2-frame offset
    (149.9, 7195),
    (250.0, 12000),                 # the limit is inclusive
])
def test_a_hole_ahead_up_to_250_ms_is_a_bridge_carrying_its_sample_count(off_ms, missing):
    got = pa.frame_continues(TS0, FRAME, SR, _ts_ahead(off_ms), _tol())
    assert got == pa.Continuity(pa.BRIDGE, missing)
    assert got.kind == pa.BRIDGE and got.missing_samples == missing


@pytest.mark.parametrize("off_ms", [250.1, 300.0, 571.7])
def test_a_hole_over_250_ms_still_restarts(off_ms):
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(off_ms), _tol()) == (pa.DISCONTINUITY, 0)


@pytest.mark.parametrize("off_ms", [-41.4, -45.0, -200.0])
def test_a_frame_behind_the_timeline_still_restarts(off_ms):
    """An overlap or a backward jump is never bridged: no sample is missing."""
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(off_ms), _tol()) == (pa.DISCONTINUITY, 0)


@pytest.mark.parametrize("off_ms", [41.0, -41.0, 0.0, 29.5])
def test_inside_the_tolerance_nothing_is_bridged(off_ms):
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(off_ms), _tol()) == (pa.CONTINUE, 0)


def test_the_bridge_starts_right_above_the_tolerance():
    tol = _tol()
    at = TS0 + round(FRAME_100NS + tol)          # rounds onto the tolerance or just inside it
    assert pa.frame_continues(TS0, FRAME, SR, at - 1, tol).kind == pa.CONTINUE
    assert pa.frame_continues(TS0, FRAME, SR, at + 1, tol).kind == pa.BRIDGE


def test_bridge_samples_rounds_and_refuses_nonsense():
    assert pa.bridge_samples(2 * FRAME_100NS, SR) == 2048
    assert pa.bridge_samples(10_417, SR) == 50      # 50.0016 -> 50
    assert pa.bridge_samples(10_521, SR) == 51      # 50.5008 -> 51 (half up)
    for bad in ((0, SR), (-5, SR), (10_000, 0)):
        with pytest.raises(ValueError):
            pa.bridge_samples(*bad)


def test_real_runs_lists_the_delivered_stretches():
    assert pa.real_runs([True, True, False, False, True, False, True]) == [(0, 2), (4, 5), (6, 7)]
    assert pa.real_runs([False, False]) == []
    assert pa.real_runs(np.ones(5, dtype=bool)) == [(0, 5)]
    with pytest.raises(ValueError):
        pa.real_runs(np.ones((2, 2), dtype=bool))


# ---------------------------------------------------------------------------------------------
# the sampler on real measurement audio (the real decoder shim, 48 kHz stereo, 1024-sample frames)
# ---------------------------------------------------------------------------------------------


def _eats_a_word(words_per_channel, start_s, stop_s, margin_s=0.06):
    return any(start_s - margin_s <= t < stop_s for ch in words_per_channel for t, _ in ch)


@pytest.mark.parametrize("jitter_ms", [0.0, 5.0])
def test_a_two_frame_hole_keeps_the_span_on_real_audio(decoder, rec_clip, tmp_path, jitter_ms):
    """The live hole: two NDI frames missing (42.7 ms), the next frame stamped exactly on the
    timeline or 5 ms late (the live offsets read +41.4...+52.6 ms). The span is kept: the verdicts
    are the uncut clip's (one start-up warm-up, then MEASUREMENT), the hole is logged as bridged,
    and the chain stays >= MARKER_CHAIN_MIN + 2 (bar a) unless the cut itself removed a decoded
    marker word, then >= MARKER_CHAIN_MIN + 1."""
    ref_blocks, _ = _frames(rec_clip)
    ref, _ = _run(ref_blocks, tmp_path, decoder)
    assert _verdicts(ref) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 4
    ref_words = decoder.decode(rec_clip, SR)
    exercised = {"clean": 0, "eats": 0}
    for t in np.arange(2.2, 9.3, 0.9):
        k = _frame_at(t)
        blocks, _ = _frames(rec_clip, drop={k, k + 1}, jitter={k + 2: round(jitter_ms * 10_000)})
        payloads, lines = _run(blocks, tmp_path, decoder)
        assert _verdicts(payloads) == _verdicts(ref), (t, lines)
        assert not any("starts over" in line for line in lines), (t, lines)
        bridged = [line for line in lines if "bridged with" in line]
        assert len(bridged) == 1 and "the marker span is kept" in bridged[0], lines
        want_ms = 2 * FRAME * 1e3 / SR + jitter_ms
        assert payloads[-1]["holes_bridged"] == 1
        assert payloads[-1]["bridged_ms"] == pytest.approx(want_ms, abs=0.06)
        eats = _eats_a_word(ref_words, k * FRAME / SR, (k + 2) * FRAME / SR)
        exercised["eats" if eats else "clean"] += 1
        need = pa.MARKER_CHAIN_MIN + (1 if eats else 2)
        assert min(p["marker_chain"] for p in _judged(payloads)) >= need, (t, eats, payloads)
    assert exercised["clean"] >= 3, exercised


def _hole(k, off_ms):
    """A real hole of `off_ms` at frame k: round(off / frame) frames never delivered and the next
    frame stamped with the rest as send jitter, so it sits `off_ms` ahead of the timeline."""
    n = max(1, int(round(off_ms / (FRAME * 1e3 / SR))))
    return set(range(k, k + n)), {k + n: round((off_ms - n * FRAME * 1e3 / SR) * 10_000)}


@pytest.mark.parametrize("off_ms, bridged", [(250.0, True), (300.0, False)])
def test_250_ms_is_bridged_and_300_ms_restarts(rec_clip, tmp_path, off_ms, bridged):
    """A 250 ms hole (12 frames missing, the next stamped 6 ms early) is bridged: the span kept, no
    warm-up, the markers after it on their line. A 300 ms one (14 frames) restarts the span: one
    UNKNOWN window."""
    audio = _with_timeline_markers(np.concatenate([rec_clip, rec_clip]))
    drop, jitter = _hole(_frame_at(6.0), off_ms)
    blocks, _ = _frames(audio, drop=drop, jitter=jitter)
    payloads, lines = _run(blocks, tmp_path, _TimelineMarkers())
    if bridged:
        assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 8
        assert any("bridged with 12000 zero samples (250.0 ms)" in line for line in lines), lines
        assert payloads[-1]["holes_bridged"] == 1 and payloads[-1]["bridged_ms"] == 250.0
        assert min(p["marker_chain"] for p in _judged(payloads)) >= 7
    else:
        assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "UNKNOWN"] + ["MEASUREMENT"] * 5
        assert any("timeline discontinuity" in line and "+300.0 ms" in line for line in lines), lines
        assert payloads[-1]["holes_bridged"] == 0


def test_an_overlap_of_45_ms_restarts(rec_clip, tmp_path):
    """A frame 45 ms BEHIND the timeline overlaps audio already received: never bridged."""
    blocks, _ = _frames(rec_clip, jumps={_frame_at(6.0): -450_000})
    payloads, lines = _run(blocks, tmp_path, FixedChain())
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "UNKNOWN", "MEASUREMENT"]
    assert any("timeline discontinuity" in line and "-45.0 ms" in line for line in lines), lines
    assert not any("bridged with" in line for line in lines)


def test_a_hole_at_a_sample_rate_change_restarts(tmp_path):
    """The zeros are counted at the previous frame's rate; a frame that also changes the rate is a
    new format and restarts like any format change."""
    x = _pink_stereo(1.0, -30.0)
    blocks = [_Block(SR, x[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR) for i in range(0, 20 * FRAME, FRAME)]
    blocks.append(_Block(44100, x[:FRAME], blocks[-1].timestamp + round(FRAME_100NS + 500_000)))
    _payloads, lines = _run(blocks, tmp_path, NoMarkers())
    assert any("format change (48000 Hz x 2 -> 44100 Hz x 2 channels" in line and "starts over" in line
               for line in lines), lines
    assert not any("bridged with" in line for line in lines)


def test_the_counters_reach_the_summary_and_the_json(rec_clip, tmp_path):
    """holes_bridged / bridged_ms: per 10-min interval in the summary line (reset with it), since
    the start in program-audio.json; a bridged frame's offset is a hole, never jitter, so it stays
    out of max_offset_ms."""
    a, b, c = _frame_at(2.0), _frame_at(4.0), _frame_at(8.0)
    gap = pas.LOG_SUMMARY_S + 1.0
    blocks, arrival = _frames(_with_timeline_markers(rec_clip), drop={a, a + 1, b, b + 1, c, c + 1},
                              jitter={a + 2: 50_000}, gaps={_frame_at(6.0): gap, c + 2: gap})
    payloads, lines = _run(blocks, tmp_path, _TimelineMarkers(), arrival_gaps=arrival)
    assert payloads[0]["holes_bridged"] == 0 and payloads[0]["bridged_ms"] == 0.0
    assert payloads[-1]["holes_bridged"] == 3
    assert payloads[-1]["bridged_ms"] == pytest.approx(3 * 42.667 + 5.0, abs=0.1)
    summary = [line for line in lines if "program-audio summary" in line]
    assert len(summary) == 2, lines
    # 5.0 = the frame after the late-stamped bridged frame (it sits 5 ms early); the bridged
    # frame's own +47.7 ms never counts
    assert summary[0].endswith("max_offset_ms=5.0 holes_bridged=2 bridged_ms=90.3"), summary[0]
    assert summary[1].endswith("holes_bridged=1 bridged_ms=42.7"), summary[1]
    assert "timeline_breaks=0" in summary[0] and "timeline_breaks=0" in summary[1]
    assert sum("bridged with" in line for line in lines) == 3


def test_the_payload_counters_are_null_when_not_sampling():
    from datetime import datetime, timezone

    p = pa.build_payload("UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", reason="sampler stopped")
    assert p["holes_bridged"] is None and p["bridged_ms"] is None
    q = pa.build_payload("MEASUREMENT", -35.0, 15.0, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", marker_chain=7, holes_bridged=4, bridged_ms=170.66666)
    assert q["holes_bridged"] == 4 and q["bridged_ms"] == 170.7
    with pytest.raises(ValueError):
        pa.build_payload("UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", holes_bridged=-1)


# ---------------------------------------------------------------------------------------------
# the replay of the live 7.10.2026 offsets
# ---------------------------------------------------------------------------------------------


def _replay(rec_clip, offsets, spacing_s=4.5):
    """Each live offset as a real hole: round(offset / frame) frames never delivered (at least
    one) and the next frame stamped with the rest as send jitter, one hole every `spacing_s` of
    sender audio, with the journal's arrival gap before it."""
    first = 3.0
    total_s = first + spacing_s * len(offsets) + 4.0
    audio = _with_timeline_markers(np.resize(rec_clip, (int(total_s * SR), 2)))
    drop, jitter, gaps = set(), {}, {}
    for j, (off_ms, gap_s) in enumerate(offsets):
        k = _frame_at(first + spacing_s * j)
        d, jit = _hole(k, off_ms)
        drop |= d
        jitter.update(jit)
        gaps[next(iter(jit))] = gap_s
    return _frames(audio, drop=drop, jitter=jitter, gaps=gaps)


def test_the_live_offset_replay_bridges_every_hole_up_to_250_ms(rec_clip, tmp_path):
    """The 49 offsets of 7.10.2026 at or under 250 ms: 0 UNKNOWN after the start-up warm-up (each
    of them restarted the span live), 49 bridged, 0 restarts."""
    small = [o for o in LIVE_OFFSETS_7_10 if o[0] <= pa.HOLE_BRIDGE_MAX_MS]
    assert len(small) == 49
    blocks, arrival = _replay(rec_clip, small)
    payloads, lines = _run(blocks, tmp_path, _TimelineMarkers(), arrival_gaps=arrival)
    verdicts = _verdicts(payloads)
    assert verdicts[:2] == ["UNKNOWN", "UNKNOWN"]
    assert set(verdicts[2:]) == {"MEASUREMENT"}, [v for v in verdicts[2:] if v != "MEASUREMENT"]
    assert payloads[-1]["holes_bridged"] == 49
    assert not any("starts over" in line for line in lines)


def test_the_full_live_replay_restarts_only_on_the_holes_over_250_ms(rec_clip, tmp_path):
    """All 55: the six over 250 ms (256.3, 337.8, 357.6, 543.0, 556.2, 571.7 ms) still restart, one
    UNKNOWN window each; the other 49 are bridged. (Before: 55 restarts.)"""
    blocks, arrival = _replay(rec_clip, LIVE_OFFSETS_7_10)
    payloads, lines = _run(blocks, tmp_path, _TimelineMarkers(), arrival_gaps=arrival)
    verdicts = _verdicts(payloads)
    assert verdicts[:2] == ["UNKNOWN", "UNKNOWN"]
    assert verdicts[2:].count("UNKNOWN") == 6 and "FOREIGN" not in verdicts
    assert sum("timeline discontinuity" in line for line in lines) == 6
    assert payloads[-1]["holes_bridged"] == 49


# ---------------------------------------------------------------------------------------------
# restreamer's safety checks: a bridged hole never hides or delays FOREIGN
# ---------------------------------------------------------------------------------------------


def _pink_stereo(seconds, dbfs, seed=1404):
    x = _scale_to_dbfs(_pink(int(seconds * SR), np.random.default_rng(seed)), dbfs)
    return np.ascontiguousarray(np.stack([x, x], axis=1), dtype=np.float32)


def _music(kind, seconds):
    if kind == "pink":
        return _pink_stereo(seconds, -20.0)
    return cal.synthetic_stream("chord", -15.0, seconds, np.random.default_rng([1404, 7]))


def _first_foreign(payloads):
    return next((i for i, p in enumerate(payloads) if p["last_foreign_ts_utc"] is not None), None)


ONSET_S = 6.5


@pytest.mark.parametrize("kind", ["pink", "chord"])
@pytest.mark.parametrize("hole_at, frames", [
    (ONSET_S - 1.3, 2),     # the hole in the window BEFORE the music arrives
    (ONSET_S - 0.3, 2),     # right before it, same window
    (ONSET_S - 0.02, 2),    # ACROSS the onset (the hole holds the first music samples)
    (ONSET_S - 0.1, 11),    # a 235 ms hole across the onset
    (ONSET_S + 0.3, 2),     # right after it, same window
    (ONSET_S + 1.6, 2),     # in the next window
])
def test_music_around_a_bridged_hole_reads_foreign_in_the_same_window(decoder, rec_clip, tmp_path,
                                                                      kind, hole_at, frames):
    """Measurement, then broadband music (pink, spectrally FOREIGN) or an in-band chord (FOREIGN only
    through the marker chain). With a bridged hole anywhere around the music's arrival, every window
    reads what it reads without the hole, so FOREIGN comes in the same window and the latch starts
    with the same payload."""
    audio = np.concatenate([rec_clip[: int(ONSET_S * SR)], _music(kind, 12.0 - ONSET_S)])
    ref, _ = _run(_frames(audio)[0], tmp_path, decoder)
    assert "FOREIGN" in _verdicts(ref)
    k = _frame_at(hole_at)
    holed, lines = _run(_frames(audio, drop=set(range(k, k + frames)))[0], tmp_path, decoder)
    assert sum("bridged with" in line for line in lines) == 1, lines
    assert _verdicts(holed) == _verdicts(ref), (kind, hole_at, lines)
    assert _first_foreign(holed) == _first_foreign(ref) is not None


def _mostly_zeros_frames(audio, start_s, seconds):
    """From `start_s` for `seconds`: 5 frames delivered, 10 frames (213 ms) missing, repeated, so
    those windows are about two thirds bridged zeros."""
    drop, k, stop = set(), _frame_at(start_s), _frame_at(start_s + seconds)
    while k < stop:
        drop.update(range(k + 5, k + 15))
        k += 15
    return drop


class _ZeroWordDecoder:
    """A decoder fake that finds a full real chain in any buffer holding a stretch of digital zero:
    the worst case of 'the inserted samples produce markers'. A loop that hands the bridged zeros
    to the decoder reads MEASUREMENT on music through it; one that decodes real samples only can
    never see them."""

    def __init__(self, inner):
        self.inner = inner
        self.calls = []

    def decode(self, samples, sample_rate):
        zero_run = _longest_zero_run(samples)
        self.calls.append(zero_run)
        if zero_run >= 512:
            ch = samples.shape[1]
            return [real_marker_words(8)] + [[] for _ in range(ch - 1)]
        return self.inner.decode(samples, sample_rate)


def _longest_zero_run(samples):
    z = np.all(np.asarray(samples) == 0.0, axis=1).astype(np.int8)
    if not z.any():
        return 0
    edges = np.flatnonzero(np.diff(np.concatenate(([0], z, [0]))))
    return int((edges[1::2] - edges[0::2]).max())


@pytest.mark.parametrize("kind", ["pink", "chord"])
def test_music_mostly_of_bridged_zeros_never_reads_measurement(decoder, tmp_path, kind):
    """Music only, from 1 s on about two thirds bridged zeros (10 of every 15 frames missing, each
    hole 213 ms): no window ever reads MEASUREMENT, every judged span's chain stays under
    MARKER_CHAIN_MIN, and the decoder never sees one bridged sample (the longest stretch of digital
    zero it was handed stays under one frame), so the zeros cannot add a word to the chain."""
    audio = _music(kind, 12.0)
    spy = _ZeroWordDecoder(decoder)
    holed, lines = _run(_frames(audio, drop=_mostly_zeros_frames(audio, 1.0, 10.5))[0], tmp_path, spy)
    assert sum("bridged with" in line for line in lines) >= 30, lines
    verdicts = _verdicts(holed)
    assert "MEASUREMENT" not in verdicts, verdicts
    assert verdicts[2:] == ["FOREIGN"] * (len(verdicts) - 2), verdicts
    assert spy.calls and max(spy.calls) < FRAME, spy.calls
    chains = [p["marker_chain"] for p in holed if p["marker_chain"] is not None]
    assert all(c < pa.MARKER_CHAIN_MIN for c in chains), chains


@pytest.mark.parametrize("kind", ["pink", "chord"])
def test_measurement_then_music_mostly_of_bridged_zeros_reads_as_without_the_holes(decoder, rec_clip,
                                                                                  tmp_path, kind):
    """Measurement, then music about two thirds bridged zeros: every window reads what it reads
    without the holes (the first music window's span still holds 2 s of real measurement, in both),
    so FOREIGN comes in the same window and the latch starts with the same payload."""
    audio = np.concatenate([rec_clip[:6 * SR], _music(kind, 8.0)])
    ref, _ = _run(_frames(audio)[0], tmp_path, decoder)
    spy = _ZeroWordDecoder(decoder)
    holed, lines = _run(_frames(audio, drop=_mostly_zeros_frames(audio, 6.0, 6.0))[0], tmp_path, spy)
    assert sum("bridged with" in line for line in lines) >= 15, lines
    assert _verdicts(holed) == _verdicts(ref), (_verdicts(holed), _verdicts(ref))
    assert _first_foreign(holed) == _first_foreign(ref) is not None
    assert spy.calls and max(spy.calls) < FRAME, spy.calls


def test_the_chain_never_grows_from_the_inserted_zeros():
    """decode_real_samples hands the decoder each delivered stretch on its own and moves the word
    times to their sender-timeline place: a decoder that would find a chain in the zeros finds
    nothing, while the real words keep their positions."""
    x = np.full((4 * SR, 2), 0.01, dtype=np.float32)
    real = np.ones(4 * SR, dtype=bool)
    real[SR: SR + 4096] = False
    real[3 * SR: 3 * SR + 2048] = False
    x[~real] = 0.0
    zero_words = _ZeroWordDecoder(NoMarkers())
    assert pa.span_markers(pas.decode_real_samples(zero_words, x, SR, real)) == (0, 0)
    assert pa.span_markers(pas.decode_real_samples(zero_words, x, SR, None))[1] == 8  # the old path

    class _Positions:
        def decode(self, samples, sample_rate):
            return [[(0.001, samples.shape[0] % 256)], []]

    words = pas.decode_real_samples(_Positions(), x, SR, real)
    assert [t for t, _ in words[0]] == pytest.approx([0.001, (SR + 4096) / SR + 0.001, (3 * SR + 2048) / SR + 0.001])
    assert words[1] == []


# ---------------------------------------------------------------------------------------------
# the spectral share stays a ratio of the real signal
# ---------------------------------------------------------------------------------------------


def _bridge(win, start_s, frames):
    y = win.copy()
    a = int(start_s * SR)
    y[a:a + frames * FRAME] = 0.0
    return y


def _mostly_zeros(win):
    y = win.copy()
    for k in range(0, win.shape[0] // FRAME, 15):
        y[(k + 5) * FRAME:(k + 15) * FRAME] = 0.0
    return y


@pytest.mark.parametrize("make", ["pink", "speech"])
@pytest.mark.parametrize("seed", [1, 2, 3])
def test_bridged_zeros_do_not_dilute_a_music_windows_share(make, seed):
    """Zeros add no energy to either side of the share: a music window with a bridged hole (2 or
    11 frames, at the centre or near an edge) or mostly bridged zeros keeps its outside-band share
    within 5 points and stays over the FOREIGN bar."""
    rng = np.random.default_rng(seed)
    gen = _pink if make == "pink" else _speech_shaped
    x = _scale_to_dbfs(gen(2 * SR, rng), -20.0)
    win = np.ascontiguousarray(np.stack([x, x], axis=1), dtype=np.float32)
    base = pa.analyse(win, SR)[1]
    assert base > pa.FOREIGN_OUTSIDE_BAND_PCT
    for y in (_bridge(win, 1.0, 2), _bridge(win, 0.05, 2), _bridge(win, 0.9, 11), _bridge(win, 1.7, 11),
              _mostly_zeros(win)):
        rms, outside = pa.analyse(y, SR)
        assert outside >= pa.FOREIGN_OUTSIDE_BAND_PCT and abs(outside - base) <= 5.0, (make, seed, base, outside)
        assert pa.spectral_foreign(rms, outside)


def test_a_bridged_hole_keeps_the_committed_measurement_windows_in_band(rec_clip):
    """The other direction, on the committed clip: a 2-frame hole at 19 positions in each of its
    five windows keeps the share under the FOREIGN bar.

    NOT a general guarantee (review round 1). The measurement's in-band energy comes in marker
    bursts (a decoded word every ~0.5 s), so a hole that removes a burst raises the share of the
    delivered audio. Random positions on the full rec3b + rec2 recordings (5340 window cases): one
    2-frame hole crossed the bar 3 times (worst 32.8 %), two holes per window 8 times (worst 53 %).
    A 5 ms fade of the hole edges gave 2 and 10, so the crossings are the lost burst, not the
    edges. Recorded on the issue as the residual false-FOREIGN risk."""
    worst = 0.0
    for w in range(rec_clip.shape[0] // (2 * SR)):
        win = rec_clip[w * 2 * SR:(w + 1) * 2 * SR]
        for start in np.arange(0.05, 1.95, 0.1):
            y = _bridge(win, start, 2)
            real = np.ones(y.shape[0], dtype=bool)
            a = int(start * SR)
            real[a:a + 2 * FRAME] = False
            worst = max(worst, pa.analyse(y, SR, real)[1])
    assert worst < pa.FOREIGN_OUTSIDE_BAND_PCT, worst


# ---------------------------------------------------------------------------------------------
# review round 1: the level of the delivered samples, the format at a hole, the exact limit, the
# zeros before the frame
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("dbfs, frames_missing, pattern", [
    (-59.6, 12, "one 249 ms hole"),
    (-56.0, 10, "two thirds bridged"),
])
def test_quiet_music_with_bridged_holes_stays_foreign_never_silent(tmp_path, dbfs, frames_missing, pattern):
    """Quiet broadband music just over the SILENT bar: with bridged holes every window reads what it
    reads without them. The level is the delivered samples' own; taken over the window with its
    zeros it fell under -60 dBFS and the window read SILENT, which the gate passes."""
    audio = _pink_stereo(10.0, dbfs, seed=7)
    ref, _ = _run(_frames(audio)[0], tmp_path, NoMarkers())
    assert set(_verdicts(ref)[1:]) == {"FOREIGN"}, _verdicts(ref)
    if pattern == "one 249 ms hole":
        drop, jitter = _hole(_frame_at(4.5), 249.0)
    else:
        drop, jitter = _mostly_zeros_frames(audio, 1.0, 8.5), {}
    holed, lines = _run(_frames(audio, drop=drop, jitter=jitter)[0], tmp_path, NoMarkers())
    assert any("bridged with" in line for line in lines), lines
    assert _verdicts(holed)[: len(_verdicts(ref))] == _verdicts(ref)[: len(_verdicts(holed))], (
        [(p["verdict"], p["rms_dbfs"]) for p in holed])
    assert "SILENT" not in _verdicts(holed)


def test_the_level_of_a_holed_window_is_the_delivered_samples_level():
    win = _pink_stereo(2.0, -30.0, seed=11)
    real = np.ones(win.shape[0], dtype=bool)
    real[30_000:42_000] = False
    real[60_000:70_240] = False
    y = win.copy()
    y[~real] = 0.0
    rms, _outside = pa.analyse(y, SR, real)
    want = 10 * np.log10(np.mean(win[real].astype(np.float64) ** 2))
    assert rms == pytest.approx(want, abs=0.01)
    assert pa.analyse(y, SR)[0] < rms - 0.5            # over the zeros it reads lower
    assert pa.analyse(win, SR, None) == pa.analyse(win, SR)
    nan_rms, none_share = pa.analyse(np.zeros_like(win), SR, np.zeros(win.shape[0], dtype=bool))
    assert np.isnan(nan_rms) and none_share is None and pa.classify(nan_rms, none_share, 7) == "UNKNOWN"
    with pytest.raises(ValueError):
        pa.analyse(y, SR, real[:-1])


def test_a_hole_at_a_channel_count_change_restarts(tmp_path):
    x = _pink_stereo(1.0, -30.0)
    blocks = [_Block(SR, x[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR) for i in range(0, 20 * FRAME, FRAME)]
    blocks.append(_Block(SR, x[:FRAME, :1], blocks[-1].timestamp + round(FRAME_100NS + 500_000)))
    _payloads, lines = _run(blocks, tmp_path, NoMarkers())
    assert any("format change (48000 Hz x 2 -> 48000 Hz x 1 channels" in line and "starts over" in line
               for line in lines), lines
    assert not any("bridged with" in line for line in lines)


def test_the_250_ms_limit_is_inclusive_to_the_100_ns_unit():
    """A 4800-sample frame is exactly 1 000 000 units of 100 ns, so the limit lands exactly."""
    tol = pa.continuity_tolerance_100ns(4800, SR)
    at_limit = TS0 + 1_000_000 + 2_500_000
    assert pa.frame_continues(TS0, 4800, SR, at_limit, tol) == (pa.BRIDGE, 12000)
    assert pa.frame_continues(TS0, 4800, SR, at_limit + 1, tol) == (pa.DISCONTINUITY, 0)


def test_the_zeros_go_in_before_the_frame_at_its_timeline_place(rec_clip, tmp_path, monkeypatch):
    """The window that holds the hole carries the zeros exactly where the missing frames were and
    the first frame after the hole right behind them: every delivered sample at its sender-timeline
    place (zeros pushed after the frame would move that frame 42.7 ms early)."""
    seen = []
    analyse = pa.analyse

    def spy(samples, sample_rate, real=None):
        seen.append((np.array(samples, copy=True), None if real is None else np.array(real, copy=True)))
        return analyse(samples, sample_rate, real)

    monkeypatch.setattr(pas.pa, "analyse", spy)
    k = _frame_at(5.0)                       # inside the third window [4 s, 6 s)
    _run(_frames(rec_clip, drop={k, k + 1})[0], tmp_path, NoMarkers())
    holed = [(w, r) for w, r in seen if r is not None]
    assert len(holed) == 1
    win, real = holed[0]
    a = k * FRAME - 2 * 2 * SR               # the hole's place inside the window starting at 4 s
    assert pa.real_runs(~real) == [(a, a + 2 * FRAME)]
    assert not win[a:a + 2 * FRAME].any()
    np.testing.assert_array_equal(win[a + 2 * FRAME:a + 3 * FRAME], rec_clip[(k + 2) * FRAME:(k + 3) * FRAME])
    np.testing.assert_array_equal(win[:a], rec_clip[2 * 2 * SR:k * FRAME])
