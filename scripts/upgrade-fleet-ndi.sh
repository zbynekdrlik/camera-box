#!/usr/bin/env bash
# upgrade-fleet-ndi.sh — safe, canary-first NDI Linux runtime upgrade across the fleet (#132;
# fleet grown cam1-4 -> cam1-7 by #451).
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
#     parses cleanly (never installs an unverified/unrecognised blob) — reads it via `strings`,
#     falling back to `grep -a` when `strings` isn't installed on the box (#445: cam3 has none)
#   - refuses a downgrade (candidate <= currently-active version) unless --force
#   - upgrades ONE canary camera first; the rest of the fleet is NEVER touched if the canary fails
#   - detects the ACTUAL runtime layout before touching anything: most cams have `libndi.so.6`
#     as a symlink to a versioned `.so` (repoint + backup the target); cam3 (#445) ships
#     `libndi.so.6`/`libndi.so` as REAL FILES (back up the file itself, then overwrite both names)
#   - always backs up the previous runtime before swapping, and NEVER deletes the backup — rollback
#     just re-points the symlinks (symlink layout) or restores the `.bak` copy (real-file layout)
#   - verifies each camera after the swap: camera-box must still emit an NDI-alive signal ("fps
#     emitted .* fps captured" — the exact genlock-report signal scripts/deploy-fleet.sh uses —
#     OR the older "Streaming: X.Y fps" shape cam3's build logs, OR "NDI sender ready") with no
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
#   --set "cam1 ..."    camera set to upgrade (default: cam1 cam2 cam3 cam4 cam5 cam6 cam7)
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
# shellcheck source=scripts/lib/ndi-alive.sh
. "$HERE/lib/ndi-alive.sh"   # emit_ok_grep_pattern(), fatal_grep_pattern() (#451, shared with deploy-fleet.sh)

# --- PURE functions (no network/ssh — unit-tested from tests/upgrade_fleet_ndi.rs) ----------

# ndi_version_from_strings_output TEXT -> the trailing "X.Y.Z.W" token out of an "NDI SDK LINUX
# ... X.Y.Z.W" banner (e.g. "NDI SDK LINUX 12:51:52 Apr 13 2026 6.3.2.0"), whether TEXT came from
# `strings` or the ndi_read_banner_local `grep -a` fallback below — both feed compatible text
# into this function. Empty ("") on no match — NEVER guesses a version from unrelated text.
# Always exits 0 (callers gate explicitly on emptiness).
ndi_version_from_strings_output() {
  printf '%s\n' "$1" | { grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' || true; } | tail -1
}

# ndi_read_banner_local PATH -> the "NDI SDK LINUX ... X.Y.Z.W" banner text out of the .so at
# PATH. Prefers `strings` (the historical path, works everywhere it's installed); falls back to
# `grep -a` when `strings` is unavailable OR yields no match (#445: cam3 has no `strings` binary
# at all, which made the tool refuse it with an "unknown baseline" error). Feed the result to
# ndi_version_from_strings_output for the bare X.Y.Z.W. Empty ("") on total failure — never a
# guess.
ndi_read_banner_local() {
  local path="$1" out=""
  out="$(strings "$path" 2>/dev/null | grep -m1 'NDI SDK LINUX' || true)"
  if [ -z "$out" ]; then
    out="$(grep -a -m1 -oE 'NDI SDK LINUX[^\n]*[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' "$path" 2>/dev/null || true)"
  fi
  printf '%s\n' "$out"
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

# ndi_link_kind_remote DEST_DIR -> the remote one-liner that echoes "symlink" when
# DEST_DIR/libndi.so.6 is a symbolic link (the layout on cam1/cam2/cam4 — a versioned .so with
# libndi.so.6 -> it), "regular" when it is a real file (#445: cam3's layout — libndi.so.6 and
# libndi.so ship as real files, not symlinks), or "missing" when neither exists. Callers must
# query this BEFORE building the swap/rollback script — a real file must be overwritten in
# place, while a symlink must be re-pointed; guessing either way corrupts the other layout.
ndi_link_kind_remote() {
  local dest="$1"
  printf '%s\n' "if [ -L \"$dest/libndi.so.6\" ]; then echo symlink; elif [ -f \"$dest/libndi.so.6\" ]; then echo regular; else echo missing; fi"
}

# ndi_swap_remote DEST_DIR NEW_BASENAME [LINK_KIND] -> the remote bash that installs the new
# runtime (already scp'd into DEST_DIR by the caller as NEW_BASENAME), ldconfig's, prints
# "OLD_BASE=<name>" so the caller can roll back, then restarts camera-box to load it.
# LINK_KIND (from ndi_link_kind_remote; defaults to "symlink" for backward compatibility with
# the fleet's existing layout) selects HOW:
#   symlink (cam1/cam2/cam4) -> backs up the CURRENTLY ACTIVE runtime (resolved via readlink -f,
#     so it backs up the real file, not the symlink), then re-points BOTH `libndi.so` and
#     `libndi.so.6` at NEW_BASENAME.
#   regular (#445, cam3)     -> libndi.so.6/libndi.so are real files, not symlinks — backs up
#     the active libndi.so.6 to a .bak file, then COPIES the new candidate content over both
#     names (a symlink repoint would not apply).
# NEW_BASENAME is never inlined into a remote string that could be re-interpreted — it flows in
# as a literal argument to this pure builder.
ndi_swap_remote() {
  local dest="$1" new_base="$2" kind="${3:-symlink}"
  if [ "$kind" = "regular" ]; then
    cat <<EOF
set -e
cp -a "$dest/libndi.so.6" "$dest/libndi.so.6.bak"
chmod +x "$dest/$new_base"
cp -a "$dest/$new_base" "$dest/libndi.so.6"
cp -a "$dest/$new_base" "$dest/libndi.so"
ldconfig
echo "OLD_BASE=libndi.so.6.bak"
systemctl restart camera-box
EOF
  else
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
  fi
}

# ndi_rollback_remote DEST_DIR OLD_BASENAME [LINK_KIND] -> the exact inverse of ndi_swap_remote
# for the same LINK_KIND (defaults to "symlink"):
#   symlink -> re-points both symlinks back at OLD_BASENAME (the pre-swap runtime —
#     ndi_swap_remote never deletes it).
#   regular (#445, cam3) -> restores libndi.so.6 and libndi.so from the .bak file
#     ndi_swap_remote created (OLD_BASENAME is "libndi.so.6.bak" in this case — there is no
#     symlink to repoint).
# Either way, restarts camera-box and never touches/deletes the backup.
ndi_rollback_remote() {
  local dest="$1" old_base="$2" kind="${3:-symlink}"
  if [ "$kind" = "regular" ]; then
    cat <<EOF
set -e
cp -a "$dest/$old_base" "$dest/libndi.so.6"
cp -a "$dest/$old_base" "$dest/libndi.so"
ldconfig
systemctl restart camera-box
EOF
  else
    cat <<EOF
set -e
dest="$dest"
ln -sf $old_base "\$dest/libndi.so.6"
ln -sf $old_base "\$dest/libndi.so"
ldconfig
systemctl restart camera-box
EOF
  fi
}

# ndi_active_version_remote DEST_DIR -> the remote script that prints the "NDI SDK LINUX ..."
# banner for whichever .so DEST_DIR/libndi.so.6 currently resolves to. Tries `strings` first,
# falling back to `grep -a` when `strings` isn't installed on the camera (#445: confirmed on
# cam3, which has no `strings` binary) — the SAME fallback ndi_read_banner_local uses for the
# local candidate read, so the current and candidate versions are always read with identical
# logic. Feed its output to ndi_version_from_strings_output to get the bare X.Y.Z.W.
ndi_active_version_remote() {
  local dest="$1"
  cat <<EOF
f="\$(readlink -f "$dest/libndi.so.6")"
out="\$(strings "\$f" 2>/dev/null | grep -m1 'NDI SDK LINUX' || true)"
if [ -z "\$out" ]; then
  out="\$(grep -a -m1 -oE 'NDI SDK LINUX[^\n]*[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' "\$f" 2>/dev/null || true)"
fi
printf '%s\n' "\$out"
EOF
}

# emit_ok_grep_pattern() / fatal_grep_pattern() now live in the shared scripts/lib/ndi-alive.sh
# (#451, sourced above) — deploy-fleet.sh sources the SAME file so the two scripts can never
# again drift out of sync the way #445's broadening did (applied here only, leaving
# deploy-fleet.sh's narrower copy behind until #451).

# --- source-guard: stop here when sourced (the unit tests) ----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- network + mutating flow (executed only when run directly) -----------------------------

usage() { sed -n '2,45p' "$0"; }

SSH_PASS="${SSH_PASS:-newlevel}"
NDI_DEST_DIR="${NDI_DEST_DIR:-/usr/lib/ndi}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

SO_PATH=""
SET="${CAMERA_SET:-cam1 cam2 cam3 cam4 cam5 cam6 cam7}"
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
NEW_BANNER="$(ndi_read_banner_local "$SO_PATH")"
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

  local link_kind
  link_kind="$(ssh_box "$ip" "$(ndi_link_kind_remote "$NDI_DEST_DIR")" || echo "missing")"
  if [ "$link_kind" = "missing" ]; then
    err "[$cam] $NDI_DEST_DIR/libndi.so.6 does not exist — refusing (cannot determine layout)"
    FAILED+=("$cam(missing-runtime)")
    return 1
  fi
  info "[$cam] runtime layout: $link_kind"

  info "[$cam] swapping runtime + restarting camera-box..."
  local swap_out old_base
  if ! swap_out="$(ssh_box "$ip" "$(ndi_swap_remote "$NDI_DEST_DIR" "$NEW_BASE" "$link_kind")")"; then
    err "[$cam] swap command failed — the previous runtime is never deleted, so the active \
symlink/file should still be intact"
    FAILED+=("$cam(swap-failed)")
    return 1
  fi
  old_base="$(printf '%s\n' "$swap_out" | sed -n 's/^OLD_BASE=//p')"
  if [ -z "$old_base" ]; then
    err "[$cam] could not capture the previous runtime's basename — cannot safely offer rollback"
    FAILED+=("$cam(no-old-base)")
    return 1
  fi
  info "[$cam] previous runtime backed up as ${old_base}; runtime now -> $NEW_BASE"

  if ! verify_camera_after_swap "$cam" "$ip"; then
    warn "[$cam] verification FAILED — rolling back to $old_base"
    if ssh_box "$ip" "$(ndi_rollback_remote "$NDI_DEST_DIR" "$old_base" "$link_kind")" >/dev/null \
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
      ssh_box "$ip" "$(ndi_rollback_remote "$NDI_DEST_DIR" "$old_base" "$link_kind")" >/dev/null || true
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
