#!/usr/bin/env bash
# scripts/avsync-dev2-install.sh -- #1331 provision the dev2 lipsync measurer (see header below).
set -euo pipefail
#
# WHY (#1331): the SyncNet A/V-sync measurement moved OFF the encoding stream box onto dev2 (GPU).
# This script provisions dev2 to run scripts/avsync-measure-dev2.sh on a systemd --user timer:
# it rsyncs the SyncNet checkout + weights + the measurer's python deps from dev1 to dev2:~/avsync,
# builds a venv (torch/cv2/scipy/numpy from the system cu128 install via --system-site-packages;
# only the SyncNet-specific pip deps are added), and installs+enables the timer. IDEMPOTENT.
#
# RUNS ON dev1 (where the source lives); pushes to dev2 over key-auth ssh/rsync (dev1->dev2 is key
# auth -- machine-identities.md; NEVER a password here). The SUPERVISOR runs `--install`; `--check`
# is a read-only report anyone can run.
#
# Usage:
#   scripts/avsync-dev2-install.sh --check     # read-only: report every prerequisite's state, exit 0
#   scripts/avsync-dev2-install.sh --install    # provision dev2 (idempotent) then enable the timer
#   scripts/avsync-dev2-install.sh --help
#
# Exit codes: 0 = ok (check printed / install done), 2 = usage error, 3 = install step failed.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

# ── config (env-overridable) ──────────────────────────────────────────────────
DEV2_SSH="${AVSYNC_DEV2_SSH:-newlevel@dev2}"
REMOTE_BASE="${AVSYNC_DEV2_REMOTE_BASE:-avsync}"                 # relative to dev2's home
SYNCNET_SRC="${AVSYNC_DEV2_SYNCNET_SRC:-$HOME/devel/syncnet_python}"
SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=10 -o BatchMode=yes)
# SyncNet-specific pip deps NOT covered by the dev2 system cu128 stack (torch/torchvision/numpy/
# scipy/opencv/tqdm/websocket are system-provided and reused via --system-site-packages). Keep this
# the single source of truth; --check reports each import's presence.
PIP_DEPS=(scenedetect python_speech_features)
# --check verifies these modules import in the venv (system cu128 stack + PIP_DEPS). Kept inline
# in the remote heredoc's own loop below (a single-quoted heredoc cannot interpolate this array):
#   torch numpy scipy cv2 scenedetect python_speech_features tqdm websocket
# the python helper files the measurer + av_sync_measure.py need co-located at dev2:~/avsync/
PY_FILES=(av_sync_measure.py avsync_freshness.py av_sync_outer_loop_guard.py obs_phase2.py)
UNITS=(avsync-measure-dev2.service avsync-measure-dev2.timer)

usage() { sed -n '4,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

log() { printf '%s [avsync-dev2-install] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }
die() { log "FATAL: $*"; exit 3; }

rssh() { ssh "${SSH_OPTS[@]}" "$DEV2_SSH" "$@"; }

# ── --check : read-only report of every prerequisite ─────────────────────────
do_check() {
  local ok=1
  echo "# avsync-dev2-install --check ($DEV2_SSH)"
  # source presence (dev1 side)
  if [ -d "$SYNCNET_SRC" ] && [ -f "$SYNCNET_SRC/data/syncnet_v2.model" ]; then
    echo "dev1 syncnet source        OK   ($SYNCNET_SRC, weights present)"
  else
    echo "dev1 syncnet source        MISSING ($SYNCNET_SRC or its weights)"; ok=0
  fi
  local f
  for f in "${PY_FILES[@]}"; do
    if [ -f "$ROOT/scripts/$f" ]; then echo "dev1 script $f: OK"; else echo "dev1 script $f: MISSING"; ok=0; fi
  done
  # dev2 side (one ssh; each probe fail-safe)
  local remote
  remote="$(rssh "bash -s" <<'REMOTE' 2>/dev/null || true
set -uo pipefail
BASE="$HOME/avsync"
printf 'ffmpeg=%s\n' "$(command -v ffmpeg || echo MISSING)"
printf 'ffprobe=%s\n' "$(command -v ffprobe || echo MISSING)"
printf 'venv=%s\n' "$([ -x "$BASE/venv/bin/python" ] && echo OK || echo MISSING)"
printf 'syncnet=%s\n' "$([ -f "$BASE/syncnet_python/data/syncnet_v2.model" ] && echo OK || echo MISSING)"
printf 'measurer=%s\n' "$([ -x "$BASE/scripts/avsync-measure-dev2.sh" ] && echo OK || echo MISSING)"
printf 'heartbeat_dir=%s\n' "$([ -d "$HOME/.camera-box" ] && echo OK || echo ABSENT)"
if [ -x "$BASE/venv/bin/python" ]; then
  for m in torch numpy scipy cv2 scenedetect python_speech_features tqdm websocket; do
    "$BASE/venv/bin/python" -c "import importlib.util,sys; sys.exit(0 if importlib.util.find_spec('$m') else 1)" 2>/dev/null \
      && printf 'import.%s=OK\n' "$m" || printf 'import.%s=MISSING\n' "$m"
  done
fi
# systemctl prints its verdict ("enabled"/"not-found") to STDOUT even on a non-zero exit, so take
# only the FIRST line of the capture (a `|| echo` would append a second stray line -- the cosmetic
# double-output caught in --check testing) and default an empty capture to a clear sentinel.
te="$(systemctl --user is-enabled avsync-measure-dev2.timer 2>/dev/null | head -n1)"
printf 'timer=%s\n' "${te:-not-installed}"
ta="$(systemctl --user is-active avsync-measure-dev2.timer 2>/dev/null | head -n1)"
printf 'timer_active=%s\n' "${ta:-inactive}"
REMOTE
)"
  if [ -z "$remote" ]; then
    echo "dev2 probe                 UNREACHABLE (ssh $DEV2_SSH failed)"; ok=0
  else
    printf '%s\n' "$remote" | sed 's/^/dev2 /'
    printf '%s\n' "$remote" | grep -q '=MISSING' && ok=0
    printf '%s\n' "$remote" | grep -q 'UNREACHABLE' && ok=0
  fi
  if [ "$ok" -eq 1 ]; then echo "# RESULT: ready"; else echo "# RESULT: NOT ready (see MISSING/UNREACHABLE above)"; fi
  return 0
}

# ── --install : idempotent provision + enable ────────────────────────────────
do_install() {
  [ -d "$SYNCNET_SRC" ] || die "syncnet source not found at $SYNCNET_SRC (nothing to rsync)"
  command -v rsync >/dev/null 2>&1 || die "rsync not installed on dev1"

  log "1/6 create dev2 dirs"
  rssh "mkdir -p \"\$HOME/$REMOTE_BASE/scripts/lib\" \"\$HOME/.camera-box\" \"\$HOME/.config/systemd/user\"" \
    || die "mkdir on dev2 failed"

  log "2/6 rsync the measurer + its lib + the python helpers"
  rsync -az -e "ssh ${SSH_OPTS[*]}" \
    "$ROOT/scripts/avsync-measure-dev2.sh" "${DEV2_SSH}:${REMOTE_BASE}/scripts/" || die "rsync measurer failed"
  rsync -az -e "ssh ${SSH_OPTS[*]}" \
    "$ROOT/scripts/lib/avsync-measure.sh" "${DEV2_SSH}:${REMOTE_BASE}/scripts/lib/" || die "rsync lib failed"
  local f
  for f in "${PY_FILES[@]}"; do
    rsync -az -e "ssh ${SSH_OPTS[*]}" \
      "$ROOT/scripts/$f" "${DEV2_SSH}:${REMOTE_BASE}/" || die "rsync $f failed"
  done
  rssh "chmod +x \"\$HOME/$REMOTE_BASE/scripts/avsync-measure-dev2.sh\"" || true

  log "3/6 rsync syncnet_python (checkout + weights)"
  rsync -az --delete -e "ssh ${SSH_OPTS[*]}" \
    "$SYNCNET_SRC/" "${DEV2_SSH}:${REMOTE_BASE}/syncnet_python/" || die "rsync syncnet_python failed"

  log "4/6 build venv (--system-site-packages: reuse the system cu128 torch/cv2/scipy/numpy stack)"
  rssh "bash -s" <<REMOTE || die "venv setup on dev2 failed"
set -euo pipefail
BASE="\$HOME/$REMOTE_BASE"
[ -x "\$BASE/venv/bin/python" ] || python3 -m venv --system-site-packages "\$BASE/venv"
"\$BASE/venv/bin/python" -m pip install --upgrade pip >/dev/null
"\$BASE/venv/bin/python" -m pip install ${PIP_DEPS[*]}
REMOTE

  log "5/6 install systemd --user units"
  local u
  for u in "${UNITS[@]}"; do
    rsync -az -e "ssh ${SSH_OPTS[*]}" \
      "$ROOT/systemd/$u" "${DEV2_SSH}:.config/systemd/user/" || die "rsync unit $u failed"
  done

  log "6/6 daemon-reload + enable --now the timer"
  rssh "systemctl --user daemon-reload && systemctl --user enable --now avsync-measure-dev2.timer" \
    || die "systemctl enable failed (is 'loginctl enable-linger newlevel' set on dev2 for a headless timer?)"

  log "DONE. Verify: scripts/avsync-dev2-install.sh --check ; then on dev1 flip the watchdogs to AVSYNC_HEARTBEAT_HOST=dev2 and restart their timers."
}

# ── flow ─────────────────────────────────────────────────────────────────────
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0   # sourced (tests): only define functions above
fi

case "${1:-}" in
  --check) do_check ;;
  --install) do_install ;;
  -h|--help) usage; exit 0 ;;
  "") echo "avsync-dev2-install: need --check or --install (try --help)" >&2; exit 2 ;;
  *) echo "avsync-dev2-install: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac
