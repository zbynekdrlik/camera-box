#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + printers, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`, so
# every pipeline here survives a no-match under that strict mode.
#
# scripts/lib/genlock-stats-abi.sh -- issue 1302: the stats-ABI gate on a FAST genlock deploy.
#
# WHY: `deploy-genlock-fleet.sh --fast` swaps obs.dll alone and keeps the frontend (obs64.exe) of the
# last full-bundle deploy. The frontend allocates `struct obs_genlock_stats` on its stack and passes no
# size to `obs_source_get_genlock_stats`, so a newer obs.dll that fills a bigger struct
# (`OBS_GENLOCK_STATS_VERSION` 2 -> 3 -> 4) writes past the frontend's copy and OBS crashes. Design:
# issue 1302 comment 6028838843 (Approach 1, part 2).
#
# The mechanism:
#   * every full-bundle deploy the planner drives records its frontend's stats version in
#     GENLOCK_STATS_ABI.txt next to GENLOCK_BUILD_SHA.txt (the Windows program clears it at step (3c)
#     before the copy and writes it at step (5b); the Linux genlock_write_markers 5th arg). An
#     install that cannot name its version (setup-imag.sh's own provisioning) removes it;
#   * the planner reads the version of the build it deploys from that build's own obs.h
#     (`git show <sha>:vendor/obs-studio/libobs/obs.h`), never from the checkout;
#   * the FAST program reads the box's marker FIRST and refuses (exit 13) when it is missing or names
#     another version, before anything on the box changes.
#
# genlock_fast_abi_verdict is the REFERENCE decision; what decides on the box is its transcription,
# the PowerShell gate the FAST program carries (genlock_fast_abi_gate_ps).
# tests/python/test_genlock_stats_abi_1302.py runs both on the same vectors and requires the same
# verdict and the same refusal text.
#
# Scope: the gate compares OBS_GENLOCK_STATS_VERSION. struct obs_genlock_output_stats is on the
# frontend's stack too (OBS_GENLOCK_OUTPUT_STATS_VERSION 1, never bumped). Until the marker records
# its version as well, a Windows --fast is refused at plan time unless the deployed commit's output
# version is GENLOCK_STATS_ABI_OUTPUT_COVERED, and the pytest pins the struct body so a change fails CI.

# The obs.h path inside the repo, read at the deployed build's commit.
GENLOCK_STATS_ABI_OBS_H="vendor/obs-studio/libobs/obs.h"
# The box marker file name (next to GENLOCK_BUILD_SHA.txt).
GENLOCK_STATS_ABI_MARKER="GENLOCK_STATS_ABI.txt"
# The OBS_GENLOCK_OUTPUT_STATS_VERSION a fast deploy is known safe for (the gate does not compare it yet).
GENLOCK_STATS_ABI_OUTPUT_COVERED="1"

# genlock_stats_abi_is_version TEXT -> 0 when TEXT is a stats version (a positive integer without a
# leading zero, at most 9 digits), else 1. The PowerShell gate uses the same pattern.
genlock_stats_abi_is_version() {
  [[ "${1:-}" =~ ^[1-9][0-9]{0,8}$ ]]
}

# genlock_stats_abi_from_obs_h [DEFINE]  (stdin: the text of an obs.h) -> the value of DEFINE
#   (default OBS_GENLOCK_STATS_VERSION). rc 1 (nothing printed) when the define is absent, defined more
#   than once, or not a version; rc 2 for a DEFINE that is not a C macro name.
genlock_stats_abi_from_obs_h() {
  local define="${1:-OBS_GENLOCK_STATS_VERSION}" lines value
  [[ "$define" =~ ^[A-Z_][A-Z0-9_]*$ ]] || return 2
  lines="$(grep -E "^[[:space:]]*#[[:space:]]*define[[:space:]]+${define}([[:space:]]|\$)" || true)"
  # two defines leave two lines in the value, which the version pattern (one whole string) refuses
  value="$(printf '%s\n' "$lines" | sed -E "s/^[[:space:]]*#[[:space:]]*define[[:space:]]+${define}[[:space:]]*//; s/[[:space:]]*(\\/[*/].*)?\$//")"
  genlock_stats_abi_is_version "$value" || return 1
  printf '%s\n' "$value"
}

# genlock_stats_abi_at_sha SHA REPO [DEFINE] -> the value of DEFINE (default OBS_GENLOCK_STATS_VERSION)
#   in the build at commit SHA, read from its own obs.h in the git checkout REPO. rc 1 (nothing printed)
#   when SHA is not a hex commit id, the commit or the file is not in REPO (fetch it first), or the
#   define is unreadable.
genlock_stats_abi_at_sha() {
  local sha="${1:-}" repo="${2:-}" define="${3:-OBS_GENLOCK_STATS_VERSION}" text
  [[ "$sha" =~ ^[0-9a-fA-F]{7,40}$ ]] || return 1
  [ -n "$repo" ] || return 1
  text="$(git -C "$repo" show "${sha}:${GENLOCK_STATS_ABI_OBS_H}" 2>/dev/null)" || return 1
  printf '%s\n' "$text" | genlock_stats_abi_from_obs_h "$define"
}

# genlock_fast_abi_verdict NEW_ABI MARKER_STATE [MARKER_TEXT] -- the FAST deploy's decision.
#   NEW_ABI      the new obs.dll's stats version (the planner's read; "" = unknown).
#   MARKER_STATE "present" (the box has GENLOCK_STATS_ABI.txt, its content is MARKER_TEXT) or "missing".
#   MARKER_TEXT  the marker's content; all whitespace is ignored (the Windows writer ends it in CRLF).
# Prints ONE line and returns 0 = the fast deploy may run, 1 = REFUSED, 2 = usage error:
#   OK frontend stats ABI vN == new obs.dll vN
#   REFUSED frontend stats ABI <vN|missing (no GENLOCK_STATS_ABI.txt)|unreadable (GENLOCK_STATS_ABI.txt is
#     not a version)>, new obs.dll <vM|unknown>: a full-bundle deploy is required
genlock_fast_abi_verdict() {
  local new="${1:-}" state="${2:-}" text="${3:-}" box="" box_text new_text
  case "$state" in
    present)
      text="$(printf '%s' "$text" | tr -d '[:space:]')"
      if genlock_stats_abi_is_version "$text"; then
        box="$text"; box_text="v$text"
      else
        box_text="unreadable ($GENLOCK_STATS_ABI_MARKER is not a version)"
      fi
      ;;
    missing) box_text="missing (no $GENLOCK_STATS_ABI_MARKER)" ;;
    *) echo "genlock_fast_abi_verdict: MARKER_STATE must be present|missing, got '$state'" >&2; return 2 ;;
  esac
  if genlock_stats_abi_is_version "$new"; then new_text="v$new"; else new_text="unknown"; new=""; fi
  if [ -n "$box" ] && [ "$box" = "$new" ]; then
    printf 'OK frontend stats ABI v%s == new obs.dll v%s\n' "$box" "$new"
    return 0
  fi
  printf 'REFUSED frontend stats ABI %s, new obs.dll %s: a full-bundle deploy is required\n' "$box_text" "$new_text"
  return 1
}

# genlock_fast_abi_gate_ps MODE NEW_ABI -> step (0f) of the Windows deploy program. FAST: the
#   PowerShell transcription of genlock_fast_abi_verdict over the box's marker, refusing with exit 13
#   before anything on the box changes. FULL: nothing (the full bundle replaces the frontend too).
genlock_fast_abi_gate_ps() {
  local mode="${1:-}" new="${2:-}"
  [ "$mode" = "fast" ] || return 0
  # the planner validated it; anything else is unknown and the gate refuses
  genlock_stats_abi_is_version "$new" || new=""
  cat <<PS
# (0f) issue 1302: FAST only -- the stats-ABI gate, before anything on this box changes. A fast deploy
#      swaps obs.dll under the frontend of the last full-bundle deploy; an obs.dll that fills a bigger
#      genlock stats struct writes past that frontend's copy and OBS crashes. The full-bundle deploy
#      recorded its frontend's version in ${GENLOCK_STATS_ABI_MARKER}; the planner read the new build's
#      from its own obs.h. Missing or different = REFUSED, the program ends with code 13.
\$abiNew  = '${new}'
\$abiFile = Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}'
\$abiBox  = ''
if (Test-Path -LiteralPath \$abiFile -PathType Leaf) {
  \$abiRaw = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes(\$abiFile)) -replace '\\s', ''
  if (\$abiRaw -match '^[1-9][0-9]{0,8}\$') { \$abiBox = \$abiRaw; \$abiBoxText = "v\$abiRaw" }
  else { \$abiBoxText = 'unreadable (${GENLOCK_STATS_ABI_MARKER} is not a version)' }
} else {
  \$abiBoxText = 'missing (no ${GENLOCK_STATS_ABI_MARKER})'
}
if (\$abiNew -match '^[1-9][0-9]{0,8}\$') { \$abiNewText = "v\$abiNew" } else { \$abiNewText = 'unknown'; \$abiNew = '' }
if (\$abiBox -eq '' -or \$abiBox -ne \$abiNew) {
  Write-Host "FAST DEPLOY REFUSED: frontend stats ABI \$abiBoxText, new obs.dll \${abiNewText}: a full-bundle deploy is required. Nothing on this box was changed."
  exit 13
}
Write-Host "stats ABI OK: frontend stats ABI v\$abiBox == new obs.dll v\$abiNew -- the fast deploy may swap obs.dll"
PS
}

# genlock_stats_abi_clear_ps MODE -> step (3c) of the Windows deploy program, right before the copy.
#   FULL: remove GENLOCK_STATS_ABI.txt, so a copy that fails half way never leaves a new frontend under
#   the old marker (step 5b writes the new version once every copy passed). FAST: nothing.
genlock_stats_abi_clear_ps() {
  [ "${1:-}" = "full" ] || return 0
  cat <<PS
# (3c) issue 1302: FULL -- remove ${GENLOCK_STATS_ABI_MARKER} before the copy; step (5b) records the new
#      build's version once every copy passed, so a half-done copy reads as "missing" to a fast deploy.
#      A marker it cannot remove stops the program here, before the copy, named, with the copy-failure code.
\$abiStale = Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}'
try {
  if (Test-Path -LiteralPath \$abiStale) { Remove-Item -LiteralPath \$abiStale -Force -ErrorAction Stop }
} catch {
  Write-Host "(3c) FAILED: could not remove \$abiStale (\$_) -- nothing was copied. OBS is stopped, and AutoHotkey64 + the keep-alive tasks stay as steps (1) and (1b) left them: bring them back by hand, then run the deploy again." -ForegroundColor Red
  exit 4
}
PS
}

# genlock_stats_abi_marker_ps MODE NEW_ABI -> step (5b) of the Windows deploy program, after the other
#   markers (it uses the program's Write-MarkerAtomic). FULL with a version: record it. FULL without one
#   (the planner could not read obs.h): REMOVE the marker, so a later fast deploy refuses until a full
#   deploy records a known version. FAST: leave it (it names the frontend, which a fast deploy keeps).
genlock_stats_abi_marker_ps() {
  local mode="${1:-}" new="${2:-}"
  if [ "$mode" = "fast" ]; then
    cat <<PS
# (5b) issue 1302: FAST -- ${GENLOCK_STATS_ABI_MARKER} is left as it is: it names the frontend, which this deploy
#      keeps, and step (0f) proved it equals the new obs.dll's version.
PS
  elif genlock_stats_abi_is_version "$new"; then
    cat <<PS
# (5b) issue 1302: record this full bundle's genlock stats ABI (read from the build's own obs.h) -- a
#      later fast deploy refuses an obs.dll whose version differs.
Write-MarkerAtomic (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}') '${new}'
PS
  else
    cat <<PS
# (5b) issue 1302: the planner could NOT read this build's genlock stats ABI (obs.h at the deployed
#      commit) -- the box's ${GENLOCK_STATS_ABI_MARKER} is REMOVED, so a fast deploy refuses until the
#      next full-bundle deploy records a known version.
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}')
Write-Warning "stats ABI unknown at plan time: ${GENLOCK_STATS_ABI_MARKER} removed -- a fast deploy will refuse until the next full-bundle deploy"
PS
  fi
}

# genlock_stats_abi_stage BUNDLE_DIR NEW_ABI -> write (a version) or remove (unknown) the bundle's
#   GENLOCK_STATS_ABI.txt before it ships to a Linux box, whose installer passes it to
#   genlock_write_markers. rc 1 on an I/O failure.
genlock_stats_abi_stage() {
  local dir="${1:-}" new="${2:-}"
  [ -d "$dir" ] || { echo "genlock_stats_abi_stage: bundle dir '$dir' not found" >&2; return 1; }
  if genlock_stats_abi_is_version "$new"; then
    printf '%s\n' "$new" > "$dir/$GENLOCK_STATS_ABI_MARKER" \
      || { echo "genlock_stats_abi_stage: cannot write $dir/$GENLOCK_STATS_ABI_MARKER" >&2; return 1; }
  else
    rm -f "$dir/$GENLOCK_STATS_ABI_MARKER" \
      || { echo "genlock_stats_abi_stage: cannot remove $dir/$GENLOCK_STATS_ABI_MARKER" >&2; return 1; }
  fi
}

# genlock_stats_abi_resolve SHA REPO MODE SWAPS_DLL [FETCH] -> the stats version of the build at SHA, or
#   "" when it cannot be read. FETCH=1 (execute mode; plan mode has no network) fetches that one commit
#   from origin before giving up (a full 40-hex SHA only: the anchor run's headSha is one, and the
#   tests' short SHAs never reach the network). SWAPS_DLL=1 when the box list holds a box a FAST
#   deploy swaps obs.dll on (fleet_boxes_swap_obs_dll). For such a FAST deploy "" is REFUSED (rc 3),
#   because the gate cannot compare an unknown version, and so is a commit whose
#   OBS_GENLOCK_OUTPUT_STATS_VERSION is not GENLOCK_STATS_ABI_OUTPUT_COVERED (the gate does not compare
#   that struct yet). Otherwise "" is allowed and named on stderr (each full-bundle box's marker is
#   removed).
genlock_stats_abi_resolve() {
  local sha="${1:-}" repo="${2:-}" mode="${3:-}" swaps="${4:-0}" fetch="${5:-0}" abi out rc=0 fast=0
  [ "$mode" = "fast" ] && [ "$swaps" = "1" ] && fast=1
  abi="$(genlock_stats_abi_at_sha "$sha" "$repo")" || abi=""
  # a fetch needs the full object id (git cannot fetch an abbreviation); the anchor run's headSha is one
  if [ -z "$abi" ] && [ "$fetch" = "1" ] && [[ "$sha" =~ ^[0-9a-fA-F]{40}$ ]]; then
    echo "# genlock stats ABI: $sha is not readable in $repo -- fetching origin once" >&2
    # the one commit only (never every branch): bounded, no credential prompt; git's reason is kept
    out="$(GIT_TERMINAL_PROMPT=0 timeout 120 git -C "$repo" fetch -q origin "$sha" 2>&1)" || rc=$?
    [ "$rc" = 0 ] || printf '# git fetch origin %s failed (rc %s): %s\n' "$sha" "$rc" "${out:-no output}" >&2
    abi="$(genlock_stats_abi_at_sha "$sha" "$repo")" || abi=""
  fi
  if [ -n "$abi" ]; then
    echo "# genlock stats ABI of $sha: v$abi (${GENLOCK_STATS_ABI_OBS_H} at that commit)" >&2
    if [ "$fast" = 1 ]; then
      out="$(genlock_stats_abi_at_sha "$sha" "$repo" OBS_GENLOCK_OUTPUT_STATS_VERSION)" || out=""
      if [ "$out" != "$GENLOCK_STATS_ABI_OUTPUT_COVERED" ]; then
        echo "ERROR: struct obs_genlock_output_stats is version ${out:-unreadable} at $sha -- the --fast stats-ABI gate covers only version ${GENLOCK_STATS_ABI_OUTPUT_COVERED} of it (it does not compare that struct yet, issue 1302); deploy --full" >&2
        return 3
      fi
    fi
    printf '%s\n' "$abi"
    return 0
  fi
  if [ "$fast" = 1 ]; then
    echo "ERROR: cannot read OBS_GENLOCK_STATS_VERSION from ${GENLOCK_STATS_ABI_OBS_H} at $sha in $repo -- a --fast deploy cannot be gated on an unknown stats ABI; fetch the commit (git fetch origin) or deploy --full" >&2
    return 3
  fi
  echo "WARNING: cannot read OBS_GENLOCK_STATS_VERSION from ${GENLOCK_STATS_ABI_OBS_H} at $sha in $repo -- this deploy REMOVES each full-bundle box's ${GENLOCK_STATS_ABI_MARKER}, so a later --fast refuses until a full deploy records a known version" >&2
  return 0
}
