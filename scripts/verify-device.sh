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

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}ALL CLEAR${NC} -- $NAME_UPPER passes every acceptance check (#454)."
  exit 0
fi
echo -e "${RED}VERIFY FAILED${NC} -- $FAILS check(s) failed on $NAME_UPPER. See [FAIL] lines above." >&2
exit 1
