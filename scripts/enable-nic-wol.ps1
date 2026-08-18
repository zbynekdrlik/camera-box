<#
.SYNOPSIS
  #1053 -- ENABLE + VERIFY the Windows NIC Wake-on-LAN settings on a broadcast OBS box (strih / stream)
  so a future network outage (issue 1001 class) is remotely recoverable via a magic packet.

.DESCRIPTION
  Idempotent, fail-loud helper that ENFORCES the WoL-relevant NIC advanced properties on the active
  adapter and then VERIFIES the effective wake state. STEP-0 live probe (issue 1053) proved both boxes
  are ALREADY magic-packet-enabled + wake_armed today, so on a healthy box this is a no-op VERIFY; its
  standing value is drift protection -- a driver reinstall / Windows update / box rebuild silently
  resets NIC advanced properties, and this re-asserts the exact desired state.

  This is a session-agnostic NIC/registry operation (NOT desktop-dependent), so it runs fine over the
  sanctioned ssh transport (scripts/lib/win-ssh-exec.sh win_ssh_run) OR the win-* MCP Shell -- see
  docs/wake-on-lan.md and .claude/rules/win-ssh-vs-mcp.md.

  Desired state (why each matters for a magic packet to wake the box):
    *WakeOnMagicPacket = Enabled    the magic packet wake itself
    *WakeOnPattern     = Disabled   "Only allow a magic packet to wake the computer" (no spurious wakes)
    *EEE / Green Ethernet = Disabled Energy-Efficient Ethernet powers the PHY down and commonly breaks WoL
    WakeFromPowerOff / WolShutdownLinkSpeed = enabled where the driver exposes it (wake from S4/S5)

.PARAMETER AdapterName
  NIC to operate on. Default: the adapter carrying the default route (the box's primary NIC).

.PARAMETER DryRun
  Report-only: print current-vs-desired for every property and change NOTHING. No admin needed.

.PARAMETER VerifyOnly
  Assert the desired state and exit non-zero on any drift; change NOTHING. No admin needed.

.NOTES
  Applying changes needs an ELEVATED session (Set-NetAdapterAdvancedProperty). -DryRun / -VerifyOnly
  only read, so they run unelevated. The BIOS half of WoL (standby power to the NIC in S4/S5,
  "Power On by PCI-E" / "Wake on LAN") is firmware and CANNOT be set here -- see docs/wake-on-lan.md.
#>
[CmdletBinding()]
param(
  [string]$AdapterName,
  [switch]$DryRun,
  [switch]$VerifyOnly
)

$ErrorActionPreference = 'Stop'
$ProgressPreference     = 'SilentlyContinue'

function Fail($msg) { Write-Error $msg; exit 1 }

# -DryRun (report-only) and -VerifyOnly (assert) are mutually exclusive: with both set the -DryRun
# branch would win in the loop below, never flag a critical drift, and the final gate could print a
# false "VERIFY OK". Refuse the contradiction outright.
if ($DryRun -and $VerifyOnly) { Fail "enable-nic-wol: -DryRun and -VerifyOnly are mutually exclusive." }

# --- resolve the target adapter ------------------------------------------------------------------
if ($AdapterName) {
  $ad = Get-NetAdapter -Name $AdapterName
} else {
  $route = Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue |
             Sort-Object RouteMetric | Select-Object -First 1
  if (-not $route) { Fail "enable-nic-wol: cannot resolve the default-route adapter; pass -AdapterName." }
  $ad = Get-NetAdapter -InterfaceIndex $route.InterfaceIndex
}
Write-Host ("adapter: {0} [{1}] MAC {2} {3}" -f $ad.Name, $ad.InterfaceDescription, $ad.MacAddress, $ad.Status)

$applying = -not ($DryRun -or $VerifyOnly)
if ($applying) {
  $isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()
             ).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
  if (-not $isAdmin) {
    Fail "enable-nic-wol: applying changes needs an ELEVATED session. Re-run as Administrator, or use -VerifyOnly / -DryRun."
  }
}

# --- desired NIC advanced properties -------------------------------------------------------------
# Only keywords the driver actually exposes are touched (each is looked up first); a keyword absent on
# this driver is reported SKIP, never forced. Two axes:
#   Optional : the keyword may legitimately not exist on this driver (SKIP, never a fail) vs required.
#   Critical : a drift on this property WOULD BLOCK a magic-packet wake (fails VerifyOnly) vs a
#              recommended HARDENING that a magic packet still wakes without (WARN only, verify passes).
# `--apply` sets EVERY property (critical + hardening) to its desired value regardless.
$desired = @(
  @{ Keyword = '*WakeOnMagicPacket';  Value = 1; Optional = $false; Critical = $true  }, # the magic-packet wake itself
  @{ Keyword = '*EEE';                Value = 0; Optional = $true;  Critical = $true  }, # EEE powers the PHY down -> WoL missed
  @{ Keyword = 'EnableGreenEthernet'; Value = 0; Optional = $true;  Critical = $true  },
  @{ Keyword = 'AdvancedEEE';         Value = 0; Optional = $true;  Critical = $true  },
  @{ Keyword = 'WakeFromPowerOff';    Value = 1; Optional = $true;  Critical = $true  }, # wake from S4/S5 (driver-specific)
  @{ Keyword = '*WakeOnPattern';      Value = 0; Optional = $true;  Critical = $false }  # HARDENING: "only allow a magic packet"
)

$critDrift = $false   # a wake-BLOCKING drift -> fails VerifyOnly
$warnDrift = $false   # a hardening drift -> reported, does NOT fail verify
foreach ($d in $desired) {
  $prop = Get-NetAdapterAdvancedProperty -Name $ad.Name -RegistryKeyword $d.Keyword -ErrorAction SilentlyContinue |
            Select-Object -First 1
  if (-not $prop) {
    if (-not $d.Optional) { Write-Host ("DRIFT {0}: not exposed by this driver (required)" -f $d.Keyword); $critDrift = $true }
    else                  { Write-Host ("SKIP  {0}: not exposed by this driver"            -f $d.Keyword) }
    continue
  }
  # RegistryValue is a [string[]] -- some drivers (Realtek *EEE) store a multi-element value like
  # {0,0}; a boolean enable/disable state is element 0. `@(...)[0]` normalizes both a scalar and an
  # array to that first element (a bare [0] on a scalar string would return its first CHARACTER).
  $cur = [string]@($prop.RegistryValue)[0]
  $want = [string]$d.Value
  $tag = if ($d.Critical) { 'DRIFT' } else { 'WARN ' }   # non-critical drift is a WARN, not a failure
  if ($cur -eq $want) {
    Write-Host ("OK    {0} = {1}" -f $d.Keyword, $cur)
  } elseif ($DryRun) {
    Write-Host ("WOULD-CHANGE {0}: {1} -> {2}" -f $d.Keyword, $cur, $want)
  } elseif ($VerifyOnly) {
    Write-Host ("{0} {1}: {2} (want {3})" -f $tag, $d.Keyword, $cur, $want)
    if ($d.Critical) { $critDrift = $true } else { $warnDrift = $true }
  } else {
    Set-NetAdapterAdvancedProperty -Name $ad.Name -RegistryKeyword $d.Keyword -RegistryValue ([string]$d.Value)
    Write-Host ("CHANGED {0}: {1} -> {2}" -f $d.Keyword, $cur, $want)
  }
}

# --- verify the EFFECTIVE wake state -------------------------------------------------------------
$pm = Get-NetAdapterPowerManagement -Name $ad.Name
$magicOk = ($pm.WakeOnMagicPacket -eq 'Enabled')
Write-Host ("verify: WakeOnMagicPacket = {0}" -f $pm.WakeOnMagicPacket)

# powercfg lists devices ARMED to wake the machine -- the master "Allow this device to wake the
# computer" enable (PnPCapabilities) must be on for the box to wake at all. It is device-manager /
# firmware territory (not a plain advanced property), so this helper VERIFIES it rather than forcing it.
$armed = (& powercfg /devicequery wake_armed) -join "`n"
$armedOk = $armed -match [regex]::Escape($ad.InterfaceDescription)
Write-Host ("verify: NIC in 'powercfg /devicequery wake_armed' = {0}" -f $armedOk)
if (-not $armedOk) {
  Write-Host "HINT: enable Device Manager -> NIC -> Power Management -> 'Allow this device to wake the computer'."
}

if ($warnDrift) { Write-Host "NOTE: a recommended hardening (e.g. 'only allow a magic packet') is not applied -- the box still wakes; run without -VerifyOnly (elevated) to apply it." }

if ($VerifyOnly) {
  if ($critDrift -or -not $magicOk -or -not $armedOk) { Fail "enable-nic-wol: VERIFY FAILED -- a magic packet would NOT wake this box (wake-blocking drift or NIC not armed)." }
  Write-Host "VERIFY OK: a magic packet can wake this box (magic-packet enabled + NIC armed)."
  exit 0
}
if (-not $magicOk -or -not $armedOk) {
  Fail "enable-nic-wol: post-apply verify FAILED (WakeOnMagicPacket/armed not satisfied). Check driver + Device Manager."
}
Write-Host ("DONE: NIC Wake-on-LAN {0}." -f $(if ($DryRun) { 'dry-run complete (nothing changed)' } else { 'enabled + verified' }))
exit 0
