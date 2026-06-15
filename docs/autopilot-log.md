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

## 2026-06-14 — auto-merge run (#45)

- **#45 Drift guard: enforce pinned OBS/DistroAV/NDI versions + settings on strih/stream.** dev already at `1.7.0-dev.26` (f2cb3e482, prior worker). New engine `scripts/drift-guard.sh` (pure functions behind the `BASH_SOURCE != $0` source-guard — manifest/OBS-log/setting parsers + semver compare; flow runs only when executed) + Rust behavioral test `tests/drift_guard.rs` (10 tests sourcing the bash, log fixtures = REAL lines captured read-only off strih/stream) + `.claude/commands/drift-guard.md` (live read-only run) + CI job `drift-guard` (`--check-pins`) + pinned-settings table in `vendor/README.md`. Two facets: `--check-pins` (CI, no prod access) validates the pin set is complete/well-formed AND cross-checks the manifest's DistroAV pin against the vendored source `vendor/distroav/buildspec.json`; `--compare KEY=VAL` (live) compares values read off a box and FAILS loudly (exit 20 drift, 11 UNKNOWN — a value not read never passes silently). fmt/clippy/shellcheck clean; full suite 251/0.
- **Live read-only verify (2026-06-14):** strih (10.77.9.202) + stream (10.77.9.204) both = OBS **32.1.2**, DistroAV **6.2.1**, NDI runtime **6.3.2.0** (≥ 6.3.0 ✓), output **1080@30**, genlock master gate `OBS_GENLOCK_WALL_CLOCK` empty (dormant) → `--compare` exit 0, **NO DRIFT** on both. These known-good values are the pin captured in `vendor/README.md`.
- **Decision:** repo-only tooling — NO prod write/restart. Updater-disabled (#43) stays guarded at the build layer by `tests/obs_updater_disabled.rs` (not runtime-readable off a running box), not duplicated here. fps `30` / genlock `0` re-pin when the #11 60-fps step or genlock activation deliberately changes them. The live facet is operator/agent-driven (win-* MCP); CI can't reach the prod LAN.

## 2026-06-14 — fleet auto-merge run (issues #39 #44 #43 #45)

- **Merged 4 issues** (auto-merge): #39 PR#52 (loopback printf %q quote hardening, RED test_loopback_env_quote_injection→GREEN), #44 PR#53 (/update-av-stack command), #43 PR#54 (disable OBS updater in vendored build; tests/obs_updater_disabled.rs), #45 PR#56 + correctness follow-up PR#57 (/drift-guard live facet). dev→1.7.0-dev.27, main CI green incl. Windows genlock build.
- **Live prod verified (drift-guard --compare, read-only):** strih + stream both NO DRIFT — OBS 32.1.2, DistroAV 6.2.1, NDI 6.3.2.0, output_fps 30, genlock_wall_clock 1 (ENABLED, HKLM agrees). Zero-loss pinned state intact, genlock active both boxes.
- **#43 deploy DECISION (not deferred-as-dropped):** NOT swapping the prod OBS binary standalone. The updater-disable is in the vendored source and ships on the next deliberate OBS rollout (/update-av-stack upstream bump, or #11 60fps step). Rationale: live state is verified-perfect; a binary swap to a known-good broadcast OBS purely to flip the updater flag risks the working zero-loss state. Drift-guard now detects any auto-update, so genlock cannot be silently clobbered in the meantime. User holds standing deploy approval — swap on request.
- **Skipped this run (autopilot-skip, auto-picked-up later):** #50 #24 #11 #8 (need CAM1/CAM4 powered), #4 (greenfield). #26 (question) / #7 (blocked) filtered.

## 2026-06-14 — auto-merge run (#55)

- **#55 windows-genlock.yml updater gate = packaging check, not source-flag check.** dev → `1.7.0-dev.28` (3d0a60c5e). The `updater.exe`-absence assertion (windows-genlock.yml) verified a rundir packaging-layout outcome, not the #43 source mechanism (`opt_disable_updater = true` + `IsUpdaterDisabled()` wiring) — a future `/update-av-stack` subtree bump (#44) changing rundir copy rules could flip it either way, false signal. Added a PRIMARY pwsh source-flag gate right after checkout (fails fast before the 150-min build; asserts default `true`, upstream `= false` gone, `IsUpdaterDisabled()` wiring intact) + re-documented the `updater.exe` step as secondary defense-in-depth. camera-box is Linux-only (v4l/alsa/evdev) so the canonical `tests/obs_updater_disabled.rs` can't run on the windows-2022 runner → the pwsh gate re-asserts the same source tokens; new Rust test `windows_genlock_workflow_gates_on_the_source_flag_not_only_packaging` keeps the two in lock-step (RED before the workflow edit, GREEN after). fmt/clippy clean, full suite green.
- **Decision:** repo-only (CI/test logic) — NO prod/device deploy. Robustness/clarity follow-up surfaced during #43 review (PR #54); no behavior change to the build or the genlocked boxes.

## 2026-06-15 — deploy run (#50)

- **#50 Deploy genlock camera-box to live CAM1/CAM4 + strih FIFO** (deploy-only, no code change). Deployed CI main artifact `camera-box 1.7.0-dev.29` (md5 `49e76ee20d38715939506f8119568688`, run 27505860120 @ `19eaa9a41`) to CAM1 (10.77.9.61) + CAM4 (10.77.9.64) per CLAUDE.md Build & Deploy (remount rw → stop → scp → start → remount ro); both md5 byte-match the artifact. Added the canonical genlock env via systemd drop-in `/etc/systemd/system/camera-box.service.d/genlock.conf` = `Environment=CAMERA_BOX_GENLOCK_FPS=30` (identical to cam2's file). Live-verified both: service active, effective env carries GENLOCK_FPS=30, journal shows steady **30.0 fps emitted / 60.0 fps captured** (wall-paced 60→30 decimation). Enabled strih FIFO via live obs-websocket (port 4455) SetInputSettings `genlock_fifo=true` (overlay merge, non-destructive) — resolved by SOURCE field per inverted-label map: CAM1=input "NDI cam5" (src "CAM1 (usb)"), CAM4=input "NDI cam3" (src "CAM4 (usb)"); correlated read-back confirms all three NDI cam ingests now `genlock_fifo=True` (CAM3="NDI cam1"/"CAM3 (usb)" was already True, left untouched). CAM3 (10.77.9.63) offline/powered-off, already complete from 2026-06-13 — not touched.
- **Decision:** full fleet CAM1/CAM3/CAM4 now genlock-live → #50 CLOSED with per-camera evidence comment. No PR — deploy-only ticket; this log line committed directly to dev.

## 2026-06-15 — auto-merge run (#24, partial)

- **#24 Extend frame-loss coverage to cam1/cam3/cam4 (harness was cam2-only) — CODE GENERALIZATION pass.** dev → `1.7.0-dev.30` (36a320886). Validated STILL_VALID (ticket-validator): the Rust probe binaries were already camera-agnostic (`frame-probe --source`, `multitap-probe --tap NAME=SRC`); the cam2 hard-coding lived ONLY in the shell orchestrators. TDD RED `4c776911f` added `tests/harness_camera_set.rs` (5 guards: cam1-4 resolve to right IP+source via the resolver, unknown/empty name rejected loudly, injection-safe per #39 threat model, multitap no longer contains `CAM2=10.77.9.62`, loopback routes through the set) → 4/5 fail RED (no `camera-set.sh`, multitap hard-codes cam2). GREEN `1d49e17f0`: new `scripts/camera-set.sh` (single source of truth, literal-`case` resolver `cam1..cam4` → IP + `"CAMn (usb)"`, no eval/array-index of untrusted value), `loopback-e2e.sh` + `multitap-e2e.sh` resolve through it via `CAM=` (default cam2, explicit `CAM_IP`/`SOURCE` still override), new `scripts/loopback-e2e-all.sh` loops a `CAMERA_SET` (defaults cam2-only so a bare run never takes live cams off-air). Review polish `fd134f939`: per-camera artifact path can't be clobbered by an exported `LOCAL_OUT` (use `LOCAL_OUT_PREFIX`), stale "cam2" log string fixed. PR #60 merged `aeb627bd9`; main CI 27535540463 green (lint/test/coverage/mutation/build/security/drift); `/review` 0🔴0🟡0🔵; `/requesting-code-review` APPROVE (2 minor, both fixed). full suite green.
- **Decision:** repo-only test-harness change (dev1-side scripts, NOT the device binary) — NO deploy. Issue left **OPEN** with a remainder comment: live cam1/3/4 single-node + full-path (#7) certification runs stay hardware-gated (physical HDMI-loopback rigs on cam1/cam4 + cam3 SSH down + the still-open cam2 single-node drop + off-air maintenance window). The harness can now drive any of cam1-4 by name; what's left is running it on the live rigs once those blockers clear.
