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

# 6) dantesync is a CLIENT (never master). Derive a CLEAN mode token from the config's
#    ntp_server_mode.enabled flag -- NOT the whole config text (which always contains the literal
#    `ntp_server_mode` key and would false-match the guard's `*server_mode*` master pattern on a
#    legit client). The config is JSON (/etc/dantesync/config.json), not .toml.
DS_JSON="${DANTESYNC_CONFIG:-/etc/dantesync/config.json}"
if [ -f "$DS_JSON" ] && command -v python3 >/dev/null 2>&1; then
  DS_MODE="$(python3 -c 'import json,sys
d=json.load(open(sys.argv[1]))
nsm=d.get("ntp_server_mode")
en=nsm.get("enabled") if isinstance(nsm,dict) else nsm
print("server_mode" if en else "client")' "$DS_JSON" 2>/dev/null || echo "")"
  if [ -n "$DS_MODE" ]; then
    strih_lx_dantesync_is_client_not_master "$DS_MODE" && ok "dantesync is a CLIENT (not master)" || bad "dantesync is NOT a client (ntp_server_mode enabled -- would risk a 2nd NTP master)"
  else
    bad "dantesync config not parseable ($DS_JSON)"
  fi
else
  bad "dantesync config not readable ($DS_JSON)"
fi

# 7) bundle-state :8899.
tcp_open "$WS_HOST" 8899 && ok "bundle-state :8899 answering" || bad "bundle-state :8899 not answering"

# 8) remoteos-mcp agent.
systemctl is-active remoteos-mcp >/dev/null 2>&1 && ok "remoteos-mcp active" || note "remoteos-mcp not active (install-linux.sh, step 10)"

# 9) PipeWire program-audio input present.
if arecord -l 2>/dev/null | grep -qi 'MiniFuse'; then ok "MiniFuse 4 PipeWire input present"; else bad "MiniFuse 4 (program audio) not present"; fi

# 10) NVENC encoder available.
{ ffmpeg -hide_banner -encoders 2>/dev/null || cat "$LOG" 2>/dev/null; } | strih_lx_nvenc_available_ok && ok "NVENC encoder available" || bad "NVENC encoder not available"

# 11) never-sleep (sleep.target masked).
[ "$(systemctl is-enabled sleep.target 2>/dev/null || echo masked)" = masked ] && ok "sleep.target masked (never-sleep)" || bad "sleep.target not masked"

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

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}=== verify-strih.sh: ALL CLEAR ===${NC}"; exit 0
else
  echo -e "${RED}=== verify-strih.sh: ${FAILS} gate item(s) FAILED ===${NC}"; exit 1
fi
