#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-input.sh -- issue 1485 (owner 2.11.2026: each application is its own Dante
# device; strih-snv only, PP and the stream role keep obs-vban). The strih OBS program audio comes in
# through the `dante_input` source of the pinned obs release (plugins/dante-input in zbynekdrlik/obs,
# dlink's Dante device). The plugin reads ~/.camera-box/dante-input.toml of the OBS user ONCE at module
# load (no file = no device and no source kind); the format is the release's dante-input.example.toml.
#
# Which box: one whose fact STRIH_DANTE_DEVICES names the device `strih-obs` (its ports
# arc:cmc:flows:rxfirst-rxlast). Such a box takes its program from Dante: the loader then refuses the
# VBAN program facts (STRIH_PROGRAM_VBAN_STREAMS / _PORT / _RESERVE_MS none) and keeps
# STRIH_PROGRAM_AUDIO_INPUT as the input's name. Every other box gets no file (a stale one we wrote is
# removed) and keeps obs-vban.
#
# The SITE data (which Dante transmitter and channel each RX channel subscribes) is never in this
# PUBLIC repo: it lives in an untracked per-box file the supervisor places, STRIH_DANTE_INPUT_SITE_FILE
# (default /etc/camera-box/dante-input.site.conf, root 0644), one line per RX channel in OBS input order:
#   RX1=<rx channel label>|<tx device>|<tx channel>
#   RX2=...
# 1-6 or 8 channels (the plugin's rule), RX numbers contiguous from 1, printable ASCII without `"`, `\`
# or `|` in a field. Setup refuses loudly when the box is on Dante and the file is missing or malformed.
#
#   strih_dante_input_install USER HOME   setup-strih step 13 (after the leg: the iface is the leg's)
#   strih_dante_input_grade LOG           verify-strih item 9's `(program-dante)` rows (ok/bad/note),
#                                         the `rx-pin` rows included (strih-dante-rx-pin.sh) and the
#                                         `reserved-cpu` rows (strih-dante-reserved-cpu.sh)
#   strih_program_audio_declared / strih_program_audio_grade SCN WS LOG   item 9's dispatcher: Dante,
#                                         else the obs-vban grade (strih-program-vban.sh)
#
# Seams: STRIH_DANTE_INPUT_SITE_FILE, STRIH_DANTE_INPUT_TOML (the destination), STRIH_DANTE_INPUT_IFACE
# (skip the leg resolve), STRIH_DANTE_INPUT_KIND_FILE (the OBS input kind instead of the WS read),
# STRIH_DANTE_INPUT_WAIT_S (the live window, 12 s).

# shellcheck source=scripts/lib/strih-dante-rx-pin.sh
. "$(dirname "${BASH_SOURCE[0]}")/strih-dante-rx-pin.sh"   # the Dante threads' own CPU (issue 1485)
# shellcheck source=scripts/lib/strih-dante-reserved-cpu.sh
. "$(dirname "${BASH_SOURCE[0]}")/strih-dante-reserved-cpu.sh"   # nothing else on that CPU (issue 1485)

STRIH_DANTE_INPUT_DEVICE="strih-obs"
STRIH_DANTE_INPUT_SOURCE_KIND="dante_input"
STRIH_DANTE_INPUT_NETWORK_RATE=96000
STRIH_DANTE_INPUT_LATENCY_MS=10
STRIH_DANTE_INPUT_HEADER="# ~/.camera-box/dante-input.toml -- written by camera-box setup-strih step 13 (issue 1485); every run rewrites it"

# strih_dante_input_ports DEVICES -> `arc cmc flows rx_port_base` of the strih-obs device in DEVICES (the
# fact's value); nothing + rc 1 when DEVICES names no strih-obs. Pure.
strih_dante_input_ports() {
  local d spec ports first
  for d in ${1:-}; do
    case "$d" in
      "${STRIH_DANTE_INPUT_DEVICE}="*)
        spec="${d#*=}"; ports="${spec%:*}"; first="${spec##*:}"; first="${first%%-*}"
        printf '%s %s\n' "${ports//:/ }" "$first"
        return 0 ;;
    esac
  done
  return 1
}

# strih_program_dante_declared -> rc 0 when the LOADED box takes its program from the Dante device.
strih_program_dante_declared() {
  local v
  v="$(strih_lx_dante_devices 2>/dev/null)" || return 1
  strih_dante_input_ports "$v" >/dev/null
}

strih_dante_input_site_file() { printf '%s' "${STRIH_DANTE_INPUT_SITE_FILE:-/etc/camera-box/dante-input.site.conf}"; }

# _strih_dante_input_field_ok TEXT -> rc 0 for 1..64 printable ASCII characters without `"`, `\` or `|`,
# no space first or last (a TOML basic string needs no escape then).
_strih_dante_input_field_ok() {
  local v="${1-}"
  [ -n "$v" ] && [ "${#v}" -le 64 ] || return 1
  [ -z "$(printf '%s' "$v" | LC_ALL=C tr -d '\040-\176')" ] || return 1
  case "$v" in *'"'* | *'\'* | *'|'* | ' '* | *' ') return 1 ;; esac
}

# strih_dante_input_site_rows FILE -> one `label<TAB>tx_device<TAB>tx_channel` line per RX channel, in RX
# order; rc 1 + every problem on stderr (the file's line named) when the file is missing or malformed.
strih_dante_input_site_rows() {
  local file="${1:?site file required}" line n=0 key val label dev chan bad=0 k
  local -A site_rows=()
  [ -f "$file" ] || { echo "dante-input: the site file ${file} is missing -- the supervisor places it (RX<n>=<rx label>|<tx device>|<tx channel>, never in git)" >&2; return 1; }
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1)); line="${line%$'\r'}"
    case "$line" in '' | '#'*) continue ;; esac
    key="${line%%=*}"; val="${line#*=}"
    if [[ ! "$key" =~ ^RX[12345678]$ ]] || [ "$key" = "$line" ]; then
      echo "dante-input: ${file} line ${n}: not RX<1-8>=<rx label>|<tx device>|<tx channel>" >&2; bad=1; continue
    fi
    IFS='|' read -r label dev chan k <<<"$val"
    if [ -n "${k:-}" ] || ! _strih_dante_input_field_ok "$label" || ! _strih_dante_input_field_ok "$dev" \
       || ! _strih_dante_input_field_ok "$chan"; then
      echo "dante-input: ${file} line ${n}: ${key} needs three fields <rx label>|<tx device>|<tx channel>, printable ASCII without quote, backslash or |" >&2; bad=1; continue
    fi
    [ -z "${site_rows[${key#RX}]:-}" ] || { echo "dante-input: ${file} line ${n}: ${key} twice" >&2; bad=1; continue; }
    site_rows[${key#RX}]="${label}"$'\t'"${dev}"$'\t'"${chan}"
  done <"$file"
  n="${#site_rows[@]}"
  case "$n" in 1 | 2 | 3 | 4 | 5 | 6 | 8) ;; *) echo "dante-input: ${file} declares ${n} RX channels; OBS takes 1 to 6 or 8" >&2; bad=1 ;; esac
  for ((k = 1; k <= n; k++)); do
    [ -n "${site_rows[$k]:-}" ] || { echo "dante-input: ${file} has no RX${k} (the RX numbers run 1..${n})" >&2; bad=1; }
  done
  [ "$bad" = 0 ] || return 1
  for ((k = 1; k <= n; k++)); do printf '%s\n' "${site_rows[$k]}"; done
}

# strih_dante_input_toml IFACE "ARC CMC FLOWS RXBASE" ROWS -> the dante-input.toml text (pure). ROWS =
# strih_dante_input_site_rows' output. network_rate 96000 (the Dante network; dlink halves it to OBS's
# 48 kHz), no sample_rate (OBS's own), latency 10 ms.
strih_dante_input_toml() {
  local iface="${1:?iface required}" ports="${2:?ports required}" chans="${3:?rows required}"
  local arc cmc flows base label dev chan rx="" i=0 subs=""
  read -r arc cmc flows base <<<"$ports"
  while IFS=$'\t' read -r label dev chan; do
    [ -n "$label" ] || continue
    i=$((i + 1))
    rx="${rx}${rx:+, }\"${label}\""
    subs="${subs}"$'\n'"[[subscribe]]"$'\n'"rx = ${i}"$'\n'"tx_channel = \"${chan}\""$'\n'"tx_device = \"${dev}\""$'\n'
  done <<<"$chans"
  printf '%s\n\n' "$STRIH_DANTE_INPUT_HEADER"
  printf 'name = "%s"\niface = "%s"\nnetwork_rate = %s\nlatency_ms = %s\nrx = [%s]\n' \
    "$STRIH_DANTE_INPUT_DEVICE" "$iface" "$STRIH_DANTE_INPUT_NETWORK_RATE" "$STRIH_DANTE_INPUT_LATENCY_MS" "$rx"
  printf 'arc_port = %s\ncmc_port = %s\nflows_port = %s\nrx_port_base = %s\n' "$arc" "$cmc" "$flows" "$base"
  printf '%s' "$subs"
}

# _strih_dante_input_dest HOME -> where the plugin reads it.
_strih_dante_input_dest() { printf '%s' "${STRIH_DANTE_INPUT_TOML:-${1%/}/.camera-box/dante-input.toml}"; }

# _strih_dante_input_iface -> the leg's interface (the Dante devices live on it, issue 1485).
_strih_dante_input_iface() {
  local res
  if [ -n "${STRIH_DANTE_INPUT_IFACE:-}" ]; then printf '%s\n' "$STRIH_DANTE_INPUT_IFACE"; return 0; fi
  res="$(strih_vban_leg_resolve)" || return 1
  res="${res##*$'\n'}"
  printf '%s\n' "${res%% *}"
}

# strih_dante_input_install USER HOME -> setup-strih step 13. A Dante box: the file from the facts + the
# site file, written as USER 0644 when it differs (obs_box_write_if_changed); a missing / malformed site
# file or an unresolvable leg fails (rc 1, named). Any other box: a file carrying our header is removed
# (an operator's own file is left, named). OBS reads it at its next start.
strih_dante_input_install() {
  local user="${1:?desktop user required}" home="${2:?home required}" dest ports chans iface group
  dest="$(_strih_dante_input_dest "$home")"
  if ! strih_program_dante_declared; then
    if [ -f "$dest" ] && head -n 1 "$dest" 2>/dev/null | grep -qxF "$STRIH_DANTE_INPUT_HEADER"; then
      rm -f "$dest" && echo "  dante-input: removed ${dest} (this box takes no Dante program)"
    elif [ -f "$dest" ]; then
      echo "  WARNING: ${dest} exists but this box's facts declare no ${STRIH_DANTE_INPUT_DEVICE} Dante device -- not ours, left alone" >&2
    else
      echo "  dante-input: no ${STRIH_DANTE_INPUT_DEVICE} Dante device on this box (STRIH_DANTE_DEVICES) -- no ${dest}"
    fi
    strih_dante_rx_pin_remove
    strih_dante_reserved_cpu_remove "$user"
    _strih_dante_input_jxs_cores "$home" "$user"
    return 0
  fi
  ports="$(strih_dante_input_ports "$(strih_lx_dante_devices)")" || return 1
  chans="$(strih_dante_input_site_rows "$(strih_dante_input_site_file)")" || return 1
  iface="$(_strih_dante_input_iface)" || { echo "dante-input: the leg's interface cannot be resolved (above)" >&2; return 1; }
  group="$(id -gn "$user" 2>/dev/null)" || { echo "dante-input: no user ${user}" >&2; return 1; }
  install -d -m 0755 -o "$user" -g "$group" "$(dirname "$dest")" || return 1
  obs_box_write_if_changed_rendered "$dest" 0644 "${user}:${group}" "dante-input" strih_dante_input_toml "$iface" "$ports" "$chans" || return 1
  echo "  dante-input: ${STRIH_DANTE_INPUT_DEVICE} on ${iface}, $(printf '%s\n' "$chans" | wc -l) RX channel(s) from $(strih_dante_input_site_file) -- OBS loads it at its next start"
  strih_dante_rx_pin_install "$user" || { echo "dante-input: the Dante threads' CPU reservation failed (above)" >&2; return 1; }
  strih_dante_reserved_cpu_install "$user" || { echo "dante-input: the managers' default off the reserved CPU failed (above)" >&2; return 1; }
  _strih_dante_input_jxs_cores "$home" "$user"
}

# _strih_dante_input_jxs_cores HOME USER -> the strih JPEG XS program encoder's cores re-derived now that
# step 11's OBS mask exists (strih-jxs-provision.sh; step 6 ran before it). rc 1 named on a failure.
_strih_dante_input_jxs_cores() {
  declare -F strih_jxs_output_cores_reconcile >/dev/null \
    || { echo "dante-input: strih_jxs_output_cores_reconcile is not defined -- source scripts/lib/strih-provision.sh" >&2; return 1; }
  strih_jxs_output_cores_reconcile "$1" "$2" || { echo "dante-input: the JPEG XS program encoder's cores (above)" >&2; return 1; }
}

# strih_dante_input_field LINE KEY -> the value of KEY=<value> in a dante-input line.
strih_dante_input_field() {
  printf ' %s\n' "$1" | sed -n "s/.*[[:space:]]$2=\([^ ]*\).*/\1/p" | head -n 1
}

# strih_dante_input_status_verdict LINE1 LINE2 -> `ok <text>` | `stale <text>` | `bad <text>` for two
# `dante-input status:` lines taken over the live window (pure).
strih_dante_input_status_verdict() {
  local l1="$1" l2="$2" k a b out="" bad=""
  [ -n "$l2" ] || { echo "bad no dante-input status: line in the OBS log"; return 0; }
  [ "$(strih_dante_input_field "$l2" clock)" = media ] || bad="${bad}clock=$(strih_dante_input_field "$l2" clock) (not on the dsync media clock); "
  [ "$(strih_dante_input_field "$l2" holdover)" = 0 ] || bad="${bad}holdover=$(strih_dante_input_field "$l2" holdover); "
  [ "$(strih_dante_input_field "$l2" gm)" != none ] || bad="${bad}gm=none (no grandmaster); "
  local grew=""
  # The obs project's field meanings (3.11.2026): these mean audio other than FOH sent. rx_late, overruns
  # and late_blocks are timing only, named below; a late packet that cost samples shows in rx_written_late.
  for k in rx_late overruns late_blocks; do
    a="$(strih_dante_input_field "$l2" "$k")"
    [[ "$a" =~ ^[0123456789]+$ ]] && [ "$a" != 0 ] && out="${out}${k}=${a} "
  done
  for k in rx_loss rx_holes underruns rx_gaps socket_drops rebootstraps rx_written_late obs_dropped; do
    a="$(strih_dante_input_field "$l2" "$k")"
    [[ "$a" =~ ^[0123456789]+$ ]] || { bad="${bad}${k} unreadable; "; continue; }
    [ "$a" = 0 ] || out="${out}${k}=${a} "
    # A counter that grows over the live window is a loss now (zero audio loss); its total since the
    # start alone is history, named but not graded.
    b="$(strih_dante_input_field "$l1" "$k")"
    if [ "$l1" != "$l2" ] && [[ "$b" =~ ^[0123456789]+$ ]] && [ "$a" -gt "$b" ]; then grew="${grew}${k} +$((a - b)) "; fi
  done
  if [ -n "$bad" ]; then echo "bad ${bad%; }"; return 0; fi
  if [ -n "$grew" ]; then echo "bad live: ${grew% } over the window -- audio lost while OBS runs"; return 0; fi
  a="$(strih_dante_input_field "$l1" delivered_frames)"; b="$(strih_dante_input_field "$l2" delivered_frames)"
  if [ "$l1" = "$l2" ] || ! [[ "$a" =~ ^[0123456789]+$ && "$b" =~ ^[0123456789]+$ ]] || [ "$b" -le "$a" ]; then
    echo "stale delivered_frames did not advance over the window (${a:-?} -> ${b:-?})"; return 0
  fi
  echo "ok delivered_frames ${a} -> ${b}, gm=$(strih_dante_input_field "$l2" gm), rx_packets=$(strih_dante_input_field "$l2" rx_packets)${out:+; since the start: ${out% }}"
}

# strih_dante_input_grade SCN_BIN WS_HOST LOG -> verify-strih item 9's `(program-dante)` rows (through the
# caller's ok / bad / note). Always rc 0.
strih_dante_input_grade() {
  local scn="${1-}" ws="${2-}" log="${3-}" name kind dest home user l1 l2 v start errs wait="${STRIH_DANTE_INPUT_WAIT_S:-12}"
  name="$(strih_lx_program_audio_input 2>/dev/null)" || name="ASIO zvuk"
  user="${STRIH_LX_USER:-newlevel}"
  home="$(getent passwd "$user" 2>/dev/null | cut -d: -f6 || true)"; home="${home:-/home/${user}}"
  dest="$(_strih_dante_input_dest "$home")"
  if [ -f "$dest" ] && grep -q "^name = \"${STRIH_DANTE_INPUT_DEVICE}\"$" "$dest" 2>/dev/null; then
    ok "(program-dante) ${dest} declares the ${STRIH_DANTE_INPUT_DEVICE} device ($(grep -c '^\[\[subscribe\]\]' "$dest") subscription(s))"
  else
    bad "(program-dante) ${dest} is missing or names no ${STRIH_DANTE_INPUT_DEVICE} device -- re-run setup-strih.sh step 13 (the site file $(strih_dante_input_site_file) must exist), then restart OBS"
  fi
  if [ -n "${STRIH_DANTE_INPUT_KIND_FILE:-}" ]; then kind="$(cat "$STRIH_DANTE_INPUT_KIND_FILE" 2>/dev/null || true)"
  else kind="$(python3 "$scn" --host "$ws" --audio-input-kind 2>/dev/null || echo absent)"; fi
  if [ "$kind" = "$STRIH_DANTE_INPUT_SOURCE_KIND" ]; then
    ok "(program-dante) OBS '${name}' is the ${STRIH_DANTE_INPUT_SOURCE_KIND} source"
  else
    bad "(program-dante) OBS '${name}' is '${kind:-absent}', not the ${STRIH_DANTE_INPUT_SOURCE_KIND} source -- restart OBS so strih_scenes.py --bootstrap switches it (the bundle must be the obs release with dante-input.so)"
  fi
  if [ -z "$log" ] || [ ! -r "$log" ]; then bad "(program-dante) no readable OBS log -- the device is not graded"; return 0; fi
  start="$(LC_ALL=C grep -a 'dante-input start: config=' "$log" | tail -n 1 || true)"
  if [ -z "$start" ]; then
    bad "(program-dante) no 'dante-input start: config=' line in ${log}: the device did not start (no dante-input.toml when OBS started, or the module is missing)"
  else
    # the rate conversion: obs v0.4.1+ (dstack #90 `Rated`) logs converts=, v0.4.0 logged halved=
    local conv_key=halved
    # decided by the key being PRESENT, never by its value (an empty converts= is graded as converts)
    [[ " ${start}" =~ [[:space:]]converts= ]] && conv_key=converts
    if [ "$(strih_dante_input_field "$start" device_rate)" = "$STRIH_DANTE_INPUT_NETWORK_RATE" ] \
         && [ "$(strih_dante_input_field "$start" "$conv_key")" = true ]; then
      ok "(program-dante) started: device_rate=$(strih_dante_input_field "$start" device_rate) ${conv_key}=true sample_rate=$(strih_dante_input_field "$start" sample_rate) iface=$(strih_dante_input_field "$start" iface)"
    else
      bad "(program-dante) start line reads device_rate=$(strih_dante_input_field "$start" device_rate) ${conv_key}=$(strih_dante_input_field "$start" "$conv_key"), wants ${STRIH_DANTE_INPUT_NETWORK_RATE} / true"
    fi
  fi
  errs="$(LC_ALL=C grep -a -c 'dante-input error:' "$log" || true)"
  if [ "${errs:-0}" != 0 ]; then
    bad "(program-dante) ${errs} 'dante-input error:' line(s) in ${log}, the last: $(LC_ALL=C grep -a 'dante-input error:' "$log" | tail -n 1 | sed 's/^.*dante-input error: //')"
  fi
  l1="$(LC_ALL=C grep -a 'dante-input status:' "$log" | tail -n 1 || true)"
  sleep "$wait"
  l2="$(LC_ALL=C grep -a 'dante-input status:' "$log" | tail -n 1 || true)"
  v="$(strih_dante_input_status_verdict "$l1" "$l2")"
  case "$v" in
    ok*) ok "(program-dante) live: ${v#ok }" ;;
    stale*) bad "(program-dante) ${v#stale } -- no Dante audio reaches OBS (the FOH subscription, the leg filter, the clock)" ;;
    *) bad "(program-dante) ${v#bad }" ;;
  esac
  strih_dante_reserved_cpu_grade
  strih_dante_rx_pin_grade
  return 0
}

# strih_program_audio_declared -> rc 0 when the loaded box's program audio is graded by item 9's direct
# rows: the Dante device (issue 1485) or the console's VBAN (issue 1439).
strih_program_audio_declared() { strih_program_dante_declared || strih_program_vban_declared; }

# strih_program_audio_grade SCN_BIN WS_HOST LOG -> the Dante grade on a Dante box, else the obs-vban grade.
strih_program_audio_grade() {
  if strih_program_dante_declared; then strih_dante_input_grade "$@"; else strih_program_vban_grade "$@"; fi
}
