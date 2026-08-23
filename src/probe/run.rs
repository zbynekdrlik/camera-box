//! Orchestrate painter + reader for a fixed duration, then analyze.

use crate::probe::analyzer::{analyze, AnalysisInput, AnalysisReport, Observed, PaintMode};
use crate::probe::painter::{run_painter, PaintParams};
use crate::probe::presenter::PresenterKind;
use crate::probe::reader::{run_reader, ReadParams};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RunConfig {
    pub mode: PaintMode,
    pub run_id: u32,
    pub source: String,
    pub fb_device: String,
    /// DRM card device for the KMS page-flip presenter (e.g. `/dev/dri/card1`).
    pub drm_device: String,
    /// Presenter selection: `Auto` (KMS with fbdev fallback), `Kms`, or `Fbdev`.
    pub presenter: PresenterKind,
    pub duration: Duration,
    pub paint_fps: f64,
    pub capture_fps: f64,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub qr_size: u32,
    /// #1179: the mode-SELECTION refresh (milli-Hz) for the KMS presenter's `pick_mode` — 60_000
    /// (the capture rate) by default = today's mode; overridable via `--display-mode` (e.g.
    /// 100_000 for the 2560×1080@100 experiment). SELECTION only; the 1:1 phase-lock check stays
    /// against the fixed 60 fps capture.
    pub mode_refresh_mhz: u32,
    pub freeze_periods: f64,
    pub connect_timeout_secs: u32,
    /// Frames painted within this window of the run end are excluded from the
    /// loss check: they may not have traversed the pipeline (latency) and been
    /// decoded before teardown. Must exceed the observed max end-to-end latency.
    pub settle_ms: u64,
    /// Hard gate: fail the verdict if p99 latency exceeds this (`None` ⇒ off).
    pub max_p99_latency_ms: Option<f64>,
    /// Hard gate: fail the verdict if a freeze run exceeds this (`None` ⇒ off).
    pub max_freeze_periods_gate: Option<f64>,
    /// Stamp `gen_ts_ns` on CLOCK_REALTIME (wall clock) instead of the monotonic
    /// `Instant`. Set ONLY for the #7 multi-node absolute-latency path (paint-only
    /// on the camera, taps on dev1, both DanteSync-disciplined). For the Phase-1
    /// single-box loopback `run()` this MUST stay false — painter and reader share
    /// one process clock there and a wall-clock gen would break that latency.
    pub wall_clock: bool,
    /// Paint two QR codes side-by-side (Vernier dual-QR path) and decode from both
    /// halves on receive. At least one half is always sharp on a mid-transition
    /// capture, eliminating the false-loss artifact from the single-QR path.
    pub dual_qr: bool,
    /// #367: paint the fixed colour-reference scale along the bottom band, alongside the
    /// dual-QR (for eye + recording colour verification, the #364 gate). Forwarded into
    /// the painter's `PaintParams`.
    pub colour_scale: bool,
    /// #751: paint the constant-velocity motion sweep (UFO-test judder indicator) in the bottom
    /// band. Forwarded into the painter's `PaintParams`.
    pub motion_sweep: bool,
    /// Optional path for `run_paint_only` to write the painter's emitted-tick
    /// CSV (`tick,gen_ts_ns`) — the cam→strih ground truth consumed by
    /// `recording-verdict --painter` (#105). `None` ⇒ no log written.
    pub paint_log: Option<String>,
    /// #188: enable QPSK A/V-sync marker emission on the ALSA device at `audio_marker_cadence_ticks`.
    pub audio_marker: bool,
    /// ALSA device string for the QPSK marker (cam2 HDMI out, e.g. `hw:CARD=PCH,DEV=3`).
    pub audio_marker_device: String,
    /// Emit the QPSK marker every N painter refresh ticks (~5 s @ 60 Hz with the default 300).
    pub audio_marker_cadence_ticks: u64,
    /// #984: true iff `audio_marker` was enabled by the DEFAULT policy (no explicit
    /// `--audio-marker` on the CLI) rather than an explicit caller request. A SOFT marker that
    /// fails to open its PCM (or dies mid-run) degrades: `run_paint_only` logs an ERROR
    /// periodically and keeps painting QR-only, instead of aborting the whole run (the permanent
    /// cam2-painter.service must never crash-loop or go dark just because the audio pin is
    /// temporarily unavailable). An EXPLICIT (`audio_marker_soft = false`) request keeps the
    /// original hard-fail contract (issue 936 measurement-gate semantics, unchanged) — a
    /// dead marker there must still fail the whole run loudly.
    pub audio_marker_soft: bool,
    /// Optional path for `run_paint_only` to write the A/V-sync marker log CSV
    /// (`index,frame_id,emit_ts_ns`). `None` ⇒ no log written.
    pub marker_log: Option<PathBuf>,
}

/// The painter's default frame rate (frames/sec) when the user did not pass an
/// explicit `--paint-fps`, given the paint `mode`, the `capture_fps`, the chosen
/// `presenter`, and whether this is a `paint_only` (rig) or `synth_ndi` run.
///
/// A path that drives a real HDMI presenter (and so must match the capture cadence
/// to resolve every captured frame) defaults to the FULL `capture_fps`; the
/// single-box fbdev loopback GATE keeps the sub-capture coverage default (12 fps —
/// its in-process `run()` reader wants ≥2 clean samples per id, no tearing
/// false-loss); the presenter-less `--synth-ndi` golden reference keeps it too.
pub fn default_paint_fps(
    mode: PaintMode,
    capture_fps: f64,
    presenter: PresenterKind,
    paint_only: bool,
    synth_ndi: bool,
) -> f64 {
    // A path that drives a real HDMI presenter must paint at the full capture rate so
    // every captured frame resolves a DISTINCT tick (#290):
    //   - the single-box loopback `run()` on the KMS/auto presenter is vblank-locked
    //     at the capture rate (the configured value matches that cadence; #79);
    //   - the rig `--paint-only` painter ALSO opens a presenter (`run_paint_only` →
    //     `run_painter` → `open_presenter`): under KMS it is vblank-locked, under the
    //     fbdev fallback it sleep-paces at this configured rate — so the configured
    //     rate MUST be the capture rate or the fbdev-fallback painter ticks too
    //     slowly (the #290 30fps-painter-vs-60fps-capture bug). The original logic
    //     wrongly excluded `paint_only`, treating it like the presenter-less synth
    //     path.
    // Only the single-box fbdev loopback GATE keeps the sub-capture coverage default
    // (its in-process `run()` reader wants ≥2 clean samples per id, no tearing
    // false-loss), and the presenter-less `--synth-ndi` golden reference keeps it too.
    let full_rate_presenter_path =
        (!matches!(presenter, PresenterKind::Fbdev) || paint_only) && !synth_ndi;
    match mode {
        PaintMode::Coverage if full_rate_presenter_path => capture_fps,
        PaintMode::Coverage => 12.0,
        PaintMode::FullRate => capture_fps,
    }
}

/// #936: spawn the painter-wedge watchdog thread (see `crate::painter_wedge`'s module doc) —
/// polls `heartbeat` (stamped by `run_painter` after every successful frame) and forces the
/// process to exit loudly the moment it goes stale, because a genuine DRM/KMS kernel hang can
/// park the painter thread in an uninterruptible wait that no signal (not even SIGKILL) can
/// preempt — mirrors `src/main.rs`'s #945 capture-wedge watchdog exactly. The returned thread is
/// deliberately never joined by the caller: its own loop breaks cleanly once `stop` is set
/// (normal shutdown), so it is safe to let it run detached until then.
fn spawn_painter_wedge_watchdog(heartbeat: Arc<AtomicU64>, stop: Arc<AtomicBool>, start: Instant) {
    std::thread::Builder::new()
        .name("painter-wedge-watchdog".into())
        .spawn(move || {
            // Poll well inside the wedge threshold so the watchdog itself can never add more
            // than one poll interval of detection latency. Checks `stop` BEFORE and AFTER the
            // sleep (mirrors src/main.rs's #945 capture-wedge watchdog exactly) so a `stop`
            // already true at spawn time is caught without waiting a full poll interval first.
            let poll_interval = Duration::from_millis(500);
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(poll_interval);
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break; // normal shutdown in progress -- never misreport as a wedge
                }
                let now_ns = start.elapsed().as_nanos() as u64;
                let last_progress_ns = heartbeat.load(std::sync::atomic::Ordering::Relaxed);
                let seconds_since_last_progress =
                    now_ns.saturating_sub(last_progress_ns) as f64 / 1_000_000_000.0;
                if crate::capture_wedge::evaluate_wedge(
                    seconds_since_last_progress,
                    crate::painter_wedge::PAINTER_WEDGE_THRESHOLD_S,
                ) == crate::capture_wedge::WedgeVerdict::Wedged
                {
                    tracing::error!(
                        "{}",
                        crate::painter_wedge::painter_wedge_message(
                            seconds_since_last_progress,
                            crate::painter_wedge::PAINTER_WEDGE_THRESHOLD_S,
                        )
                    );
                    // The painter thread is provably dead (its own blocking DRM call never
                    // returned) -- a graceful in-process shutdown of ITS state is not reachable
                    // from here, so exit immediately. A supervisor (systemd Restart=always on
                    // cam2-painter.service, or the rig operator's own tooling) recovers it.
                    std::process::exit(crate::painter_wedge::PAINTER_WEDGE_EXIT_CODE);
                }
            }
        })
        .expect("failed to spawn #936 painter-wedge watchdog thread");
}

pub fn run(cfg: RunConfig) -> Result<AnalysisReport> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    // #936: painter-wedge watchdog heartbeat -- always constructed (not gated on any flag) so a
    // DRM/KMS-level hang self-detects regardless of mode.
    let painter_heartbeat_ns = Arc::new(AtomicU64::new(0));
    spawn_painter_wedge_watchdog(painter_heartbeat_ns.clone(), stop.clone(), start);

    let reader_handle = {
        let stop = stop.clone();
        let observed = observed.clone();
        let params = ReadParams {
            run_id: cfg.run_id,
            source: cfg.source.clone(),
            connect_timeout_secs: cfg.connect_timeout_secs,
            // Decode only the centered ROI where the QR is painted (+margin for
            // quiet zone and capture jitter), so decode keeps up in real time.
            decode_crop: (cfg.qr_size + 120).min(cfg.canvas_h),
            dual_qr: cfg.dual_qr,
        };
        std::thread::spawn(move || run_reader(params, start, stop, observed))
    };

    let painter_handle = {
        let stop = stop.clone();
        let emitted = emitted.clone();
        let heartbeat = painter_heartbeat_ns.clone();
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            drm_device: cfg.drm_device.clone(),
            presenter: cfg.presenter,
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
            mode_refresh_mhz: cfg.mode_refresh_mhz,
            // Phase-1 single-box loopback: painter + reader share THIS process's
            // monotonic clock, so latency is exact without any sync. A wall-clock
            // gen here would break that — force monotonic regardless of cfg.
            wall_clock: false,
            dual_qr: cfg.dual_qr,
            colour_scale: cfg.colour_scale,
            motion_sweep: cfg.motion_sweep,
        };
        std::thread::spawn(move || run_painter(params, start, stop, emitted, None, None, heartbeat))
    };

    // Run for the duration, but stop early if either thread dies (e.g. the
    // framebuffer fails to open) so a failure surfaces in seconds, not minutes.
    let deadline = Instant::now() + cfg.duration;
    while Instant::now() < deadline {
        if painter_handle.is_finished() || reader_handle.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let stop_ns = start.elapsed().as_nanos() as i64;

    painter_handle.join().expect("painter panicked")?;
    reader_handle.join().expect("reader panicked")?;

    // Exclude the trailing settle window: frames painted that close to the end
    // may legitimately still be in flight (pipeline latency) when the reader
    // stops, so they must not count as losses.
    let settle_ns = (cfg.settle_ms as i64) * 1_000_000;
    let cutoff_ns = stop_ns - settle_ns;
    let emitted_ids: Vec<u32> = emitted
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, gen_ts, _flip_ts)| *gen_ts <= cutoff_ns)
        .map(|(id, _, _)| *id)
        .collect();
    let observed_vec = observed.lock().unwrap().clone();

    Ok(analyze(AnalysisInput {
        mode: cfg.mode,
        emitted_ids,
        observed: observed_vec,
        capture_fps: cfg.capture_fps,
        freeze_periods: cfg.freeze_periods,
        max_p99_latency_ms: cfg.max_p99_latency_ms,
        max_freeze_periods_gate: cfg.max_freeze_periods_gate,
    }))
}

/// Serialize the painter's emitted `(logical_tick, gen_ts_ns, flip_ts_ns)` sequence
/// into the `recording-verdict --painter` CSV (`tick,gen_ts_ns,flip_ts_ns`, one row per
/// painted frame, header `tick,gen_ts_ns,flip_ts_ns`). This is the cam→strih GROUND
/// TRUTH (#105) AND the cam2→cam1 flip-time reference (#194):
///
/// - `gen_ts_ns` — the frame-GENERATION instant (baked into the QR; necessarily a
///   pre-flip stamp). Used by the existing tick-column parser (column 0 = `tick`) so the
///   cam→strih assessment is unchanged.
/// - `flip_ts_ns` — the page-flip-COMPLETE instant (captured after `present()` returns =
///   the frame on screen). recording-verdict maps `tick → flip_ts_ns` from this column so
///   the cam2→cam1 optical latency is `cam1_capture − flip_ts` (real display→capture), NOT
///   the inflated `cam1_capture − gen_ts` (#194).
///
/// The header still starts with `tick,`, so the existing tick-column reader (which keys
/// on that prefix and takes column 0) keeps working verbatim — the flip column is purely
/// additive.
///
/// PURE (no I/O): the caller writes the returned string to the chosen path so the
/// formatting is unit-testable without spawning a painter or a presenter.
pub fn serialize_painter_log(emitted: &[(u32, i64, i64)]) -> String {
    let mut s = String::from("tick,gen_ts_ns,flip_ts_ns\n");
    for (tick, gen_ts_ns, flip_ts_ns) in emitted {
        s.push_str(&format!("{tick},{gen_ts_ns},{flip_ts_ns}\n"));
    }
    s
}

/// Paint QR frames for `duration` without receiving/analyzing — used on the
/// camera box in Phase 2, where the QR reaches NDI via camera-box's own
/// capture→NDI path and the taps run elsewhere (dev1).
pub fn run_paint_only(cfg: &RunConfig) -> Result<u64> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));

    // #188: shared atomics published by the painter each iteration so the audio-marker
    // thread can read the current frame id and refresh tick without touching the
    // vblank-locked paint path.
    let current_id = Arc::new(AtomicU32::new(0));
    let refresh_out = Arc::new(AtomicU64::new(0));
    // #936: painter-wedge watchdog heartbeat -- always constructed (not gated on
    // cfg.audio_marker) so a DRM/KMS-level hang self-detects on every --paint-only run.
    let painter_heartbeat_ns = Arc::new(AtomicU64::new(0));
    spawn_painter_wedge_watchdog(painter_heartbeat_ns.clone(), stop.clone(), start);

    let painter_handle = {
        let stop = stop.clone();
        let emitted = emitted.clone();
        let heartbeat = painter_heartbeat_ns.clone();
        let current_id_p = if cfg.audio_marker {
            Some(current_id.clone())
        } else {
            None
        };
        let refresh_out_p = if cfg.audio_marker {
            Some(refresh_out.clone())
        } else {
            None
        };
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            drm_device: cfg.drm_device.clone(),
            presenter: cfg.presenter,
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
            mode_refresh_mhz: cfg.mode_refresh_mhz,
            // Multi-node (#7): stamp gen_ts on the DanteSync wall clock when asked
            // so the dev1 endpoint tap's wall-clock recv − this gen is true
            // absolute latency. Defaults false (Phase-2 relative latency only).
            wall_clock: cfg.wall_clock,
            dual_qr: cfg.dual_qr,
            colour_scale: cfg.colour_scale,
            motion_sweep: cfg.motion_sweep,
        };
        std::thread::spawn(move || {
            run_painter(
                params,
                start,
                stop,
                emitted,
                current_id_p,
                refresh_out_p,
                heartbeat,
            )
        })
    };

    // #188: spawn the QPSK A/V-sync marker thread when enabled (norihiro-compatible QR-based audio,
    // continuous-feed — supersedes the chirp). Rig 60fps params (48 kHz / 442 Hz / c=1).
    // #431: pass cfg.marker_log through so the emitter appends each fired marker's row to it
    // INCREMENTALLY (not just on shutdown, below) — the hardened `#420` audible self-check
    // (scripts/lib/audio-marker-check.sh) polls that growth to prove real emission, not just an
    // ALSA PCM held RUNNING by the continuous-feed silence carrier alone.
    // #984: a SOFT (default-enabled, never explicitly requested) marker must never crash the
    // whole --paint-only run just because its PCM device is unavailable -- the permanent
    // cam2-painter.service (Restart=always, duration ~1 year) would otherwise either blank the
    // monitor forever (a persistent failure) or crash-loop every 2s (a transient one). An
    // EXPLICIT `--audio-marker` request keeps the original hard-fail contract (issue 936).
    // #1172: the SOFT (issue-984) degraded marker must SELF-RECOVER -- the shipped loop opened the
    // PCM ONCE and, on a transient-busy failure, only LOGGED "still DEGRADED" forever (never
    // re-attempting the open), so a marker degraded by a device momentarily held at painter start
    // (a lipsync-test ffmpeg still releasing hw:CARD=PCH,DEV=3) stayed silent until a manual
    // `systemctl restart cam2-painter`. This closure is the SINGLE ALSA open call site shared by
    // the initial open AND the degraded-retry path in the poll loop below, so the device is
    // re-opened each retry cycle and the marker recovers the moment it frees.
    let spawn_emitter = || {
        crate::probe::qpsk_emit::QpskEmitter::spawn(
            cfg.audio_marker_device.clone(),
            crate::qpsk_marker::AudioParams::rig60(),
            current_id.clone(),
            refresh_out.clone(),
            stop.clone(),
            start,
            cfg.wall_clock,
            cfg.audio_marker_cadence_ticks,
            cfg.marker_log.clone(),
        )
    };
    // #1172: markers accumulated across re-spawns -- a dropped dead emitter's partial log is joined
    // and preserved here so a mid-run recovery loses no markers; the final log write (below)
    // serializes this accumulated set unioned with the last live emitter's log.
    let mut accumulated_markers: crate::probe::qpsk_emit::QpskMarkerLog = Vec::new();
    let mut recovery = crate::audio_marker_policy::AudioMarkerRecovery::healthy();
    let mut audio_emitter = if cfg.audio_marker {
        match spawn_emitter() {
            Ok(emitter) => Some(emitter),
            Err(e) if cfg.audio_marker_soft => {
                tracing::error!(
                    device = %cfg.audio_marker_device,
                    error = %format!("{e:#}"),
                    retry_secs = crate::audio_marker_policy::RECOVERY_RETRY_INTERVAL.as_secs(),
                    "#984: audio-marker device failed to open -- continuing QR-only (degraded, \
                     silent rig, no audio); #1172: will retry the open periodically and recover \
                     once the device frees"
                );
                recovery = crate::audio_marker_policy::AudioMarkerRecovery::degraded();
                None
            }
            Err(e) => {
                return Err(e.context(format!(
                    "open audio-marker device {}",
                    cfg.audio_marker_device
                )))
            }
        }
    } else {
        None
    };

    let deadline = Instant::now() + cfg.duration;
    // #984: rate-limit the degraded-marker ERROR to roughly once per marker cadence period
    // instead of once per 100ms poll tick -- loud, but not a journal flood.
    let mut last_degraded_log = Instant::now();
    // #1172: the retry clock for re-opening a degraded marker's ALSA device -- seeded now (the
    // initial open attempt just happened above) and reset on every subsequent open attempt, so a
    // failed retry waits a full RECOVERY_RETRY_INTERVAL before the next one (never a hot spin on a
    // still-busy device).
    let mut last_open_attempt = Instant::now();
    while Instant::now() < deadline {
        if painter_handle.is_finished() {
            break;
        }
        // #936: fail the WHOLE run loudly the moment the QPSK marker thread dies before this
        // loop ever requests `stop` -- previously a silent ALSA-write death left the painter
        // running the full configured duration on a frozen marker log, so a LATER recording
        // measured against that dead coverage window still produced a plausible-looking (but
        // meaningless) offset from CRC-4 false decodes (see `qpsk_marker::qpsk_marker_died_message`
        // and the `--av-sync` fail-closed guard, `qpsk_marker::marker_coverage_overlaps_video_ticks`).
        //
        // #984: in SOFT mode a mid-run death degrades exactly like a startup open failure (log +
        // keep painting) instead of aborting -- an EXPLICIT (hard) request still bails as before.
        if let Some(reason) = audio_emitter.as_ref().and_then(|e| e.death_reason()) {
            if cfg.audio_marker_soft {
                // #1172: a SOFT mid-run death drops to degraded, and the retry path below re-opens
                // a FRESH emitter once the device frees. Drop the dead emitter (preserving its
                // partial marker log) and start the retry clock from the death so the first retry
                // is a full interval away.
                if let Some(dead) = audio_emitter.take() {
                    accumulated_markers.extend(dead.join());
                }
                recovery.mark_degraded();
                last_open_attempt = Instant::now();
                tracing::error!(
                    device = %cfg.audio_marker_device,
                    reason = %reason,
                    retry_secs = crate::audio_marker_policy::RECOVERY_RETRY_INTERVAL.as_secs(),
                    "#984/#1172: audio marker died mid-run -- degraded, will retry the open \
                     periodically and recover once the device frees"
                );
            } else {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                painter_handle.join().expect("painter panicked")?;
                anyhow::bail!("{}", crate::qpsk_marker::qpsk_marker_died_message(&reason));
            }
        }
        // #1172: while degraded, re-open the ALSA device every RECOVERY_RETRY_INTERVAL and
        // self-recover the moment it frees -- the recovery the shipped issue-984 loop never had.
        if recovery.step(last_open_attempt.elapsed())
            == crate::audio_marker_policy::MarkerRecoveryStep::AttemptReopen
        {
            last_open_attempt = Instant::now();
            match spawn_emitter() {
                Ok(emitter) => {
                    // NOTE: a successful re-open RE-CREATES (truncates) the incremental marker-log
                    // file (header only) inside `QpskEmitter::spawn`, so the live #420 poller sees
                    // its row count RESET then grow again on recovery -- growth still proves
                    // emission, and the FINAL log write below serializes the full accumulated set,
                    // so a consumer must not assume the incremental file grows monotonically.
                    tracing::info!(
                        device = %cfg.audio_marker_device,
                        "#1172: audio marker RECOVERED -- device reopened, marker emission resumed"
                    );
                    audio_emitter = Some(emitter);
                    recovery.record_reopen(true);
                }
                Err(_) => {
                    // Still busy -- stay degraded; the periodic #984 heartbeat below reports it and
                    // the next interval retries again (a full interval apart, never a hot spin).
                    recovery.record_reopen(false);
                }
            }
        }
        if recovery.is_degraded() && last_degraded_log.elapsed() >= Duration::from_secs(5) {
            tracing::error!(
                device = %cfg.audio_marker_device,
                "#984: audio marker still DEGRADED (no audio) -- QR-only, rig is silent"
            );
            last_degraded_log = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    painter_handle.join().expect("painter panicked")?;

    // Join the audio emitter and write its log before returning. #1172: union the markers
    // accumulated across any re-spawns (dropped dead emitters) with the last live emitter's log, so
    // a run that recovered from a degraded/died marker loses none of its emitted markers.
    let has_emitter_at_end = audio_emitter.is_some();
    if let Some(emitter) = audio_emitter {
        accumulated_markers.extend(emitter.join());
    }
    if let Some(path) = &cfg.marker_log {
        // Write the marker log when a live emitter is still running at the end (initially opened,
        // or recovered) OR markers were accumulated from one that died -- matching the original
        // "only when there was an emitter" behaviour while also covering the recovered-from-degraded
        // case. A run that stayed degraded end-to-end (no emitter, no markers) writes nothing, as
        // before.
        if has_emitter_at_end || !accumulated_markers.is_empty() {
            let csv = crate::qpsk_marker::serialize_qpsk_marker_log(
                &accumulated_markers,
                &crate::qpsk_marker::AudioParams::rig60(),
            );
            std::fs::write(path, csv)
                .with_context(|| format!("write marker log {}", path.display()))?;
            tracing::info!(path = %path.display(), markers = accumulated_markers.len(), "qpsk marker log written");
        }
    }

    let emitted_vec = emitted.lock().unwrap();
    // Write the cam→strih ground-truth CSV (#105) when a path was given, BEFORE
    // returning, so the recording-verdict has the painted-tick set this run
    // actually displayed (a strih tick the painter never painted = real phantom).
    if let Some(path) = &cfg.paint_log {
        std::fs::write(path, serialize_painter_log(&emitted_vec))
            .with_context(|| format!("write painter log {path}"))?;
        tracing::info!(path = %path, ticks = emitted_vec.len(), "painter log written");
    }
    Ok(emitted_vec.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::{default_paint_fps, serialize_painter_log};
    use crate::probe::analyzer::PaintMode;
    use crate::probe::presenter::PresenterKind;

    /// #290 HEADLINE: the rig `--paint-only --dual-qr` painter (presenter = `Auto`,
    /// the deployed cam2 path) must default to the FULL 60 fps capture rate, so it
    /// paints 60 distinct ticks/s when capture is 60 — NOT the sub-capture coverage
    /// default. At 30/12 ticks/s each painted id covers ≥2 capture frames and no
    /// 60fps optical timing can be resolved. RED before the fix (the path was wrongly
    /// excluded from the full-rate default and fell to 12.0).
    #[test]
    fn paint_only_defaults_to_full_capture_rate_at_60fps() {
        let fps = default_paint_fps(
            PaintMode::Coverage,
            60.0,
            PresenterKind::Auto,
            /* paint_only */ true,
            /* synth_ndi */ false,
        );
        assert_eq!(
            fps, 60.0,
            "#290: the rig paint-only painter must default to the capture rate (60 fps), \
             so it paints 60 distinct ticks/s — got {fps} (a sub-capture rate cannot resolve \
             60fps optical timing)"
        );
    }

    /// The paint-only painter must track the capture rate whatever the presenter, and
    /// whatever the capture rate — under the KMS auto path it is vblank-locked at the
    /// capture rate, under the fbdev fallback it sleep-paces at this configured rate,
    /// so a too-slow configured rate is the #290 30fps-painter bug on the fbdev path.
    #[test]
    fn paint_only_tracks_capture_rate_across_presenters_and_rates() {
        for presenter in [
            PresenterKind::Auto,
            PresenterKind::Kms,
            PresenterKind::Fbdev,
        ] {
            for cap in [50.0, 60.0, 120.0] {
                let fps = default_paint_fps(PaintMode::Coverage, cap, presenter, true, false);
                assert_eq!(
                    fps, cap,
                    "#290: paint-only must default to the capture rate ({cap}) on {presenter:?}"
                );
            }
        }
    }

    /// The fix must NOT regress the single-box fbdev loopback GATE: its in-process
    /// `run()` reader still wants the sub-capture coverage default (12 fps — ≥2 clean
    /// samples per id, no tearing false-loss). Only the real-presenter / paint-only
    /// paths take the capture rate.
    #[test]
    fn fbdev_loopback_gate_keeps_coverage_default() {
        let fps = default_paint_fps(
            PaintMode::Coverage,
            60.0,
            PresenterKind::Fbdev,
            false,
            false,
        );
        assert_eq!(
            fps, 12.0,
            "the fbdev single-box loopback gate must keep the 12 fps coverage default"
        );
        // The KMS/auto loopback run keeps its capture-rate default (unchanged by #290).
        assert_eq!(
            default_paint_fps(PaintMode::Coverage, 60.0, PresenterKind::Auto, false, false),
            60.0
        );
        // full-rate mode is always the capture rate; synth-ndi keeps the coverage default.
        assert_eq!(
            default_paint_fps(
                PaintMode::FullRate,
                60.0,
                PresenterKind::Fbdev,
                false,
                false
            ),
            60.0
        );
        assert_eq!(
            default_paint_fps(PaintMode::Coverage, 60.0, PresenterKind::Auto, false, true),
            12.0
        );
    }

    #[test]
    fn painter_log_csv_has_header_and_one_row_per_tick() {
        // The cam→strih ground-truth + cam2→cam1 flip-time CSV (#194): header
        // `tick,gen_ts_ns,flip_ts_ns` then one row per painted frame. `tick` stays
        // column 0 (the existing tick-column reader keys on the `tick,` prefix), gen_ts
        // column 1 (baked into the QR), flip_ts column 2 (on-screen instant, after the
        // page-flip). flip_ts >= gen_ts in every row (display follows generation).
        let csv = serialize_painter_log(&[(0, 1000, 1018), (1, 1016, 1034), (2, 1033, 1050)]);
        assert_eq!(
            csv, "tick,gen_ts_ns,flip_ts_ns\n0,1000,1018\n1,1016,1034\n2,1033,1050\n",
            "CSV: header + one `tick,gen_ts_ns,flip_ts_ns` row per painted frame"
        );
    }

    #[test]
    fn painter_log_empty_is_header_only() {
        // No painted frames ⇒ just the header (never an empty file the parser can't
        // distinguish from a missing log).
        assert_eq!(serialize_painter_log(&[]), "tick,gen_ts_ns,flip_ts_ns\n");
    }

    #[test]
    fn painter_log_carries_flip_ts_distinct_from_gen_ts() {
        // #194 REGRESSION: the flip column must be the THIRD field and carry the
        // flip-complete instant, distinct from gen_ts — proving the CSV preserves the
        // on-screen reference the cam2→cam1 latency needs (not just the gen_ts). A
        // serializer that dropped flip_ts (the pre-#194 2-column format) fails here.
        let csv = serialize_painter_log(&[(7, 2_000_000, 2_016_000)]);
        let row = csv.lines().nth(1).unwrap();
        let cols: Vec<&str> = row.split(',').collect();
        assert_eq!(cols.len(), 3, "row must be tick,gen_ts_ns,flip_ts_ns");
        assert_eq!(cols[2], "2016000", "column 2 is the flip-complete ts");
        assert_ne!(cols[1], cols[2], "flip_ts is a distinct stamp from gen_ts");
    }
}
