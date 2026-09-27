#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure verdict functions + three path pins, no other top-level statements) -- the sourcing gate owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/version-integrity-obs-identity.sh -- the issue-826 strih OBS-identity verdict family of
# scripts/version-integrity-gate.sh, moved out of the gate VERBATIM (issue 1377: the gate was over the
# repo's 1000-line file budget). No behaviour change: the gate sources this lib BEFORE its
# source-guard, exactly where these functions used to sit, so sourcing the gate (the unit tests in
# tests/version_integrity_gate.rs) still defines every function, and the gate's main() still reads the
# three DEFAULT_* pins below. The only additions to the moved text are the three
# `shellcheck disable=SC2034` lines: the pins are read by the sourcing gate, not in this file.

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

# shellcheck disable=SC2034  # read by the sourcing gate's main(), not in this lib
DEFAULT_OBS_INSTALL_EXE='C:\Program Files\obs-studio\bin\64bit\obs64.exe'
# shellcheck disable=SC2034  # read by the sourcing gate's main(), not in this lib
DEFAULT_OBS_INSTALL_WORKDIR='C:\Program Files\obs-studio\bin\64bit'
# shellcheck disable=SC2034  # read by the sourcing gate's main(), not in this lib
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
