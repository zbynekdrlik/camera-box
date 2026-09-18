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

# 9) PipeWire program-audio input present.
if arecord -l 2>/dev/null | grep -qi 'MiniFuse'; then ok "MiniFuse 4 PipeWire input present"; else bad "MiniFuse 4 (program audio) not present"; fi

# 10) NVENC encoder available.
{ ffmpeg -hide_banner -encoders 2>/dev/null || cat "$LOG" 2>/dev/null; } | strih_lx_nvenc_available_ok && ok "NVENC encoder available" || bad "NVENC encoder not available"

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

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}=== verify-strih.sh: ALL CLEAR ===${NC}"; exit 0
else
  echo -e "${RED}=== verify-strih.sh: ${FAILS} gate item(s) FAILED ===${NC}"; exit 1
fi
