#!/usr/bin/env bash
# scripts/cambox-ro-units-apply.sh -- plan / apply the issue-1394 read-only-root unit set on live camboxes (header below).
set -euo pipefail
#
# Live 8.10.2026 (issue 1394): every cambox read `degraded` -- logrotate kept its state on the
# read-only root, the apt timers were only disabled, the issue-1311 netconsole oneshot never
# retried a boot-time miss. setup-device.sh writes the fixes on a new box; THIS brings an
# already-provisioned box up to date without a setup-device re-run, through the ONE remote program
# scripts/lib/cambox-ro-units.sh builds (cambox_ro_units_apply_program):
#   - the logrotate drop-in, the netconsole unit (where issue 1311 put one) and the four apt masks,
#     written inside ONE rw window closed by the verified close (scripts/lib/ro-window.sh): nothing
#     is started before the root reads read-only again, and a root that stays writable fails by
#     name with its writers;
#   - after the close: daemon-reload, stop the masked apt units, reset-failed, one logrotate run,
#     the netconsole restart, and the `is-system-running` read-back.
# It is a root write on a cambox, so --apply runs outside a production; it never reboots anything.
#
# Usage:
#   scripts/cambox-ro-units-apply.sh --plan  --box <name> [--box <name> ...]   print the program, touch nothing
#   scripts/cambox-ro-units-apply.sh --apply --box <name> [--box <name> ...]   run it as root over ssh (bash -s)
#   scripts/cambox-ro-units-apply.sh --plan|--apply --active                   every box in CAMERA_ACTIVE_SET
# A box name resolves through scripts/camera-set.sh (camera_resolve); --active reads the fleet from
# CAMERA_ACTIVE_SET (never a typed camera range). --apply goes on to the next box after a failure and
# names every failed box at the end.
#
# Env: CAM_PW (cambox root password, default newlevel -- the fleet's dev password, as verify-device.sh),
#      SSH_TIMEOUT (ssh ConnectTimeout, default 10).
# Exit: 0 every box OK; 1 a box failed (named on the last line); 2 a bad invocation (nothing touched).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# shellcheck source=scripts/lib/cambox-ro-units.sh
. "$HERE/lib/cambox-ro-units.sh"

CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"

usage() {
  cat <<'EOF'
Usage: scripts/cambox-ro-units-apply.sh --plan|--apply (--box <name> [--box <name> ...] | --active)
  --plan    print, per box, the remote program --apply would run; touches nothing
  --apply   run it as root on each box over ssh (bash -s), outside a production
  --box     a camera name from scripts/camera-set.sh (repeatable)
  --active  every box in CAMERA_ACTIVE_SET
Env: CAM_PW (default newlevel), SSH_TIMEOUT (default 10). Exit 0 ok, 1 a box failed, 2 usage.
EOF
}

MODE=""
BOXES=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan | --apply)
      if [ -n "$MODE" ]; then
        echo "ERROR: give --plan or --apply, not both" >&2
        usage >&2
        exit 2
      fi
      MODE="${1#--}"
      ;;
    --box)
      if [ "$#" -lt 2 ]; then
        echo "ERROR: --box needs a camera name" >&2
        usage >&2
        exit 2
      fi
      BOXES+=("$2")
      shift
      ;;
    --active)
      read -r -a _active <<<"$CAMERA_ACTIVE_SET"
      BOXES+=("${_active[@]}")
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done
if [ -z "$MODE" ] || [ "${#BOXES[@]}" -eq 0 ]; then
  usage >&2
  exit 2
fi

# Resolve every name BEFORE the first box is touched: a typo stops the run with nothing changed.
NAMES=()
IPS=()
for _box in "${BOXES[@]}"; do
  _name="$(printf '%s' "$_box" | tr '[:upper:]' '[:lower:]')"
  camera_resolve "$_name" || exit 2
  NAMES+=("$CAMERA_NAME")
  IPS+=("$CAMERA_IP")
  camera_is_active "$CAMERA_NAME" \
    || echo "NOTE: $CAMERA_NAME is not in CAMERA_ACTIVE_SET ('$CAMERA_ACTIVE_SET') -- going on, as asked" >&2
done

PROGRAM="$(cambox_ro_units_apply_program)"

if [ "$MODE" = plan ]; then
  for _i in "${!NAMES[@]}"; do
    printf '== %s (%s): the program --apply runs as root over ssh (bash -s); nothing was touched ==\n' \
      "${NAMES[$_i]}" "${IPS[$_i]}"
    printf '%s\n' "$PROGRAM"
  done
  exit 0
fi

command -v sshpass >/dev/null 2>&1 || {
  echo "ERROR: sshpass is required for --apply" >&2
  exit 2
}

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT
FAILED=()
for _i in "${!NAMES[@]}"; do
  _name="${NAMES[$_i]}"
  _ip="${IPS[$_i]}"
  echo "== $_name ($_ip): applying the issue-1394 read-only-root unit set =="
  _rc=0
  printf '%s\n' "$PROGRAM" \
    | sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout="$SSH_TIMEOUT" "root@$_ip" bash -s 2>&1 | tee "$LOG" || _rc=$?
  if [ "$_rc" -eq 0 ]; then
    echo "== $_name: OK =="
    continue
  fi
  _out="$(cat "$LOG")"
  if ro_window_close_failed "$_out"; then
    FAILED+=("$_name (root-rw: $(ro_window_holders "$_out"); nothing was started)")
  else
    FAILED+=("$_name (rc=$_rc)")
  fi
  echo "== $_name: FAILED (rc=$_rc) ==" >&2
done

if [ "${#FAILED[@]}" -gt 0 ]; then
  echo "RESULT: the issue-1394 unit set did NOT land on: $(printf '%s; ' "${FAILED[@]}")" >&2
  exit 1
fi
echo "RESULT: the issue-1394 unit set is in place on: ${NAMES[*]}"
