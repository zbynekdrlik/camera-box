#!/usr/bin/env bash
#
# purge-target.sh — bound the shared dev1 Cargo target/ so it stops eating disk (#185).
#
# WHY (#185): camera-box is Tier-0 — CI builds the deployable + probe binaries; locally we run only
# cheap checks (cargo check / clippy / test --no-run). But cargo's project-local target/ has NO
# garbage collection (rust-lang/cargo#5026): every (features x profile x bin x rustc x cfg) combo
# leaves artifacts that are never auto-removed, plus the incremental cache + rust-analyzer pile up
# across a day's workers. It ballooned to 18 GB and filled dev1's disk to 93%. The ROOT fix is in
# CLAUDE.md (cheap checks run on DEFAULT features only — never --features probe / --all-features,
# which pulled qrcode/rqrr/image/drm/lz4_flex + 5 extra probe bins into the shared target/). This
# script is the BACKSTOP: it purges target/ when it crosses a size budget, so even with default-only
# checks the cache resets between sessions instead of growing forever.
#
# Usage:
#   scripts/purge-target.sh              # purge if target/ > THRESHOLD_MB (default 4096 = 4 GB)
#   THRESHOLD_MB=2048 scripts/purge-target.sh
#   scripts/purge-target.sh --force      # purge regardless of size
#   scripts/purge-target.sh --check      # report size only, never purge (exit 0)
#
# SAFETY: NEVER purges while an E2E is live — deleting target/ out from under a running probe binary
# would corrupt the run. The guarded names are the probe ANALYSIS binaries that run ON dev1 (where
# this purge runs): recording-verdict / frame-probe / multitap-probe / recording-probe / forensic-dump.
# (The probe-featured `camera-box` itself runs on the REMOTE camera, renamed to camera-box-burn-*, so
# it is never a dev1 target/ consumer — it is intentionally not in the dev1 guard list.)
# Run from the repo root (or anywhere — it cd's to the repo root via git).

set -euo pipefail

THRESHOLD_MB="${THRESHOLD_MB:-4096}"
FORCE=0
CHECK_ONLY=0

for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    --check) CHECK_ONLY=1 ;;
    -h|--help)
      sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "purge-target.sh: unknown arg '$arg'" >&2; exit 2 ;;
  esac
done

# Resolve repo root so the hook can call us from anywhere.
REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel 2>/dev/null || echo "$(cd "$(dirname "$0")/.." && pwd)")"
TARGET_DIR="$REPO_ROOT/target"

if [ ! -d "$TARGET_DIR" ]; then
  echo "purge-target.sh: no target/ — nothing to do."
  exit 0
fi

SIZE_MB="$(du -sm "$TARGET_DIR" 2>/dev/null | cut -f1)"
SIZE_MB="${SIZE_MB:-0}"
echo "purge-target.sh: target/ = ${SIZE_MB} MB (budget ${THRESHOLD_MB} MB)"

if [ "$CHECK_ONLY" -eq 1 ]; then
  exit 0
fi

# Never purge while a probe ANALYSIS binary is executing on dev1 (a live E2E).
# Match by PROCESS NAME (comm, the executable basename) — NOT `pgrep -f` over the whole command
# line, which false-positives on any process that merely MENTIONS these names as arguments (this
# very script, a `grep`/`ps` for them, an editor with the file open). `pgrep -x` matches comm.
# CAVEAT: the kernel truncates comm to 15 chars, so 'recording-verdict' (17) -> 'recording-verdi'.
# We list the truncated form so it still matches. (Only the analysis bins run on dev1; the
# probe-featured camera-box runs on the remote camera — see the SAFETY note in the header.)
PROBE_COMM='recording-verdi|frame-probe|multitap-probe|recording-probe|forensic-dump'
if pgrep -x "$PROBE_COMM" >/dev/null 2>&1; then
  echo "purge-target.sh: probe binaries are RUNNING (live E2E) — skipping purge (safety)." >&2
  exit 0
fi

if [ "$FORCE" -eq 0 ] && [ "$SIZE_MB" -le "$THRESHOLD_MB" ]; then
  echo "purge-target.sh: under budget — no purge needed."
  exit 0
fi

echo "purge-target.sh: purging target/ (${SIZE_MB} MB) — CI rebuilds; cheap checks recompile fast."
# cargo clean resets the project target/. (rm -rf target works too; cargo clean is the canonical form.)
if command -v cargo >/dev/null 2>&1; then
  ( cd "$REPO_ROOT" && cargo clean )
else
  rm -rf "$TARGET_DIR"
fi
echo "purge-target.sh: done — target/ reset."
