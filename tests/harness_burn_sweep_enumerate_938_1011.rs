//! #938/#1011 — the burn OFF / CHECK / RESTORE target set must be ENUMERATED from OBS reality
//! (`obs_burn_filter.py sweep-check`/`sweep-off` over `GetInputList`), never a static 3-input list
//! (#938: rig-mode `obs_burn_targets`) nor a `CAMERA_ACTIVE_SET`-derived list (#1011: recording-e2e
//! cleanup restore + `[0/8]` normalize). Live 2026-08-07: `strih:NDI cam3` (cam4's on-air feed,
//! OUTSIDE the active set) and `stream:phase2-probe-src` leaked `genlock_burn=true` past the pinned
//! lists; only the pixel proof caught it. Guard class issue 246/844.
//!
//! Static wiring guard: the shared exhaustive enumerator exists, and BOTH consumers route through
//! it. The pure enumeration/decision logic itself is proven RED->GREEN in
//! `tests/python/test_obs_burn_filter.py` against a multi-input fake WS.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn obs_burn_filter_exposes_the_exhaustive_sweep_enumerator() {
    let s = read("scripts/obs_burn_filter.py");
    assert!(
        s.contains("def ndi_source_input_names("),
        "the pure GetInputList->ndi-source-names filter must exist"
    );
    assert!(s.contains("def cmd_sweep_off("), "the sweep-off enumerator must exist");
    assert!(s.contains("def cmd_sweep_check("), "the sweep-check enumerator must exist");
    assert!(
        s.contains("GetInputList"),
        "the sweep must enumerate reality via GetInputList, never a static list"
    );
    assert!(
        s.contains("\"sweep-check\"") && s.contains("\"sweep-off\""),
        "sweep-check/sweep-off must be selectable argparse actions"
    );
}

#[test]
fn rig_mode_event_routes_burn_off_and_check_through_the_sweep() {
    let s = read("scripts/rig-mode.sh");
    assert!(
        s.contains("sweep-off"),
        "#938: rig-mode event OFF path must run the exhaustive sweep-off (not only obs_burn_targets)"
    );
    assert!(
        s.contains("sweep-check"),
        "#938: event_mode_assert item-3 must merge the exhaustive sweep-check into burn_states"
    );
    // ON stays pinned-only by design: the sweep-off must be guarded to the event (OFF) path.
    assert!(
        s.contains("if [ \"$mode\" = \"event\" ]"),
        "#938: the exhaustive sweep-off must be gated to EVENT (OFF) mode; TEST/ON stays pinned"
    );
}

#[test]
fn recording_e2e_cleanup_and_normalize_route_through_the_sweep() {
    let s = read("scripts/recording-e2e.sh");
    let n = s.matches("sweep-off").count();
    assert!(
        n >= 2,
        "#1011: recording-e2e must sweep-off in BOTH cleanup restore AND [0/8] normalize (got {n})"
    );
}
