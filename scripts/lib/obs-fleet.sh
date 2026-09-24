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
#   * name        -- the stable box name the watchdogs log + key state on (strih-lx/stream/imag/resolume).
#   * host-or-ip  -- the address probes dial. A literal IP for a fixed box; a HOSTNAME for a box whose
#                    DHCP lease drifts (resolume.lan), so `getent`/ping/`/dev/tcp` resolve it live.
#   * class       -- windows-genlock | linux-genlock (the genlock-build platform; used by the
#                    version-integrity / rig-health rows to pick the right parity check).
#   * home-check  -- `always` (a permanent box: obs_fleet_is_home is unconditionally true),
#                    `traveling` (home ONLY when resolvable AND its OBS-WS answers; see below), or
#                    `retired` (issue 1316: the box is physically GONE but the ROLE returns on a new
#                    notebook next year -- obs_fleet_is_home is always FALSE and obs_fleet_boxes
#                    EXCLUDES it from every facet, so a paging facet can never page a dead box; the
#                    row + its history stay, and re-provisioning is a one-word flip back to `always`).
#
# FACET POLICY (`obs_fleet_boxes <facet>`): which boxes carry each dev1-side facet, decided honestly
# per each watchdog's own header, NOT per-entry -- so the entry row stays the fixed 4-field shape:
#   The production strih is strih-lx (issue 1317, M4 cut-over 20.9.2026 -- the Windows strih PC is
#   RETIRED and its row is gone, see the table below). strih-lx joins every facet whose premise is a
#   PLATFORM-NEUTRAL read (ping / OBS-WS :4455 / the box's own :8899 bundle-state JSON / dantesync
#   :8898), and stays OUT of the Windows-only ones:
#   audio-lag     = strih-lx stream   (the vendored OBS `audio-telemetry #800` lines, read off each box's
#                   own :8899 -- identical on Linux; resolume EXCLUDED: no program-audio chain on the CG box)
#   av-step       = stream            (the av-sync dock lives on the stream box only, #1267)
#   vb-matrix     = stream            (a Windows VB-Audio Matrix process check; strih-lx has NO VB-Matrix --
#                   PipeWire replaced it, issue 1344 -- and resolume has no install either)
#   bundle-state  = strih-lx stream resolume   (the :8899 server; the watchdog's auto-restart is
#                   CLASS-resolved: systemctl --user on a linux-genlock box, schtasks on windows-genlock)
#   network-reach = strih-lx stream resolume   (resolume report-only unless obs_fleet_is_home -- below)
#   obs-liveness  = strih-lx stream resolume   (OBS-WS GetStats render liveness)
#   genlock-lock  = strih-lx stream imag resolume  (#1299: the genlock LOCKED/DEGRADED/UNLOCKED
#                   facet is fleet-wide -- imag is a pure receiver that still locks every input to the
#                   fleet clock, so it IS in scope; resolume is paged only while obs_fleet_is_home)
#   render-freeze = strih-lx stream resolume   (#1320: a PROGRAM render-thread freeze
#                   (program_render_lagged) can strike ANY genlock OBS box, and a receiver relock
#                   storm (relock_bursts) any receiving box -- resolume runs the cg-obs PROGRAM
#                   render + receives, so it IS in scope. Traveling-safe with NO is_home gate: the
#                   only page condition is a SUCCESSFULLY-FETCHED positive reading, so a dark
#                   resolume just SKIPs -> #732/#1001, exactly like audio-lag/bundle-state.)
#   ndi-portmap   = strih-lx          (issue 1363: the ONE strih box whose NDI SENDER port map the
#                   dev1 port-map watchdog (scripts/ndi-portmap-audit.sh) watches -- the box that
#                   owns the 2ME PGM program the building TVs cache by port. EXACTLY one member: the
#                   audit refuses any other count. It consumes the member NAME only (the NDI machine
#                   name = the uppercased hostname, the anchor IP = the anchor's own mDNS record), so
#                   a strih swap (the Poprad strih-pp next) is this one policy edit.)
#   obs-session   = stream resolume   (issue 1317 part 2: the #979 obs64/AHK Windows SESSION-0
#                   visibility probe -- a PowerShell probe over win_ssh_run, meaningless on a Linux
#                   box, so windows-genlock members ONLY. The consumer ALSO class-gates each box, so
#                   even an override naming strih-lx never gets the PowerShell probe; resolume is
#                   probed only while home, via obs_fleet_poll_now.)
#   burn-reconcile = strih-lx stream  (issue 1317 part 2: the #1060 fresh-OBS-start burn reconcile
#                   -- OBS-WS GetStats renderTotalFrames + obs_burn_filter sweeps, platform-neutral.
#                   The unattended-start premise holds on strih-lx too: a boot autostart or a
#                   strih-obs.service restart reloads a saved burn exactly like the Windows AHK
#                   respawn did. resolume stays OUT: its cg-OBS burn node is the opt-in CG_CHAIN
#                   profile, never a leaked measurement burn on a camera program.)
#   rig-restore   = strih-lx stream   (issue 1317 part 2: the #281 stranded-rig restore -- the
#                   recording-e2e harness's two OBS program boxes, read + torn down over OBS-WS via
#                   obs_phase2.py; platform-neutral.)
#   ndi-sender    = strih-lx stream resolume   (issue 1342: the managed OBS boxes that PUBLISH NDI
#                   outputs -- STRIH-LX (...), the stream outputs, RESOLUME-SNV (cg-obs) + Arena. Every
#                   managed receiver lists them in ndi-config.v1.json networks.ips (the camboxes come
#                   from camera-set.sh), generated by scripts/lib/ndi-discovery.sh; the traveling
#                   resolume hostname is resolved at write time, skipped when away. Not a watchdog
#                   facet: nothing polls or pages from it.)
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
# home-check PORT (#1296): the ticket's example home-check was "resolves + :8898/status answers".
# RESOLUME-SNV DOES run dantesync (1.8.54, :8898 answers whenever the box is up -- supervisor read-back
# 2026-09-12; the older harness_network_reach_watchdog_811.rs "no dantesync" premise predates the
# issue-811 deploy), but :8898 alone would read "home" with the cg OBS DOWN -- and every watchdog that
# consults this gate cares about the OBS being up. So the home+serving signal is the genlock cg-obs's
# OBS-WebSocket port :4455 (live when home, #1295), overridable via OBS_FLEET_HOME_PORT. CAVEAT: resolume.lan currently resolves to
# 10.77.9.201, the SAME IP `bridge` lists in targets.md (event-LAN DHCP collision) -- if `bridge`
# answers :4455 at .201 while resolume is off, is_home may read a FALSE home, but network-reach then
# also classifies that address REACHABLE so it still never pages; always confirm box IDENTITY
# (getent hosts resolume.lan + its OBS profile, rig-state-inspection.md §2) before a supervisor flips
# resolume to a permanent paging fixture.
#
# COLLISION RESIDUAL per CONSUMER (#1296 review): the above collision-safety argument holds for
# NETWORK-REACH only, because its is_home promotion AND its page condition are BOTH keyed on .201
# liveness (they cannot contradict). OBS-LIVENESS is different: its promotion signal (is_home =
# :4455 answers) DIFFERS from its page condition (render wedged), so on the .201 collision it could
# page a render-wedged non-resolume OBS as "resolume". That residual is NARROW + supervisor-gated
# (obs-liveness ships DISABLED; identity is confirmed before enabling) and is documented at the
# resolume branch in obs-liveness-watchdog.sh + .claude/rules/obs-fleet-list.md.

# OBS_FLEET -- the table (env-overridable as a whole for tests/ops). One row per line,
# `name|host-or-ip|class|home-check`. Blank lines are ignored by the parser below.
# issue 1317 (M4 cut-over 20.9.2026): strih-lx IS the production strih -- the Linux notebook that
# took the strih cutter/mix role -- at its real address 10.77.9.202 and permanently home (`always`).
# It dials the IP, never `strih-lx.lan`: that name has no DNS entry on dev1, and the old `traveling`
# row therefore read the production strih as AWAY, leaving every dev1 watchdog blind to it.
# The Windows strih PC (`strih|10.77.9.202|windows-genlock|always`) is RETIRED and its row REMOVED,
# not flipped to `retired`: its address now belongs to strih-lx, so keeping the row would hand a
# Linux box to any Windows-class caller naming `strih` (`obs_fleet_host strih` now fails closed like
# any unknown name). Its history lives in targets.md. strih-lx keeps its own NAME (state files and
# alert dedup keys are name-keyed, so the new machine never inherits the Windows box's state).
# imag is `retired` (issue 1316): imag-nb was RETURNED to the owner 16.9.2026 (10.77.9.182 dark),
# so its home-check is `retired` -- obs_fleet_is_home imag is FALSE and obs_fleet_boxes drops it
# from genlock-lock (and any future facet) automatically. The row + its history are KEPT because the
# IMAG role returns on a NEW notebook next year; re-provisioning re-flips this ONE word to `always`.
OBS_FLEET="${OBS_FLEET:-strih-lx|10.77.9.202|linux-genlock|always
stream|10.77.9.204|windows-genlock|always
imag|10.77.9.182|linux-genlock|retired
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
    audio-lag)     printf 'strih-lx stream' ;;
    av-step)       printf 'stream' ;;
    vb-matrix)     printf 'stream' ;;
    bundle-state)  printf 'strih-lx stream resolume' ;;
    network-reach) printf 'strih-lx stream resolume' ;;
    obs-liveness)  printf 'strih-lx stream resolume' ;;
    genlock-lock)  printf 'strih-lx stream imag resolume' ;;
    render-freeze) printf 'strih-lx stream resolume' ;;
    ndi-portmap)   printf 'strih-lx' ;;
    obs-session)   printf 'stream resolume' ;;
    burn-reconcile) printf 'strih-lx stream' ;;
    rig-restore)   printf 'strih-lx stream' ;;
    ndi-sender)    printf 'strih-lx stream resolume' ;;
    *)
      echo "obs-fleet: unknown facet '${facet}' (expected one of: audio-lag av-step vb-matrix bundle-state network-reach obs-liveness genlock-lock render-freeze ndi-portmap obs-session burn-reconcile rig-restore ndi-sender)" >&2
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
  local facet="${1:-}" members m host check out=""
  members="$(obs_fleet_facet_members "$facet")" || return 1
  for m in $members; do
    host="$(obs_fleet_host "$m")" || {
      echo "obs-fleet: facet '${facet}' names box '${m}' absent from OBS_FLEET" >&2
      return 1
    }
    # issue 1316: a `retired` box (imag-nb, returned to the owner) is EXCLUDED from every facet
    # roster centrally here -- so genlock-lock (and any future derived facet) drops it with no
    # per-consumer edit, and re-adding it next year is a one-word flip of its home-check to `always`.
    check="$(obs_fleet_home_check "$m")" || check=""
    [ "$check" = "retired" ] && continue
    out="${out:+$out }${m}|${host}"
  done
  printf '%s' "$out"
}

# -- is_home probes (the ONLY I/O in this lib; kept as overridable seams for Tier-0 testing) --------
# obs_fleet_resolve_host <host> -> the FIRST resolved address on stdout (empty if unresolvable).
# `getent ahosts` (no early `exit` in awk -> no SIGPIPE) covers a hostname AND a literal IP (which
# resolves to itself). Override in a test to stub resolution. `|| true` keeps it drain-safe.
obs_fleet_resolve_host() {
  # issue 1317 part 4 review: time-bounded -- strih_platform / the class gate now resolve an unknown
  # NAME through this seam, so a stalled DNS resolver must never stall every caller.
  timeout "${OBS_FLEET_RESOLVE_TIMEOUT:-2}" getent ahosts "${1:-}" 2>/dev/null | awk 'NR==1{print $1}' || true
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

# obs_fleet_has_ahk <name> -> 1 when NAME's OBS is guarded by an NL_STARTUP.ahk AutoHotkey
# auto-respawn watcher (whose session a Windows probe must also check and a deploy must stop +
# restart), else 0. A pure FACT keyed on the name (issue 1317 review): resolume runs the AHK v2
# safe-loop (issue 1295). The retired Windows strih ran one too; its planner arms were retired in
# issue 1317 part 3, so no name maps to it any more. The ONE source the obs-session watchdog and
# every Windows planner (deploy-genlock-fleet.sh, launch-obs-genlock.sh, obs-self-heal-install.sh,
# via scripts/lib/genlock-fleet-boxes.sh) read.
obs_fleet_has_ahk() {
  case "${1:-}" in
    resolume) printf '1' ;;
    *) printf '0' ;;
  esac
}

# _obs_fleet_is_ipv4 <s> -> 0 iff S is a dotted-quad IPv4 literal (4 all-digit fields). Pure.
_obs_fleet_is_ipv4() {
  [[ "${1:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

# _obs_fleet_row_match <mode> <key> -> the NAME of the first OBS_FLEET row matching KEY (stdout),
# return 1 when none does. mode `exact`: KEY equals the row's name or host field (case-folded, so
# `RESOLUME.lan` == `resolume.lan`); mode `name`: KEY equals the row's NAME; mode `host`: KEY equals
# the row's HOST field. Reads the WHOLE here-doc (no early exit -> SIGPIPE-safe).
_obs_fleet_row_match() {
  local mode="${1:-}" key="${2:-}" line name rest host found=""
  [ -n "$key" ] || return 1
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    name="${line%%|*}"; rest="${line#*|}"; host="${rest%%|*}"
    [ -z "$found" ] || continue
    case "$mode" in
      exact) { [ "${name,,}" = "${key,,}" ] || [ "${host,,}" = "${key,,}" ]; } && found="$name" ;;
      name) [ "${name,,}" = "${key,,}" ] && found="$name" ;;
      host) [ "$host" = "$key" ] && found="$name" ;;
    esac
  done <<EOF
${OBS_FLEET}
EOF
  [ -n "$found" ] || return 1
  printf '%s' "$found"
}

# obs_fleet_name_for_host <host-or-ip> -> the fleet NAME that HOST addresses (stdout), or return 1
# when no row does. The ONE alias-aware "which managed OBS box is this address" lookup (issue 1317
# part 4) -- the authority both obs_fleet_class_for_host (the Windows-tool class gate) and
# scripts/lib/strih-platform.sh `strih_platform` consult, so the two can never disagree. In order:
#   1. exact: the row's name or host field, case-folded (`10.77.9.202`, `strih-lx`, `STRIH-LX`).
#   2. short name: a DNS-style name's first label vs the row NAME (`strih-lx.lan` -> `strih-lx`);
#      never applied to an IPv4 literal.
#   3. resolved: a non-IP name is resolved through the obs_fleet_resolve_host seam and its address
#      compared with the row HOST fields -- this is what catches the retired Windows PC's own name
#      `strih.lan`, which the rig DNS still points at 10.77.9.202 (the Linux strih-lx). An IPv4
#      literal is NEVER sent to the resolver.
# An unknown name that does not resolve to a fleet address returns 1 (callers treat it as an
# explicit, authoritative ops target -- the pre-existing contract).
obs_fleet_name_for_host() {
  local want="${1:-}" ip
  [ -n "$want" ] || return 1
  _obs_fleet_row_match exact "$want" && return 0
  _obs_fleet_is_ipv4 "$want" && return 1
  case "$want" in
    *.*) _obs_fleet_row_match name "${want%%.*}" && return 0 ;;
  esac
  ip="$(obs_fleet_resolve_host "$want")"
  [ -n "$ip" ] || return 1
  _obs_fleet_row_match host "$ip"
}

# obs_fleet_class_for_host <host-or-ip> -> the CLASS (windows-genlock | linux-genlock) of the fleet
# row HOST addresses (stdout), or return 1 when no row matches. The class gate a Windows-only planner
# consults BEFORE emitting a Windows action against an address (issue 1317 part 3): 10.77.9.202 is
# the Linux strih-lx now, so a PowerShell/schtasks/C:\ tool aimed at it must refuse. Alias-aware via
# obs_fleet_name_for_host (issue 1317 part 4), so `strih.lan` / `strih-lx.lan` / `STRIH-LX` cannot
# slip a Windows action past the gate.
obs_fleet_class_for_host() {
  local name
  name="$(obs_fleet_name_for_host "${1:-}")" || return 1
  obs_fleet_class "$name"
}

# obs_fleet_refuse_linux_target <host-or-ip> <tool> -> returns 0 (and prints nothing) when HOST is
# NOT a linux-genlock fleet box; returns 1 with a named error on stderr when it IS -- the one-line
# class gate every Windows-only dev1 tool (a .ps1 driver, a schtasks install, a PowerShell planner)
# calls before touching HOST, so a Windows action can never be emitted for a Linux box (issue 1317
# part 3). An address the fleet list does not know passes (an explicit ops target is authoritative).
obs_fleet_refuse_linux_target() {
  local host="${1:-}" tool="${2:-this tool}" cls
  cls="$(obs_fleet_class_for_host "$host")" || return 0
  [ "$cls" = "linux-genlock" ] || return 0
  echo "ERROR: ${tool} is Windows-only, but ${host} is a linux-genlock fleet box (scripts/lib/obs-fleet.sh) -- refusing to emit a Windows action for a Linux box (issue 1317)" >&2
  return 1
}

# obs_fleet_poll_now <name> -> returns 0 when a per-box consumer loop should poll NAME this pass, 1
# when it must skip it (issue 1317 part 2 -- the traveling/retired gate the issue-1317 per-box
# consumers share: the obs-session, burn-reconcile and rig-restore watchdogs; obs-liveness and
# network-reach predate it and gate resolume with obs_fleet_is_home themselves). An `always` box is
# polled WITHOUT consulting obs_fleet_is_home, so the OBS_FLEET_HOME force-list (a traveling-box
# test seam) never drops a fixed box; a `traveling` box only while obs_fleet_is_home holds; a
# `retired` box never. A name with NO row (an ops `<X>_BOXES` override naming a box the table does
# not know yet) is polled as given -- the override is authoritative, exactly as it already bypasses
# the facet derivation.
obs_fleet_poll_now() {
  local name="${1:-}" check
  check="$(obs_fleet_home_check "$name")" || return 0
  case "$check" in
    always) return 0 ;;
    *) obs_fleet_is_home "$name" ;;
  esac
}

# obs_fleet_is_home <name> -> returns 0 iff NAME is currently "home" (reachable + serving), 1 if away
# or unknown. A `home-check=always` box is unconditionally home; a `retired` box (issue 1316) is
# NEVER home. A `traveling` box is home iff it
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
    # issue 1316: a retired box (imag-nb, returned to the owner) is NEVER home -- so no watchdog
    # that gates on obs_fleet_is_home ever probes or pages it. Re-flip to `always` on re-provision.
    retired) return 1 ;;
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
