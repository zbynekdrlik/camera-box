# Autopilot decision log

Run-scoped decisions + per-issue notes so a resumed/compacted loop re-loads context.

## 2026-06-08 — auto-merge run

- **Merged PR #5** (Phase 1 NDI frame-loss/latency E2E harness, cam2 loopback) → main at merge commit `ce1cdb8`. CI green (lint/test/coverage/mutants/build/security); /review + /requesting-code-review clean; 5-min on-device coverage run = zero loss, latency mean 112ms.
- Bumped dev → `1.6.0-dev.1`.
- **Decision (user, this session):** final quality bar is **60 fps** end-to-end (below = fail) → filed #11. Pipeline currently 30 fps (`src/capture.rs` hard-codes 1080p30).
- **Backlog assessment:** no bundle-safe (≤300 LoC, independent) issues remain.
  - #4 auto-update — large greenfield, user-deferred ("not on focus").
  - #6 Phase 2 (OBS taps) — large multi-module feature; depends on #5 (now merged) + #8; needs design.
  - #7/#8/#9/#10 — labeled `blocked` (depend on earlier phases / clock sync).
  - #11 60 fps — large pipeline-enablement, future end-state.
  → Autopilot stops after merging #5: nothing auto-implementable without crossing the bundling gate or a genuine design decision.
