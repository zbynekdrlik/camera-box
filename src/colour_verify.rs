//! #364 — per-camera COLOUR-correctness gate (pure decision + sampler).
//!
//! The zero-loss verdict ([`crate::probe`] / `bin/recording-verdict`) proves frame DELIVERY
//! only — it never looks at COLOUR, so a camera that goes grayscale / white-balance-shifted /
//! hue-rotated delivers every frame and still PASSES. Live events kept failing on exactly that.
//! This module is the gate that FAILS on it.
//!
//! ## How it ties to the painter (one source of truth)
//!
//! The #367 painter ([`crate::colour_scale`]) blits a row of KNOWN-sRGB reference patches along
//! the bottom band of the cam2 monitor — the same content rides cam1→strih→stream and lands in
//! every node's recording. This module iterates the SAME [`colour_scale_patches`] geometry and
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
//! ## The decision (strict, REAL signal — never weakenable)
//!
//! Per reference patch, expected colour `E` vs sampled mean `S`:
//! - **CHROMATIC** expected (a primary/secondary — chroma ≥ [`CHROMATIC_EXPECTED_MIN`]):
//!   - sampled chroma below [`GRAYSCALE_CHROMA_MIN`] ⇒ **Grayscale** (the colour collapsed toward
//!     gray — desaturated / mono camera). Checked FIRST: a grayscale patch has no meaningful hue.
//!   - else hue error above [`HUE_TOL_DEG`] ⇒ **HueShift** (channel swap / white-balance cast).
//!   - else sRGB distance above [`SRGB_DIST_TOL`] ⇒ **OutOfTolerance** (gross level error).
//! - **NEUTRAL** expected (white/black/gray ramp — chroma < [`CHROMATIC_EXPECTED_MIN`]):
//!   - sampled chroma above [`NEUTRAL_CHROMA_MAX`] ⇒ **NeutralTint** (a cast where there is none).
//!   - else sRGB distance above [`SRGB_DIST_TOL`] ⇒ **OutOfTolerance**.
//! - a patch with NO samplable pixels (fully behind a burn / off canvas) ⇒ **Unsamplable** — a
//!   FAIL, never a silent pass (fail-closed: an unproven colour is not a correct colour).
//!
//! A camera FAILS the gate if ANY patch fails. The tolerances are calibrated so a CORRECT colour
//! through real H.264/NDI transport (compression, gamma) passes comfortably while a grayscale /
//! hue-shifted camera fails by a wide margin — hue is exposure/compression robust, and chroma
//! collapse is unmistakable. They are a STRICT bar; the recorded-fixture proof locks them. They
//! are NEVER to be loosened to force a pass (strict-test mandate).

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

/// Max sRGB Euclidean distance (0..≈441) between a patch's known and sampled colour. Backstop for
/// gross level errors the chroma/hue checks don't catch (e.g. a primary delivered far too dark).
/// Generous enough that real transport passes; a correct colour is well inside it.
pub const SRGB_DIST_TOL: f64 = 96.0;

/// A NEUTRAL (white/black/gray) patch must stay near-neutral — sampled chroma above this is an
/// unexpected colour cast (e.g. a white-balance tint), which FAILS. Mirrors the chroma headroom a
/// correct neutral keeps after compression (a few units) versus a real cast (tens).
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

/// Euclidean distance between two sRGB colours in `[0, ≈441]`.
pub fn srgb_dist(a: Rgb, b: Rgb) -> f64 {
    let dr = a.r as f64 - b.r as f64;
    let dg = a.g as f64 - b.g as f64;
    let db = a.b as f64 - b.b as f64;
    (dr * dr + dg * dg + db * db).sqrt()
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
    /// The sampled colour was too far from the reference in sRGB space (gross level error).
    OutOfTolerance {
        expected: Rgb,
        sampled: Rgb,
        dist: f64,
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
    /// True only for [`PatchOutcome::Pass`]; every other variant is a FAIL.
    pub fn is_pass(&self) -> bool {
        matches!(self, PatchOutcome::Pass)
    }
}

/// Decide one reference patch: known colour `expected` vs the sampled mean `sampled` (`None` when
/// no samplable pixel survived the burn-dodge). See the module-level decision description.
pub fn classify_patch(expected: Rgb, sampled: Option<Rgb>) -> PatchOutcome {
    // #364 RED STUB — gate not implemented yet. Treats every samplable patch as a pass, so the
    // grayscale / hue-shift regression tests FAIL (RED). The GREEN commit replaces this body with
    // the real decision.
    match sampled {
        None => PatchOutcome::Unsamplable { expected },
        Some(_) => PatchOutcome::Pass,
    }
}

/// One camera's full colour verdict: the per-patch outcome for every reference patch, in
/// [`PATCH_COLOURS`] order.
#[derive(Clone, PartialEq, Debug)]
pub struct CameraColourVerdict {
    pub outcomes: Vec<PatchOutcome>,
}

impl CameraColourVerdict {
    /// Number of reference patches that FAILED the colour check.
    pub fn fail_count(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.is_pass()).count()
    }

    /// The camera PASSES the colour gate only when EVERY reference patch passed AND there was at
    /// least one patch to check (an empty verdict is fail-closed — nothing was proven).
    pub fn is_pass(&self) -> bool {
        !self.outcomes.is_empty() && self.fail_count() == 0
    }

    /// The first `n` failing patches paired with their table index, for human-readable reporting.
    pub fn failures(&self) -> impl Iterator<Item = (usize, &PatchOutcome)> {
        self.outcomes
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.is_pass())
    }
}

/// Sample the mean colour of every reference patch from a packed **RGB8** (`r,g,b` per pixel)
/// frame buffer of size `w`×`h`, DODGING the `exclusions` rectangles (the burn columns that sit on
/// the colour band — strih bottom-left, cam1 bottom-center, stream bottom-right). For each patch
/// from [`colour_scale_patches`], the mean is taken over the patch's pixels that are NOT inside any
/// exclusion rect and are within the buffer; `None` when no such pixel exists (patch fully behind a
/// burn / off canvas). Returns one entry per [`PATCH_COLOURS`] patch, in order.
pub fn sample_patch_means(rgb: &[u8], w: u32, h: u32, exclusions: &[Rect]) -> Vec<Option<Rgb>> {
    colour_scale_patches(w, h)
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
/// with the real recorded pixels + the per-node burn-exclusion rects.
pub fn verify_rgb_frame(rgb: &[u8], w: u32, h: u32, exclusions: &[Rect]) -> CameraColourVerdict {
    verify_samples(&sample_patch_means(rgb, w, h, exclusions))
}

/// Per-patch fail tallies across many sampled frames of ONE node's recording, used to reduce
/// transient single-frame compression noise to a stable per-node verdict.
#[derive(Clone, PartialEq, Debug)]
pub struct NodeColourSummary {
    /// `patch_fail_counts[i]` = number of sampled frames on which patch `i` failed.
    pub patch_fail_counts: Vec<usize>,
    /// How many frames were sampled (the denominator for the majority rule).
    pub frames_sampled: usize,
}

impl NodeColourSummary {
    /// A patch is charged as a node-level colour FAIL when it failed on a STRICT MAJORITY of the
    /// sampled frames — a real colour defect (grayscale camera, hue cast) fails on essentially
    /// every frame, while a one-off compression glitch on a single frame does not flip the gate.
    pub fn fail_count(&self) -> usize {
        self.patch_fail_counts
            .iter()
            .filter(|&&c| c * 2 > self.frames_sampled)
            .count()
    }

    /// The node PASSES colour only when no patch failed on a majority of frames AND at least one
    /// frame was sampled (fail-closed: zero sampled frames proves nothing).
    pub fn is_pass(&self) -> bool {
        self.frames_sampled > 0 && self.fail_count() == 0
    }
}

/// Reduce per-frame colour verdicts of ONE node's recording to a [`NodeColourSummary`]. Each
/// verdict must cover the same [`PATCH_COLOURS`] patches; the summary tallies, per patch, how many
/// frames failed it. An empty input yields a fail-closed summary (`frames_sampled == 0`).
pub fn summarize_node_colour(frames: &[CameraColourVerdict]) -> NodeColourSummary {
    let n_patches = PATCH_COLOURS.len();
    let mut patch_fail_counts = vec![0usize; n_patches];
    for v in frames {
        for (i, o) in v.outcomes.iter().enumerate() {
            if i < n_patches && !o.is_pass() {
                patch_fail_counts[i] += 1;
            }
        }
    }
    NodeColourSummary {
        patch_fail_counts,
        frames_sampled: frames.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS_W: u32 = 1920;
    const CANVAS_H: u32 = 1080;

    /// Paint a packed RGB8 canvas with the reference colour scale, transforming each patch's
    /// colour through `f` (identity = a perfectly-correct frame). The non-band area is left black.
    fn paint_canvas(w: u32, h: u32, f: impl Fn(Rgb) -> Rgb) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 3) as usize];
        for (rect, rgb) in colour_scale_patches(w, h) {
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
    fn chroma_and_distance_are_concrete() {
        assert_eq!(chroma(Rgb::new(255, 0, 0)), 255.0);
        assert_eq!(chroma(Rgb::new(128, 128, 128)), 0.0);
        assert_eq!(srgb_dist(Rgb::new(0, 0, 0), Rgb::new(0, 0, 0)), 0.0);
        assert!((srgb_dist(Rgb::new(255, 0, 0), Rgb::new(0, 0, 0)) - 255.0).abs() < 1e-9);
    }

    // ---- the sampler (geometry + burn-dodge) ----

    #[test]
    fn sampler_reads_each_patch_mean_exactly_on_a_clean_frame() {
        let buf = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        let samples = sample_patch_means(&buf, CANVAS_W, CANVAS_H, &[]);
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
        let burn = Rect {
            x: 900,
            y: CANVAS_H - 96,
            w: 120,
            h: 96,
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
        let polluted = sample_patch_means(&buf, CANVAS_W, CANVAS_H, &[]);
        let dodged = sample_patch_means(&buf, CANVAS_W, CANVAS_H, &[burn]);
        let mut any_differs = false;
        for (i, (p, d)) in polluted.iter().zip(&dodged).enumerate() {
            assert_eq!(*d, Some(PATCH_COLOURS[i]), "dodged patch {i} stays correct");
            if p != d {
                any_differs = true;
            }
        }
        assert!(any_differs, "the burn strip must actually pollute ≥1 patch when NOT dodged");
    }

    #[test]
    fn a_patch_fully_behind_a_burn_is_unsamplable() {
        let buf = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        // Exclude the entire first patch region.
        let (rect0, _) = colour_scale_patches(CANVAS_W, CANVAS_H)[0];
        let samples = sample_patch_means(&buf, CANVAS_W, CANVAS_H, &[rect0]);
        assert_eq!(samples[0], None, "patch 0 fully excluded ⇒ unsamplable");
    }

    // ---- THE REGRESSION LOCK: the gate FAILS on grayscale + hue-shift, PASSES on correct ----

    #[test]
    fn colour_gate_passes_on_correct_fails_on_grayscale_and_hue_shift() {
        // Correct frame ⇒ every patch passes ⇒ camera PASSES.
        let correct = paint_canvas(CANVAS_W, CANVAS_H, |c| c);
        let v_ok = verify_rgb_frame(&correct, CANVAS_W, CANVAS_H, &[]);
        assert!(
            v_ok.is_pass(),
            "a correct colour scale must PASS (fails: {:?})",
            v_ok.failures().collect::<Vec<_>>()
        );
        assert_eq!(v_ok.fail_count(), 0);

        // Grayscale camera ⇒ every CHROMATIC patch collapses ⇒ camera FAILS, and the failures are
        // Grayscale (not some other category).
        let gray = paint_canvas(CANVAS_W, CANVAS_H, to_gray);
        let v_gray = verify_rgb_frame(&gray, CANVAS_W, CANVAS_H, &[]);
        assert!(!v_gray.is_pass(), "a grayscale camera must FAIL the colour gate");
        let chromatic = PATCH_COLOURS.iter().filter(|c| chroma(**c) >= CHROMATIC_EXPECTED_MIN).count();
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
        let v_hue = verify_rgb_frame(&hue, CANVAS_W, CANVAS_H, &[]);
        assert!(!v_hue.is_pass(), "a hue-shifted camera must FAIL the colour gate");
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

    #[test]
    fn realistic_compression_noise_still_passes() {
        // A correct camera through H.264/NDI: small per-channel jitter + a slight overall lift.
        // The gate must NOT false-fail on this (tolerances are strict, not brittle).
        let noisy = paint_canvas(CANVAS_W, CANVAS_H, |c| {
            let j = |v: u8, d: i32| (v as i32 + d).clamp(0, 255) as u8;
            Rgb::new(j(c.r, 12), j(c.g, -9), j(c.b, 7))
        });
        let v = verify_rgb_frame(&noisy, CANVAS_W, CANVAS_H, &[]);
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
        let v = verify_rgb_frame(&buf, CANVAS_W, CANVAS_H, &[]);
        assert!(!v.is_pass(), "a neutral patch with a colour cast must FAIL");
        assert!(
            v.outcomes.iter().any(|o| matches!(o, PatchOutcome::NeutralTint { .. })),
            "the cast neutral patch fails as NeutralTint: {:?}",
            v.failures().collect::<Vec<_>>()
        );
    }

    #[test]
    fn unsamplable_patches_fail_closed() {
        // No samples at all ⇒ every patch unsamplable ⇒ NOT a pass.
        let v = verify_samples(&[]);
        assert_eq!(v.fail_count(), PATCH_COLOURS.len());
        assert!(!v.is_pass(), "nothing sampled proves nothing ⇒ fail-closed");
        assert!(v.outcomes.iter().all(|o| matches!(o, PatchOutcome::Unsamplable { .. })));
    }

    // ---- per-node aggregation across frames (majority rule) ----

    #[test]
    fn node_summary_charges_only_majority_patch_failures() {
        let correct = verify_rgb_frame(&paint_canvas(CANVAS_W, CANVAS_H, |c| c), CANVAS_W, CANVAS_H, &[]);
        let gray = verify_rgb_frame(&paint_canvas(CANVAS_W, CANVAS_H, to_gray), CANVAS_W, CANVAS_H, &[]);

        // 5 clean frames + 4 grayscale: grayscale is a MINORITY (4 of 9) ⇒ no patch charged.
        let mut frames = vec![correct.clone(); 5];
        frames.extend(vec![gray.clone(); 4]);
        let minority = summarize_node_colour(&frames);
        assert_eq!(minority.frames_sampled, 9);
        assert_eq!(minority.fail_count(), 0, "a minority of bad frames must not flip the gate");
        assert!(minority.is_pass());

        // 4 clean + 5 grayscale: grayscale is the MAJORITY ⇒ every chromatic patch charged.
        let mut frames = vec![correct; 4];
        frames.extend(vec![gray; 5]);
        let majority = summarize_node_colour(&frames);
        let chromatic = PATCH_COLOURS.iter().filter(|c| chroma(**c) >= CHROMATIC_EXPECTED_MIN).count();
        assert_eq!(majority.fail_count(), chromatic, "a majority grayscale node FAILS");
        assert!(!majority.is_pass());
    }

    #[test]
    fn empty_node_summary_is_fail_closed() {
        let s = summarize_node_colour(&[]);
        assert_eq!(s.frames_sampled, 0);
        assert!(!s.is_pass(), "zero sampled frames proves nothing");
    }
}
