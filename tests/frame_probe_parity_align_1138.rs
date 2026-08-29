//! issue 1138 (mechanism half) — the cam2 frame-probe (steady-state painter) must never silently
//! LAG the current build: a pre-gate AUTO-ALIGN (the frame-probe sibling of the camera-box
//! `cambox_parity_align_before_gate`) deploys the candidate painter to cam2 every E2E run, so
//! pin+deploy advance together (orphan-PROOF), and the report-only [1/8] pin CONFIRMS it.
//!
//! The prior DETECTION + deploy-lib work merged report-only + DORMANT (no align, deployed only at
//! dev->main merge → the deployed painter drifted between merges, needing a MANUAL redeploy — the
//! live 2026-08-29 incident). These guards pin the mechanism so a refactor cannot silently un-wire
//! it again:
//!   (1) the pure align decision (`frame_probe_align_action`) grades cam2's deployed sha vs the
//!       candidate CI-artifact sha (NOCANDIDATE/UNKNOWN/NOACTIVE/OK/ALIGN, honours CAMBOX_OFFLINE_ACK);
//!   (2) the orchestrator (`frame_probe_parity_align_before_gate`) DEPLOYS on ALIGN, EXPORTS
//!       FRAME_PROBE_ALIGN_CI_BIN for the [1/8] pin, and SKIPS under the --no-main-pin soak;
//!   (3) `deploy-fleet.sh --frame-probe` WITHOUT --binary is a frame-probe-ONLY deploy (never a
//!       camera-box fleet deploy — the align must swap ONLY cam2's painter);
//!   (4) recording-e2e wires the align at [0/8] (before the [1/8] pin) and the [1/8] pin pins
//!       against FRAME_PROBE_ALIGN_CI_BIN.
//! Static-anchor + pure-lib + real-script-under-stubs guards only (Tier-0 #557: no cargo compile of
//! the shell under test).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source scripts/lib/frame-probe-parity-align.sh under the caller's real `set -euo pipefail`
/// (recording-e2e.sh sources it that way — a report-only helper called as a bare statement MUST
/// return 0 on every input, see .claude/rules/ci-testing-gotchas.md #1133) and run `body`;
/// return stdout (asserts exit 0 — a non-zero here would mean the orchestrator can abort the run).
fn run_lib(body: &str) -> String {
    let lib = manifest_dir().join("scripts/lib/frame-probe-parity-align.sh");
    assert!(lib.exists(), "{} not found", lib.display());
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced lib harness exited non-zero (a bare-statement report-only helper must never abort \
         the caller under set -e).\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// (1) pure decision — frame_probe_align_action
// ---------------------------------------------------------------------------

#[test]
fn frame_probe_align_action_grades_deployed_vs_candidate_sha() {
    // empty candidate sha -> NOCANDIDATE (the report-only pin decides; never deploy blindly).
    assert!(
        run_lib(r#"frame_probe_align_action "" "cam2=abc"; echo"#).contains("NOCANDIDATE"),
        "empty candidate must be NOCANDIDATE"
    );
    // cam2 already on the candidate -> OK (no deploy).
    assert!(
        run_lib(r#"frame_probe_align_action "abc123" "cam2=abc123"; echo"#).contains("OK"),
        "match must be OK"
    );
    // cam2 on a different sha -> ALIGN (deploy the candidate).
    assert!(
        run_lib(r#"frame_probe_align_action "abc123" "cam2=deadbeef"; echo"#).contains("ALIGN"),
        "mismatch must be ALIGN"
    );
    // cam2 sha unread (empty) -> UNKNOWN, fail-closed (never align an unread box).
    assert!(
        run_lib(r#"frame_probe_align_action "abc123" "cam2="; echo"#).contains("UNKNOWN"),
        "unread deployed sha must be UNKNOWN"
    );
    // cam2 acked-offline -> NOACTIVE (nothing to align; never deploy to an acked box).
    let acked = run_lib(
        r#"CAMBOX_OFFLINE_ACK="cam2:x" frame_probe_align_action "abc123" "cam2=deadbeef"; echo"#,
    );
    assert!(
        acked.contains("NOACTIVE"),
        "acked-offline cam2 must be NOACTIVE (excluded), got: {acked:?}"
    );
}

// ---------------------------------------------------------------------------
// (2) orchestrator — frame_probe_parity_align_before_gate
// ---------------------------------------------------------------------------

/// Build a fake pre-fetched probe-tools artifact dir holding a `frame-probe` file (any bytes) so
/// resolve_ci_bin returns it under FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD=1 (no gh, no --version).
fn fake_artifact_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    fs::write(
        d.path().join("frame-probe"),
        b"FAKE-FRAME-PROBE-BYTES-1138\n",
    )
    .unwrap();
    d
}

#[test]
fn orchestrator_deploys_on_align_and_exports_ci_bin() {
    let art = fake_artifact_dir();
    let marker = art.path().join("deploy-marker");
    // Compute the candidate sha the orchestrator will see (sha256 of the fake frame-probe), then
    // make cam2's deployed sha DIFFERENT so the decision is ALIGN.
    let body = format!(
        r#"export FRAME_PROBE_ALIGN_ARTIFACT_DIR="{dir}"
export FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD=1
export FRAME_PROBE_GATE_SHA_CAM2="0000000000000000000000000000000000000000000000000000000000000000"
export FRAME_PROBE_ALIGN_DEPLOY_CMD='echo "DEPLOYED set=$CAMERA_SET bin=$FRAME_PROBE_ALIGN_CI_BIN" > "{marker}"'
frame_probe_parity_align_before_gate "cam2=root@10.0.0.2"
echo "EXPORTED=${{FRAME_PROBE_ALIGN_CI_BIN:-<unset>}}"
"#,
        dir = art.path().display(),
        marker = marker.display()
    );
    let out = run_lib(&body);
    assert!(
        out.contains("deploying it to /usr/local/bin/frame-probe"),
        "ALIGN must announce the deploy; out={out:?}"
    );
    assert!(
        marker.exists(),
        "the deploy command must have run on ALIGN; out={out:?}"
    );
    let m = fs::read_to_string(&marker).unwrap();
    assert!(
        m.contains("set=cam2"),
        "deploy must be cam2-scoped, got: {m:?}"
    );
    assert!(
        m.contains("bin=") && m.contains("frame-probe"),
        "deploy must receive the CI frame-probe bin, got: {m:?}"
    );
    // The [1/8] pin reads FRAME_PROBE_ALIGN_CI_BIN — the align must EXPORT it.
    assert!(
        out.contains("EXPORTED=")
            && out.contains("frame-probe")
            && !out.contains("EXPORTED=<unset>"),
        "align must export FRAME_PROBE_ALIGN_CI_BIN for the [1/8] pin; out={out:?}"
    );
}

#[test]
fn orchestrator_no_deploy_when_already_on_candidate() {
    let art = fake_artifact_dir();
    let marker = art.path().join("deploy-marker");
    // cam2's deployed sha == the candidate sha => OK => NO deploy.
    let body = format!(
        r#"export FRAME_PROBE_ALIGN_ARTIFACT_DIR="{dir}"
export FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD=1
cand="$(sha256sum "{dir}/frame-probe" | awk '{{print $1}}')"
export FRAME_PROBE_GATE_SHA_CAM2="$cand"
export FRAME_PROBE_ALIGN_DEPLOY_CMD='echo DEPLOYED > "{marker}"'
frame_probe_parity_align_before_gate "cam2=root@10.0.0.2"
"#,
        dir = art.path().display(),
        marker = marker.display()
    );
    let out = run_lib(&body);
    assert!(
        out.contains("already on the candidate"),
        "OK path must log no-deploy; out={out:?}"
    );
    assert!(
        !marker.exists(),
        "must NOT deploy when cam2 already matches; out={out:?}"
    );
}

#[test]
fn orchestrator_skips_under_no_main_pin_soak() {
    let art = fake_artifact_dir();
    let marker = art.path().join("deploy-marker");
    let body = format!(
        r#"export CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN=1
export FRAME_PROBE_ALIGN_ARTIFACT_DIR="{dir}"
export FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD=1
export FRAME_PROBE_GATE_SHA_CAM2="deadbeef"
export FRAME_PROBE_ALIGN_DEPLOY_CMD='echo DEPLOYED > "{marker}"'
frame_probe_parity_align_before_gate "cam2=root@10.0.0.2"
"#,
        dir = art.path().display(),
        marker = marker.display()
    );
    let out = run_lib(&body);
    assert!(
        out.contains("SKIPPED"),
        "--no-main-pin must SKIP the align; out={out:?}"
    );
    assert!(
        !marker.exists(),
        "must NOT deploy under --no-main-pin soak; out={out:?}"
    );
}

#[test]
fn orchestrator_version_guard_refuses_a_non_candidate_artifact() {
    // A probe-tools artifact whose co-located camera-box-probe reports a DIFFERENT version than the
    // candidate (the candidate's own ci.yml is not published yet) must NOT deploy a stale painter.
    let art = tempfile::tempdir().expect("tempdir");
    fs::write(art.path().join("frame-probe"), b"STALE-FRAME-PROBE\n").unwrap();
    let cbp = art.path().join("camera-box-probe");
    fs::write(
        &cbp,
        "#!/usr/bin/env bash\necho 'camera-box 1.7.0-dev.999'\n",
    )
    .unwrap();
    let mut perm = fs::metadata(&cbp).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    fs::set_permissions(&cbp, perm).unwrap();
    let marker = art.path().join("deploy-marker");
    let body = format!(
        r#"export FRAME_PROBE_ALIGN_ARTIFACT_DIR="{dir}"
export FRAME_PROBE_ALIGN_CANDIDATE="1.7.0-dev.574"
export FRAME_PROBE_GATE_SHA_CAM2="deadbeef"
export FRAME_PROBE_ALIGN_DEPLOY_CMD='echo DEPLOYED > "{marker}"'
frame_probe_parity_align_before_gate "cam2=root@10.0.0.2"
"#,
        dir = art.path().display(),
        marker = marker.display()
    );
    let out = run_lib(&body);
    assert!(
        !marker.exists(),
        "version-guard: a non-candidate artifact must NEVER be deployed (stale-painter protection); out={out:?}"
    );
    assert!(
        out.contains("not the candidate") || out.contains("no candidate frame-probe resolved"),
        "version-guard must log the refusal; out={out:?}"
    );
}

// ---------------------------------------------------------------------------
// (3) deploy-fleet.sh frame-probe-ONLY mode (--frame-probe WITHOUT --binary)
// ---------------------------------------------------------------------------

/// Run the REAL deploy-fleet.sh with `--frame-probe <fixture>` and NO --binary, under stubs.
/// `gh` FAILS loudly if invoked (a frame-probe-only deploy must NOT download/deploy camera-box).
fn run_frame_probe_only() -> (bool, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fp = tmp.path().join("frame-probe-artifact");
    fs::write(&fp, b"FRAME-PROBE-ARTIFACT-1138\n").unwrap();

    let stub = |name: &str, body: &str| {
        let p = bin.join(name);
        fs::write(&p, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&p, perm).unwrap();
    };
    stub("mount", "#!/usr/bin/env bash\nexit 0\n");
    stub("systemctl", "#!/usr/bin/env bash\nexit 0\n");
    // The remote sha256sum must MATCH the local artifact so byte-verify passes. The local read is
    // on the artifact path; the remote read is on /usr/local/bin/frame-probe — return the same hash.
    stub("sha256sum", "#!/usr/bin/env bash\necho 'aaaa  '\"$1\"\n");
    // sshpass: drop `-p <pass>`, scp -> success, ssh -> execute the remote command through bash so
    // the mount/systemctl/sha256sum stubs run. Rewrite the absolute remote sha256sum path so the
    // PATH stub catches it consistently with the local read.
    stub(
        "sshpass",
        r#"#!/usr/bin/env bash
shift 2
mode="$1"; shift
if [ "$mode" = "scp" ]; then exit 0; fi
cmd="${@: -1}"
bash -c "$cmd"
"#,
    );
    // gh MUST NOT be called in frame-probe-only mode (no camera-box artifact download).
    stub(
        "gh",
        "#!/usr/bin/env bash\necho 'GH-CALLED-UNEXPECTEDLY' >&2\nexit 1\n",
    );

    let script = manifest_dir().join("scripts/deploy-fleet.sh");
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(&script)
        .arg("--frame-probe")
        .arg(&fp)
        .env("PATH", &path_env)
        .env("CAMERA_SET", "cam2")
        .env("SSH_PASS", "x")
        .output()
        .expect("run deploy-fleet.sh frame-probe-only");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn deploy_fleet_frame_probe_only_mode_deploys_painter_without_camera_box() {
    let (ok, out) = run_frame_probe_only();
    assert!(
        ok,
        "deploy-fleet.sh --frame-probe (no --binary) must succeed in frame-probe-only mode; out:\n{out}"
    );
    assert!(
        !out.contains("GH-CALLED-UNEXPECTEDLY"),
        "frame-probe-only mode must NOT download/deploy camera-box (gh must not be called); out:\n{out}"
    );
    assert!(
        !out.contains("Deploying camera-box"),
        "frame-probe-only mode must NOT run the camera-box fleet deploy; out:\n{out}"
    );
    assert!(
        out.contains("FRAME-PROBE DEPLOYED") || out.contains("frame-probe byte-verify OK"),
        "frame-probe-only mode must reach the cam2 painter deploy; out:\n{out}"
    );
}

#[test]
fn deploy_fleet_frame_probe_only_uses_the_892_lifecycle() {
    // The frame-probe-only path must go through deploy_frame_probe_to_painter (the #892
    // enable-state-preserving lifecycle), same as the tail-deploy — static guard.
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("deploy_frame_probe_to_painter"),
        "deploy-fleet.sh must call deploy_frame_probe_to_painter"
    );
    // The frame-probe-only branch must gate on --frame-probe set AND --binary/--run unset.
    assert!(
        s.contains("frame-probe-ONLY mode") || s.contains("frame-probe-only"),
        "deploy-fleet.sh must have an explicit frame-probe-only branch"
    );
}

// ---------------------------------------------------------------------------
// (4) recording-e2e wiring — [0/8] align before the [1/8] pin; pin uses FRAME_PROBE_ALIGN_CI_BIN
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_sources_and_calls_the_frame_probe_align_at_step_0() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(r#". "$HERE/lib/frame-probe-parity-align.sh""#),
        "recording-e2e.sh must source the frame-probe parity-align lib"
    );
    let call = s
        .find("frame_probe_parity_align_before_gate \"cam2=root@$PAINTER_IP\"")
        .expect("recording-e2e.sh must call frame_probe_parity_align_before_gate cam2-scoped");
    // The align runs AFTER the camera-box parity align (same [0/8] region, its sibling) ...
    let cambox_align = s
        .find("cambox_parity_align_before_gate")
        .expect("cambox parity align must exist");
    assert!(
        call > cambox_align,
        "frame-probe align must sit in the [0/8] region beside camera-box's"
    );
    // ... and BEFORE the [1/8] pin that confirms it (so the pin verifies the just-deployed painter).
    let pin = s
        .find("--frame-probe-only")
        .expect("the [1/8] pin must exist");
    assert!(
        call < pin,
        "the [0/8] frame-probe align must run BEFORE the [1/8] pin that confirms it"
    );
}

#[test]
fn recording_e2e_pin_verifies_against_the_aligned_ci_artifact() {
    let s = read("scripts/recording-e2e.sh");
    let pin = s
        .find("--frame-probe-only")
        .expect("the [1/8] pin must exist");
    let window = &s[pin..(pin + 300).min(s.len())];
    // The pin must prefer FRAME_PROBE_ALIGN_CI_BIN (the artifact the align fetched + deployed) so
    // the compare is against the SAME bytes cam2 now runs — with the $PROBE_BIN_DIR fallback for
    // the align-skipped (--no-main-pin) / gh-unavailable case.
    assert!(
        window.contains("FRAME_PROBE_ALIGN_CI_BIN"),
        "the [1/8] pin must prefer the aligned CI artifact (FRAME_PROBE_ALIGN_CI_BIN); window={window:?}"
    );
    assert!(
        window.contains("PROBE_BIN_DIR/frame-probe"),
        "the [1/8] pin must keep the $PROBE_BIN_DIR fallback; window={window:?}"
    );
}
