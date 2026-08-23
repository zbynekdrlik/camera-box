#!/usr/bin/env bash
# imag-nb power/thermal-envelope boot ONESHOT (#1040) — see the extended header below.
set -euo pipefail
# =============================================================================================
# Runs at every boot as ROOT (imag-power-envelope.service, a system oneshot with RemainAfterExit,
# mirroring imag-igpu-maxperf.service) because sysfs values reset on reboot. It pins the
# SUSTAINABLE power envelope that keeps the imag 60fps render inside budget:
#   1. slpc_ignore_eff_freq = 1 on every iGPU gt (glob card*, never a hardcoded cardN).
#   2. MMIO RAPL PL1 long_term = IMAG_PL1_W watts (default 45, #1162), enabled, on the package-0 zone —
#      selected by NAME/constraint IDENTITY, never a hardcoded intel-rapl-mmio index.
# thermald (which programmed the harmful 25 W) is PURGED by setup-imag.sh; PROCHOT stays as the
# hardware backstop; imag-power-envelope-guard.timer supervises this envelope loudly at runtime.
# Hardware-agnostic (issue 816): a box with no mmio RAPL zone logs + exits 0; a box that HAS the
# zone MUST assert the write took, else it fails loud (a silently-unapplied envelope is the exact
# regression this ticket closes). Env: IMAG_PL1_W (watts). See vendor/README.md `power_pl1_w_imag`.
# =============================================================================================

# Source the shared lib (imag_pl1_watts_to_uw + the IMAG_PL1_W default) from its installed path,
# with a repo-relative fallback for running out of a checkout. Non-fatal if absent — the arithmetic
# fallback below keeps the oneshot working standalone.
for _cand in \
  /usr/local/lib/imag-power-envelope.sh \
  "$(dirname "${BASH_SOURCE[0]}")/lib/imag-power-envelope.sh"; do
  # shellcheck source=scripts/lib/imag-power-envelope.sh disable=SC1090
  [ -r "$_cand" ] && { . "$_cand"; break; }
done

PL1_W="${IMAG_PL1_W:-45}"
LOG_TAG="${IMAG_POWER_LOG_TAG:-imag-power-envelope}"

log() { logger -t "$LOG_TAG" -- "$*" 2>/dev/null || true; echo "imag-power-envelope: $*"; }

# W -> uW: prefer the shared pure function; fall back to inline arithmetic if the lib was absent.
if declare -F imag_pl1_watts_to_uw >/dev/null 2>&1; then
  PL1_UW="$(imag_pl1_watts_to_uw "$PL1_W")" || { log "FATAL: invalid IMAG_PL1_W='$PL1_W'"; exit 1; }
else
  case "$PL1_W" in *[!0-9]* | '') log "FATAL: invalid IMAG_PL1_W='$PL1_W'"; exit 1 ;; esac
  PL1_UW=$(( PL1_W * 1000000 ))
fi

# 1. iGPU SLPC efficient-freq override knob -> 1 on every gt (glob card*, hardware-agnostic).
slpc_n=0
for s in /sys/class/drm/card*/gt/gt*/slpc_ignore_eff_freq; do
  [ -w "$s" ] || continue
  if echo 1 > "$s" 2>/dev/null; then
    slpc_n=$((slpc_n + 1))
    log "slpc_ignore_eff_freq=1 ($s)"
  else
    log "WARN: could not write slpc_ignore_eff_freq at $s"
  fi
done
[ "$slpc_n" -gt 0 ] || log "no writable slpc_ignore_eff_freq knob found (harmless if absent)"

# 2. MMIO RAPL PL1 long_term -> PL1_UW on the package-0 zone (identity-based selection).
found_zone=0
for z in /sys/class/powercap/intel-rapl-mmio:*/; do
  [ -e "${z}name" ] || continue
  [ "$(cat "${z}name" 2>/dev/null || true)" = "package-0" ] || continue
  found_zone=1
  ct_path=""
  for cn in "${z}"constraint_*_name; do
    [ -e "$cn" ] || continue
    if [ "$(cat "$cn" 2>/dev/null || true)" = "long_term" ]; then
      idx="${cn##*constraint_}"; idx="${idx%_name}"
      ct_path="${z}constraint_${idx}_power_limit_uw"
      break
    fi
  done
  if [ -z "$ct_path" ]; then
    log "FATAL: package-0 zone has no long_term constraint — cannot set the power envelope"
    exit 1
  fi
  if [ ! -w "$ct_path" ]; then
    log "FATAL: $ct_path is not writable (need root) — cannot set the power envelope"
    exit 1
  fi
  echo "$PL1_UW" > "$ct_path" 2>/dev/null || { log "FATAL: write of ${PL1_UW}uW to $ct_path failed"; exit 1; }
  # Enable the constraint AND assert it took -- the drift-guard/verify-imag verdict requires
  # enabled==1 for pl1|OK, so a silently-failed enable would leave a "success" oneshot yet a DRIFT
  # gate. Assert it the same way the limit write is asserted (never best-effort).
  if [ -w "${z}enabled" ]; then
    echo 1 > "${z}enabled" 2>/dev/null || { log "FATAL: could not enable the PL1 constraint at ${z}enabled"; exit 1; }
    en="$(cat "${z}enabled" 2>/dev/null || true)"
    [ "$en" = "1" ] || { log "FATAL: PL1 enable did not take (read '${en}') at ${z}enabled"; exit 1; }
  else
    log "FATAL: ${z}enabled not writable -- cannot enforce the power envelope"
    exit 1
  fi
  got="$(cat "$ct_path" 2>/dev/null || true)"
  if [ "$got" = "$PL1_UW" ]; then
    log "MMIO RAPL PL1 long_term=${PL1_UW}uW (=${PL1_W}W) enabled ($ct_path)"
  else
    log "FATAL: PL1 write did not take (wanted ${PL1_UW}uW, read '${got}') at $ct_path"
    exit 1
  fi
  break
done
[ "$found_zone" -eq 1 ] \
  || log "no intel-rapl-mmio package-0 zone on this box — nothing to pin (hardware-agnostic, issue 816)"

exit 0
