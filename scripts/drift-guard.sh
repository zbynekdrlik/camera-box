#!/usr/bin/env bash
#
# drift-guard.sh — enforce the pinned zero-loss production set on strih + stream (#45).
#
# User directive (2026-06-12): the production OBS boxes must be KEPT on the exact versions +
# critical settings that guarantee permanent zero-loss functionality. This guard reads the
# installed OBS / DistroAV / NDI versions and the critical runtime settings (output fps, genlock
# master gate) and FAILS LOUDLY on any drift from the pinned set declared in vendor/README.md.
#
# Two facets, one engine:
#   * --check-pins (default, CI): validates the manifest declares a complete, well-formed pinned
#     set AND cross-checks the manifest's DistroAV pin against the vendored source
#     (vendor/distroav/buildspec.json) — catches the "subtree bumped but manifest stale" drift
#     class with no production access, so it runs on every CI run.
#   * --compare KEY=VAL …: compares values OBSERVED on a live box (gathered read-only via the
#     win-* MCP tools — see .claude/commands/drift-guard.md) against the pinned set and FAILS
#     loudly on drift. A missing observed value is reported UNKNOWN (never a silent pass).
#
# Like scripts/update-av-stack.sh, the file is split into PURE functions (manifest/log parse,
# version compare — unit-tested from tests/drift_guard.rs by sourcing this file) and a flow that
# runs only when executed directly. The source-guard below (BASH_SOURCE != $0) lets the tests
# exercise the pure functions in isolation. The OBS auto-update dialog is a BUILD property guarded
# at the source by tests/obs_updater_disabled.rs (#43) — it is not runtime-readable off a running
# box, so it is intentionally checked at that layer, not here.
#
# Usage:
#   scripts/drift-guard.sh [--check-pins] [--readme PATH]              # default: validate the pin set (CI)
#   scripts/drift-guard.sh --compare host=strih obs_version=32.1.2 \
#       distroav_version=6.2.1 ndi_runtime=6.3.2.0 output_fps=30 genlock_wall_clock=1
#   scripts/drift-guard.sh --help
#
# Exit codes: 0 = clean (pins valid / no drift), 20 = DRIFT detected, 11 = at least one observed
# value UNKNOWN (drift status incomplete — never reported as clean), 1 = usage/IO error.

set -euo pipefail

DEFAULT_README="vendor/README.md"

# --- PURE functions (no network, no MCP, no git mutation — unit-tested) --------------------

# pinned_subtree_version README PREFIX -> the **bold** version on PREFIX's subtree table row
# ("" if absent). The trailing `|| true` keeps a no-match from tripping `set -e` in the caller's
# command substitution, so an incomplete manifest surfaces as a loud MISSING in check_pins rather
# than a silent abort (same survives-no-match convention as update-av-stack.sh's latest_stable_tag).
pinned_subtree_version() {
  local readme="$1" prefix="$2"
  [ -f "$readme" ] || { echo "pinned_subtree_version: no such file: $readme" >&2; return 1; }
  grep -E "$prefix" "$readme" | grep 'subtree' \
    | sed -n 's/.*\*\*\([0-9][0-9.]*\)\*\*.*/\1/p' | head -1 || true
}

# pinned_obs_version / pinned_distroav_version README -> their subtree row's **bold** version.
pinned_obs_version()      { pinned_subtree_version "$1" 'vendor/obs-studio'; }
pinned_distroav_version() { pinned_subtree_version "$1" 'vendor/distroav'; }

# pinned_ndi_min README -> "6.3.0"  (the "NDI >= X.Y.Z" minimum the DistroAV plugin requires).
# Greedy ".*" lands on the last uppercase "NDI"; the digits that follow are the minimum version.
pinned_ndi_min() {
  local readme="$1"
  [ -f "$readme" ] || { echo "pinned_ndi_min: no such file: $readme" >&2; return 1; }
  grep -E 'NDI[^0-9]*[0-9]+\.[0-9]+\.[0-9]+' "$readme" \
    | sed -n 's/.*NDI[^0-9]*\([0-9][0-9.]*\).*/\1/p' | head -1 || true
}

# pinned_setting README KEY -> value from the "Pinned production settings" table row
# `| `KEY` | `VALUE` | … |` (the second back-ticked cell).
pinned_setting() {
  local readme="$1" key="$2"
  [ -f "$readme" ] || { echo "pinned_setting: no such file: $readme" >&2; return 1; }
  # The backticks below are LITERAL markdown delimiters in the grep/sed patterns, not command
  # substitution — they sit inside a double-quoted string and a single-quoted sed program.
  # shellcheck disable=SC2016
  grep -E "\| *\`${key}\` *\|" "$readme" \
    | sed -n 's/^[^|]*|[^|]*|[[:space:]]*`\([^`]*\)`.*/\1/p' | head -1 || true
}

# obs_version_from_log TEXT -> "32.1.2"  (OBS log header line "OBS 32.1.2 (64-bit, windows)").
obs_version_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*OBS \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' | head -1
}

# distroav_version_from_log TEXT -> "6.2.1"  ("you can haz DistroAV (Version 6.2.1)").
distroav_version_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*DistroAV (Version \([0-9][0-9.]*\)).*/\1/p' | head -1
}

# ndi_runtime_from_log TEXT -> "6.3.2.0"  ("[distroav] NDI Library Version detected: 6.3.2.0").
ndi_runtime_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*NDI Library Version detected: \([0-9][0-9.]*\).*/\1/p' | head -1
}

# genlock_from_log TEXT -> "1" if the running OBS reports the wall-clock genlock master gate
# ENABLED ("genlock: wall-clock-slaved render tick ENABLED"), "0" if it reports DISABLED, ""
# (UNKNOWN) if the build emits no genlock line at all. This is the AUTHORITATIVE runtime signal —
# the env var the gate is read from is captured at OBS launch, so a later `$env:` read (esp. via a
# long-lived MCP/launcher process holding a stale env snapshot) can disagree with the running
# process; the log line cannot.
genlock_from_log() {
  local text="$1" line
  # Drain-safe (matches the sibling *_from_log parsers): `grep -q` would exit on the first match
  # and leave printf writing into a closed pipe -> SIGPIPE -> pipefail flips the if-condition false
  # and the function wrongly returns UNKNOWN on a large real log. `grep | head -1` reads the input
  # through instead. `|| true` keeps a no-match from tripping the caller's set -e.
  line="$(printf '%s\n' "$text" \
    | grep -iE 'genlock:.*render tick (ENABLED|DISABLED)' | head -1 || true)"
  case "$line" in
    *ENABLED*) echo 1 ;;
    *DISABLED*) echo 0 ;;
  esac
}

# fps_from_log TEXT -> "30"  (the OUTPUT fps = the first `fps:` line INSIDE the OBS
# "video settings reset:" block — deliberately NOT the earlier graphics-adapter/monitor `fps:`).
fps_from_log() {
  printf '%s\n' "$1" | awk '
    /video settings reset:/ { inblk = 1; next }
    inblk && /fps:/ {
      line = $0
      sub(/.*fps:[ \t]+/, "", line)   # drop everything up to "fps:   "
      sub(/[^0-9].*/,    "", line)    # keep the leading integer ("30/1" -> "30")
      print line
      exit
    }'
}

# buildspec_version FILE -> top-level "version" of a DistroAV buildspec.json (vendored source).
buildspec_version() {
  local f="$1"
  [ -f "$f" ] || return 1
  if command -v jq >/dev/null 2>&1; then
    jq -r '.version // empty' "$f" 2>/dev/null
  else
    # Fallback: the top-level key sits at the document's minimum indent (4 spaces); nested
    # dependency "version" keys are deeper, so the 4-space anchor selects the canonical one.
    grep -E '^    "version":' "$f" | head -1 \
      | sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p'
  fi
}

# drift_check LABEL MODE EXPECTED OBSERVED -> prints a status line; returns 0 OK / 2 DRIFT /
# 3 UNKNOWN. MODE is "exact" (string equality) or "min" (observed semver >= expected, sort -V).
# An empty OBSERVED is UNKNOWN, never OK — a value we could not read must never look clean.
drift_check() {
  local label="$1" mode="$2" expected="$3" observed="$4" highest
  if [ -z "$observed" ]; then
    printf '  %-20s UNKNOWN  (expected %s, observed <missing>)\n' "$label" "$expected"
    return 3
  fi
  case "$mode" in
    exact)
      if [ "$observed" = "$expected" ]; then
        printf '  %-20s OK       (%s)\n' "$label" "$observed"; return 0
      fi
      printf '  %-20s DRIFT    (expected %s, observed %s)\n' "$label" "$expected" "$observed"
      return 2
      ;;
    min)
      highest="$(printf '%s\n%s\n' "${expected#v}" "${observed#v}" | sort -V | tail -1)"
      if [ "$highest" = "${observed#v}" ]; then
        printf '  %-20s OK       (%s >= %s)\n' "$label" "$observed" "$expected"; return 0
      fi
      printf '  %-20s DRIFT    (observed %s < required %s)\n' "$label" "$observed" "$expected"
      return 2
      ;;
    *)
      echo "drift_check: unknown mode '$mode'" >&2; return 1
      ;;
  esac
}

# validate_semver / validate_nonempty -> 0 if the pinned value is present + shaped, else 1 (loud).
validate_semver() {
  local name="$1" val="$2"
  if [ -z "$val" ]; then echo "  MISSING   $name" >&2; return 1; fi
  if printf '%s' "$val" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "  ok        $name = $val"; return 0
  fi
  echo "  MALFORMED $name = '$val' (want X.Y.Z)" >&2; return 1
}

validate_nonempty() {
  local name="$1" val="$2"
  if [ -z "$val" ]; then echo "  MISSING   $name" >&2; return 1; fi
  echo "  ok        $name = $val"; return 0
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------

usage() {
  cat <<'EOF'
drift-guard.sh — enforce the pinned zero-loss production set on strih + stream (#45).

Reads the pinned OBS/DistroAV/NDI versions + critical settings from vendor/README.md and either
validates that pinned set (CI) or compares it against values observed on a live box.

Usage:
  scripts/drift-guard.sh [--check-pins] [--readme PATH]   # validate the pin set (CI, default)
  scripts/drift-guard.sh --compare KEY=VAL ...            # compare live-observed values vs pins
  scripts/drift-guard.sh --help

--compare keys: host, obs_version, distroav_version, ndi_runtime, output_fps, genlock_wall_clock
  (gather them read-only off strih/stream via the win-* MCP tools — see
   .claude/commands/drift-guard.md). Any key you omit is reported UNKNOWN.

Exit codes: 0 = clean, 20 = DRIFT, 11 = some observed value UNKNOWN (incomplete, NOT clean),
1 = usage/IO error.
EOF
}

check_pins() {
  local readme="$1" p_obs="$2" p_distroav="$3" p_ndi="$4" p_fps="$5" p_genlock="$6"
  local errs=0
  echo "== drift-guard --check-pins ($readme) =="
  validate_semver   "obs_version"        "$p_obs"      || errs=$((errs + 1))
  validate_semver   "distroav_version"   "$p_distroav" || errs=$((errs + 1))
  validate_semver   "ndi_runtime_min"    "$p_ndi"      || errs=$((errs + 1))
  validate_nonempty "output_fps"         "$p_fps"      || errs=$((errs + 1))
  validate_nonempty "genlock_wall_clock" "$p_genlock"  || errs=$((errs + 1))
  if [ "$errs" -gt 0 ]; then
    echo >&2
    echo "!! $errs pinned value(s) missing or malformed in $readme." >&2
    echo "!! The drift guard cannot enforce an incomplete pin set — fix the manifest." >&2
    return 1
  fi
  echo
  echo "All pins present + well-formed:"
  echo "  obs=$p_obs distroav=$p_distroav ndi_min=$p_ndi output_fps=$p_fps genlock_wall_clock=$p_genlock"

  # Cross-check: the manifest's DistroAV pin must equal the vendored DistroAV source version.
  # This catches a `git subtree pull` that bumped vendor/distroav without updating the table
  # (or a table edit not backed by a real subtree pull) — a real drift, found with no prod access.
  local buildspec vendored
  buildspec="$(dirname "$readme")/distroav/buildspec.json"
  if [ -f "$buildspec" ]; then
    vendored="$(buildspec_version "$buildspec")"
    if [ -z "$vendored" ]; then
      echo "!! could not read the vendored DistroAV version from $buildspec." >&2
      return 1
    fi
    if [ "$vendored" != "$p_distroav" ]; then
      echo >&2
      echo "!! DRIFT: manifest pins DistroAV $p_distroav but the vendored source ($buildspec) is $vendored." >&2
      echo "!! The subtree and the manifest disagree — update the table in $readme or re-pull the subtree." >&2
      return 20
    fi
    echo "  vendored DistroAV source matches the manifest pin ($vendored)."
  else
    echo "  (vendored DistroAV buildspec not found at $buildspec — pin-shape validation only.)"
  fi
  return 0
}

compare_observed() {
  local host="$1" p_obs="$2" p_distroav="$3" p_ndi="$4" p_fps="$5" p_genlock="$6"
  local o_obs="$7" o_distroav="$8" o_ndi="$9" o_fps="${10}" o_genlock="${11}"

  echo "== drift-guard --compare  host=${host:-?}  (pins from manifest; FAILS loudly on drift) =="

  local -a checks=(
    "obs_version|exact|${p_obs}|${o_obs}"
    "distroav_version|exact|${p_distroav}|${o_distroav}"
    "ndi_runtime|min|${p_ndi}|${o_ndi}"
    "output_fps|exact|${p_fps}|${o_fps}"
    "genlock_wall_clock|exact|${p_genlock}|${o_genlock}"
  )
  local drift=0 unknown=0 rc entry label mode exp obs
  for entry in "${checks[@]}"; do
    IFS='|' read -r label mode exp obs <<< "$entry"
    rc=0
    drift_check "$label" "$mode" "$exp" "$obs" || rc=$?
    [ "$rc" -eq 2 ] && drift=$((drift + 1))
    [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
  done

  echo
  if [ "$drift" -gt 0 ]; then
    echo "!! DRIFT DETECTED on ${host:-target}: $drift setting(s) differ from the pinned zero-loss set." >&2
    echo "!! Restore the pinned versions/settings (the deploy is off-air + user-approved)." >&2
    [ "$unknown" -gt 0 ] && echo "!! ($unknown further setting(s) were UNKNOWN — drift status also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! $unknown setting(s) UNKNOWN (not read) on ${host:-target} — drift status INCOMPLETE, NOT clean." >&2
    echo "!! Supply every observed value before trusting a clean result." >&2
    exit 11
  fi
  echo "NO DRIFT — ${host:-target} matches the pinned zero-loss set."
  exit 0
}

main() {
  local mode="check-pins" readme="$DEFAULT_README"
  local -a kv=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --check-pins) mode="check-pins" ;;
      --compare)    mode="compare" ;;
      --readme)     shift; readme="${1:-}" ;;
      -h|--help)    usage; exit 0 ;;
      --*)          echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)            kv+=("$1") ;;   # key=val observed pairs for --compare
    esac
    shift || true
  done

  [ -f "$readme" ] || { echo "ERROR: manifest not found: $readme (run from repo root)" >&2; exit 1; }

  local p_obs p_distroav p_ndi p_fps p_genlock
  p_obs="$(pinned_obs_version "$readme")"
  p_distroav="$(pinned_distroav_version "$readme")"
  p_ndi="$(pinned_ndi_min "$readme")"
  p_fps="$(pinned_setting "$readme" output_fps)"
  p_genlock="$(pinned_setting "$readme" genlock_wall_clock)"

  if [ "$mode" = "check-pins" ]; then
    check_pins "$readme" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps" "$p_genlock"
    exit $?
  fi

  # --compare: collect observed key=val pairs.
  local host="" o_obs="" o_distroav="" o_ndi="" o_fps="" o_genlock="" pair k v
  for pair in "${kv[@]+"${kv[@]}"}"; do
    k="${pair%%=*}"; v="${pair#*=}"
    case "$k" in
      host)               host="$v" ;;
      obs_version)        o_obs="$v" ;;
      distroav_version)   o_distroav="$v" ;;
      ndi_runtime)        o_ndi="$v" ;;
      output_fps)         o_fps="$v" ;;
      genlock_wall_clock) o_genlock="$v" ;;
      *)                  echo "WARN: ignoring unknown observed key '$k'" >&2 ;;
    esac
  done

  compare_observed "$host" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps" "$p_genlock" \
    "$o_obs" "$o_distroav" "$o_ndi" "$o_fps" "$o_genlock"
}

main "$@"
