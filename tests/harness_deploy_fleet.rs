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

/// Outcome of running deploy-fleet.sh under stubs.
struct RunResult {
    success: bool,
    output: String,
}

/// Drive the REAL script against ONE fake camera, with stubbed `sshpass`/`gh` and stubbed
/// remote tools (`camera-box`, `journalctl`, `mount`, `systemctl`, `sha256sum`). The sshpass
/// stub EXECUTES the remote command string through bash, so the script's real pipes
/// (`… | awk '{print $NF}'`, the genlock grep, the fatal grep) actually run — this tests the
/// wired-in behavior + exit status, not a re-spelling.
///
/// * `remote_version` — what `camera-box --version` reports on the box AFTER deploy.
/// * `journal_line`   — the single streaming line `journalctl` emits (genlock vs old form vs panic).
/// * `sha_match`      — when false, the remote `sha256sum` returns a different hash (byte-verify fails).
///
/// The artifact's version is fixed to `9.9.9-test`; pass a different `remote_version` to force a
/// post-deploy mismatch.
fn run_fleet(remote_version: &str, journal_line: &str, sha_match: bool) -> RunResult {
    let tmp = std::env::temp_dir().join(format!(
        "deployfleet_test_{}_{}",
        std::process::id(),
        // unique per call so concurrent test threads don't collide
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // The artifact to "deploy": prints version 9.9.9-test so NEW_VER resolves.
    let fakebin = tmp.join("camera-box-artifact");
    fs::write(
        &fakebin,
        "#!/usr/bin/env bash\necho 'camera-box 9.9.9-test'\n",
    )
    .unwrap();
    set_exec(&fakebin);

    // The REMOTE camera-box reports `remote_version` (controls the post-deploy version check).
    let stub = |name: &str, body: String| {
        let p = bin.join(name);
        fs::write(&p, body).unwrap();
        set_exec(&p);
    };
    stub(
        "camera-box",
        format!("#!/usr/bin/env bash\necho 'camera-box {remote_version}'\n"),
    );
    // journalctl emits the chosen streaming text (genlock present/absent/panic). `printf '%b'`
    // so an embedded `\n` in `journal_line` yields multiple log lines.
    stub(
        "journalctl",
        format!("#!/usr/bin/env bash\nprintf '%b\\n' '{journal_line}'\n"),
    );
    stub("mount", "#!/usr/bin/env bash\nexit 0\n".to_string());
    stub("systemctl", "#!/usr/bin/env bash\nexit 0\n".to_string());
    // sha256sum: the script calls it BOTH locally (on the artifact) and remotely (on the deployed
    // file). Return a fixed hash for the local artifact path; for the remote path return the same
    // hash when sha_match, else a different one (forces a byte-verify mismatch).
    let remote_hash = if sha_match { "aaaa" } else { "bbbb" };
    stub(
        "sha256sum",
        format!(
            "#!/usr/bin/env bash\ncase \"$1\" in\n  */usr/local/bin/camera-box) echo '{remote_hash}  /usr/local/bin/camera-box' ;;\n  *) echo 'aaaa  '\"$1\" ;;\nesac\n"
        ),
    );

    // sshpass: drop `-p <pass>`, then for scp no-op success; for ssh EXECUTE the remote command
    // (the last arg) through bash so the real pipes run against the stubs above. Rewrite the
    // absolute `/usr/local/bin/camera-box --version` invocation to bare `camera-box` so the PATH
    // stub catches it (the absolute path the script uses in prod won't resolve to a PATH stub).
    // The sha256sum invocation keeps its absolute path arg (the sha256sum stub matches on it).
    stub(
        "sshpass",
        r#"#!/usr/bin/env bash
shift 2          # drop -p <pass>
mode="$1"; shift # ssh | scp
if [ "$mode" = "scp" ]; then exit 0; fi
cmd="${@: -1}"
cmd="${cmd//\/usr\/local\/bin\/camera-box --version/camera-box --version}"
bash -c "$cmd"
"#
        .to_string(),
    );
    // gh must not be needed (we pass --binary) — make it fail loudly if invoked.
    stub("gh", "#!/usr/bin/env bash\nexit 1\n".to_string());

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
        .env("GENLOCK_WAIT_TRIES", "1") // don't wait the full timeout for an absent genlock line
        .env("GENLOCK_WAIT_SECS", "0")
        .output()
        .expect("failed to run deploy-fleet.sh under stubs");

    let _ = fs::remove_dir_all(&tmp);
    RunResult {
        success: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

const GENLOCK_LINE: &str =
    "INFO camera_box: Streaming: 30.0 fps emitted / 60.0 fps captured (150 sent, 300 captured)";
// The older, non-decimating shape a box with no genlock decimation configured logs (src/main.rs
// only emits the "fps emitted / fps captured" report when decimation is ACTIVE). #451 broadens
// deploy-fleet.sh's alive-signal to accept this shape too (shared with upgrade-fleet-ndi.sh's
// #445 broadening) — it is genuinely ALIVE, just not decimating, so it must NOT false-fail.
const OLD_NON_GENLOCK_LINE: &str = "INFO camera_box: Streaming: 59.8 fps (299 frames)";
// A line that matches NONE of the shared alive-signal alternatives at all — the real "box is
// dead/never came up" case the no-genlock gate must still catch.
const NO_ALIVE_SIGNAL_LINE: &str = "INFO camera_box: waiting for USB capture device to appear";

/// no-false-green: a box whose journal has NO alive signal at all (no genlock report, no older
/// "Streaming: X fps" shape, no "NDI sender ready") must make the run exit nonzero and be
/// flagged `no-genlock`. Proves the alive gate is wired into the exit status (the #73
/// cam2-regression: a box that deploys fine but never comes up streaming must NOT report
/// success). #451: previously this test used the OLDER "Streaming: X fps" shape as its "not
/// genlocking" fixture — that shape is now a RECOGNIZED alive signal (see
/// `deploy_fleet_accepts_old_streaming_shape_as_alive` below), so this test was updated to a
/// line that genuinely matches nothing, to keep guarding the real "box never came up" case.
#[test]
fn deploy_fleet_exits_nonzero_when_a_box_emits_no_alive_signal() {
    let r = run_fleet("9.9.9-test", NO_ALIVE_SIGNAL_LINE, true);
    assert!(
        !r.success,
        "exited 0 for a box that emits NO alive signal at all (no-false-green broken). output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("no-genlock") || r.output.contains("NO genlock"),
        "failed, but not for the missing alive signal; output:\n{}",
        r.output
    );
}

/// #451: a box logging ONLY the older, non-decimating "Streaming: X.Y fps" shape (no genlock
/// decimation configured on that box) is genuinely ALIVE and must NOT be false-verify-failed —
/// deploy-fleet.sh must accept the SAME broadened alive-signal pattern upgrade-fleet-ndi.sh
/// already tolerates (#445), by sourcing the shared scripts/lib/ndi-alive.sh.
#[test]
fn deploy_fleet_accepts_old_streaming_shape_as_alive() {
    let r = run_fleet("9.9.9-test", OLD_NON_GENLOCK_LINE, true);
    assert!(
        r.success,
        "exited nonzero for a box emitting the older 'Streaming: X fps' shape — deploy-fleet.sh \
         must accept the shared broadened alive-signal (#445/#451). output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("FLEET ALIGNED"),
        "success but no FLEET ALIGNED line; output:\n{}",
        r.output
    );
}

/// #451: a box logging the generic "NDI sender ready" line (no fps report yet) must also be
/// accepted as alive via the shared pattern.
#[test]
fn deploy_fleet_accepts_ndi_sender_ready_as_alive() {
    let r = run_fleet(
        "9.9.9-test",
        "INFO camera_box: NDI sender ready, streaming as CAM1",
        true,
    );
    assert!(
        r.success,
        "exited nonzero for a box emitting 'NDI sender ready' — deploy-fleet.sh must accept the \
         shared broadened alive-signal (#445/#451). output:\n{}",
        r.output
    );
}

/// #451: deploy-fleet.sh must source the ONE shared scripts/lib/ndi-alive.sh instead of
/// hard-coding its own narrower alive-signal pattern, so it can never again silently drift out
/// of sync with upgrade-fleet-ndi.sh's broadening (the exact #445/#451 latent re-break).
#[test]
fn deploy_fleet_sources_shared_ndi_alive_lib() {
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains("lib/ndi-alive.sh"),
        "#451: deploy-fleet.sh must source scripts/lib/ndi-alive.sh instead of hard-coding its \
         own 'fps emitted .* fps captured' pattern."
    );
    assert!(
        !s.contains("'fps emitted .* fps captured'"),
        "#451: deploy-fleet.sh must not hard-code the narrow genlock-only pattern any more — it \
         must call emit_ok_grep_pattern() from the shared lib."
    );
}

// ---------------------------------------------------------------------------------------------
// #694 — same stale-journal-across-restart exposure #693 fixed for recording-e2e.sh's preflight:
// deploy-fleet.sh's post-restart genlock-report + FATAL scan both read `journalctl -u camera-box
// -n 200/300` unscoped -- a WARN/FATAL line from the box's PREVIOUS camera-box.service instance
// (killed by the restart this very deploy just performed) can leak into the lookback window.
// Fix: reuse scripts/lib/capture-rate-guard.sh's capture_rate_journalctl_cmd(), scoped to the
// CURRENT process's _SYSTEMD_INVOCATION_ID (resolved via `systemctl show -p InvocationID`).
// ---------------------------------------------------------------------------------------------

#[test]
fn deploy_fleet_sources_shared_capture_rate_guard_lib() {
    let s = read("scripts/deploy-fleet.sh");
    assert!(
        s.contains(". \"$HERE/lib/capture-rate-guard.sh\""),
        "#694: deploy-fleet.sh must source scripts/lib/capture-rate-guard.sh to reuse \
         capture_rate_journalctl_cmd() instead of duplicating the invocation-id-scoping logic."
    );
}

#[test]
fn deploy_fleet_scopes_post_restart_journal_reads_to_the_current_invocation() {
    let s = read("scripts/deploy-fleet.sh");
    let invocation_pos = s.find("InvocationID").expect(
        "#694: deploy-fleet.sh must resolve camera-box's CURRENT InvocationID before reading \
         the post-restart journal (same fix as #693's recording-e2e.sh preflight)",
    );
    let cmd_call_pos = s.find("capture_rate_journalctl_cmd").expect(
        "#694: the genlock/FATAL reads must be built via the shared capture_rate_journalctl_cmd",
    );
    assert!(
        invocation_pos < cmd_call_pos,
        "#694: the invocation id must be resolved BEFORE it is used to scope the journalctl read"
    );
    assert!(
        !s.contains("journalctl -u camera-box --no-pager -n 200 2>/dev/null | grep"),
        "#694: the OLD unscoped-across-restarts genlock-report literal must be gone (replaced by \
         capture_rate_journalctl_cmd)."
    );
    assert!(
        !s.contains("journalctl -u camera-box --no-pager -n 300 2>/dev/null | grep"),
        "#694: the OLD unscoped-across-restarts FATAL-scan literal must be gone (replaced by \
         capture_rate_journalctl_cmd)."
    );
}

/// Happy path: a box on the new version that IS emitting the genlock report and byte-matches
/// must make the run exit 0 with "FLEET ALIGNED". Without this, a future edit that breaks the
/// success path (e.g. always-fail) goes uncaught.
#[test]
fn deploy_fleet_exits_zero_when_box_genlocks_and_matches() {
    let r = run_fleet("9.9.9-test", GENLOCK_LINE, true);
    assert!(
        r.success,
        "exited nonzero for a healthy, genlocking, byte-matched box. output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("FLEET ALIGNED"),
        "success but no FLEET ALIGNED line; output:\n{}",
        r.output
    );
}

/// no-false-green: a post-deploy version that differs from the shipped artifact must fail the
/// run and be flagged `version=…` — even though the box IS genlocking.
#[test]
fn deploy_fleet_exits_nonzero_on_version_mismatch() {
    let r = run_fleet("1.2.3-stale", GENLOCK_LINE, true);
    assert!(
        !r.success,
        "exited 0 despite a post-deploy version mismatch (no-false-green broken). output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("version mismatch") || r.output.contains("version="),
        "failed, but not for the version mismatch; output:\n{}",
        r.output
    );
}

/// no-false-green: if the deployed bytes do NOT hash-match the artifact (partial scp / stale
/// same-version binary), the run must fail with `sha-mismatch` — deploy-from-clean-tree byte
/// diff-verify, not just a --version check.
#[test]
fn deploy_fleet_exits_nonzero_on_byte_mismatch() {
    let r = run_fleet("9.9.9-test", GENLOCK_LINE, false);
    assert!(
        !r.success,
        "exited 0 despite a byte (sha256) mismatch on the deployed binary. output:\n{}",
        r.output
    );
    assert!(
        r.output.contains("sha-mismatch") || r.output.contains("byte-verify"),
        "failed, but not for the byte mismatch; output:\n{}",
        r.output
    );
}

/// A healthy box that logs a RECOVERABLE `error!`-level line (intercom restart / NDI reconnect —
/// normal operation) must STILL pass: the fatal scan must not trip on `error`, only on genuine
/// panics/crashes. Guards against the false-RED the reviewer caught.
#[test]
fn deploy_fleet_tolerates_recoverable_error_log_lines() {
    let recoverable = "INFO camera_box: Streaming: 30.0 fps emitted / 60.0 fps captured (150 sent, 300 captured)\nERROR camera_box::intercom: Intercom error: foo - restarting in 2 seconds";
    let r = run_fleet("9.9.9-test", recoverable, true);
    assert!(
        r.success,
        "a recoverable `error!` log line false-failed a healthy genlocking box (fatal scan too \
         broad). output:\n{}",
        r.output
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
