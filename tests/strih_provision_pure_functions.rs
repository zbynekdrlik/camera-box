//! Functional (execution) guard for `scripts/lib/strih-provision.sh`'s pure #1317 decision
//! helpers — the Linux strih notebook (`strih-lx`) role FACTS + decisions.
//!
//! Same convention as `tests/setup_imag_pure_functions.rs::run_sourced` /
//! `tests/deploy_genlock_fleet.rs`: the lib is source-only (no top-level statements, its own
//! `# airuleset:script-ok` header), so sourcing it defines only the pure functions in the
//! harness shell — no root, no network, no side effects. This closes the gap a purely textual
//! guard cannot: it catches a silent LOGIC inversion (e.g. the client-not-master check flipped,
//! or a STRIH-SNV collision guard that stops firing).
//!
//! These run on CI (Tier-0 bans local `cargo test`); a green run here proves the pure decisions
//! the strih provisioning + acceptance gate rely on.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/strih-provision.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the real lib and run `body`. Returns (exit_code, stdout, stderr). `env` is passed as
/// KEY=VALUE pairs so a test can drive the STRIH_LX_* seams without leaking into other tests.
fn run_sourced(env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", lib());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn lib_is_source_only_and_defines_the_pure_functions() {
    // Sourcing must succeed with no top-level side effects (exit 0, no stderr noise).
    let (code, _o, err) = run_sourced(&[], "type strih_lx_ndi_inputs >/dev/null");
    assert_eq!(code, 0, "sourcing the lib must succeed; stderr={err}");
}

#[test]
fn ndi_inputs_are_the_ten_role_inputs() {
    let (code, out, _e) = run_sourced(&[], "strih_lx_ndi_inputs");
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 10, "exactly 10 NDI inputs, got: {out}");
    for want in [
        "CAM1 (usb)",
        "CAM7 (usb)",
        "STRIH-SNV (2ME PGM)",
        "STRIH-SNV (2ME PVW)",
        "RESOLUME-SNV (cg-obs)",
    ] {
        assert!(lines.contains(&want), "missing input {want} in: {out}");
    }
}

#[test]
fn ndi_outputs_and_republishes_are_namespaced_strih_lx_never_strih_snv() {
    let (_c, outs, _e) = run_sourced(&[], "strih_lx_ndi_outputs; strih_lx_ndi_republishes");
    for l in outs.lines().filter(|l| !l.is_empty()) {
        assert!(
            l.starts_with("STRIH-LX ("),
            "every output/republish must be STRIH-LX-namespaced, got: {l}"
        );
        assert!(
            !l.starts_with("STRIH-SNV "),
            "a STRIH-SNV sender leaked: {l}"
        );
    }
    assert!(outs.contains("STRIH-LX (2ME PGM)"));
    assert!(outs.contains("STRIH-LX (2ME PVW)"));
    assert!(outs.contains("STRIH-LX (MULTIVIEW)"));
}

#[test]
fn camera_latency_is_the_rig_floor_three() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_camera_latency_ms");
    assert_eq!(out.trim(), "3");
}

#[test]
fn bundle_artifact_is_the_strih_variant() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_bundle_artifact");
    assert_eq!(out.trim(), "obs-genlock-linux-x86_64-strih");
}

#[test]
fn dantesync_client_args_point_at_the_ntp_server_and_never_server_mode() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_dantesync_client_args");
    assert!(out.contains("--ntp-server strih.lan"), "got: {out}");
    assert!(
        !out.contains("server_mode") && !out.contains("--master"),
        "must not be master: {out}"
    );
    // Overridable NTP server seam.
    let (_c2, out2, _e2) = run_sourced(
        &[("STRIH_LX_NTP_SERVER", "strih2.lan")],
        "strih_lx_dantesync_client_args",
    );
    assert!(
        out2.contains("--ntp-server strih2.lan"),
        "override ignored: {out2}"
    );
}

#[test]
fn dantesync_client_check_is_fail_closed_and_rejects_master_modes() {
    // Client modes pass.
    for mode in ["client", "ntp-server=strih.lan", "slave"] {
        let (code, _o, _e) = run_sourced(
            &[],
            &format!("strih_lx_dantesync_is_client_not_master '{mode}'"),
        );
        assert_eq!(code, 0, "client mode '{mode}' should pass");
    }
    // Master/server/empty must FAIL (fail-closed).
    for mode in ["ntp_server_mode", "server", "master", "grandmaster", ""] {
        let (code, _o, _e) = run_sourced(
            &[],
            &format!("strih_lx_dantesync_is_client_not_master '{mode}'"),
        );
        assert_ne!(code, 0, "master/empty mode '{mode}' must fail-closed");
    }
}

#[test]
fn profile_facts_carry_the_windows_light_profile_shape() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_profile_facts");
    for want in [
        "base_res=1920x1080",
        "fps=30",
        "color_format=NV12",
        "out_mode=Advanced",
        "rec_encoder=obs_nvenc_hevc_tex",
        "rec_path=/srv/_REC",
        "rec_format=mkv",
        "rec_split_min=15",
    ] {
        assert!(out.contains(want), "profile fact missing {want} in: {out}");
    }
}

#[test]
fn audio_route_is_a_fail_loud_todo_until_wired() {
    // Unwired (default) -> the predicate fails, so setup-strih's audio step FAILS loud.
    let (code, _o, _e) = run_sourced(&[], "strih_lx_audio_route_wired");
    assert_ne!(
        code, 0,
        "audio route must read UNWIRED by default (fail-loud TODO)"
    );
    // Explicitly wired -> passes.
    let (code2, _o2, _e2) = run_sourced(
        &[("STRIH_LX_AUDIO_WIRED", "1")],
        "strih_lx_audio_route_wired",
    );
    assert_eq!(
        code2, 0,
        "audio route must read wired when STRIH_LX_AUDIO_WIRED=1"
    );
    let (_c, name, _e) = run_sourced(&[], "strih_lx_audio_input_name");
    assert_eq!(name.trim(), "MiniFuse 4");
}

#[test]
fn output_name_ok_accepts_strih_lx_and_rejects_strih_snv() {
    let (c1, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'STRIH-LX (2ME PGM)'");
    assert_eq!(c1, 0);
    let (c2, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'STRIH-SNV (2ME PGM)'");
    assert_ne!(c2, 0, "a STRIH-SNV output name must be rejected");
    let (c3, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'random'");
    assert_ne!(c3, 0);
}

#[test]
fn no_second_strih_snv_sender_guard_fires_on_a_collision() {
    // A clean strih-lx-only output set passes.
    let (code, _o, _e) = run_sourced(
        &[],
        "printf '%s\\n' 'STRIH-LX (2ME PGM)' 'STRIH-LX (MULTIVIEW)' | strih_lx_no_second_strihsnv_sender",
    );
    assert_eq!(code, 0, "a clean STRIH-LX-only output set must pass");
    // A STRIH-SNV name in the live set must fail (never a 2nd STRIH-SNV sender).
    let (code2, _o, _e) = run_sourced(
        &[],
        "printf '%s\\n' 'STRIH-LX (2ME PGM)' 'STRIH-SNV (2ME PGM)' | strih_lx_no_second_strihsnv_sender",
    );
    assert_ne!(code2, 0, "a second STRIH-SNV sender must be rejected");
}
