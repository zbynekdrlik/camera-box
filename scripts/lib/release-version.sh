#!/usr/bin/env bash
# release-version.sh -- the ONE camera-box version-order rule + the post-merge release-tag decision
# (issue 1476). Source-only lib: no top-level `set -euo pipefail` (a sourced lib must never change
# the caller's shell options). # airuleset:script-ok source-only pure lib, every function returns
# its verdict explicitly and is called under the caller's `set -euo pipefail`.
#
# THE VERSION SCHEME (issue 1476, from 1.8.0 on). Every version in the workspace Cargo.toml is a
# clean semver X.Y.Z: main ships X.Y.Z, the merge commit is tagged vX.Y.Z and gets a GitHub
# Release, and the next bump on dev is the next patch (X.Y.Z+1). The old scheme appended `-dev.N`
# to a frozen 1.7.0 on every dev bump, so a box can still report a historical `1.7.0-dev.N`; that
# string is a semver PRE-RELEASE of 1.7.0 and orders below 1.7.0, so below every 1.8.x.
#
# Functions (pure, no network, no git):
#   release_version_is_plain V      -> rc 0 iff V is a clean X.Y.Z (ASCII digits, no `v`, no suffix)
#   release_version_cmp A B         -> prints -1 / 0 / 1 (A older / same / newer than B), rc 0;
#                                      prints nothing, rc 2, when either side is not a version
#   release_version_order_word A B  -> OLDER / SAME / NEWER / UNKNOWN (never fails, rc 0)
#   release_version_next_patch V    -> X.Y.(Z+1) for a plain V, rc 0; nothing + rc 2 otherwise
#   release_tag_decision CARGO_VERSION BINARY_VERSION TAG_SHA HEAD_SHA
#                                   -> prints CREATE / EXISTS (rc 0) or FAIL: <reason> (rc 1)

# The sets are spelled out, never a bracket range: a range follows the locale under bash (an
# en_US.UTF-8 `[0-9]` also matches non-ASCII digits), see .claude/rules/ci-testing-gotchas.md.
_RELEASE_VERSION_DIGIT='0123456789'
_RELEASE_VERSION_LOWER='abcdefghijklmnopqrstuvwxyz'
_RELEASE_VERSION_UPPER='ABCDEFGHIJKLMNOPQRSTUVWXYZ'

# _release_version_parse V -> sets the globals _RV_MAJ _RV_MIN _RV_PAT _RV_PRE (pre-release text,
# empty for a release); rc 0 on a semver X.Y.Z[-PRE], rc 1 otherwise. A numeric part with a
# leading zero is refused (semver), except a plain 0.
_release_version_parse() {
  local LC_ALL=C v="${1-}" d="$_RELEASE_VERSION_DIGIT" num pre_re
  num="(0|[123456789][${d}]*)"
  pre_re="[${d}${_RELEASE_VERSION_LOWER}${_RELEASE_VERSION_UPPER}.-]+"
  _RV_MAJ="" _RV_MIN="" _RV_PAT="" _RV_PRE=""
  if [[ $v =~ ^${num}\.${num}\.${num}(-(${pre_re}))?$ ]]; then
    _RV_MAJ="${BASH_REMATCH[1]}"
    _RV_MIN="${BASH_REMATCH[2]}"
    _RV_PAT="${BASH_REMATCH[3]}"
    _RV_PRE="${BASH_REMATCH[5]}"
    return 0
  fi
  return 1
}

release_version_is_plain() {
  _release_version_parse "${1-}" || return 1
  [ -z "$_RV_PRE" ]
}

# _release_version_cmp_num A B -> -1/0/1 for two non-negative decimal strings of any length (no
# arithmetic overflow: a longer digit string is larger, equal lengths compare as text).
_release_version_cmp_num() {
  local LC_ALL=C a="$1" b="$2"
  if [ "${#a}" -lt "${#b}" ]; then printf -- '-1\n'; return 0; fi
  if [ "${#a}" -gt "${#b}" ]; then printf '1\n'; return 0; fi
  if [[ $a < $b ]]; then printf -- '-1\n'; return 0; fi
  if [[ $a > $b ]]; then printf '1\n'; return 0; fi
  printf '0\n'
}

# _release_version_cmp_pre A B -> semver pre-release precedence of two NON-EMPTY pre-release
# strings: dot-separated identifiers left to right, numeric ones numerically, a numeric one below
# an alphanumeric one, alphanumerics as ASCII text, and a shorter list below a longer equal prefix.
_release_version_cmp_pre() {
  local -a pa pb
  local LC_ALL=C i n x y d="$_RELEASE_VERSION_DIGIT" c
  IFS=. read -r -a pa <<<"$1"
  IFS=. read -r -a pb <<<"$2"
  n="${#pa[@]}"
  [ "${#pb[@]}" -lt "$n" ] && n="${#pb[@]}"
  for ((i = 0; i < n; i++)); do
    x="${pa[i]}" y="${pb[i]}"
    if [[ $x =~ ^[${d}]+$ ]] && [[ $y =~ ^[${d}]+$ ]]; then
      c="$(_release_version_cmp_num "$x" "$y")"
      [ "$c" = 0 ] || { printf '%s\n' "$c"; return 0; }
    elif [[ $x =~ ^[${d}]+$ ]]; then
      printf -- '-1\n'; return 0
    elif [[ $y =~ ^[${d}]+$ ]]; then
      printf '1\n'; return 0
    else
      if [[ $x < $y ]]; then printf -- '-1\n'; return 0; fi
      if [[ $x > $y ]]; then printf '1\n'; return 0; fi
    fi
  done
  if [ "${#pa[@]}" -lt "${#pb[@]}" ]; then printf -- '-1\n'; return 0; fi
  if [ "${#pa[@]}" -gt "${#pb[@]}" ]; then printf '1\n'; return 0; fi
  printf '0\n'
}

release_version_cmp() {
  local a="${1-}" b="${2-}" amaj amin apat apre c part
  _release_version_parse "$a" || return 2
  amaj="$_RV_MAJ" amin="$_RV_MIN" apat="$_RV_PAT" apre="$_RV_PRE"
  _release_version_parse "$b" || return 2
  for part in "$amaj:$_RV_MAJ" "$amin:$_RV_MIN" "$apat:$_RV_PAT"; do
    c="$(_release_version_cmp_num "${part%%:*}" "${part#*:}")"
    [ "$c" = 0 ] || { printf '%s\n' "$c"; return 0; }
  done
  # Same X.Y.Z: a release ranks above every pre-release of it (1.7.0-dev.830 < 1.7.0).
  if [ -z "$apre" ] && [ -z "$_RV_PRE" ]; then printf '0\n'; return 0; fi
  if [ -z "$apre" ]; then printf '1\n'; return 0; fi
  if [ -z "$_RV_PRE" ]; then printf -- '-1\n'; return 0; fi
  _release_version_cmp_pre "$apre" "$_RV_PRE"
}

release_version_order_word() {
  local c
  c="$(release_version_cmp "${1-}" "${2-}")" || c=""
  case "$c" in
    -1) printf 'OLDER\n' ;;
    0) printf 'SAME\n' ;;
    1) printf 'NEWER\n' ;;
    *) printf 'UNKNOWN\n' ;;
  esac
  return 0
}

release_version_next_patch() {
  release_version_is_plain "${1-}" || return 2
  local next
  # The patch number of a real version is small; refuse one past 9 digits rather than overflow.
  [ "${#_RV_PAT}" -le 9 ] || return 2
  next=$((10#$_RV_PAT + 1))
  printf '%s.%s.%s\n' "$_RV_MAJ" "$_RV_MIN" "$next"
}

# release_tag_decision CARGO_VERSION BINARY_VERSION TAG_SHA HEAD_SHA -> the post-merge tag step's
# decision on main. CARGO_VERSION = Cargo.toml's [workspace.package] version, BINARY_VERSION = the
# built camera-box's `--version` (last word), TAG_SHA = the commit the existing tag vCARGO_VERSION
# points at (empty when the tag does not exist), HEAD_SHA = the merge commit being released.
#   CREATE  the tag does not exist yet -> push vCARGO_VERSION on HEAD_SHA
#   EXISTS  the tag already names HEAD_SHA (a re-run) -> nothing to push, publish again
#   FAIL    main carries a pre-release / unreadable version, the binary disagrees with Cargo.toml,
#           or the tag already names ANOTHER commit (a missed bump on dev: never move a release)
release_tag_decision() {
  local ver="${1-}" bin="${2-}" tag_sha="${3-}" head="${4-}"
  if [ -z "$ver" ]; then
    printf 'FAIL: the Cargo.toml version is unreadable\n'; return 1
  fi
  if ! release_version_is_plain "$ver"; then
    printf 'FAIL: main carries %s, not a clean X.Y.Z release version (issue 1476: no -dev.N suffix on main)\n' "$ver"
    return 1
  fi
  if [ "$bin" != "$ver" ]; then
    printf 'FAIL: the built camera-box reports %s, Cargo.toml says %s\n' "${bin:-<unreadable>}" "$ver"
    return 1
  fi
  if [ -z "$head" ]; then
    printf 'FAIL: the merge commit sha is unknown\n'; return 1
  fi
  if [ -z "$tag_sha" ]; then
    printf 'CREATE\n'; return 0
  fi
  if [ "$tag_sha" = "$head" ]; then
    printf 'EXISTS\n'; return 0
  fi
  printf 'FAIL: tag v%s already names commit %s, not this merge %s -- dev was not bumped to the next patch before the release\n' "$ver" "$tag_sha" "$head"
  return 1
}
