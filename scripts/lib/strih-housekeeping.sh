#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/strih-housekeeping.sh -- issue 1425 (design 7106973291, dstack#476 + the 23.11.2026 exec
# snoop): a strih box's housekeeping runs on the CPUs OBS does not use, the box fact
# STRIH_HOUSEKEEPING_CPUS (strih-snv 0-1 = the complement of the OBS mask 2-15; `none` = nothing moved).
#
# Root cause (read live 23.11.2026 on strih-snv): OBS runs on 2-15 (/etc/strih-isolated-cpus.conf), but
# the Aquantia RX IRQs kept smp_affinity_list 0-15 with 8 RSS queues and netdev_budget_usecs 2000, and
# rustdesk (spawning ps / loginctl / sh / pgrep ~6/s), strih_display_watch.py (xrandr 0.5/s) and the OBS
# launcher's shutdown watch (stat / tail / grep / sleep 1/s) all ran on OBS CPUs. Those wake-ups delayed
# dlink's RX pump and OBS's audio thread (the late_to_mix events of issue 1425).
#
# The spread (design 7119179275, 24.11.2026, superseding the CPU-1-alone split of 7116063669): one CPU could
# not carry the RX softirq (CPU 1 ~86 % softirq, time_squeeze, InDroppedDma). EVERY CPU of the set carries rig
# NIC IRQs (strih_housekeeping_irq_cpus, strih-snv 0-1), each IRQ on ONE of them, balanced by its measured rate
# by the oneshot; the FIRST CPU also carries the tasks below and rig-background.slice
# (strih_housekeeping_task_cpus, strih-snv 0). The program-audio sampler leaves the set for the OBS mask.
#
# What this lib does (setup-strih step 11b, verify-strih item 16):
#   * the rig NIC's IRQs + RSS: the step-11b boot oneshot (strih_nic_irq_affinity_script_text in
#     strih-provision.sh, generalised in place) -- not here;
#   * /etc/strih-housekeeping-cpus.conf: the TASK CPU list, read by strih-obs-start.sh, which puts its own
#     shutdown watch (the stat / grep / tail loop) there at nice 10 at the next OBS start;
#   * NO netdev_budget_usecs change: kernel 7.0 at HZ 1000 refuses anything below 2000 us (EINVAL, read on
#     strih-snv 23.11.2026), which is what the box runs; a leftover 60-strih-housekeeping.conf of ours is
#     removed and verify reports the running value;
#   * CPUAffinity= + Nice=10 drop-ins for rustdesk.service (system) and strih-display-watch.service (the
#     operator's --user unit); a running unit whose drop-in changed is restarted (a stopped one is never
#     started);
#   * a udev rule that sets the RSS channel count at `add`, before the link is up, so the boot oneshot's
#     `ethtool -L` (a ring re-init = a link drop) never runs after network-online (review round 1);
#   * a NetworkManager dispatcher hook that re-runs the oneshot's script on the rig NIC's `up`: a link
#     reset re-requests the IRQs with the driver's own affinity.
#   * strih-nic-irq-rebalance.timer + .service (design 7119686502): the same script once, 5 min after boot,
#     when the rig carries traffic (the boot oneshot's sample reads every rate 0); the timer enabled, never
#     started.
# `none` removes our files (header-checked: a file without our first line is never touched).
#
# Seams (tests): STRIH_HOUSEKEEPING_ROOT (prefix of /etc and /proc/sys), STRIH_HOUSEKEEPING_OWNER (the
# root files' owner), STRIH_HOUSEKEEPING_SYS (/sys), STRIH_HOUSEKEEPING_PROC (/proc for pids),
# STRIH_HOUSEKEEPING_IPADDR (the `ip -o -4 addr show` text), STRIH_HOUSEKEEPING_SAMPLE_S (the (hk-rx) +
# (hk-softnet) sample, default 2 s), ETHTOOL. systemctl / udevadm / sleep from PATH.
# Needs scripts/lib/obs-box-write.sh + obs-box-background.sh (sourced here when absent) and, for the
# grade, the caller's ok / bad / note.

# The two sources are not followed by shellcheck (source=/dev/null): both libs are linted on their own, and
# following them from here grew verify-strih.sh's closure by ~250 MB (the issue-1395 memory budget).
if ! declare -F _obs_box_write_text >/dev/null; then
  # shellcheck source=/dev/null
  . "$(dirname "${BASH_SOURCE[0]}")/obs-box-write.sh"
fi
if ! declare -F obs_box_cpulist_expand >/dev/null; then
  # shellcheck source=/dev/null
  . "$(dirname "${BASH_SOURCE[0]}")/obs-box-background.sh"
fi

STRIH_HOUSEKEEPING_HEADER="# camera-box issue 1425: housekeeping off the OBS CPUs (scripts/lib/strih-housekeeping.sh) -- do not edit"
STRIH_HOUSEKEEPING_NICE=10
# The units placed on the housekeeping CPUs: <scope>:<unit>.
STRIH_HOUSEKEEPING_UNITS=(system:rustdesk.service user:strih-display-watch.service)

strih_housekeeping_conf_path() { printf '%s/etc/strih-housekeeping-cpus.conf\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
strih_housekeeping_sysctl_path() { printf '%s/etc/sysctl.d/60-strih-housekeeping.conf\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
strih_housekeeping_budget_proc() { printf '%s/proc/sys/net/core/netdev_budget_usecs\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
strih_housekeeping_isolated_path() { printf '%s/etc/strih-isolated-cpus.conf\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
strih_housekeeping_online_path() { printf '%s/devices/system/cpu/online\n' "${STRIH_HOUSEKEEPING_SYS:-/sys}"; }
strih_housekeeping_udev_path() { printf '%s/etc/udev/rules.d/71-strih-housekeeping-rss.rules\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
strih_housekeeping_hook_path() { printf '%s/etc/NetworkManager/dispatcher.d/60-strih-housekeeping-irq\n' "${STRIH_HOUSEKEEPING_ROOT:-}"; }
STRIH_HOUSEKEEPING_IRQ_SCRIPT=/usr/local/bin/strih-nic-irq-affinity.sh

# strih_housekeeping_udev_text DRIVER COUNT -> the rule: a PCIe NIC (one with msi_irqs) of DRIVER gets COUNT
# combined RSS channels at `add`, while it is still down, so no link drops.
strih_housekeeping_udev_text() {
  printf '%s\n' "$STRIH_HOUSEKEEPING_HEADER"
  printf 'ACTION=="add", SUBSYSTEM=="net", DRIVERS=="%s", TEST=="device/msi_irqs", RUN+="/usr/sbin/ethtool -L $name combined %s"\n' \
    "${1:?driver required}" "${2:?count required}"
}

# strih_housekeeping_hook_text DRIVER -> the NetworkManager dispatcher hook (IFACE ACTION): on `up` of a
# DRIVER NIC, the step-11b oneshot's script re-places the IRQs (the RSS count is already right then).
strih_housekeeping_hook_text() {
  printf '#!/bin/sh\n%s\n' "$STRIH_HOUSEKEEPING_HEADER"
  printf '# NetworkManager runs it as <iface> <action>: a link reset re-requests the rig NIC IRQs with the\n'
  printf "# driver's own affinity, so every up of a %s NIC puts them back on the housekeeping CPUs.\n" "${1:?driver required}"
  printf '[ "$2" = up ] || exit 0\n[ -x %s ] || exit 0\n' "$STRIH_HOUSEKEEPING_IRQ_SCRIPT"
  printf 'drv="$(readlink -f "/sys/class/net/$1/device/driver" 2>/dev/null || true)"\n'
  printf '[ "${drv##*/}" = %s ] || exit 0\nexec %s\n' "$1" "$STRIH_HOUSEKEEPING_IRQ_SCRIPT"
}

# The delayed re-balance (design 7119686502): the boot oneshot samples the IRQ rates before traffic flows
# (every rate 0 = the round-robin fallback), so a timer runs the same script once, 5 min after boot, when
# the strih OBS pulls the cameras + the program. Once per boot only: every IRQ move costs a loss burst.
STRIH_HOUSEKEEPING_REBALANCE=strih-nic-irq-rebalance
strih_housekeeping_rebalance_path() { printf '%s/etc/systemd/system/%s.%s\n' "${STRIH_HOUSEKEEPING_ROOT:-}" "$STRIH_HOUSEKEEPING_REBALANCE" "${1:?service|timer required}"; }

# strih_housekeeping_rebalance_service_text -> systemd/strih-nic-irq-rebalance.service (byte-parity test):
# a plain oneshot (no RemainAfterExit, no [Install]: only the timer starts it).
strih_housekeeping_rebalance_service_text() {
  printf '%s\n' "$STRIH_HOUSEKEEPING_HEADER"
  printf '[Unit]\nDescription=strih box: re-balance the rig-NIC IRQs by their measured rate once the rig carries traffic (issue 1425)\n'
  printf 'Documentation=https://github.com/zbynekdrlik/camera-box\nAfter=strih-nic-irq-affinity.service\n\n'
  printf '[Service]\nType=oneshot\nExecStart=%s\n' "$STRIH_HOUSEKEEPING_IRQ_SCRIPT"
}

# strih_housekeeping_rebalance_timer_text -> systemd/strih-nic-irq-rebalance.timer (byte-parity test):
# once, 5 min after boot; no Persistent (a missed boot run is not caught up).
strih_housekeeping_rebalance_timer_text() {
  printf '%s\n' "$STRIH_HOUSEKEEPING_HEADER"
  printf '[Unit]\nDescription=strih box: re-balance the rig-NIC IRQs 5 min after boot (issue 1425)\n'
  printf 'Documentation=https://github.com/zbynekdrlik/camera-box\n\n'
  printf '[Timer]\nOnBootSec=5min\nUnit=%s.service\n\n[Install]\nWantedBy=timers.target\n' "$STRIH_HOUSEKEEPING_REBALANCE"
}

# strih_housekeeping_rebalance_install OWNER -> both units written if changed (daemon-reload then), the timer
# enabled when it is not; never started (it fires at the next boot). rc 1 on a write failure; a failed
# reload / enable is a WARN (verify item 16 FAILs it).
strih_housekeeping_rebalance_install() {
  local owner="$1" kind dest rc changed=0 timer="${STRIH_HOUSEKEEPING_REBALANCE}.timer"
  for kind in service timer; do
    dest="$(strih_housekeeping_rebalance_path "$kind")"
    install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
    rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping "strih_housekeeping_rebalance_${kind}_text" || rc=$?
    [ "$rc" != 1 ] || return 1
    [ "$rc" != 0 ] || changed=1
  done
  if [ "$changed" = 1 ] && ! systemctl daemon-reload; then
    echo "  WARN: daemon-reload failed -- run it, then systemctl enable ${timer}; verify item 16 FAILs it until then"
  elif ! systemctl is-enabled -q "$timer"; then
    if systemctl enable "$timer"; then
      echo "  housekeeping: ${timer} enabled -- the IRQs are re-balanced by their measured rate 5 min after each boot"
    else
      echo "  WARN: systemctl enable ${timer} failed -- enable it by hand; verify item 16 FAILs it until then"
    fi
  fi
  return 0
}

# strih_housekeeping_rebalance_remove -> a `none` box: our two units go (header-checked; the timer disabled
# and stopped first, so no wants link dangles), then one daemon-reload.
strih_housekeeping_rebalance_remove() {
  local kind f gone=0 timer="${STRIH_HOUSEKEEPING_REBALANCE}.timer"
  for kind in timer service; do
    f="$(strih_housekeeping_rebalance_path "$kind")"
    [ -e "$f" ] || continue
    if ! strih_housekeeping_ours "$f"; then echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2; continue; fi
    if [ "$kind" = timer ] && ! systemctl disable --now "$timer"; then
      echo "  WARN: systemctl disable --now ${timer} failed -- disable it by hand"
    fi
    rm -f "$f" || return 1
    echo "  housekeeping: removed ${f}"
    gone=1
  done
  [ "$gone" = 0 ] || systemctl daemon-reload || echo "  WARN: daemon-reload failed after removing the re-balance units -- run it by hand"
  # a service that failed this boot would stay listed as `not-found failed` (review round 1)
  [ -e "$(strih_housekeeping_rebalance_path service)" ] || [ "$gone" = 0 ] \
    || systemctl reset-failed "${STRIH_HOUSEKEEPING_REBALANCE}.service" 2>/dev/null || true
  return 0
}

# strih_housekeeping_rebalance_row -> the `(hk-rebalance)` row: both units as rendered, the timer enabled,
# and the service's run this boot (systemd's state is per boot: a start monotonic of 0 = not run yet).
strih_housekeeping_rebalance_row() {
  local kind f show line state btime mtime mono="" result="" status="" timer="${STRIH_HOUSEKEEPING_REBALANCE}.timer"
  for kind in service timer; do
    f="$(strih_housekeeping_rebalance_path "$kind")"
    if ! cmp -s "$f" <("strih_housekeeping_rebalance_${kind}_text"); then
      bad "(hk-rebalance) ${f} missing or drifted -- re-run setup-strih.sh step 11b"; return 0
    fi
  done
  if ! systemctl is-enabled -q "$timer"; then
    bad "(hk-rebalance) ${timer} is not enabled -- systemctl enable ${timer} (or re-run setup-strih.sh step 11b)"; return 0
  fi
  show="$(systemctl show -p ExecMainStartTimestampMonotonic -p Result -p ExecMainStatus "${STRIH_HOUSEKEEPING_REBALANCE}.service" 2>/dev/null || true)"
  while IFS= read -r line; do
    case "$line" in
      ExecMainStartTimestampMonotonic=*) mono="${line#*=}" ;;
      Result=*) result="${line#*=}" ;;
      ExecMainStatus=*) status="${line#*=}" ;;
    esac
  done <<<"$show"
  case "$mono" in
    # two arms, no alternation: the verify subshell sweep reads a ` | ` before a grader as a pipe
    '') bad "(hk-rebalance) ${timer} enabled; the service's state is unreadable (ExecMainStartTimestampMonotonic '')" ;;
    *[!0123456789]*) bad "(hk-rebalance) ${timer} enabled; the service's state is unreadable (ExecMainStartTimestampMonotonic '${mono}')" ;;
    0) # not run this boot: the timer's own state says why (review round 1)
       show="$(systemctl show -p ActiveState -p SubState "$timer" 2>/dev/null || true)"
       state="$(printf '%s\n' "$show" | sed -n 's/^ActiveState=//p')/$(printf '%s\n' "$show" | sed -n 's/^SubState=//p')"
       case "$state" in
         active/waiting) note "(hk-rebalance) ${timer} enabled and waiting; the re-balance has not run yet this boot (it runs 5 min after boot)" ;;
         inactive/*)
           # an enabled timer starts with timers.target: inactive is only honest for one written after boot
           btime="$(sed -n 's/^btime //p' "${STRIH_HOUSEKEEPING_PROC:-/proc}/stat" 2>/dev/null || true)"
           mtime="$(stat -c %Y "$(strih_housekeeping_rebalance_path timer)" 2>/dev/null || true)"
           if [[ "$btime" =~ ^[0123456789]+$ ]] && [[ "$mtime" =~ ^[0123456789]+$ ]] && [ "$mtime" -lt "$btime" ]; then
             bad "(hk-rebalance) ${timer} is enabled but inactive, installed before this boot -- it should have started with timers.target (journalctl -b -u ${timer})"
           else
             note "(hk-rebalance) ${timer} enabled but not started this boot (installed after boot): the first re-balance runs 5 min after the next boot"
           fi ;;
         *) bad "(hk-rebalance) ${timer} is ${state} but the re-balance never ran this boot -- journalctl -b -u ${timer} -u ${STRIH_HOUSEKEEPING_REBALANCE}.service" ;;
       esac ;;
    *) if [ "$result" = success ] && [ "$status" = 0 ]; then
         ok "(hk-rebalance) ${timer} enabled; the re-balance ran this boot: success (exit 0)"
       else
         bad "(hk-rebalance) the re-balance ran this boot and failed: ${result:-unread} (exit ${status:-unread}) -- journalctl -b -u ${STRIH_HOUSEKEEPING_REBALANCE}.service"
       fi ;;
  esac
  return 0
}

# strih_housekeeping_dropin_path SCOPE UNIT HOME -> the drop-in file of UNIT (system: /etc/systemd/system,
# user: HOME/.config/systemd/user).
strih_housekeeping_dropin_path() {
  case "$1" in
    system) printf '%s/etc/systemd/system/%s.d/50-camera-box-housekeeping.conf\n' "${STRIH_HOUSEKEEPING_ROOT:-}" "$2" ;;
    user) printf '%s/.config/systemd/user/%s.d/50-camera-box-housekeeping.conf\n' "$3" "$2" ;;
    *) return 1 ;;
  esac
}

strih_housekeeping_conf_text() { printf '%s\n' "${1:?cpus required}"; }

# strih_housekeeping_irq_cpus [CPUS] -> the CPUs the rig NIC IRQs are spread over: all of CPUS, canonical
# (default the box fact STRIH_HOUSEKEEPING_CPUS); strih_housekeeping_task_cpus [CPUS] -> its FIRST CPU (the
# housekeeping tasks + rig-background.slice). Both print `none` for `none`; rc 1 on an unreadable fact or a
# malformed list.
strih_housekeeping_irq_cpus() { _strih_housekeeping_split obs_box_cpulist_canon "$@"; }
strih_housekeeping_task_cpus() { _strih_housekeeping_split obs_box_cpulist_first "$@"; }
_strih_housekeeping_split() {
  local fn="$1" hk
  if [ "$#" -ge 2 ]; then hk="$2"; else hk="$(strih_lx_housekeeping_cpus)" || return 1; fi
  if [ "$hk" = none ]; then printf 'none\n'; else "$fn" "$hk"; fi
}

# strih_housekeeping_slice_exclude [CPUS] -> the CPUs rig-background.slice keeps out: the IRQ CPUs that are
# not also task CPUs (strih-snv 1). `none` for a `none` box and for a one-CPU set (the IRQs share that CPU
# with the tasks: nothing else to run on). rc 1 on an unreadable fact.
strih_housekeeping_slice_exclude() {
  local irq task c out=""
  irq="$(strih_housekeeping_irq_cpus "$@")" && task="$(strih_housekeeping_task_cpus "$@")" || return 1
  [ "$irq" != none ] || { printf 'none\n'; return 0; }
  task=" $(obs_box_cpulist_expand "$task") "
  for c in $(obs_box_cpulist_expand "$irq"); do
    case "$task" in *" $c "*) ;; *) out="${out}${out:+ }${c}" ;; esac
  done
  if [ -n "$out" ]; then obs_box_cpulist_compress "$out"; else printf 'none\n'; fi
}

# strih_housekeeping_dropin_text CPUS -> the [Service] drop-in placing a unit on CPUS at nice 10.
strih_housekeeping_dropin_text() {
  printf '%s\n' "$STRIH_HOUSEKEEPING_HEADER"
  printf '[Service]\nCPUAffinity=%s\nNice=%s\n' "${1:?cpus required}" "$STRIH_HOUSEKEEPING_NICE"
}

# strih_housekeeping_ours FILE -> rc 0 when FILE carries our header on its first line (its second after a
# shebang).
strih_housekeeping_ours() {
  local l1 l2
  { IFS= read -r l1 || true; IFS= read -r l2 || true; } < "$1" 2>/dev/null || return 1
  [ "$l1" = "$STRIH_HOUSEKEEPING_HEADER" ] || { [ "$l1" = '#!/bin/sh' ] && [ "$l2" = "$STRIH_HOUSEKEEPING_HEADER" ]; }
}

# strih_housekeeping_count CPUS -> how many CPUs a cpulist names (rc 1 on a malformed list).
strih_housekeeping_count() {
  local e
  e="$(obs_box_cpulist_expand "${1-}")" || return 1
  set -- $e
  printf '%s\n' "$#"
}

# strih_housekeeping_same_cpus A B -> rc 0 when two cpulists name the same CPUs ("0-1" == "0,1").
strih_housekeeping_same_cpus() {
  local a b
  a="$(obs_box_cpulist_expand "${1-}")" || return 1
  b="$(obs_box_cpulist_expand "${2-}")" || return 1
  [ "$a" = "$b" ]
}

# _strih_housekeeping_systemctl SCOPE USER ARGS... -> systemctl (system) or the operator's --user one.
_strih_housekeeping_systemctl() {
  local scope="$1" user="$2"
  shift 2
  if [ "$scope" = user ]; then
    strih_session_apps_user_systemctl "$user" "$@"
  else
    systemctl "$@"
  fi
}

# _strih_housekeeping_put DEST MODE OWNER LABEL CMD... -> obs_box_write_if_changed_rendered, its log line
# printed; rc 0 written, 3 unchanged, 1 failure.
_strih_housekeeping_put() {
  local out
  out="$(obs_box_write_if_changed_rendered "$@")" || { printf '%s\n' "$out"; return 1; }
  printf '%s\n' "$out"
  case "$out" in *" written") return 0 ;; *) return 3 ;; esac
}

# strih_housekeeping_install USER HOME -> setup-strih step 11b's housekeeping half. rc 1 named on a failure
# (a unit restart that fails is a WARN: the unit keeps running on its old placement, verify FAILs it).
strih_housekeeping_install() {
  local user="${1:?user required}" home="${2:?home required}" hk irq task owner rc entry scope unit dest live proc drv
  owner="${STRIH_HOUSEKEEPING_OWNER:-root:root}"
  hk="$(strih_lx_housekeeping_cpus)" || { echo "housekeeping: the box fact STRIH_HOUSEKEEPING_CPUS cannot be read" >&2; return 1; }
  if [ "$hk" = none ]; then
    strih_housekeeping_remove "$user" "$home"
    return
  fi
  irq="$(strih_housekeeping_irq_cpus "$hk")" && task="$(strih_housekeeping_task_cpus "$hk")" \
    || { echo "housekeeping: STRIH_HOUSEKEEPING_CPUS '${hk}' is not a cpulist" >&2; return 1; }
  echo "  housekeeping CPUs ${hk} (STRIH_HOUSEKEEPING_CPUS): the rig NIC IRQs + RSS spread over ${irq} by load (the oneshot below); rustdesk, the display watch, the OBS launcher, rig-background.slice on ${task}"
  dest="$(strih_housekeeping_conf_path)"
  install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
  rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_conf_text "$task" || rc=$?
  [ "$rc" != 1 ] || return 1
  [ "$rc" != 0 ] || echo "  housekeeping: the OBS launcher's shutdown watch moves at the next OBS start (systemctl --user restart strih-obs.service)"

  # issue 1425 (23.11.2026): no budget change -- kernel 7.0 at HZ 1000 refuses netdev_budget_usecs < 2000
  dest="$(strih_housekeeping_sysctl_path)"
  if [ -e "$dest" ] && strih_housekeeping_ours "$dest"; then
    rm -f "$dest" || { echo "housekeeping: cannot remove ${dest}" >&2; return 1; }
    echo "  housekeeping: removed ${dest} (the kernel refuses a netdev_budget_usecs below 2000)"
  fi
  drv="$(strih_lx_nic_driver)" || { echo "housekeeping: the box fact STRIH_NIC_DRIVER cannot be read" >&2; return 1; }
  dest="$(strih_housekeeping_udev_path)"
  install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
  rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_udev_text "$drv" "$(strih_housekeeping_count "$irq")" || rc=$?
  [ "$rc" != 1 ] || return 1
  [ "$rc" != 0 ] || udevadm control --reload-rules \
    || echo "  WARN: udevadm control --reload-rules failed -- the RSS rule applies at the next boot"
  dest="$(strih_housekeeping_hook_path)"
  install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
  rc=0; _strih_housekeeping_put "$dest" 0755 "$owner" housekeeping strih_housekeeping_hook_text "$drv" || rc=$?
  [ "$rc" != 1 ] || return 1
  strih_housekeeping_rebalance_install "$owner" || return 1

  for entry in "${STRIH_HOUSEKEEPING_UNITS[@]}"; do
    scope="${entry%%:*}"; unit="${entry#*:}"
    dest="$(strih_housekeeping_dropin_path "$scope" "$unit" "$home")"
    if [ "$scope" = user ]; then
      strih_user_home_dir "$home" "$user" "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
      rc=0; _strih_housekeeping_put "$dest" 0644 "${user}:$(id -gn "$user" 2>/dev/null || printf '%s' "$user")" housekeeping strih_housekeeping_dropin_text "$task" || rc=$?
    else
      install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
      rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_dropin_text "$task" || rc=$?
    fi
    [ "$rc" != 1 ] || return 1
    [ "$rc" = 0 ] || continue
    # a unit that is not running (or not installed yet: rustdesk is step 16b, the display watch 16d) is
    # never started; it takes the drop-in when it starts.
    if ! _strih_housekeeping_systemctl "$scope" "$user" daemon-reload; then
      echo "  WARN: ${unit}: daemon-reload failed -- run it by hand, then restart ${unit}; verify item 16 FAILs it until then"
    elif ! _strih_housekeeping_systemctl "$scope" "$user" is-active -q "$unit"; then
      echo "  housekeeping: ${unit} drop-in written; ${unit} is not running -- it starts on CPUs ${task} at nice ${STRIH_HOUSEKEEPING_NICE}"
    elif _strih_housekeeping_systemctl "$scope" "$user" restart "$unit"; then
      echo "  housekeeping: ${unit} drop-in changed -- restarted onto CPUs ${task} at nice ${STRIH_HOUSEKEEPING_NICE}"
    else
      echo "  WARN: ${unit}: the restart failed -- restart it by hand ($([ "$scope" = user ] && printf 'systemctl --user' || printf 'systemctl') restart ${unit}); verify item 16 FAILs it until then"
    fi
  done
}

# strih_housekeeping_remove USER HOME -> a box with STRIH_HOUSEKEEPING_CPUS=none: our files go (header-
# checked; the conf file is a bare list, always ours by its name), units are reloaded only when a drop-in
# went. The live netdev_budget_usecs is left as it runs (the kernel default comes back at the next boot).
strih_housekeeping_remove() {
  local user="$1" home="$2" f entry scope unit
  f="$(strih_housekeeping_conf_path)"
  [ ! -e "$f" ] || { rm -f "$f" && echo "  housekeeping: removed ${f} (STRIH_HOUSEKEEPING_CPUS=none)"; } || return 1
  for f in "$(strih_housekeeping_sysctl_path)" "$(strih_housekeeping_udev_path)" "$(strih_housekeeping_hook_path)"; do
    [ -e "$f" ] || continue
    if strih_housekeeping_ours "$f"; then rm -f "$f" && echo "  housekeeping: removed ${f}" || return 1
    else echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2; fi
  done
  strih_housekeeping_rebalance_remove || return 1
  for entry in "${STRIH_HOUSEKEEPING_UNITS[@]}"; do
    scope="${entry%%:*}"; unit="${entry#*:}"
    f="$(strih_housekeeping_dropin_path "$scope" "$unit" "$home")"
    [ -e "$f" ] || continue
    if [ -L "${f%/*}" ] || [ -L "$f" ]; then echo "  WARNING: ${f} (or its directory) is a symlink -- left alone" >&2; continue; fi
    if ! strih_housekeeping_ours "$f"; then echo "  WARNING: ${f} is not ours (no camera-box header) -- left alone" >&2; continue; fi
    rm -f "$f" || return 1
    echo "  housekeeping: removed ${f}"
    _strih_housekeeping_systemctl "$scope" "$user" daemon-reload \
      || echo "  WARN: ${unit}: daemon-reload failed after removing its drop-in -- run it by hand"
  done
  return 0
}

# strih_housekeeping_proc_placement PID -> `<Cpus_allowed_list> <nice>` of a running process (rc 1 when
# unreadable). The nice value is /proc/<pid>/stat field 19, read after the last ')' (comm may hold spaces).
strih_housekeeping_proc_placement() {
  local pid="${1-}" proc="${STRIH_HOUSEKEEPING_PROC:-/proc}" cpus stat
  local -a f=()
  case "$pid" in ''|0|*[!0123456789]*) return 1 ;; esac
  cpus="$(awk '/^Cpus_allowed_list:/ { print $2; exit }' "${proc}/${pid}/status" 2>/dev/null || true)"
  stat="$(cat "${proc}/${pid}/stat" 2>/dev/null || true)"
  [ -n "$cpus" ] && [ -n "$stat" ] || return 1
  read -ra f <<<"${stat##*) }"
  [ "${#f[@]}" -ge 17 ] || return 1
  printf '%s %s\n' "$cpus" "${f[16]}"
}

# _strih_housekeeping_unit_row SCOPE UNIT USER HK HOME LABEL REQUIRED -> one `(hk-unit)` row: the drop-in
# as rendered (skipped for the launcher and the sampler, which have none) and the RUNNING main process on HK
# at nice 10.
# REQUIRED=1: a unit that is not running FAILs; 0: a NOTE.
_strih_housekeeping_unit_row() {
  local scope="$1" unit="$2" user="$3" hk="$4" home="$5" label="$6" required="$7" dest pid pl cpus nice
  if [ "$label" != launcher ] && [ "$label" != sampler ]; then
    dest="$(strih_housekeeping_dropin_path "$scope" "$unit" "$home")"
    if ! [ -f "$dest" ] || ! cmp -s "$dest" <(strih_housekeeping_dropin_text "$hk"); then
      bad "(hk-unit) ${unit}: ${dest} missing or drifted -- re-run setup-strih.sh step 11b"
      return 0
    fi
  fi
  pid="$(_strih_housekeeping_systemctl "$scope" "$user" show -p MainPID --value "$unit" 2>/dev/null || true)"
  if ! pl="$(strih_housekeeping_proc_placement "$pid")"; then
    if [ "$required" = 1 ]; then bad "(hk-unit) ${unit}: no running main process to read (MainPID '${pid}')"
    else note "(hk-unit) ${label}: ${unit} is not running -- its placement is graded once it runs"; fi
    return 0
  fi
  cpus="${pl% *}"; nice="${pl#* }"
  if strih_housekeeping_same_cpus "$cpus" "$hk" && [ "$nice" = "$STRIH_HOUSEKEEPING_NICE" ]; then
    ok "(hk-unit) ${label}: pid ${pid} runs on CPUs ${cpus} at nice ${nice}"
  else
    bad "(hk-unit) ${label}: pid ${pid} runs on CPUs ${cpus} at nice ${nice}, want ${hk} at nice ${STRIH_HOUSEKEEPING_NICE} -- restart it ($([ "$scope" = user ] && printf 'systemctl --user' || printf 'systemctl') restart ${unit})"
  fi
}

# strih_housekeeping_slice_row EXCLUDE -> one `(hk-slice)` row: the AllowedCPUs systemd holds for
# rig-background.slice (`systemctl show`, space separated ranges) name no EXCLUDE CPU (the IRQ-only CPUs,
# strih_housekeeping_slice_exclude); unread or empty FAILs. EXCLUDE `none` (a one-CPU set) passes.
strih_housekeeping_slice_row() {
  local irq="${1:-none}" slice allowed exp c
  slice="$(obs_box_background_slice_name)"
  allowed="$(systemctl show -p AllowedCPUs --value "$slice" 2>/dev/null | head -n 1 | tr -s ' ' ',' || true)"
  allowed="${allowed%,}"; allowed="${allowed#,}"
  if ! exp="$(obs_box_cpulist_expand "$allowed")"; then
    bad "(hk-slice) ${slice}: AllowedCPUs unread or empty ('${allowed}') -- re-run setup-strih.sh step 11"
    return 0
  fi
  if [ "$irq" = none ]; then
    ok "(hk-slice) ${slice}: AllowedCPUs ${allowed} (one housekeeping CPU: the rig NIC IRQs share it)"
    return 0
  fi
  for c in $(obs_box_cpulist_expand "$irq" 2>/dev/null || true); do
    case " $exp " in *" $c "*) bad "(hk-slice) ${slice}: AllowedCPUs ${allowed} holds the IRQ-only CPU ${c} -- re-run setup-strih.sh step 11"; return 0 ;; esac
  done
  ok "(hk-slice) ${slice}: AllowedCPUs ${allowed}, off the IRQ-only CPUs ${irq}"
}

# strih_housekeeping_softnet_squeeze FILE CPUS -> `cpu<n>:<time_squeeze>` per CPU of CPUS (space separated)
# from /proc/net/softnet_stat: one row per online CPU, column 3 = time_squeeze (hex), column 13 = the CPU id
# when the kernel prints it (else the row index). rc 1 when the file is unreadable or a CPU has no row.
strih_housekeeping_softnet_squeeze() {
  local file="$1" cpus="$2" line c i=0 id out="" hex='^[0123456789abcdefABCDEF]+$'
  local -a f=()
  local -A sq=()
  [ -r "$file" ] || return 1
  while IFS= read -r line; do
    read -ra f <<<"$line"
    [ "${#f[@]}" -ge 3 ] && [[ ${f[2]} =~ $hex ]] || return 1
    id="$i"
    if [ "${#f[@]}" -ge 13 ]; then [[ ${f[12]} =~ $hex ]] || return 1; id="$((16#${f[12]}))"; fi
    sq[$id]="$((16#${f[2]}))"
    i=$((i + 1))
  done < "$file"
  for c in $(obs_box_cpulist_expand "$cpus" 2>/dev/null || true); do
    [ -n "${sq[$c]+x}" ] || return 1
    out="${out}${out:+ }cpu${c}:${sq[$c]}"
  done
  [ -n "$out" ] || return 1
  printf '%s\n' "$out"
}

# strih_housekeeping_dma_drops NIC -> the NIC's `InDroppedDma` counter from `ethtool -S` (atlantic: the NIC's
# RX ring overflowed, design 7119179275); rc 1 when the driver does not expose it.
strih_housekeeping_dma_drops() {
  local v
  v="$("${ETHTOOL:-ethtool}" -S "$1" 2>/dev/null | awk '$1 == "InDroppedDma:" { print $2; exit }' || true)"
  case "$v" in '' | *[!0123456789]*) return 1 ;; esac
  printf '%s\n' "$v"
}

# strih_housekeeping_ring_row NIC -> the (hk-ring) row: PASS when the RX ring is the driver's pre-set maximum
# (`ethtool -g`), FAIL otherwise, NOTE when the driver exposes no ring. Always rc 0.
strih_housekeeping_ring_row() {
  local nic="$1" g max cur
  g="$("${ETHTOOL:-ethtool}" -g "$nic" 2>/dev/null || true)"
  max="$(printf '%s\n' "$g" | awk '/^Pre-set maximums/ { s = 1 } /^Current hardware settings/ { s = 2 } s == 1 && /^RX:/ { print $2; exit }' || true)"
  cur="$(printf '%s\n' "$g" | awk '/^Current hardware settings/ { s = 1 } s == 1 && /^RX:/ { print $2; exit }' || true)"
  if [ -z "$max" ] || [ -z "$cur" ] || [[ "$max$cur" == *[!0123456789]* ]]; then
    note "(hk-ring) ${nic}: RX ring not readable (the driver offers no ring control)"
  elif [ "$cur" = "$max" ]; then
    ok "(hk-ring) ${nic}: RX ring ${cur} = the driver's pre-set maximum"
  else
    bad "(hk-ring) ${nic}: RX ring ${cur}, the driver's pre-set maximum is ${max} -- systemctl restart strih-nic-irq-affinity.service (off-production: a ring change drops the link once)"
  fi
  return 0
}

# strih_housekeeping_rx_note SYS NIC CPUS -> two REPORT-ONLY NOTEs over ONE sample of STRIH_HOUSEKEEPING_SAMPLE_S
# (default 2 s): `(hk-rx)` = the NIC's rx_missed_errors + rx_dropped increase; `(hk-softnet)` = the
# time_squeeze increase of each housekeeping CPU (the RX softirq ran out of budget) and the NIC's InDroppedDma
# increase. Never a FAIL: the load is traffic-dependent; read them after a production. Always rc 0.
strih_housekeeping_rx_note() {
  local sys="$1" nic="$2" cpus="$3" s="${STRIH_HOUSEKEEPING_SAMPLE_S:-2}" m0 d0 m1 d1 q0 q1 x0 x1 sq="" e a b
  local softnet="${STRIH_HOUSEKEEPING_PROC:-/proc}/net/softnet_stat"
  case "$s" in ''|*[!0123456789]*) s=2 ;; esac
  m0="$(cat "${sys}/class/net/${nic}/statistics/rx_missed_errors" 2>/dev/null || true)"
  d0="$(cat "${sys}/class/net/${nic}/statistics/rx_dropped" 2>/dev/null || true)"
  q0="$(strih_housekeeping_softnet_squeeze "$softnet" "$cpus" || true)"
  x0="$(strih_housekeeping_dma_drops "$nic" || true)"
  sleep "$s"
  m1="$(cat "${sys}/class/net/${nic}/statistics/rx_missed_errors" 2>/dev/null || true)"
  d1="$(cat "${sys}/class/net/${nic}/statistics/rx_dropped" 2>/dev/null || true)"
  q1="$(strih_housekeeping_softnet_squeeze "$softnet" "$cpus" || true)"
  x1="$(strih_housekeeping_dma_drops "$nic" || true)"
  case "$m0$m1$d0$d1" in
    *[!0123456789]* | '') note "(hk-rx) ${nic}: rx_missed_errors / rx_dropped unreadable (report-only)" ;;
    *) note "(hk-rx) ${nic}: rx_missed_errors +$((m1 - m0)), rx_dropped +$((d1 - d0)) over ${s} s, total missed ${m1} (report-only)" ;;
  esac
  if [ -n "$q0" ] && [ -n "$q1" ]; then
    for e in $q1; do
      a=" $q0 "; a="${a#* "${e%%:*}":}"; a="${a%% *}"; b="${e#*:}"
      if [ "$b" -ge "$a" ]; then sq="${sq}${sq:+, }${e%%:*} +$((b - a))"; else sq="${sq}${sq:+, }${e%%:*} wrapped"; fi
    done
    sq="time_squeeze ${sq}"
  else
    sq="time_squeeze unreadable"
  fi
  if [ -n "$x0" ] && [ -n "$x1" ]; then
    note "(hk-softnet) over ${s} s: ${sq}; ${nic} InDroppedDma +$((x1 - x0)) (total ${x1}) (report-only)"
  else
    note "(hk-softnet) over ${s} s: ${sq}; ${nic} InDroppedDma not exposed by ethtool -S (report-only)"
  fi
  return 0
}

# strih_housekeeping_grade_report HOME USER -> verify-strih item 16 on a box with housekeeping CPUs: the
# fact vs the OBS mask, every rig-NIC IRQ on exactly one housekeeping CPU (placement only: the balance is the
# oneshot's, traffic-dependent), the RSS channel count, the softirq budget, rig-background.slice off the IRQ-only
# CPUs, each unit's live placement on the task CPU, the sampler on the OBS mask. rc 0 after printing its rows.
strih_housekeeping_grade_report() {
  local home="${1:?home required}" user="${2:?user required}" hk irq task iso online want sys nic irqs n aff badirq="" okirq=""
  local cur conf live f
  sys="${STRIH_HOUSEKEEPING_SYS:-/sys}"
  hk="$(strih_lx_housekeeping_cpus)" || { bad "(hk-cpus) the box fact STRIH_HOUSEKEEPING_CPUS cannot be read"; return 0; }
  if ! irq="$(strih_housekeeping_irq_cpus "$hk")" || ! task="$(strih_housekeeping_task_cpus "$hk")"; then
    bad "(hk-cpus) STRIH_HOUSEKEEPING_CPUS '${hk}' is not a cpulist -- fix the fact"; return 0
  fi
  # (hk-cpus) the fact = online CPUs minus the OBS mask, and the launcher's file carries it.
  iso="$(head -n 1 "$(strih_housekeeping_isolated_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  online="$(head -n 1 "$(strih_housekeeping_online_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  want="$(obs_box_background_cpus "$online" "$iso" 2>/dev/null || true)"
  conf="$(head -n 1 "$(strih_housekeeping_conf_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -z "$want" ]; then
    bad "(hk-cpus) cannot derive the CPUs outside the OBS mask (online '${online}', $(strih_housekeeping_isolated_path) '${iso}')"
  elif ! strih_housekeeping_same_cpus "$hk" "$want"; then
    bad "(hk-cpus) STRIH_HOUSEKEEPING_CPUS ${hk} is not the complement of the OBS mask ${iso} (online ${online}: ${want}) -- fix the fact"
  elif ! strih_housekeeping_same_cpus "$conf" "$task"; then
    bad "(hk-cpus) $(strih_housekeeping_conf_path) reads '${conf}', not the task CPUs ${task} -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-cpus) housekeeping CPUs ${hk} = online ${online} minus the OBS mask ${iso}: rig NIC IRQs on ${irq}, tasks on ${task}"
  fi
  # (hk-irq) + (hk-rss) every IRQ of the rig NIC on ONE of the IRQ CPUs; RSS queues = their count.
  nic="$(strih_lx_rig_nic "$sys" "${STRIH_HOUSEKEEPING_IPADDR-$(ip -o -4 addr show 2>/dev/null || true)}" "$(strih_lx_ip)" 2>/dev/null || true)"
  if [ -z "$nic" ]; then
    bad "(hk-irq) cannot resolve the rig NIC (set STRIH_NIC_IFACE)"
  elif [ -d "${sys}/class/net/${nic}/device/msi_irqs" ]; then
    irqs="$(for f in "${sys}/class/net/${nic}/device/msi_irqs"/*; do n="${f##*/}"; case "$n" in ''|*[!0123456789]*) ;; *) [ ! -d "${STRIH_HOUSEKEEPING_PROC:-/proc}/irq/${n}" ] || printf '%s\n' "$n" ;; esac; done | sort -n | tr '\n' ' ')"
    cur="$("${ETHTOOL:-ethtool}" -l "$nic" 2>/dev/null | awk '/^Current hardware settings/ { c = 1 } c && /^Combined:/ { print $2; exit }' || true)"
    n="$(strih_housekeeping_count "$irq" || true)"
    if [ -z "$cur" ] || [[ "$cur" == *[!0123456789]* ]]; then note "(hk-rss) ${nic}: RSS channel count not readable (the driver offers no channel control): queues left alone"
    elif [ -n "$n" ] && [ "$cur" = "$n" ]; then ok "(hk-rss) ${nic}: ${cur} RSS queues (combined) = the IRQ CPU count"
    else bad "(hk-rss) ${nic}: ${cur:-unreadable} RSS queues (ethtool -l combined), want ${n:-?} -- systemctl restart strih-nic-irq-affinity.service (off-production: a channel change drops the link once)"; fi
  else
    # a USB rig NIC: its xhci controller IRQ
    f="$(strih_nic_xhci_pci_function "$sys" "$nic" 2>/dev/null || true)"
    irqs=""; [ -z "$f" ] || irqs="$(strih_nic_xhci_irqs "${STRIH_HOUSEKEEPING_PROC:-/proc}/interrupts" "$f" 2>/dev/null | tr '\n' ' ' || true)"
    note "(hk-rss) ${nic} is not a PCIe NIC (no msi_irqs): RSS is not changed, its xhci IRQ is graded"
  fi
  if [ -n "$nic" ]; then
    for n in $irqs; do
      aff="$(tr -d '[:space:]' < "${STRIH_HOUSEKEEPING_PROC:-/proc}/irq/${n}/smp_affinity_list" 2>/dev/null || true)"
      case "$aff" in
        '' | *[!0123456789]*) badirq="${badirq:+$badirq }${n}=${aff:-unreadable}" ;;
        *) case " $(obs_box_cpulist_expand "$irq") " in
             *" $((10#$aff)) "*) okirq="${okirq:+$okirq }${n}->${aff}" ;;
             *) badirq="${badirq:+$badirq }${n}=${aff}" ;;
           esac ;;
      esac
    done
    if [ -z "$irqs" ]; then bad "(hk-irq) ${nic}: no IRQ found (msi_irqs / xhci)"
    elif [ -n "$badirq" ]; then bad "(hk-irq) ${nic}: IRQ(s) not on exactly one housekeeping CPU of ${irq}: ${badirq} -- systemctl restart strih-nic-irq-affinity.service (off-production: a channel change drops the link once)"
    else ok "(hk-irq) ${nic}: every IRQ on one housekeeping CPU of ${irq}: ${okirq}"; fi
  fi
  # (hk-hooks) the udev RSS rule + the NetworkManager re-place hook as rendered.
  f="$(strih_lx_nic_driver 2>/dev/null || true)"
  n="$(strih_housekeeping_count "$irq" 2>/dev/null || true)"
  if [ -z "$f" ] || [ -z "$n" ]; then
    bad "(hk-hooks) cannot render the RSS rule (STRIH_NIC_DRIVER '${f}', count '${n}')"
  elif ! cmp -s "$(strih_housekeeping_udev_path)" <(strih_housekeeping_udev_text "$f" "$n"); then
    bad "(hk-hooks) $(strih_housekeeping_udev_path) missing or drifted -- re-run setup-strih.sh step 11b"
  elif ! [ -x "$(strih_housekeeping_hook_path)" ] || ! cmp -s "$(strih_housekeeping_hook_path)" <(strih_housekeeping_hook_text "$f"); then
    bad "(hk-hooks) $(strih_housekeeping_hook_path) missing, not executable or drifted -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-hooks) the udev RSS rule (${n} channels at add) and the NetworkManager re-place hook are installed"
  fi
  # (hk-ring) the rig NIC's RX ring at the driver's pre-set maximum (the boot oneshot's step 2b).
  [ -z "$nic" ] || strih_housekeeping_ring_row "$nic"
  # (hk-rx) + (hk-softnet) report-only: what the NIC and the RX softirq dropped over one short sample.
  [ -z "$nic" ] || strih_housekeeping_rx_note "$sys" "$nic" "$irq"
  # (hk-sysctl) no budget change (kernel 7.0 refuses < 2000 us): our leftover drop-in FAILs, else the running value.
  f="$(strih_housekeeping_sysctl_path)"
  live="$(head -n 1 "$(strih_housekeeping_budget_proc)" 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -e "$f" ] && strih_housekeeping_ours "$f"; then
    bad "(hk-sysctl) ${f} is a leftover of an earlier setup -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-sysctl) net.core.netdev_budget_usecs = ${live:-unreadable} (left as the kernel runs it: 7.0 at HZ 1000 refuses < 2000)"
  fi
  # (hk-slice) rig-background.slice keeps off the IRQ CPUs (its exact CPUs: the baseline `background` row).
  strih_housekeeping_slice_row "$(strih_housekeeping_slice_exclude "$hk")"
  # (hk-unit) each placed unit's RUNNING process, and the OBS launcher (its shutdown watch), on the task CPU;
  # the program-audio sampler (TEST mode only: down is a NOTE) on the OBS mask, off every IRQ CPU.
  _strih_housekeeping_unit_row system rustdesk.service "$user" "$task" "$home" rustdesk.service 1
  _strih_housekeeping_unit_row user strih-display-watch.service "$user" "$task" "$home" strih-display-watch.service 1
  _strih_housekeeping_unit_row user strih-obs.service "$user" "$task" "$home" launcher 0
  _strih_housekeeping_unit_row user "${STRIH_PROGRAM_AUDIO_UNIT:-program-audio-sampler.service}" "$user" "${iso:-unread}" "$home" sampler 0
  # (hk-rebalance) the delayed re-balance after boot: units as rendered, timer enabled, this boot's run.
  strih_housekeeping_rebalance_row
  return 0
}
