#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih-lx role FACTS + pure decision helpers,
# no top-level statements) -- matches the sibling scripts/lib/*.sh convention (obs-fleet.sh,
# camera-set.sh, genlock-markers.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing
# this file executes it in the CALLER's shell, so strict mode here would leak into whichever caller
# sources it. Each caller (setup-strih.sh / verify-strih.sh) sets its own strict mode.
#
# scripts/lib/strih-provision.sh -- issue 1317: the ONE place the Linux strih notebook (`strih-lx`)
# role FACTS + pure provisioning decisions live, sourced by scripts/setup-strih.sh (provisioning),
# scripts/verify-strih.sh (acceptance gate) AND tests/strih_provision_pure_functions.rs (rustc-free
# Tier-0). Same source-of-truth model as camera-set.sh for the camera fleet.
#
# PARALLEL-RUN CONTRACT (owner 16.9.2026): the notebook runs IN PARALLEL with the Windows STRIH-SNV
# cutter until tuned. TWO strih senders coexist, so strih-lx's NDI OUTPUTS are namespaced
# `STRIH-LX (...)` and it joins the clock as a dantesync CLIENT (the Windows PC stays the one NTP
# master). The stream box + receivers must NEVER see a second `STRIH-SNV (...)` sender.

# strih_lx_host -> the hostname the fleet dials. Default strih-lx.lan; STRIH_LX_HOST overrides.
# The literal IP is TBD (assigned 17.9. on arrival, STRIH_LX_IP) -- resolution is by hostname,
# exactly as the imag leg dials "imag" (deploy-genlock-fleet.sh / obs-fleet.sh).
strih_lx_host() { printf '%s' "${STRIH_LX_HOST:-strih-lx.lan}"; }

# strih_lx_ip -> the assigned static IP, or empty until it is assigned on arrival. STRIH_LX_IP wins.
strih_lx_ip() { printf '%s' "${STRIH_LX_IP:-}"; }

# strih_lx_ndi_inputs -> the 10 NDI input names the strih role receives, one per line (issue 1317
# spec; the 2ME feedback inputs are the task's explicit STRIH-SNV names -- the self-feedback
# STRIH-LX nuance is a live-tuning follow-up documented in .claude/rules/strih-linux-provisioning.md).
strih_lx_ndi_inputs() {
  printf '%s\n' \
    'CAM1 (usb)' 'CAM2 (usb)' 'CAM3 (usb)' 'CAM4 (usb)' \
    'CAM5 (usb)' 'CAM6 (usb)' 'CAM7 (usb)' \
    'STRIH-SNV (2ME PGM)' 'STRIH-SNV (2ME PVW)' 'RESOLUME-SNV (cg-obs)'
}

# strih_lx_ndi_outputs -> the namespaced 2ME NDI output names (never a STRIH-SNV name), one per line.
strih_lx_ndi_outputs() { printf '%s\n' 'STRIH-LX (2ME PGM)' 'STRIH-LX (2ME PVW)'; }

# strih_lx_ndi_republishes -> the namespaced genlock-ndi-filter republish names, one per line.
strih_lx_ndi_republishes() {
  printf '%s\n' 'STRIH-LX (interkom)' 'STRIH-LX (MULTIVIEW)' 'STRIH-LX (Grading)'
}

# strih_lx_camera_latency_ms -> the genlock latency floor (ms) every camera input rides (3, the
# rig floor -- latency-pins-baseline.json strih-lx block; the per-run aligner owns any offset).
strih_lx_camera_latency_ms() { printf '3'; }

# strih_lx_bundle_artifact -> the strih FULL-build CI artifact name (linux-genlock.yml strih job).
strih_lx_bundle_artifact() { printf 'obs-genlock-linux-x86_64-strih'; }

# strih_lx_dantesync_client_args -> the dantesync CLIENT invocation args. It points at the Windows
# strih PC as the NTP server and NEVER enables server/master mode (single master while parallel).
strih_lx_dantesync_client_args() { printf -- '--ntp-server %s' "${STRIH_LX_NTP_SERVER:-strih.lan}"; }

# strih_lx_dantesync_is_client_not_master MODE -> 0 iff MODE is a CLIENT mode (never server/master).
# Fail-closed: an empty/unknown mode returns 1 (a strih-lx that cannot prove it is a client must not
# be trusted to not steal the master role).
strih_lx_dantesync_is_client_not_master() {
  # master flags checked first, but NARROWLY: `*server_mode*` (covers dantesync's `ntp_server_mode`)
  # + `*master*`/`*grandmaster*` -- NEVER a bare `*server*`, which would also swallow the CLIENT
  # `--ntp-server` / `ntp_server=<host>` forms and misclassify a client as master (shellcheck
  # SC2221/SC2222 caught exactly that). A bare "server" with no "_mode" is ambiguous -> fail-closed.
  case "${1:-}" in
    "") return 1 ;;
    *server_mode*|*master*) return 1 ;;
    *client*|*slave*|*ntp-server*|*ntp_server=*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_profile_facts -> the OBS profile facts (from the Windows `light` profile inventory,
# 15.9.), key=value one per line: base/output 1920x1080 @ 30 fps NV12, Advanced out, NVENC HEVC
# record to /srv/_REC as 15-min-split mkv.
strih_lx_profile_facts() {
  printf '%s\n' \
    'base_res=1920x1080' \
    'output_res=1920x1080' \
    'fps=30' \
    'color_format=NV12' \
    'out_mode=Advanced' \
    'rec_encoder=obs_nvenc_hevc_tex' \
    'rec_path=/srv/_REC' \
    'rec_format=mkv' \
    'rec_split_min=15'
}

# strih_lx_audio_input_name -> the OBS PipeWire input name for the program-audio interface.
# The Windows path is MiniFuse 4 USB -> VB-Matrix (VASIO-8) ASIO; on Linux the class-compliant
# MiniFuse 4 is a native PipeWire node and PipeWire replaces VB-Matrix (owner 15.9.: NO Dante).
strih_lx_audio_input_name() { printf 'MiniFuse 4'; }

# strih_lx_audio_route_wired -> 0 iff the VB-Matrix -> PipeWire program-audio ROUTE (the graph that
# feeds the mastered program mix into the MiniFuse 4 capture OBS reads) is wired. Fail-loud TODO:
# returns 1 until an operator wires it and sets STRIH_LX_AUDIO_WIRED=1. setup-strih.sh's audio step
# FAILS on this so an unwired audio graph never passes silently.
strih_lx_audio_route_wired() { [ "${STRIH_LX_AUDIO_WIRED:-0}" = "1" ]; }

# strih_lx_output_name_ok NAME -> 0 iff NAME is a namespaced `STRIH-LX (...)` output and NOT a
# `STRIH-SNV (...)` one (the never-a-2nd-STRIH-SNV-sender invariant, single output check).
strih_lx_output_name_ok() {
  case "${1:-}" in
    'STRIH-SNV '*) return 1 ;;
    'STRIH-LX ('*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_no_second_strihsnv_sender  (stdin: live NDI output names, one per line) -> 0 iff NONE of
# them is a `STRIH-SNV (...)` sender. Drain-safe (reads the whole stream, no early break).
strih_lx_no_second_strihsnv_sender() {
  local line collision=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in 'STRIH-SNV '*) collision=1 ;; esac
  done
  [ "$collision" = 0 ]
}

# --- verify-strih.sh pure predicates (stdin-driven, so a live reader feeds them; unit-tested) -----

# strih_lx_render_tick_ok  (stdin: newest OBS log text) -> 0 iff the genlock render tick is enabled.
strih_lx_render_tick_ok() { grep -qiE 'render tick.*(enabled|on)|genlock[^\n]*render tick'; }

# strih_lx_distroav_loaded_ok  (stdin: newest OBS log text) -> 0 iff DistroAV/NDI loaded.
strih_lx_distroav_loaded_ok() { grep -qi 'distroav'; }

# strih_lx_nvenc_available_ok  (stdin: `ffmpeg -encoders` output OR the OBS log) -> 0 iff NVENC present.
strih_lx_nvenc_available_ok() { grep -qi 'nvenc'; }

# strih_lx_single_timesync_authority_ok  (stdin: enabled/active timesync unit names, one per line) ->
# 0 iff dantesync is present AND no competing timesync daemon is (the ops single-authority rule).
strih_lx_single_timesync_authority_ok() {
  local line has_dante=0 competitor=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      *dantesync*) has_dante=1 ;;
      *timesyncd*|*chrony*|*ntpd*|*ntpsec*|*ptp4l*) competitor=1 ;;
    esac
  done
  [ "$has_dante" = 1 ] && [ "$competitor" = 0 ]
}

# --- issue 1317: strih FULL-build browser bundle (obs-browser + CEF) acceptance predicates --------

# strih_lx_browser_bundle_required  (arg1: STRIH_BUILD_FLAGS.txt content) -> 0 iff it declares
# BROWSER-ON (obs-browser + CEF are expected in the bundle). Fail-closed: absent/OFF/empty -> 1
# (browser not required, so verify-strih NOTE-skips the file-presence check rather than failing).
strih_lx_browser_bundle_required() {
  case "${1:-}" in
    *BROWSER-ON*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_browser_bundle_ok  (stdin: file paths found under the strih install root, one per line --
# what `find <root> -name obs-browser.so -o -name libcef.so` prints) -> 0 iff BOTH obs-browser.so AND
# the CEF runtime libcef.so are present. Drain-safe (reads the whole stream, no early break).
strih_lx_browser_bundle_ok() {
  local line has_browser=0 has_cef=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      *obs-browser.so) has_browser=1 ;;
      *libcef.so)      has_cef=1 ;;
    esac
  done
  [ "$has_browser" = 1 ] && [ "$has_cef" = 1 ]
}

# --- issue 1317 (F6): chrome-sandbox setuid-root (the CEF SUID sandbox helper) --------------------
# The CEF strih bundle ships `chrome-sandbox` mode 700 owned by the CI runner uid. Chromium's SUID
# sandbox contract requires it to be owned root:root mode 4755 (setuid root); otherwise the sandbox
# helper aborts at browser-source launch (unless OBS is started `--no-sandbox`, which we REJECT --
# that weakens every browser source's isolation session-wide). The proper fix is the setuid helper,
# applied once at provisioning and gated in verify-strih.

# strih_lx_chrome_sandbox_fix_cmd BUNDLE_ROOT -> prints the idempotent remote statements that make
# the installed CEF chrome-sandbox launchable: locate it by NAME under BUNDLE_ROOT (never guessing
# the multiarch obs-plugins path) then `chown root:root` + `chmod 4755` it. The emitted statement
# ends with `;` so a mid-string $(...) embedding never glues the following command (the
# scripts/lib/v4l2-neutral.sh _cmd-helper gotcha). It runs as an `if` so a missing chrome-sandbox is
# a no-op (set -e safe); the CALLER (setup-strih.sh) enforces the BROWSER-ON fail-loud presence gate.
strih_lx_chrome_sandbox_fix_cmd() {
  local root="${1:?bundle-root required}"
  printf 'if __csb="$(find %q -type f -name chrome-sandbox 2>/dev/null | head -n1 || true)"; [ -n "$__csb" ]; then chown root:root "$__csb"; chmod 4755 "$__csb"; fi;\n' "$root"
}

# strih_lx_chrome_sandbox_verdict OWNER MODE PRESENT -> prints one verdict token and returns 0 iff
# ok. PRESENT is 1 when chrome-sandbox exists, else 0. Fail-closed order: absent -> `missing`; owner
# not root:root -> `wrong-owner`; mode not 4755 (setuid rwxr-xr-x) -> `wrong-mode`; else -> `ok`.
strih_lx_chrome_sandbox_verdict() {
  local owner="${1:-}" mode="${2:-}" present="${3:-}"
  [ "$present" = 1 ]         || { printf 'missing';     return 1; }
  [ "$owner" = 'root:root' ] || { printf 'wrong-owner'; return 1; }
  [ "$mode" = '4755' ]       || { printf 'wrong-mode';  return 1; }
  printf 'ok'; return 0
}

# --- issue 1317 (owner ROZHODNUTÉ 18.9.): bundle-vs-box release parity --------------------------
# The strih bundle is built on a SPECIFIC Ubuntu release (26.04 for strih-lx) and links THAT
# release's ffmpeg/Qt sonames; deploying a 24.04-built bundle onto a 26.04 box (or vice versa) would
# crash OBS at load (libavcodec60 vs libavcodec62, Qt 6.4 vs 6.10). The CI Stage step stamps a
# `TARGET-RELEASE: ubuntu-<rel>` line into STRIH_BUILD_FLAGS.txt; this predicate gates the box's
# VERSION_ID against it.

# strih_lx_release_parity_ok BUNDLE_FLAGS_TEXT BOX_VERSION_ID -> 0 iff BUNDLE_FLAGS_TEXT carries the
# whole line `TARGET-RELEASE: ubuntu-<BOX_VERSION_ID>` (the release the bundle was built on equals
# the box's /etc/os-release VERSION_ID). Fail-closed (returns 1): any release mismatch, an ABSENT
# TARGET-RELEASE marker (a pre-marker bundle that must be rebuilt, never trusted), or an empty
# BOX_VERSION_ID.
strih_lx_release_parity_ok() {
  local flags="${1:-}" version_id="${2:-}"
  [ -n "$version_id" ] || return 1
  printf '%s\n' "$flags" | grep -qxF "TARGET-RELEASE: ubuntu-${version_id}"
}

# --- issue 1317 item H: strih-lx USB-NIC xhci IRQ placement (NET_RX softirq off the OBS cores) ----
# The RTL8156B USB 2.5GbE NIC's xhci interrupt lands on ONE core (irqbalance is not installed, so
# the kernel parks it) where ~1.1 Gb/s of NDI NET_RX softirq collides with OBS's ndir:video/libobs
# threads (issue 1354, 22.9.2026, measured on strih-lx: moving the IRQ to an idle E-core cut the
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

# strih_nic_irq_affinity_unit_text -> the systemd unit body of
# systemd/strih-nic-irq-affinity.service (a system oneshot, RemainAfterExit, enable-only, ordered
# After network-pre.target). Kept byte-identical to the committed file by a parity test. issue 1317.
strih_nic_irq_affinity_unit_text() {
  cat <<'UNIT'
[Unit]
Description=strih-lx: pin the USB-NIC xhci IRQ off the OBS cores (issue 1317 item H)
Documentation=https://github.com/zbynekdrlik/camera-box
After=network-pre.target

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
strih_nic_irq_affinity_script_text() {
  cat <<'SCRIPT'
#!/usr/bin/env bash
# /usr/local/bin/strih-nic-irq-affinity.sh
set -euo pipefail
# strih-lx USB-NIC xhci IRQ placement (issue 1317 item H). Emitted by
# strih_nic_irq_affinity_script_text in scripts/lib/strih-provision.sh, installed by
# scripts/setup-strih.sh, run at boot by strih-nic-irq-affinity.service (and once live by the
# supervisor). It pins the USB NIC's xhci interrupt to an idle E-core so its ~1.1 Gb/s NDI NET_RX
# softirq never shares an OBS core (issue 1354: cut the genlock "slow output_video" rate ~75%). It
# resolves the placement FROM FACTS -- never a hard-coded IRQ number -- and FAILS LOUD (exit 1 + a
# journal line) on any unresolved step or a read-back mismatch. Inputs are env-overridable so a
# test drives fixtures.

SYS_ROOT="${SYS_ROOT:-/sys}"
PROC_INTERRUPTS="${PROC_INTERRUPTS:-/proc/interrupts}"
CPU_ATOM_FILE="${CPU_ATOM_FILE:-/sys/devices/cpu_atom/cpus}"
CPU_ONLINE_FILE="${CPU_ONLINE_FILE:-/sys/devices/system/cpu/online}"
IRQ_DIR="${IRQ_DIR:-/proc/irq}"
TARGET_IP="${STRIH_LX_TARGET_IP:-10.77.9.202}"

log() { printf 'strih-nic-irq-affinity: %s\n' "$*"; }
die() { printf 'strih-nic-irq-affinity: FATAL: %s\n' "$*" >&2; exit 1; }

# (1) NIC iface = the interface carrying the strih-lx address (STRIH_NIC_IFACE overrides).
iface="${STRIH_NIC_IFACE:-}"
if [ -z "$iface" ]; then
  iface="$(ip -o -4 addr show 2>/dev/null | awk -v ip="$TARGET_IP" '$4 ~ ("^" ip "/") { print $2; exit }' || true)"
fi
[ -n "$iface" ] || die "could not resolve the NIC iface carrying ${TARGET_IP} (set STRIH_NIC_IFACE)"

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
irqs=""
while IFS= read -r line; do
  case "$line" in *"$pcifn"*xhci_hcd*|*xhci_hcd*"$pcifn"*) : ;; *) continue ;; esac
  n="${line%%:*}"; n="${n//[[:space:]]/}"
  case "$n" in ''|*[!0-9]*) continue ;; esac
  irqs="${irqs:+$irqs }$n"
done < "$PROC_INTERRUPTS"
[ -n "$irqs" ] || die "no xhci_hcd IRQ row for ${pcifn} in ${PROC_INTERRUPTS}"

# (4) target cpu = the LAST cpu in cpu_atom (an E-core); fallback = highest online cpu.
target=""
if [ -r "$CPU_ATOM_FILE" ]; then
  target="$(tr ',' '\n' < "$CPU_ATOM_FILE" 2>/dev/null | sed 's/.*-//' | grep -E '^[0-9]+$' | sort -n | tail -1 || true)"
fi
if [ -z "$target" ] && [ -r "$CPU_ONLINE_FILE" ]; then
  target="$(tr ',' '\n' < "$CPU_ONLINE_FILE" 2>/dev/null | sed 's/.*-//' | grep -E '^[0-9]+$' | sort -n | tail -1 || true)"
fi
[ -n "$target" ] || die "could not determine an E-core / highest-online target cpu"

# (5) write smp_affinity_list + read it back; FAIL on any mismatch.
for irq in $irqs; do
  aff="${IRQ_DIR}/${irq}/smp_affinity_list"
  [ -w "$aff" ] || die "cannot write ${aff} (missing or no permission)"
  printf '%s\n' "$target" > "$aff" || die "write to ${aff} failed"
  got="$(tr -d '[:space:]' < "$aff" 2>/dev/null || true)"
  [ "$got" = "$target" ] || die "read-back mismatch on IRQ ${irq}: wrote ${target}, read '${got}'"
  log "IRQ ${irq} (xhci_hcd ${pcifn}, iface ${iface}) pinned to cpu ${target}"
done
log "done: xhci NIC IRQ(s) [${irqs}] pinned to cpu ${target}"
SCRIPT
}
