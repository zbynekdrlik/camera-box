#!/usr/bin/env bash
# imag-wallpaper-refresh.sh — keep the wall-fallback desktop background CURRENT (#791/#800 family).
#
# WHY: the fallback background (shown by the LED wall whenever OBS is down) is a still of the
# 'resolume imag' scene. A one-shot screenshot goes STALE — live incident 2026-07-18: an evening
# OBS crash exposed a fallback still carrying the PREVIOUS band's logos. This script re-grabs the
# still every run while OBS is healthy, so the fallback is never older than the timer cadence.
#
# #882: this script ALREADY detected "obs not running" every 5 minutes and said nothing to anyone
# — the 2026-07-30 outage sat behind it for 70 minutes, logged 14 times, alerting nobody. The
# detection was already free; only the ALERT was missing. Reuses the #391 liveness-watchdog's own
# pure decision functions (confirm-over-N-passes + throttled re-alert) and the SAME
# `airuleset.py notify` path — never a second poller/alert mechanism. Confirm threshold is 1 (not
# #391's 2): this timer's own cadence is 5 minutes, not #391's 4-second polling window, so a
# single miss is already ~5 minutes of real downtime, not a transient blip to filter out.
#
# Install (systemd user timer, every 5 min):
#   systemctl --user enable --now imag-wallpaper-refresh.timer
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${IMAG_WALLPAPER_REPO:-zbynekdrlik/camera-box}"
STATE_FILE="${IMAG_WALLPAPER_STATE_FILE:-${XDG_RUNTIME_DIR:-/tmp}/imag-wallpaper-obs-alert.state}"
ALERT_THROTTLE_PASSES="${IMAG_WALLPAPER_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence

log() { printf '%s [imag-wallpaper-refresh] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

read_state() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state() {
  local key="$1" val="$2" tmp
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || echo "$STATE_FILE")"
  { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

# alert_obs_down — confirm over 1 pass (a single miss at this 5-min cadence is already real
# downtime), then throttle re-alerts so a sustained outage pages once, not every 5 minutes.
alert_obs_down() {
  local prev_confirm decision confirm act
  prev_confirm="$(read_state obs_down_confirm 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 1)"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state obs_down_confirm "${confirm:-0}"
  [ "${act:-0}" = "1" ] || return 0

  local prior_sig prior_passes throttle_out alert_now new_passes
  prior_sig="$(read_state alert_sig "")"
  prior_passes="$(read_state alert_passes 0)"
  throttle_out="$(obs_watchdog_alert_throttle "obs-down" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state alert_sig "obs-down"
  write_state alert_passes "${new_passes:-1}"

  [ "${alert_now:-0}" = "1" ] || { log "alert suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"; return 0; }

  log "ALERT: imag-nb OBS is down -- firing Discord notification"
  python3 "$NOTIFY" notify --body \
    "🚨 #882 imag-wallpaper-refresh: imag-nb OBS is NOT RUNNING ($REPO_SLUG) — the audience-facing projection is behind the wall-fallback image. Restart it: ssh into imag-nb and run /usr/local/bin/imag-obs-start.sh (or once supervised, \`systemctl --user start imag-obs.service\`)." \
    >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
}

# obs healthy again -> reset the confirm/throttle state so the NEXT outage alerts promptly
# instead of inheriting a stale throttle counter from a previous, already-recovered episode.
clear_alert_state() {
  write_state obs_down_confirm 0
  write_state alert_sig ""
  write_state alert_passes 0
}

# main — the real screenshot-refresh flow. Split out (rather than left as top-level statements)
# so tests can SOURCE this file (running only the function defs above, per the guard at the
# bottom, mirroring scripts/obs-liveness-watchdog.sh's convention) to exercise alert_obs_down /
# clear_alert_state directly, without a real OBS/websocket/feh present.
main() {
  local out="$HOME/Pictures/wall-fallback.png"
  local tmp="$out.tmp"

  # OBS down -> keep the last good image (that is the whole point of the fallback) AND alert.
  pgrep -x obs >/dev/null || { echo "obs not running — keeping existing fallback"; alert_obs_down; return 0; }
  clear_alert_state

  python3 - "$tmp" <<'PY'
import base64
import json
import sys

from websocket import create_connection

ws = create_connection("ws://127.0.0.1:4455", timeout=10)
json.loads(ws.recv())
ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1}}))
json.loads(ws.recv())
ws.send(json.dumps({"op": 6, "d": {"requestType": "GetSourceScreenshot", "requestId": "x",
                                   "requestData": {"sourceName": "resolume imag",
                                                   "imageFormat": "png",
                                                   "imageWidth": 1920, "imageHeight": 1080}}}))
while True:
    m = json.loads(ws.recv())
    if m.get("op") == 7 and m["d"]["requestId"] == "x":
        st = m["d"]["requestStatus"]
        if not st["result"]:
            sys.exit(f"GetSourceScreenshot failed: {st.get('code')} {st.get('comment', '')}")
        data = m["d"]["responseData"]["imageData"].split(",", 1)[1]
        with open(sys.argv[1], "wb") as fh:
            fh.write(base64.b64decode(data))
        break
PY

  # Atomic replace + re-apply, so a crash mid-write can never leave a corrupt fallback.
  mv -f "$tmp" "$out"
  export DISPLAY="${DISPLAY:-:0}" XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}"
  feh --no-fehbg --bg-fill "$out"
  echo "fallback refreshed: $(date -Is)"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
