#!/usr/bin/env python3
"""Issue 1387 -- the REPORT-ONLY measurement points of the YouTube stage between the stream OBS and
the VOD.

The VOD shows audio steps (`lag_jumps`, 3-18 ms) at YouTube's 5 s segment starts that the stream OBS
recording does not have. To find where a step is born, the stage judges each point it can read with
the same tool as the VOD (scripts/youtube_leg_verdict.py: the painter-tick join, the 0.25 s audio
block search, the cam2 QPSK marker A/V) against the same stream recording:

  restreamer_input   what restreamer received from the stream OBS (scripts/youtube_leg_tap.py)
  restreamer_output  what restreamer's delivery VPS pushed to YouTube: not tapped (OUTPUT_UNTAPPED)

A lag jump's time is the RECORDING's audio time (s) at every point, so the same step shows the same
time at each point it reaches. The block's `points` never change its verdict (`report_only`).
"""
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_tap import OUTPUT_UNTAPPED  # noqa: E402

INPUT, OUTPUT = "restreamer_input", "restreamer_output"
LABELS = {INPUT: "vstup restreamera (lokálne RTMP na stream boxe)",
          OUTPUT: "výstup restreamera (delivery VPS → YouTube)",
          "vod": "YouTube VOD"}
SUB = "inpoint"  # the point's dir on the measurement host and in the dev1 stage dir
CAPTURE_NAME = "inpoint-capture.mkv"


def lag_jump_details(audio):
    """[[recording time s, step ms], ...] of one window's audio block, [] when none / unread."""
    det = ((audio or {}).get("details") or {}).get("lag_jumps") or []
    return [[t, ms] for t, ms in det if isinstance(t, (int, float)) and isinstance(ms, (int, float))]


def point_digest(result, tool_exit):
    """One point's per-window A/V offset and audio steps from the tool's youtube-leg-verdict.json."""
    wins = []
    for w in (result or {}).get("windows") or []:
        au, av, ds = w.get("audio") or {}, w.get("av") or {}, w.get("dupskip") or {}
        wins.append({"name": w.get("name"), "errors": w.get("errors") or [],
                     "dup": ds.get("dup"), "skip": ds.get("skip"),
                     "audio_lag_jumps": au.get("lag_jumps"), "audio_lag_jump_details": lag_jump_details(au),
                     "audio_lag_vs_video_ms": au.get("lag_vs_video_ms"), "audio_error": au.get("error"),
                     "av_delta_ms": av.get("delta_ms"), "av_point_ms": av.get("vod_ms"), "av_rec_ms": av.get("rec_ms"),
                     "av_error": av.get("error")})
    return {"measured": True, "report_only": True, "tool_overall": (result or {}).get("overall"),
            "tool_exit": tool_exit, "reasons": list((result or {}).get("reasons") or []), "windows": wins}


def not_measured(reason):
    return {"measured": False, "report_only": True, "reason": reason}


def points(input_point, tap=None):
    """The block's `points`: the restreamer input (measured or why not) + the untapped output."""
    out = {INPUT: input_point, OUTPUT: not_measured(OUTPUT_UNTAPPED)}
    if tap:
        out[INPUT]["capture"] = {k: tap.get(k) for k in ("url", "cpus", "priority", "how", "bytes", "path")
                                 if tap.get(k) is not None}
    return out


def input_skip_reason(tap, tool_ran):
    """Why the restreamer-input point cannot be judged in this run, or None."""
    if not tap:
        return "no restreamer-input capture in this run"
    if not tap.get("started"):
        return f"the restreamer-input capture did not start: {tap.get('reason') or '?'}"
    if not tap.get("stopped"):
        return f"the restreamer-input capture was never stopped cleanly: {tap.get('stop_reason') or '?'}"
    if not tap.get("bytes"):
        return f"the restreamer-input capture is empty (ffmpeg: {tap.get('ffmpeg_log') or 'no log'})"
    if not tool_ran:
        return "the YouTube verdict never ran on the measurement host, so this point was not judged either"
    return None
