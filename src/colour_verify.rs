//! #364 — per-camera COLOUR-correctness gate (pure decision + sampler).
//!
//! The zero-loss verdict ([`crate::probe`] / `bin/recording-verdict`) proves frame DELIVERY
//! only — it never looks at COLOUR, so a camera that goes grayscale / white-balance-shifted /
//! hue-rotated delivers every frame and still PASSES. Live events kept failing on exactly that.
//! This module is the gate that FAILS on it.
//!
//! ## How it ties to the painter (one source of truth)
//!
//! The #367 painter ([`crate::colour_scale`]) blits a column of KNOWN-sRGB reference patches in the
//! central gap between the two dual-QR halves on the cam2 monitor — the same content rides
//! cam1→strih→stream and lands in every node's recording. This module iterates the SAME
//! [`colour_scale_patches`] geometry and
//! [`PATCH_COLOURS`] table, samples each patch's mean colour from a recorded frame, and compares
//! it to the known value. There is ONE colour table and ONE geometry for both the painter (write)
//! and this verifier (read).
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same Tier-0 seam as [`crate::colour_scale`] / [`crate::reannounce`]: the whole `probe` module
//! is `#[cfg(feature = "probe")]` (it pulls `image`/`rqrr`/`drm`, which balloon the shared dev1
//! `target/`, #185). The DECISION logic + the pixel SAMPLER here have no probe deps, so they
//! unit-test on default features with a SYNTHETIC frame painted from the colour table. The
//! probe-gated glue (`probe::colour_sample`) only supplies the real recorded RGB pixels + the
//! per-node burn-exclusion rects and the ffmpeg colour pass; ALL the judgement is here, locked by
//! Tier-0 tests.
//!
//! ## The decision (strict, REAL signal — gated on HUE + CHROMA, never on brightness)
//!
//! The gate checks COLOUR (hue + saturation), NOT brightness/level. Brightness is unverifiable on
//! this rig: the cam2 monitor refreshes at 60 Hz while the broadcast camera shoots at 1/1000 s, so
//! the camera samples only ~1 ms of each 16.6 ms refresh (mid-redraw) — the captured colour scale
//! arrives ~7× DIM by physics, not by fault. A correct primary reads e.g. red `(255,0,0)` →
//! `(95,45,46)`: right hue (1°), ample chroma (50), but far in sRGB distance purely from the level
//! drop. A pure sRGB-distance / level check therefore false-fails a perfectly correct camera, so it
//! is NOT used. Hue (exposure/compression robust) + chroma (collapse detector) are the real signal.
//!
//! Per reference patch, expected colour `E` vs sampled mean `S`:
//! - **CHROMATIC** expected (a primary/secondary — chroma ≥ [`CHROMATIC_EXPECTED_MIN`]):
//!   - sampled chroma below [`GRAYSCALE_CHROMA_MIN`] ⇒ **Grayscale** (the colour collapsed toward
//!     gray — desaturated / mono camera). Checked FIRST: a grayscale patch has no meaningful hue.
//!   - else hue error above [`HUE_TOL_DEG`] ⇒ **HueShift** (channel swap / white-balance cast).
//!   - else **Pass** (correct hue + un-collapsed chroma — a correct colour, however dim).
//! - **NEUTRAL** expected (white/black/gray ramp — chroma < [`CHROMATIC_EXPECTED_MIN`]):
//!   - sampled chroma above [`NEUTRAL_CHROMA_MAX`] ⇒ **NeutralTint** (a cast where there is none).
//!   - else **Pass** (stayed near-neutral).
//! - a patch with NO samplable pixels (fully behind a known burn / off canvas) ⇒ **Unsamplable** —
//!   SKIPPED, not charged: a real colour defect is global and still fails on the VISIBLE patches, so
//!   dodging a burn-covered patch never lets a defect through. The verdict is fail-closed only when
//!   NOTHING was checkable (zero checked patches proves nothing).
//!
//! A camera FAILS the gate if ANY checked patch is wrong (or nothing was checkable). The tolerances
//! are calibrated so a CORRECT colour through real H.264/NDI transport (compression, gamma, and the
//! rig's dim optical capture) passes comfortably while a grayscale / hue-shifted / channel-dead
//! camera fails by a wide margin — hue is exposure/compression robust, and chroma collapse is
//! unmistakable. They are a STRICT bar; the recorded-fixture proof
//! (`real_rig_dim_capture_passes_while_grayscale_and_dead_channel_fail`) locks them against real
//! per-patch rig values. They are NEVER to be loosened to force a pass (strict-test mandate) — and
//! dropping the level check is the OPPOSITE of loosening: it removes a check that only ever
//! false-fails the physically-dim rig, while every real colour fault still fails on hue/chroma.

use crate::colour_scale::{colour_scale_patches, Rect, Rgb, PATCH_COLOURS};

/// A reference patch whose KNOWN colour has chroma (max−min channel) at or above this is treated
/// as CHROMATIC (the six primaries/secondaries — chroma 255). Below it the patch is NEUTRAL
/// (white/black + the gray ramp, chroma 0). 64 sits far below 255 and far above 0, so the split is
/// unambiguous for the fixed [`PATCH_COLOURS`] table.
pub const CHROMATIC_EXPECTED_MIN: f64 = 64.0;

/// A CHROMATIC patch whose SAMPLED chroma falls below this has collapsed toward gray — the camera
/// went grayscale / heavily desaturated. The painted primaries arrive with chroma ~255; even
/// heavy H.264/NDI desaturation keeps a true colour well above 40, while a mono/grayscale path
/// lands at ~0. A correct colour can never drop this low.
pub const GRAYSCALE_CHROMA_MIN: f64 = 40.0;

/// Max hue error (degrees, circular) a CHROMATIC patch's sampled colour may differ from its known
/// hue. Hue is robust to exposure and compression, so a correct colour stays within a few degrees;
/// a real channel swap / white-balance hue-shift moves the hue by 60–180°, far beyond 30.
pub const HUE_TOL_DEG: f64 = 30.0;

/// A NEUTRAL (white/black/gray) patch must stay near-neutral — sampled chroma above this is an
/// unexpected colour cast (e.g. a white-balance tint), which FAILS. On the real rig the benign
/// optical cast measures ≤ 38 chroma across the neutral ramp (white 29, g255 38), so 48 sits above
/// the rig's natural cast yet well below a real white-balance fault / dead channel (tens to >100).
pub const NEUTRAL_CHROMA_MAX: f64 = 48.0;

/// Chroma of an sRGB colour: `max(channel) − min(channel)`, 0 (neutral) .. 255 (pure primary).
pub fn chroma(c: Rgb) -> f64 {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    (max - min) as f64
}

/// Hue angle of an sRGB colour in degrees `[0, 360)`, or `None` when the colour is achromatic
/// (chroma 0 — a hue is undefined for pure gray). Standard HSV hue (the max channel selects the
/// 60°-sextant); ties resolve to the same canonical hue as the pure secondaries (cyan 180°,
/// magenta 300°, yellow 60°).
pub fn hue_deg(c: Rgb) -> Option<f64> {
    let (r, g, b) = (c.r as f64, c.g as f64, c.b as f64);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if chroma == 0.0 {
        return None;
    }
    let h = if max == r {
        (((g - b) / chroma) % 6.0 + 6.0) % 6.0
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    Some(((h * 60.0) % 360.0 + 360.0) % 360.0)
}

/// Smallest circular difference between two hue angles in degrees, in `[0, 180]`.
pub fn hue_diff_deg(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// The per-patch outcome of the colour check. Every variant except [`PatchOutcome::Pass`] is a
/// FAIL and carries the evidence (expected/sampled colour + the failing metric) so the verdict can
/// show WHY a camera failed its colours — never just "colour bad".
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PatchOutcome {
    /// Sampled colour matched the reference within tolerance.
    Pass,
    /// A chromatic reference arrived with collapsed chroma — grayscale / desaturated camera.
    Grayscale {
        expected: Rgb,
        sampled: Rgb,
        sampled_chroma: f64,
    },
    /// A chromatic reference arrived with the wrong hue — channel swap / white-balance shift.
    HueShift {
        expected: Rgb,
        sampled: Rgb,
        hue_err_deg: f64,
    },
    /// A neutral (white/black/gray) reference arrived with an unexpected colour cast.
    NeutralTint {
        expected: Rgb,
        sampled: Rgb,
        chroma: f64,
    },
    /// No samplable pixels for this patch (fully behind a burn / off canvas). Fail-closed — an
    /// unproven colour is NOT a correct colour.
    Unsamplable { expected: Rgb },
}

impl PatchOutcome {
    /// True only for [`PatchOutcome::Pass`].
    pub fn is_pass(&self) -> bool {
        matches!(self, PatchOutcome::Pass)
    }

    /// True for a WRONG-COLOUR verdict (grayscale / hue / distance / neutral tint) — a real colour
    /// defect. This is what the gate counts. [`PatchOutcome::Unsamplable`] is NOT a wrong colour:
    /// it is a patch we could not read (fully behind a known burn), so it is SKIPPED, not charged —
    /// a real colour defect (grayscale / white-balance) is global and still fails on every VISIBLE
    /// patch, so skipping a burn-covered patch never lets a defect through.
    pub fn is_wrong(&self) -> bool {
        !matches!(self, PatchOutcome::Pass | PatchOutcome::Unsamplable { .. })
    }

    /// True when the patch had samplable pixels and was actually checked (Pass or a wrong-colour
    /// verdict) — i.e. NOT [`PatchOutcome::Unsamplable`].
    pub fn is_checked(&self) -> bool {
        !matches!(self, PatchOutcome::Unsamplable { .. })
    }
}

/// Decide one reference patch: known colour `expected` vs the sampled mean `sampled` (`None` when
/// no samplable pixel survived the burn-dodge). See the module-level decision description.
pub fn classify_patch(expected: Rgb, sampled: Option<Rgb>) -> PatchOutcome {
    let Some(sampled) = sampled else {
        return PatchOutcome::Unsamplable { expected };
    };
    if chroma(expected) >= CHROMATIC_EXPECTED_MIN {
        // A primary/secondary — it must arrive saturated and with the right hue (level/brightness is
        // NOT checked: the rig's optical capture is physically dim — see the module decision note).
        let sampled_chroma = chroma(sampled);
        if sampled_chroma < GRAYSCALE_CHROMA_MIN {
            // Collapsed toward gray — no meaningful hue to compare, report the collapse.
            return PatchOutcome::Grayscale {
                expected,
                sampled,
                sampled_chroma,
            };
        }
        // Both have chroma > 0 here, so both hues are defined.
        let (he, hs) = (
            hue_deg(expected).unwrap_or(0.0),
            hue_deg(sampled).unwrap_or(0.0),
        );
        let hue_err_deg = hue_diff_deg(he, hs);
        if hue_err_deg > HUE_TOL_DEG {
            return PatchOutcome::HueShift {
                expected,
                sampled,
                hue_err_deg,
            };
        }
        PatchOutcome::Pass
    } else {
        // A neutral (white/black/gray) — it must stay near-neutral (no colour cast). Level/brightness
        // is NOT checked (the dim optical capture would false-fail it — see the module decision note).
        let cast = chroma(sampled);
        if cast > NEUTRAL_CHROMA_MAX {
            return PatchOutcome::NeutralTint {
                expected,
                sampled,
                chroma: cast,
            };
        }
        PatchOutcome::Pass
    }
}

/// One camera's full colour verdict: the per-patch outcome for every reference patch, in
/// [`PATCH_COLOURS`] order.
#[derive(Clone, PartialEq, Debug)]
pub struct CameraColourVerdict {
    pub outcomes: Vec<PatchOutcome>,
}

impl CameraColourVerdict {
    /// Number of reference patches with a WRONG colour (grayscale / hue / distance / tint). This is
    /// the gate count; burn-covered ([`PatchOutcome::Unsamplable`]) patches are NOT included.
    pub fn wrong_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.is_wrong()).count()
    }

    /// Number of reference patches that had samplable pixels and were actually checked (Pass or
    /// wrong) — i.e. not burn-covered. The colour scale is only meaningfully verified when this is
    /// > 0.
    pub fn checked_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.is_checked()).count()
    }

    /// The camera PASSES colour only when at least one patch was actually CHECKED (fail-closed: a
    /// frame where every patch was burn-covered / unsamplable proves nothing) AND no checked patch
    /// has a wrong colour.
    pub fn is_pass(&self) -> bool {
        self.checked_count() > 0 && self.wrong_count() == 0
    }

    /// The wrong-colour patches paired with their table index, for human-readable reporting.
    pub fn failures(&self) -> impl Iterator<Item = (usize, &PatchOutcome)> {
        self.outcomes
            .iter()
            .enumerate()
            .filter(|(_, o)| o.is_wrong())
    }
}

/// Sample the mean colour of every reference patch from a packed **RGB8** (`r,g,b` per pixel)
/// frame buffer of size `w`×`h`, using the SAME central-gap column geometry the painter used
/// (derived from `qr_size` + `top_margin`), and DODGING the `exclusions` rectangles (the
/// bottom-anchored burns — strih bottom-left, cam1 bottom-center, stream bottom-right; with the
/// column now in the central gap these no longer overlap a patch, so the dodge is belt-and-braces).
/// For each patch from [`colour_scale_patches`], the mean is taken over the patch's pixels that are
/// NOT inside any exclusion rect and are within the buffer; `None` when no such pixel exists (patch
/// off canvas / fully excluded). Returns one entry per [`PATCH_COLOURS`] patch, in order.
pub fn sample_patch_means(
    rgb: &[u8],
    w: u32,
    h: u32,
    qr_size: u32,
    top_margin: u32,
    exclusions: &[Rect],
) -> Vec<Option<Rgb>> {
    colour_scale_patches(w, h, qr_size, top_margin)
        .into_iter()
        .map(|(rect, _)| sample_rect_mean(rgb, w, h, rect, exclusions))
        .collect()
}

/// Mean colour of one rectangle's in-bounds, non-excluded pixels (packed RGB8), or `None` if none.
fn sample_rect_mean(rgb: &[u8], w: u32, h: u32, rect: Rect, exclusions: &[Rect]) -> Option<Rgb> {
    let x_end = (rect.x + rect.w).min(w);
    let y_end = (rect.y + rect.h).min(h);
    let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in rect.y..y_end {
        for x in rect.x..x_end {
            if exclusions.iter().any(|e| e.contains(x, y)) {
                continue;
            }
            let i = (((y * w) + x) * 3) as usize;
            if i + 2 < rgb.len() {
                sr += rgb[i] as u64;
                sg += rgb[i + 1] as u64;
                sb += rgb[i + 2] as u64;
                n += 1;
            }
        }
    }
    if n == 0 {
        return None;
    }
    // Round-to-nearest mean per channel.
    let round = |s: u64| ((s + n / 2) / n) as u8;
    Some(Rgb::new(round(sr), round(sg), round(sb)))
}

/// Classify pre-sampled per-patch means into a [`CameraColourVerdict`]. `samples[i]` is the mean
/// for [`PATCH_COLOURS`]`[i]` (`None` ⇒ unsamplable). A shorter `samples` than the table treats the
/// missing trailing patches as unsamplable (fail-closed).
pub fn verify_samples(samples: &[Option<Rgb>]) -> CameraColourVerdict {
    let outcomes = PATCH_COLOURS
        .iter()
        .enumerate()
        .map(|(i, &expected)| {
            let sampled = samples.get(i).copied().flatten();
            classify_patch(expected, sampled)
        })
        .collect();
    CameraColourVerdict { outcomes }
}

/// Sample a packed RGB8 frame and classify it in one step — the entry point the probe glue calls
/// with the real recorded pixels + the dual-QR layout (`qr_size`, `top_margin`) + the per-node
/// burn-exclusion rects.
pub fn verify_rgb_frame(
    rgb: &[u8],
    w: u32,
    h: u32,
    qr_size: u32,
    top_margin: u32,
    exclusions: &[Rect],
) -> CameraColourVerdict {
    verify_samples(&sample_patch_means(
        rgb, w, h, qr_size, top_margin, exclusions,
    ))
}

/// Per-patch fail tallies across many sampled frames of ONE node's recording, used to reduce
/// transient single-frame compression noise to a stable per-node verdict. Serializable so the #208
/// per-box extract can carry it through the partial to the dev1 merge (#377).
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct NodeColourSummary {
    /// `patch_wrong_counts[i]` = number of sampled frames on which patch `i` had a WRONG colour.
    pub patch_wrong_counts: Vec<usize>,
    /// `patch_checked_counts[i]` = number of sampled frames on which patch `i` was actually checked
    /// (had samplable pixels — not burn-covered).
    pub patch_checked_counts: Vec<usize>,
    /// How many frames were sampled (the denominator for the majority rule).
    pub frames_sampled: usize,
}

impl NodeColourSummary {
    /// A patch is charged as a node-level colour FAIL when it had a WRONG colour on a STRICT
    /// MAJORITY of the sampled frames — a real colour defect (grayscale camera, hue cast) fails on
    /// essentially every frame, while a one-off compression glitch on a single frame does not flip
    /// the gate. This is the node's `colour_fail` that the verdict gate uses.
    pub fn fail_count(&self) -> usize {
        // The majority is over the frames on which the patch was actually CHECKED, not the total
        // sampled: a patch burn-covered on most frames but WRONG on every frame it WAS visible is a
        // real defect that must still be charged. Using `frames_sampled` as the denominator would
        // let such a patch escape (a vacuous-pass hole) once coverage becomes intermittent. Under
        // the current central-gap geometry the column is never burn-covered, so `checked ==
        // frames_sampled` for every patch and this matches the old behaviour exactly.
        self.patch_wrong_counts
            .iter()
            .zip(self.patch_checked_counts.iter())
            .filter(|(&wrong, &checked)| checked > 0 && wrong * 2 > checked)
            .count()
    }

    /// True when at least one patch was checked on at least one frame — i.e. the colour scale was
    /// actually readable somewhere. When false (every patch burn-covered / no frames), the colour
    /// is UNVERIFIABLE and the gate must treat it as a hard setup failure, not a silent pass.
    pub fn any_checked(&self) -> bool {
        self.patch_checked_counts.iter().any(|&c| c > 0)
    }

    /// The node PASSES colour only when frames were sampled, at least one patch was checkable, and
    /// no patch was wrong on a majority of frames (fail-closed: nothing checked proves nothing).
    pub fn is_pass(&self) -> bool {
        self.frames_sampled > 0 && self.any_checked() && self.fail_count() == 0
    }
}

/// Reduce per-frame colour verdicts of ONE node's recording to a [`NodeColourSummary`]. Each
/// verdict covers the same [`PATCH_COLOURS`] patches; the summary tallies, per patch, how many
/// frames found it WRONG and how many actually checked it. An empty input yields a fail-closed
/// summary (`frames_sampled == 0`).
pub fn summarize_node_colour(frames: &[CameraColourVerdict]) -> NodeColourSummary {
    let n_patches = PATCH_COLOURS.len();
    let mut patch_wrong_counts = vec![0usize; n_patches];
    let mut patch_checked_counts = vec![0usize; n_patches];
    for v in frames {
        for (i, o) in v.outcomes.iter().enumerate() {
            if i < n_patches {
                if o.is_wrong() {
                    patch_wrong_counts[i] += 1;
                }
                if o.is_checked() {
                    patch_checked_counts[i] += 1;
                }
            }
        }
    }
    NodeColourSummary {
        patch_wrong_counts,
        patch_checked_counts,
        frames_sampled: frames.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS_W: u32 = 1920;
    const CANVAS_H: u32 = 1080;
    const QR_SIZE: u32 = crate::colour_scale::DEFAULT_QR_SIZE;
    const TOP_MARGIN: u32 = crate::colour_scale::TOP_MARGIN_PX;

    /// Paint a packed RGB8 canvas with the reference colour scale (the central-gap column for the
    /// default dual-QR layout), transforming each patch's colour through `f` (identity = a
    /// perfectly-correct frame). The non-patch area is left black.
    fn paint_canvas(w: u32, h: u32, f: impl Fn(Rgb) -> Rgb) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 3) as usize];
        for (rect, rgb) in colour_scale_patches(w, h, QR_SIZE, TOP_MARGIN) {
            let c = f(rgb);
            for y in rect.y..(rect.y + rect.h).min(h) {
                for x in rect.x..(rect.x + rect.w).min(w) {
                    let i = (((y * w) + x) * 3) as usize;
                    buf[i] = c.r;
                    buf[i + 1] = c.g;
                    buf[i + 2] = c.b;
                }
            }
        }
        buf
    }

    /// sRGB luma — the grayscale collapse a mono/desaturated camera produces.
    fn to_gray(c: Rgb) -> Rgb {
        let y = (0.299 * c.r as f64 + 0.587 * c.g as f64 + 0.114 * c.b as f64).round() as u8;
        Rgb::new(y, y, y)
    }

    /// Rotate channels R←G←B←R — a hue shift that keeps full chroma (so it is NOT grayscale).
    fn rotate_channels(c: Rgb) -> Rgb {
        Rgb::new(c.b, c.r, c.g)
    }

    // ---- hue / chroma helper sanity (not tautological — concrete known values) ----

    #[test]
    fn hue_of_the_primaries_is_canonical() {
        assert_eq!(hue_deg(Rgb::new(255, 0, 0)), Some(0.0), "red");
        assert_eq!(hue_deg(Rgb::new(0, 255, 0)), Some(120.0), "green");
        assert_eq!(hue_deg(Rgb::new(0, 0, 255)), Some(240.0), "blue");
        assert_eq!(hue_deg(Rgb::new(0, 255, 255)), Some(180.0), "cyan");
        assert_eq!(hue_deg(Rgb::new(255, 0, 255)), Some(300.0), "magenta");
        assert_eq!(hue_deg(Rgb::new(255, 255, 0)), Some(60.0), "yellow");
        assert_eq!(hue_deg(Rgb::new(128, 128, 128)), None, "gray has no hue");
    }

    #[test]
    fn hue_diff_is_circular() {
        assert_eq!(hue_diff_deg(350.0, 10.0), 20.0, "wraps across 0/360");
        assert_eq!(hue_diff_deg(0.0, 240.0), 120.0, "shortest arc");
    }

    #[test]
    fn chroma_is_concrete() {
        assert_eq!(chroma(Rgb::new(255, 0, 0)), 255.0);
        assert_eq!(chroma(Rgb::new(128, 128, 128)), 0.0);
        assert_eq!(chroma(Rgb::new(64, 0, 0)), 64.0);
    }

    // ---- the sampler (geometry + burn-dodge) ----

    #[test]
    fn sampler_reads_each_patch_mean_exactly_on_a_clean_frame() {
        let buf = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        let samples = sample_patch_means(&buf, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert_eq!(samples.len(), PATCH_COLOURS.len());
        for (i, s) in samples.iter().enumerate() {
            assert_eq!(*s, Some(PATCH_COLOURS[i]), "patch {i} mean is exact");
        }
    }

    #[test]
    fn sampler_dodges_a_burn_column_painted_with_a_wrong_colour() {
        // Paint a correct frame, then stamp a WRONG colour (pure red) over a vertical strip that
        // overlaps several patches — exactly what a burn column does. With that strip excluded, the
        // sampled means must stay the correct patch colours (the burn is dodged, not averaged in).
        let mut buf = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        // A strip that overlaps the central colour column (x in [840,1080), y in [24,724)) only
        // PARTIALLY in width, so each crossed patch keeps samplable pixels on either side.
        let burn = Rect {
            x: 900,
            y: 100,
            w: 120,
            h: 300,
        };
        for y in burn.y..burn.y + burn.h {
            for x in burn.x..burn.x + burn.w {
                let i = (((y * CANVAS_W) + x) * 3) as usize;
                buf[i] = 255;
                buf[i + 1] = 0;
                buf[i + 2] = 0;
            }
        }
        // Without dodging, the patches under the strip would be polluted toward red.
        let polluted = sample_patch_means(&buf, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        let dodged = sample_patch_means(&buf, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[burn]);
        let mut any_differs = false;
        for (i, (p, d)) in polluted.iter().zip(&dodged).enumerate() {
            assert_eq!(*d, Some(PATCH_COLOURS[i]), "dodged patch {i} stays correct");
            if p != d {
                any_differs = true;
            }
        }
        assert!(
            any_differs,
            "the burn strip must actually pollute ≥1 patch when NOT dodged"
        );
    }

    #[test]
    fn classify_patch_outcomes_are_exhaustive_and_correct() {
        // Direct lock on each decision branch (independent of the synthetic-frame helpers), so a
        // future tweak to a tolerance or branch order is caught.
        let red = Rgb::new(255, 0, 0);
        let gray = Rgb::new(128, 128, 128);

        // Pass — exact, and within tolerance.
        assert!(classify_patch(red, Some(red)).is_pass());
        assert!(
            classify_patch(red, Some(Rgb::new(235, 18, 14))).is_pass(),
            "near-correct red"
        );
        assert!(
            classify_patch(gray, Some(Rgb::new(132, 124, 130))).is_pass(),
            "near-correct gray"
        );

        // Grayscale — a chromatic patch with collapsed chroma.
        assert!(matches!(
            classify_patch(red, Some(Rgb::new(90, 90, 90))),
            PatchOutcome::Grayscale { .. }
        ));
        // HueShift — chromatic, full chroma, wrong hue (red sampled as green).
        assert!(matches!(
            classify_patch(red, Some(Rgb::new(0, 255, 0))),
            PatchOutcome::HueShift { .. }
        ));
        // Level is NOT checked: a dim-but-correct chromatic (right hue, ample chroma) PASSES — this
        // is the rig physics (the optical capture is ~7× dim). A pure level check would false-fail it.
        assert!(
            classify_patch(red, Some(Rgb::new(140, 0, 0))).is_pass(),
            "dim red (correct hue + chroma) passes — level not gated"
        );
        // NeutralTint — a neutral patch with a colour cast.
        assert!(matches!(
            classify_patch(gray, Some(Rgb::new(190, 110, 110))),
            PatchOutcome::NeutralTint { .. }
        ));
        // A dim-but-neutral gray (no cast) also PASSES — level not gated.
        assert!(
            classify_patch(gray, Some(Rgb::new(20, 22, 18))).is_pass(),
            "dim neutral (no cast) passes — level not gated"
        );
        // Unsamplable — no sample at all.
        assert!(matches!(
            classify_patch(red, None),
            PatchOutcome::Unsamplable { .. }
        ));
    }

    #[test]
    fn tolerance_boundaries_are_exact() {
        // Pin each threshold at its EXACT value and one step over, so the relational operator and
        // the constant cannot drift (a `>`↔`>=` or value tweak flips one of these). Every boundary
        // is reachable with integer sRGB, so the comparison direction is fully constrained.
        let red = Rgb::new(255, 0, 0);
        let gray = Rgb::new(128, 128, 128);

        // CHROMATIC_EXPECTED_MIN = 64: expected chroma 64 is CHROMATIC (≥), 63 is NEUTRAL.
        assert!(
            classify_patch(Rgb::new(64, 0, 0), Some(Rgb::new(64, 0, 0))).is_pass(),
            "expected chroma 64 ⇒ chromatic ⇒ identical sample passes"
        );
        assert!(
            matches!(
                classify_patch(Rgb::new(63, 0, 0), Some(Rgb::new(63, 0, 0))),
                PatchOutcome::NeutralTint { .. }
            ),
            "expected chroma 63 ⇒ neutral ⇒ its own chroma 63 > NEUTRAL_CHROMA_MAX ⇒ tint"
        );

        // GRAYSCALE_CHROMA_MIN = 40: sampled chroma exactly 40 is NOT grayscale; 39 IS.
        assert!(
            !matches!(
                classify_patch(red, Some(Rgb::new(40, 0, 0))),
                PatchOutcome::Grayscale { .. }
            ),
            "sampled chroma 40 is at the floor — NOT grayscale"
        );
        assert!(
            matches!(
                classify_patch(red, Some(Rgb::new(39, 0, 0))),
                PatchOutcome::Grayscale { .. }
            ),
            "sampled chroma 39 is below the floor — grayscale"
        );

        // NEUTRAL_CHROMA_MAX = 48: a neutral patch with sampled chroma exactly 48 is NOT a tint; 49 is.
        assert!(
            !matches!(
                classify_patch(gray, Some(Rgb::new(176, 128, 128))),
                PatchOutcome::NeutralTint { .. }
            ),
            "neutral sample chroma 48 is at the cap — NOT a tint"
        );
        assert!(
            matches!(
                classify_patch(gray, Some(Rgb::new(177, 128, 128))),
                PatchOutcome::NeutralTint { .. }
            ),
            "neutral sample chroma 49 exceeds the cap — tint"
        );

        // Level is NOT a gate: a chromatic sample far in sRGB distance but with the right hue +
        // ample chroma PASSES (the rig is physically dim). (159,0,0) and an even darker (40,0,0) red
        // both pass — only chroma-collapse / hue-shift fail a chromatic.
        assert!(
            classify_patch(red, Some(Rgb::new(159, 0, 0))).is_pass(),
            "dim red (distance 96, right hue) passes — level not gated"
        );

        // HUE_TOL_DEG = 30: (254,127,0) is hue exactly 30° from red ⇒ NOT a hue fail (and with no
        // level gate it now PASSES); (254,128,0) is just over 30° ⇒ HueShift.
        assert_eq!(
            hue_deg(Rgb::new(254, 127, 0)),
            Some(30.0),
            "exact 30° hue sample"
        );
        assert!(
            classify_patch(red, Some(Rgb::new(254, 127, 0))).is_pass(),
            "hue error exactly 30° is at the tolerance — passes (no level gate)"
        );
        assert!(
            matches!(
                classify_patch(red, Some(Rgb::new(254, 128, 0))),
                PatchOutcome::HueShift { .. }
            ),
            "hue error just over 30° is a HueShift"
        );
    }

    #[test]
    fn a_patch_fully_behind_a_burn_is_unsamplable() {
        let buf = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        // Exclude the entire first patch region.
        let (rect0, _) = colour_scale_patches(CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN)[0];
        let samples = sample_patch_means(&buf, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[rect0]);
        assert_eq!(samples[0], None, "patch 0 fully excluded ⇒ unsamplable");
    }

    // ---- THE REGRESSION LOCK: the gate FAILS on grayscale + hue-shift, PASSES on correct ----

    #[test]
    fn colour_gate_passes_on_correct_fails_on_grayscale_and_hue_shift() {
        // Correct frame ⇒ every patch passes ⇒ camera PASSES.
        let correct = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        let v_ok = verify_rgb_frame(&correct, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert!(
            v_ok.is_pass(),
            "a correct colour scale must PASS (fails: {:?})",
            v_ok.failures().collect::<Vec<_>>()
        );
        assert_eq!(v_ok.wrong_count(), 0);
        assert_eq!(
            v_ok.checked_count(),
            PATCH_COLOURS.len(),
            "all patches checked"
        );

        // Grayscale camera ⇒ every CHROMATIC patch collapses ⇒ camera FAILS, and the failures are
        // Grayscale (not some other category).
        let gray = paint_canvas(CANVAS_W, CANVAS_H, to_gray);
        let v_gray = verify_rgb_frame(&gray, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert!(
            !v_gray.is_pass(),
            "a grayscale camera must FAIL the colour gate"
        );
        let chromatic = PATCH_COLOURS
            .iter()
            .filter(|c| chroma(**c) >= CHROMATIC_EXPECTED_MIN)
            .count();
        assert!(chromatic >= 6, "table has the 6 primaries/secondaries");
        let grayscale_fails = v_gray
            .outcomes
            .iter()
            .filter(|o| matches!(o, PatchOutcome::Grayscale { .. }))
            .count();
        assert_eq!(
            grayscale_fails, chromatic,
            "every chromatic patch fails as Grayscale on a mono camera"
        );

        // Hue-shifted camera ⇒ chromatic patches keep chroma but land on the wrong hue ⇒ FAIL, and
        // at least one failure is specifically a HueShift (not grayscale, not distance-only).
        let hue = paint_canvas(CANVAS_W, CANVAS_H, rotate_channels);
        let v_hue = verify_rgb_frame(&hue, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert!(
            !v_hue.is_pass(),
            "a hue-shifted camera must FAIL the colour gate"
        );
        let hue_fails = v_hue
            .outcomes
            .iter()
            .filter(|o| matches!(o, PatchOutcome::HueShift { .. }))
            .count();
        assert!(
            hue_fails >= 1,
            "≥1 chromatic patch fails specifically as HueShift on a channel-rotated camera: {:?}",
            v_hue.failures().collect::<Vec<_>>()
        );
    }

    /// THE RIG FIXTURE LOCK (#364) — the per-patch values below are the REAL means sampled from a
    /// genuine cam1 capture of the cam2 colour scale through the full chain (1920×1080 OBS program,
    /// the production [`sample_patch_means`] over the central-gap column). They are dim + carry the
    /// rig's natural blue/cyan optical cast (60 Hz monitor + 1/1000 s shutter). The gate MUST PASS
    /// this real camera, and MUST FAIL the same capture once a real colour fault (grayscale / dead
    /// red channel) is applied. This is what proves the tolerances fit the real signal and locks
    /// them: a future tweak that false-fails the real dim camera, or lets a fault through, breaks
    /// here. Captured 2026-06-30; if the rig camera/monitor changes materially, re-capture + update.
    #[test]
    fn real_rig_dim_capture_passes_while_grayscale_and_dead_channel_fail() {
        // PATCH_COLOURS order: white, black, R, G, B, C, M, Y, g0, g64, g128, g192, g255.
        let real: [Rgb; 13] = [
            Rgb::new(106, 129, 135), // white  — dim, blue cast (chroma 29)
            Rgb::new(33, 34, 36),    // black
            Rgb::new(95, 45, 46),    // red    — hue err 1.2°, chroma 50
            Rgb::new(62, 163, 54),   // green  — hue err 4.4°, chroma 109
            Rgb::new(29, 41, 132),   // blue   — hue err 7.0°, chroma 103
            Rgb::new(53, 162, 155),  // cyan   — hue err 3.9°, chroma 109
            Rgb::new(113, 37, 150),  // magenta— hue err 19.6°, chroma 113
            Rgb::new(134, 166, 61),  // yellow — hue err 18.3°, chroma 105
            Rgb::new(37, 46, 42),    // g0
            Rgb::new(38, 53, 55),    // g64
            Rgb::new(64, 89, 94),    // g128   — cast chroma 30
            Rgb::new(96, 122, 131),  // g192   — cast chroma 35
            Rgb::new(122, 150, 160), // g255   — cast chroma 38 (max neutral cast)
        ];
        assert_eq!(
            real.len(),
            PATCH_COLOURS.len(),
            "one sample per reference patch"
        );

        // The REAL dim+cast camera PASSES every patch (correct hue + chroma, no level gate).
        let samples: Vec<Option<Rgb>> = real.iter().map(|&c| Some(c)).collect();
        let v = verify_samples(&samples);
        assert!(
            v.is_pass(),
            "the real rig camera must PASS (wrong patches: {:?})",
            v.failures().collect::<Vec<_>>()
        );
        assert_eq!(v.wrong_count(), 0, "no patch wrong on the real dim camera");

        // Grayscale fault (luma collapse) ON TOP of the same dim capture ⇒ every chromatic patch
        // collapses ⇒ FAIL.
        let gray: Vec<Option<Rgb>> = real.iter().map(|&c| Some(to_gray(c))).collect();
        let v_gray = verify_samples(&gray);
        assert!(
            !v_gray.is_pass(),
            "grayscale fault must FAIL the real-derived frame"
        );
        assert!(
            v_gray.wrong_count() >= 6,
            "all 6 chromatic patches collapse: {}",
            v_gray.wrong_count()
        );

        // Dead RED channel (r ⇒ 0) ON TOP of the same dim capture ⇒ neutrals gain a huge cyan tint
        // and the red patch hue-shifts ⇒ FAIL.
        let dead_red: Vec<Option<Rgb>> =
            real.iter().map(|&c| Some(Rgb::new(0, c.g, c.b))).collect();
        let v_dead = verify_samples(&dead_red);
        assert!(!v_dead.is_pass(), "a dead red channel must FAIL");
        assert!(
            v_dead.wrong_count() >= 4,
            "neutrals tint + red patch hue-shifts: {}",
            v_dead.wrong_count()
        );
    }

    #[test]
    fn realistic_compression_noise_still_passes() {
        // A correct camera through H.264/NDI: small per-channel jitter + a slight overall lift.
        // The gate must NOT false-fail on this (tolerances are strict, not brittle).
        let noisy = paint_canvas(CANVAS_W, CANVAS_H, |c| {
            let j = |v: u8, d: i32| (v as i32 + d).clamp(0, 255) as u8;
            Rgb::new(j(c.r, 12), j(c.g, -9), j(c.b, 7))
        });
        let v = verify_rgb_frame(&noisy, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert!(
            v.is_pass(),
            "realistic compression jitter must still PASS: {:?}",
            v.failures().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_neutral_patch_with_a_colour_cast_fails() {
        // Tint ONLY the neutral (gray/white/black) patches red; leave the chromatic ones correct.
        let buf = paint_canvas(CANVAS_W, CANVAS_H, |c| {
            if chroma(c) < CHROMATIC_EXPECTED_MIN {
                Rgb::new((c.r as u32 + 90).min(255) as u8, c.g, c.b)
            } else {
                c
            }
        });
        let v = verify_rgb_frame(&buf, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &[]);
        assert!(!v.is_pass(), "a neutral patch with a colour cast must FAIL");
        assert!(
            v.outcomes
                .iter()
                .any(|o| matches!(o, PatchOutcome::NeutralTint { .. })),
            "the cast neutral patch fails as NeutralTint: {:?}",
            v.failures().collect::<Vec<_>>()
        );
    }

    #[test]
    fn all_unsamplable_is_fail_closed_not_a_pass() {
        // No samples at all ⇒ every patch unsamplable ⇒ nothing checked ⇒ NOT a pass (fail-closed),
        // but unsamplable is NOT counted as a WRONG colour (it is skipped, not charged).
        let v = verify_samples(&[]);
        assert_eq!(v.checked_count(), 0, "nothing was checkable");
        assert_eq!(v.wrong_count(), 0, "unsamplable is not a wrong colour");
        assert!(!v.is_pass(), "nothing checked proves nothing ⇒ fail-closed");
        assert!(v
            .outcomes
            .iter()
            .all(|o| matches!(o, PatchOutcome::Unsamplable { .. })));
    }

    #[test]
    fn burn_covered_patches_are_skipped_but_visible_defects_still_fail() {
        // Cover the first two patches entirely (as a corner burn would) AND collapse every
        // chromatic patch to gray. The covered patches are SKIPPED (not charged), but the visible
        // grayscale chromatic patches still FAIL — a global defect is caught despite the burns.
        let gray = paint_canvas(CANVAS_W, CANVAS_H, to_gray);
        let patches = colour_scale_patches(CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN);
        let cover = vec![patches[0].0, patches[1].0];
        let v = verify_rgb_frame(&gray, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &cover);
        assert!(
            matches!(v.outcomes[0], PatchOutcome::Unsamplable { .. }),
            "patch 0 skipped"
        );
        assert!(
            matches!(v.outcomes[1], PatchOutcome::Unsamplable { .. }),
            "patch 1 skipped"
        );
        assert!(
            v.checked_count() < PATCH_COLOURS.len(),
            "two patches were not checked"
        );
        assert!(
            !v.is_pass(),
            "grayscale still FAILS on the visible chromatic patches"
        );

        // And the converse: burn-covered patches on an otherwise-CORRECT frame still PASS (the
        // skipped patches don't fail the camera).
        let correct = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        let v_ok = verify_rgb_frame(&correct, CANVAS_W, CANVAS_H, QR_SIZE, TOP_MARGIN, &cover);
        assert!(
            v_ok.is_pass(),
            "burn-covered patches must not fail a correct camera"
        );
        assert_eq!(v_ok.checked_count(), PATCH_COLOURS.len() - 2);
    }

    // ---- per-node aggregation across frames (majority rule) ----

    #[test]
    fn node_summary_charges_only_majority_patch_failures() {
        let correct = verify_rgb_frame(
            &paint_canvas(CANVAS_W, CANVAS_H, |c| c),
            CANVAS_W,
            CANVAS_H,
            QR_SIZE,
            TOP_MARGIN,
            &[],
        );
        let gray = verify_rgb_frame(
            &paint_canvas(CANVAS_W, CANVAS_H, to_gray),
            CANVAS_W,
            CANVAS_H,
            QR_SIZE,
            TOP_MARGIN,
            &[],
        );

        // 5 clean frames + 4 grayscale: grayscale is a MINORITY (4 of 9) ⇒ no patch charged.
        let mut frames = vec![correct.clone(); 5];
        frames.extend(vec![gray.clone(); 4]);
        let minority = summarize_node_colour(&frames);
        assert_eq!(minority.frames_sampled, 9);
        assert!(minority.any_checked());
        assert_eq!(
            minority.fail_count(),
            0,
            "a minority of bad frames must not flip the gate"
        );
        assert!(minority.is_pass());

        // 4 clean + 5 grayscale: grayscale is the MAJORITY ⇒ every chromatic patch charged.
        let mut frames = vec![correct; 4];
        frames.extend(vec![gray; 5]);
        let majority = summarize_node_colour(&frames);
        let chromatic = PATCH_COLOURS
            .iter()
            .filter(|c| chroma(**c) >= CHROMATIC_EXPECTED_MIN)
            .count();
        assert_eq!(
            majority.fail_count(),
            chromatic,
            "a majority grayscale node FAILS"
        );
        assert!(!majority.is_pass());

        // STRICT majority boundary: exactly HALF (2 bad of 4) is NOT a majority ⇒ no patch charged.
        // Locks the `wrong*2 > frames` comparison (a `>`→`>=` drift would charge the tie).
        let mut frames = vec![correct_for_boundary(); 2];
        frames.extend(vec![gray_for_boundary(); 2]);
        let tie = summarize_node_colour(&frames);
        assert_eq!(tie.frames_sampled, 4);
        assert_eq!(
            tie.fail_count(),
            0,
            "exactly half the frames bad is NOT a strict majority — no patch charged"
        );
        assert!(
            tie.is_pass(),
            "a 2-of-4 tie must PASS (strict majority required)"
        );
    }

    fn correct_for_boundary() -> CameraColourVerdict {
        verify_rgb_frame(
            &paint_canvas(CANVAS_W, CANVAS_H, |c| c),
            CANVAS_W,
            CANVAS_H,
            QR_SIZE,
            TOP_MARGIN,
            &[],
        )
    }
    fn gray_for_boundary() -> CameraColourVerdict {
        verify_rgb_frame(
            &paint_canvas(CANVAS_W, CANVAS_H, to_gray),
            CANVAS_W,
            CANVAS_H,
            QR_SIZE,
            TOP_MARGIN,
            &[],
        )
    }

    #[test]
    fn patch_wrong_on_all_checked_but_burn_covered_majority_is_still_charged() {
        // Latent fail-open: a patch burn-covered on most frames (checked on only a few) but WRONG on
        // EVERY frame it WAS visible is a real colour defect. With the majority taken over total
        // sampled frames (the old `wrong * 2 > frames_sampled`), such a patch escapes the gate
        // (2*2=4 is not > 9) — a vacuous-pass hole. The majority must be over the CHECKED count.
        let s = NodeColourSummary {
            patch_wrong_counts: vec![2],   // wrong on both frames it was checked
            patch_checked_counts: vec![2], // checked on only 2 of 9 frames (burn-covered on 7)
            frames_sampled: 9,
        };
        assert_eq!(
            s.fail_count(),
            1,
            "a patch wrong on all its CHECKED frames must be charged even when burn-covered on the \
             majority (denominator is per-patch checked, not total sampled)"
        );
    }

    #[test]
    fn empty_node_summary_is_fail_closed() {
        let s = summarize_node_colour(&[]);
        assert_eq!(s.frames_sampled, 0);
        assert!(!s.any_checked());
        assert!(!s.is_pass(), "zero sampled frames proves nothing");
    }

    #[test]
    fn node_with_no_checkable_patch_is_unverifiable_fail_closed() {
        // Frames where every patch is burn-covered ⇒ checked nowhere ⇒ unverifiable ⇒ NOT a pass.
        let blank = CameraColourVerdict {
            outcomes: PATCH_COLOURS
                .iter()
                .map(|&expected| PatchOutcome::Unsamplable { expected })
                .collect(),
        };
        let s = summarize_node_colour(&[blank.clone(), blank]);
        assert_eq!(s.frames_sampled, 2);
        assert!(!s.any_checked(), "no patch was ever checkable");
        assert_eq!(s.fail_count(), 0, "unsamplable patches are not wrong");
        assert!(!s.is_pass(), "colour unverifiable ⇒ fail-closed");
    }
}
