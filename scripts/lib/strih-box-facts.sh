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
# metacharacter, a quote, a glob). Values are later embedded in generated configs and commands, so
# they stay plain text: letters, digits, space, `.` `-` `_` `/` `:` `(` `)` `,` `=` `+` `@` `%`.
_strih_box_unsafe() {
  local v="${1-}" i c
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
    client) [ -n "${f[STRIH_DANTESYNC_UPSTREAM]}" ] \
      || { echo "strih-box ${name}: STRIH_DANTESYNC_UPSTREAM (the NTP master host) is required for the client role" >&2; bad=1; } ;;
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
    [[ "${f[$k]}" =~ ^[A-Za-z0-9.-]+$ ]] \
      || { echo "strih-box ${name}: ${k} '${f[$k]}' is not a host name / address" >&2; bad=1; }
  done
  [[ "${f[STRIH_CAMERAS]}" =~ ^[0-9]+( [0-9]+)*$ ]] \
    || { echo "strih-box ${name}: STRIH_CAMERAS '${f[STRIH_CAMERAS]}' must be space-separated camera numbers" >&2; bad=1; }
  [ "$bad" = 0 ]
}

# strih_box_load NAME -> load box NAME's facts into the STRIH_BOX_FACTS map (+ STRIH_BOX_LOADED=NAME),
# rc 0; rc 1 with the reasons on stderr when the name, the file or a fact is invalid, when the fleet
# list has a row for NAME whose host is an IP other than STRIH_IP, or when a per-run env knob
# (STRIH_LX_IP / STRIH_LX_DANTESYNC_ROLE) contradicts the loaded fact. Nothing is loaded on failure.
strih_box_load() {
  local name="${1-}" file parsed line fleet_ip
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
  [ -n "${STRIH_BOX_LOADED:-}" ] && return 0
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
