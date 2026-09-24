#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih box-fact loader, no top-level statements
# besides constants) -- matches the sibling scripts/lib/*.sh convention (strih-provision.sh,
# obs-fleet.sh) of NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict
# mode here would leak into it. Each caller (setup-strih.sh / verify-strih.sh) sets its own.
#
# scripts/lib/strih-box-facts.sh -- issue 1361: the ONE loader for the per-box strih FACT files
# (`scripts/strih-boxes/<box>.env`). setup-strih.sh / verify-strih.sh select a box with `--box <name>`
# (default: STRIH_BOX_DEFAULT) and every box/venue identity value (hostname, IP, NDI prefix, dantesync
# role + upstream, intercom config, NIC rule, OBS profile/collection, NDI-runtime peer, Companion
# controller, CG sender, cameras) is read from the loaded file -- one script for every strih box,
# never a per-box copy (the unified-design ruling, umbrella issue 1357).
#
# The fact file is PARSED, never sourced/executed: each non-comment line must be `KEY=value`, the value
# is taken literally and may not carry a shell metacharacter. Loading REFUSES (rc 1, one stderr line per
# problem) on: an unknown / duplicate / missing key, an unsafe value, any `TODO_OWNER` value (the
# not-yet-decided marker of a template such as strih-pp.env -- each one named), and a malformed fact
# (see strih_box_validate). Run-time facts (PL1 watts, the CPU plan, the NIC interface) are NOT facts
# here: setup-strih.sh derives them on the box.

# The box a bare setup-strih.sh / verify-strih.sh (no --box) provisions -- today's production strih.
STRIH_BOX_DEFAULT=strih-lx

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
    STRIH_OBS_PROFILE STRIH_OBS_COLLECTION \
    STRIH_NDI_RUNTIME_PEER STRIH_COMPANION_HOST STRIH_CG_SENDER STRIH_CAMERAS
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
  for k in STRIH_OBS_PROFILE STRIH_OBS_COLLECTION; do
    [[ "${f[$k]}" =~ ^[A-Za-z0-9._\ -]+$ ]] \
      || { echo "strih-box ${name}: ${k} '${f[$k]}' may only carry letters, digits, space, . _ -" >&2; bad=1; }
  done
  for k in STRIH_NDI_RUNTIME_PEER STRIH_COMPANION_HOST; do
    _strih_box_host "${f[$k]}" \
      || { echo "strih-box ${name}: ${k} '${f[$k]}' is not a host name / address" >&2; bad=1; }
  done
  if [[ ! "${f[STRIH_CAMERAS]}" =~ ^[1-9][0-9]*( [1-9][0-9]*)*$ ]]; then
    echo "strih-box ${name}: STRIH_CAMERAS '${f[STRIH_CAMERAS]}' must be space-separated camera numbers (1, 2, ... no leading zero)" >&2; bad=1
  elif [ "$(tr ' ' '\n' <<<"${f[STRIH_CAMERAS]}" | sort -u | wc -l)" -ne "$(wc -w <<<"${f[STRIH_CAMERAS]}")" ]; then
    echo "strih-box ${name}: STRIH_CAMERAS '${f[STRIH_CAMERAS]}' lists a camera twice" >&2; bad=1
  fi
  [ "$bad" = 0 ]
}

# strih_box_load NAME -> load box NAME's facts into the STRIH_BOX_FACTS map (+ STRIH_BOX_LOADED=NAME),
# rc 0; rc 1 with the reasons on stderr when the name, the file or a fact is invalid, when the fleet
# list has a row for NAME whose host is an IP other than STRIH_IP, or when a per-run env knob
# (STRIH_LX_IP / STRIH_LX_DANTESYNC_ROLE) contradicts the loaded fact. Nothing is loaded on failure.
strih_box_load() {
  local name="${1-}" file parsed line fleet_ip
  strih_box_unload
  STRIH_BOX_LOAD_FAILED="$name"   # cleared only when this load succeeds (see strih_box_ensure_loaded)
  if [[ ! "$name" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
    echo "strih-box: box name '${name}' is invalid (lower-case letters, digits, '-'; never a path)" >&2
    return 1
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

# strih_box_unload -> forget the loaded box (the next accessor lazily loads the default again).
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
  # shellcheck disable=SC2086  # word-split the validated space-separated number list on purpose
  printf '%s\n' $c
}

# strih_lx_cg_sender -> the venue CG sender name, or empty when the fact is `none` (no CG inputs).
strih_lx_cg_sender() {
  local c
  c="$(strih_box_fact STRIH_CG_SENDER)" || return 1
  [ "$c" = none ] || printf '%s' "$c"
}

# strih_lx_intercom_config -> the repo-relative intercom hub routing file (fact STRIH_INTERCOM_CONFIG),
# installed as /etc/intercom-hub/intercom.toml.
strih_lx_intercom_config() { strih_box_fact STRIH_INTERCOM_CONFIG; }

# strih_lx_nic_driver -> the kernel driver of the ONE rig NDI NIC (fact STRIH_NIC_DRIVER, the NIC
# selection rule shared by the baseline tuning, the boot IRQ oneshot and verify-strih).
strih_lx_nic_driver() { strih_box_fact STRIH_NIC_DRIVER; }

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
