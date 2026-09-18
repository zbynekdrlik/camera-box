#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih-lx role FACTS + pure decision helpers,
# no top-level statements) -- matches the sibling scripts/lib/*.sh convention (obs-fleet.sh,
# camera-set.sh, genlock-markers.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing
# this file executes it in the CALLER's shell, so strict mode here would leak into whichever caller
# sources it. Each caller (setup-strih.sh / verify-strih.sh) sets its own strict mode.
#
# scripts/lib/strih-provision.sh -- issue 1317: the ONE place the Linux strih notebook (`strih-lx`)
# role FACTS + pure provisioning decisions live, sourced by scripts/setup-strih.sh (provisioning),
# scripts/verify-strih.sh (acceptance gate) AND tests/strih_provision_pure_functions.rs (rustc-free
# Tier-0). Same source-of-truth model as camera-set.sh for the camera fleet.
#
# PARALLEL-RUN CONTRACT (owner 16.9.2026): the notebook runs IN PARALLEL with the Windows STRIH-SNV
# cutter until tuned. TWO strih senders coexist, so strih-lx's NDI OUTPUTS are namespaced
# `STRIH-LX (...)` and it joins the clock as a dantesync CLIENT (the Windows PC stays the one NTP
# master). The stream box + receivers must NEVER see a second `STRIH-SNV (...)` sender.

# strih_lx_host -> the hostname the fleet dials. Default strih-lx.lan; STRIH_LX_HOST overrides.
# The literal IP is TBD (assigned 17.9. on arrival, STRIH_LX_IP) -- resolution is by hostname,
# exactly as the imag leg dials "imag" (deploy-genlock-fleet.sh / obs-fleet.sh).
strih_lx_host() { printf '%s' "${STRIH_LX_HOST:-strih-lx.lan}"; }

# strih_lx_ip -> the assigned static IP, or empty until it is assigned on arrival. STRIH_LX_IP wins.
strih_lx_ip() { printf '%s' "${STRIH_LX_IP:-}"; }

# strih_lx_ndi_inputs -> the 10 NDI input names the strih role receives, one per line (issue 1317
# spec; the 2ME feedback inputs are the task's explicit STRIH-SNV names -- the self-feedback
# STRIH-LX nuance is a live-tuning follow-up documented in .claude/rules/strih-linux-provisioning.md).
strih_lx_ndi_inputs() {
  printf '%s\n' \
    'CAM1 (usb)' 'CAM2 (usb)' 'CAM3 (usb)' 'CAM4 (usb)' \
    'CAM5 (usb)' 'CAM6 (usb)' 'CAM7 (usb)' \
    'STRIH-SNV (2ME PGM)' 'STRIH-SNV (2ME PVW)' 'RESOLUME-SNV (cg-obs)'
}

# strih_lx_ndi_outputs -> the namespaced 2ME NDI output names (never a STRIH-SNV name), one per line.
strih_lx_ndi_outputs() { printf '%s\n' 'STRIH-LX (2ME PGM)' 'STRIH-LX (2ME PVW)'; }

# strih_lx_ndi_republishes -> the namespaced genlock-ndi-filter republish names, one per line.
strih_lx_ndi_republishes() {
  printf '%s\n' 'STRIH-LX (interkom)' 'STRIH-LX (MULTIVIEW)' 'STRIH-LX (Grading)'
}

# strih_lx_camera_latency_ms -> the genlock latency floor (ms) every camera input rides (3, the
# rig floor -- latency-pins-baseline.json strih-lx block; the per-run aligner owns any offset).
strih_lx_camera_latency_ms() { printf '3'; }

# strih_lx_bundle_artifact -> the strih FULL-build CI artifact name (linux-genlock.yml strih job).
strih_lx_bundle_artifact() { printf 'obs-genlock-linux-x86_64-strih'; }

# strih_lx_dantesync_client_args -> the dantesync CLIENT invocation args. It points at the Windows
# strih PC as the NTP server and NEVER enables server/master mode (single master while parallel).
strih_lx_dantesync_client_args() { printf -- '--ntp-server %s' "${STRIH_LX_NTP_SERVER:-strih.lan}"; }

# strih_lx_dantesync_is_client_not_master MODE -> 0 iff MODE is a CLIENT mode (never server/master).
# Fail-closed: an empty/unknown mode returns 1 (a strih-lx that cannot prove it is a client must not
# be trusted to not steal the master role).
strih_lx_dantesync_is_client_not_master() {
  # master flags checked first, but NARROWLY: `*server_mode*` (covers dantesync's `ntp_server_mode`)
  # + `*master*`/`*grandmaster*` -- NEVER a bare `*server*`, which would also swallow the CLIENT
  # `--ntp-server` / `ntp_server=<host>` forms and misclassify a client as master (shellcheck
  # SC2221/SC2222 caught exactly that). A bare "server" with no "_mode" is ambiguous -> fail-closed.
  case "${1:-}" in
    "") return 1 ;;
    *server_mode*|*master*) return 1 ;;
    *client*|*slave*|*ntp-server*|*ntp_server=*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_profile_facts -> the OBS profile facts (from the Windows `light` profile inventory,
# 15.9.), key=value one per line: base/output 1920x1080 @ 30 fps NV12, Advanced out, NVENC HEVC
# record to /srv/_REC as 15-min-split mkv.
strih_lx_profile_facts() {
  printf '%s\n' \
    'base_res=1920x1080' \
    'output_res=1920x1080' \
    'fps=30' \
    'color_format=NV12' \
    'out_mode=Advanced' \
    'rec_encoder=obs_nvenc_hevc_tex' \
    'rec_path=/srv/_REC' \
    'rec_format=mkv' \
    'rec_split_min=15'
}

# strih_lx_audio_input_name -> the OBS PipeWire input name for the program-audio interface.
# The Windows path is MiniFuse 4 USB -> VB-Matrix (VASIO-8) ASIO; on Linux the class-compliant
# MiniFuse 4 is a native PipeWire node and PipeWire replaces VB-Matrix (owner 15.9.: NO Dante).
strih_lx_audio_input_name() { printf 'MiniFuse 4'; }

# strih_lx_audio_route_wired -> 0 iff the VB-Matrix -> PipeWire program-audio ROUTE (the graph that
# feeds the mastered program mix into the MiniFuse 4 capture OBS reads) is wired. Fail-loud TODO:
# returns 1 until an operator wires it and sets STRIH_LX_AUDIO_WIRED=1. setup-strih.sh's audio step
# FAILS on this so an unwired audio graph never passes silently.
strih_lx_audio_route_wired() { [ "${STRIH_LX_AUDIO_WIRED:-0}" = "1" ]; }

# strih_lx_output_name_ok NAME -> 0 iff NAME is a namespaced `STRIH-LX (...)` output and NOT a
# `STRIH-SNV (...)` one (the never-a-2nd-STRIH-SNV-sender invariant, single output check).
strih_lx_output_name_ok() {
  case "${1:-}" in
    'STRIH-SNV '*) return 1 ;;
    'STRIH-LX ('*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_no_second_strihsnv_sender  (stdin: live NDI output names, one per line) -> 0 iff NONE of
# them is a `STRIH-SNV (...)` sender. Drain-safe (reads the whole stream, no early break).
strih_lx_no_second_strihsnv_sender() {
  local line collision=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in 'STRIH-SNV '*) collision=1 ;; esac
  done
  [ "$collision" = 0 ]
}

# --- verify-strih.sh pure predicates (stdin-driven, so a live reader feeds them; unit-tested) -----

# strih_lx_render_tick_ok  (stdin: newest OBS log text) -> 0 iff the genlock render tick is enabled.
strih_lx_render_tick_ok() { grep -qiE 'render tick.*(enabled|on)|genlock[^\n]*render tick'; }

# strih_lx_distroav_loaded_ok  (stdin: newest OBS log text) -> 0 iff DistroAV/NDI loaded.
strih_lx_distroav_loaded_ok() { grep -qi 'distroav'; }

# strih_lx_nvenc_available_ok  (stdin: `ffmpeg -encoders` output OR the OBS log) -> 0 iff NVENC present.
strih_lx_nvenc_available_ok() { grep -qi 'nvenc'; }

# strih_lx_single_timesync_authority_ok  (stdin: enabled/active timesync unit names, one per line) ->
# 0 iff dantesync is present AND no competing timesync daemon is (the ops single-authority rule).
strih_lx_single_timesync_authority_ok() {
  local line has_dante=0 competitor=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      *dantesync*) has_dante=1 ;;
      *timesyncd*|*chrony*|*ntpd*|*ntpsec*|*ptp4l*) competitor=1 ;;
    esac
  done
  [ "$has_dante" = 1 ] && [ "$competitor" = 0 ]
}

# --- issue 1317: strih FULL-build browser bundle (obs-browser + CEF) acceptance predicates --------

# strih_lx_browser_bundle_required  (arg1: STRIH_BUILD_FLAGS.txt content) -> 0 iff it declares
# BROWSER-ON (obs-browser + CEF are expected in the bundle). Fail-closed: absent/OFF/empty -> 1
# (browser not required, so verify-strih NOTE-skips the file-presence check rather than failing).
strih_lx_browser_bundle_required() {
  case "${1:-}" in
    *BROWSER-ON*) return 0 ;;
    *) return 1 ;;
  esac
}

# strih_lx_browser_bundle_ok  (stdin: file paths found under the strih install root, one per line --
# what `find <root> -name obs-browser.so -o -name libcef.so` prints) -> 0 iff BOTH obs-browser.so AND
# the CEF runtime libcef.so are present. Drain-safe (reads the whole stream, no early break).
strih_lx_browser_bundle_ok() {
  local line has_browser=0 has_cef=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      *obs-browser.so) has_browser=1 ;;
      *libcef.so)      has_cef=1 ;;
    esac
  done
  [ "$has_browser" = 1 ] && [ "$has_cef" = 1 ]
}

# --- issue 1317 (F6): chrome-sandbox setuid-root (the CEF SUID sandbox helper) --------------------
# The CEF strih bundle ships `chrome-sandbox` mode 700 owned by the CI runner uid. Chromium's SUID
# sandbox contract requires it to be owned root:root mode 4755 (setuid root); otherwise the sandbox
# helper aborts at browser-source launch (unless OBS is started `--no-sandbox`, which we REJECT --
# that weakens every browser source's isolation session-wide). The proper fix is the setuid helper,
# applied once at provisioning and gated in verify-strih.

# strih_lx_chrome_sandbox_fix_cmd BUNDLE_ROOT -> prints the idempotent remote statements that make
# the installed CEF chrome-sandbox launchable: locate it by NAME under BUNDLE_ROOT (never guessing
# the multiarch obs-plugins path) then `chown root:root` + `chmod 4755` it. The emitted statement
# ends with `;` so a mid-string $(...) embedding never glues the following command (the
# scripts/lib/v4l2-neutral.sh _cmd-helper gotcha). It runs as an `if` so a missing chrome-sandbox is
# a no-op (set -e safe); the CALLER (setup-strih.sh) enforces the BROWSER-ON fail-loud presence gate.
strih_lx_chrome_sandbox_fix_cmd() {
  local root="${1:?bundle-root required}"
  printf 'if __csb="$(find %q -type f -name chrome-sandbox 2>/dev/null | head -n1 || true)"; [ -n "$__csb" ]; then chown root:root "$__csb"; chmod 4755 "$__csb"; fi;\n' "$root"
}

# strih_lx_chrome_sandbox_verdict OWNER MODE PRESENT -> prints one verdict token and returns 0 iff
# ok. PRESENT is 1 when chrome-sandbox exists, else 0. Fail-closed order: absent -> `missing`; owner
# not root:root -> `wrong-owner`; mode not 4755 (setuid rwxr-xr-x) -> `wrong-mode`; else -> `ok`.
strih_lx_chrome_sandbox_verdict() {
  local owner="${1:-}" mode="${2:-}" present="${3:-}"
  [ "$present" = 1 ]         || { printf 'missing';     return 1; }
  [ "$owner" = 'root:root' ] || { printf 'wrong-owner'; return 1; }
  [ "$mode" = '4755' ]       || { printf 'wrong-mode';  return 1; }
  printf 'ok'; return 0
}

# --- issue 1317 (owner ROZHODNUTÉ 18.9.): bundle-vs-box release parity --------------------------
# The strih bundle is built on a SPECIFIC Ubuntu release (26.04 for strih-lx) and links THAT
# release's ffmpeg/Qt sonames; deploying a 24.04-built bundle onto a 26.04 box (or vice versa) would
# crash OBS at load (libavcodec60 vs libavcodec62, Qt 6.4 vs 6.10). The CI Stage step stamps a
# `TARGET-RELEASE: ubuntu-<rel>` line into STRIH_BUILD_FLAGS.txt; this predicate gates the box's
# VERSION_ID against it.

# strih_lx_release_parity_ok BUNDLE_FLAGS_TEXT BOX_VERSION_ID -> 0 iff BUNDLE_FLAGS_TEXT carries the
# whole line `TARGET-RELEASE: ubuntu-<BOX_VERSION_ID>` (the release the bundle was built on equals
# the box's /etc/os-release VERSION_ID). Fail-closed (returns 1): any release mismatch, an ABSENT
# TARGET-RELEASE marker (a pre-marker bundle that must be rebuilt, never trusted), or an empty
# BOX_VERSION_ID.
strih_lx_release_parity_ok() {
  local flags="${1:-}" version_id="${2:-}"
  [ -n "$version_id" ] || return 1
  printf '%s\n' "$flags" | grep -qxF "TARGET-RELEASE: ubuntu-${version_id}"
}

# --- issue 1317 (launcher pair): strih-obs-start.sh / strih-obs-stop.sh presence gate -------------
# strih_launcher_pair_ok BIN_DIR -> 0 iff BOTH launcher scripts the strih-obs.service unit's
# ExecStart/ExecStop reference (strih-obs-start.sh + strih-obs-stop.sh) exist under BIN_DIR AND are
# executable. On any failure it PRINTS the offending name(s) (`missing <name>` / `not-executable
# <name>`), one per line, so a dangling ExecStart names ITSELF in verify-strih.sh (a bare "OBS not
# running under the supervisor" would otherwise hide WHY the unit never launched). setup-strih.sh
# step 8 installs the pair mode 0755 BEFORE it enables the unit; this is the acceptance check that
# the install landed and the unit will not flap 203/EXEC.
strih_launcher_pair_ok() {
  local dir="${1:?bin-dir required}" name missing=0
  for name in strih-obs-start.sh strih-obs-stop.sh; do
    if [ ! -f "${dir}/${name}" ]; then
      printf 'missing %s\n' "$name"; missing=1
    elif [ ! -x "${dir}/${name}" ]; then
      printf 'not-executable %s\n' "$name"; missing=1
    fi
  done
  [ "$missing" = 0 ]
}

# --- issue 1317: bundle runtime packages + the /usr prefix install ---------------------------------
# The genlock bundle is BUILT for the /usr prefix and links release-specific Qt6 / ffmpeg 8 / OpenGL
# runtime libraries. On a fresh 26.04 box none of that is installed and the bundle is only copied to
# /opt (no loader path), so `obs` dies at exec with `libavcodec.so.62: cannot open shared object
# file` (13 unresolved sonames). scripts/genlock-runtime-packages.sh records the exact apt packages
# the built bundle links against into RUNTIME_PACKAGES.txt; setup-strih.sh installs them and then
# installs the bundle into its /usr prefix (below), and verify-strih.sh gates both.

# strih_runtime_packages_from_file FILE -> print the apt package names in FILE, one per line, in file
# order. Skips blank lines and comment lines (first non-whitespace char '#'); trims surrounding
# whitespace (dpkg package names never contain whitespace). Returns 1 if FILE does not exist.
strih_runtime_packages_from_file() {
  local file="${1:?packages file required}" line pkg
  [ -f "$file" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    pkg="${line#"${line%%[![:space:]]*}"}"   # ltrim leading whitespace
    pkg="${pkg%"${pkg##*[![:space:]]}"}"       # rtrim trailing whitespace
    [ -n "$pkg" ] || continue
    case "$pkg" in '#'*) continue ;; esac
    printf '%s\n' "$pkg"
  done < "$file"
}

# strih_ldd_unresolved  (stdin: `ldd <file>` output, possibly concatenated for several files) -> print
# each UNRESOLVED soname (an `<soname> => not found` line), one per line, deduped. EMPTY output means
# every dependency resolved. verify-strih.sh runs it over /usr/bin/obs + libobs.so.30 + distroav.so
# and FAILS on any non-empty output (a missing runtime lib IS the 13-soname load failure this fixes).
strih_ldd_unresolved() {
  local line soname
  while IFS= read -r line; do
    case "$line" in
      *'=> not found'*)
        soname="${line%%=>*}"
        soname="${soname#"${soname%%[![:space:]]*}"}"   # ltrim
        soname="${soname%"${soname##*[![:space:]]}"}"     # rtrim
        [ -n "$soname" ] && printf '%s\n' "$soname"
        ;;
    esac
  done | sort -u
}

# strih_install_bundle_prefix BUNDLE LIBDIR BINDIR SHAREDIR -> install the staged genlock bundle into
# its /usr prefix so the dynamic loader finds it (the imag on-box program shape in
# scripts/deploy-genlock-fleet.sh -- issue 1236 perms-normalize + ldconfig): copy
# BUNDLE/lib/x86_64-linux-gnu/. -> LIBDIR (root:root, dirs 0755, files a+rX), BUNDLE/bin/obs ->
# BINDIR/obs (0755 root), BUNDLE/share/obs -> SHAREDIR/obs, then `ldconfig`. Runs as root in
# setup-strih.sh step 4 AFTER the /opt staged copy (which stays the marker home). Returns non-zero on
# a critical copy failure. NOTE: this ~30-line prefix install duplicates the imag on-box program's
# install block (a templated heredoc inside deploy-genlock-fleet.sh with its own probe-gated anchors);
# consolidating the two is the deploy-arm follow-up's job (.claude/rules/strih-linux-provisioning.md).
strih_install_bundle_prefix() {
  local bundle="${1:?bundle root required}" libdir="${2:?libdir required}" bindir="${3:?bindir required}" sharedir="${4:?sharedir required}"
  local rel dst
  [ -d "${bundle}/lib/x86_64-linux-gnu" ] || { printf 'strih_install_bundle_prefix: %s/lib/x86_64-linux-gnu missing\n' "$bundle" >&2; return 1; }
  [ -f "${bundle}/bin/obs" ] || { printf 'strih_install_bundle_prefix: %s/bin/obs missing\n' "$bundle" >&2; return 1; }
  mkdir -p "$libdir" || return 1
  cp -a "${bundle}/lib/x86_64-linux-gnu/." "${libdir}/" || { printf 'strih_install_bundle_prefix: lib copy failed\n' >&2; return 1; }
  # normalize perms/ownership deterministically (issue 1236): reset LIBDIR root:root 0755, then chown
  # root:root + dirs 0755 / files a+rX over EVERY just-installed path (scope to the installed set).
  chown root:root "$libdir" 2>/dev/null || true
  chmod 0755 "$libdir" 2>/dev/null || true
  while IFS= read -r -d '' rel; do
    dst="${libdir}/${rel}"
    [ -e "$dst" ] || continue
    chown root:root "$dst" 2>/dev/null || true
    if [ -d "$dst" ]; then chmod 0755 "$dst" 2>/dev/null || true; else chmod a+rX "$dst" 2>/dev/null || true; fi
  done < <(cd "${bundle}/lib/x86_64-linux-gnu" && find . -mindepth 1 -printf '%P\0')
  install -m 0755 -o root -g root "${bundle}/bin/obs" "${bindir}/obs" || { printf 'strih_install_bundle_prefix: %s/obs install failed\n' "$bindir" >&2; return 1; }
  if [ -d "${bundle}/share/obs" ]; then
    mkdir -p "${sharedir}/obs"
    cp -a "${bundle}/share/obs/." "${sharedir}/obs/" || { printf 'strih_install_bundle_prefix: share/obs copy failed\n' >&2; return 1; }
    chown -R root:root "${sharedir}/obs" 2>/dev/null || true
    chmod 0755 "${sharedir}/obs" 2>/dev/null || true
    find "${sharedir}/obs" -type d -exec chmod 0755 {} + 2>/dev/null || true
    find "${sharedir}/obs" -type f -exec chmod a+rX {} + 2>/dev/null || true
  fi
  ldconfig
}

# --- issue 1317 (this lane): dantesync CLIENT systemd unit + the verify sleep-mask predicate -------
# The strih notebook joins the cluster clock as a dantesync CLIENT (the Windows PC stays the one NTP
# master). setup-strih.sh step 2 installs the unit; verify-strih.sh asserts it is active + a fresh
# offset. These pure helpers are unit-tested in tests/strih_provision_pure_functions.rs.

# strih_dantesync_unit_text [ARGS] -> print the systemd unit text for the strih-lx dantesync CLIENT
# daemon (the EXACT cambox unit shape: Type=simple, Restart=always, RestartSec=5,
# WantedBy=multi-user.target). ARGS is the dantesync invocation (default the client args); ExecStart
# is `/usr/local/bin/dantesync <ARGS>`. `--service` is NOT an installer flag -- it is a run mode, so
# it never appears here; the daemon IS `dantesync --ntp-server <host>`. Fail-closed: if ARGS classify
# as a server/master invocation, emit NOTHING and return 1 (a strih-lx unit must never spawn a 2nd
# NTP master while running in parallel with the Windows PC).
strih_dantesync_unit_text() {
  local args="${1:-$(strih_lx_dantesync_client_args)}"
  strih_lx_dantesync_is_client_not_master "$args" || return 1
  cat <<EOF
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantesync ${args}
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

# strih_verify_sleep_masked STATE -> 0 iff STATE reports sleep.target masked. `systemctl is-enabled
# sleep.target` prints "masked" to stdout AND exits 1 for a masked unit, so a caller's `|| echo
# masked` fallback DOUBLE-appends -> the captured value is "masked\nmasked" and a naive `= masked`
# comparison FALSE-FAILS on a correctly-masked box (the issue-1317 verify bug). Grade the FIRST line
# only, so both the clean "masked" and the defensive "masked\nmasked" pass; "enabled"/"static"/""
# (or anything whose first line is not exactly "masked") -> not masked (return 1, fail-closed).
strih_verify_sleep_masked() {
  local state="${1:-}" first
  first="${state%%$'\n'*}"
  [ "$first" = masked ]
}
