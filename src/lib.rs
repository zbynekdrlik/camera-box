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

#[cfg(feature = "probe")]
pub mod probe;
