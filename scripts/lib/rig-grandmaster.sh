#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions, no top-level statements) -- deliberately NOT
# `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it (the standing
# .claude/rules/ci-testing-gotchas.md rule); every caller owns its own strictness.
#
# scripts/lib/rig-grandmaster.sh -- ONE source of truth for the rig's PTP grandmaster ADDRESS (#1307).
#
# Owner directive 2026-09-13: the grandmaster is addressed by DNS NAME `video-clock.lan`, never a
# literal IP. The name is a MikroTik static DNS entry pointing at the Yamaha AIC128-D Dante card's
# static DHCP lease (10.77.9.230 today). The OLD literal 10.77.9.184 was only where that card's
# DHCP lease happened to sit: when the lease moved, every `gm_allowlist: ["10.77.9.184"]` node fell
# to NTP-only for hours with nobody noticing (#1297 comments, 2026-09-13). Resolution precedence:
#   1. RIG_GRANDMASTER_IP    explicit override (CI fixtures, a deliberate one-off) -- wins verbatim.
#   2. RIG_GRANDMASTER_HOST  (default video-clock.lan) resolved to its FIRST IPv4 via getent.
#   3. Unresolvable -> LOUD failure (rc 1 + a message on stderr), NEVER a silent stale literal
#      (early-gate-pin doctrine: a gate pins to the expected grandmaster and fails CLOSED on UNKNOWN).
# Seam: RIG_GRANDMASTER_GETENT overrides the `getent` binary so Tier-0 tests inject a fake resolver.
#
# Consumers: scripts/dantesync-gate.sh (GATE_GRANDMASTER_IP), scripts/verify-imag.sh,
# scripts/setup-imag.sh + scripts/setup-device.sh STEP 17 (the dantesync `system.gm_allowlist` they
# write at provision time). Until zbynekdrlik/dantesync#113 lets `gm_allowlist` carry a hostname,
# the provisioners write the RESOLVED IPv4; the gate compares each node's `gm_source_ip` against it.

RIG_GRANDMASTER_HOST_DEFAULT="video-clock.lan"

# rig_grandmaster_host -> the configured grandmaster hostname (env override or the default).
rig_grandmaster_host() {
  printf '%s\n' "${RIG_GRANDMASTER_HOST:-$RIG_GRANDMASTER_HOST_DEFAULT}"
}

# rig_grandmaster_resolve HOST -> the first IPv4 for HOST on stdout, EMPTY when it does not resolve.
# Always rc 0 -- the caller decides loudness (rig_grandmaster_ip does).
rig_grandmaster_resolve() {
  local host="$1" getent_bin="${RIG_GRANDMASTER_GETENT:-getent}"
  "$getent_bin" ahostsv4 "$host" 2>/dev/null | awk 'NR==1 {print $1}' || true
}

# rig_grandmaster_ip -> the grandmaster IPv4 on stdout (rc 0), or rc 1 + a loud stderr line.
rig_grandmaster_ip() {
  if [ -n "${RIG_GRANDMASTER_IP:-}" ]; then
    printf '%s\n' "$RIG_GRANDMASTER_IP"
    return 0
  fi
  local host ip
  host="$(rig_grandmaster_host)"
  ip="$(rig_grandmaster_resolve "$host")"
  if [ -z "$ip" ]; then
    echo "rig-grandmaster: cannot resolve the PTP grandmaster host '${host}' -- the rig DNS (MikroTik static entry video-clock.lan -> the Yamaha Dante card) is missing or unreachable; set RIG_GRANDMASTER_IP=<ip> only as a deliberate override (#1307)" >&2
    return 1
  fi
  printf '%s\n' "$ip"
}
