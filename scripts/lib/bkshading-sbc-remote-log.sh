#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions, no top-level statements that run anything);
# sourced by scripts/bkshading-provision-sbc.sh, which carries set -euo pipefail itself -- a `set -e`
# here would change the caller's shell options (the scripts/lib convention).
#
# scripts/lib/bkshading-sbc-remote-log.sh -- issue 808: the handheld shading board's RAM journal off the
# board, and its boot clock file.
#
# WHY: the board runs on a read-only root with `Storage=volatile`, so its journal dies with every power
# cycle (28.10.2026: the cause of handheld-1's PP outage was gone). The ONE remote-logging lib
# (scripts/lib/remote-logging.sh, issue 1311/1394) gives the candidates (remote_log_board_sink_ips: the
# strih fact files that name the board), the wrapper that picks the answering one at each start and the
# systemd-journal-upload drop-in; this lib installs and grades them on the board. The venue strih's
# receiver (setup-strih step 16k, SplitMode=host) stores it as its own remote-<hostname> file, no
# receiver change.
#
# The CLOCK: fake-hwclock-save is masked (it cannot write /etc/fake-hwclock.data on the ro root), so the
# file keeps the date of the last rw boot and every boot starts there until timesyncd syncs (~44 s after
# the WiFi). --install, inside its rw window, writes the current time into the file when the clock is
# NTP-synchronized and later than the file -- never an unsynchronized or an older time. The seconds
# before the sync remain: an RTC-less board on a ro root has no durable clock, and journal entries from
# that window keep their boot id and monotonic time.

# bkshading_sbc_board_name -> this board's name = its relay id (the relay's default --relay-id is the
# hostname). BKSHADING_SBC_HOSTNAME is the test seam.
bkshading_sbc_board_name() {
  if [ -n "${BKSHADING_SBC_HOSTNAME:-}" ]; then
    printf '%s\n' "$BKSHADING_SBC_HOSTNAME"
  else
    tr -d '\n' </proc/sys/kernel/hostname
    printf '\n'
  fi
}

# bkshading_sbc_upload_dropin_text -> the board's systemd-journal-upload drop-in (the cambox drop-in with
# ExecStart = the wrapper).
bkshading_sbc_upload_dropin_text() {
  remote_log_journal_upload_dropin_content "$REMOTE_LOG_BOARD_UPLOAD_SCRIPT_PATH"
}

# bkshading_sbc_remote_log_install SINKS SCRIPT_DEST DROPIN_DEST JU_BIN APT_GET -> install the uploader
# package when JU_BIN is missing, write the wrapper (0755) and the drop-in (0644), each through a temp
# file + rename and only when it differs. Enable-only: the caller enables the unit after its
# daemon-reload, nothing is started. rc 1 + a named stderr line on a failure.
bkshading_sbc_remote_log_install() {
  local sinks="$1" script_dest="$2" dropin_dest="$3" ju_bin="$4" apt_get="$5" text
  local -a ips=()
  read -r -a ips <<<"$sinks"
  if [ ! -x "$ju_bin" ]; then
    echo "  installing ${REMOTE_LOG_JU_PKG} (systemd-journal-upload, the board journal's way off-box) via apt ..."
    "$apt_get" update -qq || { echo "ERROR: apt-get update failed -- the journal uploader is not installed" >&2; return 1; }
    "$apt_get" install -y -qq "$REMOTE_LOG_JU_PKG" \
      || { echo "ERROR: apt-get install ${REMOTE_LOG_JU_PKG} failed" >&2; return 1; }
    [ -x "$ju_bin" ] || { echo "ERROR: ${ju_bin} still missing after installing ${REMOTE_LOG_JU_PKG}" >&2; return 1; }
  fi
  text="$(remote_log_board_upload_script_content "${ips[@]}")" || return 1
  _bkshading_sbc_put "$script_dest" 0755 "$text" || return 1
  text="$(bkshading_sbc_upload_dropin_text)" || return 1
  _bkshading_sbc_put "$dropin_dest" 0644 "$text" || return 1
  echo "  the board journal ships to the venue strih: first answering of ${sinks} :${REMOTE_LOG_JOURNAL_PORT} ($script_dest + $dropin_dest)"
}

# _bkshading_sbc_put PATH MODE TEXT -> write TEXT + a newline to PATH with MODE when the bytes or the mode
# differ (temp file in the same dir + rename), else leave it.
_bkshading_sbc_put() {
  local path="$1" mode="$2" text="$3"
  if [ -f "$path" ] && [ "$(<"$path")" = "$text" ] && [ "$(stat -c %a "$path")" = "${mode#0}" ]; then
    return 0
  fi
  mkdir -p "$(dirname "$path")" || return 1
  printf '%s\n' "$text" >"$path.new.$$" || { rm -f "$path.new.$$"; return 1; }
  chmod "$mode" "$path.new.$$" && mv -f "$path.new.$$" "$path"
}

# bkshading_sbc_remote_log_check BOARD SCRIPT_DEST DROPIN_DEST JU_BIN SYSTEMCTL -> one OK row, or a FAIL
# row per gap on stderr; rc 1 on any FAIL. Graded: the board is named by a strih fact file (else there
# is no sink), the uploader is installed, the wrapper + the drop-in are byte-identical to this
# checkout's generators, systemd-journal-upload is enabled. Its ACTIVE state is not graded: it depends
# on the venue strih answering, like the cambox (ak) bar.
bkshading_sbc_remote_log_check() {
  local board="$1" script_dest="$2" dropin_dest="$3" ju_bin="$4" systemctl="$5" sinks want en fail=0
  local -a ips=()
  if ! sinks="$(remote_log_board_sink_ips "$board")"; then
    echo "FAIL: no log sink for this board ('$board', reason above) -- add N=sbc-relay@register:${board} to its venue strih fact file" >&2
    return 1
  fi
  read -r -a ips <<<"$sinks"
  if [ ! -x "$ju_bin" ]; then
    echo "FAIL: $ju_bin missing -- the board journal never leaves the board; re-run --install (it installs ${REMOTE_LOG_JU_PKG})" >&2
    fail=1
  fi
  want="$(remote_log_board_upload_script_content "${ips[@]}")"
  if [ ! -x "$script_dest" ] || [ "$(<"$script_dest")" != "$want" ]; then
    echo "FAIL: $script_dest missing, not executable or differs from this checkout (sinks ${sinks}) -- re-run --install" >&2
    fail=1
  fi
  if [ ! -r "$dropin_dest" ] || [ "$(<"$dropin_dest")" != "$(bkshading_sbc_upload_dropin_text)" ]; then
    echo "FAIL: $dropin_dest missing or differs -- the uploader would not run the wrapper; re-run --install" >&2
    fail=1
  fi
  en="$("$systemctl" is-enabled systemd-journal-upload.service 2>/dev/null || true)"
  if [ "$en" != enabled ]; then
    echo "FAIL: systemd-journal-upload.service not enabled (is-enabled=${en:-<none>}) -- re-run --install" >&2
    fail=1
  fi
  [ "$fail" = 0 ] || return 1
  echo "OK: the board journal ships off-box (systemd-journal-upload enabled, first answering of ${sinks} :${REMOTE_LOG_JOURNAL_PORT})"
}

# bkshading_sbc_clock_time_ok VALUE -> 0 iff VALUE is `YYYY-MM-DD HH:MM:SS` (fake-hwclock's own format).
# The digit set is spelled out: a bracket RANGE follows the locale in bash.
bkshading_sbc_clock_time_ok() {
  local d='[0123456789]'
  [[ "${1-}" =~ ^${d}{4}-${d}{2}-${d}{2}\ ${d}{2}:${d}{2}:${d}{2}$ ]]
}

# bkshading_sbc_clock_file_refresh FILE TIMEDATECTL -> move FILE (/etc/fake-hwclock.data) forward to the
# current UTC time when the board has the file, the clock is NTP-synchronized and the time is later than
# the file's. Prints one line; always rc 0 (a stale boot clock is not worth failing the install).
# BKSHADING_SBC_NOW replaces the clock read (test seam).
bkshading_sbc_clock_file_refresh() {
  local file="$1" timedatectl="$2" sync now old
  local LC_ALL=C # the [[ > ]] below compares bytes, never by the locale's collation
  if [ ! -f "$file" ]; then
    echo "  no $file on this board (no fake-hwclock) -- boot clock file skipped"
    return 0
  fi
  old="$(head -n 1 "$file" 2>/dev/null || true)"
  sync="$("$timedatectl" show -p NTPSynchronized --value 2>/dev/null || true)"
  if [ "$sync" != yes ]; then
    echo "  boot clock file $file kept at '${old}': the clock is not synchronized (NTPSynchronized=${sync:-<unread>})"
    return 0
  fi
  now="${BKSHADING_SBC_NOW:-$(date -u '+%Y-%m-%d %H:%M:%S')}"
  if ! bkshading_sbc_clock_time_ok "$now"; then
    echo "  boot clock file $file kept: the clock read '${now}' is not YYYY-MM-DD HH:MM:SS"
    return 0
  fi
  if bkshading_sbc_clock_time_ok "$old" && ! [[ "$now" > "$old" ]]; then
    echo "  boot clock file $file kept at '${old}' (not older than now, ${now})"
    return 0
  fi
  if printf '%s\n' "$now" >"$file.new.$$" && mv -f "$file.new.$$" "$file"; then
    echo "  boot clock file $file moved from '${old}' to '${now}' (the next boot starts there, not at the last rw boot)"
  else
    rm -f "$file.new.$$"
    echo "  WARNING: could not write $file -- the boot clock stays at '${old}'" >&2
  fi
  return 0
}
