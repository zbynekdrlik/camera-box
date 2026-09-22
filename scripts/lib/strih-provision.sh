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
# spec; the 2ME feedback inputs are the task's explicit STRIH-SNV names). NOTE (issue 1352): the
# actual box seed (strih_lx_seed_manifest_json) now points the 2ME feedback pair at strih-lx's OWN
# STRIH-LX (2ME PGM/PVW) outputs (the M4 self-loop, Windows strih retired). This facts list KEEPS the
# STRIH-SNV names DELIBERATELY -- it is unconsumed by setup-strih.sh (defined + unit-tested only), and
# the canonical self-feedback INPUT name is confirmed on the live box in the live-tuning follow-up
# documented in .claude/rules/strih-linux-provisioning.md ("Follow-ups: the 2ME self-feedback INPUT
# names"); flipping these two here is deferred to that follow-up, not this provisioning lane.
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

# strih_lx_obs_gpu_env -> print the strih-lx OBS GPU-render env block: the 4 exports that make OBS
# render on the RTX 5050 dGPU via XWayland PRIME render-offload (issue 1352). ONE source of truth for
# strih-obs-start.sh's `@STRIH_LX_OBS_GPU_ENV@` marker (setup-strih.sh step 8 substitutes it at
# install -- the `@JANUS_ROOM_SECRET@` idiom). QT_QPA_PLATFORM=xcb forces the Qt XWayland path
# (native-Wayland NVIDIA EGL crash-loops with `eglSwapBuffers failed`); the 3 __NV_/__GLX_/__EGL_
# PRIME vars route GL onto the NVIDIA vendor lib (the Intel iGPU saturates at 85 % -> program lag
# 7-20 %, MV 6-7 fps). Printed as ONE `;`-joined line (no trailing newline) so a `$(...)` embedding
# never glues the following statement (the CLAUDE.md newline-strip gotcha). NOTE: on the RTX a
# projector's toplevel GL surface still stalls ~0.5 s/present under PRIME -> strih-mv-host.service
# re-hosts every OBS projector as a CHILD window (lag 93 % -> 0 %, MV 1.8 -> 29.8 fps proven).
strih_lx_obs_gpu_env() {
  printf '%s' 'export QT_QPA_PLATFORM=xcb; export __NV_PRIME_RENDER_OFFLOAD=1; export __GLX_VENDOR_LIBRARY_NAME=nvidia; export __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json'
}

# strih_lx_ndi_output_ini_cmds USER_INI -> print the idempotent bash command that seeds DistroAV's
# `[NDIPlugin]` program/preview OUTPUT identity into USER_INI (issue 1352): the BARE names
# `MainOutputName=2ME PGM` / `PreviewOutputName=2ME PVW` + `MainOutputEnabled`/`PreviewOutputEnabled`
# =true. DistroAV PREPENDS the box hostname -> announced `STRIH-LX (2ME PGM)` / `(2ME PVW)`; a name
# that ALREADY carried `STRIH-LX ` here would double it (the `STRIH-LX (STRIH-LX (2ME PGM))` bug).
# MUST run with OBS STOPPED (OBS rewrites user.ini on exit) -- setup-strih is enable-only, so step 7
# runs before any launch. python3 RawConfigParser upsert (optionxform=str, strict=False): idempotent,
# and NEVER clobbers an unparseable ini (mirrors the step-7 SaveProjectors upsert). Emitted via an
# UNQUOTED heredoc that interpolates ONLY ${ini}; the inner `<<'PYNDI'` python body carries no `$` /
# backtick, so nothing else expands. `eval`-consumed as a standalone command (no `$(...)` embedding).
strih_lx_ndi_output_ini_cmds() {
  local ini="${1:?user.ini path required}"
  cat <<EOF
python3 - "${ini}" <<'PYNDI'
import configparser, os, sys
path = sys.argv[1]
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
if os.path.exists(path):
    try:
        cp.read(path)
    except Exception as e:
        sys.stderr.write("user.ini parse failed (%s) -- leaving it untouched\n" % e)
        sys.exit(3)
if not cp.has_section("NDIPlugin"):
    cp.add_section("NDIPlugin")
for kv in ("MainOutputName=2ME PGM", "PreviewOutputName=2ME PVW", "MainOutputEnabled=true", "PreviewOutputEnabled=true"):
    k, v = kv.split("=", 1)
    cp.set("NDIPlugin", k, v)
with open(path, "w") as fh:
    cp.write(fh, space_around_delimiters=False)
PYNDI
EOF
}

# strih_lx_obs_global_ini_cmds OBS_CFG_DIR -> print the idempotent bash command that seeds
# `[General] BrowserHWAccel=false` into OBS's `global.ini` (issue 1317). This is the ONLY thing that
# stops the CEF int3 crash-loop (exit 133 every ~60 s) of the browser sources on this RTX 5050 /
# GNOME-Wayland stack -- the GPU-accelerated CEF path crashes; false makes CEF software-render via
# libvk_swiftshader (0 crashes, verified live 22.9.2026). It is a hand edit today, not seeded. Same
# RawConfigParser upsert shape as strih_lx_ndi_output_ini_cmds (the step-7/#1352 [NDIPlugin] printer),
# but the FILE is `global.ini` (OBS's cross-collection settings), NOT `user.ini`, and the section is
# `[General]`. MUST run with OBS STOPPED (OBS rewrites global.ini on exit) -- setup-strih is
# enable-only, so step 7 runs before any launch. python3 RawConfigParser upsert (optionxform=str,
# strict=False): idempotent, and NEVER clobbers an unparseable ini. Emitted via an UNQUOTED heredoc
# that interpolates ONLY ${ini}; the inner `<<'PYBHW'` python body carries no `$` / backtick.
# `eval`-consumed as a standalone command (no `$(...)` embedding).
strih_lx_obs_global_ini_cmds() {
  local dir="${1:?obs config dir required}"
  local ini="${dir%/}/global.ini"
  cat <<EOF
python3 - "${ini}" <<'PYBHW'
import configparser, os, sys
path = sys.argv[1]
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
if os.path.exists(path):
    try:
        cp.read(path)
    except Exception as e:
        sys.stderr.write("global.ini parse failed (%s) -- leaving it untouched\n" % e)
        sys.exit(3)
if not cp.has_section("General"):
    cp.add_section("General")
cp.set("General", "BrowserHWAccel", "false")
with open(path, "w") as fh:
    cp.write(fh, space_around_delimiters=False)
PYBHW
EOF
}

# strih_lx_obs_plugin_prune_list -> print (one per line) the ONE source of truth for the obs-plugins
# the strih genlock bundle ships that must be PRUNED on strih-lx (issue 1317, owner request 22.9.):
# each logs errors at every boot and is unusable on this box. `decklink*.so` = the DeckLink family
# (no DeckLink hardware); `obs-qsv11.so` = Intel QSV (the box is NVIDIA -> nvenc); `obs-vst.so` =
# unused. KEEP everything else, especially distroav.so (NDI) + obs-browser.so/libcef.so (browser
# sources). Entries may be globs -- callers expand them against each plugin dir (setup-strih step 4
# tail rm loop; verify-strih absence check), so both the setup PRUNE and the verify ABSENCE assertion
# share this list. The next bundle install re-copies the whole tree, so the prune must re-run each
# provisioning (it is idempotent).
strih_lx_obs_plugin_prune_list() {
  printf '%s\n' 'decklink*.so' 'obs-qsv11.so' 'obs-vst.so'
}

# strih_lx_obs_plugin_dirs BUNDLE_ROOT USR_LIBDIR -> print (one per line) the two obs-plugins dirs a
# prune/absence check must cover: the /opt staged bundle copy AND the /usr-prefix copy the running
# OBS loads from (mirrors strih_lx_chrome_sandbox_setuid_roots' two-root reasoning -- pruning only the
# bundle copy would leave the dead plugins in /usr where OBS actually loads them). ONE source of truth
# shared by setup-strih.sh's step-4 tail prune loop and verify-strih.sh's absence assertion.
strih_lx_obs_plugin_dirs() {
  local bundle="${1:?bundle-root required}" libdir="${2:?usr libdir required}"
  printf '%s\n' "${bundle%/}/lib/x86_64-linux-gnu/obs-plugins" "${libdir%/}/obs-plugins"
}

# strih_lx_seed_manifest_json -> the FULL /opt/camera-box/strih-lx-seed.json for the OPERATOR
# (production) collection (issue 1317, this lane). The notebook now runs the migrated operator
# collection, whose INPUT names are the canonical strih names the whole E2E tooling addresses
# (obs_burn_filter / obs_phase2 / recv-timing / genlock-audit all key on `NDI camN`) -- NOT the
# parallel-phase derived `NDI CAMn (usb)` the old string manifest produced (which made the launch
# seed CREATE duplicate receivers). So the manifest carries EXPLICIT-name OBJECTS {sender,input,scene}
# (a DATA name-map, not code) + `"mode":"update-only"` -> strih_scenes.py --bootstrap heals the
# certified genlock class onto inputs that ALREADY exist and NEVER CreateScene/CreateInput; a missing
# declared input is REPORTED.
#   * sender = the NDI source the operator's input receives. The 7 cameras receive the fleet
#     `CAMn (usb)` senders; the 2ME feedback pair receives strih-lx's OWN `STRIH-LX (2ME PGM/PVW)`
#     outputs -- the M4 self-loop (issue 1352; the dead-parallel-phase `STRIH-SNV (2ME PGM/PVW)`
#     senders are gone now the Windows strih is retired); the cg pair
#     receives the `RESOLUME-SNV (cg-obs)` genlocked sender. The 2ME-feedback source + the CG-pair
#     senders are supervisor-confirmable DATA (the LANE-RETURN followup: re-read the live collection's
#     input settings) -- a wrong sender is a MANIFEST edit, never a code change, and update-only never
#     renames the operator's source, so a guessed sender is harmless (it only drives CLASS detection,
#     which the input NAME already provides).
#   * The 5 STRIH-LX NDI OUTPUTS stay in `outputs` (issue 1347 owns the rename); the seeder never
#     touches outputs. Latency rides the manifest floor 3 (genlock_latency_ms_src).
strih_lx_seed_manifest_json() {
  local lat
  lat="$(strih_lx_camera_latency_ms)"
  printf '{\n'
  printf '  "mode": "update-only",\n'
  printf '  "inputs": [\n'
  printf '    {"sender": "CAM1 (usb)", "input": "NDI cam1", "scene": "Cam 1"},\n'
  printf '    {"sender": "CAM2 (usb)", "input": "NDI cam2", "scene": "Cam 2"},\n'
  printf '    {"sender": "CAM3 (usb)", "input": "NDI cam3", "scene": "Cam 3"},\n'
  printf '    {"sender": "CAM4 (usb)", "input": "NDI cam4", "scene": "Cam 4"},\n'
  printf '    {"sender": "CAM5 (usb)", "input": "NDI cam5", "scene": "Cam 5"},\n'
  printf '    {"sender": "CAM6 (usb)", "input": "NDI cam6", "scene": "Cam 6"},\n'
  printf '    {"sender": "CAM7 (usb)", "input": "NDI cam7", "scene": "Cam 7"},\n'
  printf '    {"sender": "STRIH-LX (2ME PVW)", "input": "NDI 2ME PVW", "scene": "2ME PVW"},\n'
  printf '    {"sender": "STRIH-LX (2ME PGM)", "input": "NDI 2ME PGM (mv)", "scene": "2ME PGM"},\n'
  printf '    {"sender": "RESOLUME-SNV (cg-obs)", "input": "cg", "scene": "CG"},\n'
  printf '    {"sender": "RESOLUME-SNV (cg-obs)", "input": "CG-obs", "scene": "CG-obs"}\n'
  printf '  ],\n'
  printf '  "outputs": [\n'
  { strih_lx_ndi_outputs; strih_lx_ndi_republishes; } | sed 's/.*/    "&",/' | sed '$ s/,$//'
  printf '  ],\n'
  printf '  "camera_latency_ms": %s\n' "$lat"
  printf '}\n'
}

# strih_lx_bundle_artifact -> the strih FULL-build CI artifact name (linux-genlock.yml strih job).
strih_lx_bundle_artifact() { printf 'obs-genlock-linux-x86_64-strih'; }

# strih_lx_dantesync_client_args -> the dantesync CLIENT invocation args. It points at the Windows
# strih PC as the NTP server and NEVER enables server/master mode (single master while parallel).
strih_lx_dantesync_client_args() { printf -- '--ntp-server %s' "${STRIH_LX_NTP_SERVER:-strih.lan}"; }

# strih_lx_dantesync_is_client_not_master MODE -> 0 iff MODE is a CLIENT mode (never server/master).
# INTERNAL helper (issue 1317, this lane): retained as the pure CLIENT-args classifier that the
# role-aware public predicate strih_lx_dantesync_role_ok delegates to for the `client` branch. It is
# NO LONGER the caller-facing gate (step 2 + strih_dantesync_unit_text now call role_ok) -- because
# post-M4 (20.9.2026) the strih notebook IS the fleet NTP master (server role), and this predicate
# fail-closes on server mode, which is CORRECT for a client invocation but wrong to use as the sole
# gate for the box. Fail-closed: an empty/unknown mode returns 1.
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

# strih_lx_dantesync_role_ok ROLE ARGS -> the ROLE-AWARE public predicate (issue 1317, this lane):
# 0 iff ROLE + ARGS name a coherent, non-ambiguous dantesync invocation for the strih-lx box.
# REPLACES strih_lx_dantesync_is_client_not_master as the caller-facing gate. Post-M4 the notebook is
# the fleet's ONE NTP master, so a `server` role is CORRECT (never fail-closed on server mode itself);
# fail-closed only on an AMBIGUOUS shape:
#   * ROLE=server -> the bare `dantesync` daemon (NTP master, ntp_server_mode in config.json); it
#     carries NO extra args. server + any ARGS is contradictory (e.g. a stray `--ntp-server` that
#     would point the master at a host) -> AMBIGUOUS -> fail-closed. server + empty ARGS -> 0.
#   * ROLE=client -> ARGS must be a genuine CLIENT invocation (never server/master), delegated to
#     strih_lx_dantesync_is_client_not_master (which fail-closes on empty/master).
#   * any other/empty ROLE -> fail-closed.
strih_lx_dantesync_role_ok() {
  local role="${1:-}" args="${2-}"
  case "$role" in
    server) [ -z "$args" ] && return 0 || return 1 ;;
    client) strih_lx_dantesync_is_client_not_master "$args" ;;
    *) return 1 ;;
  esac
}

# strih_lx_dantesync_status_role_verdict ROLE REACHABLE MODE UDP123 -> the LIVE-read verdict for
# verify-strih's dantesync-role acceptance item (issue 1317). Prints ONE token and returns 0 only for
# the fully-`ok` state; every other state prints its own token + returns non-zero. Args (verify-strih
# feeds live reads):
#   ROLE       client|server (the provisioned STRIH_LX_DANTESYNC_ROLE, default server post-M4)
#   REACHABLE  1 iff :8898/status answered
#   MODE       the status `mode` field value (LOCK / NANO / ... / absent)
#   UDP123     1 iff an ntp UDP :123 listener is present (ss -ulnp) -- REQUIRED for the server role only
# Fail-closed order (missing args default to the not-configured value):
#   unreachable       -> :8898/status did not answer (dantesync down / no HTTP status)
#   mode:<x>          -> reachable but mode is not a locked mode (LOCK/NANO)
#   no-ntp-listener   -> server role but no UDP :123 listener (the master is not serving NTP)
#   ok                -> reachable + locked mode (+ for server, a :123 listener)
strih_lx_dantesync_status_role_verdict() {
  local role="${1:-}" reachable="${2:-0}" mode="${3:-absent}" udp123="${4:-0}"
  [ "$reachable" = 1 ] || { printf 'unreachable'; return 1; }
  case "$mode" in
    LOCK|NANO) : ;;
    *) printf 'mode:%s' "$mode"; return 1 ;;
  esac
  if [ "$role" = server ] && [ "$udp123" != 1 ]; then
    printf 'no-ntp-listener'; return 1
  fi
  printf 'ok'; return 0
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

# strih_lx_audio_input_name -> the OBS PipeWire input name kept for the program-audio input (issue
# 1344: the OBS input is `ASIO zvuk` on device `strih-program.monitor`; this label is the human name
# reported by setup/verify, unchanged for continuity).
strih_lx_audio_input_name() { printf 'ASIO zvuk'; }

# --- issue 1344: the local PipeWire program-audio graph (VB-Matrix replacement) ------------------
# Root cause (design 20.9.): the OBS program (`ASIO zvuk`) is a NETWORK stream (fohabl-strih VBAN,
# VASIO8), NOT the MiniFuse. So the hub writes the program mix to a `strih-program` null sink OBS
# captures via `strih-program.monitor`; the MiniFuse carries only the operator TALKBACK mic (the
# hub reads it). setup-strih step 12 installs these operator-session PipeWire configs; the old
# fail-loud STRIH_LX_AUDIO_WIRED flag is REMOVED — verify-strih derives wiredness from live state.

# strih_pipewire_program_sink_conf -> the operator-session pipewire.conf.d drop-in that creates the
# `strih-program` null Audio/Sink so OBS can capture `strih-program.monitor` even before the hub runs.
strih_pipewire_program_sink_conf() {
  cat <<'CONF'
# strih-lx program-audio null sink (issue 1344) — installed by setup-strih step 12.
# The intercom hub writes the mastered program mix here; OBS captures strih-program.monitor
# as its `ASIO zvuk` program input. Do NOT edit by hand.
context.objects = [
    {   factory = adapter
        args = {
            factory.name       = support.null-audio-sink
            node.name          = "strih-program"
            node.description   = "Strih Program (OBS ASIO zvuk)"
            media.class        = "Audio/Sink"
            audio.rate         = 48000
            audio.position     = [ FL FR ]
            monitor.channel-volumes = true
            object.linger      = true
        }
    }
]
CONF
}

# strih_pipewire_program_loopback_conf -> the operator-session pipewire.conf.d drop-in that
# republishes the strih-program null-sink monitor as a REAL pulse-visible Audio/Source node (issue
# 1344 follow-up, 20.9.2026 live diagnosis).
#
# Root cause: the `support.null-audio-sink` created by strih_pipewire_program_sink_conf via
# context.objects does NOT get a pulse-visible monitor on Ubuntu 26.04 pipewire (`pw-dump` shows
# `pulse.monitor = None` on it) -- OBS's pulse_input_capture enumeration never lists
# `strih-program.monitor`, so binding it reads digital silence even though the audio is really
# there (`pw-cat --target strih-program.monitor` captures it fine; only OBS/libpulse can't see it).
#
# Fix (proven live on strih-lx, 20.9.2026): a `libpipewire-module-loopback` that captures the
# strih-program SINK OUTPUT (never the monitor) and republishes it as its OWN node. The republished
# node's `media.class` MUST be plain "Audio/Source" -- NOT "Audio/Source/Virtual". With Virtual, OBS
# enumerates the source in its device list but its capture stream never links (silence); plain
# Audio/Source captures correctly. scripts/strih_scenes.py's AUDIO_MONITOR_DEVICE binds
# `strih-program-source` (this node's name), not `strih-program.monitor`.
strih_pipewire_program_loopback_conf() {
  cat <<'CONF'
# strih-lx program-audio loopback source (issue 1344 follow-up) — installed by setup-strih step 12.
# strih-program.monitor is NOT pulse-visible to OBS on this box; this loopback republishes the
# strih-program sink as a real Audio/Source node OBS's pulse_input_capture can enumerate + capture.
# Do NOT edit by hand.
context.modules = [
    {   name = libpipewire-module-loopback
        args = {
            node.description = "Strih Program (OBS ASIO zvuk)"
            capture.props = {
                node.target         = "strih-program"
                stream.capture.sink = true
                node.passive        = true
            }
            playback.props = {
                node.name         = "strih-program-source"
                node.description  = "Strih Program (OBS ASIO zvuk)"
                media.class       = "Audio/Source"
            }
        }
    }
]
CONF
}

# strih_wireplumber_minifuse_rule -> the WirePlumber rule pinning the MiniFuse 4 to its pro-audio
# profile at 48 kHz (the operator talkback capture the hub reads; OBS never touches it).
strih_wireplumber_minifuse_rule() {
  cat <<'RULE'
# strih-lx MiniFuse 4 talkback capture (issue 1344) — installed by setup-strih step 12.
# Pin the class-compliant MiniFuse 4 to its pro-audio profile at 48 kHz so the intercom hub reads
# the operator talkback mic on a stable node. Do NOT edit by hand.
monitor.alsa.rules = [
    {
        matches = [ { device.name = "~alsa_card.*MiniFuse.*" } ]
        actions = { update-props = { device.profile = "pro-audio" } }
    }
    {
        matches = [ { node.name = "~alsa_input.*MiniFuse.*" } ]
        actions = { update-props = { audio.rate = 48000 } }
    }
]
RULE
}

# strih_intercom_audio_dropin USER UID -> the systemd drop-in that runs intercom-hub AS THE OPERATOR
# (not DynamicUser) with the operator's PipeWire runtime, so the pw-cat children reach the operator
# audio session. DynamicUser cannot traverse the operator's 0700 /run/user/<uid> to reach pipewire-0,
# so the hub runs as the operator uid; the sidecar alternative adds a 2nd process + IPC socket. The
# other hub resources (unprivileged UDP/TCP, world-readable config) are unaffected.
strih_intercom_audio_dropin() {
  local user="${1:?strih_intercom_audio_dropin needs the operator USER}"
  local uid="${2:?strih_intercom_audio_dropin needs the operator UID}"
  cat <<DROPIN
# intercom-hub local-audio override (issue 1344) — installed by setup-strih step 12.
# Run the hub as the operator so its pw-cat program-sink + talkback-capture children reach the
# operator's PipeWire session. Do NOT edit by hand.
[Service]
DynamicUser=no
User=${user}
Group=${user}
SupplementaryGroups=audio pipewire render
Environment=XDG_RUNTIME_DIR=/run/user/${uid}
Environment=PIPEWIRE_RUNTIME_DIR=/run/user/${uid}
# The operator session must exist for /run/user/<uid>/pipewire-0 to be present.
After=user@${uid}.service
# ProtectHome=read-only keeps /run/user/<uid> reachable (ProtectHome=tmpfs/yes would HIDE it and
# break PipeWire); narrow the residual read-only \$HOME exposure by blanking the sensitive dirs
# (\`-\` = tolerate absence). A DynamicUser gave no home at all, so this is the minimal downgrade
# that still lets pw-cat reach the operator's PipeWire socket.
ProtectHome=read-only
InaccessiblePaths=-/home/${user}/.ssh -/home/${user}/.gnupg -/home/${user}/.config/gh
DROPIN
}

# strih_lx_program_audio_verdict SINK RX INPUT_KIND FOH_LIVE LEVEL_OK -> the DERIVED (audio) verdict
# for verify-strih (replaces the STRIH_LX_AUDIO_WIRED flag). Args:
#   SINK        1 iff the `strih-program` sink node is present (pw-cli ls Node)
#   RX          1 iff the hub reports a program source rx > 0 pkt/s
#   INPUT_KIND  the OBS `ASIO zvuk` input kind (pulse_input_capture | asio_input_capture | absent)
#   FOH_LIVE    1 iff FOH is playing (a level can be measured) | 0 | unknown
#   LEVEL_OK    1 iff the measured level clears the -60 dBFS bar | 0 | na
# Prints `PASS <msg>` / `NOTE <msg>` / `FAIL <msg>`; exits 0 for PASS/NOTE, 1 for FAIL.
strih_lx_program_audio_verdict() {
  local sink="${1:-0}" rx="${2:-0}" kind="${3:-absent}" foh="${4:-unknown}" level="${5:-na}"
  if [ "$sink" != "1" ]; then
    printf 'FAIL strih-program PipeWire sink missing\n'; return 1
  fi
  if [ "$kind" != "pulse_input_capture" ]; then
    printf 'FAIL OBS "ASIO zvuk" input missing or not pulse_input_capture (got %s)\n' "$kind"; return 1
  fi
  if [ "$rx" != "1" ]; then
    printf 'FAIL hub not receiving the program feed (rx=0)\n'; return 1
  fi
  if [ "$foh" = "1" ]; then
    if [ "$level" = "1" ]; then
      printf 'PASS program audio present and above the -60 dBFS bar\n'; return 0
    fi
    printf 'FAIL program input SILENT while FOH is live (below -60 dBFS)\n'; return 1
  fi
  printf 'NOTE program audio wired; FOH idle so level unchecked (report-only)\n'; return 0
}

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
strih_lx_nvenc_available_ok() { grep -i 'nvenc' >/dev/null 2>&1; }  # reads to EOF: drain-safe under pipefail (issue 1352)

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

# strih_lx_chrome_sandbox_fix_cmd ROOT [ROOT...] -> prints the idempotent remote statements that make
# EVERY installed CEF chrome-sandbox launchable: for each ROOT, locate chrome-sandbox by NAME under it
# (never guessing the multiarch obs-plugins path) then `chown root:root` + `chmod 4755` it. Variadic
# since issue 1317 slice: the running /usr-install OBS loads chrome-sandbox from the /usr-prefix
# obs-plugins dir, NOT only the /opt bundle root, so the setuid must cover EVERY root the running OBS
# could load it from (resolved by strih_lx_chrome_sandbox_setuid_roots, the ONE source of truth). A
# single ROOT keeps the pre-slice behaviour. Each emitted statement ends with `;` so a mid-string
# $(...) embedding never glues the following command (the scripts/lib/v4l2-neutral.sh _cmd-helper
# gotcha). Each runs as an `if` so a missing chrome-sandbox under a root is a no-op (set -e safe); the
# CALLER (setup-strih.sh) enforces the BROWSER-ON fail-loud presence gate.
strih_lx_chrome_sandbox_fix_cmd() {
  [ "$#" -ge 1 ] || { echo "strih_lx_chrome_sandbox_fix_cmd: at least one root required" >&2; return 2; }
  local root
  for root in "$@"; do
    printf 'if __csb="$(find %q -type f -name chrome-sandbox 2>/dev/null | head -n1 || true)"; [ -n "$__csb" ]; then chown root:root "$__csb"; chmod 4755 "$__csb"; fi;\n' "$root"
  done
}

# strih_lx_chrome_sandbox_setuid_roots BUNDLE_ROOT USR_LIBDIR -> print (one per line) the roots under
# which chrome-sandbox must be setuid: the /opt bundle root AND the resolved /usr-prefix obs-plugins
# dir the /usr-install OBS actually loads it from. ONE source of truth shared by the setup-strih.sh
# F6 setuid step and verify-strih.sh's /usr-path assertion (issue 1317 slice: the F6 step used to
# setuid only the bundle copy, leaving /usr/lib/.../obs-plugins/chrome-sandbox at 0755 -> CEF broke).
strih_lx_chrome_sandbox_setuid_roots() {
  local bundle="${1:?bundle-root required}" libdir="${2:?usr libdir required}"
  printf '%s\n' "$bundle" "${libdir%/}/obs-plugins"
}

# strih_lx_chrome_sandbox_usr_path USR_LIBDIR -> the /usr-prefix chrome-sandbox path the running OBS
# loads (issue 1317 slice). The multiarch obs-plugins copy, distinct from the /opt bundle copy.
strih_lx_chrome_sandbox_usr_path() {
  local libdir="${1:?usr libdir required}"
  printf '%s\n' "${libdir%/}/obs-plugins/chrome-sandbox"
}

# strih_lx_qt6_svg_iconengine_path USR_LIBDIR -> the Qt6 SVG icon-engine plugin path (issue 1317
# slice). Ubuntu 26.04 ships it in the SEPARATE `qt6-svg-plugins` package (NOT libqt6svg6, which
# carries only the library); without it OBS 32's default SVG Yami theme renders its icons blank.
strih_lx_qt6_svg_iconengine_path() {
  local libdir="${1:?usr libdir required}"
  printf '%s\n' "${libdir%/}/qt6/plugins/iconengines/libqsvgicon.so"
}

# strih_lx_obs_ui_fix_verdict SVG_PRESENT CS_OWNER CS_MODE -> print ONE verdict token, return 0 iff
# `ok` (issue 1317 slice: the combined verify item). SVG_PRESENT is 1 when the qt6-svg iconengine
# plugin exists, else 0. Fail-closed order: svg missing -> `svg-missing`; owner not root:root ->
# `sandbox-wrong-owner`; mode not 4755 (setuid rwxr-xr-x) -> `sandbox-wrong-mode`; else -> `ok`.
strih_lx_obs_ui_fix_verdict() {
  local svg="${1:-}" owner="${2:-}" mode="${3:-}"
  [ "$svg" = 1 ]            || { printf 'svg-missing';         return 1; }
  [ "$owner" = 'root:root' ] || { printf 'sandbox-wrong-owner'; return 1; }
  [ "$mode" = '4755' ]      || { printf 'sandbox-wrong-mode';  return 1; }
  printf 'ok'; return 0
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

# strih_bundle_bin_files BUNDLE -> print every REGULAR file under ${bundle}/bin, one path per line,
# sorted (obs + obs-ffmpeg-mux + obs-nvenc-test + any future helper). OBS resolves its helper
# processes (obs-ffmpeg-mux the recording muxer, obs-nvenc-test the NVENC probe) NEXT TO ITS OWN
# EXECUTABLE (`os_get_executable_path`), so a prefix install must copy the WHOLE bin/ dir next to obs
# -- installing only `obs` left the notebook OBS logging `[NVENC] Failed to launch the NVENC test
# process` -> `NVENC not supported`, and RECORDING (obs-ffmpeg-mux) would fail outright (issue 1317,
# live 20.9.2026). This is the ENUMERATION seam (pure, Tier-0-testable against a fake bundle dir); the
# install itself needs root. The bundle dir on the box is user-private (drwx------), but this runs as
# root, so `find` traverses it fine.
strih_bundle_bin_files() {
  local bundle="${1:?bundle root required}"
  [ -d "${bundle}/bin" ] || { printf 'strih_bundle_bin_files: %s/bin missing\n' "$bundle" >&2; return 1; }
  find "${bundle}/bin" -maxdepth 1 -type f | sort
}

# strih_obs_helpers_verdict MUX NVTEST -> print ONE verdict token, 0 iff `ok`. Fail-closed: both OBS
# helper binaries (obs-ffmpeg-mux the record muxer + obs-nvenc-test the NVENC probe) must be
# present+executable beside /usr/bin/obs (recording-critical). MUX/NVTEST are 1/0. verify-strih.sh
# feeds the live `[ -x /usr/bin/obs-ffmpeg-mux ]` / `[ -x /usr/bin/obs-nvenc-test ]` reads.
strih_obs_helpers_verdict() {
  local mux="${1:-0}" nvtest="${2:-0}"
  [ "$mux" = 1 ]    || { printf 'no-obs-ffmpeg-mux'; return 1; }
  [ "$nvtest" = 1 ] || { printf 'no-obs-nvenc-test'; return 1; }
  printf 'ok'; return 0
}

# strih_nvenc_log_verdict  (stdin: an OBS log's text) -> grade the NVENC state (report-only, needs a
# RUNNING OBS). `nvenc-ok` (rc 0) when the log carries the healthy `[obs-nvenc] NVENC version:` line;
# `nvenc-unsupported` (rc 1) when it logged `NVENC not supported` with NO version line (the
# missing-helper signature); `nvenc-unknown` (rc 2) otherwise (OBS not up yet / no NVENC line). The
# caller renders unknown as a NOTE (the helper-presence gate above is the HARD FAIL).
strih_nvenc_log_verdict() {
  local text
  text="$(cat)"
  # here-strings (NOT `printf | grep`): a real OBS log is 100s of KB, and `printf "$text" | grep -q`
  # SIGPIPEs printf the instant grep matches early -> under the caller's pipefail the pipeline goes
  # non-zero, the `if` reads false, and a HEALTHY large log misgrades (the drift-guard-log-parsers.md
  # SIGPIPE class). `grep -q <<<"$text"` reads the whole stdin with no upstream pipe to break.
  if grep -q '\[obs-nvenc\] NVENC version:' <<<"$text"; then printf 'nvenc-ok'; return 0; fi
  if grep -q 'NVENC not supported' <<<"$text"; then printf 'nvenc-unsupported'; return 1; fi
  printf 'nvenc-unknown'; return 2
}

# strih_install_bundle_prefix BUNDLE LIBDIR BINDIR SHAREDIR -> install the staged genlock bundle into
# its /usr prefix so the dynamic loader finds it (the imag on-box program shape in
# scripts/deploy-genlock-fleet.sh -- issue 1236 perms-normalize + ldconfig): copy
# BUNDLE/lib/x86_64-linux-gnu/. -> LIBDIR (root:root, dirs 0755, files a+rX), EVERY file in
# BUNDLE/bin/ -> BINDIR (0755 root -- obs AND its helper processes, issue 1317), BUNDLE/share/obs ->
# SHAREDIR/obs, then `ldconfig`. Runs as root in setup-strih.sh step 4 AFTER the /opt staged copy
# (which stays the marker home). Returns non-zero on a critical copy failure. NOTE: this ~30-line
# prefix install duplicates the imag on-box program's install block (a templated heredoc inside
# deploy-genlock-fleet.sh with its own probe-gated anchors); consolidating the two is the deploy-arm
# follow-up's job (.claude/rules/strih-linux-provisioning.md).
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
  # Install EVERY file in bin/ next to obs (obs + obs-ffmpeg-mux + obs-nvenc-test + any future
  # helper), root:root 0755 -- OBS resolves its helpers relative to its OWN executable (issue 1317).
  local binf base
  while IFS= read -r binf; do
    [ -n "$binf" ] || continue
    base="$(basename "$binf")"
    install -m 0755 -o root -g root "$binf" "${bindir}/${base}" || { printf 'strih_install_bundle_prefix: %s install failed\n' "$base" >&2; return 1; }
  done < <(strih_bundle_bin_files "$bundle")
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

# --- issue 1317 (this lane): dantesync ROLE-aware systemd unit + the verify sleep-mask predicate ----
# Post-M4 (20.9.2026) the strih notebook IS the fleet's ONE NTP master (`strih.lan` -> 10.77.9.202),
# so the unit now carries a ROLE: `server` (the M4 default) renders the bare NTP-master daemon; the
# historical `client` role (the dead parallel-run shape) renders `--ntp-server <host>`. setup-strih.sh
# step 2 installs the unit + removes any stale `dantesync.service.d/*.conf` drop-in (the live box had a
# hand `10-ntp-master.conf` overriding the client ExecStart -- now folded INTO the unit); verify-strih
# asserts the unit is active, a fresh offset, and (server) an NTP :123 listener. These pure helpers are
# unit-tested in tests/strih_provision_pure_functions.rs.

# strih_dantesync_unit_text ROLE [ARGS] -> print the systemd unit text for the strih-lx dantesync
# daemon (the EXACT cambox unit shape: Type=simple, Restart=always, RestartSec=5,
# WantedBy=multi-user.target). ROLE is `server` (NTP master, the M4 default -- ExecStart is the BARE
# `/usr/local/bin/dantesync`, folding the live 10-ntp-master.conf drop-in into the unit) or `client`
# (ExecStart `/usr/local/bin/dantesync <ARGS>`, ARGS defaulting to the client args). `--service` is
# NOT an installer flag -- it is a run mode, so it never appears here. Fail-closed via
# strih_lx_dantesync_role_ok: an ambiguous shape (server WITH args, client with master/empty args, an
# unknown role) emits NOTHING and returns 1 -- but a plain server role is CORRECT and NEVER refused
# (the post-M4 fact), reversing the pre-M4 "always fail-closed on server mode" behaviour.
strih_dantesync_unit_text() {
  local role="${1:?dantesync role (client|server) required}"
  local args="${2-}"
  local execargs=""
  case "$role" in
    server)
      # NTP master: the bare daemon, no client args (ntp_server_mode lives in config.json).
      strih_lx_dantesync_role_ok "$role" "$args" || return 1
      ;;
    client)
      [ -n "$args" ] || args="$(strih_lx_dantesync_client_args)"
      strih_lx_dantesync_role_ok "$role" "$args" || return 1
      execargs=" ${args}"
      ;;
    *) return 1 ;;
  esac
  cat <<EOF
[Unit]
Description=Dante Time Sync (PTP/NTP Synchronization)
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dantesync${execargs}
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

# --- issue 1317 (this lane): scene-collection hygiene (REPORT-ONLY) + RustDesk install --------------

# strih_collection_hygiene_verdict SHADER_COUNT LUA_COUNT -> the REPORT-ONLY verdict grading the
# active OBS scene collection for boot-noise carriers (issue 1317). The migrated strih-lx collection
# carried 10 `shader_filter` ("User-defined shader") filters (obs-shaderfilter, not shipped in the
# Linux genlock build -> a "Failed to create source" popup at every start) + a dead `scripts-tool`
# Lua (`D:/_APPS/vban-output.lua`); both were stripped LIVE by the owner, but a RE-IMPORTED collection
# would silently re-introduce them. The collection is the OWNER's data -- provisioning NEVER rewrites
# it, this only REPORTS. Prints ONE token; returns 0 ONLY for the clean `ok` state (both counts 0), so
# the caller renders 0->PASS/NOTE else NOTE (the whole item is report-only, never a hard FAIL). Args
# default to 0 (missing = clean). Non-numeric counts fail-closed to the dirty branch. Tokens:
#   ok                        both counts 0
#   shader_filter:N           N shader_filter filters, 0 scripts-tool
#   lua:M                     0 shader_filter, M scripts-tool
#   shader_filter:N,lua:M     both present
strih_collection_hygiene_verdict() {
  local shader="${1:-0}" lua="${2:-0}"
  case "$shader" in ''|*[!0-9]*) shader=1 ;; esac   # non-numeric -> treat as dirty (fail-closed)
  case "$lua" in    ''|*[!0-9]*) lua=1 ;; esac
  if [ "$shader" -eq 0 ] && [ "$lua" -eq 0 ]; then
    printf 'ok'; return 0
  fi
  local msg=""
  [ "$shader" -gt 0 ] && msg="shader_filter:${shader}"
  if [ "$lua" -gt 0 ]; then
    [ -n "$msg" ] && msg="${msg},lua:${lua}" || msg="lua:${lua}"
  fi
  printf '%s' "$msg"; return 1
}

# --- RustDesk remote-desktop (owner request 22.9.2026): PINNED .deb install ------------------------
# The permanent password is NEVER an argument to any of these functions (the @JANUS_ROOM_SECRET@
# discipline): it lives in a 0600 file the supervisor places, and strih_rustdesk_install_cmds reads it
# INSIDE the emitted block. The .deb URL + sha256 are PINNED (confirmed live: the release asset's
# sha256 == the box's installed deb, byte-for-byte).

# strih_rustdesk_version -> the pinned RustDesk version (as live on strih-lx 22.9.2026).
strih_rustdesk_version() { printf '1.4.9'; }

# strih_rustdesk_deb_url -> the pinned RustDesk .deb download URL (the upstream release asset).
strih_rustdesk_deb_url() { printf 'https://github.com/rustdesk/rustdesk/releases/download/1.4.9/rustdesk-1.4.9-x86_64.deb'; }

# strih_rustdesk_deb_sha256 -> the pinned sha256 of that .deb (confirmed == the box's installed deb).
strih_rustdesk_deb_sha256() { printf '7244ba47c40e804172044bfbe659467c54ce46554c98e78c8c0406f1d612fda3'; }

# strih_rustdesk_install_cmds VERSION URL SHA256 PW_FILE -> print the idempotent bash block that
# installs RustDesk from the PINNED .deb and applies the permanent password from PW_FILE (issue 1317).
# Steps: download URL -> a temp .deb, VERIFY its sha256 == SHA256 (fail-loud + remove on mismatch --
# never install an unverified binary), `apt-get install -y <deb>` (pulls libxdo3 etc.), remove the
# temp deb, `systemctl enable --now rustdesk`, then read the password from PW_FILE into a shell var
# and `rustdesk --password` it (the value never appears in this source, in THIS helper's argv, or in a
# log -- only PW_FILE's PATH is an argument to the helper; the supervisor places the 0600 file). One
# unavoidable residue: RustDesk 1.4.x has no stdin/config path for the permanent password, so the
# `rustdesk --password "$__rd_pw"` CALL momentarily exposes the value in the `rustdesk` process argv
# (/proc/<pid>/cmdline) on the box during the brief run -- a CLI limitation, local-only, on a
# single-operator provisioning box; the value is still never in git, in this helper's argv, or logged.
# The block can
# `exit` on any failure, so the caller consumes it as `( eval "$(strih_rustdesk_install_cmds ...)" ) ||
# fail`. Every URL/SHA/PW_FILE arg is %q-quoted; the final statement ends with `;` so a `$(...)`
# embedding never glues a following command (the v4l2-neutral.sh _cmd-helper gotcha). VERSION is used
# only to name the temp file (provenance).
strih_rustdesk_install_cmds() {
  local version="${1:?rustdesk version required}" url="${2:?rustdesk .deb url required}"
  local sha="${3:?rustdesk .deb sha256 required}" pwfile="${4:?rustdesk password-file path required}"
  cat <<EOF
__rd_deb="\$(mktemp "/tmp/rustdesk-${version}.XXXXXX.deb")" || { echo "rustdesk: mktemp failed" >&2; exit 1; };
curl -fsSL -o "\$__rd_deb" $(printf '%q' "$url") || { echo "rustdesk: download failed ($url)" >&2; rm -f "\$__rd_deb"; exit 1; };
__rd_got="\$(sha256sum "\$__rd_deb" | awk '{print \$1}')";
if [ "\$__rd_got" != $(printf '%q' "$sha") ]; then echo "rustdesk: sha256 mismatch (want ${sha}, got \$__rd_got) -- refusing to install" >&2; rm -f "\$__rd_deb"; exit 1; fi;
DEBIAN_FRONTEND=noninteractive apt-get install -y "\$__rd_deb" || { echo "rustdesk: apt install failed" >&2; rm -f "\$__rd_deb"; exit 1; };
rm -f "\$__rd_deb";
systemctl enable --now rustdesk || { echo "rustdesk: enable --now failed" >&2; exit 1; };
__rd_pw="\$(cat $(printf '%q' "$pwfile"))" || { echo "rustdesk: cannot read password file ${pwfile}" >&2; exit 1; };
if [ -z "\$__rd_pw" ]; then echo "rustdesk: password file ${pwfile} is empty" >&2; unset __rd_pw; exit 1; fi;
rustdesk --password "\$__rd_pw" || { echo "rustdesk: setting the permanent password failed" >&2; unset __rd_pw; exit 1; };
unset __rd_pw;
EOF
}

# --- issue 1346: fixed HDMI fullscreen projector acceptance (REPORT-ONLY) --------------------------
# The owner ROZHODNUTE (19.9.): the strih-lx HDMI output is an OBS fullscreen projector (Program or
# Multiview), PERSISTED via SaveProjectors=true and re-opened on every launch. verify-strih.sh reports
# (never hard-fails, since the live open needs an HDMI display on the notebook): SaveProjectors=true is
# pre-seeded in user.ini, and -- when an external monitor is connected -- a saved ProjectorType 3/4
# entry exists.

# strih_projector_verdict SAVEPROJ_PRESENT EXT_CONNECTED SAVED_ENTRY_PRESENT -> print ONE verdict
# token and return 0 ONLY for the fully-configured `ok` state; every other state returns non-zero so
# the caller renders 0->PASS else NOTE (the whole item is report-only -- it never hard-FAILs the gate).
#   arg1 SAVEPROJ_PRESENT:     1 iff user.ini has SaveProjectors=true
#   arg2 EXT_CONNECTED:        1 iff an external (HDMI/DP) monitor is connected (a /sys/class/drm status)
#   arg3 SAVED_ENTRY_PRESENT:  1 iff the current scene collection's saved_projectors has a type-3/4 entry
# Fail-closed order (missing args default to 0 = not configured):
#   saveprojectors-missing  -> SaveProjectors not pre-seeded (setup-strih step 7 not applied)
#   hdmi-absent             -> SaveProjectors ok but no external monitor connected (expected today)
#   projector-unseeded      -> external monitor present but no saved projector entry yet
#   ok                      -> SaveProjectors true + external monitor + a saved ProjectorType 3/4
strih_projector_verdict() {
  local saveproj="${1:-0}" ext="${2:-0}" saved="${3:-0}"
  [ "$saveproj" = 1 ] || { printf 'saveprojectors-missing'; return 1; }
  [ "$ext" = 1 ]      || { printf 'hdmi-absent';            return 1; }
  [ "$saved" = 1 ]    || { printf 'projector-unseeded';     return 1; }
  printf 'ok'; return 0
}

# --- issue 1345 M3a: Janus audiobridge audio edge (enable-only) ------------------------------------
# The phones intercom leg moves off VDO.Ninja onto a Janus audiobridge room the hub joins as a
# plain-RTP PCMU participant. setup-strih.sh installs Janus (apt), writes these two jcfg files, and
# `systemctl enable janus` (NEVER start -- the M4 cut-over starts it with the hub). These are the PURE
# config renderers (unit-tested in tests/strih_provision_pure_functions.rs); the room SECRET is NEVER
# an argument here -- it is a placeholder the caller substitutes from the 0600 file with a bash var
# (no argv exposure, never logged).

# strih_janus_audiobridge_jcfg_text ROOM SECRET_PATH [LOCAL_IP] -> print the audiobridge plugin jcfg
# for room ROOM named "interkom" (48 kHz, record off, plain-RTP participants allowed). The `secret`
# line is a `@JANUS_ROOM_SECRET@` PLACEHOLDER the caller replaces with the value read from SECRET_PATH
# (named in a provenance comment only) -- so this pure text never carries the secret.
# issue 1352: LOCAL_IP (the box's static IP, from the same source of truth as the netplan step --
# strih_lx_ip) PINS the audiobridge's plain-RTP bind via `local_ip` in `general`. Without it Janus
# binds RTP to whatever IP it auto-detected at start, so the .203->.202 renumber left every bind
# EADDRNOTAVAIL and the intercom hub could never join room ROOM. A blank/omitted LOCAL_IP leaves
# `general` empty (Janus default all-interfaces) -- the caller passes it (the `ws_ip` blank-omits idiom).
strih_janus_audiobridge_jcfg_text() {
  local room="${1:?room id required}" secret_path="${2:?secret path required}" local_ip="${3:-}"
  local ip_line=""
  if [ -n "$local_ip" ]; then
    ip_line="    local_ip = \"${local_ip}\""
  fi
  cat <<EOF
# strih-lx intercom audiobridge -- GENERATED by setup-strih.sh (issue 1345 M3a; issue 1352 pins local_ip). DO NOT EDIT BY HAND.
# The room secret is injected from ${secret_path} at provisioning (never in git, never logged).
general: {
${ip_line}
}

room-${room}: {
    description = "interkom"
    secret = "@JANUS_ROOM_SECRET@"
    sampling_rate = 48000
    record = false
    allow_rtp_participants = true
}
EOF
}

# strih_janus_ws_jcfg_text LAN_IP -> print the Janus WebSocket transport jcfg: ws BOUND to the LAN
# address on :8188 (ws_ip), NO wss (TLS terminates on the dev1 front), admin API off. The dev1 nginx
# front reaches the WS over the LAN; the loopback probe + the hub's plain-RTP session use the HTTP
# transport (127.0.0.1:8088), so binding WS to the LAN IP (not 0.0.0.0/all interfaces) never removes a
# needed path. A blank LAN_IP omits ws_ip (Janus default all-interfaces) -- the caller always passes it.
strih_janus_ws_jcfg_text() {
  local lan_ip="${1:-}"
  local ws_ip_line=""
  if [ -n "$lan_ip" ]; then
    ws_ip_line="    ws_ip = \"${lan_ip}\""
  fi
  cat <<EOF
# strih-lx intercom Janus WebSocket transport -- GENERATED by setup-strih.sh (issue 1345 M3a).
# ws BOUND to ${lan_ip} :8188 (the dev1 front reaches it over the LAN), NO wss (TLS on the front); no admin API.
general: {
    json = "indented"
    ws = true
    ws_port = 8188
${ws_ip_line}
    wss = false
    admin_ws = false
    admin_wss = false
}
EOF
}

# strih_janus_http_jcfg_text -> print the Janus HTTP transport jcfg: the HTTP API bound to LOOPBACK
# 127.0.0.1:8088 ONLY (ip = "127.0.0.1"). The hub's plain-RTP session + local packet probes use it;
# the phone never touches HTTP (it uses the WS front). NO https (no TLS here), admin API off. This is
# the issue-1345 M3 follow-up (d): Janus HTTP defaulted to *:8088 -- tighten it in the jcfg.
strih_janus_http_jcfg_text() {
  cat <<'EOF'
# strih-lx intercom Janus HTTP transport -- GENERATED by setup-strih.sh (issue 1345 M3a).
# HTTP API bound to LOOPBACK 127.0.0.1:8088 only (hub janus_rtp session + local probes); NO https,
# admin API off. The phone reaches Janus over the WS front, never HTTP.
general: {
    json = "indented"
    base_path = "/janus"
    http = true
    port = 8088
    ip = "127.0.0.1"
    https = false
}
admin: {
    admin_http = false
    admin_https = false
}
EOF
}

# strih_janus_room_jcfg_ok ROOM  (stdin: audiobridge jcfg text) -> 0 iff it declares room-<ROOM> as
# the "interkom" room at 48 kHz with plain-RTP participants allowed. A pure grep check (no janus
# binary invocation), used report-only by verify-strih.sh. Here-strings (not `printf | grep`) so an
# early `grep -q` match never SIGPIPEs under the caller's `set -euo pipefail` (the drift-guard gotcha).
strih_janus_room_jcfg_ok() {
  local room="${1:?room id required}" text
  text="$(cat)"
  grep -qE "^room-${room}:" <<<"$text" || return 1
  grep -qE 'sampling_rate[[:space:]]*=[[:space:]]*48000' <<<"$text" || return 1
  grep -qE 'allow_rtp_participants[[:space:]]*=[[:space:]]*true' <<<"$text" || return 1
  grep -qE 'description[[:space:]]*=[[:space:]]*"interkom"' <<<"$text" || return 1
  return 0
}

# --- issue 1317 (this lane): CPU performance governor + never-sleep (low-latency cutter) -----------
# The strih-lx notebook is the fleet's low-latency genlock OBS cutter; a distro-default powersave /
# schedutil governor is wrong for it (the imag-nb + cam-box fleet pin `performance` explicitly --
# setup-device.sh STEP-13, .claude/rules/realtime-isolation.md + the imag power-envelope rule). The
# owner caught the missing step live on the notebook (no performance mode). setup-strih.sh evals
# strih_performance_mode_apply, then writes+enables strih_cpu_performance_unit_text for persistence.

# strih_performance_mode_apply -> print the idempotent statements the caller evals to put the box in
# performance mode NOW. It runs BOTH power-profiles-daemon (`powerprofilesctl set performance`, when
# present) AND writes the `performance` governor to every CPU's scaling_governor -- NOT either/or
# (issue 1317, live 20.9.2026): on intel_pstate ACTIVE (this Lenovo Raptor Lake) `powerprofilesctl`
# sets EPP=performance but leaves `scaling_governor=powersave`, so a `; else <governor> ; fi` shape
# never wrote the governor and `verify (perf)` (governor==performance on all cores) FAILed. The two
# are complementary -- ppd owns EPP, the governor loop owns the governor -- so both run. Then mask the
# sleep/suspend targets (the setup-device.sh STEP-13 shape). Each statement is `;`-terminated and
# fail-soft (`|| true`) so a core/daemon that refuses never aborts the caller's set -euo pipefail;
# failing LOUD is the verify (perf) gate's job, not this apply.
strih_performance_mode_apply() {
  cat <<'CMD'
if command -v powerprofilesctl >/dev/null 2>&1 && powerprofilesctl list >/dev/null 2>&1; then powerprofilesctl set performance || true; fi;
for __gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do [ -w "$__gov" ] && echo performance > "$__gov" 2>/dev/null || true; done;
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target 2>/dev/null || true;
CMD
}

# strih_perf_effective_line GOV EPP PPD -> the effective-state log line for setup-strih.sh step 15,
# reporting the TRIPLE governor / EPP / ppd profile it actually observed AFTER the apply. This
# replaces the old self-contradicting "set to performance (now: powersave)" line (issue 1317): the
# apply now writes the governor unconditionally, so the step reports what stuck on all three facets.
# Missing/unreadable facets default (never a bare empty triple).
strih_perf_effective_line() {
  local gov="${1:-unreadable}" epp="${2:-absent}" ppd="${3:-absent}"
  printf 'governor=%s / EPP=%s / ppd=%s' "$gov" "$epp" "$ppd"
}

# strih_cpu_performance_unit_text -> print the persistent CPU-performance systemd oneshot unit that
# re-applies the `performance` governor on every boot (the setup-device.sh cpu-performance.service
# shape: Type=oneshot, RemainAfterExit=yes, WantedBy=multi-user.target). setup-strih.sh writes it to
# /etc/systemd/system/cpu-performance.service and enables it as the persistence backstop, so the
# governor survives a reboot even where power-profiles-daemon is absent.
strih_cpu_performance_unit_text() {
  cat <<'EOF'
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
}

# strih_verify_governor_ok  (stdin: each online core's scaling_governor value, one per line -- what
# `cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor` prints) -> 0 iff EVERY line reads
# `performance` AND there is at least one line. Fail-closed: an empty/unreadable input returns 1 (an
# unreadable governor is a FAIL, never a silent pass -- test-strictness). Drain-safe (reads the whole
# stream). verify-strih.sh pairs it with the existing strih_verify_sleep_masked for the (perf) item.
strih_verify_governor_ok() {
  local line seen=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    seen=1
    [ "$line" = performance ] || return 1
  done
  [ "$seen" = 1 ]
}

# --- issue 1317 (this lane): Bitfocus Companion Satellite (the Stream Deck surface agent) ----------
# The strih-lx notebook exposes its locally-attached Stream Deck to the VENUE's Companion CONTROLLER
# (10.77.9.205, the strih-autorecord-coupling box) as a headless SATELLITE -- NOT a second full
# Bitfocus Companion (a second controller would fork the venue's button/page state and fight the
# real one). The owner caught the missing step live (no Companion Satellite -> dead Stream Deck).
#
# DESKTOP TARBALL MODEL (corrected after the integration review bounce): Bitfocus does NOT ship a
# .deb from GitHub releases -- GitHub releases carry no .deb assets. Linux x64 is an Electron desktop
# app distributed as a .tar.gz from the Bitfocus CDN. The tarball extracts a `companion-satellite-x64/`
# dir carrying the binary, its own `install.sh`, and the `50-satellite-desktop.rules` (uaccess) udev
# rule. setup-strih.sh evals strih_companion_satellite_install (download the PINNED tar.gz, sha256
# verify, run its own `install.sh --system --force` which installs to /opt + the desktop udev rule),
# seeds the operator's app config.json (electron-store: remoteIp/remotePort) + writes an operator-
# login autostart entry (never a mid-provision start), and records the intended controller host in
# host.conf. verify-strih.sh grades it via strih_companion_verdict. The version + tarball + sha256 are
# REPRODUCIBLE pins (like the dantesync/NDI pins), never "latest"; COMPANION_SATELLITE_VERSION /
# COMPANION_SATELLITE_TARBALL_URL / COMPANION_SATELLITE_SHA256 / COMPANION_SATELLITE_HOST /
# COMPANION_SATELLITE_PORT override them so the supervisor can confirm/repin against the live box.

# strih_companion_satellite_version -> the PINNED Companion Satellite stable release (reproducible;
# never "latest"). COMPANION_SATELLITE_VERSION overrides. Supervisor confirms/bumps the pin against
# the live box (see the LANE-RETURN followup).
strih_companion_satellite_version() { printf '%s' "${COMPANION_SATELLITE_VERSION:-3.4.0}"; }

# strih_companion_satellite_tarball_url -> the pinned Bitfocus CDN x64 .tar.gz URL. The build hash in
# the filename (722-8bc2f14) is NOT derivable from the version, so the URL is a whole pinned constant
# (from the Bitfocus packages listing API, branch=stable). COMPANION_SATELLITE_TARBALL_URL overrides
# the whole URL so the supervisor can repin a new build with no code change.
strih_companion_satellite_tarball_url() {
  printf '%s' "${COMPANION_SATELLITE_TARBALL_URL:-https://cf-pub.bitfocus.io/companion/companion-satellite/companion-satellite-x64-722-8bc2f14.tar.gz}"
}

# strih_companion_satellite_sha256 -> the pinned sha256 of the tarball (the install emitter verifies
# the download against it, fail-loud on mismatch). COMPANION_SATELLITE_SHA256 overrides (must be
# repinned in lock-step with COMPANION_SATELLITE_TARBALL_URL).
strih_companion_satellite_sha256() {
  printf '%s' "${COMPANION_SATELLITE_SHA256:-32b8b443d1c595e91ee733b929e20cb8abb8c6119975ba7bb98730c925a2a953}"
}

# strih_companion_satellite_bin -> the launch path install.sh --system installs to (single source).
strih_companion_satellite_bin() { printf '/opt/companion-satellite/companion-satellite'; }

# strih_companion_satellite_udev_rule -> the desktop (uaccess) udev rule install.sh --system drops in.
strih_companion_satellite_udev_rule() { printf '/etc/udev/rules.d/50-satellite-desktop.rules'; }

# strih_companion_satellite_host -> the venue Companion CONTROLLER host the satellite connects to.
# COMPANION_SATELLITE_HOST overrides (default 10.77.9.205).
strih_companion_satellite_host() { printf '%s' "${COMPANION_SATELLITE_HOST:-10.77.9.205}"; }

# strih_companion_satellite_port -> the controller TCP port (Satellite DEFAULT_TCP_PORT).
# COMPANION_SATELLITE_PORT overrides (default 16622).
strih_companion_satellite_port() { printf '%s' "${COMPANION_SATELLITE_PORT:-16622}"; }

# strih_companion_satellite_config_text HOST -> the durable /etc record of the INTENDED controller
# host, human-readable. This is the operator/supervisor's paper trail; the FUNCTIONAL seed the app
# actually reads is the JSON below.
strih_companion_satellite_config_text() {
  local host="${1:?host required}"
  cat <<EOF
# strih-lx Bitfocus Companion Satellite -- GENERATED by setup-strih.sh (issue 1317). DO NOT EDIT BY HAND.
# The venue Companion CONTROLLER this satellite exposes its locally-attached Stream Deck to.
COMPANION_SATELLITE_HOST=${host}
EOF
}

# strih_companion_satellite_appconfig_json HOST [PORT] -> the electron-store config.json the desktop
# build reads. Confirmed from the Satellite v3.4.0 source (satellite/src/config.ts): the controller is
# keyed as `remoteIp` + `remotePort` (NOT host/companionAddress), `remoteProtocol` tcp; ensureFields-
# Populated only fills MISSING keys, so a pre-written file is respected before first launch. The setup
# step writes this to the operator's `~/.config/Companion Satellite/config.json`.
strih_companion_satellite_appconfig_json() {
  local host="${1:?host required}" port="${2:-16622}"
  cat <<EOF
{
  "remoteProtocol": "tcp",
  "remoteIp": "${host}",
  "remotePort": ${port}
}
EOF
}

# strih_companion_satellite_autostart_text -> the operator-login autostart Desktop Entry. Owner rule:
# a needed feature is always-ON by default, never a forgettable manual launch. The setup step writes
# this to the operator's `~/.config/autostart/companion-satellite.desktop`.
strih_companion_satellite_autostart_text() {
  cat <<EOF
[Desktop Entry]
Type=Application
Name=Companion Satellite
Comment=Bitfocus Companion Satellite -- exposes the local Stream Deck to the venue controller (issue 1317)
Exec=$(strih_companion_satellite_bin)
Terminal=false
X-GNOME-Autostart-enabled=true
Hidden=false
EOF
}

# strih_companion_satellite_install -> print the idempotent statements the caller evals (in a subshell,
# `( eval "$(...)" ) || fail`, because it `exit 1`s on failure) to install Companion Satellite the
# DESKTOP way: if the /opt binary is absent, install the documented deps, download the PINNED tar.gz,
# VERIFY its sha256 (fail-loud on mismatch), extract, and run the tarball's OWN `install.sh --system
# --force` (idempotent; installs to /opt + the desktop udev rule + the app-menu entry). A re-run with
# the binary already present is a pure no-op (deps + download + install.sh all inside the absence
# guard). NEVER a .deb, NEVER `systemctl start`/`enable` -- the desktop build has no system unit; the
# operator-login autostart (written separately by the caller) launches it. Each statement is
# `;`-terminated (the _cmd-embedding trailing-newline-strip gotcha).
strih_companion_satellite_install() {
  local url sha bin deps
  url="$(strih_companion_satellite_tarball_url)"
  sha="$(strih_companion_satellite_sha256)"
  bin="$(strih_companion_satellite_bin)"
  # issue 1317 (live 20.9.2026): a fresh strih-lx has NO curl, and this emitter's own download uses
  # `curl -fsSL` -- so step 16 FAILed on the first live run. Install curl in the SAME dep line (the
  # setup-device.sh precedent installs curl before any download).
  deps='curl libusb-1.0-0-dev libudev-dev libfontconfig1'
  cat <<CMD
if [ ! -x ${bin} ]; then DEBIAN_FRONTEND=noninteractive apt-get install -y ${deps} || { echo "companion-satellite: dependency install failed (${deps})" >&2; exit 1; }; __cs_dir="\$(mktemp -d)"; __cs_tgz="\$__cs_dir/companion-satellite.tar.gz"; if ! curl -fsSL ${url} -o "\$__cs_tgz"; then rm -rf "\$__cs_dir"; echo "companion-satellite: download failed (${url})" >&2; exit 1; fi; if ! echo "${sha}  \$__cs_tgz" | sha256sum -c - >/dev/null 2>&1; then rm -rf "\$__cs_dir"; echo "companion-satellite: sha256 mismatch (expected ${sha}) -- repin COMPANION_SATELLITE_TARBALL_URL/COMPANION_SATELLITE_SHA256" >&2; exit 1; fi; if ! tar -xzf "\$__cs_tgz" -C "\$__cs_dir"; then rm -rf "\$__cs_dir"; echo "companion-satellite: tarball extract failed" >&2; exit 1; fi; __cs_ish="\$(find "\$__cs_dir" -maxdepth 2 -name install.sh -type f | head -1)"; if [ -z "\$__cs_ish" ] || [ ! -f "\$__cs_ish" ]; then rm -rf "\$__cs_dir"; echo "companion-satellite: install.sh not found in tarball" >&2; exit 1; fi; if ! ( cd "\$(dirname "\$__cs_ish")" && bash install.sh --system --force ); then rm -rf "\$__cs_dir"; echo "companion-satellite: install.sh --system --force failed" >&2; exit 1; fi; rm -rf "\$__cs_dir"; fi;
CMD
}

# strih_companion_verdict INSTALLED UDEV AUTOSTART HOST_OK -> print ONE verdict token and return 0 iff
# the fully-configured `ok` state. Fail-closed order (missing args default to 0 = not configured):
# not-installed -> no-udev-rule -> no-autostart -> wrong-host -> ok. verify-strih.sh feeds the live
# /opt-binary / desktop-udev-rule / operator-autostart / seeded-controller reads and PASSes only on
# `ok`, FAILing loud otherwise (test-strictness, like the other verify items).
strih_companion_verdict() {
  local installed="${1:-0}" udev="${2:-0}" autostart="${3:-0}" host_ok="${4:-0}"
  [ "$installed" = 1 ] || { printf 'not-installed'; return 1; }
  [ "$udev" = 1 ]      || { printf 'no-udev-rule';  return 1; }
  [ "$autostart" = 1 ] || { printf 'no-autostart';  return 1; }
  [ "$host_ok" = 1 ]   || { printf 'wrong-host';    return 1; }
  printf 'ok'; return 0
}

# strih_companion_satellite_rest_url -> the Satellite's local REST/web base URL (the desktop build
# serves its web UI + REST API on :9999). COMPANION_SATELLITE_REST_URL overrides for a non-default
# port. The setup step POSTs the controller here; the verify item reads .connected here.
strih_companion_satellite_rest_url() {
  printf '%s' "${COMPANION_SATELLITE_REST_URL:-http://127.0.0.1:9999}"
}

# strih_companion_satellite_rest_apply_cmd HOST PORT -> emit the idempotent statements the caller evals
# AFTER seeding the app config.json (issue 1317, live 20.9.2026). The electron-store file seed alone
# left the RUNNING Satellite's EFFECTIVE controller host at 127.0.0.1 until this POST -- after
# `POST /api/config {"host","port","protocol":"tcp"}` on its local REST (:9999) `GET /api/status`
# read `connected:true`. Best-effort: guarded on the REST answering (a fresh box that seeds but never
# started the Satellite is a clean no-op), every statement `;`-terminated and `|| true` so it never
# aborts the caller's set -euo pipefail. Uses the REST keys `host`/`port`/`protocol` (NOT the
# electron-store remoteIp/remotePort).
strih_companion_satellite_rest_apply_cmd() {
  local host="${1:?host required}" port="${2:-16622}" rest
  rest="$(strih_companion_satellite_rest_url)"
  cat <<CMD
if curl -fsS --max-time 2 ${rest}/api/status >/dev/null 2>&1; then curl -fsS --max-time 5 -X POST -H 'Content-Type: application/json' -d '{"host":"${host}","port":${port},"protocol":"tcp"}' ${rest}/api/config >/dev/null 2>&1 || true; fi;
CMD
}

# strih_companion_status_verdict FILE_VERDICT RUNNING CONNECTED -> the live-aware (companion) verdict.
# RUNNING=1 iff the Satellite REST (:9999/api/status) answered; CONNECTED=1 iff that answer's
# `.connected == true`. When the Satellite is NOT up (RUNNING!=1 -- a fresh box that seeds but never
# starts it) this is a FILE-ONLY check: echo FILE_VERDICT and pass/fail on whether it is `ok`. When it
# IS up the live controller link must be established: connected -> `ok-connected` (rc 0), else
# `not-connected` (rc 1). verify-strih.sh feeds the file verdict + the live reads.
strih_companion_status_verdict() {
  local fileverdict="${1:-not-installed}" running="${2:-0}" connected="${3:-0}"
  if [ "$running" != 1 ]; then
    printf '%s' "$fileverdict"
    [ "$fileverdict" = ok ] && return 0 || return 1
  fi
  if [ "$connected" = 1 ]; then printf 'ok-connected'; return 0; fi
  printf 'not-connected'; return 1
}

# --- issue 1353: bkshading SERVICE (shading panel backend) provisioning on strih-lx ----------------
# The shading-control panel backend (bkshading/service) ran only on the Windows strih PC; post-M4 the
# notebook is the strih, so it is provisioned here as a systemd unit fed the CI-built Linux artifact.
# The panel web assets are EMBEDDED in the binary (bkshading/service/src/http.rs include_str!), so the
# unit needs only the self-contained binary; web/ ships beside it per the design as a panel-source copy.

# strih_bkshading_artifact_name -> the CI artifact NAME the ci.yml Linux job uploads and setup-strih
# fetches. SINGLE SOURCE OF TRUTH (KEEP IN SYNC with .github/workflows/ci.yml's
# `Upload bkshading service (linux, panel)` step) -- the strih sibling of the Windows
# `bkshading-windows-amd64` canon in scripts/lib/bkshading-deploy-service-runtime.sh.
strih_bkshading_artifact_name() { printf '%s\n' bkshading-service-linux-amd64; }

# strih_bkshading_unit_text -> the systemd SYSTEM unit for the bkshading panel service on strih-lx.
# User=newlevel (the operator session owns libndi/PipeWire parity for the M2 live NDI preview),
# ExecStart the installed self-contained binary + --config, Restart=on-failure (a genuine crash
# restarts; a clean operator/deploy stop does not loop), multi-user target. NO drop-ins, NO self-start
# -- enable-only is the installer's job (setup-strih step 16c); the SUPERVISOR starts the running
# service. The committed systemd/bkshading-service.service is byte-identical to this (parity-tested).
strih_bkshading_unit_text() {
  cat <<'EOF'
[Unit]
Description=bkshading shading-control panel service (issue 808 / issue 1353)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=newlevel
ExecStart=/opt/bkshading/bkshading --config /etc/bkshading/bkshading.toml
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

# strih_bkshading_config_text [BIND] -> the operator config seed for /etc/bkshading/bkshading.toml.
# BIND defaults to the service's own default_bind (bkshading/service/src/config.rs "0.0.0.0:8770").
# The camera/relay/preview DATA is pinned from the canonical bkshading/service/bkshading.example.toml
# (the SAME schema the Windows install was seeded from -- the live Windows C:\bkshading\bkshading.toml
# was unreadable, the PC being off). Development = one camera on cam1; the handheld blocks are the
# params-only placeholders for the issue-808 SBC milestone. Relay addresses are host-independent
# (<cambox>.lan:8771 -- the service connects OUT to each cambox relay, so no cambox repoint is needed).
# Seeded ONLY IF absent (setup-strih step 16c [ ! -f ] guard) so the operator's live edits are kept.
strih_bkshading_config_text() {
  local bind="${1:-0.0.0.0:8770}"
  cat <<EOF
# bkshading service config -- strih-lx (issue 1353). Seeded from the canonical
# bkshading/service/bkshading.example.toml (the schema the Windows install used); edit the live
# camera/relay set on the box. Development = one camera on cam1; the handheld blocks are the
# params-only placeholders for the issue-808 SBC milestone.

# Web panel bind address (the operator opens http://strih.lan:8770/).
bind = "${bind}"

# Live-preview tuning (M2). Optional -- every field defaults sensibly.
[preview]
fps = 3.0
jpeg_quality = 55
capture_timeout_ms = 1000
reconnect_backoff_ms = 2000

# cam1: camera USB -> cambox cam1, controlled by bkshading-relay on cam1.
[[camera]]
id = "cam1"
label = "Cam 1"
transport = "cambox-relay"
address = "cam1.lan:8771"
ndi_preview = "CAM1 (usb)"
grab_fps = 60

# Handheld cameras (x3) on a separately powered arm64 SBC running the SAME relay -- no video feed,
# so a params-only block (no ndi_preview, no grab_fps). Placeholders for the issue-808 SBC milestone.
[[camera]]
id = "handheld-1"
label = "Handheld 1"
transport = "sbc-relay"
address = "handheld-1.lan:8771"

[[camera]]
id = "handheld-2"
label = "Handheld 2"
transport = "sbc-relay"
address = "handheld-2.lan:8771"

[[camera]]
id = "handheld-3"
label = "Handheld 3"
transport = "sbc-relay"
address = "handheld-3.lan:8771"
EOF
}
