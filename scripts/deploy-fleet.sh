#!/usr/bin/env bash
# Align the whole camera-box fleet (cam1-6, #451) onto ONE pinned CI-built binary, in one command (#73).
#
# The fleet drifts: cameras get deployed at different times and end up on different versions
# (e.g. #73 found cam1/cam4=dev.29, cam3=dev.22, cam2=dev.19 — three builds, none current, cam2
# old enough that it predated the genlock-decimation report and so was NOT genlocking). This
# script makes re-alignment a single command: download the SAME CI artifact once, push it to
# every camera with the stop -> remount,rw -> scp -> remount,ro (verified) -> start cycle, then
# VERIFY each box reports the new version AND is emitting the genlock report ("N fps emitted / M fps
# captured"). issue 1407: the window closes with the ONE verified ro close (scripts/lib/ro-window.sh)
# BEFORE anything starts; a root that does not read ro again fails that box (holders named in FAILED)
# and starts nothing -- a start on a writable root opens the writers that keep it rw (issue 1405).
#
# Per deploy-from-clean-tree.md the deploy source is ALWAYS a CI artifact from a committed,
# pushed ref — never a locally built binary. This script downloads from a GitHub Actions run
# (default: the ci.yml run of the `main` head, via the shared head-anchored resolver
# scripts/lib/ci-run-resolve.sh -- a loud fallback while the head's run is in flight or failed, a
# refusal on a stale runs listing) or accepts a pre-downloaded binary path.
#
# Per approval-scope.md the deploy + the camera-box service restart it performs are the
# standing-approved WORK — this script does NOT ask permission and does NOT gate on "is it
# off-air / is there a live event". The operator who runs it guards live timing.
#
# Usage:
#   scripts/deploy-fleet.sh                       # deploy the main head's ci.yml artifact to cam1-6
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
#   BRANCH     branch whose head ci.yml run is used when --run/--binary omitted (default: main)
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
# shellcheck source=scripts/lib/ci-run-resolve.sh
. "$HERE/lib/ci-run-resolve.sh"   # ci_run_latest_success() -- the ONE head-anchored CI run resolver, shared with bkshading-deploy-relay.sh (issue 808, #1394)
# shellcheck source=scripts/lib/cam2-painter-deadman.sh
. "$HERE/lib/cam2-painter-deadman.sh"  # cam2_painter_deadman_arm_cmds() — the #1351 re-arm of the TRANSIENT deadman timer (a stopped systemd-run unit is GC'd, so `systemctl start` cannot revive it)
# shellcheck source=scripts/lib/ro-window.sh
. "$HERE/lib/ro-window.sh"  # ro_window_close_cmds / ro_window_holders — the ONE verified ro close of every rw window (issue 1407)
# The deadman re-fire window used when RESTORING a prior-armed deadman after the swap (#1351). The
# canonical value is 5 min (#1072); overridable, matching cam2-painter-deadman.sh's own convention.
CAM2_PAINTER_DEADMAN_MINUTES="${CAM2_PAINTER_DEADMAN_MINUTES:-5}"

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

# issue 1407: close a box's rw window with the ONE verified ro close (scripts/lib/ro-window.sh) --
# the ro remount, the root mode READ on the box, and on a root left writable the box's FAIL lines
# naming the writers. Returns 0 only when the root reads ro again. Otherwise it prints those lines,
# records FAILED as LABEL(root-rw: <holders>) (LABEL(root-unverified: ...) on an ssh failure) and
# returns 1, so the caller starts NOTHING.
close_ro_or_fail() {  # $1=ip $2=box name $3=FAILED label $4=what the box is left with
  local out rc=0 holders
  OPEN_WINDOW=""   # this close is the window's one close attempt (the EXIT path never repeats it)
  out="$(ssh_box "$1" "$(ro_window_close_cmds "issue 1407" "$2" "$4" \
    "stop that writer on $2, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run deploy-fleet.sh for it (never reboot a cambox remotely).")" 2>&1)" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  [ -z "$out" ] || printf '%s\n' "$out" >&2
  if [ "$rc" -eq 1 ]; then
    holders="$(ro_window_holders "$out")"
    err "[$3] root NOT read-only after the swap -- nothing started (holders: ${holders:-none named})"
    FAILED+=("$3(root-rw: ${holders:-none named})")
  else
    err "[$3] ssh failed during the ro close (rc $rc) -- the root mode is unverified, nothing started"
    FAILED+=("$3(root-unverified: ssh rc $rc)")
  fi
  return 1
}

# issue 1407 + #892: finish the cam2 painter swap. Only `systemctl enable cam2-painter.service` (the
# enable-now restore) runs inside the rw window; the window closes with the ONE verified ro close;
# only then the painter starts and the parked deadman is re-armed (#1351, prior state). A root left
# writable starts NOTHING: no painter and no deadman (its action would start the painter on that
# root). Returns non-zero when the painter was not restored (FAILED recorded).
painter_restore() {  # $1=ip $2=restore_action $3=prior deadman is-active $4=which binary runs now
  local ip="$1" action="$2" armed="$3" what="$4" painter="cam2" rc=0 active
  if [ "$action" = "enable-now" ] && ! ssh_box "$ip" "systemctl enable cam2-painter.service"; then
    err "[$painter] cam2-painter.service enable failed"; FAILED+=("$painter-painter(restart-failed)")
    rc=1
  fi
  close_ro_or_fail "$ip" "$painter" "$painter-painter" \
    "A cambox must never run on a writable root, so cam2-painter.service is NOT started and its dead-man is not re-armed." || return 1
  if [ "$action" != "enable-now" ]; then
    log "[$painter] cam2-painter.service left in its prior state (not re-armed, #892: an event-mode/dark painter must not return onto a live broadcast); $what"
  elif [ "$rc" -eq 0 ]; then
    if ! ssh_box "$ip" "systemctl start cam2-painter.service"; then
      err "[$painter] cam2-painter.service start failed"; FAILED+=("$painter-painter(restart-failed)"); rc=1
    else
      active="$(ssh_box "$ip" "systemctl is-active cam2-painter.service 2>/dev/null" || echo inactive)"
      if [ "$active" = "active" ]; then
        log "[$painter] cam2-painter.service re-armed + active on a root verified read-only; $what"
      else
        err "[$painter] cam2-painter.service not active after the start (is-active='$active')"; FAILED+=("$painter-painter(not-active)"); rc=1
      fi
    fi
  fi
  # #1351: re-arm the transient deadman timer parked before the swap, AFTER the verified close and the
  # #892 restore so it never races the painter start (prior-state restore, #892-safe).
  rearm_deadman_if_prior "$ip" "$armed" "$action"
  return "$rc"
}

# #1351: RESTORE the transient cam2-painter-deadman timer to its PRIOR state after a swap. A stopped
# systemd-run unit is garbage-collected, so `systemctl start …timer` can never revive it — the timer
# is RE-CREATED via the canonical builder (cam2_painter_deadman_arm_cmds → `systemd-run …`). Re-arm
# ONLY when it was armed BEFORE the park ($2=active) AND the #892 restore keeps the painter alive
# ($3=enable-now): a standalone deploy never arms a deadman that was absent, and a deliberately-dark
# event-mode painter is never resurrected (the deadman's action does `systemctl start cam2-painter`
# guarded only by frame-probe/burn presence, not by enabled-state — arming it onto a dark painter
# would violate #892). Best-effort; a box without the unit installed is a guarded no-op in the builder.
rearm_deadman_if_prior() {  # $1=ip  $2=prior deadman is-active  $3=restore_action
  [ "$2" = "active" ] && [ "$3" = "enable-now" ] || return 0
  ssh_box "$1" "$(cam2_painter_deadman_arm_cmds)" >/dev/null 2>&1 || true
}

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

  # #1351: capture the transient deadman timer's PRIOR armed-state so the swap can RESTORE it (a
  # stopped systemd-run unit is GC'd and cannot be `systemctl start`ed — see rearm_deadman_if_prior).
  local was_deadman_armed
  was_deadman_armed="$(ssh_box "$ip" "systemctl is-active ${CAM2_PAINTER_DEADMAN_UNIT}.timer 2>/dev/null" || true)"

  # #1351: park the transient cam2-painter-deadman re-armer BEFORE stopping the painter, then
  # remount rw for the swap. The deadman (scripts/lib/cam2-painter-deadman.sh) re-fires every ~5 min
  # and would resurrect the OLD binary mid-swap; a re-arm inside the ~2 s swap window is a second way
  # the old inode stays busy (→ ETXTBSY). It is a transient systemd-run unit, so a stop when it was
  # never armed (a bare deploy-fleet run outside E2E) is a harmless no-op (|| true).
  OPEN_WINDOW="$ip|$painter|$painter-painter"
  if ! ssh_box "$ip" "mount -o remount,rw / && (systemctl stop cam2-painter-deadman.timer 2>/dev/null || true) && (systemctl stop cam2-painter.service 2>/dev/null || true)"; then
    err "[$painter] remount-rw / painter stop failed"; FAILED+=("$painter-painter(stop-failed)")
    # issue 1407: the rw remount may have landed before the failure -- close it the verified way
    # (it records its own FAILED entry when the root stays writable).
    close_ro_or_fail "$ip" "$painter" "$painter-painter" "The painter swap stopped before the copy; nothing is started." || true
    return 0
  fi
  # #1351: ETXTBSY-proof swap — scp to a SIDECAR, then go live via ONE atomic rename. The painter
  # binary may still be executing (Restart=always) when scp opens the destination for writing, which
  # gives `Text file busy` on the live path; a rename replaces the directory entry while the running
  # process keeps its old inode, so ETXTBSY cannot occur by construction. byte-verify (below) reads
  # the FINAL path and is the real gate — a failed rename leaves stale/absent bytes there.
  if ! scp_box "$ip" "$FRAME_PROBE_BIN" "/usr/local/bin/frame-probe.new"; then
    err "[$painter] frame-probe scp failed"; FAILED+=("$painter-painter(scp-failed)")
    # issue 1407: the #892 restore of the OLD binary, the window closed verified first (painter_restore
    # records its own FAILED entry; this box already failed on the scp).
    painter_restore "$ip" "$restore_action" "$was_deadman_armed" "the OLD frame-probe (the swap failed)" || true
    return 0
  fi
  # chmod the sidecar, then atomically rename it over the (possibly running) live binary + fsync.
  ssh_box "$ip" "chmod 0755 /usr/local/bin/frame-probe.new && mv -f /usr/local/bin/frame-probe.new /usr/local/bin/frame-probe && sync" || true

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
  # issue 1407: the enable inside the window, the verified close, then the start + the deadman re-arm.
  painter_restore "$ip" "$restore_action" "$was_deadman_armed" "frame-probe swapped" || true
  echo ""
}

# Clean up a downloaded-artifact temp dir on exit (no-op when --binary was used).
DIST=""
# issue 1407: "ip|box|label" while a box's rw window is open (set just BEFORE the rw remount, cleared
# by close_ro_or_fail). A SIGINT/SIGTERM (a CI cancel) between the rw+stop and the close would
# otherwise leave that cambox on a writable root with its service stopped, silently.
OPEN_WINDOW=""
# shellcheck disable=SC2317  # invoked indirectly via the EXIT trap below
# NB: must not leak a non-zero status from the trap (it would override the script's exit code) —
# end with `:` so EXIT preserves the real exit status. An open window is closed the ONE verified way
# and nothing is started (the binary on the box may be half-copied): the box is named in FAILED.
cleanup() {
  if [ -n "$OPEN_WINDOW" ]; then
    set +e
    trap '' INT TERM
    local ip box label
    IFS='|' read -r ip box label <<<"$OPEN_WINDOW"
    err "[$label] deploy interrupted with the rw window open -- closing it (verified); the service on $box stays STOPPED"
    close_ro_or_fail "$ip" "$box" "$label" "The deploy was interrupted mid-swap; nothing is started." \
      && FAILED+=("$label(interrupted: root ro, service stopped)")
    err "DEPLOY INTERRUPTED — issues: ${FAILED[*]}"
  fi
  [ -n "$DIST" ] && rm -rf "$DIST"
  :
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- #1138 frame-probe-ONLY mode ----------------------------------------------------------
# --frame-probe WITHOUT --binary/--run deploys ONLY the cam2 painter (the auto-align path in
# scripts/lib/frame-probe-parity-align.sh), NEVER a camera-box fleet deploy. The align must swap
# just /usr/local/bin/frame-probe on cam2 -- re-deploying camera-box to the whole fleet would be a
# scope violation (and would collide with the camera-box parity align that already handles it). A
# bare / --binary / --run invocation is UNCHANGED (camera-box fleet deploy + the optional
# frame-probe tail below).
if [ -n "$FRAME_PROBE_BIN" ] && [ -z "$BINARY" ] && [ -z "$RUN_ID" ]; then
  [ -f "$FRAME_PROBE_BIN" ] || { err "--frame-probe '$FRAME_PROBE_BIN' not found"; exit 1; }
  # #1138 (review): the ONLY job of this mode is the cam2 painter deploy, so a run where cam2 is not
  # in $SET (a manual CAMERA_SET without cam2, or a painter-only active-set future) must FAIL LOUD,
  # never print the success banner over a silent skip. deploy_frame_probe_to_painter itself only
  # WARNs+returns on that case (correct for the tail-deploy, where camera-box is the main job).
  case " $SET " in
    *" cam2 "*) : ;;
    *) err "frame-probe-only: cam2 (the painter) not in CAMERA_SET [$SET] — nothing to deploy"; exit 1 ;;
  esac
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
    info "Resolving the ci.yml run of the '$BRANCH' head that carries '$ARTIFACT'..."
    # issue 808 + #1394: the ONE shared resolver -- the branch head's own run, else a loud fallback
    # to the newest older success carrying the artifact, else a loud refusal (a stale runs listing,
    # a gh error). Never the server-side `--status success --limit 1` pick that went stale.
    RUN_ID="$(ci_run_latest_success "$REPO" "$BRANCH" ci.yml "$ARTIFACT")" || RUN_ID=""
    [ -n "$RUN_ID" ] || { err "no usable ci.yml run on '$BRANCH' carries '$ARTIFACT' (the ci-run-resolve line above names why)"; exit 1; }
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
  # (never aborts the whole fleet under set -e). issue 1407: only the stop and the copy run inside
  # the rw window; it closes with the ONE verified ro close (close_ro_or_fail), and camera-box starts
  # only on a root that reads ro again. A root left writable is a FAILED box naming its writers --
  # never a start, never a swallowed close (every cambox runs read-only, setup-device STEP 18).
  info "[$cam] stop service + remount rw + copy + remount ro (verified) + start"
  OPEN_WINDOW="$ip|$cam|$cam"
  if ! ssh_box "$ip" "mount -o remount,rw / && systemctl stop camera-box"; then
    err "[$cam] remount-rw / stop failed"; FAILED+=("$cam(stop-failed)")
    # the rw remount may have landed before the failed stop (close_ro_or_fail records its own entry)
    close_ro_or_fail "$ip" "$cam" "$cam" "The camera-box stop for the swap failed; nothing was copied or started." || true
    continue
  fi
  if ! scp_box "$ip" "$BINARY" "/usr/local/bin/camera-box"; then
    err "[$cam] scp failed"; FAILED+=("$cam(scp-failed)")
    # bring camera-box back up, on a root verified read-only only. scp writes the target in place,
    # so a mid-transfer failure can leave a partial binary that fails to start: the box is already
    # FAILED (scp-failed) and the next deploy copies it again.
    if close_ro_or_fail "$ip" "$cam" "$cam" "A cambox must never run on a writable root, so camera-box.service is NOT started (the copy failed)."; then
      ssh_box "$ip" "systemctl start camera-box" || err "[$cam] camera-box start after the failed scp failed"
    fi
    continue
  fi
  close_ro_or_fail "$ip" "$cam" "$cam" "A cambox must never run on a writable root, so camera-box.service is NOT started (the new binary is in place)." || continue
  if ! ssh_box "$ip" "systemctl start camera-box"; then
    err "[$cam] start failed"; FAILED+=("$cam(start-failed)"); continue
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
