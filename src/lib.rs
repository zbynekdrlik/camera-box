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
// (#1113) genlock capture->emit pacing gate — the pure wall-clock decimation math extracted out of
// the 2555-line `ndi.rs` (issue-1111 review). Linux-gated in lock-step with `ndi` (whose
// NDI-timecode grid it complements); pure `u64` logic otherwise, Tier-0 tests on the Linux `test`
// CI job (default features), mirroring the `genlock_stamp` / `dupe_decimation` sibling precedents.
#[cfg(target_os = "linux")]
pub mod genlock_pacing;
// #286 — pure genlock timecode-stamp decision (A/V-cut root fix). Linux-gated because it reuses
// the ndi boundary math; its Tier-0 tests run on the Linux `test` CI job (default features).
#[cfg(target_os = "linux")]
pub mod genlock_stamp;
// (#889) dupe-preferring decimation for the genlock capture->emit gate — a fast/over-rate
// grabber's internal-buffer repeat gets preferentially shed over the genuine unique tick next to
// it. Linux-gated because it wraps the ndi boundary math (`genlock_pacing::genlock_emit_gate`) and is
// shaped around a raw V4L2 YUYV422 frame; pure logic otherwise, unit-tests Tier-0 on the Linux
// `test` CI job (default features).
#[cfg(target_os = "linux")]
pub mod dupe_decimation;
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

// #1179 — explicit painter display-mode override (the 2560x1080@100 experiment): pure `WxH@RR`
// parsing + proportional dual-QR geometry scaling + canvas resolution. No probe deps, so it
// unit-tests Tier-0; the probe-gated painter/presenter/kms glue threads the resolved values.
pub mod painter_mode;

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

// #984 — the QPSK A/V-sync audio marker's default-enable policy (mirrors the colour_scale/
// motion_sweep default-on-under---paint-only shape) AND the pure ALSA HDMI-device-resolution
// decision (mirrors scripts/lib/marker-device-resolve.sh). No probe deps, so it unit-tests
// Tier-0; the probe-gated glue (`aplay -l` exec, PCM open) lives in `src/bin/frame-probe.rs` /
// `probe::qpsk_emit` / `probe::run`.
pub mod audio_marker_policy;

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

// #1143 — OBS record-session render accounting (report-only). The imag E2E recording must not
// perturb the measurement: recording with SOFTWARE x264 overloaded the render thread → ~18.4%
// lagged → ~19.5% repeated recorded frames (observer effect). The fix moves the record encoder to
// the Intel iGPU HW ffmpeg_vaapi_tex; this crate-root (Tier-0-testable) struct carries OBS's own
// stop-stats lagged% through the imag partial so a stale encoder is surfaced + attributed.
pub mod record_render_stats;

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

// #895 — re-attributes a frozen_leg window to self_heal_reset when a capture_rate_selfheal (#663)
// USB reset correlates with it, so a self-heal reset firing mid-measurement is never again
// misreported as a frozen camera. Extends frozen_leg's classification; pure, no probe deps —
// unit-tests Tier-0.
pub mod self_heal_attribution;

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

// #811 — resolume-snv (CG box) frame-loss-free playback verdict. Given ONE
// resolume NDI input's genlock-FIFO AuditSummary window (from the jitter_audit
// pipeline above), decide PASS/FAIL against the ticket's acceptance bounds
// (skew flat ±20 ms + zero drop/underrun/relock/late-hold/backward-regime
// deltas). Cadence-agnostic (resolume is a non-60 CG source, #787). Pure
// Tier-0 std, self-contained (standalone-rustc testable); the crate consumer
// is `genlock-jitter-report --verdict-source`.
pub mod resolume_playback;

// #771 — MV fps observability: parse the vendored libobs `multiview-audit:` log line (the
// per-projector real render cadence emitted every ~5s by render_display()) + apply the
// target − tol alarm floor (target = canvas/effective_divisor; byte-identical to
// obs_multiview_floor_fps() in obs-display-budget.h, #776).
// Pure Tier-0 (no probe/OBS/rig); the E2E-preflight / drift-guard consumer is the thin
// `src/bin/mv-fps-gate.rs`. The receive-side NDI cadence is separate (jitter_audit above).
pub mod mv_audit;

// #1029 — PROGRAM-render observability: parse the vendored libobs `program-render-audit:` log
// line (the PROGRAM output's own render_fps + renderSkipped/lagged delta emitted every ~5s by
// obs_graphics_thread_loop()) + the `is_render_path_jump` discriminator. Pure Tier-0 (no
// probe/OBS/rig), report-only (the gate for this class is issue 798). Sibling of mv_audit (the
// monitoring-surface render cadence) and jitter_audit (the receive-side NDI cadence).
pub mod program_render_audit;

// #624 — cross-camera cam2->camera switch-latency SPREAD gate (pure decision): given each
// camera's measured cam2->camera median (p50) latency (the per-camera photon->dequeue latency
// d_X baked in by the #286 root cause), computes the cross-camera spread and gates it against
// the SPREAD_THRESHOLD_MS bound (24ms since issue 1120; was #624's 16ms half-frame — recalibrated
// for the CAM1 grabber residual, issue 1110). No probe deps, so it unit-tests
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

// #1128 — fast-capture grabber STUCK detector (ShadowCast ~62.5 fps + persistent corrupted). The
// discriminator the existing capture_rate_selfheal lacks: over-rate AND persistent corrupted, both
// sustained, so a benign over-rate wobble (0 corrupted, absorbed by the decimation gate — #909) is
// never declared stuck. Pure decision + report-line formatting, no probe deps — Tier-0 tested; the
// dev1 alert watchdog greps the `#1128 grabber STUCK` marker this module's message emits.
pub mod grabber_stuck;

// #1193 — cam2 ShadowCast SUSTAINED-OVER-RATE detector: the 3rd self-heal trigger. Keys on the
// COMBINED signature over-rate (cap-1s buckets) AND dupe-victim shed churn, both sustained — the
// churn band is the discriminator (a benign over-rate wobble sheds 0, absorbed by the decimation
// gate) exactly as #1128's corrupted band is. Pure decision + report-line formatting + the cooldown
// predicate, no probe deps — Tier-0 tested; funnels into the shared capture_rate_selfheal USB-reset
// path (gated off by CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL) via a new SelfHealMessages const.
pub mod capture_overrate;

// #625 — order-independent REAL-DROP ("gap") detection for the all-cambox painted-tick window
// continuity check: the stream recording is documented (`#133`/`#196`/`#216`) to occasionally
// deliver frames "softened"/out of order (a one-frame-late 60->30 straddle); a RECORDED-order
// walk misreads that benign reorder as a backward-jump fault plus an inflated forward jump,
// manufacturing phantom gaps on a genuinely zero-real-drop recording. No probe deps, so it
// unit-tests Tier-0; the probe-gated `probe::recording_segments::window_segment` calls this
// instead of its own inline recorded-order walk.
pub mod painted_tick_gaps;

// #859 — PAINTER-PACING attribution: from the cam2 painter's own `tick,gen_ts_ns,flip_ts_ns`
// ground-truth CSV, decide whether a residual captured duplicate is the painter's own stall
// (missed DRM-vsync deadline / repeated tick) or downstream (monitor/camera/splitter optical beat
// or strih/stream genlock FIFO). Crate-root pure seam (Tier-0 testable); surfaced report-only under
// `all_cambox_continuity.painter_pacing` by recording-verdict. Never gates.
pub mod painter_pacing;

// #859 — the genlock FIFO's BACKLOG-STORM threshold, made latency-relative. `obs-source.c`'s bare
// `GENLOCK_QDEPTH_RELOCK 6` was calibrated on "steady depth is ~1-2 at any skew" (its own comment),
// which is false for a source configured DEEP: the stream box's `NDI 2ME PGM` sits at depth 29 on
// the 923 ms latency #856's A/V controller must set, so the backlog branch fires every tick and
// sheds a frame on every jitter excursion. No probe deps, so it unit-tests Tier-0; the probe-gated
// `probe::genlock::ReleaseCadence` and the C `GENLOCK_QDEPTH_RELOCK` both derive from here.
pub mod genlock_backlog;

// #660 — the fbdev "visible page" byte range to BLANK on `probe::kms::KmsPresenter` teardown, so
// releasing DRM master reveals a deterministic black frame instead of whatever ARBITRARILY OLD
// content another writer (the fbdev-fallback presenter, or camera-box's own `--display` module)
// last left in `/dev/fb0`'s memory — the root cause of a stale, unrelated run's QR content
// decoding at the imag optical read's recording tail. No probe deps, so it unit-tests Tier-0; the
// probe-gated `probe::fb::blank_fbdev` (the actual ioctl + write) uses this geometry decision.
pub mod fb_blank;

// #1186 — graceful in-process shutdown for the frame-probe painter: an async-signal-safe
// SIGTERM/SIGINT/SIGHUP handler sets a flag the painter loops poll, so a `systemctl stop
// cam2-painter.service` runs the SAME issue-660 `blank_fbdev` teardown a clean self-exit does
// (SIGTERM's default disposition skips `KmsPresenter::Drop`, leaving a stale frame on /dev/fb0).
// The PURE half (flag + `painter_should_continue`) is std-only and Tier-0-tested; the `install()`
// sigaction glue is cfg(target_os="linux") on the existing `libc` dep, compiled by CI.
pub mod shutdown;

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

// #1088 — duplication-masked 50->60 source-cadence detector (pure decision). Given a sequence of
// per-frame CONTENT hashes in recorded order, counts exact consecutive duplicates and classifies
// whether the pattern is the sustained, regularly-spaced duplication of a 5:6 pulldown (a grabber
// padding a 50fps source up to 60 — the #794 hard layer the receiver-side `received=` rate tap is
// structurally blind to) versus the isolated free-running beat / over-rate baselines. No probe
// deps, so it unit-tests Tier-0; the probe-gated `bin/recording-verdict.rs` computes the per-frame
// codec-tolerant MAD-to-predecessor near-duplicate signal (#1166) from the offline recording and
// reports the result REPORT-ONLY (pending calibration).
pub mod dup_cadence;

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

// #881 (via #854/#707) — the TEMPORARY calibrated floor for the all-cambox segment continuity's
// optical `undecodable` term (a physical 60Hz temporal-tear artifact of the test camera's
// monitor, not chain loss). No probe deps, so it unit-tests Tier-0;
// `probe::recording_segments::window_segment`/`segment_continuity` only CALL it. Deleted
// together with #881 (connect cam2's 120Hz monitor, restore the term to absolute zero).
pub mod burn_hold;
// #1122 — PURE, dependency-free E2E recordings retention decision (keep newest-N runs UNION
// younger-than-D-days; delete ONLY files matching the harness's OWN OBS-timestamp allowlist, never
// a generic *.mkv sweep). The canonical spec that scripts/strih-recordings-retention.ps1 mirrors.
pub mod recordings_retention;
// #789 (residual B / criterion 5) — standalone retention decision for the deploy/backup DIRECTORIES
// the fleet deploy leaves behind (dated `<stamp>-789` box-backups + per-sha stage dirs); keep newest
// -N per kind UNION younger-than-D-days, delete ONLY the deploy's OWN naming allowlist (never a
// generic sweep — `previous/`/operator dirs stay protected). Canonical spec that
// scripts/obs-backup-retention.{ps1,sh} mirror.
pub mod obs_backup_retention;
// #768 — REPORT-ONLY cold-cut onset seam: the first ~1s after a program switch to a cambox hidden
// >= 60s (the transition the segmenter's guard discards, so nothing measured it — the blind spot
// that let issue 767 through). Pure crate-root logic (Tier-0), consumed thinly by recording-verdict.
pub mod cold_cut;
pub mod e2e_latency_gate;
// issue 798 (path A) — REPORT-ONLY seam for the imag-leg recording verdict: makes the imag verdict
// flow into overall_pass as a report-only term first (the imag partial reaches the merge 0/76 runs
// today), one-line-flippable to blocking by a follow-up. Pure crate-root logic (Tier-0), consumed
// thinly by recording-verdict.
pub mod imag_leg_gate;
pub mod optical_floor;
// issue 781 — REPORT-ONLY projection-tap scanout-TEAR detector: pure crate-root classifier (Tier-0)
// over the cam2-optical dual-QR Vernier span per captured frame, consumed thinly by
// recording-verdict's all-cambox sweep. `gates_overall_pass()` is `false` (the payload-level signal
// is proven-blind on the current single-vertical-band content) with a computed `TearSignalViability`;
// one-line-flippable to LIVE once the signal is Observed on a known-torn run + a bound is calibrated.
pub mod tear_detect;
// #1141 — head-end OPTICAL blur/shutter preflight: pure crate-root classifier (Tier-0) over
// the running service's `rough=` capture telemetry, consumed by scripts/lib/optical-preflight.sh.
pub mod optical_preflight;
// issue 1118 — REPORT-ONLY leg schema-degrade seam: a schema-mismatched imag partial DEGRADES
// (drop the leg, verdict from strih+stream) instead of the fatal `load(path)?` killing the
// whole merge. Pure crate-root decision (Tier-0), consumed thinly by recording-verdict::run_merge.
pub mod partial_schema_gate;

// issue 1033 — the ALL-CAMBOX cross-camera DELIVERY-latency spread gate: REPORT-ONLY today (the
// fleet data is not tight-green — cam1's delivery lottery), one-line-flippable to blocking by a
// follow-up. Pure crate-root logic (Tier-0), consumed thinly by recording-verdict; reuses the
// switch_latency SPREAD_THRESHOLD_MS bound (24 ms since issue 1120; no new constant).
pub mod delivery_spread_gate;

// #889 (user decision on #883, 2026-07-30) — the per-cambox-window `copies`/`gaps` terms become
// REPORT-ONLY (still computed, still printed, no longer fail the window/run). No probe deps, so
// it unit-tests Tier-0; `probe::recording_segments::window_segment` calls this and wires the
// result onto `CamboxSegment`. TEMPORARY, restore-gated on #883 item 4 + two clean strict runs.
pub mod window_gate;

// #930 (2026-08-01) — lipsync cross-validation gate: does SyncNet's neural offset (a real
// talking-face clip, `scripts/av_sync_measure.py`'s #917 engine) agree with the QR/QPSK
// `--av-sync` offset from a paired TEST-mode run? No probe deps, so it unit-tests Tier-0;
// `src/bin/recording-verdict.rs`'s `run_av_sync` (`--av-sync` mode) is the sole caller.
pub mod lipsync_cross_check;

// #893 (2026-07-30) — the "at least one ACTIVE camera sits at the phase-sync floor" gate term.
// No probe deps, so it unit-tests Tier-0; the `phase-sync-active-floor-gate` CLI binary
// (src/bin/) reads live pins over OBS WS and hands them in.
pub mod phase_sync_active_floor;

// #903 — the boundary TOLERANCE for confirming a backward burn-id jump crosses a
// `--switch-schedule` program-switch boundary, when the exact-instant `raw_window_index` equality
// test alone cannot see it (clock disagreement between dev1 and the painter, bounded only by the
// #326 gate's 200ms guarantee). No probe deps, so it unit-tests Tier-0;
// `probe::burn_contiguity::burn_contiguity_in_window_with_step_and_schedule` calls it.
pub mod window_boundary_tolerance;

// #804 (epic #800 A/V-desync endgame round) — ASRC bench harness: two independent free-running
// clock domains simulated deterministically, with the TDD RED/GREEN gate #803's real ASRC (a
// per-source rate estimator + libswresample soft compensation in the vendored libobs) must
// satisfy. No probe deps, so it unit-tests Tier-0 on a plain CI/bench machine — no rig required.
pub mod asrc_bench;

// #806 (epic #800 A/V-desync endgame round) — the outer-loop guard: a slow, SyncNet-driven
// feedback loop correcting #803's inner ASRC servo's own long-term residual. Pure/Tier-0, mirrors
// the asrc_bench.rs pattern; see the module's own doc comment for the root cause + design.
pub mod asrc_outer_loop;

// #929 — pure-Rust mirror of audio_resampler_set_compensation_ppm()'s ppm->sample_delta integer
// rounding, pinning the compensation-quantization threshold discovered while measuring #929's
// ASRC resampling-quality A/B (scripts/asrc-quality-bench/). Pure/Tier-0, no probe deps; see the
// module's own doc comment for what it documents and why (also feeds the #1016 follow-up).
pub mod asrc_compensation_quantization;

// #945 — capture/emit-thread WEDGE self-watchdog: the pure decision for whether the capture
// loop's blocking V4L2 dequeue has gone stale long enough to treat the loop as provably dead.
// No probe deps, so it unit-tests Tier-0; `src/main.rs`'s capture loop + a dedicated watchdog
// thread call into it.
pub mod capture_wedge;

// #944 — emit/output-liveness self-watchdog: the pure decision + message for whether the NDI
// output has gone frozen (the capture thread's blocking dequeue is still returning, but no good
// frame has been emitted for the threshold). The emit-side sibling of `capture_wedge` (#945); no
// probe deps, so it unit-tests Tier-0. `src/main.rs`'s capture loop stamps the emit heartbeat and
// the #945 watchdog thread polls this decision.
pub mod emit_freeze;

// #936 — painter WEDGE self-watchdog: the pure decision + message for whether the KMS/DRM painter
// loop's blocking present() call has gone stale long enough to treat it as provably wedged (a
// SIGTERM/SIGKILL-immune kernel-level D-state hang, the same failure class #945 root-caused on
// the capture side three days earlier). No probe deps (reuses capture_wedge's generic threshold
// math), so it unit-tests Tier-0; `src/probe/run.rs`'s `run_paint_only`/`run` + a dedicated
// watchdog thread call into it, and `src/probe/painter.rs`'s `run_painter` writes the heartbeat.
pub mod painter_wedge;

#[cfg(feature = "probe")]
pub mod probe;

// #828 — no-capture-device startup handling: the PURE slow-retry loop decision (probe + backoff
// sleep) for a box whose USB grabber is absent, so it settles into a quiet, clearly-logged retry
// instead of a ~3 s restart storm and auto-recovers on (re-)plug. No probe deps, so it unit-tests
// Tier-0; `src/main.rs`'s auto-detect branch injects the real `config::find_capture_device_opt`
// probe + `std::thread::sleep`.
pub mod no_device;
