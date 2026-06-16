#!/usr/bin/env bash
# Align the whole camera-box fleet (cam1-4) onto ONE pinned CI-built binary, in one command (#73).
#
# The fleet drifts: cameras get deployed at different times and end up on different versions
# (e.g. #73 found cam1/cam4=dev.29, cam3=dev.22, cam2=dev.19 — three builds, none current, cam2
# old enough that it predated the genlock-decimation report and so was NOT genlocking). This
# script makes re-alignment a single command: download the SAME CI artifact once, push it to
# every camera with the exact stop -> remount,rw -> scp -> start -> remount,ro cycle from the
# project CLAUDE.md "Build & Deploy" section, then VERIFY each box reports the new version AND
# is emitting the genlock report ("N fps emitted / M fps captured").
#
# Per deploy-from-clean-tree.md the deploy source is ALWAYS a CI artifact from a committed,
# pushed ref — never a locally built binary. This script downloads from a GitHub Actions run
# (default: the latest successful ci.yml run on `main`) or accepts a pre-downloaded binary path.
#
# Per approval-scope.md the deploy + the camera-box service restart it performs are the
# standing-approved WORK — this script does NOT ask permission and does NOT gate on "is it
# off-air / is there a live event". The operator who runs it guards live timing.
#
# Usage:
#   scripts/deploy-fleet.sh                       # deploy latest successful main ci.yml artifact to cam1-4
#   scripts/deploy-fleet.sh --run <run-id>        # pin a specific GitHub Actions run id
#   scripts/deploy-fleet.sh --binary ./dist/camera-box   # deploy an already-downloaded CI binary
#   CAMERA_SET="cam2" scripts/deploy-fleet.sh     # restrict to a subset (default: cam1 cam2 cam3 cam4)
#
# Env:
#   SSH_PASS   camera root password (default: newlevel)
#   REPO       GitHub repo (default: zbynekdrlik/camera-box)
#   BRANCH     branch whose latest successful ci.yml run is used when --run/--binary omitted (default: main)
#   ARTIFACT   CI artifact name (default: camera-box-linux-amd64)
#
# Exit status: 0 only if EVERY camera in the set ends on the new version AND emits the genlock
# report. Any version mismatch, missing genlock line, panic, or unreachable box => nonzero.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve(), CAMERA_SET, GENLOCK_FPS

SSH_PASS="${SSH_PASS:-newlevel}"
REPO="${REPO:-zbynekdrlik/camera-box}"
BRANCH="${BRANCH:-main}"
ARTIFACT="${ARTIFACT:-camera-box-linux-amd64}"
SET="${CAMERA_SET:-cam1 cam2 cam3 cam4}"

RUN_ID=""
BINARY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --run)    RUN_ID="${2:?--run needs a run id}"; shift 2 ;;
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "deploy-fleet: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

command -v sshpass >/dev/null 2>&1 || { err "sshpass is required (apt-get install sshpass)"; exit 1; }

ssh_box()  { sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@$1" "$2"; }
scp_box()  { sshpass -p "$SSH_PASS" scp -o StrictHostKeyChecking=no "$2" "root@$1:$3"; }

# --- 1. Obtain the pinned CI binary -------------------------------------------------------
if [ -n "$BINARY" ]; then
  [ -f "$BINARY" ] || { err "--binary '$BINARY' not found"; exit 1; }
  info "Using pre-downloaded binary: $BINARY"
else
  command -v gh >/dev/null 2>&1 || { err "gh CLI is required to download the artifact"; exit 1; }
  if [ -z "$RUN_ID" ]; then
    info "Finding latest successful ci.yml run on '$BRANCH'..."
    RUN_ID="$(gh run list --repo "$REPO" --branch "$BRANCH" --workflow ci.yml \
      --status success --limit 1 --json databaseId -q '.[0].databaseId')"
    [ -n "$RUN_ID" ] || { err "no successful ci.yml run found on '$BRANCH'"; exit 1; }
  fi
  RUN_SHA="$(gh run view "$RUN_ID" --repo "$REPO" --json headSha -q .headSha)"
  info "Downloading artifact '$ARTIFACT' from run $RUN_ID (sha ${RUN_SHA:0:9})..."
  DIST="$(mktemp -d)"
  gh run download "$RUN_ID" --repo "$REPO" -n "$ARTIFACT" --dir "$DIST"
  BINARY="$DIST/camera-box"
  [ -f "$BINARY" ] || { err "artifact did not contain camera-box"; exit 1; }
fi
chmod +x "$BINARY"

NEW_VER="$("$BINARY" --version 2>/dev/null | awk '{print $NF}')"
[ -n "$NEW_VER" ] || { err "could not read --version from the binary"; exit 1; }
log "Deploying camera-box $NEW_VER to: $SET"
echo ""

# --- 2 + 3. Deploy + verify per box -------------------------------------------------------
declare -a FAILED=()
for cam in $SET; do
  if ! camera_resolve "$cam"; then
    FAILED+=("$cam(invalid)"); continue
  fi
  ip="$CAMERA_IP"
  echo "================================================================"
  echo ">> [$cam] $ip"
  echo "================================================================"

  before="$(ssh_box "$ip" "camera-box --version 2>/dev/null | awk '{print \$NF}'" || echo "unreachable")"
  if [ "$before" = "unreachable" ]; then
    err "[$cam] unreachable — skipping"; FAILED+=("$cam(unreachable)"); continue
  fi
  info "[$cam] current version: $before"

  if [ "$before" = "$NEW_VER" ]; then
    log "[$cam] already on $NEW_VER — re-pushing anyway to guarantee byte-identical binary"
  fi

  info "[$cam] stop service + remount rw + copy + start + remount ro"
  ssh_box "$ip" "mount -o remount,rw / && systemctl stop camera-box"
  scp_box "$ip" "$BINARY" "/usr/local/bin/camera-box"
  ssh_box "$ip" "systemctl start camera-box && (mount -o remount,ro / 2>/dev/null; true)"

  # Verify version.
  after="$(ssh_box "$ip" "camera-box --version 2>/dev/null | awk '{print \$NF}'" || echo "unknown")"
  if [ "$after" != "$NEW_VER" ]; then
    err "[$cam] version mismatch after deploy: expected $NEW_VER, got '$after'"
    FAILED+=("$cam(version=$after)"); continue
  fi
  log "[$cam] version $before -> $after"

  # Give the service a moment to produce a streaming report, then verify genlock emit + no panic.
  # GENLOCK_WAIT_TRIES / GENLOCK_WAIT_SECS are overridable (the test harness sets them small).
  info "[$cam] waiting for genlock report..."
  genlock_line=""
  panic_line=""
  for _ in $(seq 1 "${GENLOCK_WAIT_TRIES:-12}"); do
    sleep "${GENLOCK_WAIT_SECS:-5}"
    genlock_line="$(ssh_box "$ip" "journalctl -u camera-box --no-pager -n 200 2>/dev/null | grep -E 'fps emitted .* fps captured' | tail -1" || true)"
    [ -n "$genlock_line" ] && break
  done
  panic_line="$(ssh_box "$ip" "journalctl -u camera-box --no-pager -n 200 2>/dev/null | grep -iE 'panic|error|fatal' | tail -3" || true)"

  if [ -z "$genlock_line" ]; then
    err "[$cam] NO genlock report ('fps emitted / fps captured') seen — not genlocking"
    FAILED+=("$cam(no-genlock)")
  else
    log "[$cam] genlocking: ${genlock_line##*camera_box: }"
  fi
  if [ -n "$panic_line" ]; then
    warn "[$cam] journal contains error/panic lines:"
    echo "$panic_line"
    FAILED+=("$cam(errors)")
  fi
  echo ""
done

# --- Summary ------------------------------------------------------------------------------
echo "================================================================"
if [ "${#FAILED[@]}" -eq 0 ]; then
  log "FLEET ALIGNED: all of [$SET] on $NEW_VER and genlocking"
  exit 0
fi
err "FLEET NOT FULLY ALIGNED — issues: ${FAILED[*]}"
exit 1
