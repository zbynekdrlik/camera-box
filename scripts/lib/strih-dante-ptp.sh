#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-ptp.sh -- issue 1485, ROZHODNUTE 6628785864 (owner 4.11.2026): every dstack
# device on strih-snv (the intercom hub's `strih-snv-intercom`, the OBS dante-input's `strih-obs`) follows
# Dante PTP IN PROCESS: dstack v0.6.23's `ClockSource::Linked` starts dsync's follower (dsync-ptp: UDP
# 319 + 320 bound on INADDR_ANY, joined to 224.0.1.129 on the device's interface, receive only) and
# falls back to the dsync service's clock when the follower cannot start. The dsync service stays the
# ONLY writer of the system clock and the fallback for the rest of the box.
#
# What this lib holds (the leg filter's PTP rule is strih_dante_nft_input in strih-dante-leg.sh):
#   - the pin threshold: STRIH_DANTE_PTP_FIRST_DSTACK, the first dstack whose devices follow in process;
#     a component (intercom | obs) is READY when its release pin's DSTACK_VERSION is at or above it
#     (strih_dante_ptp_pin_ready). Until then nothing opens 319 (the leg rule needs one, the box-wide
#     sysctl both: strih_dante_ptp_leg_open / strih_dante_ptp_all_ready) and the rows
#     below are NOTEs: the threshold follows the pins.
#   - the right of the desktop-user OBS to bind 319 (design 6629112890): the box-wide sysctl
#     `net.ipv4.ip_unprivileged_port_start = 319` in /etc/sysctl.d/60-strih-dante-ptp-port.conf on a box
#     whose program comes from Dante (strih_program_dante_declared). Not a file capability on /usr/bin/obs
#     (a secure exec: libdbus / GLib refuse the session bus, no core dumps, re-applied after every bundle
#     install) and not an ambient capability through the user manager (pam_systemd replaces its ambient
#     set). The cost: every unprivileged process may bind 319-1023. The hub gets its capability from its
#     own unit (the intercom release), graded by verify item 16.
#       strih_dante_ptp_port_install    setup-strih step 13 via strih_dante_input_install (root)
#       strih_dante_ptp_port_remove     the same on every other box (ours only, header-checked)
#       strih_dante_ptp_port_grade      verify item 9's `(program-dante) ptp-port` row
#   - the media clock each device runs on: the LAST `media clock source source=<X>` line dstack logs
#     (`dstack::clock::linked`; the obs release forwards it into the OBS log as
#     `HH:MM:SS.mmm: dante-input dlink:  INFO dstack::clock::linked: media clock source source=Internal
#     from=Service`, the hub into its journal). Internal = OK; Service, None, no line = FAIL once the
#     component is READY, a NOTE before. A `did not start` warning is named with its reason.
#       strih_dante_media_clock_verdict (stdin)      -> one line `<kind> <from> <reason>` (pure)
#       strih_dante_media_clock_grade LABEL COMPONENT (stdin) -> one row through ok / bad / note
#
# Seams: STRIH_DANTE_RX_PIN_ROOT (a prefix for /etc and /proc, the Dante libs' one seam),
# STRIH_DANTE_RX_PIN_OWNER (root:root), STRIH_DANTE_PTP_INTERCOM_PIN / STRIH_DANTE_PTP_OBS_PIN (the pins).

STRIH_DANTE_PTP_FIRST_DSTACK=v0.6.23
STRIH_DANTE_PTP_PORT=319
STRIH_DANTE_PTP_PORT_DEFAULT=1024
STRIH_DANTE_PTP_PORT_HEADER="# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it."
STRIH_DANTE_PTP_LIB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! declare -F release_version_cmp >/dev/null; then
  # shellcheck source=scripts/lib/release-version.sh
  . "$(dirname "${BASH_SOURCE[0]}")/release-version.sh"   # the ONE version-order rule (issue 1476)
fi

# --- the pin threshold --------------------------------------------------------------------------------

# strih_dante_ptp_version_ge A B -> rc 0 iff vA >= vB by the repo's one version-order rule
# (release_version_cmp, a leading `v` dropped); rc 1 when below, rc 2 when either is not a version.
strih_dante_ptp_version_ge() {
  local c
  c="$(release_version_cmp "${1#v}" "${2#v}")" || return 2
  [ "$c" != -1 ]
}

# strih_dante_ptp_pin_dstack intercom|obs -> the dstack version that component's release pin carries
# (nothing when the pin or its line is unreadable). Always rc 0.
strih_dante_ptp_pin_dstack() {
  local f key
  case "${1-}" in
    intercom) f="${STRIH_DANTE_PTP_INTERCOM_PIN:-${STRIH_DANTE_PTP_LIB_ROOT}/scripts/intercom-release.pin}"; key=INTERCOM_RELEASE_DSTACK_VERSION ;;
    obs) f="${STRIH_DANTE_PTP_OBS_PIN:-${STRIH_DANTE_PTP_LIB_ROOT}/scripts/obs-release.pin}"; key=OBS_RELEASE_DSTACK_VERSION ;;
    *) return 0 ;;
  esac
  sed -n "s/^${key}=//p" "$f" 2>/dev/null | head -n 1 || true
}

# strih_dante_ptp_pin_ready intercom|obs -> rc 0 iff that pin's dstack is at or above
# STRIH_DANTE_PTP_FIRST_DSTACK (an unreadable version reads as below).
strih_dante_ptp_pin_ready() {
  strih_dante_ptp_version_ge "$(strih_dante_ptp_pin_dstack "${1-}")" "$STRIH_DANTE_PTP_FIRST_DSTACK"
}

# strih_dante_ptp_leg_open -> rc 0 iff either pinned device (the hub, the OBS) follows PTP in process, so
# the leg admits the PTP group (strih_dante_nft_input). Before that nothing on the leg binds 319 on
# purpose: a dstack below the threshold that could bind it would start its old follower beside dsync
# (the dantesync#109 port collision the earlier rule guarded against).
strih_dante_ptp_leg_open() { strih_dante_ptp_pin_ready intercom || strih_dante_ptp_pin_ready obs; }

# strih_dante_ptp_all_ready -> rc 0 iff EVERY dstack consumer on the box (the hub AND the OBS) is READY.
# The 319 bind right (the sysctl) is box-wide: the hub, an unprivileged operator process
# (DynamicUser=no + User=), gets it too, so an old hub dlink would start its follower beside dsync while
# only the obs pin moved (review round 2).
strih_dante_ptp_all_ready() { strih_dante_ptp_pin_ready intercom && strih_dante_ptp_pin_ready obs; }

# _strih_dante_ptp_below -> the one-line reason the bind right is not granted yet.
_strih_dante_ptp_below() {
  local ic ob
  ic="$(strih_dante_ptp_pin_dstack intercom)"; ob="$(strih_dante_ptp_pin_dstack obs)"
  printf "the pins' dstack (intercom %s, obs %s): one is below %s (the bind right is box-wide; a device below it has no in-process follower yet)" \
    "${ic:-unreadable}" "${ob:-unreadable}" "$STRIH_DANTE_PTP_FIRST_DSTACK"
}

# --- the port sysctl ----------------------------------------------------------------------------------

strih_dante_ptp_port_file() { printf '%s/etc/sysctl.d/60-strih-dante-ptp-port.conf\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }
strih_dante_ptp_port_proc() { printf '%s/proc/sys/net/ipv4/ip_unprivileged_port_start\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }

# strih_dante_ptp_port_text -> the sysctl drop-in.
strih_dante_ptp_port_text() {
  printf '%s\n' "$STRIH_DANTE_PTP_PORT_HEADER"
  printf '# The strih OBS dante-input (a desktop-user process under systemd --user) follows Dante PTP in process\n'
  printf '# and binds UDP %s (ROZHODNUTE 6628785864). Every unprivileged process may bind %s-1023 on this box.\n' \
    "$STRIH_DANTE_PTP_PORT" "$STRIH_DANTE_PTP_PORT"
  printf 'net.ipv4.ip_unprivileged_port_start = %s\n' "$STRIH_DANTE_PTP_PORT"
}

# _strih_dante_ptp_port_live -> the running value ('' when unreadable).
_strih_dante_ptp_port_live() { head -n 1 "$(strih_dante_ptp_port_proc)" 2>/dev/null | tr -d '[:space:]' || true; }

# strih_dante_ptp_port_install -> a Dante-program box. Every pin READY (strih_dante_ptp_all_ready): the drop-in (written only on a
# difference) + the running value set to 319 (no restart: the follower binds at OBS's next start). Below the
# threshold: the remove path (a 319 bind right for an old dlink would start its follower beside dsync).
# rc 1 + a named reason on a failure.
strih_dante_ptp_port_install() {
  local f proc owner="${STRIH_DANTE_RX_PIN_OWNER:-root:root}" out
  if ! strih_dante_ptp_all_ready; then
    echo "  dante-ptp: no 319 bind right yet: $(_strih_dante_ptp_below)"
    strih_dante_ptp_port_remove "$(_strih_dante_ptp_below)"
    return
  fi
  f="$(strih_dante_ptp_port_file)"; proc="$(strih_dante_ptp_port_proc)"
  install -d -m 0755 "$(dirname "$f")" || { echo "dante-ptp: cannot create $(dirname "$f")" >&2; return 1; }
  out="$(obs_box_write_if_changed_rendered "$f" 0644 "$owner" "dante-ptp" strih_dante_ptp_port_text)" || { printf '%s\n' "$out"; return 1; }
  printf '%s\n' "$out"
  if [ "$(_strih_dante_ptp_port_live)" != "$STRIH_DANTE_PTP_PORT" ]; then
    { printf '%s\n' "$STRIH_DANTE_PTP_PORT" > "$proc"; } 2>/dev/null \
      || { echo "dante-ptp: cannot set ${proc} to ${STRIH_DANTE_PTP_PORT} (run setup-strih as root)" >&2; return 1; }
  fi
  [ "$(_strih_dante_ptp_port_live)" = "$STRIH_DANTE_PTP_PORT" ] \
    || { echo "dante-ptp: ${proc} reads '$(_strih_dante_ptp_port_live)' after the write, not ${STRIH_DANTE_PTP_PORT}" >&2; return 1; }
  echo "  dante-ptp: unprivileged processes may bind UDP ${STRIH_DANTE_PTP_PORT} (the OBS dante-input's in-process PTP follower) -- it binds at OBS's next start"
}

# strih_dante_ptp_port_remove [WHY] -> OUR drop-in removed (header-checked; a foreign file of the name is
# left and named) and a running value of 319 put back to 1024. WHY names the reason (default: no Dante
# program). Prints only when it touched something. rc 1 named when the running value cannot be put back.
# A running 319 with no drop-in of ours is not ours to judge (nothing records who set it) and is left.
strih_dante_ptp_port_remove() {
  local f proc why="${1:-this box takes no Dante program}"
  f="$(strih_dante_ptp_port_file)"; proc="$(strih_dante_ptp_port_proc)"
  [ -e "$f" ] || return 0
  if ! head -n 1 "$f" 2>/dev/null | grep -qxF "$STRIH_DANTE_PTP_PORT_HEADER"; then
    echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2
    return 0
  fi
  rm -f "$f" || { echo "dante-ptp: cannot remove ${f}" >&2; return 1; }
  echo "  dante-ptp: removed ${f} (${why})"
  [ "$(_strih_dante_ptp_port_live)" = "$STRIH_DANTE_PTP_PORT" ] || return 0
  { printf '%s\n' "$STRIH_DANTE_PTP_PORT_DEFAULT" > "$proc"; } 2>/dev/null \
    || { echo "dante-ptp: cannot put ${proc} back to ${STRIH_DANTE_PTP_PORT_DEFAULT}" >&2; return 1; }
  echo "  dante-ptp: ${proc} back to ${STRIH_DANTE_PTP_PORT_DEFAULT}"
}

# strih_dante_ptp_port_grade -> verify item 9's `(program-dante) ptp-port` row (ok / bad / note). Below the
# threshold the right must NOT be there (a NOTE when absent, a FAIL when our drop-in is). Always rc 0.
strih_dante_ptp_port_grade() {
  local f live
  f="$(strih_dante_ptp_port_file)"; live="$(_strih_dante_ptp_port_live)"
  if ! strih_dante_ptp_all_ready; then
    if [ -e "$f" ]; then
      bad "(program-dante) ptp-port: ${f} is set but $(_strih_dante_ptp_below) -- re-run setup-strih.sh step 13 (it removes it)"
    else
      note "(program-dante) ptp-port: not set, $(_strih_dante_ptp_below)"
    fi
  elif ! [ -f "$f" ] || ! cmp -s "$f" <(strih_dante_ptp_port_text); then
    bad "(program-dante) ptp-port: ${f} missing or drifted -- the OBS dante-input cannot bind UDP ${STRIH_DANTE_PTP_PORT} for its in-process PTP follower; re-run setup-strih.sh step 13"
  elif [ "$live" != "$STRIH_DANTE_PTP_PORT" ]; then
    bad "(program-dante) ptp-port: net.ipv4.ip_unprivileged_port_start reads '${live:-unreadable}', not ${STRIH_DANTE_PTP_PORT} -- re-run setup-strih.sh step 13 (it sets the running value)"
  else
    ok "(program-dante) ptp-port: unprivileged processes may bind UDP ${STRIH_DANTE_PTP_PORT} (${f}, the running value)"
  fi
  return 0
}

# --- the media clock source ---------------------------------------------------------------------------

# strih_dante_media_clock_verdict (stdin = a device's log) -> ONE line `<kind> <from> <reason>` (pure,
# always rc 0). kind = internal | service | none (`media clock: no source ready`) | absent (no source line
# at all) | odd:<value>; the LAST source line wins. from = the `from=` of that line ('-' when none).
# reason = the `reason="..."` of the last `in-process follower did not start` warning ('-' when none; it
# may hold spaces, so it is the rest of the line). ANSI colour codes are stripped first.
strih_dante_media_clock_verdict() {
  LC_ALL=C sed -e 's/\x1b\[[0123456789;]*m//g' | LC_ALL=C awk '
    /media clock source source=/ {
      v = $0; sub(/.*media clock source source=/, "", v); split(v, a, /[ \t]/); last = a[1]
      from = "-"; if (match($0, /from=[^ \t]+/)) from = substr($0, RSTART + 5, RLENGTH - 5)
      next
    }
    /media clock: no source ready/ { last = "None"; from = "-"; next }
    /in-process follower did not start/ {
      why = "-"; if (match($0, /reason="[^"]*"/)) why = substr($0, RSTART + 8, RLENGTH - 9)
    }
    END {
      if (why == "") why = "-"
      if (from == "") from = "-"
      if (last == "") k = "absent"
      else if (last == "Internal") k = "internal"
      else if (last == "Service") k = "service"
      else if (last == "None") k = "none"
      else k = "odd:" last
      print k " " from " " why
    }'
}

# strih_dante_media_clock_grade LABEL COMPONENT (stdin = the device's log) -> ONE row through the caller's
# ok / bad / note: LABEL = the row prefix (`(program-dante)`, `(intercom-dante)`), COMPONENT = intercom |
# obs (whose pin decides READY). Always rc 0.
strih_dante_media_clock_grade() {
  local label="${1:?label required}" comp="${2:?component required}" v kind from why have pin what
  v="$(strih_dante_media_clock_verdict)"
  kind="${v%% *}"; v="${v#* }"; from="${v%% *}"; why="${v#* }"
  have="$(strih_dante_ptp_pin_dstack "$comp")"
  pin="the ${comp} pin's dstack ${have:-unreadable}"
  if [ "$kind" = internal ]; then
    ok "${label} media clock: source=Internal (dsync's in-process PTP follower on the Dante network, from=${from}; ${pin})"
    return 0
  fi
  case "$kind" in
    service) what="source=Service (the dsync service's clock, from=${from}): the in-process follower is not in use" ;;
    none) what="no media clock source ready" ;;
    absent) what="no 'media clock source' line in the device's log" ;;
    *) what="media clock source=${kind#odd:} (not a value dstack logs)" ;;
  esac
  [ "$why" = - ] || what="${what}; the follower did not start: ${why}"
  if strih_dante_ptp_pin_ready "$comp"; then
    bad "${label} media clock: ${what} -- ${pin} follows Dante PTP in process (ROZHODNUTE 6628785864): check UDP 319/320 on the leg, the 319 bind right and the device's own log"
  else
    note "${label} media clock: ${what} -- ${pin} is below ${STRIH_DANTE_PTP_FIRST_DSTACK} (no in-process follower yet), not graded"
  fi
  return 0
}
