#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# same source-only convention as scripts/lib/bundle-state-health.sh / network-reach-health.sh:
# sourcing this file runs it in the CALLER's shell, so `set -euo pipefail` here would leak into
# whichever caller sources it (recording-e2e.sh, whose [0/8] gate MUST survive a failed per-box
# fetch). The caller sets its own strict mode.
#
# scripts/lib/bundle-state-selfheal.sh -- #817: dev1-side SELF-HEAL for the strih/stream `:8899`
# BundleStateServer, used by recording-e2e.sh's [0/8] version-integrity gate. When the gate's own
# curl to /bundle-state.json fails, rather than refuse the whole E2E run with a MISLEADING
# version-drift note (the pre-#817 "the win-* MCP holder must write the drift-guard observed values"
# text -- which cost three CI runs and misdirected a worker to hunt a genlock parity problem,
# issue-817 comment 2026-08-02), it issues the SAME session-agnostic restart the dev1 issue-732
# watchdog uses -- `schtasks /run /tn "BundleStateServer"` over ssh (a HIDDEN headless supervisor
# task; NEVER the `/it` interactive form, a documented dead end on these boxes) -- then re-fetches
# over a bounded retry window before giving up with an HONEST one-line fault.
#
# WHY here and not only in issue-732's watchdog: that watchdog restarts on a 5-min cadence, which
# closes the STEADY-STATE gap; but an E2E run launched inside the reboot -> next-watchdog-pass
# window still hits a dead :8899 and refuses (exit 11). The gate now heals itself in that window.
#
# Source-only: pure functions, no side effects at source time.

# bundle_state_down_message <box> <port> -> one HONEST human line naming the fault (#817).
#   The ticket's exact ask: say "bundle-state-server DOWN on <box> (nothing on :<port>)" instead of
#   the pre-#817 note that described the manual win-* MCP workaround rather than the fault. Pure
#   string (single-quoted printf format -- the backticks and `schtasks` token are LITERAL, never a
#   command substitution), so the wording is pinned by a Tier-0 test.
bundle_state_down_message() {
  local box="${1:-?}" port="${2:-8899}"
  printf 'bundle-state-server DOWN on %s (nothing on :%s) -- automatic self-heal did not restore it; the version-integrity gate will refuse (exit 11). Recover: run `schtasks /run /tn "BundleStateServer"` on %s (or check scripts/run-bundle-state-server.ps1).\n' "$box" "$port" "$box"
}

# bundle_state_selfheal_fetch <host> <dest> <port> <ssh_user> <ssh_pw> -> exit 0 iff, after a
#   session-agnostic restart + a bounded retry, /bundle-state.json was (re)fetched to <dest>.
#   Impure (ssh + curl). Called by recording-e2e.sh's fetch_box_state ONLY on its own curl failure,
#   so the ssh restart never runs on the healthy path (zero cost when :8899 is up). Retry bounds are
#   env-overridable so the Tier-0 harness runs instantly:
#     BUNDLE_STATE_SELFHEAL_TRIES       (default 6)   -- curl polls after the restart
#     BUNDLE_STATE_SELFHEAL_SLEEP_S     (default 5)   -- sleep between polls (~30s of sleeps; up
#                                                       to ~85s worst-case if a wedged-but-listening
#                                                       server makes each curl burn its full --max-time)
#     BUNDLE_STATE_SELFHEAL_SSH_TIMEOUT (default 15)  -- hard cap on the ssh restart call
#   On failure prints bundle_state_down_message to stderr and returns 1. Never `set -e`-sensitive:
#   a failed ssh/curl is an expected branch here, not a fatal.
bundle_state_selfheal_fetch() {
  local host="$1" dest="$2" port="${3:-8899}" ssh_user="${4:-newlevel}" ssh_pw="${5:-newlevel}"
  local tries="${BUNDLE_STATE_SELFHEAL_TRIES:-6}"
  local sleep_s="${BUNDLE_STATE_SELFHEAL_SLEEP_S:-5}"
  local ssh_timeout="${BUNDLE_STATE_SELFHEAL_SSH_TIMEOUT:-15}"
  # Reuse the issue-732 lib's exact, safety-pinned restart command (ONE source of truth -- never
  # re-invent the schtasks string here). Lazy-source so this stays a pure source-only lib.
  command -v bundle_state_restart_remote_cmd >/dev/null 2>&1 \
    || . "${BASH_SOURCE[0]%/*}/bundle-state-health.sh"
  echo "    bundle-state-server on ${host}:${port} not answering -- issuing session-agnostic restart (schtasks /run /tn BundleStateServer, #817)" >&2
  if command -v sshpass >/dev/null 2>&1; then
    timeout "$ssh_timeout" sshpass -p "$ssh_pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
      "${ssh_user}@${host}" "$(bundle_state_restart_remote_cmd)" >/dev/null 2>&1 \
      || echo "    NOTE: the schtasks /run restart call to ${host} did not return cleanly (ssh/creds?) -- still polling :${port}" >&2
  else
    echo "    NOTE: sshpass unavailable -- cannot issue the restart; only polling :${port} (#817)" >&2
  fi
  local i
  for i in $(seq 1 "$tries"); do
    if curl -fsS --max-time 30 -o "$dest" "http://${host}:${port}/bundle-state.json" 2>/dev/null; then
      return 0
    fi
    [ "$i" -lt "$tries" ] && sleep "$sleep_s"
  done
  bundle_state_down_message "$host" "$port" >&2
  return 1
}
