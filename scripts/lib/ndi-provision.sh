#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + one constant, no top-level side effects) --
# deliberately NOT `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it
# (the standing .claude/rules/ci-testing-gotchas.md rule); every caller owns its own strictness.
#
# scripts/lib/ndi-provision.sh -- ONE source of truth for the fleet NDI Linux runtime PROVISIONING
# facts (#1066). Named "ndi-provision" (not "ndi-runtime") deliberately: the token "ndi-runtime" is
# already the genlock manifest component name (tests/genlock_manifest.rs) and scripts/lib/
# bkshading-ndi-runtime.sh; this lib is the setup-device.sh/verify-device.sh provisioning half.
#
#   * NDI_VERSION_PIN                      -- the pinned fleet runtime version. Single-sourced here
#       (#1066); verify-device.sh's (o) check + setup-device.sh STEP 4's download-fallback both read
#       THIS constant instead of a second inline 6.3.2 literal.
#   * ndi_bootstrap_peer_list SELF_IP      -- the ordered, deduped list of fleet peer IPs to fetch
#       libndi from (one per line), EXCLUDING SELF_IP (the box being provisioned). Fixes the STEP 4
#       chicken-and-egg where the single hard-coded peer WAS the box being re-provisioned (cam1,
#       2026-09-13). Requires scripts/camera-set.sh already sourced (camera_resolve + CAMERA_SET).
#   * ndi_runtime_version_matches_pin FN PIN -- 0 iff a versioned libndi filename (libndi.so.6.3.2.0)
#       matches PIN (6.3.2) as a DOTTED prefix -- accepts the 4-part SDK string, rejects "6.3.20".

NDI_VERSION_PIN="${NDI_VERSION_PIN:-6.3.2}"     # fleet NDI runtime pin (#132/#547; single-sourced #1066)

# ndi_bootstrap_peer_list SELF_IP -> one candidate NDI-runtime peer IP per line: NDI_PEER first
# (when set and != SELF_IP), then every CAMERA_SET member's IP resolved via camera_resolve, minus
# SELF_IP, deduped preserving order. Each camera_resolve runs in a subshell so its CAMERA_* side
# effects never leak into the caller.
ndi_bootstrap_peer_list() {
  local self="${1:-}" seen=" " ip cam
  for ip in "${NDI_PEER:-}" \
    $(for cam in ${CAMERA_SET:-}; do camera_resolve "$cam" >/dev/null 2>&1 && printf '%s\n' "$CAMERA_IP"; done); do
    [ -n "$ip" ] || continue
    [ "$ip" = "$self" ] && continue
    case "$seen" in *" $ip "*) continue ;; esac
    seen="$seen$ip "
    printf '%s\n' "$ip"
  done
}

# ndi_runtime_version_matches_pin FILENAME PIN -> 0 iff FILENAME is libndi.so.<PIN> or
# libndi.so.<PIN>.<more> (dotted-prefix match: "6.3.2" accepts "libndi.so.6.3.2.0" and
# "libndi.so.6.3.2", but never "libndi.so.6.3.20" or "libndi.so.6.2.1.0").
ndi_runtime_version_matches_pin() {
  local fn="${1:-}" pin="${2:-}"
  [ -n "$pin" ] || return 1
  case "$fn" in
    "libndi.so.$pin" | "libndi.so.$pin."*) return 0 ;;
    *) return 1 ;;
  esac
}
