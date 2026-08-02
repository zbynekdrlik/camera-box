#!/usr/bin/env bash
#
# version-integrity-gate.sh — the pre-rig-test VERSION-INTEGRITY precondition gate (#123, EPIC #125).
#
# WHY THIS GATE EXISTS (the user's hard requirement, "we can't dev/test on randomly-deployed
# versions"): every rig test (recording-e2e, loopback, the obs phase scripts) measures the
# behaviour of the LIVE strih+stream OBS stack. Those results are ONLY trustworthy when the live
# stack matches the pinned zero-loss SHA set (the versions + critical settings in vendor/README.md
# AND, when a bundle manifest is supplied, the per-component/whole-bundle BUILD SHAs). A test run on
# a drifted / randomly-deployed / STOCK-OBS build is worthless and actively misleading (that is #119:
# a wrong-bytes-right-version build silently shipped and every "it works" claim was false). So this
# gate runs FIRST — ALONGSIDE the DanteSync NTP+PTP gate (#7) — and REFUSES (exits non-zero) on
# DRIFT or UNKNOWN, so the rig is never brought up and no result is trusted on an unverified stack.
#
# It REUSES the unit-tested deterministic engine scripts/drift-guard.sh --compare (tested in
# tests/drift_guard.rs) — it does NOT reinvent any comparison. This script is the FLOW that gathers
# each box's observed stack state and runs the engine per box, then rolls the verdicts up.
#
# BOX ACCESS (this rig): the Windows OBS boxes (strih 10.77.9.202, stream 10.77.9.204) DENY ssh/scp,
# so this script cannot read their live OBS state itself. Mirroring dantesync-gate.sh's --win-status:
# the caller (the autopilot worker / operator, who HAS the win-* MCP) gathers each box's observed
# drift-guard values (the SAME read-only PowerShell reads /drift-guard step 1 does) into a flat JSON
# state file and passes it via --win-state NAME=FILE. recording-e2e.sh (and the other rig-test entry
# scripts) try to FETCH each box's state JSON over the box's standing http.server first
# (fetch_box_state, mirroring fetch_dante_status), falling back to the caller-pre-fetched file. A box
# with NO state file is UNKNOWN -> the gate REFUSES (never a silent pass with the box unverified).
# (dantesync-gate.sh's OWN DanteSync gate no longer uses this file-relay pattern for strih/stream
# — it queries them LIVE over HTTP via --win-http, #648 — but this version-integrity gate still
# does; #123/#119 is unrelated, separate scope.)
#
# State file = a flat JSON object of the drift-guard --compare observed keys for that box, e.g.
#   { "obs_version":"32.1.2", "distroav_version":"6.2.1", "ndi_runtime":"6.3.2.0",
#     "output_fps":"30", "genlock_wall_clock":"1",
#     "ndi_input_latency":"NDI cam5=0,NDI cam1=0,NDI cam3=0",
#     "distroav_dll_paths":"C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit\\distroav.dll",
#     "genlock_capability":"…the live genlock marker text…",
#     "manifest":"./gbundle/BUNDLE_MANIFEST.json", "obs_dll_sha256":"…", "distroav_dll_sha256":"…",
#     "bundle_hashes":"relpath=sha,…" }
# Every key is OPTIONAL; any drift-guard key you omit the engine reports UNKNOWN (so the gate refuses)
# — exactly the never-false-clean discipline drift-guard already enforces.
#
# Usage:
#   version-integrity-gate.sh [--readme PATH] [--manifest PATH] \
#       --win-state strih=/tmp/strih-state.json [--win-state stream=/tmp/stream-state.json] ...
#   version-integrity-gate.sh --help
#
# Exit codes: 0 = every box matches the pinned set (rig test may proceed),
#   20 = at least one box DRIFTED (a setting/version/SHA differs — run REFUSED),
#   11 = at least one box UNKNOWN (state unread / a value the engine could not read — incomplete,
#        NOT clean),
#   1  = usage / environment error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIFT_GUARD="$HERE/drift-guard.sh"
DEFAULT_README="vendor/README.md"

# --- PURE function (no network, no MCP — unit-tested) ---------------------------------------

# compare_args_from_state FILE -> one `key=val` line per drift-guard --compare observed key found in
# the flat JSON state object FILE. Pure text parse (no jq — drift-guard itself parses without jq, and
# the windows-2022 git-bash runner has no jq): the state is a flat `{"key":"val", …}` object on one
# or more lines; each "key":"value" pair becomes `key=value`. JSON string escapes \\ -> \ and \" -> "
# are unescaped so the Windows backslash path and any quote survive. Values may contain spaces, '='
# and ',' (ndi_input_latency = `NDI cam5=0,NDI cam1=0`), so the key/value split is on the JSON
# structure (`"key": "value"`), never on those characters. Only the keys drift-guard accepts are
# emitted; an unknown key in the state is skipped (the engine would WARN-ignore it anyway).
compare_args_from_state() {
  local file="$1"
  [ -f "$file" ] || { echo "compare_args_from_state: no such file: $file" >&2; return 1; }
  # grep every "key": "value" pair (value may contain escaped \" — match up to an UNescaped closing
  # quote: a run of (non-quote | backslash-quote) chars). One pair per output line via grep -o.
  # The known drift-guard --compare observed keys (host is added by the gate, not from the state):
  local keys='obs_version|distroav_version|ndi_runtime|output_fps|genlock_wall_clock|ndi_input_latency|distroav_dll_paths|manifest|obs_dll_sha256|distroav_dll_sha256|genlock_capability|bundle_hashes'
  # Match "<key>" : "<value>" where <value> is any run of (escaped char | non-backslash-non-quote).
  grep -oE "\"(${keys})\"[[:space:]]*:[[:space:]]*\"(\\\\.|[^\"\\\\])*\"" "$file" 2>/dev/null \
  | while IFS= read -r pair; do
      [ -z "$pair" ] && continue
      # key = the first quoted token; val = the second quoted token's contents.
      local key val
      key="$(printf '%s' "$pair" | sed -E 's/^"([^"]*)".*/\1/')"
      # Strip everything up to and including the colon + opening quote, and the trailing quote.
      val="$(printf '%s' "$pair" | sed -E 's/^"[^"]*"[[:space:]]*:[[:space:]]*"(.*)"$/\1/')"
      # Unescape JSON string escapes that matter for these values: \\ -> \ and \" -> ".
      val="${val//\\\\/\\}"
      val="${val//\\\"/\"}"
      printf '%s=%s\n' "$key" "$val"
    done
}

# state_json_value FILE KEY -> the string value of "KEY":"<value>" in the flat JSON state object
# FILE, or "" if absent/unreadable/no such file. #826 — generalized out of what used to be the
# #756-only `genlock_build_sha_from_state` (behavior-preserving refactor: every existing caller/test
# of that name keeps working, now implemented as a one-line call here) so every single-key facet
# added since (obs_installs, port4455_owner_path, ...) reuses ONE tolerant parser instead of a new
# copy-pasted grep|sed each time. Same tolerant flat-JSON parse as compare_args_from_state: match
# `"KEY": "<value>"`, unescape \\ -> \ and \" -> ", take the first match only.
state_json_value() {
  local file="$1" key="$2"
  [ -f "$file" ] || return 0
  local pair val
  pair="$(grep -oE "\"${key}\"[[:space:]]*:[[:space:]]*\"(\\\\.|[^\"\\\\])*\"" "$file" 2>/dev/null | head -1)"
  [ -z "$pair" ] && return 0
  val="$(printf '%s' "$pair" | sed -E 's/^"[^"]*"[[:space:]]*:[[:space:]]*"(.*)"$/\1/')"
  val="${val//\\\\/\\}"
  val="${val//\\\"/\"}"
  printf '%s' "$val"
}

# genlock_build_sha_from_state FILE -> the #756 `genlock_build_sha` value from the flat JSON state
# object FILE, or "" if absent/unreadable. This key is NOT a drift-guard --compare key (it is not
# emitted by compare_args_from_state above and never fed to drift-guard --compare) — it is read
# separately here and handed to the CROSS-BOX parity engine (genlock_build_parity_report). "" when
# the box's state has no such key yet -- ENFORCED (#758): the parity engine's OWN "<2 read SHAs"
# branch now returns UNKNOWN (a real gate-blocking condition), so an un-upgraded/unread box's
# bundle-state-server is itself flagged, never silently skipped.
genlock_build_sha_from_state() {
  state_json_value "$1" genlock_build_sha
}

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

# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# The cross-box genlock-parity engine (#756) lives in drift-guard.sh; source it so the FLOW below
# can call genlock_build_parity_report directly (drift-guard's own source-guard returns before its
# main, so this pulls in its pure functions only). Sourced AFTER our source-guard, so the gate's own
# unit tests (which source THIS file for its pure parsers) never pull the engine in.
# shellcheck source=/dev/null
. "$DRIFT_GUARD"

# --- flow (executed only when run directly) ------------------------------------------------

usage() {
  cat <<EOF
version-integrity-gate.sh — pre-rig-test VERSION-INTEGRITY gate (#123, EPIC #125).

REFUSES to let a rig test run unless the LIVE strih+stream stack matches the pinned zero-loss SHA
set (vendor/README.md versions + settings, and the bundle BUILD SHAs when a manifest is supplied).
A test run on a randomly-deployed / drifted / stock-OBS build is worthless (#119) — so this gate
runs FIRST (alongside the DanteSync gate #7) and FAILS FAST on drift or an unverified box.

The Windows boxes deny ssh; the caller (win-* MCP holder) pre-fetches each box's observed stack
state into a flat JSON file (the drift-guard --compare observed keys) and passes it via --win-state.

Usage:
  version-integrity-gate.sh [--readme PATH] [--manifest PATH] --win-state NAME=FILE [...]

Options:
  --readme PATH     pinned-set source (default ${DEFAULT_README}); threaded to drift-guard --compare.
  --manifest PATH   the build-under-test BUNDLE_MANIFEST.json — when set, applied to every box that
                    does not already carry a manifest= in its state (activates the BUILD-SHA facet).
  --win-state N=FILE  a box N whose observed drift-guard stack state JSON the caller wrote to FILE
                    (this gate has no headless ssh gather of its own; the win-* MCP holder
                    pre-fetches it -- #701 proved plain scp/ssh reaches strih/stream, not migrated
                    here). Repeatable. A box with no
                    file is UNKNOWN -> the gate refuses.

Exit: 0 = every box matches the pinned set (proceed), 20 = a box DRIFTED (REFUSED),
11 = a box UNKNOWN/unread (INCOMPLETE, not clean), 1 = usage error.
EOF
}

main() {
  local readme="$DEFAULT_README" manifest=""
  local -a win_state=()
  # #756 — extra box genlock-build SHAs supplied directly (LABEL=SHA), for boxes not gated via
  # --win-state (imag-nb is SSH-reachable, so recording-e2e.sh reads its GENLOCK_BUILD_SHA.txt and
  # passes it here). Repeatable. Combined with the SHAs read out of each --win-state file for the
  # CROSS-BOX parity assert.
  local -a genlock_sha=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --readme)      shift; readme="${1:-}" ;;
      --manifest)    shift; manifest="${1:-}" ;;
      --win-state)   shift; win_state+=("${1:-}") ;;
      --genlock-sha) shift; genlock_sha+=("${1:-}") ;;
      -h|--help)    usage; exit 0 ;;
      --*)          echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)            echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if [ ! -x "$DRIFT_GUARD" ]; then
    echo "ERROR: drift-guard engine not found/executable: $DRIFT_GUARD" >&2
    exit 1
  fi
  if [ "${#win_state[@]}" -eq 0 ]; then
    echo "ERROR: no box to gate (no --win-state given)." >&2
    echo "The version-integrity gate cannot certify the stack with zero boxes — refusing to pass." >&2
    exit 1
  fi

  echo "== version-integrity-gate (#123): pre-rig-test — live strih+stream stack MUST match the pinned set =="
  echo "   pins from ${readme}; engine = drift-guard.sh --compare; a drifted/unverified box REFUSES the run"

  local bad=0 unknown=0 ok=0 entry name file rc
  local -a compare_args
  local -a unknown_boxes=()
  for entry in "${win_state[@]}"; do
    name="${entry%%=*}"; file="${entry#*=}"
    if [ -z "$file" ] || [ ! -s "$file" ]; then
      printf '  %-14s UNKNOWN  (no state file %s — win-* MCP fetch missing)\n' "$name" "${file:-<none>}"
      unknown=$((unknown + 1)); unknown_boxes+=("$name"); continue
    fi
    echo "  -- ${name} (${file}) --"
    # Build the drift-guard --compare arg vector from the box's observed state.
    compare_args=(--compare "host=${name}" --readme "$readme")
    local has_manifest=0 arg
    while IFS= read -r arg; do
      [ -z "$arg" ] && continue
      [ "${arg%%=*}" = "manifest" ] && has_manifest=1
      compare_args+=("$arg")
    done < <(compare_args_from_state "$file")
    # If a global --manifest was given and the box's state did not carry its own, apply it so the
    # BUILD-SHA / whole-bundle facet runs on every box uniformly.
    if [ "$has_manifest" -eq 0 ] && [ -n "$manifest" ]; then
      compare_args+=("manifest=${manifest}")
    fi
    rc=0
    # Capture the engine's exit code DIRECTLY (no pipe between drift-guard and the status read), THEN
    # indent the buffered output for display. The fail-closed property must NOT depend on `set -o
    # pipefail` staying enabled: a piped `exit 20` to `sed` would otherwise yield pipeline status 0,
    # the `||` would never fire, rc would stay 0, and a DRIFT would be miscounted as OK — a false pass.
    local engine_out=""
    engine_out="$("$DRIFT_GUARD" "${compare_args[@]}" 2>&1)" || rc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$rc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("$name") ;;
      *)  echo "    !! drift-guard exited ${rc} for ${name} (engine/usage error)" >&2; bad=$((bad + 1)) ;;
    esac

    # #826 — strih OBS-identity machine-check facet, OPT-IN per box (mirrors #756's original
    # landing): engage the generic install/port/process-count trio ONLY when this box's state
    # reports at least one of the three keys -- an un-upgraded bundle-state-server (the entire
    # fleet, until the supervisor redeploys it) is silently skipped here, never a false DRIFT/
    # UNKNOWN, exactly like genlock_build_sha before #758 enforced it fleet-wide.
    local obs_installs_csv port4455_owner_path port4455_owner_ver obs_proc_count
    obs_installs_csv="$(state_json_value "$file" obs_installs)"
    port4455_owner_path="$(state_json_value "$file" port4455_owner_path)"
    port4455_owner_ver="$(state_json_value "$file" port4455_owner_version)"
    obs_proc_count="$(state_json_value "$file" obs_process_count)"
    if [ -n "$obs_installs_csv" ] || [ -n "$port4455_owner_path" ] || [ -n "$obs_proc_count" ]; then
      local pinned_obs_ver=""
      pinned_obs_ver="$(pinned_obs_version "$readme" 2>/dev/null)" || pinned_obs_ver=""
      local frc=0

      engine_out="$(obs_installs_verdict "$DEFAULT_OBS_INSTALL_EXE" "$obs_installs_csv")" || frc=$?
      printf '%s\n' "$engine_out" | sed 's/^/    /'
      case "$frc" in
        0)  ok=$((ok + 1)) ;;
        20) bad=$((bad + 1)) ;;
        11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:obs_installs") ;;
      esac

      frc=0
      engine_out="$(port_identity_verdict "$DEFAULT_OBS_INSTALL_EXE" "$pinned_obs_ver" "$port4455_owner_path" "$port4455_owner_ver")" || frc=$?
      printf '%s\n' "$engine_out" | sed 's/^/    /'
      case "$frc" in
        0)  ok=$((ok + 1)) ;;
        20) bad=$((bad + 1)) ;;
        11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:port4455_identity") ;;
      esac

      frc=0
      engine_out="$(obs_process_count_verdict "$obs_proc_count")" || frc=$?
      printf '%s\n' "$engine_out" | sed 's/^/    /'
      case "$frc" in
        0)  ok=$((ok + 1)) ;;
        20) bad=$((bad + 1)) ;;
        11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:obs_process_count") ;;
      esac
    fi

    # #826 — startup-chain facet, scoped to boxes that actually run NL_STARTUP.ahk (only strih --
    # stream has none, per .claude/skills/obs-ops). A box with no ahk_app1_shortcut_path key at
    # all never engages this check; one that DOES report it is held fail-closed on every other
    # startup-chain fact (startup_chain_verdict's own UNKNOWN branch).
    local ahk_shortcut
    ahk_shortcut="$(state_json_value "$file" ahk_app1_shortcut_path)"
    if [ -n "$ahk_shortcut" ]; then
      local ahk_run ahk_dead shortcut_target shortcut_workdir
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
  done

  # #756 — CROSS-BOX genlock-build PARITY: every fleet box must run ONE deployed genlock build. This
  # catches the stale-imag skew the per-box origin/main ref-compare (drift-guard --check-imag) misses
  # during a long-lived dev train (#530/#756: imag ran a stale lineage, segfaulted, wedged the GPU).
  # Gather each box's live GENLOCK_BUILD_SHA.txt: from every --win-state box's state JSON (served by
  # its bundle-state-server) + every --genlock-sha LABEL=SHA supplied directly (imag, read over ssh
  # by recording-e2e.sh). ENFORCED (#758): the parity engine is fail-closed (an unread box, OR fewer
  # than 2 read peers, is UNKNOWN — a REAL gate-blocking condition, never a silent skip).
  local -a parity_args=()
  local ge gname gsha
  for entry in "${win_state[@]}"; do
    gname="${entry%%=*}"; file="${entry#*=}"
    gsha=""
    [ -n "$file" ] && [ -s "$file" ] && gsha="$(genlock_build_sha_from_state "$file")"
    parity_args+=("${gname}=${gsha}")
  done
  for ge in "${genlock_sha[@]}"; do
    parity_args+=("$ge")
  done
  # #756/#758 — ENFORCED (no longer opt-in/dormant, per the user's explicit escalation after
  # today's imag stale-build incident): the parity engine ALWAYS runs now, unconditionally — its
  # OWN "fewer than 2 read peers" branch already returns UNKNOWN (11), which this case statement
  # already treats as a gate-blocking condition exactly like every other facet's UNKNOWN. The old
  # `nonempty -ge 2` gate existed ONLY to skip calling the engine at all while the fleet's
  # bundle-state-servers were still being upgraded (#756 rollout) -- that rollout is complete
  # (strih+stream+imag all report genlock_build_sha as of 2026-07-14 ~21:40), so a box that fails
  # to report one now is itself a REAL, actionable gap (a stale/unread bundle-state-server), never
  # a reason to silently skip the whole facet.
  #
  # #949 — a Windows-only vendor/av-sync-dock/** change advances strih/stream's deployed
  # GENLOCK_BUILD_SHA.txt to a SHA imag's OWN build trigger (linux-genlock.yml, which deliberately
  # excludes vendor/av-sync-dock/**) can never be built at -- even though imag's actual built
  # bytes never changed. A raw-string mismatch is therefore NOT proof of a real skew by itself;
  # before handing the raw LABEL=SHA readings to the (still string-comparing) engine, resolve every
  # PAIR of boxes reporting a non-empty, DIFFERENT-string SHA into a real git content check, scoped
  # to the INTERSECTION of the two boxes' own consumed vendor paths (genlock_parity_consumed_paths)
  # -- an empty `git diff` there means the label mismatch is cosmetic, and an `EQUIV=labelA:labelB`
  # marker is appended so the engine treats that ONE pair as in parity. A pair whose diff is
  # NON-empty, or whose SHA cannot be resolved at all (fail-closed -- never a silent pass), gets NO
  # marker and still DRIFTs exactly as before #949. Boxes already byte-identical need no git call
  # at all (the engine's own fast path).
  local -a equiv_args=()
  local -a __ep_a=() __ep_b=()
  local pi pj la sa lb sb
  local any_mismatch=0
  for ((pi = 0; pi < ${#parity_args[@]}; pi++)); do
    for ((pj = pi + 1; pj < ${#parity_args[@]}; pj++)); do
      sa="${parity_args[$pi]#*=}"
      sb="${parity_args[$pj]#*=}"
      if [ -n "$sa" ] && [ -n "$sb" ] && [ "$sa" != "$sb" ]; then
        any_mismatch=1
      fi
    done
  done
  if [ "$any_mismatch" -eq 1 ]; then
    local repo_root=""
    repo_root="$(cd "$HERE/.." 2>/dev/null && pwd)" || repo_root=""
    if [ -z "$repo_root" ]; then
      echo "WARN: could not resolve version-integrity-gate.sh's own repo root -- skipping #949 genlock parity content-equivalence check (a label-only mismatch will DRIFT even if the content is identical)" >&2
    else
      timeout 15 git -C "$repo_root" fetch origin --quiet 2>/dev/null \
        || echo "WARN: git fetch origin failed (or timed out) -- #949 genlock parity content-check may see a stale origin (a genuinely new SHA may fail to resolve and DRIFT)" >&2
      local pth pb found_p
      for ((pi = 0; pi < ${#parity_args[@]}; pi++)); do
        for ((pj = pi + 1; pj < ${#parity_args[@]}; pj++)); do
          la="${parity_args[$pi]%%=*}"; sa="${parity_args[$pi]#*=}"
          lb="${parity_args[$pj]%%=*}"; sb="${parity_args[$pj]#*=}"
          [ -z "$sa" ] || [ -z "$sb" ] && continue
          [ "$sa" = "$sb" ] && continue
          __ep_a=()
          while IFS= read -r pth; do [ -n "$pth" ] && __ep_a+=("$pth"); done \
            < <(genlock_parity_consumed_paths "$la")
          __ep_b=()
          while IFS= read -r pth; do [ -n "$pth" ] && __ep_b+=("$pth"); done \
            < <(genlock_parity_consumed_paths "$lb")
          local -a inter=()
          for pth in "${__ep_a[@]}"; do
            found_p=0
            for pb in "${__ep_b[@]}"; do [ "$pb" = "$pth" ] && found_p=1 && break; done
            [ "$found_p" -eq 1 ] && inter+=("$pth")
          done
          if [ "${#inter[@]}" -gt 0 ] && genlock_parity_equivalent "$repo_root" "$sa" "$sb" "${inter[@]}"; then
            equiv_args+=("EQUIV=${la}:${lb}")
          fi
        done
      done
    fi
  fi
  echo "  -- cross-box genlock parity (#756/#949, ENFORCED) --"
  local prc=0 parity_out=""
  parity_out="$(genlock_build_parity_report "${parity_args[@]}" "${equiv_args[@]}")" || prc=$?
  printf '%s\n' "$parity_out" | sed 's/^/    /'
  case "$prc" in
    0)  ok=$((ok + 1)) ;;
    20) bad=$((bad + 1)) ;;
    11) unknown=$((unknown + 1)); unknown_boxes+=("genlock_parity") ;;
    *)  echo "    !! genlock_build_parity_report exited ${prc} (engine error)" >&2; bad=$((bad + 1)) ;;
  esac

  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} box(es) DRIFTED from the pinned zero-loss set — rig test REFUSED." >&2
    echo "!! A result on a randomly-deployed / drifted / stock build is worthless (#119). Restore the" >&2
    echo "!! pinned build (off-air + user-approved redeploy), re-verify with /drift-guard, then re-run." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further box(es) UNKNOWN: ${unknown_boxes[*]} — status also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN: ${unknown_boxes[*]} (state unread / a value not read) — NOT clean." >&2
    echo "!! Every box must report a complete observed stack before the rig test is trusted. (${ok} OK.)" >&2
    exit 11
  fi
  echo "GATE PASS — ${ok} box(es) match the pinned zero-loss set. The live stack is the build we expect; proceed."
  exit 0
}

main "$@"
