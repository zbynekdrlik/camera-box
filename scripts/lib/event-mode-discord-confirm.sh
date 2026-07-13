#!/usr/bin/env bash
# airuleset:script-ok sourced pure-ish library (mirrors rig-test-dropin.sh / rig-test-ledger.sh /
# event-assert.sh) -- `set -euo pipefail` must NEVER be set here because sourcing this file
# mutates the CALLING script's (rig-mode.sh) own shell options; this file's own errexit is
# managed locally inside event_mode_discord_confirm_send (mirrors e2e-discord-report.sh's
# save/restore-errexit pattern).
#
# scripts/lib/event-mode-discord-confirm.sh — #724: EVENT-mode Discord confirmation sender.
# After the #722 assert phase runs (pass AND fail), ONE Slovak message lands in the owner's
# Discord thread with @mention — so the user never again has to trust a bare terminal claim
# about the rig being broadcast-clean. Trigger (2026-07-12, #721): the user caught a live QR by
# EYE; a phone confirmation with the checklist would have surfaced the discrepancy immediately
# (and its ABSENCE would itself have been the alarm).
#
# Delivery reuses the #719 owner-thread + @mention MODEL (scripts/lib/e2e-discord-report.sh) --
# DISCORD_NOTIFICATION_CHANNEL_ZBYNEK + DISCORD_MENTION_ZBYNEK, sourced from
# ~/.claude/channels/discord/.env when not already in the environment, never clobbering an
# already-set DISCORD_BOT_TOKEN. UNLIKE #719's sender, this one NEVER falls back to
# #notifications — per the issue's own instruction ("share the sender; do NOT post to
# #notifications"): when the owner vars are genuinely absent, log loudly and SKIP sending
# entirely (fail-open — a missing confirmation must never fail rig-mode.sh event, and posting a
# rig-cleanliness confirmation to a channel nobody reads defeats the whole point).
#
# Sourced by scripts/rig-mode.sh.

# event_mode_discord_confirm_send MESSAGE -> post MESSAGE (already fully composed, Slovak,
# phone-readable — see scripts/event_assert.py::format_discord_message_sk) to the owner's
# Discord thread with an @mention prefix. ALWAYS returns 0 (fail-open, mirrors
# e2e_discord_report_send) — a Discord/network/config failure here can NEVER abort or change
# do_event()'s own exit code (which reflects the #722 assert verdict, not this notification).
event_mode_discord_confirm_send() {
  local _emdc_prev_errexit=0
  case "$-" in *e*) _emdc_prev_errexit=1 ;; esac
  set +e
  _event_mode_discord_confirm_send_inner "$@"
  if [ "$_emdc_prev_errexit" = "1" ]; then set -e; fi
  return 0
}

_event_mode_discord_confirm_send_inner() {
  local message="$1"

  # Source the local .env whenever the bot token OR either owner-thread var is still missing
  # (mirrors e2e-discord-report.sh's #719 precedence: CI/operator env always wins over whatever
  # value happens to sit in the local .env).
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
    echo "WARNING: [#724] event_mode_discord_confirm_send: DISCORD_BOT_TOKEN not set -- skipping the EVENT-mode Discord confirmation (fail-open)." >&2
    return 0
  fi

  # #724, UNLIKE #719: owner vars genuinely absent -> SKIP (never fall back to #notifications).
  if [ -z "${DISCORD_NOTIFICATION_CHANNEL_ZBYNEK:-}" ]; then
    echo "WARNING: [#724] event_mode_discord_confirm_send: DISCORD_NOTIFICATION_CHANNEL_ZBYNEK not set (neither in env nor \$HOME/.claude/channels/discord/.env) -- skipping the EVENT-mode confirmation (never falls back to #notifications; nobody reads that channel)." >&2
    return 0
  fi

  local mention_prefix=""
  if [ -n "${DISCORD_MENTION_ZBYNEK:-}" ]; then
    mention_prefix="<@${DISCORD_MENTION_ZBYNEK}> "
  else
    echo "WARNING: [#724] event_mode_discord_confirm_send: DISCORD_MENTION_ZBYNEK not set -- posting to the owner thread (channel $DISCORD_NOTIFICATION_CHANNEL_ZBYNEK) WITHOUT an @mention (no phone push)." >&2
  fi

  local content payload response http_code body
  content="${mention_prefix}${message}"
  payload="$(jq -n --arg c "$content" '{content:$c}')"
  response="$(curl -sS --max-time 10 -w '\n%{http_code}' -X POST \
    -H "Authorization: Bot ${DISCORD_BOT_TOKEN}" \
    -H "Content-Type: application/json" \
    -d "$payload" \
    "https://discord.com/api/v10/channels/${DISCORD_NOTIFICATION_CHANNEL_ZBYNEK}/messages" 2>&1)"
  http_code="${response##*$'\n'}"
  body="${response%$'\n'*}"
  if [ "$http_code" != "200" ]; then
    echo "WARNING: [#724] event_mode_discord_confirm_send: Discord POST returned HTTP '$http_code' (expected 200) -- logging loudly, do_event's own exit code unaffected. Response body:" >&2
    echo "$body" >&2
    return 0
  fi
  echo "[#724] EVENT-mode Discord confirmation posted (message id: $(printf '%s' "$body" | jq -r '.id // "unknown"'))."
  return 0
}
