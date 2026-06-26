//! #240 — the windows-genlock SHA-manifest step must gate PRs that touch the manifest
//! CHECKER LOGIC, on a real Windows runner.
//!
//! Background: #120's manifest self-consistency step (`scripts/genlock-manifest.sh --check`)
//! shipped a Windows-broken bug (#239: a SIGPIPE/pipefail checker race) that false-failed the
//! ENTIRE deployable genlock bundle on the real ~2000-file Windows build — yet it passed every
//! PR. Root cause of the COVERAGE gap: the full Windows genlock build is `workflow_dispatch`-only
//! (not a PR gate) and the manifest LOGIC is only Linux-unit-tested (`tests/genlock_manifest.rs`).
//! The 5-file synthetic Linux stages never hit the race; the real Windows bundle did. And
//! `windows-genlock-fast.yml` only fires on `vendor/obs-studio/**` / `vendor/distroav/**` paths,
//! so a change to the manifest CHECKER itself (`scripts/genlock-manifest.sh` /
//! `tests/genlock_manifest.rs`) shipped ungated — the #120 Windows-broken class could land again.
//!
//! This repo gates merges via push-to-dev (it deliberately OMITS `pull_request`, #157), so the
//! push-to-dev run IS the pre-merge gate. The fix (#240) adds a dedicated, CHEAP Windows job that
//! fires on `push:[dev]` for the manifest-checker paths and runs the manifest generate+check over
//! a SYNTHETIC real-sized stage (~1500+ dummy files laid out like the bundle) — NO OBS/DistroAV
//! compile (no 150-min build) — so the git-bash-specific behaviors (process-substitution, large-
//! file sha, locale/collation) the Linux test can't reach are exercised pre-merge.
//!
//! These are STRUCTURAL guards on the workflow YAML: they fail loudly if a future edit drops the
//! gate, its push-to-dev trigger, its manifest-checker path filter, or the generate+check teeth.
//! They run on every push, on any host, with no Windows toolchain. The DEFINITIVE proof is the CI
//! job itself running the manifest on a windows-2022 runner at scale.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

const GATE: &str = ".github/workflows/windows-manifest-gate.yml";

/// The dedicated manifest gate workflow must exist and run on a real WINDOWS runner — the
/// whole point of #240 is to exercise the manifest on git-bash (windows-2022), which the
/// Linux `test` job cannot do.
#[test]
fn manifest_gate_runs_on_a_windows_runner() {
    let wf = read(GATE);
    assert!(
        wf.contains("windows-2022"),
        "#240: {GATE} must run on a windows-2022 runner — the manifest must be exercised on \
         git-bash (the #239 SIGPIPE/comm-collation race is Windows-specific; the Linux test \
         can't reach it)."
    );
}

/// The gate must FIRE on push to dev (this repo's pre-merge gate — it omits pull_request per
/// #157) AND its path filter must include the manifest CHECKER logic (`scripts/genlock-manifest.sh`
/// + `tests/genlock_manifest.rs`) — exactly the files `windows-genlock-fast.yml` does NOT watch,
/// so a checker-logic change is no longer shipped ungated (#240).
#[test]
fn manifest_gate_triggers_on_push_dev_for_the_checker_paths() {
    let wf = read(GATE);
    assert!(
        wf.contains("push:") && wf.contains("branches: [dev]"),
        "#240: the gate must trigger on push to dev (the pre-merge gate here; pull_request is \
         omitted per #157, so push-to-dev is what populates the dev->main PR checks)."
    );
    assert!(
        wf.contains("scripts/genlock-manifest.sh"),
        "#240: the gate's path filter must include scripts/genlock-manifest.sh — a change to the \
         manifest CHECKER logic is exactly what windows-genlock-fast.yml's vendor-only path \
         filter never re-checked (the ungated #120 class)."
    );
    assert!(
        wf.contains("tests/genlock_manifest.rs"),
        "#240: the gate's path filter must include tests/genlock_manifest.rs — a change to the \
         manifest tests must re-run the Windows manifest gate too."
    );
}

/// The gate must actually GENERATE the manifest over a synthetic stage AND run the
/// self-consistency CHECK with teeth (FAIL on extra/missing/sha-drift) — both invocations of
/// the real script, not a stub. This is the behavior the #239 race broke.
#[test]
fn manifest_gate_generates_and_checks_the_manifest() {
    let wf = read(GATE);
    assert!(
        wf.contains("genlock-manifest.sh --stage") || wf.contains("genlock-manifest.sh\" --stage"),
        "#240: the gate must GENERATE the manifest (genlock-manifest.sh --stage <synthetic stage>)."
    );
    assert!(
        wf.contains("genlock-manifest.sh --check") || wf.contains("genlock-manifest.sh\" --check"),
        "#240: the gate must run the self-consistency CHECK (genlock-manifest.sh --check ... \
         --stage ...) — the step the #239 SIGPIPE/comm race broke on the real Windows bundle."
    );
}

/// The synthetic stage must be REAL-SIZED (~1500+ files), not the 5-file Linux unit-test stage —
/// the #239 race only surfaces at the ~2000-file bundle scale (the per-item grep -q SIGPIPE under
/// pipefail). A tiny stage would re-create the exact blind spot this gate exists to close.
#[test]
fn manifest_gate_uses_a_real_sized_synthetic_stage() {
    let wf = read(GATE);
    assert!(
        wf.contains("1600"),
        "#240: the gate must build a real-sized synthetic stage (~1500+ files; 1600 here) so the \
         git-bash-scale behaviors (process-sub truncation, large-file sha, locale/collation, the \
         #239 SIGPIPE/comm race) are actually exercised — a 5-file stage would miss them."
    );
}

/// The gate must stay CHEAP: NO OBS/DistroAV compile (no `cmake`), so it is a ~2-min synthetic
/// manifest check, never the 150-min vendored build. This is what makes it viable as a per-push
/// gate (option 1 in #240, the preferred cheapest fix).
#[test]
fn manifest_gate_does_not_compile_obs_or_distroav() {
    let wf = read(GATE);
    assert!(
        !wf.contains("cmake"),
        "#240: the gate must NOT compile OBS/DistroAV (no cmake) — it is a cheap synthetic-stage \
         manifest check, never the 150-min vendored build. That cheapness is what lets it gate \
         every relevant push."
    );
    assert!(
        !wf.contains("vendor/obs-studio/**") && !wf.contains("vendor/distroav/**"),
        "#240: the manifest gate must not be triggered by / coupled to the vendored OBS/DistroAV \
         build paths — those are windows-genlock-fast.yml's job; this gate watches the manifest \
         CHECKER logic and stays compile-free."
    );
}

/// No `continue-on-error` — a manifest inconsistency (extra/missing/sha-drift) MUST fail the
/// push-to-dev run, never be hidden behind a fake-green check (#240 acceptance + no-continue-on-error).
#[test]
fn manifest_gate_has_no_continue_on_error() {
    let wf = read(GATE);
    assert!(
        !wf.contains("continue-on-error"),
        "#240: the manifest gate must have NO continue-on-error — an inconsistent manifest must \
         fail the gate with full teeth (acceptance: extra/missing/sha-drift still fail)."
    );
}
