#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time) -- mirrors
# scripts/lib/cambox-offline-ack.sh / preflight-fleet-check.sh convention of deliberately NOT
# setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so
# imposing strict mode here would leak into whichever caller sources it. scripts/recording-e2e.sh
# (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/startup-self-heal.sh -- the PURE decision for #878's STARTUP restoration step.
#
# WHY (#878, and the SAME family as #844/#869/#872): recording-e2e.sh only ever gives rig state
# back inside cleanup(), the bash EXIT trap -- structurally unreachable on SIGKILL, which
# full-path-e2e.yml's `cancel-in-progress: true` concurrency group makes a ROUTINE event (any push
# to `dev` cancels an in-flight hardware run). A dead harness strands whatever it took over --
# camera-box.service on every cambox the ALL_CAMBOX sweep stopped, the permanent cam2-painter, and
# a leaked genlock_burn (#844) -- and the NEXT run then fails at [0/8] preflight on a leftover
# precondition instead of a measurement. Live evidence 2026-07-30: four consecutive runs died this
# way, `SERVICE_ACTIVE=inactive` on cam2/cam3/cam4, ten seconds apart (the ALL_CAMBOX sweep walking
# the fleet, not independent failures).
#
# This function decides ONE thing: given the SAME durable "a harness entered a test state and did
# not clean up" evidence rig-restore-watchdog.sh already trusts as its PRIMARY stranded signal
# (scripts/lib/rig-heartbeat.sh's rig_e2e_marker_present(), #353 -- written on entry, cleared ONLY
# by cleanup()'s own clean exit), should recording-e2e.sh's OWN startup repair itself before the
# fleet preflight below asserts anything?
#
# Deliberately narrow and NEVER a proxy for "is the box currently unhealthy" -- that decision stays
# entirely with the existing [0/8] fleet preflight (scripts/lib/preflight-fleet-check.sh), whose
# pass/fail policy this file does not touch and does not weaken. #878 left the preflight's own
# self-heal-vs-hard-fail policy for an UNPROVEN inactive box as an OPEN question for the user to
# decide -- this function only ever acts on POSITIVE evidence (the marker), never on a guess, so it
# cannot be read as resolving that question either way.
#
# Source-only: pure decision + message functions, no I/O, no ssh -- mirrors every other
# `*_decide`/`*_verdict` function in this codebase (cambox_offline_ack_decide, rig_restore_decide,
# preflight_fleet_check_verdict).

# startup_self_heal_decision MARKER_PRESENT -> "repair" | "skip" (stdout, pure, no I/O).
#   MARKER_PRESENT=1   -> "repair": rig_e2e_marker is present -- POSITIVE evidence a previous run
#                         of THIS harness entered a test state and never reached cleanup().
#   MARKER_PRESENT=0   -> "skip": no marker -- no evidence this harness owns any inactive state.
#   anything else      -> "skip": unrecognized/ambiguous input is NEVER read as "repair" -- see
#                         startup_self_heal_reason for the accompanying loud log line. A caller
#                         must never silently paper over an ambiguous evidence value.
startup_self_heal_decision() {
  case "${1:-}" in
    1) echo "repair" ;;
    0) echo "skip" ;;
    *) echo "skip" ;;
  esac
}

# startup_self_heal_reason MARKER_PRESENT -> the human-readable line logged alongside the decision
# above (pure string formatting, no I/O). Kept as a SEPARATE function from the decision itself so
# the ambiguous-input case gets its OWN honest message ("unrecognized ... ambiguous") rather than
# being silently folded into the same text as the clean no-evidence case.
startup_self_heal_reason() {
  case "${1:-}" in
    1)
      echo "rig_e2e_marker present (#353) -- a previous run entered a test state and did not clean up; this harness owns the leftover state"
      ;;
    0)
      echo "no rig_e2e_marker -- no evidence this harness owns any inactive state; leaving the fleet preflight's own checks to fail loud on anything genuinely wrong"
      ;;
    *)
      echo "unrecognized marker evidence '${1:-}' -- ambiguous, conservatively skipping repair rather than papering over it"
      ;;
  esac
}

# ── thin reuse wrappers -- NEVER call the two helpers below by their own bare name from
# recording-e2e.sh's startup-self-heal block ────────────────────────────────────────────────────
#
# Both `camera_box_verify_active_cmds` (scripts/lib/camera-box-restart-verify.sh, #675/#684) and
# `cam2_painter_restore_verify_cmds` (scripts/lib/cam2-painter-restore-verify.sh, #863) are exactly
# the primitives this repair step should reuse -- but several EXISTING static-anchor tests
# (tests/harness_recording_e2e_cleanup_verifies_restart_675.rs,
# tests/harness_recording_e2e_cleanup_final_verify_684.rs,
# tests/harness_cam2_painter_provisioning_863.rs) locate cleanup()'s OWN calls to these two
# helpers via a plain textual `.find()` -- some bounded to a region, one entirely UNBOUNDED over
# the whole file. A second bare occurrence of either helper's call text earlier in the file (i.e.
# THIS startup step, which the harness runs before cleanup() is even defined) shadows the real one
# and breaks those tests (the #832 anchor-collision class documented in the project CLAUDE.md,
# reproduced live while building this fix). Wrapping each in its own distinctly-named function
# means recording-e2e.sh's own source text never contains a second literal occurrence of
# `camera_box_verify_active_cmds` / `$(cam2_painter_restore_verify_cmds)` -- the real helper is
# still what actually runs, just invoked one level of indirection away from the text those tests
# scan.

# startup_self_heal_cambox_verify_cmds LABEL -> REMOTE bash, thin pass-through to
# camera_box_verify_active_cmds LABEL (#675/#684) -- STANDALONE use (no preceding stop/pkill/rm),
# exactly like the existing #684 FINAL verify pass: idempotent + cheap on an already-healthy box.
startup_self_heal_cambox_verify_cmds() {
  camera_box_verify_active_cmds "$1"
}

# startup_self_heal_painter_verify_cmds -> REMOTE bash, thin pass-through to
# cam2_painter_restore_verify_cmds (#863) -- WARN-only, never exits, guarded no-op on a box
# without the unit installed (same contract as the real helper).
startup_self_heal_painter_verify_cmds() {
  cam2_painter_restore_verify_cmds
}
