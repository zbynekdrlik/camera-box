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
#       kept. A venue whose stream box declares no program port gets an enabled file DISABLED (every
#       other key kept), anything else left alone. setup runs as root in the user's tree: a symlinked
#       file is refused, the reads are O_NOFOLLOW, an unparseable file is moved aside (mv -T).
#   verify-strih item 49 (strih_jxs_config_verify_row, next to the lost-slices row): `(jxs-seed)`,
#     `(jxs-inputs)` + `(jxs-place)` + (strih) `(jxs-output-live)` over obs-websocket
#     (obs_jxs_inputs.py --verify against the facts' render and the derived target; an unreachable
#     websocket FAILs while an obs process runs), `(jxs-output)` (the file the output is made from),
#     and on a Dante box `(jxs-cores)` (an empty jxs_cores or one holding the reserved CPU FAILs).
#   setup-strih step 13 (strih_jxs_output_cores_reconcile, called by strih_dante_input_install after the
#     managers' default): the program encoder's jxs_cores re-derived now that baseline step 11 wrote the
#     OBS mask (step 6 runs before it, so a FRESH Dante box got none, issue 1485 review).
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

# strih_jxs_input_name (the input naming rule) lives in strih-box-facts.sh (issue 1387 slice 1b).

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

# strih_jxs_output_render FILE TARGET STREAM_ID [disable] -> the jxs-output.json text (+ newline) with
# enabled, jxs_target and jxs_stream_id set, every other key of the existing FILE kept (the operator's
# bpp / cores / pace), the plugin's keys defaulted when absent; the plugin's key order first, then any
# other key. `disable` keeps every key and sets only enabled false. setup runs as root in the desktop
# user's tree, so FILE is opened O_NOFOLLOW and must be a regular file: rc 4 (nothing printed) for a
# symlink or anything else; rc 3 when it is not a JSON object.
# DEFAULT_CORES (5th, issue 1485): the derived jxs_cores. An empty jxs_cores puts the encoder on every
# ONLINE CPU, the reserved Dante CPU included, so a Dante box passes the OBS mask without it
# (strih_jxs_output_default_cores). The value written is recorded as `jxs_cores_derived`, and the file's
# jxs_cores is re-derived while it is empty or still equals that record; an operator's own value is kept,
# a WARNING on stderr when it holds RESERVED (6th, the reserved Dante CPU; empty = none). An empty
# derived value drops the record (a box that stopped taking the Dante program). MODE `cores` changes
# only these two keys of an existing FILE (setup-strih step 13).
strih_jxs_output_render() {
  python3 - "${1-}" "${2-}" "${3-}" "${4-}" "${5-}" "${6-}" <<'PY'
import json, os, stat, sys
path, target, mode, cores, reserved = sys.argv[1], sys.argv[2], sys.argv[4], sys.argv[5], sys.argv[6]
sid = int(sys.argv[3] or 0)


def holds(cpulist, cpu):
    # a cpulist that does not parse counts as holding it (fail closed: the WARNING is shown)
    try:
        for item in cpulist.split(","):
            a, _, b = item.strip().partition("-")
            if int(a) <= int(cpu) <= int(b or a):
                return True
    except ValueError:
        return True
    return False


def with_cores(out, old):
    stored = old.get("jxs_cores", "") or ""
    derived = old.get("jxs_cores_derived")
    if stored == "" or (isinstance(derived, str) and stored == derived):
        out["jxs_cores"] = cores
        if cores:
            out["jxs_cores_derived"] = cores
        else:
            out.pop("jxs_cores_derived", None)
    elif reserved and holds(str(stored), reserved):
        print("  WARNING: %s: the operator's jxs_cores '%s' holds the reserved Dante CPU %s -- the JPEG XS "
              "program encoder can run there (issue 1485); kept, remove it to get the derived set"
              % (path, stored, reserved), file=sys.stderr)
    return out


old = {}
try:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
except FileNotFoundError:
    fd = None
except OSError:
    sys.exit(4)  # ELOOP: a symlink, never followed
if fd is not None:
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        sys.exit(4)
    with os.fdopen(fd, encoding="utf-8-sig") as fh:
        try:
            old = json.load(fh)
        except ValueError:
            sys.exit(3)
    if not isinstance(old, dict):
        sys.exit(3)
if mode == "disable":
    out = dict(old, enabled=False)
elif mode == "cores":
    if fd is None:
        sys.exit(5)  # nothing to patch
    out = with_cores(dict(old), old)
else:
    out = {"enabled": True, "jxs_target": target, "jxs_stream_id": sid,
           "jxs_bpp_min": old.get("jxs_bpp_min", 0), "jxs_bpp_max": old.get("jxs_bpp_max", 0),
           "jxs_cores": old.get("jxs_cores", ""), "jxs_pace_mbps": old.get("jxs_pace_mbps", 0)}
    for k, v in old.items():
        out.setdefault(k, v)
    out = with_cores(out, old)
print(json.dumps(out))
PY
}

# _strih_jxs_dante_lib -> rc 0 when the Dante libs this one calls are sourced (strih-provision.sh sources
# them), else rc 1 + a named line: a missing lib must never read as "not a Dante box" (review finding 9).
_strih_jxs_dante_lib() {
  local f
  for f in strih_program_dante_declared strih_dante_rx_pin_plan strih_dante_reserved_cpu; do
    declare -F "$f" >/dev/null || { echo "strih-jxs: ${f} is not defined -- source scripts/lib/strih-provision.sh (issue 1485)" >&2; return 1; }
  done
}

# strih_jxs_output_default_cores -> the OBS mask without the reserved Dante CPU on a box that declares
# the Dante program (the same plan as strih_dante_rx_pin_install), else nothing; nothing too while the
# OBS mask is not written yet (step 6 on a fresh box: step 13 derives it, strih_jxs_output_cores_reconcile).
# rc 1 only when the Dante libs are not sourced.
strih_jxs_output_default_cores() {
  local online base plan
  _strih_jxs_dante_lib || return 1
  strih_program_dante_declared || return 0
  online="$(head -n 1 "${OBS_BOX_ONLINE_CPUS:-/sys/devices/system/cpu/online}" 2>/dev/null || true)"
  base="$(head -n 1 "${STRIH_DANTE_RX_PIN_ROOT:-}/etc/strih-isolated-cpus.conf" 2>/dev/null || true)"
  plan="$(strih_dante_rx_pin_plan "$online" "$base" 2>/dev/null)" || return 0
  printf '%s\n' "${plan#* }"
}

# strih_jxs_provision OBS_CFG DESKTOP_USER USER_HOME CHECKOUT -> setup-strih step 6 (module doc). rc 1 + a
# named line on a failed step; the caller fails the run.
strih_jxs_provision() {
  local cfg="${1:?OBS config dir required}" user="${2:?desktop user required}" home="${3:?user home required}"
  local src="${4:?checkout dir required}/scripts/obs_jxs_inputs.py" man target out rc st file cores res
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
  # root in the desktop user's tree: a planted symlink is never followed (read or write).
  if [ -L "$file" ]; then
    echo "strih-jxs: ${file} is a symlink -- refusing to read or write through it as root (issue 1387)" >&2
    return 1
  fi
  if [ "$target" = none ]; then
    if [ "$(_strih_jxs_output_state "$file")" = enabled ]; then
      # converge to what verify grades: a venue with no program declares the link off
      out="$(strih_jxs_output_render "$file" none 0 disable)" || { echo "strih-jxs: cannot render ${file}" >&2; return 1; }
      printf '%s\n' "$out" | obs_box_write_if_changed "$file" 0644 "${user}:$(id -gn "$user")" \
        "JPEG XS program output (disabled: the venue declares no STRIH_JXS_PROGRAM_PORT)" || return 1
    else
      echo "  JPEG XS program output: the venue's stream box declares no STRIH_JXS_PROGRAM_PORT -- nothing enabled, ${file} left as it is"
    fi
    return 0
  fi
  strih_user_home_dir "$home" "$user" "${file%/*}" || return 1
  rc=0
  cores="$(strih_jxs_output_default_cores)" || return 1
  res="$(strih_dante_reserved_cpu)"
  out="$(strih_jxs_output_render "$file" "$target" "$STRIH_JXS_PROGRAM_STREAM_ID" "" "$cores" "$res")" || rc=$?
  if [ "$rc" = 3 ]; then
    # moved aside (a rename never follows a link at the destination; -T never moves INTO a directory)
    mv -T "$file" "${file}.bak-$(date +%Y%m%dT%H%M%S)" || return 1
    echo "  WARN: ${file} is not a JSON object -- moved aside, rewritten"
    rc=0
    out="$(strih_jxs_output_render "$file" "$target" "$STRIH_JXS_PROGRAM_STREAM_ID" "" "$cores" "$res")" || rc=$?
  fi
  [ "$rc" = 0 ] && [ -n "$out" ] || { echo "strih-jxs: cannot render ${file} (rc ${rc}: 4 = not a regular file)" >&2; return 1; }
  printf '%s\n' "$out" | obs_box_write_if_changed "$file" 0644 "${user}:$(id -gn "$user")" "JPEG XS program output (-> ${target})" || return 1
}

# strih_jxs_output_cores_reconcile USER_HOME USER -> setup-strih step 13 (strih_dante_input_install, after
# the managers' default): the program output's jxs_cores re-derived from the OBS mask baseline step 11 has
# written by now (render mode `cores`: only jxs_cores + jxs_cores_derived change). The strih role with an
# existing file only; a Dante box whose set cannot be derived here fails by name (rc 1). Prints the
# write's one line.
strih_jxs_output_cores_reconcile() {
  local home="${1:?user home required}" user="${2:?desktop user required}" file cores res out rc=0
  _strih_jxs_dante_lib || return 1
  [ "$(strih_box_fact STRIH_ROLE)" = strih ] || return 0
  file="${home}/.config/obs-studio/${STRIH_JXS_OUTPUT_REL}"
  if [ -L "$file" ]; then
    echo "strih-jxs: ${file} is a symlink -- refusing to read or write through it as root (issue 1387)" >&2
    return 1
  fi
  [ -f "$file" ] || return 0
  cores="$(strih_jxs_output_default_cores)" || return 1
  if [ -z "$cores" ] && strih_program_dante_declared; then
    echo "strih-jxs: the JPEG XS program encoder's jxs_cores cannot be derived on this Dante box (no OBS mask in ${STRIH_DANTE_RX_PIN_ROOT:-}/etc/strih-isolated-cpus.conf beside the online CPUs) -- ${file} left as it is (issue 1485)" >&2
    return 1
  fi
  res="$(strih_dante_reserved_cpu)"
  out="$(strih_jxs_output_render "$file" "" 0 cores "$cores" "$res")" || rc=$?
  [ "$rc" = 0 ] && [ -n "$out" ] || { echo "strih-jxs: cannot patch jxs_cores of ${file} (rc ${rc})" >&2; return 1; }
  printf '%s\n' "$out" | obs_box_write_if_changed "$file" 0644 "${user}:$(id -gn "$user")" "JPEG XS program output cores (${cores:-every CPU})" || return 1
}

# _strih_jxs_output_state FILE -> enabled | disabled | absent | unreadable (a symlink or anything that is
# not a regular JSON object). O_NOFOLLOW, like the render.
_strih_jxs_output_state() {
  python3 - "${1-}" <<'PY' 2>/dev/null || printf 'unreadable\n'
import json, os, stat, sys
try:
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
except FileNotFoundError:
    print("absent"); sys.exit(0)
except OSError:
    print("unreadable"); sys.exit(0)
if not stat.S_ISREG(os.fstat(fd).st_mode):
    print("unreadable"); sys.exit(0)
try:
    with os.fdopen(fd, encoding="utf-8-sig") as fh:
        d = json.load(fh)
except ValueError:
    print("unreadable"); sys.exit(0)
print("enabled" if isinstance(d, dict) and d.get("enabled") is True else "disabled" if isinstance(d, dict) else "unreadable")
PY
}

# strih_jxs_config_verify_row USER_HOME -> verify-strih item 49's config rows (the caller's ok / bad /
# note); always rc 0.
strih_jxs_config_verify_row() {
  local home="${1-}" co man tmp out line level text target file have
  local -a seed_args
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
  # The venue's program target (strih role only; `none` elsewhere).
  target=none
  if [ "$(strih_box_fact STRIH_ROLE)" = strih ]; then
    target="$(strih_jxs_program_target)" || { bad "(jxs-output) the venue's program target does not resolve (above) (issue 1387)"; target=unresolved; }
  fi
  # (jxs-inputs) + (jxs-place) + (jxs-output-live): over obs-websocket (verify-strih's WS_HOST), against
  # the FACTS' render (never the installed copy) and the derived program target.
  if [ -n "$man" ] || { [ "$target" != none ] && [ "$target" != unresolved ]; }; then
    tmp="$(mktemp -d)" || { bad "(jxs-inputs) cannot create a scratch dir"; return 0; }
    [ -z "$man" ] || printf '%s\n' "$man" >"${tmp}/jxs-seed.json"
    seed_args=(--verify --manifest "${tmp}/jxs-seed.json" --host "${WS_HOST:-127.0.0.1}" --port "${STRIH_JXS_OBS_PORT:-4455}")
    case "$target" in none | unresolved) ;; *) seed_args+=(--output-target "$target" --output-stream-id "$STRIH_JXS_PROGRAM_STREAM_ID") ;; esac
    out="$(timeout 30 python3 "${co}/scripts/obs_jxs_inputs.py" "${seed_args[@]}" 2>&1 || true)"
    rm -rf "$tmp"
    [ -n "$out" ] || bad "(jxs-inputs) obs_jxs_inputs.py --verify printed nothing (issue 1387)"
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      level="${line%% *}"; text="${line#* }"
      case "$level" in
        PASS) ok "$text" ;;
        NOTE)
          # an unreachable websocket is a NOTE only while OBS is down (the studio-mode row's rule)
          if [[ "$text" == "(jxs-inputs) OBS WebSocket "* ]] && strih_operator_obs_running; then
            bad "${text%% -- OBS not running?*} -- OBS runs but its websocket does not answer (issue 1387)"
          else
            note "$text"
          fi
          ;;
        FAIL) bad "$text" ;;
        *) bad "(jxs-inputs) unexpected verifier output: ${line}" ;;
      esac
    done <<<"$out"
  fi
  # (jxs-output): the program output's settings file (the plugin re-creates the output from it at every
  # OBS start) vs the venue's stream box.
  case "$target" in unresolved) return 0 ;; esac
  [ "$(strih_box_fact STRIH_ROLE)" = strih ] || return 0
  file="${home}/.config/obs-studio/${STRIH_JXS_OUTPUT_REL}"
  have="$(_strih_jxs_output_fields "$file")"
  if [ "$target" = none ]; then
    case "$have" in
      "true "*) bad "(jxs-output) ${file} enables the program output (${have}), but the venue declares no JPEG XS program (STRIH_JXS_PROGRAM_PORT none) -- hand state; setup-strih step 6 disables it (issue 1387)" ;;
      *) note "(jxs-output) the venue declares no JPEG XS program (STRIH_JXS_PROGRAM_PORT none) -- nothing to grade (issue 1387)" ;;
    esac
  elif [ "$have" = "true ${target} ${STRIH_JXS_PROGRAM_STREAM_ID}" ]; then
    ok "(jxs-output) ${file}: enabled, the program to ${target} (stream id ${STRIH_JXS_PROGRAM_STREAM_ID}) -- the venue's stream box"
  else
    bad "(jxs-output) ${file} reads '${have:-unreadable}', not 'true ${target} ${STRIH_JXS_PROGRAM_STREAM_ID}' (enabled, target, stream id) -- re-run setup-strih.sh step 6, then restart OBS (issue 1387)"
  fi
  [ "$target" = none ] || _strih_jxs_cores_row "$file"
  return 0
}

# _strih_jxs_cores_row FILE -> verify item 49's `(jxs-cores)` row on a Dante box (issue 1485): an empty
# jxs_cores (the encoder on every online CPU) or one holding the reserved Dante CPU FAILs.
_strih_jxs_cores_row() {
  local file="${1-}" cores res
  if ! _strih_jxs_dante_lib 2>/dev/null || ! declare -F _strih_dante_cpulist_has >/dev/null; then
    bad "(jxs-cores) the Dante libs are not sourced -- the encoder's cores are not graded (issue 1485)"
    return 0
  fi
  strih_program_dante_declared || return 0
  cores="$(_strih_jxs_output_key "$file" jxs_cores)"
  res="$(strih_dante_reserved_cpu)"
  if [ -z "$cores" ]; then
    bad "(jxs-cores) ${file}: jxs_cores is empty -- the JPEG XS program encoder runs on every online CPU, the reserved Dante CPU ${res:-?} included; re-run setup-strih.sh (step 13 derives it), then Stop/StartOutput (issue 1485)"
  elif [ -n "$res" ] && _strih_dante_cpulist_has "$cores" "$res"; then
    bad "(jxs-cores) ${file}: jxs_cores '${cores}' holds the reserved Dante CPU ${res} -- remove it (or empty it and re-run setup-strih.sh step 13), then Stop/StartOutput (issue 1485)"
  else
    ok "(jxs-cores) ${file}: jxs_cores '${cores}' stays off the reserved Dante CPU ${res:-?}"
  fi
}

# _strih_jxs_output_key FILE KEY -> the string value of KEY (empty when absent, not a string, or the file
# unreadable). O_NOFOLLOW: verify runs as root.
_strih_jxs_output_key() {
  python3 - "${1-}" "${2-}" <<'PY' 2>/dev/null || true
import json, os, stat, sys
try:
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        sys.exit(0)
    with os.fdopen(fd, encoding="utf-8-sig") as fh:
        d = json.load(fh)
except (OSError, ValueError):
    sys.exit(0)
v = d.get(sys.argv[2]) if isinstance(d, dict) else None
print(v if isinstance(v, str) else "")
PY
}

# _strih_jxs_output_fields FILE -> `<enabled> <jxs_target> <jxs_stream_id>` (JSON spellings), or absent |
# unreadable (a symlink, not a regular file, not a JSON object). O_NOFOLLOW: verify runs as root too.
_strih_jxs_output_fields() {
  python3 - "${1-}" <<'PY' 2>/dev/null || printf 'unreadable\n'
import json, os, stat, sys
try:
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
except FileNotFoundError:
    print("absent"); sys.exit(0)
except OSError:
    print("unreadable"); sys.exit(0)
if not stat.S_ISREG(os.fstat(fd).st_mode):
    print("unreadable"); sys.exit(0)
try:
    with os.fdopen(fd, encoding="utf-8-sig") as fh:
        d = json.load(fh)
except ValueError:
    print("unreadable"); sys.exit(0)
if not isinstance(d, dict):
    print("unreadable"); sys.exit(0)
print("%s %s %s" % (json.dumps(d.get("enabled")), d.get("jxs_target"), json.dumps(d.get("jxs_stream_id"))))
PY
}
