#!/usr/bin/env bash
# launch-obs-genlock.sh — deterministic OBS (re)launch wrapper for the genlock boxes (#128/#257).
#
# #257 HARD-LOCKED the genlock build: the wall-clock render tick + ts-align are ALWAYS ON and the
# genlock latency is a BUILD CONST (3 ms, floor 3) — there is NO OBS_GENLOCK_* env any more, and the
# measurement burn is a per-source `genlock_burn` bool toggled over OBS WebSocket (NO relaunch, see
# scripts/obs_burn_filter.py + rig-mode.sh). So this wrapper no longer carries or verifies ANY env;
# the old #128 stale-env trap is structurally gone (there is no genlock env to lose). Its whole job
# is now: (force-kill →) clear crash sentinels → Start-Process obs64 cwd=bin\64bit → VERIFY the OBS
# log shows the genlock render tick ENABLED (the build-default proof) AND DistroAV loaded, failing
# LOUD otherwise (never a silent stock-OBS / wrong-build / broken-locale launch).
#
# #1057 VERIFY-AT-START: the emitted PLAN also directs a MANDATORY dev1-side post-launch burn
# sweep-off (scripts/obs_burn_filter.py sweep-off --host <box_ip>) — a saved genlock_burn=true
# reloads at OBS start and renders the QR burn onto the LIVE program, and the box has no on-box WS
# client to clear it, so it is forced OFF + loud-reported from dev1 (session-agnostic WS op, per
# win-ssh-vs-mcp) the moment the launch verifies. The emitted PowerShell PROGRAM still never touches
# the burn (no python/WS on the box); the sweep is a dev1-side PLAN step, not an on-box one.
#
# HOW THE PIECES FIT (same model as scripts/recording-verdict-on-stream.sh — historically "scp/ssh
# to Windows is DENIED on this rig, so the agent drives the win-* MCP"; #701 proved plain
# OpenSSH+password scp/ssh actually WORKS against strih (10.77.9.202) and stream (10.77.9.204)
# specifically with the targets.md creds, but this script stays a planner because launching a GUI
# app and reading its on-screen log state is exactly what the win-* MCP is FOR, not a workaround):
# this script is the PURE, testable PLANNER.
# Given the box + obs install dir, it PRINTS the exact PowerShell program to paste into the box's
# `win-strih` / `win-stream-snv` MCP `Shell`. It runs NO PowerShell itself and needs no Windows
# access — the Rust unit tests (tests/launch_obs_genlock.rs) source it and assert the emitted program
# is well-formed (clears sentinels, cwd=bin\64bit, log-verifies render tick ENABLED + DistroAV, fails
# loud, carries NO OBS_GENLOCK_*/OBS_BURN_* env). The emitted program is idempotent + self-verifying.
#
# Usage (planner mode — prints the PowerShell launch+verify program + the MCP plan):
#   scripts/launch-obs-genlock.sh --box strih            # uses the strih defaults
#   scripts/launch-obs-genlock.sh --box stream           # uses the stream defaults
#   scripts/launch-obs-genlock.sh --box strih --force    # force-kill a wedged obs64 first (obs-ops recovery)
#   scripts/launch-obs-genlock.sh --box strih \
#       --obs-dir 'C:\Program Files\obs-studio'          # override the OBS install dir
#
# Exit codes: 0 = plan printed, 2 = usage error. (The on-box PowerShell program's own exit code —
# 0 healthy genlock / non-zero fail-loud — is reported by the MCP Shell when the agent runs it.)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/ahk-watchdog.sh
# Sourcing (not executing) ahk-watchdog.sh: it defines ONE pure function, no top-level statements.
. "$HERE/lib/ahk-watchdog.sh"

# --- PURE functions (no network, no MCP, no Windows — unit-tested by sourcing this script) --------

# build_launch_program OBS_DIR FORCE [HAS_AHK] -> the full PowerShell program that (re)launches OBS
# env-free and then log-verifies + fails-loud. OBS_DIR is the OBS install root (its bin\64bit is the
# mandatory cwd). FORCE="1" inserts a documented force-kill of a wedged obs64 first (obs-ops recovery
# — this is a DEV rig, "kludne ho killni"); FORCE="0" aborts if obs64 is already running (relaunch
# deliberately, never double-launch). HAS_AHK (default "1") emits the #786 AHK stop-first/restart-last
# bracket around the redraw loop — pass "0" for a box with NO AutoHotkey64 auto-respawn watcher
# (stream; only strih runs NL_STARTUP.ahk, per .claude/skills/obs-ops "AHK on strih") so its program
# never carries a real AutoHotkey64 command (the #411 self-heal stream guard pins this). Pure string
# builder so a unit test can assert the program is well-formed without a Windows host. Heredoc body is
# a literal PowerShell here-string — bash-level interpolation is ONLY $OBS_DIR / the FORCE branch /
# the AHK bracket; everything else (PowerShell $vars) is literal.
build_launch_program() {
  local obs_dir="$1" force="$2" has_ahk="${3:-1}"

  # #786/#411 — the AHK bracket is emitted ONLY for a box that actually runs the AHK watcher.
  local ahk_decl ahk_stop_ps ahk_restart_ps
  if [ "$has_ahk" = "1" ]; then
    ahk_decl='$ahkStopped = $false'
    ahk_stop_ps=$(cat <<'PSAHK'
  # Stop AHK BEFORE killing obs64 (strih only; no-op elsewhere): NL_STARTUP.ahk respawns obs64 via
  # the BARE exe within seconds of the window vanishing, which would drop the shortcut params
  # (--enable-media-stream --verbose -> interkom "Permissions denied") AND race a double-launch.
  # Same stop-first/restart-last structure as the #411 self-heal. This snippet is embedded at every
  # obs64-kill site in the program (the --force kill_block AND the #786 redraw loop) — idempotent
  # (a no-op once AHK is already stopped), restarted exactly ONCE at the single shared point after
  # the whole launch+audio-verify sequence (issue 1272).
  if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) {
    Stop-Process -Name AutoHotkey64 -Force -ErrorAction SilentlyContinue
    $ahkStopped = $true
  }
PSAHK
)
    local ahk_relaunch_ps
    ahk_relaunch_ps="$(ahk_resolve_and_relaunch_ps)"
    # #867: the restart must be VERIFIED (Get-Process AutoHotkey64), never just "shortcut file
    # exists -> assume success" — a resolved-but-failed relaunch here means the redraw loop leaves
    # strih with NO respawn watcher, so this is fail-loud (Write-Error + a distinct exit), matching
    # how this script already fails loud on its other post-launch checks (obs64 didn't start, the
    # #786 audio buffering gate).
    ahk_restart_ps=$(cat <<PSAHK
if (\$ahkStopped) {
${ahk_relaunch_ps}
  if (\$ahkRelaunchVerified) {
    Write-Host "#786: AHK watchdog restarted via \$ahkRelaunchTarget."
  } else {
    Write-Error "#867 FAIL: AutoHotkey64 did not come back after relaunch (target=\$ahkRelaunchTarget) -- strih has NO respawn watcher; investigate before trusting this box."
    exit 9
  }
}
PSAHK
)
  else
    ahk_decl='# (no AutoHotkey64 watcher on this box -- the #786 AHK stop/restart bracket is a no-op)'
    ahk_stop_ps='  # AutoHotkey64: no-op (this box has no AHK auto-respawn watcher -- nothing to stop)'
    ahk_restart_ps='# AutoHotkey64 restart: no-op (no AHK watcher on this box)'
  fi
  # #978/#958 -- the SESSION-VISIBILITY GATE fragments. An obs64 launched via ssh+Invoke-CimMethod
  # lands in Windows SessionId=0 (invisible on the console) yet passes every OTHER check in this
  # program (log render tick, audio buffering) -- issue 958's real incident sat like this for
  # ~3.5h. has_ahk=1 (strih) also gates AutoHotkey64's session (a session-0 AHK re-spawns obs64
  # into session 0 forever); has_ahk=0 (stream, no AHK watcher) gets a documented no-op.
  #
  # Deliberately does NOT call scripts/lib/obs-session-visibility.sh's obs_session_visibility_*
  # functions (#977/#979's shared detector) -- that lib's shape (Write-Output field lines, parsed
  # back over ssh via bash-side sed) is built for the "ssh a probe, parse the reply" flow #977/#979
  # use; THIS gate runs inline, already inside the SAME open PowerShell session pasted into the
  # win-* MCP Shell, with no ssh round-trip at all. The two express the IDENTICAL criteria (obs64
  # count==1, SessionId==the derived active console session, MainWindowTitle required only when
  # this program's own session matches obs64's; AHK count==1, SessionId==the same derived active
  # session) independently by necessity of the different execution model -- if the criteria ever
  # change, update BOTH.
  local ahk_session_ps
  if [ "$has_ahk" = "1" ]; then
    ahk_session_ps=$(cat <<'PSAHKSESS'
$ahkSessProcs = @(Get-Process AutoHotkey64 -ErrorAction SilentlyContinue)
if ($ahkSessProcs.Count -ne 1) {
  Write-Error "#978 FAIL: expected exactly 1 AutoHotkey64 process on strih, found $($ahkSessProcs.Count) -- the respawn watcher is missing/duplicated (issue 958)."
  exit 8
}
if ($ahkSessProcs[0].SessionId -ne $activeSession) {
  Write-Error "#978 FAIL: AutoHotkey64 PID $($ahkSessProcs[0].Id) is in SessionId=$($ahkSessProcs[0].SessionId) (must be $activeSession, the active interactive session) -- a session-0 AHK re-spawns obs64 into session 0 forever (issue 958)."
  exit 8
}
Write-Host "SESSION OK: AutoHotkey64 PID $($ahkSessProcs[0].Id) SessionId=$activeSession"
PSAHKSESS
)
  else
    ahk_session_ps='# AutoHotkey64 session check: no-op (this box has no AHK auto-respawn watcher)'
  fi
  local bin64="${obs_dir}\\bin\\64bit"
  local exe="${bin64}\\obs64.exe"
  # Escape for the PowerShell SINGLE-quoted strings below: a literal ' is doubled to '' (the
  # PowerShell single-quote escape), so an OBS dir containing a quote can't break out of the
  # '...' string. The default 'C:\Program Files\obs-studio' has none; this hardens an override.
  local obs_dir_ps="${obs_dir//\'/\'\'}"
  local bin64_ps="${bin64//\'/\'\'}"
  local exe_ps="${exe//\'/\'\'}"

  # The kill branch (only when --force) — documented obs-ops recovery for a wedged OBS.
  local kill_block=""
  if [ "$force" = "1" ]; then
    local kill_body
    kill_body=$(cat <<'PSKILL'
# --force: documented obs-ops recovery -- force-kill a wedged obs64 before relaunch (DEV rig).
Get-Process obs64 -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.Id -Force }
Start-Sleep -Seconds 2
PSKILL
)
    if [ "$has_ahk" = "1" ]; then
      # issue 1272: stop AHK BEFORE killing obs64 (strih only). scripts/deploy-genlock-fleet.sh's
      # own step 8 already restarted+verified AHK before this planner even runs, and NL_STARTUP.ahk
      # respawns a BARE obs64 within seconds of the kill if left running -- racing this wrapper's
      # own .lnk relaunch into a duplicate obs64 that fails the session-visibility gate's "expected
      # exactly 1 obs64 process" check (the live 2026-09-02 incident this fixes). AHK is restarted
      # exactly once, later, after the whole launch+audio-verify sequence -- see the single
      # ${ahk_restart_ps} emission point below (never here, and never per-kill).
      kill_block="${ahk_stop_ps}
${kill_body}"
    else
      kill_block="${kill_body}"
    fi
  else
    kill_block=$(cat <<'PSNOKILL'
# No --force: refuse to double-launch a running obs64 (relaunch deliberately; use --force for a wedged one).
if (Get-Process obs64 -ErrorAction SilentlyContinue) {
  Write-Error "obs64 already running -- relaunch deliberately (--force to recover a wedged one)."; exit 3
}
PSNOKILL
)
  fi

  cat <<PS
# ===== #257 deterministic genlock OBS (re)launch + verify (paste into the box's win-* MCP Shell) =====
# The genlock build is HARD-LOCKED: render tick + ts-align ALWAYS ON, latency = 3 ms build const, NO
# OBS_GENLOCK_*/OBS_BURN_* env. The measurement burn is a per-source genlock_burn bool over WebSocket
# (scripts/obs_burn_filter.py), toggled WITHOUT a relaunch -- this wrapper never touches it.
\$ErrorActionPreference = 'Stop'

# issue 1272: \$ahkStopped is declared ONCE, here -- so both the --force kill below (kill_block,
# wired in via the has_ahk check above) and the #786 redraw loop further down share the SAME flag
# across the whole program. Never re-declare it later (a second "= \$false" would silently wipe out
# the --force branch's own stop before the single restart point below runs).
${ahk_decl}

${kill_block}

# (1) Clear stale crash sentinels so OBS does not pop the "Crash Detected" modal and hang headless.
Remove-Item "\$env:APPDATA\\obs-studio\\.sentinel\\*" -Force -ErrorAction SilentlyContinue

# (2) Launch OBS via the box's OWN Start-Menu SHORTCUT ("OBS Studio.lnk") -- the STANDARD launch
#     path, so the box's own shortcut parameters are always honored (user directive 2026-07-15:
#     "obs sa ma spustat standartne cez zastupcu", the params are PER-BOX -- e.g. strih's shortcut
#     carries --enable-media-stream --verbose, which the interkom VDO.ninja Browser source needs
#     for camera/mic access; a bare exe launch dropped them and rendered a "Permissions denied"
#     box on program output, user-caught live). Fallback to the bare exe ONLY if the shortcut is
#     genuinely absent (fail-open recovery beats no OBS at all -- the verify below still gates).
#     NB on strih: D:\\_APPS\\NL_STARTUP.ahk auto-respawns obs64 from this same dir, but it won't
#     double-launch once one is running, so this Start-Process wins; the log verify below fails loud
#     on a non-genlock build regardless. See obs-ops skill.
\$obsDir = '${obs_dir_ps}'
\$exe    = '${exe_ps}'
\$lnk    = "\$env:ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\OBS Studio.lnk"
if (-not (Test-Path \$exe)) { Write-Error "obs64 not found at \$exe"; exit 5 }
if (Test-Path \$lnk) {
  Start-Process -FilePath \$lnk
} else {
  Write-Warning "OBS Studio.lnk shortcut not found -- falling back to a bare exe launch (box-specific shortcut params will be MISSING; fix the shortcut)."
  Start-Process -FilePath \$exe -WorkingDirectory '${bin64_ps}'
}

# (3) Wait for obs64 to come up and write its log (genlock lines are emitted at launch).
\$proc = \$null
for (\$i = 0; \$i -lt 30; \$i++) {
  Start-Sleep -Seconds 1
  \$proc = Get-Process obs64 -ErrorAction SilentlyContinue | Select-Object -First 1
  if (\$proc -and \$proc.WorkingSet64 -gt 100MB) { break }
}
if (-not \$proc) { Write-Error "obs64 did not start"; exit 6 }
Start-Sleep -Seconds 3

# (3b) #786 AUDIO-BUFFERING LAUNCH-GATE (hotfix; the OBS-level fix is #786's real deliverable).
#     Some launches hit an ASIO init race ('ASIO Input Capture' on VB-Matrix VASIO-8 floods stale
#     audio in the ~1s window before Dante Virtual Soundcard finishes init) -> libobs ratchets its
#     GLOBAL audio buffering to the 960 ms MAX within the first seconds, and that buffer NEVER
#     shrinks until OBS restarts -> the whole session's A/V sync is off by ~0.9 s (live incident
#     2026-07-15: operator's 900 ms genlock latency needed ~2000 ms to compensate). A bad draw is
#     fully decided in the first seconds and is visible in the fresh log, so gate + redraw here.
#     THRESHOLD = 100 ms: the box's own measured STANDARD is 64 ms (every clean retained launch
#     2026-07-11..15; a few days show 85 ms), so 100 = norm + small headroom. The user's rule:
#     OBS must never come up with more than the standard draw.
#     NB fail-open asymmetry: an empty/absent log reads as peak 0 = "AUDIO OK" here, but step (4)
#     below fails CLOSED (exit 1) on the same missing log, so a silent overall pass is impossible.
\$logDir = "\$env:APPDATA\\obs-studio\\logs"
function Get-BufDraw {
  \$l = Get-ChildItem \$logDir -Filter *.txt | Sort-Object LastWriteTime -Descending | Select-Object -First 1
  \$t = if (\$l) { Get-Content \$l.FullName -Raw } else { "" }
  \$pk = 0
  foreach (\$m in [regex]::Matches([string]\$t, 'total audio buffering is now (\\d+) milliseconds')) {
    \$v = [int]\$m.Groups[1].Value; if (\$v -gt \$pk) { \$pk = \$v }
  }
  [pscustomobject]@{ Peak = \$pk; Maxed = ([string]\$t -match 'Max audio buffering reached') }
}
# If the box's shortcut is the #786 GUARDED launcher (obs-guarded-launch.ps1 -- the retargeted
# "OBS Studio.lnk"), the ON-BOX script owns the kill+redraw. Running a SECOND redraw loop here
# would race it (double kill -> possible double obs64). So: guarded shortcut -> VERIFY-ONLY wait
# (up to 180 s for the on-box redraws to settle clean), else -> this wrapper's own redraw loop.
\$guardedLnk = \$false
if (Test-Path \$lnk) {
  \$lnkArgs = (New-Object -ComObject WScript.Shell).CreateShortcut(\$lnk).Arguments
  if ([string]\$lnkArgs -match 'obs-guarded-launch\\.ps1') { \$guardedLnk = \$true }
}
if (\$guardedLnk) {
  Write-Host "#786: guarded shortcut detected -- the on-box launcher owns the redraw; verify-only here."
  \$bufOk = \$false
  \$deadline = (Get-Date).AddSeconds(180)
  while ((Get-Date) -lt \$deadline) {
    Start-Sleep -Seconds 10
    \$d = Get-BufDraw
    if (\$d.Peak -le 100 -and -not \$d.Maxed) { \$bufOk = \$true; break }
  }
  \$d = Get-BufDraw
  if (\$bufOk -and \$d.Peak -le 100 -and -not \$d.Maxed) {
    Write-Host "AUDIO OK: buffering peak \$(\$d.Peak) ms -- clean draw via the on-box guard (#786)."
  } else {
    Write-Error "#786 FAIL: audio buffering still \$(\$d.Peak) ms (maxed=\$(\$d.Maxed)) after the on-box guard's redraws -- A/V sync would be off. Investigate the ASIO devices (VB-Matrix VASIO-8 / Dante VSC) before trusting this box."
    exit 7
  }
} else {
\$maxLaunchAttempts = 3
for (\$attempt = 1; \$attempt -le \$maxLaunchAttempts; \$attempt++) {
  Start-Sleep -Seconds 10   # ASIO starts right after module load; a bad burst completes within ~5 s
  \$d = Get-BufDraw
  \$bufPeak = \$d.Peak
  \$bufMaxed = \$d.Maxed
  if ((-not \$bufMaxed) -and \$bufPeak -le 100) {
    Write-Host "AUDIO OK: buffering peak \${bufPeak} ms -- clean ASIO launch draw (#786 gate)."
    break
  }
  if (\$attempt -eq \$maxLaunchAttempts) {
    Write-Error "#786 FAIL: audio buffering peak \${bufPeak} ms (maxed=\$bufMaxed) after \$maxLaunchAttempts launches -- A/V sync would be off by ~\${bufPeak} ms. Investigate the ASIO devices (VB-Matrix VASIO-8 / Dante VSC) before trusting this box."
    exit 7
  }
  Write-Warning "#786: bad ASIO launch draw (buffering peak \${bufPeak} ms, maxed=\$bufMaxed) -- killing + relaunching OBS (attempt \$(\$attempt + 1)/\$maxLaunchAttempts)."
${ahk_stop_ps}
  Stop-Process -Name obs64 -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 4
  Remove-Item "\$env:APPDATA\\obs-studio\\.sentinel\\*" -Force -ErrorAction SilentlyContinue
  if (Test-Path \$lnk) {
    Start-Process -FilePath \$lnk
  } else {
    Start-Process -FilePath \$exe -WorkingDirectory '${bin64_ps}'
  }
  \$proc = \$null
  for (\$i = 0; \$i -lt 30; \$i++) {
    Start-Sleep -Seconds 1
    \$proc = Get-Process obs64 -ErrorAction SilentlyContinue | Select-Object -First 1
    if (\$proc -and \$proc.WorkingSet64 -gt 100MB) { break }
  }
  if (-not \$proc) { Write-Error "obs64 did not start on #786 relaunch"; exit 6 }
  Start-Sleep -Seconds 3
}
}

# issue 1272: restart AHK exactly ONCE here -- after the launch + #786 audio-verify sequence has
# fully settled (whichever branch above ran: the guarded-launcher verify-only wait, or this
# wrapper's own redraw loop) -- and BEFORE the (3c) session-visibility gate below, which itself
# asserts AHK's own SessionId and would otherwise find it missing.
${ahk_restart_ps}

# (3c) #978 SESSION-VISIBILITY GATE -- an obs64 launched via ssh+Invoke-CimMethod into Windows
#     session 0 is fully healthy on every OTHER check above (log render tick, audio buffering) yet
#     completely invisible to the operator (issue 958). Re-query FRESH (never trust a possibly-
#     stale \$proc from the guarded-launcher's own redraw) and fail LOUD, non-zero, naming exactly
#     what was found. The ACTIVE interactive session is derived from explorer.exe's SessionId
#     (never hardcoded) -- and the MainWindowTitle assertion is CONTEXT-GATED: this program
#     normally pastes into the win-* MCP Shell (session 1, same session as OBS), where the title
#     check stays fully REQUIRED (unchanged strictness); the #859 no-MCP CIM-breakaway fallback can
#     also run this program over ssh (a DIFFERENT session) -- there the title is structurally
#     UNREADABLE cross-session (EnumWindows only sees the calling process's own window station), so
#     the title check is SKIPPED (not failed) there and SessionId+count alone carry the gate.
\$activeSessProcs = @(Get-Process explorer -ErrorAction SilentlyContinue)
if (\$activeSessProcs.Count -lt 1) {
  Write-Error "#978 FAIL: no explorer.exe process found -- cannot determine the active interactive console session on this box."
  exit 8
}
\$activeSession = \$activeSessProcs[0].SessionId
\$ownSession = (Get-Process -Id \$PID).SessionId
\$sessObsProcs = @(Get-Process obs64 -ErrorAction SilentlyContinue)
if (\$sessObsProcs.Count -ne 1) {
  Write-Error "#978 FAIL: expected exactly 1 obs64 process, found \$(\$sessObsProcs.Count) -- investigate before trusting this box."
  exit 8
}
\$sessProc = \$sessObsProcs[0]
if (\$sessProc.SessionId -ne \$activeSession) {
  Write-Error "#978 FAIL: obs64 PID \$(\$sessProc.Id) is in SessionId=\$(\$sessProc.SessionId) (must be \$activeSession, the active interactive session), MainWindowTitle='\$(\$sessProc.MainWindowTitle)' -- OBS is INVISIBLE to the operator (issue 958). Relaunch via the win-* MCP Shell (session 1), never ssh+Invoke-CimMethod."
  exit 8
}
if (\$ownSession -eq \$sessProc.SessionId) {
  if ([string]::IsNullOrEmpty(\$sessProc.MainWindowTitle)) {
    Write-Error "#978 FAIL: obs64 PID \$(\$sessProc.Id) SessionId=\$activeSession but MainWindowTitle is EMPTY -- no visible window (issue 958). Investigate before trusting this box."
    exit 8
  }
  Write-Host "SESSION OK: obs64 PID \$(\$sessProc.Id) SessionId=\$activeSession MainWindowTitle='\$(\$sessProc.MainWindowTitle)'"
} else {
  Write-Host "SESSION OK: obs64 PID \$(\$sessProc.Id) SessionId=\$activeSession (title check skipped -- this program is running cross-session, own SessionId=\$ownSession)"
}
${ahk_session_ps}

# (4) VERIFY the fresh OBS log shows the genlock render tick ENABLED (the #257 build-default proof --
#     same line drift-guard.sh genlock_capability_from_log + the rig validation key on) AND DistroAV
#     loaded. The log is the AUTHORITATIVE runtime signal; a stock OBS / wrong build emits no genlock
#     line. Anything short of BOTH -> non-zero exit (fail loud, never a silent half-genlocked box).
\$log = Get-ChildItem \$logDir -Filter *.txt | Sort-Object LastWriteTime -Descending | Select-Object -First 1
\$logText = if (\$log) { Get-Content \$log.FullName -Raw } else { "" }
\$tickOk     = \$logText -match 'genlock:.*render tick ENABLED'
\$distroavOk = \$logText -match '(?i)distroav'
if (\$tickOk)     { Write-Host "LOG OK: genlock render tick ENABLED (build default, #257)" } else { Write-Error "#257 LOG: 'render tick ENABLED' NOT found in \$(\$log.Name) -- NOT the genlock build (stock/wrong OBS?)." }
if (\$distroavOk) { Write-Host "LOG OK: DistroAV loaded" }                                  else { Write-Warning "#257 LOG: no DistroAV line yet (may log lazily on first NDI activation)." }

# (5) FINAL VERDICT -- fail loud unless the render tick is ENABLED (the genlock build proof). DistroAV
#     is a warning only (it logs lazily on first NDI source activation).
if (\$tickOk) {
  Write-Host "#257 OK: obs64 PID \$(\$proc.Id) launched, genlock render tick ENABLED (no env needed)."
  exit 0
} else {
  Write-Error "#257 FAIL: genlock render tick NOT enabled -- do NOT trust this box (wrong OBS build?)."
  exit 1
}
PS
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------------

usage() {
  cat <<'EOF'
launch-obs-genlock.sh — deterministic OBS (re)launch wrapper for the genlock boxes (#128/#257).

Prints the exact PowerShell program to paste into the box's win-* MCP Shell. The genlock build is
hard-locked (render tick + ts-align always on, latency 3 ms build const, NO OBS_GENLOCK_*/OBS_BURN_*
env). The program clears crash sentinels, launches obs64 cwd=bin\64bit, then log-verifies the genlock
render tick ENABLED + DistroAV loaded and FAILS LOUD otherwise.

Toggling the measurement burn does NOT relaunch OBS — it is a per-source genlock_burn bool over
OBS WebSocket: scripts/obs_burn_filter.py add|remove (driven by rig-mode.sh test|event).

Usage:
  scripts/launch-obs-genlock.sh --box strih|stream [--force] [--obs-dir 'C:\Program Files\obs-studio']
  scripts/launch-obs-genlock.sh --help

  --box     strih (win-strih, 10.77.9.202) or stream (win-stream-snv, 10.77.9.204) — selects the MCP.
  --force   force-kill a wedged obs64 first (documented obs-ops recovery; DEV rig).
  --obs-dir override the OBS install root (default 'C:\Program Files\obs-studio'; its bin\64bit is cwd).

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local box="" force="0"
  local obs_dir='C:\Program Files\obs-studio'
  # `need_val FLAG` guards a value-taking flag BEFORE shift 2: a trailing flag with no value must be
  # a clean usage error (exit 2 + message), not a silent `shift 2` abort (exit 1) under set -e.
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)     need_val "$@"; box="$2"; shift 2 ;;
      --force)   force="1"; shift ;;
      --obs-dir) need_val "$@"; obs_dir="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  # has_ahk: only strih runs the NL_STARTUP.ahk auto-respawn watcher (obs-ops "AHK on strih") —
  # stream's program must not carry a real AutoHotkey64 command (#411 self-heal guard pins this).
  local mcp box_ip has_ahk
  case "$box" in
    strih)  mcp="win-strih";       box_ip="10.77.9.202"; has_ahk=1 ;;
    stream) mcp="win-stream-snv";  box_ip="10.77.9.204"; has_ahk=0 ;;
    *) echo "ERROR: --box must be 'strih' or 'stream' (got '${box}')" >&2; usage >&2; exit 2 ;;
  esac

  local PROGRAM
  PROGRAM="$(build_launch_program "$obs_dir" "$force" "$has_ahk")"

  cat <<PLAN
# ===== #257 genlock OBS (re)launch plan — box=${box} (${mcp}, ${box_ip}) =====
# Run the program below via the ${mcp} MCP Shell — a GUI relaunch + on-screen log verification is
# exactly what the win-* MCP is for (#701: plain scp/ssh DOES work against strih/stream with the
# targets.md creds, but that doesn't help drive/verify a GUI app).
#
# STEP 1: paste the following PowerShell program into:  ${mcp} Shell  -- set the Shell call's
#         timeout to >= 240 s: the #786 audio gate's worst case is 3 draws + 2 relaunches
#         (~100 s beyond a clean launch), and a truncated run never prints its verdict
#         (it clears crash sentinels, launches obs64 cwd=bin\\64bit, then log-verifies the genlock
#          render tick ENABLED + DistroAV loaded, failing LOUD otherwise — NO env carried)
# ----------------------------------------------------------------------------------------------------
${PROGRAM}
# ----------------------------------------------------------------------------------------------------
# STEP 2: the program EXITS 0 only when the OBS log shows 'render tick ENABLED' (the #257 build proof).
#         A non-zero exit means it is NOT the genlock build — do NOT trust the box; check the deploy.
# STEP 3 (#1057) — VERIFY-AT-START (MANDATORY): a saved genlock_burn=true is reloaded at every OBS
#         start and RENDERS the QR measurement burn onto the LIVE program output (the gate's [0/8]
#         sweep only clears it DURING a gate run, never on a restart outside one). The Windows box
#         has no on-box python/OBS-WebSocket client and obs_burn_filter.py is not deployed there, so
#         force burns OFF from DEV1 (a session-agnostic WS op, per win-ssh-vs-mcp) the moment STEP 2
#         confirms OBS is up — run ON DEV1, NOT in the ${mcp} MCP Shell:
#           python3 scripts/obs_burn_filter.py sweep-off --host ${box_ip}
#         It enumerates EVERY ndi_source input from OBS reality (never a static list), forces any
#         genlock_burn OFF, read-back verifies, and reports LOUDLY — exit 0 = clean; non-zero (a
#         still-ON input or an enumeration failure = fail-closed) means a burn LEAKED: do NOT trust
#         this box on live program until it clears. (A measurement burn is NEVER legitimate operator
#         state, so forcing it OFF at start never fights the operator — unlike the per-source
#         latency pins, whose verify-at-start is REPORT-ONLY — see STEP 3b below.)
# STEP 3b (#1061) — LATENCY-PIN VERIFY-AT-START (REPORT-ONLY): per-source genlock_latency_ms_src
#         ALSO persists to the scene collection and RELOADS at OBS start, so an unjustified restart
#         revert (#866/#707: strih came back at pins it had been reverted FROM as unjustified) can
#         bring back per-source latencies the operator never agreed to. UNLIKE the burn, per-source
#         latency IS the operator's A/V-align domain, so this step only REPORTS drift against the
#         committed agreed-pins baseline (scripts/latency-pins-baseline.json) — it NEVER overwrites
#         (forcing would fight the operator). Run ON DEV1 (read-only WS, session-agnostic, per
#         win-ssh-vs-mcp), NOT in the ${mcp} MCP Shell:
#           python3 scripts/latency_pins_verify.py --box ${box} --host ${box_ip}
#         exit 0 = every agreed pin on baseline; exit 1 = drift (LOUD, names box+input+got+want — an
#         ADVISORY signal, NOT a launch abort); exit 2 = a WS read failure (fail-closed). If the
#         drift is a legitimate re-tune, record it by updating the baseline in a PR; if it is an
#         unjustified restart revert, re-apply the agreed pins. (REPORT-ONLY — never overwrites.)
# STEP 4: to toggle the measurement burn (TEST mode), DON'T relaunch — flip the per-source bool over
#         WebSocket:  scripts/obs_burn_filter.py add|remove --host ${box_ip} --input "<NDI input>"
#         (or use scripts/rig-mode.sh test|event, which does it for both boxes).
# STEP 5 (#674): once STEP 2 confirms the relaunch succeeded, mark it on imag-nb's own journald so
#         a future imag judder report can be time-correlated against this restart:
#         scripts/mark-imag-restart.sh --box ${box} --reason "<why you relaunched>"
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
