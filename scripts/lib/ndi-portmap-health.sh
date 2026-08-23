#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (obs-watchdog-decision.sh, netcfg-audit.sh,
# network-reach-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The callers (scripts/ndi-portmap-audit.sh, scripts/ndi-portmap-alert-watchdog.sh) set their own.
#
# scripts/lib/ndi-portmap-health.sh -- #1181: the SHARED, PURE parse + map-diff core for the dev1-side
# NDI sender port-map stability watchdog. No I/O, no avahi, no ssh -- so it can be unit-tested
# exhaustively off-rig against captured `avahi-browse -rtp` output (mirrors netcfg-audit.sh /
# obs-watchdog-decision.sh). scripts/ndi-portmap-audit.sh does the read-only avahi gather + the
# baseline-JSON extraction and feeds these functions primitive strings only.
#
# WHY (#1181): libndi assigns sender TCP ports sequentially from 5961 in CREATION ORDER inside one OBS
# process; DistroAV defers the main/preview outputs to OBS_FRONTEND_EVENT_FINISHED_LOADING (after the
# per-source ndi_filter republishes are created during scene-collection load), so the map is
# deterministic across CLEAN restarts but reshuffles when an output was added/removed live. A stock
# receiver (building TVs' NDI Studio Monitor) reconnecting by CACHED port then silently shows whichever
# sender inherited it (NDI connect-by-URL never verifies the name). This is the SENDER-side prevention
# layer complementing #1180's receiver-side by-URL identity verify. These pure functions turn a live
# mDNS snapshot + a checked-in baseline into a report-only "a name changed port" verdict.
#
# Source-only: pure functions, no side effects at source time.

# ndi_avahi_unescape <escaped> -> stdout: the decoded service name. avahi-browse -p escapes special
#   bytes as `\DDD` where DDD is DECIMAL (NOT octal): `\032`=space(32), `\040`="("(40), `\041`=")"(41),
#   `\092`="\"(92). Each non-ASCII byte is escaped separately, so a multibyte UTF-8 char round-trips
#   byte-by-byte. A backslash NOT followed by exactly 3 decimal digits is passed through literally.
ndi_avahi_unescape() {
  printf '%s' "${1:-}" | awk '
    {
      out = ""; n = length($0); i = 1
      while (i <= n) {
        c = substr($0, i, 1)
        if (c == "\\" && i + 3 <= n) {
          d = substr($0, i + 1, 3)
          if (d ~ /^[0-9][0-9][0-9]$/) {
            out = out sprintf("%c", d + 0)   # d is a decimal string; d+0 forces decimal, never octal
            i += 4
            continue
          }
        }
        out = out c
        i++
      }
      printf "%s", out
    }'
}

# ndi_avahi_parse_resolved <one-line> -> stdout: "<name>\t<ip>\t<port>\t<hostname>" for a RESOLVED
#   avahi -p line (starts with "=;"), else nothing. avahi -p resolved format is semicolon-delimited:
#     =;<iface>;<proto>;<escaped-name>;<type>;<domain>;<hostname>;<address>;<port>;<txt...>
#   The escaped-name field never contains a raw ";" (avahi escapes it as \059), so splitting on ";" is
#   safe. A "+;" browse line (unresolved) or a malformed/non-numeric-port line yields nothing.
ndi_avahi_parse_resolved() {
  local line="${1:-}"
  case "$line" in "=;"*) : ;; *) return 0 ;; esac
  # Fields 4/8/9/7 (name/ip/port/host). Field 4 (escaped name) is space-free, so a single space-joined
  # awk output splits cleanly with `read`.
  local raw name ip port host
  raw="$(printf '%s\n' "$line" | awk -F';' 'NF>=9 {print $4" "$8" "$9" "$7}')"
  [ -n "$raw" ] || return 0
  read -r name ip port host <<<"$raw"
  [ -n "$name" ] && [ -n "$ip" ] && [ -n "$port" ] || return 0
  printf '%s' "$port" | grep -Eq '^[0-9]+$' || return 0
  printf '%s\t%s\t%s\t%s\n' "$(ndi_avahi_unescape "$name")" "$ip" "$port" "$host"
}

# ndi_portmap_select <tsv-block> <ip> <name_prefix> <anchor_fullname> -> stdout: "<name>=<port>" lines
#   (sorted) for exactly ONE OBS instance's senders. <tsv-block> is the concatenation of
#   ndi_avahi_parse_resolved output ("name\tip\tport\thost" per line). Selection:
#     1) keep lines whose ip == <ip> AND whose name STARTS WITH <name_prefix>;
#     2) among those, find the mDNS hostname of the line whose name == <anchor_fullname> (the OBS
#        program output, e.g. "STRIH-SNV (2ME PGM)"); that hostname identifies the OBS process;
#     3) emit name=port for the kept lines sharing that hostname group.
#   Two NDI instances can share a box+prefix (the strih OBS instance AND a separate Arena/CG-bridge
#   Spout at the same IP with the same "STRIH-SNV " prefix); grouping by the anchor's hostname isolates
#   the OBS process and excludes the CG source (whose port never participates in the OBS reshuffle).
#   If the anchor is absent (OBS down / avahi empty / anchor renamed) -> emits NOTHING; the caller
#   treats an empty selection as a gather error, never as a port change.
ndi_portmap_select() {
  local block="${1:-}" want_ip="${2:-}" prefix="${3:-}" anchor="${4:-}"
  awk -F'\t' -v ip="$want_ip" -v pfx="$prefix" -v anc="$anchor" '
    $2 == ip && index($1, pfx) == 1 {
      k++; nm[k] = $1; pt[k] = $3; hs[k] = $4
      if ($1 == anc) ahost = $4
    }
    END {
      if (ahost == "") exit 0
      for (i = 1; i <= k; i++) if (hs[i] == ahost) print nm[i] "=" pt[i]
    }
  ' <<<"$block" | sort
}

# ndi_portmap_classify_port <live_port> <baseline_port> -> stdout: OK | MOVED | ABSENT | UNSET
#   Per-name port drift (mirrors netcfg_classify_match's shape). Empty baseline -> UNSET (nothing
#   pinned for this name); empty live but pinned baseline -> ABSENT (the sender vanished this pass);
#   equal -> OK; different -> MOVED (the "wrong sender inherited the port" symptom -- the only status
#   that pages).
ndi_portmap_classify_port() {
  local live="${1:-}" base="${2:-}"
  if [ -z "$base" ]; then printf 'UNSET\n'; return 0; fi
  if [ -z "$live" ]; then printf 'ABSENT\n'; return 0; fi
  if [ "$live" = "$base" ]; then printf 'OK\n'; else printf 'MOVED\n'; fi
}

# ndi_portmap_verdict <status...> -> stdout: CHANGED | STABLE
#   Aggregate per-name statuses. ONLY a MOVED port pages (a receiver is displaying the wrong sender
#   RIGHT NOW). The report-only statuses (OK|ABSENT|UNSET) are surfaced by the caller but never fire an
#   alert on their own: ABSENT is a transient OBS reload / a removed output that reshuffles only the
#   NEXT restart, and an all-empty instance is a gather error the caller already screens out (box
#   reachability is #1001's job). No args -> STABLE (nothing to judge).
ndi_portmap_verdict() {
  local s
  for s in "$@"; do
    case "$s" in MOVED) printf 'CHANGED\n'; return 0 ;; esac
  done
  printf 'STABLE\n'
}
