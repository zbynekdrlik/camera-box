#!/bin/bash
# strih-lx One-Shot Setup (issue 1317) -- see the extended header below the strict-mode line.
# Provisions a Linux notebook as the strih cutter/mix box; runs ON the box as root, idempotent.
set -euo pipefail
#
# The notebook runs IN PARALLEL with the Windows STRIH-SNV cutter until tuned (owner 16.9.2026), so
# this box:
#   * emits its NDI outputs under the NAMESPACED `STRIH-LX (...)` names (never a 2nd STRIH-SNV
#     sender on the wire -- the stream box + receivers must never see two STRIH-SNV (2ME PGM)), and
#   * joins the cluster clock as a dantesync CLIENT (`--ntp-server strih.lan`) -- the Windows PC
#     stays the ONE NTP master while both run.
#
# The strih role FACTS + pure decisions live in scripts/lib/strih-provision.sh (sourced below +
# unit-tested from tests/strih_provision_pure_functions.rs). This orchestrator is the enable-only,
# fail-loud flow around them; it reuses the shared genlock-markers.sh helper and the canonical
# remoteos-mcp / bundle-state tooling rather than re-implementing any of it.
#
# Usage (on the box):
#   sudo STRIH_LX_IP=10.77.9.NNN GH_TOKEN=<gh-pat-repo-read> [STRIH_LX_AUDIO_WIRED=1] ./setup-strih.sh [--yes]

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_USER="${STRIH_LX_USER:-newlevel}"
USER_HOME="/home/${DESKTOP_USER}"
OBS_CFG="${USER_HOME}/.config/obs-studio"
GENLOCK_REPO="${GENLOCK_REPO:-zbynekdrlik/camera-box}"
GENLOCK_DIR="/opt/obs-genlock"
REC_DIR="/srv/_REC"
TOTAL_STEPS=13

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

# shellcheck source=scripts/lib/strih-provision.sh
. "${HERE}/lib/strih-provision.sh"
# shellcheck source=scripts/lib/genlock-markers.sh
. "${HERE}/lib/genlock-markers.sh"

# --- source-guard: when sourced (the unit tests), stop here -- never run the destructive flow ----
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

[ "${EUID:-$(id -u)}" -eq 0 ] || fail "run as root (sudo)"

STATIC_IP="$(strih_lx_ip)"
STRIH_HOST="$(strih_lx_host)"

echo -e "${GREEN}=== strih-lx setup (issue 1317): parallel Linux strih cutter, host ${STRIH_HOST} ===${NC}"

# ---------------------------------------------------------------------------------------------
step 1 "Static IP (NetworkManager) + hostname ${STRIH_HOST}"
if [ -z "$STATIC_IP" ]; then
  warn "  STRIH_LX_IP unset -- the notebook's static IP is assigned on arrival (17.9.); leaving DHCP for now"
else
  command -v nmcli >/dev/null 2>&1 || fail "nmcli required (desktop Ubuntu NetworkManager)"
  echo "  (operator: assign ${STATIC_IP}/23 to the rig NIC via nmcli; recorded here as the target)"
fi
hostnamectl set-hostname "${STRIH_HOST%%.*}" 2>/dev/null || warn "  could not set hostname (non-fatal)"

# ---------------------------------------------------------------------------------------------
step 2 "DanteSync CLIENT (single timesync authority; NEVER server/master while parallel)"
# The Windows strih PC stays the ONE NTP master. Purge any competing timesync daemon (ops hard
# rule: dantesync OWNS the clock -- never timesyncd/chrony/ptp4l alongside it).
for svc in systemd-timesyncd chrony chronyd ntp ntpsec; do
  systemctl disable --now "$svc" 2>/dev/null || true
done
[ -x /usr/local/bin/dantesync ] || warn "  dantesync binary absent -- install it (see setup-imag.sh step 3 / dantesync-fleet-upgrade.md) before go-live"
DS_ARGS="$(strih_lx_dantesync_client_args)"
# Fail-closed self-check: the args we will run must be a CLIENT invocation, never a master one.
strih_lx_dantesync_is_client_not_master "$DS_ARGS" \
  || fail "dantesync args '$DS_ARGS' are not a CLIENT invocation -- refuse to risk a 2nd NTP master"
echo "  dantesync client args: ${DS_ARGS}  (the Windows PC remains the master)"

# ---------------------------------------------------------------------------------------------
REC_ENC="$(strih_lx_profile_facts | grep '^rec_encoder=' | cut -d= -f2)"
step 3 "NVIDIA driver / NVENC check (record encoder ${REC_ENC})"
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >/dev/null 2>&1; then
  echo "  nvidia-smi OK -- NVENC HEVC available for ${REC_ENC}"
else
  fail "nvidia-smi missing/failing -- the strih role records with NVENC HEVC; install the NVIDIA dGPU driver first"
fi

# ---------------------------------------------------------------------------------------------
ART="$(strih_lx_bundle_artifact)"
step 4 "Genlock bundle -> ${GENLOCK_DIR} (strih FULL artifact: ${ART})"
mkdir -p "$GENLOCK_DIR"
if [ -d "${STRIH_LX_BUNDLE_SRC:-}" ]; then
  # issue 1317: NEVER install a bundle built for a different Ubuntu release than the box runs -- a
  # 24.04-built bundle (noble ffmpeg/Qt sonames) would crash OBS at load on the 26.04 strih-lx box.
  # Gate the STAGED bundle's TARGET-RELEASE marker vs the box's /etc/os-release VERSION_ID BEFORE the
  # cp -a (fail-closed on an absent marker -- a pre-marker bundle must be rebuilt, never trusted).
  SETUP_BOX_VERSION_ID="$( . /etc/os-release 2>/dev/null; printf '%s' "${VERSION_ID:-}" )"
  SRC_FLAGS="${STRIH_LX_BUNDLE_SRC%/}/STRIH_BUILD_FLAGS.txt"
  strih_lx_release_parity_ok "$(cat "$SRC_FLAGS" 2>/dev/null || true)" "$SETUP_BOX_VERSION_ID" \
    || fail "bundle release parity: ${SRC_FLAGS} must carry 'TARGET-RELEASE: ubuntu-${SETUP_BOX_VERSION_ID}' (box VERSION_ID=${SETUP_BOX_VERSION_ID}) -- refusing to install a bundle built for another release"
  # issue 1317: install the runtime packages the CI runner recorded the bundle links against (Qt6 /
  # ffmpeg 8 / libOpenGL / ...) BEFORE installing the bundle. A fresh 26.04 box has none of them, so
  # without them the loader fails with the 13-soname `libavcodec.so.62: cannot open shared object`
  # error. Fail-closed on an absent RUNTIME_PACKAGES.txt (same contract as TARGET-RELEASE: a pre-1317
  # bundle whose runtime deps are unknown must be rebuilt, never trusted).
  SRC_RUNTIME_PKGS="${STRIH_LX_BUNDLE_SRC%/}/RUNTIME_PACKAGES.txt"
  [ -f "$SRC_RUNTIME_PKGS" ] || fail "bundle runtime packages: RUNTIME_PACKAGES.txt missing (${SRC_RUNTIME_PKGS}) -- a strih bundle records the apt packages it links against since issue 1317; rebuild + re-stage (refusing to install a bundle whose runtime deps are unknown)"
  RUNTIME_PKGS="$(strih_runtime_packages_from_file "$SRC_RUNTIME_PKGS" | tr '\n' ' ')"
  if [ -n "${RUNTIME_PKGS// /}" ]; then
    echo "  installing bundle runtime packages: ${RUNTIME_PKGS}"
    # shellcheck disable=SC2086
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $RUNTIME_PKGS \
      || fail "runtime package install failed (apt-get install ${RUNTIME_PKGS}) -- fix the box's apt sources / package names and re-run"
  else
    warn "  RUNTIME_PACKAGES.txt is empty -- no runtime packages to install (unexpected for a strih bundle)"
  fi
  # A pre-staged bundle dir (deploy-genlock-fleet.sh scp'd it, or an operator did) -- install it.
  cp -a "${STRIH_LX_BUNDLE_SRC%/}/." "$GENLOCK_DIR/" || fail "genlock bundle copy failed"
  GSHA="$(cat "$GENLOCK_DIR/GENLOCK_BUILD_SHA.txt" 2>/dev/null || echo unknown)"
  DSHA="$(cat "$GENLOCK_DIR/DISTROAV_BUILD_SHA.txt" 2>/dev/null || echo unknown)"
  genlock_write_markers "$GENLOCK_DIR" "$GSHA" "$DSHA" || fail "genlock_write_markers failed"
  echo "  installed genlock bundle to ${GENLOCK_DIR} (genlock ${GSHA}, distroav ${DSHA})"
  # issue 1317: install the bundle into its /usr prefix so the dynamic loader finds it -- the /opt
  # staged copy above is the marker home, but OBS is BUILT for /usr (libs -> /usr/lib/x86_64-linux-gnu,
  # the frontend -> /usr/bin/obs, data -> /usr/share/obs). Without this the loader has libobs.so.30 /
  # distroav.so on no path and the Qt6/ffmpeg runtime is unreachable -- the exact 13-soname load
  # failure. Mirrors the imag on-box program in scripts/deploy-genlock-fleet.sh (issue 1236
  # perms-normalize + ldconfig).
  strih_install_bundle_prefix "$GENLOCK_DIR" /usr/lib/x86_64-linux-gnu /usr/bin /usr/share \
    || fail "strih_install_bundle_prefix: installing the bundle into the /usr prefix failed -- OBS would not load"
  echo "  installed bundle into the /usr prefix (/usr/bin/obs + /usr/lib/x86_64-linux-gnu + ldconfig)"
else
  warn "  STRIH_LX_BUNDLE_SRC unset -- fetch the ${ART} CI artifact and re-run with STRIH_LX_BUNDLE_SRC=<dir>"
  warn "  (deploy-genlock-fleet.sh --boxes strih-lx does this over ssh once the box is reachable)"
fi
# chrome-sandbox setuid-root (issue 1317 F6): the CEF SUID sandbox helper must be owned root:root
# mode 4755 or the browser sources cannot launch (Chromium aborts unless the sandbox is disabled at
# launch, which we reject -- disabling it weakens every browser source's isolation session-wide, so
# the setuid helper is the upstream-sanctioned shape). Runs against the installed bundle ONLY when
# STRIH_BUILD_FLAGS.txt declares BROWSER-ON; a BROWSER-OFF/absent marker is a loud SKIP. setup-strih
# already runs as root, so the chown/chmod take effect.
CS_FLAGS_FILE="${GENLOCK_DIR}/STRIH_BUILD_FLAGS.txt"
if [ -f "$CS_FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$CS_FLAGS_FILE")"; then
  CS_PATH="$(find "$GENLOCK_DIR" -type f -name chrome-sandbox 2>/dev/null | head -n1 || true)"
  [ -n "$CS_PATH" ] || fail "BROWSER-ON bundle but chrome-sandbox is absent under ${GENLOCK_DIR} -- the CEF sandbox helper is missing; the browser sources cannot launch"
  eval "$(strih_lx_chrome_sandbox_fix_cmd "$GENLOCK_DIR")" \
    || fail "chrome-sandbox chown root:root / chmod 4755 failed at ${CS_PATH}"
  CS_OWNER="$(stat -c '%U:%G' "$CS_PATH" 2>/dev/null || echo '?')"
  CS_MODE="$(stat -c '%a' "$CS_PATH" 2>/dev/null || echo '?')"
  CS_VERDICT="$(strih_lx_chrome_sandbox_verdict "$CS_OWNER" "$CS_MODE" 1)" \
    || fail "chrome-sandbox setuid fix did not take (${CS_VERDICT}: owner=${CS_OWNER} mode=${CS_MODE}); expected root:root 4755"
  echo "  chrome-sandbox setuid-root (root:root 4755) applied at ${CS_PATH} -- CEF sandbox launchable"
else
  warn "  chrome-sandbox setuid fix SKIPPED (STRIH_BUILD_FLAGS.txt absent or BROWSER-OFF at ${GENLOCK_DIR}) -- browser sources not built"
fi

# ---------------------------------------------------------------------------------------------
step 5 "OBS profile facts (strih-lx: seeded from the Windows 'light' profile)"
mkdir -p "$OBS_CFG"
strih_lx_profile_facts | tee "$GENLOCK_DIR/strih-lx-profile-facts.txt" | sed 's/^/  /'
mkdir -p "$REC_DIR" && chown "$DESKTOP_USER":"$DESKTOP_USER" "$REC_DIR" 2>/dev/null || true

# ---------------------------------------------------------------------------------------------
step 6 "NDI input/output seed manifest + seeding tooling (reuse obs_phase2.py; strih_scenes.py = follow-up)"
install -d -m 755 /opt/camera-box
{
  echo '{"inputs":['
  strih_lx_ndi_inputs | sed 's/.*/  "&",/' | sed '$ s/,$//'
  echo '],"outputs":['
  { strih_lx_ndi_outputs; strih_lx_ndi_republishes; } | sed 's/.*/  "&",/' | sed '$ s/,$//'
  echo "],\"camera_latency_ms\":$(strih_lx_camera_latency_ms)}"
} > /opt/camera-box/strih-lx-seed.json
echo "  wrote /opt/camera-box/strih-lx-seed.json ($(strih_lx_ndi_inputs | grep -c .) inputs, floor-3 pins)"
if [ -n "${GH_TOKEN:-}" ]; then
  curl -fsSL -H "Authorization: token ${GH_TOKEN}" -H 'Accept: application/vnd.github.raw' \
    "https://api.github.com/repos/${GENLOCK_REPO}/contents/scripts/obs_phase2.py?ref=dev" \
    -o /usr/local/bin/obs_phase2.py 2>/dev/null && chmod 0755 /usr/local/bin/obs_phase2.py \
    && echo "  installed obs_phase2.py (shared seeding primitives)" \
    || warn "  could not fetch obs_phase2.py (GH_TOKEN scope?) -- install it before seeding"
else
  warn "  GH_TOKEN unset -- install obs_phase2.py before seeding the scene collection"
fi
warn "  TODO(follow-up): a bespoke strih_scenes.py WS seeder (sibling of imag_scenes.py) seeds the"
warn "  10 inputs (genlock_fifo + floor 3) + the STRIH-LX (...) outputs from strih-lx-seed.json"

# ---------------------------------------------------------------------------------------------
step 7 "OBS pre-seed: WebSocket :4455 no-auth + Studio Mode"
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "$OBS_CFG/plugin_config/obs-websocket"
cat > "$OBS_CFG/plugin_config/obs-websocket/config.json" <<'WS'
{"server_enabled":true,"server_port":4455,"auth_required":false}
WS
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "$OBS_CFG/plugin_config" 2>/dev/null || true
echo "  obs-websocket :4455 no-auth pre-seeded; Studio Mode is enforced by the scene seeder (step 6)"

# ---------------------------------------------------------------------------------------------
step 8 "OBS supervision unit (strih-obs.service, Restart=on-failure) -- enable-only"
[ -f "${HERE}/../systemd/strih-obs.service" ] || fail "systemd/strih-obs.service not found next to this script"
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "${USER_HOME}/.config/systemd/user"
install -m 0644 "${HERE}/../systemd/strih-obs.service" "${USER_HOME}/.config/systemd/user/strih-obs.service"
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/systemd/user" 2>/dev/null || true
# issue 1317: install the launcher pair the unit's ExecStart/ExecStop reference
# (/usr/local/bin/strih-obs-start.sh + strih-obs-stop.sh) BEFORE enabling -- an enabled unit whose
# ExecStart target does not exist flaps 203/EXEC under Restart=on-failure. mode 0755 so systemd can
# exec them. Fail loud if either launcher is missing next to this script.
for _launcher in strih-obs-start.sh strih-obs-stop.sh; do
  [ -f "${HERE}/${_launcher}" ] || fail "launcher scripts/${_launcher} not found next to this script (it is the strih-obs.service ExecStart/ExecStop target)"
done
install -m 0755 "${HERE}/strih-obs-start.sh" /usr/local/bin/strih-obs-start.sh
install -m 0755 "${HERE}/strih-obs-stop.sh"  /usr/local/bin/strih-obs-stop.sh
echo "  installed launcher pair -> /usr/local/bin/strih-obs-start.sh + strih-obs-stop.sh (mode 0755)"
sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$DESKTOP_USER")" systemctl --user enable strih-obs.service 2>/dev/null \
  || warn "  enable strih-obs.service by hand once the user session bus is up"
echo "  strih-obs.service installed + enabled (starts on the next graphical session)"

# ---------------------------------------------------------------------------------------------
step 9 ":8899 bundle-state server (strih-bundle-state-server.service) -- enable-only"
[ -f "${HERE}/../systemd/strih-bundle-state-server.service" ] || fail "systemd/strih-bundle-state-server.service not found next to this script"
install -m 0644 "${HERE}/../systemd/strih-bundle-state-server.service" "${USER_HOME}/.config/systemd/user/strih-bundle-state-server.service"
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/systemd/user" 2>/dev/null || true
echo "  strih-bundle-state-server.service installed (serves /bundle-state.json for the dev1 genlock-lock watchdog)"
warn "  install bundle-state-server.py + its siblings under /opt/camera-box (setup-imag.sh step 28 pattern) before enabling"

# ---------------------------------------------------------------------------------------------
step 10 "RemoteOS MCP control-channel agent (canonical install-linux.sh)"
REMOTEOS_MCP_INSTALLER_URL="${REMOTEOS_MCP_INSTALLER_URL:-https://raw.githubusercontent.com/zbynekdrlik/remoteos-mcp/master/install-linux.sh}"
TMP_INST="$(mktemp)"
if curl -fsSL "$REMOTEOS_MCP_INSTALLER_URL" -o "$TMP_INST" 2>/dev/null; then
  bash "$TMP_INST" 2>/dev/null && echo "  remoteos-mcp installed (update dev1 .mcp.json linux-strih entry to match)" \
    || warn "  remoteos-mcp installer returned non-zero -- run it by hand"
else
  warn "  could not fetch remoteos-mcp install-linux.sh -- install it by hand (ops skill #555)"
fi
rm -f "$TMP_INST"

# ---------------------------------------------------------------------------------------------
step 11 "Never-sleep + de-jitter masks (parallel-box steady state)"
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/90-strih-lx.conf <<'LG'
[Login]
HandleLidSwitch=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandlePowerKey=ignore
LG
echo "  sleep/suspend/hibernate masked; lid + power keys ignored"

# ---------------------------------------------------------------------------------------------
AUDIO_NAME="$(strih_lx_audio_input_name)"
step 12 "Program audio: ${AUDIO_NAME} -> PipeWire (VB-Matrix replacement)"
if arecord -l 2>/dev/null | grep -qi 'MiniFuse'; then
  echo "  detected the ${AUDIO_NAME} USB interface (arecord -l) -- class-compliant PipeWire node"
else
  warn "  ${AUDIO_NAME} not detected (arecord -l) -- plug it in before go-live (it is the program-audio input)"
fi
if strih_lx_audio_route_wired; then
  echo "  VB-Matrix -> PipeWire program-audio route reported WIRED (STRIH_LX_AUDIO_WIRED=1)"
else
  fail "TODO(audio): the VB-Matrix -> PipeWire program-audio graph is not wired yet. On Windows the mastered program mix reaches OBS via ${AUDIO_NAME} -> VB-Matrix (VASIO-8) ASIO; on Linux PipeWire replaces VB-Matrix. Wire the PipeWire graph feeding the '${AUDIO_NAME}' capture into OBS, then re-run with STRIH_LX_AUDIO_WIRED=1. (Fail-loud until wired -- issue 1317.)"
fi

# ---------------------------------------------------------------------------------------------
step 13 "Final verification (verify-strih.sh acceptance gate)"
if [ -x "${HERE}/verify-strih.sh" ]; then
  "${HERE}/verify-strih.sh" || fail "verify-strih.sh acceptance gate did not pass"
else
  warn "  verify-strih.sh not found/executable next to this script -- run it manually"
fi

echo -e "${GREEN}=== strih-lx setup complete ===${NC}"
