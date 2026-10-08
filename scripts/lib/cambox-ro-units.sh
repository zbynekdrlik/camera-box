#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure gather/verdict/program builders; the only top-level
# statements are the lazy sources of its three sibling libs and one overridable constant) --
# deliberately NOT `set -euo pipefail`: sourcing runs this file in the CALLER's shell, so strict mode
# here would leak into every caller (the scripts/lib convention, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cambox-ro-units.sh -- the issue-1394 unit set of a read-only-root cambox, graded and
# applied in ONE place.
#
# Live 8.10.2026 (issue 1394, cam1/cam2/cam5): `systemctl is-system-running` read `degraded` on
# every box, with the same four failed units:
#   - logrotate.service, every 15 min: its state file sat on the read-only root;
#   - apt-daily.service / apt-daily-upgrade.service, every pass: /var/lib/apt is read-only, and the
#     timers were only disabled, which a package upgrade undoes;
#   - cambox-netconsole.service (issue 1311): one failed arm at boot, never retried.
# setup-device.sh now writes the fixes (the logrotate drop-in and the apt masks from the shared
# ro-root canon, scripts/lib/ro-root.sh; the retrying unit from scripts/lib/remote-logging.sh). This
# lib serves the two consumers that look at a LIVE box:
#   - verify-device.sh (ar): cambox_ro_units_gather_remote_snippet (read-only, over ssh) +
#     cambox_ro_units_verdict (pure);
#   - scripts/cambox-ro-units-apply.sh: cambox_ro_units_apply_program, the remote program that
#     brings an already-provisioned box up to date without a setup-device re-run.
# The program writes everything inside ONE rw window and closes it with the ONE verified close
# (scripts/lib/ro-window.sh, issue 1407): only file writes and masks inside, the root mode READ
# back, nothing started before the close. After the close it reloads, stops the masked apt units,
# clears the four units' failed state, runs logrotate once, restarts netconsole and reads back
# `is-system-running`. Rule: .claude/rules/ro-window.md (the sites table).

# --- the sibling libs (lazy-sourced; a caller that already sourced them keeps its own) ----------
if ! command -v ro_root_logrotate_dropin_content >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/ro-root.sh
  . "${BASH_SOURCE[0]%/*}/ro-root.sh"
fi
if ! command -v remote_log_netconsole_service_unit_content >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/remote-logging.sh
  . "${BASH_SOURCE[0]%/*}/remote-logging.sh"
fi
if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/ro-window.sh
  . "${BASH_SOURCE[0]%/*}/ro-window.sh"
fi

# The systemd unit dir on the box. Overridable so a test can point the gather and the program at a
# temp tree; production never sets it.
CAMBOX_RO_UNITS_UNIT_DIR="${CAMBOX_RO_UNITS_UNIT_DIR:-/etc/systemd/system}"

cambox_ro_units_logrotate_dropin_path() {
  printf '%s/%s\n' "$CAMBOX_RO_UNITS_UNIT_DIR" "$(ro_root_logrotate_dropin_relpath)"
}

cambox_ro_units_netconsole_unit_path() {
  printf '%s/%s.service\n' "$CAMBOX_RO_UNITS_UNIT_DIR" "$REMOTE_LOG_NC_SERVICE_NAME"
}

# cambox_ro_units_sha FUNCTION -> the sha256 hex of FUNCTION's output: the bytes setup-device
# writes, the same bytes the box's file must hold.
cambox_ro_units_sha() {
  local s
  s="$("$1" | sha256sum)"
  printf '%s\n' "${s%% *}"
}

# cambox_ro_units_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh (ar):
# one KEY=VALUE line per fact, read-only. A file that is absent reads `__ABSENT__`; a unit state
# systemctl does not print reads `<none>`. The last command is an echo, so ssh's rc is non-zero only
# on a transport failure.
cambox_ro_units_gather_remote_snippet() {
  local apt
  apt="$(ro_root_masked_apt_units)"
  printf '_rou_lr=%q\n' "$(cambox_ro_units_logrotate_dropin_path)"
  printf '_rou_nc=%q\n' "$(cambox_ro_units_netconsole_unit_path)"
  printf '_rou_nc_unit=%q\n' "${REMOTE_LOG_NC_SERVICE_NAME}.service"
  printf '_rou_apt="%s"\n' "${apt//$'\n'/ }"
  cat <<'REMOTE'
_rou_sha() { if [ -e "$1" ]; then _rou_s="$(sha256sum <"$1" 2>/dev/null)"; echo "${_rou_s%% *}"; else echo __ABSENT__; fi; }
echo "LR_DROPIN_SHA=$(_rou_sha "$_rou_lr")"
echo "LR_DROPIN_LOADED=$(systemctl show -p DropInPaths --value logrotate.service 2>/dev/null)"
echo "LR_RESULT=$(systemctl show -p Result --value logrotate.service 2>/dev/null)"
for _rou_u in $_rou_apt; do
  _rou_en="$(systemctl is-enabled "$_rou_u" 2>/dev/null || true)"
  _rou_act="$(systemctl is-active "$_rou_u" 2>/dev/null || true)"
  echo "APT_UNIT=$_rou_u ${_rou_en:-<none>} ${_rou_act:-<none>}"
done
echo "NC_UNIT_SHA=$(_rou_sha "$_rou_nc")"
echo "NC_RESTART=$(systemctl show -p Restart --value "$_rou_nc_unit" 2>/dev/null)"
REMOTE
}

# cambox_ro_units_verdict BLOCK -> `ok`, or one `FAIL: ...` line per broken facet. BLOCK is the
# gather snippet's output (or a test fixture). Fail-closed: a missing or unreadable fact is a FAIL,
# never "in place". Pure bash parsing (no grep/sed).
#   - the logrotate drop-in: byte-identical to ro_root_logrotate_dropin_content, and LOADED
#     (systemd's DropInPaths names it -- a file written without a daemon-reload does nothing yet);
#   - logrotate.service's last Result is `success` (it is `success` before the first run of a boot);
#   - each apt unit of ro_root_masked_apt_units is `masked` AND `inactive` (a masked timer still
#     running would elapse into its masked service and fail; a failed one keeps the box degraded);
#   - the netconsole unit is byte-identical to remote_log_netconsole_service_unit_content, and its
#     loaded Restart= is `on-failure`.
cambox_ro_units_verdict() {
  local block="${1:-}" line fails="" nl=$'\n'
  local lr_sha="" lr_loaded="" lr_result="" nc_sha="" nc_restart="" apt_lines=""
  local want_lr want_nc dropin nc_path u name en act found fix
  while IFS= read -r line; do
    case "$line" in
      LR_DROPIN_SHA=*) lr_sha="${line#*=}" ;;
      LR_DROPIN_LOADED=*) lr_loaded="${line#*=}" ;;
      LR_RESULT=*) lr_result="${line#*=}" ;;
      NC_UNIT_SHA=*) nc_sha="${line#*=}" ;;
      NC_RESTART=*) nc_restart="${line#*=}" ;;
      APT_UNIT=*) apt_lines="${apt_lines}${line#APT_UNIT=}${nl}" ;;
    esac
  done <<<"$block"
  want_lr="$(cambox_ro_units_sha ro_root_logrotate_dropin_content)"
  want_nc="$(cambox_ro_units_sha remote_log_netconsole_service_unit_content)"
  dropin="$(cambox_ro_units_logrotate_dropin_path)"
  nc_path="$(cambox_ro_units_netconsole_unit_path)"
  fix="run scripts/cambox-ro-units-apply.sh --apply for this box, or re-run setup-device.sh (issue 1394)"

  case "$lr_sha" in
    "$want_lr")
      case " $lr_loaded " in
        *" $dropin "*) : ;;
        *) fails="${fails}FAIL: logrotate.service has not loaded ${dropin} (DropInPaths='${lr_loaded}') -- a daemon-reload is missing; ${fix}${nl}" ;;
      esac
      ;;
    __ABSENT__) fails="${fails}FAIL: the logrotate drop-in ${dropin} is missing -- logrotate keeps its state on the read-only root and fails every 15 min, /var/log is not rotated; ${fix}${nl}" ;;
    "") fails="${fails}FAIL: the logrotate drop-in ${dropin} could not be read (no LR_DROPIN_SHA); ${fix}${nl}" ;;
    *) fails="${fails}FAIL: the logrotate drop-in ${dropin} differs from the ro-root canon (sha ${lr_sha:0:12} != ${want_lr:0:12}); ${fix}${nl}" ;;
  esac
  [ "$lr_result" = success ] \
    || fails="${fails}FAIL: logrotate.service last Result=${lr_result:-<unread>}, not success -- it fails on the read-only root (or a logrotate config is broken: journalctl -u logrotate.service); ${fix}${nl}"

  for u in $(ro_root_masked_apt_units); do
    found=0
    while read -r name en act; do
      [ "$name" = "$u" ] || continue
      found=1
      if [ "$en" != masked ]; then
        fails="${fails}FAIL: ${u} is ${en:-<none>}, not masked -- on the read-only root it fails every pass and a package upgrade re-enables a disabled unit; ${fix}${nl}"
      elif [ "$act" != inactive ]; then
        fails="${fails}FAIL: ${u} is masked but ${act:-<none>} -- stop it and clear its failed state (systemctl stop / reset-failed); ${fix}${nl}"
      fi
    done <<<"$apt_lines"
    [ "$found" = 1 ] || fails="${fails}FAIL: no state read for ${u} (is-enabled / is-active); ${fix}${nl}"
  done

  case "$nc_sha" in
    "$want_nc")
      [ "$nc_restart" = on-failure ] \
        || fails="${fails}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service is on disk but systemd runs it with Restart=${nc_restart:-<unread>} -- a daemon-reload is missing; ${fix}${nl}"
      ;;
    __ABSENT__) fails="${fails}FAIL: ${nc_path##*/} is missing at ${nc_path} -- issue-1311 netconsole is not provisioned; re-run setup-device.sh${nl}" ;;
    "") fails="${fails}FAIL: ${nc_path##*/} could not be read (no NC_UNIT_SHA); ${fix}${nl}" ;;
    *) fails="${fails}FAIL: ${nc_path##*/} differs from scripts/lib/remote-logging.sh (a unit without the issue-1394 Restart=on-failure stays failed after one missed arm); ${fix}${nl}" ;;
  esac

  if [ -n "$fails" ]; then
    printf '%s' "$fails"
  else
    printf 'ok\n'
  fi
}

# cambox_ro_units_apply_program -> the REMOTE program (root, `ssh ... bash -s`) that brings a live
# cambox's issue-1394 unit set up to date. Everything runs inside ONE function called with
# </dev/null: the program arrives on bash's stdin, so a command that read stdin would eat the rest
# of it, the verified close included (the rt-kernel-plan finding, .claude/rules/ro-window.md).
#   1. read (nothing written): which of the three need a write -- the logrotate drop-in, the
#      netconsole unit (only where issue 1311 put one; a box without it gets a NOTE, never a new
#      unit), the apt masks;
#   2. only when something needs a write: `mount -o remount,rw /` (a refused remount ends the
#      program before any write), the files written through a temp file + rename, the masks
#      (`--no-reload`), then the ONE verified close -- a root that does not read ro again exits 1
#      naming the writers, and nothing after it runs. A cambox runs read-only, so a root that was
#      already writable is also forced back read-only (the cambox-only doctrine of ro-window.md);
#   3. after the close: daemon-reload, stop the masked apt units, reset-failed the four units of the
#      live finding, run logrotate once (it must succeed), restart netconsole (it must arm; if it
#      cannot, the unit now retries every 30 s), read back `is-system-running` (must be running).
# Exit 0 only when every step held. The text is the same for every cambox (the box names itself).
cambox_ro_units_apply_program() {
  local apt lr nc
  apt="$(ro_root_masked_apt_units)"
  lr="$(ro_root_logrotate_dropin_content)"
  nc="$(remote_log_netconsole_service_unit_content)"
  cat <<'PROG'
# scripts/cambox-ro-units-apply.sh (issue 1394): the read-only-root unit set of this cambox.
_rou_main() {
set -eu
PROG
  printf '_rou_lr=%q\n' "$(cambox_ro_units_logrotate_dropin_path)"
  printf '_rou_lr_body=%q\n' "$lr"
  printf '_rou_lr_want=%q\n' "$(cambox_ro_units_sha ro_root_logrotate_dropin_content)"
  printf '_rou_nc=%q\n' "$(cambox_ro_units_netconsole_unit_path)"
  printf '_rou_nc_body=%q\n' "$nc"
  printf '_rou_nc_want=%q\n' "$(cambox_ro_units_sha remote_log_netconsole_service_unit_content)"
  printf '_rou_nc_unit=%q\n' "${REMOTE_LOG_NC_SERVICE_NAME}.service"
  printf '_rou_apt="%s"\n' "${apt//$'\n'/ }"
  cat <<'PROG'
_rou_box="$(hostname 2>/dev/null || echo this cambox)"
_rou_sha() { if [ -e "$1" ]; then _rou_s="$(sha256sum <"$1")"; echo "${_rou_s%% *}"; else echo __ABSENT__; fi; }
# Inside the window only: a temp file + rename, so a cut write never leaves half a unit file.
_rou_write() {
  mkdir -p "${1%/*}"
  printf '%s\n' "$2" >"$1.new"
  mv -f "$1.new" "$1"
}
# --- 1. what this box needs (read before anything is written) ---
_rou_do_lr=0
[ "$(_rou_sha "$_rou_lr")" = "$_rou_lr_want" ] || _rou_do_lr=1
_rou_nc_present=0
[ ! -e "$_rou_nc" ] || _rou_nc_present=1
_rou_do_nc=0
if [ "$_rou_nc_present" = 1 ] && [ "$(_rou_sha "$_rou_nc")" != "$_rou_nc_want" ]; then _rou_do_nc=1; fi
_rou_mask=""
for _rou_u in $_rou_apt; do
  [ "$(systemctl is-enabled "$_rou_u" 2>/dev/null || true)" = masked ] || _rou_mask="$_rou_mask $_rou_u"
done
echo "PLAN ($_rou_box): logrotate drop-in $([ "$_rou_do_lr" = 1 ] && echo write || echo in-place); netconsole unit $([ "$_rou_nc_present" = 0 ] && echo absent || { [ "$_rou_do_nc" = 1 ] && echo write || echo in-place; }); mask:${_rou_mask:- none (all masked)}"
if [ "$_rou_nc_present" = 0 ]; then
  echo "NOTE: no $_rou_nc on $_rou_box -- issue 1311 netconsole is not provisioned here; re-run setup-device.sh for it (this program writes no new unit)"
fi
# --- 2. the rw window, closed by the ONE verified close ---
_rou_open=0
_rou_close() {
  [ "$_rou_open" = 1 ] || return 0
  _rou_open=0
PROG
  ro_window_close_cmds "issue 1394" "\$_rou_box" \
    "The issue-1394 unit files are on disk, but the root stays read-WRITE, so this program runs no daemon-reload and starts nothing." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run scripts/cambox-ro-units-apply.sh --apply for this box (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_rou_close' EXIT
_rou_wrote=0
if [ "$_rou_do_lr" = 1 ] || [ "$_rou_do_nc" = 1 ] || [ -n "$_rou_mask" ]; then
mount -o remount,rw /
_rou_open=1
_rou_wrote=1
if [ "$_rou_do_lr" = 1 ]; then _rou_write "$_rou_lr" "$_rou_lr_body"; fi
if [ "$_rou_do_nc" = 1 ]; then _rou_write "$_rou_nc" "$_rou_nc_body"; fi
# The masks are links on the root; --no-reload: the daemon-reload runs after the close.
if [ -n "$_rou_mask" ]; then systemctl mask --no-reload $_rou_mask; fi
fi
_rou_close
trap - EXIT
if [ "$_rou_wrote" = 1 ]; then
  echo "OK: the unit files are written and the root of $_rou_box reads read-only again ($_row_opts)"
else
  echo "OK: nothing to write on $_rou_box -- the root was not touched"
fi
# --- 3. after the verified close: load, clear, run, read back ---
_rou_rc=0
systemctl daemon-reload
# A masked timer that is still running would elapse into its masked service and fail.
systemctl stop $_rou_apt
for _rou_u in logrotate.service $_rou_apt $_rou_nc_unit; do
  if systemctl is-failed --quiet "$_rou_u" 2>/dev/null; then
    systemctl reset-failed "$_rou_u"
    echo "  reset-failed $_rou_u"
  fi
done
if systemctl start logrotate.service; then
  echo "OK: logrotate.service ran with its state in /run"
else
  echo "FAIL: [issue 1394] logrotate.service failed on $_rou_box (Result=$(systemctl show -p Result --value logrotate.service 2>/dev/null || true)) -- read: journalctl -u logrotate.service -n 20" >&2
  _rou_rc=1
fi
if [ "$_rou_nc_present" = 1 ]; then
  if systemctl restart "$_rou_nc_unit"; then
    echo "OK: $_rou_nc_unit armed"
  else
    echo "FAIL: [issue 1394] $_rou_nc_unit did not arm on $_rou_box (Result=$(systemctl show -p Result --value "$_rou_nc_unit" 2>/dev/null || true)); it retries every 30 s now -- read: journalctl -u $_rou_nc_unit -n 20" >&2
    _rou_rc=1
  fi
fi
_rou_state="$(systemctl is-system-running 2>/dev/null || true)"
if [ "$_rou_state" = running ]; then
  echo "OK: is-system-running = running on $_rou_box"
else
  echo "FAIL: [issue 1394] is-system-running = ${_rou_state:-<unread>} on $_rou_box; failed units:" >&2
  systemctl --failed --no-legend --plain >&2 || true
  _rou_rc=1
fi
return "$_rou_rc"
}
_rou_main </dev/null
PROG
}
