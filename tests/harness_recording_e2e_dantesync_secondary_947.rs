//! #947 — the `[0/8]` preflight's dantesync freshest-offset sanity check for the ACTIVE
//! SECONDARY cameras (cam1/cam2 are already covered by the main DanteSync gate earlier in
//! preflight) used to filter its candidate list with a literal `grep -oE 'cam[3-7]=[^ ]*'`
//! against `PREFLIGHT_DANTESYNC_LINUX` -- which ALWAYS holds cam1=/cam2= (they're gated first,
//! unconditionally). That tested the WRONG thing: whether cam1/cam2 happened to be present
//! (always true), not whether any SECONDARY camera exists to gate. With
//! `CAMERA_ACTIVE_SET="cam1 cam2"` (cam4 retired, #947) the secondary set is EMPTY, so the old
//! `if [ -n "$PREFLIGHT_DANTESYNC_LINUX" ]` guard passed while the grep filter yielded nothing --
//! `dantesync-gate.sh` was invoked with a zero-node `--linux` and correctly refused ("no nodes to
//! gate"), failing the WHOLE preflight even though cam1+cam2 were already gated clean above (live
//! hardware run 30761247629).
//!
//! The fix derives the secondary candidate list from `camera_active_secondary_set()`
//! (scripts/camera-set.sh, #827) intersected with whatever actually came back healthy in
//! `PREFLIGHT_DANTESYNC_LINUX`, and skips the gate call entirely (with an explicit, honest skip
//! line) when that intersection is empty -- never weakening `dantesync-gate.sh`'s own zero-node
//! refusal, and never silently dropping cam1/cam2 coverage (they're still gated by the main
//! DanteSync gate, unchanged).
//!
//! These tests run the REAL production code (a byte-slice of `scripts/recording-e2e.sh` itself,
//! never a hand-typed mirror) against a fake `dantesync-gate.sh` stand-in that logs its own argv
//! -- proving RUNTIME behavior (was the gate invoked, and with which nodes), not just that some
//! text string is present in the file.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Extract the `#947` dantesync-secondary preflight block: from its own leading comment through
/// the `fi` closing it, ending right before the NEXT preflight step's own comment banner
/// (`#924`'s OBS pre-run-state normalize step). This slices the REAL script text, never a
/// hand-copied mirror.
fn secondary_gate_region(s: &str) -> &str {
    let start = s.find("# #947: dantesync freshest-offset sanity").expect(
        "#947: recording-e2e.sh must have the dantesync-secondary preflight comment banner \
             (the SECONDARY-camera-only gate, distinct from the main DanteSync gate earlier)",
    );
    let end = s[start..]
        .find("# #924 (user directive")
        .map(|i| start + i)
        .expect("#947: expected the #924 OBS pre-run-state banner to immediately follow the block");
    &s[start..end]
}

/// Run the extracted `#947` region for real: source the REAL `scripts/camera-set.sh` (so
/// `camera_active_secondary_set()` is the genuine function, never re-implemented), set
/// `CAMERA_ACTIVE_SET` + `PREFLIGHT_DANTESYNC_LINUX`, and point `$HERE` at a tempdir holding a
/// fake `dantesync-gate.sh` that logs its own argv to `$CALL_LOG` and exits 0. Returns
/// (captured stdout, contents of the call log -- empty string if never invoked).
fn run_secondary_gate(
    camera_active_set: &str,
    preflight_dantesync_linux: &str,
) -> (String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let here = tmp.path();
    let call_log = here.join("dantesync-gate-calls.log");
    let fake = here.join("dantesync-gate.sh");
    fs::write(
        &fake,
        "#!/usr/bin/env bash\necho \"$@\" >> \"$CALL_LOG\"\nexit 0\n",
    )
    .expect("write fake dantesync-gate.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake, perms).unwrap();
    }

    let script_text = read("scripts/recording-e2e.sh");
    let region = secondary_gate_region(&script_text);
    let camera_set_sh = manifest_dir().join("scripts/camera-set.sh");
    let script = format!(
        "set -euo pipefail\nsource \"$CAMERA_SET_SH\"\nPREFLIGHT_DANTESYNC_LINUX=\"$PF_LINUX\"\n{region}"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HERE", here)
        .env("CALL_LOG", &call_log)
        .env("CAMERA_ACTIVE_SET", camera_active_set)
        .env("CAMERA_SET_SH", &camera_set_sh)
        .env("PF_LINUX", preflight_dantesync_linux)
        .env("STRIH", "10.9.9.202")
        .output()
        .expect("run #947 secondary-gate region");
    assert!(
        out.status.success(),
        "#947: secondary-gate region exited non-zero.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let calls = fs::read_to_string(&call_log).unwrap_or_default();
    (stdout, calls)
}

/// Today's real default (#947): CAMERA_ACTIVE_SET="cam1 cam2" -- cam4 retired, no secondary
/// camera left at all. The gate must be SKIPPED, never invoked with a zero-node --linux, and the
/// harness must NOT abort (this is the exact failure from live run 30761247629).
#[test]
fn skips_the_gate_when_camera_active_set_has_no_secondary_cameras_947() {
    let (stdout, calls) = run_secondary_gate("cam1 cam2", "cam1=10.77.9.61 cam2=10.77.9.62");
    assert!(
        calls.is_empty(),
        "#947: with CAMERA_ACTIVE_SET=\"cam1 cam2\" (no secondary cameras) dantesync-gate.sh must \
         NOT be invoked at all -- cam1+cam2 are already covered by the main DanteSync gate above. \
         Got calls={calls:?}"
    );
    assert!(
        stdout.contains("skipped") && stdout.contains("no secondary cameras"),
        "#947: must print an explicit, honest skip line explaining cam1+cam2 are already covered \
         by the main DanteSync gate -- never a silent no-op. Got stdout={stdout:?}"
    );
}

/// The reversibility direction every #827-class fix in this repo is tested for: re-adding a
/// secondary camera to CAMERA_ACTIVE_SET must flow straight through to the gate call, carrying
/// ONLY that secondary camera (never cam1/cam2, which are already gated separately).
#[test]
fn gates_the_active_secondary_cameras_when_present_947() {
    let (stdout, calls) = run_secondary_gate(
        "cam1 cam2 cam5",
        "cam1=10.77.9.61 cam2=10.77.9.62 cam5=10.77.9.65",
    );
    assert!(
        !calls.is_empty(),
        "#947: with a secondary camera in CAMERA_ACTIVE_SET, dantesync-gate.sh MUST be invoked. \
         Got stdout={stdout:?}"
    );
    assert!(
        calls.contains("--linux") && calls.contains("cam5=10.77.9.65"),
        "#947: dantesync-gate.sh must be invoked with --linux carrying the secondary camera \
         (cam5). Got calls={calls:?}"
    );
    assert!(
        !calls.contains("cam1=") && !calls.contains("cam2="),
        "#947: cam1/cam2 must NEVER be passed to this secondary-only gate -- they're already \
         gated by the main DanteSync gate above. Got calls={calls:?}"
    );
    assert!(
        stdout.contains("cam5"),
        "#947: the banner must name the actual secondary camera being gated, not cam1/cam2. Got \
         stdout={stdout:?}"
    );
}

/// A secondary camera that came back EXCLUDED (acked-offline) above must never appear in
/// PREFLIGHT_DANTESYNC_LINUX in the first place (that's the existing exclusion mechanism, tested
/// elsewhere) -- this test just confirms the #947 filter still correctly finds NOTHING to gate
/// when the only entries present are cam1/cam2 (mirrors an acked-offline cam5: it simply never
/// reaches PREFLIGHT_DANTESYNC_LINUX), rather than mis-reading cam1/cam2 as secondary cameras.
#[test]
fn never_mistakes_cam1_cam2_for_secondary_cameras_even_with_a_wider_active_set_947() {
    let (_, calls) = run_secondary_gate("cam1 cam2 cam4", "cam1=10.77.9.61 cam2=10.77.9.62");
    assert!(
        calls.is_empty(),
        "#947: cam4 is in CAMERA_ACTIVE_SET but did NOT come back healthy (absent from \
         PREFLIGHT_DANTESYNC_LINUX, e.g. acked-offline) -- the gate must not be invoked on cam1/ \
         cam2 alone. Got calls={calls:?}"
    );
}

/// issue 1022 follow-up (live run 31669664399, cam3's first day back): dantesync-gate.sh's
/// step-chase machinery (the client bound widened by the master's own ntp_deadband_us envelope +
/// the bimodal chase-signature exclusion) only engages when the NTP MASTER (strih) is among the
/// call's configured nodes -- the secondary-camera invocation passed ONLY `--linux camN=...`, so
/// cam3 was graded against the BARE 2000us bound with no chase handling at all and false-failed
/// on the master's routine ~2.5ms step propagation (median -424us in bound, spread 2483us).
/// The secondary invocation must carry the SAME master reference the main gate call does.
#[test]
fn secondary_gate_carries_the_ntp_master_reference_for_chase_grading_1022() {
    let (_, calls) = run_secondary_gate(
        "cam1 cam2 cam3",
        "cam1=10.77.9.61 cam2=10.77.9.62 cam3=10.77.9.63",
    );
    assert!(
        calls.contains("--win-http") && calls.contains("strih=10.9.9.202"),
        "issue 1022: the secondary dantesync-gate invocation must pass the NTP master's \
         --win-http reference (strih) so the step-chase envelope + bimodal exclusion engage for \
         secondary client rows exactly as they do in the main gate call. Got calls={calls:?}"
    );
}
