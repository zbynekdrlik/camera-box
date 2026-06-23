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
use crate::probe::qr::{render_qr_bgra, render_qr_dual_bgra};
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
    /// Paint two QRs side by side using the Vernier anti-blur scheme (spec §dual-QR).
    /// When `true`, `run_painter` drives a `refresh_tick` counter and alternates
    /// which half is freshly painted each tick so at least one half is always
    /// settled (sharp) when the camera fires. When `false` (default) the original
    /// single-QR path is used unchanged.
    pub dual_qr: bool,
}

/// Vernier dual-QR ids for refresh counter `tick`. LEFT carries the latest EVEN
/// tick, RIGHT the latest ODD tick, so exactly one region changes per refresh and
/// the two are never freshly-painted on the same refresh — at least one is settled
/// (sharp) when the camera fires (the anti-blur guarantee, spec §dual-QR).
pub fn vernier_ids(tick: u64) -> (u32, u32) {
    let left = tick & !1; // latest even <= tick
    let right = if tick == 0 { 0 } else { (tick - 1) | 1 }.min(tick); // latest odd <= tick
    (left as u32, right as u32)
}

/// Paint until `stop` is set. Records `(frame_id, gen_ts_ns, flip_ts_ns)` of every
/// emitted frame.
///
/// `gen_ts_ns` is stamped at frame GENERATION (the top of the iteration) — it is the
/// value baked into the QR, which is necessarily a pre-flip stamp (the QR is rendered
/// before the page-flip). `flip_ts_ns` is captured AFTER `present()` returns — for the
/// vblank-locked KMS presenter that return IS the page-flip-complete event, i.e. the
/// instant the frame is ACTUALLY ON SCREEN (#194). The cam2→cam1 optical latency must
/// reference `flip_ts_ns` (real display→capture), NOT `gen_ts_ns`, or it is inflated by
/// the painter's own generate→render→wait-for-vblank time (~16-30ms @ 60Hz). For the
/// fbdev presenter `present()` returns immediately, so `flip_ts_ns` is the post-write
/// instant; it is still >= `gen_ts_ns` (time only moves forward).
pub fn run_painter(
    params: PaintParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    emitted: Arc<Mutex<Vec<(u32, i64, i64)>>>,
) -> Result<()> {
    let mut presenter: Box<dyn Presenter> = open_presenter(
        params.presenter,
        &params.fb_device,
        &params.drm_device,
        params.canvas_w,
        params.canvas_h,
    )?;
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

    // Dual-QR Vernier: counts refreshes so vernier_ids can assign stable left/right ids.
    let mut refresh_tick: u64 = 0;

    while !stop.load(Ordering::Relaxed) {
        let gen_ts_ns = clock_ns(start, params.wall_clock);

        // The logical id is decided + the QR rendered here (pre-flip); the id is what the
        // camera reads from the QR. The emitted-log push is DEFERRED until AFTER present()
        // returns so it can carry the flip-complete timestamp (#194) alongside gen_ts.
        let (logical_id, bgra) = if params.dual_qr {
            // Vernier anti-blur: LEFT carries the latest EVEN tick, RIGHT the latest ODD
            // tick. Exactly one half changes per refresh — the other is settled (sharp).
            let (l, r) = vernier_ids(refresh_tick);
            let logical_id = l.max(r); // the freshly-painted half's id
            let left_payload = Payload {
                run_id: params.run_id,
                frame_id: l,
                gen_ts_ns,
            };
            let right_payload = Payload {
                run_id: params.run_id,
                frame_id: r,
                gen_ts_ns,
            };
            (
                logical_id,
                render_qr_dual_bgra(
                    &left_payload,
                    &right_payload,
                    params.canvas_w,
                    params.canvas_h,
                    params.qr_size,
                ),
            )
        } else {
            let payload = Payload {
                run_id: params.run_id,
                frame_id,
                gen_ts_ns,
            };
            (
                frame_id,
                render_qr_bgra(&payload, params.canvas_w, params.canvas_h, params.qr_size),
            )
        };

        // For KMS this blocks until the vblank flip completes — that block IS the
        // 1:1 pacing (one new id per HDMI vblank). For fbdev it returns at once.
        presenter.present(&bgra)?;
        // #194: present() has returned ⇒ the frame is now ON SCREEN (the page-flip
        // completed for KMS). Stamp the flip-complete instant on the SAME clock domain
        // as gen_ts so cam2→cam1 = cam1_capture − flip_ts is the true display→capture
        // latency, not the inflated capture − gen (which includes the painter's own
        // render + vblank-wait time). flip_ts >= gen_ts always (time only moves forward).
        let flip_ts_ns = clock_ns(start, params.wall_clock);
        emitted
            .lock()
            .unwrap()
            .push((logical_id, gen_ts_ns, flip_ts_ns));
        refresh_tick = refresh_tick.wrapping_add(1);

        if !params.dual_qr {
            frame_id = frame_id.wrapping_add(1);
        }

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
    tracing::info!("painter: emitted {} frames", emitted.lock().unwrap().len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::vernier_ids;
    use crate::probe::clock_ns;
    use std::time::Instant;

    #[test]
    fn flip_ts_is_taken_after_gen_ts_so_flip_ge_gen() {
        // #194: gen_ts is stamped at generation (top of the loop, baked into the QR);
        // flip_ts is stamped AFTER present() returns (the on-screen instant). The painter
        // takes them in that order on the SAME clock domain, so flip_ts >= gen_ts ALWAYS
        // (a monotonic clock only moves forward). This locks the ordering contract the
        // cam2→cam1 flip-based latency relies on; a future refactor that stamped flip
        // BEFORE present (re-introducing the inflation bug) would break it.
        let start = Instant::now();
        for _ in 0..1000 {
            let gen_ts = clock_ns(start, false); // monotonic domain (Phase-1 path)
                                                 // (render + present happen between the two stamps in the real loop)
            let flip_ts = clock_ns(start, false);
            assert!(
                flip_ts >= gen_ts,
                "flip_ts ({flip_ts}) must be >= gen_ts ({gen_ts}) — flip is stamped after \
                 present() returns; a flip stamped before generation would re-inflate \
                 cam2→cam1 (#194)"
            );
        }
        // Same invariant must hold on the wall-clock (multi-node #7) domain.
        for _ in 0..1000 {
            let gen_ts = clock_ns(start, true);
            let flip_ts = clock_ns(start, true);
            assert!(flip_ts >= gen_ts, "flip_ts >= gen_ts on wall clock too");
        }
    }

    #[test]
    fn vernier_ids_interleave_even_left_odd_right() {
        assert_eq!(vernier_ids(0), (0, 0)); // tick 0: left fresh=0, no odd yet -> right 0
        assert_eq!(vernier_ids(1), (0, 1)); // right updates to 1
        assert_eq!(vernier_ids(2), (2, 1)); // left updates to 2
        assert_eq!(vernier_ids(3), (2, 3)); // right updates to 3
        assert_eq!(vernier_ids(4), (4, 3));
        // The fresh side equals the tick; the other is the previous parity -> the two
        // are never both freshly-changed on the same tick (the anti-blur guarantee).
        for t in 1..1000u64 {
            let (l, r) = vernier_ids(t);
            let fresh_is_left = t % 2 == 0;
            if fresh_is_left {
                assert_eq!(l as u64, t);
            } else {
                assert_eq!(r as u64, t);
            }
        }
    }
}
