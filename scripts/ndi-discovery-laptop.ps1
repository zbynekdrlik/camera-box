<#
.SYNOPSIS
  Point this Windows machine's NDI at the camera-box NDI Discovery Server (issue 1342).

.DESCRIPTION
  The rig's NDI source discovery used to be mDNS only, and a laptop brought to the venue often did
  not list every NDI source in OBS. The fleet now runs the NDI Discovery Server on dev1
  (10.77.9.200, port 5959). This script writes the ONE setting a Windows laptop needs into the
  machine-wide NDI config file %ProgramData%\NDI\ndi-config.v1.json:

      ndi.networks.discovery = <Server>      (the discovery server list)
      ndi.networks.ips       = ""            (a hand-kept static list goes stale -- cleared)

  It MERGES into an existing config (every other key is kept), backs the old file up next to it
  (ndi-config.v1.json.bak-<timestamp>), writes UTF-8 WITHOUT a BOM (a BOM-prefixed JSON config is
  the dantesync incident class: the reader silently falls back to defaults), then reads the file
  back and verifies it. mDNS keeps working for receiving: OBS lists both the server's sources and
  the ones it still finds via mDNS. Restart OBS (and any other NDI app) afterwards so it re-reads
  the config.

  The checked-in reference file is scripts\ndi-discovery\ndi-config.v1.json (the same content for a
  Linux laptop: copy it to ~/.ndi/ndi-config.v1.json). NDI Access Manager (NDI Tools) ->
  "Advanced" -> Discovery Server sets the same value through a GUI.

.PARAMETER Server
  The discovery server list (comma-delimited for redundancy). Default: dev1's rig IP.

.PARAMETER DryRun
  Print the resulting JSON and change nothing.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\ndi-discovery-laptop.ps1 -DryRun
  powershell -ExecutionPolicy Bypass -File .\ndi-discovery-laptop.ps1        (run as Administrator)
#>
[CmdletBinding()]
param(
  [string]$Server = '10.77.9.200',
  [switch]$DryRun
)
$ErrorActionPreference = 'Stop'

$dir  = Join-Path $env:ProgramData 'NDI'
$path = Join-Path $dir 'ndi-config.v1.json'

# Return $obj.$name, adding it with $default first when the property is missing.
function Get-OrAddMember {
  param($obj, [string]$name, $default)
  if ($obj.PSObject.Properties.Name -notcontains $name) {
    $obj | Add-Member -NotePropertyName $name -NotePropertyValue $default
  }
  return $obj.$name
}

$cfg = New-Object PSObject
if (Test-Path -LiteralPath $path) {
  # ReadAllText detects and drops a BOM, so an existing BOM-prefixed file still parses.
  $raw = [System.IO.File]::ReadAllText($path)
  if ($raw.Trim().Length -gt 0) {
    try {
      $cfg = $raw | ConvertFrom-Json
    } catch {
      throw "existing $path is not valid JSON -- fix or move it away, then re-run: $($_.Exception.Message)"
    }
  }
}
$ndi = Get-OrAddMember $cfg 'ndi' (New-Object PSObject)
$net = Get-OrAddMember $ndi 'networks' (New-Object PSObject)
[void](Get-OrAddMember $net 'discovery' '')
[void](Get-OrAddMember $net 'ips' '')
$before = $net.discovery
$net.discovery = $Server
$net.ips = ''
$json = ($cfg | ConvertTo-Json -Depth 20) + "`n"

if ($DryRun) {
  Write-Output "DRY-RUN: $path would contain (networks.discovery '$before' -> '$Server'):"
  Write-Output $json
  return
}

if (-not (Test-Path -LiteralPath $dir)) {
  New-Item -ItemType Directory -Path $dir | Out-Null
}
if (Test-Path -LiteralPath $path) {
  $bak = $path + '.bak-' + (Get-Date -Format 'yyyyMMdd-HHmmss')
  Copy-Item -LiteralPath $path -Destination $bak
  Write-Output "backup: $bak"
}
[System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))

# Read-back: no BOM, valid JSON, and the discovery value we meant to write.
$bytes = [System.IO.File]::ReadAllBytes($path)
if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
  throw "$path was written WITH a UTF-8 BOM -- refusing to leave it (the NDI reader may ignore it)"
}
$check = [System.IO.File]::ReadAllText($path) | ConvertFrom-Json
if ($check.ndi.networks.discovery -ne $Server) {
  throw "read-back mismatch: networks.discovery='$($check.ndi.networks.discovery)', expected '$Server'"
}
Write-Output "OK: $path networks.discovery = $Server (was '$before'), networks.ips cleared."
Write-Output "Restart OBS and any other NDI application so it re-reads the config."
