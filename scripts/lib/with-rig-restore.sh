#!/usr/bin/env bash
# scripts/lib/with-rig-restore.sh — generic self-restoring rig-step wrapper. Refs #281.
#
# Runs a step command with HUP/INT/TERM signal traps that invoke a restore command when the step
# exits non-zero or the wrapper shell receives a kill signal — so an ad-hoc rig step (deploy,
# probe, scene-change) CANNOT leave the rig stranded even if it dies or is interrupted.
#
# WHY (#281): recording-e2e.sh has a cleanup() trap that restores the rig on EXIT/HUP/INT/TERM.
# One-off manual steps (quick deploy, a single probe run, an MCP-driven scene change) have no
# such safety net: if they die mid-step the rig silently stays in TEST/BURN state. This wrapper
# adds that net generically, without an ssh to a live rig, so any caller can use it.
#
# Source-only: this file defines with_rig_restore() and runs nothing on its own.
#
# Usage:
#   . scripts/lib/with-rig-restore.sh
#   with_rig_restore [--on-failure] <restore_cmd> -- <step_cmd> [args...]
#
# Flags:
#   --on-failure   Run restore ONLY when the step exits non-zero or the wrapper is killed.
#                  Default (flag omitted): always run restore, even on clean success.
#
# Guarantees:
#   - Restore runs exactly once: the _wr_done flag prevents a double-fire when both the
#     exit-code path and a signal trap are triggered in the same invocation.
#   - Step's exit code is preserved and returned to the caller.
#   - restore_cmd is eval'd so it may be a compound shell command (pipes, variables, etc.).
#   - Pure shell — no rig access, no ssh; safe to unit-test locally.
#
# Limitation: nested calls to with_rig_restore (one wrapper inside another) are not
# supported — the inner call overwrites the shell-level _wr_restore_once function and the
# _wr_done local, which breaks the outer wrapper's idempotency guard.

# with_rig_restore [--on-failure] <restore_cmd> -- <step_cmd> [args...]
with_rig_restore() {
  local _wr_on_failure=0
  if [ "${1:-}" = "--on-failure" ]; then
    _wr_on_failure=1
    shift
  fi

  local _wr_restore_cmd="${1:?with_rig_restore: restore_cmd is required}"
  shift
  [ "${1:-}" = "--" ] && shift

  # Idempotent guard: restore runs at most once regardless of how many paths trigger it.
  local _wr_done=0

  # _wr_restore_once is a shell-level function so signal trap strings can reference it by name.
  # It reads _wr_done and _wr_restore_cmd from the enclosing with_rig_restore call frame (bash
  # local scoping makes them accessible while with_rig_restore is on the call stack).
  _wr_restore_once() {
    [ "$_wr_done" -eq 1 ] && return 0
    _wr_done=1
    eval "$_wr_restore_cmd"
  }

  # Arm signal traps: restore, disarm all three traps, then re-raise the signal so the
  # caller sees the process die from the signal (preserves standard Unix semantics).
  # shellcheck disable=SC2016  # single-quoted: $$ is intentionally deferred to trap-fire time
  trap '_wr_restore_once; trap - HUP INT TERM; kill -HUP  "$$"' HUP
  trap '_wr_restore_once; trap - HUP INT TERM; kill -INT  "$$"' INT
  trap '_wr_restore_once; trap - HUP INT TERM; kill -TERM "$$"' TERM

  # Run the step; capture exit code without letting set -e abort the wrapper.
  local _wr_rc=0
  "$@" || _wr_rc=$?

  # Disarm signal traps on the normal (success or failure) exit path.
  trap - HUP INT TERM

  # Decide whether to restore based on mode and step exit code.
  if [ "$_wr_on_failure" -eq 0 ] || [ "$_wr_rc" -ne 0 ]; then
    _wr_restore_once
  fi

  return "$_wr_rc"
}
