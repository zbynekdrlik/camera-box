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
use camera_box::probe::qr::{
    decode_qr_luma_all, decode_qr_luma_all_fast_then_robust, decode_qr_luma_all_robust,
};
use camera_box::probe::recording_latency::{
    BURN_RUN_ID_CAM1, BURN_RUN_ID_STREAM, BURN_RUN_ID_STRIH,
};
use image::GrayImage;
use std::path::PathBuf;

// The node-burn run_ids come straight from the single source of truth
// (`recording_latency::BURN_RUN_ID_*`) — never mirrored here, so a future change to the
// burn defaults can never let this test assert against a stale id.
const CAM1: u32 = BURN_RUN_ID_CAM1;
const STRIH: u32 = BURN_RUN_ID_STRIH;
const STREAM: u32 = BURN_RUN_ID_STREAM;

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

/// The robust pass does REAL recovery work — it is NOT just re-finding burns the plain
/// full-frame pass already had. Proven two ways that DON'T go red merely because the
/// upstream rqrr decoder happens to improve on one frame (the #186 bug is "plain misses the
/// small burns", but pinning "plain misses EVERY frame" would turn an rqrr improvement into
/// a false CI failure — a maintenance tax we avoid):
///
/// 1. per-fixture: robust is always a strict SUPERSET of plain (robust = plain ∪ tile
///    passes), so robust never decodes FEWER burns than plain — a hard invariant;
/// 2. in aggregate: across all fixtures the plain pass decodes strictly FEWER node burns
///    than robust, i.e. the tiled recovery genuinely adds burns the full-frame pass missed
///    (the #186 condition, from real recording pixels). Today plain recovers 0 of the
///    `CASES`; this stays green as long as the tiled pass keeps finding burns the
///    full-frame pass doesn't — even if a future rqrr finds one or two of them full-frame.
#[test]
fn robust_recovers_burns_the_plain_full_frame_pass_does_not() {
    let (mut plain_hits, mut robust_hits) = (0usize, 0usize);
    for &(name, run_id, frame_id) in CASES {
        let luma = fixture_luma(name);
        let plain = decode_qr_luma_all(luma.clone());
        let robust = decode_qr_luma_all_robust(luma);

        // (1) per-fixture superset invariant: every burn plain found, robust also found.
        if burn(&plain, run_id, frame_id) {
            plain_hits += 1;
            assert!(
                burn(&robust, run_id, frame_id),
                "{name}: robust must be a SUPERSET of the plain pass — plain decoded \
                 {run_id}.{frame_id} but robust did not"
            );
        }
        if burn(&robust, run_id, frame_id) {
            robust_hits += 1;
        }
    }
    // (2) aggregate: the tiled recovery adds burns the full-frame pass misses (#186).
    assert!(
        robust_hits > plain_hits,
        "the robust tiled pass must recover burns the plain full-frame pass misses (the #186 \
         decoder-coverage gap): plain hit {plain_hits}/{}, robust hit {robust_hits}/{}",
        CASES.len(),
        CASES.len()
    );
}

// ============================================================================
// #207 — the per-frame recording decode runs the cheap plain pass FIRST and the
// ~10×-cost robust tiles ONLY when a node burn is actually missing. These real
// "burn-unreadable" fixtures are exactly the rare frames where the plain pass
// MISSES the burn, so they must trigger the robust FALLBACK and still recover the
// exact burn — i.e. the #186 0-miss guarantee is preserved through the fast gate.
// ============================================================================

/// The #207 fast-then-robust decode still recovers EVERY real burn-unreadable frame.
/// On each fixture the plain pass misses the node burn, so the fast gate (which requires
/// the expected burn id) detects the miss and runs the full robust tiled recovery — which
/// reads the exact burn the verdict needs. This is the proof the speed optimization did
/// NOT weaken the #186 strict-zero guarantee: a present-but-hard burn still decodes.
#[test]
fn fast_then_robust_recovers_every_real_burn_unreadable_frame() {
    let mut failures = Vec::new();
    for &(name, run_id, frame_id) in CASES {
        let luma = fixture_luma(name);
        // The expected burns are this node's id — the plain pass misses it on these
        // fixtures, so the fast gate must fall back to the robust tiled recovery.
        let got = decode_qr_luma_all_fast_then_robust(luma, &[run_id]);
        if !burn(&got, run_id, frame_id) {
            failures.push(format!(
                "{name}: fast-then-robust FAILED to recover the present burn \
                 ({run_id}.{frame_id}); got {:?}",
                got.iter()
                    .map(|p| (p.run_id, p.frame_id))
                    .collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the #207 fast-then-robust decode must still read EVERY present burn (the robust \
         fallback fires when the plain pass misses one — #186 0-miss preserved):\n{}",
        failures.join("\n")
    );
}

/// The #207 fast-then-robust decode is byte-for-byte IDENTICAL to robust-always on every
/// real fixture: same payload SET (order-independent). The optimization only changes WHEN
/// the tiles run, never WHAT is read — so no decode regression vs the proven #202 robust
/// path. On a burn-unreadable fixture the fallback fires and runs the same tiles; on a
/// frame whose burns the plain pass already had, the fast path returns the same plain
/// superset (the tiles add nothing there anyway).
#[test]
fn fast_then_robust_matches_robust_always_on_every_fixture() {
    for &(name, run_id, _frame_id) in CASES {
        let luma = fixture_luma(name);
        let mut robust = decode_qr_luma_all_robust(luma.clone());
        let mut fast = decode_qr_luma_all_fast_then_robust(luma, &[run_id]);
        // Order-independent set equality on (run_id, frame_id, gen_ts_ns).
        let key = |p: &camera_box::probe::payload::Payload| (p.run_id, p.frame_id, p.gen_ts_ns);
        robust.sort_by_key(key);
        fast.sort_by_key(key);
        assert_eq!(
            robust, fast,
            "{name}: #207 fast-then-robust must read the IDENTICAL payload set as \
             robust-always (only WHEN the tiles run differs, never WHAT is read)"
        );
    }
}
