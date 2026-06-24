//! #186 regression LOCK — real-recording burn-QR decode robustness.
//!
//! THE BUG (#186, week-long): the recording verdict flagged frames as
//! "burn-unreadable" — a per-hop "loss" — when the small ~300px node burns
//! (cam1 bottom-center, strih bottom-left, stream bottom-right) were VISUALLY
//! PRESENT and sharp in the frame, but rqrr's single full-frame `detect_grids`
//! pass failed to decode them (the big optical dual-QR dominates the finder, so
//! a small burn sharing the frame is intermittently missed). A digitally-burned
//! QR that is present MUST decode — a non-decoding present burn is a decoder
//! defect, never a real drop.
//!
//! THE FIX (#202, landed in PR #203): [`decode_qr_luma_all_robust`] adds, on top
//! of the plain full-frame pass (`decode_qr_luma_all`), a bottom-band tiled +
//! cubic-upscaled retry that gives rqrr the small burn in a sub-tile where it is
//! large-relative and its finder locks. On the real recording the verdict's
//! `undecodable` count dropped from "10 cam1 / 5 strih" to 0.
//!
//! THIS TEST locks that fix against the ACTUAL recording frames the OLD decoder
//! failed on — not a synthetic blur. The committed fixtures under
//! `tests/fixtures/burn-unreadable/` are real frames pulled from the recording
//! run (#201, cam1 from 1080p strih; strih/stream at native 4K), converted to
//! grayscale (the exact luma plane the decoder consumes). On EVERY fixture:
//!
//!   * the PLAIN full-frame decode (`decode_qr_luma_all`) MISSES the node burn —
//!     this is the #186 bug condition, reproduced from real pixels (RED);
//!   * the ROBUST decode (`decode_qr_luma_all_robust`) RECOVERS the exact burn
//!     payload — the #202 fix (GREEN).
//!
//! If a future rqrr/decoder change regresses robustness, these real frames stop
//! decoding and CI catches it before the verdict starts manufacturing phantom
//! "burn-unreadable" losses again.

#![cfg(feature = "probe")]

use camera_box::probe::payload::Payload;
use camera_box::probe::qr::{decode_qr_luma_all, decode_qr_luma_all_robust};
use image::GrayImage;
use std::path::PathBuf;

/// The node-burn run_ids (mirror of `recording_latency::BURN_RUN_ID_*`).
const CAM1: u32 = 911_001;
const STRIH: u32 = 911_002;
const STREAM: u32 = 911_004;

fn fixture_luma(name: &str) -> GrayImage {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "burn-unreadable",
        name,
    ]
    .iter()
    .collect();
    image::open(&path)
        .unwrap_or_else(|e| panic!("open fixture {}: {e}", path.display()))
        .to_luma8()
}

fn burn(payloads: &[Payload], run_id: u32, frame_id: u32) -> bool {
    payloads
        .iter()
        .any(|p| p.run_id == run_id && p.frame_id == frame_id)
}

/// Each fixture + the EXACT node burn (run_id, frame_id) it carries that the OLD
/// (plain) decoder dropped. These are the real frames the verdict flagged
/// "burn-unreadable" (the dirs were literally named `cam1-missing` / `strih-missing`
/// / `stream-missing`). Robust must decode every one of these exact payloads.
const CASES: &[(&str, u32, u32)] = &[
    ("cam1-frame-1148.png", CAM1, 1727),
    ("cam1-frame-225.png", CAM1, 804),
    ("cam1-frame-4051.png", CAM1, 4630),
    ("strih-frame-1145.png", STRIH, 5105),
    ("stream-frame-2086.png", STREAM, 4558),
];

/// GREEN (#202): the robust decode recovers the EXACT node burn payload from every
/// real "burn-unreadable" frame. This is the proof the decoder — not the chain — was
/// the limiter, and that it is now fixed: a present burn always decodes.
#[test]
fn robust_decode_recovers_every_real_burn_unreadable_frame() {
    let mut failures = Vec::new();
    for &(name, run_id, frame_id) in CASES {
        let luma = fixture_luma(name);
        let robust = decode_qr_luma_all_robust(luma);
        if !burn(&robust, run_id, frame_id) {
            failures.push(format!(
                "{name}: robust FAILED to decode the present burn ({run_id}.{frame_id}); \
                 got {:?}",
                robust
                    .iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the #202 robust decode must read EVERY present burn (0 burn-unreadable on real \
         recording frames — #186 strict-zero gate):\n{}",
        failures.join("\n")
    );
}

/// RED-condition lock: the PLAIN full-frame pass MISSES the same burns the robust pass
/// recovers — this is exactly the #186 bug, reproduced from real recording pixels. If a
/// future change ever makes the plain pass find these full-frame (great — rqrr improved),
/// this assertion fails and the test must be re-anchored; until then it proves the robust
/// pass is doing real recovery work, not finding burns the plain pass already had.
#[test]
fn plain_full_frame_pass_misses_the_real_burns_robust_recovers() {
    let mut plain_found = Vec::new();
    for &(name, run_id, frame_id) in CASES {
        let luma = fixture_luma(name);
        let plain = decode_qr_luma_all(luma);
        if burn(&plain, run_id, frame_id) {
            plain_found.push(format!(
                "{name}: plain unexpectedly decoded {run_id}.{frame_id}"
            ));
        }
    }
    assert!(
        plain_found.is_empty(),
        "the plain full-frame pass is EXPECTED to miss these small burns (the #186 condition \
         the robust pass exists to fix); if rqrr now finds them full-frame, re-anchor this \
         test with harder fixtures:\n{}",
        plain_found.join("\n")
    );
}
