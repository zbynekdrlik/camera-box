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
# What this lib does (setup-strih step 11b, verify-strih item 16):
#   * the rig NIC's IRQs + RSS: the step-11b boot oneshot (strih_nic_irq_affinity_script_text in
#     strih-provision.sh, generalised in place) -- not here;
#   * /etc/strih-housekeeping-cpus.conf: the CPU list, read by strih-obs-start.sh, which puts its own
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
# `none` removes our files (header-checked: a file without our first line is never touched).
#
# Seams (tests): STRIH_HOUSEKEEPING_ROOT (prefix of /etc and /proc/sys), STRIH_HOUSEKEEPING_OWNER (the
# root files' owner), STRIH_HOUSEKEEPING_SYS (/sys), STRIH_HOUSEKEEPING_PROC (/proc for pids),
# STRIH_HOUSEKEEPING_IPADDR (the `ip -o -4 addr show` text), STRIH_HOUSEKEEPING_SAMPLE_S (the (hk-rx)
# sample, default 2 s), ETHTOOL. systemctl / udevadm from PATH.
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
  local user="${1:?user required}" home="${2:?home required}" hk owner rc entry scope unit dest live proc drv
  owner="${STRIH_HOUSEKEEPING_OWNER:-root:root}"
  hk="$(strih_lx_housekeeping_cpus)" || { echo "housekeeping: the box fact STRIH_HOUSEKEEPING_CPUS cannot be read" >&2; return 1; }
  if [ "$hk" = none ]; then
    strih_housekeeping_remove "$user" "$home"
    return
  fi
  strih_housekeeping_count "$hk" >/dev/null || { echo "housekeeping: STRIH_HOUSEKEEPING_CPUS '${hk}' is not a cpulist" >&2; return 1; }
  echo "  housekeeping CPUs ${hk} (STRIH_HOUSEKEEPING_CPUS): the rig NIC IRQs + RSS (the oneshot below), rustdesk, the display watch, the OBS launcher"
  dest="$(strih_housekeeping_conf_path)"
  install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
  rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_conf_text "$hk" || rc=$?
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
  rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_udev_text "$drv" "$(strih_housekeeping_count "$hk")" || rc=$?
  [ "$rc" != 1 ] || return 1
  [ "$rc" != 0 ] || udevadm control --reload-rules \
    || echo "  WARN: udevadm control --reload-rules failed -- the RSS rule applies at the next boot"
  dest="$(strih_housekeeping_hook_path)"
  install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
  rc=0; _strih_housekeeping_put "$dest" 0755 "$owner" housekeeping strih_housekeeping_hook_text "$drv" || rc=$?
  [ "$rc" != 1 ] || return 1

  for entry in "${STRIH_HOUSEKEEPING_UNITS[@]}"; do
    scope="${entry%%:*}"; unit="${entry#*:}"
    dest="$(strih_housekeeping_dropin_path "$scope" "$unit" "$home")"
    if [ "$scope" = user ]; then
      strih_user_home_dir "$home" "$user" "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
      rc=0; _strih_housekeeping_put "$dest" 0644 "${user}:$(id -gn "$user" 2>/dev/null || printf '%s' "$user")" housekeeping strih_housekeeping_dropin_text "$hk" || rc=$?
    else
      install -d -m 0755 "${dest%/*}" || { echo "housekeeping: cannot create ${dest%/*}" >&2; return 1; }
      rc=0; _strih_housekeeping_put "$dest" 0644 "$owner" housekeeping strih_housekeeping_dropin_text "$hk" || rc=$?
    fi
    [ "$rc" != 1 ] || return 1
    [ "$rc" = 0 ] || continue
    # a unit that is not running (or not installed yet: rustdesk is step 16b, the display watch 16d) is
    # never started; it takes the drop-in when it starts.
    if ! _strih_housekeeping_systemctl "$scope" "$user" daemon-reload; then
      echo "  WARN: ${unit}: daemon-reload failed -- run it by hand, then restart ${unit}; verify item 16 FAILs it until then"
    elif ! _strih_housekeeping_systemctl "$scope" "$user" is-active -q "$unit"; then
      echo "  housekeeping: ${unit} drop-in written; ${unit} is not running -- it starts on CPUs ${hk} at nice ${STRIH_HOUSEKEEPING_NICE}"
    elif _strih_housekeeping_systemctl "$scope" "$user" restart "$unit"; then
      echo "  housekeeping: ${unit} drop-in changed -- restarted onto CPUs ${hk} at nice ${STRIH_HOUSEKEEPING_NICE}"
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
# as rendered (skipped for the launcher, which has none) and the RUNNING main process on HK at nice 10.
# REQUIRED=1: a unit that is not running FAILs; 0: a NOTE.
_strih_housekeeping_unit_row() {
  local scope="$1" unit="$2" user="$3" hk="$4" home="$5" label="$6" required="$7" dest pid pl cpus nice
  if [ "$label" != launcher ]; then
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

# strih_housekeeping_rx_note SYS NIC -> one REPORT-ONLY `(hk-rx)` NOTE: the NIC's rx_missed_errors +
# rx_dropped increase over STRIH_HOUSEKEEPING_SAMPLE_S (default 2 s). Never a FAIL: read it before/after
# the RSS change and after a production. Always rc 0.
strih_housekeeping_rx_note() {
  local sys="$1" nic="$2" s="${STRIH_HOUSEKEEPING_SAMPLE_S:-2}" m0 d0 m1 d1
  case "$s" in ''|*[!0123456789]*) s=2 ;; esac
  m0="$(cat "${sys}/class/net/${nic}/statistics/rx_missed_errors" 2>/dev/null || true)"
  d0="$(cat "${sys}/class/net/${nic}/statistics/rx_dropped" 2>/dev/null || true)"
  sleep "$s"
  m1="$(cat "${sys}/class/net/${nic}/statistics/rx_missed_errors" 2>/dev/null || true)"
  d1="$(cat "${sys}/class/net/${nic}/statistics/rx_dropped" 2>/dev/null || true)"
  case "$m0$m1$d0$d1" in
    *[!0123456789]* | '') note "(hk-rx) ${nic}: rx_missed_errors / rx_dropped unreadable (report-only)" ;;
    *) note "(hk-rx) ${nic}: rx_missed_errors +$((m1 - m0)), rx_dropped +$((d1 - d0)) over ${s} s, total missed ${m1} (report-only)" ;;
  esac
  return 0
}

# strih_housekeeping_grade_report HOME USER -> verify-strih item 16 on a box with housekeeping CPUs: the
# fact vs the OBS mask, every rig-NIC IRQ, the RSS channel count, the softirq budget, each unit's live
# placement. rc 0 after printing its rows.
strih_housekeeping_grade_report() {
  local home="${1:?home required}" user="${2:?user required}" hk iso online want sys nic irqs n aff badirq="" okirq=""
  local cur conf live f
  sys="${STRIH_HOUSEKEEPING_SYS:-/sys}"
  hk="$(strih_lx_housekeeping_cpus)" || { bad "(hk-cpus) the box fact STRIH_HOUSEKEEPING_CPUS cannot be read"; return 0; }
  # (hk-cpus) the fact = online CPUs minus the OBS mask, and the launcher's file carries it.
  iso="$(head -n 1 "$(strih_housekeeping_isolated_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  online="$(head -n 1 "$(strih_housekeeping_online_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  want="$(obs_box_background_cpus "$online" "$iso" 2>/dev/null || true)"
  conf="$(head -n 1 "$(strih_housekeeping_conf_path)" 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -z "$want" ]; then
    bad "(hk-cpus) cannot derive the CPUs outside the OBS mask (online '${online}', $(strih_housekeeping_isolated_path) '${iso}')"
  elif ! strih_housekeeping_same_cpus "$hk" "$want"; then
    bad "(hk-cpus) STRIH_HOUSEKEEPING_CPUS ${hk} is not the complement of the OBS mask ${iso} (online ${online}: ${want}) -- fix the fact"
  elif ! strih_housekeeping_same_cpus "$conf" "$hk"; then
    bad "(hk-cpus) $(strih_housekeeping_conf_path) reads '${conf}', not ${hk} -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-cpus) housekeeping CPUs ${hk} = online ${online} minus the OBS mask ${iso}"
  fi
  # (hk-irq) + (hk-rss) every IRQ of the rig NIC on the housekeeping CPUs; RSS queues = their count.
  nic="$(strih_lx_rig_nic "$sys" "${STRIH_HOUSEKEEPING_IPADDR-$(ip -o -4 addr show 2>/dev/null || true)}" "$(strih_lx_ip)" 2>/dev/null || true)"
  if [ -z "$nic" ]; then
    bad "(hk-irq) cannot resolve the rig NIC (set STRIH_NIC_IFACE)"
  elif [ -d "${sys}/class/net/${nic}/device/msi_irqs" ]; then
    irqs="$(for f in "${sys}/class/net/${nic}/device/msi_irqs"/*; do n="${f##*/}"; case "$n" in ''|*[!0123456789]*) ;; *) [ ! -d "${STRIH_HOUSEKEEPING_PROC:-/proc}/irq/${n}" ] || printf '%s\n' "$n" ;; esac; done | sort -n | tr '\n' ' ')"
    cur="$("${ETHTOOL:-ethtool}" -l "$nic" 2>/dev/null | awk '/^Current hardware settings/ { c = 1 } c && /^Combined:/ { print $2; exit }' || true)"
    n="$(strih_housekeeping_count "$hk" || true)"
    if [ -z "$cur" ] || [[ "$cur" == *[!0123456789]* ]]; then note "(hk-rss) ${nic}: RSS channel count not readable (the driver offers no channel control): queues left alone"
    elif [ -n "$n" ] && [ "$cur" = "$n" ]; then ok "(hk-rss) ${nic}: ${cur} RSS queues (combined) = the housekeeping CPU count"
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
      if strih_housekeeping_same_cpus "$aff" "$hk"; then okirq="${okirq:+$okirq }${n}"; else badirq="${badirq:+$badirq }${n}=${aff:-unreadable}"; fi
    done
    if [ -z "$irqs" ]; then bad "(hk-irq) ${nic}: no IRQ found (msi_irqs / xhci)"
    elif [ -n "$badirq" ]; then bad "(hk-irq) ${nic}: IRQ(s) off the housekeeping CPUs ${hk}: ${badirq} -- systemctl restart strih-nic-irq-affinity.service (off-production: a channel change drops the link once)"
    else ok "(hk-irq) ${nic}: IRQ(s) ${okirq} on CPUs ${hk}"; fi
  fi
  # (hk-hooks) the udev RSS rule + the NetworkManager re-place hook as rendered.
  f="$(strih_lx_nic_driver 2>/dev/null || true)"
  n="$(strih_housekeeping_count "$hk" 2>/dev/null || true)"
  if [ -z "$f" ] || [ -z "$n" ]; then
    bad "(hk-hooks) cannot render the RSS rule (STRIH_NIC_DRIVER '${f}', count '${n}')"
  elif ! cmp -s "$(strih_housekeeping_udev_path)" <(strih_housekeeping_udev_text "$f" "$n"); then
    bad "(hk-hooks) $(strih_housekeeping_udev_path) missing or drifted -- re-run setup-strih.sh step 11b"
  elif ! [ -x "$(strih_housekeeping_hook_path)" ] || ! cmp -s "$(strih_housekeeping_hook_path)" <(strih_housekeeping_hook_text "$f"); then
    bad "(hk-hooks) $(strih_housekeeping_hook_path) missing, not executable or drifted -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-hooks) the udev RSS rule (${n} channels at add) and the NetworkManager re-place hook are installed"
  fi
  # (hk-rx) report-only: what the NIC dropped over a short sample (the 2-queue RSS has to carry the rig load).
  [ -z "$nic" ] || strih_housekeeping_rx_note "$sys" "$nic"
  # (hk-sysctl) no budget change (kernel 7.0 refuses < 2000 us): our leftover drop-in FAILs, else the running value.
  f="$(strih_housekeeping_sysctl_path)"
  live="$(head -n 1 "$(strih_housekeeping_budget_proc)" 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -e "$f" ] && strih_housekeeping_ours "$f"; then
    bad "(hk-sysctl) ${f} is a leftover of an earlier setup -- re-run setup-strih.sh step 11b"
  else
    ok "(hk-sysctl) net.core.netdev_budget_usecs = ${live:-unreadable} (left as the kernel runs it: 7.0 at HZ 1000 refuses < 2000)"
  fi
  # (hk-unit) each placed unit's RUNNING process, and the OBS launcher (its shutdown watch).
  _strih_housekeeping_unit_row system rustdesk.service "$user" "$hk" "$home" rustdesk.service 1
  _strih_housekeeping_unit_row user strih-display-watch.service "$user" "$hk" "$home" strih-display-watch.service 1
  _strih_housekeeping_unit_row user strih-obs.service "$user" "$hk" "$home" launcher 0
  return 0
}
