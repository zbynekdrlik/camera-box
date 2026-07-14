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
