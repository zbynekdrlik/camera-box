//! issue 1261 — the `[4d1/8]` MV-fps floor preflight had two stacked defects that this locks:
//!
//! 1. BLIND ON CI. `mv-fps-gate` was absent from BOTH probe-binary resolution lists in
//!    `scripts/recording-e2e.sh` — the `USE_PREBUILT_PROBE_DIR` presence/chmod loop AND the
//!    local-build fallback. `full-path-e2e.yml` does not set `USE_PREBUILT_PROBE_DIR`, so the E2E
//!    run takes the local-build branch, `target/release/mv-fps-gate` was never built, the gate exec
//!    failed, and `mv_fps_verdict` mapped that to UNKNOWN — the gate NEVER decided on CI (live NOTE
//!    path `target/release/mv-fps-gate`, run 33513175938). ci.yml already builds+uploads
//!    `mv-fps-gate` in probe-tools-linux-amd64, so the fix is the two missing entries, mirroring
//!    `frozen-camera-gate`/`render-budget-gate` (which appear in BOTH lists).
//!
//! 2. OBSERVER EFFECT. `[4d1/8]` ran AFTER `[4b/8]` turned measurement burns ON, so it measured the
//!    harness-burdened MV state; the gate's stated intent (issue 771/1091) is a PRE-EXISTING
//!    collapse. The step must run BEFORE the `[4b/8]` burns-ON gate.
//!
//! These are static-anchor (text-position) assertions over recording-e2e.sh — the same convention
//! the sibling harness_*.rs anchor tests use. RED before the fix, GREEN after.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The line (to the next '\n') that begins at the first occurrence of the unique substring `anchor`.
fn line_containing<'a>(s: &'a str, anchor: &str) -> &'a str {
    let at = s
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor not found in recording-e2e.sh: {anchor}"));
    let end = at + s[at..].find('\n').unwrap_or(s.len() - at);
    &s[at..end]
}

// -------------------------------------------------------------------------------------------
// Defect 2 — ordering: the MV-fps floor preflight must run BEFORE the [4b/8] burns-ON gate.
// -------------------------------------------------------------------------------------------
#[test]
fn mv_fps_preflight_runs_before_the_burns_on_gate_1261() {
    let s = e2e();
    let mvfps = s
        .find("[4d1/8] #771")
        .expect("recording-e2e.sh must still have the [4d1/8] #771 MV-fps preflight banner");
    let burns_on = s
        .find("[4b/8] #195/#257")
        .expect("recording-e2e.sh must still have the [4b/8] #195/#257 burn-ON gate banner");
    assert!(
        mvfps < burns_on,
        "issue 1261: the [4d1/8] MV-fps floor preflight must run BEFORE the [4b/8] burns-ON gate \
         (measure the production-shaped, burns-OFF state — never the harness-burdened one). \
         [4d1/8] byte offset {mvfps} vs [4b/8] {burns_on}"
    );
}

// -------------------------------------------------------------------------------------------
// Defect 1 — CI resolution: mv-fps-gate must be resolved the SAME way the other gate binaries
// are — present in the USE_PREBUILT_PROBE_DIR presence/chmod loop AND built in the local-build
// fallback — so a CI run (either branch) carries an executable binary and the gate decides.
// -------------------------------------------------------------------------------------------
#[test]
fn mv_fps_gate_is_in_the_prebuilt_probe_dir_presence_check_1261() {
    let s = e2e();
    let loop_line = line_containing(&s, "for b in camera-box frame-probe recording-verdict");
    assert!(
        loop_line.contains("mv-fps-gate"),
        "issue 1261: the USE_PREBUILT_PROBE_DIR presence/chmod loop must list mv-fps-gate (the CI \
         download-artifact strips the exec bit; only this loop chmod +x'es a binary). Got:\n{loop_line}"
    );
}

#[test]
fn mv_fps_gate_is_built_in_the_local_build_fallback_1261() {
    let s = e2e();
    let build_line = line_containing(&s, "cargo build --release --bin frozen-camera-gate");
    assert!(
        build_line.contains("--bin mv-fps-gate"),
        "issue 1261: the local-build fallback must build the mv-fps-gate bin (full-path-e2e.yml \
         does not set USE_PREBUILT_PROBE_DIR, so it takes this branch — else target/release/\
         mv-fps-gate is never built and the gate stays blind). Got:\n{build_line}"
    );
}
