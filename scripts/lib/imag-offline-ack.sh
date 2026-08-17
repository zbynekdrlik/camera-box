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
