"""issue 1404 -- a fleet date step is never bridged, and a chain cut short by bridged audio is never
FOREIGN on its own.

DATE_STEP (design issue 1404 comment 6037613222, Approach 1): the nightly dantesync date step moves
the sender's wall clock, and so its NDI timestamps, forward by delta with no sample lost. Bridged with
delta of zeros it pushed every later marker round(60 * delta) indices off the chain line (Design-
question 6036260703: 2 FOREIGN windows per step of 50-250 ms on the committed clip). dev1 runs the
same fleet dantesync and steps its own wall clock at the same announced instant, so the sampler reads
dev1's wall-minus-monotonic offset with every block: a forward timestamp jump that matches a dev1
wall step of the same size (within DATE_STEP_MATCH_MS, seen within DATE_STEP_WINDOW_S) re-bases the
timeline, with no zeros and no restart. Without a matching dev1 step the bridge / restart rules stay.

HOLED SPAN (ROZHODNUTÉ issue 1404 comment 6037765523, restreamer run 37602415434): a marker chain
that falls short while the trailing span holds bridged samples (or queue drops), with a
measurement-like spectrum, reads UNKNOWN, never FOREIGN. A spectral FOREIGN stays immediate.
The live pattern: 16 "holes" of +41..+54 ms within 13 s gave `chain=3` -> FOREIGN at 14:17:37.
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
from qpsk_guard_shim_1404 import NoMarkers, build_shim  # noqa: E402
from test_program_audio_bridge_1404 import _music, _pink_stereo, _with_timeline_markers, _TimelineMarkers  # noqa: E402
from test_program_audio_timeline_1404 import _Block, _verdicts  # noqa: E402

REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
SR = 48000
FRAME = 1024
FRAME_100NS = FRAME * 10_000_000 / SR
TS0 = 17_913_444_397_535_105
NDI_UNITS = 10_000_000
WALL0 = 1_791_000_000_000_000_000   # a wall-minus-monotonic offset in ns (any value)


@pytest.fixture(scope="session")
def decoder(tmp_path_factory):
    return pam.MarkerDecoder(build_shim(tmp_path_factory.mktemp("qpsk-guard-shim-datestep")))


@pytest.fixture(scope="session")
def rec_clip():
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == SR and x.shape[1] == 2
    return np.ascontiguousarray(x, dtype=np.float32)


def _tol():
    return pa.continuity_tolerance_100ns(FRAME, SR)


def _ts_ahead(off_ms):
    return TS0 + round(FRAME_100NS + off_ms * 10_000)


def _ms(v):
    return v * 10_000.0          # ms -> 100 ns


# ---------------------------------------------------------------------------------------------
# the pure decision
# ---------------------------------------------------------------------------------------------


def test_the_date_step_constants_are_single_sourced():
    assert pa.DATE_STEP == "DATE_STEP"
    assert pa.DATE_STEP_MATCH_MS == 20.0
    assert pa.DATE_STEP_WINDOW_S == 2.0
    assert pa.WALL_STEP_MIN_MS == 5.0


@pytest.mark.parametrize("step_ms", [50.0, 200.0, 250.0, 400.0, 1500.0])
def test_a_forward_jump_matching_a_dev1_wall_step_is_a_date_step(step_ms):
    got = pa.frame_continues(TS0, FRAME, SR, _ts_ahead(step_ms + 3.0), _tol(), wall_steps=(_ms(step_ms),))
    assert got == (pa.DATE_STEP, 0)


@pytest.mark.parametrize("jump_ms, kind", [(219.9, pa.DATE_STEP), (180.1, pa.DATE_STEP),
                                           (220.2, pa.BRIDGE), (179.8, pa.BRIDGE)])
def test_the_match_is_within_20_ms(jump_ms, kind):
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(jump_ms), _tol(), wall_steps=(_ms(200.0),)).kind == kind


@pytest.mark.parametrize("jump_ms, want", [(200.0, (pa.BRIDGE, 9600)), (1500.0, (pa.DISCONTINUITY, 0)),
                                           (41.0, (pa.CONTINUE, 0))])
def test_without_a_dev1_step_the_rules_stay_as_today(jump_ms, want):
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(jump_ms), _tol()) == want
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(jump_ms), _tol(), wall_steps=()) == want


def test_only_a_forward_jump_and_a_forward_dev1_step_match():
    """The design's DATE_STEP is a FORWARD jump: a backward jump stays a discontinuity even with a
    matching backward dev1 step, and a dev1 step of the other sign never excuses a jump."""
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(-200.0), _tol(), wall_steps=(_ms(-200.0),)).kind \
        == pa.DISCONTINUITY
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(200.0), _tol(), wall_steps=(_ms(-200.0),)).kind \
        == pa.BRIDGE


def test_a_jump_inside_the_tolerance_is_never_a_date_step():
    assert pa.frame_continues(TS0, FRAME, SR, _ts_ahead(10.0), _tol(), wall_steps=(_ms(10.0),)) == (pa.CONTINUE, 0)


def test_matching_wall_step_picks_the_closest_step():
    assert pa.matching_wall_step(_ms(205.0), (_ms(400.0), _ms(210.0), _ms(195.0))) == _ms(210.0)
    assert pa.matching_wall_step(_ms(205.0), (_ms(400.0),)) is None
    assert pa.matching_wall_step(_ms(-205.0), (_ms(-205.0),)) is None


def test_wall_steps_reads_a_step_of_the_wall_minus_monotonic_offset():
    w = pa.WallSteps()
    assert w.observe(10.0, WALL0) is None                     # the first reading is the baseline
    assert w.observe(10.02, WALL0 + 300_000) is None          # 0.3 ms: read noise / no step
    assert w.observe(10.04, None) is None                     # no clean read: ignored, baseline kept
    assert w.observe(10.06, WALL0 + 200_300_000) == pytest.approx(_ms(200.0))
    assert w.recent(10.07) == pytest.approx((_ms(200.0),))
    assert w.recent(12.05) == pytest.approx((_ms(200.0),))    # within DATE_STEP_WINDOW_S
    assert w.recent(12.07) == ()                              # older than 2 s: gone
    w.observe(13.0, WALL0 + 400_300_000)
    w.consume(_ms(200.0))
    assert w.recent(13.0) == ()


def test_wall_steps_consume_takes_one_step_once():
    w = pa.WallSteps()
    w.observe(1.0, WALL0)
    w.observe(1.1, WALL0 + 200_000_000)
    w.consume(pa.matching_wall_step(_ms(203.0), w.recent(1.2)))
    assert w.recent(1.2) == ()
    w.consume(_ms(999.0))                                     # nothing to take: no error


# ---------------------------------------------------------------------------------------------
# the sampler loop: a date step with and without dev1's own step
# ---------------------------------------------------------------------------------------------


def _frames(audio, jumps=None, jitter=None, drop=()):
    jumps, jitter = jumps or {}, jitter or {}
    out, shift = [], 0
    for k, i in enumerate(range(0, audio.shape[0], FRAME)):
        shift += jumps.get(k, 0)
        if k in drop:
            continue
        out.append(_Block(SR, audio[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR + shift + jitter.get(k, 0)))
    return out


def _run_with_wall(blocks, tmp_path, decoder, wall_steps_at=None):
    """Drive pas.run synchronously; dev1's wall-minus-monotonic offset steps by `ns` from the
    `k`-th captured block on for each (k, ns) in `wall_steps_at`."""
    wall_steps_at = wall_steps_at or {}
    clock = {"t": 100.0}
    state = {"i": 0, "reads": 0}
    payloads, lines = [], []

    class Rx:
        def capture(self, _t):
            i = state["i"]
            if i >= len(blocks):
                return None
            clock["t"] += blocks[i].samples.shape[0] / blocks[i].sample_rate
            state["i"] = i + 1
            return blocks[i]

        def connections(self):
            return 1

    def wall_offset():
        k = state["i"] - 1          # the block just captured
        state["reads"] += 1
        return WALL0 + sum(ns for at, ns in wall_steps_at.items() if k >= at)

    pas.run(Rx(), str(tmp_path), source="S", decoder=decoder, mono=lambda: clock["t"],
            max_loops=len(blocks), on_write=payloads.append, log=lines.append, wall_offset=wall_offset)
    assert state["reads"] == len(blocks)
    return payloads, lines


def _frame_at(seconds):
    return int(round(seconds * SR)) // FRAME


@pytest.mark.parametrize("step_ms", [50.0, 200.0, 250.0, 1500.0])
def test_a_date_step_with_a_matching_dev1_wall_step_keeps_the_span(decoder, rec_clip, tmp_path, step_ms):
    """The sender steps +step_ms at 6.1 s with nothing lost; dev1 stepped the same at the same
    moment. No zeros, no restart: 0 UNKNOWN after the start-up warm-up, 0 FOREIGN, and the chain
    on its line (>= MIN + 2, bar a) in every judged window."""
    audio = rec_clip
    k = _frame_at(6.1)
    blocks = _frames(audio, jumps={k: round(step_ms * 10_000)})
    payloads, lines = _run_with_wall(blocks, tmp_path, decoder, {k: round(step_ms * 1e6)})
    verdicts = _verdicts(payloads)
    assert verdicts[:2] == ["UNKNOWN", "UNKNOWN"]
    assert set(verdicts[2:]) == {"MEASUREMENT"}, (verdicts, lines)
    assert payloads[-1]["last_foreign_ts_utc"] is None
    date = [line for line in lines if "timeline date step" in line]
    assert len(date) == 1 and "no zeros" in date[0], lines
    assert not any("bridged with" in line or "starts over" in line for line in lines), lines
    assert payloads[-1]["holes_bridged"] == 0
    assert min(p["marker_chain"] for p in payloads if p["marker_chain"] is not None) >= pa.MARKER_CHAIN_MIN + 2


def test_dev1_stepping_up_to_2_s_before_the_sender_still_matches(decoder, rec_clip, tmp_path):
    audio = rec_clip
    k = _frame_at(6.1)
    blocks = _frames(audio, jumps={k: 2_000_000})
    payloads, lines = _run_with_wall(blocks, tmp_path, decoder, {k - _frame_at(1.5): 200_000_000})
    assert set(_verdicts(payloads)[2:]) == {"MEASUREMENT"}, lines
    assert any("timeline date step" in line for line in lines)


@pytest.mark.parametrize("step_ms, bridged", [(200.0, True), (1500.0, False)])
def test_a_date_step_without_a_dev1_step_behaves_as_today(rec_clip, tmp_path, step_ms, bridged):
    """No dev1 wall step (dev1's dantesync missed it, or the step came 2.5 s earlier): the jump is
    bridged (<= 250 ms) or restarts the span, exactly as before this change."""
    audio = _with_timeline_markers(np.concatenate([rec_clip, rec_clip]))
    k = _frame_at(6.1)
    blocks = _frames(audio, jumps={k: round(step_ms * 10_000)})
    early = {k - _frame_at(2.5): round(step_ms * 1e6)}
    for wall in ({}, early):
        payloads, lines = _run_with_wall(blocks, tmp_path, _TimelineMarkers(), wall)
        assert not any("timeline date step" in line for line in lines), lines
        if bridged:
            assert any("bridged with 9600 zero samples" in line for line in lines), lines
        else:
            assert sum("timeline discontinuity" in line for line in lines) == 1, lines
            assert _verdicts(payloads).count("UNKNOWN") == 3


def test_one_dev1_step_excuses_one_jump_only(rec_clip, tmp_path):
    """A second jump of the same size right after (a real 200 ms hole 1 s later) is not excused by
    the step the date step already used."""
    audio = _with_timeline_markers(np.concatenate([rec_clip, rec_clip]))
    k = _frame_at(6.1)
    blocks = _frames(audio, jumps={k: 2_000_000, k + _frame_at(1.0): 2_000_000})
    _payloads, lines = _run_with_wall(blocks, tmp_path, _TimelineMarkers(), {k: 200_000_000})
    assert sum("timeline date step" in line for line in lines) == 1, lines
    assert sum("bridged with 9600 zero samples" in line for line in lines) == 1, lines


def test_the_date_steps_reach_the_summary(rec_clip, tmp_path, monkeypatch):
    monkeypatch.setattr(pas, "LOG_SUMMARY_S", 0.0)
    audio = np.concatenate([rec_clip, rec_clip])
    k = _frame_at(6.1)
    _payloads, lines = _run_with_wall(_frames(audio, jumps={k: 2_000_000}), tmp_path, NoMarkers(),
                                      {k: 200_000_000})
    summary = [line for line in lines if "program-audio summary" in line]
    assert sum(int(line.split("date_steps=")[1].split()[0]) for line in summary) == 1, summary


# ---------------------------------------------------------------------------------------------
# a holed span's short chain is UNKNOWN, never FOREIGN (ROZHODNUTÉ 6037765523)
# ---------------------------------------------------------------------------------------------


def test_classify_reads_a_short_chain_over_a_holed_span_as_unknown():
    short = pa.MARKER_CHAIN_MIN - 1
    assert pa.classify(-35.0, 16.0, short, holed=True) == "UNKNOWN"
    assert pa.classify(-35.0, 16.0, short, holed=False) == "FOREIGN"
    assert pa.classify(-35.0, 16.0, short) == "FOREIGN"
    assert pa.classify(-35.0, 16.0, 0, holed=True) == "UNKNOWN"
    assert pa.classify(-35.0, 16.0, pa.MARKER_CHAIN_MIN, holed=True) == "MEASUREMENT"
    # a spectral FOREIGN stays immediate, holed or not
    assert pa.classify(-20.0, pa.FOREIGN_OUTSIDE_BAND_PCT, short, holed=True) == "FOREIGN"
    assert pa.classify(-20.0, 90.0, None, holed=True) == "FOREIGN"
    assert pa.classify(-70.0, 16.0, short, holed=True) == "SILENT"


LIVE_STEPS_MS = (44.8, 41.8, 43.5, 41.8, 44.4, 42.3, 42.3, 41.8, 49.7, 41.8, 41.9, 41.6, 42.7, 45.5,
                 53.9, 47.2)


def _live_pattern(audio, shape):
    """16 forward steps of 41.6-53.9 ms within 13 s (one every 0.8 s from 4.4 s), the live pattern of
    14:17:27-14:17:40. `loss`: frames really missing (round(step / frame) frames dropped, the rest
    as send jitter); `stall`: nothing missing, the sender submitted frame k step_ms late and frame
    k+1 step_ms - 21.3 ms late (STEP 0: two frames at -21.1 ms follow every live step)."""
    drop, jitter = set(), {}
    for j, step in enumerate(LIVE_STEPS_MS):
        k = _frame_at(4.4 + 0.8 * j)
        if shape == "loss":
            n = 2
            drop |= {k, k + 1}
            jitter[k + 2] = round((step - n * FRAME * 1e3 / SR) * 10_000)
        else:
            jitter[k] = round(step * 10_000)
            jitter[k + 1] = round((step - FRAME * 1e3 / SR) * 10_000)
    assert 4.4 + 0.8 * 15 - 4.4 <= 13.0
    return _frames(audio, jitter=jitter, drop=drop)


@pytest.mark.parametrize("shape", ["loss", "stall"])
def test_the_live_pattern_of_16_holes_in_13_s_never_reads_foreign(decoder, rec_clip, tmp_path, shape):
    """The exact live pattern on real measurement audio through the real decoder: 0 FOREIGN windows,
    and nothing reads MEASUREMENT on a chain under MARKER_CHAIN_MIN. As real losses every step is
    bridged; as sender stalls none is (the look-ahead, ROZHODNUTÉ on issue 1404)."""
    audio = np.concatenate([rec_clip, rec_clip])
    payloads, lines = _run_with_wall(_live_pattern(audio, shape), tmp_path, decoder)
    verdicts = _verdicts(payloads)
    assert "FOREIGN" not in verdicts, [(p["verdict"], p["marker_chain"]) for p in payloads]
    assert payloads[-1]["last_foreign_ts_utc"] is None
    for p in payloads:
        if p["verdict"] == "MEASUREMENT":
            assert p["marker_chain"] >= pa.MARKER_CHAIN_MIN
    if shape == "loss":
        assert sum("bridged with" in line for line in lines) >= 10, lines
    else:
        assert not any("bridged with" in line for line in lines), lines
        assert payloads[-1]["sender_stalls"] == 16


def test_a_holed_unknown_says_why(decoder, rec_clip, tmp_path):
    """An in-band chord after the measurement, with the live pattern as real losses: the windows whose
    span holds the bridged audio read UNKNOWN and say why. (The pattern as sender stalls bridges
    nothing since the look-ahead, so it holds no such window.)"""
    audio = np.concatenate([rec_clip, _music("chord", 14.0)])
    payloads, _lines = _run_with_wall(_live_pattern(audio, "loss"), tmp_path, decoder)
    holed = [p for p in payloads if p["verdict"] == "UNKNOWN" and "bridged" in (p.get("reason") or "")]
    assert len(holed) >= 2, [(p["verdict"], p["marker_chain"], p.get("reason")) for p in payloads]
    for p in holed:
        assert p["marker_chain"] is not None and p["marker_chain"] < pa.MARKER_CHAIN_MIN
        assert "never FOREIGN" in p["reason"]


@pytest.mark.parametrize("kind", ["pink", "chord"])
def test_music_after_the_holed_measurement_still_reads_foreign(decoder, rec_clip, tmp_path, kind):
    """Rule A only softens a SHORT CHAIN over a holed span: broadband music reads FOREIGN through the
    spectrum at once, and an in-band chord reads FOREIGN as soon as its span holds no bridged audio
    (here 4 s after the last hole). Never MEASUREMENT."""
    # 14 s of music: the bridges are the lost audio itself (the look-ahead), so 12 s left the last
    # window short by the early-stamped frames' few samples and it never completed
    audio = np.concatenate([rec_clip, _music(kind, 14.0)])
    blocks = _live_pattern(audio, "loss")
    payloads, _lines = _run_with_wall(blocks, tmp_path, decoder)
    verdicts = _verdicts(payloads)
    music_from = int(rec_clip.shape[0] / SR / 2) + 1     # the first window that is all music
    assert "MEASUREMENT" not in verdicts[music_from + 1:], verdicts
    assert verdicts[-1] == "FOREIGN", verdicts
    assert payloads[-1]["last_foreign_ts_utc"] is not None
    if kind == "pink":
        assert set(verdicts[music_from + 1:]) == {"FOREIGN"}, verdicts


def test_quiet_broadband_music_with_holes_stays_foreign(tmp_path):
    """A spectral FOREIGN never waits for a chain, holed or not (NoMarkers: every chain is 0)."""
    audio = _pink_stereo(20.0, -40.0, seed=3)
    payloads, _lines = _run_with_wall(_live_pattern(audio, "loss"), tmp_path, NoMarkers())
    assert set(_verdicts(payloads)[1:]) == {"FOREIGN"}, _verdicts(payloads)
