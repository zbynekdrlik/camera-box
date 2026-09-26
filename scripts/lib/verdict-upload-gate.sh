#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one pure function, no top-level statements) — the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, and every caller (recording-verdict-on-imag.sh / -on-strih-lx.sh /
# -on-resolume.sh) already sets it itself.
#
# scripts/lib/verdict-upload-gate.sh — the ONE issue-1118 sha256 VERSION GATE for deploying the
# recording-verdict binary to a box before its on-box `--extract-partial` decode. Before issue 1302
# this decision lived as two identical copies (onimag_upload_decision / onstrihlx_upload_decision);
# both now delegate here, and the Windows RESOLUME-SNV extract (recording-verdict-on-resolume.sh)
# uses it too, so the three boxes can never drift apart on WHEN a stale binary is replaced.
#
# WHY a version gate at all (issue 1118): an already-present binary whose sha256 DIFFERS from the
# local one must be re-uploaded — a schema bump (e.g. RecordingPartial v3->v4) leaves a stale
# on-box binary emitting the OLD schema, whose partial the fresh dev1 merge rejects.

# verdict_upload_decision <force> <present> <local_sha> <remote_sha>
# Pure/testable (no network): prints `upload` or `skip`. The caller runs the actual sha256 probes.
#   force=1                               -> upload  (--force-upload always wins)
#   present!=1 (absent / not a binary)    -> upload
#   present=1 but local_sha empty         -> upload  (cannot verify identity -> fail safe, never skip blind)
#   present=1 and local_sha != remote_sha -> upload  (VERSION GATE: stale emitter after a schema bump)
#   present=1 and local_sha == remote_sha (non-empty) -> skip (fast idempotent path)
verdict_upload_decision() {
  local force="${1:-0}" present="${2:-0}" local_sha="${3:-}" remote_sha="${4:-}"
  [ "$force" = "1" ] && { echo "upload"; return 0; }
  [ "$present" = "1" ] || { echo "upload"; return 0; }
  [ -n "$local_sha" ] || { echo "upload"; return 0; }
  [ "$local_sha" = "$remote_sha" ] && { echo "skip"; return 0; }
  echo "upload"
}
