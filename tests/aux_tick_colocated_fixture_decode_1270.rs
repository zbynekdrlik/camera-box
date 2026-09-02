//! issue 1270 — the mined REAL-captured-frame fixture proving the CO-LOCATED aux Vernier tick
//! pair (`src/aux_tick.rs`, both marks moved into the RIGHT burn-free gap `[1120, 1578)` by
//! PR #1275) decodes at FULL SIZE through the real projection-leg chain, per
//! `pattern-change-needs-decode-fixture`'s hard promotion precondition for a relocated painted
//! element (recorded on this ticket's own review comment, 🟡-2).
//!
//! The synthetic painter round-trip (`aux_tick_pair_round_trips_alongside_the_dual_qr_1196` in
//! `src/probe/painter.rs`) landed WITH the geometry change and proves geometry + wire format +
//! decoder reach on a crisp canvas — but nothing about the real lossy chain (imag projector →
//! cam2 camera → HDMI splitter → cambox grabber → 2×NDI hops → the stream box's 4K upscale → mp4
//! recording). THIS file is that second, real-chain proof layer.
//!
//! ## Provenance
//!
//! Frames are real 1920×1080 grayscale pixels from E2E run 33640143227 attempt 3 (RUN_ID
//! 255477892, the first green run on 1.7.0-dev.611 — the version that shipped the co-located
//! geometry), mined from **dev1's own box-side pixel-proof retention**
//! (`/tmp/recording-e2e-255477892/stream-partial-255477892-pixels/frame-{2422,2423}.png`) — the
//! E2E harness's own flagged-frame retention, byte-identical to what the production decoder
//! consumed. Zero rig access needed (`pattern-change-needs-decode-fixture.md`'s "Mining the
//! real-frame fixture from the E2E run's OWN retention" method).
//!
//! Both frames come from the run's FIRST CAM2 window (`switch-schedule.json`
//! `[1788365735130655715, 1788365765393795498)`), whose tear stats read `viability=observed`,
//! `aux_decode_fraction≈0.9917`, `tear_fraction≈0.0012` (1/848) — a real, mostly-healthy
//! projection-leg window on the NEW geometry. Each frame's own `stream-partial-255477892.json`
//! entry (the run's OWN production `decode_qr_luma_all_fast_then_robust_grouped_optical` output
//! for these exact pixels) carries a clean single-generation even/odd aux pair (run_id 911013,
//! `gen_ts_ns=0`) matching the primary Vernier pair, WITH imag's own burn (run_id 911003)
//! co-present in the same frame — confirming this is genuinely the CAM2 projection leg (imag's
//! burn only appears there). Independently cross-checked with `zbarimg --raw` (a different
//! decoder, the #186/#921 discriminator): tight (pad=0, i.e. the EXACT 210×210 design rect, no
//! extra quiet-zone margin) crops at the live `aux_tick::aux_tick_rects()` positions decode both
//! marks cleanly on both frames; the SAME crops at the pre-1270 historical rects (LEFT
//! `[466,676)`, RIGHT `[1224,1434)`) decode to NOTHING on either frame — proving the fixture
//! content genuinely reflects the new geometry, not the old one.
//!
//! | fixture                              | even (LEFT) id | odd (RIGHT) id |
//! |---------------------------------------|-----------------|-----------------|
//! | `stream-255477892-frame-2422.png`     | 21752           | 21751           |
//! | `stream-255477892-frame-2423.png`     | 21754           | 21753           |

#![cfg(feature = "probe")]

use camera_box::aux_tick::{aux_tick_rects, DESIGN_H, DESIGN_W};
use camera_box::colour_scale::{Rect, DEFAULT_QR_SIZE, TOP_MARGIN_PX};
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{decode_qr_luma, decode_qr_luma_all_fast_then_robust_grouped_optical};
use camera_box::probe::recording_latency::{
    AUX_TICK_RUN_ID, BURN_RUN_ID_CAM1, BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4,
    BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6, BURN_RUN_ID_CAM7, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use image::GrayImage;
use std::path::PathBuf;

/// The painted primary (cam2 dual-QR Vernier) run_id on this content — the E2E run id itself
/// (`RUN_ID=255477892`). A committed real frame's payloads are immutable history, so pinning the
/// literal is correct here (same convention as the sibling 1196 fixture files).
const PRIMARY_RUN_ID: u32 = 255477892;

/// Each committed fixture + the (even LEFT id, odd RIGHT id) aux pair it carries, per the module
/// doc's provenance table.
const CASES: &[(&str, u32, u32)] = &[
    ("stream-255477892-frame-2422.png", 21752, 21751),
    ("stream-255477892-frame-2423.png", 21754, 21753),
];

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "tear-781",
        name,
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

fn crop(luma: &GrayImage, r: Rect) -> GrayImage {
    image::imageops::crop_imm(luma, r.x, r.y, r.w, r.h).to_image()
}

/// The two aux rects the assertion below decodes at. **GREEN: the LIVE geometry** —
/// `aux_tick::aux_tick_rects()`, the actual production source of truth the painter (and any
/// future geometry change) both flow through. This resolves to `[1137,745,210,210]` (even/LEFT)
/// and `[1351,745,210,210]` (odd/RIGHT) at the rig's canvas/QR-size/margin defaults — the SAME
/// values `src/aux_tick.rs`'s own `canonical_rects_are_the_design_values` test pins. (Was, on the
/// RED commit: the HISTORICAL, pre-de-confliction positions — LEFT `x[466,676)`, RIGHT
/// `x[1224,1434)` — which decode to nothing on this fixture; see git history for that state.)
fn rects_under_test() -> [Rect; 2] {
    aux_tick_rects(DESIGN_W, DESIGN_H, DEFAULT_QR_SIZE, TOP_MARGIN_PX)
        .expect("the rig aux layout must be non-degenerate")
}

/// Both co-located aux marks decode INDIVIDUALLY at FULL SIZE — a tight `crop_imm` at exactly
/// each mark's own design rect (no extra quiet-zone margin — the box already contains it, per
/// `aux_tick.rs`'s module doc) is sufficient for `decode_qr_luma` (single-grid rqrr) to recover
/// the payload. See [`rects_under_test`] for which geometry is under test right now.
#[test]
fn both_colocated_aux_marks_decode_from_the_geometry_under_test_1270() {
    let [even_rect, odd_rect] = rects_under_test();
    for &(name, even_id, odd_id) in CASES {
        let luma = fixture_luma(name);
        let even = decode_qr_luma(crop(&luma, even_rect));
        let odd = decode_qr_luma(crop(&luma, odd_rect));
        assert_eq!(
            even,
            Some(Payload {
                run_id: AUX_TICK_RUN_ID,
                frame_id: even_id,
                gen_ts_ns: 0
            }),
            "{name}: the even (left) aux mark must decode at {even_rect:?}; got {even:?}"
        );
        assert_eq!(
            odd,
            Some(Payload {
                run_id: AUX_TICK_RUN_ID,
                frame_id: odd_id,
                gen_ts_ns: 0
            }),
            "{name}: the odd (right) aux mark must decode at {odd_rect:?}; got {odd:?}"
        );
    }
}

/// The `(run_id, frame_id)` pairs decoded for one run_id, sorted.
fn ids_for(payloads: &[Payload], run_id: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = payloads
        .iter()
        .filter(|p| p.run_id == run_id)
        .map(|p| p.frame_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// The PRODUCTION stream-extraction decode, exactly as `--extract-partial stream` ran it for this
/// run (mandatory = the strih + stream hop burns, any-of = the seven camera-under-test capture
/// burn ids, optical = both Vernier halves of the pinned cam2 run_id) — the SAME function whose
/// own output is already recorded, for these exact pixels, in the run's
/// `stream-partial-255477892.json`.
fn production_stream_decode(luma: GrayImage) -> Vec<Payload> {
    decode_qr_luma_all_fast_then_robust_grouped_optical(
        luma,
        &[BURN_RUN_ID_STRIH, BURN_RUN_ID_STREAM],
        &[
            BURN_RUN_ID_CAM1,
            BURN_RUN_ID_CAM2,
            BURN_RUN_ID_CAM3,
            BURN_RUN_ID_CAM4,
            BURN_RUN_ID_CAM5,
            BURN_RUN_ID_CAM6,
            BURN_RUN_ID_CAM7,
        ],
        Some((PRIMARY_RUN_ID, 2)),
    )
}

/// The full production decode (not a hand-rolled crop) recovers BOTH aux ids and the matching
/// primary Vernier pair from every committed real frame — the same shape as the sibling 1196
/// fixture tests, now proving it for the CO-LOCATED geometry.
#[test]
fn production_decode_reads_both_colocated_aux_ticks_from_every_real_frame_1270() {
    for &(name, even_id, odd_id) in CASES {
        let payloads = production_stream_decode(fixture_luma(name));
        let mut aux = ids_for(&payloads, AUX_TICK_RUN_ID);
        let mut expected = [even_id, odd_id];
        aux.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            aux,
            expected.to_vec(),
            "{name}: the aux tick pair must decode exactly {expected:?}; got aux {aux:?} \
             (all payloads: {payloads:?})"
        );
        for p in payloads.iter().filter(|p| p.run_id == AUX_TICK_RUN_ID) {
            assert_eq!(
                p.gen_ts_ns, 0,
                "{name}: an aux mark always carries the constant gen_ts_ns = 0 (issue 1196 wire \
                 format); got {p:?}"
            );
        }
        let primary = ids_for(&payloads, PRIMARY_RUN_ID);
        assert_eq!(
            primary,
            expected.to_vec(),
            "{name}: the primary dual-QR pair must decode the SAME adjacent tick pair the aux \
             mirrors; got primary {primary:?}"
        );
    }
}
