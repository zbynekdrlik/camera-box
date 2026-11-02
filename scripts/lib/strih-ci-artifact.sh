#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the CI-artifact fetch helpers, no top-level statements)
# -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would
# leak into it. setup-strih.sh sets its own.
#
# scripts/lib/strih-ci-artifact.sh -- issue 1412 (gap G6): the ONE way setup-strih.sh fetches a
# camera-box CI artifact onto the box (the bkshading service, step 16c). The intercom hub is a pinned
# release, scripts/lib/strih-intercom-release.sh.
#
# strih_ci_artifact_fetch ART PRESTAGED [BRANCH] [SHA] -> print a local directory holding the extracted artifact ART
# on stdout, rc 0; rc 1 + a warn line on stderr when there is none. In order:
#   1. PRESTAGED, when it is a directory (the supervisor extracted the artifact by hand) -- printed as is;
#   2. with GH_TOKEN: the newest unexpired artifact named ART in the repo
#      (STRIH_LX_GH_REPO, default zbynekdrlik/camera-box) through the GitHub artifacts API, downloaded
#      and unzipped into a NEW temp dir under STRIH_CI_ARTIFACT_TMP (default the system temp). The
#      caller removes that dir with strih_ci_artifact_cleanup once it installed from it. BRANCH, when
#      given, keeps only artifacts of a run on that branch; SHA (a full commit sha), only artifacts of
#      a run at that commit.
# The token is handed to curl on STDIN (`-H @-`), never in its argv, so `ps` never shows it.
# A token without actions:read (or no successful run yet) yields no URL -> rc 1, never a guess.
strih_ci_artifact_fetch() {
  local art="${1:?artifact name required}" pre="${2:-}" branch="${3:-}" sha="${4:-}" tmp dl
  if [ -n "$pre" ] && [ -d "$pre" ]; then
    printf '%s' "${pre%/}"
    return 0
  fi
  if [ -z "${GH_TOKEN:-}" ]; then
    echo "  GH_TOKEN unset and no pre-staged dir -- cannot fetch the ${art} CI artifact" >&2
    return 1
  fi
  tmp="$(mktemp -d "$(_strih_ci_artifact_tmp_root)/strih-ci-artifact.XXXXXX")" \
    || { echo "  mktemp for the ${art} artifact failed" >&2; return 1; }
  dl="$(_strih_ci_artifact_url "$art" "$branch" "$sha")"
  if [ -z "$dl" ] || ! _strih_ci_artifact_auth | curl -fsSL -H @- -L "$dl" -o "${tmp}/art.zip" 2>/dev/null; then
    rm -rf "$tmp"
    echo "  could not fetch the ${art} CI artifact (GH_TOKEN lacks actions:read, or no successful run yet)" >&2
    return 1
  fi
  if ! python3 -c 'import sys,zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "${tmp}/art.zip" "${tmp}/x" 2>/dev/null; then
    rm -rf "$tmp"
    echo "  could not extract the ${art} artifact zip" >&2
    return 1
  fi
  printf '%s' "${tmp}/x"
}

# _strih_ci_artifact_url ART BRANCH [SHA] -> the archive_download_url of the newest unexpired ART
# artifact (from a BRANCH run when BRANCH is set, from a run at commit SHA when SHA is set), or nothing.
# Without a filter the first page holds the newest artifact. With one, issue 1412 (live 11.10.2026):
# dev CI fills whole pages of the artifacts API, so the newest `main` hub sat on page 2+ and a one-page
# read found none. Walk the pages (per_page=100, at most STRIH_CI_ARTIFACT_MAX_PAGES, default 10) until
# a match or an empty page. The token goes to curl on stdin only. Always rc 0 (an empty answer is the
# caller's failure).
_strih_ci_artifact_url() {
  local art="$1" branch="$2" sha="${3:-}" page=1 max="${STRIH_CI_ARTIFACT_MAX_PAGES:-10}" body pick
  [ -n "$branch" ] || [ -n "$sha" ] || max=1
  while [ "$page" -le "$max" ]; do
    body="$(_strih_ci_artifact_auth | curl -fsSL -H @- -H 'Accept: application/vnd.github+json' \
      "${STRIH_CI_ARTIFACT_API:-https://api.github.com}/repos/${STRIH_LX_GH_REPO:-zbynekdrlik/camera-box}/actions/artifacts?name=${art}&per_page=100&page=${page}" 2>/dev/null || true)"
    pick="$(printf '%s' "$body" | python3 -c 'import json,sys
branch, sha = sys.argv[1], sys.argv[2]
try:
    all_arts=json.load(sys.stdin).get("artifacts", [])
except Exception:
    all_arts=[]
if not all_arts:
    print("END")
else:
    arts=[a for a in all_arts if not a.get("expired")
          and (not branch or (a.get("workflow_run") or {}).get("head_branch") == branch)
          and (not sha or (a.get("workflow_run") or {}).get("head_sha") == sha)]
    print(arts[0]["archive_download_url"] if arts else "")' "$branch" "$sha" 2>/dev/null || true)"
    case "$pick" in
      END) return 0 ;;
      '') ;;
      *) printf '%s' "$pick"; return 0 ;;
    esac
    page=$((page + 1))
  done
  return 0
}

# _strih_ci_artifact_tmp_root -> the directory the fetch creates its temp dirs in.
_strih_ci_artifact_tmp_root() { printf '%s' "${STRIH_CI_ARTIFACT_TMP:-${TMPDIR:-/tmp}}"; }

# _strih_ci_artifact_auth -> the Authorization header line, for curl's `-H @-` (stdin).
_strih_ci_artifact_auth() { printf 'Authorization: token %s\n' "${GH_TOKEN:-}"; }

# strih_ci_artifact_cleanup DIR [PRESTAGED] -> remove the temp dir strih_ci_artifact_fetch created for
# DIR. Only the exact shape the fetch returns is removed: DIR = <tmp root>/strih-ci-artifact.*/x, and
# DIR is not the pre-staged dir. Anything else (a pre-staged dir, even one inside an old fetch's
# leftover, a path elsewhere) is never touched.
strih_ci_artifact_cleanup() {
  local dir="${1:-}" pre="${2:-}" parent
  [ -n "$dir" ] || return 0
  [ -z "$pre" ] || [ "${dir%/}" != "${pre%/}" ] || return 0
  [ "$(basename "$dir")" = x ] || return 0
  parent="$(dirname "$dir")"
  [ "$(dirname "$parent")" = "$(_strih_ci_artifact_tmp_root)" ] || return 0
  case "$(basename "$parent")" in
    strih-ci-artifact.?*) rm -rf "$parent" ;;
  esac
  return 0
}
