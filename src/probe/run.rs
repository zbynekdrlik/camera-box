//! Orchestrate painter + reader for a fixed duration, then analyze.

use crate::probe::analyzer::{analyze, AnalysisInput, AnalysisReport, Observed, PaintMode};
use crate::probe::painter::{run_painter, PaintParams};
use crate::probe::reader::{run_reader, ReadParams};
use anyhow::Result;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RunConfig {
    pub mode: PaintMode,
    pub run_id: u32,
    pub source: String,
    pub fb_device: String,
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
        };
        std::thread::spawn(move || run_reader(params, start, stop, observed))
    };

    let painter_handle = {
        let stop = stop.clone();
        let emitted = emitted.clone();
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
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
    }))
}
