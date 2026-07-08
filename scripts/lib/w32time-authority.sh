#!/usr/bin/env bash
# scripts/lib/w32time-authority.sh — shared "dantesync is the SOLE timesync authority" verdict,
# the WINDOWS half of #591 (#598).
#
# #591/#596/#597 made a 2nd timesync authority a hard FAIL on the LINUX cam appliances
# (scripts/lib/timesync-authority.sh: dpkg-installed OR active OR enabled = FAIL, purge required).
# The SAME desync class exists on the WINDOWS OBS boxes that do the genlock — strih (10.77.9.202)
# and stream (10.77.9.204) are DanteSync slaves too, so the built-in Windows Time service
# (`W32Time`) must never be an ACTIVE (or latently-reviving) 2nd clock authority. Before this
# ticket that was a manual, unverified invariant (see .claude/skills/ops/SKILL.md): both boxes were
# fixed live 2026-07-07 (`W32Time` Stopped + Disabled), but nothing prevented drift back.
#
# WHY THESE FOUR SIGNALS (all READ-ONLY — this file never changes W32Time; purging/disabling a
# drifted box is a separate manual op): `sc query`/`sc qc`/`reg query` all answered successfully
# while W32Time was STOPPED on a live probe of strih+stream (2026-07-08, via the win-* MCP); the
# fourth, `w32tm /query /status`, instead FAILED with "The service has not been started."
# on both boxes in that same probe — which is itself real, useful signal (no Source line to read
# while stopped), not a gap. `w32tm /query /configuration` was tried first and rejected: it also
# fails outright while the service is stopped (same live probe), so it cannot ever prove the
# "cleanly stopped" case, and the configured sync mode is available from the registry instead:
#   * `sc query w32time`  -> STATE (RUNNING / STOPPED / ...)                     [works stopped]
#   * `sc qc w32time`     -> START_TYPE (AUTO_START / DEMAND_START / DISABLED)   [works stopped]
#   * `reg query HKLM\SYSTEM\CurrentControlSet\Services\W32Time\Parameters /v Type`
#                         -> Type (NTP / NT5DS / NoSync / AllSync)               [works stopped]
#   * `w32tm /query /status` -> the "Source:" line (which peer it is actually syncing from)
#                         [only present while RUNNING and synced]
#
# Live 2026-07-08 readings (both already fixed, both pass this gate):
#   strih:  STATE=STOPPED START_TYPE=DISABLED Type=NoSync
#   stream: STATE=STOPPED START_TYPE=DISABLED Type=NTP     (Type is a leftover config value;
#                                                            DISABLED means it can never run it)
#
# Verdict shape (#598's own spec): HARD-FAIL iff W32Time is RUNNING AND configured as an
# NTP/NT5DS sync client AND reports a real (non-blank, non-local) external Source. OK iff W32Time
# is disabled/stopped, or its configured Type is NoSync. Beyond that literal spec, this file adds
# ONE deliberate extra check mirroring the Linux gate's OWN "installed but merely masked is still
# a FAIL" philosophy (scripts/lib/timesync-authority.sh's `timesync_enabled_state_neutral`): a
# box whose START_TYPE is AUTO_START with an NTP/NT5DS Type is ALSO a FAIL even while momentarily
# stopped, because it will resurrect itself as a competing authority on the next reboot -- the
# exact "installed-but-disabled-yet-still-a-risk" class #591 exists to catch, just on the
# start-type axis instead of the package-installed axis. Fail-closed on anything unreadable
# (test-strictness: an unrecognised/absent STATE must never default to "ok").
#
# Source-only: this file defines pure functions + a read-only remote-gather snippet and performs
# no side effects on its own (mirrors scripts/lib/timesync-authority.sh and
# scripts/clock-offset-guard.sh's own "PURE functions, no network" convention).

# w32time_state_known STATE -> 0 iff STATE (a trimmed `sc query` STATE word) is one of the seven
# real Windows SERVICE_STATE values. Anything else (empty, garbled, a typo) is NOT known -- the
# caller must grade that as UNKNOWN, never silently pass it through as "not running" (#598
# fail-closed requirement).
w32time_state_known() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    RUNNING | STOPPED | START_PENDING | STOP_PENDING | PAUSED | PAUSE_PENDING | CONTINUE_PENDING)
      return 0 ;;
    *) return 1 ;;
  esac
}

# w32time_running STATE -> 0 iff STATE is exactly RUNNING (the only state where W32Time is
# actively serving/adjusting time right now).
w32time_running() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "RUNNING" ]
}

# w32time_autostarts START_TYPE -> 0 iff START_TYPE (a trimmed `sc qc` START_TYPE word) means the
# service starts ITSELF on the next boot with no manual trigger -- AUTO_START (2), including its
# delayed variant. DEMAND_START (manual-only) and DISABLED never self-start, so neither counts.
w32time_autostarts() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    AUTO_START | 'AUTO_START(DELAYED)') return 0 ;;
    *) return 1 ;;
  esac
}

# w32time_syncing_type REG_TYPE -> 0 iff REG_TYPE (the W32Time\Parameters\Type registry value) is
# a mode that actively pulls time FROM an external/domain source -- NTP (manual peer(s)) or NT5DS
# (domain-hierarchy client). NoSync (never syncs) and anything unrecognised are NOT this.
w32time_syncing_type() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    NTP | NT5DS) return 0 ;;
    *) return 1 ;;
  esac
}

# w32time_source_is_real SOURCE -> 0 iff SOURCE (the `w32tm /query /status` "Source:" line value)
# names a genuine external/network peer: non-empty and not one of the purely-local fallback
# references Windows itself prints when it has nothing real to sync from ("Local CMOS Clock",
# "Free-running System Clock"). A blank/local/unreadable Source means W32Time has nothing real to
# fight dantesync over, even if its Type says NTP/NT5DS.
w32time_source_is_real() {
  local s
  s="$(printf '%s' "$1" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
  case "$s" in
    '') return 1 ;;
    *'Local CMOS Clock'* | *'Free-running'*) return 1 ;;
    *) return 0 ;;
  esac
}

# w32time_daemon_verdict STATE START_TYPE REG_TYPE SOURCE -> "ok" | "FAIL: <reason>" |
# "UNKNOWN: <reason>".
#   STATE:      trimmed `sc query w32time` STATE word (e.g. "RUNNING", "STOPPED")
#   START_TYPE: trimmed `sc qc w32time` START_TYPE word (e.g. "AUTO_START", "DISABLED")
#   REG_TYPE:   the W32Time\Parameters\Type registry value (e.g. "NoSync", "NTP", "NT5DS")
#   SOURCE:     the `w32tm /query /status` "Source:" line value ("" if absent/stopped)
#
# FAIL (an ACTIVE 2nd clock authority): W32Time is RUNNING, its Type is NTP/NT5DS, and it reports
# a real external Source -- it is right now pulling time from somewhere other than dantesync.
# FAIL (a LATENT 2nd clock authority): W32Time is not running now, but START_TYPE is AUTO_START
# with an NTP/NT5DS Type -- it will come back as a competing authority on the very next reboot
# (the same "masking is not enough" class #591 already treats as a hard FAIL on Linux).
# UNKNOWN: STATE is unreadable (never silently graded as "not running"), or W32Time is RUNNING but
# its Type could not be read at all (cannot certify it is inert). Neither is ever "ok".
# OK: everything else -- in particular, disabled/stopped with no Type risk, or Type=NoSync.
w32time_daemon_verdict() {
  local state="$1" start_type="$2" reg_type="$3" source="$4"

  if ! w32time_state_known "$state"; then
    printf 'UNKNOWN: W32Time service STATE is unreadable (%s) -- cannot certify it is not a 2nd clock authority\n' \
      "${state:-<empty>}"
    return 0
  fi

  # RUNNING is graded ENTIRELY within this branch and never falls through to the latent-autostart
  # check below -- a currently-RUNNING box's live behavior (active fail / unreadable Type / benign
  # right now) is the whole story for "is it a 2nd authority THIS INSTANT"; whether it also happens
  # to be AUTO_START is moot while it is already running (that's not a *latent* risk, it's simply
  # covered by the active check above it). Without this early return, a RUNNING+AUTO_START+NTP box
  # with a benign (non-real) Source fell through into the latent check and was wrongly FAILed even
  # though it is not, right now, syncing from anywhere real.
  if w32time_running "$state"; then
    if [ -z "$reg_type" ]; then
      printf 'UNKNOWN: W32Time is RUNNING but its Type registry value is unreadable -- cannot certify it is inert\n'
      return 0
    fi
    if w32time_syncing_type "$reg_type" && w32time_source_is_real "$source"; then
      printf 'FAIL: W32Time is RUNNING as an active %s client syncing to "%s" -- a 2nd clock authority fighting dantesync\n' \
        "$reg_type" "$source"
      return 0
    fi
    printf 'ok\n'
    return 0
  fi

  # Not running right now: the only remaining risk is LATENT -- it will resurrect as a competing
  # authority on the next reboot.
  if w32time_autostarts "$start_type" && w32time_syncing_type "$reg_type"; then
    printf 'FAIL: W32Time start type is %s with Type=%s -- it will resurrect as a %s client on the next reboot even though it is not running now\n' \
      "$start_type" "$reg_type" "$reg_type"
    return 0
  fi

  printf 'ok\n'
}

# w32time_verdict_class VERDICT -> "OK" | "BAD" | "UNKNOWN" from a w32time_daemon_verdict string.
# Mirrors dantesync-gate.sh's node_verdict combiner so a caller's flow reads consistently.
w32time_verdict_class() {
  case "$1" in
    ok) printf 'OK\n' ;;
    FAIL:*) printf 'BAD\n' ;;
    *) printf 'UNKNOWN\n' ;;
  esac
}

# --- text extraction from a gathered status blob (#598's offline fixture seam) ----------------
#
# The caller (the win-* MCP holder — ssh to Windows is denied, so this file cannot gather live
# itself) pre-fetches `w32time_gather_remote_snippet()`'s output into ONE combined text file per
# box and passes it to the gate via --win-status NAME=FILE (mirrors dantesync-gate.sh's own
# --win-status convention exactly). These parsers pull each field back out of that combined text,
# so the SAME fixture-file path both drives production (live win-* MCP output) and the unit tests
# (a hand-written fixture file) with zero divergent code — the #608 offline-fixture-seam pattern.

# w32time_state_from_text TEXT -> the trimmed `sc query w32time` STATE word, "" if the STATE line
# is absent/unparseable. Real format (live-probed on strih/stream, 2026-07-08):
#   STATE              : 1  STOPPED
w32time_state_from_text() {
  printf '%s\n' "$1" \
    | grep -oE 'STATE[[:space:]]*:[[:space:]]*[0-9]+[[:space:]]+[A-Z_]+' \
    | sed -n 's/.*STATE[[:space:]]*:[[:space:]]*[0-9]\{1,\}[[:space:]]\{1,\}\([A-Z_]\{1,\}\).*/\1/p' \
    | tail -1 || true
}

# w32time_start_type_from_text TEXT -> the trimmed `sc qc w32time` START_TYPE word, "" if
# absent/unparseable. Real format (live-probed, 2026-07-08):
#   START_TYPE         : 4   DISABLED
w32time_start_type_from_text() {
  printf '%s\n' "$1" \
    | grep -oE 'START_TYPE[[:space:]]*:[[:space:]]*[0-9]+[[:space:]]+[A-Z_()]+' \
    | sed -n 's/.*START_TYPE[[:space:]]*:[[:space:]]*[0-9]\{1,\}[[:space:]]\{1,\}\([A-Z_()]\{1,\}\).*/\1/p' \
    | tail -1 || true
}

# w32time_reg_type_from_text TEXT -> the W32Time\Parameters\Type registry value, "" if
# absent/unparseable. Real format (live-probed, 2026-07-08):
#   Type    REG_SZ    NoSync
w32time_reg_type_from_text() {
  printf '%s\n' "$1" \
    | grep -oE '^[[:space:]]*Type[[:space:]]+REG_SZ[[:space:]]+[A-Za-z0-9]+' \
    | sed -n 's/.*REG_SZ[[:space:]]\{1,\}\([A-Za-z0-9]\{1,\}\).*/\1/p' \
    | tail -1 || true
}

# w32time_source_from_text TEXT -> the `w32tm /query /status` "Source:" line value, "" if absent
# (e.g. the service is stopped, so the command errors and prints no Source line at all — as
# live-probed on both strih and stream, 2026-07-08).
w32time_source_from_text() {
  printf '%s\n' "$1" \
    | grep -E '^Source:' \
    | sed -n 's/^Source:[[:space:]]*//p' \
    | tail -1 || true
}

# w32time_gather_remote_snippet -> the REMOTE shell command (a string) that gathers a Windows
# box's W32Time state into the combined text block the parsers above expect. Purely READ-ONLY --
# it never starts/stops/reconfigures W32Time; purging or disabling a drifted box is a separate,
# deliberate manual op, not this gate's job. `sc query`/`sc qc` are piped through `cmd /c` +
# `Out-String -Width 300` because the bare PowerShell-native invocation of the legacy `sc.exe`
# console tool returned EMPTY output over the win-* MCP Shell in a live 2026-07-08 probe of both
# strih and stream — routing it through cmd.exe's own console captured it correctly on both boxes.
w32time_gather_remote_snippet() {
  cat <<'REMOTE'
cmd /c "sc query w32time" 2>&1 | Out-String -Width 300
cmd /c "sc qc w32time" 2>&1 | Out-String -Width 300
reg query "HKLM\SYSTEM\CurrentControlSet\Services\W32Time\Parameters" /v Type 2>&1 | Out-String -Width 300
w32tm /query /status 2>&1
REMOTE
}
