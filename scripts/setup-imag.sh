#!/bin/bash
#
# imag-nb One-Shot Setup (#458) — provision a Linux notebook as the 60fps IMAG OBS box
#
# Runs ON the imag box as root (same model as setup-device.sh). Idempotent — safe to re-run.
# After it finishes, run scripts/imag_scenes.py from dev1 to seed the OBS profile/scenes over
# WebSocket (and later `imag_scenes.py --projector` once the HDMI monitor is connected).
#
# Usage (on the box):
#   sudo CAM_PW=<fleet-pw> GH_TOKEN=<gh-pat-with-repo-scope> ./setup-imag.sh [--yes]
#
# GH_TOKEN (repo-read scope) is required for step 6: imag-nb runs the CUSTOMIZED genlock
# OBS+DistroAV build (#460), hot-swapped over the PPA base — the artifacts are GitHub Actions
# workflow artifacts on this PRIVATE repo, which `gh run download` needs auth to fetch.
#
# Topology (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md):
#   6× cam box NDI 1080p60 -> imag-nb OBS (1080p60 low-latency IMAG, genlock build #460) ->
#   HDMI program projector
#
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

STATIC_IP="10.77.9.182"
PREFIX="23"
NDI_PEER="10.77.9.61"            # cam1 — fleet NDI runtime source (6.3.2)
NDI_DIR="/usr/lib/ndi"
DESKTOP_USER="newlevel"
USER_HOME="/home/${DESKTOP_USER}"
OBS_CFG="${USER_HOME}/.config/obs-studio"
TOTAL_STEPS=10

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

[ "$EUID" -eq 0 ] || fail "run as root (sudo)"
id "$DESKTOP_USER" >/dev/null 2>&1 || fail "user $DESKTOP_USER missing"

ASSUME_YES=0
[ "${1:-}" = "--yes" ] || [ "${1:-}" = "-y" ] && ASSUME_YES=1
if [ "$ASSUME_YES" -ne 1 ]; then
    read -p "Provision this box as imag-nb (${STATIC_IP})? (y/N) " -n 1 -r; echo
    [[ $REPLY =~ ^[Yy]$ ]] || { echo "Aborted."; exit 1; }
fi

# Pre-flight: curl + CA certs BEFORE first use (the cam5/#450 lesson — a base image without
# curl makes every download step fail silently mid-run; ensure it up-front, fail loud).
if ! command -v curl >/dev/null 2>&1; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates >/dev/null \
        || fail "cannot install curl — network/apt broken"
fi

# =============================================================================
step 1 "Static IP ${STATIC_IP}/${PREFIX} (NetworkManager — desktop Ubuntu)"
# =============================================================================
# The box got .182 via DHCP; pin the SAME address static so it never drifts (the cam6
# lesson: a DHCP lease change looks like a dead box). Same IP -> nmcli re-apply is safe
# over this very ssh session.
if command -v nmcli >/dev/null 2>&1; then
    NIC=$(ip -4 route get "$NDI_PEER" | grep -oP 'dev \K\S+' | head -1)
    [ -n "$NIC" ] || fail "cannot resolve NIC toward the rig LAN"
    CON=$(nmcli -t -f NAME,DEVICE con show --active | awk -F: -v d="$NIC" '$2==d{print $1; exit}')
    [ -n "$CON" ] || fail "no active NetworkManager connection on $NIC"
    GW=$(ip -4 route show default | grep -oP 'via \K\S+' | head -1)
    DNS=$(resolvectl dns "$NIC" 2>/dev/null | grep -oP '(\d+\.){3}\d+' | head -2 | tr '\n' ' ')
    [ -n "$DNS" ] || DNS="$GW"
    nmcli con mod "$CON" ipv4.method manual \
        ipv4.addresses "${STATIC_IP}/${PREFIX}" ipv4.gateway "$GW" ipv4.dns "${DNS// /,}"
    nmcli con up "$CON" >/dev/null || true   # same IP — session survives
    echo "  static ${STATIC_IP}/${PREFIX} gw=$GW dns=$DNS on $NIC ($CON)"
else
    fail "nmcli missing — desktop Ubuntu expected (netplan-only path not implemented)"
fi

# =============================================================================
step 2 "Max performance: governor + no USB/NIC powersave (USB-ethernet feeds the NDI!)"
# =============================================================================
cat > /etc/systemd/system/cpu-performance.service <<'EOF'
[Unit]
Description=Set CPU governor to performance
After=multi-user.target
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done'
[Install]
WantedBy=multi-user.target
EOF
cat > /etc/rc.local <<'EOF'
#!/bin/bash
# imag-nb boot tuning (fleet parity): governor + USB autosuspend off (USB NIC!) + NIC powersave off
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > "$g"; done
for u in /sys/bus/usb/devices/*/power/control; do echo on > "$u" 2>/dev/null; done
for n in /sys/class/net/*/device/power/control; do echo on > "$n" 2>/dev/null; done
exit 0
EOF
chmod +x /etc/rc.local
systemctl daemon-reload
systemctl enable --now cpu-performance.service >/dev/null 2>&1
bash /etc/rc.local
grep -q performance /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor || fail "governor not performance"

# =============================================================================
step 3 "Never sleep: lid ignore + sleep masked + GNOME idle/blank/lock off"
# =============================================================================
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/99-imag-no-sleep.conf <<'EOF'
[Login]
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
IdleAction=ignore
EOF
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target >/dev/null 2>&1 || true
systemctl restart systemd-logind
UBUS="unix:path=/run/user/$(id -u $DESKTOP_USER)/bus"
gs() { sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" gsettings set "$@" 2>/dev/null || true; }
gs org.gnome.desktop.session idle-delay 0
gs org.gnome.settings-daemon.plugins.power sleep-inactive-ac-type "'nothing'"
gs org.gnome.settings-daemon.plugins.power sleep-inactive-battery-type "'nothing'"
gs org.gnome.desktop.screensaver lock-enabled false
gs org.gnome.desktop.screensaver idle-activation-enabled false

# =============================================================================
step 4 "NDI runtime 6.3.2 from cam1 -> ${NDI_DIR} (fleet-identical)"
# =============================================================================
if [ ! -e "${NDI_DIR}/libndi.so.6" ]; then
    [ -n "${CAM_PW:-}" ] || fail "CAM_PW env required to fetch NDI runtime from cam1"
    command -v sshpass >/dev/null 2>&1 || apt-get install -y sshpass >/dev/null
    mkdir -p "$NDI_DIR"
    sshpass -p "$CAM_PW" scp -O -o StrictHostKeyChecking=no \
        "${DESKTOP_USER}@${NDI_PEER}:/usr/lib/ndi/libndi.so.*.*.*" "$NDI_DIR/" \
        || fail "NDI copy from cam1 failed"
    ( cd "$NDI_DIR" && REAL=$(ls libndi.so.*.*.* | head -1) && ln -sf "$REAL" libndi.so.6 && ln -sf libndi.so.6 libndi.so )
fi
echo "$NDI_DIR" > /etc/ld.so.conf.d/ndi.conf
ldconfig
# no `grep -q` on a pipe under pipefail — -q's early close SIGPIPEs ldconfig and fails the pipeline
ldconfig -p | grep libndi >/dev/null || fail "libndi not in linker cache"
# DistroAV's Linux loader (plugin-main.cpp load_ndilib) scans ONLY /usr/lib, /usr/lib64,
# /usr/local/lib (non-recursive — NOT the multiarch dir, NOT the ld cache) for libndi.so.<N>.
# Live-proven on imag-nb: without this symlink DistroAV logs ERR-404 despite a valid ld cache.
ln -sf "$(readlink -f "$NDI_DIR"/libndi.so.6)" /usr/local/lib/libndi.so.6
apt-get install -y avahi-daemon >/dev/null 2>&1 || true
systemctl enable --now avahi-daemon >/dev/null 2>&1

# =============================================================================
step 5 "OBS Studio (official PPA, 32.x) — base install; libobs.so.30 gets genlock hot-swapped next"
# =============================================================================
if ! command -v obs >/dev/null 2>&1; then
    add-apt-repository -y ppa:obsproject/obs-studio >/dev/null
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y obs-studio >/dev/null
fi
obs --version 2>/dev/null || true

# =============================================================================
step 6 "Genlock hot-swap (#460): deploy patched libobs.so.30 + distroav.so over the PPA base"
# =============================================================================
# imag-nb MUST run the CUSTOMIZED genlock OBS+DistroAV, not stock DistroAV (user directive,
# #458 comment 2026-07-03) — the stock-bootstrap path this step used to run is dead. The PPA
# obs-studio package installed in step 5 ships libobs.so.30 with SONAME "libobs.so.30"
# (live-verified on imag-nb) — IDENTICAL to the genlock build's own SONAME — so the genlock
# libobs.so.30 hot-swaps cleanly over it, exactly mirroring the Windows obs.dll hot-swap (see
# .claude/skills/genlock). Only libobs.so.30 (the genlock render-tick/ts-align patches live in
# obs-source.c/obs-video.c, both inside libobs core) and distroav.so are swapped —
# obs-frontend-api/obs-opengl/obs-scripting are untouched by the genlock patches and stay
# PPA-stock. No stock DistroAV .deb is installed any more — the genlock-built distroav.so IS
# the plugin.
GENLOCK_REPO="zbynekdrlik/camera-box"
GENLOCK_WORKFLOW="linux-genlock.yml"
GENLOCK_MARKER_DIR="/opt/obs-genlock"
GENLOCK_BACKUP_ROOT="/opt/obs-backup"
LIBOBS_REAL="/usr/lib/x86_64-linux-gnu/libobs.so.30"
DISTROAV_REAL="/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so"

command -v jq >/dev/null 2>&1 || apt-get install -y jq >/dev/null 2>&1 || fail "jq install failed (needed for #460 manifest verify)"

# #120 bundle-manifest self-consistency check, INLINE (jq): setup-imag.sh is copied to the box
# standalone (no sibling scripts/genlock-manifest.sh checked out here), so it cannot shell out to
# the repo's own manifest tool the way the CI build does — this re-implements the SAME per-file
# sha256 check jq-side. A `<<<` here-string (never a `| while` pipe) keeps the loop in THIS shell
# so fail()'s `exit 1` actually aborts the script — the same pipefail subshell trap already
# documented for the ldconfig check in step 4 (a piped subshell's exit would NOT abort the script).
verify_bundle_manifest() {
    local manifest="$1" stage="$2" entries relpath want_sha got_sha f
    entries="$(jq -r '.files[] | "\(.path)\t\(.sha256)"' "$manifest")" || fail "cannot parse $manifest"
    [ -n "$entries" ] || fail "$manifest lists zero files — refuse to trust an empty #460 manifest"
    while IFS=$'\t' read -r relpath want_sha; do
        [ -n "$relpath" ] || continue
        f="$stage/$relpath"
        [ -f "$f" ] || fail "#460 manifest lists $relpath but it is missing from the downloaded bundle"
        got_sha="$(sha256sum "$f" | awk '{print $1}')"
        [ "$got_sha" = "$want_sha" ] || \
            fail "#460 manifest sha mismatch for $relpath (want $want_sha got $got_sha) — corrupted/tampered artifact"
    done <<< "$entries"
}

DEPLOYED_SHA=""
[ -f "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" ] && DEPLOYED_SHA="$(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt")"

if ! command -v gh >/dev/null 2>&1; then
    GH_DEB_URL=$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest \
        | grep -oE '"browser_download_url": *"[^"]*linux_amd64\.deb"' | grep -oE 'https[^"]*' | head -1)
    [ -n "$GH_DEB_URL" ] || fail "no gh CLI linux_amd64 .deb asset found on latest cli/cli release"
    curl -fsSL -o /tmp/gh-cli.deb "$GH_DEB_URL"
    DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/gh-cli.deb >/dev/null \
        || dpkg -i /tmp/gh-cli.deb || fail "gh CLI install failed"
fi
command -v gh >/dev/null 2>&1 || fail "gh CLI still missing after install attempt"
[ -n "${GH_TOKEN:-}" ] || fail "GH_TOKEN env required (repo-read scope) to download the private #460 CI artifact"

# GENLOCK_RUN_ID overrides run resolution (pin a specific build). Default: the latest SUCCESSFUL
# linux-genlock.yml run on ANY branch — the workflow triggers ONLY on push to dev (never main;
# `gh run list -w linux-genlock.yml -b main` is EMPTY, live-verified), so branch-filtering to
# main would find nothing — the newest successful dev run IS the current genlock source state.
RUN_ID="${GENLOCK_RUN_ID:-}"
if [ -z "$RUN_ID" ]; then
    RUN_ID="$(gh run list --repo "$GENLOCK_REPO" --workflow "$GENLOCK_WORKFLOW" \
        --status success --limit 1 --json databaseId -q '.[0].databaseId')"
    [ -n "$RUN_ID" ] || fail "no successful $GENLOCK_WORKFLOW run found on $GENLOCK_REPO"
fi

GENLOCK_TMP="$(mktemp -d)"
trap 'rm -rf "$GENLOCK_TMP"' EXIT

gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n obs-genlock-linux-x86_64 --dir "$GENLOCK_TMP/bundle" \
    || fail "download of obs-genlock-linux-x86_64 failed (run $RUN_ID)"
gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n distroav-linux-fast-so --dir "$GENLOCK_TMP/fast" \
    || fail "download of distroav-linux-fast-so failed (run $RUN_ID)"

[ -f "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt" ] || fail "bundle missing GENLOCK_BUILD_SHA.txt"
NEW_SHA="$(cat "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt")"
[ -f "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt" ] || fail "distroav-linux-fast-so missing DISTROAV_BUILD_SHA.txt"
FAST_SHA="$(cat "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt")"
[ "$NEW_SHA" = "$FAST_SHA" ] || \
    fail "bundle build SHA ($NEW_SHA) != distroav-linux-fast-so build SHA ($FAST_SHA) — mismatched artifacts from different runs, refuse to deploy"

BUNDLE_LIBOBS="$GENLOCK_TMP/bundle/lib/x86_64-linux-gnu/libobs.so.30"
[ -f "$BUNDLE_LIBOBS" ] || fail "bundle missing lib/x86_64-linux-gnu/libobs.so.30"
FAST_DISTROAV="$GENLOCK_TMP/fast/distroav.so"
[ -f "$FAST_DISTROAV" ] || fail "distroav-linux-fast-so missing distroav.so"
[ -f "$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json" ] || fail "bundle missing BUNDLE_MANIFEST.json (#120)"

verify_bundle_manifest "$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json" "$GENLOCK_TMP/bundle"

if [ "$DEPLOYED_SHA" = "$NEW_SHA" ] && [ -f "$LIBOBS_REAL" ] && [ -f "$DISTROAV_REAL" ]; then
    echo "  genlock build $NEW_SHA already deployed — no-op"
else
    mkdir -p "$GENLOCK_MARKER_DIR"
    BACKUP_DIR="$GENLOCK_BACKUP_ROOT/$(date +%Y-%m-%d-%H%M%S)-458"
    mkdir -p "$BACKUP_DIR"
    [ -f "$LIBOBS_REAL" ] && cp -a "$LIBOBS_REAL" "$BACKUP_DIR/libobs.so.30"
    [ -f "$DISTROAV_REAL" ] && cp -a "$DISTROAV_REAL" "$BACKUP_DIR/distroav.so"
    # ONE permanent stock-PPA backup, created once and never overwritten again — the forever
    # rollback-to-stock path (mirrors the Windows C:\obs-backup\pre-<N> convention).
    if [ ! -f "${LIBOBS_REAL}.bak" ] && [ -f "$LIBOBS_REAL" ]; then cp -a "$LIBOBS_REAL" "${LIBOBS_REAL}.bak"; fi
    if [ ! -f "${DISTROAV_REAL}.bak" ] && [ -f "$DISTROAV_REAL" ]; then cp -a "$DISTROAV_REAL" "${DISTROAV_REAL}.bak"; fi

    install -m 0644 -o root -g root "$BUNDLE_LIBOBS" "$LIBOBS_REAL" || fail "libobs.so.30 hot-swap install failed"
    install -m 0644 -o root -g root "$FAST_DISTROAV" "$DISTROAV_REAL" || fail "distroav.so hot-swap install failed"
    ldconfig

    # SONAME sanity check (no `-q` on a piped external command under pipefail — same early-close
    # SIGPIPE footgun documented for the step-4 ldconfig check; read the full small output instead).
    readelf -d "$LIBOBS_REAL" 2>/dev/null | grep 'SONAME.*\[libobs\.so\.30\]' >/dev/null \
        || fail "post-swap libobs.so.30 SONAME check failed — refuse a mismatched ABI"

    echo "$NEW_SHA" > "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt"
    echo "$FAST_SHA" > "$GENLOCK_MARKER_DIR/DISTROAV_BUILD_SHA.txt"
    cp -a "$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json" "$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json"
    date -Is > "$GENLOCK_MARKER_DIR/DEPLOYED_AT"

    # Prevent an unattended `apt upgrade` from silently reverting the hot-swap back to PPA/stock
    # bytes (dpkg still tracks these two files under obs-studio/distroav) — drift must be a
    # deliberate re-run of this script, never a background package update.
    apt-mark hold obs-studio distroav >/dev/null 2>&1 || true

    # A swap while OBS is already running (a later re-run onto a NEWER build) needs OBS to
    # relaunch to pick up the new .so — mirrors the Windows force-kill-then-relaunch convention.
    # On the FIRST provisioning run OBS is not up yet (step 9 launches it fresh); step 9's own
    # `pgrep` guard would otherwise skip (re)launching an already-running, stale-lib OBS.
    if pgrep -x obs >/dev/null 2>&1; then
        pkill -x obs || true
        sleep 2
    fi
    echo "  genlock build $NEW_SHA deployed (was: ${DEPLOYED_SHA:-none, PPA stock})"
fi

# =============================================================================
step 7 "OBS pre-seed: WebSocket :4455 no-auth + SaveProjectors + no first-run wizard"
# =============================================================================
mkdir -p "$OBS_CFG"
seed_ini() {  # seed_ini FILE  — append our sections only if the file has no [OBSWebSocket] yet
    local f="$1"
    touch "$f"
    if ! grep -q '^\[OBSWebSocket\]' "$f"; then
        cat >> "$f" <<'EOF'

[OBSWebSocket]
ServerEnabled=true
ServerPort=4455
AuthRequired=false
FirstLoad=false
EOF
    fi
    if ! grep -q '^SaveProjectors=' "$f"; then
        printf '\n[BasicWindow]\nSaveProjectors=true\n' >> "$f"
    fi
    if ! grep -q '^LastVersion=' "$f"; then
        printf '\n[General]\nLastVersion=536936450\n' >> "$f"   # 32.1.2 — suppress first-run wizard
    fi
}
seed_ini "$OBS_CFG/global.ini"
seed_ini "$OBS_CFG/user.ini"
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$OBS_CFG"

# =============================================================================
step 8 "Desktop icon + autostart (reboot lands cutting-ready)"
# =============================================================================
APP_DESKTOP=$(ls /usr/share/applications/com.obsproject.Studio.desktop 2>/dev/null || true)
mkdir -p "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"
if [ -n "$APP_DESKTOP" ]; then
    cp -f "$APP_DESKTOP" "$USER_HOME/.config/autostart/obs.desktop"
    cp -f "$APP_DESKTOP" "$USER_HOME/Desktop/obs.desktop"
    chmod +x "$USER_HOME/Desktop/obs.desktop"
    sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        gio set "$USER_HOME/Desktop/obs.desktop" metadata::trusted true 2>/dev/null || true
fi
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/autostart" "$USER_HOME/Desktop"

# =============================================================================
step 9 "Launch OBS on the desktop session (X11 :0)"
# =============================================================================
if ! pgrep -x obs >/dev/null; then
    sudo -u "$DESKTOP_USER" DISPLAY=:0 DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        nohup obs >/tmp/obs-launch.log 2>&1 &
    sleep 8
fi
pgrep -x obs >/dev/null || fail "OBS did not start (see /tmp/obs-launch.log)"

# =============================================================================
step 10 "Verify: WebSocket :4455 + genlock render tick + DistroAV/NDI loaded"
# =============================================================================
for i in $(seq 1 15); do
    if (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; then exec 3>&-; echo "  WS :4455 up"; break; fi
    [ "$i" -eq 15 ] && fail "OBS WebSocket :4455 not listening"
    sleep 2
done

# Genlock log verify — the Linux equivalent of scripts/launch-obs-genlock.sh's Windows
# log-verify (#257 proof): the OBS log is the AUTHORITATIVE runtime signal a stock/wrong build
# cannot fake. Same regex family as scripts/drift-guard.sh genlock_capability_from_log.
OBS_LOG_DIR="$OBS_CFG/logs"
LATEST_LOG="$(ls -t "$OBS_LOG_DIR"/*.txt 2>/dev/null | head -1)"
[ -n "$LATEST_LOG" ] || fail "no OBS log found in $OBS_LOG_DIR — cannot verify the genlock build"
LOG_TEXT="$(cat "$LATEST_LOG")"
echo "$LOG_TEXT" | grep -iE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' >/dev/null \
    || fail "OBS log shows NO genlock capability marker in '$LATEST_LOG' — NOT the genlock build (check the #460 hot-swap in step 6)"
echo "  genlock render tick ENABLED (#460 build proof)"
if echo "$LOG_TEXT" | grep -i '\[distroav\] plugin loaded' >/dev/null; then
    echo "  DistroAV plugin loaded"
else
    echo "  WARNING: no '[distroav] plugin loaded' line yet (may log lazily on first NDI activation)"
fi
if echo "$LOG_TEXT" | grep -i 'NDI library initialized' >/dev/null; then
    echo "  NDI runtime loaded"
else
    echo "  WARNING: no 'NDI library initialized' line yet"
fi

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}imag-nb base provisioning DONE (genlock build: $(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" 2>/dev/null || echo unknown))${NC}"
echo -e "${GREEN}NEXT (from dev1): scripts/imag_scenes.py --host ${STATIC_IP}   # profile+scenes${NC}"
echo -e "${GREEN}       then:      scripts/imag_scenes.py --host ${STATIC_IP} --projector   # once HDMI monitor connected${NC}"
echo -e "${GREEN}========================================${NC}"
