#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + printers, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`, so
# every pipeline here survives a no-match under that strict mode.
#
# scripts/lib/genlock-stats-abi.sh -- issue 1302: the stats-ABI gate on a FAST genlock deploy.
#
# WHY: `deploy-genlock-fleet.sh --fast` swaps obs.dll alone and keeps the frontend (obs64.exe) of the
# last full-bundle deploy. The frontend allocates `struct obs_genlock_stats` AND `struct
# obs_genlock_output_stats` on its stack and passes no size to `obs_source_get_genlock_stats` /
# `obs_output_get_genlock_stats`, so a newer obs.dll that fills a bigger struct
# (`OBS_GENLOCK_STATS_VERSION` 2 -> 3 -> 4, or a bumped `OBS_GENLOCK_OUTPUT_STATS_VERSION`) writes past
# the frontend's copy and OBS crashes. Design: issue 1302 comment 6028838843 (Approach 1, part 2);
# ROZHODNUTÉ 6030159870 item 1 (the output-stats struct too).
#
# The ABI of a build is the PAIR of its two struct versions. The planner passes it as ONE value
# `<stats>:<output_stats>` (e.g. `4:1`); a part that is not a version is unknown. The box marker
# GENLOCK_STATS_ABI.txt holds it as two lines:
#     4
#     output_stats=1
#
# The mechanism:
#   * every full-bundle deploy the planner drives records its frontend's pair in GENLOCK_STATS_ABI.txt
#     next to GENLOCK_BUILD_SHA.txt (the Windows program clears it at step (3c) before the copy and
#     writes it at step (5b); the Linux genlock_write_markers 5th arg). A deploy that cannot name BOTH
#     versions, and setup-imag.sh's own provisioning, remove it;
#   * the planner reads both versions of the build it deploys from that build's own obs.h
#     (`git show <sha>:vendor/obs-studio/libobs/obs.h`), never from the checkout;
#   * the FAST program reads the box's marker FIRST and refuses (exit 13) when it is missing or a
#     version differs, naming whichever struct differs, before anything on the box changes. A marker
#     with only its first line (written before the output-stats line existed) refuses as missing
#     output stats.
#
# genlock_fast_abi_verdict is the REFERENCE decision; what decides on the box is its transcription,
# the PowerShell gate the FAST program carries (genlock_fast_abi_gate_ps).
# tests/python/test_genlock_stats_abi_1302.py runs both on the same vectors and requires the same
# verdict and the same refusal text. A struct change must bump its version (the pytest pins each
# struct body to its version), or the gate cannot see it.

# The obs.h path inside the repo, read at the deployed build's commit.
GENLOCK_STATS_ABI_OBS_H="vendor/obs-studio/libobs/obs.h"
# The box marker file name (next to GENLOCK_BUILD_SHA.txt).
GENLOCK_STATS_ABI_MARKER="GENLOCK_STATS_ABI.txt"

# genlock_stats_abi_is_version TEXT -> 0 when TEXT is a stats version (a positive integer without a
# leading zero, at most 9 digits), else 1. The PowerShell gate uses the same pattern. Every bash
# pattern here spells its digits out: a bracket RANGE such as [1-9] follows the locale's collation,
# and under en_US.UTF-8 it also matches non-ASCII digits the gate refuses (review round 1).
genlock_stats_abi_is_version() {
  [[ "${1:-}" =~ ^[123456789][0123456789]{0,8}$ ]]
}

# genlock_stats_abi_part PAIR 1|2 -> the stats (1) or output-stats (2) version of PAIR
#   (`<stats>:<output_stats>`), or nothing when that part is not a version. Part 1 is the text before
#   the first `:`, part 2 the text after it (none without a `:`), so `4` (a bare stats version) has no
#   part 2 and `4:1:2` has the part 2 `1:2`, which is not a version. Always rc 0.
genlock_stats_abi_part() {
  local pair="${1:-}" v=""
  case "${2:-}" in
    1) v="${pair%%:*}" ;;
    2) [[ "$pair" == *:* ]] && v="${pair#*:}" ;;
  esac
  genlock_stats_abi_is_version "$v" && printf '%s\n' "$v"
  return 0
}

# genlock_stats_abi_is_pair PAIR -> 0 when both parts of PAIR are versions (and nothing follows), else 1.
genlock_stats_abi_is_pair() {
  [[ "${1:-}" =~ ^[123456789][0123456789]{0,8}:[123456789][0123456789]{0,8}$ ]]
}

# _genlock_stats_abi_marker_fields (stdin: a GENLOCK_STATS_ABI.txt's text) -> two lines: what the
#   marker says about the stats struct, then about the output-stats struct, each "v<N>" or the reason
#   it names no version. Lines are split on LF; every ASCII whitespace character inside a line is
#   ignored (the Windows writer ends each line in CRLF) and a line left empty is skipped. The first
#   line must be a version; the second `output_stats=<version>` (case-sensitive); no third line. The
#   PowerShell gate transcribes it (ASCII-decoded bytes, `-split '\n'`, `-replace '\s'`, `-cmatch`).
_genlock_stats_abi_marker_fields() {
  local line n=0 l1="" l2=""
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line//[$' \t\r\v\f']/}"
    [ -n "$line" ] || continue
    n=$((n + 1))
    case "$n" in 1) l1="$line" ;; 2) l2="$line" ;; esac
  done
  if genlock_stats_abi_is_version "$l1"; then
    printf 'v%s\n' "$l1"
  else
    printf 'unreadable (line 1 of %s is not a version)\n' "$GENLOCK_STATS_ABI_MARKER"
  fi
  if [ "$n" -lt 2 ]; then
    printf 'missing (no output_stats line in %s)\n' "$GENLOCK_STATS_ABI_MARKER"
  elif [ "$n" -gt 2 ]; then
    printf 'unreadable (%s has more than two lines)\n' "$GENLOCK_STATS_ABI_MARKER"
  elif [[ "$l2" =~ ^output_stats=([123456789][0123456789]{0,8})$ ]]; then
    printf 'v%s\n' "${BASH_REMATCH[1]}"
  else
    printf 'unreadable (line 2 of %s is not output_stats=<version>)\n' "$GENLOCK_STATS_ABI_MARKER"
  fi
}

# genlock_stats_abi_pair_from_marker (stdin: a GENLOCK_STATS_ABI.txt's text) -> its `<stats>:<output_stats>`
#   pair when it names both versions, else nothing. Always rc 0. setup-strih.sh reads the planner's
#   staged marker back with it. A NUL byte, which bash read would drop silently, is read as '?'
#   (neither whitespace nor a digit), the way the PowerShell gate reads it.
genlock_stats_abi_pair_from_marker() {
  local fields s o
  fields="$(tr '\000' '?' | _genlock_stats_abi_marker_fields)"
  s="${fields%%$'\n'*}"; o="${fields#*$'\n'}"
  if [[ "$s" =~ ^v([123456789][0123456789]{0,8})$ ]]; then
    s="${BASH_REMATCH[1]}"
    [[ "$o" =~ ^v([123456789][0123456789]{0,8})$ ]] && printf '%s:%s\n' "$s" "${BASH_REMATCH[1]}"
  fi
  return 0
}

# genlock_stats_abi_marker_text PAIR -> the GENLOCK_STATS_ABI.txt content for a complete PAIR (two
#   lines, no trailing newline); rc 1 and nothing for anything else.
genlock_stats_abi_marker_text() {
  genlock_stats_abi_is_pair "${1:-}" || return 1
  printf '%s\noutput_stats=%s' "${1%%:*}" "${1#*:}"
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
  [[ "$sha" =~ ^[0123456789abcdefABCDEF]{7,40}$ ]] || return 1
  [ -n "$repo" ] || return 1
  text="$(git -C "$repo" show "${sha}:${GENLOCK_STATS_ABI_OBS_H}" 2>/dev/null)" || return 1
  printf '%s\n' "$text" | genlock_stats_abi_from_obs_h "$define"
}

# _genlock_stats_abi_version_text VERSION -> "v<VERSION>", or "unknown" for an empty one.
_genlock_stats_abi_version_text() {
  if [ -n "${1:-}" ]; then printf 'v%s\n' "$1"; else printf 'unknown\n'; fi
}

# genlock_fast_abi_verdict NEW_PAIR MARKER_STATE [MARKER_TEXT] -- the FAST deploy's decision.
#   NEW_PAIR     the new obs.dll's `<stats>:<output_stats>` pair (the planner's read; a part that is
#                not a version is unknown, genlock_stats_abi_part).
#   MARKER_STATE "present" (the box has GENLOCK_STATS_ABI.txt, its content is MARKER_TEXT) or "missing".
#   MARKER_TEXT  the marker's content, read by _genlock_stats_abi_marker_fields.
# Each struct is proven equal only when the box names a version, the new obs.dll names one and they
# are the same. Prints ONE line and returns 0 = the fast deploy may run, 1 = REFUSED, 2 = usage error:
#   OK frontend stats ABI vN == new obs.dll vN; frontend output stats ABI vK == new obs.dll vK
#   REFUSED <part>[; <part>]: a full-bundle deploy is required
# where each struct NOT proven equal adds one part, the stats struct first:
#   frontend stats ABI <vN|missing (...)|unreadable (...)>, new obs.dll <vM|unknown>
#   frontend output stats ABI <vN|missing (...)|unreadable (...)>, new obs.dll <vM|unknown>
genlock_fast_abi_verdict() {
  local new="${1:-}" state="${2:-}" text="${3:-}" fields box_s box_o new_s new_o refused=""
  case "$state" in
    present)
      fields="$(printf '%s' "$text" | _genlock_stats_abi_marker_fields)"
      box_s="${fields%%$'\n'*}"; box_o="${fields#*$'\n'}"
      ;;
    missing) box_s="missing (no $GENLOCK_STATS_ABI_MARKER)"; box_o="$box_s" ;;
    *) echo "genlock_fast_abi_verdict: MARKER_STATE must be present|missing, got '$state'" >&2; return 2 ;;
  esac
  new_s="$(genlock_stats_abi_part "$new" 1)"
  new_o="$(genlock_stats_abi_part "$new" 2)"
  if [ -z "$new_s" ] || [ "$box_s" != "v$new_s" ]; then
    refused="frontend stats ABI $box_s, new obs.dll $(_genlock_stats_abi_version_text "$new_s")"
  fi
  if [ -z "$new_o" ] || [ "$box_o" != "v$new_o" ]; then
    refused="${refused:+$refused; }frontend output stats ABI $box_o, new obs.dll $(_genlock_stats_abi_version_text "$new_o")"
  fi
  if [ -z "$refused" ]; then
    printf 'OK frontend stats ABI %s == new obs.dll v%s; frontend output stats ABI %s == new obs.dll v%s\n' \
      "$box_s" "$new_s" "$box_o" "$new_o"
    return 0
  fi
  printf 'REFUSED %s: a full-bundle deploy is required\n' "$refused"
  return 1
}

# genlock_fast_abi_gate_ps MODE NEW_PAIR -> step (0f) of the Windows deploy program. FAST: the
#   PowerShell transcription of genlock_fast_abi_verdict over the box's marker, refusing with exit 13
#   before anything on the box changes. FULL: nothing (the full bundle replaces the frontend too).
genlock_fast_abi_gate_ps() {
  local mode="${1:-}" new_s new_o
  [ "$mode" = "fast" ] || return 0
  # each part the planner validated; anything else is unknown and the gate refuses that struct
  new_s="$(genlock_stats_abi_part "${2:-}" 1)"
  new_o="$(genlock_stats_abi_part "${2:-}" 2)"
  cat <<PS
# (0f) issue 1302: FAST only -- the stats-ABI gate, before anything on this box changes. A fast deploy
#      swaps obs.dll under the frontend of the last full-bundle deploy; an obs.dll that fills a bigger
#      genlock stats struct (the source stats or the output stats) writes past that frontend's copy and
#      OBS crashes. The full-bundle deploy recorded its frontend's two versions in
#      ${GENLOCK_STATS_ABI_MARKER} (line 1: the stats version, line 2: output_stats=<N>); the planner read
#      the new build's from its own obs.h. Missing or different = REFUSED, naming the struct, and the
#      program ends with code 13.
\$abiNew    = '${new_s}'
\$abiNewOut = '${new_o}'
\$abiFile   = Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}'
\$abiBox    = ''
\$abiBoxOut = ''
if (Test-Path -LiteralPath \$abiFile -PathType Leaf) {
  \$abiLines = @(([System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes(\$abiFile)) -split '\\n') | ForEach-Object { \$_ -replace '\\s', '' } | Where-Object { \$_ -ne '' })
  if (\$abiLines.Count -ge 1 -and \$abiLines[0] -cmatch '^[1-9][0-9]{0,8}\$') { \$abiBox = \$abiLines[0]; \$abiBoxText = "v\$abiBox" }
  else { \$abiBoxText = 'unreadable (line 1 of ${GENLOCK_STATS_ABI_MARKER} is not a version)' }
  if (\$abiLines.Count -lt 2) { \$abiBoxOutText = 'missing (no output_stats line in ${GENLOCK_STATS_ABI_MARKER})' }
  elseif (\$abiLines.Count -gt 2) { \$abiBoxOutText = 'unreadable (${GENLOCK_STATS_ABI_MARKER} has more than two lines)' }
  elseif (\$abiLines[1] -cmatch '^output_stats=([1-9][0-9]{0,8})\$') { \$abiBoxOut = \$Matches[1]; \$abiBoxOutText = "v\$abiBoxOut" }
  else { \$abiBoxOutText = 'unreadable (line 2 of ${GENLOCK_STATS_ABI_MARKER} is not output_stats=<version>)' }
} else {
  \$abiBoxText = 'missing (no ${GENLOCK_STATS_ABI_MARKER})'
  \$abiBoxOutText = \$abiBoxText
}
if (\$abiNew -cmatch '^[1-9][0-9]{0,8}\$') { \$abiNewText = "v\$abiNew" } else { \$abiNewText = 'unknown'; \$abiNew = '' }
if (\$abiNewOut -cmatch '^[1-9][0-9]{0,8}\$') { \$abiNewOutText = "v\$abiNewOut" } else { \$abiNewOutText = 'unknown'; \$abiNewOut = '' }
\$abiRefused = @()
if (\$abiNew -eq '' -or \$abiBox -ne \$abiNew) { \$abiRefused += "frontend stats ABI \$abiBoxText, new obs.dll \$abiNewText" }
if (\$abiNewOut -eq '' -or \$abiBoxOut -ne \$abiNewOut) { \$abiRefused += "frontend output stats ABI \$abiBoxOutText, new obs.dll \$abiNewOutText" }
if (\$abiRefused.Count -gt 0) {
  Write-Host "FAST DEPLOY REFUSED: \$(\$abiRefused -join '; '): a full-bundle deploy is required. Nothing on this box was changed."
  exit 13
}
Write-Host "stats ABI OK: frontend stats ABI v\$abiBox == new obs.dll v\$abiNew; frontend output stats ABI v\$abiBoxOut == new obs.dll v\$abiNewOut -- the fast deploy may swap obs.dll"
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

# genlock_stats_abi_marker_ps MODE NEW_PAIR -> step (5b) of the Windows deploy program, after the other
#   markers (it uses the program's Write-MarkerAtomic, which writes each array element as one line).
#   FULL with a complete pair: record it. FULL without one (the planner could not read both versions
#   from obs.h): REMOVE the marker, so a later fast deploy refuses until a full deploy records a known
#   pair. FAST: leave it (it names the frontend, which a fast deploy keeps).
genlock_stats_abi_marker_ps() {
  local mode="${1:-}" new="${2:-}"
  if [ "$mode" = "fast" ]; then
    cat <<PS
# (5b) issue 1302: FAST -- ${GENLOCK_STATS_ABI_MARKER} is left as it is: it names the frontend, which this deploy
#      keeps, and step (0f) proved both its versions equal the new obs.dll's.
PS
  elif genlock_stats_abi_is_pair "$new"; then
    cat <<PS
# (5b) issue 1302: record this full bundle's genlock stats ABI (both versions, read from the build's own
#      obs.h) -- a later fast deploy refuses an obs.dll whose stats or output stats version differs.
Write-MarkerAtomic (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}') @('${new%%:*}', 'output_stats=${new#*:}')
PS
  else
    cat <<PS
# (5b) issue 1302: the planner could NOT read both of this build's genlock stats versions (obs.h at the
#      deployed commit) -- the box's ${GENLOCK_STATS_ABI_MARKER} is REMOVED, so a fast deploy refuses until
#      the next full-bundle deploy records a known pair.
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}')
Write-Warning "stats ABI unknown at plan time: ${GENLOCK_STATS_ABI_MARKER} removed -- a fast deploy will refuse until the next full-bundle deploy"
PS
  fi
}

# genlock_stats_abi_stage BUNDLE_DIR NEW_PAIR -> write (a complete pair, the two-line marker) or remove
#   (anything else) the bundle's GENLOCK_STATS_ABI.txt before it ships to a Linux box, whose installer
#   reads it back (genlock_stats_abi_pair_from_marker) for genlock_write_markers. rc 1 on an I/O failure.
genlock_stats_abi_stage() {
  local dir="${1:-}" new="${2:-}" text
  [ -d "$dir" ] || { echo "genlock_stats_abi_stage: bundle dir '$dir' not found" >&2; return 1; }
  if text="$(genlock_stats_abi_marker_text "$new")"; then
    printf '%s\n' "$text" > "$dir/$GENLOCK_STATS_ABI_MARKER" \
      || { echo "genlock_stats_abi_stage: cannot write $dir/$GENLOCK_STATS_ABI_MARKER" >&2; return 1; }
  else
    rm -f "$dir/$GENLOCK_STATS_ABI_MARKER" \
      || { echo "genlock_stats_abi_stage: cannot remove $dir/$GENLOCK_STATS_ABI_MARKER" >&2; return 1; }
  fi
}

# _genlock_stats_abi_read_pair SHA REPO -> the `<stats>:<output_stats>` pair of the build at SHA, with
#   "" for a part it cannot read (`4:`, `:1`, `:`). Always rc 0.
_genlock_stats_abi_read_pair() {
  local s o
  s="$(genlock_stats_abi_at_sha "$1" "$2")" || s=""
  o="$(genlock_stats_abi_at_sha "$1" "$2" OBS_GENLOCK_OUTPUT_STATS_VERSION)" || o=""
  printf '%s:%s\n' "$s" "$o"
}

# genlock_stats_abi_resolve SHA REPO MODE SWAPS_DLL [FETCH] -> the `<stats>:<output_stats>` pair of the
#   build at SHA (OBS_GENLOCK_STATS_VERSION and OBS_GENLOCK_OUTPUT_STATS_VERSION in its own obs.h), or
#   "" when either cannot be read. FETCH=1 (execute mode; plan mode has no network) fetches that one
#   commit from origin before giving up (a full 40-hex SHA only: the anchor run's headSha is one, and
#   the tests' short SHAs never reach the network). SWAPS_DLL=1 when the box list holds a box a FAST
#   deploy swaps obs.dll on (fleet_boxes_swap_obs_dll). For such a FAST deploy "" is REFUSED (rc 3),
#   because the gate cannot compare an unknown version. Otherwise "" is allowed and the unreadable
#   define is named on stderr (each full-bundle box's marker is removed). Any readable version is
#   carried as it is: the FAST gate compares both on the box.
genlock_stats_abi_resolve() {
  local sha="${1:-}" repo="${2:-}" mode="${3:-}" swaps="${4:-0}" fetch="${5:-0}" pair out rc=0 fast=0 unread=""
  [ "$mode" = "fast" ] && [ "$swaps" = "1" ] && fast=1
  pair="$(_genlock_stats_abi_read_pair "$sha" "$repo")"
  # an unreadable stats version means the commit is not in the checkout (every genlock commit defines
  # it); a fetch needs the full object id (git cannot fetch an abbreviation), the anchor run's headSha
  if [ -z "${pair%%:*}" ] && [ "$fetch" = "1" ] && [[ "$sha" =~ ^[0123456789abcdefABCDEF]{40}$ ]]; then
    echo "# genlock stats ABI: $sha is not readable in $repo -- fetching origin once" >&2
    # the one commit only (never every branch): bounded, no credential prompt; git's reason is kept
    out="$(GIT_TERMINAL_PROMPT=0 timeout 120 git -C "$repo" fetch -q origin "$sha" 2>&1)" || rc=$?
    [ "$rc" = 0 ] || printf '# git fetch origin %s failed (rc %s): %s\n' "$sha" "$rc" "${out:-no output}" >&2
    pair="$(_genlock_stats_abi_read_pair "$sha" "$repo")"
  fi
  if genlock_stats_abi_is_pair "$pair"; then
    echo "# genlock stats ABI of $sha: v${pair%%:*}, output stats v${pair#*:} (${GENLOCK_STATS_ABI_OBS_H} at that commit)" >&2
    printf '%s\n' "$pair"
    return 0
  fi
  [ -n "${pair%%:*}" ] || unread="OBS_GENLOCK_STATS_VERSION"
  [ -n "${pair#*:}" ] || unread="${unread:+$unread and }OBS_GENLOCK_OUTPUT_STATS_VERSION"
  if [ "$fast" = 1 ]; then
    # the stats version read fine: the commit is in the checkout and a fetch cannot help
    out="fetch the commit (git fetch origin) or deploy --full"
    [ -z "${pair%%:*}" ] || out="the commit is in the checkout, so deploy --full"
    echo "ERROR: cannot read $unread from ${GENLOCK_STATS_ABI_OBS_H} at $sha in $repo -- a --fast deploy cannot be gated on an unknown stats ABI; $out" >&2
    return 3
  fi
  echo "WARNING: cannot read $unread from ${GENLOCK_STATS_ABI_OBS_H} at $sha in $repo -- this deploy REMOVES each full-bundle box's ${GENLOCK_STATS_ABI_MARKER}, so a later --fast refuses until a full deploy records a known pair" >&2
  return 0
}
