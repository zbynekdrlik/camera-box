#!/usr/bin/env bash
# airuleset:script-ok read-only ops audit tool; deliberately `set -uo pipefail` (NOT -e) so one
# unreadable avahi line / missing baseline never aborts the whole audit mid-pass -- an error is
# reported and returned, never fatal (same survive-per-pass discipline as scripts/netcfg-audit.sh).
#
# scripts/ndi-portmap-audit.sh -- #1181: durable, READ-ONLY audit of the STRIH-SNV OBS instance's NDI
# SENDER port map. Captures a checked-in baseline of the OBS instance's healthy name->port map (from
# mDNS via `avahi-browse -rtp _ndi._tcp`) and REPORTS drift against it -- so a reshuffled sender port
# (which silently hands a stock NDI Studio Monitor / building TV the WRONG sender under a cached port)
# is caught LOUD instead of discovered mid-service. Sender-side complement to #1180's receiver-side
# by-URL identity verify. NEVER writes to any rig box; the only file it writes is the checked-in
# baseline JSON (on --capture).
#
# The port-reshuffle mechanism (evidence on #1180 + #1181): libndi assigns sender TCP ports
# sequentially from 5961 in CREATION ORDER inside one OBS process; DistroAV defers the main/preview
# outputs to OBS_FRONTEND_EVENT_FINISHED_LOADING (after the per-source ndi_filter republishes are made
# at scene-collection load), so the map is deterministic across CLEAN restarts but reshuffles when an
# output was added/removed live (the saved-state creation order then differs from the running order).
#
# The strih box advertises TWO NDI machine instances at the same IP: the OBS instance (2ME PGM/PVW/
# Grading/MULTIVIEW/interkom) and a SEPARATE Arena/CG-bridge Spout ("Arena - bible"). This tool
# isolates the OBS instance by the mDNS-hostname GROUP that contains the anchor program sender
# (NDI_PORTMAP_ANCHOR, default "STRIH-SNV (2ME PGM)") and ignores everything else -- the CG source's
# port is a different process's independent assignment that never participates in the OBS reshuffle.
#
# Modes:
#   scripts/ndi-portmap-audit.sh --check      # DEFAULT: read live map -> diff vs baseline -> report;
#                                             #   exit 0 = STABLE, 3 = CHANGED, 2 = gather/usage error
#   scripts/ndi-portmap-audit.sh --capture    # read live map -> (over)write scripts/ndi-portmap-baseline.json
#   scripts/ndi-portmap-audit.sh --json       # read live map -> print the OBS instance map as JSON
#   scripts/ndi-portmap-audit.sh --help
#
# Offline testing: set NDI_PORTMAP_AVAHI_FIXTURE=<file> to read captured avahi -p output from a file
# instead of running avahi-browse (the whole tool is then rig-free + Tier-0 testable).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/ndi-portmap-health.sh
. "$HERE/lib/ndi-portmap-health.sh"

MODE="check"
case "${1:-}" in
  --check | "") MODE="check" ;;
  --capture) MODE="capture" ;;
  --json) MODE="json" ;;
  --help | -h) sed -n '4,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) echo "ndi-portmap-audit: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- scope (all env-overridable) -------------------------------------------------------------------
NDI_PORTMAP_BOX="${NDI_PORTMAP_BOX:-STRIH-SNV}"
NDI_PORTMAP_BOX_IP="${NDI_PORTMAP_BOX_IP:-10.77.9.202}"
NDI_PORTMAP_NAME_PREFIX="${NDI_PORTMAP_NAME_PREFIX:-STRIH-SNV}"
NDI_PORTMAP_ANCHOR="${NDI_PORTMAP_ANCHOR:-STRIH-SNV (2ME PGM)}"
NDI_PORTMAP_BASELINE="${NDI_PORTMAP_BASELINE:-$HERE/ndi-portmap-baseline.json}"
NDI_PORTMAP_AVAHI_CMD="${NDI_PORTMAP_AVAHI_CMD:-avahi-browse -rtp _ndi._tcp}"
NDI_PORTMAP_AVAHI_FIXTURE="${NDI_PORTMAP_AVAHI_FIXTURE:-}"
NDI_PORTMAP_AVAHI_TIMEOUT="${NDI_PORTMAP_AVAHI_TIMEOUT:-15}"

log() { printf '%s [ndi-portmap-audit] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# require_tools -> non-zero (loud) if a REQUIRED external tool is missing on dev1. `awk`/`python3` are
# always needed (the pure lib parses with awk; the baseline JSON is built/read with python3).
# `avahi-browse`/`timeout` are needed only for the LIVE read -- a NDI_PORTMAP_AVAHI_FIXTURE (tests)
# needs neither. A missing tool must fail LOUD by name, never read as an empty map (a permanently-blind
# watchdog) -- .claude/rules/imag-ssh-remote-tool-preflight.md (#833).
require_tools() {
  local missing=() t need=("awk" "python3")
  if [ -z "$NDI_PORTMAP_AVAHI_FIXTURE" ]; then
    need+=("timeout")
    command -v "${NDI_PORTMAP_AVAHI_CMD%% *}" >/dev/null 2>&1 || missing+=("${NDI_PORTMAP_AVAHI_CMD%% *}")
  fi
  for t in "${need[@]}"; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing tool would read the port map as empty forever, a permanently-blind watchdog)"
    return 1
  fi
  return 0
}

# -- read raw avahi -p lines (dev1-local I/O; NOT pure -- kept out of the lib) ----------------------
_np_avahi_read() {
  if [ -n "$NDI_PORTMAP_AVAHI_FIXTURE" ]; then
    [ -f "$NDI_PORTMAP_AVAHI_FIXTURE" ] || { log "avahi fixture not found: $NDI_PORTMAP_AVAHI_FIXTURE"; return 1; }
    cat "$NDI_PORTMAP_AVAHI_FIXTURE"
    return 0
  fi
  # avahi-browse -t terminates after dumping the resolved cache; `timeout` is the whole-command backstop.
  timeout "$NDI_PORTMAP_AVAHI_TIMEOUT" $NDI_PORTMAP_AVAHI_CMD 2>/dev/null
}

# -- build the OBS instance's live name=port map (parse every resolved line -> select the group) -----
_np_live_map() {
  local raw line parsed block=""
  raw="$(_np_avahi_read)" || return 1
  while IFS= read -r line; do
    parsed="$(ndi_avahi_parse_resolved "$line")"
    [ -n "$parsed" ] && block="${block}${parsed}"$'\n'
  done <<<"$raw"
  ndi_portmap_select "$block" "$NDI_PORTMAP_BOX_IP" "$NDI_PORTMAP_NAME_PREFIX" "$NDI_PORTMAP_ANCHOR"
}

# _np_port_of <name> <map-block> -> the port for an EXACT name (split on the LAST '=', so a name that
#   itself contains '=' is still handled), or empty.
_np_port_of() {
  local want="$1" block="$2" line nm pt
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    nm="${line%=*}"; pt="${line##*=}"
    [ "$nm" = "$want" ] && { printf '%s' "$pt"; return 0; }
  done <<<"$block"
  printf ''
}

# _np_baseline_senders_flat -> the baseline "senders" object as "name=port" lines (for comparison).
_np_baseline_senders_flat() {
  [ -f "$NDI_PORTMAP_BASELINE" ] || return 1
  python3 - "$NDI_PORTMAP_BASELINE" <<'PY'
import sys, json
try:
    data = json.load(open(sys.argv[1]))
except Exception as e:
    sys.stderr.write("ndi-portmap-audit: cannot read baseline: %s\n" % e); sys.exit(1)
for name, port in (data.get("senders", {}) or {}).items():
    print("%s=%s" % (name, port))
PY
}

# _np_write_baseline <live-map-block> -> build the baseline JSON atomically (a python failure must
#   never leave the committed baseline truncated -- build into a temp, then mv over it).
_np_write_baseline() {
  local live="$1" jtmp
  jtmp="$(mktemp "${NDI_PORTMAP_BASELINE}.XXXXXX")" || return 1
  # shellcheck disable=SC2064
  trap "rm -f '$jtmp'" RETURN
  NDI_PM_CAPTURED="$(date '+%Y-%m-%d')" \
  NDI_PM_BOX="$NDI_PORTMAP_BOX" NDI_PM_IP="$NDI_PORTMAP_BOX_IP" \
  NDI_PM_PREFIX="$NDI_PORTMAP_NAME_PREFIX" NDI_PM_ANCHOR="$NDI_PORTMAP_ANCHOR" \
  python3 - "$live" > "$jtmp" <<'PY'
import sys, os, json
senders = {}
for line in sys.argv[1].splitlines():
    line = line.rstrip("\n")
    if not line or "=" not in line:
        continue
    name, _, port = line.rpartition("=")
    try:
        senders[name] = int(port)
    except ValueError:
        continue
out = {
    "_comment": ("#1181 STRIH-SNV OBS-instance NDI sender port-map baseline. REPORT-ONLY source of "
                 "truth: scripts/ndi-portmap-audit.sh --check diffs the live map vs this and reports a "
                 "moved sender port LOUD; it NEVER overwrites (a deliberate re-capture is recorded by "
                 "re-running --capture and committing the change in a PR, exactly like "
                 "scripts/netcfg-baseline.json / scripts/latency-pins-baseline.json). Seeded from a "
                 "live `avahi-browse -rtp _ndi._tcp` read (never hand-typed). A moved port silently "
                 "hands stock NDI Studio Monitor / the building TVs the WRONG sender under a cached "
                 "port. See .claude/rules/distroav-receiver-lifecycle.md."),
    "_captured": os.environ["NDI_PM_CAPTURED"],
    "box": os.environ["NDI_PM_BOX"],
    "ip": os.environ["NDI_PM_IP"],
    "name_prefix": os.environ["NDI_PM_PREFIX"],
    "anchor": os.environ["NDI_PM_ANCHOR"],
    "senders": senders,
}
if not senders:
    sys.stderr.write("ndi-portmap-audit: refusing to write an EMPTY baseline\n"); sys.exit(1)
print(json.dumps(out, indent=2, sort_keys=True))
PY
  [ -s "$jtmp" ] || return 1
  mv -f "$jtmp" "$NDI_PORTMAP_BASELINE" || return 1
  return 0
}

# _np_json <live-map-block> -> print the OBS instance map as JSON (no baseline; --json mode).
_np_json() {
  python3 - "$1" "$NDI_PORTMAP_BOX" "$NDI_PORTMAP_BOX_IP" "$NDI_PORTMAP_ANCHOR" <<'PY'
import sys, json
senders = {}
for line in sys.argv[1].splitlines():
    if not line or "=" not in line:
        continue
    name, _, port = line.rpartition("=")
    try:
        senders[name] = int(port)
    except ValueError:
        continue
print(json.dumps({"box": sys.argv[2], "ip": sys.argv[3], "anchor": sys.argv[4],
                  "senders": senders}, indent=2, sort_keys=True))
PY
}

# ============================ modes ============================
require_tools || exit 2

case "$MODE" in
  capture)
    log "capturing live OBS-instance port map -> $NDI_PORTMAP_BASELINE"
    live="$(_np_live_map)"
    if [ -z "$live" ]; then
      log "capture found NO OBS-instance senders (is strih OBS up + avahi reachable? anchor='$NDI_PORTMAP_ANCHOR')"
      exit 2
    fi
    if _np_write_baseline "$live"; then
      log "wrote baseline for $(printf '%s\n' "$live" | grep -c '=') sender(s) to $NDI_PORTMAP_BASELINE"
    else
      log "capture failed (json build / write)"; exit 2
    fi
    ;;

  json)
    live="$(_np_live_map)"
    _np_json "$live"
    ;;

  check)
    log "checking live OBS-instance port map vs baseline $NDI_PORTMAP_BASELINE"
    base="$(_np_baseline_senders_flat)" || { log "no readable baseline at $NDI_PORTMAP_BASELINE -- run --capture first"; exit 2; }
    if [ -z "$base" ]; then log "baseline has no senders -- run --capture first"; exit 2; fi
    live="$(_np_live_map)"
    if [ -z "$live" ]; then
      # An empty live map is a GATHER ERROR (OBS down / avahi unreachable / anchor renamed), NOT a port
      # change -- box reachability is #1001's job. Never page CHANGED off an unreadable map.
      log "no live OBS-instance senders found (OBS down / avahi unreachable / anchor='$NDI_PORTMAP_ANCHOR' absent) -- nothing to diff"
      exit 2
    fi

    statuses=()
    report=()
    moved=()
    # Iterate the baseline names: classify each against the live port.
    while IFS= read -r bline; do
      [ -n "$bline" ] || continue
      bname="${bline%=*}"; bport="${bline##*=}"
      lport="$(_np_port_of "$bname" "$live")"
      v="$(ndi_portmap_classify_port "$lport" "$bport")"
      statuses+=("$v")
      case "$v" in
        MOVED)  report+=("  [MOVED] $bname: baseline :$bport -> live :$lport"); moved+=("$bname :$bport->:$lport") ;;
        ABSENT) report+=("  [report-only ABSENT] $bname: baseline :$bport, not currently advertised (OBS reload / output removed?)") ;;
      esac
    done <<<"$base"

    # Surface NEW senders in the OBS instance not in the baseline (report-only: an added output
    # reshuffles only the NEXT restart, it does not move an existing sender's port right now).
    while IFS= read -r lline; do
      [ -n "$lline" ] || continue
      lname="${lline%=*}"; lport="${lline##*=}"
      if [ -z "$(_np_port_of "$lname" "$base")" ]; then
        report+=("  [report-only NEW] $lname: :$lport advertised, not in baseline (a new output reshuffles the NEXT restart)")
      fi
    done <<<"$live"

    overall="$(ndi_portmap_verdict "${statuses[@]}")"
    if [ "$overall" = "CHANGED" ]; then
      log "CHANGED detected:"
      printf '%s\n' "${report[@]}" >&2
      echo "NDI-PORTMAP-CHANGED: ${#moved[@]} sender(s) moved: $(IFS='; '; echo "${moved[*]}")"
      exit 3
    fi
    [ "${#report[@]}" -gt 0 ] && printf '%s\n' "${report[@]}" >&2
    echo "NDI-PORTMAP-STABLE: OBS instance port map matches baseline ($(printf '%s\n' "$base" | grep -c '=') senders)"
    exit 0
    ;;
esac
