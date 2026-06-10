//! Regression guard for #22 (Phase-2 OBS routing must not accumulate per-run inputs).
//!
//! The original `obs_phase2.py setup()` created a `os.getpid()`-suffixed scene + NDI
//! `ndi_source` input on EVERY run (`PHASE2-PROBE-<pid>` / `phase2-probe-src-<pid>`).
//! `teardown`'s `RemoveInput` no-ops on the production DistroAV fork, so each run left a
//! new dormant `ndi_source` behind — they accumulated and cluttered the production OBS
//! audio mixer (24 stuck inputs observed across strih + stream).
//!
//! The #22 redesign (Option A) reuses ONE stable-named scene + input per box across all
//! runs: setup ensures they exist and re-points the input at the run's upstream; teardown
//! idles the receiver (clears `ndi_source_name`) but KEEPS the scene+input for reuse. Net
//! footprint: exactly one dormant probe artifact per box, forever — never per-run growth.
//!
//! This test reads the script statically (it does NOT run python or touch OBS). If anyone
//! reintroduces per-run name suffixing, it fails.

use std::fs;

fn obs_py() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/obs_phase2.py");
    fs::read_to_string(path).expect("read scripts/obs_phase2.py")
}

/// The probe scene/input names MUST be stable constants reused across runs — never a
/// per-run `os.getpid()` (or any per-run token) suffix, which made DistroAV `ndi_source`
/// inputs accumulate because the fork's `RemoveInput` no-ops.
#[test]
fn phase2_obs_uses_stable_reusable_names_not_per_run_pid() {
    let py = obs_py();
    assert!(
        !py.contains("getpid"),
        "#22 regression: obs_phase2.py must NOT suffix the probe scene/input with \
         os.getpid() (or any per-run token). That created a fresh ndi_source on every run, \
         and the DistroAV fork's RemoveInput no-ops, so they accumulated in the production \
         OBS audio mixer. Reuse ONE stable-named scene+input instead."
    );
    assert!(
        py.contains("PHASE2-PROBE") && py.contains("phase2-probe-src"),
        "#22: expected the stable constant names PHASE2-PROBE / phase2-probe-src to be \
         present and reused across runs."
    );
    // No f-string interpolation building those names (e.g. f\"PHASE2-PROBE-{pid}\").
    assert!(
        !py.contains("PHASE2-PROBE-{") && !py.contains("phase2-probe-src-{"),
        "#22 regression: the probe scene/input names must be stable constants, not \
         f-strings interpolating a per-run suffix."
    );
}

/// Teardown must KEEP the stable scene+input (idle the receiver, don't destroy it) so the
/// next run reuses them — destroying-and-recreating is what caused the accumulation.
#[test]
fn phase2_obs_teardown_keeps_stable_input_for_reuse() {
    let py = obs_py();
    let td = py
        .find("def teardown(")
        .expect("teardown() not found in obs_phase2.py");
    let body = &py[td..];
    // Teardown must clear ndi_source_name (idle the NDI receiver cleanly) ...
    assert!(
        body.contains(r#""ndi_source_name": """#),
        "#22: teardown must clear ndi_source_name to idle the NDI receiver before leaving \
         the stable input dormant."
    );
    // ... but must NOT RemoveInput/RemoveScene — reuse is what prevents per-run growth.
    assert!(
        !body.contains("RemoveInput") && !body.contains("RemoveScene"),
        "#22 regression: teardown must NOT RemoveInput/RemoveScene the stable probe \
         scene+input — they are reused across runs. Destroying them per run (and the \
         fork's RemoveInput no-op) is exactly what made inputs accumulate."
    );
}
