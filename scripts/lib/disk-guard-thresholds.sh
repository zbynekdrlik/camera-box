#!/usr/bin/env bash
# scripts/lib/disk-guard-thresholds.sh — pure threshold decision for cam-disk-guard (#369).
#
# No I/O, no SSH, no side effects — pure computation only. Sourced by cam-disk-guard.sh.
# Unit-tested by tests/appliance_boot_hardening.rs (disk_guard_threshold_decision_correct).
#
# The single function cam_disk_alert_needed(used_pct, threshold) answers "should we alert?"
# so the policy is in one place and the test proves the boundary is correct.

# Default alert threshold (%). Matches the 80% heuristic used for most disk-space monitoring.
# Override with CAM_DISK_ALERT_THRESHOLD env var before sourcing this file.
CAM_DISK_ALERT_THRESHOLD="${CAM_DISK_ALERT_THRESHOLD:-80}"

# cam_disk_alert_needed used_pct [threshold]
# Returns 0 (alert needed) when used_pct >= threshold; returns 1 (ok) when used_pct < threshold.
# Args:
#   $1 = used_pct (integer 0-100, as reported by `df --output=pcent`)
#   $2 = threshold (integer 0-100; defaults to CAM_DISK_ALERT_THRESHOLD = 80)
cam_disk_alert_needed() {
    local used_pct="${1:-0}"
    local threshold="${2:-${CAM_DISK_ALERT_THRESHOLD}}"
    [ "${used_pct}" -ge "${threshold}" ]
}
