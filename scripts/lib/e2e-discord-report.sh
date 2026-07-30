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
#
# #719 — OWNER THREAD + @MENTION (the actual delivery target, per milestone-notifications.md's
# owner-thread model): #notifications above is a channel the user does NOT watch, and Discord
# only push-notifies a phone on an @mention (or a DM) — so every #711 report posted there was
# silently invisible. This sender now PREFERS DISCORD_NOTIFICATION_CHANNEL_ZBYNEK (the owner's
# own Discord thread) and prefixes the FIRST message chunk with <@$DISCORD_MENTION_ZBYNEK> (the
# push trigger). Both vars live in the SAME ~/.claude/channels/discord/.env file already sourced
# for DISCORD_BOT_TOKEN above — but full-path-e2e.yml passes ONLY DISCORD_BOT_TOKEN/
# DISCORD_CHANNEL_ID as GitHub secrets, never the owner vars, so that .env is sourced whenever
# EITHER the owner-channel or the mention var is still missing — even when DISCORD_BOT_TOKEN is
# already set from a real CI secret (this runs on the dev1 self-hosted runner, so the local .env
# is always right there). A preset DISCORD_BOT_TOKEN is restored afterward so CI's real secret is
# NEVER overwritten by whatever token value happens to sit in the local .env.
# #notifications is now a LOGGED FALLBACK, used ONLY when the owner vars are genuinely absent.

# e2e_discord_report_send <verdict-json-path> <run-id> <gate-exit-code> <duration-secs> [pins-json-path]
# Public entrypoint — ALWAYS returns 0 (fail-open). See header comment above.
# #756 Member 3: the optional 5th arg is the path scripts/latency_pins_snapshot.py wrote (live
# genlock latency pins + this run's own recommended pins) — omit it (or pass "") to compose the
# report WITHOUT the pins section, unchanged from before #756.
e2e_discord_report_send() {
  local _e2e_prev_errexit=0
  case "$-" in *e*) _e2e_prev_errexit=1 ;; esac
  set +e
  _e2e_discord_report_send_inner "$@"
  if [ "$_e2e_prev_errexit" = "1" ]; then set -e; fi
  return 0
}

_e2e_discord_report_send_inner() {
  local verdict_json="$1" run_id="$2" gate_exit="$3" duration_secs="$4" pins_json="${5:-}"
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

  if [ ! -s "$verdict_json" ]; then
    echo "WARNING: #711 e2e_discord_report_send: verdict JSON '$verdict_json' missing/empty — skipping Discord report (fail-open, gate unaffected)." >&2
    return 0
  fi

  # #719: source the local .env whenever the bot token OR either owner-thread var is still
  # missing (CI never passes the owner vars as secrets — see the header comment above). Preserve
  # an already-set DISCORD_BOT_TOKEN across the source (CI's real secret must win over whatever
  # value happens to be in the local .env).
  local _preset_bot_token="${DISCORD_BOT_TOKEN:-}"
  if [ -z "${DISCORD_BOT_TOKEN:-}" ] || [ -z "${DISCORD_NOTIFICATION_CHANNEL_ZBYNEK:-}" ] || [ -z "${DISCORD_MENTION_ZBYNEK:-}" ]; then
    if [ -f "$HOME/.claude/channels/discord/.env" ]; then
      # shellcheck disable=SC1090,SC1091
      . "$HOME/.claude/channels/discord/.env"
    fi
  fi
  if [ -n "$_preset_bot_token" ]; then
    DISCORD_BOT_TOKEN="$_preset_bot_token"
  fi

  if [ -z "${DISCORD_BOT_TOKEN:-}" ]; then
    echo "WARNING: #711 e2e_discord_report_send: DISCORD_BOT_TOKEN not set (neither in env nor \$HOME/.claude/channels/discord/.env) — skipping Discord report (fail-open)." >&2
    return 0
  fi

  # #719: prefer the owner's own Discord thread (push-notifies via @mention); #notifications is
  # a logged fallback only, used when the owner vars are genuinely unavailable.
  local channel_id mention_prefix=""
  if [ -n "${DISCORD_NOTIFICATION_CHANNEL_ZBYNEK:-}" ]; then
    channel_id="$DISCORD_NOTIFICATION_CHANNEL_ZBYNEK"
    if [ -n "${DISCORD_MENTION_ZBYNEK:-}" ]; then
      mention_prefix="<@${DISCORD_MENTION_ZBYNEK}> "
    else
      echo "WARNING: #719 e2e_discord_report_send: DISCORD_MENTION_ZBYNEK not set — posting to the owner thread (channel $channel_id) WITHOUT an @mention (no phone push)." >&2
    fi
    echo "#719: e2e_discord_report_send: routing the E2E report to the owner's Discord thread (channel $channel_id)."
  else
    channel_id="${DISCORD_CHANNEL_ID:-1257652233714270219}" # #notifications fallback (.claude/skills/ci)
    echo "WARNING: #719 e2e_discord_report_send: DISCORD_NOTIFICATION_CHANNEL_ZBYNEK not set (neither in env nor \$HOME/.claude/channels/discord/.env) — falling back to #notifications channel $channel_id with NO @mention (report will NOT push-notify the owner)." >&2
  fi

  local event
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    event="CI PR gate"
  else
    event="manuálny beh (recording-e2e.sh)"
  fi

  local pins_arg=()
  if [ -n "$pins_json" ] && [ -s "$pins_json" ]; then
    pins_arg=(--pins-json "$pins_json")
  fi

  local chunks_json
  chunks_json="$(python3 "$here/../e2e_discord_report.py" \
    --json "$verdict_json" --run-id "$run_id" --event "$event" \
    --duration "$duration_secs" --gate-exit "$gate_exit" "${pins_arg[@]}" --json-chunks 2>&1)"
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
    if [ "$i" -eq 0 ] && [ -n "$mention_prefix" ]; then
      content="${mention_prefix}${content}" # #719: the push trigger, chunk 1 only
    fi
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
