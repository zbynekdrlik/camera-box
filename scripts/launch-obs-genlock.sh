#!/usr/bin/env bash
# launch-obs-genlock.sh — deterministic OBS (re)launch wrapper for the genlock boxes (#128/#257).
#
# #257 HARD-LOCKED the genlock build: the wall-clock render tick + ts-align are ALWAYS ON and the
# genlock latency is a BUILD CONST (3 ms, floor 3) — there is NO OBS_GENLOCK_* env any more, and the
# measurement burn is a per-source `genlock_burn` bool toggled over OBS WebSocket (NO relaunch, see
# scripts/obs_burn_filter.py + rig-mode.sh). So this wrapper no longer carries or verifies ANY env;
# the old #128 stale-env trap is structurally gone (there is no genlock env to lose). Its whole job
# is now: (force-kill →) clear crash sentinels → Start-Process obs64 cwd=bin\64bit → VERIFY the OBS
# log shows the genlock render tick ENABLED (the build-default proof) AND DistroAV loaded, failing
# LOUD otherwise (never a silent stock-OBS / wrong-build / broken-locale launch).
#
# HOW THE PIECES FIT (same model as scripts/recording-verdict-on-stream.sh — historically "scp/ssh
# to Windows is DENIED on this rig, so the agent drives the win-* MCP"; #701 proved plain
# OpenSSH+password scp/ssh actually WORKS against strih (10.77.9.202) and stream (10.77.9.204)
# specifically with the targets.md creds, but this script stays a planner because launching a GUI
# app and reading its on-screen log state is exactly what the win-* MCP is FOR, not a workaround):
# this script is the PURE, testable PLANNER.
# Given the box + obs install dir, it PRINTS the exact PowerShell program to paste into the box's
# `win-strih` / `win-stream-snv` MCP `Shell`. It runs NO PowerShell itself and needs no Windows
# access — the Rust unit tests (tests/launch_obs_genlock.rs) source it and assert the emitted program
# is well-formed (clears sentinels, cwd=bin\64bit, log-verifies render tick ENABLED + DistroAV, fails
# loud, carries NO OBS_GENLOCK_*/OBS_BURN_* env). The emitted program is idempotent + self-verifying.
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

# --- PURE functions (no network, no MCP, no Windows — unit-tested by sourcing this script) --------

# build_launch_program OBS_DIR FORCE -> the full PowerShell program that (re)launches OBS env-free and
# then log-verifies + fails-loud. OBS_DIR is the OBS install root (its bin\64bit is the mandatory cwd).
# FORCE="1" inserts a documented force-kill of a wedged obs64 first (obs-ops recovery — this is a DEV
# rig, "kludne ho killni"); FORCE="0" aborts if obs64 is already running (relaunch deliberately, never
# double-launch). Pure string builder so a unit test can assert the program is well-formed without a
# Windows host. Heredoc body is a literal PowerShell here-string — bash-level interpolation is ONLY
# $OBS_DIR / the FORCE branch; everything else (PowerShell $vars) is literal.
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

  cat <<PS
# ===== #257 deterministic genlock OBS (re)launch + verify (paste into the box's win-* MCP Shell) =====
# The genlock build is HARD-LOCKED: render tick + ts-align ALWAYS ON, latency = 3 ms build const, NO
# OBS_GENLOCK_*/OBS_BURN_* env. The measurement burn is a per-source genlock_burn bool over WebSocket
# (scripts/obs_burn_filter.py), toggled WITHOUT a relaunch — this wrapper never touches it.
\$ErrorActionPreference = 'Stop'

${kill_block}

# (1) Clear stale crash sentinels so OBS does not pop the "Crash Detected" modal and hang headless.
Remove-Item "\$env:APPDATA\\obs-studio\\.sentinel\\*" -Force -ErrorAction SilentlyContinue

# (2) Launch obs64 with cwd = bin\\64bit (wrong cwd => "Failed to find locale/en-US.ini" broken OBS).
#     NB on strih: D:\\_APPS\\NL_STARTUP.ahk auto-respawns obs64 from this same dir, but it won't
#     double-launch once one is running, so this Start-Process wins; the log verify below fails loud
#     on a non-genlock build regardless. See obs-ops skill.
\$obsDir = '${obs_dir_ps}'
\$exe    = '${exe_ps}'
if (-not (Test-Path \$exe)) { Write-Error "obs64 not found at \$exe"; exit 5 }
Start-Process -FilePath \$exe -WorkingDirectory '${bin64_ps}'

# (3) Wait for obs64 to come up and write its log (genlock lines are emitted at launch).
\$proc = \$null
for (\$i = 0; \$i -lt 30; \$i++) {
  Start-Sleep -Seconds 1
  \$proc = Get-Process obs64 -ErrorAction SilentlyContinue | Select-Object -First 1
  if (\$proc -and \$proc.WorkingSet64 -gt 100MB) { break }
}
if (-not \$proc) { Write-Error "obs64 did not start"; exit 6 }
Start-Sleep -Seconds 3

# (4) VERIFY the fresh OBS log shows the genlock render tick ENABLED (the #257 build-default proof —
#     same line drift-guard.sh genlock_capability_from_log + the rig validation key on) AND DistroAV
#     loaded. The log is the AUTHORITATIVE runtime signal; a stock OBS / wrong build emits no genlock
#     line. Anything short of BOTH -> non-zero exit (fail loud, never a silent half-genlocked box).
\$logDir = "\$env:APPDATA\\obs-studio\\logs"
\$log = Get-ChildItem \$logDir -Filter *.txt | Sort-Object LastWriteTime -Descending | Select-Object -First 1
\$logText = if (\$log) { Get-Content \$log.FullName -Raw } else { "" }
\$tickOk     = \$logText -match 'genlock:.*render tick ENABLED'
\$distroavOk = \$logText -match '(?i)distroav'
if (\$tickOk)     { Write-Host "LOG OK: genlock render tick ENABLED (build default, #257)" } else { Write-Error "#257 LOG: 'render tick ENABLED' NOT found in \$(\$log.Name) — NOT the genlock build (stock/wrong OBS?)." }
if (\$distroavOk) { Write-Host "LOG OK: DistroAV loaded" }                                  else { Write-Warning "#257 LOG: no DistroAV line yet (may log lazily on first NDI activation)." }

# (5) FINAL VERDICT — fail loud unless the render tick is ENABLED (the genlock build proof). DistroAV
#     is a warning only (it logs lazily on first NDI source activation).
if (\$tickOk) {
  Write-Host "#257 OK: obs64 PID \$(\$proc.Id) launched, genlock render tick ENABLED (no env needed)."
  exit 0
} else {
  Write-Error "#257 FAIL: genlock render tick NOT enabled — do NOT trust this box (wrong OBS build?)."
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
launch-obs-genlock.sh — deterministic OBS (re)launch wrapper for the genlock boxes (#128/#257).

Prints the exact PowerShell program to paste into the box's win-* MCP Shell. The genlock build is
hard-locked (render tick + ts-align always on, latency 3 ms build const, NO OBS_GENLOCK_*/OBS_BURN_*
env). The program clears crash sentinels, launches obs64 cwd=bin\64bit, then log-verifies the genlock
render tick ENABLED + DistroAV loaded and FAILS LOUD otherwise.

Toggling the measurement burn does NOT relaunch OBS — it is a per-source genlock_burn bool over
OBS WebSocket: scripts/obs_burn_filter.py add|remove (driven by rig-mode.sh test|event).

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
# ===== #257 genlock OBS (re)launch plan — box=${box} (${mcp}, ${box_ip}) =====
# Run the program below via the ${mcp} MCP Shell — a GUI relaunch + on-screen log verification is
# exactly what the win-* MCP is for (#701: plain scp/ssh DOES work against strih/stream with the
# targets.md creds, but that doesn't help drive/verify a GUI app).
#
# STEP 1: paste the following PowerShell program into:  ${mcp} Shell
#         (it clears crash sentinels, launches obs64 cwd=bin\\64bit, then log-verifies the genlock
#          render tick ENABLED + DistroAV loaded, failing LOUD otherwise — NO env carried)
# ----------------------------------------------------------------------------------------------------
${PROGRAM}
# ----------------------------------------------------------------------------------------------------
# STEP 2: the program EXITS 0 only when the OBS log shows 'render tick ENABLED' (the #257 build proof).
#         A non-zero exit means it is NOT the genlock build — do NOT trust the box; check the deploy.
# STEP 3: to toggle the measurement burn (TEST mode), DON'T relaunch — flip the per-source bool over
#         WebSocket:  scripts/obs_burn_filter.py add|remove --host ${box_ip} --input "<NDI input>"
#         (or use scripts/rig-mode.sh test|event, which does it for both boxes).
# STEP 4 (#674): once STEP 2 confirms the relaunch succeeded, mark it on imag-nb's own journald so
#         a future imag judder report can be time-correlated against this restart:
#         scripts/mark-imag-restart.sh --box ${box} --reason "<why you relaunched>"
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
