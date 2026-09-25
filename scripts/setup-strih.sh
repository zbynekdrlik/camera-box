#!/bin/bash
# strih-lx One-Shot Setup (issue 1317) -- see the extended header below the strict-mode line.
# Provisions a Linux notebook as the strih cutter/mix box; runs ON the box as root, idempotent.
set -euo pipefail
# Pin a 022 umask: a caller running this under 077 (the 24.9.2026 deploy) made every directory it
# created root-only, and the intercom-hub (User=newlevel) could not read its own config.
umask 022
#
# The box:
#   * emits its NDI outputs under its own NAMESPACED `<STRIH_NDI_PREFIX> (...)` names (never a
#     `STRIH-SNV (...)` sender on the wire -- the stream box + receivers must never see two), and
#   * runs dantesync in the ROLE its fact file declares (strih-lx: `server`, the fleet NTP master since
#     the M4 cut-over 20.9.2026; a `client` box syncs from its STRIH_DANTESYNC_UPSTREAM).
#
# issue 1357 (owner rulings): the box itself is the SAME OBS-only appliance imag was -- every box-level
# item (lightdm autologin -> openbox on plain Xorg with GNOME purged, low-latency kernel, boot safety
# net, AFFINITY-ONLY core reservation, NVIDIA PRIME nvidia-primary, de-jitter + no crash popups,
# network tuning, max-performance persistence, power envelope, touchpad) comes from the shared
# scripts/lib/obs-box-baseline.sh that setup-imag.sh runs too (step 11 below), graded by the shared
# scripts/lib/obs-box-baseline-verify.sh. This script keeps only the strih ROLE steps.
#
# The strih role FACTS + pure decisions live in scripts/lib/strih-provision.sh (sourced below +
# unit-tested from tests/strih_provision_pure_functions.rs). This orchestrator is the enable-only,
# fail-loud flow around them; it reuses the shared genlock-markers.sh helper and the canonical
# remoteos-mcp / bundle-state tooling rather than re-implementing any of it.
#
# issue 1361: every box/venue FACT (hostname, IP, NDI prefix, dantesync role + upstream, intercom
# config, NIC rule, OBS profile/collection, NDI-runtime peer, Companion controller, CG sender, cameras)
# comes from the selected box's fact file scripts/strih-boxes/<box>.env, loaded + validated by
# scripts/lib/strih-box-facts.sh. One script for every strih box -- strih PP is a new fact file, never
# a copy of this script. A fact file with any TODO_OWNER value (a template) REFUSES, naming each fact.
#
# Usage (on the box):
#   sudo GH_TOKEN=<gh-pat-repo-read> ./setup-strih.sh [--box <name>] [--yes]    (default box: strih-lx)

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_USER="${STRIH_LX_USER:-newlevel}"
USER_HOME="/home/${DESKTOP_USER}"
OBS_CFG="${USER_HOME}/.config/obs-studio"
GENLOCK_REPO="${GENLOCK_REPO:-zbynekdrlik/camera-box}"
GENLOCK_DIR="/opt/obs-genlock"
REC_DIR="/srv/_REC"
TOTAL_STEPS=17

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

# shellcheck source=scripts/lib/strih-box-facts.sh
. "${HERE}/lib/strih-box-facts.sh"   # issue 1361: the ONE per-box fact loader (--box <name>)
# shellcheck source=scripts/lib/strih-provision.sh
. "${HERE}/lib/strih-provision.sh"
# shellcheck source=scripts/lib/strih-drm-output.sh
. "${HERE}/lib/strih-drm-output.sh"   # issue 1346: the DRM-lease HDMI output config + verdict helpers
# shellcheck source=scripts/lib/genlock-markers.sh
. "${HERE}/lib/genlock-markers.sh"
# shellcheck source=scripts/lib/ndi-discovery.sh
. "${HERE}/lib/ndi-discovery.sh"   # issue 1342: the receiver-side NDI config, networks.ips (with setup-device.sh)
# shellcheck source=scripts/lib/ndi-runtime.sh
. "${HERE}/lib/ndi-runtime.sh"   # issue 1317: shared NDI 6.3.2 runtime install recipe (with setup-imag.sh)
# shellcheck source=scripts/lib/obs-box-baseline.sh
. "${HERE}/lib/obs-box-baseline.sh"   # issue 1357: the ONE OBS-box appliance baseline (the SAME lib setup-imag.sh runs)
# shellcheck source=scripts/lib/remoteos-mcp.sh
. "${HERE}/lib/remoteos-mcp.sh"   # issue 1361: the ONE remoteos-mcp venv install (with setup-imag.sh + setup-device.sh)
# shellcheck source=scripts/lib/obs-downstream-keyer.sh
. "${HERE}/lib/obs-downstream-keyer.sh"   # issue 1361: the pinned Downstream Keyer OBS plugin (step 4c)

# --- issue 1361: select + load the box facts BEFORE the source-guard, so a sourced setup (the unit
# tests) sees exactly the facts the real run uses. Any invalid / TODO_OWNER fact refuses here.
STRIH_FACT_BOX="$(strih_box_cli_box "$@")" || fail "usage: setup-strih.sh [--box <name>] [--yes]"
strih_box_load "$STRIH_FACT_BOX" \
  || fail "box '${STRIH_FACT_BOX}': scripts/strih-boxes/${STRIH_FACT_BOX}.env is missing, invalid or still has TODO_OWNER facts (listed above) -- refusing to provision"

# --- source-guard: when sourced (the unit tests), stop here -- never run the destructive flow ----
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

[ "${EUID:-$(id -u)}" -eq 0 ] || fail "run as root (sudo)"

# issue 1357: the FIRST action, before any apt-get -- apt-get waits up to 10 min for a background apt
# run's dpkg lock instead of failing at once (the shared baseline drop-in; setup-imag.sh runs it too).
obs_box_apt_lock_timeout

STATIC_IP="$(strih_lx_ip)"
STRIH_HOST="$(strih_lx_host)"
BOX_NAME="$(strih_lx_hostname)"

echo -e "${GREEN}=== ${BOX_NAME} setup (issue 1317 / 1361): Linux strih cutter, facts scripts/strih-boxes/${STRIH_FACT_BOX}.env, host ${STRIH_HOST} ===${NC}"

# ---------------------------------------------------------------------------------------------
step 1 "Static IP (NetworkManager) + hostname $(strih_lx_hostname)"
command -v nmcli >/dev/null 2>&1 || fail "nmcli required (desktop Ubuntu NetworkManager)"
echo "  (operator: assign ${STATIC_IP}/23 to the rig NIC via nmcli; recorded here as the target)"
hostnamectl set-hostname "$(strih_lx_hostname)" 2>/dev/null || warn "  could not set hostname (non-fatal)"

# ---------------------------------------------------------------------------------------------
# issue 1317 (post-M4, 20.9.2026): the strih notebook IS the fleet's ONE NTP master (`strih.lan` ->
# 10.77.9.202; the cam boxes take NTP from it), so strih-lx's dantesync ROLE is `server` (a bare
# `dantesync` daemon = NTP-master mode, ntp_server_mode in /etc/dantesync/config.json). issue 1361: the
# role + its upstream are box FACTS (STRIH_DANTESYNC_ROLE / STRIH_DANTESYNC_UPSTREAM), so a venue whose
# strih is a CLIENT declares it in its fact file. The live box had a hand
# `dantesync.service.d/10-ntp-master.conf` drop-in overriding the provisioned CLIENT ExecStart -- now
# the role is folded INTO the unit and the stale drop-in is removed.
DS_ROLE="$(strih_lx_dantesync_role)"
step 2 "DanteSync ${DS_ROLE} (single timesync authority; post-M4 the notebook is the fleet NTP master)"
# Purge any competing timesync daemon (ops hard rule: dantesync OWNS the clock -- never
# timesyncd/chrony/ptp4l alongside it).
for svc in systemd-timesyncd chrony chronyd ntp ntpsec; do
  systemctl disable --now "$svc" 2>/dev/null || true
done
[ -x /usr/local/bin/dantesync ] || warn "  dantesync binary absent -- install it (see setup-imag.sh step 3 / dantesync-fleet-upgrade.md) before go-live"
# role -> args: server = bare (NTP master), client = --ntp-server <upstream fact>.
DS_ARGS="$(strih_lx_dantesync_args)" || fail "dantesync role '${DS_ROLE}' needs STRIH_DANTESYNC_UPSTREAM in the box facts"
# Fail-closed self-check (the guard BEFORE install): the role+args must be a COHERENT invocation --
# server with no args, or client with a genuine --ntp-server. An ambiguous shape refuses.
strih_lx_dantesync_role_ok "$DS_ROLE" "$DS_ARGS" \
  || fail "dantesync role '$DS_ROLE' args '$DS_ARGS' are an ambiguous invocation -- refuse to write an undefined dantesync unit"
echo "  dantesync role: ${DS_ROLE}  (args: '${DS_ARGS:-<bare NTP master>}')"
# issue 1317: install dantesync as a systemd SERVICE, role folded INTO the unit (the EXACT cambox
# shape: Type=simple, Restart=always). strih_dantesync_unit_text fail-closes on an ambiguous shape.
strih_dantesync_unit_text "$DS_ROLE" "$DS_ARGS" > /etc/systemd/system/dantesync.service \
  || fail "strih_dantesync_unit_text refused to emit a unit for role '$DS_ROLE' args '$DS_ARGS'"
# Remove any stale dantesync.service.d/*.conf drop-in: the live box had a hand 10-ntp-master.conf
# resetting ExecStart to the bare NTP-master daemon. The role is now IN the unit, so a lingering
# drop-in would hide the provisioned role (two files describing one role -- the exact undocumented
# state this bake-in eliminates).
rm -f /etc/systemd/system/dantesync.service.d/*.conf 2>/dev/null || true
rmdir /etc/systemd/system/dantesync.service.d 2>/dev/null || true
systemctl daemon-reload
# Clear a stale lock a previously-crashed dantesync may have left, or the fresh daemon refuses to start.
rm -f /var/run/dantesync.lock 2>/dev/null || true
systemctl enable dantesync 2>/dev/null || true
if [ -x /usr/local/bin/dantesync ]; then
  systemctl restart dantesync 2>/dev/null \
    || warn "  dantesync.service failed to (re)start -- check journalctl -u dantesync"
  echo "  dantesync.service installed + enabled + started (role: ${DS_ROLE})"
else
  warn "  dantesync.service installed + enabled but NOT started (binary absent) -- install /usr/local/bin/dantesync then: systemctl restart dantesync"
fi

# ---------------------------------------------------------------------------------------------
REC_ENC="$(strih_lx_profile_facts | grep '^rec_encoder=' | cut -d= -f2)"
step 3 "NVIDIA driver / NVENC pre-check (record encoder ${REC_ENC}; the driver + PRIME install is baseline step 11)"
# issue 1357: the NVIDIA driver + PRIME nvidia-primary are a BASELINE item now (obs_box_nvidia_prime,
# step 11, run in imag's order AFTER the boot safety net so the DKMS/grub change is guarded). On a box
# that already has the driver this reports NVENC; on a fresh box it only warns -- step 11 installs the
# driver, and verify-strih item 10 is the hard NVENC gate after the reboot.
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >/dev/null 2>&1; then
  echo "  nvidia-smi OK -- NVENC HEVC available for ${REC_ENC}"
else
  warn "  nvidia-smi missing/failing -- step 11 (baseline) installs nvidia-driver-595-open + PRIME; NVENC for ${REC_ENC} after the next boot (verify-strih item 10 gates it)"
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
  # issue 1317 slice: the Qt6 SVG icon-engine + image-format plugins live in the SEPARATE
  # qt6-svg-plugins package -- Ubuntu 26.04 split them out of libqt6svg6 (which carries only the
  # library). OBS 32's default Yami theme is SVG-based, so WITHOUT these plugins half the
  # toolbar/dock/settings icons render BLANK (live-verified on strih-lx 10.77.9.202, 21.9.). Install
  # it alongside the recorded OBS-runtime deps. Idempotent (apt-get is a no-op if already present);
  # fail-loud so a re-flash reproduces a working operator UI instead of silently blank icons.
  echo "  installing qt6-svg-plugins (OBS 32 SVG theme icon-engine + image-format plugins)"
  DEBIAN_FRONTEND=noninteractive apt-get install -y qt6-svg-plugins \
    || fail "qt6-svg-plugins install failed -- OBS 32's SVG theme icons would render blank; fix the box's apt sources and re-run"
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
  warn "  (deploy-genlock-fleet.sh --boxes ${BOX_NAME} does this over ssh once the box is reachable)"
fi
# chrome-sandbox setuid-root (issue 1317 F6): the CEF SUID sandbox helper must be owned root:root
# mode 4755 or the browser sources cannot launch (Chromium aborts unless the sandbox is disabled at
# launch, which we reject -- disabling it weakens every browser source's isolation session-wide, so
# the setuid helper is the upstream-sanctioned shape). Runs against the installed bundle ONLY when
# STRIH_BUILD_FLAGS.txt declares BROWSER-ON; a BROWSER-OFF/absent marker is a loud SKIP. setup-strih
# already runs as root, so the chown/chmod take effect.
CS_FLAGS_FILE="${GENLOCK_DIR}/STRIH_BUILD_FLAGS.txt"
CS_USR_LIBDIR="/usr/lib/x86_64-linux-gnu"   # the /usr-prefix strih_install_bundle_prefix installs into
if [ -f "$CS_FLAGS_FILE" ] && strih_lx_browser_bundle_required "$(cat "$CS_FLAGS_FILE")"; then
  CS_PATH="$(find "$GENLOCK_DIR" -type f -name chrome-sandbox 2>/dev/null | head -n1 || true)"
  [ -n "$CS_PATH" ] || fail "BROWSER-ON bundle but chrome-sandbox is absent under ${GENLOCK_DIR} -- the CEF sandbox helper is missing; the browser sources cannot launch"
  # issue 1317 slice: setuid EVERY chrome-sandbox the running OBS could load -- the /opt bundle home
  # AND the /usr-prefix obs-plugins copy the /usr-install OBS actually loads (that copy was 0755 and
  # broke CEF browser sources live, 21.9.). The setuid target roots come from ONE source of truth
  # (strih_lx_chrome_sandbox_setuid_roots) shared with verify-strih.sh's /usr-path assertion.
  mapfile -t CS_ROOTS < <(strih_lx_chrome_sandbox_setuid_roots "$GENLOCK_DIR" "$CS_USR_LIBDIR")
  eval "$(strih_lx_chrome_sandbox_fix_cmd "${CS_ROOTS[@]}")" \
    || fail "chrome-sandbox chown root:root / chmod 4755 failed (roots: ${CS_ROOTS[*]})"
  # Re-verify BOTH the bundle copy AND the /usr-prefix copy the running OBS loads took the setuid.
  CS_USR_PATH="$(strih_lx_chrome_sandbox_usr_path "$CS_USR_LIBDIR")"
  for __csp in "$CS_PATH" "$CS_USR_PATH"; do
    [ -e "$__csp" ] || continue
    __cso="$(stat -c '%U:%G' "$__csp" 2>/dev/null || echo '?')"
    __csm="$(stat -c '%a' "$__csp" 2>/dev/null || echo '?')"
    __csv="$(strih_lx_chrome_sandbox_verdict "$__cso" "$__csm" 1)" \
      || fail "chrome-sandbox setuid fix did not take (${__csv}: owner=${__cso} mode=${__csm}) at ${__csp}; expected root:root 4755"
  done
  echo "  chrome-sandbox setuid-root (root:root 4755) applied at ${CS_ROOTS[*]} -- CEF sandbox launchable"
else
  warn "  chrome-sandbox setuid fix SKIPPED (STRIH_BUILD_FLAGS.txt absent or BROWSER-OFF at ${GENLOCK_DIR}) -- browser sources not built"
fi

# issue 1317 (owner request 22.9.2026): PRUNE the obs-plugins the genlock bundle ships that are
# unusable on strih-lx and log errors at every boot -- decklink*.so (no DeckLink hardware),
# obs-qsv11.so (Intel QSV, the box is NVIDIA -> nvenc), obs-vst.so (unused). KEEP everything else,
# especially distroav.so (NDI) + obs-browser.so/libcef.so. Runs as a step-4 tail AFTER the /opt +
# /usr installs, so it removes the freshly-copied dead plugins from BOTH the staged copy AND the
# /usr-prefix copy the running OBS loads. Idempotent (rm -f); MUST re-run every provisioning because
# the bundle install re-copies the whole tree. The prune LIST + the two obs-plugins DIRS are the ONE
# source of truth (strih_lx_obs_plugin_prune_list / strih_lx_obs_plugin_dirs) shared with verify-strih.
PRUNE_USR_LIBDIR="/usr/lib/x86_64-linux-gnu"
PRUNED_PLUGINS=""
while IFS= read -r _pdir; do
  [ -d "$_pdir" ] || continue
  while IFS= read -r _pat; do
    [ -n "$_pat" ] || continue
    # $_pat may be a glob (decklink*.so) -- leave it UNQUOTED so it expands against the plugin dir.
    for _pf in "$_pdir"/$_pat; do
      [ -e "$_pf" ] || continue
      rm -f "$_pf" && PRUNED_PLUGINS="${PRUNED_PLUGINS} $(basename "$_pf")"
    done
  done < <(strih_lx_obs_plugin_prune_list)
done < <(strih_lx_obs_plugin_dirs "$GENLOCK_DIR" "$PRUNE_USR_LIBDIR")
if [ -n "$PRUNED_PLUGINS" ]; then
  echo "  pruned unused obs-plugins:${PRUNED_PLUGINS}"
else
  echo "  obs-plugin prune: none of $(strih_lx_obs_plugin_prune_list | tr '\n' ' ')present (already clean)"
fi

# ---------------------------------------------------------------------------------------------
step 4b "NDI 6.3.2 runtime (fleet-identical from a cambox) -> DistroAV loads WITH NDI, not UI-only"
# issue 1317: without this DistroAV logs `ERR-404 NDI library not found` / `plugin loaded (UI-only)`
# and the box has NO NDI inputs/outputs. Reuse the shared recipe (scripts/lib/ndi-runtime.sh) so
# strih + imag install the SAME runtime. Runs BEFORE the OBS launch (step 8) -- DistroAV needs libndi
# on the loader path at OBS start. Copies from the box's STRIH_NDI_RUNTIME_PEER fact (a cam box); set
# STRIH_NDI_PEER=<ip> if that box is down, and CAM_PW=<cam ssh pw> (only used when the runtime is not
# already present).
NDI_PEER="${STRIH_NDI_PEER:-$(strih_lx_ndi_runtime_peer)}"
NDI_RUNTIME_DIR_STRIH="${STRIH_NDI_DIR:-/usr/lib/ndi}"
if [ -e "${NDI_RUNTIME_DIR_STRIH}/libndi.so.6" ] || [ -n "${CAM_PW:-}" ]; then
  ( eval "$(ndi_runtime_install_cmds "$NDI_PEER" "${CAM_PW:-}" "${STRIH_NDI_USER:-newlevel}" "$NDI_RUNTIME_DIR_STRIH")" ) \
    || fail "NDI runtime install failed (see above) -- set STRIH_NDI_PEER / CAM_PW and re-run"
  echo "  NDI 6.3.2 runtime installed (${NDI_RUNTIME_DIR_STRIH} + /usr/local/lib/libndi.so.6 symlink + avahi)"
else
  fail "NDI runtime absent and CAM_PW unset -- DistroAV would load UI-only (ERR-404). Re-run with CAM_PW=<cam ssh pw> (peer ${NDI_PEER}; override with STRIH_NDI_PEER=<ip>)."
fi
# issue 1352: avahi-utils provides `avahi-browse` -- the NDI/mDNS discovery CLI the rig gates use to
# enumerate senders. Ubuntu 26.04 does NOT install it with the avahi daemon, so without it those
# gates read blind. Idempotent (apt no-op if present); warn not fail -- OBS runs without it, only the
# discovery tooling is affected.
if DEBIAN_FRONTEND=noninteractive apt-get install -y avahi-utils; then
  echo "  avahi-utils installed (avahi-browse for NDI/mDNS discovery)"
else
  warn "  avahi-utils install failed -- avahi-browse (NDI/mDNS discovery) will be absent; fix the box's apt sources and re-run"
fi
# issue 1317 item H (ffprobe): the on-box recording-verdict E2E ([8/8a], recording-verdict-on-strih-lx.sh)
# spawns `ffprobe`, which Ubuntu 26.04 ships in the `ffmpeg` package -- a TOOL dependency of the E2E
# verdict, NOT a bundle soname (never in RUNTIME_PACKAGES.txt). Same idempotent apt family as
# avahi-utils above; warn not fail (OBS runs without it, only the on-box verdict would be missing it).
if DEBIAN_FRONTEND=noninteractive apt-get install -y ffmpeg; then
  echo "  ffmpeg installed (ffprobe for the on-box recording-verdict E2E)"
else
  warn "  ffmpeg install failed -- ffprobe (on-box E2E verdict) will be absent; fix the box's apt sources and re-run"
fi
# issue 1342: the RECEIVER-side NDI config (scripts/lib/ndi-discovery.sh, the SAME generator
# setup-device.sh writes the camboxes with): networks.ips = every managed NDI sender (every camera
# from camera-set.sh + the obs-fleet ndi-sender boxes; the traveling resolume hostname resolved now,
# skipped when away). libndi queries those IPs directly IN ADDITION to mDNS; senders never read the
# list, so strih-lx's own STRIH-LX outputs keep announcing over mDNS -- no gate. THREE receivers on
# this box: OBS (strih-obs.service) and bkshading-service, both User=${DESKTOP_USER}, read the desktop
# user's ~/.ndi; intercom-hub's ProtectHome hides ~/.ndi, so it reads the system dir /etc/ndi via its
# NDI_CONFIG_DIR drop-in. Written here, before the OBS/intercom units start (steps 8/13); the next
# start reads it. Every strih-lx genlock deploy re-runs this script, so a renumber converges there.
NDI_IPS="$(ndi_discovery_sender_ips)" \
  || fail "could not generate the managed NDI sender list (camera-set.sh + obs-fleet.sh ndi-sender facet, issue 1342)"
ndi_discovery_write_config "${USER_HOME}/.ndi" "$NDI_IPS" "$DESKTOP_USER" \
  || fail "NDI receiver config write to ${USER_HOME}/.ndi failed (issue 1342)"
ndi_discovery_write_config "$NDI_DISCOVERY_SYSTEM_DIR" "$NDI_IPS" \
  || fail "NDI receiver config write to ${NDI_DISCOVERY_SYSTEM_DIR} failed (issue 1342)"
mkdir -p "$(dirname "$NDI_DISCOVERY_INTERCOM_DROPIN")"
ndi_discovery_dropin_content > "$NDI_DISCOVERY_INTERCOM_DROPIN"
echo "  NDI receiver config: ${USER_HOME}/.ndi + ${NDI_DISCOVERY_SYSTEM_DIR} (networks.ips=${NDI_IPS}) + intercom-hub NDI_CONFIG_DIR drop-in (issue 1342)"

# ---------------------------------------------------------------------------------------------
# A lettered sub-step so TOTAL_STEPS stays 17 (test-pinned).
step "4c" "Downstream Keyer OBS plugin $(obs_dsk_version) (pinned upstream .deb, sha256-checked) -> the /usr prefix"
# issue 1361: the operator collection uses exeldro's Downstream Keyer; on strih-lx it was a hand
# extraction of the upstream .deb. This installs the SAME file from the SAME pinned release asset: the
# .deb and the extracted plugin are both sha256-checked (the plugin hash == the live strih-lx file),
# then only the plugin + its locale go into the /usr prefix OBS loads. Extracted, never dpkg-installed:
# the .deb depends on the distro obs-studio package, which the genlock bundle OBS does not provide.
# Idempotent: a plugin that already has the pinned hash downloads nothing. verify-strih item 35 grades it.
( eval "$(obs_dsk_install_cmds "$(obs_dsk_version)" "$(obs_dsk_deb_url)" "$(obs_dsk_deb_sha256)" "$(obs_dsk_so_sha256)" /usr/lib/x86_64-linux-gnu /usr/share)" ) \
  || fail "Downstream Keyer install failed (see above) -- check the pinned .deb URL / sha256 in scripts/lib/obs-downstream-keyer.sh and re-run"

# ---------------------------------------------------------------------------------------------
step 5 "OBS profile facts (${BOX_NAME}: seeded from the Windows 'light' profile)"
# issue 1317: create ~/.config/obs-studio owned by the DESKTOP user (this script runs under sudo, so a
# bare `mkdir` roots it and the obs user cannot then create .sentinel -- `Permission denied`, hit live).
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "$OBS_CFG"
strih_lx_profile_facts | tee "$GENLOCK_DIR/strih-lx-profile-facts.txt" | sed 's/^/  /'
mkdir -p "$REC_DIR" && chown "$DESKTOP_USER":"$DESKTOP_USER" "$REC_DIR" 2>/dev/null || true

# ---------------------------------------------------------------------------------------------
step 6 "NDI input/output seed manifest + seeding tooling (obs_phase2.py + strih_scenes.py seeder)"
install -d -m 755 /opt/camera-box
# issue 1317: the notebook runs the OPERATOR (production) collection, so the manifest carries the
# canonical strih input names as EXPLICIT-name objects {sender,input,scene} + "mode":"update-only"
# (heal the certified genlock class onto EXISTING inputs, never CreateScene/CreateInput -> no more
# duplicate `NDI CAMn (usb)` receivers). The full DATA name-map is strih_lx_seed_manifest_json.
strih_lx_seed_manifest_json > /opt/camera-box/strih-lx-seed.json
echo "  wrote /opt/camera-box/strih-lx-seed.json (operator collection: update-only, explicit strih input names NDI camN / NDI 2ME PVW / NDI 2ME PGM (mv) / cg / CG-obs, floor-3 pins)"
# issue 1346 (owner 24.9.2026): the HDMI output is the in-OBS DRM-lease output (the imag hardware
# output, issue 1152), selectable Program / built-in Multiview -- never an OBS projector window and
# never the desktop. Its activation contract is ~/.camera-box/drm-output.json of the OBS user
# (scripts/lib/strih-drm-output.sh). Provision it ONLY when an HDMI monitor is plugged in (the
# connector name must come from X RandR), default view multiview; an existing file is the
# operator's choice (the OBS Tools menu writes it) and is left alone. The retired 19.9. projector
# config is removed; its type seeds the initial view.
DRM_CONF_DIR="${USER_HOME}/.camera-box"
DRM_CONF="${DRM_CONF_DIR}/drm-output.json"
LEGACY_PROJ=/opt/camera-box/strih-lx-projector.json
DRM_VIEW0="$(strih_drm_legacy_view "$(cat "$LEGACY_PROJ" 2>/dev/null || true)")"
# issue 1346: HOW the output leaves the X desktop is a BOX fact (STRIH_HDMI_OUTPUT_BACKEND): lease = the
# X RandR lease (an Intel-driven connector), vk-direct = Vulkan direct display (strih-lx's built-in HDMI is
# NVIDIA-driven and the NVIDIA X driver refuses the lease). vk-direct dlopens the Vulkan loader, and the
# NVIDIA driver ships the ICD.
DRM_BACKEND="$(strih_lx_hdmi_output_backend)"
if [ "$DRM_BACKEND" = vk-direct ]; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y libvulkan1 \
    || warn "  issue 1346: apt-get install libvulkan1 failed -- the vk-direct HDMI output stays dormant until the Vulkan loader is installed"
  [ -f /usr/share/vulkan/icd.d/nvidia_icd.json ] \
    || warn "  issue 1346: no NVIDIA Vulkan ICD (/usr/share/vulkan/icd.d/nvidia_icd.json) -- the vk-direct HDMI output needs the NVIDIA driver's Vulkan"
fi
if [ -L "$DRM_CONF_DIR" ] || [ -L "$DRM_CONF" ]; then
  warn "  SKIP issue 1346: ${DRM_CONF_DIR} or ${DRM_CONF} is a symlink -- refusing to write through it as root; remove it and re-run"
elif [ -f "$DRM_CONF" ]; then
  echo "  ${DRM_CONF} already present -- leaving the operator's HDMI output choice"
  # issue 1346: the backend is the box fact, not an operator choice -- bring an existing config onto it
  # (the operator's view and every other key kept; lease = the absent key). Run as the desktop user, so
  # the atomic rename keeps the file owned by the user and root never writes through a user directory.
  if sudo -u "$DESKTOP_USER" python3 -c 'import sys; sys.path[:0] = [sys.argv[1], "/usr/local/bin"]; import strih_scenes as s; s.write_drm_backend(sys.argv[2], sys.argv[3])' \
      "$HERE" "$DRM_BACKEND" "$DRM_CONF"; then
    echo "  ${DRM_CONF}: backend ${DRM_BACKEND} (the box fact STRIH_HDMI_OUTPUT_BACKEND)"
  else
    warn "  issue 1346: could not write backend ${DRM_BACKEND} into ${DRM_CONF} (not a JSON object?) -- fix or remove it and re-run setup-strih.sh"
  fi
elif strih_drm_hdmi_connected; then
  DRM_CONN="$(sudo -u "$DESKTOP_USER" env DISPLAY=:0 XAUTHORITY="${USER_HOME}/.Xauthority" xrandr --query 2>/dev/null \
    | strih_drm_hdmi_output_from_xrandr || true)"
  if [ -n "$DRM_CONN" ] && DRM_LINE="$(strih_drm_output_config_json "$DRM_CONN" "$DRM_VIEW0" "$DRM_BACKEND")"; then
    install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "$DRM_CONF_DIR"
    printf '%s\n' "$DRM_LINE" | install -m 0644 -o "$DESKTOP_USER" -g "$DESKTOP_USER" /dev/stdin "$DRM_CONF"
    echo "  wrote ${DRM_CONF} (HDMI output ${DRM_CONN}, backend ${DRM_BACKEND}, view ${DRM_VIEW0}; takes effect at the next OBS start)"
  else
    warn "  SKIP issue 1346: an HDMI monitor is connected but X RandR could not name it (Xorg :0 not up yet?) -- ${DRM_CONF} NOT provisioned; re-run setup-strih.sh after the kiosk session is up"
  fi
else
  warn "  SKIP issue 1346: no HDMI monitor connected -- ${DRM_CONF} NOT provisioned (the fixed HDMI output stays dormant); attach the HDMI monitor and re-run setup-strih.sh"
fi
rm -f /opt/camera-box/strih-lx-projector.json
if [ -n "${GH_TOKEN:-}" ]; then
  curl -fsSL -H "Authorization: token ${GH_TOKEN}" -H 'Accept: application/vnd.github.raw' \
    "https://api.github.com/repos/${GENLOCK_REPO}/contents/scripts/obs_phase2.py?ref=dev" \
    -o /usr/local/bin/obs_phase2.py 2>/dev/null && chmod 0755 /usr/local/bin/obs_phase2.py \
    && echo "  installed obs_phase2.py (shared seeding primitives)" \
    || warn "  could not fetch obs_phase2.py (GH_TOKEN scope?) -- install it before seeding"
else
  warn "  GH_TOKEN unset -- install obs_phase2.py before seeding the scene collection"
fi
# issue 1317: install the strih_scenes.py seeder (sibling of imag_scenes.py). strih-obs-start.sh
# preflights `import strih_scenes` then runs `strih_scenes.py --bootstrap` on every launch to seed
# the 10 inputs (certified genlock: genlock_fifo/ndi_sync=2/floor 3) + per-input scenes + Studio Mode
# from strih-lx-seed.json. It is in the rsynced tree next to this script (like the launcher pair, step
# 8), so install it locally -- no GH_TOKEN dependency. The 5 STRIH-LX NDI OUTPUTS are a SEPARATE
# ticket; the seeder never touches outputs.
[ -f "${HERE}/strih_scenes.py" ] || fail "scripts/strih_scenes.py not found next to this script (the strih-obs-start.sh --bootstrap seed target)"
install -m 0755 "${HERE}/strih_scenes.py" /usr/local/bin/strih_scenes.py
echo "  installed strih_scenes.py -> /usr/local/bin (input/scene/Studio-Mode seeder; strih-obs-start.sh runs --bootstrap on launch)"
# issue 1242: the strih BANDWIDTH ROLES module (program-path cameras connect only while shown, the
# multiview renders low-bandwidth MV twins). strih_scenes.py --apply-roles imports it from its own
# directory, so it installs next to it; strih-obs-start.sh applies the roles on every launch.
[ -f "${HERE}/strih_bandwidth_roles.py" ] || fail "scripts/strih_bandwidth_roles.py not found next to this script (the strih_scenes.py --apply-roles module)"
install -m 0755 "${HERE}/strih_bandwidth_roles.py" /usr/local/bin/strih_bandwidth_roles.py
echo "  installed strih_bandwidth_roles.py -> /usr/local/bin (bandwidth roles; strih-obs-start.sh runs strih_scenes.py --apply-roles on launch)"

# ---------------------------------------------------------------------------------------------
step 7 "OBS pre-seed: WebSocket :4455 no-auth + Studio Mode"
# issue 1317: ensure ~/.config/obs-studio itself is owned by the DESKTOP user (an earlier root-seeded
# run would otherwise leave it root-owned and block the obs user from creating .sentinel).
chown "$DESKTOP_USER":"$DESKTOP_USER" "$OBS_CFG" 2>/dev/null || true
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "$OBS_CFG/plugin_config/obs-websocket"
cat > "$OBS_CFG/plugin_config/obs-websocket/config.json" <<'WS'
{"server_enabled":true,"server_port":4455,"auth_required":false}
WS
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "$OBS_CFG/plugin_config" 2>/dev/null || true
echo "  obs-websocket :4455 no-auth pre-seeded; Studio Mode is enforced by the scene seeder (step 6)"
# issue 1346: pre-seed [BasicWindow] SaveProjectors=true + ProjectorAlwaysOnTop=false in the desktop
# user's user.ini so OBS PERSISTS the operator's LAPTOP-screen projector (the Multiview on the eDP
# panel) and re-opens it on every launch. The OBS default is SaveProjectors=false, so a hand-opened
# projector would NEVER come back after strih-obs.service relaunches. (The HDMI output is NOT a
# projector since 24.9.2026 -- it is the DRM lease, step 6.) Idempotent (RawConfigParser upsert; the
# literal `SaveProjectors=true` is the verify-strih anchor), owned by the desktop user. NOTE: this is
# the OPPOSITE of imag (#522 SaveProjectors=false + an openbox-autostart re-open hook) -- strih-lx
# has no such boot hook, so it relies on OBS's own SaveProjectors restore.
# ProjectorAlwaysOnTop=false: owner ruling 23.9.2026 -- the multiview must not stay on top of the
# operator's other windows (the seed rewrote a hand-set OFF back to ON on every provisioning run).
USER_INI="${OBS_CFG}/user.ini"
if command -v python3 >/dev/null 2>&1; then
  python3 - "$USER_INI" <<'PY' || warn "  could not pre-seed SaveProjectors in ${USER_INI} (non-fatal; the OBS UI / strih_scenes.py --projector still work)"
import configparser, os, sys
path = sys.argv[1]
# strict=False tolerates duplicate keys/sections OBS may write; RawConfigParser avoids % interpolation.
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
if os.path.exists(path):
    try:
        cp.read(path)
    except Exception as e:
        # NEVER clobber an existing user.ini we could not parse -- leave it untouched and let the
        # caller's `|| warn` fire (a fresh-parser rewrite would DISCARD every other OBS setting).
        sys.stderr.write("user.ini parse failed (%s) -- leaving it untouched\n" % e)
        sys.exit(3)
if not cp.has_section("BasicWindow"):
    cp.add_section("BasicWindow")
for kv in ("SaveProjectors=true", "ProjectorAlwaysOnTop=false"):
    k, v = kv.split("=", 1)
    cp.set("BasicWindow", k, v)
with open(path, "w") as fh:
    cp.write(fh, space_around_delimiters=False)
PY
  chown "$DESKTOP_USER":"$DESKTOP_USER" "$USER_INI" 2>/dev/null || true
  echo "  pre-seeded [BasicWindow] SaveProjectors=true + ProjectorAlwaysOnTop=false in ${USER_INI}"
else
  warn "  python3 absent -- cannot pre-seed SaveProjectors in ${USER_INI} (set it in the OBS UI, or install python3 and re-run)"
fi

# issue 1352: seed DistroAV's [NDIPlugin] program/preview OUTPUT identity -- the BARE names
# MainOutputName=2ME PGM / PreviewOutputName=2ME PVW + both Enabled=true. DistroAV PREPENDS the box
# hostname -> announced STRIH-LX (2ME PGM) / (2ME PVW); a name that already carried STRIH-LX here
# doubles it (the STRIH-LX (STRIH-LX (2ME PGM)) bug). Runs with OBS STOPPED (this is enable-only,
# before any launch; OBS rewrites user.ini on exit). Idempotent RawConfigParser upsert via the ONE
# source of truth strih_lx_ndi_output_ini_cmds (scripts/lib/strih-provision.sh).
if command -v python3 >/dev/null 2>&1; then
  if eval "$(strih_lx_ndi_output_ini_cmds "$USER_INI")"; then
    chown "$DESKTOP_USER":"$DESKTOP_USER" "$USER_INI" 2>/dev/null || true
    echo "  seeded [NDIPlugin] MainOutputName=2ME PGM / PreviewOutputName=2ME PVW + Enabled=true in ${USER_INI}"
  else
    warn "  could not seed [NDIPlugin] output names in ${USER_INI} (non-fatal; set them in OBS Tools > DistroAV Settings, or re-run)"
  fi
else
  warn "  python3 absent -- cannot seed [NDIPlugin] output names in ${USER_INI} (set them in OBS Tools > DistroAV Settings, or install python3 and re-run)"
fi

# issue 1317 (owner "stále crashuje" 22.9.2026): seed [General] BrowserHWAccel=false into OBS's
# global.ini. The GPU-accelerated CEF path (libcef.so) int3-traps -> exit 133 every ~60 s on this RTX
# 5050 / GNOME-Wayland stack (the whole interkom scene black); BrowserHWAccel=false makes CEF
# software-render via libvk_swiftshader (0 crashes, verified live). This is a global.ini setting (OBS's
# cross-collection settings), NOT user.ini. Runs with OBS STOPPED (enable-only, before any launch; OBS
# rewrites global.ini on exit). Idempotent RawConfigParser upsert via the ONE source of truth
# strih_lx_obs_global_ini_cmds (scripts/lib/strih-provision.sh).
if command -v python3 >/dev/null 2>&1; then
  if eval "$(strih_lx_obs_global_ini_cmds "$OBS_CFG")"; then
    chown "$DESKTOP_USER":"$DESKTOP_USER" "${OBS_CFG}/global.ini" 2>/dev/null || true
    echo "  seeded [General] BrowserHWAccel=false in ${OBS_CFG}/global.ini (CEF crash-loop guard)"
  else
    warn "  could not seed [General] BrowserHWAccel=false in ${OBS_CFG}/global.ini (non-fatal; set it in OBS Settings > Advanced > Browser Source Hardware Acceleration OFF, or re-run)"
  fi
else
  warn "  python3 absent -- cannot seed [General] BrowserHWAccel=false in ${OBS_CFG}/global.ini (turn OFF Browser Source Hardware Acceleration in OBS, or install python3 and re-run)"
fi

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
# issue 1357: both launchers install VERBATIM. The issue-1352 XWayland-PRIME GPU-env substitution is
# gone: OBS runs in the plain Xorg openbox kiosk (baseline step 11) on the PRIME nvidia-PRIMARY X server,
# so it renders on the RTX with no offload env -- exactly like imag.
install -m 0755 "${HERE}/strih-obs-start.sh" /usr/local/bin/strih-obs-start.sh
install -m 0755 "${HERE}/strih-obs-stop.sh"  /usr/local/bin/strih-obs-stop.sh
echo "  installed launcher pair -> /usr/local/bin/strih-obs-start.sh + strih-obs-stop.sh (mode 0755)"
# issue 1361: the box's OBS profile/collection facts reach the launcher through a --user drop-in (the
# launcher reads STRIH_OBS_PROFILE / STRIH_OBS_COLLECTION from its environment and installs verbatim).
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "${USER_HOME}/.config/systemd/user/strih-obs.service.d"
strih_obs_box_facts_dropin_text > "${USER_HOME}/.config/systemd/user/strih-obs.service.d/10-box-facts.conf" \
  || fail "could not write the strih-obs.service box-facts drop-in"
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/systemd/user/strih-obs.service.d" 2>/dev/null || true
echo "  strih-obs.service.d/10-box-facts.conf: OBS profile '$(strih_lx_obs_profile)', collection '$(strih_lx_obs_collection)'"
sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$DESKTOP_USER")" systemctl --user enable strih-obs.service 2>/dev/null \
  || warn "  enable strih-obs.service by hand once the user session bus is up"
echo "  strih-obs.service installed + enabled (the kiosk openbox autostart, step 15, starts it at every boot)"

# ---------------------------------------------------------------------------------------------
step "8b" "retired strih-mv-host projector-host helper (issue 1357) -- remove a leftover install"
# issue 1357: the issue-1352 strih-mv-host.py helper re-hosted every OBS projector into a child X
# window to work around an XWayland + NVIDIA-PRIME-offload present stall (~0.5 s per present). The
# plain Xorg openbox kiosk (NVIDIA-primary, baseline step 11) has no XWayland and no PRIME offload,
# so the stall's premise is gone on every box; the vendored child-host projector was removed with it
# (the stock upstream toplevel projector, identical on every OS). The helper is retired: this step
# only REMOVES a leftover install from an earlier provisioning -- stop + disable the --user unit,
# delete the WantedBy link, the unit file and the helper -- and never installs or enables anything.
# Idempotent: a box that never had the helper is a no-op. A lettered sub-step so TOTAL_STEPS is
# unchanged.
MVH_UNIT="${USER_HOME}/.config/systemd/user/strih-mv-host.service"
MVH_WANTS="${USER_HOME}/.config/systemd/user/default.target.wants/strih-mv-host.service"
MVH_HELPER="/usr/local/bin/strih-mv-host.py"
if [ -e "$MVH_UNIT" ] || [ -L "$MVH_WANTS" ] || [ -e "$MVH_HELPER" ]; then
  sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$DESKTOP_USER")" systemctl --user disable --now strih-mv-host.service 2>/dev/null || true
  rm -f "${MVH_WANTS}" "${MVH_UNIT}" "${MVH_HELPER}"
  sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$DESKTOP_USER")" systemctl --user daemon-reload 2>/dev/null || true
  echo "  removed the retired strih-mv-host helper (unit, WantedBy link, /usr/local/bin/strih-mv-host.py)"
else
  echo "  strih-mv-host helper not installed -- nothing to remove"
fi

# ---------------------------------------------------------------------------------------------
step 9 ":8899 bundle-state server (strih-bundle-state-server.service) -- enable-only"
[ -f "${HERE}/../systemd/strih-bundle-state-server.service" ] || fail "systemd/strih-bundle-state-server.service not found next to this script"
install -m 0644 "${HERE}/../systemd/strih-bundle-state-server.service" "${USER_HOME}/.config/systemd/user/strih-bundle-state-server.service"
chown -R "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/systemd/user" 2>/dev/null || true
echo "  strih-bundle-state-server.service installed (serves /bundle-state.json for the dev1 genlock-lock watchdog)"
# issue 1317: install the server tree the unit's ExecStart references BEFORE enabling (the
# setup-imag.sh step-28 pattern) -- bundle-state-server.py + its bundle_state_gather / obs_phase2
# sibling imports install together under /opt/camera-box so the server's imports resolve.
install -d -m 755 /opt/camera-box
if [ -n "${GH_TOKEN:-}" ]; then
  for _bss in bundle-state-server.py bundle_state_gather.py obs_phase2.py; do
    curl -fsSL -H "Authorization: token ${GH_TOKEN}" -H 'Accept: application/vnd.github.raw' \
      "https://api.github.com/repos/${GENLOCK_REPO}/contents/scripts/${_bss}?ref=dev" \
      -o "/opt/camera-box/${_bss}" 2>/dev/null \
      || fail "issue 1317: could not fetch scripts/${_bss} (GH_TOKEN scope?) -- required for the :8899 bundle-state server"
  done
  chmod 0644 /opt/camera-box/*.py
  echo "  installed bundle-state server tree -> /opt/camera-box (bundle-state-server.py + bundle_state_gather.py + obs_phase2.py)"
  sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$DESKTOP_USER")" systemctl --user enable strih-bundle-state-server.service 2>/dev/null \
    || warn "  enable strih-bundle-state-server.service by hand once the user session bus is up"
else
  warn "  GH_TOKEN unset -- install bundle-state-server.py + bundle_state_gather.py + obs_phase2.py under /opt/camera-box, then enable strih-bundle-state-server.service"
fi

# ---------------------------------------------------------------------------------------------
step 10 "RemoteOS MCP control-channel agent (venv /opt/remoteos-mcp-venv, shared scripts/lib/remoteos-mcp.sh)"
# issue 1361: the SAME venv install the live strih-lx box runs, from the ONE shared lib setup-imag.sh and
# setup-device.sh call too (the upstream install-linux.sh pip-installs into the SYSTEM python with
# --break-system-packages; strih-lx runs a venv). Keeps the box's existing key (REMOTEOS_MCP_AUTH_KEY overrides), restarts the
# agent only when the source / unit / key changed, and fails the run unless :8092 answers /health.
remoteos_mcp_install "$DESKTOP_USER" desktop restart \
  || fail "remoteos-mcp agent install failed -- see the remoteos-mcp line above (GH_TOKEN for a private fork; REMOTEOS_MCP_AUTH_KEY to pin the key)"

# ---------------------------------------------------------------------------------------------
step 11 "OBS-box appliance baseline (issue 1357: the SAME lib as imag -- Xorg openbox kiosk, low-latency kernel, de-jitter, max-performance, power envelope)"
# issue 1357 (owner rulings on the ticket): imag's provisioning is the reference starting position and
# strih-lx must be the same OBS-only appliance -- a lightweight openbox kiosk on plain Xorg, NOT a GNOME
# Wayland desktop. Every box-level item is the SHARED scripts/lib/obs-box-baseline.sh (moved verbatim
# out of setup-imag.sh), run here in imag's order with strih-lx's own FACTS as the arguments. This
# supersedes the old strih never-sleep (logind drop-in), the de-jitter-less GNOME session, the step-15
# governor oneshot and the 11c crash-popup sub-step. Kernel, PRIME and session changes take effect at
# the next boot (the supervisor reboots strih-lx once after this run).
STRIH_KERNEL_SERIES="$(obs_box_kernel_series)" \
  || fail "cannot derive the Ubuntu release series from /etc/os-release -- refusing to hold/install kernel packages by a guessed name"
STRIH_NIC="$(strih_lx_rig_nic /sys "$(ip -o -4 addr show 2>/dev/null || true)" "$STATIC_IP")" \
  || fail "cannot resolve the rig NDI NIC for the network tuning -- set STRIH_NIC_IFACE=<iface> and re-run"
# PL1: never below what the firmware already runs (the review read 80 W live on strih-lx) -- read the
# package-0 long_term constraint through the SAME identity-based gather the envelope verify uses.
# shellcheck source=scripts/lib/imag-power-envelope.sh
. "${HERE}/lib/imag-power-envelope.sh"
STRIH_FW_PL1_UW="$(imag_power_zone_select "$(bash -c "$(imag_power_envelope_gather_remote_snippet)" 2>/dev/null || true)" || true)"
# A re-run reads OUR pin back -- or the guard's thermal step-down while the box is hot -- so a stepped-down
# read is never taken as the firmware's, and the PL1 a previous run baked into the unit is a floor too.
if [ "$(imag_power_guard_stepped_from_state "$(cat "$IMAG_POWER_GUARD_STATE_FILE" 2>/dev/null || true)")" = stepped ]; then
  STRIH_FW_PL1_UW=""
fi
STRIH_BAKED_PL1_W="$(strih_lx_baked_pl1_watts "$(systemctl show -p Environment --value imag-power-envelope.service 2>/dev/null || true)")"
STRIH_PL1_W="$(strih_lx_pl1_watts "$STRIH_FW_PL1_UW" "$STRIH_BAKED_PL1_W")"
STRIH_PL1_STEPDOWN_W="$(strih_lx_pl1_stepdown_watts)"
echo "  box facts: series=${STRIH_KERNEL_SERIES} nic=${STRIH_NIC} user=${DESKTOP_USER} pl1=${STRIH_PL1_W}W (firmware ${STRIH_FW_PL1_UW:-unread} uW, baked ${STRIH_BAKED_PL1_W:-none} W) stepdown=${STRIH_PL1_STEPDOWN_W}W"
# the power envelope's on-box tools come from THIS checkout (setup-strih runs from the repo; imag fetches
# the same files over gh api).
strih_fetch_repo_file() {  # strih_fetch_repo_file REPO_RELPATH DEST
  cp -f "${HERE}/../$1" "$2"
}
obs_box_network_tuning "$STRIH_NIC" strih
obs_box_max_performance "$STRIH_NIC" strih
obs_box_never_sleep "$DESKTOP_USER" strih
obs_box_boot_safety_net "$STRIH_KERNEL_SERIES" strih
obs_box_lowlatency_kernel "$STRIH_KERNEL_SERIES"
obs_box_cpu_affinity strih        # -> /etc/strih-isolated-cpus.conf, strih-obs-start.sh's taskset pin
obs_box_nvidia_prime strih        # nvidia-driver-595-open + PRIME nvidia-PRIMARY (the RTX 5050)
obs_box_dejitter "$DESKTOP_USER" strih "$OBS_CFG"
obs_box_kiosk "$DESKTOP_USER" strih keep-bluetooth
obs_box_power_envelope "$STRIH_PL1_W" strih_fetch_repo_file "$STRIH_PL1_STEPDOWN_W"
obs_box_touchpad strih
obs_box_maxperf_persistence strih
# Self-heal: the retired strih never-sleep logind drop-in -- obs_box_never_sleep's 99-strih-no-sleep.conf
# + 99-production-no-powerkey.conf now own the lid/suspend/power-key policy, one source of truth.
if [ -e /etc/systemd/logind.conf.d/90-strih-lx.conf ]; then
  rm -f /etc/systemd/logind.conf.d/90-strih-lx.conf || fail "could not remove the retired logind drop-in 90-strih-lx.conf"
  echo "  removed the retired logind drop-in 90-strih-lx.conf (superseded by the baseline never-sleep item)"
fi
# rtprio stays OFF (issue 1357 design): the retired 11c grant pinned the render tick SCHED_FIFO on cores
# strih-lx never reserved and the FIFO + affinity leaked to 28 NDI threads (issue comment 5793075833).
# Self-heal: remove a grant a previous run (or a hand fix) left behind -- verify-strih FAILs while it exists.
RTPRIO_LEFTOVER="$(strih_rtprio_leftover_path)"
if [ -e "$RTPRIO_LEFTOVER" ]; then
  rm -f "$RTPRIO_LEFTOVER" || fail "could not remove the leftover rtprio grant ${RTPRIO_LEFTOVER}"
  echo -e "  ${YELLOW}removed the leftover rtprio grant ${RTPRIO_LEFTOVER} (rtprio stays OFF, issue 1357)${NC}"
fi

# ---------------------------------------------------------------------------------------------
# Lettered sub-step (TOTAL_STEPS unchanged -- the issue 1352/1353 precedent): the NIC-IRQ placement
# is the biggest de-jitter win, so it rides alongside the never-sleep/de-jitter masks above.
step "11b" "NIC xhci IRQ off the OBS cores (issue 1317 item H: NET_RX softirq must not share an OBS core)"
# The USB 2.5GbE NIC's xhci interrupt lands on ONE core (irqbalance is not installed, so the kernel
# parks it) where ~1.1 Gb/s of NDI NET_RX softirq collides with OBS's ndir:video/libobs threads
# (issue 1354, 22.9.2026: measured on strih-lx -- moving the IRQ to an idle E-core cut the genlock
# "slow output_video" rate from ~35/min to 5-10/min). This installs a boot oneshot that RESOLVES the
# placement FROM FACTS (iface -> xhci -> IRQ -> last cpu_atom E-core) and writes smp_affinity_list --
# never a hard-coded IRQ number, so it survives a different IRQ after a kernel/firmware/USB-port
# change. ENABLE-ONLY: the supervisor applies it live; the unit re-applies it at every boot.
install -d -m 755 /usr/local/bin
strih_nic_irq_affinity_script_text > /usr/local/bin/strih-nic-irq-affinity.sh \
  || fail "could not write /usr/local/bin/strih-nic-irq-affinity.sh"
chmod 0755 /usr/local/bin/strih-nic-irq-affinity.sh
[ -f "${HERE}/../systemd/strih-nic-irq-affinity.service" ] || fail "systemd/strih-nic-irq-affinity.service not found next to this script"
install -m 0644 "${HERE}/../systemd/strih-nic-irq-affinity.service" /etc/systemd/system/strih-nic-irq-affinity.service
systemctl daemon-reload 2>/dev/null || true
systemctl enable strih-nic-irq-affinity.service 2>/dev/null \
  || warn "  enable strih-nic-irq-affinity.service by hand (systemctl enable)"
echo "  strih-nic-irq-affinity.service installed + enabled (applies at next boot / supervisor runs it live)"

# ---------------------------------------------------------------------------------------------
AUDIO_NAME="$(strih_lx_audio_input_name)"
step 12 "Program audio: intercom hub -> PipeWire strih-program sink -> OBS (VB-Matrix replacement, issue 1344)"
# Root cause (design 20.9., follow-up 20.9. live diagnosis issuecomment-5751113173): the OBS
# program (${AUDIO_NAME}) is a NETWORK stream (fohabl-strih VBAN), NOT the MiniFuse. The hub writes
# the program mix to a strih-program null sink; OBS captures it via a LOOPBACK republish node
# strih-program-source (strih-program.monitor itself is NOT pulse-visible to OBS on this box --
# proven live). The MiniFuse carries only the operator TALKBACK mic (the hub reads it).
DESKTOP_UID="$(id -u "$DESKTOP_USER" 2>/dev/null || echo 1000)"
if arecord -l 2>/dev/null | grep -qi 'MiniFuse'; then
  echo "  detected the MiniFuse 4 USB interface (arecord -l) -- the operator talkback capture"
else
  warn "  MiniFuse 4 not detected (arecord -l) -- plug it in before go-live (it is the operator talkback mic)"
fi
# (c) the operator-session PipeWire null sink OBS captures as strih-program.monitor.
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "${USER_HOME}/.config/pipewire/pipewire.conf.d"
strih_pipewire_program_sink_conf > "${USER_HOME}/.config/pipewire/pipewire.conf.d/strih-program.conf"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/pipewire/pipewire.conf.d/strih-program.conf"
# (c2, issue 1344 follow-up) the loopback that republishes strih-program's monitor as a real
# pulse-visible Audio/Source (strih-program.monitor itself is NOT pulse-visible to OBS on this box).
strih_pipewire_program_loopback_conf > "${USER_HOME}/.config/pipewire/pipewire.conf.d/strih-program-loopback.conf"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/pipewire/pipewire.conf.d/strih-program-loopback.conf"
# the WirePlumber rule pinning the MiniFuse to its pro-audio profile @48 kHz.
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "${USER_HOME}/.config/wireplumber/wireplumber.conf.d"
strih_wireplumber_minifuse_rule > "${USER_HOME}/.config/wireplumber/wireplumber.conf.d/51-strih-minifuse.conf"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/wireplumber/wireplumber.conf.d/51-strih-minifuse.conf"
# issue 1345 (owner accepted 25.9.2026): the MiniFuse PLAYBACK period (1024 x 3, headroom 256 -- the
# buzz fix) and the graph's min-quantum floor 1024 (the robotic-cameraman fix). Compared, rewritten only
# on a difference, logged; both apply at the next WirePlumber/PipeWire start (the reboot after setup).
strih_wireplumber_minifuse_output_period_conf | obs_box_write_if_changed "${USER_HOME}/.config/wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf" 0664 "$DESKTOP_USER:$DESKTOP_USER" "MiniFuse output period"
strih_pipewire_quantum_conf | obs_box_write_if_changed "${USER_HOME}/.config/pipewire/pipewire.conf.d/51-strih-quantum-1024.conf" 0664 "$DESKTOP_USER:$DESKTOP_USER" "graph quantum 1024"
# the intercom-hub systemd drop-in that runs the hub AS THE OPERATOR (reach the PipeWire session).
mkdir -p /etc/systemd/system/intercom-hub.service.d
strih_intercom_audio_dropin "$DESKTOP_USER" "$DESKTOP_UID" > /etc/systemd/system/intercom-hub.service.d/10-local-audio.conf
# Linger the operator so its user@.service + PipeWire come up at boot even before an interactive
# login — otherwise a headless start finds no /run/user/<uid>/pipewire-0 (the hub self-heals via the
# pw-cat restart backoff once the session appears, but linger removes the startup gap).
loginctl enable-linger "$DESKTOP_USER" 2>/dev/null || warn "  could not enable-linger $DESKTOP_USER (the operator PipeWire session must be up before the hub's audio starts)"
systemctl daemon-reload 2>/dev/null || true
echo "  installed strih-program null sink + loopback source (strih-program-source) + WirePlumber MiniFuse rule (operator session) + intercom-hub local-audio drop-in (User=${DESKTOP_USER})"
echo "  the OBS '${AUDIO_NAME}' input (pulse_input_capture on strih-program-source) is seeded by scripts/strih_scenes.py --bootstrap; verify-strih derives the audio verdict"
echo "  NOTE: restart OBS after the operator PipeWire/WirePlumber comes up (or re-login) -- OBS enumerates audio devices only at startup, so it must start AFTER strih-program-source exists"

# ---------------------------------------------------------------------------------------------
step 13 "Intercom hub unit + matrix (issue 1345 M1: ENABLE-ONLY, NEVER started while parallel)"
# The strih-lx intercom hub replaces the Windows VB-Matrix's N-1 intercom for the VBAN camboxes.
# Install the systemd unit + the generated routing TOML and ENABLE it, but NEVER start/restart it
# here: sending VBAN to the real camboxes is the M4 cut-over (the Windows strih stays their live hub
# until then). The deployable binary (intercom-hub-linux-amd64 from CI) is placed separately.
install -Dm644 "${HERE}/../systemd/intercom-hub.service" /etc/systemd/system/intercom-hub.service
# issue 1361: the routing file is the box fact STRIH_INTERCOM_CONFIG (repo-relative).
[ -f "${HERE}/../$(strih_lx_intercom_config)" ] \
  || fail "intercom routing file ${HERE}/../$(strih_lx_intercom_config) (fact STRIH_INTERCOM_CONFIG) not found -- stage the repo intercom/ dir next to scripts/"
install -Dm644 "${HERE}/../$(strih_lx_intercom_config)" /etc/intercom-hub/intercom.toml
systemctl daemon-reload
systemctl enable intercom-hub 2>/dev/null || warn "  could not enable intercom-hub.service"
if [ -x /usr/local/bin/intercom-hub ]; then
  echo "  intercom-hub.service installed + enabled (NOT started -- M4 cut-over starts it); /etc/intercom-hub/intercom.toml in place; binary present"
else
  warn "  intercom-hub.service installed + enabled but /usr/local/bin/intercom-hub is ABSENT -- install the CI binary (intercom-hub-linux-amd64) before the M4 cut-over"
fi

# ---------------------------------------------------------------------------------------------
step 14 "Janus audiobridge audio edge (issue 1345 M3a: apt install janus + jcfg + ENABLE-ONLY)"
# The phones intercom leg (the VDO.Ninja replacement) runs over a Janus audiobridge room the hub
# joins as a plain-RTP PCMU participant (issue 1345 M3a). Install Janus (apt), generate the 0600 room
# secret if absent (NEVER printed), write the audiobridge room jcfg (the secret is injected via a bash
# var -- no argv exposure) + the WebSocket transport jcfg (ws BOUND to the LAN IP :8188, NO wss -- TLS
# terminates on the dev1 front) + the HTTP transport jcfg (bound loopback 127.0.0.1:8088 only), and
# ENABLE the janus unit. NEVER unconditionally start it (the M4 cut-over starts it with the intercom
# hub); Ubuntu's package auto-starts janus on install, so if it is ALREADY running we restart it to
# load the freshly written jcfg (issue 1345 M3 follow-up: the LAN-bound WS + loopback HTTP jcfg + the
# is-active-guarded restart).
DEBIAN_FRONTEND=noninteractive apt-get install -y janus \
  || warn "  apt-get install janus failed -- install it before the M4 cut-over (the phones leg needs the audiobridge)"
JANUS_ROOM="1000"
JANUS_SECRET_FILE="/etc/intercom-hub/janus-room.secret"
install -d -m 755 /etc/intercom-hub
if [ ! -f "$JANUS_SECRET_FILE" ]; then
  ( umask 077; openssl rand -hex 16 > "$JANUS_SECRET_FILE" ) \
    || fail "could not generate the Janus room secret at ${JANUS_SECRET_FILE} (openssl present?)"
  chmod 600 "$JANUS_SECRET_FILE"
  echo "  generated the Janus room secret (${JANUS_SECRET_FILE}, 0600 -- value never printed)"
else
  echo "  Janus room secret already present (${JANUS_SECRET_FILE}) -- leaving it"
fi
if [ -d /etc/janus ] || command -v janus >/dev/null 2>&1; then
  install -d -m 755 /etc/janus
  # Read the secret into a var and substitute the placeholder with a bash expansion (never an argv,
  # never an echo) before writing the audiobridge jcfg 0640.
  JANUS_SECRET_VALUE="$(cat "$JANUS_SECRET_FILE")"
  # issue 1352: pin the audiobridge plain-RTP bind to the box's static IP (STATIC_IP=$(strih_lx_ip),
  # the same source of truth the netplan step uses) via `local_ip` in `general` -- so the .203->.202
  # renumber can never strand the bind (EADDRNOTAVAIL) and block the hub from joining the room.
  JANUS_AB_JCFG="$(strih_janus_audiobridge_jcfg_text "$JANUS_ROOM" "$JANUS_SECRET_FILE" "$STATIC_IP")"
  JANUS_AB_JCFG="${JANUS_AB_JCFG//@JANUS_ROOM_SECRET@/$JANUS_SECRET_VALUE}"
  # Create it 0640 FROM BIRTH (umask 027 in a subshell) so the secret-bearing file is never briefly
  # world-readable between the write and a later chmod (F5); the chmod is belt-and-suspenders.
  ( umask 027; printf '%s\n' "$JANUS_AB_JCFG" > /etc/janus/janus.plugin.audiobridge.jcfg )
  chmod 640 /etc/janus/janus.plugin.audiobridge.jcfg
  unset JANUS_SECRET_VALUE JANUS_AB_JCFG
  strih_janus_ws_jcfg_text "$STATIC_IP" > /etc/janus/janus.transport.websockets.jcfg
  chmod 644 /etc/janus/janus.transport.websockets.jcfg
  strih_janus_http_jcfg_text > /etc/janus/janus.transport.http.jcfg
  chmod 644 /etc/janus/janus.transport.http.jcfg
  echo "  wrote /etc/janus/janus.plugin.audiobridge.jcfg (room ${JANUS_ROOM} 'interkom') + janus.transport.websockets.jcfg (ws :8188 LAN-bound, no wss) + janus.transport.http.jcfg (loopback 127.0.0.1:8088)"
  systemctl enable janus 2>/dev/null || warn "  could not enable janus.service (install janus first)"
  # Ubuntu's janus package auto-STARTS the service on install (before these jcfg existed). If it is
  # already running, restart it to load the freshly written room/ws/http jcfg; otherwise leave it
  # enable-only (the M4 cut-over starts it with the hub). Never an UNCONDITIONAL start/restart.
  if systemctl is-active --quiet janus; then
    systemctl restart janus 2>/dev/null || warn "  could not restart janus to load the new jcfg"
    echo "  janus was already running (apt auto-start) -- restarted to load the new jcfg"
  else
    echo "  janus.service ENABLED (NOT running -- the M4 cut-over starts it with the hub); HTTP loopback 127.0.0.1:8088"
  fi
else
  warn "  /etc/janus absent and no janus binary -- install janus, then re-run this step to write the jcfg + enable"
fi

# ---------------------------------------------------------------------------------------------
step 15 "Kiosk openbox autostart + root menu (issue 1357: OBS + Companion Satellite start in the Xorg openbox session)"
# issue 1357: the lightdm autologin -> openbox kiosk (baseline step 11) runs ~/.config/openbox/autostart
# at every boot -- the imag step-16 pattern. openbox never reaches graphical-session.target, so the
# --user units' WantedBy alone would never fire: the autostart STARTS strih-obs.service + the bundle-
# state server itself and launches Companion Satellite (openbox does not run XDG ~/.config/autostart,
# which is why the GNOME-era .desktop entries are removed below -- an XDG autostart that DID fire via
# systemd --user would double-launch). It carries the shared kiosk preamble (never-blank + OBS crash-
# sentinel clear) the baseline verify grades. The root menu is the SAME printer imag uses.
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" -m 755 "${USER_HOME}/.config/openbox"
strih_openbox_autostart_text > "${USER_HOME}/.config/openbox/autostart" \
  || fail "could not write ${USER_HOME}/.config/openbox/autostart"
chmod +x "${USER_HOME}/.config/openbox/autostart"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/openbox/autostart"
obs_box_openbox_menu_xml "$(strih_lx_hostname)" "systemctl --user start strih-obs.service" "/usr/local/bin/strih-obs-stop.sh" \
  > "${USER_HOME}/.config/openbox/menu.xml" \
  || fail "could not write ${USER_HOME}/.config/openbox/menu.xml"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${USER_HOME}/.config/openbox/menu.xml"
rm -f "${USER_HOME}/.config/autostart/companion-satellite.desktop" "${USER_HOME}/.config/autostart/obs.desktop"
rmdir "${USER_HOME}/.config/autostart" 2>/dev/null || true
echo "  ~/.config/openbox/autostart (display layout + kiosk preamble + strih-obs/bundle-state start + Companion Satellite) + menu.xml written for ${DESKTOP_USER}; GNOME-era XDG autostarts removed"

# ---------------------------------------------------------------------------------------------
step 16 "Bitfocus Companion Satellite (Stream Deck surface agent) -- desktop, launched by the openbox autostart"
# issue 1317: the owner caught this missing live -- the notebook's locally-attached Stream Deck was
# dead because Companion Satellite was never installed. Install the SATELLITE (NOT full Companion --
# the venue runs the Companion CONTROLLER at 10.77.9.205, this box only exposes its local surface to
# it) the DESKTOP way: a PINNED (never "latest") x64 tar.gz from the Bitfocus CDN, sha256-verified,
# via its own idempotent `install.sh --system --force` (-> /opt + the desktop uaccess udev rule).
# Then SEED the controller into the operator's app config.json (electron-store remoteIp/remotePort,
# confirmed from the v3.4.0 source) + write the operator-login AUTOSTART entry (owner rule: a needed
# feature is always-ON, never a forgettable manual launch) -- never a mid-provision start.
CS_HOST="$(strih_companion_satellite_host)"
CS_PORT="$(strih_companion_satellite_port)"
CS_VER="$(strih_companion_satellite_version)"
CS_APPCFG_DIR="${USER_HOME}/.config/Companion Satellite"
# Run the emitted install in a SUBSHELL (the step-4b NDI-runtime pattern): the emitter `exit 1`s on a
# fetch/sha/install failure, so a bare `eval "$(...)" || fail` would terminate setup-strih.sh directly
# and never reach `fail` (the repin hint). The subshell contains the exit, returns non-zero, and
# `|| fail` fires with the actionable message.
( eval "$(strih_companion_satellite_install)" ) \
  || fail "Companion Satellite install failed (v${CS_VER}) -- confirm the pinned tarball/sha256 (COMPANION_SATELLITE_TARBALL_URL / COMPANION_SATELLITE_SHA256) and re-run"
# Durable /etc record of the intended controller (the (companion) gate's human-readable paper trail).
install -d -m 755 /etc/companion-satellite
strih_companion_satellite_config_text "$CS_HOST" > /etc/companion-satellite/host.conf \
  || fail "could not write /etc/companion-satellite/host.conf"
# Pre-seed the operator's app config (electron-store; before first launch ensureFieldsPopulated fills
# the rest -- this is the FUNCTIONAL controller pin the (companion) gate reads). MERGE-in-place rather
# than clobber: overlay ONLY the controller keys onto any existing config.json (this provisioning run
# ENFORCES the intended controller, but must NOT drop the device id / app state the running Satellite
# writes into the same file, nor another key an operator added). A missing/corrupt file starts from {}.
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" -m 755 "$CS_APPCFG_DIR"
strih_companion_satellite_appconfig_json "$CS_HOST" "$CS_PORT" | python3 -c '
import json, sys
overlay = json.load(sys.stdin)
path = sys.argv[1]
try:
    with open(path) as f:
        cur = json.load(f)
    if not isinstance(cur, dict):
        cur = {}
except Exception:
    cur = {}
cur.update(overlay)
with open(path, "w") as f:
    json.dump(cur, f, indent=2)
    f.write("\n")
' "${CS_APPCFG_DIR}/config.json" \
  || fail "could not seed ${CS_APPCFG_DIR}/config.json"
chown "$DESKTOP_USER":"$DESKTOP_USER" "${CS_APPCFG_DIR}/config.json"
# issue 1317 (live 20.9.2026): the electron-store file seed alone left a RUNNING Satellite's effective
# controller host at 127.0.0.1 -- POST the controller to its local REST (:9999/api/config) so a
# running instance adopts it live (connected:true afterwards). Best-effort: a no-op when the Satellite
# is not up (a fresh box that seeds but never starts it), so it never aborts the provisioning run.
eval "$(strih_companion_satellite_rest_apply_cmd "$CS_HOST" "$CS_PORT")" || true
# issue 1357: launched by the kiosk openbox autostart (step 15, strih_companion_satellite_openbox_line) --
# never started mid-provision, and no XDG ~/.config/autostart entry (openbox does not run those).
echo "  Companion Satellite v${CS_VER} installed to /opt (launched by the openbox autostart for ${DESKTOP_USER}, NOT started now); controller ${CS_HOST}:${CS_PORT} seeded (config.json + host.conf)"

# ---------------------------------------------------------------------------------------------
# A lettered sub-step so TOTAL_STEPS stays 17 (test-pinned).
step "16b" "RustDesk remote desktop (owner request 22.9.2026) -- pinned .deb, permanent password from a 0600 file"
# issue 1317: install RustDesk from the PINNED .deb (sha256-verified, fail-loud on mismatch), enable
# --now the service, and apply the permanent password read INSIDE the emitted block from a 0600 file
# the SUPERVISOR places (a provisioning input) -- the @JANUS_ROOM_SECRET@ discipline: the password
# value never appears in git, in an argv here, or in a log. Gated on the password file's presence:
# without it a permanent password cannot be set, so we SKIP + warn (never invent one, never abort the
# whole provisioning run for a not-yet-placed supervisor secret). Run in a SUBSHELL (the step-4b/-16
# pattern): the emitter `exit 1`s on a fetch/sha/install failure, so `( eval "$(...)" ) || fail` fires
# with the actionable message instead of terminating setup-strih.sh directly.
RD_VER="$(strih_rustdesk_version)"
RD_URL="$(strih_rustdesk_deb_url)"
RD_SHA="$(strih_rustdesk_deb_sha256)"
RD_PW_FILE="${STRIH_LX_RUSTDESK_PW_FILE:-/etc/rustdesk/permanent-password.secret}"
if [ -s "$RD_PW_FILE" ]; then
  ( eval "$(strih_rustdesk_install_cmds "$RD_VER" "$RD_URL" "$RD_SHA" "$RD_PW_FILE")" ) \
    || fail "RustDesk install failed (v${RD_VER}) -- confirm the pinned .deb URL/sha256 and the 0600 password file ${RD_PW_FILE}, then re-run"
  RD_ID="$(rustdesk --get-id 2>/dev/null | head -1 || true)"
  echo "  RustDesk v${RD_VER} installed + enabled --now (permanent password applied from ${RD_PW_FILE}); connect ID: ${RD_ID:-<run: rustdesk --get-id>}"
else
  warn "  RustDesk NOT installed -- place the 0600 permanent-password file at ${RD_PW_FILE} (a provisioning input the supervisor sets; never in git) and re-run this step"
fi

# ---------------------------------------------------------------------------------------------
# A lettered sub-step so TOTAL_STEPS stays 17 (test-pinned).
step "16c" "bkshading shading-control service (issue 1353) -- CI artifact -> /opt/bkshading, seed config + unit, enable-only"
# issue 1353: the shading panel BACKEND (bkshading/service) ran only on the Windows strih PC; post-M4
# the notebook is the strih, so it is provisioned here as a systemd unit fed the CI-built Linux
# artifact. The panel web assets are EMBEDDED in the binary (bkshading/service/src/http.rs), so the
# unit needs only the self-contained binary; web/ is installed beside it (design) as a panel-source
# copy. ENABLE-ONLY: the SUPERVISOR deploys the running service; the Windows service stays the
# fallback until the owner accepts it.
BKSH_ART="$(strih_bkshading_artifact_name)"
BKSH_SRC=""
if [ -d "${STRIH_LX_BKSHADING_SRC:-}" ]; then
  # Supervisor pre-staged the extracted artifact (the step-4 STRIH_LX_BUNDLE_SRC precedent).
  BKSH_SRC="${STRIH_LX_BKSHADING_SRC%/}"
  echo "  using pre-staged bkshading artifact from ${BKSH_SRC}"
elif [ -n "${GH_TOKEN:-}" ]; then
  # Fetch the CI artifact via the curl+GH_TOKEN pattern (the bundle-state step-9 precedent, extended
  # to the GitHub artifacts API for the zip). A token WITHOUT actions:read -> empty URL -> the warn
  # branch below (the supervisor then pre-stages via STRIH_LX_BKSHADING_SRC or places the binary).
  BKSH_TMP="$(mktemp -d)"
  BKSH_DL="$(curl -fsSL -H "Authorization: token ${GH_TOKEN}" -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/${STRIH_LX_GH_REPO:-zbynekdrlik/camera-box}/actions/artifacts?name=${BKSH_ART}&per_page=20" 2>/dev/null \
    | python3 -c 'import json,sys
try:
    arts=[a for a in json.load(sys.stdin).get("artifacts", []) if not a.get("expired")]
    print(arts[0]["archive_download_url"] if arts else "")
except Exception:
    print("")' || true)"
  if [ -n "$BKSH_DL" ] && curl -fsSL -H "Authorization: token ${GH_TOKEN}" -L "$BKSH_DL" -o "${BKSH_TMP}/art.zip" 2>/dev/null; then
    if python3 -c 'import sys,zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "${BKSH_TMP}/art.zip" "${BKSH_TMP}/x" 2>/dev/null; then
      BKSH_SRC="${BKSH_TMP}/x"
      echo "  fetched the ${BKSH_ART} CI artifact"
    else
      warn "  could not extract the ${BKSH_ART} artifact zip"
    fi
  else
    warn "  could not fetch the ${BKSH_ART} CI artifact (GH_TOKEN lacks actions:read, or no successful run yet)"
  fi
else
  warn "  GH_TOKEN unset and STRIH_LX_BKSHADING_SRC not a dir -- fetch the ${BKSH_ART} artifact and install /opt/bkshading/bkshading (+ web/) before enabling"
fi
# Install the binary (+ web/ beside it) when a source resolved (robust to any nesting in the zip).
if [ -n "$BKSH_SRC" ]; then
  BKSH_BIN="$(find "$BKSH_SRC" -type f -name bkshading 2>/dev/null | head -1 || true)"
  BKSH_WEB="$(find "$BKSH_SRC" -type d -name web 2>/dev/null | head -1 || true)"
  [ -n "$BKSH_BIN" ] || fail "issue 1353: bkshading binary not found in the ${BKSH_ART} artifact"
  install -d -m 0755 -o root -g root /opt/bkshading
  install -m 0755 -o root -g root "$BKSH_BIN" /opt/bkshading/bkshading
  if [ -n "$BKSH_WEB" ]; then
    rm -rf /opt/bkshading/web
    cp -a "$BKSH_WEB" /opt/bkshading/web
    chown -R root:root /opt/bkshading/web
    find /opt/bkshading/web -type d -exec chmod 0755 {} + 2>/dev/null || true
    find /opt/bkshading/web -type f -exec chmod 0644 {} + 2>/dev/null || true
  fi
  echo "  installed bkshading service binary -> /opt/bkshading/bkshading (panel assets embedded; web/ copied beside it)"
fi
# Remove the artifact-fetch temp dir (only created in the GH_TOKEN branch).
[ -n "${BKSH_TMP:-}" ] && rm -rf "$BKSH_TMP" 2>/dev/null || true
# Seed the operator config ONLY IF absent (the projector.json / bkshading.example.toml precedent).
install -d -m 0755 /etc/bkshading
if [ ! -f /etc/bkshading/bkshading.toml ]; then
  strih_bkshading_config_text > /etc/bkshading/bkshading.toml \
    || fail "could not seed /etc/bkshading/bkshading.toml"
  echo "  seeded /etc/bkshading/bkshading.toml (cam1 + handhelds; edit for the live camera set)"
else
  echo "  /etc/bkshading/bkshading.toml already present -- leaving the operator's config"
fi
# Install the SYSTEM unit from the printer (source of truth) + ENABLE (NEVER start -- the supervisor
# deploys the running service; the Windows service stays the fallback until the owner accepts).
strih_bkshading_unit_text > /etc/systemd/system/bkshading-service.service \
  || fail "could not write /etc/systemd/system/bkshading-service.service"
systemctl daemon-reload
systemctl enable bkshading-service.service 2>/dev/null || warn "  could not enable bkshading-service.service"
if [ -x /opt/bkshading/bkshading ]; then
  echo "  bkshading-service.service installed + enabled (NOT started -- supervisor deploys the running service); binary present"
else
  warn "  bkshading-service.service installed + enabled but /opt/bkshading/bkshading is ABSENT -- install the CI ${BKSH_ART} binary before go-live"
fi

# ---------------------------------------------------------------------------------------------
step 17 "Final verification (verify-strih.sh acceptance gate)"
if [ -x "${HERE}/verify-strih.sh" ]; then
  if strih_lx_reboot_pending "$(cat /proc/cmdline 2>/dev/null || true)"; then
    # issue 1357: the baseline's kernel / PRIME / Xorg-kiosk changes only run after the next boot, so
    # the gate's baseline items are EXPECTED to report them pending on this run -- report, never fail
    # provisioning here; the post-reboot verify-strih.sh run is the acceptance gate.
    warn "  the shared OBS-box baseline takes effect at the NEXT boot -- reboot ${BOX_NAME}, then run verify-strih.sh --box ${STRIH_FACT_BOX} (the run below only reports what is still pending)"
    "${HERE}/verify-strih.sh" --box "$STRIH_FACT_BOX" || warn "  verify-strih.sh reports pending items -- expected before the reboot"
  else
    "${HERE}/verify-strih.sh" --box "$STRIH_FACT_BOX" || fail "verify-strih.sh acceptance gate did not pass"
  fi
else
  warn "  verify-strih.sh not found/executable next to this script -- run it manually"
fi

echo -e "${GREEN}=== ${BOX_NAME} setup complete ===${NC}"
