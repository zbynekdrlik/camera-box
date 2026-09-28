#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure verdict functions + three path pins + the row function, no other top-level statements) -- the sourcing gate owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/version-integrity-obs-identity.sh -- the issue-826 strih OBS-identity verdict family of
# scripts/version-integrity-gate.sh, moved out of the gate VERBATIM (issue 1377: the gate was over the
# repo's 1000-line file budget). No behaviour change: the gate sources this lib BEFORE its
# source-guard, exactly where these functions used to sit, so sourcing the gate (the unit tests in
# tests/version_integrity_gate.rs) still defines every function. Issue 1384 moved the facet's ROW
# block out of the gate's main() too (vig_row_obs_identity, at the end of this file), so the three
# DEFAULT_* pins below are now read here.
# The moved verdict comments were written inside the gate: their "`main` below" is the gate's main()
# (its per-box loop now calls vig_row_obs_identity), and the facet they call OPT-IN has since been
# ENFORCED (issue 829; port4455_identity last, in issue 1067) -- vig_row_obs_identity is the
# authority on how each verdict is wired.

# --- #826: strih OBS-identity machine-check facet — PURE verdict functions -------------------
#
# The 2026-07-27 incident: a hand-launched stale `1ME` OBS 31.1.2 install squatted TCP :4455 while
# this gate's own parity marker still described the pinned genlock 32.1.2 build -- the harness
# silently drove/measured the WRONG renderer for a whole gate cycle (issue #826, retitled after
# investigation). These four verdicts consume the facts scripts/bundle_state_gather.py's
# `build_bundle_state` now gathers (obs_installs, port4455_owner_path/_version,
# obs_process_count, ahk_app1_shortcut_path/_run/_dead_config_present, shortcut_target_path/
# _workdir) and are wired into `main` below as an OPT-IN per-box/per-key facet -- exactly like
# `genlock_build_sha`'s original #756 landing -- so every existing fixture and the live fleet keep
# gating exactly as today until the supervisor redeploys bundle-state-server.py with this facet.

# The three pins vig_row_obs_identity (end of this file) grades each box against.
DEFAULT_OBS_INSTALL_EXE='C:\Program Files\obs-studio\bin\64bit\obs64.exe'
DEFAULT_OBS_INSTALL_WORKDIR='C:\Program Files\obs-studio\bin\64bit'
DEFAULT_STARTUP_SHORTCUT='C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk'

# obs_installs_verdict PINNED_EXE INSTALLS_CSV -> acceptance #1: exactly ONE launchable OBS install
# may exist on the box (the pinned genlock build). Any OTHER obs*.exe/*ME.exe path found -- INCLUDING
# one sitting in a `_RETIRED_*` folder, renaming aside is not removing -- is DRIFT, named explicitly.
# INSTALLS_CSV empty -> UNKNOWN (the scan itself did not run, or found nothing at all -- not even
# the pinned one -- never a false "clean").
obs_installs_verdict() {
  local pinned="$1" csv="$2"
  if [ -z "$csv" ]; then
    printf '  %-22s UNKNOWN  (no obs_installs reported -- install scan unread)\n' "obs_installs"
    return 11
  fi
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a paths=($csv)
  IFS="$OLDIFS"
  local -a extras=()
  local found_pinned=0 p
  for p in "${paths[@]}"; do
    if [ "${p,,}" = "${pinned,,}" ]; then
      found_pinned=1
    else
      extras+=("$p")
    fi
  done
  if [ "${#extras[@]}" -gt 0 ] || [ "$found_pinned" -eq 0 ]; then
    local missing_note=""
    [ "$found_pinned" -eq 0 ] && missing_note="; the pinned genlock build itself was NOT found"
    printf '  %-22s DRIFT    (expected exactly ONE launchable OBS install (%s); found extra/other: %s%s)\n' \
      "obs_installs" "$pinned" "${extras[*]:-<none>}" "$missing_note"
    return 20
  fi
  printf '  %-22s OK       (exactly one launchable OBS install: %s)\n' "obs_installs" "$pinned"
  return 0
}

# port_identity_verdict PINNED_EXE PINNED_VERSION OWNER_PATH OWNER_VERSION -> acceptance #2: the
# process owning TCP :4455 must BE the pinned install, matched by PATH (never just process name --
# the exact hole the 2026-07-27 incident exposed: OBS 31.1.2 squatted the port while a same-named
# `obs64.exe` process was assumed to be the genlock build), and its version must match the pin.
# Empty OWNER_PATH -> UNKNOWN (unread, never assumed clean).
port_identity_verdict() {
  local pinned_exe="$1" pinned_ver="$2" owner_path="$3" owner_ver="$4"
  if [ -z "$owner_path" ]; then
    printf '  %-22s UNKNOWN  (port :4455 owner unread)\n' "port4455_identity"
    return 11
  fi
  if [ "${owner_path,,}" != "${pinned_exe,,}" ]; then
    printf '  %-22s DRIFT    (:4455 is owned by %s, expected the pinned genlock install %s -- the harness would drive/measure the WRONG OBS)\n' \
      "port4455_identity" "$owner_path" "$pinned_exe"
    return 20
  fi
  if [ -n "$pinned_ver" ] && [ -n "$owner_ver" ] && [ "$owner_ver" != "$pinned_ver" ]; then
    printf '  %-22s DRIFT    (:4455 owner %s reports version %s, expected pinned %s)\n' \
      "port4455_identity" "$owner_path" "$owner_ver" "$pinned_ver"
    return 20
  fi
  printf '  %-22s OK       (:4455 owned by the pinned install %s, version %s)\n' \
    "port4455_identity" "$owner_path" "${owner_ver:-$pinned_ver}"
  return 0
}

# obs_process_count_verdict COUNT -> acceptance #3: exactly ONE OBS-class process may be running --
# zero (not up at all) or 2+ (a second install alive alongside the genlock one) are both DRIFT.
# Empty COUNT -> UNKNOWN (unread).
obs_process_count_verdict() {
  local count="$1"
  if [ -z "$count" ]; then
    printf '  %-22s UNKNOWN  (OBS process count unread)\n' "obs_process_count"
    return 11
  fi
  if [ "$count" != "1" ]; then
    printf '  %-22s DRIFT    (%s OBS-class process(es) running, expected exactly 1)\n' "obs_process_count" "$count"
    return 20
  fi
  printf '  %-22s OK       (exactly 1 OBS-class process running)\n' "obs_process_count"
  return 0
}

# startup_chain_verdict PINNED_EXE PINNED_WORKDIR PINNED_SHORTCUT AHK_APP1_SHORTCUT AHK_APP1_RUN
#   AHK_DEAD_CONFIG SHORTCUT_TARGET SHORTCUT_WORKDIR
# -> acceptance #4 (NL_STARTUP.ahk app1 + the Start Menu shortcut both resolve to the pinned
# install, with the pinned working directory) PLUS the issue's "config states one truth" cleanup
# requirement (AHK_DEAD_CONFIG="1" -> the dead app1_binarypath leftover or an enabled app2_* block
# is still present -- itself a DRIFT, even when app1 otherwise resolves correctly). Any of
# AHK_APP1_SHORTCUT / SHORTCUT_TARGET / SHORTCUT_WORKDIR empty -> UNKNOWN (unread) -- the CALLER
# only invokes this at all for a box that reported ahk_app1_shortcut_path in the first place (only
# strih runs NL_STARTUP.ahk; a box with none of these keys never engages this facet, see main()).
startup_chain_verdict() {
  local pinned_exe="$1" pinned_workdir="$2" pinned_shortcut="$3"
  local ahk_shortcut="$4" ahk_run="$5" ahk_dead="$6"
  local shortcut_target="$7" shortcut_workdir="$8"

  if [ -z "$ahk_shortcut" ] || [ -z "$shortcut_target" ] || [ -z "$shortcut_workdir" ]; then
    printf '  %-22s UNKNOWN  (startup chain unread: NL_STARTUP.ahk / Start Menu shortcut not gathered)\n' "startup_chain"
    return 11
  fi

  local -a problems=()
  [ "$ahk_run" != "1" ] && problems+=("app1_run is not enabled (=${ahk_run:-<unread>})")
  [ "${ahk_shortcut,,}" != "${pinned_shortcut,,}" ] && problems+=("app1_path points at ${ahk_shortcut}, expected the Start Menu shortcut ${pinned_shortcut}")
  [ "${shortcut_target,,}" != "${pinned_exe,,}" ] && problems+=("the Start Menu shortcut resolves to ${shortcut_target}, expected ${pinned_exe}")
  [ "${shortcut_workdir,,}" != "${pinned_workdir,,}" ] && problems+=("the Start Menu shortcut's working directory is ${shortcut_workdir}, expected ${pinned_workdir}")
  [ "$ahk_dead" = "1" ] && problems+=("NL_STARTUP.ahk still carries the dead app1_binarypath / enabled app2_* leftover -- remove it so the config states one truth")

  if [ "${#problems[@]}" -gt 0 ]; then
    local joined
    joined="$(IFS='; '; echo "${problems[*]}")"
    printf '  %-22s DRIFT    (%s)\n' "startup_chain" "$joined"
    return 20
  fi
  printf '  %-22s OK       (NL_STARTUP.ahk app1 + Start Menu shortcut both resolve to the pinned install, workdir %s)\n' "startup_chain" "$pinned_workdir"
  return 0
}

# vig_row_obs_identity NAME FILE README IS_STRIH_LINUX -> the issue-826 OBS-identity rows of one
# --win-state box (obs_installs, port4455_identity, obs_process_count, and startup_chain on the
# box named strih), each SKIPPED on a Linux strih (issue 1351). Moved verbatim out of the gate's
# main() (issue 1384); it reads the DEFAULT_* pins above and the gate's state_json_value +
# drift-guard's pinned_obs_version (both defined by the time main() runs).
# It follows the vig_row_* contract in the header of scripts/lib/version-integrity-rows.sh.
vig_row_obs_identity() {
  local name="$1" file="$2" readme="$3" is_strih_linux="$4"
  local engine_out=""
  # #826 OBS-identity machine-check facet, ENFORCED fleet-wide (#829, the 758-style second step
  # after the 756-style opt-in landing): the generic install + process-count checks run on EVERY
  # box UNCONDITIONALLY -- an un-upgraded / absent box is a real gate-blocking UNKNOWN, no longer
  # a silent skip. #1067: port4455_identity is now ALSO enforced (its former opt-in guard is
  # removed below) -- the bundle-state-server gather context was fixed (WMI
  # Win32_Process.ExecutablePath, readable from the non-elevated task where the OpenProcess-based
  # Get-Process.Path was access-denied on the elevated OBS), so every box reports the :4455 owner
  # path now; an unreported owner is a REAL gate-blocking UNKNOWN. This completes the 756 -> 758
  # two-step for the last obs-identity facet.
  local obs_installs_csv port4455_owner_path port4455_owner_ver obs_proc_count
  obs_installs_csv="$(state_json_value "$file" obs_installs)"
  port4455_owner_path="$(state_json_value "$file" port4455_owner_path)"
  port4455_owner_ver="$(state_json_value "$file" port4455_owner_version)"
  obs_proc_count="$(state_json_value "$file" obs_process_count)"
  local frc=0

  # issue 1351 follow-up: on a Linux strih (--strih-linux, box named "strih") every #826
  # OBS-identity facet below is a Windows-only machine check (install-path scan, :4455 owner
  # exe, process count via tasklist, NL_STARTUP.ahk) -- SKIP each one loudly (counted ok, never
  # UNKNOWN) instead of running it. Windows strih/stream are byte-identical (unaffected).
  if [ "$is_strih_linux" = 1 ]; then
    printf '  %-22s SKIPPED  (strih is the Linux notebook -- #826 Windows OBS-identity facet not applicable, issue 1351)\n' "obs_installs"
    ok=$((ok + 1))
  else
    engine_out="$(obs_installs_verdict "$DEFAULT_OBS_INSTALL_EXE" "$obs_installs_csv")" || frc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$frc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:obs_installs") ;;
    esac
  fi

  # port4455_identity: ENFORCED fleet-wide (#1067, the 758-style second step) -- runs
  # UNCONDITIONALLY on every box now, exactly like obs_installs / obs_process_count above. Its
  # former opt-in `if [ -n "$port4455_owner_path" ]` guard is gone: the gather context was fixed
  # (WMI Win32_Process.ExecutablePath), so an EMPTY owner path is now a real gate-blocking UNKNOWN
  # (the verdict function returns 11 for an empty owner), never a silent skip.
  local pinned_obs_ver=""
  pinned_obs_ver="$(pinned_obs_version "$readme" 2>/dev/null)" || pinned_obs_ver=""
  frc=0
  if [ "$is_strih_linux" = 1 ]; then
    printf '  %-22s SKIPPED  (strih is the Linux notebook -- #826 Windows OBS-identity facet not applicable, issue 1351)\n' "port4455_identity"
    ok=$((ok + 1))
  else
    engine_out="$(port_identity_verdict "$DEFAULT_OBS_INSTALL_EXE" "$pinned_obs_ver" "$port4455_owner_path" "$port4455_owner_ver")" || frc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$frc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:port4455_identity") ;;
    esac
  fi

  frc=0
  if [ "$is_strih_linux" = 1 ]; then
    printf '  %-22s SKIPPED  (strih is the Linux notebook -- #826 Windows OBS-identity facet not applicable, issue 1351)\n' "obs_process_count"
    ok=$((ok + 1))
  else
    engine_out="$(obs_process_count_verdict "$obs_proc_count")" || frc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$frc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:obs_process_count") ;;
    esac
  fi

  # #826 — startup-chain facet, ENFORCED but strih-scoped (#829): strih MUST run NL_STARTUP.ahk,
  # so it now runs UNCONDITIONALLY on strih -- an unreported chain is a gate-blocking UNKNOWN
  # (unread), never a silent skip. Re-keyed from ahk-presence to the box identity so a strih box
  # that stops reporting the ahk keys can no longer silently drop the check. stream runs no
  # NL_STARTUP.ahk (per .claude/skills/obs-ops), so it NEVER engages here -- absent ahk on stream
  # stays OK, not UNKNOWN.
  if [ "$name" = "strih" ]; then
    if [ "$is_strih_linux" = 1 ]; then
      printf '  %-22s SKIPPED  (strih is the Linux notebook -- no NL_STARTUP.ahk startup chain, issue 1351)\n' "startup_chain"
      ok=$((ok + 1))
    else
      local ahk_shortcut ahk_run ahk_dead shortcut_target shortcut_workdir
      ahk_shortcut="$(state_json_value "$file" ahk_app1_shortcut_path)"
      ahk_run="$(state_json_value "$file" ahk_app1_run)"
      ahk_dead="$(state_json_value "$file" ahk_dead_config_present)"
      shortcut_target="$(state_json_value "$file" shortcut_target_path)"
      shortcut_workdir="$(state_json_value "$file" shortcut_workdir)"
      local frc2=0
      engine_out="$(startup_chain_verdict "$DEFAULT_OBS_INSTALL_EXE" "$DEFAULT_OBS_INSTALL_WORKDIR" "$DEFAULT_STARTUP_SHORTCUT" \
        "$ahk_shortcut" "$ahk_run" "$ahk_dead" "$shortcut_target" "$shortcut_workdir")" || frc2=$?
      printf '%s\n' "$engine_out" | sed 's/^/    /'
      case "$frc2" in
        0)  ok=$((ok + 1)) ;;
        20) bad=$((bad + 1)) ;;
        11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:startup_chain") ;;
      esac
    fi
  fi
  return 0
}
