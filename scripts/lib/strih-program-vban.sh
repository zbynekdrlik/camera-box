#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-program-vban.sh -- issue 1439: verify-strih item 9 on a box whose OBS program audio
# is the console's VBAN stream straight into the vendored obs-vban SOURCE (the stream role since design
# revision 6338127636: no intercom hub, no PipeWire). The source stamps every packet on its receive
# timeline and writes one status line every 10 s while packets arrive (vendor/obs-vban/src/vban-source.c):
#
#   [obs-vban] obs-vban receive: received=N lost=N late=N re_anchors=N stale=N silence_ms=X max_late_ms=X
#     sender_ppm=X phase_ms=X fit=locked|acquiring receive_buffer_ms=R rate=N ch=N port=P stream='S'
#     source='<OBS input>'
#
# The rows (through the caller's ok / bad / note; each grade is rc 0):
#   (program-vban) kind       the program-audio input is the obs-vban source (the seed switches it)
#   (program-vban) bind       OBS bound the fact port (a hub left running holds it: the bind fails)
#   (program-vban) status     a status line exists, its port + stream = the box facts (a source that
#                             logged `anchored` but no status line yet gets one period's wait first)
#   (program-vban) live       `received` advanced over STRIH_PROGRAM_VBAN_WAIT_S (12 s, > one status
#                             period); not advancing = NOTE (the console is not sending now)
#   (program-vban) R          receive_buffer_ms = the fact, else a NOTE (the operator's setting is kept)
#   (program-vban) counters   lost / late / re_anchors since the source started: a NOTE when any is > 0
#
# Seams: STRIH_PROGRAM_VBAN_WAIT_S (the live window), STRIH_PROGRAM_VBAN_KIND_FILE (a file holding the
# input kind, read instead of the strih_scenes.py websocket read).

# The obs-vban source id, pinned equal to scripts/program_audio_vban.py VBAN_SOURCE_KIND by the pytest.
STRIH_PROGRAM_VBAN_KIND="net.nagater.obs-vban.source"

# strih_program_vban_status_line LOG NAME -> the newest `obs-vban receive:` status line of the OBS input
# NAME in LOG ('' when there is none or LOG is unreadable). Streams the log through grep (never into a
# variable whole) and survives a no-match under the caller's pipefail.
strih_program_vban_status_line() {
  local log="${1-}" name="${2-}"
  [ -n "$log" ] && [ -r "$log" ] || return 0
  { grep -a -F -- "obs-vban receive: received=" "$log" || true; } \
    | { grep -a -F -- "source='${name}'" || true; } | tail -n 1
}

# strih_program_vban_field LINE KEY -> the value of the ` KEY=` token in LINE ('' when absent).
strih_program_vban_field() {
  sed -n "/ ${2}=/{s/.* ${2}=\([^ ]*\).*/\1/p;q}" <<<"${1-}"
}

# strih_program_vban_grade SCN_BIN WS_HOST LOG [NAME] -> the item-9 rows above for the program-audio input
# NAME (default the loaded box's STRIH_PROGRAM_AUDIO_INPUT: stream-pp `Zvuk z mixu`, stream-snv `mbc`,
# issue 1462), graded against the loaded box's facts.
strih_program_vban_grade() {
  local scn="${1-}" ws="${2-}" log="${3-}" name="${4-}" wait="${STRIH_PROGRAM_VBAN_WAIT_S:-12}"
  local port stream r kind l1 l2 n1 n2 v counters
  if ! port="$(strih_lx_program_vban_port)" || ! stream="$(strih_lx_program_vban_streams)" \
     || ! r="$(strih_lx_program_vban_reserve_ms)" || [ "$port" = none ] \
     || { [ -z "$name" ] && ! name="$(strih_lx_program_audio_input)"; }; then
    bad "(program-vban) the box declares no program VBAN stream (STRIH_PROGRAM_VBAN_*) -- this box's program audio is not graded"
    return 0
  fi
  if [ -n "${STRIH_PROGRAM_VBAN_KIND_FILE:-}" ]; then
    kind="$(cat -- "$STRIH_PROGRAM_VBAN_KIND_FILE" 2>/dev/null || echo absent)"
  else
    kind="$(timeout 20 python3 "$scn" --host "$ws" --audio-input-kind 2>/dev/null || echo absent)"
  fi
  if [ "$kind" = "$STRIH_PROGRAM_VBAN_KIND" ]; then
    ok "(program-vban) OBS '${name}' is the obs-vban receive source"
  else
    bad "(program-vban) OBS '${name}' is '${kind:-absent}', not the obs-vban receive source -- restart OBS so strih_scenes.py --bootstrap switches it (issue 1439; the bundle must carry obs-vban)"
  fi
  if [ -n "$log" ] && [ -r "$log" ] && grep -a -q -F -- "[obs-vban] Failed to bind port ${port}" "$log"; then
    bad "(program-vban) OBS could not bind UDP ${port} (newest OBS log): another process holds it, the intercom hub? -- re-run setup-strih.sh step 13, then restart OBS"
  fi
  l1="$(strih_program_vban_status_line "$log" "$name")"
  # The first status line comes 10 s after the first packet; the anchored line proves packets arrive,
  # so a source that anchored is given one status period before "nothing arrived" is graded.
  if [ -z "$l1" ] && [ -r "$log" ] \
     && grep -a -q -F -- "obs-vban receive anchored: source '${name}'" "$log"; then
    sleep "$wait"
    l1="$(strih_program_vban_status_line "$log" "$name")"
  fi
  if [ -z "$l1" ]; then
    bad "(program-vban) no obs-vban receive status line for '${name}' in ${log:-<no OBS log>}: no VBAN packet reached OBS since it started -- is the console sending stream '${stream}' to $(strih_lx_ip 2>/dev/null || echo this box):${port}?"
    return 0
  fi
  v="$(strih_program_vban_field "$l1" port)"
  [ "$v" = "$port" ] || bad "(program-vban) the source listens on UDP ${v:-?}, the box fact says ${port} (STRIH_PROGRAM_VBAN_PORT) -- set it in the source's properties"
  v="$(strih_program_vban_field "$l1" stream)"
  [ "$v" = "'${stream}'" ] || bad "(program-vban) the packets carry stream ${v:-?}, the box fact says '${stream}' (STRIH_PROGRAM_VBAN_STREAMS)"
  v="$(strih_program_vban_field "$l1" receive_buffer_ms)"
  [ "$v" = "$r" ] || note "(program-vban) receive_buffer_ms=${v:-?} in OBS, the box fact says ${r}: the operator's setting is kept (issue 1439) -- set it in the source's properties to follow the fact"
  n1="$(strih_program_vban_field "$l1" received)"
  sleep "$wait"
  l2="$(strih_program_vban_status_line "$log" "$name")"
  n2="$(strih_program_vban_field "$l2" received)"
  if [[ "$n1" =~ ^[0123456789]+$ ]] && [[ "$n2" =~ ^[0123456789]+$ ]] && [ "$n2" -gt "$n1" ]; then
    ok "(program-vban) receiving stream '${stream}' on UDP ${port}: received ${n1} -> ${n2} in ${wait} s, sender_ppm=$(strih_program_vban_field "$l2" sender_ppm) fit=$(strih_program_vban_field "$l2" fit) R=$(strih_program_vban_field "$l2" receive_buffer_ms) ms"
  else
    note "(program-vban) ${n1:-?} packets received since OBS started, none in the last ${wait} s -- the console is not sending now (graded live while it sends)"
  fi
  counters="lost=$(strih_program_vban_field "${l2:-$l1}" lost) late=$(strih_program_vban_field "${l2:-$l1}" late) re_anchors=$(strih_program_vban_field "${l2:-$l1}" re_anchors)"
  case "$counters" in
    "lost=0 late=0 re_anchors=0") ;;
    *) note "(program-vban) since the source started: ${counters} (each one is audible; the acceptance is zero over 8 h, issue 1439)" ;;
  esac
  return 0
}
