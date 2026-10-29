#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (never executed directly) -- sourced by
# setup-device.sh / verify-device.sh / recording-e2e.sh. Like every sibling in scripts/lib/ it must
# NOT set -euo pipefail: sourcing runs it in the CALLER's shell and would change the caller's
# options. Every function below returns 0 (the one exception, the apply command, is shell TEXT whose
# own status is the attempt's result), so a caller's `set -euo pipefail` is never tripped.
#
# scripts/lib/cambox-egress-pacing.sh -- issue 1242: the cambox NDI egress PACING, made permanent.
#
# Why: all seven cameras hand their frame to NDI on the same genlock grid instant, so ~7 x 300 KB
# reach strih-snv at the 5 Gb/s line rate in ~3-4 ms and its RTL8157 USB NIC answers with PAUSE
# storms (issue 1242 finding 5902816575). The owner approved staggered sending (5904641502).
# Delaying the frame hand-off failed live twice because the NDI SDK encodes inside the send, so the
# bursts are spread by pacing each cambox's KERNEL egress instead (decision 5904673375): an `fq`
# root qdisc with a per-flow `maxrate` sends an already-encoded frame at no more than 400 Mb/s
# (~7 ms for a 300-350 KB frame). `fq` paces per FLOW, so dantesync's PTP/NTP packets and the
# intercom are their own flows and are never queued behind a video frame (a single `tbf` bucket
# would delay them and bias the clock). `flow_limit` 2000 holds a whole frame (~240 packets).
# Measured with it live on cam1-cam7 (result 5905948208): strih-snv PAUSE 39-110/min -> 28-36/min,
# receive-gap storms from ~1 per 5 min to ~1 per 30 min, the first release E2E with clean
# continuity. It helps but is not the full cure; 10 GbE through a PCIe NIC (issue 1387) is.
#
# Issue 1387: the JPEG XS link must go out at LINE rate (its slices leave the encoder as they are
# coded; the 400 Mb/s cap spread a frame over 23-33 ms and built a backlog). No socket option can lift
# fq's per-flow cap (fq takes the SMALLER of the socket pacing rate and maxrate), so the root is now
# classful, measured on a real 7.0 kernel in a netns:
#   root `prio 1:` bands 2, EVERY skb priority mapped to band 0 (the classic priomap would split
#     ordinary traffic across bands; this one leaves only the filter to move a packet);
#   band 0 = class 1:1 -> the SAME fq maxrate/flow_limit/limit as before: NDI, PTP/NTP, intercom;
#   band 1 = class 1:2 -> a `pfifo` (CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT), no pacing;
#   `fw` filter: skb mark CAMBOX_EGRESS_PACING_JXS_MARK -> 1:2. camera-box sets that mark
#     (SO_MARK) on its JPEG XS socket only (src/jxs_send.rs JXS_SOCKET_MARK, pinned equal by pytest).
# prio serves band 0 first: a ready NDI/PTP packet always goes before JPEG XS, and while fq holds a
# paced packet back its dequeue returns nothing, so JPEG XS fills the idle wire. A mark, not a port:
# the boot script cannot know the runtime CAMERA_BOX_JXS_TARGET port.
#
# Until this lib it lived only as a runtime qdisc applied by hand, dropped silently by a reboot, a
# re-provision or a new stick. Now (main design 5905959484, Approach 1):
#   * THIS lib is the ONE declaration of the rate and the limits + the pure builders/verdicts;
#   * setup-device.sh writes the GENERATED on-box script (cambox_egress_pacing_boot_script -- the
#     appliance has no checkout of this repo, so the unit cannot source this lib) and installs the
#     checked-in systemd/cambox-egress-pacing.service, ENABLE-only (never a live start);
#   * the unit applies it at every boot on the default-route interface resolved at run time (enp or
#     a renamed enx alike), with a bounded ~60 s retry for a late default route, then fails LOUD;
#     it restarts on failure (a box that boots before its switch port has carrier gets paced once
#     the route appears, never left unpaced for its whole uptime);
#   * verify-device.sh (ap) grades the live qdisc, the unit and the installed script + unit bytes
#     (a HARD gate), naming the fix that matches each failed facet;
#   * the E2E [0/8] reads every vetted cambox REPORT-ONLY (cambox_egress_pacing_e2e_report): a
#     missing pacing is a named WARNING, the run continues.
#
# Tests: tests/python/test_cambox_egress_pacing_1242.py (fake ip / tc / systemctl / sshpass).

# --- the ONE declaration ---------------------------------------------------------------------------
CAMBOX_EGRESS_PACING_RATE="400mbit"
CAMBOX_EGRESS_PACING_FLOW_LIMIT="2000"
CAMBOX_EGRESS_PACING_LIMIT="20000"
# Issue 1387: the JPEG XS class. The mark camera-box sets with SO_MARK on its JPEG XS socket (the
# `fw` filter's handle), and the unpaced FIFO's packet limit: above the ~1800 datagrams a 4 MB socket
# send buffer can hold in the one-send-per-datagram fallback, so the socket buffer, never the FIFO,
# bounds the queue. The tree's handles are fixed: root 1:, paced band 1:1 (fq 10:), JPEG XS 1:2 (pfifo 20:).
CAMBOX_EGRESS_PACING_JXS_MARK="0x1387"
CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT="4000"
CAMBOX_EGRESS_PACING_PRIOMAP="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"

CAMBOX_EGRESS_PACING_SERVICE_NAME="cambox-egress-pacing"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (the [egress-pacing] install)
CAMBOX_EGRESS_PACING_SERVICE_PATH="/etc/systemd/system/cambox-egress-pacing.service"
# The generated on-box script the unit runs (a ro-root appliance: written in setup-device's rw window).
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (the [egress-pacing] install)
CAMBOX_EGRESS_PACING_SCRIPT_PATH="/usr/local/sbin/cambox-egress-pacing"
# The checked-in unit setup-device installs: stage scripts/ AND systemd/ together on the box.
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (pre-flight + install)
CAMBOX_EGRESS_PACING_UNIT_SRC="${BASH_SOURCE[0]%/*}/../../systemd/cambox-egress-pacing.service"
# Boot retry: attempts x sleep ~= 60 s for a DHCP-late default route. An attempt COUNT, never a
# wall-clock deadline: the rig's dantesync date master can step the date while the box boots.
CAMBOX_EGRESS_PACING_RETRY_ATTEMPTS=30
CAMBOX_EGRESS_PACING_RETRY_SLEEP_S=2

# cambox_egress_pacing_iface_cmd -> remote shell TEXT printing the default-route interface: the `dev`
# of the FIRST `ip route show default` line (the lowest-metric route the kernel uses), or nothing.
# awk reads all of its input (no early `exit`), so `ip` never SIGPIPEs under a caller's pipefail.
cambox_egress_pacing_iface_cmd() {
  cat <<'EOF'
ip route show default 2>/dev/null | awk '!d { for (i = 1; i < NF; i++) if ($i == "dev") { d = $(i + 1); break } } END { if (d != "") print d }'
EOF
}

# cambox_egress_pacing_apply_cmd -> shell TEXT: ONE attempt to build the tree on the default-route
# interface: the prio root, the paced fq under 1:1, the JPEG XS FIFO under 1:2, the fw filter (each a
# `replace`, idempotent; issue 1387). A brace group whose status is the attempt's result: 0 = all four
# applied (one line on stdout), 1 = no default route yet or a tc step failed (one named line on
# stderr; the chain stops at the failed step and is never reported applied, tc never run without a
# route). The boot script wraps it in its bounded retry; nothing else applies it (setup-device is
# enable-only).
cambox_egress_pacing_apply_cmd() {
  cat <<EOF
{
  _cep_if="\$($(cambox_egress_pacing_iface_cmd))"
  if [ -z "\$_cep_if" ]; then
    echo "cambox-egress-pacing: no default route yet -- no egress interface to pace (issue 1242)" >&2
    false
  elif tc qdisc replace dev "\$_cep_if" root handle 1: prio bands 2 priomap ${CAMBOX_EGRESS_PACING_PRIOMAP} &&
    tc qdisc replace dev "\$_cep_if" parent 1:1 handle 10: fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT} &&
    tc qdisc replace dev "\$_cep_if" parent 1:2 handle 20: pfifo limit ${CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT} &&
    tc filter replace dev "\$_cep_if" parent 1: protocol all prio 1 handle ${CAMBOX_EGRESS_PACING_JXS_MARK} fw classid 1:2; then
    echo "cambox-egress-pacing: dev \$_cep_if NDI paced (fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT} under prio 1:1), JPEG XS mark ${CAMBOX_EGRESS_PACING_JXS_MARK} at line rate (1:2) applied (issues 1242, 1387)"
  else
    # A half-built tree (a prio root over default pfifos) would leave NDI unpaced while the retries
    # run: put the pre-issue-1387 root fq back at once (NDI paced, JPEG XS paced with it).
    if tc qdisc replace dev "\$_cep_if" root fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT}; then
      echo "cambox-egress-pacing: building the egress tree on \$_cep_if FAILED -- fell back to the root fq (NDI paced, JPEG XS paced with it) (issues 1242, 1387)" >&2
    else
      echo "cambox-egress-pacing: building the egress tree on \$_cep_if FAILED, and the root fq fallback FAILED too -- the NDI egress may be UNPACED (issues 1242, 1387)" >&2
    fi
    false
  fi
}
EOF
}

# cambox_egress_pacing_boot_script -> the full content of ${CAMBOX_EGRESS_PACING_SCRIPT_PATH}: the
# apply command above (verbatim, the one source) inside a bounded retry. Exit 0 once applied; after
# the last attempt a loud FAILED line on stderr (the journal) and exit 1, and the unit restarts for
# the next round -- never a silent unpaced box. Only the first and the last attempt of a round log
# their reason, so a route-less box does not flood the journal.
# CAMBOX_EGRESS_PACING_RETRY_SLEEP_S in the unit's environment overrides the sleep (tests only).
cambox_egress_pacing_boot_script() {
  cat <<EOF
#!/bin/bash
# ${CAMBOX_EGRESS_PACING_SERVICE_NAME} boot script -- GENERATED by scripts/lib/cambox-egress-pacing.sh
# (camera-box issue 1242) and written by setup-device.sh; do not edit it on the box, re-run setup-device.sh.
# Run at boot by ${CAMBOX_EGRESS_PACING_SERVICE_NAME}.service: paces this box's NDI egress with an fq
# (maxrate ${CAMBOX_EGRESS_PACING_RATE} per flow) on the default-route interface, so the seven
# cameras' per-tick frame bursts reach strih-snv spread in time; the JPEG XS flow (mark
# ${CAMBOX_EGRESS_PACING_JXS_MARK}) goes past it at line rate (issue 1387).
set -euo pipefail
PATH="\$PATH:/usr/sbin:/sbin"
cep_attempts=${CAMBOX_EGRESS_PACING_RETRY_ATTEMPTS}
cep_sleep="\${CAMBOX_EGRESS_PACING_RETRY_SLEEP_S:-${CAMBOX_EGRESS_PACING_RETRY_SLEEP_S}}"
cep_apply()
$(cambox_egress_pacing_apply_cmd)
cep_n=0
while [ "\$cep_n" -lt "\$cep_attempts" ]; do
  cep_n=\$((cep_n + 1))
  if [ "\$cep_n" -eq 1 ] || [ "\$cep_n" -eq "\$cep_attempts" ]; then
    if cep_apply; then
      exit 0
    fi
  elif cep_apply 2>/dev/null; then
    # The middle attempts of a round stay quiet: a route-less box retries forever (the unit restarts).
    exit 0
  fi
  if [ "\$cep_n" -lt "\$cep_attempts" ]; then
    sleep "\$cep_sleep"
  fi
done
echo "cambox-egress-pacing: FAILED after \$cep_n attempt(s) -- the NDI egress is UNPACED (issue 1242)" >&2
exit 1
EOF
}

# cambox_egress_pacing_rate_bps RATE -> RATE in bits per second (integer), or nothing when it is not
# a tc rate. Unit case-insensitive: bit / kbit / mbit / gbit / tbit (SI, what tc prints: `400Mbit`)
# and the byte forms bps / kbps / mbps / gbps (x 8), so the declaration `400mbit` equals tc's `400Mbit`.
cambox_egress_pacing_rate_bps() {
  printf '%s\n' "${1:-}" | awk '
    { v = tolower($0) }
    match(v, /^[0-9]+(\.[0-9]+)?/) {
      n = substr(v, 1, RLENGTH) + 0; u = substr(v, RLENGTH + 1); m = 0
      if (u == "bit") m = 1; else if (u == "kbit") m = 1e3; else if (u == "mbit") m = 1e6
      else if (u == "gbit") m = 1e9; else if (u == "tbit") m = 1e12; else if (u == "bps") m = 8
      else if (u == "kbps") m = 8e3; else if (u == "mbps") m = 8e6; else if (u == "gbps") m = 8e9
      if (m > 0) printf "%.0f\n", n * m
    }' || true
  return 0
}

# cambox_egress_pacing_qdisc_line TEXT SELECTOR -> the FIRST `qdisc` line of TEXT (raw lines or the
# `|`-flattened gather form) whose position is SELECTOR: `root`, or a class id such as `1:1` (the
# line's `parent <id>`). Empty when there is none. Always returns 0.
cambox_egress_pacing_qdisc_line() {
  printf '%s\n' "${1:-}" | tr '|' '\n' | awk -v want="${2:-}" '
    $1 == "qdisc" && r == "" {
      pos = ""
      for (i = 3; i <= NF; i++) {
        if ($i == "root") { pos = "root"; break }
        if ($i == "parent" && i < NF) { pos = $(i + 1); break }
      }
      if (pos == want) r = $0
    }
    END { if (r != "") print r }' || true
  return 0
}

# cambox_egress_pacing_token LINE WORD -> the value after the first exact WORD token of LINE, with a
# `p` packet suffix stripped (tc prints `limit 20000p`). Empty when absent. Always returns 0.
cambox_egress_pacing_token() {
  printf '%s\n' "${1:-}" | awk -v w="${2:-}" '{ for (i = 1; i < NF; i++) if ($i == w) { v = $(i + 1); sub(/p$/, "", v); print v; break } }' || true
  return 0
}

# cambox_egress_pacing_number TEXT -> TEXT as a decimal number when it is hex (0x...) or decimal, else
# nothing (a tc handle; compared as a number). The digit sets are spelled out: a bracket range
# follows the locale (ci-testing-gotchas, issue 1302). Always returns 0.
cambox_egress_pacing_number() {
  local t="${1:-}"
  if [[ $t =~ ^0[xX][0123456789abcdefABCDEF]{1,8}$ ]]; then
    echo "$((t))"
  elif [[ $t =~ ^[0123456789]{1,10}$ ]]; then
    echo "$((10#$t))"
  fi
  return 0
}

# cambox_egress_pacing_verdict QDISC_TEXT [FILTER_TEXT] -> ONE line `OK <detail>` / `DRIFT <detail>` /
# `UNKNOWN <detail>` for `tc qdisc show dev <if>` and `tc filter show dev <if>` (raw lines, or the
# `|`-flattened form the gather emits). OK = the whole issue-1387 tree:
#   root `prio` with handle 1:, 2 bands and every priority mapped to band 0 (a priority mapped to
#     band 1 would send that traffic past the fq: the NDI egress could go unpaced);
#   under 1:1 an `fq` whose maxrate (compared in bit/s), flow_limit and limit equal the declaration;
#   under 1:2 a `pfifo` with the declared limit, the JPEG XS class;
#   a `fw` filter on parent 1: whose handle (compared as a number) is the declared mark, to 1:2,
#     and NO other classifying filter on the root (one would move traffic past the fq).
# DRIFT names every fault at once: the NDI side (`the NDI egress is UNPACED`, or a wrong value) and
# the JPEG XS side (`JPEG XS ...`: the flow is paced with NDI, or not classified). The pre-issue-1387
# root fq is DRIFT (NDI paced, the JPEG XS class missing). UNKNOWN = nothing gradable (tc absent, no
# default route, no root line). Always returns 0.
cambox_egress_pacing_verdict() {
  local text="${1:-}" filters="${2:-}" root kind handle bands priomap fq child want_mark found
  local f_kind f_handle f_target
  local maxrate flow_limit limit got_bps want_bps fifo_limit ndi="" jxs="" detail
  case "$text" in
    *__TC_ABSENT__*)
      echo "UNKNOWN tc (iproute2) is not installed -- the egress qdisc cannot be read"
      return 0 ;;
    *__NO_DEFAULT_ROUTE__*)
      echo "UNKNOWN no default route -- no egress interface to read"
      return 0 ;;
  esac
  root="$(cambox_egress_pacing_qdisc_line "$text" root)"
  if [ -z "$root" ]; then
    echo "UNKNOWN no root qdisc in the tc output"
    return 0
  fi
  kind="$(printf '%s\n' "$root" | awk '{ print $2 }' || true)"
  if [ "$kind" = "fq" ]; then
    echo "DRIFT root qdisc is fq, the pre-issue-1387 tree -- JPEG XS class 1:2 missing, the JPEG XS flow is paced with NDI"
    return 0
  fi
  if [ "$kind" != "prio" ]; then
    echo "DRIFT root qdisc is ${kind:-?}, not prio -- the NDI egress is UNPACED and the JPEG XS class 1:2 is missing"
    return 0
  fi
  handle="$(printf '%s\n' "$root" | awk '{ print $3 }' || true)"
  bands="$(cambox_egress_pacing_token "$root" bands)"
  priomap="$(printf '%s\n' "$root" | awk '{ for (i = 1; i <= NF; i++) if ($i == "priomap") { s = ""; for (j = i + 1; j <= NF; j++) s = s (s == "" ? "" : " ") $j; print s; break } }' || true)"
  if [ "$handle" != "1:" ]; then
    ndi="prio root handle ${handle:-?} (want 1:) -- its classes are not the tree's"
  fi
  if [ "$bands" != "2" ]; then
    ndi="${ndi:+$ndi, }prio bands ${bands:-?} (want 2)"
  fi
  if [ "$priomap" != "$CAMBOX_EGRESS_PACING_PRIOMAP" ]; then
    ndi="${ndi:+$ndi, }priomap '${priomap:-?}' maps a priority past band 0 -- that traffic is UNPACED"
  fi

  # The paced band: fq under 1:1 with the declared values.
  fq="$(cambox_egress_pacing_qdisc_line "$text" 1:1)"
  child="$(printf '%s\n' "$fq" | awk '{ print $2 }' || true)"
  if [ "$child" != "fq" ]; then
    ndi="${ndi:+$ndi, }no fq under 1:1 (${child:-nothing} there) -- the NDI egress is UNPACED"
  else
    maxrate="$(cambox_egress_pacing_token "$fq" maxrate)"
    flow_limit="$(cambox_egress_pacing_token "$fq" flow_limit)"
    limit="$(cambox_egress_pacing_token "$fq" limit)"
    if [ -z "$maxrate" ]; then
      ndi="${ndi:+$ndi, }fq under 1:1 without maxrate -- the NDI egress is UNPACED"
    else
      got_bps="$(cambox_egress_pacing_rate_bps "$maxrate")"
      want_bps="$(cambox_egress_pacing_rate_bps "$CAMBOX_EGRESS_PACING_RATE")"
      if [ -z "$got_bps" ] || [ "$got_bps" != "$want_bps" ]; then
        ndi="${ndi:+$ndi, }fq maxrate $maxrate (want $CAMBOX_EGRESS_PACING_RATE)"
      fi
    fi
    if [ "$flow_limit" != "$CAMBOX_EGRESS_PACING_FLOW_LIMIT" ]; then
      ndi="${ndi:+$ndi, }fq flow_limit ${flow_limit:-?} (want $CAMBOX_EGRESS_PACING_FLOW_LIMIT)"
    fi
    if [ "$limit" != "$CAMBOX_EGRESS_PACING_LIMIT" ]; then
      ndi="${ndi:+$ndi, }fq limit ${limit:-?} (want $CAMBOX_EGRESS_PACING_LIMIT)"
    fi
  fi

  # The JPEG XS class: pfifo under 1:2 + the fw filter on the declared mark.
  child="$(cambox_egress_pacing_qdisc_line "$text" 1:2)"
  if [ "$(printf '%s\n' "$child" | awk '{ print $2 }' || true)" != "pfifo" ]; then
    jxs="JPEG XS class 1:2 has no pfifo ($(printf '%s\n' "$child" | awk '{ print $2 }' || true))"
    jxs="${jxs% ()}"
  else
    fifo_limit="$(cambox_egress_pacing_token "$child" limit)"
    if [ "$fifo_limit" != "$CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT" ]; then
      jxs="JPEG XS pfifo limit ${fifo_limit:-?} (want $CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT)"
    fi
  fi
  # Every CLASSIFYING filter entry on the root (one carrying a classid / flowid; tc prints u32's as
  # `*flowid`, and a bare `filter ... fw chain 0` header line carries none) as `KIND HANDLE TARGET`.
  # Exactly one may exist: fw, the declared mark, to 1:2. Any other moves traffic past the fq.
  want_mark="$((CAMBOX_EGRESS_PACING_JXS_MARK))"
  found=0
  while read -r f_kind f_handle f_target; do
    [ -n "${f_kind:-}" ] || continue
    if [ "$f_kind" = fw ] && [ "$f_target" = "1:2" ] && [ "$(cambox_egress_pacing_number "$f_handle")" = "$want_mark" ]; then
      found=$((found + 1))
    else
      ndi="${ndi:+$ndi, }filter $f_kind $f_handle to $f_target on the root moves traffic past the fq -- that traffic is UNPACED"
    fi
  done <<FILTERS
$(printf '%s\n' "$filters" | tr '|' '\n' | awk '
    $1 == "filter" && $2 == "parent" && $3 == "1:" {
      k = "?"; h = "-"; t = ""
      for (i = 4; i <= NF; i++) {
        if ($i == "pref" && i + 2 <= NF) k = $(i + 2)
        if (($i == "handle" || $i == "fh") && i < NF) h = $(i + 1)
        if (($i == "classid" || $i == "flowid" || $i == "*flowid") && i < NF) t = $(i + 1)
      }
      if (t != "") print k, h, t
    }' || true)
FILTERS
  if [ "$found" -eq 0 ]; then
    jxs="${jxs:+$jxs, }no fw filter for mark $CAMBOX_EGRESS_PACING_JXS_MARK to 1:2 -- the JPEG XS flow is paced with NDI"
  fi

  detail="${ndi}${ndi:+${jxs:+; }}${jxs}"
  if [ -n "$detail" ]; then
    echo "DRIFT $detail"
  else
    echo "OK prio 1: -> 1:1 fq maxrate $maxrate flow_limit ${flow_limit}p limit ${limit}p (NDI paced) + 1:2 pfifo limit ${fifo_limit}p via fw mark $CAMBOX_EGRESS_PACING_JXS_MARK (JPEG XS at line rate)"
  fi
  return 0
}

# cambox_egress_pacing_functional_lines -> stdin with the comment lines (a leading `#`, the shebang
# included) and the blank lines dropped. The installed boot script and unit are compared on these
# lines, so a comment-only edit in this repo never turns every provisioned box stale, while a
# functional change (another rate, another ExecStart) still does. The gather embeds this exact
# function (`declare -f`), so the box and dev1 filter the same way.
cambox_egress_pacing_functional_lines() {
  grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*$' || true
}

# cambox_egress_pacing_gather_remote_snippet -> READ-ONLY remote bash (verify-device (ap) and the E2E
# row run it over ssh) printing one KEY=VALUE line per fact: the default-route interface, its qdisc
# (`|`-flattened, or __TC_ABSENT__ / __NO_DEFAULT_ROUTE__) and its filters (`|`-flattened, the
# issue-1387 JPEG XS fw filter), the unit's is-enabled / is-active /
# restart count, and what setup-device installed -- the boot script is executable and starts with
# its `#!/bin/bash` line, and the functional-line sha256 of the script and of the unit (empty when
# absent) -- so the verdict can prove the unit will actually run at the next boot. It never changes
# anything (only `tc qdisc show` / `tc filter show` and reads). sbin is appended to PATH because a non-login ssh PATH
# may omit it; a caller's own PATH entries still come first.
cambox_egress_pacing_gather_remote_snippet() {
  cat <<EOF
PATH="\$PATH:/usr/sbin:/sbin"
$(declare -f cambox_egress_pacing_functional_lines)
_cep_if="\$($(cambox_egress_pacing_iface_cmd))"
echo "PACING_IFACE=\$_cep_if"
if ! command -v tc >/dev/null 2>&1; then
  echo "PACING_QDISC=__TC_ABSENT__"
elif [ -z "\$_cep_if" ]; then
  echo "PACING_QDISC=__NO_DEFAULT_ROUTE__"
else
  echo "PACING_QDISC=\$(tc qdisc show dev "\$_cep_if" 2>/dev/null | tr '\n' '|')"
fi
if command -v tc >/dev/null 2>&1 && [ -n "\$_cep_if" ]; then
  echo "PACING_FILTER=\$(tc filter show dev "\$_cep_if" 2>/dev/null | tr '\n' '|')"
else
  echo "PACING_FILTER="
fi
echo "PACING_SVC_ENABLED=\$(systemctl is-enabled ${CAMBOX_EGRESS_PACING_SERVICE_NAME} 2>/dev/null)"
echo "PACING_SVC_ACTIVE=\$(systemctl is-active ${CAMBOX_EGRESS_PACING_SERVICE_NAME} 2>/dev/null)"
echo "PACING_SVC_RESTARTS=\$(systemctl show -p NRestarts --value ${CAMBOX_EGRESS_PACING_SERVICE_NAME} 2>/dev/null)"
if [ -x "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" ] && [ "\$(head -n 1 "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" 2>/dev/null)" = "#!/bin/bash" ]; then
  echo "PACING_SCRIPT_EXEC=yes"
else
  echo "PACING_SCRIPT_EXEC=no"
fi
if [ -r "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" ]; then
  echo "PACING_SCRIPT_SHA=\$(cambox_egress_pacing_functional_lines < "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" | sha256sum | awk '{ print \$1 }')"
else
  echo "PACING_SCRIPT_SHA="
fi
if [ -r "${CAMBOX_EGRESS_PACING_SERVICE_PATH}" ]; then
  echo "PACING_UNIT_SHA=\$(cambox_egress_pacing_functional_lines < "${CAMBOX_EGRESS_PACING_SERVICE_PATH}" | sha256sum | awk '{ print \$1 }')"
else
  echo "PACING_UNIT_SHA="
fi
EOF
}

# cambox_egress_pacing_expected_script_sha -> the functional-line sha256 of the boot script
# setup-device writes (this lib's own generator), so a stale installed copy (the rate changed here,
# the box was never re-provisioned) is caught. Empty only when sha256sum is missing on the host.
cambox_egress_pacing_expected_script_sha() {
  cambox_egress_pacing_boot_script | cambox_egress_pacing_functional_lines | sha256sum 2>/dev/null | awk '{ print $1 }' || true
  return 0
}

# cambox_egress_pacing_expected_unit_sha -> the functional-line sha256 of the checked-in unit
# setup-device installs, or nothing when it cannot be read (a caller that sources this lib without
# the systemd/ sibling).
cambox_egress_pacing_expected_unit_sha() {
  [ -r "$CAMBOX_EGRESS_PACING_UNIT_SRC" ] || return 0
  cambox_egress_pacing_functional_lines < "$CAMBOX_EGRESS_PACING_UNIT_SRC" | sha256sum 2>/dev/null | awk '{ print $1 }' || true
  return 0
}

# cambox_egress_pacing_block_field BLOCK KEY -> the value of the FIRST `KEY=` line of BLOCK (empty
# when absent). awk reads the whole block (no early exit), so it is pipefail/SIGPIPE-safe.
cambox_egress_pacing_block_field() {
  printf '%s\n' "${1:-}" | awk -v k="${2:-}=" 'f == 0 && index($0, k) == 1 { print substr($0, length(k) + 1); f = 1 }' || true
  return 0
}

# cambox_egress_pacing_provision_verdict BLOCK [WANT_SCRIPT_SHA] [WANT_UNIT_SHA] -> `ok`, or one
# `FAIL: <what> -- <the fix>` line per failed facet, for a gather block. The expected hashes default to
# this lib's own (cambox_egress_pacing_expected_script_sha / _unit_sha); a caller grading many boxes
# computes them once and passes them.
#
# INSTALL facets (setup-device's job -- the fix is a re-provision):
#   the boot script is executable and byte-equal to what this lib generates (not missing, not stale);
#   the installed unit is byte-equal to the checked-in systemd/ unit;
#   the unit is `enabled` (a hand-applied runtime qdisc alone is exactly the non-permanent state
#   issue 1242 fixes).
# RUNTIME facets (the fix matches the state, never a blanket re-provision):
#   the unit is not `failed` (its retry gave up: read its journal);
#   the live tree (qdisc + filter) grades OK. When it does not, the hint follows the state: no default route =
#   a network problem; tc missing = iproute2; the install fine and the unit `inactive` = it never ran
#   since the install (`systemctl start` or the next reboot); `active` = the qdisc changed after it
#   ran (`systemctl restart`); `activating` = it is still retrying.
# `active` is NOT required: setup-device is enable-only and a cambox is never rebooted remotely, so a
# freshly provisioned box stays `inactive` until a physical reboot while its runtime qdisc is live.
# Fail-closed on an unreadable block or an unreadable expected value. Always returns 0.
cambox_egress_pacing_provision_verdict() {
  local block="${1:-}" want_script want_unit iface qdisc filter enabled active script_exec script_sha unit_sha
  local restarts rounds="" v hint install_ok=1 fails="" nl=$'\n' svc="${CAMBOX_EGRESS_PACING_SERVICE_NAME}"
  local reprov="re-run setup-device.sh (its [egress-pacing] sub-step)"
  if [ "$#" -ge 2 ]; then want_script="$2"; else want_script="$(cambox_egress_pacing_expected_script_sha)"; fi
  if [ "$#" -ge 3 ]; then want_unit="$3"; else want_unit="$(cambox_egress_pacing_expected_unit_sha)"; fi
  iface="$(cambox_egress_pacing_block_field "$block" PACING_IFACE)"
  qdisc="$(cambox_egress_pacing_block_field "$block" PACING_QDISC)"
  filter="$(cambox_egress_pacing_block_field "$block" PACING_FILTER)"
  enabled="$(cambox_egress_pacing_block_field "$block" PACING_SVC_ENABLED | tr -d '[:space:]')"
  active="$(cambox_egress_pacing_block_field "$block" PACING_SVC_ACTIVE | tr -d '[:space:]')"
  script_exec="$(cambox_egress_pacing_block_field "$block" PACING_SCRIPT_EXEC | tr -d '[:space:]')"
  script_sha="$(cambox_egress_pacing_block_field "$block" PACING_SCRIPT_SHA | tr -d '[:space:]')"
  unit_sha="$(cambox_egress_pacing_block_field "$block" PACING_UNIT_SHA | tr -d '[:space:]')"
  restarts="$(cambox_egress_pacing_block_field "$block" PACING_SVC_RESTARTS | tr -d '[:space:]')"
  case "$restarts" in
    '' | *[!0-9]*) ;;
    # NRestarts counts a restart when it runs, after the pause, so during a pause it lags one round.
    *) rounds=" (at least ${restarts} failed round(s) so far)" ;;
  esac

  # --- install facets ---
  if [ "$script_exec" != "yes" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: the boot script ${CAMBOX_EGRESS_PACING_SCRIPT_PATH} is missing or not executable -- $reprov"
  elif [ -z "$want_script" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: cannot compute the expected boot script hash on this host (sha256sum missing?) -- the install cannot be proven"
  elif [ "$script_sha" != "$want_script" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: the boot script ${CAMBOX_EGRESS_PACING_SCRIPT_PATH} is stale (it differs from what scripts/lib/cambox-egress-pacing.sh generates) -- $reprov"
  fi
  if [ -z "$want_unit" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: cannot read the checked-in unit ${CAMBOX_EGRESS_PACING_UNIT_SRC} to compare -- run from a checkout with systemd/ next to scripts/"
  elif [ "$unit_sha" != "$want_unit" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: the installed unit ${CAMBOX_EGRESS_PACING_SERVICE_PATH} differs from the checked-in systemd/${svc}.service (missing or stale) -- $reprov"
  fi
  if [ "$enabled" != "enabled" ]; then
    install_ok=0
    fails="${fails:+$fails$nl}FAIL: ${svc}.service is not enabled (state=${enabled:-<none>}) -- the pacing will not survive a reboot; $reprov"
  fi

  # --- runtime facets ---
  if [ "$active" = "failed" ]; then
    fails="${fails:+$fails$nl}FAIL: ${svc}.service failed -- an apply round failed${rounds}; read journalctl -u ${svc}"
  fi
  v="$(cambox_egress_pacing_verdict "$qdisc" "$filter")"
  case "$v" in
    OK*) hint="" ;;
    *"no default route"*) hint="a network problem, not provisioning: the box has no default route" ;;
    *"not installed"*) hint="install iproute2 (setup-device.sh refuses to run without tc)" ;;
    UNKNOWN*) hint="the qdisc could not be graded" ;;
    *)
      if [ "$install_ok" = 0 ]; then
        # restart, never start: a `start` of an `active` RemainAfterExit unit re-applies nothing.
        hint="fix the install facets, then systemctl restart ${svc}"
      else
        case "$active" in
          inactive) hint="the unit is installed + enabled but has not run since: systemctl start ${svc} (or the next reboot) applies it" ;;
          active) hint="the qdisc changed after the unit applied it: systemctl restart ${svc} re-applies it" ;;
          # A DRIFT read proves the default route exists, so a unit still restarting is failing to
          # apply the qdisc (tc refusing it, e.g. a kernel without sch_fq) or is between rounds.
          activating) hint="a default route exists but the unit has not applied the qdisc${rounds}: tc keeps failing, or it is between rounds (up to 30 s); read journalctl -u ${svc}" ;;
          failed) hint="see the failed unit" ;;
          *) hint="unit state ${active:-<none>}: systemctl start ${svc}" ;;
        esac
      fi
      ;;
  esac
  case "$v" in
    OK*) ;;
    *) fails="${fails:+$fails$nl}FAIL: egress qdisc on ${iface:-<no default route>}: ${v}; fix: ${hint}" ;;
  esac
  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    echo "ok"
  fi
  return 0
}

# cambox_egress_pacing_verdict_oneline VERDICT -> the FAIL lines of a provisioning verdict joined
# with `; ` and the `FAIL: ` prefixes dropped (one log line for verify-device (ap) and the E2E row).
cambox_egress_pacing_verdict_oneline() {
  printf '%s\n' "${1:-}" | awk 'NF { sub(/^FAIL: /, ""); printf "%s%s", (n++ ? "; " : ""), $0 } END { if (n) printf "\n" }' || true
  return 0
}

# cambox_egress_pacing_e2e_report TARGETS PASS [USER] -> the E2E [0/8] REPORT-ONLY row. TARGETS is
# the caller's space-separated `box=ip` list (recording-e2e.sh passes LEG_HEALTH_TARGETS: the source
# box plus every box the fleet preflight vetted, derived from CAMERA_ACTIVE_SET). Per box: an
# operator-acked box (the caller's cambox_offline_ack_is_acked, when defined) is skipped; otherwise
# ONE bounded read-only ssh gather, then `    ok: <box> ...` or a named `    WARNING: <box> ...`
# line; an unreadable box is a WARNING too. A summary line closes the row. Never exits, never fails
# the run: always returns 0 under the caller's `set -euo pipefail`.
cambox_egress_pacing_e2e_report() {
  local targets="${1:-}" pass="${2:-}" user="${3:-root}" pair box ip block v n_ok=0 n_warn=0
  local bound="${CAMBOX_EGRESS_PACING_E2E_TIMEOUT_S:-20}" want_script want_unit
  echo "[0/8] cambox NDI egress pacing -- report-only (issue 1242): fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT} + the JPEG XS line-rate class (issue 1387) on each vetted cambox"
  if [ -z "$targets" ]; then
    echo "    NOTE: no cambox targets -- egress pacing not read (report-only)"
    return 0
  fi
  # The install every box must carry, computed once for the whole row.
  want_script="$(cambox_egress_pacing_expected_script_sha)"
  want_unit="$(cambox_egress_pacing_expected_unit_sha)"
  for pair in $targets; do
    box="${pair%%=*}"
    ip="${pair#*=}"
    if command -v cambox_offline_ack_is_acked >/dev/null 2>&1 && cambox_offline_ack_is_acked "$box"; then
      echo "    skip: $box -- operator-acknowledged offline, egress pacing not read"
      continue
    fi
    block="$(timeout "$bound" sshpass -p "$pass" ssh -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 "$user@$ip" \
      "$(cambox_egress_pacing_gather_remote_snippet)" 2>/dev/null)" || block=""
    if [ -z "$block" ]; then
      echo "    WARNING: $box ($ip) egress pacing UNKNOWN -- could not read the box over ssh (report-only, issue 1242)"
      n_warn=$((n_warn + 1))
      continue
    fi
    v="$(cambox_egress_pacing_provision_verdict "$block" "$want_script" "$want_unit")"
    if [ "$v" = "ok" ]; then
      echo "    ok: $box ($ip) -- $(cambox_egress_pacing_verdict "$(cambox_egress_pacing_block_field "$block" PACING_QDISC)" "$(cambox_egress_pacing_block_field "$block" PACING_FILTER)" | sed 's/^OK //') on $(cambox_egress_pacing_block_field "$block" PACING_IFACE), ${CAMBOX_EGRESS_PACING_SERVICE_NAME}.service installed + enabled"
      n_ok=$((n_ok + 1))
    else
      echo "    WARNING: $box ($ip) egress pacing: $(cambox_egress_pacing_verdict_oneline "$v") -- report-only, the run continues (issue 1242)"
      n_warn=$((n_warn + 1))
    fi
  done
  echo "    egress pacing: $n_ok ok, $n_warn warning (report-only, issue 1242)"
  return 0
}

# cambox_egress_pacing_apply_program -> the REMOTE program (root, `ssh ... bash -s`) that brings a live,
# already-provisioned cambox to this lib's tree WITHOUT a setup-device re-run (issue 1387: every box
# provisioned before it carries the old root-fq boot script, which (ap) names as stale). Everything
# runs inside ONE function called with </dev/null (the program arrives on bash's stdin; the ro-window
# rule's stdin finding). Non-zero (nothing printed) when the checked-in unit cannot be read here.
#   1. read (nothing written): the root mode, whether the boot script and the unit equal what
#      setup-device installs (sha256 of the whole file), whether the unit is enabled;
#   2. the rw window, when something needs a write OR the root does not read `ro` (a cambox runs
#      read-only, so a writable root is the stuck state, put back by the verified close before
#      anything starts): the boot script (0755) and the unit (0644), each a temp file + rename, and
#      `systemctl enable` -- then the ONE verified close (scripts/lib/ro-window.sh, issue 1407). A
#      failure inside reaches the EXIT trap, which runs the same close;
#   3. after the close: daemon-reload, `systemctl restart` of the unit (it runs the boot script: the
#      `tc ... replace` chain, idempotent; replacing the root drops what the OLD root qdisc still had
#      queued, once), then the read-back: `tc qdisc show` + `tc filter show` on the default-route
#      interface graded by THIS lib's verdict (embedded with `declare -f`). Exit 0 only on `OK`.
cambox_egress_pacing_apply_program() {
  local unit
  if ! command -v ro_window_close_cmds >/dev/null 2>&1; then
    # shellcheck source=scripts/lib/ro-window.sh
    . "${BASH_SOURCE[0]%/*}/ro-window.sh"
  fi
  if [ ! -r "$CAMBOX_EGRESS_PACING_UNIT_SRC" ]; then
    echo "FAIL: [issue 1387] cannot read the checked-in unit $CAMBOX_EGRESS_PACING_UNIT_SRC -- run from a checkout with systemd/ next to scripts/" >&2
    return 1
  fi
  unit="$(cat "$CAMBOX_EGRESS_PACING_UNIT_SRC")"
  cat <<'PROG'
# scripts/lib/cambox-egress-pacing.sh (issues 1242, 1387): the egress tree of this cambox.
_cep_main() {
set -eu
PROG
  printf '_cep_svc=%q\n' "$CAMBOX_EGRESS_PACING_SERVICE_NAME"
  printf '_cep_sp=%q\n' "$CAMBOX_EGRESS_PACING_SCRIPT_PATH"
  printf '_cep_up=%q\n' "$CAMBOX_EGRESS_PACING_SERVICE_PATH"
  printf '_cep_script=%q\n' "$(cambox_egress_pacing_boot_script)"
  printf '_cep_unit=%q\n' "$unit"
  local v
  for v in CAMBOX_EGRESS_PACING_RATE CAMBOX_EGRESS_PACING_FLOW_LIMIT CAMBOX_EGRESS_PACING_LIMIT \
    CAMBOX_EGRESS_PACING_JXS_MARK CAMBOX_EGRESS_PACING_JXS_FIFO_LIMIT CAMBOX_EGRESS_PACING_PRIOMAP; do
    printf '%s=%q\n' "$v" "${!v}"
  done
  printf '%s\n' "$(declare -f ro_root_mount_mode cambox_egress_pacing_rate_bps cambox_egress_pacing_qdisc_line \
    cambox_egress_pacing_token cambox_egress_pacing_number cambox_egress_pacing_verdict)"
  cat <<'PROG'
_cep_box="$(hostname 2>/dev/null || echo this cambox)"
_cep_sha() { if [ -e "$1" ]; then _cep_s="$(sha256sum <"$1")"; echo "${_cep_s%% *}"; else echo __ABSENT__; fi; }
_cep_want() { _cep_s="$(printf '%s\n' "$1" | sha256sum)"; echo "${_cep_s%% *}"; }
_cep_put() {  # PATH MODE CONTENT: a temp file + rename
  mkdir -p "${1%/*}" && printf '%s\n' "$3" >"$1.new" && chmod "$2" "$1.new" && mv -f "$1.new" "$1"
}
# --- 1. what this box needs (read before anything is written) ---
_cep_mode="$(ro_root_mount_mode "$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)")"
_cep_do_s=0
[ "$(_cep_sha "$_cep_sp")" = "$(_cep_want "$_cep_script")" ] || _cep_do_s=1
_cep_do_u=0
[ "$(_cep_sha "$_cep_up")" = "$(_cep_want "$_cep_unit")" ] || _cep_do_u=1
_cep_do_e=0
[ "$(systemctl is-enabled "$_cep_svc" 2>/dev/null || true)" = enabled ] || _cep_do_e=1
echo "PLAN ($_cep_box): root $_cep_mode; boot script $([ "$_cep_do_s" = 1 ] && echo write || echo in-place); unit $([ "$_cep_do_u" = 1 ] && echo write || echo in-place); $([ "$_cep_do_e" = 1 ] && echo enable || echo enabled)"
_cep_rc=0
# --- 2. the rw window, closed by the ONE verified close ---
_cep_open=0
_cep_close() {
  [ "$_cep_open" = 1 ] || return 0
  _cep_open=0
PROG
  ro_window_close_cmds "issue 1387" "\$_cep_box" \
    "The new boot script is on disk, but the root stays read-WRITE, so the egress tree is not applied." \
    "stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run the cambox egress pacing apply for this box (never reboot a cambox remotely)."
  cat <<'PROG'
}
trap '_cep_close' EXIT
if [ "$_cep_do_s" = 1 ] || [ "$_cep_do_u" = 1 ] || [ "$_cep_do_e" = 1 ] || [ "$_cep_mode" != ro ]; then
  if ! mount -o remount,rw /; then
    echo "FAIL: [issue 1387] could not remount / read-write on $_cep_box -- nothing was written" >&2
    exit 1
  fi
  _cep_open=1
  if [ "$_cep_do_s" = 1 ] && ! _cep_put "$_cep_sp" 0755 "$_cep_script"; then
    rm -f "$_cep_sp.new"
    echo "FAIL: [issue 1387] could not write $_cep_sp on $_cep_box -- the window is closed again by the verified close" >&2
    exit 1
  fi
  if [ "$_cep_do_u" = 1 ] && ! _cep_put "$_cep_up" 0644 "$_cep_unit"; then
    rm -f "$_cep_up.new"
    echo "FAIL: [issue 1387] could not write $_cep_up on $_cep_box -- the window is closed again by the verified close" >&2
    exit 1
  fi
  if [ "$_cep_do_e" = 1 ] && ! systemctl enable "$_cep_svc"; then
    echo "FAIL: [issue 1387] systemctl enable $_cep_svc failed on $_cep_box -- the window is closed again by the verified close" >&2
    exit 1
  fi
fi
_cep_close
trap - EXIT
# --- 3. after the verified close: reload, apply, read back ---
if ! systemctl daemon-reload; then
  echo "FAIL: [issue 1387] systemctl daemon-reload failed on $_cep_box" >&2
  _cep_rc=1
fi
if ! systemctl restart "$_cep_svc"; then
  echo "FAIL: [issue 1387] $_cep_svc did not apply the tree on $_cep_box -- read: journalctl -u $_cep_svc -n 20" >&2
  _cep_rc=1
fi
_cep_if="$(ip route show default 2>/dev/null | awk '!d { for (i = 1; i < NF; i++) if ($i == "dev") { d = $(i + 1); break } } END { if (d != "") print d }')"
if [ -z "$_cep_if" ]; then
  echo "FAIL: [issue 1387] no default route on $_cep_box -- the egress tree cannot be read back" >&2
  return 1
fi
_cep_q="$(tc qdisc show dev "$_cep_if" 2>&1 || true)"
_cep_f="$(tc filter show dev "$_cep_if" 2>&1 || true)"
echo "TREE ($_cep_box, $_cep_if):"
printf '%s\n%s\n' "$_cep_q" "$_cep_f"
_cep_v="$(cambox_egress_pacing_verdict "$_cep_q" "$_cep_f")"
case "$_cep_v" in
  OK*) echo "READBACK ($_cep_box): $_cep_v" ;;
  *)
    echo "FAIL: [issue 1387] READBACK ($_cep_box): $_cep_v" >&2
    _cep_rc=1
    ;;
esac
return "$_cep_rc"
}
_cep_main </dev/null
PROG
}
