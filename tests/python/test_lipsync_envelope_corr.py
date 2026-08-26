"""issue 1192 -- offline unit tests for scripts/lipsync_envelope_corr.py's PURE functions
(rectified_envelope / pearson / best_loop_correlation). No ffmpeg, no numpy, no network -- synthetic
sine-envelope fixtures only, so the whole thing runs under Tier-0 `python -m pytest tests/python`.

The module is the content criterion for lipsync-test-mode.sh's speech-arrival VERIFY (issue 1174 ->
1192): the recorded mbc audio's amplitude ENVELOPE correlated against the local asset. The two
properties that make it the RIGHT signal (vs volumedetect, which false-passes because the mic-chain
AGC pumps ambient to the ceiling) are proven here:
  * a MATCHED probe (a wrapped slice of the looping asset envelope) -> correlation ~1.0, and it stays
    ~1.0 even when the probe is AMPLITUDE-SCALED and DC-SHIFTED (the AGC does exactly that to the
    recording) -- because the correlation is mean-subtracted + normalized (Pearson);
  * a DEAD probe (an uncorrelated, different-period envelope = "ambient, wrong content") -> low
    correlation at EVERY loop offset, comfortably below the 0.6 arrival threshold.
"""
import math
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import lipsync_envelope_corr as lec  # noqa: E402


# ---------------------------------------------------------------------------
# rectified_envelope
# ---------------------------------------------------------------------------
def test_rectified_envelope_averages_abs_over_each_window():
    # sample_rate 200 Hz, 20 ms window -> 4 samples per window.
    samples = [3, -4, 5, -6, 1, -1, 1, -1]
    env = lec.rectified_envelope(samples, sample_rate=200, win_ms=20)
    assert env == pytest.approx([4.5, 1.0])  # mean(|3|,4,5,6)=4.5 ; mean(1,1,1,1)=1.0


def test_rectified_envelope_drops_the_trailing_partial_window():
    # 5 samples, win=4 -> exactly one full window, the 5th sample dropped (every point same span).
    env = lec.rectified_envelope([2, -2, 2, -2, 99], sample_rate=200, win_ms=20)
    assert env == pytest.approx([2.0])


def test_rectified_envelope_empty_or_too_short_is_empty():
    assert lec.rectified_envelope([], sample_rate=8000, win_ms=20) == []
    # fewer than one 20 ms window (160 samples @ 8 kHz) -> no full window.
    assert lec.rectified_envelope([1, -1, 1], sample_rate=8000, win_ms=20) == []


def test_rectified_envelope_rejects_a_nonpositive_window():
    with pytest.raises(ValueError):
        lec.rectified_envelope([1, 2, 3], sample_rate=10, win_ms=20)  # 10*20/1000 = 0.2 -> rounds to 0


# ---------------------------------------------------------------------------
# pearson
# ---------------------------------------------------------------------------
def test_pearson_identical_is_one():
    a = [math.sin(i) for i in range(64)]
    assert lec.pearson(a, a) == pytest.approx(1.0)


def test_pearson_negated_is_minus_one():
    a = [math.sin(i) for i in range(64)]
    b = [-x for x in a]
    assert lec.pearson(a, b) == pytest.approx(-1.0)


def test_pearson_scale_and_offset_invariant():
    # Pearson is invariant to a positive scale + DC shift -- the exact property that makes the
    # arrival check robust to the mic-chain AGC (which scales/offsets the recorded envelope).
    a = [math.sin(i / 3.0) for i in range(200)]
    b = [7.0 * x + 4.0 for x in a]
    assert lec.pearson(a, b) == pytest.approx(1.0)


def test_pearson_flat_sequence_is_zero_not_a_crash():
    a = [math.sin(i) for i in range(32)]
    flat = [5.0] * 32
    assert lec.pearson(a, flat) == 0.0
    assert lec.pearson(flat, flat) == 0.0


def test_pearson_too_short_is_zero():
    assert lec.pearson([1.0], [2.0]) == 0.0
    assert lec.pearson([], []) == 0.0


def test_pearson_rejects_unequal_lengths():
    with pytest.raises(ValueError):
        lec.pearson([1.0, 2.0, 3.0], [1.0, 2.0])


# ---------------------------------------------------------------------------
# best_loop_correlation -- the arrival verdict
# ---------------------------------------------------------------------------
def _asset_env(n=3000, period=50.0):
    """A speech-like positive envelope: a raised sine (>=0, the shape of a rectified envelope)."""
    return [1.0 + math.sin(2.0 * math.pi * i / period) for i in range(n)]


def _wrapped_slice(env, offset, length):
    a = len(env)
    return [env[(offset + i) % a] for i in range(length)]


def test_best_loop_correlation_matched_wrapped_slice_is_one():
    asset = _asset_env()
    # a probe that is a slice of the LOOPING asset starting at an arbitrary offset (the real case:
    # mpv --loop-file=inf, the probe window lands at an arbitrary loop phase).
    probe = _wrapped_slice(asset, offset=1234, length=750)
    assert lec.best_loop_correlation(probe, asset) == pytest.approx(1.0, abs=1e-9)


def test_best_loop_correlation_matched_across_the_loop_boundary_is_one():
    asset = _asset_env()
    # a probe window that WRAPS the loop boundary (offset near the end) must still align to ~1.0.
    probe = _wrapped_slice(asset, offset=len(asset) - 100, length=750)
    assert lec.best_loop_correlation(probe, asset) == pytest.approx(1.0, abs=1e-9)


def test_best_loop_correlation_matched_survives_agc_scale_and_offset():
    asset = _asset_env()
    raw = _wrapped_slice(asset, offset=800, length=750)
    # the recorded mbc envelope is AGC-scaled + DC-shifted vs the asset -- the verdict must not care.
    agc = [12.0 * x + 3.0 for x in raw]
    assert lec.best_loop_correlation(agc, asset) == pytest.approx(1.0, abs=1e-9)


def test_best_loop_correlation_dead_probe_is_low_at_every_offset():
    asset = _asset_env(period=50.0)
    # a DEAD probe = ambient with the WRONG content: a different, incommensurate-period envelope.
    # Two different-frequency sines are ~uncorrelated at every phase, so the max over all loop
    # offsets stays well below the 0.6 arrival threshold.
    dead = [1.0 + math.sin(2.0 * math.pi * i / 31.0) for i in range(750)]
    corr = lec.best_loop_correlation(dead, asset)
    assert corr < 0.4, f"dead probe should be clearly sub-threshold, got {corr}"


def test_best_loop_correlation_threshold_separates_arrival_from_dead():
    # The end-to-end property the arrival gate relies on: with LIPSYNC_ARRIVAL_CORR_MIN=0.6, a
    # matched probe passes and a dead probe fails, from the SAME asset.
    asset = _asset_env()
    matched = _wrapped_slice(asset, offset=42, length=750)
    dead = [1.0 + math.sin(2.0 * math.pi * i / 31.0) for i in range(750)]
    assert lec.best_loop_correlation(matched, asset) >= 0.6
    assert lec.best_loop_correlation(dead, asset) < 0.6


def test_best_loop_correlation_empty_envelope_raises():
    with pytest.raises(ValueError):
        lec.best_loop_correlation([], [1.0, 2.0, 3.0])
    with pytest.raises(ValueError):
        lec.best_loop_correlation([1.0, 2.0], [])
