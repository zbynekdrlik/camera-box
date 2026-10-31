#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + overridable constants only) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell (setup-strih / verify-strih,
# `set -euo pipefail`), so strict mode here would leak into it. Every function is safe as a bare
# statement under the caller's errexit.
#
# scripts/lib/strih-jxs-provision.sh -- issue 1387: the JPEG XS path on the Linux OBS workstation boxes,
# provisioned from the box FACTS (scripts/strih-boxes/<box>.env, the ONE loader strih-box-facts.sh).
# Until this lib the strih inputs, their 36 ms pin, the strih program output's settings file and the
# stream's `JXS PGM src` were hand state (the slice runbooks over obs-websocket + a hand-written file).
#
#   setup-strih step 6 (strih_jxs_provision, ONE call line):
#     * installs scripts/obs_jxs_inputs.py (the launch-time seeder strih-obs-start.sh runs after the
#       strih_scenes seed) and renders /opt/camera-box/jxs-seed.json from the facts: `JXS camN src` per
#       STRIH_JXS_CAMERAS entry on the strih role (port, camera N, STRIH_JXS_CAMERA_PIN_MS, grid ON),
#       `JXS PGM src` on the stream role (STRIH_JXS_PROGRAM_PORT, camera = the program stream id,
#       STRIH_JXS_PROGRAM_PIN_MS, grid OFF); a box that declares none gets no manifest;
#     * on the strih role, upserts ~/.config/obs-studio/plugin_config/distroav/jxs-output.json (the
#       ONLY persistent store of the program output, vendor/distroav/src/jxs-output.cpp): enabled +
#       jxs_target = the VENUE's stream box (the stream-role fact file of this box's STRIH_VENUE: its
#       STRIH_IP + STRIH_JXS_PROGRAM_PORT) + jxs_stream_id; the operator's bpp / cores / pace keys are
#       kept. A venue whose stream box declares no program port leaves the file untouched.
#   verify-strih item 49 (strih_jxs_config_verify_row, next to the lost-slices row): `(jxs-seed)`,
#     `(jxs-inputs)` + `(jxs-place)` over obs-websocket (obs_jxs_inputs.py --verify against the
#     facts' render), `(jxs-output)`.
# Tests: tests/python/test_strih_jxs_provision_1387.py, tests/python/test_obs_jxs_inputs_1387.py.

# Overridable (test seams); the defaults are the box paths.
STRIH_JXS_SEED_MANIFEST="${STRIH_JXS_SEED_MANIFEST:-/opt/camera-box/jxs-seed.json}"
STRIH_JXS_SEEDER_BIN="${STRIH_JXS_SEEDER_BIN:-/usr/local/bin/obs_jxs_inputs.py}"
# The program link's stream id: the strih output's jxs_stream_id AND the stream input's jxs_camera
# (CB_JXS_OUT_STREAM_ID_DEFAULT in vendor/distroav/src/jxs-output-decide.h, pinned by pytest).
STRIH_JXS_PROGRAM_STREAM_ID=100
# The strih output's settings file under the OBS config dir.
STRIH_JXS_OUTPUT_REL="plugin_config/distroav/jxs-output.json"

# strih_jxs_checkout -> the checkout root this lib lives in (scripts/lib/../..).
strih_jxs_checkout() {
  (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
}

# strih_jxs_input_name N|PGM -> the OBS input name (`JXS cam2 src`, `JXS PGM src`); the holding scene is
# the same name without ` src` (an input cannot share its scene's name).
strih_jxs_input_name() {
  case "${1-}" in
    PGM) printf 'JXS PGM src\n' ;;
    *) printf 'JXS cam%s src\n' "${1-}" ;;
  esac
}

# _strih_jxs_entry NAME PORT CAMERA PIN GRID -> one manifest entry (JSON object, one line).
_strih_jxs_entry() {
  printf '{"name": "%s", "scene": "%s", "settings": {"jxs_port": %s, "jxs_camera": %s, "jxs_max_bpp": 0, "genlock_fifo": true, "genlock_latency_ms_src": %s, "genlock_grid": %s}}' \
    "$1" "${1% src}" "$2" "$3" "$4" "$5"
}

# strih_jxs_seed_manifest_json -> the loaded box's declared JPEG XS inputs as the jxs-seed.json text
# obs_jxs_inputs.py reads; NOTHING (rc 0) when the box declares none. rc 1 when the facts do not load.
strih_jxs_seed_manifest_json() {
  local jc pin pport ppin e n port ents=""
  jc="$(strih_box_fact STRIH_JXS_CAMERAS)" || return 1
  pin="$(strih_box_fact STRIH_JXS_CAMERA_PIN_MS)" || return 1
  pport="$(strih_box_fact STRIH_JXS_PROGRAM_PORT)" || return 1
  ppin="$(strih_box_fact STRIH_JXS_PROGRAM_PIN_MS)" || return 1
  if [ "$jc" != none ]; then
    for e in $jc; do
      n="${e%%:*}"; port="${e#*:}"
      ents="${ents:+${ents}, }$(_strih_jxs_entry "$(strih_jxs_input_name "$n")" "$port" "$n" "$pin" true)"
    done
  fi
  if [ "$pport" != none ]; then
    ents="${ents:+${ents}, }$(_strih_jxs_entry "$(strih_jxs_input_name PGM)" "$pport" "$STRIH_JXS_PROGRAM_STREAM_ID" "$ppin" false)"
  fi
  [ -n "$ents" ] || return 0
  printf '{"issue": 1387, "inputs": [%s]}\n' "$ents"
}

# strih_jxs_program_target -> where the loaded STRIH box sends its JPEG XS program: `<STRIH_IP>:<port>` of
# the ONE stream-role fact file of the same STRIH_VENUE that declares STRIH_JXS_PROGRAM_PORT, or `none`
# (the stream role; a venue with no stream box, or one that declares none). rc 1 + a stderr line for two
# stream boxes in one venue or a fact file that does not parse. Runs in a subshell (the loader's
# globals stay as they are).
strih_jxs_program_target() {
  local role venue
  role="$(strih_box_fact STRIH_ROLE)" || return 1
  venue="$(strih_box_fact STRIH_VENUE)" || return 1
  if [ "$role" != strih ]; then
    printf 'none\n'
    return 0
  fi
  (
    set +f
    shopt -u nullglob failglob 2>/dev/null || true
    hits="" target=none
    for f in "$(strih_box_dir)"/*.env; do
      [ -f "$f" ] || continue
      parsed="$(strih_box_parse_file "$f")" || { echo "strih-jxs: ${f} does not parse (issue 1387)" >&2; exit 1; }
      frole="" fvenue="" fip="" fport=""
      while IFS= read -r line; do
        case "$line" in
          STRIH_ROLE=*) frole="${line#*=}" ;;
          STRIH_VENUE=*) fvenue="${line#*=}" ;;
          STRIH_IP=*) fip="${line#*=}" ;;
          STRIH_JXS_PROGRAM_PORT=*) fport="${line#*=}" ;;
        esac
      done <<<"$parsed"
      [ "$frole" = stream ] && [ "$fvenue" = "$venue" ] || continue
      hits="${hits:+${hits} }${f##*/}"
      case "${fport:-none}" in none) target=none ;; *) target="${fip}:${fport}" ;; esac
    done
    case "$hits" in
      *" "*) echo "strih-jxs: venue ${venue} has two stream boxes (${hits}) -- the program link has one target (issue 1387)" >&2; exit 1 ;;
    esac
    printf '%s\n' "$target"
  )
}

# strih_jxs_output_render FILE TARGET STREAM_ID -> the jxs-output.json text (+ newline) with enabled,
# jxs_target and jxs_stream_id set, every other key of the existing FILE kept (the operator's bpp /
# cores / pace), the plugin's keys defaulted when absent; the plugin's key order first, then any other
# key. rc 3 (nothing printed) when FILE exists but is not a JSON object.
strih_jxs_output_render() {
  python3 - "${1-}" "${2-}" "${3-}" <<'PY'
import json, os, sys
path, target, sid = sys.argv[1], sys.argv[2], int(sys.argv[3])
old = {}
if os.path.exists(path):
    try:
        with open(path, encoding="utf-8-sig") as fh:
            old = json.load(fh)
    except ValueError:
        sys.exit(3)
    if not isinstance(old, dict):
        sys.exit(3)
out = {"enabled": True, "jxs_target": target, "jxs_stream_id": sid,
       "jxs_bpp_min": old.get("jxs_bpp_min", 0), "jxs_bpp_max": old.get("jxs_bpp_max", 0),
       "jxs_cores": old.get("jxs_cores", ""), "jxs_pace_mbps": old.get("jxs_pace_mbps", 0)}
for k, v in old.items():
    out.setdefault(k, v)
print(json.dumps(out))
PY
}

# strih_jxs_provision OBS_CFG DESKTOP_USER USER_HOME CHECKOUT -> setup-strih step 6 (module doc). rc 1 + a
# named line on a failed step; the caller fails the run.
strih_jxs_provision() {
  local cfg="${1:?OBS config dir required}" user="${2:?desktop user required}" home="${3:?user home required}"
  local src="${4:?checkout dir required}/scripts/obs_jxs_inputs.py" man target out rc st file
  [ -f "$src" ] || { echo "strih-jxs: ${src} not found next to the scripts (issue 1387)" >&2; return 1; }
  mkdir -p "$(dirname "$STRIH_JXS_SEEDER_BIN")" "$(dirname "$STRIH_JXS_SEED_MANIFEST")" || return 1
  st="$(strih_session_apps_put "$src" "$STRIH_JXS_SEEDER_BIN" 0755)" || return 1
  echo "  JPEG XS seeder ${STRIH_JXS_SEEDER_BIN}: ${st} (strih-obs-start.sh runs it after the strih_scenes seed)"
  man="$(strih_jxs_seed_manifest_json)" || { echo "strih-jxs: the box facts do not load (issue 1387)" >&2; return 1; }
  if [ -z "$man" ]; then
    rm -f "$STRIH_JXS_SEED_MANIFEST" || return 1
    echo "  JPEG XS inputs: none declared by this box's facts -- no manifest, the seeder does nothing"
  elif [ -f "$STRIH_JXS_SEED_MANIFEST" ] && [ "$(cat "$STRIH_JXS_SEED_MANIFEST")" = "$man" ]; then
    echo "  JPEG XS inputs: ${STRIH_JXS_SEED_MANIFEST} unchanged"
  else
    printf '%s\n' "$man" >"${STRIH_JXS_SEED_MANIFEST}.new" && chmod 0644 "${STRIH_JXS_SEED_MANIFEST}.new" \
      && mv -f "${STRIH_JXS_SEED_MANIFEST}.new" "$STRIH_JXS_SEED_MANIFEST" \
      || { rm -f "${STRIH_JXS_SEED_MANIFEST}.new"; echo "strih-jxs: cannot write ${STRIH_JXS_SEED_MANIFEST}" >&2; return 1; }
    echo "  JPEG XS inputs: ${STRIH_JXS_SEED_MANIFEST} written (the next OBS start creates / heals them)"
  fi
  target="$(strih_jxs_program_target)" || return 1
  if [ "$(strih_box_fact STRIH_ROLE)" != strih ]; then
    return 0
  fi
  file="${cfg}/${STRIH_JXS_OUTPUT_REL}"
  if [ "$target" = none ]; then
    echo "  JPEG XS program output: the venue's stream box declares no STRIH_JXS_PROGRAM_PORT -- ${file} left as it is"
    return 0
  fi
  strih_user_home_dir "$home" "$user" "${file%/*}" || return 1
  rc=0
  out="$(strih_jxs_output_render "$file" "$target" "$STRIH_JXS_PROGRAM_STREAM_ID")" || rc=$?
  if [ "$rc" = 3 ]; then
    cp -p "$file" "${file}.bak-$(date +%Y%m%dT%H%M%S)" || return 1
    echo "  WARN: ${file} is not a JSON object -- backed up, rewritten"
    rc=0
    out="$(strih_jxs_output_render /nonexistent/jxs-output.json "$target" "$STRIH_JXS_PROGRAM_STREAM_ID")" || rc=$?
  fi
  [ "$rc" = 0 ] && [ -n "$out" ] || { echo "strih-jxs: cannot render ${file} (rc ${rc})" >&2; return 1; }
  printf '%s\n' "$out" | obs_box_write_if_changed "$file" 0644 "${user}:$(id -gn "$user")" "JPEG XS program output (-> ${target})"
}

# strih_jxs_config_verify_row USER_HOME -> verify-strih item 49's config rows (the caller's ok / bad /
# note); always rc 0.
strih_jxs_config_verify_row() {
  local home="${1-}" co man tmp out line level text target file have
  co="$(strih_jxs_checkout)"
  man="$(strih_jxs_seed_manifest_json)" || { bad "(jxs-seed) the box facts do not load -- cannot grade the JPEG XS path (issue 1387)"; return 0; }
  # (jxs-seed): the installed manifest = the facts' render, the installed seeder = the checkout's.
  if [ -z "$man" ]; then
    if [ -e "$STRIH_JXS_SEED_MANIFEST" ]; then
      bad "(jxs-seed) ${STRIH_JXS_SEED_MANIFEST} exists, but this box's facts declare no JPEG XS input -- re-run setup-strih.sh step 6 (issue 1387)"
    else
      note "(jxs-seed) this box's facts declare no JPEG XS input (STRIH_JXS_CAMERAS / STRIH_JXS_PROGRAM_PORT none) -- nothing to grade (issue 1387)"
    fi
  elif [ ! -f "$STRIH_JXS_SEED_MANIFEST" ] || [ "$(cat "$STRIH_JXS_SEED_MANIFEST" 2>/dev/null)" != "$man" ]; then
    bad "(jxs-seed) ${STRIH_JXS_SEED_MANIFEST} is missing or differs from the box facts -- re-run setup-strih.sh step 6 (issue 1387)"
  elif ! cmp -s "$STRIH_JXS_SEEDER_BIN" "${co}/scripts/obs_jxs_inputs.py"; then
    bad "(jxs-seed) ${STRIH_JXS_SEEDER_BIN} is missing or differs from the checkout -- re-run setup-strih.sh step 6 (issue 1387)"
  else
    ok "(jxs-seed) ${STRIH_JXS_SEED_MANIFEST} = the box facts, the launch seeder installed"
  fi
  # (jxs-inputs) + (jxs-place): over obs-websocket, against the FACTS' render (never the installed copy).
  if [ -n "$man" ]; then
    tmp="$(mktemp -d)" || { bad "(jxs-inputs) cannot create a scratch dir"; return 0; }
    printf '%s\n' "$man" >"${tmp}/jxs-seed.json"
    out="$(timeout 30 python3 "${co}/scripts/obs_jxs_inputs.py" --verify --manifest "${tmp}/jxs-seed.json" \
      --host "${STRIH_JXS_OBS_HOST:-127.0.0.1}" --port "${STRIH_JXS_OBS_PORT:-4455}" 2>&1 || true)"
    rm -rf "$tmp"
    [ -n "$out" ] || bad "(jxs-inputs) obs_jxs_inputs.py --verify printed nothing (issue 1387)"
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      level="${line%% *}"; text="${line#* }"
      case "$level" in
        PASS) ok "$text" ;;
        NOTE) note "$text" ;;
        FAIL) bad "$text" ;;
        *) bad "(jxs-inputs) unexpected verifier output: ${line}" ;;
      esac
    done <<<"$out"
  fi
  # (jxs-output): the strih program output's settings file vs the venue's stream box.
  [ "$(strih_box_fact STRIH_ROLE)" = strih ] || return 0
  target="$(strih_jxs_program_target)" || { bad "(jxs-output) the venue's program target does not resolve (above) (issue 1387)"; return 0; }
  file="${home}/.config/obs-studio/${STRIH_JXS_OUTPUT_REL}"
  have="$(python3 - "$file" <<'PY' 2>/dev/null || true
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8-sig") as fh:
        d = json.load(fh)
except FileNotFoundError:
    print("absent"); sys.exit(0)
except ValueError:
    print("unparseable"); sys.exit(0)
if not isinstance(d, dict):
    print("unparseable"); sys.exit(0)
print("%s %s %s" % (json.dumps(d.get("enabled")), d.get("jxs_target"), json.dumps(d.get("jxs_stream_id"))))
PY
)"
  if [ "$target" = none ]; then
    case "$have" in
      "true "*) bad "(jxs-output) ${file} enables the program output (${have}), but the venue declares no JPEG XS program (STRIH_JXS_PROGRAM_PORT none) -- hand state (issue 1387)" ;;
      *) note "(jxs-output) the venue declares no JPEG XS program (STRIH_JXS_PROGRAM_PORT none) -- nothing to grade (issue 1387)" ;;
    esac
  elif [ "$have" = "true ${target} ${STRIH_JXS_PROGRAM_STREAM_ID}" ]; then
    ok "(jxs-output) ${file}: enabled, the program to ${target} (stream id ${STRIH_JXS_PROGRAM_STREAM_ID}) -- the venue's stream box"
  else
    bad "(jxs-output) ${file} reads '${have:-unreadable}', not 'true ${target} ${STRIH_JXS_PROGRAM_STREAM_ID}' (enabled, target, stream id) -- re-run setup-strih.sh step 6, then restart OBS (issue 1387)"
  fi
  return 0
}
