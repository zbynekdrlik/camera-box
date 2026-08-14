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
    assert!(
        s.contains("def cmd_sweep_off("),
        "the sweep-off enumerator must exist"
    );
    assert!(
        s.contains("def cmd_sweep_check("),
        "the sweep-check enumerator must exist"
    );
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
    // Anchor on the INVOCATION form (a real `obs_burn_filter.py" sweep-*` call), not the bare
    // token — the words "sweep-off"/"sweep-check" also appear in this PR's own comments, so a
    // deleted invocation must still fail the guard.
    assert!(
        s.contains("obs_burn_filter.py\" sweep-off"),
        "#938: rig-mode event OFF path must INVOKE obs_burn_filter.py sweep-off (not only obs_burn_targets)"
    );
    assert!(
        s.contains("obs_burn_filter.py\" sweep-check"),
        "#938: event_mode_assert item-3 must INVOKE obs_burn_filter.py sweep-check into burn_states"
    );
    assert!(
        s.contains("__sweep_unreachable__"),
        "#1011: the item-3 sweep-check merge must FAIL CLOSED (inject a failing sentinel) when the \
         enumeration itself fails — never silently add nothing"
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
    // Count real INVOCATIONS, not the token (which also appears in this PR's comments).
    let n = s.matches("obs_burn_filter.py\" sweep-off").count();
    assert!(
        n >= 2,
        "#1011: recording-e2e must INVOKE obs_burn_filter.py sweep-off in BOTH cleanup restore AND \
         [0/8] normalize (got {n})"
    );
}
