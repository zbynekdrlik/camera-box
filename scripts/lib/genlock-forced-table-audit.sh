#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function lib (defines functions only, no top-level
# statements) -- matches the sibling scripts/lib/*.sh convention (cg-chain-verify.sh,
# v4l2-neutral.sh, asio-starve-health.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file runs it in the CALLER's shell, so strict mode here would leak into whichever
# caller sources it. The caller sets its own strict mode.
#
# scripts/lib/genlock-forced-table-audit.sh -- #1303 part 4: the REPORT-ONLY per-box-class
# certified-table AUDIO-parity classifier for the genlock deploy preflight.
#
# It is a byte-for-byte bash REPLICA of the canonical Rust table src/genlock_forced_table_audit.rs
# (the cg-chain-verify.sh shell-replica-pinned-to-Rust idiom); tests/genlock_forced_table_audit_1303.rs
# pins the two together over a fixed vector set so they can never drift. Given a box class and a
# `name<TAB>ndi_audio<TAB>yuv_range<TAB>yuv_colorspace` TSV on stdin (enumerated over OBS-WS by the
# deploy preflight, GetInputList + GetInputSettings), it prints a per-input verdict + summary so a
# program-audio box (cg OBS: sp-*/cg/music inputs) is never shipped with ndi_audio=false (the
# #1303 event-morning live defect) and a camera box is never shipped with audio bleeding into the
# mixer. It NEVER writes and NEVER exits non-zero on a mismatch -- it is print-only, never a gate.

# genlock_forced_table_is_camera NAME -> rc 0 iff NAME is a camera NDI input (a `(usb)` capture-card
# suffix, or "cam" + a digit). Mirror of is_camera_input in the Rust module.
genlock_forced_table_is_camera() {
  local n
  n="$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')"
  case "$n" in
    *'(usb)'*) return 0 ;;
  esac
  case "$n" in
    *cam*) case "$n" in *[0-9]*) return 0 ;; esac ;;
  esac
  return 1
}

# genlock_forced_table_is_program NAME -> rc 0 iff NAME is a program/music/SongPlayer input whose
# audio must be enabled. Mirror of is_program_audio_input in the Rust module (same 9 keys).
genlock_forced_table_is_program() {
  local n k
  n="$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')"
  for k in "sp-" "songplayer" "pgm" "program" "hudba" "mbc" "vban" "ndiar" "cg"; do
    case "$n" in *"$k"*) return 0 ;; esac
  done
  return 1
}

# genlock_forced_table_expected BOX NAME -> "audio" | "silent". Camera inputs are silent on every
# box; program inputs are audio on every box; otherwise the box-class default (resolume -> audio,
# strih/stream/imag -> silent). Mirror of expected_audio in the Rust module.
genlock_forced_table_expected() {
  local box="${1:-}" name="${2:-}"
  if genlock_forced_table_is_camera "$name"; then
    echo "silent"; return 0
  fi
  if genlock_forced_table_is_program "$name"; then
    echo "audio"; return 0
  fi
  case "$box" in
    resolume) echo "audio" ;;
    *)        echo "silent" ;;
  esac
}

# genlock_forced_table_verdict BOX NAME NDI_AUDIO -> OK | MISMATCH-PROGRAM-SILENT |
# MISMATCH-CAMERA-AUDIBLE. NDI_AUDIO is "true"/"false" (case-insensitive). Mirror of audio_verdict.
genlock_forced_table_verdict() {
  local box="${1:-}" name="${2:-}" ndi_audio exp
  ndi_audio="$(printf '%s' "${3:-}" | tr '[:upper:]' '[:lower:]')"
  exp="$(genlock_forced_table_expected "$box" "$name")"
  if [ "$exp" = "audio" ] && [ "$ndi_audio" = "false" ]; then
    echo "MISMATCH-PROGRAM-SILENT"; return 0
  fi
  if [ "$exp" = "silent" ] && [ "$ndi_audio" = "true" ]; then
    echo "MISMATCH-CAMERA-AUDIBLE"; return 0
  fi
  echo "OK"
}

# genlock_forced_table_audit BOX  (stdin: name<TAB>ndi_audio<TAB>yuv_range<TAB>yuv_colorspace lines)
#   Prints a per-input verdict line + a summary. REPORT-ONLY: never writes, ALWAYS returns 0 even on
#   a mismatch (the deploy preflight must never block the swap). A program input with a forced
#   yuv_range=partial gets a report-only NOTE (colour-shift risk on a full-range sender).
genlock_forced_table_audit() {
  local box="${1:-}" name ndi_audio yuv_range exp verdict mism=0 total=0
  echo "# genlock forced-table AUDIO audit (report-only, #1303 part 4) -- box=${box}"
  # 4th read field (yuv_colorspace) is caught by `_` and intentionally not classified.
  while IFS=$'\t' read -r name ndi_audio yuv_range _; do
    [ -n "$name" ] || continue
    total=$((total + 1))
    exp="$(genlock_forced_table_expected "$box" "$name")"
    verdict="$(genlock_forced_table_verdict "$box" "$name" "$ndi_audio")"
    local note=""
    if [ "$exp" = "audio" ] && [ "$(printf '%s' "${yuv_range:-}" | tr '[:upper:]' '[:lower:]')" = "partial" ]; then
      note="  NOTE yuv_range=partial on a program source (verify the sender's declared range)"
    fi
    case "$verdict" in
      OK) : ;;
      *)  mism=$((mism + 1)) ;;
    esac
    echo "${name}: expected=${exp} ndi_audio=${ndi_audio} -> ${verdict}${note}"
  done
  echo "# summary: ${total} input(s), ${mism} MISMATCH"
  return 0
}
