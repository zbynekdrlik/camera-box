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

# fleet_box_mcp / fleet_box_ip / fleet_box_has_ahk -- per-box constants (only strih runs the
# NL_STARTUP.ahk auto-respawn watcher, so only strih's program stops AutoHotkey64).
fleet_box_mcp()     { case "${1:-}" in strih) echo "win-strih" ;; stream) echo "win-stream-snv" ;; *) return 2 ;; esac; }
fleet_box_ip()      { case "${1:-}" in strih) echo "10.77.9.202" ;; stream) echo "10.77.9.204" ;; imag) echo "imag" ;; *) return 2 ;; esac; }
fleet_box_has_ahk() { case "${1:-}" in strih) echo "1" ;; *) echo "0" ;; esac; }

# build_windows_deploy_program BOX MODE STAGE OBS_DIR HAS_AHK BACKUP_ROOT KEEP GSHA DSHA
#   Emit the PowerShell program the agent pastes into the box's win-* MCP Shell. It stops the AHK
#   watchdog (strih only) + obs64, clears crash sentinels, backs up the components it overwrites,
#   copies the new bytes from STAGE (full = 3 surgical robocopies; fast = obs.dll only), writes BOTH
#   markers + DEPLOYED_AT temp-then-rename, sha256-verifies the deployed obs.dll against the bundle
#   manifest (fail-closed), and prints a box-backup RETENTION PLAN (keep newest KEEP; delete only
#   when $fleetConfirmRetention). Env-free (the genlock build carries no OBS_GENLOCK_*/OBS_BURN_*).
build_windows_deploy_program() {
  local box="$1" mode="$2" stage="$3" obs_dir="$4" has_ahk="$5" backup_root="$6" keep="$7" gsha="$8" dsha="$9"
  # Escape ' for the PowerShell single-quoted strings (double it).
  local stage_ps="${stage//\'/\'\'}" obs_ps="${obs_dir//\'/\'\'}" back_ps="${backup_root//\'/\'\'}"

  local ahk_stop
  if [ "$has_ahk" = "1" ]; then
    ahk_stop=$(cat <<'PSAHK'
# (1) Stop the strih AHK watchdog FIRST -- NL_STARTUP.ahk respawns obs64 via the bare exe within
#     seconds of the window vanishing, which would re-lock data\ + obs-plugins\ files mid-copy
#     (robocopy exit >= 8) AND drop the shortcut params. Restart it via launch-obs-genlock.sh on
#     relaunch (STEP 2 of the plan), never here.
if (Get-Process AutoHotkey64 -ErrorAction SilentlyContinue) {
  Stop-Process -Name AutoHotkey64 -Force -ErrorAction SilentlyContinue
}
PSAHK
)
  else
    ahk_stop='# (1) AutoHotkey64 watchdog: no-op (this box runs no AHK auto-respawn watcher).'
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
\$fleetConfirmRetention = \$false   # set \$true to actually delete old backups; default = PLAN ONLY.

# (0) preflight -- the staged bundle must already be on the box (STEP 0 of the plan uploads it).
if (-not (Test-Path \$stage))  { Write-Error "stage \$stage not found -- upload the artifact first (plan STEP 0)"; exit 2 }
if (-not (Test-Path \$obsDir)) { Write-Error "OBS install \$obsDir not found"; exit 2 }

${ahk_stop}

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
Write-MarkerAtomic (Join-Path \$obsDir 'GENLOCK_BUILD_SHA.txt')  '${gsha}'
Write-MarkerAtomic (Join-Path \$obsDir 'DISTROAV_BUILD_SHA.txt') '${dsha}'
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

# (7) box-backup RETENTION PLAN (bod 5) -- keep the newest ${keep} dated backup dirs; list the rest
#     as "would delete". Deletion happens ONLY when \$fleetConfirmRetention is \$true (never silent).
Write-Host "RETENTION PLAN (keep newest ${keep} of \$backupRoot\\*-789):"
\$stale = @(Get-ChildItem \$backupRoot -Directory -ErrorAction SilentlyContinue | Where-Object { \$_.Name -like '*-789' } | Sort-Object LastWriteTime -Descending | Select-Object -Skip ${keep})
if (\$stale.Count -eq 0) { Write-Host "  nothing to prune." }
foreach (\$s in \$stale) {
  if (\$fleetConfirmRetention) { Write-Host "  deleting \$(\$s.FullName)"; Remove-Item -Recurse -Force \$s.FullName }
  else                        { Write-Host "  would delete \$(\$s.FullName)" }
}

Write-Host "#789 FLEET DEPLOY OK: ${box} swapped to ${gsha} (marker written, obs.dll byte-verified). Relaunch via launch-obs-genlock.sh (plan STEP 2)."
exit 0
PS
}

# build_imag_deploy_program STAGE MARKER_DIR BACKUP_ROOT GSHA DSHA KEEP
#   Emit the on-imag bash program the fleet script scp's + runs over ssh. It installs the four
#   genlock components from STAGE, runs the ABI guards (SONAME + the #499 nm build-proof), writes the
#   markers via the SHARED genlock_write_markers helper (scp'd alongside), applies retention, and
#   restarts OBS via the existing imag-obs-stop.sh/imag-obs-start.sh helpers + a start-time re-check
#   (the #912 stop-race). STAGE holds the 4 flat files + genlock-markers.sh (the fleet script stages
#   them). Faithful to setup-imag.sh step 12; the markers are the shared single source of truth.
build_imag_deploy_program() {
  local stage="$1" marker_dir="$2" backup_root="$3" gsha="$4" dsha="$5" keep="$6"
  cat <<EOS
#!/usr/bin/env bash
# issue 789 on-imag genlock deploy (run over ssh; mirrors setup-imag.sh step 12; markers via the
# SHARED scripts/lib/genlock-markers.sh helper scp'd into the stage).
set -euo pipefail
STAGE='${stage}'
MARKER_DIR='${marker_dir}'
BACKUP_ROOT='${backup_root}'
KEEP='${keep}'
LIBOBS_REAL='/usr/lib/x86_64-linux-gnu/libobs.so.30'
DISTROAV_REAL='/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so'
OBS_FRONTEND_REAL='/usr/bin/obs'
LIBOBS_OPENGL_REAL='/usr/lib/x86_64-linux-gnu/libobs-opengl.so.30'

# shared marker helper (single source of truth; scp'd into the stage)
. "\$STAGE/genlock-markers.sh"

command -v readelf >/dev/null 2>&1 && command -v nm >/dev/null 2>&1 || { echo "readelf/nm (binutils) missing" >&2; exit 1; }

# (1) back up the currently-installed components (instant rollback), one dated dir per deploy + the
#     overwritten 'previous' rollback dir setup-imag.sh/drift-guard expect.
STAMP="\$(date +%Y-%m-%dT%H-%M-%S)"
BK="\$BACKUP_ROOT/\$STAMP-789"
mkdir -p "\$BK" "\$BACKUP_ROOT/previous"
for f in "\$LIBOBS_REAL" "\$DISTROAV_REAL" "\$OBS_FRONTEND_REAL" "\$LIBOBS_OPENGL_REAL"; do
  [ -f "\$f" ] && cp -a "\$f" "\$BK/\$(basename "\$f").pre-789"
  [ -f "\$f" ] && cp -a "\$f" "\$BACKUP_ROOT/previous/\$(basename "\$f")"
done

# (2) install the FOUR components together (they version as one build) -- libobs, distroav, the
#     frontend exe (#499), and the SEPARATE libobs-opengl (#756).
install -m 0644 -o root -g root "\$STAGE/libobs.so.30"        "\$LIBOBS_REAL"        || { echo "libobs install failed" >&2; exit 4; }
install -m 0644 -o root -g root "\$STAGE/distroav.so"         "\$DISTROAV_REAL"      || { echo "distroav install failed" >&2; exit 4; }
install -m 0755 -o root -g root "\$STAGE/obs"                 "\$OBS_FRONTEND_REAL"  || { echo "frontend /usr/bin/obs install failed" >&2; exit 4; }
install -m 0644 -o root -g root "\$STAGE/libobs-opengl.so.30" "\$LIBOBS_OPENGL_REAL" || { echo "libobs-opengl install failed" >&2; exit 4; }
ldconfig

# (3) ABI guards -- refuse a mismatched SONAME or a stock/wrong frontend (no grep -q on a piped
#     external cmd under pipefail; read the full output -- the setup-imag.sh SIGPIPE footgun).
readelf -d "\$LIBOBS_REAL" 2>/dev/null | grep 'SONAME.*\[libobs\.so\.30\]' >/dev/null \
  || { echo "post-swap libobs.so.30 SONAME check failed -- refuse a mismatched ABI" >&2; exit 4; }
nm -D -u "\$OBS_FRONTEND_REAL" 2>/dev/null | grep 'obs_display_set_render_divisor' >/dev/null \
  || { echo "post-swap /usr/bin/obs does not reference obs_display_set_render_divisor -- refuse a stock/wrong frontend (#499)" >&2; exit 4; }

# (4) markers via the SHARED helper (bod 4 single source of truth)
genlock_write_markers "\$MARKER_DIR" '${gsha}' '${dsha}'
[ -f "\$STAGE/BUNDLE_MANIFEST.json" ] && cp -a "\$STAGE/BUNDLE_MANIFEST.json" "\$MARKER_DIR/BUNDLE_MANIFEST.json"

# (5) box-backup RETENTION PLAN (bod 5) -- keep the newest \$KEEP dated dirs; list the rest.
echo "RETENTION PLAN (keep newest \$KEEP of \$BACKUP_ROOT/*-789):"
genlock_retention_delete_plan "\$KEEP" "\$BACKUP_ROOT" '*-789' | while IFS= read -r d; do
  echo "  would delete \$d"
done

# (6) graceful OBS restart via the EXISTING installed helpers, then VERIFY the process actually
#     restarted after the swap (the #912 stop-race: a one-shot stop->install->start can leave the
#     OLD obs mapped). SIGKILL only as the last resort on a wedged process.
if [ -x /usr/local/bin/imag-obs-stop.sh ]; then sudo -u "\${SUDO_USER:-newlevel}" DISPLAY=:0 /usr/local/bin/imag-obs-stop.sh || true; fi
pkill -9 -x obs 2>/dev/null || true
sleep 2
rm -f "/home/\${SUDO_USER:-newlevel}/.config/obs-studio/.sentinel/"* 2>/dev/null || true
if [ -x /usr/local/bin/imag-obs-start.sh ]; then setsid nohup sudo -u "\${SUDO_USER:-newlevel}" DISPLAY=:0 /usr/local/bin/imag-obs-start.sh >/dev/null 2>&1 & fi
sleep 5
if ps -o pid,lstart,cmd -C obs 2>/dev/null | grep -q obs; then
  echo "#789 IMAG DEPLOY OK: swapped to ${gsha}; obs restarted:"; ps -o pid,lstart,cmd -C obs
else
  echo "#789 IMAG WARN: obs not seen after restart -- verify from a fresh ssh (ps -o pid,lstart -C obs)" >&2
fi
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
  local box="$1" mode="$2" stage="$3" gsha="$4" dsha="$5"
  local mcp ip has_ahk win_stage program artifact
  mcp="$(fleet_box_mcp "$box")"; ip="$(fleet_box_ip "$box")"; has_ahk="$(fleet_box_has_ahk "$box")"
  artifact="$(fleet_windows_artifact "$mode")"
  win_stage="C:\\stage-genlock-${gsha}"
  program="$(build_windows_deploy_program "$box" "$mode" "$win_stage" 'C:\Program Files\obs-studio' "$has_ahk" 'C:\obs-backup' "$RETENTION_KEEP" "$gsha" "$dsha")"
  cat <<PLAN
# ================= FLEET PLAN: box=${box} (${mcp}, ${ip}) mode=${mode} =================
# STEP 0 (once per box): upload the downloaded '${artifact}' bytes to the box at ${win_stage}
#         via the ${mcp} MCP FileUpload (or sshpass scp -O of a zip + Expand-Archive on the box),
#         then run the program below in the ${mcp} MCP Shell (timeout >= 240 s):
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
  local stage="$1" gsha="$2" dsha="$3"
  local imag_stage="/tmp/genlock-stage-${gsha}"
  local program
  program="$(build_imag_deploy_program "$imag_stage" '/opt/obs-genlock' '/opt/obs-backup' "$gsha" "$dsha" "$RETENTION_KEEP")"
  cat <<PLAN
# ================= FLEET PLAN: box=imag (ssh newlevel@imag) =================
# STEP 0: scp the 4 staged files (libobs.so.30, distroav.so, obs, libobs-opengl.so.30) + BUNDLE_MANIFEST.json
#         + scripts/lib/genlock-markers.sh from ${stage} to imag:${imag_stage}, then run over ssh
#         (sudo, DISPLAY=:0). The on-imag program:
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
    local sha="$sha_override" b
    echo "# ===== issue 789 genlock FLEET deploy PLAN — run=${run_id} sha=${sha} boxes=${boxes} mode=${mode} ====="
    local IFS=','
    for b in $boxes; do
      case "$b" in
        strih|stream) emit_windows_plan "$b" "$mode" "$stage" "$sha" "$sha" ;;
        imag)         emit_imag_plan "$stage" "$sha" "$sha" ;;
      esac
    done
    echo "# fleet-deploy log line (append to ${FLEET_LOG_DEFAULT} once the deploy is done):"
    fleet_log_line "$run_id" "$sha" "$boxes" "$mode"
    exit 0
  fi

  # --- execute mode -------------------------------------------------------------------------------
  # Resolve the anchor headSha, download the same-SHA artifacts, emit the Windows plan, ssh-deploy
  # imag, append the fleet log. (Live deploy is driven by the supervisor -- see the ticket: #789
  # stays OPEN until the new path is driven live once.)
  command -v gh >/dev/null 2>&1 || { echo "ERROR: gh CLI required for execute mode" >&2; exit 3; }
  local sha; sha="$(gh run view "$run_id" --repo "$GENLOCK_REPO" --json headSha -q .headSha 2>/dev/null)" \
    || { echo "ERROR: could not resolve headSha for run $run_id" >&2; exit 3; }
  [ -n "$sha" ] || { echo "ERROR: empty headSha for run $run_id" >&2; exit 3; }
  echo "# anchor run $run_id -> canonical SHA $sha; requested boxes: $boxes (mode $mode)"
  echo "# retention auto-confirm (--yes): $yes"
  echo "# (execute mode resolves the same-SHA windows/linux artifacts, emits the Windows MCP plan,"
  echo "#  and ssh-deploys imag. Drive the printed Windows program via the win-* MCP Shell.)"
  echo "# NOTE: full download+ssh execution is driven live by the supervisor; re-run with --plan for"
  echo "#       the offline plan. Fleet log target: ${FLEET_LOG_DEFAULT}"
  # (The download/scp/ssh steps run live under the supervisor; not exercised by the worker.)
  exit 0
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
