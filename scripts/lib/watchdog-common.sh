#!/usr/bin/env bash
# airuleset:script-ok source-only lib (helpers only, no top-level statements) -- deliberately NOT
# `set -euo pipefail`: sourcing this into a caller must never change the caller's shell options;
# every caller owns its own strictness.
#
# scripts/lib/watchdog-common.sh -- the glue the dev1 alert watchdogs used to copy between themselves
# (issue 1386): the key=value state-file helpers and the :8899 bundle-state fetch. Sourced next to
# scripts/lib/obs-watchdog-decision.sh (the pure confirm/throttle/dedup-key lib); nothing here
# decides or notifies, and no --dedup-key is built here. ONLY a helper whose code was the same (by
# bash `declare -f`, which drops comments) in at least three watchdogs lives here; that criterion
# was applied to every function the watchdogs define. A helper whose code differs even slightly
# stays in its watchdog (see "Local copies" below).
#
# Caller contract: set STATE_FILE (the watchdog's own key=value state file) before calling the
# state helpers, NETREACH_STATE_FILE (the network-reach watchdog's state file) before
# netreach_box_alerted, and CURL_TIMEOUT / BUNDLE_PORT / BUNDLE_PATH before fetch_bundle_json. All
# are read at CALL time, never at source time.
#
# errexit: write_state_field is NOT `set -e` safe -- its `[ -f ] && existing="$(grep -v ...)"`
# returns non-zero when the file holds only that one key, which `set -e` turns into an exit (the
# helpers are moved verbatim). Every watchdog that calls this copy runs without -e. Three watchdogs
# DO run with errexit on at call time, because a lib they source turns it on and `set -uo pipefail`
# never clears it: avsync-heartbeat (lib/avsync-heartbeat.sh), imag-obs (imag-obs-reachability.sh,
# imag-obs-restart-storm.sh) and obs-session (win-ssh-exec.sh). They call only read_state_field from
# here, which is -e safe, and keep their own write_state_field -- never point them at this copy
# before they clear -e or this copy is made -e safe.
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
#   fetch_bundle_json <ip>             -> the box's :8899 bundle-state body on stdout, rc 0 only for
#                                         a `{`-body (a curl failure or a non-JSON answer is rc 1 =
#                                         SKIP for that pass, never a false page)
#
# Local copies (NOT moved, their code differs). A watchdog that sources this lib defines its copy
# AFTER the source line, so its own copy is the one that runs (bash resolves a function at call
# time, so the throttle helpers here call that copy too):
#   * an OLDER write_state_field that writes through the state file itself when mktemp fails --
#     asio-starve, avsync-heartbeat, cadence, cg-bridge, frozen-input, grabber-stuck, imag-obs,
#     imag-power-envelope, obs-session, optical-chain, splitter-port; network-reach and
#     obs-liveness carry a variant with literal-newline printf formats, obs-burn-reconcile one with
#     a fixed temp path. Converging them onto this copy changes the mktemp-failure path (and three
#     of them run with errexit, above), so it is not part of this dedup;
#   * read_state_field in ndi-portmap and netcfg-drift (a different local declaration);
#   * fetch_bundle_json in audio-mixer, genlock-lock and vb-matrix (each with its own *_FETCH_CMD
#     test seam).
# Two watchdogs do not source this lib and keep variant copies of their own: avsync-lineup (the older
# write_state_field and a read_state_field variant) and vban-rate (read and write variants).
# The per-watchdog log() (its tag), the ssh/log probes, the alert send, the recovery decision and the
# handle_* functions differ per script (by name or by code) and stay local as well.
# tests/python/test_watchdog_common_1386.py pins the override list.

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

fetch_bundle_json() {
  local ip="$1" body
  body="$(curl -fsS --max-time "$CURL_TIMEOUT" "http://${ip}:${BUNDLE_PORT}${BUNDLE_PATH}" 2>/dev/null)" \
    || return 1
  body="${body#"${body%%[![:space:]]*}"}"   # strip leading whitespace (a python-json body carries no
                                            # BOM; a hypothetical BOM'd body fails the {* case -> SKIP,
                                            # the safe direction — never a false page)
  case "$body" in
    \{*) printf '%s' "$body"; return 0 ;;
    *) return 1 ;;
  esac
}
