//! #1126 — the E2E cleanup cam2-painter restore verify loses a ~50ms race and reds a GREEN-verdict
//! run. The cam2/painter restore runs a LOT of serial work inside ONE CLEANUP_SSH_TIMEOUT(=30s)-
//! bounded ssh (stop burns + restart camera-box + is-active/painting verify + retry). On a slow
//! restart that combined ssh hits the 30s wall and `timeout` SIGKILLs it a hair (~47ms, live run
//! 1104689227, 2026-08-19) BEFORE cam2-painter.service reports active — so the subshell exits
//! non-zero, cam2/painter lands in CAMBOX_PARALLEL_FAILED_LABELS, and (since the #715 retry NEVER
//! prunes a painter) a false ::error:: reds a run whose verdict was overall_pass=true. The restore
//! genuinely SUCCEEDED; only the verify window lost the race.
//!
//! The fix (scripts/lib/cam2-painter-restore-recheck.sh): ONE final bounded genuine-painting
//! re-check AFTER cambox_parallel_wait_and_report — a SEPARATE short ssh (never extends the tight
//! parallel-restore budget, so cancellation grace is preserved) that re-checks the SAME presenter-
//! aware painting signal cam2_painter_restore_verify_cmds uses (NOT bare is-active, so it can never
//! mask a BLACK monitor, #863/#860). Only if the painter is genuinely painting NOW does it PRUNE
//! cam2/painter (+ its lockstep FAILED_IPS entry) from the failed ledger, so
//! cambox_parallel_surface_painter_failure no longer fires a false ::error::.
//!
//! Driven with a fake `sshpass` on PATH (no rig, no OBS) exactly like
//! harness_optical_chain_cleanup_surface_860.rs drives its lib.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn lib_path() -> String {
    format!(
        "{}/scripts/lib/cam2-painter-restore-recheck.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Run a bash driver: stub `sshpass` to a per-run tempdir on PATH (exit code = paint_ok), seed the
/// failed ledger, call cam2_painter_restore_final_recheck, and print the resulting ledger.
/// `paint_ok`: the exit code the stubbed remote paint-check returns (0 = genuinely painting).
/// `with_pw`: set CAM_PW. `painter_in_set`: seed cam2/painter into the failed ledger.
fn drive(paint_ok: i32, with_pw: bool, painter_in_set: bool) -> (String, String) {
    let lib = lib_path();
    let pw_line = if with_pw {
        "export CAM_PW=secret"
    } else {
        "unset CAM_PW || true"
    };
    let seed = if painter_in_set {
        r#"CAMBOX_PARALLEL_FAILED_LABELS=("cam1 (source, 10.77.9.61)" "cam2/painter, 10.77.9.62")
CAMBOX_PARALLEL_FAILED_IPS=("10.77.9.61" "10.77.9.62")"#
    } else {
        r#"CAMBOX_PARALLEL_FAILED_LABELS=("cam1 (source, 10.77.9.61)")
CAMBOX_PARALLEL_FAILED_IPS=("10.77.9.61")"#
    };
    let script = format!(
        r#"
set -uo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
# stub sshpass: consume its own flags, ignore the remote command, exit with paint_ok.
cat > "$TMP/bin/sshpass" <<STUB
#!/usr/bin/env bash
echo "SSHPASS_CALLED" >&2
exit {paint_ok}
STUB
chmod +x "$TMP/bin/sshpass"
export PATH="$TMP/bin:$PATH"
{pw_line}
source '{lib}'
{seed}
cam2_painter_restore_final_recheck "10.77.9.62"
echo "LABELS_COUNT=${{#CAMBOX_PARALLEL_FAILED_LABELS[@]}}"
echo "IPS_COUNT=${{#CAMBOX_PARALLEL_FAILED_IPS[@]}}"
printf 'LABEL=%s\n' "${{CAMBOX_PARALLEL_FAILED_LABELS[@]:-}}"
printf 'IP=%s\n' "${{CAMBOX_PARALLEL_FAILED_IPS[@]:-}}"
"#
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .expect("run recheck driver");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Genuinely painting on the final re-check -> cam2/painter is PRUNED from BOTH lockstep arrays,
/// leaving only cam1; a clear message is printed. This is the false-red fix.
#[test]
fn genuine_painting_recheck_prunes_cam2_painter_from_the_failed_ledger() {
    let (stdout, _stderr) = drive(0, true, true);
    assert!(
        stdout.contains("LABELS_COUNT=1") && stdout.contains("IPS_COUNT=1"),
        "#1126: a genuinely-painting cam2/painter must be pruned from both lockstep arrays. \
         stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("LABEL=cam1 (source, 10.77.9.61)")
            && !stdout.contains("LABEL=cam2/painter"),
        "#1126: the surviving label must be cam1, cam2/painter removed. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("IP=10.77.9.61") && !stdout.contains("IP=10.77.9.62"),
        "#1126: the lockstep IP for cam2/painter must be pruned too. stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("confirmed genuinely painting"),
        "#1126: the prune must be reported. stdout:\n{stdout}"
    );
}

/// NOT painting on the final re-check -> cam2/painter STAYS in the failed set, so the #860
/// ::error:: still fires legitimately (a genuinely dead painter must never be masked).
#[test]
fn a_still_dead_painter_is_left_in_the_failed_set() {
    let (stdout, stderr) = drive(1, true, true);
    assert!(
        stdout.contains("LABELS_COUNT=2") && stdout.contains("IPS_COUNT=2"),
        "#1126: a painter that is NOT genuinely painting must stay in the failed set (no false \
         prune that would mask a black monitor). stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("LABEL=cam2/painter, 10.77.9.62"),
        "#1126: cam2/painter must survive when the re-check does not confirm painting. stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("WARNING #1126") || stderr.contains("did NOT confirm"),
        "#1126: a failed re-check must WARN, not silently swallow. stderr:\n{stderr}"
    );
}

/// No CAM_PW (unit-test / no-credential context) -> a guarded no-op: no ssh, ledger untouched.
#[test]
fn no_credential_context_is_a_guarded_noop() {
    let (stdout, stderr) = drive(0, false, true);
    assert!(
        stdout.contains("LABELS_COUNT=2"),
        "#1126: with no CAM_PW the re-check must be a no-op (ledger untouched). stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("SSHPASS_CALLED"),
        "#1126: no CAM_PW must mean no ssh is attempted at all. stderr:\n{stderr}"
    );
}

/// cam2/painter not in the failed set -> a guarded no-op: no ssh, ledger untouched (nothing to fix).
#[test]
fn painter_not_in_failed_set_is_a_guarded_noop() {
    let (stdout, stderr) = drive(0, true, false);
    assert!(
        stdout.contains("LABELS_COUNT=1"),
        "#1126: when cam2/painter is not in the failed set, do nothing. stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("SSHPASS_CALLED"),
        "#1126: no re-check ssh when there is no cam2/painter failure to re-check. stderr:\n{stderr}"
    );
}

/// The genuine-paint remote check must use the presenter-aware signal, never bare is-active.
#[test]
fn recheck_uses_the_presenter_aware_painting_signal_not_bare_is_active() {
    let lib = fs::read_to_string(lib_path()).expect("read recheck lib");
    assert!(
        lib.contains("cam2_painter_restore_final_recheck()")
            && lib.contains("cam2_painter_genuine_paint_check_cmd()"),
        "#1126: the lib must define the recheck + its genuine-paint remote-check builder"
    );
    assert!(
        lib.contains("presenter: using DRM/KMS page-flip")
            && lib.contains("vblank-locked")
            && lib.contains("/dev/fb0"),
        "#1126: the paint check must use the SAME presenter-aware signal (KMS device held + \
         vblank-locked, OR /dev/fb0 held) as cam2_painter_restore_verify_cmds — never bare \
         is-active, which cannot tell a painting monitor from a black one (#863/#860)"
    );
}

/// The recheck helper is called in recording-e2e.sh cleanup(), AFTER the parallel wait and BEFORE
/// the surface helper (so the ledger is corrected before the ::error:: is emitted).
#[test]
fn cleanup_calls_recheck_between_wait_and_surface() {
    let s = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/recording-e2e.sh"),
    )
    .expect("read recording-e2e.sh");
    let wait = s
        .find("cambox_parallel_wait_and_report")
        .expect("wait call present");
    let recheck = s
        .find("cam2_painter_restore_final_recheck")
        .expect("#1126: cleanup() must call cam2_painter_restore_final_recheck");
    let surface = s
        .find("cambox_parallel_surface_painter_failure")
        .expect("surface call present");
    assert!(
        wait < recheck && recheck < surface,
        "#1126: the final re-check must run AFTER the parallel wait (so the box has finished \
         restarting) and BEFORE the surface helper (so a pruned painter never fires a false \
         ::error::). wait@{wait} recheck@{recheck} surface@{surface}"
    );
}
