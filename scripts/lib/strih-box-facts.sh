#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih box-fact loader, no top-level statements
# besides constants) -- matches the sibling scripts/lib/*.sh convention (strih-provision.sh,
# obs-fleet.sh) of NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict
# mode here would leak into it. Each caller (setup-strih.sh / verify-strih.sh) sets its own.
#
# scripts/lib/strih-box-facts.sh -- issue 1361: the ONE loader for the per-box strih FACT files
# (`scripts/strih-boxes/<box>.env`). setup-strih.sh / verify-strih.sh select a box with `--box <name>`
# (default: STRIH_BOX_DEFAULT) and every box/venue identity value (hostname, IP, NDI prefix, dantesync
# role + upstream, intercom config, NIC rule + its out-of-tree driver and speeds, OBS profile/collection,
# NDI-runtime peer, Companion controller, CG sender, cameras) is read from the loaded file -- one script
# for every strih box, never a per-box copy (the unified-design ruling, umbrella issue 1357).
#
# The fact file is PARSED, never sourced/executed: each non-comment line must be `KEY=value`, the value
# is taken literally and may not carry a shell metacharacter. Loading REFUSES (rc 1, one stderr line per
# problem) on: an unknown / duplicate / missing key, an unsafe value, any `TODO_OWNER` value (the
# not-yet-decided marker of a template such as strih-pp.env -- each one named), and a malformed fact
# (see strih_box_validate). Run-time facts (PL1 watts, the CPU plan, the NIC interface) are NOT facts
# here: setup-strih.sh derives them on the box.

# The box a bare setup-strih.sh / verify-strih.sh (no --box) provisions -- today's production strih.
STRIH_BOX_DEFAULT=strih-snv

# The not-yet-decided marker a template fact carries until the owner answers it.
STRIH_BOX_TODO=TODO_OWNER

# Characters a fact value may never carry (see _strih_box_unsafe).
STRIH_BOX_UNSAFE_CHARS='$`\"'"'"';|&<>*?!{}[]~#'

# strih_box_fact_keys -> every fact key, one per line (the fact-file contract; all are required).
strih_box_fact_keys() {
  printf '%s\n' \
    STRIH_HOSTNAME STRIH_IP STRIH_NDI_PREFIX \
    STRIH_DANTESYNC_ROLE STRIH_DANTESYNC_UPSTREAM \
    STRIH_INTERCOM_CONFIG STRIH_NIC_DRIVER \
    STRIH_NIC_OOT_DRIVER STRIH_NIC_MIN_USB_MBPS STRIH_NIC_MIN_LINK_MBPS \
    STRIH_OBS_PROFILE STRIH_OBS_COLLECTION \
    STRIH_NDI_RUNTIME_PEER STRIH_COMPANION_HOST STRIH_CG_SENDER STRIH_CAMERAS \
    STRIH_HDMI_OUTPUT_BACKEND \
    STRIH_ROLE STRIH_DANTE_GM STRIH_PROGRAM_AUDIO_SAMPLER \
    STRIH_PROGRAM_VBAN_STREAMS STRIH_PROGRAM_VBAN_PORT STRIH_PROGRAM_VBAN_RESERVE_MS \
    STRIH_PROGRAM_AUDIO_INPUT \
    STRIH_PROGRAM_VBAN_LEG STRIH_PROGRAM_VBAN_LEG_ADDR STRIH_PROGRAM_VBAN_SENDER \
    STRIH_PROGRAM_SENDER STRIH_VENUE STRIH_CAMERA_DEV1_NET \
    STRIH_HTTPS_INTERKOM_HOSTS STRIH_HTTPS_SHADING_HOST \
    STRIH_HDMI_OUTPUTS STRIH_HTTPS_PUBLIC \
    STRIH_STREAM_DEV_SCENE \
    STRIH_BKSHADING_RELAYS \
    STRIH_AUDIO_BUFFERING_MS \
    STRIH_JXS_CAMERAS STRIH_JXS_CAMERA_PIN_MS \
    STRIH_JXS_PROGRAM_PORT STRIH_JXS_PROGRAM_PIN_MS
}

# strih_box_repo_root -> the repo root this lib lives in (scripts/lib/../..).
strih_box_repo_root() {
  (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
}

# strih_box_dir -> the fact-file directory. STRIH_BOXES_DIR overrides it (the test seam).
strih_box_dir() {
  if [ -n "${STRIH_BOXES_DIR:-}" ]; then
    printf '%s' "${STRIH_BOXES_DIR%/}"
  else
    printf '%s/scripts/strih-boxes' "$(strih_box_repo_root)"
  fi
}

# _strih_box_unsafe VALUE -> 0 iff VALUE carries a character a fact value must never hold (a shell
# metacharacter, a quote, a glob, a control character, any non-ASCII byte). Values are later
# embedded in generated configs and commands, so
# they stay plain text: letters, digits, space, `.` `-` `_` `/` `:` `(` `)` `,` `=` `+` `@` `%`.
_strih_box_unsafe() {
  # C locale: a non-ASCII byte is "not printable" whatever locale the caller runs under.
  local LC_ALL=C
  local v="${1-}" i c
  # a control character (tab, CR from a CRLF file, ESC, ...) would corrupt the generated JSON/units.
  [[ "$v" == *[^[:print:]]* ]] && return 0
  for ((i = 0; i < ${#STRIH_BOX_UNSAFE_CHARS}; i++)); do
    c="${STRIH_BOX_UNSAFE_CHARS:i:1}"
    [[ "$v" == *"$c"* ]] && return 0
  done
  return 1
}

# strih_box_parse_file FILE -> print the file's `KEY=value` lines (comments/blank lines dropped) on
# stdout; rc 1 with one `strih-box: ...` stderr line per syntax problem (a non KEY=value line, an
# unsafe character, an unknown or duplicate key). Never executes the file.
strih_box_parse_file() {
  local file="${1:?fact file required}" line n=0 key val bad=0 seen=" " known
  known=" $(strih_box_fact_keys | tr '\n' ' ') "
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    case "$line" in ''|'#'*) continue ;; esac
    if [[ ! "$line" =~ ^[A-Z][A-Z0-9_]*= ]]; then
      echo "strih-box: ${file}:${n}: not a KEY=value line: ${line}" >&2
      bad=1; continue
    fi
    key="${line%%=*}"; val="${line#*=}"
    if _strih_box_unsafe "$val"; then
      echo "strih-box: ${file}:${n}: unsafe character in ${key} (values are literal text, never shell)" >&2
      bad=1; continue
    fi
    case "$known" in *" ${key} "*) : ;; *)
      echo "strih-box: ${file}:${n}: unknown fact ${key}" >&2
      bad=1; continue ;;
    esac
    case "$seen" in *" ${key} "*)
      echo "strih-box: ${file}:${n}: duplicate fact ${key}" >&2
      bad=1; continue ;;
    esac
    seen="${seen}${key} "
    printf '%s=%s\n' "$key" "$val"
  done < "$file"
  [ "$bad" = 0 ]
}

# _strih_box_host VALUE -> 0 iff VALUE is a host name / address: letters, digits, `.` `-`, starting
# with a letter or digit (a `-`-led value would reach a command line as a FLAG, e.g. `--master`).
_strih_box_host() {
  [[ "${1-}" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]]
}

# _strih_box_ipv4 VALUE -> 0 iff VALUE is a dotted-quad IPv4 address (each octet 0-255).
_strih_box_ipv4() {
  local ip="${1-}" o
  [[ "$ip" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
  for o in ${ip//./ }; do [ "$((10#$o))" -le 255 ] || return 1; done
}

# _strih_box_https_name NAME -> 0 iff NAME is a lower-case DNS name of two or more labels (letters,
# digits, inner `-`; a label never starts or ends with `-`). The character sets are spelled out: a
# bracket RANGE follows the locale in bash (ci-testing-gotchas.md, issue 1302).
_strih_box_https_name() {
  local l='[abcdefghijklmnopqrstuvwxyz0123456789]' m='[abcdefghijklmnopqrstuvwxyz0123456789-]'
  [[ "${1-}" =~ ^${l}(${m}*${l})?(\.${l}(${m}*${l})?)+$ ]]
}

# strih_box_hdmi_outputs_ok VALUE -> 0 iff VALUE is 1..2 space-separated CONNECTOR:VIEW[:noaudio] tokens
# (CONNECTOR = an X RandR output name of letters, digits, `.` `_` `-`; VIEW = program | multiview;
# `:noaudio` = the connector's PCM is owned elsewhere, issue 1424 part 3), no connector twice. The character set is spelled out (a bracket RANGE follows the locale in bash). The ONE grammar of
# the fact: scripts/lib/strih-drm-output.sh (step 6, verify item 4c) calls it too (issue 1424 review).
strih_box_hdmi_outputs_ok() {
  local c='[ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-]' t n=0 seen=" "
  for t in ${1-}; do
    [[ "$t" =~ ^${c}+:(program|multiview)(:noaudio)?$ ]] || return 1
    case "$seen" in *" ${t%%:*} "*) return 1 ;; esac
    seen="${seen}${t%%:*} "
    n=$((n + 1))
  done
  [ "$n" -ge 1 ] && [ "$n" -le 2 ]
}

# Issue 1439: the bounds of the obs-vban source's fixed "Receive Buffer" (R, ms), the same clamp as
# VBAN_RECV_RESERVE_MS_MIN / _MAX in vendor/obs-vban/src/vban-receive-timeline.h (test-pinned).
STRIH_BOX_VBAN_RESERVE_MS_MIN=10
STRIH_BOX_VBAN_RESERVE_MS_MAX=500

# _strih_box_vban_stream_name NAME -> 0 iff NAME is a VBAN stream name: 1..16 characters (the VBAN
# header's streamname field), letters, digits, `.` `_` `-`. The sets are spelled out (a bracket RANGE
# follows the locale in bash).
_strih_box_vban_stream_name() {
  local up='ABCDEFGHIJKLMNOPQRSTUVWXYZ' lo='abcdefghijklmnopqrstuvwxyz' dg='0123456789'
  [[ "${1-}" =~ ^[${up}${lo}${dg}._-]{1,16}$ ]]
}

# _strih_box_obs_input_name NAME -> 0 iff NAME is a plain OBS input name: 1..64 characters, letters,
# digits, space, `.` `_` `-` `(` `)`, no space first or last, and not the `none` sentinel. The sets are
# spelled out (a bracket RANGE follows the locale in bash).
_strih_box_obs_input_name() {
  local up='ABCDEFGHIJKLMNOPQRSTUVWXYZ' lo='abcdefghijklmnopqrstuvwxyz' dg='0123456789' v="${1-}"
  [ "$v" != none ] || return 1
  [[ "$v" =~ ^[${up}${lo}${dg}._()-]([${up}${lo}${dg}\ ._()-]{0,62}[${up}${lo}${dg}._()-])?$ ]]
}

# strih_box_validate NAME  (stdin: the parsed KEY=value lines) -> rc 0 iff the facts are complete and
# coherent for box NAME; else one `strih-box <NAME>: ...` stderr line per problem, rc 1. Order: a
# missing key, then every TODO_OWNER value (all named, then stop -- a template is not "malformed"),
# then the per-fact checks.
strih_box_validate() {
  local name="${1:?box name required}" line k v bad=0 todo=0
  local -A f=()
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    f["${line%%=*}"]="${line#*=}"
  done
  for k in $(strih_box_fact_keys); do
    if [ -z "${f[$k]+set}" ]; then
      echo "strih-box ${name}: missing fact ${k}" >&2; bad=1
    elif [ "${f[$k]}" = "$STRIH_BOX_TODO" ]; then
      echo "strih-box ${name}: ${k} is ${STRIH_BOX_TODO} -- the owner has not decided it yet" >&2; todo=1
    fi
  done
  [ "$bad" = 0 ] && [ "$todo" = 0 ] || return 1
  for k in $(strih_box_fact_keys); do
    case "$k" in STRIH_DANTESYNC_UPSTREAM) continue ;; esac
    if [ -z "${f[$k]}" ]; then
      echo "strih-box ${name}: ${k} is empty" >&2; bad=1
    fi
  done
  [ "$bad" = 0 ] || return 1
  v="${f[STRIH_HOSTNAME]}"
  [ "$v" = "$name" ] || { echo "strih-box ${name}: STRIH_HOSTNAME '${v}' must equal the box (file) name '${name}'" >&2; bad=1; }
  _strih_box_ipv4 "${f[STRIH_IP]}" || { echo "strih-box ${name}: STRIH_IP '${f[STRIH_IP]}' is not an IPv4 address" >&2; bad=1; }
  [ "${f[STRIH_NDI_PREFIX]}" = "${v^^}" ] \
    || { echo "strih-box ${name}: STRIH_NDI_PREFIX '${f[STRIH_NDI_PREFIX]}' must be the hostname upper-cased ('${v^^}') -- DistroAV prepends the hostname" >&2; bad=1; }
  case "${f[STRIH_DANTESYNC_ROLE]}" in
    server) [ -z "${f[STRIH_DANTESYNC_UPSTREAM]}" ] \
      || { echo "strih-box ${name}: STRIH_DANTESYNC_UPSTREAM must be empty for the server role (the NTP master syncs from nobody)" >&2; bad=1; } ;;
    client) _strih_box_host "${f[STRIH_DANTESYNC_UPSTREAM]}" \
      || { echo "strih-box ${name}: STRIH_DANTESYNC_UPSTREAM '${f[STRIH_DANTESYNC_UPSTREAM]}' must be the NTP master host name / address (required for the client role)" >&2; bad=1; } ;;
    *) echo "strih-box ${name}: STRIH_DANTESYNC_ROLE '${f[STRIH_DANTESYNC_ROLE]}' must be server or client" >&2; bad=1 ;;
  esac
  # Shape only (a repo-relative .toml under intercom/): the deploy plan stages scripts/ + systemd/ on
  # the box WITHOUT intercom/, so its existence is checked where it is installed (setup-strih step 13),
  # never here -- loading the facts must not refuse a box whose run never reaches that step.
  v="${f[STRIH_INTERCOM_CONFIG]}"
  if [[ ! "$v" =~ ^intercom/[A-Za-z0-9._-]+\.toml$ ]]; then
    echo "strih-box ${name}: STRIH_INTERCOM_CONFIG '${v}' must be a repo-relative intercom/<file>.toml" >&2; bad=1
  fi
  [[ "${f[STRIH_NIC_DRIVER]}" =~ ^[a-z0-9_]+$ ]] \
    || { echo "strih-box ${name}: STRIH_NIC_DRIVER '${f[STRIH_NIC_DRIVER]}' is not a kernel driver name" >&2; bad=1; }
  # issue 1391: the rig NIC's out-of-tree driver (`none`, or <package>-<version> vendored under vendor/ and
  # built through DKMS by setup-strih step 1b) + the lowest USB device / Ethernet link speed verify-strih
  # item 36 accepts (Mb/s; the USB one is `none` for a NIC that is not a USB device).
  [[ "${f[STRIH_NIC_OOT_DRIVER]}" =~ ^(none|[a-z0-9][a-z0-9_-]*-[0-9]+(\.[0-9]+)+)$ ]] \
    || { echo "strih-box ${name}: STRIH_NIC_OOT_DRIVER '${f[STRIH_NIC_OOT_DRIVER]}' must be none or <package>-<version> (a driver vendored under vendor/)" >&2; bad=1; }
  [[ "${f[STRIH_NIC_MIN_USB_MBPS]}" =~ ^(none|[1-9][0-9]*)$ ]] \
    || { echo "strih-box ${name}: STRIH_NIC_MIN_USB_MBPS '${f[STRIH_NIC_MIN_USB_MBPS]}' must be a speed in Mb/s (e.g. 10000 = USB 3.2 Gen 2) or none" >&2; bad=1; }
  [[ "${f[STRIH_NIC_MIN_LINK_MBPS]}" =~ ^[1-9][0-9]*$ ]] \
    || { echo "strih-box ${name}: STRIH_NIC_MIN_LINK_MBPS '${f[STRIH_NIC_MIN_LINK_MBPS]}' must be a speed in Mb/s (e.g. 5000)" >&2; bad=1; }
  for k in STRIH_OBS_PROFILE STRIH_OBS_COLLECTION; do
    [[ "${f[$k]}" =~ ^[A-Za-z0-9._\ -]+$ ]] \
      || { echo "strih-box ${name}: ${k} '${f[$k]}' may only carry letters, digits, space, . _ -" >&2; bad=1; }
  done
  for k in STRIH_NDI_RUNTIME_PEER STRIH_COMPANION_HOST; do
    _strih_box_host "${f[$k]}" \
      || { echo "strih-box ${name}: ${k} '${f[$k]}' is not a host name / address" >&2; bad=1; }
  done
  # issue 1346: how the fixed HDMI output leaves the X desktop -- `lease` (the issue-1152 X RandR lease,
  # an Intel-driven connector) or `vk-direct` (Vulkan direct display, an NVIDIA-driven connector whose X
  # driver refuses the lease). The same grammar the vendored C module + strih_scenes read.
  case "${f[STRIH_HDMI_OUTPUT_BACKEND]}:${f[STRIH_ROLE]}" in
    lease:* | vk-direct:* | none:stream) ;;
    *) echo "strih-box ${name}: STRIH_HDMI_OUTPUT_BACKEND '${f[STRIH_HDMI_OUTPUT_BACKEND]}' must be lease or vk-direct (none only for the stream role)" >&2; bad=1 ;;
  esac
  # issue 1424 part 2 (owner 13.10.2026): which connectors the fixed HDMI output drives and their default
  # views -- `auto` (ONE output on the first connected HDMI, the issue-1346 behaviour), `none` (exactly
  # when the backend is none), or 1..2 space-separated CONNECTOR:VIEW (an X RandR output name; program |
  # multiview), no connector twice. setup-strih step 6 writes the list into drm-output.json's "outputs".
  v="${f[STRIH_HDMI_OUTPUTS]}"
  case "${f[STRIH_HDMI_OUTPUT_BACKEND]}:${v}" in
    none:none) ;;
    none:* | *:none) echo "strih-box ${name}: STRIH_HDMI_OUTPUTS '${v}' must be none exactly when STRIH_HDMI_OUTPUT_BACKEND is none" >&2; bad=1 ;;
    *:auto) ;;
    *) strih_box_hdmi_outputs_ok "$v" \
      || { echo "strih-box ${name}: STRIH_HDMI_OUTPUTS '${v}' must be auto, none or 1..2 space-separated CONNECTOR:program|multiview[:noaudio] (no connector twice)" >&2; bad=1; } ;;
  esac
  # issue 1412: the box ROLE (strih = the cutter, stream = the stream OBS box), the PTP grandmaster the
  # dantesync config allows (setup-strih step 2 writes it into a FRESH box's /etc/dantesync/config.json)
  # and whether the box runs the YouTube-guard program-audio sampler (setup-strih step 16e / verify 41).
  case "${f[STRIH_ROLE]}" in
    strih | stream) ;;
    *) echo "strih-box ${name}: STRIH_ROLE '${f[STRIH_ROLE]}' must be strih or stream" >&2; bad=1 ;;
  esac
  _strih_box_host "${f[STRIH_DANTE_GM]}" \
    || { echo "strih-box ${name}: STRIH_DANTE_GM '${f[STRIH_DANTE_GM]}' is not a host name / address (the PTP grandmaster)" >&2; bad=1; }
  case "${f[STRIH_PROGRAM_AUDIO_SAMPLER]}" in
    on | off) ;;
    *) echo "strih-box ${name}: STRIH_PROGRAM_AUDIO_SAMPLER '${f[STRIH_PROGRAM_AUDIO_SAMPLER]}' must be on or off" >&2; bad=1 ;;
  esac
  # issue 1439: the program audio a box takes as VBAN straight from the console into the obs-vban source
  # (no intercom hub forwarding, no PipeWire): the stream name, the UDP port OBS binds, the fixed Receive
  # Buffer R (ms) and (issue 1462) the name of that OBS input (stream-pp `Zvuk z mixu`, stream-snv `mbc`,
  # strih-snv `ASIO zvuk`). The stream role takes exactly ONE stream (its hub is gone). The strih role
  # (phase 2, addendum 6338457001) declares all four -- one stream on its own port, the hub keeps 6980 --
  # or all four `none`: a hub-fed strih (none deployed since strih-pp took lv1's `strih-obs`, 24.10.2026).
  local vs="${f[STRIH_PROGRAM_VBAN_STREAMS]}" vp="${f[STRIH_PROGRAM_VBAN_PORT]}" vr="${f[STRIH_PROGRAM_VBAN_RESERVE_MS]}"
  local ai="${f[STRIH_PROGRAM_AUDIO_INPUT]}"
  if [ "${f[STRIH_ROLE]}" = strih ] && [ "${vs}:${vp}:${vr}:${ai}" = none:none:none:none ]; then
    :   # a hub-fed strih: its program is the hub's program_out (its STRIH_INTERCOM_CONFIG)
  else
    if [ "${f[STRIH_ROLE]}" = strih ] && { [ "$vs" = none ] || [ "$vp" = none ] || [ "$vr" = none ] || [ "$ai" = none ]; }; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_STREAMS / _PORT / _RESERVE_MS / STRIH_PROGRAM_AUDIO_INPUT are all none (a hub-fed strih) or all set on the strih role (issue 1439)" >&2; bad=1
    fi
    _strih_box_obs_input_name "$ai" \
      || { echo "strih-box ${name}: STRIH_PROGRAM_AUDIO_INPUT '${ai}' must be the OBS program-audio input's name (1..64 letters, digits, space, . _ - ( ), no space first or last)" >&2; bad=1; }
    [ "$vs" != none ] && _strih_box_vban_stream_name "$vs" \
      || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_STREAMS '${vs}' must be ONE VBAN stream name (1..16 letters, digits, . _ -) (the console's program stream)" >&2; bad=1; }
    if [[ ! "$vp" =~ ^[123456789][0123456789]{3,4}$ ]] || [ "$vp" -lt 1024 ] || [ "$vp" -gt 65535 ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_PORT '${vp}' must be the UDP port OBS receives on (1024..65535)" >&2; bad=1
    fi
    if [[ ! "$vr" =~ ^[123456789][0123456789]{0,2}$ ]] || [ "$vr" -lt "$STRIH_BOX_VBAN_RESERVE_MS_MIN" ] || [ "$vr" -gt "$STRIH_BOX_VBAN_RESERVE_MS_MAX" ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_RESERVE_MS '${vr}' must be the fixed Receive Buffer in ms (${STRIH_BOX_VBAN_RESERVE_MS_MIN}..${STRIH_BOX_VBAN_RESERVE_MS_MAX})" >&2; bad=1
    fi
  fi
  # issue 1439 (ROZHODNUTÉ 6360417876, owner 23.10.2026): the VBAN-only leg on the console's audio VLAN
  # (scripts/lib/strih-vban-leg.sh). STRIH_PROGRAM_VBAN_LEG = `mac:<MAC>` (a second NIC, its MAC lower-case
  # as sysfs prints it; stream-snv) | `vlan:<id>` (a tagged VLAN 1..4094 on the rig NIC; strih-snv, the
  # supervisor's 23.10.2026 10:31 scope) | none. _LEG_ADDR = the box's /32 on it. STRIH_PROGRAM_VBAN_SENDER =
  # the console that sends (none = the leg passes only ping; a sender needs STRIH_PROGRAM_VBAN_PORT), in
  # the address's /24 (the policy route for the ping replies covers that /24); neither is the rig address.
  # Every role (the strih role's own VBAN program is phase 2).
  local lg="${f[STRIH_PROGRAM_VBAN_LEG]}" la="${f[STRIH_PROGRAM_VBAN_LEG_ADDR]}" ls="${f[STRIH_PROGRAM_VBAN_SENDER]}"
  local hx='[0123456789abcdef]' dg='[0123456789]' nz='[123456789]'
  if [ "$lg" = none ]; then
    [ "${la}:${ls}" = none:none ] \
      || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG_ADDR '${la}' / STRIH_PROGRAM_VBAN_SENDER '${ls}' must be none while STRIH_PROGRAM_VBAN_LEG is none" >&2; bad=1; }
  else
    case "$lg" in
      mac:*) [[ "${lg#mac:}" =~ ^${hx}${hx}(:${hx}${hx}){5}$ ]] \
        || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG '${lg}': the MAC must be six lower-case hex pairs joined by : (as /sys/class/net/<if>/address prints it)" >&2; bad=1; } ;;
      vlan:*) if [[ ! "${lg#vlan:}" =~ ^${nz}${dg}{0,3}$ ]] || [ "${lg#vlan:}" -gt 4094 ]; then
          echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG '${lg}': the VLAN id must be 1..4094 (no leading zero)" >&2; bad=1
        fi ;;
      *) echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG '${lg}' must be mac:<MAC> (a second NIC), vlan:<id> (a tagged VLAN on the rig NIC) or none" >&2; bad=1 ;;
    esac
    if ! _strih_box_ipv4 "$la"; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG_ADDR '${la}' must be the box's IPv4 address on the leg" >&2; bad=1
    elif [ "$la" = "${f[STRIH_IP]}" ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG_ADDR '${la}' must not be the rig address STRIH_IP" >&2; bad=1
    fi
    if [ "$ls" != none ]; then
      # `IP` sends to STRIH_PROGRAM_VBAN_PORT; `IP:P1,P2` names its own UDP ports (1024..65535, comma-joined)
      local lsp="" p
      if [ "${ls#*:}" != "$ls" ]; then
        lsp="${ls#*:}"; ls="${ls%%:*}"
        for p in ${lsp//,/ }; do
          if [[ ! "$p" =~ ^${nz}${dg}{3,4}$ ]] || [ "$p" -gt 65535 ]; then
            echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER port '${p}' must be a UDP port 1024..65535 (IP:P1,P2)" >&2; bad=1
          fi
        done
        [[ "$lsp" =~ ^[0123456789]+(,[0123456789]+)*$ ]] \
          || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER ports '${lsp}' must be comma-joined numbers (IP:P1,P2)" >&2; bad=1; }
      fi
      if ! _strih_box_ipv4 "$ls"; then
        echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER '${ls}' must be the console's IPv4 address, or none (the leg passes only ping)" >&2; bad=1
      elif _strih_box_ipv4 "$la"; then
        [ "${la%.*}" = "${ls%.*}" ] \
          || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER '${ls}' must be in the leg's /24 (${la%.*}.0/24, STRIH_PROGRAM_VBAN_LEG_ADDR)" >&2; bad=1; }
        [ "$la" != "$ls" ] \
          || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_LEG_ADDR and STRIH_PROGRAM_VBAN_SENDER must differ (both '${la}')" >&2; bad=1; }
      fi
      [ -n "$lsp" ] || [ "${f[STRIH_PROGRAM_VBAN_PORT]}" != none ] \
        || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER '${ls}' needs the UDP port it sends to (STRIH_PROGRAM_VBAN_PORT is none; or name it, IP:PORT)" >&2; bad=1; }
    fi
  fi
  # issue 1412 slice B: the NDI program a stream box receives (`none` on a strih), and the venue whose
  # boxes list each other as NDI senders (snv = the obs-fleet ndi-sender facet; any other venue = the
  # STRIH_IP of every fact file of that venue, scripts/lib/obs-box-role.sh).
  case "${f[STRIH_ROLE]}:${f[STRIH_PROGRAM_SENDER]}" in
    strih:none) ;;
    stream:none | strih:*) echo "strih-box ${name}: STRIH_PROGRAM_SENDER '${f[STRIH_PROGRAM_SENDER]}' must be none for the strih role and an NDI source name for the stream role" >&2; bad=1 ;;
    stream:*) [[ "${f[STRIH_PROGRAM_SENDER]}" =~ ^[A-Z0-9][A-Z0-9.-]*\ \([A-Za-z0-9\ ._-]+\)$ ]] \
      || { echo "strih-box ${name}: STRIH_PROGRAM_SENDER '${f[STRIH_PROGRAM_SENDER]}' is not an NDI source name (HOST (name))" >&2; bad=1; } ;;
  esac
  [[ "${f[STRIH_VENUE]}" =~ ^[a-z][a-z0-9]*$ ]] \
    || { echo "strih-box ${name}: STRIH_VENUE '${f[STRIH_VENUE]}' must be a lower-case venue name (snv, pp)" >&2; bad=1; }
  if [ "${f[STRIH_ROLE]}" = stream ] && [ "${f[STRIH_CAMERAS]}" = none ]; then
    :  # a stream box receives no camera
  elif [[ ! "${f[STRIH_CAMERAS]}" =~ ^[1-9][0-9]*( [1-9][0-9]*)*$ ]]; then
    echo "strih-box ${name}: STRIH_CAMERAS '${f[STRIH_CAMERAS]}' must be space-separated camera numbers (1, 2, ... no leading zero)" >&2; bad=1
  elif [ "$(tr ' ' '\n' <<<"${f[STRIH_CAMERAS]}" | sort -u | wc -l)" -ne "$(wc -w <<<"${f[STRIH_CAMERAS]}")" ]; then
    echo "strih-box ${name}: STRIH_CAMERAS '${f[STRIH_CAMERAS]}' lists a camera twice" >&2; bad=1
  fi
  # issue 1413: how dev1 dials this venue's camboxes (the dantesync fleet's camera rows,
  # scripts/lib/dantesync-fleet.sh): `direct` = camera_resolve's own address (dev1 sits on that LAN),
  # `A.B.C` = the /24 dev1 reaches the venue's camera LAN through (the camera keeps camera_resolve's
  # last octet), `none` = the box lists no camera. The octets are spelled out (a bracket RANGE follows
  # the locale in bash, ci-testing-gotchas.md).
  case "${f[STRIH_CAMERAS]}:${f[STRIH_CAMERA_DEV1_NET]}" in
    none:none) ;;
    none:* | *:none) echo "strih-box ${name}: STRIH_CAMERA_DEV1_NET '${f[STRIH_CAMERA_DEV1_NET]}' must be none exactly when STRIH_CAMERAS is none" >&2; bad=1 ;;
    *:direct) ;;
    *) _strih_box_ipv4 "${f[STRIH_CAMERA_DEV1_NET]}.0" && [[ "${f[STRIH_CAMERA_DEV1_NET]}" =~ ^[0123456789]+\.[0123456789]+\.[0123456789]+$ ]] \
      || { echo "strih-box ${name}: STRIH_CAMERA_DEV1_NET '${f[STRIH_CAMERA_DEV1_NET]}' must be direct, none or the dev1-side /24 prefix A.B.C" >&2; bad=1; } ;;
  esac
  # issue 1399: the TLS front end the box serves itself (setup-strih step 16i, verify-strih item 45):
  # STRIH_HTTPS_INTERKOM_HOSTS = the cert's first name + its SANs, STRIH_HTTPS_SHADING_HOST = one name;
  # `none` = no such site. The stream role serves no intercom / shading page, and a name is one site.
  local n
  local -a names
  local -A https_seen=()
  for k in STRIH_HTTPS_INTERKOM_HOSTS STRIH_HTTPS_SHADING_HOST; do
    v="${f[$k]}"
    [ "$v" = none ] && continue
    if [ "${f[STRIH_ROLE]}" = stream ]; then
      echo "strih-box ${name}: ${k} '${v}' must be none for the stream role (it serves no intercom / shading page)" >&2; bad=1; continue
    fi
    IFS=' ' read -r -a names <<<"$v"
    if [ "$k" = STRIH_HTTPS_SHADING_HOST ] && [ "${#names[@]}" != 1 ]; then
      echo "strih-box ${name}: ${k} '${v}' must be ONE DNS name or none" >&2; bad=1; continue
    fi
    for n in "${names[@]}"; do
      if ! _strih_box_https_name "$n"; then
        echo "strih-box ${name}: ${k} '${v}': '${n}' is not a lower-case DNS name (none, or space-separated names like interkom.newlevel.media)" >&2; bad=1
      elif [ -n "${https_seen[$n]+set}" ]; then
        echo "strih-box ${name}: ${k}: '${n}' is declared twice across STRIH_HTTPS_INTERKOM_HOSTS / STRIH_HTTPS_SHADING_HOST (one name, one site)" >&2; bad=1
      else
        https_seen[$n]=1
      fi
    done
  done
  # issue 1453: whether the intercom page is published off the LAN (the outbound Cloudflare Tunnel +
  # TURN, scripts/lib/strih-interkom-public.sh). `yes` needs an intercom site to publish, so never on
  # the stream role.
  case "${f[STRIH_HTTPS_PUBLIC]}" in
    no) ;;
    yes)
      if [ "${f[STRIH_HTTPS_INTERKOM_HOSTS]}" = none ]; then
        echo "strih-box ${name}: STRIH_HTTPS_PUBLIC=yes needs an intercom site (STRIH_HTTPS_INTERKOM_HOSTS is none)" >&2; bad=1
      fi ;;
    *) echo "strih-box ${name}: STRIH_HTTPS_PUBLIC '${f[STRIH_HTTPS_PUBLIC]}' must be yes or no" >&2; bad=1 ;;
  esac
  # issue 1462 (design 6379124377): the stream box's Development design. nested-pro = the issue-1380 SNV
  # design (the nested PRO carries the measurement audio the E2E reads), clip = the stream-pp YouTube-safe
  # measurement clip; one box never runs both (two QPSK markers in one program).
  case "${f[STRIH_STREAM_DEV_SCENE]}:${f[STRIH_ROLE]}" in
    nested-pro:stream|clip:stream|none:strih) ;;
    none:stream) echo "strih-box ${name}: STRIH_STREAM_DEV_SCENE=none on the stream role -- it needs nested-pro or clip" >&2; bad=1 ;;
    nested-pro:strih|clip:strih) echo "strih-box ${name}: STRIH_STREAM_DEV_SCENE '${f[STRIH_STREAM_DEV_SCENE]}' on the strih role -- it has no stream Development (none)" >&2; bad=1 ;;
    *) echo "strih-box ${name}: STRIH_STREAM_DEV_SCENE '${f[STRIH_STREAM_DEV_SCENE]}' must be one of nested-pro, clip, none" >&2; bad=1 ;;
  esac
  # issue 808 (owner ruling 5977504457): the shading panel's per-cambox relay overrides. The stream role
  # runs no shading service (setup step 16c skips it), so it declares none.
  if [ "${f[STRIH_ROLE]}" = stream ] && [ "${f[STRIH_BKSHADING_RELAYS]}" != none ]; then
    echo "strih-box ${name}: STRIH_BKSHADING_RELAYS '${f[STRIH_BKSHADING_RELAYS]}' must be none on the stream role (it runs no shading service)" >&2; bad=1
  else
    strih_box_bkshading_relays_ok "${f[STRIH_BKSHADING_RELAYS]}" "strih-box ${name}: " "${f[STRIH_CAMERAS]}" || bad=1
  fi
  # issue 1425 (design 6443440513): the box's FIXED OBS audio buffering, ms (0..960, OBS's own maximum):
  # setup-strih step 5 writes it to ~/.camera-box/genlock-audio-buffering-ms, libobs holds the mix at it.
  # The digit set is spelled out (a bracket range follows the locale).
  if [[ ! "${f[STRIH_AUDIO_BUFFERING_MS]}" =~ ^[0123456789]{1,3}$ ]] || [ "$((10#${f[STRIH_AUDIO_BUFFERING_MS]}))" -gt 960 ]; then
    echo "strih-box ${name}: STRIH_AUDIO_BUFFERING_MS '${f[STRIH_AUDIO_BUFFERING_MS]}' must be the box's fixed OBS audio buffering in ms (an integer 0..960)" >&2; bad=1
  fi
  strih_box_jxs_facts_ok "${f[STRIH_ROLE]}" "${f[STRIH_CAMERAS]}" "${f[STRIH_JXS_CAMERAS]}" "${f[STRIH_JXS_CAMERA_PIN_MS]}" \
    "${f[STRIH_JXS_PROGRAM_PORT]}" "${f[STRIH_JXS_PROGRAM_PIN_MS]}" "strih-box ${name}: " || bad=1
  [ "$bad" = 0 ]
}

# issue 1387 (owner 31.10.2026: the JPEG XS path camera -> strih -> stream, live): the link's facts. PP
# is deferred by the owner, so every PP file declares none.
#   STRIH_JXS_CAMERAS (strih role) = `N:PORT` per camera on the link: N one of STRIH_CAMERAS, PORT the
#     UDP port its `JXS camN src` input listens on (the camera's CAMERA_BOX_JXS_TARGET port); each N and
#     each port once. STRIH_JXS_CAMERA_PIN_MS = their genlock pin (ROZHODNUTE 6546700019: 36 ms, the
#     fixed 100 ms grid age).
#   STRIH_JXS_PROGRAM_PORT (stream role) = the UDP port `JXS PGM src` receives the venue strih's program
#     on; STRIH_JXS_PROGRAM_PIN_MS = its genlock pin (the stream A/V pin: a re-calibration is this line).
# A pin is the jxs-source clamp range (vendor/distroav/src/jxs-source-decide.h CB_JXS_LATENCY_MS_MIN /
# _MAX), a port 1024..65535; a value is none exactly when its partner is none. Digit sets spelled out.
STRIH_BOX_JXS_PIN_MS_MIN=3
STRIH_BOX_JXS_PIN_MS_MAX=2000

# _strih_box_jxs_port VALUE -> 0 iff VALUE is a UDP port 1024..65535 (no leading zero).
_strih_box_jxs_port() {
  [[ "${1-}" =~ ^[123456789][0123456789]{3,4}$ ]] && [ "$1" -le 65535 ]
}

# _strih_box_jxs_pin VALUE -> 0 iff VALUE is a whole ms pin inside the jxs-source clamp range.
_strih_box_jxs_pin() {
  [[ "${1-}" =~ ^[123456789][0123456789]{0,3}$ ]] && [ "$1" -ge "$STRIH_BOX_JXS_PIN_MS_MIN" ] \
    && [ "$1" -le "$STRIH_BOX_JXS_PIN_MS_MAX" ]
}

# strih_box_jxs_facts_ok ROLE CAMERAS JXS_CAMERAS CAM_PIN PROG_PORT PROG_PIN PREFIX -> 0 iff the four
# JPEG XS facts hold together (above); one `PREFIX...` stderr line per problem.
strih_box_jxs_facts_ok() {
  local role="${1-}" cams="${2-}" jc="${3-}" cpin="${4-}" pport="${5-}" ppin="${6-}" p="${7-}" e n port bad=0
  local seen_n=" " seen_p=" " pins="${STRIH_BOX_JXS_PIN_MS_MIN}..${STRIH_BOX_JXS_PIN_MS_MAX}"
  if [ "$jc" = none ]; then
    [ "$cpin" = none ] || { echo "${p}STRIH_JXS_CAMERA_PIN_MS '${cpin}' must be none while STRIH_JXS_CAMERAS is none" >&2; bad=1; }
  elif [ "$role" != strih ]; then
    echo "${p}STRIH_JXS_CAMERAS '${jc}' must be none on the ${role} role (the cameras arrive on the strih)" >&2; bad=1
  else
    for e in $jc; do
      n="${e%%:*}"; port="${e#*:}"
      # (the port check runs its own =~, so the match groups are never read after it)
      if [[ ! "$e" =~ ^[123456789][0123456789]*:[0123456789]+$ ]] || ! _strih_box_jxs_port "$port"; then
        echo "${p}STRIH_JXS_CAMERAS entry '${e}' is not N:PORT (a camera number : a UDP port 1024..65535)" >&2; bad=1; continue
      fi
      [[ " ${cams} " == *" ${n} "* ]] || { echo "${p}STRIH_JXS_CAMERAS entry '${e}': camera ${n} is not one of this box's STRIH_CAMERAS (${cams})" >&2; bad=1; }
      [[ "$seen_n" == *" ${n} "* ]] && { echo "${p}STRIH_JXS_CAMERAS lists camera ${n} twice" >&2; bad=1; }
      [[ "$seen_p" == *" ${port} "* ]] && { echo "${p}STRIH_JXS_CAMERAS gives UDP port ${port} to two cameras (one port = one input)" >&2; bad=1; }
      seen_n="${seen_n}${n} "; seen_p="${seen_p}${port} "
    done
    _strih_box_jxs_pin "$cpin" || { echo "${p}STRIH_JXS_CAMERA_PIN_MS '${cpin}' must be the JPEG XS cameras' genlock pin in ms (${pins})" >&2; bad=1; }
  fi
  if [ "$pport" = none ]; then
    [ "$ppin" = none ] || { echo "${p}STRIH_JXS_PROGRAM_PIN_MS '${ppin}' must be none while STRIH_JXS_PROGRAM_PORT is none" >&2; bad=1; }
  elif [ "$role" != stream ]; then
    echo "${p}STRIH_JXS_PROGRAM_PORT '${pport}' must be none on the ${role} role (the program arrives on the stream box)" >&2; bad=1
  else
    _strih_box_jxs_port "$pport" || { echo "${p}STRIH_JXS_PROGRAM_PORT '${pport}' must be a UDP port 1024..65535" >&2; bad=1; }
    _strih_box_jxs_pin "$ppin" || { echo "${p}STRIH_JXS_PROGRAM_PIN_MS '${ppin}' must be the program input's genlock pin in ms (${pins})" >&2; bad=1; }
  fi
  [ "$bad" = 0 ]
}

# strih_box_jxs_port_of JXS_CAMERAS N -> the UDP port STRIH_JXS_CAMERAS gives camera N, or `none` when
# it lists no such camera (or is none). Pure; always rc 0 (a validated fact never holds a bad token).
strih_box_jxs_port_of() {
  local e
  for e in ${1-}; do
    case "$e" in "${2-}":*) printf '%s\n' "${e#*:}"; return 0 ;; esac
  done
  printf 'none\n'
}

# strih_box_alias_of NAME -> print the box NAME is an issue-1414 transition alias of, rc 0; rc 1 (no
# output) when NAME is no alias. The SNV strih was named strih-lx until issue 1414 (box names carry the
# venue): a deploy or a hand run that still says the old name loads the renamed box with ONE warning.
# The ONE alias table is obs-fleet.sh's OBS_FLEET_ALIASES (sourced lazily from this lib's own dir);
# an alias counts here only when its renamed box has a fact file.
strih_box_alias_of() {
  local want="${1-}" canon
  if ! declare -F _obs_fleet_alias_of >/dev/null; then
    # shellcheck source=scripts/lib/obs-fleet.sh
    . "$(dirname "${BASH_SOURCE[0]}")/obs-fleet.sh" || return 1
  fi
  canon="$(_obs_fleet_alias_of "$want")" || return 1
  [ -f "$(strih_box_dir)/${canon}.env" ] || return 1
  printf '%s' "$canon"
}

# strih_box_load NAME -> load box NAME's facts into the STRIH_BOX_FACTS map (+ STRIH_BOX_LOADED=NAME),
# rc 0; rc 1 with the reasons on stderr when the name, the file or a fact is invalid, when the fleet
# list has a row for NAME whose host is an IP other than STRIH_IP, or when a per-run env knob
# (STRIH_LX_IP / STRIH_LX_DANTESYNC_ROLE) contradicts the loaded fact. Nothing is loaded on failure.
strih_box_load() {
  local name="${1-}" file parsed line fleet_ip canon
  strih_box_unload
  STRIH_BOX_LOAD_FAILED="${name:-<empty>}"   # cleared only when this load succeeds (see strih_box_ensure_loaded)
  if [[ ! "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
    echo "strih-box: box name '${name}' is invalid (lower-case letters, digits, '-'; never a path)" >&2
    return 1
  fi
  if canon="$(strih_box_alias_of "$name")"; then
    echo "strih-box: '${name}' is the old name of box '${canon}' (issue 1414: box names carry the venue) -- loading ${canon}; use --box ${canon}" >&2
    name="$canon"
  fi
  file="$(strih_box_dir)/${name}.env"
  if [ ! -f "$file" ]; then
    echo "strih-box: no fact file for box '${name}' (${file}) -- known boxes: $(strih_box_known | tr '\n' ' ')" >&2
    return 1
  fi
  parsed="$(strih_box_parse_file "$file")" || return 1
  strih_box_validate "$name" <<<"$parsed" || return 1
  declare -gA STRIH_BOX_FACTS=()
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    STRIH_BOX_FACTS["${line%%=*}"]="${line#*=}"
  done <<<"$parsed"
  STRIH_BOX_LOADED="$name"
  # The fleet list (the dev1 dial list, scripts/lib/obs-fleet.sh) must agree with the fact IP when it
  # has a row for this box; a box with no row yet (a new strih before go-live) is fine.
  if ! declare -F obs_fleet_host >/dev/null; then
    # shellcheck source=scripts/lib/obs-fleet.sh
    . "$(dirname "${BASH_SOURCE[0]}")/obs-fleet.sh" || { strih_box_unload; return 1; }
  fi
  if fleet_ip="$(obs_fleet_host "$name")" && _strih_box_ipv4 "$fleet_ip" \
      && [ "$fleet_ip" != "${STRIH_BOX_FACTS[STRIH_IP]}" ]; then
    echo "strih-box ${name}: the fleet list row says ${fleet_ip} but STRIH_IP is ${STRIH_BOX_FACTS[STRIH_IP]} -- fix scripts/lib/obs-fleet.sh or the fact file" >&2
    strih_box_unload; return 1
  fi
  if [ -n "${STRIH_LX_IP:-}" ] && [ "$STRIH_LX_IP" != "${STRIH_BOX_FACTS[STRIH_IP]}" ]; then
    echo "strih-box ${name}: STRIH_LX_IP=${STRIH_LX_IP} contradicts STRIH_IP=${STRIH_BOX_FACTS[STRIH_IP]} -- the IP is a box fact now (edit ${file})" >&2
    strih_box_unload; return 1
  fi
  if [ -n "${STRIH_LX_DANTESYNC_ROLE:-}" ] && [ "$STRIH_LX_DANTESYNC_ROLE" != "${STRIH_BOX_FACTS[STRIH_DANTESYNC_ROLE]}" ]; then
    echo "strih-box ${name}: STRIH_LX_DANTESYNC_ROLE=${STRIH_LX_DANTESYNC_ROLE} contradicts STRIH_DANTESYNC_ROLE=${STRIH_BOX_FACTS[STRIH_DANTESYNC_ROLE]} -- the role is a box fact now (edit ${file})" >&2
    strih_box_unload; return 1
  fi
  STRIH_BOX_LOAD_FAILED=""
  return 0
}

# strih_box_unload -> forget the loaded box. The next accessor lazily loads the default again --
# unless a load FAILED in this shell (STRIH_BOX_LOAD_FAILED), which keeps every accessor refusing.
strih_box_unload() {
  declare -gA STRIH_BOX_FACTS=()
  STRIH_BOX_LOADED=""
}

# strih_box_known -> the box names that have a fact file, one per line.
strih_box_known() {
  local f
  for f in "$(strih_box_dir)"/*.env; do
    [ -f "$f" ] || continue
    f="${f##*/}"; printf '%s\n' "${f%.env}"
  done
}

# strih_box_ensure_loaded -> rc 0 once a box is loaded; loads STRIH_BOX_DEFAULT when none is yet (a
# sourced lib -- the unit tests -- gets today's box without an explicit load).
strih_box_ensure_loaded() {
  # A box whose load FAILED in this shell is never replaced by the default behind the caller's back.
  if [ -n "${STRIH_BOX_LOAD_FAILED:-}" ]; then
    echo "strih-box: the load of box '${STRIH_BOX_LOAD_FAILED}' failed -- no facts are served until a box loads" >&2
    return 1
  fi
  # both the name AND this shell's associative fact map: an inherited STRIH_BOX_LOADED (or a scalar
  # STRIH_BOX_FACTS from the environment) is not a load.
  [ -n "${STRIH_BOX_LOADED:-}" ] && [[ "$(declare -p STRIH_BOX_FACTS 2>/dev/null)" == "declare -A"* ]] && return 0
  strih_box_load "$STRIH_BOX_DEFAULT"
}

# strih_box_loaded_name -> the loaded box's name (loading the default first when none is).
strih_box_loaded_name() {
  strih_box_ensure_loaded || return 1
  printf '%s' "$STRIH_BOX_LOADED"
}

# strih_box_fact KEY -> print the loaded box's value for KEY (no newline); rc 1 for an unknown key or
# a failed load.
strih_box_fact() {
  local key="${1:?fact key required}"
  strih_box_ensure_loaded || return 1
  if [ -z "${STRIH_BOX_FACTS[$key]+set}" ]; then
    echo "strih-box: unknown fact ${key}" >&2
    return 1
  fi
  printf '%s' "${STRIH_BOX_FACTS[$key]}"
}

# strih_box_cli_box ARGS... -> print the box the orchestrator's command line selects: `--box NAME` or
# `--box=NAME` (once), default STRIH_BOX_DEFAULT. `--yes` (the documented non-interactive flag) is
# accepted; anything else refuses with rc 1 + a usage line (never a silently ignored argument).
strih_box_cli_box() {
  local box=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --box)
        [ -n "${2:-}" ] || { echo "strih-box: --box needs a box name" >&2; return 1; }
        [ -z "$box" ] || { echo "strih-box: --box given twice" >&2; return 1; }
        box="$2"; shift 2 ;;
      --box=*)
        [ -z "$box" ] || { echo "strih-box: --box given twice" >&2; return 1; }
        box="${1#--box=}"
        [ -n "$box" ] || { echo "strih-box: --box needs a box name" >&2; return 1; }
        shift ;;
      --yes) shift ;;
      *) echo "strih-box: unknown argument '$1' (usage: [--box <name>] [--yes])" >&2; return 1 ;;
    esac
  done
  printf '%s\n' "${box:-$STRIH_BOX_DEFAULT}"
}

# --- the fact ACCESSORS (the `strih_lx_*` names are historical -- issue 1317 wrote them for the first
# strih box; they serve whichever box is loaded). scripts/lib/strih-provision.sh sources this lib and
# calls them; each reads through strih_box_fact, so an unloaded shell loads the default box. ---------

# strih_lx_host -> the address the fleet dials for this strih box. STRIH_LX_HOST overrides; the
# default is the ONE fleet list's host for the box's name (issue 1317 part 2: `obs_fleet_host <name>`;
# the old `.lan` default had NO DNS entry on dev1). A box with no fleet row yet (a new strih before
# go-live) dials its fact IP. obs-fleet.sh is sourced lazily from this lib's own dir.
strih_lx_host() {
  local name
  if [ -n "${STRIH_LX_HOST:-}" ]; then
    printf '%s' "$STRIH_LX_HOST"
    return 0
  fi
  name="$(strih_lx_hostname)" || return 1
  if ! declare -F obs_fleet_host >/dev/null; then
    # shellcheck source=scripts/lib/obs-fleet.sh
    . "$(dirname "${BASH_SOURCE[0]}")/obs-fleet.sh" || return 1
  fi
  obs_fleet_host "$name" || strih_lx_ip
}

# strih_lx_hostname -> the box's OWN hostname = its fleet NAME = its fact-file name (the name mDNS
# announces as <name>.local). Never derived from strih_lx_host: that is a DIAL address (an IP by
# default since issue 1317 part 2), and cutting it at the first dot would rename the box to `10`.
strih_lx_hostname() { strih_box_fact STRIH_HOSTNAME; }

# strih_lx_ip -> the box's static rig-LAN IP (fact STRIH_IP; the loader refuses a STRIH_LX_IP env
# value that contradicts it).
strih_lx_ip() { strih_box_fact STRIH_IP; }

# strih_lx_ndi_prefix -> the NDI output name prefix (fact STRIH_NDI_PREFIX = the hostname upper-cased,
# because DistroAV prepends the hostname to every output it announces).
strih_lx_ndi_prefix() { strih_box_fact STRIH_NDI_PREFIX; }

# strih_lx_cameras -> the camera numbers the strih receives, one per line (fact STRIH_CAMERAS).
strih_lx_cameras() {
  local c
  c="$(strih_box_fact STRIH_CAMERAS)" || return 1
  [ "$c" != none ] || return 0  # issue 1412: a stream box receives no camera
  # shellcheck disable=SC2086  # word-split the validated space-separated number list on purpose
  printf '%s\n' $c
}

# strih_lx_cg_sender -> the venue CG sender name, or empty when the fact is `none` (no CG inputs).
strih_lx_cg_sender() {
  local c
  c="$(strih_box_fact STRIH_CG_SENDER)" || return 1
  [ "$c" = none ] || printf '%s' "$c"
}

# strih_box_role -> the box's role (fact STRIH_ROLE: strih | stream, issue 1412).
strih_box_role() { strih_box_fact STRIH_ROLE; }

# strih_lx_dante_gm -> the PTP grandmaster host the box's dantesync config allows (fact STRIH_DANTE_GM;
# RIG_GRANDMASTER_IP overrides it for one run, the same override every rig grandmaster read honours).
strih_lx_dante_gm() {
  if [ -n "${RIG_GRANDMASTER_IP:-}" ]; then
    printf '%s' "$RIG_GRANDMASTER_IP"
  else
    strih_box_fact STRIH_DANTE_GM
  fi
}

# strih_lx_program_sender -> the NDI program a stream box receives (fact STRIH_PROGRAM_SENDER), empty on
# a strih (`none`).
strih_lx_program_sender() {
  local s
  s="$(strih_box_fact STRIH_PROGRAM_SENDER)" || return 1
  [ "$s" = none ] || printf '%s' "$s"
}

# strih_lx_program_vban_streams / _port / _reserve_ms -> issue 1439: the console's program VBAN stream
# name(s) the box receives straight into its obs-vban source, the UDP port OBS binds and the fixed
# Receive Buffer R in ms (facts STRIH_PROGRAM_VBAN_*; all `none` on a hub-fed strih).
strih_lx_program_vban_streams() { strih_box_fact STRIH_PROGRAM_VBAN_STREAMS; }
strih_lx_program_vban_port() { strih_box_fact STRIH_PROGRAM_VBAN_PORT; }
strih_lx_program_vban_reserve_ms() { strih_box_fact STRIH_PROGRAM_VBAN_RESERVE_MS; }
# issue 1462: the stream role's OBS program-audio input name (`none` on the strih role).
strih_lx_program_audio_input() { strih_box_fact STRIH_PROGRAM_AUDIO_INPUT; }
# issue 1439 (ROZHODNUTÉ 6360417876): the VBAN-only leg -- `mac:<MAC>` | `vlan:<id>` | none, the box's /32
# on it, the console that sends (none = ping only; scripts/lib/strih-vban-leg.sh).
strih_lx_program_vban_leg() { strih_box_fact STRIH_PROGRAM_VBAN_LEG; }
strih_lx_program_vban_leg_addr() { strih_box_fact STRIH_PROGRAM_VBAN_LEG_ADDR; }
strih_lx_program_vban_sender() { strih_box_fact STRIH_PROGRAM_VBAN_SENDER; }

# strih_box_venue -> the box's venue (fact STRIH_VENUE: snv | pp ...).
strih_box_venue() { strih_box_fact STRIH_VENUE; }

# strih_lx_https_interkom_hosts / strih_lx_https_shading_host -> the names of the TLS front end the box
# serves itself (facts STRIH_HTTPS_INTERKOM_HOSTS = first name + SANs, STRIH_HTTPS_SHADING_HOST; `none`
# = no such site; issue 1399, scripts/lib/strih-https.sh).
strih_lx_https_interkom_hosts() { strih_box_fact STRIH_HTTPS_INTERKOM_HOSTS; }
strih_lx_https_shading_host() { strih_box_fact STRIH_HTTPS_SHADING_HOST; }
# strih_lx_https_public -> yes | no: whether the box publishes its intercom page to phones on mobile data
# (fact STRIH_HTTPS_PUBLIC; issue 1453, scripts/lib/strih-interkom-public.sh).
strih_lx_https_public() { strih_box_fact STRIH_HTTPS_PUBLIC; }

# strih_lx_stream_dev_scene -> the stream Development design: nested-pro | clip | none (fact
# STRIH_STREAM_DEV_SCENE, issue 1462).
strih_lx_stream_dev_scene() { strih_box_fact STRIH_STREAM_DEV_SCENE; }

# strih_box_fleet_cameras -> the cameras of every strih venue (issue 1431): the union of STRIH_CAMERAS over
# the fact files whose STRIH_ROLE is `strih` (parsed, never sourced), ascending, space-separated. rc 1 + one
# stderr line naming a fact file that does not parse (never a partial list). Runs in a subshell.
strih_box_fleet_cameras() {
  (
    set +f
    shopt -u nullglob failglob nocaseglob nocasematch 2>/dev/null || true
    dir="$(strih_box_dir)" all=""
    for f in "$dir"/*.env; do
      [ -f "$f" ] || continue
      parsed="$(strih_box_parse_file "$f" 2>/dev/null)" \
        || { echo "strih-box: ${f##*/} does not parse -- the venue camera list is unreadable (issue 1431)" >&2; exit 1; }
      role="" cams=""
      while IFS= read -r line; do
        case "$line" in
          STRIH_ROLE=*) role="${line#*=}" ;;
          STRIH_CAMERAS=*) cams="${line#*=}" ;;
        esac
      done <<<"$parsed"
      [ "$role" = strih ] && [ "$cams" != none ] || continue
      all="${all} ${cams}"
    done
    printf '%s\n' $all | sort -n -u | tr '\n' ' ' | sed 's/ $//'
  )
}

# strih_lx_bkshading_relays -> the shading panel's per-cambox relay overrides (fact STRIH_BKSHADING_RELAYS:
# none | `N=<transport>@<host>:<port>` ...; issue 808, the seed strih_bkshading_config_text).
strih_lx_bkshading_relays() { strih_box_fact STRIH_BKSHADING_RELAYS; }

# strih_lx_audio_buffering_ms -> the box's FIXED OBS audio buffering in ms (fact STRIH_AUDIO_BUFFERING_MS,
# issue 1425; setup-strih step 5 writes it for libobs, verify-strih item 48 + the E2E [0/8] row grade it).
strih_lx_audio_buffering_ms() { strih_box_fact STRIH_AUDIO_BUFFERING_MS; }

# strih_box_bkshading_relays_ok VALUE [PREFIX] -> 0 iff VALUE is `none` or space-separated
# `N=<transport>@<host>:<port>` entries: N one of 1..7 and of the box's STRIH_CAMERAS (owner ruling 5977504457 + owner 26.10.2026),
# no N twice, transport cambox-relay | sbc-relay | ethernet-rest (bkshading/service/src/config.rs; never
# Bluetooth), host a host name / address that starts with a letter or digit, port 1..65535. Each problem is
# one stderr line starting with PREFIX. The ONE grammar of the fact: the loader and the seed both call it.
# _strih_box_relay_id ID -> 0 when ID is a relay registration id: 1-64 of [A-Za-z0-9._-] (the set the
# bkshading service accepts, bkshading/service/src/registry.rs valid_relay_id). Spelled out, never a range.
_strih_box_relay_id() {
  local LC_ALL=C lower='abcdefghijklmnopqrstuvwxyz' upper='ABCDEFGHIJKLMNOPQRSTUVWXYZ' digit='0123456789'
  [ "${#1}" -ge 1 ] && [ "${#1}" -le 64 ] && [[ "$1" =~ ^[${lower}${upper}${digit}._-]+$ ]]
}

strih_box_bkshading_relays_ok() {
  local v="${1-}" p="${2-}" cams="${3-}" e n rest transport hp host port bad=0 seen=" "
  [ "$v" = none ] && return 0
  if [ -z "$v" ]; then
    echo "${p}STRIH_BKSHADING_RELAYS is empty -- none, or N=transport@host:port entries" >&2; return 1
  fi
  for e in $v; do
    if [[ ! "$e" =~ ^([1234567])=([abcdefghijklmnopqrstuvwxyz-]+)@(.+)$ ]]; then
      if [[ "$e" =~ ^([0123456789]+)= ]]; then
        echo "${p}STRIH_BKSHADING_RELAYS entry '${e}': the camera number must be one of 1..7" >&2
      else
        echo "${p}STRIH_BKSHADING_RELAYS entry '${e}' is not N=transport@host:port (or the whole value none)" >&2
      fi
      bad=1; continue
    fi
    n="${BASH_REMATCH[1]}"; transport="${BASH_REMATCH[2]}"; hp="${BASH_REMATCH[3]}"
    case "$transport" in
      cambox-relay|sbc-relay|ethernet-rest) ;;
      *) echo "${p}STRIH_BKSHADING_RELAYS entry '${e}': transport '${transport}' must be cambox-relay, sbc-relay or ethernet-rest" >&2; bad=1 ;;
    esac
    # issue 808 (owner 28.10.2026): a handheld SBC is named by the id it registers with
    # (`register:<id>`), never by its DHCP address; only an sbc-relay may be.
    if [[ "$hp" == register:* ]]; then
      if [ "$transport" != sbc-relay ] || ! _strih_box_relay_id "${hp#register:}"; then
        echo "${p}STRIH_BKSHADING_RELAYS entry '${e}': register:<id> is only for an sbc-relay, id 1-64 of [A-Za-z0-9._-]" >&2; bad=1
      fi
    else
      host="${hp%:*}"; port="${hp##*:}"; rest="$hp"
      if [ "$host" = "$rest" ] || ! _strih_box_host "$host" || [[ ! "$port" =~ ^[123456789][0123456789]{0,4}$ ]] \
         || [ "$port" -gt 65535 ]; then
        echo "${p}STRIH_BKSHADING_RELAYS entry '${e}': '${hp}' is not host:port (port 1..65535)" >&2; bad=1
      fi
    fi
    # owner 26.10.2026: a panel lists ITS venue's cameras only (STRIH_CAMERAS, the 3rd argument when given).
    if [ -n "$cams" ] && [[ " ${cams} " != *" ${n} "* ]]; then
      echo "${p}STRIH_BKSHADING_RELAYS entry '${e}': camera ${n} is not one of this box's STRIH_CAMERAS (${cams})" >&2; bad=1
    fi
    case "$seen" in *" ${n} "*)
      echo "${p}STRIH_BKSHADING_RELAYS: camera ${n} is declared twice" >&2; bad=1 ;;
    esac
    seen="${seen}${n} "
  done
  [ "$bad" = 0 ]
}

# strih_lx_program_audio_sampler -> on | off: whether setup-strih step 16e installs (and verify-strih
# item 41 grades) the YouTube-guard program-audio sampler (fact STRIH_PROGRAM_AUDIO_SAMPLER, issue 1412).
strih_lx_program_audio_sampler() { strih_box_fact STRIH_PROGRAM_AUDIO_SAMPLER; }

# strih_lx_hdmi_output_backend -> how the box's fixed HDMI output leaves the X desktop (fact
# STRIH_HDMI_OUTPUT_BACKEND: lease | vk-direct), written into ~/.camera-box/drm-output.json (issue 1346).
strih_lx_hdmi_output_backend() { strih_box_fact STRIH_HDMI_OUTPUT_BACKEND; }

# strih_lx_hdmi_outputs -> the connectors the fixed HDMI output drives + their default views (fact
# STRIH_HDMI_OUTPUTS: auto | none | CONNECTOR:VIEW ..., issue 1424 part 2).
strih_lx_hdmi_outputs() { strih_box_fact STRIH_HDMI_OUTPUTS; }

# strih_lx_intercom_config -> the repo-relative intercom hub routing file (fact STRIH_INTERCOM_CONFIG),
# installed as /etc/intercom-hub/intercom.toml.
strih_lx_intercom_config() { strih_box_fact STRIH_INTERCOM_CONFIG; }

# strih_lx_nic_driver -> the kernel driver of the ONE rig NDI NIC (fact STRIH_NIC_DRIVER, the NIC
# selection rule shared by the baseline tuning, the boot IRQ oneshot and verify-strih).
strih_lx_nic_driver() { strih_box_fact STRIH_NIC_DRIVER; }

# strih_lx_nic_oot_driver -> the rig NIC's out-of-tree driver `<package>-<version>` that setup-strih step 1b
# builds through DKMS from vendor/ (fact STRIH_NIC_OOT_DRIVER), or `none` (issue 1391).
strih_lx_nic_oot_driver() { strih_box_fact STRIH_NIC_OOT_DRIVER; }

# strih_lx_nic_min_usb_mbps / strih_lx_nic_min_link_mbps -> the lowest rig-NIC USB device speed (`none` for
# a non-USB NIC) / Ethernet link speed in Mb/s verify-strih item 36 accepts (facts STRIH_NIC_MIN_USB_MBPS /
# STRIH_NIC_MIN_LINK_MBPS, issue 1391).
strih_lx_nic_min_usb_mbps() { strih_box_fact STRIH_NIC_MIN_USB_MBPS; }
strih_lx_nic_min_link_mbps() { strih_box_fact STRIH_NIC_MIN_LINK_MBPS; }

# strih_lx_ndi_runtime_peer -> the cam box the NDI runtime is copied from (fact STRIH_NDI_RUNTIME_PEER).
strih_lx_ndi_runtime_peer() { strih_box_fact STRIH_NDI_RUNTIME_PEER; }

# strih_lx_obs_profile / strih_lx_obs_collection -> the OBS profile + scene-collection names the
# launcher starts OBS with (facts STRIH_OBS_PROFILE / STRIH_OBS_COLLECTION).
strih_lx_obs_profile() { strih_box_fact STRIH_OBS_PROFILE; }
strih_lx_obs_collection() { strih_box_fact STRIH_OBS_COLLECTION; }

# strih_obs_box_facts_dropin_text -> the strih-obs.service --user drop-in that hands the box's OBS
# profile/collection facts to strih-obs-start.sh (it reads STRIH_OBS_PROFILE / STRIH_OBS_COLLECTION
# from its environment), so the launcher itself installs verbatim on every strih box.
strih_obs_box_facts_dropin_text() {
  local prof coll
  prof="$(strih_lx_obs_profile)" || return 1
  coll="$(strih_lx_obs_collection)" || return 1
  printf '[Service]\nEnvironment="STRIH_OBS_PROFILE=%s"\nEnvironment="STRIH_OBS_COLLECTION=%s"\n' "$prof" "$coll"
}

# strih_lx_dantesync_client_args -> the dantesync CLIENT invocation args `--ntp-server <upstream>`,
# upstream = STRIH_LX_NTP_SERVER (an explicit override) else the box fact STRIH_DANTESYNC_UPSTREAM.
# NEVER enables server/master mode. A server-role box has no upstream -> rc 1 + a stderr reason (no
# guessed default host, issue 1361).
strih_lx_dantesync_client_args() {
  local up="${STRIH_LX_NTP_SERVER:-}"
  if [ -n "$up" ] && ! _strih_box_host "$up"; then
    echo "strih_lx_dantesync_client_args: STRIH_LX_NTP_SERVER '${up}' is not a host name / address" >&2
    return 1
  fi
  [ -n "$up" ] || up="$(strih_box_fact STRIH_DANTESYNC_UPSTREAM)" || return 1
  if [ -z "$up" ]; then
    echo "strih_lx_dantesync_client_args: box '$(strih_lx_hostname)' has no STRIH_DANTESYNC_UPSTREAM (it is the NTP master) -- no client args" >&2
    return 1
  fi
  printf -- '--ntp-server %s' "$up"
}

# strih_lx_dantesync_role -> the box's dantesync role (fact STRIH_DANTESYNC_ROLE: server | client).
strih_lx_dantesync_role() { strih_box_fact STRIH_DANTESYNC_ROLE; }

# strih_lx_dantesync_args -> the args the role implies: none for `server` (the bare NTP-master
# daemon), strih_lx_dantesync_client_args for `client`.
strih_lx_dantesync_args() {
  local role
  role="$(strih_lx_dantesync_role)" || return 1
  if [ "$role" = client ]; then
    strih_lx_dantesync_client_args
  fi
}
