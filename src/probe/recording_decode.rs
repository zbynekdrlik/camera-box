//! issue 1374 — the RECORDING decode core, split out of `qr` (which keeps the QR primitives:
//! render, the plain rqrr / Otsu passes, payload merge, the live-tap captures).
//!
//! This is the per-frame decode every recording analysis runs: the #207 fast-then-robust gate
//! (with the #632 groups and the #707 optical dimension), the #202 bottom-band tiles, the #754
//! top-band optical recovery, the issue-1370 slot recovery hook and the issue-1367 node-burn echo
//! gate ([`decode_qr_luma_all_fast_then_robust_gated`] is the one core). The items moved here
//! verbatim; `qr` re-exports the public ones, so `qr::decode_qr_luma_all_fast_then_robust_*` and
//! `qr::DecodePath` still resolve for every caller.

use crate::probe::burn_echo::{self, LocatedPayload, NodeBurnGate};
use crate::probe::payload::Payload;
use crate::probe::qr::{
    binarize_otsu, decode_qr_luma_all, decode_qr_luma_all_reads, merge_payloads,
    plain_and_otsu_reads, rqrr_decode_all_catch, OPTICAL_TOP_BAND_FRAC,
};
use image::GrayImage;
use std::sync::atomic::{AtomicU64, Ordering};

/// #207 — which decode path a single recording frame took.
///
/// The robust tiled passes are ~10× the cost of the plain full-frame pass, so the per-frame
/// decode ([`decode_qr_luma_all_fast_then_robust`]) runs them ONLY when the fast plain pass
/// missed an expected node burn. This enum reports that choice per-call so it is observable
/// WITHOUT shared global state — the path-reachability tests assert on it directly (a
/// process-wide counter would race against the recording unit tests running concurrently).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePath {
    /// The plain full-frame pass already carried every expected node burn — the tiles were
    /// skipped (the ~99 %+ common case on a clean recording).
    Fast,
    /// An expected node burn was missing from the plain pass — the tiled+upscaled recovery ran
    /// (and, for a burn the tiles still missed, its isolated slot crop — issue 1370).
    Robust,
}

/// #207 — process-wide counters of the decode-path split, for the verdict LOG only (so it
/// shows fast ≫ robust = the speedup is real). They accumulate across all recordings in one
/// run. NOT used by tests (those assert the per-call [`DecodePath`] instead — globals race
/// against concurrent tests). `fast` = plain pass already had every expected burn; `robust` =
/// a burn was missing so the tiles ran.
static FAST_PATH_FRAMES: AtomicU64 = AtomicU64::new(0);
static ROBUST_FALLBACK_FRAMES: AtomicU64 = AtomicU64::new(0);

/// (fast_path_frames, robust_fallback_frames) since process start — the #207 decode-path
/// split, for the verdict log.
pub fn decode_path_counts() -> (u64, u64) {
    (
        FAST_PATH_FRAMES.load(Ordering::Relaxed),
        ROBUST_FALLBACK_FRAMES.load(Ordering::Relaxed),
    )
}

/// #202 — how many horizontal tiles the robust (offline-recording) decode splits the BOTTOM
/// band into. The #111 layout fixes all three node burns to the BOTTOM of the frame
/// (strih = bottom-left, cam1 = bottom-center, stream = bottom-right corner) while the large
/// optical dual-QR sits in the TOP band and ALWAYS decodes on the full-frame pass. So the
/// recovery passes only need the bottom band, split into 3 columns — one per burn — giving
/// rqrr a sub-image in which the small ~320px burn is LARGE relative to the tile, so its
/// finder pattern locks where the full-frame `detect_grids` pass missed it. 3 columns (vs a
/// full 3×3 grid) cover every burn at 1/3 the rqrr passes (proven: the bottom band recovers
/// all 17 real flagged burns — cam1 11/11, strih 5/5, stream 1/1).
const TILE_COLS: u32 = 3;

/// #202 — fraction of the frame HEIGHT, measured from the bottom, the tile band covers. The
/// burns sit in the bottom corners + center (the #111 4-corner layout, ~0.28×h tall burns
/// bottom-anchored), so the bottom 45% always contains them whole with margin; the top
/// dual-QR is excluded (it never needs a tile — it decodes full-frame).
const TILE_BOTTOM_BAND_FRAC: f32 = 0.45;

/// #202 — fractional overlap between adjacent column tiles, so a burn straddling a column
/// boundary is still WHOLLY inside at least one tile (a QR cut by a hard tile edge decodes
/// in neither). 0.25 = each tile extends a quarter of its width into its neighbours.
const TILE_OVERLAP_FRAC: f32 = 0.25;

/// #202 — minimum long-side px a tile is upscaled TO before rqrr when it is smaller. A
/// crisp but small burn (the cam1 320px mark, sharp yet only ~7px/module in the 1080p
/// recording) decodes more reliably for rqrr's finder when the modules are a few px larger;
/// cubic-upscaling the tile to this floor gives the finder more pixels per module WITHOUT
/// inventing detail (the pattern is already sharp — see #202: cv2 read every flagged burn
/// from the same pixels). 1280 keeps a 1/3-of-1080 (≈640px) tile at ~2× and a 1/3-of-4K
/// (≈1280px) tile unchanged.
const TILE_UPSCALE_MIN: u32 = 1280;

/// #202 — the ROBUST offline-recording decode: every CRC-valid QR in the frame, recovering
/// the small burns rqrr's single full-frame `detect_grids` pass misses.
///
/// rqrr's grid detector reliably finds the LARGE optical dual-QR but intermittently misses
/// the small ~320px node burns when several QRs of very different sizes share one
/// full-resolution frame (#186/#202 — the burn pixels are present and sharp; cv2 reads every
/// flagged frame from the identical pixels, so this is a detector-coverage gap, NOT a
/// burn-size or burn-readability defect). The fix gives rqrr a fair look at each region:
///
/// 1. the plain full-frame pass (`decode_qr_luma_all`) — finds the big top dual-QR cheaply;
/// 2. the BOTTOM band ([`TILE_BOTTOM_BAND_FRAC`] of the height) split into [`TILE_COLS`]
///    OVERLAPPING column tiles ([`TILE_OVERLAP_FRAC`]) — one per bottom burn — each
///    cubic-upscaled to at least [`TILE_UPSCALE_MIN`] on its long side, rqrr-decoded; in a
///    column tile a 320px burn is large-relative and its finder locks.
///
/// All passes' CRC-valid payloads are merged, de-duped by `(run_id, frame_id)`
/// ([`merge_payloads`]), so a node's burn is counted exactly once and the result is always a
/// SUPERSET of the plain pass. Offline only (the parallel #166/#187 recording decode) — it
/// runs a few extra rqrr passes per frame, so it is NEVER on the latency-sensitive live tap.
pub fn decode_qr_luma_all_robust(img: GrayImage) -> Vec<Payload> {
    let mut out = decode_qr_luma_all(img.clone());
    robust_tile_passes(&img, &mut out);
    out
}

/// #1280 — the MAXIMALLY robust still-frame decode for the painter's dual-QR + aux layout:
/// [`decode_qr_luma_all_robust`] (plain full-frame ∪ Otsu ∪ the #202 bottom-band upscaled tiles)
/// UNION the #754 top-band optical crop ([`robust_optical_top_band`]) UNION a per-HALF top-band
/// crop ([`robust_optical_half_passes`]).
///
/// Root cause of the #1280 flake it recovers (traced in the rqrr-0.9.3 source, confirmed by an
/// adversarial review of the crate): the plain full-frame pass wraps ALL of rqrr's `detect_grids`
/// in ONE `catch_unwind` ([`rqrr_decode_all_catch`], #673), so a caught internal panic — the
/// silenced `assert!(scan >= 1)` in `identify/grid.rs`, or the `Perspective::map` assert in
/// `geometry.rs` — raised by a single bogus/near-degenerate capstone grouping empties the WHOLE
/// pass, dropping every code at once (not "one of four"). On the crisp 4-code canvas the degenerate
/// geometry comes from the two 210px aux marks co-located ~4px apart (issue 1270) and their
/// interplay with the primary finders — small, adjacent codes are exactly the "pathologically small
/// / near-degenerate" case #673 documents. It is per-run because the primary payloads bake a
/// varying `gen_ts_ns` into the QR content.
///
/// Every recovery region runs UNCONDITIONALLY, so a dropped code is recovered from a region-isolated
/// look where the degenerate grouping cannot form: each 700px primary from its OWN top-band HALF
/// crop (a lone crisp code — no cross-code triple can form, and rqrr reads its fixed finder/timing/
/// alignment patterns content-independently), the aux marks from their upscaled bottom-band tile
/// (the 2× upscale enlarges the small modules past rqrr's degeneracy edge). The full-width top-band
/// crop is a cheap extra look (it recovers BOTH primaries at once when the panic was aux-rooted).
/// Each pass merges by `(run_id, frame_id)` ([`merge_payloads`]), so the result is a strict SUPERSET
/// of the plain pass and a given `(run_id, frame_id)` always carries the SAME `gen_ts_ns` — byte
/// identity is preserved. This composes the EXISTING production recovery passes the RECORDING decode
/// already fires conditionally
/// ([`decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`], which likewise protects the rig
/// optical read from the same whole-pass panic); it exists for the synthetic multi-QR painter/qr
/// tests, whose crisp 4-code canvas is the one decode with no other recovery (#1280). No production
/// caller is changed.
pub fn decode_qr_luma_all_robust_optical(img: GrayImage) -> Vec<Payload> {
    let mut out = decode_qr_luma_all_robust(img.clone());
    robust_optical_top_band(&img, &mut out);
    robust_optical_half_passes(&img, &mut out);
    out
}

/// #1280 — recover each dual-QR PRIMARY from its OWN top-band HALF crop. [`render_qr_dual_bgra`]
/// centres the two halves in `[0, w/2)` and `[w/2, w)`; cropping ONE half of the top band
/// ([`OPTICAL_TOP_BAND_FRAC`], which excludes the bottom aux marks entirely) leaves a SINGLE crisp
/// primary. A lone finder triple can form no degenerate cross-code group, so the caught-panic /
/// whole-pass-empty failure of the multi-code full-frame pass (see [`decode_qr_luma_all_robust_optical`])
/// cannot reproduce, and rqrr decodes the primary from its fixed patterns content-independently.
/// Each half's CRC-valid payloads merge into `out` (de-duped by `(run_id, frame_id)`), so `out` is
/// always a SUPERSET of what it carried on entry — never fewer. (On the 2560-wide override canvas
/// the width-scaled primary is taller than the top band, so a half crop bisects it and simply adds
/// nothing; the full-frame pass covers that 2-code, aux-free geometry.)
///
/// [`render_qr_dual_bgra`]: crate::probe::qr::render_qr_dual_bgra
fn robust_optical_half_passes(img: &GrayImage, out: &mut Vec<Payload>) {
    let (w, h) = (img.width(), img.height());
    if w < 2 || h < 2 {
        return;
    }
    let band_h = crate::colour_scale::top_band_crop_height(h, OPTICAL_TOP_BAND_FRAC);
    let half = w / 2;
    for (x0, wpart) in [(0u32, half), (half, w - half)] {
        if wpart == 0 {
            continue;
        }
        let crop = image::imageops::crop_imm(img, x0, 0, wpart, band_h).to_image();
        merge_payloads(out, decode_qr_luma_all(crop));
    }
}

/// The expensive part of the robust decode: the bottom-band tiled+upscaled rqrr passes that
/// recover the small node burns the plain full-frame pass missed. Factored out of
/// [`decode_qr_luma_all_robust`] so the #207 fast path can run it CONDITIONALLY (only when a
/// burn is actually missing) instead of on every frame. Merges each tile's CRC-valid
/// payloads into `out` (de-duped by `(run_id, frame_id)`), so `out` is always a SUPERSET of
/// what it carried on entry — never fewer.
fn robust_tile_passes(img: &GrayImage, out: &mut Vec<Payload>) {
    merge_payloads(
        out,
        burn_echo::payloads(tile_pass_reads(img, NodeBurnGate::Off)),
    );
}

/// Every read of the #202 bottom tiles, tile after tile, each mapped back to FRAME pixels through
/// its crop offset and upscale (issue 1367: the echo gate places each read on the frame). `gate`
/// only decides when a tile gets its Otsu retry; the reads are gated by the caller.
fn tile_pass_reads(img: &GrayImage, gate: NodeBurnGate) -> Vec<LocatedPayload> {
    let (w, h) = (img.width(), img.height());
    let mut reads = Vec::new();
    // A tiny frame is one tile — the plain pass already covered it; nothing to gain.
    if w < TILE_COLS || h < 2 {
        return reads;
    }

    // The bottom band that holds the burns (the top dual-QR is excluded — it decodes
    // full-frame). Band top = h - band_h; the tiles span [band_top, h).
    let band_h = (((h as f32) * TILE_BOTTOM_BAND_FRAC) as u32).clamp(1, h);
    let band_top = h - band_h;

    // Overlapping column geometry across the band: base step = w / cols, each column extended
    // by the overlap on each side (clamped to the frame width). The columns span the whole
    // width, so every bottom burn — left / center / right — falls wholly inside at least one.
    let step_x = (w / TILE_COLS).max(1);
    let over_x = ((step_x as f32) * TILE_OVERLAP_FRAC) as u32;

    for gx in 0..TILE_COLS {
        let x0 = (gx * step_x).saturating_sub(over_x);
        // The last column extends to the frame edge so no right strip is left uncovered.
        let x1 = if gx == TILE_COLS - 1 {
            w
        } else {
            (((gx + 1) * step_x) + over_x).min(w)
        };
        let tw = x1 - x0;
        if tw == 0 {
            continue;
        }
        let tile = image::imageops::crop_imm(img, x0, band_top, tw, band_h).to_image();
        // Upscale a small tile so the burn's modules span more px for rqrr's finder.
        let long = tw.max(band_h);
        let tile = if long < TILE_UPSCALE_MIN {
            let nw = (tw * TILE_UPSCALE_MIN / long).max(1);
            let nh = (band_h * TILE_UPSCALE_MIN / long).max(1);
            image::imageops::resize(&tile, nw, nh, image::imageops::FilterType::CatmullRom)
        } else {
            tile
        };
        // One decoded pixel spans (tile px / decoded px) of the frame; 1.0 when not upscaled.
        let scale_x = f64::from(tw) / f64::from(tile.width().max(1));
        let scale_y = f64::from(band_h) / f64::from(tile.height().max(1));
        let in_frame =
            |tile_reads| burn_echo::reads_in_frame(tile_reads, x0, band_top, scale_x, scale_y);
        // The tile decode: the plain pass, then an Otsu-binarized retry when no plain read counts.
        // Both go through the panic-safe `rqrr_decode_all_catch` (#673), so a degenerate tile
        // yields "nothing" instead of aborting the frame's decode. Under `Off` "counts" is "any
        // read at all" (the #202 rule); under `OwnSlot` a tile whose plain pass read only node-burn
        // echoes gets the Otsu look too, so an echo never costs a real burn its retry (issue 1367).
        let plain = in_frame(rqrr_decode_all_catch(tile.clone()));
        let retry = !burn_echo::any_read_counts(&plain, w, h, gate);
        reads.extend(plain);
        if retry {
            reads.extend(in_frame(rqrr_decode_all_catch(binarize_otsu(&tile))));
        }
    }
    reads
}

/// #754 — the TOP-band optical-recovery pass: crop the top [`OPTICAL_TOP_BAND_FRAC`] of the
/// frame (which holds the whole cam2 dual-QR Vernier and NONE of the bottom node burns) and run
/// the SAME plain∪Otsu decode ([`decode_qr_luma_all`]) over just that band, merging any
/// CRC-valid payloads into `out` (de-duped by `(run_id, frame_id)`). Because the crop excludes
/// the bottom burns' SLOTS, it never adds a real node burn — `out` stays a strict SUPERSET of
/// what it carried on entry — so it is safe to fire without knowing which run_id the optical is
/// (pin-independent). It CAN read a node burn's optical ECHO when a camera films a monitor showing
/// OBS (issue 1367, cam2 on the strih-lx HDMI multiview); the recording decode gates those out
/// ([`crate::probe::burn_echo`]). This is the top-band twin of [`robust_tile_passes`] (which
/// crops the BOTTOM band for the small burns); see [`OPTICAL_TOP_BAND_FRAC`] for the #751/#754
/// root cause and the 30/30-vs-0/30 offline measurement that fixes the band fraction.
fn robust_optical_top_band(img: &GrayImage, out: &mut Vec<Payload>) {
    merge_payloads(out, burn_echo::payloads(top_band_reads(img)));
}

/// Every read of the #754 top-band crop, in FRAME pixels (the crop starts at the frame origin and
/// is not resized, so its pixels are frame pixels).
fn top_band_reads(img: &GrayImage) -> Vec<LocatedPayload> {
    let (w, h) = (img.width(), img.height());
    // A tiny frame is already one look for the plain pass — nothing a crop can add.
    if w < 2 || h < 2 {
        return Vec::new();
    }
    // #718: crop-height arithmetic now lives in the pure crate-root helper (shared with
    // `colour_sample::detect_dual_qr`'s own retry) — same formula, one source of truth.
    let band_h = crate::colour_scale::top_band_crop_height(h, OPTICAL_TOP_BAND_FRAC);
    // Same size as the source when the frac rounds to the full height — still a valid, cheap
    // second look (Otsu over the whole frame), never a panic.
    let band = image::imageops::crop_imm(img, 0, 0, w, band_h).to_image();
    decode_qr_luma_all_reads(band)
}

/// #754 — is the plain full-frame pass SHORT of the optical dual-QR, i.e. does the top-band
/// recovery ([`robust_optical_top_band`]) have work to do? Two modes, both keyed off the fact
/// that the painter's Vernier ALWAYS shows TWO halves of ONE run_id per frame (`painter::
/// vernier_ids`), while every node BURN is a single QR (exactly one `frame_id` per run_id per
/// frame):
///
/// * **Pinned** (`Some((run_id, min_distinct))`) — the caller KNOWS the optical run_id (the
///   `--cam2-run-id` pin, #707). Short ⇔ fewer than `min_distinct` DISTINCT `frame_id`s for that
///   run_id decoded on the plain pass. Identical predicate to the #707 fast-path optical gate,
///   just used here to decide whether to run the recovery FIRST.
/// * **Unpinned** (`None`) — the caller does NOT know the optical run_id (strih extracts
///   unpinned — the exact case the #707 gate could not save, see #754). We can still tell the
///   read is short structurally: on a healthy frame the optical contributes the ONLY run_id with
///   ≥2 distinct `frame_id`s; if NO run_id has ≥2, the optical is at best a single held half (or
///   absent), so the top band is worth a look. This fires exactly on the decayed frames (early
///   healthy frames, where both halves decode full-frame, are skipped for free) and never
///   mistakes a burn for a complete optical.
fn optical_read_short(payloads: &[Payload], min_distinct_optical: Option<(u32, usize)>) -> bool {
    match min_distinct_optical {
        Some((run_id, min_distinct)) => {
            let distinct: std::collections::HashSet<u32> = payloads
                .iter()
                .filter(|p| p.run_id == run_id)
                .map(|p| p.frame_id)
                .collect();
            distinct.len() < min_distinct
        }
        None => {
            let mut by_run: std::collections::HashMap<u32, std::collections::HashSet<u32>> =
                std::collections::HashMap::new();
            for p in payloads {
                by_run.entry(p.run_id).or_default().insert(p.frame_id);
            }
            !by_run.values().any(|ids| ids.len() >= 2)
        }
    }
}

/// #207 — the PER-FRAME recording decode: plain full-frame pass FIRST (fast), then the
/// robust tiled recovery ONLY when the fast pass missed an expected node burn.
///
/// The robust tiled passes ([`robust_tile_passes`]) cost ~10× the plain full-frame pass, yet
/// on a clean genlocked recording the plain pass already reads EVERY node burn on ~99 %+ of
/// frames (#202: the tiles only ever recover the rare frame where rqrr's single
/// `detect_grids` misses a small burn — the residual #186 coverage gap). Running the tiles on
/// every frame therefore made a 30-min verdict take ~50 min when ~5 would do.
///
/// So: decode the full frame plainly; if every id in `expected_burn_run_ids` already decoded,
/// return immediately (the FAST path); otherwise run the tiled recovery (the ROBUST
/// fallback). The result is IDENTICAL to [`decode_qr_luma_all_robust`] for any frame whose
/// burns the plain pass already had (a SUPERSET-of-plain that the tiles couldn't extend), and
/// for any frame missing a burn the full robust passes run unchanged — so the #186 0-miss
/// guarantee is preserved exactly, just gated behind a cheap plain-first check. `expected_…`
/// empty ⇒ always fast (no burns to require); pass [`recording::NODE_BURN_RUN_IDS`] for the
/// recording path. issue 1370: an expected burn the tiles STILL miss is then read from its own
/// isolated slot crop ([`crate::probe::burn_region_decode::recover_missing_burns`]), so on such a
/// frame the result is a superset of [`decode_qr_luma_all_robust`] that adds only that expected
/// burn.
pub fn decode_qr_luma_all_fast_then_robust(
    img: GrayImage,
    expected_burn_run_ids: &[u32],
) -> Vec<Payload> {
    let (out, path) = decode_qr_luma_all_fast_then_robust_pathed(img, expected_burn_run_ids);
    // Record the split for the verdict log (observability only — never gates correctness).
    match path {
        DecodePath::Fast => FAST_PATH_FRAMES.fetch_add(1, Ordering::Relaxed),
        DecodePath::Robust => ROBUST_FALLBACK_FRAMES.fetch_add(1, Ordering::Relaxed),
    };
    out
}

/// #632 gap 1 — [`decode_qr_luma_all_fast_then_robust`] (the counting public wrapper) over
/// [`decode_qr_luma_all_fast_then_robust_grouped_pathed`]'s two-group gate. See that function's
/// doc for why the mandatory/any-of split exists.
pub fn decode_qr_luma_all_fast_then_robust_grouped(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
) -> Vec<Payload> {
    decode_qr_luma_all_fast_then_robust_grouped_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        None,
    )
}

/// #707 — [`decode_qr_luma_all_fast_then_robust_grouped`] with the third (optical) gate
/// dimension; see [`decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`].
pub fn decode_qr_luma_all_fast_then_robust_grouped_optical(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> Vec<Payload> {
    let (out, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        min_distinct_optical,
    );
    match path {
        DecodePath::Fast => FAST_PATH_FRAMES.fetch_add(1, Ordering::Relaxed),
        DecodePath::Robust => ROBUST_FALLBACK_FRAMES.fetch_add(1, Ordering::Relaxed),
    };
    out
}

/// [`decode_qr_luma_all_fast_then_robust`] that ALSO returns which [`DecodePath`] it took.
/// This is the core; the counting public wrapper above just records the path. Returning the
/// path makes the choice observable per-call (no global state), so the path-reachability tests
/// are deterministic even when other tests decode concurrently.
pub fn decode_qr_luma_all_fast_then_robust_pathed(
    img: GrayImage,
    expected_burn_run_ids: &[u32],
) -> (Vec<Payload>, DecodePath) {
    // Every id in `expected_burn_run_ids` is MANDATORY (an empty `any_of` group is vacuously
    // satisfied) — this is the pre-#632 behavior, unchanged for every existing caller. This
    // per-frame helper keeps every read wherever it was found (issue 1367 `Off`): no recording
    // goes through it (`recording::analyze_recording*` runs the gated grouped decode), and tests
    // use it with node burns drawn at arbitrary positions on synthetic canvases.
    let d = decode_qr_luma_all_fast_then_robust_gated(
        img,
        expected_burn_run_ids,
        &[],
        None,
        NodeBurnGate::Off,
    );
    (d.payloads, d.path)
}

/// #632 gap 1 — [`decode_qr_luma_all_fast_then_robust_pathed`] generalized with a SECOND,
/// independent group: `mandatory_burn_run_ids` must ALL be found (unchanged semantics; since
/// issue 1367 this grouped path also runs the node-burn echo gate, the flat one does not), and
/// `any_of_burn_run_ids` needs only ONE member found (empty ⇒ vacuously satisfied, matching
/// `mandatory`'s existing empty-list behavior). This is what lets a recording whose
/// camera-under-test is cam3/cam4/cam5/cam6/cam2 (instead of the historically-hardcoded cam1)
/// take the #207 FAST path too: cam1..cam6 are mutually exclusive in a real run (only the
/// physically-deployed camera's burn ever appears), so requiring "cam1 AND strih" (the old
/// single-group gate) is permanently unsatisfiable — and permanently ROBUST — for any OTHER
/// deployed camera. Passing `mandatory = [strih]`, `any_of = [cam1, cam2, cam3, cam4, cam5,
/// cam6]` instead fixes that: the mandatory hop burn(s) are still always required, and the
/// deployed camera's OWN burn (whichever one it is) still satisfies the group — so a genuinely
/// missing/unreadable deployed-camera burn on THIS frame still correctly falls through to the
/// robust recovery (the #186 0-miss guarantee is unaffected; see the doc on
/// [`decode_qr_luma_all_fast_then_robust_pathed`]). A flat UNION of all 6 ids into one
/// mandatory list (requiring all of them on every frame) would NOT fix this — it would make the
/// gate permanently unsatisfiable in the OTHER direction (an undeployed camera's id never
/// appears either) — this is why the two groups must stay separate.
pub fn decode_qr_luma_all_fast_then_robust_grouped_pathed(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
) -> (Vec<Payload>, DecodePath) {
    decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        None,
    )
}

/// #707 — the #207 gate's THIRD, independent completeness dimension: the cam2 dual-QR
/// Vernier optical read. `min_distinct_optical = Some((optical_run_id, min_distinct_ids))`
/// requires the plain pass to already carry at least `min_distinct_ids` DISTINCT `frame_id`s
/// for `optical_run_id` before the fast path may skip the tiled recovery — `None` preserves
/// the exact pre-#707 behavior (burns only), unchanged for every existing caller.
///
/// **Why this is needed** (found investigating #707's residual `all_cambox_continuity`
/// `copies`/`gaps`): the dual-QR Vernier paints TWO regions per refresh (left=latest even
/// tick, right=latest odd tick, `painter::vernier_ids`) — a healthy frame's plain pass finds
/// BOTH as two distinct payloads. But the pre-#707 gate here only ever checked the NODE BURNS
/// (cam1/strih/stream/camN) — a frame where the plain pass reads the (small, digitally
/// rendered, easy) node burns fine but MISSES one dual-QR half (the actively-repainting region
/// is naturally harder to read than the held one — moiré/shimmer/focus-breathing during a brief
/// optical-degradation window) took the FAST path anyway, silently skipping the #202 robust
/// tiled+upscaled retry that has ALREADY been proven (see
/// [`fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame`]) to recover reads
/// the plain pass alone misses. The frame's resolved Vernier tick then falls back to whichever
/// single held id DID decode — the SAME value the immediately preceding frame already reported
/// — registering as a spurious `all_cambox_continuity` "copy" even though nothing was actually
/// duplicated on screen (#707 offline-validated every such frame's decoded `(frame_id,
/// gen_ts_ns)` against the painter's own ground-truth CSV: 92939/92939 payloads across 5 full
/// recordings matched a REAL painted tick exactly — zero hallucinated/misread values — so this
/// is a decode-COVERAGE gap, never a decoder-correctness bug; see #707's own issue thread).
///
/// issue 1367: this is the decode behind every recording analysis (`recording::analyze_recording*`:
/// strih, stream, imag, cg, the cam1 grab), so it runs the node-burn echo gate
/// ([`NodeBurnGate::OwnSlot`], see [`crate::probe::burn_echo`]).
pub fn decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> (Vec<Payload>, DecodePath) {
    let d = decode_qr_luma_all_fast_then_robust_gated(
        img,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        min_distinct_optical,
        NodeBurnGate::OwnSlot,
    );
    (d.payloads, d.path)
}

/// The per-frame recording decode core (#207 fast-then-robust, #632 groups, #707 optical gate,
/// #754 top band, issue 1370 slot recovery), with the issue-1367 node-burn echo gate: under
/// [`NodeBurnGate::OwnSlot`] each pass's reads are placed on the frame and a slotted node burn
/// read outside its own slot is dropped as an echo before it merges. An echo therefore never
/// satisfies the optical-short check, the fast-path gate or the missing-burn list. Under
/// [`NodeBurnGate::Off`] the result is byte-identical to the pre-issue-1367 decode.
pub fn decode_qr_luma_all_fast_then_robust_gated(
    img: GrayImage,
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
    gate: NodeBurnGate,
) -> burn_echo::FrameDecode {
    let (w, h) = (img.width(), img.height());
    // The full-frame pass (plain, then Otsu), gated read by read before anything merges: under
    // `Off` this is exactly `decode_qr_luma_all` (the plain reads as they came, Otsu merged
    // keep-first).
    let (plain, otsu) = plain_and_otsu_reads(img.clone());
    let (mut out, plain_echoes) = burn_echo::split_node_burn_echoes(plain, w, h, gate);
    let mut echoes = Vec::new();
    merge_payloads(&mut echoes, plain_echoes);
    burn_echo::admit_reads(&mut out, &mut echoes, otsu, w, h, gate);

    // #754: the SOFT optical dual-QR (top band) is missed by the full-frame plain pass on late
    // #751-sweep frames (rqrr locates the grid but the full-frame perspective/threshold decode
    // fails) WHILE the crisp bottom burns still decode — so the fast-path gate can pass with the
    // optical actually absent (unpinned: it never checked the optical; pinned: even the robust
    // fallback only tiled the BOTTOM band, never the optical's top band). Give the TOP band the
    // isolated look robust_tile_passes gives the bottom burns, BEFORE the gate, whenever the
    // plain pass is short of the optical — so a frame that was ONLY optical-short (burns fine)
    // recovers and takes the cheap FAST path instead of the ~10× bottom-tile robust fallback.
    // The crop excludes the burn slots, so it never adds a real burn; a node-burn ECHO it reads
    // (issue 1367) is dropped by the gate here, before the fast-path check below.
    if optical_read_short(&out, min_distinct_optical) {
        burn_echo::admit_reads(&mut out, &mut echoes, top_band_reads(&img), w, h, gate);
    }

    let path = if fast_path_gate_satisfied(
        &out,
        mandatory_burn_run_ids,
        any_of_burn_run_ids,
        min_distinct_optical,
    ) {
        DecodePath::Fast
    } else {
        // Robust fallback: a mandatory burn is missing, NONE of the any-of group decoded, or
        // (#707) the dual-QR optical read is STILL short after the #754 top-band recovery — give
        // rqrr the tiled+upscaled look (#202) that recovers reads the full-frame pass
        // intermittently misses.
        burn_echo::admit_reads(
            &mut out,
            &mut echoes,
            tile_pass_reads(&img, gate),
            w,
            h,
            gate,
        );

        // issue 1370: an expected burn the tiles STILL missed gets an isolated look at its own
        // known slot, so optical content the camera happens to put next to it in the tile cannot
        // hide it. A no-op when the frame went robust only for the optical dimension.
        crate::probe::burn_region_decode::recover_missing_burns(
            &img,
            mandatory_burn_run_ids,
            any_of_burn_run_ids,
            &mut out,
        );
        DecodePath::Robust
    };
    if gate == NodeBurnGate::OwnSlot {
        burn_echo::record_frame_echoes(&echoes);
    }
    burn_echo::FrameDecode {
        payloads: out,
        path,
        echoes,
    }
}

/// Pure #207/#707 fast-path gate DECISION — every completeness dimension the plain-pass
/// `payloads` must already satisfy before the ~10×-cost robust tiled retry can be skipped.
/// Extracted from [`decode_qr_luma_all_fast_then_robust_grouped_pathed_optical`] so the
/// decision itself is directly unit-testable against hand-built payload lists, with no image
/// decode involved (mirrors the project's other pure-decision seams, e.g. `send_stall::
/// is_send_stall`).
fn fast_path_gate_satisfied(
    payloads: &[Payload],
    mandatory_burn_run_ids: &[u32],
    any_of_burn_run_ids: &[u32],
    min_distinct_optical: Option<(u32, usize)>,
) -> bool {
    let mandatory_present = mandatory_burn_run_ids
        .iter()
        .all(|id| payloads.iter().any(|p| p.run_id == *id));
    let any_of_present = any_of_burn_run_ids.is_empty()
        || any_of_burn_run_ids
            .iter()
            .any(|id| payloads.iter().any(|p| p.run_id == *id));
    let optical_complete = match min_distinct_optical {
        None => true,
        Some((run_id, min_distinct_ids)) => {
            let distinct: std::collections::HashSet<u32> = payloads
                .iter()
                .filter(|p| p.run_id == run_id)
                .map(|p| p.frame_id)
                .collect();
            distinct.len() >= min_distinct_ids
        }
    };
    mandatory_present && any_of_present && optical_complete
}

#[cfg(test)]
#[path = "recording_decode_tests.rs"]
mod tests;
