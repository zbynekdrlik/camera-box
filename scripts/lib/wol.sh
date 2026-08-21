# shellcheck shell=bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (network-reach-health.sh, obs-watchdog-decision.sh,
# optical-chain-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (scripts/wake-box.sh) sets its own strict mode.
#
# scripts/lib/wol.sh -- #1053: the SHARED, PURE Wake-on-LAN logic for the dev1-side remote-recovery
# magic-packet sender. No I/O, no network, no ssh -- pure string/table transforms only, so it can be
# unit-tested exhaustively (mirrors scripts/lib/network-reach-health.sh, the issue-1001 counterpart
# lib, which is likewise a source-only pure decision core with no side effects at source time).
#
# WHY (#1053): issue 1001 ALERTS on a strih/stream network outage; this makes the outage remotely
# RECOVERABLE by sending a Wake-on-LAN magic packet from dev1. The magic packet's construction (6x
# 0xFF sync stream + 16x the target MAC), the MAC normalization, and the box->ip->mac table lookup are
# all deterministic pure logic and live HERE; the actual UDP broadcast (which needs SO_BROADCAST, a
# capability bash /dev/udp lacks) is the only impure part and lives in the thin wake-box.sh wrapper.
#
# Source-only: pure functions, no side effects at source time.

# wol_normalize_mac <mac> -> stdout: canonical AA:BB:CC:DD:EE:FF (uppercase, colon-separated).
#   Accepts any common input form -- colon (5c:6a:80:f6:6c:f7), hyphen (5C-6A-80-F6-6C-F7, the form
#   Get-NetAdapter's MacAddress prints), dot-grouped (5c6a.80f6.6cf7) or bare 12 hex nibbles. Fail-loud
#   (return 2 + a stderr diagnostic) on anything that is not EXACTLY 12 hex nibbles -- a WoL packet
#   built from a malformed MAC would silently wake nothing, so a bad MAC must stop the send, never be
#   coerced. Pure.
wol_normalize_mac() {
  local raw="${1:-}" hex
  hex="${raw//[:.-]/}"   # strip the three common group separators
  hex="${hex// /}"       # and any stray whitespace
  if [ "${#hex}" -ne 12 ] || grep -qiE '[^0-9a-f]' <<<"$hex"; then
    printf 'wol_normalize_mac: invalid MAC %q (need exactly 12 hex nibbles)\n' "$raw" >&2
    return 2
  fi
  hex="${hex^^}"
  printf '%s:%s:%s:%s:%s:%s\n' \
    "${hex:0:2}" "${hex:2:2}" "${hex:4:2}" "${hex:6:2}" "${hex:8:2}" "${hex:10:2}"
}

# wol_magic_packet_hex <mac> -> stdout: the 102-byte WoL magic packet as ONE lowercase hex string
#   (204 hex chars) -- the 6-byte 0xFF synchronization stream followed by 16 repetitions of the
#   6-byte target MAC, exactly as the Wake-on-LAN standard defines. Normalizes the MAC first (so any
#   accepted input form works) and propagates wol_normalize_mac's fail-loud on an invalid MAC. Pure.
wol_magic_packet_hex() {
  local canon machex out
  canon="$(wol_normalize_mac "${1:-}")" || return $?
  machex="$(printf '%s' "$canon" | tr -d ':' | tr 'A-F' 'a-f')"   # 12 lowercase hex nibbles
  out="ffffffffffff"                                              # 6x 0xFF sync stream
  for _ in $(seq 1 16); do out="${out}${machex}"; done            # 16x the MAC
  printf '%s\n' "$out"
}

# wol_table_lookup <table-text> <box> <field> -> stdout: the requested field's value.
#   Operates on the PASSED-IN table text (so it is pure/testable -- the file read lives in the caller),
#   whitespace-separated "box  ip  mac" rows, `#` comment lines and blank lines ignored. <field> is one
#   of: ip | mac | broadcast (broadcast is DERIVED from the ip's /24 as a.b.c.255, never stored, so the
#   table has a single source of truth per box). Fail-loud (return 2 + stderr) on an unknown box or an
#   unknown field. Pure.
wol_table_lookup() {
  local table="${1:-}" box="${2:-}" field="${3:-}" line ip mac
  line="$(printf '%s\n' "$table" | grep -vE '^[[:space:]]*(#|$)' | awk -v b="$box" '$1==b {print; exit}')"
  if [ -z "$line" ]; then
    printf 'wol_table_lookup: unknown box %q (not in table)\n' "$box" >&2
    return 2
  fi
  read -r _ ip mac _ <<<"$line"
  case "$field" in
    ip)        printf '%s\n' "$ip" ;;
    mac)       printf '%s\n' "$mac" ;;
    broadcast) printf '%s\n' "${ip%.*}.255" ;;
    *) printf 'wol_table_lookup: unknown field %q (want ip|mac|broadcast)\n' "$field" >&2; return 2 ;;
  esac
}

# wol_verify_host <table-text> <target> <override> -> stdout: the IP to poll for a post-wake
#   reachability check (wake-box.sh --wait). Precedence: a non-empty <override> (the --wait-host
#   value) wins; else a <target> that is a known box in the table resolves to its `ip`; else (a raw
#   MAC or an unknown box -- neither carries an IP) fail-loud (return 2 + stderr). A --wait verify
#   must NEVER silently poll the wrong host or no host, so an unresolvable target is a hard error, not
#   a skipped check. Pure (delegates the box lookup to wol_table_lookup).
wol_verify_host() {
  local table="${1:-}" target="${2:-}" override="${3:-}" ip
  if [ -n "$override" ]; then
    printf '%s\n' "$override"
    return 0
  fi
  if ip="$(wol_table_lookup "$table" "$target" ip 2>/dev/null)"; then
    printf '%s\n' "$ip"
    return 0
  fi
  printf 'wol_verify_host: cannot verify %q -- not a known box (give --wait-host <ip> for a raw MAC)\n' "$target" >&2
  return 2
}
