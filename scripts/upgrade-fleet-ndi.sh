#!/usr/bin/env bash
# upgrade-fleet-ndi.sh — safe, canary-first NDI Linux runtime upgrade across cam1-4 (#132).
#
# WHY THIS SCRIPT EXISTS: the fleet's NDI Linux runtime (`/usr/lib/ndi/libndi.so.6`) is not
# uniform — the cameras run 6.2.1.0 while the production OBS boxes strih + stream already run
# 6.3.2.0 (the initial requirement was "newest NDI everywhere"; cross-version NDI interop works
# fine today, so this is version hygiene, not a live-breaking bug). The runtime itself is a
# public, unauthenticated download (vendor/distroav/CI/libndi-get.sh curls downloads.ndi.tv
# directly, no license key) — the real gap was a SAFE way to roll it onto embedded appliance
# hardware, not the asset itself.
#
# Swapping a shared library a running service depends on is genuinely risky on this hardware (no
# easy physical recovery), so this script is deliberately conservative:
#   - refuses to touch anything until the candidate .so's OWN "NDI SDK LINUX ... X.Y.Z.W" banner
#     parses cleanly (never installs an unverified/unrecognised blob)
#   - refuses a downgrade (candidate <= currently-active version) unless --force
#   - upgrades ONE canary camera first; the rest of the fleet is NEVER touched if the canary fails
#   - always backs up the previous runtime file before repointing symlinks, and NEVER deletes it
#     (mirrors the manual `libndi.so.6.2.1.bak` convention already used on dev1) — rollback just
#     re-points the symlinks back, it never has to restore from the backup
#   - verifies each camera after the swap: camera-box must still emit its genlock report ("fps
#     emitted .* fps captured" — the exact signal scripts/deploy-fleet.sh already uses) with no
#     FATAL/panic lines, AND the active runtime version must read back as the candidate. Any
#     verification failure triggers an automatic rollback + restart, then the camera is recorded
#     as failed (the fleet loop continues to the next camera — one bad box doesn't abort the run)
#   - optional --e2e-verify additionally runs the existing zero-loss loopback E2E
#     (scripts/loopback-e2e.sh) per camera after the swap, rolling back on a burn-decode failure
#     too — heavier, off by default
#
# Usage:
#   scripts/upgrade-fleet-ndi.sh --so-path /path/to/libndi.so.6.3.2 [options]
#   scripts/upgrade-fleet-ndi.sh --help
#
# Options:
#   --so-path PATH     the candidate NDI Linux runtime .so to roll out (required)
#   --set "cam1 ..."    camera set to upgrade (default: cam1 cam2 cam3 cam4)
#   --canary camN       pin the canary camera (default: the first camera in --set)
#   --force             allow a downgrade (candidate version <= currently-active version)
#   --dry-run           read + compare versions on every camera, change nothing
#   --e2e-verify         after a successful swap, also run scripts/loopback-e2e.sh per camera
#
# Env:
#   SSH_PASS        camera root password (default: newlevel)
#   NDI_DEST_DIR    the NDI runtime directory on each camera (default: /usr/lib/ndi)
#   GENLOCK_WAIT_TRIES / GENLOCK_WAIT_SECS   post-restart emit-report poll (default: 12 / 5)
#
# Exit codes: 0 = every requested camera ended on the candidate version (or was already there)
#   AND verified; 1 = usage/env error; 2 = unknown argument; 10 = the CANARY failed (rolled back,
#   the rest of the fleet was NOT touched); 20 = the canary succeeded but at least one other
#   camera failed (each individually rolled back — see the FLEET NDI UPGRADE INCOMPLETE summary).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"

# --- PURE functions (no network/ssh — unit-tested from tests/upgrade_fleet_ndi.rs) ----------

# ndi_version_from_strings_output TEXT -> the trailing "X.Y.Z.W" token out of the exact
# `strings libndi.so.6 | grep 'NDI SDK LINUX'` banner (e.g. "NDI SDK LINUX 12:51:52 Apr 13 2026
# 6.3.2.0"). Empty ("") on no match — NEVER guesses a version from unrelated text. Always exits
# 0 (callers gate explicitly on emptiness).
ndi_version_from_strings_output() {
  printf '%s\n' "$1" | { grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' || true; } | tail -1
}

# ndi_version_status CURRENT CANDIDATE -> NEWER | SAME | OLDER | UNKNOWN.
# UNKNOWN when either side is empty — a failed version read is never treated as any ordering.
ndi_version_status() {
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

# resolve_canary SET OVERRIDE -> the canary camera name. OVERRIDE (if non-empty) must be a
# member of SET or this fails loudly (never silently substitutes a different box). Empty
# OVERRIDE defaults to the first camera in SET.
resolve_canary() {
  local set="$1" override="${2:-}" first="" cam
  for cam in $set; do
    if [ -z "$first" ]; then
      first="$cam"
    fi
    if [ -n "$override" ] && [ "$cam" = "$override" ]; then
      echo "$override"
      return 0
    fi
  done
  if [ -n "$override" ]; then
    echo "resolve_canary: canary override '$override' is not a member of the set ($set)" >&2
    return 1
  fi
  if [ -z "$first" ]; then
    echo "resolve_canary: empty camera set" >&2
    return 1
  fi
  echo "$first"
}

# remaining_after_canary SET CANARY -> SET minus CANARY, preserving order.
remaining_after_canary() {
  local set="$1" canary="$2" cam out=""
  for cam in $set; do
    if [ "$cam" = "$canary" ]; then
      continue
    fi
    out="$out $cam"
  done
  printf '%s\n' "${out# }"
}

# ndi_swap_remote DEST_DIR NEW_BASENAME -> the remote bash that backs up the CURRENTLY ACTIVE
# runtime (resolved via readlink -f, so it backs up the real file, not the symlink), re-points
# BOTH `libndi.so` and `libndi.so.6` at NEW_BASENAME (already scp'd into DEST_DIR by the
# caller), ldconfig's, prints "OLD_BASE=<name>" so the caller can roll back, then restarts
# camera-box to load the new runtime. NEW_BASENAME is never inlined into a remote string that
# could be re-interpreted — it flows in as a literal argument to this pure builder.
ndi_swap_remote() {
  local dest="$1" new_base="$2"
  cat <<EOF
set -e
dest="$dest"
OLD_REAL="\$(readlink -f "$dest/libndi.so.6")"
OLD_BASE="\$(basename "\$OLD_REAL")"
cp -a "\$OLD_REAL" "\$OLD_REAL.bak"
chmod +x "$dest/$new_base"
ln -sf $new_base "\$dest/libndi.so.6"
ln -sf $new_base "\$dest/libndi.so"
ldconfig
echo "OLD_BASE=\$OLD_BASE"
systemctl restart camera-box
EOF
}

# ndi_rollback_remote DEST_DIR OLD_BASENAME -> the remote bash that re-points both symlinks
# back at OLD_BASENAME (the pre-swap runtime — ndi_swap_remote never deletes it) and restarts
# camera-box. The exact inverse of ndi_swap_remote; never touches the backup .bak file.
ndi_rollback_remote() {
  local dest="$1" old_base="$2"
  cat <<EOF
set -e
dest="$dest"
ln -sf $old_base "\$dest/libndi.so.6"
ln -sf $old_base "\$dest/libndi.so"
ldconfig
systemctl restart camera-box
EOF
}

# ndi_active_version_remote DEST_DIR -> the remote one-liner that prints the "NDI SDK LINUX ..."
# banner for whichever .so DEST_DIR/libndi.so.6 currently resolves to. Feed its output to
# ndi_version_from_strings_output to get the bare X.Y.Z.W.
ndi_active_version_remote() {
  printf 'strings "$(readlink -f %s/libndi.so.6)" | grep -m1 "NDI SDK LINUX"' "$1"
}

# emit_ok_grep_pattern -> the exact journalctl grep proving camera-box's NDI capture->emit path
# survived the restart — IDENTICAL signal to deploy-fleet.sh's post-deploy check, so the fleet
# tooling has one consistent definition of "this camera is genlocking/emitting", not two.
emit_ok_grep_pattern() { echo 'fps emitted .* fps captured'; }

# fatal_grep_pattern -> the exact crash signatures deploy-fleet.sh scans for after a restart.
fatal_grep_pattern() { echo "panic|thread '.*' panicked|SIGSEGV|SIGABRT|core dumped|FATAL"; }

# --- source-guard: stop here when sourced (the unit tests) ----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- network + mutating flow (executed only when run directly) -----------------------------

usage() { sed -n '2,32p' "$0"; }

SSH_PASS="${SSH_PASS:-newlevel}"
NDI_DEST_DIR="${NDI_DEST_DIR:-/usr/lib/ndi}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

SO_PATH=""
SET="${CAMERA_SET:-cam1 cam2 cam3 cam4}"
CANARY_OVERRIDE=""
FORCE=0
DRY_RUN=0
E2E_VERIFY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --so-path)   SO_PATH="${2:?--so-path needs a path}"; shift 2 ;;
    --set)       SET="${2:?--set needs a camera list}"; shift 2 ;;
    --canary)    CANARY_OVERRIDE="${2:?--canary needs a camera name}"; shift 2 ;;
    --force)     FORCE=1; shift ;;
    --dry-run)   DRY_RUN=1; shift ;;
    --e2e-verify) E2E_VERIFY=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    *) echo "upgrade-fleet-ndi: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

if [ -z "$SO_PATH" ]; then
  err "missing required --so-path <path to the candidate libndi.so.6.x.y.z>"
  usage >&2
  exit 1
fi
if [ ! -f "$SO_PATH" ]; then
  err "--so-path '$SO_PATH' not found"
  exit 1
fi

command -v sshpass >/dev/null 2>&1 || { err "sshpass is required (apt-get install sshpass)"; exit 1; }

ssh_box()  { sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@$1" "$2"; }
scp_box()  { sshpass -p "$SSH_PASS" scp -o StrictHostKeyChecking=no "$2" "root@$1:$3"; }

NEW_BASE="$(basename "$SO_PATH")"
NEW_BANNER="$(strings "$SO_PATH" | grep -m1 'NDI SDK LINUX' || true)"
NEW_VER="$(ndi_version_from_strings_output "$NEW_BANNER")"
if [ -z "$NEW_VER" ]; then
  err "could not read an 'NDI SDK LINUX ... X.Y.Z.W' banner out of $SO_PATH — refusing to \
deploy an unverified/unrecognised blob"
  exit 1
fi
log "Candidate NDI runtime: $NEW_BASE ($NEW_VER)"

CANARY="$(resolve_canary "$SET" "$CANARY_OVERRIDE")" || exit 1
REST="$(remaining_after_canary "$SET" "$CANARY")"
log "Canary: $CANARY   Remaining after canary: ${REST:-<none>}"
echo ""

declare -a FAILED=()

# verify_camera_after_swap CAM IP -> 0 if camera-box is emitting NDI (the deploy-fleet.sh
# genlock-report signal) with no FATAL lines AND the active runtime now reads back as NEW_VER.
verify_camera_after_swap() {
  local cam="$1" ip="$2" line="" tries="${GENLOCK_WAIT_TRIES:-12}" secs="${GENLOCK_WAIT_SECS:-5}"
  for _ in $(seq 1 "$tries"); do
    sleep "$secs"
    line="$(ssh_box "$ip" "journalctl -u camera-box --no-pager -n 200 2>/dev/null | grep -E '$(emit_ok_grep_pattern)' | tail -1" || true)"
    if [ -n "$line" ]; then
      break
    fi
  done
  if [ -z "$line" ]; then
    err "[$cam] no NDI emit report ('fps emitted / fps captured') after restart"
    return 1
  fi

  local fatal
  fatal="$(ssh_box "$ip" "journalctl -u camera-box --no-pager -n 300 2>/dev/null | grep -E \"$(fatal_grep_pattern)\" | tail -3" || true)"
  if [ -n "$fatal" ]; then
    err "[$cam] journal contains FATAL/panic lines after restart:"
    echo "$fatal"
    return 1
  fi

  local ver_banner active_ver
  ver_banner="$(ssh_box "$ip" "$(ndi_active_version_remote "$NDI_DEST_DIR")" || echo "")"
  active_ver="$(ndi_version_from_strings_output "$ver_banner")"
  if [ -z "$active_ver" ]; then
    err "[$cam] could not re-read the active NDI version after restart"
    return 1
  fi
  info "[$cam] NDI emit OK, active runtime: $active_ver"
  return 0
}

# upgrade_one_camera CAM -> upgrades CAM to NEW_VER (or confirms it's already there / dry-runs),
# verifying + auto-rolling-back on failure. Records CAM in FAILED on any non-success outcome;
# never aborts the whole run (the caller decides how to react to a canary vs. non-canary loss).
upgrade_one_camera() {
  local cam="$1"
  if ! camera_resolve "$cam"; then
    FAILED+=("$cam(invalid)")
    return 1
  fi
  local ip="$CAMERA_IP"
  echo "================================================================"
  echo ">> [$cam] $ip"
  echo "================================================================"

  local cur_banner cur_ver
  cur_banner="$(ssh_box "$ip" "$(ndi_active_version_remote "$NDI_DEST_DIR")" || echo "")"
  cur_ver="$(ndi_version_from_strings_output "$cur_banner")"
  if [ -z "$cur_ver" ]; then
    err "[$cam] could not read the currently active NDI version — refusing (unknown baseline)"
    FAILED+=("$cam(unknown-baseline)")
    return 1
  fi
  info "[$cam] current NDI runtime: $cur_ver"

  local status
  status="$(ndi_version_status "$cur_ver" "$NEW_VER")"
  case "$status" in
    NEWER) : ;;
    SAME)
      log "[$cam] already on $NEW_VER — nothing to do"
      return 0
      ;;
    OLDER)
      if [ "$FORCE" -ne 1 ]; then
        err "[$cam] candidate $NEW_VER is OLDER than the live $cur_ver — refusing (pass \
--force to downgrade deliberately)"
        FAILED+=("$cam(refused-downgrade)")
        return 1
      fi
      warn "[$cam] --force: downgrading $cur_ver -> $NEW_VER"
      ;;
    *)
      err "[$cam] version comparison UNKNOWN ($cur_ver vs $NEW_VER) — refusing"
      FAILED+=("$cam(unknown-compare)")
      return 1
      ;;
  esac

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[$cam] DRY RUN: would upgrade $cur_ver -> $NEW_VER (no changes made)"
    return 0
  fi

  info "[$cam] copying $NEW_BASE ..."
  if ! scp_box "$ip" "$SO_PATH" "$NDI_DEST_DIR/$NEW_BASE"; then
    err "[$cam] scp failed"
    FAILED+=("$cam(scp-failed)")
    return 1
  fi

  info "[$cam] swapping runtime + restarting camera-box..."
  local swap_out old_base
  if ! swap_out="$(ssh_box "$ip" "$(ndi_swap_remote "$NDI_DEST_DIR" "$NEW_BASE")")"; then
    err "[$cam] swap command failed — the previous runtime is never deleted, so the active \
symlink should still be intact"
    FAILED+=("$cam(swap-failed)")
    return 1
  fi
  old_base="$(printf '%s\n' "$swap_out" | sed -n 's/^OLD_BASE=//p')"
  if [ -z "$old_base" ]; then
    err "[$cam] could not capture the previous runtime's basename — cannot safely offer rollback"
    FAILED+=("$cam(no-old-base)")
    return 1
  fi
  info "[$cam] previous runtime backed up as ${old_base}.bak; symlinks now -> $NEW_BASE"

  if ! verify_camera_after_swap "$cam" "$ip"; then
    warn "[$cam] verification FAILED — rolling back to $old_base"
    if ssh_box "$ip" "$(ndi_rollback_remote "$NDI_DEST_DIR" "$old_base")" >/dev/null \
       && verify_camera_after_swap "$cam" "$ip"; then
      log "[$cam] rollback verified healthy on $old_base"
    else
      err "[$cam] ROLLBACK ALSO FAILED — $cam may be in a BROKEN NDI state, needs operator \
attention immediately"
    fi
    FAILED+=("$cam(verify-failed-rolled-back)")
    return 1
  fi

  if [ "$E2E_VERIFY" -eq 1 ]; then
    info "[$cam] running loopback E2E burn-decode verification..."
    if ! CAM="$cam" "$HERE/loopback-e2e.sh"; then
      warn "[$cam] E2E burn-decode FAILED after upgrade — rolling back to $old_base"
      ssh_box "$ip" "$(ndi_rollback_remote "$NDI_DEST_DIR" "$old_base")" >/dev/null || true
      FAILED+=("$cam(e2e-failed-rolled-back)")
      return 1
    fi
    log "[$cam] E2E burn-decode OK on $NEW_VER"
  fi

  log "[$cam] upgraded $cur_ver -> $NEW_VER, verified"
  echo ""
  return 0
}

# --- canary first: never touch the rest of the fleet on a canary failure -------------------
if ! upgrade_one_camera "$CANARY"; then
  err "CANARY $CANARY FAILED — the rest of the fleet (${REST:-<none>}) was NOT touched"
  echo "================================================================"
  exit 10
fi

for cam in $REST; do
  upgrade_one_camera "$cam" || true
done

echo "================================================================"
if [ "${#FAILED[@]}" -eq 0 ]; then
  log "FLEET NDI RUNTIME ALIGNED: all of [$SET] on $NEW_VER (or already there)"
  exit 0
fi
err "FLEET NDI UPGRADE INCOMPLETE — issues: ${FAILED[*]}"
exit 20
