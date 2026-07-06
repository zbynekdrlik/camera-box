#!/usr/bin/env bash
#
# verify-device.sh -- POST-REBOOT runtime acceptance gate for a freshly-provisioned camera-box
# appliance (#454). This is the fourth and final phase of the unified provisioning method:
#
#   1. build USB    scripts/create-usb-linux.sh --target-disk /dev/sdX --yes   (#448)
#   2. boot + reach the box over SSH                                          (:22 up)
#   3. provision    sudo ./setup-device.sh NAME                               (#450, name-resolved)
#   4. ACCEPT       scripts/verify-device.sh NAME                             (#454, THIS script)
#
# Distinct from setup-device.sh's own STEP 19 -- that is an INSTALL-TIME (pre-reboot, still inside
# the live setup session) FILE-PRESENCE check ("is the binary/NDI .so on disk"). This script is a
# RUNTIME check: it runs AFTER the box has rebooted, connects fresh over SSH, and re-derives every
# fact from LIVE signals (systemd state, journald, `ls -la`, `avahi-browse`) -- never trusting the
# installer's own claim of success. The honest proof a fresh box is "identically built" to the
# fleet (#454's acceptance criterion).
#
# Composes ALREADY-TESTED signals instead of reinventing them:
#   - scripts/camera-set.sh          camera_resolve() -- NAME -> IP / CAMERA_GENLOCK_FPS (#24/#451)
#   - scripts/lib/ndi-alive.sh       emit_ok_grep_pattern() / fatal_grep_pattern() (#451)
#   - scripts/clock-offset-guard.sh  offset_us_from_journal() / offset_check() /
#                                    ptp_locked_from_journal() (#8)
#
# Usage:
#   scripts/verify-device.sh NAME
#   scripts/verify-device.sh --help
#
# NAME is resolved via scripts/camera-set.sh (cam1-7), case-insensitive (CAM5 / cam5 both work) --
# same convention as setup-device.sh (#450). An unknown name fails loud through camera-set.sh's own
# fail-closed `case` (never silently certifies the wrong box).
#
# Env:
#   SSH_USER          SSH user for the box (default: root)
#   CAM_PW            box root password (default: newlevel -- the fleet's standard dev password,
#                      same fallback used throughout scripts/deploy-fleet.sh, clock-offset-guard.sh,
#                      setup-device.sh; override via env for a box with a different password)
#   SSH_TIMEOUT       SSH connect timeout in seconds (default: 10)
#   CLOCK_GUARD_BOUND_US  dantesync clock-offset bound in microseconds (default: 2000, #8)
#
# Checks (all must pass):
#   (a) camera-box --version is a valid, well-formed fleet version string
#   (b) systemctl is-active camera-box == active
#   (c) NDI sender is streaming (journalctl emit-fps / genlock-report line), no FATAL/panic
#   (d) dantesync PTP servo LOCKED + clock offset within bound (scripts/clock-offset-guard.sh)
#   (e) genlock.conf drop-in present, CAMERA_BOX_GENLOCK_FPS matches camera-set.sh's per-cam value
#   (f) cpu-affinity.conf drop-in present (CPUAffinity=<isolated core>)
#   (g) /usr/lib/ndi/libndi.so.6 is a root-owned symlink chain to a root-owned regular file
#   (h) avahi mDNS NDI discovery sees this box's NDI source (avahi-browse -tp _ndi._tcp)
#   (i) capture chroma metric reports "colour", not "grayscale" (#299 regression signal)
#   (j) root filesystem mounted read-only (#547 -- ro appliance)
#   (k) exactly ONE installed kernel, equal to the running one (#547; optional KERNEL_PIN match)
#   (l) fwupd purged (#547 -- it holds a write handle that blocks the ro remount)
#   (m) systemd-networkd-wait-online masked (#547 -- avoids the 120s boot stall)
#   (n) core-isolation kernel cmdline: isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)
#   (o) NDI runtime pinned to the fleet version (NDI_VERSION_PIN, default 6.3.2 -- #132/#547)
#   (p) config.toml [display] section matches camera-set.sh's CAMERA_DISPLAY_SOURCE table entry
#       (#528/#557/#558 -- catches a box that lost its HDMI-preview config, or wrongly gained one)
#
# Exit: 0 iff every check passes. Non-zero if ANY check FAILs or is UNREADABLE (test-strictness --
# an unreachable/unreadable check is a FAIL, never a silent pass).

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; NC='\033[0m'

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"          # camera_resolve() -- NAME -> IP / CAMERA_GENLOCK_FPS (#24/#451)
# shellcheck source=scripts/lib/ndi-alive.sh
. "$HERE/lib/ndi-alive.sh"       # emit_ok_grep_pattern() / fatal_grep_pattern() (#451)
# clock-offset-guard.sh is sourced ONLY for its pure functions; its own
# `[ "${BASH_SOURCE[0]}" != "${0}" ]` guard skips clock-offset-guard.sh's own `main "$@"` flow.
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"  # offset_us_from_journal() / offset_check() / ptp_locked_from_journal()

SSH_USER="${SSH_USER:-root}"
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"
DEVICE_CLOCK_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
EXPECT_KERNEL="${KERNEL_PIN:-}"                 # optional: also require running kernel == this exact version
NDI_VERSION_PIN="${NDI_VERSION_PIN:-6.3.2}"     # fleet NDI runtime pin (#132/#547)

# =================================================================================================
# PURE functions (no network, no SSH -- unit-tested from tests/verify_device_pure_functions.rs by
# sourcing this file; the BASH_SOURCE guard below skips the live SSH flow when sourced. Same
# convention as scripts/setup-device.sh / scripts/clock-offset-guard.sh.)
# =================================================================================================

# --- (a) version -------------------------------------------------------------------------------

# version_is_valid_format V -> 0 iff V matches the fleet's vMAJOR.MINOR.PATCH[-dev.N] shape (the
# CI dev-channel form, e.g. "1.7.0-dev.244", or a plain release "1.7.0").
version_is_valid_format() {
  printf '%s' "$1" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-dev\.[0-9]+)?$'
}

# version_matches_expected ACTUAL EXPECTED -> 0 iff both are non-empty and identical.
version_matches_expected() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# --- (b) systemd service state -------------------------------------------------------------------

# active_state_is_active TEXT -> 0 iff TEXT (the trimmed `systemctl is-active` output) is exactly
# "active". Any other state (inactive/failed/activating/empty) is NOT active.
active_state_is_active() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "active" ]
}

# --- (c) NDI emit + FATAL scan (reuses scripts/lib/ndi-alive.sh) -------------------------------

# ndi_emit_ok JOURNAL_TEXT -> 0 iff the camera-box journal shows the NDI capture->emit path alive
# (the shared emit_ok_grep_pattern() from ndi-alive.sh -- genlock decimation report / plain
# "Streaming: X fps" / generic sender-ready line).
ndi_emit_ok() {
  printf '%s\n' "$1" | grep -qE "$(emit_ok_grep_pattern)"
}

# ndi_journal_has_fatal JOURNAL_TEXT -> 0 iff the journal contains a crash signature (the shared
# fatal_grep_pattern() from ndi-alive.sh).
ndi_journal_has_fatal() {
  printf '%s\n' "$1" | grep -qE "$(fatal_grep_pattern)"
}

# --- (i) colour capture chroma metric (#299) ----------------------------------------------------

# chroma_state_from_journal JOURNAL_TEXT -> the LAST "capture chroma: ... -> (colour|grayscale...)"
# line from a camera-box journal dump ("" if none seen -- the #299 chroma sample is logged only
# after the first sample lands, so a very-fresh box may not show one yet).
chroma_state_from_journal() {
  printf '%s\n' "$1" | grep -oE 'capture chroma:.*-> (colour|grayscale[^"]*)' | tail -1 || true
}

# chroma_check STATE -> 0 iff STATE ends in "-> colour" (healthy); 2 if "-> grayscale..." (the
# #299 regression signal -- capture card delivering monochrome frames); 3 if STATE is empty
# (UNKNOWN -- no chroma sample seen yet; never treated as a silent pass).
chroma_check() {
  case "$1" in
    *'-> colour') return 0 ;;
    *'-> grayscale'*) return 2 ;;
    *) return 3 ;;
  esac
}

# --- (d) dantesync PTP-lock + offset (reuses scripts/clock-offset-guard.sh) --------------------

# dantesync_locked_ok JOURNAL_TEXT -> 0 iff the dantesync journal's most recent clock event is a
# PTP servo line (ptp_locked_from_journal() == "LOCKED"), never DEGRADED or UNKNOWN.
dantesync_locked_ok() {
  [ "$(ptp_locked_from_journal "$1")" = "LOCKED" ]
}

# dantesync_offset_ok JOURNAL_TEXT BOUND_US -> 0 iff the most recent "[NTP] offset:" reading is
# within BOUND_US (offset_check() from clock-offset-guard.sh; UNKNOWN/malformed is never OK).
dantesync_offset_ok() {
  local journal="$1" bound="$2" offset
  offset="$(offset_us_from_journal "$journal")"
  offset_check "dantesync" "$offset" "$bound" >/dev/null
}

# --- (e) genlock.conf drop-in --------------------------------------------------------------------

# genlock_dropin_fps TEXT -> the numeric value of CAMERA_BOX_GENLOCK_FPS in TEXT (the contents of
# the camera-box.service.d/genlock.conf drop-in), "" if absent. `|| true` (per the #458 footgun:
# a `grep`-into-`tail`-into-`cut` pipeline with no match fails under pipefail even though `tail`/
# `cut` both succeed on empty input) -- a bare bareword-assignment caller (`X="$(genlock_dropin_fps
# ...)"`, as the live flow below does) must NEVER abort the whole script on a merely-missing value.
genlock_dropin_fps() {
  printf '%s\n' "$1" | grep -oE 'CAMERA_BOX_GENLOCK_FPS=[0-9]+' | tail -1 | cut -d= -f2 || true
}

# genlock_fps_matches ACTUAL EXPECTED -> 0 iff both non-empty and identical.
genlock_fps_matches() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# --- (f) cpu-affinity.conf drop-in ----------------------------------------------------------------

# cpu_affinity_dropin_value TEXT -> the numeric value of CPUAffinity in TEXT (the contents of the
# camera-box.service.d/cpu-affinity.conf drop-in), "" if absent (#289). `|| true` -- same #458
# footgun as genlock_dropin_fps above (a bare-assignment caller must never abort on a merely-
# missing value).
cpu_affinity_dropin_value() {
  printf '%s\n' "$1" | grep -oE 'CPUAffinity=[0-9]+' | tail -1 | cut -d= -f2 || true
}

# --- (g) libndi root-owned symlink chain --------------------------------------------------------

# ndi_symlink_target TEXT -> echoes the resolved target filename of the `libndi.so.6` entry in
# TEXT (an `ls -la /usr/lib/ndi` listing) IFF it is a SYMLINK owned root:root; prints nothing
# (empty) otherwise. Matches the EXACT filename field (avoids a substring false-match against
# `libndi.so` or `libndi.so.6.3.2.0`).
ndi_symlink_target() {
  awk '
    $0 ~ /->/ {
      n = split($0, f, " ")
      if (n < 9) next
      if (substr(f[1], 1, 1) != "l") next
      if (f[9] != "libndi.so.6") next
      if (f[3] != "root" || f[4] != "root") next
      idx = index($0, "-> ")
      if (idx == 0) next
      print substr($0, idx + 3)
      exit 0
    }
  ' <<< "$1"
}

# ndi_regular_file_root_owned TEXT NAME -> 0 iff TEXT (an `ls -la` listing) has a line whose exact
# filename field is NAME, is a REGULAR file (mode starts with "-"), and is owned root:root.
ndi_regular_file_root_owned() {
  local text="$1" name="$2"
  awk -v want="$name" '
    {
      n = split($0, f, " ")
      if (n < 9) next
      if (f[9] != want) next
      if (substr(f[1], 1, 1) != "-") next
      if (f[3] == "root" && f[4] == "root") { found = 1 }
    }
    END { exit(found ? 0 : 1) }
  ' <<< "$text"
}

# ndi_symlink_chain_ok TEXT -> 0 iff /usr/lib/ndi/libndi.so.6 is a root-owned SYMLINK pointing at a
# root-owned REGULAR file -- the canonical fleet layout. The #445 cam3-outlier layout (real files,
# user-owned -- its manual NDI upgrade never fit the fleet script) FAILS this by design: the
# acceptance gate certifies the CANONICAL build, not cam3's drift.
ndi_symlink_chain_ok() {
  local text="$1" target
  target="$(ndi_symlink_target "$text")"
  [ -n "$target" ] || return 1
  ndi_regular_file_root_owned "$text" "$target"
}

# --- (h) avahi mDNS NDI discovery -----------------------------------------------------------------

# avahi_ndi_discoverable TEXT [WANT] -> 0 iff TEXT (an `avahi-browse -tp _ndi._tcp` dump) shows at
# least one NDI service record (a `+` found or `=` resolved parseable line referencing
# `_ndi._tcp`); when WANT is given, at least one such record must also contain WANT (e.g. "CAM5").
avahi_ndi_discoverable() {
  local text="$1" want="${2:-}" matches
  matches="$(printf '%s\n' "$text" | grep -E '^[+=]' | grep -F '_ndi._tcp' || true)"
  [ -n "$matches" ] || return 1
  [ -z "$want" ] && return 0
  printf '%s\n' "$matches" | grep -qF "$want"
}

# --- (j)-(o) fleet-uniformity invariants (#547) -------------------------------------------------
# Every cambox must be IDENTICAL: read-only root, exactly ONE pinned kernel, fwupd purged,
# systemd-networkd-wait-online masked, the #289/#303 core-isolation cmdline, and the pinned NDI
# runtime. These pure decision functions are unit-tested; the live flow below feeds them real
# post-reboot signals gathered over SSH.

# root_mount_is_readonly OPTS -> 0 iff the FIRST comma-token of a mount-options string is exactly
# "ro" (the kernel always emits ro/rw first). Substring-safe: a rw mount carrying
# "errors=remount-ro" is correctly NOT read as read-only.
root_mount_is_readonly() {
  case "$1" in
    ro | ro,*) return 0 ;;
    *) return 1 ;;
  esac
}

# kernels_uniform_ok BOOT_LS RUNNING -> 0 iff BOOT_LS (an `ls -1 /boot/vmlinuz-*` dump) lists
# EXACTLY ONE installed kernel AND its version equals RUNNING (`uname -r`). Two kernels (the cam4
# drift) or a running/installed mismatch FAILs.
kernels_uniform_ok() {
  local boot_ls="$1" running="$2" count ver
  [ -n "$running" ] || return 1
  count="$(printf '%s\n' "$boot_ls" | grep -c 'vmlinuz-' || true)"
  [ "$count" = "1" ] || return 1
  ver="$(printf '%s\n' "$boot_ls" | grep -oE 'vmlinuz-.*' | head -1 | sed 's#.*vmlinuz-##')"
  [ "$ver" = "$running" ]
}

# fwupd_absent STATE -> 0 iff STATE (trimmed `systemctl is-enabled fwupd` output, or "not-found"
# when the unit/package is gone) shows fwupd is NOT installed. The fleet PURGES fwupd (it held a
# write handle that blocked the ro remount); a unit still present in ANY state (enabled/static/
# disabled/masked) FAILs -- "masked but installed" is not identical to "purged".
fwupd_absent() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    '' | not-found) return 0 ;;
    *) return 1 ;;
  esac
}

# fwupd_verdict RC STATE -> echoes "unreadable" | "ok" | "present". Guards the false-green unique to
# (l): fwupd_absent treats an EMPTY state as "purged" (pass), but a transient ssh failure on the (l)
# call ALSO yields empty stdout -- so an rc!=0 (unreadable) MUST be a FAIL (the file's
# "unreachable = FAIL, never a silent pass" contract), NOT read as purged. (l) is the only new check
# whose empty value means pass -- (j)/(k)/(m)/(n)/(o) fail-safe on empty -- so only it needs this.
fwupd_verdict() {
  if [ "$1" -ne 0 ] 2>/dev/null; then
    printf 'unreadable\n'
  elif fwupd_absent "$2"; then
    printf 'ok\n'
  else
    printf 'present\n'
  fi
}

# waitonline_masked STATE -> 0 iff STATE (trimmed `systemctl is-enabled
# systemd-networkd-wait-online`) is exactly "masked". The fleet MASKS it -- unmasked it timed out
# 120s and delayed network-online.target, starting camera-box ~123s late (cam3). Any other state
# FAILs.
waitonline_masked() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "masked" ]
}

# cmdline_has_isolation CMDLINE -> 0 iff /proc/cmdline carries ALL of the core-isolation flags
# isolcpus=3 (#289) + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#303), each as a whole
# space-delimited token (so nohz_full=3 never matches nohz_full=30).
cmdline_has_isolation() {
  local cmdline="$1" tok
  for tok in 'isolcpus=3' 'nohz_full=3' 'rcu_nocbs=3' 'irqaffinity=0-2'; do
    printf '%s' " $cmdline " | grep -qE "[[:space:]]${tok}[[:space:]]" || return 1
  done
  return 0
}

# ndi_symlink_version LS_TEXT -> the version portion of the libndi.so.6 symlink target in an
# `ls -la /usr/lib/ndi` listing (e.g. "6.3.2.0" from "libndi.so.6 -> libndi.so.6.3.2.0"), "" if
# the target is not resolvable. Builds on ndi_symlink_target (the root-owned symlink check).
ndi_symlink_version() {
  local target
  target="$(ndi_symlink_target "$1")"
  [ -n "$target" ] || return 0
  printf '%s\n' "${target#libndi.so.}"
}

# ndi_version_matches ACTUAL PIN -> 0 iff ACTUAL equals PIN or begins "PIN." (so the fleet pin
# "6.3.2" accepts both the 3-part soname "6.3.2" and the 4-part SDK string "6.3.2.0", but never
# "6.2.1" or the deceptive "6.3.20").
ndi_version_matches() {
  [ -n "$1" ] && [ -n "$2" ] || return 1
  case "$1" in
    "$2" | "$2".*) return 0 ;;
    *) return 1 ;;
  esac
}

# --- (p) config.toml [display] vs CAMERA_DISPLAY_SOURCE table (#528/#557/#558) -------------------
# A box that LOST its [display] section (rolled back, hand-edited, or provisioned via the
# divergent scripts/setup.sh path -- #557) previously still reported ALL CLEAR here. These two
# functions close that blind spot: the reader half of setup-device.sh's / setup.sh's
# config_toml_display_section() writer, plus the pure comparison against camera-set.sh's per-cam
# table entry.

# config_toml_display_source TEXT -> the value of `source = "..."` inside a `[display]` TOML
# section in TEXT (the contents of /etc/camera-box/config.toml), "" if no [display] section is
# present (or it has no `source` key, or TEXT is empty/unreadable). Unescapes `\"` and `\\` back
# to their literal characters -- the inverse of config_toml_display_section()'s escaping (undone
# in reverse order: quotes first, then backslashes, since the writer escaped backslashes first
# then quotes).
config_toml_display_source() {
  awk '
    /^\[display\][[:space:]]*$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^source[[:space:]]*=/ {
      line = $0
      sub(/^source[[:space:]]*=[[:space:]]*"/, "", line)
      sub(/"[[:space:]]*$/, "", line)
      gsub(/\\"/, "\"", line)
      gsub(/\\\\/, "\\", line)
      print line
      exit
    }
  ' <<< "$1"
}

# display_config_verdict EXPECTED ACTUAL -> echoes "ok" | "missing" | "drift" | "unexpected".
#   EXPECTED = scripts/camera-set.sh's CAMERA_DISPLAY_SOURCE table entry for this box ("" for a
#              box with no configured preview -- every box except cam1 today).
#   ACTUAL   = config_toml_display_source() read back from the LIVE config.toml ("" if absent).
# ok         -- EXPECTED == ACTUAL (both empty -- no preview configured or expected -- or both the
#               same non-empty source).
# missing    -- EXPECTED is non-empty but ACTUAL is empty: the box SHOULD have a preview but its
#               [display] section is absent/lost.
# drift      -- EXPECTED and ACTUAL are both non-empty but DIFFERENT: config.toml has the WRONG
#               source wired up.
# unexpected -- EXPECTED is empty but ACTUAL is non-empty: a box that should have NO preview
#               somehow got one (e.g. provisioned via a stale/divergent path).
display_config_verdict() {
  local expected="$1" actual="$2"
  if [ -z "$expected" ]; then
    if [ -z "$actual" ]; then
      printf 'ok\n'
    else
      printf 'unexpected\n'
    fi
  else
    if [ -z "$actual" ]; then
      printf 'missing\n'
    elif [ "$actual" = "$expected" ]; then
      printf 'ok\n'
    else
      printf 'drift\n'
    fi
  fi
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the live SSH flow below.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# =================================================================================================
# LIVE flow (executed only when run directly) -- requires sshpass + network access to the box.
# =================================================================================================

usage() {
  cat <<EOF
verify-device.sh -- POST-REBOOT runtime acceptance gate for a freshly-provisioned camera-box (#454).

Usage:
  scripts/verify-device.sh NAME
  scripts/verify-device.sh --help

NAME is resolved via scripts/camera-set.sh (cam1-7), case-insensitive (CAM5 / cam5 both work).

Checks:
  (a) camera-box --version is a valid, well-formed fleet version string
  (b) systemctl is-active camera-box == active
  (c) NDI sender is streaming (journalctl emit-fps / genlock-report line), no FATAL/panic
  (d) dantesync PTP servo LOCKED + clock offset within bound (scripts/clock-offset-guard.sh)
  (e) genlock.conf drop-in present, CAMERA_BOX_GENLOCK_FPS matches camera-set.sh's per-cam value
  (f) cpu-affinity.conf drop-in present (CPUAffinity=<isolated core>)
  (g) /usr/lib/ndi/libndi.so.6 is a root-owned symlink chain to a root-owned regular file
  (h) avahi mDNS NDI discovery sees this box's NDI source (avahi-browse -tp _ndi._tcp)
  (i) capture chroma metric reports "colour" (not "grayscale") -- #299 regression signal
  (j) root filesystem mounted read-only (ro appliance)
  (k) exactly ONE installed kernel, equal to the running one (optional KERNEL_PIN exact match)
  (l) fwupd purged (blocks the ro remount)
  (m) systemd-networkd-wait-online masked (no 120s boot stall)
  (n) core-isolation cmdline: isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)
  (o) NDI runtime pinned to the fleet version (NDI_VERSION_PIN, default 6.3.2)
  (p) config.toml [display] section matches camera-set.sh's CAMERA_DISPLAY_SOURCE table entry

Env: KERNEL_PIN (optional exact running-kernel pin), NDI_VERSION_PIN (default 6.3.2).

Exit: 0 iff every check passes.
EOF
}

DEVICE_ARG="${1:-}"
case "$DEVICE_ARG" in
  -h|--help) usage; exit 0 ;;
esac
if [ -z "$DEVICE_ARG" ]; then
  usage >&2
  exit 1
fi

# Resolve NAME -> IP / CAMERA_GENLOCK_FPS via camera-set.sh (case-insensitive, same normalization
# as setup-device.sh's resolve_device_name -- fail loud on an unknown name via camera_resolve()'s
# own fail-closed `case`).
LC_NAME="$(printf '%s' "$DEVICE_ARG" | tr '[:upper:]' '[:lower:]')"
camera_resolve "$LC_NAME" || exit 1
NAME_UPPER="$(printf '%s' "$CAMERA_NAME" | tr '[:lower:]' '[:upper:]')"
IP="$CAMERA_IP"
EXPECT_FPS="$CAMERA_GENLOCK_FPS"

command -v sshpass >/dev/null 2>&1 || { echo -e "${RED}ERROR: sshpass is required${NC}" >&2; exit 1; }

echo -e "${GREEN}== verify-device (#454): ${NAME_UPPER} @ ${IP} ==${NC}"

FAILS=0
ok()   { printf "  ${GREEN}[OK]${NC}   %s\n" "$1"; }
fail() { printf "  ${RED}[FAIL]${NC} %s\n" "$1"; FAILS=$((FAILS + 1)); }

ssh_box() {
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    "${SSH_USER}@${IP}" "$1"
}

# (a) version -------------------------------------------------------------------------------------
rc=0
VERSION="$(ssh_box "/usr/local/bin/camera-box --version 2>/dev/null | awk '{print \$NF}'")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$VERSION" ]; then
  fail "camera-box --version unreadable (ssh rc=$rc)"
elif ! version_is_valid_format "$VERSION"; then
  fail "camera-box --version '$VERSION' is not a well-formed fleet version string"
else
  ok "camera-box --version = $VERSION"
fi

# (b) service active --------------------------------------------------------------------------
rc=0
SVC_STATE="$(ssh_box "systemctl is-active camera-box 2>/dev/null")" || rc=$?
if active_state_is_active "$SVC_STATE"; then
  ok "camera-box.service active"
else
  fail "camera-box.service not active (state='${SVC_STATE:-<none>}', ssh rc=$rc)"
fi

# (c) + (i) camera-box journal: NDI emit alive, no FATAL, capture-chroma colour -----------------
rc=0
CB_JOURNAL="$(ssh_box "journalctl -u camera-box --no-pager -n 300 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$CB_JOURNAL" ]; then
  fail "camera-box journal unreadable (ssh rc=$rc)"
  fail "capture chroma metric unreadable -- camera-box journal unreadable"
else
  if ndi_emit_ok "$CB_JOURNAL"; then
    ok "NDI sender streaming (emit/genlock report seen in the last 300 journal lines)"
  else
    fail "no NDI emit/streaming report seen in the last 300 camera-box journal lines"
  fi
  if ndi_journal_has_fatal "$CB_JOURNAL"; then
    fail "camera-box journal contains a FATAL/panic signature"
  else
    ok "no FATAL/panic in camera-box journal"
  fi

  chroma_state="$(chroma_state_from_journal "$CB_JOURNAL")"
  chroma_rc=0
  chroma_check "$chroma_state" || chroma_rc=$?
  case "$chroma_rc" in
    0) ok "capture chroma: colour ($chroma_state)" ;;
    2) fail "capture chroma reports GRAYSCALE -- capture card delivering monochrome frames (#299): $chroma_state" ;;
    *) fail "capture chroma metric not seen in the journal (box may be too fresh -- no sample yet)" ;;
  esac
fi

# (d) dantesync PTP lock + offset ----------------------------------------------------------------
rc=0
DS_JOURNAL="$(ssh_box "journalctl -u dantesync --no-pager -n 200 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$DS_JOURNAL" ]; then
  fail "dantesync journal unreadable (ssh rc=$rc)"
else
  if dantesync_locked_ok "$DS_JOURNAL"; then
    ok "dantesync PTP servo LOCKED"
  else
    fail "dantesync PTP servo not LOCKED (degraded or unknown)"
  fi
  if dantesync_offset_ok "$DS_JOURNAL" "$DEVICE_CLOCK_BOUND_US"; then
    ok "dantesync clock offset within ${DEVICE_CLOCK_BOUND_US}us bound"
  else
    fail "dantesync clock offset outside the ${DEVICE_CLOCK_BOUND_US}us bound (or unreadable)"
  fi
fi

# (e) genlock.conf drop-in --------------------------------------------------------------------
rc=0
GL_CONF="$(ssh_box "cat /etc/systemd/system/camera-box.service.d/genlock.conf 2>/dev/null")" || rc=$?
ACTUAL_FPS="$(genlock_dropin_fps "$GL_CONF")"
if [ -z "$ACTUAL_FPS" ]; then
  fail "genlock.conf drop-in missing or CAMERA_BOX_GENLOCK_FPS not set (ssh rc=$rc)"
elif genlock_fps_matches "$ACTUAL_FPS" "$EXPECT_FPS"; then
  ok "genlock.conf CAMERA_BOX_GENLOCK_FPS=$ACTUAL_FPS (matches camera-set.sh: $EXPECT_FPS)"
else
  fail "genlock.conf CAMERA_BOX_GENLOCK_FPS=$ACTUAL_FPS but camera-set.sh expects $EXPECT_FPS"
fi

# (f) cpu-affinity.conf drop-in -----------------------------------------------------------------
rc=0
CA_CONF="$(ssh_box "cat /etc/systemd/system/camera-box.service.d/cpu-affinity.conf 2>/dev/null")" || rc=$?
CA_VAL="$(cpu_affinity_dropin_value "$CA_CONF")"
if [ -n "$CA_VAL" ]; then
  ok "cpu-affinity.conf present (CPUAffinity=$CA_VAL)"
else
  fail "cpu-affinity.conf drop-in missing or CPUAffinity not set (ssh rc=$rc)"
fi

# (g) libndi root-owned symlink chain ------------------------------------------------------------
rc=0
NDI_LS="$(ssh_box "ls -la /usr/lib/ndi 2>/dev/null")" || rc=$?
if ndi_symlink_chain_ok "$NDI_LS"; then
  ok "libndi.so.6 is a root-owned symlink chain to a root-owned regular file"
else
  fail "libndi.so.6 is not a root-owned symlink chain (ssh rc=$rc) -- see \`ls -la /usr/lib/ndi\`"
fi

# (h) avahi mDNS NDI discovery -----------------------------------------------------------------
rc=0
AVAHI_OUT="$(ssh_box "avahi-browse -tp _ndi._tcp 2>/dev/null")" || rc=$?
if avahi_ndi_discoverable "$AVAHI_OUT" "$NAME_UPPER"; then
  ok "avahi mDNS sees this box's NDI source ($NAME_UPPER)"
else
  fail "avahi-browse did not see an NDI source for $NAME_UPPER (ssh rc=$rc)"
fi

# (j) root filesystem mounted read-only (#547 -- ro appliance) -----------------------------------
rc=0
MOUNT_OPTS="$(ssh_box "findmnt -no OPTIONS / 2>/dev/null || awk '\$2==\"/\"{print \$4; exit}' /proc/mounts")" || rc=$?
if root_mount_is_readonly "$MOUNT_OPTS"; then
  ok "root filesystem mounted read-only (ro appliance)"
else
  fail "root filesystem is NOT read-only (opts='${MOUNT_OPTS:-<none>}', ssh rc=$rc)"
fi

# (k) exactly one installed kernel, equal to the running one (#547 -- one kernel) ----------------
rc=0
BOOT_LS="$(ssh_box "ls -1 /boot/vmlinuz-* 2>/dev/null")" || rc=$?
RUNNING_KERNEL="$(ssh_box "uname -r 2>/dev/null")" || rc=$?
if kernels_uniform_ok "$BOOT_LS" "$RUNNING_KERNEL"; then
  ok "single installed kernel == running ($RUNNING_KERNEL)"
else
  fail "kernel not uniform -- want exactly one installed kernel equal to the running one (running='${RUNNING_KERNEL:-?}', installed='$(printf '%s' "$BOOT_LS" | tr '\n' ' ')', ssh rc=$rc)"
fi
if [ -n "$EXPECT_KERNEL" ]; then
  if [ "$RUNNING_KERNEL" = "$EXPECT_KERNEL" ]; then
    ok "running kernel matches the fleet pin ($EXPECT_KERNEL)"
  else
    fail "running kernel '${RUNNING_KERNEL:-?}' != fleet pin '$EXPECT_KERNEL' (KERNEL_PIN)"
  fi
fi

# (l) fwupd purged (#547 -- it holds a write handle blocking the ro remount) ---------------------
rc=0
# NB: `systemctl is-enabled` prints the state (e.g. "masked"/"disabled") to STDOUT *and* exits
# non-zero for those states -- so `|| true` (never `|| echo <sentinel>`, which would APPEND a
# second word to the captured state and break the exact-match checks below); a purged unit prints
# nothing -> empty state, which fwupd_absent accepts.
FWUPD_STATE="$(ssh_box "systemctl is-enabled fwupd 2>/dev/null || true")" || rc=$?
case "$(fwupd_verdict "$rc" "$FWUPD_STATE")" in
  ok) ok "fwupd is not installed (purged)" ;;
  unreadable) fail "fwupd state unreadable (ssh rc=$rc) -- unreachable check is a FAIL, never a silent pass" ;;
  *) fail "fwupd still present (state='${FWUPD_STATE}') -- purge it; it blocks the ro remount" ;;
esac

# (m) systemd-networkd-wait-online masked (#547 -- avoids the 120s boot stall) -------------------
rc=0
WAITONLINE_STATE="$(ssh_box "systemctl is-enabled systemd-networkd-wait-online 2>/dev/null || true")" || rc=$?
if waitonline_masked "$WAITONLINE_STATE"; then
  ok "systemd-networkd-wait-online masked (no 120s boot stall)"
else
  fail "systemd-networkd-wait-online not masked (state='${WAITONLINE_STATE}') -- unmasked it delays network-online.target ~120s (ssh rc=$rc)"
fi

# (n) core-isolation kernel cmdline (#289 isolcpus + #303 nohz_full/rcu_nocbs/irqaffinity) -------
rc=0
CMDLINE="$(ssh_box "cat /proc/cmdline 2>/dev/null")" || rc=$?
if cmdline_has_isolation "$CMDLINE"; then
  ok "kernel cmdline carries isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)"
else
  fail "kernel cmdline missing a core-isolation flag (#289/#303): '${CMDLINE:-<none>}' (ssh rc=$rc)"
fi

# (o) NDI runtime pinned to the fleet version (#132/#547) ----------------------------------------
# Reuses NDI_LS gathered in (g); ndi_symlink_version extracts the version from the symlink target.
NDI_VER="$(ndi_symlink_version "$NDI_LS")"
if ndi_version_matches "$NDI_VER" "$NDI_VERSION_PIN"; then
  ok "NDI runtime pinned to $NDI_VERSION_PIN (active: ${NDI_VER})"
else
  fail "NDI runtime '${NDI_VER:-?}' != fleet pin '$NDI_VERSION_PIN' (NDI_VERSION_PIN)"
fi

# (p) config.toml [display] vs camera-set.sh's CAMERA_DISPLAY_SOURCE table (#528/#557/#558) ------
# CAMERA_DISPLAY_SOURCE was already resolved by camera_resolve() above (top of the live flow) --
# "" for every box except cam1 today. A box that lost its [display] section (rolled back,
# hand-edited, or provisioned via the divergent scripts/setup.sh path -- #557) previously still
# reported ALL CLEAR here; a box that wrongly GAINED one is caught too (display_config_verdict's
# "unexpected" case).
rc=0
CONFIG_TOML="$(ssh_box "cat /etc/camera-box/config.toml 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "config.toml unreadable (ssh rc=$rc) -- cannot verify [display] section"
else
  DISPLAY_ACTUAL="$(config_toml_display_source "$CONFIG_TOML")"
  case "$(display_config_verdict "$CAMERA_DISPLAY_SOURCE" "$DISPLAY_ACTUAL")" in
    ok)
      if [ -n "$CAMERA_DISPLAY_SOURCE" ]; then
        ok "config.toml [display] source matches camera-set.sh ('$CAMERA_DISPLAY_SOURCE')"
      else
        ok "config.toml has no [display] section (none expected for this box)"
      fi
      ;;
    missing)
      fail "config.toml [display] section MISSING -- expected source '$CAMERA_DISPLAY_SOURCE' (camera-set.sh)"
      ;;
    drift)
      fail "config.toml [display] source '${DISPLAY_ACTUAL}' != camera-set.sh's '${CAMERA_DISPLAY_SOURCE}'"
      ;;
    unexpected)
      fail "config.toml has an UNEXPECTED [display] section (source='${DISPLAY_ACTUAL}') -- camera-set.sh has no table entry for this box"
      ;;
  esac
fi

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}ALL CLEAR${NC} -- $NAME_UPPER passes every acceptance check (#454)."
  exit 0
fi
echo -e "${RED}VERIFY FAILED${NC} -- $FAILS check(s) failed on $NAME_UPPER. See [FAIL] lines above." >&2
exit 1
