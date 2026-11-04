#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only; its only top-level statement
# sources the equally source-only rx-pin lib) -- the scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/strih-dante-reserved-cpu.sh -- issue 1485: on a Dante-program box NOTHING but the strih
# OBS's dstack-engine + dstack-rx threads may run on the reserved CPU (/etc/strih-dante-rx-cpu.conf
# `reserved=`, strih-dante-rx-pin.sh). The rx pin only narrowed OBS's MAIN mask; every other process
# inherited PID 1's 0-15, and the program-audio sampler on CPU 15 cost the engine a rebootstrap in every
# 10-15 min window (A/B on strih-snv 3.11.2026: moved live to 0-1, a 15.5-min window was clean).
#
# The mechanism is ONE manager-level default, written by setup-strih step 13 on a Dante box only:
#   /etc/systemd/system.conf.d/50-strih-dante-reserved-cpu.conf  [Manager] CPUAffinity=<online - reserved>
#   /etc/systemd/user.conf.d/50-strih-dante-reserved-cpu.conf    the same, for every user manager
# PID 1 and every user manager start with that mask, so every unit and every forked process inherits it.
# It is AFFINITY, not a cpuset: sched_setaffinity is bounded only by the cgroup cpuset and the online
# CPUs, never by a parent's or the manager's mask, so the root helper's `taskset -p -c <reserved>` on the
# two dstack threads still succeeds, and OBS's own `taskset -c <obs=>` is a subset of it. A manager takes
# it when it re-reads its configuration: at the latest at the next boot (PID 1 may already take it on a
# daemon-reload, not verified on a box), and every process it starts from then on inherits it. Running
# processes keep their masks until they restart; setup re-executes PID 1 + the user manager when a
# drop-in changed and never moves a live process. So the grade reads the scan as a NOTE (reboot pending) while the drop-ins are newer than the
# boot. Another box removes our two files (header-checked), never another file.
# The same mechanism as the camboxes' housekeeping drop-in (scripts/lib/cambox-housekeeping-cpus.sh,
# issue 1367: PID 1 and every unit off the capture-IRQ core); that lib is cambox-bound (read-only
# root window, isolcpus, the IRQ split), so only the idea is shared, plus the user manager here.
#
#   strih_dante_reserved_cpu (strih-dante-rx-pin.sh) -> the reserved CPU from the conf, empty without one
#   strih_dante_reserved_cpus (strih-dante-rx-pin.sh) -> every reserved CPU (OBS's, then the hub's)
#   strih_dante_reserved_cpu_minus LIST CPUS    -> LIST without every CPU of CPUS (space-separated),
#                                                  compressed (pure; rc 1 when empty)
#   strih_dante_reserved_cpu_manager_text CPUS RESERVED [HUB] -> the drop-in (pure)
#
# Two reserved CPUs (4.11.2026, comment 6634395906): when the conf also holds the intercom hub's CPU
# (`hub=`, the hub's own Dante device), both leave the managers' default (strih-snv 0-13), and the grade
# scans each: CPU `reserved=` may hold only OBS's two dstack threads, CPU `hub=` only the hub's (its
# SCHED_FIFO `hub-mix` there is an offender like any other userland task).
#   strih_dante_reserved_cpu_install [USER]     setup-strih step 13 after strih_dante_rx_pin_install (root)
#   strih_dante_reserved_cpu_remove [USER]      the same step on any other box (root; silent when absent)
#   strih_dante_reserved_cpu_offenders PROC RES OBS_PID -> `comm pid/tid` per task allowed on RES
#   strih_dante_reserved_cpu_grade              verify-strih item 9's `(program-dante) reserved-cpu` rows
#
# Seams: STRIH_DANTE_RX_PIN_ROOT (a prefix for every /etc path), STRIH_DANTE_RX_PIN_OWNER (root:root),
# OBS_BOX_ONLINE_CPUS (the online list file), STRIH_DANTE_RX_PIN_PROC (/proc, the grade's reads, the
# boot time from its `stat`).

if ! declare -F strih_dante_reserved_cpu >/dev/null; then
  # shellcheck source=scripts/lib/strih-dante-rx-pin.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-dante-rx-pin.sh"   # the conf + obs_box_cpulist_expand / _compress
fi

STRIH_DANTE_RESERVED_CPU_HEADER="# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it."
# The offenders the grade names before it only counts the rest.
STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN=8

strih_dante_reserved_cpu_manager_paths() {
  local root="${STRIH_DANTE_RX_PIN_ROOT:-}"
  printf '%s/etc/systemd/system.conf.d/50-strih-dante-reserved-cpu.conf\n' "$root"
  printf '%s/etc/systemd/user.conf.d/50-strih-dante-reserved-cpu.conf\n' "$root"
}

# strih_dante_reserved_cpu_minus LIST CPUS -> LIST without every CPU of CPUS (space-separated; empty
# removes nothing), compressed. rc 1 + nothing when LIST is not a cpulist or nothing is left: never an
# empty affinity.
strih_dante_reserved_cpu_minus() {
  local all c out=""
  all="$(obs_box_cpulist_expand "${1-}")" || return 1
  for c in $all; do
    case " ${2-} " in *" ${c} "*) ;; *) out="${out}${out:+ }${c}" ;; esac
  done
  [ -n "$out" ] || return 1
  obs_box_cpulist_compress "$out"
}

# strih_dante_reserved_cpu_manager_text CPUS RESERVED [HUB] -> the system.conf.d / user.conf.d drop-in.
# Without HUB byte-identical to the one-CPU drop-in.
strih_dante_reserved_cpu_manager_text() {
  printf '%s\n' "$STRIH_DANTE_RESERVED_CPU_HEADER"
  printf '# CPU %s is reserved for the strih OBS'"'"'s dstack-engine + dstack-rx threads (/etc/strih-dante-rx-cpu.conf);\n' "${2:?reserved cpu}"
  [ -z "${3:-}" ] || printf '# CPU %s for the intercom hub'"'"'s own dstack-engine + dstack-rx threads (hub=, the same conf);\n' "$3"
  printf '# every unit and forked process starts off it. Affinity only: the rx-pin helper still moves the two threads there.\n'
  printf '[Manager]\nCPUAffinity=%s\n' "${1:?cpus}"
}

# strih_dante_reserved_cpu_want -> `<cpus> <reserved>[ <hub>]` of this box, rc 1 + a named reason on
# stderr when the conf has no reserved CPU or the online list leaves nothing.
strih_dante_reserved_cpu_want() {
  local online res cpus
  res="$(strih_dante_reserved_cpus)"
  [ -n "$res" ] || { echo "dante-reserved-cpu: no reserved CPU in $(strih_dante_rx_pin_conf_path)" >&2; return 1; }
  online="$(head -n 1 "${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}" 2>/dev/null || true)"
  cpus="$(strih_dante_reserved_cpu_minus "$online" "$res")" || {
    echo "dante-reserved-cpu: the online CPUs '${online}' leave nothing beside the reserved CPU(s) ${res}" >&2; return 1; }
  printf '%s %s\n' "$cpus" "$res"
}

# strih_dante_reserved_cpu_install -> both manager drop-ins (root), written only on a difference. rc 1 +
# a named reason when the box's sets cannot be derived. When a drop-in changed, PID 1 and USER's user
# manager are re-executed (daemon-reexec re-reads system.conf.d / user.conf.d and restarts no unit; live
# on strih-snv 3.11.2026 both read the new mask at once), so new processes start off the reserved CPU
# without a reboot; a failed re-exec is a WARN (the next boot applies it). Never a live move.
strih_dante_reserved_cpu_install() {
  local user="${1-}" owner="${STRIH_DANTE_RX_PIN_OWNER:-root:root}" want cpus res hub f text out changed=0 what
  want="$(strih_dante_reserved_cpu_want)" || return 1
  read -r cpus res hub <<<"$want"
  text="$(strih_dante_reserved_cpu_manager_text "$cpus" "$res" "$hub")" || return 1
  while IFS= read -r f; do
    install -d -m 0755 "$(dirname "$f")" || { echo "dante-reserved-cpu: cannot create $(dirname "$f")" >&2; return 1; }
    out="$(printf '%s\n' "$text" | obs_box_write_if_changed "$f" 0644 "$owner" "dante-reserved-cpu")" || { printf '%s\n' "$out"; return 1; }
    printf '%s\n' "$out"
    case "$out" in *" written") changed=1 ;; esac
  done < <(strih_dante_reserved_cpu_manager_paths)
  what="CPU ${res} only for the two dstack threads"
  [ -z "$hub" ] || what="CPU ${res} only for OBS's two dstack threads, CPU ${hub} only for the hub's"
  echo "  dante-reserved-cpu: systemd + user managers default to CPUs ${cpus} (${what}) -- a manager takes it when it re-reads its configuration, at the latest the next boot; running processes keep their masks until they restart"
  [ "$changed" = 1 ] || return 0
  _strih_dante_reserved_cpu_reexec "$user"
}

# _strih_dante_reserved_cpu_reexec [USER] -> PID 1 and USER's user manager re-read system.conf.d /
# user.conf.d (daemon-reexec restarts no unit), each bounded by `timeout 30`; each failure is its own
# WARNING naming the manager (the next boot applies it), never a failure. No USER = PID 1 only. rc 0.
_strih_dante_reserved_cpu_reexec() {
  local user="${1-}" done=""
  if timeout 30 systemctl daemon-reexec; then
    done="PID 1"
  else
    echo "  WARNING: dante-reserved-cpu: PID 1 daemon-reexec failed or timed out -- the system manager's new default applies at the next boot" >&2
  fi
  if [ -n "$user" ]; then
    if timeout 30 systemctl --user -M "${user}@" daemon-reexec; then
      done="${done:+${done} + }the user manager of ${user}"
    else
      echo "  WARNING: dante-reserved-cpu: the user manager of ${user}: daemon-reexec failed or timed out -- its new default applies at the next boot" >&2
    fi
  fi
  [ -z "$done" ] || echo "  dante-reserved-cpu: re-read: ${done} re-executed (daemon-reexec)"
  return 0
}

# strih_dante_reserved_cpu_remove [USER] -> remove OUR two drop-ins (a box without the Dante program); a
# file of the same name without our header is left and named. When one was removed, PID 1 and USER's user
# manager re-read their configuration, as on install. Prints only when it touched something.
strih_dante_reserved_cpu_remove() {
  local user="${1-}" f removed=0
  while IFS= read -r f; do
    [ -e "$f" ] || continue
    if head -n 1 "$f" 2>/dev/null | grep -qxF "$STRIH_DANTE_RESERVED_CPU_HEADER"; then
      rm -f "$f" && echo "  dante-reserved-cpu: removed ${f} (this box takes no Dante program)" && removed=1
    else
      echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2
    fi
  done < <(strih_dante_reserved_cpu_manager_paths)
  [ "$removed" = 0 ] || _strih_dante_reserved_cpu_reexec "$user"
  return 0
}

# _strih_dante_cpulist_has LIST CPU -> rc 0 when the kernel cpulist LIST holds CPU (bash only, no fork:
# the grade calls it once per task). An unparsable item counts as holding it (fail closed).
_strih_dante_cpulist_has() {
  local list="${1-}" cpu="${2-}" item a b
  local IFS=,
  for item in $list; do
    case "$item" in
      *[!0123456789-]* | '' | -* | *- | *-*-*) return 0 ;;
      *-*) a="${item%-*}"; b="${item#*-}" ;;
      *) a="$item"; b="$item" ;;
    esac
    if [ "$((10#$a))" -le "$cpu" ] && [ "$cpu" -le "$((10#$b))" ]; then return 0; fi
  done
  return 1
}

# strih_dante_reserved_cpu_offenders PROC RES OBS_PID -> one `<comm>\t<pid>/<tid>` line per task under
# PROC whose Cpus_allowed_list holds RES, except kernel threads (PF_KTHREAD in the stat flags) and the
# STRIH_DANTE_RX_PIN_THREADS of OBS_PID. A last line `scanned <n>` (rc 1 when nothing was readable).
strih_dante_reserved_cpu_offenders() {
  local proc="${1:?proc}" res="${2:?reserved cpu}" obs="${3-}" t pid tid k v name cpus line rest n=0
  local -a f
  for t in "$proc"/[0-9]*/task/[0-9]*; do
    [ -r "$t/status" ] || continue
    name=""; cpus=""
    while IFS=$'\t' read -r k v; do
      case "$k" in Name:) name="$v" ;; Cpus_allowed_list:) cpus="$v" ;; esac
    done 2>/dev/null < "$t/status" || continue
    [ -n "$cpus" ] || continue
    n=$((n + 1))
    _strih_dante_cpulist_has "$cpus" "$res" || continue
    # a task that exited since its status was read has no stat: it is gone, never an offender
    line=""
    IFS= read -r line 2>/dev/null < "$t/stat" || continue
    rest="${line##*) }"
    read -ra f <<<"$rest"
    # flags = stat field 9 = the 7th after the comm; PF_KTHREAD 0x00200000
    case "${f[6]:-}" in '' | *[!0123456789]*) continue ;; esac
    if (( f[6] & 0x00200000 )); then continue; fi
    pid="${t%/task/*}"; pid="${pid##*/}"; tid="${t##*/}"
    if [ -n "$obs" ] && [ "$pid" = "$obs" ]; then
      case " ${STRIH_DANTE_RX_PIN_THREADS} " in *" ${name} "*) continue ;; esac
    fi
    printf '%s\t%s/%s\n' "$name" "$pid" "$tid"
  done
  printf 'scanned %s\n' "$n"
  [ "$n" -gt 0 ]
}

# strih_dante_reserved_cpu_pending PROC -> rc 0 when the system drop-in was written after this boot (the
# managers have not restarted on it yet): its mtime newer than PROC/stat's `btime`. rc 1 otherwise, also
# when either time is unreadable (an unreadable boot time is never "pending").
strih_dante_reserved_cpu_pending() {
  local proc="${1:?proc}" bt="" mt k v
  [ -r "${proc}/stat" ] || return 1
  while read -r k v; do
    case "$k" in btime) bt="$v" ;; esac
  done 2>/dev/null < "${proc}/stat" || return 1
  mt="$(stat -c %Y "$(strih_dante_reserved_cpu_manager_paths | head -n 1)" 2>/dev/null || true)"
  case "${bt}:${mt}" in *[!0123456789:]* | :* | *:) return 1 ;; esac
  [ "$mt" -gt "$bt" ]
}

# strih_dante_reserved_cpu_grade -> verify-strih item 9's `(program-dante) reserved-cpu` rows through the
# caller's ok / note / bad: the manager drop-ins as rendered, and no task but OBS's two dstack threads
# (and kernel threads) allowed on the reserved CPU; with a hub CPU in the conf also an
# `(intercom-dante) reserved-cpu` row: no task but the hub's two dstack threads allowed on it. A scan
# with offenders is a NOTE naming them while the drop-ins are newer than the boot (a deploy before its
# reboot), a FAIL after. Read-only, any user. Always rc 0.
strih_dante_reserved_cpu_grade() {
  local proc="${STRIH_DANTE_RX_PIN_PROC:-/proc}" want cpus res hub f bad_files="" what
  if ! want="$(strih_dante_reserved_cpu_want 2>&1)"; then
    bad "(program-dante) reserved-cpu: ${want} -- re-run setup-strih.sh step 13"
    return 0
  fi
  read -r cpus res hub <<<"$want"
  while IFS= read -r f; do
    cmp -s "$f" <(strih_dante_reserved_cpu_manager_text "$cpus" "$res" "$hub") || bad_files="${bad_files} ${f}"
  done < <(strih_dante_reserved_cpu_manager_paths)
  what="CPU ${res} kept for the dstack threads"
  [ -z "$hub" ] || what="CPU ${res} kept for OBS's dstack threads, CPU ${hub} for the hub's"
  if [ -z "$bad_files" ]; then
    ok "(program-dante) reserved-cpu: systemd + user managers default to CPUAffinity=${cpus} (${what})"
  else
    bad "(program-dante) reserved-cpu: missing or different:${bad_files} -- re-run setup-strih.sh step 13"
  fi
  _strih_dante_reserved_cpu_scan "(program-dante)" "$proc" "$res" "$(pgrep -n -x obs 2>/dev/null || true)" "OBS's" "$cpus" "$bad_files"
  [ -z "$hub" ] || _strih_dante_reserved_cpu_scan "(intercom-dante)" "$proc" "$hub" "$(strih_dante_rx_pin_hub_pid)" "the intercom hub's" "$cpus" "$bad_files"
  return 0
}

# _strih_dante_reserved_cpu_scan LABEL PROC CPU PID OWNER CPUS BAD_FILES -> one row through the caller's
# ok / note / bad: no task under PROC but PID's two dstack threads (OWNER names them) and kernel threads
# allowed on CPU. CPUS = the managers' default (the cure's taskset); BAD_FILES non-empty = never pending.
_strih_dante_reserved_cpu_scan() {
  local label="$1" proc="$2" cpu="$3" pid="$4" owner="$5" cpus="$6" bad_files="$7" out line name scanned=0 total=0 shown="" msg
  local -A seen=()
  local -a order=()
  out="$(strih_dante_reserved_cpu_offenders "$proc" "$cpu" "$pid" 2>/dev/null || true)"
  while IFS= read -r line; do
    case "$line" in
      'scanned '*) scanned="${line#scanned }" ;;
      *$'\t'*)
        name="${line%%$'\t'*}"
        total=$((total + 1))
        # a process name is any user's text: counted by string length, never inside an arithmetic subscript
        [ -n "${seen[$name]+x}" ] || order+=("$name")
        seen[$name]+=x
        ;;
    esac
  done <<<"$out"
  if [ "$scanned" = 0 ]; then
    bad "${label} reserved-cpu: no task readable under ${proc} -- the reserved CPU ${cpu} is not graded"
  elif [ "$total" = 0 ]; then
    ok "${label} reserved-cpu: of ${scanned} tasks only ${owner} ${STRIH_DANTE_RX_PIN_THREADS// / + } (and kernel threads) may run on CPU ${cpu}"
  else
    for name in "${order[@]:0:$STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN}"; do shown="${shown}${shown:+, }${name} x${#seen[$name]}"; done
    [ "${#order[@]}" -le "$STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN" ] || shown="${shown} (+$(( ${#order[@]} - STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN )) more)"
    msg="${total} of ${scanned} tasks may run on the reserved CPU ${cpu}: ${shown}"
    if [ -z "$bad_files" ] && strih_dante_reserved_cpu_pending "$proc"; then
      note "${label} reserved-cpu: reboot pending -- the manager default was written after this boot; until then ${msg}"
    else
      bad "${label} reserved-cpu: ${msg} -- restart them, or move a live one with taskset -a -p -c ${cpus} <pid>"
    fi
  fi
  return 0
}
