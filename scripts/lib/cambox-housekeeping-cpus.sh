#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure derivation/gather/verdict/program builders; the only
# top-level statements are the lazy source of ro-window.sh and one overridable constant) --
# deliberately NOT `set -euo pipefail`: sourcing runs this file in the CALLER's shell, so strict mode
# here would leak into every caller (the scripts/lib convention, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/cambox-housekeeping-cpus.sh -- systemd PID 1 and every service OFF the capture-IRQ core
# (issue 1367, design 6180767397).
#
# The issue-1198 core split on a cambox: capture+emit thread on the isolated core (isolcpus), the
# xhci capture IRQ on ONE reserved general core (the highest non-capture core,
# src/affinity.rs select_irq_reserved_core), camera-box's painter/display threads on the rest. That
# reservation only covers camera-box's OWN threads. systemd PID 1 and every unit it starts still ran on
# the reserved IRQ core, and the work PID 1 does on a unit start delayed the URB completion there:
# measured live on cam5 (15.10.2026), 65 of 102 capture-phase re-seeds landed within 1 s of a unit
# start (chance 7.8 %), mostly the 2-min cambox-mgmt-selfcheck timer and the looping
# systemd-journal-upload. With PID 1 restricted to the other general cores: 2 re-seeds in 9 min, none
# near a unit start.
#
# The fix is ONE systemd manager setting: `[Manager] CPUAffinity=<housekeeping>` in a system.conf.d
# drop-in. PID 1 runs there (from the next boot; a live box gets it through taskset) and every unit
# inherits it by default. camera-box keeps its own
# `CPUAffinity=` drop-in + per-thread sched_setaffinity (a soft mask, never a cpuset), and the burn
# units pass their own --property=CPUAffinity, so the capture path is untouched.
#
# housekeeping = online - every isolated core - the reserved IRQ core (the highest remaining core,
# affinity.rs select_irq_reserved_core). The fleet runs ONE isolated core, so this is affinity.rs's
# split; with several isolated cores it also keeps housekeeping off the spare isolated ones. With no
# isolated core, fewer than 2 general cores, or a PREEMPT_RT kernel (affinity.rs then co-locates the
# capture IRQ on the capture core and reserves no general core), this writes nothing. On a fresh box
# the running kernel has no isolcpus yet (setup-device STEP 10 only wrote GRUB), so the install reads
# the isolated set from that GRUB file when sysfs has none.
#
# Consumers:
#   - setup-device.sh `[housekeeping-cpus]` (rw window, takes effect at the next
#     boot; the apply program sets a live box) -- cambox_hk_install_dropin "";
#   - verify-device.sh (au): cambox_hk_gather_remote_snippet (read-only, over ssh) +
#     cambox_hk_verdict (pure);
#   - a live box: cambox_hk_apply_program, piped into `ssh root@<box> bash -s` while no E2E holds the
#     rig lease. It writes inside ONE rw window closed by the ONE verified close
#     (scripts/lib/ro-window.sh, issue 1407), then sets PID 1's mask with taskset (no reboot) and
#     reads it back.
# Tests: tests/python/test_cambox_housekeeping_cpus_1367.py.

if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/ro-window.sh
  . "${BASH_SOURCE[0]%/*}/ro-window.sh"
fi

# The drop-in on the box. Overridable so a test can point the gather and the program at a temp tree.
CAMBOX_HK_DROPIN="${CAMBOX_HK_DROPIN:-/etc/systemd/system.conf.d/60-camera-box-housekeeping-cpus.conf}"

# cambox_hk_cpulist_expand LIST -> the cpus of a kernel cpulist ("0-2,5") one per line, sorted,
# unique. Anything that is not a cpulist prints nothing and returns 1.
cambox_hk_cpulist_expand() {
  local list="${1//[[:space:]]/}" part lo hi c
  local -a _hk_out=()
  [ -n "$list" ] || return 0
  local IFS=,
  for part in $list; do
    case "$part" in
      *[!0123456789-]* | '' | -* | *- | *-*-*) return 1 ;;
      *-*)
        lo="${part%-*}" hi="${part#*-}"
        [ "$((10#$lo))" -le "$((10#$hi))" ] || return 1
        for ((c = 10#$lo; c <= 10#$hi; c++)); do _hk_out+=("$c"); done
        ;;
      *) _hk_out+=("$((10#$part))") ;;
    esac
  done
  printf '%s\n' "${_hk_out[@]}" | sort -n -u
}

# cambox_hk_cpus ONLINE ISOLATED [RT] -> the housekeeping cpus as a comma list ("0,1"), rc 0. rc 1
# with a reason on stderr when there is no reserved IRQ core (RT=1: a PREEMPT_RT kernel, no isolated
# core, fewer than 2 general cores, or an unreadable list): then nothing is provisioned, like
# affinity.rs.
cambox_hk_cpus() {
  local online iso c
  local -a _hk_general=() _hk_cores=()
  if [ "${3:-0}" = 1 ]; then
    echo "a PREEMPT_RT kernel: the capture IRQ shares the capture core, no reserved capture-IRQ core" >&2
    return 1
  fi
  if ! online="$(cambox_hk_cpulist_expand "${1:-}")" || [ -z "$online" ]; then
    echo "no readable online cpu list ('${1:-}')" >&2
    return 1
  fi
  if ! iso="$(cambox_hk_cpulist_expand "${2:-}")"; then
    echo "unreadable isolated cpu list ('${2:-}')" >&2
    return 1
  fi
  if [ -z "$iso" ]; then
    echo "no isolated capture core, so no reserved capture-IRQ core" >&2
    return 1
  fi
  for c in $online; do
    grep -qx "$c" <<<"$iso" || _hk_general+=("$c")
  done
  if [ "${#_hk_general[@]}" -lt 2 ]; then
    echo "fewer than 2 general cores (${_hk_general[*]:-none}), so no reserved capture-IRQ core" >&2
    return 1
  fi
  # general is sorted ascending: the last one is the reserved IRQ core, the rest housekeeping.
  _hk_cores=("${_hk_general[@]:0:${#_hk_general[@]}-1}")
  local IFS=,
  printf '%s\n' "${_hk_cores[*]}"
}

# cambox_hk_isolated_from_grub FILE -> the isolcpus= cpu list of the last active GRUB_CMDLINE_LINUX*
# line in FILE (flags such as managed_irq,domain dropped), or nothing. Comment lines never count.
cambox_hk_isolated_from_grub() {
  local file="${1:-}" line tok val="" item list=""
  [ -r "$file" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      GRUB_CMDLINE_LINUX=* | GRUB_CMDLINE_LINUX_DEFAULT=*) ;;
      *) continue ;;
    esac
    line="${line#*=}"
    line="${line//\"/}"
    line="${line//\'/}"
    for tok in $line; do
      case "$tok" in isolcpus=*) val="${tok#isolcpus=}" ;; esac
    done
  done <"$file"
  [ -n "$val" ] || return 0
  local IFS=,
  for item in $val; do
    case "$item" in *[!0123456789-]* | '') continue ;; esac
    list="${list:+$list,}$item"
  done
  printf '%s\n' "$list"
}

# cambox_hk_kernel_rt -> 1 on a PREEMPT_RT kernel (/sys/kernel/realtime = 1, or uname -v names
# PREEMPT_RT, the affinity.rs kernel_is_preempt_rt rule), else 0.
cambox_hk_kernel_rt() {
  local flag
  flag="$(cat "${CAMBOX_HK_REALTIME:-/sys/kernel/realtime}" 2>/dev/null)" || flag=""
  [ "${flag//[[:space:]]/}" = 1 ] && { echo 1; return 0; }
  case "$(uname -v 2>/dev/null)" in *PREEMPT_RT*) echo 1 ;; *) echo 0 ;; esac
}

# cambox_hk_dropin_content HK -> the drop-in, byte-for-byte (setup-device and the apply write exactly
# this; verify-device compares it).
cambox_hk_dropin_content() {
  cat <<EOF
# camera-box issue 1367: systemd PID 1 and every unit off the capture core AND the reserved
# capture-IRQ core. Written through scripts/lib/cambox-housekeeping-cpus.sh; graded by
# verify-device.sh (au). camera-box keeps its own CPUAffinity drop-in.
[Manager]
CPUAffinity=${1}
EOF
}

# cambox_hk_install_dropin ROOT -> derives the housekeeping set from THIS box's
# /sys/devices/system/cpu/{online,isolated} and writes ROOT$CAMBOX_HK_DROPIN through a temp file +
# rename. With no reserved IRQ core it writes nothing, says why, and returns 0. Non-zero only on a
# failed write. Test seams: CAMBOX_HK_SYS (the cpu sysfs dir), CAMBOX_HK_GRUB (the GRUB defaults
# file), CAMBOX_HK_REALTIME (/sys/kernel/realtime) and, for the apply program, CAMBOX_HK_PROC (the
# /proc root it reads PID 1's mask from).
cambox_hk_install_dropin() {
  local root="${1:-}" sys="${CAMBOX_HK_SYS:-/sys/devices/system/cpu}" path hk why iso
  path="${root}${CAMBOX_HK_DROPIN}"
  iso="$(cat "$sys/isolated" 2>/dev/null)" || iso=""
  iso="${iso//[[:space:]]/}"
  # A fresh box: STEP 10 wrote isolcpus into GRUB, the running kernel has none until the reboot.
  [ -n "$iso" ] || iso="$(cambox_hk_isolated_from_grub "${CAMBOX_HK_GRUB:-${root}/etc/default/grub}")"
  if ! hk="$(cambox_hk_cpus "$(cat "$sys/online" 2>/dev/null)" "$iso" "$(cambox_hk_kernel_rt)" 2>&1)"; then
    why="$hk"
    if [ -e "$path" ]; then
      rm -f "$path" || { echo "FAIL: [issue 1367] could not remove the leftover ${path}" >&2; return 1; }
      echo "  housekeeping cpus: removed the leftover ${path} -- ${why} (issue 1367)"
      return 0
    fi
    echo "  housekeeping cpus: not written -- ${why} (issue 1367)"
    return 0
  fi
  if mkdir -p "${path%/*}" && cambox_hk_dropin_content "$hk" >"$path.new" && chmod 0644 "$path.new" \
    && mv -f "$path.new" "$path"; then
    echo "  housekeeping cpus: ${path} CPUAffinity=${hk} (PID 1 + every unit off the capture core and the capture-IRQ core)"
    return 0
  fi
  rm -f "$path.new"
  echo "FAIL: [issue 1367] could not write ${path}" >&2
  return 1
}

# cambox_hk_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh (au): one
# KEY=VALUE line per fact, read-only, the drop-in's lines as DROPIN= lines. The last line is
# GATHER_DONE=1, so a cut read is told apart.
cambox_hk_gather_remote_snippet() {
  printf '_hk_d=%q\n' "$CAMBOX_HK_DROPIN"
  printf '_hk_sample=%q\n' "${CAMBOX_HK_IRQ_SAMPLE_S:-2}"
  cat <<'REMOTE'
echo "ONLINE=$(cat /sys/devices/system/cpu/online 2>/dev/null)"
echo "ISOLATED=$(cat /sys/devices/system/cpu/isolated 2>/dev/null)"
_hk_rtf="$(cat /sys/kernel/realtime 2>/dev/null)"; case "${_hk_rtf//[[:space:]]/}|$(uname -v 2>/dev/null)" in 1\|* | *PREEMPT_RT*) echo "RT=1" ;; *) echo "RT=0" ;; esac
echo "PID1=$(awk '/^Cpus_allowed_list:/ {print $2; exit}' /proc/1/status 2>/dev/null)"
_hk_i0="$(cat /proc/interrupts 2>/dev/null)"
sleep "${_hk_sample:-2}"
_hk_i1="$(cat /proc/interrupts 2>/dev/null)"
printf '%s\n' "$_hk_i0" | awk -v later="$_hk_i1" '
  BEGIN {
    n = split(later, L, "\n")
    for (i = 1; i <= n; i++) {
      split(L[i], f, " "); k = f[1]; sub(/:$/, "", k)
      if (k ~ /^[0-9]+$/) after[k] = L[i]
    }
  }
  NR == 1 { ncpu = NF; next }
  {
    k = $1; sub(/:$/, "", k)
    if (k !~ /^[0-9]+$/ || !(k in after)) next
    split(after[k], g, " ")
    d = ""
    for (c = 2; c <= ncpu + 1; c++) { x = g[c] - $c; if (x < 0) x = 0; d = d (c > 2 ? "," : "") x }
    cap = ($0 ~ /xhci|ehci|ohci|uvcvideo/) ? 1 : 0
    aff = ""
    getline aff < ("/proc/irq/" k "/smp_affinity_list")
    close("/proc/irq/" k "/smp_affinity_list")
    print "IRQ=" k " " cap " " aff " " d
  }'

if [ -e "$_hk_d" ]; then echo "DROPIN_PRESENT=1"; if [ -n "$(tail -c1 "$_hk_d" 2>/dev/null)" ]; then echo "DROPIN_NL=0"; else echo "DROPIN_NL=1"; fi; while IFS= read -r _hk_l || [ -n "$_hk_l" ]; do echo "DROPIN=$_hk_l"; done <"$_hk_d"; else echo "DROPIN_PRESENT=0"; fi
echo "GATHER_DONE=1"
REMOTE
}

# cambox_hk_verdict BLOCK -> `ok`, `ok-none: <why>` (no reserved IRQ core on this box), or one
# `FAIL: ...` line per broken facet. Fail-closed: a missing or unreadable fact is a FAIL. Pure bash.
#   - the drop-in is byte-identical to cambox_hk_dropin_content <housekeeping>;
#   - PID 1's running mask equals the housekeeping set (the drop-in applies at boot, the apply program sets a live box).
cambox_hk_verdict() {
  local block="${1:-}" line online="" iso="" rt="" nl_ok="" irqs="" pid1="" present="" done="" body="" nl=$'\n'
  local hk why want got have fails="" fix
  while IFS= read -r line; do
    case "$line" in
      ONLINE=*) online="${line#*=}" ;;
      ISOLATED=*) iso="${line#*=}" ;;
      RT=*) rt="${line#*=}" ;;
      PID1=*) pid1="${line#*=}" ;;
      DROPIN_PRESENT=*) present="${line#*=}" ;;
      DROPIN_NL=*) nl_ok="${line#*=}" ;;
      DROPIN=*) body="${body}${line#DROPIN=}${nl}" ;;
      IRQ=*) irqs="${irqs}${line#IRQ=}${nl}" ;;
      GATHER_DONE=1) done=1 ;;
    esac
  done <<<"$block"
  fix="fix: re-run setup-device.sh, or pipe the cambox_hk_apply_program of scripts/lib/cambox-housekeeping-cpus.sh into 'ssh root@<box> bash -s' while no E2E holds the rig (issue 1367)"
  if [ "$done" != 1 ]; then
    printf 'FAIL: the cpu state could not be read (no GATHER_DONE) -- %s\n' "$fix"
    return 0
  fi
  if ! hk="$(cambox_hk_cpus "$online" "$iso" "$rt" 2>&1)"; then
    why="$hk"
    if [ "$present" = 1 ]; then
      printf 'FAIL: %s carries a housekeeping CPUAffinity, but this box has %s -- remove the drop-in\n' "$CAMBOX_HK_DROPIN" "$why"
    else
      printf 'ok-none: %s\n' "$why"
    fi
    return 0
  fi
  want="$(cambox_hk_dropin_content "$hk")${nl}"
  if [ "$present" != 1 ]; then
    fails="${fails}FAIL: ${CAMBOX_HK_DROPIN} is missing -- PID 1 and every unit may run on the capture-IRQ core; ${fix}${nl}"
  elif [ "$body" != "$want" ] || [ "$nl_ok" = 0 ]; then
    fails="${fails}FAIL: ${CAMBOX_HK_DROPIN} differs from scripts/lib/cambox-housekeeping-cpus.sh (want CPUAffinity=${hk}); ${fix}${nl}"
  fi
  if ! got="$(cambox_hk_cpulist_expand "$pid1")" || [ -z "$got" ]; then
    fails="${fails}FAIL: PID 1's cpu mask could not be read ('${pid1}') -- cannot certify the housekeeping cores${nl}"
  else
    have="$(cambox_hk_cpulist_expand "$hk")"
    if [ "$got" != "$have" ]; then
      fails="${fails}FAIL: PID 1 runs on cpus ${pid1}, not the housekeeping set ${hk} -- PID 1 still has its old mask (the apply program sets it with taskset; the drop-in applies at the next boot); ${fix}${nl}"
    fi
  fi
  # issue 1367: no OTHER device IRQ may still run on the reserved capture-IRQ core (the NIC IRQ sat
  # there on cam3/cam5); camera-box's setup_irq_affinity moves them at every start.
  local reserved irq_n irq_c irq_a irq_d on_core="" fired
  local -a _hk_dl=()
  reserved="$(cambox_hk_cpulist_expand "$online" | while read -r c; do grep -qx "$c" <<<"$(cambox_hk_cpulist_expand "$iso")" || echo "$c"; done | tail -1)"
  if [ -n "$reserved" ]; then
    while read -r irq_n irq_c irq_a irq_d; do
      [ -n "$irq_n" ] && [ "$irq_c" = 0 ] || continue
      IFS=, read -r -a _hk_dl <<<"$irq_d"
      fired="${_hk_dl[$reserved]:-0}"
      case "$fired" in '' | *[!0123456789]*) fired=0 ;; esac
      [ "$fired" -gt 0 ] && on_core="${on_core:+$on_core, }IRQ $irq_n (cpus $irq_a, $fired interrupts in the sample)"
    done <<<"$irqs"
    [ -z "$on_core" ] || fails="${fails}FAIL: ${on_core} fired on the reserved capture-IRQ core ${reserved} -- camera-box's IRQ setup (issue 1367) has not moved it; restart camera-box on a build that carries it${nl}"
  fi
  if [ -n "$fails" ]; then
    printf '%s' "$fails"
  else
    printf 'ok\n'
  fi
}

# cambox_hk_apply_program -> the REMOTE program (root, `ssh ... bash -s`) that puts a live cambox's
# PID 1 on the housekeeping cores. Everything runs inside ONE function called with </dev/null (the
# program arrives on bash's stdin):
#   1. read (nothing written): the root mode, the cpu lists, the drop-in;
#   2. the rw window, when the drop-in differs OR the root does not read `ro`: the drop-in (temp file
#      + rename), then the ONE verified close. A failure inside reaches the EXIT trap, which runs the
#      same close;
#   3. after the close: `taskset -pc <set> 1` when PID 1's mask differs, then read it back. systemd
#      applies the drop-in to itself only at boot (systemd 255 skips it on a daemon-reexec, checked on
#      cam2 15.10.2026); every unit started afterwards inherits PID 1's mask. Exit 0 only when PID 1
#      runs on exactly the housekeeping set (or the box has no reserved IRQ core).
cambox_hk_apply_program() {
  cat <<'PROG'
# scripts/lib/cambox-housekeeping-cpus.sh (issue 1367): PID 1 off the capture-IRQ core.
_hk_main() {
set -eu
PROG
  printf '_hk_d=%q\n' "$CAMBOX_HK_DROPIN"
  printf '_hk_sys=%q\n' "${CAMBOX_HK_SYS:-/sys/devices/system/cpu}"
  printf '_hk_proc=%q\n' "${CAMBOX_HK_PROC:-/proc}"
  printf '%s\n' "$(declare -f ro_root_mount_mode cambox_hk_cpulist_expand cambox_hk_cpus cambox_hk_kernel_rt cambox_hk_dropin_content)"
  cat <<'PROG'
_hk_box="$(hostname 2>/dev/null || echo this cambox)"
# --- 1. what this box needs (read before anything is written) ---
_hk_mode="$(ro_root_mount_mode "$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)")"
_hk_none=0 _hk_do_d=0 _hk_do_rm=0
if ! _hk_set="$(cambox_hk_cpus "$(cat "$_hk_sys/online" 2>/dev/null)" "$(cat "$_hk_sys/isolated" 2>/dev/null)" "$(cambox_hk_kernel_rt)" 2>&1)"; then
  # No reserved IRQ core: nothing to provision, but a leftover drop-in (e.g. after a move to a
  # PREEMPT_RT kernel) is removed, or verify (au) fails it.
  _hk_none=1
  if [ ! -e "$_hk_d" ]; then
    echo "OK ($_hk_box): nothing to do -- $_hk_set (issue 1367)"
    return 0
  fi
  _hk_do_rm=1
  echo "PLAN ($_hk_box): root $_hk_mode; $_hk_set; drop-in remove"
else
  _hk_body="$(cambox_hk_dropin_content "$_hk_set")"
  # exact bytes: the verify compares the file byte for byte, so a trailing blank line is a rewrite too
  if ! cmp -s "$_hk_d" <(printf '%s\n' "$_hk_body"); then _hk_do_d=1; fi
  echo "PLAN ($_hk_box): root $_hk_mode; housekeeping CPUAffinity=$_hk_set; drop-in $([ "$_hk_do_d" = 1 ] && echo write || echo in-place)"
fi
_hk_rc=0
# --- 2. the rw window, closed by the ONE verified close ---
_hk_open=0
_hk_close() {
  [ "$_hk_open" = 1 ] || return 0
  _hk_open=0
PROG
  ro_window_close_cmds "issue 1367" "\$_hk_box" \
    "The drop-in is on disk, but the root stays read-WRITE, so PID 1's mask is not changed." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run the cambox housekeeping-cpus apply for this box (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_hk_close' EXIT
if [ "$_hk_do_d" = 1 ] || [ "$_hk_do_rm" = 1 ] || [ "$_hk_mode" != ro ]; then
  if ! mount -o remount,rw /; then
    echo "FAIL: [issue 1367] could not remount / read-write on $_hk_box -- nothing was written" >&2
    exit 1
  fi
  _hk_open=1
  if [ "$_hk_do_d" = 1 ]; then
    if ! { mkdir -p "${_hk_d%/*}" && printf '%s\n' "$_hk_body" >"$_hk_d.new" && chmod 0644 "$_hk_d.new" && mv -f "$_hk_d.new" "$_hk_d"; }; then
      rm -f "$_hk_d.new"
      echo "FAIL: [issue 1367] could not write $_hk_d on $_hk_box -- the window is closed again by the verified close" >&2
      exit 1
    fi
  fi
  if [ "$_hk_do_rm" = 1 ] && ! rm -f "$_hk_d"; then
    echo "FAIL: [issue 1367] could not remove $_hk_d on $_hk_box -- the window is closed again by the verified close" >&2
    exit 1
  fi
fi
_hk_close
trap - EXIT
if [ "$_hk_none" = 1 ]; then
  echo "OK ($_hk_box): removed the leftover $_hk_d -- $_hk_set (PID 1 keeps its mask until the next boot; issue 1367)"
  return 0
fi
# --- 3. after the verified close: set PID 1's mask when it differs (the drop-in applies it at boot;
#        systemd 255 does not re-apply CPUAffinity to itself on daemon-reexec, checked on cam2), read back ---
_hk_want="$(cambox_hk_cpulist_expand "$_hk_set")"
_hk_pid1() { cambox_hk_cpulist_expand "$(awk '/^Cpus_allowed_list:/ {print $2; exit}' "$_hk_proc/1/status" 2>/dev/null)" 2>/dev/null || true; }
if [ "$(_hk_pid1)" != "$_hk_want" ]; then
  if ! taskset -pc "$_hk_set" 1 >/dev/null; then
    echo "FAIL: [issue 1367] taskset -pc $_hk_set 1 failed on $_hk_box" >&2
    _hk_rc=1
  fi
fi
_hk_now="$(_hk_pid1 | paste -sd, -)"
if [ "$(_hk_pid1)" = "$_hk_want" ]; then
  echo "OK ($_hk_box): PID 1 runs on cpus $_hk_now = the housekeeping set $_hk_set (issue 1367)"
else
  echo "FAIL: [issue 1367] PID 1 runs on cpus ${_hk_now:-<unreadable>}, not $_hk_set, on $_hk_box" >&2
  _hk_rc=1
fi
return "$_hk_rc"
}
_hk_main </dev/null
PROG
}
