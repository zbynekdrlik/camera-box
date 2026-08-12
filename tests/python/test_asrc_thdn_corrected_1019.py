"""camera-box #1019 -- regression test locking in the frequency-aware corrected THD+N measurement.

issue 1019 set out to redesign how continuous ASRC compensation is applied, because issues
929/1016 measured -18.19 dB THD+N while `swr_set_compensation()` is reissued every audio
callback. Investigating this ticket found the ACTUAL root cause is NOT the reissue mechanism at
all: `analyze_thdn.thdn()` assumes the resampled OUTPUT tone sits at exactly `--freq` (997 Hz),
which is only true when the resampler is NOT actively compensating. Once a nonzero ppm
compensation is held for the whole analysis window, the output is genuinely time-warped by that
ppm (measured: for a resampler producing `out_frames` output samples from `in_frames_nominal`
input samples of a `test_freq`-Hz tone, the tone's OWN output-domain frequency is EXACTLY
`test_freq * in_frames_nominal / out_frames`, not `test_freq`) -- so `thdn()`'s rectangular window,
sized for a whole number of `test_freq`-Hz cycles, is no longer coherent, and leaks tens of dB of
spectral energy across nearby bins. That leakage is what issues 929/1016 measured and attributed
to "distortion from reissuing" -- see scripts/asrc-quality-bench/RESULTS-1019.md for the full
empirical proof, including a BYTE-FOR-BYTE identical output signal between reissuing every
callback and reissuing exactly ONCE with a window wide enough to cover the whole run (proving the
reissue CADENCE itself changes nothing about the signal, for an unchanged target).

This test locks in `analyze_thdn.thdn_corrected()` / `thdn_segmented()`: a SYNTHETIC, provably-
clean frequency-shifted tone (built directly with no libswresample/C-harness dependency --
portable, fast, deterministic, no compiler needed in CI) must measure as clean under the
corrected method, distinguishing it from the naive method which is fooled by exactly this shift.
"""

import sys
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts" / "asrc-quality-bench"))
import analyze_thdn as at  # noqa: E402

SAMPLE_RATE = 48000
TEST_FREQ = 997.0
# Comfortably above EVERY corrected/segmented measurement in RESULTS-1019.md (worst case -51 dB
# for a fully constant ppm=30 case; realistic walking-ppm segments measured -58..-85 dB), and
# comfortably below the naive method's measured -15..-22 dB for the SAME realistic ppm range --
# see RESULTS-1019.md's full table. This is a "genuinely clean, not the old -18dB story" bar, not
# a claim of literal transparency.
REAL_DISTORTION_THRESHOLD_DB = -40.0


def _synthetic_shifted_tone(ppm, duration_s=10.0, amp=0.891, inflate_length=False):
    """A pure sine at the frequency a `ppm`-compensated resampler ACTUALLY produces in its output
    (`test_freq / (1 + ppm/1e6)`, RESULTS-1019.md's own derivation) -- i.e. exactly what a
    genuinely CLEAN (zero real distortion) compensated signal looks like. No resampler, no C
    harness involved -- deterministic and portable, matching this directory's existing pure-numpy
    test style (test_av_sync_*.py etc.).

    `inflate_length=True` ALSO reproduces the real resampler's OWN output-length inflation
    (measured: out_frames/in_frames == 1 + ppm/1e6, RESULTS-1019.md) -- required for
    thdn_corrected(), which derives true_freq purely from the (in_frames_nominal, len(samples))
    ratio, exactly like a real captured file would. thdn_segmented()'s blind per-segment
    estimate needs no such inflation -- it never looks at length ratios."""
    true_freq = TEST_FREQ / (1.0 + ppm / 1e6)
    nominal_n = int(round(duration_s * SAMPLE_RATE))
    n = int(round(nominal_n * (1.0 + ppm / 1e6))) if inflate_length else nominal_n
    t = np.arange(n) / SAMPLE_RATE
    return (amp * np.sin(2.0 * np.pi * true_freq * t)).astype(np.float32), true_freq


def test_naive_thdn_is_fooled_by_a_resample_frequency_shift():
    """Documents the DEFECT this ticket root-caused: thdn() assumes test_freq exactly, so a
    provably-clean tone that is merely SHIFTED by a realistic ppm reads as severely "distorted"
    even though there is not a single extra harmonic anywhere in the signal. This is what issues
    929/1016's own -15..-22dB figures actually were."""
    samples, _true_freq = _synthetic_shifted_tone(ppm=60.0)  # realistic high end of #1019's own range
    thdn_db, _pct, _frms, _rms, _freqs, _power, _fbin = at.thdn(samples, SAMPLE_RATE, TEST_FREQ)
    assert thdn_db > REAL_DISTORTION_THRESHOLD_DB, (
        f"expected the naive method to be FOOLED (report worse than {REAL_DISTORTION_THRESHOLD_DB} dB) "
        f"for a merely-shifted, provably-clean tone -- got {thdn_db:.2f} dB. If this now passes, "
        f"thdn() itself changed -- re-check RESULTS-1019.md's premise."
    )


@pytest.mark.parametrize("ppm", [5.0, 30.0, 60.0, 300.0])
def test_corrected_thdn_recognizes_a_shifted_tone_as_clean(ppm):
    """thdn_corrected(), given the KNOWN nominal input length, must recognize the SAME class of
    signal the naive test above is fooled by as clean (real THD+N, no shift-leakage) across the
    realistic 5-300ppm range RESULTS-1019.md covers -- and must recover the exact true frequency
    analytically, not just guess close."""
    samples, true_freq = _synthetic_shifted_tone(ppm=ppm, inflate_length=True)
    in_frames_nominal = int(round(10.0 * SAMPLE_RATE))
    thdn_db, _pct, measured_true_freq, _n = at.thdn_corrected(
        samples, SAMPLE_RATE, TEST_FREQ, in_frames_nominal=in_frames_nominal)
    assert thdn_db < REAL_DISTORTION_THRESHOLD_DB, (
        f"ppm={ppm}: corrected method should recognize a provably-clean shifted tone as clean "
        f"(< {REAL_DISTORTION_THRESHOLD_DB} dB) -- got {thdn_db:.2f} dB"
    )
    # thdn_corrected() derives true_freq from the INTEGER output-length ratio, so a fixture built
    # by rounding a fractional inflated length to the nearest sample (inflate_length=True above)
    # recovers true_freq only up to that same integer-rounding precision, not exactly.
    assert abs(measured_true_freq - true_freq) < 1e-3


def test_segmented_thdn_tracks_a_walking_ppm_target():
    """A genuinely time-varying (walking) compensation target -- built as several segments at
    DIFFERENT (but each locally constant) frequencies, concatenated -- has no single global
    true_freq the way the constant-ppm case above does, so thdn_segmented() must independently
    recognize EACH segment as clean via its own local frequency estimate."""
    seg_s = 1.0
    ppms = [30.0, 40.0, 50.0, 60.0]  # RESULTS-1019.md's own live-observed realistic walk range
    samples = np.concatenate([_synthetic_shifted_tone(ppm=ppm, duration_s=seg_s)[0] for ppm in ppms])

    segs = at.thdn_segmented(samples, SAMPLE_RATE, TEST_FREQ, seg_s=seg_s, skip_s=0.0)
    assert len(segs) == len(ppms)
    for seg, ppm in zip(segs, ppms):
        assert seg["thdn_db"] < REAL_DISTORTION_THRESHOLD_DB, (
            f"segment at ppm={ppm} should measure clean (< {REAL_DISTORTION_THRESHOLD_DB} dB) -- "
            f"got {seg['thdn_db']:.2f} dB"
        )
