#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure I/O helper, no top-level statements) -- deliberately NOT
# `set -euo pipefail`: sourcing this into a caller must never leak `set -e` into it (the standing
# .claude/rules/ci-testing-gotchas.md rule); every caller owns its own strictness.
#
# scripts/lib/watchdog-tcp-probe.sh -- the shared TCP-open reachability probe for dev1
# alert-watchdogs (#1308). This is the EXTRACTED mechanism of network-reach-alert-watchdog.sh's
# `probe_tcp` (bash /dev/tcp + `timeout`, no nc/netstat dependency) -- a NEW watchdog reuses THIS
# instead of hand-rolling a novel probe. It is the SAME mechanism, not a new one; the dante-clock
# watchdog (#1308) uses it for its box-up-ness discrimination. network-reach + bundle-state still
# carry their own older inline copies (converging them onto this lib is a follow-up, not this ticket).
#
# watchdog_probe_tcp <ip> <port> [timeout_s]
#   -> stdout: 1 (a TCP connect to ip:port succeeded within timeout_s) | 0 (refused/filtered/timeout)
#   ALWAYS rc 0 (prints 0 on failure), so a caller under `set -e` never aborts on a closed port.
watchdog_probe_tcp() {
  local ip="$1" port="$2" timeout_s="${3:-4}"
  # $ip/$port are passed as positional args to the INNER bash ($0/$1), never interpolated into the
  # -c string, so a config value can never be shell-injected into the probe. The single quotes are
  # DELIBERATE: $0/$1 must expand in the inner bash, not here.
  # shellcheck disable=SC2016
  if timeout "$timeout_s" bash -c 'exec 3<>/dev/tcp/"$0"/"$1"' "$ip" "$port" >/dev/null 2>&1; then
    printf '1'
  else
    printf '0'
  fi
}
