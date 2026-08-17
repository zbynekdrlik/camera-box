#!/usr/bin/env bash
# avsync-watchdog-install.sh -- #812/#807 install plan (see extended header below `set` on line 7).
set -euo pipefail
#
# WHY (#812): `avsync-watchdog` (the existing scheduled task) has a BootTrigger only -- it starts
# once per boot and is never retried if it crashes or hangs mid-boot-cycle (confirmed live: the
# task's own LastTaskResult was 0xC000013A / STATUS_CONTROL_C_EXIT, frozen since 2026-07-28, with
# two orphan python PIDs from an earlier hang). This script is the PURE PLANNER (mirrors
# scripts/obs-self-heal-install.sh's own shape exactly) an agent pastes into the win-stream-snv MCP
# Shell to deploy the fixed scripts + register the scheduled tasks -- it runs NO PowerShell itself
# and needs no Windows access; tests source it and assert the emitted XML is well formed, exactly
# like tests/obs_self_heal_install.rs does for its sibling.
#
# THREE files get deployed (see scripts/avsync-watchdog.ps1 / avsync-vlc-monitor.ps1 /
# avsync-keepalive.ps1 for what each does and why) and TWO scheduled tasks change:
#   - `avsync-watchdog`      (EXISTING task, BootTrigger unchanged -- already correct for the
#                              boot-survival half; only its TARGET FILE's CONTENT changes)
#   - `avsync-vlc-monitor`   (NEW task, BootTrigger -- launches the #807 self-verifying VLC
#                              babysitter at boot, same shape as the existing watchdog task)
#   - `avsync-keepalive`     (NEW task, Repetition trigger every N min, SHIPS DISABLED -- the
#                              periodic keep-alive check this ticket exists to add)
#
# Usage (planner mode -- prints the full install plan):
#   scripts/avsync-watchdog-install.sh
#   scripts/avsync-watchdog-install.sh --interval-min 5
#
# Exit codes: 0 = plan printed, 2 = usage error.

# --- PURE functions (no network, no MCP, no Windows -- unit-tested by sourcing this script) -------

# build_boot_task_xml TASK_NAME PS1_PATH DESCRIPTION -> a Task Scheduler XML with a BootTrigger
# only (mirrors the EXISTING `avsync-watchdog` task's own shape, confirmed live via
# `schtasks /Query /TN avsync-watchdog /XML`) -- used for the NEW `avsync-vlc-monitor` task.
build_boot_task_xml() {
  local task_name="$1" ps1_path="$2" description="$3"
  local ps1_path_xml="${ps1_path//&/&amp;}"
  cat <<XML
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>${task_name}: ${description}</Description>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
    </BootTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>__RIG_USER__</UserId>
      <LogonType>InteractiveToken</LogonType>
    </Principal>
  </Principals>
  <Settings>
    <DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>true</StopIfGoingOnBatteries>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell</Command>
      <Arguments>-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "${ps1_path_xml}"</Arguments>
    </Exec>
  </Actions>
</Task>
XML
}

# build_keepalive_task_xml TASK_NAME PS1_PATH INTERVAL_MIN -> a Task Scheduler XML that runs
# PS1_PATH every INTERVAL_MIN minutes, indefinitely (Repetition trigger, mirrors
# scripts/obs-self-heal-install.sh's own build_task_xml exactly) PLUS once at boot -- code review
# caught that this design was stated in issue 812's own design comment ("every ~5 min PLUS a
# BootTrigger") but the first cut shipped the Repetition trigger only; a fresh boot would otherwise
# wait up to a full interval before the first keep-alive check ever ran. Ships with Enabled=false
# -- the supervisor enables it only after live-verifying (per the design comment: a healthy-box
# dry run must be a no-op, and a simulated-crash run must genuinely relaunch the missing process).
build_keepalive_task_xml() {
  local task_name="$1" ps1_path="$2" interval_min="$3"
  local ps1_path_xml="${ps1_path//&/&amp;}"
  cat <<XML
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>#812/#807 camera-box avsync-keepalive (${task_name}) -- periodic check-and-relaunch for watchdog.ps1 and avsync-vlc-monitor.ps1. Ships DISABLED; enable only after supervisor live-verify.</Description>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
    </BootTrigger>
    <TimeTrigger>
      <Repetition>
        <Interval>PT${interval_min}M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>__RIG_USER__</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <Enabled>false</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT2M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>-NoProfile -ExecutionPolicy Bypass -File "${ps1_path_xml}"</Arguments>
    </Exec>
  </Actions>
</Task>
XML
}

# avsync_freshness_go_no_go -> #813 GO/NO-GO self-test for the #814 grab-freshness gate: PROVE the
# pure decider yields NO-SIGNAL for a dead relay (ffmpeg rc=-5, a plausible-looking stale clip still
# on disk) rather than a stale verdict -- the exact 2h09m frozen-input case. Prints the observed
# verdict and returns 0 iff it is NO-SIGNAL. Skips gracefully (returns 0, "SKIP") when python3 is
# unavailable so the pure planner never hard-fails on a box without it; the harness runs it where
# python3 exists.
avsync_freshness_go_no_go() {
  local here gate out
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  gate="$here/avsync_freshness.py"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "GO/NO-GO SKIP: python3 not available (run on a box that has it)"
    return 0
  fi
  if [ ! -f "$gate" ]; then
    echo "GO/NO-GO FAIL: freshness gate not found at $gate" >&2
    return 1
  fi
  out="$(python3 "$gate" --grab-rc -5 --size-bytes 500000 --mtime-age-s 7200 --duration-s 35 2>&1)" || true
  case "$out" in
    "NO-SIGNAL:"*) echo "GO/NO-GO PASS: dead relay -> $out"; return 0 ;;
    *) echo "GO/NO-GO FAIL: dead relay produced '$out' (expected a NO-SIGNAL verdict)" >&2; return 1 ;;
  esac
}

# --- source-guard: when sourced (the unit tests), stop here ----------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) --------------------------------------------------------

usage() {
  cat <<'EOF'
avsync-watchdog-install.sh -- #812/#807 install plan for the stream-box avsync measurement loop,
the VLC program-audio babysitter, and the keep-alive check that covers both.

Prints the full supervisor install plan (deploy 3 files + register/update 3 scheduled tasks +
live-verify procedure) for the win-stream-snv MCP Shell to paste and run. The keep-alive task
ships DISABLED -- do not enable it until a healthy-box dry run (no false relaunch) and a
simulated-crash run (genuine relaunch of the missing process) both pass.

Usage:
  scripts/avsync-watchdog-install.sh [--interval-min 5]
  scripts/avsync-watchdog-install.sh --help

Exit codes: 0 = plan printed, 2 = usage error.
EOF
}

main() {
  local interval_min=5
  need_val() { [ "$#" -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; usage >&2; exit 2; }; }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --interval-min) need_val "$@"; interval_min="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  local mcp="win-stream-snv" box_ip="10.77.9.204"
  local vlc_monitor_xml keepalive_xml
  vlc_monitor_xml="$(build_boot_task_xml "avsync-vlc-monitor" 'C:\avsync\avsync-vlc-monitor.ps1' \
    "#807 camera-box avsync-vlc-monitor -- self-verifying VLC program-audio babysitter")"
  keepalive_xml="$(build_keepalive_task_xml "avsync-keepalive" 'C:\avsync\avsync-keepalive.ps1' "$interval_min")"

  cat <<PLAN
# ===== #812/#807 avsync install plan (${mcp}, ${box_ip}) =====
# Run this via the ${mcp} MCP Shell -- writing scripts + registering scheduled tasks is exactly
# what the win-* MCP is for here (same convention as scripts/obs-self-heal-install.sh).
#
# STEP 0: deploy the 4 script files (FileUpload/FileWrite via ${mcp}):
#   scripts/avsync-watchdog.ps1      -> C:\\avsync\\watchdog.ps1               (OVERWRITES the
#                                        existing hand-built file at the SAME path the current
#                                        \`avsync-watchdog\` task already targets -- no task-action
#                                        change needed for this one)
#   scripts/avsync-vlc-monitor.ps1   -> C:\\avsync\\avsync-vlc-monitor.ps1     (new file)
#   scripts/avsync-keepalive.ps1     -> C:\\avsync\\avsync-keepalive.ps1       (new file)
#   scripts/avsync_freshness.py      -> C:\\avsync\\avsync_freshness.py        (NEW -- the #814 pure
#                                        grab-freshness gate; avsync-watchdog.ps1 + av_sync_measure.py
#                                        both call it, so it MUST be present -- if absent, every pass
#                                        fails SAFE to NO-SIGNAL, never a stale verdict)
#
# STEP 0b: write the Discord webhook URL to C:\\avsync\\discord-webhook.txt (ONE line, the full
#          webhook URL -- this is a SECRET, never committed to the repo; paste it directly on the
#          box). Until this file exists, avsync-watchdog.ps1 runs fine but never posts to Discord
#          (fail-safe no-op, not a crash).
#
# STEP 0c: #813 GO/NO-GO -- PROVE the freshness gate rejects a dead relay before trusting the loop.
#          On dev1: source this script and run  avsync_freshness_go_no_go  (must print NO-SIGNAL).
#          On the box: python3 C:\\avsync\\avsync_freshness.py --grab-rc -5 --size-bytes 500000
#          --mtime-age-s 7200 --duration-s 35   MUST print a NO-SIGNAL line, never an offset --
#          this is the exact 2h09m stale-clip case the whole ticket exists to prevent.
#
# STEP 1: the EXISTING \`avsync-watchdog\` task needs NO XML change (its BootTrigger + action
#          already point at C:\\avsync\\watchdog.ps1, which now has the fixed content). If it is
#          not currently running, start it once: schtasks /Run /TN avsync-watchdog
#
# STEP 2: register the NEW \`avsync-vlc-monitor\` task (BootTrigger, same shape as the existing
#          watchdog task -- safe to enable immediately, it only ever starts VLC/checks freshness,
#          no destructive action):
# ----------------------------------------------------------------------------------------------------
${vlc_monitor_xml}
# ----------------------------------------------------------------------------------------------------
#   Save as C:\\avsync\\avsync-vlc-monitor-task.xml (UTF-16, Set-Content -Encoding Unicode),
#   replace __RIG_USER__ with this box's actual logged-on account (DOMAIN\\user or .\\user), then:
#   schtasks /Create /TN avsync-vlc-monitor /XML C:\\avsync\\avsync-vlc-monitor-task.xml /F
#   schtasks /Run /TN avsync-vlc-monitor
#
# STEP 3: register the NEW \`avsync-keepalive\` task (Repetition every ${interval_min} min, SHIPS
#          DISABLED -- this is the actual #812 fix: a periodic check-and-relaunch neither the
#          watchdog nor the vlc-monitor task has today):
# ----------------------------------------------------------------------------------------------------
${keepalive_xml}
# ----------------------------------------------------------------------------------------------------
#   Save as C:\\avsync\\avsync-keepalive-task.xml (UTF-16), replace __RIG_USER__, then:
#   schtasks /Create /TN avsync-keepalive /XML C:\\avsync\\avsync-keepalive-task.xml /F
#
# STEP 4: LIVE-VERIFY before enabling avsync-keepalive (never skip):
#   a) Healthy-box dry run: with both watchdog.ps1 and avsync-vlc-monitor.ps1 already running, run
#      the task manually (schtasks /Run /TN avsync-keepalive), tail
#      C:\\avsync\\avsync-keepalive.log -- MUST log "already running" for BOTH, never relaunch.
#   b) Simulated-crash run: force-kill one of the two loops
#      (Stop-Process -Name powershell -Force -- or more precisely target the PID from
#      Get-CimInstance Win32_Process filtered on CommandLine), run the task manually -- the log
#      MUST show "NOT running - launching" for the killed one, and a fresh process must appear.
#   c) Confirm heartbeats: type C:\\avsync\\avsync-watchdog-heartbeat.txt and
#      C:\\avsync\\avsync-vlc-monitor-heartbeat.txt both update every pass.
#   d) Only after (a)-(c) pass: schtasks /Change /TN avsync-keepalive /ENABLE
#
# Disable later: schtasks /Change /TN avsync-keepalive /DISABLE
PLAN
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
