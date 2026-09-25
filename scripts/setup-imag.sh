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
# GH_TOKEN (repo-read scope) is required for the genlock hot-swap step: imag-nb runs the CUSTOMIZED genlock
# OBS+DistroAV build (#460), hot-swapped over the PPA base — the artifacts are GitHub Actions
# workflow artifacts on this PRIVATE repo, which `gh run download` needs auth to fetch.
#
# Topology (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md):
#   6× cam box NDI 1080p60 -> imag-nb OBS (1080p60 low-latency IMAG, genlock build #460) ->
#   HDMI program projector
#
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

# #816: the box under provisioning is a PARAMETER, not a constant. The default stays the
# incumbent imag notebook so every existing invocation is unchanged; a REPLACEMENT box is
# provisioned with `IMAG_IP=10.77.9.187 sudo -E ./setup-imag.sh` while the old one is still live
# on .182 (two boxes cannot hold one address).
STATIC_IP="${IMAG_IP:-10.77.9.182}"
PREFIX="23"
# #816: the NDI runtime (6.3.2) is copied from a fleet cam box. Pinning cam1 alone made
# provisioning FAIL whenever that ONE box was down (it was, on 2026-07-27 — its grabber card had
# been lent out). Every cam box carries the same fleet-identical runtime, so any REACHABLE one
# will do; the list is probed in order and the first live box wins.
NDI_PEER_CANDIDATES="${NDI_PEER_CANDIDATES:-10.77.9.61 10.77.9.62 10.77.9.63 10.77.9.64 10.77.9.65 10.77.9.66 10.77.9.67}"
# #824: the OBS base package version MUST match the genlock build's OBS version — libobs refuses
# any stock plugin built against a NEWER libobs ("compiled with newer libobs 32.2"), which on the
# .187 bring-up left OBS with only distroav.so loaded: no obs-websocket, no encoders. Overridable;
# bump it together with the vendored genlock build (#825).
IMAG_OBS_BASE_VERSION="${IMAG_OBS_BASE_VERSION:-32.2.0-0obsproject1~noble}"
NDI_PEER="${NDI_PEER:-}"         # resolved at first use from NDI_PEER_CANDIDATES (or pinned by env)
NDI_DIR="/usr/lib/ndi"
DESKTOP_USER="newlevel"
USER_HOME="/home/${DESKTOP_USER}"
OBS_CFG="${USER_HOME}/.config/obs-studio"
# #541: dev1's control-node SSH public key — installed into ${DESKTOP_USER}'s authorized_keys
# (step 19) so `scripts/drift-guard.sh --check-imag` (the #531 dynamic genlock-build staleness
# guard) can SSH from dev1 to this box NON-INTERACTIVELY (-o BatchMode=yes). This is the PUBLIC
# half of dev1's existing ~/.ssh/id_ed25519 keypair — safe to commit (a public key grants nothing
# without the matching private key, which never leaves dev1). NEVER put a private key here.
DEV1_DRIFTGUARD_PUBKEY="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB/akQWI95uekn0/CRfQA2I8vu1a/kU9sx6SmUdA3lOf dev1-driftguard-control-541"
# type+base64 only (the trailing comment is cosmetic and NOT used by sshd for auth) -- matching on
# this instead of the full line makes the idempotency check in step 19 immune to a differently
# commented instance of the SAME key already being present (e.g. installed by hand with the local
# ~/.ssh/id_ed25519.pub file's own comment).
DEV1_DRIFTGUARD_PUBKEY_TYPE_BLOB="${DEV1_DRIFTGUARD_PUBKEY% *}"
TOTAL_STEPS=28
# #731: Companion Satellite server this box connects the local Stream Deck to. .lan DNS is
# usually fine on this LAN (companion.lan -> companion-snv.lan, verified live 2026-07-13) but can
# be flaky like any other .lan name on this network -- COMPANION_HOST_IP is the documented
# fallback (see targets.md) if COMPANION_HOST ever stops resolving from a box.
COMPANION_HOST="${COMPANION_HOST:-companion.lan}"
COMPANION_HOST_IP="${COMPANION_HOST_IP:-}"

step() { echo -e "${GREEN}[$1/${TOTAL_STEPS}] $2${NC}"; }
fail() { echo -e "${RED}FAIL: $1${NC}" >&2; exit 1; }

# issue 1357: the shared OBS-box appliance baseline (the box-level steps below call it; setup-strih.sh
# sources the same lib). Sourced BEFORE the source guard so the imag_* helper wrappers resolve when
# this file is sourced by the unit tests, verify-imag.sh and recording-e2e.sh too. The directory is a
# pure parameter expansion (no dirname/cd): a caller may source this file with an empty PATH (the
# missing-tool tests do), and an external command here would break the source itself.
_OBS_BOX_HERE="${BASH_SOURCE[0]%/*}"
[ "$_OBS_BOX_HERE" != "${BASH_SOURCE[0]}" ] || _OBS_BOX_HERE=.
# shellcheck source=scripts/lib/obs-box-baseline.sh
. "${_OBS_BOX_HERE}/lib/obs-box-baseline.sh"

# --- PURE functions (no network, no root, no side effects — sourced + unit-tested from
# tests/setup_imag_pure_functions.rs against synthetic fixtures; the BASH_SOURCE guard below
# skips the destructive provisioning flow when sourced) ---------------------------------------

# manifest_sha_for_path MANIFEST RELPATH -> the #120 manifest's recorded sha256 for RELPATH, or
# fails loud if RELPATH isn't listed. Narrow, single-purpose lookup (NOT a full bundle-completeness
# walk like scripts/genlock-manifest.sh --check) — setup-imag.sh only ever needs THREE specific
# files verified (libobs.so.30 + distroav.so + #499 bin/obs), never all ~1600 bundle files, so
# hashing everything would be pure waste (the bundle also carries the Qt frontend + locale data +
# default plugins, none of which this script installs). setup-imag.sh is copied to the box
# standalone (no sibling scripts/genlock-manifest.sh checked out there), so this cannot shell out
# to the repo's own tool.
manifest_sha_for_path() {
    local manifest="$1" relpath="$2" sha
    sha="$(jq -r --arg p "$relpath" '.files[] | select(.path == $p) | .sha256' "$manifest")" \
        || fail "cannot parse $manifest"
    [ -n "$sha" ] || fail "#460 manifest lists no entry for $relpath — refuse to trust an unverifiable file"
    printf '%s\n' "$sha"
}

# _genlock_marker_atomic / genlock_write_markers — issue 789: the shared genlock deploy marker
# writer, kept BEHAVIORALLY IDENTICAL to scripts/lib/genlock-markers.sh's copy of the same name and
# locked to it by tests/deploy_genlock_fleet.rs::inline_genlock_write_markers_matches_the_shared_lib.
# setup-imag.sh is scp'd to the box STANDALONE (no sibling scripts/lib checked out there — same
# reason manifest_sha_for_path cannot shell out to the repo's own tool), so it carries this inline
# copy rather than sourcing the lib; the fleet script's on-imag program sources the lib instead.
# Writes each marker temp-then-rename (a POSIX same-dir `mv -f`) so a concurrent drift-guard reader
# never sees a half-written marker. A missing MARKER_DIR/GENLOCK_SHA/DISTROAV_SHA is fail-loud
# (return 2), never a silent partial write — under this script's `set -e` a non-zero return aborts.
_genlock_marker_atomic() {
    local dest="$1" content="$2" tmp
    tmp="${dest}.tmp.$$"
    if ! printf '%s\n' "$content" > "$tmp" 2>/dev/null; then
        echo "genlock_write_markers: could not write temp file $tmp" >&2
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
    if ! mv -f "$tmp" "$dest" 2>/dev/null; then
        echo "genlock_write_markers: could not rename $tmp -> $dest" >&2
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
    return 0
}
genlock_write_markers() {
    local marker_dir="${1:-}" genlock_sha="${2:-}" distroav_sha="${3:-}" deployed_at="${4:-}"
    if [ -z "$marker_dir" ]; then
        echo "genlock_write_markers: MARKER_DIR (arg 1) is required" >&2; return 2
    fi
    if [ -z "$genlock_sha" ]; then
        echo "genlock_write_markers: GENLOCK_SHA (arg 2) is required" >&2; return 2
    fi
    if [ -z "$distroav_sha" ]; then
        echo "genlock_write_markers: DISTROAV_SHA (arg 3) is required" >&2; return 2
    fi
    [ -n "$deployed_at" ] || deployed_at="$(date -Is)"
    if ! mkdir -p "$marker_dir" 2>/dev/null; then
        echo "genlock_write_markers: could not create marker dir $marker_dir" >&2; return 1
    fi
    _genlock_marker_atomic "$marker_dir/GENLOCK_BUILD_SHA.txt"  "$genlock_sha"  || return 1
    _genlock_marker_atomic "$marker_dir/DISTROAV_BUILD_SHA.txt" "$distroav_sha" || return 1
    _genlock_marker_atomic "$marker_dir/DEPLOYED_AT"           "$deployed_at"  || return 1
    return 0
}

# imag_cpu_isolation_plan / imag_has_discrete_nvidia / imag_same_unit -- issue 1357: the
# implementations moved VERBATIM into scripts/lib/obs-box-baseline.sh (as obs_box_cpu_isolation_plan /
# obs_box_has_discrete_nvidia / obs_box_same_unit) so setup-strih.sh derives the SAME facts. These
# imag_* names stay as the stable entry points verify-imag.sh / recording-e2e.sh / the unit tests call
# (they source this file); see the lib for the decisions themselves (#483/#816/#823).
imag_cpu_isolation_plan() { obs_box_cpu_isolation_plan "$@"; }

imag_has_discrete_nvidia() { obs_box_has_discrete_nvidia "$@"; }

# imag_pick_ndi_peer  (stdin: one "HOST STATUS" line per candidate, in preference order) -> the
# first host whose STATUS is "up". Fails loud when none is reachable — provisioning cannot fetch
# the fleet NDI runtime from nowhere, and a silent empty peer would surface much later as a
# confusing scp error (#816).
imag_pick_ndi_peer() {
    local host status
    while read -r host status; do
        [ -n "$host" ] || continue
        if [ "$status" = "up" ]; then printf '%s\n' "$host"; return 0; fi
    done
    fail "no reachable cam box among the NDI runtime candidates — cannot fetch the fleet NDI runtime"
}

# imag_resolve_ndi_peer  (args: candidate hosts, default $NDI_PEER_CANDIDATES) -> the first
# REACHABLE one on stdout. Probes every candidate into a BUFFER first, then feeds the picker —
# never `for … | imag_pick_ndi_peer`. The picker returns on the first "up" line, which closes the
# pipe, so a still-running writer loop dies on SIGPIPE and `set -euo pipefail` fails the WHOLE
# substitution: the live .187 provisioning run aborted with a bare `exit 1` and zero output while
# cam1 was up and answering. Same trap this script already documents at its `ldconfig | grep -q`
# site — buffer first. #1047: the buffer is then fed to the picker via a HERE-STRING, not a
# `printf … | picker` PIPE. `imag_pick_ndi_peer` still returns on the first "up" line (an early-exit
# consumer), so any concurrent writer PROCESS feeding it through a pipe can take SIGPIPE the moment
# the picker closes the read-end — and the `printf` write only completes atomically while `$probe`
# fits the 64 KiB pipe buffer (7 candidates ≈ 98 B today). A larger/overridden candidate list makes
# `printf`'s write block with the tail unwritten, the picker closes early, and the blocked write
# gets EPIPE → 141 → pipefail aborts provisioning (CI run 31757820465 flaked this once). A
# here-string has NO concurrent writer process, so the early exit can never SIGPIPE anything,
# regardless of buffer size. The optional CANDIDATE args exist so a caller (or a future
# test) can override the fleet default without touching NDI_PEER_CANDIDATES; no site does today,
# which is exactly what trips shellcheck's "references arguments, but none are ever passed"
# check (SC2120) -- it cannot see that the zero-arg call path is the intended, exercised
# behaviour (see tests/setup_imag_hardware_agnostic.rs), not an unused parameter.
# shellcheck disable=SC2120
imag_resolve_ndi_peer() {
    local candidates=("$@") probe="" h
    [ "${#candidates[@]}" -gt 0 ] || read -r -a candidates <<<"$NDI_PEER_CANDIDATES"
    for h in "${candidates[@]}"; do
        if ping -c1 -W1 "$h" >/dev/null 2>&1; then
            probe="${probe}${h} up"$'\n'
        else
            probe="${probe}${h} down"$'\n'
        fi
    done
    imag_pick_ndi_peer <<<"$probe"
}

# imag_require_tools TOOL... -> fail loud, NAMING the missing tool(s). #822: step 12 verifies the
# hot-swapped binaries with `readelf`/`nm` (binutils). On a freshly installed box binutils is
# ABSENT, so those commands emit nothing, the greps find nothing, and the step aborted with
# "SONAME check failed — refuse a mismatched ABI" — blaming the artifact for a missing TOOL, while
# the swap had actually succeeded. A verification that cannot run is not a failed verification.
imag_require_tools() {
    local t missing=""
    for t in "$@"; do
        command -v "$t" >/dev/null 2>&1 || missing="${missing:+$missing }$t"
    done
    [ -z "$missing" ] || fail "#822: required verification tool(s) not installed: ${missing} (apt-get install binutils) — refusing to run a check that cannot execute"
}

imag_same_unit() { obs_box_same_unit "$@"; }

# imag_obs_base_plan CANDIDATE WANTED -> "apt" when the PPA still offers the wanted version,
# "deb" when it has moved on (the superseded binary is still downloadable from Launchpad). Never
# "just take the candidate" — a base whose libobs is NEWER than the genlock build disables every
# stock plugin (#824).
imag_obs_base_plan() {
    local candidate="$1" wanted="$2"
    [ -n "$wanted" ] || fail "#824: no OBS base version pinned"
    if [ "$candidate" = "$wanted" ]; then printf 'apt\n'; else printf 'deb\n'; fi
}

# imag_obs_base_deb_url VERSION -> the Launchpad +files URL for that (possibly superseded) PPA
# binary. The PPA pool only keeps the CURRENT version; +files keeps superseded ones (live-verified
# 200 for 32.1.2 while the pool 404s).
imag_obs_base_deb_url() {
    printf 'https://launchpad.net/~obsproject/+archive/ubuntu/obs-studio/+files/obs-studio_%s_amd64.deb\n' "$1"
}

# verify_file_sha FILE EXPECTED_SHA LABEL -> fail loud on any mismatch (corrupted/tampered file).
verify_file_sha() {
    local f="$1" want="$2" label="$3" got
    [ -f "$f" ] || fail "$label: file missing at $f"
    got="$(sha256sum "$f" | awk '{print $1}')"
    [ "$got" = "$want" ] || fail "$label: sha256 mismatch (want $want got $got) — corrupted/tampered artifact"
}

# --- source-guard: when sourced (the unit tests), stop here — never run the destructive
# provisioning flow below. Same convention as scripts/launch-obs-genlock.sh /
# scripts/genlock-manifest.sh / scripts/drift-guard.sh. -----------------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0
fi

[ "$EUID" -eq 0 ] || fail "run as root (sudo)"
id "$DESKTOP_USER" >/dev/null 2>&1 || fail "user $DESKTOP_USER missing"

ASSUME_YES=0
[ "${1:-}" = "--yes" ] || [ "${1:-}" = "-y" ] && ASSUME_YES=1
if [ "$ASSUME_YES" -ne 1 ]; then
    read -p "Provision this box as imag-nb (${STATIC_IP})? (y/N) " -n 1 -r; echo
    [[ $REPLY =~ ^[Yy]$ ]] || { echo "Aborted."; exit 1; }
fi

# issue 1357: the FIRST provisioning action, before any apt-get -- apt-get waits up to 10 min for a
# background apt run's dpkg lock instead of failing at once (the shared baseline drop-in; setup-strih.sh
# runs it too).
obs_box_apt_lock_timeout

# #816: resolve the NDI runtime peer ONCE, up front, from whichever cam box is actually alive —
# a fleet box being down (grabber card lent out, being re-flashed) must not abort provisioning of
# an unrelated notebook. `ping -c1 -W1` per candidate: fast, and reachability is exactly what the
# later scp needs. NDI_PEER pinned by env skips the probe entirely.
if [ -z "$NDI_PEER" ]; then
    NDI_PEER="$(imag_resolve_ndi_peer)" \
        || fail "#816: could not resolve an NDI runtime peer from: ${NDI_PEER_CANDIDATES} — pin one with NDI_PEER=<ip> if the fleet is down"
    echo "  #816: NDI runtime peer = ${NDI_PEER} (first reachable of: ${NDI_PEER_CANDIDATES})"
fi

# Pre-flight: curl + CA certs BEFORE first use (the cam5/#450 lesson — a base image without
# curl makes every download step fail silently mid-run; ensure it up-front, fail loud).
if ! command -v curl >/dev/null 2>&1; then
    obs_box_apt_update
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
    # #1103: Wake-on-LAN -- arm the NDI NIC for a magic-packet wake so a post-event powered-down /
    # slept imag-nb is remotely recoverable (the imag counterpart of issue 1053's strih/stream WoL).
    # Set on the SAME $CON as the static IP above (ONE source of truth); NM re-applies it on every
    # connection-up (every boot), so it survives reboot. Live-confirmed the NIC supports it (r8152 USB
    # dongle: Supports Wake-on: pumbg). The BIOS standby-power layer is a separate hands-on step
    # (docs/wake-on-lan.md) -- this arms the OS half. The one con up below applies IP + WoL together.
    nmcli con mod "$CON" 802-3-ethernet.wake-on-lan magic 802-3-ethernet.wake-on-lan-password ""
    nmcli con up "$CON" >/dev/null || true   # same IP — session survives
    echo "  static ${STATIC_IP}/${PREFIX} gw=$GW dns=$DNS on $NIC ($CON)"
    echo "  #1103: Wake-on-LAN armed (802-3-ethernet.wake-on-lan=magic) on $CON"
else
    fail "nmcli missing — desktop Ubuntu expected (netplan-only path not implemented)"
fi

# =============================================================================
step 2 "Network performance tuning (#486): sysctl + EEE off, flow-control advertised on the NDI NIC"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_network_tuning "$NIC" imag

# =============================================================================
step 3 "DanteSync (#479): pin imag's system clock to the cluster master (genlock needs it)"
# =============================================================================
# imag's genlock render tick + FIFO ts-align read clock_gettime(CLOCK_REALTIME) — the system
# clock. Without cluster clock discipline, imag free-runs vs the dante-disciplined cameras and
# the genlock FIFO ts_head_skew drifts unbounded (live-proven 2026-07-04: skew drifted
# 474->541ms/80min, CAM underruns climbing). DanteSync OWNS the clock (ops skill hard rule) —
# NEVER timesyncd/chrony/ptp4l alongside it. Pin the NIC ($NIC, resolved in step 1) since imag is
# a notebook with other network interfaces that must not be mistaken for the rig link.
DANTESYNC_REPO="zbynekdrlik/dantesync"
DANTESYNC_URL="$(curl -fsSL "https://api.github.com/repos/${DANTESYNC_REPO}/releases/latest" 2>/dev/null \
    | grep -o '"browser_download_url": *"[^"]*dantesync-linux-amd64"' \
    | grep -o 'https://[^"]*' | head -1 || true)"

# #1289: derive the release's TARGET version from the download URL's own path
# (.../releases/download/vX.Y.Z/dantesync-linux-amd64), and read the CURRENTLY-installed
# dantesync's own version (per .claude/rules/dantesync-version-reading.md: `dantesync --version`
# answers on every platform, no journal/bundle-state coupling needed). Skip the install entirely
# when they already agree -- both to avoid a needless replace on an idempotent re-run, and because
# `curl -o`/`scp -O` straight onto a RUNNING dantesync.service binary fails (the kernel refuses to
# open a currently-executing file for write, ETXTBSY) -- mirrors the setup-device.sh #1289 fix.
DANTESYNC_TARGET_VERSION=""
if [ -n "$DANTESYNC_URL" ]; then
    DANTESYNC_TARGET_VERSION="$(printf '%s\n' "$DANTESYNC_URL" | grep -oE '/v[0-9]+\.[0-9]+\.[0-9]+/' | head -1 | tr -d '/v' || true)"
fi
DANTESYNC_CURRENT_VERSION=""
if [ -x /usr/local/bin/dantesync ]; then
    DANTESYNC_CURRENT_VERSION="$(/usr/local/bin/dantesync --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
fi

# #1289: an ALREADY-installed dantesync is fine to keep even when the release URL could not be
# determined (imag's other network interfaces mean GitHub is not always reachable, per the
# pre-existing fallback ladder below) -- only reinstall when we KNOW the target differs, never
# because we simply couldn't check. Preserves the pre-#1289 behavior for this specific case (the
# old `if [ ! -x /usr/local/bin/dantesync ]` gate skipped the whole ladder whenever a binary was
# already present, regardless of network reachability).
if [ -x /usr/local/bin/dantesync ] \
    && { [ -z "$DANTESYNC_TARGET_VERSION" ] || [ "$DANTESYNC_CURRENT_VERSION" = "$DANTESYNC_TARGET_VERSION" ]; }; then
    if [ -n "$DANTESYNC_TARGET_VERSION" ]; then
        echo "  dantesync already at v$DANTESYNC_CURRENT_VERSION -- skipping install"
    else
        echo "  dantesync already installed (release URL unreachable -- keeping existing binary)"
    fi
else
    # download/copy to a temp file and `install -m 0755` it into place, never write straight onto
    # the live path (see #1289 rationale above) -- install's rename-into-place semantics are safe
    # over a running dantesync.service.
    _dantesync_dl_tmp="$(mktemp)"
    if [ -n "$DANTESYNC_URL" ]; then
        curl -fsSL "$DANTESYNC_URL" -o "$_dantesync_dl_tmp" || {
            rm -f "$_dantesync_dl_tmp"
            fail "dantesync download failed"
        }
    elif [ -n "${CAM_PW:-}" ]; then
        command -v sshpass >/dev/null 2>&1 || apt-get install -y sshpass >/dev/null
        sshpass -p "$CAM_PW" scp -O -o StrictHostKeyChecking=no \
            "${DESKTOP_USER}@${NDI_PEER}:/usr/local/bin/dantesync" "$_dantesync_dl_tmp" \
            || {
                rm -f "$_dantesync_dl_tmp"
                fail "dantesync copy from cam1 fallback failed"
            }
    else
        rm -f "$_dantesync_dl_tmp"
        fail "dantesync: no GitHub release asset found and CAM_PW unset for the cam-box fallback copy"
    fi
    [ -s "$_dantesync_dl_tmp" ] || {
        rm -f "$_dantesync_dl_tmp"
        fail "downloaded/copied dantesync is empty"
    }
    install -m 0755 "$_dantesync_dl_tmp" /usr/local/bin/dantesync || {
        rm -f "$_dantesync_dl_tmp"
        fail "could not install dantesync to /usr/local/bin/dantesync"
    }
    rm -f "$_dantesync_dl_tmp"
fi
[ -x /usr/local/bin/dantesync ] || fail "dantesync binary missing after install attempt"

# #1215: install the phase_slew canary config (dantesync issue 97) so dantesync SLEWS phase
# error smoothly instead of STEPPING it in discrete jumps on a from-scratch provision. imag-nb was
# the ONE box on the rig that never received this -- it had no /etc/dantesync/ directory at all
# and stepped 16x/hour of 6-7ms each, a visible hitch on the projected output every ~4 minutes.
# The cam1-4 fleet got its copy through an out-of-band canary rollout, never this script -- this
# is written BEFORE the systemd unit/restart below so the same restart that proves PTP re-lock
# also picks up phase_slew on a first provision. RIG_GRANDMASTER_IP mirrors the SAME override
# verify-imag.sh already uses (#834) so one env var controls the grandmaster everywhere.
_RG_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# #1307: DNS-named grandmaster (video-clock.lan) via the shared resolver -- the allowlist written
# below carries the RESOLVED IPv4 until zbynekdrlik/dantesync#113 lets it carry the hostname.
# shellcheck source=scripts/lib/rig-grandmaster.sh
. "$_RG_HERE/lib/rig-grandmaster.sh"
# shellcheck source=scripts/lib/ndi-runtime.sh
. "$_RG_HERE/lib/ndi-runtime.sh"   # issue 1317: shared NDI 6.3.2 runtime install recipe (with setup-strih.sh)
RIG_GRANDMASTER_IP="$(rig_grandmaster_ip)" || {
  echo "FAIL: setup-imag: cannot resolve the PTP grandmaster host -- refusing to write an empty gm_allowlist (#1307)" >&2
  exit 1
}
install -d -m 755 /etc/dantesync
cat > /etc/dantesync/config.json <<DANTECFGEOF
{
  "_ntp_server_examples": "sk.pool.ntp.org, europe.pool.ntp.org, time.google.com, time.cloudflare.com",
  "http_status": {
    "enabled": true,
    "port": 8898
  },
  "ntp_server_mode": {
    "enabled": false,
    "max_step_us": 100000,
    "port": 123,
    "stratum": 3
  },
  "system": {
    "gm_allowlist": [
      "${RIG_GRANDMASTER_IP}"
    ],
    "phase_slew": {
      "enabled": true
    }
  }
}
DANTECFGEOF
chmod 644 /etc/dantesync/config.json
echo "  #1215: /etc/dantesync/config.json installed (phase_slew.enabled=true, gm_allowlist=${RIG_GRANDMASTER_IP}) -- reprovision-durable"

cat > /etc/systemd/system/dantesync.service <<EOF
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantesync -i ${NIC} --ntp-server strih.lan
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

# DanteSync OWNS the clock — MASK systemd-timesyncd so nothing else ever disciplines it (ops
# skill hard rule: NEVER chrony/ptp4l/timesyncd alongside dantesync). Mask BEFORE enabling
# dantesync so the two clock sources can never race even for one boot cycle.
timedatectl set-ntp false 2>/dev/null || true
systemctl disable --now systemd-timesyncd >/dev/null 2>&1 || true
systemctl mask systemd-timesyncd >/dev/null 2>&1 || true

systemctl daemon-reload
systemctl enable dantesync >/dev/null 2>&1 || true
# #491: stamp the restart instant, then FORCE a restart so the running daemon matches the
# just-(re)installed binary+unit, and verify a FRESH post-restart lock. A re-provision restarts an
# already-locked dantesync; PTP re-acquisition then takes 30-70s while the pre-restart LOCK lines
# scroll out of a `-n 50` window — the old 60s / `-n 50` check false-failed even though dantesync
# re-locked fine. The journal read is ANCHORED to the restart instant (--since) so a stale
# pre-restart LOCK can't satisfy it and a fresh one can't be scrolled out; budget is 150s.
DANTESYNC_RESTART_EPOCH="$(date +%s)"
systemctl restart dantesync

# Verify PTP/NTP lock — the Linux equivalent of the ops-skill journalctl check. Accepts either
# PTP LOCK/NANO or the NTP-fallback offset line (grandmaster may be transiently absent).
DANTESYNC_LOCKED=0
for i in $(seq 1 75); do
    if journalctl -u dantesync --no-pager --since "@$DANTESYNC_RESTART_EPOCH" 2>/dev/null | grep -qE '\[PTP\][[:space:]]+(LOCK|NANO)|\[NTP\] offset'; then
        DANTESYNC_LOCKED=1
        break
    fi
    sleep 2
done
[ "$DANTESYNC_LOCKED" -eq 1 ] || fail "dantesync did not report PTP/NTP lock within 150s of restart — genlock clock discipline not established (check journalctl -u dantesync)"
echo "  dantesync locked to strih.lan via $NIC (timesyncd masked)"

# =============================================================================
step 4 "Max performance: governor + no USB/NIC powersave (USB-ethernet feeds the NDI!)"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_max_performance "$NIC" imag

# =============================================================================
step 5 "Never sleep: lid ignore + power/suspend/hibernate key ignore (#727) + sleep masked + idle/blank/lock off (openbox: xset in the step-16 autostart; the gsettings below apply while GNOME is still installed on THIS run — they become no-ops once step 15 purges it later in the same run, and on any subsequent re-run)"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
# It also exports UBUS (the desktop user's session bus) that steps 16/17/21/27 below reuse.
obs_box_never_sleep "$DESKTOP_USER" imag

# =============================================================================
step 6 "Boot safety net (#487): kernel apt-hold + initrd-guarantee hook + unattended-upgrade lockdown"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
# The kernel SERIES (24.04 on imag) is derived from /etc/os-release, not hard-coded -- the boot-safety
# hold and step 7's lowlatency meta package are named by it. safe_grub_regen (#295) moved with it.
IMAG_KERNEL_SERIES="$(obs_box_kernel_series)" \
    || fail "#487: cannot derive the Ubuntu release series from /etc/os-release -- refusing to hold/install kernel packages by a guessed name"
obs_box_boot_safety_net "$IMAG_KERNEL_SERIES" imag

# =============================================================================
step 7 "Low-latency kernel (#482): preempt=full via lowlatency-kernel config — zero downgrade"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_lowlatency_kernel "$IMAG_KERNEL_SERIES"

# =============================================================================
step 8 "CPU affinity (#483/#842): P-core block reserved for OBS -- AFFINITY-ONLY, no kernel isolcpus"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
# obs_box_cpu_affinity derives the AFFINITY-ONLY P-core block from this box's topology (#483/#842),
# persists it to /etc/imag-isolated-cpus.conf and self-heals a leftover 98-imag-isolation.cfg.
obs_box_cpu_affinity imag
# #841: the SAME derived value feeds the boot autostart's env export (step 16) and step 17's launch.
IMAG_ISOLATED_CPUS="$OBS_BOX_ISOLATED_CPUS"

# camera-box #484: grant the desktop user rtprio so OBS's genlock render-tick pin can go SCHED_FIFO.
# The #484 pin (vendor/obs-studio/libobs/obs-video.c) calls sched_setscheduler(SCHED_FIFO) on the ONE
# timing-critical graphics thread and pins it to the nohz_full=10,11 cores reserved just above. OBS
# runs as the UNPRIVILEGED ${DESKTOP_USER}, so without an rtprio ulimit grant that syscall fails
# EPERM and the pin's warn-and-continue fallback silently leaves the thread SCHED_OTHER (harmless but
# inert). This limits.d drop-in grants rtprio 20 (headroom above the ~10 the thread requests); PAM
# applies it at the user's next login session — i.e. from the next boot's lightdm autologin, the same
# boot the #483 isolation takes effect. Idempotent (rewritten each run).
cat > /etc/security/limits.d/95-imag-genlock-rtprio.conf <<EOF
# camera-box #484: allow ${DESKTOP_USER} to set SCHED_FIFO (rtprio) so OBS's genlock render-tick
# thread can be pinned realtime on the #483-reserved nohz_full cores (cpu10,11). Value 20 = headroom
# above the ~10 the thread requests. Applied by PAM at session start (next boot's autologin).
${DESKTOP_USER}   -   rtprio   20
EOF
echo "  #484: /etc/security/limits.d/95-imag-genlock-rtprio.conf grants ${DESKTOP_USER} rtprio 20"
echo "  NOTE: the rtprio grant applies at the next login session (next boot's autologin)"

# =============================================================================
step 9 "NVIDIA dGPU driver (#500): nvidia-driver-595-open + PRIME nvidia-primary"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_nvidia_prime imag

# =============================================================================
step 10 "NDI runtime 6.3.2 from ${NDI_PEER} -> ${NDI_DIR} (fleet-identical)"
# =============================================================================
# issue 1317: the NDI 6.3.2 runtime install recipe now lives in the SHARED scripts/lib/ndi-runtime.sh
# (`ndi_runtime_install_cmds`) so setup-imag + setup-strih install the SAME runtime from ONE source of
# truth. Behaviour is unchanged: idempotent copy from a cam box (only when absent) + the libndi.so.6 /
# libndi.so symlinks, the /etc/ld.so.conf.d/ndi.conf + ldconfig (no `grep -q` SIGPIPE), the
# /usr/local/lib/libndi.so.6 symlink DistroAV's Linux loader scans, and avahi-daemon -- PLUS an
# unconditional root:root a+rX perms-normalize (a no-op on imag's already-readable copy, a fix for the
# strih 0600-dlopen case). The eval runs in a subshell so the recipe's fail-loud `exit 1` is caught here.
[ -e "${NDI_DIR}/libndi.so.6" ] || [ -n "${CAM_PW:-}" ] || fail "CAM_PW env required to fetch NDI runtime from cam1"
( eval "$(ndi_runtime_install_cmds "$NDI_PEER" "${CAM_PW:-}" "$DESKTOP_USER" "$NDI_DIR")" ) \
    || fail "NDI runtime install failed (see above)"

# =============================================================================
step 11 "OBS Studio (official PPA, 32.x) — base install; libobs.so.30 gets genlock hot-swapped next"
# =============================================================================
INSTALLED_OBS_VERSION="$(dpkg-query -W -f='${Version}' obs-studio 2>/dev/null || true)"
if [ "$INSTALLED_OBS_VERSION" != "$IMAG_OBS_BASE_VERSION" ]; then
    add-apt-repository -y -n ppa:obsproject/obs-studio >/dev/null   # -n: never refresh the lists here -- obs_box_apt_update below does, waiting out a held lock
    obs_box_apt_update
    OBS_CANDIDATE="$(apt-cache policy obs-studio 2>/dev/null | awk '/Candidate:/{print $2}')"
    OBS_BASE_PLAN="$(imag_obs_base_plan "$OBS_CANDIDATE" "$IMAG_OBS_BASE_VERSION")" || exit 1
    echo "  #824: OBS base pin ${IMAG_OBS_BASE_VERSION} (PPA candidate ${OBS_CANDIDATE:-none}) -> ${OBS_BASE_PLAN}"
    if [ "$OBS_BASE_PLAN" = "apt" ]; then
        DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-downgrades \
            --allow-change-held-packages "obs-studio=${IMAG_OBS_BASE_VERSION}" >/dev/null \
            || fail "#824: obs-studio=${IMAG_OBS_BASE_VERSION} install failed"
        # The hold is DELIBERATE (issue 824: no unattended PPA upgrades under the hot-swap) — the
        # pinned install above may legitimately MOVE the held version when the #824 pin advances
        # with the PPA candidate, so it must be allowed to change a held package; re-assert the
        # hold right after so the box never floats.
        apt-mark hold obs-studio >/dev/null 2>&1 || true
    else
        # the PPA moved on — fetch the pinned (superseded) binary from Launchpad's +files endpoint
        OBS_DEB_URL="$(imag_obs_base_deb_url "$IMAG_OBS_BASE_VERSION")"
        curl -fsSL -o /tmp/obs-base.deb "$OBS_DEB_URL" \
            || fail "#824: could not download the pinned OBS base ${IMAG_OBS_BASE_VERSION} from ${OBS_DEB_URL}"
        DEBIAN_FRONTEND=noninteractive apt-get install -y --allow-downgrades /tmp/obs-base.deb >/dev/null \
            || fail "#824: pinned OBS base ${IMAG_OBS_BASE_VERSION} install failed"
        rm -f /tmp/obs-base.deb
    fi
fi
# #824: hold it — an unattended upgrade to a newer libobs disables every stock plugin against the
# genlock build (no obs-websocket, no encoders).
apt-mark hold obs-studio >/dev/null 2>&1 \
    || echo "  WARNING: apt-mark hold obs-studio failed — an apt upgrade could break the genlock plugin ABI"
GOT_OBS_VERSION="$(dpkg-query -W -f='${Version}' obs-studio 2>/dev/null || true)"
[ "$GOT_OBS_VERSION" = "$IMAG_OBS_BASE_VERSION" ] \
    || fail "#824: OBS base is ${GOT_OBS_VERSION:-none}, expected ${IMAG_OBS_BASE_VERSION} — a mismatched base disables every stock plugin against the genlock build"
obs --version 2>/dev/null || true

# =============================================================================
step 12 "Genlock hot-swap (#460): deploy patched libobs.so.30 + distroav.so over the PPA base"
# =============================================================================
# imag-nb MUST run the CUSTOMIZED genlock OBS+DistroAV, not stock DistroAV (user directive,
# #458 comment 2026-07-03) — the stock-bootstrap path this step used to run is dead. The PPA
# obs-studio package installed in the prior step ships libobs.so.30 with SONAME "libobs.so.30"
# (live-verified on imag-nb) — IDENTICAL to the genlock build's own SONAME — so the genlock
# libobs.so.30 hot-swaps cleanly over it, exactly mirroring the Windows obs.dll hot-swap (see
# .claude/skills/genlock). libobs.so.30 (the genlock render-tick/ts-align patches live in
# obs-source.c/obs-video.c, both inside libobs core), distroav.so, AND the FRONTEND executable
# /usr/bin/obs are all swapped — obs-frontend-api/obs-opengl/obs-scripting (the shared LIBRARIES)
# are untouched by the genlock patches and stay PPA-stock. No stock DistroAV .deb is installed any
# more — the genlock-built distroav.so IS the plugin.
#
# #499: /usr/bin/obs (the frontend EXECUTABLE, compiled from vendor/obs-studio/frontend/) MUST
# also be swapped — skipping it leaves a half-stock box. The multiview render-budget decouple
# (#276 obs_display_set_render_divisor / #278 adaptive EWMA skip / #293 anti-starvation floor) AND
# the "newlevel.media" window title both live in the frontend EXE, NOT in libobs.so.30 — exactly
# the "frontend lives in the exe, not the DLL" gotcha already documented for Windows
# (.claude/skills/genlock/SKILL.md). A genlock deploy that only swaps libobs.so.30/distroav.so
# leaves the stock frontend running: the multiview code path that calls
# obs_display_set_render_divisor() never runs, and multiview chokes the program render (live-
# proven 2026-07-04: 16fps/59ms with the stock frontend vs 60fps/1.7ms once bin/obs was swapped).
GENLOCK_REPO="zbynekdrlik/camera-box"
GENLOCK_WORKFLOW="linux-genlock.yml"
GENLOCK_MARKER_DIR="/opt/obs-genlock"
GENLOCK_BACKUP_ROOT="/opt/obs-backup"
LIBOBS_REAL="/usr/lib/x86_64-linux-gnu/libobs.so.30"
DISTROAV_REAL="/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so"
OBS_FRONTEND_REAL="/usr/bin/obs"
# #756 (live wedge, 2026-07-15): libobs-opengl is a SEPARATE shared library
# (vendor/obs-studio/libobs-opengl/CMakeLists.txt: add_library(libobs-opengl SHARED)) from
# libobs.so.30 -- Fix B (the X11/EGL client-size cache, gl-x11-egl.c) lives entirely in it. It
# was NEVER named in this hot-swap before, so every prior "successful" swap silently left the
# PPA-stock (or an even older ad-hoc-deployed) libobs-opengl.so.30 in place while the
# GENLOCK_BUILD_SHA.txt marker claimed the current dev HEAD was fully deployed -- live-confirmed
# on imag-nb: the loaded file was dated 11 days stale, predating Fix B entirely, while a fresh
# wedge was captured blocking in the EXACT call chain Fix B was supposed to have eliminated.
LIBOBS_OPENGL_REAL="/usr/lib/x86_64-linux-gnu/libobs-opengl.so.30"

command -v jq >/dev/null 2>&1 || apt-get install -y jq >/dev/null 2>&1 || fail "jq install failed (needed for #460 manifest verify)"
# #822: readelf/nm (binutils) verify the hot-swapped binaries further down this step. A fresh
# Ubuntu install has NO binutils — install it, then preflight, so a missing TOOL can never be
# reported as a failed ABI check.
command -v readelf >/dev/null 2>&1 && command -v nm >/dev/null 2>&1 \
    || apt-get install -y binutils >/dev/null 2>&1 \
    || fail "#822: binutils install failed (readelf/nm needed to verify the genlock hot-swap)"
imag_require_tools readelf nm

# manifest_sha_for_path() and verify_file_sha() are defined at the TOP of this file (pure
# functions, no root/network needed) — see the source-guard block near the top for why: they are
# sourced + unit-tested directly against synthetic fixtures from
# tests/setup_imag_pure_functions.rs, which needs the destructive flow below to be skippable.

DEPLOYED_SHA=""
[ -f "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" ] && DEPLOYED_SHA="$(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt")"

if ! command -v gh >/dev/null 2>&1; then
    # Capture curl's output into a variable FIRST, then grep it — never `curl | grep | head -1`
    # as a live pipe (the same early-pipe-closure class the SONAME check below documents; a
    # materialised string can't truncate a still-writing upstream process under pipefail).
    GH_RELEASE_JSON="$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest)" \
        || fail "could not reach api.github.com/repos/cli/cli/releases/latest"
    # `|| true` is load-bearing: if BOTH greps find zero matches (e.g. the GitHub API response
    # shape ever changes), `grep` exits non-zero, and under pipefail that propagates to this bare
    # assignment — `set -e` would abort the script HERE, before the very next line's intended
    # `fail "no gh CLI ... asset found"` ever runs, with NO diagnostic at all. Same footgun class
    # (and same fix) as the LATEST_LOG lookup in the verify step — found in review.
    GH_DEB_URL="$(printf '%s' "$GH_RELEASE_JSON" \
        | grep -oE '"browser_download_url": *"[^"]*linux_amd64\.deb"' | grep -oE 'https[^"]*' | head -1 || true)"
    [ -n "$GH_DEB_URL" ] || fail "no gh CLI linux_amd64 .deb asset found on latest cli/cli release"
    curl -fsSL -o /tmp/gh-cli.deb "$GH_DEB_URL"
    DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/gh-cli.deb >/dev/null \
        || dpkg -i /tmp/gh-cli.deb || fail "gh CLI install failed"
fi
command -v gh >/dev/null 2>&1 || fail "gh CLI still missing after install attempt"
[ -n "${GH_TOKEN:-}" ] || fail "GH_TOKEN env required (repo-read scope) to download the private #460 CI artifact"

# GENLOCK_RUN_ID overrides run resolution (pin a specific build). Default: the latest SUCCESSFUL
# linux-genlock.yml run on the dev branch — the workflow triggers on push to dev, but ALSO carries
# a bare `workflow_dispatch:` with no branch restriction, so an unfiltered `gh run list` could pick
# up an experimental manual dispatch on some other branch as "latest successful" (`--branch main`
# is wrong for a different reason: linux-genlock.yml never runs ON main — `gh run list -w
# linux-genlock.yml -b main` is EMPTY, live-verified — a push-triggered run's headBranch stays
# "dev" even after the commit later merges to main). `--branch dev` is the correct filter: it
# matches every push-to-dev run AND any workflow_dispatch explicitly run against dev, while
# excluding a stray dispatch against a feature/experimental branch.
RUN_ID="${GENLOCK_RUN_ID:-}"
if [ -z "$RUN_ID" ]; then
    # `-q '.[0].databaseId'` on an EMPTY result list yields the literal 4-char text "null" (jq's
    # normal behaviour indexing a nonexistent array element), which would silently pass the
    # `[ -n "$RUN_ID" ]` guard below as if it were a real id. `// empty` collapses that case to a
    # genuinely empty string so the guard actually fires.
    RUN_ID="$(gh run list --repo "$GENLOCK_REPO" --workflow "$GENLOCK_WORKFLOW" --branch dev \
        --status success --limit 1 --json databaseId -q '.[0].databaseId // empty')"
    [ -n "$RUN_ID" ] || fail "no successful $GENLOCK_WORKFLOW run found on $GENLOCK_REPO (branch dev)"
fi

# Idempotency check BEFORE any download: `gh run view --json headSha` is a single cheap API call
# (the workflow's own GENLOCK_BUILD_SHA.txt is just `git rev-parse HEAD` at build time, i.e. the
# SAME commit as the run's headSha — no need to pull the ~90MB bundle just to learn this). Skips
# the whole download+verify+install cycle on every no-op re-run (setup-imag.sh may be re-run for
# unrelated reasons, and linux-genlock.yml fires on every dev push touching vendor/**).
NEW_SHA="$(gh run view "$RUN_ID" --repo "$GENLOCK_REPO" --json headSha -q .headSha)"
[ -n "$NEW_SHA" ] || fail "could not read headSha for run $RUN_ID"

NOOP_VALID=0
if [ "$DEPLOYED_SHA" = "$NEW_SHA" ] && [ -f "$LIBOBS_REAL" ] && [ -f "$DISTROAV_REAL" ] && [ -f "$OBS_FRONTEND_REAL" ] && [ -f "$LIBOBS_OPENGL_REAL" ]; then
    NOOP_VALID=1
    # #472 defense-in-depth (PR #471 review, deliberately deferred): the SHA-marker + file
    # *existence* check above trusts the on-disk marker without re-verifying the installed
    # BYTES. If the deployed libobs.so.30/distroav.so/bin-obs/libobs-opengl.so.30 were ever
    # silently reverted (e.g. an unattended `apt upgrade` slipping past the apt-mark hold, manual
    # tampering, OR -- the #756 live incident this libobs-opengl.so.30 re-verify closes -- a
    # PRIOR version of this very script that never named libobs-opengl.so.30 at all, so its bytes
    # never changed while the marker kept claiming full deployment), a no-op re-run would wrongly
    # report "already deployed" and skip re-swapping — the verify step's runtime log-verify would
    # still catch it eventually, but only after a confusing failure. Re-verify the CURRENTLY
    # INSTALLED files against the manifest cached locally on the LAST successful swap (pure local
    # sha256 compare, zero network cost, only paid on the already-rare re-run path) and fall
    # through to a fresh re-install on any mismatch.
    CACHED_MANIFEST="$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json"
    if [ -f "$CACHED_MANIFEST" ]; then
        WANT_LIBOBS_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'lib/x86_64-linux-gnu/libobs.so.30')"
        GOT_LIBOBS_SHA_CACHED="$(sha256sum "$LIBOBS_REAL" | awk '{print $1}')"
        WANT_DISTROAV_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
        GOT_DISTROAV_SHA_CACHED="$(sha256sum "$DISTROAV_REAL" | awk '{print $1}')"
        WANT_OBS_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'bin/obs')"
        GOT_OBS_SHA_CACHED="$(sha256sum "$OBS_FRONTEND_REAL" | awk '{print $1}')"
        WANT_LIBOBS_OPENGL_SHA_CACHED="$(manifest_sha_for_path "$CACHED_MANIFEST" 'lib/x86_64-linux-gnu/libobs-opengl.so.30')"
        GOT_LIBOBS_OPENGL_SHA_CACHED="$(sha256sum "$LIBOBS_OPENGL_REAL" | awk '{print $1}')"
        if [ "$GOT_LIBOBS_SHA_CACHED" != "$WANT_LIBOBS_SHA_CACHED" ] || [ "$GOT_DISTROAV_SHA_CACHED" != "$WANT_DISTROAV_SHA_CACHED" ] \
            || [ "$GOT_OBS_SHA_CACHED" != "$WANT_OBS_SHA_CACHED" ] || [ "$GOT_LIBOBS_OPENGL_SHA_CACHED" != "$WANT_LIBOBS_OPENGL_SHA_CACHED" ]; then
            echo "  WARNING: installed genlock bytes do not match the cached manifest — forcing re-install"
            NOOP_VALID=0
        fi
    else
        echo "  WARNING: no cached $CACHED_MANIFEST to re-verify against — trusting the deployed-SHA marker"
    fi
fi

if [ "$NOOP_VALID" -eq 1 ]; then
    echo "  genlock build $NEW_SHA already deployed — no-op (skipped download)"
else
    GENLOCK_TMP="$(mktemp -d)"
    trap 'rm -rf "$GENLOCK_TMP"' EXIT

    gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n obs-genlock-linux-x86_64 --dir "$GENLOCK_TMP/bundle" \
        || fail "download of obs-genlock-linux-x86_64 failed (run $RUN_ID)"
    gh run download "$RUN_ID" --repo "$GENLOCK_REPO" -n distroav-linux-fast-so --dir "$GENLOCK_TMP/fast" \
        || fail "download of distroav-linux-fast-so failed (run $RUN_ID)"

    [ -f "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt" ] || fail "bundle missing GENLOCK_BUILD_SHA.txt"
    BUNDLE_SHA="$(cat "$GENLOCK_TMP/bundle/GENLOCK_BUILD_SHA.txt")"
    [ "$BUNDLE_SHA" = "$NEW_SHA" ] || \
        fail "bundle build SHA ($BUNDLE_SHA) != resolved run headSha ($NEW_SHA) — refuse to deploy"
    [ -f "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt" ] || fail "distroav-linux-fast-so missing DISTROAV_BUILD_SHA.txt"
    FAST_SHA="$(cat "$GENLOCK_TMP/fast/DISTROAV_BUILD_SHA.txt")"
    [ "$FAST_SHA" = "$NEW_SHA" ] || \
        fail "distroav-linux-fast-so build SHA ($FAST_SHA) != resolved run headSha ($NEW_SHA) — mismatched artifacts, refuse to deploy"

    BUNDLE_LIBOBS="$GENLOCK_TMP/bundle/lib/x86_64-linux-gnu/libobs.so.30"
    FAST_DISTROAV="$GENLOCK_TMP/fast/distroav.so"
    BUNDLE_OBS="$GENLOCK_TMP/bundle/bin/obs"
    BUNDLE_LIBOBS_OPENGL="$GENLOCK_TMP/bundle/lib/x86_64-linux-gnu/libobs-opengl.so.30"
    [ -f "$FAST_DISTROAV" ] || fail "distroav-linux-fast-so missing distroav.so"
    [ -f "$BUNDLE_OBS" ] || fail "bundle missing bin/obs (#499: the frontend executable)"
    [ -f "$BUNDLE_LIBOBS_OPENGL" ] || fail "bundle missing lib/x86_64-linux-gnu/libobs-opengl.so.30 (#756: the X11/EGL client-size cache Fix B lives in this SEPARATE library)"
    [ -f "$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json" ] || fail "bundle missing BUNDLE_MANIFEST.json (#120)"
    MANIFEST="$GENLOCK_TMP/bundle/BUNDLE_MANIFEST.json"

    # #120 sha256 verify — libobs.so.30 against the bundle's own manifest (both on disk).
    # NOTE: the manifest lookup MUST be a bare `VAR="$(...)"` assignment on its OWN line, never
    # embedded as one of several arguments to another command — `set -e` only aborts the script on
    # a subshell's failing exit status when that substitution IS the entire simple command (a bare
    # assignment); a `fail()`/`exit` inside a function called as `cmd "$(func)" other-arg` only
    # kills the command-substitution SUBSHELL, and `cmd` still runs with that argument silently
    # empty — live-verified with a minimal repro during review of this exact PR.
    WANT_LIBOBS_SHA="$(manifest_sha_for_path "$MANIFEST" 'lib/x86_64-linux-gnu/libobs.so.30')"
    verify_file_sha "$BUNDLE_LIBOBS" "$WANT_LIBOBS_SHA" "bundle libobs.so.30"
    # #756: libobs-opengl.so.30 is a SEPARATE library from libobs.so.30 (its own CMake SHARED
    # target) that carries the Fix B X11/EGL client-size cache — it has its OWN manifest entry
    # (confirmed live: BUNDLE_MANIFEST.json already lists lib/x86_64-linux-gnu/libobs-opengl.so.30
    # even before this fix, because the bundle stage already copies the full OBS rundir — only the
    # DEPLOY side ever missed it), verify it the same way.
    WANT_LIBOBS_OPENGL_SHA="$(manifest_sha_for_path "$MANIFEST" 'lib/x86_64-linux-gnu/libobs-opengl.so.30')"
    verify_file_sha "$BUNDLE_LIBOBS_OPENGL" "$WANT_LIBOBS_OPENGL_SHA" "bundle libobs-opengl.so.30"
    # distroav.so has NO manifest of its own in the distroav-linux-fast-so artifact (it ships only
    # DISTROAV_BUILD_SHA.txt — a commit id, not a content hash) — cross-check it against the
    # BUNDLE's manifest entry for the SAME file instead (both jobs build distroav.so from the
    # identical commit; live-verified byte-identical across both jobs' outputs). Without this, the
    # one file actually plugged into OBS as the NDI-carrying plugin had ZERO integrity check.
    WANT_DISTROAV_SHA="$(manifest_sha_for_path "$MANIFEST" 'lib/x86_64-linux-gnu/obs-plugins/distroav.so')"
    verify_file_sha "$FAST_DISTROAV" "$WANT_DISTROAV_SHA" \
        "distroav-linux-fast-so distroav.so (cross-checked against bundle manifest)"
    # #499: bin/obs (the frontend executable) ships IN the same bundle as libobs.so.30 (both are
    # part of the staged OBS rundir), so it has its OWN manifest entry — verify it the same way.
    WANT_OBS_SHA="$(manifest_sha_for_path "$MANIFEST" 'bin/obs')"
    verify_file_sha "$BUNDLE_OBS" "$WANT_OBS_SHA" "bundle bin/obs (frontend)"

    mkdir -p "$GENLOCK_MARKER_DIR" "$GENLOCK_BACKUP_ROOT"
    # Exactly TWO bounded backup dirs (never accumulate one per re-run — #185's unbounded target/
    # growth is the cautionary precedent): a permanent STOCK backup made once on the very first
    # swap ever (the forever rollback-to-PPA-stock path) and a PREVIOUS backup overwritten on every
    # swap (quick rollback to the immediately-prior deployed build). #499: the frontend gets the
    # SAME stock/previous treatment as libobs/distroav — a bare file under $GENLOCK_BACKUP_ROOT
    # (live-verified path, hand-created 2026-07-04) rather than nested in stock-pre-genlock/, since
    # it is a standalone executable, not a plugin library pair.
    STOCK_BACKUP="$GENLOCK_BACKUP_ROOT/stock-pre-genlock"
    PREV_BACKUP="$GENLOCK_BACKUP_ROOT/previous"
    OBS_FRONTEND_STOCK_BACKUP="$GENLOCK_BACKUP_ROOT/obs.stock"
    if [ ! -d "$STOCK_BACKUP" ]; then
        mkdir -p "$STOCK_BACKUP"
        [ -f "$LIBOBS_REAL" ] && cp -a "$LIBOBS_REAL" "$STOCK_BACKUP/libobs.so.30"
        [ -f "$DISTROAV_REAL" ] && cp -a "$DISTROAV_REAL" "$STOCK_BACKUP/distroav.so"
        [ -f "$LIBOBS_OPENGL_REAL" ] && cp -a "$LIBOBS_OPENGL_REAL" "$STOCK_BACKUP/libobs-opengl.so.30"
    fi
    if [ ! -f "$OBS_FRONTEND_STOCK_BACKUP" ]; then
        [ -f "$OBS_FRONTEND_REAL" ] && cp -a "$OBS_FRONTEND_REAL" "$OBS_FRONTEND_STOCK_BACKUP"
    fi
    rm -rf "$PREV_BACKUP"
    mkdir -p "$PREV_BACKUP"
    [ -f "$LIBOBS_REAL" ] && cp -a "$LIBOBS_REAL" "$PREV_BACKUP/libobs.so.30"
    [ -f "$DISTROAV_REAL" ] && cp -a "$DISTROAV_REAL" "$PREV_BACKUP/distroav.so"
    [ -f "$OBS_FRONTEND_REAL" ] && cp -a "$OBS_FRONTEND_REAL" "$PREV_BACKUP/obs"
    [ -f "$LIBOBS_OPENGL_REAL" ] && cp -a "$LIBOBS_OPENGL_REAL" "$PREV_BACKUP/libobs-opengl.so.30"
    [ -n "$DEPLOYED_SHA" ] && echo "$DEPLOYED_SHA" > "$PREV_BACKUP/GENLOCK_BUILD_SHA.txt"

    install -m 0644 -o root -g root "$BUNDLE_LIBOBS" "$LIBOBS_REAL" || fail "libobs.so.30 hot-swap install failed"
    install -m 0644 -o root -g root "$FAST_DISTROAV" "$DISTROAV_REAL" || fail "distroav.so hot-swap install failed"
    install -m 0755 -o root -g root "$BUNDLE_OBS" "$OBS_FRONTEND_REAL" || fail "frontend obs hot-swap install failed (#499)"
    install -m 0644 -o root -g root "$BUNDLE_LIBOBS_OPENGL" "$LIBOBS_OPENGL_REAL" || fail "libobs-opengl.so.30 hot-swap install failed (#756)"
    ldconfig

    # SONAME sanity check (no `-q` on a piped external command under pipefail — same early-close
    # SIGPIPE footgun documented for the step-4 ldconfig check; read the full small output instead).
    readelf -d "$LIBOBS_REAL" 2>/dev/null | grep 'SONAME.*\[libobs\.so\.30\]' >/dev/null \
        || fail "post-swap libobs.so.30 SONAME check failed — refuse a mismatched ABI"
    # #756: same SONAME sanity check for the newly-swapped libobs-opengl.so.30.
    readelf -d "$LIBOBS_OPENGL_REAL" 2>/dev/null | grep 'SONAME.*\[libobs-opengl\.so\.30\]' >/dev/null \
        || fail "post-swap libobs-opengl.so.30 SONAME check failed — refuse a mismatched ABI (#756)"

    # #499 post-swap build-proof: the stock PPA frontend never references
    # obs_display_set_render_divisor (the #276/#278/#293 multiview render-budget decouple symbol)
    # — live-verified `nm -D -u` shows it as an UNDEFINED (U) symbol only on the genlock-built
    # frontend. A missing reference here means the wrong/stock binary got installed.
    # No `-q` on a piped external command under pipefail (same early-close SIGPIPE footgun as the
    # SONAME check above): `nm -D -u` on this binary emits ~2900 lines (~170KB, live-measured) and
    # the target symbol sits at line ~286 — `grep -q` would exit right after that early match,
    # SIGPIPE-ing `nm` mid-write, and under `set -euo pipefail` that would wrongly `fail()` a
    # CORRECT build. Read the full output instead.
    nm -D -u "$OBS_FRONTEND_REAL" 2>/dev/null | grep 'obs_display_set_render_divisor' >/dev/null \
        || fail "post-swap /usr/bin/obs does not reference obs_display_set_render_divisor — refuse a stock/wrong frontend binary (#499: multiview render-budget decouple would be missing)"

    # issue 789: write GENLOCK_BUILD_SHA.txt + DISTROAV_BUILD_SHA.txt + DEPLOYED_AT via the shared
    # marker helper (atomic temp-then-rename) so this provisioning path and deploy-genlock-fleet.sh
    # can never drift on HOW a box records its deployed build. The manifest copy stays here (the
    # helper writes only the three text markers).
    genlock_write_markers "$GENLOCK_MARKER_DIR" "$NEW_SHA" "$FAST_SHA"
    cp -a "$MANIFEST" "$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json"

    # Prevent an unattended `apt upgrade` from silently reverting libobs.so.30 back to PPA-stock
    # bytes behind the operator's back (dpkg still tracks obs-studio) — drift must be a deliberate
    # re-run of this script, never a background package update. distroav is NOT held: this rework
    # removed the stock DistroAV .deb install entirely, so `distroav` is no longer a dpkg package
    # at all (distroav.so is installed via a bare `install`, outside dpkg) — `apt upgrade` cannot
    # touch it regardless, holding it would be a no-op. Any hold failure is LOGGED, never silent —
    # it is a real (if soft) drift-protection guarantee, not a cosmetic nicety.
    if ! apt-mark hold obs-studio >/dev/null 2>&1; then
        echo "  WARNING: apt-mark hold obs-studio failed — an unattended apt upgrade could revert this deploy"
    fi

    # #785: a swap while OBS is already running (a re-run onto a NEWER build) must relaunch OBS to
    # pick up the new .so — but a bare SIGKILL here silently EATS the operator's unsaved UI state
    # (Show-in-Multiview flags, source transforms, dock geometry): SIGKILL never runs OBS's own
    # clean-shutdown save path (that is the whole class of bug #785 fixes). So stop OBS GRACEFULLY
    # first. When the supervised unit (imag-obs.service, issue 882) is active, route the stop through
    # `systemctl --user stop` — an external pkill/kill of the tracked process looks like a crash to
    # systemd and refights the stood-down issue-788 watchdog via Restart=on-failure (see
    # .claude/rules/imag-obs-supervision.md); the unit's ExecStop runs imag-obs-stop.sh's wmctrl-c ->
    # SIGTERM ladder, which is what actually persists the collection. Fall back to the installed
    # graceful helper, then an inline SIGTERM, for a box where the unit is not active. SIGKILL stays
    # ONLY as the LAST resort on a wedged process — the "Launch OBS" step's `if ! pgrep -x obs`
    # relaunch guard would otherwise SKIP relaunching a still-exiting OBS, silently leaving the OLD
    # build's process resident — so we still WAIT for actual death and fail loud if it won't die.
    if pgrep -x obs >/dev/null 2>&1; then
        HS_UID="$(id -u "$DESKTOP_USER")"
        HS_RUN="/run/user/${HS_UID}"
        if sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="$HS_RUN" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=${HS_RUN}/bus" \
                systemctl --user is-active --quiet imag-obs.service 2>/dev/null; then
            echo "  #785: OBS bezi pod imag-obs.service — graceful stop cez systemctl --user stop (uklada operatorov UI stav, ziadny Restart=on-failure fight)"
            sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="$HS_RUN" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=${HS_RUN}/bus" \
                systemctl --user stop imag-obs.service || true
        elif [ -x /usr/local/bin/imag-obs-stop.sh ]; then
            echo "  #785: graceful stop cez imag-obs-stop.sh (imag-obs.service nie je aktivny)"
            sudo -u "$DESKTOP_USER" DISPLAY=:0 XDG_RUNTIME_DIR="$HS_RUN" \
                DBUS_SESSION_BUS_ADDRESS="unix:path=${HS_RUN}/bus" \
                /usr/local/bin/imag-obs-stop.sh || true
        else
            echo "  #785: ziadny graceful helper — posielam SIGTERM (OBS uklada na svojom signal handleri)"
            pkill -TERM -x obs || true
        fi
        # graceful wait: OBS zapisuje kolekciu az pri ciste exite. Distinct loop form from the
        # SIGKILL-wait loop below, so the existing swap-kill test still anchors on that one.
        for _ in $(seq 1 25); do
            if ! pgrep -x obs >/dev/null 2>&1; then break; fi
            sleep 1
        done
        # SIGKILL only as the LAST resort on a wedged process — relaunch onto the new .so is unsafe
        # while the old obs is still resident.
        if pgrep -x obs >/dev/null 2>&1; then
            echo "  WARN: obs ignored the graceful stop for 25s — force-killing to load the new build (operator UI state may be lost this time)"
            pkill -9 -x obs || true
            for _ in $(seq 1 10); do
                pgrep -x obs >/dev/null 2>&1 || break
                sleep 1
            done
            pgrep -x obs >/dev/null 2>&1 && fail "old obs64 would not die after SIGKILL — cannot safely relaunch onto the new build"
        fi
    fi
    echo "  genlock build $NEW_SHA deployed (was: ${DEPLOYED_SHA:-none, PPA stock})"
fi

# =============================================================================
step 13 "OBS pre-seed: WebSocket :4455 no-auth + SaveProjectors + no first-run wizard"
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
        # #522: false — the openbox autostart hook (step 16) is now the SOLE, authoritative
        # opener of BOTH projectors (PROGRAM+MULTIVIEW) on every boot, self-healing monitor
        # assignment even if the box's monitor topology changes. Leaving OBS's own SaveProjectors
        # restore ON would double-open the projectors (OBS restoring its last-saved projector
        # state on top of the boot hook re-opening them fresh).
        printf '\n[BasicWindow]\nSaveProjectors=false\n' >> "$f"
    fi
    if ! grep -q '^CloseExistingProjectors=' "$f"; then
        # #756: true — OBSBasic::OpenProjector() (vendor/obs-studio/frontend/widgets/
        # OBSBasic_Projectors.cpp) only closes an EXISTING projector on the SAME monitor before
        # opening a new one when this key is true; it has NO compiled-in default (a missing key
        # reads as false), so a fresh profile leaves every OpenVideoMixProjector call (the #758
        # [0/8] preflight calls it via obs_phase2.py open-projectors on EVERY recording-e2e.sh
        # run, unconditionally) STACKING a brand-new Multiview/Program window on top of the
        # previous one instead of replacing it. Live-caught (2026-07-15): 7 stray Multiview + 7
        # stray Program projector windows had accumulated on imag (`DISPLAY=:0 wmctrl -l`) over
        # one afternoon of repeated preflight runs — seven independently-throttled Multiview
        # renders, each still costing real graphics-thread time, fully explains the render-health
        # preflight's intermittent sub-58fps failures even on a build whose #276/#278/#293
        # divisor mechanism is confirmed correctly compiled in (nm -D -u). Setting this makes
        # every future OpenVideoMixProjector call self-correct to exactly one window per monitor.
        #
        # INSERT into the EXISTING [BasicWindow] section (this SAME seed_ini() call already
        # created one two lines above, via the SaveProjectors seed) — NEVER append a duplicate
        # `[BasicWindow]` header. libobs's own util/config-file.c is a CUSTOM ini parser (NOT
        # Qt's QSettings) that keys sections in a uthash table by name; a SECOND `[BasicWindow]`
        # header does not merge into the first section, it adds a separate instance under the
        # same hash key, and every config_get_bool()/config_find_item() lookup only ever
        # resolves the FIRST-inserted section — a key seeded into a later duplicate section is
        # silently unreachable, and gets DROPPED ENTIRELY the next time OBS itself cleanly saves
        # the file (its own save only ever writes back what it had loaded). Live-caught
        # (2026-07-15): appending a duplicate section this way was applied to the already-running
        # imag box, then vanished completely after the next OBS restart's config save — 4
        # repeated open-projectors calls afterward still stacked 4 stray Multiview + 4 stray
        # Program windows, proving the naive append never took effect.
        if grep -q '^\[BasicWindow\]$' "$f"; then
            sed -i '0,/^\[BasicWindow\]$/s//[BasicWindow]\nCloseExistingProjectors=true/' "$f"
        else
            printf '\n[BasicWindow]\nCloseExistingProjectors=true\n' >> "$f"
        fi
    fi
    if ! grep -q '^LastVersion=' "$f"; then
        printf '\n[General]\nLastVersion=537001984\n' >> "$f"   # 32.2.0 — suppress first-run wizard
    fi
    if ! grep -q '^DockState=' "$f"; then
        # #791: OBS only persists [BasicWindow] geometry/DockState on a CLEAN exit -- imag-nb has
        # run 24/7 since first bring-up and has therefore never shed one (confirmed live on BOTH
        # .182 and .187: neither global.ini has ever carried these keys), so the operator's
        # requested Stats dock (visible + DOCKED) reverts to OBS's bare default layout on every
        # restart instead of surviving it.
        #
        # These two values are a REAL Qt QMainWindow::saveState()/saveGeometry() blob -- NOT
        # hand-authored (it is an internal, versioned Qt binary stream; hand-constructing one is
        # not safe). Generated ONCE, off-rig, on dev1 with a disposable stock OBS 30.0.2 kiosk
        # (Xvfb + openbox, a throwaway blank profile, zero scenes): Docks -> Stats, dragged into a
        # docked column after Controls, then a clean File > Exit so OBS ITSELF wrote these into its
        # own global.ini. Qt's dock-widget object names (scenesDock/sourcesDock/mixerDock/
        # transitionsDock/controlsDock/statsDock) have been stable across OBS's Qt frontend for
        # years, so this applies cleanly to the box's actual OBS 32.2.0 genlock build too --
        # Qt's restoreState() is best-effort per-widget-by-objectName, so even a future OBS build
        # that adds/removes an unrelated dock degrades to "some docks not repositioned", never a
        # crash or a corrupted layout.
        #
        # Seeded ONLY when DockState is missing (this same idempotent-seed convention every other
        # key in this function already uses) -- an operator who performs one real clean exit gets
        # THEIR OWN captured layout persisted from then on, and this seed never overwrites it.
        DOCKSTATE_GEOMETRY='AdnQywADAAAAAAF6AAAAogAAB38AAAOVAAABewAAALgAAAd+AAADkAAAAAAAAAAAB4AAAAF7AAAAuAAAB34AAAOQ'
        DOCKSTATE_BLOB='AAAA/wAAAAD9AAAAAQAAAAMAAAYEAAABAfwBAAAABvsAAAAUAHMAYwBlAG4AZQBzAEQAbwBjAGsBAAAAAAAAAKAAAACgAP////sAAAAWAHMAbwB1AHIAYwBlAHMARABvAGMAawEAAACkAAAAoAAAAKAA////+wAAABIAbQBpAHgAZQByAEQAbwBjAGsBAAABSAAAAN4AAADeAP////sAAAAeAHQAcgBhAG4AcwBpAHQAaQBvAG4AcwBEAG8AYwBrAQAAAioAAACgAAAAoAD////7AAAAGABjAG8AbgB0AHIAbwBsAHMARABvAGMAawEAAALOAAAAngAAAJ4A////+wAAABIAcwB0AGEAdABzAEQAbwBjAGsBAAADcAAAApQAAAKUAP///wAABgQAAAGbAAAABAAAAAQAAAAIAAAACPwAAAAA'
        if grep -q '^\[BasicWindow\]$' "$f"; then
            # Insert right after the FIRST [BasicWindow] header (never append a duplicate one --
            # see the CloseExistingProjectors comment above for why a second header is silently
            # unreachable). awk, not sed: the base64 blobs contain literal `/` characters, which
            # would collide with sed's own `s/.../.../ ` delimiter.
            awk -v geo="$DOCKSTATE_GEOMETRY" -v dock="$DOCKSTATE_BLOB" '
                { print }
                /^\[BasicWindow\]$/ && !done { print "geometry=" geo; print "DockState=" dock; done=1 }
            ' "$f" > "${f}.tmp791" && mv "${f}.tmp791" "$f"
        else
            printf '\n[BasicWindow]\ngeometry=%s\nDockState=%s\n' "$DOCKSTATE_GEOMETRY" "$DOCKSTATE_BLOB" >> "$f"
        fi
    fi
}
seed_ini "$OBS_CFG/global.ini"
seed_ini "$OBS_CFG/user.ini"
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$OBS_CFG"

# camera-box issue 1367 (ROZHODNUTÉ 5827497952, item 4): declare THIS box a genlock MIN-LATENCY box
# (the imag projection: every input at the 3 ms floor -- "najmensia mozna latencia"). The vendored
# libobs reads this marker once and caps a shallow source's latched depth at its pin-derived
# base + 1 here, REPORTING the cap (a genlock-shallow-lock WARNING + shallow_capped=1 on the audit
# line) instead of deepening an imag input. libobs has no other box identity (the imag and strih
# inputs share their names), so this file IS the identity. Idempotent.
install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER" "$USER_HOME/.camera-box"
install -o "$DESKTOP_USER" -g "$DESKTOP_USER" -m 0644 /dev/null "$USER_HOME/.camera-box/genlock-min-latency"
echo "  issue 1367: genlock min-latency box marker written ($USER_HOME/.camera-box/genlock-min-latency)"

# #791: install the CANONICAL 17-scene operator collection -- ONLY when this box genuinely has NO
# scene collection yet (a fresh profile). imag_scenes.py's own WS-based seed deliberately never
# creates "resolume imag" / "MW resolume imag" (its #785 OPERATOR-WINS carve-out -- those are
# hand-built scenes, not owned by the automated seeder), so a from-scratch box was silently
# missing them, Cam 7/MV Cam 7, and the whole correct scene ORDER until an operator built them by
# hand (the exact repeated-manual-work complaint this ticket exists to kill). The canonical file
# was captured live off the incumbent .182 (byte-identical to the already-live-restored .187,
# 2026-07-27/28) and is fetched the same way imag_scenes.py itself is (gh api against this repo's
# dev branch -- this script has no sibling repo checkout at runtime). Never overwrites an existing
# collection (operator-wins, same discipline as every seed_ini() key above).
SCENES_DIR="${USER_HOME}/.config/obs-studio/basic/scenes"
mkdir -p "$SCENES_DIR"
if ! ls "$SCENES_DIR"/*.json >/dev/null 2>&1; then
    gh api -H "Accept: application/vnd.github.raw" \
        "repos/${GENLOCK_REPO}/contents/scripts/imag-obs-scenes-canonical.json?ref=dev" \
        > "$SCENES_DIR/Untitled.json" \
        || fail "#791: could not fetch the canonical scene collection from ${GENLOCK_REPO} (dev)"
    chown "$DESKTOP_USER:$DESKTOP_USER" "$SCENES_DIR/Untitled.json"
    echo "  #791: canonical 17-scene collection installed (fresh box had none)"
else
    echo "  #791: existing scene collection found on disk -- leaving it untouched (operator wins)"
fi

# =============================================================================
step 14 "Desktop de-jitter (#485): mask background jitter sources + OBS ProcessPriority=High"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
# issue 1357: the crash-popup item (apport + whoopsie + the 26.04 apport coredump-hook template masked,
# systemd-coredump installed) rides with it -- on imag's 24.04 the hook template does not exist, so its
# mask is a harmless /dev/null link.
obs_box_dejitter "$DESKTOP_USER" imag "$OBS_CFG"

# =============================================================================
step 15 "Kiosk environment (#504): openbox+lightdm autologin, DM→lightdm, disable+purge GNOME"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_kiosk "$DESKTOP_USER" imag

# =============================================================================
step 16 "Reboot-durable openbox autostart (#522/#488) + Desktop icon"
# =============================================================================
# ROOT CAUSE (#522/#488): the box's real reboot-durable state lived ONLY as a hand-edited
# ~/.config/openbox/autostart on the box itself -- NOTHING in this script wrote it. This box runs
# lightdm+openbox directly (no GNOME/systemd session manager), which NEVER reads XDG
# ~/.config/autostart/*.desktop -- the old GNOME-style block this step used to write was dead code
# from day one. A reboot therefore silently regressed to whatever the hand file said, which
# wrongly primaried the HDMI projector output instead of the panel, and dropped the #507
# multiview-membership + projector self-heal. setup-imag.sh is now the SOLE writer of
# ~/.config/openbox/autostart -- the old `.config/autostart/obs.desktop` copy + sed-patch is gone.

# #526 self-heal: DELETE any leftover ~/.config/autostart/obs.desktop from a pre-#530 provision.
# CORRECTION to the note above: modern Ubuntu's systemd --user DOES launch XDG autostart --
# app-<id>@autostart.service fires for every ~/.config/autostart/*.desktop once
# graphical-session.target is up. So a leftover obs.desktop launches a SECOND obs ~30 s after
# boot (an "OBS is already running" modal stuck over the projector output -- live-hit 2026-07-05),
# on top of the one the openbox autostart below launches. Remove it so OBS starts exactly once.
rm -f "$USER_HOME/.config/autostart/obs.desktop"
rmdir "$USER_HOME/.config/autostart" 2>/dev/null || true

# Install imag_scenes.py + its websocket-client dependency onto the box at a FIXED path -- the
# boot hook below runs the seeder LOCALLY (127.0.0.1) on every boot, so it cannot depend on a
# hand-made venv or a checked-out copy of the repo (this script "is copied to the box standalone",
# per the step-12 comment above -- no sibling scripts/ files exist here at runtime).
obs_box_apt_update
DEBIAN_FRONTEND=noninteractive apt-get install -y python3-websocket >/dev/null \
    || fail "python3-websocket install failed — imag_scenes.py needs it for the boot-time self-heal (#522)"
SCN="/usr/local/bin/imag_scenes.py"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag_scenes.py?ref=dev" \
    > "$SCN" \
    || fail "could not fetch scripts/imag_scenes.py from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$SCN"
# #1156: imag_scenes.py imports the imag_record_encoder sibling module (the #1143 record-encoder
# lane). An imported sibling MUST ride the SAME on-box install list, fetched the SAME way -- or a
# deploy pushes the importer WITHOUT the imported module and every imag-obs-start.sh seed dies on
# ModuleNotFoundError, Restart-looping OBS (the 1737-restart / 8.5h incident this closes).
REC_ENC="/usr/local/bin/imag_record_encoder.py"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag_record_encoder.py?ref=dev" \
    > "$REC_ENC" \
    || fail "could not fetch scripts/imag_record_encoder.py from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$REC_ENC"
# issue 1218/1230: imag_scenes.py LAZILY imports the obs_phase2 sibling (the shared, issue-795-safe
# reenforce_ndi_name policy) inside enforce_ndi_names -- the ONE imag NDI-name heal point (issue 1230
# removed the idle policy; the #1158 name-healing stays). Ride it on the SAME on-box deploy list (same
# gh-api fetch + chmod, the #1156 sibling-install discipline) so the on-box --bootstrap heal gets the
# discoverability-gated name recovery.
# The import is LAZY + degrades to a direct set if this file is ever absent, so a stale box never
# crash-loops OBS -- but a fresh provision installs it for the gated path.
OBS_PHASE2="/usr/local/bin/obs_phase2.py"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/obs_phase2.py?ref=dev" \
    > "$OBS_PHASE2" \
    || fail "could not fetch scripts/obs_phase2.py from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$OBS_PHASE2"
# issue 1230: the #1218 active-cams state file (~/.config/camera-box/imag-active-cams) was REMOVED
# (owner ruling 2026-08-30: no idle policy). imag keeps all seven cameras named + alive, so there is
# no active set to persist. The obs_phase2 sibling install above STAYS — the #1158 name-healing in
# imag_scenes.enforce_ndi_names still uses the shared #795-safe obs_phase2.reenforce_ndi_name.


# #840: install the operator start/stop scripts onto the box too -- the openbox autostart below
# now launches OBS THROUGH imag-obs-start.sh (the SAME path the operator's right-click "Spustit
# OBS" menu entry uses) instead of a separate inline launch+seed, so a boot-durable box HARD
# DEPENDS on this file actually existing. Neither script was ever installed by this provisioner
# before #840 (both existed on THIS box only because a prior session hand-placed them) -- fetched
# here with the SAME gh-api pattern as imag_scenes.py above so a from-scratch reprovision (a
# future 3rd notebook) is never missing them.
OBS_START_SH="/usr/local/bin/imag-obs-start.sh"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag-obs-start.sh?ref=dev" \
    > "$OBS_START_SH" \
    || fail "could not fetch scripts/imag-obs-start.sh from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$OBS_START_SH"

OBS_STOP_SH="/usr/local/bin/imag-obs-stop.sh"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag-obs-stop.sh?ref=dev" \
    > "$OBS_STOP_SH" \
    || fail "could not fetch scripts/imag-obs-stop.sh from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$OBS_STOP_SH"

APP_DESKTOP=$(ls /usr/share/applications/com.obsproject.Studio.desktop 2>/dev/null || true)
mkdir -p "$USER_HOME/Desktop"
if [ -n "$APP_DESKTOP" ]; then
    # Desktop double-click icon only -- kept as a harmless convenience. Deliberately left
    # UNMODIFIED (plain `Exec=obs`, no taskset pin): a human double-clicking it is not the
    # reboot-durable boot path, which is pinned inside the openbox autostart script below instead.
    cp -f "$APP_DESKTOP" "$USER_HOME/Desktop/obs.desktop"
    chmod +x "$USER_HOME/Desktop/obs.desktop"
    sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        gio set "$USER_HOME/Desktop/obs.desktop" metadata::trusted true 2>/dev/null || true
fi
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/Desktop"

# The openbox autostart script IS the boot-durable authority (lightdm+openbox reads this file on
# every session start, unlike XDG .config/autostart/). Written with a QUOTED heredoc so every
# $VAR inside stays LITERAL text in the generated file -- $PANEL/$PROJ/$i are meant to be
# evaluated by openbox AT BOOT TIME, never expanded here at provisioning time. The __ISOLCPUS__
# placeholder (#840: now just an exported env var for imag-obs-start.sh, no longer a direct
# taskset argument) is substituted with the DERIVED isolated-CPU set right after, via sed -- a
# quoted heredoc cannot both keep $PANEL/$PROJ literal AND interpolate $IMAG_ISOLATED_CPUS in the
# same pass.
mkdir -p "$USER_HOME/.config/openbox"
cat > "$USER_HOME/.config/openbox/autostart" <<'AUTOSTART_EOF'
#!/bin/bash
# imag-nb OBS cutting kiosk boot — WRITTEN BY setup-imag.sh (#522/#488). Do not hand-edit.
# issue 1146: PROJ (HDMI-*, projector) = PRIMARY = the vsync anchor for the tear-free picom present
# (imag drives two 60Hz outputs on independent crystals; GL/scanout vsyncs only the primary CRTC, so
# the projector MUST be primary or its clock beats against the panel -> walking tear line). PANEL
# (DP-*/eDP-*, notebook) is a plain secondary; it still shows the OBS UI + Multiview because
# imag_scenes.py places projectors by connector TYPE (HDMI vs panel), never by the --primary flag,
# and OBS restores its own saved main-window geometry on the panel. This REVERSES the #522/#488
# panel-primary doctrine (its real regression was a lost self-heal, handled by imag-obs.service now).
sleep 1
PANEL=$(xrandr | awk '/ connected/ && $1 !~ /^HDMI/ {print $1; exit}')
PROJ=$(xrandr  | awk '/ connected/ && $1 ~  /^HDMI/ {print $1; exit}')
[ -n "$PANEL" ] && xrandr --output "$PANEL" --mode 1920x1080 --rate 60 2>/dev/null || true
if [ -n "$PROJ" ]; then
  { [ -n "$PANEL" ] && xrandr --output "$PROJ" --primary --mode 1920x1080 --rate 60 --left-of "$PANEL" 2>/dev/null; } \
    || xrandr --output "$PROJ" --primary --mode 1920x1080 --rate 60 2>/dev/null || true
fi
xset s off -dpms s noblank 2>/dev/null || true
# wall-fallback: resolume-imag still ako pozadie -- restart OBS nikdy neukaze ciernu stenu.
# Obrazok je OPERATORSKY ASSET (nie git) -- refresh: OBS-WS GetSourceScreenshot sceny
# 'resolume imag' do ~/Pictures/wall-fallback.png (viz #791 reprovision parity note).
[ -f "$HOME/Pictures/wall-fallback.png" ] && command -v feh >/dev/null && feh --no-fehbg --bg-fill "$HOME/Pictures/wall-fallback.png" 2>/dev/null || true
# Clear stale OBS crash sentinels BEFORE launch -- a hard/unclean reboot is EXACTLY the case OBS's
# own "Crash or unclean shutdown detected" modal fires on, which would hang the boot headless and
# :4455 would never come up (same fix as this script's own provisioning-time relaunch, step 17).
# Since #1195 the genlock build removes that modal at source (OBSApp.cpp checkForUncleanShutdown()
# auto-selects a NORMAL launch), so this clear is now belt-&-braces for a not-yet-redeployed binary;
# there is no OBS CLI flag that suppresses the check either way.
rm -rf "$HOME/.config/obs-studio/.sentinel"/* 2>/dev/null || true
# #522: strip any saved projectors from the scene-collection JSON so OBS restores NONE on load.
# OBS restores a scene collection's saved_projectors on launch INDEPENDENT of SaveProjectors=false
# (that flag only stops OBS from SAVING new ones on exit -- a pre-existing entry, from before the
# fix, is still restored). The autostart below is the SOLE projector opener, so a stale saved
# projector would stack a DUPLICATE on the HDMI output. Zero it every boot -> idempotent 1+1.
for f in "$HOME"/.config/obs-studio/basic/scenes/*.json; do
  [ -f "$f" ] && python3 -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d['saved_projectors']=[]; json.dump(d,open(p,'w'))" "$f" 2>/dev/null || true
done
# #840: boot runs OBS through the SAME operator path as the "Spustit OBS" right-click menu entry
# instead of a separate inline launch+wait+seed -- a second launch mechanism (a bare 30s WebSocket
# wait with NO obs-process-liveness check, its failure swallowed by `|| true`) is what let the boot
# path silently drop the projector self-heal while the manual path kept working: a real capture on
# 10.77.9.187 showed imag_scenes.py failing with ConnectionRefusedError at boot because the OLD
# inline wait loop timed out before OBS's WebSocket came up. That operator script's own 90s wait
# DOES check obs process liveness while polling.
# #884 (follow-up to issue 882): the call below now goes THROUGH the systemd unit rather than
# invoking the operator script directly -- the unit's own ExecStart still runs that identical
# script, so every WebSocket-wait/seed/projector guarantee above is unchanged, but the launch is
# now systemd-SUPERVISED (Restart=on-failure): a future segfault auto-restarts instead of leaving
# OBS dead until the next reboot, which is exactly what happened for ~70 minutes before issue 882
# added the unit. Enabling the unit without ALSO switching this call site would race two launchers
# at boot -- this line and the `enable --now` below (step 21) are a single, paired change.
export IMAG_ISOLATED_CPUS="__ISOLCPUS__"
systemctl --user start imag-obs.service || true
AUTOSTART_EOF
sed -i "s#__ISOLCPUS__#${IMAG_ISOLATED_CPUS}#" "$USER_HOME/.config/openbox/autostart"
chmod +x "$USER_HOME/.config/openbox/autostart"
chown "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/openbox/autostart"

# #785: PROVISION the openbox root menu (~/.config/openbox/menu.xml) instead of leaving it
# hand-placed on the live box -- the same provisioning-parity gap #840 closed for the operator
# start/stop scripts. openbox's stock rc.xml binds the desktop right-click to id="root-menu", so
# that is the menu id here. The GRACEFUL "Zastav OBS (korektne)" entry routes the operator's stop
# through imag-obs-stop.sh (installed above): that helper delegates to `systemctl --user stop
# imag-obs.service` when the supervised unit is active and otherwise runs the wmctrl-c -> SIGTERM
# ladder, either way giving OBS its clean-shutdown save path so the operator's UNSAVED UI state
# (Show-in-Multiview flags, source transforms, dock geometry) is persisted -- the whole point of
# this ticket. "Spustit OBS" launches via the UNIT (systemctl --user start), never a bare
# imag-obs-start.sh, so OBS stays inside the unit cgroup and supervised (#1015). The clean
# restart/shutdown entries let the operator power the box off cleanly FROM THE DESKTOP -- the
# hardware power key deliberately stays HandlePowerKey=ignore (#727: an accidental short press once
# shut the box down mid-event), so a desktop menu entry is the intended clean-poweroff path.
# issue 1357: the menu body is the SHARED kiosk menu (obs_box_openbox_menu_xml, the same printer
# setup-strih.sh uses) -- imag passes its label, its supervised unit and its graceful stop helper.
mkdir -p "$USER_HOME/.config/openbox"
obs_box_openbox_menu_xml "imag-nb" "systemctl --user start imag-obs.service" "/usr/local/bin/imag-obs-stop.sh" \
    > "$USER_HOME/.config/openbox/menu.xml" \
    || fail "#785: could not generate $USER_HOME/.config/openbox/menu.xml"
chown "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/openbox/menu.xml"

# =============================================================================
step 17 "Launch OBS on the desktop session (X11 :0)"
# =============================================================================
# Clear stale OBS crash sentinels BEFORE relaunching — mirrors the Windows
# launch-obs-genlock.sh convention (Remove-Item .sentinel\* before Start-Process obs64). On a
# genlock RE-deploy, the genlock hot-swap step force-restarts (SIGKILL) a running OBS to load the
# swapped libobs.so.30/distroav.so; without clearing the sentinel here first, the relaunched OBS
# pops the "Crash or unclean shutdown detected" recovery modal and hangs headless — WebSocket
# :4455 never comes up, and the verify step fails "not listening" even though the swap succeeded
# (hit live 2026-07-04 during a #463 re-deploy to imag-nb; recovered by hand).
rm -rf "${OBS_CFG}/.sentinel"/* 2>/dev/null || true
# #504: on a genuine from-scratch box, step 15's GNOME purge removes the gdm3 PACKAGE — and
# package removal invokes gdm3's own maintainer scripts, which stop the service as part of the
# removal REGARDLESS of the `disable` (no `--now`) call in step 15(d). If gdm3 owned the CURRENT
# :0 session, that teardown kills the X server this step would otherwise launch OBS into. Detect
# whether :0 is actually alive (its Unix socket exists) BEFORE attempting the launch — same "takes
# effect on the NEXT boot" convention as the kernel/CPU-isolation/NVIDIA-driver steps above: if :0
# died, OBS launches fresh via the lightdm+openbox autostart (step 16) after the next boot instead.
# On THIS (already-openbox) box :0 is alive throughout (openbox owns it, not gdm3) — unchanged path.
OBS_LAUNCHED_THIS_RUN=0
if [ -S /tmp/.X11-unix/X0 ]; then
    if ! pgrep -x obs >/dev/null; then
        # #483: pin this provisioning-time launch to the P-core block too, matching the autostart
        # entry above -- without taskset here, a provisioning launch (before the isolcpus grub change
        # is active, i.e. before the first post-provision reboot) still lands unpinned; after reboot,
        # an un-pinned `obs` would be STARVED onto the tiny cpu0,1,12-15 remainder once isolcpus takes
        # effect. `nice -n -5` was deliberately NOT added -- the desktop user lacks CAP_SYS_NICE
        # (live-confirmed, #483 comment).
        # shellcheck disable=SC2024  # redirect target is /tmp/obs-launch.log, world-writable and
        # written by root (this script runs as root); sudo -u drops privilege only for the `obs`
        # process itself, not for the redirect -- that is intentional here, not a bug.
        sudo -u "$DESKTOP_USER" DISPLAY=:0 DBUS_SESSION_BUS_ADDRESS="$UBUS" \
            nohup taskset -c "$IMAG_ISOLATED_CPUS" obs >/tmp/obs-launch.log 2>&1 &
        sleep 8
    fi
    pgrep -x obs >/dev/null || fail "OBS did not start (see /tmp/obs-launch.log)"
    OBS_LAUNCHED_THIS_RUN=1
else
    echo "  #504: DISPLAY=:0 is not alive this run (expected on a from-scratch box once step 15's \
GNOME/gdm3 purge tears down the old session) — OBS will auto-launch via the lightdm+openbox \
autostart (step 16) on the NEXT boot; this script does not reboot the box"
fi

# =============================================================================
step 18 "Verify: WebSocket :4455 + genlock render tick + DistroAV/NDI loaded"
# =============================================================================
if [ "$OBS_LAUNCHED_THIS_RUN" -eq 1 ]; then
    for i in $(seq 1 15); do
        if (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; then exec 3>&-; echo "  WS :4455 up"; break; fi
        [ "$i" -eq 15 ] && fail "OBS WebSocket :4455 not listening"
        sleep 2
    done

    # Genlock log verify — the Linux equivalent of scripts/launch-obs-genlock.sh's Windows
    # log-verify (#257 proof): the OBS log is the AUTHORITATIVE runtime signal a stock/wrong build
    # cannot fake. Same regex family as scripts/drift-guard.sh genlock_capability_from_log.
    OBS_LOG_DIR="$OBS_CFG/logs"
    # `|| true` is load-bearing: under `set -euo pipefail`, `ls` on a non-matching glob exits non-zero
    # even though `head` succeeds with empty output, and pipefail propagates that failure to the bare
    # assignment — `set -e` would abort the script HERE, before the very next line's intended
    # `fail "no OBS log found..."` ever runs (same convention already used for the desktop-icon step's
    # `APP_DESKTOP=$(ls ... 2>/dev/null || true)`).
    LATEST_LOG="$(ls -t "$OBS_LOG_DIR"/*.txt 2>/dev/null | head -1 || true)"
    [ -n "$LATEST_LOG" ] || fail "no OBS log found in $OBS_LOG_DIR — cannot verify the genlock build"
    LOG_TEXT="$(cat "$LATEST_LOG")"
    # #1184: LC_ALL=C grep -a -> byte-literal match, so invalid-UTF-8 bytes in the OBS log (DistroAV
    # mojibake) can never suppress a marker that IS present in a UTF-8 locale (same class as #1183).
    echo "$LOG_TEXT" | LC_ALL=C grep -aiE 'genlock:.*(render tick ENABLED|timestamp-aligned release|sub-frame jitter reserve|latency = [0-9]+ ms)' >/dev/null \
        || fail "OBS log shows NO genlock capability marker in '$LATEST_LOG' — NOT the genlock build (check the #460 hot-swap step)"
    echo "  genlock render tick ENABLED (#460 build proof)"
    if echo "$LOG_TEXT" | LC_ALL=C grep -ai '\[distroav\] plugin loaded' >/dev/null; then
        echo "  DistroAV plugin loaded"
    else
        echo "  WARNING: no '[distroav] plugin loaded' line yet (may log lazily on first NDI activation)"
    fi
    if echo "$LOG_TEXT" | LC_ALL=C grep -ai 'NDI library initialized' >/dev/null; then
        echo "  NDI runtime loaded"
    else
        echo "  WARNING: no 'NDI library initialized' line yet"
    fi
else
    echo "  #504: no live X session this run (see step 17) — skipping the WebSocket/genlock/NDI \
runtime verify; re-run this script (or just scripts/imag_scenes.py) after the next reboot to verify"
fi

# =============================================================================
step 19 "dev1 drift-guard SSH access (#541): install control-node public key"
# =============================================================================
# Idempotent: creates ~/.ssh with correct perms if absent, appends the key ONLY if not already
# present (never duplicates a line on re-run, never clobbers OTHER keys already authorized here).
SSH_DIR="${USER_HOME}/.ssh"
AUTH_KEYS="${SSH_DIR}/authorized_keys"
sudo -u "$DESKTOP_USER" mkdir -p "$SSH_DIR"
sudo -u "$DESKTOP_USER" chmod 700 "$SSH_DIR"
sudo -u "$DESKTOP_USER" touch "$AUTH_KEYS"
sudo -u "$DESKTOP_USER" chmod 600 "$AUTH_KEYS"
if grep -qF "$DEV1_DRIFTGUARD_PUBKEY_TYPE_BLOB" "$AUTH_KEYS" 2>/dev/null; then
    echo "  dev1 driftguard key already authorized"
else
    echo "$DEV1_DRIFTGUARD_PUBKEY" | sudo -u "$DESKTOP_USER" tee -a "$AUTH_KEYS" >/dev/null
    echo "  dev1 driftguard key appended to $AUTH_KEYS"
fi

# =============================================================================
step 20 "Companion Satellite for the connected Stream Deck (#731, server $COMPANION_HOST)"
# =============================================================================
# Headless install of Bitfocus Companion Satellite (bitfocus/companion-satellite) via the
# official installer script -- idempotent (re-running it re-syncs to the pinned stable build; the
# whole step is safe to re-run on every provisioning pass).
COMPANION_TARGET="$COMPANION_HOST"
if ! getent hosts "$COMPANION_HOST" >/dev/null 2>&1; then
    if [ -n "$COMPANION_HOST_IP" ]; then
        echo "  WARNING: '$COMPANION_HOST' does not resolve from this box -- using COMPANION_HOST_IP=$COMPANION_HOST_IP instead"
        COMPANION_TARGET="$COMPANION_HOST_IP"
    else
        echo "  WARNING: '$COMPANION_HOST' does not resolve from this box and no COMPANION_HOST_IP \
override was given -- configuring the satellite with the hostname anyway (it will retry DNS at \
connect time); set COMPANION_HOST_IP=<ip> and re-run this step if the connection never comes up"
    fi
fi

curl -fsSL https://raw.githubusercontent.com/bitfocus/companion-satellite/main/pi-image/install.sh \
    -o /tmp/companion-satellite-install.sh \
    || fail "could not download the companion-satellite installer"
bash /tmp/companion-satellite-install.sh \
    || fail "companion-satellite install.sh failed (see output above)"

# fixup-pi-config.js (satellite.service's ExecStartPre) re-reads /boot/satellite-config on EVERY
# service start, imports COMPANION_IP/REST_PORT into the persisted config, then resets this file
# back to a blank template -- so writing it fresh before every (re)start is the correct idempotent
# way to (re-)point the satellite, not a one-shot "only matters on first install" file.
cat > /boot/satellite-config <<CFGEOF
# Written by setup-imag.sh step 20 (#731) -- re-applied by fixup-pi-config.js on every satellite start
COMPANION_IP=$COMPANION_TARGET
REST_PORT=9999
CFGEOF
chmod 666 /boot/satellite-config

# #731 GOTCHA: systemd's hwdb classifies the Stream Deck (and similar surfaces) as
# ID_AV_PRODUCTION_CONTROLLER=1; /usr/lib/udev/rules.d/70-uaccess.rules then TAG+="uaccess"'s the
# hidraw node, which makes systemd-logind apply a per-SEAT ACL that OVERRIDES the plain
# GROUP=satellite/MODE=660 the installer's own 50-satellite.rules sets -- the ACL's `group::---`
# leaves the headless "satellite" service user unable to open the device ("cannot open device with
# path /dev/hidrawN") even though the group ownership is correct. Strip the uaccess tag for these
# devices (numbered AFTER 70- on purpose -- tag removal must run after the tag is added) so the
# plain group-based permission is the one that actually applies. Live-confirmed 2026-07-13: without
# this, satellite logs an "Open ... failed: cannot open device" error for the Stream Deck; with it,
# the very next start logs its firmware version and the REST /api/surfaces endpoint lists it.
cat > /etc/udev/rules.d/99-companion-satellite-no-uaccess.rules <<'UDEVEOF'
# #731: keep AV-production-controller HID surfaces (Stream Deck etc.) on plain group permissions
# so the headless "satellite" service user can open them -- see setup-imag.sh step 20 for the why.
SUBSYSTEM=="hidraw", ENV{ID_AV_PRODUCTION_CONTROLLER}=="1", TAG-="uaccess"
UDEVEOF
udevadm control --reload-rules
udevadm trigger --subsystem-match=hidraw
# Clear any ACL a PRIOR run/boot already applied before this rule existed (a fresh hotplug/reboot
# would never gain one going forward, but an already-plugged device needs an explicit reset now).
for dev in /sys/class/hidraw/hidraw*; do
    [ -e "$dev" ] || continue
    if udevadm info "/dev/$(basename "$dev")" 2>/dev/null | grep -q 'ID_AV_PRODUCTION_CONTROLLER=1'; then
        setfacl -b "/dev/$(basename "$dev")" 2>/dev/null || true
    fi
done

systemctl enable satellite >/dev/null 2>&1 || fail "could not enable the satellite systemd service"
systemctl restart satellite || fail "could not (re)start the satellite systemd service"
for i in $(seq 1 10); do
    systemctl is-active --quiet satellite && break
    [ "$i" -eq 10 ] && fail "satellite service not active after start (journalctl -u satellite for detail)"
    sleep 1
done
echo "  satellite service active + enabled at boot, configured for $COMPANION_TARGET (REST :9999)"

# =============================================================================
step 21 "OBS supervision unit + wallpaper-refresh provisioning + core dumps (#882)"
# =============================================================================
# #882: imag-obs-start.sh/imag-obs-stop.sh were already fetched in step 16 above -- this step adds
# (a) systemd-coredump, so LimitCORE=infinity's captured cores actually land somewhere inspectable
# (ulimit -c was 0 and kernel.core_pattern was a bare non-piped "core" before this, leaving the
# 2026-07-30 segfault with nothing debuggable); (b) imag-wallpaper-refresh.sh + its timer, which
# was hand-installed on the live box only before this ticket -- the exact same "never actually
# provisioned" gap #840 already found for imag-obs-start.sh/stop.sh (the SCREENSHOT-refresh
# behavior is unchanged from before #882 -- the obs-down ALERT is fired from a separate, DEV1-side
# watchdog, scripts/imag-obs-alert-watchdog.sh, since imag-nb has no ~/devel/airuleset checkout or
# Discord credentials to fire it itself -- confirmed live); (c) the imag-obs.service unit file
# itself, installed AND enabled+started (issue 884, follow-up to this ticket): the boot-time
# openbox autostart heredoc above now launches OBS THROUGH this unit rather than calling the
# operator script directly, so enabling it here no longer races two launchers -- it is the ONLY
# launcher now. Restart=on-failure gives OBS supervised auto-restart on a real segfault that a
# bare script call never had (the 2026-07-30 outage this whole feature exists to prevent
# recurring).

DEBIAN_FRONTEND=noninteractive apt-get install -y systemd-coredump >/dev/null \
    || fail "systemd-coredump install failed -- needed so LimitCORE=infinity's captured cores land somewhere inspectable (#882)"

WALLPAPER_SH="/usr/local/bin/imag-wallpaper-refresh.sh"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag-wallpaper-refresh.sh?ref=dev" \
    > "$WALLPAPER_SH" \
    || fail "could not fetch scripts/imag-wallpaper-refresh.sh from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$WALLPAPER_SH"

sudo -u "$DESKTOP_USER" mkdir -p "$USER_HOME/.config/systemd/user"
for unit in imag-obs.service imag-wallpaper-refresh.service imag-wallpaper-refresh.timer; do
    gh api -H "Accept: application/vnd.github.raw" \
        "repos/${GENLOCK_REPO}/contents/systemd/${unit}?ref=dev" \
        > "$USER_HOME/.config/systemd/user/${unit}" \
        || fail "could not fetch systemd/${unit} from ${GENLOCK_REPO} (dev) via gh api"
done
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/systemd"

UID_DESKTOP="$(id -u "$DESKTOP_USER")"
# #1182: a from-scratch box provisioned detached has NO user bus yet (see user_bus_alive above), so
# these `systemctl --user` calls would die "Failed to connect to bus: Connection refused" and their
# `|| fail` would abort the whole run at step 21 (never reaching steps 22-27). Gate on the bus,
# mirroring step 17's dead-:0 degrade -- and complete the ENABLE bus-free on the deferred path.
if user_bus_alive; then
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${UID_DESKTOP}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user daemon-reload || fail "systemctl --user daemon-reload failed"
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${UID_DESKTOP}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user enable --now imag-wallpaper-refresh.timer \
        || echo "  WARNING: could not enable imag-wallpaper-refresh.timer -- the wall-fallback screenshot will go stale"
    echo "  imag-wallpaper-refresh.timer enabled (wall-fallback screenshot refresh every 5 min; the obs-down Discord alert is a SEPARATE dev1-side watchdog, scripts/imag-obs-alert-watchdog.sh)"
    # issue 884: the autostart heredoc above now calls through this unit instead of the operator
    # script directly, so enable+start is safe here -- see the step header comment for why.
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${UID_DESKTOP}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user enable --now imag-obs.service \
        || fail "systemctl --user enable --now imag-obs.service failed"
    echo "  imag-obs.service enabled + started (OBS boot launch is now systemd-supervised, Restart=on-failure; issue 884)"
else
    # #1182: (fresh box) no user bus this run -- the desktop user's systemd manager only comes up on
    # the FIRST kiosk boot (lightdm autologin), exactly like step 17's dead-:0 case. DEFER the `--now`
    # START (it needs X, which the openbox autostart provides on that boot) but complete the ENABLE
    # right now BUS-FREE: create the units' wants-symlinks by hand -- functionally equivalent to what
    # `systemctl --user enable` writes (an ABSOLUTE symlink rather than the relative `../<unit>` it
    # writes, but the manager resolves it identically and is-enabled reads 'enabled' either way) and
    # the same wants-symlink the incumbent box already carries on every boot -- so verify-imag.sh
    # check (t) reads is-enabled=enabled after ONE reboot with no
    # re-run. The unit FILES were written to disk above; the fresh boot's user manager reads these
    # symlinks at startup (no daemon-reload needed then). The target dirs mirror each unit's own
    # [Install] WantedBy (systemd/imag-obs.service = graphical-session.target, the timer = timers.target).
    sudo -u "$DESKTOP_USER" mkdir -p \
        "$USER_HOME/.config/systemd/user/graphical-session.target.wants" \
        "$USER_HOME/.config/systemd/user/timers.target.wants"
    sudo -u "$DESKTOP_USER" ln -sf "$USER_HOME/.config/systemd/user/imag-obs.service" \
        "$USER_HOME/.config/systemd/user/graphical-session.target.wants/imag-obs.service"
    sudo -u "$DESKTOP_USER" ln -sf "$USER_HOME/.config/systemd/user/imag-wallpaper-refresh.timer" \
        "$USER_HOME/.config/systemd/user/timers.target.wants/imag-wallpaper-refresh.timer"
    echo "  #1182: (fresh box) no user bus this run -- imag-obs.service + imag-wallpaper-refresh.timer daemon-reload/START deferred to first kiosk boot; both ENABLED bus-free (wants-symlinks on disk), OBS launches via the openbox autostart on the next boot"
fi

# =============================================================================
step 22 "Power/thermal envelope (#1040): purge thermald + pin MMIO RAPL PL1 + slpc, supervised by a loud guard"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
# imag installs the on-box envelope scripts the same gh-api way as every other file this script fetches.
imag_fetch_repo_file() {  # imag_fetch_repo_file REPO_RELPATH DEST
    gh api -H "Accept: application/vnd.github.raw" "repos/${GENLOCK_REPO}/contents/$1?ref=dev" > "$2"
}
# PL1 is overridable per run (IMAG_PL1_W=30 sudo -E ./setup-imag.sh ...); 45 W = the #1162 re-baseline
# for the i7-13620H replacement notebook.
obs_box_power_envelope "${IMAG_PL1_W:-45}" imag_fetch_repo_file

step 23 "RemoteOS MCP control-channel agent (#858): provision via the canonical zbynekdrlik/remoteos-mcp installer"
# The linux-imag-nb MCP surface (:8092) is served by the SEPARATE zbynekdrlik/remoteos-mcp project
# (ops skill #555). camera-box does NOT re-implement or re-pin the agent -- it INVOKES that project's
# own canonical install-linux.sh (pip-git install + config.json + systemd unit + enable/start),
# matching the standing "use the installer, never a bare pip command" discipline.
#
# Auth-key handling (security-boundary): the --auth-key is a full-shell-RCE bearer token bound to
# 0.0.0.0:8092, so it NEVER lands in this repo. Two paths, mirroring this script's env-secret
# convention (CAM_PW/GH_TOKEN):
#   - REMOTEOS_MCP_AUTH_KEY set  -> pre-seed /etc/remoteos-mcp/config.json (chmod 600) so the
#     installer REUSES that known key and dev1's gitignored .mcp.json keeps matching a freshly
#     hardware'd box (fully closes #858: a working MCP surface, not just a running agent).
#   - unset                      -> the installer generates a fresh key ON the box; we print that
#     dev1's .mcp.json linux-imag-nb entry must be updated to match (fail-safe fallback).
REMOTEOS_MCP_INSTALLER_URL="${REMOTEOS_MCP_INSTALLER_URL:-https://raw.githubusercontent.com/zbynekdrlik/remoteos-mcp/master/install-linux.sh}"
REMOTEOS_MCP_CONFIG="/etc/remoteos-mcp/config.json"
# curl+ca-certificates are already ensured fail-loud up-front (the cam5/#450 preflight above).
if [ -n "${REMOTEOS_MCP_AUTH_KEY:-}" ]; then
    # Reject any shell/JSON-special char: the installer generates [A-Za-z0-9]{32} keys, and a
    # non-alphanumeric value in the unquoted heredoc below would break the JSON (the installer
    # then silently discards it and generates a DIFFERENT key -- dev1's .mcp.json breaks while
    # the is-active gate still passes) or run command substitution. Fail loud instead.
    case "$REMOTEOS_MCP_AUTH_KEY" in
        *[!A-Za-z0-9]*) fail "#858: REMOTEOS_MCP_AUTH_KEY must be alphanumeric [A-Za-z0-9] (installer key charset); refusing to write it unsafely" ;;
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
    echo "  #858: pre-seeded $REMOTEOS_MCP_CONFIG from REMOTEOS_MCP_AUTH_KEY (installer reuses it; dev1 .mcp.json stays valid)"
else
    echo "  #858: REMOTEOS_MCP_AUTH_KEY unset -- the installer will generate a fresh on-box key; update dev1's .mcp.json linux-imag-nb entry to match"
fi
REMOTEOS_MCP_INSTALLER_TMP="$(mktemp /tmp/remoteos-mcp-install-linux.XXXXXX.sh)"
curl -fsSL "$REMOTEOS_MCP_INSTALLER_URL" -o "$REMOTEOS_MCP_INSTALLER_TMP" \
    || fail "#858: cannot fetch remoteos-mcp installer from $REMOTEOS_MCP_INSTALLER_URL"
bash "$REMOTEOS_MCP_INSTALLER_TMP" \
    || fail "#858: canonical remoteos-mcp install-linux.sh failed"
rm -f "$REMOTEOS_MCP_INSTALLER_TMP"
systemctl is-active --quiet remoteos-mcp \
    || fail "#858: remoteos-mcp.service not active after install -- the linux-imag-nb MCP surface would be dead"
echo "  #858: remoteos-mcp agent active on :8092 (linux-imag-nb MCP surface provisioned)"

# =============================================================================
step 24 "imag OBS render watchdog (#764): install alarm-only watchdog + unit, LEFT DISABLED (issue 791)"
# =============================================================================
# The on-imag OBS render watchdog (scripts/imag-obs-watchdog.py -> /usr/local/sbin/) is ALARM-ONLY
# and NEVER reboots (issue 778): detect wedge -> snapshot + Discord alarm (via the dev1-side relay),
# then WAIT for a human; it only relaunches a genuinely DEAD OBS process (recovery, not a reboot).
# Both the script AND its systemd unit used to exist on the box ONLY as a hand-install -- the exact
# "provisioning gap hidden by a hand patch" shape issue 840 found for imag-obs-start.sh/imag-obs-stop.sh,
# so a fresh reprovision had NO watchdog at all and verify-imag.sh check (p) would fail. Fetched here
# from the repo (single source of truth) the SAME gh-api way as imag-obs.service in step 21.
#
# INSTALLED-BUT-DISABLED per the issue-791 agreed model: OBS keep-alive is imag-obs.service's job
# (Restart=on-failure, issue 882) + a dev1-side alert watchdog; this on-imag watchdog is a dormant,
# ready-to-enable-after-issue-788 artifact. We install + daemon-reload but DO NOT enable it, and
# explicitly `disable` so `systemctl is-enabled imag-obs-watchdog` reports exactly "disabled" (not
# "static"), which is what verify-imag.sh check (p) requires. NEVER enable it here -- that would race
# imag-obs.service on an OBS relaunch (two relaunchers), the reason it stays disabled until issue 788.
mkdir -p /usr/local/sbin
WATCHDOG_PY="/usr/local/sbin/imag-obs-watchdog.py"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/scripts/imag-obs-watchdog.py?ref=dev" \
    > "$WATCHDOG_PY" \
    || fail "could not fetch scripts/imag-obs-watchdog.py from ${GENLOCK_REPO} (dev) via gh api"
chmod 755 "$WATCHDOG_PY"

gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/systemd/imag-obs-watchdog.service?ref=dev" \
    > /etc/systemd/system/imag-obs-watchdog.service \
    || fail "could not fetch systemd/imag-obs-watchdog.service from ${GENLOCK_REPO} (dev) via gh api"

systemctl daemon-reload
# Leave DISABLED (issue 791). `disable` is idempotent on a never-enabled unit and makes the state
# deterministic even if a prior box had it enabled from an old hand-deploy.
systemctl disable imag-obs-watchdog >/dev/null 2>&1 || true
echo "  #764: imag-obs-watchdog installed (script + unit) and LEFT DISABLED (alarm-only; enable only after issue 788 fix)"

# =============================================================================
step 25 "Touchpad usability (#779): tap-to-click + natural scroll + gentler scroll (reprovision-durable)"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_touchpad imag

# =============================================================================
step 26 "Full max-performance persistence (issue 756/#791): EPP/turbo/platform-profile/runtime-PM via imag-maxperf.service + hotplug udev rule"
# =============================================================================
# issue 1357: this step's body moved VERBATIM into the shared OBS-box appliance baseline
# (scripts/lib/obs-box-baseline.sh) -- setup-strih.sh runs the SAME function, so the two boxes can
# never diverge again. Box facts are its arguments; with these imag values it writes exactly what
# this step always wrote.
obs_box_maxperf_persistence imag

# =============================================================================
step 27 "picom vsync compositor (issue 1146): tear-free HDMI-projector present + enable"
# =============================================================================
# ROOT CAUSE (issue 1146): imag drives TWO 60Hz outputs (eDP panel + HDMI projector) on independent
# crystals. GL/scanout presentation vsyncs to only ONE CRTC (the primary), so a compositor-free
# direct scanout (the #841 doctrine) does not guarantee the PROJECTOR is the sync target -> the two
# clocks beat -> a walking tear line on the projector, intermittently ("raz dobre, raz zle"). The
# live fix (deployed by hand 2026-08-20, folded in here for reproducibility): a picom v10 vsync
# compositor (glx, unredir-if-possible=false so the fullscreen Program projector stays composited,
# zero eye-candy) ANCHORED on the projector by making HDMI the xrandr primary (step 16 above). Cost
# is <=1 frame of projector display latency; the NDI 3ms mandate is untouched (picom composites only
# the local projector present, never the NDI receive path). The inert 20-tearfree.conf on the live
# box is deliberately NOT provisioned here -- Option "TearFree" is a proven-dead option on this
# modesetting build (#841), and the picom compositor is the real mechanism.
#
# ENABLE-ONLY (never --now): this provisioner defers taking effect to the box's next graphical
# session, exactly like the touchpad/maxperf steps -- verify-imag.sh check (z) is the post-reboot
# acceptance gate that proves picom actually came up. `systemctl --user enable` creates the
# graphical-session.target.wants/picom.service symlink that scripts/lib/imag-display-path.sh reads.
DEBIAN_FRONTEND=noninteractive apt-get install -y picom >/dev/null \
    || fail "issue 1146: picom install failed -- the vsync compositor is the tear-free HDMI-projector present; without it the dual-output beat returns"

# picom.conf -- byte-faithful to the live box (glx vsync, keep the fullscreen projector composited,
# zero eye-candy). QUOTED heredoc: the body is literal (no shell expansion).
sudo -u "$DESKTOP_USER" mkdir -p "$USER_HOME/.config/picom"
cat > "$USER_HOME/.config/picom/picom.conf" <<'PICOM_CONF_EOF'
# camera-box #1130 tearing fix (2026-08-20): the ONLY job of this compositor is a vsynced
# present of the OBS projectors (modesetting TearFree is not available in this xorg build).
backend = "glx";
vsync = true;
# NEVER unredirect fullscreen — the fullscreen Program projector is exactly the window
# that must stay composited/vsynced, or the tearing returns.
unredir-if-possible = false;
# zero eye-candy: no shadows/fading/blur — pure sync, minimal latency/CPU.
shadow = false;
fading = false;
blur-background = false;
PICOM_CONF_EOF

# picom.service (user systemd unit) -- byte-faithful to the live box. QUOTED heredoc: %h stays a
# literal systemd specifier (never shell-expanded).
sudo -u "$DESKTOP_USER" mkdir -p "$USER_HOME/.config/systemd/user"
cat > "$USER_HOME/.config/systemd/user/picom.service" <<'PICOM_SVC_EOF'
[Unit]
Description=picom vsync compositor (tear-free OBS projectors, camera-box issue 1130)
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Environment=DISPLAY=:0
ExecStart=/usr/bin/picom --config %h/.config/picom/picom.conf
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
PICOM_SVC_EOF
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/picom" "$USER_HOME/.config/systemd"

PICOM_UID="$(id -u "$DESKTOP_USER")"
# #1182: same bus-liveness gate as step 21. On a from-scratch box this daemon-reload's `|| fail`
# was the NEXT abort point once step 21 no longer aborts; defer it (picom must stay DORMANT, so no
# wants-symlink is ever created for it -- the belt-and-braces rm below and the unit-on-disk keep it
# present-but-disabled at the next boot).
if user_bus_alive; then
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${PICOM_UID}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user daemon-reload || fail "issue 1146: systemctl --user daemon-reload failed before enabling picom.service"
    # issue 1146 REVERT (live-measured 2026-08-20): the unit is provisioned DORMANT — installed +
    # configured but DISABLED. Running the compositor cost 21.57% OBS render skips on the 25W power
    # envelope (real dropped output frames chain-wide), strictly worse than the display-only tearing
    # it cured; the tear-free direction is the OBS projector's own vsync / single-display (issue 1146 /
    # issue 1147). The disable is deterministic: `systemctl --user disable` plus a belt-and-braces
    # removal of the on-disk wants symlink (the exact artifact imag-display-path.sh reads), and it
    # never `--now`-stops anything live (enable-only convention applies to the disable direction too).
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${PICOM_UID}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user disable picom.service 2>/dev/null || true
else
    # #1182: (fresh box) no user bus this run -- picom's daemon-reload/disable is deferred to first
    # kiosk boot. Nothing else is needed: picom must stay DORMANT, so NO wants-symlink is created
    # for it (the opposite of imag-obs.service in step 21), the unit file is already on disk, and the
    # belt-and-braces rm below clears any stale wants-symlink -- the fresh user manager finds picom
    # present-but-disabled at the next boot, exactly the intended dormant state.
    echo "  #1182: (fresh box) no user bus this run -- picom daemon-reload/disable deferred to first kiosk boot; picom stays DORMANT (unit on disk, no wants-symlink created)"
fi
rm -f "$USER_HOME/.config/systemd/user/graphical-session.target.wants/picom.service"
echo "  issue 1146 revert: picom provisioned DORMANT (installed+configured, unit disabled — render budget stays with OBS; HDMI stays xrandr primary via step 16)"

# =============================================================================
step 28 "imag :8899 bundle-state server (issue 1299): install + ENABLE-ONLY so the dev1 genlock-lock watchdog covers imag"
# =============================================================================
# imag is listed in scripts/lib/obs-fleet.sh's genlock-lock facet fleet (strih stream imag
# resolume), but had NO :8899 server, so scripts/genlock-lock-alert-watchdog.sh SKIPped it every
# pass (":8899 not fetchable") and an imag genlock LOCK loss was never paged. This installs the
# canonical scripts/bundle-state-server.py under a --user unit; its Windows-only identity gathers
# degrade to absent facets on Linux (its IS_WINDOWS gate, issue 1299), so on imag it serves
# genlock_lock, genlock_build_sha, obs_version, audio_ts_lag_* and record-dir-stats. The three
# sibling files (the server + the bundle_state_gather / obs_phase2 modules it imports) install
# together under /opt/camera-box so the server's sibling imports resolve.
#
# ENABLE-ONLY (never --now): this provisioner never live-starts the server -- the supervisor starts
# it once after the fleet genlock deploy and runs the verify-imag.sh :8899 check (per
# systemd/genlock-lock-alert-watchdog.README.md + .claude/rules/provisioning-scripts.md).
mkdir -p /opt/camera-box
for f in bundle-state-server.py bundle_state_gather.py obs_phase2.py; do
    gh api -H "Accept: application/vnd.github.raw" \
        "repos/${GENLOCK_REPO}/contents/scripts/${f}?ref=dev" \
        > "/opt/camera-box/${f}" \
        || fail "issue 1299: could not fetch scripts/${f} from ${GENLOCK_REPO} (dev) via gh api"
done
chmod 0644 /opt/camera-box/*.py

sudo -u "$DESKTOP_USER" mkdir -p "$USER_HOME/.config/systemd/user"
gh api -H "Accept: application/vnd.github.raw" \
    "repos/${GENLOCK_REPO}/contents/systemd/imag-bundle-state-server.service?ref=dev" \
    > "$USER_HOME/.config/systemd/user/imag-bundle-state-server.service" \
    || fail "issue 1299: could not fetch systemd/imag-bundle-state-server.service from ${GENLOCK_REPO} (dev) via gh api"
chown -R "$DESKTOP_USER:$DESKTOP_USER" "$USER_HOME/.config/systemd"

BSS_UID="$(id -u "$DESKTOP_USER")"
# #1182: same bus-liveness gate as step 21 -- a from-scratch box has no user bus this run, so gate
# the `systemctl --user` calls and complete the ENABLE bus-free on the deferred path.
if user_bus_alive; then
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${BSS_UID}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user daemon-reload || fail "issue 1299: systemctl --user daemon-reload failed before enabling imag-bundle-state-server.service"
    # ENABLE-ONLY (no --now): creates the graphical-session.target.wants symlink so it starts on the
    # next graphical session; the supervisor starts it now to validate :8899.
    sudo -u "$DESKTOP_USER" XDG_RUNTIME_DIR="/run/user/${BSS_UID}" DBUS_SESSION_BUS_ADDRESS="$UBUS" \
        systemctl --user enable imag-bundle-state-server.service \
        || fail "issue 1299: systemctl --user enable imag-bundle-state-server.service failed"
    echo "  imag-bundle-state-server.service ENABLED (not started -- the supervisor starts it once; verify-imag.sh check (ba) gates :8899)"
else
    # #1182: (fresh box) no user bus this run -- complete the ENABLE bus-free by hand-writing the
    # wants-symlink (the unit is WantedBy=graphical-session.target), exactly like step 21's deferred
    # imag-obs.service path. The START stays deferred (enable-only convention); the fresh user manager
    # reads this symlink at the next boot.
    sudo -u "$DESKTOP_USER" mkdir -p "$USER_HOME/.config/systemd/user/graphical-session.target.wants"
    sudo -u "$DESKTOP_USER" ln -sf "$USER_HOME/.config/systemd/user/imag-bundle-state-server.service" \
        "$USER_HOME/.config/systemd/user/graphical-session.target.wants/imag-bundle-state-server.service"
    echo "  #1182: (fresh box) no user bus this run -- imag-bundle-state-server.service ENABLED bus-free (wants-symlink on disk); the supervisor starts it once"
fi

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}imag-nb base provisioning DONE (genlock build: $(cat "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt" 2>/dev/null || echo unknown))${NC}"
echo -e "${GREEN}NEXT (from dev1): scripts/imag_scenes.py --host ${STATIC_IP}   # profile+scenes${NC}"
echo -e "${GREEN}       then:      scripts/imag_scenes.py --host ${STATIC_IP} --projector   # once HDMI monitor connected${NC}"
echo -e "${GREEN}========================================${NC}"
