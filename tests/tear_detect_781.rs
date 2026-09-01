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

use camera_box::tear_detect::{
    signal_promotable, window_promotable, window_tear_stats, TearSignalViability,
};

/// Parse a fixture file: one line per in-window captured frame, space-separated optical `frame_id`s
/// (a blank line = an undecodable frame, no optical QR). `#`-prefixed lines are the provenance
/// header. Returns one `(primary_ids, aux_ids)` per frame — aux always empty (pre-aux content).
fn load_fixture_file(rel: &str) -> Vec<(Vec<u32>, Vec<u32>)> {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path} readable: {e}"));
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

/// The pre-aux SINGLE-BAND fixture (a real 847-frame CAM2 window, healthy single-tile content).
fn load_fixture() -> Vec<(Vec<u32>, Vec<u32>)> {
    load_fixture_file("tests/fixtures/tear-781/cam2_window_optical_ids.txt")
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
    // issue 1196: real green content is NEVER promotable — an all-zero distribution cannot prove
    // the signal works (the blind-signal trap). A LIVE flip must gate on a known-torn run.
    assert!(
        !window_promotable(&stats),
        "real green content is not promotable"
    );
    assert!(
        !signal_promotable(std::slice::from_ref(&stats)),
        "a green run cannot promote the tear gate"
    );
}

#[test]
fn a_synthetic_tear_spliced_into_the_real_window_is_detected() {
    // Take the real window and splice ONE synthetic SINGLE-TILE torn frame: one tile's dual-QR
    // captured gen G's even and gen G+2's even (2 ids, span 2 > VERNIER_MAX_SPREAD). This proves
    // the detector fires on a genuine tear even though the real content never produces one (the
    // blindness is a property of the CONTENT, not the detector). issue 1196 v2.1: the frame must
    // stay SINGLE-SOURCE (<= 2 primary ids) — a 4-id splice would be multi-path-suspect (a single
    // band cannot yield 4 clean generations), scored as inter-tile skew, not a tear.
    let mut frames = load_fixture();
    let base = frames
        .last()
        .and_then(|(p, _)| p.iter().max().copied())
        .unwrap_or(20_000);
    frames.push((vec![base, base + 2], Vec::new())); // span 2 > VERNIER_MAX_SPREAD, one tile

    let stats = window_tear_stats(&frames);
    assert_eq!(stats.tear_frames, 1, "the spliced torn frame is detected");
    assert!(stats.max_spread >= 2);
    assert_eq!(
        stats.multi_path_suspect_frames, 0,
        "the real single-band window carries no multi-tile frames"
    );
    assert_eq!(
        stats.viability,
        TearSignalViability::Observed,
        "one real tear makes the signal Observed"
    );
    // issue 1196: a real window with one observed single-tile tear IS promotable (the shape a
    // known-torn calibration run must produce on the CAM2 projection leg).
    assert!(
        window_promotable(&stats),
        "an observed single-tile tear on real content is promotable"
    );
    assert!(signal_promotable(std::slice::from_ref(&stats)));
}

#[test]
fn real_multitile_window_is_multi_path_skew_not_torn_1196() {
    // The issue-1196 finding, proven against REAL data (E2E 1859005342, ticket comment 5415952812):
    // the recorded program is MULTI-TILE — an ALL_CAMBOX composition carrying TWO grabber-path tiles
    // of the SAME painted cam2 monitor, offset ~2-4 ticks of inter-path latency. So ~99.8% of frames
    // decode 3-4 primary optical QRs whose union spans 2-4, which v2 mis-scored as ~99% torn
    // (verdict-1859005342 all_cambox_continuity.tear: tear_fraction ~0.99, max_spread 4). v2.1's
    // single-source-only scoping reads this correctly: nearly every frame is MULTI-PATH SUSPECT
    // (>= 3 primary ids = >= 2 tiles), excluded from the tear count, so tear_frames == 0 and the
    // signal is Unproven — the honest "multi-tile, tear unscoreable here" verdict.
    let frames = load_fixture_file("tests/fixtures/tear-781/cam2_window_multitile_ids_1196.txt");
    assert_eq!(
        frames.len(),
        846,
        "fixture frame count (first 846 real frames)"
    );

    let stats = window_tear_stats(&frames);

    assert_eq!(stats.total_frames, 846, "every attributed frame counted");
    // 844 of 846 frames carry >= 3 primary optical ids = a multi-tile composite -> suspect.
    assert_eq!(
        stats.multi_path_suspect_frames, 844,
        "the multi-tile frames are flagged suspect, not scored"
    );
    assert!(
        (stats.multi_path_suspect_fraction - 844.0 / 846.0).abs() < 1e-9,
        "~0.998 of the window is multi-tile (got {})",
        stats.multi_path_suspect_fraction
    );
    // Only the 2 single-source frames (both healthy adjacent pairs, span 1) are scored.
    assert_eq!(
        stats.decodable_frames, 2,
        "only single-source frames scored"
    );
    assert_eq!(
        stats.tear_frames, 0,
        "inter-path skew is NOT a tear (v2 read ~844)"
    );
    assert_eq!(stats.tear_fraction, 0.0);
    assert_eq!(
        stats.max_spread, 1,
        "the 2 single-source frames are healthy adjacencies"
    );
    assert_eq!(
        stats.max_cluster_count, 2,
        "peak 2 composited tiles per frame"
    );
    assert_eq!(
        stats.max_multi_path_spread, 3,
        "peak inter-path skew surfaced separately from the (clean) tear magnitude"
    );
    assert_eq!(
        stats.aux_decode_fraction, 0.0,
        "aux QRs did not survive the lossy chain on this run"
    );
    assert_eq!(
        stats.viability,
        TearSignalViability::Unproven,
        "a fully multi-tile window has no scoreable tear -> Unproven, never a false Observed"
    );
    // issue 1196: a real MULTI-TILE window is NEVER promotable — both because it is Unproven AND
    // because its suspect fraction (~0.998) is far above MULTI_PATH_SUSPECT_CEILING. The suspect
    // ceiling is what keeps a multi-tile window from ever being promoted.
    assert!(
        !window_promotable(&stats),
        "a multi-tile window is not promotable"
    );
    assert!(!signal_promotable(std::slice::from_ref(&stats)));
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
