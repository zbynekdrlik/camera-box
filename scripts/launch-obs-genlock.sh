#!/usr/bin/env bash
# launch-obs-genlock.sh — deterministic, env-safe OBS (re)launch wrapper for the genlock boxes (#128).
#
# WHY (#128, the recurring "stale-env trap"): every OBS relaunch (deploy, crash-recovery, reboot,
# config change) MUST come up carrying the four REQUIRED genlock env vars
#   OBS_GENLOCK_WALL_CLOCK   — the wall-clock render-tick master gate (#136)
#   OBS_GENLOCK_RESERVE_MS   — the held latency in ms, #184/#235-validated = 3 (now the BACK-COMPAT
#                              ALIAS of the canonical OBS_GENLOCK_LATENCY_MS, #235)
#   OBS_GENLOCK_TS_ALIGN     — timestamp-aligned multi-source release (#136; implied ON by the ms knob, #235)
#   OBS_GENLOCK_PRELOAD_FRAMES — legacy/internal FIFO depth default (#70/#97; auto-derived under the ms knob, #235)
# plus the OPTIONAL canonical single latency knob (carried only when set in Machine — a re-pinned box):
#   OBS_GENLOCK_LATENCY_MS   — THE single user-facing genlock latency in ms (#235); wins over the
#                              RESERVE_MS alias. A legacy box without it still launches on the alias.
# If the launching process is a LONG-LIVED win-* MCP shell whose environment snapshot PREDATES the
# Machine-scope env write, the spawned obs64 inherits the STALE snapshot → the var is UNSET → the
# render tick is silently OFF and the whole genlock guarantee is gone, INVISIBLY (issue #128, the
# #126 deploy near-miss). A `$env:` read in that stale shell agrees with the wrong value, so it
# can't be trusted; the AUTHORITATIVE values are the persistent Machine-scope (HKLM) env, which
# survives reboot (drift-guard #45) and is what a fresh-boot AHK-launched OBS reads correctly.
#
# THE FIX, in ONE self-contained PowerShell program (so a human/agent relaunching OBS CANNOT lose
# the env): read the four vars FRESH from Machine scope, set them EXPLICITLY in the spawning shell
# (defeats the stale snapshot), clear stale crash sentinels, Start-Process obs64 with
# cwd=bin\64bit (wrong cwd → "Failed to find locale" broken OBS), then VERIFY and FAIL LOUDLY:
#   (a) the launched obs64's CHILD PEB env actually holds all four vars matching Machine, AND
#   (b) the OBS log shows `genlock: ... render tick ENABLED` and `sub-frame jitter reserve = N ms`.
# A relaunch that can't prove BOTH exits non-zero — never a silent half-genlocked box.
#
# HOW THE PIECES FIT (same model as scripts/recording-verdict-on-stream.sh — scp/ssh to Windows is
# DENIED on this rig, so the agent drives the win-* MCP): this script is the PURE, testable PLANNER.
# Given the box + obs install dir, it PRINTS the exact PowerShell program to paste into the box's
# `win-strih` / `win-stream-snv` MCP `Shell`. It runs NO PowerShell itself and needs no Windows
# access — the Rust unit tests (tests/launch_obs_genlock.rs) source it and assert the emitted
# program is well-formed (reads Machine env, sets $env explicit, cwd=bin\64bit, PEB+log verify,
# fails loud). The emitted program is idempotent and self-verifying on the box.
#
# Usage (planner mode — prints the PowerShell launch+verify program + the MCP plan):
#   scripts/launch-obs-genlock.sh --box strih            # uses the strih defaults
#   scripts/launch-obs-genlock.sh --box stream           # uses the stream defaults
#   scripts/launch-obs-genlock.sh --box strih --force    # force-kill a wedged obs64 first (obs-ops recovery)
#   scripts/launch-obs-genlock.sh --box strih \
#       --obs-dir 'C:\Program Files\obs-studio'          # override the OBS install dir
#
# Exit codes: 0 = plan printed, 2 = usage error. (The on-box PowerShell program's own exit code —
# 0 healthy genlock / non-zero fail-loud — is reported by the MCP Shell when the agent runs it.)
set -euo pipefail

# The four REQUIRED genlock env vars carried on EVERY launch (the #128 set). The held latency is
# #184/#235-validated = 3 ms; the wrapper reads the LIVE Machine value (so a future re-pin needs only
# a setx + relaunch), and the verify asserts the child PEB matches Machine — single source of truth,
# no hard-coded drift. OBS_GENLOCK_RESERVE_MS stays REQUIRED as the #235 back-compat alias (so a box
# pinned only on reserve keeps working unchanged); the new canonical OBS_GENLOCK_LATENCY_MS is carried
# OPTIONALLY (only when set in Machine) so a box that has not yet been re-pinned to the new knob name
# still launches cleanly.
GENLOCK_VARS=(OBS_GENLOCK_WALL_CLOCK OBS_GENLOCK_RESERVE_MS OBS_GENLOCK_TS_ALIGN OBS_GENLOCK_PRELOAD_FRAMES)

# #235: the canonical single latency knob, carried OPTIONALLY (only if set in Machine — a re-pinned
# box sets this; a legacy box on the reserve alias does not, and must still launch). When set it WINS
# over the reserve alias in libobs (genlock_latency_ms resolution).
GENLOCK_OPTIONAL_VARS=(OBS_GENLOCK_LATENCY_MS)

# --- PURE functions (no network, no MCP, no Windows — unit-tested by sourcing this script) --------

# genlock_var_list -> space-separated list of the four REQUIRED genlock var names (single source of truth).
genlock_var_list() { printf '%s ' "${GENLOCK_VARS[@]}"; }

# genlock_optional_var_list -> space-separated list of the OPTIONAL canonical genlock var(s) (#235).
genlock_optional_var_list() { printf '%s ' "${GENLOCK_OPTIONAL_VARS[@]}"; }

# --- #247 TEST/EVENT-mode #111 burn handling -----------------------------------------------------
# The deterministic rig TEST/EVENT switch (#247) drives this wrapper with --mode {test|event}:
#   TEST  : launch OBS with the #111 burn ON (OBS_BURN_QR=1 + OBS_BURN_QR_PX + the per-box run_id),
#           set ONLY in the launch shell — NEVER Machine scope. A Machine-scope OBS_BURN_* survives
#           reboot and paints QR onto the LIVE broadcast (the #246 incident); a launch-shell var dies
#           with the OBS process, so it can never contaminate prod.
#   EVENT : launch OBS with the burn OFF — clear the burn vars in the launch shell AND assert the
#           Machine scope carries NONE (refuse to launch otherwise). This is the #246 readiness guard.
# The genlock env is carried from Machine in BOTH modes exactly as before (only the burn changes).

# The reserved per-node #111 burn run_ids — SINGLE SOURCE OF TRUTH mirrors the Rust consts in
# src/probe/recording_latency.rs (BURN_RUN_ID_STRIH / BURN_RUN_ID_STREAM); the recording-verdict
# decoder pairs hops on these exact ids, so they must never drift between the painter/launch side
# and the decode side.
BURN_RUN_ID_STRIH=911002
BURN_RUN_ID_STREAM=911004
# The TEST-mode burn QR module size in px — the validated small node-stamp burn (#226 native-1080p
# recording keeps the ~300px burn crisp; a 4K-rescaled recording softened it).
BURN_QR_PX=300
# The #111 burn env var names cleared + asserted-absent in EVENT mode (the #246 guard set).
BURN_VARS=(OBS_BURN_QR OBS_BURN_QR_PX OBS_BURN_RUN_ID)

# burn_var_list -> space-separated list of the three #111 burn env var names (single source of truth).
burn_var_list() { printf '%s ' "${BURN_VARS[@]}"; }

# burn_run_id_for_box BOX -> the reserved #111 burn run_id for strih|stream (mirrors the Rust consts).
# An unknown box is a HARD error (return 2) — never silently emit a wrong/zero run_id into a live burn.
burn_run_id_for_box() {
  case "${1:-}" in
    strih)  printf '%s' "$BURN_RUN_ID_STRIH" ;;
    stream) printf '%s' "$BURN_RUN_ID_STREAM" ;;
    *) echo "burn_run_id_for_box: unknown box '${1:-}' (expected strih|stream)" >&2; return 2 ;;
  esac
}

# _burn_env_block MODE RUN_ID -> the PowerShell snippet that sets (TEST) or clears+asserts-Machine-clean
# (EVENT) the #111 burn env in the LAUNCH SHELL, BEFORE Start-Process. Built with a QUOTED heredoc
# (literal $ / backticks) + a RUN_ID placeholder, then substituted — the same verbatim-interpolation
# model as kill_block (bash expands the heredoc once, so the snippet's literal $ land as PowerShell $).
# Empty MODE -> empty string (a default launch is byte-identical to the pre-#247 wrapper).
_burn_env_block() {
  local mode="$1" run_id="${2:-}" blk
  case "$mode" in
    test)
      blk=$(cat <<'PSBURNON'
# --mode test (#247): TEST-mode burns ON. Set the #111 burn vars EXPLICITLY in THIS launch shell ONLY
# (Set-Item Env:) — NEVER Machine scope. A Machine-scope OBS_BURN_* survives reboot and paints QR on
# the LIVE broadcast (the #246 incident); the launch-shell scope dies with the OBS process.
Set-Item -Path Env:OBS_BURN_QR     -Value '1'
Set-Item -Path Env:OBS_BURN_QR_PX  -Value '__BURN_QR_PX__'
Set-Item -Path Env:OBS_BURN_RUN_ID -Value '__BURN_RUN_ID__'
Write-Host "set OBS_BURN_QR=1 OBS_BURN_QR_PX=__BURN_QR_PX__ OBS_BURN_RUN_ID=__BURN_RUN_ID__ (launch shell ONLY, #247 TEST mode)"
PSBURNON
)
      blk="${blk//__BURN_RUN_ID__/$run_id}"
      printf '%s' "${blk//__BURN_QR_PX__/$BURN_QR_PX}"
      ;;
    event)
      cat <<'PSBURNOFF'
# --mode event (#247): EVENT-mode burns OFF. Clear any burn var in THIS launch shell AND assert the
# Machine scope carries NONE — a Machine-scope OBS_BURN_* survives reboot -> QR on the live broadcast
# (the #246 incident). Refuse to launch (exit 8) if any burn var is still set in Machine.
foreach ($bn in @('OBS_BURN_QR','OBS_BURN_QR_PX','OBS_BURN_RUN_ID')) { Remove-Item "Env:$bn" -ErrorAction SilentlyContinue }
$burnLeak = @()
foreach ($bn in @('OBS_BURN_QR','OBS_BURN_QR_PX','OBS_BURN_RUN_ID')) {
  $mv = [System.Environment]::GetEnvironmentVariable($bn, 'Machine')
  if ($null -ne $mv -and $mv -ne '') { $burnLeak += "$bn=$mv" }
}
if ($burnLeak.Count -gt 0) {
  Write-Error "#246 BURN LEAK: Machine scope carries burn var(s): $($burnLeak -join ', '). A Machine-scope burn survives reboot and paints QR on the LIVE broadcast. Clear it first: foreach (`$n in 'OBS_BURN_QR','OBS_BURN_QR_PX','OBS_BURN_RUN_ID') { [Environment]::SetEnvironmentVariable(`$n, `$null, 'Machine') }"
  exit 8
}
Write-Host "EVENT mode (#247): burns cleared in launch shell + verified ABSENT from Machine (#246 guard passed)"
PSBURNOFF
      ;;
    "") : ;;
  esac
}

# _burn_verify_block MODE RUN_ID -> the PowerShell snippet (run AFTER the child PEB env dump) that
# proves the launched obs64 actually carries the burns (TEST) or carries NONE (EVENT, the #246
# post-launch guard). Sets $burnOk; folded into the final verdict only when a mode is set.
_burn_verify_block() {
  local mode="$1" run_id="${2:-}" blk
  case "$mode" in
    test)
      blk=$(cat <<'PSVBON'
# (#247 TEST) belt-and-suspenders: prove the launched obs64's CHILD PEB actually carries the burn vars
# (a stale launch-shell snapshot could otherwise drop them, the same trap as the genlock vars).
$burnOk = $true
$wantBurn = [ordered]@{ 'OBS_BURN_QR' = '1'; 'OBS_BURN_QR_PX' = '__BURN_QR_PX__'; 'OBS_BURN_RUN_ID' = '__BURN_RUN_ID__' }
$childBurn = @{}
foreach ($line in ($dump -split "`n")) { if ($line -match '^(OBS_BURN_[^=]+)=(.*)$') { $childBurn[$matches[1]] = $matches[2] } }
foreach ($bn in $wantBurn.Keys) {
  if ($childBurn[$bn] -ne $wantBurn[$bn]) { Write-Error "#247 TEST burn MISMATCH: obs64 $bn='$($childBurn[$bn])' want '$($wantBurn[$bn])' — burns NOT carried."; $burnOk = $false }
  else { Write-Host "PEB OK (burn): $bn=$($childBurn[$bn])" }
}
PSVBON
)
      blk="${blk//__BURN_RUN_ID__/$run_id}"
      printf '%s' "${blk//__BURN_QR_PX__/$BURN_QR_PX}"
      ;;
    event)
      cat <<'PSVBOFF'
# (#247 EVENT) prove the launched obs64's CHILD PEB carries NO OBS_BURN_* — the post-launch #246 guard.
# The Machine assertion + the launch-shell clear above should already guarantee it; this confirms it
# on the actual running process, immune to any stale snapshot.
$burnOk = $true
$childBurn = @()
foreach ($line in ($dump -split "`n")) { if ($line -match '^(OBS_BURN_[^=]+)=') { $childBurn += $matches[1] } }
if ($childBurn.Count -gt 0) { Write-Error "#246 EVENT burn LEAK: obs64 still carries $($childBurn -join ', ') — relaunch clean."; $burnOk = $false }
else { Write-Host "PEB OK (event): obs64 carries NO OBS_BURN_* (#246 clean)" }
PSVBOFF
      ;;
    "") : ;;
  esac
}

# build_launch_program OBS_DIR FORCE [MODE] [BURN_RUN_ID] -> the full PowerShell program that
# (re)launches OBS carrying the genlock env FRESH from Machine scope and then verifies+fails-loud.
# OBS_DIR is the OBS install root (its bin\64bit is the mandatory cwd). FORCE="1" inserts a documented
# force-kill of a wedged obs64 first (obs-ops recovery — this is a DEV rig, "kludne ho killni");
# FORCE="0" aborts if obs64 is already running (relaunch deliberately, never double-launch).
# MODE (#247, OPTIONAL — default "" preserves the exact pre-#247 behavior) selects the #111 burn
# handling: "test" sets the burns in the launch shell + verifies them in the child PEB; "event"
# clears them + asserts Machine carries none + verifies the child PEB is burn-free (the #246 guard).
# BURN_RUN_ID is the per-box reserved burn run_id (only used when MODE=test).
#
# Pure string builder so a unit test can assert the program is well-formed without a Windows host.
# Heredoc body is a literal PowerShell here-string — bash-level interpolation is ONLY $OBS_DIR / the
# FORCE branch / the var list / the #247 burn blocks; everything else (PowerShell $vars) is literal.
build_launch_program() {
  local obs_dir="$1" force="$2" mode="${3:-}" burn_run_id="${4:-}"
  local bin64="${obs_dir}\\bin\\64bit"
  local exe="${bin64}\\obs64.exe"
  # Escape for the PowerShell SINGLE-quoted strings below: a literal ' is doubled to '' (the
  # PowerShell single-quote escape), so an OBS dir containing a quote can't break out of the
  # '...' string. The default 'C:\Program Files\obs-studio' has none; this hardens an override.
  local obs_dir_ps="${obs_dir//\'/\'\'}"
  local bin64_ps="${bin64//\'/\'\'}"
  local exe_ps="${exe//\'/\'\'}"

  # The kill branch (only when --force) — documented obs-ops recovery for a wedged OBS.
  local kill_block=""
  if [ "$force" = "1" ]; then
    kill_block=$(cat <<'PSKILL'
# --force: documented obs-ops recovery — force-kill a wedged obs64 before relaunch (DEV rig).
Get-Process obs64 -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.Id -Force }
Start-Sleep -Seconds 2
PSKILL
)
  else
    kill_block=$(cat <<'PSNOKILL'
# No --force: refuse to double-launch a running obs64 (relaunch deliberately; use --force for a wedged one).
if (Get-Process obs64 -ErrorAction SilentlyContinue) {
  Write-Error "obs64 already running — relaunch deliberately (--force to recover a wedged one)."; exit 3
}
PSNOKILL
)
  fi

  # #247 per-mode #111 burn blocks (empty for a default no-mode launch -> byte-identical to pre-#247).
  # burn_env_block sets/clears the burn env in the launch shell; burn_verify_block proves the child
  # PEB matches; verdict_cond folds $burnOk into the final verdict ONLY when a mode is set.
  local burn_env_block burn_verify_block verdict_cond
  burn_env_block="$(_burn_env_block "$mode" "$burn_run_id")"
  burn_verify_block="$(_burn_verify_block "$mode" "$burn_run_id")"
  # The PowerShell $pebOk/$tickOk/$burnOk are LITERAL here (assembled into the emitted program, not
  # expanded by bash) — the single quotes are intentional.
  # shellcheck disable=SC2016
  verdict_cond='$pebOk -and $tickOk'
  # shellcheck disable=SC2016
  [ -n "$mode" ] && verdict_cond='$pebOk -and $tickOk -and $burnOk'

  # The verify block reads the launched child's PEB env via NtQueryInformationProcess +
  # ReadProcessMemory (the win-* MCP `$env:` read is a STALE snapshot — only the child's own PEB
  # proves what obs64 actually inherited) AND scans the fresh OBS log for the render-tick +
  # jitter-reserve lines. Anything short of BOTH → non-zero exit (fail loud, never a silent
  # half-genlocked box). The C# is compiled once via Add-Type (confirmed available on both boxes).
  cat <<PS
# ===== #128 deterministic genlock OBS (re)launch + verify (paste into the box's win-* MCP Shell) =====
\$ErrorActionPreference = 'Stop'
\$genlockVars = @($(printf "'%s'," "${GENLOCK_VARS[@]}" | sed 's/,$//'))
# #235: the canonical single latency knob, carried only when it is set in Machine (a re-pinned box).
\$genlockOptionalVars = @($(printf "'%s'," "${GENLOCK_OPTIONAL_VARS[@]}" | sed 's/,$//'))

# (1) Read the REQUIRED genlock vars FRESH from Machine scope (the persistent HKLM source of truth,
#     survives reboot) and (2) set them EXPLICITLY in THIS shell, so the spawned obs64 inherits the
#     CORRECT values regardless of any stale env snapshot in the long-lived MCP/launcher process (#128).
foreach (\$n in \$genlockVars) {
  \$v = [System.Environment]::GetEnvironmentVariable(\$n, 'Machine')
  if (\$null -eq \$v -or \$v -eq '') { Write-Error "Machine env \$n is UNSET — set it (setx /M) before launching genlock OBS (#128)."; exit 4 }
  Set-Item -Path "Env:\$n" -Value \$v
  Write-Host "set \$n=\$v (from Machine)"
}
# #235: carry the OPTIONAL canonical latency knob only when it is set in Machine (a legacy box on the
# reserve alias does not have it and must still launch — it is NOT required, just preferred when present).
\$genlockOptionalSet = @{}
foreach (\$n in \$genlockOptionalVars) {
  \$v = [System.Environment]::GetEnvironmentVariable(\$n, 'Machine')
  if (\$null -ne \$v -and \$v -ne '') { Set-Item -Path "Env:\$n" -Value \$v; \$genlockOptionalSet[\$n] = \$v; Write-Host "set \$n=\$v (from Machine, #235 canonical knob)" }
  else { Write-Host "(optional \$n not set in Machine — using the OBS_GENLOCK_RESERVE_MS alias, #235 back-compat)" }
}

# (2b) #247 per-mode #111 burn env — set in THIS launch shell only (test) or cleared + Machine-asserted
#      clean (event). Empty for a default no-mode launch (byte-identical to the pre-#247 wrapper).
${burn_env_block}

${kill_block}

# (3) Clear stale crash sentinels so OBS does not pop the "Crash Detected" modal and hang headless.
Remove-Item "\$env:APPDATA\\obs-studio\\.sentinel\\*" -Force -ErrorAction SilentlyContinue

# (4) Launch obs64 with cwd = bin\\64bit (wrong cwd => "Failed to find locale/en-US.ini" broken OBS).
#     NB on strih: D:\\_APPS\\NL_STARTUP.ahk auto-respawns obs64 from this same dir, but it won't
#     double-launch once one is running, so this Start-Process wins; the PEB verify below would in
#     any case fail loud on a non-genlock AHK respawn (the safe outcome). See obs-ops skill.
\$obsDir = '${obs_dir_ps}'
\$exe    = '${exe_ps}'
if (-not (Test-Path \$exe)) { Write-Error "obs64 not found at \$exe"; exit 5 }
Start-Process -FilePath \$exe -WorkingDirectory '${bin64_ps}'

# (5) Wait for obs64 to come up and write its log (genlock lines are emitted at launch).
\$proc = \$null
for (\$i = 0; \$i -lt 30; \$i++) {
  Start-Sleep -Seconds 1
  \$proc = Get-Process obs64 -ErrorAction SilentlyContinue | Select-Object -First 1
  if (\$proc -and \$proc.WorkingSet64 -gt 100MB) { break }
}
if (-not \$proc) { Write-Error "obs64 did not start"; exit 6 }
Start-Sleep -Seconds 3

# (6a) VERIFY the launched child's PEB env actually carries all four genlock vars matching Machine.
#      The MCP \$env: read is a stale snapshot; only the child's own PEB proves what obs64 inherited.
\$cs = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PebEnv {
  [DllImport("ntdll.dll")] static extern int NtQueryInformationProcess(IntPtr h, int cls, ref PROCESS_BASIC_INFORMATION pbi, int len, out int ret);
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(int access, bool inherit, int pid);
  [DllImport("kernel32.dll")] static extern bool ReadProcessMemory(IntPtr h, IntPtr addr, byte[] buf, int size, out int read);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] struct PROCESS_BASIC_INFORMATION { public IntPtr Reserved1; public IntPtr PebBaseAddress; public IntPtr R2a; public IntPtr R2b; public IntPtr UniqueProcessId; public IntPtr R3; }
  const int PROCESS_QUERY_INFORMATION = 0x0400;
  const int PROCESS_VM_READ = 0x0010;
  static IntPtr ReadPtr(IntPtr h, IntPtr addr){ byte[] b=new byte[8]; int r; ReadProcessMemory(h,addr,b,8,out r); return (IntPtr)BitConverter.ToInt64(b,0); }
  static int ReadInt(IntPtr h, IntPtr addr){ byte[] b=new byte[4]; int r; ReadProcessMemory(h,addr,b,4,out r); return BitConverter.ToInt32(b,0); }
  public static string Get(int pid){
    IntPtr h = OpenProcess(PROCESS_QUERY_INFORMATION|PROCESS_VM_READ, false, pid);
    if(h==IntPtr.Zero) return "ERR:open";
    var pbi=new PROCESS_BASIC_INFORMATION(); int rl;
    if(NtQueryInformationProcess(h,0,ref pbi,Marshal.SizeOf(pbi),out rl)!=0){CloseHandle(h);return "ERR:ntq";}
    IntPtr pp = ReadPtr(h, (IntPtr)((long)pbi.PebBaseAddress + 0x20));
    IntPtr envAddr = ReadPtr(h, (IntPtr)((long)pp + 0x80));
    int envSize = ReadInt(h, (IntPtr)((long)pp + 0x3F0));
    if(envSize<=0 || envSize>1048576) envSize=65536;
    byte[] buf=new byte[envSize]; int read;
    ReadProcessMemory(h, envAddr, buf, envSize, out read);
    CloseHandle(h);
    return Encoding.Unicode.GetString(buf,0,read).Replace("\0","\n");
  }
}
'@
Add-Type -TypeDefinition \$cs -ErrorAction Stop
\$dump = [PebEnv]::Get(\$proc.Id)
if (\$dump -like 'ERR:*') { Write-Error "could not read obs64 child PEB env (\$dump)"; exit 7 }
\$childEnv = @{}
foreach (\$line in (\$dump -split "\`n")) { if (\$line -match '^(OBS_GENLOCK_[^=]+)=(.*)\$') { \$childEnv[\$matches[1]] = \$matches[2] } }
\$pebOk = \$true
foreach (\$n in \$genlockVars) {
  \$want = [System.Environment]::GetEnvironmentVariable(\$n, 'Machine')
  \$got  = \$childEnv[\$n]
  if (\$got -ne \$want) { Write-Error "#128 PEB MISMATCH: obs64 \$n='\$got' but Machine='\$want' — stale-env trap, genlock NOT carried."; \$pebOk = \$false }
  else { Write-Host "PEB OK: \$n=\$got" }
}
# #235: verify the OPTIONAL canonical knob in the PEB ONLY when it was set in Machine (a re-pinned box).
foreach (\$n in \$genlockOptionalSet.Keys) {
  \$want = \$genlockOptionalSet[\$n]
  \$got  = \$childEnv[\$n]
  if (\$got -ne \$want) { Write-Error "#235 PEB MISMATCH: obs64 \$n='\$got' but Machine='\$want' — canonical latency knob NOT carried."; \$pebOk = \$false }
  else { Write-Host "PEB OK (#235 canonical): \$n=\$got" }
}

# (6c) #247 per-mode burn PEB verify — TEST: the child must carry the burns; EVENT: the child must
#      carry NONE (#246 post-launch guard). Sets \$burnOk; folded into the final verdict only with --mode.
${burn_verify_block}

# (6b) VERIFY the fresh OBS log shows the render tick ENABLED and the genlock-latency line. The log is
#      the AUTHORITATIVE runtime signal — same line drift-guard.sh genlock_from_log() keys on. #235:
#      the single-knob latency line is 'genlock: latency = N ms (≈ M frames @ Ffps)'.
\$logDir = "\$env:APPDATA\\obs-studio\\logs"
\$log = Get-ChildItem \$logDir -Filter *.txt | Sort-Object LastWriteTime -Descending | Select-Object -First 1
\$logText = if (\$log) { Get-Content \$log.FullName -Raw } else { "" }
\$tickOk    = \$logText -match 'genlock:.*render tick ENABLED'
\$latencyOk = \$logText -match 'genlock: latency = \d+ ms'
if (\$tickOk)    { Write-Host "LOG OK: render tick ENABLED" }          else { Write-Error "#128 LOG: 'render tick ENABLED' NOT found in \$(\$log.Name) — genlock master gate OFF." }
if (\$latencyOk) { Write-Host ("LOG OK: " + ([regex]::Match(\$logText,'genlock: latency = \d+ ms[^)]*\)').Value)) } else { Write-Warning "#235 LOG: 'genlock: latency = N ms' not yet emitted (printed lazily when a genlock_fifo input first activates; the PEB OBS_GENLOCK_LATENCY_MS/RESERVE_MS is already proven)." }

# (7) FINAL VERDICT — fail loud unless the child PEB carries every genlock var AND the render tick is
#     ENABLED (and, with --mode, the #247 burn state is correct — \$burnOk).
if (${verdict_cond}) {
  Write-Host "#128 OK: obs64 PID \$(\$proc.Id) launched with genlock env carried (PEB verified) and render tick ENABLED."
  exit 0
} else {
  Write-Error "#128 FAIL: genlock env NOT reliably carried — do NOT trust this box (relaunch with the wrapper)."
  exit 1
}
PS
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------------

usage() {
  cat <<'EOF'
launch-obs-genlock.sh — deterministic, env-safe OBS (re)launch wrapper for the genlock boxes (#128).

Prints the exact PowerShell program to paste into the box's win-* MCP Shell. The program reads the
four OBS_GENLOCK_* vars FRESH from Machine scope, sets them explicit in the spawning shell (defeats
the stale-env trap), clears crash sentinels, launches obs64 cwd=bin\64bit, then verifies the child
PEB env + the OBS log render-tick line and FAILS LOUD otherwise.

Usage:
  scripts/launch-obs-genlock.sh --box strih|stream [--mode test|event] [--force] \
      [--obs-dir 'C:\Program Files\obs-studio']
  scripts/launch-obs-genlock.sh --help

  --box     strih (win-strih, 10.77.9.202) or stream (win-stream-snv, 10.77.9.204) — selects the MCP.
  --mode    #247 rig mode (default: none — genlock only, no burn change):
              test  — burns ON: OBS_BURN_QR=1 + OBS_BURN_QR_PX=300 + the per-box run_id (strih 911002,
                      stream 911004) set ONLY in the launch shell (NEVER Machine), verified in the PEB.
              event — burns OFF: cleared in the launch shell AND asserted ABSENT from Machine (the #246
                      guard — refuses to launch if any OBS_BURN_* is set Machine-wide).
  --force   force-kill a wedged obs64 first (documented obs-ops recovery; DEV rig).
  --obs-dir override the OBS install root (default 'C:\Program Files\obs-studio'; its bin\64bit is cwd).

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local box="" force="0" mode=""
  local obs_dir='C:\Program Files\obs-studio'
  # `need_val FLAG` guards a value-taking flag BEFORE shift 2: a trailing flag with no value must be
  # a clean usage error (exit 2 + message), not a silent `shift 2` abort (exit 1) under set -e.
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)     need_val "$@"; box="$2"; shift 2 ;;
      --mode)    need_val "$@"; mode="$2"; shift 2 ;;
      --force)   force="1"; shift ;;
      --obs-dir) need_val "$@"; obs_dir="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  # #247: validate the mode up front (empty = the default genlock-only launch).
  case "$mode" in
    ""|test|event) : ;;
    *) echo "ERROR: --mode must be 'test' or 'event' (got '${mode}')" >&2; usage >&2; exit 2 ;;
  esac

  local mcp box_ip
  case "$box" in
    strih)  mcp="win-strih";       box_ip="10.77.9.202" ;;
    stream) mcp="win-stream-snv";  box_ip="10.77.9.204" ;;
    *) echo "ERROR: --box must be 'strih' or 'stream' (got '${box}')" >&2; usage >&2; exit 2 ;;
  esac

  # #247: TEST mode needs the per-box reserved burn run_id; event/default do not use it.
  local burn_run_id=""
  if [ "$mode" = "test" ]; then
    burn_run_id="$(burn_run_id_for_box "$box")"
  fi

  local PROGRAM
  PROGRAM="$(build_launch_program "$obs_dir" "$force" "$mode" "$burn_run_id")"

  # Per-mode one-line description for the plan header + the burn note.
  local mode_tag="" burn_note
  case "$mode" in
    test)  mode_tag=" — #247 TEST mode (burns ON, run_id=${burn_run_id})"
           burn_note="#         #247 TEST: also sets OBS_BURN_QR=1 + OBS_BURN_QR_PX=300 + OBS_BURN_RUN_ID=${burn_run_id} in the launch shell ONLY." ;;
    event) mode_tag=" — #247 EVENT mode (burns OFF, #246 guard)"
           burn_note="#         #247 EVENT: clears OBS_BURN_* in the launch shell AND refuses to launch if Machine carries any (the #246 guard)." ;;
    *)     burn_note="#         (no --mode: genlock-only launch, burns unchanged)" ;;
  esac

  cat <<PLAN
# ===== #128 genlock OBS (re)launch plan — box=${box} (${mcp}, ${box_ip})${mode_tag} =====
# scp/ssh to Windows is DENIED on this rig — the agent runs the program below via the ${mcp} MCP Shell.
#
# STEP 1: paste the following PowerShell program into:  ${mcp} Shell
#         (it reads the genlock env from Machine, sets it explicit, launches obs64 cwd=bin\\64bit,
#          and verifies the child PEB env + the OBS log render-tick line, failing LOUD otherwise)
${burn_note}
# ----------------------------------------------------------------------------------------------------
${PROGRAM}
# ----------------------------------------------------------------------------------------------------
# STEP 2: the program EXITS 0 only when the launched obs64's child PEB carries all four genlock vars
#         matching Machine AND the OBS log shows 'render tick ENABLED'${mode:+ AND the #247 ${mode} burn state is correct}.
#         A non-zero exit means the launch was NOT clean — do NOT trust the box; re-run this wrapper
#         (--force if wedged).
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
