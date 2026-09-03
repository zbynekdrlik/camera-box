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
#   { "obs_version":"32.2.0", "distroav_version":"6.2.1", "ndi_runtime":"6.3.2.0",
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

# vendor_pin_range_log REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: prints `git log
# --format='%h %s' $(git merge-base DEPLOYED_SHA origin/main)..origin/main -- vendor/` (one
# vendor-touching commit per line origin/main carries that DEPLOYED_SHA's own lineage never
# received -- i.e. DEPLOYED_SHA is genuinely LAGGING relative to it); exit status mirrors the FIRST
# failing git call (the merge-base resolve, then the log). Mirrors drift-guard.sh's
# imag_genlock_range_log's LOGIC exactly (same #1292 merge-base fix, same `--end-of-options` defense
# against an unvalidated SHA value shaped like a git flag) -- two deliberate differences: `--format=
# '%h %s'` instead of `--oneline` (identical output shape, consistent with this file's own
# pre-existing `--format=` style elsewhere), and scoped to the WHOLE `vendor/` tree instead of
# just `vendor/obs-studio vendor/distroav`, because this facet covers every deployed box (strih,
# stream, imag), not only imag's own consumed paths (see genlock_parity_consumed_paths for that
# per-box distinction, which this facet deliberately does NOT apply -- it PINS every box's deployed
# SHA against the single newest vendor/** commit on origin/main, regardless of which sub-paths that
# box's own build actually consumes).
#
# #1292 root cause this exists to fix: the caller used to compute PENDING_LIST via a PLAIN ancestry
# range (`DEPLOYED_SHA..origin/main`), which reads LAGGING for a deployed SHA that is genuinely AHEAD
# of main on the dev candidate line -- this repo's two-branch workflow never merges main's own merge
# commits back into dev (top-level CLAUDE.md GOTCHA), so a deployed SHA's dev-side lineage is never a
# git-ancestor of main's merge commits even when it is a CONTENT superset of them. Scoping the range
# to the common ancestor (git merge-base) removes ONLY that false positive -- see
# vendor_pin_ahead_log/vendor_pin_on_dev immediately below for the AHEAD-direction classification,
# and genlock_vendor_pin_verdict for how the three combine into the report-only verdict. Isolated so
# it is independently testable against a throwaway synthetic repo (tests/version_integrity_gate.rs)
# -- no live git fetch needed.
vendor_pin_range_log() {
  local repo_root="$1" deployed="$2" base
  base="$(git -C "$repo_root" merge-base --end-of-options "$deployed" origin/main 2>/dev/null)" \
    || return $?
  git -C "$repo_root" log --format='%h %s' --end-of-options "${base}..origin/main" \
    -- vendor/ 2>/dev/null
}

# vendor_pin_ahead_log REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: the AHEAD-direction
# counterpart to vendor_pin_range_log -- prints `git log --format='%h %s'
# origin/main..DEPLOYED_SHA -- vendor/` (one vendor-touching commit per line DEPLOYED_SHA carries
# that origin/main does not). Mirrors drift-guard.sh's imag_genlock_ahead_log's logic (same
# `--end-of-options` defense, same explicit `-n` empty-SHA guard so an empty DEPLOYED_SHA fails LOUD
# instead of silently resolving `origin/main..` as `origin/main..HEAD`; same `--format=` vs
# `--oneline` style difference as vendor_pin_range_log above).
vendor_pin_ahead_log() {
  local repo_root="$1" deployed="$2"
  [ -n "$deployed" ] || return 128
  git -C "$repo_root" log --format='%h %s' --end-of-options "origin/main..${deployed}" \
    -- vendor/ 2>/dev/null
}

# vendor_pin_on_dev REPO_ROOT DEPLOYED_SHA -> #1292 review follow-up: exit 0 when DEPLOYED_SHA is
# reachable from origin/dev (a recognized release-candidate bundle deployed ahead of main),
# non-zero otherwise (unreachable, or DEPLOYED_SHA itself unresolvable) -- fail CLOSED, never a
# silent "yes" on an unresolvable check. Mirrors drift-guard.sh's imag_genlock_on_dev exactly. A
# deployed SHA that carries vendor commits reachable from NEITHER origin/main NOR origin/dev is an
# unrecognized/orphan build (early-gate-pin doctrine: "an orphan release must SCREAM"), never a
# quiet OK just because it happens to be a content superset of main.
vendor_pin_on_dev() {
  local repo_root="$1" deployed="$2"
  [ -n "$deployed" ] || return 128
  git -C "$repo_root" merge-base --is-ancestor --end-of-options "$deployed" origin/dev 2>/dev/null
}

# genlock_vendor_pin_verdict DEPLOYED_SHA NEWEST_VENDOR_SHA PENDING_LIST [AHEAD_LIST] [ON_DEV] ->
# #1137 REPORT-ONLY vendor-pin ALARM. The gate's only genlock check is CROSS-BOX PARITY
# (genlock_build_parity_report, #756/#949) -- it PASSES a UNIFORMLY-stale fleet where every box
# agrees on an OLD build (live: both boxes 03cd9c073 with 2 undeployed #1097 vendor commits). This
# layer PINS the fleet-deployed genlock_build_sha to the NEWEST origin/main commit touching
# vendor/** -- the missing PIN the .claude/rules/early-gate-pin-doctrine.md orphan class names
# ("peer parity is a SUPPLEMENT, never a substitute"):
#   DEPLOYED_SHA empty                             -> UNKNOWN (31): deployed SHA unread, fail-closed
#   NEWEST_VENDOR_SHA empty                        -> UNKNOWN (31): origin/main newest vendor/**
#                                                      commit unresolved, fail-closed
#   PENDING_LIST non-empty                         -> ALARM   (30): deployed bundle LAGS -- names
#                                                      every pending vendor commit
#   PENDING_LIST empty + AHEAD_LIST non-empty
#     + ON_DEV="1"                                 -> OK      (0):  deployed bundle is AHEAD of
#                                                      origin/main on the dev candidate line -- a
#                                                      recognized release-candidate build (#1292,
#                                                      mirrors drift-guard.sh's
#                                                      genlock_build_drift_report AHEAD branch)
#   PENDING_LIST empty + AHEAD_LIST non-empty
#     + ON_DEV!="1"                                -> ALARM   (30): ORPHAN -- vendor commits
#                                                      reachable from NEITHER origin/main NOR
#                                                      origin/dev
#   else (both PENDING_LIST and AHEAD_LIST empty)  -> OK      (0):  deployed bundle is at the
#                                                      newest vendored HEAD
# PENDING_LIST/AHEAD_LIST = newline-separated "<sha> <subject>" (main() computes them via
# vendor_pin_range_log/vendor_pin_ahead_log; empty = none). AHEAD_LIST/ON_DEV are #1292 additions,
# OPTIONAL (default "" / "0") so every pre-#1292 3-arg call site keeps its exact prior behavior for
# the LAGS/OK branches -- only the NEW ahead-but-empty-pending branch needs them.
#
# REPORT-ONLY by design, unchanged by #1292 (rc 30 for BOTH the LAGS and the ORPHAN reason): the
# vendored OBS bundle deploys via COORDINATED OBS restarts (not a hot swap), so a merged-but-not-yet-
# redeployed vendor commit is a normal transient during dev -- a hard block on every E2E would be
# "too blunt" (the doctrine's own word), so this component gets an ALARM, not a hard-gate, exactly
# like the dantesync canary lag (#1139). But it SCREAMS on every run and NAMES the pending/ahead
# commits, so an orphan can never sit silently "discovered by eye weeks later" (#1136 owner
# directive) -- and, since #1292, a box that is legitimately ahead on the dev candidate line no
# longer false-ALARMs at all. It prints its verdict to STDOUT (tests capture it); main() adds a
# stderr SCREAM banner on ALARM/UNKNOWN and NEVER folds it into the gate's bad/unknown counters
# (that is what keeps it report-only). The documented two-step upgrade to a hard-gate is: once the
# vendored bundle is folded into an auto-deploy that advances with origin/main (the camera-box
# orphan-PROOF shape), flip the ALARM rows into the gate's bad/unknown roll-up.
genlock_vendor_pin_verdict() {
  local deployed="$1" newest="$2" pending="$3" ahead="${4:-}" on_dev="${5:-0}"
  if [ -z "$deployed" ]; then
    printf '  %-22s UNKNOWN  (deployed genlock_build_sha unread -- vendor pin unverifiable, fail-closed)\n' "vendor_pin"
    return 31
  fi
  if [ -z "$newest" ]; then
    printf '  %-22s UNKNOWN  (origin/main newest vendor/** commit unresolved for %s -- vendor pin unverifiable, fail-closed)\n' "vendor_pin" "$deployed"
    return 31
  fi
  local cleaned n
  cleaned="$(printf '%s\n' "$pending" | sed '/^[[:space:]]*$/d')"
  if [ -n "$cleaned" ]; then
    n="$(printf '%s\n' "$cleaned" | wc -l | tr -d ' ')"
    printf '  %-22s ALARM    (deployed bundle %s LAGS origin/main vendor HEAD %s -- %s undeployed vendor commit(s), redeploy the fleet):\n' \
      "vendor_pin" "$deployed" "$newest" "$n"
    printf '%s\n' "$cleaned" | sed 's/^/                           - /'
    return 30
  fi
  local cleaned_ahead n_ahead
  cleaned_ahead="$(printf '%s\n' "$ahead" | sed '/^[[:space:]]*$/d')"
  if [ -n "$cleaned_ahead" ]; then
    n_ahead="$(printf '%s\n' "$cleaned_ahead" | wc -l | tr -d ' ')"
    if [ "$on_dev" = "1" ]; then
      printf '  %-22s OK       (deployed bundle %s is %s vendored vendor/** commit(s) AHEAD of origin/main on the dev candidate line -- a recognized release-candidate build, #1292)\n' \
        "vendor_pin" "$deployed" "$n_ahead"
      return 0
    fi
    printf '  %-22s ALARM    (deployed bundle %s genlock ORPHAN -- reachable from NEITHER origin/main NOR origin/dev; it carries %s vendored vendor/** commit(s) beyond origin/main, redeploy the fleet -- if this is unexpected, confirm origin/dev is fetched in this checkout):\n' \
      "vendor_pin" "$deployed" "$n_ahead"
    printf '%s\n' "$cleaned_ahead" | sed 's/^/                           - /'
    return 30
  fi
  printf '  %-22s OK       (deployed bundle %s is at the newest origin/main vendor HEAD)\n' "vendor_pin" "$deployed"
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

# imag_bytes_verdict LABEL MANIFEST CSV -> #1082 imag .so BYTE parity. For each `path=sha` in CSV (the
# imag box's DEPLOYED libobs.so.30 / distroav.so / libobs-opengl.so.30 sha256s, gathered over ssh by
# recording-e2e.sh via scripts/lib/manifest-autosource.sh), resolve the AUTHORITATIVE sha for that
# EXACT manifest path via drift-guard's manifest_sha_for_path (the linux-.so resolver -- #122's
# manifest_sha_for_component knows only the Windows obs.dll/distroav.dll basenames) and compare. A
# TARGETED per-.so compare, NOT the whole-bundle drift_check_all_files walk, so a partial 3-file
# gather never flips the gate UNKNOWN for the ~1600 files it did not hash.
#
# ENFORCED (#758-shape, #1100): an absent CSV or MANIFEST is a gate-blocking UNKNOWN (returns 11), so
# a live gather/auto-source failure REFUSES the run rather than silently passing -- the live imag
# gather is deployed + verified on the rig (imag_so_bytes OK on a green E2E, obs-genlock bundle at
# /usr/lib). Present + all match -> OK (0); any mismatch -> DRIFT (20)
# naming the .so + box; a path absent from the manifest -> UNKNOWN (11, never a false clean). Defined
# below the source-guard because it calls manifest_sha_for_path (drift-guard) -- tested end-to-end via
# the gate subprocess (tests/version_integrity_gate.rs), the same path the #770 byte facet uses.
imag_bytes_verdict() {
  local label="$1" manifest="$2" csv="$3"
  if [ -z "$csv" ] || [ -z "$manifest" ]; then
    printf '  %-22s UNKNOWN  (%s byte gather/manifest not supplied -- #1100 ENFORCED, every box must report its .so bytes)\n' "imag_so_bytes" "$label"
    return 11
  fi
  if [ ! -f "$manifest" ]; then
    printf '  %-22s UNKNOWN  (%s manifest %s not readable)\n' "imag_so_bytes" "$label" "$manifest"
    return 11
  fi
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  local entry path sha exp drift=0 unknown=0 ok=0 total=0
  for entry in "${entries[@]}"; do
    entry="${entry#"${entry%%[![:space:]]*}"}"; entry="${entry%"${entry##*[![:space:]]}"}"
    [ -z "$entry" ] && continue
    path="${entry%%=*}"; sha="${entry#*=}"
    total=$((total + 1))
    exp="$(manifest_sha_for_path "$manifest" "$path")"
    if [ -z "$exp" ]; then
      printf '  %-22s UNKNOWN  (%s: %s not listed in the manifest -- byte parity unverifiable)\n' "imag_so_bytes" "$label" "$path"
      unknown=$((unknown + 1))
    elif [ "$sha" = "$exp" ]; then
      printf '  %-22s OK       (%s: %s matches the manifest)\n' "imag_so_bytes" "$label" "${path##*/}"
      ok=$((ok + 1))
    else
      printf '  %-22s DRIFT    (%s: %s bytes differ -- expected %s, deployed %s)\n' "imag_so_bytes" "$label" "$path" "$exp" "$sha"
      drift=$((drift + 1))
    fi
  done
  if [ "$total" -eq 0 ]; then
    printf '  %-22s UNKNOWN  (%s byte CSV empty -- #1100 ENFORCED)\n' "imag_so_bytes" "$label"
    return 11
  fi
  [ "$drift" -gt 0 ] && return 20
  [ "$unknown" -gt 0 ] && return 11
  return 0
}

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
  --imag-manifest PATH  #1082 -- the CI-authoritative linux BUNDLE_MANIFEST.json for imag's build,
                    against which imag's DEPLOYED .so bytes are compared. ENFORCED (#1100).
  --imag-bytes LABEL=path=sha,...  #1082 -- imag's DEPLOYED libobs.so.30 / distroav.so /
                    libobs-opengl.so.30 sha256s (gathered over ssh; imag is not a --win-state box).
                    ENFORCED (#1100): absent -> the imag byte facet is UNKNOWN -> the gate refuses.
  --imag-acked-offline REASON  #1164 -- imag is physically absent and operator-acked offline
                    (rig-fleet.txt \`imag:REASON\`, issue 1013). SKIPS the imag .so byte facet (a
                    loud SKIPPED line, counted OK, never UNKNOWN) and drops any \`imag\`-labelled
                    --genlock-sha entry from the cross-box parity (which then certifies strih+stream).
                    WITHOUT this flag an absent imag is still fail-closed UNKNOWN (the #1100 default).

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
  # #1082 -- imag (Linux) .so BYTE parity: a linux BUNDLE_MANIFEST for imag's build + imag's DEPLOYED
  # .so sha256s (LABEL=path=sha,...), gathered over ssh (imag is NOT a --win-state bundle-state box).
  # Both ENFORCED (#758-shape, #1100): absent -> the facet is UNKNOWN -> the gate refuses.
  local imag_manifest="" imag_bytes=""
  # #1164 -- imag acked-offline (physically absent, operator-acked in rig-fleet.txt, issue 1013).
  # When set to the ack REASON, the imag .so byte facet is SKIPPED (a loud line, counted ok, never
  # UNKNOWN) and any --genlock-sha entry labelled exactly `imag` is dropped from the cross-box parity
  # (which then certifies the remaining fleet strih+stream). WITHOUT this flag the gate is
  # byte-identical to before -- an absent imag is still fail-closed UNKNOWN(11) (the #1100 contract).
  local imag_acked_offline=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --readme)             shift; readme="${1:-}" ;;
      --manifest)           shift; manifest="${1:-}" ;;
      --win-state)          shift; win_state+=("${1:-}") ;;
      --genlock-sha)        shift; genlock_sha+=("${1:-}") ;;
      --imag-manifest)      shift; imag_manifest="${1:-}" ;;
      --imag-bytes)         shift; imag_bytes="${1:-}" ;;
      --imag-acked-offline) shift; imag_acked_offline="${1:-}" ;;
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

    engine_out="$(obs_installs_verdict "$DEFAULT_OBS_INSTALL_EXE" "$obs_installs_csv")" || frc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$frc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("${name}:obs_installs") ;;
    esac

    # port4455_identity: ENFORCED fleet-wide (#1067, the 758-style second step) -- runs
    # UNCONDITIONALLY on every box now, exactly like obs_installs / obs_process_count above. Its
    # former opt-in `if [ -n "$port4455_owner_path" ]` guard is gone: the gather context was fixed
    # (WMI Win32_Process.ExecutablePath), so an EMPTY owner path is now a real gate-blocking UNKNOWN
    # (the verdict function returns 11 for an empty owner), never a silent skip.
    local pinned_obs_ver=""
    pinned_obs_ver="$(pinned_obs_version "$readme" 2>/dev/null)" || pinned_obs_ver=""
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

    # #826 — startup-chain facet, ENFORCED but strih-scoped (#829): strih MUST run NL_STARTUP.ahk,
    # so it now runs UNCONDITIONALLY on strih -- an unreported chain is a gate-blocking UNKNOWN
    # (unread), never a silent skip. Re-keyed from ahk-presence to the box identity so a strih box
    # that stops reporting the ahk keys can no longer silently drop the check. stream runs no
    # NL_STARTUP.ahk (per .claude/skills/obs-ops), so it NEVER engages here -- absent ahk on stream
    # stays OK, not UNKNOWN.
    if [ "$name" = "strih" ]; then
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
    # #1164 -- imag acked offline: drop its genlock-sha entry so the parity certifies the remaining
    # fleet (strih+stream) instead of UNKNOWN-refusing on the physically-absent, acked box. Defense
    # in depth -- the acked call site (recording-e2e.sh) already omits --genlock-sha imag=... entirely.
    if [ -n "$imag_acked_offline" ] && [ "${ge%%=*}" = "imag" ]; then continue; fi
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
  # at all (the engine's own fast path). Carries BOTH EQUIV= markers (pair proven content-
  # identical) and DIFF= markers (pair genuinely differs -- names the actual paths so the DRIFT
  # message stays actionable) -- genlock_build_parity_report tells them apart by prefix.
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
          if [ "${#inter[@]}" -eq 0 ]; then
            continue
          fi
          if genlock_parity_equivalent "$repo_root" "$sa" "$sb" "${inter[@]}"; then
            equiv_args+=("EQUIV=${la}:${lb}")
          else
            # #949: not equivalent (a real diff, or an unresolvable sha). Try to name the ACTUAL
            # differing paths so a genuine DRIFT is actionable, not just "two opaque SHAs differ" —
            # empty output here (unresolvable sha) simply means no DIFF= marker is added, and the
            # DRIFT message falls back to its pre-#949 wording.
            local diff_paths=""
            diff_paths="$(genlock_parity_diff_paths "$repo_root" "$sa" "$sb" "${inter[@]}" \
              | paste -sd, - 2>/dev/null || true)"
            if [ -n "$diff_paths" ]; then
              equiv_args+=("DIFF=${la}:${lb}:${diff_paths}")
            fi
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

  # #1082/#1100 -- imag (Linux) .so BYTE parity facet: compare imag's DEPLOYED libobs.so.30 /
  # distroav.so / libobs-opengl.so.30 sha256s (--imag-bytes, gathered over ssh) against the
  # CI-authoritative linux BUNDLE_MANIFEST for imag's build (--imag-manifest, auto-sourced per box by
  # recording-e2e.sh). This closes the byte-parity gap #770 left for imag (its bytes had NO path into
  # the gate -- only its marker). ENFORCED (#758-shape, #1100): the facet runs UNCONDITIONALLY and an
  # absent gather/manifest is a gate-blocking UNKNOWN (11), never the old silent DORMANT skip -- the
  # live imag gather is deployed + verified on the rig (imag_so_bytes OK on a green E2E). Same
  # 756->758 second step #1067 applied to port4455_identity. (The WINDOWS obs.dll/distroav.dll byte
  # enforcement -- removing recording-e2e.sh's manifest-autosource opt-in guard -- stays staged until
  # the bundle-state-server byte gather is redeployed to strih+stream; see #1100.)
  echo "  -- imag .so byte parity (#1082/#1100, enforced) --"
  if [ -n "$imag_acked_offline" ]; then
    # #1164 -- imag physically absent + operator-acked offline (rig-fleet.txt `imag:...`, issue 1013).
    # SKIP the .so byte facet with a LOUD, greppable line instead of the #1100 UNKNOWN(11) refuse --
    # counted ok (never unknown), never a silent pass (the whole imag leg is a NAMED partial this run,
    # exactly like every other imag_leg_skip_note site). The #1100 fail-closed default is untouched:
    # this branch runs ONLY when the operator explicitly acked imag offline.
    printf '  %-22s SKIPPED  (imag acked offline: %s -- issue-1013 leg skip; facet not judged)\n' \
      "imag_so_bytes" "$imag_acked_offline" | sed 's/^/    /'
    ok=$((ok + 1))
  else
    local ib_label="imag" ib_csv=""
    if [ -n "$imag_bytes" ]; then ib_label="${imag_bytes%%=*}"; ib_csv="${imag_bytes#*=}"; fi
    local ib_out="" ibrc=0
    ib_out="$(imag_bytes_verdict "${ib_label:-imag}" "$imag_manifest" "$ib_csv")" || ibrc=$?
    printf '%s\n' "$ib_out" | sed 's/^/    /'
    case "$ibrc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("imag:so_bytes") ;;
      *)  echo "    !! imag_bytes_verdict exited ${ibrc} (unexpected)" >&2; bad=$((bad + 1)) ;;
    esac
  fi

  # #1137 -- REPORT-ONLY vendor-pin ALARM. The cross-box parity above passes a UNIFORMLY-stale fleet
  # (every box agrees on an OLD genlock build); this PINS the fleet-deployed genlock_build_sha to the
  # NEWEST origin/main commit touching vendor/** and SCREAMS when it lags. It NEVER touches the gate's
  # bad/unknown counters (report-only) -- the coordinated-restart bundle deploy makes a hard block on
  # every E2E too blunt, so #1136's doctrine assigns this component an ALARM (see
  # genlock_vendor_pin_verdict's header for the two-step upgrade to a hard-gate). Reuses the deployed
  # SHAs already gathered in parity_args (no new read). Fail-closed-LOUD on an unreadable pin. Fixture
  # seams for the flow test: VERSION_INTEGRITY_GATE_VENDOR_NEWEST (override the newest vendor HEAD),
  # VERSION_INTEGRITY_GATE_VENDOR_PENDING (override the pending list; set-but-empty = "current"), and
  # (#1292) VERSION_INTEGRITY_GATE_VENDOR_AHEAD / VERSION_INTEGRITY_GATE_VENDOR_ON_DEV (override the
  # ahead-list / on-dev-line facts -- read only once VENDOR_PENDING is set, same activation as the
  # pending seam).
  echo "  -- vendor-pin alarm (#1137, report-only) --"
  local repo_root_vp=""
  repo_root_vp="$(cd "$HERE/.." 2>/dev/null && pwd)" || repo_root_vp=""
  local -a vp_shas=()
  local vp_seen=" " pe psha
  for pe in "${parity_args[@]}"; do
    psha="${pe#*=}"
    [ -z "$psha" ] && continue
    case "$vp_seen" in *" $psha "*) continue ;; esac
    vp_seen="${vp_seen}${psha} "
    vp_shas+=("$psha")
  done
  local vp_newest=""
  if [ -n "${VERSION_INTEGRITY_GATE_VENDOR_NEWEST:-}" ]; then
    vp_newest="$VERSION_INTEGRITY_GATE_VENDOR_NEWEST"
  elif [ -n "$repo_root_vp" ]; then
    timeout 15 git -C "$repo_root_vp" fetch origin --quiet 2>/dev/null || true
    vp_newest="$(git -C "$repo_root_vp" log -1 --format='%H' origin/main -- vendor/ 2>/dev/null || true)"
  fi
  if [ "${#vp_shas[@]}" -eq 0 ]; then
    local vp_out=""; vp_out="$(genlock_vendor_pin_verdict "" "$vp_newest" "")" || true
    printf '%s\n' "$vp_out" | sed 's/^/    /'
    echo "!! VENDOR-PIN ALARM: no deployed genlock_build_sha to pin -- vendor currency UNVERIFIED (report-only, does NOT block this run)." >&2
  else
    local vp_sha vp_newest_eff vp_pending vp_ahead vp_on_dev vp_rc vp_out vprc
    for vp_sha in "${vp_shas[@]}"; do
      vp_newest_eff="$vp_newest"
      vp_pending=""
      vp_ahead=""
      vp_on_dev=0
      vp_rc=0
      if [ -n "${VERSION_INTEGRITY_GATE_VENDOR_PENDING+x}" ]; then
        vp_pending="$VERSION_INTEGRITY_GATE_VENDOR_PENDING"
        vp_ahead="${VERSION_INTEGRITY_GATE_VENDOR_AHEAD:-}"
        vp_on_dev="${VERSION_INTEGRITY_GATE_VENDOR_ON_DEV:-0}"
      elif [ -n "$repo_root_vp" ] && [ -n "$vp_newest_eff" ]; then
        if git -C "$repo_root_vp" cat-file -e "${vp_sha}^{commit}" 2>/dev/null; then
          # #1292: merge-base-scoped LAG range (vendor_pin_range_log), never a plain ancestry range
          # -- see its own header for why a plain range falsely reads a deployed SHA that is
          # genuinely AHEAD of main on the dev candidate line as LAGGING. `|| vp_rc=$?` is
          # load-bearing (mirrored from drift-guard.sh's own #1292 review finding W1): a failing
          # ahead-log call must land on the SAME UNKNOWN path as a failing range-log call, never
          # silently swallow into an empty vp_ahead that genlock_vendor_pin_verdict would read as
          # "OK, current".
          vp_pending="$(vendor_pin_range_log "$repo_root_vp" "$vp_sha")" || vp_rc=$?
          if [ "$vp_rc" = "0" ] && [ -z "$vp_pending" ]; then
            vp_ahead="$(vendor_pin_ahead_log "$repo_root_vp" "$vp_sha")" || vp_rc=$?
            if [ "$vp_rc" = "0" ] && [ -n "$vp_ahead" ]; then
              vendor_pin_on_dev "$repo_root_vp" "$vp_sha" && vp_on_dev=1 || vp_on_dev=0
            fi
          fi
          if [ "$vp_rc" != "0" ]; then
            # A merge-base/log git error -> fail-closed UNKNOWN (never a false "no pending" from an
            # empty/partial read).
            vp_newest_eff=""
          fi
        else
          # The deployed SHA is unknown to local git -> rev-range would silently return "" and read
          # as "none pending" (a FALSE OK). Force UNKNOWN (fail-closed) instead.
          vp_newest_eff=""
        fi
      fi
      vprc=0
      vp_out="$(genlock_vendor_pin_verdict "$vp_sha" "$vp_newest_eff" "$vp_pending" "$vp_ahead" "$vp_on_dev")" || vprc=$?
      printf '%s\n' "$vp_out" | sed 's/^/    /'
      case "$vprc" in
        30) echo "!! VENDOR-PIN ALARM: deployed genlock bundle ${vp_sha} is DRIFTED from origin/main vendor HEAD (LAGS, or an unrecognized ORPHAN build reachable from neither origin/main nor origin/dev) -- see the vendor_pin detail line above for the exact reason; redeploy the fleet (report-only, does NOT block this run)." >&2 ;;
        31) echo "!! VENDOR-PIN ALARM: could not verify deployed genlock bundle ${vp_sha} against origin/main vendor HEAD (report-only)." >&2 ;;
      esac
    done
  fi

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
