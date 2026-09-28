#!/usr/bin/env bash
# airuleset:script-ok source-only lib (helpers only, no top-level statements) -- deliberately NOT
# `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it (the dev1 alert
# watchdogs run `set -uo pipefail` so one failed probe never ends the pass); every caller owns its
# own strictness.
#
# scripts/lib/watchdog-common.sh -- the state-file glue the dev1 alert watchdogs used to copy between
# themselves (issue 1386). Sourced next to scripts/lib/obs-watchdog-decision.sh (the pure
# confirm/throttle/dedup-key lib); nothing here decides or notifies, and no --dedup-key is built
# here. ONLY code that was the same in at least three watchdogs lives here (the same code by bash
# `declare -f`, which drops comments) -- a helper whose code differs even slightly stays in its
# watchdog (see "Local copies" below).
#
# Caller contract: set STATE_FILE (the watchdog's own key=value state file) before calling the
# state helpers, and NETREACH_STATE_FILE (the network-reach watchdog's state file) before
# netreach_box_alerted. Both are read at CALL time, never at source time. Source it only into a
# shell WITHOUT `set -e` (the watchdog family's `set -uo pipefail`): write_state_field's
# `[ -f ] && existing="$(grep -v ...)"` returns non-zero when the file holds only that one key, which
# `set -e` would turn into an exit (the helpers are moved verbatim; no current caller runs -e).
#
#   read_state_field <key> <default>   -> the LAST `<key>=` value in $STATE_FILE, else <default>
#   write_state_field <key> <value>    -> replace <key>'s line in $STATE_FILE (one line per key);
#                                         the other keys are read into memory BEFORE any file is
#                                         opened for writing, so even the mktemp-failure fallback
#                                         never drops them
#   clear_throttle                     -> reset confirm / alert_sig / alert_passes
#   clear_box_throttle <box>           -> the same three fields suffixed _<box>
#   clear_source_throttle <key>        -> the same three fields suffixed _<key> (a source_key)
#   source_key <raw source name>       -> a state-field-safe key: sanitized name + cksum of the raw
#                                         name, so two names that sanitize alike never share state
#   netreach_box_alerted <box>         -> the network-reach watchdog's alerted_<box> field (1 = it
#                                         has that box CONFIRMED unreachable and paged), 0 when
#                                         absent -- the no-double-page guard (issue 1001)
#
# Local copies (NOT moved, their code differs), each defined AFTER this lib is sourced, so the
# watchdog's own copy is the one that runs (bash resolves a function at call time, so the throttle
# helpers here call that copy too):
#   * an OLDER write_state_field that writes through the state file itself when mktemp fails --
#     asio-starve, avsync-heartbeat, cadence, cg-bridge, frozen-input, grabber-stuck, imag-obs,
#     imag-power-envelope, obs-session, optical-chain, splitter-port; network-reach and
#     obs-liveness carry a third variant, obs-burn-reconcile a fourth. Converging them onto this
#     copy changes the mktemp-failure path, so it is its own change, not a dedup;
#   * read_state_field in ndi-portmap and netcfg-drift (a different local declaration).
# The per-watchdog log() (its tag), fetch/probe, alert-send and handle_* functions differ per
# script and stay local as well. tests/python/test_watchdog_common_1386.py pins this list.

read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  # Read the OTHER keys into memory FIRST, before any file is opened for writing -- so even the
  # mktemp-failure fallback (a direct rewrite of STATE_FILE) can never truncate-before-read and drop
  # them (the older `tmp=$STATE_FILE` fallback has exactly that latent state-loss bug: a failed
  # mktemp truncates STATE_FILE via the redirect before `grep` reads it, collapsing it to the one
  # written key and losing every alerted_ latch).
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    # mktemp unavailable: `existing` is already captured, so a direct (non-atomic) rewrite is safe.
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# The single-incident watchdogs keep one confirm / throttle triple; the per-box and per-source ones
# key it by box or source.
clear_throttle() {
  write_state_field confirm 0
  write_state_field alert_sig ""
  write_state_field alert_passes 0
}

clear_box_throttle() {
  local box="$1"
  write_state_field "confirm_${box}" 0
  write_state_field "alert_sig_${box}" ""
  write_state_field "alert_passes_${box}" 0
}

clear_source_throttle() {
  local k="$1"
  write_state_field "confirm_${k}" 0
  write_state_field "alert_sig_${k}" ""
  write_state_field "alert_passes_${k}" 0
}

source_key() {
  local san sum
  san="$(printf '%s' "$1" | tr -c 'A-Za-z0-9' '_')"
  sum="$(printf '%s' "$1" | cksum | cut -d' ' -f1)"
  printf '%s_%s' "$san" "$sum"
}

netreach_box_alerted() {
  local box="$1"
  [ -f "$NETREACH_STATE_FILE" ] || { printf '0'; return 0; }
  local v
  v="$(sed -n "s/^alerted_${box}=//p" "$NETREACH_STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-0}"
}
