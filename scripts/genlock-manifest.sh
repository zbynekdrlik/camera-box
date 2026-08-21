#!/usr/bin/env bash
#
# genlock-manifest.sh — emit the per-component SHA manifest for the windows-genlock bundle (#120).
#
# Part of EPIC #125 (robust version integrity). #119's root cause was a windows-genlock artifact
# that shipped a pre-#97 DistroAV (a stale prebuilt DLL) while the version pins still passed: the
# preload knob was inert because the bytes on the box were an OLD build of the right *version*.
# drift-guard (#45) only checks MARKETING versions (6.2.1), so it could not catch that. The fix is
# a MANIFEST shipped INSIDE the bundle that records the exact BUILD of every shipped component:
#
#   * the pinned SOURCE of each rebuilt-from-source component (OBS, DistroAV) — version + the
#     vendored subtree commit, read from vendor/README.md (the pin manifest) cross-checked against
#     vendor/distroav/buildspec.json (the same source-of-truth drift-guard.sh uses), AND
#   * the per-file sha256 of every file actually in the bundle — walked from the staged directory,
#     so the file list is SELF-CONSISTENT with what ships by construction.
#
# #121 (post-deploy verify) later byte/SHA-checks a deployed stack against this manifest; #122
# (drift-guard per-component BUILD SHA) consumes it too. THIS script only PRODUCES the manifest.
#
# Like scripts/drift-guard.sh and scripts/update-av-stack.sh, the file is split into PURE functions
# (manifest synthesis, pin parsing, self-consistency check — unit-tested from tests/genlock_manifest.rs
# by sourcing this file) and a flow that runs only when executed directly. The source-guard at the
# bottom (BASH_SOURCE != $0) lets the tests exercise the pure functions in isolation.
#
# Runs in the windows-genlock build's staging step (the windows-2022 runner ships git-bash, so
# bash + sha256sum + git are available — the workflow already uses `shell: bash` for the cmake
# steps) AND on the Linux per-PR CI (the Rust `test` job exercises it against a synthetic stage
# dir, proving the manifest logic without the 150-min Windows build).
#
# Usage:
#   scripts/genlock-manifest.sh --stage DIR [--readme PATH] [--buildspec PATH] [--build-sha SHA] \
#                               [--out FILE]
#       Generate the manifest for the staged bundle at DIR. Writes JSON to --out (default
#       DIR/BUNDLE_MANIFEST.json) and prints it to stdout. Pins come from --readme
#       (default vendor/README.md) + --buildspec (default vendor/distroav/buildspec.json).
#       --build-sha overrides the genlock-monorepo build SHA (default: `git rev-parse HEAD`).
#   scripts/genlock-manifest.sh --check FILE --stage DIR
#       Self-consistency check: every file listed in manifest FILE exists in stage DIR with the
#       recorded sha256, AND every regular file in DIR (except the manifest itself) is listed.
#       Exit 0 = consistent, 21 = INCONSISTENT, 1 = usage/IO error.
#   scripts/genlock-manifest.sh --help
#
# Exit codes: 0 = OK, 21 = manifest self-inconsistent (--check), 1 = usage/IO error.

set -euo pipefail

DEFAULT_README="vendor/README.md"
DEFAULT_BUILDSPEC="vendor/distroav/buildspec.json"
MANIFEST_SCHEMA="camera-box/genlock-bundle-manifest@1"

# --- PURE functions (no network, no git mutation beyond an optional `git rev-parse`) -----------

# sha256_of FILE -> the lowercase 64-hex sha256 of FILE. Uses sha256sum (Linux + git-bash on the
# windows-2022 runner both ship it); falls back to `shasum -a 256` (macOS dev box). Empty on error.
sha256_of() {
  local f="$1"
  [ -f "$f" ] || { echo "sha256_of: no such file: $f" >&2; return 1; }
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$f" | awk '{print $1}'
  else
    echo "sha256_of: no sha256sum or shasum available" >&2; return 1
  fi
}

# pinned_subtree_version README PREFIX -> the **bold** version on PREFIX's subtree table row.
# Identical parse to drift-guard.sh::pinned_subtree_version (single source-of-truth: the README pin
# table). The trailing `|| true` keeps a no-match from tripping the caller's set -e.
pinned_subtree_version() {
  local readme="$1" prefix="$2"
  [ -f "$readme" ] || { echo "pinned_subtree_version: no such file: $readme" >&2; return 1; }
  grep -E "$prefix" "$readme" | grep 'subtree' \
    | sed -n 's/.*\*\*\([0-9][0-9.]*\)\*\*.*/\1/p' | head -1 || true
}

# pinned_subtree_commit README PREFIX -> the `commit` hash on PREFIX's subtree table row
# (e.g. "7546be726"). The README row is `… (commit `<hash>`) …`; the back-ticked token after the
# word "commit" is the vendored subtree's upstream import commit.
pinned_subtree_commit() {
  local readme="$1" prefix="$2"
  [ -f "$readme" ] || { echo "pinned_subtree_commit: no such file: $readme" >&2; return 1; }
  # The backticks below sit inside a SINGLE-quoted sed program — they are LITERAL markdown
  # delimiters, not command substitution (the intentional no-expand SC2016 reports).
  # shellcheck disable=SC2016
  grep -E "$prefix" "$readme" | grep 'subtree' \
    | sed -n 's/.*commit `\([0-9a-f][0-9a-f]*\)`.*/\1/p' | head -1 || true
}

# pinned_ndi_min README -> the "NDI >= X.Y.Z" minimum the DistroAV plugin requires (e.g. "6.3.0").
# Same parse as drift-guard.sh::pinned_ndi_min.
pinned_ndi_min() {
  local readme="$1"
  [ -f "$readme" ] || { echo "pinned_ndi_min: no such file: $readme" >&2; return 1; }
  grep -E 'NDI[^0-9]*[0-9]+\.[0-9]+\.[0-9]+' "$readme" \
    | sed -n 's/.*NDI[^0-9]*\([0-9][0-9.]*\).*/\1/p' | head -1 || true
}

# buildspec_version FILE -> top-level "version" of a DistroAV buildspec.json (the vendored source
# of truth for the DistroAV marketing version). Identical to drift-guard.sh::buildspec_version, so
# the manifest's DistroAV pin and drift-guard's pin can never silently disagree.
buildspec_version() {
  local f="$1"
  [ -f "$f" ] || return 1
  if command -v jq >/dev/null 2>&1; then
    jq -r '.version // empty' "$f" 2>/dev/null
  else
    grep -E '^    "version":' "$f" | head -1 \
      | sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p'
  fi
}

# json_escape STR -> STR with the JSON-significant characters (\ and ") escaped, for safe
# interpolation into the hand-built JSON below (paths can contain neither newline nor control char
# in this bundle, so backslash + quote cover the realistic cases; backslash MUST be escaped first).
# NOTE: this escapes on WRITE. The text read-back parser `manifest_listed_files` uses a simple
# `"path": "[^"]*"` regex that stops at the first quote, so a path containing a LITERAL `"` would
# read back wrong and `--check` would (fail-closed) report INCONSISTENT — never mask a tamper. A
# Windows filename cannot contain `"`, so this is unreachable for the windows-genlock bundle.
json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

# stage_files DIR -> every regular file under DIR (relative paths, forward slashes, sorted),
# EXCLUDING the manifest file itself (BUNDLE_MANIFEST.json) — the manifest never lists itself, so a
# re-run is idempotent and the self-consistency check below has a stable expected set. Deterministic
# order (LC_ALL=C sort) so the manifest is reproducible. The trailing `|| true` keeps an EMPTY result
# (a stage with only the manifest, or no files) from the final `grep -vx` exiting 1 and tripping the
# caller's set -e — an empty stage is a legitimate (if degenerate) input, not an error.
stage_files() {
  local dir="$1"
  [ -d "$dir" ] || { echo "stage_files: no such dir: $dir" >&2; return 1; }
  ( cd "$dir" && find . -type f \
      | sed 's#^\./##' \
      | grep -vx 'BUNDLE_MANIFEST.json' \
      | LC_ALL=C sort ) || true
}

# assert_manifest_complete EXPECTED ACTUAL -> 0 if EXPECTED == ACTUAL, else FAIL LOUD (exit 22).
# The generator's own completeness backstop (#239 follow-up): generate walks the staged file list
# ONCE and emits one files[] entry per file. If those two counts ever diverge — a truncated find /
# process-substitution stream on git-bash, an aborted loop — the manifest is PARTIAL and must NEVER
# silently pass to --check. This dies AT generation, naming both counts so the truncation is
# debuggable. Exit 22 (distinct from --check's 21) so a generation-completeness failure is told apart
# from a self-consistency failure. (#239's checker race made a COMPLETE manifest look short; this is
# the inverse guard — a genuinely short manifest can't ship.)
assert_manifest_complete() {
  local expected="$1" actual="$2"
  if [ "$expected" != "$actual" ]; then
    echo "!! INCOMPLETE MANIFEST: staged $expected files but wrote $actual files[] entries" >&2
    echo "!! a truncated generate must not ship — failing loud at generation (not at --check)" >&2
    return 22
  fi
  echo "manifest completeness: $expected staged == $actual manifested — OK" >&2
  return 0
}

# assert_entry_valid PATH SHA SIZE -> 0 if SHA is a non-empty 64-hex-lowercase digest AND SIZE is a
# non-empty integer byte count, else FAIL LOUD (exit 23, distinct from 21 --check / 22 completeness).
# The per-entry teeth of the #239 cmdsub-set-e fix: main() runs generate as
# `manifest="$(generate_manifest …)"`, and a command substitution on an assignment RHS SUPPRESSES
# `set -e` inside it — so a mid-loop `sha256_of`/`wc -c` failure (a staged file briefly unreadable:
# concurrent writer, Windows AV lock, transient I/O) does NOT abort. The loop would then emit
# `{ "sha256": "", "size":  }` (empty sha + value-less size = invalid JSON), `written` would still
# increment, and the completeness count (every PATH present) would stay blind to it. Validating each
# emitted entry explicitly catches exactly what the count guard cannot.
assert_entry_valid() {
  local path="$1" sha="$2" size="$3"
  if ! grep -qE '^[0-9a-f]{64}$' <<<"$sha"; then
    echo "!! INVALID MANIFEST ENTRY: '$path' has a bad sha256 ('$sha') — expected 64 lowercase hex" >&2
    echo "!! a staged file's sha256 could not be computed (suppressed set -e in the generate cmdsub) — failing loud" >&2
    return 23
  fi
  if ! grep -qE '^[0-9]+$' <<<"$size"; then
    echo "!! INVALID MANIFEST ENTRY: '$path' has a bad size ('$size') — expected a non-negative integer byte count" >&2
    return 23
  fi
  return 0
}

# generate_manifest STAGE README BUILDSPEC BUILD_SHA -> the manifest JSON on stdout.
#   * components[]: the rebuilt-from-source pins (OBS, DistroAV) + the non-redistributable NDI
#     runtime minimum. DistroAV's version is taken from the vendored buildspec.json when present
#     (the true vendored source), falling back to the README pin; the README pin is recorded too so
#     a mismatch is auditable.
#   * files[]: every staged file with its sha256 + byte size — walked from STAGE, so the list is
#     SELF-CONSISTENT with the bundle by construction.
generate_manifest() {
  local stage="$1" readme="$2" buildspec="$3" build_sha="$4"
  [ -d "$stage" ] || { echo "generate_manifest: no such stage dir: $stage" >&2; return 1; }

  local obs_ver obs_commit da_ver_readme da_commit da_ver_buildspec da_ver ndi_min
  obs_ver="$(pinned_subtree_version "$readme" 'vendor/obs-studio')"
  obs_commit="$(pinned_subtree_commit "$readme" 'vendor/obs-studio')"
  da_ver_readme="$(pinned_subtree_version "$readme" 'vendor/distroav')"
  da_commit="$(pinned_subtree_commit "$readme" 'vendor/distroav')"
  ndi_min="$(pinned_ndi_min "$readme")"
  da_ver_buildspec=""
  [ -f "$buildspec" ] && da_ver_buildspec="$(buildspec_version "$buildspec" || true)"
  # The vendored buildspec is the authoritative DistroAV version (what actually built); the README
  # pin is the human-declared one. Prefer buildspec, record both so a drift is visible in-manifest.
  da_ver="${da_ver_buildspec:-$da_ver_readme}"

  local now
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  printf '{\n'
  printf '  "schema": "%s",\n' "$MANIFEST_SCHEMA"
  printf '  "generated_at": "%s",\n' "$now"
  printf '  "build_sha": "%s",\n' "$(json_escape "$build_sha")"
  printf '  "components": [\n'
  printf '    { "name": "obs-studio", "rebuilt_from_source": true, "pinned_version": "%s", "pinned_commit": "%s", "source": "vendor/obs-studio" },\n' \
    "$(json_escape "$obs_ver")" "$(json_escape "$obs_commit")"
  printf '    { "name": "distroav", "rebuilt_from_source": true, "pinned_version": "%s", "pinned_version_readme": "%s", "pinned_commit": "%s", "source": "vendor/distroav" },\n' \
    "$(json_escape "$da_ver")" "$(json_escape "$da_ver_readme")" "$(json_escape "$da_commit")"
  printf '    { "name": "ndi-runtime", "rebuilt_from_source": false, "redistributable": false, "min_version": "%s", "note": "installed per-box (licensing forbids bundling) — not a file in this bundle" }\n' \
    "$(json_escape "$ndi_min")"
  printf '  ],\n'
  printf '  "files": [\n'

  # Capture the staged file list ONCE into a variable, then iterate it with a here-string. This
  # deliberately AVOIDS `done < <(stage_files …)` process substitution: on Windows git-bash a
  # process-substitution FIFO can be cut short, silently truncating the loop (the exact failure mode
  # the #239 build LOOKED like). Reading a fully-materialised string can't be truncated mid-stream,
  # and it lets us count the INTENDED files before the loop and the WRITTEN entries after, then assert
  # they match (assert_manifest_complete) so a partial manifest dies loud at generation.
  local file_list expected
  file_list="$(stage_files "$stage")"
  # Count non-empty lines = files we intend to manifest (an empty list -> 0, the degenerate stage).
  expected="$(printf '%s\n' "$file_list" | grep -c . || true)"

  local first=1 rel abs sha size written=0
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    abs="$stage/$rel"
    sha="$(sha256_of "$abs")"
    size="$(wc -c < "$abs" | tr -d ' ')"
    # #239: validate the freshly-computed sha+size BEFORE emitting the entry. The explicit `|| return`
    # is load-bearing: this loop runs inside `manifest="$(generate_manifest …)"`, where `set -e` is
    # suppressed, so a silent empty sha/size must be aborted here by hand, not left to set -e.
    assert_entry_valid "$rel" "$sha" "$size" || return $?
    if [ "$first" -eq 1 ]; then first=0; else printf ',\n'; fi
    printf '    { "path": "%s", "sha256": "%s", "size": %s }' \
      "$(json_escape "$rel")" "$sha" "$size"
    written=$((written + 1))
  done <<< "$file_list"
  printf '\n  ]\n'
  printf '}\n'

  # Completeness backstop: every staged file MUST have produced exactly one files[] entry. A
  # divergence means the file list was truncated or the loop aborted -> fail loud, exit 22, never
  # emit a silently-partial manifest. (The JSON is already printed above; the manifest is only
  # CONSUMED after generate_manifest returns 0, so failing here aborts the caller before the partial
  # file is trusted — main() captures stdout, then only writes/uses it when this returns 0.)
  assert_manifest_complete "$expected" "$written"
}

# manifest_listed_files FILE -> the "path" of every files[] entry (one per line). Pure text parse
# (no jq dependency on the windows runner): the files[] entries are one-per-line `{ "path": "…", …`.
manifest_listed_files() {
  local f="$1"
  [ -f "$f" ] || { echo "manifest_listed_files: no such file: $f" >&2; return 1; }
  # `|| true` on the grep: an empty files[] (a degenerate stage with no listed files) is a no-match,
  # which would otherwise exit 1 and trip the caller's `set -e` / pipefail. An empty list is valid.
  { grep -oE '"path": "[^"]*"' "$f" || true; } | sed -n 's/"path": "\(.*\)"/\1/p'
}

# manifest_sha_for FILE PATH -> the recorded sha256 for files[] entry PATH ("" if absent).
manifest_sha_for() {
  local f="$1" path="$2"
  [ -f "$f" ] || { echo "manifest_sha_for: no such file: $f" >&2; return 1; }
  # Match the exact one-line entry for PATH and pull its sha256. `sed … ;q` quits sed itself after
  # the first match — a clean exit, NOT a downstream `head -1` that would SIGPIPE the upstream
  # grep/sed (the #239 SIGPIPE-under-pipefail class). `|| true` so a no-match (path absent) is not a
  # pipefail abort. Pattern anchored to the path entry so we read the right line's sha256.
  { grep -F "\"path\": \"$path\"" "$f" || true; } \
    | sed -n '/"sha256"/{s/.*"sha256": "\([0-9a-f]*\)".*/\1/p;q}'
}

# check_consistency FILE STAGE -> 0 if the manifest is SELF-CONSISTENT with STAGE: every listed
# file exists in STAGE with the recorded sha256, AND every regular file in STAGE (minus the
# manifest) is listed. Returns 21 on ANY mismatch (extra, missing, or sha drift). This is the
# in-CI proof that the produced manifest matches the bundle — the property #121 relies on.
#
# The two set-membership directions (staged-not-listed, listed-not-staged) are computed in a SINGLE
# PASS each with `comm` over LC_ALL=C-sorted lists — NOT a `printf | grep -qxF` re-scan once per
# file (#239). That per-item pipe was both O(n²) (each of ~2000 staged files re-grepped the whole
# 2000-line list) and, fatally, RACY: `grep -q` exits early on a match → SIGPIPE to the upstream
# `printf` → under `set -euo pipefail` the SIGPIPE poisoned the pipeline's exit status
# nondeterministically, so ~half a perfectly-valid bundle's files were falsely flagged "not in
# manifest" and the build failed (exit 21) on a correct manifest. `comm` reads each list once, no
# per-item subshell, no SIGPIPE.
check_consistency() {
  local f="$1" stage="$2" rc=0
  [ -f "$f" ] || { echo "check_consistency: no such manifest: $f" >&2; return 1; }
  [ -d "$stage" ] || { echo "check_consistency: no such stage dir: $stage" >&2; return 1; }

  local listed actual
  listed="$(manifest_listed_files "$f" | LC_ALL=C sort)"
  actual="$(stage_files "$stage" | LC_ALL=C sort)"

  # Set-diff in one pass each. CRITICAL: `comm` MUST run under the SAME collation the lists were
  # sorted with (LC_ALL=C above) — comm decides "only in A / only in B / both" by stepping two
  # streams it assumes are sorted in ITS locale; a locale mismatch makes it see the lists as
  # "unsorted" and emit garbage (e.g. every file reported as differing). LC_ALL=C on every comm
  # keeps it byte-collated, matching the sort.
  #   comm -23 = lines only in actual  -> staged files the manifest does NOT list (un-audited bytes)
  #   comm -13 = lines only in listed  -> manifest entries with NO matching bundle file
  # `|| true` so an empty diff (no-match) does not trip `set -e`/pipefail.
  local extra missing rel
  extra="$(LC_ALL=C comm -23 <(printf '%s\n' "$actual") <(printf '%s\n' "$listed") || true)"
  missing="$(LC_ALL=C comm -13 <(printf '%s\n' "$actual") <(printf '%s\n' "$listed") || true)"

  if [ -n "$extra" ]; then
    while IFS= read -r rel; do
      [ -n "$rel" ] || continue
      echo "!! INCONSISTENT: staged file not in manifest: $rel" >&2
    done <<< "$extra"
    rc=21
  fi
  if [ -n "$missing" ]; then
    while IFS= read -r rel; do
      [ -n "$rel" ] || continue
      echo "!! INCONSISTENT: manifest lists a file absent from the bundle: $rel" >&2
    done <<< "$missing"
    rc=21
  fi

  # For every file present in BOTH sets, the recorded sha256 must match the bundle's actual bytes
  # (the anti-#119 property: pins the BYTES, not just the version). Only files in both sets can drift;
  # extras/missing are already reported above. No `grep -q`/`head` early-exit pipe here.
  local both want got
  both="$(LC_ALL=C comm -12 <(printf '%s\n' "$actual") <(printf '%s\n' "$listed") || true)"
  if [ -n "$both" ]; then
    while IFS= read -r rel; do
      [ -n "$rel" ] || continue
      want="$(manifest_sha_for "$f" "$rel")"
      got="$(sha256_of "$stage/$rel")"
      if [ "$want" != "$got" ]; then
        echo "!! INCONSISTENT: sha256 drift for $rel: manifest=$want bundle=$got" >&2; rc=21
      fi
    done <<< "$both"
  fi

  if [ "$rc" -eq 0 ]; then
    echo "manifest self-consistent with $stage ($(printf '%s\n' "$actual" | grep -c . ) files) — OK"
  fi
  return "$rc"
}

# --- FLOW (runs only when executed directly) --------------------------------------------------

usage() {
  sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^#//'
}

main() {
  local mode="generate" stage="" readme="$DEFAULT_README" buildspec="$DEFAULT_BUILDSPEC"
  local build_sha="" out="" check_file=""

  while [ $# -gt 0 ]; do
    case "$1" in
      --stage)     stage="$2"; shift 2 ;;
      --readme)    readme="$2"; shift 2 ;;
      --buildspec) buildspec="$2"; shift 2 ;;
      --build-sha) build_sha="$2"; shift 2 ;;
      --out)       out="$2"; shift 2 ;;
      --check)     mode="check"; check_file="$2"; shift 2 ;;
      --help|-h)   usage; exit 0 ;;
      *) echo "unknown argument: $1" >&2; usage; exit 1 ;;
    esac
  done

  [ -n "$stage" ] || { echo "ERROR: --stage DIR is required" >&2; exit 1; }
  [ -d "$stage" ] || { echo "ERROR: stage dir not found: $stage" >&2; exit 1; }

  if [ "$mode" = "check" ]; then
    [ -n "$check_file" ] || { echo "ERROR: --check FILE is required" >&2; exit 1; }
    check_consistency "$check_file" "$stage"
    exit $?
  fi

  # generate
  [ -n "$build_sha" ] || build_sha="$(git rev-parse HEAD 2>/dev/null || echo UNKNOWN)"
  [ -n "$out" ] || out="$stage/BUNDLE_MANIFEST.json"

  local manifest
  manifest="$(generate_manifest "$stage" "$readme" "$buildspec" "$build_sha")"
  printf '%s\n' "$manifest" > "$out"
  printf '%s\n' "$manifest"
  echo "wrote $out" >&2

  # Self-verify what we just wrote (the manifest never lists itself, so this passes by
  # construction — it is the producer's own loud guard against a generation bug).
  check_consistency "$out" "$stage" >&2
}

# Source guard: when sourced (tests), expose the pure functions without running the flow.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
