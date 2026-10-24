#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the per-box step gate + the stream-role renderers, no
# top-level statements besides constants) -- the scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell, so strict mode here would leak into it. setup-strih.sh / verify-strih.sh set their own.
#
# scripts/lib/obs-box-role.sh -- issue 1412: which setup-strih steps / verify-strih items run on the
# LOADED box, and what the STREAM role writes instead of the strih one. ONE provisioner serves every
# Linux OBS box (the unified-design ruling, umbrella issue 1357), so a step that does not apply to a
# box is skipped by a box FACT here, never by a box name and never by a copy of the script.
# setup-strih.sh and verify-strih.sh carry one gate line per step:
#
#   if WHY="$(obs_box_step_applies <step>)"; then <the step> else <a SKIP line naming WHY>; fi
#
# The role -> step table (STRIH_ROLE):
#
#   step    strih            stream
#   3       NVENC precheck   the stream encoder by GPU (VAAPI without an NVIDIA dGPU) + the profile
#   6       strih manifest   one genlocked `NDI 2ME PGM` <- STRIH_PROGRAM_SENDER (class "program"),
#                            program_audio_input STRIH_PROGRAM_AUDIO_INPUT (stream-pp `Zvuk z mixu`,
#                            stream-snv `mbc`, issue 1462) = the obs-vban source on the console's VBAN
#                            stream (program_audio_vban, issue 1439); no HDMI output (backend none)
#   7       2ME PGM + PVW    one NDI output `<prefix> (stream)`, no preview output
#   12      strih-program    the operator's headphones through a LOCKED PipeWire (issue 1465 item 3,
#           graph, MiniFuse  scripts/lib/stream-operator-audio.sh: the strih-role drop-ins are removed);
#                            the seed manifest forbids pulse capture inputs (`"pulse_capture": "forbid"`)
#   13      intercom hub     skipped, and a hub left on the box is moved aside (issue 1439: the console's
#                            VBAN goes straight into OBS; 4b / 12 write no hub drop-in, verify 16 = no hub)
#   14      Janus            skipped (no phones leg on a stream box)
#   16      Satellite        skipped (no Stream Deck)
#   16c     bkshading        skipped (no shading panel)
#   16d     4 session apps   the browser-source keeper, the headphone monitor of the outgoing stream
#                            (issue 1436) and the dantesync tray
#   16f     tally            skipped
#   avsync-dock  nothing     step 16g seeds the A/V-sync dock into the saved OBS layout, verify item 47
#                            grades that layout (issue 1470, scripts/lib/obs-box-avsync-dock.sh; the
#                            plugin rows of item 47 run on every role)
#   16i     HTTPS front      skipped (no intercom / shading page; the facts declare none, issue 1399)
#   13-hub-live  verify item 16: an inactive hub FAILs   report-only (issue 1418)
#
# Independent of the role: 11b only for a USB rig NIC, 16e only with STRIH_PROGRAM_AUDIO_SAMPLER=on, and
# a venue other than snv lists its own boxes' STRIH_IP as the NDI senders (step 4b / verify item 34).
#
# The facts come from scripts/strih-boxes/<box>.env through scripts/lib/strih-box-facts.sh.

# The stream role's names: the NDI program input (the SNV stream box's input name), the scene it lives
# in (the ported collection's program scene), and the bare DistroAV main output name (DistroAV prepends
# the hostname -> `STREAM-PP (stream)`). The program audio input's name is a box fact
# (STRIH_PROGRAM_AUDIO_INPUT, issue 1462: the SNV and PP collections name it differently).
OBS_BOX_STREAM_INPUT="NDI 2ME PGM"
OBS_BOX_STREAM_SCENE="PRO"
# issue 1436: the seeder keeps the program audio input's OBS monitoring OFF -- the stream box's headphones play the
# real outgoing stream (strih-stream-monitor.service), never one monitored source.
OBS_BOX_STREAM_AUDIO_MONITOR=off
OBS_BOX_STREAM_OUTPUT_NAME="stream"
# The stream role's session apps, in start order (obs_box_role_apply below; the genlock deploy's start
# step reads the same list for a stream-role box, issue 1462).
OBS_BOX_STREAM_SESSION_APP_UNITS=(strih-browser-keeper.service strih-stream-monitor.service dantesync-tray.service strih-display-watch.service)
# issue 1380 (ROZHODNUTE 13.10.2026): the stream role's YouTube-safe Development scene -- the program
# video input (muted) over the measurement clip, never the production scene (it holds the live mixer).
# The scene name equals STREAM_DEV_SCENE_DEFAULT (scripts/lib/stream-dev-scene.sh, pinned by a pytest).
OBS_BOX_STREAM_DEV_SCENE="Development"
OBS_BOX_STREAM_DEV_CLIP_INPUT="Merací klip"
OBS_BOX_STREAM_DEV_CLIP="/opt/camera-box/measurement-clip-v1.mp4"
OBS_BOX_STREAM_DEV_CLIP_FRAMES=3840   # gen_measurement_clip.py: 128 s x 30 fps
# The local restreamer the stream OBS sends to (the stream-pp laptop runs it; recon 11.10.2026).
OBS_BOX_STREAM_RTMP_SERVER="${OBS_BOX_STREAM_RTMP_SERVER:-rtmp://127.0.0.1:1234/live}"

# obs_box_stream_key -> the stream role's OBS stream key = the box name (issue 1436): OBS publishes to
# <OBS_BOX_STREAM_RTMP_SERVER>/<key>, and strih-stream-monitor.service plays rtmp://127.0.0.1:1234/live/%l
# (%l = the short hostname setup step 1 sets to the same fact). rc 1 when the fact cannot be read.
obs_box_stream_key() { strih_lx_hostname; }

# obs_box_step_applies STEP -> rc 0 when the loaded box runs STEP; rc 1 with the reason on stdout when
# it skips it. A step with no rule here always runs. A fact that cannot be read never skips a step.
#   11b -- the NIC IRQ unit resolves the rig NIC's XHCI interrupt, so it exists only for a USB rig NIC
#          (STRIH_NIC_MIN_USB_MBPS != none). On a PCIe NIC (strih-pp's RTL8125) the walk to a usbN root
#          would fail the unit at every boot (gap G7).
#   16e -- the YouTube-guard stream program-audio sampler (issue 1404) reads the SNV stream output, so
#          only a box with STRIH_PROGRAM_AUDIO_SAMPLER=on runs it (gap G11).
#   3-nvenc / 12-strih-audio / 13 / 14 / 16 / 16c / 16f / 16i / 13-hub-live -- the strih role only (the table above; 13 = the
#          intercom hub itself, issue 1439; 16f = the camera tally, issue 1417; 16i = the box's own HTTPS
#          front, issue 1399; 13-hub-live = verify item 16 FAILs an inactive or disabled hub, issue 1418;
#          12-strih-audio = setup step 12's strih-program graph + MiniFuse drop-ins and verify item 9b's
#          grade of them, issue 1465 item 3: the stream role gets the locked operator PipeWire instead).
#   6-hdmi -- only a box with a fixed HDMI output (STRIH_HDMI_OUTPUT_BACKEND != none).
#   avsync-dock -- the stream role only: the A/V-sync dock reads the measurement audio there (issue 1470).
obs_box_step_applies() {
  local step="${1:?step required}" v
  case "$step" in
    11b)
      v="$(strih_lx_nic_min_usb_mbps)" || return 0
      if [ "$v" = none ]; then
        printf 'the rig NIC is not a USB device (STRIH_NIC_MIN_USB_MBPS=none) -- no xhci IRQ to place'
        return 1
      fi ;;
    16e)
      v="$(strih_lx_program_audio_sampler)" || return 0
      if [ "$v" != on ]; then
        printf 'STRIH_PROGRAM_AUDIO_SAMPLER=%s -- this box runs no stream program-audio sampler' "$v"
        return 1
      fi ;;
    3-nvenc | 12-strih-audio | 13 | 14 | 16 | 16c | 16f | 16i | 16k | 13-hub-live)
      v="$(strih_box_role)" || return 0
      if [ "$v" = stream ]; then
        case "$step" in
          3-nvenc) printf 'the stream role encodes with the GPU it has (VAAPI without an NVIDIA dGPU), not the strih NVENC' ;;
          12-strih-audio) printf 'the stream role plays the operator headphones through a locked PipeWire (issue 1465 item 3), never the strih-program graph' ;;
          13) printf 'the stream role runs no intercom hub: the console VBAN goes straight into the OBS obs-vban source (issue 1439)' ;;
          14) printf 'the stream role runs no Janus phones leg' ;;
          16) printf 'the stream role runs no Companion Satellite (no Stream Deck)' ;;
          16c) printf 'the stream role runs no bkshading shading service' ;;
          16f) printf 'the stream role runs no camera tally (no camera operators on stream-pp)' ;;
          16i) printf 'the stream role serves no intercom / shading page (issue 1399)' ;;
          16k) printf 'the stream role receives no cambox logs: the venue strih is the sink (issue 1394)' ;;
          13-hub-live) printf 'the stream role grades its hub report-only (issue 1418 FAILs an inactive hub on the strih role)' ;;
        esac
        return 1
      fi ;;
    avsync-dock)
      v="$(strih_box_role)" || return 0
      if [ "$v" != stream ]; then
        printf 'the A/V-sync dock reads the measurement audio on the stream box (issue 1470: stream role only)'
        return 1
      fi ;;
    16j-restreamer)
      v="$(strih_box_role)" || return 0
      if [ "$v" != stream ]; then
        printf 'the strih role is a production-only box: no Chrome, no restreamer control room (owner 24.9.2026)'
        return 1
      fi ;;
    6-devclip)
      v="$(strih_lx_stream_dev_scene)" || return 0
      if [ "$v" != clip ]; then
        printf 'STRIH_STREAM_DEV_SCENE=%s -- the measurement clip belongs to the clip design only (issue 1462)' "$v"
        return 1
      fi ;;
    6-hdmi)
      v="$(strih_lx_hdmi_output_backend)" || return 0
      if [ "$v" = none ]; then
        printf 'STRIH_HDMI_OUTPUT_BACKEND=none -- this box has no fixed HDMI output'
        return 1
      fi ;;
  esac
  return 0
}

# obs_box_venue_sender_ips VENUE -> the comma list of the STRIH_IP of every fact file whose
# STRIH_VENUE is VENUE, in file-name order. A file the loader cannot parse is skipped. rc 1 when none.
obs_box_venue_sender_ips() {
  local venue="${1:?venue required}" f parsed v ip out="" LC_ALL=C   # C collation: one fixed file order
  for f in "$(strih_box_dir)"/*.env; do
    [ -f "$f" ] || continue
    parsed="$(strih_box_parse_file "$f" 2>/dev/null)" || continue
    v="$(sed -n 's/^STRIH_VENUE=//p' <<<"$parsed")"
    ip="$(sed -n 's/^STRIH_IP=//p' <<<"$parsed")"
    [ "$v" = "$venue" ] && _strih_box_ipv4 "$ip" || continue
    out="${out:+$out,}$ip"
  done
  [ -n "$out" ] || { echo "obs-box-role: no fact file of venue '${venue}' carries an IPv4 STRIH_IP" >&2; return 1; }
  printf '%s' "$out"
}

# obs_box_role_apply -> apply the loaded box's role + venue to the shell (setup-strih / verify-strih call
# it once, right after the box loads, BEFORE their source-guard):
#   * the stream role runs THREE session apps: the browser-source keeper, the headphone monitor that plays
#     the real outgoing stream (issue 1436) and the dantesync tray (issue 1429: every Linux workstation
#     shows the clock state) (STRIH_SESSION_APP_UNITS: step 16d, the kiosk autostart lines, verify items
#     37-40 + the monitor's rows);
#   * a venue other than snv lists its own boxes as the NDI senders (NDI_DISCOVERY_SENDER_HOSTS, read by
#     scripts/lib/ndi-discovery.sh): never the SNV fleet's addresses on a PP box.
# rc 1 + a stderr line on a fact that cannot be read.
obs_box_role_apply() {
  local role venue
  role="$(strih_box_role)" || return 1
  venue="$(strih_box_venue)" || return 1
  if [ "$role" = stream ]; then
    # shellcheck disable=SC2034  # read cross-file by scripts/lib/strih-session-apps.sh
    STRIH_SESSION_APP_UNITS=("${OBS_BOX_STREAM_SESSION_APP_UNITS[@]}")
  fi
  if [ "$venue" != snv ]; then
    NDI_DISCOVERY_SENDER_HOSTS="$(obs_box_venue_sender_ips "$venue")" || return 1
    NDI_DISCOVERY_SENDER_HOSTS="${NDI_DISCOVERY_SENDER_HOSTS//,/$'\n'}"
  else
    unset NDI_DISCOVERY_SENDER_HOSTS   # an inherited value never replaces the SNV fleet list
  fi
  return 0
}

# obs_box_stream_seed_manifest_json -> the stream role's /opt/camera-box/strih-seed.json (the fixed
# ROLE path strih_scenes.py reads on every Linux OBS box): update-only (the ported collection is
# authoritative), ONE genlocked program input of class "program" (genlock_fifo + SOURCE_TIMECODE, the
# latency pin never written: the stream A/V delay is the operator's knob), the NDI output
# `<prefix> (stream)`, the program audio input name (the box fact, issue 1462), and (issue 1439) the
# console's VBAN stream that input receives: "program_audio_vban" = the obs-vban source's settings
# from the box facts (strih_scenes.py switches that input to the source, keeping its mixer state).
obs_box_stream_seed_manifest_json() {
  local sender prefix vban ai
  sender="$(strih_lx_program_sender)" || return 1
  ai="$(strih_lx_program_audio_input)" || return 1
  prefix="$(strih_lx_ndi_prefix)" || return 1
  vban="$(strih_lx_program_audio_vban_json)" || return 1
  [ -n "$sender" ] || { echo "obs-box-role: STRIH_PROGRAM_SENDER is none -- not a stream box" >&2; return 1; }
  printf '{\n  "mode": "update-only",\n  "inputs": [\n'
  printf '    {"sender": "%s", "input": "%s", "scene": "%s", "class": "program"}\n' \
    "$sender" "$OBS_BOX_STREAM_INPUT" "$OBS_BOX_STREAM_SCENE"
  printf '  ],\n  "outputs": [\n    "%s (%s)"\n  ],\n' "$prefix" "$OBS_BOX_STREAM_OUTPUT_NAME"
  printf '  "camera_latency_ms": %s,\n  "program_audio_input": "%s",\n  "program_audio_monitor": "%s",\n' \
    "$(strih_lx_camera_latency_ms)" "$ai" "$OBS_BOX_STREAM_AUDIO_MONITOR"
  # The loader validated the four facts (one stream name and one input name of plain characters, two
  # integers); the block's one renderer is strih_lx_program_audio_vban_json (scripts/lib/strih-provision.sh).
  printf '  "program_audio_vban": %s,\n' "$vban"
  # issue 1465 item 3: no pulse capture input may capture the operator's headphone mix into the program
  printf '  "pulse_capture": "forbid",\n'
  # issue 1462: ONE Development design per box (fact STRIH_STREAM_DEV_SCENE). clip = the stream-pp
  # YouTube-safe scene (stream_dev_safe.py); nested-pro = the issue-1380 SNV design the E2E seeds (its
  # seed only removes a leftover measurement clip item, which would put a second QPSK marker on program).
  case "$(strih_lx_stream_dev_scene)" in
    clip)
      printf '  "dev_scene": {"scene": "%s", "video_input": "%s", "audio_input": "%s", "audio_file": "%s"}\n}\n' \
        "$OBS_BOX_STREAM_DEV_SCENE" "$OBS_BOX_STREAM_INPUT" "$OBS_BOX_STREAM_DEV_CLIP_INPUT" "$OBS_BOX_STREAM_DEV_CLIP" ;;
    nested-pro)
      printf '  "dev_scene_nested": {"scene": "%s", "production": "%s", "clip_input": "%s"}\n}\n' \
        "$OBS_BOX_STREAM_DEV_SCENE" "$OBS_BOX_STREAM_SCENE" "$OBS_BOX_STREAM_DEV_CLIP_INPUT" ;;
    *) echo "obs-box-role: STRIH_STREAM_DEV_SCENE is not a stream design" >&2; return 1 ;;
  esac
}

# obs_box_seed_manifest_json -> the loaded box's seed manifest: the stream one, or the strih one.
obs_box_seed_manifest_json() {
  if [ "$(strih_box_role)" = stream ]; then
    obs_box_stream_seed_manifest_json
  else
    strih_lx_seed_manifest_json
  fi
}

# obs_box_ndi_output_kvs [ROLE] -> the DistroAV [NDIPlugin] user.ini key=value lines of ROLE (default
# the loaded box's), one per line: the strih's program + preview outputs, or the stream's ONE output.
obs_box_ndi_output_kvs() {
  local role="${1:-}"
  [ -n "$role" ] || role="$(strih_box_role 2>/dev/null || echo strih)"
  if [ "$role" = stream ]; then
    printf '%s\n' "MainOutputName=${OBS_BOX_STREAM_OUTPUT_NAME}" "MainOutputEnabled=true" "PreviewOutputEnabled=false"
  else
    printf '%s\n' "MainOutputName=2ME PGM" "PreviewOutputName=2ME PVW" "MainOutputEnabled=true" "PreviewOutputEnabled=true"
  fi
}

# obs_box_has_nvidia -> 0 when lspci lists an NVIDIA display controller (the baseline's own test).
obs_box_has_nvidia() {
  local out
  if ! declare -F obs_box_has_discrete_nvidia >/dev/null; then
    # verify-strih does not source the baseline lib; it is source-only, so load it on first use.
    # shellcheck source=scripts/lib/obs-box-baseline.sh
    . "$(dirname "${BASH_SOURCE[0]}")/obs-box-baseline.sh" || return 1
  fi
  out="$(lspci 2>/dev/null || true)"
  obs_box_has_discrete_nvidia <<<"$out"
}

# obs_box_vaapi_h264_encode -> 0 when vainfo lists an H.264 encode entrypoint on the render node (the
# output is read whole first: no early-closing pipe under the caller's pipefail).
obs_box_vaapi_h264_encode() {
  local vi
  vi="$(vainfo --display drm --device /dev/dri/renderD128 2>/dev/null || true)"
  grep -E 'VAProfileH264[A-Za-z]*[[:space:]]*:[[:space:]]*VAEntrypointEncSlice' >/dev/null <<<"$vi"
}

# obs_box_stream_encoder_provision OBS_CFG DESKTOP_USER PROFILE -> setup-strih step 3 for the stream
# role: the encoder by GPU (NVENC on an NVIDIA dGPU -- the baseline's step 11 installs the driver --
# else VAAPI: mesa-va-drivers + vainfo, the desktop user in group render), then the profile
# (scripts/obs_box_stream_profile.py: basic.ini, streamEncoder.json, service.json = the server + the box
# name as the stream key when it carries none, issue 1436). The
# profile is written ONLY while OBS is stopped (OBS rewrites it on exit): a running OBS gets a WARNING
# and the files are left alone. rc 1 + a stderr line on a failure.
obs_box_stream_encoder_provision() {
  local cfg="${1:?obs config dir required}" user="${2:?desktop user required}" profile="${3:?profile required}"
  local nv=0 dir key
  key="$(obs_box_stream_key)" || { echo "obs-box-role: the box name (the stream key) could not be read" >&2; return 1; }
  if obs_box_has_nvidia; then
    nv=1
    echo "  NVIDIA dGPU present -- stream encoder NVENC (the driver is baseline step 11)"
  else
    echo "  no NVIDIA dGPU -- stream encoder VAAPI (${profile}): installing mesa-va-drivers + vainfo"
    DEBIAN_FRONTEND=noninteractive apt-get install -y mesa-va-drivers vainfo \
      || { echo "obs-box-role: apt-get install mesa-va-drivers vainfo failed" >&2; return 1; }
    usermod -aG render "$user" || { echo "obs-box-role: usermod -aG render ${user} failed" >&2; return 1; }
    if obs_box_vaapi_h264_encode; then
      echo "  vainfo: H.264 encode available on /dev/dri/renderD128"
    else
      echo "  WARNING: vainfo lists no H.264 encode entrypoint on /dev/dri/renderD128 -- check the GPU driver before go-live" >&2
    fi
  fi
  if pgrep -x obs >/dev/null 2>&1; then
    echo "  WARNING: OBS is running -- the stream profile is NOT written (stop OBS and re-run this step)" >&2
    return 0
  fi
  dir="${cfg%/}/basic/profiles/${profile}"
  python3 "$(dirname "${BASH_SOURCE[0]}")/../obs_box_stream_profile.py" --profile-dir "$dir" \
    --profile-name "$profile" --has-nvidia "$nv" --server "$OBS_BOX_STREAM_RTMP_SERVER" --key "$key" \
    || { echo "obs-box-role: the stream profile write failed (above)" >&2; return 1; }
  chown -R "$user:$user" "${cfg%/}/basic" 2>/dev/null || echo "  WARNING: could not chown ${cfg%/}/basic to ${user}" >&2
  return 0
}

# obs_box_stream_encoder_grade PROFILE_DIR -> verify-strih item 10 for the stream role, through the
# caller's ok / bad: the profile's [AdvOut] Encoder is the one the GPU calls for, its streamEncoder.json
# exists, and (VAAPI) vainfo lists an H.264 encode entrypoint. Always rc 0.
obs_box_stream_encoder_grade() {
  local dir="${1:?profile dir required}" want=ffmpeg_vaapi_tex got
  obs_box_has_nvidia && want=obs_nvenc_h264_tex
  got="$(awk '/^\[AdvOut\]/{a=1;next} /^\[/{a=0} a && /^Encoder=/{sub(/^Encoder=/,""); print; exit}' "${dir}/basic.ini" 2>/dev/null || true)"
  # The 4K output rescale + CBR 19000 / keyint 2 (scripts/obs_box_stream_profile.py --grade).
  local drift
  drift="$(python3 "$(dirname "${BASH_SOURCE[0]}")/../obs_box_stream_profile.py" --grade "$dir" 2>&1 || true)"
  if [ "$got" = "$want" ] && [ -z "$drift" ]; then
    ok "(stream-encoder) ${dir}: [AdvOut] Encoder=${got}, rescale 3840x2160 lanczos + streamEncoder.json CBR 19000 keyint 2"
  else
    bad "(stream-encoder) ${dir}: [AdvOut] Encoder='${got}' (want ${want}) ${drift} -- re-run setup-strih.sh step 3 with OBS stopped"
  fi
  if [ "$want" = ffmpeg_vaapi_tex ]; then
    if obs_box_vaapi_h264_encode; then
      ok "(stream-encoder) vainfo: H.264 encode on /dev/dri/renderD128"
    else
      bad "(stream-encoder) vainfo lists no H.264 encode entrypoint on /dev/dri/renderD128 (mesa-va-drivers? the render group?)"
    fi
  fi
  return 0
}

# obs_box_stream_dev_clip_ok [CLIP] -> 0 when the measurement clip holds exactly
# OBS_BOX_STREAM_DEV_CLIP_FRAMES video frames (ffprobe count) and its marker log sits beside it.
obs_box_stream_dev_clip_ok() {
  local clip="${1:-$OBS_BOX_STREAM_DEV_CLIP}" n
  [ -s "$clip" ] && [ -s "${clip}.markers.csv" ] || return 1
  n="$(ffprobe -v error -select_streams v:0 -count_packets -show_entries stream=nb_read_packets \
    -of csv=p=0 "$clip" 2>/dev/null || true)"
  [ "$n" = "$OBS_BOX_STREAM_DEV_CLIP_FRAMES" ]
}

# obs_box_stream_dev_clip_provision SCRIPTS_DIR -> setup-strih step 6-devclip (stream role, issue 1380):
# the measurement clip the safe Development scene plays, generated ON the box by
# scripts/gen_measurement_clip.py (never copied from dev1). Installs python3-numpy + python3-qrcode (the
# generator's two non-stdlib imports) when missing; an
# existing clip with the right frame count and its marker log is kept. rc 1 + a reason on failure.
obs_box_stream_dev_clip_provision() {
  local here="${1:?scripts dir required}" clip="$OBS_BOX_STREAM_DEV_CLIP"
  if obs_box_stream_dev_clip_ok "$clip"; then
    echo "  measurement clip ${clip}: present (${OBS_BOX_STREAM_DEV_CLIP_FRAMES} frames + marker log), kept"
    return 0
  fi
  if ! python3 -c 'import numpy, qrcode' 2>/dev/null; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y python3-numpy python3-qrcode >/dev/null \
      || { echo "obs-box-role: apt-get install python3-numpy python3-qrcode failed" >&2; return 1; }
  fi
  mkdir -p "$(dirname "$clip")"
  echo "  generating the measurement clip ${clip} (gen_measurement_clip.py, a few minutes at nice 19)"
  nice -n 19 python3 "${here}/gen_measurement_clip.py" --out "$clip" \
    || { echo "obs-box-role: gen_measurement_clip.py failed" >&2; return 1; }
  chmod 0644 "$clip" "${clip}.markers.csv"
  obs_box_stream_dev_clip_ok "$clip" \
    || { echo "obs-box-role: ${clip} does not hold ${OBS_BOX_STREAM_DEV_CLIP_FRAMES} frames + its marker log" >&2; return 1; }
  echo "  measurement clip ${clip}: generated, sha256 $(sha256sum "$clip" | cut -d' ' -f1)"
}

# obs_box_stream_dev_grade SCRIPTS_DIR -> verify-strih's stream-role rows of issue 1380: the
# measurement clip file and the YouTube-safe Development scene, read live over obs-websocket by
# stream_dev_safe.py --verify. Uses the caller's ok/bad/note; always rc 0.
obs_box_stream_dev_grade() {
  local here="${1:?scripts dir required}" line level text
  if [ "$(strih_lx_stream_dev_scene)" != clip ]; then
    note "(dev-clip) not graded: STRIH_STREAM_DEV_SCENE=$(strih_lx_stream_dev_scene) carries no measurement clip (issue 1462)"
  elif obs_box_stream_dev_clip_ok; then
    ok "(dev-clip) ${OBS_BOX_STREAM_DEV_CLIP}: ${OBS_BOX_STREAM_DEV_CLIP_FRAMES} frames + marker log"
  else
    bad "(dev-clip) ${OBS_BOX_STREAM_DEV_CLIP} missing or not ${OBS_BOX_STREAM_DEV_CLIP_FRAMES} frames + marker log -- re-run setup-strih.sh step 6"
  fi
  local out
  out="$(timeout 20 python3 "${here}/stream_dev_safe.py" --verify 2>&1 || true)"
  [ -n "$out" ] || { bad "(dev-scene) stream_dev_safe.py --verify printed nothing (OBS WebSocket down?)"; return 0; }
  while IFS= read -r line; do
    level="${line%% *}"; text="${line#* }"
    case "$level" in
      PASS) ok "$text" ;;
      NOTE) note "$text" ;;
      FAIL) bad "$text" ;;
      *) bad "(dev-scene) unexpected verifier output: ${line}" ;;
    esac
  done <<<"$out"
  return 0
}

# obs_box_strih_profile_provision OBS_CFG DESKTOP_USER PROFILE -> setup-strih step 3 on the strih role
# (issues 1361 + 1419): the profile's Output, Video (1920x1080 base + output, 30 fps, NV12/709/Partial),
# Audio and record keys + recordEncoder.json (strih-snv's setup), written only while OBS is stopped (OBS rewrites its profile on exit). The launcher
# passes --profile once the directory exists; live strih-pp ran the default `Untitled` (720p output).
obs_box_strih_profile_provision() {
  local cfg="${1:?obs config dir required}" user="${2:?desktop user required}" profile="${3:?profile required}"
  if pgrep -x obs >/dev/null 2>&1; then
    echo "  WARNING: OBS is running -- the strih profile '${profile}' is NOT written (stop OBS and re-run this step)" >&2
    return 0
  fi
  python3 "$(dirname "${BASH_SOURCE[0]}")/../obs_box_stream_profile.py" --strih \
    --profile-dir "${cfg%/}/basic/profiles/${profile}" --profile-name "$profile" \
    || { echo "obs-box-role: the strih profile write failed (above)" >&2; return 1; }
  chown -R "$user:$user" "${cfg%/}/basic" 2>/dev/null || echo "  WARNING: could not chown ${cfg%/}/basic to ${user}" >&2
  return 0
}

# obs_box_strih_profile_grade PROFILE_DIR -> verify-strih item 10's strih row (issues 1361 + 1419): the
# box's own profile exists with every key setup step 3 writes -- 1920x1080 base + output @ 30 fps (the
# NDI program follows the output size) and strih-snv's record / output keys + recordEncoder.json.
# Uses the caller's ok/bad; always rc 0.
obs_box_strih_profile_grade() {
  local dir="${1:?profile dir required}" drift
  drift="$(python3 "$(dirname "${BASH_SOURCE[0]}")/../obs_box_stream_profile.py" --grade-strih "$dir" 2>&1 || true)"
  if [ -z "$drift" ]; then
    ok "(strih-profile) ${dir}: 1920x1080 @ 30 fps, NVENC HEVC record to /srv/_REC (15-min mkv), the strih-snv output keys"
  else
    bad "(strih-profile) ${dir}: ${drift} -- re-run setup-strih.sh step 3 with OBS stopped (issue 1419)"
  fi
  return 0
}

# --- the restreamer's control-room host (stream role, restreamer issue 388, 13.10.2026) ---------------
# google-chrome-stable from Google's signed apt repo (it is not in the Ubuntu archive, so no .deb can
# Depend on it) and xvfb (the restreamer runs its own hidden Xvfb display for that Chrome, nothing on the
# kiosk screen). The signing key is accepted ONLY by its pinned primary fingerprint (read 13.10.2026).
OBS_BOX_GOOGLE_KEY_URL=https://dl.google.com/linux/linux_signing_key.pub
OBS_BOX_GOOGLE_KEY_FPR=EB4C1BFD4F042F6DDDCCEC917721F63BD38B4796
OBS_BOX_RESTREAMER_HOST_PACKAGES=(google-chrome-stable xvfb)

# obs_box_apt_root -> the filesystem root the apt files live under (OBS_BOX_APT_ROOT is the test seam).
obs_box_apt_root() { printf '%s' "${OBS_BOX_APT_ROOT:-}"; }

# obs_box_google_chrome_list -> the one apt source line of Google's Chrome repo, signed-by its keyring.
obs_box_google_chrome_list() {
  printf 'deb [arch=amd64 signed-by=/etc/apt/keyrings/google-chrome.gpg] https://dl.google.com/linux/chrome/deb/ stable main\n'
}

# obs_box_google_repo_ok -> 0 when a Google Chrome apt source is in place: our bootstrap list (signed-by
# the pinned keyring) OR the deb822 google-chrome.sources the google-chrome-stable postinst writes with
# its own keyring (live stream-pp 13.10.2026: it deletes the bootstrap list and keeps its own file up to
# date). Prints which one.
obs_box_google_repo_ok() {
  local root d
  root="$(obs_box_apt_root)"
  d="${root}/etc/apt/sources.list.d"
  if [ -s "${root}/etc/apt/keyrings/google-chrome.gpg" ] \
    && [ "$(cat "${d}/google-chrome.list" 2>/dev/null)" = "$(obs_box_google_chrome_list)" ]; then
    printf 'the bootstrap source (pinned key)'
    return 0
  fi
  if grep -qx 'URIs: https://dl.google.com/linux/chrome[-a-z]*/deb/' "${d}/google-chrome.sources" 2>/dev/null \
    && grep -q '^Signed-By: /' "${d}/google-chrome.sources" \
    && [ -s "${root}$(sed -n 's/^Signed-By: //p' "${d}/google-chrome.sources" | head -1)" ]; then
    printf "Chrome's own source (google-chrome.sources, signed)"
    return 0
  fi
  return 1
}

# _obs_box_pkg_installed PKG -> 0 when dpkg reports PKG installed.
_obs_box_pkg_installed() {
  [ "$(dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true)" = "install ok installed" ]
}

# obs_box_stream_restreamer_host_provision -> setup-strih step 16j (stream role). rc 1 + a reason on a
# refused key or a failed install; an installed host (both packages + the repo files) is left alone.
obs_box_stream_restreamer_host_provision() {
  local root key list missing="" p fprs armored repo
  root="$(obs_box_apt_root)"
  key="${root}/etc/apt/keyrings/google-chrome.gpg"
  list="${root}/etc/apt/sources.list.d/google-chrome.list"
  for p in "${OBS_BOX_RESTREAMER_HOST_PACKAGES[@]}"; do _obs_box_pkg_installed "$p" || missing="${missing} ${p}"; done
  if [ -z "$missing" ] && repo="$(obs_box_google_repo_ok)"; then
    echo "  restreamer host: google-chrome-stable + xvfb installed, ${repo} -- unchanged"
    return 0
  fi
  armored="$(curl -fsSL "$OBS_BOX_GOOGLE_KEY_URL")" \
    || { echo "obs-box-role: cannot fetch ${OBS_BOX_GOOGLE_KEY_URL}" >&2; return 1; }
  fprs="$(printf '%s' "$armored" | gpg --show-keys --with-colons 2>/dev/null | awk -F: '$1 == "fpr" {print $10}')"
  if ! grep -qx "$OBS_BOX_GOOGLE_KEY_FPR" <<<"$fprs"; then
    echo "obs-box-role: the Google signing key's fingerprint is not ${OBS_BOX_GOOGLE_KEY_FPR} -- refused, nothing written" >&2
    return 1
  fi
  install -d -m 0755 "$(dirname "$key")" "$(dirname "$list")" \
    || { echo "obs-box-role: cannot create the apt keyring/source dirs" >&2; return 1; }
  printf '%s' "$armored" | gpg --dearmor > "${key}.new" && chmod 0644 "${key}.new" && mv -f "${key}.new" "$key" \
    || { echo "obs-box-role: cannot write ${key}" >&2; return 1; }
  obs_box_google_chrome_list > "${list}.new" && chmod 0644 "${list}.new" && mv -f "${list}.new" "$list" \
    || { echo "obs-box-role: cannot write ${list}" >&2; return 1; }
  echo "  Google Chrome apt repo: keyring ${key} (fingerprint ${OBS_BOX_GOOGLE_KEY_FPR}) + ${list}"
  DEBIAN_FRONTEND=noninteractive apt-get update -o Dir::Etc::sourcelist=- \
    -o Dir::Etc::sourceparts="$(dirname "$list")" >/dev/null 2>&1 \
    || DEBIAN_FRONTEND=noninteractive apt-get update >/dev/null \
    || { echo "obs-box-role: apt-get update failed" >&2; return 1; }
  DEBIAN_FRONTEND=noninteractive apt-get install -y "${OBS_BOX_RESTREAMER_HOST_PACKAGES[@]}" \
    || { echo "obs-box-role: apt-get install ${OBS_BOX_RESTREAMER_HOST_PACKAGES[*]} failed" >&2; return 1; }
  echo "  restreamer host: ${OBS_BOX_RESTREAMER_HOST_PACKAGES[*]} installed"
}

# obs_box_stream_restreamer_host_grade -> verify-strih's stream-branch row (restreamer issue 388): both
# packages installed, the signed repo files in place, google-chrome answers --version. Uses the caller's
# ok/bad; always rc 0.
obs_box_stream_restreamer_host_grade() {
  local missing="" p ver repo=""
  for p in "${OBS_BOX_RESTREAMER_HOST_PACKAGES[@]}"; do _obs_box_pkg_installed "$p" || missing="${missing} ${p}"; done
  ver="$(timeout 10 google-chrome --version 2>/dev/null || true)"
  repo="$(obs_box_google_repo_ok || true)"
  if [ -z "$missing" ] && [ -n "$repo" ] && [ -n "$ver" ]; then
    ok "(restreamer-host) ${ver% } + xvfb installed, ${repo}"
  else
    bad "(restreamer-host) missing:${missing:- none}; Google apt source: ${repo:-none signed}; google-chrome --version '${ver% }' -- re-run setup-strih.sh step 16j"
  fi
  return 0
}
