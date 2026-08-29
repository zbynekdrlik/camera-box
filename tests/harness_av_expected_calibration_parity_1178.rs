//! #1178 parity gate — the `recording-e2e.sh` harness default for `AV_EXPECTED_MS` (the value the
//! production A/V gate is invoked with, since the harness ALWAYS passes `--av-expected-ms`
//! explicitly by its own design) MUST match recording-verdict's calibrated
//! `av_window::RIG_VIDEO_LEG_OFFSET_MS` — the single source of truth for the fixed rig video-leg.
//! The value necessarily lives in two places (Rust const + shell default); this gate stops them
//! silently drifting apart, which would gate the whole fleet at the wrong centre. Pure Tier-0
//! (reads source text at runtime, no probe, no rig).

use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Parse `<N>` from `pub const RIG_VIDEO_LEG_OFFSET_MS: f64 = <N>;` in src/av_window.rs.
fn rust_constant_ms() -> f64 {
    let src = read("src/av_window.rs");
    let marker = "pub const RIG_VIDEO_LEG_OFFSET_MS: f64 =";
    let after = src
        .split(marker)
        .nth(1)
        .expect("RIG_VIDEO_LEG_OFFSET_MS const not found in src/av_window.rs");
    let val: String = after
        .trim_start()
        .chars()
        .take_while(|c| *c != ';')
        .collect();
    val.trim()
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("could not parse RIG_VIDEO_LEG_OFFSET_MS value {val:?}: {e}"))
}

/// Parse `<N>` from `AV_EXPECTED_MS="${AV_EXPECTED_MS:-<N>}"` in scripts/recording-e2e.sh.
fn shell_default_ms() -> f64 {
    let src = read("scripts/recording-e2e.sh");
    let marker = "AV_EXPECTED_MS=\"${AV_EXPECTED_MS:-";
    let after = src
        .split(marker)
        .nth(1)
        .expect("AV_EXPECTED_MS default assignment not found in scripts/recording-e2e.sh");
    let val: String = after.chars().take_while(|c| *c != '}').collect();
    val.trim()
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("could not parse AV_EXPECTED_MS shell default {val:?}: {e}"))
}

#[test]
fn harness_av_expected_default_mirrors_the_rust_calibration_1178() {
    let rust = rust_constant_ms();
    let shell = shell_default_ms();
    assert!(
        (rust - shell).abs() < 1e-9,
        "recording-e2e.sh AV_EXPECTED_MS default ({shell}) must MIRROR \
         av_window::RIG_VIDEO_LEG_OFFSET_MS ({rust}); a drift gates the fleet at the wrong centre — \
         re-derive BOTH from the same E2E cluster median when the rig video chain changes"
    );
    // #1178 RE-DERIVATION (2026-08-29): the −92 calibration was a stale-painter artifact (issue
    // 1138 class); with the marker delay now compensated AT SOURCE, the calibrated video-leg is
    // 0.0. A future non-zero value is legitimate ONLY after a rig-verified physical video-chain
    // change (grabber/monitor/camera swap) with fresh full-fleet verdict evidence — never a
    // silent re-drift back to a stale number.
    assert!(
        (rust - 0.0).abs() < 1e-9,
        "the recalibrated video-leg must be 0.0 (marker delay compensated at source, issue 1138); \
         got {rust} — re-derive with fresh full-fleet verdict evidence if the physical video chain \
         genuinely changed"
    );
}
