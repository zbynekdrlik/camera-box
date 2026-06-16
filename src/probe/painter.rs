//! Painter thread: draw QR frames to the HDMI output, recording emitted IDs.
//!
//! The painter writes through a [`Presenter`] (chosen at runtime): the DRM/KMS
//! page-flip presenter ([`crate::probe::kms::KmsPresenter`], tear-free + 1:1,
//! vblank-locked, #79) or the fbdev fallback ([`crate::probe::fb::VsyncFb`],
//! single-buffer vsync-gated write, #68).
//!
//! ## Pacing
//!
//! Two pacing regimes, selected by [`Presenter::paces_on_present`]:
//!
//! - **KMS (`paces_on_present() == true`)** — `present()` blocks until the next
//!   HDMI vblank (the page-flip completion event), so the painter advances
//!   exactly one new id per vblank: 60 fps, 1:1, phase-locked to the capture.
//!   The painter does NO sleeping; the hardware vblank is the clock.
//! - **fbdev (`paces_on_present() == false`)** — `present()` returns
//!   immediately, so the painter sleep-paces at `--paint-fps` (the #68 regime).

use crate::probe::clock_ns;
use crate::probe::payload::Payload;
use crate::probe::presenter::{open_presenter, Presenter, PresenterKind};
use crate::probe::qr::render_qr_bgra;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// #68: the strictly-next wall-clock frame boundary at or after `now_ns`, for a
/// frame period of `period_ns`. Pure pacing math (mirrors src/ndi.rs's genlock
/// `next_boundary_100ns`): the painter must advance ids on the SAME absolute
/// wall-clock boundaries the genlock decimator samples on, or the two equal-rate
/// cadences drift out of phase and the decimator skips painted ids (~13% measured
/// on the live cam2 rig at 30 fps paint into 60→30 decimation), which the
/// endpoint-sequence check then reports as spurious "missing" generator ids.
///
/// "Strictly next" (so an exact boundary advances one period) keeps the painter
/// from emitting two ids at the same instant. `period_ns == 0` (fps 0) is guarded
/// — returns `now_ns` (zero wait) rather than dividing by zero.
///
/// Only used in the fbdev (sleep-paced) regime; the KMS presenter paces on the
/// vblank flip and never calls this.
pub fn next_wall_boundary_ns(now_ns: u64, period_ns: u64) -> u64 {
    if period_ns == 0 {
        return now_ns;
    }
    (now_ns / period_ns + 1) * period_ns
}

/// Absolute wall-clock now in ns since the Unix epoch (the genlock clock domain).
fn wall_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock before epoch")
        .as_nanos() as u64
}

pub struct PaintParams {
    pub run_id: u32,
    pub fb_device: String,
    /// DRM card device for the KMS presenter (e.g. `/dev/dri/card1`). Ignored
    /// when the fbdev presenter is selected.
    pub drm_device: String,
    /// Which presenter to use (auto = KMS with fbdev fallback).
    pub presenter: PresenterKind,
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

/// Paint until `stop` is set. Records `(frame_id, gen_ts_ns)` of every emitted frame.
pub fn run_painter(
    params: PaintParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    emitted: Arc<Mutex<Vec<(u32, i64)>>>,
) -> Result<()> {
    let mut presenter: Box<dyn Presenter> =
        open_presenter(params.presenter, &params.fb_device, &params.drm_device)?;
    let flip_paced = presenter.paces_on_present();
    if flip_paced {
        if presenter.phase_locked() {
            tracing::info!(
                "painter: vblank-locked DRM page-flip — tear-free 1:1 at 60Hz (--paint-fps ignored)"
            );
        } else {
            tracing::warn!(
                "painter: presenter paces on vblank flip but mode is NOT 60Hz — \
                 NOT a true 1:1 phase-locked run (--paint-fps ignored)"
            );
        }
    }
    let mut frame_id: u32 = 0;

    // #68: pace on absolute WALL-CLOCK frame boundaries in the multi-node path
    // (`wall_clock` true — the genlock-decimated camera path) so the painter ticks
    // at the SAME phase as the genlock decimator (src/ndi.rs wall-boundary pacing).
    // A monotonic-paced 30 fps painter into a 60→30 wall-clock decimator drifts out
    // of phase and ~13% of ids are never sampled into NDI (measured live). For the
    // Phase-1 single-box loopback (`wall_clock` false) the painter+reader share one
    // monotonic process clock, so monotonic pacing is correct there — keep it.
    //
    // Both regimes apply ONLY when the presenter does not pace itself (fbdev). The
    // KMS presenter blocks on the vblank flip in present(), which IS the pacing.
    let period_ns: u64 = (1_000_000_000f64 / params.paint_fps).round() as u64;
    // Monotonic-pacing state (used only when !wall_clock && !flip_paced).
    let period = Duration::from_secs_f64(1.0 / params.paint_fps);
    let mut next = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let gen_ts_ns = clock_ns(start, params.wall_clock);
        let payload = Payload {
            run_id: params.run_id,
            frame_id,
            gen_ts_ns,
        };
        let bgra = render_qr_bgra(&payload, params.canvas_w, params.canvas_h, params.qr_size);
        // For KMS this blocks until the vblank flip completes — that block IS the
        // 1:1 pacing (one new id per HDMI vblank). For fbdev it returns at once.
        presenter.present(&bgra)?;
        emitted.lock().unwrap().push((frame_id, gen_ts_ns));

        frame_id = frame_id.wrapping_add(1);

        if flip_paced {
            // The vblank flip in present() already paced this iteration — no sleep.
            continue;
        }

        if params.wall_clock {
            // Sleep to the next absolute wall-clock boundary (decimator-phase-locked).
            let now = wall_now_ns();
            let target = next_wall_boundary_ns(now, period_ns);
            std::thread::sleep(Duration::from_nanos(target - now));
        } else {
            next += period;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    }
    tracing::info!("painter: emitted {} frames", frame_id);
    Ok(())
}
