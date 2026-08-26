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
#      (Windows FAST: windows-genlock-fast.yml / obs-genlock-fast-dll; imag: linux-genlock.yml /
#      obs-genlock-linux-x86_64 -- the setup-imag.sh recipe), so the gate compares DEPLOYED bytes
#      against the build the marker claims, closing the WHOLESALE-wrong-bundle case.
#   2. GATHER imag's deployed .so sha256s over ssh (imag is NOT a --win-state bundle-state box; its
#      bytes have no other path into the gate) for the new --imag-bytes facet.
#
# ALL best-effort: a fetch/gather failure yields "" so the caller OMITS the arg and the gate facet
# stays DORMANT (opt-in, #756-shape) -- never a spurious refuse. The ENFORCE flip (#758-shape) is
# deferred to a follow-up (needs the live gather deployed + verified, a property no worktree worker
# can check -- the #1067 port4455 class). `gh run download` + the imag ssh gather are isolated behind
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
  cat <<'REMOTE'
if command -v sha256sum >/dev/null 2>&1; then
  for _p in lib/x86_64-linux-gnu/libobs.so.30 lib/x86_64-linux-gnu/obs-plugins/distroav.so lib/x86_64-linux-gnu/libobs-opengl.so.30; do
    _f="/usr/$_p"
    if [ -r "$_f" ]; then
      _h="$(sha256sum "$_f" 2>/dev/null | awk '{print $1}')"
      [ -n "$_h" ] && printf '%s %s\n' "$_p" "$_h"
    fi
  done
else
  echo TOOL_MISSING:sha256sum
fi
REMOTE
}

# imag_so_bytes_csv PROBE_OUTPUT -> pure LOCAL parser (no ssh): turn the gather's `<path> <sha>` lines
# into the `path=sha,path=sha,...` CSV the gate's --imag-bytes wants. A TOOL_MISSING line (#833) or
# empty input -> "" (facet dormant). Only `lib/`-prefixed lines are accepted (defensive against a
# stray login banner). Never a partial/false CSV -- a line missing its sha is skipped.
imag_so_bytes_csv() {
  local out="$1" path sha _rest csv=""
  grep -qi 'TOOL_MISSING' <<<"$out" && return 0
  while IFS=' ' read -r path sha _rest; do
    [ -z "$path" ] || [ -z "$sha" ] && continue
    case "$path" in
      lib/*) : ;;
      *) continue ;;
    esac
    csv="${csv:+$csv,}${path}=${sha}"
  done <<< "$out"
  printf '%s' "$csv"
}

# manifest_autosource_fetch REPO WORKFLOW ARTIFACT SHA DEST -> best-effort: fetch the
# CI-authoritative BUNDLE_MANIFEST.json for the CI run of WORKFLOW at commit SHA, cache it at DEST,
# and echo DEST. Echoes NOTHING (empty) and returns 0 on ANY failure (no marker SHA / unresolved run
# / download error / no manifest inside) -- the caller treats "" as "facet dormant", so a fetch
# failure never refuses a run.
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
  local run_id=""
  # jq reads the marker SHA via env.SHA (never string-interpolated into the filter) so a box's
  # reported marker can't break out of the jq program even if it carried a metacharacter.
  run_id="$(SHA="$sha" gh run list --repo "$repo" --workflow "$workflow" -L 100 \
    --json databaseId,conclusion,headSha \
    --jq '[.[] | select(.headSha==env.SHA and .conclusion=="success")][0].databaseId' 2>/dev/null)" || return 0
  [ -n "$run_id" ] && [ "$run_id" != "null" ] || return 0
  local tmp=""
  tmp="$(mktemp -d 2>/dev/null)" || return 0
  if ! gh run download "$run_id" --repo "$repo" -n "$artifact" --dir "$tmp" >/dev/null 2>&1; then
    rm -rf "$tmp" 2>/dev/null
    return 0
  fi
  # `find -print -quit` (first match, clean exit 0) — NOT `find | head -1`, whose SIGPIPE-on-a-large
  # match set can trip `set -o pipefail` (the #239 class drift-guard's manifest_sha_for_path avoids).
  local found=""
  found="$(find "$tmp" -name BUNDLE_MANIFEST.json -type f -print -quit 2>/dev/null)"
  if [ -n "$found" ] && [ -s "$found" ]; then
    mkdir -p "$(dirname "$dest")" 2>/dev/null
    cp -f "$found" "$dest" 2>/dev/null && printf '%s' "$dest"
  fi
  rm -rf "$tmp" 2>/dev/null
  return 0
}

# genlock_build_sha_state_read FILE -> the `genlock_build_sha` value (the marker SHA a box's
# bundle-state-server reports) from the flat JSON state FILE, "" if absent/unreadable. Mirrors the
# gate's own genlock_build_sha_from_state, kept here so recording-e2e.sh can key the Windows manifest
# auto-source on the box's OWN reported SHA without sourcing the whole gate.
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
