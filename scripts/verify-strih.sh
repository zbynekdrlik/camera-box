#!/bin/bash
# verify-strih.sh (issue 1317) -- see the extended header below the strict-mode line.
# strih-lx acceptance gate: RUN ON the box, read-only, fail loud on any missing item.
set -euo pipefail
#
# The imag verify-imag.sh pattern: pure decision predicates live in scripts/lib/strih-provision.sh
# (sourced + unit-tested from tests/strih_provision_pure_functions.rs); this flow feeds LIVE reads
# into them and prints one PASS/FAIL line per acceptance item. Exit 0 = all clear, 1 = a gate item
# failed. Latency pins are REPORT-ONLY (per-source latency is the operator's A/V-align domain).
#
# Usage (on the box):  ./verify-strih.sh [--box <name>]   (default box: strih-lx; issue 1361 -- the
#                      box's facts come from scripts/strih-boxes/<name>.env; a TODO_OWNER fact refuses)
#                      [STRIH_LX_HOST / OBS_WS_HOST override the WS target]

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/strih-box-facts.sh
. "${HERE}/lib/strih-box-facts.sh"   # issue 1361: the ONE per-box fact loader (--box <name>)
# shellcheck source=scripts/lib/strih-provision.sh
. "${HERE}/lib/strih-provision.sh"
# shellcheck source=scripts/lib/strih-drm-output.sh
. "${HERE}/lib/strih-drm-output.sh"   # issue 1346: item 4c grades the DRM-lease HDMI output
# shellcheck source=scripts/lib/ndi-discovery.sh
. "${HERE}/lib/ndi-discovery.sh"   # issue 1342: item 34 grades the receiver-side NDI config (networks.ips)
# issue 1359: the REPORT-ONLY CEF keyring item (14b) grades the OBS CEF password-store switch.
# shellcheck source=scripts/lib/strih-cef-keyring.sh
. "${HERE}/lib/strih-cef-keyring.sh"
# issue 1357: the ONE grader for the shared OBS-box appliance baseline (verify-imag.sh runs it too).
# shellcheck source=scripts/lib/obs-box-baseline-verify.sh
. "${HERE}/lib/obs-box-baseline-verify.sh"
# issue 1361: item 8 grades the shared remoteos-mcp venv install, item 35 the Downstream Keyer plugin.
# shellcheck source=scripts/lib/remoteos-mcp.sh
. "${HERE}/lib/remoteos-mcp.sh"
# shellcheck source=scripts/lib/obs-downstream-keyer.sh
. "${HERE}/lib/obs-downstream-keyer.sh"
# issue 1317: the dantesync item grades a FRESH offset via the SHARED freshness-aware verdict (the
# cambox verify-device (d) shape) instead of reading a Windows/imag dantesync JSON config file a
# flag-based Linux client never creates. clock-offset-guard.sh has its own source-guard, so sourcing
# it defines only its pure functions (dantesync_offset_verdict / ptp_locked_from_journal).
# shellcheck source=scripts/clock-offset-guard.sh
. "${HERE}/clock-offset-guard.sh"

# --- issue 1361: select + load the box facts BEFORE the source-guard (a sourced verify -- the unit
# tests -- sees the same facts the real run uses). An invalid / TODO_OWNER fact refuses here.
STRIH_FACT_BOX="$(strih_box_cli_box "$@")" || { echo "usage: verify-strih.sh [--box <name>]" >&2; exit 1; }
strih_box_load "$STRIH_FACT_BOX" \
  || { echo -e "${RED}FAIL: box '${STRIH_FACT_BOX}': scripts/strih-boxes/${STRIH_FACT_BOX}.env is missing, invalid or still has TODO_OWNER facts (listed above)${NC}" >&2; exit 1; }

# --- source-guard: when sourced (the unit tests), stop here -- never run the live checks ----------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

FAILS=0
ok()   { echo -e "  ${GREEN}PASS${NC} $1"; }
bad()  { echo -e "  ${RED}FAIL${NC} $1"; FAILS=$((FAILS+1)); }
note() { echo -e "  ${YELLOW}NOTE${NC} $1"; }

WS_HOST="${OBS_WS_HOST:-127.0.0.1}"
GENLOCK_DIR="${STRIH_GENLOCK_DIR:-/opt/obs-genlock}"   # installed bundle root (setup-strih.sh step 4)
USER_HOME="/home/${STRIH_LX_USER:-newlevel}"
OBS_LOG_DIR="${OBS_LOG_DIR:-${USER_HOME}/.config/obs-studio/logs}"
newest_log() { ls -1t "${OBS_LOG_DIR}"/*.txt 2>/dev/null | head -1; }
tcp_open()   { timeout 4 bash -c "exec 3<>/dev/tcp/${1}/${2}" 2>/dev/null; }
# The ONE "is OBS running" predicate (item 1): the supervisor unit OR an obs process.
obs_running() {
  systemctl --user is-active strih-obs.service >/dev/null 2>&1 \
    || pgrep -x obs >/dev/null 2>&1 \
    || pgrep -f 'bin/64bit/obs\|/obs$' >/dev/null 2>&1
}

NDI_PREFIX_V="$(strih_lx_ndi_prefix)"
echo -e "${GREEN}=== verify-strih.sh (issue 1317) acceptance gate -- box $(strih_lx_hostname) ===${NC}"

# 0) launcher pair present + executable (issue 1317): strih-obs.service ExecStart/ExecStop reference
#    /usr/local/bin/strih-obs-start.sh + strih-obs-stop.sh; a missing/dangling launcher makes the
#    unit flap 203/EXEC. Checked FIRST so a dangling ExecStart names ITSELF here, instead of only
#    surfacing as the generic "OBS not running under the supervisor" item below.
LAUNCHER_BIN_DIR="${STRIH_LAUNCHER_BIN_DIR:-/usr/local/bin}"
if _lp_missing="$(strih_launcher_pair_ok "$LAUNCHER_BIN_DIR")"; then
  ok "strih-obs launcher pair present + executable in ${LAUNCHER_BIN_DIR}"
else
  bad "strih-obs launcher pair not installed in ${LAUNCHER_BIN_DIR} (${_lp_missing//$'\n'/, }) -- strih-obs.service ExecStart would flap 203/EXEC; re-run setup-strih.sh step 8"
fi

# 0b) runtime libraries resolve + the recorded runtime packages are installed (issue 1317). The
#     bundle is installed into its /usr prefix (setup-strih.sh step 4), so the loader must resolve
#     EVERY dependency of /usr/bin/obs, the bundle's libobs.so.30 and distroav.so -- an unresolved
#     soname IS the 13-soname load failure this fixes. And every package RUNTIME_PACKAGES.txt records
#     (Qt6/ffmpeg/GL) must be dpkg-installed. Checked BEFORE the "OBS running" item so a bundle that
#     cannot load names WHY instead of only surfacing as the generic "OBS not running" below.
STRIH_LIBDIR="${STRIH_LIBDIR:-/usr/lib/x86_64-linux-gnu}"
STRIH_OBS_BIN_PATH="${STRIH_OBS_BIN_PATH:-/usr/bin/obs}"
if command -v ldd >/dev/null 2>&1; then
  _unresolved=""
  for _obj in "$STRIH_OBS_BIN_PATH" "${STRIH_LIBDIR}/libobs.so.30" "${STRIH_LIBDIR}/obs-plugins/distroav.so"; do
    [ -e "$_obj" ] || continue
    _u="$(ldd "$_obj" 2>/dev/null | strih_ldd_unresolved || true)"
    [ -n "$_u" ] && _unresolved="${_unresolved}${_unresolved:+, }${_u//$'\n'/, }"
  done
  if [ -z "$_unresolved" ]; then
    ok "OBS runtime libraries all resolve (ldd over /usr/bin/obs + libobs.so.30 + distroav.so)"
  else
    bad "OBS has UNRESOLVED runtime libraries -- the bundle cannot load (install the runtime packages / re-run setup-strih.sh step 4): ${_unresolved}"
  fi
else
  note "ldd absent -- cannot check runtime library resolution"
fi
RUNTIME_PKGS_FILE="${GENLOCK_DIR}/RUNTIME_PACKAGES.txt"
if [ -f "$RUNTIME_PKGS_FILE" ]; then
  _missing_pkg=""
  while IFS= read -r _pkg; do
    [ -n "$_pkg" ] || continue
    dpkg -s "$_pkg" >/dev/null 2>&1 || { _missing_pkg="$_pkg"; break; }
  done < <(strih_runtime_packages_from_file "$RUNTIME_PKGS_FILE")
  if [ -z "$_missing_pkg" ]; then
    ok "all bundle runtime packages installed (RUNTIME_PACKAGES.txt)"
  else
    bad "bundle runtime package '${_missing_pkg}' is NOT installed (RUNTIME_PACKAGES.txt) -- re-run setup-strih.sh step 4"
  fi
else
  bad "RUNTIME_PACKAGES.txt missing under ${GENLOCK_DIR} -- a strih bundle always records it since issue 1317; rebuild + re-provision"
fi

# 1) OBS running under the supervisor.
if obs_running; then
  ok "OBS running (strih-obs.service / obs process)"
else
  bad "OBS not running under the strih-obs.service supervisor"
fi

# 2) render tick ENABLED + DistroAV loaded in the newest OBS log.
LOG="$(newest_log)"
if [ -n "$LOG" ] && [ -f "$LOG" ]; then
  strih_lx_render_tick_ok    < "$LOG" && ok "genlock render tick ENABLED (newest log)" || bad "genlock render tick not enabled in the newest log"
  strih_lx_distroav_loaded_ok < "$LOG" && ok "DistroAV/NDI loaded (newest log)" || bad "DistroAV not loaded in the newest log"
else
  bad "no OBS log found under ${OBS_LOG_DIR}"
fi

# 3) OBS-WebSocket :4455.
tcp_open "$WS_HOST" 4455 && ok "obs-websocket :4455 answering" || bad "obs-websocket :4455 not answering on ${WS_HOST}"

# 4) NDI outputs namespaced STRIH-LX (...) (never a 2nd STRIH-SNV sender). Live enum via
#    obs_phase2.py when present; fall back to the seed manifest (what the box will publish).
if [ -f /opt/camera-box/strih-lx-seed.json ] && command -v python3 >/dev/null 2>&1; then
  LIVE_OUTS="$(python3 -c 'import json,sys; d=json.load(open("/opt/camera-box/strih-lx-seed.json")); print("\n".join(d.get("outputs",[])))' 2>/dev/null || true)"
  if [ -n "$LIVE_OUTS" ]; then
    all_ns=1
    while IFS= read -r o; do [ -n "$o" ] || continue; strih_lx_output_name_ok "$o" || all_ns=0; done <<< "$LIVE_OUTS"
    [ "$all_ns" = 1 ] && ok "all declared NDI outputs are ${NDI_PREFIX_V}-namespaced" || bad "a declared NDI output is not ${NDI_PREFIX_V}-namespaced"
    printf '%s\n' "$LIVE_OUTS" | strih_lx_no_second_strihsnv_sender && ok "no 2nd STRIH-SNV sender in the output set" || bad "a STRIH-SNV sender is present (collision with the Windows PC)"
  else
    note "no outputs in strih-lx-seed.json to check"
  fi
else
  note "strih-lx-seed.json / python3 absent -- run setup-strih.sh step 6 first"
fi

# 4b) seeded NDI INPUTS present as genlock_fifo sources (issue 1317) -- REPORT-ONLY. Runs the seeder's
#     own read-only --verify-parity (the imag verify_parity grep-qxF whole-line shape) and reports the
#     verdict. A not-yet-launched box (OBS/WS down) yields no OK line -> NOTE, never a hard FAIL: the
#     seed is a LAUNCH-time action (strih-obs-start.sh --bootstrap), not a provisioning artifact, so
#     the acceptance gate must not depend on OBS being live during verify.
SCN_BIN="${STRIH_SCENES_BIN:-/usr/local/bin/strih_scenes.py}"
if [ -f "$SCN_BIN" ] && command -v python3 >/dev/null 2>&1; then
  SEED_PARITY="$(python3 "$SCN_BIN" --host "$WS_HOST" --verify-parity 2>/dev/null || true)"
  # issue 1317: surface the per-input CLASS (cameras genlocked, 2ME PGM/PVW feedback non-genlocked) --
  # REPORT-ONLY, so a mis-classed input is visible in the acceptance output without gating.
  CLASS_LINE="$(printf '%s\n' "$SEED_PARITY" | grep '^strih ndi input classes:' | head -1 || true)"
  [ -n "$CLASS_LINE" ] && note "seeded input classes -- ${CLASS_LINE#strih ndi input classes: }"
  if printf '%s\n' "$SEED_PARITY" | grep -qxF "strih ndi inputs: OK"; then
    ok "all seed NDI inputs present in their expected class (cameras genlocked, 2ME feedback non-genlocked; strih_scenes.py --verify-parity)"
  else
    PARITY_LINE="$(printf '%s\n' "$SEED_PARITY" | grep '^strih ndi inputs:' | head -1 || true)"
    note "seed-input parity not confirmed over WS (OBS/WS down, or a drift) -- report-only: ${PARITY_LINE:-<no verdict line>}"
  fi
else
  note "strih_scenes.py / python3 absent -- run setup-strih.sh step 6 first (seed-input parity report skipped)"
fi

# 4c) issue 1346 (owner 24.9.2026): the fixed HDMI output = the in-OBS DRM-lease output (issue 1152),
#     selectable Program / built-in Multiview -- never a projector window, never the desktop. SKIP
#     when no HDMI monitor is connected (the kernel connector status; today's strih-lx is eDP-only).
#     With one plugged in: ~/.camera-box/drm-output.json must arm the lease (classified by the ONE
#     Python grammar in strih_scenes.py, the C module's own contract) and the newest OBS log must
#     reach `drm-output: program scanout LIVE` -- plus `drm-output: multiview bind LIVE` for the
#     multiview view. The pure verdict is strih_drm_output_verdict (scripts/lib/strih-drm-output.sh).
DRM_CONF_V="${USER_HOME}/.camera-box/drm-output.json"
DRM_HDMI=0
strih_drm_hdmi_connected && DRM_HDMI=1
DRM_SUMMARY="? program"   # no classifier at all (strih_scenes / python3 missing) = unclassified
if [ -f "$SCN_BIN" ] && command -v python3 >/dev/null 2>&1; then
  DRM_SUMMARY="$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import strih_scenes as s; t = s.drm_output_config_text(sys.argv[2]); print(s.drm_output_lease_connector(t) or "-", s.drm_output_view_token(t))' \
    "$(dirname "$SCN_BIN")" "$DRM_CONF_V" 2>/dev/null || echo "? program")"
fi
DRM_ARMED="${DRM_SUMMARY%% *}"
DRM_VIEW_V="${DRM_SUMMARY##* }"
# issue 1346: the config's backend against the box fact STRIH_HDMI_OUTPUT_BACKEND (a lease config on the
# NVIDIA-driven HDMI never goes live). "?" = not read (an older installed strih_scenes) -- the check skips.
DRM_BACKEND_V="?"
if [ -f "$SCN_BIN" ] && command -v python3 >/dev/null 2>&1; then
  DRM_BACKEND_V="$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import strih_scenes as s; print(s.drm_output_backend_token(s.drm_output_config_text(sys.argv[2])))' \
    "$(dirname "$SCN_BIN")" "$DRM_CONF_V" 2>/dev/null || echo "?")"
fi
DRM_BACKEND_FACT="$(strih_lx_hdmi_output_backend 2>/dev/null || echo "?")"
DRM_LIVE=0
DRM_MV_LIVE=0
DRM_LOG="$(newest_log || true)"
if [ -n "$DRM_LOG" ]; then
  # byte-safe: OBS logs carry raw invalid UTF-8 (the imag-display-path.sh grep form)
  LC_ALL=C grep -aqF 'drm-output: program scanout LIVE' "$DRM_LOG" 2>/dev/null && DRM_LIVE=1
  LC_ALL=C grep -aqF 'drm-output: multiview bind LIVE' "$DRM_LOG" 2>/dev/null && DRM_MV_LIVE=1
fi
# issue 1346 review: `program scanout LIVE` stays in the log after the vk-direct present loop died.
DRM_VK_DEAD=0
if [ -n "$DRM_LOG" ] && strih_drm_vk_present_dead < "$DRM_LOG"; then
  DRM_VK_DEAD=1
fi
DRM_VERDICT="$(strih_drm_output_verdict "$DRM_HDMI" "$DRM_ARMED" "$DRM_VIEW_V" "$DRM_LIVE" "$DRM_MV_LIVE" "$DRM_BACKEND_V" "$DRM_BACKEND_FACT" "$DRM_VK_DEAD" || true)"
case "$DRM_VERDICT" in
  ok)                 ok   "HDMI output: ${DRM_ARMED} live (backend ${DRM_BACKEND_V}), view ${DRM_VIEW_V} (${DRM_CONF_V} + the newest OBS log)" ;;
  skip-no-hdmi)       note "HDMI output: SKIP -- no HDMI monitor connected, the DRM-lease output stays dormant (attach one and re-run setup-strih.sh step 6)" ;;
  hdmi-unplugged)     note "HDMI output: ${DRM_ARMED} is armed in ${DRM_CONF_V} but no HDMI monitor is connected (report-only)" ;;
  classify-failed)    bad  "HDMI output: could not classify ${DRM_CONF_V} (strih_scenes.py / python3 missing or its import failed) -- re-run setup-strih.sh step 6" ;;
  config-missing)     bad  "HDMI output: an HDMI monitor is connected but ${DRM_CONF_V} does not arm the DRM lease -- re-run setup-strih.sh (step 6)" ;;
  view-invalid)       bad  "HDMI output: ${DRM_CONF_V} \"view\" is not program or multiview (OBS falls back to Program) -- fix it in OBS Tools > HDMI výstup" ;;
  backend-invalid)    bad  "HDMI output: ${DRM_CONF_V} \"backend\" is not lease or vk-direct (OBS keeps the output dormant) -- re-run setup-strih.sh step 6" ;;
  backend-drift)      bad  "HDMI output: ${DRM_CONF_V} uses backend ${DRM_BACKEND_V} but the box fact STRIH_HDMI_OUTPUT_BACKEND is ${DRM_BACKEND_FACT} -- re-run setup-strih.sh step 6" ;;
  present-dead)       bad  "HDMI output: the vk-direct present loop died in the newest OBS log ('vk-direct present loop exited' with no stop after it) -- read its drm-output: lines, then restart strih-obs.service" ;;
  lease-not-live)     bad  "HDMI output: ${DRM_ARMED} is armed but the newest OBS log never reached 'drm-output: program scanout LIVE' -- read its drm-output: lines, then restart strih-obs.service" ;;
  multiview-not-live) bad  "HDMI output: the view is multiview but the newest OBS log has no 'drm-output: multiview bind LIVE' (the built-in Multiview never reached the scanout)" ;;
  *)                  bad  "HDMI output: unknown verdict '${DRM_VERDICT}'" ;;
esac
# The operator's LAPTOP projector (the Multiview on the eDP panel) persists via SaveProjectors
# (setup-strih.sh step 7) -- report-only.
if grep -qi '^SaveProjectors=true' "$(dirname "$OBS_LOG_DIR")/user.ini" 2>/dev/null; then
  ok "laptop projector persistence: SaveProjectors=true pre-seeded in user.ini"
else
  note "laptop projector persistence: SaveProjectors=true NOT pre-seeded in user.ini (re-run setup-strih.sh step 7)"
fi

# 5) Certified latency pins vs scripts/latency-pins-baseline.json (strih-lx key) -- REPORT-ONLY.
BASELINE="${HERE}/latency-pins-baseline.json"
if command -v python3 >/dev/null 2>&1 && [ -f "$BASELINE" ]; then
  PIN_BOX_V="$(strih_lx_hostname)"
  FLOOR="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); b=d.get(sys.argv[2],{}); print(b.get("_all_camera_ndi_inputs_ms","?"))' "$BASELINE" "$PIN_BOX_V" 2>/dev/null || echo '?')"
  note "latency-pins-baseline.json ${PIN_BOX_V} floor = ${FLOOR} ms (report-only; aligner owns any offset)"
else
  note "latency baseline / python3 absent -- pin verify is report-only"
fi

# 6) dantesync unit ACTIVE + a FRESH in-bound clock offset (issue 1317 -- the cambox verify-device
#    (d) shape). A flag-based Linux client has NO dantesync JSON config file (that is a Windows/imag
#    artifact), so this asserts the RUNNING state: the unit is active AND the journal shows a fresh
#    offset within bound via the SHARED dantesync_offset_verdict/freshest_offset_us. A `stale`/`absent`
#    offset with the PTP servo LOCKED is disciplined near-zero (the #550 reasoning) -> PASS; not
#    locked -> FAIL (no trustworthy clock signal). setup-strih.sh step 2 refuses an ambiguous
#    role+args shape (strih_lx_dantesync_role_ok) at install time; the role itself is graded in 6b.
#    Issue 1372: under dantesync 1.9.0 strih-lx is the fleet DATE master, whose `(date authority,
#    ..., step bound Nus)` journal line is graded on that step bound + margin through the shared
#    dantesync_journal_clock_verdict (clock-offset-guard.sh), never on the 2 ms UTC bound.
DS_ACTIVE="$(systemctl is-active dantesync 2>/dev/null || true)"
if [ "$DS_ACTIVE" != active ]; then
  bad "dantesync.service not active (state='${DS_ACTIVE:-<none>}') -- clock undisciplined/free-running"
else
  DS_JOURNAL="$(journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null || true)"
  # Issue 1372: strih-lx is the fleet DATE master under dantesync 1.9.0; its journal reads
  # `[NTP] offset:-25217us (date authority, fleet line ..., step bound 50000us)` and is graded on
  # that step bound + DANTESYNC_DATE_MARGIN_US, median-only (dantesync_journal_clock_verdict). Any
  # other journal line shape keeps the CLOCK_GUARD_BOUND_US + stability grade.
  DS_STEP_US="$(date_step_bound_us_from_journal "$DS_JOURNAL")"
  DS_BOUND_TXT="${CLOCK_GUARD_BOUND_US:-2000}us bound"
  [ -n "$DS_STEP_US" ] && DS_BOUND_TXT="date master step bound ${DS_STEP_US}us + ${DANTESYNC_DATE_MARGIN_US:-1000}us margin"
  case "$(dantesync_journal_clock_verdict "$DS_JOURNAL" "${DANTESYNC_OFFSET_FRESHNESS_S:-300}" "${CLOCK_GUARD_BOUND_US:-2000}" "${DANTESYNC_STABILITY_US:-2000}" "${DANTESYNC_DATE_MARGIN_US:-1000}")" in
    ok)
      ok "dantesync active + FRESH clock offset within ${DS_BOUND_TXT}" ;;
    stale|absent)
      if [ "$(ptp_locked_from_journal "$DS_JOURNAL")" = LOCKED ]; then
        ok "dantesync active + PTP servo LOCKED (no fresh [NTP] line; offset disciplined near-zero, #550)"
      else
        bad "dantesync active but NO fresh clock offset and PTP servo not LOCKED -- no trustworthy clock signal"
      fi ;;
    *)
      bad "dantesync clock offset OUTSIDE the ${DS_BOUND_TXT} / unstable -- a REAL clock desync" ;;
  esac
fi

# 7) bundle-state :8899.
tcp_open "$WS_HOST" 8899 && ok "bundle-state :8899 answering" || bad "bundle-state :8899 not answering"

# 8) remoteos-mcp agent (issue 1361): the shared venv install -- the unit runs the /opt/remoteos-mcp-venv
#    python with the key in its 0600 root EnvironmentFile (never an --auth-key in the unit), the venv
#    imports remoteos, the service is enabled + active, and an unauthenticated /mcp request is refused
#    (401). A box still on the hand-made / upstream-
#    installer unit FAILs until setup-strih.sh step 10 re-runs (it keeps the box's key).
_rm_unit="$(cat "$(remoteos_mcp_unit_path)" 2>/dev/null || true)"
_rm_env="$(stat -c '%a %U' "$(remoteos_mcp_env_file)" 2>/dev/null || true)"
_rm_import=0
"$(remoteos_mcp_venv_dir)/bin/python" -c 'import remoteos' >/dev/null 2>&1 && _rm_import=1
_rm_en="$(systemctl is-enabled remoteos-mcp 2>/dev/null || true)"
_rm_act="$(systemctl is-active remoteos-mcp 2>/dev/null || true)"
# An unauthenticated POST to the local agent must be refused (401): an empty key turns auth OFF.
_rm_unauth="$(remoteos_mcp_unauth_code)"
_rm_verdict="$(remoteos_mcp_verdict "$_rm_unit" "$_rm_env" "$_rm_import" "$_rm_en" "$_rm_act" "$_rm_unauth")" || true
case "$_rm_verdict" in
  ok*) ok "remoteos-mcp ${_rm_verdict#ok }" ;;
  *) bad "remoteos-mcp ${_rm_verdict#FAIL: } -- re-run setup-strih.sh step 10 (issue 1361)" ;;
esac

# 9) PipeWire program audio (issue 1344): the DERIVED verdict — the strih-program null sink + the OBS
#    `ASIO zvuk` pulse_input_capture + (when the hub is active) the program-feed rx. The FOH-live
#    level is a SUPERVISOR live-acceptance step, so verify passes FOH=unknown here (never a level
#    FAIL); a FAIL means a structural provisioning gap (sink/input absent) or, once the hub is
#    active, no program rx.
AUDIO_SINK=0
# 22.9.2026: setup-strih step 17 runs this gate as ROOT, whose pw-cli cannot see the OPERATOR's
# PipeWire session (the null sink lives there) -> a false "sink missing" FAIL on a healthy box. Probe
# the operator session explicitly when running as root (sudo -u + its XDG_RUNTIME_DIR), else inline.
if command -v pw-cli >/dev/null 2>&1; then
  if [ "$(id -u)" = 0 ]; then
    PW_LS="$(sudo -u "${STRIH_LX_USER:-newlevel}" XDG_RUNTIME_DIR="/run/user/$(id -u "${STRIH_LX_USER:-newlevel}")" pw-cli ls Node 2>/dev/null || true)"
  else
    PW_LS="$(pw-cli ls Node 2>/dev/null || true)"
  fi
  printf '%s\n' "$PW_LS" | grep -q '"strih-program"' && AUDIO_SINK=1
fi
AUDIO_KIND="$(python3 "$SCN_BIN" --host "$WS_HOST" --audio-input-kind 2>/dev/null || echo absent)"
if systemctl is-active intercom-hub >/dev/null 2>&1; then
  # hub active (M4+): grade the real program-feed rx off the hub /api/state.
  if curl -fsS --max-time 3 "http://${WS_HOST}:8790/api/state" 2>/dev/null \
       | python3 -c 'import sys,json; d=json.load(sys.stdin); sys.exit(0 if any(p.get("name")=="fohabl" and p.get("rx_packets",0)>0 for p in d.get("participants",[])) else 1)'; then
    AUDIO_RX=1
  else
    AUDIO_RX=0
  fi
else
  # enable-only phase (hub enabled-not-started until the M4 cut-over): grade only the structural
  # sink + OBS input; the rx is verified once the hub runs.
  AUDIO_RX=1
fi
AUDIO_VERDICT="$(strih_lx_program_audio_verdict "$AUDIO_SINK" "$AUDIO_RX" "$AUDIO_KIND" unknown na || true)"
case "$AUDIO_VERDICT" in
  PASS*) ok   "program audio: $AUDIO_VERDICT" ;;
  NOTE*) note "program audio: $AUDIO_VERDICT" ;;
  *)     bad  "program audio: $AUDIO_VERDICT" ;;
esac
# talkback capture (report-only): the MiniFuse 4 is the operator talkback mic (the hub reads it).
if arecord -l 2>/dev/null | grep -qi 'MiniFuse'; then note "talkback: MiniFuse 4 present (operator mic)"; else note "talkback: MiniFuse 4 not detected (plug it in before go-live)"; fi

# 9b) MiniFuse period + graph quantum (issue 1345, owner accepted 25.9.2026): both operator-session
#     drop-ins present with the rendered content (FAIL when missing or drifted -- the buzz / robotic
#     cameraman fixes a reprovision must keep), and the LIVE graph held at quantum 1024
#     (`pw-metadata -n settings`, read in the operator session as root like item 9). The live read is
#     REPORTED: PASS when held, NOTE otherwise (the drop-in applies only at the next PipeWire start).
for _q_pair in \
  "${USER_HOME}/.config/wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf|strih_wireplumber_minifuse_output_period_conf" \
  "${USER_HOME}/.config/pipewire/pipewire.conf.d/51-strih-quantum-1024.conf|strih_pipewire_quantum_conf"; do
  _q_file="${_q_pair%%|*}"; _q_fn="${_q_pair##*|}"
  if [ -f "$_q_file" ] && cmp -s "$_q_file" <("$_q_fn"); then
    ok "(audio-quantum) ${_q_file##*/} present with the provisioned content"
  else
    bad "(audio-quantum) ${_q_file} missing or drifted from ${_q_fn} -- re-run setup-strih.sh step 12 (issue 1345)"
  fi
done
if command -v pw-metadata >/dev/null 2>&1; then
  if [ "$(id -u)" = 0 ]; then
    PW_SETTINGS="$(sudo -u "${STRIH_LX_USER:-newlevel}" XDG_RUNTIME_DIR="/run/user/$(id -u "${STRIH_LX_USER:-newlevel}")" timeout 5 pw-metadata -n settings 2>/dev/null || true)"
  else
    PW_SETTINGS="$(timeout 5 pw-metadata -n settings 2>/dev/null || true)"
  fi
else
  PW_SETTINGS=""
fi
if Q_DETAIL="$(strih_lx_graph_quantum_ok "$PW_SETTINGS")"; then
  ok "(audio-quantum) ${Q_DETAIL}"
else
  note "(audio-quantum) ${Q_DETAIL} -- report-only; restart the operator PipeWire session (or reboot) so the min-quantum drop-in applies"
fi

# 10) NVENC encoder available.
{ ffmpeg -hide_banner -encoders 2>/dev/null; cat "$LOG" 2>/dev/null; } | strih_lx_nvenc_available_ok && ok "NVENC encoder available" || bad "NVENC encoder not available"

# 12) single timesync authority (dantesync only).
UNITS="$(systemctl list-units --type=service --state=active --no-legend 2>/dev/null | awk '{print $1}')"
printf '%s\n' "$UNITS" | strih_lx_single_timesync_authority_ok && ok "single timesync authority (dantesync only)" || bad "timesync authority not single (dantesync missing or a competitor active)"

# 13) browser bundle (issue 1317): when the installed STRIH_BUILD_FLAGS.txt declares BROWSER-ON, the
#     bundle MUST carry obs-browser.so AND the CEF runtime (libcef.so) -- fail loud by name. A
#     BROWSER-OFF (or absent) marker means browser was not built, so this item NOTE-skips.
FLAGS_FILE="${GENLOCK_DIR}/STRIH_BUILD_FLAGS.txt"
if [ -f "$FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$FLAGS_FILE")"; then
  if find "$GENLOCK_DIR" -type f \( -name 'obs-browser.so' -o -name 'libcef.so' \) 2>/dev/null \
       | strih_lx_browser_bundle_ok; then
    ok "BROWSER-ON: obs-browser.so + CEF runtime (libcef.so) present in ${GENLOCK_DIR}"
  else
    bad "BROWSER-ON declared but obs-browser.so and/or CEF runtime (libcef.so) missing under ${GENLOCK_DIR}"
  fi
else
  note "browser bundle not required (STRIH_BUILD_FLAGS.txt absent or BROWSER-OFF at ${GENLOCK_DIR})"
fi

# 14) chrome-sandbox setuid (issue 1317 F6): when BROWSER-ON, the CEF SUID sandbox helper must be
#     owned root:root mode 4755 (setuid root) or the browser sources cannot launch (Chromium aborts
#     unless the sandbox is disabled at launch). Same GENLOCK_DIR find-path as item 13.
#     BROWSER-OFF/absent -> NOTE-skip.
if [ -f "$FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$FLAGS_FILE")"; then
  CS="$(find "$GENLOCK_DIR" -type f -name chrome-sandbox 2>/dev/null | head -1 || true)"
  if [ -n "$CS" ]; then
    CS_OWNER="$(stat -c '%U:%G' "$CS" 2>/dev/null || echo '?')"
    CS_MODE="$(stat -c '%a' "$CS" 2>/dev/null || echo '?')"
    CS_VERDICT="$(strih_lx_chrome_sandbox_verdict "$CS_OWNER" "$CS_MODE" 1 || true)"
    [ "$CS_VERDICT" = ok ] && ok "chrome-sandbox setuid-root (root:root 4755) -- CEF sandbox launchable" || bad "chrome-sandbox not setuid-root (${CS_VERDICT}: owner=${CS_OWNER} mode=${CS_MODE}); expected root:root 4755"
  else
    bad "chrome-sandbox absent under ${GENLOCK_DIR} (BROWSER-ON but the CEF sandbox helper is missing)"
  fi
else
  note "chrome-sandbox setuid check skipped (STRIH_BUILD_FLAGS.txt absent or BROWSER-OFF at ${GENLOCK_DIR})"
fi

# 14b) CEF keyring prompt (issue 1359) -- REPORT-ONLY. GNOME auto-login leaves the login keyring
#      locked, so an OBS CEF without Chromium's --password-store=basic raises a keyring unlock
#      dialog on the operator screen after every reboot. PASS when a running obs-browser-page
#      command line carries the switch, else when the LOADED obs-browser.so has it compiled in (the
#      CEF browser process is OBS itself; the switch is applied in-process, so a child argv need not
#      show it). A plugin without it = a pre-fix bundle -> NOTE. Never FAILs. BROWSER-OFF -> skip.
if [ -f "$FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$FLAGS_FILE")"; then
  CEF_SO_V="${STRIH_LIBDIR}/obs-plugins/obs-browser.so"
  CEF_SO_STATE_V="$(strih_cef_so_password_store_state "$CEF_SO_V")"
  CEF_PAGES_V="$(pgrep -af obs-browser-page 2>/dev/null || true)"
  CEF_VERDICT_V="$(strih_cef_password_store_verdict "$CEF_SO_STATE_V" <<<"$CEF_PAGES_V" || true)"
  case "$CEF_VERDICT_V" in
    ok-live)
      ok "(cef-keyring) a running obs-browser-page argv carries --password-store=basic (behaviour proof = the two-reboot acceptance: no keyring dialog after an auto-login reboot)" ;;
    ok-built)
      ok "(cef-keyring) ${CEF_SO_V} carries the compiled-in password-store switch (applied in-process; no running obs-browser-page argv shows it; behaviour proof = the two-reboot acceptance)" ;;
    missing)
      note "(cef-keyring) ${CEF_SO_V} lacks the password-store=basic switch -- the deployed bundle predates issue 1359; the OBS CEF can raise the GNOME keyring unlock dialog after a reboot (deploy the current strih bundle)" ;;
    *)
      note "(cef-keyring) ${CEF_SO_V} not found -- cannot grade the OBS CEF password-store switch" ;;
  esac
else
  note "(cef-keyring) skipped (STRIH_BUILD_FLAGS.txt absent or BROWSER-OFF at ${GENLOCK_DIR})"
fi

# 15) bundle-vs-box release parity (issue 1317): the installed bundle's TARGET-RELEASE marker must
#     match the box's /etc/os-release VERSION_ID, or a 24.04-built bundle would silently run on a
#     26.04 box (ffmpeg/Qt soname mismatch -> OBS crashes at load). Fail-closed: an absent marker or
#     any mismatch FAILS (a strih bundle always carries TARGET-RELEASE since issue 1317).
BOX_VERSION_ID="$( . /etc/os-release 2>/dev/null; printf '%s' "${VERSION_ID:-}" )"
if [ -f "$FLAGS_FILE" ] && strih_lx_release_parity_ok "$(cat "$FLAGS_FILE")" "$BOX_VERSION_ID"; then
  ok "bundle release parity: TARGET-RELEASE matches box (ubuntu-${BOX_VERSION_ID})"
else
  bad "bundle release parity FAILED: ${FLAGS_FILE} must carry 'TARGET-RELEASE: ubuntu-${BOX_VERSION_ID}' (box VERSION_ID=${BOX_VERSION_ID}); a bundle built for another release must never run here"
fi

# 16) intercom hub unit (issue 1345 M1): the strih-lx intercom hub is installed ENABLE-ONLY while the
#     Windows VB-Matrix stays the live intercom (M4 is the cut-over). REPORT-ONLY: an installed +
#     enabled but NOT active unit is the CORRECT parallel-run state, never a FAIL.
if [ -f /etc/systemd/system/intercom-hub.service ]; then
  IH_EN="$(systemctl is-enabled intercom-hub 2>/dev/null || echo unknown)"
  IH_ACT="$(systemctl is-active intercom-hub 2>/dev/null || echo inactive)"
  note "intercom-hub.service installed (enabled=${IH_EN}, active=${IH_ACT}); enable-only until the M4 cut-over (issue 1345) -- report-only, an inactive unit is correct while parallel"
else
  note "intercom-hub.service not installed -- run setup-strih.sh step 13 (issue 1345 M1); report-only"
fi

# 17) Janus audiobridge audio edge (issue 1345 M3a) -- REPORT-ONLY. Janus is installed ENABLE-ONLY
#     while the phones leg is not yet cut over (the M4 cut-over starts it with the hub), so an
#     installed + enabled but INACTIVE unit is CORRECT here, never a FAIL. Reports: package installed,
#     unit enabled, and the audiobridge room jcfg declares the interkom room (a pure grep -- no janus
#     binary invocation, per the design).
if dpkg -s janus >/dev/null 2>&1 || command -v janus >/dev/null 2>&1; then
  note "janus installed (audiobridge audio edge, issue 1345 M3a) -- report-only"
else
  note "janus NOT installed -- run setup-strih.sh step 14 before the M4 cut-over; report-only"
fi
JANUS_EN="$(systemctl is-enabled janus 2>/dev/null || echo unknown)"
JANUS_ACT="$(systemctl is-active janus 2>/dev/null || echo inactive)"
note "janus.service (enabled=${JANUS_EN}, active=${JANUS_ACT}) -- enable-only until the M4 cut-over (issue 1345 M3a); an inactive unit is correct while parallel, report-only"
JANUS_AB="${JANUS_AUDIOBRIDGE_JCFG:-/etc/janus/janus.plugin.audiobridge.jcfg}"
if [ -f "$JANUS_AB" ]; then
  # The jcfg is root:root 0640 (it carries the room secret): read it as the operator user when
  # possible, else through a NON-interactive sudo (a cached ticket / NOPASSWD); with neither the text
  # is empty and the items below degrade to the honest "unreadable" note, never a false PASS
  # (issue 1352 acceptance run: the pin was live and the gate said "NOT pinned (or unreadable)").
  JANUS_AB_TEXT="$({ cat "$JANUS_AB" 2>/dev/null || sudo -n cat "$JANUS_AB" 2>/dev/null; } || true)"
  if printf '%s\n' "$JANUS_AB_TEXT" | strih_janus_room_jcfg_ok "${JANUS_ROOM:-1000}"; then
    note "janus audiobridge room jcfg declares the interkom room (48 kHz, plain-RTP participants) -- report-only"
  else
    note "janus audiobridge room jcfg present but does not declare room-${JANUS_ROOM:-1000} 'interkom' at 48 kHz (or unreadable) -- re-run setup-strih.sh step 14; report-only"
  fi
  # issue 1352: general.local_ip must pin the plain-RTP bind to the box's static IP (a renumber
  # otherwise strands it EADDRNOTAVAIL and the hub cannot join the room). Report-only, like item 17.
  JANUS_LOCAL_IP_LINE="$(printf '%s\n' "$JANUS_AB_TEXT" | grep -oE '^[[:space:]]*local_ip = "[^"]*"' | head -1 | sed -E 's/^[[:space:]]+//' || true)"
  if [ -n "$JANUS_LOCAL_IP_LINE" ]; then
    note "janus audiobridge general.local_ip pinned (${JANUS_LOCAL_IP_LINE}) -- renumber-proof RTP bind (issue 1352); report-only"
  else
    note "janus audiobridge general.local_ip NOT pinned (or jcfg unreadable) -- a renumber can strand the RTP bind (issue 1352); re-run setup-strih.sh step 14; report-only"
  fi
else
  note "janus audiobridge jcfg absent (${JANUS_AB}) -- run setup-strih.sh step 14; report-only"
fi

# 19) Bitfocus Companion Satellite installed + desktop udev rule + kiosk launch + controller
#     host seeded (issue 1317 rework -- the (companion) item, DESKTOP tarball model). /opt binary +
#     /etc/udev/rules.d/50-satellite-desktop.rules + the openbox autostart launch line (issue 1357:
#     the kiosk launches it; openbox runs no XDG ~/.config/autostart) + the seeded app
#     config.json's remoteIp == the controller (strih_companion_verdict, fail-closed). FAIL loud on
#     any missing part.
CS_INSTALLED=0; [ -x "$(strih_companion_satellite_bin)" ] && CS_INSTALLED=1
CS_UDEV_OK=0; [ -f "$(strih_companion_satellite_udev_rule)" ] && CS_UDEV_OK=1
CS_AUTOSTART="${USER_HOME}/.config/openbox/autostart"
CS_AUTOSTART_OK=0; grep -qxF "$(strih_companion_satellite_openbox_line)" "$CS_AUTOSTART" 2>/dev/null && CS_AUTOSTART_OK=1
CS_HOST_OK=0
CS_APPCFG="${COMPANION_SATELLITE_CONF:-${USER_HOME}/.config/Companion Satellite/config.json}"
# Exact JSON read-back (never a regex whose dots would wildcard the IP); fail-closed on unreadable.
if [ -f "$CS_APPCFG" ] && python3 -c 'import json,sys
try:
    d=json.load(open(sys.argv[1]))
    sys.exit(0 if isinstance(d,dict) and d.get("remoteIp")==sys.argv[2] else 1)
except Exception:
    sys.exit(1)' "$CS_APPCFG" "$(strih_companion_satellite_host)"; then CS_HOST_OK=1; fi
CS_FILE_VERDICT="$(strih_companion_verdict "$CS_INSTALLED" "$CS_UDEV_OK" "$CS_AUTOSTART_OK" "$CS_HOST_OK" || true)"
# issue 1317: when the Satellite is RUNNING, its local REST (:9999/api/status) reports the LIVE
# controller link -- require .connected == true (the electron-store file seed alone left the effective
# host 127.0.0.1 until the step-16 POST). When it is NOT running (a fresh box that seeds but never
# starts it), this stays a FILE-ONLY check (the file verdict).
CS_REST_URL="$(strih_companion_satellite_rest_url)"
CS_RUNNING=0; CS_CONNECTED=0
CS_STATUS_JSON="$(curl -fsS --max-time 2 "${CS_REST_URL}/api/status" 2>/dev/null || true)"
if [ -n "$CS_STATUS_JSON" ]; then
  CS_RUNNING=1
  printf '%s' "$CS_STATUS_JSON" | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin); sys.exit(0 if (isinstance(d,dict) and d.get("connected") is True) else 1)
except Exception:
    sys.exit(1)' && CS_CONNECTED=1
fi
CS_VERDICT="$(strih_companion_status_verdict "$CS_FILE_VERDICT" "$CS_RUNNING" "$CS_CONNECTED" || true)"
if strih_companion_status_verdict "$CS_FILE_VERDICT" "$CS_RUNNING" "$CS_CONNECTED" >/dev/null 2>&1; then
  ok "(companion) Companion Satellite ${CS_VERDICT} -- /opt + desktop udev rule + openbox autostart launch + controller $(strih_companion_satellite_host) (REST running=${CS_RUNNING} connected=${CS_CONNECTED})"
else
  bad "(companion) Companion Satellite gate: ${CS_VERDICT} (bin=${CS_INSTALLED} udev=${CS_UDEV_OK} autostart=${CS_AUTOSTART_OK} host=${CS_HOST_OK} running=${CS_RUNNING} connected=${CS_CONNECTED}) -- re-run setup-strih.sh step 16"
fi

# 20) OBS helper binaries beside /usr/bin/obs (issue 1317, live 20.9.2026): OBS resolves its helper
#     processes -- obs-ffmpeg-mux (the recording muxer) + obs-nvenc-test (the NVENC probe) -- NEXT TO
#     ITS OWN EXECUTABLE, so the prefix install must place them in /usr/bin. Missing = a broken record
#     muxer + `NVENC not supported`. FAIL loud (recording-critical). Additionally, when OBS is running,
#     grade its newest log's NVENC state (report-only NOTE -- needs a running OBS).
CS_MUX_OK=0; [ -x /usr/bin/obs-ffmpeg-mux ] && CS_MUX_OK=1
CS_NVT_OK=0; [ -x /usr/bin/obs-nvenc-test ] && CS_NVT_OK=1
OBSH_VERDICT="$(strih_obs_helpers_verdict "$CS_MUX_OK" "$CS_NVT_OK" || true)"
if [ "$OBSH_VERDICT" = ok ]; then
  ok "(obs-helpers) obs-ffmpeg-mux + obs-nvenc-test present beside /usr/bin/obs (record muxer + NVENC probe)"
else
  bad "(obs-helpers) ${OBSH_VERDICT} beside /usr/bin/obs (mux=${CS_MUX_OK} nvenc-test=${CS_NVT_OK}) -- the prefix install must copy the WHOLE bundle bin/ (re-run setup-strih.sh step 4)"
fi
OBS_LOG_DIR="${USER_HOME}/.config/obs-studio/logs"
NEWEST_OBS_LOG="$(ls -1t "${OBS_LOG_DIR}"/*.txt 2>/dev/null | head -1 || true)"
if [ -n "$NEWEST_OBS_LOG" ] && [ -r "$NEWEST_OBS_LOG" ]; then
  NVENC_VERDICT="$(strih_nvenc_log_verdict < "$NEWEST_OBS_LOG" || true)"
  case "$NVENC_VERDICT" in
    nvenc-ok)          note "(obs-helpers) NVENC live: the newest OBS log carries '[obs-nvenc] NVENC version:'" ;;
    nvenc-unsupported) bad  "(obs-helpers) the newest OBS log says 'NVENC not supported' with no version line -- obs-nvenc-test missing/broken (re-run setup-strih.sh step 4, then restart OBS)" ;;
    *)                 note "(obs-helpers) NVENC state unknown from the OBS log (OBS not running or no NVENC line yet)" ;;
  esac
else
  note "(obs-helpers) no OBS log to read the live NVENC state (OBS not running -- report-only)"
fi

# 21) OBS UI provisioning fixes (issue 1317 slice): the Qt6 SVG icon-engine plugin MUST be present
#     (OBS 32's SVG Yami theme renders its toolbar/dock/settings icons via it -- Ubuntu 26.04 ships it
#     in the SEPARATE qt6-svg-plugins package), AND -- when BROWSER-ON -- chrome-sandbox MUST be setuid
#     root:root 4755 at the /usr-prefix path the running OBS actually loads (item 14 only grades the
#     /opt bundle copy; the /usr copy was 0755 and broke CEF live). Placed before the final summary.
UI_LIBDIR="${STRIH_USR_LIBDIR:-/usr/lib/x86_64-linux-gnu}"
UI_SVG_PATH="$(strih_lx_qt6_svg_iconengine_path "$UI_LIBDIR")"
UI_SVG_PRESENT=0; [ -e "$UI_SVG_PATH" ] && UI_SVG_PRESENT=1
if [ -f "$FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$FLAGS_FILE")"; then
  UI_CS_PATH="$(strih_lx_chrome_sandbox_usr_path "$UI_LIBDIR")"
  UI_CS_OWNER="$(stat -c '%U:%G' "$UI_CS_PATH" 2>/dev/null || echo '?')"
  UI_CS_MODE="$(stat -c '%a' "$UI_CS_PATH" 2>/dev/null || echo '?')"
  UI_VERDICT="$(strih_lx_obs_ui_fix_verdict "$UI_SVG_PRESENT" "$UI_CS_OWNER" "$UI_CS_MODE" || true)"
  if [ "$UI_VERDICT" = ok ]; then
    ok "(obs-ui) qt6-svg iconengine present + chrome-sandbox setuid root:root 4755 at ${UI_CS_PATH}"
  else
    if [ "$UI_SVG_PRESENT" = 1 ]; then _svg_state=present; else _svg_state=MISSING; fi
    bad "(obs-ui) ${UI_VERDICT}: qt6-svg iconengine ${_svg_state} at ${UI_SVG_PATH} / chrome-sandbox owner=${UI_CS_OWNER} mode=${UI_CS_MODE} at ${UI_CS_PATH}; expected the SVG plugin present + root:root 4755 -- re-run setup-strih.sh (qt6-svg-plugins install + F6 setuid)"
  fi
else
  # BROWSER-OFF/absent: no CEF sandbox to grade -- only the SVG theme icons matter.
  if [ "$UI_SVG_PRESENT" = 1 ]; then
    ok "(obs-ui) qt6-svg iconengine present at ${UI_SVG_PATH} (BROWSER-OFF -- chrome-sandbox not graded)"
  else
    bad "(obs-ui) qt6-svg iconengine MISSING at ${UI_SVG_PATH} -- OBS 32's SVG theme icons render blank; install qt6-svg-plugins (re-run setup-strih.sh)"
  fi
fi

# 22) retired strih-mv-host helper absent (issue 1357): the issue-1352 projector re-hosting helper
#     worked around an XWayland + PRIME-offload present stall that the plain Xorg kiosk does not have;
#     it was retired together with the vendored child-host projector (the stock toplevel projector
#     runs on every box). A leftover unit, WantedBy link or helper is drift from the one baseline --
#     FAIL loud and point at setup-strih.sh step 8b, which removes it. Read from the files, so no
#     --user session bus is needed at verify time.
MVH_UNIT="${USER_HOME}/.config/systemd/user/strih-mv-host.service"
MVH_WANTS="${USER_HOME}/.config/systemd/user/default.target.wants/strih-mv-host.service"
MVH_HELPER="/usr/local/bin/strih-mv-host.py"
if [ ! -e "$MVH_UNIT" ] && [ ! -L "$MVH_WANTS" ] && [ ! -e "$MVH_HELPER" ]; then
  ok "(mv-host) retired strih-mv-host helper absent (stock toplevel OBS projector, no re-hosting)"
else
  bad "(mv-host) retired strih-mv-host helper still installed: unit=$( [ -e "$MVH_UNIT" ] && echo present || echo absent ) enabled=$( [ -L "$MVH_WANTS" ] && echo yes || echo no ) helper=$( [ -e "$MVH_HELPER" ] && echo present || echo absent ) -- re-run setup-strih.sh step 8b"
fi

# 24) avahi-browse present (issue 1352): the NDI/mDNS discovery CLI (avahi-utils) -- Ubuntu 26.04 omits
#     it with the avahi daemon, and the rig discovery gates read blind without it. FAIL loud.
command -v avahi-browse >/dev/null 2>&1 \
  && ok "(avahi) avahi-browse present (NDI/mDNS discovery)" \
  || bad "(avahi) avahi-browse MISSING -- install avahi-utils (re-run setup-strih.sh step 4b)"

# 25) DistroAV [NDIPlugin] output identity in user.ini (issue 1352): the BARE names
#     MainOutputName=2ME PGM / PreviewOutputName=2ME PVW + both Enabled=true (DistroAV prepends the
#     hostname -> announced STRIH-LX (2ME PGM)/(2ME PVW); a namespaced name here would double it).
#     FAIL loud.
NDI_USER_INI="${USER_HOME}/.config/obs-studio/user.ini"
NDI_INI_MISSING=""
for _kv in 'MainOutputName=2ME PGM' 'PreviewOutputName=2ME PVW' 'MainOutputEnabled=true' 'PreviewOutputEnabled=true'; do
  grep -qF "$_kv" "$NDI_USER_INI" 2>/dev/null || NDI_INI_MISSING="${NDI_INI_MISSING} [${_kv}]"
done
if [ -z "$NDI_INI_MISSING" ]; then
  ok "(ndi-outputs) user.ini [NDIPlugin] MainOutputName=2ME PGM / PreviewOutputName=2ME PVW + Enabled=true"
else
  bad "(ndi-outputs) ${NDI_USER_INI} missing DistroAV output identity:${NDI_INI_MISSING} -- re-run setup-strih.sh step 7"
fi

# 26) BrowserHWAccel=false in global.ini (issue 1317): the ONLY guard against the CEF int3 crash-loop
#     (exit 133 every ~60 s) of the browser sources on this RTX/GNOME-Wayland stack. FAIL loud.
GLOBAL_INI="${USER_HOME}/.config/obs-studio/global.ini"
if grep -qE '^BrowserHWAccel=false' "$GLOBAL_INI" 2>/dev/null; then
  ok "(browser-hwaccel) global.ini [General] BrowserHWAccel=false (CEF crash-loop guard)"
else
  bad "(browser-hwaccel) ${GLOBAL_INI} missing BrowserHWAccel=false -- CEF browser sources int3 crash-loop; re-run setup-strih.sh step 7"
fi

# 27) unused obs-plugins pruned (issue 1317): decklink*.so / obs-qsv11.so / obs-vst.so must be ABSENT
#     in BOTH the /opt bundle copy AND the /usr-prefix copy the running OBS loads. The prune LIST +
#     the two DIRS are the SAME source of truth setup-strih.sh step 4 prunes with. FAIL loud.
PLUGIN_PRESENT=""
while IFS= read -r _pdir; do
  [ -d "$_pdir" ] || continue
  while IFS= read -r _pat; do
    [ -n "$_pat" ] || continue
    # $_pat may be a glob (decklink*.so) -- UNQUOTED so it expands against the plugin dir.
    for _pf in "$_pdir"/$_pat; do
      [ -e "$_pf" ] && PLUGIN_PRESENT="${PLUGIN_PRESENT} ${_pf}"
    done
  done < <(strih_lx_obs_plugin_prune_list)
done < <(strih_lx_obs_plugin_dirs "$GENLOCK_DIR" /usr/lib/x86_64-linux-gnu)
if [ -z "$PLUGIN_PRESENT" ]; then
  ok "(plugin-prune) unused obs-plugins absent ($(strih_lx_obs_plugin_prune_list | tr '\n' ' '))"
else
  bad "(plugin-prune) dead obs-plugins still present:${PLUGIN_PRESENT} -- re-run setup-strih.sh step 4"
fi

# 28) scene-collection hygiene (issue 1317, REPORT-ONLY): count shader_filter filters + scripts-tool
#     entries in the ACTIVE collection JSON via the pure strih_collection_hygiene_verdict; NOTE only --
#     provisioning NEVER rewrites the owner's collection, it only reports (a re-import re-introduces the
#     boot popups this bake-in warns about).
OBS_BASE="${USER_HOME}/.config/obs-studio"
COLL_NAME="$(sed -n 's/^SceneCollectionFile=//p' "${OBS_BASE}/global.ini" 2>/dev/null | head -1 || true)"
COLL_JSON=""
if [ -n "$COLL_NAME" ] && [ -f "${OBS_BASE}/basic/scenes/${COLL_NAME}.json" ]; then
  COLL_JSON="${OBS_BASE}/basic/scenes/${COLL_NAME}.json"
else
  # Fallback: newest *.json (the `*.json` glob already excludes the `*.json.bak*` backups). No ls|grep.
  for _cj in "${OBS_BASE}/basic/scenes/"*.json; do
    [ -e "$_cj" ] || continue
    if [ -z "$COLL_JSON" ] || [ "$_cj" -nt "$COLL_JSON" ]; then COLL_JSON="$_cj"; fi
  done
fi
if [ -n "$COLL_JSON" ] && command -v python3 >/dev/null 2>&1; then
  HYG_COUNTS="$(python3 - "$COLL_JSON" <<'PYHY' 2>/dev/null || true
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)   # print nothing -> caller NOTEs "could not parse"
def count_id(o, val):
    n = 0
    if isinstance(o, dict):
        if o.get("id") == val:
            n += 1
        for v in o.values():
            n += count_id(v, val)
    elif isinstance(o, list):
        for v in o:
            n += count_id(v, val)
    return n
shader = count_id(d, "shader_filter")
lua = 0
mods = d.get("modules", {}) if isinstance(d, dict) else {}
st = mods.get("scripts-tool") if isinstance(mods, dict) else None
if isinstance(st, list):
    lua = len(st)
elif isinstance(st, dict):
    inner = st.get("scripts")
    lua = len(inner) if isinstance(inner, list) else (1 if st else 0)
print("%d %d" % (shader, lua))
PYHY
)"
  if [ -n "$HYG_COUNTS" ]; then
    # HYG_COUNTS is "SHADER LUA" -- word-split into the two args on purpose.
    # shellcheck disable=SC2086
    HYG_VERDICT="$(strih_collection_hygiene_verdict $HYG_COUNTS || true)"
    if [ "$HYG_VERDICT" = ok ]; then
      note "(collection-hygiene) active collection clean (0 shader_filter, 0 scripts-tool) -- report-only"
    else
      note "(collection-hygiene) active collection carries ${HYG_VERDICT} (a re-import re-introduces the boot popups; owner-reversible in the UI) -- report-only, provisioning never rewrites the collection"
    fi
  else
    note "(collection-hygiene) could not parse ${COLL_JSON} -- report-only"
  fi
else
  note "(collection-hygiene) active collection JSON / python3 absent -- report-only"
fi

# 29) RustDesk remote desktop (issue 1317, owner request 22.9.): the service is active + a connect ID
#     is readable. FAIL loud when installed-but-broken; NOTE when not installed (the pw file was not
#     placed, so setup-strih step 16b skipped it -- report-only until installed).
if command -v rustdesk >/dev/null 2>&1; then
  RD_ACTIVE="$(systemctl is-active rustdesk 2>/dev/null || true)"
  RD_ID="$(rustdesk --get-id 2>/dev/null | head -1 || true)"
  if [ "$RD_ACTIVE" = active ] && [ -n "$RD_ID" ]; then
    ok "(rustdesk) service active + connect ID present"
  else
    bad "(rustdesk) installed but not ready (active='${RD_ACTIVE:-<none>}', id='${RD_ID:-<empty>}') -- systemctl enable --now rustdesk; re-run setup-strih.sh step 16b"
  fi
else
  note "(rustdesk) not installed -- place the 0600 password file + re-run setup-strih.sh step 16b (report-only until installed)"
fi

# 6b) dantesync ROLE live check (issue 1317): :8898/status reachable AND mode LOCK/NANO AND -- for the
#     server role (the post-M4 default: the notebook IS the fleet NTP master) -- an ntp UDP :123
#     listener. Complements item 6 (unit active + fresh offset) with the role/serving-state proof via
#     the pure strih_lx_dantesync_status_role_verdict.
DS_ROLE_V="$(strih_lx_dantesync_role)"
DS_STATUS="$(curl -s --max-time 4 http://127.0.0.1:8898/status 2>/dev/null || true)"
[ -n "$DS_STATUS" ] && DS_REACH=1 || DS_REACH=0
DS_MODE="$(printf '%s' "$DS_STATUS" | grep -oE '"mode":"[A-Za-z]+"' | head -1 | sed 's/.*:"//; s/"//' || true)"
[ -n "$DS_MODE" ] || DS_MODE=absent
# Match the PORT column only (field 4 = Local-Address:Port), not the whole line -- an IPv6 address
# containing the hextet `123` (e.g. fe80::123:abcd) would false-match a whole-line grep. Capture then
# grep a here-string (no upstream pipe to SIGPIPE the `ss|awk` under pipefail); `|| true` is drain-safe.
DS_UDP_PORTS="$(ss -uln 2>/dev/null | awk 'NR>1{print $4}' || true)"
if grep -qE ':123$' <<<"$DS_UDP_PORTS"; then DS_UDP123=1; else DS_UDP123=0; fi
DS_ROLE_VERDICT="$(strih_lx_dantesync_status_role_verdict "$DS_ROLE_V" "$DS_REACH" "$DS_MODE" "$DS_UDP123" || true)"
case "$DS_ROLE_VERDICT" in
  ok)
    if [ "$DS_ROLE_V" = server ]; then
      ok "(dantesync-role) server: :8898/status mode=${DS_MODE} + NTP :123 listener (fleet NTP master)"
    else
      ok "(dantesync-role) client: :8898/status mode=${DS_MODE}"
    fi ;;
  unreachable)     bad "(dantesync-role) :8898/status not answering -- dantesync down / no HTTP status" ;;
  no-ntp-listener) bad "(dantesync-role) server role but NO UDP :123 listener -- the fleet's NTP master is not serving NTP; re-run setup-strih.sh step 2" ;;
  mode:*)          bad "(dantesync-role) :8898/status ${DS_ROLE_VERDICT} (not LOCK/NANO) -- clock not disciplined" ;;
  *)               bad "(dantesync-role) unknown verdict '${DS_ROLE_VERDICT}'" ;;
esac

# 30) ffmpeg/ffprobe present (issue 1317): the on-box recording-verdict E2E spawns ffprobe to demux
#     the strih recording; without ffmpeg the [8/8] on-box verdict fails. FAIL loud (release gate).
FFMPEG_MISSING=""
for _t in ffprobe ffmpeg; do
  command -v "$_t" >/dev/null 2>&1 || FFMPEG_MISSING="${FFMPEG_MISSING} ${_t}"
done
if [ -z "$FFMPEG_MISSING" ]; then
  ok "(ffmpeg) ffprobe + ffmpeg present (on-box recording-verdict E2E)"
else
  bad "(ffmpeg) missing:${FFMPEG_MISSING} -- the on-box recording-verdict E2E cannot demux the recording; re-run setup-strih.sh step 4b (apt install ffmpeg)"
fi

# 31) bkshading shading-control service (issue 1353): the panel backend, provisioned ENABLE-ONLY by
#     setup-strih.sh step 16c (the SUPERVISOR deploys the running service; the Windows service is the
#     fallback until the owner accepts). Grade like intercom-hub/janus: an installed + enabled but
#     INACTIVE unit is the CORRECT enable-only state (report-only); an ACTIVE unit is asserted HARD --
#     the :8770 listener OWNER must be the bkshading binary (the rule's "confirm the listener's owner"
#     check via ss -tlnp) AND /api/version must answer (the SERVICE's route -- /api/state is the
#     relay's / intercom hub's endpoint, :8770/api/state is 404), else the service is broken (FAIL loud).
BKSH_PORT="${BKSHADING_SERVICE_PORT:-8770}"
if [ -f /etc/systemd/system/bkshading-service.service ]; then
  BKSH_EN="$(systemctl is-enabled bkshading-service 2>/dev/null || echo unknown)"
  BKSH_ACT="$(systemctl is-active bkshading-service 2>/dev/null || echo inactive)"
  if [ "$BKSH_ACT" = active ]; then
    # Capture ss output first, then grep a here-string (no upstream pipe to SIGPIPE under pipefail).
    BKSH_SS="$(ss -tlnp 2>/dev/null | grep -E ":${BKSH_PORT} " || true)"
    BKSH_OWNER="$(printf '%s\n' "$BKSH_SS" | strih_bkshading_listener_owner || true)"
    BKSH_API="$(curl -fsS --max-time 3 "http://127.0.0.1:${BKSH_PORT}/api/version" 2>/dev/null || true)"
    if [ "$BKSH_OWNER" = bkshading ] && [ -n "$BKSH_API" ]; then
      ok "(bkshading-service) active: :${BKSH_PORT} listener owned by bkshading + /api/version answers"
    else
      bad "(bkshading-service) active but unhealthy (:${BKSH_PORT} owner='${BKSH_OWNER:-none}', /api/version=$([ -n "$BKSH_API" ] && echo answered || echo silent)) -- the running service is broken; check journalctl -u bkshading-service"
    fi
  else
    note "(bkshading-service) installed (enabled=${BKSH_EN}, active=${BKSH_ACT}); enable-only until the supervisor deploys the running service (issue 1353) -- report-only, an inactive unit is correct"
  fi
else
  bad "(bkshading-service) unit /etc/systemd/system/bkshading-service.service not installed -- re-run setup-strih.sh step 16c (issue 1353)"
fi

# 16) NIC xhci IRQ affinity (issue 1317 item H): the USB-NIC's xhci interrupt must be pinned to a
#     SINGLE E-core (>= the first cpu_atom cpu) so its NET_RX softirq never shares an OBS core, AND
#     that IRQ's /proc/interrupts counter must be ADVANCING over a live 2-s window (NEVER a static
#     file check -- a smp_affinity_list read alone is a lying gate). Read-only, drain-safe, fail loud.
IRQ_TARGET_IP="${STRIH_LX_TARGET_IP:-$(strih_lx_ip)}"
IRQ_IFACE="${STRIH_NIC_IFACE:-}"
if [ -z "$IRQ_IFACE" ]; then
  # driver-first (the box's STRIH_NIC_DRIVER fact -- strih-lx: the r8152 USB NIC), then fall back to
  # the address match; MULTI -> NOTE + no iface.
  IRQ_DRV="$(strih_nic_iface_by_driver /sys "$(strih_lx_nic_driver)" 2>/dev/null || true)"
  case "$IRQ_DRV" in
    MULTI:*) note "  multiple $(strih_lx_nic_driver) NICs (${IRQ_DRV#MULTI:}) -- set STRIH_NIC_IFACE"; IRQ_IFACE="" ;;
    "")      IRQ_IFACE="$(ip -o -4 addr show 2>/dev/null | awk -v ip="$IRQ_TARGET_IP" 'BEGIN { gsub(/\./, "\\.", ip) } $4 ~ ("^" ip "/") { print $2; exit }' || true)" ;;
    *)       IRQ_IFACE="$IRQ_DRV" ;;
  esac
fi
IRQ_PCIFN=""; [ -n "$IRQ_IFACE" ] && IRQ_PCIFN="$(strih_nic_xhci_pci_function /sys "$IRQ_IFACE" 2>/dev/null || true)"
IRQ_NUMS="";  [ -n "$IRQ_PCIFN" ] && IRQ_NUMS="$(strih_nic_xhci_irqs /proc/interrupts "$IRQ_PCIFN" 2>/dev/null || true)"
if [ -z "$IRQ_IFACE" ] || [ -z "$IRQ_PCIFN" ] || [ -z "$IRQ_NUMS" ]; then
  bad "NIC xhci IRQ affinity: could not resolve the xhci IRQ (iface='${IRQ_IFACE}' pcifn='${IRQ_PCIFN}' irqs='${IRQ_NUMS}')"
else
  ATOM_FIRST="$(strih_cpulist_min "$(cat /sys/devices/cpu_atom/cpus 2>/dev/null || true)" 2>/dev/null || true)"
  # On this hybrid box cpu_atom is a stable kernel file; if it is unreadable the verdict skips the
  # E-core floor (>= first cpu_atom) and enforces only single-cpu placement -- surface that so a
  # low-P-core pin can't read "ok" silently on a transient cpu_atom read failure.
  [ -n "$ATOM_FIRST" ] || note "  cpu_atom unreadable -- E-core floor check skipped (single-cpu placement still enforced)"
  irq_all_ok=1
  for irqn in $IRQ_NUMS; do
    AFF="$(cat "/proc/irq/${irqn}/smp_affinity_list" 2>/dev/null || true)"
    VERD="$(strih_nic_irq_affinity_verdict "$AFF" "$ATOM_FIRST" 2>/dev/null || true)"
    C1="$(strih_irq_total_count /proc/interrupts "$irqn" 2>/dev/null || true)"
    sleep 2
    C2="$(strih_irq_total_count /proc/interrupts "$irqn" 2>/dev/null || true)"
    ADV=no; strih_counter_advanced "$C1" "$C2" && ADV=yes || true
    if [ "$VERD" = ok ] && [ "$ADV" = yes ]; then
      : # this IRQ passes both the single-E-core placement and the advancing-counter liveness
    else
      irq_all_ok=0
      note "  IRQ ${irqn}: affinity='${AFF}' verdict=${VERD} advancing=${ADV} (want a single cpu >= ${ATOM_FIRST}, advancing)"
    fi
  done
  if [ "$irq_all_ok" = 1 ]; then
    ok "NIC xhci IRQ(s) [${IRQ_NUMS}] pinned to a single E-core (>= cpu ${ATOM_FIRST}) and advancing (iface ${IRQ_IFACE})"
  else
    bad "NIC xhci IRQ affinity FAILED (iface ${IRQ_IFACE}, irqs ${IRQ_NUMS}): each must be a single cpu >= first cpu_atom (${ATOM_FIRST}) AND advancing over 2 s"
  fi
fi

# 35) Downstream Keyer OBS plugin (issue 1361): setup-strih.sh step 4c installs the pinned upstream
#     plugin into the /usr prefix OBS loads; FAIL unless the installed .so has the pinned sha256 (the
#     exact file strih-lx ran by hand before) and its locale is present. Placed BEFORE item 34: the
#     item-34 behaviour test runs the text from "# 34)" up to "# 32)" with only ndi-discovery.sh
#     sourced, and nothing may sit after item 33 (its test slices "# 33)" to the closing summary).
_dsk_so="$(obs_dsk_so_path "${STRIH_LIBDIR:-/usr/lib/x86_64-linux-gnu}")"
_dsk_sha="$(sha256sum "$_dsk_so" 2>/dev/null | awk '{print $1}' || true)"
_dsk_loc=0
[ -f "$(obs_dsk_data_dir "${STRIH_SHAREDIR:-/usr/share}")/locale/en-US.ini" ] && _dsk_loc=1
_dsk_verdict="$(obs_dsk_verdict "$_dsk_sha" "$(obs_dsk_so_sha256)" "$_dsk_loc")" || true
if [ "$_dsk_verdict" = ok ]; then
  ok "(downstream-keyer) $(obs_dsk_version) installed at ${_dsk_so} (pinned sha256)"
else
  bad "(downstream-keyer) ${_dsk_verdict#FAIL: } at ${_dsk_so} -- re-run setup-strih.sh step 4c (issue 1361)"
fi

# 34) NDI discovery receiver config (issue 1342): networks.ips lists every PINNED managed NDI sender
#     (the SAME scripts/lib/ndi-discovery.sh generator setup-strih.sh step 4b writes with -- every
#     camera + strih-lx/stream; the traveling resolume hostname is best-effort, never required) and
#     carries no networks.discovery, in BOTH readers' config -- the desktop user's ~/.ndi (OBS +
#     bkshading-service) and the system dir /etc/ndi (intercom-hub, whose ProtectHome hides ~/.ndi) --
#     plus the intercom-hub NDI_CONFIG_DIR drop-in. Read-only; FAIL on any miss (a renumbered sender
#     FAILs until the box is re-provisioned). Placed BEFORE item 32: the item-33 test slices "# 33)"
#     to the closing summary, so nothing may sit after item 33.
_ndi_required="$(ndi_discovery_sender_ips pinned)" || _ndi_required=""
_ndi_dropin_dir="$(ndi_discovery_dropin_config_dir "$(cat "$NDI_DISCOVERY_INTERCOM_DROPIN" 2>/dev/null || true)")"
if [ -z "$_ndi_required" ]; then
  bad "(ndi-discovery) could not generate the managed NDI sender list (camera-set.sh + obs-fleet.sh ndi-sender facet) -- both configs ungraded"
else
  for _ndi_dir in "${USER_HOME}/.ndi" "$NDI_DISCOVERY_SYSTEM_DIR"; do
    _ndi_text="$(cat "${_ndi_dir}/${NDI_DISCOVERY_CONFIG_NAME}" 2>/dev/null || true)"
    _ndi_verdict="$(ndi_discovery_config_verdict "$_ndi_text" "$_ndi_required")"
    if [ "$_ndi_verdict" = ok ]; then
      ok "(ndi-discovery) ${_ndi_dir}/${NDI_DISCOVERY_CONFIG_NAME}: networks.ips lists every managed sender"
    else
      bad "(ndi-discovery) ${_ndi_dir}/${NDI_DISCOVERY_CONFIG_NAME}: $(printf '%s' "$_ndi_verdict" | tr '\n' ' ' | sed 's/FAIL: //g')-- re-run setup-strih.sh step 4b (issue 1342)"
    fi
  done
  # A traveling sender (resolume.lan, DHCP) is never REQUIRED, but when it resolves NOW to an address
  # the OBS config does not list (it was away at the last setup-strih run, or its lease moved), say so:
  # a NOTE, never a FAIL -- re-running setup-strih.sh picks it up.
  # Only the TRAVELING part (resolved minus pinned) is diffed -- a missing PINNED sender is already a
  # FAIL above, never re-reported here as "traveling".
  _ndi_resolved="$(ndi_discovery_sender_ips resolve 2>/dev/null)" || _ndi_resolved=""
  _ndi_traveling="$(ndi_discovery_list_minus "$_ndi_resolved" "$_ndi_required")"
  _ndi_drift="$(ndi_discovery_missing_ips "$(cat "${USER_HOME}/.ndi/${NDI_DISCOVERY_CONFIG_NAME}" 2>/dev/null || true)" "$_ndi_traveling")"
  if [ -n "$_ndi_drift" ]; then
    note "(ndi-discovery) a traveling NDI sender resolves now to ${_ndi_drift}, which ${USER_HOME}/.ndi/${NDI_DISCOVERY_CONFIG_NAME} does not list -- re-run setup-strih.sh step 4b to add it (issue 1342)"
  fi
fi
if [ "$_ndi_dropin_dir" = "$NDI_DISCOVERY_SYSTEM_DIR" ]; then
  ok "(ndi-discovery) intercom-hub NDI_CONFIG_DIR=${NDI_DISCOVERY_SYSTEM_DIR} drop-in present"
else
  bad "(ndi-discovery) ${NDI_DISCOVERY_INTERCOM_DROPIN} missing or NDI_CONFIG_DIR='${_ndi_dropin_dir:-<none>}' (want ${NDI_DISCOVERY_SYSTEM_DIR}) -- re-run setup-strih.sh step 4b (issue 1342)"
fi

# 32) the shared OBS-box appliance baseline (issue 1357) -- the ONE grader verify-imag.sh runs too
#     (scripts/lib/obs-box-baseline-verify.sh): network tuning, governor + strih-maxperf persistence,
#     the PM QoS CPU idle wake-up latency bound (`cstate`), never-sleep, boot safety net,
#     preempt=full low-latency kernel, AFFINITY-ONLY core reservation,
#     PRIME nvidia-primary, de-jitter, no operator crash popups, the lightdm -> openbox Xorg kiosk with
#     GNOME purged, the openbox autostart contract, the power envelope, the touchpad InputClass. One
#     PASS/FAIL line per item; a box missing ANY item FAILS (it supersedes the old never-sleep,
#     governor and crash-popup items).
BASELINE_FACTS="$(bash -c "$(obs_box_baseline_gather_snippet strih "${STRIH_LX_USER:-newlevel}" strih-obs.service)" 2>/dev/null || true)"
while IFS='|' read -r _bl_item _bl_state _bl_detail; do
  [ -n "$_bl_item" ] || continue
  if [ "$_bl_state" = OK ]; then
    ok "(baseline:${_bl_item}) ${_bl_detail}"
  else
    bad "(baseline:${_bl_item}) ${_bl_detail} -- re-run setup-strih.sh step 11 (the shared OBS-box baseline) and reboot"
  fi
done < <(obs_box_baseline_verdict <<<"$BASELINE_FACTS" || true)
CRASH_DIR_V="${STRIH_CRASH_DIR:-/var/crash}"
CRASH_REPORTS_V="$(obs_box_crash_reports_count "$CRASH_DIR_V")"
if [ "$CRASH_REPORTS_V" != 0 ]; then
  note "(crash-popup) ${CRASH_REPORTS_V} stale crash report(s) in ${CRASH_DIR_V} -- update-notifier re-raises the popup for them at login; inspect (coredumpctl list) and clear them (supervisor data action)"
fi

# 33) NO realtime-priority grant (issue 1357 design): the render-tick SCHED_FIFO pin assumed a reserved
#     core and its FIFO + affinity leaked to 28 NDI threads on strih-lx (issue comment 5793075833), so
#     the baseline keeps rtprio OFF. FAIL while the retired grant file still exists (setup-strih.sh step
#     11 removes it; it only takes effect for OBS after a reboot, so reboot once after removing it).
RTPRIO_LEFTOVER_V="$(strih_rtprio_leftover_path)"
if [ -e "$RTPRIO_LEFTOVER_V" ]; then
  bad "(rtprio-off) ${RTPRIO_LEFTOVER_V} still grants realtime priority -- rtprio must stay OFF (issue 1357); re-run setup-strih.sh step 11 (it removes it) and reboot"
else
  ok "(rtprio-off) no realtime-priority grant (${RTPRIO_LEFTOVER_V} absent)"
fi

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}=== verify-strih.sh: ALL CLEAR ===${NC}"; exit 0
else
  echo -e "${RED}=== verify-strih.sh: ${FAILS} gate item(s) FAILED ===${NC}"; exit 1
fi
