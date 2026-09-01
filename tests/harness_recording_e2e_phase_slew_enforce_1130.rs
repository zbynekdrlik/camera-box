//! #1130 — flip `DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1` on the recording-e2e.sh DanteSync gate
//! invocations.
//!
//! Follow-up to issue 1130's own report-first phase_slew term. `phase_slew_check`/
//! `phase_slew_enabled_from_pipe_json` (clock-offset-guard.sh, #1215) and their wiring into
//! `dantesync-gate.sh`'s `grade_http_node` (#1130) shipped report-first (default
//! `DANTESYNC_GATE_PHASE_SLEW_ENFORCE=0`): every HTTP-graded node's phase_slew state is printed as
//! `PHASE-SLEW ENABLED/DISABLED/UNKNOWN`, but its rc only feeds the node verdict when
//! `DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1`. It shipped report-first because #1130's own live check
//! only confirmed cam1-4+strih+stream serve `phase_slew_enabled` — flipping to enforce is safe only
//! once every graded node is confirmed (mirroring #834's own report-first→#1073-enforce path for
//! `gm_check`). This is the enforce-flip follow-up, byte-for-byte the same shape #1073 used for GM.
//!
//! These tests RUN the two real `dantesync-gate.sh` invocation regions of
//! `scripts/recording-e2e.sh` (a byte-slice of the production script, never a hand-typed mirror)
//! against a fake `dantesync-gate.sh` that records the `DANTESYNC_GATE_PHASE_SLEW_ENFORCE` value it
//! was invoked with. This proves the env var actually REACHES the subprocess at runtime — a static
//! "the text is present" assertion cannot prove the prefix is on the right line or well-formed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &str) -> String {
    let path = manifest_dir().join(p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A fake `dantesync-gate.sh` that logs both enforce env values (and its argv) to `$CALL_LOG`,
/// then exits 0 — so the script's own `"$HERE/dantesync-gate.sh"` invocation resolves to it. The
/// env values (not argv) are what prove the flip reached the subprocess.
fn write_fake_gate(dir: &Path) {
    let fake = dir.join("dantesync-gate.sh");
    fs::write(
        &fake,
        "#!/usr/bin/env bash\n\
         echo \"GM_ENFORCE=${DANTESYNC_GATE_GM_ENFORCE:-UNSET} \
         PS_ENFORCE=${DANTESYNC_GATE_PHASE_SLEW_ENFORCE:-UNSET} ARGV=$*\" >> \"$CALL_LOG\"\n\
         exit 0\n",
    )
    .expect("write fake dantesync-gate.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake, perms).unwrap();
    }
}

/// The MAIN DanteSync gate (#7) region: from its `[0/8]` echo banner through to (but not
/// including) the next preflight step's own comment banner (the #123 version-integrity gate).
/// Same slice `tests/harness_recording_e2e_gm_enforce_1073.rs` already uses for this region.
fn main_gate_region(s: &str) -> &str {
    let start = s
        .find("echo \"[0/8] DanteSync NTP+PTP gate")
        .expect("#1130: recording-e2e.sh must have the main [0/8] DanteSync gate echo banner");
    let end = s[start..]
        .find("# Version-integrity precondition gate")
        .map(|i| start + i)
        .expect("#1130: expected the #123 version-integrity banner to follow the main gate");
    &s[start..end]
}

/// The SECONDARY (#947) dantesync freshest-offset sanity region (cam1/cam2 already covered by the
/// main gate): from its own comment banner through the `fi`, ending at the next step's banner.
/// Same slice `tests/harness_recording_e2e_gm_enforce_1073.rs` already uses for this region.
fn secondary_gate_region(s: &str) -> &str {
    let start = s
        .find("# #947: dantesync freshest-offset sanity")
        .expect("#1130: recording-e2e.sh must have the #947 secondary dantesync-gate region");
    let end = s[start..]
        .find("# #924 (user directive")
        .map(|i| start + i)
        .expect("#1130: expected the #924 OBS pre-run-state banner to follow the secondary region");
    &s[start..end]
}

/// The MAIN gate call grades cam1, cam2, strih AND stream — the exact nodes issue 1130 needs the
/// phase_slew enforce check enforced on. The flip must reach THIS invocation.
#[test]
fn main_dantesync_gate_is_invoked_with_phase_slew_enforce_on_1130() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let here = tmp.path();
    let call_log = here.join("gate-calls.log");
    write_fake_gate(here);

    let script_text = read("scripts/recording-e2e.sh");
    let region = main_gate_region(&script_text);
    let script = format!("set -euo pipefail\n{region}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HERE", here)
        .env("CALL_LOG", &call_log)
        .env("CAMERA_NAME", "cam1")
        .env("CAM1_IP", "10.77.9.61")
        .env("PAINTER_IP", "10.77.9.62")
        .env("STRIH", "10.77.9.202")
        .env("STREAM", "10.77.9.204")
        .env_remove("DANTESYNC_GATE_GM_ENFORCE")
        .env_remove("DANTESYNC_GATE_PHASE_SLEW_ENFORCE")
        .output()
        .expect("run #1130 main-gate region");
    assert!(
        out.status.success(),
        "#1130: main-gate region exited non-zero.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(&call_log).unwrap_or_default();
    assert!(
        calls.contains("cam2=10.77.9.62")
            && calls.contains("--win-http")
            && calls.contains("stream=10.77.9.204"),
        "#1130 sanity: the main DanteSync gate must be invoked grading cam1/cam2 + strih/stream. Got calls={calls:?}"
    );
    // GM enforce must stay on too (#1073) — this flip is additive, never a regression of it.
    assert!(
        calls.contains("GM_ENFORCE=1"),
        "#1130: the MAIN dantesync-gate.sh invocation must still carry DANTESYNC_GATE_GM_ENFORCE=1 \
         (#1073) — the phase_slew flip must be additive. Got calls={calls:?}"
    );
    assert!(
        calls.contains("PS_ENFORCE=1"),
        "#1130: the MAIN dantesync-gate.sh invocation must carry DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1 \
         so a box that silently reverts to phase_slew=off (re-introducing the chronic NTP step \
         storm) hard-fails the gate (DISABLED->20, UNKNOWN->11) instead of only being reported. \
         Got calls={calls:?}"
    );
}

/// The SECONDARY (#947) sanity gate also grades strih (the NTP master) plus the active secondary
/// cameras — nodes whose phase_slew state must be enforced too, so the flip must reach this
/// invocation as well (mirroring why #1073 flipped GM enforce at both call sites).
#[test]
fn secondary_dantesync_gate_is_invoked_with_phase_slew_enforce_on_1130() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let here = tmp.path();
    let call_log = here.join("gate-calls.log");
    write_fake_gate(here);

    let camera_set_sh = manifest_dir().join("scripts/camera-set.sh");
    let script_text = read("scripts/recording-e2e.sh");
    let region = secondary_gate_region(&script_text);
    // A secondary camera present + healthy so the gate actually invokes (not the skip branch).
    let script = format!(
        "set -euo pipefail\nsource \"$CAMERA_SET_SH\"\nPREFLIGHT_DANTESYNC_LINUX=\"$PF_LINUX\"\n{region}"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HERE", here)
        .env("CALL_LOG", &call_log)
        .env("CAMERA_SET_SH", &camera_set_sh)
        .env("CAMERA_ACTIVE_SET", "cam1 cam2 cam5")
        .env(
            "PF_LINUX",
            "cam1=10.77.9.61 cam2=10.77.9.62 cam5=10.77.9.65",
        )
        .env("STRIH", "10.77.9.202")
        .env_remove("DANTESYNC_GATE_GM_ENFORCE")
        .env_remove("DANTESYNC_GATE_PHASE_SLEW_ENFORCE")
        .output()
        .expect("run #1130 secondary-gate region");
    assert!(
        out.status.success(),
        "#1130: secondary-gate region exited non-zero.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(&call_log).unwrap_or_default();
    assert!(
        calls.contains("cam5=10.77.9.65"),
        "#1130 sanity: the secondary gate must be invoked with the secondary camera. Got calls={calls:?}"
    );
    assert!(
        calls.contains("GM_ENFORCE=1"),
        "#1130: the SECONDARY dantesync-gate.sh invocation must still carry \
         DANTESYNC_GATE_GM_ENFORCE=1 (#1073) — the phase_slew flip must be additive. \
         Got calls={calls:?}"
    );
    assert!(
        calls.contains("PS_ENFORCE=1"),
        "#1130: the SECONDARY dantesync-gate.sh invocation must ALSO carry \
         DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1 (it grades strih plus the active secondary cameras, \
         whose phase_slew state must be enforced too). Got calls={calls:?}"
    );
}
