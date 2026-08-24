//! issue 781 — the projection-tap tear detector proven against a REAL captured-frame fixture
//! (`pattern-change-needs-decode-fixture`): per-frame cam2-optical Vernier `frame_id`s decoded from a
//! real CAM2 window of an all-cambox sweep recording (`tests/fixtures/tear-781/
//! cam2_window_optical_ids.txt`, extracted from stream-partial-1265251855.json). The detector must
//! read this real, healthy content as tear-free — and, because the payload-level signal is
//! structurally blind on the current single-vertical-band dual-QR content, must report the signal as
//! `Unproven` (never a false `Observed`, never a false tear).

use camera_box::tear_detect::{window_tear_stats, TearSignalViability};

/// Parse the fixture: one line per in-window captured frame, space-separated optical `frame_id`s
/// (a blank line = an undecodable frame, no optical QR). `#`-prefixed lines are the provenance
/// header. Returns one `Vec<u32>` per frame.
fn load_fixture() -> Vec<Vec<u32>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tear-781/cam2_window_optical_ids.txt"
    );
    let text = std::fs::read_to_string(path).expect("fixture readable");
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| {
            l.split_whitespace()
                .map(|t| t.parse::<u32>().expect("optical id is a u32"))
                .collect::<Vec<u32>>()
        })
        .collect()
}

#[test]
fn real_cam2_window_is_tear_free_and_unproven() {
    let frames = load_fixture();
    // The mined CAM2 window carries 847 in-window frames (matches verdict-1265251855
    // all_cambox_continuity.segments[0].frames).
    assert_eq!(frames.len(), 847, "fixture frame count");

    let stats = window_tear_stats(&frames);

    // Every real frame decoded at least one optical half in this window (0 undecodable in the
    // source segment), so all 847 are decodable.
    assert_eq!(
        stats.decodable_frames, 847,
        "every in-window frame carried an optical Vernier payload"
    );
    // The healthy dual-QR Vernier never spans more than the by-design even/odd adjacency on real
    // content — ZERO torn frames, max span <= 1.
    assert_eq!(stats.tear_frames, 0, "real healthy content has no tears");
    assert_eq!(stats.tear_fraction, 0.0);
    assert!(
        stats.max_spread <= 1,
        "real optical span never exceeds the Vernier adjacency (got {})",
        stats.max_spread
    );
    // The signal never fired on this real content -> Unproven, NOT a promotable Observed.
    assert_eq!(stats.viability, TearSignalViability::Unproven);
}

#[test]
fn a_synthetic_tear_spliced_into_the_real_window_is_detected() {
    // Take the real window and splice ONE synthetic torn frame (two paint generations captured in
    // one frame: even/odd of gen G plus even/odd of gen G+1). This proves the detector fires on a
    // genuine tear even though the real content never produces one (the blindness is a property of
    // the CONTENT, not the detector).
    let mut frames = load_fixture();
    let base = frames
        .last()
        .and_then(|f| f.iter().max().copied())
        .unwrap_or(20_000);
    frames.push(vec![base, base + 1, base + 2, base + 3]); // span 3 > VERNIER_MAX_SPREAD

    let stats = window_tear_stats(&frames);
    assert_eq!(stats.tear_frames, 1, "the spliced torn frame is detected");
    assert!(stats.max_spread >= 2);
    assert_eq!(
        stats.viability,
        TearSignalViability::Observed,
        "one real tear makes the signal Observed"
    );
}
