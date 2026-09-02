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
# on the box itself — no ssh round-trip, no MCP, no agent session required (a LOCAL scheduled job
# has no reason to reach back out over either channel).
#
# HOW THE PIECES FIT (mirrors scripts/launch-obs-genlock.sh's own model exactly):
#   - NEITHER decision this script needs is reimplemented in PowerShell:
#     - The WEDGE VERDICT: the emitted recovery script pipes a LOCAL sample (Get-Process obs64:
#       count / Responding / CPU%, no OBS WebSocket round-trip needed) to the EXISTING
#       `obs-watchdog-gate.exe` binary, which runs the EXACT SAME `camera_box::obs_watchdog::
#       classify` the #391 dev1 alert watchdog uses remotely.
#     - The RECOVERY decision (confirm-threshold / throttle / single-recovery lock / stale-lock
#       detection): the emitted script pipes its persisted state + this pass's verdict to the
#       EXISTING `obs-self-heal-gate.exe` binary, which calls `camera_box::obs_self_heal::decide`
#       (src/obs_self_heal.rs) DIRECTLY — never a hand-rolled re-derivation. The step ORDER (the
#       AHK-race fix) is asserted structurally by `tests/obs_self_heal_install.rs` against the
#       SAME `RecoveryStep` enum `decide()` returns.
#     Both gate binaries default their thresholds to `camera_box::obs_self_heal`'s own `DEFAULT_*`
#     Rust constants when the caller passes `null` (the default here, unless `main()`'s
#     `--confirm-threshold`/`--min-interval-s`/`--stale-lock-s` flags override it) — so those Rust
#     constants are the SINGLE actual source of default truth, never a second hardcoded literal in
#     this bash script that could silently drift from the kernel it claims to mirror.
#   - The kill+relaunch step REUSES `launch-obs-genlock.sh`'s `build_launch_program` VERBATIM (this
#     script sources that file) — there is ONE idempotent, self-verifying obs64 launch path in this
#     whole repo, never a second hand-rolled one.
#   - this script is the PURE PLANNER an agent/supervisor pastes (#701 proved plain scp/ssh
#     reaches strih/stream, but WRITING a recovery script + REGISTERING a Task Scheduler job is
#     exactly what the win-* MCP Shell is for here, not a ssh workaround)
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
# shellcheck source=scripts/lib/ahk-watchdog.sh
# Explicit even though launch-obs-genlock.sh above already sources this transitively — never rely
# on a transitive path staying wired.
. "$HERE/lib/ahk-watchdog.sh"

# --- PURE functions (no network, no MCP, no Windows — unit-tested by sourcing this script) --------

# ps_null_or_number VALUE -> "$null" (a literal PowerShell null token) when VALUE is empty, else
# VALUE itself. Used so an UNSET threshold/interval/stale-lock override becomes an OMITTED JSON
# field, which obs-self-heal-gate.exe defaults to its own camera_box::obs_self_heal::DEFAULT_*
# Rust constant — the single source of default truth, never a second hardcoded bash literal.
ps_null_or_number() {
  if [ -n "$1" ]; then printf '%s' "$1"; else printf '$null'; fi
}

# build_recovery_script BOX OBS_DIR TARGET_FPS [CONFIRM_THRESHOLD] [MIN_INTERVAL_S] [STALE_LOCK_S]
#                        [ENABLE_REBOOT]
#   -> the full PowerShell recovery script Task Scheduler runs every ~2 min on BOX. Pure string
#      builder: never touches the network/MCP/Windows itself. CONFIRM_THRESHOLD/MIN_INTERVAL_S/
#      STALE_LOCK_S are OPTIONAL — pass an empty string (or omit) to let obs-self-heal-gate.exe
#      apply its own DEFAULT_* Rust constant; pass a number to install an explicit override.
#      ENABLE_REBOOT (#89) is a plain boolean opt-in ("1" = enabled) — unlike the numeric
#      overrides above, a bare true/false has no "magic number" drift risk to protect against, so
#      it always installs a concrete $true/$false (never $null); omitted/anything-else = "0" =
#      $false (a host reboot is a destructive, approval-gated action — this MUST default off,
#      see camera_box::obs_self_heal::DEFAULT_REBOOT_ENABLED).
build_recovery_script() {
  local box="$1" obs_dir="$2" target_fps="$3"
  local confirm_threshold="${4:-}" min_interval_s="${5:-}" stale_lock_s="${6:-}"
  local enable_reboot="${7:-0}"
  local threshold_ps min_interval_ps stale_lock_ps reboot_enabled_ps
  threshold_ps="$(ps_null_or_number "$confirm_threshold")"
  min_interval_ps="$(ps_null_or_number "$min_interval_s")"
  stale_lock_ps="$(ps_null_or_number "$stale_lock_s")"
  if [ "$enable_reboot" = "1" ]; then reboot_enabled_ps='$true'; else reboot_enabled_ps='$false'; fi

  # REUSE launch-obs-genlock.sh's own planner for the kill+relaunch+log-verify step — force=1
  # (self-heal only ever acts on a CONFIRMED wedge, so it always force-kills). This is the ONE
  # launch path; nothing here re-derives it. has_ahk mirrors the block below: only strih runs the
  # AHK watcher, so only strih's embedded program may carry a real AutoHotkey64 command (#786).
  local kill_relaunch_program launch_has_ahk
  if [ "$box" = "strih" ]; then launch_has_ahk=1; else launch_has_ahk=0; fi
  kill_relaunch_program="$(build_launch_program "$obs_dir" "1" "$launch_has_ahk")"

  # AHK auto-respawn only exists on strih (.claude/skills/obs-ops "AHK on strih") — stream has no
  # second watcher to race.
  #
  # issue 1273 — SINGLE OWNER of the AutoHotkey64 stop/restart bracket. The embedded launch program
  # ($kill_relaunch_program, built with has_ahk=1 on strih) already owns the WHOLE bracket: it stops
  # AutoHotkey64 BEFORE killing obs64 (its own --force kill_block prepends the stop, so it covers the
  # wedge-kill race), restarts + VERIFIES it AFTER the launch+audio-verify sequence, then runs its own
  # #978 session-visibility gate. So the outer self-heal script must NOT ALSO pre-stop AHK: that ran
  # in a SEPARATE `powershell.exe -File` child process, so the embedded program's own $ahkStopped
  # always started fresh $false — it found AHK already stopped, never set $ahkStopped=$true, so its
  # own restart-gate never fired and its #978 gate then found 0 AutoHotkey64 and exit-8'd, forcing
  # $relaunchExit != 0 and misreporting verified=False on an otherwise-clean recovery (a diagnostics
  # false-negative). The ONLY outer AHK action left is a FAILURE-PATH backstop: if the embedded
  # program exited non-zero it may have aborted BEFORE its own restart point (an audio-buffering
  # exit 7, or the #786-relaunch exit 6, both sit before it), so — and only then — if AutoHotkey64
  # is genuinely down, best-effort relaunch it so a wedged box never ends with NO respawn watcher.
  # Idempotent AHK-present: never double-launches what the embedded program already restored on a
  # clean recovery. stream (has_ahk=0) has no watcher, so its backstop is a documented no-op.
  local ahk_backstop_block
  if [ "$box" = "strih" ]; then
    local ahk_relaunch_ps
    ahk_relaunch_ps="$(ahk_resolve_and_relaunch_ps)"
    # #867: the backstop restart is VERIFIED ($ahkRelaunchVerified) and logs an explicit FATAL line
    # when it does not come back — never a blind success claim. It is log-only, not a hard exit (a
    # scheduled task retries ~every 2 min regardless).
    ahk_backstop_block=$(cat <<PS1
if (\$relaunchExit -ne 0) {
  if (-not (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue)) {
    Write-SelfHealLog "RestartAhk backstop: embedded launch program exited \$relaunchExit and AutoHotkey64 is down -- best-effort relaunch so strih keeps a respawn watcher"
${ahk_relaunch_ps}
    if (\$ahkRelaunchVerified) {
      Write-SelfHealLog "RestartAhk backstop: AutoHotkey64 relaunched via \$ahkRelaunchTarget (crash/reboot auto-respawn restored)"
    } else {
      Write-SelfHealLog "FATAL: RestartAhk backstop failed -- AutoHotkey64 did not come back after relaunch (target=\$ahkRelaunchTarget) -- strih has NO respawn watcher until this is fixed"
    }
  } else {
    Write-SelfHealLog "RestartAhk backstop: embedded launch program exited \$relaunchExit but AutoHotkey64 is already running -- no action (embedded program restored it)"
  }
}
PS1
)
  else
    ahk_backstop_block='Write-SelfHealLog "RestartAhk backstop: no-op ('"$box"' has no AutoHotkey64 auto-respawn watcher)"'
  fi

  cat <<PS
# ===== #411 obs-self-heal recovery script — box=${box} (Task Scheduler action, ~2 min cadence) =====
# Runs ENTIRELY on this box — no ssh, no MCP, no agent session. NEITHER decision this script needs
# is reimplemented here: the wedge VERDICT is obs-watchdog-gate.exe (camera_box::obs_watchdog::
# classify, LOCAL process signals only — no OBS WebSocket round-trip); the RECOVERY decision
# (confirm/throttle/lock) is obs-self-heal-gate.exe (camera_box::obs_self_heal::decide DIRECTLY —
# see src/obs_self_heal.rs). The kill+relaunch step reuses launch-obs-genlock.sh's own program.
\$ErrorActionPreference = 'Stop'

\$InstallDir      = 'C:\\ProgramData\\camera-box'
\$StateFile       = Join-Path \$InstallDir 'obs-self-heal-state.json'
\$LogFile         = Join-Path \$InstallDir 'obs-self-heal.log'
\$GateBin         = Join-Path \$InstallDir 'obs-watchdog-gate.exe'
\$SelfHealGateBin = Join-Path \$InstallDir 'obs-self-heal-gate.exe'
\$TargetFps       = ${target_fps}
# \$null here means "use obs-self-heal-gate.exe's own camera_box::obs_self_heal::DEFAULT_* Rust
# constant" — only a real number (from --confirm-threshold/--min-interval-s/--stale-lock-s at
# generation time) installs an explicit override. Never a second hardcoded default here.
\$ConfirmThresholdOverride = ${threshold_ps}
\$MinIntervalSOverride     = ${min_interval_ps}
\$StaleLockSOverride       = ${stale_lock_ps}
# #89: a host reboot is a destructive, approval-gated action — defaults to \$false (see
# camera_box::obs_self_heal::DEFAULT_REBOOT_ENABLED). Only an explicit --enable-reboot at
# generation time installs \$true.
\$RebootEnabledOverride    = ${reboot_enabled_ps}

function Write-SelfHealLog(\$msg) {
  \$ts = Get-Date -Format 'yyyy-MM-ddTHH:mm:sszzz'
  Add-Content -Path \$LogFile -Value "\$ts [obs-self-heal:${box}] \$msg"
}

function Save-SelfHealState(\$s) {
  \$s | ConvertTo-Json | Set-Content -Path \$StateFile -Encoding UTF8
}

New-Item -ItemType Directory -Path \$InstallDir -Force | Out-Null

# ---- load persisted state (fail-safe defaults if missing/corrupt — never GUESSES a healthy prior) ----
# last_cpu_s / last_sample_epoch_s are LOCAL CPU-sampling continuity bookkeeping (Windows
# process-telemetry plumbing, not part of camera_box::obs_self_heal::SelfHealState — decide() only
# ever sees confirm_count / last_attempt_epoch_s / recovery_in_progress).
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
    \$computed = ((\$cpuNow - \$state.last_cpu_s) / (\$now - \$state.last_sample_epoch_s) / \$cores) * 100.0
    # A negative delta (process restarted, PID reused with lower cumulative CPU time) or a
    # non-finite result must never be sent as a number — ConvertTo-Json would emit invalid JSON
    # (NaN/Infinity are not valid JSON tokens) and could poison the gate's parse. Treat it as
    # "not sampled this pass" (null), never a guessed value.
    if ([double]::IsFinite(\$computed) -and (\$computed -ge 0)) { \$cpuPercent = \$computed }
  }
  \$state.last_cpu_s          = \$cpuNow
  \$state.last_sample_epoch_s = \$now
} else {
  \$state.last_cpu_s          = \$null
  \$state.last_sample_epoch_s = \$now
}

# ---- OBS-log DXGI device-lost audit (#89) — LOCAL read, no MCP/ssh needed on this box. Checks
# ---- the SAME three codes camera_box::dxgi_device_lost::DXGI_DEVICE_LOST_CODES matches
# ---- (887A0005/6/7) — never a re-derived/partial code list that could silently drift.
\$dxgiDeviceLost = \$false
\$obsLogDir = Join-Path \$env:APPDATA 'obs-studio\logs'
if (Test-Path \$obsLogDir) {
  \$latestObsLog = Get-ChildItem \$obsLogDir -Filter '*.txt' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if (\$latestObsLog) {
    \$dxgiDeviceLost = [bool](Select-String -Path \$latestObsLog.FullName -Pattern '887A0005|887A0006|887A0007' -Quiet -ErrorAction SilentlyContinue)
  }
}
\$cause = if (\$dxgiDeviceLost) { 'GpuDeviceRemoved' } else { 'ProcessWedge' }
Write-SelfHealLog "log-audit: dxgiDeviceLost=\$dxgiDeviceLost -> cause=\$cause"

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
  dxgi_device_lost    = \$dxgiDeviceLost
}
\$payload = @{ '${box}' = \$sample } | ConvertTo-Json -Depth 5 -Compress
if (-not (Test-Path \$GateBin)) {
  Write-SelfHealLog "FATAL: obs-watchdog-gate.exe not found at \$GateBin — cannot verify, refusing to guess. Install it before enabling this task."
  Save-SelfHealState \$state
  exit 5
}
\$verdictLine = \$payload | & \$GateBin
\$gateExit    = \$LASTEXITCODE
Write-SelfHealLog "sample obs64_count=\$obs64Count responding=\$responding cpu%=\$cpuPercent -> \$verdictLine (gate exit \$gateExit)"

# gate exit 0 = HEALTHY, 1 = a real classify verdict says unhealthy (wedged=true is correct). 2 =
# a TOOLING error in the payload WE built (bad JSON, wrong field type) — OUR bug, never evidence
# of a wedge. ANY OTHER exit code (a crash/panic/AV interference/corrupted install) is UNKNOWN and
# must NEVER silently fall through to "healthy" — that would SILENTLY STOP detecting a real wedge.
if (\$gateExit -eq 2) {
  Write-SelfHealLog "FATAL: obs-watchdog-gate.exe reported a payload error (exit 2) — this is a \
self-heal tooling bug, NOT a wedge verdict. Skipping this pass without acting; state left \
untouched so the next pass tries fresh."
  Save-SelfHealState \$state
  exit 6
}
if (\$gateExit -ne 0 -and \$gateExit -ne 1) {
  Write-SelfHealLog "FATAL: obs-watchdog-gate.exe returned an UNEXPECTED exit code \$gateExit \
(expected 0/1/2 — possible crash/panic/AV interference). Refusing to guess healthy or wedged; \
skipping this pass without acting."
  Save-SelfHealState \$state
  exit 8
}
\$wedged = (\$gateExit -eq 1)

# ---- RECOVERY decision: obs-self-heal-gate.exe calls camera_box::obs_self_heal::decide DIRECTLY ----
# ---- (never a hand-rolled re-derivation of confirm/throttle/lock here).                          ----
if (-not (Test-Path \$SelfHealGateBin)) {
  Write-SelfHealLog "FATAL: obs-self-heal-gate.exe not found at \$SelfHealGateBin — cannot decide, refusing to guess. Install it before enabling this task."
  Save-SelfHealState \$state
  exit 9
}
\$decisionInput = @{
  confirm_count        = \$state.confirm_count
  last_attempt_epoch_s = \$state.last_attempt_epoch_s
  recovery_in_progress = \$state.recovery_in_progress
  wedged               = \$wedged
  now_epoch_s          = \$now
  threshold            = \$ConfirmThresholdOverride
  min_interval_s       = \$MinIntervalSOverride
  stale_lock_s         = \$StaleLockSOverride
  cause                = \$cause
  reboot_enabled       = \$RebootEnabledOverride
} | ConvertTo-Json -Compress
\$decisionLine = \$decisionInput | & \$SelfHealGateBin
\$decisionExit = \$LASTEXITCODE
Write-SelfHealLog "decide: input=\$decisionInput -> \$decisionLine (exit \$decisionExit)"

# exit 2 = a TOOLING error in the payload THIS script built (bad JSON, wrong field type) — OUR
# bug, distinct from a real crash/panic (mirrors the same distinction obs-watchdog-gate's exit 2
# gets above, so an operator gets the SAME clear "this is our bug" signal on both gate paths).
if (\$decisionExit -eq 2) {
  Write-SelfHealLog "FATAL: obs-self-heal-gate.exe reported a payload error (exit 2) — this is a \
self-heal tooling bug (a malformed decision JSON THIS script built), NOT evidence about the box. \
Skipping this pass without acting; state left untouched so the next pass tries fresh."
  Save-SelfHealState \$state
  exit 11
}
if (\$decisionExit -ne 0 -and \$decisionExit -ne 1) {
  Write-SelfHealLog "FATAL: obs-self-heal-gate.exe returned an UNEXPECTED exit code \$decisionExit \
(expected 0/1/2 — possible crash/panic). Refusing to guess; skipping this pass without acting."
  Save-SelfHealState \$state
  exit 10
}

\$decision = \$decisionLine | ConvertFrom-Json
if (\$decision.stale_lock_cleared) {
  Write-SelfHealLog "STALE LOCK CLEARED: a prior recovery attempt's lock was held past the stale \
budget and has been treated as ABANDONED (obs_self_heal::lock_is_stale) — this pass's decision \
follows that clear, not necessarily a fresh confirm cycle."
}
# next_state is persisted EVERY pass, regardless of decision — even Healthy resets confirm_count.
# A Recover decision's next_state ALREADY has recovery_in_progress=true (the lock is set BEFORE
# obs64 is ever touched, by decide() itself — fail-safe on a crash mid-recovery).
\$state.confirm_count        = \$decision.next_state.confirm_count
\$state.last_attempt_epoch_s = \$decision.next_state.last_attempt_epoch_s
\$state.recovery_in_progress = \$decision.next_state.recovery_in_progress
Save-SelfHealState \$state

switch (\$decision.decision) {
  'Healthy'           { Write-SelfHealLog "Healthy: no action" }
  'AlreadyRecovering' { Write-SelfHealLog "AlreadyRecovering: lock held since \$(\$state.last_attempt_epoch_s) — skipping this pass" }
  'Confirming'        { Write-SelfHealLog "Confirming: wedged pass \$(\$decision.confirm_count)/\$(\$decision.threshold) — not yet acting" }
  'Throttled'         { Write-SelfHealLog "Throttled: confirmed wedged but \$(\$decision.seconds_remaining)s remain before the next attempt is allowed — waiting" }
  'Recover' {
    # #89: recovery_plan()'s CONTENT branches on cause — this if/elseif/else mirrors that
    # branching exactly. The ORIGINAL #411 process-wedge plan (KillAndRelaunchObs present) is
    # checked FIRST so its step sequence/log text below is completely untouched by this change.
    if (\$decision.steps -contains 'KillAndRelaunchObs') {
    Write-SelfHealLog "RECOVER: obs-self-heal-gate.exe says ACT — running the recovery plan (\$(\$decision.steps -join ' -> ')). The embedded launch-obs-genlock program OWNS the AutoHotkey64 stop/restart bracket (issue 1273); the outer script only backstops AHK on a failure exit."

    # --- KillAndRelaunchObs — the SAME launch-obs-genlock.sh program, --force. Built with has_ahk=1
    # ---   on strih, so it OWNS the whole AutoHotkey64 bracket: it stops AHK BEFORE killing obs64
    # ---   (its own --force kill covers the wedge-kill race), restarts + verifies it AFTER the
    # ---   launch, then runs its own #978 session gate. The outer script must NOT pre-stop AHK —
    # ---   that ran in a SEPARATE child process, leaving this embedded program's own \$ahkStopped
    # ---   false, so its restart never fired and its session gate exit-8'd on a clean recovery,
    # ---   force-falsing \$verified below (issue 1273). ---
    \$tmpPs1 = Join-Path \$env:TEMP "camera-box-self-heal-relaunch-\$PID.ps1"
    @'
${kill_relaunch_program}
'@ | Set-Content -Path \$tmpPs1 -Encoding UTF8
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File \$tmpPs1
    \$relaunchExit = \$LASTEXITCODE
    Remove-Item \$tmpPs1 -ErrorAction SilentlyContinue
    Write-SelfHealLog "KillAndRelaunchObs: launch-obs-genlock program exited \$relaunchExit"

    # --- VerifyRecovered — exactly one obs64 AND the launch program's own log-verify passed
    # ---   (obs_self_heal::recovery_verified — the SAME rule both sides check). With the AHK bracket
    # ---   now owned by the embedded program, a clean recovery exits 0, so \$verified is HONEST — no
    # ---   longer force-false by a nested-process AutoHotkey64 session-gate exit 8 (issue 1273). ---
    Start-Sleep -Seconds 2
    \$postCount = @(Get-Process obs64 -ErrorAction SilentlyContinue).Count
    \$verified  = (\$postCount -eq 1) -and (\$relaunchExit -eq 0)
    Write-SelfHealLog "VerifyRecovered: obs64_count=\$postCount relaunchExit=\$relaunchExit -> verified=\$verified"

    # --- RestartAhk backstop — FAILURE PATH ONLY. On a clean recovery the embedded program already
    # ---   restarted + verified AutoHotkey64 (regardless of obs64's own render-verify, so AHK is
    # ---   NEVER withheld on a false \$verified). Only if the embedded program exited non-zero (it
    # ---   may have aborted before its own restart point) AND AHK is genuinely down does the outer
    # ---   script best-effort relaunch it — idempotent, never double-launching (issue 1273). ---
${ahk_backstop_block}

    \$state.recovery_in_progress = \$false
    Save-SelfHealState \$state
    Write-SelfHealLog "Recovery attempt complete (verified=\$verified) — lock cleared"
    } elseif (\$decision.steps -contains 'RebootPc') {
    # #89: GpuDeviceRemoved cause, --enable-reboot was set at generation time — the ONLY branch
    # that ever executes a real host reboot, and only ever reached when recovery_plan() itself
    # already decided to include RebootPc (i.e. \$RebootEnabledOverride was \$true this pass).
    Write-SelfHealLog "RECOVER (#89): obs-self-heal-gate.exe says ACT — GPU device removed (DXGI log signature), reboot enabled — executing Restart-Computer"
    \$state.recovery_in_progress = \$false
    Save-SelfHealState \$state
    Write-SelfHealLog "REBOOT (#89): restarting the host now to clear the GPU device-removed wedge (an OBS-only restart would not clear it)"
    Restart-Computer -Force
    } else {
    # #89: GpuDeviceRemoved cause, reboot DISABLED (the default, \$RebootEnabledOverride = \$false)
    # -> recovery_plan() returned an EMPTY plan — nothing safe to auto-execute (an OBS-only
    # restart would not clear this cause). ALERT ONLY: log it and clear the lock so the next
    # pass can re-confirm; a human/agent must reboot the box (or --enable-reboot can be set).
    Write-SelfHealLog "RECOVER (#89): GPU device removed but auto-reboot is DISABLED (--enable-reboot not set) — ALERT ONLY, no automatic action taken; a full PC reboot is required to clear this box"
    \$state.recovery_in_progress = \$false
    Save-SelfHealState \$state
    }
  }
  default { Write-SelfHealLog "FATAL: obs-self-heal-gate.exe returned an unrecognized decision '\$(\$decision.decision)' — treating as no-op, never acting on an unrecognized signal." }
}
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

#89: the script audits the box's own OBS log locally for the DXGI device-lost (GPU TDR /
driver-internal-error) signature every pass. A confirmed GpuDeviceRemoved wedge does NOT
force-kill/relaunch obs64 (an OBS-only restart typically does not clear it) — by DEFAULT it is
ALERT ONLY (logged, no automatic action; a full PC reboot is required). Pass --enable-reboot to
opt this box's job into actually rebooting the host when that happens (still gated behind the
overall task's own <Enabled>false</Enabled> until the supervisor live-verifies + enables it).

Usage:
  scripts/obs-self-heal-install.sh --box strih|stream
      [--obs-dir 'C:\Program Files\obs-studio']
      [--confirm-threshold 2] [--min-interval-s 600] [--stale-lock-s 900] [--interval-min 2]
      [--enable-reboot]
  scripts/obs-self-heal-install.sh --help

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local box="" obs_dir='C:\Program Files\obs-studio'
  # Empty by default — obs-self-heal-gate.exe applies its own camera_box::obs_self_heal::
  # DEFAULT_* Rust constant when these are unset. Only an explicit flag installs an override.
  local confirm_threshold="" min_interval_s="" stale_lock_s="" interval_min=2
  # #89: OFF by default (a host reboot is a destructive, approval-gated action) — only an
  # explicit --enable-reboot flips it on for this box's installed self-heal job.
  local enable_reboot=0
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)               need_val "$@"; box="$2"; shift 2 ;;
      --obs-dir)           need_val "$@"; obs_dir="$2"; shift 2 ;;
      --confirm-threshold) need_val "$@"; confirm_threshold="$2"; shift 2 ;;
      --min-interval-s)    need_val "$@"; min_interval_s="$2"; shift 2 ;;
      --stale-lock-s)      need_val "$@"; stale_lock_s="$2"; shift 2 ;;
      --interval-min)      need_val "$@"; interval_min="$2"; shift 2 ;;
      --enable-reboot)     enable_reboot=1; shift 1 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  # Topology v2 (#459, EPIC #466, was 60 pre-#459): strih is now cut-to-stream only at 30fps --
  # the 60fps LED-wall IMAG role moved to the new imag-nb box (#458/#463).
  local mcp box_ip target_fps
  case "$box" in
    strih)  mcp="win-strih";      box_ip="10.77.9.202"; target_fps=30 ;;
    stream) mcp="win-stream-snv"; box_ip="10.77.9.204"; target_fps=30 ;;
    *) echo "ERROR: --box must be 'strih' or 'stream' (got '${box}')" >&2; usage >&2; exit 2 ;;
  esac

  local task_name="camera-box-obs-self-heal-${box}"
  local ps1_path="C:\\ProgramData\\camera-box\\obs-self-heal.ps1"
  local xml_path="C:\\ProgramData\\camera-box\\obs-self-heal-task.xml"

  # Display string for STEP 3's live-verify note — shows the EFFECTIVE threshold whether it's an
  # explicit override or a reference to the gate binary's own DEFAULT_CONFIRM_THRESHOLD. NEVER a
  # bare number here when unset — that would be exactly the second hardcoded literal (independent
  # of src/obs_self_heal.rs's Rust constant) this design otherwise avoids.
  local confirm_threshold_display="${confirm_threshold:-DEFAULT_CONFIRM_THRESHOLD in src/obs_self_heal.rs}"

  local RECOVERY_SCRIPT TASK_XML
  RECOVERY_SCRIPT="$(build_recovery_script "$box" "$obs_dir" "$target_fps" "$confirm_threshold" "$min_interval_s" "$stale_lock_s" "$enable_reboot")"
  TASK_XML="$(build_task_xml "$task_name" "$ps1_path" "$interval_min")"

  cat <<PLAN
# ===== #411 obs-self-heal install plan — box=${box} (${mcp}, ${box_ip}) =====
# Run this via the ${mcp} MCP Shell — writing the recovery script + registering the Task
# Scheduler job is exactly what the win-* MCP is for (#701: plain scp/ssh DOES reach strih/stream,
# but that doesn't replace registering a scheduled task).
#
# STEP 0: deploy the obs-watchdog-gate.exe AND obs-self-heal-gate.exe CI artifacts (both ship in
#         probe-tools-windows-amd64) to C:\\ProgramData\\camera-box\\ on this box (FileUpload via
#         ${mcp}). The recovery script below fails loud (does not act) if either is missing.
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
#      unresponsive), run the task manually TWICE (confirm-threshold=${confirm_threshold_display}) — the
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
