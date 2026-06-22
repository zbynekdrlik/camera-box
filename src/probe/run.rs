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

pub fn run(cfg: RunConfig) -> Result<AnalysisReport> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64)>>> = Arc::new(Mutex::new(Vec::new()));
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
        .filter(|(_, gen_ts)| *gen_ts <= cutoff_ns)
        .map(|(id, _)| *id)
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

/// Serialize the painter's emitted `(logical_tick, gen_ts_ns)` sequence into the
/// `recording-verdict --painter` CSV (`tick,gen_ts_ns`, one row per painted
/// frame, header `tick,gen_ts_ns`). This is the cam→strih GROUND TRUTH (#105):
/// every logical tick the painter actually displayed. The recording-verdict
/// parser reads the `tick` column (its CSV reader takes the 1st column of a bare
/// file or the 3rd of a recording-probe CSV; here `tick` is the 1st column, so a
/// strih tick the painter never displayed is provably a phantom/corruption).
///
/// PURE (no I/O): the caller writes the returned string to the chosen path so the
/// formatting is unit-testable without spawning a painter or a presenter.
pub fn serialize_painter_log(emitted: &[(u32, i64)]) -> String {
    let mut s = String::from("tick,gen_ts_ns\n");
    for (tick, gen_ts_ns) in emitted {
        s.push_str(&format!("{tick},{gen_ts_ns}\n"));
    }
    s
}

/// Paint QR frames for `duration` without receiving/analyzing — used on the
/// camera box in Phase 2, where the QR reaches NDI via camera-box's own
/// capture→NDI path and the taps run elsewhere (dev1).
pub fn run_paint_only(cfg: &RunConfig) -> Result<u64> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64)>>> = Arc::new(Mutex::new(Vec::new()));

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
    use super::serialize_painter_log;

    #[test]
    fn painter_log_csv_has_header_and_one_row_per_tick() {
        // The cam→strih ground-truth CSV: header `tick,gen_ts_ns` then one row per
        // painted frame, the `tick` first so recording-verdict's --painter reads it
        // as the painted-tick set (a strih tick absent here = phantom/corruption).
        let csv = serialize_painter_log(&[(0, 1000), (1, 1016), (2, 1033)]);
        assert_eq!(
            csv, "tick,gen_ts_ns\n0,1000\n1,1016\n2,1033\n",
            "CSV: header + one `tick,gen_ts_ns` row per painted frame"
        );
    }

    #[test]
    fn painter_log_empty_is_header_only() {
        // No painted frames ⇒ just the header (never an empty file the parser can't
        // distinguish from a missing log).
        assert_eq!(serialize_painter_log(&[]), "tick,gen_ts_ns\n");
    }
}
