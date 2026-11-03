#!/usr/bin/env python3
"""issue 1404 -- is a CG source's audio IN the stream program? Pure (numpy), no NDI, no thread.

WHY: the CG arm (program_audio_cg.py) made ANY CG audio at or over -60 dBFS a FOREIGN `cg-audio`
(the 3.11.2026 Content ID claim: a CG song reached the stream program 15-20 dB under the cam2
marker, where the spectral share could not see it). The owner then cleaned mbc ("mbc upravene aby
bol len meraci signal"), but the CG box keeps playing (cg-obs -13.5 dBFS live), so the strict arm
refused every YouTube stage although the CG never reached the stream. The refinement (issue 1404,
the open checklist item): FOREIGN only when the CG audio is IN the program -- measured, never
assumed.

The measure: the normalized cross-correlation (NCC) of the CG source's audio against the received
stream program audio, lag-searched over the real path, both PRE-WHITENED (GCC-PHAT style: each
spectrum divided by its own smoothed magnitude) and band-limited to PASS_LO_HZ..PASS_HI_HZ with the
cam2 marker band (program_audio.BAND_LO_HZ..BAND_HI_HZ, 200-800 Hz) notched out of BOTH, so the
loud marker can never correlate and the comparison is the same band on both sides.
  * Whitening makes the statistic independent of the music's spectral tilt and of the FOH console's
    EQ, and makes the null distribution (CG absent) close to that of white noise: about
    1/sqrt(2 * bandwidth * duration) = 1/sqrt(2 * 2700 Hz * 4 s) = 0.0068 per lag.
  * The band is 800-3500 Hz: from the top of the marker band (so nothing below it either: no mains
    hum harmonics and no bass, which a CG and the room could share by accident) up to 3.5 kHz,
    where a sample-clock mismatch between the CG box and the program chain still keeps most of the
    coherence (+50 ppm on the mixed copy measured: the median NCC fell 0.21 -> 0.17).
  * Calibration (3.11.2026, dev1, frames of 1024 at 48 kHz, random offsets): the null -- live
    cg-obs (60 s read on strih-snv) and live SP-program (68 s) against the live stream program (68 s,
    the CG absent by the owner's mbc cleanup) and against the committed real measurement clip --
    peaked at 0.014-0.041 over 90 trials. Detection -- the same live cg-obs mixed into the live
    program 18 dB under the program level, 600 ms late: NCC 0.105-0.339 (median 0.21 over 20); 24
    dB under: median 0.11 (min 0.018, a quiet passage). DETECT_NCC = 0.07 = 1.7 x the null maximum,
    ~10 sd of the theory. A song whose passages sit more than ~24 dB under the program level can read ABSENT in a
    window; one PRESENT window latches FOREIGN for the guard's hold, and a song has loud passages.
    Re-calibrate only from real audio (the rule file has the recipe), never to pass a single run.
  * Mono CG (the channel mean) against EACH program channel, the best one: the measurement track
    carries the marker on L and R ~10 ms apart, but the marker band is notched, and the console may
    put the CG on one side only.

Time alignment: both rings hold NDI frames at their SDK `timestamp` (100 ns, the sender's
submission wall clock; both boxes run the fleet dantesync). A CG sample stamped c appears in the
program stamped p = c + lag, lag in [LAG_MIN_MS, LAG_MAX_MS]:
  * LAG_MAX_MS = 1500: the CG box -> FOH console / mbc -> Dante or VBAN -> stream OBS -> its NDI
    output path (Dante 1-5 ms, the stream OBS's audio buffering and the genlock hold of up to ~1 s
    on the stream box), with margin;
  * LAG_MIN_MS = -500: the CG sender's NDI copy can be stamped AFTER the same audio left the CG box
    physically (the cg OBS's own audio buffering grows 128-362 ms legitimately,
    .claude/rules/genlock-audio-buffering-floor.md), plus the two boxes' clock error.
The program segment is the trailing PROGRAM_SEGMENT_S of program audio whose CG counterpart over
the whole lag range is already buffered; a frame missing in either ring is zeros, and either
segment covered less than MIN_COVERAGE is not computable.

The decision (`judge`), per CG source the CG arm hears at or over the silence bar:
  PRESENT     peak NCC >= DETECT_NCC                  -> the CG audio IS in the program (FOREIGN)
  ABSENT      peak NCC <  DETECT_NCC                  -> it is not (the window keeps its verdict)
  UNDECIDED   not computable: no timestamps, too little CG or program audio buffered, coverage
              under MIN_COVERAGE, the CG segment under MIN_CG_DBFS (a near-silent CG never decides),
              the program segment digitally silent or its band empty, a non-finite sample
              -> the strict arm (FOREIGN `cg-audio`, fail closed)
"""
from __future__ import annotations

import math
import os
import sys
import threading
from collections import deque
from typing import NamedTuple

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402

CORR_FS = 8000              # the rate the correlation runs at (its band ends at PASS_HI_HZ)
PASS_LO_HZ = 800.0          # = the top of the marker band: no mains hum, no shared bass
PASS_HI_HZ = 3500.0
NOTCH_LO_HZ = pa.BAND_LO_HZ  # the cam2 marker band, never correlated
NOTCH_HI_HZ = pa.BAND_HI_HZ
WHITEN_SMOOTH_HZ = 40.0     # the magnitude smoothing the whitening divides by
CG_TAPER_S = 0.05           # the CG segment's raised-cosine edges (the program's: a full Hann window)
PROGRAM_SEGMENT_S = 4.0     # program audio correlated per window
LAG_MIN_MS = -500.0         # the CG's NDI copy stamped up to this much after its physical output
LAG_MAX_MS = 1500.0         # the longest CG -> program path (module doc)
MAX_SEGMENT_AGE_S = 3.0     # the correlated program segment may end at most this long before the newest program audio
MIN_COVERAGE = 0.95         # each segment's share of samples actually delivered
MIN_CG_DBFS = -50.0         # the CG segment's level below which the correlation never decides
MIN_BAND_DBFS = -90.0       # a side with less than this in the correlation band has nothing to compare
DETECT_NCC = 0.07           # the PRESENT threshold: ~1.8 x the live null maximum, ~10 sd (module doc)
RING_S = PROGRAM_SEGMENT_S + (LAG_MAX_MS - LAG_MIN_MS) / 1e3 + MAX_SEGMENT_AGE_S + 1.0
PRESENT = "PRESENT"
ABSENT = "ABSENT"
UNDECIDED = "UNDECIDED"
UNITS = pa.NDI_TIME_UNITS_PER_S


class Correlation(NamedTuple):
    """One CG source against the program: `kind` PRESENT / ABSENT / UNDECIDED, the peak NCC and its
    lag in ms (None when not computed), the program channel it was found on, and `why` (UNDECIDED)."""
    kind: str
    ncc: float | None = None
    lag_ms: float | None = None
    channel: int | None = None
    why: str | None = None


def _undecided(why: str) -> Correlation:
    return Correlation(UNDECIDED, why=why)


class AudioRing:
    """The last `seconds` of one source's NDI frames at their sender timestamps, bounded by the
    stamps (never by a count). `mono=True` keeps the channel mean (the CG side), else every channel
    (the program). Thread-safe: one writer (a capture thread or the consumer), one reader."""

    def __init__(self, seconds: float = RING_S, mono: bool = False):
        self.seconds = float(seconds)
        self.mono = mono
        self._lock = threading.Lock()
        self._frames: deque = deque()   # (timestamp, samples float32 (n, ch), sample_rate)
        self._bad_stamp = False         # the newest frame came without a sender timestamp

    def push(self, timestamp: int, samples, sample_rate: int) -> None:
        x = np.asarray(samples, dtype=np.float32)
        if x.ndim == 1:
            x = x[:, None]
        if self.mono and x.shape[1] > 1:
            x = x.mean(axis=1, keepdims=True, dtype=np.float32)
        with self._lock:
            if not pa.timestamp_defined(timestamp):
                self._bad_stamp = True  # the newest frame has no sender stamp: not alignable
                return
            if sample_rate <= 0 or x.shape[0] == 0:
                return
            self._bad_stamp = False
            self._frames.append((int(timestamp), x, int(sample_rate)))
            newest = int(timestamp)
            while self._frames and self._frames[0][0] < newest - self.seconds * UNITS:
                self._frames.popleft()

    def clear(self) -> None:
        with self._lock:
            self._frames.clear()
            self._bad_stamp = False

    def snapshot(self):
        """(frames list, the newest frame had no stamp): what judge() reads."""
        with self._lock:
            return list(self._frames), self._bad_stamp


def span(frames) -> tuple[int, int] | None:
    """(first stamp, end stamp of the newest frame) in 100 ns, None for no frames."""
    if not frames:
        return None
    ts, x, sr = frames[-1]
    return frames[0][0], ts + int(round(x.shape[0] * UNITS / sr))


def segment(frames, t0: int, t1: int) -> tuple[np.ndarray | None, float, int | None]:
    """The frames' samples placed by their stamps over [t0, t1) at the frames' own rate: (array
    (n, ch), coverage 0..1, sample rate). Frames of another rate than the newest are left out (zeros);
    an overlap keeps the later frame."""
    if not frames or t1 <= t0:
        return None, 0.0, None
    sr = frames[-1][2]
    ch = frames[-1][1].shape[1]
    n = int(round((t1 - t0) * sr / UNITS))
    out = np.zeros((n, ch), dtype=np.float32)
    have = np.zeros(n, dtype=bool)
    for ts, x, fsr in frames:
        if fsr != sr or x.shape[1] != ch:
            continue
        a = int(round((ts - t0) * sr / UNITS))
        b = a + x.shape[0]
        lo, hi = max(a, 0), min(b, n)
        if lo >= hi:
            continue
        out[lo:hi] = x[lo - a:hi - a]
        have[lo:hi] = True
    return out, float(have.mean()) if n else 0.0, sr


def _whiten(x: np.ndarray, sr: int, n_out: int, taper_s: float | None) -> np.ndarray | None:
    """Resample x (1-D, rate sr) to CORR_FS by FFT, divide its spectrum by its own smoothed
    magnitude and keep only PASS_LO..PASS_HI without NOTCH_LO..NOTCH_HI. None for an empty band.
    `taper_s`: the raised-cosine edge length; None = a full Hann window (the program side, below)."""
    n = x.shape[0]
    taper = n // 2 if taper_s is None else min(n // 20, int(taper_s * sr))
    w = np.ones(n)
    if taper > 1:
        ramp = 0.5 - 0.5 * np.cos(np.pi * np.arange(taper) / taper)
        w[:taper] = ramp
        w[n - taper:] = ramp[::-1]
    spec = np.fft.rfft(x.astype(np.float64) * w)
    nbins_out = n_out // 2 + 1
    spec = spec[:nbins_out] if spec.shape[0] >= nbins_out else np.pad(spec, (0, nbins_out - spec.shape[0]))
    freqs = np.arange(nbins_out) * (sr / n)
    mask = (freqs >= PASS_LO_HZ) & (freqs <= PASS_HI_HZ) & ~((freqs >= NOTCH_LO_HZ) & (freqs <= NOTCH_HI_HZ))
    mag = np.abs(spec)
    k = max(1, int(round(WHITEN_SMOOTH_HZ / (sr / n))))
    # a moving average over 2k+1 bins (the same as np.convolve(mag, ones/(2k+1), "same"), by a
    # cumulative sum: O(bins), not O(bins * k))
    c = np.concatenate(([0.0], np.cumsum(np.pad(mag, k))))
    smooth = (c[2 * k + 1:] - c[:-(2 * k + 1)]) / (2 * k + 1)
    if not mask.any():
        return None
    # The band's own mean square (Parseval over the one-sided spectrum): a side with nothing in the
    # correlation band would whiten its rounding noise to full scale, so it decides nothing.
    band_ms = 2.0 * float(np.sum(np.abs(spec[mask]) ** 2)) / (n * n)
    if not band_ms > 10 ** (MIN_BAND_DBFS / 10):
        return None
    floor = float(np.max(smooth[mask])) * 1e-6
    white = np.where(mask, spec / np.maximum(smooth, floor), 0.0)
    return np.fft.irfft(white, n_out)


def ncc_peak(prog: np.ndarray, prog_sr: int, cg: np.ndarray, cg_sr: int, k_lo: int, k_hi: int):
    """The best NCC of the whitened program channels against the whitened CG over CG offsets
    k_lo..k_hi (CORR_FS samples: program sample t pairs with CG sample t + k). Returns (ncc, k,
    channel) or None when a side has no band energy."""
    n_p = int(round(prog.shape[0] * CORR_FS / prog_sr))
    n_c = int(round(cg.shape[0] * CORR_FS / cg_sr))
    # The program segment takes a full Hann window: its loud in-band marker, cut at the segment's
    # edges, leaks a broadband edge pattern into the correlation band, and two such patterns (any
    # marker-like or tonal CG content against the program) correlated at 0.4 with 50 ms tapers
    # (tests/python/test_program_audio_cg_corr_1404.py). The CG side keeps short tapers so every lag
    # of the range sees its audio at full weight; its edges meet a program with no edge.
    cw = _whiten(cg.mean(axis=1) if cg.ndim == 2 else cg, cg_sr, n_c, CG_TAPER_S)
    if cw is None or n_c < n_p:
        return None
    k_lo, k_hi = max(0, k_lo), min(n_c - n_p, k_hi)
    if k_hi < k_lo:
        return None
    c2 = np.concatenate(([0.0], np.cumsum(cw * cw)))
    energy = c2[n_p:] - c2[:-n_p]          # energy[k] = sum cw[k:k+n_p]^2
    L = 1 << int(math.ceil(math.log2(n_c + n_p)))
    cf = np.fft.rfft(cw, L)
    best = None
    for ch in range(prog.shape[1]):
        pw = _whiten(prog[:, ch], prog_sr, n_p, None)
        if pw is None:
            continue
        pn = float(np.sqrt(np.dot(pw, pw)))
        if not pn > 0.0:
            continue
        r = np.fft.irfft(cf * np.conj(np.fft.rfft(pw, L)), L)[:n_c - n_p + 1]
        seg = r[k_lo:k_hi + 1] / (pn * np.sqrt(np.maximum(energy[k_lo:k_hi + 1], 1e-30)))
        i = int(np.argmax(seg))
        if best is None or seg[i] > best[0]:
            best = (float(seg[i]), k_lo + i, ch)
    return best


def _level_dbfs(x: np.ndarray) -> float:
    ms = float(np.mean(np.asarray(x, dtype=np.float64) ** 2)) if x.size else 0.0
    return 10.0 * math.log10(ms) if ms > 0.0 else pa.DIGITAL_SILENCE_DBFS


def judge(program_frames, cg_frames, *, program_bad_stamp: bool = False,
          cg_bad_stamp: bool = False) -> Correlation:
    """Is the CG source (its ring frames) in the program (its ring frames)? Rules in the module doc."""
    if program_bad_stamp or cg_bad_stamp:
        side = "program" if program_bad_stamp else "CG"
        return _undecided(f"the {side} audio carries no NDI sender timestamp")
    ps, cs = span(program_frames), span(cg_frames)
    if ps is None:
        return _undecided("no program audio buffered")
    if cs is None:
        return _undecided("no CG audio buffered")
    lag_min, lag_max = int(LAG_MIN_MS * 1e4), int(LAG_MAX_MS * 1e4)
    seg = int(PROGRAM_SEGMENT_S * UNITS)
    p1 = min(ps[1], cs[1] + lag_min)
    p0 = p1 - seg
    if ps[1] - p1 > MAX_SEGMENT_AGE_S * UNITS:
        return _undecided(f"the CG audio is {(ps[1] - cs[1]) / UNITS:.1f} s behind the program")
    c0, c1 = p0 - lag_max, p1 - lag_min
    if ps[0] > p0 or cs[0] > c0:
        return _undecided(f"too little audio buffered for a {PROGRAM_SEGMENT_S:g} s segment over the "
                          f"{LAG_MIN_MS:g}..{LAG_MAX_MS:g} ms lag range")
    prog, pcov, psr = segment(program_frames, p0, p1)
    cg, ccov, csr = segment(cg_frames, c0, c1)
    if prog is None or cg is None:
        return _undecided("no audio in the segment")
    if pcov < MIN_COVERAGE or ccov < MIN_COVERAGE:
        return _undecided(f"segment coverage program {pcov:.0%} CG {ccov:.0%} < {MIN_COVERAGE:.0%}")
    if not (np.all(np.isfinite(prog)) and np.all(np.isfinite(cg))):
        return _undecided("a non-finite sample")
    cg_db = _level_dbfs(cg)
    if cg_db < MIN_CG_DBFS:
        return _undecided(f"the CG segment at {cg_db:.1f} dBFS < {MIN_CG_DBFS:g} dBFS")
    if _level_dbfs(prog) <= pa.DIGITAL_SILENCE_DBFS:
        return _undecided("the program segment is digitally silent")
    # program sample t (time p0 + t/fs) pairs with CG sample t + k (time c0 + (t + k)/fs), so
    # lag = p - c = (p0 - c0) - k/fs = lag_max - k/fs  ->  k = (lag_max - lag) * fs
    k_lo = 0
    k_hi = int(round((lag_max - lag_min) / UNITS * CORR_FS))
    best = ncc_peak(prog, psr, cg, csr, k_lo, k_hi)
    if best is None:
        return _undecided("no energy in the correlation band")
    ncc, k, ch = best
    lag_ms = (lag_max / 1e4) - k * 1e3 / CORR_FS
    return Correlation(PRESENT if ncc >= DETECT_NCC else ABSENT, ncc, lag_ms, ch)
