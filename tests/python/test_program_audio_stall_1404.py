"""issue 1404 -- the sender-stall look-ahead (ROZHODNUTÉ on issue 1404, Design-question 6037861831).

STEP 0 of design 6037613222 measured it: the small "holes" (+41..+54 ms) were no lost audio. The
stream OBS stamps each NDI audio frame with its wall clock AT SUBMISSION; when its audio thread
stalls ~64 ms it submits the next frames back to back, so the first sits +42 ms "ahead" and the next
two -21.1 ms each. The cumulative offset over the next frames comes back to about 0. Bridging the
+42 ms with zeros put every later marker 2.6 indices off the chain line: the live false FOREIGN
(chain 3), with rule A a flap UNKNOWN (the replay: U U M M M M U M U U M).

The rule:
  * a frame more than the tolerance AHEAD with no matching dev1 wall step is held together with up
    to STALL_LOOKAHEAD_FRAMES (4) following frames (~85 ms);
  * the hole = the SMALLEST cumulative offset over them, against the frame before the step;
  * at or under the tolerance -> a SENDER STALL: no zeros, the span kept, counted as `sender_stalls`
    (additive in the summary and program-audio.json);
  * over it -> a real hole of that size: bridged up to 250 ms, else a restart, as before;
  * rule A and the spectral FOREIGN are unchanged.
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
import program_audio_capture as pac  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
from qpsk_guard_shim_1404 import NoMarkers, build_shim  # noqa: E402
from test_program_audio_bridge_1404 import _first_foreign, _music  # noqa: E402
from test_program_audio_datestep_1404 import _live_pattern, _run_with_wall  # noqa: E402
from test_program_audio_timeline_1404 import _Block, _run, _verdicts  # noqa: E402

REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
SR = 48000
FRAME = 1024
FRAME_MS = FRAME * 1e3 / SR                   # 21.333 ms
FRAME_100NS = FRAME * 10_000_000 / SR
TS0 = 17_913_444_397_535_105                  # a real sender timestamp read live on 7.10.2026 (100 ns)
NDI_UNITS = 10_000_000
# STEP 0 (comment 6037861831): identical in all three dev1 receivers, each followed by two frames
# at -21.1 ms (the stream OBS log: `audio-stall #1367: tick_gap_max_ms=58.6...68.3`)
STEP0_STALLS_MS = (42.2, 45.6, 42.0)
FOLLOWER_MS = -21.1


@pytest.fixture(scope="session")
def decoder(tmp_path_factory):
    return pam.MarkerDecoder(build_shim(tmp_path_factory.mktemp("qpsk-guard-shim-stall")))


@pytest.fixture(scope="session")
def rec_clip():
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == SR and x.shape[1] == 2
    return np.ascontiguousarray(x, dtype=np.float32)


def _tol():
    return pa.continuity_tolerance_100ns(FRAME, SR)


def _ms(v):
    return round(v * 10_000)          # ms -> 100 ns


def _place(k):
    """Frame k's place on the sender timeline after the frame stamped TS0 (frame 0)."""
    return TS0 + round(k * FRAME_100NS)


def _held(*cum_ms, drop0_ms=0.0):
    """Held frames 1, 2, ... after frame 0 at TS0, each `cum_ms` off its own timeline place (the
    known drop of the first one shifts every place by `drop0_ms`)."""
    return [(_place(i + 1) + _ms(drop0_ms) + _ms(c), FRAME, _ms(drop0_ms) if i == 0 else 0.0)
            for i, c in enumerate(cum_ms)]


def _resolve(held, complete=False):
    return pa.resolve_ahead(TS0, FRAME, SR, held, _tol(), complete=complete)


# ---------------------------------------------------------------------------------------------
# the pure decision: pa.resolve_ahead
# ---------------------------------------------------------------------------------------------


def test_the_lookahead_is_four_frames_single_sourced():
    assert pa.STALL_LOOKAHEAD_FRAMES == 4
    assert pa.SENDER_STALL == "SENDER_STALL"
    assert 4 * FRAME_MS == pytest.approx(85.3, abs=0.1)   # "~85 ms"


@pytest.mark.parametrize("step_ms", STEP0_STALLS_MS)
def test_the_recorded_stall_is_a_sender_stall_decided_at_its_first_catch_up_frame(step_ms):
    """+step, then -21.1 ms: the second frame's cumulative offset is back within the tolerance, so
    the look-ahead decides at once (no wait for 4 frames): nothing lost, no zeros, the first frame
    filed at its timeline place (never its late stamp)."""
    first = _held(step_ms)
    assert _resolve(first) is None                      # one frame ahead: hold the next one
    got = _resolve(_held(step_ms, step_ms + FOLLOWER_MS))
    assert got.kind == pa.SENDER_STALL and got.missing_samples == 0
    assert got.stamps == (_place(1),)
    assert got.first_offset_100ns == pytest.approx(_ms(step_ms), abs=1)
    assert got.hole_100ns == pytest.approx(_ms(step_ms + FOLLOWER_MS), abs=1)


def test_a_bigger_stall_spread_over_several_frames_is_one_stall():
    """A ~100 ms stall: +78.9, then -21.3 per frame; back within the tolerance at the third
    follower. Every frame before it is filed at its timeline place; no frame ever reads -58 ms
    BEHIND its late predecessor (that would be a restart)."""
    got = _resolve(_held(78.9, 57.6, 36.3))
    assert got.kind == pa.SENDER_STALL and got.missing_samples == 0
    assert got.stamps == (_place(1), _place(2))


def test_a_real_two_frame_loss_waits_for_the_full_lookahead_then_bridges_exactly():
    """Two frames never delivered: every later frame stays 42.7 ms ahead. Undecided until 1 + 4
    frames, then BRIDGE with exactly the 2048 lost samples."""
    lost = 2 * FRAME_MS
    for n in range(1, 1 + pa.STALL_LOOKAHEAD_FRAMES):
        assert _resolve(_held(*[lost] * n)) is None, n
    got = _resolve(_held(*[lost] * 5))
    assert got.kind == pa.BRIDGE and got.missing_samples == 2048
    assert got.stamps == (_place(1) + _ms(lost),)


def test_a_late_stamp_after_a_loss_bridges_only_the_lost_audio():
    """Two frames lost and the next one stamped 5 ms late (send jitter): the hole is the smallest
    offset (42.7 ms, never the 47.7 ms of the late stamp), and the late frame is filed at its
    timeline place behind the hole."""
    lost = 2 * FRAME_MS
    got = _resolve(_held(lost + 5.0, lost, lost, lost, lost))
    assert got.kind == pa.BRIDGE and got.missing_samples == 2048
    assert got.stamps == (_place(1) + _ms(lost),)
    assert got.first_offset_100ns == pytest.approx(_ms(lost + 5.0), abs=1)


@pytest.mark.parametrize("hole_ms, kind", [(250.0, pa.BRIDGE), (300.0, pa.DISCONTINUITY)])
def test_250_ms_is_bridged_and_300_ms_restarts(hole_ms, kind):
    got = _resolve(_held(*[hole_ms] * 5))
    assert got.kind == kind
    if kind == pa.DISCONTINUITY:
        assert got.missing_samples == 0 and got.stamps == (_place(1) + _ms(hole_ms),)
    else:
        assert got.missing_samples == 12000


def test_complete_decides_with_what_arrived():
    """The capture went quiet (or a frame that cannot join came): one held frame is a hole of its
    own offset, as before the look-ahead."""
    got = _resolve(_held(42.7), complete=True)
    assert got.kind == pa.BRIDGE and got.missing_samples == pa.bridge_samples(_ms(42.7), SR)


def test_a_stall_that_does_not_come_back_in_four_frames_is_a_hole_of_its_smallest_offset():
    """The documented limit: a ~150 ms stall (+128, then -21.3 per frame) is still 42.7 ms ahead
    after 4 followers, so 42.7 ms of zeros go in although nothing was lost (a holed span, rule A)."""
    got = _resolve(_held(128.0, 106.7, 85.3, 64.0, 42.7))
    assert got.kind == pa.BRIDGE and got.missing_samples == pa.bridge_samples(_ms(42.7), SR)
    assert len(got.stamps) == 4      # the four frames before the smallest one, at their place


def test_a_known_drop_before_a_stall_bridges_only_the_drop():
    """The sampler's own queue dropped 4 frames before the stalled one: the drop is a known hole
    (bridged exactly), the stall on top of it is not."""
    drop = 4 * FRAME_MS
    got = _resolve(_held(42.2, 21.1, drop0_ms=drop))
    assert got.kind == pa.SENDER_STALL and got.missing_samples == 4096
    assert got.stamps == (_place(1) + _ms(drop),)
    big = _resolve(_held(42.2, 21.1, drop0_ms=300.0))
    assert big.kind == pa.DISCONTINUITY


def test_a_follower_far_behind_is_a_stall_and_the_next_judgement_restarts():
    """+42 then -300: the follower is 'at or under the tolerance' (behind), so the first frame is a
    stall; the follower itself is judged normally against it and reads as a backward jump."""
    got = _resolve(_held(42.2, -258.0))
    assert got.kind == pa.SENDER_STALL and got.stamps == (_place(1),)
    follower_ts = _held(42.2, -258.0)[1][0]
    assert pa.frame_continues(got.stamps[-1], FRAME, SR, follower_ts, _tol()).kind == pa.DISCONTINUITY


def test_the_stamps_are_exact_100_ns_integers():
    """The live stamps (~1.8e16) do not fit a float exactly: the places are summed as integer
    differences."""
    got = _resolve(_held(42.2, 21.1))
    assert isinstance(got.stamps[0], int)
    assert got.stamps[0] - TS0 == round(FRAME_100NS)


@pytest.mark.parametrize("held, why", [
    ([], "no held frame"),
    ([(_place(1) + _ms(10.0), FRAME, 0.0)], "not ahead"),
    ([(_place(1) + _ms(42.7), FRAME, 0.0), (_place(2) + _ms(42.7), FRAME, 100.0)], "drop"),
    ([(pa.NDI_TIMESTAMP_UNDEFINED, FRAME, 0.0)], "no sender stamp"),
])
def test_resolve_ahead_refuses_misuse(held, why):
    with pytest.raises(ValueError, match=why):
        _resolve(held)


def test_ahead_of_timeline_is_beyond_the_tolerance_and_any_known_drop():
    tol = _tol()
    assert pa.ahead_of_timeline(TS0, FRAME, SR, _place(1) + _ms(42.2), tol)
    assert not pa.ahead_of_timeline(TS0, FRAME, SR, _place(1) + _ms(41.0), tol)
    assert not pa.ahead_of_timeline(TS0, FRAME, SR, _place(1) + _ms(42.2), tol, dropped_100ns=_ms(42.2))
    assert not pa.ahead_of_timeline(pa.NDI_TIMESTAMP_UNDEFINED, FRAME, SR, _place(1) + _ms(42.2), tol)


# ---------------------------------------------------------------------------------------------
# the sampler on real measurement audio (the real decoder shim)
# ---------------------------------------------------------------------------------------------


def _stall_frames(audio, at_s, steps_ms=STEP0_STALLS_MS, spacing_s=0.8):
    """The recorded stall: frame k stamped `step` late, the next two each -21.1 ms against their
    predecessor (so k+1 sits step - 21.1 and k+2 step - 42.2 off the timeline); nothing missing."""
    jitter = {}
    for j, step in enumerate(steps_ms):
        k = int(round((at_s + spacing_s * j) * SR)) // FRAME
        jitter[k] = _ms(step)
        jitter[k + 1] = _ms(step + FOLLOWER_MS)
        jitter[k + 2] = _ms(step + 2 * FOLLOWER_MS)
    return [_Block(SR, audio[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR + jitter.get(k, 0))
            for k, i in enumerate(range(0, audio.shape[0], FRAME))]


def test_the_recorded_stall_pattern_gives_no_zeros_no_unknown_no_foreign(decoder, rec_clip, tmp_path):
    """+42/-21/-21 (and the +45.6 / +42.0 ones) on real measurement audio through the real decoder:
    0 zeros, 0 UNKNOWN after the start-up warm-up, 0 FOREIGN, the chain on its line (bar a)."""
    blocks = _stall_frames(rec_clip, 3.3)
    payloads, lines = _run(blocks, tmp_path, decoder)
    assert _verdicts(payloads) == ["UNKNOWN", "UNKNOWN"] + ["MEASUREMENT"] * 4, lines
    assert payloads[-1]["holes_bridged"] == 0 and payloads[-1]["bridged_ms"] == 0.0
    assert payloads[-1]["sender_stalls"] == len(STEP0_STALLS_MS)
    assert payloads[-1]["last_foreign_ts_utc"] is None
    assert not any("bridged with" in line or "starts over" in line for line in lines), lines
    assert min(p["marker_chain"] for p in payloads if p["marker_chain"] is not None) >= pa.MARKER_CHAIN_MIN + 2


def test_the_replay_that_flapped_reads_measurement_after_the_warm_up(decoder, rec_clip, tmp_path):
    """The replay of the 16 live steps of 14:17:27-14:17:40 as sender stalls (STEP 0's shape): with
    rule A alone it read U U M M M M U M U U M (a flap stop); now every window after the start-up
    warm-up reads MEASUREMENT, with no zeros."""
    audio = np.concatenate([rec_clip, rec_clip])
    payloads, lines = _run_with_wall(_live_pattern(audio, "stall"), tmp_path, decoder)
    verdicts = _verdicts(payloads)
    assert verdicts[:2] == ["UNKNOWN", "UNKNOWN"]
    assert set(verdicts[2:]) == {"MEASUREMENT"}, [(p["verdict"], p["marker_chain"]) for p in payloads]
    assert payloads[-1]["holes_bridged"] == 0
    assert payloads[-1]["sender_stalls"] == 16
    assert not any("bridged with" in line for line in lines)


def test_a_real_two_frame_loss_is_still_bridged(decoder, rec_clip, tmp_path):
    """Two frames really missing, the next one stamped 5 ms late: bridged with exactly the lost
    2048 samples, the span kept, the verdicts those of the uncut clip."""
    ref, _ = _run([_Block(SR, rec_clip[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR)
                   for i in range(0, rec_clip.shape[0], FRAME)], tmp_path, decoder)
    k = int(round(4.1 * SR)) // FRAME
    blocks = [_Block(SR, rec_clip[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR + (_ms(5.0) if j == k + 2 else 0))
              for j, i in enumerate(range(0, rec_clip.shape[0], FRAME)) if j not in (k, k + 1)]
    payloads, lines = _run(blocks, tmp_path, decoder)
    assert _verdicts(payloads) == _verdicts(ref)
    assert payloads[-1]["holes_bridged"] == 1
    assert payloads[-1]["bridged_ms"] == pytest.approx(2 * FRAME_MS, abs=0.05)
    assert payloads[-1]["sender_stalls"] == 0
    bridged = [line for line in lines if "bridged with 2048 zero samples" in line]
    assert len(bridged) == 1 and "the marker span is kept" in bridged[0], lines


def test_a_300_ms_loss_still_restarts(rec_clip, tmp_path):
    """14 frames (298.7 ms) missing, the next one stamped on 300 ms: over the bridge -> one restart,
    one UNKNOWN window."""
    k = int(round(6.0 * SR)) // FRAME
    jitter = {k + 14: _ms(300.0 - 14 * FRAME_MS)}
    blocks = [_Block(SR, rec_clip[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR + jitter.get(j, 0))
              for j, i in enumerate(range(0, rec_clip.shape[0], FRAME)) if not k <= j < k + 14]
    payloads, lines = _run(blocks, tmp_path, NoMarkers())
    assert sum("timeline discontinuity" in line and "+300.0 ms" in line for line in lines) == 1, lines
    assert payloads[-1]["holes_bridged"] == 0 and payloads[-1]["sender_stalls"] == 0


ONSET_S = 6.5


@pytest.mark.parametrize("kind", ["pink", "chord"])
@pytest.mark.parametrize("stall_at", [ONSET_S - 1.3, ONSET_S - 0.3, ONSET_S - 0.02, ONSET_S + 0.3, ONSET_S + 1.6])
def test_music_around_a_stall_reads_foreign_in_the_same_window(decoder, rec_clip, tmp_path, kind, stall_at):
    """restreamer's safety check: measurement, then broadband music (pink) or an in-band chord, with
    a sender stall anywhere around the music's arrival. Nothing is bridged, so every window reads
    exactly what it reads without the stall: FOREIGN in the same window, the latch with the same
    payload. (The chord with the stall bridged read UNKNOWN until its span was free of zeros.)"""
    audio = np.concatenate([rec_clip[: int(ONSET_S * SR)], _music(kind, 16.0 - ONSET_S)])
    ref_blocks = [_Block(SR, audio[i:i + FRAME], TS0 + (i * NDI_UNITS) // SR) for i in range(0, audio.shape[0], FRAME)]
    ref, _ = _run(ref_blocks, tmp_path, decoder)
    assert "FOREIGN" in _verdicts(ref)
    stalled, lines = _run(_stall_frames(audio, stall_at, steps_ms=(42.2,)), tmp_path, decoder)
    assert not any("bridged with" in line for line in lines), lines
    assert stalled[-1]["sender_stalls"] == 1
    assert _verdicts(stalled) == _verdicts(ref), (kind, stall_at)
    assert _first_foreign(stalled) == _first_foreign(ref) is not None


# ---------------------------------------------------------------------------------------------
# counting, and every held frame decided (the stop, an error, the capture going quiet)
# ---------------------------------------------------------------------------------------------


def test_the_stalls_reach_the_summary_and_the_json(rec_clip, tmp_path, monkeypatch):
    monkeypatch.setattr(pas, "LOG_SUMMARY_S", 0.0)
    payloads, lines = _run(_stall_frames(rec_clip, 3.3), tmp_path, NoMarkers())
    summary = [line for line in lines if "program-audio summary" in line]
    assert sum(int(line.split("sender_stalls=")[1].split()[0]) for line in summary) == 3, summary
    assert max(float(line.split("max_stall_ms=")[1].split()[0]) for line in summary) == pytest.approx(45.6, abs=0.1)
    assert payloads[-1]["sender_stalls"] == 3


def test_the_payload_counter_is_null_when_not_sampling():
    from datetime import datetime, timezone

    p = pa.build_payload("UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", reason="sampler stopped")
    assert p["sender_stalls"] is None
    q = pa.build_payload("MEASUREMENT", -35.0, 15.0, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", marker_chain=7, sender_stalls=3)
    assert q["sender_stalls"] == 3
    with pytest.raises(ValueError):
        pa.build_payload("UNKNOWN", None, None, now=datetime.now(timezone.utc), window_s=2.0,
                         source="S", sender_stalls=-1)


class _Scripted:
    """A capture side that hands the consumer a fixed list of Captured items (None = a quiet poll)."""

    def __init__(self, items):
        self.items = list(items)

    def get(self, _timeout_ms):
        return self.items.pop(0) if self.items else None

    def qsize(self):
        return len(self.items)


def _audio_item(k, off_ms=0.0, t=None):
    x = np.full((FRAME, 2), 0.01, dtype=np.float32)
    return pac.Captured(_Block(SR, x, _place(k) + _ms(off_ms)), None, (100.0 + k * FRAME_MS / 1e3) if t is None else t, 0)


class _Rx:
    def connections(self):
        return 1


def _scripted_run(items, tmp_path, max_loops):
    payloads, lines = [], []
    pas.run(_Rx(), str(tmp_path), source="S", decoder=NoMarkers(), capture=_Scripted(items),
            mono=lambda: 200.0, max_loops=max_loops, on_write=payloads.append, log=lines.append)
    return payloads, lines


def test_a_frame_held_at_the_stop_is_decided_never_left_behind(tmp_path):
    """The last frame sits 45 ms ahead and the loop stops right after it: it is decided (a hole of
    its own offset) and taken in, so its audio completes the window it belongs to."""
    n = int(2.0 * SR) // FRAME + 1              # one whole window needs 94 frames of 1024
    items = [_audio_item(k) for k in range(n - 1)] + [_audio_item(n - 1, 45.0)]
    payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    assert any("bridged with" in line and "next 0 frame(s)" in line for line in lines), lines
    assert len([p for p in payloads if p.get("reason") != "sampler starting"]) == 1


def test_an_error_frame_decides_the_held_frames_first(tmp_path):
    """Frames held before an NDI error frame are real audio of the old span: decided (and logged)
    before the error restarts it."""
    items = [_audio_item(k) for k in range(10)] + [_audio_item(10, 45.0)]
    items.append(pac.Captured(None, ConnectionError("lost"), 101.0, None))
    _payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    bridge = next(i for i, line in enumerate(lines) if "bridged with" in line)
    error = next(i for i, line in enumerate(lines) if "lost" in line)
    assert bridge < error, lines


def test_a_quiet_capture_decides_the_held_frames(tmp_path):
    """A frame ahead, then a quiet poll (no item): the held frame is decided with what arrived (no
    follower), so frames after the quiet poll are judged against it, never joined to it."""
    items = [_audio_item(k) for k in range(10)] + [_audio_item(10, 45.0), None]
    items += [_audio_item(k, 45.0) for k in range(11, 16)]
    _payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    bridged = [line for line in lines if "bridged with" in line]
    assert len(bridged) == 1 and "next 0 frame(s)" in bridged[0], lines
    assert not any("starts over" in line for line in lines), lines


# ---------------------------------------------------------------------------------------------
# review round 1: the first frame's drop in a bridge / restart, the exact limits, a frame far ahead
# held, re-held groups (a frame that cannot join, the order of every frame)
# ---------------------------------------------------------------------------------------------


def test_a_bridge_or_a_restart_counts_the_first_frames_known_drop():
    """The first held frame carries a known queue drop of 4 frames and the timeline stays 60 ms
    ahead beyond it: the zeros are the drop and the hole, and the 250 ms limit applies to both."""
    drop = 4 * FRAME_MS
    got = _resolve(_held(60.0, 60.0, 60.0, 60.0, 60.0, drop0_ms=drop))
    assert got.kind == pa.BRIDGE
    assert got.missing_samples == pa.bridge_samples(_ms(drop) + _ms(60.0), SR)
    over = _resolve(_held(60.0, 60.0, 60.0, 60.0, 60.0, drop0_ms=200.0))
    assert over.kind == pa.DISCONTINUITY and over.missing_samples == 0


# 4800-sample frames are exactly 1 000 000 units of 100 ns, so a stamp can sit ON a limit (1024-sample
# frames never do: 213 333.3 units)
BIG = 4800
BIG_100NS = 1_000_000


def _big_held(*cum_100ns):
    return [(TS0 + (i + 1) * BIG_100NS + c, BIG, 0.0) for i, c in enumerate(cum_100ns)]


def _big_resolve(held, complete=False):
    return pa.resolve_ahead(TS0, BIG, SR, held, pa.continuity_tolerance_100ns(BIG, SR), complete=complete)


def test_the_limits_are_exact_to_the_100_ns_unit():
    tol = round(pa.continuity_tolerance_100ns(BIG, SR))
    assert tol == 1_200_000
    # a follower exactly ON the tolerance is back: a stall; one unit over is not
    assert _big_resolve(_big_held(tol + 1, tol)).kind == pa.SENDER_STALL
    assert _big_resolve(_big_held(tol + 1, tol + 1)) is None
    # a hole of exactly 250 ms is bridged, one unit more restarts
    limit = round(pa.hole_bridge_max_100ns())
    assert _big_resolve(_big_held(*[limit] * 5)).kind == pa.BRIDGE
    assert _big_resolve(_big_held(*[limit + 1] * 5)).kind == pa.DISCONTINUITY


def test_a_frame_stamped_300_ms_late_with_its_followers_on_the_timeline_is_a_stall(tmp_path):
    """A frame more than 250 ms AHEAD is held too (never an immediate restart): its followers on the
    timeline make it a stall, no restart, no zeros."""
    items = [_audio_item(k) for k in range(10)] + [_audio_item(10, 300.0)]
    items += [_audio_item(k) for k in range(11, 200)]      # 200 frames: a window gets written
    payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    assert not any("starts over" in line or "bridged with" in line for line in lines), lines
    assert payloads[-1]["sender_stalls"] == 1


def _regroup_items(tail):
    """f10 sits +45 ms ahead, f11 +90 ms (+45 against f10), then `tail`. The first group's smallest
    offset is f10's 45 ms: bridged, f10 at its own stamp; f11 is then +45 against it and is held
    again (a re-held group)."""
    return [_audio_item(k) for k in range(10)] + [_audio_item(10, 45.0), _audio_item(11, 90.0)] + tail


def test_a_re_held_group_keeps_every_frame_in_order(tmp_path):
    """f12 onward sit +50 ms: after f10's 45 ms bridge, f11 is held again and f12 (+5 against f10's
    place) brings it back: one stall. Every frame is taken in once, in order: 2 s windows complete
    on the expected sample count, never an error."""
    items = _regroup_items([_audio_item(k, 50.0) for k in range(12, 200)])
    payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    assert payloads[-1]["holes_bridged"] == 1 and payloads[-1]["sender_stalls"] == 1, lines
    assert payloads[-1]["bridged_ms"] == pytest.approx(45.0, abs=0.05)
    assert not any("starts over" in line for line in lines), lines
    taken_s = 200 * FRAME / SR + 0.045
    assert len([p for p in payloads if p.get("reason") != "sampler starting"]) == int(taken_s // 2.0)


def test_a_queue_drop_after_a_re_held_group_never_joins_it(tmp_path):
    """f12 carries a known queue drop: it cannot join, so the held frames are decided first -- the
    re-held f11 too -- and only then is f12 judged with its own drop (never a held frame with a drop)."""
    x = np.full((FRAME, 2), 0.01, dtype=np.float32)
    f12 = pac.Captured(_Block(SR, x, _place(13) + _ms(90.0)), None, 101.0, 0, dropped_frames=1,
                       dropped_100ns=FRAME_100NS)
    items = _regroup_items([f12] + [_audio_item(k, 90.0) for k in range(14, 30)])
    payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    bridged = [line for line in lines if "bridged with" in line]
    assert len(bridged) == 3, lines            # f10's hole, the re-held f11's, f12's dropped frame
    assert sum("next 0 frame(s)" in line for line in bridged) == 1, bridged   # f11, alone
    assert any("queue overflow: 1 frames" in line for line in bridged), bridged
    assert not any("starts over" in line for line in lines), lines


def test_a_frame_with_another_channel_count_never_joins(tmp_path):
    """f11 has one channel (a format change): it cannot join the held f10, so f10 is decided alone
    (next 0 frames) and f11 restarts at its format change."""
    mono = pac.Captured(_Block(SR, np.full((FRAME, 1), 0.01, dtype=np.float32), _place(11) + _ms(90.0)),
                        None, 101.0, 0)
    items = [_audio_item(k) for k in range(10)] + [_audio_item(10, 45.0), mono]
    _payloads, lines = _scripted_run(items, tmp_path, max_loops=len(items))
    bridged = [line for line in lines if "bridged with" in line]
    assert len(bridged) == 1 and "next 0 frame(s)" in bridged[0], lines
    assert any("format change" in line and "starts over" in line for line in lines), lines
