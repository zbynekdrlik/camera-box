#!/usr/bin/env bash
# dantesync-maintenance-gate.sh -- REPORT-ONLY maintenance-tier dantesync health check for the
# traveling CG box RESOLUME-SNV (issue 1297). See the extended header below; set -e up front per
# script-failure-policy.md.
set -euo pipefail
#
# WHY THIS GATE EXISTS (issue 1297). RESOLUME-SNV runs dantesync but is a TRAVELING box (powered
# off/away between events) and is NOT a measured source in the cam->strih->stream recording path,
# so it must NEVER be wired into recording-e2e.sh's blocking [0/8] precondition set -- that would
# fail-CLOSE every E2E whenever the box is away (the imag-nb issue-1013 pain the ops skill warns
# about). But with no check at all, resolume silently drifted: its ntp_server pointed at strih.lan
# (unresolvable while strih is off) so NTP phase discipline died (ntp_failed, 0 samples, a -14ms
# accumulated phase walk), and phase_slew was off so it STEPS the clock -- exactly the issue-1130
# storm the rig boxes + mbc already cured. The existing gates do not catch this: dantesync-version-
# gate.sh (#862) checks only the daemon VERSION, and dantesync-gate.sh (#7) is the BLOCKING [0/8]
# gate a traveling box cannot join.
#
# So this is a SEPARATE, STANDALONE, maintenance-cadence gate: it asserts resolume's version pin +
# live lock/NTP/phase on the SAME cadence the fleet asserts strih/stream, REUSING the already-unit-
# tested pure parsers rather than reinventing any:
#   * ptp_locked_from_pipe_json / ntp_freshness_verdict (ntp_failed + ntp_age_s) /
#     offset_us_from_pipe_json / clock_discipline_class / abs_int              -- clock-offset-guard.sh
#   * dantesync_version_from_version_output + DANTESYNC_VERSION_PIN                -- dantesync-version-gate.sh
#   * obs_fleet_is_home / obs_fleet_host  (#1296 traveling-box home gate)          -- lib/obs-fleet.sh
# It prints ONE honest row: SKIP when the box is away (never a false red), OK / ALARM / UNKNOWN
# when it is home. It is NOT in [0/8]; run it during maintenance or from a dev1 watchdog.
#
# Acceptance fields graded (issue 1297 #4), all from the SAME :8898/status blob + `dantesync
# --version`: dantesync --version == pin, is_locked (PTP LOCKED), ntp_failed=false AND
# ntp_age_s < 120 (one ntp_freshness_verdict), |ntp_offset_us| < 2000, and the clock discipline
# (issue 1372): dantesync 1.9.0's ptp_phase_lock with ptp_phase_locked=true, or on a pre-1.9.0 node
# phase_slew_enabled=true -- clock_discipline_class, the ONE classifier the E2E gate also uses.
#
# Exit codes: 0 = OK or SKIP (away -- never a false red); 30 = ALARM (home + a field is wrong);
#   11 = UNKNOWN (home but a field could not be read -- never a silent pass); 1 = usage/env error.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"        # ptp_locked_from_pipe_json / ntp_freshness_verdict / offset_us_from_pipe_json / clock_discipline_class / abs_int
# shellcheck source=scripts/dantesync-version-gate.sh
. "$HERE/dantesync-version-gate.sh"    # dantesync_version_from_version_output + DANTESYNC_VERSION_PIN
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"             # obs_fleet_is_home / obs_fleet_host

MAINT_FRESHNESS_S="${DANTESYNC_MAINT_FRESHNESS_S:-120}"
MAINT_BOUND_US="${DANTESYNC_MAINT_BOUND_US:-2000}"
MAINT_STATUS_PORT="${DANTESYNC_MAINT_STATUS_PORT:-8898}"
MAINT_SSH_TIMEOUT="${DANTESYNC_MAINT_SSH_TIMEOUT:-10}"
MAINT_HTTP_TIMEOUT="${DANTESYNC_MAINT_HTTP_TIMEOUT:-6}"
# The Windows dantesync.exe path for read_maint_version's OWN thin ssh reader. We write our own
# transport (not dantesync-version-gate.sh's read_dantesync_version_output) because that function
# lives BELOW that gate's source-guard and is UNDEFINED when we source the gate for its pure parser
# (dantesync-version-reading.md's documented split). To keep the two readers from drifting, DEFAULT
# this (and the ssh pass, below) from the version-gate's OWN override env vars when they are set, so
# an operator who repoints one repoints both; overridable in its own right for tests/ops.
DANTESYNC_MAINT_WIN_EXE="${DANTESYNC_MAINT_WIN_EXE:-${DANTESYNC_VERSION_GATE_WIN_EXE:-C:\\Program Files\\DanteSync\\dantesync.exe}}"

# --- PURE verdict (no network/ssh -- unit-tested by sourcing this file) -------------------------

# dantesync_maintenance_verdict NAME HOME_FLAG VERSION_OUT STATUS_JSON PIN FRESHNESS_S BOUND_US ->
# prints ONE honest row on stdout and returns 0 OK / 30 ALARM / 11 UNKNOWN. HOME_FLAG "1" = home;
# anything else -> SKIP row + return 0 (a traveling box that is away is never a false red). When
# home, every acceptance field is graded from VERSION_OUT (`dantesync --version` stdout) + the
# STATUS_JSON blob via the shared parsers; an UNREADABLE field is UNKNOWN, never a silent OK
# (test-strictness). ALARM only when a field is READ and WRONG.
dantesync_maintenance_verdict() {
  local name="$1" home="$2" vout="$3" status="$4" pin="$5" fresh="$6" bound="$7"
  if [ "$home" != "1" ]; then
    printf '  %-10s SKIP     (away -- obs_fleet_is_home false; traveling box powered off/unreachable)\n' "$name"
    return 0
  fi

  local version ptp ntpf off disc
  version="$(dantesync_version_from_version_output "$vout")"
  ptp="$(ptp_locked_from_pipe_json "$status")"
  ntpf="$(ntp_freshness_verdict "$status" "$fresh")"
  off="$(offset_us_from_pipe_json "$status")"
  disc="$(clock_discipline_class "$status")"

  # Per-field token + state (OK|ALARM|UNKNOWN). Worst state wins the aggregate.
  local agg="OK"
  local vtok ptok ntok otok stok
  # version
  if [ -z "$version" ]; then vtok="ver?"; agg="UNKNOWN"
  elif [ "$version" = "$pin" ]; then vtok="v${version}"
  else vtok="v${version}!=${pin}"; [ "$agg" = UNKNOWN ] || agg="ALARM"; fi
  # PTP lock
  case "$ptp" in
    LOCKED)   ptok="PTP LOCKED" ;;
    DEGRADED) ptok="PTP DEGRADED"; [ "$agg" = UNKNOWN ] || agg="ALARM" ;;
    *)        ptok="PTP?"; agg="UNKNOWN" ;;
  esac
  # NTP freshness (ntp_failed + ntp_age_s together)
  case "$ntpf" in
    fresh)  ntok="NTP fresh" ;;
    stale)  ntok="NTP stale(failed/age>${fresh}s)"; [ "$agg" = UNKNOWN ] || agg="ALARM" ;;
    never)  ntok="NTP never(0 samples)"; [ "$agg" = UNKNOWN ] || agg="ALARM" ;;
    *)      ntok="NTP?(no ntp_age_s)"; agg="UNKNOWN" ;;
  esac
  # offset bound
  if [ -z "$off" ] || ! printf '%s' "$off" | grep -qE '^-?[0-9]+$'; then
    otok="off?"; agg="UNKNOWN"
  elif [ "$(abs_int "$off")" -le "$bound" ]; then
    otok="off ${off}us"
  else
    otok="off ${off}us>${bound}"; [ "$agg" = UNKNOWN ] || agg="ALARM"
  fi
  # clock discipline (issue 1372): the dantesync 1.9.0 PTP phase lock, or a pre-1.9.0 node's
  # phase_slew state. A phase lock that is not locked is a READ, wrong field -> ALARM.
  case "$disc" in
    PTP_PHASE_LOCK) stok="clock ptp_phase_lock" ;;
    LEGACY_SLEW)    stok="phase-slew ENABLED" ;;
    LEGACY_NO_SLEW) stok="phase-slew DISABLED"; [ "$agg" = UNKNOWN ] || agg="ALARM" ;;
    *)
      if [ "$(clock_discipline_from_pipe_json "$status")" = ptp_phase_lock ] \
         && [ "$(ptp_phase_locked_from_pipe_json "$status")" = false ]; then
        stok="ptp-phase UNLOCKED"; [ "$agg" = UNKNOWN ] || agg="ALARM"
      else
        stok="clock-discipline?"; agg="UNKNOWN"
      fi ;;
  esac

  printf '  %-10s %-8s (%s | %s | %s | %s | %s)\n' \
    "$name" "$agg" "$vtok" "$ptok" "$ntok" "$otok" "$stok"
  case "$agg" in
    OK)      return 0 ;;
    ALARM)   return 30 ;;
    *)       return 11 ;;
  esac
}

# --- source-guard: when sourced (the unit tests), stop here -------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) -----------------------------------------------------

usage() { sed -n '2,40p' "$0"; }

BOX="resolume"
PIN="${DANTESYNC_VERSION_PIN}"
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h)      usage; exit 0 ;;
    --box)          BOX="${2:?--box needs a name}"; shift 2 ;;
    --pin)          PIN="${2:?--pin needs a version}"; shift 2 ;;
    --freshness-s)  MAINT_FRESHNESS_S="${2:?--freshness-s needs seconds}"; shift 2 ;;
    --bound-us)     MAINT_BOUND_US="${2:?--bound-us needs microseconds}"; shift 2 ;;
    *)              printf 'ERROR: unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

# read_maint_status NAME HOST -> the box's :PORT/status JSON (curl from this box -- no ssh, no MCP).
# A DANTESYNC_MAINT_STATUS_<NAME> env var (uppercased, non-alnum -> _) overrides with a fixture for
# Tier-0 tests. `|| true` keeps an unreachable box from aborting under set -e (-> empty -> UNKNOWN).
read_maint_status() {
  local name="$1" host="$2" key
  key="DANTESYNC_MAINT_STATUS_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!key:-}" ]; then printf '%s' "${!key}"; return 0; fi
  curl -fsS --max-time "$MAINT_HTTP_TIMEOUT" "http://${host}:${MAINT_STATUS_PORT}/status" 2>/dev/null || true
}

# read_maint_version NAME HOST -> `dantesync --version` stdout read over ssh (Windows exe path runs
# via cmd.exe directly -- dantesync-version-reading.md). A DANTESYNC_MAINT_VERSION_<NAME> fixture
# overrides for tests. `|| true` -> empty on failure -> UNKNOWN downstream, never a guessed version.
read_maint_version() {
  local name="$1" host="$2" key
  key="DANTESYNC_MAINT_VERSION_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!key:-}" ]; then printf '%s' "${!key}"; return 0; fi
  sshpass -p "${SSH_PASS:-${DANTESYNC_VERSION_GATE_SSH_PASS:-newlevel}}" ssh -o StrictHostKeyChecking=no \
    -o "ConnectTimeout=${MAINT_SSH_TIMEOUT}" "${DANTESYNC_MAINT_SSH_USER:-newlevel}@${host}" \
    "\"${DANTESYNC_MAINT_WIN_EXE}\" --version" 2>/dev/null || true
}

echo "== dantesync-maintenance-gate (issue 1297): REPORT-ONLY clock health for ${BOX} (pin ${PIN}) =="
HOST="$(obs_fleet_host "$BOX" 2>/dev/null || true)"
if [ -z "$HOST" ]; then
  printf '  %-10s UNKNOWN  (box not in the OBS_FLEET table -- cannot resolve its host)\n' "$BOX"
  exit 11
fi

if obs_fleet_is_home "$BOX"; then HOME_FLAG=1; else HOME_FLAG=0; fi
if [ "$HOME_FLAG" = 1 ]; then
  STATUS_JSON="$(read_maint_status "$BOX" "$HOST")"
  VERSION_OUT="$(read_maint_version "$BOX" "$HOST")"
else
  STATUS_JSON=""; VERSION_OUT=""
fi

rc=0
dantesync_maintenance_verdict "$BOX" "$HOME_FLAG" "$VERSION_OUT" "$STATUS_JSON" \
  "$PIN" "$MAINT_FRESHNESS_S" "$MAINT_BOUND_US" || rc=$?
echo
case "$rc" in
  0)  echo "MAINT OK (or SKIP away) -- ${BOX} is on the pinned dantesync and phase-disciplined, or is away." ;;
  30) echo "!! MAINT ALARM: ${BOX} is HOME but a clock field is wrong (see the row) -- fix per .claude/rules/resolume-dantesync.md. REPORT-ONLY (never blocks [0/8])." >&2 ;;
  11) echo "!! MAINT UNKNOWN: ${BOX} is HOME but a field could not be read -- fix SSH/HTTP reachability, then re-run. REPORT-ONLY." >&2 ;;
esac
exit "$rc"
