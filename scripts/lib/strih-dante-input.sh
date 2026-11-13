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
# Which box: one whose fact STRIH_DANTE_DEVICES names the program device `<STRIH_ROLE>-obs` (its ports
# arc:cmc:flows:rxfirst-rxlast): `strih-obs` on a strih box (strih-snv), `stream-obs` on a stream box
# (stream-snv since issue 1495, distinct from mbc's own Dante device `mbc-snv`). Such a box takes its program
# from Dante: the loader then refuses the VBAN program facts (STRIH_PROGRAM_VBAN_STREAMS / _PORT /
# _RESERVE_MS none) and keeps STRIH_PROGRAM_AUDIO_INPUT as the input's name (`ASIO zvuk`, `mbc`). Every
# other box gets no file (a stale one we wrote is removed) and keeps obs-vban.
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
#                                         the `media clock` + `ptp-port` rows (strih-dante-ptp.sh)
#   strih_program_audio_declared / strih_program_audio_grade SCN WS LOG   item 9's dispatcher: the
#                                         `(dante-cpu)` leftover rows (strih-dante-cpu-leftovers.sh), then
#                                         Dante, else the obs-vban grade (strih-program-vban.sh)
#
# No CPU pinning for the Dante threads (dstack v0.6.28, owner ruling relayed by dstack issue 1, 4.11.2026):
# step 13 removes what the earlier CPU reservation left on a box (strih-dante-cpu-leftovers.sh). The latency
# is the box facts' NAMED terms since obs v0.4.27 (issues 1510 + 1513): `network_latency_ms` + `receive_buffer_ms`
# (never `latency_ms`, which the release refuses), and on stream-snv the program TX (`tx` + `tx_track`).
#
# Seams: STRIH_DANTE_INPUT_SITE_FILE, STRIH_DANTE_INPUT_TOML (the destination), STRIH_DANTE_INPUT_IFACE
# (skip the leg resolve), STRIH_DANTE_INPUT_KIND_FILE (the OBS input kind instead of the WS read),
# STRIH_DANTE_INPUT_WAIT_S (the live window, 12 s).

# shellcheck source=scripts/lib/strih-dante-cpu-leftovers.sh
. "$(dirname "${BASH_SOURCE[0]}")/strih-dante-cpu-leftovers.sh"   # the removed CPU reservation, off the box
if ! declare -F strih_dante_ptp_pin_ready >/dev/null; then
  # shellcheck source=scripts/lib/strih-dante-ptp.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-dante-ptp.sh"   # in-process PTP: the 319 bind right + the media clock row
fi

# The strih role's device name (issue 1485); the loaded box's own is strih_dante_input_device.
STRIH_DANTE_INPUT_DEVICE="strih-obs"
STRIH_DANTE_INPUT_SOURCE_KIND="dante_input"
STRIH_DANTE_INPUT_NETWORK_RATE=96000
STRIH_DANTE_INPUT_HEADER="# ~/.camera-box/dante-input.toml -- written by camera-box setup-strih step 13 (issue 1485); every run rewrites it"

# strih_dante_input_device -> the loaded box's program Dante device: `<STRIH_ROLE>-obs` (issue 1495).
strih_dante_input_device() {
  local role
  role="$(strih_box_role 2>/dev/null)" || role=strih
  case "$role" in stream) printf 'stream-obs\n' ;; *) printf '%s\n' "$STRIH_DANTE_INPUT_DEVICE" ;; esac
}

# strih_dante_input_ports DEVICES [NAME] -> `arc cmc flows rx_port_base` of the program device NAME (default
# the loaded box's, strih_dante_input_device) in DEVICES (the fact's value); nothing + rc 1 when DEVICES names
# no such device.
strih_dante_input_ports() {
  local d spec ports first name="${2-}"
  [ -n "$name" ] || name="$(strih_dante_input_device)"
  for d in ${1:-}; do
    case "$d" in
      "${name}="*)
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
# 48 kHz), no sample_rate (OBS's own), the facts' latency terms + TX (strih_dante_input_terms_toml).
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
  printf 'name = "%s"\niface = "%s"\nnetwork_rate = %s\nrx = [%s]\n' \
    "$(strih_dante_input_device)" "$iface" "$STRIH_DANTE_INPUT_NETWORK_RATE" "$rx"
  printf 'arc_port = %s\ncmc_port = %s\nflows_port = %s\nrx_port_base = %s\n' "$arc" "$cmc" "$flows" "$base"
  strih_dante_input_terms_toml || return 1   # a failed fact read never writes a file without the terms
  printf '%s' "$subs"
}

# _strih_dante_input_float MS -> MS as a TOML float (`1` -> `1.0`; the obs release reads an f64).
_strih_dante_input_float() { case "$1" in *.*) printf '%s' "$1" ;; *) printf '%s.0' "$1" ;; esac; }

# strih_dante_input_terms_toml -> the loaded box's named latency terms + program TX as dante-input.toml lines
# (issues 1510 + 1513, obs v0.4.27 / dstack 244): `network_latency_ms` (a) + `receive_buffer_ms` (b) = the Dante
# latency P, and on a TX box `tx` + `tx_track`. A `none` fact writes no line. Before the [[subscribe]] tables.
strih_dante_input_terms_toml() {
  local net rb tx tr
  net="$(strih_box_fact STRIH_DANTE_NETWORK_LATENCY_MS)" && rb="$(strih_box_fact STRIH_DANTE_RECEIVE_BUFFER_MS)" \
    && tx="$(strih_box_fact STRIH_DANTE_TX)" && tr="$(strih_box_fact STRIH_DANTE_TX_TRACK)" || return 1
  [ "$net" = none ] || printf 'network_latency_ms = %s\n' "$(_strih_dante_input_float "$net")"
  [ "$rb" = none ] || printf 'receive_buffer_ms = %s\n' "$(_strih_dante_input_float "$rb")"
  [ "$tx" = none ] || printf 'tx = ["%s", "%s"]\ntx_track = %s\n' "${tx%%,*}" "${tx#*,}" "$tr"
}

# strih_dante_input_expected_latency_ns -> the Dante latency P = (a) + (b) of the loaded box in ns (rounded).
strih_dante_input_expected_latency_ns() {
  LC_ALL=C awk -v a="$(strih_box_fact STRIH_DANTE_NETWORK_LATENCY_MS)" -v b="$(strih_box_fact STRIH_DANTE_RECEIVE_BUFFER_MS)" \
    'BEGIN { if (a == "none" || b == "none") exit 1; printf "%d\n", (a + b) * 1000000 + 0.5 }'
}

# _strih_dante_input_line_ms LINE -> the OBS log line's HH:MM:SS.mmm prefix in ms since midnight (rc 1 without).
_strih_dante_input_line_ms() {
  local d='[0123456789]'
  [[ "$1" =~ ^(${d}{2}):(${d}{2}):(${d}{2})\.(${d}{3}) ]] || return 1
  printf '%s\n' "$(( (10#${BASH_REMATCH[1]} * 3600 + 10#${BASH_REMATCH[2]} * 60 + 10#${BASH_REMATCH[3]}) * 1000 + 10#${BASH_REMATCH[4]} ))"
}

# strih_dante_input_terms_toml_grade DEST -> the `(program-dante) latency terms` row: DEST carries exactly the
# facts' term + TX lines (issues 1510 + 1513). Through the caller's ok / bad.
strih_dante_input_terms_toml_grade() {
  local dest="$1" want line miss=""
  want="$(strih_dante_input_terms_toml)" || { bad "(program-dante) latency terms: the box facts are unreadable"; return 0; }
  while IFS= read -r line; do
    [ -z "$line" ] || grep -qxF -- "$line" "$dest" 2>/dev/null || miss="${miss}${miss:+; }${line%% =*}"
  done <<<"$want"
  if [[ "$want" != *"tx = "* ]] && grep -qE '^(tx|tx_track) =' "$dest" 2>/dev/null; then miss="${miss}${miss:+; }a tx the facts do not declare"; fi
  if [ -n "$miss" ]; then
    bad "(program-dante) latency terms in ${dest} differ from the box facts (${miss}) -- re-run setup-strih.sh step 13, then restart OBS"
  else
    ok "(program-dante) latency terms in ${dest}: $(printf '%s' "$want" | tr '\n' ' ' | sed 's/ $//')"
  fi
}

# strih_dante_input_tx_grade LOG L1 L2 -> verify item 9's TX rows (issue 1513) + the dante latency and the
# network-latency NOTE (issue 1510). L1 / L2 = the status lines of the live window. Through ok / bad / note.
strih_dante_input_tx_grade() {
  local log="$1" l1="$2" l2="$3" want split got tx tr k a b t1 t2 grew="" hist="" probs=() rate line
  want="$(strih_dante_input_expected_latency_ns 2>/dev/null || true)"
  split="$(LC_ALL=C grep -a 'dante-input start: latency split' "$log" | tail -n 1 || true)"
  got="$(strih_dante_input_field "$split" dante_latency_ns)"
  if [ -z "$split" ]; then
    bad "(program-dante) no 'dante-input start: latency split' line in ${log} (an obs release before v0.4.27, or the device did not start)"
  elif [[ "$got" =~ ^[0123456789]+$ ]] && [ -n "$want" ] && [ "$((got > want ? got - want : want - got))" -le 1000 ]; then
    ok "(program-dante) dante latency ${got} ns = (a) $(strih_box_fact STRIH_DANTE_NETWORK_LATENCY_MS) + (b) $(strih_box_fact STRIH_DANTE_RECEIVE_BUFFER_MS) ms (the start line's split)"
  else
    bad "(program-dante) dante latency ${got:-?} ns, the facts say ${want:-?} ns -- OBS runs another config: restart OBS (a latency change needs the owner, issue 1510)"
  fi
  # dlink's own statement of the terms (live 13.11.2026: `dlink latency: Dante latency P 13.041666 ms = network latency
  # (a) 1 ms (default) + receive buffer (b) 12.041666 ms (default)`), named; `(default)` = OBS read no term
  line="$(LC_ALL=C grep -a 'dlink latency: Dante latency P' "$log" | tail -n 1 | sed 's/^.*dlink latency: /dlink latency: /' || true)"
  if [ -n "$line" ]; then
    note "(program-dante) ${line}$([[ "$line" != *"(default)"* ]] || printf '%s' ' -- a term reads (default) while the box facts declare it: restart OBS onto the TOML')"
  fi
  tx="$(strih_box_fact STRIH_DANTE_TX)"; tr="$(strih_box_fact STRIH_DANTE_TX_TRACK)"; k="$(strih_dante_input_field "$l2" tx_track)"
  if [ "$tx" = none ]; then
    if [ -z "$l2" ]; then :
    elif [ -z "$k" ]; then note "(program-dante) tx: the status line carries no tx_track (an obs release before v0.4.27)"
    elif [ "$k" = 0 ]; then ok "(program-dante) tx: none on this device (tx_track=0)"
    else bad "(program-dante) tx: tx_track=${k} on a device whose facts declare no TX -- re-run setup-strih.sh step 13, then restart OBS"; fi
  else
    line="$(LC_ALL=C grep -a "dante-input start: TX of OBS track ${tr} " "$log" | tail -n 1 || true)"
    if [ -n "$line" ]; then ok "(program-dante) TX of OBS track ${tr}: obs_to_wire_ns=$(strih_dante_input_field "$line" obs_to_wire_ns) (computed by dlink)"
    else bad "(program-dante) no 'dante-input start: TX of OBS track ${tr}' line in ${log}: the program TX did not start (the TOML's tx keys, then restart OBS)"; fi
    if [ -n "$l2" ] && [ "$k" != "$tr" ]; then
      bad "(program-dante) tx: tx_track=${k:-absent}, the facts send track ${tr} (an obs release before v0.4.27, or OBS started without the tx keys)"
    elif [ -n "$l2" ]; then
      [ "$(strih_dante_input_field "$l2" tx_priming)" = 0 ] || probs+=("tx_priming=$(strih_dante_input_field "$l2" tx_priming) (the TX player waits for its FIFO: silence goes out)")
      for k in tx_underruns tx_recentres tx_overflows; do
        a="$(strih_dante_input_field "$l1" "$k")"; b="$(strih_dante_input_field "$l2" "$k")"
        [[ "$b" =~ ^[0123456789]+$ ]] || { probs+=("${k} unreadable"); continue; }
        [ "$b" = 0 ] || hist="${hist}${k}=${b} "
        if [ "$l1" != "$l2" ] && [[ "$a" =~ ^[0123456789]+$ ]] && [ "$b" -gt "$a" ]; then grew="${grew}${k} +$((b - a)) "; fi
      done
      [ -z "$grew" ] || probs+=("${grew% } over the window -- program audio to Dante lost while OBS runs")
      a="$(strih_dante_input_field "$l1" tx_played)"; b="$(strih_dante_input_field "$l2" tx_played)"
      if [ "$l1" = "$l2" ] || ! [[ "$a" =~ ^[0123456789]+$ && "$b" =~ ^[0123456789]+$ ]] || [ "$b" -le "$a" ]; then
        probs+=("tx_played did not advance over the window (${a:-?} -> ${b:-?})")
      fi
      if [ "${#probs[@]}" -gt 0 ]; then
        for k in "${probs[@]}"; do bad "(program-dante) tx: ${k}"; done
      else
        ok "(program-dante) tx: tx_played ${a} -> ${b}, tx_ppm=$(strih_dante_input_field "$l2" tx_ppm)${hist:+; since the start: ${hist% }}"
        if t1="$(_strih_dante_input_line_ms "$l1")" && t2="$(_strih_dante_input_line_ms "$l2")"; then
          [ "$t2" -ge "$t1" ] || t2=$((t2 + 86400000))
          if [ "$t2" -gt "$t1" ]; then
            rate=$(( (b - a) * 1000 / (t2 - t1) ))
            [ "$rate" -ge 43200 ] && [ "$rate" -le 52800 ] \
              || note "(program-dante) tx rate ${rate} frames/s over the window (48000 expected from the app rate; report-only)"
          fi
        fi
      fi
    fi
  fi
  line="$(LC_ALL=C grep -a 'network_latency_p99_us=' "$log" | tail -n 1 || true)"
  if [ -n "$line" ]; then
    note "(program-dante) network latency (report-only): min $(strih_dante_input_field "$line" network_latency_min_us) us p99 $(strih_dante_input_field "$line" network_latency_p99_us) us max $(strih_dante_input_field "$line" network_latency_max_us) us (the newest flow line)"
  else
    note "(program-dante) network latency (report-only): no network_latency_p99_us field in ${log}"
  fi
  return 0
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
# (an operator's own file is left, named). OBS reads it at its next start. Every box first loses the
# removed CPU reservation's leftovers (strih_dante_cpu_leftovers_remove).
strih_dante_input_install() {
  local user="${1:?desktop user required}" home="${2:?home required}" dest ports chans iface group
  dest="$(_strih_dante_input_dest "$home")"
  strih_dante_cpu_leftovers_remove "$user" || { echo "dante-input: the removed CPU reservation's leftovers (above)" >&2; return 1; }
  if ! strih_program_dante_declared; then
    if [ -f "$dest" ] && head -n 1 "$dest" 2>/dev/null | grep -qxF "$STRIH_DANTE_INPUT_HEADER"; then
      rm -f "$dest" && echo "  dante-input: removed ${dest} (this box takes no Dante program)"
    elif [ -f "$dest" ]; then
      echo "  WARNING: ${dest} exists but this box's facts declare no $(strih_dante_input_device) Dante device -- not ours, left alone" >&2
    else
      echo "  dante-input: no $(strih_dante_input_device) Dante device on this box (STRIH_DANTE_DEVICES) -- no ${dest}"
    fi
    strih_dante_ptp_port_remove || return 1
    return 0
  fi
  ports="$(strih_dante_input_ports "$(strih_lx_dante_devices)")" || return 1
  chans="$(strih_dante_input_site_rows "$(strih_dante_input_site_file)")" || return 1
  iface="$(_strih_dante_input_iface)" || { echo "dante-input: the leg's interface cannot be resolved (above)" >&2; return 1; }
  group="$(id -gn "$user" 2>/dev/null)" || { echo "dante-input: no user ${user}" >&2; return 1; }
  install -d -m 0755 -o "$user" -g "$group" "$(dirname "$dest")" || return 1
  obs_box_write_if_changed_rendered "$dest" 0644 "${user}:${group}" "dante-input" strih_dante_input_toml "$iface" "$ports" "$chans" || return 1
  echo "  dante-input: $(strih_dante_input_device) on ${iface}, $(printf '%s\n' "$chans" | wc -l) RX channel(s) from $(strih_dante_input_site_file) -- OBS loads it at its next start"
  strih_dante_ptp_port_install || { echo "dante-input: the right to bind UDP 319 for the in-process PTP follower failed (above)" >&2; return 1; }
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
  local scn="${1-}" ws="${2-}" log="${3-}" name kind dest home user l1 l2 v start errs wait="${STRIH_DANTE_INPUT_WAIT_S:-12}" dev
  name="$(strih_lx_program_audio_input 2>/dev/null)" || name="ASIO zvuk"
  user="${STRIH_LX_USER:-newlevel}"
  home="$(getent passwd "$user" 2>/dev/null | cut -d: -f6 || true)"; home="${home:-/home/${user}}"
  dest="$(_strih_dante_input_dest "$home")"
  dev="$(strih_dante_input_device)"
  if [ -f "$dest" ] && grep -q "^name = \"${dev}\"$" "$dest" 2>/dev/null; then
    ok "(program-dante) ${dest} declares the ${dev} device ($(grep -c '^\[\[subscribe\]\]' "$dest") subscription(s))"
  else
    bad "(program-dante) ${dest} is missing or names no ${dev} device -- re-run setup-strih.sh step 13 (the site file $(strih_dante_input_site_file) must exist), then restart OBS"
  fi
  strih_dante_input_terms_toml_grade "$dest"   # issues 1510 + 1513: the named latency terms + the program TX
  if [ -n "${STRIH_DANTE_INPUT_KIND_FILE:-}" ]; then kind="$(cat "$STRIH_DANTE_INPUT_KIND_FILE" 2>/dev/null || true)"
  else kind="$(python3 "$scn" --host "$ws" --audio-input-kind 2>/dev/null || echo absent)"; fi
  if [ "$kind" = "$STRIH_DANTE_INPUT_SOURCE_KIND" ]; then
    ok "(program-dante) OBS '${name}' is the ${STRIH_DANTE_INPUT_SOURCE_KIND} source"
  else
    bad "(program-dante) OBS '${name}' is '${kind:-absent}', not the ${STRIH_DANTE_INPUT_SOURCE_KIND} source -- restart OBS so strih_scenes.py --bootstrap switches it (the bundle must be the obs release with dante-input.so)"
  fi
  # issue 1495 review: the per-channel meter row is source-agnostic (OBS's own meter of the input)
  strih_program_vban_channels_row "$ws" "$name" "(program-dante)" "a channel arrives empty: the transmitter's channel or the site file's subscription (RX<n>), issue 1495"
  strih_dante_ptp_port_grade   # reads no log: graded even when the log is not readable
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
  strih_dante_media_clock_grade "(program-dante)" obs < "$log"
  strih_dante_input_tx_grade "$log" "$l1" "$l2"   # issues 1510 + 1513: the dante latency, the TX tokens, the network-latency NOTE
  # issue 1485: timing signals, report-only (a pump WARN that lost blocks also raises the gated rebootstraps)
  local tl pw
  tl="$(LC_ALL=C grep -a -c 'dante-input timing:' "$log" || true)"
  pw="$(LC_ALL=C grep -a -c 'engine pump lost or overran blocks' "$log" || true)"
  note "(program-dante) timing (report-only): ${tl:-0} 'dante-input timing:' line(s), ${pw:-0} pump WARN line(s) in this OBS log"
  return 0
}

# strih_program_audio_declared -> rc 0 when the loaded box's program audio is graded by item 9's direct
# rows: the Dante device (issue 1485) or the console's VBAN (issue 1439).
strih_program_audio_declared() { strih_program_dante_declared || strih_program_vban_declared; }

# strih_program_audio_grade SCN_BIN WS_HOST LOG -> the removed CPU reservation's leftovers, then the Dante
# grade on a Dante box, else the obs-vban grade.
strih_program_audio_grade() {
  strih_dante_cpu_leftovers_grade
  if strih_program_dante_declared; then strih_dante_input_grade "$@"; else strih_program_vban_grade "$@"; fi
}
