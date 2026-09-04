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

// --- Windows upgrade .ps1 content + file invocation -----------------------------------------
// The Windows path SENDS A .ps1 (scp -O) and runs it with `-File`, never a nested
// `powershell -Command "..."` over ssh (which fails SILENTLY — .claude/rules/rig-state-inspection.md
// §2). So `dantesync_windows_upgrade_ps` / `_rollback_ps` return the .ps1 CONTENT, and
// `dantesync_windows_run_ps_file_cmd` is the `-File` invocation.

#[test]
fn windows_upgrade_ps_downloads_pinned_verifies_stops_swaps_starts() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.41");
    assert!(
        ps.contains("releases/download/v1.8.41/dantesync-windows-amd64.exe"),
        "#876: Windows upgrade must download the PINNED asset. Got:\n{ps}"
    );
    assert!(
        ps.contains("Get-FileHash") && ps.to_uppercase().contains("SHA256"),
        "#876: Windows upgrade must verify the download's SHA256. Got:\n{ps}"
    );
    assert!(
        ps.contains("Stop-Service"),
        "#876: Windows upgrade must stop the service before replacing the exe. Got:\n{ps}"
    );
    assert!(
        ps.contains(r"C:\Program Files\DanteSync\dantesync.exe"),
        "#876: Windows upgrade must replace the installed exe path. Got:\n{ps}"
    );
    assert!(
        ps.contains("Start-Service"),
        "#876: Windows upgrade must start the service after the swap. Got:\n{ps}"
    );
    assert!(
        ps.contains("--version"),
        "#876: Windows upgrade must read the version back for verification. Got:\n{ps}"
    );
}

/// The Windows path must run the uploaded .ps1 by FILE (the repo's rig-state-inspection.md §2
/// rule), never a nested `powershell -Command "..."`.
#[test]
fn windows_run_ps_file_cmd_uses_file_invocation() {
    let cmd = run_sourced(r#"dantesync_windows_run_ps_file_cmd 'C:\Windows\Temp\x.ps1'"#);
    assert!(
        cmd.contains("powershell") && cmd.contains("-NoProfile"),
        "#876: must run headless PowerShell. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("-ExecutionPolicy Bypass"),
        "#876: must bypass the execution policy for the .ps1. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("-File"),
        "#876: must run the .ps1 by -File, never a nested -Command (rig-state-inspection.md §2). Got:\n{cmd}"
    );
    assert!(
        !cmd.contains("-Command"),
        "#876: must NOT use a nested -Command (fails silently over ssh). Got:\n{cmd}"
    );
}

/// The Windows upgrade actively PURGES the dead `DanteSyncUpdate` relic task (retire, don't
/// replace) — the direct answer to "a task that silently stops scheduling is as bad as no task".
#[test]
fn windows_upgrade_ps_purges_the_dead_dantesyncupdate_task() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.41");
    assert!(
        ps.contains("DanteSyncUpdate") && ps.contains("/Delete"),
        "#876: the Windows path must purge the dead DanteSyncUpdate scheduled task. Got:\n{ps}"
    );
}

/// SAFETY: the exe is backed up BEFORE the new exe overwrites it — asserted on the REAL
/// statement order (backup `Copy-Item ... $exe $bak` precedes swap `Copy-Item ... $tmp $exe`).
#[test]
fn windows_upgrade_ps_backs_up_current_exe_before_replace() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.41");
    let backup = ps
        .find("Copy-Item -Force $exe $bak")
        .expect("#876: expected the exe backup (Copy-Item -Force $exe $bak)");
    let swap = ps
        .find("Copy-Item -Force $tmp $exe")
        .expect("#876: expected the exe swap (Copy-Item -Force $tmp $exe)");
    assert!(
        backup < swap,
        "#876: the current exe must be backed up BEFORE the new exe overwrites it. Got:\n{ps}"
    );
}

/// SAFETY: the swap is wrapped in a try/catch that restores the .bak and restarts on any failure
/// past the point of no return, then rethrows — a failed swap never leaves the master down.
#[test]
fn windows_upgrade_ps_self_heals_on_swap_failure() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.41");
    assert!(
        ps.contains("try {") && ps.contains("catch {"),
        "#876: the swap must be wrapped in try/catch self-heal. Got:\n{ps}"
    );
    assert!(
        ps.contains("Copy-Item -Force $bak $exe") && ps.contains("throw"),
        "#876: the catch must restore the .bak and rethrow. Got:\n{ps}"
    );
}

#[test]
fn windows_rollback_ps_restores_backup_and_starts() {
    let ps = run_sourced("dantesync_windows_rollback_ps");
    assert!(
        ps.contains("dantesync.exe.bak")
            && ps.contains(r"C:\Program Files\DanteSync\dantesync.exe"),
        "#876: Windows rollback must restore the .bak over the live exe. Got:\n{ps}"
    );
    assert!(
        ps.contains("Start-Service"),
        "#876: Windows rollback must start the service. Got:\n{ps}"
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

/// A node that fails VERIFICATION (the swap provably completed) must be rolled back to the
/// previous binary — both OSes.
#[test]
fn rolls_back_on_verification_failure() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("dantesync_linux_rollback_cmd") && s.contains("dantesync_windows_rollback_ps"),
        "#876: the main flow must roll a node back (both OSes) when verification fails"
    );
    // and the rollback must be gated on the verify-failure branch, not an upgrade-command failure
    let verify_fail = s
        .find("verification failed after upgrade")
        .expect("#876: expected the verify-failure branch");
    let rollback = s
        .find("rollback_node \"$name\"")
        .expect("#876: expected a rollback_node call");
    assert!(
        verify_fail < rollback,
        "#876: rollback_node must be called from the verify-failure branch. Got context around it."
    );
}

/// The remote upgrade scripts SELF-HEAL: a failure past the point of no return restores the
/// previous binary ON THE BOX, so the orchestrator must NOT blind-rollback on an upgrade-command
/// failure (which — with a pre-existing .bak — would stop a HEALTHY master and downgrade it).
#[test]
fn upgrade_command_failure_is_reported_not_blind_rolled_back() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    // Linux self-heal: an ERR trap restoring the .bak, disarmed on success.
    assert!(
        s.contains("_dantesync_restore") && s.contains("trap '_dantesync_restore' ERR"),
        "#876: the Linux upgrade must self-heal via an ERR trap that restores the .bak"
    );
    // The upgrade-command-failure branch reports self-heal and does NOT call rollback_node.
    assert!(
        s.contains("self-healed to its previous version (not rolled forward)"),
        "#876: an upgrade-command failure must be reported as self-healed, never blind-rolled-back"
    );
}

/// Single-node verification of a SLAVE node opts out of the NTP-master concept
/// (`master_arg=""` -> `--ntp-master ""`), so verifying a non-master node (e.g. stream, cam2)
/// never hits the gate's "master not among configured nodes" refusal. The MASTER node instead
/// gets its OWN name as `--ntp-master` (the #1014 master-aware median+freshness grade) so that
/// verifying it right after its own restart tolerates the restart-induced fleet sawtooth
/// (#1077, replacing the old blanket `--ntp-master ""` for every node).
#[test]
fn verify_grades_master_master_aware_and_opts_out_for_slaves() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("dantesync_is_ntp_master"),
        "#1077: verify_node must branch on whether the node is the NTP master"
    );
    assert!(
        s.contains(r#"--ntp-master "$master_arg""#),
        "#1077: verify must pass a COMPUTED --ntp-master arg (self for the master, empty for a slave)"
    );
    assert!(
        s.contains(r#"master_arg="""#),
        "#1077: a non-master (slave) node must opt out of the NTP master concept (empty master_arg)"
    );
    assert!(
        s.contains(r#"master_arg="$name""#),
        "#1077: the master node must be graded with its OWN name as --ntp-master (master-aware grade)"
    );
}

/// The Windows path sends a .ps1 file (scp) and runs it by -File — never a nested -Command.
#[test]
fn windows_path_sends_a_ps1_file() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("scp_node") && s.contains("dantesync_windows_upgrade_ps"),
        "#876: the Windows path must scp the .ps1 content, not inline a nested -Command"
    );
    assert!(
        s.contains("dantesync_windows_run_ps_file_cmd"),
        "#876: the Windows path must run the uploaded .ps1 by -File"
    );
}

/// The cam boxes run DELIBERATE read-only roots (the deploy-fleet.sh remount cycle exists for
/// exactly this), and the FIRST live canary run failed on cam1 with
/// `cp: cannot create regular file '/usr/local/bin/dantesync.bak': Read-only file system`
/// (2026-08-16, v1.8.42 roll). The generated Linux upgrade script must therefore detect a
/// non-writable binary dir, remount rw BEFORE the backup step, and restore ro via the EXIT
/// trap so BOTH the success path and the self-heal ERR path end read-only again.
#[test]
fn linux_upgrade_cmd_remounts_ro_root_rw_before_backup_and_restores_ro_on_exit() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.42");
    let rw = cmd
        .find("mount -o remount,rw /")
        .expect("upgrade cmd must remount a read-only root rw before touching the binary");
    let bak = cmd
        .find("/usr/local/bin/dantesync.bak")
        .expect("#876: expected the backup of the current binary");
    assert!(
        rw < bak,
        "the rw remount must come BEFORE the backup step (the exact 2026-08-16 canary failure). Got:\n{cmd}"
    );
    assert!(
        cmd.contains("mount -o remount,ro /"),
        "the upgrade cmd must restore the read-only root on exit. Got:\n{cmd}"
    );
    let ro_helper = cmd
        .find("_dantesync_remount_ro")
        .expect("expected the ro-restore helper wired into the EXIT trap");
    assert!(
        ro_helper < bak,
        "the ro-restore helper must be armed before the point of no return. Got:\n{cmd}"
    );
}

/// The orchestrator-invoked rollback (VERIFY-failure path) hits the same read-only root and
/// must carry the same remount handling.
#[test]
fn linux_rollback_cmd_handles_read_only_root() {
    let cmd = run_sourced("dantesync_linux_rollback_cmd");
    assert!(
        cmd.contains("mount -o remount,rw /"),
        "rollback cmd must remount a read-only root rw before restoring the backup. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("mount -o remount,ro /"),
        "rollback cmd must restore the read-only root afterwards. Got:\n{cmd}"
    );
}

/// The dead-task purge inside the generated Windows upgrade .ps1 must be idempotent on an
/// ALREADY-ABSENT task: the live 2026-08-16 v1.8.43 canary failed with
/// `schtasks : ERROR: The system cannot find the file specified.` because the task had been
/// purged by the previous roll and the bare `schtasks ... 2>$null` still surfaces the native
/// stderr as a terminating NativeCommandError under `$ErrorActionPreference = "Stop"` — the
/// upgrade aborted BEFORE the swap. The purge must be routed through `cmd /c` with full
/// redirection so PowerShell never sees the stderr of an absent-task delete.
#[test]
fn windows_upgrade_ps_purges_dead_task_idempotently_via_cmd_wrapper() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.43");
    let purge = ps
        .lines()
        .find(|l| l.contains("DanteSyncUpdate") && l.contains("/Delete"))
        .expect("#876: the Windows upgrade must still purge the dead DanteSyncUpdate task");
    assert!(
        purge.contains("cmd /c"),
        "the purge must run under cmd /c so an absent task's stderr never becomes a \
         terminating NativeCommandError. Got: {purge}"
    );
    assert!(
        purge.contains(">nul 2>&1") || purge.contains("2>&1"),
        "the purge must swallow the absent-task stderr inside cmd. Got: {purge}"
    );
}

// =============================================================================================
// #1077 — the v1.8.42/43 live-roll defects in the LINUX path.
//
// (c) Non-root Linux nodes (imag-nb ssh newlevel@, dev1 --local) had NO privilege escalation —
//     the generated script does root-only ops (mount remount, install, systemctl) and silently
//     assumed a root@ ssh session, so it died at `mount: /: must be superuser`. AND a box without
//     curl (cam3: no curl, broken apt) could not fetch the binary at all.
// (d) Verifying the NTP MASTER (strih) right after ITS OWN dantesync restart measured the
//     restart-induced fleet-wide sawtooth (rc=20 rolled back a HEALTHY swap twice).
//
// These tests source the REAL script (its BASH_SOURCE!=$0 guard skips the network/ssh-mutating
// main flow) and exercise the new pure functions + generated command text + structural wiring.
// NO test here ever ssh's a real box (same PURE-PLANNER model as the rest of this file).
// =============================================================================================

// --- (c) privilege escalation: needs-sudo + the run-script-by-file escalation wrapper ---------

/// A `root@` ssh node (the cam boxes) needs no escalation; a non-root user (newlevel — imag-nb,
/// dev1 --local) does.
#[test]
fn needs_sudo_true_for_non_root_and_false_for_root() {
    assert_eq!(
        run_sourced("if dantesync_needs_sudo newlevel; then echo SUDO; else echo NOSUDO; fi")
            .trim(),
        "SUDO",
        "#1077: a non-root ssh user must be escalated"
    );
    assert_eq!(
        run_sourced("if dantesync_needs_sudo root; then echo SUDO; else echo NOSUDO; fi").trim(),
        "NOSUDO",
        "#1077: a root ssh user is already privileged — no sudo"
    );
}

/// A root node runs the uploaded script by FILE with no sudo (the cam boxes are root@).
#[test]
fn linux_run_script_cmd_root_runs_by_file_without_sudo() {
    let out = run_sourced(r#"dantesync_linux_run_script_cmd root /tmp/x.sh newlevel"#);
    assert!(
        out.contains(r#"bash "/tmp/x.sh""#),
        "#1077: a root node must run the uploaded script by file. Got:\n{out}"
    );
    assert!(
        !out.contains("sudo"),
        "#1077: a root node must NOT escalate via sudo. Got:\n{out}"
    );
}

/// A non-root node escalates: prefer passwordless `sudo -n` (dev1), else feed the password to
/// `sudo -S` via stdin (imag-nb). The uploaded script is run by FILE (bash "$path"), never inline,
/// and the password is fed to sudo only — never written into the on-disk script file.
#[test]
fn linux_run_script_cmd_non_root_escalates_prefer_passwordless_then_sudo_s() {
    let out = run_sourced(r#"dantesync_linux_run_script_cmd newlevel /tmp/x.sh secretpw"#);
    assert!(
        out.contains("sudo -n true"),
        "#1077: a non-root node must probe passwordless sudo first (sudo -n). Got:\n{out}"
    );
    assert!(
        out.contains("sudo -S -p ''"),
        "#1077: a non-root node must fall back to sudo -S reading the password. Got:\n{out}"
    );
    assert!(
        out.contains(r#"bash "/tmp/x.sh""#),
        "#1077: must run the uploaded script by file (never a nested inline -c). Got:\n{out}"
    );
    assert!(
        out.contains("secretpw"),
        "#1077: the password must be fed to sudo -S (via printf | sudo -S). Got:\n{out}"
    );
    let probe = out.find("sudo -n true").expect("sudo -n probe");
    let fallback = out.find("sudo -S").expect("sudo -S fallback");
    assert!(
        probe < fallback,
        "#1077: passwordless sudo -n must be tried BEFORE the sudo -S password fallback. Got:\n{out}"
    );
}

/// The password given to the escalation wrapper is fed to sudo via stdin (printf | sudo -S), so it
/// is never baked into the on-disk script FILE the orchestrator scp's to the box.
#[test]
fn escalation_feeds_password_via_stdin_not_the_uploaded_file() {
    let out = run_sourced(r#"dantesync_linux_run_script_cmd newlevel /tmp/x.sh secretpw"#);
    assert!(
        out.contains("printf") && out.contains("| sudo -S"),
        "#1077: the sudo password must be piped to sudo -S, not embedded in the uploaded script. Got:\n{out}"
    );
}

// --- (c) curl-less fetch: staged binary (scp'd by dev1) -> curl -> wget -> fail loud -----------

/// A box without curl (cam3, broken apt) must still upgrade: the orchestrator stages a verified
/// binary via scp, so the generated script prefers that pre-placed binary; on-box curl then wget
/// are only standalone fallbacks; and it fails LOUD when none is available.
#[test]
fn linux_upgrade_cmd_prefers_staged_binary_then_curl_then_wget() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.43");
    assert!(
        cmd.contains("/tmp/dantesync-staged"),
        "#1077: the generated script must prefer a pre-staged binary (curl-less path for cam3). Got:\n{cmd}"
    );
    assert!(
        cmd.contains("command -v curl") && cmd.contains("command -v wget"),
        "#1077: on-box fetch must fall back to curl then wget when there is no staged binary. Got:\n{cmd}"
    );
    let staged = cmd
        .find("dantesync-staged")
        .expect("#1077: expected the pre-staged binary check");
    let curl = cmd
        .find("command -v curl")
        .expect("#1077: expected the curl fallback branch");
    let wget = cmd
        .find("command -v wget")
        .expect("#1077: expected the wget fallback branch");
    assert!(
        staged < curl && curl < wget,
        "#1077: fetch order must be staged-binary -> curl -> wget. Got:\n{cmd}"
    );
    assert!(
        cmd.to_lowercase().contains("neither curl nor wget"),
        "#1077: must fail LOUD when no staged binary and no curl/wget is available. Got:\n{cmd}"
    );
}

/// Whichever fetch path is taken (staged / curl / wget), the sha256 verification still runs before
/// the service is touched — a corrupt scp OR download never reaches the clock master.
#[test]
fn linux_upgrade_cmd_sha_verifies_after_fetch_before_stop_for_every_path() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.43");
    let sha = cmd
        .find("sha256sum")
        .expect("#1077: expected a sha256sum verification of the fetched binary");
    let stop = cmd
        .find("systemctl stop dantesync")
        .expect("#1077: expected the service-stop line");
    assert!(
        sha < stop,
        "#1077: the sha256 verify must still precede the service stop. Got:\n{cmd}"
    );
    // the staged branch copies both the binary and its .sha256 into the verify tmp dir
    assert!(
        cmd.contains("/tmp/dantesync-staged.sha256"),
        "#1077: the staged path must carry its own .sha256 for the on-box re-verify. Got:\n{cmd}"
    );
}

// --- (c) flow wiring: stage on dev1 once, upload the script as a FILE, run escalated -----------

/// The Linux upgrade flow stages the binary ONCE on dev1 (download + sha256-verify), then runs the
/// uploaded script as a FILE via the escalation wrapper — never the old inline
/// `ssh_node "$addr" "$(dantesync_linux_upgrade_cmd ...)"` (which had no escalation and needed curl
/// on every box).
#[test]
fn linux_upgrade_stages_on_dev1_and_runs_script_by_file_escalated() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("ensure_linux_binary_staged"),
        "#1077: the orchestrator must stage (download + verify once) the binary on dev1"
    );
    assert!(
        s.contains("dantesync_linux_run_script_cmd"),
        "#1077: the Linux path must run the uploaded script via the escalation wrapper"
    );
    assert!(
        s.contains("DANTESYNC_LINUX_SH_REMOTE"),
        "#1077: the Linux path must upload the generated script to a file and run it by path"
    );
    assert!(
        !s.contains(r#"ssh_node "$addr" "$(dantesync_linux_upgrade_cmd"#),
        "#1077: the Linux upgrade must no longer be run inline over ssh (escalation needs a file)"
    );
    assert!(
        !s.contains(r#"bash -c "$(dantesync_linux_upgrade_cmd"#),
        "#1077: the --local upgrade must no longer be run inline (dev1 is non-root — needs sudo)"
    );
}

/// The dev1-side staging downloads the PINNED asset and sha256-verifies it on dev1 before it is
/// scp'd to any node.
#[test]
fn ensure_linux_binary_staged_downloads_pinned_and_verifies_on_dev1() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    let start = s
        .find("ensure_linux_binary_staged()")
        .expect("#1077: expected the ensure_linux_binary_staged function");
    let region = &s[start..(start + 900).min(s.len())];
    assert!(
        region.contains("dantesync_release_url_linux") || region.contains("releases/download"),
        "#1077: staging must fetch the PINNED release asset. Got:\n{region}"
    );
    assert!(
        region.contains("sha256sum"),
        "#1077: dev1-side staging must sha256-verify the downloaded binary. Got:\n{region}"
    );
}

/// The orchestrator-invoked rollback (VERIFY-failure path) hits the same root-only remount, so it
/// must ALSO run its script by file via the escalation wrapper, not the old inline path.
#[test]
fn linux_rollback_runs_by_file_escalated() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    let start = s
        .find("rollback_node()")
        .expect("#1077: expected rollback_node");
    let region = &s[start..(start + 1000).min(s.len())];
    assert!(
        region.contains("dantesync_linux_run_script_cmd"),
        "#1077: rollback must also escalate via the run-script wrapper. Got:\n{region}"
    );
    assert!(
        !region.contains(r#"bash -c "$(dantesync_linux_rollback_cmd"#),
        "#1077: the --local rollback must no longer run inline (dev1 non-root -> needs sudo)"
    );
}

// --- (d) master-node verify: master-aware grade + longer bounded settle window ----------------

/// Only the configured NTP master matches; an empty master name means no node is the master.
#[test]
fn is_ntp_master_matches_only_the_configured_master() {
    assert_eq!(
        run_sourced("if dantesync_is_ntp_master strih strih; then echo YES; else echo NO; fi")
            .trim(),
        "YES",
        "#1077: the configured master must be recognized"
    );
    assert_eq!(
        run_sourced("if dantesync_is_ntp_master cam2 strih; then echo YES; else echo NO; fi")
            .trim(),
        "NO",
        "#1077: a non-master node must not be treated as the master"
    );
    assert_eq!(
        run_sourced("if dantesync_is_ntp_master strih ''; then echo YES; else echo NO; fi").trim(),
        "NO",
        "#1077: an empty master name means NO node is the master (never a false match)"
    );
}

/// The master node gets a LONGER, bounded settle window (retry to steady state) — the restart-
/// induced sawtooth converges minutes later, so the strict ~60s slave window rolled back a
/// healthy swap. The window is bounded (seq/tries, clear PASS/FAIL), never a silent sleep-and-hope.
#[test]
fn master_verify_uses_a_longer_bounded_settle_window() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    assert!(
        s.contains("MASTER_GATE_WAIT_TRIES") && s.contains("MASTER_GATE_WAIT_SECS"),
        "#1077: the master must get its own bounded settle window (tries x secs)"
    );
    assert!(
        s.contains("NTP_MASTER"),
        "#1077: the master node name must be resolvable (NTP_MASTER, default from the gate's own)"
    );
    // still a bounded retry loop -- the master path reuses the same seq/tries + gate_rc PASS/FAIL,
    // never an unbounded wait.
    assert!(
        !s.contains("while true") && !s.contains("while :"),
        "#1077: the settle loop must stay bounded (no unbounded while-true wait)"
    );
}

/// #1077 review: `ensure_linux_binary_staged` must publish the memo (`STAGED_LOCAL_DIR`) only
/// AFTER a fully-verified download — a failed dev1 fetch must NOT poison the memo (which would
/// falsely short-circuit the next node's call). Assert the `STAGED_LOCAL_DIR="$dir"` publish comes
/// AFTER the `sha256sum` verification inside the function.
#[test]
fn ensure_linux_binary_staged_publishes_memo_only_after_sha_verify() {
    let s = fs::read_to_string(script()).expect("read dantesync-fleet-upgrade.sh");
    let start = s
        .find("ensure_linux_binary_staged()")
        .expect("#1077: expected the ensure_linux_binary_staged function");
    let region = &s[start..(start + 1200).min(s.len())];
    let sha = region
        .find("sha256sum")
        .expect("#1077: expected the sha256 verification");
    let publish = region
        .find(r#"STAGED_LOCAL_DIR="$dir""#)
        .expect("#1077: the memo must be published from a local $dir, not armed before the fetch");
    assert!(
        sha < publish,
        "#1077: STAGED_LOCAL_DIR must be published only AFTER the sha256 verify, so a failed \
         dev1 download never poisons the memo. Got region:\n{region}"
    );
}

// --- (3) real ro-mount detection: read the actual mount state (findmnt), not a write probe -----

/// #1077 defect (3): the ro-root detection must read the ACTUAL mount state — `findmnt`, with a
/// `/proc/mounts` fallback for a findmnt-less box — never a `touch` write probe (which conflates a
/// read-only filesystem with a mere permission error, doubly so now the script always runs
/// escalated). Mirrors setup-device.sh's ensure_root_writable()/root_mount_is_readonly() (#599):
/// `ro` is matched as the FIRST comma-token so `errors=remount-ro` never false-positives.
#[test]
fn linux_upgrade_cmd_detects_ro_root_via_findmnt_with_proc_mounts_fallback() {
    let cmd = run_sourced("dantesync_linux_upgrade_cmd 1.8.43");
    assert!(
        cmd.contains("findmnt -no OPTIONS /"),
        "#1077: ro detection must read the real mount options of / via findmnt. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/proc/mounts"),
        "#1077: the findmnt-less fallback must ALSO read the real mount state (/proc/mounts), \
         never a write probe. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("ro | ro,*"),
        "#1077: 'ro' must be matched as the FIRST comma-token (never a bare 'ro' substring that \
         'errors=remount-ro' would satisfy). Got:\n{cmd}"
    );
    assert!(
        !cmd.contains("dantesync-write-test"),
        "#1077: the write-probe conflation must be fully removed — both detect paths read real \
         mount state. Got:\n{cmd}"
    );
    // the detected ro root is still remounted rw before the swap (the action is unchanged).
    assert!(
        cmd.contains("mount -o remount,rw /"),
        "#1077: a detected ro root must still be remounted rw for the swap. Got:\n{cmd}"
    );
}

/// The orchestrator-invoked rollback hits the same ro root and must use the same real-mount-state
/// detection (findmnt + `/proc/mounts` fallback), never a bare write probe.
#[test]
fn linux_rollback_cmd_detects_ro_root_via_findmnt_with_proc_mounts_fallback() {
    let cmd = run_sourced("dantesync_linux_rollback_cmd");
    assert!(
        cmd.contains("findmnt -no OPTIONS /"),
        "#1077: rollback ro detection must also read the real mount state via findmnt. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("/proc/mounts"),
        "#1077: rollback's findmnt-less fallback must ALSO read /proc/mounts, never a write probe. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("ro | ro,*"),
        "#1077: rollback must match 'ro' as the FIRST comma-token. Got:\n{cmd}"
    );
    assert!(
        !cmd.contains("dantesync-write-test"),
        "#1077: rollback must not fall back to a write probe. Got:\n{cmd}"
    );
    assert!(
        cmd.contains("mount -o remount,rw /"),
        "#1077: a detected ro root must still be remounted rw for the rollback. Got:\n{cmd}"
    );
}

// --- #1265: wait for the dantesync PROCESS to exit before touching the exe ----------------------
// `Stop-Service dantesync` returns when the SCM reports STOPPED, but on strih the dantesync.exe
// PROCESS lingers a few seconds after that (its Npcap capture handle on the X520 tears down
// slowly), so an immediate `Copy-Item -Force $tmp $exe` hits "The process cannot access the file
// ... because it is being used by another process", the self-heal restores the .bak, and the
// canary ABORTS the whole roll (live 2026-09-03 19:02Z: `CANARY strih failed`, exit 10; stream had
// swapped fine minutes earlier because its process exits promptly). The generated scripts must
// wait on the REAL resource (the process holding the exe) between stop and copy — a bounded
// `Wait-Process` plus a forced `Stop-Process` backstop for a wedged process — never a blind sleep.

/// Slice-order helper: the byte offset of `needle` in `hay`, or a loud panic naming what is missing.
fn offset_of(hay: &str, needle: &str, what: &str) -> usize {
    hay.find(needle).unwrap_or_else(|| {
        panic!("#1265: expected {what} ({needle:?}) in the generated script. Got:\n{hay}")
    })
}

#[test]
fn windows_upgrade_ps_waits_for_the_process_to_exit_between_stop_and_swap_1265() {
    let ps = run_sourced("dantesync_windows_upgrade_ps 1.8.53");
    let stop = offset_of(&ps, "Stop-Service dantesync", "the service stop");
    let wait = offset_of(
        &ps,
        "Wait-Process -Name dantesync",
        "a bounded wait for the dantesync PROCESS to exit",
    );
    let swap = offset_of(&ps, "Copy-Item -Force $tmp $exe", "the exe swap");
    assert!(
        stop < wait && wait < swap,
        "#1265: the swap must wait for the dantesync process to exit AFTER Stop-Service and BEFORE \
         Copy-Item (stop={stop} wait={wait} swap={swap}). Got:\n{ps}"
    );
    // a process that survives the SCM stop + the bounded wait is wedged (the dead-pcap-handle
    // dantesync of nic-swap-timesync-recovery.md) — the documented cure is a forced kill, so the
    // script must fall back to Stop-Process -Force before the swap, never leave the file locked.
    let kill = offset_of(
        &ps,
        "Stop-Process -Name dantesync -Force",
        "the forced-kill backstop for a wedged process",
    );
    assert!(
        wait < kill && kill < swap,
        "#1265: the forced-kill backstop must sit between the bounded wait and the swap \
         (wait={wait} kill={kill} swap={swap}). Got:\n{ps}"
    );
    // exact-name process cmdlets only — `dantesync-tray.exe` (the autostart tray, a separate
    // process) must never be waited on or killed by the daemon swap.
    assert!(
        !ps.contains("dantesync*") && !ps.contains("dantesync-tray"),
        "#1265: the process wait/kill must target the daemon process name EXACTLY, never a wildcard \
         that would also hit dantesync-tray. Got:\n{ps}"
    );
}

#[test]
fn windows_rollback_ps_waits_for_the_process_to_exit_between_stop_and_restore_1265() {
    let ps = run_sourced("dantesync_windows_rollback_ps");
    let stop = offset_of(&ps, "Stop-Service dantesync", "the service stop");
    let wait = offset_of(
        &ps,
        "Wait-Process -Name dantesync",
        "a bounded wait for the dantesync PROCESS to exit",
    );
    let restore = offset_of(&ps, "Copy-Item -Force $bak $exe", "the .bak restore");
    assert!(
        stop < wait && wait < restore,
        "#1265: the rollback must ALSO wait for the process to exit between Stop-Service and the \
         .bak restore, or a lingering process blocks the restore the same way (stop={stop} \
         wait={wait} restore={restore}). Got:\n{ps}"
    );
    let kill = offset_of(
        &ps,
        "Stop-Process -Name dantesync -Force",
        "the forced-kill backstop for a wedged process",
    );
    assert!(
        wait < kill && kill < restore,
        "#1265: the rollback's forced-kill backstop must sit between the wait and the restore \
         (wait={wait} kill={kill} restore={restore}). Got:\n{ps}"
    );
}
