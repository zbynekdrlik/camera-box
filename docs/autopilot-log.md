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

## 2026-06-09 — auto-merge run

- Session already shipped #6 (Phase 2, closed via #12/#15) and #10 (latency/freeze
  hard gate, merged PR #16 → main `945877a`, main CI green). dev bumped → `1.7.0-dev.4`.
- Removed stale `blocked` labels on #10 and #9 (their only blocker, Phase 1 PR #5, is merged).
- **Backlog scan: NONE hands-off auto-mergeable.**
  - #8, #7 — `blocked`. #8 needs a design call (chrony NTP vs PTP) + destructive deploy
    to live cam1/3/4 + Windows OBS; #7 depends on #8.
  - #11 — phase-3; depends on #7+#8 + hardware capture 30→60 fps change. Fails gate.
  - #4 — user "not on focus right now"; greenfield fleet auto-updater. Deferred.
  - #9 — actionable but needs a sudo self-hosted-runner service install on dev1
    (security boundary: runs arbitrary CI code) + repo-admin token (gh HAS it).
    cam2 off-air verification is pre-authorized.
- **Action: STOPPED for authorization.** No silent destructive/security-sensitive action
  under auto-merge. Awaiting user choice: authorize #9 runner setup, or make the #8
  chrony-vs-PTP design call + authorize the production off-air clock deploy.

## 2026-06-13 — auto-merge run

- **#39 Harden loopback-e2e.sh remote env interpolation (printf %q).** dev bumped → `1.7.0-dev.23` (c6ae5cad3). TDD: RED `e5f1e6fb3` extracted `build_remote_env()` (still single-quote interpolation) behind a `BASH_SOURCE != $0` source-guard + added behavioral test `tests/harness_remote_env_quoting.rs::loopback_remote_env_is_injection_safe` (injects a `'`-bearing SOURCE, evals the prefix as the remote shell would, asserts no command runs + value round-trips) → injection executed = RED. GREEN `02a2823b7` switched the builder to `printf %q` → safe, test passes. Full suite 140/0/0; clippy/fmt/shellcheck clean.
- **Decision:** repo-only script change — NO production/device deploy (no cam2 hardware re-verify needed for the quoting fix; the env handoff is unit-proven injection-safe). multitap-e2e.sh checked — uses `$VAR` (numeric/local) in double-quoted ssh, NOT the free-text single-quote env class, so not vulnerable to #39; no follow-up filed.
