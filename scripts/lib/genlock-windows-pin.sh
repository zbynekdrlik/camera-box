#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/genlock-windows-pin.sh -- issue 1505: the Windows OBS boxes take the pinned obs release's
# Windows zip (scripts/lib/obs-release.sh obs_release_windows_bundle), the ONLY source of their bytes:
# camera-box's own windows-genlock build of vendor/ is retired, so a pin without the Windows zip refuses
# a Windows box by name, never another build. deploy-genlock-fleet.sh (< 1000-line budget) keeps only
# the calls; the routing lives here.
#
# What this means for a Windows box:
#   - its canonical SHA is the pin's commit (the markers the deploy program writes name it), never an
#     anchor run's; --run-id is needed only for imag (linux-genlock.yml);
#   - the staged tree is the verified, unzipped release (the stage layout build_windows_deploy_program
#     -- the ONE Windows program -- reads);
#   - the frontend's stats ABI pair comes from the pinned manifest's release block, never obs.h;
#   - --fast is refused: the release ships the full bundle only.
# Needs obs-release.sh + genlock-fleet-boxes.sh sourced by the caller.

# fleet_win_split ROOT CSV -> two lines: the boxes of CSV that take an anchor build (imag), then the Windows
# boxes, which take the pinned release's zip. rc 3 + a named line when CSV holds a Windows box and the pin
# carries no Windows zip, or names one whose sha256 fields are malformed (never a vendor fallback).
fleet_win_split() {
  local root="${1:?tree required}" b anchor="" win="" IFS=','
  for b in ${2:-}; do
    if [ "$(fleet_boxes_swap_obs_dll "$b")" = 1 ]; then win="${win:+$win,}$b"; else anchor="${anchor:+$anchor,}$b"; fi
  done
  if [ -n "$win" ] && ! obs_pin_has_windows "$root"; then
    echo "ERROR: ${win}: the pin carries no Windows zip (scripts/obs-release.pin has no OBS_RELEASE_WINDOWS_SHA256) -- camera-box's own windows-genlock build is retired (issue 1505), so a Windows box deploys only from the pinned obs release" >&2
    return 3
  fi
  if [ -n "$win" ]; then
    obs_pin_windows_sha256 "$root" >/dev/null && obs_pin_windows_manifest_sha256 "$root" >/dev/null || return 3
  fi
  printf '%s\n%s\n' "$anchor" "$win"
}

# fleet_win_pin_mode_ok MODE -> rc 0 for full; rc 2 + a named line for fast (the release ships no fast bundle).
fleet_win_pin_mode_ok() {
  [ "${1:-}" = full ] && return 0
  echo "ERROR: --fast cannot deploy a Windows box from the pinned obs release (it ships the full bundle only, issue 1505) -- deploy the Windows box separately with --full" >&2
  return 2
}

# fleet_win_pin_stats_abi ROOT DIR -> the stats pair `<stats>:<output_stats>` of DIR's BUNDLE_MANIFEST.json
# when its release block names the pinned tag + commit; nothing + a named WARNING otherwise (the program
# then removes the box's marker -- never a pair from another release).
fleet_win_pin_stats_abi() {
  local root="${1:?tree required}" dir="${2:?dir required}" tag commit
  tag="$(obs_pin_tag "$root" 2>/dev/null)" || return 0
  commit="$(obs_pin_commit "$root" 2>/dev/null)" || return 0
  if [ "$(obs_release_manifest_field "$dir" obs_tag || true)" != "$tag" ] \
     || [ "$(obs_release_manifest_field "$dir" obs_commit || true)" != "$commit" ]; then
    echo "WARNING: ${dir}/BUNDLE_MANIFEST.json is not the pinned obs release ${tag} (${commit}) -- no stats ABI pair, the Windows box's GENLOCK_STATS_ABI.txt will be removed" >&2
    return 0
  fi
  obs_release_stats_abi "$dir" \
    || echo "WARNING: the pinned Windows manifest records no stats ABI pair -- the Windows box's GENLOCK_STATS_ABI.txt will be removed" >&2
  return 0
}

# fleet_win_note ROOT WIN_BOXES -> one NOTE line (nothing when WIN_BOXES is empty): the Windows boxes take
# the pinned release, whatever --run-id / --sha say (those are the anchor boxes' only).
fleet_win_note() {
  [ -n "${2:-}" ] || return 0
  printf '# NOTE: Windows boxes %s take the pinned obs release %s (%s), not the anchor run (issue 1505)\n' \
    "$2" "$(obs_pin_tag "${1:?tree required}")" "$(obs_pin_commit "$1")"
}

# fleet_win_log_lines ROOT RUN SHA BOXES WIN_SHA WIN_BOXES MODE -> the fleet-deploy log lines: BOXES minus
# WIN_BOXES under RUN/SHA, then WIN_BOXES under obs-<tag>/WIN_SHA (each only when non-empty).
fleet_win_log_lines() {
  local root="${1:?tree required}" run="$2" sha="$3" boxes="$4" wsha="$5" wboxes="$6" mode="$7" b rest="" IFS=','
  for b in $boxes; do
    case ",${wboxes}," in *",${b},"*) ;; *) rest="${rest:+$rest,}$b" ;; esac
  done
  [ -z "$rest" ] || fleet_log_line "$run" "$sha" "$rest" "$mode"
  [ -z "$wboxes" ] || fleet_log_line "obs-$(obs_pin_tag "$root")" "$wsha" "$wboxes" "$mode"
}

# fleet_win_bundle_label ROOT -> what STEP 0 of a Windows plan uploads: the pinned zip's name (unpacked +
# verified).
fleet_win_bundle_label() {
  printf '%s (the pinned obs release, unzipped + verified)\n' "$(obs_release_windows_zip_name "$(obs_pin_tag "${1:?tree required}")")"
}

# fleet_win_bundle_source ROOT WIN_STAGE -> where STEP 0's bytes come from: the cached, sha-proved zip (its
# root is the stage layout), expanded on the box into WIN_STAGE -- the unzipped tree the plan verified is a
# mktemp dir the planner removes on exit; a --plan run never fetches, so a zip not cached yet is named as
# such.
fleet_win_bundle_source() {
  local root="${1:?tree required}" tag zip sha
  tag="$(obs_pin_tag "$root" 2>/dev/null)" || tag="UNREADABLE"
  zip="${OBS_RELEASE_CACHE:-${root%/}/obs-release}/${tag}/$(obs_release_windows_zip_name "$tag")"
  sha="$(obs_pin_windows_sha256 "$root" 2>/dev/null)" || sha="UNREADABLE (scripts/obs-release.pin)"
  if [ -f "$zip" ]; then
    printf 'the cached pinned zip %s, sha256 %s -- upload it, check its sha256 there and Expand-Archive it into %s\n' \
      "$zip" "$sha" "${2:-the box stage}"
  else
    printf 'the pinned zip is not cached yet at %s -- run this deploy once without --plan to fetch + prove it\n' "$zip"
  fi
}
