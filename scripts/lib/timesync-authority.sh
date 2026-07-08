#!/usr/bin/env bash
# scripts/lib/timesync-authority.sh — shared "dantesync is the SOLE timesync authority" verdict
# (#591, extracted for reuse by #596).
#
# The rig's clock master is dantesync (PTP/NTP). A minimalist cambox/imag appliance must run NO
# other timesync daemon -- cam5/cam6 shipped with systemd-timesyncd active ALONGSIDE dantesync,
# causing a real 5.28-second clock desync ([NTP] offset:-5280959us) invisible to weeks of "passing"
# verification. This gate makes a 2nd authority impossible to ship: a competing daemon that is
# INSTALLED (even masked), ACTIVE, or merely enabled/unmasked is a hard FAIL. dantesync is the ONE
# authority that must be present; it is deliberately NOT in the competing set.
#
# scripts/verify-device.sh's (r) check (the cam1-6 fleet's post-reboot acceptance gate) and
# scripts/drift-guard.sh's --check-imag facet (imag-nb, #596 -- the SAME class of gap #591 closed
# on the fleet) both need the IDENTICAL verdict. Originally these four functions lived only inside
# verify-device.sh; #596 extracted them here so drift-guard.sh can reuse them instead of carrying
# a second, driftable copy (the SAME discipline scripts/lib/ndi-alive.sh and scripts/lib/cli-log.sh
# already apply to their own shared checks).
#
# Source-only: this file defines four pure functions and performs no side effects on its own.

# dpkg_status_installed STATUS -> 0 iff STATUS (a `dpkg -s` "Status:" line value, e.g.
# "install ok installed") shows the package's files are CURRENTLY present on disk (a review-caught
# false-green gap, #591: the ORIGINAL form only matched the exact "install ok installed" triad,
# missing "hold ok installed" (an installed daemon the operator apt-mark-held) and every
# files-unpacked-but-not-fully-configured state -- "install ok unpacked" / "half-configured" /
# "half-installed" / "triggers-pending" / "triggers-awaiting" -- all of which leave the competing
# daemon's binary+unit on disk, exactly the "installed" state this check exists to reject even
# though it isn't yet an active systemd unit). dpkg's Status line is "<want> <flag> <state>"; the
# ONLY states meaning "files genuinely gone" are: EMPTY (dpkg has never heard of the package --
# never installed, or fully purged so no Status line prints at all), "not-installed", and
# "config-files" (removed, only leftover conffiles remain -- binary/unit ARE gone). Every OTHER
# state means the daemon's files are present on disk in some form -- treated as installed
# (test-strictness: an ambiguous/partial state is never read as "safely absent").
dpkg_status_installed() {
  case "$1" in
    '') return 1 ;;
    *' not-installed' | *' config-files') return 1 ;;
    *) return 0 ;;
  esac
}

# timesync_enabled_state_neutral STATE -> 0 iff STATE (trimmed `systemctl is-enabled` output) is a
# NEUTRAL/absent state: masked / disabled / not-found / empty. Any other state (enabled / static /
# alias / indirect / enabled-runtime / generated / linked) means the unit is still wired to start.
timesync_enabled_state_neutral() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    '' | masked | disabled | not-found) return 0 ;;
    *) return 1 ;;
  esac
}

# timesync_daemon_verdict NAME DPKG_STATUS IS_ACTIVE IS_ENABLED -> echoes "ok" or "FAIL: <reason>".
# A competing timesync daemon FAILs if it is INSTALLED (dpkg) OR ACTIVE OR not in a neutral
# enabled-state. Masking a competing daemon is NOT enough -- a minimalist appliance purges it, so
# INSTALLED = FAIL even when masked+inactive (the cam1-4 "installed-but-disabled" state now rejected).
timesync_daemon_verdict() {
  local name="$1" dpkg="$2" active="$3" enabled="$4"
  if dpkg_status_installed "$dpkg"; then
    printf 'FAIL: %s is INSTALLED -- purge it (a minimalist appliance runs only dantesync; masking is not enough)\n' "$name"
    return 0
  fi
  if [ "$(printf '%s' "$active" | tr -d '[:space:]')" = "active" ]; then
    printf 'FAIL: %s is ACTIVE -- a 2nd timesync daemon fights dantesync (cam5/6 -> 5.28s desync)\n' "$name"
    return 0
  fi
  if ! timesync_enabled_state_neutral "$enabled"; then
    printf 'FAIL: %s is enabled (state=%s) -- must be masked/disabled/purged\n' "$name" "${enabled:-<none>}"
    return 0
  fi
  printf 'ok\n'
}

# timesync_authority_verdict COLLECTED -> "ok" if EVERY competing daemon passes, else the
# newline-joined "FAIL: ..." reasons. COLLECTED is the live flow's gathered block: one line per
# daemon, pipe-delimited "NAME|DPKG_STATUS|IS_ACTIVE|IS_ENABLED" (the dpkg status carries spaces but
# never a `|`, so `|` is a safe field separator). Blank lines are skipped.
timesync_authority_verdict() {
  local collected="$1" name dpkg active enabled verdict fails="" nl
  nl=$'\n'
  while IFS='|' read -r name dpkg active enabled; do
    [ -n "$name" ] || continue
    verdict="$(timesync_daemon_verdict "$name" "$dpkg" "$active" "$enabled")"
    if [ "$verdict" != "ok" ]; then
      fails="${fails:+$fails$nl}$verdict"
    fi
  done <<< "$collected"
  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    printf 'ok\n'
  fi
}
