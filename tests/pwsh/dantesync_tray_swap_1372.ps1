# Issue 1372: RUN the emitted dantesync tray arm (not only read it) against a stubbed Windows node.
#
# The committed pytest reads the emitted PowerShell as text, because dev1 has no pwsh. This harness
# executes the real text: it dot-sources the program `dantesync_windows_tray_only_ps` emits, with
# [CmdletBinding()] stub FUNCTIONS for every cmdlet it calls (a function outranks the cmdlet). The
# stubs model the node: a tray list, Stop-Process that may start a fresh tray N times (the Task
# Scheduler / the HKLM Run entry relaunching it), trays that cannot be killed, and Copy-Item that
# throws a sharing violation (Win32 32) while any tray holds the exe, or a lock violation (Win32 33)
# after a partial write, and a relaunch right after the new exe landed.
#
# Get-FileHash is a CMDLET in pwsh 7 (a read failure throws) but a script FUNCTION in Windows
# PowerShell 5.1 (a read failure is a non-terminating error -> a null hash). The emitted text guards
# that with -ErrorAction Stop + non-empty checks; this harness cannot reproduce the 5.1 behaviour.
#
# Run: tests/pwsh/run_dantesync_tray_swap_1372.sh (it emits the program and needs pwsh).
# pwsh 7 is not Windows PowerShell 5.1: this proves the logic, not 5.1 compatibility.
param([Parameter(Mandatory = $true)][string]$Program)

$env:TEMP = [System.IO.Path]::GetTempPath()
$script:Exe = 'C:\Program Files\DanteSync\dantesync-tray.exe'
$script:TmpNew = Join-Path $env:TEMP 'dantesync-tray-new.exe'

function Reset-Node([int]$Respawn, [string]$Unkillable, [bool]$Lock33, [bool]$SpawnAfterCopy) {
    $script:S = @{
        procs = [System.Collections.ArrayList]@()
        nextId = 100
        respawn = $Respawn
        unkillable = $Unkillable
        lock33 = $Lock33
        spawnAfterCopy = $SpawnAfterCopy
        files = @{}
        started = 0
    }
    $script:S.files[$script:Exe] = 'OLDHASH'
    [void](New-Tray -Unkillable:($Unkillable -eq 'initial'))
}
function New-Tray([switch]$Unkillable) {
    $p = [pscustomobject]@{
        Id = $script:S.nextId; ProcessName = 'dantesync-tray'; SessionId = 1
        ParentProcessId = 4242; Unkillable = [bool]$Unkillable
    }
    $script:S.nextId++
    [void]$script:S.procs.Add($p)
    $p
}
function Get-Process {
    [CmdletBinding()] param([string]$Name, [int[]]$Id)
    if ($PSBoundParameters.ContainsKey('Id')) { return @($script:S.procs | Where-Object { $Id -contains $_.Id }) }
    return @($script:S.procs)
}
function Stop-Process {
    [CmdletBinding()] param([Parameter(ValueFromPipeline = $true)]$InputObject, [int[]]$Id, [switch]$Force)
    process {
        if ($null -eq $InputObject) { return }
        $victim = @($script:S.procs | Where-Object { $_.Id -eq $InputObject.Id -and -not $_.Unkillable })
        foreach ($v in $victim) { $script:S.procs.Remove($v) }
        if ($victim.Count -gt 0 -and $script:S.respawn -gt 0) {
            $script:S.respawn--
            [void](New-Tray -Unkillable:($script:S.unkillable -eq 'respawn'))
        }
    }
}
function Wait-Process { [CmdletBinding()] param([string]$Name, [int[]]$Id, $Timeout) }
function Get-CimInstance {
    [CmdletBinding()] param([Parameter(Position = 0)]$ClassName, $Filter)
    $n = [int]($Filter -replace 'ProcessId=', '')
    if ($n -eq 4242) {
        return [pscustomobject]@{ Name = 'svchost.exe'; CommandLine = 'C:\Windows\system32\svchost.exe -k netsvcs -p -s Schedule' }
    }
    $p = @($script:S.procs | Where-Object { $_.Id -eq $n })
    if ($p.Count -gt 0) { return $p[0] }
    return $null
}
function Test-Path { [CmdletBinding()] param([Parameter(Position = 0)]$Path) $script:S.files.ContainsKey($Path) }
function Copy-Item {
    [CmdletBinding()] param([switch]$Force, [Parameter(Position = 0)]$Path, [Parameter(Position = 1)]$Destination)
    if ($Destination -eq $script:Exe -and $script:S.procs.Count -gt 0) {
        throw [System.IO.IOException]::new("The process cannot access the file '$Destination' because it is being used by another process.", -2147024864)
    }
    if ($Destination -eq $script:Exe -and $Path -eq $script:TmpNew -and $script:S.lock33) {
        $script:S.files[$Destination] = 'PARTIAL'
        throw [System.IO.IOException]::new('The process cannot access the file because another process has locked a portion of the file.', -2147024863)
    }
    $script:S.files[$Destination] = $script:S.files[$Path]
    # the relaunch source starts the tray again right after the new exe landed
    if ($Destination -eq $script:Exe -and $Path -eq $script:TmpNew -and $script:S.spawnAfterCopy) { [void](New-Tray) }
}
function Get-FileHash {
    [CmdletBinding()] param([string]$Algorithm, [Parameter(Position = 0)]$Path, $LiteralPath)
    if ($LiteralPath) { $Path = $LiteralPath }
    [pscustomobject]@{ Hash = $script:S.files[$Path] }
}
function Invoke-WebRequest {
    [CmdletBinding()] param([switch]$UseBasicParsing, $Uri, $OutFile)
    if ($Uri -like '*.sha256') { $script:S.files[$OutFile] = 'NEWHASH  dantesync-tray-windows-amd64.exe' }
    else { $script:S.files[$OutFile] = 'NEWHASH' }
}
function Get-Content { [CmdletBinding()] param([Parameter(Position = 0)]$Path) $script:S.files[$Path] }
function Remove-Item { [CmdletBinding()] param([switch]$Force, [Parameter(Position = 0)]$Path) }
function New-ScheduledTaskAction { [CmdletBinding()] param($Execute) 'action' }
function New-ScheduledTaskPrincipal { [CmdletBinding()] param($GroupId, $RunLevel) 'principal' }
function New-ScheduledTaskSettingsSet { [CmdletBinding()] param([switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, $ExecutionTimeLimit) 'settings' }
function Register-ScheduledTask { [CmdletBinding()] param($TaskName, $Action, $Principal, $Settings, [switch]$Force) 'task' }
function Start-ScheduledTask { [CmdletBinding()] param($TaskName) $script:S.started++; [void](New-Tray) }
function Unregister-ScheduledTask { [CmdletBinding()] param($TaskName, $Confirm) }

function Invoke-Case {
    param([string]$Name, [int]$Respawn, [string]$Unkillable, [bool]$Lock33, [bool]$SpawnAfterCopy,
          [string]$Want, [string[]]$Has, [string[]]$HasNot, [string]$Exe, [int]$Trays, [int]$Starts)
    Reset-Node -Respawn $Respawn -Unkillable $Unkillable -Lock33 $Lock33 -SpawnAfterCopy $SpawnAfterCopy
    $out = . $Program
    $line = [string](@($out | Where-Object { "$_" -like 'TRAY*' }) | Select-Object -Last 1)
    $why = @()
    if (-not $line.StartsWith($Want)) { $why += "line does not start with '$Want'" }
    foreach ($h in $Has) { if (-not $line.Contains($h)) { $why += "missing '$h'" } }
    foreach ($h in $HasNot) { if ($line.Contains($h)) { $why += "unexpected '$h'" } }
    if ($script:S.files[$script:Exe] -ne $Exe) { $why += ('exe=' + $script:S.files[$script:Exe] + ' want ' + $Exe) }
    if ($script:S.procs.Count -ne $Trays) { $why += ('trays=' + $script:S.procs.Count + ' want ' + $Trays) }
    if ($script:S.started -ne $Starts) { $why += ('task starts=' + $script:S.started + ' want ' + $Starts) }
    if ($why.Count -gt 0) {
        Write-Host ('FAIL ' + $Name + ': ' + ($why -join '; ') + "`n     " + $line)
        return $false
    }
    Write-Host ('ok   ' + $Name + ': ' + $line)
    return $true
}

$results = @(
    (Invoke-Case -Name 'no relaunch' -Respawn 0 -Unkillable '' -Lock33 $false -Want 'TRAY OK: ' `
        -Has @('sha256 NEWHASH') -HasNot @('relaunched', 'kept') -Exe 'NEWHASH' -Trays 1 -Starts 1)
    (Invoke-Case -Name 'relaunched once' -Respawn 1 -Unkillable '' -Lock33 $false -Want 'TRAY OK: ' `
        -Has @('killed a relaunched tray before the swap, started by: svchost.exe (C:\Windows\system32\svchost.exe -k netsvcs -p -s Schedule)') `
        -HasNot @() -Exe 'NEWHASH' -Trays 1 -Starts 1)
    (Invoke-Case -Name 'keeps relaunching' -Respawn 99 -Unkillable '' -Lock33 $false `
        -Want 'TRAY-WARNING: a tray keeps relaunching: svchost.exe' `
        -Has @('the previous tray exe is untouched') -HasNot @('found 2', 'did not die') -Exe 'OLDHASH' -Trays 1 -Starts 0)
    (Invoke-Case -Name 'killed tray survives' -Respawn 0 -Unkillable 'initial' -Lock33 $false `
        -Want 'TRAY-WARNING: tray: the killed tray (pid 100) did not exit' `
        -Has @() -HasNot @('found 2') -Exe 'OLDHASH' -Trays 1 -Starts 0)
    (Invoke-Case -Name 'relaunched tray survives' -Respawn 1 -Unkillable 'respawn' -Lock33 $false `
        -Want 'TRAY-WARNING: a tray did not die (pid 101)' `
        -Has @('the previous tray exe is untouched') -HasNot @('keeps relaunching') -Exe 'OLDHASH' -Trays 1 -Starts 0)
    (Invoke-Case -Name 'lock violation after a partial write' -Respawn 0 -Unkillable '' -Lock33 $true `
        -Want 'TRAY-WARNING: tray swap failed, the previous tray restored' `
        -Has @() -HasNot @('untouched') -Exe 'OLDHASH' -Trays 1 -Starts 1)
    (Invoke-Case -Name 'relaunched right after the swap' -Respawn 0 -Unkillable '' -Lock33 $false -SpawnAfterCopy $true `
        -Want 'TRAY OK: ' -Has @('kept a tray that was running again, not launched') -HasNot @('killed a relaunched') `
        -Exe 'NEWHASH' -Trays 1 -Starts 0)
)
$failed = @($results | Where-Object { -not $_ }).Count
Write-Host ("tray swap cases: " + ($results.Count - $failed) + '/' + $results.Count + ' ok')
if ($failed -gt 0) { exit 1 }
exit 0
