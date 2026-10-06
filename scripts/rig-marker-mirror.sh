#!/usr/bin/env bash
# rig-marker-mirror.sh -- issue 1404: ONE pass mirroring cam2's QPSK marker log to dev1's :8890 (header below).
set -euo pipefail
#
# WHAT: scp cam2's `/run/rig-qpsk-markers.csv` (the growing marker emit log the cam2 painter writes,
# see .claude/rules/cam2-painter-lifecycle.md) into the rig-lease server's SERVE dir, through a temp
# file in that dir + an atomic rename. `scripts/rig-lease-server.py` serves it read-only at
# `http://dev1:8890/rig-qpsk-markers.csv` with an `X-Mirror-Age-S` header (seconds since the last
# successful pass = the renamed file's mtime).
#
# WHY: restreamer's YouTube A/V gate (restreamer issue 357) pairs the QPSK markers in the VOD against
# their emit times, and it has no fleet ssh -- the fleet credentials stay with camera-box. One dev1
# mirror gives every gate the log over plain LAN HTTP (design: issue 1404 comment 6023622339).
#
# RUN BY: systemd/rig-marker-mirror.timer (--user, every 10 s), shipped DISABLED; the supervisor
# installs it on dev1 and turns it on (the runbook: systemd/rig-marker-mirror.README.md).
#
# FAIL LOUD: any failure (cam2 unreachable, the file absent -- EVENT mode purges it --, an empty or
# foreign file) exits non-zero with an `ERROR` line on stderr and leaves the PREVIOUS mirror
# untouched, so its X-Mirror-Age-S keeps growing and a consumer sees the mirror is stale; nothing
# partial is ever served. A consumer must judge freshness from that header, never assume it.
#
# Env:
#   RIG_LEASE_SERVE_DIR        serve dir (default /var/tmp/rig-lease-serve = rig_serve_files.py's
#                              DEFAULT_SERVE_DIR; never the lease dir, whose existence = held=true)
#   SSH_PASS                   the fleet root password (the same default + override name as
#                              scripts/deploy-fleet.sh)
#   RIG_MARKER_MIRROR_TIMEOUT  seconds bounding the whole scp (default 8; the timer fires every 10 s)
#
# Traffic: one full copy per pass (~2 MB after a long painter session) on the rig LAN. While the rig
# is away at an event the painter is off and the file is purged, so a pass is one failed ssh login.
#
# Logging: a failure prints its ERROR line on every pass. A success prints one line only when it
# (re)establishes the mirror (no previous mirror, or the previous one older than
# RIG_MARKER_MIRROR_QUIET_AGE_S, default 30 s) -- a healthy 10 s cadence would otherwise write
# ~8600 identical lines a day (the unit also drops systemd's per-run Starting/Finished lines).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve()

PAINTER_CAMERA="cam2"   # the ONE fixed painter box (mirrors deploy-fleet.sh's deploy_frame_probe_to_painter)
REMOTE_PATH="/run/rig-qpsk-markers.csv"
MIRROR_NAME="rig-qpsk-markers.csv"
MARKER_HEADER="index,frame_id,emit_ts_ns"
SERVE_DIR="${RIG_LEASE_SERVE_DIR:-/var/tmp/rig-lease-serve}"
SSH_PASS="${SSH_PASS:-newlevel}"
SCP_TIMEOUT="${RIG_MARKER_MIRROR_TIMEOUT:-8}"
QUIET_AGE_S="${RIG_MARKER_MIRROR_QUIET_AGE_S:-30}"

die() { echo "ERROR rig-marker-mirror: $*" >&2; exit 1; }

# The serve dir must never be (inside) the lease dir: `mkdir -p` there would create the lease dir,
# whose mere existence is a held lease (scripts/rig_serve_files.py serve_dir_conflict).
LEASE_DIR="$(realpath -m "${RIG_LEASE_DIR:-/var/tmp/rig-lease}")"
SERVE_REAL="$(realpath -m "$SERVE_DIR")"
case "$SERVE_REAL/" in
  "$LEASE_DIR/"*) die "serve dir $SERVE_DIR is (inside) the lease dir $LEASE_DIR -- refusing" ;;
esac

command -v sshpass >/dev/null 2>&1 || die "sshpass is required (apt-get install sshpass)"
camera_resolve "$PAINTER_CAMERA" || die "camera_resolve $PAINTER_CAMERA failed"
mkdir -p "$SERVE_DIR" || die "cannot create serve dir $SERVE_DIR"

# Age of the mirror this pass replaces (-1 = none), for the quiet-success logging above.
PREV_AGE=-1
if [ -f "$SERVE_DIR/$MIRROR_NAME" ]; then
  PREV_MTIME="$(stat -c %Y "$SERVE_DIR/$MIRROR_NAME")" || die "cannot stat $SERVE_DIR/$MIRROR_NAME"
  PREV_AGE=$(( $(date +%s) - PREV_MTIME ))
fi

TMP="$(mktemp "$SERVE_DIR/.${MIRROR_NAME}.XXXXXX")" || die "cannot create a temp file in $SERVE_DIR"
trap 'rm -f "$TMP"' EXIT

# `timeout` sits INSIDE sshpass (sshpass stays the outer command, the fleet-script convention).
# UserKnownHostsFile=/dev/null: a re-provisioned cam2 gets a new host key every time.
if ! sshpass -p "$SSH_PASS" timeout "$SCP_TIMEOUT" scp -q \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
    -o ConnectTimeout=5 \
    "root@${CAMERA_IP}:${REMOTE_PATH}" "$TMP"; then
  die "scp of ${REMOTE_PATH} from ${PAINTER_CAMERA} (${CAMERA_IP}) failed -- previous mirror kept"
fi

[ -s "$TMP" ] || die "${REMOTE_PATH} on ${PAINTER_CAMERA} (${CAMERA_IP}) is empty -- previous mirror kept"
# The painter writes a `# qpsk-params ...` line then the column header; refuse anything else.
# (No `head | grep -q` pipe: under pipefail an early grep exit can SIGPIPE head into a false refusal.)
FIRST_LINES="$(head -n 3 "$TMP")" || die "cannot read $TMP"
if ! grep -qxF "$MARKER_HEADER" <<<"$FIRST_LINES"; then
  die "${REMOTE_PATH} on ${PAINTER_CAMERA} (${CAMERA_IP}) has no '${MARKER_HEADER}' header -- previous mirror kept"
fi

chmod 0644 "$TMP" || die "chmod of $TMP failed"
mv -f "$TMP" "$SERVE_DIR/$MIRROR_NAME" || die "rename into $SERVE_DIR/$MIRROR_NAME failed"
trap - EXIT
if [ "$PREV_AGE" -lt 0 ] || [ "$PREV_AGE" -gt "$QUIET_AGE_S" ]; then
  echo "rig-marker-mirror: mirror established ${PAINTER_CAMERA} (${CAMERA_IP}) ${REMOTE_PATH} -> $SERVE_DIR/$MIRROR_NAME ($(wc -c < "$SERVE_DIR/$MIRROR_NAME") bytes; previous mirror age ${PREV_AGE} s, -1 = none)"
fi
