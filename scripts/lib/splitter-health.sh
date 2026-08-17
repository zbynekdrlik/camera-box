#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (network-reach-health.sh, obs-watchdog-decision.sh,
# optical-chain-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (splitter-port-alert-watchdog.sh) sets its own strict mode.
#
# scripts/lib/splitter-health.sh -- #739: the SHARED, PURE decision core for the dev1-side per-cambox
# HDMI-splitter-port no-signal recurrence watch. No I/O, no ssh, no journalctl, so it can be
# unit-tested exhaustively (mirrors scripts/lib/network-reach-health.sh / optical-chain-health.sh).
#
# WHY (#739, live 2026-07-13): the rig feeds ONE camera through an HDMI splitter to every cambox, so
# per-cambox capture can only differ by each box's INDIVIDUAL leg (its splitter output port + cable/
# grabber). When 4/6 splitter ports died, the boxes on dead ports saw NO SIGNAL while siblings saw the
# shared camera -- but each grabber renders no-signal differently (Elgato 4K S = purple noise;
# ShadowCast 2 = flat grey), so the failures MASQUERADED as per-camera "colour" bugs and burned two
# days of tint-hunting. The masquerade happened because each box's colour was judged IN ISOLATION
# instead of COMPARED against the fleet consensus -- the one comparison that isolates a per-port fault.
#
# THE DISCRIMINATOR (splitter_health_classify): a box is a SPLITTER-PORT suspect iff it is degraded
# (not capturing OR grayscale) AND >=1 SIBLING is proven-good (reachable + capturing + colour). A
# proven-good sibling proves the shared camera is delivering AND dev1's path to the rig is up, so the
# only element that can differ for the bad box is its own output port. If EVERY reachable box is
# equally degraded -> shared source (camera off / AWB / idle rig), NOT a per-port fault -> never a
# false page. This self-anchors (no separate reference-anchor guard needed, unlike network-reach
# #1001, whose per-box signal has no fleet-consensus) and encodes the rig rule "identical-across-boxes
# = one shared fault; per-box divergence = that box's leg".
#
# The per-box READABLE signal is the #299 chroma metric camera-box already logs every ~5s to its
# journal: `capture chroma: u_dev=X.X v_dev=Y.Y rough=R.R -> colour|grayscale (source likely
# monochrome)`. This robustly catches the flat-grey no-signal mode (ShadowCast) and any frame-stall
# mode (no fresh line). The Elgato purple-noise mode (colourful, frames flow) reads as colour, so it
# needs the #1079 `rough=` term (per-frame spatial-roughness): high roughness + colour = the structure-
# less-noise signature. This lib PARSES + surfaces `rough=` REPORT-ONLY (fleet-wide telemetry); it does
# NOT gate/classify on it yet — a data-first follow-up calibrates the threshold before it pages.
#
# Source-only: pure functions, no side effects at source time.

# splitter_health_parse_probe <raw> -> stdout ONE line:
#   reachable=<0|1> capturing=<0|1> colour=<0|1> u_dev=<val|-> v_dev=<val|-> rough=<val|->
#   Parses one cambox's raw ssh probe output. The remote command echoes the sentinel `PROBE_OK` on a
#   successful ssh connection, then optionally the box's most recent `capture chroma:` journal line
#   (already time-bounded by the caller's `--since` window, so its mere PRESENCE is the liveness
#   signal). An empty/`PROBE_OK`-less raw (ssh failed / box off the wire) -> reachable=0 = NODATA,
#   never a false signal.
#   `rough=` is the #1079 per-frame spatial-roughness metric (mean adjacent-pixel luma delta),
#   REPORT-ONLY here — surfaced fleet-wide by the watchdog so a data-first follow-up can calibrate a
#   noise threshold; it does NOT feed splitter_health_classify yet. A cambox not yet redeployed with
#   the metric logs the OLD line (no `rough=`) -> rough=- (a placeholder, never a bogus number), so a
#   rolling fleet redeploy is safe and the 6-field record shape is stable regardless of box version.
splitter_health_parse_probe() {
  local raw="${1:-}"
  local reachable=0 capturing=0 colour=0 u_dev="-" v_dev="-" rough="-"
  case "$raw" in
    *PROBE_OK*) reachable=1 ;;
  esac
  if [ "$reachable" = "1" ]; then
    local line
    line="$(printf '%s\n' "$raw" | grep 'capture chroma:' | tail -1)"
    if [ -n "$line" ]; then
      capturing=1
      case "$line" in
        *"-> colour"*) colour=1 ;;
        *) colour=0 ;;
      esac
      local u v r
      u="$(printf '%s\n' "$line" | sed -n 's/.*u_dev=\([0-9.]*\).*/\1/p')"
      v="$(printf '%s\n' "$line" | sed -n 's/.*v_dev=\([0-9.]*\).*/\1/p')"
      r="$(printf '%s\n' "$line" | sed -n 's/.*rough=\([0-9.]*\).*/\1/p')"
      [ -n "$u" ] && u_dev="$u"
      [ -n "$v" ] && v_dev="$v"
      [ -n "$r" ] && rough="$r"
    fi
  fi
  printf 'reachable=%s capturing=%s colour=%s u_dev=%s v_dev=%s rough=%s\n' \
    "$reachable" "$capturing" "$colour" "$u_dev" "$v_dev" "$rough"
}

# splitter_health_is_healthy <reachable 0|1> <capturing 0|1> <colour 0|1> -> stdout: 1 | 0
#   A "proven-good sibling": reachable AND capturing AND colour. Any value other than "1" for any of
#   the three counts as not-healthy (defensive: empty/garbage is never a false proven-good).
splitter_health_is_healthy() {
  local r="${1:-0}" c="${2:-0}" k="${3:-0}"
  if [ "$r" = "1" ] && [ "$c" = "1" ] && [ "$k" = "1" ]; then
    printf '1\n'
  else
    printf '0\n'
  fi
}

# splitter_health_classify <reachable> <capturing> <colour> <healthy_siblings> -> stdout: verdict=<X>
#   NODATA       : reachable != 1 (box unreadable -- never a per-port claim; box off / network).
#   NO_CAPTURE   : reachable + NOT capturing (no fresh chroma line). Report-only, NEVER paged: a
#                  DIFFERENT, ambiguous failure class (camera-box crashed / device-busy / stopped by
#                  an E2E run / a genuine grabber stall) that is ROUTINE on this rig, so attributing it
#                  to the HDMI splitter port would be an overstatement / false page. The ORIGINAL
#                  dead-port failure (#739) kept the grabber PRODUCING frames (Elgato purple noise /
#                  ShadowCast flat grey) = capturing=1 with bad CONTENT -- which is what DEAD_PORT keys
#                  on. A fully-stalled grabber on a dead port therefore lands in this report bucket
#                  (operator-visible in the log) rather than a mis-attributed splitter-port page.
#   OK           : reachable + capturing + colour.
#   DEAD_PORT    : reachable + capturing + GRAYSCALE + >=1 proven-good sibling. The tight splitter-port
#                  signal: the box is alive and capturing, but its filmed content lost the signal a
#                  sibling on the SAME camera+splitter still receives -> that box's own output leg.
#   SOURCE_WIDE  : reachable + capturing + grayscale + NO proven-good sibling (every reachable box
#                  equally grey => shared camera/source or idle rig, NOT a per-port fault -> report-only).
#   A non-numeric healthy_siblings is treated as 0 (a garbage count must NEVER be read as "a healthy
#   sibling exists" and produce a false DEAD_PORT page -- fail toward SOURCE_WIDE, the report-only side).
splitter_health_classify() {
  local r="${1:-0}" c="${2:-0}" k="${3:-0}" sib="${4:-0}"
  case "$sib" in *[!0-9]* | "") sib=0 ;; esac
  if [ "$r" != "1" ]; then
    printf 'verdict=NODATA\n'
    return 0
  fi
  if [ "$c" != "1" ]; then
    printf 'verdict=NO_CAPTURE\n'
    return 0
  fi
  if [ "$k" = "1" ]; then
    printf 'verdict=OK\n'
    return 0
  fi
  if [ "$sib" -ge 1 ]; then
    printf 'verdict=DEAD_PORT\n'
  else
    printf 'verdict=SOURCE_WIDE\n'
  fi
}

# splitter_health_alert_detail <box> <capturing> <colour> <u_dev> <v_dev> -> stdout: one human line
#   naming the box and WHY it is degraded. The GRAYSCALE branch is the PAGED (DEAD_PORT) line: it names
#   the HDMI splitter port as the leading suspect (the ticket's whole point -- a dead port must page as
#   a SPLITTER-PORT suspicion, not masquerade as a per-camera colour bug), with cable/grabber as the
#   alternatives. The NOT-capturing branch describes the AMBIGUOUS NO_CAPTURE class honestly and does
#   NOT attribute it to the splitter port (it is report-only, never a page -- see classify).
splitter_health_alert_detail() {
  local box="${1:-?}" c="${2:-0}" k="${3:-0}" u="${4:--}" v="${5:--}"
  if [ "$c" != "1" ]; then
    printf '%s: NOT capturing (no fresh "capture chroma:" line recently) -- ambiguous: camera-box down / device-busy / E2E-stop / grabber stall; NOT attributed to the splitter port (report-only)\n' "$box"
  elif [ "$k" != "1" ]; then
    printf '%s: capturing but GRAYSCALE (u_dev=%s v_dev=%s) while the fleet is in colour -- its HDMI splitter port likely lost the signal siblings still receive (also its cable/grabber)\n' "$box" "$u" "$v"
  else
    printf '%s: OK (u_dev=%s v_dev=%s)\n' "$box" "$u" "$v"
  fi
}
