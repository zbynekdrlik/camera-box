#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/strih-nic-irq.sh -- the rig-NIC IRQ placement of setup-strih step 11b (issue 1317 item H,
# issue 1425): the resolvers verify-strih item 16 shares and the boot oneshot strih-nic-irq-affinity.sh.
# Moved verbatim out of strih-provision.sh (its line budget, issue 1425 review); strih-provision.sh
# sources it, so every caller sees the same functions. The fact accessors come from strih-box-facts.sh.

# --- issue 1317 item H: strih-snv USB-NIC xhci IRQ placement (NET_RX softirq off the OBS cores) ----
# The RTL8156B USB 2.5GbE NIC's xhci interrupt lands on ONE core (irqbalance is not installed, so
# the kernel parks it) where ~1.1 Gb/s of NDI NET_RX softirq collides with OBS's ndir:video/libobs
# threads (issue 1354, 22.9.2026, measured on strih-snv: moving the IRQ to an idle E-core cut the
# genlock "slow output_video" rate from ~35/min to 5-10/min and the idle cam6/cam7 HOLD rate from
# 1-2/min to ~0.15/min). The live smp_affinity write is lost at reboot, so it is baked into
# provisioning as a boot oneshot resolving the placement FROM FACTS -- never a hard-coded IRQ number,
# so it survives a different IRQ after a kernel/firmware change or a different USB port. Every
# resolution helper takes its sysfs/proc inputs as args/env so the tests drive /proc-shaped fixtures.

# strih_cpulist_max LIST -> the highest cpu in a Linux cpulist ("12-15" -> 15, "0,1,2" -> 2,
# "3" -> 3). Non-zero (empty print) on an empty/garbage list. issue 1317.
strih_cpulist_max() {
  local list="${1:-}" part b max=''
  local -a _parts
  [ -n "$list" ] || { printf ''; return 1; }
  IFS=',' read -ra _parts <<< "$list" || true
  for part in "${_parts[@]}"; do
    part="${part//[[:space:]]/}"
    [ -n "$part" ] || continue
    case "$part" in *-*) b="${part##*-}" ;; *) b="$part" ;; esac
    case "$b" in ''|*[!0-9]*) continue ;; esac
    if [ -z "$max" ] || [ "$b" -gt "$max" ]; then max="$b"; fi
  done
  [ -n "$max" ] || { printf ''; return 1; }
  printf '%s' "$max"
}

# strih_cpulist_min LIST -> the lowest cpu in a Linux cpulist ("12-15" -> 12, "3,1,2" -> 1).
# Non-zero (empty print) on an empty/garbage list. issue 1317.
strih_cpulist_min() {
  local list="${1:-}" part a min=''
  local -a _parts
  [ -n "$list" ] || { printf ''; return 1; }
  IFS=',' read -ra _parts <<< "$list" || true
  for part in "${_parts[@]}"; do
    part="${part//[[:space:]]/}"
    [ -n "$part" ] || continue
    case "$part" in *-*) a="${part%%-*}" ;; *) a="$part" ;; esac
    case "$a" in ''|*[!0-9]*) continue ;; esac
    if [ -z "$min" ] || [ "$a" -lt "$min" ]; then min="$a"; fi
  done
  [ -n "$min" ] || { printf ''; return 1; }
  printf '%s' "$min"
}

# strih_nic_xhci_pci_function SYSROOT IFACE -> the xhci host-controller PCI function (e.g.
# 0000:00:14.0) that IFACE's USB NIC hangs off: readlink -f <SYSROOT>/class/net/<IFACE>/device,
# then walk UP to the usbN root -- its parent basename is the PCI function. Fail (empty, non-zero)
# when the iface device node or the usbN root cannot be found. issue 1317.
strih_nic_xhci_pci_function() {
  local sysroot="${1:?sysroot required}" iface="${2:?iface required}" dev p base cand
  dev="$(readlink -f "${sysroot}/class/net/${iface}/device" 2>/dev/null || true)"
  [ -n "$dev" ] || { printf ''; return 1; }
  p="$dev"
  while [ -n "$p" ] && [ "$p" != "/" ]; do
    base="$(basename "$p")"
    case "$base" in
      usb[0-9]*)
        cand="$(basename "$(dirname "$p")")"
        case "$cand" in
          [0-9a-fA-F]*:[0-9a-fA-F]*:[0-9a-fA-F]*.[0-9]) printf '%s' "$cand"; return 0 ;;
          *) printf ''; return 1 ;;
        esac
        ;;
    esac
    p="$(dirname "$p")"
  done
  printf ''; return 1
}

# strih_nic_xhci_irqs PROC_INTERRUPTS PCIFN -> the IRQ number(s), one per line, of the /proc/
# interrupts rows that name BOTH the xhci_hcd driver AND the PCI function PCIFN (the row's
# IR-PCI-MSI-<pcifn> chip column). Drain-safe (reads the whole file). Non-zero when none match.
# issue 1317.
strih_nic_xhci_irqs() {
  local proc_interrupts="${1:?/proc/interrupts path required}" pcifn="${2:-}" found=0 line irq
  [ -n "$pcifn" ] || return 1
  while IFS= read -r line; do
    case "$line" in
      *"$pcifn"*xhci_hcd*|*xhci_hcd*"$pcifn"*) : ;;
      *) continue ;;
    esac
    irq="${line%%:*}"; irq="${irq//[[:space:]]/}"
    case "$irq" in ''|*[!0-9]*) continue ;; esac
    printf '%s\n' "$irq"
    found=1
  done < "$proc_interrupts"
  [ "$found" = 1 ]
}

# strih_nic_irq_target_cpu CPU_ATOM_FILE [ONLINE_FILE] -> the target cpu for the NIC IRQ: the LAST
# cpu in CPU_ATOM_FILE (an E-core on an Intel hybrid box). Fallback when CPU_ATOM_FILE is absent
# (non-hybrid): the highest online cpu from ONLINE_FILE, else nproc-1. Non-zero when nothing
# resolves. issue 1317.
strih_nic_irq_target_cpu() {
  local cpu_atom_file="${1:-/sys/devices/cpu_atom/cpus}"
  local online_file="${2:-/sys/devices/system/cpu/online}"
  local list last n
  if [ -r "$cpu_atom_file" ]; then
    list="$(cat "$cpu_atom_file" 2>/dev/null || true)"
    last="$(strih_cpulist_max "$list" 2>/dev/null || true)"
    [ -n "$last" ] && { printf '%s' "$last"; return 0; }
  fi
  if [ -r "$online_file" ]; then
    list="$(cat "$online_file" 2>/dev/null || true)"
    last="$(strih_cpulist_max "$list" 2>/dev/null || true)"
    [ -n "$last" ] && { printf '%s' "$last"; return 0; }
  fi
  n="$(nproc 2>/dev/null || echo 0)"
  case "$n" in ''|*[!0-9]*) n=0 ;; esac
  [ "$n" -gt 0 ] && { printf '%s' "$((n - 1))"; return 0; }
  printf ''; return 1
}

# strih_nic_irq_affinity_verdict AFFINITY_LIST FIRST_ATOM_CPU -> prints ok|multi|below-atom and
# returns 0 only on ok. AFFINITY_LIST is /proc/irq/<n>/smp_affinity_list content (e.g. "15"); it
# must be a SINGLE cpu (no comma, no range). FIRST_ATOM_CPU (the first cpu_atom cpu, e.g. "12") is
# the hybrid floor -- the pinned cpu must be >= it; an empty FIRST_ATOM_CPU skips the floor check
# (non-hybrid box). Fail-closed on empty/garbage. issue 1317.
strih_nic_irq_affinity_verdict() {
  local list="${1:-}" first_atom="${2:-}" cpu
  cpu="${list//[[:space:]]/}"
  case "$cpu" in
    ''|*,*|*-*|*[!0-9]*) printf 'multi'; return 1 ;;
  esac
  case "$first_atom" in
    ''|*[!0-9]*) : ;;
    *) [ "$cpu" -lt "$first_atom" ] && { printf 'below-atom'; return 1; } ;;
  esac
  printf 'ok'; return 0
}

# strih_irq_total_count PROC_INTERRUPTS IRQ -> the sum of IRQ's per-cpu interrupt counts (the
# leading integer columns of its /proc/interrupts row, stopping at the first non-integer field =
# the chip-name column). Non-zero (empty) when the row is absent. issue 1317.
strih_irq_total_count() {
  local proc_interrupts="${1:?/proc/interrupts path required}" irq="${2:?irq required}" line tot=0 tok
  line="$(grep -E "^[[:space:]]*${irq}:" "$proc_interrupts" 2>/dev/null | head -1 || true)"
  [ -n "$line" ] || { printf ''; return 1; }
  line="${line#*:}"
  for tok in $line; do
    case "$tok" in ''|*[!0-9]*) break ;; esac
    tot=$((tot + tok))
  done
  printf '%s' "$tot"
}

# strih_counter_advanced C1 C2 -> 0 iff both are integers and C2 > C1 (the IRQ counter advanced
# across the sample window -- the live-liveness half of the verify item; a static file check is a
# lying gate). issue 1317.
strih_counter_advanced() {
  local a="${1:-}" b="${2:-}"
  case "$a" in ''|*[!0-9]*) return 1 ;; esac
  case "$b" in ''|*[!0-9]*) return 1 ;; esac
  [ "$b" -gt "$a" ]
}

# strih_nic_iface_by_driver SYSROOT DRIVER -> resolve the NIC iface by its kernel DRIVER (the
# strih-snv USB NIC is r8152 = the RTL815x USB-ethernet driver; the onboard PCIe NIC is r8169). Scans
# <SYSROOT>/class/net/*/device/driver (readlink basename) for ifaces whose driver == DRIVER, skipping
# `lo`. Encodes the result in the OUTPUT string so a set -e caller can branch on it without an rc
# dance: EXACTLY ONE match -> prints the iface, returns 0; NONE -> prints '' , returns 1 (the caller
# falls back to the address match); MORE THAN ONE -> prints 'MULTI:<iface> <iface> ...', returns 2
# (fail loud -- STRIH_NIC_IFACE must disambiguate). issue 1317 item H.
strih_nic_iface_by_driver() {
  local sysroot="${1:?sysroot required}" driver="${2:?driver required}"
  local net ifc dl drv matches=''
  for net in "${sysroot}/class/net/"*; do
    [ -e "$net" ] || continue
    ifc="$(basename "$net")"
    [ "$ifc" = lo ] && continue
    dl="$(readlink -f "${net}/device/driver" 2>/dev/null || true)"
    drv=''
    [ -n "$dl" ] && drv="$(basename "$dl")"
    [ "$drv" = "$driver" ] && matches="${matches:+$matches }$ifc"
  done
  case "$matches" in
    '')    printf '';                 return 1 ;;
    *' '*) printf 'MULTI:%s' "$matches"; return 2 ;;
    *)     printf '%s' "$matches";     return 0 ;;
  esac
}

# strih_nic_irq_affinity_unit_text -> the systemd unit body of
# systemd/strih-nic-irq-affinity.service (a system oneshot, RemainAfterExit, enable-only, ordered
# After+Wants network-online.target so the NIC is up when it runs). Kept byte-identical to the
# committed file by a parity test. issue 1317.
strih_nic_irq_affinity_unit_text() {
  cat <<'UNIT'
[Unit]
Description=strih box: pin the rig-NIC IRQs off the OBS cores (issue 1317 item H, issue 1425)
Documentation=https://github.com/zbynekdrlik/camera-box
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/strih-nic-irq-affinity.sh

[Install]
WantedBy=multi-user.target
UNIT
}

# strih_nic_irq_affinity_script_text -> the body of /usr/local/bin/strih-nic-irq-affinity.sh, the
# boot oneshot that resolves iface -> xhci PCI function -> IRQ(s) -> last cpu_atom E-core FROM FACTS
# and writes smp_affinity_list, failing LOUD on any unresolved step or read-back mismatch. Every
# /sys + /proc input is env-overridable (SYS_ROOT / PROC_INTERRUPTS / CPU_ATOM_FILE / IRQ_DIR /
# STRIH_NIC_IFACE) so a test can run the emitted script over fixtures shaped like the real box.
# issue 1317 item H.
# strih_nic_irq_affinity_script_install DEST -> writes the script ATOMICALLY (issue 1425): a sibling temp
# file, chmod 0755, then rename over DEST, so the boot oneshot or the re-balance timer never execs a
# half-written file (strih-snv 24.11.2026: Exec format error, 203/EXEC, while step 11b rewrote it in place).
# rc 0 written, rc 1 any step failed (the temp file removed).
strih_nic_irq_affinity_script_install() {
  local dest="${1:?strih_nic_irq_affinity_script_install: DEST}" tmp
  tmp="$(mktemp "${dest%/*}/.${dest##*/}.XXXXXX")" || return 1
  if strih_nic_irq_affinity_script_text >"$tmp" && chmod 0755 "$tmp" && mv -f -- "$tmp" "$dest"; then
    return 0
  fi
  rm -f -- "$tmp"
  return 1
}

strih_nic_irq_affinity_script_text() {
  # issue 1361: the target IP + NIC driver are the loaded box's facts, substituted into the quoted
  # script body's @STRIH_TARGET_IP@ / @STRIH_NIC_DRIVER@ placeholders (the rest stays literal); issue 1425
  # adds @STRIH_HOUSEKEEPING_IRQ_CPUS@ = the IRQ CPUs of the housekeeping set (strih_housekeeping_irq_cpus in
  # scripts/lib/strih-housekeeping.sh: every CPU of it, design 7119179275), `none` without housekeeping CPUs,
  # and @STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS@ = the IRQ CPUs that are not task CPUs (strih_housekeeping_slice_exclude,
  # strih-snv 1-3; `none` for a one-CPU set): the placement uses only those (finding 26.11.2026).
  local ip drv box irq irqonly body
  ip="$(strih_lx_ip)" || return 1
  drv="$(strih_lx_nic_driver)" || return 1
  box="$(strih_lx_hostname)" || return 1
  irq="$(strih_housekeeping_irq_cpus)" || return 1
  irqonly="$(strih_housekeeping_slice_exclude)" || return 1
  body="$(_strih_nic_irq_affinity_script_template)" || return 1
  body="${body//@STRIH_TARGET_IP@/"$ip"}"
  body="${body//@STRIH_BOX@/"$box"}"
  body="${body//@STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS@/"$irqonly"}"
  body="${body//@STRIH_HOUSEKEEPING_IRQ_CPUS@/"$irq"}"
  printf '%s\n' "${body//@STRIH_NIC_DRIVER@/"$drv"}"
}

# _strih_nic_irq_affinity_script_template -> the boot oneshot body with the box facts as placeholders.
_strih_nic_irq_affinity_script_template() {
  cat <<'SCRIPT'
#!/usr/bin/env bash
# /usr/local/bin/strih-nic-irq-affinity.sh
set -euo pipefail
# @STRIH_BOX@ rig-NIC IRQ placement (issue 1317 item H, issue 1425). Emitted by
# strih_nic_irq_affinity_script_text in scripts/lib/strih-nic-irq.sh, installed by
# scripts/setup-strih.sh, run at boot by strih-nic-irq-affinity.service, on every NetworkManager `up` of the
# rig NIC by the issue-1425 dispatcher hook (a link reset can re-place the IRQs), and once live by the
# supervisor. It keeps the rig NIC's NET_RX softirq off the OBS cores. With housekeeping CPUs
# (box fact STRIH_HOUSEKEEPING_CPUS, issue 1425) it places EVERY IRQ of the rig NIC
# (/sys/class/net/<nic>/device/msi_irqs; a USB NIC: its xhci interrupt) on ONE of them, balanced by the
# rate measured over IRQ_RATE_SAMPLE_S (design 7119179275: heaviest first to the least-loaded CPU, ties to
# the lower CPU), writing an affinity only when it differs (every move costs a short loss burst). Only the
# IRQ-only CPUs (IRQ_ONLY_CPUS: the set minus the task CPU, finding 26.11.2026: a 12k/s queue on the task
# CPU 0 starved its softirq beside rustdesk / the launcher / the slice) take IRQs; the whole set only when
# there is none (a one-CPU set). A sample
# under IRQ_LOAD_MIN_PER_S in total (boot, or OBS down during a deploy: 26.11.2026 such a sample put 7 of 9
# heavy queues on CPU 0) is no load to balance by: an IRQ already on one housekeeping CPU keeps it, only
# one outside the set is placed, round-robin by IRQ count. Before that it set the NIC's RSS queues to the
# IRQ CPU count (ethtool -L combined, only when it differs). Without them
# (`none`) it pins the USB NIC's xhci interrupt to the last E-core (issue 1354: cut the genlock
# "slow output_video" rate ~75%). It resolves the placement FROM FACTS -- never a hard-coded IRQ
# number -- and FAILS LOUD (exit 1 + a journal line) on any unresolved step or a read-back mismatch.
# Inputs are env-overridable so a test drives fixtures.

SYS_ROOT="${SYS_ROOT:-/sys}"
PROC_INTERRUPTS="${PROC_INTERRUPTS:-/proc/interrupts}"
CPU_ATOM_FILE="${CPU_ATOM_FILE:-/sys/devices/cpu_atom/cpus}"
CPU_ONLINE_FILE="${CPU_ONLINE_FILE:-/sys/devices/system/cpu/online}"
IRQ_DIR="${IRQ_DIR:-/proc/irq}"
ETHTOOL="${ETHTOOL:-ethtool}"
CARRIER_WAIT_S="${CARRIER_WAIT_S:-5}"
IRQ_RATE_SAMPLE_S="${IRQ_RATE_SAMPLE_S:-2}"
# The total rig-NIC IRQ rate below which a sample is no load (finding 26.11.2026): the loaded SNV strih read
# ~120k/s (05:24, 60266/s vs 60261/s per CPU) and ~42k/s (24.11, the CPU-1 split); boot and OBS down read
# far less. Under it the current placement is kept, never re-balanced on meaningless rates.
IRQ_LOAD_MIN_PER_S="${IRQ_LOAD_MIN_PER_S:-20000}"
TARGET_IP="${STRIH_LX_TARGET_IP:-@STRIH_TARGET_IP@}"
IRQ_CPUS="${STRIH_HOUSEKEEPING_IRQ_CPUS:-@STRIH_HOUSEKEEPING_IRQ_CPUS@}"
# a hand run that overrides STRIH_HOUSEKEEPING_IRQ_CPUS overrides STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS with it
IRQ_ONLY_CPUS="${STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS:-@STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS@}"

log() { printf 'strih-nic-irq-affinity: %s\n' "$*"; }
die() { printf 'strih-nic-irq-affinity: FATAL: %s\n' "$*" >&2; exit 1; }

# cpulist_canon LIST -> the CPUs of a kernel cpulist, sorted, one space between ("0-1" and "0,1" both
# read "0 1"); empty on a malformed list. The kernel prints smp_affinity_list compressed, so a read-back
# compares sets, never text.
cpulist_canon() {
  local item a b i out=""
  case "${1:-}" in ''|*[!0123456789,-]*) return 0 ;; esac
  for item in ${1//,/ }; do
    case "$item" in
      *-*) a="${item%-*}"; b="${item#*-}" ;;
      *) a="$item"; b="$item" ;;
    esac
    case "$a$b" in ''|*[!0123456789]*) return 0 ;; esac
    for ((i = 10#$a; i <= 10#$b; i++)); do out="$out $i"; done
  done
  printf '%s\n' $out | sort -nu | tr '\n' ' ' | sed 's/ $//'
}

# (1) NIC iface -- the ONE rig-NIC rule of strih_lx_rig_nic: STRIH_NIC_IFACE override wins; else the ONE
# @STRIH_NIC_DRIVER@ iface (works at boot BEFORE any address is assigned); else -- no such iface, or more than
# one (issue 1425: strih-pp's dual-port ixgbe card) -- the interface carrying the target IP. None or two
# carrying it fail LOUD (set STRIH_NIC_IFACE to disambiguate).
by_address() {
  { ip -o -4 addr show 2>/dev/null || true; } | awk -v ip="$TARGET_IP" 'BEGIN { gsub(/\./, "\\.", ip) } $4 ~ ("^" ip "/") { print $2 }' | sort -u | tr '\n' ' ' | sed 's/ $//'
}
iface="${STRIH_NIC_IFACE:-}"
if [ -z "$iface" ]; then
  matches=""
  for _net in "${SYS_ROOT}/class/net/"*; do
    [ -e "$_net" ] || continue
    _ifc="$(basename "$_net")"
    [ "$_ifc" = lo ] && continue
    _dl="$(readlink -f "${_net}/device/driver" 2>/dev/null || true)"
    _drv=""
    [ -n "$_dl" ] && _drv="$(basename "$_dl")"
    [ "$_drv" = @STRIH_NIC_DRIVER@ ] && matches="${matches:+$matches }$_ifc"
  done
  case "$matches" in
    "" | *" "*)
      iface="$(by_address)"
      case "$iface" in
        *" "*) die "${TARGET_IP} is on more than one interface (${iface}) -- set STRIH_NIC_IFACE" ;;
        "")    [ -z "$matches" ] || die "multiple @STRIH_NIC_DRIVER@ NICs (${matches}) and none carries ${TARGET_IP} -- set STRIH_NIC_IFACE" ;;
        *)     [ -z "$matches" ] || log "multiple @STRIH_NIC_DRIVER@ NICs (${matches}): ${iface} carries ${TARGET_IP}" ;;
      esac ;;
    *)     iface="$matches" ;;
  esac
fi
[ -n "$iface" ] || die "could not resolve the NIC iface (no @STRIH_NIC_DRIVER@, no ${TARGET_IP} address; set STRIH_NIC_IFACE)"

irqs=""; src=""
msi_dir="${SYS_ROOT}/class/net/${iface}/device/msi_irqs"
if [ "$IRQ_CPUS" != none ]; then
  hk="$(cpulist_canon "$IRQ_CPUS")"
  [ -n "$hk" ] || die "STRIH_HOUSEKEEPING_IRQ_CPUS '${IRQ_CPUS}' is not a cpulist"
  # the placement CPUs: the IRQ-only ones (never the task CPU while one exists), else the whole set
  place="$hk"
  if [ "$IRQ_ONLY_CPUS" != none ]; then
    place="$(cpulist_canon "$IRQ_ONLY_CPUS")"
    [ -n "$place" ] || die "STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS '${IRQ_ONLY_CPUS}' is not a cpulist"
    for _c in $place; do
      case " $hk " in *" $_c "*) ;; *) die "STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS '${IRQ_ONLY_CPUS}' is not inside the IRQ CPUs ${IRQ_CPUS}" ;; esac
    done
  fi
  [[ $IRQ_RATE_SAMPLE_S =~ ^[0123456789]{1,4}$ ]] || die "IRQ_RATE_SAMPLE_S '${IRQ_RATE_SAMPLE_S}' is not a whole number of seconds"
  [[ $IRQ_LOAD_MIN_PER_S =~ ^[0123456789]{1,9}$ ]] || die "IRQ_LOAD_MIN_PER_S '${IRQ_LOAD_MIN_PER_S}' is not a whole number of interrupts per second"
fi

if [ "$IRQ_CPUS" != none ] && [ -d "$msi_dir" ]; then
  # (2a) issue 1425: RSS queues = the IRQ CPU count, changed only when it differs (a change
  # re-initialises the NIC's rings: the driver may drop the link for a moment, once). At boot the
  # udev rule of scripts/lib/strih-housekeeping.sh already set it at `add`, before the link came up, so
  # this is a no-op there; the carrier_changes counter around a change shows any link drop it caused.
  want="$(printf '%s\n' $hk | wc -l | tr -d '[:space:]')"
  chan() { "$ETHTOOL" -l "$iface" 2>/dev/null | awk '/^Current hardware settings/ { c = 1 } c && /^Combined:/ { print $2; exit }' || true; }
  cur="$(chan)"
  # issue 1425 (23.11.2026): a driver may offer no channel control (`ethtool -l`: Operation not supported)
  if [ -z "$cur" ] || [[ "$cur" == *[!0123456789]* ]]; then
    log "${iface}: RSS channel count not readable (the driver offers no channel control) -- left alone"
  elif [ "$cur" != "$want" ]; then
    cc0="$(cat "${SYS_ROOT}/class/net/${iface}/carrier_changes" 2>/dev/null || echo '?')"
    "$ETHTOOL" -L "$iface" combined "$want" || die "ethtool -L ${iface} combined ${want} failed (was ${cur})"
    got="$(chan)"
    [ "$got" = "$want" ] || die "read-back mismatch on ${iface}: set combined ${want}, read '${got}'"
    # the driver drops the link about a second after -L returns: wait for the counter to move
    for ((_w = 0; _w < CARRIER_WAIT_S; _w++)); do
      cc1="$(cat "${SYS_ROOT}/class/net/${iface}/carrier_changes" 2>/dev/null || echo '?')"
      [ "$cc1" = "$cc0" ] || break
      sleep 1
    done
    cc1="$(cat "${SYS_ROOT}/class/net/${iface}/carrier_changes" 2>/dev/null || echo '?')"
    log "${iface}: RSS queues ${cur} -> ${want} (combined); carrier_changes ${cc0} -> ${cc1}"
  else
    log "${iface}: RSS queues already ${want} (combined) -- unchanged"
  fi
  # (3a) every IRQ of the rig NIC, read AFTER the channel change (the driver re-requests its vectors).
  for _f in "$msi_dir"/*; do
    _n="$(basename "$_f")"
    case "$_n" in ''|*[!0123456789]*) continue ;; esac
    if [ ! -d "${IRQ_DIR}/${_n}" ]; then log "IRQ ${_n} of ${iface} has no /proc/irq entry (not requested) -- skipped"; continue; fi
    irqs="${irqs:+$irqs }$_n"
  done
  [ -n "$irqs" ] || die "no IRQ listed in ${msi_dir}"
  irqs="$(printf '%s\n' $irqs | sort -n | tr '\n' ' ' | sed 's/ $//')"
  src="msi ${iface}"
else
  [ "$IRQ_CPUS" = none ] || log "${iface}: no msi_irqs (not a PCIe NIC) -- RSS left alone, its xhci IRQ goes to the placement CPUs"
  # (2) xhci host controller PCI function = walk the device symlink up to the usbN root.
  dev="$(readlink -f "${SYS_ROOT}/class/net/${iface}/device" 2>/dev/null || true)"
  [ -n "$dev" ] || die "no /sys device node for iface ${iface}"
  pcifn=""; p="$dev"
  while [ -n "$p" ] && [ "$p" != "/" ]; do
    base="$(basename "$p")"
    case "$base" in
      usb[0-9]*)
        cand="$(basename "$(dirname "$p")")"
        case "$cand" in [0-9a-fA-F]*:[0-9a-fA-F]*:[0-9a-fA-F]*.[0-9]) pcifn="$cand" ;; esac
        break ;;
    esac
    p="$(dirname "$p")"
  done
  [ -n "$pcifn" ] || die "could not find the xhci PCI function above ${dev} (iface ${iface})"

  # (3) IRQ(s) = /proc/interrupts rows naming BOTH xhci_hcd AND that PCI function.
  while IFS= read -r line; do
    case "$line" in *"$pcifn"*xhci_hcd*|*xhci_hcd*"$pcifn"*) : ;; *) continue ;; esac
    n="${line%%:*}"; n="${n//[[:space:]]/}"
    case "$n" in ''|*[!0-9]*) continue ;; esac
    irqs="${irqs:+$irqs }$n"
  done < "$PROC_INTERRUPTS"
  [ -n "$irqs" ] || die "no xhci_hcd IRQ row for ${pcifn} in ${PROC_INTERRUPTS}"
  src="xhci_hcd ${pcifn}"
fi

# irq_count IRQ -> the sum of IRQ's per-CPU counts in PROC_INTERRUPTS (empty when it has no row).
irq_count() {
  local line tok tot=0
  line="$(grep -E "^[[:space:]]*${1}:" "$PROC_INTERRUPTS" 2>/dev/null | head -n 1 || true)"
  [ -n "$line" ] || return 0
  for tok in ${line#*:}; do
    case "$tok" in ''|*[!0123456789]*) break ;; esac
    tot=$((tot + 10#$tok))
  done
  printf '%s\n' "$tot"
}

# (4) each IRQ's target CPU. Housekeeping CPUs: ONE placement CPU per IRQ (the IRQ-only CPUs, else the whole
# set), by the measured rate (greedy: heaviest
# first to the CPU with the least assigned rate, ties to the lower CPU; a total under IRQ_LOAD_MIN_PER_S =
# keep every IRQ already on one of them, the rest to the CPU holding the fewest IRQs, in IRQ order, ties to
# the lower CPU: round-robin at boot). Without them: the LAST cpu in cpu_atom (an E-core), fallback the
# highest online cpu, for every IRQ.
declare -A tgt=() rate=()
if [ "$IRQ_CPUS" != none ]; then
  read -ra cpus <<<"$place"
  declare -A c0=() load=()
  for irq in $irqs; do c0[$irq]="$(irq_count "$irq")"; done
  [ "$IRQ_RATE_SAMPLE_S" = 0 ] || sleep "$IRQ_RATE_SAMPLE_S"
  total=0
  for irq in $irqs; do
    c1="$(irq_count "$irq")"
    if [ -z "$c1" ] || [ -z "${c0[$irq]}" ]; then
      log "IRQ ${irq}: no /proc/interrupts row -- rate 0"; rate[$irq]=0
    elif [ "$c1" -lt "${c0[$irq]}" ]; then
      rate[$irq]=0
    elif [ "$IRQ_RATE_SAMPLE_S" = 0 ]; then
      rate[$irq]=$((c1 - c0[$irq]))
    else
      rate[$irq]=$(((c1 - c0[$irq]) / IRQ_RATE_SAMPLE_S))
    fi
    total=$((total + rate[$irq]))
  done
  for c in "${cpus[@]}"; do load[$c]=0; done
  if [ "$total" -lt "$IRQ_LOAD_MIN_PER_S" ]; then
    if [ "$total" = 0 ]; then
      log "no IRQ traffic measured (rates all 0) -- round-robin in IRQ order over cpus ${place}; an IRQ already on one of them keeps it"
    else
      log "rig NIC IRQ traffic ${total}/s measured over ${IRQ_RATE_SAMPLE_S} s is under IRQ_LOAD_MIN_PER_S ${IRQ_LOAD_MIN_PER_S}/s (no real load: OBS down or boot) -- an IRQ already on one of cpus ${place} keeps it, the rest round-robin"
    fi
    declare -A nirq=()
    for c in "${cpus[@]}"; do nirq[$c]=0; done
    pending=""
    for irq in $irqs; do
      curc="$(cpulist_canon "$({ tr -d '[:space:]' < "${IRQ_DIR}/${irq}/smp_affinity_list"; } 2>/dev/null || true)")"
      if [ -n "$curc" ] && [ -n "${nirq[$curc]+x}" ]; then
        tgt[$irq]="$curc"; nirq[$curc]=$((nirq[$curc] + 1))
        log "IRQ ${irq}: on housekeeping cpu ${curc} -- kept (no load to balance by)"
      else
        pending="${pending:+$pending }${irq}"
      fi
    done
    for irq in $pending; do
      best=""
      for c in "${cpus[@]}"; do
        if [ -z "$best" ] || [ "${nirq[$c]}" -lt "${nirq[$best]}" ]; then best="$c"; fi
      done
      tgt[$irq]="$best"; nirq[$best]=$((nirq[$best] + 1))
    done
  else
    while read -r r irq; do
      best=""
      for c in "${cpus[@]}"; do
        if [ -z "$best" ] || [ "${load[$c]}" -lt "${load[$best]}" ]; then best="$c"; fi
      done
      tgt[$irq]="$best"; load[$best]=$((load[$best] + r))
    done < <(for irq in $irqs; do printf '%s %s\n' "${rate[$irq]}" "$irq"; done | sort -k1,1nr -k2,2n)
    summary=""
    for c in "${cpus[@]}"; do summary="${summary:+$summary, }cpu ${c} ${load[$c]}/s"; done
    log "rig NIC IRQ rates measured over ${IRQ_RATE_SAMPLE_S} s, assigned load: ${summary}"
  fi
else
  target=""
  if [ -r "$CPU_ATOM_FILE" ]; then
    target="$(tr ',' '\n' < "$CPU_ATOM_FILE" 2>/dev/null | sed 's/.*-//' | grep -E '^[0-9]+$' | sort -n | tail -1 || true)"
  fi
  if [ -z "$target" ] && [ -r "$CPU_ONLINE_FILE" ]; then
    target="$(tr ',' '\n' < "$CPU_ONLINE_FILE" 2>/dev/null | sed 's/.*-//' | grep -E '^[0-9]+$' | sort -n | tail -1 || true)"
  fi
  [ -n "$target" ] || die "could not determine an E-core / highest-online target cpu"
  for irq in $irqs; do tgt[$irq]="$target"; done
fi

# (5) write smp_affinity_list ONLY when it differs, then read it back (as a CPU set); every IRQ is tried,
# then any failure FAILS.
failed=""; placed=""
for irq in $irqs; do
  aff="${IRQ_DIR}/${irq}/smp_affinity_list"
  t="${tgt[$irq]}"
  what="IRQ ${irq} (${src}, iface ${iface})"
  [ -z "${rate[$irq]+x}" ] || what="${what}: ${rate[$irq]}/s"
  cur="$(tr -d '[:space:]' < "$aff" 2>/dev/null || true)"
  if [ -n "$cur" ] && [ "$(cpulist_canon "$cur")" = "$(cpulist_canon "$t")" ]; then
    log "${what} -> cpu ${t} (already there, unchanged)"; placed="${placed:+$placed }${irq}->${t}"; continue
  fi
  if [ ! -w "$aff" ]; then
    log "cannot write ${aff} (missing or no permission)"; failed="${failed:+$failed }${irq}"; continue
  fi
  if ! printf '%s\n' "$t" > "$aff"; then
    log "write to ${aff} failed"; failed="${failed:+$failed }${irq}"; continue
  fi
  got="$(tr -d '[:space:]' < "$aff" 2>/dev/null || true)"
  if [ "$(cpulist_canon "$got")" != "$(cpulist_canon "$t")" ]; then
    log "read-back mismatch on IRQ ${irq}: wrote ${t}, read '${got}'"; failed="${failed:+$failed }${irq}"; continue
  fi
  log "${what} -> cpu ${t} (was ${cur:-unreadable})"; placed="${placed:+$placed }${irq}->${t}"
done
if [ "$IRQ_CPUS" != none ]; then
  [ -z "$failed" ] || die "IRQ(s) [${failed}] of ${iface} could not be placed on the housekeeping CPUs ${IRQ_CPUS} (above)"
else
  [ -z "$failed" ] || die "IRQ(s) [${failed}] of ${iface} could not be pinned to cpu ${target} (above)"
fi
log "done: rig NIC IRQ(s) [${irqs}] placed: ${placed}"
SCRIPT
}
