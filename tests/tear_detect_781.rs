//! issue 781 — the projection-tap tear detector proven against a REAL captured-frame fixture
//! (`pattern-change-needs-decode-fixture`): per-frame cam2-optical Vernier `frame_id`s decoded from a
//! real CAM2 window of an all-cambox sweep recording (`tests/fixtures/tear-781/
//! cam2_window_optical_ids.txt`, extracted from stream-partial-1265251855.json). The detector must
//! read this real, healthy content as tear-free — and, because the payload-level signal is
//! structurally blind on the current single-vertical-band dual-QR content, must report the signal as
//! `Unproven` (never a false `Observed`, never a false tear).
//!
//! issue 1196 (v2): the detector now takes per-frame `(primary_ids, aux_ids)` — the aux Vernier
//! tick pair painted in the bottom burn-gaps. The fixture PREDATES the aux marks (painted content
//! without them), so every frame's aux slice is EMPTY here: the real-content assertions must hold
//! unchanged with zero aux coverage, and the aux-driven cross-band detection is proven by splicing
//! synthetic aux data onto the real window. The REAL-captured-frame fixture WITH aux marks is mined
//! from the first rig run after the painter redeploy (a promotion precondition — see
//! `.claude/rules/projection-tap-tear-detect.md`).

use camera_box::tear_detect::{window_tear_stats, TearSignalViability};

/// Parse the fixture: one line per in-window captured frame, space-separated optical `frame_id`s
/// (a blank line = an undecodable frame, no optical QR). `#`-prefixed lines are the provenance
/// header. Returns one `(primary_ids, aux_ids)` per frame — aux always empty (pre-aux content).
fn load_fixture() -> Vec<(Vec<u32>, Vec<u32>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tear-781/cam2_window_optical_ids.txt"
    );
    let text = std::fs::read_to_string(path).expect("fixture readable");
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| {
            (
                l.split_whitespace()
                    .map(|t| t.parse::<u32>().expect("optical id is a u32"))
                    .collect::<Vec<u32>>(),
                Vec::new(),
            )
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

    assert_eq!(stats.total_frames, 847, "every attributed frame counted");
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
    // Pre-aux content: zero aux coverage, zero discriminator — honest zeros, never fabricated.
    assert_eq!(stats.aux_decode_fraction, 0.0, "fixture predates aux marks");
    assert_eq!(stats.primary_dark_aux_alive_fraction, 0.0);
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
        .and_then(|(p, _)| p.iter().max().copied())
        .unwrap_or(20_000);
    frames.push((vec![base, base + 1, base + 2, base + 3], Vec::new())); // span 3 > VERNIER_MAX_SPREAD

    let stats = window_tear_stats(&frames);
    assert_eq!(stats.tear_frames, 1, "the spliced torn frame is detected");
    assert!(stats.max_spread >= 2);
    assert_eq!(
        stats.viability,
        TearSignalViability::Observed,
        "one real tear makes the signal Observed"
    );
}

#[test]
fn a_cross_band_tear_via_aux_marks_spliced_into_the_real_window_is_detected_1196() {
    // The issue-1196 capability the aux pair exists for: a horizontal seam BETWEEN the primary
    // band and the aux band — the primary pair reads gen G+1 while the bottom aux marks still
    // read gen G. NEITHER band alone spans > 1; the union does. Splice that shape onto the real
    // window and prove the v2 detector fires where the v1 (primary-only) detector was blind.
    let mut frames = load_fixture();
    let base = frames
        .last()
        .and_then(|(p, _)| p.iter().max().copied())
        .unwrap_or(20_000);
    // primary = gen G+1 (ticks base+2 / base+3), aux = gen G (ticks base / base+1).
    frames.push((vec![base + 2, base + 3], vec![base, base + 1]));

    let stats = window_tear_stats(&frames);
    assert_eq!(stats.tear_frames, 1, "the cross-band seam frame is torn");
    assert!(stats.max_spread >= 2);
    assert_eq!(stats.viability, TearSignalViability::Observed);
}

#[test]
fn primary_dark_aux_alive_frames_raise_the_discriminator_on_the_real_window_1196() {
    // A seam INSIDE the primary band: both primary halves corrupt (undecodable) while BOTH aux
    // marks decode — band-localized corruption, which the primary-only v1 counted as a plain
    // undecodable frame. The discriminator fraction surfaces it (report-only).
    let mut frames = load_fixture();
    let n_before = frames.len() as u32;
    frames.push((Vec::new(), vec![30_000, 30_001]));

    let stats = window_tear_stats(&frames);
    assert_eq!(stats.total_frames, n_before + 1);
    assert_eq!(stats.tear_frames, 0, "aux span 1 alone is not a tear");
    let expect = 1.0 / (n_before + 1) as f64;
    assert!(
        (stats.primary_dark_aux_alive_fraction - expect).abs() < 1e-12,
        "exactly the spliced frame is primary-dark-aux-alive (got {})",
        stats.primary_dark_aux_alive_fraction
    );
    assert!(
        (stats.aux_decode_fraction - expect).abs() < 1e-12,
        "exactly the spliced frame carries both aux marks (got {})",
        stats.aux_decode_fraction
    );
}
