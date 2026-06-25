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

# build_launch_program OBS_DIR FORCE -> the full PowerShell program that (re)launches OBS carrying
# the genlock env FRESH from Machine scope and then verifies+fails-loud. OBS_DIR is the OBS install
# root (its bin\64bit is the mandatory cwd). FORCE="1" inserts a documented force-kill of a wedged
# obs64 first (obs-ops recovery — this is a DEV rig, "kludne ho killni"); FORCE="0" aborts if obs64
# is already running (a healthy box should be relaunched deliberately, not double-launched).
#
# Pure string builder so a unit test can assert the program is well-formed without a Windows host.
# Heredoc body is a literal PowerShell here-string — note the bash-level interpolation is ONLY
# $OBS_DIR / the FORCE branch / the var list; everything else (PowerShell $vars) is literal.
build_launch_program() {
  local obs_dir="$1" force="$2"
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

# (7) FINAL VERDICT — fail loud unless the child PEB carries every genlock var AND the render tick is ENABLED.
if (\$pebOk -and \$tickOk) {
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
  scripts/launch-obs-genlock.sh --box strih|stream [--force] [--obs-dir 'C:\Program Files\obs-studio']
  scripts/launch-obs-genlock.sh --help

  --box     strih (win-strih, 10.77.9.202) or stream (win-stream-snv, 10.77.9.204) — selects the MCP.
  --force   force-kill a wedged obs64 first (documented obs-ops recovery; DEV rig).
  --obs-dir override the OBS install root (default 'C:\Program Files\obs-studio'; its bin\64bit is cwd).

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local box="" force="0"
  local obs_dir='C:\Program Files\obs-studio'
  # `need_val FLAG` guards a value-taking flag BEFORE shift 2: a trailing flag with no value must be
  # a clean usage error (exit 2 + message), not a silent `shift 2` abort (exit 1) under set -e.
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)     need_val "$@"; box="$2"; shift 2 ;;
      --force)   force="1"; shift ;;
      --obs-dir) need_val "$@"; obs_dir="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  local mcp box_ip
  case "$box" in
    strih)  mcp="win-strih";       box_ip="10.77.9.202" ;;
    stream) mcp="win-stream-snv";  box_ip="10.77.9.204" ;;
    *) echo "ERROR: --box must be 'strih' or 'stream' (got '${box}')" >&2; usage >&2; exit 2 ;;
  esac

  local PROGRAM
  PROGRAM="$(build_launch_program "$obs_dir" "$force")"

  cat <<PLAN
# ===== #128 genlock OBS (re)launch plan — box=${box} (${mcp}, ${box_ip}) =====
# scp/ssh to Windows is DENIED on this rig — the agent runs the program below via the ${mcp} MCP Shell.
#
# STEP 1: paste the following PowerShell program into:  ${mcp} Shell
#         (it reads the genlock env from Machine, sets it explicit, launches obs64 cwd=bin\\64bit,
#          and verifies the child PEB env + the OBS log render-tick line, failing LOUD otherwise)
# ----------------------------------------------------------------------------------------------------
${PROGRAM}
# ----------------------------------------------------------------------------------------------------
# STEP 2: the program EXITS 0 only when the launched obs64's child PEB carries all four genlock vars
#         matching Machine AND the OBS log shows 'render tick ENABLED'. A non-zero exit means the
#         genlock env was NOT carried — do NOT trust the box; re-run this wrapper (--force if wedged).
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
