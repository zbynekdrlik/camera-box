#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only) -- sourcing executes it in the caller's shell,
# so strict mode here would leak into the dev1 watchdogs that source it (ci-testing-gotchas.md).
#
# scripts/lib/program-audio-rx.sh -- issue 1460: which program-audio RECEIVER a managed OBS box's
# program takes, from its BOX FACTS (scripts/strih-boxes/<box>.env), never a literal box list. The dev1
# audio-mixer watchdog's PROGRAM-RX arm grades exactly that receiver's entry of the `:8899` `audio_rx`
# facet (scripts/bundle_state_audio_rx.py).
#
#   program_audio_rx_key NAME -> stdout + rc:
#     `obs-vban:<STRIH_PROGRAM_AUDIO_INPUT>`  STRIH_PROGRAM_VBAN_STREAMS != none (the obs-vban source,
#                                             issue 1439: stream-pp `Zvuk z mixu`, strih-pp `ASIO zvuk`)
#     `dante`                                 a declared Dante program device (issue 1485 / 1495:
#                                             strih-snv `strih-obs`, stream-snv `stream-obs`)
#     nothing, rc 0                           the box has no fact file (resolume: no program receiver)
#                                             or its facts declare neither
#     nothing, rc 1                           the box HAS a fact file that does not load (never read as
#                                             "no receiver": the arm reports UNKNOWN)
#   The facts load in a SUBSHELL (the stream_program_dante_input precedent): the caller never gets a
#   loaded box.

program_audio_rx_key() {
  local name="${1-}" dir="${BASH_SOURCE[0]%/*}"
  (
    command -v strih_box_load >/dev/null 2>&1 || . "${dir}/strih-box-facts.sh"
    command -v strih_program_dante_declared >/dev/null 2>&1 || . "${dir}/strih-dante-input.sh"
    if ! strih_box_load "$name" >/dev/null 2>&1; then
      # a known box (or the old name of one) whose facts do not load is a fault, not "no receiver"
      if strih_box_known | grep -qxF -- "$name" || strih_box_alias_of "$name" >/dev/null 2>&1; then
        exit 1
      fi
      exit 0
    fi
    streams="$(strih_lx_program_vban_streams 2>/dev/null)" || exit 1
    if [ -n "$streams" ] && [ "$streams" != "none" ]; then
      input="$(strih_lx_program_audio_input 2>/dev/null)" || exit 1
      [ -n "$input" ] && [ "$input" != "none" ] || exit 1
      printf 'obs-vban:%s\n' "$input"
    elif strih_program_dante_declared; then
      printf 'dante\n'
    fi
    exit 0
  )
}

# program_audio_rx_stream_box NAME -> stdout: the STREAM-role box of NAME's venue (issue 1460, the
# SILENT page's production read: the owner's ruling 12.11.2026, a production = that box's OBS programs
# PRO and streams), from the box facts; nothing when NAME has no fact file, its facts do not load, or
# its venue has no stream box. rc 0 always.
program_audio_rx_stream_box() {
  local name="${1-}" dir="${BASH_SOURCE[0]%/*}"
  (
    command -v strih_box_load >/dev/null 2>&1 || . "${dir}/strih-box-facts.sh"
    strih_box_load "$name" >/dev/null 2>&1 || exit 0
    venue="$(strih_box_venue 2>/dev/null)" || exit 0
    for b in $(strih_box_known 2>/dev/null); do
      if ( strih_box_load "$b" >/dev/null 2>&1 && [ "$(strih_box_role)" = stream ] \
           && [ "$(strih_box_venue)" = "$venue" ] ); then
        printf '%s\n' "$b"
        exit 0
      fi
    done
    exit 0
  )
}
