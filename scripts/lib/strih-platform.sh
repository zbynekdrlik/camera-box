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
# issue 1317 part 4: the ONE authority for "is this address the Linux strih" is the obs-fleet list
# (scripts/lib/obs-fleet.sh `obs_fleet_name_for_host`, alias-aware). Lazy-sourced so every caller of
# this lib (recording-e2e.sh, the strih log reader, the handover check, the tests) gets it.
command -v obs_fleet_name_for_host >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/obs-fleet.sh"

# strih_platform HOST -> "windows" (default) | "linux". No network for an IP literal:
#   1. STRIH_PLATFORM env override wins outright (windows|linux only; any other/typo'd value is
#      IGNORED and falls through to the host-based resolution below -- never abort a live run on
#      a bad env value).
#   2. HOST equal to STRIH_LX_HOST (when the operator sets it -- an explicit ops/test override for
#      a strih-lx at another address) -> "linux".
#   3. HOST addressing a fleet row whose CLASS is linux-genlock (issue 1317 part 4:
#      `obs_fleet_class_for_host` -- 10.77.9.202, `strih-lx`, `STRIH-LX`, `strih-lx.lan`, and
#      `strih.lan`, the retired Windows PC's own name that the rig DNS points at 10.77.9.202) ->
#      "linux". The SAME alias-aware lookup the Windows-tool class gate uses, so the two resolvers
#      cannot disagree; keyed on the row's CLASS, never a literal box name, so the next strih (a Linux
#      strih-pp) is one OBS_FLEET row with no code edit. A non-IP name is resolved through the fleet
#      lib's time-bounded getent seam; an IP literal never is.
#   4. otherwise -> "windows" (any other address -- a future Windows strih, parallel-run tolerant).
strih_platform() {
  local host="${1:-}"
  case "${STRIH_PLATFORM:-}" in
    windows|linux)
      printf '%s' "$STRIH_PLATFORM"
      return 0
      ;;
  esac
  if [ -n "$host" ] && [ -n "${STRIH_LX_HOST:-}" ] && [ "$host" = "$STRIH_LX_HOST" ]; then
    printf 'linux'
    return 0
  fi
  if [ -n "$host" ] && [ "$(obs_fleet_class_for_host "$host" 2>/dev/null || true)" = "linux-genlock" ]; then
    printf 'linux'
  else
    printf 'windows'
  fi
}

# ---- recording-e2e.sh [8/8] plan TEXT (issue 1317 part 4) -------------------------------------------
# The [8/8a] strih decode runs over plain ssh/scp on strih-lx (recording-verdict-on-strih-lx.sh, which
# also pulls the partial + pixel proofs back itself); the Windows strih ran it via the win-strih MCP.
# These print the platform-correct lines so the harness never tells an operator to run a win-strih
# FileDownload / Remove-Item against the Linux box. Kept here (not inline) per the recording-e2e.sh
# anchor discipline: new behavior in a sourced helper, the call site a single line.

# strih_access_label HOST -> how the strih box is reached in the [8/8a] banner.
strih_access_label() {
  if [ "$(strih_platform "${1:-}")" = "linux" ]; then
    printf 'strih-lx, plain ssh/scp'
  else
    printf 'win-strih'
  fi
}

# strih_obs_restart_hint HOST -> how an operator restarts the strih OBS (issue 1361): the systemd user
# unit on a Linux strih, the launch-obs-genlock.sh win-* MCP plan on a Windows one. Used by the
# zero-loss restart mode's instruction text.
strih_obs_restart_hint() {
  if [ "$(strih_platform "${1:-}")" = "linux" ]; then
    printf 'systemctl --user restart strih-obs.service over plain ssh'
  else
    printf 'scripts/launch-obs-genlock.sh via the win-* MCP'
  fi
}

# strih_lx_partial_pullback_note PARTIAL PIXELS -> the strih-lx [8/8a] pull-back lines (the Linux
# decode sibling already pulled both back over scp). The Windows win-strih FileDownload text stays
# INLINE in recording-e2e.sh's else-branch: tests/harness_recording_e2e_paths.rs pins it there.
strih_lx_partial_pullback_note() {
  printf '    pull back to dev1: %s  AND the #186 pixel-proof dir %s\n' "${1:-}" "${2:-}"
  printf '      (strih-lx: already pulled back over scp by recording-verdict-on-strih-lx.sh;\n'
  printf '       the pixel-proof dir is absent on a clean run)\n'
}

# strih_lx_recording_cleanup_note INDENT HOST PATH [USER] -> the strih-lx #652 cleanup PLAN line: an
# `rm -f --` of the EXACT StopRecord path over plain ssh (never a glob, never a directory sweep).
# The line is pasted into a LOCAL shell and ssh then hands the joined command to the REMOTE shell, so
# the path is quoted for BOTH parses (review round 1): single-quoted for the remote shell (an embedded
# ' becomes '\''), and that remote command is one double-quoted argument for the local shell (\ " $ `
# escaped). An OBS default filename carries a space -- it stays exactly one argument remotely.
strih_lx_recording_cleanup_note() {
  local indent="${1:-}" host="${2:-}" path="${3:-<unknown>}" user="${4:-${STRIH_USER:-newlevel}}"
  local remote dq
  remote="rm -f -- '${path//\'/\'\\\'\'}'"
  dq="${remote//\\/\\\\}"
  dq="${dq//\"/\\\"}"
  dq="${dq//\$/\\\$}"
  dq="${dq//\`/\\\`}"
  printf '%sstrih-lx ssh:         ssh %s@%s "%s"\n' "$indent" "$user" "$host" "$dq"
}

# strih_planner_holder_note HOST -> the first two lines of the per-box PLANNER hand-off note. On
# strih-lx the [8/8a] decode already ran over ssh/scp, so only 8/8b is left for the win-* MCP holder.
strih_planner_holder_note() {
  if [ "$(strih_platform "${1:-}")" = "linux" ]; then
    printf '    The win-* MCP holder runs 8/8b on stream (strih-lx 8/8a and imag 8/8c ALREADY ran above --\n'
    printf '    plain ssh/scp, no MCP needed), pulls the stream partial (+ its <partial>-pixels\n'
    return 0
  fi
  printf '    The win-* MCP holder runs 8/8a + 8/8b on strih+stream (imag'"'"'s 8/8c ALREADY ran above —\n'
  printf '    #462, plain ssh, no MCP needed), pulls the strih+stream partials (+ their <partial>-pixels\n'
}

# strih_platform_refuse_windows_only_mode HOST WHAT -> returns 0 (silent) when HOST's strih platform
# is windows; returns 1 with a named error on stderr when it is linux. The ONE guard a Windows-only
# strih code path (a win-* MCP / PowerShell plan with no strih-lx port yet) calls BEFORE doing
# anything, so the Linux strih is refused loudly instead of receiving a Windows plan (issue 1317
# part 3). Pure (only strih_platform above), no network.
strih_platform_refuse_windows_only_mode() {
  local host="${1:-}" what="${2:-this step}"
  [ "$(strih_platform "$host")" = "linux" ] || return 0
  echo "ERROR: ${what} is Windows-strih-only (win-* MCP / PowerShell), but strih ${host} is the Linux strih-lx (strih_platform=linux) -- it has no strih-lx port yet (issue 1317); refusing instead of emitting a Windows plan" >&2
  return 1
}

# strih_zero_loss_restart_preflight HOST -> returns 0 unless the opt-in #109 restart-survival mode
# (ZERO_LOSS_RESTART_GATE=1) is requested against a strih whose platform is linux, in which case it
# refuses (rc 1, named stderr). That mode plans the WINDOWS per-box decode (win-* MCP paste steps)
# and has no strih-lx port yet (issue 1317 part 3), so recording-e2e.sh calls this right after it
# resolves the strih -- BEFORE any rig mutation (the cambox burn deploys, the painter step) -- instead
# of failing after the whole preflight and a 360 s capture. Pure (env + strih_platform), no network.
strih_zero_loss_restart_preflight() {
  [ "${ZERO_LOSS_RESTART_GATE:-0}" = "1" ] || return 0
  strih_platform_refuse_windows_only_mode "${1:-}" "the restart-survival measurement mode (ZERO_LOSS_RESTART_GATE=1)"
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
  # issue 1351: the obs-websocket round-trip was the un-timeout-bounded strih-touching call on the
  # Linux path (the ssh probe above IS bounded; obs_phase2.py's own create_connection(timeout=10)
  # only caps a single socket op, not a wedged handshake). Bound it so a wedge becomes ws_ok=0 -> the
  # existing "obs-websocket on :4455 did not answer" named diagnosis, never a silent [0/8] hang.
  timeout "${STRIH_LX_WS_TIMEOUT:-20}" python3 "$here/obs_phase2.py" record --host "$host" --action status >/dev/null 2>&1 && ws_ok=1
  strih_linux_visibility_message "$sysout" "$ws_ok"
}
