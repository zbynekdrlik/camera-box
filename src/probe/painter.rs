//! Painter thread: draw QR frames to /dev/fb0, paced, recording emitted IDs.

use crate::probe::fb::VsyncFb;
use crate::probe::payload::Payload;
use crate::probe::qr::render_qr_bgra;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct PaintParams {
    pub run_id: u32,
    pub fb_device: String,
    pub paint_fps: f64,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub qr_size: u32,
    /// Clock domain stamped into each frame's `gen_ts_ns`. `false` (default) ⇒
    /// the shared monotonic `Instant` — correct for Phase-1 single-box loopback
    /// where painter+reader share one process clock. `true` ⇒ CLOCK_REALTIME
    /// epoch ns — required for the #7 ABSOLUTE end-to-end latency, so the
    /// camera-emitted `gen_ts` and the dev1 endpoint tap's wall-clock `recv_ts`
    /// share the DanteSync-disciplined origin (strih = master). MUST match the
    /// taps' `wall_clock` or the subtraction is meaningless.
    pub wall_clock: bool,
}

/// `gen_ts_ns` on the configured clock domain — epoch ns when `wall_clock`, else
/// ns since the shared monotonic `start`. Single place that picks the painter's
/// time origin so it cannot diverge between the fb and synth-NDI paths.
pub fn gen_now(start: Instant, wall_clock: bool) -> i64 {
    if wall_clock {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock before epoch")
            .as_nanos() as i64
    } else {
        start.elapsed().as_nanos() as i64
    }
}

/// Paint until `stop` is set. Records `(frame_id, gen_ts_ns)` of every emitted frame.
pub fn run_painter(
    params: PaintParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    emitted: Arc<Mutex<Vec<(u32, i64)>>>,
) -> Result<()> {
    let mut fb = VsyncFb::open(&params.fb_device)?;
    let period = Duration::from_secs_f64(1.0 / params.paint_fps);
    let mut frame_id: u32 = 0;
    let mut next = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let gen_ts_ns = gen_now(start, params.wall_clock);
        let payload = Payload {
            run_id: params.run_id,
            frame_id,
            gen_ts_ns,
        };
        let bgra = render_qr_bgra(&payload, params.canvas_w, params.canvas_h, params.qr_size);
        fb.present(&bgra)?;
        emitted.lock().unwrap().push((frame_id, gen_ts_ns));

        frame_id = frame_id.wrapping_add(1);
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    tracing::info!("painter: emitted {} frames", frame_id);
    Ok(())
}
