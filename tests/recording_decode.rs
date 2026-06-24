//! Regression test for #106: the recorded-file → ffmpeg → rqrr analysis
//! entrypoint must decode a strih-6519-type frame (two sharp QRs side by side)
//! to BOTH QRs. This is the foundation for the #105/#107 loss/delivery verdict,
//! which counts ONLY from the recorded OBS program-output file — never an NDI tap
//! or the lz4 spool.
//!
//! The committed fixture `tests/fixtures/strih-6519-dual-qr.mkv` is a 1-frame
//! lossless (ffv1, gray8) clip carrying two sharp side-by-side QRs rendered by the
//! repo's own dual-QR painter (`render_qr_dual_bgra`): left = even tick
//! (frame_id 6518), right = odd tick (frame_id 6519). opencv `detectAndDecodeMulti`
//! returned 0 on the real strih frame 6519; the rqrr path MUST return 2.
//!
//! This drives the FULL entrypoint (spawn ffmpeg to emit gray8 rawvideo at native
//! resolution → feed each frame's luma into the existing rqrr decoder), so it
//! requires `ffmpeg`/`ffprobe` on PATH (a documented runtime dependency of
//! camera-box, installed in CI).

#![cfg(feature = "probe")]

use camera_box::capture::yuyv_to_gray8;
use camera_box::probe::luma::bgra_to_luma;
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{render_qr_bgra, render_qr_dual_bgra};
use camera_box::probe::recording::analyze_recording;
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect()
}

/// FIX 1 round-trip (#156 regression lock): a SHARP dual-QR gray8 frame must survive the
/// FULL cam1 grab-record store→decode path — the EXACT luma extraction the camera-box
/// `--record-grab` mode performs (`capture::yuyv_to_gray8`), through ffmpeg ffv1 (the dev1
/// listener), back through `analyze_recording` — and STILL decode BOTH QRs.
///
/// This locks the live-rig finding that the cam1 grab decodes ~100% when the camera is sharp:
/// the prior "~5% decode" was NOT the gray8 plane, the ffv1 params, or the decoder (a live
/// re-capture decoded 846/846 frames both-QR), it was the device losing its certified picture
/// controls on restart (fixed durably in `VideoCapture::open_with_controls`). A future
/// regression in any of those store→decode steps now fails CI here, not silently on the rig.
/// Requires ffmpeg (a documented camera-box runtime dependency, installed in CI).
#[test]
fn sharp_dual_qr_gray8_round_trips_through_grab_record_store_and_decode() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        // ffmpeg is a HARD dependency of the recording path; its absence is an environment
        // fault, surfaced loudly — NOT a silent skip of the assertion logic.
        panic!("ffmpeg not found — the grab-record round-trip cannot run; install ffmpeg");
    }

    // 1) Render a SHARP dual-QR luma frame (what cam1 films off the cam2 monitor).
    let (w, h, qs) = (960u32, 540u32, 260u32);
    let left = Payload {
        run_id: 6519,
        frame_id: 424,
        gen_ts_ns: 1,
    };
    let right = Payload {
        run_id: 6519,
        frame_id: 425,
        gen_ts_ns: 2,
    };
    let bgra = render_qr_dual_bgra(&left, &right, w, h, qs);
    let luma = bgra_to_luma(&bgra, w, h, w * 4);
    assert_eq!(luma.dimensions(), (w, h));

    // 2) Pack that luma into a YUYV buffer (luma in even bytes, neutral 128 chroma in odd
    //    bytes) — the on-the-wire capture format — then run the EXACT grab extraction the
    //    --record-grab path uses. The extracted gray8 MUST be byte-identical to the source
    //    luma (the path is lossless).
    let stride = (2 * w) as usize;
    let mut yuyv = vec![128u8; stride * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            yuyv[y * stride + 2 * x] = luma.get_pixel(x as u32, y as u32)[0];
        }
    }
    let gray = yuyv_to_gray8(&yuyv, w, h, stride as u32);
    assert_eq!(
        gray.as_slice(),
        luma.as_raw().as_slice(),
        "grab gray8 extraction must be lossless for a tightly-packed YUYV frame"
    );

    // 3) Encode the gray8 plane (3 identical frames) to a lossless ffv1 mkv EXACTLY as
    //    scripts/recording-e2e.sh's dev1 listener does (gray rawvideo → ffv1).
    let dir = std::env::temp_dir().join(format!("grab-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let raw = dir.join("frames.gray8");
    let mkv = dir.join("cam1.mkv");
    let mut raw_bytes = Vec::with_capacity(gray.len() * 3);
    for _ in 0..3 {
        raw_bytes.extend_from_slice(&gray);
    }
    std::fs::write(&raw, &raw_bytes).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-s"])
        .arg(format!("{w}x{h}"))
        .args(["-r", "30", "-i"])
        .arg(&raw)
        .args(["-c:v", "ffv1", "-level", "3"])
        .arg(&mkv)
        .status()
        .expect("spawn ffmpeg ffv1 encode");
    assert!(status.success(), "ffmpeg ffv1 encode failed");

    // 4) Decode it back through the SAME analyze_recording the 4-node verdict uses — EVERY
    //    frame must carry BOTH dual-QR halves (the sharp-grab 100% guarantee).
    let frames = analyze_recording(&mkv).expect("analyze the round-tripped recording");
    assert!(!frames.is_empty(), "decoded at least one frame");
    for f in &frames {
        assert_eq!(
            f.payloads.len(),
            2,
            "every sharp gray8 frame decodes BOTH QRs through the grab-record path: {:?}",
            f.payloads
        );
        let mut ids: Vec<u32> = f.payloads.iter().map(|p| p.frame_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![424, 425], "both painted ticks recovered");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strih_6519_dual_qr_frame_decodes_both_qrs() {
    let path = fixture("strih-6519-dual-qr.mkv");
    let frames = analyze_recording(&path).expect("analyze_recording on fixture mkv");

    // The fixture is exactly one frame.
    assert_eq!(frames.len(), 1, "fixture is a single-frame clip");

    let frame = &frames[0];
    // The strih-6519 acceptance bar: a 2-sharp-QR frame MUST decode to 2 (the
    // exact case opencv silently returned 0/1 on). No false-negatives.
    assert_eq!(
        frame.payloads.len(),
        2,
        "strih-6519-type frame must decode to BOTH QRs (got {:?})",
        frame.payloads
    );

    // Both painted ticks are present (left even 6518, right odd 6519); the Vernier
    // effective tick is the max of the two.
    let mut ids: Vec<u32> = frame.payloads.iter().map(|p| p.frame_id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![6518, 6519], "both painted QR ticks present");
    assert_eq!(frame.tick, Some(6519), "Vernier tick = max(left, right)");
}

/// #202 END-TO-END: the offline recording decode (`analyze_recording`, which the 4-node
/// verdict consumes) must recover a SOFTENED bottom burn that rqrr's single full-frame
/// `detect_grids` pass misses — the residual burn-unreadable defect (#202/#186). This drives
/// the FULL path (compose a frame with the top dual-QR + a 260px node burn in the bottom-left
/// corner, soften the whole frame as the NDI/encode chain does, encode to a lossless ffv1
/// gray8 mkv, decode through `analyze_recording`) and asserts ALL THREE marks decode: both
/// optical QRs AND the node burn (run_id 911_001). Before the bottom-band tiled robust decode
/// this frame yielded only the two optical QRs. Requires ffmpeg (a camera-box runtime dep,
/// installed in CI).
#[test]
fn analyze_recording_recovers_a_softened_bottom_burn() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        panic!("ffmpeg not found — the #202 recovery round-trip cannot run; install ffmpeg");
    }
    let (w, h) = (1920u32, 1080u32);
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
        run_id: 911_001, // a cam1-capture node burn
        frame_id: 1234,
        gen_ts_ns: 3,
    };

    // Compose: big top dual-QR + a 260px burn in the bottom-left corner, then soften the
    // WHOLE frame (the real recording chain anti-aliases the small burn). At this blur the
    // full-frame pass misses the burn; the bottom-band tiled robust decode recovers it.
    let bgra = render_qr_dual_bgra(&left, &right, w, h, 700);
    let mut luma = bgra_to_luma(&bgra, w, h, w * 4);
    // Render the burn as a centered ~260px QR on a small white BGRA canvas, take its luma,
    // and composite the QR's non-white (module) pixels into the bottom-left corner.
    let canvas = 320u32;
    let burn_bgra = render_qr_bgra(&burn, canvas, canvas, 260);
    let burn_luma = bgra_to_luma(&burn_bgra, canvas, canvas, canvas * 4);
    let (ox, oy) = (40u32, h - canvas - 40);
    for y in 0..canvas {
        for x in 0..canvas {
            luma.put_pixel(ox + x, oy + y, *burn_luma.get_pixel(x, y));
        }
    }
    let luma = image::imageops::blur(&luma, 2.8);

    // Encode the gray8 plane to a lossless ffv1 mkv exactly as the dev1 recording listener
    // does, then decode through the SAME analyze_recording the verdict uses.
    let dir = std::env::temp_dir().join(format!("burn-recover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let raw = dir.join("frame.gray8");
    let mkv = dir.join("burn.mkv");
    std::fs::write(&raw, luma.as_raw()).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-s"])
        .arg(format!("{w}x{h}"))
        .args(["-r", "30", "-i"])
        .arg(&raw)
        .args(["-c:v", "ffv1", "-level", "3"])
        .arg(&mkv)
        .status()
        .expect("spawn ffmpeg ffv1 encode");
    assert!(status.success(), "ffmpeg ffv1 encode failed");

    let frames = analyze_recording(&mkv).expect("analyze the softened-burn recording");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!frames.is_empty(), "decoded at least one frame");
    let got: Vec<u32> = {
        let mut r: Vec<u32> = frames[0].payloads.iter().map(|p| p.run_id).collect();
        r.sort_unstable();
        r.dedup();
        r
    };
    // The big optical QR decodes (sanity) AND the softened node burn is RECOVERED (#202).
    assert!(
        got.contains(&136_141_133),
        "the optical dual-QR must decode: {got:?}"
    );
    assert!(
        got.contains(&911_001),
        "the softened bottom node burn must be RECOVERED by the robust offline decode \
         (#202): run_ids={got:?}"
    );
}
