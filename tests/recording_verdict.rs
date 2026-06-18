//! RED→GREEN regression/feature tests for #107: the hard-fail loss verdict that
//! consumes the #106 recording-probe per-frame stream (recorded OBS program file
//! ONLY — never an NDI tap, never the lz4 spool, never a percentage threshold).
//!
//! The verdict engine is pure: it takes the per-frame resolved-tick sequence
//! (`FrameTick`) and decides PASS / FAIL with NO thresholds. PASS requires
//! 0 undecodable AND 0 real-copy AND 0 real-gap AND an analyzed span ≥ 300 s.
//! The free-running 60→30 camera-sampling beat (avg step exactly 2.0, symmetric
//! around 2.0) is recognized and NOT counted as loss; only the NET imbalance is.
//!
//! These tests build SYNTHETIC per-frame tick streams (no rig needed). The single
//! PNG-extraction test uses the committed `tests/fixtures/strih-6519-dual-qr.mkv`.

#![cfg(feature = "probe")]

use camera_box::probe::recording::extract_frames_png;
use camera_box::probe::recording_verdict::{
    cam_strih_assessment, strih_stream_verdict, verdict, FrameTick, VerdictConfig,
};
use std::collections::HashSet;
use std::path::PathBuf;

/// A clean, balanced 60→30 Vernier beat: the resolved tick advances by 1 or 3
/// alternately (avg exactly 2.0, symmetric 1↔3) — pure sampling, NOT loss.
/// `n` camera frames starting at tick `start`.
fn balanced_beat(start: u32, n: usize) -> Vec<FrameTick> {
    // A symmetric 1↔3 Vernier beat with an EXACTLY balanced step multiset: emit
    // the steps in complete (1, 3) pairs so the sum is exactly 2·steps (mean 2.0)
    // for ANY frame count — an odd-length alternation would be off by ±1 and is
    // NOT a clean beat (that asymmetry IS a sub-frame net loss). `n` frames ⇒ n−1
    // steps; the last partial pair (when n is even) is completed with a balancing
    // step so the multiset stays symmetric.
    let mut ticks = Vec::with_capacity(n);
    let mut t = start;
    for i in 0..n {
        ticks.push(FrameTick {
            frame_index: i as u64,
            tick: Some(t),
        });
        // Steps come in (1,3) pairs: step before an even-index frame is 1, before
        // an odd-index frame is 3, but we pair them so the count of 1s == count of
        // 3s. With `i` the index of the frame just pushed, the NEXT step is 1 when
        // `i` is even and 3 when odd — for an even total `n` that yields equal
        // counts; for an odd `n` the trailing 1 is dropped (n−1 steps, still
        // balanced because we start each pair with the 1).
        t += if i.is_multiple_of(2) { 1 } else { 3 };
    }
    ticks
}

/// 30 fps recording: `secs` seconds → an ODD camera-frame count (so the n−1 steps
/// are EVEN and the 1↔3 beat is exactly balanced — mean step precisely 2.0). A
/// clean beat that is ≥300 s when `secs >= 300`.
fn beat_for_secs(secs: f64) -> Vec<FrameTick> {
    let mut n = (secs * 30.0) as usize;
    if n.is_multiple_of(2) {
        n += 1; // odd frame count ⇒ even step count ⇒ exact 1↔3 balance
    }
    balanced_beat(1000, n)
}

fn cfg() -> VerdictConfig {
    // 30 fps camera, 300 s minimum-span gate, 60 Hz logical counter.
    VerdictConfig::default()
}

// ---- (a) clean balanced-beat stream → PASS ----

#[test]
fn clean_balanced_beat_passes() {
    let frames = beat_for_secs(300.0); // exactly the 300 s floor
    let v = verdict(&frames, &cfg());
    assert!(v.is_pass(), "clean balanced beat must PASS: {v:?}");
    assert!(v.undecodable_frames.is_empty());
    assert!(v.real_copy_frames.is_empty());
    assert!(v.real_gap_frames.is_empty());
    // The beat itself is recognized and labelled (avg step exactly 2.0).
    assert!(
        (v.avg_step - 2.0).abs() < 1e-9,
        "balanced beat avg step == 2.0, got {}",
        v.avg_step
    );
    assert!(v.beat_balanced, "symmetric beat must be flagged balanced");
}

// ---- (b) a real GAP → FAIL + names the frame ----

#[test]
fn real_gap_fails_and_names_frame() {
    let mut frames = beat_for_secs(300.0);
    // Inject a large unbalanced jump at a known frame: a missing camera frame
    // shows as a tick jump far beyond the beat's {1,3} that is NOT matched by a
    // compensating short step (so avg drifts above 2.0 / gaps unbalanced).
    let bad = 5000usize;
    for f in frames.iter_mut().skip(bad) {
        if let Some(t) = f.tick {
            f.tick = Some(t + 20); // permanent +20 skip from here on (lost frames)
        }
    }
    let v = verdict(&frames, &cfg());
    assert!(!v.is_pass(), "a real gap must FAIL");
    assert!(
        v.real_gap_frames.contains(&(bad as u64)),
        "the gap frame {bad} must be named, got {:?}",
        v.real_gap_frames
    );
}

// ---- (c) a stale-COPY repeat → FAIL ----

#[test]
fn real_stale_copy_fails_and_names_frame() {
    let mut frames = beat_for_secs(300.0);
    // Inject a NET stale-copy (FIFO underrun): the output served the SAME frame an
    // extra time — INSERT a duplicate of frame `bad-1`'s tick at index `bad` and
    // push the rest down by one camera frame. The total logical-tick span is
    // unchanged but there is one MORE camera frame ⇒ avg step < 2.0 ⇒ a net copy
    // unmatched by any compensating overshoot (NOT the beat's balanced 0↔4).
    let bad = 4000usize;
    let dup_tick = frames[bad - 1].tick.unwrap();
    frames.insert(
        bad,
        FrameTick {
            frame_index: 0, // reindexed below
            tick: Some(dup_tick),
        },
    );
    // Re-number frame_index contiguously after the insert.
    for (i, f) in frames.iter_mut().enumerate() {
        f.frame_index = i as u64;
    }
    let v = verdict(&frames, &cfg());
    assert!(!v.is_pass(), "a net stale-copy must FAIL");
    assert!(
        v.real_copy_frames.contains(&(bad as u64)),
        "the stale-copy frame {bad} must be named, got {:?}",
        v.real_copy_frames
    );
}

// ---- (d) an UNDECODABLE frame → FAIL + (PNG path tested separately) ----

#[test]
fn undecodable_frame_fails_and_names_frame() {
    let mut frames = beat_for_secs(300.0);
    let bad = 2000usize;
    frames[bad].tick = None; // no CRC-valid QR decoded in this camera frame
    let v = verdict(&frames, &cfg());
    assert!(!v.is_pass(), "an undecodable frame must FAIL");
    assert!(
        v.undecodable_frames.contains(&(bad as u64)),
        "the undecodable frame {bad} must be named, got {:?}",
        v.undecodable_frames
    );
}

// ---- (e) a <300 s span is NOT a zero-loss PASS even when 0/0/0 ----

#[test]
fn short_span_is_not_a_zero_loss_pass() {
    let frames = beat_for_secs(299.0); // one second short of the 300 s floor
    let v = verdict(&frames, &cfg());
    assert!(
        !v.is_pass(),
        "a sub-300 s clean run must NOT certify zero-loss"
    );
    assert!(
        v.undecodable_frames.is_empty()
            && v.real_copy_frames.is_empty()
            && v.real_gap_frames.is_empty(),
        "no loss was injected — the only failure is the duration gate"
    );
    assert!(!v.duration_ok, "span < 300 s -> duration gate not met");
    assert!(
        v.analyzed_secs < 300.0,
        "analyzed span {} must be under 300 s",
        v.analyzed_secs
    );
}

#[test]
fn span_exactly_at_floor_passes_duration_gate() {
    // Exactly 300 s (9000 frames @ 30 fps) meets the floor (>=, not >).
    let frames = beat_for_secs(300.0);
    let v = verdict(&frames, &cfg());
    assert!(v.duration_ok, "exactly 300 s must meet the duration gate");
    assert!((v.analyzed_secs - 300.0).abs() < 0.5);
}

// ---- strih→stream: direct per-frame tick-sequence compare (beat cancels) ----

#[test]
fn strih_stream_identical_ticks_pass() {
    // The camera beat is COMMON to both digital outputs, so identical strih and
    // stream tick sequences = zero strih→stream hop loss.
    let strih = beat_for_secs(300.0);
    let stream = beat_for_secs(300.0);
    let v = strih_stream_verdict(&strih, &stream, &cfg());
    assert!(v.is_pass(), "identical strih/stream ticks must PASS: {v:?}");
    assert!(v.divergent_frames.is_empty());
}

#[test]
fn strih_stream_divergence_fails_and_names_frame() {
    let strih = beat_for_secs(300.0);
    let mut stream = beat_for_secs(300.0);
    // stream dropped a frame the strih output had: tick diverges at `bad`.
    let bad = 6000usize;
    stream[bad].tick = Some(stream[bad].tick.unwrap() + 2);
    let v = strih_stream_verdict(&strih, &stream, &cfg());
    assert!(!v.is_pass(), "a strih≠stream divergence must FAIL");
    assert!(
        v.divergent_frames.contains(&(bad as u64)),
        "divergent frame {bad} must be named, got {:?}",
        v.divergent_frames
    );
}

// ---- cam→strih: honest assessment against the cam2 painter ground truth ----

#[test]
fn cam_strih_matches_painter_ground_truth() {
    // The painter (cam2) emitted a contiguous logical counter; strih recorded the
    // sampled subset. When every strih tick is a tick the painter actually
    // displayed, cam→strih has no PROVABLE loss (within the camera-beat limit).
    let strih = beat_for_secs(300.0);
    // Painter ground truth = the full 60 Hz logical counter spanning the same range.
    let lo = strih.first().unwrap().tick.unwrap();
    let hi = strih.last().unwrap().tick.unwrap();
    let painter: Vec<u32> = (lo..=hi).collect();
    let a = cam_strih_assessment(&strih, &painter, &cfg());
    assert!(
        a.unknown_ticks.is_empty(),
        "every strih tick must be a painted tick, got unknown {:?}",
        a.unknown_ticks
    );
    // The limitation is ALWAYS stated explicitly (no false zero claim).
    assert!(
        !a.limitation.is_empty(),
        "cam→strih assessment must state its provability limitation"
    );
    assert!(
        !a.claims_zero_loss,
        "cam→strih must NOT emit a false zero-loss claim (beat overlaps loss)"
    );
}

#[test]
fn cam_strih_flags_tick_never_painted() {
    // A strih tick the painter never displayed = corruption / phantom id =
    // provable cam→strih fault.
    let mut strih = beat_for_secs(300.0);
    let bad = 1234usize;
    let phantom = 99_999_999u32; // far outside the painted range
    strih[bad].tick = Some(phantom);
    let lo = 1000u32;
    let hi = strih
        .iter()
        .filter_map(|f| f.tick)
        .filter(|&t| t != phantom)
        .max()
        .unwrap();
    let painter: Vec<u32> = (lo..=hi).collect();
    let a = cam_strih_assessment(&strih, &painter, &cfg());
    assert!(
        a.unknown_ticks.contains(&phantom),
        "a never-painted strih tick must be flagged, got {:?}",
        a.unknown_ticks
    );
}

// ---- PNG pixel-proof extraction (uses the committed dual-QR fixture) ----

fn fixture(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect()
}

#[test]
fn extract_png_writes_pixel_proof_and_flags_sharp_decoder_bug() {
    // The fixture is a single SHARP dual-QR frame (frame 0, ticks 6518/6519). If a
    // verdict ever flagged it UNDECODABLE, the extracted pixels still decode — that
    // is a Step-1 / #106 DECODER BUG, not a chain loss, and extract_frames_png must
    // surface it (sharp_qr_but_flagged_undecodable). This also proves the PNG is
    // actually written (the pixel-proof deliverable) — needs ffmpeg + the `png`
    // image feature, both present in CI.
    let path = fixture("strih-6519-dual-qr.mkv");
    let out_dir = std::env::temp_dir().join(format!("cb-107-png-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);

    let flagged = vec![0u64];
    let undecodable: HashSet<u64> = [0u64].into_iter().collect();
    let extracted =
        extract_frames_png(&path, &flagged, &undecodable, &out_dir).expect("extract pixel proof");

    assert_eq!(extracted.len(), 1, "one flagged frame extracted");
    let e = &extracted[0];
    assert_eq!(e.frame_index, 0);
    assert!(
        e.png_path.is_file(),
        "the pixel-proof PNG must exist on disk"
    );
    assert!(
        std::fs::metadata(&e.png_path).unwrap().len() > 0,
        "the PNG must be non-empty"
    );
    // The fixture QR is sharp, so flagging it undecodable is a decoder bug — proven
    // by re-decoding the extracted pixels to a CRC-valid payload.
    assert!(
        e.sharp_qr_but_flagged_undecodable,
        "a sharp decodable QR flagged undecodable must be surfaced as a decoder bug"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn extract_png_no_flagged_frames_is_a_noop() {
    let path = fixture("strih-6519-dual-qr.mkv");
    let out_dir = std::env::temp_dir().join(format!("cb-107-png-empty-{}", std::process::id()));
    let extracted =
        extract_frames_png(&path, &[], &HashSet::new(), &out_dir).expect("noop extraction");
    assert!(extracted.is_empty(), "no flagged frames -> no PNGs");
}
