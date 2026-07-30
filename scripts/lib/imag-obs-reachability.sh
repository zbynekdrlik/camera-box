#!/bin/bash
# #882 -- imag-nb's OBS process being simply NOT RUNNING must be distinguished from "the process
# is up but WebSocket port 4455 isn't listening yet" and from a deeper failure (handshake/auth or
# no matching monitor, both already handled inside scripts/obs_phase2.py::open_projectors).
set -euo pipefail
#
# WHY: the 2026-07-30 imag-nb outage this issue investigates left every subsequent preflight
# failure reading:
#
#   ERROR: [preflight] FAIL: imag-nb (10.77.9.182): could not open the Multiview/Program
#          projectors — check imag-nb's OBS WebSocket is reachable and DP-0/HDMI-0 are actually
#          connected monitors.
#
# Both named connectors are WRONG for this box (it has eDP-1/HDMI-1; DP-1 is the disconnected
# one) and the single true fact -- OBS was not running at all, :4455 not listening, no process --
# was absent from the message. A one-line honest diagnosis would have replaced ~30 minutes of
# investigation (the user's own words on the ticket).
#
# This is the REMOTE counterpart of scripts/lib/imag-require-remote-tool.sh's #833 pattern: a
# command-builder function prints a bash snippet embedded via $(...) into an ssh command string,
# always exits 0 on the remote side, and prints exactly ONE line describing what it found. The
# CALLER (recording-e2e.sh's [0/8] preflight) decides what that line means -- never masked by an
# outer `|| true`.
#
# Usage:
#   probe="$(sshpass -p "$PW" ssh ... "$HOST" "$(imag_obs_reachability_probe_cmd)" 2>/dev/null || true)"
#   msg="$(imag_obs_reachability_message "$probe")"
#   [ -z "$msg" ] || { echo "ERROR: [preflight] FAIL: imag-nb ($HOST): $msg" >&2; exit 1; }
#   # else: process is running AND port 4455 is listening -- proceed to the real open-projectors
#   # attempt, whose own errors (handshake/auth, no matching monitor) are already accurate.

# imag_obs_reachability_probe_cmd [port] -> prints a REMOTE bash snippet. Checks, IN ORDER:
#   1. is an `obs` process running at all (`pgrep -x obs`) -- if not, prints OBS_PROCESS_ABSENT
#      and stops (this was the actual 2026-07-30 cause; short-circuits regardless of port state).
#   2. is the WebSocket port listening (`/dev/tcp` connect attempt, no netstat/ss dependency) --
#      if not, prints OBS_PORT_NOT_LISTENING.
#   3. otherwise prints OBS_REACHABLE (does NOT mean OBS is healthy -- only that a further
#      attempt, e.g. open-projectors, is worth making; a handshake/auth or monitor-selection
#      failure from THAT point on is a genuinely different, already-labelled cause).
# `port` defaults to 4455 and is substituted HERE (locally, at command-string build time) --
# deliberately an UNQUOTED heredoc so only `$port` expands, nothing else in the body needs it.
imag_obs_reachability_probe_cmd() {
  local port="${1:-4455}"
  cat <<EOF
if ! pgrep -x obs >/dev/null 2>&1; then
  echo "OBS_PROCESS_ABSENT"
elif (exec 3<>/dev/tcp/127.0.0.1/$port) 2>/dev/null; then
  echo "OBS_REACHABLE"
else
  echo "OBS_PORT_NOT_LISTENING"
fi
EOF
}

# imag_obs_reachability_message PROBE_OUTPUT -> pure parser. Returns the honest preflight FAIL
# message text for a non-reachable probe result, or an EMPTY string when the probe reports
# OBS_REACHABLE (the caller then proceeds to the real attempt). Never names a hardcoded connector
# — that is exactly the #882 bug this replaces.
imag_obs_reachability_message() {
  local out="$1"
  case "$out" in
    *OBS_PROCESS_ABSENT*)
      printf 'imag-nb OBS process is NOT RUNNING (pgrep -x obs found nothing) -- restart it: ssh into imag-nb and run /usr/local/bin/imag-obs-start.sh (or, once supervised, "systemctl --user start imag-obs.service")'
      ;;
    *OBS_PORT_NOT_LISTENING*)
      printf 'imag-nb OBS process is running but WebSocket port 4455 is NOT listening -- OBS may still be starting, or obs-websocket failed to bind (check ~/.config/obs-studio/logs on imag-nb)'
      ;;
    *)
      printf ''
      ;;
  esac
}
