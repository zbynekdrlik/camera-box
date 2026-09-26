//! issue 1374 — the unit tests of `recording_decode` (a `#[path]` child, split out like the other
//! large modules' tests). They moved verbatim from `qr`'s test module with the items they test.

use super::*;
use crate::probe::luma::bgra_to_luma;
use crate::probe::qr::tests::{blit_burn_luma, optical_fixture_luma};
use crate::probe::qr::{
    blit_aux_tick_bgra, cam1_burn_origin, render_payload_qr, render_qr_bgra, render_qr_dual_bgra,
    CAM1_BURN_QR_PX, TOP_MARGIN_PX,
};

#[test]
fn optical_dual_qr_recovered_on_late_sweep_frame_via_top_band_755754() {
    // #754 — a REAL late-range (#751 motion-sweep) frame from the surviving imag recording of
    // run 303636614 (t≈290s, frame-12081): the crisp bottom node burns (cam1 911001, imag
    // 911003) decode on the plain full-frame pass, but the SOFT optical dual-QR (run_id
    // 303636614, top band) does NOT — the exact decoder-side decay this ticket is about. cv2
    // reads both optical halves off this same frame; rqrr's full-frame pass locates the
    // optical grid but the perspective/threshold decode fails. The #754 top-band recovery
    // crop restores it. Fixture committed alongside; sharpness/contrast are constant across
    // the decay (see the ticket) — this is a coverage gap, not a pixel defect.
    const OPTICAL_RUN_ID: u32 = 303_636_614;
    const IMAG_BURN: u32 = 911_003; // present on this frame (fast-path burn gate is satisfied)
    let luma = optical_fixture_luma("optical-sweep-decay-late-imag.png");

    // RED-condition lock (permanent, mechanism): the FULL-FRAME plain∪Otsu pass
    // (`decode_qr_luma_all`, the current production full-frame decode) MISSES the optical on
    // this late frame — 0 payloads for run_id 303636614 — even though it reads the burns.
    // This is what makes every fast-path decode of this frame drop the optical. If a future
    // rqrr reads it full-frame, re-tune this fixture, never delete the guard.
    let full = decode_qr_luma_all(luma.clone());
    assert!(
        !full.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
        "the full-frame plain∪Otsu pass is expected to MISS the soft optical on this late \
         sweep frame (run_id {OPTICAL_RUN_ID}); got {:?}",
        full.iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );
    assert!(
        full.iter().any(|p| p.run_id == IMAG_BURN),
        "sanity: the crisp imag burn {IMAG_BURN} MUST decode full-frame on this same frame \
         (proves the decode itself works — only the soft optical is missed); got {:?}",
        full.iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );

    // GREEN (PINNED, the fused verdict + imag/stream path, #707 `--cam2-run-id`): the
    // per-frame recording decode, gated with min_distinct_optical=Some, recovers BOTH optical
    // halves via the #754 top-band crop. Without the fix this returns only burns → 0 optical
    // → fails.
    let pinned = decode_qr_luma_all_fast_then_robust_grouped_optical(
        luma.clone(),
        &[IMAG_BURN],
        &[],
        Some((OPTICAL_RUN_ID, 2)),
    );
    let optical_pinned = distinct_optical_ids(&pinned, OPTICAL_RUN_ID);
    assert!(
        optical_pinned.len() >= 2,
        "PINNED: the recording decode must recover BOTH optical dual-QR halves via the #754 \
         top-band crop (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got {optical_pinned:?} \
         from {:?}",
        pinned
            .iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );

    // GREEN (UNPINNED, the strih-extract path — min_distinct_optical=None, the exact case the
    // #707 gate could NOT save, see #754): the burns satisfy the gate (fast path), yet the
    // structural optical-short trigger still fires the top-band recovery and the optical is
    // surfaced. This is what makes the fix pin-INDEPENDENT.
    let unpinned =
        decode_qr_luma_all_fast_then_robust_grouped_optical(luma.clone(), &[IMAG_BURN], &[], None);
    let optical_unpinned = distinct_optical_ids(&unpinned, OPTICAL_RUN_ID);
    assert!(
        optical_unpinned.len() >= 2,
        "UNPINNED: the top-band recovery must still surface BOTH optical halves without a \
         --cam2-run-id pin (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got \
         {optical_unpinned:?} from {:?}",
        unpinned
            .iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );
}

/// The DISTINCT `frame_id`s decoded for `run_id`, sorted+deduped — the count of optical
/// Vernier halves recovered for the #754 fixture assertions.
fn distinct_optical_ids(payloads: &[Payload], run_id: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = payloads
        .iter()
        .filter(|p| p.run_id == run_id)
        .map(|p| p.frame_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn robust_is_always_a_superset_of_the_plain_pass() {
    // No regression: every payload the plain pass finds is also in the robust result
    // (robust = plain ∪ tile passes, de-duped). A clean dual-QR frame decodes identically
    // plus whatever the tiles add — never fewer.
    let left = Payload {
        run_id: 7,
        frame_id: 100,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 101,
        gen_ts_ns: 2,
    };
    let (w, h) = (1920u32, 1080u32);
    let bgra = render_qr_dual_bgra(&left, &right, w, h, 700);
    let luma = bgra_to_luma(&bgra, w, h, w * 4);
    let plain = decode_qr_luma_all(luma.clone());
    let robust = decode_qr_luma_all_robust(luma);
    assert!(
        !plain.is_empty(),
        "the clean dual-QR must decode: {plain:?}"
    );
    for p in &plain {
        assert!(
            robust.contains(p),
            "robust must be a SUPERSET of the plain pass: {p:?} missing from {robust:?}"
        );
    }
}

#[test]
fn robust_does_not_duplicate_a_burn_seen_in_multiple_tiles() {
    // The same burn appears in several overlapping tiles AND the full-frame pass; the
    // result must carry each DISTINCT (run_id, frame_id) exactly once (merge_payloads
    // de-dups), never an inflated count that would over-report.
    let burn = Payload {
        run_id: 911_001,
        frame_id: 555,
        gen_ts_ns: 9,
    };
    let (w, h) = (1280u32, 720u32);
    let mut luma = GrayImage::from_raw(w, h, vec![255u8; (w * h) as usize]).unwrap();
    // A 200px burn in the BOTTOM band (where the recovery tiles look), straddling the
    // boundary between two overlapping column tiles so it lands inside BOTH at once — the
    // exact condition the (run_id, frame_id) de-dup must collapse to a single payload.
    let qpx = 200u32;
    let qr = render_payload_qr(&burn, qpx);
    let qh = qr.height();
    // x at ~1/3 width (the col0/col1 boundary), y near the bottom (inside the band).
    blit_burn_luma(&mut luma, &burn, qpx, w / 3 - qr.width() / 2, h - qh - 30);
    let robust = decode_qr_luma_all_robust(luma);
    let count = robust
        .iter()
        .filter(|p| p.run_id == burn.run_id && p.frame_id == burn.frame_id)
        .count();
    assert_eq!(
        count, 1,
        "a burn seen in many tiles must appear exactly once: {robust:?}"
    );
}

#[test]
fn robust_does_not_panic_on_a_degenerate_frame() {
    // rqrr's grid identifier has an internal assert!(scan >= 1) that PANICS on certain
    // degenerate finder geometries (rqrr-0.9.3 identify/grid.rs:206). The robust pass
    // feeds rqrr many sub-tiles; a panicking tile must NOT abort the whole frame's decode
    // (it would crash the verdict via the worker pool). A near-uniform / noisy small frame
    // is exactly the kind of input that can trip it — robust must return cleanly (empty).
    let mut img = GrayImage::from_raw(200, 130, vec![200u8; 200 * 130]).unwrap();
    // Sprinkle a few dark specks so rqrr's finder attempts (and may trip) rather than
    // bailing on a flat field — the panic path this guards.
    for (i, px) in img.pixels_mut().enumerate() {
        if i % 37 == 0 {
            px.0[0] = 0;
        }
    }
    // Must not panic; degenerate input simply yields no payloads.
    let got = decode_qr_luma_all_robust(img);
    assert!(
        got.is_empty(),
        "a degenerate frame yields no payloads (and never panics): {got:?}"
    );
}

/// #1280 RED→GREEN vehicle. The painter's dual-QR + aux canvas carries FOUR crisp QR codes: two
/// 700px primary Vernier halves in the top band plus the two 210px aux marks co-located ~4px
/// apart in the right gap (`y in [745,955]`, issue 1270). The plain full-frame pass wraps ALL of
/// rqrr's `detect_grids` in one `catch_unwind` (`rqrr_decode_all_catch`, #673), so a caught
/// internal panic from a single near-degenerate capstone grouping (the adjacent small aux pair
/// is the "pathologically small" case #673 documents) EMPTIES the whole pass — so
/// `decode_qr_luma_all` misses a code nondeterministically across CI runs (the primary payloads
/// bake a varying `gen_ts_ns`), the #1280 flake
/// (`settled_left_half_payload_is_byte_identical_across_the_next_tick` panicked on its FIRST
/// decode, "run_id 7 frame_id 2 not found"). Full mechanism: `decode_qr_luma_all_robust_optical`.
///
/// This renders canvases with FIXED, diverse primary `gen_ts_ns` seeds (NOT `Instant::now()`),
/// so pass/fail is a deterministic function of the pixels then the rqrr pipeline — the test is
/// never itself flaky. Exact painter composition (`render_qr_dual_bgra` then `blit_aux_tick_bgra`),
/// tick-2 ids (left fresh frame_id 2, right settled frame_id 1). GREEN asserts the robust path
/// (`decode_qr_luma_all_robust_optical` — plain ∪ bottom-tiles ∪ top-band ∪ per-half top crops)
/// recovers all four on every seed. RED was this same loop pinned to `decode_qr_luma_all`; it
/// reproduces the miss only PROBABILISTICALLY across a fixed seed set and cannot be observed
/// locally (probe-gated, Tier-0 #557 — CI is the first compile), so the RED→GREEN commit order is
/// preserved by construction, not by a locally-observed red. A deterministic RED needs the real
/// failing canvas captured as a fixture (`pattern-change-needs-decode-fixture.md`) — mineable only
/// from a CI run, tracked as a follow-up. Seed count is bounded well under the nextest slow-test
/// kill (`.config/nextest.toml`).
#[test]
fn dual_qr_plus_aux_four_code_canvas_decodes_all_four_across_masks_1280() {
    let (w, h, qr) = (1920u32, 1080u32, 700u32);
    let run_id = 7u32;
    let aux = crate::probe::recording_latency::AUX_TICK_RUN_ID;
    for seed in 0..12u64 {
        // Distinct, diverse primary payloads per seed give distinct QR content (the per-run
        // `gen_ts_ns` variation the real flake rides). The aux marks are constant (gen_ts 0),
        // exactly as the painter bakes them.
        let gen_left = 4_096_i64 + seed as i64 * 1_000_003;
        let gen_right = 8_192_i64 + seed as i64 * 999_983;
        let left = Payload {
            run_id,
            frame_id: 2,
            gen_ts_ns: gen_left,
        };
        let right = Payload {
            run_id,
            frame_id: 1,
            gen_ts_ns: gen_right,
        };
        let aux_left = Payload {
            run_id: aux,
            frame_id: 2,
            gen_ts_ns: 0,
        };
        let aux_right = Payload {
            run_id: aux,
            frame_id: 1,
            gen_ts_ns: 0,
        };

        let mut canvas = render_qr_dual_bgra(&left, &right, w, h, qr);
        blit_aux_tick_bgra(&mut canvas, w, h, qr, TOP_MARGIN_PX, &aux_left, &aux_right);
        let luma = bgra_to_luma(&canvas, w, h, w * 4);

        // #1280 GREEN: the maximally-robust path — plain full-frame ∪ bottom-tiles ∪ top-band
        // crop ∪ per-half top crops — recovers every dropped code from a region-isolated look
        // (each primary alone in a top-band half, the aux upscaled in a bottom tile). (RED was
        // this same loop pinned to the plain `decode_qr_luma_all`, whose whole-pass panic drops
        // codes on an unlucky fixed seed.)
        let got = decode_qr_luma_all_robust_optical(luma);
        for want in [&left, &right, &aux_left, &aux_right] {
            assert!(
                got.iter()
                    .any(|p| p.run_id == want.run_id && p.frame_id == want.frame_id),
                "seed {seed}: (run_id {}, frame_id {}) missing from the 4-code canvas — \
                 decoded {} payloads: {:?}",
                want.run_id,
                want.frame_id,
                got.len(),
                got.iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            );
        }
    }
}

// ========================================================================
// #207 — decode_qr_luma_all_fast_then_robust: plain pass first (FAST), robust
// tiled recovery only on a missing burn (ROBUST FALLBACK). The path is asserted via
// the per-call DecodePath return (decode_qr_luma_all_fast_then_robust_pathed) — NO
// global counter, so these run correctly even alongside the concurrent recording
// unit tests that also decode (a process-global counter raced; that was the first
// CI red).
// ========================================================================

/// Composite a node burn (rendered at `qr_px`) into the BOTTOM-LEFT corner of a clean
/// dual-QR frame, returning the luma. Models the #111 layout: big optical dual-QR top,
/// small node burn bottom-corner. `blur` softens the whole frame (the recording chain).
fn dual_with_bottom_burn(
    left: &Payload,
    right: &Payload,
    burn: &Payload,
    burn_px: u32,
    blur: f32,
) -> GrayImage {
    let (w, h) = (1920u32, 1080u32);
    let bgra = render_qr_dual_bgra(left, right, w, h, 700);
    let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
    let qh = render_payload_qr(burn, burn_px).height();
    // Reuse the tests' blit helper (defined in `qr`'s test module).
    blit_burn_luma(&mut luma, burn, burn_px, 40, h - qh - 40);
    if blur > 0.0 {
        image::imageops::blur(&luma, blur)
    } else {
        luma
    }
}

#[test]
fn fast_path_taken_when_plain_already_reads_every_expected_burn() {
    // The required id is one the plain full-frame pass ALWAYS reads — the big optical
    // dual-QR (run_id 7), which decodes full-frame on every well-formed frame (every other
    // decode test relies on this). With the required id already present, the fast gate must
    // SKIP the ~10×-cost tiled recovery: the call reports DecodePath::Fast and the result
    // equals the plain pass (the tiles would add nothing). Using the optical id keeps this
    // DETERMINISTIC — no dependence on whether rqrr's full-frame finder happens to lock a
    // small synthetic burn (the very intermittency #186/#202 is about).
    let left = Payload {
        run_id: 7,
        frame_id: 100,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 101,
        gen_ts_ns: 2,
    };
    let bgra = render_qr_dual_bgra(&left, &right, 1920, 1080, 700);
    let luma = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);

    // Precondition: the plain pass alone already reads the required (optical) id.
    let plain = decode_qr_luma_all(luma.clone());
    assert!(
        plain.iter().any(|p| p.run_id == 7),
        "precondition: the plain pass must read the optical dual-QR (run_id 7): {plain:?}"
    );

    let (got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[7]);
    assert_eq!(
        path,
        DecodePath::Fast,
        "a frame whose expected id the plain pass already read must take the FAST path \
         (no tiled recovery)"
    );
    assert!(
        got.iter().any(|p| p.run_id == 7),
        "fast path still returns the required id: {got:?}"
    );
}

#[test]
fn fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame() {
    // A REAL "burn-unreadable" recording frame (cam1 burn 911001.1727) where the
    // full-frame pass — even #363's plain ∪ Otsu union — genuinely MISSES the node burn
    // (the #186/#202 size-disparity / detector-coverage gap; cv2 reads it from the same
    // pixels, rqrr's full-frame detect_grids does not). The #207 fast gate must detect the
    // missing expected burn, fall back to the robust tiled recovery (DecodePath::Robust),
    // and RECOVER the exact burn. This is the genuine gap that the synthetic perfect-QR
    // model can no longer reproduce after #363 strengthened the full-frame pass — so it is
    // locked here against the real pixels (the verdict's actual input). Cross-checked by
    // tests/burn_fixture_decode.rs.
    const RID: u32 = 911_001; // BURN_RUN_ID_CAM1
    const FID: u32 = 1727;
    let luma = optical_fixture_luma("burn-unreadable/cam1-frame-1148.png");

    // Precondition: even the #363 full-frame Otsu union MISSES this real burn.
    let full = decode_qr_luma_all(luma.clone());
    assert!(
        !full.iter().any(|p| p.run_id == RID && p.frame_id == FID),
        "precondition: the full-frame pass (plain ∪ Otsu) must MISS this real burn \
         ({RID}.{FID}) — the #186 detector-coverage gap; full={:?}",
        full.iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );

    let (got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[RID]);
    assert_eq!(
        path,
        DecodePath::Robust,
        "a frame missing an expected burn from the full-frame pass must take the ROBUST \
         fallback"
    );
    assert!(
        got.iter().any(|p| p.run_id == RID && p.frame_id == FID),
        "the robust fallback must RECOVER the real burn ({RID}.{FID}) — #186 0-miss \
         preserved: {:?}",
        got.iter()
            .map(|p| (p.run_id, p.frame_id))
            .collect::<Vec<_>>()
    );
}

#[test]
fn fast_then_robust_is_identical_to_robust_always() {
    // The optimization changes WHEN the tiles run, never WHAT is read (issue 1370 only adds
    // a burn the tiles still miss; the burn here is read full-frame). On both a clean
    // frame (fast path) and a softened frame (robust fallback) the fast-then-robust
    // result must equal robust-always exactly (order-independent payload set).
    let left = Payload {
        run_id: 7,
        frame_id: 200,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 201,
        gen_ts_ns: 2,
    };
    let burn = Payload {
        run_id: 911_002,
        frame_id: 5555,
        gen_ts_ns: 3,
    };
    let key = |p: &Payload| (p.run_id, p.frame_id, p.gen_ts_ns);
    for blur in [0.0f32, 2.8] {
        let luma = dual_with_bottom_burn(
            &left,
            &right,
            &burn,
            if blur == 0.0 { 360 } else { 260 },
            blur,
        );
        let mut robust = decode_qr_luma_all_robust(luma.clone());
        let mut fast = decode_qr_luma_all_fast_then_robust(luma, &[burn.run_id]);
        robust.sort_by_key(key);
        fast.sort_by_key(key);
        assert_eq!(
            robust, fast,
            "fast-then-robust must read the IDENTICAL set as robust-always (blur={blur})"
        );
    }
}

/// #632 gap 1: the any-of group IS satisfied when the deployed camera's OWN burn (cam3, not
/// cam1) decoded in the plain pass — proving cam3-deployed recordings get the SAME fast path
/// benefit cam1-deployed ones already had, without requiring cam1's (never-emitted) burn.
#[test]
fn grouped_gate_fast_path_when_deployed_camera_is_cam3_not_cam1() {
    const STRIH_ID: u32 = 911_002;
    const CAM1_ID: u32 = 911_001;
    const CAM3_ID: u32 = 911_008;
    let left = Payload {
        run_id: 7,
        frame_id: 400,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 401,
        gen_ts_ns: 2,
    };
    // Composite BOTH the strih burn and cam3's burn into the frame (two distinct node
    // burns, as a real strih recording under cam3 test would carry: cam3's forwarded
    // capture burn + strih's own render burn). `dual_with_bottom_burn` only blits one
    // burn, so blit the camera burn manually where a real one sits: the capture-burn
    // slot, bottom-centre (issue 1367: the recording decode counts a node burn only
    // in its own slot, so a camera burn drawn in the stream corner is an echo).
    let strih_burn = Payload {
        run_id: STRIH_ID,
        frame_id: 1670,
        gen_ts_ns: 3,
    };
    let cam3_burn = Payload {
        run_id: CAM3_ID,
        frame_id: 5000,
        gen_ts_ns: 4,
    };
    let mut luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);
    let (w, h) = (1920u32, 1080u32);
    let cam3_qr = render_payload_qr(&cam3_burn, CAM1_BURN_QR_PX);
    let (qw, qh) = (cam3_qr.width(), cam3_qr.height());
    // The production capture-burn origin (measured actual size, never the requested px):
    // bottom-centre, below the top dual-QR and clear of the strih burn bottom-LEFT.
    let (ox, oy) = cam1_burn_origin(w, h, qw, qh);
    blit_burn_luma(&mut luma, &cam3_burn, CAM1_BURN_QR_PX, ox, oy);

    // Precondition: the plain pass reads BOTH burns, never cam1's (it was never drawn).
    let plain = decode_qr_luma_all(luma.clone());
    assert!(
        plain.iter().any(|p| p.run_id == STRIH_ID),
        "precondition: strih burn must decode: {plain:?}"
    );
    assert!(
        plain.iter().any(|p| p.run_id == CAM3_ID),
        "precondition: cam3 burn must decode: {plain:?}"
    );
    assert!(
        !plain.iter().any(|p| p.run_id == CAM1_ID),
        "precondition: cam1's burn must never appear (cam3 is under test): {plain:?}"
    );

    let (got, path) =
        decode_qr_luma_all_fast_then_robust_grouped_pathed(luma, &[STRIH_ID], &[CAM1_ID, CAM3_ID]);
    assert_eq!(
        path,
        DecodePath::Fast,
        "#632: strih (mandatory) present + cam3 (any-of member) present must take FAST, \
         even though cam1 (another any-of member) never appears in this recording at all"
    );
    assert!(
        got.iter().any(|p| p.run_id == CAM3_ID),
        "fast path still returns cam3's burn: {got:?}"
    );
}

/// #632: when NONE of the any-of group decoded (the deployed camera's burn genuinely missed
/// the plain pass on this frame), the gate must still fall back to ROBUST — the any-of group
/// is not a free pass, it only widens WHICH id counts, never whether recovery still runs.
#[test]
fn grouped_gate_falls_back_to_robust_when_any_of_group_entirely_absent() {
    const STRIH_ID: u32 = 911_002;
    const CAM1_ID: u32 = 911_001;
    const CAM3_ID: u32 = 911_008;
    let left = Payload {
        run_id: 7,
        frame_id: 500,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 501,
        gen_ts_ns: 2,
    };
    let strih_burn = Payload {
        run_id: STRIH_ID,
        frame_id: 1670,
        gen_ts_ns: 3,
    };
    // Only the mandatory strih burn is drawn — no camera burn at all (neither cam1 nor
    // cam3), modeling a frame where the deployed camera's own burn failed to decode.
    let luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);
    let (_got, path) =
        decode_qr_luma_all_fast_then_robust_grouped_pathed(luma, &[STRIH_ID], &[CAM1_ID, CAM3_ID]);
    assert_eq!(
        path,
        DecodePath::Robust,
        "#632: mandatory satisfied but the any-of group is ENTIRELY absent — must still \
         attempt robust recovery (the group only widens which id counts, never skips \
         recovery when none of them are found)"
    );
}

#[test]
fn empty_expected_burns_always_takes_fast_path() {
    // With no expected node burns required, the fast gate is vacuously satisfied — the
    // decode never runs the tiles (a caller with nothing to require — e.g. an optical-only
    // frame — pays only the plain pass).
    let p = Payload {
        run_id: 7,
        frame_id: 1,
        gen_ts_ns: 1,
    };
    let bgra = render_qr_dual_bgra(&p, &p, 1920, 1080, 700);
    let luma = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);
    let (_got, path) = decode_qr_luma_all_fast_then_robust_pathed(luma, &[]);
    assert_eq!(
        path,
        DecodePath::Fast,
        "empty expected-burns set ⇒ always FAST"
    );
}

// ========================================================================
// #707 — the gate's THIRD (optical) completeness dimension. Found while investigating
// #707's residual `all_cambox_continuity` copies/gaps: a frame where the plain pass reads
// every expected node burn fine but MISSES one dual-QR Vernier half took the FAST path
// anyway, silently skipping the #202 robust recovery that (proven above) CAN recover a
// read the plain pass alone misses. See `fast_path_gate_satisfied`'s doc for the full
// reasoning + the #707 ground-truth evidence that this is a coverage gap, not a
// decoder-correctness bug.
// ========================================================================

#[test]
fn fast_path_gate_optical_check_is_a_no_op_when_none() {
    // `min_distinct_optical = None` must reproduce the EXACT pre-#707 gate (burns only) —
    // every existing caller passes `None` via the un-suffixed wrappers and must see no
    // behavior change at all.
    let one_optical = [Payload {
        run_id: 7,
        frame_id: 100,
        gen_ts_ns: 1,
    }];
    assert!(
        fast_path_gate_satisfied(&one_optical, &[], &[], None),
        "no burns required + no optical requirement ⇒ satisfied regardless of optical count"
    );
}

#[test]
fn fast_path_gate_requires_min_distinct_optical_ids_when_specified() {
    // Only ONE distinct id of the optical run_id present — short of the required 2 (the
    // dual-QR Vernier's left+right halves) — the gate must NOT be satisfied even though
    // there are no burns to require at all.
    let one_optical = [Payload {
        run_id: 7,
        frame_id: 100,
        gen_ts_ns: 1,
    }];
    assert!(
        !fast_path_gate_satisfied(&one_optical, &[], &[], Some((7, 2))),
        "only 1 distinct optical id present, 2 required ⇒ NOT satisfied (must retry robust)"
    );
}

#[test]
fn fast_path_gate_satisfied_when_optical_ids_meet_the_minimum() {
    let two_optical = [
        Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        },
        Payload {
            run_id: 7,
            frame_id: 101,
            gen_ts_ns: 1,
        },
    ];
    assert!(
        fast_path_gate_satisfied(&two_optical, &[], &[], Some((7, 2))),
        "2 distinct optical ids present, 2 required ⇒ satisfied"
    );
}

#[test]
fn fast_path_gate_optical_check_ignores_a_repeated_id_and_other_run_ids() {
    // A repeated frame_id (same held id decoded twice, e.g. duplicate rqrr detections) must
    // NOT count as 2 DISTINCT ids; a burn payload (a different run_id entirely) must not
    // count toward the optical requirement either.
    let repeated_plus_burn = [
        Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        },
        Payload {
            run_id: 7,
            frame_id: 100,
            gen_ts_ns: 1,
        },
        Payload {
            run_id: 911_002,
            frame_id: 5555,
            gen_ts_ns: 2,
        },
    ];
    assert!(
        !fast_path_gate_satisfied(&repeated_plus_burn, &[], &[], Some((7, 2))),
        "1 distinct optical id (repeated) + an unrelated burn ⇒ still short of 2: {repeated_plus_burn:?}"
    );
}

#[test]
fn optical_gate_fast_path_when_both_dual_qr_halves_and_burns_already_read() {
    // The common case (#707): a clean frame where the plain pass reads BOTH Vernier halves
    // AND the expected burn — must take FAST, unchanged from before this dimension existed.
    const STRIH_ID: u32 = 911_002;
    let left = Payload {
        run_id: 7,
        frame_id: 600,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 7,
        frame_id: 601,
        gen_ts_ns: 2,
    };
    let strih_burn = Payload {
        run_id: STRIH_ID,
        frame_id: 1670,
        gen_ts_ns: 3,
    };
    let luma = dual_with_bottom_burn(&left, &right, &strih_burn, 360, 0.0);

    // Precondition: the plain pass already reads both optical halves.
    let plain = decode_qr_luma_all(luma.clone());
    let distinct_optical: std::collections::HashSet<u32> = plain
        .iter()
        .filter(|p| p.run_id == 7)
        .map(|p| p.frame_id)
        .collect();
    assert_eq!(
        distinct_optical.len(),
        2,
        "precondition: plain pass must read both Vernier halves: {plain:?}"
    );

    let (got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        luma,
        &[STRIH_ID],
        &[],
        Some((7, 2)),
    );
    assert_eq!(
        path,
        DecodePath::Fast,
        "both optical halves + the mandatory burn already read ⇒ FAST, unchanged by #707"
    );
    assert!(got.iter().any(|p| p.run_id == STRIH_ID));
}

#[test]
fn optical_gate_falls_back_to_robust_when_only_one_dual_qr_half_decodes() {
    // #707's actual defect, reproduced deterministically: render only ONE Vernier QR (the
    // "held" half — the OTHER, freshly-repainting half is entirely absent from this frame,
    // modeling the real-world moiré/shimmer miss). The plain pass finds every expected node
    // burn AND one optical id — the PRE-#707 gate (burns only) would wrongly call this FAST.
    // With the optical dimension wired in, it must fall through to ROBUST instead.
    const STRIH_ID: u32 = 911_002;
    let held = Payload {
        run_id: 7,
        frame_id: 700,
        gen_ts_ns: 1,
    };
    let strih_burn = Payload {
        run_id: STRIH_ID,
        frame_id: 1671,
        gen_ts_ns: 2,
    };
    let (w, h) = (1920u32, 1080u32);
    let bgra = render_qr_bgra(&held, w, h, 700);
    let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
    let qh = render_payload_qr(&strih_burn, 360).height();
    blit_burn_luma(&mut luma, &strih_burn, 360, 40, h - qh - 40);

    // Precondition: exactly ONE distinct optical id decodes in the plain pass (the other
    // Vernier half was never painted onto this frame at all).
    let plain = decode_qr_luma_all(luma.clone());
    let distinct_optical: std::collections::HashSet<u32> = plain
        .iter()
        .filter(|p| p.run_id == 7)
        .map(|p| p.frame_id)
        .collect();
    assert_eq!(
        distinct_optical.len(),
        1,
        "precondition: plain pass must read exactly one Vernier half: {plain:?}"
    );
    assert!(
        plain.iter().any(|p| p.run_id == STRIH_ID),
        "precondition: the mandatory burn must still decode: {plain:?}"
    );

    let (_got, path) = decode_qr_luma_all_fast_then_robust_grouped_pathed_optical(
        luma,
        &[STRIH_ID],
        &[],
        Some((7, 2)),
    );
    assert_eq!(
        path,
        DecodePath::Robust,
        "#707: only 1 of 2 expected optical ids read by the plain pass — even with every \
         burn present, the gate must retry robust instead of silently accepting the short \
         optical read (the pre-#707 gate wrongly returned Fast here)"
    );
}
