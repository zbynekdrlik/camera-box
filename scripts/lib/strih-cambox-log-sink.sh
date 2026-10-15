#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the cambox log sink constants, renderers, the setup-strih
# step 16k install and the verify-strih item 46 grader; no top-level statements besides constants and
# lazy sources) -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here:
# sourcing runs in the CALLER's shell, so strict mode would leak into it. setup-strih.sh /
# verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/strih-cambox-log-sink.sh -- issue 1394 (owner ROZHODNUTE 9.10.2026): the cambox
# off-box log SINK lives on the venue's strih box ("strih nb, which has the performance and the disk"),
# never on dev1 (no rig service and no rig data on dev1). Every cambox sends to the strih whose
# STRIH_CAMERAS lists it (scripts/lib/remote-logging.sh remote_log_sink_ip); this lib is the receiving
# half, on every box of the strih ROLE (the stream role is never a sink: `obs_box_step_applies 16k`).
#
#   * systemd-journal-remote (package systemd-journal-remote) behind its stock socket :19532, switched
#     to plain HTTP by a drop-in (the cambox uploaders use http://) and capped HARD by a
#     journal-remote.conf.d file (MaxUse / KeepFree / MaxFileSize / MaxFiles), one journal per sending
#     host (SplitMode=host) under /var/log/journal/remote/.
#   * cambox-netconsole-sink.service: a DEDICATED rsyslog instance (rsyslogd -n -iNONE -f <own conf>,
#     User=syslog + CAP_NET_BIND_SERVICE), imudp on :514, writing one file per camera of this venue
#     (/var/log/cambox/camN-kernel.log, the sender matched by the camera's camera_resolve address)
#     and everything else to /var/log/cambox/other-senders.log -- a fixed set of files, nothing
#     dropped. The box's own rsyslog.service is never touched. Its config lives in a SUBDIRECTORY of
#     /etc/rsyslog.d: Ubuntu's ENFORCED AppArmor profile for /usr/sbin/rsyslogd lets it read only
#     /etc/rsyslog.conf + /etc/rsyslog.d/** and write /var/log/** + /var/spool/rsyslog/** (checked on
#     dev1: a config elsewhere is "Permission denied"), while the box's own instance loads only
#     /etc/rsyslog.d/*.conf, so a subdirectory is read by this instance alone. No workDirectory and
#     no disk queue: nothing else is written.
#   * cambox-netconsole-rotate.timer (every 10 min) runs logrotate with this lib's own config and
#     state: each file rotated past STRIH_LOG_SINK_FILE_SIZE, STRIH_LOG_SINK_FILE_KEEP compressed copies
#     kept, so the netconsole files are bounded at (camera count + 1) x (keep + 1) x size plus 10 min
#     of growth.
#   * every receiver runs at IDLE priority (Nice=19, CPUSchedulingPolicy=idle, IOSchedulingClass=idle)
#     and, on a hybrid CPU, on the E-cores (CPUAffinity= read the strih_lx_lowprio_prefix way through
#     strih_program_audio_ecores; strih-snv is AMD, so no affinity line there), so the production OBS
#     is untouched.
#
#   * strih_cambox_log_sink_install -- setup-strih step 16k (as root), ENABLE-ONLY: the packages only
#     when missing, every file written only when its bytes or mode differ (strih_session_apps_put), a
#     daemon-reload after a unit change, `enable` of the socket, the sink and the rotate timer, and a
#     `try-restart` of a RUNNING unit whose files changed. A stopped unit is never started here.
#   * strih_cambox_log_sink_grade_rows / _report -- verify-strih item 46, 3 rows:
#       (log-sink-files)  the packages installed and every file byte-identical to its renderer;
#       (log-sink-units)  the socket, the sink and the rotate timer enabled + active;
#       (log-sink-listen) UDP :514 and TCP :19532 listening.
#
# Test seams (tests/python/test_strih_cambox_log_sink_1394.py): STRIH_LOG_SINK_UNIT_DIR
# (/etc/systemd/system), STRIH_LOG_SINK_RSYSLOG_DIR (/etc/rsyslog.d/cambox-netconsole-sink),
# STRIH_LOG_SINK_CONF_DIR (/etc/camera-box/cambox-log-sink, the logrotate config),
# STRIH_LOG_SINK_JR_CONF_DIR (/etc/systemd/journal-remote.conf.d), STRIH_LOG_SINK_SYSFS (/sys),
# STRIH_LOG_SINK_SYSTEMCTL (systemctl), STRIH_LOG_SINK_SS (ss); `dpkg-query` and `apt-get` are looked
# up on PATH.

if ! declare -F strih_lx_cameras >/dev/null; then
  # shellcheck source=scripts/lib/strih-box-facts.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-box-facts.sh"
fi
if ! declare -F strih_session_apps_put >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi
if ! declare -F strih_program_audio_ecores >/dev/null; then
  # shellcheck source=scripts/lib/strih-program-audio.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-program-audio.sh"
fi
# The ports: the ONE declaration both sides read (the cambox generators send to them).
if ! declare -F remote_log_sink_ip >/dev/null; then
  # shellcheck source=scripts/lib/remote-logging.sh
  . "$(dirname "${BASH_SOURCE[0]}")/remote-logging.sh"
fi
if ! declare -F camera_resolve >/dev/null; then
  # shellcheck source=scripts/camera-set.sh
  . "$(dirname "${BASH_SOURCE[0]}")/../camera-set.sh"
fi

STRIH_LOG_SINK_PACKAGES=(systemd-journal-remote rsyslog logrotate)
STRIH_LOG_SINK_UNIT="cambox-netconsole-sink.service"
STRIH_LOG_SINK_ROTATE_SERVICE="cambox-netconsole-rotate.service"
STRIH_LOG_SINK_ROTATE_TIMER="cambox-netconsole-rotate.timer"
STRIH_LOG_SINK_JR_SOCKET="systemd-journal-remote.socket"
STRIH_LOG_SINK_JR_SERVICE="systemd-journal-remote.service"
STRIH_LOG_SINK_JR_DROPIN="systemd-journal-remote.service.d/10-cambox-sink.conf"
STRIH_LOG_SINK_JR_CONF_FILE="10-cambox-sink.conf"
# Where the files land on the sink (the rsyslog instance's LogsDirectory, the stock journal-remote dir).
STRIH_LOG_SINK_KERNEL_DIR="/var/log/cambox"
STRIH_LOG_SINK_JOURNAL_DIR="/var/log/journal/remote"
# The HARD caps. journal-remote: at most MaxUse on disk and never below KeepFree free (the smaller
# wins), 128 MiB per file. netconsole: each file rotated past the size, KEEP compressed copies.
STRIH_LOG_SINK_JR_MAX_USE="4G"
STRIH_LOG_SINK_JR_KEEP_FREE="20G"
STRIH_LOG_SINK_JR_MAX_FILE="128M"
STRIH_LOG_SINK_JR_MAX_FILES="200"
STRIH_LOG_SINK_FILE_SIZE="16M"
STRIH_LOG_SINK_FILE_KEEP="4"

strih_log_sink_unit_dir() { printf '%s' "${STRIH_LOG_SINK_UNIT_DIR:-/etc/systemd/system}"; }
strih_log_sink_conf_dir() { printf '%s' "${STRIH_LOG_SINK_CONF_DIR:-/etc/camera-box/cambox-log-sink}"; }
strih_log_sink_rsyslog_dir() { printf '%s' "${STRIH_LOG_SINK_RSYSLOG_DIR:-/etc/rsyslog.d/cambox-netconsole-sink}"; }
strih_log_sink_jr_conf_dir() { printf '%s' "${STRIH_LOG_SINK_JR_CONF_DIR:-/etc/systemd/journal-remote.conf.d}"; }
strih_log_sink_systemctl() { "${STRIH_LOG_SINK_SYSTEMCTL:-systemctl}" "$@"; }

# strih_log_sink_ecores -> this box's E-core list (empty on a non-hybrid box: no affinity line).
strih_log_sink_ecores() { strih_program_audio_ecores "${STRIH_LOG_SINK_SYSFS:-/sys}"; }

# _strih_log_sink_idle_lines ECORES -> the idle-priority [Service] lines every receiver carries.
_strih_log_sink_idle_lines() {
  printf '%s\n' "Nice=19" "CPUSchedulingPolicy=idle" "IOSchedulingClass=idle"
  [ -z "${1-}" ] || printf 'CPUAffinity=%s\n' "$1"
}

# strih_cambox_log_sink_senders -> `camN<TAB>address` per camera this box lists (STRIH_CAMERAS), the
# address from camera_resolve (the cambox's own address, netconsole's source). rc 1 + a stderr line
# when a camera does not resolve; nothing for a box that lists none.
strih_cambox_log_sink_senders() {
  local n cams out=""
  cams="$(strih_lx_cameras)" || { echo "strih-cambox-log-sink: the box's STRIH_CAMERAS could not be read" >&2; return 1; }
  for n in $cams; do
    out+="$( (camera_resolve "cam${n}" >/dev/null && printf 'cam%s\t%s' "$n" "$CAMERA_IP") )" \
      || { echo "strih-cambox-log-sink: cam${n} (STRIH_CAMERAS) does not resolve in scripts/camera-set.sh" >&2; return 1; }
    out+=$'\n'
  done
  printf '%s' "$out"
}

# strih_cambox_log_sink_rsyslog_conf -> the dedicated rsyslog instance's config: imudp on the
# netconsole port into a ruleset that writes one file per camera of this box and the rest to
# other-senders.log. rc 1 when the senders do not resolve.
strih_cambox_log_sink_rsyslog_conf() {
  local senders cam ip
  senders="$(strih_cambox_log_sink_senders)" || return 1
  cat <<EOF
# GENERATED by scripts/lib/strih-cambox-log-sink.sh (camera-box issue 1394, setup-strih step 16k).
# The cambox netconsole sink: the kernel printk of this venue's camboxes over UDP :${REMOTE_LOG_NETCONSOLE_PORT}, one
# file per camera (the sender matched by its address), anything else in other-senders.log. A DEDICATED
# rsyslog instance (cambox-netconsole-sink.service), so the box's own rsyslog is never touched.
module(load="imudp")
input(type="imudp" port="${REMOTE_LOG_NETCONSOLE_PORT}" ruleset="cambox_netconsole")

template(name="camboxKernelLine" type="string" string="%timegenerated:::date-rfc3339% %fromhost-ip% %msg%\\n")

ruleset(name="cambox_netconsole") {
EOF
  while IFS=$'\t' read -r cam ip; do
    [ -n "$cam" ] || continue
    cat <<EOF
    if \$fromhost-ip == "${ip}" then {
        action(type="omfile" file="${STRIH_LOG_SINK_KERNEL_DIR}/${cam}-kernel.log" template="camboxKernelLine")
        stop
    }
EOF
  done <<<"$senders"
  cat <<EOF
    action(type="omfile" file="${STRIH_LOG_SINK_KERNEL_DIR}/other-senders.log" template="camboxKernelLine")
}
EOF
}

# strih_cambox_log_sink_logrotate_conf -> the rotation of the netconsole files (run by the rotate
# timer with its own state, never the box's daily logrotate).
strih_cambox_log_sink_logrotate_conf() {
  cat <<EOF
# GENERATED by scripts/lib/strih-cambox-log-sink.sh (camera-box issue 1394): the cambox netconsole
# files, rotated past ${STRIH_LOG_SINK_FILE_SIZE} by ${STRIH_LOG_SINK_ROTATE_TIMER} (every 10 min), ${STRIH_LOG_SINK_FILE_KEEP} compressed copies kept.
${STRIH_LOG_SINK_KERNEL_DIR}/*.log {
    su syslog syslog
    size ${STRIH_LOG_SINK_FILE_SIZE}
    rotate ${STRIH_LOG_SINK_FILE_KEEP}
    compress
    missingok
    notifempty
    copytruncate
}
EOF
}

# strih_cambox_log_sink_unit_text ECORES -> cambox-netconsole-sink.service.
strih_cambox_log_sink_unit_text() {
  cat <<EOF
[Unit]
Description=cambox netconsole sink: the venue camboxes' kernel printk over UDP :${REMOTE_LOG_NETCONSOLE_PORT} (camera-box issue 1394)
Documentation=https://github.com/zbynekdrlik/camera-box/issues/1394
After=network.target

[Service]
Type=simple
User=syslog
Group=syslog
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
ExecStart=/usr/sbin/rsyslogd -n -iNONE -f $(strih_log_sink_rsyslog_dir)/rsyslog.conf
LogsDirectory=cambox
LogsDirectoryMode=0755
Restart=always
RestartSec=5
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
EOF
  _strih_log_sink_idle_lines "${1-}"
  cat <<'EOF'

[Install]
WantedBy=multi-user.target
EOF
}

# strih_cambox_log_sink_rotate_service_text ECORES / _timer_text -> the rotation oneshot + its timer.
strih_cambox_log_sink_rotate_service_text() {
  cat <<EOF
[Unit]
Description=rotate the cambox netconsole sink files (camera-box issue 1394)
Documentation=https://github.com/zbynekdrlik/camera-box/issues/1394

[Service]
Type=oneshot
ExecStart=/usr/sbin/logrotate --state /var/lib/cambox-netconsole-rotate/logrotate.status $(strih_log_sink_conf_dir)/logrotate.conf
StateDirectory=cambox-netconsole-rotate
EOF
  _strih_log_sink_idle_lines "${1-}"
}

strih_cambox_log_sink_rotate_timer_text() {
  cat <<'EOF'
[Unit]
Description=rotate the cambox netconsole sink files every 10 min (camera-box issue 1394)

[Timer]
OnBootSec=5min
OnUnitActiveSec=10min
AccuracySec=1min

[Install]
WantedBy=timers.target
EOF
}

# strih_cambox_log_sink_jr_dropin_text ECORES -> the systemd-journal-remote.service drop-in: plain HTTP
# on the socket's fd (the stock unit listens HTTPS) into the stock output dir, at idle priority.
strih_cambox_log_sink_jr_dropin_text() {
  cat <<EOF
# GENERATED by scripts/lib/strih-cambox-log-sink.sh (camera-box issue 1394): the cambox uploaders send
# plain HTTP (the venue LAN is private), so the receiver listens HTTP on the socket's fd :${REMOTE_LOG_JOURNAL_PORT}.
[Service]
ExecStart=
ExecStart=/usr/lib/systemd/systemd-journal-remote --listen-http=-3 --output=${STRIH_LOG_SINK_JOURNAL_DIR}/
EOF
  _strih_log_sink_idle_lines "${1-}"
}

# strih_cambox_log_sink_jr_conf_text -> journal-remote.conf.d: the HARD cap and one journal per host.
strih_cambox_log_sink_jr_conf_text() {
  cat <<EOF
# GENERATED by scripts/lib/strih-cambox-log-sink.sh (camera-box issue 1394): the uploaded cambox
# journals never take more than MaxUse and never leave less than KeepFree free (the smaller wins).
[Remote]
Seal=false
SplitMode=host
MaxUse=${STRIH_LOG_SINK_JR_MAX_USE}
KeepFree=${STRIH_LOG_SINK_JR_KEEP_FREE}
MaxFileSize=${STRIH_LOG_SINK_JR_MAX_FILE}
MaxFiles=${STRIH_LOG_SINK_JR_MAX_FILES}
EOF
}

# strih_cambox_log_sink_files ECORES -> `PATH<TAB>RENDERER` per installed file (the install and the
# grader walk the same list). RENDERER is a function name + its argument, evaluated by _strih_log_sink_render.
strih_cambox_log_sink_files() {
  local u c j r
  u="$(strih_log_sink_unit_dir)"
  c="$(strih_log_sink_conf_dir)"
  j="$(strih_log_sink_jr_conf_dir)"
  r="$(strih_log_sink_rsyslog_dir)"
  printf '%s\t%s\n' \
    "${r}/rsyslog.conf" strih_cambox_log_sink_rsyslog_conf \
    "${c}/logrotate.conf" strih_cambox_log_sink_logrotate_conf \
    "${u}/${STRIH_LOG_SINK_UNIT}" strih_cambox_log_sink_unit_text \
    "${u}/${STRIH_LOG_SINK_ROTATE_SERVICE}" strih_cambox_log_sink_rotate_service_text \
    "${u}/${STRIH_LOG_SINK_ROTATE_TIMER}" strih_cambox_log_sink_rotate_timer_text \
    "${u}/${STRIH_LOG_SINK_JR_DROPIN}" strih_cambox_log_sink_jr_dropin_text \
    "${j}/${STRIH_LOG_SINK_JR_CONF_FILE}" strih_cambox_log_sink_jr_conf_text
}

# _strih_log_sink_render FUNCTION ECORES TMP -> FUNCTION's text + one newline into TMP. rc 1 on failure.
_strih_log_sink_render() {
  local text
  text="$("$1" "${2-}")" || return 1
  printf '%s\n' "$text" > "$3"
}

strih_cambox_log_sink_missing_packages() {
  local p
  for p in "${STRIH_LOG_SINK_PACKAGES[@]}"; do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] || printf '%s\n' "$p"
  done
  return 0
}

# --- setup-strih step 16k ---------------------------------------------------------------------------------

# strih_cambox_log_sink_install -> see the header. One line per action. rc 1 + a stderr line on a
# package that will not install, a renderer that fails, a failed write or a failed systemctl call.
strih_cambox_log_sink_install() {
  local ecores missing path fn tmp w changed_sink=0 changed_jr=0 changed_timer=0 changed_units=0
  local -a restart=()
  ecores="$(strih_log_sink_ecores)"
  missing="$(strih_cambox_log_sink_missing_packages)"
  if [ -n "$missing" ]; then
    echo "  installing ${missing//$'\n'/ }"
    # shellcheck disable=SC2086  # one word per package name
    DEBIAN_FRONTEND=noninteractive apt-get install -y $missing || {
      echo "strih-cambox-log-sink: apt-get install ${missing//$'\n'/ } failed" >&2; return 1; }
  fi
  tmp="$(mktemp)" || { echo "strih-cambox-log-sink: mktemp failed" >&2; return 1; }
  while IFS=$'\t' read -r path fn; do
    [ -n "$path" ] || continue
    install -d -m 0755 "$(dirname "$path")" \
      || { rm -f "$tmp"; echo "strih-cambox-log-sink: cannot create $(dirname "$path")" >&2; return 1; }
    _strih_log_sink_render "$fn" "$ecores" "$tmp" \
      || { rm -f "$tmp"; echo "strih-cambox-log-sink: cannot render ${path} (${fn})" >&2; return 1; }
    w="$(strih_session_apps_put "$tmp" "$path" 0644)" || { rm -f "$tmp"; return 1; }
    echo "  ${path} ${w}"
    [ "$w" = written ] || continue
    case "$path" in
      */rsyslog.conf | */"${STRIH_LOG_SINK_UNIT}") changed_sink=1 ;;
      */"${STRIH_LOG_SINK_ROTATE_TIMER}" | */"${STRIH_LOG_SINK_ROTATE_SERVICE}") changed_timer=1 ;;
      */"${STRIH_LOG_SINK_JR_DROPIN##*/}" | */"${STRIH_LOG_SINK_JR_CONF_FILE}") changed_jr=1 ;;
    esac
    case "$path" in "$(strih_log_sink_unit_dir)"/*) changed_units=1 ;; esac
  done < <(strih_cambox_log_sink_files)
  rm -f "$tmp"
  echo "  idle priority (Nice=19, CPUSchedulingPolicy=idle, IOSchedulingClass=idle); CPUAffinity=${ecores:-none: no cpu_atom, every cpu}"
  if [ "$changed_units" = 1 ]; then
    strih_log_sink_systemctl daemon-reload || { echo "strih-cambox-log-sink: systemctl daemon-reload failed" >&2; return 1; }
  fi
  strih_log_sink_systemctl enable "$STRIH_LOG_SINK_JR_SOCKET" "$STRIH_LOG_SINK_UNIT" "$STRIH_LOG_SINK_ROTATE_TIMER" || {
    echo "strih-cambox-log-sink: systemctl enable ${STRIH_LOG_SINK_JR_SOCKET} ${STRIH_LOG_SINK_UNIT} ${STRIH_LOG_SINK_ROTATE_TIMER} failed" >&2
    return 1; }
  echo "  enabled ${STRIH_LOG_SINK_JR_SOCKET} ${STRIH_LOG_SINK_UNIT} ${STRIH_LOG_SINK_ROTATE_TIMER} (enable-only: never started here)"
  [ "$changed_sink" = 1 ] && restart+=("$STRIH_LOG_SINK_UNIT")
  [ "$changed_jr" = 1 ] && restart+=("$STRIH_LOG_SINK_JR_SERVICE")
  [ "$changed_timer" = 1 ] && restart+=("$STRIH_LOG_SINK_ROTATE_TIMER")
  if [ "${#restart[@]}" -gt 0 ]; then
    # only a RUNNING unit moves onto its new files; a stopped one stays stopped
    strih_log_sink_systemctl try-restart "${restart[@]}" || {
      echo "strih-cambox-log-sink: systemctl try-restart ${restart[*]} failed" >&2; return 1; }
    echo "  try-restart ${restart[*]}"
  fi
  return 0
}

# --- verify-strih item 46 ---------------------------------------------------------------------------------

# _strih_log_sink_listening PROTO PORT -> 0 iff ss lists a listener on PORT (PROTO: u = UDP, t = TCP).
_strih_log_sink_listening() {
  local out
  out="$("${STRIH_LOG_SINK_SS:-ss}" -H -ln"$1" "sport = :$2" 2>/dev/null || true)"
  [ -n "$out" ]
}

# strih_cambox_log_sink_grade_rows -> verify-strih rows `OK|text` or `FAIL|text`
# (strih_cambox_log_sink_row_count of them). Always rc 0.
strih_cambox_log_sink_grade_rows() {
  local ecores missing path fn tmp st bad="" u en act states="" units_ok=1 listen=""
  ecores="$(strih_log_sink_ecores)"
  missing="$(strih_cambox_log_sink_missing_packages)"
  missing="${missing//$'\n'/ }"   # one row: a newline inside it would split it into more rows
  tmp="$(mktemp)"
  while IFS=$'\t' read -r path fn; do
    [ -n "$path" ] || continue
    if _strih_log_sink_render "$fn" "$ecores" "$tmp" 2>/dev/null; then
      st="$(strih_session_app_file_state "$path" "$tmp")"
    else
      st=render-failed
    fi
    [ "$st" = ok ] || bad="${bad} ${path##*/}=${st}"
  done < <(strih_cambox_log_sink_files)
  rm -f "$tmp"
  if [ -z "$missing" ] && [ -z "$bad" ]; then
    printf 'OK|(log-sink-files) %s installed, every receiver file matches its renderer (journal-remote capped MaxUse=%s KeepFree=%s; netconsole files %s x %s)\n' \
      "${STRIH_LOG_SINK_PACKAGES[*]}" "$STRIH_LOG_SINK_JR_MAX_USE" "$STRIH_LOG_SINK_JR_KEEP_FREE" "$STRIH_LOG_SINK_FILE_SIZE" "$STRIH_LOG_SINK_FILE_KEEP"
  else
    printf 'FAIL|(log-sink-files) missing packages: %s; files:%s -- re-run setup-strih.sh step 16k (issue 1394)\n' \
      "${missing:-none}" "${bad:- ok}"
  fi

  for u in "$STRIH_LOG_SINK_JR_SOCKET" "$STRIH_LOG_SINK_UNIT" "$STRIH_LOG_SINK_ROTATE_TIMER"; do
    en="$(strih_log_sink_systemctl is-enabled "$u" 2>/dev/null | head -n 1 || true)"
    act="$(strih_log_sink_systemctl is-active "$u" 2>/dev/null | head -n 1 || true)"
    states="${states} ${u}=${en:-unreadable}/${act:-unreadable}"
    { [ "$en" = enabled ] && [ "$act" = active ]; } || units_ok=0
  done
  if [ "$units_ok" = 1 ]; then
    printf 'OK|(log-sink-units)%s\n' "$states"
  else
    printf 'FAIL|(log-sink-units)%s -- setup-strih step 16k only enables them: systemctl start %s %s %s (issue 1394)\n' \
      "$states" "$STRIH_LOG_SINK_JR_SOCKET" "$STRIH_LOG_SINK_UNIT" "$STRIH_LOG_SINK_ROTATE_TIMER"
  fi

  _strih_log_sink_listening u "$REMOTE_LOG_NETCONSOLE_PORT" || listen="${listen} UDP:${REMOTE_LOG_NETCONSOLE_PORT}"
  _strih_log_sink_listening t "$REMOTE_LOG_JOURNAL_PORT" || listen="${listen} TCP:${REMOTE_LOG_JOURNAL_PORT}"
  if [ -z "$listen" ]; then
    printf 'OK|(log-sink-listen) UDP :%s (netconsole) and TCP :%s (journal-upload) are listening\n' \
      "$REMOTE_LOG_NETCONSOLE_PORT" "$REMOTE_LOG_JOURNAL_PORT"
  else
    printf 'FAIL|(log-sink-listen) nothing listens on%s -- the camboxes of this venue log into the void (journalctl -u %s -u %s)\n' \
      "$listen" "$STRIH_LOG_SINK_UNIT" "$STRIH_LOG_SINK_JR_SERVICE"
  fi
  return 0
}

strih_cambox_log_sink_row_count() { printf '3'; }

# strih_cambox_log_sink_grade_report -> print the rows through the CALLER's ok / note / bad functions
# (verify-strih). rc 1 unless the grader printed every row.
strih_cambox_log_sink_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_cambox_log_sink_grade_rows)
  [ "$rows" = "$(strih_cambox_log_sink_row_count)" ]
}
