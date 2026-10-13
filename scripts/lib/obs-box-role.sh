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
#                            program_audio_input `Zvuk z mixu`; no HDMI output (backend none)
#   7       2ME PGM + PVW    one NDI output `<prefix> (stream)`, no preview output
#   14      Janus            skipped (no phones leg on a stream box)
#   16      Satellite        skipped (no Stream Deck)
#   16c     bkshading        skipped (no shading panel)
#   16d     4 session apps   the browser-source keeper only
#   16f     tally            skipped
#   13-hub-live  verify item 16: an inactive hub FAILs   report-only (issue 1418)
#
# Independent of the role: 11b only for a USB rig NIC, 16e only with STRIH_PROGRAM_AUDIO_SAMPLER=on, and
# a venue other than snv lists its own boxes' STRIH_IP as the NDI senders (step 4b / verify item 34).
#
# The facts come from scripts/strih-boxes/<box>.env through scripts/lib/strih-box-facts.sh.

# The stream role's names: the NDI program input (the SNV stream box's input name), the scene it lives
# in (the ported collection's program scene), the program audio input of the ported PP collection, and
# the bare DistroAV main output name (DistroAV prepends the hostname -> `STREAM-PP (stream)`).
OBS_BOX_STREAM_INPUT="NDI 2ME PGM"
OBS_BOX_STREAM_SCENE="PRO"
OBS_BOX_STREAM_AUDIO_INPUT="Zvuk z mixu"
OBS_BOX_STREAM_OUTPUT_NAME="stream"
# issue 1380 (ROZHODNUTE 13.10.2026): the stream role's YouTube-safe Development scene -- the program
# video input (muted) over the measurement clip, never the production scene (it holds the live mixer).
# The scene name equals STREAM_DEV_SCENE_DEFAULT (scripts/lib/stream-dev-scene.sh, pinned by a pytest).
OBS_BOX_STREAM_DEV_SCENE="Development"
OBS_BOX_STREAM_DEV_CLIP_INPUT="Merací klip"
OBS_BOX_STREAM_DEV_CLIP="/opt/camera-box/measurement-clip-v1.mp4"
OBS_BOX_STREAM_DEV_CLIP_FRAMES=3840   # gen_measurement_clip.py: 128 s x 30 fps
# The local restreamer the stream OBS sends to (the stream-pp laptop runs it; recon 11.10.2026).
OBS_BOX_STREAM_RTMP_SERVER="${OBS_BOX_STREAM_RTMP_SERVER:-rtmp://127.0.0.1:1234/live}"

# obs_box_step_applies STEP -> rc 0 when the loaded box runs STEP; rc 1 with the reason on stdout when
# it skips it. A step with no rule here always runs. A fact that cannot be read never skips a step.
#   11b -- the NIC IRQ unit resolves the rig NIC's XHCI interrupt, so it exists only for a USB rig NIC
#          (STRIH_NIC_MIN_USB_MBPS != none). On a PCIe NIC (strih-pp's RTL8125) the walk to a usbN root
#          would fail the unit at every boot (gap G7).
#   16e -- the YouTube-guard stream program-audio sampler (issue 1404) reads the SNV stream output, so
#          only a box with STRIH_PROGRAM_AUDIO_SAMPLER=on runs it (gap G11).
#   3-nvenc / 14 / 16 / 16c / 16f / 13-hub-live -- the strih role only (the table above; 16f = the camera
#          tally, issue 1417; 13-hub-live = verify item 16 FAILs an inactive or disabled intercom hub, issue 1418).
#   6-hdmi -- only a box with a fixed HDMI output (STRIH_HDMI_OUTPUT_BACKEND != none).
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
    3-nvenc | 14 | 16 | 16c | 16f | 13-hub-live)
      v="$(strih_box_role)" || return 0
      if [ "$v" = stream ]; then
        case "$step" in
          3-nvenc) printf 'the stream role encodes with the GPU it has (VAAPI without an NVIDIA dGPU), not the strih NVENC' ;;
          14) printf 'the stream role runs no Janus phones leg' ;;
          16) printf 'the stream role runs no Companion Satellite (no Stream Deck)' ;;
          16c) printf 'the stream role runs no bkshading shading service' ;;
          16f) printf 'the stream role runs no camera tally (no camera operators on stream-pp)' ;;
          13-hub-live) printf 'the stream role grades its hub report-only (issue 1418 FAILs an inactive hub on the strih role)' ;;
        esac
        return 1
      fi ;;
    6-devclip)
      v="$(strih_box_role)" || return 0
      if [ "$v" != stream ]; then
        printf 'the strih role has no YouTube-safe stream Development scene (issue 1380: stream role only)'
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
#   * the stream role runs ONE session app, the browser-source keeper (STRIH_SESSION_APP_UNITS: step 16d,
#     the kiosk autostart lines, verify items 37-40);
#   * a venue other than snv lists its own boxes as the NDI senders (NDI_DISCOVERY_SENDER_HOSTS, read by
#     scripts/lib/ndi-discovery.sh): never the SNV fleet's addresses on a PP box.
# rc 1 + a stderr line on a fact that cannot be read.
obs_box_role_apply() {
  local role venue
  role="$(strih_box_role)" || return 1
  venue="$(strih_box_venue)" || return 1
  if [ "$role" = stream ]; then
    # shellcheck disable=SC2034  # read cross-file by scripts/lib/strih-session-apps.sh
    STRIH_SESSION_APP_UNITS=(strih-browser-keeper.service)
  fi
  if [ "$venue" != snv ]; then
    NDI_DISCOVERY_SENDER_HOSTS="$(obs_box_venue_sender_ips "$venue")" || return 1
    NDI_DISCOVERY_SENDER_HOSTS="${NDI_DISCOVERY_SENDER_HOSTS//,/$'\n'}"
  else
    unset NDI_DISCOVERY_SENDER_HOSTS   # an inherited value never replaces the SNV fleet list
  fi
  return 0
}

# obs_box_stream_seed_manifest_json -> the stream role's /opt/camera-box/strih-lx-seed.json (the fixed
# ROLE path strih_scenes.py reads on every Linux OBS box): update-only (the ported collection is
# authoritative), ONE genlocked program input of class "program" (genlock_fifo + SOURCE_TIMECODE, the
# latency pin never written: the stream A/V delay is the operator's knob), the NDI output
# `<prefix> (stream)`, and the program audio input name of the ported collection.
obs_box_stream_seed_manifest_json() {
  local sender prefix
  sender="$(strih_lx_program_sender)" || return 1
  prefix="$(strih_lx_ndi_prefix)" || return 1
  [ -n "$sender" ] || { echo "obs-box-role: STRIH_PROGRAM_SENDER is none -- not a stream box" >&2; return 1; }
  printf '{\n  "mode": "update-only",\n  "inputs": [\n'
  printf '    {"sender": "%s", "input": "%s", "scene": "%s", "class": "program"}\n' \
    "$sender" "$OBS_BOX_STREAM_INPUT" "$OBS_BOX_STREAM_SCENE"
  printf '  ],\n  "outputs": [\n    "%s (%s)"\n  ],\n' "$prefix" "$OBS_BOX_STREAM_OUTPUT_NAME"
  printf '  "camera_latency_ms": %s,\n  "program_audio_input": "%s",\n' \
    "$(strih_lx_camera_latency_ms)" "$OBS_BOX_STREAM_AUDIO_INPUT"
  printf '  "dev_scene": {"scene": "%s", "video_input": "%s", "audio_input": "%s", "audio_file": "%s"}\n}\n' \
    "$OBS_BOX_STREAM_DEV_SCENE" "$OBS_BOX_STREAM_INPUT" "$OBS_BOX_STREAM_DEV_CLIP_INPUT" "$OBS_BOX_STREAM_DEV_CLIP"
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
# (scripts/obs_box_stream_profile.py: basic.ini, streamEncoder.json, service.json server only). The
# profile is written ONLY while OBS is stopped (OBS rewrites it on exit): a running OBS gets a WARNING
# and the files are left alone. rc 1 + a stderr line on a failure.
obs_box_stream_encoder_provision() {
  local cfg="${1:?obs config dir required}" user="${2:?desktop user required}" profile="${3:?profile required}"
  local nv=0 dir
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
    --profile-name "$profile" --has-nvidia "$nv" --server "$OBS_BOX_STREAM_RTMP_SERVER" \
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
  if obs_box_stream_dev_clip_ok; then
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
