# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

## Playbook router

- Rig ops (DanteSync clock, device deploy, recovery) → load `.claude/skills/ops`
- bkshading remote camera shading control (service + cambox/SBC relay, gphoto2 transport, USB/USB-Eth only never BT, workspace/CI wiring that leaves the appliance untouched, Tier-0 verify path, #808 M1) → `.claude/rules/bkshading.md` (auto-loads on its `paths:`)
- Deploying a HISTORICAL camera-box build (bisect/rollback: run-id lookup, deploy-fleet `--run`, the mixed-fleet version-parity gate refusal + neutralization seams, local E2E, the ci.yml auto-deploy clobber) → `.claude/rules/historical-build-bisect.md` (auto-loads on its `paths:`)
- Provisioning / new cam box (build USB → setup-device.sh → verify-device.sh acceptance gate, #448-#454) → load `.claude/skills/provision`
- V4L2 capture controls (colour vs sharp sets, device-state persistence, NZXT CAM4 no-controls, grayscale/tint, the #299 colour-capture chroma metric) → load `.claude/skills/capture`
- Head-end OPTICAL blur/shutter `[0/8]` preflight (the head-end `rough=` signal + healthy 7.1–8.0 baseline; the observer-effect trap — never calibrate a camera-health gate from imag `stuck_density`; no on-box zbar/probe so journal-mine not a v4l2 grab; pure-Rust-classifier + shell-replica + parity-harness; Tier-0 #557 kills `--no-run` so verify via fmt + a language replica + direct bash, #1141) → `.claude/rules/optical-head-end-preflight.md` (auto-loads on its `paths:`)
- Genlock OBS (deployed state, monorepo direction, NDI input mapping, timecode lag) → load `.claude/skills/genlock`
  - Genlock latency is ONE user knob in MS (#235): `OBS_GENLOCK_LATENCY_MS=N` (canonical; `OBS_GENLOCK_RESERVE_MS` is the back-compat alias; prod=3ms). Setting it implies ts-align on; preload is internal/auto-derived. Display: `latency = N ms (≈ M frames)`.
- OBS launch/recovery on strih/stream → load `.claude/skills/obs-ops`
- OBS launch-path contract (.lnk primary + per-box params test-pinned; obs-guarded-launch.ps1 bare-no-args = correct for stream; strih AHK versioned at scripts/strih/NL_STARTUP.ahk, #774/#775) → `.claude/rules/obs-launch-paths.md` (auto-loads on its `paths:`)
- `--display` HDMI path (connector/phantom-fb detect, upscale cap, capture-dropped counter) → load `.claude/skills/display`
- CI artifacts, Discord notify, probe binary flow → load `.claude/skills/ci`
- E2E zero-loss testing (acceptance criteria, QR harness, reporting scope, active fleet size / `CAMERA_ACTIVE_SET` reactivation) → load `.claude/skills/e2e`
- Rig TEST/EVENT mode switch (#247 `scripts/rig-mode.sh`: pinned QR/burns/genlock per mode, the #246 burn-leak guard) → load `.claude/skills/e2e`
- Recording-verdict QR decode path (fast/robust gate, per-recording burn sets, #186 fixtures) → load `.claude/skills/recording-decode`
- A/V-sync offset measurement (cam2 QPSK marker, `--av-sync`, ring-bias + cluster-pairing gotchas) → load `.claude/skills/av-sync`
- A/V silent-vs-undecoded discriminator (`all_cambox_av_sync.av_audio_silent` fed by `preamble_screens_passed` via crate-root `av_window::classify_av_audio_state`; distinguishes a silent mbc chain from present-but-undecoded audio; adding a field to probe-gated `AvMarkerInputs` = update all struct-literal sites, #748) → `.claude/rules/av-audio-silent-discriminator.md` (auto-loads on its `paths:`)
- SRT A/V-sync tap safety (listener-not-caller-or-OBS-crashes #802: `srt_tap.py` guard, `probe_udp_port_refused` fail-open asymmetry, the ffmpeg listener-start bench probe, the bench-gated vendored-C leak) → `.claude/rules/srt-tap-safety.md` (auto-loads on its `paths:`)
- Decode/recognition geometry or algorithm changes (crop/downscale/threshold/retry — never tuned blind) → `.claude/rules/pattern-change-needs-decode-fixture.md` (auto-loads on its `paths:`)
- imag-nb swap (install-imag-nb.sh → setup-imag.sh → verify-imag.sh acceptance gate, #821; derived CPU/GPU/IP) → `.claude/rules/imag-nb-provisioning.md` (auto-loads on its `paths:`)
- E2E gate preconditions (DanteSync servo, bundle-state-server) → `.claude/rules/rig-standing-services.md` (auto-loads on its `paths:`)
- CI/workflow concurrency-cancel risk, sourced-bash-test-harness `set -e` leak → `.claude/rules/ci-testing-gotchas.md` (auto-loads on its `paths:`)
- `CAMERA_ACTIVE_SET` fleet-enumeration discipline (never a literal cam-number range) → `.claude/rules/camera-active-set.md` (auto-loads on its `paths:`)
- imag-nb SSH-remote tool preflight (a missing wmctrl/nm must fail loud by name, never read as a measured zero, #833) → `.claude/rules/imag-ssh-remote-tool-preflight.md` (auto-loads on its `paths:`)
- Presenter DRM device selection + dual-QR Vernier payload stability (`/dev/dri/cardN` renumbering, #854) → `.claude/rules/presenter-drm-selection.md` (auto-loads on its `paths:`)
- `recording-e2e.sh` cleanup()'s always-runs stream-latency restore composing with a late OBS write (#856) + the 703 byte-window test → `.claude/rules/recording-e2e-cleanup-composition.md` (auto-loads on its `paths:`)
- Sender-bounce reverify painter-order proof + receiver-wedge escalation (the painter-dark-vs-strih-receiver-wedge `received=` discriminator; the headless strih-OBS restart = kill+sentinel over ssh + AHK respawn, NEVER an ssh GUI launch, with the AHK-presence guard; READ_FAIL≠no-recv; the `--warm-settle` projector-independent re-check, #1093/#1096/#1098) → `.claude/rules/mv-reverify-escalate.md` (auto-loads on its `paths:`)
- On-box dead-man for PRODUCTION camera-box.service + the ExecStartPre device-free bake-in (#772): the KEY difference from cam2-painter-deadman — the burn never self-terminates, so DELAY the first fire past the run window instead of a presence-guard; + the systemd-run-action nested-quoting layering; + running compiled test binaries directly under #477 → `.claude/rules/camera-box-deadman.md` (auto-loads on its `paths:`)
- `setup-device.sh`/`verify-device.sh` companion conventions (enable-only never live-start, the `(q)`-must-stay-last check ordering, cam2-painter display-ownership, #863) → `.claude/rules/provisioning-scripts.md` (auto-loads on its `paths:`)
- Realtime isolation subsystem (merged RT-conditional affinity.rs capture-IRQ routing + verify (ac); owner REJECTED Ubuntu Pro 2026-08-20 → STAGED two-step FREE kernel path: STEP 1 = `linux-lowlatency-hwe-24.04` (config meta = preempt=full, the imag-nb precedent — on the cam boxes it also pulls a new HWE image so it is reboot-class + single-kernel purge), STEP 2 = custom PREEMPT_RT via CI only with measurement data; reboot-class supervisor-only via the rt-kernel-upgrade.sh dry-run planner + runbook; safe atomic order reboot-before-purge; per-box GRUB drift; the before/after 10-min emit-jitter+underrun journal measurement gates STEP 2; defect 2 = runbook Step B; #899) → `.claude/rules/realtime-isolation.md` (auto-loads on its `paths:`)
- DanteSync clock-offset gate (#8 precondition: HTTP-vs-journal grading paths, the #1021/#1022/#1041/#1055 master-slew false-DRIFT lineage + master-independent journal step-correlation rescue, the onset-drift recency anchor, `DANTESYNC_GATE_LINUX_{JOURNAL,HTTP}_<NAME>` fixture seams: journal env is a FILE PATH not content, distinct-`updated_ts` requirement) → `.claude/rules/dantesync-clock-offset-gate.md` (auto-loads on its `paths:`)
- Reading dantesync's own VERSION (`dantesync --version` answers on every platform, no journal/bundle-state coupling, pin-vs-relative-parity comparison model, #862) → `.claude/rules/dantesync-version-reading.md` (auto-loads on its `paths:`)
- version-integrity-gate.sh Windows-stack drift gate (the 756→758 opt-in→ENFORCED two-step, the with_sha/with_obs_identity_ok enforced-fixture injection pattern, the port4455 non-elevated-gather limitation #829/#1067, strih obs_installs/startup_chain DRIFT = #826 physical cleanup not a code bug, the disabled-build-ok Tier-0 reality) → `.claude/rules/version-integrity-gate.md` (auto-loads on its `paths:`)
- drift-guard.sh `*_from_log` parsers must be drain-safe (every `printf | consumer` pipeline ends `|| true` — awk `exit` / `head -1` early-close SIGPIPEs printf on a large log → 141 under `set -euo pipefail` kills the whole `--check-imag` run; the SIGPIPE-survival Tier-0 test pattern: `run_sourced_status` under `set -euo pipefail` + a >1 MB fixture, no cargo; issue 514 + #1189) → `.claude/rules/drift-guard-log-parsers.md` (auto-loads on its `paths:`)
- Early-gate PIN doctrine (EVERY early version/preflight gate PINS to the expected release + fails CLOSED on UNKNOWN; peer parity is a supplement not a substitute; MOVING-pin for a continuously-deployed component = read origin/main + auto-deploy so pin+fleet advance together; the pin must NOT LAG the newest release — an orphan release (published-but-not-deployed+pinned) must SCREAM, advancing pin+deploy is a mandatory release step; live audit table + holes #1137 OBS-bundle-vs-vendor / #1138 frame-probe-unpinnable / #1139 dantesync-tray+pin-lag; #1136) → `.claude/rules/early-gate-pin-doctrine.md` (auto-loads on its `paths:`)
- Reading dantesync's own VERSION (`dantesync --version` answers on every platform, no journal/bundle-state coupling, pin-vs-relative-parity comparison model, #862; sourcing the gate exposes only the PURE parser + pin, not its SSH reader, #876) → `.claude/rules/dantesync-version-reading.md` (auto-loads on its `paths:`)
- dantesync FLEET UPGRADE mechanism (canary-first per-OS-class, pin-not-latest, self-heal-not-blind-rollback, `.ps1`-file Windows path, `--ntp-master ""` single-node verify, #876) → `.claude/rules/dantesync-fleet-upgrade.md` (auto-loads on its `paths:`)
- imag-nb OBS runtime supervision + alerting (dev1-side-only alerting topology, systemd Restart=on-failure vs the issue-788 operator-fighting bug, wait-on-child-pid pattern, #882) → `.claude/rules/imag-obs-supervision.md` (auto-loads on its `paths:`)
- imag E2E record encoder (VAAPI-tex over x264/never-QSV; the WS-SetProfileParameter-needs-restart make-it-live ordering + `recordEncoder.json`-while-OBS-down; `[AdvOut]`-scoped RecEncoder read; disk-vs-live-encoder liveness via OBS-start-vs-json-mtime; the x264-vs-VAAPI-clean stop-stats parse shapes; record_render report-only carry + JSON-through-`printf %q`-ssh, #1143) → `.claude/rules/imag-record-encoder.md` (auto-loads on its `paths:`)
- `gaps` vs `residual_events` divergence (two independently-correct metrics, both directions locked by tests, #852/#883) + pulling real per-segment data from a historical CI run's own log → `.claude/rules/gap-metric-reconciliation.md` (auto-loads on its `paths:`)
- Burn-id contiguity is PRESENCE-ONLY (set-based, blind to a REPEATING/freezing hop) — the per-hop max-hold term is the separate `src/burn_hold.rs` (`MAX_HOLD_FRAMES=4`, LIVE since #870; hold input is #575-boundary-trimmed via `recording_boundary_trim::trim_boundary_pairs`); any repeat/run-length metric MUST use `burn_ids_with_frame_index_in` + break on non-adjacent `frame_index`, never `burn_ids_in` → `.claude/rules/burn-hold-uniqueness.md` (auto-loads on its `paths:`)
- phase-sync calibrator/snapshot test writes clobbering the real `~/.camera-box/phase-sync-last.json`, + recalibrating from a recent green run's own verdict JSON instead of a fresh ~1h E2E run (#893) → `.claude/rules/phase-sync-calibrator-testing.md` (auto-loads on its `paths:`)
- udev device ownership during an E2E burn run (burn-vs-production `/dev/videoN` ownership, the burn-gated hotplug rule, autosuspend re-apply on re-enumeration, the real-unplug-vs-self-heal kernel-signature discriminator, #894) → `.claude/rules/udev-device-ownership.md` (auto-loads on its `paths:`)
- self-heal reset must never misreport as frozen_leg (two independent trigger bands sharing one reset log line, ALLOW-not-SUPPRESS correlation, all-active-camera scan scope, #895) → `.claude/rules/self-heal-frozen-leg-attribution.md` (auto-loads on its `paths:`)
- Fast-capture grabber STUCK self-heal + alert (ShadowCast ~62.5 fps + persistent corrupted — `systemctl restart` doesn't clear, USB re-auth does; the corrupted band is the DISCRIMINATOR that keeps the benign over-rate wobble from reset-spamming #909; re-auth reuses perform_usb_reset gated OFF by CAMERA_BOX_GRABBER_STUCK_SELFHEAL; dev1 watchdog relays the `#1128 grabber STUCK` marker, one ping per episode, ships DISABLED, #1128) → `.claude/rules/grabber-stuck-selfheal.md` (auto-loads on its `paths:`)
- In-process USB self-heal action SEQUENCE — ONE helper `capture_rate_selfheal::attempt_self_heal(...)` that BOTH the #656/#663/#971 capture-rate trigger and the #1128 grabber-STUCK trigger call (add a 3rd trigger via a `SelfHealMessages` const, never a re-inlined copy); emitted log lines are BYTE-anchored by dev1-watchdog grep patterns (harness_capture_rate_guard / harness_self_heal_attribution_895 test scripts/lib patterns vs hardcoded samples — neither reads main.rs), so preserve substrings; the real reset is injected for Tier-0 testability, #1149) → `.claude/rules/capture-selfheal-action-sequence.md` (auto-loads on its `paths:`)
- cleanup() cambox parallel-restore group (the #1085 launch-stagger via fork-time `${#CAMBOX_PARALLEL_PIDS[@]}` index + the honest bounded-cancellation-window tradeoff; the #1085 explicit-`CAMBOX_PARALLEL_IPS` retry retiring #715's label→IP parse; the stagger-sits-ABOVE-the-ssh-anchor slicing gotcha + the `STAGGER_MS=0` timing-driver isolation, #712/#713/#715/#1085) → `.claude/rules/cambox-parallel-restore.md` (auto-loads on its `paths:`)
- ASRC bench harness (two-clock-domain simulation, the `AsrcCompensator` seam #803 mirrors, EMA convergence closed form, the RED/GREEN stub-then-restore TDD pattern, #804) → `.claude/rules/asrc-bench-harness.md` (auto-loads on its `paths:`)
- Optical undecodable floor report-only decoupling (`gates_overall_pass()` seam mirroring #914, strict-vs-relaxed independence, scoped JSON flag naming, #915) → `.claude/rules/optical-undecodable-floor-report-only.md` (auto-loads on its `paths:`)
- Walking `WINDOW_COPIES_GAPS_TOLERANCE` up/down = a DATA-FIRST step (mine the verdict per-window copies/gaps distribution, segregate pre/post-fix by the RIG-VERIFIED genlock-OBS deploy time — verdicts record no version — exclude convergence-transient + dead-painter runs, step to the tightest value the steady post-fix data supports, keep the walk-down on its own ticket; #1031) → `.claude/rules/window-gate-tolerance-walkdown.md` (auto-loads on its `paths:`)
- Changing the cross-camera `SPREAD_THRESHOLD_MS` (the ONE constant shared by the blocking SOURCE-side + report-only DELIVERY-side spread gates; its full lock-step consumer list incl. the easy-to-miss `src/lib.rs` mod-decl comments + the probe-gated `recording_verdict_merge_gate_exit_code.rs` subprocess test; the one-directional flip risk; #1120/#1121 re-tighten) → `.claude/rules/spread-threshold-consumers.md` (auto-loads on its `paths:`)
- Restoring a #904-style relaxed verdict-gate constant (RED→GREEN with no local compile path, keep the mechanism dormant not deleted, #905) → `.claude/rules/gate-allowance-restore-red-green.md` (auto-loads on its `paths:`)
- Calibrating + wiring a NEW fused-verdict gate seam (mine local verdict JSONs for the distribution, pick the tight-green-ceiling signal, rate=one-term vs count=two-terms, LIVE-vs-report-only via the cam1-grabber issue-909 empirical test, the `gates_overall_pass()` one-line seam, #1036) → `.claude/rules/verdict-gate-seam-calibration.md` (auto-loads on its `paths:`)
- Lipsync cross-check fold (issue 1032): evidence is a MANUAL paired-run campaign via `scripts/lipsync-cross-check.sh` — NOT the E2E verdict JSONs (0/144 carry it), the harness persists nothing; report-only via a COUNTER-derived `gates_overall_pass()` (bump `RECORDED_CLEAN_PAIRED_RUNS`→5, one constant, no consumer edit); Disagree fails / Agree+Unknown pass; folds in `run_av_sync` not the verdict accumulator → `.claude/rules/lipsync-cross-check-fold.md` (auto-loads on its `paths:`)
- lipsync-test-mode.sh playback is `mpv --vo=drm` NOT raw fbdev (#1187): the `--audio-delay` SIGN gotcha (negative delays video = the old positive video `-itsoffset`; flipping it doubles the offset), the fb0-blank reuses `rig_test_ledger_clean_paint_fallback_cmds`, the `LIPSYNC_MPV_BIN`/`LIPSYNC_DRM_DEVICE` seams, mpv provisioned STEP-16 + verify-device `(x2)`, live playback + 408ms recalibration are supervisor rig steps → `.claude/rules/lipsync-drm-playback.md` (auto-loads on its `paths:`)
- The cam2 Vernier `tick` decodes on EVERY cambox window on the splitter rig (CAM1/CAM3 undecodable≈0, populated presentation_cadence) — the `recording_segments.rs` "non-cam2 → tick None" doc is STALE; verify tick-decodability from verdict data, not that doc; re-confirm per-cambox before flipping any tick-based gate LIVE (#768) → `.claude/rules/cambox-tick-decodability.md` (auto-loads on its `paths:`)
- Cold-cut gate — the keepalive-bypass step (`scripts/lib/cold-cut-step.sh`: `COLD_CUT_BYPASS_CAM`/`COLD_CUT_BYPASS_INPUT` opt-in; idle-after-1st-appearance/restore-before-2nd-cut + cleanup restore-on-abort; the #767 keep-alive build keeps receivers warm off-program so a genuine cold cut needs clearing `ndi_source_name`), the `obs_phase2.py idle-receiver` primitive (overlay:True keeps the latency pin), and the report-only seam's LIVE-flip prerequisites (warm baseline + a genuine-cold run + per-cambox tick re-confirm + calibrating the sustained-fps / issue-793 segfault-discriminator constants; #768/#1086) → `.claude/rules/cold-cut-gate.md` (auto-loads on its `paths:`)
- `DockLockCorrector` hold-band boundary tuning (verify what the test suite pins before closing/widening an interval — degenerate vs ordinary margin, #942) → `.claude/rules/dock-lock-hold-band.md` (auto-loads on its `paths:`)
- Refactoring the dock-lock if/else-if/else decision chain in `sync-test-output.cpp` (stale text-anchor gates hide in a probe-gated Rust test AND both windows-genlock*.yml pwsh steps, all invisible to local Tier-0 checks, #955; a NEW emit-site guard needs its own pwsh mirror too, #999/#1005) → `.claude/rules/av-sync-dock-anchor-refactor-safety.md` (auto-loads on its `paths:`)
- Writing a NEW `tests/av_sync_dock_*.rs` twin-harness test (the `r#"..."#` raw-string vs an embedded `"#<N>:` CHECK message collision; incremental bimodal test-batch construction can trigger a premature degenerate lock, #999/#1005) → `.claude/rules/av-sync-dock-twin-harness-testing.md` (auto-loads on its `paths:`)
- Dock DISPLAY math (`dock_lock_display_offset_ms`/`dock_latency_display_ms` ↔ C++ `cb_*` parity-mirrored VALUE seam gated by `av_sync_dock_cpp_mirror_gate`; the dock locks on the STREAM box not strih — `mbc` source; quantifying dock-vs-gate residual from paired stream-log + verdict data; #952/#1004 residual UNSTABLE → no compensation) → `.claude/rules/av-sync-dock-display-parity.md` (auto-loads on its `paths:`)
- QPSK marker demod gate (`src/qpsk_marker.rs` ↔ `vendor/av-sync-dock/src/camera-box-audio.hpp`: the 20-bit word's 4-nibble redundancy — preamble 0xF / ZERO nibble bits[15:12] / index bits[11:4] / CRC-4 — gate ALL of it, #1153 reclaimed the unchecked zero nibble = 16× fewer false decodes; "98.7% CRC fail" is inherent CRC-4 physics not a bug, tune the CLUSTER not the crc_ok/crc_fail ratio; reading the live dock diag line; Tier-0 verify a gate change via a rustc replica + direct `g++` selftest) → `.claude/rules/qpsk-marker-demod.md` (auto-loads on its `paths:`)
- Inspecting AND deploying live rig state without fooling yourself (a merged `vendor/<plugin>/**` fix is NOT deployed; nested PowerShell over ssh fails SILENTLY; the painter is `frame-probe` on DRM so an all-zero `/dev/fb0` proves nothing; the plain-ssh FULL-BUNDLE deploy recipe; a hook-blocked Bash call runs none of its heredocs either) → `.claude/rules/rig-state-inspection.md` (auto-loads on its `paths:`)
- One canonical genlock deploy path across the whole rig from ONE anchor CI run id (`scripts/deploy-genlock-fleet.sh` planner: same-SHA cross-workflow resolution; Windows EMIT-only via win-* MCP, imag ssh; the AHK stop→restart-verified-BEFORE-launch contract; imag ships the WHOLE bundle per issue 1026; the shared `genlock_write_markers` marker helper + print-only retention, #789) → `.claude/rules/genlock-fleet-deploy.md` (auto-loads on its `paths:`)
- Heartbeat wedge-watchdog recipe for a NEW blocking-hardware call site that can hang forever (the D-state/no-signal-can-preempt root cause, the 4-piece pattern, threshold-vs-existing-inner-timeout sizing, the `const _: () = assert!(...)` clippy gotcha, #945/#936) → `.claude/rules/wedge-watchdog-pattern.md` (auto-loads on its `paths:`)
- Stream-box avsync watchdog + VLC monitor + dev1 heartbeat alert (Task Scheduler keep-alive idiom, generalizing the dev1-side alert topology to a heartbeat FILE, the multi-file-one-ssh-call `type ... & echo SEP & type ...` trick, bounding an external call by what needs killing, #812/#807) → `.claude/rules/avsync-monitoring.md` (auto-loads on its `paths:`)
- ssh vs win-* MCP on strih/stream — the HARD two-context rule (agent session = MCP ONLY, never ssh; headless CI/watchdog = ssh allowed but ONLY session-agnostic signals — session-0 `EnumWindows` blindness makes `MainWindowTitle` empty on a HEALTHY box, issue 958) → `.claude/rules/win-ssh-vs-mcp.md` (auto-loads on its `paths:`)
- Wake-on-LAN remote recovery for strih/stream (#1053 — dev1 `wake-box.sh` magic-packet sender + pure `scripts/lib/wol.sh` + `enable-nic-wol.ps1` NIC enable/verify; the STEP-0 finding that both NICs are ALREADY WoL-enabled so BIOS standby-power is the gap; the Realtek `*EEE` multi-element `RegistryValue` array gotcha) → `.claude/rules/wake-on-lan.md` (auto-loads on its `paths:`)
- dev1-side reachability + frozen-input alert watchdogs (probe-FROM-dev1, shared confirm/throttle, per-box/per-source state) → `.claude/rules/network-reach-watchdog.md` (reachability, #1001) + `.claude/rules/frozen-input-watchdog.md` (phase-2 frozen-input via the genlock-fifo `received=` tap + the no-double-page guard, #1052) (both auto-load on their `paths:`)
- Genlock FIFO limit-cycle diagnosis from a failed E2E verdict (frozen_leg = per-window aggregate, copies≈gaps uniform = FIFO signature, stream 2ME PGM audit deltas, frac(latency/33.3)<0.5 discriminator, date-less OBS logs, #998) → `.claude/rules/genlock-fifo-limit-cycle-diagnosis.md` (auto-loads on its `paths:`)
- Attributing a DUPLICATE / `copies` residual — painter-stall vs downstream (the painter's own `painter-*.csv` `tick,gen_ts_ns,flip_ts_ns` exonerates/incriminates stage 1 via `src/painter_pacing.rs`: painted-tick faults + missed-DRM-vsync-deadline detection; clean painter ⇒ residual is downstream optical/genlock, never a per-box fix; surfaced report-only under `all_cambox_continuity.painter_pacing`, never gates, #859) → `.claude/rules/painter-pacing-attribution.md` (auto-loads on its `paths:`)
- Changing the genlock C in libobs (CI is its first compile: the lift-and-compile-standalone recipe, the committed C-vs-Rust parity gate + why it was initially blind to a tie-break mutation, the remembered-state seam list, wrap-independent anchors, #1003) → `.claude/rules/vendored-libobs-change-safety.md` (auto-loads on its `paths:`)
- Vendored OBS FRONTEND crash-safety (`obs_data_get_json()` returns NULL on json_dumps failure → the c0000005 `ucrtbase!strcmp` / `std::string(NULL)` NULL-deref class, guard at the CONSUMER; reading a live OBS crash log on strih via win-* MCP file read incl. the `Arg0=0x0` tell; Facet A anchor + Facet B `cc` lift of a frontend `.cpp` helper; no pwsh mirror + FULL-BUNDLE deploy, #773) → `.claude/rules/vendored-obs-frontend-crash-safety.md` (auto-loads on its `paths:`)
- imag HDMI tearing → OBS projector present-vsync (the dual-output 60Hz beat; picom compositor REVERTED for 21.57% render skips; the deployed cure is OBS's own EGL `eglSwapInterval(1)` armed ONLY for the fullscreen non-multiview Program projector — never both, serial graphics thread; Linux/EGL-only no-pwsh-mirror; the one-shot `projector-vsync: present-vsync ARMED` OBS-log marker at obs_display level; SCANOUT proof needs the #781 tap, #1107/#1146) → `.claude/rules/obs-projector-vsync.md` (auto-loads on its `paths:`)
- imag HDMI Program OUT of Xorg → in-OBS vendored DRM-lease output (the forked OBS leases the HDMI connector via `xcb_randr_create_lease` and page-flips directly, no NDI hop/no presenter; module in libobs NOT a plugin — linux-genlock.yml is `ENABLE_PLUGINS=OFF`; X RandR name `HDMI-1` ≠ DRM connector `HDMI-A-1`; DEFAULT-OFF `~/.camera-box/drm-output.json`; the fsyntax-vs-real-headers net; M1=lease+solid-flip, M2 hook=Program dma-buf; #1152) → `.claude/rules/obs-drm-output.md` (auto-loads on its `paths:`)
- DistroAV NDI receiver-thread lifecycle (a `break` never clears `s->running`, so `ndi_source_update` can't revive a dead thread → permanent reattach-proof black; retry-in-place not break; the #767 watchdog makes the reset path reachable unattended; the std-only decision-helper gate pattern; do not conflate with the #1096 finder-poison wedge; #1080/#1097) → `.claude/rules/distroav-receiver-lifecycle.md` (auto-loads on its `paths:`)
- NDI `ndi_source_name` recovery (an EMPTY name STOPS the receiver thread so the in-loop #767/#1096 watchdogs can NEVER revive it — a permanent wedge fixed at the NAME layer, never vendored; the reattach CLEAR-then-SET no longer leaves ""; the ONE shared `obs_phase2.reenforce_ndi_name` policy — discoverable→set→read-back-verify→else OFFLINE, never a #795 mangle — reused by reattach's baseline re-enforce + `set-ndi-mapping --heal` + the [4c/8] self-heal; the #1133 `if`-not-bare set-e safety; #1158) → `.claude/rules/ndi-name-recovery.md` (auto-loads on its `paths:`)
- NDI SENDER port-map stability watchdog (dev1 baseline alert on a moved sender port that hands stock TVs the wrong source; avahi `-p` DECIMAL `\DDD` escapes + resolved-line field layout; the TWO NDI instances at one IP isolated by the anchor's mDNS-hostname group; baseline stores name→port only so hostname-suffix drift never matters; empty/anchor-absent map = gather error never a page; the DistroAV FINISHED_LOADING deferral root cause for the #1185 PGM-pin; python-shells-to-bash Tier-0; #1181) → `.claude/rules/ndi-portmap-watchdog.md` (auto-loads on its `paths:`)
- DistroAV SENDER-output lifecycle + NDI port ordering (ports assigned by CREATION ORDER from :5961; the program starts LAST at FINISHED_LOADING so ndi_filter republishes win the low ports; #1185 reserve-at-obs_module_post_load + adopt-in-ndi_output_start pins 2ME PGM to :5961; the begin_data_capture-fail leak that turns the pin-holder into a frameless PGM ghost; call-site-unique lock-step anchors; the frameless-idle-sender tolerance is UNVERIFIED without the rig) → `.claude/rules/distroav-sender-output-lifecycle.md` (auto-loads on its `paths:`)
- OBS window-title build identity (the `/Brepro` `__DATE__`-blanking trap → read GENLOCK_BUILD_SHA.txt via `os_get_executable_path_ptr` not cwd; the 3-copy lock-step anchor gates OBSBasic.cpp+test+both windows-genlock*.yml; FRONTEND change = full-bundle deploy not fast-dll; #152/#313/#1018) → `.claude/rules/obs-titlebar-build-id.md` (auto-loads on its `paths:`)
- Genlock hold-collapse diagnosis (A/V offset ≈ −latency_ms signature, the once-per-event backward-step latch that makes log silence lie, ceil-stamp hair-trigger, OBS-relaunch-only repair, 15-min offline arbiter recipe, Windows remote-grep traps, #1007/#1009) → `.claude/rules/genlock-hold-collapse-diagnosis.md` (auto-loads on its `paths:`)
- obs-liveness watchdog (#391) render-liveness signal — GetStats `activeFps` LIES during a render stall (it is the configured canvas fps; read 30.0 while renderTotalFrames was frozen, #935); `render_advanced` (renderTotalFrames advancement) is the true WS-only signal; the probe→gate→classify seam list; #391 does/does-not cover → `.claude/rules/obs-liveness-render-signal.md` (auto-loads on its `paths:`)
- genlock audit-log parser (`src/jitter_audit.rs` + `genlock-jitter-report`: two coexisting parser families — input `genlock-fifo audit` and #874 send-side `genlock-ndi-output`/`genlock-ndi-filter`; adding a new line kind needs a mutually-non-substring marker + Tier-0 RED→GREEN; the CLI `--json` is the #757 calibrator contract, input-side only, never add keys) → `.claude/rules/jitter-audit-parser.md` (auto-loads on its `paths:`)
- PROGRAM-render observability line (`program-render-audit:` — the 3rd OBS-log audit family beside `genlock-fifo audit` + `multiview-audit:`; the PROGRAM output's honest render_fps + renderSkipped/`lagged` delta emitted ~5s by `obs_graphics_thread_loop`; `is_render_path_jump` attributes a burn-square JUMP to render-path vs FIFO/scanout; report-only, gate is issue 798; #1029's live jump root cause is HARDWARE throttle 880/1043 — at 3 ms the jump is correct FIFO behavior) → `.claude/rules/program-render-audit.md` (auto-loads on its `paths:`)
- Offline audio-quality (THD+N) measurement (coherent-sampling window-leakage trap, the CI-only-vendor-code standalone-harness pattern, CPU-timing noise on a shared box, #929; the swr_set_compensation revert-vs-reissue proof + the stateless distance_ms-widening quantization fix, #1016) → `.claude/rules/audio-quality-measurement.md` (auto-loads on its `paths:`)
- Full-path E2E failed-gate Discord alert — derive the failed STAGE from durable per-run artifacts (verdict-<RUN_ID>.json + downloaded recordings), never hardcode a frame-loss verdict; + local shell-helper verification (cargo tests don't run locally, build-ok disabled #477) → `.claude/rules/e2e-failure-alert.md` (auto-loads on its `paths:`)
- E2E per-run Discord report — TWO renderings (`compose_summary` short/`--json-chunks`/Discord vs `compose_report` full-detail/plain/CI-log); the caller captures stdout `2>&1`+jq so `--json-chunks` must print ONLY the JSON array; blocking-vs-report-only is DERIVED from each verdict-JSON seam's `gates_overall_pass` mirroring recording-verdict.rs's `all_pass` fold (SOURCE spread blocks, DELIVERY spread is report-only; report-only NEVER `❌`; PASS=3 lines); #711/#1127 → `.claude/rules/e2e-discord-report.md` (auto-loads on its `paths:`)
- E2E recordings retention — dry-run-first sweep of the OBS record dir (strih `D:\_REC`, ~691 GiB of old runs vs the 50 GB budget): pure `src/recordings_retention.rs` decision (keep newest-N runs UNION younger-than-D-days; EXPLICIT OBS-timestamp allowlist, NEVER a generic `*.mkv` sweep — `strih700105.mkv` stays PROTECTED) mirrored by `scripts/strih-recordings-retention.ps1` (scp -O + `powershell -File`, DRY-RUN default, `-Execute` = supervisor's reviewed step); #1122 → `.claude/rules/recordings-retention.md` (auto-loads on its `paths:`)
- OBS deploy/backup DIRECTORY retention (#789 residual B / criterion 5 — a DIFFERENT artifact than #1122 recordings): the fleet deploy leaves dated `<stamp>-789` box-backups (`C:\obs-backup` / `/opt/obs-backup`) + per-sha stage dirs (`stage-genlock-*` under `C:\` / `genlock-stage-*` under `/tmp`) behind, swept only inline during a `--yes` deploy. Standalone dry-run-first sweep: pure `src/obs_backup_retention.rs` decision (keep newest-N PER KIND UNION younger-than-D-days; EXPLICIT allowlist, `previous/` + operator dirs PROTECTED, never a generic sweep) mirrored by `scripts/obs-backup-retention.ps1` (win) + the `--local-sweep` bash decision in `scripts/obs-backup-retention.sh` (imag); DRY-RUN default, `--execute`/`-Execute` = supervisor step; NOTHING wired automatically → `.claude/rules/obs-backup-retention.md` (auto-loads on its `paths:`)
- Absolute e2e latency + freeze bounds — the MAIN E2E (recording-verdict) vs LOOPBACK E2E (frame-probe/differ) are SEPARATE gate subsystems; calibrate bounds from `/tmp/recording-e2e-*/verdict-*.json`; crate-root `gates_overall_pass()` seam; cam→stream ~1s hold is by design, freeze=frozen_leg already report-only (#1035) → `.claude/rules/e2e-latency-gate.md` (auto-loads on its `paths:`)
- imag leg recording verdict — SPLIT since #1142 (was issue-798 report-only): PRESENCE/VERIFICATION (`imag_leg_gate::gates_overall_pass()`=true — imag_leg_verified [offline-ack #1013 exempt] + span + undecodable floor, only when `--require-imag-leg`) BLOCKS; PER-FRAME CONTENT (`content_gates_overall_pass()`=false — burn contiguity + optical beat + per-segment sweep) stays REPORT-ONLY pending the #1143 encoder fix (issue 1130 x264 observer effect); `partial_schema_gate::box_degrades_on_schema_mismatch` decoupled (schema-degrade still degrades but REDs via imag_leg_verified). The imag verdict only FLOWS when recording-e2e.sh `[8/8c]` succeeds (0/76 historically), so check `full_chain.imag_leg_verified` before trusting a green run → `.claude/rules/imag-leg-report-only.md` (auto-loads on its `paths:`)
- imag-nb power/thermal envelope (the MMIO RAPL PL1 25W clamp diagnosis, identity-based zone selection, the source-only shared gather+verdict+guard-decision lib, the check_imag_report optional-arg convention, step-at-the-END TOTAL_STEPS rule, #1040) → `.claude/rules/imag-power-envelope.md` (auto-loads on its `paths:`)
- imag display-path drift facets (the box is Intel-iGPU-only — NVIDIA-era knobs obsolete; picom-off/igpu-maxperf/tap-conf as OK/DRIFT/UNKNOWN facets in the shared `imag-display-path.sh` lib, consumed by drift-guard `--check-imag` #10 AND the E2E `[0/8]` preflight; PICOM_PGREP inline-marker vs the shared require-tool helper, #780) → `.claude/rules/imag-display-path.md` (auto-loads on its `paths:`)
- strih/stream network-UNREACHABLE alert (dev1-side watchdog that probes the two OBS boxes FROM dev1 — never ssh IN — with a multi-signal ping OR :4455 OR :8899 check; REACHABLE iff ANY; 2-pass confirm + shared obs_watchdog throttle; per-box state; reference-anchor guard against a dev1-side path outage; recovery ping; closes the 50-min silent-strih-outage gap, #1001) → `.claude/rules/network-reach-watchdog.md` (auto-loads on its `paths:`)
- strih/stream :8899 BundleStateServer health-check + AUTO-RESTART (dev1-side watchdog: curl :8899/bundle-state.json FROM dev1 — not MCP Invoke-WebRequest which hangs; box-up=ping|:4455 EXCLUDES :8899; auto-restarts the task via session-agnostic `schtasks /run` over ssh — never `/it`; the auto-restart-vs-alert-only discriminator vs obs-liveness; require_tools fail-loud; defers a fully-dead box to #1001; ships DISABLED, #732) → `.claude/rules/bundle-state-watchdog.md` (auto-loads on its `paths:`)
- non-60 source-cadence alert (dev1-side watchdog: strih `genlock-fifo audit received=` DELTA ÷ the audit lines' OWN timestamps — the #797 phantom-50.1 avoidance, NEVER a wall-clock divisor; the `@fps` decoration is the useless CANVAS fps, not per-source; watch STRIH cameras @60±3, a frozen source is UNKNOWN not "wrong 0 fps" (defers to #1052); reuse obs_watchdog confirm/throttle + #1001 no-double-page + require_tools fail-loud; the duplication-masked 50→60 hard layer is a follow-up; ships DISABLED, #794) → `.claude/rules/cadence-watchdog.md` (auto-loads on its `paths:`)
- Re-pinning a probe-gated `ReleaseCadence` mirror against OBSERVED output (the authority-importing default-feature replica — Rust analogue of the vendored-C lift-and-compile; the phase-anchored-selection==newest-due-when-anchor-unset finding; fmt-check parses probe code; demonstrative set-anchor tests, #1037) → `.claude/rules/probe-mirror-replica-testing.md` (auto-loads on its `paths:`)
- Cam2 optical-injection-leg health (dead-painter/optical-black detection, alert, fail-fast: the dev1 alert-watchdog framework reuse, the pidfile-OR-service TEST/EVENT discriminator, reusing #901 assert-program-nonblack, the never-false-abort [0/8] preflight, the #712 CAMBOX_PARALLEL_FAILED_LABELS ::error:: surface, #860) → `.claude/rules/optical-chain-health-watchdog.md` (auto-loads on its `paths:`)
- CG-bridge (Spout republish) black-on-air detection — DIFFERENTIAL not blanket (Arena publishes a black CG-bridge Spout while its upstream NDI is live; a blanket "every scene non-black" gate false-fails on legitimately-idle overlay scenes like CG bridge/Ableset; page ONLY when upstream LIVE + republish BLACK via `obs_phase2.py republish-black-check`; dev1 alert reuses obs-watchdog-decision.sh, ships disabled; Arena state = read-only win-strih MCP Snapshot, never a control op, #1006) → `.claude/rules/cg-bridge-republish-black.md` (auto-loads on its `paths:`)
- cam2 painter LIFECYCLE — who paints /dev/fb0 + emits the QPSK marker in each state (permanent supervised cam2-painter.service = durable steady state; the transient nohup = verification-only; rig-mode test HANDS OFF to the unit via cam2_painter_steady_state_handoff_cmds, event disables it #892, recording-e2e stops+restores it #872, the marker-log unit flag, #1008/#937) → `.claude/rules/cam2-painter-lifecycle.md` (auto-loads on its `paths:`)
- /dev/fb0 blank-on-teardown + frame-probe graceful shutdown (BOTH KmsPresenter::Drop #660 AND VsyncFb::Drop #1186 blank via blank_fbdev; the SIGTERM/SIGINT/SIGHUP handler in src/shutdown.rs sets a flag the paint loops poll so the existing Drop teardown runs; HARD invariant — never blank fb0 directly from a signal handler, drive it through the presenter Drop; Tier-0 pure half via rustc replica, #1176/#1186) → `.claude/rules/fb0-blank-on-teardown.md` (auto-loads on its `paths:`)
- Measurement-burn OFF/CHECK/RESTORE target set — ENUMERATE ndi_source inputs over WS (obs_burn_filter.py sweep-check/sweep-off), never a static or CAMERA_ACTIVE_SET list; fail CLOSED on a failed GetInputList (#938/#1011 leak, guard class #246/#844) → `.claude/rules/burn-target-enumeration.md` (auto-loads on its `paths:`)
- dev1 fresh-OBS-start burn reconciliation for UNATTENDED strih/stream starts (renderTotalFrames restart signal via obs_burn_filter session-probe; discriminator is a FRESH START not burn-presence — a persistent TEST burn is legit; durable ~/.camera-box baseline never tmpfs; defer while #281 heartbeat/#830 lease coordinate; unresolved-retry; fail-closed; #1060) → `.claude/rules/obs-burn-reconcile-watchdog.md` (auto-loads on its `paths:`)
- Reading/verifying per-source genlock latency pins (authoritative key is `genlock_latency_ms_src` over WS, NOT bundle-state's DistroAV-stock `ndi_input_latency`=0; verify-at-start is REPORT-ONLY vs `scripts/latency-pins-baseline.json`; fail-closed enumeration vs honest-None per-input read, #1061/#866) → `.claude/rules/latency-pins-verify.md` (auto-loads on its `paths:`)
- Measurement-window per-camera A/V equalization (`MEASUREMENT_EQ=1` opt-in harness profile: DELIVERY-derived deep pins + coherent hold snapshot-restored for the measurement window only, production untouched; the #900 re-anchor's PREVIEW basis is wrong for A/V so it's a separate profile; forces re-anchor off + replaces the #893 floor gate with a read-back verify; three-way classify_leftover fail-loud-on-stale; the pin-dependent cam→strih p99 bound must rise with the cam2 pin; #1003) → `.claude/rules/measurement-eq.md` (auto-loads on its `paths:`)
- Floor-3 per-run camera auto-align (`qr_align_pins.py` + the BLOCKING `[4i/8align]` E2E step): the simultaneous painter-QR `gen_ts_ns`+`t_send` spread signal (NOT `frame_id×fps`); floor-3 relative-only model; sanity bound must stay BELOW the owner's 94ms nonsense; #893 active-floor is mutually exclusive with it; `CAMERA_ALIGN_SET` superset incl. cam4 minus acked-offline; strih pins only; the revert's dangling deep-pin fixtures; #1003) → `.claude/rules/qr-align.md` (auto-loads on its `paths:`)
- Per-cambox HDMI-splitter-port no-signal recurrence watch (dev1-side timer reads each ACTIVE cambox's last `capture chroma:` journal line; SELF-ANCHORING discriminator — PAGE (`DEAD_PORT`) only when a box is capturing-but-GRAYSCALE AND ≥1 sibling on the SAME camera+splitter is proven-good; `capturing=0` = `NO_CAPTURE` report-only, NOT a splitter-port page — it is the routine cambox-down/device-busy/E2E-stop class, a mis-attribution otherwise; all-grey = `SOURCE_WIDE` report-only; the FIRST check for "weird colours on some cameras" = per-box signal presence not card tuning; Elgato purple-noise residual; #739) → `.claude/rules/splitter-port-health-watchdog.md` (auto-loads on its `paths:`)
- MV-clone-vs-main presentation SKEW via OBS-WS screenshots (the `t_send` latch-timing + local-wall-gap compensation — RPC-midpoint stamping is the noise source; universal-painter vs cam1-burn run_id; shared-source regression-guard reframing; imag WS uses OBS_PASSWORD not IMAG_PW, #761) → `.claude/rules/mv-skew-measurement.md` (auto-loads on its `paths:`)
- Venue-switch config-drift audit + checked-in baseline (`netcfg` facet: read-only `admin@` ssh to the MikroTik chain — RB4011 router_snv 10.77.8.1 + 4× CRS310 10.77.9.2-.5; `scripts/netcfg-audit.sh --capture/--check/--json` diffs live shared-buffers/ROS/per-port rate+role vs `scripts/netcfg-baseline.json` + a live drop-RATE probe; report-only dev1 hourly alert-watchdog reusing obs_watchdog confirm/throttle, ships DISABLED; guards the KEPT #797 shared-buffers 40→80% microburst fix against a silent revert; pw NEVER committed → EnvironmentFile; the `python3 - <<'PY'` stdin-collision + `print stats` columnar-vs-`where name=` gotchas, #797) → `.claude/rules/netcfg-audit.md` (auto-loads on its `paths:`)
- imag-nb offline-ack leg skip (imag ackable as box name "imag" via cambox-offline-ack.sh; the FULL imag hard-abort site inventory in recording-e2e.sh a "make imag optional" change must cover; the imag-vs-imag-nb naming split across gates; IMAG_OFFLINE_ACKED guard shape + #798 marker 3rd arg, #1013) → `.claude/rules/imag-offline-ack.md` (auto-loads on its `paths:`)
- MV-fps observability + live alarm (the `multiview-audit:` emit → `mv_audit.rs`/`mv-fps-gate` → dev1 `mv-fps-alert-watchdog.sh`; DATA-FIRST floor calibration mining live `rendered_fps` per box; the `target−tol` floor tight for BOTH boxes (#776 retargeted it from `canvas/2−tol`, which was loose for divisor-1 strih → floor 13 missed moderate collapses; now floor 28 both); strih 4K MV collapses under CONTENTION not itself; autostart-restart-reset via log-id change; the `cargo build --release`-in-comment Tier-0-hook trap, #771/#1083) → `.claude/rules/mv-fps-watchdog.md` (auto-loads on its `paths:`)
- ASIO-source-starved alert (dev1-side watchdog: the NEW `asrc: source '<name>' … starved_blocks=N` tap — per-interval reset-on-read, NOT jitter_audit.rs's genlock-fifo family; healthy=0 vs starved~2946 sustained; the healthy-SIBLING discriminator = per-source defect vs box-wide UNKNOWN owned by obs-liveness/audio-presence; watch REAL inputs only — synthetic test-audio/fallback-repro excluded; alert-only OBS-reset cure since the closed VB-Matrix/Dante plugin is upstream of vendor/; complementary to the #786 launch guard; ships DISABLED, #1023) → `.claude/rules/asio-starve-watchdog.md` (auto-loads on its `paths:`)
- Rig status page (`scripts/rig-status.py` — dev1-hosted RENDERER over `rig-health-audit.py`, NEVER a second prober; the GENERIC key=value parser so a new feeder facet renders automatically — build-sha #789 belongs in the FEEDER not here; the FALSE-GREEN guard `overall_state()` — empty/crashed/timeout audit = ERROR + a page, never silent PASS; Discord dedup reuses obs-watchdog-decision.sh; serve-dir vs state-dir split; ships DISABLED, tailscale bind; pytest Tier-0 via importlib; #787) → `.claude/rules/rig-status-page.md` (auto-loads on its `paths:`)
- NTP-client DSCP nftables rule (dantesync issue 52 provisioning half: dedicated `table ip dantesync_dscp` OUTPUT-mangle rule + `dantesync-dscp` boot oneshot, NOT the distro nftables.service; EF/46 must match dantesync src/dscp.rs; nft needs ROOT NETLINK even for `-c` so render/validate fixtures via `sudo unshare -n`; the never-flush test checks a non-comment DIRECTIVE not the substring; verify check (ae) stays before (q); enable-only) → `.claude/rules/dscp-nft-provisioning.md` (auto-loads on its `paths:`)

## DO NOT DELETE These Files

**NEVER delete `targets.md`** — it contains IP addresses for all deployment targets (Windows and cameras). This file has been accidentally deleted multiple times during PR cleanup. DO NOT remove it.

## GOTCHA — `fix: #N ...` commit prefixes auto-close #N on ANY merge it rides along in

This repo's convention tags commits with `fix: #N <description>` / `feat: #N <description>` as a
plain topic reference. A **regular (non-squash) merge** makes GitHub scan **every individual
commit** in the merged range for closing keywords — not just the merging PR's own body. GitHub's
keyword matcher accepts `fix`/`close`/`resolve` immediately followed by `#N` **even across a bare
colon** (`fix: #458` matches), so a `fix: #458 ...` commit that has been sitting on `dev`,
UNMERGED, for a prior ticket will silently auto-close `#458` the moment it finally rides along in
ANY later PR's merge — even one whose own body only says `Closes #459`/`Closes #461` for
completely different issues. **Incident (2026-07-03):** PR #468 (bundling #459+#461) merged and
GitHub auto-closed **#458** too, even though #458 carried an explicit "stays OPEN until the rework
lands — do not let a PR merge auto-close it early" comment; three earlier `fix: #458 ...` commits
from a prior session's WIP were still unmerged on `dev` and rode along. Reopened + explained in
`gh issue comment 458`.

**Mitigation:** when a `fix:`/`feat:` commit message must NOT auto-close its referenced issue on a
future merge (the work is genuinely partial / multi-PR), phrase it so the keyword and `#N` are NOT
adjacent — `fix(#458): ...`, `fix — #458: ...`, or drop the leading verb entirely (`#458:
description`). Before merging any PR, `git log origin/main..HEAD --oneline` and grep for
`^(fix|close|resolve)[a-z]*:\s*#` to catch a stray reference-only commit that would trigger an
unwanted auto-close.

**Extension — the SAME trap fires from a PR title/body, and NEGATION DOES NOT PROTECT YOU
(incident 2026-07-05, #504/PR #539):** GitHub's closing-keyword matcher scans the merging PR's
OWN title and body too, not just commits — and it is a bare substring match with **no negation
parsing**. A PR body written to explicitly scope a partial/code-only PR — *"...it does NOT close
#504"* — still auto-closed **#504** on merge, because the literal substring `close #504` is
present regardless of the preceding "does NOT". Every commit message that session had already been
checked clean (`git log origin/main..HEAD | grep -iE '(fix|close|resolve)...#[0-9]'` → none), so
the commit-message mitigation above is NOT sufficient by itself — the PR title AND body need the
same check. **Before opening/editing a PR that must NOT close an issue it merely references, grep
the PR title+body text itself** (not just commits) for `(close|closes|closed|fix|fixes|fixed|
resolve|resolves|resolved)\s*#[0-9]` and rephrase any hit — including a NEGATED one — so the verb
and `#N` are not adjacent (e.g. "the live purge for #504 is separate" instead of "does not close
#504"). Recovery: `gh issue reopen <N>` + a `gh issue comment <N>` explaining the accidental
auto-close (see issuecomment-4887235757 on #504 for the template).

## GOTCHA — `git commit -m "..."` with literal backticks in a DOUBLE-quoted message is mangled

This repo's commit messages routinely reference code with backtick-quoted spans
(`` `function_name()` ``, `` `field_name` ``, `` `4/2-1=1` `` style arithmetic) — exactly the style
`gh-cli-recipes.md` already warns about for `gh issue create --body`, but the SAME shell mangling
hits **any** `git commit -m "..."` when the message is a double-quoted string: bash treats each
backtick pair inside double quotes as command substitution and silently replaces it with that
"command"'s (usually empty) output, deleting the quoted text. **Incident (2026-07-07, PR #587):** a
commit message written as `git commit -m "...dropped the now-unused \`step\` param..."` (plain
double quotes, no heredoc) landed with `` `step` `` silently deleted (`Bash completed with no
output` plus a stray `step: command not found` on the terminal) — the word vanished from the
committed message, and every OTHER backtick-quoted span in that same message lost its backticks
too, even where the "command" happened to fail silently instead of printing an error.

**Mitigation:** for ANY commit message containing a backtick, `$`, or `%`, use the same
quoted-heredoc pattern the global commit-conventions template already shows:
```bash
git commit -m "$(cat <<'EOF'
fix(#N): message with `backticks`, $VARS, and 100% safe symbols

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)" -- <exact paths>
```
The single-quoted `'EOF'` delimiter disables ALL shell expansion inside the heredoc body — backticks,
`$(...)`, and `%` all pass through literally. A plain `git commit -m "..."` is safe ONLY when the
message contains none of those three characters; once you write a single backtick, switch to the
heredoc form for the WHOLE message, not just the backtick-containing line. **Never amend/rewrite a
commit that shipped with a mangled message** (`commit-conventions.md` — no history rewrites); the
next commit's message is where you note the correction if it matters.

## GOTCHA — mentioning ANY OTHER `#N` in a commit message (even just as prose context) can BLOCK the commit on that ticket's own design gate

The autopilot design-before-code hook (`block-commit-without-design.sh`) scans the WHOLE `git
commit` command text for issue references — its regex is `(?:^|[\s(/])#([0-9]+)\b`, i.e. ANY
`#N` preceded by whitespace, `(`, `/`, or the string start, not just the commit's OWN `fix(#N):`
prefix. **Every distinct `#N` it finds must already have a design comment posted, or the commit
is BLOCKED** — including a number you only mentioned in passing to explain WHY a rule exists
(e.g. writing "the standing #836 rule: never widen the tolerance" while fixing `#855`). Live
incident (2026-07-28, `#855`): a GREEN commit's body referenced `(#836: never widen the
tolerance...)` — a completely different, already-resolved ticket, cited only for context — and
the hook blocked the commit demanding a design comment on `#836` too.

**The scan covers the WHOLE Bash call, not just the `-m` message — a `cat >> file <<EOF`
heredoc in the SAME call as a `git commit` trips the gate on `#N` refs the heredoc merely
writes into a FILE** (live: appending a docs/autopilot-log.md entry naming other tickets +
committing it in one call, 2026-08-07; and per the rig-state-inspection rule, the BLOCKED call
runs nothing — the append never happened either). Split file-writing and `git commit` into
separate Bash calls when the written content names other tickets.

**Fix: never write a bare `#N` for an issue OTHER than the one(s) this commit's own design
comment already covers.** Reference the RULE, not the ticket number, in commit-message prose
("the standing gate-strictness rule: never widen the tolerance" instead of "the `#836` rule") —
or, if the number must appear, keep it out of the `(?:^|[\s(/])#\d+` shape (e.g. spelled as
`issue 836` with no `#`, which the regex does not match). This does NOT apply to the commit's OWN
ticket number(s) in the conventional `fix(#N):` / `test(#N):` prefix — those already have (or are
about to get) their design comment via the normal flow; it is specifically OTHER tickets casually
named in the body that trip the gate.

## GOTCHA — two autopilot workers sharing this dev1 checkout WILL interleave on `dev`

`~/devel/camera-box` is a single shared clone with **no git worktree isolation** — every worker's
`git commit`/`git push` operates on the SAME local checkout, the SAME git index, and the SAME
local `dev` branch ref, not just the same remote branch. If the supervisor ever dispatches two
workers into this repo at once (violates `two-branch-workflow.md`'s "dispatch serially — one
active worker per repo", but has happened), their commits interleave on one linear history with
no isolation and no conflict warning.

**Incident (2026-07-04):** worker A (#499+#500, `setup-imag.sh`) and worker B (#505, a GL
PBO-orphan fix) both committed to `dev` concurrently. Worker A protected its pushes by pushing an
exact commit SHA (`git push origin <own-sha>:refs/heads/dev`, never a bare `git push origin dev`)
so B's not-yet-pushed commits weren't dragged to `origin` prematurely — but a `git commit` A ran ON
TOP of B's already-advanced local HEAD unavoidably included B's ancestry on the next push (a push
always carries a commit's full ancestor chain; excluding mid-branch commits needs a banned
force-push). Net result: A's PR ended up also shipping B's fully-complete #505 work, auto-closing
it via B's own `fix: #505 ...` commit title. Harmless here (B's work was genuinely finished +
TDD'd), but in a worse timing it could ship a STILL-IN-PROGRESS body of foreign work through the
wrong PR with no review of it.

**Consequences + mitigations, confirmed live:**

- **A stray untracked/modified file you didn't create can appear in `git status`.** NEVER
  `git add -A`/`git commit -a` — stage and commit ONLY the exact paths you touched, in the same
  breath: `git commit -m "..." -- <path>` (the pathspec form commits ONLY that path's
  working-tree content, ignoring anything else staged, and leaves the other worker's staged
  changes untouched). If a sweep still happens (`git show --stat HEAD` shows a file you never
  edited), `git rm --cached <path>` in a follow-up commit restores it to untracked — never `git
  rm`/delete it from disk, it's someone else's live work.
  - **GOTCHA (confirmed live, 2026-07-11, PR #692 / #684): `git commit -m "..." -- <path>`
    commits that path's FULL CURRENT working-tree content, not just what you `git add -p`'d for
    it.** This matters even OUTSIDE the two-worker scenario, any time you need TWO separate
    commits (e.g. a RED test commit then a GREEN fix commit, `regression-test-first.md`) touching
    the SAME file with only PART of your edits ready for the first commit. Selectively staging
    hunks via `git add -p <path>` (answering `y`/`n` per hunk) then running
    `git commit -m "..." -- <path>` does **NOT** commit only the staged hunks — it commits the
    path's CURRENT ON-DISK state, staged-or-not, silently pulling in the unstaged hunks too. Live
    incident: staging only #682's hunks in `scripts/recording-e2e.sh` via `git add -p`, then
    running `git commit -- scripts/lib/imag-scene-route.sh scripts/recording-e2e.sh` (intending
    to land ONLY the #682 fix), silently also committed the NOT-YET-STAGED #684 final-verify
    block still sitting in the working tree — collapsing two intended separate RED→GREEN pairs
    into one commit. **Never git-history-rewrite to fix this** (`commit-conventions.md`) — the
    clean recovery is a NEW commit pair: temporarily `Edit` the file to REMOVE the
    accidentally-early hunk (recreating the true pre-fix state), commit that as the RED test
    commit, then re-add the removed hunk as its own GREEN commit. To avoid it going forward: when
    you need ONLY the staged hunks of a partially-staged file, commit with **no pathspec at all**
    (`git commit -m "..."`, which commits exactly the INDEX) rather than repeating the file's path
    — the pathspec form is for "commit this path's CURRENT state", not "commit what I staged for
    this path".
- **Before every `git commit`, `git log --oneline -3`** to confirm HEAD is still what you expect
  — if it shows commits you didn't write, the other worker advanced the shared branch under you.
- **A push by EITHER worker pushes the local `dev` ref as it stands**, including the other
  worker's already-made local commits (both share one ref). There is no way to "un-push" someone
  else's commits without a banned force-push/history-rewrite; NEVER `git reset`/force-push to try
  — you'd be mutating a ref the other process may still be relying on mid-operation. If it
  happens, check `gh issue view <N>` for whether it changed which issue(s) a shared PR
  auto-closes (see the GOTCHA above) and adapt the PR body / commit wording rather than fighting
  the git state.
- **GitHub allows only ONE open PR per (head, base) branch pair.** If another worker's `dev`→`main`
  PR is already open when you're ready to push, you CANNOT open a second one — wait for theirs to
  reach a terminal state (poll `gh pr view <N> --json state,mergedAt`) before pushing, or your
  commits just fold into their PR's diff.
- **A later push cancels an in-flight `linux-genlock.yml` run via its
  `linux-genlock-${{ github.ref }}` concurrency group — even if the later push doesn't itself
  touch `vendor/**`** (the group keys on the ref, not the paths). A cancelled run is NOT a build
  proof — re-trigger manually once the ref is stable: `gh workflow run "Linux genlock build
  (vendored OBS + DistroAV, imag-nb parity)" --ref dev`.
- **`linux-genlock.yml` only triggers on push to `dev`, never `main`** (`on.push.branches` is
  `[dev]` only) — after a dev→main merge, main never automatically gets a genlock build. If you
  need proof the exact merged main state compiles, trigger it explicitly with `--ref main`.
- Note the collision plainly in your evidence block/autopilot-log entry, and if a foreign commit
  auto-closed an issue that wasn't yours, explain it via `gh issue comment <N>` for traceability.
  The supervisor should prefer serial dispatch or per-worker `git worktree` isolation for this
  repo going forward.
- **Worktree FLEET rounds (the #317 default) — proven 2026-08-11, with two hard constraints.**
  The `EnterWorktree` TOOL refuses inside a worker here (`worktree.bgIsolation: "none"`), but
  plain git CLI worktrees work: `git -C <repo> worktree add <repo>/.claude/worktrees/<name> -b
  <branch> dev`, then the worker does ALL file edits by ABSOLUTE path under the worktree and all
  git via `git -C <worktree>` — its process cwd never changes, and it must NEVER `git checkout`/
  `switch` in the shared root. Constraints learned the hard way: (1) **workers run TARGETED tests
  only; the supervisor runs the ONE full `cargo test` suite on merged dev at integration** — three
  workers each running the full 200-binary suite concurrently built 3× ~4.5 GB worktree `target/`
  dirs, filled the disk to 100% and oversubscribed the 4-core box; (2) at integration, DELETE each
  worktree's `target/` immediately and `git worktree remove` + branch-delete once the round PR
  merges. Expect trivially-resolvable append-append conflicts in `docs/autopilot-log.md` (keep
  both entries) and identical version-bump commits across workers (merge clean).
- **A red "CI" run on the OTHER worker's push can be YOUR OWN not-yet-fixed RED commit riding
  along, not a real regression in their code.** Confirmed live (2026-07-30, #854/#881 vs #878):
  worker A committed a TDD RED commit (failing-on-purpose, per `regression-test-first.md`) while
  worker B's own commit landed on top of it in the shared local history; when B pushed next, the
  push carried A's still-broken RED commit too (a push always carries full ancestry — see the
  bullet above), and B's CI run failed on TESTS A HAD NOT YET FIXED. Before treating a scary CI
  failure on a push you didn't make as evidence the other worker's code is broken, check the
  failing test names against YOUR OWN recent RED commit (`git log --oneline` around the failing
  SHA) — if they're yours, it's a superseded interleaving artifact, not a real bug; your own next
  push (carrying the GREEN fix) supersedes it and should be judged on its OWN CI run instead.

## GOTCHA — editing `scripts/recording-e2e.sh` (OR `scripts/rig-mode.sh`) can silently break OTHER test files' static anchors

Many separate `tests/harness_*.rs` / `tests/rig_mode.rs` files independently `.find()`/`.split()`
literal substrings/adjacency in `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh` (a banner like
`"[5/8] StartRecord"`, a bare function-name anchor like `.split("do_event()")`, or structural
adjacency like `fi\ntrap cleanup EXIT`) to pin ordering/structure — the same static-string-assertion
model `tests/harness_recording_e2e_cleanup_resilient.rs` (#328) established, now reused across many
unrelated features in BOTH files (`#137` av-restart-sync, `#286` all-cambox burn targets, `#649`
StopRecord ordering, `#524`'s `event_mode_calls_stop_stray_recordings_guard`, etc.). A new comment
or code line you add to EITHER file CAN accidentally (1) duplicate a literal anchor another test's
`.find()`/`.split()` relies on — `.find()` returns the FIRST match in the WHOLE file, and
`.split(X).nth(1)` grabs the segment AFTER the SECOND occurrence of `X`, not the occurrence near
your own edit — or (2) break a textual adjacency two unrelated tests hard-code (e.g. inserting a
line between an `if` block's closing `fi` and the following `trap cleanup EXIT HUP INT TERM` line).

**Confirmed live (#649, 2026-07-10, recording-e2e.sh):** a new `cleanup()` comment containing the
literal text `[5/8] StartRecord` broke 3 tests in `tests/harness_av_restart_sync_gate.rs` (the
`#137` gate, which anchors on that exact string to slice "everything before the main record
step"); and adding new variable declarations directly before `trap cleanup EXIT HUP INT TERM` broke
`recording_e2e_all_cambox_extends_burn_targets_to_every_strih_input_286` in
`tests/harness_recording_e2e_paths.rs` (which hard-codes that the `#286` ALL_CAMBOX block's
closing `fi` is immediately followed by `trap cleanup EXIT`).

**Confirmed live (#722, 2026-07-13, rig-mode.sh — the SAME class, a DIFFERENT file):** a new
comment reading "...sent by `do_event()` AFTER this function returns..." — placed INSIDE
`event_mode_assert()`, BEFORE the real `do_event() {` definition — broke
`event_mode_calls_stop_stray_recordings_guard` in `tests/rig_mode.rs`, which extracts the function
body via `s.split("do_event()").nth(1)`. With TWO occurrences of the literal text `do_event()` (the
new comment, then the real definition), `.nth(1)` grabbed the segment BETWEEN them (a few lines of
comment) instead of the real function body — the test's failure message printed the WRONG slice
(the comment text), which is the tell: if a `.split()`/`.find()`-based test's failure output looks
like the wrong region of the file, suspect a duplicated anchor, not a logic regression. Fix: reword
the comment so it never contains the bare function-name-with-parens text (e.g. "the EVENT-mode
caller" instead of "`do_event()`") when that text sits BEFORE the real definition in the file.

**Confirmed live (#832, 2026-07-27) — the anchor you break can be a test YOU are writing IN THE
SAME PR, not just a pre-existing one.** Adding an explanatory comment right before a call site
(`# #832: recording-verdict-on-imag.sh has its OWN independent IMAG_BOX default...` right before
`"$HERE/recording-verdict-on-imag.sh"`) created a SECOND occurrence of the literal script name —
a NEW test's own `s.find("recording-verdict-on-imag.sh")` then latched onto the comment (the FIRST
occurrence) instead of the real invocation a few lines later, so its assertion window never
reached the actual call. Same failure shape hit a second, unrelated anchor in the same PR:
`rig-mode.sh`'s pre-existing explanatory comment already said `` `scripts/drift-guard.sh
--check-imag` `` (backticked, no `bash ` prefix) several lines above the REAL
`bash scripts/drift-guard.sh --check-imag ...` call — a naive `.find("drift-guard.sh
--check-imag")` grabbed the comment, not the call. Fix in both cases: anchor on a substring that
can ONLY appear at the real call site (the quoted `"$HERE/...").sh"` invocation form, or a
`bash ` / other prefix the comment never uses) — never a bare script/flag name that a nearby
comment could also contain. **The general rule: when you write a NEW static-anchor test against
one of these two files in the SAME commit/PR that adds explanatory prose near the call site,
verify your OWN anchor is unique too — you can self-collide, not just collide with someone else's
existing test.**

**Mitigation:** after ANY edit to `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh`, run the FULL `cargo test` suite —
not just your own new/targeted test file — before pushing. Since the `# airuleset:build-ok` bypass
is DISABLED here (#477, see Local Build Policy below), do this by `cargo test --no-run` (compiles
every binary, allowed) then running each affected compiled binary DIRECTLY from
`target/debug/deps/…`. A failure elsewhere in the suite right after touching this
file is very likely a textual collision, not a real regression — grep the failing test's
`.find(...)` argument (or the surrounding slice logic) to see which literal string or adjacency
moved, then reword your new text (or relocate it) so it no longer matches/breaks that anchor.

**Prevention pattern (#675) — ADD new behavior via a sourced helper, never edit the literal
anchor line itself.** When the new logic needs to run right after an EXISTING pinned line (e.g.
"verify camera-box came back active after `systemctl restart camera-box`"), don't touch that
line's text at all — append a call to a NEW function in a NEW `scripts/lib/*.sh` file via command
substitution on the line(s) immediately after it (`$(my_new_helper_cmds "label")`). The static
anchor test suite reads ONLY `scripts/recording-e2e.sh`'s own text, never a sourced lib's — the
function CALL is invisible there (compile-time text), but its expanded OUTPUT still lands in the
final remote command at actual runtime. This adds a whole new capability with ZERO risk to any
existing `.find()`/adjacency assertion, and keeps the new logic in ONE sourced source of truth
(mirrors `rig_test_dropin_clear_cmds` in `scripts/lib/rig-test-dropin.sh`, #309) instead of
duplicating it inline at every call site. See `scripts/lib/camera-box-restart-verify.sh` for a
worked example — 3 call sites (cam1, the ALL_CAMBOX loop, cam2/painter) each gained a poll+retry
step with the ORIGINAL restart lines byte-for-byte unchanged, verified by the full `cargo test`
suite staying green (115/115 binaries, no anchor collisions).

**Variant (#712) — WRAPPING an anchored line's execution mode (not just appending after it) is
ALSO safe, PROVIDED you check every sibling test uses SUBSTRING `.find()`, never a full-line/exact
match.** #712 needed the cam3/4/5/6 ALL_CAMBOX restore loop's ssh call to run BACKGROUNDED
(`( timeout ... ssh ... ) &` instead of a bare foreground call) so 4 boxes restore concurrently
instead of sequentially — this touches the anchor line itself, not just text after it. Before
doing this: `grep -rn '\.find(' tests/*.rs` for every string that could live inside the region
being touched, and confirm each is a `body.contains(...)`/`region.find(...)` SUBSTRING check
(unaffected by a `(`/`) &` wrapper on the same logical command) rather than something that
requires the anchor to be the literal FIRST token on its line or hard-codes exact whitespace. The
new PID-collection + wait logic itself went into a new sourced lib
(`scripts/lib/cambox-parallel-restore.sh`), same as the #675 pattern — only the wrap-in-parens
touched the anchored region directly, and it was verified safe (grep first, then the full
`cargo test` suite green after) rather than assumed safe.

## GOTCHA — one failing test binary makes `cargo test` SKIP the remaining binaries (a second RED hides)

`cargo test` stops scheduling not-yet-started test binaries after a binary fails ("waiting for
other jobs to finish") — so a run that shows ONE failure is NOT a complete accounting: another
already-RED test file later in the schedule silently never ran. Live incident (2026-07-16, #792
session): the full suite showed only `obs_self_heal_install` failing; after fixing it, a SECOND
pre-existing failure surfaced (`setup_imag_guards`, stale since the #783 same-source pivot the
day before). Both had sat unnoticed because the event-mode hotfix sessions never ran the full
suite. Rules: (1) after fixing a failure, ALWAYS re-run the FULL suite — never conclude "now
green" from the first fix; (2) a hotfix session that skips the full suite leaves landmines for
the next session — run it before ending the session even when CI is deliberately not triggered
(count `test result: ok` lines and expect the full binary count, currently ~156).

## GOTCHA — a NEW `.sh` file's long header comment must not push `set -euo pipefail` past line 15

`pre-write-script-check.sh` (script-failure-policy.md) blocks a brand-new `.sh` file's `Write` if
`set -euo pipefail` isn't found within the file's first ~15 lines — but this repo's convention for
a new acceptance-gate/guard script (`verify-device.sh`, `clock-offset-guard.sh`, `setup-imag.sh`,
...) is a LONG header comment (checks list, env vars, rationale) BEFORE `set -euo pipefail`, often
80+ lines. Those existing files predate the hook (or were edited incrementally past it); a fresh
`Write` of a NEW file in that style gets hard-blocked (#821, `scripts/verify-imag.sh`'s first
draft). Fix: shebang, a ONE-LINE summary comment pointing at "the extended header below", then
`set -euo pipefail` on/before line ~7, then continue the FULL detailed header comment underneath
it (bash comments work anywhere) — satisfies the hook without shrinking the documentation.

## GOTCHA — a `scripts/lib/*.sh` "_cmd" helper embedded via `$(...)` mid-string gets its trailing newline STRIPPED, gluing it to whatever follows

Several sourced libs (`scripts/lib/v4l2-neutral.sh`, and the same pattern is likely reusable
elsewhere) expose functions that print REMOTE bash TEXT for the caller to embed via
`$(...)` inside a larger ssh command string (e.g. `"...$(some_cmd_fn) more literal text..."`).
**Bash's `$(...)` command substitution UNCONDITIONALLY STRIPS ALL trailing newlines from the
captured output** — a completely standard, well-known behaviour (it's why `$(echo foo)` doesn't
leave a stray blank line), but it is easy to forget when the thing being captured is MULTI-LINE
REMOTE SCRIPT TEXT rather than a simple value. If the helper function's LAST printed statement
relies on its own trailing newline to separate it from whatever literal text the caller
concatenates immediately after the `$(...)` (as `[2/8]`/`[2b/8]` in `recording-e2e.sh` do — the
embedding sits in the MIDDLE of a bigger command string, not at its end), that trailing newline is
gone by the time the text is spliced in, and the function's last command silently swallows
whatever follows as EXTRA ARGUMENTS.

**Live incident (#744/#746, 2026-07-13):** `v4l2_neutral_set_default_cmd`'s last statement was
`v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --get-ctrl=saturation,contrast 2>/dev/null` (no trailing `;`).
Embedded as `"...\n   $(v4l2_neutral_set_default_cmd) \\\n   rm -f /tmp/cbox-burn-cam6.log; ..."`,
the stripped newline glued the two together into ONE command line:
`v4l2-ctl ... --get-ctrl=saturation,contrast 2>/dev/null rm -f /tmp/cbox-burn-cam6.log` — v4l2-ctl
errored `unknown arguments: rm`, and the intended `rm` never ran at all. This reproduced live on a
real gate run (29265311504) and was only caught because the log showed the exact "unknown
arguments: rm" text — a purely LOCAL `bash -n` syntax check on the reconstructed command string
does NOT catch this class of bug (gluing valid-looking tokens onto a command's argv is still
syntactically valid bash; it's a semantic error, not a parse error).

**A subtler variant, if what follows is ALSO a bare `VAR=value` assignment (no command name), not
an external command:** bash then treats the WHOLE glued sequence as a "prefix assignment before a
command" if a real command eventually follows on the same unterminated line — which sets the
variable ONLY in that ONE command's temporary environment, NOT persisting in the calling shell, so
a LATER reference to that variable reads as unset/empty. This is easy to miss because it doesn't
error at all; it just silently produces the wrong (empty/default) value downstream.

**Fix, and the rule going forward for any NEW `_cmd`-style helper meant for mid-string embedding:**
end the function's LAST printed statement with an explicit `;` (e.g.
`'v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --get-ctrl=saturation,contrast 2>/dev/null;'` as the final
`printf` argument) — the literal `;` character survives the newline-strip and correctly terminates
the statement regardless of what the caller concatenates immediately after it, whether that's
another bare assignment, a real command, or nothing at all (a harmless trailing `;` at the very
end of a script is valid bash). **Test this class of bug functionally, not just with `bash -n`:**
reproduce the caller's EXACT embedding shape (a fake stand-in binary on `$PATH` logging its argv +
a marker file a "next" command must remove) and assert the following command actually ran as its
own statement — see `tests/harness_v4l2_neutral_744.rs`'s
`set_default_cmd_embedding_never_glues_the_following_command_746` /
`resolve_node_cmd_embedding_never_glues_the_following_command_746` for the pattern.

## GOTCHA — `gh pr merge` falsely refuses a green PR as "not up to date"; the direct REST call works

This repo's `dev` branch is **structurally always "behind" `main`** by design: `main` only ever
gains 2-parent MERGE commits from past `dev`→`main` PRs (`Merge pull request #N from
zbynekdrlik/dev`); `dev` itself is a pure linear branch that NEVER pulls those merge commits back
in (confirmed by walking several consecutive `Merge pull request #N` commits' parents — each
merge's own dev-side parent is dev's OLD tip, never main's). So `git merge-base --is-ancestor
origin/main origin/dev` is **permanently false**, and every PR's `mergeable_state` reads `"behind"`
forever, even on a fully green PR with zero real conflicts (`mergeable: true`).

**Incident (2026-07-11, PR #697):** with every required check green, `gh pr merge 697 --merge`
(and `--auto`) both refused: `"the head branch is not up to date with the base branch"`. This is
`gh`'s own CLIENT-SIDE heuristic being overly cautious for this repo's workflow shape — it is NOT
what GitHub's server-side branch protection actually enforces here. The direct REST call — the
EXACT SAME operation the green "Merge pull request" web button performs, **not** an admin/bypass —
succeeded immediately with zero special flags:

```bash
gh api repos/OWNER/REPO/pulls/<N>/merge -X PUT -f merge_method=merge -f commit_title="Merge pull request #<N> from zbynekdrlik/dev"
```

**Never reach for `--admin`** just because `gh pr merge` complains about "not up to date" — that
IS a branch-protection bypass and is banned regardless (`autonomous-quality-discipline.md`). This
`behind` state is a known-harmless artifact of this repo's specific two-branch shape, not a real
staleness problem; the plain REST merge call is the correct, non-bypassing path when EVERY actual
required check is green and `gh pr merge` is merely being overcautious about it.

## GOTCHA — `gh pr edit --body-file` fails with a GraphQL "Projects (classic)" error; use the REST PATCH instead

`gh pr edit 704 --body-file <file>` (or `--body`) fails on this repo with `GraphQL: Projects
(classic) is being deprecated...(repository.pullRequest.projectCards)` and exit code 1 — `gh`'s
GraphQL mutation for editing a PR fetches the `projectCards` field in its response even when you
never touch project cards, and this repo (or org) still has that legacy field wired up. The body
is **silently NOT updated** when this happens (confirmed: re-reading the PR body afterward showed
the OLD text). The direct REST PATCH sidesteps the broken GraphQL response entirely and works
every time:

```bash
gh api repos/OWNER/REPO/pulls/<N> -X PATCH -F body=@/path/to/new-body.md
```

Same family as the `gh pr merge` GOTCHA above (a `gh` CLI convenience wrapper misbehaving on this
specific repo; the equivalent raw REST call is the reliable fallback) — check the PATCH response's
own `.body` (or re-`gh pr view --json body`) to confirm the write actually landed before trusting
it, since a `gh pr edit` failure here is easy to miss (it prints an error to stderr but the exit
code alone doesn't make the silent no-op obvious without a diff-back).

## GOTCHA — post design/validated/review comments with `gh issue comment`, NEVER `gh api .../comments`

`gh issue view <N> --comments` is GraphQL-broken on this repo (the SAME Projects-classic error as
`gh pr edit`/`gh pr merge` above), which tempts a worker to post issue comments via
`gh api repos/.../issues/<N>/comments -F body=@file` instead. That POST succeeds and the comment
appears on GitHub — **but the airuleset design-before-code recorder (`post-record-design-comment.sh`)
matches ONLY the literal `gh issue comment <N>` command shape**, so a `gh api` comment NEVER registers
a design / validated / reviewed marker. The next `git commit` for that issue is then hard-blocked by
`block-commit-without-design.sh` ("no design comment posted yet") even though the comment is right
there on the ticket. **`gh issue comment <N> --body-file <file>` (the WRITE) works fine on this repo
even though the `--comments` READ is broken** — always use it for the design (root cause + approach +
rejected alt), the STEP-0 validation, and the CYCLE-step-6 review comments so the markers register.
A NON-TRIVIAL design comment additionally needs 2-3 NUMBERED approaches (`Approach 1/2/3` — the
`classify_triage_and_approaches` shape); one chosen + one rejected reads as trivial and is rejected.
(Incident 2026-08-16, #1070/#1075 batch: 4 comments posted via `gh api` never registered — had to
delete them and re-post via `gh issue comment`.)
## GOTCHA — the design/validated/reviewed marker recorder needs `gh issue comment`, and its re-read intermittently times out on this repo

The autopilot design-gate has THREE mandatory durable comments (validated at STEP 0, design before
first code commit, reviewed at CYCLE step 6); a hook writes a `~/.claude/{design,validated,reviewed}
-posted/camera-box#<N>` marker for each, and `block-commit-without-design.sh` blocks the first commit
until the DESIGN marker exists. Three things bite on THIS repo specifically:

- **The recorder (`post-record-design-comment.sh`) fires ONLY on a `gh issue comment` Bash call** — it
  word-matches `gh issue comment`, then re-reads the issue and classifies the FRESHEST comment you
  authored in the last 180s. Posting the comment via `gh api repos/.../issues/<N>/comments -F body=@…`
  (the projectCards-safe path the sibling gotchas above recommend for PR bodies) posts the comment
  fine but writes NO marker → the commit stays blocked. Post design/validation/review comments with
  `gh issue comment <N> --body-file <abs>` so the recorder runs. (`gh issue comment` itself works here;
  only `gh issue view`/`gh pr edit`'s GraphQL hits projectCards.)
- **The recorder's own `gh issue view <N> --json comments` re-read INTERMITTENTLY times out at 10s on
  this repo** (see `~/.claude/design-gate-errors.log` — many `gh-view #N … TimeoutExpired`), and a
  returncode-nonzero read is a SILENT `continue` (no marker, no reject, no log). So a correctly-shaped
  comment can still leave no marker. After each such post, `ls ~/.claude/<kind>-posted/camera-box#<N>`;
  if missing (and not in `…-rejected/`), the read timed out — re-post the SAME comment (delete the
  prior one via `gh api -X DELETE …/issues/comments/<id>` to avoid a dup) when the query is fast.
- **A NON-TRIVIAL design comment must match the exact classifier shape or it lands in
  `design-rejected/`:** a `Triage:` line naming non-trivial, ≥2 NUMBERED approaches spelled
  `Prístup/Approach/Možnosť/Variant 1-3` (NOT `A)/B)`), a trade-off word (`Trade-off`/`výhod`/…), and
  an `Architektúra:` section (colon or `###` heading) containing a structure word
  (`štruktúra`/`topológia`/`structure`), ≥400 chars. VERIFY locally before posting:
  `python3 -c "import sys; sys.path.insert(0,'/home/newlevel/devel/airuleset'); import design_gate as
  dg; b=open('body.md').read(); print(dg.classify_design_comment(b), dg.classify_triage_and_approaches(b),
  dg.classify_architecture_section(b))"` — all three must be `(True, …)`.
- **Post each design/validated/review comment as a STANDALONE `gh issue comment` call — a COMPOUND
  bash call silently registers NO marker (#784, 2026-08-17).** When the `gh issue comment` sits in a
  compound command (`gh api -X DELETE …/comments/<id> ; gh issue comment <N> --body-file … ; sleep 6 ;
  ls`), the recorder's issue-number extraction is confused and writes no marker at all (no
  `*-posted`, no `*-rejected`, no `design-gate-errors.log` entry) — the comment lands on GitHub fine,
  but the next `git commit` for that issue is blocked as "no design comment posted". Running the exact
  same `gh issue comment <N> --body-file <abs>` as its OWN Bash call (nothing before/after it)
  registered the marker every time. Do the delete of a prior comment, the sleep, and the `ls` marker
  check in SEPARATE Bash calls.
- **A `validated` comment needs an ACTION verb, not just "re-validation" (#784).**
  `classify_validation_comment` requires BOTH an action word (`reproduc`/`verified`/`validated`/
  `confirm`/`checked (the) live/current`/`tested live`/`overil`/`preveril`/`potvrd`) AND an evidence
  word (`still valid`/`stále plat`/`already fixed`/`current code`/…). A comment that only says
  "re-validation" / "Re-derived the current state" matches the evidence tier but NOT the action tier →
  `missing: validation action` → no marker. Add an explicit "I verified … and confirmed …".

## GOTCHA — a live-triggered E2E gate run can race ahead of a mid-cycle fleet redeploy

If a PR's fix requires a fleet redeploy to actually take effect on the live rig BEFORE the gate
can pass (e.g. a WARN threshold recalibration — #685), the PR's own automatic `pull_request`-
triggered "Full-path E2E" run can start (and fail, against the STILL-stale rig) before the redeploy
finishes — pushing the fix and deploying it is NOT atomic with the CI trigger. Don't chase that
failed run; once the redeploy is verified live (journal/WS read-back), get a fresh REAL verdict.

**CORRECTED 2026-07-12/13 (#717, #719/#726 dispatch) — this section used to recommend
`gh workflow run "Full-path E2E ..." --ref dev` here. DO NOT DO THAT — it is DANGEROUSLY WRONG
for this purpose.** `full-path-e2e.yml` branches `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` on
`github.event_name == 'pull_request'` — a `workflow_dispatch` run (what `gh workflow run` always
creates) ALWAYS gets `E2E_EXECUTE_VERDICT=0`/`ALL_CAMBOX=0` and stays in the OLD plan-print-only
mode: it never decodes strih/stream, never computes a real verdict, and "succeeds" trivially —
**yet GitHub still posts a check-run with the SAME required-check NAME on the SAME commit SHA**,
which SATISFIES the PR's branch-protection requirement. Following this GOTCHA as originally
written could let a genuinely broken PR merge behind a MEANINGLESS green. The correct way to get
a fresh REAL verdict on an already-pushed commit (no new push, e.g. after a fleet-side fix or an
infra repair with no code diff) is:

```bash
gh run rerun <the-original-pull_request-run-id>
```

`gh run rerun` preserves the ORIGINAL trigger's event context (`github.event_name` stays
`pull_request`), so `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` are correctly `1` again. Find the run id via
`gh run list --branch dev --workflow "Full-path E2E (recording-based · hardware · self-hosted
dev1)" --json databaseId,event,headSha` and pick the one with `"event": "pull_request"` matching
your commit. Full detail + the "two same-commit `pull_request` runs can disagree for reasons
outside your own diff" corollary: `.claude/skills/e2e`'s own `gh workflow run` section (the
canonical source now — this CLAUDE.md section is kept only as a pointer + a loud warning against
the old advice; do not restore the `gh workflow run` snippet here). Same "manual re-trigger after a
stale/superseded run" IDEA the `linux-genlock.yml` GOTCHA above documents for a cancelled run — but
`gh workflow run` is the WRONG mechanism for this specific workflow; `gh run rerun` is correct.

## Local Build Policy

**Tier 0 (default) — CI builds the deployable binary; local checkouts run cheap checks only.**

CI builds the `camera-box` release binary AND the probe/verdict binaries (`--features probe`)
via two artifact uploads (`camera-box-linux-amd64`, `probe-tools-linux-amd64`). Download and run
the CI artifact — never build locally.

Run locally before every push (**DEFAULT FEATURES ONLY — never `--features probe` / `--all-features`**):
```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets -- -D warnings   # NO --all-features
cargo test --no-run
```

**Do NOT compile `--features probe` (or `--all-features`) locally — that is what balloons `target/`.**
The `probe` feature pulls heavy deps (`qrcode`, `rqrr`, `image`, `drm`, `lz4_flex`) and 5 extra
`required-features = ["probe"]` `[[bin]]` targets; with `--all-targets --all-features` every worker's
cheap check recompiled all of them into the single shared dev1 `target/`, which has no GC
(rust-lang/cargo#5026) — so it grew to 18 GB and filled the disk (#185). The probe code is
**compile-checked + built ON CI ONLY**: the C++/vendored gate runs on CI (#101) and the probe
binaries are built + uploaded as `probe-tools-linux-amd64` on CI (#192) — local probe compilation
is redundant. Default-feature checks compile only the small appliance crate (`target/` stays in the
**low hundreds of MB**, not GB); `cargo check`/`cargo tree` on default features pulls NONE of the
probe crates.

Heavy builds in CI only: `cargo build --release`, running `cargo test`, `cargo bench`, `--features probe`.

**Make probe logic Tier-0 testable — pure seam at the CRATE ROOT, not in `src/probe/`.**
The whole `probe` module is `#[cfg(feature = "probe")]` (lib.rs), so its tests run ONLY under
`--features probe` (CI only — banned locally). To get a locally-verifiable RED→GREEN on probe
work, extract the PURE logic (geometry, decisions, tables) into a crate-root module that compiles
on default features — the `src/reannounce.rs` / `src/colour_scale.rs` (#367) pattern — and have
the probe-gated code (`src/probe/…`) iterate/call it. The pure module's tests run on default
features; the probe-gated glue (framebuffer blit, ioctl) gets a thin probe-gated test CI runs.
To OBSERVE RED→GREEN on a cheap default-feature test (the Tier-0 hook blocks all `cargo test`
that RUNS): the `# airuleset:build-ok` bypass is **DISABLED for camera-box (airuleset #477)** — the
marker is now a no-op here and `cargo test`/`--lib`/`--test` that RUNS is hard-blocked regardless.
The working pattern is **compile with `--no-run`, then run the compiled binary DIRECTLY**:
`cargo test --no-run --test <file>` (or `--lib <module>` — both allowed by Tier-0), then execute
`./target/debug/deps/<file>-<hash>` (cargo prints the exact path on its `Executable …` line).
Running an already-built binary is not a `cargo` build/test invocation, so the Tier-0 hook never
sees it. The test harnesses read their shell/script fixtures at RUNTIME, so after editing a sourced
lib you can re-run the SAME compiled binary without recompiling. (Confirmed live #715, 2026-08-17.)

**SUPERSEDED for the COMPILE step (airuleset #557, 2026-08-18, confirmed live #789): even
`cargo test --no-run` is now HARD-BLOCKED** — Tier-0 was tightened to "EVERY compiling cargo shape
(build/test/bench/run/check/clippy/doc/… , scoped or whole-workspace, `--no-run` or not) runs in CI
ONLY". So the `--no-run` half of the pattern above no longer works locally; only running an
ALREADY-built binary from a prior CI/earlier session survives (nothing recompiles it). For a change
that is PURE bash / shell-anchor (e.g. a `scripts/rig-mode.sh` pure function + wiring, no new Rust
type), the full LOCAL net with zero cargo compile is: (1) `bash -n` + `shellcheck -S warning`;
(2) source the script and call the new pure bash function directly over representative fixtures — the
SAME thing a `tests/rig_mode.rs` `run_sourced` test does, so a green bash-level RED→GREEN predicts
the Rust test's pass at CI; (3) a python occurrence-count anchor sweep (OLD `git show HEAD:<script>`
vs NEW, flag any test string-literal whose count went 1→0 or 1→2 — the `camera-active-set.md` net);
(4) `cargo fmt --all --check` (rustfmt parses even probe-gated + new test files → proves the Rust
compiles-shaped OK / is brace-balanced). CI is the FIRST place the Rust test actually type-checks +
runs — expect a TYPE mistake to surface there, not locally.

**No bypass exists for `src/bin/recording-verdict.rs` or any `src/probe/*.rs` file itself** — the
bin has `required-features = ["probe"]` and every file under `src/probe/` is behind the SAME
feature gate, so `cargo check`/`clippy`/`test` on DEFAULT features doesn't even attempt to compile
them (confirmed live, #632/#638: `cargo test --lib probe::qr::` / `qr::tests::` / `grouped_gate`
all silently match "0 tests" — NOT a passing run, just nothing to run). The compile-then-run-the-
binary-directly pattern above only helps a PURE module already extracted to the crate root (the
`# airuleset:build-ok` marker itself is a disabled no-op here, #477); a change confined
entirely to `recording-verdict.rs`/`src/probe/` has **zero local verification path** — not even a
compile check — until CI runs. Treat every such change with extra manual review rigor (type/
signature checks, `cargo fmt --all -- --check`, diffing brace/paren balance against `origin/main`)
before pushing, and expect CI to be the FIRST place a TYPE mistake surfaces. One partial local net
IS worth running deliberately: `cargo fmt --all --check` DOES parse and format-check the
probe-gated files — rustfmt is a purely syntactic tool and ignores `cfg`, so it follows `#[cfg(...)]
mod` paths and formats `#![cfg(feature = "probe")]` test files like any other. A fmt-clean result
therefore PROVES the probe code parses and is brace/format-balanced (it catches a stray brace,
a broken literal, a mis-indented block) even though nothing TYPE-checks it locally. Confirmed live
#1045 (a ~180-line mechanical removal from `src/probe/differ.rs` + a probe-gated `tests/*.rs`): the
whole edit was validated locally by fmt + hand-audit before CI, and fmt would have flagged any
structural slip from the multi-edit removal.

**Gotcha within that extra-review-rigor pass — adding an `f64` field to a probe-gated struct that
derives `Eq` breaks the build (#726).** `f64`/`f32` have no `Eq` impl (NaN has no total order), so
`#[derive(..., Eq, ...)]` on a struct that gains a field containing (or wrapping) a float no longer
compiles. Since this lives under `src/probe/` it is INVISIBLE locally (per above) — the break only
surfaces on CI. Before adding a float-carrying field to any `src/probe/*.rs` struct: `grep -n
"derive(" <file>` for that struct and drop `Eq` if present (keep `PartialEq`/`Debug` — `assert_eq!`
only needs those, never `Eq`), then `grep -rn "StructName" src/ tests/` to confirm nothing outside
the file relies on the dropped `Eq` bound (a HashSet/BTreeSet key, a generic `T: Eq` constraint) —
if something does, that's a real blocker to resolve, not just delete the derive. Example:
`probe::recording_segments::CamboxSegment`/`SegmentedContinuity` dropped `Eq` when
`presentation_cadence: Option<CadenceEvenness>` (which carries `f64` fractions) was added; verified
clean via the grep above before pushing.

**Bound the shared dev1 `target/` (backstop).** Even default-feature checks + rust-analyzer
accumulate over a day (incremental cache, never purged). Keep it under ~4 GB:
```bash
# Check size, then purge when stale / over budget (CI rebuilds it):
du -sh target 2>/dev/null
[ "$(du -sm target 2>/dev/null | cut -f1)" -gt 4096 ] && cargo clean   # >4 GB → reset
```
The repo's `scripts/purge-target.sh` (run by the `pre-push` git hook, installed by
`scripts/install-git-hooks.sh`) does this automatically before each push. **Never purge while an
E2E is live** (probe binaries executing) — the hook skips when `recording-verdict`/`frame-probe`
are running.
