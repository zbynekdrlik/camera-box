#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh,
# verify-strih.sh and the Tier-0 harnesses), and strict mode here would leak into all of them.
#
# scripts/lib/strih-drm-output.sh -- issue 1346 (owner 24.9.2026): the strih-snv HDMI output is the
# SAME fixed hardware output imag has -- the in-OBS DRM-lease output (issue 1152, the vendored
# libobs module; .claude/rules/obs-drm-output.md) -- selectable between Program and the BUILT-IN
# Multiview. It is never an OBS projector window and never the desktop.
#
# The module's activation contract is ONE file, ~/.camera-box/drm-output.json of the OBS user, a
# single machine-written line: {"enabled":true,"connector":"<X RandR name>","argb":2105376,
# "view":"multiview"}. The connector is the X RandR OUTPUT name (xrandr prints HDMI-0 on the
# NVIDIA-primary strih-snv, HDMI-1 under modesetting) -- never the kernel connector name HDMI-A-1.
# The operator switches the view in OBS (Tools menu) and the choice is written back into the file;
# `strih_scenes.py --projector program|multiview` is the scripted twin.
#
# Pure helpers (unit-tested by tests/strih_drm_output_provision_1346.rs):
#   strih_drm_hdmi_connected [SYSFS_DIR] [BACKEND] [XRANDR_TEXT]
#                                           0 iff an HDMI monitor is connected: the kernel status
#                                           (lease), the X RandR view (vk-direct)
#   strih_drm_hdmi_output_from_xrandr       stdin `xrandr --query` -> the first connected HDMI name
#   strih_drm_xrandr_query USER_HOME USER   (gatherer) `xrandr --query` of display :0, or nothing
#   strih_drm_output_config_json CONN [VIEW] [BACKEND] the one-line config (refuses a bad name/view/backend)
#   strih_drm_legacy_view TEXT              the retired strih-lx-projector.json type -> initial view
#   strih_drm_output_verdict ...            verify-strih's drm-output item verdict
#   strih_drm_vk_present_dead               stdin = an OBS log -> 0 iff the vk-direct present loop died
#
# Provisioning ACTION (root, setup-strih.sh step 6; uses the caller's warn/fail):
#   strih_drm_output_provision USER_HOME DESKTOP_USER SCRIPTS_DIR BACKEND [OUTPUTS]
#     (issue 1424: also writes the WirePlumber rule that leaves the HDMI/DP PCMs to OBS's audio leg;
#     part 2: OUTPUTS = the fact STRIH_HDMI_OUTPUTS -- a connector list writes / migrates the "outputs"
#     array, the operator's per-connector views kept)
# issue 1424 part 2 (the array form; the fact grammar strih_box_hdmi_outputs_ok and the fact accessor
# strih_lx_hdmi_outputs come from scripts/lib/strih-box-facts.sh, which strih-provision.sh sources):
# strih_drm_xrandr_connected,
#   strih_drm_outputs_config_json, strih_drm_output_log_of, strih_drm_outputs_graded_here and the verify
#   grader strih_drm_outputs_verify_report (item 4c grades EVERY output by its own log lines)

# issue 1424: the output's audio leg -- the WirePlumber rule that leaves every HDMI/DP PCM to OBS
# (written at the end of strih_drm_output_provision) and its verify grading.
# shellcheck source=scripts/lib/drm-output-audio.sh
. "$(dirname "${BASH_SOURCE[0]}")/drm-output-audio.sh"
# issue 1424 (design 6386467303 layer 1): the X config that keeps the fixed HDMI outputs out of X.
if ! declare -F strih_x_display_provision >/dev/null; then
  # shellcheck source=scripts/lib/strih-x-display.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-x-display.sh"
fi

# The M1 dark-grey solid (0x202020): the image before the first rendered frame + the fail-open one.
STRIH_DRM_OUTPUT_ARGB=2105376

# strih_drm_hdmi_connected [SYSFS_DIR] [BACKEND] [XRANDR_QUERY_TEXT] -> 0 iff an HDMI monitor is
# plugged in, read the way the box's HDMI output backend (the fact STRIH_HDMI_OUTPUT_BACKEND) needs:
#   lease (the default, also for an absent BACKEND): some <dir>/card*-HDMI*/status reads `connected`.
#     The KERNEL status is the truth there: after a lease X RandR can stick at `disconnected` while
#     the kernel stays `connected` (obs-drm-output.md M1 runbook gotcha). The xrandr text is ignored.
#   vk-direct: the X RandR view (XRANDR_QUERY_TEXT = `xrandr --query`, from strih_drm_xrandr_query)
#     names a connected HDMI output. The NVIDIA X driver does not drive the KMS connector status, so
#     sysfs reads `disconnected` with the monitor plugged in (main design 5840508308). A connected
#     output with NO mode/CRTC counts too -- that is how xrandr lists HDMI-0 while vk-direct holds it.
#     An empty text (X not up) is never connected. Any connected `HDMI*` output counts: on an
#     NVIDIA-primary box the NVIDIA outputs (HDMI-0) are the ones X lists first, and the connector the
#     config arms comes from the same first match (strih_drm_hdmi_output_from_xrandr).
strih_drm_hdmi_connected() {
  local dir="${1:-/sys/class/drm}" backend="${2:-lease}" xr="${3:-}" st
  if [ "$backend" = vk-direct ]; then
    [ -n "$(printf '%s\n' "$xr" | strih_drm_hdmi_output_from_xrandr)" ]
    return
  fi
  for st in "$dir"/card*-HDMI*/status; do
    [ -f "$st" ] || continue
    if [ "$(cat "$st" 2>/dev/null || true)" = connected ]; then
      return 0
    fi
  done
  return 1
}

# strih_drm_hdmi_output_from_xrandr (stdin = `xrandr --query`) -> print the FIRST connected HDMI
# output's X RandR name (nothing when none). Always returns 0 and reads ALL of stdin (no early
# exit, so an upstream writer never takes a SIGPIPE under the caller's pipefail).
strih_drm_hdmi_output_from_xrandr() {
  awk '$1 ~ /^HDMI/ && $2 == "connected" && !done { print $1; done = 1 }' || true
}

# strih_drm_xrandr_query USER_HOME DESKTOP_USER -> print `xrandr --query` of the desktop user's X
# session :0 (with that user's Xauthority), or nothing when X does not answer. As root it runs as the
# desktop user (the setup-strih step-6 shape); otherwise inline (verify-strih run by the operator).
# Bounded (a wedged X never hangs setup/verify) and always returns 0, so a set -e caller survives.
strih_drm_xrandr_query() {
  local user_home="$1" desktop_user="$2"
  if [ "$(id -u)" = 0 ]; then
    sudo -u "$desktop_user" env DISPLAY=:0 XAUTHORITY="${user_home}/.Xauthority" timeout 10 xrandr --query 2>/dev/null || true
  else
    env DISPLAY=:0 XAUTHORITY="${user_home}/.Xauthority" timeout 10 xrandr --query 2>/dev/null || true
  fi
}

# strih_drm_output_config_json CONNECTOR [VIEW] [BACKEND] -> the one-line config the C module and the
# Python classifier both arm on. VIEW defaults to multiview (owner ROZHODNUTE). BACKEND (issue 1346)
# defaults to lease = NO "backend" key, so a lease config stays byte-identical to the pre-backend one;
# vk-direct (the NVIDIA Vulkan direct-display backend) is written explicitly. Refuses (prints nothing,
# returns 1) an empty / non-[A-Za-z0-9._-] connector, an unknown view or an unknown backend -- a config
# the C would ignore or keep dormant must never be written.
strih_drm_output_config_json() {
  local conn="${1:-}" view="${2:-multiview}" backend="${3:-lease}" extra=""
  case "$conn" in
    '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  case "$view" in
    program | multiview) ;;
    *) return 1 ;;
  esac
  case "$backend" in
    lease) ;;
    vk-direct) extra=',"backend":"vk-direct"' ;;
    *) return 1 ;;
  esac
  printf '{"enabled":true,"connector":"%s","argb":%s,"view":"%s"%s}\n' "$conn" "$STRIH_DRM_OUTPUT_ARGB" "$view" "$extra"
}

# strih_drm_legacy_view TEXT -> the initial view carried over from the retired
# /opt/camera-box/strih-lx-projector.json ({"type":"program"} -> program; anything else ->
# multiview, the owner's default). Always returns 0.
strih_drm_legacy_view() {
  local text="${1:-}" re='"type"[[:space:]]*:[[:space:]]*"program"'
  if [[ $text =~ $re ]]; then
    printf 'program'
  else
    printf 'multiview'
  fi
  return 0
}

# strih_drm_output_verdict HDMI_CONNECTED ARMED_CONNECTOR VIEW LIVE_SCANOUT LIVE_MULTIVIEW [BACKEND FACT [DEAD]]
#   HDMI_CONNECTED   1 iff strih_drm_hdmi_connected; "?" = UNKNOWN (a vk-direct box whose X RandR
#                    view could not be read -- never a measured "no HDMI")
#   ARMED_CONNECTOR  the connector the config arms ("-" / empty = dormant: absent, disabled, bad;
#                    "?" = the classifier itself could not run, e.g. the strih_scenes import failed)
#   VIEW             program | multiview | unknown (the config's "view" token)
#   LIVE_SCANOUT     1 iff the newest OBS log has `drm-output: program scanout LIVE`
#   LIVE_MULTIVIEW   1 iff the newest OBS log has `drm-output: multiview bind LIVE`
#   BACKEND          issue 1346: the config's backend token (lease | vk-direct | unknown; "?" or empty
#                    = not read -- the check is skipped)
#   FACT             the box fact STRIH_HDMI_OUTPUT_BACKEND ("?" or empty = not read -- skipped)
#   DEAD             1 iff strih_drm_vk_present_dead on the newest OBS log (the vk present loop died)
# Prints ONE token; return code 0 = PASS, 2 = NOTE (skip / report), 1 = FAIL:
#   x-unreadable        (1) HDMI_CONNECTED "?": the X view a vk-direct box needs was not readable
#   skip-no-hdmi        (2) no HDMI monitor and no armed config -- today's eDP-only strih-snv
#   hdmi-unplugged      (2) the config is armed but no HDMI monitor is plugged in
#   classify-failed     (1) an HDMI monitor is plugged in but the config could not be classified
#   config-missing      (1) an HDMI monitor is plugged in but no armed config -> setup-strih step 6
#   view-invalid        (1) the "view" value is neither program nor multiview (the C runs Program)
#   backend-invalid     (1) the "backend" value is neither lease nor vk-direct (the C stays dormant)
#   backend-drift       (1) the config's backend is not the box fact (setup-strih step 6 re-writes it)
#   present-dead        (1) the vk-direct present loop died after going live (the HDMI is dead)
#   lease-not-live      (1) armed + plugged, but the OBS log never reached the scanout
#   multiview-not-live  (1) view multiview, scanout live, but the built-in Multiview never bound
#   ok                  (0)
strih_drm_output_verdict() {
  local hdmi="${1:-0}" conn="${2:-}" view="${3:-program}" live="${4:-0}" mv="${5:-0}"
  local backend="${6:-?}" fact="${7:-?}" dead="${8:-0}"
  [ "$conn" = "-" ] && conn=""
  if [ "$hdmi" = "?" ]; then
    printf 'x-unreadable'
    return 1
  fi
  if [ "$hdmi" != 1 ]; then
    if [ -n "$conn" ] && [ "$conn" != "?" ]; then
      printf 'hdmi-unplugged'
    else
      printf 'skip-no-hdmi'
    fi
    return 2
  fi
  if [ "$conn" = "?" ]; then
    printf 'classify-failed'
    return 1
  fi
  if [ -z "$conn" ]; then
    printf 'config-missing'
    return 1
  fi
  case "$view" in
    program | multiview) ;;
    *)
      printf 'view-invalid'
      return 1
      ;;
  esac
  if [ "$backend" = unknown ]; then
    printf 'backend-invalid'
    return 1
  fi
  if [ -n "$backend" ] && [ "$backend" != "?" ] && [ -n "$fact" ] && [ "$fact" != "?" ] && [ "$backend" != "$fact" ]; then
    printf 'backend-drift'
    return 1
  fi
  if [ "$dead" = 1 ]; then
    printf 'present-dead'
    return 1
  fi
  if [ "$live" != 1 ]; then
    printf 'lease-not-live'
    return 1
  fi
  if [ "$view" = multiview ] && [ "$mv" != 1 ]; then
    printf 'multiview-not-live'
    return 1
  fi
  printf 'ok'
  return 0
}

# strih_drm_vk_present_dead (stdin = the newest OBS log) -> 0 iff the vk-direct present loop EXITED
# (`drm-output: vk-direct present loop exited`) with no later `drm-output: stopped (vk-direct` -- the loop
# died on its own (a display the bounded rebuild could not bring back, a lost device) while OBS kept
# running, so the HDMI is dead although the log once reached `program scanout LIVE` (review round 1).
# A clean stop (halt -> exit line -> stopped line) and a log without the loop are not dead. Reads all of
# stdin (no early exit); always returns 0/1, never aborts a set -e caller.
# issue 1424 (design 6386467303 layer 3): the retry supervisor RELEASES a dead output with a regular stop
# right after its `retry: '<c>' lost its present loop` line, so that stop does not clear the death; only
# `retry: '<c>' LIVE again` does.
strih_drm_vk_present_dead() {
  local verdict
  verdict="$(LC_ALL=C awk '
    index($0, "drm-output: vk-direct present loop exited") { dead = 1 }
    index($0, "drm-output: retry: ") && index($0, " lost its present loop") { dead = 1; lost = 1 }
    index($0, "drm-output: stopped (vk-direct") && !lost { dead = 0 }
    index($0, "drm-output: retry: ") && index($0, " LIVE again") { dead = 0; lost = 0 }
    END { print dead ? "dead" : "alive" }' || true)"
  [ "$verdict" = dead ]
}

# strih_drm_output_retry_state (stdin = one output's drm-output lines) -> `live` / `dormant` from the LAST
# retry supervisor line (issue 1424: `retry: '<c>' LIVE again` / `... dormant -- ...` / `... lost its
# present loop`), nothing when the log has none. Reads all of stdin; always rc 0.
strih_drm_output_retry_state() {
  LC_ALL=C awk '
    index($0, "drm-output: retry: ") && index($0, " LIVE again") { s = "live" }
    index($0, "drm-output: retry: ") && (index($0, " dormant -- ") || index($0, " lost its present loop")) { s = "dormant" }
    END { if (s != "") print s }' || true
}

# ---- issue 1424 part 2 (owner 13.10.2026): up to two fixed outputs at once ---------------------------
# strih-pp drives HDMI-0 = the cutter's Multiview AND HDMI-1 = the vestibule Program + its audio. The box
# fact STRIH_HDMI_OUTPUTS lists them (`HDMI-0:multiview HDMI-1:program`; `auto` = the one-output
# issue-1346 behaviour above, `none` = no fixed output) and drm-output.json carries them as an
# "outputs" array (obs-drm-output-outputs.c; all or nothing -- a malformed array starts NO output).

# strih_drm_xrandr_connected CONNECTOR (stdin = `xrandr --query`) -> 0 iff X RandR lists CONNECTOR as
# connected (with or without a mode: vk-direct holds it with none). Reads all of stdin.
strih_drm_xrandr_connected() {
  local want="${1:-}" got
  got="$(awk -v w="$want" '$1 == w && $2 == "connected" { f = 1 } END { print f ? "yes" : "no" }' || true)"
  [ -n "$want" ] && [ "$got" = yes ]
}

# strih_drm_outputs_config_json SPEC [BACKEND] -> the one-line array config for a FRESH box, the entries
# in the machine-written key order (connector, argb, view, backend; lease = the ABSENT backend key, as
# in the single-object form) -- byte-identical to what strih_scenes.write_drm_outputs writes. Refuses
# (prints nothing, returns 1) a malformed SPEC or an unknown backend.
strih_drm_outputs_config_json() {
  local spec="${1:-}" backend="${2:-lease}" extra="" t out="" sep=""
  strih_box_hdmi_outputs_ok "$spec" || return 1
  case "$backend" in
    lease) ;;
    vk-direct) extra=',"backend":"vk-direct"' ;;
    *) return 1 ;;
  esac
  local rest audio
  for t in $spec; do
    rest="${t#*:}"
    audio=""
    [ "${rest#*:}" = noaudio ] && audio=',"audio":false'   # issue 1424 part 3: its PCM is owned elsewhere
    out="${out}${sep}$(printf '{"connector":"%s","argb":%s,"view":"%s"%s%s}' "${t%%:*}" "$STRIH_DRM_OUTPUT_ARGB" "${rest%%:*}" "$extra" "$audio")"
    sep=","
  done
  printf '{"enabled":true,"outputs":[%s]}\n' "$out"
}

# strih_drm_outputs_spec_audio SPEC CONNECTOR -> `off` when SPEC lists CONNECTOR with `:noaudio`, else `on`
# (issue 1424 part 3: the fact names who owns the connector's PCM).
strih_drm_outputs_spec_audio() {
  local t
  for t in ${1-}; do
    [ "${t%%:*}" = "${2-}" ] && [ "${t##*:}" = noaudio ] && { printf off; return 0; }
  done
  printf on
}

# strih_drm_outputs_spec_connectors SPEC -> the connectors of SPEC, space-separated, sorted (for the
# fact-vs-config comparison).
strih_drm_outputs_spec_connectors() {
  local t
  for t in ${1-}; do printf '%s\n' "${t%%:*}"; done | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'
}

# strih_drm_output_log_of CONNECTOR (stdin = an OBS log) -> only the `drm-output` lines that name
# 'CONNECTOR' (quoted, so HDMI-0 never matches HDMI-01): one output's own lines.
strih_drm_output_log_of() {
  LC_ALL=C awk -v q="'${1:-}'" 'index($0, "drm-output") && index($0, q)' || true
}

# strih_drm_outputs_verify_lines SCN_DIR CONF -> the strih_scenes reading of an "outputs" config, for
# verify: the first line `<verdict>`, then one `<connector> <view> <backend> <on|off>` line per output
# (only on ok; the last field = its "audio"). Prints `?` when the classifier could not run. Always returns 0.
strih_drm_outputs_verify_lines() {
  python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import strih_scenes as s
v, o = s.drm_output_instances(s.drm_output_config_text(sys.argv[2]))
print(v)
for i in o:
    print(i["connector"], i["view"], i["backend"], "on" if i.get("audio", True) else "off")' "$1" "$2" 2>/dev/null || echo "?"
}

# strih_drm_outputs_is_array CONF -> 0 iff CONF is a JSON object with an "outputs" key (the issue-1424
# array form). 1 otherwise (absent, unreadable, the single-object form, no python3).
strih_drm_outputs_is_array() {
  command -v python3 >/dev/null 2>&1 || return 1
  python3 -c 'import json, sys
try:
    c = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    sys.exit(1)
sys.exit(0 if isinstance(c, dict) and "outputs" in c else 1)' "${1:-}" 2>/dev/null
}

# strih_drm_outputs_graded_here CONF -> 0 iff verify item 4c grades CONF per output (the call
# below): CONF is the "outputs" array form, or it exists while the box fact STRIH_HDMI_OUTPUTS is a
# connector list (a config setup-strih step 6 has not brought onto the fact yet). 1 = the
# single-object grading of item 4c applies (today's one-output boxes, an absent config).
strih_drm_outputs_graded_here() {
  strih_drm_outputs_is_array "${1:-}" && return 0
  [ -f "${1:-}" ] && strih_box_hdmi_outputs_ok "$(strih_lx_hdmi_outputs 2>/dev/null || true)"
}

# strih_drm_outputs_verify_dispatch VERDICT USER_HOME SCN_BIN CONF LOG BACKEND_FACT XRANDR_TEXT -- verify
# item 4c's per-output arm: when the single-output VERDICT is not skip-no-output and CONF is graded per
# output (strih_drm_outputs_graded_here), grade it (strih_drm_outputs_verify_report) and return 0 (the
# caller then skips its single-output grading); 1 = the single-output grading applies.
strih_drm_outputs_verify_dispatch() {
  [ "${1:-}" != skip-no-output ] && strih_drm_outputs_graded_here "${4:-}" || return 1
  shift
  strih_drm_outputs_verify_report "$@"
  return 0
}

# strih_drm_outputs_verify_report USER_HOME SCN_BIN CONF LOG BACKEND_FACT XRANDR_TEXT -- verify-strih
# item 4c for the "outputs" array form: the array must be valid, its connectors must be the box fact's
# STRIH_HDMI_OUTPUTS list (when the fact is a list), and EVERY output is graded on its own -- connected
# (vk-direct: X RandR names THAT connector connected; lease: a kernel HDMI connector), its own
# `program scanout LIVE` line, `multiview bind LIVE` for a Multiview, a present loop that did not die,
# and its own audio leg line. Uses the caller's ok/bad/note. Always returns 0.
strih_drm_outputs_verify_report() {
  local user_home="$1" scn_bin="$2" conf="$3" log="$4" fact="$5" xr="${6:-}" lines verdict
  local conn view backend hdmi live mv dead v want got spec olog rs
  spec="$(strih_lx_hdmi_outputs 2>/dev/null || true)"
  if ! strih_drm_outputs_is_array "$conf"; then
    bad "HDMI outputs: ${conf} drives a single output but the box fact STRIH_HDMI_OUTPUTS lists '${spec}' -- re-run setup-strih.sh step 6"
    return 0
  fi
  lines="$(strih_drm_outputs_verify_lines "$(dirname "$scn_bin")" "$conf")"
  verdict="${lines%%$'\n'*}"
  if [ "$verdict" = "?" ]; then
    bad "HDMI outputs: could not classify ${conf} (strih_scenes.py / python3 missing or its import failed) -- re-run setup-strih.sh step 6"
    return 0
  fi
  if [ "$verdict" != ok ]; then
    bad "HDMI outputs: ${conf} \"outputs\" is malformed (${verdict}) -- OBS starts NO output (all or none); re-run setup-strih.sh step 6"
    return 0
  fi
  if strih_box_hdmi_outputs_ok "$spec"; then
    want="$(strih_drm_outputs_spec_connectors "$spec")"
    got="$(printf '%s\n' "$lines" | sed 1d | awk 'NF { print $1 }' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
    if [ "$want" != "$got" ]; then
      bad "HDMI outputs: ${conf} drives '${got}' but the box fact STRIH_HDMI_OUTPUTS lists '${want}' -- re-run setup-strih.sh step 6"
    fi
  fi
  while read -r conn view backend audio; do
    [ -n "$conn" ] || continue
    hdmi=0
    if [ "$fact" = vk-direct ] && [ -z "$xr" ]; then
      hdmi="?"
    elif [ "$fact" = vk-direct ]; then
      printf '%s\n' "$xr" | strih_drm_xrandr_connected "$conn" && hdmi=1
    elif strih_drm_hdmi_connected /sys/class/drm "$fact" ""; then
      # lease: ANY kernel HDMI connector (known limit: X RandR sticks at disconnected after a lease and its
      # names differ from the kernel's, HDMI-1 vs HDMI-A-1 -- no per-connector signal; every strih is vk-direct)
      hdmi=1
    fi
    olog=""
    [ -n "$log" ] && [ -f "$log" ] && olog="$(strih_drm_output_log_of "$conn" < "$log")"
    live=0; mv=0; dead=0
    grep -aqF 'drm-output: program scanout LIVE' <<<"$olog" && live=1
    grep -aqF 'drm-output: multiview bind LIVE' <<<"$olog" && mv=1
    strih_drm_vk_present_dead <<<"$olog" && dead=1
    # issue 1424: the retry supervisor's last line wins over an old LIVE line
    rs="$(strih_drm_output_retry_state <<<"$olog")"
    [ "$rs" = dormant ] && live=0
    v="$(strih_drm_output_verdict "$hdmi" "$conn" "$view" "$live" "$mv" "$backend" "$fact" "$dead" || true)"
    if [ "$rs" = dormant ] && { [ "$v" = present-dead ] || [ "$v" = lease-not-live ]; }; then
      bad "HDMI output ${conn}: dark -- connected in X but its output is dormant and retried by OBS itself (no restart needed once the display answers); last: $(grep -aF 'drm-output: retry: ' <<<"$olog" | tail -n 1 | cut -c1-240)"
      continue
    fi
    case "$v" in
      ok)                 ok   "HDMI output ${conn}: live (backend ${backend}), view ${view} (${conf} + its own lines in the newest OBS log)"
                          strih_drm_outputs_audio_grade "$user_home" "$log" "$olog" "$conn" "$view" "${audio:-on}" "$spec" ;;
      x-unreadable)       bad  "HDMI output ${conn}: the box fact is vk-direct, but xrandr on :0 answered nothing -- cannot grade it" ;;
      skip-no-hdmi | hdmi-unplugged) note "HDMI output ${conn}: armed in ${conf} but X RandR does not list it connected (report-only)" ;;
      backend-drift)      bad  "HDMI output ${conn}: uses backend ${backend} but the box fact STRIH_HDMI_OUTPUT_BACKEND is ${fact} -- re-run setup-strih.sh step 6" ;;
      present-dead)       bad  "HDMI output ${conn}: its vk-direct present loop died in the newest OBS log -- read its drm-output: lines, then restart strih-obs.service" ;;
      lease-not-live)     bad  "HDMI output ${conn}: armed but the newest OBS log has no 'drm-output: program scanout LIVE' line naming it -- read its drm-output: lines" ;;
      multiview-not-live) bad  "HDMI output ${conn}: the view is multiview but the newest OBS log has no 'drm-output: multiview bind LIVE' line naming it" ;;
      *)                  bad  "HDMI output ${conn}: verdict '${v}'" ;;
    esac
  done < <(printf '%s\n' "$lines" | sed 1d)
  return 0
}

# strih_drm_outputs_audio_grade USER_HOME LOG OUTPUT_LOG CONNECTOR VIEW CONFIG_AUDIO SPEC -- one output's
# audio row (issue 1424 part 3). The config's "audio" must match the box fact (`:noaudio` = the PCM is
# owned elsewhere, e.g. by the intercom hub); an output whose audio is off must log `drm-output audio:
# '<conn>' off by config (owned elsewhere)` and nothing else of its leg; an output with audio is graded
# by drm_output_audio_verify_report on its own lines. Uses the caller's ok/bad/note.
strih_drm_outputs_audio_grade() {
  local user_home="$1" log="$2" olog="$3" conn="$4" view="$5" cfg="$6" spec="$7" want
  want=on
  strih_box_hdmi_outputs_ok "$spec" && want="$(strih_drm_outputs_spec_audio "$spec" "$conn")"
  if [ "$cfg" != "$want" ]; then
    bad "HDMI audio ${conn}: the config has audio ${cfg} but the box fact STRIH_HDMI_OUTPUTS says ${want} -- re-run setup-strih.sh step 6"
  elif [ "$cfg" = off ]; then
    if grep -aqF "drm-output audio: '${conn}' off by config (owned elsewhere)" <<<"$olog" \
      && ! grep -aqE "drm-output audio: (pcm hw:|'${conn}' matched)" <<<"$olog"; then
      ok "HDMI audio ${conn}: off by config -- its PCM is owned elsewhere, the module opened nothing"
    else
      bad "HDMI audio ${conn}: the config turns its audio off, but the newest OBS log has no 'off by config' line naming it (or the module opened its PCM) -- an OBS build without issue 1424 part 3?"
    fi
  else
    drm_output_audio_verify_report "$user_home" "${user_home##*/}" "$log" "" "$conn" "$view"
  fi
  return 0
}

# strih_drm_outputs_provision_fresh USER_HOME DESKTOP_USER BACKEND SPEC CONF -- step 6, no config yet, the
# fact lists the connectors (issue 1424 part 2): write the "outputs" array config when X RandR answers
# and names at least one listed connector connected; each listed connector X does not list connected is
# WARNed (its output stays dormant until it is plugged in -- the other one starts anyway). Otherwise a
# loud SKIP, like the one-output path. Uses the caller's warn.
strih_drm_outputs_provision_fresh() {
  local user_home="$1" desktop_user="$2" backend="$3" spec="$4" conf="$5" xr t any=0 line
  xr="$(strih_drm_xrandr_query "$user_home" "$desktop_user")"
  if [ -z "$xr" ]; then
    warn "  SKIP issue 1424: the HDMI outputs are detected through X RandR, but xrandr on :0 answered nothing (Xorg :0 not up yet?) -- ${conf} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
    return 0
  fi
  for t in $spec; do
    if printf '%s\n' "$xr" | strih_drm_xrandr_connected "${t%%:*}"; then
      any=1
    else
      warn "  issue 1424: ${t%%:*} is not connected in X RandR -- its output stays dormant until it is plugged in (OBS restart)"
    fi
  done
  if [ "$any" != 1 ]; then
    warn "  SKIP issue 1424: none of ${spec} is connected -- ${conf} NOT provisioned (the fixed HDMI outputs stay dormant); attach them and re-run setup-strih.sh"
    return 0
  fi
  line="$(strih_drm_outputs_config_json "$spec" "$backend")" || {
    warn "  SKIP issue 1424: STRIH_HDMI_OUTPUTS '${spec}' / backend '${backend}' is not writable -- ${conf} NOT provisioned"
    return 0
  }
  install -d -o "$desktop_user" -g "$desktop_user" "$(dirname "$conf")"
  printf '%s\n' "$line" | install -m 0644 -o "$desktop_user" -g "$desktop_user" /dev/stdin "$conf"
  echo "  wrote ${conf} (HDMI outputs ${spec}, backend ${backend}; takes effect at the next OBS start)"
}

# strih_drm_output_provision USER_HOME DESKTOP_USER SCRIPTS_DIR BACKEND [OUTPUTS] -- setup-strih.sh step 6 (root):
# the fixed HDMI output's activation contract, ~/.camera-box/drm-output.json of the OBS (desktop) user.
#   * the retired 19.9. projector config is removed; its "type" seeds the initial view (default multiview);
#   * BACKEND is the box fact STRIH_HDMI_OUTPUT_BACKEND (issue 1346): lease = the X RandR lease (an
#     Intel-driven connector), vk-direct = Vulkan direct display (an NVIDIA-driven connector -- the
#     NVIDIA X driver refuses the lease). vk-direct needs the Vulkan loader (installed here, a failed
#     install FAILS the step) and the NVIDIA driver's ICD (a WARN when missing);
#   * a symlinked ~/.camera-box or config is refused (root never writes through it);
#   * an EXISTING config is the operator's view choice and is kept -- only its backend is brought onto
#     the fact, as the desktop user (strih_scenes.write_drm_backend: every other key kept, one line);
#   * a fresh config is written ONLY when an HDMI monitor is connected (the kernel status for lease, the
#     X RandR view for vk-direct -- strih_drm_hdmi_connected) AND X RandR names it (`install -o <desktop
#     user>`, never a root redirect); otherwise a loud SKIP.
# Uses the caller's warn/fail (setup-strih.sh).
strih_drm_output_provision() {
  local user_home="$1" desktop_user="$2" scripts_dir="$3" backend="$4" outputs="${5:-}"
  # issue 1424 part 2: the box fact STRIH_HDMI_OUTPUTS (auto = today's one output on the first connected
  # HDMI; a CONNECTOR:VIEW list = an "outputs" array). Read from the loaded box when not passed.
  [ -n "$outputs" ] || outputs="$(strih_lx_hdmi_outputs 2>/dev/null || true)"
  [ -n "$outputs" ] || outputs=auto
  # the module's activation contract: <user_home>/.camera-box/drm-output.json of the desktop user
  local DRM_CONF_DIR="${user_home}/.camera-box"
  local DRM_CONF="${DRM_CONF_DIR}/drm-output.json"
  local LEGACY_PROJ=/opt/camera-box/strih-lx-projector.json
  LEGACY_PROJ="${STRIH_LEGACY_PROJ:-$LEGACY_PROJ}" # a test seam only; a real step-6 run never sets it
  local DRM_VIEW0 DRM_CONN DRM_LINE DRM_XRANDR=""
  case "$backend" in
    lease | vk-direct) ;;
    *) fail "issue 1346: the HDMI output backend fact is '${backend}' (want lease or vk-direct) -- fix the box fact STRIH_HDMI_OUTPUT_BACKEND" ;;
  esac
  DRM_VIEW0="$(strih_drm_legacy_view "$(cat "$LEGACY_PROJ" 2>/dev/null || true)")"
  if [ "$backend" = vk-direct ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y libvulkan1 \
      || fail "issue 1346: apt-get install libvulkan1 failed -- the vk-direct HDMI output cannot start without the Vulkan loader"
    [ -f /usr/share/vulkan/icd.d/nvidia_icd.json ] \
      || warn "  issue 1346: no NVIDIA Vulkan ICD (/usr/share/vulkan/icd.d/nvidia_icd.json) -- the vk-direct HDMI output needs the NVIDIA driver's Vulkan"
  fi
  if [ -L "$DRM_CONF_DIR" ] || [ -L "$DRM_CONF" ]; then
    warn "  SKIP issue 1346: ${DRM_CONF_DIR} or ${DRM_CONF} is a symlink -- refusing to write through it as root; remove it and re-run"
  elif [ ! -f "$DRM_CONF" ] && strih_box_hdmi_outputs_ok "$outputs"; then
    strih_drm_outputs_provision_fresh "$user_home" "$desktop_user" "$backend" "$outputs" "$DRM_CONF"
  elif [ -f "$DRM_CONF" ]; then
    echo "  ${DRM_CONF} already present -- leaving the operator's HDMI output choice"
    if sudo -u "$desktop_user" python3 -c '
import sys
sys.path[:0] = [sys.argv[1], "/usr/local/bin"]
try:
    import strih_scenes as s
except ImportError as e:
    sys.exit("cannot import strih_scenes (%s) -- setup-strih.sh installs python3-websocket at step 11; re-run it" % e)
try:
    if sys.argv[4] in ("auto", "none"):
        s.write_drm_backend(sys.argv[2], sys.argv[3])
    else:  # issue 1424 part 2: the fact lists the connectors, the operator views are kept
        s.write_drm_outputs(sys.argv[4], sys.argv[2], sys.argv[3])
except ValueError as e:
    sys.exit(str(e))
' "$scripts_dir" "$backend" "$DRM_CONF" "$outputs"; then
      echo "  ${DRM_CONF}: backend ${backend} (the box fact STRIH_HDMI_OUTPUT_BACKEND), outputs ${outputs} (STRIH_HDMI_OUTPUTS; the operator's views kept)"
    else
      warn "  issue 1346: could not write backend ${backend} into ${DRM_CONF} (the reason is above) -- verify-strih item 4c reports it as backend-drift"
    fi
  else
    # No config yet. The NVIDIA connector's kernel status stays `disconnected`, so a vk-direct box
    # detects the monitor through the X RandR view (main design 5840508308), queried only here.
    if [ "$backend" = vk-direct ]; then
      DRM_XRANDR="$(strih_drm_xrandr_query "$user_home" "$desktop_user")"
    fi
    if [ "$backend" = vk-direct ] && [ -z "$DRM_XRANDR" ]; then
      warn "  SKIP issue 1346: vk-direct detects the HDMI monitor through X RandR, but xrandr on :0 answered nothing (Xorg :0 not up yet?) -- ${DRM_CONF} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
    elif strih_drm_hdmi_connected /sys/class/drm "$backend" "$DRM_XRANDR"; then
      [ -n "$DRM_XRANDR" ] || DRM_XRANDR="$(strih_drm_xrandr_query "$user_home" "$desktop_user")"
      DRM_CONN="$(printf '%s\n' "$DRM_XRANDR" | strih_drm_hdmi_output_from_xrandr)"
      if [ -n "$DRM_CONN" ] && DRM_LINE="$(strih_drm_output_config_json "$DRM_CONN" "$DRM_VIEW0" "$backend")"; then
        install -d -o "$desktop_user" -g "$desktop_user" "$DRM_CONF_DIR"
        printf '%s\n' "$DRM_LINE" | install -m 0644 -o "$desktop_user" -g "$desktop_user" /dev/stdin "$DRM_CONF"
        echo "  wrote ${DRM_CONF} (HDMI output ${DRM_CONN}, backend ${backend}, view ${DRM_VIEW0}; takes effect at the next OBS start)"
      else
        warn "  SKIP issue 1346: an HDMI monitor is connected but X RandR could not name it (Xorg :0 not up yet?) -- ${DRM_CONF} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
      fi
    else
      warn "  SKIP issue 1346: no HDMI monitor connected -- ${DRM_CONF} NOT provisioned (the fixed HDMI output stays dormant); attach the HDMI monitor and re-run setup-strih.sh"
    fi
  fi
  rm -f "$LEGACY_PROJ"
  # issue 1424 (design 6386467303 layer 1): X never sets a mode on a fixed HDMI output, at any monitor
  # state (an xorg.conf.d MetaModes over the operator outputs only; applies at the next X start)
  strih_x_display_provision "$user_home" "$desktop_user" "$scripts_dir" "$DRM_CONF"
  # issue 1424: the output writes the program mix straight into its connector's HDMI PCM, so
  # PipeWire must leave every HDMI/DP PCM alone (generic, no per-box fact)
  drm_output_audio_wp_provision
}
