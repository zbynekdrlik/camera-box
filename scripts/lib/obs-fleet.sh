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
#   * name        -- the stable box name the watchdogs log + key state on: `<role>-<venue>` since issue
#                    1414 (strih-snv / stream-snv / resolume-snv / strih-pp / stream-pp; imag).
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
#   The production strih is strih-snv (issue 1317, M4 cut-over 20.9.2026 -- the Windows strih PC is
#   RETIRED and its row is gone, see the table below). strih-snv joins every facet whose premise is a
#   PLATFORM-NEUTRAL read (ping / OBS-WS :4455 / the box's own :8899 bundle-state JSON / dantesync
#   :8898), and stays OUT of the Windows-only ones:
#   audio-lag     = strih-snv stream   (the vendored OBS `audio-telemetry #800` lines, read off each box's
#                   own :8899 -- identical on Linux; resolume EXCLUDED: no program-audio chain on the CG box)
#   av-step       = stream            (the av-sync dock lives on the stream box only, #1267)
#   vb-matrix     = stream            (a Windows VB-Audio Matrix process check; strih-snv has NO VB-Matrix --
#                   PipeWire replaced it, issue 1344 -- and resolume has no install either)
#   bundle-state  = strih-snv stream resolume   (the :8899 server; the watchdog's auto-restart is
#                   CLASS-resolved: systemctl --user on a linux-genlock box, schtasks on windows-genlock)
#   network-reach = strih-snv stream resolume   (resolume report-only unless obs_fleet_is_home -- below)
#   obs-liveness  = strih-snv stream resolume   (OBS-WS GetStats render liveness)
#   genlock-lock  = strih-snv stream imag resolume  (#1299: the genlock LOCKED/DEGRADED/UNLOCKED
#                   facet is fleet-wide -- imag is a pure receiver that still locks every input to the
#                   fleet clock, so it IS in scope; resolume is paged only while obs_fleet_is_home)
#   render-freeze = strih-snv stream resolume   (#1320: a PROGRAM render-thread freeze
#                   (program_render_lagged) can strike ANY genlock OBS box, and a receiver relock
#                   storm (relock_bursts) any receiving box -- resolume runs the cg-obs PROGRAM
#                   render + receives, so it IS in scope. Traveling-safe with NO is_home gate: the
#                   only page condition is a SUCCESSFULLY-FETCHED positive reading, so a dark
#                   resolume just SKIPs -> #732/#1001, exactly like audio-lag/bundle-state.)
#   ndi-portmap   = strih-snv          (issue 1363: the ONE strih box whose NDI SENDER port map the
#                   dev1 port-map watchdog (scripts/ndi-portmap-audit.sh) watches -- the box that
#                   owns the 2ME PGM program the building TVs cache by port. EXACTLY one member: the
#                   audit refuses any other count. It consumes the member NAME only (the NDI machine
#                   name = the uppercased hostname, the anchor IP = the anchor's own mDNS record), so
#                   a strih swap (the Poprad strih-pp next) is this one policy edit.)
#   obs-session   = strih-snv stream resolume   (the #979 operator-visibility watchdog between E2E
#                   runs. The consumer picks the probe by CLASS: a windows-genlock box gets the obs64/AHK
#                   SESSION-0 PowerShell probe over win_ssh_run, a linux-genlock box (issue 1474) the
#                   recording-e2e [0/8] Linux check -- strih-obs.service over plain ssh + a bounded
#                   obs-websocket round-trip -- and never PowerShell; any other class is skipped.
#                   resolume is probed only while home, via obs_fleet_poll_now. The PP boxes stay
#                   out, like obs-liveness / network-reach.)
#   burn-reconcile = strih-snv stream  (issue 1317 part 2: the #1060 fresh-OBS-start burn reconcile
#                   -- OBS-WS GetStats renderTotalFrames + obs_burn_filter sweeps, platform-neutral.
#                   The unattended-start premise holds on strih-snv too: a boot autostart or a
#                   strih-obs.service restart reloads a saved burn exactly like the Windows AHK
#                   respawn did. resolume stays OUT: its cg-OBS burn node is the opt-in CG_CHAIN
#                   profile, never a leaked measurement burn on a camera program.)
#   rig-restore   = strih-snv stream   (issue 1317 part 2: the #281 stranded-rig restore -- the
#                   recording-e2e harness's two OBS program boxes, read + torn down over OBS-WS via
#                   obs_phase2.py; platform-neutral.)
#   ndi-sender    = strih-snv stream resolume   (issue 1342: the managed OBS boxes that PUBLISH NDI
#                   outputs -- STRIH-SNV (...), the stream outputs, RESOLUME-SNV (cg-obs) + Arena. Every
#                   OBS-box receiver lists them -- and ONLY them, never a cambox (issue 1389; the
#                   camboxes carry no list at all) -- in ndi-config.v1.json networks.ips, generated by
#                   scripts/lib/ndi-discovery.sh; the traveling resolume hostname is resolved at write
#                   time, skipped when away. Not a watchdog facet: nothing polls or pages from it.)
#   audio-mixer   = strih-snv stream resolume   (issue 1381: every managed OBS box runs a libobs
#                   audio mixer whose `audio-stall` dump says whether it keeps real time; resolume
#                   also carries the obs-vban FOH/lv1 senders whose pacer loss the same watchdog
#                   pages. The consumer polls resolume only while home, via obs_fleet_poll_now.)
#   win-baseline  = the windows-genlock rows of `stream resolume`   (issue 1357: the Windows OBS boxes
#                   graded against the ONE Windows baseline -- power plan, sleep, USB suspend, WER -- by
#                   scripts/win-baseline-check.sh, report-only; resolume SKIPPED when away. Issue
#                   1474: a Windows baseline means nothing on a Linux box, so the facet takes only the
#                   rows whose class is windows-genlock: resolume alone since stream-snv went Linux
#                   (issue 1462), both again on a fleet whose stream box is Windows.)
#   obs-handles   = strih-snv stream resolume   (issue 1406: every managed OBS process can leak
#                   handles -- the 5.10.2026 stream obs64 held 4M after ~22 h -- and the :8899
#                   `obs_handles*` facet reads its count on Windows AND Linux (strih-snv: /proc fds).
#                   The consumer polls resolume only while home, via obs_fleet_poll_now.)
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
# issue 1317 (M4 cut-over 20.9.2026): strih-snv IS the production strih -- the Linux notebook that
# took the strih cutter/mix role -- at its real address 10.77.9.202 and permanently home (`always`).
# It dials the IP (a fixed box; the old `traveling` + `.lan` row read the production strih as AWAY,
# leaving every dev1 watchdog blind to it). issue 1414: the SNV rows carry the venue -- strih-snv,
# stream-snv, resolume-snv (OBS_FLEET_ALIASES below keeps the old names resolving).
# The Windows strih PC (`strih|10.77.9.202|windows-genlock|always`) is RETIRED and its row REMOVED,
# not flipped to `retired`: its address now belongs to strih-snv, so keeping the row would hand a
# Linux box to any Windows-class caller naming `strih` (`obs_fleet_host strih` now fails closed like
# any unknown name). Its history lives in targets.md. strih-snv keeps its own NAME (state files and
# alert dedup keys are name-keyed, so the new machine never inherits the Windows box's state).
# imag is `retired` (issue 1316): imag-nb was RETURNED to the owner 16.9.2026 (10.77.9.182 dark),
# so its home-check is `retired` -- obs_fleet_is_home imag is FALSE and obs_fleet_boxes drops it
# from genlock-lock (and any future facet) automatically. The row + its history are KEPT because the
# IMAG role returns on a NEW notebook next year; re-provisioning re-flips this ONE word to `always`.
# issue 1412: the two Poprad boxes, reinstalled as Linux OBS boxes on 11.10.2026 -- strih-pp (the PP
# strih, scripts/strih-boxes/strih-pp.env) and stream-pp (the PP stream box). They dial their NAMES:
# dev1 reaches the PP LAN as 10.76.8.x through the companion-pp subnet router while the boxes carry
# 10.77.8.x, so an IP row would never match the box's own STRIH_IP. `traveling` (polled only while
# their OBS-WS answers).
# issue 1426: resolume-pp (the PP CG PC, Windows, its own cg OBS) joins as a traveling NAME row, and
# the three PP boxes are members of the `build-parity` facet (the fleet version-parity pass).
# issue 1441 (owner 15.10.2026, one solution for PP and SNV): strih-pp + stream-pp also join the four
# content watchdogs whose :8899 facets they serve -- genlock-lock, render-freeze, audio-mixer,
# obs-handles -- each polled/paged only while home (obs_fleet_is_home / obs_fleet_poll_now).
OBS_FLEET="${OBS_FLEET:-strih-snv|10.77.9.202|linux-genlock|always
stream-snv|10.77.9.204|linux-genlock|always
imag|10.77.9.182|linux-genlock|retired
resolume-snv|resolume.lan|windows-genlock|traveling
strih-pp|strih-pp.lan|linux-genlock|traveling
stream-pp|stream-pp.lan|linux-genlock|traveling
resolume-pp|resolume-pp.lan|windows-genlock|traveling}"

# OBS_FLEET_ALIASES -- issue 1414 (box names carry the venue): the names the SNV rows had before the
# rename, as space-separated OLD=NEW pairs. The ONE alias table of the repo (the python twin
# scripts/obs_fleet_table.py FLEET_ALIASES is pinned equal; the strih box-facts loader reads this one):
# every row lookup (_obs_fleet_entry, so obs_fleet_host & co), obs_fleet_canonical_name and
# obs_fleet_name_for_host map an old name to the renamed row, so a caller, a config or a ROLE token
# (`--box stream`) that still says strih-lx / stream / resolume (issue 1414) keeps addressing it. An
# exact row always wins, and an alias whose renamed row is not in the table is no alias. The retired
# Windows `strih` is NOT an alias: its name stays unknown (its old DNS name strih.lan resolves to the
# strih-snv address, step 3 of the lookup).
OBS_FLEET_ALIASES="strih-lx=strih-snv stream=stream-snv resolume=resolume-snv"   # issue 1414 transition aliases

# _obs_fleet_alias_of <name> -> the renamed row NAME is a transition alias of (case-folded), rc 0;
# rc 1 when NAME is no alias or its renamed row is not in the table. Pure.
_obs_fleet_alias_of() {
  local want="${1:-}" pair IFS=$' \t\n'   # a caller may run with IFS=',' (the deploy planner's CSV loop)
  [ -n "$want" ] || return 1
  for pair in $OBS_FLEET_ALIASES; do
    [ "${pair%%=*}" = "${want,,}" ] || continue
    _obs_fleet_row_exact "${pair#*=}" >/dev/null || return 1
    printf '%s' "${pair#*=}"
    return 0
  done
  return 1
}

# obs_fleet_canonical_name <name> -> the fleet row NAME addresses (stdout, always rc 0): the name as
# given when a row has it, else the renamed row of a transition alias, else the lower-cased name when
# a row has that, else NAME as given. For a CLI that prints or compares a box name (the per-box
# genlock table, the deploy planner, the retention drivers' --box). Pure.
obs_fleet_canonical_name() {
  local n="${1:-}" a
  if _obs_fleet_row_exact "$n" >/dev/null; then printf '%s' "$n"; return 0; fi
  if a="$(_obs_fleet_alias_of "$n")"; then printf '%s' "$a"; return 0; fi
  if _obs_fleet_row_exact "${n,,}" >/dev/null; then printf '%s' "${n,,}"; return 0; fi
  printf '%s' "$n"
}

# _obs_fleet_entry <name> -> prints the whole `name|host|class|home-check` row for NAME on stdout and
# returns 0; returns 1 (no output) for an unknown name. An exact row wins; else a transition alias
# (issue 1414) addresses its renamed row, so every fact lookup takes an old name.
_obs_fleet_entry() {
  local alias
  _obs_fleet_row_exact "${1:-}" && return 0
  alias="$(_obs_fleet_alias_of "${1:-}")" || return 1
  _obs_fleet_row_exact "$alias"
}

# _obs_fleet_row_exact <name> -> the row whose NAME is exactly <name>, rc 0; rc 1 (no output) else.
# Word-exact on the leading `name|` so a hypothetical `stream2` can never match `stream`. The here-doc
# read consumes the WHOLE stream (no early `break`/`exit`), so it is SIGPIPE-safe under a caller's
# `set -euo pipefail`.
_obs_fleet_row_exact() {
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

# _obs_fleet_windows_rows NAMES -> the NAMES (space-separated, in order) whose fleet row class is
# windows-genlock; a name with no row is left out. issue 1474: a Windows-only facet's members.
_obs_fleet_windows_rows() {
  local m out=""
  for m in ${1:-}; do
    [ "$(obs_fleet_class "$m" 2>/dev/null || true)" = "windows-genlock" ] && out="${out:+$out }$m"
  done
  printf '%s' "$out"
}

# obs_fleet_facet_members <facet> -> the space-separated box NAMES carrying FACET (stdout), decided
# by the policy table documented in this file's header. Unknown facet -> error on stderr + return 1
# (fail-closed: a caller must never silently derive an empty roster from a typo'd facet).
obs_fleet_facet_members() {
  local facet="${1:-}"
  case "$facet" in
    audio-lag)     printf 'strih-snv stream-snv' ;;
    av-step)       printf 'stream-snv' ;;
    vb-matrix)     printf 'stream-snv' ;;
    bundle-state)  printf 'strih-snv stream-snv resolume-snv' ;;
    network-reach) printf 'strih-snv stream-snv resolume-snv' ;;
    obs-liveness)  printf 'strih-snv stream-snv resolume-snv' ;;
    genlock-lock)  printf 'strih-snv stream-snv imag resolume-snv strih-pp stream-pp' ;;   # issue 1441: + the PP Linux boxes
    render-freeze) printf 'strih-snv stream-snv resolume-snv strih-pp stream-pp' ;;
    ndi-portmap)   printf 'strih-snv' ;;
    obs-session)   printf 'strih-snv stream-snv resolume-snv' ;;   # issue 1474: the probe is picked by class
    burn-reconcile) printf 'strih-snv stream-snv' ;;
    rig-restore)   printf 'strih-snv stream-snv' ;;
    ndi-sender)    printf 'strih-snv stream-snv resolume-snv' ;;
    win-baseline)  _obs_fleet_windows_rows 'stream-snv resolume-snv' ;;   # issue 1357/1474: the Windows OBS-box baseline, Windows rows only
    audio-mixer)   printf 'strih-snv stream-snv resolume-snv strih-pp stream-pp' ;;   # issue 1381: mixer real time + VBAN pacer loss
    obs-handles)   printf 'strih-snv stream-snv resolume-snv strih-pp stream-pp' ;;   # issue 1406: the OBS process handle / fd count
    build-parity)  printf 'strih-snv stream-snv resolume-snv strih-pp stream-pp resolume-pp' ;;   # issue 1426: one OBS genlock build across both venues
    *)
      echo "obs-fleet: unknown facet '${facet}' (expected one of: audio-lag av-step vb-matrix bundle-state network-reach obs-liveness genlock-lock render-freeze ndi-portmap obs-session burn-reconcile rig-restore ndi-sender win-baseline audio-mixer obs-handles build-parity)" >&2
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

# obs_fleet_resolve_host_v4 <host> -> the FIRST resolved IPv4 address (empty if none). Same bound as
# obs_fleet_resolve_host; `ahostsv4` because `ahosts` may answer IPv6 first and some consumers (the
# NDI receiver list, issue 1342) accept IPv4 only.
obs_fleet_resolve_host_v4() {
  timeout "${OBS_FLEET_RESOLVE_TIMEOUT:-2}" getent ahostsv4 "${1:-}" 2>/dev/null | awk 'NR==1{print $1}' || true
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
  case "$(obs_fleet_canonical_name "${1:-}")" in   # issue 1414: an old name reads the same fact
    resolume-snv) printf '1' ;;
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
#   1. exact: the row's name or host field, case-folded (`10.77.9.202`, `strih-snv`, `STRIH-SNV`).
#   1b. an issue-1414 transition alias, case-folded (OBS_FLEET_ALIASES: the old name of a renamed row,
#      `stream` -> `stream-snv`), also as a DNS name's first label.
#   2. short name: a DNS-style name's first label vs the row NAME (`strih-snv.lan` -> `strih-snv`);
#      never applied to an IPv4 literal.
#   3. resolved: a non-IP name is resolved through the obs_fleet_resolve_host seam and its address
#      compared with the row HOST fields -- this is what catches the retired Windows PC's own name
#      `strih.lan`, which the rig DNS still points at 10.77.9.202 (the Linux strih-snv). An IPv4
#      literal is NEVER sent to the resolver.
# An unknown name that does not resolve to a fleet address returns 1 (callers treat it as an
# explicit, authoritative ops target -- the pre-existing contract).
obs_fleet_name_for_host() {
  local want="${1:-}" ip
  [ -n "$want" ] || return 1
  _obs_fleet_row_match exact "$want" && return 0
  _obs_fleet_is_ipv4 "$want" && return 1
  _obs_fleet_alias_of "$want" && return 0
  case "$want" in
    *.*) _obs_fleet_row_match name "${want%%.*}" && return 0
         _obs_fleet_alias_of "${want%%.*}" && return 0 ;;
  esac
  ip="$(obs_fleet_resolve_host "$want")"
  [ -n "$ip" ] || return 1
  _obs_fleet_row_match host "$ip"
}

# obs_fleet_class_for_host <host-or-ip> -> the CLASS (windows-genlock | linux-genlock) of the fleet
# row HOST addresses (stdout), or return 1 when no row matches. The class gate a Windows-only planner
# consults BEFORE emitting a Windows action against an address (issue 1317 part 3): 10.77.9.202 is
# the Linux strih-snv now, so a PowerShell/schtasks/C:\ tool aimed at it must refuse. Alias-aware via
# obs_fleet_name_for_host (issue 1317 part 4), so `strih.lan` / `strih-snv.lan` / `STRIH-SNV` cannot
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
#
# issue 1357 (follow-up 5866910703): a traveling box that is home is ALSO identity-checked before rc
# 0 -- the computer name that answers at its host must be the one OBS_FLEET_IDENTITY declares
# (resolume.lan resolves to 10.77.9.201, an address the `bridge` PC has also held). A different or
# unreadable name returns 2 (UNKNOWN: never poll, never copy) with OBS_FLEET_IDENTITY_REASON naming
# the machine that answered, plus one stderr line, so every reader is guarded; a reader that only
# tests `if ! obs_fleet_poll_now` skips it like an away box. rc: 0 poll / 1 away / 2 identity UNKNOWN.
obs_fleet_poll_now() {
  local name="${1:-}" check host
  OBS_FLEET_IDENTITY_REASON=""
  check="$(obs_fleet_home_check "$name")" || return 0
  case "$check" in
    always) return 0 ;;
  esac
  obs_fleet_is_home "$name" || return 1
  host="$(obs_fleet_host "$name")" || return 0
  obs_fleet_identity_check "$name" "$host" && return 0
  echo "obs-fleet: $name UNKNOWN -- $OBS_FLEET_IDENTITY_REASON at $host (issue 1357 identity check): not polled, nothing copied" >&2
  return 2
}

# OBS_FLEET_IDENTITY -- issue 1357: the expected COMPUTER NAME of each row whose host is a DHCP NAME,
# as space-separated `name=EXPECTED` pairs. The ONE declaration every reader of obs_fleet_poll_now
# checks against (never hand-typed per script). Compared case-insensitively (Windows `hostname`
# prints RESOLUME-SNV, a Linux box its lower-case name). The fixed-IP `always` rows carry none: their
# address is pinned, and obs_fleet_poll_now does no live I/O for them. resolume-pp is not declared
# until its computer name is read live (an undeclared row is polled as before).
OBS_FLEET_IDENTITY="${OBS_FLEET_IDENTITY-resolume-snv=RESOLUME-SNV strih-pp=strih-pp stream-pp=stream-pp resolume-pp=resolume-pp}"

# obs_fleet_expected_identity <name> -> the declared expected computer name of NAME (stdout, rc 0),
# or rc 1 when none is declared. A transition alias (issue 1414) finds its renamed row's name. Pure.
obs_fleet_expected_identity() {
  local want pair IFS=$' \t\n'
  want="$(obs_fleet_canonical_name "${1:-}")"
  [ -n "$want" ] || return 1
  for pair in $OBS_FLEET_IDENTITY; do
    [ "${pair%%=*}" = "$want" ] || continue
    [ -n "${pair#*=}" ] || return 1
    printf '%s' "${pair#*=}"
    return 0
  done
  return 1
}

# obs_fleet_identity_read <name> <host> -> the computer name that answers at HOST (stdout; empty when
# unreadable). Seam: OBS_FLEET_IDENTITY_CMD, invoked as `<cmd> <name> <host>`. Default: `hostname`
# over ssh with the fleet credential (OBS_FLEET_SSH_USER / OBS_FLEET_SSH_PASS, default the
# targets.md newlevel login every fleet reader uses), bounded by OBS_FLEET_IDENTITY_TIMEOUT (s,
# default 10). sshpass stays the OUTER command, so a test's sshpass function stub still intercepts it.
obs_fleet_identity_read() {
  local name="${1:-}" host="${2:-}" out
  if [ -n "${OBS_FLEET_IDENTITY_CMD:-}" ]; then
    out="$("$OBS_FLEET_IDENTITY_CMD" "$name" "$host" 2>/dev/null)" || out=""
  else
    out="$(sshpass -p "${OBS_FLEET_SSH_PASS:-newlevel}" timeout "${OBS_FLEET_IDENTITY_TIMEOUT:-10}" \
      ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
      -o ConnectTimeout=5 "${OBS_FLEET_SSH_USER:-newlevel}@${host}" hostname 2>/dev/null)" || out=""
  fi
  out="${out%%$'\n'*}"
  out="${out//$'\r'/}"
  out="${out//[[:space:]]/}"
  printf '%s' "$out"
}

# obs_fleet_identity_check <name> <host> -> rc 0 when NAME declares no expected computer name or the
# one answering at HOST matches it; rc 1 otherwise, with OBS_FLEET_IDENTITY_REASON set to
# `answered as <GOT>, expected <EXP>` or `identity unreadable (expected <EXP>)`. OBS_FLEET_HOME set
# without OBS_FLEET_IDENTITY_CMD = a forced test/ops state: authoritative, nothing is read.
obs_fleet_identity_check() {
  local name="${1:-}" host="${2:-}" want got
  OBS_FLEET_IDENTITY_REASON=""
  want="$(obs_fleet_expected_identity "$name")" || return 0
  if [ -n "${OBS_FLEET_HOME:-}" ] && [ -z "${OBS_FLEET_IDENTITY_CMD:-}" ]; then
    return 0
  fi
  got="$(obs_fleet_identity_read "$name" "$host")"
  if [ -z "$got" ]; then
    OBS_FLEET_IDENTITY_REASON="identity unreadable (expected $want)"
    return 1
  fi
  if [ "${got,,}" != "${want,,}" ]; then
    OBS_FLEET_IDENTITY_REASON="answered as $got, expected $want"
    return 1
  fi
  return 0
}

# obs_fleet_is_home <name> -> returns 0 iff NAME is currently "home" (reachable + serving), 1 if away
# or unknown. A `home-check=always` box is unconditionally home; a `retired` box (issue 1316) is
# NEVER home. A `traveling` box is home iff it
# resolves AND its OBS-WS (OBS_FLEET_HOME_PORT, default 4455) answers. TEST/OPS OVERRIDE: when
# OBS_FLEET_HOME is set (space-separated names) it is authoritative -- NAME is home iff it is a word
# in that list -- so a test (or a supervisor forcing a known state) never depends on live I/O. An
# unknown name returns 1 (fail-closed: an untracked box is never treated as home).
obs_fleet_is_home() {
  local name="${1:-}" check host ip w IFS=$' \t\n'
  if [ -n "${OBS_FLEET_HOME:-}" ]; then
    # issue 1414: a listed name and the asked name match through the transition aliases too.
    for w in ${OBS_FLEET_HOME}; do
      [ "$(obs_fleet_canonical_name "$w")" = "$(obs_fleet_canonical_name "$name")" ] && return 0
    done
    return 1
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
