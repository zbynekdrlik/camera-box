//! Behavioral guard for the canary-first dantesync fleet-upgrade tool
//! `scripts/dantesync-fleet-upgrade.sh` (#876).
//!
//! ## Why this script exists (#876 — dantesync fleet upgrade has no mechanism)
//!
//! dantesync is the rig's CLOCK AUTHORITY on every box (video genlock + Dante audio), so a
//! regression breaks the whole rig. The fleet drifted into FIVE versions across EIGHT boxes and
//! nobody noticed until #862's version-parity gate went in — the most damaging pair (strih vs
//! stream) was invisible. Two holes caused it: the Windows `DanteSyncUpdate` scheduled task died
//! at the DanteTimeSync->DanteSync rename (`f8dfd6c`, #18) and was never replaced (it still sits
//! on strih/stream `Enabled`, `Next Run = N/A`, `Last Result = 0` — silently dead), and Linux
//! never had ANY upgrade mechanism at all. Every convergence since is a manual eight-box
//! hand-roll — the hand-roll IS the bug.
//!
//! #862 (`dantesync-version-gate.sh`) is the DETECTION half. This tool is the REMEDIATION half: a
//! single, operator/agent-invoked, canary-first fleet-upgrade path covering BOTH OSes, targeting
//! the PINNED version (the same `DANTESYNC_VERSION_PIN` authority the gate uses — never "latest",
//! which would chase docs-only bumps and schedule pointless clock-master redeploys). It does NOT
//! re-introduce a per-box scheduled task: a task that silently stops scheduling is exactly as bad
//! as no task (the ticket's own lesson), so the mechanism's liveness IS the #862 gate's liveness
//! (loud, CI-wired), never a silent cron.
//!
//! Same PURE-PLANNER model as tests/upgrade_fleet_ndi.rs / tests/rig_mode.rs: these tests source
//! the REAL script (its `BASH_SOURCE != $0` guard skips the network/ssh-mutating main flow) and
//! exercise its pure functions — version ordering, release-URL construction, the exact remote
//! command text, canary selection — directly. NO test here ever ssh's a real box.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/dantesync-fleet-upgrade.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script (BASH_SOURCE!=$0 guard skips the network/mutating main flow) and run
/// `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Source + run `body` WITHOUT asserting success — for pure functions that intentionally return
/// non-zero (e.g. an override not in the set).
fn run_sourced_status(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_script(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(script())
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run dantesync-fleet-upgrade.sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The script must be SOURCE-SAFE — sourcing it (the unit-test harness) must not run the
/// network/ssh-mutating main flow.
#[test]
fn script_is_source_safe() {
    let out = run_sourced("echo SOURCED_OK");
    assert!(
        out.contains("SOURCED_OK"),
        "#876: the script must be source-safe (BASH_SOURCE != $0 guard) — sourcing ran main"
    );
}

#[test]
fn help_exits_zero_and_documents_canary_and_pin() {
    let (code, stdout, _stderr) = run_script(&["--help"]);
    assert_eq!(code, 0, "#876: --help must exit 0");
    let h = stdout.to_lowercase();
    assert!(
        h.contains("canary"),
        "#876: --help must document the canary discipline"
    );
    assert!(
        h.contains("pin") || h.contains("target"),
        "#876: --help must document the pinned target version"
    );
}

// --- version ordering (upgrade decision) ----------------------------------------------------

#[test]
fn upgrade_status_orders_semver_and_flags_unknown() {
    assert_eq!(
        run_sourced("dantesync_upgrade_status 1.8.20 1.8.41").trim(),
        "NEWER",
        "#876: target 1.8.41 is an upgrade over installed 1.8.20"
    );
    assert_eq!(
        run_sourced("dantesync_upgrade_status 1.8.41 1.8.41").trim(),
        "SAME"
    );
    assert_eq!(
        run_sourced("dantesync_upgrade_status 1.8.41 1.8.20").trim(),
        "OLDER",
        "#876: a downgrade must be detectable so it can be refused without --force"
    );
    // semver, not lexical: 1.8.9 < 1.8.10
    assert_eq!(
        run_sourced("dantesync_upgrade_status 1.8.9 1.8.10").trim(),
        "NEWER",
        "#876: must order semver numerically (1.8.10 > 1.8.9), not lexically"
    );
    assert_eq!(
        run_sourced("dantesync_upgrade_status '' 1.8.41").trim(),
        "UNKNOWN",
        "#876: an unread installed version must never be treated as any ordering"
    );
    assert_eq!(
        run_sourced("dantesync_upgrade_status 1.8.41 ''").trim(),
        "UNKNOWN",
        "#876: an unread target version must never be treated as any ordering"
    );
}

// --- release-URL construction (pinned tag, both OSes, sha companion) -------------------------

#[test]
fn release_url_linux_points_at_pinned_tag_asset() {
    assert_eq!(
        run_sourced("dantesync_release_url_linux 1.8.41").trim(),
        "https://github.com/zbynekdrlik/dantesync/releases/download/v1.8.41/dantesync-linux-amd64",
        "#876: Linux URL must pin the tag (vX.Y.Z), never releases/latest"
    );
}

#[test]
fn release_url_windows_points_at_pinned_tag_asset() {
    assert_eq!(
        run_sourced("dantesync_release_url_windows 1.8.41").trim(),
        "https://github.com/zbynekdrlik/dantesync/releases/download/v1.8.41/dantesync-windows-amd64.exe",
        "#876: Windows URL must pin the tag (vX.Y.Z), never releases/latest"
    );
}

// --- Linux upgrade command text -------------------------------------------------------------

#[test]
fn linux_upgrade_cmd_downloads_pinned_verifies_sha_stops_swaps_restarts() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.41");
    assert!(
        cmd.contains("releases/download/v1.8.41/dantesync-linux-amd64"),
        "#876: Linux upgrade must download the PINNED asset. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("sha256") || cmd.contains("sha256sum"),
        "#876: Linux upgrade must verify the download's SHA256. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("systemctl stop dantesync"),
        "#876: Linux upgrade must stop the service before swapping the binary. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/usr/local/bin/dantesync"),
        "#876: Linux upgrade must install to /usr/local/bin/dantesync (install.sh's path). Got:\n{cmd}"
    );
    assert!(
        cmd.contains("systemctl restart dantesync") || cmd.contains("systemctl start dantesync"),
        "#876: Linux upgrade must (re)start the service after the swap. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("dantesync --version"),
        "#876: Linux upgrade must read the version back for verification. Got:\n{cmd}"
    );
}

/// SAFETY: the download + SHA verification must happen BEFORE the service is stopped — a failed
/// download must never leave the clock master stopped.
#[test]
fn linux_upgrade_cmd_downloads_and_verifies_before_stopping_the_service() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.41");
    let dl = cmd
        .find("dantesync-linux-amd64")
        .expect("#876: expected the download line");
    let sha = cmd
        .find("sha256")
        .or_else(|| cmd.find("sha256sum"))
        .expect("#876: expected a sha256 verification step");
    let stop = cmd
        .find("systemctl stop dantesync")
        .expect("#876: expected a service-stop line");
    assert!(
        dl < stop && sha < stop,
        "#876: download+verify must precede the service stop so a bad download never leaves the \
         clock master down. Got:\n{cmd}"
    );
}

/// SAFETY: the current binary is backed up BEFORE it is overwritten, so a failed verification
/// can be rolled back to the exact previous binary.
#[test]
fn linux_upgrade_cmd_backs_up_current_binary_before_swap() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.41");
    let bak = cmd
        .find("/usr/local/bin/dantesync.bak")
        .expect("#876: expected a backup of the current binary");
    // the live binary is overwritten via `install -m ... /usr/local/bin/dantesync`; the backup
    // must appear before that overwrite so a failed verification can be rolled back.
    let overwrite = cmd
        .find("install -m")
        .expect("#876: expected the live-binary overwrite (install -m)");
    assert!(
        bak < overwrite,
        "#876: the current binary must be backed up BEFORE it is overwritten. Got:\n{cmd}"
    );
}

#[test]
fn linux_rollback_cmd_restores_backup_and_restarts() {
    let cmd = run_sourced("dantesync_linux_rollback_cmd");
    assert!(
        cmd.contains("/usr/local/bin/dantesync.bak") && cmd.contains("/usr/local/bin/dantesync"),
        "#876: Linux rollback must restore the .bak over the live binary. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("systemctl restart dantesync") || cmd.contains("systemctl start dantesync"),
        "#876: Linux rollback must restart the service. Got:\n{cmd}"
    );
}

// --- Windows upgrade command text -----------------------------------------------------------

#[test]
fn windows_upgrade_cmd_downloads_pinned_verifies_stops_swaps_starts() {
    let cmd = run_sourced("dantesync_windows_upgrade_cmd 1.8.41");
    assert!(
        cmd.contains("powershell") && cmd.contains("-NoProfile"),
        "#876: Windows upgrade runs headless PowerShell (session-agnostic, ssh-legal). Got:\n{cmd}"
    );
    assert!(
        cmd.contains("releases/download/v1.8.41/dantesync-windows-amd64.exe"),
        "#876: Windows upgrade must download the PINNED asset. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("Get-FileHash") && cmd.to_uppercase().contains("SHA256"),
        "#876: Windows upgrade must verify the download's SHA256. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("Stop-Service"),
        "#876: Windows upgrade must stop the service before replacing the exe. Got:\n{cmd}"
    );
    assert!(
        cmd.contains(r"C:\Program Files\DanteSync\dantesync.exe"),
        "#876: Windows upgrade must replace the installed exe path. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("Start-Service"),
        "#876: Windows upgrade must start the service after the swap. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("--version"),
        "#876: Windows upgrade must read the version back for verification. Got:\n{cmd}"
    );
}

/// The Windows upgrade actively PURGES the dead `DanteSyncUpdate` relic task (retire, don't
/// replace) — the direct answer to "a task that silently stops scheduling is as bad as no task".
#[test]
fn windows_upgrade_cmd_purges_the_dead_dantesyncupdate_task() {
    let cmd = run_sourced("dantesync_windows_upgrade_cmd 1.8.41");
    assert!(
        cmd.contains("DanteSyncUpdate") && cmd.contains("/Delete"),
        "#876: the Windows path must purge the dead DanteSyncUpdate scheduled task. Got:\n{cmd}"
    );
}

#[test]
fn windows_upgrade_cmd_backs_up_current_exe_before_replace() {
    let cmd = run_sourced("dantesync_windows_upgrade_cmd 1.8.41");
    let bak = cmd
        .find(r"dantesync.exe.bak")
        .expect("#876: expected a backup of the current exe");
    let copy = cmd
        .find("Copy-Item")
        .expect("#876: expected a Copy-Item swap");
    assert!(
        bak < copy || cmd.matches("Copy-Item").count() >= 2,
        "#876: the current exe must be backed up BEFORE the new exe overwrites it. Got:\n{cmd}"
    );
}

#[test]
fn windows_rollback_cmd_restores_backup_and_starts() {
    let cmd = run_sourced("dantesync_windows_rollback_cmd");
    assert!(
        cmd.contains("dantesync.exe.bak")
            && cmd.contains(r"C:\Program Files\DanteSync\dantesync.exe"),
        "#876: Windows rollback must restore the .bak over the live exe. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("Start-Service"),
        "#876: Windows rollback must start the service. Got:\n{cmd}"
    );
}

#[test]
fn purge_dead_task_cmd_is_a_forced_idempotent_delete() {
    let cmd = run_sourced("dantesync_windows_purge_dead_task_cmd");
    assert!(
        cmd.contains("schtasks"),
        "#876: purge uses schtasks. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/Delete"),
        "#876: purge deletes the task. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/TN") && cmd.contains("DanteSyncUpdate"),
        "#876: names the task. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/F"),
        "#876: forced delete (idempotent, no prompt). Got:\n{cmd}"
    );
}

// --- canary selection (one representative per OS class present) ------------------------------

#[test]
fn resolve_canary_default_covers_each_os_class_present() {
    // Linux (cam1 first) + Windows (strih first) -> one canary of each class.
    let out = run_sourced("dantesync_resolve_canary 'cam1 cam2 imag-nb' 'strih stream' ''");
    assert_eq!(
        out.trim(),
        "cam1 strih",
        "#876: a green Linux canary must NOT authorize touching a Windows box — the default \
         canary set must include one representative of EACH OS class present. Got:\n{out}"
    );
}

#[test]
fn resolve_canary_single_class_picks_first_member() {
    assert_eq!(
        run_sourced("dantesync_resolve_canary 'cam1 cam2 imag-nb' '' ''").trim(),
        "cam1",
        "#876: a Linux-only set defaults to exactly one canary (the first Linux node)"
    );
    assert_eq!(
        run_sourced("dantesync_resolve_canary '' 'strih stream' ''").trim(),
        "strih",
        "#876: a Windows-only set defaults to exactly one canary (the first Windows node)"
    );
}

#[test]
fn resolve_canary_honors_override_when_all_members_present() {
    let out = run_sourced("dantesync_resolve_canary 'cam1 cam2' 'strih stream' 'cam2 stream'");
    assert_eq!(
        out.trim(),
        "cam2 stream",
        "#876: an explicit --canary override wins over the class-coverage default"
    );
}

#[test]
fn resolve_canary_rejects_override_member_not_in_set() {
    let (code, _stdout, stderr) =
        run_sourced_status("dantesync_resolve_canary 'cam1' 'strih' 'cam9'");
    assert_ne!(
        code, 0,
        "#876: an override naming a node not in the fleet must fail"
    );
    assert!(
        stderr.contains("cam9"),
        "#876: the error must name the offending override node. Got stderr:\n{stderr}"
    );
}

// --- remaining-after-canary -----------------------------------------------------------------

#[test]
fn remaining_after_canary_excludes_canaries_preserves_order() {
    assert_eq!(
        run_sourced(
            "dantesync_remaining_after_canary 'cam1 cam2 imag-nb strih stream' 'cam1 strih'"
        )
        .trim(),
        "cam2 imag-nb stream",
        "#876: remaining = full set minus the canary set, order preserved"
    );
}

// --- structural invariants (source-text) ----------------------------------------------------

/// The whole canary SET must be upgraded (and verified) BEFORE the loop over the rest of the
/// fleet — the core canary-first safety property.
#[test]
fn canary_is_upgraded_before_the_remaining_fleet() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    let canary_loop = s
        .find("for node in $CANARY_SET")
        .expect("#876: expected a loop over the canary set");
    let rest_loop = s
        .find("for node in $REST")
        .expect("#876: expected a loop over the remaining (non-canary) nodes");
    assert!(
        canary_loop < rest_loop,
        "#876: the canary set MUST be upgraded before the loop over the rest of the fleet"
    );
}

/// A node already on the target version is a documented no-op — never a needless
/// service-restart of the clock master.
#[test]
fn same_version_is_a_documented_noop() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("SAME") && s.contains("nothing to do"),
        "#876: the SAME-version case must be a documented no-op, not a needless re-swap/restart"
    );
}

/// A downgrade must be refused unless --force is given.
#[test]
fn downgrade_is_refused_without_force() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("OLDER") && s.contains("force"),
        "#876: an OLDER target (downgrade) must be refused unless --force"
    );
}

/// Reuse, never reinvent: the version parser + the pin come from the #862 gate (single source of
/// truth), and canary verification is the existing dantesync-gate.sh (PTP-lock + fresh offset).
#[test]
fn reuses_version_gate_parser_and_pin_and_dantesync_gate() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("dantesync-version-gate.sh"),
        "#876: must source dantesync-version-gate.sh for the version parser + DANTESYNC_VERSION_PIN"
    );
    assert!(
        s.contains("dantesync_version_from_version_output"),
        "#876: must reuse the gate's version parser, never a second parser"
    );
    assert!(
        s.contains("DANTESYNC_VERSION_PIN"),
        "#876: the default target must be the gate's pin (single source of truth, not 'latest')"
    );
    assert!(
        s.contains("dantesync-gate.sh"),
        "#876: canary verification must reuse dantesync-gate.sh (PTP-lock + fresh in-bound offset)"
    );
}

/// Knowingly-offline nodes are excluded via the SAME shared mechanism every other fleet gate
/// uses — never a second exclusion mechanism.
#[test]
fn reuses_shared_offline_ack_mechanism() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("cambox-offline-ack.sh"),
        "#876: knowingly-offline node exclusion must reuse scripts/lib/cambox-offline-ack.sh"
    );
}

/// --dry-run must be wired and must NOT invoke either OS upgrade command.
#[test]
fn dry_run_is_wired_and_mutates_nothing() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("--dry-run") || s.contains("DRY_RUN"),
        "#876: a --dry-run mode (read + report, change nothing) must exist"
    );
}

/// A canary that fails verification must be rolled back to the previous binary — and, per the
/// canary-first contract, the rest of the fleet must NOT be touched.
#[test]
fn rolls_back_on_verification_failure() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("dantesync_linux_rollback_cmd") && s.contains("dantesync_windows_rollback_cmd"),
        "#876: the main flow must roll a node back (both OSes) when verification fails"
    );
}
