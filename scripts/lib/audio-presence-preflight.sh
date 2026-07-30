#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects), mirrors the sibling
# scripts/lib/imag-gpu-guard.sh / scripts/lib/capture-rate-guard.sh convention.
#
# scripts/lib/audio-presence-preflight.sh — pre-record measurement-audio presence preflight (#748).
#
# #748 root cause (live, 2026-07-13, SECOND occurrence of the same class): the fused gate ran a
# full ~300s cycle with the measurement audio DEAD SILENT (stream program recording audio track:
# mean/max -91.0 dB, run 29272685333 / RUN_ID 237189640) and reported it only as a quiet per-camera
# `av_sync verdict: "unknown", candidates: 0` — no loud failure, no alert. The previous occurrence
# (mbc Ableton mic channel muted) went UNNOTICED FOR A WEEK. The measurement audio (the mbc chain:
# speaker -> church-PA mic -> mbc -> Dante -> stream OBS) is the instrument the whole A/V-sync leg
# reads; a silent instrument makes every A/V number meaningless, and the run should never proceed
# on it.
#
# This preflight makes a SHORT probe recording on the stream box (the mbc measurement mic rides the
# stream program recording), runs ffmpeg volumedetect on the box, and FAILS FAST with an
# operator-actionable message when the track is silent — the same fail-fast-before-burning-a-run
# philosophy as the #365 frozen-camera gate, the #709 GPU-VRAM preflight, and the #405 render-budget
# gate. A preflight guarding the full run is the sanctioned exception to the one-full-test rule.
#
# The DECISION LOGIC (parse the measured level, classify silent vs audible, compose the messages,
# build the remote commands) lives here as pure functions so it is Tier-0 unit-testable
# (tests/harness_audio_presence_preflight.rs); the recording-e2e.sh step is a thin caller.
#
# Digital-silence reference: a fully silent track measures ~-91 dB (the run 237189640 failure); a
# live QPSK marker measured ~-5.4 dB after the user's fix (2026-07-13 21:21). The -60 dB default
# threshold sits with wide margin between the two — well above silence, far below any real signal.
#
# Source-only: this file defines pure functions and performs no side effects on its own.

# audio_preflight_volumedetect_ps WIN_PATH -> the PowerShell command text (for win_ssh_run) that
# runs ffmpeg volumedetect on the probe recording ON the stream box and streams volumedetect's
# output back. ffmpeg writes the `max_volume: -X.X dB` line to stderr; `2>&1` merges it into the
# stream win_ssh_run captures. `-f null NUL` is the Windows null sink (no output file written).
audio_preflight_volumedetect_ps() {
  local win_path="$1"
  printf 'ffmpeg -hide_banner -nostats -i "%s" -af volumedetect -f null NUL 2>&1' "$win_path"
}

# audio_preflight_delete_ps WIN_PATH -> the PowerShell command text (for win_ssh_run) that deletes
# the throwaway probe recording on the stream box. Best-effort (SilentlyContinue) — a failed delete
# leaves a small orphan the next run's StartRecord path handles, it must never abort the run.
audio_preflight_delete_ps() {
  local win_path="$1"
  printf 'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "%s"' "$win_path"
}

# audio_preflight_parse_max_db FFMPEG_OUTPUT -> the parsed max_volume dB number (e.g. "-5.4" or
# "-91.0") from ffmpeg volumedetect's output, or empty + non-zero exit when no max_volume line is
# present (ffmpeg missing/errored, no audio stream, empty output). An unparseable value is NEVER
# silently treated as "audio present" — the caller turns an empty parse into the DISTINCT
# unreadable diagnostic (mirrors imag_gpu_free_mib_from_query's "unreadable is never a silent pass"
# convention). Takes the LAST max_volume line (a multi-pass ffmpeg could print more than one).
audio_preflight_parse_max_db() {
  local out="$1" db
  db="$(printf '%s\n' "$out" \
        | grep -aoE 'max_volume:[[:space:]]*-?[0-9]+(\.[0-9]+)?[[:space:]]*dB' \
        | tail -1 \
        | grep -aoE '\-?[0-9]+(\.[0-9]+)?' \
        | head -1)"
  [ -z "$db" ] && return 1
  printf '%s\n' "$db"
}

# audio_preflight_is_silent DB [THRESHOLD_DB=-60] -> "true"/"false". Silent (a fail) iff the
# measured DB is STRICTLY below the threshold — a track exactly at the threshold is treated as
# audible, not silent. Float-safe (levels are like -5.4 / -91.0) via awk. DB MUST already be a
# validated numeric (the caller routes an unparseable value to the unreadable diagnostic first).
audio_preflight_is_silent() {
  local db="$1" threshold="${2:--60}"
  if awk -v d="$db" -v t="$threshold" 'BEGIN { exit !((d + 0) < (t + 0)) }'; then
    echo "true"
  else
    echo "false"
  fi
}

# audio_preflight_silent_message DB [THRESHOLD_DB=-60] -> the operator-facing fail message, a pure
# string formatter (no I/O) so it is directly unit-testable — names the measured level, the
# threshold, and the exact chain link to check (the mbc Ableton mic channel + Dante routing).
audio_preflight_silent_message() {
  local db="$1" threshold="${2:--60}"
  echo "measurement audio SILENT — the stream program recording measured max_volume ${db} dB (< ${threshold} dB threshold; digital silence is ~-91 dB, a live QPSK marker reads ~-5 dB). The mbc measurement-mic chain is dead: check the mbc Ableton mic channel is UNMUTED and the Dante routing into stream OBS (targets.md mbc row has the checklist). Fix the chain, then re-run — the A/V-sync leg reads this audio, so a run on a silent measurement instrument burns a full cycle for nothing (#748)."
}

# audio_preflight_unreadable_message -> the fail message for when ffmpeg volumedetect produced no
# parseable max_volume at all (distinct from "silent but readable" — never conflate the two).
audio_preflight_unreadable_message() {
  echo "measurement-audio preflight could not parse a max_volume from ffmpeg volumedetect on the stream box — cannot confirm the mbc measurement chain is audible before recording. Check that ffmpeg is on PATH on the stream box and that the short probe recording was written (#748)."
}

# audio_preflight_norec_message -> the fail message for when the stream box returned no recording
# path from StopRecord (the probe recording never actually started — an OBS-WS / encoder problem).
audio_preflight_norec_message() {
  echo "measurement-audio preflight: the stream box returned NO recording path from StopRecord — the short probe recording never started (OBS WebSocket / encoder problem on stream). Cannot confirm the mbc measurement chain before the real run (#748)."
}
