//! Regression guard for #32 (full-rate painter → strong single-copy guard).
//!
//! At the sub-fps coverage default (12 fps) the QR painter oversampled every id,
//! so the strih→stream hop carried only 2–63 single-copy (oversample-independent)
//! frames per run — far too few for the #29 guard to certify, so that hop was left
//! UNGATED. Measured on the live rig (cam2 x86_64, 60 s taps), painting at the 30 fps
//! pipeline rate instead lifts single-copy to ~1200 (cam→strih) / ~1780 (strih→stream)
//! with the SAME qr_size 700 (the 30 fps NDI taps decode 100%, decode_failed=0), so
//! the guard can now certify both hops.
//!
//! These tests pin the two script invariants that make that true. If anyone reverts
//! the painter to the coverage default or un-gates strih→stream, they fail.

use std::fs;

fn script() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/multitap-e2e.sh");
    fs::read_to_string(path).expect("read scripts/multitap-e2e.sh")
}

/// The paint-only invocation MUST set an explicit `--paint-fps` (the pipeline rate),
/// not fall back to the 12 fps coverage default — otherwise single-copy is starved.
#[test]
fn phase2_painter_paints_at_pipeline_rate_not_coverage_default() {
    let s = script();
    // Anchor on the real command (`/tmp/frame-probe --paint-only`), not the topology
    // comment's bare `(frame-probe --paint-only)`.
    let paint_at = s
        .find("/tmp/frame-probe --paint-only")
        .expect("painter invocation '/tmp/frame-probe --paint-only' not found in multitap-e2e.sh");
    // Look within the painter invocation (its line + a little slack), not the whole file.
    let window = &s[paint_at..(paint_at + 240).min(s.len())];
    assert!(
        window.contains("--paint-fps"),
        "#32 regression: the multitap painter must pass an explicit --paint-fps (the \
         pipeline rate) on its --paint-only invocation. Without it the painter uses the \
         12 fps coverage default, which oversamples every id and starves strih→stream \
         single-copy (2–63/run) so the #29 guard cannot certify that hop. Painter window: \
         {window:?}"
    );
}

/// With full-rate painting, strih→stream now carries abundant single-copy frames, so
/// the #29 guard MUST be enabled there (a non-empty `MIN_SC_STREAM` default).
#[test]
fn phase2_strih_stream_single_copy_guard_is_enabled() {
    let s = script();
    assert!(
        !s.contains(r#"MIN_SC_STREAM="${MIN_SC_STREAM-}""#),
        "#32 regression: MIN_SC_STREAM must have a NON-EMPTY default now that full-rate \
         painting feeds the strih→stream hop abundant single-copy frames. An empty default \
         leaves the binding hop ungated — a starved/degenerate run would read as a false \
         green instead of INCONCL."
    );
    // And a positive default must actually be present (gated), e.g. MIN_SC_STREAM-100.
    let gated = s
        .lines()
        .any(|l| l.contains("MIN_SC_STREAM=\"${MIN_SC_STREAM-") && !l.contains("MIN_SC_STREAM-}"));
    assert!(
        gated,
        "#32 regression: expected a positive MIN_SC_STREAM default (e.g. \
         MIN_SC_STREAM=\"${{MIN_SC_STREAM-100}}\") gating the strih→stream hop."
    );
}
