#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function lib (never executed directly) -- must NOT set -e,
# which would propagate into a sourcing caller's shell (e.g. #979's watchdog, which deliberately
# uses `set -uo pipefail`, not -e, so it survives every per-pass failure and keeps polling on the
# next timer tick -- same convention as scripts/obs-liveness-watchdog.sh /
# scripts/imag-obs-alert-watchdog.sh).
#
# scripts/lib/obs-session-visibility.sh -- #977/#978/#979: obs64/AHK Windows-session-visibility
# probe + pure message parser.
#
# WHY (issue 958): an obs64 relaunched via ssh+Invoke-CimMethod lands in Windows SessionId=0 --
# fully healthy on every OTHER check (OBS WebSocket, NDI, genlock log, recording) yet completely
# invisible to the operator on the console. The real incident sat like this for ~3.5h before the
# user found it manually.
#
# Shape mirrors the EXISTING scripts/lib/imag-obs-reachability.sh (#882): a probe-cmd builder that
# prints REMOTE text to embed via $(...), plus a PURE message parser returning "" when healthy or
# a human-readable diagnosis otherwise. This is the ONE detector both #977's per-PR E2E gate
# preflight and #979's continuous dev1 watchdog reuse -- never a second/third reimplementation of
# the same signal.

# obs_session_visibility_probe_ps <has_ahk 0|1> -> PowerShell probe TEXT (embed via $(...) into
# win_ssh_run's 4th arg, scripts/lib/win-ssh-exec.sh #703). Read-only: Get-Process only, no writes,
# no relaunch. has_ahk=1 (strih) additionally probes AutoHotkey64; has_ahk=0 (stream) probes obs64
# only -- stream has no AHK auto-respawn watcher (.claude/skills/obs-ops "AHK on strih").
obs_session_visibility_probe_ps() {
  local has_ahk="${1:-1}"
  cat <<'PS'
$obs = @(Get-Process obs64 -ErrorAction SilentlyContinue)
Write-Output ("OBS_COUNT=" + $obs.Count)
if ($obs.Count -ge 1) {
  Write-Output ("OBS_SESSION=" + $obs[0].SessionId)
  Write-Output ("OBS_TITLE=" + $obs[0].MainWindowTitle)
}
PS
  if [ "$has_ahk" = "1" ]; then
    cat <<'PSAHK'
$ahk = @(Get-Process AutoHotkey64 -ErrorAction SilentlyContinue)
Write-Output ("AHK_COUNT=" + $ahk.Count)
if ($ahk.Count -ge 1) { Write-Output ("AHK_SESSION=" + $ahk[0].SessionId) }
PSAHK
  fi
}

# obs_session_visibility_message PROBE_OUTPUT HAS_AHK -> pure parser (no network, no ssh). Empty
# string = fully visible (obs64 exactly 1, SessionId=1, non-empty window; AHK too when has_ahk=1).
# Any other case returns a non-empty, human-readable diagnosis naming exactly what was found --
# including an EMPTY probe_output (an ssh/connectivity failure), which is deliberately treated as
# a real diagnosis here too (never a silent pass) -- CALLERS decide what an empty probe means for
# them (#977's gate fails loud on it; #979's watchdog explicitly short-circuits BEFORE calling this
# function on empty output, mirroring #882's own "connectivity is a different watchdog's job").
obs_session_visibility_message() {
  local out="$1" has_ahk="${2:-1}"
  if [ -z "$out" ]; then
    printf 'no probe output (ssh/connectivity failure -- box unreachable, or the command did not run)'
    return 0
  fi
  # win_ssh_run's PowerShell probe returns Windows CRLF line endings -- strip the trailing \r from
  # EVERY line before parsing, or a genuinely healthy box (count=1) fails the sed-captured value
  # "1\r" != "1" comparison and gets misreported INVISIBLE (real-hardware regression, found live).
  out="${out//$'\r'/}"
  local obs_count obs_session obs_title
  obs_count="$(printf '%s\n' "$out" | sed -n 's/^OBS_COUNT=//p' | tail -1)"
  obs_session="$(printf '%s\n' "$out" | sed -n 's/^OBS_SESSION=//p' | tail -1)"
  obs_title="$(printf '%s\n' "$out" | sed -n 's/^OBS_TITLE=//p' | tail -1)"
  if [ "${obs_count:-0}" != "1" ]; then
    printf 'obs64 process count=%s (want exactly 1)' "${obs_count:-0}"
    return 0
  fi
  if [ "${obs_session:-}" != "1" ]; then
    printf 'obs64 SessionId=%s (want 1) -- OBS is healthy but INVISIBLE on the operator console (issue 958)' "${obs_session:-unknown}"
    return 0
  fi
  if [ -z "${obs_title:-}" ]; then
    printf 'obs64 SessionId=1 but MainWindowTitle is EMPTY -- no visible window (issue 958)'
    return 0
  fi
  if [ "$has_ahk" = "1" ]; then
    local ahk_count ahk_session
    ahk_count="$(printf '%s\n' "$out" | sed -n 's/^AHK_COUNT=//p' | tail -1)"
    ahk_session="$(printf '%s\n' "$out" | sed -n 's/^AHK_SESSION=//p' | tail -1)"
    if [ "${ahk_count:-0}" != "1" ]; then
      printf 'AutoHotkey64 count=%s on strih (want exactly 1) -- the respawn watcher is missing/duplicated' "${ahk_count:-0}"
      return 0
    fi
    if [ "${ahk_session:-}" != "1" ]; then
      printf 'AutoHotkey64 SessionId=%s on strih (want 1) -- a session-0 AHK re-spawns obs64 into session 0 forever (issue 958)' "${ahk_session:-unknown}"
      return 0
    fi
  fi
  printf ''
}
