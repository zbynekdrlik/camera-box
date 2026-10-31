#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (plus a tiny CLI when executed, which sets its
# own strict mode below), sourced by setup-device.sh / verify-device.sh + tests -- the scripts/lib
# convention: sourcing runs in the CALLER's shell (`set -euo pipefail`), so strict mode at file level
# would leak into it. Every function returns 0 on its success path and is safe as a bare statement.
#
# scripts/lib/cambox-jxs-target.sh -- issue 1387: the cambox JPEG XS link TARGET, provisioned.
#
# camera-box sends its JPEG XS copy of the camera to CAMERA_BOX_JXS_TARGET (src/jxs_send.rs); unset =
# NDI only. Until this lib the target lived only in a RUNTIME drop-in (/run/systemd/system/
# camera-box.service.d/jxs.conf, `systemctl edit --runtime`): the root is read-only, and a reboot
# reverted the camera to NDI only. Now:
#   * the target is DERIVED from the venue fact files, never typed: the venue strih is the strih-role
#     scripts/strih-boxes/*.env whose STRIH_CAMERAS lists the camera (the ONE camera -> venue-strih
#     lookup of scripts/lib/remote-logging.sh, issue 1394), the address its STRIH_IP, the port the
#     camera's N:PORT entry of its STRIH_JXS_CAMERAS (`none` = this camera stays NDI only; every PP
#     file declares none, the PP rollout is deferred by the owner);
#   * setup-device.sh STEP 7 writes /etc/systemd/system/camera-box.service.d/jxs.conf (the SAME name as
#     the runtime file, so /etc masks /run) -- enable-only, the next camera-box start takes it -- and
#     removes it on a camera its venue does not declare;
#   * verify-device.sh (ay) grades the file, the LOADED unit environment and the RUNNING camera-box's
#     environ, and FAILs a target on a camera its venue does not declare;
#   * a live box gets it without a setup-device re-run through cambox_jxs_apply_program (the CLI below
#     prints it per camera): the file written inside ONE rw window closed by the verified close
#     (scripts/lib/ro-window.sh, issue 1407); after the close the runtime drop-ins are removed,
#     daemon-reload, camera-box restarted ONLY when its running target differs (never started when it
#     is not running), and the read-back.
# Tests: tests/python/test_cambox_jxs_target_1387.py.

# The env var camera-box reads (src/jxs_send.rs JxsSender::start_from_env, pinned by pytest).
CAMBOX_JXS_ENV="CAMERA_BOX_JXS_TARGET"
# The persistent drop-in and the runtime drop-in dir. Overridable so a test points them at a temp tree.
CAMBOX_JXS_DROPIN="${CAMBOX_JXS_DROPIN:-/etc/systemd/system/camera-box.service.d/jxs.conf}"
CAMBOX_JXS_RUNTIME_DIR="${CAMBOX_JXS_RUNTIME_DIR:-/run/systemd/system/camera-box.service.d}"
# The /proc root the running environ is read from (a test seam).
CAMBOX_JXS_PROC="${CAMBOX_JXS_PROC:-/proc}"

if ! declare -F remote_log_sink_ip >/dev/null; then
  # shellcheck source=scripts/lib/remote-logging.sh
  . "${BASH_SOURCE[0]%/*}/remote-logging.sh"
fi

# cambox_jxs_target_ok VALUE -> 0 iff VALUE is `none` or IPv4:PORT (PORT 1..65535). The digit set is
# spelled out (a bracket range follows the locale).
cambox_jxs_target_ok() {
  local v="${1-}"
  [ "$v" = none ] && return 0
  remote_log_ipv4_ok "${v%:*}" || return 1
  [[ "${v##*:}" =~ ^[123456789][0123456789]{0,4}$ ]] && [ "${v##*:}" -le 65535 ] && [ "${v%:*}:${v##*:}" = "$v" ]
}

# cambox_jxs_target CAMERA -> `<venue strih STRIH_IP>:<port>` or `none` on stdout, rc 0; rc 1 + the
# lookup's own stderr line when no venue (or two) lists the camera, a fact file does not parse or the
# result is malformed. Never a guess.
cambox_jxs_target() {
  local row port ip t
  row="$(_remote_log_strih_row "${1-}")" || return 1
  ip="${row%%$'\t'*}"
  port="${row##*$'\t'}"
  if [ "$port" = none ]; then
    printf 'none\n'
    return 0
  fi
  t="${ip}:${port}"
  cambox_jxs_target_ok "$t" || { echo "cambox-jxs: ${1-}: '${t}' is not IPv4:PORT (issue 1387)" >&2; return 1; }
  printf '%s\n' "$t"
}

# cambox_jxs_dropin_content TARGET -> the drop-in text (no trailing newline; a writer adds one). rc 1 for
# `none` or a malformed target: such a camera carries no drop-in.
cambox_jxs_dropin_content() {
  [ "${1-}" != none ] && cambox_jxs_target_ok "${1-}" || return 1
  printf '%s\n%s\n%s' \
    "# issue 1387: the JPEG XS link target, derived from the venue fact files (scripts/lib/cambox-jxs-target.sh)." \
    "[Service]" "Environment=${CAMBOX_JXS_ENV}=$1"
}

# cambox_jxs_sha TARGET -> the sha256 the installed drop-in must have (content + one newline), or
# __ABSENT__ for `none` (no file at all).
cambox_jxs_sha() {
  local c s
  c="$(cambox_jxs_dropin_content "${1-}")" || { printf '__ABSENT__\n'; return 0; }
  s="$(printf '%s\n' "$c" | sha256sum)"
  printf '%s\n' "${s%% *}"
}

# cambox_jxs_dropin_install ROOT TARGET -> setup-device STEP 7: ROOT$CAMBOX_JXS_DROPIN written (temp
# file + rename, 0644) for a target, removed for `none`. Enable-only: the next camera-box start takes it.
# rc 1 + a stderr line on a bad target or a failed write.
cambox_jxs_dropin_install() {
  local path="${1:-}${CAMBOX_JXS_DROPIN}" t="${2-}" c
  cambox_jxs_target_ok "$t" || { echo "FAIL: [issue 1387] '${t}' is not a JPEG XS target (IPv4:PORT or none)" >&2; return 1; }
  if [ "$t" = none ]; then
    if [ -e "$path" ] || [ -L "$path" ]; then
      rm -f "$path" || { echo "FAIL: [issue 1387] could not remove ${path}" >&2; return 1; }
      echo "  JPEG XS: ${path} removed -- the venue declares no JPEG XS link for this camera (NDI only)"
    else
      echo "  JPEG XS: none declared for this camera by its venue (NDI only)"
    fi
    return 0
  fi
  c="$(cambox_jxs_dropin_content "$t")"
  if mkdir -p "${path%/*}" && printf '%s\n' "$c" >"$path.new" && chmod 0644 "$path.new" && mv -f "$path.new" "$path"; then
    echo "  JPEG XS: ${path} -> ${CAMBOX_JXS_ENV}=${t} (the next camera-box start sends JPEG XS to its venue strih)"
    return 0
  fi
  rm -f "$path.new"
  echo "FAIL: [issue 1387] could not write ${path}" >&2
  return 1
}

# cambox_jxs_gather_remote_snippet -> the REMOTE read-only bash verify-device (ay) runs over ssh. One
# KEY=VALUE line per fact: the drop-in's sha (__ABSENT__), the loaded unit's LAST target assignment
# (systemctl show Environment; empty = none), whether that read worked, the running camera-box's
# environ target (__NONE__ = not set, __UNREAD__ = no running process / unreadable) and the runtime
# drop-ins that set the variable. The last command is an echo (ssh's rc = transport only).
cambox_jxs_gather_remote_snippet() {
  printf '_cjx_dropin=%q\n_cjx_rundir=%q\n_cjx_proc=%q\n_cjx_var=%q\n' \
    "$CAMBOX_JXS_DROPIN" "$CAMBOX_JXS_RUNTIME_DIR" "$CAMBOX_JXS_PROC" "$CAMBOX_JXS_ENV"
  cat <<'REMOTE'
if [ -e "$_cjx_dropin" ]; then _cjx_s="$(sha256sum <"$_cjx_dropin" 2>/dev/null || true)"; echo "JXS_DROPIN_SHA=${_cjx_s%% *}"; else echo "JXS_DROPIN_SHA=__ABSENT__"; fi
if _cjx_env="$(systemctl show -p Environment --value camera-box 2>/dev/null)"; then
  echo "JXS_LOADED_READ=1"
  echo "JXS_LOADED=$(printf '%s\n' "$_cjx_env" | tr ' ' '\n' | awk -v k="$_cjx_var=" 'index($0, k) == 1 { v = substr($0, length(k) + 1) } END { print v }')"
else
  echo "JXS_LOADED_READ=0"
fi
_cjx_pid="$(systemctl show -p MainPID --value camera-box 2>/dev/null || true)"
case "$_cjx_pid" in
  "" | 0 | *[!0123456789]*) echo "JXS_RUNNING=__UNREAD__" ;;
  *)
    if [ -r "$_cjx_proc/$_cjx_pid/environ" ]; then
      _cjx_run="$(tr '\0' '\n' <"$_cjx_proc/$_cjx_pid/environ" | awk -v k="$_cjx_var=" 'index($0, k) == 1 { v = substr($0, length(k) + 1); f = 1 } END { if (f) print v; else print "__NONE__" }')"
      echo "JXS_RUNNING=${_cjx_run:-__NONE__}"
    else
      echo "JXS_RUNNING=__UNREAD__"
    fi
    ;;
esac
_cjx_rt=""
for _cjx_f in "$_cjx_rundir"/*.conf; do
  [ -f "$_cjx_f" ] || continue
  if grep -q "^Environment=.*$_cjx_var=" "$_cjx_f" 2>/dev/null; then _cjx_rt="$_cjx_rt $_cjx_f"; fi
done
echo "JXS_RUNTIME_DROPINS=${_cjx_rt# }"
REMOTE
}

# cambox_jxs_verdict BLOCK WANT -> `ok`, or one `FAIL: ...` line per broken facet. BLOCK = the gather's
# output, WANT = cambox_jxs_target of this box. Fail-closed: an unread fact is a FAIL. Pure bash.
cambox_jxs_verdict() {
  local block="${1:-}" want="${2-}" line sha="" lread="" loaded="" running="" rt="" fails="" nl=$'\n'
  local have_sha=0 have_loaded=0 have_running=0 have_rt=0 fix wantsha
  while IFS= read -r line; do
    case "$line" in
      JXS_DROPIN_SHA=*) sha="${line#*=}"; have_sha=1 ;;
      JXS_LOADED_READ=*) lread="${line#*=}" ;;
      JXS_LOADED=*) loaded="${line#*=}"; have_loaded=1 ;;
      JXS_RUNNING=*) running="${line#*=}"; have_running=1 ;;
      JXS_RUNTIME_DROPINS=*) rt="${line#*=}"; have_rt=1 ;;
    esac
  done <<<"$block"
  fix="fix: bash scripts/lib/cambox-jxs-target.sh --apply-program <this box> | ssh root@<this box> bash -s (issue 1387)"
  if ! cambox_jxs_target_ok "$want"; then
    printf 'FAIL: no JPEG XS target resolved for this box (got %s) -- its venue strih fact file must list it in STRIH_CAMERAS (issue 1387)\n' "'${want}'"
    return 0
  fi
  wantsha="$(cambox_jxs_sha "$want")"
  if [ "$have_sha" = 0 ] || [ -z "$sha" ]; then
    fails="${fails}FAIL: ${CAMBOX_JXS_DROPIN} could not be read (no JXS_DROPIN_SHA); ${fix}${nl}"
  elif [ "$sha" != "$wantsha" ]; then
    if [ "$want" = none ]; then
      fails="${fails}FAIL: ${CAMBOX_JXS_DROPIN} exists, but the venue declares no JPEG XS link for this camera (STRIH_JXS_CAMERAS); ${fix}${nl}"
    elif [ "$sha" = __ABSENT__ ]; then
      fails="${fails}FAIL: ${CAMBOX_JXS_DROPIN} is missing -- a reboot drops the camera to NDI only (${CAMBOX_JXS_ENV}=${want} declared); ${fix}${nl}"
    else
      fails="${fails}FAIL: ${CAMBOX_JXS_DROPIN} differs from the declared target ${want}; ${fix}${nl}"
    fi
  fi
  if [ "$lread" != 1 ] || [ "$have_loaded" = 0 ]; then
    fails="${fails}FAIL: the loaded camera-box environment could not be read (systemctl show -p Environment); ${fix}${nl}"
  elif [ "$want" = none ] && [ -n "$loaded" ]; then
    fails="${fails}FAIL: camera-box.service loads ${CAMBOX_JXS_ENV}=${loaded}, but the venue declares no JPEG XS link for this camera (hand state); ${fix}${nl}"
  elif [ "$want" != none ] && [ "$loaded" != "$want" ]; then
    fails="${fails}FAIL: camera-box.service loads ${CAMBOX_JXS_ENV}=${loaded:-<unset>}, not ${want} -- a daemon-reload is missing or another drop-in overrides it; ${fix}${nl}"
  fi
  if [ "$have_running" = 0 ] || [ -z "$running" ] || [ "$running" = __UNREAD__ ]; then
    fails="${fails}FAIL: the running camera-box's ${CAMBOX_JXS_ENV} could not be read (no running process or an unreadable /proc environ) -- read the (c) row first${nl}"
  elif [ "$want" = none ] && [ "$running" != __NONE__ ]; then
    fails="${fails}FAIL: the running camera-box sends JPEG XS to ${running}, but the venue declares no JPEG XS link for this camera (hand state); ${fix}${nl}"
  elif [ "$want" != none ] && [ "$running" != "$want" ]; then
    fails="${fails}FAIL: the running camera-box has ${CAMBOX_JXS_ENV}=${running/__NONE__/<unset>}, not ${want} -- it was not restarted onto the declared target; ${fix}${nl}"
  fi
  if [ "$have_rt" = 0 ]; then
    fails="${fails}FAIL: the runtime drop-ins in ${CAMBOX_JXS_RUNTIME_DIR} could not be listed; ${fix}${nl}"
  elif [ -n "$rt" ]; then
    fails="${fails}FAIL: a runtime drop-in sets ${CAMBOX_JXS_ENV} (${rt}) -- hand state the provisioned file replaces; ${fix}${nl}"
  fi
  if [ -n "$fails" ]; then
    printf '%s' "$fails"
  else
    printf 'ok\n'
  fi
}

# cambox_jxs_verdict_oneline VERDICT -> the FAIL lines joined by `; ` (prefixes cut), for one fail() line.
cambox_jxs_verdict_oneline() {
  local line out=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    out="${out:+${out}; }${line#FAIL: }"
  done <<<"${1:-}"
  printf '%s\n' "$out"
}

# cambox_jxs_apply_program TARGET -> the REMOTE program (root, `ssh ... bash -s`) that brings a live
# cambox to TARGET (cambox_jxs_target of that box) without a setup-device re-run. Everything runs inside
# ONE function called with </dev/null (the program arrives on bash's stdin; the ro-window stdin rule).
#   1. read (nothing written): the root mode, whether the drop-in matches, the running camera-box's
#      target, the runtime drop-ins that set the variable;
#   2. the rw window, when the drop-in needs a write / removal OR the root does not read `ro` (a cambox
#      runs read-only; a writable root is put back by the verified close before anything runs): the
#      drop-in (temp file + rename) or its removal, then the ONE verified close; a failure inside
#      reaches the EXIT trap, which runs the same close;
#   3. after the close: the runtime drop-ins removed (/run is tmpfs, no window), daemon-reload, and
#      camera-box RESTARTED only when it runs a target other than TARGET (a ~3 s camera outage; never a
#      start of a stopped camera-box), then the read-back (loaded + running). Exit 0 only when both hold.
# rc 1 (nothing printed) for a malformed TARGET.
cambox_jxs_apply_program() {
  local t="${1-}" body=""
  cambox_jxs_target_ok "$t" || { echo "FAIL: [issue 1387] '${t}' is not a JPEG XS target (IPv4:PORT or none)" >&2; return 1; }
  if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
    # shellcheck source=scripts/lib/ro-window.sh
    . "${BASH_SOURCE[0]%/*}/ro-window.sh"
  fi
  [ "$t" = none ] || body="$(cambox_jxs_dropin_content "$t")"
  cat <<'PROG'
# scripts/lib/cambox-jxs-target.sh (issue 1387): this cambox's JPEG XS link target.
_cjx_main() {
set -eu
PROG
  printf '_cjx_want=%q\n_cjx_dropin=%q\n_cjx_body=%q\n_cjx_want_sha=%q\n' \
    "$t" "$CAMBOX_JXS_DROPIN" "$body" "$(cambox_jxs_sha "$t")"
  printf '_cjx_rundir=%q\n_cjx_proc=%q\n_cjx_var=%q\n' "$CAMBOX_JXS_RUNTIME_DIR" "$CAMBOX_JXS_PROC" "$CAMBOX_JXS_ENV"
  printf '%s\n' "$(declare -f ro_root_mount_mode)"
  cat <<'PROG'
_cjx_box="$(hostname 2>/dev/null || echo this cambox)"
_cjx_sha() { if [ -e "$1" ]; then _cjx_s="$(sha256sum <"$1")"; echo "${_cjx_s%% *}"; else echo __ABSENT__; fi; }
# The running camera-box's target: the value, __NONE__ (not set) or __UNREAD__ (no running process).
_cjx_running() {
  local pid
  pid="$(systemctl show -p MainPID --value camera-box 2>/dev/null || true)"
  case "$pid" in "" | 0 | *[!0123456789]*) echo __UNREAD__; return 0 ;; esac
  [ -r "$_cjx_proc/$pid/environ" ] || { echo __UNREAD__; return 0; }
  tr '\0' '\n' <"$_cjx_proc/$pid/environ" | awk -v k="$_cjx_var=" 'index($0, k) == 1 { v = substr($0, length(k) + 1); f = 1 } END { if (f) print v; else print "__NONE__" }'
}
_cjx_loaded() {
  systemctl show -p Environment --value camera-box 2>/dev/null | tr ' ' '\n' \
    | awk -v k="$_cjx_var=" 'index($0, k) == 1 { v = substr($0, length(k) + 1) } END { print v }'
}
# --- 1. what this box needs (read before anything is written) ---
_cjx_mode="$(ro_root_mount_mode "$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)")"
_cjx_do=0
[ "$(_cjx_sha "$_cjx_dropin")" = "$_cjx_want_sha" ] || _cjx_do=1
_cjx_rt=""
for _cjx_f in "$_cjx_rundir"/*.conf; do
  [ -f "$_cjx_f" ] || continue
  if grep -q "^Environment=.*$_cjx_var=" "$_cjx_f"; then _cjx_rt="$_cjx_rt $_cjx_f"; fi
done
_cjx_run0="$(_cjx_running)"
_cjx_wantrun="$_cjx_want"
[ "$_cjx_want" != none ] || _cjx_wantrun=__NONE__
echo "PLAN ($_cjx_box): root $_cjx_mode; JPEG XS target $_cjx_want; drop-in $([ "$_cjx_do" = 1 ] && { [ "$_cjx_want" = none ] && echo remove || echo write; } || echo in-place); runtime drop-ins:${_cjx_rt:- none}; camera-box runs ${_cjx_run0}"
_cjx_rc=0
# --- 2. the rw window, closed by the ONE verified close ---
_cjx_open=0
_cjx_close() {
  [ "$_cjx_open" = 1 ] || return 0
  _cjx_open=0
PROG
  ro_window_close_cmds "issue 1387" "\$_cjx_box" \
    "The JPEG XS drop-in is on disk, but the root stays read-WRITE, so this program reloads and restarts nothing." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run the cambox JPEG XS apply for this box (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_cjx_close' EXIT
if [ "$_cjx_do" = 1 ] || [ "$_cjx_mode" != ro ]; then
  if ! mount -o remount,rw /; then
    echo "FAIL: [issue 1387] could not remount / read-write on $_cjx_box -- nothing was written" >&2
    exit 1
  fi
  _cjx_open=1
  if [ "$_cjx_do" = 1 ] && [ "$_cjx_want" = none ]; then
    rm -f "$_cjx_dropin" || { echo "FAIL: [issue 1387] could not remove $_cjx_dropin on $_cjx_box -- the window is closed again by the verified close" >&2; exit 1; }
  elif [ "$_cjx_do" = 1 ]; then
    if ! { mkdir -p "${_cjx_dropin%/*}" && printf '%s\n' "$_cjx_body" >"$_cjx_dropin.new" && chmod 0644 "$_cjx_dropin.new" && mv -f "$_cjx_dropin.new" "$_cjx_dropin"; }; then
      rm -f "$_cjx_dropin.new"
      echo "FAIL: [issue 1387] could not write $_cjx_dropin on $_cjx_box -- the window is closed again by the verified close" >&2
      exit 1
    fi
  fi
fi
_cjx_close
trap - EXIT
# --- 3. after the verified close: runtime drop-ins, reload, restart only on a difference, read back ---
for _cjx_f in $_cjx_rt; do
  if rm -f "$_cjx_f"; then echo "  removed the runtime drop-in $_cjx_f (hand state)"; else echo "FAIL: [issue 1387] could not remove $_cjx_f on $_cjx_box" >&2; _cjx_rc=1; fi
done
if ! systemctl daemon-reload; then
  echo "FAIL: [issue 1387] systemctl daemon-reload failed on $_cjx_box -- the drop-in is not loaded" >&2
  _cjx_rc=1
fi
if [ "$_cjx_run0" = __UNREAD__ ]; then
  echo "NOTE: camera-box is not running on $_cjx_box -- never started here; it takes the target at its next start"
elif [ "$_cjx_run0" != "$_cjx_wantrun" ]; then
  if systemctl restart camera-box; then
    echo "  restarted camera-box ($_cjx_run0 -> $_cjx_want)"
  else
    echo "FAIL: [issue 1387] camera-box did not restart on $_cjx_box -- read: journalctl -u camera-box -n 30" >&2
    _cjx_rc=1
  fi
else
  echo "  camera-box already runs the declared target -- not restarted"
fi
_cjx_l="$(_cjx_loaded)"
_cjx_r="$(_cjx_running)"
if [ "${_cjx_l:-__NONE__}" != "$_cjx_wantrun" ]; then
  echo "FAIL: [issue 1387] READBACK ($_cjx_box): the loaded unit carries ${_cjx_var}=${_cjx_l:-<unset>}, not $_cjx_want" >&2
  _cjx_rc=1
elif [ "$_cjx_r" != __UNREAD__ ] && [ "$_cjx_r" != "$_cjx_wantrun" ]; then
  echo "FAIL: [issue 1387] READBACK ($_cjx_box): the running camera-box carries ${_cjx_var}=${_cjx_r}, not $_cjx_want" >&2
  _cjx_rc=1
else
  echo "READBACK ($_cjx_box): OK ${_cjx_var}=${_cjx_want} (loaded; running ${_cjx_r})"
fi
return "$_cjx_rc"
}
_cjx_main </dev/null
PROG
}

# --- CLI (executed, not sourced) -------------------------------------------------------------------
#   bash scripts/lib/cambox-jxs-target.sh --target camN          the target its venue declares (or none)
#   bash scripts/lib/cambox-jxs-target.sh --apply-program camN   the live-box program, then:
#        ... | sshpass -p "$DEVICE_ROOT_PW" ssh -o UserKnownHostsFile=/dev/null root@<camN> bash -s
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  case "${1:-}" in
    --target) _cjx_t="$(cambox_jxs_target "${2:-}")"; printf '%s\n' "$_cjx_t" ;;
    --apply-program)
      _cjx_t="$(cambox_jxs_target "${2:-}")"
      _cjx_p="$(cambox_jxs_apply_program "$_cjx_t")"
      printf '%s\n' "$_cjx_p"
      ;;
    *)
      echo "usage: $0 --target camN | --apply-program camN" >&2
      exit 2
      ;;
  esac
fi
