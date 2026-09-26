#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the DANTESYNC_FLEET table + pure helpers, no top-level
# statements beyond defaults) -- the sibling scripts/lib/obs-fleet.sh convention: sourcing this file
# executes it in the CALLER's shell, so a `set -euo pipefail` here would leak into every caller. Each
# caller owns its own strictness.
#
# scripts/lib/dantesync-fleet.sh -- issue 1372 part B: the ONE declared list of every machine that
# runs dantesync, and the single place each dantesync consumer derives its node set from.
#
# WHY: before this list, "which boxes run dantesync" was answered separately by each consumer, from
# the two VIDEO-fleet lists only (camera-set.sh + obs-fleet.sh), plus literals. The audio-VLAN PCs
# mbc (10.77.7.232) and fohabl (10.77.7.30) run dantesync too, yet no gate, upgrader or watchdog read
# them. On 25.9.2026 mbc was one release behind the pin and both carried a different config (no
# gm_allowlist; fohabl without phase_slew), and the owner found it by hand again. Owner (verbatim,
# 25.9.2026): "chcem aby si prevzal ... zodpovednost za vsetky stroje kde bezi dantesync ... teda aj
# za mbc a ablfoh". This is the camera-set.sh / obs-fleet.sh analogue for the dantesync fleet:
# adding a node is one row here, and every consumer picks it up.
#
# THE NODE SET (dantesync_fleet_rows):
#   * the camboxes -- every camera camera_resolve (scripts/camera-set.sh) knows, walked cam1, cam2,
#     ... to the first unknown name (the ndi-discovery.sh walk; never a literal range, per
#     .claude/rules/camera-active-set.md). All of them, not CAMERA_ACTIVE_SET: a camera retired from
#     MEASUREMENT still runs dantesync and can still lose the clock.
#   * the DANTESYNC_FLEET table below: dev1, the OBS boxes (addresses from obs-fleet.sh, never a
#     second literal), and the audio-VLAN PCs.
#   lv1 (10.77.7.100) is deliberately ABSENT: the owner ruled it an operational PC that is on the
#   network only temporarily (ROZHODNUTÉ on issue 1372, 25.9.2026: "lv1 je pc ktory je operativne na
#   nasej sieti. zatial vynechaj").
#
# ROW FORMAT (the table, and every line dantesync_fleet_rows prints):
#   name|addr|os|role|homegate|user|credvar
#   * addr     -- a literal IP/hostname, `obs:<obs-fleet name>` (resolved through obs_fleet_host at
#                 read time), or `local` (dev1 itself, read without ssh).
#   * os       -- linux | windows (which reader/config path applies).
#   * role     -- video | audio | ntp-master: the canonical config template the drift check compares
#                 against (scripts/dantesync-canonical-config.json) and the grandmaster the node must
#                 lock to (dantesync_fleet_role_gm_host).
#   * homegate -- always (a fixed box) | obsfleet (obs_fleet_is_home decides: the traveling
#                 resolume) | local (dev1). A `retired` obs-fleet box is dropped from the rows.
#   * user     -- the ssh login; `-` = the consumer's own default login.
#   * credvar  -- the NAME of the env var holding this node's ssh password. Empty = the consumer's
#                 default fleet password. The VALUE is never written in a committed file: it comes
#                 from the environment or from the dev1-local credential file (below).
#
# Python twin: scripts/dantesync_fleet.py reads the SAME table and walks the SAME camera arms;
# tests/python/test_dantesync_fleet_1372.py pins the two to identical output.

# --- the two sources this list builds on (lazy-sourced; a caller that already has them keeps its own)
if ! command -v camera_resolve >/dev/null 2>&1; then
  # shellcheck source=scripts/camera-set.sh
  . "${BASH_SOURCE[0]%/*}/../camera-set.sh"
fi
if ! command -v obs_fleet_host >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/obs-fleet.sh
  . "${BASH_SOURCE[0]%/*}/obs-fleet.sh"
fi
if ! command -v rig_grandmaster_host >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/rig-grandmaster.sh
  . "${BASH_SOURCE[0]%/*}/rig-grandmaster.sh"
fi

# DANTESYNC_FLEET -- the non-camera dantesync nodes (env-overridable as a whole for tests/ops).
#   dev1      -- the control box; it runs every dev1-side gate, so its clock is never exempt.
#   strih-lx  -- the production strih and the fleet NTP master (ntp_server_mode on, upstream NTP).
#   stream / resolume / imag -- the other OBS boxes (imag is `retired` in obs-fleet.sh today, so it
#                drops out automatically and returns with a one-word flip there).
#   mbc       -- the Master Broadcast Console (Ableton, the measurement-audio chain), audio VLAN.
#   fohabl    -- the FOH Ableton PC that hosts the AIC128-D card, audio VLAN, ssh user `master` with
#                its own password (DANTESYNC_FOHABL_SSH_PASS).
DANTESYNC_FLEET="${DANTESYNC_FLEET:-dev1|local|linux|video|local|-|
strih-lx|obs:strih-lx|linux|ntp-master|obsfleet|-|
stream|obs:stream|windows|video|obsfleet|-|
resolume|obs:resolume|windows|video|obsfleet|-|
imag|obs:imag|linux|video|obsfleet|-|
mbc|10.77.7.232|windows|audio|always|-|
fohabl|10.77.7.30|windows|audio|always|master|DANTESYNC_FOHABL_SSH_PASS}"

# The audio-VLAN PTP grandmaster: an Audinate device, locked to the same clock as the video
# grandmaster video-clock.lan (#1367 comment 5832526338). Since 25.9.2026 ~21:07 the leader is
# 10.77.7.104 (MAC 00:1d:c1:08:02:14/15); the earlier leader 10.77.7.106 (00:1d:c1:1a:44:30) is
# unreachable. A 20-min FOH VBAN capture on 26.9. put the audio Dante rate within 0.06 ppm of the
# video clock. There is no DNS name for it yet, so it is an address here -- the ONE place it is
# written; the canonical audio config's gm_allowlist and the clock watchdog's audio grading both read
# it from this variable.
DANTESYNC_AUDIO_GM_HOST="${DANTESYNC_AUDIO_GM_HOST:-10.77.7.104}"

# The dev1-local file that carries per-node ssh passwords by NAME (KEY=VALUE lines, mode 0600, never
# committed). Only keys that a fleet row names as its credvar are read from it.
DANTESYNC_FLEET_CRED_FILE="${DANTESYNC_FLEET_CRED_FILE:-$HOME/.config/camera-box/dantesync-fleet.env}"

# The config.json path per OS (the drift check reads it; nothing here ever writes it).
# shellcheck disable=SC2034  # consumed cross-file by scripts/dantesync-config-drift.sh
DANTESYNC_FLEET_CONFIG_LINUX="/etc/dantesync/config.json"
# shellcheck disable=SC2034  # consumed cross-file by scripts/dantesync-config-drift.sh
DANTESYNC_FLEET_CONFIG_WINDOWS='C:\ProgramData\DanteSync\config.json'

# A bound on the camera_resolve walk (a guard against a runaway loop, not a roster).
DANTESYNC_FLEET_CAMERA_MAX=99

# _dantesync_fleet_camera_rows -> one row per camera camera_resolve knows. Runs in a SUBSHELL so the
# caller's CAMERA_IP / CAMERA_NAME are never clobbered.
_dantesync_fleet_camera_rows() {
  (
    n=1
    while [ "$n" -le "$DANTESYNC_FLEET_CAMERA_MAX" ] && camera_resolve "cam$n" 2>/dev/null; do
      printf 'cam%s|%s|linux|video|always|root|\n' "$n" "$CAMERA_IP"
      n=$((n + 1))
    done
  )
}

# dantesync_fleet_rows -> every current dantesync node, one `name|addr|os|role|homegate|user|credvar`
# line each: the cameras first (camera order), then the table rows in table order. `obs:<name>`
# addresses are resolved through obs_fleet_host; an obs-fleet box whose home-check is `retired` is
# dropped; a malformed row or an unknown obs-fleet name fails loudly (stderr + rc 1).
dantesync_fleet_rows() {
  local line name addr os role homegate user credvar pipes obsname check out rc=0
  out="$(_dantesync_fleet_camera_rows)"
  [ -n "$out" ] && printf '%s\n' "$out"
  while IFS= read -r line; do
    [ -n "${line//[[:space:]]/}" ] || continue
    IFS='|' read -r name addr os role homegate user credvar <<<"$line"
    pipes="${line//[^|]/}"
    if [ -z "$name" ] || [ -z "$addr" ] || [ -z "$os" ] || [ -z "$role" ] || [ -z "$homegate" ] \
      || [ -z "$user" ] || [ "${#pipes}" -ne 6 ]; then
      echo "dantesync-fleet: malformed DANTESYNC_FLEET row '$line' (expected name|addr|os|role|homegate|user|credvar)" >&2
      rc=1
      continue
    fi
    case "$addr" in
      obs:*)
        obsname="${addr#obs:}"
        check="$(obs_fleet_home_check "$obsname" 2>/dev/null)" || {
          echo "dantesync-fleet: row '$name' names obs-fleet box '$obsname' absent from OBS_FLEET" >&2
          rc=1
          continue
        }
        [ "$check" = "retired" ] && continue
        addr="$(obs_fleet_host "$obsname" 2>/dev/null)" || addr=""
        if [ -z "$addr" ]; then
          echo "dantesync-fleet: row '$name': no address for obs-fleet box '$obsname' (obs_fleet_host failed)" >&2
          rc=1
          continue
        fi
        ;;
    esac
    printf '%s|%s|%s|%s|%s|%s|%s\n' "$name" "$addr" "$os" "$role" "$homegate" "$user" "$credvar"
  done <<EOF
${DANTESYNC_FLEET}
EOF
  return "$rc"
}

# dantesync_fleet_field <name> <field> -> one field of NAME's row (stdout). Fields: addr os role
# homegate user credvar. rc 1 (no output) for an unknown name or field.
dantesync_fleet_field() {
  local want="${1:-}" field="${2:-}" name addr os role homegate user credvar
  while IFS='|' read -r name addr os role homegate user credvar; do
    [ "$name" = "$want" ] || continue
    case "$field" in
      addr) printf '%s' "$addr" ;;
      os) printf '%s' "$os" ;;
      role) printf '%s' "$role" ;;
      homegate) printf '%s' "$homegate" ;;
      user) printf '%s' "$user" ;;
      credvar) printf '%s' "$credvar" ;;
      *) return 1 ;;
    esac
    return 0
  done < <(dantesync_fleet_rows 2>/dev/null || true)
  return 1
}

# dantesync_fleet_names [<field>=<value> ...] -> the space-separated NAMES of every node whose row
# matches ALL the given filters (e.g. `role=audio`, `os=windows`, `homegate=obsfleet`).
dantesync_fleet_names() {
  local out="" name os role homegate f key val ok
  while IFS='|' read -r name _ os role homegate _ _; do
    ok=1
    for f in "$@"; do
      key="${f%%=*}"; val="${f#*=}"
      case "$key" in
        os) [ "$os" = "$val" ] || ok=0 ;;
        role) [ "$role" = "$val" ] || ok=0 ;;
        homegate) [ "$homegate" = "$val" ] || ok=0 ;;
        *) echo "dantesync-fleet: unknown filter '$f' (expected os=, role= or homegate=)" >&2; return 1 ;;
      esac
    done
    [ "$ok" = 1 ] && out="${out:+$out }$name"
  done < <(dantesync_fleet_rows 2>/dev/null || true)
  printf '%s' "$out"
}

# dantesync_fleet_camera_names -> the space-separated camera node names (the camera_resolve walk).
dantesync_fleet_camera_names() {
  local out="" name
  while IFS='|' read -r name _; do
    out="${out:+$out }$name"
  done < <(_dantesync_fleet_camera_rows)
  printf '%s' "$out"
}

# dantesync_fleet_fixed_names -> the always-home NON-camera nodes (the audio-VLAN PCs today): every
# `homegate=always` row that is not a camera.
dantesync_fleet_fixed_names() {
  local out="" name cams
  cams=" $(dantesync_fleet_camera_names) "
  for name in $(dantesync_fleet_names homegate=always); do
    case "$cams" in *" $name "*) continue ;; esac
    out="${out:+$out }$name"
  done
  printf '%s' "$out"
}

# dantesync_fleet_present <name> -> rc 0 iff NAME should be read this pass: a traveling obs-fleet box
# only while obs_fleet_is_home (the #1296 gate; its I/O), every other node always. Unknown -> rc 1.
dantesync_fleet_present() {
  local homegate
  homegate="$(dantesync_fleet_field "${1:-}" homegate)" || return 1
  if [ "$homegate" = "obsfleet" ]; then
    obs_fleet_is_home "$1"
    return
  fi
  return 0
}

# dantesync_fleet_spec <linux|win|local> <default-user> [--present] -> the node spec for ONE arm of
# the version gate / fleet upgrader: "name=user@addr ..." for linux/win (a `-` user becomes
# DEFAULT-USER), the bare names for local. --present drops a traveling box that is away (I/O).
dantesync_fleet_spec() {
  local arm="${1:-}" defuser="${2:-}" present="${3:-}" out="" name addr os homegate user
  case "$arm" in linux | win | local) ;; *)
    echo "dantesync-fleet: unknown arm '$arm' (expected linux, win or local)" >&2
    return 1 ;;
  esac
  while IFS='|' read -r name addr os _ homegate user _; do
    if [ "$arm" = "local" ]; then
      [ "$homegate" = "local" ] || continue
      out="${out:+$out }$name"
      continue
    fi
    [ "$homegate" = "local" ] && continue
    case "$arm:$os" in linux:linux | win:windows) ;; *) continue ;; esac
    if [ "$present" = "--present" ] && ! dantesync_fleet_present "$name"; then
      continue
    fi
    [ "$user" = "-" ] && user="$defuser"
    out="${out:+$out }${name}=${user}@${addr}"
  done < <(dantesync_fleet_rows 2>/dev/null || true)
  printf '%s' "$out"
}

# dantesync_fleet_cred_var_for_target <user@addr> -> the credvar NAME of the fleet row that TARGET
# addresses (stdout), or nothing when the row has none / no row matches (the consumer then uses its
# default fleet password). A `-` user in the row matches any login.
dantesync_fleet_cred_var_for_target() {
  local target="${1:-}" tuser taddr addr user credvar
  tuser="${target%%@*}"; taddr="${target#*@}"
  [ "$tuser" = "$target" ] && tuser=""
  while IFS='|' read -r _ addr _ _ _ user credvar; do
    [ "$addr" = "$taddr" ] || continue
    [ "$user" = "-" ] || [ -z "$tuser" ] || [ "$user" = "$tuser" ] || continue
    printf '%s' "$credvar"
    return 0
  done < <(dantesync_fleet_rows 2>/dev/null || true)
  return 0
}

# dantesync_fleet_load_credentials -> export every credvar a fleet row names from
# DANTESYNC_FLEET_CRED_FILE when it is not already set in the environment. The file is PARSED
# (KEY=VALUE, optional surrounding quotes), never sourced, and only row-named keys are taken; a
# missing file is fine (the affected nodes then read UNKNOWN, never a guess).
dantesync_fleet_load_credentials() {
  local vars var line key val
  [ -r "$DANTESYNC_FLEET_CRED_FILE" ] || return 0
  vars="$(dantesync_fleet_rows 2>/dev/null | awk -F'|' '$7 != "" {print $7}' | sort -u || true)"
  for var in $vars; do
    [ -n "${!var:-}" ] && continue
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in "$var="*) ;; *) continue ;; esac
      key="${line%%=*}"; val="${line#*=}"
      case "$val" in \"*\") val="${val#\"}"; val="${val%\"}" ;; \'*\') val="${val#\'}"; val="${val%\'}" ;; esac
      [ "$key" = "$var" ] && export "$var=$val"
    done <"$DANTESYNC_FLEET_CRED_FILE"
  done
  return 0
}

# dantesync_fleet_role_gm_host <role> -> the grandmaster HOST a node of ROLE must lock to: the video
# roles use the rig grandmaster name (video-clock.lan, scripts/lib/rig-grandmaster.sh), the audio
# role DANTESYNC_AUDIO_GM_HOST. Unknown role -> rc 1.
dantesync_fleet_role_gm_host() {
  case "${1:-}" in
    video | ntp-master) rig_grandmaster_host ;;
    audio) printf '%s\n' "$DANTESYNC_AUDIO_GM_HOST" ;;
    *) echo "dantesync-fleet: unknown role '${1:-}' (expected video, audio or ntp-master)" >&2; return 1 ;;
  esac
}

# dantesync_fleet_role_gm_ip <role> -> that grandmaster's IPv4 (stdout, rc 0), or rc 1 when it does
# not resolve. The video roles go through rig_grandmaster_ip (its RIG_GRANDMASTER_IP override + loud
# failure); an audio IPv4 literal is returned as is.
dantesync_fleet_role_gm_ip() {
  local host
  case "${1:-}" in
    video | ntp-master) rig_grandmaster_ip ;;
    audio)
      host="$DANTESYNC_AUDIO_GM_HOST"
      if [[ "$host" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        printf '%s\n' "$host"
        return 0
      fi
      host="$(rig_grandmaster_resolve "$host")"
      [ -n "$host" ] || { echo "dantesync-fleet: cannot resolve the audio grandmaster '$DANTESYNC_AUDIO_GM_HOST'" >&2; return 1; }
      printf '%s\n' "$host"
      ;;
    *) echo "dantesync-fleet: unknown role '${1:-}'" >&2; return 1 ;;
  esac
}
