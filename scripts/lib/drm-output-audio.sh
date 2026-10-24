#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell (setup-strih.sh via
# strih-drm-output.sh, setup-imag.sh, verify-strih.sh and the Tier-0 harnesses).
#
# scripts/lib/drm-output-audio.sh -- issue 1424 (owner 13.10.2026): the in-OBS fixed HDMI output
# (issue 1152 lease / issue 1346 vk-direct) plays the OBS program mix on its OWN connector's HDMI
# audio, written by the vendored libobs module straight into that connector's ALSA hw PCM
# (`hw:<card>,<device>`, matched by EDID vs ELD -- vendor/obs-studio/libobs/obs-drm-output-audio.c).
# "Ziadnych milion vrstiev po ceste": no PipeWire, no monitoring device, no mixer in between.
#
# For OBS to own that PCM, PipeWire must never hold an HDMI/DP PCM on such a box. ONE WirePlumber
# rule, generic for every box with the in-OBS HDMI output (no per-box fact): every ALSA node whose
# `api.alsa.path` is an `hdmi:` path is disabled. Every HDMI/DP PCM of every card has that path
# (NVIDIA "HDA NVidia", AMD/Intel HDMI codecs), while analog and USB interfaces (the laptop speakers,
# the MiniFuse) keep theirs, so the rule never touches the operator's own audio.
#
# WirePlumber 0.5 (Ubuntu 26.04 strih boxes) reads SPA-JSON drop-ins from
# /etc/wireplumber/wireplumber.conf.d/; 0.4 reads Lua from /etc/wireplumber/main.lua.d/. The rule
# takes effect when WirePlumber starts (the next session start); provisioning never restarts the
# running audio session -- verify reports a still-present HDMI node by name.
#
# Pure helpers (unit-tested by tests/python/test_drm_output_audio_1424.py):
#   drm_output_audio_wp_flavour VERSION_TEXT   `wireplumber --version` -> conf | lua | unknown
#   drm_output_audio_wp_rule FLAVOUR           the rule file's exact content
#   drm_output_audio_wp_rule_path FLAVOUR [ROOT]  where it lives
#   drm_output_audio_hdmi_node_count            stdin = `pw-dump` JSON -> the HDMI ALSA node count, "?"
#   drm_output_audio_log_state                  stdin = an OBS log -> open | no-audio | none
#   drm_output_audio_log_line                   stdin = an OBS log -> its last `drm-output audio:` open/NO AUDIO line
#   drm_output_audio_verdict RULE NODES LOG     the verify verdict token
# Actions:
#   drm_output_audio_wp_provision [ROOT]        setup (root): write the rule if it differs
#   drm_output_audio_verify_report USER_HOME USER LOG [ROOT [CONNECTOR [VIEW]]]  verify: grade it (caller's ok/bad/note)

DRM_OUTPUT_AUDIO_WP_NAME="51-camera-box-drm-output-hdmi"

# drm_output_audio_wp_flavour VERSION_TEXT -> conf (WirePlumber >= 0.5), lua (0.4.x), unknown.
drm_output_audio_wp_flavour() {
  local text="${1:-}" re='libwireplumber[[:space:]]+([0123456789]+)\.([0123456789]+)\.'
  if [[ $text =~ $re ]]; then
    if [ "${BASH_REMATCH[1]}" -ge 1 ] || [ "${BASH_REMATCH[2]}" -ge 5 ]; then
      printf 'conf'
    else
      printf 'lua'
    fi
  else
    printf 'unknown'
  fi
  return 0
}

# drm_output_audio_wp_rule_path FLAVOUR [ROOT] -> the system-wide drop-in path (empty for unknown).
drm_output_audio_wp_rule_path() {
  local root="${2:-}"
  case "${1:-}" in
    conf) printf '%s/etc/wireplumber/wireplumber.conf.d/%s.conf' "$root" "$DRM_OUTPUT_AUDIO_WP_NAME" ;;
    lua) printf '%s/etc/wireplumber/main.lua.d/%s.lua' "$root" "$DRM_OUTPUT_AUDIO_WP_NAME" ;;
    *) return 1 ;;
  esac
}

# drm_output_audio_wp_rule FLAVOUR -> the rule file content (returns 1 for unknown).
drm_output_audio_wp_rule() {
  case "${1:-}" in
    conf)
      cat <<'RULE'
# camera-box issue 1424 -- written by setup (scripts/lib/drm-output-audio.sh), do not edit.
# The in-OBS fixed HDMI output writes the program mix straight into its connector's HDMI PCM,
# so no PipeWire node may hold an HDMI/DP PCM on this box.
monitor.alsa.rules = [
  {
    matches = [
      { api.alsa.path = "~hdmi:.*" }
    ]
    actions = {
      update-props = {
        node.disabled = true
      }
    }
  }
]
RULE
      ;;
    lua)
      cat <<'RULE'
-- camera-box issue 1424 -- written by setup (scripts/lib/drm-output-audio.sh), do not edit.
-- The in-OBS fixed HDMI output writes the program mix straight into its connector's HDMI PCM,
-- so no PipeWire node may hold an HDMI/DP PCM on this box.
table.insert(alsa_monitor.rules, {
  matches = { { { "api.alsa.path", "matches", "hdmi:*" } } },
  apply_properties = { ["node.disabled"] = true },
})
RULE
      ;;
    *) return 1 ;;
  esac
}

# drm_output_audio_hdmi_node_count (stdin = `pw-dump` JSON) -> how many PipeWire nodes hold an
# HDMI/DP ALSA PCM (api.alsa.path starting with hdmi:), or "?" when the JSON does not parse.
drm_output_audio_hdmi_node_count() {
  python3 -c '
import json, sys
try:
    objs = json.load(sys.stdin)
except Exception:
    print("?")
    sys.exit(0)
n = 0
for o in objs if isinstance(objs, list) else []:
    if not isinstance(o, dict) or o.get("type") != "PipeWire:Interface:Node":
        continue
    props = ((o.get("info") or {}).get("props") or {})
    if str(props.get("api.alsa.path", "")).startswith("hdmi:"):
        n += 1
print(n)
' 2>/dev/null || printf '?\n'
}

# drm_output_audio_log_state (stdin = an OBS log) -> open | no-audio | none: the LAST audio-leg
# outcome of that OBS session (`drm-output audio: pcm hw:` = the first block placed, `drm-output
# audio: '<conn>' matched hw:` = the resolver opened the PCM -- the only line while the view is the
# Multiview, which places no block -- `NO AUDIO` = it gave up and named why; the watcher's `no longer
# carries the connector's monitor ... muted` and a `write to hw:... failed` line are no-audio until a
# later `carries the connector's monitor again`). The LAST outcome line wins. Reads all of stdin;
# always returns 0.
drm_output_audio_log_state() {
  LC_ALL=C awk '
    index($0, "drm-output audio: pcm hw:") { s = "open" }
    index($0, "drm-output audio: \047") && index($0, " matched hw:") { s = "open" }
    index($0, "drm-output audio: \047") && index($0, " carries the connector\047s monitor again") { s = "open" }
    index($0, "drm-output audio: NO AUDIO") { s = "no-audio" }
    index($0, "drm-output audio: \047") && index($0, " no longer carries the connector\047s monitor") { s = "no-audio" }
    index($0, "drm-output audio: \047") && index($0, " write to hw:") { s = "no-audio" }
    END { print (s == "" ? "none" : s) }' || true
}

# drm_output_audio_log_line (stdin = an OBS log) -> the last open / NO AUDIO line (for the report).
drm_output_audio_log_line() {
  LC_ALL=C awk '
    index($0, "drm-output audio: pcm hw:") || index($0, "drm-output audio: NO AUDIO") { l = $0 }
    index($0, "drm-output audio: \047") && index($0, " matched hw:") { l = $0 }
    index($0, "drm-output audio: \047") && index($0, "the connector\047s monitor") { l = $0 }
    index($0, "drm-output audio: \047") && index($0, " write to hw:") { l = $0 }
    END { print l }' || true
}

# drm_output_audio_no_sound_server -> rc 0 when the box has NO sound server at all (issue 1440: the strih
# role purges PipeWire where its facts need none): neither `wireplumber` nor `pipewire` is a command, so
# nothing can hold the HDMI PCM and no rule is needed. DRM_OUTPUT_AUDIO_SERVER_BINS (the test seam) names
# the binaries looked up; default "wireplumber pipewire".
drm_output_audio_no_sound_server() {
  local b
  for b in ${DRM_OUTPUT_AUDIO_SERVER_BINS:-wireplumber pipewire}; do
    command -v "$b" >/dev/null 2>&1 && return 1
  done
  return 0
}

# drm_output_audio_verdict RULE NODES LOG -> ONE token; return 0 = PASS, 2 = NOTE, 1 = FAIL:
#   RULE  ok | missing | drift (the rule file vs drm_output_audio_wp_rule) | unknown (no WirePlumber
#         version read) | none (no sound server on the box, issue 1440: no rule needed; NODES is 0)
#   NODES the PipeWire HDMI node count, "?" = not read
#   LOG   drm_output_audio_log_state of the newest OBS log
#   wp-unknown (1)  no WirePlumber version could be read -- the rule cannot be checked
#   rule-missing (1) / rule-drift (1)  the WirePlumber rule is absent / not the shipped content
#   pipewire-holds-hdmi (1)  PipeWire still has an HDMI node (the rule applies at the next session start)
#   pipewire-unread (2)  the node list could not be read (report-only)
#   no-audio (1)  the module gave up (the line names why)
#   no-line (1)   the newest OBS log has no audio-leg outcome (an OBS without the leg, or it is still resolving)
#   ok (0)
drm_output_audio_verdict() {
  local rule="${1:-missing}" nodes="${2:-?}" log="${3:-none}"
  case "$rule" in
    unknown) printf 'wp-unknown'; return 1 ;;
    missing) printf 'rule-missing'; return 1 ;;
    drift) printf 'rule-drift'; return 1 ;;
  esac
  if [ "$nodes" != "?" ] && [ "$nodes" != 0 ]; then
    printf 'pipewire-holds-hdmi'
    return 1
  fi
  case "$log" in
    no-audio) printf 'no-audio'; return 1 ;;
    open) ;;
    *) printf 'no-line'; return 1 ;;
  esac
  if [ "$nodes" = "?" ]; then
    printf 'pipewire-unread'
    return 2
  fi
  printf 'ok'
  return 0
}

# drm_output_audio_wp_provision [ROOT] -- setup (root): write the rule for this box's WirePlumber when
# it differs (write-if-changed, 0644). Never restarts the running audio session: the rule applies
# when WirePlumber next starts. ROOT (default: DRM_OUTPUT_AUDIO_WP_ROOT, a test seam, else /) prefixes
# the path. A failed write is a WARNING (verify grades the missing rule by name). Always returns 0.
drm_output_audio_wp_provision() {
  local root="${1:-${DRM_OUTPUT_AUDIO_WP_ROOT:-}}" flavour path want
  if drm_output_audio_no_sound_server; then
    echo "  no sound server on this box (issue 1440: no wireplumber / pipewire) -- nothing can hold the HDMI PCM, no WirePlumber rule needed"
    return 0
  fi
  flavour="$(drm_output_audio_wp_flavour "$(wireplumber --version 2>/dev/null || true)")"
  if ! path="$(drm_output_audio_wp_rule_path "$flavour" "$root")"; then
    echo "  WARNING issue 1424: no WirePlumber version could be read -- the HDMI audio rule is NOT written (the fixed HDMI output's PCM may stay held by PipeWire)" >&2
    return 0
  fi
  want="$(drm_output_audio_wp_rule "$flavour")"
  if [ -f "$path" ] && [ "$(cat "$path" 2>/dev/null || true)" = "$want" ]; then
    echo "  ${path} already current (PipeWire holds no HDMI/DP PCM: the fixed HDMI output owns its audio)"
    return 0
  fi
  if install -d -m 0755 "$(dirname "$path")" && printf '%s\n' "$want" | install -m 0644 /dev/stdin "$path"; then
    echo "  wrote ${path} (WirePlumber ${flavour}: HDMI/DP nodes disabled; takes effect at the next audio session start)"
  else
    echo "  WARNING issue 1424: could not write ${path} -- PipeWire may keep the fixed HDMI output's PCM (verify names it)" >&2
  fi
  return 0
}

# drm_output_audio_pw_dump USER -> `pw-dump` of USER's PipeWire (as that user when run as root), or
# nothing. Bounded; always returns 0.
drm_output_audio_pw_dump() {
  local user="$1" uid
  uid="$(id -u "$user" 2>/dev/null || true)"
  [ -n "$uid" ] || return 0
  if [ "$(id -u)" = 0 ]; then
    sudo -u "$user" env XDG_RUNTIME_DIR="/run/user/${uid}" timeout 10 pw-dump 2>/dev/null || true
  else
    env XDG_RUNTIME_DIR="/run/user/${uid}" timeout 10 pw-dump 2>/dev/null || true
  fi
}

# drm_output_audio_verify_report USER_HOME USER LOG [ROOT [CONNECTOR [VIEW]]] -- verify (the HDMI output item):
# the WirePlumber rule file, PipeWire's live node list and the module's line in LOG. issue 1424 part 2:
# with CONNECTOR, only the audio lines naming 'CONNECTOR' count (each output has its own leg; on a
# two-output box the other leg's lines must never grade this one). With VIEW multiview, a leg that gave
# up (the monitor reports no audio, or a write failed) is a NOTE: that output never plays sound (issue
# 1424 review), the cutter's monitor may have no speakers. Uses the caller's ok/bad/note. Always returns 0.
drm_output_audio_verify_report() {
  local user="$2" log="${3:-}" root="${4:-}" conn="${5:-}" view="${6:-}" flavour path rule="missing" nodes dump state line verdict
  local text label="HDMI audio"
  nodes="?"
  if drm_output_audio_no_sound_server; then
    rule=none; nodes=0; path="no sound server on the box"   # issue 1440
  else
    flavour="$(drm_output_audio_wp_flavour "$(wireplumber --version 2>/dev/null || true)")"
    if ! path="$(drm_output_audio_wp_rule_path "$flavour" "$root")"; then
      rule=unknown
    elif [ -f "$path" ]; then
      rule=drift
      [ "$(cat "$path" 2>/dev/null || true)" = "$(drm_output_audio_wp_rule "$flavour")" ] && rule=ok
    fi
    dump="$(drm_output_audio_pw_dump "$user")"
    [ -n "$dump" ] && nodes="$(printf '%s\n' "$dump" | drm_output_audio_hdmi_node_count)"
  fi
  state=none
  line=""
  if [ -n "$log" ] && [ -f "$log" ]; then
    if [ -n "$conn" ]; then
      label="HDMI audio ${conn}"
      text="$(LC_ALL=C awk -v q="'${conn}'" 'index($0, "drm-output audio:") && index($0, q)' < "$log" || true)"
    else
      text="$(cat "$log" 2>/dev/null || true)"
    fi
    state="$(drm_output_audio_log_state <<<"$text")"
    line="$(drm_output_audio_log_line <<<"$text")"
  fi
  verdict="$(drm_output_audio_verdict "$rule" "$nodes" "$state" || true)"
  case "$verdict" in
    ok)                  ok   "${label}: the module writes the program mix to its connector's PCM (${line##*drm-output audio: }); PipeWire holds no HDMI node (${path})" ;;
    pipewire-unread)     note "${label}: the module opened its PCM (${line##*drm-output audio: }), but PipeWire's node list was not readable as ${user}" ;;
    wp-unknown)          bad  "${label}: no WirePlumber version could be read -- cannot check that PipeWire leaves the HDMI PCM to OBS" ;;
    rule-missing)        bad  "${label}: ${path} is missing -- re-run setup (PipeWire would hold the HDMI PCM the output needs)" ;;
    rule-drift)          bad  "${label}: ${path} is not the shipped rule -- re-run setup" ;;
    pipewire-holds-hdmi) bad  "${label}: PipeWire still has ${nodes} HDMI node(s) -- the rule applies at the next session start (as ${user}: systemctl --user restart wireplumber, then restart OBS)" ;;
    no-audio)            if [ "$view" = multiview ]; then
                           note "${label}: no audio leg (${line##*drm-output audio: }) -- the Multiview output plays no sound, report-only"
                         else
                           bad  "${label}: the module gave up -- ${line##*drm-output audio: }"
                         fi ;;
    *)                   bad  "${label}: the newest OBS log has no 'drm-output audio:' outcome line (an OBS build without the audio leg, or it is still resolving)" ;;
  esac
  return 0
}
