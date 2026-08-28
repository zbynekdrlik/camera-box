//! issue 1152 M4 lease-tolerance slice — the #756 projector-count preflight
//! (`scripts/recording-e2e.sh`) must expect the DRM-lease projector shape (1 Multiview + 0
//! Program) instead of the dormant shape (1 Multiview + 1 Program) once
//! `~/.camera-box/drm-output.json` arms the in-OBS DRM-lease HDMI output
//! (`.claude/rules/obs-drm-output.md`, "Known boundary" — this ticket's own STILL-OPEN follow-up
//! item (a), left by the immediately-previous M4 lane that made `obs_phase2.py::open_projectors`
//! lease-aware). In lease mode the Program is drawn by the vendored OBS DRM output directly onto
//! the leased CRTC — never an X window — so a bare `wmctrl -c 'Projector - Program'` count of
//! exactly 1 is structurally the WRONG expectation in that mode.
//!
//! Two things are locked here:
//!   1. The PURE decision function `imag_projector_lease_count_verdict` in the new
//!      `scripts/lib/imag-projector-lease-count.sh` (sourced + called directly — same
//!      convention as `tests/harness_imag_scene_route_682.rs`).
//!   2. `scripts/recording-e2e.sh` structurally sources that lib and consults it (via a python3
//!      one-liner reusing `imag_scenes.drm_output_lease_connector` — the ONE decision grammar
//!      `obs_phase2.py::_drm_lease_connector_for_host` / `imag-obs-start.sh` already use) INSIDE
//!      the existing `#756` count-check block, without disturbing the pre-existing anchors the
//!      sibling `tests/harness_projector_count_756.rs` suite already locks (`_mv_count`,
//!      `_pgm_count`, `exit 1`, the `wmctrl -l` / `Projector - Multiview` / `Projector - Program`
//!      substrings, and the banner text).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-projector-lease-count.sh")
}

const RECORDING_E2E: &str = "scripts/recording-e2e.sh";

/// Source the real lib and call `imag_projector_lease_count_verdict CONNECTOR MV PGM`. Returns
/// (exit_code, trimmed stdout, stderr).
fn verdict(connector: &str, mv: &str, pgm: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -euo pipefail; . '{}'; imag_projector_lease_count_verdict '{connector}' '{mv}' '{pgm}'",
            lib_path().display()
        ))
        .output()
        .expect("run bash");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// pure function: imag_projector_lease_count_verdict (no ssh, no python, no OBS)
// ---------------------------------------------------------------------------------------------

#[test]
fn lib_file_exists_and_defines_the_verdict_function() {
    assert!(lib_path().exists(), "scripts/lib/imag-projector-lease-count.sh must exist (issue 1152 M4 lease-tolerance slice)");
    let body = read("scripts/lib/imag-projector-lease-count.sh");
    assert!(
        body.contains("imag_projector_lease_count_verdict()"),
        "the lib must define imag_projector_lease_count_verdict()"
    );
}

#[test]
fn dormant_mode_keeps_the_pre_1152_exactly_one_each_contract() {
    // connector == "" (dormant) — byte-identical expectation to the pre-#1152 #756 contract.
    let cases = [
        ("1", "1", "ok-dormant"),
        ("0", "1", "fail-dormant"),
        ("2", "1", "fail-dormant"),
        ("1", "0", "fail-dormant"),
        ("", "", "fail-dormant"), // an unreadable/empty count must never silently pass
    ];
    for (mv, pgm, want) in cases {
        let (code, out, err) = verdict("", mv, pgm);
        assert_eq!(code, 0, "verdict(\"\", {mv}, {pgm}) stderr: {err}");
        assert_eq!(
            out, want,
            "dormant verdict(\"\", mv={mv}, pgm={pgm}) expected {want}, got {out}"
        );
    }
}

#[test]
fn lease_mode_requires_exactly_one_multiview_and_zero_program() {
    // connector != "" (drm-output lease ENABLED) — Program is DRM scanout, never an X window.
    let cases = [
        ("1", "0", "ok-lease"),
        ("1", "1", "fail-lease"), // a Program window STILL present -> genuinely inconsistent
        ("0", "0", "fail-lease"),
        ("2", "0", "fail-lease"),
        ("", "", "fail-lease"), // an unreadable/empty count must never silently pass
        ("abc", "0", "fail-lease"), // non-numeric read must never silently pass
    ];
    for (mv, pgm, want) in cases {
        let (code, out, err) = verdict("HDMI-1", mv, pgm);
        assert_eq!(code, 0, "verdict(\"HDMI-1\", {mv}, {pgm}) stderr: {err}");
        assert_eq!(
            out, want,
            "lease verdict(\"HDMI-1\", mv={mv}, pgm={pgm}) expected {want}, got {out}"
        );
    }
}

#[test]
fn a_nonempty_connector_of_any_name_is_treated_as_lease_enabled() {
    // The verdict only cares whether the connector is non-empty, not its literal value — a
    // future connector name other than "HDMI-1" must still route to the lease contract.
    let (code, out, err) = verdict("DP-1", "1", "0");
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out, "ok-lease",
        "any non-empty connector name must select the lease contract"
    );
}

// ---------------------------------------------------------------------------------------------
// scripts/recording-e2e.sh: sources the lib + consults the shared lease classifier INSIDE the
// existing #756 count-check block, without disturbing the pre-existing anchors.
// ---------------------------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_the_lease_count_lib_and_calls_the_verdict_function() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("lib/imag-projector-lease-count.sh"),
        "{RECORDING_E2E} must source scripts/lib/imag-projector-lease-count.sh"
    );
    assert!(
        s.contains("imag_projector_lease_count_verdict"),
        "{RECORDING_E2E} must call imag_projector_lease_count_verdict"
    );
}

#[test]
fn recording_e2e_derives_the_lease_connector_via_the_shared_imag_scenes_classifier() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("imag_scenes.drm_output_lease_connector")
            && s.contains("imag_scenes._drm_output_config_text"),
        "{RECORDING_E2E} must derive the lease connector via imag_scenes' shared classifier \
         pair (drm_output_lease_connector / _drm_output_config_text) -- the SAME grammar \
         obs_phase2.py::_drm_lease_connector_for_host and imag-obs-start.sh already use, never \
         a second, divergent config reader (.claude/rules/obs-drm-output.md)"
    );
}

#[test]
fn lease_branch_sits_inside_the_756_count_check_and_preserves_every_pre_existing_anchor() {
    let s = read(RECORDING_E2E);
    let idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 projector-count preflight banner must exist");
    let block = &s[idx..(idx + 2400).min(s.len())];

    // Every anchor the sibling harness_projector_count_756.rs suite already locks must survive.
    for needle in [
        "_mv_count",
        "_pgm_count",
        "exit 1",
        "wmctrl -l",
        "Projector - Multiview",
        "Projector - Program",
    ] {
        assert!(
            block.contains(needle),
            "the #756 count-check block must still contain {needle:?} (pre-existing anchor, \
             locked by tests/harness_projector_count_756.rs) -- got:\n{block}"
        );
    }

    // The new lease branch must be INSIDE this same block, not somewhere unrelated.
    assert!(
        block.contains("imag_projector_lease_count_verdict"),
        "the lease verdict call must sit inside the #756 count-check block -- got:\n{block}"
    );
    assert!(
        block.contains("ok-lease") && block.contains("ok-dormant") && block.contains("fail-lease"),
        "the count-check block must branch on all three of ok-lease/ok-dormant/fail-lease -- \
         got:\n{block}"
    );

    // A fail-lease verdict must ALSO hard-fail (exit 1), never be silently tolerated as "extra
    // is fine" -- the dispatch's own constraint: a gate that becomes a no-op in lease mode is
    // NOT acceptable.
    let fail_lease_idx = block
        .find("fail-lease")
        .expect("fail-lease case must exist in the block");
    let after_fail_lease = &block[fail_lease_idx..(fail_lease_idx + 700).min(block.len())];
    assert!(
        after_fail_lease.contains("exit 1"),
        "the fail-lease branch must hard-fail (exit 1), never silently continue -- got:\n{after_fail_lease}"
    );
}

#[test]
fn windowed_stray_heal_and_lease_derivation_run_between_open_projectors_and_the_count_check() {
    // #769's heal (order-locked by the sibling suite) must still run BEFORE the count check;
    // the NEW lease-connector derivation must also sit at/after the count computation and
    // before the branch that consumes it — i.e. nothing here regresses the #769 ordering.
    let s = read(RECORDING_E2E);
    let open_idx = s
        .find("imag-nb Multiview + Program projectors must be OPEN")
        .expect("open-projectors preflight must exist");
    let heal_idx = s
        .find("imag_projector_heal_cmds")
        .expect("the #769 heal call must exist");
    let count_idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 count preflight must exist");
    let lease_idx = s
        .find("imag_projector_lease_count_verdict")
        .expect("the lease verdict call must exist");
    let studio_idx = s
        .find("imag Studio Mode must be ON")
        .expect("the #767 studio-mode-on preflight must exist");
    assert!(
        open_idx < heal_idx
            && heal_idx < count_idx
            && count_idx < lease_idx
            && lease_idx < studio_idx,
        "expected ordering open({open_idx}) < heal({heal_idx}) < count-banner({count_idx}) < \
         lease-verdict({lease_idx}) < studio-mode({studio_idx})"
    );
}
