//! issue 1105 — wire the kernel-cmdline ISOLATION drift check into the E2E `[0/8]` preflight (the
//! issue-784 lib's SECOND consumer).
//!
//! Issue 784 ported the issue-780 two-consumer shape to a new facet, `scripts/lib/imag-cmdline-
//! isolation.sh`: the pure `imag_cmdline_isolation_verdict` + `imag_cmdline_isolation_gather_remote_
//! snippet`, wired into `drift-guard --check-imag` (consumer 1). This ticket adds the missing
//! consumer 2 — the E2E `[0/8]` fail-fast preflight in `scripts/recording-e2e.sh` — so a live-rig
//! kernel CPU-isolation drift (`isolcpus=`/`nohz_full=`/scoped-`rcu_nocbs`, the #784/#842 footgun
//! re-appearing via a stray grub.d drop-in or hand-edit) aborts the ~40-min run at minute 0 instead
//! of projecting a starved run (OBS's ~119-thread pool piled onto one core).
//!
//! The new `imag_cmdline_isolation_preflight_assert HOST [USER]` mirrors `imag_display_path_preflight_
//! assert` exactly: gather `/proc/cmdline` over ssh, run the SHARED verdict, `return 1` (naming the
//! offending token) on DRIFT, WARN on UNKNOWN, print an OK line otherwise. Like the display-path
//! sibling it is thin ssh glue — the JUDGMENT is the pure verdict, already covered by the issue-784
//! tests in `tests/drift_guard.rs`. Here we prove the fail-fast CONTRACT by driving the function
//! through a fake `ssh` on `PATH` (no rig), plus `fs::read_to_string` wiring assertions on
//! `recording-e2e.sh` (source + guarded `[0/8]` call before the DanteSync gate).
//!
//! RED before the function + wiring exist (function undefined -> exit 127; `.find()` panics); GREEN
//! after. Same source-the-real-lib convention as `tests/harness_imag_display_path_780.rs`.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/imag-cmdline-isolation.sh")
}

fn read_repo(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// A unique temp dir holding a fake `ssh` that ignores every arg and echoes `$FAKE_SSH_OUT`. The
/// preflight builds `timeout 15 ssh …`; with this dir prepended to PATH the real `timeout` execs
/// our fake `ssh`, so the function's whole gather→verdict→exit-code path runs with a canned
/// `/proc/cmdline` and no rig.
fn make_fake_ssh_dir() -> PathBuf {
    let mut dir = std::env::temp_dir();
    let uniq = format!(
        "cbox1105-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    dir.push(uniq);
    std::fs::create_dir_all(&dir).expect("mk fake ssh dir");
    let ssh = dir.join("ssh");
    std::fs::write(
        &ssh,
        "#!/usr/bin/env bash\n# fake ssh (issue 1105 test) — ignore args, echo the canned gather.\nprintf '%s' \"$FAKE_SSH_OUT\"\nexit 0\n",
    )
    .expect("write fake ssh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake ssh");
    }
    dir
}

/// Run `imag_cmdline_isolation_preflight_assert testhost newlevel` with a fake ssh returning
/// `fake_ssh_out` as the gathered `/proc/cmdline` block. Returns (function_return_code, stdout,
/// stderr). The bash harness uses `set -uo pipefail` (NOT `-e`, exactly like the #780 harness) so a
/// `return 1` from the function does not abort the harness; the code is surfaced via `PREFLIGHT_RC=N`.
fn run_preflight(fake_ssh_out: &str) -> (i32, String, String) {
    let dir = make_fake_ssh_dir();
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", dir.display(), orig_path);
    let harness = "set -uo pipefail\n. \"$LIB\"\n\
         imag_cmdline_isolation_preflight_assert testhost newlevel\n\
         echo \"PREFLIGHT_RC=$?\"";
    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("LIB", lib())
        .env("PATH", &path)
        .env("FAKE_SSH_OUT", fake_ssh_out)
        .current_dir(manifest_dir())
        .output()
        .expect("run preflight harness");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let rc = stdout
        .lines()
        .find_map(|l| l.strip_prefix("PREFLIGHT_RC="))
        .and_then(|n| n.trim().parse::<i32>().ok())
        .unwrap_or_else(|| panic!("no PREFLIGHT_RC in stdout: {stdout:?}"));
    (rc, stdout, String::from_utf8_lossy(&out.stderr).to_string())
}

// ---- the lib gains the preflight consumer -----------------------------------------------------

#[test]
fn lib_defines_the_preflight_function_1105() {
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -uo pipefail\n. \"$LIB\"\ndeclare -F imag_cmdline_isolation_preflight_assert")
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    assert!(
        out.status.success(),
        "imag_cmdline_isolation_preflight_assert must be defined by the shared lib: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---- the fail-fast contract (DRIFT -> 1, OK -> 0, UNKNOWN -> 0 + WARN) -------------------------

#[test]
fn preflight_fails_fast_on_an_isolcpus_drift_naming_the_token_1105() {
    // isolcpus=/nohz_full= on /proc/cmdline is the #784/#842 footgun -> DRIFT -> return 1.
    let (rc, stdout, stderr) = run_preflight(
        "CMDLINE|root=UUID=x ro preempt=full isolcpus=2-11 nohz_full=10,11 rcu_nocbs=all\n",
    );
    assert_eq!(
        rc, 1,
        "a cmdline-isolation DRIFT must fail-fast (return 1): stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("isolcpus"),
        "the offending token must be named on stderr: {stderr:?}"
    );
    let low = stderr.to_lowercase();
    assert!(
        low.contains("drift") || low.contains("refusing"),
        "stderr must announce the drift/refusal to start: {stderr:?}"
    );
}

#[test]
fn preflight_fails_fast_on_a_scoped_rcu_nocbs_drift_1105() {
    // A SCOPED per-core rcu_nocbs (NOT =all) is the isolation family -> DRIFT -> return 1.
    let (rc, _stdout, stderr) =
        run_preflight("CMDLINE|root=UUID=x ro preempt=full rcu_nocbs=2-11\n");
    assert_eq!(
        rc, 1,
        "a scoped rcu_nocbs must fail-fast: stderr={stderr:?}"
    );
    assert!(
        stderr.contains("rcu_nocbs=2-11"),
        "the scoped rcu_nocbs value must be named: {stderr:?}"
    );
}

#[test]
fn preflight_passes_the_clean_live_cmdline_1105() {
    // The exact healthy imag-nb shape (read live 2026-08-18): rcu_nocbs=all (legit issue-482
    // low-latency token), NO isolcpus/nohz_full -> OK -> return 0, so a healthy run is NOT false-failed.
    let (rc, stdout, stderr) = run_preflight(
        "CMDLINE|BOOT_IMAGE=/boot/vmlinuz-7.0.0-28-generic root=UUID=e98d6b72 ro quiet splash preempt=full rcu_nocbs=all vt.handoff=7\n",
    );
    assert_eq!(
        rc, 0,
        "a clean cmdline (rcu_nocbs=all only) must pass: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.to_lowercase().contains("preflight ok"),
        "an OK line must be printed on stdout: stdout={stdout:?}"
    );
}

#[test]
fn preflight_warns_but_does_not_abort_on_an_unknown_gather_1105() {
    // An empty/unreadable gather (SSH hiccup) is UNKNOWN -> WARN, never a false fail: the [0/8]
    // reachability preflight already owns genuine unreachability.
    let (rc, stdout, stderr) = run_preflight("");
    assert_eq!(
        rc, 0,
        "an UNKNOWN gather must NOT fail the run: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.to_uppercase().contains("WARN"),
        "an UNKNOWN gather must WARN on stderr: stderr={stderr:?}"
    );
}

// ---- issue 1105: recording-e2e.sh is wired to the SAME shared lib, guarded, before DanteSync ----

#[test]
fn recording_e2e_sources_the_cmdline_isolation_lib_1105() {
    let body = read_repo("scripts/recording-e2e.sh");
    assert!(
        body.contains(r#". "$HERE/lib/imag-cmdline-isolation.sh""#),
        "recording-e2e.sh must source the shared cmdline-isolation lib"
    );
}

#[test]
fn recording_e2e_calls_the_cmdline_preflight_fail_fast_guarded_before_dantesync_1105() {
    let body = read_repo("scripts/recording-e2e.sh");
    // The [0/8] preflight call must exist, target the imag host, and hard-exit on a proven drift.
    let call = body
        .find(r#"imag_cmdline_isolation_preflight_assert "$IMAG_IP""#)
        .expect(
            "recording-e2e.sh must call imag_cmdline_isolation_preflight_assert with the imag host",
        );
    let win = &body[call..(call + 120).min(body.len())];
    assert!(
        win.contains("|| exit 1"),
        "the cmdline-isolation preflight must fail-fast (|| exit 1): {win}"
    );
    // New imag hard-abort site: guarded by IMAG_OFFLINE_ACKED so an acked-offline imag skips cleanly
    // instead of hard-aborting the whole run (.claude/rules/imag-offline-ack.md inventory).
    let guard_start = call.saturating_sub(1400);
    assert!(
        body[guard_start..call].contains("IMAG_OFFLINE_ACKED"),
        "the cmdline-isolation preflight must be guarded by IMAG_OFFLINE_ACKED"
    );
    // Early fail-fast: the banner must precede the DanteSync gate (same ordering guard as #780).
    let banner = body
        .find("imag kernel-cmdline isolation preflight")
        .expect("a [0/8] banner announcing the cmdline-isolation preflight must exist");
    let dantesync = body
        .find("[0/8] DanteSync NTP+PTP gate")
        .expect("the DanteSync banner must still exist");
    assert!(
        banner < dantesync,
        "the cmdline-isolation preflight must run before the DanteSync gate (early fail-fast)"
    );
}
