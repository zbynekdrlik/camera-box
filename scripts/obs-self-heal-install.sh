#!/usr/bin/env bash
# obs-self-heal-install.sh — #411 Windows-local unattended self-heal for the #391 OBS liveness
# watchdog. Recovery-decision policy: scripts/lib (none — the policy is scripts/../src/obs_self_heal.rs,
# unit-tested Tier-0) + this file's PURE planners for the Windows-side mechanism.
#
# WHY (#411): #391 shipped DETECT + ALERT only — a dev1 systemd timer polls OBS WebSocket
# `GetStats` and fires a Discord alert once a wedge is confirmed, but RECOVERY still needs an
# agent to see the alert and run `scripts/launch-obs-genlock.sh --force` via the win-* MCP. That
# fails the exact overnight/unattended case the watchdog exists to cover (the founding incident:
# stream OBS wedged for ~25 HOURS and nothing acted on it). This script emits the Windows-LOCAL
# mechanism that closes that gap: a per-box Task Scheduler job (~2 min cadence) that runs entirely
# on the box itself — no ssh (denied), no MCP (agent-only), no agent session required.
#
# HOW THE PIECES FIT (mirrors scripts/launch-obs-genlock.sh's own model exactly):
#   - `camera_box::obs_self_heal` (src/obs_self_heal.rs) is the canonical, Tier-0 unit-tested
#     RECOVERY-decision policy (confirm-threshold / throttle / lock / AHK-race-safe step order /
#     post-recovery verify rule). This script's `build_recovery_script` is a PowerShell
#     re-expression of that SAME policy (PowerShell cannot call Rust directly), kept in lock-step
#     by `tests/obs_self_heal_install.rs` asserting the constants + step order match.
#   - The WEDGE VERDICT itself is NEVER reimplemented: the emitted recovery script pipes a LOCAL
#     sample (Get-Process obs64: count / Responding / CPU%, no OBS WebSocket round-trip needed) to
#     the EXISTING `obs-watchdog-gate.exe` binary, which runs the EXACT SAME
#     `camera_box::obs_watchdog::classify` the #391 dev1 alert watchdog uses remotely.
#   - The kill+relaunch step REUSES `launch-obs-genlock.sh`'s `build_launch_program` VERBATIM (this
#     script sources that file) — there is ONE idempotent, self-verifying obs64 launch path in this
#     whole repo, never a second hand-rolled one.
#   - scp/ssh to Windows is DENIED — this script is the PURE PLANNER an agent/supervisor pastes
#     into the box's `win-strih` / `win-stream-snv` MCP `Shell` to WRITE the recovery script +
#     REGISTER the Task Scheduler job. It runs NO PowerShell itself and needs no Windows access —
#     `tests/obs_self_heal_install.rs` sources it and asserts the emitted PowerShell/XML is well
#     formed, exactly like `tests/launch_obs_genlock.rs` does for `launch-obs-genlock.sh`.
#
# SHIPS DISABLED (the emitted Task Scheduler XML has `<Enabled>false</Enabled>`) — the supervisor
# installs it, live-verifies on the real rig (a genuine wedge auto-recovers with no agent open; a
# healthy box never false-force-kills; AHK never double-launches), and only then enables the task.
#
# Usage (planner mode — prints the full install plan for the box):
#   scripts/obs-self-heal-install.sh --box strih
#   scripts/obs-self-heal-install.sh --box stream
#   scripts/obs-self-heal-install.sh --box strih --obs-dir 'C:\Program Files\obs-studio' \
#       --confirm-threshold 2 --min-interval-s 600 --stale-lock-s 900 --interval-min 2
#
# Exit codes: 0 = plan printed, 2 = usage error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/launch-obs-genlock.sh
# Sourcing (not executing) launch-obs-genlock.sh: its own source-guard stops right after defining
# build_launch_program, so this ONLY pulls in that pure function — nothing runs.
. "$HERE/launch-obs-genlock.sh"

# --- PURE functions (no network, no MCP, no Windows — unit-tested by sourcing this script) --------

# build_recovery_script BOX OBS_DIR TARGET_FPS CONFIRM_THRESHOLD MIN_INTERVAL_S STALE_LOCK_S
#   -> the full PowerShell recovery script Task Scheduler runs every ~2 min on BOX. Pure string
#      builder: never touches the network/MCP/Windows itself.
build_recovery_script() {
  local box="$1" obs_dir="$2" target_fps="$3" confirm_threshold="$4" min_interval_s="$5" stale_lock_s="$6"

  # REUSE launch-obs-genlock.sh's own planner for the kill+relaunch+log-verify step — force=1
  # (self-heal only ever acts on a CONFIRMED wedge, so it always force-kills). This is the ONE
  # launch path; nothing here re-derives it.
  local kill_relaunch_program
  kill_relaunch_program="$(build_launch_program "$obs_dir" "1")"

  # AHK auto-respawn only exists on strih (.claude/skills/obs-ops "AHK on strih") — stream has no
  # second watcher to race, so its Stop/RestartAhk steps are documented no-ops, never a guess at a
  # script path that doesn't exist there.
  local ahk_stop_block ahk_start_block
  if [ "$box" = "strih" ]; then
    ahk_stop_block=$(cat <<'PS1'
Stop-Process -Name AutoHotkey64 -Force -ErrorAction SilentlyContinue
Write-SelfHealLog "StopAhk: AutoHotkey64 stopped (or was not running) — obs64 is now safe to touch"
PS1
)
    ahk_start_block=$(cat <<'PS1'
Start-Process -FilePath 'AutoHotkey64.exe' -ArgumentList '"D:\_APPS\NL_STARTUP.ahk"'
Write-SelfHealLog "RestartAhk: AutoHotkey64 relaunched (crash/reboot auto-respawn restored)"
PS1
)
  else
    ahk_stop_block='Write-SelfHealLog "StopAhk: no-op ('"$box"' has no AutoHotkey64 auto-respawn watcher)"'
    ahk_start_block='Write-SelfHealLog "RestartAhk: no-op ('"$box"' has no AutoHotkey64 auto-respawn watcher)"'
  fi

  cat <<PS
# ===== #411 obs-self-heal recovery script — box=${box} (Task Scheduler action, ~2 min cadence) =====
# Runs ENTIRELY on this box — no ssh, no MCP, no agent session. Reuses camera_box::obs_watchdog::
# classify (via obs-watchdog-gate.exe, LOCAL process signals only — no OBS WebSocket round-trip)
# for the wedge verdict, and launch-obs-genlock.sh's own kill+relaunch+verify program for recovery.
# The confirm/throttle/lock policy mirrors camera_box::obs_self_heal::decide() (src/obs_self_heal.rs)
# — kept in lock-step by tests/obs_self_heal_install.rs.
\$ErrorActionPreference = 'Stop'

\$InstallDir       = 'C:\\ProgramData\\camera-box'
\$StateFile        = Join-Path \$InstallDir 'obs-self-heal-state.json'
\$LogFile          = Join-Path \$InstallDir 'obs-self-heal.log'
\$GateBin          = Join-Path \$InstallDir 'obs-watchdog-gate.exe'
\$TargetFps        = ${target_fps}
\$ConfirmThreshold = ${confirm_threshold}
\$MinIntervalS     = ${min_interval_s}
\$StaleLockS       = ${stale_lock_s}

function Write-SelfHealLog(\$msg) {
  \$ts = Get-Date -Format 'yyyy-MM-ddTHH:mm:sszzz'
  Add-Content -Path \$LogFile -Value "\$ts [obs-self-heal:${box}] \$msg"
}

function Save-SelfHealState(\$s) {
  \$s | ConvertTo-Json | Set-Content -Path \$StateFile -Encoding UTF8
}

New-Item -ItemType Directory -Path \$InstallDir -Force | Out-Null

# ---- load persisted state (fail-safe defaults if missing/corrupt — never GUESSES a healthy prior) ----
\$state = [pscustomobject]@{
  confirm_count        = 0
  last_attempt_epoch_s = \$null
  recovery_in_progress = \$false
  last_cpu_s           = \$null
  last_sample_epoch_s  = \$null
}
if (Test-Path \$StateFile) {
  try {
    \$loaded = Get-Content \$StateFile -Raw | ConvertFrom-Json
    foreach (\$p in \$state.PSObject.Properties.Name) {
      if (\$null -ne \$loaded.\$p) { \$state.\$p = \$loaded.\$p }
    }
  } catch {
    Write-SelfHealLog "WARN: state file unreadable/corrupt (\$(\$_.Exception.Message)) — starting fresh (fail-safe)"
  }
}

\$now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()

# ---- stale-lock check (obs_self_heal::lock_is_stale) — a crashed prior run must not permanently ----
# ---- disable self-heal for this box.                                                            ----
if (\$state.recovery_in_progress -and \$state.last_attempt_epoch_s -and ((\$now - \$state.last_attempt_epoch_s) -gt \$StaleLockS)) {
  Write-SelfHealLog "lock held \$(\$now - \$state.last_attempt_epoch_s)s (> \${StaleLockS}s stale budget) — treating as ABANDONED, clearing (obs_self_heal::lock_is_stale)"
  \$state.recovery_in_progress = \$false
}

# ---- gather the LOCAL sample — process signals ONLY, no OBS WebSocket round-trip ----
\$procs      = Get-Process obs64 -ErrorAction SilentlyContinue
\$obs64Count = @(\$procs).Count
\$responding = \$null
\$cpuPercent = \$null
if (\$obs64Count -ge 1) {
  \$p     = \$procs | Select-Object -First 1
  \$responding = [bool]\$p.Responding
  \$cpuNow = \$p.TotalProcessorTime.TotalSeconds
  if ((\$null -ne \$state.last_cpu_s) -and (\$null -ne \$state.last_sample_epoch_s) -and ((\$now - \$state.last_sample_epoch_s) -gt 0)) {
    \$cores = [Environment]::ProcessorCount
    \$cpuPercent = ((\$cpuNow - \$state.last_cpu_s) / (\$now - \$state.last_sample_epoch_s) / \$cores) * 100.0
  }
  \$state.last_cpu_s          = \$cpuNow
  \$state.last_sample_epoch_s = \$now
} else {
  \$state.last_cpu_s          = \$null
  \$state.last_sample_epoch_s = \$now
}

# ---- verdict: REUSE obs_watchdog::classify via obs-watchdog-gate.exe — NEVER reinvent thresholds ----
\$sample = @{
  ws_reachable        = \$true
  active_fps          = \$null
  avg_render_time_ms  = \$null
  render_skipped_frac = \$null
  target_fps          = \$TargetFps
  obs64_count         = \$obs64Count
  responding          = \$responding
  cpu_percent         = \$cpuPercent
}
\$payload = @{ '${box}' = \$sample } | ConvertTo-Json -Depth 5 -Compress
if (-not (Test-Path \$GateBin)) {
  Write-SelfHealLog "FATAL: obs-watchdog-gate.exe not found at \$GateBin — cannot verify, refusing to guess. Install it before enabling this task."
  exit 5
}
\$verdictLine = \$payload | & \$GateBin
\$gateExit    = \$LASTEXITCODE
\$wedged      = (\$gateExit -ne 0)
Write-SelfHealLog "sample obs64_count=\$obs64Count responding=\$responding cpu%=\$cpuPercent -> \$verdictLine (gate exit \$gateExit)"

# ---- confirm / throttle / lock decision (mirrors camera_box::obs_self_heal::decide()) ----
if (\$state.recovery_in_progress) {
  Write-SelfHealLog "AlreadyRecovering: lock held since \$(\$state.last_attempt_epoch_s) — skipping this pass"
} elseif (-not \$wedged) {
  if (\$state.confirm_count -ne 0) { Write-SelfHealLog "Healthy: resetting confirm_count from \$(\$state.confirm_count) to 0" }
  \$state.confirm_count = 0
} else {
  \$state.confirm_count = \$state.confirm_count + 1
  if (\$state.confirm_count -lt \$ConfirmThreshold) {
    Write-SelfHealLog "Confirming: wedged pass \$(\$state.confirm_count)/\$ConfirmThreshold — not yet acting"
  } else {
    \$elapsed = if (\$state.last_attempt_epoch_s) { \$now - \$state.last_attempt_epoch_s } else { [double]::PositiveInfinity }
    if (\$elapsed -lt \$MinIntervalS) {
      Write-SelfHealLog "Throttled: confirmed wedged but only \${elapsed}s since last attempt (< \${MinIntervalS}s) — waiting"
    } else {
      # ---- ACT (obs_self_heal::SelfHealDecision::Recover) ----
      # Set the lock IMMEDIATELY, before touching obs64 at all — a crash mid-recovery must leave
      # the lock HELD (fail-safe), never silently cleared (obs_self_heal::decide() doc).
      \$state.last_attempt_epoch_s = \$now
      \$state.recovery_in_progress = \$true
      Save-SelfHealState \$state
      Write-SelfHealLog "RECOVER: confirmed wedged (\$(\$state.confirm_count)/\$ConfirmThreshold), not throttled — starting the 4-step recovery plan"

      # --- Step 1/4: StopAhk — MUST run before obs64 is ever touched (the AHK-race fix, #411) ---
${ahk_stop_block}

      # --- Step 2/4: KillAndRelaunchObs — the SAME launch-obs-genlock.sh program, --force ---
      \$tmpPs1 = Join-Path \$env:TEMP "camera-box-self-heal-relaunch-\$PID.ps1"
      @'
${kill_relaunch_program}
'@ | Set-Content -Path \$tmpPs1 -Encoding UTF8
      & powershell.exe -NoProfile -ExecutionPolicy Bypass -File \$tmpPs1
      \$relaunchExit = \$LASTEXITCODE
      Remove-Item \$tmpPs1 -ErrorAction SilentlyContinue
      Write-SelfHealLog "KillAndRelaunchObs: launch-obs-genlock program exited \$relaunchExit"

      # --- Step 3/4: VerifyRecovered — exactly one obs64 AND the launch program's own log-verify ---
      # ---            passed (obs_self_heal::recovery_verified — the SAME rule both sides check) ---
      Start-Sleep -Seconds 2
      \$postCount = @(Get-Process obs64 -ErrorAction SilentlyContinue).Count
      \$verified  = (\$postCount -eq 1) -and (\$relaunchExit -eq 0)
      Write-SelfHealLog "VerifyRecovered: obs64_count=\$postCount relaunchExit=\$relaunchExit -> verified=\$verified"

      # --- Step 4/4: RestartAhk — ALWAYS runs, regardless of \$verified (obs_self_heal.rs doc: ---
      # ---           AHK's crash-respawn duty is more valuable always-on than conditional)      ---
${ahk_start_block}

      \$state.recovery_in_progress = \$false
      Write-SelfHealLog "Recovery attempt complete (verified=\$verified) — lock cleared"
    }
  }
}

Save-SelfHealState \$state
PS
}

# build_task_xml TASK_NAME PS1_PATH INTERVAL_MIN
#   -> a Windows Task Scheduler XML (schema 2004/02/mit/task) that runs PS1_PATH every
#      INTERVAL_MIN minutes, indefinitely, as the INTERACTIVE logged-on user (obs64 is a GUI app —
#      a SYSTEM-context task would run in Session 0, isolated from the visible desktop and unable
#      to force-kill/relaunch obs64 into the real session). Ships with Enabled=false — the
#      supervisor enables it only after live-verifying on this exact box. UserId is a
#      __RIG_USER__ placeholder the supervisor fills in at install time (this planner does not
#      know the box's actual logged-on account).
build_task_xml() {
  local task_name="$1" ps1_path="$2" interval_min="$3"
  local ps1_path_xml="${ps1_path//&/&amp;}"
  cat <<XML
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>#411 camera-box obs-self-heal (${task_name}) — Windows-local unattended recovery for a wedged obs64, reusing camera_box::obs_watchdog::classify. Ships DISABLED; enable only after supervisor live-verify.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>PT${interval_min}M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>__RIG_USER__</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <Enabled>false</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>-NoProfile -ExecutionPolicy Bypass -File "${ps1_path_xml}"</Arguments>
    </Exec>
  </Actions>
</Task>
XML
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------------

usage() {
  cat <<'EOF'
obs-self-heal-install.sh — #411 Windows-local unattended self-heal for the #391 OBS liveness watchdog.

Prints the full supervisor install plan for one box: the recovery PowerShell script content (paste
into a file on the box) + the Task Scheduler XML (register with schtasks) + the live-verify
procedure. Ships DISABLED — do not enable the task until BOTH a healthy-box dry run (no false
force-kill) and a simulated-wedge run (genuine auto-recovery, AHK never double-launches) pass.

Usage:
  scripts/obs-self-heal-install.sh --box strih|stream
      [--obs-dir 'C:\Program Files\obs-studio']
      [--confirm-threshold 2] [--min-interval-s 600] [--stale-lock-s 900] [--interval-min 2]
  scripts/obs-self-heal-install.sh --help

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local box="" obs_dir='C:\Program Files\obs-studio'
  local confirm_threshold=2 min_interval_s=600 stale_lock_s=900 interval_min=2
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)               need_val "$@"; box="$2"; shift 2 ;;
      --obs-dir)           need_val "$@"; obs_dir="$2"; shift 2 ;;
      --confirm-threshold) need_val "$@"; confirm_threshold="$2"; shift 2 ;;
      --min-interval-s)    need_val "$@"; min_interval_s="$2"; shift 2 ;;
      --stale-lock-s)      need_val "$@"; stale_lock_s="$2"; shift 2 ;;
      --interval-min)      need_val "$@"; interval_min="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  local mcp box_ip target_fps
  case "$box" in
    strih)  mcp="win-strih";      box_ip="10.77.9.202"; target_fps=60 ;;
    stream) mcp="win-stream-snv"; box_ip="10.77.9.204"; target_fps=30 ;;
    *) echo "ERROR: --box must be 'strih' or 'stream' (got '${box}')" >&2; usage >&2; exit 2 ;;
  esac

  local task_name="camera-box-obs-self-heal-${box}"
  local ps1_path="C:\\ProgramData\\camera-box\\obs-self-heal.ps1"
  local xml_path="C:\\ProgramData\\camera-box\\obs-self-heal-task.xml"

  local RECOVERY_SCRIPT TASK_XML
  RECOVERY_SCRIPT="$(build_recovery_script "$box" "$obs_dir" "$target_fps" "$confirm_threshold" "$min_interval_s" "$stale_lock_s")"
  TASK_XML="$(build_task_xml "$task_name" "$ps1_path" "$interval_min")"

  cat <<PLAN
# ===== #411 obs-self-heal install plan — box=${box} (${mcp}, ${box_ip}) =====
# scp/ssh to Windows is DENIED — the agent/supervisor runs this via the ${mcp} MCP Shell.
#
# STEP 0: deploy the obs-watchdog-gate.exe CI artifact (probe-tools-windows-amd64) to
#         C:\\ProgramData\\camera-box\\obs-watchdog-gate.exe on this box (FileUpload via ${mcp}).
#
# STEP 1: write the recovery script to ${ps1_path} — paste via ${mcp} Shell (Set-Content, or
#         FileWrite) with the content between the dashed lines:
# ----------------------------------------------------------------------------------------------------
${RECOVERY_SCRIPT}
# ----------------------------------------------------------------------------------------------------
#
# STEP 2: write the Task Scheduler XML to ${xml_path}, then register it (ships DISABLED —
#         <Enabled>false</Enabled> — do NOT flip it on before live-verify below). Replace
#         __RIG_USER__ with this box's actual logged-on account (DOMAIN\\user or .\\user) before
#         registering — InteractiveToken logon needs the real account so obs64 launches into the
#         VISIBLE desktop session, not an isolated Session 0. schtasks /XML requires the file
#         saved as UTF-16 (the declared encoding below) — use
#         Set-Content -Encoding Unicode, not the UTF-8 default.
# ----------------------------------------------------------------------------------------------------
${TASK_XML}
# ----------------------------------------------------------------------------------------------------
#         schtasks /Create /TN "${task_name}" /XML "${xml_path}" /F
#
# STEP 3: LIVE-VERIFY before enabling (never skip — this is a force-kill mechanism):
#   a) Healthy-box dry run: with OBS running normally, run the task manually
#      (schtasks /Run /TN "${task_name}"), tail C:\\ProgramData\\camera-box\\obs-self-heal.log —
#      MUST log "HEALTHY"/no action, NEVER force-kill a healthy box.
#   b) Simulated-wedge run: force-kill obs64 WITHOUT relaunching (or otherwise make it
#      unresponsive), run the task manually TWICE (confirm-threshold=${confirm_threshold}) — the
#      SECOND run must force-kill+relaunch, log tick=ENABLED, and (on strih) show AHK stopped then
#      restarted with never more than one obs64 process at any point.
#   c) Only after BOTH (a) and (b) pass: schtasks /Change /TN "${task_name}" /ENABLE
#
# Disable later: schtasks /Change /TN "${task_name}" /DISABLE
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
