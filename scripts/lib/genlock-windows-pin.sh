#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/genlock-windows-pin.sh -- issue 1505: the Windows OBS boxes take the pinned obs release's
# Windows zip (scripts/lib/obs-release.sh obs_release_windows_bundle) instead of an anchor run's
# windows-genlock.yml artifact, once scripts/obs-release.pin carries OBS_RELEASE_WINDOWS_SHA256.
# deploy-genlock-fleet.sh (< 1000-line budget) keeps only the calls; the routing lives here.
#
# What changes for a Windows box when the pin carries the zip:
#   - its canonical SHA is the pin's commit (the markers the deploy program writes name it), never the
#     anchor run's; --run-id is needed only for a box that still takes an anchor build (imag);
#   - the staged tree is the verified, unzipped release (same layout as the windows-genlock stage, so
#     build_windows_deploy_program -- the ONE Windows program -- reads it unchanged);
#   - the frontend's stats ABI pair comes from the pinned manifest's release block, never obs.h;
#   - --fast is refused: the release ships the full bundle only.
# Needs obs-release.sh + genlock-fleet-boxes.sh sourced by the caller.

# fleet_win_split ROOT CSV -> two lines: the boxes of CSV that still take an anchor build, then the Windows
# boxes that take the pinned release's zip (empty when the pin carries no Windows zip). rc 3 + a named line
# when the pin names a Windows zip whose sha256 fields are malformed (never a silent vendor fallback).
fleet_win_split() {
  local root="${1:?tree required}" b anchor="" win="" IFS=','
  if ! obs_pin_has_windows "$root"; then printf '%s\n\n' "${2:-}"; return 0; fi
  obs_pin_windows_sha256 "$root" >/dev/null && obs_pin_windows_manifest_sha256 "$root" >/dev/null || return 3
  for b in ${2:-}; do
    if [ "$(fleet_boxes_swap_obs_dll "$b")" = 1 ]; then win="${win:+$win,}$b"; else anchor="${anchor:+$anchor,}$b"; fi
  done
  printf '%s\n%s\n' "$anchor" "$win"
}

# fleet_win_pin_mode_ok MODE -> rc 0 for full; rc 2 + a named line for fast (the release ships no fast bundle).
fleet_win_pin_mode_ok() {
  [ "${1:-}" = full ] && return 0
  echo "ERROR: --fast cannot deploy a Windows box from the pinned obs release (it ships the full bundle only, issue 1505) -- deploy --full" >&2
  return 2
}

# fleet_win_pin_stats_abi ROOT DIR -> the stats pair `<stats>:<output_stats>` of DIR's BUNDLE_MANIFEST.json
# when its release block names the pinned tag + commit; nothing otherwise (the program then removes the
# box's marker, so a later fast deploy refuses -- never a pair from another release).
fleet_win_pin_stats_abi() {
  local root="${1:?tree required}" dir="${2:?dir required}" tag commit
  tag="$(obs_pin_tag "$root" 2>/dev/null)" || return 0
  commit="$(obs_pin_commit "$root" 2>/dev/null)" || return 0
  [ "$(obs_release_manifest_field "$dir" obs_tag || true)" = "$tag" ] || return 0
  [ "$(obs_release_manifest_field "$dir" obs_commit || true)" = "$commit" ] || return 0
  obs_release_stats_abi "$dir" || true
}

# fleet_win_bundle_label ROOT MODE -> what STEP 0 of a Windows plan uploads: the pinned zip's name (unpacked
# + verified) when the pin carries one, else the windows-genlock artifact of MODE.
fleet_win_bundle_label() {
  local tag
  if obs_pin_has_windows "${1:?tree required}" && tag="$(obs_pin_tag "$1" 2>/dev/null)"; then
    printf '%s (the pinned obs release, unzipped + verified)\n' "$(obs_release_windows_zip_name "$tag")"
  else
    fleet_windows_artifact "${2:-full}"
  fi
}
