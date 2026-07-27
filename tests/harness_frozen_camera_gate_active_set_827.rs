//! #827 follow-up (2026-07-28) — the frozen-camera-gate preflight (and its two siblings) still
//! sampled RETIRED cameras.
//!
//! Live hardware gate run 30310110884 got past the `[0/8]` fleet preflight correctly (cam4
//! EXCLUDED as operator-acknowledged offline), then died in the `[1/8]` frozen-camera-gate
//! MV-liveness preflight:
//!
//! ```text
//! [frozen-camera-gate] timeline JSON: {"NDI cam1": [...], "NDI cam2": [...], "NDI cam3": [...],
//!                                      "NDI cam5": [null, null], "NDI cam6": [null, null], "NDI cam7": [null, null]}
//! [frozen-camera-gate] FAIL — FROZEN: NDI cam5, NDI cam6, NDI cam7
//! ```
//!
//! Root cause: three separate loops in `scripts/recording-e2e.sh` enumerated the fleet via a
//! LITERAL `for _n in 1 2 3 4 5 6 7` range and only subtracted `PREFLIGHT_EXCLUDED_CAMS` (the
//! TEMPORARY acked-offline list) — never intersecting with `CAMERA_ACTIVE_SET` (the PERMANENT
//! retired-fleet list, #827). cam5/cam6/cam7 are retired (not merely acked), so they were never
//! evaluated for exclusion at all and stayed in every derived source list.
//!
//! These are pure content-assert guards (same discipline as
//! `tests/harness_render_health_divisor_758.rs`) proving all three call sites now derive from
//! `CAMERA_ACTIVE_SET` (via the new `camera_active_excluding` /
//! `camera_active_ndi_sources_excluding_csv` helpers in `scripts/camera-set.sh` — see
//! `tests/harness_camera_set.rs` for the functional fixture tests of those helpers themselves)
//! instead of a hardcoded range. The functional proof that a retired camera is actually excluded
//! (and a reactivated one flows back in) lives in `tests/harness_camera_set.rs`.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn camera_set() -> String {
    let path = manifest_dir().join("scripts/camera-set.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn recording_e2e_no_longer_enumerates_the_fleet_via_a_literal_1_through_7_range() {
    // The exact bug shape, repo-wide: a hardcoded `1 2 3 4 5 6 7` range can never respect
    // CAMERA_ACTIVE_SET, no matter what exclusion logic sits next to it. All three call sites
    // (the [0/8] genlock_burn check, the [1/8] frozen-camera MV-liveness preflight, and the
    // [5/8 pre] live-freeze-watch arming) must derive from CAMERA_ACTIVE_SET instead.
    let s = recording_e2e();
    assert!(
        !s.contains("for _n in 1 2 3 4 5 6 7"),
        "#827: recording-e2e.sh must not enumerate the camera fleet via a literal 1..7 range \
         anywhere -- every consumer must derive from CAMERA_ACTIVE_SET (camera-set.sh), or a \
         retired camera (cam5/cam6/cam7) silently reappears in a sampled/checked source list."
    );
}

#[test]
fn camera_set_declares_the_active_excluding_helpers() {
    let s = camera_set();
    assert!(
        s.contains("camera_active_excluding()"),
        "#827: scripts/camera-set.sh must declare camera_active_excluding() — the single \
         derivation point for 'active minus acked-offline' cam-name lists."
    );
    assert!(
        s.contains("camera_active_ndi_sources_excluding_csv()"),
        "#827: scripts/camera-set.sh must declare camera_active_ndi_sources_excluding_csv() — \
         the single derivation point for 'active minus acked-offline' NDI-source CSV lists."
    );
}

#[test]
fn preflight_genlock_burn_check_derives_from_camera_active_set() {
    // [0/8] genlock_burn-must-be-OFF pre-check.
    let s = recording_e2e();
    let idx = s
        .find("genlock_burn OFF on every strih NDI input")
        .expect("the [0/8] genlock_burn pre-check must exist");
    let window = &s[idx..(idx + 600).min(s.len())];
    assert!(
        window.contains("camera_active_excluding"),
        "#827: the [0/8] genlock_burn pre-check must derive its checked camera list from \
         camera_active_excluding(), not a literal range. Window:\n{window}"
    );
}

#[test]
fn frozen_camera_mv_liveness_preflight_derives_from_camera_active_set() {
    // [1/8] frozen-camera-gate MV-liveness preflight -- the ACTUAL call site that failed live.
    let s = recording_e2e();
    let idx = s
        .find("PREFLIGHT_MV_SOURCES")
        .expect("the [1/8] MV-liveness preflight must exist");
    let window = &s[idx..(idx + 600).min(s.len())];
    assert!(
        window.contains("camera_active_ndi_sources_excluding_csv"),
        "#827: the [1/8] frozen-camera-gate MV-liveness preflight (PREFLIGHT_MV_SOURCES) must \
         derive its sampled source list from camera_active_ndi_sources_excluding_csv(), not a \
         literal 1..7 range -- this is the exact call site that sampled retired NDI cam5/cam6/ \
         cam7 and failed FROZEN on live run 30310110884. Window:\n{window}"
    );
}

#[test]
fn live_freeze_watch_arming_derives_from_camera_active_set() {
    // [5/8 pre] in-run live-freeze-watch arming.
    let s = recording_e2e();
    let idx = s
        .find("FREEZE_WATCH_SOURCES")
        .expect("the [5/8 pre] live-freeze-watch arming must exist");
    let window = &s[idx..(idx + 600).min(s.len())];
    assert!(
        window.contains("camera_active_ndi_sources_excluding_csv"),
        "#827: the [5/8 pre] live-freeze-watch arming (FREEZE_WATCH_SOURCES) must derive its \
         watched source list from camera_active_ndi_sources_excluding_csv(), not a literal 1..7 \
         range. Window:\n{window}"
    );
}
