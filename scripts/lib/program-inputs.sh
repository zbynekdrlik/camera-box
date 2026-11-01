#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only) -- the scripts/lib/*.sh convention:
# sourcing runs in the CALLER's shell (recording-e2e.sh / rig-mode.sh / av-soak.sh, `set -euo
# pipefail`), so strict mode here would leak into it. Every function is safe as a bare statement or
# an `if`-condition under the caller's errexit.
#
# scripts/lib/program-inputs.sh -- issue 1387 slice 1b (design 6558387590): WHICH strih camera inputs
# and WHICH stream program input the full-path E2E, rig-mode TEST and the A/V soak measure, resolved
# ONCE from the box FACTS (scripts/strih-boxes/<box>.env, the ONE loader strih-box-facts.sh):
#
#   * the strih box's STRIH_JXS_CAMERAS covers every one of its STRIH_CAMERAS and the stream box
#     declares a STRIH_JXS_PROGRAM_PORT -> the JPEG XS path: the cameras arrive as `JXS camN src`,
#     the program as `JXS PGM src` (strih_jxs_input_name, the one naming rule);
#   * neither box declares any JPEG XS -> the NDI path, byte-identical to before (`NDI camN`,
#     `NDI 2ME PGM`, the callers' own defaults);
#   * anything in between (some cameras on NDI, a strih on JPEG XS with a stream that receives no
#     JPEG XS program, ...) is a MIXED venue: refused, naming the cameras / the box.
#
# A host no obs-fleet row names, or a box with no fact file (a Windows box), declares no JPEG XS.
#
#   program_inputs_resolve STRIH STREAM [SOURCE_CAM]  sets E2E_PROGRAM_PATH, CAMERA_STRIH_JXS_SET and
#       (JPEG XS only) STRIH_PROG_SOURCE / STREAM_PROG_SOURCE, each only when UNSET (an env override
#       wins; E2E_PROGRAM_PATH must agree with the facts), plus PROGRAM_INPUTS_STRIH_BOX /
#       PROGRAM_INPUTS_STREAM_BOX / PROGRAM_INPUTS_STREAM_PIN_MS. rc 1 + the reason on stderr.
#   program_path_ndi                                  0 on the NDI path (also when nothing resolved)
#   program_inputs_refuse_ndi_only_optins             the E2E opt-ins that write NDI pins refuse
#   cambox_jxs_burn_setenv_resolve_cmd                REMOTE text: the E2E burn unit's JPEG XS target
#   program_inputs_jxs_preflight HERE STRIH STREAM OUTDIR RUN_ID     the BLOCKING [4i/8jxs] step
#   program_inputs_rendered_verify HERE STRIH STREAM SWEEP           the BLOCKING rendered-input check
#   program_inputs_av_pin_hold                        the #856 A/V apply on JPEG XS = a recommendation
#
# Tests: tests/python/test_program_inputs_1387.py.

# _program_inputs_libdir -> this lib's directory.
_program_inputs_libdir() {
  (cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
}

# _program_inputs_facts STRIH STREAM -> the two boxes' JPEG XS facts as KEY=VALUE lines on stdout:
# path= (jxs|ndi), cams= (the strih's JPEG XS cameras as cam names), pin= (the stream program pin),
# strih_box= / stream_box= (the fact-file boxes, empty for none). Runs in a SUBSHELL so the caller
# never gets a loaded box. rc 2 = a mixed venue (the reason on stderr), rc 3 = a fact file that does
# not load (the loader's own line says why).
_program_inputs_facts() {
  local strih="${1-}" stream="${2-}" dir
  dir="$(_program_inputs_libdir)" || return 3
  (
    set +e
    declare -F obs_fleet_name_for_host >/dev/null || . "${dir}/obs-fleet.sh" || exit 3
    declare -F strih_box_load >/dev/null || . "${dir}/strih-box-facts.sh" || exit 3
    # the fleet row's box, when it has a fact file. A file test, never `strih_box_known | grep -q`:
    # grep -q's early exit SIGPIPEs the lister under the caller's pipefail and reads as "no box".
    _pi_box() {
      local b
      b="$(obs_fleet_name_for_host "${1-}" 2>/dev/null)" || return 0
      [[ "$b" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || return 0
      [ -f "$(strih_box_dir)/${b}.env" ] && printf '%s' "$b"
      return 0
    }
    sbox="$(_pi_box "$strih")"
    pbox="$(_pi_box "$stream")"
    jcams="" ndicams="" pport=none ppin=""
    if [ -n "$sbox" ]; then
      strih_box_load "$sbox" >/dev/null || exit 3
      jc="$(strih_box_fact STRIH_JXS_CAMERAS)" || exit 3
      sc="$(strih_box_fact STRIH_CAMERAS)" || exit 3
      if [ "$jc" != none ]; then
        for e in $jc; do jcams="${jcams:+${jcams} }cam${e%%:*}"; done
        if [ "$sc" != none ]; then
          for n in $sc; do
            case " ${jcams} " in *" cam${n} "*) ;; *) ndicams="${ndicams:+${ndicams} }cam${n}" ;; esac
          done
        fi
      fi
    fi
    if [ -n "$pbox" ]; then
      strih_box_load "$pbox" >/dev/null || exit 3
      pport="$(strih_box_fact STRIH_JXS_PROGRAM_PORT)" || exit 3
      ppin="$(strih_box_fact STRIH_JXS_PROGRAM_PIN_MS)" || exit 3
    fi
    path=""
    if [ -z "$jcams" ] && [ "$pport" = none ]; then
      path=ndi
    elif [ -n "$jcams" ] && [ -z "$ndicams" ] && [ "$pport" != none ]; then
      path=jxs
    elif [ -n "$ndicams" ]; then
      echo "program-inputs: the strih '${sbox}' (${strih}) is a MIXED venue: ${jcams} over JPEG XS but ${ndicams} over NDI (scripts/strih-boxes/${sbox}.env STRIH_JXS_CAMERAS vs STRIH_CAMERAS) -- the E2E measures one path, never both (issue 1387)" >&2
      exit 2
    elif [ -n "$jcams" ]; then
      echo "program-inputs: the strih '${sbox}' (${strih}) takes ${jcams} over JPEG XS, but the stream '${pbox:-<no fact file>}' (${stream}) declares no JPEG XS program (STRIH_JXS_PROGRAM_PORT none) -- a MIXED venue, the E2E measures one path (issue 1387)" >&2
      exit 2
    else
      echo "program-inputs: the stream '${pbox}' (${stream}) receives the program over JPEG XS (STRIH_JXS_PROGRAM_PORT ${pport}), but the strih '${sbox:-<no fact file>}' (${strih}) declares no JPEG XS camera -- a MIXED venue, the E2E measures one path (issue 1387)" >&2
      exit 2
    fi
    printf 'path=%s\ncams=%s\npin=%s\nstrih_box=%s\nstream_box=%s\n' "$path" "$jcams" "$ppin" "$sbox" "$pbox"
  )
}

# program_inputs_resolve STRIH STREAM [SOURCE_CAM] -> see the header. SOURCE_CAM (default
# $CAMERA_NAME; empty = leave STRIH_PROG_SOURCE alone) is the camera whose strih input the program
# scene shows. Prints one summary line on stderr. rc 1 on a mixed venue, unreadable facts, or an
# E2E_PROGRAM_PATH override the facts contradict; nothing is set then.
program_inputs_resolve() {
  local strih="${1-}" stream="${2-}" src="${3-${CAMERA_NAME:-}}" out rc=0 line
  local path="" cams="" pin="" sbox="" pbox=""
  out="$(_program_inputs_facts "$strih" "$stream")" || rc=$?
  if [ "$rc" != 0 ]; then
    echo "program-inputs: REFUSED (rc ${rc}) -- cannot tell which inputs to measure on strih ${strih} / stream ${stream} (the line above says why) (issue 1387)" >&2
    return 1
  fi
  while IFS= read -r line; do
    case "$line" in
      path=*) path="${line#path=}" ;;
      cams=*) cams="${line#cams=}" ;;
      pin=*) pin="${line#pin=}" ;;
      strih_box=*) sbox="${line#strih_box=}" ;;
      stream_box=*) pbox="${line#stream_box=}" ;;
    esac
  done <<<"$out"
  case "$path" in jxs | ndi) ;; *) echo "program-inputs: REFUSED -- no path resolved (issue 1387)" >&2; return 1 ;; esac
  if [ -n "${E2E_PROGRAM_PATH:-}" ] && [ "$E2E_PROGRAM_PATH" != "$path" ]; then
    echo "program-inputs: REFUSED -- E2E_PROGRAM_PATH=${E2E_PROGRAM_PATH} but the box facts say ${path} (strih '${sbox}', stream '${pbox}'): the facts are the one source of truth (issue 1387)" >&2
    return 1
  fi
  E2E_PROGRAM_PATH="$path"
  CAMERA_STRIH_JXS_SET="${CAMERA_STRIH_JXS_SET:-$cams}"
  PROGRAM_INPUTS_STRIH_BOX="$sbox"
  PROGRAM_INPUTS_STREAM_BOX="$pbox"
  PROGRAM_INPUTS_STREAM_PIN_MS="$pin"
  if [ "$path" = jxs ]; then
    if [ -z "${STRIH_PROG_SOURCE:-}" ] && [ -n "$src" ]; then
      STRIH_PROG_SOURCE="$(camera_strih_input "$src")"
    fi
    if [ -z "${STREAM_PROG_SOURCE:-}" ]; then
      STREAM_PROG_SOURCE="$(_program_inputs_jxs_name PGM)" || return 1
    fi
  fi
  echo "[program-inputs] issue 1387: the ${path} path (strih '${sbox:-<no facts>}': JPEG XS cameras '${CAMERA_STRIH_JXS_SET:-}'; stream '${pbox:-<no facts>}') -- measured inputs: strih '${STRIH_PROG_SOURCE:-<caller default>}', stream '${STREAM_PROG_SOURCE:-<caller default>}'" >&2
  return 0
}

# _program_inputs_jxs_name N|PGM -> the JPEG XS input name through the ONE naming rule
# (strih_jxs_input_name in strih-box-facts.sh), loaded in a subshell when the caller has not.
_program_inputs_jxs_name() {
  local dir
  if declare -F strih_jxs_input_name >/dev/null; then
    strih_jxs_input_name "${1-}"
    return
  fi
  dir="$(_program_inputs_libdir)" || return 1
  ( . "${dir}/strih-box-facts.sh" && strih_jxs_input_name "${1-}" )
}

# program_path_ndi -> 0 on the NDI path. A caller that never resolved reads NDI (the pre-1387 shape).
program_path_ndi() {
  [ "${E2E_PROGRAM_PATH:-ndi}" = ndi ]
}

# program_inputs_refuse_ndi_only_optins -> rc 1 (named on stderr) when an E2E opt-in that writes or
# idles an NDI input is set on the JPEG XS path: COLD_CUT_BYPASS_CAM (idles an NDI receiver),
# MEASUREMENT_EQ=1 and PRERECORD_PHASE_CALIBRATE=1 (write strih NDI pins). On JPEG XS every pin is a
# box fact the launch seeder heals back, so they would measure nothing real. rc 0 otherwise.
program_inputs_refuse_ndi_only_optins() {
  local bad=""
  program_path_ndi && return 0
  [ -z "${COLD_CUT_BYPASS_CAM:-}" ] || bad="${bad:+${bad}, }COLD_CUT_BYPASS_CAM=${COLD_CUT_BYPASS_CAM}"
  [ "${MEASUREMENT_EQ:-0}" != 1 ] || bad="${bad:+${bad}, }MEASUREMENT_EQ=1"
  [ "${PRERECORD_PHASE_CALIBRATE:-0}" != 1 ] || bad="${bad:+${bad}, }PRERECORD_PHASE_CALIBRATE=1"
  [ -n "$bad" ] || return 0
  echo "ERROR: [preflight] REFUSED: ${bad} -- an NDI-only measurement opt-in on the JPEG XS path (its pins are box facts, the cameras are not NDI inputs); unset it (issue 1387)" >&2
  return 1
}

# cambox_jxs_burn_setenv_resolve_cmd -> REMOTE bash (embedded in the E2E burn deploy's ssh string
# through `$(...)`, the cpu_affinity_burn_resolve_cmd shape) that sets CAMBOX_JXS_BURN_SETENV to
# `--setenv=CAMERA_BOX_JXS_TARGET=<target>` from the LOADED camera-box unit's environment (the facts'
# jxs.conf drop-in, verify-device (ay)), or empty on a camera that sends no JPEG XS. The temporary
# burn unit then sends the burned frame over JPEG XS like production does (issue 1387 slice 1a).
# Only an IPv4:port target is passed (it lands unquoted on the systemd-run line); anything else is
# refused with a WARNING. Every statement ends with `;` (the `$(...)` trailing-newline gotcha).
cambox_jxs_burn_setenv_resolve_cmd() {
  printf '%s\n' \
    '_cbjxs_t="$(systemctl show -p Environment --value camera-box 2>/dev/null | tr " " "\n" | awk -F= '"'"'$1 == "CAMERA_BOX_JXS_TARGET" { v = substr($0, length($1) + 2) } END { print v }'"'"' || true)";' \
    'case "$_cbjxs_t" in "") CAMBOX_JXS_BURN_SETENV=""; echo "    [jxs-burn] $(hostname): no CAMERA_BOX_JXS_TARGET in the camera-box unit -- the burn unit sends NDI only (issue 1387)";; *[!0123456789.:]* | *:*:* | :* | *:) CAMBOX_JXS_BURN_SETENV=""; echo "WARNING: [jxs-burn] $(hostname): CAMERA_BOX_JXS_TARGET=$_cbjxs_t is not IPv4:port -- the burn unit sends NO JPEG XS (issue 1387)" >&2;; *:*) CAMBOX_JXS_BURN_SETENV="--setenv=CAMERA_BOX_JXS_TARGET=$_cbjxs_t"; echo "    [jxs-burn] $(hostname): the burn unit sends JPEG XS to $_cbjxs_t (issue 1387)";; *) CAMBOX_JXS_BURN_SETENV=""; echo "WARNING: [jxs-burn] $(hostname): CAMERA_BOX_JXS_TARGET=$_cbjxs_t has no port -- the burn unit sends NO JPEG XS (issue 1387)" >&2;; esac;'
}

# _program_inputs_box_render BOX manifest|target -> the box's JPEG XS seed manifest (the facts'
# render, strih_jxs_seed_manifest_json) or its program target (`ip:port|none`, strih role), in a
# subshell. rc != 0 when the facts do not load.
_program_inputs_box_render() {
  local box="${1-}" what="${2-}" dir
  dir="$(_program_inputs_libdir)" || return 1
  (
    set +e
    . "${dir}/strih-box-facts.sh" || exit 4
    . "${dir}/strih-jxs-provision.sh" || exit 4
    strih_box_load "$box" >/dev/null || exit 4
    case "$what" in
      manifest) strih_jxs_seed_manifest_json || exit 4 ;;
      target) strih_jxs_program_target || exit 4 ;;
      sid) printf '%s\n' "$STRIH_JXS_PROGRAM_STREAM_ID" ;;
      *) exit 4 ;;
    esac
  )
}

# _program_inputs_verify_box HERE HOST BOX ROLE OUTDIR -> obs_jxs_inputs.py --verify against the
# FACTS' render on one box (+ the running program output on the strih), graded for the E2E: every
# FAIL row, an unreachable websocket and a camera the operator moved off its declared input (the
# (jxs-pick) NOTE) are blocking; PASS and the (jxs-place) NOTE are fine. rc 1 on any blocking row.
_program_inputs_verify_box() {
  local here="${1-}" host="${2-}" box="${3-}" role="${4-}" outdir="${5-}" man target sid out line level bad=0
  local -a args
  man="$(_program_inputs_box_render "$box" manifest)" || {
    echo "ERROR: [4i/8jxs] ${role} '${box}': the box facts do not render a JPEG XS manifest (issue 1387)" >&2
    return 1
  }
  if [ -z "$man" ]; then
    echo "ERROR: [4i/8jxs] ${role} '${box}' declares no JPEG XS input, yet the venue resolved to the JPEG XS path (issue 1387)" >&2
    return 1
  fi
  printf '%s\n' "$man" >"${outdir}/jxs-seed-${box}.json" || return 1
  args=(--verify --manifest "${outdir}/jxs-seed-${box}.json" --host "$host" --port "${PROGRAM_INPUTS_OBS_PORT:-4455}")
  if [ "$role" = strih ]; then
    target="$(_program_inputs_box_render "$box" target)" || target=""
    sid="$(_program_inputs_box_render "$box" sid)" || sid=""
    case "$target" in
      "" | none)
        echo "ERROR: [4i/8jxs] strih '${box}': no JPEG XS program target resolves from the venue facts (issue 1387)" >&2
        return 1
        ;;
    esac
    args+=(--output-target "$target" --output-stream-id "$sid")
  fi
  out="$(timeout "${PROGRAM_INPUTS_VERIFY_TIMEOUT_S:-60}" python3 "${here}/obs_jxs_inputs.py" "${args[@]}" 2>&1)" || {
    echo "ERROR: [4i/8jxs] ${role} '${box}' (${host}): obs_jxs_inputs.py --verify failed (rc $?): ${out}" >&2
    return 1
  }
  [ -n "$out" ] || { echo "ERROR: [4i/8jxs] ${role} '${box}': obs_jxs_inputs.py --verify printed nothing (issue 1387)" >&2; return 1; }
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    level="${line%% *}"
    case "$level" in
      PASS) echo "    [4i/8jxs] ${role}: ${line}" ;;
      NOTE)
        case "$line" in
          *"(jxs-place)"*) echo "    [4i/8jxs] ${role}: ${line}" ;;
          *"(jxs-pick)"*) echo "ERROR: [4i/8jxs] ${role}: ${line} -- the E2E measures each camera on its declared input; rig-mode.sh test puts the mapping back (issue 1387)" >&2; bad=1 ;;
          *) echo "ERROR: [4i/8jxs] ${role}: ${line} -- the E2E needs the box's OBS answering (issue 1387)" >&2; bad=1 ;;
        esac
        ;;
      FAIL) echo "ERROR: [4i/8jxs] ${role}: ${line}" >&2; bad=1 ;;
      *) echo "ERROR: [4i/8jxs] ${role}: unexpected verifier output: ${line}" >&2; bad=1 ;;
    esac
  done <<<"$out"
  [ "$bad" = 0 ]
}

# program_inputs_measure_verdict JSON -> `COMMON|SPREAD|UNKNOWN <detail>` for a qr_align_pins.py
# --measure-only table: COMMON = every camera decoded the same painter source frame (spread_frames 0)
# over a converged tail, SPREAD = a converged tail with cameras frames apart, UNKNOWN = an error /
# no table / a tail that never converged. Pure. Always rc 0.
program_inputs_measure_verdict() {
  printf '%s' "${1-}" | python3 -c '
import json, sys
try:
    t = json.loads(sys.stdin.read() or "null")
except ValueError:
    t = None
if not isinstance(t, dict):
    print("UNKNOWN no measure-only table")
    sys.exit(0)
if t.get("error"):
    print("UNKNOWN " + str(t["error"]))
    sys.exit(0)
spread = t.get("spread_frames")
conv = t.get("converged")
rounds = t.get("rounds_used")
lags = ", ".join("%s=%s" % (k, v) for k, v in sorted(t.items())
                 if k not in ("spread_frames", "rounds_used", "converged", "error")) or "-"
detail = "spread_frames=%s converged=%s rounds=%s lags: %s" % (spread, conv, rounds, lags)
if not isinstance(spread, int) or isinstance(spread, bool) or conv is not True:
    print("UNKNOWN " + detail)
elif spread == 0:
    print("COMMON " + detail)
else:
    print("SPREAD " + detail)
' 2>/dev/null || printf 'UNKNOWN the verdict parse failed\n'
  return 0
}

# program_inputs_jxs_preflight HERE STRIH STREAM OUTDIR RUN_ID -> the BLOCKING [4i/8jxs] step on the
# JPEG XS path (a no-op line on NDI, where the NDI pin steps run instead). (a) BLOCKING: every declared
# JPEG XS input on strih + stream present with the declared settings, each camera on its declared
# camera, and the strih's program output running to the venue's stream box (obs_jxs_inputs.py
# --verify against the facts). (b) REPORT-ONLY: qr_align_pins.py --measure-only over the measured
# cameras' JPEG XS inputs (ALL_CAMBOX runs, 2+ cameras), the table saved and graded for one common
# painter source frame -- report-only until rig data shows the measure reads the 30 fps JPEG XS grid
# cleanly (the measure counts 60 fps painter frames; a 30 fps input steps two at a time). rc 1 on (a).
program_inputs_jxs_preflight() {
  local here="${1-}" strih="${2-}" stream="${3-}" outdir="${4-}" run_id="${5-}" bad=0 srcs n json verdict
  if program_path_ndi; then
    echo "[4i/8jxs] issue 1387 JPEG XS input check -- SKIPPED (the NDI path: the NDI pin steps run)"
    return 0
  fi
  echo "[4i/8jxs] issue 1387 the JPEG XS path: (a) the declared inputs + the program output vs the box facts (BLOCKING), (b) one painter frame on every camera (REPORT-ONLY)"
  _program_inputs_verify_box "$here" "$strih" "${PROGRAM_INPUTS_STRIH_BOX:-}" strih "$outdir" || bad=1
  _program_inputs_verify_box "$here" "$stream" "${PROGRAM_INPUTS_STREAM_BOX:-}" stream "$outdir" || bad=1
  if [ "$bad" != 0 ]; then
    echo "ERROR: [4i/8jxs] FAIL: the JPEG XS path is not what the box facts declare (above) -- the run is ABORTED before StartRecord (issue 1387)" >&2
    return 1
  fi
  if [ "${ALL_CAMBOX:-0}" != 1 ]; then
    echo "    [4i/8jxs] (b) REPORT-ONLY one-frame measure -- SKIPPED (single-camera run)"
    return 0
  fi
  srcs="$(camera_active_ndi_sources_excluding_csv "${PREFLIGHT_EXCLUDED_CAMS:-}")"
  n="$(printf '%s' "$srcs" | tr ',' '\n' | grep -c .)" || n=0
  if [ "$n" -lt 2 ]; then
    echo "    [4i/8jxs] (b) REPORT-ONLY one-frame measure -- SKIPPED (${n} camera(s): '${srcs}')"
    return 0
  fi
  json="$(timeout "${PROGRAM_INPUTS_MEASURE_TIMEOUT_S:-300}" python3 "${here}/qr_align_pins.py" --measure-only \
    --host "$strih" --password "${OBS_PASSWORD:-}" --sources "$srcs" 2>/dev/null || true)"
  printf '%s\n' "$json" >"${outdir}/jxs-measure-${run_id}.json" 2>/dev/null || true
  verdict="$(program_inputs_measure_verdict "$json")"
  echo "    [4i/8jxs] (b) REPORT-ONLY one painter frame on every camera (${srcs}): ${verdict} -- not graded until rig data shows the 30 fps JPEG XS grid measures cleanly (issue 1387)"
  return 0
}

# _program_inputs_sweep_cams SWEEP -> `<scene><TAB><cam>` per `<scene>:<LABEL>` pair (the
# switch_schedule.parse_sweep grammar: scene names hold spaces, a label holds no ':'), cam = the
# lower-cased label. Malformed text yields nothing for that part. Pure.
_program_inputs_sweep_cams() {
  local tok head scene="" label
  local -a toks=()
  read -ra toks <<<"${1-}" || true
  for tok in "${toks[@]}"; do
    case "$tok" in
      *:*)
        head="${tok%:*}"
        [ -z "$head" ] || scene="${scene:+${scene} }${head}"
        label="$(printf '%s' "${tok##*:}" | tr 'ABCDEFGHIJKLMNOPQRSTUVWXYZ' 'abcdefghijklmnopqrstuvwxyz')"
        printf '%s\t%s\n' "$scene" "$label"
        scene=""
        ;;
      *) scene="${scene:+${scene} }${tok}" ;;
    esac
  done
  return 0
}

# _program_inputs_rendered_one HERE HOST SCENE EXPECTED LABEL -> 0 when SCENE on HOST renders
# EXPECTED (obs_phase2.py program-rendered-input, descending a nested scene); rc 1 + a named ERROR.
_program_inputs_rendered_one() {
  local here="${1-}" host="${2-}" scene="${3-}" want="${4-}" label="${5-}" got
  got="$(timeout "${PROGRAM_INPUTS_RENDERED_TIMEOUT_S:-30}" python3 "${here}/obs_phase2.py" program-rendered-input \
    --host "$host" --password "${OBS_PASSWORD:-}" --scene "$scene" 2>/dev/null || true)"
  got="$(printf '%s' "$got" | tail -n 1)"
  if [ -n "$got" ] && [ "$got" = "$want" ]; then
    echo "    [rendered-inputs] ${label} scene '${scene}' renders '${got}' (issue 1387)"
    return 0
  fi
  echo "ERROR: [rendered-inputs] ${label} scene '${scene}' renders '${got:-<unreadable>}', not the measured input '${want}' -- the recording would not carry this input's burn (issue 1387)" >&2
  return 1
}

# program_inputs_rendered_verify HERE STRIH STREAM SWEEP -> the BLOCKING check before StartRecord on
# the JPEG XS path (the issue-1204 pattern): the strih program scene renders STRIH_PROG_SOURCE, every
# ALL_CAMBOX sweep scene renders its camera's measured input (camera_strih_input), and the stream
# program scene renders STREAM_PROG_SOURCE. A no-op line on the NDI path. rc 1 when any differs.
program_inputs_rendered_verify() {
  local here="${1-}" strih="${2-}" stream="${3-}" sweep="${4-}" bad=0 scene cam
  if program_path_ndi; then
    echo "[rendered-inputs] issue 1387 rendered-input check -- SKIPPED (the NDI path)"
    return 0
  fi
  echo "[rendered-inputs] issue 1387 every measured scene renders its JPEG XS input (BLOCKING, before StartRecord)"
  _program_inputs_rendered_one "$here" "$strih" "${STRIH_PROG_SCENE:-}" "${STRIH_PROG_SOURCE:-}" strih || bad=1
  if [ "${ALL_CAMBOX:-0}" = 1 ]; then
    while IFS=$'\t' read -r scene cam; do
      [ -n "$scene" ] && [ -n "$cam" ] || continue
      [ "$scene" != "${STRIH_PROG_SCENE:-}" ] || continue
      _program_inputs_rendered_one "$here" "$strih" "$scene" "$(camera_strih_input "$cam")" strih || bad=1
    done < <(_program_inputs_sweep_cams "$sweep")
  fi
  _program_inputs_rendered_one "$here" "$stream" "${STREAM_PROG_SCENE:-}" "${STREAM_PROG_SOURCE:-}" stream || bad=1
  if [ "$bad" != 0 ]; then
    echo "ERROR: [rendered-inputs] FAIL: a measured scene does not render its input (above) -- the run is ABORTED before StartRecord (issue 1387)" >&2
    return 1
  fi
  return 0
}

# program_inputs_av_pin_hold -> the #856 A/V apply on the JPEG XS path: the stream program pin is the
# box FACT STRIH_JXS_PROGRAM_PIN_MS (the launch seeder heals any WS write back, and a latency never
# changes at run time), so the computed correction is a RECOMMENDATION: it prints
# STRIH_JXS_PROGRAM_PIN_MS=<new> (av_sync_calibrate.required_delay_ms from the fact pin, the same rule
# the NDI apply uses), saves it to $OUTDIR, and clears AV_SYNC_APPLY_OFFSET_MS with a HOLD reason (the
# issue-1265 HOLD shape). Run in the caller's shell (it sets AV_SYNC_*). A no-op on NDI or with no
# correction. Always rc 0.
program_inputs_av_pin_hold() {
  local off="${AV_SYNC_APPLY_OFFSET_MS:-}" pin="${PROGRAM_INPUTS_STREAM_PIN_MS:-}" rec here
  program_path_ndi && return 0
  [ -n "$off" ] || return 0
  here="$(_program_inputs_libdir)/.."
  rec="$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import av_sync_calibrate as a; print(a.required_delay_ms(int(sys.argv[2]), float(sys.argv[3])))' \
    "$here" "$pin" "$off" 2>/dev/null || true)"
  case "$rec" in '' | *[!0123456789]*) rec="" ;; esac
  AV_SYNC_HELD_REASON="the JPEG XS path: the stream pin is the box fact STRIH_JXS_PROGRAM_PIN_MS=${pin:-?} (never changed at run time); recommended STRIH_JXS_PROGRAM_PIN_MS=${rec:-<not computable>} for a measured offset ${off} ms"
  echo "[cleanup] HOLD the A/V apply: ${AV_SYNC_HELD_REASON} -- RECOMMENDATION (a fact change, merged + set up on ${PROGRAM_INPUTS_STREAM_BOX:-the stream box} while it is off air): STRIH_JXS_PROGRAM_PIN_MS=${rec:-<not computable>} (issue 1387)" >&2
  if [ -n "${OUTDIR:-}" ]; then
    printf 'STRIH_JXS_PROGRAM_PIN_MS=%s\n# %s\n' "${rec:-}" "$AV_SYNC_HELD_REASON" \
      >"${OUTDIR}/av-sync-jxs-pin-recommendation-${RUN_ID:-run}.txt" 2>/dev/null || true
  fi
  AV_SYNC_APPLY_OFFSET_MS=""
  return 0
}
