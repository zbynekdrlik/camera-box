//! #1301 — the opt-in CG_CHAIN=1 E2E profile for the SongPlayer-originated content chain
//! (SongPlayer -> cg OBS (RESOLUME-SNV) -> strih -> stream). Two layers locked here, all Tier-0
//! (no rig, no ssh — the best-effort record/burn runners are exercised only for their return
//! codes, never a real network call):
//!  1. the pure lib `scripts/lib/cg-chain-e2e.sh` — the CG_CHAIN enable gate, the SongPlayer
//!     burn-toggle URL builder (env-overridable), and the best-effort runners that ALWAYS return 0
//!     on a no-op / disabled path so they can never trip the caller's `set -euo pipefail`;
//!  2. `recording-e2e.sh` actually WIRES the profile: sources the lib, turns the burn ON + cg OBS
//!     StartRecord at [5/8], the cleanup() leak-guard (burn OFF + StopRecord), and the `--cg`
//!     MERGE_ARGS append — all behind `cg_chain_enabled`, so a normal run is a pure no-op (a
//!     static read of the shell script, the same model as tests/harness_cbox_burn_log_persist.rs).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/cg-chain-e2e.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `snippet` under the CALLER's real `set -euo pipefail` (the exact
/// recording-e2e.sh context), returning (exit_ok, stdout_trimmed). A best-effort helper that
/// tripped `-e` on a no-op path would fail the harness here — exactly the production-abort class
/// ci-testing-gotchas.md warns about, so this MUST use `-e`, not `-uo`-only.
fn run(snippet: &str) -> (bool, String) {
    let script = format!(
        "set -euo pipefail\n. \"{}\"\n{}",
        lib_script().display(),
        snippet
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn cg_chain_enabled_is_off_by_default_and_on_at_1() {
    let (ok0, _) = run("CG_CHAIN=0; if cg_chain_enabled; then echo ON; else echo OFF; fi");
    assert!(ok0);
    let (ok1, out1) = run("CG_CHAIN=1; cg_chain_enabled && echo ON || echo OFF");
    assert!(ok1);
    assert_eq!(out1, "ON", "CG_CHAIN=1 enables the profile");
    let (_, out0) = run("CG_CHAIN=0; if cg_chain_enabled; then echo ON; else echo OFF; fi");
    assert_eq!(out0, "OFF", "CG_CHAIN unset/0 is a pure no-op");
}

#[test]
fn songplayer_burn_url_is_pure_and_env_overridable() {
    // Since the SongPlayer burn API shipped, the URL is the fixed `/api/v1/ndi/burn` endpoint and
    // on/off travels in the JSON body (tests/harness_cg_chain_e2e_1302.rs pins the body).
    let (ok, def) = run("cg_chain_songplayer_burn_url");
    assert!(ok);
    assert_eq!(def, "http://resolume.lan:8920/api/v1/ndi/burn");
    let (_, ov) = run("CG_CHAIN_SONGPLAYER_API=http://sp.test:9 cg_chain_songplayer_burn_url");
    assert_eq!(
        ov, "http://sp.test:9/api/v1/ndi/burn",
        "the burn API base is env-overridable"
    );
}

#[test]
fn cleanup_is_a_safe_no_op_returning_zero_when_disabled() {
    // Must NOT abort the caller's `set -e` cleanup() flow on a normal (CG_CHAIN unset) run.
    let (ok, _) = run("CG_CHAIN=0; cg_chain_cleanup \"\" /nonexistent.py 1; echo REACHED");
    assert!(
        ok,
        "cg_chain_cleanup must return 0 when the profile is disabled"
    );
}

#[test]
fn burn_toggle_is_best_effort_never_aborts_on_failure() {
    // An unreachable SongPlayer API (enabled profile) must warn, not abort the run.
    let (ok, _) = run(
        "CG_CHAIN=1 CG_CHAIN_SONGPLAYER_API=http://127.0.0.1:1 cg_chain_songplayer_burn off; \
         echo REACHED",
    );
    assert!(
        ok,
        "cg_chain_songplayer_burn must return 0 even when the POST fails (songplayer#151 unshipped)"
    );
}

#[test]
fn pull_without_configured_cmd_returns_nonzero_so_cg_is_omitted() {
    // No CG_CHAIN_PULL_CMD ⇒ the pull reports "not configured" and returns nonzero, so the caller's
    // `if ... cg_chain_pull_recording ...` omits --cg (the merge runs exactly as today).
    let (ok, _) = run(
        "CG_CHAIN=1; if cg_chain_pull_recording 1.2.3.4 /tmp/nope.mkv; then echo GOT; else echo NONE; fi",
    );
    // The `if` absorbs the nonzero return, so the snippet itself exits 0.
    assert!(ok);
}

// ---- recording-e2e.sh wiring (static reads — the CG_CHAIN block must stay wired) ----

#[test]
fn recording_e2e_sources_the_cg_chain_lib() {
    let s = recording_e2e_text();
    assert!(
        s.contains(". \"$HERE/lib/cg-chain-e2e.sh\""),
        "#1301: recording-e2e.sh must source scripts/lib/cg-chain-e2e.sh"
    );
}

#[test]
fn recording_e2e_wires_the_cg_chain_start_cleanup_and_merge_arg() {
    let s = recording_e2e_text();
    // [5/8] start: burn ON + cg OBS StartRecord, guarded by cg_chain_enabled.
    assert!(
        s.contains("cg_chain_songplayer_burn on"),
        "#1301: the [5/8] block must turn the SongPlayer burn ON"
    );
    assert!(
        s.contains("cg_chain_record_start"),
        "#1301: the [5/8] block must StartRecord cg OBS"
    );
    // cleanup() leak-guard: burn OFF + StopRecord, even on an early abort.
    assert!(
        s.contains("cg_chain_cleanup \"${CG_HOST_IP:-}\""),
        "#1301: cleanup() must call the cg_chain leak-guard (burn OFF + StopRecord)"
    );
    // the --cg verdict arg, appended to MERGE_ARGS only when the pull produced the file.
    assert!(
        s.contains("MERGE_ARGS+=(--cg \"$CG_RECORDING\")"),
        "#1301: the merge must feed the pulled cg OBS recording as --cg"
    );
    // every CG_CHAIN wiring site is gated so a normal run is a pure no-op.
    assert!(
        s.contains("if cg_chain_enabled;")
            || s.contains("cg_chain_enabled &&")
            || s.contains("cg_chain_enabled()"),
        "#1301: the cg_chain wiring must be gated behind cg_chain_enabled"
    );
}
