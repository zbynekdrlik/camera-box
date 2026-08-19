<#
  strih-recordings-retention.ps1 -- #1122

  Dry-run-first retention sweep for the E2E harness's OBS recordings (strih: D:\_REC, ~691 GiB of
  344 old .mkv runs vs the 50 GB working budget). Deletes ONLY files whose name matches the
  harness's OWN OBS-timestamp allowlist (`YYYY-MM-DD HH-MM-SS[ (n)].mkv|.mp4`), keeping the newest
  -KeepRuns runs UNION anything younger than -KeepDays. It is DRY-RUN by default: it prints the
  FULL keep/protect/delete plan and a summary, and deletes NOTHING unless -Execute is passed.

  A differently-named operator/debug recording (e.g. `strih700105.mkv`, which is really present in
  D:\_REC) does NOT match the allowlist and is therefore PROTECTED -- a generic `*.mkv` sweep would
  eat it; this one never touches it.

  ** The first real -Execute run is the SUPERVISOR's explicit, reviewed step (issue #1122).**
  Run the dry-run, read the printed plan, and only then re-run with -Execute.

  PARITY: this is a faithful port of the PURE decision in src/recordings_retention.rs (the same
  allowlist shape + newest-N UNION younger-than-D rule). That Rust module + tests/recordings_retention.rs
  are the canonical spec -- keep this script in sync with them.

  Deploy (deploy-genlock-fleet.sh style): scp -O this file to the box, then run it via
  `powershell -NoProfile -ExecutionPolicy Bypass -File <remote.ps1> [-Execute]` -- never a nested
  `powershell -Command` over ssh. scripts/strih-recordings-retention.sh automates the dry-run leg.

  Usage examples (on the box):
    powershell -NoProfile -ExecutionPolicy Bypass -File strih-recordings-retention.ps1
    powershell -NoProfile -ExecutionPolicy Bypass -File strih-recordings-retention.ps1 -KeepRuns 20 -KeepDays 3
    powershell -NoProfile -ExecutionPolicy Bypass -File strih-recordings-retention.ps1 -Execute   # SUPERVISOR only
#>
param(
    [string]$RecordDir = "D:\_REC",
    [int]$KeepRuns = 20,
    [double]$KeepDays = 3,
    [double]$BudgetGb = 50,
    [switch]$Execute
)
$ErrorActionPreference = "Stop"

# The EXPLICIT allowlist -- OBS FilenameFormatting `%CCYY-%MM-%DD %hh-%mm-%ss` + a recording
# extension + an OPTIONAL OBS ` (n)` dedup suffix. Case-SENSITIVE (`-cmatch`): OBS writes lowercase,
# and a `.MKV` / custom-named file must stay protected. Mirrors is_harness_recording() in Rust.
$allow = '^\d{4}-\d{2}-\d{2} \d{2}-\d{2}-\d{2}( \(\d+\))?\.(mkv|mp4)$'

function Format-Gb([long]$bytes) { return ("{0:N1} GB" -f ($bytes / 1GB)) }

Write-Output "=== strih-recordings-retention (#1122) ==="
Write-Output ("RecordDir : {0}" -f $RecordDir)
Write-Output ("Policy    : keep newest {0} runs UNION younger than {1} days" -f $KeepRuns, $KeepDays)
Write-Output ("Budget    : {0} GB" -f $BudgetGb)
Write-Output ("Mode      : {0}" -f ($(if ($Execute) { "EXECUTE (deleting)" } else { "DRY-RUN (no deletion)" })))
Write-Output ""

if (-not (Test-Path -LiteralPath $RecordDir -PathType Container)) {
    Write-Error ("record directory not found: {0}" -f $RecordDir)
    exit 1
}

$files = @(Get-ChildItem -LiteralPath $RecordDir -File)
$now = Get-Date

$matching  = @($files | Where-Object { $_.Name -cmatch $allow } | Sort-Object LastWriteTime -Descending)
$protected = @($files | Where-Object { $_.Name -cnotmatch $allow })

$keep = New-Object System.Collections.Generic.List[object]
$delete = New-Object System.Collections.Generic.List[object]
for ($i = 0; $i -lt $matching.Count; $i++) {
    $fl = $matching[$i]
    $ageDays = ($now - $fl.LastWriteTime).TotalDays
    if ($i -lt $KeepRuns) {
        $keep.Add([pscustomobject]@{ File = $fl; Reason = "newest-run"; AgeDays = $ageDays })
    }
    elseif ($KeepDays -gt 0 -and $ageDays -lt $KeepDays) {
        $keep.Add([pscustomobject]@{ File = $fl; Reason = "within-days"; AgeDays = $ageDays })
    }
    else {
        $delete.Add([pscustomobject]@{ File = $fl; AgeDays = $ageDays })
    }
}

# ---- full plan ---------------------------------------------------------------------------------
Write-Output "--- PROTECT (non-matching names -- never deleted) ---"
if ($protected.Count -eq 0) { Write-Output "  (none)" }
foreach ($fl in ($protected | Sort-Object LastWriteTime -Descending)) {
    Write-Output ("  PROTECT  {0,8}  {1}  {2}" -f (Format-Gb $fl.Length), $fl.LastWriteTime.ToString("yyyy-MM-dd"), $fl.Name)
}
Write-Output ""
Write-Output "--- KEEP (matching, retained by policy) ---"
if ($keep.Count -eq 0) { Write-Output "  (none)" }
foreach ($k in $keep) {
    Write-Output ("  KEEP     {0,8}  {1}  {2}  [{3}, {4:N1}d]" -f (Format-Gb $k.File.Length), $k.File.LastWriteTime.ToString("yyyy-MM-dd"), $k.File.Name, $k.Reason, $k.AgeDays)
}
Write-Output ""
Write-Output "--- DELETE (matching, past retention) ---"
if ($delete.Count -eq 0) { Write-Output "  (none)" }
foreach ($d in $delete) {
    Write-Output ("  DELETE   {0,8}  {1}  {2}  [{3:N1}d]" -f (Format-Gb $d.File.Length), $d.File.LastWriteTime.ToString("yyyy-MM-dd"), $d.File.Name, $d.AgeDays)
}
Write-Output ""

# ---- summary -----------------------------------------------------------------------------------
$totalBytes     = ($files | Measure-Object Length -Sum).Sum
$deleteBytes    = if ($delete.Count) { ($delete | ForEach-Object { $_.File.Length } | Measure-Object -Sum).Sum } else { 0 }
$protectedBytes = if ($protected.Count) { ($protected | Measure-Object Length -Sum).Sum } else { 0 }
$resultBytes    = $totalBytes - $deleteBytes
Write-Output "--- SUMMARY ---"
Write-Output ("  files total     : {0}  ({1})" -f $files.Count, (Format-Gb $totalBytes))
Write-Output ("  protected       : {0}  ({1})" -f $protected.Count, (Format-Gb $protectedBytes))
Write-Output ("  keep (matching) : {0}" -f $keep.Count)
Write-Output ("  DELETE          : {0}  ({1} freed)" -f $delete.Count, (Format-Gb $deleteBytes))
Write-Output ("  after cleanup   : {0}  (budget {1} GB)" -f (Format-Gb $resultBytes), $BudgetGb)
if (($resultBytes / 1GB) -gt $BudgetGb) {
    Write-Output ("  NOTE: still over budget after this plan -- lower -KeepRuns / -KeepDays if you need to free more.")
}
Write-Output ""

if (-not $Execute) {
    Write-Output "DRY-RUN -- nothing deleted. Re-run with -Execute to delete the DELETE set above."
    Write-Output "(The first -Execute run is the supervisor's explicit, reviewed step -- #1122.)"
    exit 0
}

# ---- execute -----------------------------------------------------------------------------------
Write-Output "EXECUTE -- deleting the DELETE set above ..."
$freed = 0
$errors = 0
foreach ($d in $delete) {
    try {
        Remove-Item -LiteralPath $d.File.FullName -Force
        $freed += $d.File.Length
        Write-Output ("  deleted  {0,8}  {1}" -f (Format-Gb $d.File.Length), $d.File.Name)
    }
    catch {
        $errors++
        Write-Output ("  ERROR deleting {0}: {1}" -f $d.File.Name, $_.Exception.Message)
    }
}
Write-Output ""
Write-Output ("EXECUTE done: freed {0}, {1} error(s)." -f (Format-Gb $freed), $errors)
if ($errors -gt 0) { exit 1 }
exit 0
