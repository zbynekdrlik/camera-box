<#
.SYNOPSIS
  Make this Windows machine's NDI receivers query every camera-box rig sender by IP (issue 1342).

.DESCRIPTION
  The rig's NDI source discovery is mDNS multicast, and on the venue network a laptop (or the stream /
  resolume OBS box) often did not list every NDI source. The NDI SDK lets a RECEIVER query a list of
  sender IPs directly, IN ADDITION to mDNS: the "extra IPs" of the finder, read from the machine-wide
  config file %ProgramData%\NDI\ndi-config.v1.json:

      ndi.networks.ips = <every managed rig sender IP, comma separated>

  Senders never read that list, so this machine's own NDI outputs keep announcing over mDNS exactly as
  before. This script does NOT configure an NDI Discovery Server, and it removes the retired rig value
  10.77.9.200 if an earlier version of this script wrote it.

  The default -Ips list is GENERATED from the repo's fleet lists (scripts/camera-set.sh +
  scripts/lib/obs-fleet.sh) by `bash scripts/lib/ndi-discovery.sh --ips pinned`; a test pins this
  default to that output. The traveling RESOLUME-SNV box has no fixed IP, so it is not in the default.
  To include it, pass the list dev1 generates with the box home:
      bash scripts/lib/ndi-discovery.sh --ips      (on dev1; resolves resolume.lan)

  Behaviour:
  - MERGES into an existing config: every other key is kept, and existing networks.ips entries are
    kept too, with the rig IPs added (this machine may list other senders of its own).
  - Backs the old file up next to it (ndi-config.v1.json.bak-<timestamp>).
  - Writes UTF-8 WITHOUT a BOM (a BOM-prefixed JSON config is the dantesync incident class: the
    reader silently falls back to defaults), then reads the file back and verifies it.
  Restart OBS (and any other NDI app) afterwards so it re-reads the config.

  The checked-in reference file is scripts\ndi-discovery\ndi-config.v1.json (the same content for a
  Linux laptop: ~/.ndi/ndi-config.v1.json). NDI Access Manager (NDI Tools) -> "Remote Sources" sets
  the same list through a GUI.

.PARAMETER Ips
  The comma-separated sender IP list to add. Default: the pinned rig list (every camera + strih-lx +
  stream).

.PARAMETER DryRun
  Print the resulting JSON and change nothing.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\ndi-discovery-laptop.ps1 -DryRun
  powershell -ExecutionPolicy Bypass -File .\ndi-discovery-laptop.ps1        (run as Administrator)
#>
[CmdletBinding()]
param(
  [string]$Ips = '10.77.9.61,10.77.9.62,10.77.9.63,10.77.9.64,10.77.9.65,10.77.9.66,10.77.9.67,10.77.9.202,10.77.9.204',
  [switch]$DryRun
)
$ErrorActionPreference = 'Stop'

$dir  = Join-Path $env:ProgramData 'NDI'
$path = Join-Path $dir 'ndi-config.v1.json'
# The part-1 NDI Discovery Server address (issue 1342). No server was ever run; a sender with a
# discovery server configured stops announcing over mDNS, so this value is removed when found.
$retiredServer = '10.77.9.200'

# Return $obj.$name, adding it with $default first when the property is missing -- or replacing it
# when it is present but null (e.g. an existing config with "ndi": null).
function Get-OrAddMember {
  param($obj, [string]$name, $default)
  if ($obj.PSObject.Properties.Name -notcontains $name) {
    $obj | Add-Member -NotePropertyName $name -NotePropertyValue $default
  } elseif ($null -eq $obj.$name) {
    $obj.$name = $default
  }
  return $obj.$name
}

# Split a comma-separated list into trimmed, non-empty entries.
function Split-IpList {
  param([string]$list)
  if ($null -eq $list) { return @() }
  return @($list -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne '' })
}

$want = @(Split-IpList $Ips)
if ($want.Count -eq 0) {
  throw "-Ips is empty -- nothing to add"
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
[void](Get-OrAddMember $net 'ips' '')
$before = [string]$net.ips

# Existing entries first (kept), then every rig IP not already listed.
$merged = New-Object System.Collections.Generic.List[string]
foreach ($ip in (@(Split-IpList $before) + $want)) {
  if (-not $merged.Contains($ip)) { $merged.Add($ip) }
}
$net.ips = ($merged -join ',')

if ($net.PSObject.Properties.Name -contains 'discovery') {
  # The earlier script accepted a comma list, so drop the key when EVERY entry is the retired server.
  $disc = @(Split-IpList ([string]$net.discovery))
  $foreign = @($disc | Where-Object { $_ -ne $retiredServer })
  if ($disc.Count -gt 0 -and $foreign.Count -eq 0) {
    $net.PSObject.Properties.Remove('discovery')
    $verb = if ($DryRun) { 'would remove' } else { 'removed' }
    Write-Output "$verb the retired discovery server $retiredServer (it silenced this machine's NDI outputs on mDNS)"
  } elseif ([string]$net.discovery -ne '') {
    Write-Warning "networks.discovery='$($net.discovery)' is set and left as is -- a configured discovery server stops this machine's own NDI outputs from announcing over mDNS"
  }
}
$json = ($cfg | ConvertTo-Json -Depth 20) + "`n"

if ($DryRun) {
  Write-Output "DRY-RUN: $path would contain (networks.ips '$before' -> '$($net.ips)'):"
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

# Read-back: no BOM, valid JSON, and every rig IP present.
$bytes = [System.IO.File]::ReadAllBytes($path)
if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
  throw "$path was written WITH a UTF-8 BOM -- refusing to leave it (the NDI reader may ignore it)"
}
$check = [System.IO.File]::ReadAllText($path) | ConvertFrom-Json
$have = @(Split-IpList ([string]$check.ndi.networks.ips))
$missing = @($want | Where-Object { $have -notcontains $_ })
if ($missing.Count -gt 0) {
  throw "read-back mismatch: networks.ips='$($check.ndi.networks.ips)' lacks $($missing -join ',')"
}
Write-Output "OK: $path networks.ips = $($check.ndi.networks.ips) (was '$before')."
Write-Output "Restart OBS and any other NDI application so it re-reads the config."
