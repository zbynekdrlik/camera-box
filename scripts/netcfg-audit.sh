#!/usr/bin/env bash
# airuleset:script-ok read-only ops audit tool; deliberately `set -uo pipefail` (NOT -e) so one
# unreachable switch never aborts the whole chain audit -- an unreachable switch is reported, not
# fatal (same survive-per-target discipline as scripts/network-reach-alert-watchdog.sh).
#
# scripts/netcfg-audit.sh -- #797: durable, READ-ONLY audit of the venue MikroTik switch chain.
# Captures a checked-in baseline of the chain's healthy config (per-port link rate + role,
# shared-buffers, ROS version) and REPORTS drift against it -- turning the 2026-07-17/18 burst-gap /
# 18:41-collapse investigation into a re-checkable baseline so the next event starts from a known-good
# reference instead of a hand-ssh scramble mid-incident.
#
# What it reads (per switch, all READ-ONLY -- NO config writes are ever issued from this tool):
#   - /system identity + /system resource            -> name, ROS version, board
#   - /interface ethernet switch qos settings print  -> shared-buffers (the KEPT #797 40->80% fix)
#   - a :foreach over /interface ethernet            -> per-port comment(role)/running/rate/duplex +
#                                                       cumulative tx-drop-queue1 / tx-drop / rx-fcs
#   - (--check only, per flagged port) print stats x2 -> a live drop-RATE probe (the microburst signal)
#
# Modes:
#   scripts/netcfg-audit.sh --check      # DEFAULT: gather live -> diff vs baseline -> drift report;
#                                        #   exit 0 = CLEAN, 3 = DRIFT, 2 = usage/gather error
#   scripts/netcfg-audit.sh --capture    # gather live -> (over)write scripts/netcfg-baseline.json
#   scripts/netcfg-audit.sh --json       # gather live -> print the raw snapshot JSON (no baseline)
#   scripts/netcfg-audit.sh --help
#
# Credential: the MikroTik `admin` password is read from $NETCFG_SWITCH_PW -- NEVER hardcoded here
# (the fleet pw is not committed). The systemd service loads it from an operator-owned EnvironmentFile
# on dev1; see .claude/rules/netcfg-audit.md.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/netcfg-audit.sh
. "$HERE/lib/netcfg-audit.sh"

MODE="check"
case "${1:-}" in
  --check | "") MODE="check" ;;
  --capture) MODE="capture" ;;
  --json) MODE="json" ;;
  --help | -h) sed -n '4,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) echo "netcfg-audit: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- inventory (name|ip); router first, then the 4 CRS310s. All env-overridable. -------------------
# The router is captured for identity/ROS only (no switch-port/qos section); the CRS310s get the full
# per-port + qos gather. router|... entries are marked with a trailing "|router" role tag.
NETCFG_ROUTER="${NETCFG_ROUTER:-router_snv|10.77.8.1}"
NETCFG_SWITCHES="${NETCFG_SWITCHES:-foh1_audio|10.77.9.2 stage_av|10.77.9.3 foh1_video|10.77.9.4 foh2_video|10.77.9.5}"
NETCFG_BASELINE="${NETCFG_BASELINE:-$HERE/netcfg-baseline.json}"
NETCFG_SWITCH_PW="${NETCFG_SWITCH_PW:-}"
NETCFG_SSH_TIMEOUT="${NETCFG_SSH_TIMEOUT:-8}"
# Drop-RATE probe (--check): a port whose cumulative tx-drop-queue1 is nonzero is re-probed with two
# reads WINDOW seconds apart; a rate above THRESHOLD drops/s pages (the microburst-tail-drop signature).
NETCFG_DROP_WINDOW="${NETCFG_DROP_WINDOW:-6}"
NETCFG_DROP_THRESHOLD="${NETCFG_DROP_THRESHOLD:-1}"

log() { printf '%s [netcfg-audit] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

if [ -z "$NETCFG_SWITCH_PW" ]; then
  log "NETCFG_SWITCH_PW is empty -- set it to the MikroTik admin password (never committed). Aborting."
  exit 2
fi

# -- read-only ssh to a switch (dev1-local I/O; NOT pure -- kept out of the lib) --------------------
# _nc_ssh <ip> <routeros-command> -> stdout of the command (stderr silenced); rc mirrors ssh.
_nc_ssh() {
  local ip="$1" cmd="$2"
  sshpass -p "$NETCFG_SWITCH_PW" ssh \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout="$NETCFG_SSH_TIMEOUT" -o PreferredAuthentications=password \
    "admin@$ip" "$cmd" 2>/dev/null
}

# The clean per-port gather: RouterOS `:foreach` emits ONE pipe-delimited line per ethernet port.
# `get` returns raw integer counters (no thousands-space); `monitor ... as-value` yields live rate.
_NC_PORT_FOREACH=':foreach i in=[/interface ethernet find] do={:local n [/interface ethernet get $i name]; :local m [/interface ethernet monitor $n once as-value]; :put ("PORT|" . $n . "|comment=" . [/interface ethernet get $i comment] . "|running=" . [/interface ethernet get $i running] . "|rate=" . ($m->"rate") . "|fullduplex=" . ($m->"full-duplex") . "|status=" . ($m->"status") . "|dq1=" . [/interface ethernet get $i tx-drop-queue1-packet] . "|txdrop=" . [/interface ethernet get $i tx-drop-packet] . "|fcs=" . [/interface ethernet get $i rx-fcs-error])}'

# _nc_kv <key> <pipe-line> -> value of key=... from a "PORT|name|k=v|k=v" line (trivial split).
_nc_kv() {
  local key="$1" line="$2" field
  IFS='|' read -r -a field <<< "$line"
  local f
  for f in "${field[@]}"; do
    case "$f" in "$key="*) printf '%s' "${f#*=}"; return 0 ;; esac
  done
  printf ''
}

# -- gather one node into flat "node.key=value" lines on stdout -------------------------------------
# _nc_gather_node <name> <ip> <role: router|switch>
_nc_gather_node() {
  local name="$1" ip="$2" role="$3"
  local sys qos ports_out line port
  sys="$(_nc_ssh "$ip" '/system identity print; /system resource print')"
  if [ -z "$sys" ]; then
    printf '%s.reachable=false\n' "$name"
    return 0
  fi
  printf '%s.reachable=true\n' "$name"
  printf '%s.ip=%s\n' "$name" "$ip"
  printf '%s.role=%s\n' "$name" "$role"
  printf '%s.identity=%s\n' "$name" "$(netcfg_parse_field name "$sys")"
  printf '%s.ros=%s\n' "$name" "$(netcfg_normalize_version "$(netcfg_parse_field version "$sys")")"
  printf '%s.board=%s\n' "$name" "$(netcfg_parse_field board-name "$sys")"
  [ "$role" = "switch" ] || return 0

  qos="$(_nc_ssh "$ip" '/interface ethernet switch qos settings print')"
  printf '%s.shared_buffers=%s\n' "$name" "$(netcfg_parse_field shared-buffers "$qos")"

  ports_out="$(_nc_ssh "$ip" "$_NC_PORT_FOREACH")"
  while IFS= read -r line; do
    case "$line" in PORT\|*) : ;; *) continue ;; esac
    port="$(printf '%s' "$line" | cut -d'|' -f2)"
    [ -n "$port" ] || continue
    printf '%s.port.%s.comment=%s\n'    "$name" "$port" "$(_nc_kv comment "$line")"
    printf '%s.port.%s.running=%s\n'    "$name" "$port" "$(_nc_kv running "$line")"
    printf '%s.port.%s.rate=%s\n'       "$name" "$port" "$(_nc_kv rate "$line")"
    printf '%s.port.%s.fullduplex=%s\n' "$name" "$port" "$(_nc_kv fullduplex "$line")"
    printf '%s.port.%s.dq1=%s\n'        "$name" "$port" "$(_nc_kv dq1 "$line")"
    printf '%s.port.%s.fcs=%s\n'        "$name" "$port" "$(_nc_kv fcs "$line")"
  done <<< "$ports_out"
}

# _nc_gather_all -> all nodes' flat lines on stdout.
_nc_gather_all() {
  local pair name ip
  pair="$NETCFG_ROUTER"; name="${pair%%|*}"; ip="${pair##*|}"
  _nc_gather_node "$name" "$ip" router
  for pair in $NETCFG_SWITCHES; do
    name="${pair%%|*}"; ip="${pair##*|}"
    _nc_gather_node "$name" "$ip" switch
  done
}

# -- live drop-RATE probe for one port (--check): read dq1, sleep WINDOW, read again -> classify -----
_nc_drop_rate_verdict() {
  local ip="$1" port="$2" a b st
  st="$(_nc_ssh "$ip" "/interface ethernet print stats where name=$port")"
  a="$(netcfg_parse_stat tx-drop-queue1-packet "$st")"
  sleep "$NETCFG_DROP_WINDOW"
  st="$(_nc_ssh "$ip" "/interface ethernet print stats where name=$port")"
  b="$(netcfg_parse_stat tx-drop-queue1-packet "$st")"
  [ -n "$a" ] && [ -n "$b" ] || { printf 'UNKNOWN'; return 0; }
  netcfg_classify_drop_rate "$((b - a))" "$NETCFG_DROP_WINDOW" "$NETCFG_DROP_THRESHOLD"
}

# -- JSON build (flat lines -> nested JSON) via python3 (standard on dev1; latency_pins_verify.py precedent)
# The flat "node.key=value" data is passed as a FILE (argv), never on stdin: `python3 - <<'PY'` already
# consumes stdin for the PROGRAM heredoc, so reading data from stdin too would collide (it reads empty).
_nc_flat_to_json() {
  local captured="$1" flatfile="$2"
  python3 - "$captured" "$flatfile" <<'PY'
import sys, json
captured = sys.argv[1]
tree = {}
for line in open(sys.argv[2]):
    line = line.rstrip("\n")
    if not line or "=" not in line:
        continue
    key, _, val = line.partition("=")
    parts = key.split(".")
    node = tree
    for p in parts[:-1]:
        node = node.setdefault(p, {})
    node[parts[-1]] = val
out = {
    "_comment": ("#797 venue MikroTik switch-chain config baseline. REPORT-ONLY source of truth: "
                 "scripts/netcfg-audit.sh --check diffs the live chain vs this and reports drift LOUD; "
                 "it NEVER overwrites (a legitimate re-config is recorded by updating this file in a PR, "
                 "exactly like scripts/latency-pins-baseline.json). Seeded from a live read-only snapshot. "
                 "See .claude/rules/netcfg-audit.md."),
    "_captured": captured,
    "nodes": tree,
}
print(json.dumps(out, indent=2, sort_keys=True))
PY
}

# _nc_baseline_json -> the baseline "nodes" object as flat "node.key=value" lines (for comparison).
_nc_baseline_flat() {
  [ -f "$NETCFG_BASELINE" ] || return 1
  python3 - "$NETCFG_BASELINE" <<'PY'
import sys, json
try:
    data = json.load(open(sys.argv[1]))
except Exception as e:
    sys.stderr.write("netcfg-audit: cannot read baseline: %s\n" % e); sys.exit(1)
def walk(prefix, obj):
    if isinstance(obj, dict):
        for k, v in obj.items():
            walk(prefix + [k], v)
    else:
        print("%s=%s" % (".".join(prefix), obj))
walk([], data.get("nodes", {}))
PY
}

# ============================ modes ============================
case "$MODE" in
  capture)
    log "capturing live chain -> $NETCFG_BASELINE"
    tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
    _nc_gather_all > "$tmp"
    [ -s "$tmp" ] || { log "capture gathered nothing (all switches unreachable?)"; exit 2; }
    _nc_flat_to_json "$(date '+%Y-%m-%d')" "$tmp" > "$NETCFG_BASELINE" \
      || { log "capture failed"; exit 2; }
    log "wrote baseline for $(grep -c '\.reachable=true' "$tmp") reachable node(s) to $NETCFG_BASELINE"
    ;;

  json)
    tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
    _nc_gather_all > "$tmp"
    _nc_flat_to_json "$(date '+%Y-%m-%d')" "$tmp"
    ;;

  check)
    log "checking live chain vs baseline $NETCFG_BASELINE"
    base="$(_nc_baseline_flat)" || { log "no readable baseline at $NETCFG_BASELINE -- run --capture first"; exit 2; }
    live="$(_nc_gather_all)"
    statuses=()
    report=()
    # Iterate the baselined config fields we treat as STABLE: shared_buffers, ros, per-port rate +
    # comment(role). `running`/counter fields live in the snapshot but are NOT baseline-diffed (a
    # device unplugged between events must not page). Drop-RATE is a separate live probe below.
    while IFS= read -r bline; do
      key="${bline%%=*}"; bval="${bline#*=}"
      case "$key" in
        *.shared_buffers|*.ros) : ;;
        *.port.*.rate|*.port.*.comment) : ;;
        *) continue ;;
      esac
      lval="$(printf '%s\n' "$live" | sed -n "s/^$(printf '%s' "$key" | sed 's/[.[\*^$/]/\\&/g')=//p" | head -1)"
      case "$key" in
        *.port.*.rate)
          v="$(netcfg_classify_rate "$lval" "$bval")" ;;
        *)
          v="$(netcfg_classify_match "$lval" "$bval")" ;;
      esac
      statuses+=("$v")
      case "$v" in
        DRIFT|DEGRADED) report+=("  [$v] $key: baseline='$bval' live='$lval'") ;;
        ABSENT)         report+=("  [report-only $v] $key: baseline='$bval' live='(absent)'") ;;
      esac
    done <<< "$base"

    # Live drop-RATE probe: any switch port whose live cumulative dq1 is nonzero gets a rate re-probe.
    while IFS= read -r lline; do
      case "$lline" in *.port.*.dq1=*) : ;; *) continue ;; esac
      key="${lline%%=*}"; dq1="${lline#*=}"
      [ -n "$dq1" ] && [ "$dq1" != "0" ] || continue
      node="${key%%.port.*}"; rest="${key#*.port.}"; port="${rest%%.*}"
      ip="$(printf '%s\n' "$live" | sed -n "s/^${node}\.ip=//p" | head -1)"
      [ -n "$ip" ] || continue
      dv="$(_nc_drop_rate_verdict "$ip" "$port")"
      statuses+=("$dv")
      case "$dv" in
        DROPPING) report+=("  [DROPPING] $node $port: tx-drop-queue1 climbing >${NETCFG_DROP_THRESHOLD}/s (cumulative dq1=$dq1) -- microburst tail-drop; check shared-buffers / uplink step-down") ;;
        RESET)    report+=("  [report-only RESET] $node $port: drop counters went backwards (switch rebooted since read)") ;;
      esac
    done <<< "$live"

    overall="$(netcfg_drift_verdict "${statuses[@]}")"
    if [ "$overall" = "DRIFT" ]; then
      log "DRIFT detected:"
      printf '%s\n' "${report[@]}" >&2
      echo "NETCFG-DRIFT: venue switch chain drifted from baseline (${#report[@]} finding(s))"
      exit 3
    fi
    # surface report-only notes even on a CLEAN verdict (they never change the exit code)
    [ "${#report[@]}" -gt 0 ] && printf '%s\n' "${report[@]}" >&2
    echo "NETCFG-CLEAN: venue switch chain matches baseline"
    exit 0
    ;;
esac
