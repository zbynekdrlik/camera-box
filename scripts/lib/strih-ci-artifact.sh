#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the CI-artifact fetch helpers, no top-level statements)
# -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would
# leak into it. setup-strih.sh sets its own.
#
# scripts/lib/strih-ci-artifact.sh -- issue 1412 (gap G6): the ONE way setup-strih.sh fetches a
# camera-box CI artifact onto the box. Step 16c fetched the bkshading service with an inline copy of
# this; the intercom-hub binary (step 13) needs the same, so both now call it.
#
# strih_ci_artifact_fetch ART PRESTAGED -> print a local directory holding the extracted artifact ART
# on stdout, rc 0; rc 1 + a warn line on stderr when there is none. In order:
#   1. PRESTAGED, when it is a directory (the supervisor extracted the artifact by hand) -- printed as is;
#   2. with GH_TOKEN: the newest unexpired artifact named ART in the repo
#      (STRIH_LX_GH_REPO, default zbynekdrlik/camera-box) through the GitHub artifacts API, downloaded
#      and unzipped into a NEW temp dir under STRIH_CI_ARTIFACT_TMP (default the system temp). The
#      caller removes that dir with strih_ci_artifact_cleanup once it installed from it.
# A token without actions:read (or no successful run yet) yields no URL -> rc 1, never a guess.
strih_ci_artifact_fetch() {
  local art="${1:?artifact name required}" pre="${2:-}" tmp dl
  if [ -n "$pre" ] && [ -d "$pre" ]; then
    printf '%s' "${pre%/}"
    return 0
  fi
  if [ -z "${GH_TOKEN:-}" ]; then
    echo "  GH_TOKEN unset and no pre-staged dir -- cannot fetch the ${art} CI artifact" >&2
    return 1
  fi
  tmp="$(mktemp -d "${STRIH_CI_ARTIFACT_TMP:-${TMPDIR:-/tmp}}/strih-ci-artifact.XXXXXX")" \
    || { echo "  mktemp for the ${art} artifact failed" >&2; return 1; }
  dl="$(curl -fsSL -H "Authorization: token ${GH_TOKEN}" -H 'Accept: application/vnd.github+json' \
    "${STRIH_CI_ARTIFACT_API:-https://api.github.com}/repos/${STRIH_LX_GH_REPO:-zbynekdrlik/camera-box}/actions/artifacts?name=${art}&per_page=20" 2>/dev/null \
    | python3 -c 'import json,sys
try:
    arts=[a for a in json.load(sys.stdin).get("artifacts", []) if not a.get("expired")]
    print(arts[0]["archive_download_url"] if arts else "")
except Exception:
    print("")' || true)"
  if [ -z "$dl" ] || ! curl -fsSL -H "Authorization: token ${GH_TOKEN}" -L "$dl" -o "${tmp}/art.zip" 2>/dev/null; then
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

# strih_ci_artifact_cleanup DIR -> remove the temp dir strih_ci_artifact_fetch created for DIR (its
# parent, named strih-ci-artifact.*); a pre-staged dir is never removed.
strih_ci_artifact_cleanup() {
  local dir="${1:-}" parent
  [ -n "$dir" ] || return 0
  parent="$(dirname "$dir")"
  case "$(basename "$parent")" in
    strih-ci-artifact.*) rm -rf "$parent" ;;
  esac
  return 0
}

# strih_intercom_hub_binary_install [BIN] -> put the intercom-hub binary at BIN (/usr/local/bin/intercom-hub)
# from the intercom-hub-linux-amd64 CI artifact (or the dir STRIH_LX_HUB_SRC) when NO binary is there.
# An installed binary is kept: the hub on a running strih is upgraded by its own deploy, never silently
# by a re-provisioning run (every strih-lx genlock deploy re-runs setup-strih). rc 0 with a warn line when
# no artifact can be fetched (the box then needs the binary placed before go-live); rc 1 on an install
# failure or an artifact without the binary.
strih_intercom_hub_binary_install() {
  local bin="${1:-/usr/local/bin/intercom-hub}" src found
  if [ -x "$bin" ]; then
    echo "  intercom-hub binary present (${bin}) -- kept (the hub is upgraded by its own deploy)"
    return 0
  fi
  if ! src="$(strih_ci_artifact_fetch intercom-hub-linux-amd64 "${STRIH_LX_HUB_SRC:-}")"; then
    echo "  WARNING: ${bin} is ABSENT and the intercom-hub-linux-amd64 artifact could not be fetched -- place the CI binary before go-live (program audio needs the hub)" >&2
    return 0
  fi
  found="$(find "$src" -type f -name intercom-hub 2>/dev/null | head -1 || true)"
  if [ -z "$found" ]; then
    strih_ci_artifact_cleanup "$src"
    echo "issue 1412: no intercom-hub binary in the intercom-hub-linux-amd64 artifact (${src})" >&2
    return 1
  fi
  if ! install -D -m 0755 "$found" "$bin"; then
    strih_ci_artifact_cleanup "$src"
    echo "issue 1412: could not install ${bin}" >&2
    return 1
  fi
  strih_ci_artifact_cleanup "$src"
  echo "  installed the intercom-hub CI binary -> ${bin}"
}
