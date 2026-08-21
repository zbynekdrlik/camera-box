#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# scripts/lib/event-assert.sh's convention -- `set -euo pipefail` must NEVER be set here since
# sourcing this file mutates the CALLING script's own shell options.
#
# scripts/lib/preflight-fleet-check.sh — #758 item 1: the per-box minute-0 preflight check. Two
# things event_assert_fleet_check_cmds (#722, scripts/lib/event-assert.sh) does NOT already cover:
#
#   1. `PAINT_COUNT` there counts --paint-only PROCESSES (the frame-probe painter) -- this repo's
#      #758 preflight instead needs the CAMERA-BOX EMITTER process count (a different process
#      entirely). `pgrep -cx camera-box` matches the process's exact `comm` name (-x), which
#      NEVER self-matches the enclosing `bash -c "$SCRIPT"` shell that runs this over ssh (unlike
#      `pgrep -f`, whose #722-documented self-match footgun needed a base64-encoded pattern to
#      dodge -- `-x`'s exact-name match has no such risk, so no encoding trick is needed here).
#   2. Reports `SERVICE_ACTIVE`/`EMITTER_COUNT`/`STRAY_UNITS` as ONE preflight-shaped line, so the
#      harness's per-box loop can parse a SINGLE ssh round trip per box.
#
# #762 item 2 extends the SAME one-round-trip line with the log-storm EARLY-WARNING signal: a
# real live incident (cam1, 2026-07-15) showed rsyslogd enter a write-error feedback loop at
# ~400 lines/s (42.8% CPU) once the 50MB /var/log tmpfs filled, starving the camera-box send
# path badly enough to measurably drift delivery timing and NDI under-deliver to imag -- all with
# NO loud signal until forensics were run after the fact. `DISK_LOG_PCT`/`DISK_TMP_PCT` catch the
# fill itself; `RSYSLOGD_CPU`/`JOURNALD_CPU` catch the storm SIGNATURE even before the tmpfs is
# fully wedged (a box mid-storm but not yet 100% full still burns CPU on the write-error loop).
# The permanent fix (purging rsyslog + capping journald, scripts/lib/log-diet.sh) makes
# RSYSLOGD_CPU structurally ~0 going forward; these two fields stay as a backstop against a
# regression (rsyslog reinstalled/re-enabled) and against ANY other source of the same disk-fill
# failure class (not just rsyslog).
#
# Sourced by scripts/recording-e2e.sh's [0/8] preflight (ALL_CAMBOX=1 fleet sweep).

# Alert thresholds (%). A trivial, self-contained comparison -- deliberately NOT sourced from
# scripts/lib/disk-guard-thresholds.sh (a different, ships-disabled watchdog with a different
# mount set) to avoid a new lib-sources-lib coupling for a one-line `-ge` check.
PREFLIGHT_DISK_PCT_FAIL="${PREFLIGHT_DISK_PCT_FAIL:-80}"
PREFLIGHT_CPU_PCT_FAIL="${PREFLIGHT_CPU_PCT_FAIL:-20}"

# preflight_fleet_check_cmds -> the REMOTE bash run on EACH cam box (over ssh) that reports, in
# one round trip (AFTER the caller has already run tmp_burn_sweep_stale_units_cmds +
# tmp_burn_sweep_stale_cmds to self-heal routine leftover junk from a prior run):
#   SERVICE_ACTIVE=<state>   — `systemctl is-active camera-box`
#   EMITTER_COUNT=<n>        — exact-name pgrep count for the camera-box process itself
#   STRAY_UNITS=<comma-list> — any camera-box-burn-* systemd unit STILL present (empty when clean
#                               — i.e. the self-heal sweep actually worked)
#   DISK_LOG_PCT=<n>         — `/var/log` tmpfs used% (#762)
#   DISK_TMP_PCT=<n>         — `/tmp` tmpfs used% (#762)
#   RSYSLOGD_CPU=<n>         — summed %cpu of any `rsyslogd` process (#762; 0 once purged)
#   JOURNALD_CPU=<n>         — summed %cpu of any `systemd-journald` process (#762)
#   VIDEO_NODES=<comma-list> — this box's `/dev/video*` capture nodes, empty when NONE are
#                               present (#828). The physical, binary-independent signal for
#                               "no capture card present" — the issue's own repro (`ls /dev/video*`
#                               -> "No such file or directory"). Kept LAST so an empty value is an
#                               unambiguous trailing `VIDEO_NODES=`.
preflight_fleet_check_cmds() {
  cat <<'REMOTE'
SERVICE_ACTIVE=$(systemctl is-active camera-box 2>/dev/null); SERVICE_ACTIVE="${SERVICE_ACTIVE:-unknown}"
EMITTER_COUNT=$(pgrep -cx camera-box 2>/dev/null); EMITTER_COUNT="${EMITTER_COUNT:-0}"
STRAY_UNITS=$(systemctl list-units --all --plain --no-legend 'camera-box-burn-*' 2>/dev/null | awk '{print $1}' | paste -sd, -)
DISK_LOG_PCT=$(df --output=pcent /var/log 2>/dev/null | tail -1 | tr -d '% '); DISK_LOG_PCT="${DISK_LOG_PCT:-0}"
DISK_TMP_PCT=$(df --output=pcent /tmp 2>/dev/null | tail -1 | tr -d '% '); DISK_TMP_PCT="${DISK_TMP_PCT:-0}"
_pscpu=$(ps -eo comm,pcpu --no-headers 2>/dev/null)
RSYSLOGD_CPU=$(printf '%s\n' "$_pscpu" | awk '$1 ~ /^rsyslogd/ {s+=$2} END{printf "%d", s+0}')
JOURNALD_CPU=$(printf '%s\n' "$_pscpu" | awk '$1 ~ /^systemd-journal/ {s+=$2} END{printf "%d", s+0}')
VIDEO_NODES=$(ls -d /dev/video* 2>/dev/null | paste -sd, -)
echo "SERVICE_ACTIVE=$SERVICE_ACTIVE EMITTER_COUNT=$EMITTER_COUNT STRAY_UNITS=$STRAY_UNITS DISK_LOG_PCT=$DISK_LOG_PCT DISK_TMP_PCT=$DISK_TMP_PCT RSYSLOGD_CPU=$RSYSLOGD_CPU JOURNALD_CPU=$JOURNALD_CPU VIDEO_NODES=$VIDEO_NODES"
REMOTE
}

# preflight_fleet_check_verdict OUTPUT_LINE -> "" (empty = PASS) or a human-readable reason string
# (FAIL) — the PURE decision over preflight_fleet_check_cmds's parsed output. No I/O; the caller
# does the ssh round trip and passes the resulting line here to decide pass/fail. Fields absent
# from OUTPUT_LINE (e.g. a hand-built test fixture predating #762) default to 0 -- a missing
# disk/CPU reading is never mistaken for a storm, matching this function's existing "absent
# STRAY_UNITS = clean" convention.
preflight_fleet_check_verdict() {
  local line="$1" service_active emitter_count stray_units disk_log_pct disk_tmp_pct rsyslogd_cpu journald_cpu video_nodes
  # #828 — the physical, operator-actionable cause "no capture card present" is the MOST specific
  # reason and is checked FIRST (it wins over the generic "service is <state>, not active" when a
  # device-less box also happens to be inactive/activating). An empty VIDEO_NODES field that IS
  # reported = no /dev/video* capture nodes; a field that is entirely ABSENT (a fixture predating
  # #828) is "not reported" and skipped, so nothing false-fails. Empty-list != absent for a string
  # field, so presence is detected with `grep -q`, never by the value alone. When nodes DO exist
  # but the service is down for some OTHER reason, this stays silent -> honest generic message.
  if grep -q 'VIDEO_NODES=' <<<"$line" ; then
    video_nodes="$(printf '%s' "$line" | grep -oP 'VIDEO_NODES=\K\S*' || true)"
    if [ -z "${video_nodes:-}" ]; then
      echo "no capture card present — fit/check the grabber (#828)"
      return 0
    fi
  fi
  service_active="$(printf '%s' "$line" | grep -oP 'SERVICE_ACTIVE=\K\S+' || true)"
  emitter_count="$(printf '%s' "$line" | grep -oP 'EMITTER_COUNT=\K[0-9]+' || true)"
  stray_units="$(printf '%s' "$line" | grep -oP 'STRAY_UNITS=\K\S*' || true)"
  disk_log_pct="$(printf '%s' "$line" | grep -oP 'DISK_LOG_PCT=\K[0-9]+' || true)"
  disk_tmp_pct="$(printf '%s' "$line" | grep -oP 'DISK_TMP_PCT=\K[0-9]+' || true)"
  rsyslogd_cpu="$(printf '%s' "$line" | grep -oP 'RSYSLOGD_CPU=\K[0-9]+' || true)"
  journald_cpu="$(printf '%s' "$line" | grep -oP 'JOURNALD_CPU=\K[0-9]+' || true)"
  if [ "${service_active:-unknown}" != "active" ]; then
    echo "camera-box.service is ${service_active:-unreachable/unknown}, not active"
    return 0
  fi
  if [ "${emitter_count:-0}" != "1" ]; then
    echo "expected exactly ONE camera-box emitter process, found ${emitter_count:-0}"
    return 0
  fi
  if [ -n "${stray_units:-}" ]; then
    echo "stray camera-box-burn-* unit(s) survived the preflight sweep: ${stray_units}"
    return 0
  fi
  if [ "${disk_log_pct:-0}" -ge "$PREFLIGHT_DISK_PCT_FAIL" ]; then
    echo "/var/log at ${disk_log_pct}% (threshold ${PREFLIGHT_DISK_PCT_FAIL}%) — log storm risk, vycisti/restartuj logging (#762)"
    return 0
  fi
  if [ "${disk_tmp_pct:-0}" -ge "$PREFLIGHT_DISK_PCT_FAIL" ]; then
    echo "/tmp at ${disk_tmp_pct}% (threshold ${PREFLIGHT_DISK_PCT_FAIL}%) — disk pressure risk (#762)"
    return 0
  fi
  if [ "${rsyslogd_cpu:-0}" -ge "$PREFLIGHT_CPU_PCT_FAIL" ]; then
    echo "rsyslogd CPU at ${rsyslogd_cpu}% (threshold ${PREFLIGHT_CPU_PCT_FAIL}%) — log storm signature (#762)"
    return 0
  fi
  if [ "${journald_cpu:-0}" -ge "$PREFLIGHT_CPU_PCT_FAIL" ]; then
    echo "systemd-journald CPU at ${journald_cpu}% (threshold ${PREFLIGHT_CPU_PCT_FAIL}%) — log storm signature (#762)"
    return 0
  fi
  echo "" # empty = PASS
}
