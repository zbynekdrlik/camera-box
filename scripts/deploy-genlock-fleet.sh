#!/usr/bin/env bash
# scripts/deploy-genlock-fleet.sh -- ONE deploy path for the OBS genlock build across the whole rig.
# See the extended header below for the full contract.
set -euo pipefail
#
# issue 789 (bod 4 "jediná deploy cesta" + bod 5 retencia): before this script the genlock build was
# deployed to strih + stream by a PROSE runbook (rig-state-inspection.md §5 / the genlock skill) and
# to imag by the 25-step setup-imag.sh provisioning -- three separate, uncommitted, untestable paths
# from THREE separate CI run ids (windows-genlock.yml vs windows-genlock-fast.yml vs
# linux-genlock.yml). The imag leg was routinely forgotten, so imag lagged strih/stream and every
# subsequent Full-path E2E was REFUSED at the genlock_parity preflight (issue 949) -- the recurring
# pain of issue 923 / issue 932.
#
# This script is the SINGLE ENTRY POINT: given ONE anchor run id it resolves the SAME-commit-SHA
# artifacts across all three workflows and drives ONE deploy path to every requested box, plus box
# retention (bod 5) and one durable fleet-deploy log line.
#
# It is a PLANNER + bounded ssh-executor in the exact shape of scripts/launch-obs-genlock.sh
# (pure builders -> source-guard -> main). The Windows leg is EMIT-ONLY: a bash script cannot drive
# the win-* MCP (win-ssh-vs-mcp HARD rule -- dev1 has no strih C$, an ssh-launched GUI lands in
# session 0), so it PRINTS the exact PowerShell program the agent pastes into the box's win-* MCP
# Shell. The imag leg is Linux (file copy + CLI = Context B), so in execute mode the script actually
# scp's + ssh-runs its emitted on-imag program. Marker writes go through the shared
# scripts/lib/genlock-markers.sh helper (setup-imag.sh carries a parity-locked inline copy).
#
# The pure builders take NO network / MCP / Windows and are unit-tested by sourcing this file
# (tests/deploy_genlock_fleet.rs) -- a regression in an emitted program is caught with no rig.
#
# Usage (planner mode -- print the whole fleet deploy plan, no network, from a pre-staged bundle):
#   scripts/deploy-genlock-fleet.sh --plan --run-id <id> --sha <headSha> --stage <dir> \
#       [--full|--fast] [--boxes strih,stream,imag]
#
# Usage (execute mode -- resolve + download the same-SHA artifacts, emit the Windows plan, ssh-deploy
# imag, append the fleet log; drive Windows via the printed win-* MCP program):
#   scripts/deploy-genlock-fleet.sh --run-id <id> [--full|--fast] [--boxes strih,stream,imag] [--yes]
#
#   --run-id   the ANCHOR CI run id (any of the three genlock workflows) -- its headSha is the ONE
#              canonical version every box converges to.
#   --full     deploy the full windows-genlock bundle (obs.dll + data + obs-plugins) -- the default;
#              required for any vendor/<plugin>/** or frontend change (fast has no deploy path for it).
#   --fast     deploy only the libobs hot-swap dll (obs.dll) -- a libobs-only change (§5b).
#   --boxes    comma list of strih,stream,imag (default: all three).
#   --plan     print the plan only; no gh/ssh/scp. Requires --stage + --sha (no network).
#   --stage    local dir holding the (pre-)downloaded artifact bytes (plan mode / test seam).
#   --sha      the canonical build SHA to stamp into the markers (plan mode override).
#   --yes      (execute mode) auto-confirm the box-backup retention deletions.
#
# Exit codes: 0 = ok, 2 = usage error, 3 = resolution/download failure, 4 = a box deploy failed.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/genlock-markers.sh
. "$HERE/lib/genlock-markers.sh"
# shellcheck source=scripts/lib/ahk-watchdog.sh
# The strih AHK watchdog restart (#789 issue-review #1): the deploy program stops AHK to copy, then
# MUST restart it before handing off to launch-obs-genlock.sh -- launch only restarts AHK IF IT
# stopped it itself and its #978 session gate then hard-fails (exit 8) on AHK count != 1. Reuse the
# ONE verified relaunch helper launch itself uses, never a fork.
. "$HERE/lib/ahk-watchdog.sh"

GENLOCK_REPO="zbynekdrlik/camera-box"
FLEET_LOG_DEFAULT="${HOME}/.camera-box/genlock-fleet-deploy.log"
RETENTION_KEEP=3

# ============================================================================================
# PURE functions (no network / MCP / Windows -- unit-tested by sourcing this file).
# ============================================================================================

# fleet_normalize_boxes CSV -> the requested boxes in canonical order (strih,stream,imag), deduped
# and validated. Empty -> the whole fleet. An unknown box is a fail-loud usage error (return 2).
fleet_normalize_boxes() {
  local csv="${1:-}" b out="" has_strih=0 has_stream=0 has_imag=0
  [ -n "$csv" ] || csv="strih,stream,imag"
  local IFS=','
  for b in $csv; do
    b="${b//[[:space:]]/}"
    [ -z "$b" ] && continue
    case "$b" in
      strih)  has_strih=1 ;;
      stream) has_stream=1 ;;
      imag)   has_imag=1 ;;
      *) echo "fleet_normalize_boxes: unknown box '$b' (valid: strih, stream, imag)" >&2; return 2 ;;
    esac
  done
  [ "$has_strih" = 1 ]  && out="strih"
  [ "$has_stream" = 1 ] && out="${out:+$out,}stream"
  [ "$has_imag" = 1 ]   && out="${out:+$out,}imag"
  [ -n "$out" ] || { echo "fleet_normalize_boxes: empty box set" >&2; return 2; }
  printf '%s\n' "$out"
}

# fleet_windows_artifact MODE -> the CI artifact name for the Windows genlock deploy.
fleet_windows_artifact() {
  case "${1:-}" in
    full) echo "obs-genlock-windows-x64" ;;
    fast) echo "obs-genlock-fast-dll" ;;
    *) echo "fleet_windows_artifact: MODE must be full|fast" >&2; return 2 ;;
  esac
}

# fleet_windows_workflow MODE -> the workflow file that produces the Windows artifact.
fleet_windows_workflow() {
  case "${1:-}" in
    full) echo "windows-genlock.yml" ;;
    fast) echo "windows-genlock-fast.yml" ;;
    *) echo "fleet_windows_workflow: MODE must be full|fast" >&2; return 2 ;;
  esac
}

fleet_linux_bundle_artifact()   { echo "obs-genlock-linux-x86_64"; }
fleet_linux_distroav_artifact() { echo "distroav-linux-fast-so"; }

# fleet_pick_run_at_sha SHA  (stdin: a `gh run list --json databaseId,headSha,conclusion` JSON array)
#   -> the databaseId of the FIRST successful run whose headSha == SHA, or empty. This is the heart
#   of "one canonical version everywhere": the anchor run id designates a commit SHA, and each box's
#   artifact must come from a run of ITS workflow at the SAME SHA (Windows and Linux genlock build in
#   SEPARATE workflows/run-ids). Pure (stdin -> stdout via jq) so it is unit-tested with a fixture.
fleet_pick_run_at_sha() {
  local sha="${1:-}"
  [ -n "$sha" ] || { echo "fleet_pick_run_at_sha: SHA required" >&2; return 2; }
  jq -r --arg s "$sha" '[.[] | select(.headSha == $s and .conclusion == "success")][0].databaseId // empty'
}

# fleet_box_mcp / fleet_box_ip / fleet_box_has_ahk -- per-box constants (only strih runs the
# NL_STARTUP.ahk auto-respawn watcher, so only strih's program stops AutoHotkey64).
fleet_box_mcp()     { case "${1:-}" in strih) echo "win-strih" ;; stream) echo "win-stream-snv" ;; *) return 2 ;; esac; }
fleet_box_ip()      { case "${1:-}" in strih) echo "10.77.9.202" ;; stream) echo "10.77.9.204" ;; imag) echo "imag" ;; *) return 2 ;; esac; }
fleet_box_has_ahk() { case "${1:-}" in strih) echo "1" ;; *) echo "0" ;; esac; }

# fleet_box_keepalive_tasks BOX -> the OBS keep-alive SCHEDULED-TASK names the deploy must disable so
# NONE of them respawns obs64 while the bytes are being copied (#1140). Per-box + CURATED, never all
# of a box's ~nine scheduled tasks: the stream box runs the #812 avsync-keepalive (~10 min) AND the
# #411 obs-self-heal (~2 min) -- avsync-keepalive is the named minimum, but the actual obs64 respawner
# is obs-self-heal (avsync-keepalive only relaunches the two avsync monitor scripts), so both must be
# named. strih's keep-alive is the AHK watcher (handled by the has_ahk stop/restart path), so it
# lists none. The emitted program disables+restores ONLY a task that is PRESENT and ENABLED, so a
# name absent (or deliberately disabled) on the box is a harmless skip -- adding another box's
# keep-alive here later is a one-line change, not a hardcoded pile inline at the call site.
# Task names MUST be whitespace-free: the emitter word-splits this space-separated list.
fleet_box_keepalive_tasks() {
  case "${1:-}" in
    stream) echo 'avsync-keepalive camera-box-obs-self-heal-stream' ;;
    *)      echo '' ;;
  esac
}

# build_windows_deploy_program BOX MODE STAGE OBS_DIR HAS_AHK BACKUP_ROOT KEEP GSHA DSHA
#   Emit the PowerShell program the agent pastes into the box's win-* MCP Shell. It stops the AHK
#   watchdog (strih only) + obs64, clears crash sentinels, backs up the components it overwrites,
#   copies the new bytes from STAGE (full = 3 surgical robocopies; fast = obs.dll only), writes BOTH
#   markers + DEPLOYED_AT temp-then-rename, sha256-verifies the deployed obs.dll against the bundle
#   manifest (fail-closed), and prints a box-backup RETENTION PLAN (keep newest KEEP; delete only
#   when $fleetConfirmRetention). Env-free (the genlock build carries no OBS_GENLOCK_*/OBS_BURN_*).
build_windows_deploy_program() {
  local box="$1" mode="$2" stage="$3" obs_dir="$4" has_ahk="$5" backup_root="$6" keep="$7" gsha="$8" dsha="$9" confirm="${10:-0}"
  # Escape ' for the PowerShell single-quoted strings (double it) -- incl. gsha/dsha (--sha is
  # operator-supplied in plan mode and also flows into $stage paths; #789 review #7 defense-in-depth).
  local stage_ps="${stage//\'/\'\'}" obs_ps="${obs_dir//\'/\'\'}" back_ps="${backup_root//\'/\'\'}"
  local gsha_ps="${gsha//\'/\'\'}" dsha_ps="${dsha//\'/\'\'}"
  local confirm_ps='$false'; [ "$confirm" = "1" ] && confirm_ps='$true'

  local ahk_stop ahk_restart
  if [ "$has_ahk" = "1" ]; then
    ahk_stop=$(cat <<'PSAHK'
# (1) Stop the strih AHK watchdog FIRST -- NL_STARTUP.ahk respawns obs64 via the bare exe within
#     seconds of the window vanishing, which would re-lock data\ + obs-plugins\ files mid-copy
#     (robocopy exit >= 8) AND drop the shortcut params. This program RESTARTS it at the end (step 8)
#     so the box is left consistent and launch-obs-genlock.sh's #978 session gate (AHK count == 1)
#     passes at STEP 2 -- launch only restarts AHK it stopped ITSELF, so we must not leave it down.
if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) {
  Stop-Process -Name AutoHotkey64 -Force -ErrorAction SilentlyContinue
}
PSAHK
)
    # #789 review #1: restart AHK VERIFIED via the ONE shared helper launch-obs-genlock.sh uses
    # (scripts/lib/ahk-watchdog.sh) -- never a fork. Fail loud if it does not come back.
    local ahk_relaunch_ps; ahk_relaunch_ps="$(ahk_resolve_and_relaunch_ps)"
    ahk_restart=$(cat <<PSAHKR
# (8) Restart the strih AHK watchdog we stopped in step (1), VERIFIED (leaves AHK running so the
#     STEP-2 launch-obs-genlock.sh session gate passes). AHK's app1_run then keeps obs64 alive via
#     the .lnk; the STEP-2 launch (--force) does the deterministic relaunch + render-tick verify.
${ahk_relaunch_ps}
if (\$ahkRelaunchVerified) {
  Write-Host "#789: AHK watchdog restarted via \$ahkRelaunchTarget."
} else {
  Write-Error "#789 FAIL: AutoHotkey64 did not come back after the deploy (target=\$ahkRelaunchTarget) -- strih has NO respawn watcher; investigate before trusting this box."
  exit 9
}
PSAHKR
)
  else
    ahk_stop='# (1) AutoHotkey64 watchdog: no-op (this box runs no AHK auto-respawn watcher).'
    ahk_restart='# (8) AutoHotkey64 restart: no-op (no AHK watcher on this box).'
  fi

  # #1140: the STREAM box (and any box the per-box fleet_box_keepalive_tasks lists) runs Task-Scheduler
  # keep-alive job(s) that respawn obs64 within their cadence -- if one fires during the byte copy it
  # re-locks the OBS bin directory (robocopy ERROR 32 sharing violation -> exit 4, the 2026-08-19
  # incident). Mirror the strih AHK stop->verified-restart contract for scheduled tasks: DISABLE the
  # box's keep-alive tasks before stopping obs64 (step 1b), RE-ENABLE + verify exactly the ones we
  # disabled at the end (step 8b). Runtime state (which tasks we actually disabled) lives in the
  # PowerShell $disabledKeepAlive array, exactly as AHK's own restart tracks $ahkRelaunchVerified.
  local keepalive_disable keepalive_restore
  local ka_tasks; ka_tasks="$(fleet_box_keepalive_tasks "$box")"
  if [ -n "$ka_tasks" ]; then
    local -a ka_arr=(); IFS=' ' read -r -a ka_arr <<< "$ka_tasks"
    local ps_arr="" t t_ps
    for t in "${ka_arr[@]}"; do
      t_ps="${t//\'/\'\'}"   # double any single quote for the PowerShell single-quoted literal
      if [ -z "$ps_arr" ]; then ps_arr="'${t_ps}'"; else ps_arr="${ps_arr}, '${t_ps}'"; fi
    done
    # HEAD heredoc is UNQUOTED (interpolates ${ps_arr} + \$keepAliveTasks); BODY heredoc is QUOTED
    # (all literal PowerShell, no escaping) -- so only the one array line needs interpolation.
    keepalive_disable="$(cat <<PSKAD_HEAD
# (1b) #1140 -- disable the OBS keep-alive scheduled tasks so NONE of them respawns obs64 mid-copy.
#      This box runs Task-Scheduler keep-alive job(s) that relaunch obs64 within their cadence of the
#      window vanishing (the #411 obs-self-heal, ~2 min, is the real respawner; the #812
#      avsync-keepalive is the named minimum) -- exactly what re-locked the OBS bin directory during
#      the byte copy on 2026-08-19 (ERROR 32 sharing violation, deploy exit 4). Step (8b) RE-ENABLES +
#      verifies them at the end, mirroring the strih AHK stop->verified-restart contract. Only a task
#      that is PRESENT AND ENABLED now is disabled+restored, so a deliberately-disabled task
#      (obs-self-heal ships DISABLED) is left exactly as-is.
\$keepAliveTasks    = @(${ps_arr})
PSKAD_HEAD
)
$(cat <<'PSKAD_BODY'
$disabledKeepAlive = @()
foreach ($t in $keepAliveTasks) {
  $eapSave = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  $q = schtasks /Query /TN $t /FO LIST /V 2>$null
  $ErrorActionPreference = $eapSave
  if ($LASTEXITCODE -ne 0) { Write-Host "#1140: keep-alive task '$t' not present -- skipping."; continue }
  $stateLine = ($q | Where-Object { $_ -match 'Scheduled Task State:' } | Select-Object -First 1)
  # FAIL LOUD (never fail-OPEN) if the task is present but its state is unreadable -- silently
  # treating it as "already disabled" would leave a live keep-alive to respawn obs64 mid-copy, the
  # exact #1140 incident with no warning. This surfaces the English-locale assumption instead of
  # hiding it (consistent with the disable/restore fail-closed exits below).
  if (-not $stateLine) { Write-Error "#1140 FAIL: could not read the Scheduled Task State for '$t' (present but no state line -- unexpected schtasks output/locale); refusing to proceed unprotected. Already-disabled keep-alive task(s) needing manual re-enable: $($disabledKeepAlive -join ', ') (schtasks /Change /TN <name> /ENABLE)."; exit 10 }
  if ($stateLine -notmatch 'Enabled') { Write-Host "#1140: keep-alive task '$t' already disabled -- leaving as-is."; continue }
  schtasks /Change /TN $t /DISABLE | Out-Null
  if ($LASTEXITCODE -ne 0) { Write-Error "#1140 FAIL: could not disable keep-alive task '$t' before the copy -- it could respawn obs64 and corrupt the deploy; aborting. Already-disabled keep-alive task(s) needing manual re-enable: $($disabledKeepAlive -join ', ') (schtasks /Change /TN <name> /ENABLE)."; exit 10 }
  schtasks /End /TN $t 2>$null | Out-Null   # terminate any in-flight instance too (parity with the AHK Stop-Process), best-effort
  $disabledKeepAlive += $t
  Write-Host "#1140: disabled keep-alive task '$t' for the deploy."
}
PSKAD_BODY
)"
    keepalive_restore=$(cat <<'PSKAR'
# (8b) #1140 -- re-enable the OBS keep-alive scheduled tasks we disabled in step (1b), VERIFIED
#      (mirrors the strih AHK verified-restart). Once the STEP-2 launch relaunches obs64 the
#      keep-alive resumes its normal watch. Fail loud if any we disabled does not come back enabled --
#      the box would be left with NO keep-alive for that monitor.
$keepAliveRestoreFailed = @()
foreach ($t in $disabledKeepAlive) {
  schtasks /Change /TN $t /ENABLE | Out-Null
  $eapSave2 = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  $q2 = schtasks /Query /TN $t /FO LIST /V 2>$null
  $ErrorActionPreference = $eapSave2
  $stateLine2 = ($q2 | Where-Object { $_ -match 'Scheduled Task State:' } | Select-Object -First 1)
  if ($LASTEXITCODE -eq 0 -and $stateLine2 -match 'Enabled') {
    Write-Host "#1140: keep-alive task '$t' re-enabled and verified."
  } else {
    $keepAliveRestoreFailed += $t
  }
}
if ($keepAliveRestoreFailed.Count -gt 0) {
  Write-Error "#1140 FAIL: keep-alive task(s) did not re-enable after the deploy: $($keepAliveRestoreFailed -join ', ') -- re-enable by hand (schtasks /Change /TN <name> /ENABLE); the box has NO keep-alive for them until then."
  exit 10
}
PSKAR
)
  else
    keepalive_disable='# (1b) OBS keep-alive scheduled tasks: no-op (this box runs no OBS keep-alive scheduled task; any keep-alive is the AHK watcher handled in step 1).'
    keepalive_restore='# (8b) OBS keep-alive scheduled tasks: no-op (none disabled on this box).'
  fi

  local copy_block
  if [ "$mode" = "full" ]; then
    copy_block=$(cat <<'PSFULL'
# (4) FULL bundle: three surgical robocopies (overwrite-keep-extras, NEVER /MIR|/PURGE). robocopy
#     exit 0-7 is SUCCESS; only >= 8 is failure. /XF the two files that must not ride here:
#     *.pdb (never deployed) and obs-virtualcam-module64.dll (held by the Camera Frame Server ->
#     robocopy retries forever without /XF), and distroav.dll (lives in ProgramData; a copy here is
#     a shadow drift-guard flags). /R:2 /W:2 so a locked file fails fast instead of hanging.
robocopy "$stage\bin\64bit"          "$obsDir\bin\64bit"          /E /XF *.pdb /MT:16 /R:2 /W:2
if ($LASTEXITCODE -ge 8) { Write-Error "robocopy bin\64bit failed ($LASTEXITCODE)"; exit 4 }
robocopy "$stage\data"               "$obsDir\data"               /E /XF obs-virtualcam-module64.dll /MT:16 /R:2 /W:2
if ($LASTEXITCODE -ge 8) { Write-Error "robocopy data failed ($LASTEXITCODE)"; exit 4 }
robocopy "$stage\obs-plugins\64bit"  "$obsDir\obs-plugins\64bit"  /E /XF distroav.dll /MT:16 /R:2 /W:2
if ($LASTEXITCODE -ge 8) { Write-Error "robocopy obs-plugins\64bit failed ($LASTEXITCODE)"; exit 4 }
Copy-Item -Force (Join-Path $stage 'BUNDLE_MANIFEST.json') (Join-Path $obsDir 'BUNDLE_MANIFEST.json')
PSFULL
)
  else
    copy_block=$(cat <<'PSFAST'
# (4) FAST (libobs-only) deploy: swap obs.dll ONLY -- no bulk copy of data\ / obs-plugins\ (a
#     libobs-only change has no other changed files). obs.dll can stay locked past a short sleep
#     after Stop-Process, so retry the copy once.
$src = Join-Path $stage 'obs.dll'
$dst = Join-Path $obsDir 'bin\64bit\obs.dll'
try { Copy-Item -Force $src $dst } catch { Start-Sleep -Seconds 5; Copy-Item -Force $src $dst }
PSFAST
)
  fi
  # #1115 (Option A): FULL mode ALSO deploys the bundle's genlock distroav.dll to the REAL OBS load
  # path in ProgramData (backup + fail-closed byte verify), so the LOADED plugin is the canonical
  # build and the byte-parity gather/compare against the manifest becomes real. FAST is obs.dll-only
  # (the fast bundle carries no distroav.dll), so the block is a no-op there. Quoted heredoc =>
  # literal PowerShell (same pattern as copy_block); $backupDir/$manifest come from steps 3/6.
  local distroav_deploy_block
  if [ "$mode" = "full" ]; then
    distroav_deploy_block=$(cat <<'PSDISTROAV'
# (6b) #1115 -- deploy the bundle's genlock distroav.dll to the REAL OBS load path and byte-verify it
#      (Option A). OBS loads DistroAV ONLY from C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\
#      distroav.dll (NEVER Program Files\obs-plugins\64bit -- that copy stays /XF-excluded above as a
#      shadow drift-guard #124 flags). The bundle->ProgramData layout is NOT 1:1 (bundle path
#      obs-plugins/64bit/distroav.dll; on-box plugins\distroav\bin\64bit\distroav.dll), so this is an
#      EXPLICIT path-mapped copy of the ONE DLL the byte-parity gather (bundle-state-server.py, the
#      ProgramData first-located copy) + compare (drift-guard.sh, matched by basename) hash -- never a
#      bulk copy. The distroav data\ tree is left unmanaged on purpose: the byte-parity gate is
#      DLL-scoped and DistroAV data is stable across genlock rebuilds at the pinned 6.2.1.
$pdDistroav  = 'C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll'
$srcDistroav = Join-Path $stage 'obs-plugins\64bit\distroav.dll'
if (-not (Test-Path $srcDistroav)) { Write-Error "staged distroav.dll not found at $srcDistroav -- the FULL bundle must carry it"; exit 4 }
if (-not (Test-Path $pdDistroav))  { Write-Error "ProgramData distroav load path $pdDistroav not found -- DistroAV is not installed where OBS loads it"; exit 4 }
# back up the pre-deploy ProgramData distroav.dll alongside obs.dll.pre-789 (instant rollback).
Copy-Item -Force $pdDistroav (Join-Path $backupDir 'distroav.dll.pre-789') -ErrorAction SilentlyContinue
# explicit path-mapped copy: staged bundle obs-plugins\64bit\distroav.dll -> the ProgramData load path.
Copy-Item -Force $srcDistroav $pdDistroav
# byte-verify the DEPLOYED ProgramData distroav.dll vs the manifest's distroav entry (matched by
# basename: manifest path obs-plugins/64bit/distroav.dll == the on-box ProgramData DLL), fail-closed --
# proof the canonical genlock distroav actually landed (mirrors the obs.dll verify in step 6).
if (Test-Path $manifest) {
  $md = Get-Content $manifest -Raw | ConvertFrom-Json
  $wantD = ($md.files | Where-Object { $_.path -match '(^|/)distroav\.dll$' } | Select-Object -First 1).sha256
  $gotD  = (Get-FileHash -Algorithm SHA256 $pdDistroav).Hash.ToLower()
  $matchD = ($wantD -and ($gotD -eq $wantD.ToLower()))
  Write-Host "VERIFY distroav.dll want=$wantD got=$gotD match=$matchD"
  if (-not $matchD) { Write-Error "VERIFY FAIL: deployed distroav.dll bytes do not match the bundle manifest -- do NOT trust this box"; exit 4 }
} else {
  Write-Warning "no BUNDLE_MANIFEST.json in $stage -- cannot byte-verify distroav.dll (marker-only)."
}
PSDISTROAV
)
  else
    distroav_deploy_block='# (6b) #1115: distroav ProgramData deploy -- no-op on a FAST (obs.dll-only) deploy (no distroav in the fast bundle).'
  fi

  cat <<PS
# ===== issue 789 genlock FLEET deploy -- box=${box} mode=${mode} (paste into the ${box} win-* MCP Shell) =====
# The genlock build is env-free (render tick + ts-align build defaults, latency 3 ms const, burn a
# per-source WS bool) -- this program carries NO OBS_GENLOCK_*/OBS_BURN_* env. It STOPS OBS to swap
# the bytes; RELAUNCH is a SEPARATE step via scripts/launch-obs-genlock.sh (STEP 2 of the plan) so
# obs64 comes up in the interactive session (win-ssh-vs-mcp), never session 0.
\$ErrorActionPreference = 'Stop'
\$stage       = '${stage_ps}'
\$obsDir      = '${obs_ps}'
\$backupRoot  = '${back_ps}'
\$fleetConfirmRetention = ${confirm_ps}   # --yes wires this to \$true; default \$false = PLAN ONLY.

# (0) preflight -- the staged bundle must already be on the box (STEP 0 of the plan uploads it).
if (-not (Test-Path \$stage))  { Write-Error "stage \$stage not found -- upload the artifact first (plan STEP 0)"; exit 2 }
if (-not (Test-Path \$obsDir)) { Write-Error "OBS install \$obsDir not found"; exit 2 }

${ahk_stop}

${keepalive_disable}

# (2) Stop obs64 (+ obs-browser-page) and clear crash sentinels so the swap's files are unlocked and
#     OBS does not pop the Crash-Detected modal on relaunch. obs.dll's handle can outlive a short
#     sleep, so wait a bit.
Get-Process obs64,obs-browser-page -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 5
Remove-Item "\$env:APPDATA\\obs-studio\\.sentinel\\*" -Force -ErrorAction SilentlyContinue

# (3) Back up the components we are about to overwrite (instant rollback), one dated dir per deploy.
\$stamp     = Get-Date -Format 'yyyy-MM-ddTHH-mm-ss'
\$backupDir = Join-Path \$backupRoot "\$stamp-789"
New-Item -ItemType Directory -Force -Path \$backupDir | Out-Null
Copy-Item -Force (Join-Path \$obsDir 'bin\\64bit\\obs.dll') (Join-Path \$backupDir 'obs.dll.pre-789') -ErrorAction SilentlyContinue
Copy-Item -Force (Join-Path \$obsDir 'GENLOCK_BUILD_SHA.txt') (Join-Path \$backupDir 'GENLOCK_BUILD_SHA.txt.pre-789') -ErrorAction SilentlyContinue

${copy_block}

# (5) Write BOTH markers + DEPLOYED_AT atomically (temp-then-rename), mirroring the shared
#     genlock_write_markers helper (scripts/lib/genlock-markers.sh) on the Linux legs.
function Write-MarkerAtomic(\$dest, \$content) {
  \$tmp = "\$dest.tmp.\$PID"
  Set-Content -Path \$tmp -Value \$content -Encoding ascii
  Move-Item -Force \$tmp \$dest
}
Write-MarkerAtomic (Join-Path \$obsDir 'GENLOCK_BUILD_SHA.txt')  '${gsha_ps}'
Write-MarkerAtomic (Join-Path \$obsDir 'DISTROAV_BUILD_SHA.txt') '${dsha_ps}'
Write-MarkerAtomic (Join-Path \$obsDir 'DEPLOYED_AT')            (Get-Date -Format o)

# (6) sha256 verify the DEPLOYED obs.dll bytes against the bundle manifest (fail-closed) -- proof the
#     right build actually landed, not just that the copy step said ok (the #122/#121 deploy contract).
\$manifest = Join-Path \$stage 'BUNDLE_MANIFEST.json'
\$deployedObs = Join-Path \$obsDir 'bin\\64bit\\obs.dll'
if (Test-Path \$manifest) {
  \$m = Get-Content \$manifest -Raw | ConvertFrom-Json
  \$want = (\$m.files | Where-Object { \$_.path -match '(^|/)obs\\.dll\$' } | Select-Object -First 1).sha256
  \$got  = (Get-FileHash -Algorithm SHA256 \$deployedObs).Hash.ToLower()
  \$match = (\$want -and (\$got -eq \$want.ToLower()))
  Write-Host "VERIFY obs.dll want=\$want got=\$got match=\$match"
  if (-not \$match) { Write-Error "VERIFY FAIL: deployed obs.dll bytes do not match the bundle manifest -- do NOT trust this box"; exit 4 }
} else {
  Write-Warning "no BUNDLE_MANIFEST.json in \$stage -- cannot byte-verify obs.dll (fast deploy); marker-only proof."
}

${distroav_deploy_block}

# (7) box-backup RETENTION PLAN (bod 5) -- keep the newest ${keep} dated backup dirs; list the rest
#     as "would delete". Deletion happens ONLY when \$fleetConfirmRetention is \$true (never silent).
Write-Host "RETENTION PLAN (keep newest ${keep} of \$backupRoot\\*-789):"
\$stale = @(Get-ChildItem \$backupRoot -Directory -ErrorAction SilentlyContinue | Where-Object { \$_.Name -like '*-789' } | Sort-Object LastWriteTime -Descending | Select-Object -Skip ${keep})
if (\$stale.Count -eq 0) { Write-Host "  nothing to prune." }
foreach (\$s in \$stale) {
  if (\$fleetConfirmRetention) { Write-Host "  deleting \$(\$s.FullName)"; Remove-Item -Recurse -Force \$s.FullName }
  else                        { Write-Host "  would delete \$(\$s.FullName)" }
}

${ahk_restart}

${keepalive_restore}

Write-Host "#789 FLEET DEPLOY OK: ${box} swapped to ${gsha_ps} (marker written, obs.dll byte-verified). Relaunch OBS via launch-obs-genlock.sh --box ${box} --force (plan STEP 2)."
exit 0
PS
}

# build_imag_deploy_program BUNDLE_DIR MARKER_DIR BACKUP_ROOT GSHA DSHA KEEP CONFIRM
#   Emit the on-imag bash program the fleet script scp's + runs (sudo) over ssh. BUNDLE_DIR is the
#   scp'd FULL linux-genlock bundle root (bin/, lib/x86_64-linux-gnu/ incl EVERY obs-plugins/*.so,
#   share/obs/, BUNDLE_MANIFEST.json) + the shared genlock-markers.sh. It sha256-VERIFIES the staged
#   key files against the bundle manifest (fail-closed), installs the WHOLE bundle -- NOT a hand-picked
#   4-file list: on Linux the plugins link against libobs's INTERNAL struct layout, so a stale
#   obs-plugins/*.so over a new libobs.so.30 is a latent SIGSEGV (issue 1026 -- the exact overnight
#   OBS-crash + genlock_parity-refusal class #789 exists to end) -- runs the SONAME (libobs +
#   libobs-opengl) + #499 nm ABI guards, writes the markers via the SHARED genlock_write_markers
#   helper, applies retention (delete only when CONFIRM=1), restarts OBS via the existing
#   imag-obs-stop.sh/imag-obs-start.sh helpers, and directs the mandatory 1026 WS filter-enum survival
#   check. The markers are the shared single source of truth across the deploy legs.
build_imag_deploy_program() {
  local bundle_dir="$1" marker_dir="$2" backup_root="$3" gsha="$4" dsha="$5" keep="$6" confirm="${7:-0}"
  # Escape ' for the emitted bash single-quoted strings (' -> '\'') -- parity with the Windows leg's
  # #789 review #7 defense-in-depth (--sha is operator-supplied and flows into the marker call).
  local gsha_bq="${gsha//"'"/"'\''"}" dsha_bq="${dsha//"'"/"'\''"}"
  local del_line='  echo "  would delete $d"'
  [ "$confirm" = "1" ] && del_line='  echo "  deleting $d"; rm -rf "$d"'
  cat <<EOS
#!/usr/bin/env bash
# issue 789 on-imag genlock deploy (run sudo over ssh). Ships the FULL bundle (issue 1026 -- a
# hand-picked subset over a new libobs is a latent SIGSEGV); markers via the SHARED
# scripts/lib/genlock-markers.sh helper scp'd into the bundle.
set -euo pipefail
BUNDLE='${bundle_dir}'
MARKER_DIR='${marker_dir}'
BACKUP_ROOT='${backup_root}'
KEEP='${keep}'
MANIFEST="\$BUNDLE/BUNDLE_MANIFEST.json"
LIBDIR='/usr/lib/x86_64-linux-gnu'
LIBOBS_REAL="\$LIBDIR/libobs.so.30"
DISTROAV_REAL="\$LIBDIR/obs-plugins/distroav.so"
OBS_FRONTEND_REAL='/usr/bin/obs'
LIBOBS_OPENGL_REAL="\$LIBDIR/libobs-opengl.so.30"

# shared marker helper (single source of truth; scp'd into the bundle)
. "\$BUNDLE/genlock-markers.sh"

command -v readelf >/dev/null 2>&1 && command -v nm >/dev/null 2>&1 || { echo "readelf/nm (binutils) missing" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq missing (needed for the manifest sha256 verify)" >&2; exit 1; }
[ -f "\$MANIFEST" ] || { echo "bundle missing BUNDLE_MANIFEST.json -- refuse an unverifiable deploy" >&2; exit 4; }

# (1) sha256 VERIFY the staged key files against the bundle manifest BEFORE installing (fail-closed;
#     the #121/#122 deploy contract -- a corrupt scp/download never reaches the live install).
verify_sha() {  # RELPATH  STAGED_FILE
  local relpath="\$1" f="\$2" want got
  want="\$(jq -r --arg p "\$relpath" '.files[] | select(.path == \$p) | .sha256' "\$MANIFEST")"
  [ -n "\$want" ] || { echo "manifest lists no \$relpath -- refuse an unverifiable file" >&2; exit 4; }
  [ -f "\$f" ] || { echo "staged bundle missing \$f" >&2; exit 4; }
  got="\$(sha256sum "\$f" | awk '{print \$1}')"
  [ "\$want" = "\$got" ] || { echo "sha256 MISMATCH for \$relpath (want \$want got \$got) -- refuse to deploy" >&2; exit 4; }
}
verify_sha 'lib/x86_64-linux-gnu/libobs.so.30'                "\$BUNDLE/lib/x86_64-linux-gnu/libobs.so.30"
verify_sha 'lib/x86_64-linux-gnu/libobs-opengl.so.30'         "\$BUNDLE/lib/x86_64-linux-gnu/libobs-opengl.so.30"
verify_sha 'bin/obs'                                          "\$BUNDLE/bin/obs"
verify_sha 'lib/x86_64-linux-gnu/obs-plugins/distroav.so'    "\$BUNDLE/lib/x86_64-linux-gnu/obs-plugins/distroav.so"

# (2) back up the components we overwrite (instant rollback), one dated dir + the 'previous' dir
#     setup-imag.sh/drift-guard expect.
STAMP="\$(date +%Y-%m-%dT%H-%M-%S)"
BK="\$BACKUP_ROOT/\$STAMP-789"
mkdir -p "\$BK" "\$BACKUP_ROOT/previous"
for f in "\$LIBOBS_REAL" "\$DISTROAV_REAL" "\$OBS_FRONTEND_REAL" "\$LIBOBS_OPENGL_REAL"; do
  [ -f "\$f" ] && cp -a "\$f" "\$BK/\$(basename "\$f").pre-789"
  [ -f "\$f" ] && cp -a "\$f" "\$BACKUP_ROOT/previous/\$(basename "\$f")"
done

# (3) install the WHOLE bundle (issue 1026): libobs + libobs-opengl + EVERY obs-plugins/*.so built
#     together + the frontend exe + share/obs data -- never a hand-picked subset. cp -a merges the
#     bundle's OBS libs into the system dir (nothing else lives under obs-plugins/); (3a) normalizes
#     the perms + ownership afterward (issue 1236) so the CI runner's uid/mode never leaks onto /usr.
cp -a "\$BUNDLE/lib/x86_64-linux-gnu/." "\$LIBDIR/" || { echo "install of the bundle libs failed" >&2; exit 4; }
# (3a) NORMALIZE the just-installed payload deterministically (issue 1236). GNU cp -a with the src/.
#      operand stamps the SOURCE dir's mode+ownership onto the DESTINATION: a 0700 newlevel mktemp
#      staging dir made \$LIBDIR itself drwx------ newlevel:newlevel and installed 0700 root:root libs,
#      so a runtime uid could not open libobs.so.30 (imag-obs.service flapped; the supervised verify
#      refused, exit 4). Hold REGARDLESS of the staging perms: reset \$LIBDIR root:root 0755, then
#      chown root:root + set dirs 0755 / files a+rX over EVERY installed path (walk the bundle source
#      tree -- scope to the just-installed set, never a whole-libdir sweep).
chown root:root "\$LIBDIR"; chmod 0755 "\$LIBDIR"
while IFS= read -r -d '' rel; do
  dst="\$LIBDIR/\$rel"
  [ -e "\$dst" ] || continue
  chown root:root "\$dst" 2>/dev/null || true
  if [ -d "\$dst" ]; then chmod 0755 "\$dst" 2>/dev/null || true; else chmod a+rX "\$dst" 2>/dev/null || true; fi
done < <(cd "\$BUNDLE/lib/x86_64-linux-gnu" && find . -mindepth 1 -printf '%P\0')
install -m 0755 -o root -g root "\$BUNDLE/bin/obs" "\$OBS_FRONTEND_REAL" || { echo "frontend /usr/bin/obs install failed" >&2; exit 4; }
if [ -d "\$BUNDLE/share/obs" ]; then
  mkdir -p /usr/share/obs
  cp -a "\$BUNDLE/share/obs/." /usr/share/obs/     # (3a, issue 1236) normalize the share subtree too (OBS-only dir)
  chown -R root:root /usr/share/obs 2>/dev/null || true
  chmod 0755 /usr/share/obs
  find /usr/share/obs -type d -exec chmod 0755 {} + 2>/dev/null || true
  find /usr/share/obs -type f -exec chmod a+rX {} + 2>/dev/null || true
fi
ldconfig

# (3b) fail-closed post-install perms assert (issue 1236): \$LIBDIR itself MUST be root:root 0755,
#      and EVERY just-installed path (the libs AND the share/obs data) MUST be root:root with dirs
#      world-traversable (o+rx) / files world-readable (o+r) -- else a runtime uid cannot open
#      libobs.so.30 or read OBS data. Refuse the restart (the SONAME/manifest fail-loud spirit; scoped
#      to the just-installed set so it also catches a chown/chmod the normalize step swallowed with
#      2>/dev/null || true).
_libdir_owner="\$(stat -c '%U:%G' "\$LIBDIR" 2>/dev/null || echo '?')"
_libdir_mode="\$(stat -c '%a' "\$LIBDIR" 2>/dev/null || echo '?')"
[ "\$_libdir_owner" = "root:root" ] || { echo "#789 IMAG FAIL: post-install perms assert: \$LIBDIR owner \$_libdir_owner, want root:root (issue 1236)" >&2; exit 4; }
[ "\$_libdir_mode" = "755" ] || { echo "#789 IMAG FAIL: post-install perms assert: \$LIBDIR mode \$_libdir_mode, want 755 (issue 1236)" >&2; exit 4; }
_perms_bad=0
assert_installed_perms() {  # SRC_ROOT  DST_ROOT -- walk the source tree, assert each dest path
  local src="\$1" dst_root="\$2" rel dst own
  while IFS= read -r -d '' rel; do
    dst="\$dst_root/\$rel"
    [ -e "\$dst" ] || continue
    own="\$(stat -c '%U:%G' "\$dst" 2>/dev/null || echo '?')"
    [ "\$own" = "root:root" ] || { echo "#789 IMAG FAIL: post-install perms assert: \$dst owner \$own, want root:root (issue 1236)" >&2; _perms_bad=1; }
    if [ -d "\$dst" ]; then
      [ -n "\$(find "\$dst" -maxdepth 0 -perm -o+rx -print 2>/dev/null)" ] || { echo "#789 IMAG FAIL: post-install perms assert: dir \$dst not world-traversable (issue 1236)" >&2; _perms_bad=1; }
    else
      [ -n "\$(find "\$dst" -maxdepth 0 -perm -o+r -print 2>/dev/null)" ] || { echo "#789 IMAG FAIL: post-install perms assert: \$dst is not world-readable (issue 1236)" >&2; _perms_bad=1; }
    fi
  done < <(cd "\$src" && find . -mindepth 1 -printf '%P\0')
}
assert_installed_perms "\$BUNDLE/lib/x86_64-linux-gnu" "\$LIBDIR"
if [ -d "\$BUNDLE/share/obs" ]; then assert_installed_perms "\$BUNDLE/share/obs" /usr/share/obs; fi
[ "\$_perms_bad" = 0 ] || { echo "#789 IMAG FAIL: just-installed payload perms assert failed -- refuse the restart (issue 1236)" >&2; exit 4; }

# (4) ABI guards -- refuse a mismatched SONAME (libobs AND the SEPARATE libobs-opengl, #756) or a
#     stock/wrong frontend. No grep -q on a piped external cmd under pipefail (the setup-imag.sh
#     SIGPIPE footgun); read the full output.
readelf -d "\$LIBOBS_REAL" 2>/dev/null | grep 'SONAME.*\[libobs\.so\.30\]' >/dev/null \
  || { echo "post-swap libobs.so.30 SONAME check failed -- refuse a mismatched ABI" >&2; exit 4; }
readelf -d "\$LIBOBS_OPENGL_REAL" 2>/dev/null | grep 'SONAME.*\[libobs-opengl\.so\.30\]' >/dev/null \
  || { echo "post-swap libobs-opengl.so.30 SONAME check failed -- refuse a mismatched ABI (#756)" >&2; exit 4; }
nm -D -u "\$OBS_FRONTEND_REAL" 2>/dev/null | grep 'obs_display_set_render_divisor' >/dev/null \
  || { echo "post-swap /usr/bin/obs does not reference obs_display_set_render_divisor -- refuse a stock/wrong frontend (#499)" >&2; exit 4; }

# (5) markers via the SHARED helper (bod 4 single source of truth)
genlock_write_markers "\$MARKER_DIR" '${gsha_bq}' '${dsha_bq}'
cp -a "\$MANIFEST" "\$MARKER_DIR/BUNDLE_MANIFEST.json"

# (6) box-backup RETENTION (bod 5) -- keep the newest \$KEEP dated dirs; delete the rest ONLY when the
#     operator confirmed (--yes), else print the plan.
echo "RETENTION PLAN (keep newest \$KEEP of \$BACKUP_ROOT/*-789):"
genlock_retention_delete_plan "\$KEEP" "\$BACKUP_ROOT" '*-789' | while IFS= read -r d; do
${del_line}
done

# (7) supervised OBS restart THROUGH the durable systemd USER unit (issue 789 handoff residual).
#     The first live fleet run (2026-08-18) background-launched imag-obs-start.sh OUTSIDE the
#     imag-obs.service cgroup: no Restart=on-failure, ExecStop= ladder bypassed, the launch tied to
#     the ssh session -- it died in ~21s and needed a hand systemctl --user restart. imag-obs.service
#     is a USER unit (issue 998): over a non-login ssh sudo the user bus needs XDG_RUNTIME_DIR
#     exported; we act as \${SUDO_USER:-newlevel}. Stop-then-pkill guarantees the OLD obs (old libobs
#     still mapped -- the #912 stop-race; and any stray from a prior background-launch deploy) is gone
#     before the fresh supervised start. This mirrors the strih AHK stop->verify->relaunch ordering.
IMAG_USER="\${SUDO_USER:-newlevel}"
IMAG_UID="\$(id -u "\$IMAG_USER")"
# env sets XDG_RUNTIME_DIR + DBUS in the CHILD, bypassing sudo env_reset (a bare sudo -u user VAR=val
# form can be stripped by sudoers env policy -- a stripped XDG_RUNTIME_DIR silently loses the user bus
# and re-creates the exact unsupervised-launch failure this fix exists to kill; DBUS mirrors
# setup-imag.sh's live-proven u_systemctl). NO backticks in this heredoc body -- unquoted EOS would
# command-substitute them at emit time.
uctl() { sudo -u "\$IMAG_USER" env XDG_RUNTIME_DIR="/run/user/\$IMAG_UID" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/\$IMAG_UID/bus" systemctl --user "\$@"; }
OBS_START_LOG="/tmp/imag-obs-start.log"
prev_log_lines="\$(wc -l < "\$OBS_START_LOG" 2>/dev/null || echo 0)"
uctl stop imag-obs.service 2>/dev/null || true
pkill -9 -x obs 2>/dev/null || true
sleep 2
rm -f "/home/\$IMAG_USER/.config/obs-studio/.sentinel/"* 2>/dev/null || true
uctl reset-failed imag-obs.service 2>/dev/null || true
uctl restart imag-obs.service || { echo "#789 IMAG FAIL: systemctl --user restart imag-obs.service failed -- the supervised unit did not come up" >&2; exit 4; }

# (7a) bounded supervised-verify: the unit reports active AND the RUNNING obs lives inside the unit
#      cgroup (verify-imag.sh issue-1015 discriminator -- systemd bookkeeping can say active while
#      the real obs sits OUTSIDE the cgroup, launched by a bypassed path). All THREE checks share ONE
#      retry loop: imag-obs.service is Type=simple, so is-active flips true the moment the ExecStart
#      WRAPPER forks -- BEFORE it forks obs itself (it logs + clears sentinels + reads the CPU pin
#      first), so a single un-retried pgrep right after is-active races the wrapper->obs fork and can
#      false-exit-4 on a healthy bringup. Poll all three together instead. If a stray survived the
#      kill, the wrapper's own idempotent guard (it exits 0 when obs already runs) makes ExecStart a
#      no-op -> the unit goes
#      inactive -> is-active stays false -> the loop times out -> exit 4 (never a silent unsupervised
#      pass). obs_pid stays visible to the record echo below.
imag_supervised=0
obs_pid=""
for _ in \$(seq 1 30); do
  if uctl is-active --quiet imag-obs.service; then
    obs_pid="\$(pgrep -x obs | head -n1 || true)"
    if [ -n "\$obs_pid" ] && grep -q imag-obs.service "/proc/\$obs_pid/cgroup" 2>/dev/null; then imag_supervised=1; break; fi
  fi
  sleep 2
done
[ "\$imag_supervised" = 1 ] || { echo "#789 IMAG FAIL: imag-obs.service did not come up SUPERVISED (active + obs in its cgroup) after restart (obs_pid=\${obs_pid:-none}) -- refuse an unsupervised deploy (issue 789 was exactly this)" >&2; exit 4; }

# (7b) render-tick log verify the launch contract demands: imag-obs.service's ExecStart
#      (imag-obs-start.sh) writes /tmp/imag-obs-start.log and prints 'OK: OBS bezi' only AFTER the WS
#      came up + scenes seeded -- the on-box proof the fresh instance actually rendered, the imag
#      analogue of the Windows leg's render-tick verify. Read only the tail PAST prev_log_lines so a
#      STALE 'OK: OBS bezi' from a previous start cannot false-pass. Budget 75x2=150s comfortably
#      EXCEEDS imag-obs-start.sh's own 90s WS deadline + the sleep/bootstrap/projector seed after it
#      (a false FAIL on a slow-but-healthy bringup would be worse than the wait). Buffer-then-match,
#      never a tail-piped-into-grep-q -- a SIGPIPE'd tail under pipefail can drop a real match (the same-file
#      SIGPIPE footgun the ABI-guard step above already avoids).
imag_tick=0
for _ in \$(seq 1 75); do
  new_log="\$(tail -n +\$((prev_log_lines + 1)) "\$OBS_START_LOG" 2>/dev/null || true)"
  case "\$new_log" in *"OK: OBS bezi"*) imag_tick=1; break ;; esac
  pgrep -x obs >/dev/null 2>&1 || break
  sleep 2
done
[ "\$imag_tick" = 1 ] || { echo "#789 IMAG FAIL: imag-obs-start.log never reached 'OK: OBS bezi' after the swap -- OBS did not render/seed" >&2; exit 4; }
echo "#789 IMAG DEPLOY OK: swapped to ${gsha}; obs (pid \$obs_pid) SUPERVISED in imag-obs.service, WS up + scenes seeded:"
ps -o pid,lstart,cmd -C obs 2>/dev/null || true

# (8) MANDATORY 1026 survival check (dev1-side WS op -- run AFTER this program returns): exercise a
#     WS filter-enum to prove the previously-crashing get_const_root <- obs_enum path survives the
#     full-bundle swap:  python3 scripts/obs_burn_filter.py check --host <imag-ip>   (from dev1).
echo "#789 NEXT (issue 1026): from dev1 run 'python3 scripts/obs_burn_filter.py check --host <imag-ip>' to prove the WS filter-enum path survives."
EOS
}

# fleet_log_line RUN SHA BOXES MODE -> one tab-separated durable record for the fleet-deploy log.
fleet_log_line() {
  local run="${1:-}" sha="${2:-}" boxes="${3:-}" mode="${4:-}"
  printf '%s\t%s\t%s\t%s\t%s\n' "$(date -Is)" "$run" "$sha" "$boxes" "$mode"
}

# ============================================================================================
# source-guard: when sourced (the unit tests), stop here.
# ============================================================================================
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# ============================================================================================
# flow (executed only when run directly).
# ============================================================================================

usage() {
  cat <<'EOF'
deploy-genlock-fleet.sh -- ONE deploy path for the OBS genlock build across strih + stream + imag
from ONE anchor CI run id (issue 789 bod 4 + bod 5). Resolves the same-SHA artifacts, emits the
Windows deploy program for the win-* MCP Shell, ssh-deploys imag, applies box-backup retention, and
writes one durable fleet-deploy log line.

Usage (plan -- print the whole fleet plan, no network, from a pre-staged bundle):
  scripts/deploy-genlock-fleet.sh --plan --run-id <id> --sha <headSha> --stage <dir> \
      [--full|--fast] [--boxes strih,stream,imag]

Usage (execute -- resolve + download the same-SHA artifacts, emit the Windows plan, ssh-deploy imag):
  scripts/deploy-genlock-fleet.sh --run-id <id> [--full|--fast] [--boxes strih,stream,imag] [--yes]

  --run-id   the ANCHOR CI run id -- its headSha is the ONE canonical version every box converges to.
  --full     full windows-genlock bundle (default; required for any plugin/frontend change).
  --fast     libobs-only obs.dll hot-swap.
  --boxes    comma list of strih,stream,imag (default: all three).
  --plan     print the plan only; no gh/ssh/scp. Requires --stage + --sha.
  --stage    local dir holding the (pre-)downloaded artifact bytes.
  --sha      the canonical build SHA to stamp into the markers (plan mode).
  --yes      (execute mode) auto-confirm the box-backup retention deletions.

Exit codes: 0 ok, 2 usage error, 3 resolution/download failure, 4 a box deploy failed.
EOF
}

# emit the plan for one Windows box (STEP 0 upload -> the deploy program -> STEP 2 relaunch).
emit_windows_plan() {
  local box="$1" mode="$2" stage="$3" gsha="$4" dsha="$5" confirm="${6:-0}"
  local mcp ip has_ahk win_stage program artifact
  mcp="$(fleet_box_mcp "$box")"; ip="$(fleet_box_ip "$box")"; has_ahk="$(fleet_box_has_ahk "$box")"
  artifact="$(fleet_windows_artifact "$mode")"
  win_stage="C:\\stage-genlock-${gsha}"
  program="$(build_windows_deploy_program "$box" "$mode" "$win_stage" 'C:\Program Files\obs-studio' "$has_ahk" 'C:\obs-backup' "$RETENTION_KEEP" "$gsha" "$dsha" "$confirm")"
  cat <<PLAN
# ================= FLEET PLAN: box=${box} (${mcp}, ${ip}) mode=${mode} =================
# STEP 0 (once per box): upload the downloaded '${artifact}' bytes (staged locally at ${stage}) to
#         the box at ${win_stage} via the ${mcp} MCP FileUpload (or sshpass scp -O of a zip +
#         Expand-Archive on the box), then run the program below in the ${mcp} MCP Shell
#         (timeout >= 240 s):
# ----------------------------------------------------------------------------------------------------
${program}
# ----------------------------------------------------------------------------------------------------
# STEP 2 (relaunch): once the program exits 0, relaunch OBS in the interactive session (NEVER over
#         ssh -- win-ssh-vs-mcp):  bash scripts/launch-obs-genlock.sh --box ${box} --force
#         -> paste its emitted program into the ${mcp} MCP Shell.
PLAN
}

# emit the imag plan (STEP 0 scp -> run the on-imag program over ssh).
emit_imag_plan() {
  local stage="$1" gsha="$2" dsha="$3" confirm="${4:-0}"
  local imag_stage="/tmp/genlock-stage-${gsha}"
  local program
  program="$(build_imag_deploy_program "$imag_stage" '/opt/obs-genlock' '/opt/obs-backup' "$gsha" "$dsha" "$RETENTION_KEEP" "$confirm")"
  cat <<PLAN
# ================= FLEET PLAN: box=imag (ssh newlevel@imag) =================
# STEP 0: scp the FULL linux-genlock bundle (bin/, lib/x86_64-linux-gnu/ incl EVERY obs-plugins/*.so,
#         share/obs/, BUNDLE_MANIFEST.json -- issue 1026: never a hand-picked subset) + the shared
#         scripts/lib/genlock-markers.sh from ${stage} to imag:${imag_stage}, then run the program
#         below over ssh (sudo, DISPLAY=:0):
# ----------------------------------------------------------------------------------------------------
${program}
# ----------------------------------------------------------------------------------------------------
PLAN
}

main() {
  local plan=0 run_id="" mode="" boxes_csv="" stage="" sha_override="" yes=0
  local mode_full=0 mode_fast=0
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --plan)    plan=1; shift ;;
      --run-id)  need_val "$@"; run_id="$2"; shift 2 ;;
      --full)    mode_full=1; shift ;;
      --fast)    mode_fast=1; shift ;;
      --boxes)   need_val "$@"; boxes_csv="$2"; shift 2 ;;
      --stage)   need_val "$@"; stage="$2"; shift 2 ;;
      --sha)     need_val "$@"; sha_override="$2"; shift 2 ;;
      --yes)     yes=1; shift ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  if [ "$mode_full" = 1 ] && [ "$mode_fast" = 1 ]; then
    echo "ERROR: --full and --fast are mutually exclusive" >&2; exit 2
  fi
  mode="full"; [ "$mode_fast" = 1 ] && mode="fast"

  [ -n "$run_id" ] || { echo "ERROR: --run-id is required" >&2; usage >&2; exit 2; }

  local boxes; boxes="$(fleet_normalize_boxes "$boxes_csv")" || exit 2

  if [ "$plan" = 1 ]; then
    [ -n "$stage" ] || { echo "ERROR: --plan requires --stage (no network in plan mode)" >&2; exit 2; }
    [ -n "$sha_override" ] || { echo "ERROR: --plan requires --sha (no network in plan mode)" >&2; exit 2; }
    local sha="$sha_override"
    echo "# ===== issue 789 genlock FLEET deploy PLAN — run=${run_id} sha=${sha} boxes=${boxes} mode=${mode} ====="
    # subshell so the comma-split IFS never leaks past the loop (the loop only prints).
    ( IFS=','; for b in $boxes; do
      case "$b" in
        strih|stream) emit_windows_plan "$b" "$mode" "$stage" "$sha" "$sha" ;;
        imag)         emit_imag_plan "$stage" "$sha" "$sha" ;;
      esac
    done )
    echo "# fleet-deploy log line (append to ${FLEET_LOG_DEFAULT} once the deploy is done):"
    fleet_log_line "$run_id" "$sha" "$boxes" "$mode"
    exit 0
  fi

  # --- execute mode -------------------------------------------------------------------------------
  # Resolve the anchor headSha, resolve + download the SAME-SHA artifacts across the (separate)
  # windows/linux workflows, emit the Windows plan (agent uploads + pastes into the win-* MCP Shell),
  # ssh-deploy imag, and append one fleet-deploy log line. The worker never runs this; the supervisor
  # drives it live (the ticket stays OPEN until then).
  command -v gh >/dev/null 2>&1 || { echo "ERROR: gh CLI required for execute mode" >&2; exit 3; }
  command -v jq >/dev/null 2>&1 || { echo "ERROR: jq required for execute mode" >&2; exit 3; }
  local sha; sha="$(gh run view "$run_id" --repo "$GENLOCK_REPO" --json headSha -q .headSha 2>/dev/null)" \
    || { echo "ERROR: could not resolve headSha for anchor run $run_id" >&2; exit 3; }
  [ -n "$sha" ] || { echo "ERROR: empty headSha for anchor run $run_id" >&2; exit 3; }
  echo "# anchor run $run_id -> canonical SHA $sha; boxes: $boxes (mode $mode; retention --yes=$yes)"

  local workdir; workdir="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$workdir'" EXIT

  # detect requested box classes without a `local IFS=','` that would leak past a brace group
  # (a { …; } group is not a new scope) -- match the comma-list directly.
  local want_win=0 want_imag=0
  case ",$boxes," in *,strih,*|*,stream,*) want_win=1 ;; esac
  case ",$boxes," in *,imag,*) want_imag=1 ;; esac

  # --- Windows: download the same-SHA artifact, emit the per-box plan (agent uploads + pastes) -----
  if [ "$want_win" = 1 ]; then
    local win_wf win_art win_run
    win_wf="$(fleet_windows_workflow "$mode")"; win_art="$(fleet_windows_artifact "$mode")"
    win_run="$(gh run list --repo "$GENLOCK_REPO" --workflow "$win_wf" --json databaseId,headSha,conclusion --limit 50 2>/dev/null | fleet_pick_run_at_sha "$sha")" || true
    [ -n "$win_run" ] || { echo "ERROR: no successful $win_wf run at $sha -- build the Windows artifact at that commit first (gh workflow run / a tag dispatch)" >&2; exit 3; }
    gh run download "$win_run" --repo "$GENLOCK_REPO" -n "$win_art" -D "$workdir/win" \
      || { echo "ERROR: download of $win_art (run $win_run) failed" >&2; exit 3; }
    echo "# Windows artifact $win_art (run $win_run) downloaded to $workdir/win"
    echo "# Upload it to each box at C:\\stage-genlock-$sha (win-* MCP FileUpload / scp), then paste each program:"
    # subshell so the comma-split IFS never leaks into the rest of main (the loop only prints).
    ( IFS=','; for b in $boxes; do case "$b" in strih|stream) emit_windows_plan "$b" "$mode" "$workdir/win" "$sha" "$sha" "$yes" ;; esac; done )
  fi

  # --- imag: download the same-SHA linux artifacts, scp the WHOLE bundle + ssh-run (issue 1026) -----
  if [ "$want_imag" = 1 ]; then
    local lrun
    lrun="$(gh run list --repo "$GENLOCK_REPO" --workflow linux-genlock.yml --json databaseId,headSha,conclusion --limit 50 2>/dev/null | fleet_pick_run_at_sha "$sha")" || true
    [ -n "$lrun" ] || { echo "ERROR: no successful linux-genlock.yml run at $sha -- build the imag artifact at that commit first (a tag dispatch, per the issue-923 recipe)" >&2; exit 3; }
    gh run download "$lrun" --repo "$GENLOCK_REPO" -n "$(fleet_linux_bundle_artifact)"   -D "$workdir/bundle" \
      || { echo "ERROR: download of the linux bundle (run $lrun) failed" >&2; exit 3; }
    gh run download "$lrun" --repo "$GENLOCK_REPO" -n "$(fleet_linux_distroav_artifact)" -D "$workdir/fast" \
      || { echo "ERROR: download of the linux distroav (run $lrun) failed" >&2; exit 3; }
    # Overlay the freshly-built fast distroav.so into the bundle tree (byte-identical to the bundle's
    # own per setup-imag's cross-check, but this pins the compile-check job's exact output), then ship
    # the WHOLE bundle (issue 1026 -- every obs-plugins/*.so must travel with libobs). Add the shared
    # marker lib + the emitted program into the bundle root the on-imag program reads.
    [ -f "$workdir/fast/distroav.so" ] || { echo "ERROR: fast artifact missing distroav.so" >&2; exit 3; }
    [ -d "$workdir/bundle/lib/x86_64-linux-gnu/obs-plugins" ] || { echo "ERROR: bundle missing lib/x86_64-linux-gnu/obs-plugins" >&2; exit 3; }
    cp -f "$workdir/fast/distroav.so" "$workdir/bundle/lib/x86_64-linux-gnu/obs-plugins/distroav.so"
    cp "$HERE/lib/genlock-markers.sh" "$workdir/bundle/genlock-markers.sh" || { echo "ERROR: cannot stage genlock-markers.sh" >&2; exit 3; }

    # imag transport: the repo's established pattern (sshpass, IMAG_IP from scripts/imag-host.sh,
    # sudo -S with the password on stdin -- same as recording-e2e.sh / dantesync-fleet-upgrade.sh).
    command -v sshpass >/dev/null 2>&1 || { echo "ERROR: sshpass required for the imag ssh deploy (apt-get install sshpass)" >&2; exit 3; }
    local imag_ip="${IMAG_IP:-}"
    if [ -z "$imag_ip" ] && [ -f "$HERE/imag-host.sh" ]; then
      # shellcheck source=scripts/imag-host.sh
      . "$HERE/imag-host.sh"; imag_ip="${IMAG_HOST_IP:-}"
    fi
    imag_ip="${imag_ip:-10.77.9.182}"
    local imag_user="${IMAG_USER:-newlevel}" imag_pw="${IMAG_PW:-newlevel}"
    local imag_stage="/tmp/genlock-stage-$sha"
    build_imag_deploy_program "$imag_stage" '/opt/obs-genlock' '/opt/obs-backup' "$sha" "$sha" "$RETENTION_KEEP" "$yes" > "$workdir/bundle/deploy.sh"

    echo "# imag: staging the FULL bundle $workdir/bundle -> ${imag_user}@${imag_ip}:$imag_stage and running the on-imag deploy (sudo) over ssh..."
    sshpass -p "$imag_pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=12 "${imag_user}@${imag_ip}" "rm -rf '$imag_stage' && mkdir -p '$imag_stage'" \
      || { echo "ERROR: imag stage mkdir failed" >&2; exit 4; }
    # rsync, not scp: newer OpenSSH scp -O rejects a "dir/." source ("unexpected filename: .",
    # hit on the first live run 2026-08-18); rsync -a copies the bundle CONTENTS incl. dotfiles.
    sshpass -p "$imag_pw" rsync -a -e "ssh -o StrictHostKeyChecking=no" "$workdir/bundle/" "${imag_user}@${imag_ip}:$imag_stage/" \
      || { echo "ERROR: imag bundle rsync failed" >&2; exit 4; }
    sshpass -p "$imag_pw" ssh -o StrictHostKeyChecking=no "${imag_user}@${imag_ip}" "printf '%s\\n' '$imag_pw' | sudo -S -p '' bash '$imag_stage/deploy.sh'" \
      || { echo "ERROR: imag deploy failed (see the on-imag output above)" >&2; exit 4; }
    echo "# imag deploy done. Verify from a fresh ssh: ps -o pid,lstart -C obs (started after the swap) + 'render tick ENABLED' in the newest OBS log."
    echo "# issue 1026 survival check: python3 scripts/obs_burn_filter.py check --host ${imag_ip}   (run from dev1)"
  fi

  # --- durable fleet-deploy log line ---------------------------------------------------------------
  mkdir -p "$(dirname "$FLEET_LOG_DEFAULT")"
  fleet_log_line "$run_id" "$sha" "$boxes" "$mode" >> "$FLEET_LOG_DEFAULT"
  echo "# fleet-deploy log appended: $FLEET_LOG_DEFAULT"
  exit 0
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
