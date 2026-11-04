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
#   strih_dante_rx_pin_plan ONLINE OBS [HUB]  -> `<reserved cpu> <obs mask without it>[ <hub cpu>]` (pure)
#   strih_dante_rx_pin_install USER [HUB]     setup-strih step 13 via strih_dante_input_install (root)
#   strih_dante_rx_pin_remove                 the same step on any other box (root; silent when absent)
#   strih_dante_rx_pin_verdict RES MASK ROWS [NAME] -> `ok|bad <text>` lines (pure)
#   strih_dante_rx_pin_grade [HUB]            verify-strih item 9's `(program-dante)` + `(intercom-dante)` rx-pin rows
#
# The intercom hub's own Dante device (4.11.2026, comment 6634395906): its dstack-engine is a second
# 0.33 ms pump, and two engines on one CPU collide (OBS 25 pump WARNs in 3 min with the hub's threads on
# CPU 15 beside OBS's), so each needs its OWN CPU. On a box whose facts declare a hub Dante device
# beside the Dante program (HUB=1, strih_dante_hub_declared) the plan takes the next CPU down for the
# hub: strih-snv OBS 15, the hub 14, OBS on 2-13; the conf gains `hub=`. The same helper places the
# hub's two threads (`strih-dante-rx-pin hub`, as root, the hub's MainPID from systemd) from a hub
# drop-in's `ExecStartPost=-+`, so every hub start (a crash restart too) re-pins them; the threads
# appear ~0.2 s after the hub's start (measured 4.11.2026), the budget is 30 s against the unit's 90 s
# start timeout, and `-` keeps a failed pin from failing the hub. The drop-in sets ONLY ExecStartPost:
# the released unit's capability lines stay the hub's only grant (strih-dante-ptp.sh). A HUB=0 box is
# byte-identical to before (no `hub=`, no drop-in; a drop-in of ours left over is removed).
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
strih_dante_rx_pin_hub_dropin_path() { printf '%s/etc/systemd/system/intercom-hub.service.d/60-strih-dante-rx-pin.conf\n' "${STRIH_DANTE_RX_PIN_ROOT:-}"; }

STRIH_DANTE_RX_PIN_HUB_HEADER="# issue 1485: written by camera-box setup-strih step 13 on a box whose hub Dante device has its own CPU; every run rewrites it."

# strih_dante_reserved_cpu -> the conf's `reserved=` CPU when it is a number, else nothing. Always rc 0.
# The one reader for every grade and install (strih-dante-reserved-cpu.sh, strih-program-audio.sh); the
# root helper keeps its own copy (it runs standalone and takes no path from its caller).
strih_dante_reserved_cpu() {
  local v
  v="$(sed -n 's/^reserved=//p' "$(strih_dante_rx_pin_conf_path)" 2>/dev/null | head -n 1 || true)"
  case "$v" in '' | *[!0123456789]*) v="" ;; esac
  printf '%s' "$v"
}

# strih_dante_reserved_hub_cpu -> the conf's `hub=` CPU (the intercom hub's dstack threads) when it is a
# number, else nothing (a box with one reserved CPU). Always rc 0.
strih_dante_reserved_hub_cpu() {
  local v
  v="$(sed -n 's/^hub=//p' "$(strih_dante_rx_pin_conf_path)" 2>/dev/null | head -n 1 || true)"
  case "$v" in '' | *[!0123456789]*) v="" ;; esac
  printf '%s' "$v"
}

# strih_dante_reserved_cpus -> every reserved CPU of the conf, space-separated: OBS's, then the hub's
# (`15 14` on strih-snv); nothing without a conf. Always rc 0.
strih_dante_reserved_cpus() {
  local res hub
  res="$(strih_dante_reserved_cpu)"
  hub="$(strih_dante_reserved_hub_cpu)"
  [ -n "$res" ] || hub=""
  printf '%s' "${res}${hub:+ ${hub}}"
}

# strih_dante_rx_pin_plan ONLINE OBS [HUB] -> `<cpu> <rest>`: the highest CPU online and in OBS, and OBS's
# mask without it (compressed). HUB=1 (the hub's Dante device has its own CPU): `<cpu> <rest> <hub>`, the
# hub's = the next CPU down that is online and in OBS, `rest` without both. rc 1 when a list is
# unreadable, a CPU to reserve is missing, fewer than two OBS CPUs would remain, or HUB is not 0 / 1.
strih_dante_rx_pin_plan() {
  local online obs c cpu="" hub="" rest="" n=0 want="${3:-0}"
  case "$want" in 0 | 1) ;; *) return 1 ;; esac
  online="$(obs_box_cpulist_expand "${1-}")" || return 1
  obs="$(obs_box_cpulist_expand "${2-}")" || return 1
  for c in $obs; do
    case " $online " in *" $c "*) hub="$cpu"; cpu="$c" ;; esac
  done
  [ -n "$cpu" ] || return 1
  [ "$want" = 1 ] || hub=""
  [ "$want" = 0 ] || [ -n "$hub" ] || return 1
  for c in $obs; do
    [ "$c" = "$cpu" ] || [ "$c" = "$hub" ] || { rest="${rest}${rest:+ }${c}"; n=$((n + 1)); }
  done
  [ "$n" -ge 2 ] || return 1
  printf '%s %s%s\n' "$cpu" "$(obs_box_cpulist_compress "$rest")" "${hub:+ ${hub}}"
}

# strih_dante_rx_pin_conf_text CPU REST BASE SIBLINGS [HUB] -> the conf file. `base` = the OBS mask it was
# derived from: the launcher uses `obs` only while /etc/strih-isolated-cpus.conf still reads `base`.
# `hub=` only when the hub's Dante threads have their own CPU (a HUB-less conf is byte-identical to before).
strih_dante_rx_pin_conf_text() {
  printf '# issue 1485: written by setup-strih step 13 on a Dante-program box; every run rewrites it.\n'
  printf '# reserved = the CPU of the dstack-engine + dstack-rx threads (nice %s); obs = OBS'"'"'s mask without it.\n' "$STRIH_DANTE_RX_PIN_NICE"
  [ -z "${5:-}" ] || printf '# hub = the CPU of the intercom hub'"'"'s own dstack-engine + dstack-rx threads (nice %s).\n' "$STRIH_DANTE_RX_PIN_NICE"
  printf 'reserved=%s\nobs=%s\nbase=%s\nsmt_siblings=%s\n' "${1:?cpu}" "${2:?rest}" "${3:?base}" "${4:-unread}"
  [ -z "${5:-}" ] || printf 'hub=%s\n' "$5"
}

# strih_dante_rx_pin_hub_dropin_text -> the intercom hub drop-in: after every hub start the root helper
# places the hub's two Dante threads. ExecStartPost only (never a capability or SecureBits key: the
# released unit's grant stays the only one, issue 1485 ROZHODNUTE 6628785864).
strih_dante_rx_pin_hub_dropin_text() {
  printf '%s\n' "$STRIH_DANTE_RX_PIN_HUB_HEADER"
  printf '# After every hub start (a crash restart too) the root helper puts the hub'"'"'s dstack-engine + dstack-rx\n'
  printf '# threads on the hub CPU of /etc/strih-dante-rx-cpu.conf at nice %s. `+` = as root, outside the unit'"'"'s\n' "$STRIH_DANTE_RX_PIN_NICE"
  printf '# User= and sandbox; `-` = a failed pin never fails the hub (the helper logs it). Do NOT edit by hand.\n'
  printf '[Service]\nExecStartPost=-+/usr/local/sbin/strih-dante-rx-pin hub\n'
}

# strih_dante_rx_pin_helper_text -> /usr/local/sbin/strih-dante-rx-pin.
strih_dante_rx_pin_helper_text() {
  cat <<'EOF'
#!/bin/bash
# strih-dante-rx-pin -- issue 1485: put a process's dstack-engine + dstack-rx threads on their reserved
# CPU (/etc/strih-dante-rx-cpu.conf) at nice -10.
#   (no argument)  the strih OBS: run by strih-obs-start.sh through sudo after every OBS launch; the
#                  newest `obs` of the sudo caller, CPU `reserved=`.
#   hub            the intercom hub: run as root by intercom-hub.service's ExecStartPost (a camera-box
#                  drop-in) after every hub start; the unit's MainPID, CPU `hub=`.
# Touches only threads of that process whose comm is one of the two names; never another thread, never
# a realtime policy. Waits for the threads (they start when the Dante device starts) for a bounded
# time. Idempotent.
set -euo pipefail
PATH=/usr/sbin:/usr/bin:/sbin:/bin
CONF=/etc/strih-dante-rx-cpu.conf
PROC=/proc
BUDGET_S=120
HUB_BUDGET_S=30
THREADS="dstack-engine dstack-rx"
NICE=-10
log() {
    echo "strih-dante-rx-pin: $*"
    logger -t strih-dante-rx-pin -- "$*" 2>/dev/null || true
}
MODE=obs
if [ "$#" = 1 ] && [ "$1" = hub ]; then
    MODE=hub
elif [ "$#" != 0 ]; then
    echo "usage: strih-dante-rx-pin [hub]" >&2
    exit 2
fi
if [ "$(id -u)" != 0 ]; then
    echo "strih-dante-rx-pin: run it as root (OBS: through sudo; the hub: its unit's ExecStartPost)" >&2
    exit 2
fi
if [ "$MODE" = obs ]; then
    case "${SUDO_UID:-}" in
        '' | 0 | *[!0123456789]*)
            echo "strih-dante-rx-pin: no non-root sudo caller (SUDO_UID='${SUDO_UID:-}')" >&2
            exit 2
            ;;
    esac
    KEY=reserved
    NAME=obs
else
    KEY=hub
    NAME=intercom-hub
    BUDGET_S="$HUB_BUDGET_S"
fi
CPU="$(sed -n "s/^${KEY}=//p" "$CONF" 2>/dev/null | head -n 1 || true)"
case "$CPU" in
    '' | *[!0123456789]*)
        log "FAIL: no ${KEY} CPU in ${CONF} -- re-run setup-strih.sh step 13"
        exit 1
        ;;
esac
# find_pid -> the process to pin: the newest `obs` of the sudo caller, or the hub unit's main PID.
find_pid() {
    local p
    if [ "$MODE" = obs ]; then
        pgrep -n -x -u "$SUDO_UID" obs 2>/dev/null || true
    else
        # systemd exports MAINPID to ExecStartPost; a hand run as root asks systemd for it
        p="${MAINPID:-}"
        case "$p" in '' | 0 | *[!0123456789]*) p="$(systemctl show -p MainPID --value intercom-hub 2>/dev/null || true)" ;; esac
        case "$p" in '' | 0 | *[!0123456789]*) ;; *) printf '%s\n' "$p" ;; esac
    fi
}
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
    pid="$(find_pid)"
    if [ -n "$pid" ] && done_list="$(pin_once "$pid")"; then
        log "${NAME} pid ${pid}: ${done_list} -> CPU ${CPU} nice ${NICE}"
        exit 0
    fi
    [ "$i" -lt "$BUDGET_S" ] && sleep 1
done
log "FAIL: after ${BUDGET_S} s the ${THREADS// / + } threads of ${NAME} (pid ${pid:-none}) are not all on CPU ${CPU} at nice ${NICE} -- its Dante device did not start them, or the move was refused"
exit 1
EOF
}

# strih_dante_rx_pin_sudoers_text USER -> the sudoers line: the helper alone, no arguments ("").
strih_dante_rx_pin_sudoers_text() {
  printf '# issue 1485: the strih OBS launcher pins the Dante receive threads through this root helper.\n'
  printf '%s ALL=(root) NOPASSWD: /usr/local/sbin/strih-dante-rx-pin ""\n' "${1:?strih_dante_rx_pin_sudoers_text: USER required}"
}

# strih_dante_rx_pin_install USER [HUB] -> the conf, the helper and the sudoers line (root); HUB=1 (the
# hub's Dante device has its own CPU, strih_dante_hub_declared) also the hub drop-in, HUB=0 removes a
# drop-in of ours. rc 1 + a named reason when the plan cannot be derived, the sudoers line does not parse
# or the reload after a drop-in change fails. Nothing is restarted: OBS takes the reduced mask and the
# pin at its next start (the unit restarts OBS through the launcher), the hub at its next start.
strih_dante_rx_pin_install() {
  local user="${1:?strih_dante_rx_pin_install: USER required}" hubwant="${2:-0}" owner="${STRIH_DANTE_RX_PIN_OWNER:-root:root}"
  local root="${STRIH_DANTE_RX_PIN_ROOT:-}" online base plan cpu rest hub sib tmp
  online="$(head -n 1 "${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}" 2>/dev/null || true)"
  base="$(head -n 1 "${root}/etc/strih-isolated-cpus.conf" 2>/dev/null || true)"
  plan="$(strih_dante_rx_pin_plan "$online" "$base" "$hubwant")" || {
    echo "dante-rx-pin: no CPU to reserve (online '${online}', OBS mask '${base}' from ${root}/etc/strih-isolated-cpus.conf, hub CPU wanted: ${hubwant}) -- step 11 writes the mask" >&2
    return 1
  }
  read -r cpu rest hub <<<"$plan"
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
  obs_box_write_if_changed_rendered "$(strih_dante_rx_pin_conf_path)" 0644 "$owner" "dante-rx-pin conf" \
    strih_dante_rx_pin_conf_text "$cpu" "$rest" "$base" "${sib:-unread}" "$hub" || return 1
  obs_box_write_if_changed_rendered "$(strih_dante_rx_pin_helper_path)" 0755 "$owner" "dante-rx-pin helper" strih_dante_rx_pin_helper_text || return 1
  obs_box_write_if_changed_rendered "$(strih_dante_rx_pin_sudoers_path)" 0440 "$owner" "dante-rx-pin sudoers" strih_dante_rx_pin_sudoers_text "$user" || return 1
  if [ -n "$hub" ]; then
    echo "  dante-rx-pin: CPU ${cpu} reserved for ${STRIH_DANTE_RX_PIN_THREADS// / + } at nice ${STRIH_DANTE_RX_PIN_NICE}; OBS on ${rest} (from ${base}); SMT siblings of CPU ${cpu}: ${sib:-unread} (CPU ${hub} is the hub's own) -- applied at OBS's next start"
    _strih_dante_rx_pin_hub_dropin_install "$owner" || return 1
    echo "  dante-rx-pin: CPU ${hub} reserved for the intercom hub's ${STRIH_DANTE_RX_PIN_THREADS// / + } at nice ${STRIH_DANTE_RX_PIN_NICE} (its own Dante device; ExecStartPost of $(strih_dante_rx_pin_hub_dropin_path)) -- applied at the hub's next start: restart intercom-hub.service (setup never restarts a running hub)"
  else
    echo "  dante-rx-pin: CPU ${cpu} reserved for ${STRIH_DANTE_RX_PIN_THREADS// / + } at nice ${STRIH_DANTE_RX_PIN_NICE}; OBS on ${rest} (from ${base}); SMT siblings of CPU ${cpu}: ${sib:-unread} (the sibling stays OBS's) -- applied at OBS's next start"
    _strih_dante_rx_pin_hub_dropin_remove || return 1
  fi
}

# _strih_dante_rx_pin_hub_dropin_install OWNER -> the hub drop-in, written on a difference; a written one is
# followed by a daemon-reload (bounded) so the next hub start runs it. rc 1 named on a failure.
_strih_dante_rx_pin_hub_dropin_install() {
  local f out
  f="$(strih_dante_rx_pin_hub_dropin_path)"
  install -d -m 0755 "$(dirname "$f")" || { echo "dante-rx-pin: cannot create $(dirname "$f")" >&2; return 1; }
  out="$(obs_box_write_if_changed_rendered "$f" 0644 "${1:?owner}" "dante-rx-pin hub drop-in" strih_dante_rx_pin_hub_dropin_text)" \
    || { printf '%s\n' "$out"; return 1; }
  printf '%s\n' "$out"
  case "$out" in *" written") _strih_dante_rx_pin_reload ;; esac
}

# _strih_dante_rx_pin_hub_dropin_remove -> remove OUR hub drop-in (a header-less file of that name is
# left, named), then reload. Prints only when it touched something. rc 1 when the reload fails.
_strih_dante_rx_pin_hub_dropin_remove() {
  local f
  f="$(strih_dante_rx_pin_hub_dropin_path)"
  [ -e "$f" ] || return 0
  if ! head -n 1 "$f" 2>/dev/null | grep -qxF "$STRIH_DANTE_RX_PIN_HUB_HEADER"; then
    echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2
    return 0
  fi
  rm -f "$f" || { echo "dante-rx-pin: cannot remove ${f}" >&2; return 1; }
  echo "  dante-rx-pin: removed ${f} (the hub's Dante threads have no CPU of their own here)"
  _strih_dante_rx_pin_reload
}

_strih_dante_rx_pin_reload() {
  timeout 30 systemctl daemon-reload \
    || { echo "dante-rx-pin: systemctl daemon-reload failed or timed out after the hub drop-in changed" >&2; return 1; }
}

# strih_dante_rx_pin_remove -> remove our conf, helper, sudoers line and hub drop-in (a box without the
# Dante program). Prints only when it removed something, so a box that never had them stays quiet.
strih_dante_rx_pin_remove() {
  local f
  for f in "$(strih_dante_rx_pin_conf_path)" "$(strih_dante_rx_pin_helper_path)" "$(strih_dante_rx_pin_sudoers_path)"; do
    if [ -e "$f" ]; then
      rm -f "$f" && echo "  dante-rx-pin: removed ${f} (this box takes no Dante program)"
    fi
  done
  _strih_dante_rx_pin_hub_dropin_remove
}


# strih_dante_rx_pin_verdict RESERVED MASK ROWS [NAME] -> `ok <text>` / `bad <text>` lines (pure). ROWS:
# one `<comm> <tid> <cpus_allowed_list> <nice>` line per thread named in STRIH_DANTE_RX_PIN_THREADS. NAME
# = obs (default: the strih OBS, MASK its main affinity) or hub (the intercom hub, RESERVED the conf's
# `hub=`, MASK the hub's main affinity).
strih_dante_rx_pin_verdict() {
  local res="${1-}" mask="${2-}" rows="${3-}" who="${4:-obs}" name comm tid cpus nice found m key proc label cure
  if [ "$who" = hub ]; then
    key=hub proc="intercom hub" label="the hub's"
    cure="restart intercom-hub.service (the managers' default keeps it off the reserved CPUs; its ExecStartPost moves the two threads)"
  else
    key=reserved proc=OBS label="OBS's"
    cure="restart OBS through strih-obs.service (the launcher takes the reduced mask)"
  fi
  case "$res" in
    '' | *[!0123456789]*) echo "bad rx-pin: no ${key} CPU in $(strih_dante_rx_pin_conf_path) -- re-run setup-strih.sh step 13"; return 0 ;;
  esac
  if [ -z "$mask" ]; then
    echo "bad rx-pin: no running ${proc} (or its mask is unreadable) -- the threads cannot be graded"
    return 0
  fi
  if ! m="$(obs_box_cpulist_expand "$mask" 2>/dev/null)"; then
    echo "bad rx-pin: ${label} mask '${mask}' is unreadable"
  elif case " $m " in *" $res "*) true ;; *) false ;; esac; then
    echo "bad rx-pin: ${label} main affinity ${mask} still includes the reserved CPU ${res} -- ${cure}"
  else
    echo "ok rx-pin: ${label} main affinity ${mask} leaves CPU ${res} to the Dante threads"
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
    if [ "$found" = 0 ]; then
      if [ "$who" = hub ]; then
        echo "bad rx-pin: no ${name} thread in the running intercom hub -- its Dante device did not start (verify item 16's (intercom-dante) rows)"
      else
        echo "bad rx-pin: no ${name} thread in the running OBS -- the dante_input device did not start, or OBS runs without it"
      fi
    fi
  done
  return 0
}

# _strih_dante_rx_pin_rows PROC PID -> `<mask>` on the first line, then one `<comm> <tid> <cpus> <nice>`
# line per thread of PID named in STRIH_DANTE_RX_PIN_THREADS. Nothing for an empty PID. Always rc 0.
_strih_dante_rx_pin_rows() {
  local proc="$1" pid="${2-}" t comm stat line v
  [ -n "$pid" ] || return 0
  sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "${proc}/${pid}/status" 2>/dev/null | head -n 1 || true
  for t in "${proc}/${pid}"/task/*; do
    comm="$(cat "$t/comm" 2>/dev/null || true)"
    case " ${STRIH_DANTE_RX_PIN_THREADS} " in *" ${comm} "*) ;; *) continue ;; esac
    stat="$(cat "$t/stat" 2>/dev/null || true)"
    stat="${stat##*) }"
    # shellcheck disable=SC2086  # split the stat fields on purpose
    v="$(set -- $stat; printf '%s' "${17:-}")"
    line="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "$t/status" 2>/dev/null || true)"
    printf '%s %s %s %s\n' "$comm" "${t##*/}" "${line:-unread}" "${v:-unread}"
  done
  return 0
}

# _strih_dante_rx_pin_emit LABEL [PENDING] -> the verdict lines on stdin through the caller's ok / bad;
# PENDING=1 turns each bad line into a `restart pending` note (the hub has not run its drop-in yet).
_strih_dante_rx_pin_emit() {
  local line
  while IFS= read -r line; do
    case "$line" in
      ok\ *) ok "${1} ${line#ok }" ;;
      bad\ *)
        if [ "${2:-0}" = 1 ]; then
          note "${1} rx-pin: restart pending -- the hub started before its drop-in was written (setup never restarts a running hub); restart intercom-hub.service, until then: ${line#bad rx-pin: }"
        else
          bad "${1} ${line#bad }"
        fi
        ;;
    esac
  done
}

# strih_dante_rx_pin_hub_pid -> the intercom hub unit's main PID, nothing when it does not run. rc 0.
strih_dante_rx_pin_hub_pid() {
  local p
  p="$(systemctl show -p MainPID --value intercom-hub 2>/dev/null || true)"
  case "$p" in '' | 0 | *[!0123456789]*) ;; *) printf '%s' "$p" ;; esac
}

# strih_dante_rx_pin_grade [HUB] -> verify-strih item 9's `(program-dante) rx-pin` rows and, HUB=1 (the
# facts give the hub's Dante device its own CPU, strih_dante_hub_declared), the `(intercom-dante) rx-pin`
# rows: the conf's `hub=`, the hub drop-in as rendered, the hub's two threads on it at nice -10. HUB=0
# with a `hub=` in the conf or our drop-in left = a FAIL (the facts no longer declare it). Through the
# caller's ok / bad. Read-only, any user. Always rc 0.
strih_dante_rx_pin_grade() {
  local proc="${STRIH_DANTE_RX_PIN_PROC:-/proc}" hubwant="${1:-0}" out mask rows hub f pid pending=0
  out="$(_strih_dante_rx_pin_rows "$proc" "$(pgrep -n -x obs 2>/dev/null || true)")"
  mask="$(printf '%s\n' "$out" | head -n 1)"
  rows="$(printf '%s\n' "$out" | tail -n +2)"
  # never a pipe into the emitter: its ok / bad must run in THIS shell (verify counts FAILS here)
  _strih_dante_rx_pin_emit "(program-dante)" < <(strih_dante_rx_pin_verdict "$(strih_dante_reserved_cpu)" "$mask" "$rows" obs)
  hub="$(strih_dante_reserved_hub_cpu)"
  f="$(strih_dante_rx_pin_hub_dropin_path)"
  if [ "$hubwant" != 1 ]; then
    if [ -n "$hub" ] || [ -e "$f" ]; then
      bad "(intercom-dante) rx-pin: the facts give the hub's Dante threads no CPU of their own, but $( [ -n "$hub" ] && printf 'the conf holds hub=%s' "$hub" || printf '%s is there' "$f" ) -- re-run setup-strih.sh step 13"
    fi
    return 0
  fi
  pid="$(strih_dante_rx_pin_hub_pid)"
  if cmp -s "$f" <(strih_dante_rx_pin_hub_dropin_text); then
    ok "(intercom-dante) rx-pin: ${f} runs the helper after every hub start"
    # a hub that started before its drop-in was written (setup never restarts a running hub) has not
    # run it yet: its rows are a NOTE until it restarts, like the managers' reboot-pending NOTE
    ! _strih_dante_rx_pin_hub_pending "$proc" "$pid" "$f" || pending=1
  else
    bad "(intercom-dante) rx-pin: ${f} missing or different -- re-run setup-strih.sh step 13, then restart intercom-hub"
  fi
  out="$(_strih_dante_rx_pin_rows "$proc" "$pid")"
  mask="$(printf '%s\n' "$out" | head -n 1)"
  rows="$(printf '%s\n' "$out" | tail -n +2)"
  _strih_dante_rx_pin_emit "(intercom-dante)" "$pending" < <(strih_dante_rx_pin_verdict "$hub" "$mask" "$rows" hub)
  return 0
}

# _strih_dante_rx_pin_hub_pending PROC PID DROPIN -> rc 0 when the hub process PID started before DROPIN
# was last written: DROPIN's mtime newer than btime (PROC/stat) + the process's start time (stat field 22,
# clock ticks since boot). rc 1 otherwise, also when anything is unreadable (never "pending" by default).
_strih_dante_rx_pin_hub_pending() {
  local proc="${1:?proc}" pid="${2-}" f="${3:?dropin}" bt="" k v stat start hz mt
  [ -n "$pid" ] || return 1
  while read -r k v; do
    case "$k" in btime) bt="$v" ;; esac
  done 2>/dev/null < "${proc}/stat" || true
  # the main thread's stat: its start time is the process's
  stat="$(cat "${proc}/${pid}/task/${pid}/stat" 2>/dev/null || true)"
  stat="${stat##*) }"
  # shellcheck disable=SC2086  # split the stat fields on purpose
  start="$(set -- $stat; printf '%s' "${20:-}")"
  hz="$(getconf CLK_TCK 2>/dev/null || true)"
  mt="$(stat -c %Y "$f" 2>/dev/null || true)"
  case "${bt}:${start}:${hz}:${mt}" in *[!0123456789:]* | :* | *::* | *:) return 1 ;; esac
  [ "$hz" -gt 0 ] || return 1
  [ "$mt" -gt "$((bt + start / hz))" ]
}
