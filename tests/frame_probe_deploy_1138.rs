//! #1138 (residual) — engage the dormant frame-probe (cam2 painter) sha-pin alarm during E2E, and
//! add frame-probe to the fleet auto-deploy path with cam2-painter.service lifecycle discipline.
//!
//! The merged DETECTION half (`f99ba2671`) is DORMANT (no expected sha wired) and frame-probe is not
//! auto-deployed (deploy-fleet.sh + the post-merge ci deploy job ship only camera-box). These guards
//! pin the two remaining lanes so a refactor cannot silently un-wire them again:
//!   (a1) recording-e2e engages the report from [1/8] (where the current-build frame-probe exists),
//!        cam2-scoped (frame-probe lives ONLY on cam2 — setup-device.sh STEP 3b `cam2_is_painter_box`).
//!   (a2) deploy-fleet.sh deploys frame-probe to cam2 with the #892 enable-state-PRESERVING lifecycle
//!        (never blindly `enable --now` — EVENT mode disables the unit so a QR can't return onto air).
//! Static-anchor + pure-lib guards only (Tier-0 #557: no cargo compile of the shell under test).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source scripts/lib/frame-probe-deploy.sh and run `body`; return stdout (asserts exit 0).
fn run_lib(body: &str) -> String {
    let lib = manifest_dir().join("scripts/lib/frame-probe-deploy.sh");
    assert!(lib.exists(), "{} not found", lib.display());
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced lib harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// (a1) recording-e2e engages the frame-probe sha-pin report
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_engages_the_frame_probe_report_after_the_probe_build() {
    let s = read("scripts/recording-e2e.sh");
    // #1235: the DEFAULT (first) engagement is now the HARD gate (--frame-probe-hard), pinning
    // against the aligned CI artifact FRAME_PROBE_ALIGN_CI_BIN (NOT the byte-different $PROBE_BIN_DIR
    // local build — that fallback lives only in the --no-main-pin soak branch, below).
    let engage = s
        .find("--frame-probe-only")
        .expect("#1235: recording-e2e.sh must engage the frame-probe pin via --frame-probe-only");
    let window = &s[engage..(engage + 300).min(s.len())];
    assert!(
        window.contains("--frame-probe-hard")
            && window.contains("--frame-probe-expected-bin")
            && window.contains("FRAME_PROBE_ALIGN_CI_BIN"),
        "#1235: the default engagement must be the HARD gate pinning against the aligned CI artifact. window={window:?}"
    );
    // The engagement must run AFTER $PROBE_BIN_DIR is resolved ([1/8]) — else the bin doesn't exist
    // yet and the report would be silently dormant (the exact trap this ticket exists to kill).
    let probe_dir = s
        .find("PROBE_BIN_DIR=\"${PROBE_BIN_DIR:-target/release}\"")
        .expect("recording-e2e.sh must resolve PROBE_BIN_DIR at [1/8]");
    assert!(
        engage > probe_dir,
        "#1138: the frame-probe engagement must come AFTER PROBE_BIN_DIR is resolved (the current-build \
         frame-probe is built at [1/8], not at [0/8])"
    );
    // And still before StartRecord (a preflight, like every other gate).
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        engage < start_record,
        "#1138: the frame-probe report must run BEFORE StartRecord"
    );
}

#[test]
fn recording_e2e_frame_probe_engagement_is_cam2_scoped_hard_by_default_report_only_under_soak() {
    let s = read("scripts/recording-e2e.sh");
    // #1235: the pin has TWO branches — the DEFAULT hard gate (first --frame-probe-only) and the
    // --no-main-pin operator-soak report-only fallback (second --frame-probe-only).
    let mut occ = s.match_indices("--frame-probe-only");
    let hard = occ
        .next()
        .expect("#1235: recording-e2e.sh must engage the frame-probe pin")
        .0;
    let soak = occ
        .next()
        .expect("#1235: the --no-main-pin soak branch must keep a report-only frame-probe pin")
        .0;
    let hard_win = &s[hard..(hard + 300).min(s.len())];
    let soak_win = &s[soak..(soak + 300).min(s.len())];

    // frame-probe lives ONLY on cam2 (setup-device STEP 3b). Engaging the whole fleet would
    // UNKNOWN-spam every non-painter box — so the pin is cam2/PAINTER_IP scoped, NOT the
    // fleet-wide $CAMBOX_VERSION_LINUX list the [0/8] parity gate uses.
    assert!(
        hard_win.contains("PAINTER_IP") && !hard_win.contains("CAMBOX_VERSION_LINUX"),
        "#1235: the hard-gate pin must be cam2/PAINTER_IP-scoped, not fleet-wide. window={hard_win:?}"
    );
    // The DEFAULT branch is a HARD gate: NO `|| true` (a non-zero exit must abort the E2E).
    assert!(
        !hard_win.contains("|| true"),
        "#1235: the default frame-probe pin is a HARD gate — it must NOT be `|| true`-guarded. window={hard_win:?}"
    );
    // The soak branch is the #1138 report-only fallback: `|| true`-guarded + the local build fallback.
    assert!(
        soak_win.contains("|| true") && soak_win.contains("PROBE_BIN_DIR/frame-probe"),
        "#1235: the --no-main-pin soak branch must stay report-only (`|| true`) against the local build. window={soak_win:?}"
    );
    // The soak branch is guarded by the --no-main-pin env (the same escape the [0/8] align honours).
    assert!(
        s[..soak].contains("CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN"),
        "#1235: the soak report-only branch must sit under a CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN guard"
    );
}

// ---------------------------------------------------------------------------
// (a2) deploy-fleet.sh deploys frame-probe to cam2 with the #892 lifecycle
// ---------------------------------------------------------------------------

#[test]
fn deploy_fleet_supports_frame_probe_deploy_to_the_painter() {
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("--frame-probe"),
        "#1138: deploy-fleet.sh must accept a --frame-probe <path> option"
    );
    assert!(
        s.contains("scripts/lib/frame-probe-deploy.sh") || s.contains("lib/frame-probe-deploy.sh"),
        "#1138: deploy-fleet.sh must source the frame-probe-deploy lib (the #892 enable-state decision)"
    );
    assert!(
        s.contains("cam2-painter.service"),
        "#1138: the frame-probe deploy must target the cam2-painter.service lifecycle"
    );
    assert!(
        s.contains("frame_probe_restore_enable_decision"),
        "#1138: the deploy must use the #892 enable-state-preserving decision"
    );
    assert!(
        s.contains("is-enabled"),
        "#1138: the deploy must READ the unit's prior enabled-state (#892) before touching it"
    );
    assert!(
        s.contains("/usr/local/bin/frame-probe"),
        "#1138: the deploy must swap /usr/local/bin/frame-probe"
    );
}

// ---------------------------------------------------------------------------
// (a2) ci.yml post-merge deploy job ships frame-probe too
// ---------------------------------------------------------------------------

#[test]
fn ci_deploy_job_downloads_probe_tools_and_passes_frame_probe() {
    let ci = read(".github/workflows/ci.yml");
    let job = ci
        .find("deploy-fleet:")
        .expect("ci.yml must have the deploy-fleet job");
    let end = ci[job..]
        .find("notify-on-failure:")
        .map(|i| job + i)
        .unwrap_or(ci.len());
    let block = &ci[job..end];
    assert!(
        block.contains("probe-tools-linux-amd64"),
        "#1138: the post-merge deploy job must download the probe-tools artifact (frame-probe)"
    );
    assert!(
        block.contains("--frame-probe"),
        "#1138: the post-merge deploy job must pass --frame-probe to deploy-fleet.sh"
    );
}

// ---------------------------------------------------------------------------
// pure lib — the #892 enable-state-preserving decision
// ---------------------------------------------------------------------------

#[test]
fn frame_probe_restore_enable_decision_preserves_state() {
    // enabled (devel/test mode) => re-arm the painter with the new binary.
    let en = run_lib(r#"frame_probe_restore_enable_decision "enabled"; echo"#);
    assert!(en.contains("enable-now"), "enabled must re-arm: {en:?}");
    // disabled (EVENT mode — #892: deliberately dark so a QR never returns onto a live broadcast).
    let dis = run_lib(r#"frame_probe_restore_enable_decision "disabled"; echo"#);
    assert!(
        dis.contains("leave") && !dis.contains("enable-now"),
        "disabled must LEAVE dark (#892): {dis:?}"
    );
    // static / masked / unreadable / not-installed => leave untouched, never re-arm.
    for state in ["static", "masked", "", "not-installed", "enabled-runtime"] {
        let d = run_lib(&format!(
            r#"frame_probe_restore_enable_decision "{state}"; echo"#
        ));
        assert!(
            d.contains("leave") && !d.contains("enable-now"),
            "state {state:?} must LEAVE untouched (only a persistently-enabled unit re-arms): {d:?}"
        );
    }
}
