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
#
# ACTIVE_SESSION/OWN_SESSION (issue 958 follow-up, supervisor root-cause on issue 958): a probe run
# over win_ssh_run (ssh from dev1) lands in Windows session 0, while obs64/AHK live in the
# INTERACTIVE session -- .NET's MainWindowTitle (EnumWindows) only sees the calling process's own
# window station, so it is ALWAYS empty cross-session, even on a perfectly healthy box. The parser
# below uses ACTIVE_SESSION (derived from explorer.exe, never hardcoded) to judge SessionId
# mismatches (readable cross-session, catches the real issue-958 signature) and OWN_SESSION (this
# probing shell's own SessionId) to decide whether the title check can be trusted at all.
obs_session_visibility_probe_ps() {
  local has_ahk="${1:-1}"
  cat <<'PS'
$explorerProcs = @(Get-Process explorer -ErrorAction SilentlyContinue)
if ($explorerProcs.Count -ge 1) {
  Write-Output ("ACTIVE_SESSION=" + $explorerProcs[0].SessionId)
} else {
  Write-Output "ACTIVE_SESSION="
}
Write-Output ("OWN_SESSION=" + (Get-Process -Id $PID).SessionId)
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
# string = fully visible (obs64 exactly 1, SessionId==the active console session; AHK too when
# has_ahk=1). Any other case returns a non-empty, human-readable diagnosis naming exactly what was
# found -- including an EMPTY probe_output (an ssh/connectivity failure), which is deliberately
# treated as a real diagnosis here too (never a silent pass) -- CALLERS decide what an empty probe
# means for them (#977's gate fails loud on it; #979's watchdog explicitly short-circuits BEFORE
# calling this function on empty output, mirroring #882's own "connectivity is a different
# watchdog's job").
#
# CONTEXT-GATED MainWindowTitle (issue 958 follow-up): the SessionId comparison (against
# ACTIVE_SESSION, the console's real interactive session, never hardcoded) is the ONLY
# cross-session-valid FAIL criterion -- it's what actually reproduces the issue-958 incident
# (ssh-launched OBS landing in session 0). The MainWindowTitle check is enforced ONLY when
# OWN_SESSION (this probing shell's own session) equals the target's SessionId -- i.e. a same-
# session read (pasted into the win-* MCP Shell). A cross-session read (OWN_SESSION differs, e.g.
# every win_ssh_run call from dev1) SKIPS the title check instead of failing it: Windows makes the
# title structurally unreadable cross-session (EnumWindows only sees the caller's own window
# station), so a genuinely healthy box would otherwise be permanently misreported INVISIBLE.
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
  local active_session own_session obs_count obs_session obs_title
  active_session="$(printf '%s\n' "$out" | sed -n 's/^ACTIVE_SESSION=//p' | tail -1)"
  own_session="$(printf '%s\n' "$out" | sed -n 's/^OWN_SESSION=//p' | tail -1)"
  obs_count="$(printf '%s\n' "$out" | sed -n 's/^OBS_COUNT=//p' | tail -1)"
  obs_session="$(printf '%s\n' "$out" | sed -n 's/^OBS_SESSION=//p' | tail -1)"
  obs_title="$(printf '%s\n' "$out" | sed -n 's/^OBS_TITLE=//p' | tail -1)"
  if [ "${obs_count:-0}" != "1" ]; then
    printf 'obs64 process count=%s (want exactly 1)' "${obs_count:-0}"
    return 0
  fi
  if [ -z "${active_session:-}" ]; then
    printf 'no explorer.exe process found -- cannot determine the active interactive console session on this box (issue 958)'
    return 0
  fi
  if [ "${obs_session:-}" != "${active_session}" ]; then
    printf 'obs64 SessionId=%s (want %s, the active interactive session) -- OBS is healthy but INVISIBLE on the operator console (issue 958)' "${obs_session:-unknown}" "$active_session"
    return 0
  fi
  if [ "${own_session:-}" = "${obs_session:-}" ] && [ -z "${obs_title:-}" ]; then
    printf 'obs64 SessionId=%s but MainWindowTitle is EMPTY -- no visible window (issue 958)' "$obs_session"
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
    if [ "${ahk_session:-}" != "${active_session}" ]; then
      printf 'AutoHotkey64 SessionId=%s on strih (want %s, the active interactive session) -- a session-0 AHK re-spawns obs64 into session 0 forever (issue 958)' "${ahk_session:-unknown}" "$active_session"
      return 0
    fi
  fi
  printf ''
}
