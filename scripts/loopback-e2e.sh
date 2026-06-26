#!/usr/bin/env bash
# Phase-1 NDI frame-loss/latency loopback E2E.
#
# Builds frame-probe on the dev machine, deploys it to a camera-box device (the
# off-air rig), runs the QR HDMI-loopback probe, pulls the JSON artifact, and
# exits non-zero on a failing verdict.
#
# On the device, camera-box normally runs with `--display`, which holds /dev/fb0.
# The probe's painter needs fb0, so this script stops the service and runs
# camera-box WITHOUT --display for the duration (keeps capture->NDI alive, frees
# fb0), then ALWAYS restores the service via a trap — even on failure.
#
# Env overrides: CAM CAM_IP CAM_PASS SOURCE MODE DURATION_SECS QR_SIZE SETTLE_MS
#                CAPTURE_FPS MAX_P99_MS MAX_FREEZE_PERIODS LOCAL_OUT
#
# Camera selection (#24): set CAM=cam1|cam2|cam3|cam4 to target a camera by NAME — its IP
# and NDI source are resolved from scripts/camera-set.sh (the single source of truth for
# the cam1-4 map). Defaults to cam2 (the off-air dev rig) so existing behaviour is
# unchanged. Explicit CAM_IP / SOURCE still override the resolved values (back-compat).
# To drive the whole set in one go, use scripts/loopback-e2e-all.sh.
#
# NO version-integrity gate here (#123) — INTENTIONALLY. The #123 gate (scripts/version-integrity-
# gate.sh) refuses a rig test unless the live strih+stream genlocked OBS stack matches the pinned
# build. Loopback is a SINGLE-BOX cam->NDI->tap test on the camera-box device itself; it never
# touches the strih/stream OBS stack (see the #66 NOTE below — there is NO downstream genlocked OBS
# in this path). So the strih/stream pinned-build gate does not apply; recording-e2e.sh (which DOES
# route through that stack) carries it. Do NOT add it here "to match" it.
set -euo pipefail

# Resolve the selected camera's IP + NDI source from the shared set.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
camera_resolve "${CAM:-cam2}"

CAM_IP="${CAM_IP:-$CAMERA_IP}"
CAM_PASS="${CAM_PASS:-newlevel}"
SOURCE="${SOURCE:-$CAMERA_SOURCE}"    # NDI name is "<machine> (<ndi_name>)"
MODE="${MODE:-coverage}"               # coverage (gate) | full-rate (stress)
DURATION_SECS="${DURATION_SECS:-300}"  # default 5 min evidence run
QR_SIZE="${QR_SIZE:-700}"
SETTLE_MS="${SETTLE_MS:-500}"          # must exceed observed max latency (1080p60 max ~290 ms)
# camera-box now captures + sends NDI at 1080p60 (#11). The probe's coverage
# oversample math needs the real capture rate; at 60 fps capture the default
# 12 fps coverage paint is ~5x oversampled (>= 2 samples/id required).
CAPTURE_FPS="${CAPTURE_FPS:-60}"
# Hard latency/freeze gates (spec §15). Defaults are RIG-SPECIFIC — derived from
# the cam2 1080p60 baseline (2026-06-08, 180s coverage: p99 280 ms, max 290 ms,
# zero loss; ShadowCast 2 USB capture + V4L2 4-buffer queue + NDI roundtrip
# dominate the single-box loopback path) with ~1.25x margin so normal jitter
# does not flake. The 60fps loopback latency is ~2x the old 30fps baseline
# (p99 157 ms) — a deeper but stable pipeline, NOT CPU starvation (box was 50%
# idle). RE-BASELINE when the capture dongle / cabling / device / fps changes.
MAX_P99_MS="${MAX_P99_MS:-350}"            # fail if p99 latency > this (1080p60)
MAX_FREEZE_PERIODS="${MAX_FREEZE_PERIODS:-6}"  # fail if a stall repeats > this many frames
LOCAL_OUT="${LOCAL_OUT:-./frame-probe-${MODE}.json}"
REMOTE_BIN="/tmp/frame-probe"
REMOTE_OUT="/tmp/frame-probe.json"

ssh_cam() { sshpass -p "$CAM_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@${CAM_IP}" "$@"; }

# Build the env-assignment prefix that is shipped to the remote shell ahead of `bash -s`.
# These values include the free-text `source` workflow input, so the quoting MUST be
# injection-safe (see #39 regression test).
build_remote_env() {
  # printf %q emits a shell-safe quoting of every value, so a single quote (or any
  # metacharacter) in a free-text value cannot break out and inject into the remote
  # root shell — the #39 hardening. Safe here because BOTH this builder (#!/usr/bin/env
  # bash) and the remote consumer (`bash -s`) are bash: %q may emit bash-only $'…'
  # ANSI-C quoting, which a POSIX sh/dash would not parse — don't reuse this in an sh ctx.
  printf 'MODE=%q DURATION=%q SOURCE=%q QR_SIZE=%q SETTLE_MS=%q CAPTURE_FPS=%q MAX_P99_MS=%q MAX_FREEZE_PERIODS=%q OUT=%q' \
    "$MODE" "$DURATION_SECS" "$SOURCE" "$QR_SIZE" "$SETTLE_MS" "$CAPTURE_FPS" "$MAX_P99_MS" "$MAX_FREEZE_PERIODS" "$REMOTE_OUT"
}

# When sourced (the #39 regression test exercises build_remote_env in isolation), stop
# before the build/deploy/ssh actions. Direct execution (`bash scripts/loopback-e2e.sh`)
# has $0 == BASH_SOURCE and runs the full flow below.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

echo ">> Building frame-probe (release, --features probe)"
cargo build --release --features probe --bin frame-probe

echo ">> Deploying to ${CAM_IP}:${REMOTE_BIN}"
sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no target/release/frame-probe "root@${CAM_IP}:${REMOTE_BIN}"

echo ">> Running ${MODE} loopback for ${DURATION_SECS}s on ${CAM_IP} (source: '${SOURCE}')"
echo "   camera-box runs WITHOUT --display for the duration; service is restored afterwards."

# Remote orchestration. Quoted heredoc — values are passed via env on the ssh line,
# quoted by build_remote_env so a free-text value cannot inject into the remote shell.
set +e
ssh_cam "$(build_remote_env) bash -s" <<'REMOTE'
set -uo pipefail
export NDI_RUNTIME_DIR_V6=/usr/lib/ndi
chmod +x /tmp/frame-probe
MANUAL_PID=""
cleanup() {
  if [ -n "$MANUAL_PID" ]; then kill "$MANUAL_PID" 2>/dev/null || true; fi
  sleep 1
  systemctl start camera-box
  echo ">> CLEANUP: camera-box service restarted ($(systemctl is-active camera-box))"
}
trap cleanup EXIT HUP INT TERM

echo ">> stop camera-box (release fb0 + video0)"
systemctl stop camera-box
# Sweep any orphaned manual camera-box (a broken earlier cleanup can leave one outside
# systemd holding /dev/video0) — pkill -x is exact-name and cannot match this shell.
pkill -x camera-box 2>/dev/null || true
# WAIT until /dev/video0 is actually free instead of racing uvcvideo's async teardown
# with a fixed sleep (the EBUSY rc=3 failure of the first #9 dispatch). ~15s ceiling.
# fuser errors are silenced in the loop, so fail loudly if it's missing entirely —
# otherwise a future device image without psmisc would silently skip the wait.
command -v fuser >/dev/null 2>&1 || { echo "ERROR: fuser not found on device (psmisc)"; exit 5; }
i=0
while fuser -s /dev/video0 2>/dev/null && [ "$i" -lt 30 ]; do sleep 0.5; i=$((i + 1)); done
if fuser -s /dev/video0 2>/dev/null; then
  echo "ERROR: /dev/video0 still busy after stop+sweep:"; fuser -v /dev/video0 2>&1; exit 4
fi

echo ">> start camera-box WITHOUT --display (capture->NDI only)"
# #66 NOTE: loopback INTENTIONALLY does NOT set CAMERA_BOX_GENLOCK_FPS here (unlike the
# genlocked full-path run, recording-e2e.sh, which routes through the strih/stream OBS FIFO).
# Loopback measures cam->NDI->tap on the SAME box with frame-probe reading the NDI source
# directly; there is NO downstream genlocked OBS FIFO in this path, so the #66 genlock-decimation
# requirement does not apply. Loopback deliberately exercises the raw ~60fps capture path
# (CAPTURE_FPS=60). Do NOT add the genlock env here "to match the full-path run" — it would change
# what loopback measures.
nohup /usr/local/bin/camera-box >/tmp/cam-manual.log 2>&1 &
MANUAL_PID=$!
sleep 7
if ! kill -0 "$MANUAL_PID" 2>/dev/null; then
  echo "ERROR: manual camera-box died:"; tail -20 /tmp/cam-manual.log; exit 3
fi

setterm --blank 0 --powerdown 0 >/dev/null 2>&1 || true

echo ">> running frame-probe"
/tmp/frame-probe --mode "$MODE" --source "$SOURCE" --duration-secs "$DURATION" \
  --qr-size "$QR_SIZE" --settle-ms "$SETTLE_MS" --capture-fps "$CAPTURE_FPS" \
  --max-p99-latency-ms "$MAX_P99_MS" --max-freeze-periods "$MAX_FREEZE_PERIODS" \
  --out "$OUT" --connect-timeout-secs 20
rc=$?
echo "PROBE_RC=$rc"
exit $rc
REMOTE
PROBE_RC=$?
set -e

echo ">> Pulling artifact to ${LOCAL_OUT}"
sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no "root@${CAM_IP}:${REMOTE_OUT}" "$LOCAL_OUT" || echo "!! could not pull artifact"

if [ -f "$LOCAL_OUT" ]; then
  echo ">> Result summary:"
  python3 -c "
import json,sys
d=json.load(open('$LOCAL_OUT'))
l=d.get('latency') or {}
print(f\"  mode={d['mode']} verdict={'PASS' if d['verdict_pass'] else 'FAIL'}\")
print(f\"  emitted={d['emitted_count']} observed={d['observed_count']} unique={d['unique_observed']}\")
print(f\"  missing={len(d['missing_ids'])} reorders={len(d['reorders'])} freezes={len(d['freezes'])}\")
c=d.get('coverage') or {}
if c: print(f\"  coverage: oversample_p50={c['oversample_p50']} (floor {c['min_confirm_samples']}, oversampled={c['run_oversampled']}) confirmed_drops={len(c['confirmed_drops'])} inconclusive_gaps={len(c['inconclusive_gaps'])} low_coverage={len(c['low_coverage_ids'])}\")
if c and c['confirmed_drops']: print(f\"    CONFIRMED DROP ids: {c['confirmed_drops'][:20]}\")
if c and c['inconclusive_gaps']: print(f\"    inconclusive (torn-prone) ids: {c['inconclusive_gaps'][:20]}\")
if l: print(f\"  latency_ms: mean={l['mean_ms']:.1f} p50={l['p50_ms']:.1f} p95={l['p95_ms']:.1f} p99={l['p99_ms']:.1f} max={l['max_ms']:.1f} (n={l['samples']})\")
" 2>/dev/null || cat "$LOCAL_OUT"
fi

if [ "$PROBE_RC" -ne 0 ]; then
  echo "!! FRAME-PROBE FAILED (rc=${PROBE_RC}) — see ${LOCAL_OUT}"
  exit "$PROBE_RC"
fi
echo ">> PASS"
