#!/usr/bin/env bash
# dev1-remote-log-install.sh (#1311) -- dev1-side receiver for cambox off-box logging. See the full
# header below `set` (a one-line summary here keeps set -euo pipefail inside the first 15 lines).
set -euo pipefail
#
# WHY (#1311, Finding 1 step 2): the cam boxes now SEND their kernel log (netconsole, UDP :514) and
# their journal (systemd-journal-upload, HTTP :19532) off-box so the NEXT #1309 half-dead-stick death
# is diagnosable -- the on-STICK persistent journal dies with the stick. This is the dev1 RECEIVER
# half: it un-comments rsyslog's imudp (dev1 already runs rsyslog.service) to write per-box kernel
# files under /var/log/cambox/, and stands up systemd-journal-remote --listen-http on :19532 to
# collect the uploaded journals under /var/log/journal/remote/.
#
# This is a PURE PLANNER by default (mirrors scripts/avsync-watchdog-install.sh's shape): it emits
# each config file's content + the exact apply commands, runs NOTHING privileged, and is source-able
# by tests/harness_remote_logging_1311.rs which asserts the emitted config shapes. The SUPERVISOR
# runs the apply (dev1-side service changes are a supervisor step; an autopilot worker never touches
# dev1 services). `--apply` (root, opt-in) writes the files + apt-installs + enables + restarts for
# the supervisor's convenience -- default prints the plan.
#
# Usage:
#   scripts/dev1-remote-log-install.sh              # print the full receiver install plan (default)
#   scripts/dev1-remote-log-install.sh --emit rsyslog    # print ONE config's content (rsyslog|logrotate|journal-remote-dropin|journal-remote-conf)
#   sudo scripts/dev1-remote-log-install.sh --apply      # SUPERVISOR ONLY: actually install on dev1
#
# Exit codes: 0 = plan printed / applied, 2 = usage error.

# --- shared constants (the dev1 receiver counterparts of scripts/lib/remote-logging.sh) -----------
DEV1_NETCONSOLE_PORT="514"                 # UDP, rsyslog imudp (matches REMOTE_LOG_NETCONSOLE_PORT)
DEV1_JOURNAL_PORT="19532"                  # HTTP, systemd-journal-remote (matches REMOTE_LOG_JOURNAL_PORT)
DEV1_CAMBOX_LOG_DIR="/var/log/cambox"      # per-box kernel logs land here
DEV1_JOURNAL_REMOTE_DIR="/var/log/journal/remote"   # uploaded journals land here
DEV1_RSYSLOG_DROPIN="/etc/rsyslog.d/40-cambox-netconsole.conf"
DEV1_LOGROTATE_CONF="/etc/logrotate.d/cambox-remote"
DEV1_JOURNAL_REMOTE_DROPIN="/etc/systemd/system/systemd-journal-remote.service.d/10-cambox-http.conf"
DEV1_JOURNAL_REMOTE_CONF="/etc/systemd/journal-remote.conf"

# --- PURE config generators (no side effects; unit-tested by sourcing this script) ----------------

# dev1_rsyslog_dropin_content -> the rsyslog drop-in that receives netconsole UDP and writes one
# file per source box. A DEDICATED ruleset (never the default rule chain) so cambox kernel spew never
# mixes into dev1's own /var/log; `stop` after writing means these messages are not re-forwarded.
# Keyed by %fromhost-ip% (100% reliable, no reverse-DNS dependency); netconsole's basic format carries
# no syslog hostname, and the source IP uniquely identifies the box (see the box<->IP map in
# targets.md).
dev1_rsyslog_dropin_content() {
  cat <<EOF
# camera-box #1311 -- receive cambox netconsole kernel messages over UDP :${DEV1_NETCONSOLE_PORT} and
# write one file per source box under ${DEV1_CAMBOX_LOG_DIR}. netconsole ships raw kernel printk; the
# source IP identifies the box (box<->IP map: targets.md). A DEDICATED ruleset so this never mixes
# into dev1's own system log, and \`stop\` so it is not re-forwarded.
module(load="imudp")
input(type="imudp" port="${DEV1_NETCONSOLE_PORT}" ruleset="cambox_netconsole")

template(name="camboxKernelFile" type="string" string="${DEV1_CAMBOX_LOG_DIR}/%fromhost-ip%-kernel.log")
template(name="camboxKernelLine" type="string" string="%timegenerated:::date-rfc3339% %fromhost-ip% %msg%\\n")

ruleset(name="cambox_netconsole") {
    action(type="omfile" dynaFile="camboxKernelFile" template="camboxKernelLine")
    stop
}
EOF
}

# dev1_logrotate_content -> rotate the per-box kernel files so a chatty box cannot fill dev1's disk.
# copytruncate: rsyslog holds the dynaFile open, so rotate in place rather than signalling rsyslog.
dev1_logrotate_content() {
  cat <<EOF
${DEV1_CAMBOX_LOG_DIR}/*-kernel.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
}
EOF
}

# dev1_journal_remote_dropin_content -> the systemd-journal-remote.service drop-in that switches the
# receiver to PLAIN HTTP (the stock unit is --listen-https, which needs certs the cambox uploaders do
# not carry) on the socket-passed fd, writing the uploaded journals under ${DEV1_JOURNAL_REMOTE_DIR}.
# The venue LAN is private, so plain HTTP is acceptable (matches the cambox uploader's http:// URL).
dev1_journal_remote_dropin_content() {
  cat <<EOF
[Service]
# camera-box #1311: plain HTTP receiver (the cambox uploaders use http://, not https).
ExecStart=
ExecStart=/usr/lib/systemd/systemd-journal-remote --listen-http=-3 --output=${DEV1_JOURNAL_REMOTE_DIR}/
EOF
}

# dev1_journal_remote_conf_content -> /etc/systemd/journal-remote.conf: no cryptographic sealing
# (plain HTTP on a private LAN) and split the store per-remote host so each box's journal is its own
# file under ${DEV1_JOURNAL_REMOTE_DIR}.
dev1_journal_remote_conf_content() {
  cat <<'EOF'
[Remote]
Seal=false
SplitMode=host
EOF
}

# --- planner ---------------------------------------------------------------------------------------

emit_one() {
  case "$1" in
    rsyslog) dev1_rsyslog_dropin_content ;;
    logrotate) dev1_logrotate_content ;;
    journal-remote-dropin) dev1_journal_remote_dropin_content ;;
    journal-remote-conf) dev1_journal_remote_conf_content ;;
    *) echo "unknown --emit target: $1 (rsyslog|logrotate|journal-remote-dropin|journal-remote-conf)" >&2; exit 2 ;;
  esac
}

print_plan() {
  cat <<PLAN
=== dev1 remote-log receiver install plan (#1311) -- SUPERVISOR runs this ============================

Receives the cam boxes' off-box logs:
  * netconsole kernel messages  -> UDP  :${DEV1_NETCONSOLE_PORT}  (rsyslog imudp)      -> ${DEV1_CAMBOX_LOG_DIR}/<ip>-kernel.log
  * systemd-journal-upload      -> HTTP :${DEV1_JOURNAL_PORT} (systemd-journal-remote) -> ${DEV1_JOURNAL_REMOTE_DIR}/<host>.journal

--- files to install ---
  ${DEV1_RSYSLOG_DROPIN}
  ${DEV1_LOGROTATE_CONF}
  ${DEV1_JOURNAL_REMOTE_CONF}
  ${DEV1_JOURNAL_REMOTE_DROPIN}

--- apply (run on dev1 as root) ---
  # 1. netconsole sink (rsyslog imudp is commented out in the stock /etc/rsyslog.conf; this drop-in re-enables it)
  install -d -m 0755 ${DEV1_CAMBOX_LOG_DIR}
  scripts/dev1-remote-log-install.sh --emit rsyslog   > ${DEV1_RSYSLOG_DROPIN}
  scripts/dev1-remote-log-install.sh --emit logrotate > ${DEV1_LOGROTATE_CONF}
  systemctl restart rsyslog
  ss -lunp | grep ':${DEV1_NETCONSOLE_PORT}\\b'   # verify: an imudp listener now exists

  # 2. journal sink
  apt-get install -y systemd-journal-remote
  install -d -m 2755 -o systemd-journal-remote -g systemd-journal-remote ${DEV1_JOURNAL_REMOTE_DIR}
  scripts/dev1-remote-log-install.sh --emit journal-remote-conf   > ${DEV1_JOURNAL_REMOTE_CONF}
  install -d ${DEV1_JOURNAL_REMOTE_DROPIN%/*}
  scripts/dev1-remote-log-install.sh --emit journal-remote-dropin > ${DEV1_JOURNAL_REMOTE_DROPIN}
  systemctl daemon-reload
  systemctl enable --now systemd-journal-remote.socket
  ss -ltnp | grep ':${DEV1_JOURNAL_PORT}\\b'      # verify: an HTTP listener now exists

--- MGMT_DEAD correlation (#1309/#1311) ---
When the dante-clock watchdog pages MGMT_DEAD / box-down for <box>, read the last 5 min of that box's
off-box kernel log on dev1 (replace <ip> with the box IP from targets.md):
  awk -v cutoff="\$(date -d '5 minutes ago' '+%Y-%m-%dT%H:%M:%S')" '\$1 >= cutoff' ${DEV1_CAMBOX_LOG_DIR}/<ip>-kernel.log
and the uploaded journal:
  journalctl --file ${DEV1_JOURNAL_REMOTE_DIR}/*<host>*.journal --since '5 minutes ago'

======================================================================================================
PLAN
}

apply_on_dev1() {
  [ "$(id -u)" -eq 0 ] || { echo "--apply must run as root (dev1)" >&2; exit 2; }
  install -d -m 0755 "$DEV1_CAMBOX_LOG_DIR"
  dev1_rsyslog_dropin_content > "$DEV1_RSYSLOG_DROPIN"
  dev1_logrotate_content > "$DEV1_LOGROTATE_CONF"
  systemctl restart rsyslog
  apt-get install -y systemd-journal-remote
  install -d -m 2755 -o systemd-journal-remote -g systemd-journal-remote "$DEV1_JOURNAL_REMOTE_DIR" 2>/dev/null \
    || install -d -m 2755 "$DEV1_JOURNAL_REMOTE_DIR"
  dev1_journal_remote_conf_content > "$DEV1_JOURNAL_REMOTE_CONF"
  install -d "${DEV1_JOURNAL_REMOTE_DROPIN%/*}"
  dev1_journal_remote_dropin_content > "$DEV1_JOURNAL_REMOTE_DROPIN"
  systemctl daemon-reload
  systemctl enable --now systemd-journal-remote.socket
  echo "dev1 remote-log receiver installed. Verify: ss -lunp | grep :${DEV1_NETCONSOLE_PORT} ; ss -ltnp | grep :${DEV1_JOURNAL_PORT}"
}

main() {
  case "${1:-}" in
    "") print_plan ;;
    --emit) shift; emit_one "${1:-}" ;;
    --apply) apply_on_dev1 ;;
    -h|--help) print_plan ;;
    *) echo "usage: $0 [--emit <target> | --apply]" >&2; exit 2 ;;
  esac
}

# Only run main when executed, not when sourced by the test harness.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
