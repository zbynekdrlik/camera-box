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
#     the genlock build SHA marker, the deployed distroav.so hash, the OBS log's genlock
#     capability + fps + latency lines, and (since #596) the SAME #591 sole-timesync-authority
#     verdict verify-device.sh hard-gates on the cam1-6 fleet — and compares them against the
#     imag-specific pins in vendor/README.md. Simpler than the Windows path by construction
#     (SSH IS the gathering).
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
#   scripts/drift-guard.sh --compare host=strih obs_version=32.2.0 \
#       distroav_version=6.2.1 ndi_runtime=6.3.2.0 output_fps=30 genlock_wall_clock=1 \   # host=strih→30, host=stream→30 (#459, was strih→60/#11)
#       ndi_input_latency="NDI cam5=0,NDI cam1=0,NDI cam3=0" \
#       distroav_dll_paths="C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"
#   scripts/drift-guard.sh --help
#
# Exit codes: 0 = clean (pins valid / no drift), 20 = DRIFT detected, 11 = at least one observed
# value UNKNOWN (drift status incomplete — never reported as clean), 1 = usage/IO error.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# scripts/lib/timesync-authority.sh is sourced ONLY for its pure functions (dpkg_status_installed/
# timesync_daemon_verdict/timesync_authority_verdict, #591) -- it performs no side effects of its
# own. Shared with scripts/verify-device.sh's (r) fleet check so the --check-imag facet below
# (imag-nb) runs the IDENTICAL sole-timesync-authority verdict instead of a driftable copy (#596).
# shellcheck source=scripts/lib/timesync-authority.sh
. "$HERE/lib/timesync-authority.sh"
# scripts/lib/imag-power-envelope.sh is sourced ONLY for its pure functions
# (imag_power_envelope_verdict + imag_power_envelope_gather_remote_snippet, #1040) — same
# extraction discipline as timesync-authority.sh, SHARED with scripts/verify-imag.sh so the
# --check-imag power-envelope facet below runs the IDENTICAL verdict instead of a driftable copy.
# shellcheck source=scripts/lib/imag-power-envelope.sh
. "$HERE/lib/imag-power-envelope.sh"
# scripts/lib/imag-display-path.sh is sourced ONLY for its pure functions
# (imag_display_path_verdict + imag_display_path_gather_remote_snippet, #780) — same extraction
# discipline as timesync-authority.sh / imag-power-envelope.sh, SHARED with the E2E [0/8] preflight
# so the --check-imag display-path facet below runs the IDENTICAL verdict instead of a driftable copy.
# shellcheck source=scripts/lib/imag-display-path.sh
. "$HERE/lib/imag-display-path.sh"
# scripts/lib/imag-cmdline-isolation.sh is sourced ONLY for its pure functions
# (imag_cmdline_isolation_verdict + imag_cmdline_isolation_gather_remote_snippet, #784) — same
# extraction discipline as imag-display-path.sh / imag-power-envelope.sh; the --check-imag
# cmdline-isolation facet below (check #11) runs the pure verdict instead of a driftable inline copy.
# shellcheck source=scripts/lib/imag-cmdline-isolation.sh
. "$HERE/lib/imag-cmdline-isolation.sh"
# scripts/lib/obs-projector-vsync.sh is sourced ONLY for its pure functions
# (projector_vsync_verdict + projector_vsync_armed_from_log, #1151) — same extraction discipline as
# imag-display-path.sh / imag-cmdline-isolation.sh, SHARED with the E2E [0/8] preflight so the
# --check-imag projector-vsync facet below runs the IDENTICAL verdict against ONE marker string.
# shellcheck source=scripts/lib/obs-projector-vsync.sh
. "$HERE/lib/obs-projector-vsync.sh"

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

# obs_version_from_log TEXT -> "32.2.0"  (OBS log header line "OBS 32.2.0 (64-bit, windows)").
obs_version_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*OBS \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' | head -1 || true
}

# distroav_version_from_log TEXT -> "6.2.1"  ("you can haz DistroAV (Version 6.2.1)").
distroav_version_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*DistroAV (Version \([0-9][0-9.]*\)).*/\1/p' | head -1 || true
}

# ndi_runtime_from_log TEXT -> "6.3.2.0"  ("[distroav] NDI Library Version detected: 6.3.2.0").
ndi_runtime_from_log() {
  printf '%s\n' "$1" \
    | sed -n 's/.*NDI Library Version detected: \([0-9][0-9.]*\).*/\1/p' | head -1 || true
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
  # #1184: LC_ALL=C grep -a -> byte-literal, so invalid-UTF-8 bytes (DistroAV mojibake) in the OBS
  # log cannot suppress a marker that IS present when this greps locally in a UTF-8 locale.
  line="$(printf '%s\n' "$text" \
    | LC_ALL=C grep -aiE 'genlock:.*render tick (ENABLED|DISABLED)' | head -1 || true)"
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
    }' || true
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
      | sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' || true
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
  # #1184: LC_ALL=C grep -a -> byte-literal, invalid-UTF-8-safe (same class as #1183); this reads
  # remote OBS-log text but greps LOCALLY (dev1's UTF-8 locale), so it needs the byte-literal match.
  line="$(printf '%s\n' "$text" \
    | LC_ALL=C grep -aiE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' \
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
  # #1184: LC_ALL=C grep -a -> byte-literal, invalid-UTF-8-safe (same class as #1183). The `sed`
  # ALSO needs LC_ALL=C: grep -a passes the raw invalid bytes through, and sed in a UTF-8 locale
  # chokes on them and fails the extraction (returns the whole mangled line instead of the digits).
  printf '%s\n' "$1" \
    | LC_ALL=C grep -aiE 'genlock:.*latency = [0-9]+ ms' \
    | LC_ALL=C sed -n 's/.*latency = \([0-9][0-9]*\) ms.*/\1/p' | head -1 || true
}

# genlock_rt_pin_from_log TEXT -> "ok" if imag-nb's OBS log shows the genlock render-tick thread
# achieved SCHED_FIFO (#484: vendor/obs-studio/libobs/obs-video.c genlock_pin_render_tick_thread,
# Linux-only — the render tick is pinned SCHED_FIFO prio 10 on the isolated nohz_full cores so it
# gets on-time wakeups), "failed" if the log shows the WARN-and-continue SCHED_OTHER fallback (the
# syscall failed, almost always a missing rtprio ulimit grant) — the EXACT #572 root cause: imag-nb
# ran an entire recording on ordinary SCHED_OTHER and lost 35 single-tick 60fps render deadlines.
# "" (UNKNOWN/absent) when TEXT carries NEITHER line — the log was never read, or the deployed
# build predates #484 (a stale build is a SEPARATE facet, imag_build_drift_report's dynamic
# origin/main compare; this parser only judges the pin OUTCOME a #484-or-later build always logs
# unconditionally on Linux, so silently returning UNKNOWN rather than guessing keeps the two
# facets from double-diagnosing the same staleness with two different reasons). PURE — no I/O.
# Drain-safe (grep|head, never grep -q under this file's set -euo pipefail — same SIGPIPE hazard
# genlock_capability_from_log's own comment documents: `grep -q` can flip a genuine match into a
# false non-match when the upstream `printf` is SIGPIPE'd after grep's early exit).
genlock_rt_pin_from_log() {
  local text="$1" ok_line failed_line
  # #1184: LC_ALL=C grep -a -> byte-literal, invalid-UTF-8-safe (same class as #1183).
  ok_line="$(printf '%s\n' "$text" \
    | LC_ALL=C grep -aiE 'genlock: render-tick thread set SCHED_FIFO prio [0-9]+ on the isolated core' \
    | head -1 || true)"
  if [ -n "$ok_line" ]; then
    printf 'ok\n'
    return 0
  fi
  failed_line="$(printf '%s\n' "$text" \
    | LC_ALL=C grep -aiE 'genlock: could NOT set render-tick thread SCHED_FIFO' \
    | head -1 || true)"
  [ -n "$failed_line" ] && printf 'failed\n'
  return 0
}

# dantesync_locked_from_log DANTESYNC_JOURNAL_TEXT -> "locked" if a PTP LOCK/NANO or NTP-offset
# line is present -- the SAME markers `scripts/setup-imag.sh`'s own provisioning-time dantesync
# restart check keys on (setup-imag.sh:230, `\[PTP\][[:space:]]+(LOCK|NANO)|\[NTP\] offset`).
# "unlocked" when the text is NON-EMPTY (journalctl was read; dantesync is up) but carries no lock
# marker at all -- the genuine drift case (clock never disciplined). "" when the text itself is
# EMPTY (journalctl was never read -- SSH failure, or dantesync was never started) -- UNKNOWN,
# never a false "unlocked" DRIFT for a mere connectivity hiccup. PURE -- no I/O -- mirrors
# fps_from_log / genlock_latency_ms_from_log's two-tier shape. #489, spun out of #479.
#
# Drain-safe (grep|head, never grep -q -- see genlock_from_log's note above): `grep -q` exits on
# the FIRST match and closes its read end, so a `printf` still writing a large blob AFTER that
# early match can raise SIGPIPE on its next write; under this file's `set -euo pipefail`, an `if`
# testing that pipeline directly is exempt from errexit, so the SCRIPT SURVIVES but the pipeline's
# non-zero exit status flips the `if` to its else branch -- a SILENT WRONG ANSWER ("unlocked" for
# a genuinely-locked box), not a crash. Confirmed empirically (#489 review) against a properly
# materialized ~15.6MB blob (a live SIGPIPE race needs the writer still producing when the reader
# closes -- a synthetic `yes | head` *generator* has its OWN unrelated SIGPIPE hazard between
# `yes` and `head` that can crash a script regardless of any downstream consumer, so any repro
# MUST materialize the test input from a plain string/file, never mid-pipe from `yes`). This is
# the exact drift-guard-goes-silently-wrong failure mode `genlock_from_log`'s own comment already
# documents for the identical pattern -- see that function's note above.
dantesync_locked_from_log() {
  local log_text="$1" line
  if [ -z "$log_text" ]; then
    printf '\n'
    return
  fi
  line="$(printf '%s\n' "$log_text" \
    | grep -iE '\[PTP\][[:space:]]+(LOCK|NANO)|\[NTP\] offset' | head -1 || true)"
  if [ -n "$line" ]; then
    printf 'locked\n'
  else
    printf 'unlocked\n'
  fi
}

# imag_build_drift_report BOX_SHA GIT_RC RANGE_LOG -> the #531 DYNAMIC genlock-build staleness check
# for imag-nb: is the box's DEPLOYED genlock build BEHIND origin/main's vendored-genlock HEAD? This
# REPLACES the pre-#531 static empty-pin compare (the box GENLOCK_BUILD_SHA.txt vs an empty
# genlock_build_sha_imag README pin), which was inert — always UNKNOWN, could never FAIL, so it never
# caught the exact failure it should: a genlock change merged to main that NOBODY deployed to imag.
# The #530 disaster: imag-nb ran a STALE genlock build at a live event -> 45fps. The authoritative
# "what SHOULD be deployed" is origin/main's vendored-genlock HEAD, so the impure caller
# ([`gather_and_check_imag`]) runs `git log <BOX_SHA>..origin/main -- vendor/obs-studio
# vendor/distroav` and passes its output (RANGE_LOG) + exit status (GIT_RC) here; THIS pure function
# decides. NO I/O — directly unit-testable (tests/drift_guard.rs) by mocking BOX_SHA / GIT_RC /
# RANGE_LOG, no live box or real git repo needed.
#
#   BOX_SHA    the commit SHA read from /opt/obs-genlock/GENLOCK_BUILD_SHA.txt on imag-nb ("" = unread)
#   GIT_RC     the `git log` range command's exit status ("0" = ran OK; non-zero = git failed, e.g.
#              BOX_SHA is not a commit in this checkout, or a shallow clone / unreachable fetch)
#   RANGE_LOG  the `git log --oneline BOX_SHA..origin/main -- vendor/obs-studio vendor/distroav`
#              output — one `<short-sha> <subject>` line per genlock-touching commit the box is
#              BEHIND ("" = none = the box is at/after origin/main's genlock HEAD = current)
#
# Two-tier, mirroring the rest of the engine (never a silent clean): BOX_SHA empty -> UNKNOWN (we
# never read the box), GIT_RC != 0 -> UNKNOWN (we could not COMPUTE the drift — never a false OK for
# a git error), RANGE_LOG empty -> OK (current), RANGE_LOG non-empty -> DRIFT (box is behind — FAIL
# LOUD with the count + the stale-commit SHAs + the exact operator action). Returns 0 OK / 20 DRIFT /
# 11 UNKNOWN (the engine's exit-code contract).
genlock_build_drift_report() {
  local box_label="$1" deploy_action="$2" box_sha="$3" git_rc="$4" range_log="$5"
  if [ -z "$box_sha" ]; then
    printf '  %-22s UNKNOWN  (genlock build SHA not read on %s)\n' "genlock_build" "$box_label"
    return 11
  fi
  if [ "$git_rc" != "0" ]; then
    printf '  %-22s UNKNOWN  (could not compare box=%s to origin/main — git log rc=%s; is %s a commit in this checkout, and is the fetch reachable?)\n' \
      "genlock_build" "$box_sha" "$git_rc" "$box_sha"
    return 11
  fi
  if [ -z "$range_log" ]; then
    printf '  %-22s OK       (box=%s is current with origin/main vendored-genlock HEAD)\n' \
      "genlock_build" "$box_sha"
    return 0
  fi
  # Behind: count the genlock-touching commits the box is missing + list their short SHAs. Drain-safe
  # (grep/awk read the whole here-string, never grep -q; `|| true` keeps a no-match from tripping the
  # caller's set -e/pipefail — though range_log is non-empty here by construction).
  local n shas
  n="$(printf '%s\n' "$range_log" | grep -c . || true)"
  shas="$(printf '%s\n' "$range_log" | awk 'NF{print $1}' | paste -sd, - || true)"
  printf '  %-22s DRIFT    (%s genlock STALE: box=%s is %s genlock-commit(s) behind origin/main [%s]; %s)\n' \
    "genlock_build" "$box_label" "$box_sha" "$n" "$shas" "$deploy_action"
  return 20
}

# #531 imag-nb caller + tests use imag_build_drift_report(box_sha, git_rc, range_log); it is now a
# thin box-specific alias over the box-agnostic genlock_build_drift_report. #548 extended the SAME
# dynamic-staleness verdict to strih/stream (whose OBS/DistroAV/NDI version strings are IDENTICAL
# across a stock vs genlock build, so only this deployed-SHA-vs-origin/main compare catches a
# Windows box left on a stale genlock build — the 843-commit deploy-drift #548 exists to prevent).
imag_build_drift_report() {
  genlock_build_drift_report "imag-nb" \
    "deploy the latest build via setup-imag.sh step-12 at a safe off-event time" "$@"
}

# genlock_parity_consumed_paths LABEL -> #949: the vendor/** paths whose content actually SHIPS in
# LABEL's genlock build, mirrored 1:1 off the REAL CI trigger path filters so this table can never
# silently drift out of sync with what actually rebuilds each platform:
#   - "imag"          -> linux-genlock.yml's own `on.push.paths` (vendor/obs-studio/**,
#                        vendor/distroav/** — deliberately NOT vendor/av-sync-dock, a Windows-only
#                        OBS dock DLL imag never links against)
#   - anything else    -> windows-genlock-fast.yml's `on.push.paths` (adds vendor/av-sync-dock/**)
# (see tests/drift_guard.rs's genlock_parity_consumed_paths_matches_the_ci_workflow_path_filters_949
# for the lock-step assertion against the two workflow files themselves.) PURE — a lookup table, no
# I/O. An unrecognized label gets the WINDOWS (superset) set: the fail-closed default that can never
# make a real consumed dir silently invisible to the #949 content-equivalence check below.
genlock_parity_consumed_paths() {
  case "$1" in
    imag) printf 'vendor/obs-studio\nvendor/distroav\n' ;;
    *)    printf 'vendor/obs-studio\nvendor/distroav\nvendor/av-sync-dock\n' ;;
  esac
}

# genlock_parity_equivalent REPO_ROOT SHA_A SHA_B PATH... -> #949 IMPURE git check: do SHA_A and
# SHA_B have byte-IDENTICAL content over the given PATHs, in the git repo at REPO_ROOT?
#
# WHY THIS EXISTS — `genlock_build_parity_report` below used to treat ANY raw-string SHA mismatch
# as a proven fleet skew. But `linux-genlock.yml`'s push trigger deliberately excludes
# vendor/av-sync-dock/** (a Windows-only OBS dock DLL) — so a Windows-only vendor change advances
# strih/stream's deployed GENLOCK_BUILD_SHA.txt to a SHA imag's build can NEVER be produced at
# through the normal trigger, even though imag's actual built bytes never changed (#949; live on
# run 30768287281 / PR #948). This function is how the caller (version-integrity-gate.sh's flow)
# tells a genuine cross-box skew apart from a mere LABEL mismatch: restrict the diff to the
# INTERSECTION of the two boxes' own consumed vendor paths (genlock_parity_consumed_paths above).
#
# Fail-closed (never a false "equivalent"): EITHER sha failing to resolve as a commit in REPO_ROOT
# (a shallow/incomplete checkout, a force-pushed-away commit, a corrupted marker file) is treated
# exactly like a genuine content difference — returns 1 (NOT equivalent), never assumed equivalent
# merely because it could not be checked. No network call happens here (the caller runs `git fetch
# origin` once, up front, before calling this per pair) — a fetch failure there already degrades to
# "may fail to resolve", which this function turns into the same safe 1 (not equivalent), never a
# pass. `--end-of-options` mirrors `imag_genlock_range_log`'s defense against an option-shaped
# (corrupted) SHA value being silently consumed as a git flag instead of a revision.
#
# IMPURE (real git I/O against REPO_ROOT) — like `imag_genlock_range_log`, it needs no live SSH/box
# to unit test: it runs against THIS repo's own checkout (tests/drift_guard.rs passes "$(pwd)"),
# which always has real historical commits to exercise both the equivalent and the genuinely-
# skewed case against real vendor/** history.
genlock_parity_equivalent() {
  local repo_root="$1" sha_a="$2" sha_b="$3"
  shift 3
  local -a paths=("$@")
  git -C "$repo_root" rev-parse --quiet --verify --end-of-options "${sha_a}^{commit}" \
    >/dev/null 2>&1 || return 1
  git -C "$repo_root" rev-parse --quiet --verify --end-of-options "${sha_b}^{commit}" \
    >/dev/null 2>&1 || return 1
  git -C "$repo_root" diff --quiet --end-of-options "$sha_a" "$sha_b" -- "${paths[@]}" 2>/dev/null
}

# genlock_parity_diff_paths REPO_ROOT SHA_A SHA_B PATH... -> #949: the ACTUAL file paths that
# differ between SHA_A and SHA_B, restricted to PATH..., one per line (git diff --name-only).
# Used ONLY to make a genuine DRIFT message actionable (name what changed, not just "SHAs differ")
# once genlock_parity_equivalent has already said "not equivalent" — a caller that only needs the
# yes/no verdict calls genlock_parity_equivalent alone and never pays for this. Same fail-closed
# resolution as genlock_parity_equivalent (an unresolvable sha yields EMPTY output, never a
# fabricated path list) — empty output here means either sha could not be resolved, or (should not
# happen if the caller only calls this after a confirmed non-equivalent verdict) the diff is
# genuinely empty. IMPURE — real git I/O against REPO_ROOT, same convention as
# genlock_parity_equivalent (tests/drift_guard.rs exercises it against this repo's own history).
genlock_parity_diff_paths() {
  local repo_root="$1" sha_a="$2" sha_b="$3"
  shift 3
  local -a paths=("$@")
  git -C "$repo_root" rev-parse --quiet --verify --end-of-options "${sha_a}^{commit}" \
    >/dev/null 2>&1 || return 0
  git -C "$repo_root" rev-parse --quiet --verify --end-of-options "${sha_b}^{commit}" \
    >/dev/null 2>&1 || return 0
  git -C "$repo_root" diff --name-only --end-of-options "$sha_a" "$sha_b" -- "${paths[@]}" 2>/dev/null
}

# genlock_build_parity_report LABEL=SHA... [EQUIV=LABEL_A:LABEL_B]... [DIFF=LABEL_A:LABEL_B:PATHS]...
# -> #756 CROSS-BOX genlock-build PARITY: do the LIVE deployed genlock build SHAs of the fleet
# boxes (imag + strih + stream) all MATCH EACH OTHER?
#
# WHY THIS EXISTS — the false OK `genlock_build_drift_report`/`imag_build_drift_report` cannot
# escape: those compare a box against ORIGIN/MAIN's vendored-genlock HEAD. But the live fleet runs
# DEV-TRAIN builds (the long-lived unmerged PR #704 train) that are AHEAD of origin/main — so every
# box reads "OK: current with origin/main" even while imag is GENERATIONS behind the build actually
# deployed on strih/stream. #530/#756: imag ran a stale lineage, its hot-swapped libobs segfaulted
# and wedged the GPU at ~11fps, and the ref-compare never flagged the skew because a ref compare is
# a false OK BY CONSTRUCTION during any long-lived train (the user's #756 upgrade: "musí kontrolovať
# či sú všade najnovšie verzie"). The ONLY trustworthy assertion is PEER parity: every box's
# DEPLOYED build SHA must be IDENTICAL. Any skew = FAIL, no git ref involved.
#
# Each `LABEL=SHA` arg is the box's live `GENLOCK_BUILD_SHA.txt` (imag: /opt/obs-genlock/…, read
# over ssh; the Windows boxes: the SAME file in the deployed genlock bundle, served by the standing
# :8899 bundle-state service and threaded in by `version-integrity-gate.sh`). A box whose SHA came
# back empty is UNREAD, never silently dropped.
#
# #949 — a raw-string mismatch between two boxes is no longer, by itself, proof of a real skew: a
# Windows-only vendor change can leave imag's LABEL behind even though its actual built bytes are
# unchanged. The caller (version-integrity-gate.sh) resolves this BEFORE calling here, via
# `genlock_parity_equivalent` restricted to each pair's consumed-path intersection, and passes the
# verdict as an `EQUIV=LABEL_A:LABEL_B` marker (order-independent) for every pair it proved
# content-identical. This function stays PURE — no I/O — it only consumes the pre-computed EQUIV
# markers; a mismatched pair with NO marker is treated exactly as before #949: a real, unexplained
# skew, DRIFT. An OPTIONAL `DIFF=LABEL_A:LABEL_B:PATHS` marker (PATHS = comma-joined file paths,
# from genlock_parity_equivalent's sibling genlock_parity_diff_paths) makes an unexplained skew's
# DRIFT message name the actual files that differ, not just the two boxes' opaque SHAs — the issue
# this facet exists for is otherwise "hard to act on" (#949).
#
# Verdict ordering is fail-closed, never a silent clean (the engine's exit-code contract, matching
# every other facet): a DEFINITE, UNEXPLAINED skew among the boxes we COULD read WINS (report it
# even if a third box is unread — a proven skew is a proven skew); else any unread box OR fewer than
# two read peers is UNKNOWN (parity can't be certified — never a false OK for an incomplete
# picture); else every box read and every pair either byte-identical or EQUIV-covered is OK. Returns
# 0 OK / 20 DRIFT / 11 UNKNOWN.
genlock_build_parity_report() {
  local -a read_labels=() read_shas=() unread=() equiv_pairs=() diff_pairs=()
  local arg label sha pairs=""
  for arg in "$@"; do
    case "$arg" in
      EQUIV=*)
        equiv_pairs+=("${arg#EQUIV=}")
        continue
        ;;
      DIFF=*)
        diff_pairs+=("${arg#DIFF=}")
        continue
        ;;
    esac
    label="${arg%%=*}"
    sha="${arg#*=}"
    if [ -z "$sha" ]; then
      unread+=("$label")
    else
      read_labels+=("$label")
      read_shas+=("$sha")
      pairs="${pairs:+$pairs, }${label}=${sha}"
    fi
  done

  # 1. A DEFINITE, UNEXPLAINED skew among the boxes we COULD read wins — even if another box is
  #    unread. A proven fleet split must FAIL LOUD regardless of a third box's readability. A pair
  #    whose raw SHAs differ but which the caller already proved content-equivalent (an EQUIV
  #    marker naming this exact pair, either order) is NOT a skew — #949.
  if [ "${#read_shas[@]}" -ge 2 ]; then
    local i j e is_equiv skew=0 skew_pairs="" n="${#read_shas[@]}"
    local pair_note dla drest dlb dpths dpaths
    for ((i = 0; i < n; i++)); do
      for ((j = i + 1; j < n; j++)); do
        [ "${read_shas[$i]}" = "${read_shas[$j]}" ] && continue
        is_equiv=0
        for e in "${equiv_pairs[@]}"; do
          if [ "$e" = "${read_labels[$i]}:${read_labels[$j]}" ] \
            || [ "$e" = "${read_labels[$j]}:${read_labels[$i]}" ]; then
            is_equiv=1
            break
          fi
        done
        [ "$is_equiv" -eq 1 ] && continue
        skew=1
        pair_note="${read_labels[$i]}~${read_labels[$j]}"
        # #949: if the caller supplied a matching DIFF= marker, name the actual differing paths
        # (never fabricated — only appended when the caller's own git-diff already found them).
        dpaths=""
        for e in "${diff_pairs[@]}"; do
          dla="${e%%:*}"; drest="${e#*:}"
          dlb="${drest%%:*}"; dpths="${drest#*:}"
          if { [ "$dla" = "${read_labels[$i]}" ] && [ "$dlb" = "${read_labels[$j]}" ]; } \
            || { [ "$dla" = "${read_labels[$j]}" ] && [ "$dlb" = "${read_labels[$i]}" ]; }; then
            dpaths="$dpths"
            break
          fi
        done
        [ -n "$dpaths" ] && pair_note="${pair_note} [changed: ${dpaths}]"
        skew_pairs="${skew_pairs:+$skew_pairs, }${pair_note}"
      done
    done
    if [ "$skew" -eq 1 ]; then
      printf '  %-22s DRIFT    (genlock build SKEW across the fleet — boxes are NOT on one lineage: %s; unexplained skew: %s; hot-swap the current linux-genlock build onto the lagging box(es) [#460 runbook] so the whole fleet runs ONE build)\n' \
        "genlock_parity" "$pairs" "$skew_pairs"
      return 20
    fi
  fi

  # 2. Incomplete — a box we could not read, or fewer than two peers to compare. Never a false clean.
  if [ "${#unread[@]}" -gt 0 ] || [ "${#read_shas[@]}" -lt 2 ]; then
    printf '  %-22s UNKNOWN  (cross-box genlock parity INCOMPLETE — read [%s]%s; need every fleet box read + >=2 peers to certify one lineage)\n' \
      "genlock_parity" "${pairs:-none}" \
      "$([ "${#unread[@]}" -gt 0 ] && printf '; UNREAD: %s' "${unread[*]}")"
    return 11
  fi

  # 3. Every box read; every pair is either byte-identical or EQUIV-proven content-identical -> ONE
  #    effective lineage. Keep the ORIGINAL wording (existing callers grep for it) for the plain
  #    all-byte-identical fast path; use a #949-specific wording only when labels genuinely differ.
  local all_identical=1 k
  for ((k = 1; k < ${#read_shas[@]}; k++)); do
    [ "${read_shas[$k]}" != "${read_shas[0]}" ] && all_identical=0
  done
  if [ "$all_identical" -eq 1 ]; then
    printf '  %-22s OK       (all %s fleet boxes on ONE genlock build: %s)\n' \
      "genlock_parity" "${#read_shas[@]}" "${read_shas[0]}"
  else
    printf '  %-22s OK       (all %s fleet boxes in genlock-build PARITY — labels differ but every pair is content-identical over its consumed vendor paths [#949]: %s)\n' \
      "genlock_parity" "${#read_shas[@]}" "$pairs"
  fi
  return 0
}

# check_imag_report EXP_DISTROAV_SHA OBS_DISTROAV_SHA EXP_FPS OBS_FPS
#   EXP_LATENCY_MS OBS_LATENCY_MS OBS_LOG_TEXT EXP_PLUGIN_PATH PLUGIN_PATH_PRESENT
#   EXP_DANTESYNC_LOCKED OBS_DANTESYNC_LOG_TEXT OBS_TIMESYNC_STATES
#   -> the #463 imag-nb (Topology v2, EPIC #466) host case. #531: the genlock BUILD-IDENTITY check
#   (was check #1 here — the inert static empty-pin compare) moved OUT to imag_build_drift_report
#   above, which [`gather_and_check_imag`] runs ALONGSIDE this report; so this function now covers the
#   SSH-gathered LIVE-state pins only. PURE — every value is already
#   gathered (by [`gather_and_check_imag`], which does the actual SSH), so this function has NO
#   I/O and is directly unit-testable, mirroring `compare_observed`'s pure-report-from-inputs
#   shape. Prints one report line per check (OK / DRIFT / UNKNOWN, matching the strih/stream
#   report style) and returns 0 = clean, 20 = DRIFT, 11 = at least one UNKNOWN (never a silent
#   pass — same exit-code contract as the rest of the engine).
#
# OBS_LOG_TEXT is the RAW log text (not a pre-extracted capability flag) — the #463 review
# caught that pre-extracting collapsed "the log was never read at all" (SSH failure / OBS never
# launched) and "the log WAS read but carries no genlock marker" (a genuine stock/wrong build)
# into the same empty string, which this function then reported as a false DRIFT for a mere
# connectivity hiccup. check_imag_report derives the marker itself via
# genlock_capability_from_log, mirroring the sibling strih/stream `drift_check_capability`'s
# two-tier check (empty text -> UNKNOWN; non-empty text with no marker -> DRIFT).
#
# OBS_DANTESYNC_LOG_TEXT (#489, spun out of #479) is likewise the RAW `journalctl -u dantesync`
# text, for the same reason: check_imag_report derives the lock state itself via
# dantesync_locked_from_log, so "journalctl never read" (empty text -> UNKNOWN) and "dantesync
# running but never locked" (non-empty text, no marker -> DRIFT) stay distinguishable.
#
# OBS_TIMESYNC_STATES (#596) is the SAME per-daemon `NAME|DPKG|ACTIVE|ENABLED` block
# scripts/verify-device.sh's (r) check gathers for the cam1-6 fleet — this closes #591's
# remaining "drift-guard should inherit it too" gap for imag-nb. check_imag_report derives the
# verdict itself via the shared timesync_authority_verdict (scripts/lib/timesync-authority.sh), so
# "block never read" (empty text -> UNKNOWN, an SSH hiccup) and "read, no competing daemon" (ok)
# vs "read, a competing daemon installed/active/enabled" (DRIFT) stay distinguishable — same
# two-tier shape as every other facet in this function.
#
# Simpler than the strih/stream `--compare` path (#463 research comment): imag is a plain Linux
# box reachable over SSH, so drift-guard can gather its OWN observed values directly instead of
# depending on an external win-* MCP round-trip.
check_imag_report() {
  # #531: exp_build_sha/obs_build_sha (the old static-pin build check, check #1 here) are GONE — the
  # genlock build-identity check is now the DYNAMIC imag_build_drift_report, run alongside this by
  # gather_and_check_imag. This function covers the SSH-gathered live-state pins only.
  local exp_distroav_sha="$1" obs_distroav_sha="$2"
  local exp_fps="$3" obs_fps="$4" exp_latency="$5" obs_latency="$6"
  local obs_log_text="$7" exp_plugin_path="$8" plugin_present="$9"
  # #489: optional trailing pair (older call sites pass only 9 args) — default-empty so a
  # caller that hasn't been updated yet still gets a graceful UNKNOWN row, never an `unbound
  # variable` crash under this script's `set -u` (mirrors compare_observed's own optional
  # trailing params, e.g. `av_sync_calibrated_ms`).
  local exp_dantesync_locked="${10:-}" obs_dantesync_log_text="${11:-}"
  # #596: optional 12th param (older call sites pass only 9-11 args) — default-empty, same
  # backward-compatible convention as the #489 pair above.
  local obs_timesync_states="${12:-}"
  # #1040: optional 13th (power-envelope gather block) + 14th (pinned power_pl1_w_imag watts) —
  # default-empty, SAME backward-compatible convention. An unread/unsupplied block reads UNKNOWN
  # (never a false DRIFT for a mere SSH hiccup), exactly like every other two-tier check here.
  local obs_power_envelope="${13:-}" exp_power_pl1_w="${14:-}"
  # #780: optional 15th (display-path gather block) — default-empty, SAME backward-compatible
  # convention. An unread/unsupplied block reads UNKNOWN per facet in check #10 (never a false
  # DRIFT for a mere SSH hiccup), exactly like every two-tier check here.
  local obs_display_path="${15:-}"
  # #784: optional 16th (raw /proc/cmdline gather block) — default-empty, SAME backward-compatible
  # convention as #780's 15th. An unread/unsupplied cmdline reads UNKNOWN per check #11 (never a
  # false DRIFT for a mere SSH hiccup), exactly like every two-tier check here.
  local obs_cmdline="${16:-}"
  local drift=0 unknown=0

  # 1. distroav.so hash (the Linux plugin binary itself, not just the marker file).
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

  # 2. genlock render-tick capability marker present in the OBS log — the #119 wrong-build guard
  # (a stock/wrong build emits no `genlock:` line at all, regardless of marketing version).
  # Two-tier check (mirrors drift_check_capability's strih/stream logic): an EMPTY log text
  # (SSH failed to reach imag-nb, or OBS has never been launched there) is UNKNOWN — nothing was
  # read, never a false DRIFT for a connectivity hiccup. Only NON-EMPTY text with no marker line
  # is the genuine #119 stock/wrong-build DRIFT.
  if [ -z "$obs_log_text" ]; then
    printf '  %-22s UNKNOWN  (OBS log not read on imag-nb)\n' "genlock_capability"
    unknown=$((unknown + 1))
  elif [ "$(genlock_capability_from_log "$obs_log_text")" = "1" ]; then
    printf '  %-22s OK       (genlock build-unique marker present)\n' "genlock_capability"
  else
    printf '  %-22s DRIFT    (no genlock render-tick marker in the OBS log — stock/wrong build)\n' "genlock_capability"
    drift=$((drift + 1))
  fi

  # 3. fps pin (imag is the 60fps low-latency IMAG role, Topology v2 — a drift DOWN to 30 is drift).
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

  # 4. genlock latency pin (the #235 single-knob ms value).
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

  # 5. Linux plugin path — the SAME single-canonical-path invariant as `canonical_plugin_path`
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

  # 6. dantesync PTP/NTP clock lock (#489, spun out of #479's setup-imag.sh provisioning check) —
  # imag-nb's OWN wall-clock discipline, the basis genlock's `wall-clock-slaved render tick`
  # (genlock_wall_clock above) depends on. Mirrors the SAME journalctl markers setup-imag.sh's
  # own provisioning-time restart check keys on (setup-imag.sh:230). Two-tier check via
  # dantesync_locked_from_log, same shape as genlock_capability above: an EMPTY journal read (SSH
  # failure, or the journal was never read) is UNKNOWN; a NON-EMPTY journal with no lock marker
  # at all is a genuine DRIFT (dantesync is running but the clock never locked — genlock's timing
  # basis is compromised even though every OTHER pin can still look clean).
  local obs_dantesync_locked
  obs_dantesync_locked="$(dantesync_locked_from_log "$obs_dantesync_log_text")"
  if [ -z "$obs_dantesync_locked" ]; then
    printf '  %-22s UNKNOWN  (journalctl -u dantesync not read on imag-nb)\n' "dantesync_locked"
    unknown=$((unknown + 1))
  elif [ -z "$exp_dantesync_locked" ]; then
    printf '  %-22s UNKNOWN  (no pinned dantesync_locked_imag in README)\n' "dantesync_locked"
    unknown=$((unknown + 1))
  elif [ "$obs_dantesync_locked" = "$exp_dantesync_locked" ]; then
    printf '  %-22s OK       (%s)\n' "dantesync_locked" "$obs_dantesync_locked"
  else
    printf '  %-22s DRIFT    (expected %s, observed %s)\n' "dantesync_locked" "$exp_dantesync_locked" "$obs_dantesync_locked"
    drift=$((drift + 1))
  fi

  # 7. genlock render-tick SCHED_FIFO pin (#484 intent, #572 root cause). imag-nb ONLY — the
  # obs-video.c pin is Linux-guarded, so this marker only ever appears in imag-nb's log. Two-tier
  # via genlock_rt_pin_from_log, same shape as genlock_capability above: an EMPTY log is UNKNOWN
  # (never read / OBS never launched); a NON-EMPTY log with NEITHER the success nor the failure
  # line is ALSO UNKNOWN (the deployed build predates #484 — imag_build_drift_report's dynamic
  # origin/main compare already flags a stale build separately; this facet stays silent rather
  # than guessing at a pin outcome the build never attempted). Only the EXPLICIT "could NOT set
  # ... SCHED_FIFO" line is DRIFT — the exact #572 signature (missing rtprio ulimit grant leaves
  # the render-tick thread on ordinary SCHED_OTHER, causing occasional missed 60fps deadlines).
  if [ -z "$obs_log_text" ]; then
    printf '  %-22s UNKNOWN  (OBS log not read on imag-nb)\n' "genlock_rt_pin"
    unknown=$((unknown + 1))
  else
    local rt_pin_status
    rt_pin_status="$(genlock_rt_pin_from_log "$obs_log_text")"
    case "$rt_pin_status" in
      ok)
        printf '  %-22s OK       (render-tick thread achieved SCHED_FIFO on the isolated core)\n' "genlock_rt_pin"
        ;;
      failed)
        printf '  %-22s DRIFT    (render-tick thread stuck SCHED_OTHER — missing rtprio ulimit grant, #572)\n' "genlock_rt_pin"
        drift=$((drift + 1))
        ;;
      *)
        printf '  %-22s UNKNOWN  (no genlock RT-pin marker in the OBS log — build may predate #484)\n' "genlock_rt_pin"
        unknown=$((unknown + 1))
        ;;
    esac
  fi

  # 8. dantesync sole timesync authority — no competing timesync daemon (systemd-timesyncd/
  # chrony/ntp/ntpsec/openntpd) installed/active/enabled alongside dantesync (#591's cam1-6 fleet
  # gate, extended to imag-nb here — #596, the remaining "drift-guard should inherit it too" gap).
  # Two-tier via the shared timesync_authority_verdict (scripts/lib/timesync-authority.sh, sourced
  # by both verify-device.sh and drift-guard.sh): an EMPTY gathered block (SSH failure, or the
  # remote per-daemon loop produced no output at all) is UNKNOWN — never a false OK for a mere
  # connectivity hiccup. A non-empty block runs the EXACT SAME per-daemon dpkg/active/enabled
  # verdict #591 already hard-gates on the cam1-6 fleet — no README pin needed, this is an
  # unconditional policy check, same as verify-device.sh's own (r).
  if [ -z "$obs_timesync_states" ]; then
    printf '  %-22s UNKNOWN  (timesync-daemon state not read on imag-nb)\n' "timesync_authority"
    unknown=$((unknown + 1))
  else
    local ts_verdict
    ts_verdict="$(timesync_authority_verdict "$obs_timesync_states")"
    if [ "$ts_verdict" = "ok" ]; then
      printf '  %-22s OK       (dantesync is the sole timesync authority)\n' "timesync_authority"
    else
      # code-review finding: joining on ';' then blanket-replacing '/;/; /g' also matched a ';'
      # that was ALREADY part of a reason's own text (the INSTALLED message reads "...only
      # dantesync; masking is not enough)"), producing a double space. '|' never appears inside
      # any FAIL reason (the daemon-state block already relies on that same invariant), so it is
      # a safe join delimiter that can be blanket-replaced without touching reason text.
      local ts_reasons
      ts_reasons="$(printf '%s\n' "$ts_verdict" | sed 's/^FAIL: //' | paste -sd'|' - | sed 's/|/; /g')"
      printf '  %-22s DRIFT    (%s)\n' "timesync_authority" "$ts_reasons"
      drift=$((drift + 1))
    fi
  fi

  # 9. power/thermal envelope (#1040) — the MMIO RAPL PL1 pin (`power_pl1_w_imag`), the slpc knob,
  # thermald being PURGED, and both envelope units alive. Runs the SHARED imag_power_envelope_
  # verdict (scripts/lib/imag-power-envelope.sh) over the gathered block, mapping each per-facet
  # OK/DRIFT/UNKNOWN into this function's own report rows + exit-code contract. Two-tier, same as
  # every check above: an EMPTY gathered block (SSH hiccup / not read) is UNKNOWN per facet, never
  # a false DRIFT. An in-progress LEGITIMATE guard step-down reads as DRIFT — CORRECT (a clamp IS a
  # degradation; the [0/8] preflight refusing during a clamp episode is the desired behavior).
  if [ -z "$obs_power_envelope" ]; then
    printf '  %-22s UNKNOWN  (power-envelope state not read on imag-nb)\n' "power_envelope"
    unknown=$((unknown + 1))
  else
    local pe_facet pe_status pe_detail pe_line
    while IFS='|' read -r pe_facet pe_status pe_detail; do
      [ -n "$pe_facet" ] || continue
      pe_line="power_envelope/${pe_facet}"
      case "$pe_status" in
        OK)      printf '  %-22s OK       (%s)\n' "$pe_line" "$pe_detail" ;;
        DRIFT)   printf '  %-22s DRIFT    (%s)\n' "$pe_line" "$pe_detail"; drift=$((drift + 1)) ;;
        *)       printf '  %-22s UNKNOWN  (%s)\n' "$pe_line" "$pe_detail"; unknown=$((unknown + 1)) ;;
      esac
    done <<< "$(imag_power_envelope_verdict "$obs_power_envelope" "$exp_power_pl1_w")"
  fi

  # 10. display-path config (#780 / issue 1146 REVERT) — picom NOT running + picom.service NOT
  # enabled (the compositor tear fix cost 21.57% OBS render skips on the 25W envelope, live
  # 2026-08-20 — the #841 "picom off" doctrine stands; the package/config/unit stay installed
  # dormant), HDMI the xrandr PRIMARY, the #841 iGPU max-freq pin (imag-igpu-maxperf.service
  # enabled+active + gt_min_freq pinned to gt_RP0 — the Intel GPUPowerMizerMode=1 counterpart),
  # and the #779 touchpad tap conf. See scripts/lib/imag-display-path.sh's doctrine header for the
  # full reversal→revert history (the dual-output beat physics stays valid; only the compositor
  # CURE is rejected for its render cost — the tear-free direction is the OBS projector's own
  # vsync / single-display, issue 1146 / issue 1147). Runs the SHARED
  # imag_display_path_verdict (scripts/lib/imag-display-path.sh) over the gathered block, mapping each
  # per-facet OK/DRIFT/UNKNOWN into this function's own report rows + exit-code contract — the loop is
  # generic, so new facets flow through with no edit here. Two-tier, same as every check above: an
  # EMPTY gathered block (SSH hiccup / not read) is UNKNOWN per facet, never a false DRIFT; a MISSING
  # pgrep/xrandr on the box is UNKNOWN by name (#833), never a false verdict. NVIDIA-era
  # ForceFullCompositionPipeline is obsolete-by-hardware (the box is Intel-only, no FFCP — #816/#841;
  # the inert 20-tearfree.conf is deliberately NOT provisioned; the picom vsync compositor is the
  # real mechanism now — see the issue 1146 design comment). Since issue 1152 M4 the shared verdict
  # also carries the drm_output facet (the DEFAULT-OFF in-OBS DRM-lease HDMI output: dormant config
  # = OK, ENABLED demands the current OBS log's `program scanout LIVE` proof else DRIFT), and
  # hdmi_primary is lease-aware (DRM output ENABLED ⇒ HDMI is leased OUT of the X layout by design,
  # so a panel primary is then OK, never the issue-1146 DRIFT) — both flow through this same loop.
  if [ -z "$obs_display_path" ]; then
    printf '  %-22s UNKNOWN  (display-path state not read on imag-nb)\n' "display_path"
    unknown=$((unknown + 1))
  else
    local dp_facet dp_status dp_detail dp_line
    while IFS='|' read -r dp_facet dp_status dp_detail; do
      [ -n "$dp_facet" ] || continue
      dp_line="display_path/${dp_facet}"
      case "$dp_status" in
        OK)      printf '  %-22s OK       (%s)\n' "$dp_line" "$dp_detail" ;;
        DRIFT)   printf '  %-22s DRIFT    (%s)\n' "$dp_line" "$dp_detail"; drift=$((drift + 1)) ;;
        *)       printf '  %-22s UNKNOWN  (%s)\n' "$dp_line" "$dp_detail"; unknown=$((unknown + 1)) ;;
      esac
    done <<< "$(imag_display_path_verdict "$obs_display_path")"
  fi

  # 11. kernel-cmdline isolation (#784) — /proc/cmdline must carry NO kernel isolcpus=/nohz_full= and
  # no SCOPED rcu_nocbs=<cpu-list> (the #784/#842 footgun: isolcpus= removes CPUs from the scheduler
  # load-balancing domain and piled 114 of OBS's 119 threads onto one core, 60fps -> ~53fps NDI
  # receive). rcu_nocbs=all is the legitimate #482 low-latency (preempt=full) token and stays OK.
  # Runs the SHARED imag_cmdline_isolation_verdict (scripts/lib/imag-cmdline-isolation.sh) over the
  # gathered raw cmdline, mapping its single OK/DRIFT/UNKNOWN facet line into this function's own
  # report row + exit-code contract. Two-tier, same as every check above: an EMPTY gathered block
  # (SSH hiccup / not read / a 9..15-arg call site) is UNKNOWN, never a false DRIFT.
  if [ -z "$obs_cmdline" ]; then
    printf '  %-22s UNKNOWN  (/proc/cmdline not read on imag-nb)\n' "cmdline_isolation"
    unknown=$((unknown + 1))
  else
    local ci_facet ci_status ci_detail
    while IFS='|' read -r ci_facet ci_status ci_detail; do
      [ -n "$ci_facet" ] || continue
      case "$ci_status" in
        OK)      printf '  %-22s OK       (%s)\n' "$ci_facet" "$ci_detail" ;;
        DRIFT)   printf '  %-22s DRIFT    (%s)\n' "$ci_facet" "$ci_detail"; drift=$((drift + 1)) ;;
        *)       printf '  %-22s UNKNOWN  (%s)\n' "$ci_facet" "$ci_detail"; unknown=$((unknown + 1)) ;;
      esac
    done <<< "$(imag_cmdline_isolation_verdict "$obs_cmdline")"
  fi

  # 12. projector present-vsync ARMED marker (#1151, REPORT-ONLY — issue-1107 fullscreen-Program EGL
  # present-vsync + issue-1146 observability marker). Runs the SHARED projector_vsync_verdict
  # (scripts/lib/obs-projector-vsync.sh) over the SAME already-gathered $obs_log_text — no extra SSH.
  # REPORT-ONLY: this facet touches NEITHER $drift NOR $unknown, so it never changes the 20/11/0 exit
  # contract. A missing marker is a healthy ordering-dependent state (the Program projector was not
  # (re)opened since OBS start — the marker is one-shot-on-change at projector open), NOT a config
  # drift; and per issue 781 the marker only proves the tear-free present MECHANISM is engaged, never
  # that scanout tearing is gone (objective proof needs the physical HDMI tap, ops-wait hardware).
  # #833: an unreadable/empty log surfaces UNKNOWN via the verdict, never a false OK.
  local pv_facet pv_status pv_detail
  while IFS='|' read -r pv_facet pv_status pv_detail; do
    [ -n "$pv_facet" ] || continue
    case "$pv_status" in
      OK) printf '  %-22s OK       (%s)\n' "$pv_facet" "$pv_detail" ;;
      *)  printf '  %-22s UNKNOWN  (%s)\n' "$pv_facet" "$pv_detail" ;;
    esac
  done <<< "$(projector_vsync_verdict "$obs_log_text")"

  [ "$drift" -gt 0 ] && return 20
  [ "$unknown" -gt 0 ] && return 11
  return 0
}

# imag_genlock_range_log REPO_ROOT BOX_SHA -> prints `git log --oneline BOX_SHA..origin/main --
# vendor/obs-studio vendor/distroav` (one genlock-touching commit per line the box is BEHIND);
# exit status mirrors that `git log` call. #531 review: BOX_SHA comes from a file READ OVER SSH
# from imag-nb (`GENLOCK_BUILD_SHA.txt`) and is used UNVALIDATED — normally a clean 40-hex commit
# SHA, but a truncated/corrupted write (a crash mid-write, disk corruption, a future setup-imag.sh
# bug) could leave it shaped like a git long-option, e.g. `--grep=x`. WITHOUT `--end-of-options`,
# `git log "${box_sha}..origin/main" ...` would silently CONSUME such a value as a real git FLAG
# instead of a revision range, exiting 0 with EMPTY output — the exact "box is current, OK" verdict
# in imag_build_drift_report, i.e. a FALSE OK, precisely the failure mode this whole #531 check
# exists to eliminate. `--end-of-options` (git >= 2.24, well within this repo's toolchain) marks the
# end of git's own option parsing so any value here is ALWAYS treated as a revision, never a flag —
# a malformed value now fails LOUD (`fatal: bad revision`, non-zero exit) instead of silently
# succeeding empty. Mirrors the SAME OpenSSH-argument-injection defense `gather_and_check_imag`
# already applies to `$target` via ssh's own `--` below. Isolated into its own function (rather than
# inlined in gather_and_check_imag) so it is independently testable against THIS repo's own local
# checkout — no live SSH to imag-nb needed (tests/drift_guard.rs).
imag_genlock_range_log() {
  local repo_root="$1" box_sha="$2"
  git -C "$repo_root" log --oneline --end-of-options "${box_sha}..origin/main" \
    -- vendor/obs-studio vendor/distroav 2>/dev/null
}

# gather_and_check_imag HOST USER README -> SSH-gathers the observed values from the LIVE
# imag-nb box (#463) and runs [`check_imag_report`] against the pinned set in README. NOT unit
# tested (it is pure I/O glue over `ssh` — same convention as the win-* MCP gathering for
# strih/stream, which is also untested at this layer; the JUDGEMENT is in the pure function
# above). Paths are the ones `scripts/setup-imag.sh` installs to (DESKTOP_USER=newlevel):
#   - /opt/obs-genlock/GENLOCK_BUILD_SHA.txt   (genlock build identity marker)
#   - /usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so   (the Linux plugin binary)
#   - ~/.config/obs-studio/logs/*.txt   (OBS log — the MOST RECENT file; OBS names logs `.txt`, #1151,
#     same libobs log lines fps_from_log / genlock_capability_from_log / genlock_latency_ms_from_log /
#     projector_vsync_verdict already parse on Windows, since the log format is platform-independent
#     libobs text)
#   - `journalctl -u dantesync` (#489, spun out of #479) — the SAME PTP LOCK/NANO or NTP-offset
#     markers scripts/setup-imag.sh's own provisioning-time restart check keys on (setup-imag.sh:230)
#   - `dpkg -s` + `systemctl is-active`/`is-enabled` for systemd-timesyncd/chrony/ntp/ntpsec/
#     openntpd (#596) — the SAME per-daemon signal scripts/verify-device.sh's (r) check gathers
#     for the cam1-6 fleet's sole-timesync-authority gate (#591)
gather_and_check_imag() {
  local host="$1" user="${2:-newlevel}" readme="$3"
  # #463 review: `host=`/`user=` are operator-supplied CLI values. `--` marks the end of ssh's
  # own option parsing so a value starting with `-` (e.g. a stray `-oProxyCommand=...`) can
  # never be parsed as an ssh FLAG instead of the positional target (OpenSSH argument
  # injection). `-o BatchMode=yes` refuses an interactive password prompt (fail fast instead of
  # hanging waiting for input this non-interactive check can never supply). `timeout 15` bounds
  # the WHOLE call — `-o ConnectTimeout=10` only bounds the TCP connect phase; a remote command
  # that blocks AFTER connecting (a stalled mount, a wedged log directory) would otherwise hang
  # this check forever.
  local target="${user}@${host}"
  local plugin_path="/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so"
  local ssh_cmd=(timeout 15 ssh -o ConnectTimeout=10 -o BatchMode=yes -- "$target")

  # #531: the genlock build IDENTITY is no longer a static README pin — it is compared DYNAMICALLY
  # against origin/main's vendored-genlock HEAD below (imag_build_drift_report), so
  # genlock_build_sha_imag is no longer read here.
  local exp_distroav_sha exp_fps exp_latency exp_dantesync_locked exp_power_pl1_w
  exp_distroav_sha="$(pinned_setting "$readme" distroav_so_sha256_imag)"
  exp_fps="$(pinned_setting "$readme" output_fps_imag)"
  exp_latency="$(pinned_setting "$readme" genlock_latency_ms_imag)"
  exp_dantesync_locked="$(pinned_setting "$readme" dantesync_locked_imag)"
  # #1040: the pinned MMIO RAPL PL1 watts (the strict envelope gate's authority — the README pin,
  # never a hardcoded literal). A missing pin -> empty -> the power_envelope pl1 row reads UNKNOWN.
  exp_power_pl1_w="$(pinned_setting "$readme" power_pl1_w_imag)"

  local obs_build_sha obs_distroav_sha obs_log obs_fps obs_latency plugin_present obs_dantesync_log
  local obs_timesync_states obs_power_envelope obs_display_path obs_cmdline
  obs_build_sha="$("${ssh_cmd[@]}" \
    'cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null' 2>/dev/null || true)"
  # #531: DYNAMIC genlock-build staleness — compare the box's deployed GENLOCK_BUILD_SHA.txt against
  # origin/main's vendored-genlock HEAD (the authoritative "what SHOULD be deployed"). `git fetch`
  # first so origin/main is fresh (best-effort: a fetch failure WARNS but still compares against
  # whatever origin/main already is — a possibly-slightly-stale compare beats no compare); then
  # `git log <box>..origin/main -- vendor/obs-studio vendor/distroav` lists the genlock commits the
  # box is BEHIND. Anchored to the SCRIPT's own repo (`$(dirname BASH_SOURCE)/..`), not CWD, so it
  # works however the guard is invoked (rig-mode.sh, the /drift-guard command, CI). The `|| git_rc=$?`
  # OR-list keeps a bad-SHA / unreachable git error from aborting the whole script under set -e (the
  # same #463 lesson as the ssh capture below) — a git error is reported UNKNOWN, never a false OK.
  # The pure imag_build_drift_report (run below) turns (obs_build_sha, git_rc, git_range) into the verdict.
  # #531 review: the `cd` here is itself a fallible command under this file's `set -e` — an
  # unguarded `repo_root="$(cd ... && pwd)"` would abort the WHOLE script (silently, no report line
  # at all) on the essentially-never-but-possible case that the script's own parent directory can't
  # be `cd`'d into. `|| repo_root=""` neutralizes errexit; an empty repo_root then short-circuits to
  # UNKNOWN below instead of silently running `git` against the wrong (cwd) directory.
  local repo_root git_range="" git_rc=0
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || repo_root=""
  if [ -z "$repo_root" ]; then
    git_rc=1
    echo "WARN: could not resolve drift-guard.sh's own repo root — skipping imag genlock-build compare" >&2
  else
    # #531 review: `timeout 15` bounds the fetch the SAME way the ssh calls in this function are
    # bounded — `warn_imag_genlock_stale` (rig-mode.sh) advertises this whole check as "never blocks
    # going live", and an unbounded `git fetch` against a host that silently drops packets (rather
    # than refusing the connection) can hang well past any TCP-level default before failing.
    timeout 15 git -C "$repo_root" fetch origin --quiet 2>/dev/null \
      || echo "WARN: git fetch origin failed (or timed out) — comparing imag build against a possibly-stale origin/main" >&2
    if [ -n "$obs_build_sha" ]; then
      git_range="$(imag_genlock_range_log "$repo_root" "$obs_build_sha")" || git_rc=$?
    fi
  fi
  # #463 review: derive `plugin_present` from THIS SAME sha256sum call instead of a separate
  # `test -f` round-trip (one fewer SSH connection), AND distinguish an SSH CONNECTION failure
  # (OpenSSH's own reserved exit code 255, or 124 from the `timeout` wrapper firing) from the
  # remote command genuinely finding no file: `sha256sum` on a missing file exits non-zero with
  # empty stdout either way, so the ssh/timeout exit code is the ONLY way to tell "imag-nb was
  # unreachable" apart from "the file is genuinely gone". Collapsing both into a bare 0/1 (the
  # pre-review shape) reported a transient network hiccup as a false `distroav_so_path DRIFT
  # (not found)` alarm — never a wrong-signal pass OR a false alarm.
  # #463 review (2nd pass): this script runs under `set -euo pipefail` (top of file) — a bare
  # `var="$(cmd)"` with NO `|| ...` guard triggers `errexit` and ABORTS THE WHOLE SCRIPT the
  # instant ssh/timeout returns nonzero (255/124, precisely the case being handled here),
  # so `local ssh_rc=$?` on the next line would never even run. `|| ssh_rc=$?` puts the
  # assignment in an OR-list (the one `set -e` exemption that actually applies: only the LAST
  # command of an AND/OR list is errexit-checked), which both captures the real exit code AND
  # keeps the script alive to report UNKNOWN instead of crashing. Empirically confirmed: without
  # this guard, `--check-imag` against an unreachable imag-nb crashes with a bare exit 255
  # instead of printing a graceful UNKNOWN report.
  local ssh_rc=0
  obs_distroav_sha="$("${ssh_cmd[@]}" \
    "sha256sum '$plugin_path' 2>/dev/null | awk '{print \$1}'" 2>/dev/null)" || ssh_rc=$?
  if [ "$ssh_rc" -eq 255 ] || [ "$ssh_rc" -eq 124 ]; then
    plugin_present=""  # connection failure / timeout -> UNKNOWN, never a false "missing" alarm
  elif [ -n "$obs_distroav_sha" ]; then
    plugin_present=1
  else
    plugin_present=0
  fi
  # Most-recently-modified OBS log file. #1151: OBS names its logs `YYYY-MM-DD HH-MM-SS.txt` (NOT
  # .log) — confirmed live on imag-nb, and the SAME `*.txt` glob every other imag OBS-log reader uses
  # (verify-imag.sh, imag_scenes.py, imag-jitter-monitor.sh, rig-health-audit.py, mv-fps-*). The
  # earlier `*.log` glob matched NOTHING on imag, so every OBS-log facet below (genlock_capability /
  # output_fps / genlock_latency / rt_pin / projector_vsync) read EMPTY -> chronic UNKNOWN. Fixing
  # it makes those facets actually read the log; SAFE because rig-mode's only --check-imag HARD-BLOCK
  # (issue 789) is genlock_build-scoped (the GENLOCK_BUILD_SHA.txt SSH compare), never these facets.
  obs_log="$("${ssh_cmd[@]}" \
    'f=$(ls -t "$HOME/.config/obs-studio/logs/"*.txt 2>/dev/null | head -1); [ -n "$f" ] && cat "$f"' \
    2>/dev/null || true)"
  obs_fps="$(fps_from_log "$obs_log")"
  obs_latency="$(genlock_latency_ms_from_log "$obs_log")"
  # #489: a bounded one-shot journal read (no polling loop — this is a read-only drift snapshot,
  # not setup-imag.sh's post-restart wait). `--since -10min` bounds by RECENCY (#489 review: a
  # bare `-n 100` reads the last 100 lines EVER logged for this unit, however old -- if dantesync
  # is fully stopped/masked, those 100 lines never change and a stale historic LOCK line would
  # report false OK forever even though the daemon is dead) and `-n 100` caps volume as a
  # secondary bound. 10 minutes is generous vs. the lock lines' periodic cadence; a genuinely-
  # locked dantesync always logs one well within that window.
  obs_dantesync_log="$("${ssh_cmd[@]}" \
    'journalctl -u dantesync --no-pager --since "-10min" -n 100 2>/dev/null' 2>/dev/null || true)"
  # #596: gather per-competing-daemon state the EXACT SAME way verify-device.sh's own (r) sole-
  # timesync-authority check does for the cam1-6 fleet — one SSH call collecting each daemon's
  # `dpkg -s` status + `systemctl is-active` + `is-enabled` into a `NAME|DPKG|ACTIVE|ENABLED`
  # block, so drift-guard can run the IDENTICAL timesync_authority_verdict (scripts/lib/
  # timesync-authority.sh) against imag-nb. `|| true` (same convention as the other gathers above)
  # keeps an SSH failure from aborting the script — an empty result reads as UNKNOWN, never a
  # false pass, in check_imag_report's check #8. The gathering command itself is shared via
  # timesync_gather_remote_snippet() (code-review finding: sharing only the VERDICT function while
  # leaving this exact daemon-list for-loop duplicated would let a future daemon added to the
  # competing set silently diverge between verify-device.sh and drift-guard.sh).
  obs_timesync_states="$("${ssh_cmd[@]}" "$(timesync_gather_remote_snippet)" 2>/dev/null || true)"
  # #1040: gather the power/thermal-envelope state in ONE SSH call via the SHARED
  # imag_power_envelope_gather_remote_snippet (scripts/lib/imag-power-envelope.sh) — the SAME
  # identity-based RAPL/slpc/thermald/units block scripts/verify-imag.sh's check (u) gathers. `|| true`
  # (same convention as every gather above) keeps an SSH failure from aborting the script — an empty
  # block reads as UNKNOWN per facet in check_imag_report's check #9, never a false DRIFT.
  obs_power_envelope="$("${ssh_cmd[@]}" "$(imag_power_envelope_gather_remote_snippet)" 2>/dev/null || true)"
  # #780: gather the display-path state in ONE SSH call via the SHARED
  # imag_display_path_gather_remote_snippet (scripts/lib/imag-display-path.sh) — the SAME block the
  # E2E [0/8] preflight gathers. `|| true` (same convention as every gather above) keeps an SSH
  # failure from aborting the script — an empty block reads as UNKNOWN per facet in check #10.
  obs_display_path="$("${ssh_cmd[@]}" "$(imag_display_path_gather_remote_snippet)" 2>/dev/null || true)"
  # #784: gather the raw /proc/cmdline in ONE SSH call via the SHARED
  # imag_cmdline_isolation_gather_remote_snippet (scripts/lib/imag-cmdline-isolation.sh). `|| true`
  # (same convention as every gather above) keeps an SSH failure from aborting the script — an empty
  # block reads as UNKNOWN in check_imag_report's check #11, never a false DRIFT.
  obs_cmdline="$("${ssh_cmd[@]}" "$(imag_cmdline_isolation_gather_remote_snippet)" 2>/dev/null || true)"

  echo "== drift-guard --check-imag  host=${host}  (SSH-gathered + git-compared; FAILS loudly on drift) =="
  # #531: the DYNAMIC genlock-build staleness check (box vs origin/main's vendored-genlock HEAD)
  # runs FIRST — it is the #530 recurrence guard, the one check that can catch a merged-but-never-
  # deployed genlock change. Then the SSH-gathered live-state pins.
  local rc_build=0 rc_report=0
  imag_build_drift_report "$obs_build_sha" "$git_rc" "$git_range" || rc_build=$?
  # #463 review: pass the RAW `$obs_log` text (not a pre-extracted capability flag) — see
  # check_imag_report's doc comment for why pre-extracting collapsed "log unreadable" and "log
  # read, no marker" into the same false-DRIFT signal. #489: `$obs_dantesync_log` is passed RAW
  # for the identical reason. #596: `$obs_timesync_states` (the 12th arg) is likewise the RAW
  # gathered block — check_imag_report derives the ok/DRIFT/UNKNOWN verdict itself.
  # #1040: `$obs_power_envelope` (13th) is the RAW gathered block + `$exp_power_pl1_w` (14th) the
  # pinned watts — check_imag_report's check #9 derives the per-facet ok/DRIFT/UNKNOWN verdict itself.
  # #780: `$obs_display_path` (15th) is the RAW display-path gathered block — check_imag_report's
  # check #10 derives the per-facet ok/DRIFT/UNKNOWN verdict itself.
  # #784: `$obs_cmdline` (16th) is the RAW /proc/cmdline gathered block — check_imag_report's
  # check #11 derives the cmdline-isolation ok/DRIFT/UNKNOWN verdict itself.
  check_imag_report "$exp_distroav_sha" "$obs_distroav_sha" \
    "$exp_fps" "$obs_fps" "$exp_latency" "$obs_latency" "$obs_log" "$plugin_path" "$plugin_present" \
    "$exp_dantesync_locked" "$obs_dantesync_log" "$obs_timesync_states" \
    "$obs_power_envelope" "$exp_power_pl1_w" "$obs_display_path" "$obs_cmdline" || rc_report=$?
  # Combine the two facets' exit codes into the engine's single contract: DRIFT (20) dominates, then
  # UNKNOWN (11), else clean (0). A STALE build FAILS LOUD even when every live-state pin is clean.
  if [ "$rc_build" -eq 20 ] || [ "$rc_report" -eq 20 ]; then return 20; fi
  if [ "$rc_build" -eq 11 ] || [ "$rc_report" -eq 11 ]; then return 11; fi
  return 0
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
          if grep -qE '^[0-9]+$' <<<"$obs_lat" \
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
  if grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' <<<"$val"; then
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

--check-imag (#463, EPIC #466 Topology v2; #531 dynamic build-staleness): unlike strih/stream
  (Windows, needs the win-* MCP tools to read logs/settings), imag-nb is a plain Linux box reachable
  over SSH, so drift-guard gathers its OWN observed values directly: `/opt/obs-genlock/
  GENLOCK_BUILD_SHA.txt` (the deployed genlock build identity — #531: compared DYNAMICALLY against
  origin/main's vendored-genlock HEAD via `git fetch` + `git log <box>..origin/main -- vendor/
  obs-studio vendor/distroav`; a non-empty range = the box is BEHIND merged genlock commits =
  STALE = DRIFT, the #530 45fps recurrence guard — no static README pin any more), a SHA256 of
  `/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so` (the Linux plugin binary), the OBS log
  (`~/.config/obs-studio/logs/*.txt`, most recent file — OBS names logs `.txt`, #1151) for the genlock capability marker + the fps +
  latency pins + the #484 render-tick SCHED_FIFO pin outcome (#572 — DRIFT if the log shows the
  WARN-and-continue SCHED_OTHER fallback, i.e. a missing rtprio ulimit grant), and
  `journalctl -u dantesync` (#489) for the DanteSync PTP/NTP clock-lock pin.
  Optional `host=` / `user=` override the imag-nb defaults (`10.77.9.182` / `newlevel`). The
  remaining live-state pins live in vendor/README.md as `distroav_so_sha256_imag` (secondary) /
  `output_fps_imag` / `genlock_latency_ms_imag` / `dantesync_locked_imag`.

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

--compare DEPLOYED-BUILD currency key (#548, opt-in — dynamic staleness vs origin/main):
  genlock_build_sha (the deployed vendored-genlock commit — read BUNDLE_MANIFEST.json .build_sha off
  the box, e.g. `(Get-Content 'C:\Program Files\obs-studio\BUNDLE_MANIFEST.json' | ConvertFrom-Json).build_sha`).
  The OBS/DistroAV/NDI VERSION strings above are byte-identical across a stock vs genlock build AND
  across an OLD vs NEW genlock build, so a box left on a STALE genlock build passes every other
  --compare check — the exact blind spot that hid the 843-commit deploy-drift. This key runs the SAME
  dynamic `git log <sha>..origin/main -- vendor/obs-studio vendor/distroav` staleness check imag-nb
  got in #531: OK if current, DRIFT (fail loud) if the box is behind, UNKNOWN if the SHA was not read
  (never a silent clean). Omit it → the check is skipped (historic behavior).

--compare per-component BUILD SHA + capability keys (#122, opt-in — supply `manifest` to activate):
  manifest (path to the build-under-test's #120 BUNDLE_MANIFEST.json — download it from the
    windows-genlock / windows-genlock-fast artifact for the deployed build),
  obs_dll_sha256 (the deployed obs.dll Get-FileHash SHA256, read live off the box),
  distroav_dll_sha256 (the deployed distroav.dll Get-FileHash SHA256, read live off the box),
  genlock_capability (the live OBS-log genlock marker text — the build-unique
    `genlock: … render tick ENABLED` / `sub-frame jitter reserve` / `timestamp-aligned release`
    lines; a STOCK OBS 32.2.0 emits NONE -> DRIFT even though its version matches).
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
  # #463 review: output_fps_imag / genlock_latency_ms_imag are the two imag-nb pins that ARE
  # always backtick-pinned in vendor/README.md (unlike genlock_build_sha_imag/distroav_so_
  # sha256_imag, deliberately left unpinned until the first post-#463 live deploy — those two
  # stay OUT of this offline check for that reason). Without this, a malformed/missing imag fps
  # or latency pin would only ever surface via a LIVE `--check-imag` SSH run against imag-nb,
  # never in CI's manifest-only `--check-pins` pass.
  local p_fps_imag="${12}" p_latency_imag="${13}"
  # #489: dantesync_locked_imag is, like output_fps_imag/genlock_latency_ms_imag above, ALWAYS
  # backtick-pinned from day one (a runtime steady-state pin, not a build-artifact SHA that waits
  # for a live deploy) — so it belongs in this offline check, not the excluded genlock_build_sha_
  # imag/distroav_so_sha256_imag category above.
  local p_dantesync_locked_imag="${14}"
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
  # #463: imag-nb's own host-keyed fps + genlock-latency pins.
  validate_nonempty "output_fps_imag"                "$p_fps_imag"        || errs=$((errs + 1))
  validate_nonempty "genlock_latency_ms_imag"        "$p_latency_imag"    || errs=$((errs + 1))
  # #489: imag-nb's dantesync clock-lock pin (always-pinned steady state, see comment above).
  validate_nonempty "dantesync_locked_imag"          "$p_dantesync_locked_imag" || errs=$((errs + 1))
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
  echo "  output_fps_imag=$p_fps_imag  genlock_latency_ms_imag=$p_latency_imag"
  echo "  dantesync_locked_imag=$p_dantesync_locked_imag"

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
  # #548 dynamic genlock-BUILD staleness for strih/stream (opt-in when genlock_build_sha= supplied):
  # the git range vs origin/main is computed by the IMPURE caller (main) and passed in, keeping this
  # function pure + unit-testable exactly like the imag gather/report split (#531).
  local o_gl_build_sha="${26:-}" o_gl_build_rc="${27:-0}" o_gl_build_range="${28:-}"

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
  # above pass a STOCK OBS 32.2.0 — byte-for-byte a different build from our genlock 32.2.0, but the
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
      # #1115 path mapping (explicit): the manifest key is obs-plugins/64bit/distroav.dll but
      # the on-box DEPLOYED distroav lives at C:\ProgramData\obs-studio\plugins\distroav\
      # bin\64bit\distroav.dll (the ONLY path OBS loads it from). manifest_sha_for_component
      # resolves by BASENAME, so these two different keys map to the same distroav.dll and the
      # compare is honest once the deploy ships the bundle bytes to that load path (Option A).
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

  # #548: deployed genlock-BUILD currency for strih/stream. The OBS/DistroAV/NDI version strings
  # checked above are byte-identical across a stock vs genlock build, so a box left on a STALE genlock
  # build passes every other --compare check — exactly how the 843-commit deploy-drift stayed invisible
  # until #548. OPT-IN when genlock_build_sha= is supplied (the agent reads BUNDLE_MANIFEST.json
  # .build_sha off the box); same pure verdict fn as imag (#531): return 20 DRIFT / 11 UNKNOWN / 0 OK,
  # box-unread = UNKNOWN, never a silent clean.
  if [ -n "$o_gl_build_sha" ]; then
    rc=0
    genlock_build_drift_report "$host" \
      "redeploy the current genlock bundle to this box (see .claude/skills/obs-ops + the #548 Windows deploy) at a safe off-event time" \
      "$o_gl_build_sha" "$o_gl_build_rc" "$o_gl_build_range" || rc=$?
    [ "$rc" -eq 20 ] && drift=$((drift + 1))
    [ "$rc" -eq 11 ] && unknown=$((unknown + 1))
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
  local p_fps_imag p_latency_imag p_dantesync_locked_imag
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
    # #463 review: imag-nb's own fps + genlock-latency pins, validated here too (offline, in
    # CI) so a malformed/missing value is caught before ever needing a live SSH run.
    p_fps_imag="$(pinned_setting "$readme" output_fps_imag)"
    p_latency_imag="$(pinned_setting "$readme" genlock_latency_ms_imag)"
    # #489: imag-nb's dantesync clock-lock pin, validated here too (offline, in CI) for the same
    # reason as the fps/latency pins above.
    p_dantesync_locked_imag="$(pinned_setting "$readme" dantesync_locked_imag)"
    if [ "$mode" = "check-pins" ]; then
      check_pins "$readme" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps_strih" "$p_fps_stream" "$p_genlock" "$p_latency" "$p_plugin" \
        "$p_src_lat_strih" "$p_src_lat_stream" "$p_fps_imag" "$p_latency_imag" "$p_dantesync_locked_imag"
      exit $?
    fi
  fi

  # --compare / --status: collect observed key=val pairs (both facets read the same inputs).
  local host="" o_obs="" o_distroav="" o_ndi="" o_fps="" o_genlock="" o_latency="" o_plugin="" pair k v
  local manifest="" o_obs_sha="" o_distroav_sha="" o_capability="" o_bundle_hashes="" o_burn="" o_src_latency=""
  local o_av_sync_calibrated_ms="" o_genlock_build_sha=""
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
      genlock_build_sha)  o_genlock_build_sha="$v" ;; # #548: deployed vendored-genlock commit (BUNDLE_MANIFEST.json .build_sha) — DYNAMIC staleness vs origin/main
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

  # #548: compute the deployed genlock-build staleness range HERE (impure — best-effort git fetch +
  # imag_genlock_range_log) so compare_observed stays pure/testable. Runs ONLY when genlock_build_sha=
  # was supplied; every historic --compare call (no such key) is unchanged. Mirrors the imag
  # gather/report split (git I/O in the caller, verdict in the pure fn). The 15s timeout bounds the
  # fetch the same way the imag path does (never blocks going live).
  local o_gl_build_rc=0 o_gl_build_range="" repo_root_c
  if [ -n "$o_genlock_build_sha" ]; then
    repo_root_c="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || repo_root_c=""
    if [ -z "$repo_root_c" ] || [ ! -d "$repo_root_c/.git" ]; then
      echo "WARN: could not resolve drift-guard.sh's own repo root — skipping ${host} genlock-build compare" >&2
      o_gl_build_rc=127
    else
      timeout 15 git -C "$repo_root_c" fetch origin --quiet 2>/dev/null \
        || echo "WARN: git fetch origin failed (or timed out) — comparing ${host} build against a possibly-stale origin/main" >&2
      o_gl_build_range="$(imag_genlock_range_log "$repo_root_c" "$o_genlock_build_sha")" || o_gl_build_rc=$?
    fi
  fi

  compare_observed "$host" "$p_obs" "$p_distroav" "$p_ndi" "$p_fps" "$p_genlock" "$p_latency" "$p_plugin" \
    "$o_obs" "$o_distroav" "$o_ndi" "$o_fps" "$o_genlock" "$o_latency" "$o_plugin" \
    "$manifest" "$o_obs_sha" "$o_distroav_sha" "$o_capability" "$o_bundle_hashes" "$o_burn" \
    "$p_src_lat_strih" "$p_src_lat_stream" "$o_src_latency" "$o_av_sync_calibrated_ms" \
    "$o_genlock_build_sha" "$o_gl_build_rc" "$o_gl_build_range"
}

main "$@"
