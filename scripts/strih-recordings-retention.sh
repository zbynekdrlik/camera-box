#!/usr/bin/env bash
# strih-recordings-retention.sh (#1122) — scp the dry-run-first retention .ps1 to a rig OBS box and run it.
set -euo pipefail
#
# Deploys scripts/strih-recordings-retention.ps1 to a Windows OBS box (strih by default) and runs
# it, DRY-RUN by default. This is the deploy-genlock-fleet.sh emission style: the .ps1 goes over
# `scp -O` and is invoked with `powershell -NoProfile -ExecutionPolicy Bypass -File <remote.ps1>` —
# NEVER a nested `powershell -Command` over ssh (which fails silently on this rig, per
# .claude/rules/rig-state-inspection.md).
#
# The E2E harness (scripts/recording-e2e.sh) records one OBS program capture per run into the box's
# live OBS record directory (strih: D:\_REC); [8/8e] only deletes each run's OWN file, so aborted /
# skipped / failed-download runs leak forever (strih: 344 .mkv = ~691 GiB vs the 50 GB budget).
# The .ps1 keeps the newest N runs UNION anything younger than D days and deletes ONLY files
# matching the harness's OWN OBS-timestamp allowlist — never a generic *.mkv sweep, so a
# differently-named operator recording (e.g. strih700105.mkv) is always protected.
#
# ** The first real --execute run is the SUPERVISOR's explicit, reviewed step (#1122). ** Run the
# dry-run first, read the printed plan, and only then re-run with --execute. This deployer's dry-run
# leg is READ-ONLY (it lists a plan and deletes nothing).
#
# Usage:
#   scripts/strih-recordings-retention.sh                         # dry-run on strih (D:\_REC, keep 20 runs / 3 days)
#   scripts/strih-recordings-retention.sh --keep-runs 20 --keep-days 3
#   scripts/strih-recordings-retention.sh --host 10.77.9.204 --record-dir 'C:\Users\newlevel\Videos'  # stream box
#   scripts/strih-recordings-retention.sh --execute              # SUPERVISOR only — actually deletes
#
# Env: STRIH_SSH_PW (default "newlevel") — the box's ssh password (newlevel/newlevel).

HOST="10.77.9.202"
USER="newlevel"
RECORD_DIR="D:\\_REC"
KEEP_RUNS="20"
KEEP_DAYS="3"
BUDGET_GB="50"
EXECUTE=0
REMOTE_PATH='C:\Users\newlevel\strih-recordings-retention.ps1'

while [ $# -gt 0 ]; do
  case "$1" in
    --host)        HOST="$2"; shift 2 ;;
    --user)        USER="$2"; shift 2 ;;
    --record-dir)  RECORD_DIR="$2"; shift 2 ;;
    --keep-runs)   KEEP_RUNS="$2"; shift 2 ;;
    --keep-days)   KEEP_DAYS="$2"; shift 2 ;;
    --budget-gb)   BUDGET_GB="$2"; shift 2 ;;
    --remote-path) REMOTE_PATH="$2"; shift 2 ;;
    --execute)     EXECUTE=1; shift ;;
    -h|--help)     sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PS1_LOCAL="$HERE/strih-recordings-retention.ps1"
PW="${STRIH_SSH_PW:-newlevel}"

[ -f "$PS1_LOCAL" ] || { echo "missing $PS1_LOCAL" >&2; exit 1; }
command -v sshpass >/dev/null || { echo "sshpass not installed (sudo apt-get install -y sshpass)" >&2; exit 1; }

SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=15)

echo "[1/2] scp -O $PS1_LOCAL -> ${USER}@${HOST}:${REMOTE_PATH}"
sshpass -p "$PW" scp -O "${SSH_OPTS[@]}" "$PS1_LOCAL" "${USER}@${HOST}:${REMOTE_PATH}"

MODE_ARG=""
if [ "$EXECUTE" = "1" ]; then
  MODE_ARG="-Execute"
  echo "[2/2] run (EXECUTE — DELETING): $REMOTE_PATH"
else
  echo "[2/2] run (DRY-RUN — no deletion): $REMOTE_PATH"
fi

# `powershell -File` with named params — NOT a nested `powershell -Command` over ssh.
# shellcheck disable=SC2029  # the remote-side expansion of these vars is intentional.
sshpass -p "$PW" ssh "${SSH_OPTS[@]}" "${USER}@${HOST}" \
  "powershell -NoProfile -ExecutionPolicy Bypass -File \"${REMOTE_PATH}\" -RecordDir \"${RECORD_DIR}\" -KeepRuns ${KEEP_RUNS} -KeepDays ${KEEP_DAYS} -BudgetGb ${BUDGET_GB} ${MODE_ARG}"
