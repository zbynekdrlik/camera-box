#!/usr/bin/env bash
# scripts/netcfg-rules.sh -- issue 1554: the venue network rule gate of the rig test (header below).
set -euo pipefail
#
# Reads every MikroTik of one venue (SNV or PP) ONCE, read-only, and FAILS (exit 3) when the network
# breaks a rule or its configuration changed since the committed baseline. The rules and their
# parser are the pure scripts/netcfg_rules.py; the venue facts are scripts/netcfg-venues.json. This
# script only does the ssh: one call per device (the verified `print` / `monitor ... once` /
# `export terse` commands between `=== <section>` markers), never a loop, never a write.
#
#   scripts/netcfg-rules.sh --venue snv|pp [--check]      DEFAULT: exit 0 clean, 3 FAIL, 2 error
#   scripts/netcfg-rules.sh --venue snv|pp --capture      write the export baseline (PR the hash)
#   scripts/netcfg-rules.sh --venue V --from-dir DIR      read <DIR>/<device>.txt instead of ssh
#
# Access (scripts/netcfg-venues.json): SNV = the key-only read-only user `camerabox` (network#15);
# PP = the read-only user `snv` of the PP admin through `ssh -J companion-pp`, its password read by
# `sshpass -f <file>` (never printed, never on the command line; ROZHODNUTE 7167031660). PP runs
# only inside a PP rig run, never on a timer.
#
# State on dev1 (NETCFG_RULES_STATE_DIR, default ~/.camera-box): netcfg-export/<venue>/<device>.rsc
# (the export text a drift line is named from, never committed: the repo is public) and
# netcfg-rules-<venue>-tc.json (RSTP topology-change counters of the previous run).
# See .claude/rules/netcfg-rules.md.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NR_PY="$HERE/netcfg_rules.py"
NETCFG_RULES_FACTS="${NETCFG_RULES_FACTS:-$HERE/netcfg-venues.json}"
NETCFG_RULES_BASELINE="${NETCFG_RULES_BASELINE:-$HERE/netcfg-export-baseline.json}"
NETCFG_RULES_STATE_DIR_SET="${NETCFG_RULES_STATE_DIR:+1}"
NETCFG_RULES_STATE_DIR="${NETCFG_RULES_STATE_DIR:-$HOME/.camera-box}"
NETCFG_RULES_SSH_TIMEOUT="${NETCFG_RULES_SSH_TIMEOUT:-20}"

usage() { sed -n '4,24{/^#/p}' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
log() { printf '%s [netcfg-rules] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

MODE="check"
VENUE=""
FROM_DIR=""
_need_value() { [ "$2" -ge 2 ] || { echo "netcfg-rules: $1 needs a value" >&2; exit 2; }; }
while [ $# -gt 0 ]; do
  case "$1" in
    --venue) _need_value "$1" "$#"; VENUE="$2"; shift 2 ;;
    --check) MODE="check"; shift ;;
    --capture) MODE="capture"; shift ;;
    --from-dir) _need_value "$1" "$#"; FROM_DIR="$2"; shift 2 ;;
    --help | -h) usage; exit 0 ;;
    *) echo "netcfg-rules: unknown arg '$1' (try --help)" >&2; exit 2 ;;
  esac
done
case "$VENUE" in
  "" ) echo "netcfg-rules: --venue is required" >&2; exit 2 ;;
  *[!a-z0-9]*) echo "netcfg-rules: bad venue '$VENUE'" >&2; exit 2 ;;
esac
case "$NETCFG_RULES_SSH_TIMEOUT" in
  "" | *[!0123456789]* | 0) NETCFG_RULES_SSH_TIMEOUT=20 ;;
esac

devices="$(python3 "$NR_PY" devices "$NETCFG_RULES_FACTS" "$VENUE")" || exit 2

NR_USER="" NR_KEY="" NR_PWFILE="" NR_JUMP=""
while IFS=$'\t' read -r tag a b c d; do
  [ "$tag" = "ACCESS" ] || continue
  NR_USER="$a"; NR_KEY="$b"; NR_PWFILE="$c"; NR_JUMP="$d"
done <<< "$devices"
[ "$NR_KEY" = "-" ] && NR_KEY=""
[ "$NR_PWFILE" = "-" ] && NR_PWFILE=""
[ "$NR_JUMP" = "-" ] && NR_JUMP=""
[ -n "$NR_USER" ] && [ "$NR_USER" != "-" ] || { echo "netcfg-rules: venue $VENUE names no ssh user" >&2; exit 2; }

# _nr_ssh <host> <routeros-command> -> the device's output on stdout (stderr dropped); rc = ssh's.
# `ssh -n`: never read the caller's stdin. timeout bounds the whole call, not only the connect.
# Host keys: trust on first use into a per-venue known_hosts in the state dir, then pinned, so a
# password is never sent to a device whose key changed.
_nr_ssh() {
  local host="$1" cmd="$2"
  local -a opts=(-n -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$NR_KNOWN_HOSTS"
    -o LogLevel=ERROR -o ConnectTimeout="$NETCFG_RULES_SSH_TIMEOUT")
  [ -n "$NR_JUMP" ] && opts+=(-J "$NR_JUMP")
  if [ -n "$NR_PWFILE" ]; then
    # timeout OUTSIDE sshpass: a command timeout starts is not in the terminal's foreground, and
    # ssh would stop reading the password prompt sshpass answers on its pty.
    timeout "$NETCFG_RULES_SSH_TIMEOUT" sshpass -f "$NR_PWFILE" ssh "${opts[@]}" \
      -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive \
      "$NR_USER@$host" "$cmd" 2>/dev/null
    return
  fi
  timeout "$NETCFG_RULES_SSH_TIMEOUT" ssh "${opts[@]}" -i "$NR_KEY" -o IdentitiesOnly=yes \
    -o BatchMode=yes "$NR_USER@$host" "$cmd" 2>/dev/null
}

RAW_DIR="$(mktemp -d)"
trap 'rm -rf "$RAW_DIR"' EXIT
# A --from-dir replay must never overwrite the live run's RSTP counters or export copies: without
# an explicit NETCFG_RULES_STATE_DIR it keeps its state in the temporary dir.
if [ -n "$FROM_DIR" ] && [ -z "${NETCFG_RULES_STATE_DIR_SET:-}" ]; then
  NETCFG_RULES_STATE_DIR="$RAW_DIR/state"
fi
NR_KNOWN_HOSTS="$NETCFG_RULES_STATE_DIR/netcfg-known_hosts-$VENUE"
mkdir -p "$NETCFG_RULES_STATE_DIR"

if [ -n "$FROM_DIR" ]; then
  [ -d "$FROM_DIR" ] || { echo "netcfg-rules: --from-dir '$FROM_DIR' is not a directory" >&2; exit 2; }
  log "venue $VENUE: reading the device reads from $FROM_DIR (no ssh)"
  while IFS=$'\t' read -r tag name host cmd; do
    [ "$tag" = "DEVICE" ] || continue
    [ -f "$FROM_DIR/$name.txt" ] && cp "$FROM_DIR/$name.txt" "$RAW_DIR/$name.txt"
  done <<< "$devices"
else
  if [ -n "$NR_PWFILE" ]; then
    [ -r "$NR_PWFILE" ] || { log "no credential: $NR_PWFILE is not readable (venue $VENUE)"; exit 2; }
    case "$(stat -c %a "$NR_PWFILE")" in
      600 | 400) : ;;
      *) log "the password file $NR_PWFILE is readable by others (mode $(stat -c %a "$NR_PWFILE")), refusing"; exit 2 ;;
    esac
  elif [ ! -r "$NR_KEY" ]; then
    log "no credential: the key $NR_KEY is not readable (venue $VENUE)"; exit 2
  fi
  while IFS=$'\t' read -r tag name host cmd; do
    [ "$tag" = "DEVICE" ] || continue
    log "venue $VENUE: reading $name ($host)"
    _nr_ssh "$host" "$cmd" > "$RAW_DIR/$name.txt" || log "venue $VENUE: $name ($host) read failed"
  done <<< "$devices"
fi

EXPORT_DIR="$NETCFG_RULES_STATE_DIR/netcfg-export/$VENUE"
STATE_FILE="$NETCFG_RULES_STATE_DIR/netcfg-rules-$VENUE-tc.json"

if [ "$MODE" = "capture" ]; then
  python3 "$NR_PY" capture "$NETCFG_RULES_FACTS" "$VENUE" "$RAW_DIR" "$NETCFG_RULES_BASELINE" "$EXPORT_DIR" \
    || { log "capture failed, the baseline is unchanged"; exit 2; }
  log "baseline written to $NETCFG_RULES_BASELINE -- commit it in a PR"
  exit 0
fi

out="$(python3 "$NR_PY" check "$NETCFG_RULES_FACTS" "$VENUE" "$RAW_DIR" "$NETCFG_RULES_BASELINE" \
  "$EXPORT_DIR" "$STATE_FILE")" || { log "the rule check failed to run"; exit 2; }
hard=0
while IFS=$'\t' read -r status is_hard msg; do
  [ -n "$status" ] || continue
  if [ "$is_hard" = "1" ]; then
    hard=$((hard + 1))
    printf '  [%s] %s\n' "$status" "$msg"
  else
    printf '  [report-only %s] %s\n' "$status" "$msg"
  fi
done <<< "$out"
if [ "$hard" -gt 0 ]; then
  echo "NETCFG-RULES-FAIL: venue $VENUE network breaks $hard rule(s) -- fix the network (or record a deliberate change with --capture + a PR)"
  exit 3
fi
echo "NETCFG-RULES-CLEAN: venue $VENUE network passes the rules"
exit 0
