//! issue 1351 — the cam2 painter (frame-probe) binary swap in `scripts/deploy-fleet.sh` must be
//! ETXTBSY-proof (scp to a sidecar + atomic rename), and must park the transient
//! `cam2-painter-deadman.timer` re-armer across the swap.
//!
//! Root cause (measured live, release E2E run 35770453666, head dcf1852b7): the painter step scp'd
//! the new binary STRAIGHT onto the live `/usr/local/bin/frame-probe` while `cam2-painter.service`
//! (Restart=always, re-armed every ~5 min by the deadman) was still executing that inode — scp
//! opened the destination for writing and hit `Text file busy` (ETXTBSY), so the `[1/8]`
//! frame-probe sha-pin (issue 1235) REFUSED the whole run. The fix (main design Prístup 1): scp to
//! `/usr/local/bin/frame-probe.new`, then `chmod 0755 … && mv -f … /usr/local/bin/frame-probe &&
//! sync` — a rename replaces the directory entry while the running process keeps its old inode, so
//! ETXTBSY cannot occur by construction. The deadman timer is stopped BEFORE the painter stop and
//! started AFTER the restore, so a re-arm cannot resurrect the old binary mid-swap.
//!
//! These tests drive the REAL `deploy-fleet.sh --frame-probe` (frame-probe-only mode) under PATH
//! stubs that LOG each remote `ssh`/`scp` invocation in order (the repo's `harness_deploy_fleet.rs`
//! / `frame_probe_parity_align_1138.rs` stub convention), then assert the swap contract on the
//! ordered log. Tier-0: this compiles + runs on CI only; locally proven via a bash replica.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn set_exec(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

/// Outcome of one stubbed frame-probe-only deploy run.
struct SwapRun {
    success: bool,
    output: String,
    /// One entry per remote invocation, in order: `SCP <dest>` or `SSH <remote-command>`.
    log: Vec<String>,
}

/// Run the REAL deploy-fleet.sh `--frame-probe <fixture>` (NO --binary) under PATH stubs over
/// CAMERA_SET=cam2. `scp_fail` forces the sidecar scp to fail (the swap-failure branch). Every
/// remote command is appended to a log file so ordering can be asserted.
fn run_swap(scp_fail: bool) -> SwapRun {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    let fp = tmp.path().join("frame-probe-artifact");
    fs::write(&fp, b"FRAME-PROBE-ARTIFACT-1351\n").unwrap();
    set_exec(&fp);

    let logf = tmp.path().join("remote.log");
    fs::write(&logf, b"").unwrap();

    let stub = |name: &str, body: &str| {
        let p = bin.join(name);
        fs::write(&p, body).unwrap();
        set_exec(&p);
    };
    // Non-remote local helpers the script + the remote commands resolve via PATH.
    stub("mount", "#!/usr/bin/env bash\nexit 0\n");
    stub("systemctl", "#!/usr/bin/env bash\nexit 0\n");
    stub("chmod", "#!/usr/bin/env bash\nexit 0\n");
    stub("mv", "#!/usr/bin/env bash\nexit 0\n");
    stub("sync", "#!/usr/bin/env bash\nexit 0\n");
    // sha256sum: match the local artifact read and the remote FINAL-path read to the same hash so
    // byte-verify passes (a stubbed no-op mv leaves no real file — the hash is what the gate reads).
    stub("sha256sum", "#!/usr/bin/env bash\necho 'aaaa  '\"$1\"\n");
    // gh MUST NOT be called in frame-probe-only mode.
    stub(
        "gh",
        "#!/usr/bin/env bash\necho 'GH-CALLED-UNEXPECTEDLY' >&2\nexit 1\n",
    );

    // sshpass: drop `-p <pass>`; log + honor scp (fail when SCP_FAIL); log + EXECUTE ssh remote
    // commands through bash so the mount/systemctl/chmod/mv/sync/sha256sum stubs run.
    let sshpass = r#"#!/usr/bin/env bash
shift 2
mode="$1"; shift
if [ "$mode" = "scp" ]; then
  echo "SCP ${@: -1}" >> "$FLEET_LOG"
  [ "${SCP_FAIL:-0}" = "1" ] && exit 1
  exit 0
fi
cmd="${@: -1}"
echo "SSH $cmd" >> "$FLEET_LOG"
bash -c "$cmd"
"#;
    stub("sshpass", sshpass);

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
        .env("FLEET_LOG", &logf)
        .env("SCP_FAIL", if scp_fail { "1" } else { "0" })
        .output()
        .expect("run deploy-fleet.sh frame-probe-only");

    let log = fs::read_to_string(&logf)
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .collect();

    SwapRun {
        success: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        log,
    }
}

/// Index of the first log entry that CONTAINS `needle` (panics with the log if none).
fn idx(log: &[String], needle: &str) -> usize {
    log.iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "no remote entry contains {needle:?}; log:\n{}",
                log.join("\n")
            )
        })
}

#[test]
fn scp_targets_the_sidecar_not_the_live_path() {
    // ETXTBSY-proof: the scp DESTINATION must be the sidecar `/usr/local/bin/frame-probe.new`,
    // never the live `/usr/local/bin/frame-probe` (which the running painter holds open).
    let r = run_swap(false);
    let scp = r
        .log
        .iter()
        .find(|l| l.starts_with("SCP "))
        .unwrap_or_else(|| panic!("no scp invocation; log:\n{}", r.log.join("\n")));
    assert!(
        scp.ends_with("/usr/local/bin/frame-probe.new"),
        "issue 1351: scp destination must be the sidecar `/usr/local/bin/frame-probe.new`, got: {scp:?}"
    );
    assert!(
        !scp.ends_with(":/usr/local/bin/frame-probe"),
        "issue 1351: scp must NOT write the live path directly (ETXTBSY); got: {scp:?}"
    );
}

#[test]
fn atomic_rename_follows_the_scp() {
    // After the sidecar scp, exactly ONE atomic rename makes it live.
    let r = run_swap(false);
    let scp_i = idx(&r.log, "SCP ");
    let mv_i = idx(
        &r.log,
        "mv -f /usr/local/bin/frame-probe.new /usr/local/bin/frame-probe",
    );
    assert!(
        mv_i > scp_i,
        "issue 1351: the `mv -f …frame-probe.new …frame-probe` rename must FOLLOW the sidecar scp; \
         log:\n{}",
        r.log.join("\n")
    );
    // And it is chmod'd + fsync'd in the same remote command (the swap is durable + executable).
    let mv_line = &r.log[mv_i];
    assert!(
        mv_line.contains("chmod 0755 /usr/local/bin/frame-probe.new") && mv_line.contains("sync"),
        "issue 1351: the swap must chmod the sidecar + sync after the rename; got: {mv_line:?}"
    );
}

#[test]
fn byte_verify_reads_the_final_path_not_the_sidecar() {
    // deploy-from-clean-tree Layer 3 — byte-verify is the real gate and must read the FINAL path
    // (a failed rename leaves stale/absent bytes there), never the sidecar.
    let r = run_swap(false);
    assert!(
        r.log
            .iter()
            .any(|l| l.contains("sha256sum /usr/local/bin/frame-probe 2>/dev/null")),
        "issue 1351: byte-verify must `sha256sum /usr/local/bin/frame-probe` (the FINAL path); \
         log:\n{}",
        r.log.join("\n")
    );
    assert!(
        !r.log
            .iter()
            .any(|l| l.contains("sha256sum /usr/local/bin/frame-probe.new")),
        "issue 1351: byte-verify must NOT read the sidecar `.new`; log:\n{}",
        r.log.join("\n")
    );
    assert!(
        r.output.contains("frame-probe byte-verify OK"),
        "issue 1351: the swap must byte-verify OK on the final path; out:\n{}",
        r.output
    );
}

#[test]
fn deadman_timer_is_parked_before_the_painter_stop() {
    // The transient re-armer must be stopped BEFORE the painter stop so it cannot resurrect the
    // old binary mid-swap (a deadman re-arm keeps the old inode busy → ETXTBSY).
    let r = run_swap(false);
    let deadman_stop_i = idx(&r.log, "systemctl stop cam2-painter-deadman.timer");
    let painter_stop_i = idx(&r.log, "systemctl stop cam2-painter.service");
    assert!(
        deadman_stop_i <= painter_stop_i,
        "issue 1351: `systemctl stop cam2-painter-deadman.timer` must PRECEDE the painter stop; \
         log:\n{}",
        r.log.join("\n")
    );
    // Same-command ordering (both live in the one remount,rw command): the deadman stop text
    // must appear before the painter stop text within that line.
    let line = &r.log[deadman_stop_i];
    if let (Some(a), Some(b)) = (
        line.find("stop cam2-painter-deadman.timer"),
        line.find("stop cam2-painter.service"),
    ) {
        assert!(
            a < b,
            "issue 1351: within the stop command, the deadman stop must precede the painter stop; \
             got: {line:?}"
        );
    }
}

#[test]
fn deadman_timer_is_rearmed_after_the_restore() {
    // Symmetric re-arm AFTER the #892 restore so it never races the painter restart.
    let r = run_swap(false);
    let start_i = idx(&r.log, "systemctl start cam2-painter-deadman.timer");
    let mv_i = idx(
        &r.log,
        "mv -f /usr/local/bin/frame-probe.new /usr/local/bin/frame-probe",
    );
    assert!(
        start_i > mv_i,
        "issue 1351: `systemctl start cam2-painter-deadman.timer` must come AFTER the swap+restore; \
         log:\n{}",
        r.log.join("\n")
    );
}

#[test]
fn scp_failure_still_rearms_the_deadman_and_remounts_ro() {
    // On a failed swap the deadman must be re-armed and the rootfs returned to read-only (never
    // left rw with a dark deadman).
    let r = run_swap(true);
    assert!(
        r.log.iter().any(|l| l.contains("SCP ")),
        "the scp must have been attempted; log:\n{}",
        r.log.join("\n")
    );
    assert!(
        r.log
            .iter()
            .any(|l| l.contains("systemctl start cam2-painter-deadman.timer")),
        "issue 1351: the scp-failure branch must re-arm the deadman timer; log:\n{}",
        r.log.join("\n")
    );
    assert!(
        r.log.iter().any(|l| l.contains("mount -o remount,ro /")),
        "issue 1351: the scp-failure branch must remount rootfs read-only; log:\n{}",
        r.log.join("\n")
    );
    // A failed swap must NOT print the byte-verify-OK proof.
    assert!(
        !r.output.contains("frame-probe byte-verify OK"),
        "a failed scp must not report a successful byte-verify; out:\n{}",
        r.output
    );
    // And the mv/rename must NOT run when the sidecar scp failed.
    assert!(
        !r.log
            .iter()
            .any(|l| l.contains("mv -f /usr/local/bin/frame-probe.new")),
        "issue 1351: a failed sidecar scp must not proceed to the rename; log:\n{}",
        r.log.join("\n")
    );
}
