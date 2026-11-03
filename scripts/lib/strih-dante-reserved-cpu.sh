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
# two dstack threads still succeeds, and OBS's own `taskset -c <obs=>` is a subset of it. It applies when
# a manager starts (the next boot): running processes keep their masks, setup never daemon-reexecs or
# moves a live process. Another box removes our two files (header-checked), never another file.
# The same mechanism as the camboxes' housekeeping drop-in (scripts/lib/cambox-housekeeping-cpus.sh,
# issue 1367: PID 1 and every unit off the capture-IRQ core); that lib is cambox-bound (read-only
# root window, isolcpus, the IRQ split), so only the idea is shared, plus the user manager here.
#
#   strih_dante_reserved_cpu                    -> the reserved CPU from the conf, empty without one
#   strih_dante_reserved_cpu_minus LIST CPU     -> LIST without CPU, compressed (pure; rc 1 when empty)
#   strih_dante_reserved_cpu_manager_text CPUS RESERVED -> the drop-in (pure)
#   strih_dante_reserved_cpu_install            setup-strih step 13 after strih_dante_rx_pin_install (root)
#   strih_dante_reserved_cpu_remove             the same step on any other box (root; silent when absent)
#   strih_dante_reserved_cpu_offenders PROC RES OBS_PID -> `comm pid/tid` per task allowed on RES
#   strih_dante_reserved_cpu_grade              verify-strih item 9's `(program-dante) reserved-cpu` rows
#
# Seams: STRIH_DANTE_RX_PIN_ROOT (a prefix for every /etc path), STRIH_DANTE_RX_PIN_OWNER (root:root),
# OBS_BOX_ONLINE_CPUS (the online list file), STRIH_DANTE_RX_PIN_PROC (/proc, the grade's reads).

if ! declare -F strih_dante_rx_pin_conf_path >/dev/null; then
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

# strih_dante_reserved_cpu -> the conf's `reserved=` CPU when it is a number, else nothing. Always rc 0.
strih_dante_reserved_cpu() {
  local v
  v="$(sed -n 's/^reserved=//p' "$(strih_dante_rx_pin_conf_path)" 2>/dev/null | head -n 1 || true)"
  case "$v" in '' | *[!0123456789]*) v="" ;; esac
  printf '%s' "$v"
}

# strih_dante_reserved_cpu_minus LIST CPU -> LIST without CPU (an empty CPU removes nothing), compressed.
# rc 1 + nothing when LIST is not a cpulist or nothing is left: never an empty affinity.
strih_dante_reserved_cpu_minus() {
  local all c out=""
  all="$(obs_box_cpulist_expand "${1-}")" || return 1
  for c in $all; do
    [ "$c" = "${2-}" ] || out="${out}${out:+ }${c}"
  done
  [ -n "$out" ] || return 1
  obs_box_cpulist_compress "$out"
}

# strih_dante_reserved_cpu_manager_text CPUS RESERVED -> the system.conf.d / user.conf.d drop-in.
strih_dante_reserved_cpu_manager_text() {
  printf '%s\n' "$STRIH_DANTE_RESERVED_CPU_HEADER"
  printf '# CPU %s is reserved for the strih OBS'"'"'s dstack-engine + dstack-rx threads (/etc/strih-dante-rx-cpu.conf);\n' "${2:?reserved cpu}"
  printf '# every unit and forked process starts off it. Affinity only: the rx-pin helper still moves the two threads there.\n'
  printf '[Manager]\nCPUAffinity=%s\n' "${1:?cpus}"
}

# strih_dante_reserved_cpu_want -> `<cpus> <reserved>` of this box, rc 1 + a named reason on stderr when
# the conf has no reserved CPU or the online list leaves nothing.
strih_dante_reserved_cpu_want() {
  local online res cpus
  res="$(strih_dante_reserved_cpu)"
  [ -n "$res" ] || { echo "dante-reserved-cpu: no reserved CPU in $(strih_dante_rx_pin_conf_path)" >&2; return 1; }
  online="$(head -n 1 "${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}" 2>/dev/null || true)"
  cpus="$(strih_dante_reserved_cpu_minus "$online" "$res")" || {
    echo "dante-reserved-cpu: the online CPUs '${online}' leave nothing beside the reserved CPU ${res}" >&2; return 1; }
  printf '%s %s\n' "$cpus" "$res"
}

# strih_dante_reserved_cpu_install -> both manager drop-ins (root), written only on a difference. rc 1 +
# a named reason when the box's sets cannot be derived. Never a daemon-reexec, never a live move.
strih_dante_reserved_cpu_install() {
  local owner="${STRIH_DANTE_RX_PIN_OWNER:-root:root}" want cpus res f text
  want="$(strih_dante_reserved_cpu_want)" || return 1
  read -r cpus res <<<"$want"
  text="$(strih_dante_reserved_cpu_manager_text "$cpus" "$res")" || return 1
  while IFS= read -r f; do
    install -d -m 0755 "$(dirname "$f")" || { echo "dante-reserved-cpu: cannot create $(dirname "$f")" >&2; return 1; }
    printf '%s\n' "$text" | obs_box_write_if_changed "$f" 0644 "$owner" "dante-reserved-cpu" || return 1
  done < <(strih_dante_reserved_cpu_manager_paths)
  echo "  dante-reserved-cpu: systemd + user managers default to CPUs ${cpus} (CPU ${res} only for the two dstack threads) -- applies at the next boot; running processes keep their masks"
}

# strih_dante_reserved_cpu_remove -> remove OUR two drop-ins (a box without the Dante program); a file of
# the same name without our header is left and named. Prints only when it touched something.
strih_dante_reserved_cpu_remove() {
  local f
  while IFS= read -r f; do
    [ -e "$f" ] || continue
    if head -n 1 "$f" 2>/dev/null | grep -qxF "$STRIH_DANTE_RESERVED_CPU_HEADER"; then
      rm -f "$f" && echo "  dante-reserved-cpu: removed ${f} (this box takes no Dante program)"
    else
      echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2
    fi
  done < <(strih_dante_reserved_cpu_manager_paths)
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
    line=""
    IFS= read -r line 2>/dev/null < "$t/stat" || true
    rest="${line##*) }"
    read -ra f <<<"$rest"
    # flags = stat field 9 = the 7th after the comm; PF_KTHREAD 0x00200000
    case "${f[6]:-}" in '' | *[!0123456789]*) ;; *) if (( f[6] & 0x00200000 )); then continue; fi ;; esac
    pid="${t%/task/*}"; pid="${pid##*/}"; tid="${t##*/}"
    if [ -n "$obs" ] && [ "$pid" = "$obs" ]; then
      case " ${STRIH_DANTE_RX_PIN_THREADS} " in *" ${name} "*) continue ;; esac
    fi
    printf '%s\t%s/%s\n' "$name" "$pid" "$tid"
  done
  printf 'scanned %s\n' "$n"
  [ "$n" -gt 0 ]
}

# strih_dante_reserved_cpu_grade -> verify-strih item 9's two `(program-dante) reserved-cpu` rows through
# the caller's ok / bad: the manager drop-ins as rendered, and no task but the two dstack threads (and
# kernel threads) allowed on the reserved CPU. Read-only, any user. Always rc 0.
strih_dante_reserved_cpu_grade() {
  local proc="${STRIH_DANTE_RX_PIN_PROC:-/proc}" want cpus res f bad_files="" obs out line name scanned=0 total=0 shown=""
  local -A seen=()
  local -a order=()
  if ! want="$(strih_dante_reserved_cpu_want 2>&1)"; then
    bad "(program-dante) reserved-cpu: ${want} -- re-run setup-strih.sh step 13"
    return 0
  fi
  read -r cpus res <<<"$want"
  while IFS= read -r f; do
    cmp -s "$f" <(strih_dante_reserved_cpu_manager_text "$cpus" "$res") || bad_files="${bad_files} ${f}"
  done < <(strih_dante_reserved_cpu_manager_paths)
  if [ -z "$bad_files" ]; then
    ok "(program-dante) reserved-cpu: systemd + user managers default to CPUAffinity=${cpus} (CPU ${res} kept for the dstack threads)"
  else
    bad "(program-dante) reserved-cpu: missing or different:${bad_files} -- re-run setup-strih.sh step 13 (applies at the next boot)"
  fi
  obs="$(pgrep -n -x obs 2>/dev/null || true)"
  out="$(strih_dante_reserved_cpu_offenders "$proc" "$res" "$obs" 2>/dev/null || true)"
  while IFS= read -r line; do
    case "$line" in
      'scanned '*) scanned="${line#scanned }" ;;
      *$'\t'*)
        name="${line%%$'\t'*}"
        total=$((total + 1))
        if [ -z "${seen[$name]+x}" ]; then order+=("$name"); seen[$name]=0; fi
        seen[$name]=$((seen[$name] + 1))
        ;;
    esac
  done <<<"$out"
  if [ "$scanned" = 0 ]; then
    bad "(program-dante) reserved-cpu: no task readable under ${proc} -- the reserved CPU is not graded"
  elif [ "$total" = 0 ]; then
    ok "(program-dante) reserved-cpu: of ${scanned} tasks only OBS's ${STRIH_DANTE_RX_PIN_THREADS// / + } (and kernel threads) may run on CPU ${res}"
  else
    for name in "${order[@]:0:$STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN}"; do shown="${shown}${shown:+, }${name} x${seen[$name]}"; done
    [ "${#order[@]}" -le "$STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN" ] || shown="${shown} (+$(( ${#order[@]} - STRIH_DANTE_RESERVED_CPU_NAMES_SHOWN )) more)"
    bad "(program-dante) reserved-cpu: ${total} of ${scanned} tasks may run on the reserved CPU ${res}: ${shown} -- the manager default applies at the next boot; move a live one with taskset -a -p -c ${cpus} <pid>"
  fi
  return 0
}
