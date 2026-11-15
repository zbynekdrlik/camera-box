#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure printer functions, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`.
# genlock-plugin-deploy.sh -- the plugin PowerShell blocks of the Windows genlock deploy.
# Sourced by scripts/deploy-genlock-fleet.sh (build_windows_deploy_program); pure printers, no I/O.
#
# Each function prints literal PowerShell (quoted heredocs, nothing expands) that the fleet
# deploy program embeds. $obsDir, $stage, $backupDir and $manifest are variables of that program
# (steps 0, 3 and 6). Every Windows deploy ships the whole bundle (issue 1505 retired the obs.dll-only
# FAST deploy; issue 1522 removed its branches), so both always print their blocks.
#
#   genlock_plugin_backup_ps       -- step (3b): keep the box's current obs-vban.dll and (issue 1425)
#                                     vlc-video.dll for rollback, BEFORE the step-(4) robocopy of
#                                     obs-plugins\64bit overwrites them.
#   genlock_plugin_deploy_ps       -- step (6b) #1115: deploy + byte-verify distroav.dll at its
#                                     ProgramData load path (issue 1505: created from the bundle on a
#                                     box whose DistroAV lived only in Program Files); the leftover step issue
#                                     1505: move a leftover Program Files distroav.dll/.pdb into the
#                                     backup dir, fail-closed, and read back exactly one DistroAV;
#                                     step (6c) issue 1372: byte-verify the
#                                     paced obs-vban.dll the robocopy put into Program Files; step (6d)
#                                     issue 1425: byte-verify the media-clock vlc-video.dll the same way.

# The obs-vban rollback copy (issue 1372). Program Files\obs-studio\obs-plugins\64bit is the one
# path the boxes load obs-vban from (no ProgramData / AppData copy, verified on resolume 26.9.2026).
genlock_plugin_backup_ps() {
  cat <<'PSOBSVBANBACKUP'
# (3b) issue 1372 -- keep the box's current obs-vban.dll (the stock 0.3.1 on the first deploy).
Copy-Item -Force (Join-Path $obsDir 'obs-plugins\64bit\obs-vban.dll') (Join-Path $backupDir 'obs-vban.dll.pre-789') -ErrorAction SilentlyContinue
# (3b) issue 1425 -- keep the box's current vlc-video.dll (the stock OBS one on the first deploy).
Copy-Item -Force (Join-Path $obsDir 'obs-plugins\64bit\vlc-video.dll') (Join-Path $backupDir 'vlc-video.dll.pre-789') -ErrorAction SilentlyContinue
PSOBSVBANBACKUP
}

genlock_plugin_deploy_ps() {
  # #1115 (Option A): the deploy ALSO deploys the bundle's genlock distroav.dll to the REAL OBS load
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
$pfDistroav  = Join-Path $obsDir 'obs-plugins\64bit\distroav.dll'
if (-not (Test-Path $srcDistroav)) { Write-Error "staged distroav.dll not found at $srcDistroav -- the FULL bundle must carry it"; exit 4 }
if (-not (Test-Path -LiteralPath $pdDistroav)) {
  # issue 1505: a box whose DistroAV lives only in Program Files (grading-pp, read 15.11.2026) gets the
  # ProgramData install created from the bundle -- the DLL below plus its data tree here -- and the
  # leftover step below moves the Program Files copy aside. A box with no DistroAV anywhere is still
  # refused, and so is a bundle without the DistroAV data tree (a DLL without its locale/data files).
  if (-not (Test-Path -LiteralPath $pfDistroav)) { Write-Error "ProgramData distroav load path $pdDistroav not found -- DistroAV is not installed where OBS loads it"; exit 4 }
  $srcDistroavData = Join-Path $stage 'data\obs-plugins\distroav'
  if (-not (Test-Path -LiteralPath $srcDistroavData)) {
    Write-Host "#1505 FAIL: the bundle carries no $srcDistroavData -- cannot create the ProgramData DistroAV install with its data files; nothing created, NOT relaunching." -ForegroundColor Red
    exit 4
  }
  $pdPluginDir = Split-Path (Split-Path (Split-Path $pdDistroav -Parent) -Parent) -Parent
  New-Item -ItemType Directory -Force -Path (Split-Path $pdDistroav -Parent) | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $pdPluginDir 'data') | Out-Null
  Copy-Item -Recurse -Force (Join-Path $srcDistroavData '*') (Join-Path $pdPluginDir 'data')
  Write-Host "#1505: no ProgramData DistroAV on this box -- created $pdPluginDir from the bundle; the Program Files copy is moved aside below."
}
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
# (6e) issue 1505 -- ONE DistroAV per OBS. A leftover Program Files obs-plugins\64bit\distroav.dll (+ its
#      .pdb) next to the ProgramData one would load a second, older DistroAV. MOVE it into this deploy's
#      dated backup dir (no copy-then-delete: the backup IS the moved file). A move that fails stops the
#      program here with exit 4, before the relaunch step, and the leftover stays where it was. A box with
#      no leftover logs a no-op. Then the read-back: exactly one DistroAV copy, the ProgramData one.
#      (Numbered 6e and run here, inside the DistroAV block, before 6c/6d.)
$pfLeftovers = @(@($pfDistroav, [System.IO.Path]::ChangeExtension($pfDistroav, '.pdb')) | Where-Object { Test-Path -LiteralPath $_ })
if ($pfLeftovers.Count -eq 0) { Write-Host "#1505: no leftover DistroAV in Program Files -- nothing to move." }
foreach ($leftover in $pfLeftovers) {
  $leftoverDst = Join-Path $backupDir ((Split-Path $leftover -Leaf) + '.programfiles.pre-789')
  $moveError = ''
  try {
    Move-Item -LiteralPath $leftover -Destination $leftoverDst -Force -ErrorAction Stop
  } catch {
    $moveError = $_.Exception.Message
  }
  # Judge the move by the files, not only by the cmdlet: the leftover must be gone and its backup there.
  if ($moveError -or (Test-Path -LiteralPath $leftover) -or -not (Test-Path -LiteralPath $leftoverDst)) {
    if (-not $moveError) { $moveError = 'the file is still there after the move' }
    Write-Host "#1505 FAIL: could not move the leftover $leftover to $leftoverDst ($moveError) -- OBS would load two DistroAV copies; NOT relaunching (do not start OBS by hand either). Free the file and re-run the deploy." -ForegroundColor Red
    exit 4
  }
  Write-Host "#1505: moved the leftover $leftover to $leftoverDst."
}
$pfDistroavLeft = @(Get-ChildItem -LiteralPath (Join-Path $obsDir 'obs-plugins') -Recurse -File -ErrorAction Stop | Where-Object { $_.Name -ieq 'distroav.dll' })
$pdDistroavOk = Test-Path -LiteralPath $pdDistroav
$distroavCopies = $pfDistroavLeft.Count + [int]$pdDistroavOk
Write-Host "READBACK distroav copies=$distroavCopies programdata=$pdDistroavOk programfiles=$($pfDistroavLeft.Count)"
if ((-not $pdDistroavOk) -or ($pfDistroavLeft.Count -ne 0)) {
  Write-Host "#1505 FAIL: expected exactly one DistroAV (the ProgramData one); still under Program Files: $(@($pfDistroavLeft | ForEach-Object { $_.FullName }) -join ', ') -- NOT relaunching." -ForegroundColor Red
  exit 4
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
  echo
  # issue 1425: the vendored vlc-video.dll (its stamps on the OBS media clock) rides the same robocopy.
  cat <<'PSVLCVIDEO'
# (6d) issue 1425 -- byte-verify the deployed vlc-video.dll (stamps mapped onto the OBS media clock) against
#      the bundle manifest, fail-closed. The robocopy of obs-plugins\64bit put it into Program Files, where
#      the stock OBS vlc-video.dll was installed; it still loads libvlc from the installed VLC. A bundle
#      built before issue 1425 carries no vlc-video.dll: the box then keeps its current one.
$pfVlc = Join-Path $obsDir 'obs-plugins\64bit\vlc-video.dll'
$srcVlc = Join-Path $stage 'obs-plugins\64bit\vlc-video.dll'
if (-not (Test-Path $srcVlc)) {
  Write-Warning "the bundle carries no vlc-video.dll (built before issue 1425) -- the box keeps its current vlc-video."
} elseif (Test-Path $manifest) {
  $mvl = Get-Content $manifest -Raw | ConvertFrom-Json
  $wantL = ($mvl.files | Where-Object { $_.path -match '(^|/)vlc-video\.dll$' } | Select-Object -First 1).sha256
  $gotL  = (Get-FileHash -Algorithm SHA256 $pfVlc -ErrorAction Stop).Hash.ToLower()
  $matchL = ($wantL -and $gotL -and ($gotL -eq $wantL.ToLower()))
  Write-Host "VERIFY vlc-video.dll want=$wantL got=$gotL match=$matchL"
  if (-not $matchL) { Write-Error "VERIFY FAIL: deployed vlc-video.dll bytes do not match the bundle manifest -- do NOT trust this box"; exit 4 }
} else {
  Write-Warning "no BUNDLE_MANIFEST.json in $stage -- cannot byte-verify vlc-video.dll (marker-only)."
}
PSVLCVIDEO
}
