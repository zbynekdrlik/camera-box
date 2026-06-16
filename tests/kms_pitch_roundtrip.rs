//! #79: the DRM/KMS presenter copies a tightly-packed BGRA QR frame into a dumb
//! buffer whose scanline `pitch` is padded wider than `width*4` (i915 dumb BOs
//! commonly round the pitch up to a 256-byte stride). If the stride math is
//! wrong the QR lands skewed/sheared in the scanned-out buffer and the capture
//! can no longer decode it — surfacing as spurious "loss". These tests pin the
//! copy: a real rendered QR survives the tight AND the padded-pitch path
//! byte-for-byte, so the page-flipped buffer is always a decodable QR.
//!
//! Pure-logic guard (no DRM hardware): `copy_bgra_into` is exactly what the live
//! `KmsPresenter::present` uses to fill each back buffer, so verifying it here
//! proves the on-rig copy without needing a real card.

use camera_box::probe::kms::{copy_bgra_into, copy_plan, CopyPlan};
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{decode_qr_luma, render_qr_bgra};
use image::GrayImage;

/// Extract a tightly-packed BGRA frame back out of a padded-pitch buffer — the
/// inverse of `copy_bgra_into`, modelling what the capture reads from the
/// scanned-out dumb buffer (it sees `width*4` bytes per scanline, pitch stride).
fn unpad(buf: &[u8], width: u32, height: u32, pitch: u32) -> Vec<u8> {
    let row = (width * 4) as usize;
    let pitch = pitch as usize;
    let mut out = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        out[y * row..(y + 1) * row].copy_from_slice(&buf[y * pitch..y * pitch + row]);
    }
    out
}

fn bgra_to_gray(bgra: &[u8], width: u32, height: u32) -> GrayImage {
    // The QR canvas is gray (B==G==R), so the blue channel is the luma.
    let mut g = vec![0u8; (width * height) as usize];
    for (i, px) in g.iter_mut().enumerate() {
        *px = bgra[i * 4];
    }
    GrayImage::from_raw(width, height, g).expect("gray image")
}

#[test]
fn padded_pitch_preserves_qr_decode() {
    // A width whose tight stride (w*4) is NOT a multiple of 256, so rounding up
    // to a 256-byte dumb-buffer stride genuinely PADS — exercising the per-row
    // copy path the way an i915 dumb BO does for an odd resolution.
    let (w, h, qr) = (1900u32, 1080u32, 700u32);
    let payload = Payload {
        run_id: 42,
        frame_id: 123_456,
        gen_ts_ns: 9_999,
    };
    let src = render_qr_bgra(&payload, w, h, qr);

    // i915-style padded pitch: round width*4 (7600) up to a 256-byte stride (7680).
    let tight = (w * 4) as usize;
    let pitch = (tight.div_ceil(256) * 256) as u32;
    assert!(pitch > w * 4, "test must exercise a PADDED pitch");
    assert_eq!(
        copy_plan(src.len(), w, h, pitch).unwrap(),
        CopyPlan::PerRow,
        "padded pitch must take the per-row copy path"
    );

    // Fill a padded dumb buffer exactly as the live presenter does.
    let mut dumb = vec![0u8; pitch as usize * h as usize];
    copy_bgra_into(&mut dumb, &src, w, h, pitch).unwrap();

    // What the capture scans out: the tight frame back out of the padded stride.
    let scanned = unpad(&dumb, w, h, pitch);
    let gray = bgra_to_gray(&scanned, w, h);
    assert_eq!(
        decode_qr_luma(gray),
        Some(payload),
        "QR copied through a padded dumb-buffer pitch must still decode to the painted id"
    );
}

#[test]
fn tight_pitch_preserves_qr_decode() {
    let (w, h, qr) = (1280u32, 720u32, 600u32);
    let payload = Payload {
        run_id: 7,
        frame_id: 1,
        gen_ts_ns: 0,
    };
    let src = render_qr_bgra(&payload, w, h, qr);

    let pitch = w * 4; // tight — the contiguous fast path
    assert_eq!(
        copy_plan(src.len(), w, h, pitch).unwrap(),
        CopyPlan::Contiguous
    );
    let mut dumb = vec![0u8; pitch as usize * h as usize];
    copy_bgra_into(&mut dumb, &src, w, h, pitch).unwrap();
    // Contiguous: the dumb buffer IS the tight frame.
    assert_eq!(dumb, src);

    let gray = bgra_to_gray(&dumb, w, h);
    assert_eq!(decode_qr_luma(gray), Some(payload));
}
