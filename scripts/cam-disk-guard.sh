#!/usr/bin/env bash
# scripts/cam-disk-guard.sh — fleet disk-space guard for camera-box devices (#369).
#
# WHY (#369): a manual audit (2026-06-30) found cam4 at 92%-full (root partition never grown)
# and cam1 /var/cache at 100%-full (old 100M tmpfs size). A disk fill bricks the box via apt
# ENOSPC (#295). This guard runs on dev1 from a systemd --user timer, SSHes each cam box,
# checks `df /` and `df /var/cache`, and fires a Discord alert when any mount exceeds
# CAM_DISK_ALERT_THRESHOLD (default 80%).
#
# SHIPS DISABLED — see systemd/cam-disk-guard.{service,timer}. The supervisor installs and
# enables it (same pattern as rig-restore-watchdog, #281 Fix#3). Do NOT enable as part of
# merging this PR.
#
# All "is the usage too high?" decisions live in scripts/lib/disk-guard-thresholds.sh (pure,
# unit-tested). This script only gathers disk observations and fires the alert.
#
# Usage:
#   scripts/cam-disk-guard.sh            # one pass: observe → alert if over threshold
#   scripts/cam-disk-guard.sh --dry-run  # observe + log only; never alert
#   scripts/cam-disk-guard.sh --help

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/disk-guard-thresholds.sh
. "$HERE/lib/disk-guard-thresholds.sh"

# ── config (all env-overridable) ─────────────────────────────────────────────
DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "cam-disk-guard: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# Cam boxes (cam3 excluded — pending re-image, #301).
CAM1_IP="${CAM1_IP:-10.77.9.61}"
CAM2_IP="${CAM2_IP:-10.77.9.62}"
CAM4_IP="${CAM4_IP:-10.77.9.64}"
CAM_PW="${CAM_PW:-newlevel}"

SSH_TIMEOUT="${CAM_DISK_GUARD_SSH_TIMEOUT:-8}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${CAM_DISK_GUARD_REPO:-zbynekdrlik/camera-box}"

# Mounts to check on each cam box.
CHECK_MOUNTS="/ /var/cache"

# ── logging ──────────────────────────────────────────────────────────────────
log() { printf '%s [cam-disk-guard] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── SSH helper ───────────────────────────────────────────────────────────────
_ssh_cam() {
    local ip="$1"; shift
    sshpass -p "$CAM_PW" ssh \
        -o StrictHostKeyChecking=no \
        -o ConnectTimeout="$SSH_TIMEOUT" \
        -o BatchMode=no \
        root@"$ip" "$@" 2>/dev/null
}

# ── observe one cam box ───────────────────────────────────────────────────────
# probe_cam <name> <ip>
# Echoes one line per mount: "cam=<name> mount=<path> used_pct=<N>"
# Silently returns nothing if the box is unreachable (conservative — don't alert on dead box).
probe_cam() {
    local name="$1" ip="$2"
    local mounts_data
    mounts_data="$(_ssh_cam "$ip" "df --output=target,pcent $CHECK_MOUNTS 2>/dev/null | tail -n +2")" || {
        log "cam $name ($ip): UNREACHABLE — skipping"
        return 0
    }
    while IFS= read -r line; do
        local mount used_pct
        mount="$(printf '%s' "$line" | awk '{print $1}')"
        used_pct="$(printf '%s' "$line" | awk '{print $2}' | tr -d '%')"
        [[ -n "$mount" && -n "$used_pct" ]] || continue
        log "cam $name ($ip): mount=$mount used=${used_pct}%"
        printf 'cam=%s mount=%s used_pct=%s\n' "$name" "$mount" "$used_pct"
    done <<< "$mounts_data"
}

# ── collect observations in the MAIN shell (#403) ─────────────────────────────
# Mirror of the #394 rig-restore-watchdog fix: run each probe in the long-lived main shell with
# its records REDIRECTED to a temp file (a redirection does not fork), NOT inside a per-probe
# command substitution. Under the systemd oneshot unit, log() stderr from a fast-exiting `$()`
# subshell child is intermittently dropped by journald; main-shell collection makes the observation
# log reliable. Result lands in DISK_OBS.
DISK_OBS=""
collect_observations() {
    local obs_file
    obs_file="$(mktemp "${TMPDIR:-/tmp}/camera-box-cam-disk-guard-obs.XXXXXX")"
    probe_cam cam1 "$CAM1_IP" >>"$obs_file"
    probe_cam cam2 "$CAM2_IP" >>"$obs_file"
    probe_cam cam4 "$CAM4_IP" >>"$obs_file"
    DISK_OBS="$(cat "$obs_file")"
    rm -f "$obs_file"
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
    log "pass start (threshold=${CAM_DISK_ALERT_THRESHOLD}%, dry_run=$DRY_RUN)"

    local alerts=()
    # #1206: parallel cam:mount identity tokens (NOT used_pct — which churns per pass) so the
    # dedup key distinguishes DISTINCT incidents (cam1 / full vs cam4 /var/cache full) while a
    # repeat of the SAME over-threshold set edits the card instead of re-pinging.
    local key_ids=()
    # #403: collect in the main shell (no $() subshell per probe) so journald keeps the obs log.
    collect_observations

    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        local cam mount used_pct
        cam="$(printf '%s' "$line" | sed -n 's/.*cam=\([^ ]*\).*/\1/p')"
        mount="$(printf '%s' "$line" | sed -n 's/.*mount=\([^ ]*\).*/\1/p')"
        used_pct="$(printf '%s' "$line" | sed -n 's/.*used_pct=\([^ ]*\).*/\1/p')"
        if cam_disk_alert_needed "${used_pct:-0}"; then
            log "ALERT: cam=$cam mount=$mount used=${used_pct}% >= ${CAM_DISK_ALERT_THRESHOLD}%"
            alerts+=("cam $cam $mount ${used_pct}% (≥${CAM_DISK_ALERT_THRESHOLD}%)")
            key_ids+=("${cam}:${mount}")
        fi
    done <<< "$DISK_OBS"

    if [[ ${#alerts[@]} -eq 0 ]]; then
        log "pass end — all cam boxes below threshold"
        return 0
    fi

    local msg
    msg="⚠️ #369 cam-disk-guard: disk usage alert on ${#alerts[@]} mount(s) — ${REPO_SLUG}"$'\n'
    for a in "${alerts[@]}"; do
        msg+="  $a"$'\n'
    done
    msg+="Root auto-grow runs on first boot (camera-box-grow-root.service); if still high, check."

    log "ALERT: ${#alerts[@]} mount(s) over threshold"
    local disk_key
    disk_key="cam-disk-$(printf '%s\n' "${key_ids[@]}" | LC_ALL=C sort | tr '\n' ',' | sed 's/,$//')"
    if [[ "$DRY_RUN" -eq 1 ]]; then
        log "[dry-run] WOULD fire Discord (dedup-key=$disk_key): $msg"
    else
        python3 "$NOTIFY" notify --body "$msg" --dedup-key "$disk_key" >/dev/null 2>&1 \
            || log "notify failed (non-fatal) — alert still logged above"
    fi

    log "pass end — alerted on ${#alerts[@]} mount(s)"
}

# #403: run main ONLY when executed, not when sourced — so collect_observations is unit-testable
# with stubbed probes (mirrors the #394 rig-restore-watchdog guard).
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main
fi
