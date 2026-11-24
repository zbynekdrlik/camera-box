#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + path defaults only) -- deliberately NOT
# `set -euo pipefail`: sourcing runs in the CALLER's shell (scripts/dev1-dsync-iface.sh), and strict
# mode here would leak. Every function returns its own status and is safe under the caller's -euo.
#
# scripts/lib/dsync-local-iface.sh -- issue 1545: dev1's dsync on the PTP NIC the dantesync fleet
# declares for it (DANTESYNC_FLEET_PTP_NICS in scripts/lib/dantesync-fleet.sh).
#
# WHY: dev1's dsync.service (written at the dsync 1.8.0 install) runs dsync with no `--interface`.
# On 20.11.2026, right after the 1.35.16 roll, dsync joined the link-DOWN enp39s0 instead of the rig
# NIC enp38s0 and ran NTP-only until a hand drop-in pinned it. dev1 has no provisioning script, so
# this lib renders + installs that drop-in from the fleet fact (a reinstall gets it back with one
# command) and grades the live /status against it.
#
# Helpers:
#   dsync_local_iface_dropin_text NIC   -> the drop-in text (ExecStart reset + `--interface NIC`),
#                                          rc 1 + stderr on a NIC that is not one interface name
#   dsync_local_iface_install NIC       -> compare-then-write: an identical installed file = nothing
#                                          written, nothing restarted (`UNCHANGED`); else the file is
#                                          written (sudo -n, a .new sidecar + rename), then
#                                          daemon-reload + restart dsync (`WROTE`). A failed write
#                                          never restarts. rc 0 / 1.
#   dsync_local_iface_check NODE NIC    -> ONE read of dsync's /status through dsync_iface_grade
#                                          (scripts/lib/dsync-iface.sh, the one verdict, sourced by
#                                          the caller); prints `dsync-iface: node= declared= verdict=`
#                                          and, on a wrong NIC, the fix line. rc 0 ok / 1 wrong or
#                                          missing --interface / 2 unreadable. Read-only.
#
# Seams (tests): DSYNC_LOCAL_IFACE_DROPIN (the drop-in path), DSYNC_LOCAL_IFACE_BIN (the dsync binary
# the unit runs), DSYNC_LOCAL_IFACE_UNIT (the unit name); `sudo` / `systemctl` from PATH.

DSYNC_LOCAL_IFACE_DROPIN="${DSYNC_LOCAL_IFACE_DROPIN:-/etc/systemd/system/dsync.service.d/zz-camera-box-interface.conf}"
DSYNC_LOCAL_IFACE_BIN="${DSYNC_LOCAL_IFACE_BIN:-/usr/local/bin/dsync}"
DSYNC_LOCAL_IFACE_UNIT="${DSYNC_LOCAL_IFACE_UNIT:-dsync}"

# _dsync_local_iface_nic_ok NIC -> rc 0 iff NIC is ONE Linux interface name (the fleet fact's rule:
# 1-15 of [A-Za-z0-9_.-], first a letter or digit; the sets spelled out, issue 1302).
_dsync_local_iface_nic_ok() {
  local nic="${1-}" LC_ALL=C
  local lower='abcdefghijklmnopqrstuvwxyz' upper='ABCDEFGHIJKLMNOPQRSTUVWXYZ' digit='0123456789'
  local first="${lower}${upper}${digit}" rest="${lower}${upper}${digit}_.-"
  [[ $nic =~ ^[${first}][${rest}]{0,14}$ ]]
}

dsync_local_iface_dropin_text() {
  local nic="${1-}"
  if ! _dsync_local_iface_nic_ok "$nic"; then
    echo "dsync_local_iface: '$nic' is not one interface name -- nothing rendered" >&2
    return 1
  fi
  cat <<EOF
# camera-box issue 1545: dsync joins PTP on the NIC the dantesync fleet declares for this node
# (DANTESYNC_FLEET_PTP_NICS in scripts/lib/dantesync-fleet.sh), never a NIC it picks by itself.
# Written by scripts/dev1-dsync-iface.sh --apply.
[Service]
ExecStart=
ExecStart=${DSYNC_LOCAL_IFACE_BIN} --interface ${nic}
EOF
}

dsync_local_iface_install() {
  local nic="${1-}" dst="$DSYNC_LOCAL_IFACE_DROPIN" text tmp rc=0
  text="$(dsync_local_iface_dropin_text "$nic")" || return 1
  tmp="$(mktemp "${TMPDIR:-/tmp}/dsync-iface-dropin.XXXXXX")" || {
    echo "dsync_local_iface_install: cannot create a temp file -- nothing written" >&2
    return 1
  }
  printf '%s\n' "$text" >"$tmp" || rc=1
  if [ "$rc" -eq 0 ] && [ -f "$dst" ] && cmp -s "$tmp" "$dst"; then
    rm -f "$tmp"
    echo "dsync-iface: UNCHANGED $dst (--interface $nic); dsync not restarted"
    return 0
  fi
  if [ "$rc" -eq 0 ]; then
    sudo -n install -d -m 755 "${dst%/*}" \
      && sudo -n install -m 644 "$tmp" "$dst.new" \
      && sudo -n mv -f "$dst.new" "$dst" || rc=1
  fi
  rm -f "$tmp"
  if [ "$rc" -ne 0 ]; then
    sudo -n rm -f "$dst.new" 2>/dev/null || true
    echo "dsync_local_iface_install: could not write $dst -- dsync NOT restarted" >&2
    return 1
  fi
  if ! sudo -n systemctl daemon-reload || ! sudo -n systemctl restart "$DSYNC_LOCAL_IFACE_UNIT"; then
    echo "dsync_local_iface_install: $dst written, but daemon-reload / restart $DSYNC_LOCAL_IFACE_UNIT failed" >&2
    return 1
  fi
  echo "dsync-iface: WROTE $dst (--interface $nic); $DSYNC_LOCAL_IFACE_UNIT restarted"
}

dsync_local_iface_check() {
  local node="${1-}" nic="${2-}" verdict
  verdict="$(dsync_iface_grade "$nic")"
  echo "dsync-iface: node=${node} declared=${nic} verdict=${verdict}"
  case "$verdict" in
    ok:*) return 0 ;;
    unreadable | norig) return 2 ;;
  esac
  echo "fix: scripts/dev1-dsync-iface.sh --apply (the drop-in $DSYNC_LOCAL_IFACE_DROPIN)"
  return 1
}
