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
#   * the camboxes -- the cameras each VENUE has (issue 1413): every camera camera_resolve
#     (scripts/camera-set.sh) knows, walked cam1, cam2, ... to the first unknown name (never a literal
#     range, .claude/rules/camera-active-set.md), and each one that a strih fact file
#     (scripts/strih-boxes/<box>.env, PARSED never sourced) lists in its STRIH_CAMERAS. A camera of
#     the HOME venue (DANTESYNC_FLEET_HOME_VENUE, snv) is a plain `video` / `always` row; a camera of
#     another venue (Poprad: cam1/4/6/7 since 11.10.2026) is a `video-<venue>` / `reach` row, locked
#     to that venue's grandmaster and skipped while unreachable. The address comes from the venue's
#     STRIH_CAMERA_DEV1_NET: `direct` = camera_resolve's address, `A.B.C` = that /24 + the camera's
#     camera_resolve last octet (dev1 reaches the PP LAN as 10.76.x through companion-pp). Not
#     CAMERA_ACTIVE_SET: a camera retired from MEASUREMENT still runs dantesync and can still lose the
#     clock. A camera no venue lists is in no row; one listed by two venues fails loudly.
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
#   * role     -- video | audio | ntp-master, or a VENUE role `video-<venue>` / `ntp-master-<venue>`
#                 (issue 1412: `video-pp`, `ntp-master-pp`): the canonical config template the drift
#                 check compares against (scripts/dantesync-canonical-config.json) and the grandmaster
#                 the node must lock to (dantesync_fleet_role_gm_host). The plain roles are the home
#                 venue (SNV, video-clock.lan); a venue role locks to ITS venue's grandmaster, read from
#                 that venue's NTP-master strih fact file (dantesync_fleet_venue_gm_host).
#   * homegate -- always (a fixed box) | obsfleet (obs_fleet_is_home decides: the traveling
#                 resolume) | reach (issue 1412: read only while the box answers on ssh :22 -- the
#                 Poprad boxes, reachable from dev1 only while the companion-pp subnet route is up) |
#                 local (dev1). A `retired` obs-fleet box is dropped from the rows.
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
#   strih-pp / stream-pp / resolume-pp -- issue 1412: the Poprad (PP) venue. strih-pp is the PP NTP
#                master (scripts/strih-boxes/strih-pp.env: STRIH_DANTESYNC_ROLE=server), stream-pp and
#                resolume-pp (Windows, no obs-fleet row) follow it; all three lock to the PP PTP
#                grandmaster (role `*-pp`). dev1 reaches the PP LAN as 10.76.8.x through the
#                companion-pp subnet router, so the rows dial NAMES (strih-pp/stream-pp through their
#                obs-fleet rows), and their homegate is `reach`: an unreachable PP box is skipped,
#                never a page or a failed gate. Owner (11.10.2026): "a tiez tam nie je dantesync tam
#                nie je updatnuty cize potrebujes zaradit tento pc do updatovania".
DANTESYNC_FLEET="${DANTESYNC_FLEET:-dev1|local|linux|video|local|-|
strih-lx|obs:strih-lx|linux|ntp-master|obsfleet|-|
stream|obs:stream|windows|video|obsfleet|-|
resolume|obs:resolume|windows|video|obsfleet|-|
imag|obs:imag|linux|video|obsfleet|-|
mbc|10.77.7.232|windows|audio|always|-|
fohabl|10.77.7.30|windows|audio|always|master|DANTESYNC_FOHABL_SSH_PASS
strih-pp|obs:strih-pp|linux|ntp-master-pp|reach|-|
stream-pp|obs:stream-pp|linux|video-pp|reach|-|
resolume-pp|resolume-pp.lan|windows|video-pp|reach|-|}"

# The audio-VLAN PTP grandmaster: an Audinate device, locked to the same clock as the video
# grandmaster video-clock.lan (#1367 comment 5832526338). Since 25.9.2026 ~21:07 the leader is
# 10.77.7.104 (MAC 00:1d:c1:08:02:14/15); the earlier leader 10.77.7.106 (00:1d:c1:1a:44:30) is
# unreachable. A 20-min FOH VBAN capture on 26.9. put the audio Dante rate within 0.06 ppm of the
# video clock. There is no DNS name for it yet, so it is an address here -- the ONE place it is
# written; the canonical audio config's gm_allowlist and the clock watchdog's audio grading both read
# it from this variable.
DANTESYNC_AUDIO_GM_HOST="${DANTESYNC_AUDIO_GM_HOST:-10.77.7.104}"

# The strih box fact files (scripts/strih-boxes/<box>.env, issue 1361) -- a venue role's grandmaster
# is read from the ONE file whose STRIH_VENUE is that venue and whose STRIH_DANTESYNC_ROLE is `server`
# (the venue's NTP master), field STRIH_DANTE_GM. Env-overridable for tests.
DANTESYNC_FLEET_STRIH_BOXES_DIR="${DANTESYNC_FLEET_STRIH_BOXES_DIR:-${BASH_SOURCE[0]%/*}/../strih-boxes}"

# The port the `reach` homegate probes: ssh, which every PP box answers (Linux and Windows alike)
# whenever it is up and the PP link is up. A dead dantesync on a reachable box is then still read
# (UNKNOWN / NO_DANTESYNC), never hidden behind the gate.
DANTESYNC_FLEET_REACH_PORT="${DANTESYNC_FLEET_REACH_PORT:-22}"

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

# issue 1413: the HOME venue (its cameras are plain `video` / `always` rows, locked to the home rig's
# grandmaster video-clock.lan). Every other venue's cameras get the venue role + the `reach` gate.
DANTESYNC_FLEET_HOME_VENUE="${DANTESYNC_FLEET_HOME_VENUE:-snv}"

# _dantesync_fleet_venue_cameras -> one `file|venue|dev1-net|cameras` line per strih fact file that
# lists cameras (STRIH_CAMERAS other than none / empty). The files are PARSED (KEY=VALUE lines),
# never sourced.
_dantesync_fleet_venue_cameras() {
  local f line key val v c net
  # The version gate builds its specs under `set -f`; the fact-file glob must expand anyway.
  # `local -` restores the caller's noglob setting when this function returns.
  local -
  set +f
  for f in "$DANTESYNC_FLEET_STRIH_BOXES_DIR"/*.env; do
    [ -f "$f" ] || continue
    v="" c="" net=""
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in \#* | "") continue ;; esac
      key="${line%%=*}"; val="${line#*=}"
      case "$key" in
        STRIH_VENUE) v="$val" ;;
        STRIH_CAMERAS) c="$val" ;;
        STRIH_CAMERA_DEV1_NET) net="$val" ;;
      esac
    done <"$f"
    case "$c" in "" | none) continue ;; esac
    printf '%s|%s|%s|%s\n' "${f##*/}" "$v" "$net" "$c"
  done
}

# _dantesync_fleet_camera_rows -> one row per camera a venue lists, in camera_resolve order (cam1,
# cam2, ... to the first unknown name). A home-venue camera: `camN|<camera_resolve ip>|linux|video|
# always|root|`; another venue's: `camN|<dev1 address>|linux|video-<venue>|reach|root|`. Runs in a
# SUBSHELL so the caller's CAMERA_IP / CAMERA_NAME are never clobbered. A camera listed by two
# venues, a venue name that is not lower-case letters + digits, or a dev1 network that is neither
# `direct` nor an A.B.C prefix prints no row for that camera and fails (stderr + rc 1).
_dantesync_fleet_camera_rows() {
  (
    venues="$(_dantesync_fleet_venue_cameras)"
    rc=0 n=1
    while [ "$n" -le "$DANTESYNC_FLEET_CAMERA_MAX" ] && camera_resolve "cam$n" 2>/dev/null; do
      hit="" hvenue="" hnet=""
      while IFS='|' read -r file venue net cams; do
        [ -n "$file" ] || continue
        case " $cams " in *" $n "*) ;; *) continue ;; esac
        if [ -n "$hit" ]; then
          echo "dantesync-fleet: cam$n is listed by two venue fact files ($hit and $file) -- a camera is in one venue" >&2
          hit="DUP"
          break
        fi
        hit="$file" hvenue="$venue" hnet="$net"
      done <<<"$venues"
      if [ "$hit" = DUP ]; then
        rc=1
      elif [ -n "$hit" ]; then
        if [[ ! "$hvenue" =~ ^[abcdefghijklmnopqrstuvwxyz][abcdefghijklmnopqrstuvwxyz0123456789]*$ ]]; then
          echo "dantesync-fleet: $hit lists cam$n but its STRIH_VENUE '$hvenue' is not a venue name" >&2
          rc=1
        else
          case "$hnet" in
            direct) addr="$CAMERA_IP" ;;
            *)
              if [[ "$hnet" =~ ^[0123456789]+\.[0123456789]+\.[0123456789]+$ ]]; then
                addr="$hnet.${CAMERA_IP##*.}"
              else
                addr=""
              fi
              ;;
          esac
          if [ -z "$addr" ]; then
            echo "dantesync-fleet: $hit lists cam$n but its STRIH_CAMERA_DEV1_NET '$hnet' is neither direct nor an A.B.C prefix" >&2
            rc=1
          elif [ "$hvenue" = "$DANTESYNC_FLEET_HOME_VENUE" ]; then
            printf 'cam%s|%s|linux|video|always|root|\n' "$n" "$addr"
          else
            printf 'cam%s|%s|linux|video-%s|reach|root|\n' "$n" "$addr" "$hvenue"
          fi
        fi
      fi
      n=$((n + 1))
    done
    exit "$rc"
  )
}

# dantesync_fleet_rows -> every current dantesync node, one `name|addr|os|role|homegate|user|credvar`
# line each: the cameras first (camera order), then the table rows in table order. `obs:<name>`
# addresses are resolved through obs_fleet_host; an obs-fleet box whose home-check is `retired` is
# dropped; a malformed row or an unknown obs-fleet name fails loudly (stderr + rc 1).
dantesync_fleet_rows() {
  local line name addr os role homegate user credvar pipes obsname check out rc=0
  out="$(_dantesync_fleet_camera_rows)" || rc=1
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

# dantesync_fleet_camera_names -> the space-separated HOME-venue camera node names (issue 1413: the
# `always` camera rows; the clock watchdog's CAM arm dials them at their camera_resolve address).
# Another venue's cameras are `reach` rows and reach that watchdog through its REACH arm, never twice.
dantesync_fleet_camera_names() {
  local out="" name homegate
  while IFS='|' read -r name _ _ _ homegate _; do
    [ "$homegate" = always ] || continue
    out="${out:+$out }$name"
  done < <(_dantesync_fleet_camera_rows 2>/dev/null || true)
  printf '%s' "$out"
}

# dantesync_fleet_fixed_names -> the always-home NON-camera nodes (the audio-VLAN PCs today): every
# `homegate=always` row that is not a camera.
dantesync_fleet_fixed_names() {
  local out="" name cams
  cams=" $({ _dantesync_fleet_camera_rows 2>/dev/null || true; } | cut -d'|' -f1 | tr '\n' ' ') "
  for name in $(dantesync_fleet_names homegate=always); do
    case "$cams" in *" $name "*) continue ;; esac
    out="${out:+$out }$name"
  done
  printf '%s' "$out"
}

# dantesync_fleet_master_names -> the space-separated NAMES of every NTP master: role `ntp-master`
# (the home venue, strih-lx) and every venue master `ntp-master-<venue>` (issue 1412: strih-pp).
dantesync_fleet_master_names() {
  local out="" name role
  while IFS='|' read -r name _ _ role _; do
    case "$role" in ntp-master | ntp-master-*) out="${out:+$out }$name" ;; esac
  done < <(dantesync_fleet_rows 2>/dev/null || true)
  printf '%s' "$out"
}

# dantesync_fleet_present <name> -> rc 0 iff NAME should be read this pass: a traveling obs-fleet box
# only while obs_fleet_is_home (the #1296 gate; its I/O), a `reach` node only while its address
# resolves AND ssh (DANTESYNC_FLEET_REACH_PORT) answers (issue 1412; its I/O), every other node
# always. Unknown -> rc 1. OBS_FLEET_HOME, the one traveling-box force list (test/ops seam), is
# authoritative for a `reach` node too: named = present, otherwise away, no network touched.
dantesync_fleet_present() {
  local homegate addr ip
  homegate="$(dantesync_fleet_field "${1:-}" homegate)" || return 1
  case "$homegate" in
    obsfleet)
      obs_fleet_is_home "$1"
      return
      ;;
    reach)
      if [ -n "${OBS_FLEET_HOME:-}" ]; then
        case " ${OBS_FLEET_HOME} " in *" $1 "*) return 0 ;; *) return 1 ;; esac
      fi
      addr="$(dantesync_fleet_field "$1" addr)" || return 1
      ip="$(obs_fleet_resolve_host "$addr")"
      [ -n "$ip" ] || return 1
      [ "$(obs_fleet_status_probe "$ip" "$DANTESYNC_FLEET_REACH_PORT")" = 1 ]
      return
      ;;
  esac
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

# dantesync_fleet_role_venue <role> -> the VENUE of a venue role (`video-<venue>` /
# `ntp-master-<venue>`, venue = lower-case ASCII letters and digits) on stdout, rc 0; rc 1 (no
# output) for a plain role or anything else. The sets are spelled out: a bracket RANGE follows the
# locale in bash (ci-testing-gotchas.md, issue 1302).
dantesync_fleet_role_venue() {
  local role="${1:-}" venue
  case "$role" in
    video-*) venue="${role#video-}" ;;
    ntp-master-*) venue="${role#ntp-master-}" ;;
    *) return 1 ;;
  esac
  [[ "$venue" =~ ^[abcdefghijklmnopqrstuvwxyz0123456789]+$ ]] || return 1
  printf '%s' "$venue"
}

# dantesync_fleet_venue_gm_host <venue> -> the PTP grandmaster of VENUE: STRIH_DANTE_GM of the ONE
# strih fact file (DANTESYNC_FLEET_STRIH_BOXES_DIR/*.env) with STRIH_VENUE=<venue> and
# STRIH_DANTESYNC_ROLE=server -- the venue's NTP master, whose dantesync config allows exactly that
# grandmaster. The files are PARSED (KEY=VALUE lines), never sourced. No such file, two of them, or
# one without STRIH_DANTE_GM -> stderr naming the venue + rc 1, never a guessed grandmaster.
dantesync_fleet_venue_gm_host() {
  local venue="${1:-}" f line key val v r g found="" n=0 gm=""
  # issue 1413: the fact-file glob must expand under a caller's `set -f` too (restored on return).
  local -
  set +f
  for f in "$DANTESYNC_FLEET_STRIH_BOXES_DIR"/*.env; do
    [ -f "$f" ] || continue
    v="" r="" g=""
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in \#* | "") continue ;; esac
      key="${line%%=*}"; val="${line#*=}"
      case "$key" in
        STRIH_VENUE) v="$val" ;;
        STRIH_DANTESYNC_ROLE) r="$val" ;;
        STRIH_DANTE_GM) g="$val" ;;
      esac
    done <"$f"
    { [ "$v" = "$venue" ] && [ "$r" = server ]; } || continue
    found="${found:+$found }${f##*/}"
    n=$((n + 1))
    gm="$g"
  done
  if [ "$n" -eq 0 ]; then
    echo "dantesync-fleet: venue '$venue' has no NTP-master strih fact file (STRIH_VENUE=$venue + STRIH_DANTESYNC_ROLE=server) in $DANTESYNC_FLEET_STRIH_BOXES_DIR" >&2
    return 1
  fi
  if [ "$n" -gt 1 ]; then
    echo "dantesync-fleet: venue '$venue' has $n NTP-master strih fact files ($found) -- expected exactly one" >&2
    return 1
  fi
  if [ -z "$gm" ]; then
    echo "dantesync-fleet: venue '$venue' NTP-master fact file $found has no STRIH_DANTE_GM" >&2
    return 1
  fi
  printf '%s\n' "$gm"
}

# dantesync_fleet_role_gm_host <role> -> the grandmaster HOST a node of ROLE must lock to: the video
# roles use the rig grandmaster name (video-clock.lan, scripts/lib/rig-grandmaster.sh), the audio
# role DANTESYNC_AUDIO_GM_HOST, a venue role its venue's grandmaster (issue 1412). Unknown role ->
# rc 1.
dantesync_fleet_role_gm_host() {
  local venue
  case "${1:-}" in
    video | ntp-master) rig_grandmaster_host ;;
    audio) printf '%s\n' "$DANTESYNC_AUDIO_GM_HOST" ;;
    *)
      if venue="$(dantesync_fleet_role_venue "${1:-}")"; then
        dantesync_fleet_venue_gm_host "$venue"
        return
      fi
      echo "dantesync-fleet: unknown role '${1:-}' (expected video, audio, ntp-master, video-<venue> or ntp-master-<venue>)" >&2
      return 1
      ;;
  esac
}

# dantesync_fleet_role_gm_ip <role> -> that grandmaster's IPv4 (stdout, rc 0), or rc 1 when it does
# not resolve. The video roles go through rig_grandmaster_ip (its RIG_GRANDMASTER_IP override + loud
# failure); an audio or venue grandmaster that is an IPv4 literal is returned as is, a name is
# resolved. RIG_GRANDMASTER_IP never moves a venue grandmaster (it is the home rig's override).
dantesync_fleet_role_gm_ip() {
  local host ip
  case "${1:-}" in
    video | ntp-master) rig_grandmaster_ip; return ;;
    audio) host="$DANTESYNC_AUDIO_GM_HOST" ;;
    *)
      if ! dantesync_fleet_role_venue "${1:-}" >/dev/null; then
        echo "dantesync-fleet: unknown role '${1:-}'" >&2
        return 1
      fi
      host="$(dantesync_fleet_role_gm_host "$1")" || return 1
      ;;
  esac
  if [[ "$host" =~ ^[0123456789]+\.[0123456789]+\.[0123456789]+\.[0123456789]+$ ]]; then
    printf '%s\n' "$host"
    return 0
  fi
  ip="$(rig_grandmaster_resolve "$host")"
  [ -n "$ip" ] || { echo "dantesync-fleet: cannot resolve the $1 grandmaster '$host'" >&2; return 1; }
  printf '%s\n' "$ip"
}
