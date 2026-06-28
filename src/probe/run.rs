//! Orchestrate painter + reader for a fixed duration, then analyze.

use crate::probe::analyzer::{analyze, AnalysisInput, AnalysisReport, Observed, PaintMode};
use crate::probe::painter::{run_painter, PaintParams};
use crate::probe::presenter::PresenterKind;
use crate::probe::reader::{run_reader, ReadParams};
use anyhow::{Context, Result};
use std::sync::atomic::AtomicBool;
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
    /// Optional path for `run_paint_only` to write the painter's emitted-tick
    /// CSV (`tick,gen_ts_ns`) — the cam→strih ground truth consumed by
    /// `recording-verdict --painter` (#105). `None` ⇒ no log written.
    pub paint_log: Option<String>,
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

pub fn run(cfg: RunConfig) -> Result<AnalysisReport> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));

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
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            drm_device: cfg.drm_device.clone(),
            presenter: cfg.presenter,
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
            // Phase-1 single-box loopback: painter + reader share THIS process's
            // monotonic clock, so latency is exact without any sync. A wall-clock
            // gen here would break that — force monotonic regardless of cfg.
            wall_clock: false,
            dual_qr: cfg.dual_qr,
        };
        std::thread::spawn(move || run_painter(params, start, stop, emitted))
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

    let painter_handle = {
        let stop = stop.clone();
        let emitted = emitted.clone();
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            drm_device: cfg.drm_device.clone(),
            presenter: cfg.presenter,
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
            // Multi-node (#7): stamp gen_ts on the DanteSync wall clock when asked
            // so the dev1 endpoint tap's wall-clock recv − this gen is true
            // absolute latency. Defaults false (Phase-2 relative latency only).
            wall_clock: cfg.wall_clock,
            dual_qr: cfg.dual_qr,
        };
        std::thread::spawn(move || run_painter(params, start, stop, emitted))
    };

    let deadline = Instant::now() + cfg.duration;
    while Instant::now() < deadline {
        if painter_handle.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    painter_handle.join().expect("painter panicked")?;

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
