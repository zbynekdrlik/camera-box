#!/usr/bin/env python3
"""issue 1404 -- stream program-audio classification: is the stream program carrying ONLY the
measurement signal? Pure (numpy), no NDI, no network.

WHY: every YouTube test session must stop at once when non-measurement audio (music, a rehearsal)
is on the stream program -- copyrighted content on the channel risks a ban (owner amendment, issue
1404 comment 6016489928). A LEVEL bar cannot tell them apart: the healthy QPSK marker is loud
(peaks -11...-16 dBFS). Spectrally it is narrow: the marker (carrier 442 Hz) and its room sit in
200-800 Hz, music and speech do not. So the verdict is the share of spectral energy OUTSIDE that
band, gated by level (design: issue 1404 comment 6023622339, Approach 1).

Measurement per window (WINDOW_S = 2 s, any sample rate, mono or N channels):
  rms_dbfs          10*log10(mean of x^2 over every sample of every channel); full scale = 1.0;
                    minus the energy of the declared measurement tone lines (below). Digital zero
                    reads DIGITAL_SILENCE_DBFS.
  outside_band_pct  100 * (energy outside [BAND_LO_HZ, BAND_HI_HZ]) / (energy >= SPECTRUM_FLOOR_HZ),
                    both WITHOUT the declared measurement tone lines, from a Hann-windowed
                    FFT per channel with the channel POWERS summed. Never a mono downmix: the
                    measurement track carries the marker on L and R ~10 ms apart and their sum
                    comb-filters it (an anti-phase pair would cancel to zero). None when there is
                    no energy above the floor. A NaN/Inf sample makes the window UNKNOWN.
  MEASUREMENT_TONE_LINES_HZ  narrow lines (+-TONE_LINE_HALF_WIDTH_HZ) removed before measuring: the
                    plan's CG measurement clip (issue 1404 Task 5, the owner amendment) plays the
                    QPSK marker over a -30 dBFS 1 kHz tone bed, which would read ~84 % outside the
                    band. Removed from the level AND both sides of the share: counting it as
                    measurement instead would let the louder bed dilute foreign content under it.
                    A +-3 Hz line removes nothing measurable from broadband music. The clip
                    generator must import this constant (one parameter set for both sides).

Verdict (classify):
  UNKNOWN      a measurement is missing / not a number (the sampler also writes UNKNOWN itself
               when no audio arrives at all)
  SILENT       rms_dbfs < SILENT_RMS_DBFS (the spectral share of a noise floor means nothing)
  FOREIGN      outside_band_pct >= FOREIGN_OUTSIDE_BAND_PCT
  MEASUREMENT  otherwise

CALIBRATION (6.10.2026, the real session recordings in ~/.claude/work-products/issue-1404/audio/,
48 kHz stereo, every 2 s window): rec2 (604 windows), rec3a (324), rec3b (286), session (637) --
1851 measurement-only windows: outside_band_pct min 9.1 / median ~16 / p99 23.7 / max 25.3 %,
rms_dbfs -37.0 ... -34.9. (The design's "83 % in band, 3.8 % above 800 Hz" is the same audio; the
rest of the outside share is room rumble below 200 Hz, 5-21 % per window.) Generated foreign
content, never downloaded music: white noise 97.6 %, pink (broadband, music-like) 81.2 %,
speech-shaped noise 42.2 %. FOREIGN_OUTSIDE_BAND_PCT = 30 sits 4.7 points above the measurement
maximum and 12 below speech. SILENT_RMS_DBFS = -60 sits 23 dB under the quietest measurement window.
LIVE (6.10.2026, the real NDI path): the stream program (`STREAM-SNV (stream)`) read 12.4-22.1 %
at -35.9...-35.5 dBFS = MEASUREMENT; the SongPlayer program (`RESOLUME-SNV (SP-program)`, music)
read 87.6-94.0 % at -15.4...-14.6 dBFS = FOREIGN.
Known limits:
  * foreign content mixed well BELOW the measurement level is missed: pink noise under it reads
    32.3 % at -3 dB (FOREIGN) but 26.2 % at -6 dB. Music at program level is ~20 dB OVER the
    measurement and reads ~90 %.
  * tonal content that sits inside 200-800 Hz reads MEASUREMENT (a soft C-E-G chord 0.7 %, a
    220-440 Hz melody 10.1 %, the issue-1404 review). Real program music is broadband, but this
    class is not caught by the spectral share; the discriminator for it is the QPSK marker itself
    (a follow-up candidate, not this guard).
Pinned by tests/python/test_program_audio_1404.py.
"""
from __future__ import annotations

import json
import math
import os
from datetime import datetime

import numpy as np

import rig_serve_files as rsf

# -- the ONE constants block (calibrated above; pinned by tests/python/test_program_audio_1404.py) --
BAND_LO_HZ = 200.0
BAND_HI_HZ = 800.0
SPECTRUM_FLOOR_HZ = 20.0
WINDOW_S = 2.0
SILENT_RMS_DBFS = -60.0
FOREIGN_OUTSIDE_BAND_PCT = 30.0
MEASUREMENT_TONE_LINES_HZ = (1000.0,)
TONE_LINE_HALF_WIDTH_HZ = 3.0
DIGITAL_SILENCE_DBFS = -200.0

VERDICTS = ("MEASUREMENT", "FOREIGN", "SILENT", "UNKNOWN")
SCHEMA = 1


def analyse(samples, sample_rate: int) -> tuple[float, float | None]:
    """(rms_dbfs, outside_band_pct) of one window. `samples`: shape (n,) or (n, channels)."""
    x = np.asarray(samples, dtype=np.float64)
    if x.ndim == 1:
        x = x[:, None]
    if x.ndim != 2 or x.shape[0] < 2:
        raise ValueError(f"analyse: need (n,) or (n, channels) with n >= 2, got shape {x.shape}")
    mean_sq = float(np.mean(x * x))
    if not math.isfinite(mean_sq):
        return float("nan"), None  # a NaN/Inf sample: never SILENT, never a number -> UNKNOWN
    if not mean_sq > 0.0:
        return DIGITAL_SILENCE_DBFS, None
    n = x.shape[0]
    window = np.hanning(n)
    power = np.zeros(n // 2 + 1)
    for c in range(x.shape[1]):
        spec = np.fft.rfft(x[:, c] * window)
        power += spec.real * spec.real + spec.imag * spec.imag
    freqs = np.fft.rfftfreq(n, 1.0 / sample_rate)
    # The declared measurement tone lines are taken OUT first -- out of the level and out of both
    # sides of the share -- so a tone bed neither hides foreign content (by inflating the measured
    # side) nor counts against the program. A program that is only the bed reads as its remainder.
    tone = np.zeros(freqs.shape, dtype=bool)
    for line in MEASUREMENT_TONE_LINES_HZ:
        tone |= np.abs(freqs - line) <= TONE_LINE_HALF_WIDTH_HZ
    all_power = float(power.sum())
    rest_power = float(power[~tone].sum())
    if not rest_power > 0.0:
        return DIGITAL_SILENCE_DBFS, None
    rms_dbfs = 10.0 * math.log10(mean_sq * rest_power / all_power)
    counted = (freqs >= SPECTRUM_FLOOR_HZ) & ~tone
    total = float(power[counted].sum())
    if not total > 0.0:
        return rms_dbfs, None
    in_band = float(power[counted & (freqs >= BAND_LO_HZ) & (freqs <= BAND_HI_HZ)].sum())
    return rms_dbfs, 100.0 * (1.0 - in_band / total)


def _is_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def classify(rms_dbfs, outside_band_pct) -> str:
    """MEASUREMENT | FOREIGN | SILENT | UNKNOWN (rules in the module doc)."""
    if not _is_number(rms_dbfs):
        return "UNKNOWN"
    if rms_dbfs < SILENT_RMS_DBFS:
        return "SILENT"
    if not _is_number(outside_band_pct):
        return "UNKNOWN"
    if outside_band_pct >= FOREIGN_OUTSIDE_BAND_PCT:
        return "FOREIGN"
    return "MEASUREMENT"


def _round1(v):
    return round(float(v), 1) if _is_number(v) else None


def build_payload(verdict: str, rms_dbfs, outside_band_pct, *, now: datetime, window_s: float,
                  source: str, reason: str | None = None,
                  last_foreign_ts_utc: str | None = None) -> dict:
    """The program-audio.json payload. `age_s` is 0.0 as written; the lease server recomputes it
    (and `last_foreign_age_s` from `last_foreign_ts_utc`, the FOREIGN latch) at every request
    (rig_serve_files.program_audio_response)."""
    if verdict not in VERDICTS:
        raise ValueError(f"unknown verdict {verdict!r}")
    payload = {
        "schema": SCHEMA,
        "ts_utc": rsf.format_ts_utc(now),
        "age_s": 0.0,
        "verdict": verdict,
        "rms_dbfs": _round1(rms_dbfs),
        "outside_band_pct": _round1(outside_band_pct),
        "window_s": float(window_s),
        "source": source,
        "last_foreign_ts_utc": last_foreign_ts_utc,
    }
    if reason is not None:
        payload["reason"] = reason
    return payload


def write_payload(serve_dir: str, payload: dict) -> None:
    """Atomically replace `<serve_dir>/program-audio.json` (temp + rename; errors propagate)."""
    data = (json.dumps(payload) + "\n").encode("utf-8")
    rsf.write_bytes_atomic(os.path.join(serve_dir, rsf.PROGRAM_AUDIO_NAME), data)
