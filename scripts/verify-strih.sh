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
# Usage (on the box):  ./verify-strih.sh   [STRIH_LX_HOST / OBS_WS_HOST override the WS target]

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/strih-provision.sh
. "${HERE}/lib/strih-provision.sh"
# issue 1317: the dantesync item grades a FRESH offset via the SHARED freshness-aware verdict (the
# cambox verify-device (d) shape) instead of reading a Windows/imag dantesync JSON config file a
# flag-based Linux client never creates. clock-offset-guard.sh has its own source-guard, so sourcing
# it defines only its pure functions (dantesync_offset_verdict / ptp_locked_from_journal).
# shellcheck source=scripts/clock-offset-guard.sh
. "${HERE}/clock-offset-guard.sh"

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

echo -e "${GREEN}=== verify-strih.sh (issue 1317) acceptance gate ===${NC}"

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
if systemctl --user is-active strih-obs.service >/dev/null 2>&1 || pgrep -x obs >/dev/null 2>&1 || pgrep -f 'bin/64bit/obs\|/obs$' >/dev/null 2>&1; then
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
    [ "$all_ns" = 1 ] && ok "all declared NDI outputs are STRIH-LX-namespaced" || bad "a declared NDI output is not STRIH-LX-namespaced"
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

# 4c) issue 1346: fixed HDMI fullscreen projector -- REPORT-ONLY (the live open needs an HDMI display
#     on the notebook, a supervisor/owner rig step, so this NEVER hard-FAILs). Reports via the pure
#     strih_projector_verdict: SaveProjectors=true pre-seeded in user.ini; and -- when an external
#     HDMI/DP monitor is connected -- a saved ProjectorType 3/4 entry in a scene collection.
OBS_CFG_DIR_V="$(dirname "$OBS_LOG_DIR")"   # OBS_LOG_DIR is <cfg>/logs -> the cfg dir is its parent
PROJ_USER_INI="${OBS_CFG_DIR_V}/user.ini"
SAVEPROJ=0
[ -f "$PROJ_USER_INI" ] && grep -qi '^SaveProjectors=true' "$PROJ_USER_INI" && SAVEPROJ=1
EXT_CONN=0
for _st in /sys/class/drm/card*-HDMI*/status /sys/class/drm/card*-DP*/status; do
  [ -f "$_st" ] || continue
  if [ "$(cat "$_st" 2>/dev/null)" = connected ]; then EXT_CONN=1; break; fi
done
SAVED_ENTRY=0
if command -v python3 >/dev/null 2>&1; then
  SAVED_ENTRY="$(python3 - "$OBS_CFG_DIR_V" <<'PY'
import glob, json, os, sys
cfg = sys.argv[1]
found = 0
for path in glob.glob(os.path.join(cfg, "basic", "scenes", "*.json")):
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError):
        continue
    for p in (d.get("saved_projectors") or []):
        if isinstance(p, dict) and p.get("type") in (3, 4):
            found = 1
            break
    if found:
        break
print(found)
PY
)"
fi
PROJ_VERDICT="$(strih_projector_verdict "$SAVEPROJ" "$EXT_CONN" "${SAVED_ENTRY:-0}" || true)"
case "$PROJ_VERDICT" in
  ok)                     ok   "fixed HDMI projector: SaveProjectors + external monitor + a saved ProjectorType 3/4" ;;
  saveprojectors-missing) note "fixed HDMI projector: SaveProjectors=true NOT pre-seeded in ${PROJ_USER_INI} (re-run setup-strih.sh step 7)" ;;
  hdmi-absent)            note "fixed HDMI projector: SaveProjectors ok; HDMI display not connected (report-only -- plug a display into HDMI for the live projector)" ;;
  projector-unseeded)     note "fixed HDMI projector: external monitor present but no saved projector yet (strih_scenes.py --bootstrap opens it on the next launch)" ;;
  *)                      note "fixed HDMI projector: unknown verdict '${PROJ_VERDICT}'" ;;
esac

# 5) Certified latency pins vs scripts/latency-pins-baseline.json (strih-lx key) -- REPORT-ONLY.
BASELINE="${HERE}/latency-pins-baseline.json"
if command -v python3 >/dev/null 2>&1 && [ -f "$BASELINE" ]; then
  FLOOR="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); b=d.get("strih-lx",{}); print(b.get("_all_camera_ndi_inputs_ms","?"))' "$BASELINE" 2>/dev/null || echo '?')"
  note "latency-pins-baseline.json strih-lx floor = ${FLOOR} ms (report-only; aligner owns any offset)"
else
  note "latency baseline / python3 absent -- pin verify is report-only"
fi

# 6) dantesync unit ACTIVE + a FRESH in-bound clock offset (issue 1317 -- the cambox verify-device
#    (d) shape). A flag-based Linux client has NO dantesync JSON config file (that is a Windows/imag
#    artifact), so this asserts the RUNNING state: the unit is active AND the journal shows a fresh
#    offset within bound via the SHARED dantesync_offset_verdict/freshest_offset_us. A `stale`/`absent`
#    offset with the PTP servo LOCKED is disciplined near-zero (the #550 reasoning) -> PASS; not
#    locked -> FAIL (no trustworthy clock signal). setup-strih.sh step 2 already fail-closes the unit's
#    ExecStart to a CLIENT invocation, so a 2nd-master risk is guarded at install time, not here.
DS_ACTIVE="$(systemctl is-active dantesync 2>/dev/null || true)"
if [ "$DS_ACTIVE" != active ]; then
  bad "dantesync.service not active (state='${DS_ACTIVE:-<none>}') -- clock undisciplined/free-running"
else
  DS_JOURNAL="$(journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null || true)"
  case "$(dantesync_offset_verdict "$DS_JOURNAL" "${DANTESYNC_OFFSET_FRESHNESS_S:-300}" "${CLOCK_GUARD_BOUND_US:-2000}" "${DANTESYNC_STABILITY_US:-2000}")" in
    ok)
      ok "dantesync active + FRESH clock offset within ${CLOCK_GUARD_BOUND_US:-2000}us bound" ;;
    stale|absent)
      if [ "$(ptp_locked_from_journal "$DS_JOURNAL")" = LOCKED ]; then
        ok "dantesync active + PTP servo LOCKED (no fresh [NTP] line; offset disciplined near-zero, #550)"
      else
        bad "dantesync active but NO fresh clock offset and PTP servo not LOCKED -- no trustworthy clock signal"
      fi ;;
    *)
      bad "dantesync clock offset OUTSIDE the ${CLOCK_GUARD_BOUND_US:-2000}us bound / unstable -- a REAL clock desync" ;;
  esac
fi

# 7) bundle-state :8899.
tcp_open "$WS_HOST" 8899 && ok "bundle-state :8899 answering" || bad "bundle-state :8899 not answering"

# 8) remoteos-mcp agent.
systemctl is-active remoteos-mcp >/dev/null 2>&1 && ok "remoteos-mcp active" || note "remoteos-mcp not active (install-linux.sh, step 10)"

# 9) PipeWire program audio (issue 1344): the DERIVED verdict — the strih-program null sink + the OBS
#    `ASIO zvuk` pulse_input_capture + (when the hub is active) the program-feed rx. The FOH-live
#    level is a SUPERVISOR live-acceptance step, so verify passes FOH=unknown here (never a level
#    FAIL); a FAIL means a structural provisioning gap (sink/input absent) or, once the hub is
#    active, no program rx.
AUDIO_SINK=0
if command -v pw-cli >/dev/null 2>&1 && pw-cli ls Node 2>/dev/null | grep -q '"strih-program"'; then AUDIO_SINK=1; fi
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

# 10) NVENC encoder available.
{ ffmpeg -hide_banner -encoders 2>/dev/null; cat "$LOG" 2>/dev/null; } | strih_lx_nvenc_available_ok && ok "NVENC encoder available" || bad "NVENC encoder not available"

# 11) never-sleep (sleep.target masked). issue 1317: `systemctl is-enabled` prints "masked" AND exits
#     1 for a masked unit, so the old echo-masked-on-failure fallback DOUBLE-appended ("masked" twice) and
#     FALSE-FAILED a correctly-masked box. strih_verify_sleep_masked grades the FIRST line only.
SLEEP_STATE="$(systemctl is-enabled sleep.target 2>/dev/null || true)"
strih_verify_sleep_masked "$SLEEP_STATE" && ok "sleep.target masked (never-sleep)" || bad "sleep.target not masked (is-enabled='${SLEEP_STATE//$'\n'/|}')"

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

# 18) CPU performance governor on every online core + sleep masked (issue 1317 -- low-latency
#     genlock cutter). The (perf) item: `cat` all cores' scaling_governor -> every line must be
#     `performance` (strih_verify_governor_ok, fail-closed on an empty/unreadable read), AND
#     sleep.target masked (the existing strih_verify_sleep_masked). FAIL loud, like the other items.
GOVS="$(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null || true)"
PERF_SLEEP_STATE="$(systemctl is-enabled sleep.target 2>/dev/null || true)"
if printf '%s\n' "$GOVS" | strih_verify_governor_ok && strih_verify_sleep_masked "$PERF_SLEEP_STATE"; then
  ok "(perf) CPU governor performance on all online cores + sleep.target masked"
else
  bad "(perf) CPU not in performance mode on all cores (or sleep.target not masked) -- re-run setup-strih.sh step 15 (governors: ${GOVS//$'\n'/,} ; sleep is-enabled: ${PERF_SLEEP_STATE:-<none>})"
fi

# 19) Bitfocus Companion Satellite installed + desktop udev rule + operator autostart + controller
#     host seeded (issue 1317 rework -- the (companion) item, DESKTOP tarball model). /opt binary +
#     /etc/udev/rules.d/50-satellite-desktop.rules + operator autostart .desktop + the seeded app
#     config.json's remoteIp == the controller (strih_companion_verdict, fail-closed). FAIL loud on
#     any missing part.
CS_INSTALLED=0; [ -x "$(strih_companion_satellite_bin)" ] && CS_INSTALLED=1
CS_UDEV_OK=0; [ -f "$(strih_companion_satellite_udev_rule)" ] && CS_UDEV_OK=1
CS_AUTOSTART="${USER_HOME}/.config/autostart/companion-satellite.desktop"
CS_AUTOSTART_OK=0; [ -f "$CS_AUTOSTART" ] && CS_AUTOSTART_OK=1
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
  ok "(companion) Companion Satellite ${CS_VERDICT} -- /opt + desktop udev rule + operator autostart + controller $(strih_companion_satellite_host) (REST running=${CS_RUNNING} connected=${CS_CONNECTED})"
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

# 22) strih-mv-host projector-host helper (issue 1352): the --user unit must be installed + ENABLED
#     (it re-hosts every OBS projector as a child window so the RTX render path does not stall
#     0.5 s/present), the helper present + executable, and python3-xlib importable. FAIL loud -- the
#     RTX render path depends on it until the vendored child-display projector fix lands. Enablement
#     is read from the WantedBy=default.target symlink (no --user session bus needed at verify time).
MVH_UNIT="${USER_HOME}/.config/systemd/user/strih-mv-host.service"
MVH_WANTS="${USER_HOME}/.config/systemd/user/default.target.wants/strih-mv-host.service"
MVH_HELPER="/usr/local/bin/strih-mv-host.py"
MVH_XLIB=MISSING; python3 -c "import Xlib" 2>/dev/null && MVH_XLIB=ok
if [ -f "$MVH_UNIT" ] && [ -L "$MVH_WANTS" ] && [ -x "$MVH_HELPER" ] && [ "$MVH_XLIB" = ok ]; then
  ok "(mv-host) strih-mv-host.service installed + enabled + helper present + python3-xlib importable"
else
  bad "(mv-host) strih-mv-host gate: unit=$( [ -f "$MVH_UNIT" ] && echo present || echo MISSING ) enabled=$( [ -L "$MVH_WANTS" ] && echo yes || echo no ) helper=$( [ -x "$MVH_HELPER" ] && echo present || echo MISSING ) xlib=${MVH_XLIB} -- re-run setup-strih.sh step 8b"
fi

# 23) strih-obs-start.sh RTX GPU env (issue 1352): the DEPLOYED wrapper must carry all 4 XWayland-PRIME
#     exports (setup-strih step 8 substitutes strih_lx_obs_gpu_env into the @STRIH_LX_OBS_GPU_ENV@
#     marker). Without them OBS renders on the saturated iGPU. FAIL loud; also FAIL if the marker
#     survived un-substituted (a verbatim install).
DEPLOYED_WRAPPER="${STRIH_LAUNCHER_BIN_DIR:-/usr/local/bin}/strih-obs-start.sh"
GPU_MISSING=""
for _ex in 'QT_QPA_PLATFORM=xcb' '__NV_PRIME_RENDER_OFFLOAD=1' '__GLX_VENDOR_LIBRARY_NAME=nvidia' '__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json'; do
  grep -qF "$_ex" "$DEPLOYED_WRAPPER" 2>/dev/null || GPU_MISSING="${GPU_MISSING} ${_ex}"
done
if [ -z "$GPU_MISSING" ] && ! grep -qF '#@STRIH_LX_OBS_GPU_ENV@' "$DEPLOYED_WRAPPER" 2>/dev/null; then
  ok "(gpu-env) strih-obs-start.sh carries the 4 RTX XWayland-PRIME exports (marker substituted)"
else
  _mk=""; grep -qF '#@STRIH_LX_OBS_GPU_ENV@' "$DEPLOYED_WRAPPER" 2>/dev/null && _mk=" (marker NOT substituted)"
  bad "(gpu-env) ${DEPLOYED_WRAPPER} missing RTX exports:${GPU_MISSING:- none}${_mk} -- re-run setup-strih.sh step 8"
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
DS_ROLE_V="${STRIH_LX_DANTESYNC_ROLE:-server}"
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
#     check via ss -tlnp) AND /api/state must answer, else the running service is broken (FAIL loud).
BKSH_PORT="${BKSHADING_SERVICE_PORT:-8770}"
if [ -f /etc/systemd/system/bkshading-service.service ]; then
  BKSH_EN="$(systemctl is-enabled bkshading-service 2>/dev/null || echo unknown)"
  BKSH_ACT="$(systemctl is-active bkshading-service 2>/dev/null || echo inactive)"
  if [ "$BKSH_ACT" = active ]; then
    # Capture ss output first, then grep a here-string (no upstream pipe to SIGPIPE under pipefail).
    BKSH_SS="$(ss -tlnp 2>/dev/null | grep -E ":${BKSH_PORT} " || true)"
    BKSH_OWNER="$(printf '%s\n' "$BKSH_SS" | strih_bkshading_listener_owner || true)"
    BKSH_API="$(curl -fsS --max-time 3 "http://127.0.0.1:${BKSH_PORT}/api/state" 2>/dev/null || true)"
    if [ "$BKSH_OWNER" = bkshading ] && [ -n "$BKSH_API" ]; then
      ok "(bkshading-service) active: :${BKSH_PORT} listener owned by bkshading + /api/state answers"
    else
      bad "(bkshading-service) active but unhealthy (:${BKSH_PORT} owner='${BKSH_OWNER:-none}', /api/state=$([ -n "$BKSH_API" ] && echo answered || echo silent)) -- the running service is broken; check journalctl -u bkshading-service"
    fi
  else
    note "(bkshading-service) installed (enabled=${BKSH_EN}, active=${BKSH_ACT}); enable-only until the supervisor deploys the running service (issue 1353) -- report-only, an inactive unit is correct"
  fi
else
  bad "(bkshading-service) unit /etc/systemd/system/bkshading-service.service not installed -- re-run setup-strih.sh step 16c (issue 1353)"
# 16) NIC xhci IRQ affinity (issue 1317 item H): the USB-NIC's xhci interrupt must be pinned to a
#     SINGLE E-core (>= the first cpu_atom cpu) so its NET_RX softirq never shares an OBS core, AND
#     that IRQ's /proc/interrupts counter must be ADVANCING over a live 2-s window (NEVER a static
#     file check -- a smp_affinity_list read alone is a lying gate). Read-only, drain-safe, fail loud.
IRQ_TARGET_IP="${STRIH_LX_TARGET_IP:-10.77.9.202}"
IRQ_IFACE="${STRIH_NIC_IFACE:-}"
if [ -z "$IRQ_IFACE" ]; then
  IRQ_IFACE="$(ip -o -4 addr show 2>/dev/null | awk -v ip="$IRQ_TARGET_IP" '$4 ~ ("^" ip "/") { print $2; exit }' || true)"
fi
IRQ_PCIFN=""; [ -n "$IRQ_IFACE" ] && IRQ_PCIFN="$(strih_nic_xhci_pci_function /sys "$IRQ_IFACE" 2>/dev/null || true)"
IRQ_NUMS="";  [ -n "$IRQ_PCIFN" ] && IRQ_NUMS="$(strih_nic_xhci_irqs /proc/interrupts "$IRQ_PCIFN" 2>/dev/null || true)"
if [ -z "$IRQ_IFACE" ] || [ -z "$IRQ_PCIFN" ] || [ -z "$IRQ_NUMS" ]; then
  bad "NIC xhci IRQ affinity: could not resolve the xhci IRQ (iface='${IRQ_IFACE}' pcifn='${IRQ_PCIFN}' irqs='${IRQ_NUMS}')"
else
  ATOM_FIRST="$(strih_cpulist_min "$(cat /sys/devices/cpu_atom/cpus 2>/dev/null || true)" 2>/dev/null || true)"
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

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}=== verify-strih.sh: ALL CLEAR ===${NC}"; exit 0
else
  echo -e "${RED}=== verify-strih.sh: ${FAILS} gate item(s) FAILED ===${NC}"; exit 1
fi
