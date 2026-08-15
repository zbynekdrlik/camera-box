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
#     offset) BEFORE the rest of the fleet is touched; if ANY canary fails, it is rolled back and
#     the whole roll ABORTS (rest untouched). A non-canary failure is rolled back and recorded,
#     but the loop continues (one bad box doesn't abort the others).
#   * SAFETY: the new binary is downloaded AND sha256-verified BEFORE the running service is
#     stopped, and the current binary is backed up BEFORE it is overwritten — a failed download or
#     a failed verification never leaves the clock master down or unrecoverable.
#
# REUSE, NEVER REINVENT: sources dantesync-version-gate.sh for the version PARSER
# (dantesync_version_from_version_output) + the PIN; uses dantesync-gate.sh (→ clock-offset-guard.sh
# pure parsers) as the per-canary verification gate; uses scripts/lib/cambox-offline-ack.sh +
# rig-fleet.txt for knowingly-offline exclusion — the SAME mechanism every other fleet gate uses.
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
# per-class default (every member must be in the fleet). --dry-run reads + reports, changes
# nothing. --force allows a downgrade (target OLDER than installed).
#
# Env: SSH_PASS (default newlevel), DANTESYNC_GATE_BOUND_US (offset bound, passed to the gate),
#      GATE_WAIT_TRIES/GATE_WAIT_SECS (post-restart settle poll for the verification gate).
#
# Exit codes: 0 = every requested node ended on the target (or was already there) AND verified;
#   1 = usage/env error; 2 = unknown argument; 10 = a CANARY failed (rolled back; rest NOT
#   touched); 20 = every canary passed but at least one other node failed (each rolled back).

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
DANTESYNC_LINUX_BIN="/usr/local/bin/dantesync"
DANTESYNC_LINUX_BAK="/usr/local/bin/dantesync.bak"
DANTESYNC_DEAD_TASK="DanteSyncUpdate"

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
# Order enforces the safety contract: download + sha256-verify FIRST (a bad download never stops
# the daemon), THEN back up the current binary, THEN stop/swap/restart, THEN read the version back.
dantesync_linux_upgrade_cmd() {
  local version="$1" url
  url="$(dantesync_release_url_linux "$version")"
  cat <<EOF
set -e
tmp="\$(mktemp -d)"
trap 'rm -rf "\$tmp"' EXIT
# 1. download + sha256-verify BEFORE touching the running clock master
curl --fail -fsSL -o "\$tmp/dantesync" "$url"
curl --fail -fsSL -o "\$tmp/dantesync.sha256" "$url.sha256"
expected="\$(awk '{print \$1}' "\$tmp/dantesync.sha256")"
actual="\$(sha256sum "\$tmp/dantesync" | awk '{print \$1}')"
if [ "\$expected" != "\$actual" ]; then
  echo "SHA256 MISMATCH: expected \$expected got \$actual" >&2
  exit 1
fi
chmod +x "\$tmp/dantesync"
# 2. back up the current binary BEFORE overwriting it (rollback target)
cp -a "$DANTESYNC_LINUX_BIN" "$DANTESYNC_LINUX_BAK"
# 3. swap + restart
systemctl stop dantesync
install -m 0755 "\$tmp/dantesync" $DANTESYNC_LINUX_BIN
systemctl restart dantesync
# 4. read the new version back
dantesync --version
EOF
}

# dantesync_linux_rollback_cmd -> restore the pre-upgrade binary from its .bak and restart.
dantesync_linux_rollback_cmd() {
  cat <<EOF
set -e
if [ ! -f "$DANTESYNC_LINUX_BAK" ]; then
  echo "no $DANTESYNC_LINUX_BAK to roll back to" >&2
  exit 1
fi
systemctl stop dantesync
cp -a "$DANTESYNC_LINUX_BAK" $DANTESYNC_LINUX_BIN
systemctl restart dantesync
dantesync --version
EOF
}

# dantesync_windows_purge_dead_task_cmd -> the idempotent forced delete of the dead DanteSyncUpdate
# relic scheduled task (the #18-rename orphan). Standalone-usable; also embedded in the upgrade.
dantesync_windows_purge_dead_task_cmd() {
  echo "schtasks /Delete /TN \"$DANTESYNC_DEAD_TASK\" /F"
}

# dantesync_windows_upgrade_cmd VERSION -> a headless PowerShell command (session-agnostic,
# ssh-legal — no GUI atom) to upgrade a Windows node to VERSION. Same safety order as Linux:
# download + Get-FileHash-verify FIRST, back up the exe BEFORE replacing it, stop/swap/start,
# purge the dead relic task, then read the version back. Body uses single quotes only so it nests
# cleanly inside the outer -Command "..." over ssh/cmd.exe.
dantesync_windows_upgrade_cmd() {
  local version="$1" url
  url="$(dantesync_release_url_windows "$version")"
  local body="\$ErrorActionPreference='Stop';"
  body="$body \$url='$url';"
  body="$body \$exe='$DANTESYNC_WIN_EXE';"
  body="$body \$bak='$DANTESYNC_WIN_BAK';"
  body="$body \$tmp=\$env:TEMP + '\\dantesync-new.exe';"
  body="$body Invoke-WebRequest -UseBasicParsing -Uri \$url -OutFile \$tmp;"
  body="$body Invoke-WebRequest -UseBasicParsing -Uri (\$url + '.sha256') -OutFile (\$tmp + '.sha256');"
  body="$body \$expected=((Get-Content (\$tmp + '.sha256')) -split '\\s+')[0].Trim();"
  body="$body \$actual=(Get-FileHash -Algorithm SHA256 \$tmp).Hash;"
  body="$body if (\$expected -ne \$actual) { Write-Error ('SHA256 MISMATCH expected ' + \$expected + ' got ' + \$actual); exit 1 };"
  body="$body Copy-Item -Force \$exe \$bak;"
  body="$body Stop-Service dantesync;"
  body="$body Copy-Item -Force \$tmp \$exe;"
  body="$body Start-Service dantesync;"
  body="$body schtasks /Delete /TN '$DANTESYNC_DEAD_TASK' /F 2>\$null;"
  body="$body & \$exe --version"
  echo "powershell -NoProfile -ExecutionPolicy Bypass -Command \"$body\""
}

# dantesync_windows_rollback_cmd -> restore the pre-upgrade exe from its .bak and start the service.
dantesync_windows_rollback_cmd() {
  local body="\$ErrorActionPreference='Stop';"
  body="$body \$exe='$DANTESYNC_WIN_EXE';"
  body="$body \$bak='$DANTESYNC_WIN_BAK';"
  body="$body if (-not (Test-Path \$bak)) { Write-Error 'no dantesync.exe.bak to roll back to'; exit 1 };"
  body="$body Stop-Service dantesync;"
  body="$body Copy-Item -Force \$bak \$exe;"
  body="$body Start-Service dantesync;"
  body="$body & \$exe --version"
  echo "powershell -NoProfile -ExecutionPolicy Bypass -Command \"$body\""
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

# --- source-guard: when sourced (the unit tests), stop here ----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) -------------------------------------------------

usage() { sed -n '2,55p' "$0"; }
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
# every node kind; the lock/offset gate runs for the node kinds it supports (linux cams over ssh,
# windows over --win-http). A settle poll gives the servo a few seconds to re-lock after restart.
verify_node() {
  local name="$1" kind="$2" addr="$3" ip got tries="$GATE_WAIT_TRIES" gate_rc=0 i
  ip="$(node_ip "$addr")"
  got="$(read_node_version "$name" "$kind" "$addr")"
  if [ "$got" != "$TARGET" ]; then
    err "[$name] version read back as '${got:-<unread>}', expected $TARGET"
    return 1
  fi
  # lock + offset via the existing gate, with a bounded settle poll
  for i in $(seq 1 "$tries"); do
    gate_rc=0
    case "$kind" in
      linux) "$HERE/dantesync-gate.sh" --linux "$name=$ip" >/dev/null 2>&1 || gate_rc=$? ;;
      win)   "$HERE/dantesync-gate.sh" --win-http "$name=$ip" >/dev/null 2>&1 || gate_rc=$? ;;
      local) gate_rc=0 ;;  # dev1 lock is confirmed by the fleet-wide gate precondition on the next E2E
    esac
    [ "$gate_rc" -eq 0 ] && break
    [ "$i" -lt "$tries" ] && sleep "$GATE_WAIT_SECS"
  done
  if [ "$gate_rc" -ne 0 ]; then
    err "[$name] dantesync-gate.sh did not confirm PTP-lock + in-bound offset (rc=$gate_rc)"
    return 1
  fi
  log "[$name] verified: $TARGET, PTP-locked, offset in-bound"
  return 0
}

# upgrade_node NAME KIND ADDR -> 0 on a verified upgrade, non-zero on failure (already rolled
# back). SAME target = documented no-op; OLDER = refused unless --force; UNKNOWN = refused.
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

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[$name] DRY-RUN: would upgrade to $TARGET (no change made)"
    return 0
  fi

  local up_rc=0
  case "$kind" in
    local) bash -c "$(dantesync_linux_upgrade_cmd "$TARGET")" >/dev/null 2>&1 || up_rc=$? ;;
    linux) ssh_node "$addr" "$(dantesync_linux_upgrade_cmd "$TARGET")" >/dev/null 2>&1 || up_rc=$? ;;
    win)   ssh_node "$addr" "$(dantesync_windows_upgrade_cmd "$TARGET")" >/dev/null 2>&1 || up_rc=$? ;;
  esac
  if [ "$up_rc" -ne 0 ]; then
    err "[$name] upgrade command failed (rc=$up_rc) — rolling back"
    rollback_node "$name" "$kind" "$addr"
    return 1
  fi

  if verify_node "$name" "$kind" "$addr"; then
    return 0
  fi
  err "[$name] verification failed after upgrade — rolling back"
  rollback_node "$name" "$kind" "$addr"
  return 1
}

# rollback_node NAME KIND ADDR -> restore the pre-upgrade binary/exe and restart. Best-effort:
# a rollback that itself fails is logged loudly but never masks the original upgrade failure.
rollback_node() {
  local name="$1" kind="$2" addr="$3" rb_rc=0
  case "$kind" in
    local) bash -c "$(dantesync_linux_rollback_cmd)" >/dev/null 2>&1 || rb_rc=$? ;;
    linux) ssh_node "$addr" "$(dantesync_linux_rollback_cmd)" >/dev/null 2>&1 || rb_rc=$? ;;
    win)   ssh_node "$addr" "$(dantesync_windows_rollback_cmd)" >/dev/null 2>&1 || rb_rc=$? ;;
  esac
  if [ "$rb_rc" -ne 0 ]; then
    err "[$name] ROLLBACK FAILED (rc=$rb_rc) — this node may be in a mixed state, inspect it by hand"
  else
    log "[$name] rolled back to the previous binary"
  fi
}

# --- build the node table ---------------------------------------------------------------------
# NODES: name|kind|addr ; a node is skipped (EXCLUDED) if acked offline in rig-fleet.txt.
declare -a NODES=()
declare -a LINUX_NAMES=() WIN_NAMES=()
add_node() {  # NAME KIND ADDR
  if cambox_offline_ack_is_acked "$1"; then
    log "  $1 EXCLUDED (acked offline: $(cambox_offline_ack_reason "$1"))"
    return 0
  fi
  NODES+=("$1|$2|$3")
  case "$2" in
    win) WIN_NAMES+=("$1") ;;
    *)   LINUX_NAMES+=("$1") ;;   # linux + local both take the Linux upgrade path
  esac
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

# addr_of NAME -> the node's addr (kind is re-derived from the table)
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

# 2. REST — a failure is rolled back + recorded, but the loop continues.
for node in $REST; do
  if ! upgrade_node "$node" "$(kind_of "$node")" "$(addr_of "$node")"; then
    FAILED+=("$node")
  fi
done

echo
if [ "${#FAILED[@]}" -gt 0 ]; then
  err "FLEET DANTESYNC UPGRADE INCOMPLETE: canaries passed but ${#FAILED[*]} node(s) failed (rolled back): ${FAILED[*]}"
  exit 20
fi
log "== dantesync-fleet-upgrade complete: every requested node on v${TARGET}, PTP-locked, offset in-bound =="
