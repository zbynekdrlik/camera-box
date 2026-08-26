<#
  obs-backup-retention.ps1 -- #789 (residual B / criterion 5)

  Dry-run-first retention sweep for the deploy/backup DIRECTORIES the ONE fleet deploy path
  (scripts/deploy-genlock-fleet.sh) leaves behind on a Windows OBS box (strih/stream). Two kinds
  accumulate and are NOT swept outside a deploy:

    * dated box-backup dirs  <stamp>-789  under -BackupRoot (default C:\obs-backup); stamp is
      yyyy-MM-ddTHH-mm-ss (Get-Date). The deploy program prunes these to the newest 3 -- but ONLY
      inline during a deploy, and ONLY when --yes was passed.
    * per-sha stage dirs  stage-genlock-<sha>  under -StageParent (default C:\). NEVER pruned:
      one grows per deployed sha forever.

  Keeps the newest -KeepRuns dirs of EACH KIND separately, UNION anything younger than -KeepDays,
  and deletes ONLY dirs whose name matches the deploy's OWN naming allowlist. It is DRY-RUN by
  default: prints the FULL keep/protect/delete plan + a summary and deletes NOTHING unless -Execute.

  A differently-named dir -- the imag 'previous' rollback dir, an operator's own folder, anything on
  C:\ that is not exactly stage-genlock-<hex> -- does NOT match the allowlist and is PROTECTED. This
  is NEVER a generic dir sweep.

  ** The first real -Execute run is the SUPERVISOR's explicit, reviewed step (#789). ** Run the
  dry-run, read the printed plan, and only then re-run with -Execute.

  PARITY: this is a faithful port of the PURE decision in src/obs_backup_retention.rs (same
  allowlist shapes + newest-N-per-kind UNION younger-than-D rule). That Rust module +
  tests/obs_backup_retention.rs are the canonical spec -- keep this script in sync with them. The
  imag leg is scripts/obs-backup-retention.sh --local-sweep (a bash port of the same decision).

  Deploy (deploy-genlock-fleet.sh style): scp -O this file to the box, then run it via
  `powershell -NoProfile -ExecutionPolicy Bypass -File <remote.ps1> [-Execute]` -- never a nested
  `powershell -Command` over ssh. scripts/obs-backup-retention.sh automates the dry-run leg.

  Usage (on the box):
    powershell -NoProfile -ExecutionPolicy Bypass -File obs-backup-retention.ps1
    powershell -NoProfile -ExecutionPolicy Bypass -File obs-backup-retention.ps1 -KeepRuns 3 -KeepDays 7
    powershell -NoProfile -ExecutionPolicy Bypass -File obs-backup-retention.ps1 -Execute   # SUPERVISOR only
#>
param(
    [string]$BackupRoot = "C:\obs-backup",
    [string]$StageParent = "C:\",
    [int]$KeepRuns = 3,
    [double]$KeepDays = 7,
    [switch]$Execute
)
$ErrorActionPreference = "Stop"

# EXPLICIT allowlists. Case-SENSITIVE (`-cmatch`). `[0-9]`, NOT `\d`: .NET `\d` also matches Unicode
# decimal digits (fullwidth/Arabic/Devanagari), which would make the executor MORE permissive than
# the Rust spec's is_ascii_digit() -- the wrong direction for a delete gate. These mirror
# is_dated_backup() / is_stage_dir() in src/obs_backup_retention.rs exactly.
$datedRe = '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}-789$'
$stageRe = '^(stage-genlock|genlock-stage)-[0-9a-f]+$'

function Format-Gb([long]$bytes) { return ("{0:N2} GB" -f ($bytes / 1GB)) }

function Get-DirBytes([string]$path) {
    $sum = (Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return [long]0 } else { return [long]$sum }
}

# Build the KEEP/DELETE plan for ONE kind's directory list (newest-N UNION younger-than-D). Mirrors
# the per-kind loop in obs_backup_retention::plan(). Newest first with a deterministic Name
# tie-break so the on-box plan matches the Rust spec at an exact LastWriteTime tie.
function Get-KindPlan($dirs, [int]$keepRuns, [double]$keepDays, [datetime]$now) {
    $keep = New-Object System.Collections.Generic.List[object]
    $delete = New-Object System.Collections.Generic.List[object]
    $sorted = @($dirs | Sort-Object @{ Expression = 'LastWriteTime'; Descending = $true }, @{ Expression = 'Name'; Descending = $false })
    for ($i = 0; $i -lt $sorted.Count; $i++) {
        $dir = $sorted[$i]
        $ageDays = ($now - $dir.LastWriteTime).TotalDays
        if ($i -lt $keepRuns) {
            $keep.Add([pscustomobject]@{ Dir = $dir; Reason = "newest"; AgeDays = $ageDays })
        }
        elseif ($keepDays -gt 0 -and $ageDays -lt $keepDays) {
            $keep.Add([pscustomobject]@{ Dir = $dir; Reason = "within-days"; AgeDays = $ageDays })
        }
        else {
            $delete.Add([pscustomobject]@{ Dir = $dir; AgeDays = $ageDays })
        }
    }
    return @{ Keep = $keep; Delete = $delete }
}

Write-Output "=== obs-backup-retention (#789) ==="
Write-Output ("BackupRoot  : {0}  (dated <stamp>-789 dirs)" -f $BackupRoot)
Write-Output ("StageParent : {0}  (stage-genlock-<sha> dirs)" -f $StageParent)
Write-Output ("Policy      : keep newest {0} of EACH kind UNION younger than {1} days" -f $KeepRuns, $KeepDays)
Write-Output ("Mode        : {0}" -f ($(if ($Execute) { "EXECUTE (deleting)" } else { "DRY-RUN (no deletion)" })))
Write-Output ""

$now = Get-Date

# Gather candidate dirs per kind (allowlist match against the top-level dir NAME only).
$datedDirs = @()
if (Test-Path -LiteralPath $BackupRoot -PathType Container) {
    $datedDirs = @(Get-ChildItem -LiteralPath $BackupRoot -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -cmatch $datedRe })
}
else {
    Write-Output ("NOTE: backup root not found (nothing to prune there): {0}" -f $BackupRoot)
}
$stageDirs = @()
if (Test-Path -LiteralPath $StageParent -PathType Container) {
    $stageDirs = @(Get-ChildItem -LiteralPath $StageParent -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -cmatch $stageRe })
}
else {
    Write-Output ("NOTE: stage parent not found (nothing to prune there): {0}" -f $StageParent)
}

$datedPlan = Get-KindPlan $datedDirs $KeepRuns $KeepDays $now
$stagePlan = Get-KindPlan $stageDirs $KeepRuns $KeepDays $now

$keepAll = @($datedPlan.Keep) + @($stagePlan.Keep)
$deleteAll = @($datedPlan.Delete) + @($stagePlan.Delete)

# Protected (non-matching) dirs. LIST them for the dedicated BackupRoot (the review step's need);
# for the shared StageParent (C:\) show only a COUNT so we never dump every top-level system dir.
$datedProtected = @()
if (Test-Path -LiteralPath $BackupRoot -PathType Container) {
    $datedProtected = @(Get-ChildItem -LiteralPath $BackupRoot -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -cnotmatch $datedRe })
}
$stageProtectedCount = 0
if (Test-Path -LiteralPath $StageParent -PathType Container) {
    $stageProtectedCount = @(Get-ChildItem -LiteralPath $StageParent -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -cnotmatch $stageRe }).Count
}

# ---- full plan ---------------------------------------------------------------------------------
Write-Output "--- PROTECT (non-matching names -- never deleted) ---"
if ($datedProtected.Count -eq 0) { Write-Output "  (backup root: none)" }
foreach ($fl in ($datedProtected | Sort-Object Name)) {
    Write-Output ("  PROTECT  {0,-30}  ({1})" -f $fl.Name, $BackupRoot)
}
if ($stageProtectedCount -gt 0) {
    Write-Output ("  ({0} non-matching top-level dir(s) under {1} protected, not listed)" -f $stageProtectedCount, $StageParent)
}
Write-Output ""
Write-Output "--- KEEP (matching, retained by policy) ---"
if ($keepAll.Count -eq 0) { Write-Output "  (none)" }
foreach ($k in $keepAll) {
    Write-Output ("  KEEP     {0,10}  {1}  {2}  [{3}, {4:N1}d]" -f (Format-Gb (Get-DirBytes $k.Dir.FullName)), $k.Dir.LastWriteTime.ToString("yyyy-MM-dd"), $k.Dir.Name, $k.Reason, $k.AgeDays)
}
Write-Output ""
Write-Output "--- DELETE (matching, past retention) ---"
if ($deleteAll.Count -eq 0) { Write-Output "  (none)" }
$deleteBytes = [long]0
foreach ($del in $deleteAll) {
    $b = Get-DirBytes $del.Dir.FullName
    $deleteBytes += $b
    Write-Output ("  DELETE   {0,10}  {1}  {2}  [{3:N1}d]" -f (Format-Gb $b), $del.Dir.LastWriteTime.ToString("yyyy-MM-dd"), $del.Dir.Name, $del.AgeDays)
}
Write-Output ""

# ---- summary -----------------------------------------------------------------------------------
Write-Output "--- SUMMARY ---"
Write-Output ("  dated backups : {0} matched  ({1} keep / {2} delete)" -f ($datedDirs.Count), ($datedPlan.Keep.Count), ($datedPlan.Delete.Count))
Write-Output ("  stage dirs    : {0} matched  ({1} keep / {2} delete)" -f ($stageDirs.Count), ($stagePlan.Keep.Count), ($stagePlan.Delete.Count))
Write-Output ("  DELETE total  : {0}  ({1} freed)" -f $deleteAll.Count, (Format-Gb $deleteBytes))
Write-Output ""

if (-not $Execute) {
    Write-Output "DRY-RUN -- nothing deleted. Re-run with -Execute to delete the DELETE set above."
    Write-Output "(The first -Execute run is the supervisor's explicit, reviewed step -- #789.)"
    exit 0
}

# ---- execute -----------------------------------------------------------------------------------
Write-Output "EXECUTE -- deleting the DELETE set above ..."
$freed = [long]0
$errors = 0
foreach ($del in $deleteAll) {
    try {
        $b = Get-DirBytes $del.Dir.FullName
        Remove-Item -LiteralPath $del.Dir.FullName -Recurse -Force
        $freed += $b
        Write-Output ("  deleted  {0,10}  {1}" -f (Format-Gb $b), $del.Dir.Name)
    }
    catch {
        $errors++
        Write-Output ("  ERROR deleting {0}: {1}" -f $del.Dir.Name, $_.Exception.Message)
    }
}
Write-Output ""
Write-Output ("EXECUTE done: freed {0}, {1} error(s)." -f (Format-Gb $freed), $errors)
if ($errors -gt 0) { exit 1 }
exit 0
