//! #1066 (provisioning-defects batch) -- four defects found re-provisioning cam1 from a clean
//! noble image on 2026-09-13, each fixed in `scripts/setup-device.sh` (+ a `verify-device.sh`
//! constant move and a new `scripts/lib/ndi-provision.sh`):
//!
//!   D1. STEP 17b remoteos-mcp install died on the noble pip-vs-debian RECORD conflict; the fix
//!       exports `PIP_BREAK_SYSTEM_PACKAGES=1 PIP_IGNORE_INSTALLED=1` for the installer's own pip.
//!   D2. STEP 4 fetched libndi from ONE hard-coded peer (`NDI_PEER`=cam1); re-provisioning cam1
//!       itself hit a "nothing to fetch" dead-end. The fix derives an ordered peer list from
//!       camera-set.sh (`ndi_bootstrap_peer_list`, excluding this box), tries each, then falls back
//!       to a version-guarded pinned download, and fails loud naming every peer tried.
//!   D3. The default binary source was the dev tip; the fix defaults to `main` (the fleet's pin,
//!       matching deploy-fleet.sh) with `--binary` / `--run` overrides.
//!   D4. STEP 17 wrote the RESOLVED grandmaster IPv4 into `gm_allowlist` and installed
//!       `releases/latest`; the fix writes the literal `video-clock.lan` hostname (dantesync#113,
//!       kept only as a loud resolve precondition) and installs the pinned `DANTESYNC_VERSION_PIN`.
//!
//! Tier-0: pure functions are driven by sourcing the real shell libs; the script wiring is pinned
//! by static-text anchors (same convention as `setup_device_fleet_binary_ndi.rs` /
//! `setup_device_remoteos_mcp_1066.rs`).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn setup() -> String {
    read("scripts/setup-device.sh")
}
fn verify() -> String {
    read("scripts/verify-device.sh")
}

/// True if `needle` appears on a line that is NOT a `#` comment (mirrors the sibling tests'
/// `on_noncomment_line` helper): a comment mentioning the string cannot satisfy a wiring assertion.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Source `scripts/camera-set.sh` + `scripts/lib/ndi-provision.sh` and run `body` against the pure
/// functions. Returns (exit_code, stdout, stderr).
fn run_ndi(env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let dir = manifest_dir();
    let harness = format!(
        "set -uo pipefail\n. \"{cs}\"\n. \"{lib}\"\n{body}",
        cs = dir.join("scripts/camera-set.sh").display(),
        lib = dir.join("scripts/lib/ndi-provision.sh").display(),
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness);
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

// ============================================================================
// scripts/lib/ndi-provision.sh -- pure functions (D2 core)
// ============================================================================

/// Re-provisioning cam1 (self == the old hard-coded peer 10.77.9.61) must yield the OTHER fleet
/// boxes as peers, never a self-only dead end.
#[test]
fn ndi_bootstrap_peer_list_excludes_self_when_provisioning_cam1_1066() {
    let (code, out, err) = run_ndi(
        &[
            ("NDI_PEER", "10.77.9.61"),
            ("CAMERA_SET", "cam1 cam2 cam3 cam4"),
        ],
        r#"ndi_bootstrap_peer_list 10.77.9.61 | tr '\n' ' '"#,
    );
    assert_eq!(code, 0, "peer-list must succeed; stderr: {err}");
    assert_eq!(
        out.trim(),
        "10.77.9.62 10.77.9.63 10.77.9.64",
        "provisioning cam1 must list the OTHER fleet boxes as peers (self 10.77.9.61 excluded), \
         fixing the chicken-and-egg dead end (#1066)"
    );
}

/// Provisioning a non-source box lists NDI_PEER first, then the rest, self excluded, deduped.
#[test]
fn ndi_bootstrap_peer_list_orders_peer_first_and_excludes_self_1066() {
    let (code, out, err) = run_ndi(
        &[
            ("NDI_PEER", "10.77.9.61"),
            ("CAMERA_SET", "cam1 cam2 cam3 cam4"),
        ],
        r#"ndi_bootstrap_peer_list 10.77.9.62 | tr '\n' ' '"#,
    );
    assert_eq!(code, 0, "peer-list must succeed; stderr: {err}");
    assert_eq!(
        out.trim(),
        "10.77.9.61 10.77.9.63 10.77.9.64",
        "NDI_PEER must lead, self (cam2) excluded, no duplicate of NDI_PEER (#1066)"
    );
}

/// The version guard accepts the 4-part SDK string against the 3-part pin, and rejects a decimal
/// look-alike or a different minor.
#[test]
fn ndi_runtime_version_matches_pin_is_a_dotted_prefix_1066() {
    let (code, out, err) = run_ndi(
        &[],
        r#"for f in libndi.so.6.3.2.0 libndi.so.6.3.2 libndi.so.6.3.20 libndi.so.6.2.1.0; do
             if ndi_runtime_version_matches_pin "$f" 6.3.2; then echo "$f=Y"; else echo "$f=N"; fi
           done"#,
    );
    assert_eq!(code, 0, "version guard must run; stderr: {err}");
    let got: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(
        got,
        vec![
            "libndi.so.6.3.2.0=Y",
            "libndi.so.6.3.2=Y",
            "libndi.so.6.3.20=N",
            "libndi.so.6.2.1.0=N"
        ],
        "the pinned-download guard must dotted-prefix-match 6.3.2 (never 6.3.20 / 6.2.1) (#1066)"
    );
}

/// The pin constant lives in the shared lib, so setup-device.sh and verify-device.sh cannot
/// disagree on it.
#[test]
fn ndi_provision_lib_single_sources_the_pin_1066() {
    let (code, out, err) = run_ndi(&[], r#"echo "PIN=$NDI_VERSION_PIN""#);
    assert_eq!(code, 0, "lib must define NDI_VERSION_PIN; stderr: {err}");
    assert_eq!(
        out.trim(),
        "PIN=6.3.2",
        "the lib must default NDI_VERSION_PIN to the fleet pin (#1066)"
    );
    let v = verify();
    assert!(
        on_noncomment_line(&v, ". \"$HERE/lib/ndi-provision.sh\""),
        "verify-device.sh must SOURCE scripts/lib/ndi-provision.sh for the pin (#1066)"
    );
    assert!(
        !v.contains(r#"NDI_VERSION_PIN="${NDI_VERSION_PIN:-6.3.2}""#),
        "verify-device.sh must NOT keep its own inline NDI_VERSION_PIN literal -- it is now \
         single-sourced from the shared lib (#1066)"
    );
}

// ============================================================================
// D1 -- STEP 17b noble pip-vs-debian conflict
// ============================================================================

/// The remoteos-mcp installer must run with the pip env vars that make it install its deps freshly
/// under /usr/local (shadowing the RECORD-less debian copies) -- and they must attach to the
/// installer invocation.
#[test]
fn setup_device_remoteos_install_uses_break_system_and_ignore_installed_1066() {
    let body = setup();
    for needle in ["PIP_BREAK_SYSTEM_PACKAGES=1", "PIP_IGNORE_INSTALLED=1"] {
        assert!(
            on_noncomment_line(&body, needle),
            "STEP 17b must export `{needle}` for the remoteos-mcp installer so its pip installs \
             deps freshly instead of failing to uninstall debian's RECORD-less packages (#1066)"
        );
    }
    let env = body
        .find("PIP_BREAK_SYSTEM_PACKAGES=1 PIP_IGNORE_INSTALLED=1")
        .expect("the pip env prefix must exist");
    let run = body
        .find("bash \"$REMOTEOS_MCP_INSTALLER_TMP\"")
        .expect("the installer invocation must exist");
    assert!(
        env < run && run - env < 200,
        "the pip env prefix (idx {env}) must attach to the installer invocation (idx {run}) (#1066)"
    );
    // The #555 no-inline-pip guard must stay honoured -- the fix must NOT add a bare git pip line.
    assert!(
        !body.contains("remoteos-mcp.git"),
        "the pip fix must NOT inline a bare `pip install ...remoteos-mcp.git` (#555 / #1066)"
    );
}

// ============================================================================
// D2 -- STEP 4 NDI peer bootstrap wiring
// ============================================================================

#[test]
fn setup_device_step4_uses_the_peer_bootstrap_list_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/ndi-provision.sh\""),
        "setup-device.sh must source scripts/lib/ndi-provision.sh (#1066)"
    );
    assert!(
        on_noncomment_line(&body, "ndi_bootstrap_peer_list \"$DEVICE_IP\""),
        "STEP 4 must iterate ndi_bootstrap_peer_list \"$DEVICE_IP\" (the fleet peers minus this \
         box), not a single hard-coded peer (#1066)"
    );
    assert!(
        !body.contains("This box IS the fleet NDI source"),
        "STEP 4 must NOT keep the old self-peer dead end (\"This box IS the fleet NDI source -- \
         nothing to fetch\") -- re-provisioning cam1 must have a way forward (#1066)"
    );
    assert!(
        on_noncomment_line(&body, "cleanup_bak_cruft /usr/lib/ndi"),
        "STEP 4 must still clean up stale .bak cruft (#453 -- unchanged by #1066)"
    );
    // Fail-loud must name every peer tried (D2 requirement) + the pinned download.
    assert!(
        body.contains("tried fleet peers"),
        "STEP 4's terminal failure must name every peer tried (#1066)"
    );
}

#[test]
fn setup_device_step4_pinned_download_fallback_is_version_guarded_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, "ndi_runtime_version_matches_pin"),
        "the STEP 4 download fallback must version-guard the extracted .so against NDI_VERSION_PIN \
         so it can never silently drift the fleet off the pin (#1066)"
    );
    assert!(
        body.contains("libndi-get.sh"),
        "the fallback must reuse the ONE canonical NDI download (vendor/distroav/CI/libndi-get.sh), \
         never a second curl (#1066)"
    );
}

// ============================================================================
// D3 -- binary source defaults to main, with a --run override
// ============================================================================

#[test]
fn setup_device_ci_branch_defaults_to_main_1066() {
    let body = setup();
    assert!(
        body.contains(r#"CI_BRANCH="${CAMERA_BOX_CI_BRANCH:-main}"#),
        "CI_BRANCH must default to `main` (the fleet's production pin, matching deploy-fleet.sh), \
         NOT the dev tip -- a dev-tip provision trips the [0/8] PIN-DRIFT gate (#1066/#1136)"
    );
    assert!(
        !body.contains(r#"CI_BRANCH="${CAMERA_BOX_CI_BRANCH:-dev}"#),
        "the old dev default must be gone (#1066)"
    );
}

#[test]
fn setup_device_supports_explicit_run_id_override_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, "--run)"),
        "the arg parser must accept `--run <ci.yml run id>` as an explicit-artifact override \
         (mirrors deploy-fleet.sh's --run) (#1066)"
    );
    assert!(
        on_noncomment_line(&body, "CI_RUN_ID_ARG"),
        "the --run value must feed the CI artifact lookup via CI_RUN_ID_ARG (#1066)"
    );
    // The default gh-run-download path must still be present (the override does not remove it).
    for needle in [
        "gh run list",
        "gh run download",
        r#"--branch "$CI_BRANCH""#,
        "--status success",
    ] {
        assert!(
            on_noncomment_line(&body, needle),
            "the default CI-artifact lookup must keep `{needle}` (#457, preserved by #1066)"
        );
    }
}

// ============================================================================
// D4 -- dantesync gm_allowlist hostname + pinned release
// ============================================================================

#[test]
fn setup_device_dantesync_installs_the_pinned_release_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, ". \"$HERE/dantesync-version-gate.sh\""),
        "setup-device.sh must source dantesync-version-gate.sh for DANTESYNC_VERSION_PIN (the same \
         single source dantesync-fleet-upgrade.sh uses) (#1066)"
    );
    assert!(
        body.contains("releases/tags/v${DANTESYNC_VERSION_PIN}"),
        "STEP 17 must install the PINNED dantesync release (releases/tags/v$DANTESYNC_VERSION_PIN), \
         not a moving latest (#1066)"
    );
    assert!(
        !body.contains("${DANTESYNC_REPO}/releases/latest"),
        "STEP 17 must NOT install dantesync from releases/latest any more (#1066)"
    );
}

#[test]
fn setup_device_dantesync_gm_allowlist_is_the_hostname_1066() {
    let body = setup();
    assert!(
        on_noncomment_line(&body, r#"_RG_GM_HOST="$(rig_grandmaster_host)""#),
        "STEP 17 must resolve the grandmaster HOSTNAME (rig_grandmaster_host = video-clock.lan) \
         for gm_allowlist (dantesync#113 accepts a hostname since 1.8.54) (#1066)"
    );
    // The gm_allowlist array must carry the hostname var, not the resolved IPv4 var.
    let allow = body
        .find("\"gm_allowlist\"")
        .expect("STEP 17 must write a gm_allowlist");
    let tail = &body[allow..allow + 120.min(body.len() - allow)];
    assert!(
        tail.contains("${_RG_GM_HOST}"),
        "gm_allowlist must carry the DNS hostname (${{_RG_GM_HOST}}), not the resolved IPv4 (#1066)"
    );
    assert!(
        !tail.contains("${_RG_GM_IP}"),
        "gm_allowlist must NOT be the resolved IPv4 any more -- that drifts when the DHCP lease \
         moves (#1066)"
    );
    // The resolve is kept only as a loud precondition (fail closed) -- still sources the resolver.
    assert!(
        on_noncomment_line(&body, r#"_RG_GM_IP="$(rig_grandmaster_ip)""#),
        "rig_grandmaster_ip must remain as the loud resolve precondition (#1066)"
    );
    assert!(
        on_noncomment_line(&body, ". \"$HERE/lib/rig-grandmaster.sh\""),
        "setup-device.sh must still source the shared grandmaster resolver (#1307/#1066)"
    );
}
