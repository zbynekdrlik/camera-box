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
#   - scripts/obs_phase2.py open-projectors + wmctrl window count -- the EXACT mechanism
#                              recording-e2e.sh's own "[0/8] imag-nb Multiview + Program
#                              projectors" preflight already proves live (#758/#756)
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
#   (d) /proc/cmdline carries preempt=full + the DERIVED isolcpus=/nohz_full=/irqaffinity= (from
#       THIS box's own CPU topology via imag_cpu_isolation_plan -- never a hardcoded literal, #816)
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
#   (n) scenes present (Cam 1-6) and Multiview populated (MV Cam 1-6)
#   (o) both projectors open -- exactly 1 Program (HDMI) + 1 Multiview (panel), no stray windows
#   (p) operator scaffolding present (#791): /usr/local/bin/imag-obs-start.sh, wmctrl, the
#       right-click menu (~/.config/openbox/menu.xml), the wall-fallback image, the watchdog
#       installed-but-disabled (#756)
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
IMAG_CLOCK_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
DANTESYNC_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"
DANTESYNC_JOURNAL_MAX_AGE_S="${DANTESYNC_JOURNAL_MAX_AGE_S:-60}"
# #834: the rig's PTP grandmaster every node must agree on. Every DanteSync status-pipe/HTTP
# payload across this whole codebase (clock_offset_guard.rs's own real fixtures, dantesync-gate.sh's
# banner "GM = 10.77.9.184") pins this same address.
RIG_GRANDMASTER_IP="${RIG_GRANDMASTER_IP:-10.77.9.184}"
# #824: same pin + same default setup-imag.sh itself uses -- a superseded PPA binary breaks every
# stock plugin (obs-websocket included) if the base version drifts past the genlock build's own.
IMAG_OBS_BASE_VERSION="${IMAG_OBS_BASE_VERSION:-32.1.2-0obsproject1~noble}"
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
  printf '%s' " $text " | grep -qF " ${ip} " || printf '%s' " $text " | grep -qF " ${ip}/"
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

# --- (d) kernel cmdline: preempt=full + DERIVED isolation (#289/#303/#816) -------------------

# imag_cmdline_has_preempt_full CMDLINE -> 0 iff CMDLINE carries the whole-token `preempt=full`
# flag (the #482 low-latency-kernel config).
imag_cmdline_has_preempt_full() {
  printf '%s' " $1 " | grep -qE '[[:space:]]preempt=full[[:space:]]'
}

# imag_cmdline_has_derived_isolation CMDLINE ISOLATED NOHZ HOUSEKEEP -> 0 iff CMDLINE carries
# isolcpus=ISOLATED + nohz_full=NOHZ + irqaffinity=HOUSEKEEP as whole space-delimited tokens.
# ISOLATED/NOHZ/HOUSEKEEP are the plan THIS box's own topology derives via
# imag_cpu_isolation_plan() (#816) -- never a hardcoded literal (a different core count, or the
# other known imag box, would derive a genuinely different plan; asserting a fixed cam-fleet-style
# literal here would be the exact #816 regression this script must not reintroduce).
imag_cmdline_has_derived_isolation() {
  local cmdline="$1" isolated="$2" nohz="$3" housekeep="$4"
  [ -n "$isolated" ] && [ -n "$nohz" ] && [ -n "$housekeep" ] || return 1
  printf '%s' " $cmdline " | grep -qE "[[:space:]]isolcpus=${isolated}[[:space:]]" \
    && printf '%s' " $cmdline " | grep -qE "[[:space:]]nohz_full=${nohz}[[:space:]]" \
    && printf '%s' " $cmdline " | grep -qE "[[:space:]]irqaffinity=${housekeep}[[:space:]]"
}

# --- (e) display-manager -> lightdm + autologin; gdm3 absent ---------------------------------

# imag_autologin_conf_ok TEXT USER -> 0 iff TEXT (the 50-imag-autologin.conf drop-in contents)
# carries both `autologin-user=USER` and `autologin-session=openbox` as whole lines.
imag_autologin_conf_ok() {
  local text="$1" user="$2"
  [ -n "$user" ] || return 1
  printf '%s\n' "$text" | grep -qxF "autologin-user=${user}" \
    && printf '%s\n' "$text" | grep -qxF "autologin-session=openbox"
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
  ! printf '%s' "$1" | grep -qE '__[A-Za-z_]+__'
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
  printf '%s\n' "$1" | grep -qxF "$2"
}

# --- (h) OBS log: genlock tick, no version-mismatch, DistroAV + NDI loaded -------------------

# imag_obs_log_shows_genlock_tick LOG_TEXT -> 0 iff LOG_TEXT carries a genlock capability marker.
# EXACT same regex family as setup-imag.sh's own step-18 verify + scripts/drift-guard.sh's
# genlock_capability_from_log -- never a second, drifting pattern.
imag_obs_log_shows_genlock_tick() {
  printf '%s' "$1" | grep -iqE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)'
}

# imag_obs_log_no_version_mismatch LOG_TEXT -> 0 iff LOG_TEXT contains NO "compiled with newer
# libobs" warning -- the #824 regression signature (a superseded OBS base whose libobs is OLDER
# than a stock plugin's build refuses to load it; left OBS with ONLY distroav.so, no
# obs-websocket, no encoders).
imag_obs_log_no_version_mismatch() {
  ! printf '%s' "$1" | grep -qi 'compiled with newer libobs'
}

# imag_obs_log_shows_distroav_loaded LOG_TEXT -> 0 iff LOG_TEXT shows the DistroAV plugin loaded
# (same grep verify-device.sh's own log-verify convention and setup-imag.sh step 18 both use).
imag_obs_log_shows_distroav_loaded() {
  printf '%s' "$1" | grep -qi '\[distroav\] plugin loaded'
}

# imag_obs_log_shows_ndi_loaded LOG_TEXT -> 0 iff LOG_TEXT shows the NDI runtime initialized.
imag_obs_log_shows_ndi_loaded() {
  printf '%s' "$1" | grep -qi 'NDI library initialized'
}

# --- (j) OBS base version pin + apt-mark hold (#824) ------------------------------------------

# imag_obs_base_version_matches ACTUAL EXPECTED -> 0 iff both non-empty and identical.
imag_obs_base_version_matches() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# imag_pkg_is_held HOLD_LIST_TEXT NAME -> 0 iff NAME appears as an exact line in HOLD_LIST_TEXT
# (an `apt-mark showhold` dump).
imag_pkg_is_held() {
  printf '%s\n' "$1" | grep -qxF "$2"
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

# imag_scenes_output_ok STDOUT -> 0 iff STDOUT (imag_scenes.py's own printed report) shows BOTH
# the 6 "Cam N" scenes and the 6 "MV Cam N" scenes fully present. Text-based (not just the exit
# code) so a caller can distinguish which set is short from the SAME captured output the operator
# would read.
imag_scenes_output_ok() {
  # Line-anchored (^) -- "MV scenes: 6/6 OK" contains "scenes: 6/6 OK" as a plain SUBSTRING, so an
  # unanchored -F match would wrongly pass the main-scenes check off the MV line alone even when
  # the main "scenes: N/6" line reports a shortfall. Anchor each to its own line's start.
  printf '%s\n' "$1" | grep -qE '^scenes: 6/6 OK' && printf '%s\n' "$1" | grep -qE '^MV scenes: 6/6 OK'
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

# --- (p) operator scaffolding present (#791) ---------------------------------------------------

# imag_openbox_menu_looks_valid TEXT -> 0 iff TEXT (the ~/.config/openbox/menu.xml contents) is
# non-empty and contains a `<menu` tag (basic XML sanity -- never pass on an empty/corrupted file).
imag_openbox_menu_looks_valid() {
  [ -n "$(printf '%s' "$1" | tr -d '[:space:]')" ] && printf '%s' "$1" | grep -qi '<menu'
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
  printf '%s' "$unit_list" | grep -qF "imag-obs-watchdog.service" || return 1
  [ "$enabled" = "disabled" ]
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

ssh_box() {
  sshpass -p "$IMAG_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    "${IMAG_USER}@${IMAG_IP}" "$1"
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
HWE_STATUS="$(ssh_box "dpkg -s linux-image-generic-hwe-24.04 2>/dev/null | sed -n 's/^Status: //p' || true")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "HWE kernel dpkg status unreadable (ssh rc=$rc)"
elif imag_hwe_kernel_installed "$HWE_STATUS"; then
  ok "kernel on the HWE line (linux-image-generic-hwe-24.04 installed)"
else
  fail "linux-image-generic-hwe-24.04 not installed (status='${HWE_STATUS:-<none>}') -- #819 GA-baseline regression"
fi

# (d) kernel cmdline: preempt=full + DERIVED isolation (#289/#303/#816) --------------------------
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
  # Derive the EXPECTED plan from this box's own live topology -- the identical computation
  # setup-imag.sh step 8 runs, never a hardcoded literal (#816: a different core count, or the
  # other known imag box, derives a genuinely different plan).
  rc=0
  TOPO="$(ssh_box "for f in /sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list; do [ -r \"\$f\" ] || continue; c=\"\${f#/sys/devices/system/cpu/cpu}\"; c=\"\${c%%/*}\"; printf '%s %s\n' \"\$c\" \"\$(cat \"\$f\")\"; done | sort -n -k1,1")" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$TOPO" ]; then
    fail "CPU topology unreadable over SSH (rc=$rc) -- cannot derive the expected isolation plan"
  else
    PLAN=""
    PLAN="$(printf '%s\n' "$TOPO" | imag_cpu_isolation_plan)" || true
    ISOLATED="$(printf '%s\n' "$PLAN" | sed -n 1p)"
    NOHZ="$(printf '%s\n' "$PLAN" | sed -n 2p)"
    HOUSEKEEP="$(printf '%s\n' "$PLAN" | sed -n 3p)"
    if [ -z "$ISOLATED" ] || [ -z "$NOHZ" ] || [ -z "$HOUSEKEEP" ]; then
      fail "could not derive the CPU isolation plan from this box's own topology (#816)"
    elif imag_cmdline_has_derived_isolation "$CMDLINE" "$ISOLATED" "$NOHZ" "$HOUSEKEEP"; then
      ok "kernel cmdline carries the DERIVED isolcpus=${ISOLATED} nohz_full=${NOHZ} irqaffinity=${HOUSEKEEP} (#816)"
    else
      fail "kernel cmdline missing the derived isolcpus=${ISOLATED} nohz_full=${NOHZ} irqaffinity=${HOUSEKEEP}: '$CMDLINE'"
    fi
  fi
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
GDM3_STATUS="$(ssh_box "dpkg -s gdm3 2>/dev/null | sed -n 's/^Status: //p' || true")" || rc=$?
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
OBS_PKG_VERSION="$(ssh_box "dpkg-query -W -f='\${Version}' obs-studio 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$OBS_PKG_VERSION" ]; then
  fail "obs-studio package version unreadable (ssh rc=$rc)"
elif imag_obs_base_version_matches "$OBS_PKG_VERSION" "$IMAG_OBS_BASE_VERSION"; then
  ok "obs-studio package version = ${OBS_PKG_VERSION} (matches the pinned genlock build)"
else
  fail "obs-studio package version '${OBS_PKG_VERSION}' != pinned '${IMAG_OBS_BASE_VERSION}' (#824)"
fi

rc=0
HOLD_LIST="$(ssh_box "apt-mark showhold 2>/dev/null")" || rc=$?
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
    DRIVER_STATUS="$(ssh_box "dpkg -s nvidia-driver-595-open 2>/dev/null | sed -n 's/^Status: //p' || true")"
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
DS_JOURNAL="$(ssh_box "journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null")" || rc=$?
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
    case "$(dantesync_offset_verdict "$DS_JOURNAL" "$DANTESYNC_OFFSET_FRESHNESS_S" "$IMAG_CLOCK_BOUND_US")" in
      ok) ok "dantesync clock offset within ${IMAG_CLOCK_BOUND_US}us bound (fresh)" ;;
      drift) fail "dantesync clock offset OUTSIDE the ${IMAG_CLOCK_BOUND_US}us bound -- a real clock desync" ;;
      *) fail "dantesync clock offset has no FRESH reading -- status incomplete" ;;
    esac
    fail "grandmaster identity unreadable via the journal path (no gm_source_ip in journald text) -- the :8898/status endpoint is required to certify #834; it was unreachable above"
  fi
fi

# (m) dantesync is the SOLE timesync authority (#591) --------------------------------------------
rc=0
TS_STATES="$(ssh_box "$(timesync_gather_remote_snippet)")" || rc=$?
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
rc=0
SCENES_OUT="$(python3 "$HERE/imag_scenes.py" --host "$IMAG_IP" 2>&1)" || rc=$?
if imag_scenes_output_ok "$SCENES_OUT"; then
  ok "scenes: Cam 1-6 + MV Cam 1-6 all present"
else
  fail "scenes/Multiview membership incomplete (rc=$rc): $(printf '%s' "$SCENES_OUT" | tr '\n' ' ')"
fi

# (o) both projectors open -- exactly 1 Program + 1 Multiview (#756/#758) -----------------------
rc=0
PROJ_OUT="$(python3 "$HERE/obs_phase2.py" open-projectors --host "$IMAG_IP" 2>&1)" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "could not open the Program/Multiview projectors (rc=$rc): $(printf '%s' "$PROJ_OUT" | tr '\n' ' ')"
else
  ok "projectors opened/confirmed: $(printf '%s' "$PROJ_OUT" | tr '\n' ' ')"
fi

rc=0
WMCTRL_PATH="$(ssh_box "command -v wmctrl 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$WMCTRL_PATH" ]; then
  missing_tool "wmctrl (apt-get install wmctrl) -- cannot count projector windows"
else
  MV_COUNT="$(ssh_box "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Multiview' || true")"
  PGM_COUNT="$(ssh_box "DISPLAY=:0 wmctrl -l 2>/dev/null | grep -c 'Projector - Program' || true")"
  if imag_projector_counts_ok "${MV_COUNT:-0}" "${PGM_COUNT:-0}"; then
    ok "exactly 1 Multiview + 1 Program projector window (no stray accumulation, #756)"
  else
    fail "projector count is Multiview=${MV_COUNT:-0} Program=${PGM_COUNT:-0}, expected exactly 1+1 (#756)"
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

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}ALL CLEAR${NC} -- imag-nb (${IMAG_IP}) passes every acceptance check (#821)."
  exit 0
fi
echo -e "${RED}VERIFY FAILED${NC} -- $FAILS check(s) failed on imag-nb (${IMAG_IP}). See [FAIL] lines above." >&2
exit 1
