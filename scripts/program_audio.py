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
  rms_dbfs          10*log10(mean of x^2 over every sample of every channel); full scale = 1.0.
                    Digital zero reads DIGITAL_SILENCE_DBFS.
  outside_band_pct  100 * (energy outside [BAND_LO_HZ, BAND_HI_HZ]) / (energy >= SPECTRUM_FLOOR_HZ),
                    from a Hann-windowed FFT per channel with the channel POWERS summed. Never a
                    mono downmix: the measurement track carries the marker on L and R ~10 ms apart
                    and their sum comb-filters it. None when there is no energy above the floor.

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
Known limit: pink noise mixed UNDER the measurement reads 32.3 % at -3 dB (FOREIGN) but 26.2 % at
-6 dB (missed) -- content well below the measurement level is not caught; music at program level
(-20 dBFS and louder, ~15 dB over the measurement) is (59.8 % at +6 dB). Pinned by
tests/python/test_program_audio_1404.py.
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
    rms_dbfs = 10.0 * math.log10(mean_sq) if mean_sq > 0.0 else DIGITAL_SILENCE_DBFS
    n = x.shape[0]
    window = np.hanning(n)
    power = np.zeros(n // 2 + 1)
    for c in range(x.shape[1]):
        spec = np.fft.rfft(x[:, c] * window)
        power += spec.real * spec.real + spec.imag * spec.imag
    freqs = np.fft.rfftfreq(n, 1.0 / sample_rate)
    total = float(power[freqs >= SPECTRUM_FLOOR_HZ].sum())
    if not total > 0.0:
        return rms_dbfs, None
    in_band = float(power[(freqs >= BAND_LO_HZ) & (freqs <= BAND_HI_HZ)].sum())
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
                  source: str, reason: str | None = None) -> dict:
    """The program-audio.json payload. `age_s` is 0.0 as written; the lease server recomputes it
    at every request from `ts_utc` (rig_serve_files.program_audio_response)."""
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
    }
    if reason is not None:
        payload["reason"] = reason
    return payload


def write_payload(serve_dir: str, payload: dict) -> None:
    """Atomically replace `<serve_dir>/program-audio.json` (temp + rename; errors propagate)."""
    data = (json.dumps(payload) + "\n").encode("utf-8")
    rsf.write_bytes_atomic(os.path.join(serve_dir, rsf.PROGRAM_AUDIO_NAME), data)
