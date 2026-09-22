#!/usr/bin/env bash
# scripts/lib/intercom-target-dropin.sh — #1345 M1b: SINGLE SOURCE OF TRUTH for the /run systemd
# drop-in that repoints ONE dev cambox's VBAN intercom target at the Linux strih-lx hub. Extended
# header below `set`.
set -euo pipefail
#
# WHY (#1345 M1b): the cambox root filesystem is READ-ONLY, so `/etc/camera-box/config.toml` cannot
# be edited to change the intercom target host. camera-box reads a `CAMERA_BOX_INTERCOM_TARGET` env
# override instead (precedence env > CLI flag > config.toml, see `src/intercom_target.rs`). A drop-in
# under /run (tmpfs) that sets that `Environment=` line — so a reboot auto-reverts to the deployed
# Windows-strih target — is the only appliance-side seam. This repoints ONLY the DEV cambox cam1 for
# the M1 loopback test; cam2-7 stay on the Windows strih until the M4 cut-over (repointing a
# production cambox mid-show = double talkback).
#
# Source-only: this file defines a constant + PURE `*_cmds` string builders that PRINT remote bash
# text and runs nothing itself (no ssh, no systemctl). It is a SUPERVISOR tool used by hand at the
# M1b live step; it is deliberately NOT wired into recording-e2e.sh or rig-mode.sh. Every printed
# statement ends with `;` so embedding a builder via `$(...)` — which strips the trailing newline —
# can never glue the builder's last statement onto whatever the caller concatenates after it (the
# CLAUDE.md `$(...)` newline-strip gotcha).

# The TRANSIENT intercom-target drop-in path (#1345). In /run (tmpfs) so a reboot auto-reverts to the
# deployed unit's config.toml/CLI target. Overridable for a test; this default IS the single source.
INTERCOM_TARGET_DROPIN="${INTERCOM_TARGET_DROPIN:-/run/systemd/system/camera-box.service.d/zz-intercom-target.conf}"

# intercom_target_dropin_set_cmds <host> [DROPIN] -> the REMOTE bash that WRITES the drop-in setting
# `Environment=CAMERA_BOX_INTERCOM_TARGET=<host>`, `daemon-reload`s, `restart`s camera-box, then reads
# the effective Environment back and greps the var (fail-loud if the override did not take). <host>
# is validated HERE (in the builder): non-empty and no whitespace / newline / quote — otherwise the
# builder prints NOTHING and returns non-zero (a bad value would corrupt the Environment= directive).
intercom_target_dropin_set_cmds() {
  local host="${1:-}"
  local dropin="${2:-$INTERCOM_TARGET_DROPIN}"
  if [[ -z "$host" || "$host" == *[[:space:]\'\"]* ]]; then
    echo "intercom_target_dropin_set_cmds: invalid host '$host' (empty or contains whitespace/quote)" >&2
    return 1
  fi
  local dropin_dir
  dropin_dir="$(dirname "$dropin")"
  # `printf '%s\n' ...` writes the two config lines; single-quoted values are safe because the host
  # is validated above. Each statement is `;`-terminated (see header).
  cat <<SET
mkdir -p '$dropin_dir' ;
printf '%s\n' '[Service]' 'Environment=CAMERA_BOX_INTERCOM_TARGET=$host' > '$dropin' ;
systemctl daemon-reload ;
systemctl restart camera-box ;
systemctl show -p Environment --value camera-box | grep -q 'CAMERA_BOX_INTERCOM_TARGET=$host' ;
SET
}

# intercom_target_dropin_clear_cmds [DROPIN] -> the REMOTE bash that REMOVES the drop-in (and its
# now-empty drop-in dir), `daemon-reload`s and `restart`s camera-box so it reverts to the deployed
# config.toml/CLI (Windows-strih) target. Fully idempotent (rm -f / rmdir / reload are safe no-ops).
intercom_target_dropin_clear_cmds() {
  local dropin="${1:-$INTERCOM_TARGET_DROPIN}"
  local dropin_dir
  dropin_dir="$(dirname "$dropin")"
  cat <<CLEAR
rm -f '$dropin' 2>/dev/null || true ;
rmdir '$dropin_dir' 2>/dev/null || true ;
systemctl daemon-reload 2>/dev/null || true ;
systemctl restart camera-box ;
CLEAR
}
