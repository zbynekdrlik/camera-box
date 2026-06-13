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

## 2026-06-13 — auto-merge run (#44)

- **#44 `/update-av-stack` slash command.** dev bumped → `1.7.0-dev.24` (57b7e80fb). #41 vendoring (subtree --squash) + manifest table in `vendor/README.md` + genlock patches (`cf7b06067`, `ac1c73dfc`) all already exist → #44 grounded, no prerequisite filed. Built engine `scripts/update-av-stack.sh` (pure functions behind `BASH_SOURCE != $0` guard: parse_manifest/normalize_url/version_status/subtree_pull_cmd; network+mutating `--check`/`--apply` flow after guard) + Claude slash command `.claude/commands/update-av-stack.md` + Rust behavioral test `tests/av_stack_update.rs` (4 tests, sources the bash like #39's pattern). Verified live: `--check` against real upstream → both UP-TO-DATE (32.1.2/6.2.1) exit 0; fake-old manifest → BEHIND emits exact `git subtree pull --squash` + checklist exit 10; `--apply` aborts loudly on dirty tree. shellcheck/fmt/clippy clean; full suite green.
- **Decision:** repo-only tooling — NO device/production deploy. The command re-applies genlock patches through the subtree 3-way merge and reports conflicts loudly; conflict-resolution richness grows as more genlock patches land (#42), command is correct now regardless of patch count.

## 2026-06-13 — auto-merge run (#43)

- **#43 Disable OBS upgrade dialog/auto-update in the genlocked build.** dev bumped → `1.7.0-dev.25` (a18e20f79). Genlock patch flips the OBS-native `opt_disable_updater` global default `false`→`true` in `vendor/obs-studio/frontend/obs-main.cpp` — the same mechanism the upstream `--disable-updater` flag / `disable_updater` file set, baked to default-ON. Nothing else assigns it false, so `OBSApp::IsUpdaterDisabled()` is permanently true → cascades to all three chokepoints: `TimedCheckForUpdates()` early-returns (no auto/timed check), `OBSBasic.cpp` disables the "Check For Updates" menu action (no manual trigger → `AutoUpdateThread`/dialog), and `OBSBasicSettings.cpp` hides the auto-update settings. TDD: RED `a5f3bbc4b` patch-presence test `tests/obs_updater_disabled.rs` (asserts default `true` + the IsUpdaterDisabled wiring at the 3 sites is intact; defends against a `git subtree pull` reverting to upstream's `false`), GREEN flips the initializer. fmt/clippy clean, full suite 147/0.
- **Decision:** PROD DEPLOY DEFERRED — applying this to the live genlocked OBS on strih (10.77.9.202) + stream (10.77.9.204) needs a Windows OBS REBUILD from this tree + REDEPLOY (no automatic pipeline; user is 'the guard'). Source/build change merged; the rebuild+redeploy is flagged for user approval, not done unattended.
