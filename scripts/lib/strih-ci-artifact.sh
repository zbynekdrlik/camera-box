#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the CI-artifact fetch helpers, no top-level statements)
# -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would
# leak into it. setup-strih.sh sets its own.
#
# scripts/lib/strih-ci-artifact.sh -- issue 1412 (gap G6): the ONE way setup-strih.sh fetches a
# camera-box CI artifact onto the box. Step 16c fetched the bkshading service with an inline copy of
# this; the intercom-hub binary (step 13) needs the same, so both now call it.
#
# strih_ci_artifact_fetch ART PRESTAGED [BRANCH] [SHA] -> print a local directory holding the extracted artifact ART
# on stdout, rc 0; rc 1 + a warn line on stderr when there is none. In order:
#   1. PRESTAGED, when it is a directory (the supervisor extracted the artifact by hand) -- printed as is;
#   2. with GH_TOKEN: the newest unexpired artifact named ART in the repo
#      (STRIH_LX_GH_REPO, default zbynekdrlik/camera-box) through the GitHub artifacts API, downloaded
#      and unzipped into a NEW temp dir under STRIH_CI_ARTIFACT_TMP (default the system temp). The
#      caller removes that dir with strih_ci_artifact_cleanup once it installed from it. BRANCH, when
#      given, keeps only artifacts of a run on that branch; SHA (a full commit sha), only artifacts of
#      a run at that commit (issue 1467: the hub binary of the provisioning checkout's own commit).
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

# strih_checkout_source_sha ROOT -> the full commit sha of the provisioning tree at ROOT (issue 1467),
# rc 0; nothing + rc 1 when none is known. ROOT is a git checkout (its work tree's top level, never a
# directory that merely sits inside some other checkout) -> its HEAD; else the CAMERA_BOX_SOURCE_SHA
# file the strih-snv deploy writes into its staged tree (an archive of HEAD, no .git). Only a 40-hex
# sha is printed.
strih_checkout_source_sha() {
  local root="${1:-}" top sha=""
  [ -n "$root" ] && [ -d "$root" ] || return 1
  top="$(git -C "$root" rev-parse --show-toplevel 2>/dev/null || true)"
  if [ -n "$top" ] && [ "$(cd "$top" && pwd -P)" = "$(cd "$root" && pwd -P)" ]; then
    sha="$(git -C "$root" rev-parse HEAD 2>/dev/null || true)"
  elif [ -f "$root/CAMERA_BOX_SOURCE_SHA" ]; then
    sha="$(head -n 1 "$root/CAMERA_BOX_SOURCE_SHA" 2>/dev/null | tr -d '[:space:]' || true)"
  fi
  [[ $sha =~ ^[0123456789abcdef]{40}$ ]] || return 1
  printf '%s\n' "$sha"
}

# strih_checkout_version ROOT -> the camera-box workspace version of the tree at ROOT (its Cargo.toml's
# one column-0 `version = "X"` line, the [workspace.package] version every member inherits), rc 0;
# nothing + rc 1 when unreadable. The strih-snv deploy stages Cargo.toml with scripts/ (issue 1467).
strih_checkout_version() {
  local root="${1:-}" v
  [ -f "$root/Cargo.toml" ] || return 1
  v="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$root/Cargo.toml" 2>/dev/null | head -n 1 || true)"
  [ -n "$v" ] || return 1
  printf '%s\n' "$v"
}

# _strih_hub_record_version BIN REF SHA [ONLY_WHEN_CHANGED] -> write BIN.version (version / source_sha /
# artifact_ref, one key=value per line) from `BIN --version` (clap: `intercom-hub <version>`), so verify
# item 16 can grade a hub that does not answer and an operator can read where it came from. With
# ONLY_WHEN_CHANGED=1 (a kept binary) the file is written only when absent or its version line differs
# from the binary's (a hub replaced by hand), then with REF/SHA `unknown`. Never fails the caller.
_strih_hub_record_version() {
  local bin="$1" ref="$2" sha="$3" only="${4:-0}" vf="${1}.version" ver
  ver="$("$bin" --version 2>/dev/null | sed -n 's/^intercom-hub \([^ ]*\)$/\1/p' | head -n 1 || true)"
  [ -n "$ver" ] || ver=unknown
  if [ "$only" = 1 ] && [ -f "$vf" ] && grep -qxF "version=${ver}" "$vf" 2>/dev/null; then
    return 0
  fi
  if ! printf 'version=%s\nsource_sha=%s\nartifact_ref=%s\n' "$ver" "${sha:-unknown}" "$ref" > "${vf}.tmp" 2>/dev/null \
     || ! mv -f "${vf}.tmp" "$vf" 2>/dev/null; then
    rm -f "${vf}.tmp" 2>/dev/null || true
    echo "  WARNING: could not record the intercom-hub version at ${vf}" >&2
    return 0
  fi
  echo "  intercom-hub version ${ver} recorded at ${vf} (artifact ${ref})"
}

# strih_intercom_hub_binary_install [BIN] [ROOT] -> put the intercom-hub binary at BIN
# (/usr/local/bin/intercom-hub) when NO binary is there, from the dir STRIH_LX_HUB_SRC, else the
# intercom-hub-linux-amd64 CI artifact of the provisioning checkout's own commit (ROOT = the tree
# setup-strih runs from; strih_checkout_source_sha), else -- that commit has no artifact, or ROOT knows
# no commit -- of a `main` run, with a WARNING (issue 1467: main's hub can be older than the checkout;
# 23.10.2026 it lacked the 24-bit VBAN decode and FOH went silent). The log names the ref used, and the
# installed version is recorded next to the binary (BIN.version).
# An installed binary is kept: the hub on a running strih is upgraded by its own deploy, never silently
# by a re-provisioning run (every strih-snv genlock deploy re-runs setup-strih); verify item 16 NOTEs a
# kept hub older than the checkout. rc 0 with a warn line when no artifact can be fetched (the box then
# needs the binary placed before go-live); rc 1 on an install failure or an artifact without the binary.
strih_intercom_hub_binary_install() {
  local bin="${1:-/usr/local/bin/intercom-hub}" root="${2:-}" src found sha="" ref
  if [ -x "$bin" ]; then
    echo "  intercom-hub binary present (${bin}) -- kept (the hub is upgraded by its own deploy)"
    _strih_hub_record_version "$bin" unknown unknown 1
    return 0
  fi
  if [ -n "${STRIH_LX_HUB_SRC:-}" ] && [ -d "${STRIH_LX_HUB_SRC}" ]; then
    ref="pre-staged"
    src="$(strih_ci_artifact_fetch intercom-hub-linux-amd64 "${STRIH_LX_HUB_SRC}")" || return 1
    echo "  intercom-hub from the pre-staged dir ${src}"
  else
    if [ -n "$root" ]; then sha="$(strih_checkout_source_sha "$root" || true)"; fi
    if [ -n "$sha" ] && src="$(strih_ci_artifact_fetch intercom-hub-linux-amd64 "" "" "$sha")"; then
      ref="$sha"
      echo "  intercom-hub from the CI artifact of the checkout's commit ${sha}"
    else
      if [ -n "$sha" ]; then
        echo "  WARNING: commit ${sha} (the provisioning checkout) has no intercom-hub-linux-amd64 artifact -- falling back to the newest main run, which can be OLDER than this checkout (verify item 16 names it)" >&2
      else
        echo "  WARNING: no source commit known for the provisioning tree ${root:-<none given>} (not a git checkout, no CAMERA_BOX_SOURCE_SHA) -- taking the intercom-hub of the newest main run" >&2
      fi
      if ! src="$(strih_ci_artifact_fetch intercom-hub-linux-amd64 "" main)"; then
        echo "  WARNING: ${bin} is ABSENT and the intercom-hub-linux-amd64 artifact could not be fetched -- place the CI binary before go-live (program audio needs the hub)" >&2
        return 0
      fi
      ref=main
      echo "  intercom-hub from the CI artifact of the newest main run"
    fi
  fi
  found="$(find "$src" -type f -name intercom-hub 2>/dev/null | head -1 || true)"
  if [ -z "$found" ]; then
    strih_ci_artifact_cleanup "$src" "${STRIH_LX_HUB_SRC:-}"
    echo "issue 1412: no intercom-hub binary in the intercom-hub-linux-amd64 artifact (${src})" >&2
    return 1
  fi
  if ! install -D -m 0755 "$found" "$bin"; then
    strih_ci_artifact_cleanup "$src" "${STRIH_LX_HUB_SRC:-}"
    echo "issue 1412: could not install ${bin}" >&2
    return 1
  fi
  strih_ci_artifact_cleanup "$src" "${STRIH_LX_HUB_SRC:-}"
  echo "  installed the intercom-hub CI binary -> ${bin}"
  _strih_hub_record_version "$bin" "$ref" "${sha:-unknown}"
}
