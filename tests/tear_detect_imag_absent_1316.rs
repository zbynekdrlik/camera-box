//! issue 1316 — Tier-0 std-only REPLICA of `src/tear_detect.rs`'s viability classification,
//! proving the new `Absent` state for the post-retire cam2 topology.
//!
//! Root cause: imag-nb was returned 16.9.2026, so cam2's grabber no longer receives imag's HDMI
//! projection output — it now sees the SPLITTER feed like every other cambox. The aux projection
//! tick (issue 1196) therefore has NO source and never decodes on cam2, yet cam2 stays a normal
//! camera leg whose primary dual-QR decodes fine. Before this change the classifier had only
//! `Observed` / `Unproven`, so a sourceless cam2 window was indistinguishable from a
//! blind-on-content window. This adds an explicit `Absent` viability (decodable primary frames but
//! ZERO aux decode) that is REPORT-ONLY — it must never fail the tear gate.
//!
//! WHY a replica: `src/tear_detect.rs` derives `serde::Serialize`, so it cannot be compiled by a
//! bare `rustc --edition 2021 --test` (the projection-tap-tear-detect.md Tier-0 recipe: "strip the
//! serde derive" / assemble a standalone replica). This file mirrors the pure classification logic
//! ONLY (no serde, no crate dep), so it RED→GREENs locally with `rustc --edition 2021 --test`. The
//! REAL module is exercised by the inline `#[cfg(test)]` test
//! `no_projection_source_cam2_window_is_absent_and_passes_1316` (CI runs it); keep the two in sync.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viability {
    Observed,
    Unproven,
    Absent,
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

/// The pure classifier — mirrors `window_tear_stats`'s viability decision (issue 1316).
fn classify(frames: &[(Vec<u32>, Vec<u32>)]) -> Stats {
    let total_frames = frames.len() as u32;
    let mut decodable_frames = 0u32;
    let mut tear_frames = 0u32;
    let mut aux_any_frames = 0u32;
    for (primary, aux) in frames {
        if !aux.is_empty() {
            aux_any_frames += 1;
        }
        // decodable = SINGLE-SOURCE (not multi-tile) frame whose union decoded.
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
    } else if decodable_frames > 0 && aux_any_decode_fraction == 0.0 {
        // issue 1316: NO PROJECTION SOURCE — a real camera leg (decodable primary frames) with the
        // aux projection tick never decoding on ANY frame. Distinct from `Unproven` (aux present).
        Viability::Absent
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

/// Mirrors `tear_gate_pass`: a window FAILS only when it is `Observed` (and single-tile, over the
/// ceilings). Everything else — `Unproven`, `Absent`, multi-tile — passes.
fn tear_gate_pass(s: &Stats) -> bool {
    s.viability != Viability::Observed
}

fn f(primary: &[u32], aux: &[u32]) -> (Vec<u32>, Vec<u32>) {
    (primary.to_vec(), aux.to_vec())
}

#[test]
fn no_projection_source_cam2_window_is_absent_1316() {
    // The post-retire cam2 splitter leg: primary decodes, aux never does.
    let frames = vec![
        f(&[100, 101], &[]),
        f(&[102, 103], &[]),
        f(&[104], &[]),
        f(&[], &[]),
    ];
    let s = classify(&frames);
    assert!(s.decodable_frames > 0, "primary decodes on a real camera leg");
    assert_eq!(s.tear_frames, 0);
    assert_eq!(s.aux_any_decode_fraction, 0.0, "no projection aux source");
    assert_eq!(
        s.viability,
        Viability::Absent,
        "a sourceless cam2 leg reads Absent, not Unproven"
    );
    assert!(tear_gate_pass(&s), "an Absent leg never fails the tear gate");
}

#[test]
fn aux_present_but_no_tear_stays_unproven_1316() {
    // The pre-retire / imag-present case: aux DOES decode, no tear -> Unproven, NOT Absent.
    let frames = vec![f(&[100, 101], &[100, 101]), f(&[102, 103], &[102, 103])];
    let s = classify(&frames);
    assert!(s.aux_any_decode_fraction > 0.0);
    assert_eq!(
        s.viability,
        Viability::Unproven,
        "aux present but blind on content stays Unproven"
    );
    assert!(tear_gate_pass(&s));
}

#[test]
fn multi_tile_skew_window_stays_unproven_not_absent_1316() {
    // A MULTI-TILE window (every frame >= 3 primary ids, no aux) has decodable_frames == 0, so it
    // must stay Unproven, NEVER be misread as Absent (the discriminator is decodable_frames > 0).
    let frames = vec![f(&[100, 101, 102], &[]), f(&[104, 105, 106, 107], &[])];
    let s = classify(&frames);
    assert_eq!(s.decodable_frames, 0, "no single-source frame to score");
    assert_eq!(s.aux_any_decode_fraction, 0.0);
    assert_eq!(
        s.viability,
        Viability::Unproven,
        "a multi-tile skew window is Unproven, not Absent"
    );
    assert!(tear_gate_pass(&s));
}

#[test]
fn empty_window_stays_unproven_not_absent_1316() {
    let s = classify(&[]);
    assert_eq!(s.total_frames, 0);
    assert_eq!(s.decodable_frames, 0);
    assert_eq!(
        s.viability,
        Viability::Unproven,
        "an empty window (no frames) is Unproven, not Absent"
    );
    assert!(tear_gate_pass(&s));
}

#[test]
fn a_real_tear_still_fails_the_gate_1316() {
    // Regression guard: a genuine cross-band tear (primary gen G+1, one aux mark gen G) is Observed
    // and fails the gate — the Absent change must not blind a real tear.
    let frames = vec![f(&[102, 103], &[100])];
    let s = classify(&frames);
    assert_eq!(s.tear_frames, 1);
    assert_eq!(s.viability, Viability::Observed);
    assert!(!tear_gate_pass(&s), "a real Observed tear still fails");
}
