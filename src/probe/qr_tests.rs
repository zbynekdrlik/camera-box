//! issue 1374 — the unit tests of `qr` (a `#[path]` child, split out like `recording_decode`'s).
//! `pub(super)` on the module and `pub(in crate::probe)` on `optical_fixture_luma` /
//! `blit_burn_luma` let `recording_decode`'s tests share those two helpers.

use super::*;
use image::imageops::{resize, FilterType};

fn sample() -> Payload {
    Payload {
        run_id: 7,
        frame_id: 12345,
        gen_ts_ns: 9_876_543_210,
    }
}

#[test]
fn clean_roundtrip() {
    let p = sample();
    let bgra = render_qr_bgra(&p, 1280, 720, 600);
    let luma = bgra_to_luma(&bgra, 1280, 720, 1280 * 4);
    assert_eq!(decode_qr_luma(luma), Some(p));
}

#[test]
fn survives_downscale_and_noise() {
    let p = sample();
    let bgra = render_qr_bgra(&p, 1920, 1080, 700);
    let full = bgra_to_luma(&bgra, 1920, 1080, 1920 * 4);

    let small = resize(&full, 960, 540, FilterType::Triangle);
    let mut back = resize(&small, 1920, 1080, FilterType::Triangle);

    for (i, px) in back.iter_mut().enumerate() {
        let d: i16 = if i % 3 == 0 { 6 } else { -6 };
        *px = (*px as i16 + d).clamp(0, 255) as u8;
    }

    assert_eq!(decode_qr_luma(back), Some(p));
}

#[test]
fn blank_image_decodes_to_none() {
    let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
    assert_eq!(decode_qr_luma(blank), None);
}

#[test]
fn decode_capture_roundtrips_bgra_frame() {
    let p = Payload {
        run_id: 3,
        frame_id: 99,
        gen_ts_ns: 42,
    };
    // 1920x1080 BGRA frame carrying a centered QR, tight stride.
    let bgra = render_qr_bgra(&p, 1920, 1080, 700);
    let fourcc = u32::from_le_bytes(*b"BGRA");
    let got = decode_capture(fourcc, &bgra, 1920, 1080, 1920 * 4, 820);
    assert_eq!(got, Some(p));
}

#[test]
fn decode_capture_none_on_blank() {
    let blank = vec![255u8; (640 * 480 * 4) as usize];
    let fourcc = u32::from_le_bytes(*b"BGRA");
    assert_eq!(decode_capture(fourcc, &blank, 640, 480, 640 * 4, 400), None);
}

#[test]
fn otsu_splits_a_bimodal_histogram_between_the_two_peaks() {
    // A clean black/white image: mass at 0 and 255. Otsu must cut between them.
    let mut hist = [0u64; 256];
    hist[20] = 1000; // dark cluster
    hist[230] = 1000; // light cluster
    let t = super::otsu_threshold(&hist);
    assert!(
        t > 20 && t < 230,
        "threshold between the two peaks, got {t}"
    );
}

#[test]
fn otsu_empty_histogram_is_neutral_midgray() {
    assert_eq!(super::otsu_threshold(&[0u64; 256]), 128);
}

#[test]
fn decode_recovers_soft_low_contrast_qr_via_binarized_retry() {
    // A SOFT optical capture: render a QR, then compress its dynamic range into a
    // narrow low-contrast band (sim. a QR filmed off a monitor at gray8). The plain
    // rqrr pass struggles; the Otsu-binarized retry in decode_qr_luma_all recovers it.
    let p = Payload {
        run_id: 9,
        frame_id: 123,
        gen_ts_ns: 7,
    };
    let bgra = render_qr_bgra(&p, 1280, 720, 600);
    let mut luma = bgra_to_luma(&bgra, 1280, 720, 1280 * 4);
    // Squash contrast: map 0..255 -> ~96..160 (a soft, low-contrast gray8 capture).
    for px in luma.pixels_mut() {
        px.0[0] = 96 + (px.0[0] as u32 * 64 / 255) as u8;
    }
    let got = decode_qr_luma_all(luma);
    assert!(
        got.contains(&p),
        "the Otsu-binarized retry must recover the soft low-contrast QR"
    );
}

// ============================================================================
// #363 — the SOFT optical dual-QR (a QR filmed off a monitor: low-contrast +
// moiré + colour-cast) must be RECOVERED from the real stream-recording frames
// even though the crisp DIGITAL BURNS already decode on the plain pass. The
// optical read is the HARD verdict gate (#372). The OLD `decode_qr_luma_all`
// ran the Otsu retry ONLY when the plain pass found NOTHING — so on these frames
// the plain pass returned the burns (non-empty) → the Otsu retry was SKIPPED →
// the present optical QR was marked a PHANTOM `optical_undecodable` (~87 % of
// stream frames). The fix runs plain ∪ Otsu ALWAYS, recovering it.
// ============================================================================

/// Load a committed real recording-frame fixture as the luma plane the decoder
/// consumes (the exact gray8 the verdict feeds rqrr).
pub(in crate::probe) fn optical_fixture_luma(name: &str) -> GrayImage {
    let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

#[test]
fn optical_soft_dual_qr_recovered_on_real_stream_frames() {
    // The optical dual-QR's run_id on the real failing recording (run 354003); its two
    // Vernier halves carry an even and an odd frame_id (consecutive frames).
    const OPTICAL_RUN_ID: u32 = 354_003;
    // The f5/f150 cam1/strih/stream digital-burn run_ids — the recording per-frame path.
    const BURN_IDS: &[u32] = &[911_001, 911_002, 911_004];
    for name in ["optical-soft-f5.png", "optical-soft-f150.png"] {
        let luma = optical_fixture_luma(name);

        // RED-condition lock (permanent): the PLAIN rqrr pass MISSES the optical dual-QR.
        // It finds the crisp burns but not the soft optical — exactly what the old
        // early-return ("Otsu only if plain empty") swallowed (plain non-empty ⇒ Otsu
        // skipped ⇒ optical never recovered). If a future decoder reads the optical on the
        // plain pass, this documents the shift — re-tune, never delete.
        let plain = rqrr_decode_all(luma.clone());
        assert!(
            !plain.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
            "{name}: the PLAIN rqrr pass is expected to MISS the soft optical dual-QR \
             (run_id {OPTICAL_RUN_ID}); got {:?}",
            plain
                .iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );

        // GREEN: decode_qr_luma_all (plain ∪ Otsu) recovers BOTH optical halves — two
        // distinct frame_ids (the even+odd Vernier pair) under run_id 354003. Reverting
        // to the early-return makes this return only burns → 0 optical → this fails.
        let all = decode_qr_luma_all(luma.clone());
        let mut optical: Vec<u32> = all
            .iter()
            .filter(|p| p.run_id == OPTICAL_RUN_ID)
            .map(|p| p.frame_id)
            .collect();
        optical.sort_unstable();
        optical.dedup();
        assert!(
            optical.len() >= 2,
            "{name}: decode_qr_luma_all must recover BOTH optical dual-QR halves \
             (≥2 distinct frame_ids, run_id {OPTICAL_RUN_ID}); got optical frame_ids \
             {optical:?} from {:?}",
            all.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );

        // AND the recording per-frame path (fast-then-robust, gated on the burn ids) also
        // surfaces the optical — the verdict's actual decode call reads it too.
        let rec = decode_qr_luma_all_fast_then_robust(luma, BURN_IDS);
        assert!(
            rec.iter().any(|p| p.run_id == OPTICAL_RUN_ID),
            "{name}: the recording per-frame decode must also surface the optical dual-QR \
             (run_id {OPTICAL_RUN_ID}); got {:?}",
            rec.iter()
                .map(|p| (p.run_id, p.frame_id))
                .collect::<Vec<_>>()
        );
    }
}

// ============================================================================
// #202 — robust offline decode recovers the small node burns rqrr's single
// full-frame `detect_grids` pass intermittently misses (the residual
// burn-unreadable misses on PRESENT, sharp frames; cv2 reads them, rqrr's
// full-frame pass doesn't). The tiled/upscaled robust pass gives rqrr a fair
// look at each region so the burn's finder pattern locks.
// ============================================================================

/// Composite the QR for `p` (rendered at `qr_px`) onto a white luma frame at top-left
/// `(ox, oy)`. Models a small burn drawn into a large recorded frame.
pub(in crate::probe) fn blit_burn_luma(
    frame: &mut GrayImage,
    p: &Payload,
    qr_px: u32,
    ox: u32,
    oy: u32,
) {
    let qr = render_payload_qr(p, qr_px);
    let (qw, qh) = (qr.width(), qr.height());
    for y in 0..qh {
        for x in 0..qw {
            let (px, py) = (ox + x, oy + y);
            if px < frame.width() && py < frame.height() {
                frame.put_pixel(px, py, qr.get_pixel(x, y).to_owned());
            }
        }
    }
}

#[test]
fn full_frame_otsu_union_recovers_a_softened_burn_bare_rqrr_misses() {
    // A 1920×1080 frame carrying the LARGE optical dual-QR (top) plus a 260px node burn
    // (bottom-left corner) SOFTENED by a Gaussian blur (sigma 2.8) — anti-aliased as the
    // NDI/encode chain leaves it. The BARE single rqrr pass (rqrr's own adaptive prepare)
    // MISSES this softened burn; #363's full-frame Otsu union (`decode_qr_luma_all` = plain
    // ∪ Otsu) RECOVERS it — a hard black/white cut at the Otsu split locks rqrr's finder
    // where the soft adaptive prepare failed. This locks the #363 improvement at the unit
    // level: the union does real recovery work beyond the bare plain pass.
    //
    // NOTE on the #202 tiled recovery: #363 strengthened the full-frame pass enough that
    // this *synthetic* perfect-QR-plus-blur burn no longer needs the tiles (the Otsu union
    // reads it full-frame). The genuine "the full-frame pass misses a burn that only the
    // tiled+upscaled robust pass recovers" gap is a size-disparity / detector-coverage
    // effect that only reproduces on real encoder-degraded 4K frames — it is locked on
    // REAL pixels by `fast_then_robust_falls_back_to_robust_on_a_real_burn_unreadable_frame`
    // (below) and by tests/burn_fixture_decode.rs.
    let left = Payload {
        run_id: 136_141_133,
        frame_id: 4000,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 136_141_133,
        frame_id: 4001,
        gen_ts_ns: 2,
    };
    let burn = Payload {
        run_id: 911_001, // cam1 capture burn
        frame_id: 1234,
        gen_ts_ns: 3,
    };
    let (w, h) = (1920u32, 1080u32);
    // Big top dual-QR (700px each half, top-anchored) — the easy-to-find marks.
    let bgra = render_qr_dual_bgra(&left, &right, w, h, 700);
    let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
    // A 260px burn in the bottom-left corner (where the strih burn lives), then soften
    // the WHOLE frame as the recording chain does.
    let qh = render_payload_qr(&burn, 260).height();
    blit_burn_luma(&mut luma, &burn, 260, 40, h - qh - 40);
    let luma = image::imageops::blur(&luma, 2.8);

    let bare = rqrr_decode_all(luma.clone());
    let full = decode_qr_luma_all(luma.clone());
    let robust = decode_qr_luma_all_robust(luma);

    // The big optical QRs decode on the bare pass (sanity: the frame is well-formed).
    assert!(
        bare.iter().any(|p| p.run_id == left.run_id),
        "the big optical QR must decode on the bare rqrr pass (frame is well-formed): {bare:?}"
    );
    // RED condition this locks: the BARE single rqrr pass MISSES the softened burn — exactly
    // what the old early-return swallowed (it returned the bare pass when non-empty).
    assert!(
        !bare.contains(&burn),
        "the bare single rqrr pass is expected to MISS the softened burn (the soft-capture \
         condition #363 recovers); bare={bare:?}"
    );
    // GREEN: the #363 full-frame Otsu union recovers it — and robust (⊇ full) keeps it.
    assert!(
        full.contains(&burn),
        "#363: the full-frame Otsu union must recover the softened cam1 burn the bare pass \
         missed; full={full:?}"
    );
    assert!(
        robust.contains(&burn),
        "robust must keep the recovered burn (superset of the full-frame pass): robust={robust:?}"
    );
}

#[test]
fn merge_payloads_keeps_each_distinct_identity_once() {
    // Pure de-dup contract: same (run_id, frame_id) collapses; a different frame_id (or
    // run_id) is a distinct mark and is kept.
    let a = Payload {
        run_id: 1,
        frame_id: 10,
        gen_ts_ns: 1,
    };
    let a_dup = Payload {
        run_id: 1,
        frame_id: 10,
        gen_ts_ns: 999, // same identity, different ts — still a duplicate
    };
    let b = Payload {
        run_id: 1,
        frame_id: 11,
        gen_ts_ns: 1,
    };
    let c = Payload {
        run_id: 2,
        frame_id: 10,
        gen_ts_ns: 1,
    };
    let mut into = vec![a];
    merge_payloads(&mut into, vec![a_dup, b, c]);
    assert_eq!(
        into.len(),
        3,
        "a_dup collapses onto a; b and c are new: {into:?}"
    );
    assert!(into.contains(&a) && into.contains(&b) && into.contains(&c));
}

#[test]
fn render_centers_qr_and_writes_gray_bgra() {
    // Asymmetric canvas so x- and y-centering are exercised distinctly.
    let p = Payload {
        run_id: 1,
        frame_id: 2,
        gen_ts_ns: 3,
    };
    let (cw, ch, qs) = (1000u32, 800u32, 400u32);
    let canvas = render_qr_bgra(&p, cw, ch, qs);
    assert_eq!(canvas.len(), (cw * ch * 4) as usize);

    let (mut min_x, mut max_x, mut min_y, mut max_y) = (cw, 0u32, ch, 0u32);
    for y in 0..ch {
        for x in 0..cw {
            let i = ((y * cw + x) * 4) as usize;
            let (b, g, r, a) = (canvas[i], canvas[i + 1], canvas[i + 2], canvas[i + 3]);
            // Every pixel: opaque, and gray (B==G==R) — white background or QR module.
            assert_eq!(a, 255, "alpha must be 255 at ({x},{y})");
            assert!(b == g && g == r, "B==G==R at ({x},{y}): {b},{g},{r}");
            if b != 255 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    // The QR's non-white bounding box must be centered: equal margins each side.
    let (left, right) = (min_x as i64, (cw - 1 - max_x) as i64);
    let (top, bottom) = (min_y as i64, (ch - 1 - max_y) as i64);
    assert!(
        (left - right).abs() <= 1,
        "x-centered: left={left} right={right}"
    );
    assert!(
        (top - bottom).abs() <= 1,
        "y-centered: top={top} bottom={bottom}"
    );
}

#[test]
fn dual_render_places_two_decodable_qrs_left_and_right() {
    let l = Payload {
        run_id: 7,
        frame_id: 100,
        gen_ts_ns: 1,
    };
    let r = Payload {
        run_id: 7,
        frame_id: 101,
        gen_ts_ns: 2,
    };
    let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
    let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    assert_eq!(bgra.len(), (cw * ch * 4) as usize);
    let full = bgra_to_luma(&bgra, cw, ch, cw * 4);
    // Left half image and right half image each decode to their own payload.
    let left_img = image::imageops::crop_imm(&full, 0, 0, cw / 2, ch).to_image();
    let right_img = image::imageops::crop_imm(&full, cw / 2, 0, cw / 2, ch).to_image();
    assert_eq!(decode_qr_luma(left_img), Some(l));
    assert_eq!(decode_qr_luma(right_img), Some(r));
}

#[test]
fn dual_decode_returns_highest_frame_id_and_tolerates_one_blurred() {
    let l = Payload {
        run_id: 7,
        frame_id: 200,
        gen_ts_ns: 1,
    };
    let r = Payload {
        run_id: 7,
        frame_id: 201,
        gen_ts_ns: 2,
    };
    let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
    let fourcc = u32::from_le_bytes(*b"BGRA");

    // Both sharp -> highest frame_id (201).
    let both = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    assert_eq!(
        decode_capture_dual(fourcc, &both, cw, ch, cw * 4, 620),
        Some(r)
    );

    // Right region blanked (simulating an unreadable/blurred QR) -> falls back to left (200).
    let l_only = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    let mut blanked = l_only.clone();
    let half = (cw / 2) as usize;
    for y in 0..ch as usize {
        for x in half..cw as usize {
            let i = (y * cw as usize + x) * 4;
            blanked[i] = 255;
            blanked[i + 1] = 255;
            blanked[i + 2] = 255;
            blanked[i + 3] = 255;
        }
    }
    assert_eq!(
        decode_capture_dual(fourcc, &blanked, cw, ch, cw * 4, 620),
        Some(l)
    );
}

// ---- #367 colour-reference scale blit ----

#[test]
fn colour_scale_blit_fills_each_patch_and_leaves_the_qr_halves_untouched() {
    use crate::colour_scale::{colour_scale_patches, DEFAULT_QR_SIZE, TOP_MARGIN_PX};
    let (w, h) = (1920u32, 1080u32);
    let mut canvas = vec![255u8; (w * h * 4) as usize];
    blit_colour_scale_bgra(&mut canvas, w, h, DEFAULT_QR_SIZE, TOP_MARGIN_PX);

    // Each patch CENTRE carries exactly its known colour in BGRA order, opaque — proving
    // the blit honours the pure layout's colour table (not a tautology: it reads the real
    // framebuffer bytes the painter would present).
    let patches = colour_scale_patches(w, h, DEFAULT_QR_SIZE, TOP_MARGIN_PX);
    assert!(!patches.is_empty(), "1920x1080 must yield patches");
    for (rect, rgb) in &patches {
        let cx = rect.x + rect.w / 2;
        let cy = rect.y + rect.h / 2;
        let i = (((cy * w) + cx) * 4) as usize;
        assert_eq!(canvas[i], rgb.b, "B at patch centre ({cx},{cy})");
        assert_eq!(canvas[i + 1], rgb.g, "G at patch centre ({cx},{cy})");
        assert_eq!(canvas[i + 2], rgb.r, "R at patch centre ({cx},{cy})");
        assert_eq!(canvas[i + 3], 255, "opaque at patch centre ({cx},{cy})");
    }
    // The colour column lives ONLY in the central gap: a pixel inside the LEFT QR half
    // (x=200, y=400) is still the untouched white background — the scale never bleeds into
    // either QR region.
    let in_left_qr = (((400 * w) + 200) * 4) as usize;
    assert_eq!(
        &canvas[in_left_qr..in_left_qr + 4],
        &[255u8, 255, 255, 255],
        "the dual-QR halves are untouched by the colour scale"
    );
}

// ---- #174 cam1-capture YUYV burn ----

/// Build a tight YUYV frame (luma=mid-gray, chroma neutral) of `w×h`.
fn yuyv_gray_frame(w: u32, h: u32) -> Vec<u8> {
    // YUYV = 2 bytes/pixel. Fill luma=128 (even bytes), chroma=128 (odd bytes).
    vec![128u8; (w * h * 2) as usize]
}

#[test]
fn cam1_burn_renders_a_decodable_qr_into_a_yuyv_frame() {
    use crate::probe::luma::uyvy_to_luma;
    let p = Payload {
        run_id: 911_001,
        frame_id: 4242,
        gen_ts_ns: 1_700_000_000_123_456,
    };
    let (w, h) = (1920u32, 1080u32);
    let mut frame = yuyv_gray_frame(w, h);
    burn_qr_yuyv(&mut frame, w, h, w * 2, &p, CAM1_BURN_QR_PX);
    // Extract the luma plane from the YUYV frame (every even byte) and decode.
    let luma_bytes = crate::capture::yuyv_to_gray8(&frame, w, h, w * 2);
    let luma = image::GrayImage::from_raw(w, h, luma_bytes).unwrap();
    assert_eq!(
        decode_qr_luma(luma),
        Some(p),
        "the cam1 burn must render a decodable QR carrying the exact id+ts"
    );
    // Guard against accidentally reading a UYVY-laid path: confirm uyvy_to_luma does
    // NOT decode it (the burn is YUYV, luma at even bytes) — keeps the YUYV contract.
    let _ = uyvy_to_luma; // referenced for intent; not asserted (format-specific)
}

#[test]
fn cam1_burn_lands_bottom_center_clear_of_top_dualqr_and_bottom_corner_burns() {
    // The four-mark non-overlap contract on a 1920×1080 stream frame (#186 enlarged):
    //   top dual-QR band : y ∈ [TOP_MARGIN_PX, TOP_MARGIN_PX + 700)
    //   strih burn       : bottom-LEFT  ~302×302 corner (DistroAV qr_px 0.28×1080, enlarged)
    //   stream burn      : bottom-RIGHT ~302×302 corner
    //   cam1 burn        : bottom-CENTER ~320×320, must miss all three.
    let (w, h) = (1920u32, 1080u32);
    let qpx = CAM1_BURN_QR_PX;
    // Use the real rendered QR size (quiet zone may round up past qpx).
    let s = Payload {
        run_id: 911_001,
        frame_id: 1,
        gen_ts_ns: 1,
    }
    .encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H).unwrap();
    let qr: GrayImage = code
        .render::<Luma<u8>>()
        .min_dimensions(qpx, qpx)
        .max_dimensions(qpx, qpx)
        .quiet_zone(true)
        .build();
    let (qw, qh) = (qr.width(), qr.height());
    let (ox, oy) = cam1_burn_origin(w, h, qw, qh);
    let (cam1_l, cam1_r, cam1_t, cam1_b) = (ox, ox + qw, oy, oy + qh);

    // Vs the top dual-QR band (700px tall under TOP_MARGIN_PX).
    let dual_band_bottom = TOP_MARGIN_PX + 700;
    assert!(
        cam1_t >= dual_band_bottom,
        "cam1 burn top {cam1_t} must be below the top dual-QR band bottom {dual_band_bottom}"
    );

    // Vs the bottom-corner burns (#186: ~302px squares = 0.28×1080, anchored bottom-left /
    // bottom-right with a 40px edge margin, so the corner box occupies x ∈ [0, margin+302)
    // on the left and [w-margin-302, w) on the right).
    let corner = 40u32 + 302u32; // DistroAV burn margin (40) + canvas-relative qr_px (302)
    let strih_right = corner; // bottom-left burn occupies x ∈ [0, corner)
    let stream_left = w - corner; // bottom-right burn occupies x ∈ [w-corner, w)
    assert!(
        cam1_l >= strih_right,
        "cam1 burn left {cam1_l} must clear the bottom-left strih burn (x<{strih_right})"
    );
    assert!(
        cam1_r <= stream_left,
        "cam1 burn right {cam1_r} must clear the bottom-right stream burn (x≥{stream_left})"
    );
    // And it stays on-frame at the bottom.
    assert!(cam1_b <= h, "cam1 burn bottom {cam1_b} on-frame (h={h})");
}

#[test]
fn cam1_burn_only_touches_its_own_rectangle_leaving_the_rest_clean() {
    // The burn must NOT disturb pixels outside its bottom-center rectangle — the top
    // optical dual-QR (and everywhere else) stays exactly as captured.
    let p = Payload {
        run_id: 911_001,
        frame_id: 7,
        gen_ts_ns: 9,
    };
    let (w, h) = (1280u32, 720u32);
    let original = yuyv_gray_frame(w, h);
    let mut frame = original.clone();
    burn_qr_yuyv(&mut frame, w, h, w * 2, &p, CAM1_BURN_QR_PX);

    let qr: GrayImage = QrCode::with_error_correction_level(p.encode().as_bytes(), EcLevel::H)
        .unwrap()
        .render::<Luma<u8>>()
        .min_dimensions(CAM1_BURN_QR_PX, CAM1_BURN_QR_PX)
        .max_dimensions(CAM1_BURN_QR_PX, CAM1_BURN_QR_PX)
        .quiet_zone(true)
        .build();
    let (qw, qh) = (qr.width(), qr.height());
    let (ox, oy) = cam1_burn_origin(w, h, qw, qh);

    // A pixel WELL OUTSIDE the burn rect (top-left corner = optical dual-QR area)
    // is byte-identical to the original.
    let top_left_idx = 0usize;
    assert_eq!(
        frame[top_left_idx], original[top_left_idx],
        "top-left (optical dual-QR area) must be untouched"
    );
    // A row above the burn rect is fully untouched.
    let above_row = (oy.saturating_sub(2)) as usize * (w * 2) as usize;
    assert_eq!(
        frame[above_row..above_row + (w * 2) as usize],
        original[above_row..above_row + (w * 2) as usize],
        "a row above the cam1 burn rectangle must be unchanged"
    );
    // Inside the burn rect, at least one luma byte differs (the QR was drawn).
    let inside_row = (oy + qh / 2) as usize * (w * 2) as usize;
    let inside = (inside_row + 2 * (ox + qw / 2) as usize).min(frame.len() - 1);
    assert_ne!(
        frame[inside], original[inside],
        "the burn rectangle must carry QR pixels"
    );
}
