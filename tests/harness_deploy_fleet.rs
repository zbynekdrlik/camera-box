//! Regression guards for `scripts/deploy-fleet.sh` (#73 — fleet binary alignment).
//!
//! #73: the four cameras drifted onto three different camera-box builds (cam1/cam4=dev.29,
//! cam3=dev.22, cam2=dev.19). cam2 was old enough that it predated the genlock-decimation
//! report and so was NOT genlocking — it emitted the old single-rate `Streaming: N fps`
//! line, never the `N fps emitted / M fps captured` form. The fix is a one-command fleet
//! updater that pushes ONE pinned CI binary to all four boxes and VERIFIES, per box, both the
//! new version AND that the box is now emitting the genlock report.
//!
//! These tests pin the script's load-bearing contract so a future edit that silently drops a
//! verification (the exact failure that let cam2 sit un-genlocked for weeks) fails CI:
//!   1. it sources the SINGLE camera source-of-truth (`camera-set.sh`), not a baked-in IP map;
//!   2. the deploy source is a CI artifact from a pushed ref (deploy-from-clean-tree), never a
//!      locally built binary — it downloads via `gh run download`, not `cargo build`;
//!   3. it performs the exact stop -> remount,rw -> scp -> start -> remount,ro cycle;
//!   4. it VERIFIES the genlock report (`fps emitted` / `fps captured`) per box and FAILS the
//!      box if it is missing — the cam2-regression guard;
//!   5. it verifies the post-deploy `--version` matches the deployed binary and fails on drift;
//!   6. its overall exit status is nonzero if ANY box fails a check (no false green).
//!
//! Style follows the repo's other harness_*.rs guards: read the real script + (where cheap)
//! exercise it through bash, asserting on the REAL contract, not a re-spelling of it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn deploy_fleet_script_exists_and_is_executable() {
    let p = manifest_dir().join("scripts/deploy-fleet.sh");
    assert!(p.exists(), "scripts/deploy-fleet.sh must exist (#73)");
    let bytes = fs::read(&p).unwrap();
    assert!(
        bytes.starts_with(b"#!"),
        "deploy-fleet.sh must start with a shebang"
    );
}

#[test]
fn deploy_fleet_sources_shared_camera_set() {
    // The IP map must come from the ONE source of truth, not be re-baked. A re-baked map
    // would silently diverge from camera-set.sh (the #24 single-source-of-truth invariant).
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("camera-set.sh"),
        "deploy-fleet.sh must source scripts/camera-set.sh for the cam1-4 IP map (single \
         source of truth), not hard-code device IPs."
    );
    // And it must NOT bake literal device IPs of its own (that would defeat sourcing).
    for ip in ["10.77.9.61", "10.77.9.62", "10.77.9.63", "10.77.9.64"] {
        assert!(
            !s.contains(ip),
            "deploy-fleet.sh hard-codes device IP {ip}; resolve it via camera-set.sh instead."
        );
    }
}

#[test]
fn deploy_fleet_deploys_ci_artifact_not_a_local_build() {
    // deploy-from-clean-tree.md: the deploy source is a CI artifact from a pushed ref, never
    // a locally built binary. The script must download via `gh run download` and must NOT
    // build locally.
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("gh run download"),
        "deploy-fleet.sh must obtain the binary via `gh run download` (a CI artifact from a \
         pushed ref) per deploy-from-clean-tree.md."
    );
    assert!(
        !s.contains("cargo build"),
        "deploy-fleet.sh must NOT build the binary locally — deploy only a CI artifact \
         (deploy-from-clean-tree.md)."
    );
}

#[test]
fn deploy_fleet_uses_exact_remount_stop_start_cycle() {
    // The CLAUDE.md "Build & Deploy" cycle: remount rw + stop, scp to /usr/local/bin, then
    // start + remount ro. Each piece is load-bearing — a missing remount,rw leaves the
    // read-only rootfs and the scp fails; a missing start leaves the service down.
    let s = read("scripts/deploy-fleet.sh");
    for needle in [
        "mount -o remount,rw /",
        "systemctl stop camera-box",
        "/usr/local/bin/camera-box",
        "systemctl start camera-box",
        "remount,ro /",
    ] {
        assert!(
            s.contains(needle),
            "deploy-fleet.sh missing required deploy step `{needle}` (CLAUDE.md Build & Deploy)."
        );
    }
}

#[test]
fn deploy_fleet_verifies_genlock_report_per_box() {
    // THE cam2 regression guard: cam2 ran an old build that never emitted the genlock report.
    // The updater must verify each box is emitting `fps emitted` / `fps captured` AND fail the
    // box (record it) when the line is absent. Without this check the script could report a
    // "successful" deploy of a binary that isn't genlocking — exactly the #73 failure.
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("fps emitted") && s.contains("fps captured"),
        "deploy-fleet.sh must grep the genlock report (`fps emitted` / `fps captured`) to \
         confirm each box is genlocking."
    );
    assert!(
        s.contains("no-genlock"),
        "deploy-fleet.sh must mark a box as FAILED (e.g. `no-genlock`) when the genlock \
         report is absent — a deploy with no genlock is not a success (#73 cam2 regression)."
    );
}

#[test]
fn deploy_fleet_verifies_version_after_deploy() {
    // Post-deploy version check: confirm the running binary is the one we shipped. A silent
    // scp-to-read-only-fs or wrong-binary failure must be caught, not reported as success.
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("camera-box --version"),
        "deploy-fleet.sh must read `camera-box --version` from each box to confirm the new \
         binary is actually running."
    );
    assert!(
        s.contains("version mismatch"),
        "deploy-fleet.sh must fail a box on a post-deploy version mismatch (no false green)."
    );
}

/// Exercise the real failure-accounting logic: stub out `sshpass`, `gh`, `camera-box` and
/// drive the script against fake cameras whose journals do NOT contain the genlock report.
/// The script must exit nonzero (the no-false-green contract) — proving the genlock gate is
/// not merely present in text but actually wired into the exit status.
#[test]
fn deploy_fleet_exits_nonzero_when_a_box_is_not_genlocking() {
    let tmp = std::env::temp_dir().join(format!("deployfleet_test_{}", std::process::id()));
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // Fake binary to "deploy": prints a version so NEW_VER resolves to `9.9.9-test`.
    let fakebin = tmp.join("camera-box-artifact");
    fs::write(
        &fakebin,
        "#!/usr/bin/env bash\necho 'camera-box 9.9.9-test'\n",
    )
    .unwrap();
    set_exec(&fakebin);

    // Fake remote tools that the script's remote command strings invoke. The sshpass stub
    // EXECUTES the remote command string (so the real `… | awk '{print $NF}'` pipe and the
    // real genlock grep run), but against these fakes instead of a live camera:
    //   * camera-box --version -> the NEW version (so the post-deploy version check passes)
    //   * journalctl           -> a log with NO genlock report (box not genlocking)
    //   * mount / systemctl    -> succeed silently
    // This isolates the ONE thing under test: a box with no genlock report must fail the run.
    for (name, body) in [
        (
            "camera-box",
            "#!/usr/bin/env bash\necho 'camera-box 9.9.9-test'\n",
        ),
        // journalctl prints the OLD single-rate line — never `fps emitted / fps captured`.
        (
            "journalctl",
            "#!/usr/bin/env bash\necho 'INFO camera_box: Streaming: 59.8 fps (299 frames)'\n",
        ),
        ("mount", "#!/usr/bin/env bash\nexit 0\n"),
        ("systemctl", "#!/usr/bin/env bash\nexit 0\n"),
    ] {
        let p = bin.join(name);
        fs::write(&p, body).unwrap();
        set_exec(&p);
    }

    // Stub `sshpass`: drop the leading `-p <pass>` and the ssh/scp option+target args, then
    // EXECUTE the remote command (the last arg, for ssh) through bash so the real pipes run.
    // For scp (no trailing remote command) it is a no-op success.
    let sshpass = bin.join("sshpass");
    fs::write(
        &sshpass,
        r#"#!/usr/bin/env bash
# args: -p <pass> <ssh|scp> <opts...> root@host [remote-cmd]
shift 2          # drop -p <pass>
mode="$1"; shift # ssh | scp
if [ "$mode" = "scp" ]; then exit 0; fi   # file copy: no-op success
# ssh: the remote command is the LAST argument; everything before is opts + target.
cmd="${@: -1}"
bash -c "$cmd"
"#,
    )
    .unwrap();
    set_exec(&sshpass);

    // Stub `gh` (must not be invoked because we pass --binary, but be safe).
    let gh = bin.join("gh");
    fs::write(&gh, "#!/usr/bin/env bash\nexit 1\n").unwrap();
    set_exec(&gh);

    let script = manifest_dir().join("scripts/deploy-fleet.sh");
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::new("bash")
        .arg(&script)
        .arg("--binary")
        .arg(&fakebin)
        .env("PATH", &path_env)
        .env("CAMERA_SET", "cam2") // single box keeps the test fast
        .env("SSH_PASS", "x")
        .env("GENLOCK_WAIT_TRIES", "1") // don't wait the full 60s for the (absent) genlock line
        .env("GENLOCK_WAIT_SECS", "0")
        .output()
        .expect("failed to run deploy-fleet.sh under stubs");

    let _ = fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "deploy-fleet.sh exited 0 for a box that emits NO genlock report — the genlock gate \
         is not wired into the exit status (no-false-green broken). stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // And it must say WHY (the box is flagged no-genlock), not fail for an unrelated reason.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("no-genlock") || combined.contains("NO genlock"),
        "deploy-fleet.sh failed but not for the missing genlock report; output:\n{combined}"
    );
}

#[cfg(unix)]
fn set_exec(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}
#[cfg(not(unix))]
fn set_exec(_p: &std::path::Path) {}
