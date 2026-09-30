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
# reach strih-lx at the 5 Gb/s line rate in ~3-4 ms and its RTL8157 USB NIC answers with PAUSE
# storms (issue 1242 finding 5902816575). The owner approved staggered sending (5904641502).
# Delaying the frame hand-off failed live twice because the NDI SDK encodes inside the send, so the
# bursts are spread by pacing each cambox's KERNEL egress instead (decision 5904673375): an `fq`
# root qdisc with a per-flow `maxrate` sends an already-encoded frame at no more than 400 Mb/s
# (~7 ms for a 300-350 KB frame). `fq` paces per FLOW, so dantesync's PTP/NTP packets and the
# intercom are their own flows and are never queued behind a video frame (a single `tbf` bucket
# would delay them and bias the clock). `flow_limit` 2000 holds a whole frame (~240 packets).
# Measured with it live on cam1-cam7 (result 5905948208): strih-lx PAUSE 39-110/min -> 28-36/min,
# receive-gap storms from ~1 per 5 min to ~1 per 30 min, the first release E2E with clean
# continuity. It helps but is not the full cure; 10 GbE through a PCIe NIC (issue 1387) is.
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

# cambox_egress_pacing_apply_cmd -> shell TEXT: ONE attempt to pace the default-route interface
# (`tc qdisc replace ... root fq maxrate ... flow_limit ... limit ...`, idempotent). A brace group
# whose status is the attempt's result: 0 = applied (one line on stdout), 1 = no default route yet
# or tc failed (one named line on stderr, tc never run without a route). The boot script wraps it in
# its bounded retry; nothing else applies it (setup-device is enable-only).
cambox_egress_pacing_apply_cmd() {
  cat <<EOF
{
  _cep_if="\$($(cambox_egress_pacing_iface_cmd))"
  if [ -z "\$_cep_if" ]; then
    echo "cambox-egress-pacing: no default route yet -- no egress interface to pace (issue 1242)" >&2
    false
  elif tc qdisc replace dev "\$_cep_if" root fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT}; then
    echo "cambox-egress-pacing: dev \$_cep_if root fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT} applied (issue 1242)"
  else
    echo "cambox-egress-pacing: tc qdisc replace on \$_cep_if FAILED (issue 1242)" >&2
    false
  fi
}
EOF
}

# cambox_egress_pacing_boot_script -> the full content of ${CAMBOX_EGRESS_PACING_SCRIPT_PATH}: the
# apply command above (verbatim, the one source) inside a bounded retry. Exit 0 once applied; after
# the last attempt a loud FAILED line on stderr (the journal) and exit 1, so the unit reads `failed`
# and verify-device (ap) names it -- never a silent unpaced box.
# CAMBOX_EGRESS_PACING_RETRY_SLEEP_S in the unit's environment overrides the sleep (tests only).
cambox_egress_pacing_boot_script() {
  cat <<EOF
#!/bin/bash
# ${CAMBOX_EGRESS_PACING_SERVICE_NAME} boot script -- GENERATED by scripts/lib/cambox-egress-pacing.sh
# (camera-box issue 1242) and written by setup-device.sh; do not edit it on the box, re-run setup-device.sh.
# Run at boot by ${CAMBOX_EGRESS_PACING_SERVICE_NAME}.service: paces this box's NDI egress with an fq
# root qdisc (maxrate ${CAMBOX_EGRESS_PACING_RATE} per flow) on the default-route interface, so the
# seven cameras' per-tick frame bursts reach strih-lx spread in time.
set -euo pipefail
PATH="\$PATH:/usr/sbin:/sbin"
cep_attempts=${CAMBOX_EGRESS_PACING_RETRY_ATTEMPTS}
cep_sleep="\${CAMBOX_EGRESS_PACING_RETRY_SLEEP_S:-${CAMBOX_EGRESS_PACING_RETRY_SLEEP_S}}"
cep_apply()
$(cambox_egress_pacing_apply_cmd)
cep_n=0
while [ "\$cep_n" -lt "\$cep_attempts" ]; do
  cep_n=\$((cep_n + 1))
  if cep_apply; then
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

# cambox_egress_pacing_verdict QDISC_TEXT -> ONE line `OK <detail>` / `DRIFT <detail>` /
# `UNKNOWN <detail>` for a `tc qdisc show dev <if>` output (raw lines, or the `|`-flattened form the
# gather snippet emits). Only the ROOT qdisc is graded (a child `parent :N` line never is). OK = an fq
# root whose maxrate (compared in bit/s), flow_limit and limit all equal the declaration. DRIFT = a
# readable root that is not that (pfifo_fast / mq / fq_codel, fq without maxrate, a wrong value).
# UNKNOWN = nothing gradable (tc absent, no default route, no root line). Always returns 0.
cambox_egress_pacing_verdict() {
  local text="${1:-}" root kind maxrate flow_limit limit got_bps want_bps drift=""
  case "$text" in
    *__TC_ABSENT__*)
      echo "UNKNOWN tc (iproute2) is not installed -- the egress qdisc cannot be read"
      return 0 ;;
    *__NO_DEFAULT_ROUTE__*)
      echo "UNKNOWN no default route -- no egress interface to read"
      return 0 ;;
  esac
  root="$(printf '%s\n' "$text" | tr '|' '\n' | awk '
    $1 == "qdisc" && r == "" {
      for (i = 3; i <= NF; i++) {
        if ($i == "parent") break
        if ($i == "root") { r = $0; break }
      }
    }
    END { if (r != "") print r }' || true)"
  if [ -z "$root" ]; then
    echo "UNKNOWN no root qdisc in the tc output"
    return 0
  fi
  kind="$(printf '%s\n' "$root" | awk '{ print $2 }' || true)"
  if [ "$kind" != "fq" ]; then
    echo "DRIFT root qdisc is ${kind:-?}, not fq -- the NDI egress is UNPACED"
    return 0
  fi
  # Exact tokens: `limit` and `flow_limit` are distinct words, and the `p` packet suffix is stripped.
  maxrate="$(printf '%s\n' "$root" | awk '{ for (i = 1; i < NF; i++) if ($i == "maxrate") { print $(i + 1); break } }' || true)"
  flow_limit="$(printf '%s\n' "$root" | awk '{ for (i = 1; i < NF; i++) if ($i == "flow_limit") { v = $(i + 1); sub(/p$/, "", v); print v; break } }' || true)"
  limit="$(printf '%s\n' "$root" | awk '{ for (i = 1; i < NF; i++) if ($i == "limit") { v = $(i + 1); sub(/p$/, "", v); print v; break } }' || true)"
  if [ -z "$maxrate" ]; then
    echo "DRIFT fq root without maxrate -- the NDI egress is UNPACED"
    return 0
  fi
  got_bps="$(cambox_egress_pacing_rate_bps "$maxrate")"
  want_bps="$(cambox_egress_pacing_rate_bps "$CAMBOX_EGRESS_PACING_RATE")"
  if [ -z "$got_bps" ] || [ "$got_bps" != "$want_bps" ]; then
    drift="maxrate $maxrate (want $CAMBOX_EGRESS_PACING_RATE)"
  fi
  if [ "$flow_limit" != "$CAMBOX_EGRESS_PACING_FLOW_LIMIT" ]; then
    drift="${drift:+$drift, }flow_limit ${flow_limit:-?} (want $CAMBOX_EGRESS_PACING_FLOW_LIMIT)"
  fi
  if [ "$limit" != "$CAMBOX_EGRESS_PACING_LIMIT" ]; then
    drift="${drift:+$drift, }limit ${limit:-?} (want $CAMBOX_EGRESS_PACING_LIMIT)"
  fi
  if [ -n "$drift" ]; then
    echo "DRIFT fq root with $drift"
  else
    echo "OK fq maxrate $maxrate flow_limit ${flow_limit}p limit ${limit}p"
  fi
  return 0
}

# cambox_egress_pacing_gather_remote_snippet -> READ-ONLY remote bash (verify-device (ap) and the E2E
# row run it over ssh) printing one KEY=VALUE line per fact: the default-route interface, its qdisc
# (`|`-flattened, or __TC_ABSENT__ / __NO_DEFAULT_ROUTE__), the unit's is-enabled / is-active, and
# what setup-device installed -- the boot script's executable bit + sha256 and the unit's sha256
# (empty when absent), so the verdict can prove the unit will actually run at the next boot. It
# never changes anything (only `tc qdisc show` and reads). sbin is appended to PATH because a
# non-login ssh PATH may omit it; a caller's own PATH entries still come first.
cambox_egress_pacing_gather_remote_snippet() {
  cat <<EOF
PATH="\$PATH:/usr/sbin:/sbin"
_cep_if="\$($(cambox_egress_pacing_iface_cmd))"
echo "PACING_IFACE=\$_cep_if"
if ! command -v tc >/dev/null 2>&1; then
  echo "PACING_QDISC=__TC_ABSENT__"
elif [ -z "\$_cep_if" ]; then
  echo "PACING_QDISC=__NO_DEFAULT_ROUTE__"
else
  echo "PACING_QDISC=\$(tc qdisc show dev "\$_cep_if" 2>/dev/null | tr '\n' '|')"
fi
echo "PACING_SVC_ENABLED=\$(systemctl is-enabled ${CAMBOX_EGRESS_PACING_SERVICE_NAME} 2>/dev/null)"
echo "PACING_SVC_ACTIVE=\$(systemctl is-active ${CAMBOX_EGRESS_PACING_SERVICE_NAME} 2>/dev/null)"
if [ -x "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" ]; then echo "PACING_SCRIPT_EXEC=yes"; else echo "PACING_SCRIPT_EXEC=no"; fi
echo "PACING_SCRIPT_SHA=\$(sha256sum "${CAMBOX_EGRESS_PACING_SCRIPT_PATH}" 2>/dev/null | awk '{ print \$1 }')"
echo "PACING_UNIT_SHA=\$(sha256sum "${CAMBOX_EGRESS_PACING_SERVICE_PATH}" 2>/dev/null | awk '{ print \$1 }')"
EOF
}

# cambox_egress_pacing_expected_script_sha -> the sha256 of the boot script setup-device writes
# (this lib's own generator), so a stale installed copy (the rate changed here, the box was never
# re-provisioned) is caught. Empty only when sha256sum is missing on the reading host.
cambox_egress_pacing_expected_script_sha() {
  cambox_egress_pacing_boot_script | sha256sum 2>/dev/null | awk '{ print $1 }' || true
  return 0
}

# cambox_egress_pacing_expected_unit_sha -> the sha256 of the checked-in unit setup-device installs,
# or nothing when it cannot be read (a caller that sources this lib without the systemd/ sibling).
cambox_egress_pacing_expected_unit_sha() {
  [ -r "$CAMBOX_EGRESS_PACING_UNIT_SRC" ] || return 0
  sha256sum "$CAMBOX_EGRESS_PACING_UNIT_SRC" 2>/dev/null | awk '{ print $1 }' || true
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
#   the live root qdisc grades OK. When it does not, the hint follows the state: no default route =
#   a network problem; tc missing = iproute2; the install fine and the unit `inactive` = it never ran
#   since the install (`systemctl start` or the next reboot); `active` = the qdisc changed after it
#   ran (`systemctl restart`); `activating` = it is still retrying.
# `active` is NOT required: setup-device is enable-only and a cambox is never rebooted remotely, so a
# freshly provisioned box stays `inactive` until a physical reboot while its runtime qdisc is live.
# Fail-closed on an unreadable block or an unreadable expected value. Always returns 0.
cambox_egress_pacing_provision_verdict() {
  local block="${1:-}" want_script want_unit iface qdisc enabled active script_exec script_sha unit_sha
  local v hint install_ok=1 fails="" nl=$'\n' svc="${CAMBOX_EGRESS_PACING_SERVICE_NAME}"
  local reprov="re-run setup-device.sh (its [egress-pacing] sub-step)"
  if [ "$#" -ge 2 ]; then want_script="$2"; else want_script="$(cambox_egress_pacing_expected_script_sha)"; fi
  if [ "$#" -ge 3 ]; then want_unit="$3"; else want_unit="$(cambox_egress_pacing_expected_unit_sha)"; fi
  iface="$(cambox_egress_pacing_block_field "$block" PACING_IFACE)"
  qdisc="$(cambox_egress_pacing_block_field "$block" PACING_QDISC)"
  enabled="$(cambox_egress_pacing_block_field "$block" PACING_SVC_ENABLED | tr -d '[:space:]')"
  active="$(cambox_egress_pacing_block_field "$block" PACING_SVC_ACTIVE | tr -d '[:space:]')"
  script_exec="$(cambox_egress_pacing_block_field "$block" PACING_SCRIPT_EXEC | tr -d '[:space:]')"
  script_sha="$(cambox_egress_pacing_block_field "$block" PACING_SCRIPT_SHA | tr -d '[:space:]')"
  unit_sha="$(cambox_egress_pacing_block_field "$block" PACING_UNIT_SHA | tr -d '[:space:]')"

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
    fails="${fails:+$fails$nl}FAIL: ${svc}.service failed -- its apply gave up; read journalctl -u ${svc}"
  fi
  v="$(cambox_egress_pacing_verdict "$qdisc")"
  case "$v" in
    OK*) hint="" ;;
    *"no default route"*) hint="a network problem, not provisioning: the box has no default route" ;;
    *"not installed"*) hint="install iproute2 (setup-device.sh does)" ;;
    UNKNOWN*) hint="the qdisc could not be graded" ;;
    *)
      if [ "$install_ok" = 0 ]; then
        hint="fix the install facets, then systemctl start ${svc}"
      else
        case "$active" in
          inactive) hint="the unit is installed + enabled but has not run since: systemctl start ${svc} (or the next reboot) applies it" ;;
          active) hint="the qdisc changed after the unit applied it: systemctl restart ${svc} re-applies it" ;;
          activating) hint="the unit is still retrying (no default route yet?)" ;;
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
  echo "[0/8] cambox NDI egress pacing -- report-only (issue 1242): fq maxrate ${CAMBOX_EGRESS_PACING_RATE} flow_limit ${CAMBOX_EGRESS_PACING_FLOW_LIMIT} limit ${CAMBOX_EGRESS_PACING_LIMIT} on each vetted cambox"
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
      echo "    ok: $box ($ip) -- $(cambox_egress_pacing_verdict "$(cambox_egress_pacing_block_field "$block" PACING_QDISC)" | sed 's/^OK //') on $(cambox_egress_pacing_block_field "$block" PACING_IFACE), ${CAMBOX_EGRESS_PACING_SERVICE_NAME}.service installed + enabled"
      n_ok=$((n_ok + 1))
    else
      echo "    WARNING: $box ($ip) egress pacing: $(cambox_egress_pacing_verdict_oneline "$v") -- report-only, the run continues (issue 1242)"
      n_warn=$((n_warn + 1))
    fi
  done
  echo "    egress pacing: $n_ok ok, $n_warn warning (report-only, issue 1242)"
  return 0
}
