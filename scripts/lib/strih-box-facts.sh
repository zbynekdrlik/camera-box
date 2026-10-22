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
    STRIH_PROGRAM_SENDER STRIH_VENUE STRIH_CAMERA_DEV1_NET \
    STRIH_HTTPS_INTERKOM_HOSTS STRIH_HTTPS_SHADING_HOST \
    STRIH_HDMI_OUTPUTS STRIH_HTTPS_PUBLIC
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
  # (no intercom hub, no PipeWire): the stream name(s), the UDP port OBS binds and the fixed Receive
  # Buffer R (ms). The stream role takes exactly ONE stream (its `Zvuk z mixu` input, the hub is gone);
  # the strih role keeps its hub-fed program until its own slice, so it declares none of the three.
  local vs="${f[STRIH_PROGRAM_VBAN_STREAMS]}" vp="${f[STRIH_PROGRAM_VBAN_PORT]}" vr="${f[STRIH_PROGRAM_VBAN_RESERVE_MS]}"
  if [ "${f[STRIH_ROLE]}" = strih ]; then
    if [ "${vs}:${vp}:${vr}" != none:none:none ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_STREAMS / _PORT / _RESERVE_MS must be none on the strih role (its VBAN program input is not provisioned yet, issue 1439)" >&2; bad=1
    fi
  else
    [ "$vs" != none ] && _strih_box_vban_stream_name "$vs" \
      || { echo "strih-box ${name}: STRIH_PROGRAM_VBAN_STREAMS '${vs}' must be ONE VBAN stream name (1..16 letters, digits, . _ -) on the stream role (the console's program stream)" >&2; bad=1; }
    if [[ ! "$vp" =~ ^[123456789][0123456789]{3,4}$ ]] || [ "$vp" -lt 1024 ] || [ "$vp" -gt 65535 ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_PORT '${vp}' must be the UDP port OBS receives on (1024..65535)" >&2; bad=1
    fi
    if [[ ! "$vr" =~ ^[123456789][0123456789]{0,2}$ ]] || [ "$vr" -lt "$STRIH_BOX_VBAN_RESERVE_MS_MIN" ] || [ "$vr" -gt "$STRIH_BOX_VBAN_RESERVE_MS_MAX" ]; then
      echo "strih-box ${name}: STRIH_PROGRAM_VBAN_RESERVE_MS '${vr}' must be the fixed Receive Buffer in ms (${STRIH_BOX_VBAN_RESERVE_MS_MIN}..${STRIH_BOX_VBAN_RESERVE_MS_MAX})" >&2; bad=1
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
  [ "$bad" = 0 ]
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
# Receive Buffer R in ms (facts STRIH_PROGRAM_VBAN_*; all `none` on the strih role).
strih_lx_program_vban_streams() { strih_box_fact STRIH_PROGRAM_VBAN_STREAMS; }
strih_lx_program_vban_port() { strih_box_fact STRIH_PROGRAM_VBAN_PORT; }
strih_lx_program_vban_reserve_ms() { strih_box_fact STRIH_PROGRAM_VBAN_RESERVE_MS; }

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
