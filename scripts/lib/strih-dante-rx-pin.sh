#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-rx-pin.sh -- issue 1485: the Dante receive threads of the strih OBS on their
# own CPU. The obs release's `dante_input` source runs dstack's dlink inside OBS; its pump thread
# `dstack-engine` wakes every 0.33 ms and `dstack-rx` reads the network. With OBS at load ~10 on 16 CPUs
# and every thread SCHED_OTHER nice 0, blocks finished up to 6.7 ms late against a 5 ms margin and audio
# was skipped (underruns, rebootstraps). The obs project's ask (their side is v0.4.0): take one CPU out
# of OBS's affinity and put those two threads on it at nice -10. No realtime priority (rtprio stays off
# on every OBS box, obs-box-baseline.md), no buffer or latency change.
#
# Only a box that takes its program from the Dante device (strih_program_dante_declared) gets it; every
# other box gets nothing, and a leftover of ours is removed.
#
# The reserved CPU is DERIVED: the highest CPU that is online AND in OBS's mask
# (/etc/strih-isolated-cpus.conf, obs_box_cpu_affinity), never a literal. strih-snv: OBS 2-15 -> CPU 15,
# OBS 2-14. The mask file itself stays the full block (the background slice and the baseline grader read
# it); the launcher takes the reduced mask from this lib's file.
#
# The nice -10 is set by a ROOT helper (/usr/local/sbin/strih-dante-rx-pin) that the desktop user may run
# through one sudoers line with NO arguments -- the obs-box-background-run pattern (issue 1387). The
# launcher (strih-obs-start.sh) runs it in the background after every OBS launch, so it survives every
# OBS restart. A limits.d nice grant was rejected: it is a session-wide rlimit, like the retired
# 95-<box>-genlock-rtprio.conf grant, and would let any thread of any process of the user lower its nice.
#
#   strih_dante_rx_pin_plan ONLINE OBS        -> `<reserved cpu> <obs mask without it>` (pure)
#   strih_dante_rx_pin_install USER           setup-strih step 13 via strih_dante_input_install (root)
#   strih_dante_rx_pin_remove                 the same step on any other box (root; silent when absent)
#   strih_dante_rx_pin_verdict RES MASK ROWS  -> `ok|bad <text>` lines (pure)
#   strih_dante_rx_pin_grade                  verify-strih item 9's `(program-dante) rx-pin` rows
#
# The helper reads /proc itself and has no env seam (a root program never takes a path from its caller);
# its tests rewrite its PATH= / PROC= / BUDGET_S= lines.
# Seams: STRIH_DANTE_RX_PIN_ROOT (a prefix for every /etc and /usr path), STRIH_DANTE_RX_PIN_OWNER
# (root:root), OBS_BOX_ONLINE_CPUS (the online list file), STRIH_DANTE_RX_PIN_SYSFS (the cpu sysfs dir),
# STRIH_DANTE_RX_PIN_PROC (/proc, the grade's reads).

# shellcheck source=scripts/lib/obs-box-background.sh
. "$(dirname "${BASH_SOURCE[0]}")/obs-box-background.sh"   # obs_box_cpulist_expand / _compress

STRIH_DANTE_RX_PIN_THREADS="dstack-engine dstack-rx"
STRIH_DANTE_RX_PIN_NICE=-10

strih_dante_rx_pin_conf_path() { printf '%s/etc/strih-dante-rx-cpu.conf\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }
strih_dante_rx_pin_helper_path() { printf '%s/usr/local/sbin/strih-dante-rx-pin\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }
strih_dante_rx_pin_sudoers_path() { printf '%s/etc/sudoers.d/strih-dante-rx-pin\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }

# strih_dante_rx_pin_plan ONLINE OBS -> `<cpu> <rest>`: the highest CPU online and in OBS, and OBS's mask
# without it (compressed). rc 1 when a list is unreadable, no OBS CPU is online, or fewer than two OBS
# CPUs would remain.
strih_dante_rx_pin_plan() {
  local online obs c cpu="" rest="" n=0
  online="$(obs_box_cpulist_expand "${1-}")" || return 1
  obs="$(obs_box_cpulist_expand "${2-}")" || return 1
  for c in $obs; do
    case " $online " in *" $c "*) cpu="$c" ;; esac
  done
  [ -n "$cpu" ] || return 1
  for c in $obs; do
    [ "$c" = "$cpu" ] || { rest="${rest}${rest:+ }${c}"; n=$((n + 1)); }
  done
  [ "$n" -ge 2 ] || return 1
  printf '%s %s\n' "$cpu" "$(obs_box_cpulist_compress "$rest")"
}

# strih_dante_rx_pin_conf_text CPU REST BASE SIBLINGS -> the conf file. `base` = the OBS mask it was
# derived from: the launcher uses `obs` only while /etc/strih-isolated-cpus.conf still reads `base`.
strih_dante_rx_pin_conf_text() {
  printf '# issue 1485: written by setup-strih step 13 on a Dante-program box; every run rewrites it.\n'
  printf '# reserved = the CPU of the dstack-engine + dstack-rx threads (nice %s); obs = OBS'"'"'s mask without it.\n' "$STRIH_DANTE_RX_PIN_NICE"
  printf 'reserved=%s\nobs=%s\nbase=%s\nsmt_siblings=%s\n' "${1:?cpu}" "${2:?rest}" "${3:?base}" "${4:-unread}"
}

# strih_dante_rx_pin_helper_text -> /usr/local/sbin/strih-dante-rx-pin.
strih_dante_rx_pin_helper_text() {
  cat <<'EOF'
#!/bin/bash
# strih-dante-rx-pin -- issue 1485: put the strih OBS's dstack-engine + dstack-rx threads on the reserved
# CPU (/etc/strih-dante-rx-cpu.conf) at nice -10. Run by strih-obs-start.sh through sudo after every OBS
# launch. Takes no argument: the OBS is the newest `obs` of the sudo caller. Touches only threads of that
# process whose comm is one of the two names; never another thread, never a realtime policy. Waits for
# the threads (they start when OBS loads the module) for a bounded time. Idempotent.
set -euo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
CONF=/etc/strih-dante-rx-cpu.conf
PROC=/proc
BUDGET_S=120
THREADS="dstack-engine dstack-rx"
NICE=-10
log() {
    echo "strih-dante-rx-pin: $*"
    logger -t strih-dante-rx-pin -- "$*" 2>/dev/null || true
}
if [ "$#" != 0 ]; then
    echo "usage: strih-dante-rx-pin (no arguments)" >&2
    exit 2
fi
if [ "$(id -u)" != 0 ]; then
    echo "strih-dante-rx-pin: run it through sudo" >&2
    exit 2
fi
case "${SUDO_UID:-}" in
    '' | 0 | *[!0123456789]*)
        echo "strih-dante-rx-pin: no non-root sudo caller (SUDO_UID='${SUDO_UID:-}')" >&2
        exit 2
        ;;
esac
CPU="$(sed -n 's/^reserved=//p' "$CONF" 2>/dev/null | head -n 1 || true)"
case "$CPU" in
    '' | *[!0123456789]*)
        log "FAIL: no reserved CPU in ${CONF} -- re-run setup-strih.sh step 13"
        exit 1
        ;;
esac
# pin_once PID -> rc 0 when every named thread of PID runs on CPU at NICE (read back), printing the
# `name(tid)` list; rc 1 when a name has no thread yet or a read-back differs.
pin_once() {
    local pid="$1" t tid comm name list="" cpus nice stat
    for name in $THREADS; do
        local seen=0
        for t in "$PROC/$pid"/task/*; do
            [ -r "$t/comm" ] || continue
            comm="$(cat "$t/comm" 2>/dev/null || true)"
            [ "$comm" = "$name" ] || continue
            tid="${t##*/}"
            taskset -p -c "$CPU" "$tid" >/dev/null 2>&1 || return 1
            renice --priority "$NICE" -p "$tid" >/dev/null 2>&1 || return 1
            cpus="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "$t/status" 2>/dev/null || true)"
            stat="$(cat "$t/stat" 2>/dev/null || true)"
            stat="${stat##*) }"
            # shellcheck disable=SC2086  # split the stat fields on purpose
            set -- $stat
            nice="${17:-}"
            [ "$cpus" = "$CPU" ] && [ "$nice" = "$NICE" ] || return 1
            list="${list}${list:+, }${name} tid ${tid}"
            seen=1
        done
        [ "$seen" = 1 ] || return 1
    done
    printf '%s\n' "$list"
}
pid=""
for ((i = 0; i <= BUDGET_S; i++)); do
    pid="$(pgrep -n -x -u "$SUDO_UID" obs 2>/dev/null || true)"
    if [ -n "$pid" ] && done_list="$(pin_once "$pid")"; then
        log "obs pid ${pid}: ${done_list} -> CPU ${CPU} nice ${NICE}"
        exit 0
    fi
    [ "$i" -lt "$BUDGET_S" ] && sleep 1
done
log "FAIL: after ${BUDGET_S} s the ${THREADS// / + } threads of obs (pid ${pid:-none}) are not all on CPU ${CPU} at nice ${NICE} -- the dante_input module did not start them, or the move was refused"
exit 1
EOF
}

# strih_dante_rx_pin_sudoers_text USER -> the sudoers line: the helper alone, no arguments ("").
strih_dante_rx_pin_sudoers_text() {
  printf '# issue 1485: the strih OBS launcher pins the Dante receive threads through this root helper.\n'
  printf '%s ALL=(root) NOPASSWD: /usr/local/sbin/strih-dante-rx-pin ""\n' "${1:?strih_dante_rx_pin_sudoers_text: USER required}"
}

# strih_dante_rx_pin_install USER -> the conf, the helper and the sudoers line (root). rc 1 + a named
# reason when the plan cannot be derived or the sudoers line does not parse. Nothing is restarted: OBS
# takes the reduced mask and the pin at its next start (the unit restarts OBS through the launcher).
strih_dante_rx_pin_install() {
  local user="${1:?strih_dante_rx_pin_install: USER required}" owner="${STRIH_DANTE_RX_PIN_OWNER:-root:root}"
  local root="${STRIH_DANTE_RX_PIN_ROOT:-}" online base plan cpu rest sib tmp
  online="$(head -n 1 "${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}" 2>/dev/null || true)"
  base="$(head -n 1 "${root}/etc/strih-isolated-cpus.conf" 2>/dev/null || true)"
  plan="$(strih_dante_rx_pin_plan "$online" "$base")" || {
    echo "dante-rx-pin: no CPU to reserve (online '${online}', OBS mask '${base}' from ${root}/etc/strih-isolated-cpus.conf) -- step 11 writes the mask" >&2
    return 1
  }
  read -r cpu rest <<<"$plan"
  sib="$(cat "${STRIH_DANTE_RX_PIN_SYSFS:-/sys/devices/system/cpu}/cpu${cpu}/topology/thread_siblings_list" 2>/dev/null || true)"
  tmp="$(mktemp)" || return 1
  strih_dante_rx_pin_sudoers_text "$user" >"$tmp"
  if ! visudo -cf "$tmp" >/dev/null; then
    rm -f "$tmp"
    echo "dante-rx-pin: the rendered sudoers line does not parse" >&2
    return 1
  fi
  rm -f "$tmp"
  install -d -m 0755 "$(dirname "$(strih_dante_rx_pin_helper_path)")" "$(dirname "$(strih_dante_rx_pin_sudoers_path)")" || return 1
  strih_dante_rx_pin_conf_text "$cpu" "$rest" "$base" "${sib:-unread}" \
    | obs_box_write_if_changed "$(strih_dante_rx_pin_conf_path)" 0644 "$owner" "dante-rx-pin conf" || return 1
  strih_dante_rx_pin_helper_text | obs_box_write_if_changed "$(strih_dante_rx_pin_helper_path)" 0755 "$owner" "dante-rx-pin helper" || return 1
  strih_dante_rx_pin_sudoers_text "$user" | obs_box_write_if_changed "$(strih_dante_rx_pin_sudoers_path)" 0440 "$owner" "dante-rx-pin sudoers" || return 1
  echo "  dante-rx-pin: CPU ${cpu} reserved for ${STRIH_DANTE_RX_PIN_THREADS// / + } at nice ${STRIH_DANTE_RX_PIN_NICE}; OBS on ${rest} (from ${base}); SMT siblings of CPU ${cpu}: ${sib:-unread} (the sibling stays OBS's) -- applied at OBS's next start"
}

# strih_dante_rx_pin_remove -> remove our conf, helper and sudoers line (a box without the Dante program).
# Prints only when it removed something, so a box that never had them stays quiet.
strih_dante_rx_pin_remove() {
  local f
  for f in "$(strih_dante_rx_pin_conf_path)" "$(strih_dante_rx_pin_helper_path)" "$(strih_dante_rx_pin_sudoers_path)"; do
    if [ -e "$f" ]; then
      rm -f "$f" && echo "  dante-rx-pin: removed ${f} (this box takes no Dante program)"
    fi
  done
  return 0
}

# strih_dante_rx_pin_verdict RESERVED OBS_MASK ROWS -> `ok <text>` / `bad <text>` lines (pure). ROWS:
# one `<comm> <tid> <cpus_allowed_list> <nice>` line per thread named in STRIH_DANTE_RX_PIN_THREADS.
strih_dante_rx_pin_verdict() {
  local res="${1-}" mask="${2-}" rows="${3-}" name comm tid cpus nice found m
  case "$res" in
    '' | *[!0123456789]*) echo "bad rx-pin: no reserved CPU in $(strih_dante_rx_pin_conf_path) -- re-run setup-strih.sh step 13"; return 0 ;;
  esac
  if [ -z "$mask" ]; then
    echo "bad rx-pin: no running OBS (or its mask is unreadable) -- the threads cannot be graded"
    return 0
  fi
  if ! m="$(obs_box_cpulist_expand "$mask" 2>/dev/null)"; then
    echo "bad rx-pin: OBS's mask '${mask}' is unreadable"
  elif case " $m " in *" $res "*) true ;; *) false ;; esac; then
    echo "bad rx-pin: OBS's main affinity ${mask} still includes the reserved CPU ${res} -- restart OBS through strih-obs.service (the launcher takes the reduced mask)"
  else
    echo "ok rx-pin: OBS's main affinity ${mask} leaves CPU ${res} to the Dante threads"
  fi
  for name in $STRIH_DANTE_RX_PIN_THREADS; do
    found=0
    while read -r comm tid cpus nice; do
      [ "$comm" = "$name" ] || continue
      found=1
      if [ "$cpus" != "$res" ]; then
        echo "bad rx-pin: ${name} tid ${tid} runs on ${cpus:-unread}, not the reserved CPU ${res}"
      elif ! [[ "$nice" =~ ^-?[0123456789]+$ ]] || [ "$nice" -gt "$STRIH_DANTE_RX_PIN_NICE" ]; then
        echo "bad rx-pin: ${name} tid ${tid} has nice ${nice:-unread}, wants ${STRIH_DANTE_RX_PIN_NICE} or lower"
      else
        echo "ok rx-pin: ${name} tid ${tid} on CPU ${cpus} at nice ${nice}"
      fi
    done <<<"$rows"
    [ "$found" = 1 ] || echo "bad rx-pin: no ${name} thread in the running OBS -- the dante_input device did not start, or OBS runs without it"
  done
  return 0
}

# strih_dante_rx_pin_grade -> verify-strih item 9's `(program-dante) rx-pin` rows (through the caller's
# ok / bad). Read-only, runs as the desktop user. Always rc 0.
strih_dante_rx_pin_grade() {
  local proc="${STRIH_DANTE_RX_PIN_PROC:-/proc}" res pid mask="" rows="" t comm stat line v
  res="$(sed -n 's/^reserved=//p' "$(strih_dante_rx_pin_conf_path)" 2>/dev/null | head -n 1 || true)"
  pid="$(pgrep -n -x obs 2>/dev/null || true)"
  if [ -n "$pid" ]; then
    mask="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "${proc}/${pid}/status" 2>/dev/null || true)"
    for t in "${proc}/${pid}"/task/*; do
      comm="$(cat "$t/comm" 2>/dev/null || true)"
      case " ${STRIH_DANTE_RX_PIN_THREADS} " in *" ${comm} "*) ;; *) continue ;; esac
      stat="$(cat "$t/stat" 2>/dev/null || true)"
      stat="${stat##*) }"
      # shellcheck disable=SC2086  # split the stat fields on purpose
      v="$(set -- $stat; printf '%s' "${17:-}")"
      line="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "$t/status" 2>/dev/null || true)"
      rows="${rows}${comm} ${t##*/} ${line:-unread} ${v:-unread}"$'\n'
    done
  fi
  while IFS= read -r line; do
    case "$line" in
      ok\ *) ok "(program-dante) ${line#ok }" ;;
      bad\ *) bad "(program-dante) ${line#bad }" ;;
    esac
  done < <(strih_dante_rx_pin_verdict "$res" "$mask" "$rows")
  return 0
}
