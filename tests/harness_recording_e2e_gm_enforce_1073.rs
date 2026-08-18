//! #1073 — flip `DANTESYNC_GATE_GM_ENFORCE=1` on the recording-e2e.sh DanteSync gate invocations.
//!
//! Follow-up to issue 834. The grandmaster-IDENTITY check (`gm_check`, clock-offset-guard.sh) was
//! shipped report-first in `dantesync-gate.sh` (default `DANTESYNC_GATE_GM_ENFORCE=0`): every
//! graded node's `gm_source_ip` is printed as `GM OK/FOREIGN/UNKNOWN`, but its rc only feeds the
//! node verdict when `DANTESYNC_GATE_GM_ENFORCE=1`. It shipped report-only because the STREAM box
//! was PTP-locked to a FOREIGN grandmaster (`10.77.7.109`) while reporting `is_locked=true`, so
//! enforcing then would have failed every E2E run. Once the dantesync-side election + PTP-interface
//! fix (v1.8.42–1.8.46) put every fleet node back on the rig grandmaster `10.77.9.184`, the enforce
//! flip is safe: a foreign or unreadable grandmaster becomes a hard gate failure (FOREIGN → 20,
//! UNKNOWN → 11) instead of a silent report-only line, which is the end-state issue 834's item 3
//! asked for.
//!
//! These tests RUN the two real `dantesync-gate.sh` invocation regions of
//! `scripts/recording-e2e.sh` (a byte-slice of the production script, never a hand-typed mirror)
//! against a fake `dantesync-gate.sh` that records the `DANTESYNC_GATE_GM_ENFORCE` value it was
//! invoked with. This proves the env var actually REACHES the subprocess at runtime — a static
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

/// A fake `dantesync-gate.sh` that logs the `DANTESYNC_GATE_GM_ENFORCE` env value (and its argv)
/// to `$CALL_LOG`, then exits 0 — so the script's own `"$HERE/dantesync-gate.sh"` invocation
/// resolves to it. The env value (not argv) is what proves the flip reached the subprocess.
fn write_fake_gate(dir: &Path) {
    let fake = dir.join("dantesync-gate.sh");
    fs::write(
        &fake,
        "#!/usr/bin/env bash\n\
         echo \"ENFORCE=${DANTESYNC_GATE_GM_ENFORCE:-UNSET} ARGV=$*\" >> \"$CALL_LOG\"\n\
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
fn main_gate_region(s: &str) -> &str {
    let start = s
        .find("echo \"[0/8] DanteSync NTP+PTP gate")
        .expect("#1073: recording-e2e.sh must have the main [0/8] DanteSync gate echo banner");
    let end = s[start..]
        .find("# Version-integrity precondition gate")
        .map(|i| start + i)
        .expect("#1073: expected the #123 version-integrity banner to follow the main gate");
    &s[start..end]
}

/// The SECONDARY (#947) dantesync freshest-offset sanity region (cam1/cam2 already covered by the
/// main gate): from its own comment banner through the `fi`, ending at the next step's banner.
fn secondary_gate_region(s: &str) -> &str {
    let start = s
        .find("# #947: dantesync freshest-offset sanity")
        .expect("#1073: recording-e2e.sh must have the #947 secondary dantesync-gate region");
    let end = s[start..]
        .find("# #924 (user directive")
        .map(|i| start + i)
        .expect("#1073: expected the #924 OBS pre-run-state banner to follow the secondary region");
    &s[start..end]
}

/// The MAIN gate call grades cam1, cam2, strih AND stream — the exact nodes issue 1073 needs the
/// grandmaster-identity check enforced on. The flip must reach THIS invocation.
#[test]
fn main_dantesync_gate_is_invoked_with_gm_enforce_on_1073() {
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
        .output()
        .expect("run #1073 main-gate region");
    assert!(
        out.status.success(),
        "#1073: main-gate region exited non-zero.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(&call_log).unwrap_or_default();
    assert!(
        calls.contains("cam2=10.77.9.62")
            && calls.contains("--win-http")
            && calls.contains("stream=10.77.9.204"),
        "#1073 sanity: the main DanteSync gate must be invoked grading cam1/cam2 + strih/stream. Got calls={calls:?}"
    );
    assert!(
        calls.contains("ENFORCE=1"),
        "#1073: the MAIN dantesync-gate.sh invocation must carry DANTESYNC_GATE_GM_ENFORCE=1 so a \
         node PTP-locked to a foreign/unreadable grandmaster hard-fails the gate (FOREIGN->20, \
         UNKNOWN->11) instead of only being reported (the stream-on-10.77.7.109 false-green issue \
         834 is about). Got calls={calls:?}"
    );
}

/// The SECONDARY (#947) sanity gate also grades strih (the NTP master), a node whose grandmaster
/// identity must be enforced too — so the flip must reach this invocation as well.
#[test]
fn secondary_dantesync_gate_is_invoked_with_gm_enforce_on_1073() {
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
        .output()
        .expect("run #1073 secondary-gate region");
    assert!(
        out.status.success(),
        "#1073: secondary-gate region exited non-zero.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(&call_log).unwrap_or_default();
    assert!(
        calls.contains("cam5=10.77.9.65"),
        "#1073 sanity: the secondary gate must be invoked with the secondary camera. Got calls={calls:?}"
    );
    assert!(
        calls.contains("ENFORCE=1"),
        "#1073: the SECONDARY dantesync-gate.sh invocation must ALSO carry \
         DANTESYNC_GATE_GM_ENFORCE=1 (it grades strih, whose grandmaster identity must be enforced \
         too). Got calls={calls:?}"
    );
}
