//! issue 1316 — Tier-0 std-only REPLICA of `src/tear_detect.rs`'s viability classification,
//! proving the post-retire cam2 "no projection source" topology is tolerated report-only.
//!
//! imag-nb was returned 16.9.2026, so cam2's grabber now receives the SPLITTER feed like every
//! other cambox — the aux projection tick (issue 1196) has NO source and never decodes on cam2.
//! From WINDOW STATS ALONE this is INDISTINGUISHABLE from an ordinary aux-empty content window (the
//! pre-aux `tests/tear_detect_781.rs` fixtures, whose aux slice is always empty), so it correctly
//! reads `Unproven` and PASSES the tear gate (never a red) — it is NOT given a separate `Absent`
//! viability (an automatic "Absent" would misclassify those real fixtures). The honest "no aux
//! source this window" reading is surfaced REPORT-ONLY by `aux_any_decode_fraction == 0.0` (and, in
//! the real module, `signal_operable` / `ProjectionProof::hdmi1_proof_backed`).
//!
//! WHY a replica: `src/tear_detect.rs` derives `serde::Serialize`, so a bare
//! `rustc --edition 2021 --test` cannot compile it (the projection-tap-tear-detect.md Tier-0
//! recipe). This mirrors the pure viability logic only (no serde, no crate dep). The REAL module is
//! exercised by the inline `no_projection_source_cam2_window_passes_report_only_1316` (CI runs it)
//! and the pre-existing `tests/tear_detect_781.rs` fixtures (which this change leaves GREEN — the
//! classifier behaviour is unchanged); keep them in sync.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viability {
    Observed,
    Unproven,
}

const VERNIER_MAX_SPREAD: u32 = 1;

/// One tile's dual-QR / aux pair yields AT MOST 2 optical QRs, so >= 3 ids in either band means the
/// frame was composited from >= 2 capture paths (multi-tile skew), which is never a tear.
fn is_multi_path_suspect(primary: &[u32], aux: &[u32]) -> bool {
    primary.len() as u32 >= 3 || aux.len() as u32 >= 3
}

fn union_spread(primary: &[u32], aux: &[u32]) -> Option<u32> {
    let it = primary.iter().chain(aux);
    let min = it.clone().min()?;
    let max = it.max()?;
    Some(max - min)
}

fn is_torn(primary: &[u32], aux: &[u32]) -> bool {
    !is_multi_path_suspect(primary, aux)
        && union_spread(primary, aux).is_some_and(|s| s > VERNIER_MAX_SPREAD)
}

struct Stats {
    total_frames: u32,
    decodable_frames: u32,
    tear_frames: u32,
    aux_any_decode_fraction: f64,
    viability: Viability,
}

/// The pure classifier — mirrors `window_tear_stats`'s viability decision (unchanged by issue 1316:
/// Observed iff a tear fired, else Unproven — no separate `Absent` state).
fn classify(frames: &[(Vec<u32>, Vec<u32>)]) -> Stats {
    let total_frames = frames.len() as u32;
    let mut decodable_frames = 0u32;
    let mut tear_frames = 0u32;
    let mut aux_any_frames = 0u32;
    for (primary, aux) in frames {
        if !aux.is_empty() {
            aux_any_frames += 1;
        }
        if !is_multi_path_suspect(primary, aux) && union_spread(primary, aux).is_some() {
            decodable_frames += 1;
            if is_torn(primary, aux) {
                tear_frames += 1;
            }
        }
    }
    let aux_any_decode_fraction = if total_frames > 0 {
        aux_any_frames as f64 / total_frames as f64
    } else {
        0.0
    };
    let viability = if tear_frames > 0 {
        Viability::Observed
    } else {
        Viability::Unproven
    };
    Stats {
        total_frames,
        decodable_frames,
        tear_frames,
        aux_any_decode_fraction,
        viability,
    }
}

/// Mirrors `tear_gate_pass`: a window FAILS only when it is `Observed`. Everything else passes.
fn tear_gate_pass(s: &Stats) -> bool {
    s.viability != Viability::Observed
}

fn f(primary: &[u32], aux: &[u32]) -> (Vec<u32>, Vec<u32>) {
    (primary.to_vec(), aux.to_vec())
}

#[test]
fn no_projection_source_cam2_window_passes_as_unproven_1316() {
    // The post-retire cam2 splitter leg: primary decodes, aux never does.
    let frames = vec![
        f(&[100, 101], &[]),
        f(&[102, 103], &[]),
        f(&[104], &[]),
        f(&[], &[]),
    ];
    let s = classify(&frames);
    assert!(
        s.decodable_frames > 0,
        "primary decodes on a real camera leg"
    );
    assert_eq!(s.tear_frames, 0);
    assert_eq!(
        s.aux_any_decode_fraction, 0.0,
        "no projection aux source (report-only signal)"
    );
    assert_eq!(
        s.viability,
        Viability::Unproven,
        "a sourceless cam2 leg reads Unproven and passes — NOT a separate Absent state"
    );
    assert!(
        tear_gate_pass(&s),
        "a no-projection-source leg never fails the tear gate"
    );
}

#[test]
fn pre_aux_empty_content_windows_are_not_reclassified_1316() {
    // The exact shape of the real tests/tear_detect_781.rs fixtures (aux always empty): a healthy
    // single-band window (decodable > 0) AND a multi-tile window with a couple single-source frames
    // (decodable == 2). BOTH must stay Unproven — issue 1316 must NOT reclassify them.
    let healthy: Vec<_> = (0..20u32).map(|i| f(&[100 + i, 101 + i], &[])).collect();
    let s = classify(&healthy);
    assert!(s.decodable_frames > 0 && s.aux_any_decode_fraction == 0.0);
    assert_eq!(
        s.viability,
        Viability::Unproven,
        "healthy aux-empty window stays Unproven"
    );

    let multitile = vec![
        f(&[100, 101, 102], &[]),
        f(&[104, 105, 106, 107], &[]),
        f(&[110, 111], &[]),
    ];
    let m = classify(&multitile);
    assert!(
        m.decodable_frames > 0,
        "the single-source frame is scored (decodable > 0)"
    );
    assert_eq!(m.aux_any_decode_fraction, 0.0);
    assert_eq!(
        m.viability,
        Viability::Unproven,
        "multi-tile aux-empty window stays Unproven"
    );
}

#[test]
fn aux_present_but_no_tear_stays_unproven_1316() {
    let frames = vec![f(&[100, 101], &[100, 101]), f(&[102, 103], &[102, 103])];
    let s = classify(&frames);
    assert!(s.aux_any_decode_fraction > 0.0);
    assert_eq!(s.viability, Viability::Unproven);
    assert!(tear_gate_pass(&s));
}

#[test]
fn empty_window_stays_unproven_1316() {
    let s = classify(&[]);
    assert_eq!(s.total_frames, 0);
    assert_eq!(s.viability, Viability::Unproven);
    assert!(tear_gate_pass(&s));
}

#[test]
fn a_real_tear_still_fails_the_gate_1316() {
    // Regression guard: a genuine cross-band tear (primary gen G+1, one aux mark gen G) is Observed
    // and fails the gate — the post-retire topology must not blind a real tear.
    let frames = vec![f(&[102, 103], &[100])];
    let s = classify(&frames);
    assert_eq!(s.tear_frames, 1);
    assert_eq!(s.viability, Viability::Observed);
    assert!(!tear_gate_pass(&s), "a real Observed tear still fails");
}
