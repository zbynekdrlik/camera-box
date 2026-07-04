#!/usr/bin/env bash
#
# drift-guard.sh — enforce the pinned zero-loss production set on strih + stream + imag-nb (#45, #463).
#
# User directive (2026-06-12): the production OBS boxes must be KEPT on the exact versions +
# critical settings that guarantee permanent zero-loss functionality. This guard reads the
# installed OBS / DistroAV / NDI versions and the critical runtime settings (output fps, genlock
# master gate) and FAILS LOUDLY on any drift from the pinned set declared in vendor/README.md.
#
# Three facets, one engine:
#   * --check-pins (default, CI): validates the manifest declares a complete, well-formed pinned
#     set AND cross-checks the manifest's DistroAV pin against the vendored source
#     (vendor/distroav/buildspec.json) — catches the "subtree bumped but manifest stale" drift
#     class with no production access, so it runs on every CI run.
#   * --compare KEY=VAL … (strih/stream, Windows): compares values OBSERVED on a live box
#     (gathered read-only via the win-* MCP tools — see .claude/commands/drift-guard.md) against
#     the pinned set and FAILS loudly on drift. A missing observed value is reported UNKNOWN
#     (never a silent pass). When a `manifest=<BUNDLE_MANIFEST.json>` is supplied it ALSO checks
#     each component's BUILD SHA (the live obs.dll/distroav.dll Get-FileHash vs the #120 manifest)
#     + the genlock CAPABILITY markers only our build emits (#122) — so a STOCK/wrong build is
#     drift even when the marketing version matches (the #119 wrong-build-right-version that
#     silently shipped).
#   * --check-imag [host=IP] [user=U] (imag-nb, Linux, #463): a plain Linux box reachable over
#     SSH, so drift-guard gathers its OWN observed values directly (no win-* MCP round-trip) —
#     the genlock build SHA marker, the deployed distroav.so hash, and the OBS log's genlock
#     capability + fps + latency lines — and compares them against the imag-specific pins in
#     vendor/README.md. Simpler than the Windows path by construction (SSH IS the gathering).
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
#       distroav_version=6.2.1 ndi_runtime=6.3.2.0 output_fps=30 genlock_wall_clock=1 \   # host=strih→30, host=stream→30 (#459, was strih→60/#11)
#       ndi_input_latency="NDI cam5=0,NDI cam1=0,NDI cam3=0" \
#       distroav_dll_paths="C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"
#   scripts/drift-guard.sh --help
#
# Exit codes: 0 = clean (pins valid / no drift), 20 = DRIFT detected, 11 = at least one observed
# value UNKNOWN (drift status incomplete — never reported as clean), 1 = usage/IO error.

set -euo pipefail

DEFAULT_README="vendor/README.md"

# #390: the DistroAV per-source genlock-latency clamp (PROP_GENLOCK_LATENCY_MS_MIN /
# PROP_GENLOCK_SOURCE_LATENCY_MS_MAX in the vendored ndi-source.cpp fork) — the sane BACKSTOP range
# for a calibration-tracked source-latency pin (see drift_check_source_latency's `range:MIN-MAX`
# mode below). Any per-source held-latency the DistroAV UI/WS can even accept lies inside
# [MIN, MAX]; a value outside it is impossible from a correct apply. Mirror
# scripts/av_sync_calibrate.py's LATENCY_MIN/LATENCY_MAX — keep both in lock-step (same convention
# as required_delay_ms: Bash/Python can't share a literal across the WS boundary, so both copies
# must be updated together if the DistroAV clamp ever changes).
GENLOCK_LATENCY_MS_MIN=3
GENLOCK_LATENCY_MS_MAX=2000

# #390: best-effort tolerance (ms) for the live-vs-last-calibrated cross-check
# (drift_check_calibrated_source_latency below). Accounts for rounding noise between the Python
# controller's `round()` and the live OBS-read integer; a genuine drift (e.g. a hand-nudge in the
# OBS UI since the last calibration run) is normally far larger than this.
AV_SYNC_CALIBRATION_TOLERANCE_MS=10

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

# manifest_sha_for_component MANIFEST COMPONENT -> the sha256 recorded in MANIFEST's files[] for the
# logical COMPONENT's DLL ("obs" -> obs.dll, "distroav" -> distroav.dll), matched by BASENAME so both
# bundle layouts resolve: the hot-swap fast-dll layout (obs.dll at the stage root) AND the full
# windows-genlock bundle layout (bin/64bit/obs.dll, obs-plugins/64bit/distroav.dll). This is the
# MANIFEST side of the #122 per-component BUILD-SHA compare — the bytes drift-guard expects on the
# rig for a given build. Empty (-> UNKNOWN in the caller, never a false clean) if the component's
# dll is not listed. Pure text parse: files[] entries are one-per-line `{ "path": "…", "sha256": …`
# (same format genlock-manifest.sh::generate_manifest emits + tests/genlock_manifest.rs assert).
manifest_sha_for_component() {
  local manifest="$1" component="$2" dll
  [ -f "$manifest" ] || { echo "manifest_sha_for_component: no such file: $manifest" >&2; return 1; }
  # The basename is interpolated into the extended-regex below, so the literal dot is bracket-escaped
  # ([.]) — an unescaped `.` is an any-char wildcard that would over-match a (hypothetical) dot-less
  # basename like `obsXdll` and return the WRONG file's sha (#237).
  case "$component" in
    obs)      dll="obs[.]dll" ;;
    distroav) dll="distroav[.]dll" ;;
    *) echo "manifest_sha_for_component: unknown component '$component' (want obs|distroav)" >&2; return 1 ;;
  esac
  # Match a files[] line whose "path" ends in the dll basename (root or any nested dir), pull its
  # sha256. `|| true` keeps a no-match from tripping the caller's set -e/pipefail.
  grep -E "\"path\": \"([^\"]*/)?${dll}\"" "$manifest" \
    | sed -n 's/.*"sha256": "\([0-9a-f]*\)".*/\1/p' | head -1 || true
}

# manifest_all_paths MANIFEST -> the "path" of EVERY files[] entry (one per line, manifest order).
# This is the #121 whole-bundle lister: the #122 per-component lookup above resolves only obs.dll +
# distroav.dll, but #121's post-deploy verify must check that EVERY shipped file matches byte-for-byte
# (a partial/corrupted deploy where a non-DLL file is stale would otherwise pass #122). drift-guard
# owns its own parser here — it must NOT depend on genlock-manifest.sh at --compare time (that script
# is the PRODUCER; this one is a stand-alone CONSUMER on the operator's dev1, where only the manifest
# JSON has been downloaded). Same one-line `{ "path": "…", "sha256": … }` shape genlock-manifest.sh
# emits (+ tests/genlock_manifest.rs asserts). `|| true` so an empty files[] is a no-match, not a
# pipefail abort.
manifest_all_paths() {
  local manifest="$1"
  [ -f "$manifest" ] || { echo "manifest_all_paths: no such file: $manifest" >&2; return 1; }
  { grep -oE '"path": "[^"]*"' "$manifest" || true; } | sed -n 's/"path": "\(.*\)"/\1/p'
}

# manifest_sha_for_path MANIFEST PATH -> the recorded sha256 for the files[] entry whose "path" is
# exactly PATH ("" if absent). The #121 per-file lookup (distinct from the #122 by-BASENAME
# manifest_sha_for_component): the whole-bundle compare needs the sha for an EXACT path, since two
# files can share a basename across dirs. `sed … ;q` quits sed after the first match (a clean exit,
# NOT a downstream `head -1` that would SIGPIPE the upstream grep under pipefail — the #239 class).
# `|| true` so a no-match (path absent) is not a pipefail abort.
manifest_sha_for_path() {
  local manifest="$1" path="$2"
  [ -f "$manifest" ] || { echo "manifest_sha_for_path: no such file: $manifest" >&2; return 1; }
  { grep -F "\"path\": \"$path\"" "$manifest" || true; } \
    | sed -n '/"sha256"/{s/.*"sha256": "\([0-9a-f]*\)".*/\1/p;q}'
}

# observed_sha_for PATH OBSERVED_CSV -> the live sha256 observed for bundle file PATH from the
# comma-separated `relpath=sha256` list gathered off the box (Get-FileHash per deployed file), "" if
# PATH is not in the set (-> the caller reports it UNKNOWN, never a silent clean). Bundle relpaths use
# forward slashes and never contain commas or '=' (manifest paths are sha-stamped relative paths), so
# splitting on ',' then on the FIRST '=' is unambiguous. A whitespace-only entry is skipped.
observed_sha_for() {
  local want="$1" csv="$2" entry path sha
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    entry="${entry#"${entry%%[![:space:]]*}"}"; entry="${entry%"${entry##*[![:space:]]}"}"
    [ -z "$entry" ] && continue
    path="${entry%%=*}"; sha="${entry#*=}"
    path="${path#"${path%%[![:space:]]*}"}"; path="${path%"${path##*[![:space:]]}"}"
    if [ "$path" = "$want" ]; then printf '%s' "$sha"; return 0; fi
  done
}

# drift_check_all_files MANIFEST OBSERVED_CSV -> the #121 WHOLE-BUNDLE byte/SHA verify. Walks EVERY
# files[] entry in MANIFEST and compares its recorded sha256 against the live sha for that exact path
# in OBSERVED_CSV (`relpath=sha256,…`). Prints one status line per file + a roll-up "bundle_files
# N/total verified" line, and returns 0 OK (every file matches) / 2 DRIFT (any file's bytes differ) /
# 3 UNKNOWN (a manifest file was not observed, OR the observed set was empty — a file we could not hash
# is NEVER a silent clean). This is the deploy-from-clean-tree contract: a deploy is "done" only when
# every manifest-LISTED file on the live box matches byte-for-byte, so ANY mismatch fails the deploy.
# Scope note (asymmetric by design): this verifies the bytes of the files the manifest lists; it does
# NOT flag an EXTRA un-manifested file present on the box. The producer-side genlock-manifest.sh
# check_consistency catches extras at build time, and the dangerous subclass — a shadowing duplicate
# plugin DLL — is caught independently by drift_check_plugin_paths.
drift_check_all_files() {
  local manifest="$1" csv="$2" path exp obs drift=0 unknown=0 ok=0 total=0
  if [ ! -f "$manifest" ]; then
    echo "drift_check_all_files: no such manifest: $manifest" >&2; return 1
  fi
  if [ -z "$csv" ]; then
    printf '  %-20s UNKNOWN  (whole-bundle hash scan not run — no bundle_hashes observed)\n' "bundle_files"
    return 3
  fi
  # Iterate the manifest's files via a here-string (NOT `done < <(proc-sub)` — the genlock-manifest.sh
  # git-bash FIFO lesson) so the loop body's counters survive in this shell.
  local paths; paths="$(manifest_all_paths "$manifest")"
  while IFS= read -r path; do
    [ -z "$path" ] && continue
    total=$((total + 1))
    exp="$(manifest_sha_for_path "$manifest" "$path")"
    obs="$(observed_sha_for "$path" "$csv")"
    if [ -z "$obs" ]; then
      printf '  file %-32s UNKNOWN  (deployed file not hashed: %s)\n' "${path##*/}" "$path"
      unknown=$((unknown + 1))
    elif [ "$obs" = "$exp" ]; then
      printf '  file %-32s OK       (%s)\n' "${path##*/}" "$path"
      ok=$((ok + 1))
    else
      printf '  file %-32s DRIFT    (%s — expected %s, observed %s)\n' "${path##*/}" "$path" "$exp" "$obs"
      drift=$((drift + 1))
    fi
  done <<< "$paths"
  if [ "$total" -eq 0 ]; then
    printf '  %-20s UNKNOWN  (manifest lists no files[])\n' "bundle_files"
    return 3
  fi
  printf '  %-20s %d/%d verified (%d drift, %d unread)\n' "bundle_files" "$ok" "$total" "$drift" "$unknown"
  [ "$drift" -gt 0 ] && return 2
  [ "$unknown" -gt 0 ] && return 3
  return 0
}

# genlock_capability_from_log TEXT -> "1" if the running OBS log carries a genlock CAPABILITY marker
# that ONLY our genlock build emits (the wall-clock render-tick line, the #136 timestamp-aligned
# release line, the #184 sub-frame jitter reserve line, or the #235 single-knob `genlock: latency = N
# ms` line that superseded it), "" (UNKNOWN/absent) if the text carries none — a STOCK OBS log, which
# is the #119 wrong-build-right-version case this facet exists to catch. Distinct from genlock_from_log
# (which reads the ENABLED/DISABLED *state* of the wall-clock gate): this reads the PRESENCE of a
# build-unique capability, so a stock OBS (emits no `genlock:` line at all) is detectable even though
# its marketing version is identical to ours. Drain-safe (grep|head, never grep -q, matching the
# sibling *_from_log parsers — see genlock_from_log's note).
genlock_capability_from_log() {
  local text="$1" line
  line="$(printf '%s\n' "$text" \
    | grep -iE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' \
    | head -1 || true)"
  # Echo "1" when a build-unique marker is present; otherwise echo NOTHING (the absent signal).
  # `return 0` so the absent case is a clean exit (empty output, not a non-zero status) — the sibling
  # genlock_from_log relies on its final `case` falling through to 0; this explicit return matches.
  [ -n "$line" ] && echo 1
  return 0
}

# genlock_source_latency_from_log TEXT -> CSV "NAME=latency_ms,..." of per-source genlock held-latency
# parsed from `genlock-fifo audit 'SOURCE': … latency_ms=N …` log lines (#357). Returns every source
# found as "NAME=N" joined by commas. Empty output = no audit lines present. Drain-safe (grep|true
# convention — never grep -q to avoid SIGPIPE under set -euo pipefail).
genlock_source_latency_from_log() {
  local text="$1" result="" line name lat
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    name="$(printf '%s' "$line" | sed -n "s/.*genlock-fifo audit '\([^']*\)'.*/\1/p" || true)"
    # Match `latency_ms=N` where the preceding char is a space — this picks the EFFECTIVE latency
    # (the actual held value) and not `src_latency_ms=` or `global_latency_ms=` (both have
    # underscores immediately before `latency_ms`).
    lat="$(printf '%s' "$line" | sed -n 's/.* latency_ms=\([0-9][0-9]*\).*/\1/p' || true)"
    if [ -n "$name" ] && [ -n "$lat" ]; then
      [ -n "$result" ] && result="${result},"
      result="${result}${name}=${lat}"
    fi
  done <<< "$(printf '%s\n' "$text" | grep -E "genlock-fifo audit '" || true)"
  printf '%s' "$result"
}

# genlock_src_latency_for NAME CSV -> the observed latency_ms for source NAME out of a
# "NAME=ms,NAME=ms,…" CSV (the same shape drift_check_source_latency parses), "" if absent (never
# observed). Factored out so the #390 calibration cross-check below can look up a single source's
# live value without duplicating the split/trim loop already in drift_check_source_latency.
genlock_src_latency_for() {
  local want="$1" csv="$2" entry name lat
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    [ -z "$entry" ] && continue
    name="${entry%%=*}"; lat="${entry#*=}"
    name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"
    lat="${lat#"${lat%%[![:space:]]*}"}"; lat="${lat%"${lat##*[![:space:]]}"}"
    if [ "$name" = "$want" ]; then printf '%s' "$lat"; return 0; fi
  done
}

# genlock_latency_ms_from_log TEXT -> "3" (the configured genlock latency in ms) from the #235
# single-knob log line "genlock: latency = N ms (...)" ("" if absent). #463: used by imag's
# drift-guard host case to pin the deployed VALUE, not just its PRESENCE (which
# genlock_capability_from_log already covers as one of its alternation branches). Drain-safe
# (grep|sed|head, never grep -q) matching the sibling *_from_log parsers.
genlock_latency_ms_from_log() {
  printf '%s\n' "$1" \
    | grep -iE 'genlock:.*latency = [0-9]+ ms' \
    | sed -n 's/.*latency = \([0-9][0-9]*\) ms.*/\1/p' | head -1 || true
}

# check_imag_report EXP_BUILD_SHA OBS_BUILD_SHA EXP_DISTROAV_SHA OBS_DISTROAV_SHA EXP_FPS OBS_FPS
#   EXP_LATENCY_MS OBS_LATENCY_MS CAPABILITY_TEXT EXP_PLUGIN_PATH PLUGIN_PATH_PRESENT
#   -> the #463 imag-nb (Topology v2, EPIC #466) host case. PURE — every value is already
#   gathered (by [`gather_and_check_imag`], which does the actual SSH), so this function has NO
#   I/O and is directly unit-testable, mirroring `compare_observed`'s pure-report-from-inputs
#   shape. Prints one report line per check (OK / DRIFT / UNKNOWN, matching the strih/stream
#   report style) and returns 0 = clean, 20 = DRIFT, 11 = at least one UNKNOWN (never a silent
#   pass — same exit-code contract as the rest of the engine).
#
# Simpler than the strih/stream `--compare` path (#463 research comment): imag is a plain Linux
# box reachable over SSH, so drift-guard can gather its OWN observed values directly instead of
# depending on an external win-* MCP round-trip.
check_imag_report() {
  local exp_build_sha="$1" obs_build_sha="$2" exp_distroav_sha="$3" obs_distroav_sha="$4"
  local exp_fps="$5" obs_fps="$6" exp_latency="$7" obs_latency="$8"
  local capability_text="$9" exp_plugin_path="${10}" plugin_present="${11}"
  local drift=0 unknown=0

  # 1. /opt/obs-genlock/GENLOCK_BUILD_SHA.txt — the deployed genlock build identity.
  if [ -z "$obs_build_sha" ]; then
    printf '  %-22s UNKNOWN  (GENLOCK_BUILD_SHA.txt not read on imag-nb)\n' "genlock_build_sha"
    unknown=$((unknown + 1))
  elif [ -z "$exp_build_sha" ]; then
    printf '  %-22s UNKNOWN  (no pinned genlock_build_sha_imag in README)\n' "genlock_build_sha"
    unknown=$((unknown + 1))
  elif [ "$obs_build_sha" = "$exp_build_sha" ]; then
    printf '  %-22s OK       (%s)\n' "genlock_build_sha" "$obs_build_sha"
  else
    printf '  %-22s DRIFT    (expected %s, observed %s)\n' "genlock_build_sha" "$exp_build_sha" "$obs_build_sha"
    drift=$((drift + 1))
  fi

  # 2. distroav.so hash (the Linux plugin binary itself, not just the marker file).
  if [ -z "$obs_distroav_sha" ]; then
    printf '  %-22s UNKNOWN  (distroav.so hash not read on imag-nb)\n' "distroav_so_sha256"
    unknown=$((unknown + 1))
  elif [ -z "$exp_distroav_sha" ]; then
    printf '  %-22s UNKNOWN  (no pinned distroav_so_sha256_imag in README)\n' "distroav_so_sha256"
    unknown=$((unknown + 1))
  elif [ "$obs_distroav_sha" = "$exp_distroav_sha" ]; then
    printf '  %-22s OK       (%s)\n' "distroav_so_sha256" "$obs_distroav_sha"
  else
    printf '  %-22s DRIFT    (expected %s, observed %s)\n' "distroav_so_sha256" "$exp_distroav_sha" "$obs_distroav_sha"
    drift=$((drift + 1))
  fi

  # 3. genlock render-tick capability marker present in the OBS log — the #119 wrong-build guard
  # (a stock/wrong build emits no `genlock:` line at all, regardless of marketing version).
  if [ -n "$capability_text" ]; then
    printf '  %-22s OK       (%s)\n' "genlock_capability" "$capability_text"
  else
    printf '  %-22s DRIFT    (no genlock render-tick marker in the OBS log — stock/wrong build)\n' "genlock_capability"
    drift=$((drift + 1))
  fi

  # 4. fps pin (imag is the 60fps low-latency IMAG role, Topology v2 — a drift DOWN to 30 is drift).
  if [ -z "$obs_fps" ]; then
    printf '  %-22s UNKNOWN  (fps not read on imag-nb)\n' "output_fps_imag"
    unknown=$((unknown + 1))
  elif [ -z "$exp_fps" ]; then
    printf '  %-22s UNKNOWN  (no pinned output_fps_imag in README)\n' "output_fps_imag"
    unknown=$((unknown + 1))
  elif [ "$obs_fps" = "$exp_fps" ]; then
    printf '  %-22s OK       (%s)\n' "output_fps_imag" "$obs_fps"
  else
    printf '  %-22s DRIFT    (expected %s, observed %s)\n' "output_fps_imag" "$exp_fps" "$obs_fps"
    drift=$((drift + 1))
  fi

  # 5. genlock latency pin (the #235 single-knob ms value).
  if [ -z "$obs_latency" ]; then
    printf '  %-22s UNKNOWN  (latency not read on imag-nb)\n' "genlock_latency_ms_imag"
    unknown=$((unknown + 1))
  elif [ -z "$exp_latency" ]; then
    printf '  %-22s UNKNOWN  (no pinned genlock_latency_ms_imag in README)\n' "genlock_latency_ms_imag"
    unknown=$((unknown + 1))
  elif [ "$obs_latency" = "$exp_latency" ]; then
    printf '  %-22s OK       (%s)\n' "genlock_latency_ms_imag" "$obs_latency"
  else
    printf '  %-22s DRIFT    (expected %s, observed %s)\n' "genlock_latency_ms_imag" "$exp_latency" "$obs_latency"
    drift=$((drift + 1))
  fi

  # 6. Linux plugin path — the SAME single-canonical-path invariant as `canonical_plugin_path`
  # (#124/#125) on strih/stream, applied to imag's Linux plugin directory instead.
  if [ "$plugin_present" = "1" ]; then
    printf '  %-22s OK       (%s)\n' "distroav_so_path" "$exp_plugin_path"
  elif [ -z "$plugin_present" ]; then
    printf '  %-22s UNKNOWN  (path presence not read on imag-nb)\n' "distroav_so_path"
    unknown=$((unknown + 1))
  else
    printf '  %-22s DRIFT    (%s not found on imag-nb)\n' "distroav_so_path" "$exp_plugin_path"
    drift=$((drift + 1))
  fi

  [ "$drift" -gt 0 ] && return 20
  [ "$unknown" -gt 0 ] && return 11
  return 0
}

# gather_and_check_imag HOST USER README -> SSH-gathers the observed values from the LIVE
# imag-nb box (#463) and runs [`check_imag_report`] against the pinned set in README. NOT unit
# tested (it is pure I/O glue over `ssh` — same convention as the win-* MCP gathering for
# strih/stream, which is also untested at this layer; the JUDGEMENT is in the pure function
# above). Paths are the ones `scripts/setup-imag.sh` installs to (DESKTOP_USER=newlevel):
#   - /opt/obs-genlock/GENLOCK_BUILD_SHA.txt   (genlock build identity marker)
#   - /usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so   (the Linux plugin binary)
#   - ~/.config/obs-studio/logs/*.log   (OBS log — the MOST RECENT file, same libobs log lines
#     fps_from_log / genlock_capability_from_log / genlock_latency_ms_from_log already parse on
#     Windows, since the log format is platform-independent libobs text)
gather_and_check_imag() {
  local host="$1" user="${2:-newlevel}" readme="$3"
  local target="${user}@${host}"
  local plugin_path="/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so"

  local exp_build_sha exp_distroav_sha exp_fps exp_latency
  exp_build_sha="$(pinned_setting "$readme" genlock_build_sha_imag)"
  exp_distroav_sha="$(pinned_setting "$readme" distroav_so_sha256_imag)"
  exp_fps="$(pinned_setting "$readme" output_fps_imag)"
  exp_latency="$(pinned_setting "$readme" genlock_latency_ms_imag)"

  local obs_build_sha obs_distroav_sha obs_log obs_fps obs_latency capability plugin_present
  obs_build_sha="$(ssh -o ConnectTimeout=10 "$target" \
    'cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null' 2>/dev/null || true)"
  obs_distroav_sha="$(ssh -o ConnectTimeout=10 "$target" \
    "sha256sum '$plugin_path' 2>/dev/null | awk '{print \$1}'" 2>/dev/null || true)"
  if ssh -o ConnectTimeout=10 "$target" "test -f '$plugin_path'" 2>/dev/null; then
    plugin_present=1
  else
    plugin_present=0
  fi
  # Most-recently-modified OBS log file (OBS names logs by timestamp, no "latest" symlink).
  obs_log="$(ssh -o ConnectTimeout=10 "$target" \
    'f=$(ls -t "$HOME/.config/obs-studio/logs/"*.log 2>/dev/null | head -1); [ -n "$f" ] && cat "$f"' \
    2>/dev/null || true)"
  obs_fps="$(fps_from_log "$obs_log")"
  obs_latency="$(genlock_latency_ms_from_log "$obs_log")"
  capability="$(genlock_capability_from_log "$obs_log")"

  echo "== drift-guard --check-imag  host=${host}  (SSH-gathered; FAILS loudly on drift) =="
  check_imag_report "$exp_build_sha" "$obs_build_sha" "$exp_distroav_sha" "$obs_distroav_sha" \
    "$exp_fps" "$obs_fps" "$exp_latency" "$obs_latency" "$capability" "$plugin_path" "$plugin_present"
}

# range_tracked_source_name EXPECTED_CSV -> the NAME of the (first) source in a mixed
# "NAME=ms,…" / "NAME=range:MIN-MAX,…" expected CSV that is pinned as `range:` (calibration-
# tracked), "" if none. #390: the stream A/V-align source (`NDI 2ME PGM`) is pinned this way; the
# strih camera-floor sources stay plain exact-ms pins (structural, not calibrated).
range_tracked_source_name() {
  local csv="$1" entry name val
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    [ -z "$entry" ] && continue
    name="${entry%%=*}"; val="${entry#*=}"
    name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"
    val="${val#"${val%%[![:space:]]*}"}"; val="${val%"${val##*[![:space:]]}"}"
    case "$val" in
      range:*) printf '%s' "$name"; return 0 ;;
    esac
  done
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

# drift_check_inputs EXPECTED OBSERVED_CSV -> per-input latency drift on the genlocked
# broadcast-path NDI inputs (#84). EXPECTED is the single pinned latency mode (e.g. "0"=Normal);
# OBSERVED_CSV is a comma-separated "input name=latency" list gathered live (the obs-websocket
# GetInputSettings `latency` field per input). Each entry that differs from EXPECTED is DRIFT;
# an EMPTY observed set is UNKNOWN (never OK — a path we could not read must not look clean).
# Prints one status line per input and a verdict; returns 0 OK / 2 DRIFT / 3 UNKNOWN.
drift_check_inputs() {
  local expected="$1" csv="$2" entry name lat drift=0 n=0
  if [ -z "$csv" ]; then
    printf '  %-20s UNKNOWN  (expected every broadcast input = %s, observed <none>)\n' \
      "ndi_input_latency" "$expected"
    return 3
  fi
  # Split on commas (input names may contain spaces — "NDI cam5" — but never commas).
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    [ -z "$entry" ] && continue
    name="${entry%%=*}"; lat="${entry#*=}"
    # trim surrounding whitespace from the name/value
    name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"
    lat="${lat#"${lat%%[![:space:]]*}"}"; lat="${lat%"${lat##*[![:space:]]}"}"
    # A whitespace-only entry (e.g. a doubled comma " , ") trims to a blank name — it
    # carries no input, so skip it rather than emit a confusing blank-named DRIFT line.
    [ -z "$name" ] && continue
    n=$((n + 1))
    if [ "$lat" = "$expected" ]; then
      printf '  input %-20s OK       (latency=%s)\n' "$name" "$lat"
    else
      printf '  input %-20s DRIFT    (expected latency=%s, observed %s)\n' "$name" "$expected" "$lat"
      drift=$((drift + 1))
    fi
  done
  if [ "$n" -eq 0 ]; then
    printf '  %-20s UNKNOWN  (expected every broadcast input = %s, observed <none>)\n' \
      "ndi_input_latency" "$expected"
    return 3
  fi
  [ "$drift" -gt 0 ] && return 2
  return 0
}

# drift_check_source_latency EXPECTED_CSV OBSERVED_CSV -> per-source genlock FIFO held-latency gate
# (#357, range mode added #390). EXPECTED_CSV is the pinned "NAME=ms,…" list from the manifest
# (host-keyed: genlock_source_latency_strih or _stream); an entry's VALUE may be either a plain
# ms number (exact-match mode — the strih camera-floor pins, which are structural, not calibrated)
# or `range:MIN-MAX` (#390 calibration-tracked mode — the stream A/V-align source, whose correct
# value changes every time the operator re-calibrates #188 and so cannot be a single hardcoded
# constant without going stale; only an egregiously out-of-range value is flagged). OBSERVED_CSV is
# the live "NAME=ms,…" read from the OBS log via genlock_source_latency_from_log. Each source in
# EXPECTED is checked against OBSERVED; a source present in EXPECTED but absent in OBSERVED is
# UNKNOWN (never silently OK). Returns 0 OK / 2 DRIFT / 3 UNKNOWN. Prints one status line per
# expected source. (The SEPARATE best-effort check against the #427-persisted last-calibrated
# value lives in drift_check_calibrated_source_latency below — this function only enforces the
# sane backstop range / the structural exact pins.)
drift_check_source_latency() {
  local expected_csv="$1" observed_csv="$2" drift=0 unknown=0 n=0
  if [ -z "$observed_csv" ]; then
    printf '  %-20s UNKNOWN  (per-source genlock latency not read — no genlock-fifo audit lines?)\n' \
      "genlock_src_latency"
    return 3
  fi
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a obs_entries=($observed_csv)
  # shellcheck disable=SC2206
  local -a exp_entries=($expected_csv)
  IFS="$OLDIFS"
  local exp_entry obs_entry exp_name exp_lat obs_name obs_val obs_lat
  for exp_entry in "${exp_entries[@]}"; do
    [ -z "$exp_entry" ] && continue
    exp_name="${exp_entry%%=*}"; exp_lat="${exp_entry#*=}"
    exp_name="${exp_name#"${exp_name%%[![:space:]]*}"}"; exp_name="${exp_name%"${exp_name##*[![:space:]]}"}"
    exp_lat="${exp_lat#"${exp_lat%%[![:space:]]*}"}"; exp_lat="${exp_lat%"${exp_lat##*[![:space:]]}"}"
    [ -z "$exp_name" ] && continue
    n=$((n + 1))
    obs_lat=""
    for obs_entry in "${obs_entries[@]}"; do
      [ -z "$obs_entry" ] && continue
      obs_name="${obs_entry%%=*}"; obs_val="${obs_entry#*=}"
      obs_name="${obs_name#"${obs_name%%[![:space:]]*}"}"; obs_name="${obs_name%"${obs_name##*[![:space:]]}"}"
      obs_val="${obs_val#"${obs_val%%[![:space:]]*}"}"; obs_val="${obs_val%"${obs_val##*[![:space:]]}"}"
      if [ "$obs_name" = "$exp_name" ]; then
        obs_lat="$obs_val"
        break
      fi
    done
    if [ -z "$obs_lat" ]; then
      printf '  source %-20s UNKNOWN  (pinned latency_ms=%s, source not in log)\n' "$exp_name" "$exp_lat"
      unknown=$((unknown + 1))
    else
      case "$exp_lat" in
        range:*)
          # #390 calibration-tracked mode: any value inside the sane [MIN, MAX] backstop is OK —
          # there is no single correct constant (it changes with every A/V-sync re-calibration).
          local rng rmin rmax
          rng="${exp_lat#range:}"; rmin="${rng%-*}"; rmax="${rng#*-}"
          if printf '%s' "$obs_lat" | grep -qE '^[0-9]+$' \
             && [ "$obs_lat" -ge "$rmin" ] && [ "$obs_lat" -le "$rmax" ]; then
            printf '  source %-20s OK       (latency_ms=%s, within calibration-tracked range %s-%s)\n' \
              "$exp_name" "$obs_lat" "$rmin" "$rmax"
          else
            printf '  source %-20s DRIFT    (latency_ms=%s outside the sane calibration-tracked range %s-%s)\n' \
              "$exp_name" "$obs_lat" "$rmin" "$rmax"
            drift=$((drift + 1))
          fi
          ;;
        *)
          if [ "$obs_lat" = "$exp_lat" ]; then
            printf '  source %-20s OK       (latency_ms=%s)\n' "$exp_name" "$obs_lat"
          else
            printf '  source %-20s DRIFT    (pinned latency_ms=%s, observed %s)\n' "$exp_name" "$exp_lat" "$obs_lat"
            drift=$((drift + 1))
          fi
          ;;
      esac
    fi
  done
  if [ "$n" -eq 0 ]; then
    printf '  %-20s UNKNOWN  (expected pin is empty)\n' "genlock_src_latency"
    return 3
  fi
  # DRIFT takes priority over UNKNOWN — consistent with all sibling checkers (drift_check_all_files,
  # drift_check_inputs). When a source is drifted AND another source is unobserved, the correct
  # top-level verdict is DRIFT (exit 20 to callers), not UNKNOWN (exit 11). The per-source UNKNOWN
  # lines are printed regardless; the top-level exit code names the WORST condition.
  [ "$drift" -gt 0 ] && return 2
  [ "$unknown" -gt 0 ] && return 3
  return 0
}

# drift_check_calibrated_source_latency SOURCE OBSERVED_CSV CALIBRATED_MS TOLERANCE_MS -> #390
# best-effort cross-check of the LIVE per-source genlock latency against the #427-persisted
# last-calibrated value (av-sync-last.json's `applied_latency_ms`, read off the OBS box's
# ProgramData and passed in by the operator/agent — drift-guard itself runs on dev1 and cannot
# reach that path directly). This is IN ADDITION to the sane-range backstop in
# drift_check_source_latency above: the range check alone would miss a genuine drift (e.g. someone
# hand-nudges the OBS UI slider to a still-in-range but wrong value) — this check catches that IF
# the calibrated value was supplied.
#
# CALIBRATED_MS="" (not supplied — the file was unreachable, or the operator/agent skipped the
# gather) is NEVER a failure: prints an informational SKIPPED line and returns 0 — the #390
# graceful-degradation contract ("do NOT fail the whole drift-guard on its absence"). A missing
# SOURCE in OBSERVED_CSV while CALIBRATED_MS IS supplied is a genuine UNKNOWN (we meant to check
# but the live value was not read) — never a silent pass. Returns 0 OK(/skipped) / 2 DRIFT /
# 3 UNKNOWN. TOLERANCE_MS defaults to 10 (rounding-noise allowance, see AV_SYNC_CALIBRATION_TOLERANCE_MS).
drift_check_calibrated_source_latency() {
  local source="$1" observed_csv="$2" calibrated_ms="$3" tolerance_ms="${4:-10}"
  local obs_lat diff
  if [ -z "$calibrated_ms" ]; then
    printf '  %-20s SKIPPED  (last-calibrated value not available for %s — av-sync-last.json not supplied; range-checked only)\n' \
      "genlock_calibration" "$source"
    return 0
  fi
  obs_lat="$(genlock_src_latency_for "$source" "$observed_csv")"
  if [ -z "$obs_lat" ]; then
    printf '  %-20s UNKNOWN  (last-calibrated=%sms for %s, but source not in observed latency set)\n' \
      "genlock_calibration" "$calibrated_ms" "$source"
    return 3
  fi
  diff=$((obs_lat - calibrated_ms))
  [ "$diff" -lt 0 ] && diff=$((-diff))
  if [ "$diff" -le "$tolerance_ms" ]; then
    printf '  %-20s OK       (%s latency_ms=%s matches last-calibrated %sms, within +/-%sms)\n' \
      "genlock_calibration" "$source" "$obs_lat" "$calibrated_ms" "$tolerance_ms"
    return 0
  fi
  printf '  %-20s DRIFT    (%s latency_ms=%s has drifted %sms from last-calibrated %sms, tolerance +/-%sms)\n' \
    "genlock_calibration" "$source" "$obs_lat" "$diff" "$calibrated_ms" "$tolerance_ms"
  return 2
}

# drift_check_plugin_paths CANONICAL OBSERVED_CSV -> single-canonical OBS plugin-load path guard
# (#124, EPIC #125). CANONICAL is the pinned single directory the genlock DistroAV plugin must load
# from (e.g. C:\ProgramData\obs-studio\plugins\distroav\bin\64bit). OBSERVED_CSV is a comma-separated
# list of EVERY distroav.dll location found across the box's OBS scan paths (gathered live — see
# .claude/commands/drift-guard.md). The #124 failure class is a SECOND copy in another scan path
# (ProgramData AND Program Files\obs-plugins\64bit, or a portable dir) that can silently SHADOW the
# intended build — the mixed-version incident #119 that burned the user. Rules:
#   * exactly ONE location, AND it is at the canonical path  -> OK (rc 0)
#   * more than one location (a shadow/duplicate)            -> DRIFT (rc 2) — names the extra path(s)
#   * exactly one location but NOT at the canonical path     -> DRIFT (rc 2)
#   * empty observed set (scan not run)                      -> UNKNOWN (rc 3, never silently OK)
# An observed entry may be the directory OR the full distroav.dll path; both count as "at canonical"
# when the entry's directory equals CANONICAL (Windows paths compared case-insensitively, since the
# filesystem is). Windows paths contain backslashes and spaces but never commas, so the CSV split is
# unambiguous (same convention as drift_check_inputs).
drift_check_plugin_paths() {
  local canonical="$1" csv="$2" entry dir lc_dir lc_canon n=0 at_canon=0 off=0
  if [ -z "$csv" ]; then
    printf '  %-20s UNKNOWN  (expected one distroav.dll at %s, observed <none>)\n' \
      "distroav_dll_paths" "$canonical"
    return 3
  fi
  lc_canon="$(printf '%s' "${canonical%\\}" | tr '[:upper:]' '[:lower:]')"
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    # trim surrounding whitespace
    entry="${entry#"${entry%%[![:space:]]*}"}"; entry="${entry%"${entry##*[![:space:]]}"}"
    [ -z "$entry" ] && continue
    n=$((n + 1))
    # Reduce an entry to its directory: if it ends in .dll, strip the trailing \<file>; else it IS
    # the directory. Then drop a trailing backslash and lower-case for the case-insensitive compare.
    case "$entry" in
      *.dll|*.DLL|*.Dll) dir="${entry%\\*}" ;;
      *)                 dir="$entry" ;;
    esac
    dir="${dir%\\}"
    lc_dir="$(printf '%s' "$dir" | tr '[:upper:]' '[:lower:]')"
    if [ "$lc_dir" = "$lc_canon" ]; then
      at_canon=$((at_canon + 1))
      printf '  plugin %-20s OK       (%s)\n' "distroav.dll" "$entry"
    else
      off=$((off + 1))
      printf '  plugin %-20s DRIFT    (off the canonical path: %s)\n' "distroav.dll" "$entry"
    fi
  done
  if [ "$n" -eq 0 ]; then
    printf '  %-20s UNKNOWN  (expected one distroav.dll at %s, observed <none>)\n' \
      "distroav_dll_paths" "$canonical"
    return 3
  fi
  # More than one location anywhere = a shadow (even if one of them is canonical): a stale copy in a
  # second scan path can mask the intended build. A lone copy off the canonical path is drift too.
  if [ "$n" -gt 1 ]; then
    printf '  %-20s DRIFT    (%d distroav.dll copies across scan paths — a stale one can shadow the canonical build)\n' \
      "distroav_dll_paths" "$n"
    return 2
  fi
  if [ "$off" -gt 0 ]; then
    printf '  %-20s DRIFT    (the single distroav.dll is not on the canonical path %s)\n' \
      "distroav_dll_paths" "$canonical"
    return 2
  fi
  return 0
}

# drift_check_capability OBSERVED_CAP_TEXT -> the #122 genlock CAPABILITY guard. OBSERVED_CAP_TEXT is
# the live OBS-log text gathered read-only off the box (the lines the running OBS emitted). A build
# that emits a genlock capability marker is OUR build (OK); a build that emits NONE is a STOCK /
# wrong build (DRIFT — the #119 case: identical marketing version, different bytes); an EMPTY observed
# text is UNKNOWN (the log was not read — never a silent clean). Prints one status line; returns
# 0 OK / 2 DRIFT / 3 UNKNOWN.
drift_check_capability() {
  local cap_text="$1" present
  if [ -z "$cap_text" ]; then
    printf '  %-20s UNKNOWN  (genlock capability marker not read off the box)\n' "genlock_capability"
    return 3
  fi
  present="$(genlock_capability_from_log "$cap_text")"
  if [ "$present" = "1" ]; then
    printf '  %-20s OK       (genlock build-unique marker present — our build)\n' "genlock_capability"
    return 0
  fi
  printf '  %-20s DRIFT    (NO genlock capability marker — a STOCK/wrong OBS build, identical version)\n' \
    "genlock_capability"
  return 2
}

# drift_check_burn_env OBSERVED -> the #246/#257 prod burn guard. The QR measurement burn is
# TEST-mode ONLY and must NEVER be left ON in prod. #257 made it a per-source `genlock_burn` bool
# (no OBS_BURN_* env any more), toggled over OBS WebSocket — so the check is now "no prod source has
# genlock_burn=on", read read-only off the box over WebSocket (see .claude/commands/drift-guard.md):
# OBSERVED is the literal "none" when no source has the burn on, or a comma-separated `SOURCE=on`
# list of every source whose genlock_burn IS on. Returns 0 OK (none on) / 2 DRIFT (any source on) /
# 3 UNKNOWN (empty — not read; never a silent clean). An entry with an EMPTY value is NOT set
# (skipped). Source/var names contain no commas, so the CSV split is unambiguous. (The `burn_env`
# label + key name are kept for back-compat with the --compare contract; the VALUE it carries is now
# the per-source genlock_burn state, not Machine env.)
drift_check_burn_env() {
  local observed="$1" entry name val set_count=0
  if [ -z "$observed" ]; then
    printf '  %-20s UNKNOWN  (prod burn state not read — expected no source genlock_burn=on)\n' "burn_env"
    return 3
  fi
  if [ "$observed" = "none" ]; then
    printf '  %-20s OK       (no prod source has genlock_burn=on)\n' "burn_env"
    return 0
  fi
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($observed)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    entry="${entry#"${entry%%[![:space:]]*}"}"; entry="${entry%"${entry##*[![:space:]]}"}"
    [ -z "$entry" ] && continue
    name="${entry%%=*}"; val="${entry#*=}"
    name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"
    val="${val#"${val%%[![:space:]]*}"}"; val="${val%"${val##*[![:space:]]}"}"
    [ -z "$name" ] && continue
    # An entry present but with an empty value is NOT actually on — skip it (never a false DRIFT).
    # (The gather emits a source only when genlock_burn is genuinely on, e.g. `NDI cam5=on`, so the
    # trimmed-empty skip cannot mask a genuine prod burn.)
    [ -z "$val" ] && continue
    printf '  burn %-20s DRIFT    (prod source has the measurement burn ON: %s=%s)\n' "$name" "$name" "$val"
    set_count=$((set_count + 1))
  done
  if [ "$set_count" -gt 0 ]; then
    return 2
  fi
  printf '  %-20s OK       (no prod source has genlock_burn=on)\n' "burn_env"
  return 0
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

# validate_source_latency_range NAME CSV -> 0 if every `range:MIN-MAX` entry in a
# genlock_source_latency_* pin's CSV matches the CURRENT DistroAV clamp EXACTLY
# (GENLOCK_LATENCY_MS_MIN/_MAX above), else 1 (loud). #390: the pin text and the code's clamp are
# two independent copies (bash constant vs markdown table) — a manifest typo (e.g. `range:3-200`
# instead of `range:3-2000`) would silently narrow the backstop range (weakening the gate) or widen
# it (letting a genuinely bad value through) without CI ever noticing. This closes that gap. Entries
# that are plain exact-ms values (the structural strih pins) are not this function's concern.
validate_source_latency_range() {
  local name="$1" csv="$2" entry val rng rmin rmax errs=0
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  for entry in "${entries[@]}"; do
    [ -z "$entry" ] && continue
    val="${entry#*=}"
    case "$val" in
      range:*)
        rng="${val#range:}"; rmin="${rng%-*}"; rmax="${rng#*-}"
        if [ "$rmin" != "$GENLOCK_LATENCY_MS_MIN" ] || [ "$rmax" != "$GENLOCK_LATENCY_MS_MAX" ]; then
          echo "  MALFORMED $name: '$entry' pins range $rmin-$rmax but the code's DistroAV clamp is $GENLOCK_LATENCY_MS_MIN-$GENLOCK_LATENCY_MS_MAX (they must match)" >&2
          errs=$((errs + 1))
        fi
        ;;
    esac
  done
  [ "$errs" -eq 0 ]
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
  scripts/drift-guard.sh --status  KEY=VAL ...            # read-only genlock + burn state, one place
  scripts/drift-guard.sh --check-imag [host=IP] [user=U]  # #463: imag-nb, gathered over SSH (no MCP)
  scripts/drift-guard.sh --help

--check-imag (#463, EPIC #466 Topology v2): unlike strih/stream (Windows, needs the win-* MCP
  tools to read logs/settings), imag-nb is a plain Linux box reachable over SSH, so drift-guard
  gathers its OWN observed values directly: `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt` (the deployed
  genlock build identity), a SHA256 of `/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so` (the
  Linux plugin binary), and the OBS log (`~/.config/obs-studio/logs/*.log`, most recent file) for
  the genlock capability marker + the fps + latency pins. Optional `host=` / `user=` override the
  imag-nb defaults (`10.77.9.182` / `newlevel`). Pins live in vendor/README.md as
  `genlock_build_sha_imag` / `distroav_so_sha256_imag` / `output_fps_imag` / `genlock_latency_ms_imag`.

--compare keys: host, obs_version, distroav_version, ndi_runtime, output_fps, genlock_wall_clock,
  ndi_input_latency (a comma-separated "input name=latency" list for the genlocked broadcast-path
  NDI inputs, e.g. ndi_input_latency="NDI cam5=0,NDI cam1=0,NDI cam3=0" on strih or
  ndi_input_latency="NDI 2ME PGM=0" on stream — each input's obs-websocket GetInputSettings
  `latency` field; 0=Normal is the pinned certified low-latency zero-loss mode, #84),
  distroav_dll_paths (a comma-separated list of EVERY distroav.dll location found across the box's
  OBS scan paths — Program Files\obs-studio\obs-plugins\64bit, ProgramData\obs-studio\plugins\*\
  bin\64bit, %APPDATA%\obs-studio\plugins\*\bin\64bit; must be exactly one, at the pinned canonical
  path — a second copy is a shadow, #124).
  (gather them read-only off strih/stream via the win-* MCP tools — see
   .claude/commands/drift-guard.md). Any key you omit is reported UNKNOWN.

--compare per-component BUILD SHA + capability keys (#122, opt-in — supply `manifest` to activate):
  manifest (path to the build-under-test's #120 BUNDLE_MANIFEST.json — download it from the
    windows-genlock / windows-genlock-fast artifact for the deployed build),
  obs_dll_sha256 (the deployed obs.dll Get-FileHash SHA256, read live off the box),
  distroav_dll_sha256 (the deployed distroav.dll Get-FileHash SHA256, read live off the box),
  genlock_capability (the live OBS-log genlock marker text — the build-unique
    `genlock: … render tick ENABLED` / `sub-frame jitter reserve` / `timestamp-aligned release`
    lines; a STOCK OBS 32.1.2 emits NONE -> DRIFT even though its version matches).
  With a manifest supplied, an unread live SHA or capability marker is UNKNOWN (exit 11), never a
  silent clean — a wrong build we failed to hash is exactly the false-negative this facet prevents.

--compare WHOLE-BUNDLE byte/SHA key (#121, post-deploy verify — supply `bundle_hashes` with `manifest`):
  bundle_hashes (a comma-separated `relpath=sha256` list of EVERY deployed bundle file's live
    Get-FileHash, read off the box — relpaths forward-slashed, matching the manifest's files[] paths).
    The deploy is "done" only when the live box matches the manifest byte-for-byte: drift-guard walks
    the manifest's files[] and FAILS on ANY mismatch (DRIFT) or any unread file (UNKNOWN) — so a
    partial/corrupted deploy where even one non-DLL file is stale can never pass. Dormant unless
    supplied (the #122 two-DLL contract is unchanged for the hot-swap obs.dll-only verify path).

--compare prod burn key (#246/#257, opt-in — supply `burn_env` to activate):
  burn_env (the prod per-source MEASUREMENT-BURN state, read read-only off the box over OBS
    WebSocket — #257 made the burn a per-source `genlock_burn` bool, no OBS_BURN_* env any more).
    The literal "none" when NO source has genlock_burn=on, or a comma-separated `SOURCE=on` list of
    every source whose burn IS on. The burn is TEST-mode only; ANY source left ON in prod draws QR
    test-burns onto the LIVE broadcast (RUN 235001) -> DRIFT (exit 20). Dormant unless supplied
    (every historic --compare call is unchanged); the /drift-guard command always feeds it. (The key
    name `burn_env` is kept for --compare back-compat; the value is now the genlock_burn state.)

--compare per-source genlock latency keys (#357, calibration-tracking added #390, opt-in — supply
`genlock_source_latency` to activate):
  genlock_source_latency (a comma-separated "SOURCE NAME=latency_ms" list of every genlocked
    source's LIVE effective held-latency, read from the OBS log `genlock-fifo audit 'SOURCE': …
    latency_ms=N …` lines — see genlock_source_latency_from_log). The pinned side is HOST-KEYED
    (genlock_source_latency_strih / _stream): the strih camera-floor sources are pinned as plain
    exact ms values (structural — a drift is a real regression); the stream A/V-align source
    (`NDI 2ME PGM`) is pinned as `range:MIN-MAX` (calibration-tracked, #390 — its correct value
    changes every time the operator re-calibrates #188, so a single hardcoded ms constant goes
    stale; only an egregiously out-of-range value is DRIFT).
  av_sync_calibrated_ms (OPTIONAL, #390 best-effort — the #427-persisted `applied_latency_ms` read
    from `av-sync-last.json` on the OBS box's ProgramData, gathered by the operator/agent since
    drift-guard runs on dev1 and cannot reach that path itself). When supplied, cross-checks the
    LIVE `NDI 2ME PGM` latency against this last-calibrated value (+/-10ms) and flags GENUINE drift
    (e.g. a hand-nudge in the OBS UI since the last calibration) that the sane-range check alone
    would miss. Dormant (range-checked only, no failure) when omitted or the file is unreachable —
    never fails the whole guard on its absence.

--status keys: host, genlock_wall_clock, genlock_capability, burn_env — a read-only ONE-PLACE dump
  of the genlock gate + build marker + burn state (always exit 0; --compare is the fail-loud gate;
  the rich live OBS dock is the separate #188).

Exit codes: 0 = clean, 20 = DRIFT, 11 = some observed value UNKNOWN (incomplete, NOT clean),
1 = usage/IO error.
EOF
}

check_pins() {
  local readme="$1" p_obs="$2" p_distroav="$3" p_ndi="$4" p_fps_strih="$5" p_fps_stream="$6" p_genlock="$7" p_latency="$8" p_plugin="$9"
  local p_src_lat_strih="${10}" p_src_lat_stream="${11}"
  local errs=0
  echo "== drift-guard --check-pins ($readme) =="
  validate_semver   "obs_version"                    "$p_obs"             || errs=$((errs + 1))
  validate_semver   "distroav_version"               "$p_distroav"        || errs=$((errs + 1))
  validate_semver   "ndi_runtime_min"                "$p_ndi"             || errs=$((errs + 1))
  # #459 (Topology v2, was #11 mixed 60/30): both host-keyed output_fps pins MUST be present
  # (strih=30 cut-to-stream-only, stream=30 plain pass-through — the 60fps IMAG role moved to
  # the separate imag-nb box, #458/#463).
  validate_nonempty "output_fps_strih"               "$p_fps_strih"       || errs=$((errs + 1))
  validate_nonempty "output_fps_stream"              "$p_fps_stream"      || errs=$((errs + 1))
  validate_nonempty "genlock_wall_clock"             "$p_genlock"         || errs=$((errs + 1))
  validate_nonempty "ndi_input_latency"              "$p_latency"         || errs=$((errs + 1))
  validate_nonempty "canonical_plugin_path"          "$p_plugin"          || errs=$((errs + 1))
  # #357 per-source genlock FIFO held-latency: both host-keyed pins MUST be present.
  validate_nonempty "genlock_source_latency_strih"   "$p_src_lat_strih"   || errs=$((errs + 1))
  validate_nonempty "genlock_source_latency_stream"  "$p_src_lat_stream"  || errs=$((errs + 1))
  # #390: any `range:MIN-MAX` calibration-tracked entry in either pin must match the code's
  # current DistroAV clamp EXACTLY — catches a manifest range typo silently narrowing/widening
  # the backstop, independent of the plain non-empty checks above.
  validate_source_latency_range "genlock_source_latency_strih"  "$p_src_lat_strih"  || errs=$((errs + 1))
  validate_source_latency_range "genlock_source_latency_stream" "$p_src_lat_stream" || errs=$((errs + 1))
  if [ "$errs" -gt 0 ]; then
    echo >&2
    echo "!! $errs pinned value(s) missing or malformed in $readme." >&2
    echo "!! The drift guard cannot enforce an incomplete pin set — fix the manifest." >&2
    return 1
  fi
  echo
  echo "All pins present + well-formed:"
  echo "  obs=$p_obs distroav=$p_distroav ndi_min=$p_ndi output_fps_strih=$p_fps_strih output_fps_stream=$p_fps_stream genlock_wall_clock=$p_genlock ndi_input_latency=$p_latency"
  echo "  canonical_plugin_path=$p_plugin"
  echo "  genlock_source_latency_strih=$p_src_lat_strih  genlock_source_latency_stream=$p_src_lat_stream"

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
  local host="$1" p_obs="$2" p_distroav="$3" p_ndi="$4" p_fps="$5" p_genlock="$6" p_latency="$7" p_plugin="$8"
  local o_obs="$9" o_distroav="${10}" o_ndi="${11}" o_fps="${12}" o_genlock="${13}" o_latency="${14}" o_plugin="${15}"
  # #122 build-SHA + capability facet (opt-in when a bundle manifest is supplied):
  local manifest="${16}" o_obs_sha="${17}" o_distroav_sha="${18}" o_capability="${19}"
  # #121 whole-bundle byte/SHA facet (opt-in when bundle_hashes= is also supplied alongside manifest):
  local o_bundle_hashes="${20}"
  # #246 prod burn-env facet (opt-in when burn_env= is supplied):
  local o_burn="${21:-}"
  # #357 per-source genlock FIFO held-latency (opt-in when genlock_source_latency= is supplied):
  local p_src_lat_strih="${22:-}" p_src_lat_stream="${23:-}" o_src_latency="${24:-}"
  # #390 best-effort cross-check against the #427-persisted last-calibrated value (opt-in when
  # av_sync_calibrated_ms= is supplied; dormant otherwise — see drift_check_calibrated_source_latency):
  local o_av_sync_calibrated_ms="${25:-}"

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

  # Per-input NDI ingest latency (#84): every genlocked broadcast-path input must run the pinned
  # Normal(0) mode (the certified low-latency zero-loss pin). drift_check_inputs prints one line per
  # observed input and rolls up to OK/DRIFT/UNKNOWN, so a single drifted input (the failure this
  # guard exists to catch) fails the box.
  rc=0
  drift_check_inputs "$p_latency" "$o_latency" || rc=$?
  [ "$rc" -eq 2 ] && drift=$((drift + 1))
  [ "$rc" -eq 3 ] && unknown=$((unknown + 1))

  # Single canonical OBS plugin-load path (#124): distroav.dll must exist in EXACTLY ONE OBS scan
  # path, and that path must be the pinned canonical one. A second copy in another scan path can
  # silently shadow the intended genlock/DistroAV build (the mixed-version incident #119).
  rc=0
  drift_check_plugin_paths "$p_plugin" "$o_plugin" || rc=$?
  [ "$rc" -eq 2 ] && drift=$((drift + 1))
  [ "$rc" -eq 3 ] && unknown=$((unknown + 1))

  # Per-component BUILD SHA + genlock capability (#122, EPIC #125). The marketing-version checks
  # above pass a STOCK OBS 32.1.2 — byte-for-byte a different build from our genlock 32.1.2, but the
  # identical version (the #119/#120 wrong-build-right-version that silently shipped). This facet
  # compares the LIVE rig's obs.dll/distroav.dll Get-FileHash against the #120 bundle manifest's
  # recorded sha256 AND asserts the genlock capability marker only our build emits is present, so a
  # stock/wrong build is DRIFT even when every version + setting matches. It is OPT-IN: it runs only
  # when a manifest is supplied (the operator/agent downloads the build-under-test's
  # BUNDLE_MANIFEST.json — see .claude/commands/drift-guard.md). Without a manifest the engine keeps
  # the historic marketing-version-only contract. With a manifest, an UNREAD live SHA/capability is
  # UNKNOWN, never a silent clean.
  if [ -n "$manifest" ]; then
    if [ ! -f "$manifest" ]; then
      echo "!! --compare manifest not found: $manifest" >&2
      exit 1
    fi
    # The #122 per-component obs.dll/distroav.dll SHA checks run ONLY when the #121 whole-bundle facet
    # is NOT active. When `bundle_hashes=` is supplied, drift_check_all_files (below) verifies EVERY
    # bundle file — including obs.dll + distroav.dll by their exact path — so the two-DLL checks here
    # would be redundant AND would falsely demand the separate obs_dll_sha256/distroav_dll_sha256 keys
    # (UNKNOWN) that the whole-bundle scan already covers. So #121 supersedes them; #122's hot-swap
    # obs.dll-only verify (no full file set) is preserved when bundle_hashes is absent.
    if [ -z "$o_bundle_hashes" ]; then
      local m_obs_sha m_distroav_sha
      m_obs_sha="$(manifest_sha_for_component "$manifest" obs)"
      m_distroav_sha="$(manifest_sha_for_component "$manifest" distroav)"

      # obs.dll build SHA — the libobs core our genlock patches live in. The manifest must list it;
      # if it does not, the manifest is unusable for this check (UNKNOWN, never a false clean).
      if [ -z "$m_obs_sha" ]; then
        printf '  %-20s UNKNOWN  (manifest %s lists no obs.dll sha256)\n' "obs_dll_sha256" "$manifest"
        unknown=$((unknown + 1))
      else
        rc=0
        drift_check "obs_dll_sha256" exact "$m_obs_sha" "$o_obs_sha" || rc=$?
        [ "$rc" -eq 2 ] && drift=$((drift + 1))
        [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
      fi

      # distroav.dll build SHA — only checked when the manifest carries it (the hot-swap fast-dll
      # bundle ships obs.dll only). A manifest that lists distroav.dll demands the live SHA; the live
      # SHA observed without a manifest entry is reported, not silently dropped.
      if [ -n "$m_distroav_sha" ]; then
        rc=0
        drift_check "distroav_dll_sha256" exact "$m_distroav_sha" "$o_distroav_sha" || rc=$?
        [ "$rc" -eq 2 ] && drift=$((drift + 1))
        [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
      elif [ -n "$o_distroav_sha" ]; then
        # #237: the supplied distroav SHA is NOT compared here (this is an obs.dll-only manifest), so
        # it must be labeled SKIPPED — NOT OK. Calling an UNCHECKED value "OK" misleads an operator
        # into believing distroav was verified. SKIPPED != DRIFT/UNKNOWN, so the verdict stays NO
        # DRIFT (distroav is verified vs its full bundle in a separate invocation per /drift-guard).
        printf '  %-20s SKIPPED  (observed %s; not in this obs.dll-only manifest — verify distroav vs its full bundle)\n' \
          "distroav_dll_sha256" "$o_distroav_sha"
      fi
    fi

    # genlock capability marker — the build-unique tell that distinguishes our build from a stock
    # 32.1.2 even if the bytes were swapped without updating the manifest.
    rc=0
    drift_check_capability "$o_capability" || rc=$?
    [ "$rc" -eq 2 ] && drift=$((drift + 1))
    [ "$rc" -eq 3 ] && unknown=$((unknown + 1))

    # #121 WHOLE-BUNDLE byte/SHA verify (opt-in: runs when bundle_hashes= is supplied alongside the
    # manifest). The #122 facet above checks only the two genlock-bearing DLLs; #121 raises the bar to
    # deploy-from-clean-tree's contract — EVERY file the bundle shipped must match the manifest
    # byte-for-byte on the live box, so a partial/corrupted deploy (one non-DLL file silently stale)
    # can never pass. The post-deploy step gathers every deployed file's Get-FileHash off the box into
    # a `relpath=sha256,…` list and passes it here; drift_check_all_files walks the manifest's files[]
    # and FAILS on any mismatch (DRIFT) or any unread file (UNKNOWN, never a silent clean). Without
    # bundle_hashes= this facet is dormant and the #122 two-DLL contract is unchanged (the hot-swap
    # obs.dll-only verify path).
    if [ -n "$o_bundle_hashes" ]; then
      rc=0
      drift_check_all_files "$manifest" "$o_bundle_hashes" || rc=$?
      [ "$rc" -eq 2 ] && drift=$((drift + 1))
      [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
    fi
  fi

  # Prod burn guard (#246/#257): the measurement burn is TEST-mode only; ANY prod source left with
  # genlock_burn=on draws QR test-burns onto the LIVE broadcast (RUN 235001). #257 made it a
  # per-source bool over OBS WebSocket (no OBS_BURN_* env). Opt-in: runs only when burn_env= is
  # supplied (so every historic --compare call is unchanged); the /drift-guard command always feeds
  # it for the prod boxes (none / a `SOURCE=on` list, gathered over WS).
  if [ -n "$o_burn" ]; then
    rc=0
    drift_check_burn_env "$o_burn" || rc=$?
    [ "$rc" -eq 2 ] && drift=$((drift + 1))
    [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
  fi

  # Per-source genlock FIFO held-latency (#357, calibration-tracked range mode added #390): the
  # effective latency each OBS source holds in the genlock FIFO must match its pinned value. OPT-IN:
  # runs only when genlock_source_latency= is supplied (preserving backward compat for all historic
  # --compare calls). The pin is HOST-KEYED (genlock_source_latency_strih vs _stream) because stream
  # deliberately holds NDI 2ME PGM at a deliberate A/V-align latency (calibration-tracked, #390 —
  # NOT a fixed constant) while strih holds all camera inputs at the 3ms global floor (structural).
  if [ -n "$o_src_latency" ]; then
    local p_src_lat range_source
    case "$host" in
      strih)  p_src_lat="$p_src_lat_strih" ;;
      stream) p_src_lat="$p_src_lat_stream" ;;
      *)      p_src_lat="" ;;
    esac
    if [ -z "$p_src_lat" ]; then
      printf '  %-20s UNKNOWN  (no per-source latency pin for host=%s)\n' "genlock_src_latency" "$host"
      unknown=$((unknown + 1))
    else
      rc=0
      drift_check_source_latency "$p_src_lat" "$o_src_latency" || rc=$?
      [ "$rc" -eq 2 ] && drift=$((drift + 1))
      [ "$rc" -eq 3 ] && unknown=$((unknown + 1))

      # #390 best-effort cross-check against the #427-persisted last-calibrated value. Only
      # meaningful when this host actually has a calibration-tracked (range:) source pinned
      # (today: stream's `NDI 2ME PGM`) — dormant on strih (no range-tracked source) and dormant
      # when the caller did not supply av_sync_calibrated_ms= (never fails the guard on its
      # absence; see drift_check_calibrated_source_latency).
      range_source="$(range_tracked_source_name "$p_src_lat")"
      if [ -n "$range_source" ]; then
        rc=0
        drift_check_calibrated_source_latency "$range_source" "$o_src_latency" \
          "$o_av_sync_calibrated_ms" "$AV_SYNC_CALIBRATION_TOLERANCE_MS" || rc=$?
        [ "$rc" -eq 2 ] && drift=$((drift + 1))
        [ "$rc" -eq 3 ] && unknown=$((unknown + 1))
      fi
    fi
  fi

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

# status_surface HOST O_GENLOCK O_CAPABILITY O_BURN -> the #246 read-only ONE-PLACE state surface.
# Prints the genlock master-gate state, the genlock build-unique capability marker, and the burn
# state from the SAME observed key=val inputs --compare reads — so an operator never needs ad-hoc
# PEB/env reads to answer "is genlock on? is this our build? are burns clean?". This is
# INFORMATIONAL (always exit 0): --compare is the fail-loud drift gate; the rich LIVE OBS dock is
# the separate #188. An unread input is shown UNKNOWN (never silently omitted).
status_surface() {
  local host="$1" o_genlock="$2" o_capability="$3" o_burn="$4"
  echo "== drift-guard --status  host=${host:-?}  (read-only genlock + burn state) =="

  # genlock master gate (the wall-clock render tick). #257: this is a BUILD DEFAULT (always on, no
  # env) proven by the capability marker below; the genlock_wall_clock value is the build-default
  # sentinel "1" (no longer an OBS_GENLOCK_WALL_CLOCK env read).
  if [ -z "$o_genlock" ]; then
    printf '  %-20s UNKNOWN  (genlock gate not read)\n' "genlock_gate"
  elif [ "$o_genlock" = "1" ]; then
    printf '  %-20s ENABLED  (wall-clock render tick on — build default, #257)\n' "genlock_gate"
  else
    printf '  %-20s DISABLED (observed %s)\n' "genlock_gate" "$o_genlock"
  fi

  # genlock build-unique capability marker (our build vs a stock/wrong build).
  if [ -z "$o_capability" ]; then
    printf '  %-20s UNKNOWN  (capability marker not read)\n' "genlock_build"
  elif [ "$(genlock_capability_from_log "$o_capability")" = "1" ]; then
    printf '  %-20s OUR-BUILD (genlock capability marker present)\n' "genlock_build"
  else
    printf '  %-20s STOCK?   (no genlock capability marker in the supplied log)\n' "genlock_build"
  fi

  # burn state (#246/#257) — the one fact that draws QR onto the live broadcast if wrong. #257: the
  # per-source genlock_burn state (read over WS), not OBS_BURN_* Machine env.
  if [ -z "$o_burn" ]; then
    printf '  %-20s UNKNOWN  (burn state not read)\n' "burn_env"
  elif [ "$o_burn" = "none" ]; then
    printf '  %-20s CLEAN    (no source genlock_burn=on)\n' "burn_env"
  else
    printf '  %-20s SET!     (%s)\n' "burn_env" "$o_burn"
  fi

  echo
  echo "(read-only status; '--compare burn_env=…' is the fail-loud gate; the live OBS dock is #188.)"
  return 0
}

main() {
  local mode="check-pins" readme="$DEFAULT_README"
  local -a kv=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --check-pins) mode="check-pins" ;;
      --compare)    mode="compare" ;;
      --status)     mode="status" ;;
      --check-imag) mode="check-imag" ;;  # #463: SSH-gathered imag-nb host case
      --readme)     shift; readme="${1:-}" ;;
      -h|--help)    usage; exit 0 ;;
      --*)          echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)            kv+=("$1") ;;   # key=val observed pairs for --compare / --check-imag
    esac
    shift || true
  done

  # #463 — --check-imag: imag is a plain Linux box, so drift-guard gathers its OWN observed
  # values over SSH (no external MCP round-trip needed, unlike strih/stream). `host=` / `user=`
  # override the imag-nb defaults (10.77.9.182 / newlevel, scripts/setup-imag.sh's DESKTOP_USER).
  if [ "$mode" = "check-imag" ]; then
    [ -f "$readme" ] || { echo "ERROR: manifest not found: $readme (run from repo root)" >&2; exit 1; }
    local imag_host="10.77.9.182" imag_user="newlevel" pair k v
    for pair in "${kv[@]+"${kv[@]}"}"; do
      k="${pair%%=*}"; v="${pair#*=}"
      case "$k" in
        host) imag_host="$v" ;;
        user) imag_user="$v" ;;
        *)    echo "WARN: ignoring unknown --check-imag key '$k'" >&2 ;;
      esac
    done
    gather_and_check_imag "$imag_host" "$imag_user" "$readme"
    exit $?
  fi

  # --status is a read-only dump of the live genlock + burn state — it needs NONE of the pinned
  # set, so skip the manifest requirement + pin load for it (it must work even without
  # vendor/README.md, e.g. a checkout that only ships the script). #246.
  local p_obs p_distroav p_ndi p_fps p_fps_strih p_fps_stream p_genlock p_latency p_plugin p_src_lat_strih p_src_lat_stream
  if [ "$mode" != "status" ]; then
    [ -f "$readme" ] || { echo "ERROR: manifest not found: $readme (run from repo root)" >&2; exit 1; }
    p_obs="$(pinned_obs_version "$readme")"
    p_distroav="$(pinned_distroav_version "$readme")"
    p_ndi="$(pinned_ndi_min "$readme")"
    # #459 (was #11 mixed 60/30): output_fps is HOST-KEYED (strih=30 cut-to-stream-only,
    # stream=30 plain pass-through). The per-host pin is resolved from the `host` arg in
    # --compare (below); --check-pins validates BOTH are present so no future box silently
    # defaults to the wrong fps.
    p_fps_strih="$(pinned_setting "$readme" output_fps_strih)"
    p_fps_stream="$(pinned_setting "$readme" output_fps_stream)"
    p_genlock="$(pinned_setting "$readme" genlock_wall_clock)"
    p_latency="$(pinned_setting "$readme" ndi_input_latency)"
    p_plugin="$(pinned_setting "$readme" canonical_plugin_path)"
    # #357 host-keyed per-source genlock FIFO held-latency pins.
    p_src_lat_strih="$(pinned_setting "$readme" genlock_source_latency_strih)"
    p_src_lat_stream="$(pinned_setting "$readme" genlock_source_latency_stream)"
    if [ "$mode" = "check-pins" ]; then
      check_pins "$readme" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps_strih" "$p_fps_stream" "$p_genlock" "$p_latency" "$p_plugin" \
        "$p_src_lat_strih" "$p_src_lat_stream"
      exit $?
    fi
  fi

  # --compare / --status: collect observed key=val pairs (both facets read the same inputs).
  local host="" o_obs="" o_distroav="" o_ndi="" o_fps="" o_genlock="" o_latency="" o_plugin="" pair k v
  local manifest="" o_obs_sha="" o_distroav_sha="" o_capability="" o_bundle_hashes="" o_burn="" o_src_latency=""
  local o_av_sync_calibrated_ms=""
  for pair in "${kv[@]+"${kv[@]}"}"; do
    k="${pair%%=*}"; v="${pair#*=}"
    case "$k" in
      host)               host="$v" ;;
      obs_version)        o_obs="$v" ;;
      distroav_version)   o_distroav="$v" ;;
      ndi_runtime)        o_ndi="$v" ;;
      output_fps)         o_fps="$v" ;;
      genlock_wall_clock) o_genlock="$v" ;;
      ndi_input_latency)  o_latency="$v" ;;
      distroav_dll_paths) o_plugin="$v" ;;
      manifest)           manifest="$v" ;;       # #122: BUNDLE_MANIFEST.json of the build under test
      obs_dll_sha256)     o_obs_sha="$v" ;;      # #122: live Get-FileHash of the deployed obs.dll
      distroav_dll_sha256) o_distroav_sha="$v" ;; # #122: live Get-FileHash of the deployed distroav.dll
      genlock_capability) o_capability="$v" ;;   # #122: the live OBS-log genlock marker text
      bundle_hashes)      o_bundle_hashes="$v" ;; # #121: live `relpath=sha256,…` of every deployed bundle file
      burn_env)              o_burn="$v" ;;         # #246: prod burn-env state ("none" or NAME=VALUE,…)
      genlock_source_latency) o_src_latency="$v" ;; # #357: per-source genlock held-latency CSV
      av_sync_calibrated_ms) o_av_sync_calibrated_ms="$v" ;; # #390: #427-persisted applied_latency_ms
      *)                  echo "WARN: ignoring unknown observed key '$k'" >&2 ;;
    esac
  done

  if [ "$mode" = "status" ]; then
    status_surface "$host" "$o_genlock" "$o_capability" "$o_burn"
    exit $?
  fi

  # #459 (was #11 mixed 60/30): output_fps is HOST-KEYED — resolve the pin for THIS box. An
  # unknown/empty host has no pin → FAIL LOUDLY so no future box silently defaults to the wrong
  # fps (strih=30 cut-to-stream-only, stream=30 plain pass-through).
  if [ -z "$host" ]; then
    echo "ERROR: --compare requires host= (output_fps is host-keyed: output_fps_strih / output_fps_stream)." >&2
    exit 1
  fi
  p_fps="$(pinned_setting "$readme" "output_fps_${host}")"
  if [ -z "$p_fps" ]; then
    echo "ERROR: no output_fps pin for host '${host}' (expected an 'output_fps_${host}' row in $readme; known hosts: strih, stream)." >&2
    exit 1
  fi

  compare_observed "$host" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps" "$p_genlock" "$p_latency" "$p_plugin" \
    "$o_obs" "$o_distroav" "$o_ndi" "$o_fps" "$o_genlock" "$o_latency" "$o_plugin" \
    "$manifest" "$o_obs_sha" "$o_distroav_sha" "$o_capability" "$o_bundle_hashes" "$o_burn" \
    "$p_src_lat_strih" "$p_src_lat_stream" "$o_src_latency" "$o_av_sync_calibrated_ms"
}

main "$@"
