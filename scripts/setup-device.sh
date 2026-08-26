#!/bin/bash
#
# Camera-Box Device Setup Script
# Sets up a clean Ubuntu installation as a camera-box appliance
#
# Usage: ./setup-device.sh [--binary <url|path>] DEVICE_NAME
# Example: ./setup-device.sh CAM5        (case-insensitive; cam5 works too)
#
# DEVICE_NAME is resolved via scripts/camera-set.sh (#24/#451 -- the single source of truth for
# the cam1-6 fleet map): IP address / VBAN stream name / genlock emit-rate are all DERIVED from
# it, never passed as free-text positional args (#450). An unknown name fails loudly through
# camera-set.sh's fail-closed `case` -- never silently provisions the wrong box.
#

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cli-log.sh
. "$HERE/lib/cli-log.sh"  # RED/GREEN/YELLOW/BLUE/NC + log()/info()/warn()/err() (#559/#568) --
                          # this script keeps its own fail() below (different shape/behavior:
                          # a hard exit, "FAIL: msg" not "[ERROR] msg" -- so it stays local rather
                          # than folding into cli-log.sh's err()).

# fail MSG -- print in red to stderr and exit non-zero. This is a ONE-SHOT provisioner
# (script-failure-policy): every install step that could otherwise leave the box
# half-configured (binary/NDI/ALSA/dantesync) fails loud here instead of warn-and-continue (#450).
fail() {
    echo -e "${RED}FAIL: $1${NC}" >&2
    exit 1
}

# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"   # camera_resolve() -- NAME -> IP / VBAN stream / genlock FPS (#450)

# shellcheck source=scripts/lib/log-bound.sh
. "$HERE/lib/log-bound.sh"  # log_bound_logrotate_config/log_bound_timer_dropin (#679) -- also
                            # sourced (unmodified) by verify-device.sh's (s) check and
                            # create-usb-linux.sh, single source of truth for the size cap + paths

# shellcheck source=scripts/lib/log-diet.sh
. "$HERE/lib/log-diet.sh"  # log_diet_journald_dropin (#762) -- also sourced (unmodified) by
                           # verify-device.sh's (u) check and create-usb-linux.sh, single source
                           # of truth for the journald RuntimeMaxUse cap path/value

# shellcheck source=scripts/lib/udev-camera-box.sh
. "$HERE/lib/udev-camera-box.sh"  # udev_camera_box_rules_content/udev_camera_box_helper_script_content
                                   # (#894) -- also sourced (unmodified) by verify-device.sh's (w)
                                   # check and create-usb-linux.sh, single source of truth for the
                                   # conditional restart-on-hotplug + autosuspend-on-readd udev rule
# shellcheck source=scripts/lib/camera-box-free-device.sh
. "$HERE/lib/camera-box-free-device.sh"  # camera_box_free_capture_device_script_content /
                                          # camera_box_free_capture_device_dropin_content (#772) --
                                          # also sourced by verify-device.sh's (y) check; the
                                          # ExecStartPre device-free bake-in for camera-box.service
# shellcheck source=scripts/lib/interkom-audio.sh
. "$HERE/lib/interkom-audio.sh"  # interkom_asound_conf_content / interkom_mic_pct / interkom_pcm_pct
                                  # (#782) -- also sourced by verify-device.sh's (aa) check, single
                                  # source of truth for the by-NAME asound.conf + per-box interkom
                                  # Mic/PCM mixer-gain table (STEP 5 write + STEP 16 amixer apply)

# shellcheck source=scripts/lib/dscp-nft.sh
. "$HERE/lib/dscp-nft.sh"  # dscp_nft_ruleset_content / dscp_nft_service_unit_content (dantesync
                           # issue 52) -- also sourced by verify-device.sh's (ae) check +
                           # create-usb-linux.sh, single source of truth for the NTP-client DSCP
                           # nftables OUTPUT-mangle rule (udp dport 123 -> dscp ef) + its boot oneshot

# GitHub repo + CI dev-build channel for installing the fleet-matching binary (#457 -- the fleet
# runs CI dev-builds, e.g. 1.7.0-dev.157, never a GitHub release; see STEP 3 below).
GITHUB_REPO="zbynekdrlik/camera-box"
CI_BRANCH="${CAMERA_BOX_CI_BRANCH:-dev}"

# Fleet NDI runtime source -- the licensed .so is never built by CI, so a fresh box fetches it
# from a known-good fleet peer instead of requiring a manual per-box scp copy (STEP 4, #457).
NDI_PEER="${CAMERA_BOX_NDI_PEER:-10.77.9.61}"
NDI_PEER_PW="${CAM_PW:-newlevel}"

# --- PURE functions (no root, no network, no side effects -- sourced + unit-tested from
# tests/setup_device_pure_functions.rs; the BASH_SOURCE guard below skips the destructive
# provisioning flow when sourced. Same convention as scripts/setup-imag.sh.) --------------------

# resolve_device_name NAME -- resolves a single fleet camera name (case-insensitive: CAM5, cam5,
# Cam5 all equivalent -- the historical hostname convention is uppercase) to DEVICE_NAME
# (uppercase hostname, e.g. CAM5) / DEVICE_IP / VBAN_STREAM (lowercase stream name, e.g. cam5) /
# CAMERA_GENLOCK_FPS (per-cam emit rate, #451), via scripts/camera-set.sh's camera_resolve().
# Fails loud on an unknown/empty name: camera-set.sh's own fail-closed `case` already rejects it
# and prints why; this just turns that nonzero return into a hard exit instead of letting a
# careless caller ignore it and provision the wrong box.
resolve_device_name() {
    local raw="${1:-}"
    local lc
    lc="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"
    camera_resolve "$lc" || exit 1
    DEVICE_NAME="$(printf '%s' "$CAMERA_NAME" | tr '[:lower:]' '[:upper:]')"
    DEVICE_IP="$CAMERA_IP"
    VBAN_STREAM="$CAMERA_NAME"
}

# #528 design pivot (2026-07-08): this script used to carry config_toml_display_section() /
# execstart_display_flag() here, wiring a per-cam CAMERA_DISPLAY_SOURCE / CAMERA_DISPLAY_
# EXECSTART_SOURCE table entry (scripts/camera-set.sh) into either config.toml's [display]
# section or a baked ExecStart --display flag. The owner rejected the whole per-box-config
# approach (camboxes have no keyboard/mouse; the preview monitor moves between cameras during an
# event, so a static per-box table can never track it) -- the HDMI cameraman preview is now
# UNCONDITIONAL and fleet-wide, baked directly into the binary's default (`DEFAULT_DISPLAY_SOURCE`
# in src/main.rs). Neither STEP 6 (config.toml) nor STEP 7 (ExecStart) needs to wire anything for
# it any more; both pure functions are gone.

# cleanup_bak_cruft DIR PATTERN... -- removes any files directly under DIR matching the given
# glob PATTERN(s) (#453: fleet self-heal for inert `.bak` leftovers -- a manual NDI upgrade left
# stale `/usr/lib/ndi/libndi.so.6*.bak` files on cam1/cam2/cam4, and a stale drop-in edit left
# `camera-box.service.d/genlock.conf.bak-30` on cam1; neither is loaded by anything -- ldconfig
# never resolves a `.bak` suffix, systemd only reads `*.conf` -- but a fresh/re-provisioned box
# should not carry the cruft forward forever). Idempotent (a no-op when nothing matches); exact
# glob-scoped to DIR + the given PATTERN(s) ONLY -- never a broad/recursive rm. Echoes one line
# per file actually removed so a provisioning run shows what it cleaned.
cleanup_bak_cruft() {
    local dir="${1:?cleanup_bak_cruft: directory required}"
    shift
    local pattern f
    for pattern in "$@"; do
        for f in "$dir"/$pattern; do
            [ -e "$f" ] || continue
            # Only ever remove regular files / symlinks. A stray `.bak`-named DIRECTORY would make
            # `rm -f` exit 1 and abort the whole provisioner under `set -e` -- skip it instead.
            [ -f "$f" ] || [ -L "$f" ] || continue
            rm -f -- "$f"
            echo "  Removed stale cruft: $f"
        done
    done
}

# cam2_is_painter_box DEVICE_NAME -> 0 iff DEVICE_NAME (already uppercased by resolve_device_name)
# is "CAM2", the ONE fixed painter box. #863: cam2 is permanently excluded from
# camera_strih_route() (scripts/camera-set.sh / recording-e2e.sh) -- its monitor is a diagnostic
# screen, never a camera-operator return preview. This is a FIXED-ROLE gate (like cam1 being the
# default source camera), NOT a re-introduction of the per-box preview-routing table #528
# rejected -- it decides nothing about WHICH camera's feed shows anywhere, only whether THIS one
# architecturally-fixed box gets the permanent devel-mode painter installed.
cam2_is_painter_box() {
    [ "${1:-}" = "CAM2" ]
}

# cam2_painter_no_display_dropin_content -> the systemd drop-in text that makes camera-box on
# cam2 PERMANENTLY skip its own --display thread (#863). cam2-painter.service (below) is the SOLE
# owner of cam2's /dev/fb0 / DRM master -- if camera-box's own unconditional HDMI preview (#528)
# also grabbed it, the two would race the same physical output with undefined visual result, the
# same conflict rig-mode.sh's TRANSIENT #291 no-display drop-in avoids during a measurement
# window. Here it's PERMANENT because cam2's painter role is architecturally fixed, not a
# per-run state.
cam2_painter_no_display_dropin_content() {
    cat <<'EOF'
[Service]
# #863: cam2 is the fixed painter box -- camera-box's own display thread must never grab
# /dev/fb0 (or DRM master) here; cam2-painter.service is always the sole owner of this
# box's monitor.
Environment=CAMERA_BOX_NO_DISPLAY=1
EOF
}

# cam2_painter_service_unit_content -> the systemd unit text for the PERMANENT devel-mode dual-QR
# painter (#863 -- "V devel režime má na cam2 monitore trvale bežať QR"). Mirrors the exact
# pinned flags rig-mode.sh's TEST-mode painter uses (qr-size 700, paint-fps 60, --dual-qr) --
# colour-scale/motion-sweep AND (#984) the QPSK audio marker all default ON under --paint-only
# (src/audio_marker_policy.rs / src/bin/frame-probe.rs), so nothing else needs to be passed for
# this unit to be audible. #984: a permanently-missing --audio-marker flag here is exactly the
# bug that left this unit painting QR forever with zero sound -- the marker's ALSA device is now
# resolved LIVE (never a hardcoded pin) and a failure to open it degrades (loud ERROR, keeps
# painting) instead of crashing the unit. duration-secs is a large-but-finite bound (~1 year)
# because frame-probe has no "run forever" mode; Restart=always self-heals both that eventual
# natural exit and any crash, so the monitor practically never goes dark on its own.
# #1008/#937: --marker-log /run/rig-qpsk-markers.csv -- TEST mode now hands STEADY STATE to this
# permanent unit (rig-mode.sh test, via cam2_painter_steady_state_handoff_cmds) instead of a
# disposable 2h nohup, so the unit itself must write the growing QPSK marker CSV the offline
# verdict pairs audio->frame from AND the "must-stay-alive" liveness check reads. Promotes the
# live 2026-08-06 10-marker-log.conf drop-in into the base unit (single source of truth).
cam2_painter_service_unit_content() {
    cat <<'EOF'
[Unit]
Description=cam2 permanent dual-QR devel-mode painter (#863)
Documentation=https://github.com/zbynekdrlik/camera-box
After=camera-box.service
Wants=camera-box.service

[Service]
Type=simple
ExecStart=/usr/local/bin/frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60 --duration-secs 31536000 --marker-log /run/rig-qpsk-markers.csv
Restart=always
RestartSec=2

StandardOutput=journal
StandardError=journal
SyslogIdentifier=cam2-painter

[Install]
WantedBy=multi-user.target
EOF
}

# root_mount_is_readonly OPTS -> 0 iff the FIRST comma-token of a mount-options string is exactly
# "ro" (the kernel always emits ro/rw first). Substring-safe: a rw mount carrying
# "errors=remount-ro" is correctly NOT read as read-only. Mirrors verify-device.sh's function of
# the same name/contract (#547) -- kept as a local copy (not a shared-lib extraction) to keep
# #599's fix scoped to this file.
root_mount_is_readonly() {
    case "$1" in
        ro | ro,*) return 0 ;;
        *) return 1 ;;
    esac
}

# ROOT_WAS_RO -- set by ensure_root_writable(), read by restore_root_mode() (#599). Tracks whether
# THIS run found root already read-only (an in-place re-provisioning pass against an
# already-booted appliance) so restore_root_mode() only remounts back to ro if THIS run is the one
# that changed it. A first-provisioning run (root naturally rw -- STEP 18 below only WRITES the ro
# fstab; that only takes effect on the NEXT reboot) must never be force-remounted ro early.
ROOT_WAS_RO=false

# ensure_root_writable -- #599: STEP 15-18 below run apt-get/dpkg/systemctl and write files under
# /etc, all of which require a writable root. On a FIRST provisioning run root is naturally rw, but
# on an IN-PLACE RE-RUN against an already-booted ro appliance (the box's own "self-heal on the
# next provisioning pass"), root is `ro` -- every apt-get/dpkg call in STEP 15-17 then fails and is
# swallowed by the `|| true`/`2>/dev/null` guards, silently leaving a purge/install that never took
# effect while the script still reports success. Detect ro root up front and remount rw BEFORE any
# of those steps run. Stop+mask PackageKit and unattended-upgrades first (rig-timesync-single-
# authority incident: PackageKit is D-Bus-activated by apt and holds an open write handle on
# /var/lib/PackageKit/transactions.db, which later blocks `mount -o remount,ro /` with EBUSY) so
# neither can reactivate mid-run. FAIL LOUD if the remount itself doesn't succeed -- never silently
# proceed on a still-ro root and claim success afterward.
#
# If a fail() call inside STEP 15-17 aborts the script while root is rw (e.g. the STEP 17
# dantesync download failing), restore_root_mode() never runs and the live mount stays rw until
# the next reboot -- bounded/self-healing (the ro fstab from the PRIOR successful pass still pins
# ro on reboot), not the claims-success-while-wrong failure #599 targets, so no explicit trap is
# added here.
ensure_root_writable() {
    local opts
    # `findmnt` failing outright (missing binary, unreadable /proc) must not silently read as "not
    # ro" -- fall back to /proc/mounts directly, mirroring verify-device.sh's identical fallback
    # for the same read, so a transient findmnt failure on a genuinely-ro box can't reproduce #599
    # by skipping the remount.
    opts="$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null)"
    if root_mount_is_readonly "$opts"; then
        ROOT_WAS_RO=true
        echo "  Root is read-only (re-provisioning an already-booted appliance) -- remounting rw"
        systemctl stop packagekit unattended-upgrades 2>/dev/null || true
        systemctl mask packagekit unattended-upgrades 2>/dev/null || true
        mount -o remount,rw / \
            || fail "root is read-only ('$opts') and 'mount -o remount,rw /' failed -- cannot safely apply package/config changes on a re-run (#599)"
    fi
}

# restore_root_mode -- #599 counterpart to ensure_root_writable(): if THIS run remounted rw, put
# root back to ro once every write in STEP 15-18 is done, instead of leaving a re-provisioned box
# unexpectedly rw until its next reboot. Stops PackageKit + unattended-upgrades again first (an
# apt-get call in STEP 15-17 may have D-Bus-reactivated PackageKit despite the earlier mask), then
# FAILS LOUD if the remount back to ro does not stick -- a script that silently leaves the box rw
# after printing "Setup Complete!" is exactly the false-success failure #599 exists to close.
restore_root_mode() {
    if [ "$ROOT_WAS_RO" = true ]; then
        systemctl stop packagekit unattended-upgrades 2>/dev/null || true
        mount -o remount,ro / \
            || fail "could not remount root back to read-only after applying package/config changes (#599) -- box would be left unexpectedly rw"
        echo "  Root remounted back to read-only"
    fi
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the destructive
# provisioning flow below. Same convention as scripts/setup-imag.sh / scripts/genlock-manifest.sh.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0
fi

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    fail "please run as root"
fi

# Parse arguments. --binary <url|path> may appear anywhere; DEVICE_NAME is the sole positional
# argument (#450 -- name-resolved single-arg invocation; IP/stream/genlock-fps are all DERIVED
# from it via camera-set.sh, replacing the old free-text 3-positional-arg form).
BINARY_ARG=""
POSITIONAL=()
while [ $# -gt 0 ]; do
    case "$1" in
        --binary)
            BINARY_ARG="${2:?--binary needs a URL or local path}"
            shift 2
            ;;
        *)
            POSITIONAL+=("$1")
            shift
            ;;
    esac
done
set -- "${POSITIONAL[@]}"

DEVICE_NAME_ARG="${1:-}"

if [ -z "$DEVICE_NAME_ARG" ]; then
    echo -e "${RED}Usage: $0 [--binary <url|path>] DEVICE_NAME${NC}"
    echo ""
    echo "DEVICE_NAME is resolved via scripts/camera-set.sh (cam1-6) -- case-insensitive."
    echo ""
    echo "Examples:"
    echo "  $0 CAM5"
    echo "  $0 --binary ./dist/camera-box CAM2"
    exit 1
fi

resolve_device_name "$DEVICE_NAME_ARG"

TOTAL_STEPS=19

# GitHub repos
DANTESYNC_REPO="zbynekdrlik/dantesync"

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Camera-Box Device Setup${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "Device Name:  ${YELLOW}${DEVICE_NAME}${NC}"
echo -e "Device IP:    ${YELLOW}${DEVICE_IP}${NC}"
echo -e "VBAN Stream:  ${YELLOW}${VBAN_STREAM}${NC}"
echo ""

# Confirm
read -p "Continue with setup? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 1
fi

# =============================================================================
# Pre-flight: ensure curl + CA certificates BEFORE first use
# =============================================================================
# STEP 3 (binary) and STEP 17 (dantesync) download via curl, but the minimal
# create-usb base image ships WITHOUT curl — and STEP 16 (which installs packages)
# runs AFTER STEP 3. On cam5 this made the binary download silently fail. Install
# curl up-front, fail loud if it can't be obtained, and stay idempotent (skip when
# already present).
echo ""
echo -e "${GREEN}[pre-flight] Ensuring curl + CA certificates...${NC}"
if command -v curl >/dev/null 2>&1; then
    echo "  curl already present"
else
    echo "  curl missing — installing (base image ships without it)"
    apt-get update -qq
    apt-get install -y -qq curl ca-certificates
    command -v curl >/dev/null 2>&1 || {
        echo -e "  ${RED}Error: curl still missing after install — cannot download binary/dantesync${NC}"
        exit 1
    }
    echo "  curl installed"
fi

# =============================================================================
# STEP 1: Set hostname
# =============================================================================
echo ""
echo -e "${GREEN}[1/${TOTAL_STEPS}] Setting hostname...${NC}"
echo "$DEVICE_NAME" > /etc/hostname
hostnamectl set-hostname "$DEVICE_NAME"
sed -i "s/127.0.1.1.*/127.0.1.1\t$DEVICE_NAME/" /etc/hosts
echo "  Hostname set to: $DEVICE_NAME"

# =============================================================================
# STEP 2: Configure static IP
# =============================================================================
echo ""
echo -e "${GREEN}[2/${TOTAL_STEPS}] Configuring static IP...${NC}"
# #1155: pin the LAN stanza to the PCI NIC by NAME (enp*), never the driver wildcard. A
# driver-wildcard match also claims a USB CDC-NCM camera link (bkshading, issue 808) and hands
# it this box's static IP + a duplicate default route -- which lands the dantesync PTP multicast
# join on the camera link and makes the box PTP-deaf (cam1 live incident 2026-08-20).
cat > /etc/netplan/01-netcfg.yaml << EOF
network:
  version: 2
  renderer: networkd
  ethernets:
    all-ethernet:
      match:
        name: "enp*"
      addresses:
        - ${DEVICE_IP}/23
      routes:
        - to: default
          via: 10.77.8.1
      nameservers:
        addresses:
          - 10.77.8.1
EOF
chmod 600 /etc/netplan/01-netcfg.yaml
rm -f /etc/netplan/50-cloud-init.yaml
echo "  Static IP configured: $DEVICE_IP"

# =============================================================================
# STEP 3: Install camera-box binary
# =============================================================================
echo ""
echo -e "${GREEN}[3/${TOTAL_STEPS}] Installing camera-box binary...${NC}"
# The fleet runs CI dev-builds (e.g. 1.7.0-dev.157), never a GitHub release -- installing from
# releases/latest silently version-drifted a fresh box from the fleet (cam6, #457). Resolution
# order:
#   1. --binary <local path>                  - use the file directly (already fetched elsewhere)
#   2. --binary <url> / CAMERA_BOX_BINARY_URL  - curl this exact URL (a raw camera-box binary)
#   3. default                                 - gh run download the latest successful CI
#      artifact on $CI_BRANCH, mirroring scripts/deploy-fleet.sh's own mechanism, so a fresh box
#      matches the fleet with no manual copy.
BINARY_SRC="${BINARY_ARG:-${CAMERA_BOX_BINARY_URL:-}}"
if [ -n "$BINARY_SRC" ] && [ -f "$BINARY_SRC" ]; then
    echo "  Using local binary: $BINARY_SRC"
    install -m 0755 "$BINARY_SRC" /usr/local/bin/camera-box
    echo "  Binary installed: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
elif [ -n "$BINARY_SRC" ]; then
    echo "  Downloading binary from: $BINARY_SRC"
    curl -fsSL "$BINARY_SRC" -o /usr/local/bin/camera-box \
        || fail "could not download binary from $BINARY_SRC"
    chmod +x /usr/local/bin/camera-box
    echo "  Binary installed: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
elif command -v gh >/dev/null 2>&1 && [ -n "${GH_TOKEN:-}" ]; then
    echo "  Fetching latest CI dev-build artifact (branch: $CI_BRANCH)..."
    RUN_ID="$(gh run list --repo "$GITHUB_REPO" --branch "$CI_BRANCH" --workflow ci.yml \
        --status success --limit 1 --json databaseId -q '.[0].databaseId // empty' 2>/dev/null || true)"
    [ -n "$RUN_ID" ] || fail "no successful CI run found on branch '$CI_BRANCH' -- install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
    DIST_DIR="$(mktemp -d)"
    if gh run download "$RUN_ID" --repo "$GITHUB_REPO" -n camera-box-linux-amd64 --dir "$DIST_DIR" 2>/dev/null \
        && [ -f "$DIST_DIR/camera-box" ]; then
        install -m 0755 "$DIST_DIR/camera-box" /usr/local/bin/camera-box
        echo "  Binary installed from CI run $RUN_ID: $(/usr/local/bin/camera-box --version 2>/dev/null || echo 'unknown version')"
    else
        rm -rf "$DIST_DIR"
        fail "gh run download failed for run $RUN_ID -- install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
    fi
    rm -rf "$DIST_DIR"
else
    fail "gh CLI unavailable or GH_TOKEN unset -- cannot auto-fetch the fleet dev-build. Install manually, or re-run with --binary <url|path> / CAMERA_BOX_BINARY_URL"
fi

# =============================================================================
# STEP 3b: cam2 ONLY -- install the permanent devel-mode dual-QR painter (#863)
# =============================================================================
# #863: "V devel režime má na cam2 monitore trvale bežať QR" -- cam2_painter_service_stop_cmds/
# start_cmds in scripts/rig-mode.sh (#440) and recording-e2e.sh's cleanup() `systemctl start
# cam2-painter` have ALWAYS assumed this unit exists; it was simply never installed anywhere --
# this STEP is the missing provisioning path (#863's root cause). Enable-only, like camera-box's
# own STEP 7 below: this script never starts services live, everything here takes effect on the
# next reboot.
if cam2_is_painter_box "$DEVICE_NAME"; then
    echo ""
    echo -e "${GREEN}[3b] Installing cam2 permanent dual-QR devel-mode painter (#863)...${NC}"
    # Same resolution shape as STEP 3's camera-box binary fetch (local path / URL override /
    # default CI artifact download), scoped to the probe-tools-linux-amd64 artifact's frame-probe
    # binary.
    FRAME_PROBE_SRC="${FRAME_PROBE_BINARY_URL:-}"
    if [ -n "$FRAME_PROBE_SRC" ] && [ -f "$FRAME_PROBE_SRC" ]; then
        echo "  Using local frame-probe: $FRAME_PROBE_SRC"
        install -m 0755 "$FRAME_PROBE_SRC" /usr/local/bin/frame-probe
    elif [ -n "$FRAME_PROBE_SRC" ]; then
        echo "  Downloading frame-probe from: $FRAME_PROBE_SRC"
        curl -fsSL "$FRAME_PROBE_SRC" -o /usr/local/bin/frame-probe \
            || fail "could not download frame-probe from $FRAME_PROBE_SRC"
        chmod +x /usr/local/bin/frame-probe
    elif command -v gh >/dev/null 2>&1 && [ -n "${GH_TOKEN:-}" ]; then
        echo "  Fetching probe-tools-linux-amd64 CI artifact (branch: $CI_BRANCH)..."
        PROBE_RUN_ID="$(gh run list --repo "$GITHUB_REPO" --branch "$CI_BRANCH" --workflow ci.yml \
            --status success --limit 1 --json databaseId -q '.[0].databaseId // empty' 2>/dev/null || true)"
        [ -n "$PROBE_RUN_ID" ] || fail "no successful CI run found on branch '$CI_BRANCH' -- cannot fetch frame-probe (#863). Install manually to /usr/local/bin/frame-probe, or re-run with FRAME_PROBE_BINARY_URL=<url|path>."
        PROBE_DIST_DIR="$(mktemp -d)"
        if gh run download "$PROBE_RUN_ID" --repo "$GITHUB_REPO" -n probe-tools-linux-amd64 --dir "$PROBE_DIST_DIR" 2>/dev/null \
            && [ -f "$PROBE_DIST_DIR/frame-probe" ]; then
            install -m 0755 "$PROBE_DIST_DIR/frame-probe" /usr/local/bin/frame-probe
            echo "  frame-probe installed from CI run $PROBE_RUN_ID"
        else
            rm -rf "$PROBE_DIST_DIR"
            fail "gh run download failed for run $PROBE_RUN_ID -- could not fetch frame-probe (probe-tools-linux-amd64 artifact, #863)"
        fi
        rm -rf "$PROBE_DIST_DIR"
    else
        fail "gh CLI unavailable or GH_TOKEN unset -- cannot auto-fetch frame-probe (probe-tools-linux-amd64 CI artifact, #863). Install manually, or re-run with FRAME_PROBE_BINARY_URL=<url|path>."
    fi

    # #863: cam2's OWN camera-box must never contest /dev/fb0 -- see the cam2_painter_no_display_
    # dropin_content() header comment above for the full story.
    mkdir -p /etc/systemd/system/camera-box.service.d
    cam2_painter_no_display_dropin_content > /etc/systemd/system/camera-box.service.d/cam2-no-display.conf
    cam2_painter_service_unit_content > /etc/systemd/system/cam2-painter.service
    systemctl daemon-reload
    systemctl enable cam2-painter.service
    echo "  cam2-painter.service installed + enabled (permanent dual-QR, qr-size 700, 60fps -- takes effect after reboot, #863)"
    echo "  camera-box.service.d/cam2-no-display.conf installed -- camera-box will never grab /dev/fb0 on cam2 (after reboot)"
fi

# =============================================================================
# STEP 4: Install NDI library
# =============================================================================
echo ""
echo -e "${GREEN}[4/${TOTAL_STEPS}] Setting up NDI library...${NC}"
mkdir -p /usr/lib/ndi
cleanup_bak_cruft /usr/lib/ndi 'libndi.so.6*.bak'   # #453: drop any stale manual-upgrade backup
# Add NDI library path to ldconfig
echo '/usr/lib/ndi' > /etc/ld.so.conf.d/ndi.conf
if [ -f /usr/lib/ndi/libndi.so.6 ]; then
    ldconfig
    echo "  NDI library: present and configured"
elif [ "$DEVICE_IP" = "$NDI_PEER" ]; then
    echo -e "  ${YELLOW}This box IS the fleet NDI source ($NDI_PEER) -- nothing to fetch${NC}"
    echo "  Copy libndi.so.* onto it manually before re-running this script"
else
    # NDI is a licensed runtime -- CI never builds it, so fetch it from a known-good fleet peer
    # (cam1) instead of requiring a manual per-box scp copy (#457). Mirrors the already-proven
    # setup-imag.sh step-10 dance: scp the versioned .so, then symlink libndi.so.6/libndi.so onto it.
    echo "  NDI library not found locally -- fetching from fleet peer $NDI_PEER..."
    command -v sshpass >/dev/null 2>&1 || apt-get install -y -qq sshpass >/dev/null 2>&1 || true
    if command -v sshpass >/dev/null 2>&1 \
        && sshpass -p "$NDI_PEER_PW" scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
            "root@${NDI_PEER}:/usr/lib/ndi/libndi.so.*.*.*" /usr/lib/ndi/ 2>/dev/null; then
        REAL="$(cd /usr/lib/ndi && ls libndi.so.*.*.* 2>/dev/null | head -1 || true)"
        [ -n "$REAL" ] || fail "NDI fetch from $NDI_PEER produced no file -- copy manually: scp root@${NDI_PEER}:/usr/lib/ndi/libndi.so.* /usr/lib/ndi/"
        ln -sf "$REAL" /usr/lib/ndi/libndi.so.6
        ln -sf libndi.so.6 /usr/lib/ndi/libndi.so
        ldconfig
        echo "  NDI library fetched from $NDI_PEER and configured ($REAL)"
    else
        fail "could not fetch NDI library from fleet peer $NDI_PEER -- copy manually: scp root@${NDI_PEER}:/usr/lib/ndi/libndi.so.* /usr/lib/ndi/"
    fi
fi

# =============================================================================
# STEP 5: Configure ALSA for USB headset
# =============================================================================
echo ""
echo -e "${GREEN}[5/${TOTAL_STEPS}] Configuring ALSA audio...${NC}"

# #782: write the canonical by-NAME /etc/asound.conf -- reference the CSCTEK "HID" card by NAME
# (`sysdefault:CARD=HID`), never the enumeration-time card NUMBER. The old hw:<card-number>,0 form
# baked whatever number the USB headset happened to enumerate as at provisioning time, which
# DANGLES the moment the box re-enumerates the headset onto a different card (cam7 live proof:
# provisioned as card 2, today card 1 -> a dead default; the #728 dangling-card class). The lib's
# `interkom_asound_conf_content` is the single source of truth, byte-identical to the hand-unified
# live fleet (sha256 d5db405c...). Confirm the HID card exists by NAME first (fail loud, #450
# posture) -- the config itself needs no number. `grep -q` in an `if !` is fully handled by the if.
if ! grep -qE '\[HID[[:space:]]*\]' /proc/asound/cards 2>/dev/null; then
    fail "no ALSA card named 'HID' on /proc/asound/cards -- the CSCTEK USB Audio+HID intercom headset is not enumerated (refusing to write a dangling asound.conf, #782/#450)"
fi
interkom_asound_conf_content > /etc/asound.conf
echo "  ALSA config: /etc/asound.conf (by NAME -- sysdefault:CARD=HID, enumeration-proof, #782)"

# =============================================================================
# STEP 6: Create camera-box config
# =============================================================================
echo ""
echo -e "${GREEN}[6/${TOTAL_STEPS}] Creating camera-box config...${NC}"
mkdir -p /etc/camera-box
cat > /etc/camera-box/config.toml << EOF
# Camera-Box Configuration
# Device: ${DEVICE_NAME}
# Generated: $(date -Iseconds)

# NDI source name (appears on network)
ndi_name = "usb"

# Video capture device ("auto" for auto-detection)
device = "auto"

# VBAN Intercom Configuration
[intercom]
stream = "${VBAN_STREAM}"
target = "strih.lan"
sample_rate = 48000
channels = 1
EOF
echo "  Config: /etc/camera-box/config.toml"
# #528: the HDMI cameraman preview is UNCONDITIONAL (baked into the binary's
# DEFAULT_DISPLAY_SOURCE) -- no [display] section is written here any more.

# =============================================================================
# STEP 7: Create systemd service
# =============================================================================
echo ""
echo -e "${GREEN}[7/${TOTAL_STEPS}] Creating systemd service...${NC}"
# #528: ExecStart is the canonical PLAIN line on every box, unconditionally -- no per-cam
# --display flag is ever baked in any more (the #556/#562 per-box table this used to read from is
# gone; the preview is a fleet-wide default in the binary itself, see camera-set.sh's comment).
cat > /etc/systemd/system/camera-box.service << 'EOF'
[Unit]
Description=Camera Box - USB Video Capture to NDI
Documentation=https://github.com/zbynekdrlik/camera-box
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/camera-box
Restart=always
RestartSec=3

# Run with real-time priority for low latency
Nice=-10
CPUSchedulingPolicy=fifo
CPUSchedulingPriority=50

# Environment for NDI SDK
Environment=NDI_RUNTIME_DIR_V6=/usr/lib/ndi

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=camera-box

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadOnlyPaths=/
ReadWritePaths=/dev /sys /run

# Allow access to video devices
SupplementaryGroups=video

[Install]
WantedBy=multi-user.target
EOF
# #528: canonical ExecStart, unconditional HDMI preview via the binary's own default -- nothing
# else to report here (see the "Service created and enabled" echo below, after daemon-reload).

# #289 + #11 systemd drop-ins: the realtime CPU-isolation + genlock emit-rate
# overrides live in drop-ins (not the base unit) so they can be re-applied / tuned
# without rewriting the whole unit. Today NO script created these — every box has
# been a manual SSH edit that drifted (30<->60 across boxes; a reinstall came up
# free-running/uncapped and with the grab NOT pinned to the isolated core). Writing
# them here makes a fresh box match the fleet in one run. Idempotent: re-running
# overwrites with identical content, and the daemon-reload below picks them up.
mkdir -p /etc/systemd/system/camera-box.service.d
cleanup_bak_cruft /etc/systemd/system/camera-box.service.d '*.bak' '*.bak-*'   # #453: drop stale
                                                                                # drop-in leftovers
# #289 — pin the SCHED_FIFO grab onto the isolcpus=3 reserved core. Soft (inherited)
# CPUAffinity mask, NOT a hard cpuset: the per-thread sched_setaffinity calls in the
# binary still move painter / --display / intercom threads OFF onto the general cores
# and re-pin capture+emit to the /sys-derived isolated core (src/affinity.rs, the
# authoritative path). This static value just matches the fleet's isolcpus=3.
cat > /etc/systemd/system/camera-box.service.d/cpu-affinity.conf << 'EOF'
[Service]
# #289: pin grab to the isolated core (isolcpus=3) so box load never starves capture/emit
CPUAffinity=3
EOF
# #11/#450 — NDI emit rate, from the per-cam CAMERA_GENLOCK_FPS table (scripts/camera-set.sh,
# #451) rather than a hardcoded literal. Every fleet camera emits 60fps today (the stream box
# decimates 60->30 downstream), so this resolves to the same value as before -- but a future
# per-camera divergence now needs only a camera-set.sh edit, not a setup-device.sh edit too.
cat > /etc/systemd/system/camera-box.service.d/genlock.conf << EOF
[Service]
Environment=CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}
EOF
# #772 -- ExecStartPre device-free bake-in: a small helper + drop-in so EVERY camera-box start
# (the on-box dead-man, cleanup(), the next-run preflight, or a manual operator restart) first
# frees /dev/video from a killed E2E run's stray capture burn, instead of crash-looping on
# "Device or resource busy". Single-sourced in scripts/lib/camera-box-free-device.sh, verified by
# verify-device.sh's (y) check. Enable-only convention: files written now, take effect next start.
camera_box_free_capture_device_script_content > /usr/local/bin/camera-box-free-capture-device.sh
chmod +x /usr/local/bin/camera-box-free-capture-device.sh
camera_box_free_capture_device_dropin_content > /etc/systemd/system/camera-box.service.d/free-capture-device.conf
echo "  camera-box.service.d/free-capture-device.conf installed -- frees /dev/video on every start (#772)"
# issue 792 / #1087 — the secondary 30fps NDI blend stream ("CAMn (30p)", a 2-frame 60->30
# temporal blend) is enabled by this env drop-in; the binary defaults the feature OFF. Every active
# fleet box already runs it (hand-installed until now), so writing it here makes a re-provisioned
# box keep the (30p) stream instead of silently regressing to 60p-only. Same enable-only convention
# as the drop-ins above — effective on the box's next reboot. The heredoc below reproduces the live
# fleet file byte-for-byte; verify-device.sh's (z) check then proves the drop-in AND the live (30p)
# stream post-reboot.
cat > /etc/systemd/system/camera-box.service.d/publish-30p.conf << 'EOF'
[Service]
Environment=CAMERA_BOX_PUBLISH_30P=1
EOF

systemctl daemon-reload
systemctl enable camera-box
echo "  Service created and enabled"
echo "  Drop-ins: cpu-affinity.conf (CPUAffinity=3, isolcpus core) + genlock.conf (CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}) + publish-30p.conf (CAMERA_BOX_PUBLISH_30P=1)"

# =============================================================================
# STEP 8: Configure auto-login on tty1
# =============================================================================
echo ""
echo -e "${GREEN}[8/${TOTAL_STEPS}] Configuring auto-login...${NC}"
mkdir -p /etc/systemd/system/getty@tty1.service.d
cat > /etc/systemd/system/getty@tty1.service.d/autologin.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear %I $TERM
EOF
systemctl daemon-reload
echo "  Auto-login: root on tty1"

# =============================================================================
# STEP 9: Set binary capabilities
# =============================================================================
echo ""
echo -e "${GREEN}[9/${TOTAL_STEPS}] Setting binary capabilities...${NC}"
if [ -f /usr/local/bin/camera-box ]; then
    setcap 'cap_sys_nice,cap_ipc_lock+ep' /usr/local/bin/camera-box
    echo "  Capabilities set (real-time priority, memory lock)"
else
    echo -e "  ${YELLOW}Skipped - binary not installed${NC}"
fi

# =============================================================================
# STEP 10: GRUB — fast boot + #295 brick-proofing (pin a known-good kernel)
# =============================================================================
echo ""
echo -e "${GREEN}[10/${TOTAL_STEPS}] Configuring GRUB (fast + safe boot)...${NC}"
sed -i 's/GRUB_TIMEOUT=.*/GRUB_TIMEOUT=0/' /etc/default/grub
sed -i 's/GRUB_TIMEOUT_STYLE=.*/GRUB_TIMEOUT_STYLE=hidden/' /etc/default/grub
grep -q "GRUB_TIMEOUT_STYLE" /etc/default/grub || echo "GRUB_TIMEOUT_STYLE=hidden" >> /etc/default/grub
grep -q "GRUB_RECORDFAIL_TIMEOUT" /etc/default/grub || echo "GRUB_RECORDFAIL_TIMEOUT=0" >> /etc/default/grub
# #295: pin the default to the explicitly-saved known-good kernel, never "newest".
if grep -q '^GRUB_DEFAULT=' /etc/default/grub; then
    sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' /etc/default/grub
else
    echo 'GRUB_DEFAULT=saved' >> /etc/default/grub
fi
# #289/#303: reserve core 3 for the SCHED_FIFO capture/emit path (isolcpus=3) and quiet it
# the rest of the way — nohz_full=3 stops the periodic scheduler tick on it, rcu_nocbs=3
# offloads RCU callbacks off it, and irqaffinity=0-2 defaults ALL boot IRQs onto the general
# cores (the only lever that moves managed MSI xhci IRQs, which reject runtime
# /proc/irq/<n>/smp_affinity writes — #289's ExecStartPre logs those as non-fatal and they
# stay put without this). Each flag is appended to GRUB_CMDLINE_LINUX IDEMPOTENTLY (never
# duplicated), one shared loop for all four so there is exactly one append code path. This
# edit lives INSIDE this #295 initrd-guaranteed grub step (the update-grub + abort-if-no-initrd
# guard below) so the cmdline change can never strand a box on an initrd-less kernel the way
# an ad-hoc grub edit did.
for flag_tag in "isolcpus=3:#289" "nohz_full=3:#303" "rcu_nocbs=3:#303" "irqaffinity=0-2:#303"; do
    flag="${flag_tag%%:*}"
    tag="${flag_tag##*:}"
    if ! grep -qE "^GRUB_CMDLINE_LINUX=.*${flag}([\" ]|\$)" /etc/default/grub; then
        if grep -q '^GRUB_CMDLINE_LINUX=' /etc/default/grub; then
            # Append inside the existing double-quoted value.
            sed -i "s/^\(GRUB_CMDLINE_LINUX=\"[^\"]*\)\"/\1 ${flag}\"/" /etc/default/grub
            # Normalise a leading space when GRUB_CMDLINE_LINUX was previously empty ("" -> " isolcpus=3").
            sed -i 's/^GRUB_CMDLINE_LINUX="  */GRUB_CMDLINE_LINUX="/' /etc/default/grub
        else
            echo "GRUB_CMDLINE_LINUX=\"${flag}\"" >> /etc/default/grub
        fi
        echo "  Kernel cmdline: ${flag} added to GRUB_CMDLINE_LINUX [${tag}]"
    else
        echo "  Kernel cmdline: ${flag} already present [${tag}]"
    fi
done
# #295: GUARANTEE every installed kernel has an initrd BEFORE regenerating grub. A kernel without an
# initrd that becomes the grub default cannot mount root — that bricked CAM3 + CAM4.
for vmlinuz in /boot/vmlinuz-*; do
    [ -e "$vmlinuz" ] || continue
    kver="${vmlinuz#/boot/vmlinuz-}"
    if [ ! -e "/boot/initrd.img-${kver}" ]; then
        echo -e "  ${YELLOW}#295: kernel ${kver} has no initrd — generating before grub${NC}"
        update-initramfs -c -k "${kver}"
    fi
done
update-grub
# #295: refuse to leave a default boot entry without a kernel image AND an initrd, then pin the
# saved default to the running (known-good) kernel.
GRUB_CFG="/boot/grub/grub.cfg"
if [ -f "$GRUB_CFG" ]; then
    DEFAULT_ENTRY="$(awk '/^[[:space:]]*menuentry /{c++} c==1{print} c==2{exit}' "$GRUB_CFG")"
    if ! echo "$DEFAULT_ENTRY" | grep -qE '(vmlinuz|[[:space:]]linux )' \
        || ! echo "$DEFAULT_ENTRY" | grep -q 'initrd'; then
        echo -e "${RED}#295: grub default entry lacks a kernel image or initrd — aborting to avoid a brick${NC}"
        exit 1
    fi
    # The default entry (index 0) is now proven to carry both a kernel image and an initrd. Pin it
    # explicitly as the saved default so it boots deterministically. The kernel is held (apt-mark
    # hold), so index 0 is the single known-good kernel on a freshly-provisioned appliance.
    grub-set-default 0
fi
echo "  GRUB: timeout 0s, default pinned to known-good kernel with initrd [#295]"

# =============================================================================
# STEP 11: Don't block boot on the network
# #547: MASK systemd-networkd-wait-online. Unmasked it timed out at 120s and delayed
# network-online.target -> camera-box started ~123s late (observed on cam3). The box has a static
# IP (STEP 2), and camera-box (After=network-online.target + its own retry) never needs to wait for
# the link to be "online", so masking is safe and cuts boot-to-stream to a few seconds.
# =============================================================================
echo ""
echo -e "${GREEN}[11/${TOTAL_STEPS}] Removing the network-wait boot stall...${NC}"
systemctl mask systemd-networkd-wait-online.service 2>/dev/null || true
# Keep a short-timeout override too (belt-and-braces): harmless while masked, and if the unit is
# ever unmasked it still caps the wait at 5s instead of the 120s default.
mkdir -p /etc/systemd/system/systemd-networkd-wait-online.service.d
cat > /etc/systemd/system/systemd-networkd-wait-online.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/lib/systemd/systemd-networkd-wait-online --timeout=5
EOF
echo "  Network wait: masked (no 120s boot stall) + 5s override fallback"

# =============================================================================
# STEP 12: Disable power button shutdown
# =============================================================================
echo ""
echo -e "${GREEN}[12/${TOTAL_STEPS}] Disabling power button shutdown...${NC}"
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/disable-power-button.conf << EOF
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
EOF
echo "  Power button: ignored (used for mute toggle)"

# =============================================================================
# STEP 13: Disable all power saving / sleep
# =============================================================================
echo ""
echo -e "${GREEN}[13/${TOTAL_STEPS}] Disabling power saving...${NC}"
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true
# Disable CPU frequency scaling (use performance governor)
for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo "performance" > "$cpu" 2>/dev/null || true
done
# Make it persistent
cat > /etc/systemd/system/cpu-performance.service << 'EOF'
[Unit]
Description=Set CPU to performance mode
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/bin/bash -c 'for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance > $cpu; done'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable cpu-performance.service 2>/dev/null || true
echo "  Sleep/suspend: disabled"
echo "  CPU governor: performance"

# =============================================================================
# STEP 14: Optimize network for performance
# =============================================================================
echo ""
echo -e "${GREEN}[14/${TOTAL_STEPS}] Optimizing network performance...${NC}"
cat > /etc/sysctl.d/99-network-performance.conf << 'EOF'
# Network performance optimizations for low-latency streaming

# Increase network buffer sizes
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 1048576
net.core.wmem_default = 1048576
net.core.netdev_max_backlog = 5000

# TCP optimizations
net.ipv4.tcp_rmem = 4096 1048576 134217728
net.ipv4.tcp_wmem = 4096 1048576 134217728
net.ipv4.tcp_congestion_control = bbr
net.ipv4.tcp_fastopen = 3

# Reduce latency
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_nodelay = 1

# Disable IPv6 if not needed
net.ipv6.conf.all.disable_ipv6 = 1
net.ipv6.conf.default.disable_ipv6 = 1
EOF
sysctl -p /etc/sysctl.d/99-network-performance.conf 2>/dev/null || true

# Disable EEE (Energy Efficient Ethernet / Green Ethernet) and flow control
# This reduces latency by preventing power-saving mode transitions
cat > /etc/networkd-dispatcher/routable.d/optimize-nic << 'NICEOF'
#!/bin/bash
# Disable EEE (Green Ethernet) and flow control for low latency
IFACE="$IFACE"
if [ -n "$IFACE" ] && [ "$IFACE" != "lo" ]; then
    # Disable Energy Efficient Ethernet
    ethtool --set-eee "$IFACE" eee off 2>/dev/null || true
    # Disable flow control (pause frames)
    ethtool -A "$IFACE" rx off tx off 2>/dev/null || true
fi
NICEOF
chmod +x /etc/networkd-dispatcher/routable.d/optimize-nic

# Apply to current interface
for iface in /sys/class/net/*/device; do
    IFACE=$(basename "$(dirname "$iface")")
    ethtool --set-eee "$IFACE" eee off 2>/dev/null || true
    ethtool -A "$IFACE" rx off tx off 2>/dev/null || true
done

echo "  Network buffers: optimized"
echo "  TCP congestion: BBR"
echo "  IPv6: disabled"
echo "  EEE (Green Ethernet): disabled"
echo "  Flow control: disabled"

# =============================================================================
# #599: ensure root is writable before STEP 15-18 apply package/config changes -- a no-op on a
# first-provisioning run (root already rw); remounts rw on an in-place re-run against an
# already-booted ro appliance. Paired with restore_root_mode() after STEP 18.
# =============================================================================
ensure_root_writable

# =============================================================================
# STEP 15: Disable unnecessary services
# =============================================================================
echo ""
echo -e "${GREEN}[15/${TOTAL_STEPS}] Disabling unnecessary services...${NC}"
# Snap
systemctl disable --now snapd.service snapd.socket snapd.seeded.service 2>/dev/null || true
systemctl mask snapd.service 2>/dev/null || true
# Cloud-init
systemctl disable --now cloud-init.service cloud-init-local.service cloud-config.service cloud-final.service 2>/dev/null || true
touch /etc/cloud/cloud-init.disabled 2>/dev/null || true
# Auto updates — #295: an active unattended-upgrades auto-installed a kernel without an initrd that
# bricked CAM3/CAM4. PIN the kernel and disable unattended upgrades entirely; an appliance must
# never silently gain a new kernel.
systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer \
    apt-daily.service apt-daily-upgrade.service 2>/dev/null || true
systemctl mask unattended-upgrades.service 2>/dev/null || true
apt-mark hold linux-image-generic linux-headers-generic linux-generic 2>/dev/null || true
cat > /etc/apt/apt.conf.d/20auto-upgrades << 'EOF'
// Camera-box appliance: never auto-update (#295 — kernels are pinned with apt-mark hold).
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
EOF
cat > /etc/apt/apt.conf.d/51camera-box-no-kernel-autoupgrade << 'EOF'
// #295: never let unattended-upgrades touch the kernel on the appliance.
Unattended-Upgrade::Package-Blacklist {
    "linux-image";
    "linux-headers";
    "linux-generic";
};
EOF
# #295: any FUTURE kernel install must always get an initrd. This /etc/kernel/postinst.d hook sorts
# before grub's own `zz-update-grub` hook, so a missing initrd is regenerated BEFORE grub is updated.
mkdir -p /etc/kernel/postinst.d
cat > /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee << 'EOF'
#!/bin/sh
# #295: guarantee every installed kernel has an initrd (a kernel without one bricked CAM3/CAM4).
# #547: on the read-only-root appliance, mkinitramfs' default build dir /var/tmp is a 50M tmpfs --
# far too small for a ~400M initramfs ("No space left on device" -> no initrd -> a half-installed,
# unbootable kernel). Build in /root/.itmp (the real ~51G disk) instead. A kernel install only ever
# runs inside a `mount -o remount,rw /` window, so /root (and /boot) are writable then.
set -e
version="$1"
[ -n "$version" ] || exit 0
if [ ! -e "/boot/initrd.img-${version}" ]; then
    mkdir -p /root/.itmp
    TMPDIR=/root/.itmp update-initramfs -c -k "${version}"
fi
EOF
chmod +x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee
echo "  #295: kernel pinned (apt-mark hold), unattended-upgrades disabled, initrd hook installed"
# fwupd — #547: PURGE it. On the read-only-root appliance fwupd holds an open write handle on
# /var/lib/fwupd/pending.db, which makes `mount -o remount,ro /` fail with EBUSY (blocked the ro
# conversion on cam1/cam4). The appliance never firmware-updates itself, so remove it outright.
systemctl disable --now fwupd.service fwupd-refresh.timer 2>/dev/null || true
apt-get purge -y fwupd fwupd-signed 2>/dev/null \
    || dpkg --purge --force-depends fwupd fwupd-signed 2>/dev/null \
    || systemctl mask fwupd.service 2>/dev/null || true
# ModemManager (not needed)
systemctl disable --now ModemManager.service 2>/dev/null || true
# Bluetooth (not needed)
systemctl disable --now bluetooth.service 2>/dev/null || true
# Printing (not needed)
systemctl disable --now cups.service cups-browsed.service 2>/dev/null || true
echo "  Disabled: snapd, cloud-init, auto-updates, fwupd (purged), ModemManager, bluetooth, cups"

# =============================================================================
# STEP 16: Install required packages
# =============================================================================
echo ""
echo -e "${GREEN}[16/${TOTAL_STEPS}] Installing required packages...${NC}"
apt-get update -qq
# #362: include the FULL NDI/audio runtime dep set so a fresh box can RUN camera-box (the CAM3
# clone crash-looped on missing libndi deps): libasound2t64 (ALSA, intercom), libavahi-common3
# (libndi links it alongside libavahi-client3), and avahi-utils (avahi-browse for diagnosis).
# #743: psmisc (provides `fuser`) joins the same dual-bake as create-usb-linux.sh -- a fresh
# cam2 clone (2026-07-13) had no `fuser`, false-FAILing rig-mode.sh's #464 KMS-held check AND
# silently no-op'ing recording-e2e.sh's capture-release busy-wait.
# dantesync issue 52: nftables provides `nft`, needed by STEP 17c to install the NTP-client
# DSCP OUTPUT-mangle rule (rsntp cannot setsockopt(IP_TOS) -- see scripts/lib/dscp-nft.sh).
apt-get install -y -qq avahi-daemon libavahi-client3 libavahi-common3 avahi-utils libasound2t64 v4l-utils alsa-utils ethtool curl ca-certificates psmisc nftables 2>/dev/null || true
systemctl enable avahi-daemon
echo "  Installed: avahi-daemon, libavahi-client3, libavahi-common3, avahi-utils, libasound2t64, v4l-utils, alsa-utils, ethtool, curl, ca-certificates, psmisc, nftables"

# #782: bake the per-box interkom mixer gains. This MUST run AFTER alsa-utils is installed (amixer/
# alsactl land above), and it belongs here in STEP 16 rather than STEP 5 for that reason. Previously
# the mixer gain was never set in provisioning at all -- a fresh box kept the CSCTEK headset's
# power-on default (Mic 91%/-3dB) while the hand-tuned older boxes ran quieter, so cam5-7 shipped
# ~5dB louder mics in the intercom. Per-box compensation table (owner 2026-07-15, analog headset
# differences): cam1-4 Mic 75%/PCM 79%, cam5-7 Mic 80%/PCM 94%. `alsactl store` persists it to
# /var/lib/alsa/asound.state so it survives the box's next reboot (verify-device.sh's (aa) check
# reads it back). Best-effort with a warning (never a hard fail): STEP 5 already asserted the HID
# card is present, so a failure here means a transient amixer/HID glitch, not a wrong box -- and
# verify-device.sh's post-reboot acceptance gate catches a silently-unset gain anyway.
MIC_PCT="$(interkom_mic_pct "$DEVICE_NAME")"
PCM_PCT="$(interkom_pcm_pct "$DEVICE_NAME")"
if amixer -c HID sset Mic "${MIC_PCT}%" >/dev/null 2>&1 &&
    amixer -c HID sset PCM "${PCM_PCT}%" >/dev/null 2>&1 &&
    alsactl store >/dev/null 2>&1; then
    echo "  Interkom mixer: Mic ${MIC_PCT}% / PCM ${PCM_PCT}% (per-box #782), persisted via alsactl store"
else
    warn "could not apply interkom mixer gains (Mic ${MIC_PCT}%/PCM ${PCM_PCT}%) -- amixer/alsactl or the HID card unavailable? verify-device.sh (aa) will catch a wrong/unset gain post-reboot (#782)"
fi

# #930: ffmpeg + EGL runtime for the lipsync cross-validation TEST-mode variant (any box may take
# cam2's painter role -- unified provisioning, see the "Unified cam-box provisioning" playbook
# entry) -- one ffmpeg process feeds BOTH /dev/fb0 (video) and the QPSK marker's ALSA device
# (audio) from a single demux/decode timeline (scripts/lipsync-test-mode.sh). --no-install-
# recommends keeps this to ~180MB instead of ~170MB of GTK/VA-API/pocketsphinx recommends
# pulled in by a plain `apt-get install ffmpeg` (live-verified on cam2, #930); libegl1/
# libegl-mesa0 are separate from libgl1-mesa-dri and were the actual missing piece behind an
# initial "EGL not initialized" failure trying an SDL2/KMSDRM alternative (kept here anyway --
# harmless, and useful if a future variant of this tool ever needs it).
# issue 1187: the lipsync-test-mode playback path moved OFF raw fbdev (ffmpeg -f fbdev, which left
# a stale frame in /dev/fb0 memory -- issue 1176) ONTO DRM/KMS via `mpv --vo=drm`. mpv is therefore
# the DRM/KMS lipsync playback runtime and is installed on the SAME fail-loud line as the #930
# packages (it Depends on the ffmpeg libav* codecs already present, so --no-install-recommends still
# decodes the H.264 test asset). ffmpeg/EGL stay installed -- harmless, and ffmpeg's libav* are
# mpv's own decode dependency anyway.
apt-get install -y -qq --no-install-recommends ffmpeg libsdl2-2.0-0 libegl1 libegl-mesa0 libgl1-mesa-dri mpv
echo "  Installed: ffmpeg, libsdl2-2.0-0, libegl1, libegl-mesa0, libgl1-mesa-dri, mpv (#930/#1187 lipsync-test-mode runtime)"

# Create rc.local for power management settings (USB autosuspend, etc.)
cat > /etc/rc.local << 'RCEOF'
#!/bin/bash
# Camera-box power settings

# CPU performance mode
for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -f "$gov" ] && echo "performance" > "$gov" 2>/dev/null
done

# USB autosuspend off
for ctrl in /sys/bus/usb/devices/*/power/control; do
    [ -f "$ctrl" ] && echo "on" > "$ctrl" 2>/dev/null
done

# Network power management off
for iface in /sys/class/net/*/device/power/control; do
    [ -f "$iface" ] && echo "on" > "$iface" 2>/dev/null
done

exit 0
RCEOF
chmod +x /etc/rc.local
echo "  Created: /etc/rc.local (USB autosuspend off, CPU performance)"

# #894: rc.local (above) only applies USB-autosuspend-off ONCE at boot -- a grabber that
# re-enumerates LATER comes back at the kernel default `auto` (measured fleet-wide: the box that
# stayed at `on` had zero re-enumerations that day; the two that drifted to `auto` had 5 and 1, an
# amplifying feedback loop). Install a udev rule that re-applies it on EVERY video4linux "add",
# scoped to the grabber that actually fired (never a blanket SUBSYSTEM=="usb" match) via the
# helper script below. The SAME rule also replaces the fleet's old UNCONDITIONAL
# "restart camera-box.service on hotplug" rule (traced to the retired scripts/setup.sh, #563 --
# it never migrated into this script) with a CONDITIONAL one: skip the restart while an E2E
# camera-box-burn-*.service owns the device, so a benign USB re-enumeration during a measurement
# run can no longer steal the capture node back from the burn unit (77/NOPERM, mislabeled as a
# frozen camera in the verdict).
mkdir -p /etc/udev/rules.d
udev_camera_box_rules_content > /etc/udev/rules.d/99-camera-box.rules
udev_camera_box_helper_script_content > /usr/local/bin/camera-box-udev-video-add.sh
chmod +x /usr/local/bin/camera-box-udev-video-add.sh
# Reload the rule DB now so a re-provisioning pass against an already-booted box picks it up
# immediately -- this is NOT starting/restarting camera-box.service itself (that stays deferred to
# the next reboot, per this script's own convention), just refreshing udev's own rule cache.
udevadm control --reload-rules 2>/dev/null || true
echo "  Installed: /etc/udev/rules.d/99-camera-box.rules + /usr/local/bin/camera-box-udev-video-add.sh (#894 -- conditional hotplug restart + autosuspend re-apply)"

# =============================================================================
# STEP 17: Install dantesync (PTP time synchronization) -- the SOLE clock authority
# =============================================================================
echo ""
echo -e "${GREEN}[17/${TOTAL_STEPS}] Installing dantesync...${NC}"

# #591: dantesync is the rig's SOLE clock authority (PTP/NTP). A minimalist cambox/imag appliance
# must run NO other timesync daemon -- cam5/cam6 (N150) shipped with systemd-timesyncd active
# ALONGSIDE dantesync, causing a real 5.28-second clock desync ([NTP] offset:-5280959us) that was
# invisible to weeks of "passing" verification. Masking is a band-aid; the package must be GONE, so
# PURGE every competing timesync daemon FIRST (then install dantesync below). systemd-timesyncd is
# the one the base Ubuntu image actually ships; chrony/ntp/ntpsec/openntpd are belt-and-suspenders
# (normally absent -> a no-op purge). Runs in the rw window (the ro conversion is STEP 18); a masked
# /dev/null symlink backstops each so a stray re-install can't silently re-activate it. The (r)
# check in verify-device.sh hard-fails a box on which ANY of these is still installed/active/enabled.
for _ts in systemd-timesyncd chrony ntp ntpsec openntpd; do
    systemctl disable --now "$_ts" 2>/dev/null || true
    apt-get purge -y "$_ts" 2>/dev/null \
        || dpkg --purge --force-depends "$_ts" 2>/dev/null || true
    systemctl mask "$_ts" 2>/dev/null || true
done
# #597: linuxptp (ptp4l/phc2sys) is a 2nd class of competing timesync authority -- a rogue PTP
# daemon would fight dantesync's OWN PTP servo directly on this PTP rig. Unlike the NTP daemons
# above, its dpkg PACKAGE ("linuxptp") differs from its systemd UNITS ("ptp4l"/"phc2sys"), so it
# needs its own stanza rather than fitting the shared `for _ts in ...` loop -- but the SAME
# disable -> purge -> mask order as that loop: disable+stop each unit, purge the ONE shared
# package, then mask each unit as a backstop. dantesync is a standalone binary
# (/usr/local/bin/dantesync, downloaded below) with no dependency on linuxptp -- safe to purge
# outright.
for _u in ptp4l phc2sys; do
    systemctl disable --now "$_u" 2>/dev/null || true
done
apt-get purge -y linuxptp 2>/dev/null \
    || dpkg --purge --force-depends linuxptp 2>/dev/null || true
for _u in ptp4l phc2sys; do
    systemctl mask "$_u" 2>/dev/null || true
done
echo "  #591/#597: purged + masked competing timesync daemons (dantesync is the sole clock authority)"

# #762: rsyslog is REDUNDANT on this appliance -- journald already captures everything (the
# ACTUAL log store any operator/harness reads, e.g. `journalctl -u camera-box`), and nothing
# reads /var/log/syslog on a read-only appliance with no operator logging in. A live incident
# (cam1, 2026-07-15) showed rsyslogd enter a write-error feedback loop once the 50MB /var/log
# tmpfs filled -- write fails, logs the failure, journald forwards it, write fails again -- at
# ~400 lines/s, burning 42.8% CPU and starving the camera-box send path badly enough to
# measurably drift NDI delivery timing. Same disable -> purge -> mask discipline as the
# #591/#597 stanzas above -- purge, not just mask (a masked-but-installed daemon can still be
# re-enabled). Cap journald's own RuntimeMaxUse so the journal itself can never grow to fill the
# SAME tmpfs rsyslog used to (log_diet_journald_dropin, scripts/lib/log-diet.sh -- single source
# of truth shared with verify-device.sh's (u) check and create-usb-linux.sh).
systemctl disable --now rsyslog 2>/dev/null || true
apt-get purge -y rsyslog 2>/dev/null \
    || dpkg --purge --force-depends rsyslog 2>/dev/null || true
systemctl mask rsyslog 2>/dev/null || true
mkdir -p "$(dirname "$LOG_DIET_JOURNALD_DROPIN_PATH")"
log_diet_journald_dropin > "$LOG_DIET_JOURNALD_DROPIN_PATH"
systemctl restart systemd-journald 2>/dev/null || true
echo "  #762: purged rsyslog + capped journald RuntimeMaxUse=${LOG_DIET_JOURNALD_RUNTIME_MAX} (log-storm CPU/disk fix)"

DANTESYNC_INSTALLED=false

# Get latest release URL from GitHub
DANTESYNC_URL=$(curl -fsSL "https://api.github.com/repos/${DANTESYNC_REPO}/releases/latest" 2>/dev/null | \
    grep -o '"browser_download_url": *"[^"]*dantesync-linux-amd64"' | \
    grep -o 'https://[^"]*' | head -1) || true

# #450: fail loud -- dantesync disciplines the cluster wall-clock genlock depends on (#8); a box
# provisioned without it silently free-runs its own clock instead of the fleet's shared reference.
[ -n "$DANTESYNC_URL" ] || fail "could not get dantesync release URL from GitHub -- dantesync is required for cluster clock sync (#8), not optional"

curl -fsSL "$DANTESYNC_URL" -o /usr/local/bin/dantesync 2>/dev/null \
    || fail "failed to download dantesync from $DANTESYNC_URL"
chmod +x /usr/local/bin/dantesync

# Create systemd service
cat > /etc/systemd/system/dantesync.service << 'DANTEEOF'
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
# Sync to the cluster master strih (10.77.9.202), NOT the dantesync binary's default public NTP
# pool (sk.pool.ntp.org / time.cloudflare.com) — every node must discipline its clock to the SAME
# reference or the wall-clock genlock in src/ndi.rs silently diverges across cameras (#8). Use the
# IP, not strih.lan: per CLAUDE.md/targets.md .lan DNS may not resolve on a freshly-provisioned
# read-only-rootfs camera, and a failed resolve makes dantesync fall back to its public-pool
# default and silently desync — the exact regression #8 guards against.
ExecStart=/usr/local/bin/dantesync --ntp-server 10.77.9.202
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
DANTEEOF

systemctl daemon-reload
systemctl enable dantesync 2>/dev/null || true
DANTESYNC_INSTALLED=true
echo "  dantesync: installed and enabled"

# =============================================================================
# STEP 17b: RemoteOS MCP control-channel agent (#1066) -- cam1-4 parity with the 858 imag fix
# =============================================================================
echo ""
echo -e "${GREEN}[17b] Provisioning RemoteOS MCP control-channel agent...${NC}"

# The linux-camN MCP surface (:8092) is served by the SEPARATE zbynekdrlik/remoteos-mcp project
# (ops skill #555). camera-box does NOT re-implement or re-pin the agent -- it INVOKES that
# project's own canonical install-linux.sh (pip-git install + config.json + systemd unit +
# enable/start), mirroring setup-imag.sh's own remoteos step and the standing "use the installer,
# never a bare pip command" discipline. The agent survived only as a hand-install on each live cam
# box before this step; a fresh reprovision / new box came up with the MCP surface DEAD.
#
# Runs HERE, after STEP 17 (dantesync) and BEFORE STEP 18's ro-root flip: the installer writes to
# /usr + /etc, which must happen while root is still rw. Per this script's enable-only convention
# (.claude/rules/provisioning-scripts.md), the gate is `systemctl is-enabled` (the reboot-survival
# property), NOT is-active -- the LIVE :8092 surface is proven post-reboot by verify-device.sh's
# (ab) acceptance check. curl and the CA store are ensured fail-loud by the pre-flight above (and
# STEP 16), so both are present by the time this step runs.
#
# Auth-key handling (security-boundary): the --auth-key is a full-shell-RCE bearer token bound to
# 0.0.0.0:8092, so it NEVER lands in this repo. Two paths, mirroring this script's env-secret
# convention (CAM_PW/GH_TOKEN):
#   - REMOTEOS_MCP_AUTH_KEY set -> pre-seed /etc/remoteos-mcp/config.json (chmod 600) so the
#     installer REUSES that known key and dev1's gitignored .mcp.json keeps matching a freshly
#     hardware'd box (fully closes #1066: a working MCP surface, not just an installed agent).
#   - unset -> the installer generates a fresh on-box key; update dev1's .mcp.json linux-camN entry.
REMOTEOS_MCP_INSTALLER_URL="${REMOTEOS_MCP_INSTALLER_URL:-https://raw.githubusercontent.com/zbynekdrlik/remoteos-mcp/master/install-linux.sh}"
REMOTEOS_MCP_CONFIG="/etc/remoteos-mcp/config.json"
if [ -n "${REMOTEOS_MCP_AUTH_KEY:-}" ]; then
    # Reject any shell/JSON-special char: the installer generates [A-Za-z0-9] keys, and a
    # non-alphanumeric value in the unquoted heredoc below would break the JSON (the installer then
    # silently discards it and generates a DIFFERENT key -- dev1's .mcp.json breaks while the
    # is-enabled gate still passes) or run command substitution. Fail loud instead.
    case "$REMOTEOS_MCP_AUTH_KEY" in
        *[!A-Za-z0-9]*) fail "REMOTEOS_MCP_AUTH_KEY must be alphanumeric [A-Za-z0-9] (installer key charset); refusing to write it unsafely (#1066)" ;;
    esac
    install -d -m 700 /etc/remoteos-mcp
    ( umask 077; cat > "$REMOTEOS_MCP_CONFIG" <<CFG
{
  "port": 8092,
  "auth_key": "${REMOTEOS_MCP_AUTH_KEY}",
  "host": "0.0.0.0"
}
CFG
    )
    chmod 600 "$REMOTEOS_MCP_CONFIG"
    echo "  #1066: pre-seeded $REMOTEOS_MCP_CONFIG from REMOTEOS_MCP_AUTH_KEY (installer reuses it; dev1 .mcp.json stays valid)"
else
    echo "  #1066: REMOTEOS_MCP_AUTH_KEY unset -- the installer will generate a fresh on-box key; update dev1's .mcp.json linux-camN entry to match"
fi
REMOTEOS_MCP_INSTALLER_TMP="$(mktemp /tmp/remoteos-mcp-install-linux.XXXXXX.sh)"
curl -fsSL "$REMOTEOS_MCP_INSTALLER_URL" -o "$REMOTEOS_MCP_INSTALLER_TMP" \
    || fail "cannot fetch remoteos-mcp installer from $REMOTEOS_MCP_INSTALLER_URL (#1066)"
bash "$REMOTEOS_MCP_INSTALLER_TMP" \
    || fail "canonical remoteos-mcp install-linux.sh failed (#1066)"
rm -f "$REMOTEOS_MCP_INSTALLER_TMP"
# Enable-only convention: ensure the reboot-survival symlink exists (idempotent if the installer
# already did `enable --now`), then gate on is-enabled. verify-device.sh's (ab) check proves the
# LIVE :8092 surface after the box reboots.
systemctl enable remoteos-mcp 2>/dev/null || true
# Compare the LITERAL is-enabled state, not `--quiet`'s exit code: `is-enabled --quiet` returns 0
# for a `static` unit (no [Install] section) too, which is NOT pulled in at boot -- the exact
# reboot-survival property this gate claims to prove. A literal `= enabled` compare rejects that
# (review 🔵); verify-device.sh's (ab) check makes the same strict compare post-reboot.
REMOTEOS_MCP_ENABLED_STATE="$(systemctl is-enabled remoteos-mcp 2>/dev/null || true)"
[ "$REMOTEOS_MCP_ENABLED_STATE" = "enabled" ] \
    || fail "remoteos-mcp.service is not enabled (is-enabled='${REMOTEOS_MCP_ENABLED_STATE:-<none>}') after install -- the linux-camN MCP surface would be dead on next boot (#1066)"
echo "  #1066: remoteos-mcp agent installed + enabled (linux-camN MCP surface :8092; proven live post-reboot by verify-device.sh (ab))"

# =============================================================================
# STEP 17c: DSCP-mark outgoing NTP client packets (dantesync issue 52)
# =============================================================================
echo ""
echo -e "${GREEN}[17c] Installing NTP-client DSCP nftables rule (dantesync issue 52)...${NC}"
# dantesync's Linux NTP client (rsntp) creates its UDP socket internally, so it cannot
# setsockopt(IP_TOS) to DSCP-mark its own NTP REQUESTS -- only the master's REPLIES are EF-marked
# (dantesync src/dscp.rs). The venue MikroTik CRS switches honour DSCP in hardware (TRUST-L3), so
# marking the request direction to the SAME class removes the queue-delay bias at the source. This
# is the camera-box provisioning half of dantesync issue 52. Smallest robust mechanism (see
# scripts/lib/dscp-nft.sh): a DEDICATED `table ip dantesync_dscp` (never `flush ruleset`, so it
# coexists with any future firewall and is idempotently replaceable) applied by a tiny systemd
# oneshot at boot -- NOT the distro nftables.service (the boxes ship none). `nft` is installed in
# STEP 16. Enable-only (no live start), per this script's convention (.claude/rules/provisioning-
# scripts.md) -- effective on the next reboot; verify-device.sh's (ae) check proves it live.
command -v nft >/dev/null 2>&1 \
    || fail "nft (nftables) is not installed -- STEP 16 must install it before the NTP-client DSCP rule can be provisioned (dantesync issue 52)"
mkdir -p "$(dirname "$DSCP_NFT_RULESET_PATH")" "$(dirname "$DSCP_NFT_SERVICE_PATH")"
dscp_nft_ruleset_content > "$DSCP_NFT_RULESET_PATH"
dscp_nft_service_unit_content > "$DSCP_NFT_SERVICE_PATH"
systemctl daemon-reload
systemctl enable "$DSCP_NFT_SERVICE_NAME"   # fail-loud (set -e) like the sibling avahi enable -- a freshly-written+reloaded unit must enable cleanly (review 5B2)
echo "  Installed: $DSCP_NFT_RULESET_PATH (udp dport 123 -> dscp ${DSCP_NFT_CLASS}) + ${DSCP_NFT_SERVICE_NAME}.service (enabled; applies at next boot)"


# =============================================================================
# STEP 18: Configure read-only root filesystem
# =============================================================================
echo ""
echo -e "${GREEN}[18/${TOTAL_STEPS}] Configuring read-only filesystem...${NC}"

# Get the root partition UUID
ROOT_UUID=$(findmnt -n -o UUID /)

# Backup original fstab -- IDEMPOTENCY GUARD (#450): only back up ONCE. An unconditional `cp`
# here clobbers /etc/fstab.bak with the ALREADY-REWRITTEN (ro+tmpfs) fstab on a re-run,
# permanently losing the true pre-provisioning original -- including the EFI entry this step
# reads back out of it, below.
if [ ! -f /etc/fstab.bak ]; then
    cp /etc/fstab /etc/fstab.bak
    echo "  Backed up original fstab to /etc/fstab.bak"
else
    echo "  /etc/fstab.bak already exists -- keeping the original backup (idempotent re-run)"
fi

# Create new fstab with read-only root and tmpfs mounts
cat > /etc/fstab << FSTABEOF
# Root filesystem - read-only for reliability
UUID=${ROOT_UUID} / ext4 ro 0 1

# EFI partition (if exists)
$(grep '/boot/efi' /etc/fstab.bak 2>/dev/null || echo "# No EFI partition")

# tmpfs mounts for writable directories
tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0
tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0
tmpfs /var/tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=50M 0 0
# #295: size /var/cache >=512M (uniformly across the fleet) so apt can never ENOSPC and leave a
# freshly-installed kernel without its initrd (a 100M /var/cache filled up and did exactly that).
tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0
tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0
FSTABEOF

echo "  Root filesystem: read-only (ro)"
echo "  tmpfs mounts: /tmp, /var/log, /var/tmp, /var/cache, /var/spool"
echo "  To remount read-write: mount -o remount,rw /"

# =============================================================================
# #679: bound /var/log (the fixed 50MB tmpfs above) against runaway growth from ANY chatty
# logger. The stock logrotate config rotates ONLY on a weekly calendar with no `size` cap -- a
# chatty logger (dantesync's per-second [PTP] Drift line was the dominant driver) filled the whole
# tmpfs in ~4-5 days and crashed cam2's camera-box.service (2026-07-11, see scripts/lib/log-bound.sh
# for the full writeup). Idempotent full-file overwrite (same pattern as the fstab rewrite above) --
# safe to re-run on an already-hardened box.
# =============================================================================
mkdir -p "$(dirname "$LOG_BOUND_TIMER_DROPIN_PATH")"
log_bound_logrotate_config > "$LOG_BOUND_LOGROTATE_PATH"
log_bound_timer_dropin > "$LOG_BOUND_TIMER_DROPIN_PATH"
systemctl daemon-reload
systemctl restart logrotate.timer
echo "  /var/log bound: ${LOG_BOUND_LOGROTATE_PATH} size cap ${LOG_BOUND_SIZE_CAP}, logrotate.timer every 15min (#679)"

# =============================================================================
# #599: restore root to its original mode now that STEP 15-18 are done -- a no-op on a
# first-provisioning run (ro only takes effect on the next reboot via the fstab just written
# above); remounts back to ro on an in-place re-run that ensure_root_writable() had remounted rw.
# =============================================================================
restore_root_mode

# =============================================================================
# STEP 19: Final verification + summary
# =============================================================================
echo ""
echo -e "${GREEN}[19/${TOTAL_STEPS}] Verifying installation...${NC}"

# #450: NEVER print "Setup Complete!" on a half-configured box. Every failure path above already
# fails loud at the point of failure, but this box being the fleet's own NDI source is a
# legitimate NON-fatal branch in STEP 4 (it fetches from itself) -- so it alone can still reach
# here without libndi.so.6 actually present. This is the belt-and-braces final gate.
MISSING=""
[ -f /usr/local/bin/camera-box ] || MISSING="${MISSING}camera-box binary (/usr/local/bin/camera-box) "
[ -f /usr/lib/ndi/libndi.so.6 ] || MISSING="${MISSING}NDI library (/usr/lib/ndi/libndi.so.6) "
if [ -n "$MISSING" ]; then
    fail "half-configured box -- missing: ${MISSING}-- refusing to report Setup Complete"
fi

echo -e "${GREEN}Setup Complete!${NC}"
echo "=========================================="
echo ""
echo "Configuration:"
echo "  Hostname:     $DEVICE_NAME"
echo "  IP Address:   $DEVICE_IP"
echo "  VBAN Stream:  $VBAN_STREAM"
echo "  Genlock FPS:  $CAMERA_GENLOCK_FPS"
echo "  NDI Name:     usb"
echo ""
echo "Optimizations applied:"
echo "  - GRUB timeout: 0s"
echo "  - Network wait: 5s"
echo "  - Power button: mute toggle (not shutdown)"
echo "  - Sleep/suspend: disabled"
echo "  - CPU governor: performance"
echo "  - CPU isolation: core 3 reserved + quieted (isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2) for the realtime grab [#289/#303]"
echo "  - Realtime pin: CPUAffinity=3 drop-in; NDI emit ${CAMERA_GENLOCK_FPS}fps (genlock.conf) [#289/#11/#451]"
echo "  - Network: optimized for streaming"
echo "  - Unnecessary services: disabled"
echo "  - Root filesystem: read-only (with tmpfs overlays)"
if [ "$DANTESYNC_INSTALLED" = true ]; then
    echo "  - Dante time sync: installed (PTP synchronization)"
fi
echo ""
echo -e "${YELLOW}Next steps:${NC}"
echo "  1. Apply network config: netplan apply"
echo "  2. Reboot: reboot"
echo ""
echo -e "${GREEN}After reboot, connect via: ssh root@${DEVICE_IP}${NC}"
