# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

## Playbook router

Every rule below lives in `.claude/rules/` and auto-loads on its `paths:`; its full router description is moved verbatim to its `## Router entry` section.

- Rig ops (DanteSync clock, device deploy, recovery) → load `.claude/skills/ops`
- bkshading = a pinned release of … → `docs/CONTRACT.md` + shading-pinned-release.md
- bkshading deploy + operation here → bkshading.md
- Fleet upgrade = camera-box AND the pinned … → fleet-relay-upgrade.md
- bkshading handheld SBC → bkshading-sbc.md
- Read-only-root rw windows → ro-window.md
- E2E `[0/8]` test-camera shutter/ISO ENFORCE → camera-test-settings.md
- The intercom = a pinned release of … → intercom-pinned-release.md
- Audio clock-domain + latency map → audio-clock-map.md
- Deploying a HISTORICAL camera-box build → historical-build-bisect.md
- camera-box version-parity treadmill + the … → version-parity-treadmill-align.md
- The cambox JPEG XS link beside NDI → jxs-link.md
- The forked OBS on the Linux AND Windows OBS … → obs-pinned-release.md
- strih-snv + stream-snv program audio over … → strih-dante-input.md
- Pinned vstack tag → vstack-pin.md
- Provisioning / new cam box (build USB → setup-device.sh → verify-device.sh acceptance gate, #448-#454) → load `.claude/skills/provision`
- V4L2 capture controls (colour vs sharp sets, device-state persistence, NZXT CAM4 no-controls, grayscale/tint, the #299 colour-capture chroma metric) → load `.claude/skills/capture`
- Head-end OPTICAL blur/shutter `[0/8]` … → optical-head-end-preflight.md
- Genlock OBS (deployed state, monorepo direction, NDI input mapping, timecode lag) → load `.claude/skills/genlock`
  - Genlock latency is ONE user knob in MS (#235): `OBS_GENLOCK_LATENCY_MS=N` (canonical; `OBS_GENLOCK_RESERVE_MS` is the back-compat alias; prod=3ms). Setting it implies ts-align on; preload is internal/auto-derived. Display: `latency = N ms (≈ M frames)`.
- OBS launch/recovery on strih/stream → load `.claude/skills/obs-ops`
- OBS launch-path contract → obs-launch-paths.md
- `--display` HDMI path (connector/phantom-fb detect, upscale cap, capture-dropped counter) → load `.claude/skills/display`
- Linux OBS boxes without / after plugging / … → display-hotplug.md
- Cameraman HDMI preview SOURCE resolution → cameraman-preview-source.md
- CI artifacts, Discord notify, probe binary flow → load `.claude/skills/ci`
- E2E zero-loss testing (acceptance criteria, QR harness, reporting scope, active fleet size / `CAMERA_ACTIVE_SET` reactivation) → load `.claude/skills/e2e`
- Rig TEST/EVENT mode switch (#247 `scripts/rig-mode.sh`: pinned QR/burns/genlock per mode, the #246 burn-leak guard) → load `.claude/skills/e2e`
- The stream box is the Linux stream-snv since … → stream-box-linux.md
- Stream OBS `Development` scene → stream-development-scene.md
- Recording-verdict QR decode path (fast/robust gate — the core is `src/probe/recording_decode.rs` since issue 1374, `qr.rs` keeps the primitives and re-exports it; per-recording burn sets, #186 fixtures) → load `.claude/skills/recording-decode`
- Burn-isolated slot recovery in the recording … → burn-region-recovery.md
- CG-chain receiver-side verdict → cg-chain-verify.md
- RESOLUME-SNV cg OBS on the genlock fleet → resolume-cg-obs.md
- EVENT-mode CONTRACT → event-assert-none-tolerance.md
- A/V-sync offset measurement (cam2 QPSK marker, `--av-sync`, ring-bias + cluster-pairing gotchas) → load `.claude/skills/av-sync`
- A/V silent-vs-undecoded discriminator → av-audio-silent-discriminator.md
- SRT A/V-sync tap safety → srt-tap-safety.md
- Decode/recognition geometry or algorithm … → pattern-change-needs-decode-fixture.md
- imag-nb swap → imag-nb-provisioning.md
- The ONE shared OBS-box appliance baseline → obs-box-baseline.md
- The ONE Windows OBS-box baseline → obs-box-baseline-win.md
- strih-snv Linux provisioning → strih-linux-provisioning.md
- strih-snv session apps → strih-session-apps.md
- Stream-box headphone monitor → stream-headphone-monitor.md
- The A/V-sync dock on the Linux OBS boxes → av-sync-dock-linux.md
- The stream operator's headphones through a … → stream-operator-audio.md
- Housekeeping off the OBS CPUs on a strih box → strih-housekeeping.md
- PipeWire OUT of the strih role → strih-pipewire-purge.md
- The operator desktop = box fact … → obs-box-xfce.md
- The kiosk panel of the Linux OBS boxes → obs-box-panel.md
- strih-tally camera tally lights → strih-tally.md
- strih box's OWN intercom + shading HTTPS … → strih-https.md
- strih OPERATOR surface → strih-operator.md
- strih-snv in-place recording decode → recording-verdict-on-strih-snv.md
- Reading the strih OBS log from a … → strih-log-read.md
- E2E gate preconditions → rig-standing-services.md
- Never mutate the rig while a broadcast may be … → rig-mutation-broadcast-guard.md
- Why strih+stream OBS auto-RECORD on stream … → strih-autorecord-coupling.md
- Rig lease read-only HTTP exposure on dev1 → rig-lease-http.md
- stream program-audio guard → program-audio-guard.md
- ANY new dev1 `--user` systemd unit's … → dev1-systemd-user-unit-hardening.md
- CI/workflow concurrency-cancel risk … → ci-testing-gotchas.md
- `CAMERA_ACTIVE_SET` fleet-enumeration … → camera-active-set.md
- `OBS_FLEET` → obs-fleet-list.md
- Editing `targets.md` → targets-md-editing.md
- imag-nb SSH-remote tool preflight → imag-ssh-remote-tool-preflight.md
- Presenter DRM device selection + dual-QR … → presenter-drm-selection.md
- `recording-e2e.sh` cleanup()'s always-runs … → recording-e2e-cleanup-composition.md
- Sender-bounce reverify painter-order proof + … → mv-reverify-escalate.md
- `[4c/8]` frozen-camera gate leg-liveness = … → frozen-cam-received-gate.md
- On-box dead-man for PRODUCTION … → camera-box-deadman.md
- `setup-device.sh`/`verify-device.sh` … → provisioning-scripts.md
- cambox off-box remote logging → cambox-remote-logging.md
- Realtime isolation subsystem → realtime-isolation.md
- The vendored genlock render-tick CPU pin on … → genlock-render-tick-pin.md
- DanteSync clock-offset gate → dantesync-clock-offset-gate.md
- dev1 dante-clock alert watchdog → dantesync-clock-alert-watchdog.md
- dev1 measurement-audio presence watchdog → measurement-audio-watchdog.md
- Reading dantesync's own VERSION → dantesync-version-reading.md
- version-integrity-gate.sh Windows-stack drift … → version-integrity-gate.md
- drift-guard.sh `*_from_log` parsers must be … → drift-guard-log-parsers.md
- Early-gate PIN doctrine → early-gate-pin-doctrine.md
- dantesync became dsync → dsync-rename.md
- dantesync FLEET UPGRADE mechanism → dantesync-fleet-upgrade.md
- The ONE declared dantesync fleet → dantesync-fleet.md
- Inter-PC VBAN rate + loss → vban-rate.md
- obs-vban send pacing → obs-vban-pacing.md
- obs-vban RECEIVE side + the stream role's … → obs-vban-receive.md
- VBAN receiver bench → vban-receiver-bench.md
- RESOLUME-SNV dantesync maintenance → resolume-dantesync.md
- imag-nb OBS runtime supervision + alerting → imag-obs-supervision.md
- imag E2E record encoder → imag-record-encoder.md
- `gaps` vs `residual_events` divergence → gap-metric-reconciliation.md
- Burn-id contiguity is PRESENCE-ONLY → burn-hold-uniqueness.md
- CG-path burn-id node role → cg-burn-node-role.md
- phase-sync calibrator/snapshot test writes … → phase-sync-calibrator-testing.md
- udev device ownership during an E2E burn run → udev-device-ownership.md
- self-heal reset must never misreport as … → self-heal-frozen-leg-attribution.md
- Fast-capture grabber STUCK self-heal + alert → grabber-stuck-selfheal.md
- Leg-health `[0/8]` preflight capture-health … → leg-health-frame-loss.md
- Classifying a #707 genlock emit-gate SKIPPED … → emit-gate-skip-triage.md
- Capture PHASE tracker + the STAMP-DRIVEN emit … → capture-phase-tracker.md
- In-process USB self-heal action SEQUENCE → capture-selfheal-action-sequence.md
- cleanup() cambox parallel-restore group → cambox-parallel-restore.md
- ASRC bench harness → asrc-bench-harness.md
- The issue-1381 wall-step, relabel and … → asrc-timecode-step-bench.md
- Reading the stream asrc: source 'mbc' … → asrc-residual-floor.md
- The genlock libobs audio buffering → genlock-audio-buffering-floor.md
- Optical undecodable floor gate → optical-undecodable-floor-report-only.md
- Walking `WINDOW_COPIES_GAPS_TOLERANCE` … → window-gate-tolerance-walkdown.md
- Changing the cross-camera … → spread-threshold-consumers.md
- Restoring a #904-style relaxed verdict-gate … → gate-allowance-restore-red-green.md
- Calibrating + wiring a NEW fused-verdict gate … → verdict-gate-seam-calibration.md
- Lipsync cross-check fold → lipsync-cross-check-fold.md
- lipsync-test-mode.sh playback is mpv … → lipsync-drm-playback.md
- The cam2 Vernier `tick` decodes on EVERY … → cambox-tick-decodability.md
- Projection-tap scanout-TEAR detector → projection-tap-tear-detect.md
- A MULTI-SOURCE cambox window → multi-source-window.md
- Cold-cut gate → cold-cut-gate.md
- `DockLockCorrector` hold-band boundary tuning → dock-lock-hold-band.md
- Refactoring the dock-lock if/else-if/else … → av-sync-dock-anchor-refactor-safety.md
- NO analysis on libobs's video-output thread → av-sync-dock-decode-worker.md
- Writing a NEW `tests/av_sync_dock_*.rs` … → av-sync-dock-twin-harness-testing.md
- Dock DISPLAY math → av-sync-dock-display-parity.md
- QPSK marker demod gate → qpsk-marker-demod.md
- Inspecting AND deploying live rig state … → rig-state-inspection.md
- One canonical genlock deploy path across the … → genlock-fleet-deploy.md
- Heartbeat wedge-watchdog recipe for a NEW … → wedge-watchdog-pattern.md
- Stream-box avsync watchdog + VLC monitor + … → avsync-monitoring.md
- ssh vs win-* MCP on strih/stream → win-ssh-vs-mcp.md
- Wake-on-LAN remote recovery for strih/stream → wake-on-lan.md
- dev1-side reachability + frozen-input alert … → network-reach-watchdog.md + frozen-input-watchdog.md
- Genlock FIFO limit-cycle diagnosis from a … → genlock-fifo-limit-cycle-diagnosis.md
- Attributing a DUPLICATE / `copies` residual → painter-pacing-attribution.md
- Attributing a copies/gaps residual to SOURCE → residual-churn-attribution.md
- Changing the genlock C in libobs → vendored-libobs-change-safety.md
- Windows OBS media clock runs at the … → windows-disciplined-media-clock.md
- A dantesync fleet DATE step on the OBS boxes → genlock-wall-step.md
- The nightly fleet DATE step at the genlock … → genlock-fifo-relabel.md
- In-OBS GENLOCK LOCK indicator → genlock-lock-indicator.md
- Fleet-visible genlock LOCK facet + dev1 … → genlock-lock-facet.md
- Vendored OBS FRONTEND crash-safety → vendored-obs-frontend-crash-safety.md
- imag HDMI tearing → obs-projector-vsync.md
- OBS projector window path = the STOCK … → obs-projector-stock-toplevel.md
- imag HDMI Program OUT of Xorg → obs-drm-output.md
- DRM-latency measurement tooling → drm-latency-measurement.md
- DistroAV NDI receiver-thread lifecycle → distroav-receiver-lifecycle.md
- NDI `ndi_source_name` recovery → ndi-name-recovery.md
- imag NDI name-healing → imag-ndi-active-set-policy.md
- NDI discovery = a RECEIVER-side list of the … → ndi-discovery.md
- NDI SENDER port-map stability watchdog → ndi-portmap-watchdog.md
- DistroAV SENDER-output lifecycle + NDI port … → distroav-sender-output-lifecycle.md
- OBS window-title build identity → obs-titlebar-build-id.md
- Genlock hold-collapse diagnosis → genlock-hold-collapse-diagnosis.md
- obs-liveness watchdog → obs-liveness-render-signal.md
- genlock audit-log parser → jitter-audit-parser.md
- Receiver-side AUDIO genlock parity → genlock-audio-pairing.md
- Genlocked NDI SENDER contract → `docs/genlock-sender-contract.md` + genlock-sender-contract.md
- genlock-FIFO settle-wait → genlock-settle-wait.md
- ONE per-second genlock grid → genlock-per-second-grid.md
- N==1 PIN-DERIVED DEPTH → genlock-n1-pin-derived-depth.md
- GRID-EXACT N>=2 conveyor → genlock-n2-grid-conveyor.md
- N>=2 conveyor arrival-jitter BUDGET → genlock-conveyor-jitter-budget.md
- PROGRAM-render observability line → program-render-audit.md
- Offline audio-quality → audio-quality-measurement.md
- Full-path E2E failed-gate Discord alert → e2e-failure-alert.md
- The E2E's BLOCKING whole-session video gate → genlock-session-continuity.md
- E2E per-run Discord report → e2e-discord-report.md
- YouTube-leg verdict tool → youtube-leg-verdict.md
- The full-path E2E's BLOCKING YouTube stage → youtube-leg-e2e-stage.md
- The CG segments' measurement clip, never … → measurement-clip.md
- Pixel proof for merge-classified slots with … → missing-slot-pixels.md
- E2E recordings retention → recordings-retention.md
- OBS deploy/backup DIRECTORY retention → obs-backup-retention.md
- Absolute e2e latency + freeze bounds → e2e-latency-gate.md
- The 8 h stream-output A/V soak → av-soak.md
- The stream-output RESTART MATRIX → av-restart-matrix.md
- imag leg recording verdict → imag-leg-report-only.md
- Switch-in transient classifier → switch-in-transient.md
- imag-nb power/thermal envelope → imag-power-envelope.md
- imag display-path drift facets → imag-display-path.md
- strih/stream :8899 BundleStateServer … → bundle-state-watchdog.md
- bundle-state SERVER internals → bundle-state-gather-latency.md
- non-60 source-cadence alert → cadence-watchdog.md
- OBS audio-timeline-lag alert → audio-lag-watchdog.md
- Render-freeze / relock-storm alert → render-freeze-watchdog.md
- OBS audio MIXER off real time + obs-vban … → audio-mixer-watchdog.md
- Program-audio RECEIVE loss pager → program-audio-rx-watchdog.md
- OBS process HANDLE-LEAK alert → obs-handles-watchdog.md
- Stream OBS STUCK-OUTPUT page → stream-output-stuck-watchdog.md
- Upstream-audio-latency STEP detector → av-step-upstream-detector.md
- Re-pinning a probe-gated `ReleaseCadence` … → probe-mirror-replica-testing.md
- Cam2 optical-injection-leg health → optical-chain-health-watchdog.md
- CG-bridge → cg-bridge-republish-black.md
- cam2 painter LIFECYCLE → cam2-painter-lifecycle.md
- /dev/fb0 blank-on-teardown + frame-probe … → fb0-blank-on-teardown.md
- Measurement-burn OFF/CHECK/RESTORE target set → burn-target-enumeration.md
- dev1 fresh-OBS-start burn reconciliation for … → obs-burn-reconcile-watchdog.md
- Reading/verifying per-source genlock latency … → latency-pins-verify.md
- Measurement-window per-camera A/V equalization → measurement-eq.md
- Floor-3 per-run camera auto-align → qr-align.md
- Per-cambox HDMI-splitter-port no-signal … → splitter-port-health-watchdog.md
- Gating a TEST-premise dev1 watchdog on rig … → rig-mode-event-gate.md
- "development" handover check → rig-dev-handover-check.md
- MV-clone-vs-main presentation SKEW via OBS-WS … → mv-skew-measurement.md
- NIC swap aftermath → nic-swap-timesync-recovery.md
- Venue-switch config-drift audit + checked-in … → netcfg-audit.md
- Per-camera NDI SEND stagger → ndi-send-stagger.md
- OBS-WS writes to a genlocked NDI input → obs-ws-lockdown-writes.md
- imag-nb offline-ack leg skip → imag-offline-ack.md
- MV-fps observability + live alarm → mv-fps-watchdog.md
- Byte-safe extraction from a PS-fetched … → ps-log-byte-safety-extraction.md
- ASIO-source-starved alert → asio-starve-watchdog.md
- NDI per-connection rate-halving auto-heal → ndi-halving-watchdog.md
- VB-Matrix presence facet + dev1 alert watchdog → vb-matrix-watchdog.md
- Alert-watchdog Discord notify discipline → watchdog-notify-dedup.md
- The shared dev1 watchdog glue … → watchdog-common.md
- Rig status page → rig-status-page.md
- NTP-client DSCP nftables rule → dscp-nft-provisioning.md
- cambox NDI egress PACING → cambox-egress-pacing.md
- strih / stream box sender egress PACING → strih-egress-pacing.md
- Per-box arrival-floor STAGE decomposition … → arrival-floor-decompose.md

## DO NOT DELETE These Files

**NEVER delete `targets.md`** — it contains IP addresses for all deployment targets (Windows and cameras). This file has been accidentally deleted multiple times during PR cleanup. DO NOT remove it.

## Every-ticket rules (full text in `.claude/rules/`, issue 1552)

Pointers; the rule file holds the incidents and the full recipe.

- a `fix: #N` / `close #N` substring in ANY commit, PR title or PR body auto-closes #N on merge (negation does not protect) — write `fix(#N):` or `issue N` → `commit-message-gotchas.md`
- a commit message with a backtick, `$` or `%` goes through the quoted heredoc (`<<'EOF'`), never a double-quoted `-m` → `commit-message-gotchas.md`
- never a bare `#N` of ANOTHER ticket in a commit message or in the same Bash call as `git commit` (the design gate blocks it) — write `issue N` → `commit-message-gotchas.md`
- the dev1 checkout is shared: pathspec commits only, never `git add -A`, worktree lanes based on `origin/dev`, diff against `git merge-base` → `shared-checkout-worktrees.md`
- editing `scripts/recording-e2e.sh` / `scripts/rig-mode.sh` can break other tests' literal anchors — add behaviour through a sourced helper → `static-anchor-tests.md`
- one failing test binary makes `cargo test` skip the rest — re-run the suite after every fix → `cargo-test-full-suite.md`
- a new `.sh` file puts `set -euo pipefail` within its first 15 lines; a `_cmd` helper embedded via `$(...)` ends its last statement with `;` → `shell-script-gotchas.md`
- `gh pr merge` / `gh pr edit` misbehave here — use the REST `gh api .../pulls/<N>/merge` / PATCH (never `--admin`) → `gh-cli-gotchas.md`
- design / validated / review comments go through a standalone `gh issue comment <N> --body-file`, never `gh api`, then check the marker → `gh-cli-gotchas.md`
- a fresh REAL E2E verdict on a pushed commit = `gh run rerun <pull_request run id>`, NEVER `gh workflow run` (plan-only green) → `e2e-rerun-not-workflow-dispatch.md`
- every version is a clean `X.Y.Z` in the root `Cargo.toml`; dev = main + 1 patch, bumped first after each dev->main merge; every merge to main is a tagged release → `release-versions.md`

## Local Build Policy

CI (GitHub runners) builds the deployable `camera-box` release binary AND the probe/verdict
binaries (`--features probe`) via two artifact uploads (`camera-box-linux-amd64`,
`probe-tools-linux-amd64`); deploys and E2E runs use those CI artifacts. Before a push:
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

**Pure probe logic goes in a crate-root module, not in `src/probe/`.** The whole `probe` module is
`#[cfg(feature = "probe")]` (lib.rs); extract the PURE logic (geometry, decisions, tables) into a
crate-root module that compiles on default features — the `vstack:av/ndi/src/reannounce.rs` /
`src/colour_scale.rs` (#367) pattern — and have the probe-gated code (`src/probe/…`) call it, so
its tests run without the heavy probe deps.
