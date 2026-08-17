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
      printf 'imag-nb OBS process is NOT RUNNING (pgrep -x obs found nothing) -- restart it via the supervised unit: ssh into imag-nb and run `export XDG_RUNTIME_DIR=/run/user/$(id -u); systemctl --user start imag-obs` -- NEVER call /usr/local/bin/imag-obs-start.sh directly, that bypasses Restart=on-failure supervision entirely (issue 1015)'
      ;;
    *OBS_PORT_NOT_LISTENING*)
      printf 'imag-nb OBS process is running but WebSocket port 4455 is NOT listening -- OBS may still be starting, or obs-websocket failed to bind (check ~/.config/obs-studio/logs on imag-nb)'
      ;;
    *)
      printf ''
      ;;
  esac
}

# ── #788: distinguish a DELIBERATE operator quit from a real crash (dev1-side alert path only) ──
#
# The RELAUNCH half of #788 is already solved by imag-obs.service (issue 882): Restart=on-failure
# leaves a clean exit(0) alone; deliberate stops route through `systemctl --user stop`. The RESIDUAL
# these two functions close is the dev1-side ALERT path (scripts/imag-obs-alert-watchdog.sh), which
# used to fire "OBS is DOWN" on ANY OBS_PROCESS_ABSENT with no operator-quit discrimination -- so an
# operator quitting OBS on purpose (to test latency) still paged the crew (the live 2026-07-16
# incident: 4 false 'crashed' alarms). The AUTHORITATIVE discriminator is systemd's own
# Restart=on-failure verdict on imag-obs.service: a clean quit / `systemctl stop` reads
# `LoadState=loaded ActiveState=inactive Result=success`; a crash-loop ends `failed`. Plus a
# time-bounded operator override file. These are used ONLY by the alert path -- NEVER by the [0/8]
# preflight (which legitimately fails when OBS is absent for any reason, deliberate or not).

# imag_obs_deliberate_down_probe_cmd [pause_file] [pause_window_s] -> prints a REMOTE bash snippet
# (embedded via $(...) into a second ssh, same always-exit-0 command-builder shape as
# imag_obs_reachability_probe_cmd). Run ONLY when the OBS process is ABSENT. Emits, one token per
# line: OPERATOR_PAUSE=0|1 (the pause file exists AND its mtime is within pause_window_s), then the
# user unit's `LoadState=`/`ActiveState=`/`Result=` (or UNIT_QUERY=FAILED if the user bus is
# unreachable). `pause_file`/`pause_window_s` are substituted HERE at build time (unquoted heredoc);
# every remote `$` is escaped so it survives to the box. A non-login ssh session needs
# XDG_RUNTIME_DIR to reach the user bus (issue 998).
imag_obs_deliberate_down_probe_cmd() {
  local pause_file="${1:-/tmp/imag-watchdog-pause}"
  local pause_window_s="${2:-3600}"
  cat <<EOF
export XDG_RUNTIME_DIR="/run/user/\$(id -u)" >/dev/null 2>&1 || true
if [ -f "$pause_file" ]; then
  __mt=\$(stat -c %Y "$pause_file" 2>/dev/null || echo 0)
  __now=\$(date +%s 2>/dev/null || echo 0)
  if [ "\$__now" -ge "\$__mt" ] && [ \$(( __now - __mt )) -le $pause_window_s ]; then
    echo "OPERATOR_PAUSE=1"
  else
    echo "OPERATOR_PAUSE=0"
  fi
else
  echo "OPERATOR_PAUSE=0"
fi
__st=\$(systemctl --user show imag-obs.service --property=LoadState,ActiveState,Result 2>/dev/null)
if [ -n "\$__st" ]; then
  printf '%s\n' "\$__st"
else
  echo "UNIT_QUERY=FAILED"
fi
EOF
}

# imag_obs_down_is_deliberate PROBE2_OUTPUT -> pure token classifier. Prints:
#   deliberate=0|1
#   reason=<short>
# deliberate=1 (suppress the alert) iff:
#   (a) OPERATOR_PAUSE=1 (a fresh operator override file), OR
#   (b) LoadState=loaded AND ActiveState=inactive AND Result=success -- a clean exit(0) / operator
#       `systemctl --user stop`, i.e. systemd's Restart=on-failure deliberately left it down.
# Everything else -> deliberate=0 (fall through to the existing alarm): `failed`/`activating`/
# exit-code/signal (a crash), LoadState=not-found (the live-confirmed systemd quirk: a not-found
# unit ALSO reports inactive/success -- requiring LoadState=loaded is what stops it being misread as
# a clean quit), UNIT_QUERY=FAILED, or empty. Fail-safe: "bez clean markera = pád -> alarm".
# Pure: no I/O, no external command (a while-read over a herestring, so it is safe under the
# caller's `set -euo pipefail` -- no pipe SIGPIPE, per ci-testing-gotchas.md).
imag_obs_down_is_deliberate() {
  local out="${1:-}"
  case "$out" in
    *OPERATOR_PAUSE=1*)
      printf 'deliberate=1\nreason=operator-pause-file\n'
      return 0
      ;;
  esac
  local load="" active="" result="" __line
  while IFS= read -r __line; do
    case "$__line" in
      LoadState=*)   load="${__line#LoadState=}" ;;
      ActiveState=*) active="${__line#ActiveState=}" ;;
      Result=*)      result="${__line#Result=}" ;;
    esac
  done <<<"$out"
  if [ "$load" = "loaded" ] && [ "$active" = "inactive" ] && [ "$result" = "success" ]; then
    printf 'deliberate=1\nreason=clean-exit imag-obs.service inactive/success (operator quit or systemctl stop)\n'
    return 0
  fi
  printf 'deliberate=0\nreason=not-a-deliberate-quit (LoadState=%s ActiveState=%s Result=%s)\n' \
    "${load:-?}" "${active:-?}" "${result:-?}"
  return 0
}
