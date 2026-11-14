#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# same source-only convention as scripts/lib/bundle-state-selfheal.sh / cbox-burn-log-persist.sh:
# sourcing this file runs it in the CALLER's shell, so `set -euo pipefail` here would leak into the
# caller (recording-e2e.sh, whose [0/8] gate MUST survive a failed per-box fetch). The caller owns
# its own strict mode; every function here is best-effort and returns 0.
#
# scripts/lib/manifest-autosource.sh -- #1082: make the [0/8] version-integrity gate's byte facet a
# genuine POINTER to CI truth. #770 wired the byte-vs-manifest COMPARE + the Windows on-box gather
# but left the marker (GENLOCK_BUILD_SHA.txt) as the only cross-box signal; this lib supplies the two
# deferred live-side pieces:
#   1. AUTO-SOURCE each box's CI-authoritative BUNDLE_MANIFEST for its OWN marker SHA X
#      (imag: linux-genlock.yml / obs-genlock-linux-x86_64 -- the setup-imag.sh recipe; the Windows
#      FAST/FULL pair went with camera-box's own Windows build, issue 1505), so the gate compares
#      DEPLOYED bytes against the build the marker claims, closing the WHOLESALE-wrong-bundle case.
#   2. GATHER imag's deployed .so sha256s over ssh (imag is NOT a --win-state bundle-state box; its
#      bytes have no other path into the gate) for the new --imag-bytes facet.
#
# ALL best-effort: a fetch/gather failure yields "" so the caller OMITS the arg. What an omitted arg
# means is the GATE's call: the imag byte facet (ENFORCED since #1100) and the Linux boxes'
# linux_so_bytes row (issue 1474) read it as UNKNOWN and refuse. `gh run download` + the imag ssh gather are isolated behind
# env-overridable command seams (MANIFEST_AUTOSOURCE_CMD, #836 executable-fixture) so the whole path
# is offline-testable with no gh/ssh/network (tests/harness_manifest_autosource_1082.rs).

# imag_so_gather_cmd -> a REMOTE bash snippet (embed as the WHOLE ssh payload: `ssh ...
# "$(imag_so_gather_cmd)"`) that prints `<manifest-path> <sha256>` per readable genlock-bearing .so
# on imag, using sha256sum. The 3 files whose BYTES carry the genlock patches (per setup-imag.sh):
# libobs.so.30 (the render-tick/ts-align core), obs-plugins/distroav.so, and libobs-opengl.so.30
# (the X11/EGL client-size cache Fix B). Deployed at /usr/<path>; the printed key is the
# manifest-relative <path> (lib/x86_64-linux-gnu/...) so it resolves directly against the linux
# BUNDLE_MANIFEST via the gate's manifest_sha_for_path. A missing/unreadable file simply prints no
# line for it (the caller's parser then omits it -> that path UNKNOWN, never a false clean). #833: a
# missing sha256sum must fail LOUD by name, never read as a measured zero -> prints
# `TOOL_MISSING:sha256sum` (the parser yields "" -> facet dormant).
imag_so_gather_cmd() {
  _obs_box_so_gather_cmd /usr lib/x86_64-linux-gnu/libobs.so.30 lib/x86_64-linux-gnu/obs-plugins/distroav.so \
    lib/x86_64-linux-gnu/libobs-opengl.so.30
}

# _obs_box_so_gather_cmd PREFIX PATH... -> the ONE remote gather snippet behind imag_so_gather_cmd and
# linux_obs_box_so_gather_cmd (issue 1474): `<path> <sha256>` per readable PREFIX/PATH, nothing for an
# unreadable one, `TOOL_MISSING:sha256sum` (#833) when the box has no sha256sum. imag's text is
# byte-identical to the issue-1082 heredoc this replaced (pinned by tests/python/test_linux_bytes_gate_1474.py).
# The replacement words are quoted (bash 5.2 patsub_replacement would expand an `&` in them).
_obs_box_so_gather_cmd() {
  local prefix="$1"; shift
  local t IFS=' '   # "$*" joins with IFS's first character: always one space, whatever the caller set
  t="$(cat <<'REMOTE'
if command -v sha256sum >/dev/null 2>&1; then
  for _p in __PATHS__; do
    _f="__PREFIX__/$_p"
    if [ -r "$_f" ]; then
      _h="$(sha256sum "$_f" 2>/dev/null | awk '{print $1}')"
      [ -n "$_h" ] && printf '%s %s\n' "$_p" "$_h"
    fi
  done
else
  echo TOOL_MISSING:sha256sum
fi
REMOTE
)"
  t="${t//__PATHS__/"$*"}"
  t="${t//__PREFIX__/"$prefix"}"
  printf '%s\n' "$t"
}

# linux_obs_box_byte_paths -> issue 1474: the files of the strih-variant linux bundle (the one
# setup-strih.sh installs into /usr on strih-snv, stream-snv, strih-pp, stream-pp) whose bytes carry
# our code, manifest-relative, one per line: libobs (the genlock core), the frontend API, DistroAV,
# obs-vban, the A/V-sync dock, and the frontend itself (bin/obs). The ONE list: the gather hashes
# exactly these and the gate's linux_so_bytes row requires every one of them.
linux_obs_box_byte_paths() {
  printf '%s\n' lib/x86_64-linux-gnu/libobs.so.30 lib/x86_64-linux-gnu/libobs-frontend-api.so.30 \
    lib/x86_64-linux-gnu/obs-plugins/distroav.so lib/x86_64-linux-gnu/obs-plugins/obs-vban.so \
    lib/x86_64-linux-gnu/obs-plugins/obs-audio-video-sync-dock.so bin/obs
}

# linux_obs_box_bundle_artifact -> the linux-genlock.yml artifact a setup-strih box is deployed from
# (deploy-genlock-fleet.sh's fleet_linux_bundle_artifact_for, pinned equal by the issue-1474 test), so
# the gate grades the box against the manifest of the bundle the deploy installed.
linux_obs_box_bundle_artifact() {
  echo obs-genlock-linux-x86_64-strih
}

# linux_obs_box_so_gather_cmd [PREFIX] -> the remote gather of linux_obs_box_byte_paths under PREFIX
# (default /usr, the setup-strih install prefix; a test points it at a fake root).
linux_obs_box_so_gather_cmd() {
  local -a paths=()
  local p
  while IFS= read -r p; do paths+=("$p"); done < <(linux_obs_box_byte_paths)
  _obs_box_so_gather_cmd "${1:-/usr}" "${paths[@]}"
}

# imag_so_bytes_csv PROBE_OUTPUT -> pure LOCAL parser (no ssh): turn the gather's `<path> <sha>` lines
# into the `path=sha,path=sha,...` CSV the gate's --imag-bytes wants. A TOOL_MISSING line (#833) or
# empty input -> "" (facet dormant). Only `lib/`- and `bin/`-prefixed lines are accepted (defensive
# against a stray login banner; imag's gather prints only lib/ lines). Never a partial/false CSV -- a
# line missing its sha is skipped.
imag_so_bytes_csv() {
  obs_box_so_bytes_csv "$@"
}

# linux_obs_box_so_bytes_csv PROBE_OUTPUT -> the same parser for a Linux OBS box's gather (issue 1474).
linux_obs_box_so_bytes_csv() {
  obs_box_so_bytes_csv "$@"
}

# obs_box_so_bytes_csv PROBE_OUTPUT -> the shared parser of both gathers.
obs_box_so_bytes_csv() {
  local out="$1" path sha _rest csv=""
  grep -qi 'TOOL_MISSING' <<<"$out" && return 0
  while IFS=' ' read -r path sha _rest; do
    [ -z "$path" ] || [ -z "$sha" ] && continue
    case "$path" in
      lib/*|bin/*) : ;;
      *) continue ;;
    esac
    csv="${csv:+$csv,}${path}=${sha}"
  done <<< "$out"
  printf '%s' "$csv"
}

# manifest_autosource_fetch REPO WORKFLOW ARTIFACT SHA DEST -> best-effort: fetch the
# CI-authoritative BUNDLE_MANIFEST.json for the CI run of WORKFLOW at commit SHA, cache it at DEST,
# and echo DEST. Echoes NOTHING (empty) and returns 0 on ANY failure (no marker SHA / unresolved run
# / download error / no manifest inside). The CALLER decides what "" means: the Windows fast pin
# treats it as a fetch outage (the pin omitted for that run), while the imag facet (issue 1100) and
# the Linux boxes' linux_so_bytes row (issue 1474) refuse the run on it (fail-closed).
# Bounds: every gh call is under MANIFEST_AUTOSOURCE_TIMEOUT_S (300 s); the artifact DOWNLOAD alone is
# under MANIFEST_AUTOSOURCE_DOWNLOAD_TIMEOUT_S (default the same), so a caller fetching a big bundle
# (the ~600 MB strih artifact) widens only the download, never the run lookup.
#
# #836 executable-fixture seam: if MANIFEST_AUTOSOURCE_CMD is set to an executable, it is invoked as
# `$MANIFEST_AUTOSOURCE_CMD REPO WORKFLOW ARTIFACT SHA DEST` INSTEAD of gh (its contract: place a
# manifest at DEST and print the path, or exit non-zero) -- so the whole path is offline-testable.
manifest_autosource_fetch() {
  local repo="$1" workflow="$2" artifact="$3" sha="$4" dest="$5"
  [ -n "$sha" ] || return 0   # no marker SHA -> nothing to key on -> dormant
  local seam="${MANIFEST_AUTOSOURCE_CMD:-}"
  if [ -n "$seam" ] && [ -x "$seam" ]; then
    local out=""
    out="$("$seam" "$repo" "$workflow" "$artifact" "$sha" "$dest" 2>/dev/null)" || return 0
    [ -n "$out" ] && [ -s "$out" ] && printf '%s' "$out"
    return 0
  fi
  command -v gh >/dev/null 2>&1 || return 0
  # #1346 review: every gh call is time-bounded (a stalled ~270 MB full-bundle download must not hang
  # the [0/8] preflight); a timeout is just another fetch failure -> "" -> dormant.
  local tmo="${MANIFEST_AUTOSOURCE_TIMEOUT_S:-300}"
  local run="" run_id="" run_stamp=""
  # The run is resolved with the SERVER-SIDE --commit/--status filter (#1346 review: the same filter
  # manifest_autosource_run_state uses, so a build older than any recency window is still found and
  # the two never disagree). jq re-checks the sha via env.SHA (never string-interpolated into the
  # filter, so a box's reported marker can't break out of the jq program) and prints
  # `<run id> <updatedAt>` (the stamp keys the cache below), or nothing when no run matches.
  run="$(SHA="$sha" timeout "$tmo" gh run list --repo "$repo" --workflow "$workflow" \
    --commit "$sha" --status success -L 5 \
    --json databaseId,conclusion,headSha,updatedAt \
    --jq '[.[] | select(.headSha==env.SHA and .conclusion=="success")][0] | select(. != null) | "\(.databaseId) \(.updatedAt)"' 2>/dev/null)" || return 0
  run_id="${run%% *}"
  case "$run_id" in
    ''|*[!0-9]*) return 0 ;;   # unresolved or not a plain run id -> dormant
  esac
  [ "$run" != "$run_id" ] && run_stamp="${run#* }"
  run_stamp="${run_stamp//[!0-9]/}"
  # #1346 review: the manifest is cached per (workflow, artifact, run id, updatedAt), so a second E2E
  # on the same build reads the cache instead of downloading again. updatedAt is in the key because a
  # GitHub re-run KEEPS the run id and a full re-run republishes non-reproducible bytes; without a
  # stamp nothing is cached (the fetch still works).
  local cache=""
  [ -n "$run_stamp" ] && cache="$(manifest_autosource_cache_path "$workflow" "$artifact" "${run_id}-${run_stamp}")"
  if [ -n "$cache" ] && [ -s "$cache" ]; then
    mkdir -p "$(dirname "$dest")" 2>/dev/null || true
    cp -f "$cache" "$dest" 2>/dev/null && printf '%s' "$dest"
    return 0
  fi
  local tmp=""
  tmp="$(mktemp -d 2>/dev/null)" || return 0
  # Issue 1474: a failed download is logged with gh's own error text (stderr only; stdout stays the
  # path-or-nothing contract), so a refused [0/8] says why the manifest is missing.
  local dl_err="" dl_rc=0
  dl_err="$(timeout "${MANIFEST_AUTOSOURCE_DOWNLOAD_TIMEOUT_S:-$tmo}" gh run download "$run_id" --repo "$repo" -n "$artifact" --dir "$tmp" 2>&1 >/dev/null)" || dl_rc=$?
  if [ "$dl_rc" -ne 0 ]; then
    printf 'manifest-autosource: gh run download %s -n %s failed (rc %s): %s\n' \
      "$run_id" "$artifact" "$dl_rc" "$(printf '%s' "$dl_err" | tail -n 3 | tr '\n' ' ')" >&2
    rm -rf "$tmp" 2>/dev/null
    return 0
  fi
  # `find -print -quit` (first match, clean exit 0) — NOT `find | head -1`, whose SIGPIPE-on-a-large
  # match set can trip `set -o pipefail` (the #239 class drift-guard's manifest_sha_for_path avoids).
  local found=""
  found="$(find "$tmp" -name BUNDLE_MANIFEST.json -type f -print -quit 2>/dev/null)"
  if [ -n "$found" ] && [ -s "$found" ]; then
    mkdir -p "$(dirname "$dest")" 2>/dev/null || true
    cp -f "$found" "$dest" 2>/dev/null && printf '%s' "$dest"
    manifest_autosource_cache_store "$found" "$cache"
  fi
  rm -rf "$tmp" 2>/dev/null
  return 0
}

# manifest_autosource_cache_path WORKFLOW ARTIFACT RUN_KEY -> the cache file for that CI run's
# manifest: "${MANIFEST_AUTOSOURCE_CACHE_DIR:-$HOME/.camera-box/manifest-cache}/<workflow>--<artifact>--<run_key>.json",
# RUN_KEY = `<run id>-<updatedAt digits>`. Empty (no cache) when neither the dir override nor HOME is
# set. Keyed on the run, never the commit sha (a second run at one sha builds different,
# non-reproducible bytes), and on updatedAt too (a re-run keeps the run id but republishes).
manifest_autosource_cache_path() {
  local dir="${MANIFEST_AUTOSOURCE_CACHE_DIR:-}"
  if [ -z "$dir" ] && [ -n "${HOME:-}" ]; then
    dir="$HOME/.camera-box/manifest-cache"
  fi
  [ -n "$dir" ] || return 0
  printf '%s/%s--%s--%s.json' "$dir" "$1" "$2" "$3"
}

# manifest_autosource_cache_store SRC CACHE -> best-effort: copy SRC to CACHE atomically (a temp name
# then rename, so a concurrent reader never sees a half-written manifest), and drop cache entries --
# and temp files a killed store left behind -- older than 30 days (manifests are a few hundred KB;
# old builds are never fetched again). A manifest that is not valid JSON is never cached, so a bad
# download affects one run, not the next 30 days.
manifest_autosource_cache_store() {
  local src="$1" cache="$2"
  [ -n "$cache" ] || return 0
  python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$src" >/dev/null 2>&1 || return 0
  local dir="${cache%/*}"
  mkdir -p "$dir" 2>/dev/null || return 0
  cp -f "$src" "$cache.tmp.$$" 2>/dev/null && mv -f "$cache.tmp.$$" "$cache" 2>/dev/null
  rm -f "$cache.tmp.$$" 2>/dev/null
  find "$dir" -maxdepth 1 -type f -name '*--*--*.json*' -mtime +30 -delete 2>/dev/null || true
  return 0
}

# genlock_build_sha_state_read FILE -> the `genlock_build_sha` value (the marker SHA a box's
# bundle-state-server reports) from the flat JSON state FILE, "" if absent/unreadable. Mirrors the
# gate's own genlock_build_sha_from_state, kept here so the E2E (scripts/lib/e2e-linux-bytes.sh) can key
# a box's manifest fetch on its OWN reported SHA without sourcing the whole gate.
genlock_build_sha_state_read() {
  local file="$1"
  [ -f "$file" ] || return 0
  grep -oE "\"genlock_build_sha\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" "$file" 2>/dev/null \
    | head -1 | sed -E 's/.*:[[:space:]]*"([^"]*)"/\1/'
}

# manifest_autosource_state_has_key FILE KEY -> exit 0 iff the flat JSON state FILE carries KEY with a
# NON-EMPTY string value. The Windows manifest auto-source gates on this for obs_dll_sha256: supplying
# a manifest when the box does NOT yet report the deployed obs.dll sha would flip obs_dll_sha256 to
# UNKNOWN (drift-guard compare of a manifest sha vs an empty observed) -> a spurious gate-blocking
# refuse. So the auto-source stays fully dormant until the #770 on-box byte gather is live (the
# ENFORCE precondition). An empty value ("obs_dll_sha256":"") must NOT count as reported.
manifest_autosource_state_has_key() {
  local file="$1" key="$2"
  [ -f "$file" ] || return 1
  grep -qE "\"${key}\"[[:space:]]*:[[:space:]]*\"[^\"]" "$file" 2>/dev/null
}
