#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function lib (never executed directly) -- must NOT set -e,
# which would propagate into recording-e2e.sh's own carefully-scoped set +e/-e regions when this
# is sourced there (same convention as scripts/lib/obs-session-visibility.sh).
#
# scripts/lib/strih-platform.sh -- issue 1351: resolve the strih box's OBS PLATFORM
# (windows | linux) after the M4 cut-over (strih role moved from the Windows STRIH-SNV PC to the
# Linux notebook strih-lx, 10.77.9.202, running OBS under `strih-obs.service` on GNOME/Wayland),
# plus the Linux-side operator-session-visibility predicate that replaces the Windows-only
# obs64/AHK CIM probe (scripts/lib/obs-session-visibility.sh, #977/#958) for a Linux strih.
#
# Design-by: main (issue 1351, comment 5751246658) -- Approach 1: a per-box platform resolver +
# two branched touch-points in scripts/recording-e2e.sh, the Windows path kept byte-identical
# (#675 sourced-helper pattern -- every existing anchored line in recording-e2e.sh is unchanged;
# only NEW branching lines call into this lib).
#
# strih_platform HOST -> "windows" (default) | "linux". Pure, no network:
#   1. STRIH_PLATFORM env override wins outright (windows|linux only; any other/typo'd value is
#      IGNORED and falls through to the host-based resolution below -- never abort a live run on
#      a bad env value).
#   2. HOST matching the known strih-lx address (STRIH_LX_HOST, default 10.77.9.202 -- the M4
#      cut-over target; also recording-e2e.sh's own $STRIH default post-cutover) -> "linux".
#   3. otherwise -> "windows" (the old STRIH-SNV box on any other address, or any future/other
#      target -- unaffected, parallel-run tolerant).
strih_platform() {
  local host="${1:-}"
  case "${STRIH_PLATFORM:-}" in
    windows|linux)
      printf '%s' "$STRIH_PLATFORM"
      return 0
      ;;
  esac
  local lx_host="${STRIH_LX_HOST:-10.77.9.202}"
  if [ -n "$host" ] && [ "$host" = "$lx_host" ]; then
    printf 'linux'
  else
    printf 'windows'
  fi
}

# strih_linux_visibility_probe_cmd -> REMOTE bash TEXT (embed via $(...) into an ssh command
# string, plain ssh -- NEVER win_ssh_run/CIM for a Linux strih). Checks whether strih-obs.service
# is active under the OPERATOR's own user session (systemd --user), the Linux analogue of "visible
# to the operator on the console" the Windows obs64/AHK SessionId probe answers for STRIH-SNV.
# Read-only: no writes, no relaunch. Emits ONE KEY=VALUE line a pure parser below classifies,
# mirroring obs_session_visibility_probe_ps's shape.
strih_linux_visibility_probe_cmd() {
  cat <<'CMD'
SVC_ACTIVE=$(systemctl --user is-active strih-obs.service 2>/dev/null || echo inactive)
echo "SVC_ACTIVE=$SVC_ACTIVE"
CMD
}

# strih_linux_visibility_message SYSTEMCTL_PROBE_OUT WS_OK(0|1) -> pure parser (no network, no
# ssh). Empty string = fully visible (strih-obs.service active under the operator session AND
# obs-websocket answering on :4455); any other case returns a non-empty, human-readable diagnosis.
#
# WS_OK is the CALLER's own empirical result of a real obs-websocket round-trip (recording-e2e.sh
# already has one: `python3 obs_phase2.py record --host "$STRIH" --action status`, which performs
# a full WS connect via the SAME obsws-python client `record --action stop` uses at [7/8] -- reused
# here rather than a second hand-rolled WS client, per the design's "obs_phase2.py already speaks
# WS" rationale) -- this function stays a PURE classifier over that already-computed signal, same
# split as obs_session_visibility_message (probe text in, diagnosis out, no I/O of its own).
strih_linux_visibility_message() {
  local probe_out="${1:-}" ws_ok="${2:-0}"
  probe_out="${probe_out//$'\r'/}"
  if [ -z "$probe_out" ]; then
    printf 'no probe output (ssh/connectivity failure -- strih-lx unreachable, or the command did not run) -- issue 1351'
    return 0
  fi
  local svc
  svc="$(printf '%s\n' "$probe_out" | sed -n 's/^SVC_ACTIVE=//p' | tail -n1)"
  if [ "${svc:-}" != "active" ]; then
    printf 'strih-obs.service is not active under the operator session (systemctl --user is-active -> %s) -- issue 1351' "${svc:-<absent>}"
    return 0
  fi
  if [ "$ws_ok" != "1" ]; then
    printf 'strih-obs.service is active but obs-websocket on :4455 did not answer (GetRecordStatus failed) -- issue 1351'
    return 0
  fi
  printf ''
}

# strih_linux_visibility_check HOST USER PW TIMEOUT HERE -> the LIVE orchestrator the [0/8] gate
# calls directly (plain ssh -- NEVER win_ssh_run/CIM). Runs the systemd probe over ssh (bounded by
# TIMEOUT, same discipline as the Windows win_ssh_run call it replaces) + a real obs-websocket
# round-trip via the existing obs_phase2.py (record --action status, the SAME WS client
# `record --action stop` already uses at [7/8]), then feeds both into the pure classifier above.
# Kept OUT of recording-e2e.sh itself so the [0/8] call site stays a single line (#675 pattern).
strih_linux_visibility_check() {
  local host="$1" user="$2" pw="$3" tmo="$4" here="$5"
  local sysout ws_ok=0
  sysout="$(timeout "$tmo" sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 \
    "${user}@${host}" "$(strih_linux_visibility_probe_cmd)" 2>/dev/null || true)"
  python3 "$here/obs_phase2.py" record --host "$host" --action status >/dev/null 2>&1 && ws_ok=1
  strih_linux_visibility_message "$sysout" "$ws_ok"
}
