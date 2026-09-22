#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time) — matches the
# scripts/lib/*.sh convention (cambox-offline-ack.sh / imag-leg-marker.sh) of deliberately NOT
# setting `set -euo pipefail` here: sourcing executes in the CALLER's shell, so strict mode here
# would leak into whichever caller sources it. scripts/recording-e2e.sh (the only caller) already
# sets -euo pipefail itself.
#
# scripts/lib/imag-offline-ack.sh — issue 1013: imag-nb's OFFLINE-ACK leg-skip note.
#
# WHY: imag-nb (10.77.9.182) is woven through ~12 hard-abort steps of recording-e2e.sh; when the
# notebook is a KNOWN-ABSENT box (physically taken away after an event) the whole gate used to die
# at minute 0 in the [0/8] reachability preflight — imag had no ack path, so an absent imag was
# indistinguishable from a broken one. The cam-box fleet already has the EXACT mechanism for a
# known-absent box (CAMBOX_OFFLINE_ACK / rig-fleet.txt via scripts/lib/cambox-offline-ack.sh,
# #758/#827); imag is now wired into it as just another "box" name. When imag is acked-offline the
# recording-e2e.sh gate sets IMAG_OFFLINE_ACKED=1 and SKIPS every imag step — but a skipped leg
# must NEVER read back as a hidden pass ("ONE full test, no partials", #798), so each skip emits
# this loud, distinct, greppable report-only NOTE naming the step and the operator's own reason.
#
# This is the imag-leg twin of cambox_offline_ack_note (the cam-box preflight NOTE) and
# imag_leg_marker.sh's IMAG-LEG-NOT-VERIFIED marker (the #798 extract twin): the note fires at each
# SKIPPED step, the marker fires ONCE at [8/8c] with the same acked reason.

# imag_leg_skip_note LABEL REASON -> ONE distinct, greppable run-log line for a SKIPPED imag step.
# Pure (a single printf) — no network, no mutation; safe to unit-test by sourcing + calling.
#   LABEL  — the harness step being skipped (e.g. "[4d/8] render-budget gate").
#   REASON — the operator-acked reason string (from CAMBOX_OFFLINE_ACK, via
#            cambox_offline_ack_reason "imag"); an empty reason degrades to "unspecified".
imag_leg_skip_note() {
  local label="${1:-}" reason="${2:-}"
  printf 'IMAG-LEG-SKIPPED: %s — imag acked offline (%s); the imag leg is SKIPPED this run (report-only, issue 1013). A green run that skips the imag leg is a NAMED partial, never a silent pass (ONE full test, no partials, issue 798).\n' \
    "$label" "${reason:-unspecified}"
}

# imag_service_reachable HOST -> exit 0 (reachable) iff a SERVICE answers on HOST, exit 1 otherwise.
# This is the STALE-ACK liveness probe for the issue-1013 offline-ack (issue 1317). It replaces the
# old bare `ping -c1 -W2` used by rig-mode.sh's two sites (resolve_imag_offline_leg /
# require_imag_genlock_current) and by recording-e2e.sh's [0/8] imag-acked branch. WHY not ping:
# imag-nb (10.77.9.182) is OUT of the rig ~1 year (owner 20.9.2026 — its USB ethernet dongle now
# carries strih-lx at .202), yet on the venue LAN a router / proxy-ARP / foreign responder still
# answers ICMP for .182 (ip neigh INCOMPLETE, erratic RTT) while the box's own services are dead.
# ICMP therefore proves only "some responder holds the IP", not "imag-nb's services are back", so a
# ping-based staleness probe rejects a legitimate ack as STALE and hard-blocks the whole gate. A
# SERVICE probe cannot be fooled by a bare responder: reachable iff ssh :22 accepts a TCP connect
# OR dantesync :8898/status answers (2 s timeout each). Trade-off (accepted): a box whose sshd AND
# dantesync are both down but which is genuinely present reads "unreachable" and lets a stale ack
# stand — such a box cannot pass the genlock gate anyway, and the ack is an explicit operator
# statement with a reason.
#
# Tier-0 seams (a fixture command whose EXIT CODE stands in for the real probe, so the whole
# predicate is hermetically testable with no network):
#   IMAG_REACH_SSH_PROBE_CMD   -- overrides the ssh :22 TCP probe (exit 0 = ssh reachable).
#   IMAG_REACH_HTTP_PROBE_CMD  -- overrides the dantesync :8898/status probe (exit 0 = http answers).
# Called only inside an `if` condition (both rig-mode.sh sites + the recording-e2e.sh mirror), so
# the caller's `set -euo pipefail` is disabled for this function body (bash condition-context rule)
# and a failing probe never aborts the caller.
imag_service_reachable() {
  local host="${1:-}"
  [ -n "$host" ] || return 1
  # ssh :22 service probe (or its Tier-0 fixture) — NEVER a bare ICMP ping.
  if [ -n "${IMAG_REACH_SSH_PROBE_CMD:-}" ]; then
    bash -c "${IMAG_REACH_SSH_PROBE_CMD}" >/dev/null 2>&1 && return 0
  elif timeout 2 bash -c "exec 3<>/dev/tcp/${host}/22" >/dev/null 2>&1; then
    return 0
  fi
  # dantesync :8898/status service probe (or its Tier-0 fixture).
  if [ -n "${IMAG_REACH_HTTP_PROBE_CMD:-}" ]; then
    bash -c "${IMAG_REACH_HTTP_PROBE_CMD}" >/dev/null 2>&1 && return 0
  elif curl -fsS --max-time 2 "http://${host}:8898/status" >/dev/null 2>&1; then
    return 0
  fi
  return 1
}
