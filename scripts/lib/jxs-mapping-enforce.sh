#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statement) -- the scripts/lib/*.sh
# convention: sourcing runs in the CALLER's shell (rig-mode.sh, `set -euo pipefail`), so strict mode here
# would leak into it. The function is safe in the caller's `jxs_mapping_enforce ... || rc=$?` shape.
#
# scripts/lib/jxs-mapping-enforce.sh -- issue 1387 (owner 1.11.2026, design 6558038959 Approach 1). The
# operator picks which camera a strih JPEG XS input shows: its "Kamera" list writes the jxs_port + jxs_camera
# pair of another declared camera (vendor/distroav/src/jxs-source.cpp), like picking an NDI source, and the
# launch seeder keeps that pick. rig-mode TEST and EVENT put the declared 1:1 mapping back at the moment the
# strih NDI mapping is enforced (the issue-399 parity): scripts/obs_jxs_inputs.py --enforce-mapping against
# the manifest the strih's FACTS render (strih_jxs_seed_manifest_json, the file setup-strih step 6 installs
# as /opt/camera-box/jxs-seed.json), read back, never while the strih OBS streams or records. dev1-side: the
# facts come from this checkout, the strih is reached over obs-websocket only.
#
# Test: tests/python/test_jxs_camera_pick_1387.py (JXS_MAPPING_ENFORCE_SEEDER is the seeder seam).

# jxs_mapping_enforce MODE IP PASSWORD -> put the declared JPEG XS camera mapping back on the strih at IP.
# rc 0 when every declared input is on its declared camera, or when no strih fact file declares IP / the
# strih declares no JPEG XS camera (a NOTE). Else the failing step's rc (the seeder: 3 = a mapping not
# proven, 1 = OBS unreachable, 2 = a bad manifest; 124 = it timed out) after ONE named WARNING on stderr.
# MODE (test | event) only names the mode in that WARNING.
jxs_mapping_enforce() {
  local mode="${1-}" ip="${2-}" pw="${3-}" lib out box man tmp rc=0
  lib="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || return 1
  # The strih whose fact file declares IP, and its manifest. A subshell keeps the fact loader's globals
  # out of the caller. rc 5 = no strih declares IP.
  out="$(
    set +e +f
    # shellcheck source=scripts/lib/strih-box-facts.sh
    . "$lib/strih-box-facts.sh" || exit 4
    # shellcheck source=scripts/lib/strih-jxs-provision.sh
    . "$lib/strih-jxs-provision.sh" || exit 4
    found=""
    while IFS= read -r b; do
      parsed="$(strih_box_parse_file "$(strih_box_dir)/${b}.env" 2>/dev/null)" || continue
      case $'\n'"$parsed"$'\n' in *$'\nSTRIH_ROLE=strih\n'*) ;; *) continue ;; esac
      case $'\n'"$parsed"$'\n' in *$'\nSTRIH_IP='"$ip"$'\n'*) found="$b" ;; esac
    done < <(strih_box_known)
    [ -n "$found" ] || exit 5
    strih_box_load "$found" || exit 4
    printf '%s\n' "$found"
    strih_jxs_seed_manifest_json || exit 4
  )" || rc=$?
  if [ "$rc" = 5 ]; then
    echo "    [strih jxs-map] NOTE: no strih fact file declares ${ip} -- no declared JPEG XS mapping to put back (issue 1387)"
    return 0
  fi
  if [ "$rc" != 0 ]; then
    echo "WARNING issue 1387: the strih JPEG XS camera mapping enforce FAILED (rc=${rc}: the box facts for ${ip} do not load) -- continuing, ${mode^^} mode will still exit non-zero." >&2
    return "$rc"
  fi
  box="${out%%$'\n'*}"
  man="${out#*$'\n'}"
  [ "$man" != "$out" ] || man=""
  if [ -z "$man" ]; then
    echo "    [strih jxs-map] NOTE: the strih ${box} declares no JPEG XS camera (STRIH_JXS_CAMERAS none) -- nothing to put back (issue 1387)"
    return 0
  fi
  tmp="$(mktemp)" || return 1
  printf '%s\n' "$man" >"$tmp"
  out="$(timeout "${JXS_MAPPING_ENFORCE_TIMEOUT:-60}" python3 "${JXS_MAPPING_ENFORCE_SEEDER:-$lib/../obs_jxs_inputs.py}" \
    --enforce-mapping --manifest "$tmp" --host "$ip" --password "$pw" 2>&1)" || rc=$?
  rm -f "$tmp"
  [ -z "$out" ] || printf '%s\n' "$out" | sed 's/^/    [strih jxs-map] /'
  if [ "$rc" != 0 ]; then
    echo "WARNING issue 1387: the strih JPEG XS camera mapping enforce FAILED (rc=${rc}, see the [strih jxs-map] lines above) -- continuing, ${mode^^} mode will still exit non-zero." >&2
    return "$rc"
  fi
  return 0
}
