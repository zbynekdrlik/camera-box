"""issue 1404 -- the CG correlation's cross-window confirmation keys on the LAG, never on the kind
(program_audio_cg_corr.confirm / remember, program_audio_cg.CgArm.state).

Review of 3662dfa599 (finding 1): a weak PRESENT (DETECT_NCC <= |ncc| < STRONG_NCC) was confirmed only by
a previous RAW PRESENT at the same lag, so a real CG song ~26-28 dB under the program whose |ncc| at its
one true lag went 0.09, 0.05, 0.08, 0.06, 0.10 never read FOREIGN: every ABSENT window at that lag broke
the chain. Finding 5: confirming twice on one unchanged program segment confirmed a window by itself.
"""
import pathlib
import sys

import numpy as np
import pytest

_HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE))
sys.path.insert(0, str(_HERE.parents[1] / "scripts"))

import program_audio_cg as pcg  # noqa: E402
import program_audio_cg_corr as pcc  # noqa: E402
import test_program_audio_cg_corr_1404 as base  # noqa: E402
from test_program_audio_cg_corr_1404 import (  # noqa: E402
    CGOBS, SR, TS0, UNITS, _Probe, _monitor, frames, music_like, program_with)

W = 2 * UNITS  # one sampler window of program time


def _c(kind, ncc, lag, end):
    return pcc.Correlation(kind, ncc, lag, 0, seg_end=end)


def _run(raws):
    """The arm's memory over a sequence of raw correlations: the confirmed kind of each."""
    hist, out = (), []
    for raw in raws:
        out.append(pcc.confirm(hist, raw).kind)
        hist = pcc.remember(hist, raw)
    return out


def _shape(nccs, lags=None, kinds=None):
    lags = lags or [600.0] * len(nccs)
    return [_c(kinds[i] if kinds else (pcc.PRESENT if abs(n) >= pcc.DETECT_NCC else pcc.ABSENT),
               n, lags[i], TS0 + i * W) for i, n in enumerate(nccs)]


# ----------------------------------------------------------------- the pure rule

def test_present_absent_at_the_same_lag_present_reads_present():
    assert _run(_shape([0.09, 0.05, 0.08])) == [pcc.ABSENT, pcc.ABSENT, pcc.PRESENT]


def test_a_real_song_dipping_under_the_threshold_at_its_one_lag_is_confirmed():
    # the review's shape: 0.09, 0.05, 0.08, 0.06, 0.10 at one stable lag (jitter under LAG_AGREE_MS)
    got = _run(_shape([0.09, 0.05, 0.08, 0.06, 0.10], lags=[600.0, 601.0, 599.5, 600.5, 600.0]))
    assert got == [pcc.ABSENT, pcc.ABSENT, pcc.PRESENT, pcc.ABSENT, pcc.PRESENT]


def test_isolated_weak_presents_at_jumping_lags_never_confirm():
    # A SYNTHETIC shape around E2E 41742028800's live PRESENT values (0.071-0.083 at jumping lags); the
    # ABSENT windows between them are invented (the run kept no per-window ncc/lag). The measured null
    # replay is in program-audio-guard.md (992 live windows, 4.11.2026: none PRESENT, none confirmed).
    nccs = [0.031, 0.083, 0.028, 0.035, 0.071, 0.030, 0.072, 0.026, 0.044, 0.079]
    lags = [1210.0, 2734.0, -310.0, 455.0, -998.0, 2001.0, 623.0, -742.0, 1888.0, 80.0]
    assert pcc.PRESENT not in _run(_shape(nccs, lags))


def test_an_earlier_window_under_the_agreement_floor_never_vouches():
    floor = pcc.AGREE_FLOOR_NCC
    two = 2 * W                                    # one segment apart: no shared audio
    assert _run([_c(pcc.ABSENT, floor - 0.005, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + two)]) == \
        [pcc.ABSENT, pcc.ABSENT]
    assert _run([_c(pcc.ABSENT, floor, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + two)]) == \
        [pcc.ABSENT, pcc.PRESENT]


def test_an_overlapping_window_vouches_only_when_it_was_present_itself():
    # review round 2: the next window shares 2 s of program with this one, so its null peak can sit at
    # the same lag; an overlapping earlier window must be PRESENT (the old rule), a disjoint one any kind
    assert _run([_c(pcc.ABSENT, 0.06, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + W)]) == \
        [pcc.ABSENT, pcc.ABSENT]
    assert _run([_c(pcc.PRESENT, 0.08, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + W)]) == \
        [pcc.ABSENT, pcc.PRESENT]


def test_an_undecided_window_between_two_presents_does_not_break_the_chain():
    raws = [_c(pcc.PRESENT, 0.08, 600.0, TS0), pcc.Correlation(pcc.UNDECIDED, why="coverage"),
            _c(pcc.PRESENT, 0.08, 601.0, TS0 + 2 * W)]
    assert _run(raws) == [pcc.ABSENT, pcc.UNDECIDED, pcc.PRESENT]


def test_an_agreement_older_than_the_confirm_span_does_not_count():
    old = int(pcc.CONFIRM_SPAN_S * UNITS) + 1
    assert _run([_c(pcc.PRESENT, 0.08, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + old)]) == \
        [pcc.ABSENT, pcc.ABSENT]


def test_only_the_last_confirm_windows_earlier_windows_can_vouch():
    raws = _shape([0.08] + [0.05] * pcc.CONFIRM_WINDOWS + [0.08],
                  lags=[600.0] + [1500.0] * pcc.CONFIRM_WINDOWS + [600.0])
    for i, r in enumerate(raws):   # all inside the span: only the window count limits it
        raws[i] = r._replace(seg_end=TS0 + i * UNITS)
    assert _run(raws)[-1] == pcc.ABSENT


def test_the_same_segment_read_twice_never_confirms_itself():
    weak = _c(pcc.PRESENT, 0.08, 600.0, TS0)
    assert _run([weak, weak, weak]) == [pcc.ABSENT, pcc.ABSENT, pcc.ABSENT]
    assert len(pcc.remember(pcc.remember((), weak), weak)) == 1   # a repeated segment never advances it


def test_a_strong_present_still_counts_at_once():
    assert _run([_c(pcc.PRESENT, pcc.STRONG_NCC, 600.0, TS0)]) == [pcc.PRESENT]


def test_judge_stamps_the_judged_segment_end():
    song = music_like()
    c = pcc.judge(frames(program_with(song)), frames(song))
    assert c.kind == pcc.PRESENT and isinstance(c.seg_end, int) and c.seg_end > TS0


# ----------------------------------------------------------------- the arm's memory

class _Ring:
    def snapshot(self):
        return [], False


class _FakeMonitor:
    def __init__(self, source):
        self.source, self.level, self.ring = source, -20.0, _Ring()

    def reading(self, now=None):
        return pcg.CgReading(self.source, self.level, 0.1, 1, None)


def _scripted_arm(monkeypatch, raws):
    queue = list(raws)
    monkeypatch.setattr(pcg, "_correlate", lambda _prog, _m: queue.pop(0))
    m = _FakeMonitor(CGOBS)
    return pcg.CgArm("resolume-snv", [m], _Probe()), m


def test_the_arm_keeps_its_memory_across_a_quiet_window(monkeypatch):
    # review round 2: a song passage under the silence bar for one window is the loudness twin of the
    # ABSENT dip; the memory ages by the judged segments' end stamps (CONFIRM_SPAN_S), never by quiet
    arm, m = _scripted_arm(monkeypatch, [_c(pcc.PRESENT, 0.08, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + 2 * W)])
    prog = pcc.AudioRing()
    assert arm.state(program=prog).readings[0].correlation.kind == pcc.ABSENT
    m.level = None                                     # quiet: never correlated
    assert arm.state(program=prog).readings[0].correlation is None
    m.level = -20.0
    assert arm.state(program=prog).readings[0].correlation.kind == pcc.PRESENT


def test_the_arm_forgets_everything_without_a_program_ring(monkeypatch):
    arm, _m = _scripted_arm(monkeypatch, [_c(pcc.PRESENT, 0.08, 600.0, TS0), _c(pcc.PRESENT, 0.08, 600.0, TS0 + 2 * W)])
    prog = pcc.AudioRing()
    assert arm.state(program=prog).readings[0].correlation.kind == pcc.ABSENT
    assert arm.state(program=None).readings[0].correlation is None
    assert arm.state(program=prog).readings[0].correlation.kind == pcc.ABSENT


def test_the_arm_confirms_across_an_absent_window(monkeypatch):
    arm, _m = _scripted_arm(monkeypatch, _shape([0.09, 0.05, 0.08]))
    prog = pcc.AudioRing()
    kinds = [arm.state(program=prog).readings[0].correlation.kind for _ in range(3)]
    assert kinds == [pcc.ABSENT, pcc.ABSENT, pcc.PRESENT]


def test_a_frozen_program_ring_never_confirms_by_itself():
    """The sampler's no-audio UNKNOWN path calls the arm every window on a program ring that no longer
    grows: the same segment, judged again and again, must stay unconfirmed."""
    song = music_like()
    cg = _monitor(CGOBS, np.stack([song, song], axis=1) * 0.3)
    arm = pcg.CgArm("resolume-snv", [cg], _Probe())
    ring = pcc.AudioRing(mono=False)
    for ts, x, sr in frames(program_with(song, rel_db=-24.0)):
        ring.push(ts, x, sr)
    first = arm.state(now=100.0, program=ring).readings[0].correlation
    assert pcc.DETECT_NCC <= abs(first.ncc) < pcc.STRONG_NCC, first   # a WEAK present, as the old test had
    for _ in range(3):
        assert arm.state(now=100.0, program=ring).readings[0].correlation.kind == pcc.ABSENT


def test_the_arm_fed_advancing_rings_confirms_a_song_24_db_under_the_marker():
    """Replaces the old one-snapshot-read-twice arm test: the CG ring holds 14 s, the program ring grows
    window by window (10, 12, 14 s), each judged segment is a NEW one."""
    secs = 14.0
    song = music_like(secs=secs)
    cg = _monitor(CGOBS, np.stack([song, song], axis=1) * 0.3)
    arm = pcg.CgArm("resolume-snv", [cg], _Probe())
    # program_with's marker is SECS (10 s) long: build the 14 s program here
    marker = base.marker_like(1, secs=secs)
    prog = marker + base.room_floor(11, secs=secs)
    prog = prog + base.delayed(base._scale(song, base._power(marker) * 10 ** (-24.0 / 10)), 0.6)
    prog = np.stack([prog, base.delayed(prog, 0.0102)], axis=1)
    kinds, ends, lags = [], [], []
    for end_s in (10.0, 12.0, 14.0):
        ring = pcc.AudioRing(mono=False)
        for ts, x, sr in frames(prog[:int(end_s * SR)]):
            ring.push(ts, x, sr)
        c = arm.state(now=100.0, program=ring).readings[0].correlation
        kinds.append(c.kind)
        ends.append(c.seg_end)
        lags.append(c.lag_ms)
    assert len(set(ends)) == 3, ends
    assert all(lag == pytest.approx(600.0, abs=pcc.LAG_AGREE_MS) for lag in lags), lags
    assert pcc.PRESENT in kinds[1:], kinds
