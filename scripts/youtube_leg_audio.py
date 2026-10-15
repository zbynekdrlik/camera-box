#!/usr/bin/env python3
"""Issue 1404 -- audio continuity of a YouTube VOD against stream OBS's recording (criterion 4).

Every 0.25 s block of the recording's audio (16 kHz mono) is found again in the VOD's audio by
normalised cross-correlation, tracking the lag block to block (ported from the session tool
audiocont.py). A downstream discontinuity shows as a lag jump (> 1.5 ms: audio dropped or
inserted), a low-correlation block (< 0.6 while the recording has signal), a silent VOD block
(< -70 dBFS), a level drop (> 10 dB below the window's median gain) or foreign sound (the recording
quiet, the VOD > 20 dB louder). The result also says how much of the window it really measured, so
a VOD whose audio ends early, or a window with no usable recording audio, is never a silent PASS.
"""
import os
import sys

import numpy as np

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_proc import run_bounded  # noqa: E402
from youtube_leg_timeline import clamp_window, vod_pts_for  # noqa: E402

SR = 16000
BLOCK = int(0.25 * SR)
FIRST_SEARCH = int(0.20 * SR)
TRACK_SEARCH = int(0.040 * SR)
LOCK_REF = int(4.0 * SR)
LOCK_SEARCH = int(2.5 * SR)
LAG_JUMP = int(round(0.0015 * SR))
LOW_CORR = 0.6
REC_SIGNAL_DBFS = -50.0
SILENT_REC_DBFS = -60.0
SILENT_VOD_DBFS = -70.0
LEVEL_DROP_DB = 10.0
FOREIGN_DB = 20.0  # a quiet recording block whose VOD block is this much louder (and has signal): foreign sound
AUDIO_LOAD_TIMEOUT_S = 900
DETAIL_LIMIT = 20


def load_audio(path, start_s=None, dur_s=None):
    """First audio track of a file as 16 kHz mono float32 (ffmpeg)."""
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error"]
    if start_s is not None:
        cmd += ["-ss", f"{start_s:.3f}"]
    cmd += ["-i", str(path)]
    if dur_s is not None:
        cmd += ["-t", f"{dur_s:.3f}"]
    cmd += ["-map", "0:a:0", "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"]
    raw = run_bounded(cmd, AUDIO_LOAD_TIMEOUT_S, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def drop_samples(x, at_s, ms):
    """Test helper: `ms` of audio lost at `at_s` (a dropped AAC frame)."""
    i, n = int(at_s * SR), int(round(ms / 1000.0 * SR))
    return np.concatenate([x[:i], x[i + n:]])


def dbfs(x):
    if not len(x):
        return -150.0
    r = float(np.sqrt(np.mean(np.square(x.astype(np.float64)))))
    return 20 * np.log10(r) if r > 0 else -150.0


def ncc_best(ref, seg):
    """(offset into seg, normalised correlation) of the best match of ref inside seg (FFT)."""
    ref = ref.astype(np.float64) - float(np.mean(ref))
    seg = seg.astype(np.float64) - float(np.mean(seg))
    m, n = len(ref), len(seg)
    nr = float(np.sqrt(np.dot(ref, ref)))
    if nr == 0 or n < m:
        return 0, 0.0
    size = 1 << (n + m - 1).bit_length()
    c = np.fft.irfft(np.fft.rfft(seg, size) * np.conj(np.fft.rfft(ref, size)), size)[: n - m + 1]
    e = np.concatenate([[0.0], np.cumsum(seg * seg)])
    den = nr * np.sqrt(np.maximum(e[m:] - e[:-m], 1e-20))
    r = c / den
    k = int(np.argmax(r))
    return k, float(r[k])


def audio_blocks(rec, vod, lag_s=0.0, start_s=0.0, end_s=None):
    """Block-by-block continuity of `vod` against `rec` (16 kHz mono arrays) over [start_s, end_s).

    lag_s = the expected vod-array time minus rec-array time of the same content (from the video tick
    map). A 4 s reference locks the real lag within +/-2.5 s; the lock starts where its whole search
    lies inside the VOD audio (at most 2.5 s into a window that starts at the VOD's first sample).
    Every 0.25 s block is then found again within +/-40 ms of the tracked lag (+/-200 ms after an
    unreliable block). Only reliable blocks (recording signal, correlation >= 0.6) move the lag,
    so one glitch is one low-corr block, never a cascade of lag jumps.

    Coverage fields: expected_blocks (from the lock start to the window end), blocks (measured),
    signal_blocks (the recording carries signal: the only blocks the criteria can judge),
    reliable_blocks (signal and matched), vod_ends_early_s (the VOD audio ran out before the window
    end) and rec_ends_early_s (the recording audio did)."""
    lag = int(round(lag_s * SR))
    want_end = len(rec) if end_s is None else int(round(end_s * SR))
    rec_short = max(0, want_end - len(rec)) / SR
    end = min(len(rec), want_end) - BLOCK
    i = max(int(round(start_s * SR)), LOCK_SEARCH - lag, 0)
    ref4 = rec[i: i + LOCK_REF]
    lo4 = max(0, i + lag - LOCK_SEARCH)
    seg4 = vod[lo4: i + lag + LOCK_REF + LOCK_SEARCH]
    if len(ref4) < LOCK_REF or len(seg4) < LOCK_REF:
        return {"error": "audio window too short for the 4 s lock"}
    k4, r4 = ncc_best(ref4, seg4)
    lag = lock_lag = lo4 + k4 - i
    start = i
    expected = max(0, (end - i) // BLOCK + 1)
    det = {"lag_jumps": [], "low_corr": [], "silent": [], "foreign": []}
    lags, gains, corr, recdb = [], [], [], []
    search, reliable_prev, vod_short, signal = FIRST_SEARCH, None, 0.0, 0
    while i <= end:
        ref = rec[i: i + BLOCK]
        if i + lag < 0 or i + lag + BLOCK > len(vod):
            vod_short = (end + BLOCK - i) / SR  # the VOD audio does not reach this far
            break
        lo, hi = max(0, i + lag - search), min(len(vod), i + lag + search + BLOCK)  # search clipped to the audio
        k, r = ncc_best(ref, vod[lo:hi])
        new_lag = lo + k - i
        rec_db = dbfs(ref)
        signal += 1 if rec_db > REC_SIGNAL_DBFS else 0
        reliable = rec_db > REC_SIGNAL_DBFS and r >= LOW_CORR
        at = i + (new_lag if reliable else lag)  # an unreliable match is no position: keep the tracked one
        vod_db = dbfs(vod[at: at + BLOCK])
        t = round(i / SR, 3)
        if rec_db > REC_SIGNAL_DBFS and r < LOW_CORR:
            det["low_corr"].append((t, round(r, 3), round(rec_db, 1)))
        if rec_db > SILENT_REC_DBFS and vod_db < SILENT_VOD_DBFS:
            det["silent"].append((t, round(rec_db, 1), round(vod_db, 1)))
        if rec_db <= REC_SIGNAL_DBFS and vod_db > REC_SIGNAL_DBFS and vod_db > rec_db + FOREIGN_DB:
            det["foreign"].append((t, round(rec_db, 1), round(vod_db, 1)))  # VOD sound the recording lacks
        if reliable:
            if reliable_prev is not None and abs(new_lag - reliable_prev) > LAG_JUMP:
                det["lag_jumps"].append((t, round((new_lag - reliable_prev) / SR * 1000, 2)))
            reliable_prev, lag, search = new_lag, new_lag, TRACK_SEARCH
            lags.append(new_lag)
            gains.append((t, vod_db - rec_db))
        else:
            search = FIRST_SEARCH
        corr.append(r)
        recdb.append(rec_db)
        i += BLOCK
    if not corr:
        return {"error": "no audio block in the window", "expected_blocks": expected,
                "vod_ends_early_s": round(vod_short, 3), "rec_ends_early_s": round(rec_short, 3)}
    med = float(np.median([g for _, g in gains])) if gains else 0.0
    drops = [(t, round(g - med, 1)) for t, g in gains if g - med < -LEVEL_DROP_DB]
    c = np.array(corr)
    return {"blocks": len(corr), "expected_blocks": expected, "signal_blocks": signal, "reliable_blocks": len(lags),
            "analysed_from_s": round(start / SR, 3), "vod_ends_early_s": round(vod_short, 3),
            "rec_ends_early_s": round(rec_short, 3),
            "lag_jumps": len(det["lag_jumps"]), "low_corr": len(det["low_corr"]),
            "silent": len(det["silent"]), "level_drops": len(drops), "foreign": len(det["foreign"]),
            "corr_median": round(float(np.median(c)), 3), "corr_p01": round(float(np.percentile(c, 1)), 3),
            "initial_lock_corr": round(r4, 3), "lag_vs_video_ms": round((lock_lag / SR - lag_s) * 1000, 1),
            "lag_range_ms": round((max(lags) - min(lags)) / SR * 1000, 2) if lags else None,
            "gain_db_median": round(med, 1), "rec_level_dbfs_median": round(float(np.median(recdb)), 1),
            "details": {"lag_jumps": det["lag_jumps"][:DETAIL_LIMIT], "low_corr": det["low_corr"][:DETAIL_LIMIT],
                        "silent": det["silent"][:DETAIL_LIMIT], "level_drops": drops[:DETAIL_LIMIT],
                        "foreign": det["foreign"][:DETAIL_LIMIT]}}


def audio_window(rec_audio, vod_audio, rec_rows, vod_rows, t0, a, b, rec_pts0=0.0, vod_pts0=0.0):
    """Audio continuity over content window [a, b): the start is clamped to the VOD's first frame
    (YouTube's live transition), the end is NOT -- audio needs no painter, so VOD audio that stops
    before `b` is measured as ending early, never cut off with the picture.

    rec_pts0 / vod_pts0 = the pts (recording / VOD timeline) of sample 0 of each audio array."""
    span = clamp_window(rec_rows, vod_rows, t0, a, b)
    if span is None or span[1] <= span[0]:
        return {"error": "the VOD has no frame of this window"}
    rec_p, vod_p = vod_pts_for(rec_rows, vod_rows, t0, span[0])
    if rec_p is None or vod_p is None:
        return {"error": "window start tick not found in the VOD"}
    res = audio_blocks(rec_audio, vod_audio, lag_s=(vod_p - vod_pts0) - (rec_p - rec_pts0),
                       start_s=rec_p - rec_pts0, end_s=(b - t0) - rec_pts0)
    res["start_utc"] = t0 + rec_p
    return res
