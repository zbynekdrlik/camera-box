"""#1143 -- imag E2E recording must not perturb the measurement: pure record-encoder logic.

The imag E2E records its OBS PROGRAM for the topology-v2 zero-loss verdict. Recording it with the
SOFTWARE x264 encoder overloads the 30W-PL1-clamped imag-nb (render past its 16.67ms budget ->
~18.4% OBS "lagged" frames -> ~19.5% repeated recorded frames = observer effect, #1130). The fix is
the Intel iGPU HARDWARE encoder ffmpeg_vaapi_tex (FFmpeg VAAPI H.264, TEXTURE/zero-copy -- no CPU
frame download, no CPU encode), LIVE-PROVEN this session to record valid H.264 High 1080p60 with
render held at ~4ms / ~0% lagged.

This module holds ONLY the PURE, side-effect-free logic (Tier-0 pytest-testable -- Rust is CI-only
under #557): the encoder DECISION, the VAAPI recordEncoder.json settings, and the OBS-log parsers.
The stop/start + WebSocket + ssh orchestration that APPLIES the choice lives in the harness
(imag_scenes.py ensure-rec-encoder + recording-e2e.sh), never here.
"""
from __future__ import annotations

import re

# The record-encoder ids OBS advertises. NVENC for a discrete-NVIDIA box (unchanged from #502/#847);
# VAAPI-tex is the Intel-iGPU HW fix (#1143); x264 is the software fallback of last resort.
NVENC_ENCODER = "obs_nvenc_h264_tex"
VAAPI_TEX_ENCODER = "ffmpeg_vaapi_tex"
SOFTWARE_ENCODER = "obs_x264"

# The DRM render node the Intel iGPU exposes headless (root:render; `newlevel` is in the render
# group). vainfo confirms iHD H.264 EncSliceLP here.
VAAPI_RENDER_NODE = "/dev/dri/renderD128"

# CQP quantizer for the measurement recording. The consult: on the EncSliceLP (low-power) entrypoint
# CBR/VBR need HuC firmware, but CQP does not -- AND CQP preserves the QR/high-contrast-burn detail
# the recording is decoded for (disk is covered by the #1122 retention sweep). qp 18-22 is the
# detail-safe band; 20 is the default.
VAAPI_DEFAULT_QP = 20

# FFmpeg H.264 High profile int (FF_PROFILE_H264_HIGH) -- what OBS's ffmpeg_vaapi encoder took for
# the live-proven "High 1080p60" file.
_H264_HIGH_PROFILE = 100


def choose_record_encoder(has_discrete_nvidia: bool, available_encoders=None) -> str:
    """The pure record-encoder decision for THIS box's hardware + OBS build.

    - discrete NVIDIA GPU present -> NVENC (byte-for-byte unchanged from #502/#847).
    - no dGPU (the imag-nb Intel-iGPU case) -> ffmpeg_vaapi_tex when it is available, else x264.
      ``available_encoders`` is the set OBS advertised (parse_available_encoders); ``None`` means
      "not probed" -> trust the vendored Intel bundle, which always ships ffmpeg_vaapi_tex (so
      seed_profile, which has no log to probe, still seeds VAAPI).
    - NEVER QSV (obs_qsv11_v2/hevc): #847 live-proved it fails at Init() on this box/build
      (libmfx Texture-interop MFX_ERR_UNSUPPORTED), so it is never a valid choice here.
    """
    if has_discrete_nvidia:
        return NVENC_ENCODER
    if available_encoders is None or VAAPI_TEX_ENCODER in available_encoders:
        return VAAPI_TEX_ENCODER
    return SOFTWARE_ENCODER


def vaapi_record_encoder_settings(qp: int = VAAPI_DEFAULT_QP) -> dict:
    """The recordEncoder.json body OBS's advanced-output ffmpeg_vaapi_tex encoder reads (the file
    WebSocket SetProfileParameter cannot write -- it must be placed on disk while OBS is DOWN).
    CQP on the render node; no B-frames; High profile -- the live-proven shape."""
    return {
        "vaapi_device": VAAPI_RENDER_NODE,
        "rate_control": "CQP",
        "qp": int(qp),
        "keyint_sec": 2,
        "bf": 0,
        "profile": _H264_HIGH_PROFILE,
    }


# A "  - <id> (Human Name)" line inside OBS's "Available Encoders:" -> "  Video Encoders:" block.
_ENCODER_LINE = re.compile(r"-\s+([A-Za-z0-9_]+)\s+\(")


def parse_available_encoders(obs_log_text: str) -> set:
    """The set of VIDEO encoder ids OBS registered, read from the "Available Encoders:" ->
    "Video Encoders:" block of a fresh-start OBS log. Audio encoders (the "Audio Encoders:"
    sub-block) are excluded. Empty set when the block is absent."""
    out: set = set()
    in_video = False
    for line in obs_log_text.splitlines():
        if "Available Encoders:" in line:
            in_video = False
            continue
        if "Video Encoders:" in line:
            in_video = True
            continue
        if "Audio Encoders:" in line:
            in_video = False
            continue
        if in_video:
            m = _ENCODER_LINE.search(line)
            if m:
                out.add(m.group(1))
    return out


_DRAWN = re.compile(r"Total drawn frames:\s*(\d+)(?:\s*\((\d+)\s*attempted\))?")
_LAGGED = re.compile(r"Number of lagged frames[^:]*:\s*(\d+)\s*\(([\d.]+)%\)")
_RENDER_MS = re.compile(r"program-render-audit:.*avg_frame_ms=([\d.]+)")


def parse_obs_record_stats(log_text: str, recording_basename: str = None) -> dict | None:
    """Parse OBS's own record-session render accounting from a slice of the OBS log covering the
    record window. Handles BOTH observed shapes:

    - x264 (observer-effect) shape: ``Total drawn frames: N (M attempted)`` + an explicit
      ``Number of lagged frames due to rendering lag/stalls: L (P%)`` line.
    - VAAPI-clean shape: ``Total drawn frames: N`` with NO "(M attempted)" and NO lagged line at
      all (OBS omits it when there are ~no lagged frames) -> lagged 0, pct 0.0, attempted = drawn.

    Also returns ``max_render_ms`` -- the maximum program-render-audit ``avg_frame_ms`` seen in the
    slice (#1143 Task 4: the render budget measured DURING the active recording, report-only).

    ``recording_basename`` (optional) restricts parsing to the window of that one .mkv (from its
    "Writing file '...<basename>'" line to its "stopped" line) so a slice carrying several records
    still attributes the right one. Returns ``None`` when no stop-stats block is present.
    """
    text = log_text
    if recording_basename:
        window = _slice_record_window(log_text, recording_basename)
        if window is not None:
            text = window

    dm = _DRAWN.search(text)
    if not dm:
        return None
    drawn = int(dm.group(1))
    attempted = int(dm.group(2)) if dm.group(2) is not None else drawn

    lm = _LAGGED.search(text)
    if lm:
        lagged = int(lm.group(1))
        lagged_pct = float(lm.group(2))
    else:
        lagged = 0
        lagged_pct = 0.0

    render_ms = [float(x) for x in _RENDER_MS.findall(text)]
    max_render_ms = max(render_ms) if render_ms else None

    return {
        "drawn_frames": drawn,
        "attempted_frames": attempted,
        "lagged_frames": lagged,
        "lagged_pct": lagged_pct,
        "max_render_ms": max_render_ms,
    }


def _slice_record_window(log_text: str, recording_basename: str) -> str | None:
    """The log lines from the recording's "Writing file '...<basename>'" up to (and including) the
    few stop-stats lines after its "Output of file '...<basename>' stopped". ``None`` when the
    window cannot be located (caller then parses the whole slice)."""
    lines = log_text.splitlines()
    start = None
    for i, ln in enumerate(lines):
        if "Writing file" in ln and recording_basename in ln:
            start = i
            break
    if start is None:
        return None
    stop = None
    for i in range(start, len(lines)):
        if "Output of file" in lines[i] and recording_basename in lines[i] and "stopped" in lines[i]:
            stop = i
            break
    if stop is None:
        return "\n".join(lines[start:])
    # include a small tail so the drawn/attempted/lagged lines (emitted just after "stopped") are in
    return "\n".join(lines[start : stop + 8])


def record_encoder_apply_plan(current_encoder: str, target_encoder: str, record_json_ok: bool) -> str:
    """Decide whether the harness must (re)apply the record encoder to make ``target_encoder`` LIVE.

    OBS only rebuilds the record encoder at (re)start / ResetOutputs -- a WebSocket param change on
    an already-current profile does NOT go live (proven #1143). So the make-it-live restart is
    needed ONLY when something is off on disk:

    - "ok"    -> already live: RecEncoder already IS the target AND (for VAAPI) recordEncoder.json is
                 present+correct; OBS booted into it. No restart.
    - "apply" -> RecEncoder differs from target, OR the VAAPI target lacks its recordEncoder.json
                 (OBS would fall back to VAAPI defaults, not the CQP settings). Write config +
                 restart (WS RecEncoder FIRST -> stop -> write json -> start).

    ``record_json_ok`` is only meaningful for the VAAPI target (x264/NVENC read no recordEncoder.json
    the harness manages), so it is ignored when the target is not VAAPI.
    """
    if current_encoder != target_encoder:
        return "apply"
    if target_encoder == VAAPI_TEX_ENCODER and not record_json_ok:
        return "apply"
    return "ok"
