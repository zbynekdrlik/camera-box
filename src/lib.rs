//! camera-box library
//!
//! This module exports the public APIs for testing and benchmarking.

// #193: the camera APPLIANCE modules are Linux-only — they bind v4l (capture/config/ndi),
// the /dev/fb0 framebuffer + libc ioctl (display/ndi_display), and ALSA/cpal/evdev
// (intercom). The cameras are x86_64 Ubuntu, so these only ever build on Linux. Gating
// them on cfg(target_os="linux") lets the PROBE tooling (recording-verdict, src/bin) build
// for Windows, so the #193 recording decode runs ON stream.lan where the video lives — never
// downloaded to dev1. grab_record calls crate::capture::yuyv_to_gray8, so it is gated in
// lock-step. vban (pure UDP) and the probe tree (which the verdict needs) stay cross-platform.
#[cfg(target_os = "linux")]
pub mod affinity;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
pub mod config;
#[cfg(target_os = "linux")]
pub mod display;
#[cfg(target_os = "linux")]
pub mod grab_record;
#[cfg(target_os = "linux")]
pub mod intercom;
#[cfg(target_os = "linux")]
pub mod ndi;
#[cfg(target_os = "linux")]
pub mod ndi_display;
pub mod vban;

// #297 — NDI sender re-announce trigger (pure decision + network signature). Cross-platform
// (no v4l/libc) so it unit-tests Tier-0; the Linux-only IO (interface read + sender re-create)
// lives in `ndi`.
pub mod reannounce;

// #367 — colour-scale reference layout (pure geometry + colour table). Cross-platform, no
// probe deps, so it unit-tests Tier-0; the probe-gated framebuffer blit lives in `probe::qr`.
pub mod colour_scale;

// #188/#145 — QR-based (QPSK) audio marker, byte-compatible with the norihiro
// obs-audio-video-sync-dock protocol. Pure Tier-0 (encode + decode + estimator); the continuous-feed
// ALSA emitter (`probe::qpsk_emit`) and recording-verdict decode call into this. Supersedes the chirp.
pub mod qpsk_marker;

// #398 — the LIVE OBS A/V-sync dock decode logic, pure Tier-0 so the vendored C++ dock
// (`vendor/av-sync-dock/src/camera-box-*.hpp`) can MIRROR it and a committed C++ self-test can
// cross-check the mirror against these Rust results. Holds the streaming QPSK marker detector
// (rolling `decode_markers` window + dedup), the rolling densest-cluster offset estimator (robust to
// the CRC-4 false-decode flood the offline path also fights), the live video-QR top-band decode
// geometry, and the Otsu threshold — all with NO probe deps, so it compiles + unit-tests on DEFAULT
// features. The dock's OBS/quirc GLUE stays in `sync-test-output.cpp`; every DECISION lives here.
pub mod av_sync_dock;

// #364 — per-camera COLOUR-correctness gate (pure decision + sampler). Iterates the SAME
// `colour_scale` table/geometry, samples each reference patch's mean colour from a frame
// (dodging the burn columns), and decides per-patch + per-camera PASS/FAIL (grayscale collapse,
// hue-shift, out-of-tolerance). No probe deps, so it unit-tests Tier-0; the probe-gated pixel
// sampling + ffmpeg colour pass live in `probe::colour_sample`, and the verdict gate wiring is in
// `bin/recording-verdict`.
pub mod colour_verify;

// #405 / EPIC #406 — pure OBS render-budget verdict. The strict gate signal for whether the
// program RENDER loop (activeFps + averageFrameRenderTime + renderSkipped) holds its frame
// deadline — the REAL render-health signal, not the encoder outputFps that duplicates to target
// and stays green while render chokes (the 2026-07-02 60→27fps burn regression). No probe deps,
// so it unit-tests Tier-0; the rig E2E (recording-e2e.sh live OBS WS GetStats) calls it to gate.
pub mod render_budget;

// #373 — the zero-loss HEADLINE analyzed-span duration gate (pure decision). A collapsed/partial
// cam2 optical read must not vacuously pass the headline over a handful of frames. No probe deps,
// so it unit-tests Tier-0; the probe-gated `bin/recording-verdict` feeds each node's optical-span
// frame count here to gate the headline alongside contiguity + the optical + colour gates.
pub mod recording_span_gate;

// #356 — cross-recording cam1 loss reconciliation (pure kernel). In the recording-verdict MERGE,
// a cam1 REAL DROP read from the clean upstream strih recording that IS decoded in the downstream
// stream recording was proven delivered → re-classify it BURN-UNREADABLE (a strih-recording
// readability gap at the high-latency 60→30 hop), never a chain loss. No probe deps, so it
// unit-tests Tier-0; the probe-gated `bin/recording-verdict` computes the downstream cam1 id set
// and applies the returned downgrade to the cam1 node's classification.
pub mod burn_reconcile;

// #365 — frozen-camera freshness gate (pure decision + hash-timeline analysis). Hashes each
// camera's raw NDI input from OBS GetSourceScreenshot at ~1 s cadence; a camera whose hash is
// unchanged for > FREEZE_THRESHOLD consecutive samples is FROZEN. Fail-closed: < 2 successful
// samples → FROZEN. Pure Rust, no probe deps, so it unit-tests Tier-0; the OBS I/O lives in
// `scripts/frozen-camera-gate.py`; the thin CLI binary lives in `src/bin/frozen-camera-gate.rs`.
pub mod frozen_camera;

// #391 — broadcast-OBS liveness/wedge verdict (pure decision). Stream OBS was hung
// "(Not Responding)" ~25h (obs64 pegged ~168% CPU, 16.0% render-lag) with nothing
// detecting it. This is the strict Tier-0 kernel: GetStats (always available from a
// dev1 timer over OBS WS) + optional agent/MCP-only process signals (obs64 count /
// Responding / CPU%) -> HEALTHY / WEDGED-RENDER-LAG / WS-DEAD / FPS-ZERO /
// OBS-COUNT-WRONG. No probe deps, so it unit-tests Tier-0; the OBS I/O lives in
// `scripts/obs-liveness-probe.py`; the thin CLI binary lives in
// `src/bin/obs-watchdog-gate.rs`.
pub mod obs_watchdog;

// #411 — Windows-local unattended self-heal for the #391 liveness watchdog (pure decision).
// #391 shipped DETECT + ALERT only — recovery still needed an agent to see a Discord alert
// and run launch-obs-genlock.sh via the win-* MCP, which fails the exact overnight/unattended
// case the watchdog exists to cover. This is the RECOVERY-decision + AHK-sequencing kernel: a
// confirm-threshold, a min-interval throttle, a single-recovery-in-flight lock, the ordered
// step plan that makes an AHK double-launch impossible by construction, and the post-recovery
// verify rule. Reuses `obs_watchdog::classify` unchanged as the wedge verdict — never
// reimplements it. No probe deps, so it unit-tests Tier-0; the Windows-side mechanism (recovery
// PowerShell + Task Scheduler XML) is emitted by `scripts/obs-self-heal-install.sh`.
pub mod obs_self_heal;

// #137 — OBS-restart A/V-sync SURVIVAL verdict (pure decision). An OBS stop→start
// sometimes drifts the video↔audio offset by ~200-300ms and destroys lipsync, with
// nothing automatic to catch it. This is the strict Tier-0 kernel: a BEFORE and AFTER
// `recording-verdict --av-sync` measurement (#188) -> PASS / FAIL / UNKNOWN, fail-closed
// on measurement quality so an untrustworthy decode can never manufacture a false PASS.
// No probe deps, so it unit-tests Tier-0; the rig I/O (two bracketing recordings around
// a real OBS restart) lives in `scripts/recording-e2e.sh`'s optional `AV_RESTART_GATE`
// step; the thin CLI binary lives in `src/bin/av-restart-sync-gate.rs`.
pub mod av_restart_sync;

// #286 — 4-camera MUTUAL phase-sync offset kernel (pure decision). Given each camera's
// measured cam→strih latency, computes the per-source genlock-latency offset that makes all
// cameras release the SAME captured instant at the SAME wall-clock time: the slowest camera at
// the floor, every faster camera held back by its deficit. No probe deps, so it unit-tests
// Tier-0; the probe-gated measurement lives in `probe::recording_latency`'s
// `n_camera_strih_samples` / `n_camera_median_latency_ms`, and the OBS-WS apply + persist
// controller is `scripts/phase_sync_calibrate.py` (mirrors this module's math in Python).
pub mod phase_sync;

// #109 — restart-survival ZERO-LOSS verdict (pure decision), the exact sibling of
// `av_restart_sync` for the #186 delivery signal instead of the #188 A/V-sync signal. A BEFORE
// and AFTER `recording-verdict --json` report -> PASS / FAIL / UNKNOWN, fail-closed on any
// internally-inconsistent report so a corrupt/mismatched JSON can never manufacture a false
// PASS. No probe deps, so it unit-tests Tier-0; the rig I/O (two bracketing recordings around a
// real OBS restart AND a real PC reboot of strih+stream) lives in `scripts/recording-e2e.sh`'s
// optional `ZERO_LOSS_RESTART_GATE` step; the thin CLI binary lives in
// `src/bin/zero-loss-restart-gate.rs`.
pub mod zero_loss_restart_survival;

// #272 — genlock arrival-jitter audit-log parser + per-run reserve→loss summarizer. Turns
// the #148 periodic `genlock-fifo audit` log line into, per source, the DELTA loss/
// backpressure counters over a captured window plus the head-skew jitter distribution —
// the "did lowering the reserve introduce loss, how big is the real arrival jitter"
// answer the #272 investigation needs. No probe deps, so it unit-tests Tier-0; the thin
// CLI binary lives in `src/bin/genlock-jitter-report.rs`. See
// `docs/genlock-latency-floor-rationale.md`.
pub mod jitter_audit;

#[cfg(feature = "probe")]
pub mod probe;
