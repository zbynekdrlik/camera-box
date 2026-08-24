#!/usr/bin/env bash
#
# verify-imag.sh -- POST-PROVISION runtime acceptance gate for the imag notebook (#821), the imag
# twin of scripts/verify-device.sh (#454) for the cam1-6 fleet. See the extended header comment
# below (after `set -euo pipefail`) for the full rationale, checks list, and env vars.
#
set -euo pipefail
#
# WHY THIS EXISTS: there was no acceptance gate for the imag box at all. Consequence, live on
# 2026-07-27: the replacement notebook (10.77.9.187) was reported "verified booted from disk" when
# it was in fact only an installed OS -- no autologin, no openbox kiosk, no OBS, still on gdm3 with
# a login prompt. The user found it, not the tooling ("nechapem preco musim po tebe kazdu hlupost
# kontrolovat a testovat"). This script re-derives every fact from LIVE signals (systemd state,
# journald, `ls -la`, obs-websocket, dpkg/apt-mark) gathered fresh over SSH/network AFTER the swap
# runbook has run -- never trusting setup-imag.sh's own claim of success.
#
# Runbook (four mandatory phases, .claude/rules/imag-nb-provisioning.md):
#   1. INSTALL   scripts/install-imag-nb.sh --target-disk /dev/nvme0n1 --ip <addr> --yes
#   2. REBOOT    into the installed system
#   3. PROVISION IMAG_IP=<addr> sudo -E ./setup-imag.sh --yes                     (on the box)
#   4. VERIFY    scripts/verify-imag.sh                                          (#821, THIS script)
#
# Composes ALREADY-TESTED signals instead of reinventing them:
#   - scripts/setup-imag.sh    imag_cpu_isolation_plan() / imag_has_discrete_nvidia() (#816)
#   - scripts/verify-device.sh ndi_symlink_target() / ndi_regular_file_root_owned() /
#                              ndi_symlink_chain_ok() / ndi_symlink_version() /
#                              ndi_version_matches() (#454/#132/#547)
#   - scripts/lib/timesync-authority.sh  dpkg_status_installed() / timesync_authority_verdict() /
#                              timesync_gather_remote_snippet() (#591/#596)
#   - scripts/clock-offset-guard.sh  ptp_locked_from_pipe_json()/_journal(), offset_check(),
#                              dantesync_offset_verdict(), AND the NEW (#834) gm_source_ip_from_
#                              pipe_json()/gm_matches_expected()/gm_check() -- grandmaster IDENTITY,
#                              not just offset (a node can report is_locked:true while 15ms out on
#                              a FOREIGN grandmaster; is_locked alone must never be sufficient)
#   - scripts/imag-host.sh     imag_host_resolve() -- the #832 single source of truth for IMAG_IP
#   - scripts/imag_scenes.py (bare)         scene + Multiview membership (idempotent self-heal,
#                              the SAME mechanism the openbox autostart runs every boot)
#   - imag-obs-stop.sh + imag-obs-start.sh + wmctrl window count -- restarts OBS through the
#                              box's OWN operator scripts and re-counts, proving PERSISTENCE
#                              across a real restart (#840) rather than opening the projectors
#                              itself (the #840 self-establishing bug this replaced)
#
# Usage:
#   scripts/verify-imag.sh
#   scripts/verify-imag.sh --help
#
# Env:
#   IMAG_HOST_ACTIVE / IMAG_IP   which imag box (scripts/imag-host.sh; default: the #832 active one)
#   IMAG_USER                    desktop user on the box (default: newlevel)
#   IMAG_PW                      box password (default: newlevel -- same fleet default recording-
#                                e2e.sh/setup-imag.sh already use)
#   SSH_TIMEOUT                  SSH connect timeout in seconds (default: 10)
#   IMAG_OBS_RESTART_TIMEOUT     hard wall-clock cap on check (o)'s OBS restart (default: 60, #890)
#   IMAG_OBS_PROJECTOR_POLL_S    budget for projectors to reappear after the restart (default: 120, #890)
#   IMAG_READ_TIMEOUT            general per-read execution budget in seconds (default: 20, #1058)
#   IMAG_SLOW_READ_TIMEOUT       slow-read (dpkg/apt/journal/gather) execution budget (default: 60, #1058)
#   CLOCK_GUARD_BOUND_US         dantesync clock-offset bound in microseconds (default: 2000, #8)
#   RIG_GRANDMASTER_IP           the rig's PTP grandmaster every node must agree on (default:
#                                10.77.9.184 -- see #834)
#   IMAG_OBS_BASE_VERSION        pinned OBS base package version (default: the SAME pin
#                                setup-imag.sh uses, 32.1.2-0obsproject1~noble -- #824)
#   IMAG_HOSTNAME_EXPECT         expected `hostname` (default: imag-nb)
#
# Checks (all must pass; an unreadable/unreachable signal is a hard FAIL, never a silent pass):
#   (a) hostname + static IP as provisioned
#   (b) ssh.service enabled (NOT ssh.socket, noble's default)
#   (c) kernel on the HWE line (linux-image-generic-hwe-24.04 installed, #819)
#   (d) /proc/cmdline carries preempt=full AND is FREE of kernel isolcpus=/nohz_full= isolation
#       (#784/#842 -- isolcpus disables scheduler load balancing, piled 114/119 OBS threads onto
#       ONE core); the taskset AFFINITY-only pin (/etc/imag-isolated-cpus.conf) is unaffected and
#       is verified to match THIS box's own topology-derived plan via imag_cpu_isolation_plan (#816)
#   (e) display-manager.service -> lightdm.service; 50-imag-autologin.conf present; gdm3 absent
#   (f) zero failed systemd units
#   (g) openbox autostart present+executable+no unsubstituted __PLACEHOLDER__; openbox + obs
#       running as the desktop user
#   (h) OBS log shows the genlock render tick ENABLED marker; no libobs-version-mismatch warning
#       (#824 regression signature); DistroAV + NDI runtime loaded
#   (i) OBS WebSocket :4455 listening (the functional proof obs-websocket.so actually loaded --
#       #824's failure mode left OBS with ONLY distroav.so and :4455 dark)
#   (j) OBS base package version matches the pinned genlock build AND is apt-mark held (#824)
#   (k) NDI runtime pinned: libndi.so.6 -> libndi.so.6.3.2
#   (k2) when a discrete NVIDIA GPU IS present: driver installed + `prime-select nvidia`; when
#        absent: the step is correctly SKIPPED, never assumed either way (#816/#500)
#   (l) dantesync PTP LOCKED + a FRESH clock offset within bound + the SAME grandmaster as the
#       rest of the rig (#834 -- gates grandmaster IDENTITY, not just the offset)
#   (m) dantesync is the SOLE timesync authority (no systemd-timesyncd/chrony/ntp/linuxptp)
#   (n) scenes present (Cam 1-N, N = imag_scenes.py's own IMAG_SCENE_CAM_COUNT, default 7) and
#       Multiview populated (MV Cam 1-N)
#   (o) both projectors PRESENT (never opened by this gate) AND PERSIST across a real OBS
#       restart via imag-obs-stop.sh + imag-obs-start.sh -- exactly 1 Program (HDMI) + 1
#       Multiview (panel), no stray windows, both before AND after the restart (#840)
#   (p) operator scaffolding present (#791): /usr/local/bin/imag-obs-start.sh, wmctrl, the
#       right-click menu (~/.config/openbox/menu.xml), the wall-fallback image, the watchdog
#       installed-but-disabled (#756)
#   (q) OPERATOR parity (#791): the full canonical 17-scene ORDER (Scene, Cam N..Cam 1, resolume
#       imag, MV Cam 1..N, MW resolume imag) and all 10 canonical NDI-source bindings (7 fleet
#       cams + the 3 Resolume/overlay inputs no automated seeder creates) -- via
#       `imag_scenes.py --verify-parity`, read-only
#   (r) OBS stats dock persistence: global.ini carries a non-empty DockState (#791 -- OBS never
#       writes this on its own on a box that has run 24/7 without a clean exit; setup-imag.sh
#       seeds a known-good captured default the first time a box provisions)
#   (s) OBS threads are NOT concentrated onto a single CPU core (#842 -- the DIRECT SYMPTOM check,
#       independent of the (d) cmdline check, so a future variant of the same defect class can't
#       pass silently just because it doesn't happen to write a kernel-cmdline token)
#   (t) imag-obs.service supervision (#884, follow-up to #882): the unit is installed+enabled+
#       active; its Restart= property is EXACTLY on-failure (never "always" -- issue 788's
#       operator-fighting bug); the openbox autostart's boot launch goes through the unit rather
#       than calling the operator script directly; core dumps are ACTUALLY enabled -- kernel
#       core_pattern is a piped collector AND the live obs process's own /proc/<pid>/limits shows
#       an unlimited core-file size (proves LimitCORE=infinity is applied to the real process, not
#       just configured in the unit file); AND (#1015, claim-vs-reality) the live obs PID's own
#       /proc/<pid>/cgroup genuinely contains an imag-obs.service path component -- systemd's own
#       is-enabled/is-active bookkeeping can read correctly even while the ACTUAL running process
#       was launched directly (bypassing systemctl), leaving Restart=on-failure supervising
#       nothing; this per-PID cgroup read is the independent proof that cannot be spoofed that way
#   (u) power/thermal envelope (#1040): the MMIO RAPL PL1 long_term pin matches the provisioned
#       IMAG_PL1_W (default 45 W, #1162) and is enabled; every iGPU slpc_ignore_eff_freq knob reads 1;
#       thermald is PURGED (not installed/active/enabled); BOTH imag-power-envelope.service and
#       imag-power-envelope-guard.timer are enabled+active; TCPU is below the guard's step-down
#       ceiling; and the guard's journald tag is readable. Shares imag_power_envelope_verdict with
#       drift-guard's --check-imag facet. GUARD-STATE-AWARE (#1188): a pl1 DRIFT to the 25 W
#       step-down value AND a TCPU at/above the ceiling are downgraded to OK-with-note when the
#       guard's own /run state proves a LEGITIMATE thermal step-down (STEPPED=1) -- on the #1162
#       unit that clamp is the normal steady state, NOT foreign drift; a step-down the guard did not
#       make still FAILs. MUST run BEFORE check (o) below (its restart replaces the tracked obs
#       process, #884 ordering).
#   (v) power-button + lid + sleep protection (#727): imag-nb is a PRODUCTION box (a short
#       accidental power-button press suspended it during the 2026-07-12 live event). The running
#       logind reports HandlePowerKey/HandleSuspendKey/HandleHibernateKey/HandleLidSwitch = ignore
#       (the EFFECTIVE reloaded policy, read via `loginctl show-seat`) AND sleep.target/
#       suspend.target/hibernate.target/hybrid-sleep.target are all masked. setup-imag.sh step 5
#       persists this; a re-provision that silently lost it must FAIL here, not pass. Side-effect
#       free (pure systemd/logind reads), so it is appended at the END of the flow.
#   (w) touchpad usability (#779): /etc/X11/xorg.conf.d/30-touchpad-tap.conf carries the four
#       live-verified libinput options -- Tapping/TappingDrag/NaturalScrolling on + ScrollPixel
#       Distance 50 (tap-to-click + natural scroll + a gentler scroll step, the operator's touchpad
#       config, set live 2026-07-15). setup-imag.sh step 25 provisions it; a re-provision that
#       silently dropped it (or regenerated a partial/wrong file) must FAIL here. Pure static file
#       read (side-effect free), so it runs BEFORE check (o)'s restart (#884 ordering).
#   (x) Wake-on-LAN armed (#1103): the persisted NM setting 802-3-ethernet.wake-on-lan=magic on the
#       NDI-NIC connection, so a post-event powered-down/slept imag-nb is remotely wakeable via a
#       magic packet from dev1 (scripts/wake-box.sh imag-nb). NM re-applies it on every connection-up
#       (durable across reboot) and it reads SUDO-LESSLY, unlike the root-only runtime ethtool
#       Wake-on line. setup-imag.sh step 1 arms it; a re-provision that lost it must FAIL here. Pure
#       static read (side-effect free), so it runs BEFORE check (o)'s restart (#884 ordering). The
#       BIOS standby-power layer is a separate hands-on step (docs/wake-on-lan.md), not gated here.
#   (y) full max-performance persistence (#756/#791): the imag-maxperf trio -- imag-maxperf.service
#       enabled+active, /usr/local/sbin/imag-maxperf.sh present, /etc/udev/rules.d/
#       99-imag-maxperf-pm.rules present -- AND the runtime STATE reads performance (governor + EPP +
#       intel_pstate no_turbo=0 + platform_profile, optional knobs `absent`-tolerant for hardware
#       agnosticism, #816). Closes the hand-placed-never-provisioned gap #791 exists for; setup-imag.sh
#       step 26 provisions it. Pure sysfs/systemd reads (side-effect free), so it runs BEFORE check (o).
#   (z) display-path tear-free config (issue 1146): the picom vsync compositor is RUNNING + its
#       user systemd unit is ENABLED, HDMI is the xrandr PRIMARY (the projector is the vsync anchor),
#       the #841 iGPU freq pin holds, and the #779 tap conf is present. Runs the SHARED
#       imag_display_path_verdict (scripts/lib/imag-display-path.sh) -- the SAME verdict drift-guard
#       --check-imag and the E2E [0/8] preflight run. setup-imag.sh step 27 provisions picom + step
#       16 sets HDMI primary; a re-provision that lost either (or picom that failed to come up after
#       a reboot) must FAIL here. Pure ssh reads (side-effect free), appended at the END.
#
# Every remote helper this gate shells out to (wmctrl, python3) is preflighted BY NAME before use
# (#822 pattern) -- a missing tool is reported as a missing tool, never folded into a failed
# measurement (the #833 false blame: an absent wmctrl made the projector count read "0" and the
# gate accused stray-window accumulation instead of naming the missing binary).
#
# Exit: 0 iff every check passes. Non-zero if ANY check FAILs or is UNREADABLE.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cli-log.sh
. "$HERE/lib/cli-log.sh"            # RED/GREEN/YELLOW/BLUE/NC + log()/info()/warn()/err()
# shellcheck source=scripts/lib/timesync-authority.sh
. "$HERE/lib/timesync-authority.sh" # dpkg_status_installed/timesync_daemon_verdict/
                                     # timesync_authority_verdict/timesync_gather_remote_snippet
# shellcheck source=scripts/lib/imag-power-envelope.sh
. "$HERE/lib/imag-power-envelope.sh" # imag_power_envelope_verdict/imag_power_envelope_gather_
                                     # remote_snippet (#1040) -- SHARED with drift-guard.sh's
                                     # --check-imag power-envelope facet, never a driftable copy
# shellcheck source=scripts/lib/imag-display-path.sh
. "$HERE/lib/imag-display-path.sh"   # imag_display_path_verdict/imag_display_path_gather_remote_
                                     # snippet (#780/issue 1146) -- SHARED with drift-guard.sh's
                                     # --check-imag display-path facet + the E2E [0/8] preflight
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"      # offset_check/ptp_locked_from_pipe_json/_journal/
                                     # dantesync_offset_verdict/gm_source_ip_from_pipe_json/
                                     # gm_matches_expected/gm_check (#834)
# scripts/setup-imag.sh is sourced ONLY for its pure functions; its own
# `[ "${BASH_SOURCE[0]}" != "${0}" ]` guard skips setup-imag.sh's own destructive provisioning
# flow (identical convention to verify-device.sh sourcing clock-offset-guard.sh for pure
# functions only).
# shellcheck source=scripts/setup-imag.sh
. "$HERE/setup-imag.sh"              # imag_cpu_isolation_plan/imag_has_discrete_nvidia (#816)
# scripts/verify-device.sh is likewise sourced ONLY for its pure NDI-symlink functions -- the
# fleet's (g)/(o) checks are the identical decision imag needs for its OWN NDI runtime pin, and
# duplicating that awk logic here would be exactly the "second, driftable copy" #596/#595 exist to
# prevent. Its own guard skips verify-device.sh's live SSH flow (which needs a NAME argument this
# script never provides).
# shellcheck source=scripts/verify-device.sh
. "$HERE/verify-device.sh"           # ndi_symlink_target/ndi_regular_file_root_owned/
                                     # ndi_symlink_chain_ok/ndi_symlink_version/ndi_version_matches
# shellcheck source=scripts/imag-host.sh
. "$HERE/imag-host.sh"               # imag_host_resolve() -- the #832 single source of truth

# #816: sourcing setup-imag.sh (above) unconditionally runs its own top-level env-var defaults
# (STATIC_IP/PREFIX/NDI_PEER_CANDIDATES/IMAG_OBS_BASE_VERSION/...) and its `fail()`/`step()`
# helpers -- harmless (pure assignments, no network/root calls before ITS OWN source-guard).
# Likewise sourcing verify-device.sh sets ITS OWN SSH_USER/CAM_PW/SSH_TIMEOUT/... defaults. Both
# scripts' own `fail()` do an immediate `exit 1` (their native, single-script control flow); THIS
# script's own accumulate-and-report `ok()`/`fail()`/`warn()` (defined below, AFTER every source)
# intentionally SHADOW those for the rest of this file -- exactly the same convention
# verify-device.sh already uses to shadow cli-log.sh's own warn(). Bash resolves a function name
# dynamically at CALL time, so when a sourced pure function (e.g. imag_cpu_isolation_plan) calls
# `fail "..."` internally on a genuinely-impossible input, it invokes THIS script's own
# accumulate-style fail() too -- consistent, never a crash.

IMAG_USER="${IMAG_USER:-newlevel}"
IMAG_PW="${IMAG_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"
# #890: hard bounds for check (o)'s OBS restart-proof so this gate can NEVER hang again.
IMAG_OBS_RESTART_TIMEOUT="${IMAG_OBS_RESTART_TIMEOUT:-60}"       # wall-clock cap on the ssh restart
IMAG_OBS_PROJECTOR_POLL_S="${IMAG_OBS_PROJECTOR_POLL_S:-120}"    # budget for projectors to reappear
# #1058: per-CLASS execution budgets for every OTHER ssh read (issue 890 bounded only check (o)).
# ssh_box delegates to the bounded ssh_box_timeout with IMAG_READ_TIMEOUT (a general read budget --
# generous for fast sysfs/cat/systemctl/ls/pgrep reads); the genuinely-slow reads (dpkg/apt under a
# held lock, the dantesync journal, the timesync/power-envelope gathers) get IMAG_SLOW_READ_TIMEOUT.
# A blanket SSH_TIMEOUT execution cap would false-FAIL a healthy box on the slow reads (per-class,
# never a single flat cap).
IMAG_READ_TIMEOUT="${IMAG_READ_TIMEOUT:-20}"                    # general remote-read execution budget
IMAG_SLOW_READ_TIMEOUT="${IMAG_SLOW_READ_TIMEOUT:-60}"          # slow reads (dpkg/apt/journal/gather)
IMAG_CLOCK_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
# #837: max tolerated SPREAD (max-min) of the FRESH offset samples, in us -- the journal twin of
# the #836 HTTP spread check; same 2000us default + DANTESYNC_STABILITY_US knob as the gate.
IMAG_CLOCK_STABILITY_US="${DANTESYNC_STABILITY_US:-2000}"
DANTESYNC_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"
DANTESYNC_JOURNAL_MAX_AGE_S="${DANTESYNC_JOURNAL_MAX_AGE_S:-60}"
# #834: the rig's PTP grandmaster every node must agree on. Every DanteSync status-pipe/HTTP
# payload across this whole codebase (clock_offset_guard.rs's own real fixtures, dantesync-gate.sh's
# banner "GM = 10.77.9.184") pins this same address.
RIG_GRANDMASTER_IP="${RIG_GRANDMASTER_IP:-10.77.9.184}"
# #824: same pin + same default setup-imag.sh itself uses -- a superseded PPA binary breaks every
# stock plugin (obs-websocket included) if the base version drifts past the genlock build's own.
IMAG_OBS_BASE_VERSION="${IMAG_OBS_BASE_VERSION:-32.2.0-0obsproject1~noble}"
IMAG_HOSTNAME_EXPECT="${IMAG_HOSTNAME_EXPECT:-imag-nb}"

# =================================================================================================
# PURE functions (no network, no SSH -- unit-tested from tests/verify_imag_pure_functions.rs by
# sourcing this file; the BASH_SOURCE guard below skips the live flow when sourced. Same
# convention as scripts/verify-device.sh / scripts/setup-imag.sh.)
# =================================================================================================

# --- (a) hostname + static IP ---------------------------------------------------------------

# imag_hostname_matches ACTUAL EXPECTED -> 0 iff both non-empty and identical.
imag_hostname_matches() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# imag_static_ip_present IP_ADDR_TEXT EXPECTED_IP -> 0 iff EXPECTED_IP appears as a whole token in
# IP_ADDR_TEXT (an `ip -4 -o addr show` / `hostname -I` dump) -- never a substring false-match
# (e.g. "10.77.9.18" must not match "10.77.9.187").
imag_static_ip_present() {
  local text="$1" ip="$2"
  [ -n "$ip" ] || return 1
  grep -qF " ${ip} " <<<" $text " || grep -qF " ${ip}/" <<<" $text "
}

# --- (b) ssh.service (not ssh.socket) --------------------------------------------------------

# imag_sshd_via_service SERVICE_ENABLED SOCKET_ENABLED -> 0 iff ssh.service is enabled AND
# ssh.socket is NOT enabled. noble enables ssh.socket (socket-activation) by default; the fleet
# convention (see verify-device.sh's own header) is the plain always-on ssh.service.
imag_sshd_via_service() {
  local svc sock
  svc="$(printf '%s' "$1" | tr -d '[:space:]')"
  sock="$(printf '%s' "$2" | tr -d '[:space:]')"
  [ "$svc" = "enabled" ] || return 1
  case "$sock" in
    enabled) return 1 ;;
    *) return 0 ;;
  esac
}

# --- (c) kernel on the HWE line (#819) --------------------------------------------------------

# imag_hwe_kernel_installed DPKG_STATUS -> 0 iff DPKG_STATUS (the `dpkg -s
# linux-image-generic-hwe-24.04` Status: line) shows the HWE meta-kernel is genuinely installed.
# Thin, named wrapper over the shared dpkg_status_installed() (scripts/lib/timesync-authority.sh)
# -- reused rather than reinvented, same discipline as every other dpkg-status check in this file.
imag_hwe_kernel_installed() {
  dpkg_status_installed "$1"
}

# --- (d) kernel cmdline: preempt=full + NO kernel isolcpus/nohz_full isolation (#289/#482/#784/#842)

# imag_cmdline_has_preempt_full CMDLINE -> 0 iff CMDLINE carries the whole-token `preempt=full`
# flag (the #482 low-latency-kernel config).
imag_cmdline_has_preempt_full() {
  grep -qE '[[:space:]]preempt=full[[:space:]]' <<<" $1 "
}

# imag_cmdline_free_of_kernel_isolation CMDLINE -> 0 iff CMDLINE carries NEITHER an `isolcpus=`
# NOR a `nohz_full=` token. #784/#842: isolcpus= removes the listed CPUs from the kernel
# scheduler's load-balancing domains -- measured live to pile 114 of OBS's 119 threads onto ONE
# core while sibling cores in the SAME affinity mask sat idle (60fps -> ~53fps NDI receive,
# 7-10 underruns/s). setup-imag.sh must never write either token again (the taskset AFFINITY pin,
# /etc/imag-isolated-cpus.conf, is unaffected and stays) -- this is #784's own outstanding
# acceptance-gate item, deferred between #780/#791 since 2026-07-15 and the direct cause of the
# #842 recurrence on the replacement notebook. A hard FAIL, never a warning.
imag_cmdline_free_of_kernel_isolation() {
  ! grep -qE '[[:space:]](isolcpus|nohz_full)=' <<<" $1 "
}

# --- (e) display-manager -> lightdm + autologin; gdm3 absent ---------------------------------

# imag_autologin_conf_ok TEXT USER -> 0 iff TEXT (the 50-imag-autologin.conf drop-in contents)
# carries both `autologin-user=USER` and `autologin-session=openbox` as whole lines.
imag_autologin_conf_ok() {
  local text="$1" user="$2"
  [ -n "$user" ] || return 1
  grep -qxF "autologin-user=${user}" <<<"$text" \
    && grep -qxF "autologin-session=openbox" <<<"$text"
}

# imag_pkg_absent STATUS -> 0 iff STATUS shows the package is genuinely NOT installed. The
# inverse of dpkg_status_installed -- named for the gdm3-must-be-gone call site's own clarity.
imag_pkg_absent() {
  ! dpkg_status_installed "$1"
}

# --- (f) zero failed systemd units ------------------------------------------------------------

# imag_failed_units_ok TEXT -> 0 iff TEXT (the `systemctl list-units --failed --no-legend` dump)
# is empty/blank.
imag_failed_units_ok() {
  [ -z "$(printf '%s' "$1" | tr -d '[:space:]')" ]
}

# --- (g) openbox autostart + openbox/obs running as the desktop user -------------------------

# imag_autostart_placeholders_resolved TEXT -> 0 iff TEXT (the openbox autostart script contents)
# contains NO unsubstituted `__WORD__`-shaped placeholder (setup-imag.sh sed's __PYBIN__/__SCN__/
# __ISOLCPUS__ in at provisioning time -- a leftover literal means the sed step silently no-op'd).
imag_autostart_placeholders_resolved() {
  ! grep -qE '__[A-Za-z_]+__' <<<"$1"
}

# imag_regular_file_present MODE -> 0 iff MODE (the first whitespace-token of an `ls -la` line,
# e.g. "-rwxr-xr-x") denotes a REGULAR file is present (mode string starts with '-'). Empty MODE
# (no ls output -- file absent/unreadable) correctly fails.
imag_regular_file_present() {
  [ "${1:0:1}" = "-" ]
}

# imag_regular_executable_file MODE -> 0 iff MODE denotes a regular file (as above) AND the owner
# execute bit is set (4th character of the mode string is 'x').
imag_regular_executable_file() {
  imag_regular_file_present "$1" && [ "${1:3:1}" = "x" ]
}

# imag_proc_running PS_LINES NAME -> 0 iff NAME appears as an EXACT line in PS_LINES (a `ps -u
# USER -o comm=` dump, one bare process name per line -- exact match avoids a substring false-hit,
# e.g. "obs" must not match "obs-plugin-helper").
imag_proc_running() {
  grep -qxF "$2" <<<"$1"
}

# --- (h) OBS log: genlock tick, no version-mismatch, DistroAV + NDI loaded -------------------

# imag_obs_log_shows_genlock_tick LOG_TEXT -> 0 iff LOG_TEXT carries a genlock capability marker.
# EXACT same regex family as setup-imag.sh's own step-18 verify + scripts/drift-guard.sh's
# genlock_capability_from_log -- never a second, drifting pattern.
imag_obs_log_shows_genlock_tick() {
  # #1183: -a + LC_ALL=C -> byte-literal match, so invalid-UTF-8 bytes in the OBS log (DistroAV
  # mojibake) can never suppress a marker that IS present in a UTF-8 locale. Here-string, NOT a
  # `printf '%s' "$1" | grep -q` pipe: under `set -euo pipefail`, grep -q exits at the first match
  # and SIGPIPEs the printf writer on a >64 KiB log (the marker is a startup line at the top; live
  # logs are 173 KB-40 MB) -> rc=141 -> pipefail false-FAILs a healthy box. The here-string has no
  # writer process (bash materializes the body to a temp file), so it is SIGPIPE-immune at any size
  # -- the issue-1047 sanctioned form, matching the three sibling matchers below.
  LC_ALL=C grep -aiqE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' <<<"$1"
}

# imag_obs_log_no_version_mismatch LOG_TEXT -> 0 iff LOG_TEXT contains NO "compiled with newer
# libobs" warning -- the #824 regression signature (a superseded OBS base whose libobs is OLDER
# than a stock plugin's build refuses to load it; left OBS with ONLY distroav.so, no
# obs-websocket, no encoders).
imag_obs_log_no_version_mismatch() {
  # #1183: -a + LC_ALL=C so invalid-UTF-8 bytes cannot blind this NEGATIVE check into falsely
  # reporting "no mismatch".
  ! LC_ALL=C grep -aqi 'compiled with newer libobs' <<<"$1"
}

# imag_obs_log_shows_distroav_loaded LOG_TEXT -> 0 iff LOG_TEXT shows the DistroAV plugin loaded
# (same grep verify-device.sh's own log-verify convention and setup-imag.sh step 18 both use).
imag_obs_log_shows_distroav_loaded() {
  # #1183: -a + LC_ALL=C (invalid-UTF-8-safe, same audit as the other log-text matchers)
  LC_ALL=C grep -aqi '\[distroav\] plugin loaded' <<<"$1"
}

# imag_obs_log_shows_ndi_loaded LOG_TEXT -> 0 iff LOG_TEXT shows the NDI runtime initialized.
imag_obs_log_shows_ndi_loaded() {
  # #1183: -a + LC_ALL=C (invalid-UTF-8-safe, same audit as the other log-text matchers)
  LC_ALL=C grep -aqi 'NDI library initialized' <<<"$1"
}

# --- (j) OBS base version pin + apt-mark hold (#824) ------------------------------------------

# imag_obs_base_version_matches ACTUAL EXPECTED -> 0 iff both non-empty and identical.
imag_obs_base_version_matches() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# imag_pkg_is_held HOLD_LIST_TEXT NAME -> 0 iff NAME appears as an exact line in HOLD_LIST_TEXT
# (an `apt-mark showhold` dump).
imag_pkg_is_held() {
  grep -qxF "$2" <<<"$1"
}

# --- (k2) NVIDIA dGPU: driver + prime-select when present, correctly skipped when absent (#816) -

# imag_nvidia_verdict HAS_DGPU DRIVER_STATUS PRIME_OUTPUT -> "ok" | "fail" | "na".
#   na   -- HAS_DGPU is not "yes" (the box's ACTUAL hardware has no discrete NVIDIA GPU, e.g. the
#           i5-13420H replacement notebook) -- the step is correctly SKIPPED, never a fail (#816:
#           the driver step used to be mandatory + fail-hard, aborting provisioning on a box that
#           simply has no dGPU).
#   fail -- HAS_DGPU is "yes" but the driver package is not installed, OR `prime-select query`
#           does not report "nvidia".
#   ok   -- HAS_DGPU is "yes", driver installed, PRIME set to nvidia.
imag_nvidia_verdict() {
  local has_dgpu="$1" driver_status="$2" prime_output="$3"
  if [ "$has_dgpu" != "yes" ]; then
    printf 'na\n'
    return 0
  fi
  if ! dpkg_status_installed "$driver_status"; then
    printf 'fail\n'
    return 0
  fi
  if [ "$(printf '%s' "$prime_output" | tr -d '[:space:]')" = "nvidia" ]; then
    printf 'ok\n'
  else
    printf 'fail\n'
  fi
}

# --- (n) scenes present + Multiview populated (imag_scenes.py, bare) -------------------------

# imag_scenes_output_ok STDOUT EXPECTED_COUNT -> 0 iff STDOUT (imag_scenes.py's own printed
# report) shows BOTH the "Cam N" scenes and the "MV Cam N" scenes fully present, EXPECTED_COUNT/
# EXPECTED_COUNT each. Text-based (not just the exit code) so a caller can distinguish which set
# is short from the SAME captured output the operator would read. EXPECTED_COUNT is NOT a literal
# here -- #791: this used to hardcode "6/6", which silently kept passing even after cam7 (#753)
# was wired into the fleet and imag_scenes.py's own CAMS range still excluded it (the exact bug
# this whole ticket exists to catch). The caller passes the CURRENT expected count (derived from
# imag_scenes.py's own IMAG_SCENE_CAM_COUNT default/override), never a re-hardcoded number here.
#
# #843: the MV line's real text is "MV scenes: N/N (multiview, low-bw) OK" -- imag_scenes.py's
# own f-string prints that qualifier BETWEEN the count and OK. The regex below matches it
# verbatim rather than an assumed "MV scenes: N/N OK" shape, which could never match on any box
# (confirmed via git log -p: wrong since the check's very first commit, not a regression).
imag_scenes_output_ok() {
  local out="$1" count="$2"
  [ -n "$count" ] || return 1
  # Line-anchored (^) -- "MV scenes: N/N ... OK" contains "scenes: N/N ... OK" as a plain
  # SUBSTRING, so an unanchored -F match would wrongly pass the main-scenes check off the MV line
  # alone even when the main "scenes: N/N" line reports a shortfall. Anchor each to its own
  # line's start.
  grep -qE "^scenes: ${count}/${count} OK" <<<"$out" \
    && grep -qE "^MV scenes: ${count}/${count} \(multiview, low-bw\) OK" <<<"$out"
}

# --- (q) canonical scene ORDER + NDI-source bindings (imag_scenes.py --verify-parity, #791) ---

# imag_parity_output_ok STDOUT -> 0 iff STDOUT (imag_scenes.py --verify-parity's own printed
# report) shows BOTH "scene order: OK" and "ndi sources: OK" as whole lines. This is the check
# that actually catches a box whose reprovision silently reproduced only PART of the operator's
# real layout (missing Cam 7/MV Cam 7, missing the Resolume/overlay NDI sources, or a scrambled
# scene ORDER) -- (n) above only ever proved the "Cam N"/"MV Cam N" COUNT, never the full 17-scene
# set (incl. "resolume imag"/"MW resolume imag", which NO automated seeder creates) nor the order.
imag_parity_output_ok() {
  grep -qxF "scene order: OK" <<<"$1" \
    && grep -qxF "ndi sources: OK" <<<"$1"
}

# --- (r) OBS stats dock persisted (DockState in global.ini, #791) -----------------------------

# imag_dockstate_present GLOBAL_INI_TEXT -> 0 iff GLOBAL_INI_TEXT (the box's own
# ~/.config/obs-studio/global.ini contents) carries a non-empty `DockState=` line under
# [BasicWindow]. OBS only ever WRITES this key on a clean exit (imag-nb runs 24/7 and has
# therefore never shed one on its own) -- setup-imag.sh seeds a known-good captured default (see
# its own step-13 comment) the FIRST time a box provisions, so this check simply proves that seed
# (or a real captured layout from an actual operator clean-exit) is present on disk; it cannot
# prove the CURRENTLY RUNNING session has the dock docked live (that would need a UI-level read
# this repo has no mechanism for), only that the box will come back with it after its next
# restart -- which is exactly the persistence gap #791 reported.
imag_dockstate_present() {
  local text="$1" line
  line="$(printf '%s\n' "$text" | grep '^DockState=' | head -1)"
  [ -n "${line#DockState=}" ]
}

# --- (s) OBS thread distribution: no single CPU core concentrates OBS's threads (#842) --------

# imag_obs_thread_concentration_ok PSR_LIST -> 0 iff PSR_LIST (raw `ps -L -o psr= -C obs` output --
# one CPU-core number per OBS thread, one per line) shows NO single core holding more than ~60% of
# the live thread count. This is the #842 DIRECT-SYMPTOM check: whatever mechanism causes OBS's
# threads to pile onto one core (isolcpus was the measured cause, but a future variant might not
# write a kernel-cmdline token at all) must not pass this gate silently. An empty/unreadable list
# (OBS not running, or the remote `ps` failed) never silently passes -- #833 class, a measured
# zero is not the same as "check skipped". Measured #842 signature: 114 of 119 threads (96%) on
# ONE core; the fixed distribution spreads 19/16/24/26/12/17 across 6 cores (max 23%).
imag_obs_thread_concentration_ok() {
  local list="$1" total max
  # #1183: `ps -L -o psr= -C obs` RIGHT-PADS its single column to the widest value's width, so real
  # lines are "  6" / " 11" (verified cat -A: "  6$"), NOT bare "6". A `^[0-9]+$` grep matches ZERO
  # padded lines -> total=0 -> false-FAIL on a healthy box. Normalise to the bare core number with
  # `awk '{print $1}'` FIRST, applied IDENTICALLY to the total count AND the per-core max so the two
  # can never disagree; keep the `^[0-9]+$` anchor to still reject genuine non-numeric junk.
  total="$(printf '%s\n' "$list" | awk '{print $1}' | grep -cE '^[0-9]+$')"
  [ "$total" -gt 0 ] || return 1
  max="$(printf '%s\n' "$list" | awk '{print $1}' | grep -E '^[0-9]+$' | sort -n | uniq -c \
    | awk '{print $1}' | sort -rn | head -1)"
  [ -n "$max" ] || return 1
  # fail when max/total > 60% -- integer form: max*100 > total*60
  [ "$((max * 100))" -le "$((total * 60))" ]
}

# --- (o) projector count -- exactly 1 Program + 1 Multiview (#756/#758) ----------------------

# imag_projector_counts_ok MV_COUNT PGM_COUNT -> 0 iff BOTH are the exact string "1" (numeric
# equality would silently accept "01"/" 1" from a malformed read; requiring the literal "1"
# matches recording-e2e.sh's own `-eq 1` gate MORE strictly, catching a non-numeric read too --
# `-eq` on a non-numeric string throws a shell arithmetic error rather than failing closed, so a
# plain string compare is the safer contract here).
imag_projector_counts_ok() {
  [ "$1" = "1" ] && [ "$2" = "1" ]
}

# imag_obs_service_restart_cmd -> prints the REMOTE command check (o) runs to restart OBS (#890).
# It restarts through imag-obs.service, NEVER a direct imag-obs-start.sh call: since #882 that
# wrapper ends in `wait "$OBS_PID"` (correct for the Type=simple unit -- systemd needs obs to be
# the tracked main process), so invoking it DIRECTLY over ssh never returns and hangs the whole
# gate forever. `systemctl --user restart` returns as soon as the unit re-forks obs (systemd owns
# the blocking wait) AND keeps the new obs INSIDE the unit's cgroup (supervised, LimitCORE applied
# -- unlike the old bare invocation, the #1015 untracked-process class). Since #884 the box's OWN
# boot path IS this unit, so the service is also the operator-faithful "real restart" #840 wanted.
# XDG_RUNTIME_DIR is exported so a non-graphical ssh session can reach the --user bus
# (imag-obs-supervision.md); $(id -u) stays single-quoted so it evaluates ON the box.
imag_obs_service_restart_cmd() {
  printf '%s' 'export XDG_RUNTIME_DIR="/run/user/$(id -u)"; systemctl --user restart imag-obs.service'
}

# --- (p) operator scaffolding present (#791) ---------------------------------------------------

# imag_openbox_menu_looks_valid TEXT -> 0 iff TEXT (the ~/.config/openbox/menu.xml contents) is
# non-empty and contains a `<menu` tag (basic XML sanity -- never pass on an empty/corrupted file).
imag_openbox_menu_looks_valid() {
  [ -n "$(printf '%s' "$1" | tr -d '[:space:]')" ] && grep -qi '<menu' <<<"$1"
}

# imag_openbox_root_menu_bound RC_XML_TEXT -> 0 iff RC_XML_TEXT (the EFFECTIVE openbox rc.xml the
# box will load -- the user's ~/.config/openbox/rc.xml if present, else the stock
# /etc/xdg/openbox/rc.xml) binds the desktop right-click (the Root mouse context, Right button) to
# `ShowMenu root-menu`, so the provisioned menu.xml (`<menu id="root-menu">`, #785) is actually
# reachable (#1095). Newlines/tabs are flattened first (the binding spans several indented lines),
# then XML comments (`<!-- ... -->`) are STRIPPED so a COMMENTED-OUT binding cannot false-PASS a
# box whose live binding actually points elsewhere (the sed stops at the first `-->`, since `--` is
# illegal inside an XML comment, so it never over-strips across a real binding). The match then
# requires, INSIDE a `<context name="Root">...</context>` block, a `<mousebind ... button="Right"
# ...>` whose `<action ... name="ShowMenu" ...>` names `<menu>root-menu</menu>`. Attribute ORDER is
# not significant in XML, so `button`/`name` are matched positionally-independent
# (`\s[^>]*\bATTR=`). Scoped to the Root mouse context ON PURPOSE -- a root-menu named only in a
# keybind, or a Root right-click bound to a DIFFERENT menu, must NOT pass. Tolerant of both XML
# attribute quote styles ([\x22\x27] = " or '). Assert-only (#1095 design (b)): the acceptance gate
# fails loud, it never rewrites a hand-tuned operator rc.xml. Runs LOCALLY on dev1 against the
# ssh-read text (GNU grep 3.11 and ugrep both provide -P/PCRE2), never on the box.
imag_openbox_root_menu_bound() {
  local flat root_block
  flat="$(printf '%s' "$1" | tr '\n\t' '  ' | sed 's/<!--\([^-]\|-[^-]\)*-->//g' | tr -s ' ')"
  root_block="$(printf '%s' "$flat" | grep -oP '<context\s+name=[\x22\x27]Root[\x22\x27]\s*>.*?</context>' || true)"
  [ -n "$root_block" ] || return 1
  grep -qP '<mousebind\s[^>]*\bbutton=[\x22\x27]Right[\x22\x27][^>]*>.*?<action\s[^>]*\bname=[\x22\x27]ShowMenu[\x22\x27][^>]*>\s*<menu>\s*root-menu\s*</menu>' <<<"$root_block"
}

# imag_watchdog_installed_but_disabled SCRIPT_MODE UNIT_LIST_TEXT IS_ENABLED -> 0 iff the
# watchdog script is present+executable (SCRIPT_MODE, an `ls -la` mode field), its systemd unit
# is genuinely installed (UNIT_LIST_TEXT, a `systemctl list-unit-files imag-obs-watchdog.service
# --no-legend` dump, contains the unit name), AND it is NOT enabled to auto-start (IS_ENABLED is
# exactly "disabled" -- the #791 agreed model: "boot autostart [imag-obs-start.sh] + menu, ziadny
# auto-respawn [watchdog] -- enable az po fixe #788"). "not-found"/masked/enabled/static are all
# WRONG states here -- "not-found" means the unit was never installed at all (missing, not
# disabled); anything other than plain "disabled" means the installed-but-off contract drifted.
imag_watchdog_installed_but_disabled() {
  local script_mode="$1" unit_list="$2" enabled
  enabled="$(printf '%s' "$3" | tr -d '[:space:]')"
  imag_regular_executable_file "$script_mode" || return 1
  grep -qF "imag-obs-watchdog.service" <<<"$unit_list" || return 1
  [ "$enabled" = "disabled" ]
}

# --- (t) imag-obs.service supervision (#884, follow-up to #882) -------------------------------
#
# The live box (10.77.9.182) already runs the boot launch through the supervised systemd unit
# (enabled+active, Restart=on-failure), but setup-imag.sh used to write the OLD direct
# imag-obs-start.sh call, and this whole area had ZERO acceptance checks -- so a fresh reprovision
# would silently regress to the unsupervised state that produced the 2026-07-30 ~70-minute OBS
# outage, and this gate would have certified that regression as ALL CLEAR.

# imag_obs_service_state_ok IS_ENABLED IS_ACTIVE -> 0 iff both trimmed args are EXACTLY "enabled"
# and "active" respectively (a `systemctl --user is-enabled`/`is-active` reply, whitespace-trimmed
# since a real SSH capture carries a trailing newline). A re-provisioned box with the unit merely
# installed (is-enabled would report "disabled") or enabled-but-not-running (is-active anything
# other than "active") must fail here.
imag_obs_service_state_ok() {
  local enabled active
  enabled="$(printf '%s' "$1" | tr -d '[:space:]')"
  active="$(printf '%s' "$2" | tr -d '[:space:]')"
  [ "$enabled" = "enabled" ] && [ "$active" = "active" ]
}

# imag_obs_service_restart_is_on_failure RESTART_PROPERTY_LINE -> 0 iff the trimmed
# `systemctl --user show imag-obs.service --property=Restart` reply is EXACTLY "Restart=on-failure"
# -- never "always" (issue 788's operator-fighting bug: a tight auto-relaunch loop fights a
# deliberate manual quit, which is why the imag-obs-watchdog was stood down in the first place).
imag_obs_service_restart_is_on_failure() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "Restart=on-failure" ]
}

# imag_autostart_launches_via_service_not_script AUTOSTART_TEXT -> 0 iff AUTOSTART_TEXT (the live
# openbox autostart file's contents, already read at check (g)) calls
# `systemctl --user start imag-obs.service` and does NOT directly invoke imag-obs-start.sh as CODE
# (#884 -- that direct call is exactly the divergence that would silently strip supervision on a
# re-provision). Full-line comments (this repo's own header/inline comments legitimately mention
# imag-obs-start.sh in prose) are stripped FIRST so a comment referencing the script name is never
# mistaken for a real call -- the same self-collision class this repo's CLAUDE.md GOTCHA warns
# about for static-anchor tests.
imag_autostart_launches_via_service_not_script() {
  local code
  code="$(printf '%s\n' "$1" | grep -vE '^[[:space:]]*#')"
  grep -qF "systemctl --user start imag-obs.service" <<<"$code" \
    && ! grep -q "imag-obs-start.sh" <<<"$code"
}

# imag_core_pattern_captures_dumps CORE_PATTERN_TEXT -> 0 iff CORE_PATTERN_TEXT (the box's
# `/proc/sys/kernel/core_pattern`) is a PIPED collector (starts with '|', e.g. systemd-coredump or
# apport) rather than a bare/relative pattern like "core" or "core.%p" -- #882's root cause: even
# with an unlimited ulimit, an unpiped pattern can silently drop a core (wrong cwd, a read-only
# rootfs), and a piped collector is the only configuration this repo has verified actually keeps a
# crash inspectable.
imag_core_pattern_captures_dumps() {
  case "$1" in '|'*) return 0 ;; *) return 1 ;; esac
}

# imag_obs_cgroup_shows_service_unit CGROUP_TEXT -> 0 iff CGROUP_TEXT (a live `/proc/<pid>/cgroup`
# dump -- either the cgroup-v2 unified single line "0::/...path", or one-or-more cgroup-v1 hybrid
# "hierarchy-id:controller-list:/...path" lines) shows AT LEAST ONE line whose path ends in the
# exact component "/imag-obs.service". This is the #1015 claim-vs-reality proof: (t)'s own
# is-enabled/is-active/Restart= checks above are all systemd BOOKKEEPING -- they can read
# correctly even when the box's live incident (a manual `imag-obs-start.sh` recovery, bypassing
# systemctl entirely) leaves the ACTUAL running obs process outside any unit's cgroup. Reading the
# LIVE process's own cgroup membership cannot be spoofed by stale/inconsistent systemd state the
# way a second is-active read could. Component-boundary matched (never a bare substring) so a
# hypothetical differently-named unit sharing a prefix/suffix (e.g. "imag-obs.service-old") never
# false-matches.
imag_obs_cgroup_shows_service_unit() {
  grep -qE '(^|/)imag-obs\.service($|/)' <<<"$1"
}

# imag_obs_core_dumps_enabled LIMITS_LINE -> 0 iff LIMITS_LINE (a `grep -i "Max core file size"
# /proc/<obs-pid>/limits` line from the LIVE obs process) shows BOTH the soft and hard column as
# "unlimited". This proves LimitCORE=infinity is actually APPLIED to the real running process, not
# merely configured in the unit file -- the #882 root cause: ulimit -c was 0, so the 2026-07-30
# segfault produced nothing debuggable.
imag_obs_core_dumps_enabled() {
  grep -qE '^Max core file size[[:space:]]+unlimited[[:space:]]+unlimited' <<<"$1"
}

# imag_powerkey_protection_ok LOGINCTL MASKED -> 0 iff the running box is protected against an
# accidental power-button / lid / suspend / hibernate action (#727 -- imag-nb is a PRODUCTION box;
# a short power-button press suspended it during the 2026-07-12 live event). LOGINCTL is a
# `loginctl show-seat` dump: its `Handle*=` lines carry the EFFECTIVE, reloaded logind policy on
# this box (`systemctl show systemd-logind -p HandlePowerKey` reads EMPTY here, so it is NOT a
# viable source). MASKED is one `<unit>=<state>` line per sleep target. Every key must read
# EXACTLY `=ignore` (whole-line grep -qxF, so HandlePowerKey=ignore is never satisfied by the
# DISTINCT HandlePowerKeyLongPress=ignore line), and every sleep target must be `masked`.
imag_powerkey_protection_ok() {
  local loginctl="$1" masked="$2" k t
  for k in HandlePowerKey HandleSuspendKey HandleHibernateKey HandleLidSwitch; do
    grep -qxF "${k}=ignore" <<<"$loginctl" || return 1
  done
  for t in sleep.target suspend.target hibernate.target hybrid-sleep.target; do
    grep -qxF "${t}=masked" <<<"$masked" || return 1
  done
  return 0
}

# imag_touchpad_conf_ok CONF_TEXT -> 0 iff the touchpad InputClass (#779) carries BOTH the selector
# (MatchIsTouchpad "on" + Driver "libinput" -- WITHOUT them libinput never binds the class and the
# config is inert, so a file that dropped either selector but kept the options must still FAIL) AND
# the four live-verified libinput options at their EXACT values: Tapping/TappingDrag/NaturalScrolling
# "on" and ScrollPixelDistance "50" (the user's final tuning; the libinput default 15 is far too
# sensitive). A reprovision that regenerated a PARTIAL file (a missing selector/option) or the WRONG
# value must FAIL, not pass on mere presence -- the gate proves durability, not just existence.
# Purely textual (unit-tested), whitespace-tolerant, here-string fed (no SIGPIPE-under-pipefail risk).
imag_touchpad_conf_ok() {
  local conf="$1" pat
  for pat in \
    'MatchIsTouchpad[[:space:]]+"on"' \
    'Driver[[:space:]]+"libinput"' \
    'Option[[:space:]]+"Tapping"[[:space:]]+"on"' \
    'Option[[:space:]]+"TappingDrag"[[:space:]]+"on"' \
    'Option[[:space:]]+"NaturalScrolling"[[:space:]]+"on"' \
    'Option[[:space:]]+"ScrollPixelDistance"[[:space:]]+"50"'; do
    grep -qE "$pat" <<<"$conf" || return 1
  done
  return 0
}

# imag_wol_enabled_ok WOL_VALUE -> 0 iff WOL_VALUE (the `nmcli -g 802-3-ethernet.wake-on-lan
# connection show <con>` output, trimmed) is EXACTLY "magic" -- the persisted magic-packet
# Wake-on-LAN setting on imag's NDI-NIC connection (#1103, provisioned by setup-imag.sh step 1). NM
# re-applies this on every connection-up, so it is the DURABLE source of truth (survives reboot) and
# is readable SUDO-LESSLY, unlike the runtime `ethtool <nic>` Wake-on line (root-only). A
# "default"/"none"/empty value FAILs (WoL not provisioned -> a post-event powered-down box would not
# be remotely wakeable); so does "g" (that is the runtime ethtool word, not the NM value) and a
# "magic secureon" (password-protected wake our passwordless wake-box.sh sender cannot trigger).
# Purely textual (unit-tested), whitespace-tolerant. Pure.
imag_wol_enabled_ok() {
  local v; v="$(printf '%s' "${1:-}" | tr -d '[:space:]')"
  [ "$v" = "magic" ]
}

# imag_maxperf_state_ok STATE_TEXT -> 0 iff the gathered max-performance runtime STATE (#756/#791)
# reads performance. STATE_TEXT is labelled KNOB=VALUE lines gathered over SSH:
#   GOVERNOR=<value>                 (mandatory backbone -- must be `performance`; a missing/empty
#                                     line means the gather was unreadable and must FAIL, never
#                                     silently pass -- the #833 measured-zero class)
#   EPP=<value|absent>               (optional; if present must be `performance`)
#   NO_TURBO=<value|absent>          (optional; if present must be `0` -- turbo ENABLED)
#   PLATFORM_PROFILE=<value|absent>  (optional; if present must be `performance`)
# The optional-knob `absent` tolerance keeps the check hardware-agnostic (#816): a box without
# intel_pstate/platform_profile simply omits those knobs, exactly as imag-maxperf.sh only writes the
# knobs that exist (`[ -f ]`/`command -v` guarded). This proves the persistence actually TOOK EFFECT
# (the #840 lesson: presence of the unit != the state it should produce), not merely that the unit
# file exists. Purely textual (unit-tested), here-string fed (no SIGPIPE-under-pipefail risk, #1047).
imag_maxperf_state_ok() {
  local state="${1:-}" k v
  local gov="" epp="" turbo="" prof=""
  while IFS='=' read -r k v; do
    case "$k" in
      GOVERNOR) gov="$v" ;;
      EPP) epp="$v" ;;
      NO_TURBO) turbo="$v" ;;
      PLATFORM_PROFILE) prof="$v" ;;
    esac
  done <<<"$state"
  [ "$gov" = performance ] || return 1
  case "$epp"   in ''|absent|performance) ;; *) return 1 ;; esac
  case "$turbo" in ''|absent|0)           ;; *) return 1 ;; esac
  case "$prof"  in ''|absent|performance) ;; *) return 1 ;; esac
  return 0
}

# --- (u) power/thermal-envelope ACCEPTANCE reclassification (guard-state-aware, #1188) --------
# The SHARED imag_power_envelope_verdict (scripts/lib/imag-power-envelope.sh) is deliberately
# guard-BLIND: it reports a pl1 DRIFT whenever the live PL1 != the pinned watts. That is CORRECT for
# drift-guard's STRICT [0/8] preflight (it must refuse a run during a clamp episode), but on the
# #1162 unit the guard's OWN legitimate thermal step-down (PL1 -> 25 W at >=93 C) is the PERMANENT
# steady state, so the ACCEPTANCE gate must not read it as foreign drift. These two functions encode
# the acceptance-ONLY downgrade; the shared verdict stays untouched (drift-guard is unaffected).

# imag_power_pl1_guard_reclassify OBSERVED_UW ENABLED GUARD_STATE STEPDOWN_WATTS -> echoes
# `stepdown-ok` iff a pl1 DRIFT is FULLY explained by an active guard thermal step-down (GUARD_STATE
# == stepped AND the live long_term uW == STEPDOWN_WATTS-in-uW AND the constraint is still enabled);
# otherwise `drift` (foreign re-program, a wrong/disabled value, or the guard is NOT stepped / its
# state is unreadable -> never mask a genuine drift). Reuses the shared imag_pl1_watts_to_uw for the
# exact watt->uW comparison. Pure; always returns 0 (called inside a `$(...)` under set -euo pipefail).
imag_power_pl1_guard_reclassify() {
  local observed_uw="$1" enabled="$2" guard_state="$3" stepdown_watts="$4" stepdown_uw
  stepdown_uw="$(imag_pl1_watts_to_uw "$stepdown_watts" 2>/dev/null || true)"
  if [ "$guard_state" = "stepped" ] && [ -n "$stepdown_uw" ] \
    && [ "$observed_uw" = "$stepdown_uw" ] && [ "$enabled" = "1" ]; then
    printf 'stepdown-ok\n'
  else
    printf 'drift\n'
  fi
}

# imag_power_tcpu_guard_verdict TCPU CEIL GUARD_STATE -> echoes one of `unreadable | ok | ok-stepdown
# | over-ceiling`. `unreadable` = TCPU/CEIL empty or non-numeric. `ok` = TCPU below the step-down
# ceiling. `ok-stepdown` = TCPU at/above the ceiling BUT the guard has stepped PL1 down: on the #1162
# unit the box holds at its thermal ceiling even under the 25 W clamp, so this is the EXPECTED steady
# state, not a new clamp episode. `over-ceiling` = at/above the ceiling with the guard NOT stepped
# (or its state unknown) -> a live clamp episode / foreign heat (the existing FAIL). Pure; returns 0.
imag_power_tcpu_guard_verdict() {
  local tcpu="$1" ceil="$2" guard_state="$3"
  case "$tcpu" in '' | *[!0-9-]*) printf 'unreadable\n'; return 0 ;; esac
  case "$ceil" in '' | *[!0-9-]*) printf 'unreadable\n'; return 0 ;; esac
  if [ "$tcpu" -lt "$ceil" ]; then printf 'ok\n'; return 0; fi
  if [ "$guard_state" = "stepped" ]; then printf 'ok-stepdown\n'; else printf 'over-ceiling\n'; fi
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the live SSH/WS flow.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# =================================================================================================
# LIVE flow (executed only when run directly) -- requires sshpass + network access to the box.
# =================================================================================================

usage() {
  cat <<EOF
verify-imag.sh -- POST-PROVISION runtime acceptance gate for the imag notebook (#821).

Usage:
  scripts/verify-imag.sh
  scripts/verify-imag.sh --help

Resolves the active imag box via scripts/imag-host.sh (IMAG_HOST_ACTIVE / IMAG_IP override).
See this script's own header comment for the full list of checks.

Exit: 0 iff every check passes.
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac

command -v sshpass >/dev/null 2>&1 || { echo -e "${RED}ERROR: sshpass is required${NC}" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo -e "${RED}ERROR: python3 is required (imag_scenes.py / obs_phase2.py)${NC}" >&2; exit 1; }

echo -e "${GREEN}== verify-imag (#821): ${IMAG_HOST_NAME:-imag} @ ${IMAG_IP} ==${NC}"

FAILS=0
ok()   { printf "  ${GREEN}[OK]${NC}   %s\n" "$1"; }
fail() { printf "  ${RED}[FAIL]${NC} %s\n" "$1"; FAILS=$((FAILS + 1)); }
warn() { printf "  ${YELLOW}[WARN]${NC} %s\n" "$1"; }
missing_tool() { printf "  ${RED}[FAIL]${NC} MISSING TOOL: %s -- refusing to run a check that cannot execute (#822/#833)\n" "$1"; FAILS=$((FAILS + 1)); }

# ssh_box_timeout SECONDS CMD -- the SOLE raw-ssh primitive, with a HARD execution timeout (#890).
# ssh's own ConnectTimeout bounds only the connect phase, never remote command runtime, so a remote
# command that never returns hangs forever (the check (o) bug, and #1058 for every other read).
# `timeout` bounds the whole ssh invocation; a timed-out call exits 124, which the caller treats as
# a loud FAIL, never a hang. Every read goes through here -- ssh_box below just picks the default
# read budget, so "every remote read is bounded" is an invariant, not per-call diligence.
ssh_box_timeout() {
  timeout "$1" sshpass -p "$IMAG_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    "${IMAG_USER}@${IMAG_IP}" "$2"
}

# ssh_box CMD -- a bounded remote read at the general read budget (#1058). Delegates to the single
# ssh_box_timeout primitive so it can never be an UNbounded raw ssh; a slow-class read overrides the
# budget by calling ssh_box_timeout with the IMAG_SLOW_READ_TIMEOUT budget directly (dpkg/apt/journal/
# gather sites below).
ssh_box() {
  ssh_box_timeout "$IMAG_READ_TIMEOUT" "$1"
}

# (a) hostname + static IP -------------------------------------------------------------------
rc=0
HOSTNAME_ACTUAL="$(ssh_box "hostname 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$HOSTNAME_ACTUAL" ]; then
  fail "hostname unreadable (ssh rc=$rc)"
elif imag_hostname_matches "$HOSTNAME_ACTUAL" "$IMAG_HOSTNAME_EXPECT"; then
  ok "hostname = $HOSTNAME_ACTUAL"
else
  fail "hostname '$HOSTNAME_ACTUAL' != expected '$IMAG_HOSTNAME_EXPECT'"
fi

rc=0
IP_TEXT="$(ssh_box "hostname -I 2>/dev/null; ip -4 -o addr show 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$IP_TEXT" ]; then
  fail "IP address state unreadable (ssh rc=$rc)"
elif imag_static_ip_present "$IP_TEXT" "$IMAG_IP"; then
  ok "static IP $IMAG_IP present on the box"
else
  fail "box does not report $IMAG_IP among its own addresses (ssh rc=$rc): $IP_TEXT"
fi

# (b) ssh.service (not ssh.socket) -------------------------------------------------------------
rc=0
SSHD_SVC="$(ssh_box "systemctl is-enabled ssh.service 2>/dev/null || true")" || rc=$?
SSHD_SOCK="$(ssh_box "systemctl is-enabled ssh.socket 2>/dev/null || true")" || true
if [ "$rc" -ne 0 ]; then
  fail "ssh.service state unreadable (ssh rc=$rc)"
elif imag_sshd_via_service "$SSHD_SVC" "$SSHD_SOCK"; then
  ok "ssh.service enabled (ssh.socket not enabled)"
else
  fail "ssh.service='${SSHD_SVC:-<none>}' ssh.socket='${SSHD_SOCK:-<none>}' -- expected ssh.service enabled, ssh.socket not"
fi

# (c) kernel on the HWE line (#819) -------------------------------------------------------------
rc=0
HWE_STATUS="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "dpkg -s linux-image-generic-hwe-24.04 2>/dev/null | sed -n 's/^Status: //p' || true")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "HWE kernel dpkg status unreadable (ssh rc=$rc)"
elif imag_hwe_kernel_installed "$HWE_STATUS"; then
  ok "kernel on the HWE line (linux-image-generic-hwe-24.04 installed)"
else
  fail "linux-image-generic-hwe-24.04 not installed (status='${HWE_STATUS:-<none>}') -- #819 GA-baseline regression"
fi

# (d) kernel cmdline: preempt=full + NO kernel isolcpus/nohz_full isolation (#289/#482/#784/#842)
rc=0
CMDLINE="$(ssh_box "cat /proc/cmdline 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$CMDLINE" ]; then
  fail "/proc/cmdline unreadable (ssh rc=$rc)"
else
  if imag_cmdline_has_preempt_full "$CMDLINE"; then
    ok "kernel cmdline carries preempt=full (#482)"
  else
    fail "kernel cmdline missing preempt=full (#482): '$CMDLINE'"
  fi
  # #784/#842: hard FAIL if the box still carries kernel-level isolcpus=/nohz_full= -- this is
  # #784's own outstanding acceptance-gate item (deferred between #780/#791 since 2026-07-15) and
  # the guard that would have caught the #842 recurrence on the replacement notebook at
  # provisioning time instead of live on the rig.
  if imag_cmdline_free_of_kernel_isolation "$CMDLINE"; then
    ok "kernel cmdline carries NO isolcpus/nohz_full kernel isolation (#784/#842 -- affinity-only OBS core reservation)"
  else
    fail "kernel cmdline STILL carries isolcpus=/nohz_full= (#784/#842 regression -- disables scheduler load balancing, piles OBS's threads onto ONE core): '$CMDLINE'"
  fi
  # The taskset AFFINITY pin (unaffected by #842 -- only kernel-level isolation was removed) must
  # still be correctly persisted. Derive the EXPECTED isolated-CPU set from this box's own live
  # topology -- the identical computation setup-imag.sh step 8 runs, never a hardcoded literal
  # (#816: a different core count, or the other known imag box, derives a genuinely different set).
  rc=0
  TOPO="$(ssh_box "for f in /sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list; do [ -r \"\$f\" ] || continue; c=\"\${f#/sys/devices/system/cpu/cpu}\"; c=\"\${c%%/*}\"; printf '%s %s\n' \"\$c\" \"\$(cat \"\$f\")\"; done | sort -n -k1,1")" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$TOPO" ]; then
    fail "CPU topology unreadable over SSH (rc=$rc) -- cannot derive the expected affinity plan"
  else
    PLAN=""
    PLAN="$(printf '%s\n' "$TOPO" | imag_cpu_isolation_plan)" || true
    ISOLATED="$(printf '%s\n' "$PLAN" | sed -n 1p)"
    if [ -z "$ISOLATED" ]; then
      fail "could not derive the CPU affinity plan from this box's own topology (#816)"
    else
      rc=0
      PERSISTED="$(ssh_box "cat /etc/imag-isolated-cpus.conf 2>/dev/null" | tr -d '[:space:]')" || rc=$?
      if [ "$rc" -ne 0 ] || [ "$PERSISTED" != "$(printf '%s' "$ISOLATED" | tr -d '[:space:]')" ]; then
        fail "/etc/imag-isolated-cpus.conf ('${PERSISTED:-<unreadable>}') does not match the derived affinity set (${ISOLATED}) (#816/#842)"
      else
        ok "OBS core affinity persisted correctly: ${ISOLATED} (taskset-only, no kernel isolation, #842)"
      fi
    fi
  fi
fi

# (s) OBS threads not concentrated onto a single CPU core -- the #842 DIRECT SYMPTOM ------------
rc=0
OBS_PSR_LIST="$(ssh_box "ps -L -o psr= -C obs 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$OBS_PSR_LIST" ]; then
  fail "OBS thread/CPU list unreadable (ssh rc=$rc) -- cannot verify thread distribution (#842)"
elif imag_obs_thread_concentration_ok "$OBS_PSR_LIST"; then
  ok "OBS threads spread across multiple CPU cores -- no single-core pileup (#842)"
else
  fail "OBS threads are concentrated onto a single CPU core -- the #842/#784 isolcpus signature (a future variant of this bug must not pass silently): '$OBS_PSR_LIST'"
fi

# (e) display-manager -> lightdm + autologin; gdm3 absent ----------------------------------------
rc=0
DM_LINK="$(ssh_box "readlink -f /etc/systemd/system/display-manager.service 2>/dev/null")" || rc=$?
LIGHTDM_UNIT="$(ssh_box "readlink -f /lib/systemd/system/lightdm.service 2>/dev/null" || true)"
if [ "$rc" -ne 0 ] || [ -z "$DM_LINK" ] || [ -z "$LIGHTDM_UNIT" ]; then
  fail "display-manager symlink unreadable (ssh rc=$rc)"
elif [ "$DM_LINK" = "$LIGHTDM_UNIT" ]; then
  ok "display-manager.service -> lightdm.service"
else
  fail "display-manager.service does NOT resolve to lightdm.service (got '$DM_LINK', want '$LIGHTDM_UNIT')"
fi

rc=0
AUTOLOGIN_CONF="$(ssh_box "cat /etc/lightdm/lightdm.conf.d/50-imag-autologin.conf 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$AUTOLOGIN_CONF" ]; then
  fail "50-imag-autologin.conf unreadable/missing (ssh rc=$rc)"
elif imag_autologin_conf_ok "$AUTOLOGIN_CONF" "$IMAG_USER"; then
  ok "lightdm autologin configured for ${IMAG_USER} (openbox session)"
else
  fail "50-imag-autologin.conf present but missing autologin-user=${IMAG_USER} / autologin-session=openbox"
fi

rc=0
GDM3_STATUS="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "dpkg -s gdm3 2>/dev/null | sed -n 's/^Status: //p' || true")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "gdm3 dpkg status unreadable (ssh rc=$rc)"
elif imag_pkg_absent "$GDM3_STATUS"; then
  ok "gdm3 purged"
else
  fail "gdm3 still present (status='${GDM3_STATUS}') -- must be purged (#504)"
fi

# (f) zero failed systemd units -------------------------------------------------------------
rc=0
FAILED_UNITS="$(ssh_box "systemctl list-units --failed --no-legend 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "failed-units state unreadable (ssh rc=$rc)"
elif imag_failed_units_ok "$FAILED_UNITS"; then
  ok "zero failed systemd units"
else
  fail "failed systemd units present: $(printf '%s' "$FAILED_UNITS" | tr '\n' ' ')"
fi

# (g) openbox autostart + openbox/obs running as the desktop user -----------------------------
rc=0
AUTOSTART="$(ssh_box "cat /home/${IMAG_USER}/.config/openbox/autostart 2>/dev/null")" || rc=$?
AUTOSTART_MODE="$(ssh_box "ls -la /home/${IMAG_USER}/.config/openbox/autostart 2>/dev/null | awk '{print \$1}'" || true)"
if [ "$rc" -ne 0 ] || [ -z "$AUTOSTART" ]; then
  fail "openbox autostart unreadable/missing (ssh rc=$rc)"
else
  if imag_regular_executable_file "${AUTOSTART_MODE:-}"; then
    ok "openbox autostart present + executable"
  else
    fail "openbox autostart present but not executable (mode='${AUTOSTART_MODE:-<none>}')"
  fi
  if imag_autostart_placeholders_resolved "$AUTOSTART"; then
    ok "openbox autostart has no unsubstituted __PLACEHOLDER__ left"
  else
    fail "openbox autostart still contains an unsubstituted __PLACEHOLDER__ (setup-imag.sh sed step silently no-op'd)"
  fi
fi

rc=0
PS_TEXT="$(ssh_box "ps -u ${IMAG_USER} -o comm= 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$PS_TEXT" ]; then
  fail "process list for ${IMAG_USER} unreadable (ssh rc=$rc)"
else
  if imag_proc_running "$PS_TEXT" "openbox"; then
    ok "openbox running as ${IMAG_USER}"
  else
    fail "openbox NOT running as ${IMAG_USER}"
  fi
  if imag_proc_running "$PS_TEXT" "obs"; then
    ok "obs running as ${IMAG_USER}"
  else
    fail "obs NOT running as ${IMAG_USER}"
  fi
fi

# (h)+(i) OBS log: genlock tick, no version-mismatch, DistroAV+NDI loaded; :4455 listening -----
rc=0
LATEST_LOG_PATH="$(ssh_box "ls -t /home/${IMAG_USER}/.config/obs-studio/logs/*.txt 2>/dev/null | head -1" || true)"
OBS_LOG=""
if [ -n "$LATEST_LOG_PATH" ]; then
  OBS_LOG="$(ssh_box "cat '${LATEST_LOG_PATH}' 2>/dev/null")" || rc=$?
fi
if [ -z "$LATEST_LOG_PATH" ] || [ -z "$OBS_LOG" ]; then
  fail "no OBS log found on the box (ssh rc=$rc) -- cannot verify the genlock build/module health"
else
  if imag_obs_log_shows_genlock_tick "$OBS_LOG"; then
    ok "OBS log shows genlock render tick ENABLED"
  else
    fail "OBS log shows NO genlock capability marker in '${LATEST_LOG_PATH}' -- not the genlock build"
  fi
  if imag_obs_log_no_version_mismatch "$OBS_LOG"; then
    ok "OBS log has no libobs-version-mismatch warning (#824)"
  else
    fail "OBS log shows 'compiled with newer libobs' -- #824 regression: stock plugins (incl. obs-websocket) refused"
  fi
  if imag_obs_log_shows_distroav_loaded "$OBS_LOG"; then
    ok "DistroAV plugin loaded"
  else
    fail "OBS log shows no '[distroav] plugin loaded' line"
  fi
  if imag_obs_log_shows_ndi_loaded "$OBS_LOG"; then
    ok "NDI runtime loaded"
  else
    fail "OBS log shows no 'NDI library initialized' line"
  fi
fi

rc=0
WS_UP="$(ssh_box "for i in \$(seq 1 5); do (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null && { exec 3>&-; echo up; break; }; sleep 1; done")" || rc=$?
if [ "$rc" -eq 0 ] && [ "$(printf '%s' "$WS_UP" | tr -d '[:space:]')" = "up" ]; then
  ok "OBS WebSocket :4455 listening (obs-websocket.so loaded -- the #824 module-count proof)"
else
  fail "OBS WebSocket :4455 not listening (ssh rc=$rc) -- obs-websocket.so likely not loaded"
fi

# (j) OBS base package version matches the pinned genlock build AND is apt-mark held (#824) -----
rc=0
OBS_PKG_VERSION="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "dpkg-query -W -f='\${Version}' obs-studio 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$OBS_PKG_VERSION" ]; then
  fail "obs-studio package version unreadable (ssh rc=$rc)"
elif imag_obs_base_version_matches "$OBS_PKG_VERSION" "$IMAG_OBS_BASE_VERSION"; then
  ok "obs-studio package version = ${OBS_PKG_VERSION} (matches the pinned genlock build)"
else
  fail "obs-studio package version '${OBS_PKG_VERSION}' != pinned '${IMAG_OBS_BASE_VERSION}' (#824)"
fi

rc=0
HOLD_LIST="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "apt-mark showhold 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "apt-mark showhold unreadable (ssh rc=$rc)"
elif imag_pkg_is_held "$HOLD_LIST" "obs-studio"; then
  ok "obs-studio apt-mark held (protects the #824 pin from an unattended upgrade)"
else
  fail "obs-studio is NOT apt-mark held -- an apt upgrade could silently break the genlock plugin ABI again (#824)"
fi

# (k) NDI runtime pinned: libndi.so.6 -> libndi.so.6.3.2 (#132/#547) -----------------------------
rc=0
NDI_LS="$(ssh_box "ls -la /usr/lib/ndi 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$NDI_LS" ]; then
  fail "NDI dir unreadable (ssh rc=$rc)"
else
  if ndi_symlink_chain_ok "$NDI_LS"; then
    ok "libndi.so.6 is a root-owned symlink chain to a root-owned regular file"
  else
    fail "libndi.so.6 is not a root-owned symlink chain -- see \`ls -la /usr/lib/ndi\`"
  fi
  NDI_VER="$(ndi_symlink_version "$NDI_LS")"
  if ndi_version_matches "$NDI_VER" "6.3.2"; then
    ok "NDI runtime pinned to 6.3.2 (active: ${NDI_VER})"
  else
    fail "NDI runtime '${NDI_VER:-?}' != fleet pin '6.3.2'"
  fi
fi

# (k2) NVIDIA dGPU: driver + prime-select when present, correctly skipped when absent (#816) -----
rc=0
LSPCI_OUT="$(ssh_box "lspci -nn 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$LSPCI_OUT" ]; then
  fail "lspci output unreadable (ssh rc=$rc) -- cannot determine whether a discrete NVIDIA GPU is present"
else
  HAS_DGPU=no
  if printf '%s\n' "$LSPCI_OUT" | imag_has_discrete_nvidia; then HAS_DGPU=yes; fi
  if [ "$HAS_DGPU" = "yes" ]; then
    DRIVER_STATUS="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "dpkg -s nvidia-driver-595-open 2>/dev/null | sed -n 's/^Status: //p' || true")" || true
    PRIME_OUT="$(ssh_box "prime-select query 2>/dev/null" || true)"
  else
    DRIVER_STATUS=""
    PRIME_OUT=""
  fi
  case "$(imag_nvidia_verdict "$HAS_DGPU" "$DRIVER_STATUS" "$PRIME_OUT")" in
    na) ok "no discrete NVIDIA GPU on this box -- driver/PRIME step correctly skipped (#816)" ;;
    ok) ok "nvidia-driver-595-open installed, prime-select nvidia (dGPU present)" ;;
    fail) fail "discrete NVIDIA GPU present but driver not installed / PRIME not set to nvidia (status='${DRIVER_STATUS:-<none>}', prime='${PRIME_OUT:-<none>}')" ;;
  esac
fi

# (l) dantesync PTP LOCKED + FRESH offset + SAME grandmaster (#8/#550/#591/#834) -----------------
rc=0
DS_JOURNAL="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null")" || rc=$?
DS_HTTP_STATUS="$(ssh_box "curl -fsS --max-time 8 http://127.0.0.1:8898/status 2>/dev/null" || true)"
if [ -n "$DS_HTTP_STATUS" ]; then
  # #686 precedent: the network status endpoint is authoritative and immune to journal-cadence
  # throttling -- prefer it, journal is the fallback only.
  ptp_state="$(ptp_locked_from_pipe_json "$DS_HTTP_STATUS")"
  offset_us="$(offset_us_from_pipe_json "$DS_HTTP_STATUS")"
  gm_actual="$(gm_source_ip_from_pipe_json "$DS_HTTP_STATUS")"
  rc_ptp=0; ptp_check imag "$ptp_state" || rc_ptp=$?
  rc_off=0; offset_check imag "$offset_us" "$IMAG_CLOCK_BOUND_US" || rc_off=$?
  rc_gm=0; gm_check imag "$gm_actual" "$RIG_GRANDMASTER_IP" || rc_gm=$?
  [ "$rc_ptp" -eq 0 ] && ok "dantesync PTP servo LOCKED (via :8898/status)" || fail "dantesync PTP servo not LOCKED (via :8898/status)"
  [ "$rc_off" -eq 0 ] && ok "dantesync clock offset within ${IMAG_CLOCK_BOUND_US}us bound" || fail "dantesync clock offset OUTSIDE bound or unreadable (rc=$rc_off)"
  [ "$rc_gm" -eq 0 ] && ok "dantesync grandmaster = ${gm_actual} (matches the rig, #834)" || fail "dantesync grandmaster mismatch/unreadable (rc=$rc_gm, want ${RIG_GRANDMASTER_IP})"
elif [ "$rc" -ne 0 ] || [ -z "$DS_JOURNAL" ]; then
  fail "dantesync journal unreadable (ssh rc=$rc) and :8898/status unreachable"
else
  DS_ACTIVE="$(ssh_box "systemctl is-active dantesync 2>/dev/null")" || true
  ds_now_rc=0
  BOX_NOW="$(ssh_box "date +%s 2>/dev/null")" || ds_now_rc=$?
  if ! dantesync_service_active "$DS_ACTIVE"; then
    fail "dantesync service NOT active (state='${DS_ACTIVE:-<none>}')"
  elif [ "$ds_now_rc" -ne 0 ] || [ -z "$BOX_NOW" ]; then
    fail "could not read the box wall clock over SSH (rc=$ds_now_rc)"
  elif [ "$(dantesync_journal_fresh "$DS_JOURNAL" "$BOX_NOW" "$DANTESYNC_JOURNAL_MAX_AGE_S")" = stale ]; then
    fail "dantesync journal has not advanced within ${DANTESYNC_JOURNAL_MAX_AGE_S}s of the box clock -- daemon hung"
  else
    if dantesync_locked_ok "$DS_JOURNAL"; then
      ok "dantesync PTP servo LOCKED"
    else
      fail "dantesync PTP servo not LOCKED"
    fi
    case "$(dantesync_offset_verdict "$DS_JOURNAL" "$DANTESYNC_OFFSET_FRESHNESS_S" "$IMAG_CLOCK_BOUND_US" "$IMAG_CLOCK_STABILITY_US")" in
      ok) ok "dantesync clock offset within ${IMAG_CLOCK_BOUND_US}us bound + samples within ${IMAG_CLOCK_STABILITY_US}us spread (fresh)" ;;
      drift) fail "dantesync clock offset OUTSIDE the ${IMAG_CLOCK_BOUND_US}us bound -- a real clock desync" ;;
      unstable) fail "dantesync clock offset median within the ${IMAG_CLOCK_BOUND_US}us bound but the FRESH samples scatter past the ${IMAG_CLOCK_STABILITY_US}us stability bound -- scattered/unusable clock (#837)" ;;
      drift_unstable) fail "dantesync clock offset OUTSIDE the ${IMAG_CLOCK_BOUND_US}us bound AND samples scatter past the ${IMAG_CLOCK_STABILITY_US}us stability bound (#837)" ;;
      *) fail "dantesync clock offset has no FRESH reading -- status incomplete" ;;
    esac
    fail "grandmaster identity unreadable via the journal path (no gm_source_ip in journald text) -- the :8898/status endpoint is required to certify #834; it was unreachable above"
  fi
fi

# (m) dantesync is the SOLE timesync authority (#591) --------------------------------------------
rc=0
TS_STATES="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "$(timesync_gather_remote_snippet)")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$TS_STATES" ]; then
  fail "could not read timesync-daemon state over SSH (rc=$rc)"
else
  TS_VERDICT="$(timesync_authority_verdict "$TS_STATES")"
  if [ "$TS_VERDICT" = "ok" ]; then
    ok "dantesync is the SOLE timesync authority (#591)"
  else
    while IFS= read -r _reason; do
      [ -n "$_reason" ] && fail "timesync authority: ${_reason#FAIL: }"
    done <<< "$TS_VERDICT"
  fi
fi

# (n) scenes present + Multiview populated (imag_scenes.py, bare) --------------------------------
# #791: the expected count is IMAG_SCENE_CAM_COUNT's own default (7) -- overridable the same way
# imag_scenes.py itself is, never re-hardcoded independently here.
IMAG_SCENE_CAM_COUNT="${IMAG_SCENE_CAM_COUNT:-7}"
rc=0
SCENES_OUT="$(IMAG_SCENE_CAM_COUNT="$IMAG_SCENE_CAM_COUNT" python3 "$HERE/imag_scenes.py" --host "$IMAG_IP" 2>&1)" || rc=$?
if imag_scenes_output_ok "$SCENES_OUT" "$IMAG_SCENE_CAM_COUNT"; then
  ok "scenes: Cam 1-${IMAG_SCENE_CAM_COUNT} + MV Cam 1-${IMAG_SCENE_CAM_COUNT} all present"
else
  fail "scenes/Multiview membership incomplete (rc=$rc): $(printf '%s' "$SCENES_OUT" | tr '\n' ' ')"
fi

# (q) OPERATOR parity: full canonical scene ORDER + NDI-source bindings (#791) -------------------
rc=0
PARITY_OUT="$(IMAG_SCENE_CAM_COUNT="$IMAG_SCENE_CAM_COUNT" python3 "$HERE/imag_scenes.py" --host "$IMAG_IP" --verify-parity 2>&1)" || rc=$?
if imag_parity_output_ok "$PARITY_OUT"; then
  ok "operator parity: canonical scene order + NDI-source bindings all match (#791)"
else
  fail "operator parity mismatch (rc=$rc): $(printf '%s' "$PARITY_OUT" | tr '\n' ' ')"
fi

# (t) imag-obs.service supervision: installed+enabled+active, Restart=on-failure, autostart wired
# through the unit (not a direct script call), core dumps ACTUALLY enabled (#884, follow-up to
# #882). MUST run BEFORE check (o) below -- (o)'s own restart-proof calls
# imag-obs-stop.sh/imag-obs-start.sh DIRECTLY over SSH (bypassing systemctl), which leaves this
# unit `inactive (dead)` and starts a fresh, UNTRACKED obs process with NO LimitCORE applied
# (live-confirmed on 10.77.9.182: the post-restart process showed Max core file size = 0, not
# unlimited) -- reading these checks after (o) would falsely FAIL a healthy, correctly-provisioned
# box every time this gate runs. ------------------------------------------------------------------
rc=0
OBS_SVC_ENABLED="$(ssh_box "systemctl --user is-enabled imag-obs.service 2>/dev/null || true")" || rc=$?
OBS_SVC_ACTIVE="$(ssh_box "systemctl --user is-active imag-obs.service 2>/dev/null || true")" || true
if [ "$rc" -ne 0 ] || [ -z "$OBS_SVC_ENABLED" ]; then
  fail "imag-obs.service enabled/active state unreadable (ssh rc=$rc)"
elif imag_obs_service_state_ok "$OBS_SVC_ENABLED" "$OBS_SVC_ACTIVE"; then
  ok "imag-obs.service enabled + active -- the openbox autostart's boot launch is systemd-supervised (#884)"
else
  fail "imag-obs.service NOT enabled+active (enabled='${OBS_SVC_ENABLED}', active='${OBS_SVC_ACTIVE}') -- a re-provision without this leaves OBS unsupervised, the state behind the 2026-07-30 outage (#882/#884)"
fi

rc=0
OBS_SVC_RESTART="$(ssh_box "systemctl --user show imag-obs.service --property=Restart 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$OBS_SVC_RESTART" ]; then
  fail "imag-obs.service Restart= property unreadable (ssh rc=$rc)"
elif imag_obs_service_restart_is_on_failure "$OBS_SVC_RESTART"; then
  ok "imag-obs.service Restart=on-failure (never 'always' -- issue 788's operator-fighting bug)"
else
  fail "imag-obs.service Restart is NOT exactly 'on-failure' (got '${OBS_SVC_RESTART}') -- 'always' fights a deliberate operator quit (issue 788)"
fi

if imag_autostart_launches_via_service_not_script "${AUTOSTART:-}"; then
  ok "openbox autostart launches OBS via imag-obs.service, not a direct script call (#884)"
else
  fail "openbox autostart does NOT launch OBS via imag-obs.service -- a re-provision would silently strip supervision, regressing to the 2026-07-30 outage (#884)"
fi

rc=0
CORE_PATTERN="$(ssh_box "cat /proc/sys/kernel/core_pattern 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$CORE_PATTERN" ]; then
  fail "kernel.core_pattern unreadable (ssh rc=$rc)"
elif imag_core_pattern_captures_dumps "$CORE_PATTERN"; then
  ok "kernel.core_pattern is a piped collector ('${CORE_PATTERN}') -- crashes are captured (#882)"
else
  fail "kernel.core_pattern is NOT a piped collector ('${CORE_PATTERN}') -- a future segfault could silently drop its core even with an unlimited ulimit (#882)"
fi

rc=0
OBS_PID="$(ssh_box "pgrep -x obs | head -1" || true)"
OBS_CORE_LIMIT=""
if [ -n "$OBS_PID" ]; then
  OBS_CORE_LIMIT="$(ssh_box "grep -i 'Max core file size' /proc/${OBS_PID}/limits 2>/dev/null")" || rc=$?
fi
if [ -z "$OBS_PID" ] || [ -z "$OBS_CORE_LIMIT" ]; then
  fail "could not read the live obs process's core-dump limit (pid='${OBS_PID:-<none>}', ssh rc=$rc)"
elif imag_obs_core_dumps_enabled "$OBS_CORE_LIMIT"; then
  ok "obs process core dumps enabled (Max core file size = unlimited, #882)"
else
  fail "obs process core dumps NOT enabled ('${OBS_CORE_LIMIT}') -- a future segfault would again leave nothing debuggable (#882)"
fi

# claim-vs-reality: the LIVE obs PID must genuinely run INSIDE imag-obs.service's cgroup, not just
# systemd's is-enabled/is-active bookkeeping above (#1015). Reuses the SAME OBS_PID already
# resolved for the core-dump check -- MUST also run before check (o)'s restart below.
rc=0
OBS_CGROUP=""
if [ -n "$OBS_PID" ]; then
  OBS_CGROUP="$(ssh_box "cat /proc/${OBS_PID}/cgroup 2>/dev/null")" || rc=$?
fi
if [ -z "$OBS_PID" ] || [ -z "$OBS_CGROUP" ]; then
  fail "could not read the live obs process's cgroup (pid='${OBS_PID:-<none>}', ssh rc=$rc) -- cannot verify it runs inside imag-obs.service (#1015)"
elif imag_obs_cgroup_shows_service_unit "$OBS_CGROUP"; then
  ok "live obs process (pid ${OBS_PID}) runs INSIDE imag-obs.service's cgroup -- genuinely supervised, not a bypass launch (#1015)"
else
  fail "live obs process (pid ${OBS_PID}) is OUTSIDE imag-obs.service's cgroup ($(printf '%s' "$OBS_CGROUP" | tr '\n' ' ')) -- systemd may report the unit enabled+active while the RUNNING obs was launched directly (e.g. via imag-obs-start.sh over ssh, bypassing systemctl) -- Restart=on-failure supervises NOTHING (#1015, the #840 claim-vs-reality class)"
fi

# (u) power/thermal envelope (#1040) -- MUST run BEFORE check (o) below (its restart replaces the
# tracked obs process, #884 ordering). Gathers the envelope state over SSH via the SHARED
# imag_power_envelope_gather_remote_snippet and runs the SHARED imag_power_envelope_verdict --
# IDENTICAL to drift-guard.sh's --check-imag facet, never a driftable copy. On this gate a DRIFT
# OR an UNKNOWN facet both FAIL (unreadable = hard FAIL, never silent) -- a healthy imag-nb always
# gathers every facet (it has the RAPL zone, the iGPU slpc knob, and both enabled+active units).
rc=0
PE_GATHER="$(ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "$(imag_power_envelope_gather_remote_snippet)")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$PE_GATHER" ]; then
  fail "could not read the power/thermal-envelope state over SSH (rc=$rc) -- cannot verify PL1/slpc/thermald/units (#1040)"
else
  # Read the SAME README pin drift-guard.sh's --check-imag facet reads (its pinned_setting), so the
  # acceptance gate and the strict gate never check DIFFERENT wattages after a deliberate re-pin.
  # Fall back to the lib/env default only when the README is unreadable.
  PE_PIN="$(imag_power_pl1_pin_from_readme_text "$(cat "$HERE/../vendor/README.md" 2>/dev/null || true)")"
  [ -n "$PE_PIN" ] || PE_PIN="${IMAG_PL1_W:-45}"

  # #1188: consult the guard's OWN /run state so a LEGITIMATE thermal step-down (the guard clamped
  # PL1 to the 25 W step-down on a >=93 C excursion -- on the #1162 unit the normal steady state) is
  # not read as a FOREIGN PL1 re-program. Read the state file over the same SSH (the guard writes it
  # world-readable, #1188); an unreadable/absent file -> `unknown` -> we keep the strict DRIFT/FAIL
  # (never mask). The SHARED imag_power_envelope_verdict stays guard-BLIND, so drift-guard's strict
  # preflight is unaffected -- the downgrade below is acceptance-gate-only.
  PE_GUARD_RAW="$(ssh_box_timeout "$IMAG_READ_TIMEOUT" "cat ${IMAG_POWER_GUARD_STATE:-$IMAG_POWER_GUARD_STATE_FILE} 2>/dev/null")" || true
  PE_GUARD_STATE="$(imag_power_guard_stepped_from_state "$PE_GUARD_RAW")"
  PE_PL1_UW="$(imag_power_zone_select "$PE_GATHER" || true)"
  PE_PL1_EN="$(imag_power_pl1_enabled "$PE_GATHER" || true)"
  # Prefer the step-down watts the guard ITSELF recorded in its state (its own authority), so a
  # provisioning-time IMAG_PL1_STEPDOWN_W override baked into the guard unit can never diverge from
  # verify's independent env default (#1188). Fall back to the env/lib default only for an older
  # guard that did not record it.
  PE_GUARD_STEPDOWN_W="$(imag_power_guard_stepdown_w_from_state "$PE_GUARD_RAW")"
  PE_STEPDOWN_W="${PE_GUARD_STEPDOWN_W:-${IMAG_PL1_STEPDOWN_W:-25}}"

  while IFS='|' read -r pe_facet pe_status pe_detail; do
    [ -n "$pe_facet" ] || continue
    if [ "$pe_status" = "OK" ]; then
      ok "power envelope (${pe_facet}): ${pe_detail}"
    elif [ "$pe_facet" = "pl1" ] && [ "$pe_status" = "DRIFT" ] \
      && [ "$(imag_power_pl1_guard_reclassify "$PE_PL1_UW" "$PE_PL1_EN" "$PE_GUARD_STATE" "$PE_STEPDOWN_W")" = "stepdown-ok" ]; then
      ok "power envelope (pl1): long_term=${PE_PL1_UW}uW at the ${PE_STEPDOWN_W}W step-down, guard thermal step-down active -- a legitimate #1040 clamp (guard state STEPPED), NOT foreign drift (#1188)"
    else
      fail "power envelope (${pe_facet}) ${pe_status}: ${pe_detail}"
    fi
  done <<< "$(imag_power_envelope_verdict "$PE_GATHER" "$PE_PIN")"

  # TCPU must be BELOW the guard's step-down ceiling -- a reading at/above it means a clamp episode
  # is live right now (the envelope is thermally degraded, not merely mis-provisioned) -- UNLESS the
  # guard has already stepped PL1 down (#1188): on the #1162 unit the box holds at its thermal
  # ceiling even under the 25 W clamp, so a stepped-down at/above-ceiling reading is the EXPECTED
  # steady state, not a new clamp episode.
  PE_TCPU="$(printf '%s\n' "$PE_GATHER" | sed -n 's/^TCPU|//p' | head -1)"
  PE_CEIL="${IMAG_TCPU_STEPDOWN_C:-93}"
  case "$(imag_power_tcpu_guard_verdict "$PE_TCPU" "$PE_CEIL" "$PE_GUARD_STATE")" in
    unreadable)
      fail "TCPU (x86_pkg_temp) unreadable in the envelope gather -- cannot confirm the box is below the ${PE_CEIL}C step-down ceiling (#1040)"
      ;;
    ok)
      ok "TCPU=${PE_TCPU}C is below the ${PE_CEIL}C guard step-down ceiling (#1040)"
      ;;
    ok-stepdown)
      ok "TCPU=${PE_TCPU}C is at/above the ${PE_CEIL}C step-down ceiling BUT the guard has stepped PL1 down (thermal step-down active) -- the expected steady state on this unit under the ${PE_STEPDOWN_W}W clamp (#1162/#1188), not a new clamp episode"
      ;;
    *)
      fail "TCPU=${PE_TCPU}C is AT/ABOVE the ${PE_CEIL}C step-down ceiling -- a thermal clamp episode is live (#1040)"
      ;;
  esac

  # The guard's journald tag must be readable -- proves its step-down/re-assert transitions are
  # retrievable for the dev1-side alert watchdog (the never-silent-degradation rule).
  rc=0
  ssh_box_timeout "$IMAG_SLOW_READ_TIMEOUT" "journalctl -t imag-power-envelope --no-pager -n 1 >/dev/null 2>&1; true" >/dev/null 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    fail "could not read the imag-power-envelope journald tag over SSH (rc=$rc) -- the guard's transitions would be invisible to the dev1-side alert watchdog (#1040)"
  else
    ok "imag-power-envelope journald tag is readable (guard transitions retrievable for dev1-side alerting, #1040)"
  fi
fi

# (w) touchpad usability config present + correct (#779) -- a pure static file read, side-effect
# free, kept ABOVE check (o)'s OBS restart for #884-ordering hygiene. Reads the durable
# 30-touchpad-tap.conf back over SSH and fails loud if a reprovision dropped it or wrote a
# partial/wrong file (the issue-840 "verify the file the provisioner writes" pairing).
rc=0
TOUCHPAD_CONF="$(ssh_box "cat /etc/X11/xorg.conf.d/30-touchpad-tap.conf 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$TOUCHPAD_CONF" ]; then
  fail "/etc/X11/xorg.conf.d/30-touchpad-tap.conf missing/unreadable over SSH (rc=$rc) -- the touchpad usability config (#779) is absent; a reprovision dropped tap-to-click/natural-scroll (setup-imag.sh step 25)"
elif imag_touchpad_conf_ok "$TOUCHPAD_CONF"; then
  ok "touchpad usability config present + correct (tap-to-click + natural scroll + ScrollPixelDistance 50, #779)"
else
  fail "30-touchpad-tap.conf present but INCOMPLETE/WRONG (#779) -- must carry the selector (MatchIsTouchpad \"on\" + Driver \"libinput\") AND Tapping/TappingDrag/NaturalScrolling \"on\" + ScrollPixelDistance \"50\"; a reprovision regenerated a partial/wrong file"
fi

# (x) Wake-on-LAN armed on the NDI NIC (#1103) -- a post-event powered-down/slept imag-nb must be
# remotely wakeable via a magic packet from dev1 (scripts/wake-box.sh imag-nb). The DURABLE signal is
# the persisted NM setting (802-3-ethernet.wake-on-lan=magic): NM re-applies it on every
# connection-up, so it survives reboot, and it is readable SUDO-LESSLY (unlike the runtime ethtool
# Wake-on line, which is root-only). A pure static read, side-effect free, kept ABOVE check (o)'s OBS
# restart for #884-ordering hygiene. Resolves the rig NIC by the box's OWN static rig IP ($IMAG_IP,
# the address this gate is already SSHed in over) -- unambiguous even on a multi-homed notebook where
# a Wi-Fi default route could otherwise point at a DIFFERENT connection than setup-imag.sh armed --
# then reads WoL off that NIC's active NM connection.
rc=0
WOL_VALUE="$(ssh_box "NIC=\$(ip -o -4 addr show | awk -v ip=\"$IMAG_IP\" '{split(\$4,a,\"/\"); if(a[1]==ip){print \$2; exit}}'); CON=\$(nmcli -t -f NAME,DEVICE con show --active | awk -F: -v d=\"\$NIC\" '\$2==d{print \$1; exit}'); nmcli -g 802-3-ethernet.wake-on-lan connection show \"\$CON\" 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "Wake-on-LAN NM setting unreadable over SSH (rc=$rc) -- cannot confirm imag-nb is remotely wakeable (#1103)"
elif imag_wol_enabled_ok "$WOL_VALUE"; then
  ok "Wake-on-LAN armed (NM 802-3-ethernet.wake-on-lan=magic on the NDI NIC, #1103)"
else
  fail "Wake-on-LAN NOT armed (NM value='${WOL_VALUE}', want 'magic') -- a powered-down imag-nb would not be remotely wakeable; setup-imag.sh step 1 arms it (#1103)"
fi

# (y) full max-performance persistence (#756/#791) ---------------------------------------------
# The imag-maxperf trio (imag-maxperf.service + /usr/local/sbin/imag-maxperf.sh + the hotplug udev
# rule) MUST be provisioned AND the runtime STATE must read performance -- governor + EPP +
# intel_pstate no_turbo=0 + platform_profile. This closes the exact hand-placed-never-provisioned
# gap #791 exists for (the trio lived only on the live box, never in the generator, so a fresh box
# silently lost EPP/turbo/PCI-PM persistence -- the 2026-07-18 audit demand). setup-imag.sh step 26
# provisions it. Reads artifact presence + the live sysfs STATE with no side effect (systemctl
# is-enabled/is-active + sysfs reads), so it is kept ABOVE check (o)'s OBS restart (#884 ordering).
# The optional-knob `absent` tolerance in imag_maxperf_state_ok keeps this hardware-agnostic (#816).
rc=0
MP_GATHER="$(ssh_box '
printf "SVC_ENABLED=%s\n" "$(systemctl is-enabled imag-maxperf.service 2>/dev/null || echo unknown)"
printf "SVC_ACTIVE=%s\n"  "$(systemctl is-active  imag-maxperf.service 2>/dev/null || echo unknown)"
[ -x /usr/local/sbin/imag-maxperf.sh ] && echo SCRIPT_PRESENT=yes || echo SCRIPT_PRESENT=no
[ -f /etc/udev/rules.d/99-imag-maxperf-pm.rules ] && echo UDEV_PRESENT=yes || echo UDEV_PRESENT=no
printf "GOVERNOR=%s\n" "$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | paste -sd, -)"
mp_epp=$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference 2>/dev/null | paste -sd, -); printf "EPP=%s\n" "${mp_epp:-absent}"
if [ -f /sys/devices/system/cpu/intel_pstate/no_turbo ]; then printf "NO_TURBO=%s\n" "$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null)"; else echo NO_TURBO=absent; fi
if [ -f /sys/firmware/acpi/platform_profile ]; then printf "PLATFORM_PROFILE=%s\n" "$(cat /sys/firmware/acpi/platform_profile 2>/dev/null)"; else echo PLATFORM_PROFILE=absent; fi
')" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$MP_GATHER" ]; then
  fail "could not read the max-performance state over SSH (rc=$rc) -- cannot verify imag-maxperf.service/script/udev + governor/EPP/turbo/profile (#756/#791)"
else
  MP_SVC_ENABLED="$(printf '%s\n' "$MP_GATHER" | sed -n 's/^SVC_ENABLED=//p')"
  MP_SVC_ACTIVE="$(printf '%s\n' "$MP_GATHER" | sed -n 's/^SVC_ACTIVE=//p')"
  MP_SCRIPT="$(printf '%s\n' "$MP_GATHER" | sed -n 's/^SCRIPT_PRESENT=//p')"
  MP_UDEV="$(printf '%s\n' "$MP_GATHER" | sed -n 's/^UDEV_PRESENT=//p')"
  if [ "$MP_SVC_ENABLED" != enabled ] || [ "$MP_SVC_ACTIVE" != active ]; then
    fail "imag-maxperf.service not enabled+active (enabled='${MP_SVC_ENABLED}' active='${MP_SVC_ACTIVE}') -- max-performance persistence (EPP/turbo/PCI-PM) would not survive reboot (#756/#791); setup-imag.sh step 26"
  elif [ "$MP_SCRIPT" != yes ]; then
    fail "/usr/local/sbin/imag-maxperf.sh missing -- imag-maxperf.service would fail at boot (#756/#791); setup-imag.sh step 26 provisions it"
  elif [ "$MP_UDEV" != yes ]; then
    fail "/etc/udev/rules.d/99-imag-maxperf-pm.rules missing -- PCI/USB runtime-PM would revert on hotplug (#756/#791)"
  elif imag_maxperf_state_ok "$MP_GATHER"; then
    ok "full max-performance persistence: imag-maxperf.service active + governor/EPP/turbo/platform-profile all performance (#756/#791)"
  else
    fail "max-performance runtime STATE not performance ($(printf '%s\n' "$MP_GATHER" | grep -E '^(GOVERNOR|EPP|NO_TURBO|PLATFORM_PROFILE)=' | paste -sd' ' -)) -- imag-maxperf did not take effect (#756/#791)"
  fi
fi

# (o) both projectors PRESENT (never self-established) + PERSIST across a real restart (#756/#840)
# ---------------------------------------------------------------------------------------------
# #840: this check used to call obs_phase2.py's projector-OPEN action itself, then count via wmctrl --
# establishing the very condition it then asserted, so it would pass even on a box that comes up
# with ZERO projectors every single boot (the exact live symptom this ticket fixes). It now (1)
# reads the CURRENT state with no side effect and FAILS if it isn't already exactly 1+1, and (2)
# restarts OBS and re-counts, to actually prove the projectors PERSIST across a real restart rather
# than merely CAN be opened once from dev1.
# #890: the restart goes through imag-obs.service (imag_obs_service_restart_cmd) wrapped in a HARD
# execution timeout -- NOT a direct imag-obs-stop.sh/imag-obs-start.sh ssh call. Since #882 that
# wrapper blocks on `wait "$OBS_PID"` (correct for the Type=simple unit systemd owns), so invoking
# it DIRECTLY over ssh never returned and hung this whole gate forever. Since #884 the box's OWN
# boot path IS the unit (check (t) above asserts it), so the service restart is both the
# operator-faithful "real restart" #840 wants AND non-hanging -- and it keeps the new obs supervised
# inside the unit's cgroup. `systemctl --user restart` returns as soon as the unit re-forks obs, so
# the projectors reappear only afterward (obs launch + WS + seed + projector-open, ~90s budget); a
# BOUNDED poll waits for the 1+1 to come back and FAILs loud on expiry, never an unbounded wait.
rc=0
WMCTRL_PATH="$(ssh_box "command -v wmctrl 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$WMCTRL_PATH" ]; then
  missing_tool "wmctrl (apt-get install wmctrl) -- cannot count projector windows"
else
  # #890: the wmctrl reads touch X, so bound them too (a wedged X server could otherwise hang the
  # read) -- symmetric with the post-restart poll below, so check (o) can genuinely NEVER hang.
  MV_COUNT="$(ssh_box_timeout "$SSH_TIMEOUT" "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Multiview' || true" 2>/dev/null || echo 0)"
  PGM_COUNT="$(ssh_box_timeout "$SSH_TIMEOUT" "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Program' || true" 2>/dev/null || echo 0)"
  if imag_projector_counts_ok "${MV_COUNT:-0}" "${PGM_COUNT:-0}"; then
    ok "exactly 1 Multiview + 1 Program projector window BEFORE restart (measured from the box's OWN current state, never opened by this gate, #840)"
  else
    fail "projector count is Multiview=${MV_COUNT:-0} Program=${PGM_COUNT:-0}, expected exactly 1+1 -- the box's OWN startup path did not establish them (#756/#840)"
  fi

  rc=0
  RESTART_OUT="$(ssh_box_timeout "$IMAG_OBS_RESTART_TIMEOUT" "$(imag_obs_service_restart_cmd)" 2>&1)" || rc=$?
  if [ "$rc" -eq 124 ]; then
    fail "OBS restart via 'systemctl --user restart imag-obs.service' TIMED OUT after ${IMAG_OBS_RESTART_TIMEOUT}s -- the unit did not re-activate (#890): $(printf '%s' "$RESTART_OUT" | tr '\n' ' ')"
  elif [ "$rc" -ne 0 ]; then
    fail "OBS restart via 'systemctl --user restart imag-obs.service' failed (rc=$rc): $(printf '%s' "$RESTART_OUT" | tr '\n' ' ') (#890)"
  else
    # systemctl restart returns once the Type=simple unit re-forks obs; the projectors come back
    # only after obs launches + seeds. Poll BOUNDED for the 1+1 to reappear; FAIL loud on expiry.
    persist_ok=0
    MV_COUNT2=0
    PGM_COUNT2=0
    poll_deadline=$((SECONDS + IMAG_OBS_PROJECTOR_POLL_S))
    while :; do
      MV_COUNT2="$(ssh_box_timeout "$SSH_TIMEOUT" "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Multiview' || true" 2>/dev/null || echo 0)"
      PGM_COUNT2="$(ssh_box_timeout "$SSH_TIMEOUT" "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Program' || true" 2>/dev/null || echo 0)"
      if imag_projector_counts_ok "${MV_COUNT2:-0}" "${PGM_COUNT2:-0}"; then
        persist_ok=1
        break
      fi
      if [ "$SECONDS" -ge "$poll_deadline" ]; then
        break
      fi
      sleep 5
    done
    if [ "$persist_ok" -eq 1 ]; then
      ok "projectors PERSIST across a real OBS restart: exactly 1 Multiview + 1 Program after 'systemctl --user restart imag-obs.service' (#840/#890)"
    else
      fail "projectors did NOT persist across a real OBS restart within ${IMAG_OBS_PROJECTOR_POLL_S}s -- Multiview=${MV_COUNT2:-0} Program=${PGM_COUNT2:-0} after 'systemctl --user restart imag-obs.service' (#840/#890)"
    fi
  fi
fi

# (p) operator scaffolding present (#791) ---------------------------------------------------------
OBS_START_MODE="$(ssh_box "ls -la /usr/local/bin/imag-obs-start.sh 2>/dev/null | awk '{print \$1}'" || true)"
if imag_regular_executable_file "${OBS_START_MODE:-}"; then
  ok "/usr/local/bin/imag-obs-start.sh present + executable (#788)"
else
  fail "/usr/local/bin/imag-obs-start.sh missing or not executable (mode='${OBS_START_MODE:-<none>}') (#791/#788)"
fi

if [ -n "${WMCTRL_PATH:-}" ]; then
  ok "wmctrl present on PATH ($WMCTRL_PATH)"
else
  missing_tool "wmctrl (apt-get install wmctrl) -- required for the right-click menu's window management too (#791/#833)"
fi

MENU_TEXT="$(ssh_box "cat /home/${IMAG_USER}/.config/openbox/menu.xml 2>/dev/null" || true)"
if imag_openbox_menu_looks_valid "$MENU_TEXT"; then
  ok "right-click menu (~/.config/openbox/menu.xml) present"
else
  fail "/home/${IMAG_USER}/.config/openbox/menu.xml missing or invalid (#791)"
fi

# #1095: the menu.xml above is only REACHABLE if the openbox rc.xml binds the desktop right-click
# (Root mouse context, Right button) to ShowMenu root-menu. openbox loads the user's
# ~/.config/openbox/rc.xml when present, else the stock /etc/xdg/openbox/rc.xml -- assert whichever
# it will ACTUALLY load, and name that file in the failure so the operator knows where to fix. This
# is a static-file read (independent of check (o)'s OBS restart above), so its placement here is
# harmless (#884 ordering). Assert-only (#1095 design (b)): the gate fails loud on a stale
# bind-elsewhere rc.xml, it NEVER rewrites a hand-tuned operator rc.xml (the same operator-state
# preservation #785 is about).
RC_USER_PATH="/home/${IMAG_USER}/.config/openbox/rc.xml"
RC_STOCK_PATH="/etc/xdg/openbox/rc.xml"
if [ "$(ssh_box "[ -f ${RC_USER_PATH} ] && echo user || echo stock")" = "user" ]; then
  RC_EFFECTIVE_PATH="$RC_USER_PATH"
else
  RC_EFFECTIVE_PATH="$RC_STOCK_PATH"
fi
RC_TEXT="$(ssh_box "cat ${RC_EFFECTIVE_PATH} 2>/dev/null" || true)"
if imag_openbox_root_menu_bound "$RC_TEXT"; then
  ok "openbox rc.xml (${RC_EFFECTIVE_PATH}) binds the desktop right-click to root-menu -- the ~/.config/openbox/menu.xml is reachable (#1095)"
else
  fail "openbox rc.xml (${RC_EFFECTIVE_PATH}) does NOT bind the desktop right-click (Root context, Right button) to ShowMenu root-menu -- the ~/.config/openbox/menu.xml is UNREACHABLE on this box (#1095). Assert-only: fix the operator rc.xml by hand (it is never auto-overwritten, to protect a hand-tuned openbox config)."
fi

WALL_MODE="$(ssh_box "ls -la /home/${IMAG_USER}/Pictures/wall-fallback.png 2>/dev/null | awk '{print \$1}'" || true)"
if imag_regular_file_present "${WALL_MODE:-}"; then
  ok "wall-fallback image (~/Pictures/wall-fallback.png) present"
else
  fail "/home/${IMAG_USER}/Pictures/wall-fallback.png missing (#791) -- an OBS restart would show a black wall"
fi

WATCHDOG_MODE="$(ssh_box "ls -la /usr/local/sbin/imag-obs-watchdog.py 2>/dev/null | awk '{print \$1}'" || true)"
WATCHDOG_UNIT_LIST="$(ssh_box "systemctl list-unit-files imag-obs-watchdog.service --no-legend 2>/dev/null" || true)"
WATCHDOG_ENABLED="$(ssh_box "systemctl is-enabled imag-obs-watchdog 2>/dev/null || true")"
if imag_watchdog_installed_but_disabled "${WATCHDOG_MODE:-}" "${WATCHDOG_UNIT_LIST:-}" "${WATCHDOG_ENABLED:-}"; then
  ok "imag-obs-watchdog installed but disabled (agreed model: boot autostart + menu, no auto-respawn, #756)"
else
  fail "imag-obs-watchdog not in the agreed installed-but-disabled state (script mode='${WATCHDOG_MODE:-<none>}', unit='${WATCHDOG_UNIT_LIST:-<none>}', enabled='${WATCHDOG_ENABLED:-<none>}') (#791)"
fi

# (r) OBS stats dock persistence: global.ini carries a non-empty DockState (#791) --------------
rc=0
GLOBAL_INI="$(ssh_box "cat /home/${IMAG_USER}/.config/obs-studio/global.ini 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$GLOBAL_INI" ]; then
  fail "global.ini unreadable (ssh rc=$rc) -- cannot check stats-dock persistence"
elif imag_dockstate_present "$GLOBAL_INI"; then
  ok "OBS dock layout (incl. the Stats dock) persisted in global.ini -- survives a restart (#791)"
else
  fail "global.ini has no DockState -- the Stats dock (and any other dock arrangement) will NOT survive an OBS restart (#791)"
fi

# (v) power-button + lid + sleep protection (#727) --------------------------------------------
# imag-nb is a PRODUCTION box; a short accidental power-button press suspended it during the
# 2026-07-12 live event. setup-imag.sh step 5 writes the logind drop-ins + masks the sleep
# targets. Prove it is EFFECTIVE on the running box (not merely a file on disk): the running
# logind reports every power/suspend/hibernate/lid key = ignore AND all four sleep targets are
# masked. A re-provision that silently lost step 5 must FAIL here. Pure systemd/logind reads with
# no side effects, so placement after check (o)'s restart is harmless.
rc=0
LOGIND_KEYS="$(ssh_box "loginctl show-seat 2>/dev/null | grep -E '^Handle'")" || rc=$?
MASKED_TARGETS="$(ssh_box "for t in sleep.target suspend.target hibernate.target hybrid-sleep.target; do printf '%s=' \"\$t\"; systemctl is-enabled \"\$t\" 2>&1; done")" || true
if [ "$rc" -ne 0 ] || [ -z "$LOGIND_KEYS" ]; then
  fail "logind key-handling state unreadable (ssh rc=$rc) -- cannot verify the #727 power-button protection"
elif imag_powerkey_protection_ok "$LOGIND_KEYS" "$MASKED_TARGETS"; then
  ok "power-button/lid/suspend/hibernate keys all ignored + sleep/suspend/hibernate/hybrid-sleep targets masked (#727 -- production box can't be accidentally suspended)"
else
  fail "#727 power-button protection NOT effective -- need HandlePowerKey/HandleSuspendKey/HandleHibernateKey/HandleLidSwitch =ignore AND the four sleep targets masked. loginctl: $(printf '%s' "$LOGIND_KEYS" | tr '\n' ' ') || masked: $(printf '%s' "$MASKED_TARGETS" | tr '\n' ' ')"
fi

# (z) display-path tear-free config (issue 1146): picom vsync compositor running + enabled, HDMI
# the xrandr primary, iGPU freq pinned, tap conf. Reuses the SHARED imag_display_path_verdict --
# the SAME verdict drift-guard --check-imag and the E2E [0/8] preflight run (no bespoke logic here).
# A DRIFT (picom off, panel primary, etc.) FAILs; an UNKNOWN (SSH hiccup / unreadable) warns, never
# a false fail. Pure ssh reads (side-effect free), so it is appended at the END.
DP_GATHER="$(ssh_box "$(imag_display_path_gather_remote_snippet)" 2>/dev/null || true)"
if [ -z "$DP_GATHER" ]; then
  warn "display-path config unreadable over SSH -- cannot verify the issue-1146 tear-free config (picom/HDMI-primary)"
else
  while IFS='|' read -r dp_facet dp_status dp_detail; do
    [ -n "$dp_facet" ] || continue
    case "$dp_status" in
      OK)      ok "display-path/${dp_facet}: ${dp_detail}" ;;
      DRIFT)   fail "display-path/${dp_facet} DRIFT: ${dp_detail}" ;;
      *)       warn "display-path/${dp_facet} UNKNOWN: ${dp_detail}" ;;
    esac
  done <<< "$(imag_display_path_verdict "$DP_GATHER")"
fi

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}ALL CLEAR${NC} -- imag-nb (${IMAG_IP}) passes every acceptance check (#821)."
  exit 0
fi
echo -e "${RED}VERIFY FAILED${NC} -- $FAILS check(s) failed on imag-nb (${IMAG_IP}). See [FAIL] lines above." >&2
exit 1
