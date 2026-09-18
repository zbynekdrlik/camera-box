#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/obs-liveness-watchdog.sh / scripts/imag-obs-alert-watchdog.sh (set -uo pipefail, not -e).
#
# scripts/avsync-heartbeat-alert-watchdog.sh -- #812/#807 stream-box avsync heartbeat alert, DEV1-SIDE.
#
# WHY: neither avsync-watchdog.ps1 (#812, the A/V-sync measurement loop) nor avsync-vlc-monitor.ps1
# (#807, the VLC program-audio babysitter) can alert Discord on their OWN silence -- a process that
# has crashed or hung obviously cannot report its own death, and the stream box has no
# ~/devel/airuleset checkout / Discord credentials of its own (the SAME topology gap #882's
# imag-obs-alert-watchdog.sh and #391's obs-liveness-watchdog.sh already close for their own boxes
# -- see .claude/rules/imag-obs-supervision.md). This script applies that SAME dev1-side alert
# topology to BOTH avsync heartbeats: a dev1 systemd --user timer SSHes into the stream box, reads
# both heartbeat files in ONE round-trip (scripts/lib/avsync-heartbeat.sh), and fires a Discord
# alert via airuleset.py notify the moment either heartbeat goes stale -- independent
# confirm/throttle state per leg, reusing the SAME pure scripts/lib/obs-watchdog-decision.sh #391
# already established (never a third alerting mechanism).
#
# Usage:
#   scripts/avsync-heartbeat-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/avsync-heartbeat-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/avsync-heartbeat-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/avsync-heartbeat.sh
. "$HERE/lib/avsync-heartbeat.sh"
# avsync-heartbeat.sh sets `-e` for ITS OWN sourcing safety; re-assert this script's own intended
# options afterward so a stray non-zero return from a plain assignment never aborts a pass early
# (this watchdog must survive a bad pass and keep polling on the next timer tick, see the header).
set -uo pipefail

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '5,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "avsync-heartbeat-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
# #1331: the shared lib's stream-fallback ssh prefix reads STREAM_USER/STREAM_PW/STREAM_IP directly
# (normalize them here so a sourced-lib call sees the same values; the old *_SSH aliases were retired
# when the inline sshpass call moved into avsync_heartbeat_ssh_prefix_argv).
STREAM_IP="${STREAM_IP:-10.77.9.204}"
STREAM_USER="${STREAM_USER:-newlevel}"
STREAM_PW="${STREAM_PW:-newlevel}"
# 2x avsync-watchdog.ps1's ~90s natural cadence AND 2x avsync-vlc-monitor.ps1's ~15-35s cadence,
# with comfortable margin either way -- one env override covers both legs (they run independently
# but on similar timescales; a per-leg override was not worth the extra complexity here).
STALE_S="${AVSYNC_HEARTBEAT_STALE_S:-300}"
CONFIRM_THRESHOLD="${AVSYNC_HEARTBEAT_CONFIRM_THRESHOLD:-1}"
ALERT_THROTTLE_PASSES="${AVSYNC_HEARTBEAT_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${AVSYNC_HEARTBEAT_REPO:-zbynekdrlik/camera-box}"

# issue 968 -- the DURABLE Discord verdict-forward leg (dev1-side, direct bot API POST). On
# 2026-07-26 the user WAS receiving these same verdict messages, but posted by a live agent-
# session loop straight into the alerts-snv thread -- not a durable service, which is why delivery
# died the moment that session ended. Also confirmed live: the claude_robot bot cannot mint a
# Discord webhook (no MANAGE_WEBHOOKS guild-wide), so avsync-watchdog.ps1's own
# C:\avsync\discord-webhook.txt design has no self-service path to ever get a working URL -- it
# stays wired as a harmless optional fallback, but the DELIVERED path is this dev1-side forwarder,
# mirroring this repo's own established dev1-side-alerting topology (see the file header above and
# .claude/rules/avsync-monitoring.md).
DISCORD_ENV_FILE="${AVSYNC_DISCORD_ENV:-$HOME/.claude/channels/discord/.env}"
DISCORD_THREAD_ID="${AVSYNC_DISCORD_THREAD_ID:-1373592666733940816}"   # alerts-snv thread

STATE_DIR="${AVSYNC_HEARTBEAT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
STATE_FILE="${AVSYNC_HEARTBEAT_STATE_FILE:-$STATE_DIR/camera-box-avsync-heartbeat.state}"

# #1331 -- the VERIFIED-A/V session report (replaces the raw one-clip Discord forward).
# The dev2 measurer appends every pass to a per-day TSV; this pass fetches those rows and hands them
# to the PURE session decider scripts/avsync_report.py, which returns the Discord messages to post
# (a verified summary at the start, every 20 min, on a verdict change, and at end-of-broadcast) plus
# the new report state. AVSYNC_REPORT_FETCH_CMD is the row-fetch seam the dry-run tests stub.
REPORT_SCRIPT="${AVSYNC_REPORT_SCRIPT:-$HERE/avsync_report.py}"
REPORT_PYTHON="${AVSYNC_REPORT_PYTHON:-python3}"
REPORT_STATE_FILE="${AVSYNC_REPORT_STATE_FILE:-$STATE_DIR/avsync-report-state.json}"

log() { printf '%s [avsync-heartbeat-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── measure (SSH + the shared avsync-heartbeat probe) ───────────────────────
# #1331: the transport + remote read command now come from the shared lib, selected by
# AVSYNC_HEARTBEAT_HOST (default "dev2" -- the Linux GPU measurer, key auth, no sshpass/password;
# "stream" keeps the retired Windows box's sshpass+`type` path). The stream-path STREAM_* env the
# lib reads is the SAME the config block above sets (STREAM_PW/STREAM_USER/STREAM_IP), so the
# fallback credentials are unchanged.
measure() {
  local -a pfx=()
  mapfile -t pfx < <(avsync_heartbeat_ssh_prefix_argv)
  if [ "${#pfx[@]}" -eq 0 ]; then
    log "ERROR: unknown AVSYNC_HEARTBEAT_HOST='$(avsync_heartbeat_host)' -- no probe transport; nothing to decide this pass"
    PROBE_OUT=""
    return 0
  fi
  PROBE_OUT="$("${pfx[@]}" "$(avsync_heartbeat_remote_cmd)" 2>/dev/null || true)"
}

# ── read / write persisted state (same key=value shape as the #391/#882 siblings) ──────────────
read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || echo "$STATE_FILE")"
  { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

# ── Discord verdict-forward leg (dev1-side bot POST -- issue 968) ───────────────────────────────
# Deliberately a per-key sed read (mirrors THIS file's own read_state_field convention) rather than
# sourcing the whole .env like scripts/lib/event-mode-discord-confirm.sh does -- sourcing executes
# the file as shell code; a per-key extract never does, regardless of what else might someday land
# in that file. Review-noted: this is a second, marginally more defensive, convention for the SAME
# config file rather than the sourcing one -- kept deliberately, not an oversight.
read_discord_env_field() {
  local key="$1"
  [ -f "$DISCORD_ENV_FILE" ] || { printf ''; return 0; }
  sed -n "s/^${key}=//p" "$DISCORD_ENV_FILE" 2>/dev/null | tail -1
}

# discord_mention_prefix -> "<@id> " for a bare numeric DISCORD_MENTION_ZBYNEK, the value verbatim
# + a space when it's already shaped (<@...>, @here, ...), or "" when unset -- mirrors airuleset's
# own notify.mention_prefix() semantics exactly (never invent a second mention convention).
discord_mention_prefix() {
  local val
  val="$(read_discord_env_field DISCORD_MENTION_ZBYNEK)"
  [ -n "$val" ] || return 0
  case "$val" in
    *[!0-9]*) printf '%s ' "$val" ;;
    *) printf '<@%s> ' "$val" ;;
  esac
}

# Discord's own message cap (2000 chars) minus headroom -- mirrors airuleset's own
# notify._MAX_CONTENT (~/devel/airuleset/notify/__init__.py) exactly, never a second number.
readonly DISCORD_VERDICT_MAX_CONTENT=1900

# post_discord_verdict TEXT -> POST TEXT (with the owner mention prepended) to the alerts-snv
# thread via the bot token. A missing token or a failed POST is logged loudly (including the
# response BODY on failure -- diagnosability matters here: this exact bot identity already once
# hit a Discord permission wall, see the design comment) but NEVER aborts the pass (this watchdog
# must survive and keep polling -- see the file header's set -uo pipefail convention). Bounded with
# --max-time (mirrors scripts/lib/e2e-discord-report.sh / event-mode-discord-confirm.sh's own
# Discord POST calls exactly -- never a second timeout convention) so a stalled connection can
# never wedge this pass beyond the systemd unit's own TimeoutStartSec budget. Never called in
# --dry-run (see run_report_pass).
# RETURNS 0 iff the message was DELIVERED (HTTP 200); returns 1 on a missing token or a non-200
# (still non-fatal -- the caller keeps polling). #1331: run_report_pass uses this rc to COMMIT the
# advanced dedup state only after delivery succeeds, so a transient POST failure re-emits next pass.
post_discord_verdict() {
  local text="$1" token content payload response http_code body
  token="$(read_discord_env_field DISCORD_BOT_TOKEN)"
  if [ -z "$token" ]; then
    log "VERDICT-FORWARD: no DISCORD_BOT_TOKEN configured at $DISCORD_ENV_FILE -- skipping post (non-fatal)"
    return 1
  fi
  content="$(discord_mention_prefix)${text}"
  content="${content:0:$DISCORD_VERDICT_MAX_CONTENT}"
  payload="$(jq -n --arg c "$content" '{content:$c}')"
  response="$(curl -sS --max-time 10 -w '\n%{http_code}' -X POST \
    -H "Authorization: Bot $token" \
    -H 'Content-Type: application/json' \
    -H 'User-Agent: DiscordBot (https://github.com/zbynekdrlik/airuleset, 1.0)' \
    -d "$payload" \
    "https://discord.com/api/v10/channels/${DISCORD_THREAD_ID}/messages" 2>&1)"
  http_code="${response##*$'\n'}"
  body="${response%$'\n'*}"
  if [ "$http_code" != "200" ]; then
    log "VERDICT-FORWARD: Discord POST returned HTTP '$http_code' (expected 200, non-fatal). Response body: $body"
    return 1
  fi
  log "VERDICT-FORWARD: posted to thread $DISCORD_THREAD_ID (message id: $(printf '%s' "$body" | jq -r '.id // "unknown"' 2>/dev/null))"
  return 0
}

# ── #1331 verified-A/V session report (REPLACES the raw one-clip Discord forward) ─────────
# report_fetch_rows -> emit the dev2 per-day rows ("<epoch>\t<status>", today + yesterday). The
# AVSYNC_REPORT_FETCH_CMD seam lets the dry-run tests stub the fetch; otherwise the SAME key-auth ssh
# transport the heartbeat probe uses (avsync_heartbeat_ssh_prefix_argv) cats the two day files. Never
# fatal -- a fetch failure yields no rows (the report simply skips this pass).
# #1331 (review F4): the day FILE names come from THIS box's (dev1) local date, while dev2 wrote the
# files under dev2's local date -- correct while both boxes share Europe/Bratislava (they do). The
# fetch pulls today's + YESTERDAY's file, so a row written near local midnight is still in the window;
# and aggregation is EPOCH-based (TZ-independent), so only which files are fetched depends on the day.
report_fetch_rows() {
  if [ -n "${AVSYNC_REPORT_FETCH_CMD:-}" ]; then
    bash -c "$AVSYNC_REPORT_FETCH_CMD" 2>/dev/null || true
    return 0
  fi
  local -a pfx=()
  mapfile -t pfx < <(avsync_heartbeat_ssh_prefix_argv)
  if [ "${#pfx[@]}" -eq 0 ]; then
    log "report: no ssh transport for host=$(avsync_heartbeat_host) -- no rows this pass"
    return 0
  fi
  local today yday
  today="$(date +%Y-%m-%d)"
  yday="$(date -d 'yesterday' +%Y-%m-%d 2>/dev/null || echo "$today")"
  "${pfx[@]}" "cat ~/avsync/measurements-$yday.tsv ~/avsync/measurements-$today.tsv 2>/dev/null" \
    2>/dev/null || true
}

# run_report_pass -> fetch the rows, run the PURE decider, and post each returned message (one Discord
# message per stdout line) via the SAME post_discord_verdict path the forward used. --dry-run logs
# "WOULD post" and never posts. Never fatal (the watchdog must survive and keep polling).
#
# #1331 (review F1/F2): the report state is committed TRANSACTIONALLY -- the decider runs against a
# COPY of the state, and the advanced state is committed to REPORT_STATE_FILE only AFTER every message
# in this pass was DELIVERED (post_discord_verdict returned 0). If a POST fails, the advanced state is
# DISCARDED so the next pass re-derives + re-emits (the decider is idempotent) -- a transient Discord
# outage never permanently drops a verified-A/V message (the owner's original complaint). And --dry-run
# NEVER touches the production state (it works on the discarded copy), so a manual dry-run during a
# live can never consume the real dedup and suppress a production message.
run_report_pass() {
  local rows rows_file work_state msgs line all_ok=1
  rows="$(report_fetch_rows)"
  if [ -z "$rows" ]; then
    log "report: no rows fetched this pass (nothing to aggregate)"
    return 0
  fi
  rows_file="$(mktemp 2>/dev/null)" || { log "report: mktemp failed (non-fatal)"; return 0; }
  printf '%s\n' "$rows" > "$rows_file"
  work_state="$(mktemp 2>/dev/null)" || { rm -f "$rows_file"; log "report: mktemp failed (non-fatal)"; return 0; }
  mkdir -p "$(dirname "$REPORT_STATE_FILE")" 2>/dev/null || true
  # Run the decider against a COPY of the committed state (decider rewrites --state in place).
  if [ -f "$REPORT_STATE_FILE" ]; then cp -f "$REPORT_STATE_FILE" "$work_state" 2>/dev/null || true; fi
  msgs="$("$REPORT_PYTHON" "$REPORT_SCRIPT" --state "$work_state" --rows "$rows_file" \
    2>/dev/null || true)"
  rm -f "$rows_file"
  if [ -z "$msgs" ]; then
    # No messages to deliver -> committing the advanced session-tracking is safe (nothing to lose),
    # except in --dry-run which must never mutate the production state.
    if [ "$DRY_RUN" -eq 1 ]; then rm -f "$work_state" 2>/dev/null || true; else mv -f "$work_state" "$REPORT_STATE_FILE" 2>/dev/null || rm -f "$work_state" 2>/dev/null || true; fi
    log "report: no new messages this pass"
    return 0
  fi
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if [ "$DRY_RUN" -eq 1 ]; then
      log "[dry-run] WOULD post report message: $line"
    else
      post_discord_verdict "$line" || all_ok=0
    fi
  done <<< "$msgs"
  if [ "$DRY_RUN" -eq 1 ]; then
    rm -f "$work_state" 2>/dev/null || true
  elif [ "$all_ok" -eq 1 ]; then
    mv -f "$work_state" "$REPORT_STATE_FILE" 2>/dev/null || rm -f "$work_state" 2>/dev/null || true
  else
    log "report: a Discord POST failed this pass -- discarding advanced state so the next pass re-emits"
    rm -f "$work_state" 2>/dev/null || true
  fi
}

# ── one leg's confirm/throttle/alert pass -- reused for BOTH "watchdog" and "vlc" ───────────────
process_leg() {
  local leg="$1" epoch="$2" now epoch_display wedged prev_confirm decision confirm act
  now="$(date +%s)"
  wedged=0
  if avsync_heartbeat_is_stale "$epoch" "$now" "$STALE_S"; then wedged=1; fi
  epoch_display="${epoch:-<none>}"
  log "$leg: last_heartbeat_epoch=$epoch_display now=$now stale_s=$STALE_S wedged=$wedged"

  prev_confirm="$(read_state_field "${leg}_confirm" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" "$wedged" "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "${leg}_confirm" "${confirm:-0}"
  log "$leg: confirm=$prev_confirm -> $confirm act=$act"

  if [ "$wedged" -eq 0 ]; then
    write_state_field "${leg}_alert_sig" ""
    write_state_field "${leg}_alert_passes" 0
    return 0
  fi
  [ "${act:-0}" = "1" ] || return 0

  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes
  current_sig="${leg}:stale"
  prior_sig="$(read_state_field "${leg}_alert_sig" "")"
  prior_passes="$(read_state_field "${leg}_alert_passes" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "${leg}_alert_sig" "$new_sig"
  write_state_field "${leg}_alert_passes" "$new_passes"

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $leg heartbeat CONFIRMED stale (last=$epoch_display) alert_now=$alert_now"
    return 0
  fi

  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $leg"
    python3 "$NOTIFY" notify --body \
      "🚨 A/V-sync monitor ($REPO_SLUG): heartbeat vetvy $leg na stream-boxe je STARÝ (naposledy=${epoch_display}, limit=${STALE_S}s) — monitorovanie A/V-sync/VLC na stream-boxe zrejme spadlo. Rieši Claude automaticky, ty nemusíš nič robiť." \
      --dedup-key "$(watchdog_notify_key "avsync-heartbeat-$leg" "$(date +%s)")" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle for $leg (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN, stale_s=$STALE_S, threshold=$CONFIRM_THRESHOLD)"

  measure
  if [ -z "${PROBE_OUT:-}" ]; then
    # #1331: the heartbeat legs have nothing to decide, but the report pass is INDEPENDENT (its own
    # fetch), so we skip only the legs here -- never the report.
    log "ERROR: no probe output from stream box (ssh/connectivity failure) -- skipping heartbeat legs this pass"
  else
    local watchdog_segment vlc_segment watchdog_epoch vlc_epoch
    watchdog_segment="$(avsync_heartbeat_extract_segment "$PROBE_OUT" watchdog)"
    vlc_segment="$(avsync_heartbeat_extract_segment "$PROBE_OUT" vlc)"
    watchdog_epoch="$(avsync_heartbeat_last_epoch "$watchdog_segment")"
    vlc_epoch="$(avsync_heartbeat_last_epoch "$vlc_segment")"

    process_leg "watchdog" "$watchdog_epoch"
    # #1331: the dev2 measurer has NO VLC-monitor heartbeat (that was a stream-box babysitter), so a
    # permanently-missing vlc segment must SKIP, never page as CONFIRMED-stale. The "stream" host
    # still has both legs.
    if avsync_heartbeat_has_vlc_leg; then
      process_leg "vlc" "$vlc_epoch"
    else
      log "vlc: skipped (host=$(avsync_heartbeat_host) has no VLC-monitor heartbeat leg)"
    fi
  fi

  # #1331: the verified-A/V SESSION report (replaces the raw one-clip forward). Independent fetch,
  # always run regardless of the heartbeat-leg outcome above.
  run_report_pass

  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
