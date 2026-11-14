#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure functions + printers, no top-level statements) -- the
# scripts/lib/*.sh convention: the caller (deploy-genlock-fleet.sh) owns `set -euo pipefail`, so
# every pipeline here survives a no-match under that strict mode.
#
# scripts/lib/genlock-stats-abi.sh -- issue 1302: the genlock stats ABI each full-bundle deploy records.
#
# WHY: the OBS frontend allocates `struct obs_genlock_stats` AND `struct obs_genlock_output_stats` on
# its stack and passes no size to `obs_source_get_genlock_stats` / `obs_output_get_genlock_stats`, so
# an obs.dll / libobs that fills a bigger struct than its frontend knows writes past the frontend's
# copy and OBS crashes. Design: issue 1302 comment 6028838843; ROZHODNUTÉ 6030159870 item 1 (the
# output-stats struct too). Every deploy now ships the whole bundle, frontend included (issue 1505
# retired the Windows obs.dll-only FAST deploy and issue 1522 removed its step-(0f) gate), so the
# pair is RECORDED next to the build, never compared at deploy time.
#
# The ABI of a build is the PAIR of its two struct versions, carried as ONE value
# `<stats>:<output_stats>` (e.g. `4:1`). The box marker GENLOCK_STATS_ABI.txt holds it as two lines:
#     4
#     output_stats=1
#
# The mechanism:
#   * every full-bundle deploy the planner drives records its frontend's pair in GENLOCK_STATS_ABI.txt
#     next to GENLOCK_BUILD_SHA.txt (the Windows program clears it at step (3c) before the copy and
#     writes it at step (5b); the Linux genlock_write_markers 5th arg). A deploy that cannot name BOTH
#     versions, and setup-imag.sh's own provisioning, remove it;
#   * the pinned-release boxes take the pair from the release's own BUNDLE_MANIFEST.json
#     (obs_release_stats_abi); imag's anchor build takes it from that build's own obs.h
#     (`git show <sha>:vendor/obs-studio/libobs/obs.h`), never from the checkout.
# A struct change must bump its version (the pytest pins each struct body to its version).

# The obs.h path inside the repo, read at the deployed build's commit.
GENLOCK_STATS_ABI_OBS_H="vendor/obs-studio/libobs/obs.h"
# The box marker file name (next to GENLOCK_BUILD_SHA.txt).
GENLOCK_STATS_ABI_MARKER="GENLOCK_STATS_ABI.txt"

# genlock_stats_abi_is_version TEXT -> 0 when TEXT is a stats version (a positive integer without a
# leading zero, at most 9 digits), else 1. Every bash pattern here spells its digits out: a bracket
# RANGE such as [1-9] follows the locale's collation, and under en_US.UTF-8 it also matches
# non-ASCII digits (review round 1).
genlock_stats_abi_is_version() {
  [[ "${1:-}" =~ ^[123456789][0123456789]{0,8}$ ]]
}

# genlock_stats_abi_is_pair PAIR -> 0 when both parts of PAIR are versions (and nothing follows), else 1.
genlock_stats_abi_is_pair() {
  [[ "${1:-}" =~ ^[123456789][0123456789]{0,8}:[123456789][0123456789]{0,8}$ ]]
}

# _genlock_stats_abi_marker_fields (stdin: a GENLOCK_STATS_ABI.txt's text) -> two lines: what the
#   marker says about the stats struct, then about the output-stats struct, each "v<N>" or the reason
#   it names no version. Lines are split on LF; every ASCII whitespace character inside a line is
#   ignored (the Windows writer ends each line in CRLF) and a line left empty is skipped. The first
#   line must be a version; the second `output_stats=<version>` (case-sensitive); no third line, so a
#   later format never reads as a known pair.
_genlock_stats_abi_marker_fields() {
  local line="" n=0 l1="" l2=""
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
#   (neither whitespace nor a digit).
genlock_stats_abi_pair_from_marker() {
  local fields s o
  # an unreadable stdin (a directory, an I/O error) reads as no marker, never an abort of the caller
  fields="$(tr '\000' '?' | _genlock_stats_abi_marker_fields)" || fields=""
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
  [[ "$define" =~ ^[ABCDEFGHIJKLMNOPQRSTUVWXYZ_][ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_]*$ ]] || return 2
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

# genlock_stats_abi_clear_ps -> step (3c) of the Windows deploy program, right before the copy: remove
#   GENLOCK_STATS_ABI.txt, so a copy that fails half way never leaves a new frontend under the old
#   marker (step 5b writes the new pair once every copy passed).
genlock_stats_abi_clear_ps() {
  cat <<PS
# (3c) issue 1302: remove ${GENLOCK_STATS_ABI_MARKER} before the copy; step (5b) records the new build's
#      pair once every copy passed, so a half-done copy never names the new frontend's pair.
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

# genlock_stats_abi_marker_ps NEW_PAIR -> step (5b) of the Windows deploy program, after the other
#   markers (it uses the program's Write-MarkerAtomic, which writes each array element as one line).
#   A complete pair: record it. Anything else (the pinned manifest's release block names another
#   release, or a part is not a version): REMOVE the marker, so it never names a pair it cannot prove.
genlock_stats_abi_marker_ps() {
  local new="${1:-}"
  if genlock_stats_abi_is_pair "$new"; then
    cat <<PS
# (5b) issue 1302: record this full bundle's genlock stats ABI (both versions, from the pinned
#      release's manifest) next to GENLOCK_BUILD_SHA.txt.
Write-MarkerAtomic (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}') @('${new%%:*}', 'output_stats=${new#*:}')
PS
  else
    cat <<PS
# (5b) issue 1302: the planner could NOT name both of this build's genlock stats versions -- the box's
#      ${GENLOCK_STATS_ABI_MARKER} is REMOVED, so it never names a pair this deploy cannot prove.
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path \$obsDir '${GENLOCK_STATS_ABI_MARKER}')
Write-Warning "stats ABI unknown at plan time: ${GENLOCK_STATS_ABI_MARKER} removed until a deploy records a known pair"
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

# genlock_stats_abi_resolve SHA REPO [FETCH] -> the `<stats>:<output_stats>` pair of the build at SHA
#   (OBS_GENLOCK_STATS_VERSION and OBS_GENLOCK_OUTPUT_STATS_VERSION in its own obs.h), or "" when
#   either cannot be read: the unreadable define is named on stderr and the deploy REMOVES the box's
#   marker. FETCH=1 (execute mode; plan mode has no network) fetches that one commit from origin
#   before giving up (a full 40-hex SHA only: the anchor run's headSha is one, and the tests' short
#   SHAs never reach the network). Only imag's anchor build is read here (every other OBS box takes
#   the pinned release's manifest pair, issue 1505). Always rc 0.
genlock_stats_abi_resolve() {
  local sha="${1:-}" repo="${2:-}" fetch="${3:-0}" pair out rc=0 unread=""
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
  echo "WARNING: cannot read $unread from ${GENLOCK_STATS_ABI_OBS_H} at $sha in $repo -- this deploy REMOVES the box's ${GENLOCK_STATS_ABI_MARKER} until a deploy records a known pair" >&2
  return 0
}
