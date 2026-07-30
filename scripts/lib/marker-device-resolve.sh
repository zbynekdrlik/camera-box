#!/usr/bin/env bash
# airuleset:script-ok sourced pure-function library (mirrors rig-test-dropin.sh /
# presenter-liveness-check.sh / audio-marker-check.sh) -- functions use `return 1` as their
# normal calling convention (e.g. "no monitor found"), and `set -euo pipefail` must NEVER be set
# here because sourcing this file mutates the CALLING script's (rig-mode.sh) own shell options.
#
# scripts/lib/marker-device-resolve.sh — #725: resolve the QPSK A/V-sync audio-marker's ALSA
# device DYNAMICALLY from the live `aplay -l` output, rather than trusting the hardcoded
# `hw:CARD=PCH,DEV=3` default. Any HDMI renegotiation (a reboot, a cable reseat) can move which
# `DEV=N` the physical monitor actually lands on — the marker then plays into a DEAD pin while
# every existing check (ALSA `state: RUNNING`, the #431 marker-log-growth check) still reports
# PASS, because both only prove the CONTINUOUS-FEED writer is alive, never that a real sink is
# attached. Root cause: 2026-07-12 evening, second speaker-silent incident that day — TEST mode
# claimed "audio marker RUNNING+VERIFIED" while no sound left the speaker.
#
# The resolution mechanism (per the issue #725 owner's PREMISE CORRECTION, live-resolved at the
# rig — the ticket originally proposed scanning `/proc/asound/card*/eld#N.M` files, which needs
# guesswork to map an eld# index to a PCM device number; that guesswork made things briefly
# WORSE that evening): `aplay -l` already prints the negotiated monitor's EDID product name next
# to the HDMI device that carries it —
#   card 0: PCH [HDA Intel PCH], device 3: HDMI 0 [BenQ GL2480]
# — while a device with NO connected monitor (or one whose EDID hasn't negotiated) instead shows
# the GENERIC placeholder identical to its own slot label:
#   card 0: PCH [HDA Intel PCH], device 7: HDMI 1 [HDMI 1]
# So "this device carries a genuine monitor" == "the bracketed name differs from its own `HDMI N`
# slot label" — simple, correct, and needs no eld#-file numbering at all.
#
# Sourced by scripts/rig-mode.sh (do_test — the TEST-mode painter launch). Pure text-parsing
# functions only: no ssh, no side effects at source time — safe to source from unit tests.

# marker_device_aplay_list_cmds -> the REMOTE bash snippet (embed via `$(...)` inside an ssh
# call) that prints the box's `aplay -l` output, never failing the caller if aplay itself is
# missing/errors (an empty transcript just means resolution finds nothing -> caller falls back).
marker_device_aplay_list_cmds() {
  echo 'aplay -l 2>/dev/null || true'
}

# _marker_device_parse_entries APLAY_TEXT -> stdout, one line per HDMI playback device found:
#   CARD|DEV|HAS_MONITOR|MONITOR_NAME
# HAS_MONITOR is 1 when the bracketed name differs from the device's own generic "HDMI N" slot
# label (a genuine connected-monitor EDID name), else 0. Internal helper — pure, no I/O.
_marker_device_parse_entries() {
  local aplay_text="$1" line
  while IFS= read -r line; do
    if [[ "$line" =~ ^card\ [0-9]+:\ ([A-Za-z0-9_]+)\ \[.*\],\ device\ ([0-9]+):\ (HDMI\ [0-9]+)\ \[(.*)\]$ ]]; then
      local card="${BASH_REMATCH[1]}" dev="${BASH_REMATCH[2]}" slot="${BASH_REMATCH[3]}" monitor="${BASH_REMATCH[4]}"
      local has_monitor=0
      if [ -n "$monitor" ] && [ "$monitor" != "$slot" ]; then
        has_monitor=1
      fi
      printf '%s|%s|%s|%s\n' "$card" "$dev" "$has_monitor" "$monitor"
    fi
  done <<< "$aplay_text"
}

# marker_device_resolve_from_aplay APLAY_TEXT -> stdout: "hw:CARD=<name>,DEV=<n>" for the FIRST
# HDMI playback device (in aplay -l's own listing order — deterministic, never ambiguous) that
# carries a genuine (non-generic) monitor name. Prints nothing + returns 1 when no device in the
# transcript carries a real monitor — the caller MUST treat that as a hard resolution failure
# (never silently pick a dead pin).
marker_device_resolve_from_aplay() {
  local aplay_text="$1" card dev has_monitor monitor
  while IFS='|' read -r card dev has_monitor monitor; do
    [ -n "$card" ] || continue
    if [ "$has_monitor" = "1" ]; then
      echo "hw:CARD=${card},DEV=${dev}"
      return 0
    fi
  done < <(_marker_device_parse_entries "$aplay_text")
  return 1
}

# marker_device_carries_monitor APLAY_TEXT DEVICE_STR -> exit 0 iff DEVICE_STR (a
# "hw:CARD=<name>,DEV=<n>" string) appears in APLAY_TEXT's HDMI device listing AND carries a
# genuine (non-generic) monitor name right now. Exit 1 when the device is missing from the
# transcript entirely (the pin vanished) OR shows only the generic placeholder (no monitor
# attached). This is the #725 "verify monitor_present on the RESOLVED device" re-check — run
# again right after painter launch to catch a monitor that was unplugged in the gap between
# resolution and launch, or a fallback device that turns out to have no monitor either.
marker_device_carries_monitor() {
  local aplay_text="$1" device_str="$2"
  local want_card="${device_str#*CARD=}"; want_card="${want_card%%,*}"
  local want_dev="${device_str#*DEV=}"; want_dev="${want_dev%%,*}"
  local card dev has_monitor monitor
  while IFS='|' read -r card dev has_monitor monitor; do
    [ -n "$card" ] || continue
    if [ "$card" = "$want_card" ] && [ "$dev" = "$want_dev" ]; then
      [ "$has_monitor" = "1" ] && return 0 || return 1
    fi
  done < <(_marker_device_parse_entries "$aplay_text")
  return 1   # device not present in the transcript at all
}
