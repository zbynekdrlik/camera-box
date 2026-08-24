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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// #1179: the mode-SELECTION refresh (milli-Hz) handed to `pick_mode` when opening the KMS
    /// presenter. Default `TARGET_REFRESH_MHZ` (60_000) selects exactly today's mode; an override
    /// (e.g. 100_000 for the 2560×1080@100 experiment) selects the higher-refresh mode. It is the
    /// SELECTION target only — `is_phase_lockable` still measures the 1:1 lock against the fixed
    /// 60 fps capture rate, so a non-60 Hz run is honestly reported NOT phase-locked. Ignored by the
    /// fbdev presenter (which paces on `--paint-fps`).
    pub mode_refresh_mhz: u32,
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
    /// #367: also paint the fixed colour-reference scale (a row of solid known-sRGB
    /// patches) along the bottom band of the canvas, clear of the dual-QR. Lets the
    /// monitor's colours be checked by eye AND sampled per-patch from the recording
    /// (the #364 colour gate). When `false` the canvas carries only the QR(s).
    pub colour_scale: bool,
    /// #751: also paint the constant-velocity motion sweep (a bright ball sweeping the bottom
    /// band) so judder is visible BY EYE on the monitor / multiview / recording, not only via QR
    /// decode. Fully outside the dual-QR + colour-scale zones. Default: ON in --paint-only mode
    /// (the permanent cam2 painter shows it), OFF otherwise.
    pub motion_sweep: bool,
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

/// #854: per-side "last stamped" `gen_ts_ns` for the dual-QR Vernier, persisted across painter
/// ticks by the caller so the SETTLED side's rendered payload stays byte-identical to the
/// previous tick — only the FRESH side (the one whose `frame_id` just changed, per
/// [`vernier_ids`]) gets a new `gen_ts_ns` baked into its payload this tick. Before this existed,
/// `paint_one_frame` stamped a fresh `gen_ts_ns` into BOTH halves every tick regardless of which
/// side changed, so the settled half's QR pixels silently differed tick-to-tick too — defeating
/// the anti-blur/tear-tolerance guarantee ([`vernier_ids`]'s own doc comment: "the other is
/// settled (sharp)"). The tick's own returned/logged `gen_ts_ns` (the ground truth
/// `recording-verdict` joins on by tick/frame_id) is UNCHANGED by this — it is still the fresh
/// clock read every tick; only the SETTLED side's BAKED payload now correctly reflects when that
/// content was actually last generated instead of claiming "now".
#[derive(Debug, Clone, Copy, Default)]
struct VernierGenTs {
    left: i64,
    right: i64,
}

/// Render + present ONE frame and return `(logical_id, gen_ts_ns, flip_ts_ns)`.
///
/// The ORDER is the #194 contract, and is the whole point of this helper being testable:
/// 1. stamp `gen_ts_ns` (frame GENERATION — baked into the QR, a pre-flip stamp),
/// 2. render the QR(s) carrying `gen_ts_ns`,
/// 3. `present()` — for KMS this BLOCKS until the vblank page-flip completes (the frame is
///    on screen); for fbdev it returns immediately,
/// 4. stamp `flip_ts_ns` AFTER `present()` returns (the on-screen instant), on the SAME
///    clock domain as `gen_ts_ns`.
///
/// Because step 4 happens strictly after step 1, `flip_ts_ns >= gen_ts_ns` on the monotonic
/// clock (the Phase-1 / default path, `wall_clock=false`). On the wall-clock path
/// (`wall_clock=true`, CLOCK_REALTIME) an NTP/PTP step backward between the two stamps could
/// momentarily make `flip < gen`; every consumer guards that (the flip-based latency guards
/// `flip > 0`, `painter_internal_gen_to_flip` skips `flip < gen`), so it is never a wrong
/// number. The gap is the painter's own render + vblank-wait time, the
/// test-rig artifact that cam2→cam1 must subtract by referencing `flip_ts_ns` not
/// `gen_ts_ns` (#194). Returning the triple (instead of stamping flip before present) is
/// what keeps the inflation out; a refactor that moved the flip stamp before `present()`
/// would break [`flip_ts_is_stamped_after_present_returns`].
fn paint_one_frame(
    presenter: &mut dyn Presenter,
    params: &PaintParams,
    start: Instant,
    frame_id: u32,
    refresh_tick: u64,
    vernier_gen: &mut VernierGenTs,
) -> Result<(u32, i64, i64)> {
    let gen_ts_ns = clock_ns(start, params.wall_clock);

    // The logical id is decided + the QR rendered here (pre-flip); the id is what the
    // camera reads from the QR.
    let (logical_id, mut bgra) = if params.dual_qr {
        // Vernier anti-blur: LEFT carries the latest EVEN tick, RIGHT the latest ODD
        // tick. Exactly one half changes per refresh — the other is settled (sharp).
        let (l, r) = vernier_ids(refresh_tick);
        let logical_id = l.max(r); // the freshly-painted half's id
                                   // #854: left is fresh exactly on EVEN ticks (vernier_ids' `l == tick` there); right is
                                   // fresh on ODD ticks, PLUS tick 0 (the bootstrap — both sides start fresh together, same
                                   // as vernier_ids(0) == (0, 0)). Only a fresh side's stored gen_ts_ns advances this tick;
                                   // a settled side keeps whatever it was last stamped with, so its payload — and therefore
                                   // its rendered QR pixels — is byte-identical to the previous tick.
        let left_fresh = refresh_tick.is_multiple_of(2);
        let right_fresh = refresh_tick == 0 || !refresh_tick.is_multiple_of(2);
        if left_fresh {
            vernier_gen.left = gen_ts_ns;
        }
        if right_fresh {
            vernier_gen.right = gen_ts_ns;
        }
        let left_payload = Payload {
            run_id: params.run_id,
            frame_id: l,
            gen_ts_ns: vernier_gen.left,
        };
        let right_payload = Payload {
            run_id: params.run_id,
            frame_id: r,
            gen_ts_ns: vernier_gen.right,
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

    // #367/#364: paint the colour-reference scale onto the SAME frame (a VERTICAL column in the
    // central gap BETWEEN the two dual-QR halves — where the camera reliably captures it), so the
    // displayed monitor + the recording carry it alongside the dual-QR. The column is derived from
    // the SAME qr_size/top_margin the dual-QR was rendered with, so painter and gate agree.
    if params.colour_scale {
        crate::probe::qr::blit_colour_scale_bgra(
            &mut bgra,
            params.canvas_w,
            params.canvas_h,
            params.qr_size,
            crate::probe::qr::TOP_MARGIN_PX,
        );
    }

    // #751 — paint the motion sweep LAST (over the blank bottom band, clear of every decode zone),
    // keyed on `refresh_tick` (the per-frame counter that advances in BOTH single- and dual-QR
    // modes), so its position is a pure function of the painter frame index.
    if params.motion_sweep {
        crate::probe::qr::blit_motion_sweep_bgra(
            &mut bgra,
            params.canvas_w,
            params.canvas_h,
            refresh_tick,
        );
    }

    // For KMS this blocks until the vblank flip completes — that block IS the 1:1 pacing
    // (one new id per HDMI vblank). For fbdev it returns at once.
    presenter.present(&bgra)?;
    // #194: present() has returned ⇒ the frame is now ON SCREEN (the page-flip completed for
    // KMS). Stamp the flip-complete instant on the SAME clock domain as gen_ts so cam2→cam1 =
    // cam1_capture − flip_ts is the true display→capture latency, not the inflated capture −
    // gen (which includes the painter's own render + vblank-wait time).
    let flip_ts_ns = clock_ns(start, params.wall_clock);
    Ok((logical_id, gen_ts_ns, flip_ts_ns))
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
///
/// `heartbeat` is #936's painter-wedge watchdog progress marker (see
/// `crate::painter_wedge`'s module doc): stamped with the monotonic elapsed-since-`start` ns
/// immediately after every successful `paint_one_frame()` call, unconditional on `dual_qr`/
/// `audio_marker` — a SEPARATE watchdog thread (spawned by the caller, `probe::run`) polls it and
/// forces the process to exit loudly the moment it goes stale, because a genuine DRM/KMS kernel
/// hang can leave this whole thread parked in an uninterruptible wait that no signal can preempt.
pub fn run_painter(
    params: PaintParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    emitted: Arc<Mutex<Vec<(u32, i64, i64)>>>,
    current_id: Option<Arc<AtomicU32>>,
    refresh_out: Option<Arc<AtomicU64>>,
    heartbeat: Arc<AtomicU64>,
) -> Result<()> {
    // #289 — keep the QR painter OFF the isolated capture core (onto the general
    // cores 0-2) so on the painter box (.62) generation can never steal from the
    // capture core. The non-capture cores are derived from /sys (never hardcoded).
    crate::affinity::pin_off_capture_core("painter");

    let mut presenter: Box<dyn Presenter> = open_presenter(
        params.presenter,
        &params.fb_device,
        &params.drm_device,
        params.canvas_w,
        params.canvas_h,
        params.mode_refresh_mhz,
    )?;
    // #936 review follow-up: seed the heartbeat the INSTANT open_presenter() succeeds, before the
    // paint loop even starts. open_presenter() (device open, acquire_master_lock, connector/mode
    // enumeration, the two make_slot() dumb-buffer allocations, the initial set_crtc) has NO inner
    // timeout of its own and can legitimately take longer than PAINTER_WEDGE_THRESHOLD_S (a cold
    // boot's connector/EDID probing, or the GPU/DRM subsystem still settling right after a PREVIOUS
    // wedge-triggered restart) -- without this seed the watchdog's clock starts at `start` (process
    // spawn) and would blame that legitimate open latency on the paint loop, potentially exiting
    // before the painter ever paints its first frame and turning one recoverable wedge into a
    // self-sustaining crash loop on cam2-painter.service's Restart=always/RestartSec=2. This
    // matches the #945 capture_wedge precedent's ACTUAL behavior (VideoCapture::open_with_controls
    // runs to completion on the main thread BEFORE that watchdog thread is even spawned, so device-
    // open cost is never charged against ITS threshold either) -- the loop below then updates this
    // same heartbeat every frame, so from here on the threshold measures only PER-FRAME stalls.
    heartbeat.store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
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
    // #854: per-side last-stamped gen_ts_ns, persisted across ticks — see VernierGenTs.
    let mut vernier_gen = VernierGenTs::default();

    // #1186: break the paint loop on a graceful shutdown signal (SIGTERM/SIGINT/SIGHUP, e.g.
    // `systemctl stop cam2-painter.service`) as well as the local `stop` flag. Breaking cleanly
    // returns from run_painter, which drops `presenter` -> KmsPresenter::Drop runs the #660
    // blank_fbdev, leaving /dev/fb0 a deterministic black frame instead of the last painted frame
    // frozen on cam2's HDMI monitor (SIGTERM's default disposition would otherwise skip Drop). The
    // decision is the Tier-0-tested `shutdown::painter_should_continue` so the shipped logic is the
    // tested logic.
    while crate::shutdown::painter_should_continue(
        stop.load(Ordering::Relaxed),
        crate::shutdown::is_shutdown_requested(),
    ) {
        let (logical_id, gen_ts_ns, flip_ts_ns) = paint_one_frame(
            presenter.as_mut(),
            &params,
            start,
            frame_id,
            refresh_tick,
            &mut vernier_gen,
        )?;
        // #936: progress proof for the painter-wedge watchdog — stamped the INSTANT
        // paint_one_frame() (render + present()) returns, unconditional on mode, mirroring
        // #945's "immediately after process_frame() returns" placement.
        heartbeat.store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        emitted
            .lock()
            .unwrap()
            .push((logical_id, gen_ts_ns, flip_ts_ns));
        refresh_tick = refresh_tick.wrapping_add(1);
        if let Some(ref c) = current_id {
            c.store(logical_id, Ordering::Relaxed);
        }
        if let Some(ref r) = refresh_out {
            r.store(refresh_tick, Ordering::Relaxed);
        }

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
    use super::{paint_one_frame, vernier_ids, PaintParams, VernierGenTs};
    use crate::probe::presenter::{Presenter, PresenterKind};
    use anyhow::Result;
    use std::time::{Duration, Instant};

    /// A fake presenter whose `present()` SLEEPS — modelling the KMS vblank-wait. The frame
    /// is "on screen" only when present() returns, so a correctly-ordered painter stamps
    /// flip_ts strictly AFTER that sleep. The sleep makes the gen→flip gap large + reliably
    /// measurable, so the test FAILS if flip_ts were ever stamped before present() returns
    /// (the #194 inflation bug).
    struct SleepyPresenter {
        present_sleep: Duration,
    }
    impl Presenter for SleepyPresenter {
        fn dimensions(&self) -> (u32, u32) {
            (64, 64)
        }
        fn present(&mut self, _bgra: &[u8]) -> Result<()> {
            std::thread::sleep(self.present_sleep);
            Ok(())
        }
        fn paces_on_present(&self) -> bool {
            true
        }
    }

    fn test_params() -> PaintParams {
        PaintParams {
            run_id: 7,
            fb_device: String::new(),
            drm_device: String::new(),
            presenter: PresenterKind::Fbdev,
            paint_fps: 60.0,
            canvas_w: 64,
            canvas_h: 64,
            qr_size: 32,
            mode_refresh_mhz: crate::probe::kms::TARGET_REFRESH_MHZ,
            wall_clock: false,
            dual_qr: false,
            colour_scale: false,
            motion_sweep: false,
        }
    }

    #[test]
    fn flip_ts_is_stamped_after_present_returns() {
        // #194 CONTRACT TEST (not a tautology): drive the real per-frame helper with a
        // presenter that BLOCKS ~10ms in present(). gen_ts is stamped before render; flip_ts
        // after present() returns. So flip_ts − gen_ts MUST be at least the present() sleep —
        // proving flip is stamped on the on-screen side of the flip, not before it. A
        // refactor that moved the flip stamp before present() would yield ~0 gap and FAIL.
        let mut p = SleepyPresenter {
            present_sleep: Duration::from_millis(10),
        };
        let params = test_params();
        let start = Instant::now();
        let (id, gen_ts, flip_ts) =
            paint_one_frame(&mut p, &params, start, 0, 0, &mut VernierGenTs::default()).unwrap();
        assert_eq!(id, 0, "single-QR logical id is the frame_id");
        assert!(flip_ts >= gen_ts, "flip_ts >= gen_ts always");
        let gap_ms = (flip_ts - gen_ts) as f64 / 1_000_000.0;
        assert!(
            gap_ms >= 9.0,
            "flip_ts must be stamped AFTER the ~10ms present() block (gap was {gap_ms:.2}ms) — \
             a flip stamped before present() would re-inflate cam2→cam1 (#194)"
        );
    }

    #[test]
    fn flip_ts_ge_gen_ts_with_instant_present_too() {
        // With a non-blocking present() (the fbdev regime) flip_ts is still >= gen_ts (time
        // only moves forward) — the contract holds regardless of presenter pacing. dual_qr
        // path here, to cover the Vernier logical-id branch in the same helper.
        let mut p = SleepyPresenter {
            present_sleep: Duration::ZERO,
        };
        let mut params = test_params();
        params.dual_qr = true;
        let start = Instant::now();
        let (id, gen_ts, flip_ts) =
            paint_one_frame(&mut p, &params, start, 0, 4, &mut VernierGenTs::default()).unwrap();
        // refresh_tick 4 ⇒ vernier_ids(4) = (4, 3) ⇒ logical_id = max = 4.
        assert_eq!(
            id, 4,
            "dual-QR logical id is the freshly-painted (max) half"
        );
        assert!(
            flip_ts >= gen_ts,
            "flip_ts >= gen_ts on instant present too"
        );
    }

    /// A presenter that stores the last frame it was given, so a test can decode what was
    /// actually rendered (paint_one_frame itself only returns the (id, gen_ts, flip_ts) tuple,
    /// not the pixels).
    struct CapturingPresenter {
        dims: (u32, u32),
        last: Option<Vec<u8>>,
    }
    impl Presenter for CapturingPresenter {
        fn dimensions(&self) -> (u32, u32) {
            self.dims
        }
        fn present(&mut self, bgra: &[u8]) -> Result<()> {
            self.last = Some(bgra.to_vec());
            Ok(())
        }
        fn paces_on_present(&self) -> bool {
            false
        }
    }

    fn decode_payload(
        bgra: &[u8],
        w: u32,
        h: u32,
        frame_id: u32,
    ) -> crate::probe::payload::Payload {
        let luma = crate::probe::luma::bgra_to_luma(bgra, w, h, w * 4);
        crate::probe::qr::decode_qr_luma_all(luma)
            .into_iter()
            .find(|p| p.frame_id == frame_id)
            .unwrap_or_else(|| panic!("frame_id {frame_id} not found in decoded payloads"))
    }

    /// #854 RED: the dual-QR Vernier anti-blur/tear-tolerance guarantee ("the settled half is
    /// byte-identical to the previous tick, so a capture straddling the refresh boundary reads
    /// the same bits regardless of which side of the seam it lands on") does NOT hold today —
    /// `paint_one_frame` stamps a FRESH gen_ts_ns into BOTH halves' payloads every tick,
    /// regardless of which side's frame_id actually changed. Real-rig evidence (#854): a capture
    /// straddling exactly one 60Hz refresh shows ~50% data-module bit errors on whichever side
    /// it reads mid-transition — consistent with the settled side's payload (and therefore its
    /// QR pixels) silently changing every tick.
    ///
    /// tick 2 -> vernier_ids(2) = (2, 1): LEFT just became fresh (id 2).
    /// tick 3 -> vernier_ids(3) = (2, 3): LEFT is now SETTLED (still id 2); RIGHT is fresh (id 3).
    /// The LEFT payload decoded from each tick's rendered canvas must be byte-identical.
    #[test]
    fn settled_left_half_payload_is_byte_identical_across_the_next_tick() {
        let (w, h, qr) = (1920u32, 1080u32, 700u32);
        let mut p = CapturingPresenter {
            dims: (w, h),
            last: None,
        };
        let mut params = test_params();
        params.dual_qr = true;
        params.canvas_w = w;
        params.canvas_h = h;
        params.qr_size = qr;
        let start = Instant::now();
        let mut vernier = VernierGenTs::default();

        let (_, gen2, _) = paint_one_frame(&mut p, &params, start, 0, 2, &mut vernier).unwrap();
        let bgra2 = p.last.clone().unwrap();
        std::thread::sleep(Duration::from_micros(1));
        let (_, gen3, _) = paint_one_frame(&mut p, &params, start, 0, 3, &mut vernier).unwrap();
        let bgra3 = p.last.clone().unwrap();

        let left2 = decode_payload(&bgra2, w, h, 2);
        let left3 = decode_payload(&bgra3, w, h, 2);
        let right3 = decode_payload(&bgra3, w, h, 3);

        assert_eq!(
            left2.gen_ts_ns, gen2,
            "the fresh side's payload carries the tick's own gen_ts_ns"
        );
        assert_eq!(
            right3.gen_ts_ns, gen3,
            "the fresh side's payload carries the tick's own gen_ts_ns"
        );
        assert_ne!(
            gen2, gen3,
            "two distinct ticks must stamp two distinct clock reads"
        );
        assert_eq!(
            left3, left2,
            "#854: the SETTLED left half's payload (frame_id AND gen_ts_ns) must be byte-\
             identical across the tick where only the right half changes — a capture straddling \
             this refresh boundary must read the same bits either side of the seam"
        );
    }

    /// #1179: the dual-QR must still encode→render→decode cleanly at the 2560×1080 override canvas
    /// with the proportionally-scaled QR (700 → 933). The half positions are `canvas_w/2`-relative
    /// in `render_qr_dual_bgra`, so they auto-scale; only `qr_size` changes. Decoded via the SAME
    /// production path (`decode_qr_luma_all`, through the test's `decode_payload` helper) the
    /// recording verdict uses — a real-captured-frame fixture is deferred to adoption time.
    #[test]
    fn dual_qr_round_trips_at_the_scaled_2560_canvas_geometry_1179() {
        let (w, h) = (2560u32, 1080u32);
        let qr =
            crate::painter_mode::scaled_qr_size(700, crate::painter_mode::BASELINE_CANVAS_W, w);
        assert_eq!(qr, 933, "scaled QR px for a 2560-wide canvas");
        let mut p = CapturingPresenter {
            dims: (w, h),
            last: None,
        };
        let mut params = test_params();
        params.dual_qr = true;
        params.canvas_w = w;
        params.canvas_h = h;
        params.qr_size = qr;
        let start = Instant::now();
        let mut vernier = VernierGenTs::default();
        // tick 4 → vernier_ids(4) = (4, 3): left=4 (fresh), right=3 (settled). Both halves must
        // decode to their own payloads from the scaled-geometry render.
        let (logical_id, gen, _) =
            paint_one_frame(&mut p, &params, start, 0, 4, &mut vernier).unwrap();
        assert_eq!(
            logical_id, 4,
            "dual-QR logical id is the freshly-painted (max) half"
        );
        let bgra = p.last.clone().unwrap();
        assert_eq!(
            bgra.len(),
            (w * h * 4) as usize,
            "canvas is the full 2560×1080 BGRA frame"
        );

        let left = decode_payload(&bgra, w, h, 4);
        let right = decode_payload(&bgra, w, h, 3);
        assert_eq!(left.run_id, params.run_id);
        assert_eq!(left.frame_id, 4);
        assert_eq!(
            left.gen_ts_ns, gen,
            "the fresh left half carries this tick's gen_ts_ns"
        );
        assert_eq!(right.run_id, params.run_id);
        assert_eq!(right.frame_id, 3);
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
