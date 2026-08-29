#!/usr/bin/env bash
# Align the whole camera-box fleet (cam1-6, #451) onto ONE pinned CI-built binary, in one command (#73).
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
#   scripts/deploy-fleet.sh                       # deploy latest successful main ci.yml artifact to cam1-6
#   scripts/deploy-fleet.sh --run <run-id>        # pin a specific GitHub Actions run id
#   scripts/deploy-fleet.sh --binary ./dist/camera-box   # deploy an already-downloaded CI binary
#   scripts/deploy-fleet.sh --frame-probe ./dist-probe/frame-probe   # ALSO deploy the cam2-painter
#                                                 # (frame-probe) binary to cam2 with #1138 #892
#                                                 # enable-state-preserving lifecycle (opt-in)
#   CAMERA_SET="cam2" scripts/deploy-fleet.sh     # restrict to a subset (default: $CAMERA_ACTIVE_SET, camera-set.sh; today cam1-4, #827)
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
# shellcheck source=scripts/lib/ndi-alive.sh
. "$HERE/lib/ndi-alive.sh"   # emit_ok_grep_pattern(), fatal_grep_pattern() (#451, shared with upgrade-fleet-ndi.sh)
# shellcheck source=scripts/lib/cli-log.sh
. "$HERE/lib/cli-log.sh"   # log()/info()/warn()/err() (#559, shared with upgrade-fleet-ndi.sh + verify-fleet.sh)
# shellcheck source=scripts/lib/capture-rate-guard.sh
. "$HERE/lib/capture-rate-guard.sh"   # invocation-id-scoped journalctl builder (#694, shared with upgrade-fleet-ndi.sh + verify-device.sh)
# shellcheck source=scripts/lib/frame-probe-deploy.sh
. "$HERE/lib/frame-probe-deploy.sh"   # frame_probe_restore_enable_decision() — the #1138 #892 enable-state-preserving painter deploy decision

SSH_PASS="${SSH_PASS:-newlevel}"
REPO="${REPO:-zbynekdrlik/camera-box}"
BRANCH="${BRANCH:-main}"
ARTIFACT="${ARTIFACT:-camera-box-linux-amd64}"
SET="${CAMERA_SET:-$CAMERA_ACTIVE_SET}"

RUN_ID=""
BINARY=""
# #1138: the cam2-painter (frame-probe) binary to also deploy to the painter box. Opt-in: a bare
# deploy-fleet.sh run (no --frame-probe) is unchanged; the post-merge ci.yml deploy job downloads
# the probe-tools-linux-amd64 artifact and passes its frame-probe here.
FRAME_PROBE_BIN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --run)    RUN_ID="${2:?--run needs a run id}"; shift 2 ;;
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    --frame-probe) FRAME_PROBE_BIN="${2:?--frame-probe needs a path}"; shift 2 ;;
    -h|--help) sed -n '2,36p' "$0"; exit 0 ;;
    *) echo "deploy-fleet: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

command -v sshpass >/dev/null 2>&1 || { err "sshpass is required (apt-get install sshpass)"; exit 1; }

ssh_box()  { sshpass -p "$SSH_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@$1" "$2"; }
scp_box()  { sshpass -p "$SSH_PASS" scp -o StrictHostKeyChecking=no "$2" "root@$1:$3"; }

# --- #1138: deploy the cam2-painter (frame-probe) binary to cam2, with the #892 lifecycle --------
# frame-probe is installed ONLY on the painter box (setup-device.sh STEP 3b, cam2_is_painter_box),
# so this is a cam2-only step, mirroring the camera-box loop's shape (stop → remount,rw → scp →
# byte-verify → restore → remount,ro). The KEY difference from camera-box: the restart is
# ENABLE-STATE-PRESERVING (frame_probe_restore_enable_decision, .claude/rules/cam2-painter-
# lifecycle.md #892) — re-arm cam2-painter.service (`enable --now`) ONLY if it was persistently
# enabled (devel/TEST mode); if it was disabled (EVENT mode — the operator deliberately dropped the
# QR so it can't return onto a live broadcast) swap the binary but LEAVE the unit dark (the next
# `rig-mode.sh test` re-arms it). Any genuine deploy failure is recorded in FAILED[] like a cam box.
deploy_frame_probe_to_painter() {
  local painter="cam2"   # the ONE fixed painter box (mirrors setup-device.sh's cam2_is_painter_box)
  case " $SET " in
    *" $painter "*) : ;;
    *) info "[frame-probe] $painter not in set [$SET] — skipping cam2-painter deploy"; return 0 ;;
  esac
  [ -f "$FRAME_PROBE_BIN" ] || { err "[frame-probe] binary '$FRAME_PROBE_BIN' not found"; FAILED+=("$painter-painter(no-binary)"); return 0; }
  chmod +x "$FRAME_PROBE_BIN" 2>/dev/null || true
  if ! camera_resolve "$painter"; then
    FAILED+=("$painter-painter(invalid)"); return 0
  fi
  local ip="$CAMERA_IP"
  echo "================================================================"
  echo ">> [$painter] cam2-painter (frame-probe) — $ip"
  echo "================================================================"

  # #892: read the unit's prior enabled-state and decide the restore action BEFORE touching it.
  local was_enabled restore_action
  was_enabled="$(ssh_box "$ip" "systemctl is-enabled cam2-painter.service 2>/dev/null" || true)"
  restore_action="$(frame_probe_restore_enable_decision "$was_enabled")"
  info "[$painter] cam2-painter.service is-enabled='${was_enabled:-<none>}' -> restore: $restore_action"

  # stop the painter (best-effort — it may be inactive / not-installed) + remount rw for the swap.
  if ! ssh_box "$ip" "mount -o remount,rw / && (systemctl stop cam2-painter.service 2>/dev/null || true)"; then
    err "[$painter] remount-rw / painter stop failed"; FAILED+=("$painter-painter(stop-failed)"); return 0
  fi
  if ! scp_box "$ip" "$FRAME_PROBE_BIN" "/usr/local/bin/frame-probe"; then
    err "[$painter] frame-probe scp failed"; FAILED+=("$painter-painter(scp-failed)")
    # best-effort restore of the unit + read-only root even on a failed swap.
    [ "$restore_action" = "enable-now" ] && ssh_box "$ip" "systemctl enable --now cam2-painter.service 2>/dev/null || true" || true
    ssh_box "$ip" "(mount -o remount,ro / 2>/dev/null; true)" || true
    return 0
  fi
  ssh_box "$ip" "chmod +x /usr/local/bin/frame-probe 2>/dev/null || true" || true

  # Byte-verify (deploy-from-clean-tree.md Layer 3 — a partial scp / stale same-name binary would
  # pass a mere presence check but fail this).
  local local_sha remote_sha
  local_sha="$(sha256sum "$FRAME_PROBE_BIN" | awk '{print $1}')"
  remote_sha="$(ssh_box "$ip" "sha256sum /usr/local/bin/frame-probe 2>/dev/null | awk '{print \$1}'" || echo "")"
  if [ "$local_sha" != "$remote_sha" ]; then
    err "[$painter] frame-probe byte-verify FAILED: local $local_sha != remote ${remote_sha:-<none>}"
    FAILED+=("$painter-painter(sha-mismatch)")
  else
    info "[$painter] frame-probe byte-verify OK (sha256 ${local_sha:0:12})"
  fi

  # #892 restore: re-arm ONLY a persistently-enabled unit; leave a disabled (event-mode) unit dark.
  if [ "$restore_action" = "enable-now" ]; then
    if ! ssh_box "$ip" "systemctl enable --now cam2-painter.service && (mount -o remount,ro / 2>/dev/null; true)"; then
      err "[$painter] cam2-painter.service enable --now failed"; FAILED+=("$painter-painter(restart-failed)")
      # #1138 (review): the && short-circuits the remount-ro when enable --now fails, leaving root
      # rw. Re-assert read-only root unconditionally before returning (best-effort).
      ssh_box "$ip" "(mount -o remount,ro / 2>/dev/null; true)" || true
      return 0
    fi
    local active
    active="$(ssh_box "$ip" "systemctl is-active cam2-painter.service 2>/dev/null" || echo inactive)"
    if [ "$active" = "active" ]; then
      log "[$painter] cam2-painter.service re-armed + active on the new frame-probe"
    else
      err "[$painter] cam2-painter.service not active after enable --now (is-active='$active')"; FAILED+=("$painter-painter(not-active)")
    fi
  else
    ssh_box "$ip" "(mount -o remount,ro / 2>/dev/null; true)" || true
    log "[$painter] frame-probe swapped; cam2-painter.service left in its prior state ('${was_enabled:-<none>}') — not re-armed (#892: an event-mode/dark painter must not return onto a live broadcast)"
  fi
  echo ""
}

# Clean up a downloaded-artifact temp dir on exit (no-op when --binary was used).
DIST=""
# shellcheck disable=SC2317  # invoked indirectly via the EXIT trap below
# NB: must not leak a non-zero status from the trap (it would override the script's exit code) —
# end with `:` so EXIT preserves the real exit status.
cleanup() { [ -n "$DIST" ] && rm -rf "$DIST"; :; }
trap cleanup EXIT

# --- #1138 frame-probe-ONLY mode ----------------------------------------------------------
# --frame-probe WITHOUT --binary/--run deploys ONLY the cam2 painter (the auto-align path in
# scripts/lib/frame-probe-parity-align.sh), NEVER a camera-box fleet deploy. The align must swap
# just /usr/local/bin/frame-probe on cam2 -- re-deploying camera-box to the whole fleet would be a
# scope violation (and would collide with the camera-box parity align that already handles it). A
# bare / --binary / --run invocation is UNCHANGED (camera-box fleet deploy + the optional
# frame-probe tail below).
if [ -n "$FRAME_PROBE_BIN" ] && [ -z "$BINARY" ] && [ -z "$RUN_ID" ]; then
  [ -f "$FRAME_PROBE_BIN" ] || { err "--frame-probe '$FRAME_PROBE_BIN' not found"; exit 1; }
  declare -a FAILED=()
  deploy_frame_probe_to_painter
  echo "================================================================"
  if [ "${#FAILED[@]}" -eq 0 ]; then
    log "FRAME-PROBE DEPLOYED: cam2 painter aligned to the requested build"
    exit 0
  fi
  err "FRAME-PROBE DEPLOY FAILED — issues: ${FAILED[*]}"
  exit 1
fi

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

  before="$(ssh_box "$ip" "/usr/local/bin/camera-box --version 2>/dev/null | awk '{print \$NF}'" || echo "unreachable")"
  if [ "$before" = "unreachable" ]; then
    err "[$cam] unreachable — skipping"; FAILED+=("$cam(unreachable)"); continue
  fi
  info "[$cam] current version: $before"

  if [ "$before" = "$NEW_VER" ]; then
    log "[$cam] already on $NEW_VER — re-pushing anyway to guarantee byte-identical binary"
  fi

  # Each deploy step is guarded: a failure on ONE box records it and moves on to the next box
  # (never aborts the whole fleet under set -e). The remount-ro is best-effort (2>/dev/null; true)
  # — some devices have no read-only rootfs.
  info "[$cam] stop service + remount rw + copy + start + remount ro"
  if ! ssh_box "$ip" "mount -o remount,rw / && systemctl stop camera-box"; then
    err "[$cam] remount-rw / stop failed"; FAILED+=("$cam(stop-failed)"); continue
  fi
  if ! scp_box "$ip" "$BINARY" "/usr/local/bin/camera-box"; then
    err "[$cam] scp failed"; FAILED+=("$cam(scp-failed)")
    ssh_box "$ip" "systemctl start camera-box && (mount -o remount,ro / 2>/dev/null; true)" || true
    continue
  fi
  if ! ssh_box "$ip" "systemctl start camera-box && (mount -o remount,ro / 2>/dev/null; true)"; then
    err "[$cam] start failed"; FAILED+=("$cam(start-failed)")
    # #1138 (review): the && short-circuits the remount-ro on a failed start, leaving root rw —
    # re-assert read-only root unconditionally before moving to the next box (best-effort).
    ssh_box "$ip" "(mount -o remount,ro / 2>/dev/null; true)" || true
    continue
  fi

  # Byte-verify: the deployed binary must hash-match the artifact we shipped (deploy-from-clean-tree.md
  # Layer 3 — a --version match alone does NOT prove byte-identity; a partial scp or a stale same-version
  # binary would pass a version check but fail this).
  local_sha="$(sha256sum "$BINARY" | awk '{print $1}')"
  remote_sha="$(ssh_box "$ip" "sha256sum /usr/local/bin/camera-box 2>/dev/null | awk '{print \$1}'" || echo "")"
  if [ "$local_sha" != "$remote_sha" ]; then
    err "[$cam] byte-verify FAILED: local $local_sha != remote ${remote_sha:-<none>}"
    FAILED+=("$cam(sha-mismatch)"); continue
  fi
  info "[$cam] byte-verify OK (sha256 ${local_sha:0:12})"

  # Verify version (absolute path — don't rely on the remote PATH resolving camera-box).
  after="$(ssh_box "$ip" "/usr/local/bin/camera-box --version 2>/dev/null | awk '{print \$NF}'" || echo "unknown")"
  if [ "$after" != "$NEW_VER" ]; then
    err "[$cam] version mismatch after deploy: expected $NEW_VER, got '$after'"
    FAILED+=("$cam(version=$after)"); continue
  fi
  log "[$cam] version $before -> $after"

  # Give the service a moment to produce a streaming report, then verify genlock emit + no FATAL.
  # GENLOCK_WAIT_TRIES / GENLOCK_WAIT_SECS are overridable (the test harness sets them small).
  # #694: same stale-journal-across-restart exposure #693 fixed for recording-e2e.sh's preflight
  # -- `journalctl -u camera-box` spans ACROSS the restart this deploy just performed, so a
  # WARN/FATAL from the box's PREVIOUS process instance could leak into the lookback window.
  # Resolve the CURRENT camera-box.service InvocationID each retry (the service was JUST
  # restarted, so early tries may still be racing systemd) and scope both reads to it via the
  # shared capture_rate_journalctl_cmd(); empty on failure falls back to the old unscoped read.
  info "[$cam] waiting for genlock report..."
  genlock_line=""
  cb_invocation_id=""
  for _ in $(seq 1 "${GENLOCK_WAIT_TRIES:-12}"); do
    sleep "${GENLOCK_WAIT_SECS:-5}"
    cb_invocation_id="$(ssh_box "$ip" "systemctl show -p InvocationID --value camera-box 2>/dev/null" || true)"
    genlock_line="$(ssh_box "$ip" "$(capture_rate_journalctl_cmd "$cb_invocation_id") | grep -E '$(emit_ok_grep_pattern)' | tail -1" || true)"
    [ -n "$genlock_line" ] && break
  done

  # FATAL scan: only genuinely unrecoverable signals (a panic / process crash), scoped to the
  # CURRENT boot of the just-restarted service. We deliberately do NOT trip on `error!`-level
  # lines — the app logs recoverable events at that level in normal operation (intercom restart,
  # NDI reconnect, capture retry), so greping for 'error' would false-fail a healthy, genlocking box.
  fatal_line="$(ssh_box "$ip" "$(capture_rate_journalctl_cmd "$cb_invocation_id" 300) | grep -E \"$(fatal_grep_pattern)\" | tail -3" || true)"

  if [ -z "$genlock_line" ]; then
    err "[$cam] NO genlock report ('fps emitted / fps captured') seen — not genlocking"
    FAILED+=("$cam(no-genlock)")
  else
    log "[$cam] genlocking: ${genlock_line##*camera_box: }"
  fi
  if [ -n "$fatal_line" ]; then
    warn "[$cam] journal contains FATAL/panic lines:"
    echo "$fatal_line"
    FAILED+=("$cam(fatal)")
  fi
  echo ""
done

# --- #1138: ALSO deploy the cam2-painter (frame-probe) binary when --frame-probe was given -------
if [ -n "$FRAME_PROBE_BIN" ]; then
  deploy_frame_probe_to_painter
fi

# --- Summary ------------------------------------------------------------------------------
echo "================================================================"
if [ "${#FAILED[@]}" -eq 0 ]; then
  log "FLEET ALIGNED: all of [$SET] on $NEW_VER and genlocking"
  exit 0
fi
err "FLEET NOT FULLY ALIGNED — issues: ${FAILED[*]}"
exit 1
