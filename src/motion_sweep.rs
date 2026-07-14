//! camera-box #751 — a constant-velocity MOTION SWEEP for the cam2 painter (the UFO-test element).
//!
//! The dual-QR pattern changes every frame, but its smoothness is invisible to the naked EYE —
//! judder only shows when a camera points at real people/motion. This adds a bright ball/bar
//! sweeping HORIZONTALLY at CONSTANT velocity (bounce at the edges) in a dedicated strip along the
//! BOTTOM of the 1920×1080 painted frame, so any chain-side stutter is instantly visible by eye on
//! the monitor, on the multiview, and in recordings ("niečo čo sa pekne hýbe... na ktorom je vidieť
//! že pohyb je trhaný"). Constant-velocity linear motion is the most judder-revealing pattern for
//! the human visual system (the classic UFO-test principle).
//!
//! Position is a PURE function of the painter frame index, so a recording analysis could later also
//! verify per-frame displacement uniformity as a machine-readable smoothness signal (the primary
//! deliverable is the eye test, this is a bonus). Because the painter is 60fps vblank-locked (KMS
//! page-flip), the motion inherits that timing and any chain judder shows as visible steps.
//!
//! The strip lives in the bottom [`BAND_HEIGHT_PX`]px band — fully OUTSIDE the top dual-QR decode
//! zones AND the central colour-reference column (both live in the TOP band, y ∈ [top_margin,
//! top_margin+qr_size)). `ball_never_touches_the_dual_qr_or_colour_scale_zones` machine-proves the
//! clearance at the rig's `DEFAULT_QR_SIZE`, so a fused run still decodes every QR + colour patch.
//!
//! Pure Tier-0 (default features, no probe): the geometry + position math live here and unit-test
//! off-rig; the probe-gated painter (`src/probe/painter.rs` via `src/probe/qr.rs`) only CALLS
//! [`ball_rect`] / [`sweep_band`] to blit — the `src/reannounce.rs` / `src/colour_scale.rs` pattern.

use crate::colour_scale::Rect;

/// Height (px) of the bottom motion strip. 120px sits well below the rig dual-QR (bottom ~724 at
/// `DEFAULT_QR_SIZE`=700 on a 1080 canvas) and the colour column (same top band), so the strip is
/// clear of every decode zone.
pub const BAND_HEIGHT_PX: u32 = 120;

/// The sweeping ball/bar width (px). A wide bar reads clearly by eye at broadcast distance and
/// survives the downstream NDI re-compression better than a thin line.
pub const BALL_WIDTH_PX: u32 = 90;

/// The ball height (px) — shorter than the band so it is vertically centred with clearance above
/// and below.
pub const BALL_HEIGHT_PX: u32 = 80;

/// Constant horizontal velocity (px per painter frame). At 60fps → 1440 px/s; over the ~1830px
/// track (1920 − ball width) that is ~1.3s per one-way sweep — a lively, unmistakable bounce whose
/// steps reveal any judder.
pub const SPEED_PX_PER_FRAME: u32 = 24;

/// The ball's LEFT-EDGE x for painter frame `frame_idx` on a track of length `track_px` (the max
/// left-edge position), moving at `speed_px_per_frame` and REFLECTING at both ends (a triangle
/// wave). Constant |velocity| everywhere except the two turnaround points. Returns a value in
/// `[0, track_px]`; degenerate inputs (`track_px == 0` or `speed == 0`) return 0.
pub fn sweep_x(frame_idx: u64, track_px: u32, speed_px_per_frame: u32) -> u32 {
    if track_px == 0 || speed_px_per_frame == 0 {
        return 0;
    }
    // Triangle wave with period `2*track` (there and back). `phase = (frame_idx * speed) mod
    // period`. Reduce the multiply modulo `period` FIRST — `(a*b) mod m == ((a mod m)*(b mod m))
    // mod m` — so the product can never overflow u64 regardless of how long the painter runs.
    let period = 2 * track_px as u64;
    let phase = ((frame_idx % period) * (speed_px_per_frame as u64 % period)) % period;
    let x = if phase <= track_px as u64 {
        phase // outbound leg: 0 → track
    } else {
        period - phase // return leg: track → 0
    };
    x as u32
}

/// The full bottom motion band as a pixel rectangle: `[0, canvas_w) × [canvas_h − BAND_HEIGHT_PX,
/// canvas_h)`. Used to fill the track background and to bound the ball.
pub fn sweep_band(canvas_w: u32, canvas_h: u32) -> Rect {
    let h = BAND_HEIGHT_PX.min(canvas_h);
    Rect {
        x: 0,
        y: canvas_h - h,
        w: canvas_w,
        h,
    }
}

/// The ball's pixel rectangle for painter frame `frame_idx` on a `canvas_w×canvas_h` frame:
/// horizontally sweeping via [`sweep_x`], vertically centred inside the bottom [`BAND_HEIGHT_PX`]
/// band. Fully bounds-clamped so it never leaves the canvas or the band.
pub fn ball_rect(frame_idx: u64, canvas_w: u32, canvas_h: u32) -> Rect {
    let w = BALL_WIDTH_PX.min(canvas_w);
    let band_h = BAND_HEIGHT_PX.min(canvas_h);
    let h = BALL_HEIGHT_PX.min(band_h);
    let track = canvas_w.saturating_sub(w); // max left-edge x so x+w stays on-canvas
    let x = sweep_x(frame_idx, track, SPEED_PX_PER_FRAME);
    // Vertically centre the ball inside the bottom band.
    let band_top = canvas_h - band_h;
    let y = band_top + (band_h - h) / 2;
    Rect { x, y, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour_scale::{
        colour_scale_patches, dual_qr_rects, DEFAULT_QR_SIZE, TOP_MARGIN_PX,
    };

    const W: u32 = 1920;
    const H: u32 = 1080;

    #[test]
    fn sweep_x_is_constant_velocity_except_at_turnarounds() {
        let track = 1830u32;
        let speed = SPEED_PX_PER_FRAME;
        let mut turnarounds = 0;
        let mut prev = sweep_x(0, track, speed);
        // One full period is 2*track/speed frames; walk ~1.5 periods.
        let frames = (2 * track / speed) as u64 * 3 / 2 + 4;
        for f in 1..frames {
            let x = sweep_x(f, track, speed);
            let step = x.abs_diff(prev);
            if step == speed {
                // constant-velocity leg — fine
            } else {
                // the only non-`speed` steps are the reflections at the two edges
                assert!(
                    step < speed,
                    "a step must be either exactly the speed or a smaller reflection step, got {step} at frame {f}"
                );
                turnarounds += 1;
            }
            prev = x;
        }
        assert!(
            (2..=4).contains(&turnarounds),
            "over ~1.5 periods the ball must reverse ~3 times (a real bounce), got {turnarounds}"
        );
    }

    #[test]
    fn sweep_x_bounces_between_zero_and_the_track_and_never_leaves_it() {
        let track = 1830u32;
        let speed = SPEED_PX_PER_FRAME;
        let mut min_x = u32::MAX;
        let mut max_x = 0u32;
        for f in 0..(2 * track / speed) as u64 + 2 {
            let x = sweep_x(f, track, speed);
            assert!(
                x <= track,
                "x={x} left the track (track={track}) at frame {f}"
            );
            min_x = min_x.min(x);
            max_x = max_x.max(x);
        }
        assert_eq!(min_x, 0, "the ball must reach the left edge");
        assert!(
            max_x >= track - speed,
            "the ball must reach (within one step of) the right edge: max_x={max_x} track={track}"
        );
    }

    #[test]
    fn sweep_x_is_deterministic_and_guards_degenerate_inputs() {
        assert_eq!(
            sweep_x(12345, 1830, 24),
            sweep_x(12345, 1830, 24),
            "pure fn"
        );
        assert_eq!(
            sweep_x(7, 0, 24),
            0,
            "zero track → 0, no panic/divide-by-zero"
        );
        assert_eq!(sweep_x(7, 1830, 0), 0, "zero speed → 0");
    }

    #[test]
    fn ball_stays_entirely_inside_the_bottom_band() {
        let band = sweep_band(W, H);
        assert_eq!(band.y, H - BAND_HEIGHT_PX);
        assert_eq!(band.h, BAND_HEIGHT_PX);
        for f in 0..500u64 {
            let b = ball_rect(f, W, H);
            assert!(
                b.w > 0 && b.h > 0,
                "the ball must be a real rectangle at frame {f}"
            );
            assert!(
                b.y >= band.y,
                "ball top {} above the band {} at frame {f}",
                b.y,
                band.y
            );
            assert!(
                b.y + b.h <= H,
                "ball bottom {} past the canvas at frame {f}",
                b.y + b.h
            );
            assert!(
                b.x + b.w <= W,
                "ball right {} past the canvas at frame {f}",
                b.x + b.w
            );
        }
    }

    #[test]
    fn ball_never_touches_the_dual_qr_or_colour_scale_zones() {
        // THE safety gate for #751: at the rig's canvas + DEFAULT_QR_SIZE, the sweeping ball must
        // never share a pixel with either dual-QR half OR any colour-reference patch, over a full
        // sweep — so a fused run still decodes every QR and colour patch (#364/#207/dual-QR gates).
        let qrs = dual_qr_rects(W, H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
            .expect("the rig dual-QR layout must be non-degenerate");
        let patches: Vec<_> = colour_scale_patches(W, H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
            .into_iter()
            .map(|(rect, _rgb)| rect)
            .collect();
        assert!(
            !patches.is_empty(),
            "the rig colour scale must have patches to guard against"
        );
        for f in 0..2000u64 {
            let b = ball_rect(f, W, H);
            for qr in &qrs {
                assert!(
                    !b.intersects(qr),
                    "ball intersected a dual-QR half at frame {f}: {b:?} vs {qr:?}"
                );
            }
            for p in &patches {
                assert!(
                    !b.intersects(p),
                    "ball intersected a colour patch at frame {f}: {b:?} vs {p:?}"
                );
            }
        }
    }
}
