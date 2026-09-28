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
# errexit: every helper here is `set -e` safe (the watchdogs run without -e; a lib that turned it on
# is cleared right after the source block, and tests/python/test_watchdog_errexit_1386.py pins that).
#
#   read_state_field <key> <default>   -> the LAST `<key>=` value in $STATE_FILE, else <default>
#   write_state_field <key> <value>    -> replace <key>'s line in $STATE_FILE (one line per key),
#                                         the other keys kept in order. Always returns 0: a pass
#                                         never ends on a state write. A failure is reported on
#                                         stderr (the journal), never silent, and never costs the
#                                         other keys -- see the function's own comment
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
# write_state_field has NO local copy anywhere: the sixteen the watchdogs used to carry (the older
# write-through-the-state-file-on-mktemp-failure copy, the literal-newline read-first variant, the
# fixed-temp-path one) were removed, so this is the one state write every watchdog runs.
#
# Local copies (NOT moved, their code differs). A watchdog that sources this lib defines its copy
# AFTER the source line, so its own copy is the one that runs (bash resolves a function at call
# time, so the throttle helpers here call that copy too):
#   * read_state_field in ndi-portmap, netcfg-drift, avsync-lineup and vban-rate (a different
#     local declaration);
#   * fetch_bundle_json in audio-mixer, genlock-lock and vb-matrix (each with its own *_FETCH_CMD
#     test seam).
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
  # The one state write of the dev1 watchdogs (issue 1386):
  #   * the OTHER keys are read into memory BEFORE any file is opened for writing -- the older copy's
  #     `tmp=$STATE_FILE` fallback truncated the file before grep read it and kept only this key;
  #   * a state file grep cannot read (exit > 1) is left alone -- rewriting it would drop every key
  #     we could not read;
  #   * the new content goes to a mktemp file and is renamed over the state file only when the write
  #     succeeded, so a full disk (mktemp needs an inode, the write a block) never replaces the state
  #     with an empty or partial file;
  #   * mktemp unavailable: rewrite the state file in place from the captured keys (not atomic, so it
  #     is reported). Skipping the write instead would freeze every confirm counter, and the watchdog
  #     could never page;
  #   * every failure prints one ERROR line on stderr (the journal) and the call returns 0: a watchdog
  #     pass never dies on a state write.
  local key="$1" val="$2" tmp="" existing="" content why="" rc=0
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  if [ -f "$STATE_FILE" ]; then
    existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)" || rc=$?
  fi
  if [ "$rc" -gt 1 ]; then
    why="could not read the other keys (grep exit $rc); the state file is left as it was"
  else
    content="${key}=${val}"
    [ -z "$existing" ] || content="${existing}"$'\n'"${content}"
    tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null)" || tmp=""
    if [ -n "$tmp" ]; then
      if ! { printf '%s\n' "$content" > "$tmp"; } 2>/dev/null || ! mv -f "$tmp" "$STATE_FILE" 2>/dev/null; then
        rm -f "$tmp" 2>/dev/null || true
        why="the temp write or rename failed; the state file is left as it was"
      fi
    else
      printf '%s [%s] WARNING: mktemp failed next to %s -- rewriting it in place (not atomic)\n' \
        "$(date '+%Y-%m-%dT%H:%M:%S%z')" "${0##*/}" "$STATE_FILE" >&2
      if ! { printf '%s\n' "$content" > "$STATE_FILE"; } 2>/dev/null; then
        why="mktemp failed and the in-place rewrite failed too"
      fi
    fi
  fi
  if [ -n "$why" ]; then
    printf '%s [%s] ERROR: state write failed: %s in %s -- %s\n' \
      "$(date '+%Y-%m-%dT%H:%M:%S%z')" "${0##*/}" "$key" "$STATE_FILE" "$why" >&2
  fi
  return 0
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
