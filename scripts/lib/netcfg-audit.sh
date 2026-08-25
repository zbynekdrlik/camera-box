#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (obs-watchdog-decision.sh, network-reach-health.sh,
# cadence-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file executes
# it in the CALLER's shell, so strict mode here would leak into whichever caller sources it. The
# callers (scripts/netcfg-audit.sh, scripts/netcfg-drift-alert-watchdog.sh) set their own strict mode.
#
# scripts/lib/netcfg-audit.sh -- #797: the SHARED, PURE parse + drift-classification core for the
# dev1-side venue-switch config-drift audit (the "netcfg" facet). No I/O, no ssh, no jq, so it can be
# unit-tested exhaustively off-rig against captured RouterOS output (mirrors network-reach-health.sh /
# obs-watchdog-decision.sh). The scripts/netcfg-audit.sh orchestrator does the read-only ssh gather +
# the baseline-JSON field extraction and feeds these functions primitive strings only.
#
# WHY (#797): the venue MikroTik chain (RB4011 router_snv + 4x CRS310 foh1_audio/stage_av/foh1_video/
# foh2_video) had no durable machine-checkable record of its healthy state. The KEPT microburst fix
# (`shared-buffers` 40%->80%) has no guard against a silent revert, and per-port drop counters / link
# rates / roles are watched by nobody until someone hand-ssh-es in mid-incident. These pure functions
# turn a live read-only snapshot + a checked-in baseline into a report-only drift verdict.
#
# Source-only: pure functions, no side effects at source time.

# netcfg_parse_field <label> <block> -> stdout: the trimmed value after "<label>:" (first match), or
#   empty if absent. START-ANCHORED on the (whitespace-trimmed) line so `sfp-vendor-name:` never
#   satisfies a `name` query and `mirror-buffers:` never satisfies a `shared-buffers` query. `<label>`
#   is matched literally (RouterOS labels carry hyphens: full-duplex, shared-buffers, tx-drop-...).
netcfg_parse_field() {
  local label="${1:-}" block="${2:-}"
  [ -n "$label" ] || { printf ''; return 0; }
  local esc
  esc=$(printf '%s' "$label" | sed 's/[.[\*^$/]/\\&/g')
  printf '%s\n' "$block" \
    | sed -n "s/^[[:space:]]*${esc}:[[:space:]]*\(.*[^[:space:]]\)[[:space:]]*\$/\1/p" \
    | head -1
}

# netcfg_parse_stat <counter> <stats_block> -> stdout: the counter's integer with RouterOS thousands
#   spaces removed (`100 054` -> `100054`), or empty if absent. START-ANCHORED like parse_field, so a
#   counter name is never matched inside a longer one.
netcfg_parse_stat() {
  local counter="${1:-}" block="${2:-}"
  [ -n "$counter" ] || { printf ''; return 0; }
  local esc raw
  esc=$(printf '%s' "$counter" | sed 's/[.[\*^$/]/\\&/g')
  raw=$(printf '%s\n' "$block" \
    | sed -n "s/^[[:space:]]*${esc}:[[:space:]]*\([0-9 ]*[0-9]\)[[:space:]]*\$/\1/p" \
    | head -1)
  [ -n "$raw" ] || { printf ''; return 0; }
  printf '%s' "${raw// /}"
}

# netcfg_normalize_rate <rate_str> -> stdout: a comparable Mbps integer (10Gbps->10000, 2.5Gbps->2500,
#   1Gbps->1000, 100Mbps->100, 10Mbps->10), or empty for an unparseable/absent rate (a down link).
netcfg_normalize_rate() {
  local r="${1:-}"
  case "$r" in
    *Gbps) awk -v n="${r%Gbps}" 'BEGIN{ if (n ~ /^[0-9]+(\.[0-9]+)?$/) printf "%d", n*1000 }' ;;
    *Mbps) awk -v n="${r%Mbps}" 'BEGIN{ if (n ~ /^[0-9]+(\.[0-9]+)?$/) printf "%d", n*1 }' ;;
    *) printf '' ;;
  esac
}

# netcfg_normalize_version <version_str> -> stdout: the bare version, channel suffix dropped
#   (`7.23.3 (stable)` -> `7.23.3`).
netcfg_normalize_version() {
  local v="${1:-}"
  printf '%s' "${v%% *}"
}

# netcfg_classify_match <live> <baseline> -> stdout: OK | DRIFT | ABSENT | UNSET
#   Exact-string drift for a config field that SHOULD be stable (shared-buffers, ROS version, port
#   role, host->port). Empty baseline -> UNSET (nothing pinned); empty live but pinned baseline ->
#   ABSENT (the field vanished); equal -> OK; different -> DRIFT (the silent-revert case).
netcfg_classify_match() {
  local live="${1:-}" base="${2:-}"
  if [ -z "$base" ]; then printf 'UNSET\n'; return 0; fi
  if [ -z "$live" ]; then printf 'ABSENT\n'; return 0; fi
  if [ "$live" = "$base" ]; then printf 'OK\n'; else printf 'DRIFT\n'; fi
}

# netcfg_classify_rate <live_rate_str> <baseline_rate_str> -> stdout: OK | FASTER | DEGRADED | ABSENT | UNSET
#   Link-speed drift. A baselined port negotiating SLOWER than baseline (10G->1G) is DEGRADED (the
#   duplex/speed-regression class the incident asked to catch); FASTER is only informational; a live
#   link that won't parse (down / not present) is ABSENT; empty baseline is UNSET.
netcfg_classify_rate() {
  local live="${1:-}" base="${2:-}"
  [ -n "$base" ] || { printf 'UNSET\n'; return 0; }
  local lm bm
  bm=$(netcfg_normalize_rate "$base")
  [ -n "$bm" ] || { printf 'UNSET\n'; return 0; }
  lm=$(netcfg_normalize_rate "$live")
  [ -n "$lm" ] || { printf 'ABSENT\n'; return 0; }
  if [ "$lm" -eq "$bm" ]; then printf 'OK\n'
  elif [ "$lm" -gt "$bm" ]; then printf 'FASTER\n'
  else printf 'DEGRADED\n'; fi
}

# netcfg_classify_drop_rate <delta> <window_s> [threshold_per_s=1] -> stdout: OK | DROPPING | RESET | UNKNOWN
#   A measured drop-counter DELTA over a <window_s> second window vs a per-second threshold (the
#   ops-skill `:delay 6s` recipe, moved to a pure classifier). rate = delta/window; DROPPING when it
#   EXCEEDS the threshold (the microburst-tail-drop signature). A negative delta = the switch's
#   cumulative counters reset (a reboot) since the window's first read -> report-only RESET (the
#   baseline is stale, not a live drop storm). A zero/garbage window or non-integer delta -> UNKNOWN
#   (never a divide-by-zero).
netcfg_classify_drop_rate() {
  local delta="${1:-}" window="${2:-}" thr="${3:-1}"
  printf '%s' "$delta"  | grep -Eq '^-?[0-9]+$' || { printf 'UNKNOWN\n'; return 0; }
  printf '%s' "$window" | grep -Eq '^[0-9]+$'   || { printf 'UNKNOWN\n'; return 0; }
  # A non-numeric threshold passed raw to awk makes `rate > thr` a fragile STRING comparison (e.g.
  # "2" > "bad" is false -> a real drop storm silently MISSED); fall back to a deterministic safe
  # default of 1/s instead. (int or float accepted.)
  printf '%s' "$thr"    | grep -Eq '^[0-9]+(\.[0-9]+)?$' || thr=1
  [ "$window" -gt 0 ] || { printf 'UNKNOWN\n'; return 0; }
  [ "$delta" -lt 0 ] && { printf 'RESET\n'; return 0; }
  awk -v d="$delta" -v w="$window" -v t="$thr" \
    'BEGIN{ if (w>0 && (d/w) > t) print "DROPPING"; else print "OK" }'
}

# netcfg_drift_verdict <status...> -> stdout: DRIFT | CLEAN
#   Aggregate per-field statuses into an overall verdict. Only the HARD-drift statuses
#   (DRIFT|DEGRADED|DROPPING) page; the report-only statuses (OK|FASTER|UNSET|ABSENT|RESET|UNKNOWN)
#   are surfaced in the report but never fire an alert on their own -- a disconnected device (ABSENT),
#   a rebooted switch (RESET), or an unreadable field (UNKNOWN) must not page mid-event. No args ->
#   CLEAN (nothing to judge).
netcfg_drift_verdict() {
  local s
  for s in "$@"; do
    case "$s" in
      DRIFT | DEGRADED | DROPPING) printf 'DRIFT\n'; return 0 ;;
    esac
  done
  printf 'CLEAN\n'
}

# netcfg_port_is_designated <node> <port> <designated-list> -> exit 0 if "<node>|<port>" is an EXACT
#   token in the space-separated <designated-list> (each token "node|port"), else exit 1. A designated
#   port is the drop-RATE probe's ALWAYS-sampled set (#1110): it is live rate-probed on EVERY --check
#   regardless of cumulative-counter growth, so the audit always carries a fresh drop DELTA from the
#   suspect uplink for the next starvation episode -- the strih PC is a direct-DAC uplink into
#   foh2_video port sfp-sfpplus2, so its egress-toward-strih tx-drop-queue1 must be sampled every run
#   even while flat. Empty node/port/list -> not designated (exit 1). Pure: no I/O.
netcfg_port_is_designated() {
  local node="${1:-}" port="${2:-}" list="${3:-}"
  [ -n "$node" ] && [ -n "$port" ] || return 1
  local want="$node|$port" tok
  for tok in $list; do
    [ "$tok" = "$want" ] && return 0
  done
  return 1
}
