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
// #286 — pure genlock timecode-stamp decision (A/V-cut root fix). Linux-gated because it reuses
// the ndi boundary math; its Tier-0 tests run on the Linux `test` CI job (default features).
#[cfg(target_os = "linux")]
pub mod genlock_stamp;
#[cfg(target_os = "linux")]
pub mod ndi_display;
// #792 — optional secondary 30fps NDI stream (2-frame temporal blend of the emitted 60fps
// pairs). Linux-gated in lock-step with capture/ndi (it carries capture::FrameInfo across a
// channel and owns a second NdiSender); the pairing/blend/config logic is plain std and
// unit-tests Tier-0 on the Linux `test` CI job (default features).
#[cfg(target_os = "linux")]
pub mod publish_30p;
pub mod vban;

// #464 — the pure Auto-fallback PRESENTER decision (`resolve_presenter_kind`), extracted out of
// `probe::presenter::open_presenter`'s hardware I/O. No probe deps, so it unit-tests Tier-0;
// `probe::presenter` re-exports `PresenterKind` from here so every existing
// `probe::presenter::PresenterKind` reference keeps compiling unchanged.
pub mod presenter_kind;

// #297 — NDI sender re-announce trigger (pure decision + network signature). Cross-platform
// (no v4l/libc) so it unit-tests Tier-0; the Linux-only IO (interface read + sender re-create)
// lives in `ndi`.
pub mod reannounce;

// #367 — colour-scale reference layout (pure geometry + colour table). Cross-platform, no
// probe deps, so it unit-tests Tier-0; the probe-gated framebuffer blit lives in `probe::qr`.
pub mod colour_scale;

// #751 — constant-velocity motion sweep (the UFO-test element) for the cam2 painter. Pure
// geometry + position math (no probe deps, unit-tests Tier-0); the probe-gated framebuffer blit
// lives in `probe::qr` and the painter only CALLS it.
pub mod motion_sweep;

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

// #461 — burn-less optical zero-loss gate (pure kernel) for a node with NO digital burn (imag-nb,
// EPIC #466 Topology v2). First..=last integer contiguity over the cam2 painted OPTICAL tick,
// deliberately a sibling of (not reused from) `probe::burn_contiguity` so it stays Tier-0
// testable outside the probe feature. No probe deps; the probe-gated `bin/recording-verdict`
// extracts `RecordingFrame::tick` for imag's recording and feeds it here.
pub mod imag_tick_gate;

// #575 — recording START/STOP boundary artifact trim (pure kernel). A recording's genlock-fifo
// pre-roll flush (start) and mux-finalization tail-drain (stop) can inject non-real-time gaps
// that are not pipeline loss. Trims a small, bounded, named lead/tail frame-position window
// before a signal's ids are fed into a contiguity check — trimming by frame POSITION (not by
// decoded VALUE) means a genuine mid-recording drop can never be masked. No probe deps, so it
// unit-tests Tier-0; the probe-gated `bin/recording-verdict` feeds imag's optical tick + digital
// burn samples here before `imag_tick_gate`'s contiguity checks.
pub mod recording_boundary_trim;

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

// #758 item 4 — the frozen-leg classifier: distinguishes a SUSTAINED camera freeze (hard-fail)
// from isolated stale-replay frames (informational-only) from a segment's own copies/frames/
// duration (the SAME data probe::recording_segments::CamboxSegment already computes). Pure
// Rust, no probe deps, no image/rqrr — unit-tests Tier-0.
pub mod frozen_leg;

// #89 — pure DXGI device-lost (GPU TDR / driver-internal-error) log-signature matcher, extracted
// from `probe::obs_log_audit` (#81) to a crate-root pure module so the default-feature
// watchdog/self-heal pipeline can share the exact same match — never a second drifting copy.
// No probe deps, so it unit-tests Tier-0.
pub mod dxgi_device_lost;

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

// #624 — cross-camera cam2->camera switch-latency SPREAD gate (pure decision): given each
// camera's measured cam2->camera median (p50) latency (the per-camera photon->dequeue latency
// d_X baked in by the #286 root cause), computes the cross-camera spread and gates it against
// the issue's fixed half-a-30fps-frame threshold (16ms). No probe deps, so it unit-tests
// Tier-0; the probe-gated measurement (generalizing `probe::recording_latency::
// cam2_cam1_samples_from_burn`/`_from_flip` from cam1-only to cam1/cam3/cam4, per
// `--switch-schedule` window) lives in `bin/recording-verdict`.
pub mod switch_latency;

// #312 item 2 (PR A) — per-camera A/V-sync WINDOW POOLING (pure decision): given a camera's
// per-`--switch-schedule`-window candidate offset lists (`qpsk_marker::av_offset_candidates`,
// computed per window in the probe-gated `bin/recording-verdict` from that window's decoded
// `(tick, video_ts)` samples), pools them and decides the fail-closed per-camera verdict
// (`AvSyncVerdict::Measured`/`Unknown`) — never a fabricated number from too few samples or a
// scattered non-cluster. Also holds `window_ticks`, the pure `(tick, video_ts)` builder shared
// by every camera's window (mirrors `probe::av_sync_recording::av_sync_from_recording`'s
// identical whole-recording construction, LEFT UNTOUCHED). No probe deps, so it unit-tests
// Tier-0; fuses into `all_cambox_continuity` / `all_cambox_latency`'s SAME `--switch-schedule`
// sweep. PR A reports `all_cambox_av_sync`; it does NOT gate the headline (PR B / #624
// deliverable 4 wires the ±20ms bound on top of this).
pub mod av_window;

// #656 — capture-delivery-rate sanity check (pure decision, prevention item 1). Given the
// periodic captured-fps sample the appliance's own capture loop already computes, decides
// whether the box's capture device has silently drifted off its negotiated rate for enough
// CONSECUTIVE report windows to be a real defect (e.g. a USB capture dongle re-negotiating
// ~64fps instead of a configured 60fps, #656's cam1 ShadowCast 2 root cause) rather than a
// momentary blip. No probe deps, so it unit-tests Tier-0; `src/main.rs`'s capture loop calls it
// every ~5s report window and logs a WARN when it fires.
pub mod capture_rate_health;

// #663 — capture-delivery-rate SELF-HEAL: given a `capture_rate_health::should_warn`-confirmed
// sustained deviation, decides WHEN to automatically USB-reset the defective grabber (rate-limited
// so a dying grabber can't reset-loop forever, escalating to a CRITICAL "replace the hardware" log
// once the fix keeps not holding) and performs the actual sysfs `authorized` toggle. No probe
// deps, so the decision logic unit-tests Tier-0; `src/main.rs`'s capture loop calls it right after
// the #656 WARN fires.
pub mod capture_rate_selfheal;

// #625 — order-independent REAL-DROP ("gap") detection for the all-cambox painted-tick window
// continuity check: the stream recording is documented (`#133`/`#196`/`#216`) to occasionally
// deliver frames "softened"/out of order (a one-frame-late 60->30 straddle); a RECORDED-order
// walk misreads that benign reorder as a backward-jump fault plus an inflated forward jump,
// manufacturing phantom gaps on a genuinely zero-real-drop recording. No probe deps, so it
// unit-tests Tier-0; the probe-gated `probe::recording_segments::window_segment` calls this
// instead of its own inline recorded-order walk.
pub mod painted_tick_gaps;

// #660 — the fbdev "visible page" byte range to BLANK on `probe::kms::KmsPresenter` teardown, so
// releasing DRM master reveals a deterministic black frame instead of whatever ARBITRARILY OLD
// content another writer (the fbdev-fallback presenter, or camera-box's own `--display` module)
// last left in `/dev/fb0`'s memory — the root cause of a stale, unrelated run's QR content
// decoding at the imag optical read's recording tail. No probe deps, so it unit-tests Tier-0; the
// probe-gated `probe::fb::blank_fbdev` (the actual ioctl + write) uses this geometry decision.
pub mod fb_blank;

// #707 — NDI blocking-send STALL diagnostic (pure decision). Given how long a SINGLE blocking
// `NDIlib_send_send_video_v2` call took and the sender's configured frame interval, decides
// whether THIS call stalled — the missing direct evidence the #656/#663/#665/#666/#707
// emit-rate-deficit family has never had (only the downstream 5s-averaged emitted-fps symptom
// was ever measured). No probe deps, so it unit-tests Tier-0; `ndi::NdiSender::
// send_frame_data_with_timecode` times the real call and WARNs via this decision.
pub mod send_stall;

// #707 — V4L2 capture DEQUEUE stall diagnostic (pure decision). Given how long a SINGLE blocking
// `process_frame` dequeue (`self.stream.next()`, a VIDIOC_DQBUF under the hood) took and the
// capture device's own configured frame interval, decides whether THIS dequeue stalled — the
// capture-side half of the observability pair `send_stall` started on the NDI-send side. No probe
// deps, so it unit-tests Tier-0; `main.rs`'s own capture loop times the real dequeue (via
// `capture::FrameInfo::dequeue_duration_ms`) and WARNs via this decision.
pub mod capture_stall;

// #726 — presentation-cadence EVENNESS metric (pure decision). Given the per-frame painted-tick
// sequence in RECORDED order (the same `SegmentFrame.tick` data `probe::recording_segments::
// window_segment` already extracts), classifies whether a recording's 60fps->30fps downsample is
// SMOOTH (uniform `expected_step` cadence) or JUDDERY (paired duplicate+catchup spacing — the
// "15fps-like" live-event symptom the existing loss/continuity gates were blind to). No probe
// deps, so it unit-tests Tier-0; `probe::recording_segments::window_segment` reports it as a new
// field on `CamboxSegment` (REPORTED metric first — not yet gate-enforced pending calibration).
pub mod presentation_cadence;

// #707 EVENT-FORENSICS — per-event residual copy/gap detection (pure decision). Given the same
// per-frame painted-tick data `presentation_cadence`/`painted_tick_gaps` already consume, locates
// SPECIFIC recorded frames as Copy/Gap events (frame index, tick values, wall-clock second, switch-
// schedule offset) so every residual deviation `window_segment` counts gets its own evidence
// bundle, per the user's binding #707 decision ("every residual deviation must have its own
// documented reason"). No probe deps, so it unit-tests Tier-0; `probe::recording_segments::
// window_segment` reports the events as a new field on `CamboxSegment` /
// `SegmentedContinuity`, and `recording-verdict`'s per-box `--extract-partial` flags their
// neighbouring frames for #186 pixel proof.
pub mod residual_events;

// #707 B1 — per-second emit/capture rate ring (pure decision). The 5s `Streaming:` report averages
// fps over the whole window, so a sub-5s EMIT PAUSE (the #707 freeze) can hide in it. This ring
// keeps the last N completed 1-second (emit, capture) buckets so `main.rs` prints a compact
// `emit-1s:` line and WARNs the instant any single second's emit dips below the send floor — the
// box-side prong of #707 B1's freeze discriminator. No probe deps, so it unit-tests Tier-0.
pub mod emit_rate_ring;

// #752 — rate-limit the #707 genlock emit-gate-skip diagnostic. Pure accumulator that coalesces
// the ~10/s per-skip WARN into ONE aggregated line per 5s report window, killing the rsyslogd/
// journald CPU-starvation feedback loop on the 3-core boxes. No probe deps, unit-tests Tier-0.
pub mod emit_skip_log;

// #853 — is a decoded QR payload list evidence of a genuine OPTICAL (non-burn) read, or only the
// easy, always-present digital node burns? No probe deps, so it unit-tests Tier-0;
// `probe::recording::extract_frames_png`'s `sharp_qr_but_flagged_undecodable` self-check calls it
// instead of asking "did ANY QR decode" (guaranteed true on every #853 fleet-wide undecodable
// frame purely from the always-crisp node burns).
pub mod optical_payload_check;

// #855 — parse the shell-side CAMBOX_OFFLINE_ACK / rig-fleet.txt ack format on the Rust side, so
// recording-verdict's all_cambox_av_sync gate can report an acked-offline box EXCLUDED instead of
// judging it UNKNOWN/FAIL on samples it was never going to produce. No probe deps, so it unit
// tests Tier-0.
pub mod offline_ack;

#[cfg(feature = "probe")]
pub mod probe;
