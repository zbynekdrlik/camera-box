#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the OBS_FLEET table + pure helpers, no top-level
# statements) -- matches the sibling scripts/lib/*.sh convention (network-reach-health.sh,
# bundle-state-health.sh, obs-watchdog-decision.sh) of deliberately NOT setting `set -euo pipefail`
# here: sourcing this file executes it in the CALLER's shell, so strict mode here would leak into
# whichever caller sources it. The caller (each dev1-side alert watchdog) sets its own strict mode.
#
# scripts/lib/obs-fleet.sh -- #1296: the ONE declared list of managed broadcast-OBS boxes the dev1
# fleet mechanisms watch + version-check, and the single place each watchdog's box-roster default is
# derived from. This is the direct analogue of scripts/camera-set.sh / CAMERA_ACTIVE_SET
# (.claude/rules/camera-active-set.md) for the camera fleet: before #1296 the fleet membership was
# duplicated as SIX independent literals (the five `BOXES="${X_BOXES:-strih|… stream|…}"` defaults
# in audio-lag/av-step/bundle-state/network-reach/vb-matrix-alert-watchdog.sh, plus obs-liveness's
# hardcoded STRIH_HOST/STREAM_HOST --box pair), so registering a new box meant editing six files
# with no source of truth. Now each watchdog derives its default from `obs_fleet_boxes <facet>` and
# the env override (`X_BOXES=`) stays byte-compatible, so a new box is one table edit here.
#
# THE TABLE (`OBS_FLEET`): one `name|host-or-ip|class|home-check` row per managed OBS box.
#   * name        -- the stable box name the watchdogs log + key state on (strih/stream/imag/resolume).
#   * host-or-ip  -- the address probes dial. A literal IP for a fixed box; a HOSTNAME for a box whose
#                    DHCP lease drifts (resolume.lan), so `getent`/ping/`/dev/tcp` resolve it live.
#   * class       -- windows-genlock | linux-genlock (the genlock-build platform; used by the
#                    version-integrity / rig-health rows to pick the right parity check).
#   * home-check  -- `always` (a permanent box: obs_fleet_is_home is unconditionally true) or
#                    `traveling` (home ONLY when resolvable AND its OBS-WS answers; see below).
#
# FACET POLICY (`obs_fleet_boxes <facet>`): which boxes carry each dev1-side facet, decided honestly
# per each watchdog's own header, NOT per-entry -- so the entry row stays the fixed 4-field shape:
#   audio-lag     = strih stream      (resolume EXCLUDED: no mbc audio chain on the CG box)
#   av-step       = stream            (the av-sync dock lives on the stream box only, #1267)
#   vb-matrix     = strih stream      (resolume EXCLUDED: no VB-Matrix install on the CG box)
#   bundle-state  = strih stream resolume
#   network-reach = strih stream resolume   (resolume report-only unless obs_fleet_is_home -- below)
#   obs-liveness  = strih stream resolume   (resolume polled only while obs_fleet_is_home -- below)
#
# TRAVELING-BOX SAFETY (resolume is home only sometimes): a naive add to the PAGING watchdogs would
# false-page whenever resolume is away (the owner's hardest sensitivity -- the #739 5x false-page
# incident). So the is_home gate is applied by each CONSUMER, never baked into this pure list:
#   * network-reach keeps resolume REPORT-ONLY by default and promotes it to a paging node ONLY while
#     obs_fleet_is_home resolume holds (the NETWORK_REACH_REPORT_ONLY_BOXES env override still wins).
#   * obs-liveness includes resolume in its poll set ONLY while obs_fleet_is_home resolume holds.
#   * bundle-state defers a fully-unreachable box to #1001 already (no page/restart against a dark
#     box), so it is traveling-safe without an is_home gate.
#
# home-check PORT (#1296): the ticket's example home-check was "resolves + :8898/status answers", but
# RESOLUME-SNV carries NO dantesync (:8898 is dantesync's status port, dantesync#47) per
# tests/harness_network_reach_watchdog_811.rs -- so :8898 would never answer even when home, making
# the gate inert. The honest home signal is the genlock cg-obs's OBS-WebSocket port :4455 (live when
# home, #1295), overridable via OBS_FLEET_HOME_PORT. CAVEAT: resolume.lan currently resolves to
# 10.77.9.201, the SAME IP `bridge` lists in targets.md (event-LAN DHCP collision) -- if `bridge`
# answers :4455 at .201 while resolume is off, is_home may read a FALSE home, but network-reach then
# also classifies that address REACHABLE so it still never pages; always confirm box IDENTITY
# (getent hosts resolume.lan + its OBS profile, rig-state-inspection.md §2) before a supervisor flips
# resolume to a permanent paging fixture.

# OBS_FLEET -- the table (env-overridable as a whole for tests/ops). One row per line,
# `name|host-or-ip|class|home-check`. Blank lines are ignored by the parser below.
OBS_FLEET="${OBS_FLEET:-strih|10.77.9.202|windows-genlock|always
stream|10.77.9.204|windows-genlock|always
imag|10.77.9.182|linux-genlock|always
resolume|resolume.lan|windows-genlock|traveling}"

# _obs_fleet_entry <name> -> prints the whole `name|host|class|home-check` row for NAME on stdout and
# returns 0; returns 1 (no output) for an unknown name. Word-exact on the leading `name|` so a
# hypothetical `stream2` can never match `stream`. The here-doc read consumes the WHOLE stream (no
# early `break`/`exit`), so it is SIGPIPE-safe under a caller's `set -euo pipefail`.
_obs_fleet_entry() {
  local name="${1:-}" line found=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$line" in
      "${name}|"*) found="$line" ;;
    esac
  done <<EOF
${OBS_FLEET}
EOF
  [ -n "$found" ] || return 1
  printf '%s' "$found"
}

# obs_fleet_host <name> / obs_fleet_class <name> / obs_fleet_home_check <name> -> the 2nd/3rd/4th
# field of NAME's row (stdout), or return 1 for an unknown name. FACT lookups -- never policy.
obs_fleet_host() {
  local e; e="$(_obs_fleet_entry "${1:-}")" || return 1
  e="${e#*|}"; printf '%s' "${e%%|*}"
}
obs_fleet_class() {
  local e; e="$(_obs_fleet_entry "${1:-}")" || return 1
  e="${e#*|}"; e="${e#*|}"; printf '%s' "${e%%|*}"
}
obs_fleet_home_check() {
  local e; e="$(_obs_fleet_entry "${1:-}")" || return 1
  printf '%s' "${e##*|}"
}

# obs_fleet_facet_members <facet> -> the space-separated box NAMES carrying FACET (stdout), decided
# by the policy table documented in this file's header. Unknown facet -> error on stderr + return 1
# (fail-closed: a caller must never silently derive an empty roster from a typo'd facet).
obs_fleet_facet_members() {
  local facet="${1:-}"
  case "$facet" in
    audio-lag)     printf 'strih stream' ;;
    av-step)       printf 'stream' ;;
    vb-matrix)     printf 'strih stream' ;;
    bundle-state)  printf 'strih stream resolume' ;;
    network-reach) printf 'strih stream resolume' ;;
    obs-liveness)  printf 'strih stream resolume' ;;
    *)
      echo "obs-fleet: unknown facet '${facet}' (expected one of: audio-lag av-step vb-matrix bundle-state network-reach obs-liveness)" >&2
      return 1
      ;;
  esac
}

# obs_fleet_boxes <facet> -> the `name|host name|host ...` space-separated pairs for every member
# carrying FACET, in the facet's declared order -- the exact shape every watchdog's `BOXES=` consumes
# (`for pair in $BOXES; do box="${pair%%|*}"; ip="${pair##*|}"`). Resolves each member's host from the
# OBS_FLEET table (single source of truth); an unknown member name (a table/policy mismatch) fails
# loudly rather than emitting a nameless pair.
obs_fleet_boxes() {
  local facet="${1:-}" members m host out=""
  members="$(obs_fleet_facet_members "$facet")" || return 1
  for m in $members; do
    host="$(obs_fleet_host "$m")" || {
      echo "obs-fleet: facet '${facet}' names box '${m}' absent from OBS_FLEET" >&2
      return 1
    }
    out="${out:+$out }${m}|${host}"
  done
  printf '%s' "$out"
}

# -- is_home probes (the ONLY I/O in this lib; kept as overridable seams for Tier-0 testing) --------
# obs_fleet_resolve_host <host> -> the FIRST resolved address on stdout (empty if unresolvable).
# `getent ahosts` (no early `exit` in awk -> no SIGPIPE) covers a hostname AND a literal IP (which
# resolves to itself). Override in a test to stub resolution. `|| true` keeps it drain-safe.
obs_fleet_resolve_host() {
  getent ahosts "${1:-}" 2>/dev/null | awk 'NR==1{print $1}' || true
}
# obs_fleet_status_probe <host> <port> -> 1 (a TCP connect to host:port succeeded) | 0. The OBS-WS
# port answering is the "box is home + serving" signal. Bash /dev/tcp so no nc/curl dependency; a
# subshell + timeout bounds a hung connect. Override in a test to stub the probe.
obs_fleet_status_probe() {
  local host="${1:-}" port="${2:-}"
  if timeout "${OBS_FLEET_STATUS_TIMEOUT:-4}" bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null; then
    printf '1'
  else
    printf '0'
  fi
}

# obs_fleet_is_home <name> -> returns 0 iff NAME is currently "home" (reachable + serving), 1 if away
# or unknown. A `home-check=always` box is unconditionally home. A `traveling` box is home iff it
# resolves AND its OBS-WS (OBS_FLEET_HOME_PORT, default 4455) answers. TEST/OPS OVERRIDE: when
# OBS_FLEET_HOME is set (space-separated names) it is authoritative -- NAME is home iff it is a word
# in that list -- so a test (or a supervisor forcing a known state) never depends on live I/O. An
# unknown name returns 1 (fail-closed: an untracked box is never treated as home).
obs_fleet_is_home() {
  local name="${1:-}" check host ip
  if [ -n "${OBS_FLEET_HOME:-}" ]; then
    case " ${OBS_FLEET_HOME} " in
      *" ${name} "*) return 0 ;;
      *) return 1 ;;
    esac
  fi
  check="$(obs_fleet_home_check "$name")" || return 1
  case "$check" in
    always) return 0 ;;
    traveling)
      host="$(obs_fleet_host "$name")" || return 1
      ip="$(obs_fleet_resolve_host "$host")"
      [ -n "$ip" ] || return 1
      [ "$(obs_fleet_status_probe "$host" "${OBS_FLEET_HOME_PORT:-4455}")" = 1 ] && return 0
      return 1
      ;;
    *) return 1 ;;
  esac
}
