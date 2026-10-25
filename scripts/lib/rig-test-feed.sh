#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines a constant + pure builders, no top-level statements besides the
# defaults) -- the scripts/lib/*.sh convention: strict mode belongs to the caller (rig-mode.sh / recording-e2e.sh).
#
# scripts/lib/rig-test-feed.sh -- issue 1482: HOW the SNV rig's cameras see cam2's test pattern. Owner 25.10.2026:
# "aby sme nemuseli mat 24/7 zapnutu kameru a nicit jej snimace tak sme ruobili priame preponie hdmi kableom medzi
# cam2 a hdmi spliterom ... kedze uz nemame monitor s jackom tak sme vytiahli zvuk jackom priamo z cam2 boxu", then
# "a nech to mas zabudovane do test/event modu korektne". ONE fact, RIG_TEST_FEED:
#   optical  a camera films cam2's monitor; the QPSK marker plays from the monitor's speaker (cam2's HDMI PCM,
#            hw:CARD=PCH,DEV=3, the #725 live resolution); cam2 keeps its intercom.
#   direct   (the wiring since 25.10.2026, the default) cam2's HDMI goes straight into the splitter; the marker
#            plays from cam2's USB jack (the CSCTEK dongle, RIG_DIRECT_MARKER_DEVICE, default hw:CARD=HID,DEV=0),
#            which is also cam2's intercom headset device, so in TEST cam2 runs NO intercom (owner ROZHODNUTE
#            25.10.2026 on issue 1482) and the E2E skips the test-camera exposure enforce (no camera in the path).
# What TEST / EVENT do with it (rig-mode.sh):
#   TEST   a camera-box /run drop-in of its own sets CAMERA_BOX_INTERCOM=off (direct), the transient painter gets the
#          jack as --audio-marker-device, and the steady-state cam2-painter.service gets a /run drop-in naming
#          it (FRAME_PROBE_AUDIO_MARKER_DEVICE); optical removes that drop-in.
#   EVENT  removes both drop-ins before its camera-box restart, so cam2 is back
#          to its configured intercom and the painter unit to its live HDMI resolution.

RIG_TEST_FEED="${RIG_TEST_FEED:-direct}"
RIG_DIRECT_MARKER_DEVICE="${RIG_DIRECT_MARKER_DEVICE:-hw:CARD=HID,DEV=0}"
RIG_OPTICAL_MARKER_DEVICE="${RIG_OPTICAL_MARKER_DEVICE:-hw:CARD=PCH,DEV=3}"
RIG_TEST_FEED_PAINTER_DROPIN="${RIG_TEST_FEED_PAINTER_DROPIN:-/run/systemd/system/cam2-painter.service.d/zz-rig-direct-marker.conf}"
RIG_TEST_FEED_INTERCOM_DROPIN="${RIG_TEST_FEED_INTERCOM_DROPIN:-/run/systemd/system/camera-box.service.d/zz-rig-direct-intercom-off.conf}"

# rig_test_feed -> `optical` | `direct` (rc 0); anything else is named on stderr, rc 2 (never guessed).
rig_test_feed() {
  case "$RIG_TEST_FEED" in
    optical | direct) printf '%s' "$RIG_TEST_FEED" ;;
    *) echo "rig-test-feed: RIG_TEST_FEED='${RIG_TEST_FEED}' is neither optical nor direct (issue 1482)" >&2; return 2 ;;
  esac
}

# rig_test_feed_marker_device -> the ALSA device the QPSK marker plays to in this feed.
rig_test_feed_marker_device() {
  local f
  f="$(rig_test_feed)" || return 2
  if [ "$f" = direct ]; then printf '%s' "$RIG_DIRECT_MARKER_DEVICE"; else printf '%s' "$RIG_OPTICAL_MARKER_DEVICE"; fi
}

# rig_test_feed_intercom_dropin_cmds -> remote bash for rig-mode TEST, run on cam2 BEFORE its camera-box restart:
# the direct feed writes camera-box's own intercom drop-in (CAMERA_BOX_INTERCOM=off), the optical feed removes
# it. A drop-in of its own, NOT a line in the no-display drop-in: every E2E cleanup clears that one to restore
# the preview, and the intercom would come back and take the jack from the marker after every run. Only EVENT
# removes this one (rig_test_feed_painter_dropin_clear_cmds).
rig_test_feed_intercom_dropin_cmds() {
  local f q
  f="$(rig_test_feed)" || return 2
  q="$(printf '%q' "$RIG_TEST_FEED_INTERCOM_DROPIN")"
  if [ "$f" = direct ]; then
    printf 'mkdir -p "$(dirname %s)"; printf %s > %s;\n' "$q" "'[Service]\nEnvironment=CAMERA_BOX_INTERCOM=off\n'" "$q"
    printf 'echo "[issue 1482] direct feed: cam2 camera-box runs no intercom (the jack belongs to the QPSK marker)";\n'
  else
    printf 'rm -f %s;\n' "$q"
  fi
}

# rig_test_feed_burn_setenv -> the systemd-run argument the E2E's cam2 burn camera-box needs: a transient unit
# never reads camera-box.service's drop-ins, so in the direct feed its intercom would open the jack and the QPSK
# marker fails "Device or resource busy". Empty in the optical feed. Ends in a space (it prefixes the next arg).
rig_test_feed_burn_setenv() {
  local f
  f="$(rig_test_feed)" || return 2
  if [ "$f" = direct ]; then printf '%s' '--setenv=CAMERA_BOX_INTERCOM=off '; fi
}

# The A/V offset every camera measures in the direct feed (E2E 40233177561, 25.10.2026: cam2 -50.1, cam3 -51.4,
# cam5 -51.1 ms, MAD ~5 ms). The direct feed has no camera filming cam2's monitor, so the picture arrives that camera's
# latency earlier than in the optical feed and in production. A property of the test feed, never of production.
RIG_DIRECT_AV_EXPECTED_MS="${RIG_DIRECT_AV_EXPECTED_MS:--51}"

# rig_test_feed_av_expected_ms -> the A/V gate's expected offset for this feed (0 optical, the direct video leg).
rig_test_feed_av_expected_ms() {
  local f
  f="$(rig_test_feed)" || return 2
  if [ "$f" = direct ]; then printf '%s' "$RIG_DIRECT_AV_EXPECTED_MS"; else printf '0'; fi
}

# rig_test_feed_av_apply_hold -> a HOLD reason (non-empty) when this feed must never tune the production A/V pin, else
# nothing. The E2E's cleanup #856 apply moves the stream 'NDI 2ME PGM' pin + the mbc sync offset; only the optical
# feed has a camera in its video path like production, so a direct-feed measurement would detune production.
rig_test_feed_av_apply_hold() {
  local f
  f="$(rig_test_feed)" || return 2
  if [ "$f" = direct ]; then
    printf '%s' 'issue 1482: the direct test feed has no camera in its video path, so its A/V offset never tunes the production stream pin'
  fi
}

# rig_test_feed_painter_dropin_cmds -> remote bash (run as root on cam2, BEFORE the steady-state handoff): the
# direct feed writes the cam2-painter.service drop-in naming the jack, the optical feed removes it; then
# daemon-reload. Fails loud (exit 1) when the jack is not in aplay -l in the direct feed.
rig_test_feed_painter_dropin_cmds() {
  local f dev card pcm q
  f="$(rig_test_feed)" || return 2
  q="$(printf '%q' "$RIG_TEST_FEED_PAINTER_DROPIN")"
  if [ "$f" != direct ]; then
    printf 'rm -f %s; rmdir "$(dirname %s)" 2>/dev/null || true; systemctl daemon-reload;\n' "$q" "$q"
    printf 'echo "[issue 1482] optical feed: cam2-painter.service resolves its HDMI marker device live";\n'
    return 0
  fi
  dev="$RIG_DIRECT_MARKER_DEVICE"
  card="${dev#hw:CARD=}"; card="${card%%,*}"; pcm="${dev##*DEV=}"
  cat <<EOF
if ! aplay -l 2>/dev/null | grep -Eq '^card [0-9]+: ${card} .*, device ${pcm}:'; then
  echo "FAIL: [issue 1482] direct feed: the marker jack ${dev} is not in cam2's aplay -l -- is the USB audio dongle plugged in?" >&2
  exit 1
fi
mkdir -p "\$(dirname ${q})"
printf '[Service]\nEnvironment=FRAME_PROBE_AUDIO_MARKER_DEVICE=%s\n' '${dev}' > ${q}
systemctl daemon-reload
echo "[issue 1482] direct feed: cam2-painter.service plays the QPSK marker to the jack ${dev} (${RIG_TEST_FEED_PAINTER_DROPIN})";
EOF
}

# rig_test_feed_painter_dropin_clear_cmds -> remote bash for EVENT, run BEFORE its camera-box restart: remove the
# painter drop-in and the intercom drop-in, daemon-reload (the restart brings cam2's configured intercom back).
rig_test_feed_painter_dropin_clear_cmds() {
  local q i
  q="$(printf '%q' "$RIG_TEST_FEED_PAINTER_DROPIN")"
  i="$(printf '%q' "$RIG_TEST_FEED_INTERCOM_DROPIN")"
  printf 'rm -f %s %s; rmdir "$(dirname %s)" 2>/dev/null || true; systemctl daemon-reload || true;\n' "$q" "$i" "$q"
}

# rig_test_feed_jack_present APLAY_TEXT DEVICE -> rc 0 iff APLAY_TEXT (`aplay -l`) lists DEVICE (hw:CARD=X,DEV=N).
rig_test_feed_jack_present() {
  local text="${1-}" dev="${2-}" card pcm
  card="${dev#hw:CARD=}"; card="${card%%,*}"; pcm="${dev##*DEV=}"
  [ -n "$card" ] && [ -n "$pcm" ] || return 1
  grep -Eq "^card [0-9]+: ${card} .*, device ${pcm}:" <<<"$text"
}

# rig_test_feed_skips_test_camera -> rc 0 (and one line naming why) when the E2E must skip the test-camera
# exposure enforce: the direct feed has no camera in the path.
rig_test_feed_skips_test_camera() {
  [ "$(rig_test_feed 2>/dev/null)" = direct ] || return 1
  echo "[0/8] test-camera exposure enforce SKIPPED: RIG_TEST_FEED=direct -- cam2's HDMI goes straight into the splitter, no camera in the path (issue 1482)"
}
