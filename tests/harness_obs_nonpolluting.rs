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

/// #22 verification exposed a strih→stream flake: setup resolved NDI names with a single
/// best-effort `_match_full` against cold DistroAV discovery, so when the full
/// `MACHINE (name)` form wasn't discovered yet it bound the BARE name (e.g. `2ME PGM`) —
/// which connects to nothing → stream rendered black → 0 decode. The fix polls discovery
/// until the full name resolves. This pins that the resolution polls, not races.
#[test]
fn phase2_obs_resolves_ndi_names_by_polling_discovery() {
    let py = obs_py();
    assert!(
        py.contains("def _resolve_full(") && py.contains("time.sleep"),
        "#22: obs_phase2.py must resolve NDI names by POLLING discovery (a _resolve_full \
         helper that waits for the full 'MACHINE (name)' form), not race it — binding the \
         bare Main-Output name connects to nothing (black render, 0 decode downstream)."
    );
    // #22: BOTH the ingest source AND this box's own Main Output must be resolved via
    // _resolve_full (poll-until-full), so a cold-discovery run never binds a bare,
    // unconnectable NDI name (the strih→stream black-render flake). #91 factored the
    // OWN-output resolution out of setup() into the _resolve_own_output helper (which
    // setup() calls), so the two _resolve_full call sites now live across setup() +
    // _resolve_own_output — count them together to keep guarding the same invariant.
    let su = py.find("def setup(").expect("setup() not found");
    let su_end = py[su..].find("\ndef ").map(|i| su + i).unwrap_or(py.len());
    let setup_body = &py[su..su_end];
    let ro = py
        .find("def _resolve_own_output(")
        .expect("#91: _resolve_own_output helper not found");
    let ro_end = py[ro..].find("\ndef ").map(|i| ro + i).unwrap_or(py.len());
    let resolve_own_body = &py[ro..ro_end];
    let resolve_full_calls = setup_body.matches("_resolve_full(").count()
        + resolve_own_body.matches("_resolve_full(").count();
    assert!(
        resolve_full_calls >= 2,
        "#22: setup() (+ the #91 _resolve_own_output helper it calls) must resolve BOTH \
         the ingest source AND this box's own Main Output via _resolve_full (poll-until-\
         full), so a cold-discovery run never binds a bare, unconnectable NDI name (the \
         strih→stream black-render flake). Found {resolve_full_calls} _resolve_full call(s)."
    );
}

/// Review hardening (#22): the per-host prev-scene state file is the ONLY source teardown
/// uses to restore the production program scene. If a crash leaves it corrupt/truncated and
/// the load isn't corruption-safe, teardown raises before restoring → the probe scene is
/// stranded as live program on a production OBS. So the load must tolerate a corrupt file
/// (not just a missing one), and the write must be atomic so a crash can't create that
/// corrupt file in the first place.
#[test]
fn phase2_obs_state_io_is_corruption_safe_and_atomic() {
    let py = obs_py();
    assert!(
        py.contains("ValueError"),
        "#22 safety: state-file load must catch ValueError (JSONDecodeError), not only \
         FileNotFoundError, so a corrupt /tmp state file can't make teardown raise before \
         it restores the prior program scene (stranding the probe scene as live program)."
    );
    assert!(
        py.contains("os.replace("),
        "#22 safety: the state write must be atomic (tmp file + os.replace), so a crash \
         mid-write can't leave a corrupt state file in the first place."
    );
}
