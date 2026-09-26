#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure printer functions, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`.
# genlock-plugin-deploy.sh -- the FULL-mode plugin PowerShell blocks of the Windows genlock deploy.
# Sourced by scripts/deploy-genlock-fleet.sh (build_windows_deploy_program); pure printers, no I/O.
#
# Each function prints literal PowerShell (quoted heredocs, nothing expands) that the fleet
# deploy program embeds. $obsDir, $stage, $backupDir and $manifest are variables of that program
# (steps 0, 3 and 6). A FAST (obs.dll-only) deploy carries neither plugin, so both print a no-op
# comment for it.
#
#   genlock_plugin_backup_ps MODE  -- step (3b): keep the box's current obs-vban.dll for rollback,
#                                     BEFORE the step-(4) robocopy of obs-plugins\64bit overwrites it.
#   genlock_plugin_deploy_ps MODE  -- step (6b) #1115: deploy + byte-verify distroav.dll at its
#                                     ProgramData load path; step (6c) issue 1372: byte-verify the
#                                     paced obs-vban.dll the robocopy put into Program Files.

# The obs-vban rollback copy (issue 1372). Program Files\obs-studio\obs-plugins\64bit is the one
# path the boxes load obs-vban from (no ProgramData / AppData copy, verified on resolume 26.9.2026).
genlock_plugin_backup_ps() {
  if [ "$1" != "full" ]; then
    echo "# (3b) issue 1372: no obs-vban backup on a FAST deploy (no obs-vban in the fast bundle)."
    return 0
  fi
  cat <<'PSOBSVBANBACKUP'
# (3b) issue 1372 -- keep the box's current obs-vban.dll (the stock 0.3.1 on the first deploy).
Copy-Item -Force (Join-Path $obsDir 'obs-plugins\64bit\obs-vban.dll') (Join-Path $backupDir 'obs-vban.dll.pre-789') -ErrorAction SilentlyContinue
PSOBSVBANBACKUP
}

genlock_plugin_deploy_ps() {
  if [ "$1" != "full" ]; then
    echo '# (6b) #1115: distroav ProgramData deploy -- no-op on a FAST (obs.dll-only) deploy (no distroav in the fast bundle).'
    echo "# (6c) issue 1372: obs-vban verify -- no-op on a FAST (obs.dll-only) deploy (no obs-vban in the fast bundle)."
    return 0
  fi
  # #1115 (Option A): FULL mode ALSO deploys the bundle's genlock distroav.dll to the REAL OBS load
  # path in ProgramData (backup + fail-closed byte verify), so the LOADED plugin is the canonical
  # build and the byte-parity gather/compare against the manifest becomes real.
  cat <<'PSDISTROAV'
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
  echo
  # issue 1372: the vendored, paced obs-vban.dll rides the obs-plugins\64bit robocopy (step 4).
  cat <<'PSOBSVBAN'
# (6c) issue 1372 -- byte-verify the deployed obs-vban.dll (the paced VBAN sender) against the bundle
#      manifest, fail-closed. The robocopy of obs-plugins\64bit put it into Program Files, where the
#      stock 0.3.1 DLL was installed. A bundle built before issue 1372 carries no obs-vban.dll: the
#      box then keeps its current one.
$pfVban = Join-Path $obsDir 'obs-plugins\64bit\obs-vban.dll'
$srcVban = Join-Path $stage 'obs-plugins\64bit\obs-vban.dll'
if (-not (Test-Path $srcVban)) {
  Write-Warning "the bundle carries no obs-vban.dll (built before issue 1372) -- the box keeps its current obs-vban."
} elseif (Test-Path $manifest) {
  $mv = Get-Content $manifest -Raw | ConvertFrom-Json
  $wantV = ($mv.files | Where-Object { $_.path -match '(^|/)obs-vban\.dll$' } | Select-Object -First 1).sha256
  $gotV  = (Get-FileHash -Algorithm SHA256 $pfVban).Hash.ToLower()
  $matchV = ($wantV -and ($gotV -eq $wantV.ToLower()))
  Write-Host "VERIFY obs-vban.dll want=$wantV got=$gotV match=$matchV"
  if (-not $matchV) { Write-Error "VERIFY FAIL: deployed obs-vban.dll bytes do not match the bundle manifest -- do NOT trust this box"; exit 4 }
} else {
  Write-Warning "no BUNDLE_MANIFEST.json in $stage -- cannot byte-verify obs-vban.dll (marker-only)."
}
PSOBSVBAN
}
