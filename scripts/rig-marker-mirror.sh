#!/usr/bin/env bash
# rig-marker-mirror.sh -- issue 1404: mirror cam2's QPSK marker log to dev1's :8890 (header below).
set -euo pipefail
#
# The entry point of the rig-marker-mirror --user service: resolves the painter camera (cam2) with
# scripts/camera-set.sh's camera_resolve and the fleet credential (the same default + SSH_PASS
# override as scripts/deploy-fleet.sh), then runs scripts/rig_marker_mirror.py: ONE long-lived ssh
# connection streaming `tail -c +1 -F /run/rig-qpsk-markers.csv` into the rig-lease server's serve
# dir (temp + atomic rename). Why one connection and not a 10 s scp timer, the stream semantics and
# the reconnect backoff: the module doc of scripts/rig_marker_mirror.py.
#
# The password reaches sshpass through $SSHPASS (`sshpass -e`), never the command line.
# A serve dir that is (inside) the lease dir, or not this user's private dir, is refused (exit 2).
#
# Env:
#   RIG_LEASE_SERVE_DIR   serve dir (default $XDG_RUNTIME_DIR/rig-lease-serve, scripts/rig_serve_files.py)
#   RIG_LEASE_DIR         the lease dir the serve dir must stay out of (default /var/tmp/rig-lease)
#   SSH_PASS              the fleet root password (default as in scripts/deploy-fleet.sh)
# Extra arguments pass through to rig_marker_mirror.py (--write-interval, --max-runtime for tests).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve()

PAINTER_CAMERA="cam2"   # the ONE fixed painter box (mirrors deploy-fleet.sh's deploy_frame_probe_to_painter)
SSH_PASS="${SSH_PASS:-newlevel}"

command -v sshpass >/dev/null 2>&1 || { echo "ERROR rig-marker-mirror: sshpass is required (apt-get install sshpass)" >&2; exit 1; }
camera_resolve "$PAINTER_CAMERA" || { echo "ERROR rig-marker-mirror: camera_resolve $PAINTER_CAMERA failed" >&2; exit 1; }

SSHPASS="$SSH_PASS" exec python3 "$HERE/rig_marker_mirror.py" --host "$CAMERA_IP" "$@"
