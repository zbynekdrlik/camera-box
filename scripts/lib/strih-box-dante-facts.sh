#!/usr/bin/env bash
# airuleset:script-ok source-only lib (the Dante fact validators of the strih box-fact loader; no top-level
# statements) -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing
# runs in the CALLER's shell. scripts/lib/strih-box-facts.sh sources it and calls both validators from
# strih_box_validate. Moved verbatim out of the loader (issue 1502) so the loader stays under its
# 1000-line budget.

# _strih_box_dante_devices_ok NAME VALUE LEG SENDER -> rc 0 for `none`, or for `name=arc:cmc:flows` devices
# (space-joined) on a box with a leg and no leg VBAN sender, every port 1024..65535, the ARC port in dstack's
# slot range 4440-4471 and no two devices sharing one. Issue 1519 (dstack #264): a device's flow ports are the
# fixed block of its slot (29800-31847, from the ARC port), so no RX range is declared; VBAN on a Dante leg is
# retired (dstack's generic filter admits no VBAN). rc 1 + a named line otherwise.
_strih_box_dante_devices_ok() {
  local name="$1" v="$2" leg="$3" sender="${4:-none}" d dev ports arc cmc flows p seen=" " rc=0 dg='[0123456789]'
  [ "$v" != none ] || return 0
  if [ "$leg" = none ]; then
    echo "strih-box ${name}: STRIH_DANTE_DEVICES needs a leg on the audio VLAN (STRIH_PROGRAM_VBAN_LEG is none)" >&2; return 1
  fi
  if [ "$sender" != none ]; then
    echo "strih-box ${name}: STRIH_PROGRAM_VBAN_SENDER must be none on a Dante leg: dstack's generic filter admits no VBAN (issue 1519)" >&2; rc=1
  fi
  for d in $v; do
    dev="${d%%=*}"; ports="${d#*=}"
    if [ "$d" = "$dev" ] || [[ ! "$dev" =~ ^[abcdefghijklmnopqrstuvwxyz][abcdefghijklmnopqrstuvwxyz0123456789-]*$ ]] \
       || [[ ! "$ports" =~ ^${dg}{4,5}:${dg}{4,5}:${dg}{4,5}$ ]]; then
      echo "strih-box ${name}: STRIH_DANTE_DEVICES '${d}' must be name=arc:cmc:flows (issue 1519: no RX range)" >&2; rc=1; continue
    fi
    IFS=: read -r arc cmc flows <<<"$ports"
    for p in "$arc" "$cmc" "$flows"; do
      if [ "$((10#$p))" -lt 1024 ] || [ "$((10#$p))" -gt 65535 ]; then
        echo "strih-box ${name}: STRIH_DANTE_DEVICES '${d}': port '${p}' must be 1024..65535" >&2; rc=1; continue 2
      fi
    done
    if [ "$((10#$arc))" -lt 4440 ] || [ "$((10#$arc))" -gt 4471 ]; then
      echo "strih-box ${name}: STRIH_DANTE_DEVICES '${d}': the ARC port must sit in dstack's slots 4440-4471 (no rx/tx port base is written, issue 1519)" >&2; rc=1; continue
    fi
    for p in "$arc" "$cmc" "$flows"; do
      case "$seen" in *" ${p} "*) echo "strih-box ${name}: STRIH_DANTE_DEVICES: port ${p} is used twice" >&2; rc=1 ;; esac
      seen="${seen}${p} "
    done
  done
  return "$rc"
}

# _strih_box_dante_terms_ok NAME PROG NET RBUF TX TRACK -> issues 1510 + 1513 (obs v0.4.27): PROG=1 needs NET (a: 0.25/0.5/1/2/5)
# + RBUF (b: 0 < ms <= 50, <= 6 decimals, no leading 0) explicit, TX none or `L,R` (two labels) + TRACK 1..6, never without RBUF; PROG=0 all none.
_strih_box_dante_terms_ok() {
  local n="strih-box $1:" prog="$2" net="$3" rb="$4" tx="$5" tr="$6" rc=0 d='[0123456789]' l r k
  if [ "$prog" = 0 ]; then
    for k in NETWORK_LATENCY_MS:"$net" RECEIVE_BUFFER_MS:"$rb" TX:"$tx" TX_TRACK:"$tr"; do
      [ "${k#*:}" = none ] || { echo "$n STRIH_DANTE_${k%%:*} must be none: no <role>-obs program device in STRIH_DANTE_DEVICES" >&2; rc=1; }
    done; return "$rc"; fi
  case "$net" in 0.25 | 0.5 | 1 | 2 | 5) ;; *) echo "$n STRIH_DANTE_NETWORK_LATENCY_MS '${net}' must be explicit for the program device: Dante Controller's 0.25, 0.5, 1, 2 or 5 ms (issue 1510)" >&2; rc=1 ;; esac
  if [[ ! "$rb" =~ ^(0|[123456789]${d}?)(\.${d}{1,6})?$ ]] || [[ "$rb" =~ ^[0.]+$ ]] || [ "$((10#${rb%%.*}))" -gt 50 ] || [[ "$rb" =~ ^50\.0*[123456789] ]]; then
    echo "$n STRIH_DANTE_RECEIVE_BUFFER_MS '${rb}' must be explicit for the program device: ms above 0 and at most 50, up to 6 decimals (issue 1510)" >&2; rc=1; fi
  [ "$tx" != none ] || { [ "$tr" = none ] || { echo "$n STRIH_DANTE_TX_TRACK must be none without STRIH_DANTE_TX" >&2; rc=1; }; return "$rc"; }
  l="${tx%%,*}"; r="${tx#*,}"
  if [ "$l" = "$tx" ] || [[ "$r" == *,* || -z "$l" || -z "$r" || "$l$r" == " "* || "$l" == *" " || "$r" == [\ ]* || "$r" == *" " ]] || [ "$l" = "$r" ] || [[ "$l$r" == *[\"\\]* ]] || [ "${#l}" -gt 31 ] || [ "${#r}" -gt 31 ]; then
    echo "$n STRIH_DANTE_TX '${tx}' must be two different channel labels 'L,R' (1..31 characters each, no quote or backslash, no space first or last)" >&2; rc=1; fi
  [[ "$tr" =~ ^[123456]$ ]] || { echo "$n STRIH_DANTE_TX_TRACK '${tr}' must be the OBS track sent, 1..6" >&2; rc=1; }
  [ "$rb" != none ] || { echo "$n STRIH_DANTE_TX needs an explicit STRIH_DANTE_RECEIVE_BUFFER_MS: a TX without it lowers the RX latency to 8.365 ms (issue 1513)" >&2; rc=1; }
  return "$rc"
}
