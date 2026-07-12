#!/usr/bin/env bash
set -euo pipefail
# scripts/lib/e2e-discord-report.sh — #711 Discord full-report sender for the full-path E2E.
#
# User directive (2026-07-12, issue #711): after EVERY full-path E2E run — CI PR gate AND a
# manual/supervisor-driven run — a Discord notification MUST go out with a full report of what
# the test did and found (per-camera zero-loss, latency stability, video sync, A/V sync, overall
# verdict + blocking gates). Before this, the user had zero visibility into test executions.
#
# The REPORT TEXT is composed by scripts/e2e_discord_report.py (pure, fixture-tested — see
# tests/python/test_e2e_discord_report.py). This file's ONLY job is: call the composer, then POST
# the result to Discord using the SAME mechanism ci.yml / full-path-e2e.yml's existing "Discord
# alert" steps already use (Bot token REST POST to #notifications) — see .claude/skills/ci
# "Discord CI Notifications (#25)". REUSE that path, never build a second sender.
#
# FAIL-OPEN (issue #711 requirement, verbatim): "Fail-open on notify errors (a failed Discord
# post must never fail the gate), but LOG loudly." e2e_discord_report_send() therefore runs its
# entire body with `set +e` (restoring the caller's errexit setting before returning) and returns
# 0 on every path — a Discord/network/jq/python failure here can NEVER abort or change the exit
# code of scripts/recording-e2e.sh (which runs under `set -euo pipefail` and treats its own $GATE,
# the REAL zero-loss/A/V verdict, as the only thing that may fail the run).
#
# DISCORD_BOT_TOKEN / DISCORD_CHANNEL_ID resolution:
#   - CI (full-path-e2e.yml): passed as real GitHub Actions secrets in the E2E step's env.
#   - Manual/supervisor-driven run on dev1 (recording-e2e.sh invoked directly, outside CI): falls
#     back to sourcing ~/.claude/channels/discord/.env for DISCORD_BOT_TOKEN — ONLY when it is
#     not already present in the environment, so CI's real secret is never shadowed. The channel
#     id (#notifications, 1257652233714270219) is not a secret — same default the existing
#     workflow "Discord alert" steps use — overridable via DISCORD_CHANNEL_ID.

# e2e_discord_report_send <verdict-json-path> <run-id> <gate-exit-code> <duration-secs>
# Public entrypoint — ALWAYS returns 0 (fail-open). See header comment above.
e2e_discord_report_send() {
  local _e2e_prev_errexit=0
  case "$-" in *e*) _e2e_prev_errexit=1 ;; esac
  set +e
  _e2e_discord_report_send_inner "$@"
  if [ "$_e2e_prev_errexit" = "1" ]; then set -e; fi
  return 0
}

_e2e_discord_report_send_inner() {
  local verdict_json="$1" run_id="$2" gate_exit="$3" duration_secs="$4"
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

  if [ ! -s "$verdict_json" ]; then
    echo "WARNING: #711 e2e_discord_report_send: verdict JSON '$verdict_json' missing/empty — skipping Discord report (fail-open, gate unaffected)." >&2
    return 0
  fi

  if [ -z "${DISCORD_BOT_TOKEN:-}" ]; then
    if [ -f "$HOME/.claude/channels/discord/.env" ]; then
      # shellcheck disable=SC1090,SC1091
      . "$HOME/.claude/channels/discord/.env"
    fi
  fi
  local channel_id="${DISCORD_CHANNEL_ID:-1257652233714270219}" # #notifications (.claude/skills/ci)

  if [ -z "${DISCORD_BOT_TOKEN:-}" ]; then
    echo "WARNING: #711 e2e_discord_report_send: DISCORD_BOT_TOKEN not set (neither in env nor \$HOME/.claude/channels/discord/.env) — skipping Discord report (fail-open)." >&2
    return 0
  fi

  local event
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    event="CI PR gate"
  else
    event="manuálny beh (recording-e2e.sh)"
  fi

  local chunks_json
  chunks_json="$(python3 "$here/../e2e_discord_report.py" \
    --json "$verdict_json" --run-id "$run_id" --event "$event" \
    --duration "$duration_secs" --gate-exit "$gate_exit" --json-chunks 2>&1)"
  if [ $? -ne 0 ]; then
    echo "WARNING: #711 e2e_discord_report_send: report composer failed — skipping Discord report (fail-open). Output:" >&2
    echo "$chunks_json" >&2
    return 0
  fi

  local n
  n="$(printf '%s' "$chunks_json" | jq 'length' 2>/dev/null)"
  if [ -z "$n" ] || [ "$n" -lt 1 ] 2>/dev/null; then
    echo "WARNING: #711 e2e_discord_report_send: composer produced no message chunks — skipping (fail-open). Raw output:" >&2
    echo "$chunks_json" >&2
    return 0
  fi

  echo "#711: posting $n Discord message chunk(s) for the full-path E2E report (run $run_id)..."
  local i content payload response http_code body
  i=0
  while [ "$i" -lt "$n" ]; do
    content="$(printf '%s' "$chunks_json" | jq -r ".[$i]")"
    payload="$(jq -n --arg c "$content" '{content:$c}')"
    response="$(curl -sS --max-time 10 -w '\n%{http_code}' -X POST \
      -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
      -H "Content-Type: application/json" \
      -d "$payload" \
      "https://discord.com/api/v10/channels/${channel_id}/messages" 2>&1)"
    http_code="${response##*$'\n'}"
    body="${response%$'\n'*}"
    if [ "$http_code" != "200" ]; then
      echo "WARNING: #711 e2e_discord_report_send: Discord POST chunk $((i + 1))/$n returned HTTP '$http_code' (expected 200) — logging loudly, gate unaffected. Response body:" >&2
      echo "$body" >&2
      return 0
    fi
    echo "#711: Discord report chunk $((i + 1))/$n posted (message id: $(printf '%s' "$body" | jq -r '.id // "unknown"'))."
    i=$((i + 1))
  done
  return 0
}
