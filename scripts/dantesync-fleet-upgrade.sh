#!/usr/bin/env bash
# dantesync-fleet-upgrade.sh — canary-first dantesync fleet upgrade (#876). Extended header below.
set -euo pipefail
#
# WHY THIS SCRIPT EXISTS (#876): dantesync is the rig's CLOCK AUTHORITY on every box (video
# genlock + Dante audio), so a regression breaks the whole rig. The fleet drifted into FIVE
# versions across EIGHT boxes and nobody noticed until #862's version-parity gate went in — the
# most damaging pair (strih 1.8.20 vs stream 1.8.25) was invisible. Two holes caused it:
#   * Windows: the `DanteSyncUpdate` scheduled task died at the DanteTimeSync->DanteSync rename
#     (dantesync commit f8dfd6c, PR #18) and was never replaced. It still sits on strih/stream
#     `Enabled`, `Next Run = N/A`, `Last Result = 0` — enabled, never fires, exits 0 so nothing
#     ever alarmed (verified live on strih 2026-08-15). The current dantesync repo has ZERO
#     references to it — it is a pre-rename relic living only on the boxes.
#   * Linux: never had ANY upgrade mechanism. `install.sh` installs `releases/latest` ONCE at
#     provisioning and is never re-run.
# Every convergence since (incl. the current 8/8 at 1.8.41) is a manual eight-box hand-roll — the
# hand-roll IS the bug. #862's `dantesync-version-gate.sh` is the DETECTION half (loud, CI-wired);
# THIS is the missing REMEDIATION half.
#
# DESIGN (see the #876 design comment for the full rationale):
#   * Operator/agent-INVOKED, never a per-box scheduled task. A task that silently stops
#     scheduling is exactly as bad as no task (the ticket's own lesson) — so the mechanism's
#     liveness IS the #862 gate's liveness, not a silent cron. The dead Windows relic is actively
#     PURGED (schtasks /Delete), never replaced.
#   * Targets the PINNED version (default DANTESYNC_VERSION_PIN, sourced from
#     dantesync-version-gate.sh — the SAME single-source-of-truth the gate uses), NEVER
#     "releases/latest" (which would chase docs-only bumps and schedule pointless clock-master
#     redeploys — the phantom-drift concern from this ticket's own follow-up comment).
#   * CANARY-FIRST, one representative per OS CLASS present (a green Linux canary must never
#     authorize touching a Windows box; the class here is the OS — the #452 per-class insight from
#     upgrade-fleet-ndi.sh, whose structure this script mirrors). Each canary is verified
#     (version read-back == target AND dantesync-gate.sh green: PTP-locked + fresh in-bound
#     offset) BEFORE the rest of the fleet is touched; if ANY canary fails, the whole roll ABORTS
#     (rest untouched). A non-canary failure is recovered and recorded, but the loop continues
#     (one bad box doesn't abort the others).
#   * SAFETY (both OSes): the new binary is downloaded AND sha256-verified BEFORE the running
#     service is stopped (a failed download never touches the clock master), and the swap is
#     SELF-HEALING — the remote upgrade script backs up the current binary, then arms a restore
#     trap (bash `trap ... ERR` / PowerShell try-catch) so any failure AFTER the point of no
#     return rolls the binary back and restarts the service ON THE BOX before returning non-zero.
#     The orchestrator therefore only ever needs an EXTERNAL rollback on the VERIFY-failure path
#     (where the swap provably completed and the service is running the new-but-unverified binary);
#     a failed upgrade command is already recovered remotely and is only reported, never blindly
#     rolled back (which — with a pre-existing `.bak` — would otherwise stop a HEALTHY master and
#     downgrade it).
#
# REUSE, NEVER REINVENT: sources dantesync-version-gate.sh for the version PARSER
# (dantesync_version_from_version_output) + the PIN; uses dantesync-gate.sh (→ clock-offset-guard.sh
# pure parsers) as the per-canary verification gate; uses scripts/lib/cambox-offline-ack.sh +
# rig-fleet.txt for knowingly-offline exclusion — the SAME mechanism every other fleet gate uses.
# The Windows path SENDS A .ps1 (scp -O) and runs it with `-File` rather than a nested
# `powershell -Command "..."` over ssh — the repo's hard rule (.claude/rules/rig-state-inspection.md
# §2: nested PowerShell quoting through ssh fails SILENTLY, exit 0 with no output).
#
# Usage:
#   scripts/dantesync-fleet-upgrade.sh [--target VERSION] \
#       --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 imag-nb=newlevel@10.77.9.182" \
#       --win "strih=newlevel@10.77.9.202 stream=newlevel@10.77.9.204" \
#       [--local dev1] [--canary "cam1 strih"] [--dry-run] [--force]
#   scripts/dantesync-fleet-upgrade.sh --help
#
# --linux/--win entries are "name=user@ip"; --local NAME is read+upgraded on this box (dev1).
# --target defaults to DANTESYNC_VERSION_PIN (the #862 gate's pin). --canary overrides the
# per-class default (every member must be in the fleet). --dry-run reads + reports the plan,
# changes nothing. --force allows a downgrade (target OLDER than installed).
#
# Env: SSH_PASS (default newlevel; also the sudo password fed to sudo -S on non-root Linux nodes),
#      DANTESYNC_GATE_BOUND_US (offset bound, passed to the gate),
#      GATE_WAIT_TRIES/GATE_WAIT_SECS (post-restart settle poll for a SLAVE node's verification gate),
#      NTP_MASTER (the master node name, default from DANTESYNC_NTP_MASTER_NAME / strih),
#      MASTER_GATE_WAIT_TRIES/MASTER_GATE_WAIT_SECS (#1077: the LONGER bounded settle window the
#      master node gets so verifying it right after its own restart waits out the fleet sawtooth).
# Linux privilege (#1077): a root@ node runs the generated script directly; a non-root node
#      (imag-nb newlevel@, dev1 --local) runs it by FILE with sudo (sudo -n where passwordless, else
#      sudo -S fed SSH_PASS). Binary fetch: dev1 downloads+verifies ONCE and scp's the binary to each
#      Linux node (curl-less boxes like cam3 upgrade with no on-box fetch; on-box curl then wget are
#      only standalone fallbacks).
#
# Exit codes: 0 = every requested node ended on the target (or was already there) AND verified;
#   1 = usage/env error; 2 = unknown argument; 10 = a CANARY failed (recovered; rest NOT
#   touched); 20 = every canary passed but at least one other node failed (each recovered).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/dantesync-version-gate.sh
. "$HERE/dantesync-version-gate.sh"   # dantesync_version_from_version_output + DANTESYNC_VERSION_PIN (source-safe)
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"   # cambox_offline_ack_is_acked/_reason (shared exclusion)

# The GitHub release download base for the (Claude-stewarded) dantesync repo. Releases are
# ALL-OR-NOTHING (dantesync #56): a published tag always carries BOTH the Linux and Windows
# assets plus their .sha256 companions, so a pinned tag can always be fetched for either OS.
DANTESYNC_RELEASE_BASE="${DANTESYNC_RELEASE_BASE:-https://github.com/zbynekdrlik/dantesync/releases/download}"
DANTESYNC_WIN_EXE='C:\Program Files\DanteSync\dantesync.exe'
DANTESYNC_WIN_BAK='C:\Program Files\DanteSync\dantesync.exe.bak'
DANTESYNC_WIN_PS_REMOTE='C:\Windows\Temp\dantesync-fleet-upgrade.ps1'
DANTESYNC_LINUX_BIN="/usr/local/bin/dantesync"
DANTESYNC_LINUX_BAK="/usr/local/bin/dantesync.bak"
DANTESYNC_DEAD_TASK="DanteSyncUpdate"
# #1077: the orchestrator downloads + sha256-verifies the binary ONCE on dev1 and scp's it to each
# Linux node here — the generated script prefers this pre-placed binary (the curl-less path: cam3
# has no curl and a broken apt; also friendlier to the metered venue LAN). DANTESYNC_LINUX_SH_REMOTE
# is where the generated upgrade/rollback script is uploaded and run BY FILE (so a non-root node can
# `sudo -S bash <file>` without any nested-quoting hazard — mirrors the Windows -File pattern).
DANTESYNC_LINUX_STAGED="/tmp/dantesync-staged"
DANTESYNC_LINUX_SH_REMOTE="/tmp/dantesync-fleet-upgrade-remote.sh"

# --- PURE functions (no network/ssh — unit-tested from tests/dantesync_fleet_upgrade.rs) -----

# dantesync_upgrade_status CURRENT TARGET -> NEWER | SAME | OLDER | UNKNOWN.
# NEWER = TARGET is newer than CURRENT (an upgrade). OLDER = TARGET is older (a downgrade —
# refused unless --force). UNKNOWN when either side is empty (an unread version is NEVER treated
# as any ordering). Semver-numeric via `sort -V` (1.8.10 > 1.8.9), never lexical.
dantesync_upgrade_status() {
  local cur="$1" cand="$2" highest
  if [ -z "$cur" ] || [ -z "$cand" ]; then
    echo "UNKNOWN"
    return 0
  fi
  if [ "$cur" = "$cand" ]; then
    echo "SAME"
    return 0
  fi
  highest="$(printf '%s\n%s\n' "$cur" "$cand" | sort -V | tail -1)"
  if [ "$highest" = "$cand" ]; then
    echo "NEWER"
  else
    echo "OLDER"
  fi
}

# dantesync_release_url_linux VERSION -> the pinned-tag Linux asset URL. NEVER releases/latest.
dantesync_release_url_linux() {
  echo "${DANTESYNC_RELEASE_BASE}/v${1}/dantesync-linux-amd64"
}

# dantesync_release_url_windows VERSION -> the pinned-tag Windows asset URL. NEVER releases/latest.
dantesync_release_url_windows() {
  echo "${DANTESYNC_RELEASE_BASE}/v${1}/dantesync-windows-amd64.exe"
}

# dantesync_linux_upgrade_cmd VERSION -> the remote bash text to upgrade a Linux node to VERSION.
# Safety order: download + sha256-verify FIRST (a bad download never stops the daemon), THEN back
# up the current binary, THEN arm a restore-on-error trap (self-heal), THEN stop/swap/restart.
# Any failure PAST the backup point restores the previous binary and restarts before returning
# non-zero; a failure BEFORE it leaves the running clock master untouched.
dantesync_linux_upgrade_cmd() {
  local version="$1" url
  url="$(dantesync_release_url_linux "$version")"
  cat <<EOF
set -e
tmp="\$(mktemp -d)"
trap 'rm -rf "\$tmp"' EXIT
# 1. obtain the new binary BEFORE touching the running clock master. Order (#1077):
#    (a) a pre-staged binary the orchestrator downloaded+verified on dev1 and scp'd here -- the
#        curl-less path (cam3 has no curl + a broken apt) AND the metered-LAN-friendly path (one
#        download on dev1, not eight); (b) on-box curl; (c) on-box wget as a secondary. Whichever
#        path, the sha256 is re-verified below, so a corrupt scp OR download never reaches the
#        clock master. The staged file is consumed (moved into \$tmp) and removed immediately.
if [ -f "$DANTESYNC_LINUX_STAGED" ]; then
  cp -a "$DANTESYNC_LINUX_STAGED" "\$tmp/dantesync"
  cp -a "$DANTESYNC_LINUX_STAGED.sha256" "\$tmp/dantesync.sha256"
  rm -f "$DANTESYNC_LINUX_STAGED" "$DANTESYNC_LINUX_STAGED.sha256"
elif command -v curl >/dev/null 2>&1; then
  curl --fail -fsSL -o "\$tmp/dantesync" "$url"
  curl --fail -fsSL -o "\$tmp/dantesync.sha256" "$url.sha256"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "\$tmp/dantesync" "$url"
  wget -q -O "\$tmp/dantesync.sha256" "$url.sha256"
else
  echo "no staged binary at $DANTESYNC_LINUX_STAGED and neither curl nor wget is available" >&2
  exit 1
fi
expected="\$(awk '{print \$1}' "\$tmp/dantesync.sha256")"
actual="\$(sha256sum "\$tmp/dantesync" | awk '{print \$1}')"
if [ "\$expected" != "\$actual" ]; then
  echo "SHA256 MISMATCH: expected \$expected got \$actual" >&2
  exit 1
fi
chmod +x "\$tmp/dantesync"
# 1.5 cam boxes run a DELIBERATE read-only root (the deploy-fleet.sh remount cycle exists for
# exactly this; the 2026-08-16 canary failed here with 'cp: ... Read-only file system').
# Detect a read-only root, remount rw for the swap, and restore ro via the EXIT trap — so BOTH
# the success path and the self-heal ERR path end read-only again. #1077 defect (3): read the
# ACTUAL mount state (findmnt, with a /proc/mounts fallback for a findmnt-less box), never a
# 'touch' write probe — a write probe conflates a read-only filesystem with a mere permission
# error, and now that the script always runs escalated it would read as writable everywhere a
# real move is possible. Mirrors setup-device.sh's ensure_root_writable()/root_mount_is_readonly()
# (#599): match 'ro' as the FIRST comma-token so 'errors=remount-ro' never false-positives.
ro_root=0
opts="\$(findmnt -no OPTIONS / 2>/dev/null || awk '\$2=="/"{print \$4; exit}' /proc/mounts 2>/dev/null)"
case "\$opts" in ro | ro,*) ro_root=1 ;; esac
if [ "\$ro_root" = 1 ]; then mount -o remount,rw /; fi
_dantesync_remount_ro() {
  if [ "\$ro_root" = 1 ]; then mount -o remount,ro / 2>/dev/null || true; fi
}
trap 'rm -rf "\$tmp"; _dantesync_remount_ro' EXIT
# 2. back up the current binary BEFORE overwriting it (rollback target)
cp -a "$DANTESYNC_LINUX_BIN" "$DANTESYNC_LINUX_BAK"
# 3. self-heal: from here (the point of no return), restore the .bak on ANY error
_dantesync_restore() {
  cp -a "$DANTESYNC_LINUX_BAK" "$DANTESYNC_LINUX_BIN" 2>/dev/null || true
  systemctl restart dantesync 2>/dev/null || true
  echo "SELF-HEAL: restored previous dantesync binary" >&2
}
trap '_dantesync_restore' ERR
# 4. swap + restart
systemctl stop dantesync
install -m 0755 "\$tmp/dantesync" $DANTESYNC_LINUX_BIN
systemctl restart dantesync
trap 'rm -rf "\$tmp"; _dantesync_remount_ro' EXIT   # success — disarm the restore trap, keep tmp cleanup + ro restore
# 5. read the new version back
dantesync --version
EOF
}

# dantesync_linux_rollback_cmd -> restore the pre-upgrade binary from its .bak and restart. Only
# ever invoked by the orchestrator on the VERIFY-failure path (the swap provably completed).
dantesync_linux_rollback_cmd() {
  cat <<EOF
set -e
if [ ! -f "$DANTESYNC_LINUX_BAK" ]; then
  echo "no $DANTESYNC_LINUX_BAK to roll back to" >&2
  exit 1
fi
# Same read-only-root handling as the upgrade cmd (cam boxes) — restore ro on ANY exit. #1077
# defect (3): read the real mount state (findmnt, /proc/mounts fallback), never a write probe —
# mirrors setup-device.sh's ensure_root_writable() (#599); 'ro' as the FIRST comma-token.
ro_root=0
opts="\$(findmnt -no OPTIONS / 2>/dev/null || awk '\$2=="/"{print \$4; exit}' /proc/mounts 2>/dev/null)"
case "\$opts" in ro | ro,*) ro_root=1 ;; esac
if [ "\$ro_root" = 1 ]; then mount -o remount,rw /; fi
_dantesync_remount_ro() {
  if [ "\$ro_root" = 1 ]; then mount -o remount,ro / 2>/dev/null || true; fi
}
trap '_dantesync_remount_ro' EXIT
systemctl stop dantesync
cp -a "$DANTESYNC_LINUX_BAK" $DANTESYNC_LINUX_BIN
systemctl restart dantesync
dantesync --version
EOF
}

# dantesync_windows_purge_dead_task_cmd -> the idempotent forced delete of the dead DanteSyncUpdate
# relic scheduled task (the #18-rename orphan). Standalone-usable; also embedded in the upgrade ps.
dantesync_windows_purge_dead_task_cmd() {
  echo "schtasks /Delete /TN \"$DANTESYNC_DEAD_TASK\" /F"
}

# dantesync_windows_wait_service_exit_ps -> the PowerShell lines that wait for the dantesync
# PROCESS to actually exit after `Stop-Service dantesync`, before the exe is touched (#1265).
# `Stop-Service` returns when the SCM reports STOPPED, but on strih the dantesync.exe process
# lingers a few seconds after that (its Npcap capture handle on the X520 tears down slowly —
# .claude/rules/nic-swap-timesync-recovery.md §2, the box where `sc stop` "silently hung"), so an
# immediate Copy-Item hits "The process cannot access the file ... being used by another process",
# the self-heal restores the .bak and the canary ABORTS the whole roll (live 2026-09-03 19:02Z).
# Waits on the REAL resource (the process holding the exe): a bounded Wait-Process, then a forced
# Stop-Process backstop for a wedged process (the documented cure for a dead-pcap-handle
# dantesync) + a short re-wait. Exact process NAME only — never a wildcard: `dantesync-tray.exe`
# (the autostart tray, a separate process) must survive the daemon swap. Shared by the upgrade
# and the rollback .ps1 so both swap directions get the same guarantee.
dantesync_windows_wait_service_exit_ps() {
  cat <<'EOF'
    # #1265: Stop-Service returns on SCM STOPPED, but the process can linger holding the exe --
    # wait on the PROCESS (bounded), force-kill a wedged one, then re-wait before the swap.
    Wait-Process -Name dantesync -Timeout 30 -ErrorAction SilentlyContinue
    if (Get-Process -Name dantesync -ErrorAction SilentlyContinue) {
        Stop-Process -Name dantesync -Force
        Wait-Process -Name dantesync -Timeout 10 -ErrorAction SilentlyContinue
    }
EOF
}

# dantesync_windows_upgrade_ps VERSION -> the CONTENT of a PowerShell .ps1 that upgrades a Windows
# node to VERSION. Sent as a FILE (scp -O) and run with `-File` — never a nested
# `powershell -Command "..."` over ssh (which fails SILENTLY, .claude/rules/rig-state-inspection.md
# §2). Same safety order as Linux: download + Get-FileHash-verify FIRST, back up the exe, then a
# try/catch self-heal around stop/swap/start, purge the dead relic task, read the version back.
dantesync_windows_upgrade_ps() {
  local version="$1" url
  url="$(dantesync_release_url_windows "$version")"
  cat <<EOF
\$ErrorActionPreference = 'Stop'
\$url = '$url'
\$exe = '$DANTESYNC_WIN_EXE'
\$bak = '$DANTESYNC_WIN_BAK'
\$tmp = Join-Path \$env:TEMP 'dantesync-new.exe'
# 1. download + sha256-verify BEFORE touching the running clock master
Invoke-WebRequest -UseBasicParsing -Uri \$url -OutFile \$tmp
Invoke-WebRequest -UseBasicParsing -Uri (\$url + '.sha256') -OutFile (\$tmp + '.sha256')
\$expected = ((Get-Content (\$tmp + '.sha256')) -split '\s+')[0].Trim()
\$actual = (Get-FileHash -Algorithm SHA256 \$tmp).Hash
if (\$expected -ne \$actual) { throw ('SHA256 MISMATCH expected ' + \$expected + ' got ' + \$actual) }
# 2. back up the current exe BEFORE overwriting it
Copy-Item -Force \$exe \$bak
# 3. self-heal: any failure during stop/swap/start restores the .bak and restarts before rethrow
try {
    Stop-Service dantesync
$(dantesync_windows_wait_service_exit_ps)
    Copy-Item -Force \$tmp \$exe
    Start-Service dantesync
} catch {
    Copy-Item -Force \$bak \$exe -ErrorAction SilentlyContinue
    Start-Service dantesync -ErrorAction SilentlyContinue
    throw
}
# 4. purge the dead DanteSyncUpdate relic task -- genuinely idempotent: routed through cmd /c
# with full redirection, because a bare schtasks on an ALREADY-ABSENT task writes to stderr and
# \$ErrorActionPreference=Stop turns that into a terminating NativeCommandError AFTER the swap
# (live 2026-08-16 v1.8.43 canary: swap completed, ps1 exited non-zero, orchestrator misreported
# a failed upgrade).
cmd /c "schtasks /Delete /TN \"$DANTESYNC_DEAD_TASK\" /F >nul 2>&1"
& \$exe --version
EOF
}

# dantesync_windows_rollback_ps -> the CONTENT of a .ps1 that restores the pre-upgrade exe from
# its .bak and starts the service. Orchestrator-invoked only on the VERIFY-failure path.
dantesync_windows_rollback_ps() {
  cat <<EOF
\$ErrorActionPreference = 'Stop'
\$exe = '$DANTESYNC_WIN_EXE'
\$bak = '$DANTESYNC_WIN_BAK'
if (-not (Test-Path \$bak)) { throw 'no dantesync.exe.bak to roll back to' }
Stop-Service dantesync
$(dantesync_windows_wait_service_exit_ps)
Copy-Item -Force \$bak \$exe
Start-Service dantesync
& \$exe --version
EOF
}

# dantesync_windows_run_ps_file_cmd REMOTE_PATH -> the ssh command that runs an already-uploaded
# .ps1 by path (the repo's rig-state-inspection.md §2 pattern — a FILE, never a nested -Command).
dantesync_windows_run_ps_file_cmd() {
  echo "powershell -NoProfile -ExecutionPolicy Bypass -File \"$1\""
}

# dantesync_resolve_canary LINUX_SET WIN_SET OVERRIDE -> the canary node set (space-separated).
# Default: one representative per OS class present (first Linux node + first Windows node) — a
# green Linux canary must never authorize touching a Windows box (#452 per-class insight; the
# class here is the OS). OVERRIDE (space-separated) is honored verbatim iff every member is in the
# union of the two sets; errors (non-zero, names the offender) otherwise.
dantesync_resolve_canary() {
  local linux_set="$1" win_set="$2" override="$3" all cam a found out=""
  all="$linux_set $win_set"
  if [ -n "$override" ]; then
    for cam in $override; do
      found=0
      for a in $all; do
        [ "$a" = "$cam" ] && { found=1; break; }
      done
      if [ "$found" -ne 1 ]; then
        echo "dantesync_resolve_canary: canary override '$cam' is not in the fleet ($all)" >&2
        return 1
      fi
    done
    echo "$override"
    return 0
  fi
  for cam in $linux_set; do out="$cam"; break; done
  for cam in $win_set; do out="${out:+$out }$cam"; break; done
  if [ -z "$out" ]; then
    echo "dantesync_resolve_canary: empty fleet" >&2
    return 1
  fi
  echo "$out"
}

# dantesync_remaining_after_canary SET CANARY_SET -> SET minus every node in CANARY_SET, SET order
# preserved.
dantesync_remaining_after_canary() {
  local set="$1" canary_set="$2" node cnode skip out=""
  for node in $set; do
    skip=0
    for cnode in $canary_set; do
      [ "$node" = "$cnode" ] && { skip=1; break; }
    done
    [ "$skip" -eq 0 ] && out="${out:+$out }$node"
  done
  echo "$out"
}

# dantesync_needs_sudo USER -> 0 (needs privilege escalation) unless USER is already root. #1077:
# the Linux path silently assumed a root@ ssh session; imag-nb (newlevel@) and dev1 (--local, runs
# as newlevel) are NON-root and must escalate to run the root-only remount/install/systemctl steps.
dantesync_needs_sudo() {
  [ "$1" != "root" ]
}

# dantesync_linux_run_script_cmd USER REMOTE_PATH PW -> the command that RUNS an already-uploaded
# upgrade/rollback script FILE on a Linux node, escalating iff USER is not root (#1077). A root@
# node (the cam boxes) runs it directly. A non-root node prefers passwordless sudo (`sudo -n`, e.g.
# dev1) and otherwise feeds the password to `sudo -S` via stdin (imag-nb) -- so the password reaches
# sudo only and is NEVER written into the on-disk script FILE the orchestrator scp'd. Running by
# FILE (bash "$path"), never a nested inline -c, mirrors the Windows -File pattern (no quoting hazard).
dantesync_linux_run_script_cmd() {
  local user="$1" path="$2" pw="${3:-}"
  if dantesync_needs_sudo "$user"; then
    cat <<EOF
if sudo -n true 2>/dev/null; then
  sudo bash "$path"
else
  printf '%s\n' '$pw' | sudo -S -p '' bash "$path"
fi
EOF
  else
    echo "bash \"$path\""
  fi
}

# dantesync_is_ntp_master NODE MASTER_NAME -> 0 iff NODE is the (non-empty) configured NTP master.
# #1077: the master node is graded/settled differently at verify time (see verify_node) -- an empty
# master name means NO node is the master (never a false match).
dantesync_is_ntp_master() {
  [ -n "$2" ] && [ "$1" = "$2" ]
}

# --- source-guard: when sourced (the unit tests), stop here ----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) -------------------------------------------------

usage() { sed -n '2,58p' "$0"; }
log()   { printf '%s\n' "$*"; }
err()   { printf 'ERROR: %s\n' "$*" >&2; }

SSH_PASS="${SSH_PASS:-newlevel}"
TARGET="${DANTESYNC_VERSION_PIN}"
LINUX_SPEC=""
WIN_SPEC=""
LOCAL_SPEC=""
CANARY_OVERRIDE=""
DRY_RUN=0
FORCE=0
GATE_WAIT_TRIES="${GATE_WAIT_TRIES:-10}"
GATE_WAIT_SECS="${GATE_WAIT_SECS:-6}"
# #1077: the NTP master's OWN dantesync restart makes it re-discipline against upstream and the
# whole fleet chases (a sawtooth that converges MINUTES later). Verifying the master right after its
# restart with the strict ~60s slave window measured that restart-induced storm and rolled back a
# HEALTHY swap (rc=20 twice, live v1.8.43 roll). So the master node gets (a) --ntp-master <self> (the
# gate's master-aware median+freshness grade, #1014, not the strict single-node offset bound) and
# (b) a LONGER bounded settle window (retry to steady state, clear PASS/FAIL, no silent sleep-and-
# hope). NTP_MASTER defaults to the SAME name dantesync-gate.sh uses (single source of truth).
NTP_MASTER="${NTP_MASTER:-${DANTESYNC_NTP_MASTER_NAME:-strih}}"
MASTER_GATE_WAIT_TRIES="${MASTER_GATE_WAIT_TRIES:-20}"
MASTER_GATE_WAIT_SECS="${MASTER_GATE_WAIT_SECS:-15}"

while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h)  usage; exit 0 ;;
    --target)   TARGET="${2:?--target needs a version}"; shift 2 ;;
    --pin)      TARGET="${2:?--pin needs a version}"; shift 2 ;;
    --linux)    LINUX_SPEC="${2:?--linux needs \"name=user@ip ...\"}"; shift 2 ;;
    --win)      WIN_SPEC="${2:?--win needs \"name=user@ip ...\"}"; shift 2 ;;
    --local)    LOCAL_SPEC="${2:?--local needs a node name}"; shift 2 ;;
    --canary)   CANARY_OVERRIDE="${2:?--canary needs a node set}"; shift 2 ;;
    --dry-run)  DRY_RUN=1; shift ;;
    --force)    FORCE=1; shift ;;
    *)          err "unknown argument: $1"; usage >&2; exit 2 ;;
  esac
done

command -v sshpass >/dev/null 2>&1 || { err "sshpass is required (apt-get install sshpass)"; exit 1; }
case "$TARGET" in
  [0-9]*.[0-9]*.[0-9]*) : ;;
  *) err "--target '$TARGET' is not an X.Y.Z version"; exit 1 ;;
esac

ssh_node() {  # ADDR CMD  (ADDR is user@ip)
  sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=12 "$1" "$2"
}
scp_node() {  # LOCAL_PATH  ADDR:REMOTE
  sshpass -p "$SSH_PASS" scp -O -o StrictHostKeyChecking=no -o ConnectTimeout=12 "$1" "$2"
}

# #1077: the orchestrator downloads + sha256-verifies the pinned binary ONCE on dev1 (this box HAS
# curl), caches it for the whole run, and scp's the verified copy to each Linux node -- so a
# curl-less box (cam3) never needs to fetch, and the metered venue LAN pays ONE download, not eight.
STAGED_LOCAL_DIR=""
trap 'rm -rf "${STAGED_LOCAL_DIR:-}" 2>/dev/null || true' EXIT

# ensure_linux_binary_staged VERSION -> download+verify the pinned Linux binary+sha into
# STAGED_LOCAL_DIR on dev1 (memoized: only the first successful call fetches). Returns non-zero
# (nothing staged) if the download or the dev1-side sha256 verification fails -- a bad download
# never reaches a node. #1077 review: stage into a LOCAL dir first and publish it to the memo
# (STAGED_LOCAL_DIR) ONLY after the sha256 passes, so a failed dev1 fetch never poisons the memo
# (which would falsely short-circuit the next node's call); the temp dir is cleaned on every
# failure branch.
ensure_linux_binary_staged() {
  local version="$1" url expected actual dir
  [ -n "$STAGED_LOCAL_DIR" ] && return 0
  url="$(dantesync_release_url_linux "$version")"
  dir="$(mktemp -d)"
  if ! curl --fail -fsSL -o "$dir/dantesync" "$url"; then
    err "could not download the pinned dantesync binary on dev1 ($url)"
    rm -rf "$dir"
    return 1
  fi
  if ! curl --fail -fsSL -o "$dir/dantesync.sha256" "$url.sha256"; then
    err "could not download the pinned dantesync sha256 on dev1 ($url.sha256)"
    rm -rf "$dir"
    return 1
  fi
  expected="$(awk '{print $1}' "$dir/dantesync.sha256")"
  actual="$(sha256sum "$dir/dantesync" | awk '{print $1}')"
  if [ "$expected" != "$actual" ]; then
    err "staged binary SHA256 mismatch on dev1: expected $expected got $actual"
    rm -rf "$dir"
    return 1
  fi
  STAGED_LOCAL_DIR="$dir"   # publish to the memo only after a fully-verified download
  return 0
}

# stage_linux_binary_to KIND ADDR -> place the verified binary+sha where the generated script's
# staged-binary branch will find it: for a remote node scp it to DANTESYNC_LINUX_STAGED on the box;
# for --local copy it to the same path on dev1. Assumes ensure_linux_binary_staged already ran.
stage_linux_binary_to() {  # KIND ADDR
  local kind="$1" addr="$2"
  case "$kind" in
    local)
      cp -a "$STAGED_LOCAL_DIR/dantesync" "$DANTESYNC_LINUX_STAGED" \
        && cp -a "$STAGED_LOCAL_DIR/dantesync.sha256" "$DANTESYNC_LINUX_STAGED.sha256" ;;
    linux)
      scp_node "$STAGED_LOCAL_DIR/dantesync" "$addr:$DANTESYNC_LINUX_STAGED" \
        && scp_node "$STAGED_LOCAL_DIR/dantesync.sha256" "$addr:$DANTESYNC_LINUX_STAGED.sha256" ;;
  esac
}

# read_node_version NAME KIND ADDR -> the parsed dantesync version ("" if unread). KIND is
# local|linux|win. Reuses the #862 gate's PURE parser — never a second parser.
read_node_version() {
  local kind="$2" addr="$3" raw=""
  case "$kind" in
    local) raw="$(dantesync --version 2>/dev/null || true)" ;;
    linux) raw="$(ssh_node "$addr" 'dantesync --version' 2>/dev/null || true)" ;;
    win)   raw="$(ssh_node "$addr" "\"$DANTESYNC_WIN_EXE\" --version" 2>/dev/null || true)" ;;
  esac
  dantesync_version_from_version_output "$raw"
}

# node_ip ADDR -> the ip portion of a user@ip (or ADDR unchanged if no user@).
node_ip() { printf '%s\n' "${1##*@}"; }

# verify_node NAME KIND ADDR -> 0 iff (version reads back == TARGET) AND (dantesync-gate.sh green
# for this node: PTP-locked + fresh in-bound offset). The version read-back is the hard gate on
# every node kind; the lock/offset gate runs for linux (ssh) + win (--win-http) nodes.
#
# #1077 -- master-vs-slave grading + settle window:
#  * A SLAVE node passes `--ntp-master ""` -- a single-node isolated verification has no NTP master
#    among its one configured node, so opting out avoids the gate's "master not among configured
#    nodes" refusal AND grades the freshly-relocked node on plain offset+PTP-lock. Its settle window
#    is the normal GATE_WAIT_TRIES x GATE_WAIT_SECS (the servo re-locks in seconds after a slave's
#    own restart).
#  * The MASTER node passes `--ntp-master <self>` so the gate applies its master-aware median+
#    freshness grade (#1014) instead of the strict single-node offset bound, AND gets a LONGER
#    bounded settle window (MASTER_GATE_WAIT_TRIES x MASTER_GATE_WAIT_SECS). The master's OWN restart
#    makes it re-discipline against upstream and the whole fleet chases (a sawtooth that converges
#    minutes later); the old strict ~60s slave window measured that storm and rolled back a HEALTHY
#    swap (rc=20 twice, live v1.8.43). Because the master (strih) is the first Windows canary, this
#    wait-to-steady-state also gates the REST loop -- slaves are only verified after the fleet has
#    already converged. The loop stays BOUNDED (seq/tries) with a clear PASS/FAIL, never a silent
#    sleep-and-hope.
verify_node() {
  local name="$1" kind="$2" addr="$3" ip got gate_rc=0 i master_arg tries secs
  ip="$(node_ip "$addr")"
  got="$(read_node_version "$name" "$kind" "$addr")"
  if [ "$got" != "$TARGET" ]; then
    err "[$name] version read back as '${got:-<unread>}', expected $TARGET"
    return 1
  fi
  if dantesync_is_ntp_master "$name" "$NTP_MASTER"; then
    master_arg="$name"; tries="$MASTER_GATE_WAIT_TRIES"; secs="$MASTER_GATE_WAIT_SECS"
    log "[$name] is the NTP master — verifying master-aware, settling to steady state (up to $((tries * secs))s)"
  else
    master_arg=""; tries="$GATE_WAIT_TRIES"; secs="$GATE_WAIT_SECS"
  fi
  for i in $(seq 1 "$tries"); do
    gate_rc=0
    case "$kind" in
      linux) "$HERE/dantesync-gate.sh" --linux "$name=$ip" --ntp-master "$master_arg" >/dev/null 2>&1 || gate_rc=$? ;;
      win)   "$HERE/dantesync-gate.sh" --win-http "$name=$ip" --ntp-master "$master_arg" >/dev/null 2>&1 || gate_rc=$? ;;
      local) gate_rc=0 ;;  # dev1 lock is confirmed by the fleet-wide gate precondition on the next E2E
    esac
    [ "$gate_rc" -eq 0 ] && break
    [ "$i" -lt "$tries" ] && sleep "$secs"
  done
  if [ "$gate_rc" -ne 0 ]; then
    err "[$name] dantesync-gate.sh did not confirm PTP-lock + in-bound offset (rc=$gate_rc)"
    return 1
  fi
  log "[$name] verified: $TARGET, PTP-locked, offset in-bound"
  return 0
}

# run_upgrade NAME KIND ADDR -> run the OS-appropriate upgrade, capturing combined remote output
# into REMOTE_OUT. Returns the remote command's rc. The remote scripts are self-healing (see the
# header): a failure PAST the swap restores the previous binary on the box before returning
# non-zero, so a non-zero rc here means the service is on the PREVIOUS (working) version already.
REMOTE_OUT=""
run_upgrade() {
  local name="$1" kind="$2" addr="$3" rc=0 local_ps local_sh user runcmd
  case "$kind" in
    local)
      # #1077: dev1 runs as newlevel (non-root). Stage the verified binary, then run the upgrade
      # script by FILE with sudo escalation (dev1 has passwordless sudo -> sudo -n).
      if ! ensure_linux_binary_staged "$TARGET"; then REMOTE_OUT="dev1-side staging failed"; return 1; fi
      if ! stage_linux_binary_to local ""; then REMOTE_OUT="could not place the staged binary on dev1"; return 1; fi
      local_sh="$(mktemp)"
      dantesync_linux_upgrade_cmd "$TARGET" >"$local_sh"
      runcmd="$(dantesync_linux_run_script_cmd "$(id -un)" "$local_sh" "$SSH_PASS")"
      REMOTE_OUT="$(bash -c "$runcmd" 2>&1)" || rc=$?
      rm -f "$local_sh" ;;
    linux)
      # #1077: stage the verified binary + upload the upgrade script as a FILE, then run it escalated
      # (root@ boxes run it directly; newlevel@ imag-nb via sudo -S). The staged binary makes a
      # curl-less box (cam3) upgrade with no on-box fetch.
      if ! ensure_linux_binary_staged "$TARGET"; then REMOTE_OUT="dev1-side staging failed"; return 1; fi
      user="${addr%%@*}"
      local_sh="$(mktemp)"
      dantesync_linux_upgrade_cmd "$TARGET" >"$local_sh"
      if ! REMOTE_OUT="$( { stage_linux_binary_to linux "$addr" \
            && scp_node "$local_sh" "$addr:$DANTESYNC_LINUX_SH_REMOTE"; } 2>&1)"; then
        rm -f "$local_sh"
        return 1
      fi
      rm -f "$local_sh"
      runcmd="$(dantesync_linux_run_script_cmd "$user" "$DANTESYNC_LINUX_SH_REMOTE" "$SSH_PASS")"
      REMOTE_OUT="$(ssh_node "$addr" "$runcmd" 2>&1)" || rc=$? ;;
    win)
      local_ps="$(mktemp)"
      dantesync_windows_upgrade_ps "$TARGET" >"$local_ps"
      if ! REMOTE_OUT="$(scp_node "$local_ps" "$addr:$DANTESYNC_WIN_PS_REMOTE" 2>&1)"; then
        rm -f "$local_ps"
        return 1
      fi
      rm -f "$local_ps"
      REMOTE_OUT="$(ssh_node "$addr" "$(dantesync_windows_run_ps_file_cmd "$DANTESYNC_WIN_PS_REMOTE")" 2>&1)" || rc=$? ;;
  esac
  return "$rc"
}

# rollback_node NAME KIND ADDR -> restore the pre-upgrade binary/exe and restart. ONLY called on
# the VERIFY-failure path (the swap provably completed). A rollback that itself fails is logged
# loudly but never masks the original failure.
rollback_node() {
  local name="$1" kind="$2" addr="$3" rb_rc=0 out="" local_ps local_sh user runcmd
  case "$kind" in
    local)
      # #1077: the rollback also does root-only ops (remount, install, systemctl) -> run by FILE with
      # sudo escalation, same as the upgrade path. No staging (it restores from the on-box .bak).
      local_sh="$(mktemp)"
      dantesync_linux_rollback_cmd >"$local_sh"
      runcmd="$(dantesync_linux_run_script_cmd "$(id -un)" "$local_sh" "$SSH_PASS")"
      out="$(bash -c "$runcmd" 2>&1)" || rb_rc=$?
      rm -f "$local_sh" ;;
    linux)
      user="${addr%%@*}"
      local_sh="$(mktemp)"
      dantesync_linux_rollback_cmd >"$local_sh"
      if scp_node "$local_sh" "$addr:$DANTESYNC_LINUX_SH_REMOTE" >/dev/null 2>&1; then
        runcmd="$(dantesync_linux_run_script_cmd "$user" "$DANTESYNC_LINUX_SH_REMOTE" "$SSH_PASS")"
        out="$(ssh_node "$addr" "$runcmd" 2>&1)" || rb_rc=$?
      else
        rb_rc=1
      fi
      rm -f "$local_sh" ;;
    win)
      local_ps="$(mktemp)"
      dantesync_windows_rollback_ps >"$local_ps"
      if scp_node "$local_ps" "$addr:$DANTESYNC_WIN_PS_REMOTE" >/dev/null 2>&1; then
        out="$(ssh_node "$addr" "$(dantesync_windows_run_ps_file_cmd "$DANTESYNC_WIN_PS_REMOTE")" 2>&1)" || rb_rc=$?
      else
        rb_rc=1
      fi
      rm -f "$local_ps" ;;
  esac
  if [ "$rb_rc" -ne 0 ]; then
    err "[$name] ROLLBACK FAILED (rc=$rb_rc) — this node may be in a mixed state, inspect it by hand"
    [ -n "$out" ] && err "[$name] rollback output: $out"
  else
    log "[$name] rolled back to the previous binary"
  fi
}

# upgrade_node NAME KIND ADDR -> 0 on a verified upgrade, non-zero on failure. SAME target = a
# documented no-op; OLDER = refused unless --force; UNKNOWN = refused. On an upgrade-command
# failure the remote script has already self-healed to the previous version, so we only REPORT
# (never blind-rollback). On a verify failure the swap completed, so we roll back.
upgrade_node() {
  local name="$1" kind="$2" addr="$3" cur status
  cur="$(read_node_version "$name" "$kind" "$addr")"
  status="$(dantesync_upgrade_status "$cur" "$TARGET")"
  case "$status" in
    SAME)
      log "[$name] already on $TARGET — nothing to do"
      return 0 ;;
    UNKNOWN)
      err "[$name] could not read the current version — refusing to upgrade a node in an unknown state"
      return 1 ;;
    OLDER)
      if [ "$FORCE" -ne 1 ]; then
        err "[$name] target $TARGET is OLDER than installed $cur — refusing downgrade (pass --force)"
        return 1
      fi
      log "[$name] --force: downgrading $cur -> $TARGET" ;;
    NEWER)
      log "[$name] upgrading $cur -> $TARGET" ;;
  esac

  if ! run_upgrade "$name" "$kind" "$addr"; then
    err "[$name] upgrade command failed — the box self-healed to its previous version (not rolled forward)"
    [ -n "$REMOTE_OUT" ] && err "[$name] upgrade output: $REMOTE_OUT"
    return 1
  fi

  if verify_node "$name" "$kind" "$addr"; then
    return 0
  fi
  err "[$name] verification failed after upgrade — rolling back the completed swap"
  rollback_node "$name" "$kind" "$addr"
  return 1
}

# --- build the node table ---------------------------------------------------------------------
# NODES: name|kind|addr ; a node is skipped (EXCLUDED) if acked offline in rig-fleet.txt.
declare -a NODES=()
add_node() {  # NAME KIND ADDR
  if cambox_offline_ack_is_acked "$1"; then
    log "  $1 EXCLUDED (acked offline: $(cambox_offline_ack_reason "$1"))"
    return 0
  fi
  NODES+=("$1|$2|$3")
}

log "== dantesync-fleet-upgrade (#876): target v${TARGET} =="
for entry in $LINUX_SPEC; do add_node "${entry%%=*}" linux "${entry#*=}"; done
for entry in $WIN_SPEC;   do add_node "${entry%%=*}" win   "${entry#*=}"; done
for name in $LOCAL_SPEC;  do add_node "$name" local "$name"; done

if [ "${#NODES[@]}" -eq 0 ]; then
  err "no nodes to act on — pass --linux/--win/--local (see --help)"
  exit 1
fi

# --- decide who needs an upgrade -------------------------------------------------------------
declare -a NEED_LINUX=() NEED_WIN=()
declare -a NEED_ALL=()
echo
log "-- current fleet state (target v${TARGET}) --"
for spec in "${NODES[@]}"; do
  IFS='|' read -r name kind addr <<<"$spec"
  cur="$(read_node_version "$name" "$kind" "$addr")"
  status="$(dantesync_upgrade_status "$cur" "$TARGET")"
  printf '  %-12s %-8s %-10s -> %s\n' "$name" "$kind" "${cur:-<unread>}" "$status"
  case "$status" in
    SAME) : ;;
    *)
      NEED_ALL+=("$name")
      case "$kind" in
        win) NEED_WIN+=("$name") ;;
        *)   NEED_LINUX+=("$name") ;;
      esac ;;
  esac
done
echo

if [ "${#NEED_ALL[@]}" -eq 0 ]; then
  log "Every node is already on v${TARGET} — nothing to do."
  exit 0
fi

# --- canary-first ordering (one representative per OS class present) --------------------------
CANARY_SET="$(dantesync_resolve_canary "${NEED_LINUX[*]:-}" "${NEED_WIN[*]:-}" "$CANARY_OVERRIDE")" || exit 1
REST="$(dantesync_remaining_after_canary "${NEED_ALL[*]}" "$CANARY_SET")"
log "Canary set: $CANARY_SET   Remaining after canary: ${REST:-<none>}"
echo

if [ "$DRY_RUN" -eq 1 ]; then
  log "DRY-RUN: would upgrade the node(s) above to v${TARGET}, canary-first ($CANARY_SET), then the rest (${REST:-<none>}). No change made."
  exit 0
fi

# addr_of / kind_of NAME -> the node's addr / kind from the table.
addr_of() { local n="$1" s; for s in "${NODES[@]}"; do IFS='|' read -r nm kd ad <<<"$s"; [ "$nm" = "$n" ] && { printf '%s\n' "$ad"; return; }; done; }
kind_of() { local n="$1" s; for s in "${NODES[@]}"; do IFS='|' read -r nm kd ad <<<"$s"; [ "$nm" = "$n" ] && { printf '%s\n' "$kd"; return; }; done; }

declare -a FAILED=()

# 1. CANARY — any failure aborts the whole roll (rest NOT touched).
for node in $CANARY_SET; do
  if ! upgrade_node "$node" "$(kind_of "$node")" "$(addr_of "$node")"; then
    err "CANARY $node failed — ABORTING the fleet roll. The rest of the fleet was NOT touched."
    exit 10
  fi
done
log "All canaries verified on v${TARGET}."
echo

# 2. REST — a failure is recovered + recorded, but the loop continues.
for node in $REST; do
  if ! upgrade_node "$node" "$(kind_of "$node")" "$(addr_of "$node")"; then
    FAILED+=("$node")
  fi
done

echo
if [ "${#FAILED[@]}" -gt 0 ]; then
  err "FLEET DANTESYNC UPGRADE INCOMPLETE: canaries passed but ${#FAILED[*]} node(s) failed (recovered): ${FAILED[*]}"
  exit 20
fi
log "== dantesync-fleet-upgrade complete: every requested node on v${TARGET}, PTP-locked, offset in-bound =="
