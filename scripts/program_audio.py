#!/usr/bin/env python3
"""issue 1404 -- stream program-audio classification: is the stream program carrying ONLY the
measurement signal? Pure (numpy), no NDI, no network; the marker words come from the decoder
(scripts/program_audio_marker.py).

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

Marker requirement (ROZHODNUTÉ issue 1404 comments 6026577906 + 6026826572): the spectral share
is necessary, not sufficient -- tonal content inside 200-800 Hz has none outside the band. The only
property unique to the measurement is the cam2 QPSK marker itself, decoded by the dock's own
decoder (scripts/qpsk_guard_shim.cpp via scripts/program_audio_marker.py) over the trailing
MARKER_SPAN_S of contiguous non-silent audio, every channel on its own (never a downmix). A raw
CRC-valid word proves nothing (preamble + zero nibble + CRC-4 is only 12 bits per screen pass;
steady in-band tones yield runs of them), so per channel:
  1. same-index words less than MARKER_MIN_SEP_S apart are one marker;
  2. an index that appears again MARKER_MIN_SEP_S or more away is dropped: the emitter's index is
     frame_id mod 256 at 60 fps and wraps only every 256/60 = 4.27 s, longer than the span, while
     steady tones decode the same few indices again and again;
  3. the chain = the most remaining markers, MARKER_MIN_SEP_S apart, on one timecode line
     idx_j - idx_i == round(60 * (t_j - t_i)) (mod 256, +-MARKER_INDEX_TOL);
  4. MEASUREMENT needs chain >= MARKER_CHAIN_MIN.
`markers_decoded` (the most raw CRC-valid words any channel decoded, diagnostics) and `marker_chain`
are additive fields of program-audio.json.

Verdict (classify):
  UNKNOWN      a measurement is missing / not a number, or the window is in band but has no
               marker chain (the 4 s warm-up after a start or a span restart, a span cut by a
               silent window, no decoder) -- the sampler also writes UNKNOWN itself when no
               audio arrives at all
  SILENT       rms_dbfs < SILENT_RMS_DBFS (the spectral share of a noise floor means nothing)
  FOREIGN      spectral_foreign (outside_band_pct >= FOREIGN_OUTSIDE_BAND_PCT above the silent
               level) -- also during the warm-up, ROZHODNUTÉ 6027706292 item 1: only MEASUREMENT
               needs the marker chain -- or marker_chain < MARKER_CHAIN_MIN
  MEASUREMENT  otherwise (in band AND chain >= MARKER_CHAIN_MIN)

Receive continuity (design issue 1404 comment 6030385284, Approach 1): the marker span must never
stitch audio across a hole, and must not restart when nothing was lost. Arrival time cannot tell
the two apart: a sampler starved for ~1 s on a busy dev1 gets the SDK's queued audio in one late
burst (live 7.10.2026: 57 spurious `receive gap of 1.0-2.0 s` UNKNOWNs in 6 h), while audio lost
with blocks still arriving under 1 s apart showed no gap at all. So the sampler judges continuity on
the SENDER's audio timeline (frame_continues): every NDI audio frame carries the SDK `timestamp`
(100 ns, the sender's submission time, NDIlib_recv_timestamp_undefined = INT64_MAX when the SDK has
none) and its sample count, and
  expected = prev_timestamp + prev_samples / sample_rate
  |timestamp - expected| <= one frame duration + CONTINUITY_SLACK_S   CONTINUE (any arrival time)
  otherwise                                                           DISCONTINUITY (span restarts)
  either timestamp undefined (INT64_MAX, or <= 0)                     UNKNOWN_TS (the sampler falls
                                                                      back to the arrival gap)
STEP 0 (7.10.2026, 25 min / 70 304 frames of the live stream program, received by a second sampler
instance while dev1 was loaded; issue 1404 comment 6030714990): the sender's submission jitter
reached 24.5 ms against a tolerance of 41.3 ms (1024 samples at 48 kHz + 20 ms); every arrival gap
(the 1.25 s one the old rule restarted on, and 14 of 0.5-0.96 s) was a late burst on a continuous
timeline; one real 200 ms hole (its frames never delivered, the arrival gap only 0.19 s) was off it.
A dantesync DATE STEP moves the sender's wall clock, and so its timestamps, once: a step over the
tolerance reads as ONE discontinuity = one UNKNOWN warm-up window (never FOREIGN: a restarted span
is never judged as a short chain); a micro-correction of a few ms stays inside the tolerance.

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
MARKER CALIBRATION (7.10.2026, scripts/program_audio_marker_calibrate.py through the real sampler
loop and the real shim; the bars of ROZHODNUTÉ 6026577906):
  (a) real measurement audio, chain >= MARKER_CHAIN_MIN + 2 in every judged window and 0 FOREIGN:
      rec2 / rec3a / rec3b / session = 1847 judged windows, chain 6-8, minimum 6 (rec3b at
      292 s); the Task 1 fixtures 51 windows, minimum 6 (s3-A-vod's first span after the
      stream began). Margin: exactly two missed decodes.
  (b) synthetic in-band content with no marker, 50 trials x 10 windows, -30 and -15 dBFS: the
      worst chain held over 3 consecutive windows = chords 1, tremolo chords 1, melody 1,
      band-limited 200-800 Hz noise 3 -- all below 4, so every 3 windows hold a FOREIGN.
  Without rule 2 a held tremolo chord read a chain of 4 window after window (6026817074).
Known limits:
  * foreign content mixed well BELOW the measurement level is missed: pink noise under it reads
    32.3 % at -3 dB (FOREIGN) but 26.2 % at -6 dB. Music at program level is ~20 dB OVER the
    measurement and reads ~90 %.
  * in-band music mixed UNDER a marker that still decodes reads MEASUREMENT (the marker chain
    stands, the share stays in band); broadband music is caught by the spectral share. The
    measurement-clip-only rule for SongPlayer and the cg OBS (plan Task 5) is the control for it.
  * a sender stall longer than the tolerance (its audio thread submitting > ~20 ms late beyond its
    normal jitter) reads as a discontinuity although no sample was lost: one warm-up. STEP 0 saw
    no such stall in 25 min; the arrival-time rule it replaces cost a warm-up per dev1 stall.
  * a timestamp hole is seen only between two frames that both carry a timestamp; a sender
    without one (an SDK older than v2.5) falls back to the arrival gap and its known limit
    (audio lost without a 1 s arrival gap is stitched, review of 6027557132: 4 of 762 cut clips
    read one FOREIGN window).
Pinned by tests/python/test_program_audio_1404.py + test_program_audio_marker_1404.py +
test_program_audio_timeline_1404.py.
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
# -- the marker requirement (ROZHODNUTÉ issue 1404 comments 6026577906 + 6026826572; the rule and
#    its calibration in the module doc; pinned by tests/python/test_program_audio_marker_1404.py) --
MARKER_INDEX_RATE_HZ = 60.0   # the emitter's index = frame_id mod 256 at vr=60/1 (`# qpsk-params`)
MARKER_INDEX_MODULUS = 256    # the 8-bit index wraps every 256 / 60 = 4.27 s
MARKER_INDEX_TOL = 2          # +-2 indices around round(60 * dt): the emit jitter of +-1 frame, with margin
MARKER_MIN_SEP_S = 0.25       # one marker: same-index re-hits closer than this; distinct markers this far apart
MARKER_SPAN_S = 4.0           # the trailing span of contiguous non-silent audio; MUST stay < 256 / 60 s
MARKER_CHAIN_MIN = 4          # MEASUREMENT needs a chain this long (real audio min 6, in-band content < 4)
# -- the receive continuity (design issue 1404 comment 6030385284; the rule in the module doc;
#    pinned by tests/python/test_program_audio_timeline_1404.py) --
NDI_TIMESTAMP_UNDEFINED = 2**63 - 1   # NDIlib_recv_timestamp_undefined (Processing.NDI.structs.h)
NDI_TIME_UNITS_PER_S = 10_000_000     # NDI timestamps count 100 ns
CONTINUITY_SLACK_S = 0.020            # tolerance = one frame duration + this (continuity_tolerance_100ns)
CONTINUE = "CONTINUE"
DISCONTINUITY = "DISCONTINUITY"
UNKNOWN_TS = "UNKNOWN_TS"

VERDICTS = ("MEASUREMENT", "FOREIGN", "SILENT", "UNKNOWN")
SCHEMA = 1


def timestamp_defined(ts) -> bool:
    """A real NDI sender timestamp: an integer above 0 and below NDIlib_recv_timestamp_undefined
    (INT64_MAX, "the SDK has none"). 0 and anything that is not an integer read as undefined."""
    return (isinstance(ts, (int, np.integer)) and not isinstance(ts, bool)
            and 0 < int(ts) < NDI_TIMESTAMP_UNDEFINED)


def _frame_100ns(samples: int, sample_rate: int) -> float:
    if not samples > 0:
        raise ValueError(f"a frame needs samples > 0, got {samples!r}")
    if not sample_rate > 0:
        raise ValueError(f"a frame needs sample_rate > 0, got {sample_rate!r}")
    return samples * NDI_TIME_UNITS_PER_S / sample_rate


def continuity_tolerance_100ns(samples: int, sample_rate: int) -> float:
    """THE continuity tolerance (100 ns units): one frame duration (`samples` at `sample_rate`, the
    previous frame) + CONTINUITY_SLACK_S. 1024 samples at 48 kHz: 21.3 + 20 = 41.3 ms."""
    return _frame_100ns(samples, sample_rate) + CONTINUITY_SLACK_S * NDI_TIME_UNITS_PER_S


def timeline_offset_100ns(prev_ts: int, prev_samples: int, sample_rate: int, ts: int) -> float:
    """How far frame `ts` sits from where the sender's audio timeline puts it: ts minus
    (prev_ts + prev_samples / sample_rate), in 100 ns units. Both timestamps must be defined."""
    return int(ts) - (int(prev_ts) + _frame_100ns(prev_samples, sample_rate))


def frame_continues(prev_ts, prev_samples: int, sample_rate: int, ts, tolerance: float) -> str:
    """CONTINUE | DISCONTINUITY | UNKNOWN_TS: does the frame stamped `ts` continue the sender's
    audio timeline after the previous frame (stamped `prev_ts`, `prev_samples` long at
    `sample_rate`)? Within +-`tolerance` (100 ns; continuity_tolerance_100ns) of the expected
    stamp it continues, whatever its arrival time; farther away (a hole, a sender restart, a date
    step) it is a discontinuity. UNKNOWN_TS when either timestamp is undefined: the caller then
    falls back to the arrival gap."""
    if not tolerance >= 0:
        raise ValueError(f"frame_continues: tolerance {tolerance!r} must be >= 0")
    if not (timestamp_defined(prev_ts) and timestamp_defined(ts)):
        _frame_100ns(prev_samples, sample_rate)  # an invalid frame is refused here too
        return UNKNOWN_TS
    if abs(timeline_offset_100ns(prev_ts, prev_samples, sample_rate, ts)) <= tolerance:
        return CONTINUE
    return DISCONTINUITY


def _round_half_up(x):
    return np.floor(x + 0.5)


def marker_candidates(words) -> list[tuple[float, int]]:
    """Rules 1 + 2 over ONE channel's CRC-valid words `[(start_s, index), ...]` of one span: the
    candidate markers, one per index, in time order.

    1. Same-index words less than MARKER_MIN_SEP_S apart are ONE marker (a re-hit of one burst); it
       takes the earliest word's time.
    2. An index whose words lie MARKER_MIN_SEP_S or more apart repeats inside the span, so it is
       dropped entirely: a real marker's index advances 60/s and wraps only every 256/60 = 4.27 s,
       longer than the span, while steady in-band tones decode the same few indices again and again.
    Rule 2 is decided on the whole index (its first and last word), never by chaining re-hits, so a
    dense run of one index can never collapse into a single marker."""
    by_index: dict[int, list[float]] = {}
    for start_s, index in words:
        t = float(start_s)
        if not math.isfinite(t):
            raise ValueError(f"marker_candidates: word time {start_s!r} is not finite")
        by_index.setdefault(int(index) % MARKER_INDEX_MODULUS, []).append(t)
    out = [(min(ts), index) for index, ts in by_index.items() if max(ts) - min(ts) < MARKER_MIN_SEP_S]
    return sorted(out)


def marker_chain(words) -> int:
    """Rules 1-3 over ONE channel's CRC-valid words of one span: the longest timecode chain.

    3. A real marker's index is the emitter's frame_id mod 256 at MARKER_INDEX_RATE_HZ, so two real
       markers satisfy `idx_j - idx_i == round(60 * (t_j - t_i)) (mod 256, +-MARKER_INDEX_TOL)`. The
       chain is the largest number of candidates (rules 1 + 2) on one such timecode line -- each
       candidate in turn is the anchor the others are checked against -- counting only candidates
       at least MARKER_MIN_SEP_S apart, in time order."""
    cands = marker_candidates(words)
    if not cands:
        return 0
    t = np.asarray([c[0] for c in cands], dtype=np.float64)
    idx = np.asarray([c[1] for c in cands], dtype=np.int64)
    best = 0
    for a in range(t.shape[0]):
        expected = _round_half_up(MARKER_INDEX_RATE_HZ * (t - t[a])).astype(np.int64)
        d = np.mod(idx - idx[a] - expected, MARKER_INDEX_MODULUS)
        on_line = t[np.minimum(d, MARKER_INDEX_MODULUS - d) <= MARKER_INDEX_TOL]  # t is sorted
        count, last = 0, None
        for ts in on_line:
            if last is None or ts - last >= MARKER_MIN_SEP_S:
                count += 1
                last = ts
        best = max(best, count)
    return best


def span_markers(words_per_channel) -> tuple[int, int]:
    """(markers_decoded, marker_chain) of one span: every channel is read on its own (never a
    downmix, see the module doc) and the best one counts. `markers_decoded` = the most raw CRC-valid
    words any channel decoded (diagnostics only); `marker_chain` = the longest chain (rule 3)."""
    channels = list(words_per_channel)
    if not channels:
        raise ValueError("span_markers: no channels")
    return max(len(w) for w in channels), max(marker_chain(w) for w in channels)


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


def is_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def spectral_foreign(rms_dbfs, outside_band_pct) -> bool:
    """The window's spectrum alone says FOREIGN: a level at or above SILENT_RMS_DBFS and at least
    FOREIGN_OUTSIDE_BAND_PCT of the energy outside the marker band. Needs no marker span, so the
    sampler reports it during the warm-up too (ROZHODNUTÉ issue 1404 comment 6027706292 item 1)."""
    return (is_number(rms_dbfs) and rms_dbfs >= SILENT_RMS_DBFS
            and is_number(outside_band_pct) and outside_band_pct >= FOREIGN_OUTSIDE_BAND_PCT)


def classify(rms_dbfs, outside_band_pct, marker_chain) -> str:
    """MEASUREMENT | FOREIGN | SILENT | UNKNOWN (rules in the module doc). `marker_chain` is the
    trailing span's chain (span_markers), or None when there is none (warm-up, a span cut by
    silence, no decoder): None can never read MEASUREMENT."""
    if not is_number(rms_dbfs):
        return "UNKNOWN"
    if rms_dbfs < SILENT_RMS_DBFS:
        return "SILENT"
    if not is_number(outside_band_pct):
        return "UNKNOWN"
    if spectral_foreign(rms_dbfs, outside_band_pct):
        return "FOREIGN"
    if not isinstance(marker_chain, int) or isinstance(marker_chain, bool):
        return "UNKNOWN"
    if marker_chain < MARKER_CHAIN_MIN:
        return "FOREIGN"
    return "MEASUREMENT"


def _round1(v):
    return round(float(v), 1) if is_number(v) else None


def _count(v):
    if v is None:
        return None
    if not isinstance(v, int) or isinstance(v, bool) or v < 0:
        raise ValueError(f"a marker count must be a non-negative int, got {v!r}")
    return v


def build_payload(verdict: str, rms_dbfs, outside_band_pct, *, now: datetime, window_s: float,
                  source: str, reason: str | None = None,
                  last_foreign_ts_utc: str | None = None, markers_decoded: int | None = None,
                  marker_chain: int | None = None) -> dict:
    """The program-audio.json payload. `age_s` is 0.0 as written; the lease server recomputes it
    (and `last_foreign_age_s` from `last_foreign_ts_utc`, the FOREIGN latch) at every request
    (rig_serve_files.program_audio_response). `markers_decoded` (raw CRC-valid words, diagnostics)
    and `marker_chain` are additive fields, null when the window has no full marker span."""
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
        "markers_decoded": _count(markers_decoded),
        "marker_chain": _count(marker_chain),
    }
    if reason is not None:
        payload["reason"] = reason
    return payload


def write_payload(serve_dir: str, payload: dict) -> None:
    """Atomically replace `<serve_dir>/program-audio.json` (temp + rename; errors propagate)."""
    data = (json.dumps(payload) + "\n").encode("utf-8")
    rsf.write_bytes_atomic(os.path.join(serve_dir, rsf.PROGRAM_AUDIO_NAME), data)
