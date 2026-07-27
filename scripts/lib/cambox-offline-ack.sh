#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# scripts/lib/disk-guard-thresholds.sh / rig-test-dropin.sh convention.
#
# scripts/lib/cambox-offline-ack.sh — #758: an EXPLICIT, LOUD, TEMPORARY exclusion mechanism for a
# box that is known-offline for a REASON outside this harness's control (e.g. cam7's V-mount
# battery discharging mid-run, 2026-07-14) -- so the fused gate can keep running WITHOUT that box
# ("zatial pokracuj bez nej", the user's directive) without silently, permanently weakening the
# gate. Silently dropping an unreachable box from the sweep is banned gate-weakening
# (strict-test-mandate-no-gate-weakening); this makes the exclusion NAMED, VISIBLE in the run log
# AND the verdict JSON, and SELF-EXPIRING the moment the box comes back (a stale ack for a box
# that IS reachable is a loud FAIL, never a silent no-op) — so it cannot silently outlive the
# outage it was written for.
#
# CAMBOX_OFFLINE_ACK format: comma-separated "box:reason" pairs, e.g.
#   CAMBOX_OFFLINE_ACK="cam7:vmount-battery-discharged-2026-07-14"
#   CAMBOX_OFFLINE_ACK="cam7:vmount-battery-discharged-2026-07-14,cam5:hdmi-splitter-swap"
# A box with no ':' (just a bare name) is acked with an empty/unspecified reason -- accepted, but
# the operator message will be less useful; always prefer "box:reason".

# cambox_offline_ack_reason BOX -> the acked reason string for BOX, or empty if BOX is not named
# in CAMBOX_OFFLINE_ACK. Pure string parsing, no I/O. Case-sensitive exact box-name match (never a
# substring match -- "cam7" must not match "cam70" or a "camera7" typo).
cambox_offline_ack_reason() {
  local box="$1" entry name reason
  local ack="${CAMBOX_OFFLINE_ACK:-}"
  [ -n "$ack" ] || return 0
  local IFS=','
  for entry in $ack; do
    name="${entry%%:*}"
    if [ "$name" = "$entry" ]; then
      reason="" # bare box name, no ':reason' part
    else
      reason="${entry#*:}"
    fi
    if [ "$name" = "$box" ]; then
      printf '%s' "${reason:-unspecified}"
      return 0
    fi
  done
  return 0
}

# cambox_offline_ack_is_acked BOX -> exit 0 (true) if BOX is named in CAMBOX_OFFLINE_ACK, exit 1
# (false) otherwise. Thin boolean wrapper around cambox_offline_ack_reason for callers that only
# need the yes/no, not the reason text.
cambox_offline_ack_is_acked() {
  local box="$1"
  local ack="${CAMBOX_OFFLINE_ACK:-}"
  [ -n "$ack" ] || return 1
  local IFS=','
  local entry name
  for entry in $ack; do
    name="${entry%%:*}"
    [ "$name" = "$box" ] && return 0
  done
  return 1
}

# cambox_offline_ack_note BOX -> the loud, greppable preflight NOTE line for an acked-offline box
# (item 1's contract: "prints a prominent NOTE line... so no report can claim full coverage").
cambox_offline_ack_note() {
  local box="$1" reason
  reason="$(cambox_offline_ack_reason "$box")"
  echo "[preflight] NOTE: ${box} EXCLUDED — operator-acknowledged offline (${reason})"
}

# cambox_offline_ack_stale_message BOX -> the loud FAIL message when a box named in
# CAMBOX_OFFLINE_ACK is ACTUALLY REACHABLE (a stale ack that must never silently outlive the
# outage it was written for -- the ack has to be removed once the box returns, per #758's
# item-1 contract: "FAILS the run if the ack names a box that is actually reachable").
cambox_offline_ack_stale_message() {
  local box="$1" reason
  reason="$(cambox_offline_ack_reason "$box")"
  echo "ERROR: [preflight] STALE ACK: ${box} is named in CAMBOX_OFFLINE_ACK (reason: ${reason}) but is REACHABLE — remove it from CAMBOX_OFFLINE_ACK now that the box is back (re-enable checklist: #753)."
}

# #827: the E2E workflow had NO way to feed CAMBOX_OFFLINE_ACK from CI at all, and the ack
# mechanism above conflated "reachable" with "healthy" -- a box whose OS/network answers ping but
# whose camera-box.service is stuck (e.g. "activating" forever, cam4's grabber card physically
# removed, 2026-07-27) could never be acked without hitting the STALE hard-fail above, because
# staleness was decided purely on ping success. The three functions below fix both: a repo-level
# default-ack file the harness reads when CI sets no explicit override, and a single decision
# function that distinguishes "healthy" from "unhealthy" (not just "reachable"/"unreachable").

# cambox_offline_ack_effective EXPLICIT FILE -> the effective CAMBOX_OFFLINE_ACK value.
#   EXPLICIT non-empty -> wins outright (an operator's explicit override is never silently
#     replaced by the checked-in default -- a workflow_dispatch input, or a hand-set env var).
#   EXPLICIT empty -> read FILE (one "box:reason", "# comment", or blank line per line;
#     comments/blanks ignored, non-blank lines comma-joined into the same format
#     cambox_offline_ack_reason/_is_acked already parse). A missing/absent FILE is simply "no
#     default" (empty result, never an error) -- a repo without the file behaves exactly like
#     today (every box must be reachable-or-explicit-acked).
cambox_offline_ack_effective() {
  local explicit="${1:-}" file="${2:-}"
  if [ -n "$explicit" ]; then
    printf '%s' "$explicit"
    return 0
  fi
  if [ -z "$file" ] || [ ! -f "$file" ]; then
    return 0
  fi
  local line stripped entries=""
  while IFS= read -r line || [ -n "$line" ]; do
    stripped="${line%%#*}"
    # trim leading/trailing whitespace without a subshell/sed round trip
    stripped="${stripped#"${stripped%%[![:space:]]*}"}"
    stripped="${stripped%"${stripped##*[![:space:]]}"}"
    [ -n "$stripped" ] || continue
    entries="${entries:+$entries,}$stripped"
  done <"$file"
  printf '%s' "$entries"
}

# cambox_offline_ack_decide STATUS ACKED -> "ok" | "stale" | "exclude" | "fail" — the single
# source of truth for the preflight decision matrix (STATUS = healthy|unhealthy|unreachable,
# ACKED = 1|0). Replaces scattered inline if/else in recording-e2e.sh's [0/8] loop:
#   healthy    + unacked -> ok       (proceed normally, exactly like today)
#   healthy    + acked   -> stale    (the fleet came back and nobody updated the ack — loud)
#   unhealthy  + acked   -> exclude  (#827 fix: reachable-but-broken, e.g. cam4 — proceed, named)
#   unhealthy  + unacked -> fail     (exactly like today)
#   unreachable+ acked   -> exclude  (exactly like today)
#   unreachable+ unacked -> fail     (exactly like today)
#   anything else (unknown STATUS)  -> fail (never silently OK on an unrecognized status)
cambox_offline_ack_decide() {
  local status="${1:-}" acked="${2:-0}"
  case "$status" in
  healthy)
    if [ "$acked" = "1" ]; then echo "stale"; else echo "ok"; fi
    ;;
  unhealthy | unreachable)
    if [ "$acked" = "1" ]; then echo "exclude"; else echo "fail"; fi
    ;;
  *)
    echo "fail"
    ;;
  esac
}

# cambox_offline_ack_excluded_json BOXES -> a JSON array `[{"box":"cam4","reason":"..."}, ...]`
# for embedding into the run's verdict JSON (recording-e2e.sh jq-merges this into $REPORT_JSON as
# .excluded_cams after the merge writes it) -- so a partial-fleet run can NEVER be read back as
# "full-fleet clean" from the JSON alone (#827 acceptance). BOXES is a space-separated box-name
# list (as PREFLIGHT_EXCLUDED_CAMS accumulates it); reasons come from CAMBOX_OFFLINE_ACK via
# cambox_offline_ack_reason. Requires jq (already a repo dependency, see e2e-discord-report.sh).
cambox_offline_ack_excluded_json() {
  local boxes="${1:-}" box reason out="[]"
  for box in $boxes; do
    reason="$(cambox_offline_ack_reason "$box")"
    out="$(printf '%s' "$out" | jq --arg b "$box" --arg r "$reason" '. + [{"box":$b,"reason":$r}]')"
  done
  printf '%s' "$out"
}
